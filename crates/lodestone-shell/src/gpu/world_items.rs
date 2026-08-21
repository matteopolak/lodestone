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
        Arm, FLAT_ITEM_DEPTH_THRESHOLD, brushable_item_mesh, camera_orientation,
        campfire_item_mesh, dropped_item_mesh, framed_item_mesh, ground_transform,
        hand_transform, held_item_mesh, item_bob_offset, item_cluster_jitter,
        posed_item_z_extent, rendered_amount, shelf_item_mesh, thrown_item_for,
        thrown_item_mesh,
    },
};

use lodestone_model::event::EquipmentSlot;

use crate::entities::{EntityDraw, ITEM_ENTITY_TYPE_PATH};

use super::entity_passes::{entity_light, framed_content_light, item_frame_light};
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
            if draw.type_path.as_ref() != ITEM_ENTITY_TYPE_PATH {
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
            // This drop's real components — `Default` for every field but
            // `dyed_color`/`potion_color`, the only two `item_tint::resolve` can
            // ever read live (see `ItemGeometry::live_tints`'s doc). Built once
            // per drop, outside the up-to-five-copy loop below: every copy of one
            // stack shares one colour, the same way they share one light sample.
            let live_components = lodestone_model::item::ItemComponents {
                dyed_color: draw.item_dyed_color,
                potion_color: draw.item_potion_color,
                ..Default::default()
            };
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
                let mut mesh = dropped_item_mesh(
                    &geometry.quads,
                    geometry.gui_light,
                    &ground,
                    draw.feet + offset,
                    draw.anim.age_ticks,
                    item_bob_offset(draw.id),
                    // Every copy of the stack shares the drop's one sample:
                    // `ItemEntityRenderer` reads `state.lightCoords` once and
                    // `submitMultipleFromCount` reuses it for all five.
                    entity_light(&self.entity_light, draw),
                );
                // A no-op for the overwhelming majority (`live_tints` empty) —
                // real work only for a dyed leather drop or a mixed potion,
                // which until this existed drew the item definition's plain
                // default on the ground exactly as in a mob's hand.
                lodestone_render::stamp_live_item_tint(
                    &mut mesh,
                    &geometry.quads,
                    &geometry.live_tints,
                    &live_components,
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
        // Vault display-item clusters. Same reason as campfire items: a
        // vault's renderer owns no cuboid mesh, only a spinning item cluster,
        // so there is nothing to batch through `prepare_block_entities`.
        self.merge_vault_items(model, camera, &frustum, &mut combined, stats);
        // Brushable-block revealed items. Same reason as campfire/vault: the
        // suspicious sand/gravel a player sees is entirely a real block
        // model, and `BrushableBlockRenderer` draws only the single revealed
        // item on top of it.
        self.merge_brushable_items(model, camera, &frustum, &mut combined, stats);
        // Shelved items. Same reason as campfire/vault/brushable: a shelf's
        // board/back/sides are all real block-model geometry, and
        // `ShelfRenderer` draws only up to three item models on top of it.
        self.merge_shelf_items(model, camera, &frustum, &mut combined, stats);
        // Items hanging in item frames. Same reason again: the frame's border and
        // back plate are a block model (`gpu/moving_blocks.rs` draws them), and
        // `ItemFrameRenderer`'s item branch is one `ItemStackRenderState.submit`
        // on top. This is the *ordinary*-item half — a sword, an ingot, a block
        // item; a chest or a skull is `minecraft:special` and goes through the
        // block-entity rig in `entity_passes.rs` instead, exactly as it does when
        // dropped or held.
        self.merge_framed_items(model, entities, &frustum, &mut combined, &mut foil, stats);
        let Some(mesh) = GpuModelMesh::upload(device, &combined) else {
            return (None, None);
        };
        stats.total_quads += combined.quad_count();
        // No camera write here: dropped items draw through `model.cam_bind_group`,
        // the same shared view_proj+fog buffer every section uses, written once
        // per frame at the top of `render_inner` — not a buffer of their own.
        (mesh.into(), GpuModelMesh::upload(device, &foil))
    }

    /// Merge every ordinary item hanging in an item frame into `combined` —
    /// vanilla's `ItemFrameRenderer.submit` item branch.
    ///
    /// # What this closes
    ///
    /// Three surfaces draw a framed stack and until this existed only two of them
    /// did anything: `prepare_framed_maps` for a `filled_map`, and
    /// `special_item_instances` for the handful of items whose model is
    /// `minecraft:special` (chest, shulker box, skull, conduit). Everything else
    /// — which is the whole of the rest of the item registry — resolved fine,
    /// reached no producer, and drew nothing. `entity_passes.rs`'s own doc
    /// comment stated that shortfall in plain words, which is exactly the shape
    /// `CLAUDE.md` says to read as a defect report rather than as scope.
    ///
    /// # `DisplaySlot::Fixed`, not `Ground`
    ///
    /// `ItemFrameRenderer.extractRenderState` resolves the stack in
    /// `ItemDisplayContext.FIXED`, the same context the campfire path uses and
    /// the single easiest thing to get wrong here, because the *drop* on this
    /// same path is `Ground`. Reusing `Ground` lays a framed sword flat.
    ///
    /// # Named deviations
    ///
    /// * **No `submitMultipleFromCount`.** A frame holds one stack and vanilla's
    ///   item branch draws it once whatever its count, unlike a drop.
    /// * **A framed `filled_map` is skipped**, because `prepare_framed_maps`
    ///   already draws its picture through the map-texture pipeline — vanilla
    ///   takes the same either/or (`state.mapId != null` selects the map branch
    ///   and returns).
    fn merge_framed_items(
        &self,
        model: &ModelRenderer,
        entities: &[EntityDraw],
        frustum: &lodestone_render::Frustum,
        combined: &mut ModelMesh,
        foil: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        let ctx = ItemStateContext::new(DisplaySlot::Fixed);
        for draw in entities {
            let type_path = draw.type_path.as_ref();
            if !super::maps::ITEM_FRAME_TYPES.contains(&type_path) {
                continue;
            }
            let Some(item) = draw.item.as_ref() else {
                continue;
            };
            if item.path() == super::maps::FILLED_MAP_ITEM {
                continue;
            }
            // A framed item is half a block across at most, centred within one
            // block of the entity however the frame is turned.
            if !frustum.intersects_aabb(
                draw.feet - glam::Vec3::splat(1.0),
                draw.feet + glam::Vec3::splat(1.0),
            ) {
                continue;
            }
            let Some(geometry) = model.items.get(item).and_then(|v| v.resolve(&ctx)) else {
                continue;
            };
            let glow = type_path == super::maps::GLOW_ITEM_FRAME_TYPE_PATH;
            let light = framed_content_light(
                item_frame_light(&self.entity_light, draw, glow),
                glow,
            );
            let mut mesh = framed_item_mesh(
                &geometry.quads,
                geometry.gui_light,
                &geometry.display.get(DisplaySlot::Fixed),
                draw.feet,
                draw.yaw,
                draw.pitch,
                draw.item_frame_rotation,
                draw.invisible,
                light,
            );
            // The same live-component tint a drop gets: a dyed leather cap in a
            // frame is the case that separates this from drawing the item
            // definition's plain default.
            lodestone_render::stamp_live_item_tint(
                &mut mesh,
                &geometry.quads,
                &geometry.live_tints,
                &lodestone_model::item::ItemComponents {
                    dyed_color: draw.item_dyed_color,
                    potion_color: draw.item_potion_color,
                    ..Default::default()
                },
            );
            if draw.foil {
                foil.merge(&mesh);
            }
            combined.merge(&mesh);
            stats.item_frame_items_drawn += 1;
        }
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

    /// Merge every vault's display-item cluster into `combined` — vanilla's
    /// `VaultRenderer`.
    ///
    /// # Why this lives in the item pass and not with the other block entities
    ///
    /// `VaultRenderer` bakes no layer and binds no sheet: its `submit` is
    /// `ItemEntityRenderer.renderMultipleFromCount` at a fixed pose. The cage,
    /// door and base a player sees are the *block* model, drawn by the terrain
    /// mesher with no help from here — `blockstates/vault.json` is a plain
    /// `variants` map, the same shape the mob-spawner cage and trial-spawner's
    /// per-state textures already proved — so an unset
    /// [`VaultSource`](super::VaultSource) leaves a complete vault showing no
    /// reward, not a hole.
    ///
    /// # `DisplaySlot::Ground`, matching a dropped item
    ///
    /// `VaultRenderer.extractRenderState` resolves the display item in
    /// `ItemDisplayContext.GROUND` — the identical context a drop resolves in
    /// — which is why this reuses [`ground_transform`] rather than a
    /// vault-specific display slot, and why the multi-copy loop below is the
    /// same flat/solid fan-vs-jitter split the drop loop above uses (both
    /// port `ItemEntityRenderer.renderMultipleFromCount`/`submitMultipleFromCount`,
    /// the same algorithm under two names in the real jar).
    ///
    /// No glint arm, for the reason [`merge_campfire_items`] has none: the
    /// NBT parse behind [`lodestone_render::VaultSpawn`] carries no
    /// `components`, so an enchanted display item draws without its shimmer
    /// rather than with a wrong one.
    fn merge_vault_items(
        &self,
        model: &ModelRenderer,
        camera: &Camera,
        frustum: &lodestone_render::Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        let ctx = ItemStateContext::new(DisplaySlot::Ground);
        for spawn in self.vault_source.vaults(camera.position) {
            let block_pos = glam::Vec3::new(
                spawn.pos[0] as f32,
                spawn.pos[1] as f32,
                spawn.pos[2] as f32,
            );
            if !frustum.intersects_aabb(block_pos, block_pos + glam::Vec3::ONE) {
                continue;
            }
            let Some(geometry) = model.items.get(&spawn.item).and_then(|v| v.resolve(&ctx))
            else {
                continue;
            };
            let ground = ground_transform(&geometry.display, geometry.gui_light);
            let (min_z, max_z) = posed_item_z_extent(&geometry.quads, &ground);
            let depth = max_z - min_z;
            let flat = depth <= FLAT_ITEM_DEPTH_THRESHOLD;
            let amount = rendered_amount(spawn.count);
            let fan_step = depth * 1.5;
            let jitter_extent = if flat { 0.075 } else { 0.15 };
            // Vanilla seeds `renderMultipleFromCount`'s RNG off
            // `ItemClusterRenderState.getSeedForItemStack`
            // (`Item.getId(item) + damageValue`); reusing the registry item id
            // as `item_cluster_jitter`'s hash key gives the identical
            // *property* that function's own doc names — no two vaults'
            // clusters scatter in lockstep — rather than chasing the exact
            // unobservable bytes (the `+ damageValue` term is not modelled).
            let seed_key = lodestone_data::items::item_id(&spawn.item.to_string()).unwrap_or(0);
            for copy in 0..amount {
                let mut offset = item_cluster_jitter(seed_key, copy, jitter_extent);
                if flat {
                    offset.z =
                        fan_step * (copy as f32 - (amount.saturating_sub(1) as f32) / 2.0);
                }
                let mesh = lodestone_render::entity::vault_display_item_mesh(
                    &geometry.quads,
                    geometry.gui_light,
                    &ground,
                    block_pos,
                    spawn.spin_deg,
                    offset,
                    spawn.light,
                );
                combined.merge(&mesh);
                stats.vault_items_drawn += 1;
            }
        }
    }

    /// Merge every brushable block's revealed item into `combined` — vanilla's
    /// `BrushableBlockRenderer`.
    ///
    /// # Why this lives in the item pass and not with the other block entities
    ///
    /// `BrushableBlockRenderer` bakes no layer, binds no sheet and has no model
    /// field: its `submit` is a single `ItemStackRenderState.submit` at one
    /// pose. The suspicious sand/gravel a player sees is the *block* model,
    /// drawn by the terrain mesher with no help from here — so an unset
    /// [`BrushableSource`](super::BrushableSource) leaves a complete,
    /// correctly-dusted block with no item above it, not a hole.
    ///
    /// # `DisplaySlot::Fixed`, matching a campfire item
    ///
    /// `extractRenderState` calls `updateForTopItem(.., ItemDisplayContext.FIXED,
    /// ..)`, the identical context [`merge_campfire_items`] resolves in — the
    /// item-frame pose, not `Ground`.
    ///
    /// No glint arm and no multi-copy scatter, for the same reason
    /// [`merge_campfire_items`] has neither: this draws the block entity's
    /// single `item` stack (no `components`, no glint), one copy, at one pose.
    fn merge_brushable_items(
        &self,
        model: &ModelRenderer,
        camera: &Camera,
        frustum: &lodestone_render::Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        let ctx = ItemStateContext::new(DisplaySlot::Fixed);
        for spawn in self.brushable_source.brushable_items(camera.position) {
            let min = glam::Vec3::new(
                spawn.pos[0] as f32,
                spawn.pos[1] as f32,
                spawn.pos[2] as f32,
            );
            if !frustum.intersects_aabb(min, min + glam::Vec3::ONE) {
                continue;
            }
            let Some(geometry) = model.items.get(&spawn.item).and_then(|v| v.resolve(&ctx))
            else {
                continue;
            };
            combined.merge(&brushable_item_mesh(
                &geometry.quads,
                geometry.gui_light,
                &geometry.display.get(DisplaySlot::Fixed),
                spawn.pos,
                spawn.hit_direction,
                spawn.dust_progress,
                spawn.light,
            ));
            stats.brushable_items_drawn += 1;
        }
    }

    /// Merge every shelf's occupied-slot items into `combined` — vanilla's
    /// `ShelfRenderer`.
    ///
    /// # Why this lives in the item pass and not with the other block entities
    ///
    /// `ShelfRenderer` bakes no layer, binds no sheet and has no model field:
    /// its `submit` is up to three `ItemStackRenderState.submit` calls, one
    /// per occupied slot. A shelf's board/back/sides are all *block* model
    /// geometry the terrain mesher already draws, so an unset
    /// [`ShelfSource`](super::ShelfSource) leaves a complete, empty shelf,
    /// not a hole.
    ///
    /// # `DisplaySlot::OnShelf`, a third display context beside `Fixed`/`Ground`
    ///
    /// `extractRenderState` resolves each item in `ItemDisplayContext.ON_SHELF`
    /// — its own context, distinct from campfire's `Fixed` and a drop's
    /// `Ground`. [`shelf_item_mesh`] is the one caller in this codebase that
    /// needs it.
    ///
    /// No glint arm, for the reason [`merge_campfire_items`] has none: the
    /// `Items` NBT parse behind [`lodestone_render::ShelfItemSpawn`] carries
    /// no `components`.
    fn merge_shelf_items(
        &self,
        model: &ModelRenderer,
        camera: &Camera,
        frustum: &lodestone_render::Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        let ctx = ItemStateContext::new(DisplaySlot::OnShelf);
        for spawn in self.shelf_source.shelf_items(camera.position) {
            let min = glam::Vec3::new(
                spawn.pos[0] as f32,
                spawn.pos[1] as f32,
                spawn.pos[2] as f32,
            );
            if !frustum.intersects_aabb(min, min + glam::Vec3::ONE) {
                continue;
            }
            let Some(geometry) = model.items.get(&spawn.item).and_then(|v| v.resolve(&ctx))
            else {
                continue;
            };
            combined.merge(&shelf_item_mesh(
                &geometry.quads,
                geometry.gui_light,
                &geometry.display.get(DisplaySlot::OnShelf),
                spawn.pos,
                spawn.facing_yaw_deg,
                spawn.slot,
                spawn.align_to_bottom,
                spawn.light,
            ));
            stats.shelf_items_drawn += 1;
        }
    }

    /// Merge one thrown item projectile into `combined` as a camera-facing
    /// billboard of its item model — vanilla's `ThrownItemRenderer`.
    ///
    /// # Which item id, and why the wire is preferred over the table
    ///
    /// `ThrowableItemProjectile`, `Fireball` and `EyeOfEnder` all sync their stack
    /// through `DATA_ITEM_STACK` — the **same** `ITEM_STACK` serializer a dropped
    /// item uses (the decode keys on the serializer, not the index, and
    /// `apply_entity_metadata` inserts `DisplayItem` for any entity type), so
    /// `EntityDraw::item` carries a projectile's real stack. That value is
    /// authoritative and takes precedence.
    ///
    /// **It did not, for as long as this comment claimed it did.**
    /// `extract_entity_draws` narrowed the recorded stack to
    /// `ITEM_ENTITY_TYPE_PATH`, so this arm never once matched in production and
    /// every projectile fell through to the table default below — which is why
    /// a splash potion drew the item definition's plain colour rather than its
    /// own mix. The fallback is what made that invisible: the draw was never
    /// missing, only wrong.
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
            entity_light(&self.entity_light, draw)
        };
        // `display.ground`: `extractRenderState` resolves the item in
        // `ItemDisplayContext.GROUND`, the same context a drop uses — which is why
        // this is `ground_transform` and not a projectile-specific transform.
        let ground = ground_transform(&geometry.display, geometry.gui_light);
        let mut mesh = thrown_item_mesh(
            &geometry.quads,
            geometry.gui_light,
            &ground,
            draw.feet,
            orientation,
            thrown.scale,
            light,
        );
        // A splash/lingering potion's real mix, when the wire reported one
        // (`draw.item` matched above rather than falling back to
        // `thrown.item`) — a no-op for every other projectile in
        // `thrown_item_for`'s table, none of which has a live-tinted layer.
        let live_components = lodestone_model::item::ItemComponents {
            dyed_color: draw.item_dyed_color,
            potion_color: draw.item_potion_color,
            ..Default::default()
        };
        lodestone_render::stamp_live_item_tint(
            &mut mesh,
            &geometry.quads,
            &geometry.live_tints,
            &live_components,
        );
        combined.merge(&mesh);
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
        // The *holder's* light, not the item's own position: vanilla's
        // `ItemInHandLayer` is a layer of the holder's renderer and draws with the
        // holder's `state.lightCoords`, so a sword follows the hand that carries
        // it — eye-probed and fire-forced like every other layer.
        let light = entity_light(&self.entity_light, draw);

        for (slot, id) in &draw.equipment {
            // `Mob.getMainArm()` is `RIGHT` unless `draw.main_arm_left` is set
            // (`Mob.isLeftHanded()`, decoded off the same mob-flags byte as
            // `Mob.isAggressive()`), in which case both hands flip sides.
            let arm = match (slot, draw.main_arm_left) {
                (EquipmentSlot::MainHand, false) | (EquipmentSlot::OffHand, true) => Arm::Right,
                (EquipmentSlot::MainHand, true) | (EquipmentSlot::OffHand, false) => Arm::Left,
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
            //   off-hand item mid-use. `ItemUse::off_hand` is exactly that test —
            //   compared against the *slot* this loop iteration is drawing rather
            //   than `arm.is_left()`, so the check stays correct regardless of
            //   which physical side `main_arm_left` put this slot on.
            let using = draw
                .item_use
                .is_some_and(|use_| use_.using && use_.off_hand == (*slot == EquipmentSlot::OffHand));
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
            let mut mesh = held_item_mesh(
                &geometry.quads,
                geometry.gui_light,
                arm_transform,
                arm,
                baby,
                &transform,
                light,
            );
            // A mob's dyed leather item — `equipment_dye` is the same per-slot
            // fact `armour_layer_tint_with_dye` reads for the four armour
            // slots, so a dyed item in a zombie's hand and a dyed helmet on its
            // head come from one wire fact either way. No per-slot potion
            // colour is tracked for equipment today (unlike a dropped or
            // thrown stack), so a mob holding a potion still draws the
            // definition's default — a real remaining gap, not silently
            // patched over here.
            let live_components = lodestone_model::item::ItemComponents {
                dyed_color: draw
                    .equipment_dye
                    .iter()
                    .find(|(s, _)| *s == *slot)
                    .map(|(_, rgb)| *rgb),
                ..Default::default()
            };
            lodestone_render::stamp_live_item_tint(
                &mut mesh,
                &geometry.quads,
                &geometry.live_tints,
                &live_components,
            );
            combined.merge(&mesh);
            stats.held_items_drawn += 1;
        }
    }
}
