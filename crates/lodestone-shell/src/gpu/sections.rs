//! Section residency and the resources this pass lends out.
//!
//! Uploading, replacing and removing per-section GPU meshes for **both**
//! terrain paths (the packed demo table and the live-vanilla model table —
//! see [`super::terrain`]), the depth buffer's resize hook, the animated-sprite
//! uniform's per-frame rewrite, and the read-only borrows the HUD's 3-D item
//! pass shares.
//!
//! # Nothing per-section is written per frame
//!
//! Both upload paths write a section's world origin **once**, into a slot of a
//! shared [`SectionOriginArena`], and a draw selects it by dynamic offset. A
//! remesh of an already-resident coord reuses that coord's slot rather than
//! leaking it — the origin is a pure function of the [`SectionKey`], so it
//! never actually changes. Issue #75 profiled the shape this replaced at 52.9%
//! of main-thread CPU; see `docs/section-camera-uniform.md`.
//!
//! # Why the accessors lend rather than re-upload
//!
//! `wgpu` resources are `Arc`-backed and a bind group keeps its own strong
//! reference, so a caller may build a bind group from one of these borrows and
//! outlive it. Uploading a second copy of the block atlas for the hotbar would
//! cost tens of megabytes to draw nine 16 px icons.
use lodestone_render::{DepthBuffer, GpuMesh, GpuModelMesh, Mesh, update_model_anim_buffer};

use crate::mesher::{SectionGeometry, SectionKey};

use super::RenderState;
use super::terrain::{ModelSectionGpu, SectionGpu, anim_slots_at};

impl RenderState {

    /// Recreate the depth buffer to match a resized target.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self.depth.width != width || self.depth.height != height {
            self.depth = DepthBuffer::new(device, width, height);
        }
    }

    /// Upload (or replace) a section's mesh. An empty mesh removes the section.
    ///
    /// Dispatches on the geometry variant: packed full-cube meshes (demo world)
    /// go to the packed [`BlockPipeline`] table; wide baked-model meshes (live
    /// vanilla world) go to the [`ModelRenderer`] table. A `Model` upload with no
    /// model renderer present (never happens in a consistent session, since the
    /// vanilla classifier and the model renderer are built from the same atlas)
    /// is a no-op.
    pub fn upload_section(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: SectionKey,
        mesh: &SectionGeometry,
    ) {
        match mesh {
            SectionGeometry::Packed(mesh) => self.upload_packed_section(device, queue, key, mesh),
            SectionGeometry::Model { opaque, water } => {
                let Some(model) = self.model.as_mut() else {
                    return;
                };
                let origin = key.origin();
                let origin_f = [origin[0] as f32, origin[1] as f32, origin[2] as f32];
                let opaque_gpu = GpuModelMesh::upload(device, opaque);
                let water_gpu = GpuModelMesh::upload(device, water);
                // A remesh of an already-resident coord (the dirty-propagation
                // case) reuses that coord's origin slot rather than leaking it —
                // the origin is a pure function of `key`, so it never actually
                // changes.
                let existing = model.sections.remove(&key);
                // A section may carry only opaque terrain, only water (an ocean
                // surface section with no solid blocks), or both. Drop it only
                // when neither has geometry.
                if opaque_gpu.is_none() && water_gpu.is_none() {
                    if let Some(old) = existing {
                        model.origin_arena.free(old.origin_alloc);
                    }
                    return;
                }
                let origin_alloc = match existing {
                    Some(old) => old.origin_alloc,
                    None => match model.origin_arena.alloc(queue, origin_f) {
                        Some((alloc, _offset)) => alloc,
                        None => {
                            // Should not happen — see `SectionOriginArena`'s
                            // doc for the capacity margin — but degrade to a
                            // dropped (missing) section rather than a panic if
                            // it ever does.
                            tracing::warn!(
                                "section-origin arena exhausted at {key:?}; \
                                 dropping this section's geometry"
                            );
                            return;
                        }
                    },
                };
                model.sections.insert(
                    key,
                    ModelSectionGpu {
                        mesh: opaque_gpu,
                        quad_count: opaque.quad_count(),
                        water: water_gpu,
                        water_quad_count: water.quad_count(),
                        origin_alloc,
                    },
                );
            }
        }
    }

    /// Upload a packed full-cube section (the demo path).
    ///
    /// Mirrors the model path above since issue #76: the section's world origin
    /// is written **once**, here, into a slot of the shared
    /// [`packed_origin_arena`](Self::packed_origin_arena), and a remesh of an
    /// already-resident coord reuses that slot rather than leaking it — the
    /// origin is a pure function of `key`, so it never actually changes. Nothing
    /// per-section is written per frame any more.
    fn upload_packed_section(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: SectionKey,
        mesh: &Mesh,
    ) {
        let existing = self.sections.remove(&key);
        match GpuMesh::upload(device, mesh) {
            None => {
                if let Some(old) = existing {
                    self.packed_origin_arena.free(old.origin_alloc);
                }
            }
            Some(gpu_mesh) => {
                let origin = key.origin();
                let origin_f = [origin[0] as f32, origin[1] as f32, origin[2] as f32];
                let origin_alloc = match existing {
                    Some(old) => old.origin_alloc,
                    None => match self.packed_origin_arena.alloc(queue, origin_f) {
                        Some((alloc, _offset)) => alloc,
                        None => {
                            // Should not happen — `PACKED_ORIGIN_ARENA_SLOTS` is
                            // sized twice over the demo world's own hard cap —
                            // but degrade to a dropped (missing) section rather
                            // than a panic if it ever does, exactly as the model
                            // path does.
                            tracing::warn!(
                                "packed section-origin arena exhausted at {key:?}; \
                                 dropping this section's geometry"
                            );
                            return;
                        }
                    },
                };
                self.sections.insert(
                    key,
                    SectionGpu {
                        mesh: gpu_mesh,
                        quad_count: mesh.quad_count(),
                        origin_alloc,
                    },
                );
            }
        }
    }

    /// Remove a section (e.g. an unloaded chunk).
    pub fn remove_section(&mut self, key: &SectionKey) {
        if let Some(old) = self.sections.remove(key) {
            self.packed_origin_arena.free(old.origin_alloc);
        }
        if let Some(model) = self.model.as_mut()
            && let Some(old) = model.sections.remove(key)
        {
            model.origin_arena.free(old.origin_alloc);
        }
    }

    /// Number of uploaded (non-empty) sections.
    #[must_use]
    pub fn section_count(&self) -> usize {
        self.sections.len() + self.model.as_ref().map_or(0, |m| m.sections.len())
    }

    /// The stitched **model** atlas's texture view — the atlas whose UVs every
    /// [`BakedQuad`](lodestone_assets::BakedQuad) indexes, terrain and 3-D item
    /// icons alike. `None` on the demo path, which has no baked models.
    ///
    /// Lent out (rather than re-uploaded) so a second consumer of the model
    /// shader — the HUD's 3-D item pass — samples the *same* GPU texture. `wgpu`
    /// resources are `Arc`-backed and a bind group keeps its own strong
    /// reference, so a caller may build a bind group from this borrow and outlive
    /// it. Uploading a second copy of the block atlas for the hotbar would cost
    /// tens of megabytes to draw nine 16 px icons.
    #[must_use]
    pub fn model_atlas_view(&self) -> Option<&wgpu::TextureView> {
        self.model.as_ref().map(|m| &m.atlas.view)
    }

    /// The model atlas's sampler, paired with [`Self::model_atlas_view`].
    #[must_use]
    pub fn model_atlas_sampler(&self) -> Option<&wgpu::Sampler> {
        self.model.as_ref().map(|m| &m.atlas.sampler)
    }

    /// The tint-palette uniform buffer the model shader reads at group 2. Shared
    /// so a hotbar icon's tinted faces (grass block, leaves) resolve through the
    /// same palette slots as the world block.
    #[must_use]
    pub fn model_palette_buffer(&self) -> Option<&wgpu::Buffer> {
        self.model.as_ref().map(|m| &m.palette_buffer)
    }

    /// The per-slot animation uniform buffer the model shader reads at group 3,
    /// rewritten every frame by [`update_animation`](Self::update_animation).
    ///
    /// Sharing it is what makes an animated **item** icon (magma block, sea
    /// lantern, prismarine) advance in lock-step with the same block in the
    /// world, for free: one buffer, one per-frame write, two readers.
    #[must_use]
    pub fn model_anim_buffer(&self) -> Option<&wgpu::Buffer> {
        self.model.as_ref().map(|m| &m.anim_buffer)
    }

    /// The depth attachment sized to the current target. Lent to the HUD's 3-D
    /// item pass, which needs a depth buffer for the near faces of an isometric
    /// mini-block to win over the far ones. That pass **clears** it, so it does
    /// not disturb the world depth already consumed earlier in the frame.
    #[must_use]
    pub fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth.view
    }

    /// Total merged quads currently resident on the GPU.
    #[must_use]
    pub fn total_quads(&self) -> usize {
        let packed: usize = self.sections.values().map(|s| s.quad_count).sum();
        let model: usize = self
            .model
            .as_ref()
            .map_or(0, |m| m.sections.values().map(|s| s.quad_count).sum());
        packed + model
    }

    /// Rewrite the animated-block uniform for the current game `tick`.
    ///
    /// Call once per frame *before* [`render`](Self::render) with the live game
    /// tick (`Sim::tick_count`). Each animated sprite slot is sampled at `tick`
    /// via the existing `anim.rs` timing and its resolved V offset uploaded, so
    /// the model/fluid shaders draw the correct frame. A no-op when there is no
    /// live-vanilla model pass (the offline demo path). Skipping it leaves every
    /// sprite on frame 0 — the pre-wiring behaviour — rather than erroring.
    pub fn update_animation(&self, queue: &wgpu::Queue, tick: u64) {
        if let Some(model) = &self.model {
            let slots = anim_slots_at(&model.animations, tick);
            update_model_anim_buffer(queue, &model.anim_buffer, &slots);
        }
    }
}
