//! The mob-spawner/trial-spawner cage's spinning display mob.
//!
//! Unlike every block-entity family in `gpu/block_entities.rs`, this is not a
//! [`lodestone_render::BlockEntityModelSet`] consumer at all — it reuses the
//! **ordinary mob** entity pipeline `gpu/entity_passes.rs`'s
//! `prepare_entities` draws, at a nested, shrunk, spinning placement
//! [`lodestone_render::spawner_display_outer_matrix`] builds. Both spawner
//! block types already have real block-model geometry for the cage itself
//! (`models/block/spawner.json`/`trial_spawner.json`), drawn by the ordinary
//! terrain mesher regardless of this pass — see `docs/block-entity-renderers.md`'s
//! Mob spawner section.
//!
//! # Why this is not folded into `prepare_entities`
//!
//! Every instance there is placed by [`lodestone_render::entity_model_matrix`]
//! from a `(feet, yaw, scale)` triple — the ordinary "standing on the ground"
//! convention. The display mob's placement is a **nested** composition
//! (block position, then vanilla's own spin/tilt/shrink pose stack, then the
//! entity's *own* placement matrix inside that) that no `(feet, yaw, scale)`
//! triple can express, which is exactly why
//! [`lodestone_render::EntityModelSet::resolve_at`] exists — see its doc and
//! `lodestone_render::spawner`'s module doc for the full derivation.
//!
//! # What is reused, and what is new
//!
//! Reused, unmodified: the entity mesh corpus, the GPU model/texture maps
//! (`EntityRenderer::gpu_models`/`textures` — a spinning zombie needs no new
//! GPU resource, it is the same zombie already loaded for full-size mobs),
//! [`plan_entities`] for frustum culling and per-model grouping, and the
//! draw loop in `gpu/frame.rs` — this pass's output is the exact same
//! [`EntityDrawBatch`] type `prepare_entities` returns, appended into the
//! same `Vec` and drawn through the same `self.entities.pipeline`/camera bind
//! group, so it needs no draw-loop changes at all. New: this one function,
//! which resolves [`lodestone_render::SpawnerMobSpawn`]s (gathered by
//! `crate::block_entities::spawner_mob_spawns`, installed by
//! `Sim::spawner_source`) into instances at the nested transform.

use lodestone_render::{
    AnimInput, Camera, CameraUniform, EntityCameraUniform, InstanceTint, SpawnerMobSpawn,
    entity_model_matrix, plan_entities, spawner_display_outer_matrix, upload_instances_tinted,
};

use super::{EntityDrawBatch, RenderState};

impl RenderState {
    /// Rewrite the entity group-0 uniform (view-projection, fog, sky
    /// darkening) — the exact write `prepare_entities` makes internally, but
    /// callable unconditionally.
    ///
    /// `prepare_entities` only writes this when its own `entities` slice is
    /// non-empty, because there is nothing to draw through group 0 otherwise
    /// — and it early-returns before reaching the write. That is the correct
    /// call there, and the wrong one here: a spawner cage can be in view with
    /// **zero** ordinary entities on screen (an empty room, no other
    /// players), and this pass shares the *same* `self.entities.cam_buffer`
    /// and the *same* pipeline bind group `gpu/frame.rs`'s draw loop reuses —
    /// so if `prepare_entities` skipped the write, the spawner's mob would
    /// draw with whatever stale camera the buffer last held (or, on the very
    /// first frame, uninitialised). `gpu/frame.rs` calls this unconditionally
    /// before both passes, so a frame with entities present simply writes the
    /// identical bytes twice — cheap, and correct in both directions.
    pub(super) fn write_entity_camera_uniform(&self, queue: &wgpu::Queue, camera: &Camera) {
        let eye = camera.position;
        queue.write_buffer(
            &self.entities.cam_buffer,
            0,
            bytemuck::bytes_of(
                &EntityCameraUniform {
                    camera: CameraUniform {
                        view_proj: self.world_view_projection(camera).to_cols_array_2d(),
                        section_origin: [0.0, 0.0, 0.0, 0.0],
                    },
                    fog: self.fog_with_clock(eye),
                }
                .with_sky_darken(self.sky_darken.value()),
            ),
        );
    }

    /// Resolve this frame's spawner/trial-spawner display mobs into uploaded
    /// per-part instance buffers, ready to append into `prepare_entities`'
    /// own `Vec<EntityDrawBatch>` — see `gpu/frame.rs`.
    ///
    /// A spawn whose `entity_type` has no baked model (an unported mob, or a
    /// malformed id) is silently skipped, the same miss every other
    /// `resolve*` call in this codebase makes — a spawner drawing no mob
    /// reads as "empty cage" rather than a crash, matching vanilla's own
    /// `getEntityToSpawn().getString("id").isEmpty()` early-out.
    pub(super) fn prepare_spawner_mobs(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        spawns: &[SpawnerMobSpawn],
    ) -> Vec<EntityDrawBatch> {
        if spawns.is_empty() {
            return Vec::new();
        }

        // The entity's own placement at the origin — `entity_model_matrix`'s
        // flip/lift, with no translation, since the outer chain below
        // supplies the actual position. This is the second half of the
        // nesting `SpawnerRenderer.submitEntityInSpawner` produces by
        // handing an already-transformed pose stack to
        // `EntityRenderDispatcher.submit`: the outer matrix places and
        // spins the cage slot, and the entity's own renderer (here, this
        // same matrix) places the mob inside it.
        let entity_own_placement = entity_model_matrix(glam::Vec3::ZERO, 0.0, 1.0);

        let instances: Vec<_> = spawns
            .iter()
            .filter_map(|spawn| {
                let block_translate = glam::Mat4::from_translation(glam::Vec3::new(
                    spawn.pos[0] as f32,
                    spawn.pos[1] as f32,
                    spawn.pos[2] as f32,
                ));
                let outer = spawner_display_outer_matrix(spawn.spin_deg, spawn.scale);
                let transform = block_translate * outer * entity_own_placement;
                self.entities
                    .models
                    .resolve_at(&spawn.entity_type, transform, &AnimInput::REST)
                    .map(|instance| instance.with_light(spawn.light))
            })
            .collect();

        if instances.is_empty() {
            return Vec::new();
        }

        let frustum = camera.frustum();
        let frame = plan_entities(&instances, &frustum);

        // Same tail `prepare_entities` uses to turn a plan into uploaded
        // batches, minus the hurt/white-overlay/skin/variant grouping that
        // pass needs and this one does not: every display mob here is a
        // fresh, un-hurt, un-skinned instance, so one shared `InstanceTint::NONE`
        // covers the whole batch.
        frame
            .batches
            .iter()
            .map(|batch| {
                let count = u32::try_from(batch.transforms.len()).unwrap_or(u32::MAX);
                let tints = vec![InstanceTint::NONE; batch.transforms.len()];
                let parts = batch
                    .parts
                    .iter()
                    .map(|p| upload_instances_tinted(device, p, &batch.lights, &tints))
                    .collect();
                EntityDrawBatch {
                    model: batch.model,
                    count,
                    parts,
                    skin: None,
                    variant_sheet: None,
                }
            })
            .collect()
    }
}
