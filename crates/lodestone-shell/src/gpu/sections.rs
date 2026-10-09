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
//! never actually changes. That fix profiled the shape this replaced at 52.9%
//! of main-thread CPU; see `docs/section-camera-uniform.md`.
//!
//! # Why the accessors lend rather than re-upload
//!
//! `wgpu` resources are `Arc`-backed and a bind group keeps its own strong
//! reference, so a caller may build a bind group from one of these borrows and
//! outlive it. Uploading a second copy of the block atlas for the hotbar would
//! cost tens of megabytes to draw nine 16 px icons.
use std::sync::atomic::{AtomicBool, Ordering};

use lodestone_render::{DepthBuffer, GpuMesh, GpuModelMesh, Mesh, update_model_anim_buffer};

use crate::mesher::{SectionGeometry, SectionKey};

/// PERF INSTRUMENT: set to true on first `upload_section` to log first-mesh timing once.
static FIRST_SECTION_UPLOADED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionUploadOutcome {
    Applied,
    Unchanged,
    Failed,
}

use super::RenderState;
use super::terrain::{ModelSectionGpu, ResidentMesh, SectionGpu, anim_slots_at};

/// Upload one `ModelMesh` into the shared arena, falling back to a dedicated
/// buffer pair if the arena cannot place it.
///
/// `None` means the mesh was empty — the caller treats that as "this half of the
/// section has no geometry", which is what drops an all-air or all-solid section.
/// It never means "the upload failed": a failed *arena* placement degrades to
/// [`ResidentMesh::Dedicated`] rather than losing the section, because a silently
/// dropped section is a hole in the world that looks exactly like a meshing bug.
fn upload_resident(
    arena: &mut lodestone_render::ModelMeshArena,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mesh: &lodestone_render::models::ModelMesh,
) -> Option<ResidentMesh> {
    if mesh.indices.is_empty() {
        return None;
    }
    if let Some(placed) = arena.upload(device, queue, mesh) {
        return Some(ResidentMesh::Arena(placed));
    }
    tracing::warn!(
        vertices = mesh.vertices.len(),
        indices = mesh.indices.len(),
        "model mesh arena could not place a section mesh; falling back to a dedicated buffer"
    );
    GpuModelMesh::upload(device, mesh).map(ResidentMesh::Dedicated)
}

/// Return a resident mesh's arena spans to the free pool. A dedicated buffer pair
/// needs nothing: dropping it releases the `wgpu::Buffer`s.
fn free_resident(arena: &mut lodestone_render::ModelMeshArena, mesh: Option<&ResidentMesh>) {
    if let Some(ResidentMesh::Arena(span)) = mesh {
        arena.free(*span);
    }
}

/// Bytes a resident mesh occupies **outside** the arena's own bookkeeping.
///
/// An [`ResidentMesh::Arena`] span is already counted by
/// [`ModelMeshArena::live_bytes`](lodestone_render::ModelMeshArena::live_bytes),
/// so counting it here as well would double it; a
/// [`ResidentMesh::Dedicated`] pair is two `wgpu::Buffer`s of its own that no
/// arena knows about. Reading `Buffer::size()` rather than recomputing from a
/// quad count keeps this exact: the buffers were created from the mesh's own
/// slices and are the real footprint, padding included.
fn dedicated_bytes(mesh: &ResidentMesh) -> u64 {
    match mesh {
        ResidentMesh::Arena(_) => 0,
        ResidentMesh::Dedicated(m) => m.vertices.size() + m.indices.size(),
    }
}

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
    ) -> SectionUploadOutcome {
        let fingerprint = mesh.fingerprint();
        self.upload_section_fingerprinted(device, queue, key, mesh, fingerprint)
    }

    pub fn upload_meshed(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        meshed: &crate::mesher::Meshed,
    ) -> SectionUploadOutcome {
        self.upload_section_fingerprinted(device, queue, meshed.key, &meshed.mesh, meshed.fingerprint)
    }

    fn upload_section_fingerprinted(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: SectionKey,
        mesh: &SectionGeometry,
        fingerprint: u128,
    ) -> SectionUploadOutcome {
        if self.section_fingerprints.get(&key) == Some(&fingerprint) {
            return SectionUploadOutcome::Unchanged;
        }
        self.upload_section_uncached(device, queue, key, mesh);
        let applied = match mesh {
            SectionGeometry::Packed(mesh) => {
                mesh.vertices.is_empty()
                    || mesh.indices.is_empty()
                    || self.sections.contains_key(&key)
            }
            SectionGeometry::Model {
                opaque,
                water,
                translucent_blocks,
                ..
            } => self.model.as_ref().is_some_and(|model| {
                let resident = model.sections.get(&key);
                (opaque.indices.is_empty() || resident.is_some_and(|section| section.mesh.is_some()))
                    && (water.indices.is_empty()
                        || resident.is_some_and(|section| section.water.is_some()))
                    && (translucent_blocks.indices.is_empty()
                        || resident.is_some_and(|section| section.translucent.is_some()))
            }),
        };
        if applied {
            self.section_fingerprints.insert(key, fingerprint);
            SectionUploadOutcome::Applied
        } else {
            self.section_fingerprints.remove(&key);
            SectionUploadOutcome::Failed
        }
    }

    fn upload_section_uncached(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: SectionKey,
        mesh: &SectionGeometry,
    ) {
        if !FIRST_SECTION_UPLOADED.swap(true, Ordering::Relaxed) {
            tracing::info!(
                "first section uploaded to GPU: cx={} cz={} si={} min_y={}, {:?} quads",
                key.cx, key.cz, key.si, key.min_y,
                mesh.quad_count(),
            );
        }
        match mesh {
            SectionGeometry::Packed(mesh) => self.upload_packed_section(device, queue, key, mesh),
            SectionGeometry::Model {
                opaque,
                water,
                translucent_blocks,
                visibility,
            } => {
                // The occlusion graph (U3), recorded **before** the early
                // returns below and regardless of whether this section has any
                // geometry at all. A fully-enclosed underground section meshes to
                // nothing and is dropped from `model.sections` — and it is
                // precisely the section whose connectivity (nothing connects to
                // anything) stops the camera walk from descending. Recording only
                // sections that draw would leave the walk a world of open air.
                self.record_section_visibility(key.coord(), *visibility);
                let Some(model) = self.model.as_mut() else {
                    return;
                };
                let origin = key.origin();
                let origin_f = [origin[0] as f32, origin[1] as f32, origin[2] as f32];
                let opaque_gpu = upload_resident(&mut model.mesh_arena, device, queue, opaque);
                let water_gpu = upload_resident(&mut model.mesh_arena, device, queue, water);
                let translucent_gpu =
                    upload_resident(&mut model.mesh_arena, device, queue, translucent_blocks);
                // Replacements reuse the origin slot but release the old geometry spans.
                let existing = model.sections.remove(&key);
                if let Some(old) = &existing {
                    free_resident(&mut model.mesh_arena, old.mesh.as_ref());
                    free_resident(&mut model.mesh_arena, old.water.as_ref());
                    free_resident(&mut model.mesh_arena, old.translucent.as_ref());
                }
                if opaque_gpu.is_none() && water_gpu.is_none() && translucent_gpu.is_none() {
                    if let Some(old) = existing {
                        model.origin_arena.free(old.origin_alloc);
                    }
                    return;
                }
                let origin_alloc = match existing {
                    Some(old) => old.origin_alloc,
                    None => {
                        // Only unseen, distant sections fade; before the first camera
                        // sample, distance is unknown and fresh sections also fade.
                        let is_nearby = self
                            .last_camera_block_pos
                            .get()
                            .is_some_and(|camera_pos| {
                                lodestone_render::section_is_nearby(origin, camera_pos)
                            });
                        let build_time = if model.seen.contains(&key) || is_nearby {
                            lodestone_render::SECTION_FADE_ALREADY_VISIBLE
                        } else {
                            self.section_fade_tick.get() as f32 / 20.0
                        };
                        match model.origin_arena.alloc(queue, origin_f, build_time) {
                            Some((alloc, _offset)) => alloc,
                            None => {
                                free_resident(&mut model.mesh_arena, opaque_gpu.as_ref());
                                free_resident(&mut model.mesh_arena, water_gpu.as_ref());
                                free_resident(&mut model.mesh_arena, translucent_gpu.as_ref());
                                tracing::warn!(
                                    "section-origin arena exhausted at {key:?}; \
                                     dropping this section's geometry"
                                );
                                return;
                            }
                        }
                    }
                };
                model.seen.insert(key);
                model.sections.insert(
                    key,
                    ModelSectionGpu {
                        mesh: opaque_gpu,
                        quad_count: opaque.quad_count(),
                        water: water_gpu,
                        water_quad_count: water.quad_count(),
                        translucent: translucent_gpu,
                        translucent_quad_count: translucent_blocks.quad_count(),
                        origin_alloc,
                    },
                );
            }
        }
    }

    /// Upload a packed full-cube section (the demo path).
    ///
    /// Mirrors the model path above since that fix: the section's world origin
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
                    // The packed/demo path draws through `block.wgsl`, which
                    // never reads the origin's `w` lane at all — the sentinel
                    // is passed for honesty, not because it changes anything
                    // this path draws. The fade is model-path-only; see
                    // `upload_section`'s `SectionGeometry::Model` arm.
                    None => match self.packed_origin_arena.alloc(
                        queue,
                        origin_f,
                        lodestone_render::SECTION_FADE_ALREADY_VISIBLE,
                    ) {
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
        self.section_fingerprints.remove(key);
        // Drop its occlusion-graph entry too, or the graph is the one structure
        // here that only ever grows — the same shape as the leak that fix fixed
        // for `model.sections` and the origin arena. An absent coord reads as open
        // to the walk, so over-removing draws more and never less.
        self.forget_section_visibility(key.coord());
        if let Some(old) = self.sections.remove(key) {
            self.packed_origin_arena.free(old.origin_alloc);
        }
        if let Some(model) = self.model.as_mut() {
            // A genuine unload: forget this coord was ever seen, so a later
            // re-arrival (walking back into range) fades in again exactly
            // like a real vanilla `RenderSection` slot recycled onto a
            // different chunk address — see `ModelRenderer::seen`'s doc.
            model.seen.remove(key);
            if let Some(old) = model.sections.remove(key) {
                free_resident(&mut model.mesh_arena, old.mesh.as_ref());
                free_resident(&mut model.mesh_arena, old.water.as_ref());
                free_resident(&mut model.mesh_arena, old.translucent.as_ref());
                model.origin_arena.free(old.origin_alloc);
            }
        }
    }

    /// Number of uploaded (non-empty) sections.
    #[must_use]
    pub fn section_count(&self) -> usize {
        self.sections.len() + self.model.as_ref().map_or(0, |m| m.sections.len())
    }

    #[must_use]
    pub fn has_section(&self, key: &SectionKey) -> bool {
        self.sections.contains_key(key)
            || self.model.as_ref().is_some_and(|model| model.sections.contains_key(key))
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

    /// Exact bytes of GPU **mesh** storage currently handed out to resident
    /// sections — what the debug overlay's `MESH VRAM` reports.
    ///
    /// A function of *residency* alone, and that is the whole point. It consults
    /// the two section tables and the model arena's own occupancy, and nothing
    /// about the camera, the frustum or the cull, because the only two places
    /// that touch GPU mesh storage are [`upload_section`](Self::upload_section)
    /// and [`remove_section`](Self::remove_section) — neither of which a camera
    /// movement can reach. So a pure rotation must not move this number, and
    /// `mesh_vram_is_a_function_of_residency_not_of_the_camera` measures that it
    /// does not.
    ///
    /// It replaces `lodestone_render::vertex::vram_bytes(stats.total_quads)`,
    /// which was wrong twice over. `total_quads` accumulates only over sections
    /// that **survived the cull that frame**, so the reported figure moved every
    /// time the player turned on the spot — which reads as buffer churn and was
    /// reported as such, while nothing was being allocated or freed at all. And
    /// it priced every live-vanilla quad at the *packed* path's 72 B when a
    /// `ModelVertex` quad is 152 B (4 × 32 B + 6 × 4 B), understating real mesh
    /// VRAM by a further ~2.1×.
    ///
    /// Scope: mesh storage only. The atlases, the fixed `SectionOriginArena`
    /// pair (32 MiB + 2 MiB, allocated once at construction) and every
    /// entity/HUD buffer are out, because mesh storage is the part that scales
    /// with the world and so the only part worth watching per frame. Compare
    /// [`reserved_mesh_bytes`](Self::reserved_mesh_bytes) for what the driver is
    /// actually holding.
    #[must_use]
    pub fn resident_mesh_bytes(&self) -> usize {
        self.mesh_storage_bytes().0
    }

    /// Occupied and reserved terrain bytes from one residency walk.
    #[must_use]
    pub fn mesh_storage_bytes(&self) -> (usize, usize) {
        let packed: u64 = self
            .sections
            .values()
            .map(|s| s.mesh.vertices.size() + s.mesh.indices.size())
            .sum();
        let model = self.model.as_ref().map_or((0, 0), |m| {
            let dedicated: u64 = m
                .sections
                .values()
                .flat_map(|s| [s.mesh.as_ref(), s.water.as_ref(), s.translucent.as_ref()])
                .flatten()
                .map(dedicated_bytes)
                .sum();
            (m.mesh_arena.live_bytes() + dedicated, m.mesh_arena.reserved_bytes() + dedicated)
        });
        ((packed + model.0) as usize, (packed + model.1) as usize)
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

    /// Refresh the fade clock each frame and sprite uniforms when the game tick changes.
    pub fn update_animation(&self, queue: &wgpu::Queue, tick: u64) {
        self.section_fade_tick.set(tick);
        if let Some(model) = &self.model
            && model.animation_tick.replace(tick) != tick
        {
            let slots = anim_slots_at(&model.animations, tick);
            update_model_anim_buffer(queue, &model.anim_buffer, &slots);
        }
    }
}

#[cfg(test)]
mod tests {
    use lodestone_render::{Camera, HeadlessTarget, RenderTarget};
    use lodestone_render::vertex::{BYTES_PER_INDEX, BYTES_PER_VERTEX, vram_bytes};

    use super::*;

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn model_origin_exhaustion_releases_all_new_mesh_spans() {
        use lodestone_render::{BlockAtlas, BlockModels, BlocksJsonRegistry, ModelMesh, ModelVertex};
        use super::super::terrain::SectionOriginArena;

        let ctx = lodestone_render::GpuContext::new_headless_blocking()
            .expect("GPU regression opted in but no adapter is available");
        let device = ctx.device();
        let queue = ctx.queue();
        let mut texture = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut texture, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().unwrap()
                .write_image_data(&[255; 4]).unwrap();
        }
        let mut source = lodestone_assets::MemorySource::new("origin-exhaustion");
        source.insert("assets/minecraft/textures/block/water_still.png", texture);
        let manager = lodestone_assets::ResourceManager::new(vec![Box::new(source)]);
        let registry = BlocksJsonRegistry::from_slice(
            br#"{"test:empty":{"states":[{"id":0,"default":true}]}}"#,
        )
        .unwrap();
        let models = BlockModels::build_with_mip_levels(&manager, &registry, 0).unwrap();
        let atlas = BlockAtlas::build_with_mip_levels(&manager, &registry, 0)
            .unwrap()
            .with_models(models);
        let mut state = RenderState::new(
            device, queue, wgpu::TextureFormat::Rgba8Unorm, 32, 32, Some(&atlas),
        );
        let model = state.model.as_mut().expect("fixture has a model renderer");
        model.origin_arena = SectionOriginArena::new(device, queue, "two-origin-slots", 2);
        model.mesh_arena = lodestone_render::ModelMeshArena::with_block_sizes(4_096, 4_096);

        let quad = ModelMesh {
            vertices: [
                [0.0, 0.0, 0.0], [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
            ]
                .into_iter()
                .map(|position| ModelVertex {
                    position,
                    uv: [0.0, 0.0],
                    ao: 1.0,
                    light: 0xf0,
                    tint: 255,
                    anim: 0,
                    cutout_bypass: 0,
                    tint_rgb_override: [0; 4],
                })
                .collect(),
            indices: vec![0, 1, 2, 0, 2, 3],
        };
        let layer = |quads| {
            let mut mesh = ModelMesh::default();
            for _ in 0..quads {
                mesh.merge(&quad);
            }
            mesh
        };
        let mut geometry = SectionGeometry::Model {
            opaque: layer(1),
            water: layer(2),
            translucent_blocks: layer(3),
            visibility: lodestone_render::SectionVisibility::all(),
        };
        let key = |cx| SectionKey { cx, cz: 0, si: 0, min_y: 0 };
        assert_eq!(std::mem::size_of::<ModelVertex>(), 32);
        assert_eq!(std::mem::size_of::<u32>(), 4);
        let expected_bytes = 24 * 32 + 36 * 4;
        assert_eq!(expected_bytes, 912);
        assert_eq!(
            state.upload_section(device, queue, key(0), &geometry),
            SectionUploadOutcome::Applied,
        );
        assert_eq!(
            state.upload_section(device, queue, key(0), &geometry),
            SectionUploadOutcome::Unchanged,
        );
        assert_eq!(state.model.as_ref().unwrap().mesh_arena.live_bytes(), expected_bytes);
        assert!(state.has_section(&key(0)));
        let first_origin = state.model.as_ref().unwrap().sections[&key(0)].origin_alloc;
        let mut target = HeadlessTarget::new(device, 32, 32, wgpu::TextureFormat::Rgba8Unorm);
        for (x, culling, expected_drawn) in [(8.0, true, 1), (4096.0, true, 0), (4096.0, false, 1)] {
            state.set_terrain_culling(culling);
            let camera = Camera { position: glam::Vec3::new(x, 8.0, 8.0), aspect: 1.0, ..Camera::default() };
            let frame = target.acquire().unwrap();
            let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
            assert_eq!((stats.sections_drawn, stats.water_sections_drawn, stats.translucent_sections_drawn),
                (expected_drawn, expected_drawn, expected_drawn));
            let culled = 1 - expected_drawn;
            assert_eq!((stats.sections_culled_distance, stats.water_sections_culled, stats.translucent_sections_culled),
                (culled, culled, culled));
            assert_eq!(stats.total_quads, expected_drawn * 6);
            assert_eq!(stats.vram_bytes, 912);
            assert_eq!(state.mesh_storage_bytes(), (stats.vram_bytes, stats.vram_reserved_bytes));
        }
        state.set_terrain_culling(true);
        for cx in [1, 1, 2, 3] {
            assert_eq!(
                state.upload_section(device, queue, key(cx), &geometry),
                SectionUploadOutcome::Failed,
            );
            let model = state.model.as_ref().unwrap();
            assert_eq!(model.mesh_arena.live_bytes(), expected_bytes);
            assert_eq!(model.sections.len(), 1);
            assert_eq!(model.sections[&key(0)].origin_alloc, first_origin);
            assert!(!model.sections.contains_key(&key(cx)));
            assert!(!state.section_fingerprints.contains_key(&key(cx)));
            assert!(!state.has_section(&key(cx)));
        }
        let SectionGeometry::Model { visibility, .. } = &mut geometry else {
            unreachable!()
        };
        *visibility = lodestone_render::SectionVisibility::solid();
        assert_eq!(
            state.upload_section(device, queue, key(0), &geometry),
            SectionUploadOutcome::Applied,
        );
        assert_eq!(state.model.as_ref().unwrap().mesh_arena.live_bytes(), expected_bytes);
        assert_eq!(state.model.as_ref().unwrap().sections[&key(0)].origin_alloc, first_origin);
        state.remove_section(&key(0));
        assert!(!state.has_section(&key(0)));
        assert_eq!(state.model.as_ref().unwrap().mesh_arena.live_bytes(), 0);
        assert_eq!(
            state.upload_section(device, queue, key(1), &geometry),
            SectionUploadOutcome::Applied,
        );
        assert_eq!(
            state.upload_section(device, queue, key(1), &geometry),
            SectionUploadOutcome::Unchanged,
        );
        assert_eq!(state.model.as_ref().unwrap().mesh_arena.live_bytes(), expected_bytes);
        state.remove_section(&key(1));
        assert_eq!(state.model.as_ref().unwrap().mesh_arena.live_bytes(), 0);
    }

    /// **Rotating the camera must not move the reported mesh VRAM, and the
    /// pre-fix formula must move.** Both hypotheses are computed in the same run,
    /// so nothing here rests on a description of what the old code would have
    /// done.
    ///
    /// A pure rotation is the discriminating input, and it is the only one: a
    /// *step* changes which columns the server has sent, so residency legitimately
    /// changes with it and a walking gate could not separate "the counter is
    /// derived from the cull" from "the world really did stream". Turning on the
    /// spot cannot allocate or free GPU mesh storage — [`RenderState::upload_section`]
    /// and [`RenderState::remove_section`] are the only two paths that can, and a
    /// camera reaches neither — so the correct prediction is *byte-identical*, with
    /// no tolerance.
    ///
    /// Three assertions, in the order this repo's doctrine asks for:
    ///
    /// 1. *precondition* — `sections_drawn` and `total_quads` must genuinely
    ///    **differ** between the two yaws, or the cull is not responding and every
    ///    later assertion passes vacuously (the input where both hypotheses
    ///    coincide).
    /// 2. *the fix* — `vram_bytes` is identical across the two frames and equals
    ///    the byte total predicted from the uploaded meshes' own vertex and index
    ///    counts, computed here rather than read back from the accessor.
    /// 3. *the wrong hypothesis* — `vram_bytes(total_quads)`, exactly what this
    ///    field used to hold, differs between the two frames. That is the control,
    ///    and it fires in the same run.
    ///
    /// Then a there-and-back: dropping a column's sections and re-uploading them
    /// must return the byte total to the *same* value, not merely to a similar
    /// one — the counter that would catch a leak or a double-count in the
    /// remesh/eviction path.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn mesh_vram_is_a_function_of_residency_not_of_the_camera() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let mut target = HeadlessTarget::new(device, w, h, format);

        let world = crate::worldgen::generate(2);
        let classifier = crate::blocks::DemoClassifier;
        let mut state = RenderState::new(device, queue, format, w, h, None);

        // The expected byte total, accumulated from each mesh's own element counts
        // as it is uploaded. `PackedVertex` is 12 B and an index is 4 B, and
        // `create_buffer_init` pads only to 4, so `12 * v + 4 * i` is the exact
        // footprint rather than an estimate.
        assert_eq!((BYTES_PER_VERTEX, BYTES_PER_INDEX), (12, 4));
        let mut expected_bytes = 0usize;
        let mut uploaded: Vec<SectionKey> = Vec::new();
        let radius = 2;
        for cz in -radius..=radius {
            for cx in -radius..=radius {
                for si in 0..crate::worldgen::SECTION_COUNT {
                    let key = SectionKey {
                        cx,
                        cz,
                        si,
                        min_y: crate::worldgen::MIN_Y,
                    };
                    let Some(snap) = crate::mesher::snapshot_section(&world, key, Default::default()) else {
                        continue;
                    };
                    let mesh = crate::mesher::mesh_snapshot(&snap, &classifier);
                    if mesh.indices.is_empty() {
                        continue;
                    }
                    expected_bytes += mesh.vertices.len() * BYTES_PER_VERTEX
                        + mesh.indices.len() * BYTES_PER_INDEX;
                    uploaded.push(key);
                    let geometry = crate::mesher::SectionGeometry::Packed(mesh);
                    assert_eq!(
                        state.upload_section(device, queue, key, &geometry),
                        SectionUploadOutcome::Applied
                    );
                    if uploaded.len() == 1 {
                        assert_eq!(
                            state.upload_section(device, queue, key, &geometry),
                            SectionUploadOutcome::Unchanged
                        );
                        let crate::mesher::SectionGeometry::Packed(mesh) = &geometry else {
                            unreachable!()
                        };
                        let mut changed = mesh.clone();
                        changed.vertices[0].words[2] ^= 1;
                        let changed = crate::mesher::SectionGeometry::Packed(changed);
                        assert_eq!(
                            state.upload_section(device, queue, key, &changed),
                            SectionUploadOutcome::Applied
                        );
                        assert_eq!(
                            state.upload_section(device, queue, key, &changed),
                            SectionUploadOutcome::Unchanged
                        );
                        assert_eq!(
                            state.upload_section(device, queue, key, &geometry),
                            SectionUploadOutcome::Applied
                        );
                    }
                }
            }
        }
        assert!(!uploaded.is_empty(), "some sections must have meshed");
        assert_eq!(
            state.resident_mesh_bytes(),
            expected_bytes,
            "resident bytes must equal the uploaded meshes' own footprint"
        );

        // Same eye, two facings. Pitch 0 so the horizon splits the frame and a
        // large part of the world is behind the camera at one of them.
        let feet = crate::worldgen::spawn_feet();
        let eye = glam::Vec3::new(feet[0] as f32, feet[1] as f32 + 4.0, feet[2] as f32);
        let camera_at = |yaw: f32| Camera {
            position: eye,
            yaw,
            pitch: 0.0,
            fov_y_degrees: 70.0,
            aspect: w as f32 / h as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };

        // Collected, not asserted per iteration: a failure in the first facing
        // would otherwise abort before the second is even measured, and the whole
        // claim is about the pair.
        let mut frames = Vec::new();
        for yaw in [0.0_f32, 180.0] {
            let frame = target.acquire().expect("headless acquire");
            let stats = state.render(device, queue, frame.view(), &camera_at(yaw), None, &[]);
            frames.push((yaw, stats));
        }
        let (a, b) = (&frames[0].1, &frames[1].1);

        eprintln!("=== mesh VRAM vs the camera ===");
        for (yaw, s) in &frames {
            eprintln!(
                "yaw {yaw:>5.0}: drawn {:>4} quads {:>7} vram {:>9} reserved {:>9} \
                 (pre-fix estimate {:>9})",
                s.sections_drawn,
                s.total_quads,
                s.vram_bytes,
                s.vram_reserved_bytes,
                vram_bytes(s.total_quads),
            );
        }

        // 1. Precondition: the cull really is responding to the rotation. Without
        //    this the two frames could agree for the uninteresting reason.
        assert_ne!(
            a.sections_drawn, b.sections_drawn,
            "the two facings must draw different section counts, or this input \
             cannot tell a residency figure from a cull-derived one"
        );
        assert_ne!(a.total_quads, b.total_quads);

        // 2. The fix: identical, and equal to the independently accumulated total.
        assert_eq!(
            a.vram_bytes, b.vram_bytes,
            "a pure rotation moved the reported mesh VRAM: {} at yaw 0 vs {} at \
             yaw 180 — nothing between the two frames allocated or freed GPU mesh \
             storage",
            a.vram_bytes, b.vram_bytes
        );
        assert_eq!(a.vram_bytes, expected_bytes);
        assert!(a.vram_reserved_bytes >= a.vram_bytes);

        // 3. The control, in the same run: the formula this field used to hold
        //    disagrees between the two frames, so the gate above is not passing
        //    because the cull happens to be inert.
        assert_ne!(
            vram_bytes(a.total_quads),
            vram_bytes(b.total_quads),
            "the pre-fix estimate must differ across the two facings — if it does \
             not, this test proves nothing about which quantity is being reported"
        );

        // There and back: drop one column's sections, then re-upload them. The
        // total must land on the *same* byte count, and the intermediate must be
        // strictly smaller so the removal is not itself a no-op.
        let column: Vec<SectionKey> = uploaded
            .iter()
            .copied()
            .filter(|k| (k.cx, k.cz) == (0, 0))
            .collect();
        assert!(!column.is_empty(), "column (0,0) must hold sections");
        for key in &column {
            state.remove_section(key);
        }
        let after_removal = state.resident_mesh_bytes();
        assert!(
            after_removal < expected_bytes,
            "removing a column freed nothing: {after_removal} vs {expected_bytes}"
        );
        for key in &column {
            let snap = crate::mesher::snapshot_section(&world, *key, Default::default()).expect("re-snapshot");
            let mesh = crate::mesher::mesh_snapshot(&snap, &classifier);
            state.upload_section(
                device,
                queue,
                *key,
                &crate::mesher::SectionGeometry::Packed(mesh),
            );
        }
        assert_eq!(
            state.resident_mesh_bytes(),
            expected_bytes,
            "a remove-then-upload cycle must return the byte total exactly, not \
             approximately — a drift here is a leak or a double-count"
        );
    }
}
