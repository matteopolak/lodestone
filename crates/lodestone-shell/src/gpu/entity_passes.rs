//! The per-entity layer passes: mob bodies, humanoid armour, sheep wool, the
//! mob-fire billboard, and the block-entity rigs.
//!
//! # One resolver, one pose
//!
//! Every layer here resolves through the *same* `EntityModelSet` and the same
//! [`lodestone_render::AnimInput`] that [`RenderState::prepare_entities`] puts
//! on screen, and reads `instance.part_transforms` **without writing anything
//! back**. That is the whole discipline of this file: a helmet can never be
//! posed off a head the body pass did not draw, and a future "optimisation"
//! that posed a layer by mutating the wearer's transforms would break the mob
//! rather than the layer.
//!
//! # Block entities are the one input that is not a mob
//!
//! A chest, skull or bell is a *block*, gathered from the world's decoded
//! block-entity records by an installed source rather than taken from the
//! `entities` slice. Everything downstream — per-part instance buffers, the
//! group-0 camera+fog write, the frustum cull — is deliberately identical,
//! because a chest that fogged or lit differently from the mobs next to it
//! would be the more visible bug.
//!
//! Each function returns uploaded per-part instance buffers for
//! [`super::frame`] to submit; see that module on why they all run before the
//! render pass opens.
use std::collections::HashMap;

use lodestone_assets::entity_models::sheep_wool_tint;
use lodestone_assets::equipment::ArmourSlot;
use lodestone_render::{
    Camera, CameraUniform, EntityCameraUniform, InstanceTint,
    entity::{armour_layer_tint_with_dye, armour_layers},
    plan_block_entities, plan_entities, upload_instances_tinted,
};

use crate::entities::EntityDraw;

use super::block_entities::BlockEntityDrawBatch;
use super::{
    ArmourAccum, ArmourDrawBatch, ArmourPartAccum, EntityDrawBatch, FlameBatch, RenderState,
    RenderStats, WoolPartAccum, humanoid_armour_slot,
};

impl RenderState {

    /// Resolve each interpolated entity into a renderable instance, frustum-cull
    /// and group them by model, upload one instance buffer per surviving model,
    /// and record draw/cull counts. Runs before the render pass so every GPU
    /// buffer it creates outlives the pass that reads it.
    ///
    /// # Why this plans twice (issue #98's hurt overlay)
    ///
    /// `plan_entities` groups by model and drops the input order, so a
    /// per-entity flag cannot be zipped back onto a batch afterwards — and
    /// `EntityInstance` (in `lodestone-render`'s `entity.rs`) carries only the
    /// light byte, not the overlay. The instances are therefore split by
    /// [`EntityDraw::hurt`] *before* planning, and each half's flag stays
    /// attached to the plan it produced as a `(bool, EntityFrame)` pair. That
    /// pairing is the point: a `Vec<bool>` parallel to the batches would be an
    /// invariant nothing enforces, which is precisely how this class of bug
    /// comes back. Grouping by `(model, hurt)` instead of `model` is also what
    /// a hurt mob costs in vanilla — one extra batch while its 10 ticks run,
    /// and nothing at all the rest of the time (the hurt half is empty, and
    /// `plan_entities` on an empty slice returns no batches).
    pub(super) fn prepare_entities(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<EntityDrawBatch> {
        if entities.is_empty() {
            return Vec::new();
        }

        // Rewrite the entity group-0 uniform: view-projection (world position
        // lives per-instance, so the section origin stays zero), **this frame's
        // fog** from the same `self.fog` the terrain sections get, and **this
        // frame's sky darkening**. Both passes therefore fade on one curve; a mob
        // under water or at the render edge dissolves with the blocks around it
        // instead of punching through.
        //
        // Sky darkening rides the fog block's one spare lane, and is rewritten
        // every frame rather than at install time, because the world clock moves:
        // a value captured once would freeze the mob at whatever time of day it
        // happened to spawn.
        let eye = camera.position;
        queue.write_buffer(
            &self.entities.cam_buffer,
            0,
            bytemuck::bytes_of(
                &EntityCameraUniform {
                    camera: CameraUniform {
                        view_proj: camera.view_projection().to_cols_array_2d(),
                        section_origin: [0.0, 0.0, 0.0, 0.0],
                    },
                    fog: self.fog_with_clock(eye),
                }
                .with_sky_darken(self.sky_darken.value()),
            ),
        );

        // Split by `hurt` here, at the one point that still knows which
        // `EntityDraw` each instance came from.
        let mut plain: Vec<_> = Vec::new();
        let mut hurt: Vec<_> = Vec::new();
        for e in entities {
            // `resolve_posed`, not `resolve`, and this is the *only* call site that
            // needs it (issue #380): the pitch selects the **placement**, and a
            // projectile placed by the mob matrix draws 1.501 blocks high and
            // mirrored. For every mob the extra argument changes nothing — a mob's
            // pitch is head tracking and arrives through `e.anim`, not through the
            // placement — so the other five `resolve` call sites are deliberately
            // left alone rather than widened for symmetry.
            let Some(instance) = self
                .entities
                .models
                .resolve_posed(&e.type_path, e.feet, e.yaw, e.pitch, e.scale, &e.anim)
                .map(|i| i.with_light(self.entity_light.sample(e.feet)))
            else {
                continue;
            };
            if e.hurt { &mut hurt } else { &mut plain }.push(instance);
        }

        let frustum = camera.frustum();
        // The flag and the plan it describes travel as one value from here on.
        let plans = [
            (false, plan_entities(&plain, &frustum)),
            (true, plan_entities(&hurt, &frustum)),
        ];
        stats.entities_drawn = plans.iter().map(|(_, f)| f.stats.drawn).sum();
        stats.entities_culled = plans.iter().map(|(_, f)| f.stats.culled_frustum).sum();

        // One instance buffer per *part*, not per entity: the mesh's vertices are
        // part-local, so a limb only moves if its own matrices are uploaded
        // separately. A mob is ~10–35 parts but hundreds of quads, so this moves
        // roughly 1% of the data a per-entity vertex re-bake would.
        plans
            .iter()
            .flat_map(|(hurt, frame)| frame.batches.iter().map(move |batch| (*hurt, batch)))
            .map(|(hurt, batch)| {
                let count = u32::try_from(batch.transforms.len()).unwrap_or(u32::MAX);
                // Every instance in this batch shares one overlay state, by
                // construction of the split above — so one repeated value rather
                // than a per-instance vector, and no way for the two to disagree.
                let tints = vec![InstanceTint::NONE.with_hurt(hurt); batch.transforms.len()];
                // Every part uploads the *same* light and tint slices: a mob's
                // lightmap sample and its overlay state are per entity, so its
                // head and its leg share both values.
                let parts = batch
                    .parts
                    .iter()
                    .map(|p| upload_instances_tinted(device, p, &batch.lights, &tints))
                    .collect();
                EntityDrawBatch {
                    model: batch.model,
                    count,
                    parts,
                }
            })
            .collect()
    }

    /// Resolve this frame's **humanoid armour layers** into per-`(slot, texture)`
    /// instance buffers, ready to draw over the mobs wearing them.
    ///
    /// # Every piece is posed off the wearer's own part matrix
    ///
    /// Vanilla's armour model is an instance of the wearer's model *class* and is
    /// animated by the wearer's render state, so a zombie's chestplate reaches
    /// out in front with `animateZombieArms`. The equivalent here is to run no
    /// second pose at all: `ArmourMesh::attach` pairs each armour part with the
    /// wearer's index for the same name, and this reads
    /// `instance.part_transforms[i]` — the matrix the mob is *already* being
    /// drawn with.
    ///
    /// **Nothing is written back.** That is the same discipline
    /// `EntityInstance::hand_transforms` exists to enforce for held items: there,
    /// folding the item's pivot shift into `part_transforms` would have dragged
    /// the visible arm along with the sword. Armour needs the wearer's matrix
    /// *unmodified*, so there is nothing to fold in — but the rule is the same
    /// one, and a future "optimisation" that poses armour by mutating the
    /// wearer's transforms would break the mob, not the armour.
    ///
    /// # What is deliberately not handled
    ///
    /// * **Trims** (`minecraft:trim`). Not decoded anywhere in this engine and
    ///   not carried past `net::entity_snapshot`, so there is no input; they also
    ///   need a stitched trim-sprite atlas and a third depth mode
    ///   (`CompareOp.EQUAL`, no depth write). See `docs/armour-rendering.md`.
    /// * **A stack's own dye** (`minecraft:dyed_color`). Same reason: the
    ///   component is dropped at `entity_snapshot`, which narrows a stack to its
    ///   item id. Leather therefore always draws at
    ///   `Dyeable.colorWhenUndyed`, which is the correct answer for an undyed
    ///   piece and the only reachable one for a dyed one.
    /// * **Baby rigs.** Vanilla swaps in a whole second mesh set
    ///   (`createBabyArmorMesh`, `humanoid_baby` sheets, its own deformations);
    ///   a baby zombie wears adult armour scaled by the mob's 0.5 uniform scale
    ///   instead. Visibly close, not vanilla.
    /// * **Enchantment glint.** `hasFoil` is not on this side of the wire.
    pub(super) fn prepare_armour(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<ArmourDrawBatch> {
        // No pack, no sheets, nothing to draw — and no synthetic fallback, on
        // purpose (see `EntityRenderer::armour_textures`).
        if self.entities.armour_textures.is_empty() {
            return Vec::new();
        }
        let frustum = camera.frustum();
        let mut accum: Vec<ArmourAccum> = Vec::new();

        for draw in entities {
            if draw.equipment.is_empty() {
                continue;
            }
            // Cheap reject before any pose work: most equipment is a held item.
            if !draw
                .equipment
                .iter()
                .any(|(slot, _)| humanoid_armour_slot(*slot).is_some())
            {
                continue;
            }
            // Same resolver, same `AnimInput` as `prepare_entities`, so a piece
            // of armour can never be posed off a different pose than the body it
            // is drawn over.
            let Some(instance) = self.entities.models.resolve(
                &draw.type_path,
                draw.feet,
                draw.yaw,
                draw.scale,
                &draw.anim,
            ) else {
                continue;
            };
            if !frustum.intersects_aabb(instance.aabb_min, instance.aabb_max) {
                continue;
            }
            let Some(wearer) = self.entities.models.get(instance.model) else {
                continue;
            };
            let light = u32::from(self.entity_light.sample(draw.feet));

            // Walk the *slots* rather than the equipment list, so the draw order
            // is `HumanoidArmorLayer.submit`'s (chest, legs, feet, head)
            // regardless of what order the server happened to send.
            for slot in ArmourSlot::ALL {
                let Some((_, id)) = draw
                    .equipment
                    .iter()
                    .find(|(s, _)| humanoid_armour_slot(*s) == Some(slot))
                else {
                    continue;
                };
                // A modded namespace has no entry in the 26.2 asset table, and
                // guessing one would draw the wrong material.
                if id.namespace() != "minecraft" {
                    continue;
                }
                let layers = armour_layers(slot, id.path());
                if layers.is_empty() {
                    continue;
                }
                let Some(mesh) = self.entities.armour_models.get(slot) else {
                    continue;
                };
                // The humanoid gate lives inside `attach`: a pig handed a
                // chestplate resolves `body` by name and still wears nothing.
                let attached: Vec<_> = mesh.attach(&wearer.skeleton).collect();
                if attached.is_empty() {
                    continue;
                }
                for layer in layers {
                    let texture = (layer.texture, slot.layer_type());
                    if !self.entities.armour_textures.contains_key(&texture) {
                        continue;
                    }
                    // Vanilla's overlay is sampled by every layer of a
                    // `LivingEntityRenderer`'s model, armour included — a hurt
                    // mob whose breastplate stayed its own colour would read as
                    // a rendering fault, not as damage.
                    //
                    // `dye` is looked up per-slot, not per-layer: a slot's dye
                    // applies to every layer drawn for it, and
                    // `armour_layer_tint_with_dye` itself is what ignores the
                    // dye for a non-dyeable layer (diamond, iron, …) — see
                    // that function's own doc and `docs/armour-rendering.md`.
                    let dye = draw
                        .equipment_dye
                        .iter()
                        .find(|(s, _)| humanoid_armour_slot(*s) == Some(slot))
                        .map(|(_, dye)| *dye);
                    let tint =
                        InstanceTint::rgb(armour_layer_tint_with_dye(layer, dye)).with_hurt(draw.hurt);
                    let group = match accum
                        .iter_mut()
                        .position(|a| a.slot == slot && a.texture == texture)
                    {
                        Some(i) => &mut accum[i],
                        None => {
                            accum.push(ArmourAccum {
                                slot,
                                texture,
                                parts: Vec::new(),
                            });
                            accum.last_mut().expect("just pushed")
                        }
                    };
                    for (range, wearer_index) in &attached {
                        let Some(transform) = instance.part_transforms.get(*wearer_index) else {
                            continue;
                        };
                        let part = match group.parts.iter_mut().position(|p| p.range == *range) {
                            Some(i) => &mut group.parts[i],
                            None => {
                                group.parts.push(ArmourPartAccum {
                                    range: *range,
                                    transforms: Vec::new(),
                                    lights: Vec::new(),
                                    tints: Vec::new(),
                                });
                                group.parts.last_mut().expect("just pushed")
                            }
                        };
                        part.transforms.push(*transform);
                        part.lights.push(light);
                        part.tints.push(tint);
                    }
                    stats.armour_layers_drawn += 1;
                }
            }
        }

        accum
            .into_iter()
            .map(|group| ArmourDrawBatch {
                slot: group.slot,
                texture: group.texture,
                parts: group
                    .parts
                    .into_iter()
                    .filter_map(|p| {
                        let count = u32::try_from(p.transforms.len()).unwrap_or(u32::MAX);
                        upload_instances_tinted(device, &p.transforms, &p.lights, &p.tints)
                            .map(|buffer| (p.range, buffer, count))
                    })
                    .collect(),
            })
            .collect()
    }

    /// Resolve this frame's on-fire entities into per-model-type flame
    /// instance buffers (issue #434 — player report: "mobs dont show flames
    /// yet"). One [`FlameBatch`] per distinct `EntityDraw::type_path` that has
    /// at least one on-fire, frustum-visible instance this frame.
    ///
    /// No pack, no texture, nothing to draw — and no synthetic fallback, on
    /// purpose (see `EntityRenderer::flame_texture`'s doc, the same asymmetry
    /// `wool_texture`/`armour_textures` already document).
    ///
    /// The billboard rotation is the camera's own yaw only (vanilla's
    /// `Mth.rotationAroundAxis(Mth.Y_AXIS, camera.orientation, …)`,
    /// `EntityRenderDispatcher.java:163`) — identical for every flame drawn
    /// this frame, not a per-entity look-at vector. The exact sign is not
    /// pixel-matched against vanilla's own convention: this engine's entity
    /// draws are double-sided (`cull_mode: None`, see `entity_pipeline.rs`),
    /// so a flat billboard reads identically face-on for either sign of the
    /// rotation — only *which* horizontal axis the flame's thin edge points
    /// down would flip, never its visibility.
    pub(super) fn prepare_flame(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<FlameBatch> {
        let Some(_flame_texture) = &self.entities.flame_texture else {
            return Vec::new();
        };
        // The current animation frame. Both `fire_0`/`fire_1` have exactly 32
        // frames, held one *render* frame each rather than the real 20 Hz
        // game tick — see `docs/entity-rendering.md`'s "Mob fire" section for
        // why: avoiding a new parameter threaded through `render`/
        // `render_with_crack`/`render_with_effects`'s call sites.
        let tick = self.flame_frame_counter.get();
        self.flame_frame_counter.set(tick.wrapping_add(1));
        let frame = (tick % 32) as u32;

        let frustum = camera.frustum();
        let billboard = glam::Mat4::from_rotation_y(camera.yaw.to_radians());
        let mut accum: HashMap<String, Vec<glam::Mat4>> = HashMap::new();

        for draw in entities {
            if !draw.on_fire {
                continue;
            }
            if !self.entities.flame_gpu_models.contains_key(&draw.type_path) {
                continue;
            }
            let Some(instance) = self.entities.models.resolve(
                &draw.type_path,
                draw.feet,
                draw.yaw,
                draw.scale,
                &draw.anim,
            ) else {
                continue;
            };
            if !frustum.intersects_aabb(instance.aabb_min, instance.aabb_max) {
                continue;
            }
            let transform = glam::Mat4::from_translation(draw.feet) * billboard;
            accum.entry(draw.type_path.clone()).or_default().push(transform);
            stats.flame_billboards_drawn += 1;
        }

        accum
            .into_iter()
            .filter_map(|(model, transforms)| {
                let count = u32::try_from(transforms.len()).unwrap_or(u32::MAX);
                lodestone_render::entity_pipeline::upload_flame_instances(device, &transforms, frame)
                    .map(|buffer| FlameBatch { model, buffer, count })
            })
            .collect()
    }

    /// Sheep wool layers (issue #53), over the same instances `prepare_entities`
    /// resolved — same resolver, same `AnimInput`, so wool can never be posed
    /// off a different pose than the body it grows out of. Mirrors
    /// [`prepare_armour`](Self::prepare_armour) exactly, minus the per-slot/
    /// per-texture grouping armour needs: wool has one mesh and one sheet, so
    /// every attached part accumulates into a single set of per-part buffers.
    ///
    /// # What is deliberately not handled
    ///
    /// * **Sheared sheep.** `draw.wool.sheared` is checked here, not filtered
    ///   upstream — [`EntityDraw::wool`]'s own doc explains why the data stays
    ///   honest about what the server reported. This is vanilla's own
    ///   `if (!state.isSheared)` gate (`SheepWoolLayer.submit`), applied at
    ///   exactly the point that draws the mesh.
    /// * **The pig/cow trap.** [`WoolMesh::attach`]'s `wearer_model` argument
    ///   is `instance.model` — the *resolved* model name — never
    ///   `wearer.family()`. `AnimFamily::Quadruped` is shared by `pig`, `cow`
    ///   and `wolf`; gating on family alone would grow wool on a pig the way
    ///   an ungated armour attach once drew a breastplate on one. In practice
    ///   `EntityDraw::wool` is already `None` for every non-sheep type
    ///   ([`crate::entities::sheep_wool`]'s own gate), so this is a second,
    ///   independent gate rather than the only one — belt and braces, the same
    ///   discipline `docs/entity-rendering.md` asks for.
    /// * **Baby sheep, the `jeb_` rainbow name, and the undercoat overlay.**
    ///   Not built — see `docs/entity-rendering.md`'s "deliberately out of
    ///   scope" list, unchanged by this pass.
    pub(super) fn prepare_wool(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<(lodestone_render::PartRange, wgpu::Buffer, u32)> {
        // No pack, no sheet, nothing to draw — and no synthetic fallback, on
        // purpose (see `EntityRenderer::wool_texture`).
        let (Some(wool_texture), Some(_wool_gpu)) =
            (&self.entities.wool_texture, &self.entities.wool_gpu)
        else {
            return Vec::new();
        };
        let _ = wool_texture; // presence check only; the bind group is read at draw time.
        let frustum = camera.frustum();
        let mut accum: Vec<WoolPartAccum> = Vec::new();

        for draw in entities {
            let Some(wool) = draw.wool else { continue };
            // Vanilla's own gate: a sheared sheep grows no wool mesh at all.
            if wool.sheared {
                continue;
            }
            let Some(instance) = self.entities.models.resolve(
                &draw.type_path,
                draw.feet,
                draw.yaw,
                draw.scale,
                &draw.anim,
            ) else {
                continue;
            };
            if !frustum.intersects_aabb(instance.aabb_min, instance.aabb_max) {
                continue;
            }
            let Some(wearer) = self.entities.models.get(instance.model) else {
                continue;
            };
            // The pig/cow-trap gate lives inside `attach`, keyed on the
            // resolved model name — see this method's docs.
            let attached: Vec<_> = self
                .entities
                .wool_models
                .mesh()
                .attach(&wearer.skeleton, instance.model)
                .collect();
            if attached.is_empty() {
                continue;
            }
            let light = u32::from(self.entity_light.sample(draw.feet));
            // Same reason armour carries it: the wool is one of the sheep's
            // model layers, so it reddens with the body.
            let tint = InstanceTint::rgb(sheep_wool_tint(wool.color)).with_hurt(draw.hurt);
            for (range, wearer_index) in &attached {
                let Some(transform) = instance.part_transforms.get(*wearer_index) else {
                    continue;
                };
                let part = match accum.iter_mut().position(|p| p.range == *range) {
                    Some(i) => &mut accum[i],
                    None => {
                        accum.push(WoolPartAccum {
                            range: *range,
                            transforms: Vec::new(),
                            lights: Vec::new(),
                            tints: Vec::new(),
                        });
                        accum.last_mut().expect("just pushed")
                    }
                };
                part.transforms.push(*transform);
                part.lights.push(light);
                part.tints.push(tint);
            }
            stats.wool_layers_drawn += 1;
        }

        accum
            .into_iter()
            .filter_map(|p| {
                let count = u32::try_from(p.transforms.len()).unwrap_or(u32::MAX);
                upload_instances_tinted(device, &p.transforms, &p.lights, &p.tints)
                    .map(|buffer| (p.range, buffer, count))
            })
            .collect()
    }

    /// Resolve this frame's block entities (chests, issue #23) into per-part
    /// instance buffers, frustum-culled and batched by `(model, sheet)`.
    ///
    /// # The one thing that is *not* like `prepare_entities`
    ///
    /// A chest's input does not come from the `entities` slice — it is a block,
    /// gathered from the world's decoded block-entity records by the source the
    /// shell installs. Everything downstream (per-part instance buffers, the
    /// group-0 camera+fog write, the `Frustum` cull) is deliberately identical,
    /// because a chest that fogged or lit differently from the mobs standing next
    /// to it would be the more visible bug.
    ///
    /// Light arrives already sampled on each [`lodestone_render::ChestSpawn`]
    /// rather than being read through [`Self::entity_light`] here: the gather
    /// already holds the world open to find the chest at all, and sampling there
    /// costs one lock instead of one per chest.
    pub(super) fn prepare_block_entities(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
        stats: &mut RenderStats,
    ) -> Vec<BlockEntityDrawBatch> {
        // Always reported, even on an empty frame: this is what separates "no
        // chests in view" from "no pack, so nothing can ever draw" — a chest with
        // no sheet draws nothing rather than a placeholder.
        stats.block_entity_sheets_loaded = self.block_entities.sheet_count();

        let eye = camera.position;
        let chests = self.block_entity_source.chests(eye);
        let skulls = self.skull_source.skulls(eye);
        let bells = self.bell_source.bells(eye);
        // All three, not any pair: an early return on only `chests`/`skulls`
        // would make a bell in an otherwise chestless, skull-less room draw
        // nothing, which is exactly how this pass would have grown a third
        // island.
        if chests.is_empty() && skulls.is_empty() && bells.is_empty() {
            return Vec::new();
        }

        // Same group-0 contents as the entity pass, written to this pass's own
        // buffer: view-projection (world position is per-instance, so the section
        // origin stays zero), this frame's fog, and this frame's sky darkening.
        queue.write_buffer(
            &self.block_entities.cam_buffer,
            0,
            bytemuck::bytes_of(
                &EntityCameraUniform {
                    camera: CameraUniform {
                        view_proj: camera.view_projection().to_cols_array_2d(),
                        section_origin: [0.0, 0.0, 0.0, 0.0],
                    },
                    fog: self.fog_with_clock(eye),
                }
                .with_sky_darken(self.sky_darken.value()),
            ),
        );

        let mut instances: Vec<_> = chests
            .iter()
            .filter_map(|spawn| self.block_entities.models.resolve_chest(spawn))
            .collect();
        // Appended into the same list rather than planned separately: a chest and
        // a skull batch independently inside one `plan_block_entities` call, so
        // frustum culling and the batch split are shared for free.
        instances.extend(
            skulls
                .iter()
                .filter_map(|spawn| self.block_entities.models.resolve_skull(spawn)),
        );
        instances.extend(
            bells
                .iter()
                .filter_map(|spawn| self.block_entities.models.resolve_bell(spawn)),
        );

        let frame = plan_block_entities(&instances, &camera.frustum());
        stats.block_entities_drawn = frame.stats.drawn;
        stats.block_entities_culled = frame.stats.culled_frustum;

        frame
            .batches
            .iter()
            .map(|batch| BlockEntityDrawBatch {
                model: batch.model,
                texture: batch.texture,
                count: batch.count(),
                // One buffer per part, for the reason `prepare_entities` gives:
                // vertices are part-local, so the lid only moves if its own
                // matrices are uploaded separately from the bottom's.
                //
                // `_tinted`, not the plain `upload_instances`: block entities
                // now carry a per-instance `InstanceTint` (Job 2 step A,
                // `lodestone_render::block_entity::BlockEntityBatch::tints`),
                // the same plumbing sheep wool/dyed armour/the hurt overlay
                // already use. Every resolver still passes white
                // (`[255, 255, 255]`, `InstanceTint::NONE`'s rgb), so this is
                // a no-op today — proved by the chest/skull/bell pixel gates
                // coming out byte-identical — and the hook the next tinted
                // block-entity type (e.g. a banner base colour) plugs into.
                parts: batch
                    .parts
                    .iter()
                    .map(|p| upload_instances_tinted(device, p, &batch.lights, &batch.tints))
                    .collect(),
            })
            .collect()
    }
}
