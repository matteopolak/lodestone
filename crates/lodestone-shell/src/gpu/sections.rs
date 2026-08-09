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
use std::sync::atomic::{AtomicBool, Ordering};

use lodestone_render::{DepthBuffer, GpuMesh, GpuModelMesh, Mesh, update_model_anim_buffer};

use crate::mesher::{SectionGeometry, SectionKey};

/// PERF INSTRUMENT: set to true on first `upload_section` to log first-mesh timing once.
static FIRST_SECTION_UPLOADED: AtomicBool = AtomicBool::new(false);

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
    ) {
        // PERF INSTRUMENT: log when the first section reaches the GPU
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
                // A remesh of an already-resident coord (the dirty-propagation
                // case) reuses that coord's origin slot rather than leaking it —
                // the origin is a pure function of `key`, so it never actually
                // changes. Its **arena spans** are not reusable, though — a
                // remesh changes the quad count — so the old spans are returned
                // to the free pool below, or the arena leaks one section's
                // geometry per remesh and fills up while walking around.
                let existing = model.sections.remove(&key);
                if let Some(old) = &existing {
                    free_resident(&mut model.mesh_arena, old.mesh.as_ref());
                    free_resident(&mut model.mesh_arena, old.water.as_ref());
                }
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
        // Drop its occlusion-graph entry too, or the graph is the one structure
        // here that only ever grows — the same shape as the leak issue #479 fixed
        // for `model.sections` and the origin arena. An absent coord reads as open
        // to the walk, so over-removing draws more and never less.
        self.forget_section_visibility(key.coord());
        if let Some(old) = self.sections.remove(key) {
            self.packed_origin_arena.free(old.origin_alloc);
        }
        if let Some(model) = self.model.as_mut()
            && let Some(old) = model.sections.remove(key)
        {
            free_resident(&mut model.mesh_arena, old.mesh.as_ref());
            free_resident(&mut model.mesh_arena, old.water.as_ref());
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
        let packed: u64 = self
            .sections
            .values()
            .map(|s| s.mesh.vertices.size() + s.mesh.indices.size())
            .sum();
        let model = self.model.as_ref().map_or(0, |m| {
            let dedicated: u64 = m
                .sections
                .values()
                .flat_map(|s| [s.mesh.as_ref(), s.water.as_ref()])
                .flatten()
                .map(dedicated_bytes)
                .sum();
            m.mesh_arena.live_bytes() + dedicated
        });
        (packed + model) as usize
    }

    /// Bytes of GPU mesh storage the **driver** is holding for terrain, as
    /// opposed to the [`resident_mesh_bytes`](Self::resident_mesh_bytes) actually
    /// occupied by live geometry.
    ///
    /// The difference is the model arena, which allocates in fixed 32 MiB +
    /// 8 MiB blocks and **never releases one** (a released block would invalidate
    /// every later block index still held by a resident section). So this is a
    /// high-water mark: walking away from a region returns its spans to the free
    /// pool, where the next region reuses them, and the reserved figure stays
    /// put. That is deliberate retention — freed mesh space is kept rather than
    /// handed back — and it is why there is no eviction budget here to tune.
    ///
    /// Watch the two together: `resident` sawtoothing while `reserved` is flat is
    /// healthy reuse. `reserved` climbing while `resident` is flat is
    /// fragmentation, and it is the only shape that would justify a budget.
    #[must_use]
    pub fn reserved_mesh_bytes(&self) -> usize {
        let packed: u64 = self
            .sections
            .values()
            .map(|s| s.mesh.vertices.size() + s.mesh.indices.size())
            .sum();
        let model = self.model.as_ref().map_or(0, |m| {
            let dedicated: u64 = m
                .sections
                .values()
                .flat_map(|s| [s.mesh.as_ref(), s.water.as_ref()])
                .flatten()
                .map(dedicated_bytes)
                .sum();
            m.mesh_arena.reserved_bytes() + dedicated
        });
        (packed + model) as usize
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

#[cfg(test)]
mod tests {
    use lodestone_render::{Camera, HeadlessTarget, RenderTarget};
    use lodestone_render::vertex::{BYTES_PER_INDEX, BYTES_PER_VERTEX, vram_bytes};

    use super::*;

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
                    let Some(snap) = crate::mesher::snapshot_section(&world, key) else {
                        continue;
                    };
                    let mesh = crate::mesher::mesh_snapshot(&snap, &classifier);
                    if mesh.indices.is_empty() {
                        continue;
                    }
                    expected_bytes += mesh.vertices.len() * BYTES_PER_VERTEX
                        + mesh.indices.len() * BYTES_PER_INDEX;
                    uploaded.push(key);
                    state.upload_section(
                        device,
                        queue,
                        key,
                        &crate::mesher::SectionGeometry::Packed(mesh),
                    );
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
            let snap = crate::mesher::snapshot_section(&world, *key).expect("re-snapshot");
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
