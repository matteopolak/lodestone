//! World item geometry: dropped items, thrown-item projectile billboards, and
//! the items in mobs' hands.
//!
//! All three are **item models** — the same baked quads a hotbar slot uses —
//! not cuboid part rigs, so none of them can go through
//! [`lodestone_render::EntityPipeline`] however closely attached to one they
//! are. They draw through the *model* pipeline instead, with the same atlas /
//! palette / animation bind groups as terrain, so a dropped block is textured
//! from exactly the pixels the placed block is.
//!
//! Each item's placement — a drop's bob and spin, a projectile's camera-facing
//! billboard, a held item's arm chain — is folded into its **vertex
//! positions**, so unlike the mobs there is no per-instance matrix to batch on
//! and no shared geometry between two different items. They are therefore
//! concatenated into one buffer and one draw call per frame however many items
//! exist. See [`RenderState::prepare_item_geometry`].
use lodestone_assets::DisplaySlot;
use lodestone_render::{
    Camera, ENTITY_FULLBRIGHT, GpuModelMesh, ItemStateContext, ModelMesh,
    entity::{
        Arm, FLAT_ITEM_DEPTH_THRESHOLD, camera_orientation, campfire_item_mesh, dropped_item_mesh,
        ground_transform, hand_transform, held_item_mesh, item_bob_offset, item_cluster_jitter,
        posed_item_z_extent, rendered_amount, thrown_item_for, thrown_item_mesh,
    },
};

use lodestone_model::event::EquipmentSlot;

use crate::entities::{EntityDraw, ITEM_ENTITY_TYPE_PATH};

use super::terrain::ModelRenderer;
use super::{RenderState, RenderStats};

impl RenderState {

    /// Mesh this frame's **world item geometry** — dropped items *and* items in
    /// mobs' hands — into one world-space [`GpuModelMesh`], and rewrite the
    /// pass's camera uniform.
    ///
    /// Returns `None` — and draws nothing — when there is no vanilla model pass,
    /// or when nothing on screen resolves to baked item geometry. For a drop that
    /// last case is vanilla's own behaviour: `ItemEntityRenderer.submit` returns
    /// immediately on an empty stack, and so does `ItemInHandLayer` on an empty
    /// hand.
    ///
    /// # One mesh, not one per item
    ///
    /// Each item's placement (a drop's bob and spin, a held item's arm chain) is
    /// folded into its **vertex positions** by [`dropped_item_mesh`] /
    /// [`held_item_mesh`], so unlike the mobs there is no per-instance matrix to
    /// batch on and no shared geometry between two different items. Concatenating
    /// them into a single buffer is therefore both the simplest and the cheapest
    /// option: one upload and one draw call per frame however many items exist,
    /// versus one of each per item.
    ///
    /// # Why held items are here and not in the entity pass
    ///
    /// An item is an *item model* — the same baked quads a hotbar slot uses —
    /// not a cuboid part rig, so it cannot go through [`EntityPipeline`] however
    /// closely it is attached to one. The only thing the entity side contributes
    /// is the arm's world matrix, which is why this reads `part_transforms` out
    /// of a freshly resolved instance rather than the other way round.
    ///
    /// # Two meshes, because the glint is a second rasterisation of the same quads
    ///
    /// The returned pair is `(all items, enchanted items only)`. The glint pipeline
    /// is depth-`EQUAL`, so it can only shimmer where the base draw already wrote
    /// depth — which means the enchanted subset has to be a *separate* buffer
    /// carrying byte-identical vertices, not a filtered range of the first. Both
    /// halves are merged in the same loop from the same
    /// [`dropped_item_mesh`] output for exactly that reason: two calls could
    /// diverge, and the depth compare would then silently reject the shimmer.
    pub(super) fn prepare_item_geometry(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> (Option<GpuModelMesh>, Option<GpuModelMesh>) {
        let Some(model) = self.model.as_ref() else {
            return (None, None);
        };
        let frustum = camera.frustum();
        let mut combined = ModelMesh::default();
        let mut foil = ModelMesh::default();
        // `camera.orientation` for every thrown projectile this frame: one
        // matrix, not one per entity — a billboard's rotation depends only on the
        // camera.
        let orientation = camera_orientation(camera.view_matrix());
        for draw in entities {
            if draw.type_path != ITEM_ENTITY_TYPE_PATH {
                if let Some(thrown) = thrown_item_for(&draw.type_path) {
                    self.merge_thrown_item(
                        model,
                        draw,
                        thrown,
                        orientation,
                        &frustum,
                        &mut combined,
                        stats,
                    );
                    // A projectile holds no equipment; skip the held-item scan.
                    continue;
                }
                self.merge_held_items(model, draw, &frustum, &mut combined, stats);
                continue;
            }
            // No stack reported (today: all of them — see
            // `EntityInterpolator::set_item_stack`) or a sprite-only item with
            // no 3-D geometry: draw nothing rather than a stand-in.
            //
            // `DisplaySlot::Ground` is the context vanilla's
            // `ItemEntityRenderer.extractRenderState` resolves in, and it is a real
            // branch: `spyglass`, `trident` and the spears list `ground` alongside
            // `gui` in their `display_context` case, so a drop must resolve there
            // rather than inherit whatever the inventory picked.
            let Some(geometry) = draw
                .item
                .as_ref()
                .and_then(|id| model.items.get(id))
                .and_then(|v| v.resolve(&ItemStateContext::new(DisplaySlot::Ground)))
            else {
                continue;
            };
            // A drop is at most a quarter-block across, so a cheap point-in-
            // frustum test on its position is enough to keep off-screen piles
            // out of the buffer without an AABB.
            if !frustum.intersects_aabb(
                draw.feet - glam::Vec3::splat(0.5),
                draw.feet + glam::Vec3::splat(0.5),
            ) {
                continue;
            }
            // The item's **own** `display.ground`, now that the asset layer
            // carries every slot and not just `gui`. `ground_transform` falls
            // back to the `GuiLight`-keyed vanilla constants only for a model
            // chain that declares no `ground` at all.
            let ground = ground_transform(&geometry.display, geometry.gui_light);
            // Vanilla draws a *stack* as up to five copies —
            // `ItemEntityRenderer.submitMultipleFromCount`. The first is
            // unperturbed; the rest scatter, and how they scatter depends on the
            // posed model's own depth: a solid model jitters in all three axes,
            // a flat sprite instead fans evenly along `z` with a smaller jitter,
            // centred so the fan grows both ways rather than only backwards.
            let (min_z, max_z) = posed_item_z_extent(&geometry.quads, &ground);
            let depth = max_z - min_z;
            let flat = depth <= FLAT_ITEM_DEPTH_THRESHOLD;
            let amount = rendered_amount(draw.count);
            let fan_step = depth * 1.5;
            let jitter_extent = if flat { 0.075 } else { 0.15 };
            for copy in 0..amount {
                let mut offset = item_cluster_jitter(draw.id, copy, jitter_extent);
                if flat {
                    // Even spacing along z, centred on the entity, and no z
                    // jitter — the fan *is* the z placement.
                    offset.z = fan_step
                        * (copy as f32 - (amount.saturating_sub(1) as f32) / 2.0);
                }
                let mesh = dropped_item_mesh(
                    &geometry.quads,
                    geometry.gui_light,
                    &ground,
                    draw.feet + offset,
                    draw.anim.age_ticks,
                    item_bob_offset(draw.id),
                    self.entity_light.sample(draw.feet),
                );
                if draw.foil {
                    foil.merge(&mesh);
                }
                combined.merge(&mesh);
                stats.item_drops_drawn += 1;
            }
        }
        // Campfire cooking items. Merged into the same buffer as the drops for
        // the same reason they share one: the placement is folded into the
        // vertices, so there is nothing to batch on. This is *not* in
        // `prepare_block_entities` — a campfire's renderer owns no cuboid mesh at
        // all, only four item poses.
        self.merge_campfire_items(model, camera, &frustum, &mut combined, stats);
        let Some(mesh) = GpuModelMesh::upload(device, &combined) else {
            return (None, None);
        };
        stats.total_quads += combined.quad_count();
        // No camera write here: dropped items draw through `model.cam_bind_group`,
        // the same shared view_proj+fog buffer every section uses, written once
        // per frame at the top of `render_inner` — not a buffer of their own.
        (mesh.into(), GpuModelMesh::upload(device, &foil))
    }

    /// Merge every campfire's cooking items into `combined` — vanilla's
    /// `CampfireRenderer`, which is the whole of that renderer.
    ///
    /// # Why this lives in the item pass and not with the other block entities
    ///
    /// `CampfireRenderer` bakes no layer, binds no sheet and has no model field:
    /// its `submit` is four `ItemStackRenderState.submit` calls at four poses. The
    /// fire, the logs and the smoke a player sees are the *block* model, drawn by
    /// the terrain mesher with no help from here — so an unset
    /// [`CampfireSource`](super::CampfireSource) leaves a complete campfire
    /// cooking nothing, not a hole. Routing this through the entity pipeline would
    /// need a texture stem that does not exist.
    ///
    /// # `DisplaySlot::Fixed`, not `Ground`
    ///
    /// `extractRenderState` calls
    /// `updateForTopItem(.., ItemDisplayContext.FIXED, ..)` — the item-frame
    /// context. Reusing the drop path's `Ground` would pose a steak on its edge,
    /// and it is the single easiest thing to get wrong here because every other
    /// world item on this path *is* `Ground`.
    ///
    /// No glint arm: a campfire cooks food, and `ItemStackRenderState`'s foil is
    /// not derivable from the `Items` NBT we read (which carries no `components`
    /// parse). An enchanted item on a campfire therefore draws without its
    /// shimmer rather than with a wrong one.
    fn merge_campfire_items(
        &self,
        model: &ModelRenderer,
        camera: &Camera,
        frustum: &lodestone_render::Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        let ctx = ItemStateContext::new(DisplaySlot::Fixed);
        for spawn in self.campfire_source.campfire_items(camera.position) {
            // The whole block, not the item's own quarter: one test per campfire
            // slot is cheaper than deriving a 0.375-wide box, and a campfire on
            // the frustum edge showing three of its four items would be worse
            // than drawing all four.
            let min = glam::Vec3::new(
                spawn.pos[0] as f32,
                spawn.pos[1] as f32,
                spawn.pos[2] as f32,
            );
            if !frustum.intersects_aabb(min, min + glam::Vec3::ONE) {
                continue;
            }
            let Some(geometry) = model.items.get(&spawn.item).and_then(|v| v.resolve(&ctx)) else {
                continue;
            };
            combined.merge(&campfire_item_mesh(
                &geometry.quads,
                geometry.gui_light,
                &geometry.display.get(DisplaySlot::Fixed),
                spawn.pos,
                spawn.facing_yaw_deg,
                spawn.slot,
                spawn.light,
            ));
            stats.campfire_items_drawn += 1;
        }
    }

    /// Merge one thrown item projectile into `combined` as a camera-facing
    /// billboard of its item model — vanilla's `ThrownItemRenderer`.
    ///
    /// # Which item id, and why the wire is preferred over the table
    ///
    /// `ThrowableItemProjectile`, `Fireball` and `EyeOfEnder` all sync their stack
    /// through `DATA_ITEM_STACK` — the **same** `ITEM_STACK` serializer at the same
    /// metadata index a dropped item uses, so `EntityDraw::item` is already
    /// populated for a projectile with no new plumbing (`apply_entity_metadata`
    /// inserts `DisplayItem` for any entity type, not just `item`). That value is
    /// authoritative and takes precedence.
    ///
    /// [`ThrownItem::item`] is the fallback for the case the wire cannot cover:
    /// vanilla only marks the field dirty when a constructor *sets* it, so a
    /// snowball thrown by a snow golem — built through the position-only
    /// constructor — arrives with the field never reported. Drawing nothing there
    /// would be a silent hole in exactly the situation a player is being pelted.
    ///
    /// # Full-bright
    ///
    /// [`ThrownItem::full_bright`] is vanilla's `getBlockLightLevel` override
    /// returning `15`; it maps onto [`ENTITY_FULLBRIGHT`], the same byte the GUI
    /// item path nails every vertex to. The world sample is used otherwise, so a
    /// snowball crossing a shadow dims and a fireball does not.
    fn merge_thrown_item(
        &self,
        model: &ModelRenderer,
        draw: &EntityDraw,
        thrown: lodestone_render::entity::ThrownItem,
        orientation: glam::Mat4,
        frustum: &lodestone_render::Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        // The wire's stack first, the registration's default second. `and_then`
        // rather than `or_else` on the geometry lookup: an id that resolves to no
        // baked geometry should fall through to the default too, not draw nothing.
        //
        // `Ground`, as the drop pass: `extractRenderState` resolves a projectile's
        // item in `ItemDisplayContext.GROUND` too, which is the same reason the
        // pose below is `ground_transform` and not a projectile-specific one.
        let ctx = ItemStateContext::new(DisplaySlot::Ground);
        let geometry = draw
            .item
            .as_ref()
            .and_then(|id| model.items.get(id))
            .and_then(|v| v.resolve(&ctx))
            .or_else(|| {
                let id: lodestone_assets::ResourceLocation = thrown.item.parse().ok()?;
                model.items.get(&id)?.resolve(&ctx)
            });
        let Some(geometry) = geometry else {
            return;
        };
        // Scaled slack: a `fireball` is drawn at 3x, so a half-block box would cull
        // it while a third of it was still on screen.
        let slack = glam::Vec3::splat(0.5 * thrown.scale.max(1.0));
        if !frustum.intersects_aabb(draw.feet - slack, draw.feet + slack) {
            return;
        }
        let light = if thrown.full_bright {
            ENTITY_FULLBRIGHT
        } else {
            self.entity_light.sample(draw.feet)
        };
        // `display.ground`: `extractRenderState` resolves the item in
        // `ItemDisplayContext.GROUND`, the same context a drop uses — which is why
        // this is `ground_transform` and not a projectile-specific transform.
        let ground = ground_transform(&geometry.display, geometry.gui_light);
        combined.merge(&thrown_item_mesh(
            &geometry.quads,
            geometry.gui_light,
            &ground,
            draw.feet,
            orientation,
            thrown.scale,
            light,
        ));
        stats.projectiles_drawn += 1;
    }

    /// Merge whatever `draw` is holding into `combined`, posed off its own arm.
    ///
    /// Called for every non-item entity, so the early returns are the common
    /// path: most mobs carry no equipment at all, and `EntityDraw::equipment` is
    /// then an empty `Vec` and this costs one branch.
    ///
    /// # What is deliberately not handled
    ///
    /// * **The four humanoid armour slots.** Still skipped *here*, because armour
    ///   is not an item model hung off an arm — it is a cuboid mesh layer over
    ///   the wearer's rig, and it goes through the *entity* pipeline. See
    ///   [`RenderState::prepare_armour`], which is where `Head`/`Chest`/`Legs`/
    ///   `Feet` are consumed. Faking one here by posing an *item* model at a
    ///   chest slot would draw a floating chestplate icon, which is worse than
    ///   nothing.
    /// * **`Body` and `Saddle`.** Neither is humanoid armour and neither has a
    ///   mesh: `BODY` is `ANIMAL_ARMOR` (wolf armour, horse barding —
    ///   `WolfArmorLayer`, `HorseArmorLayer`) and `SADDLE` is its own type with
    ///   eleven per-mount layer types. See [`humanoid_armour_slot`] for why
    ///   folding `Body` into `Chest` is specifically wrong.
    /// * **Rigs with no arm.** A creeper with a `MainHand` item (a plugin can do
    ///   this) resolves no `right_arm` part, so nothing is drawn. Vanilla agrees:
    ///   `ItemInHandLayer` is only attached to renderers whose model implements
    ///   `ArmedModel`.
    fn merge_held_items(
        &self,
        model: &ModelRenderer,
        draw: &EntityDraw,
        frustum: &lodestone_render::Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        if draw.equipment.is_empty() {
            return;
        }
        // Cull on the holder, before doing any pose work: a mob behind the
        // camera cannot show its sword. Two blocks of slack around the feet
        // covers a tall mob plus the item's own reach.
        if !frustum.intersects_aabb(
            draw.feet - glam::Vec3::new(1.0, 0.5, 1.0),
            draw.feet + glam::Vec3::new(1.0, 2.5, 1.0),
        ) {
            return;
        }
        // The arm matrices come from the same resolver — and therefore the same
        // `AnimInput` — that `prepare_entities` puts on screen, so a held item can
        // never be posed off a different pose than the arm the player sees. An
        // entity type with no ported model resolves to `None` and holds nothing,
        // which is also what happens to the mob itself.
        let Some(instance) = self.entities.models.resolve(
            &draw.type_path,
            draw.feet,
            draw.yaw,
            draw.scale,
            &draw.anim,
        ) else {
            return;
        };
        let Some(mesh) = self.entities.models.get(instance.model) else {
            return;
        };
        // `net::entity_snapshot` maps `baby` onto a 0.5 uniform scale, which is
        // the only baby signal that reaches this layer — the same test
        // `entities.rs` already uses to pick `BABY_LIMB_SCALE`.
        let baby = draw.scale < 1.0;
        let light = self.entity_light.sample(draw.feet);

        for (slot, id) in &draw.equipment {
            // Every `Mob` returns `HumanoidArm.RIGHT` from `getMainArm()` (only
            // a `Player` can be left-handed, and the wire never tells us), so
            // main hand → right arm, off hand → left arm.
            let arm = match slot {
                EquipmentSlot::MainHand => Arm::Right,
                EquipmentSlot::OffHand => Arm::Left,
                // Humanoid armour is drawn by `prepare_armour` through the
                // entity pipeline; `Body`/`Saddle` are animal equipment with no
                // mesh at all. See this method's docs.
                _ => continue,
            };
            // Which variant this hand draws. Three things decide it, and all three
            // are live here:
            //
            // * the display context — `thirdperson_{left,right}hand`, which is what
            //   makes a mob's spyglass the 3-D tube rather than the flat sprite;
            // * whether this entity is using an item at all;
            // * **and which hand it is using.** Vanilla's `using_item` is
            //   `owner.isUsingItem() && owner.getUseItem() == itemStack`, so a
            //   skeleton drawing a bow in its main hand must not also draw its
            //   off-hand item mid-use. `ItemUse::off_hand` is exactly that test,
            //   and dropping it is the mistake that would bend both items.
            let using = draw
                .item_use
                .is_some_and(|use_| use_.using && use_.off_hand == arm.is_left());
            // `arm.display_slot(false)` — the *same* expression `hand_transform`
            // below reads the pose from, so the variant and the transform cannot
            // disagree about which hand this is.
            let ctx = ItemStateContext::new(arm.display_slot(false))
                .with_use(using, draw.item_use.map_or(0, |use_| use_.ticks));
            let Some(geometry) = model.items.get(id).and_then(|v| v.resolve(&ctx)) else {
                continue;
            };
            // Prefer the dedicated hand transform over `part_transforms[arm]`.
            // Five models (skeleton/stray/wither_skeleton, player_slim, vex,
            // allay) shift or scale the item's pivot relative to the arm, and
            // that shift must *not* move the arm's own visible mesh — which is
            // what `part_transforms` places. `hand_transform` is exactly the
            // structural pose for every other model, so this is not a special
            // case, it is the correct source.
            let Some(arm_transform) = instance.hand_transform(arm).or_else(|| {
                let part = mesh.skeleton.index_of(arm.part_name())?;
                instance.part_transforms.get(part).copied()
            }) else {
                continue;
            };
            let transform = hand_transform(&geometry.display, arm, false);
            combined.merge(&held_item_mesh(
                &geometry.quads,
                geometry.gui_light,
                arm_transform,
                arm,
                baby,
                &transform,
                light,
            ));
            stats.held_items_drawn += 1;
        }
    }
}
