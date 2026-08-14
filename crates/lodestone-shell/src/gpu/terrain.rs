//! GPU-side terrain storage: the packed-path per-section table
//! ([`SectionGpu`]) and the live-vanilla model path's shared-camera renderer
//! ([`ModelRenderer`], [`ModelSectionGpu`], [`SectionOriginArena`]). See
//! `docs/section-camera-uniform.md` for the per-section-uniform perf fix this
//! shape exists for.
use std::collections::HashMap;

use lodestone_assets::ResourceLocation;
use lodestone_render::{
    AnimSlotUniform, ArenaAllocation, ArenaBuffer, ArenaMesh, GpuAtlas, GpuMesh, GpuModelMesh,
    ItemVariants, ModelMeshArena, ModelPipeline, SpriteAnimation,
    crack_pipeline::CrackPipeline, crack_resolver::CrackResolver, write_section_origin,
};

use crate::mesher::SectionKey;

/// One uploaded packed full-cube section (the demo/headless path).
///
/// Carries **no camera buffer or bind group of its own** since that fix:
/// `RenderState::packed_cam_bind_group` is shared by every packed section, and
/// `origin_alloc` is only this section's *slot* within
/// `RenderState::packed_origin_arena` — its offset is what selects this
/// section's origin at draw time via `set_bind_group`'s dynamic offset. Exactly
/// [`ModelSectionGpu`]'s shape, for exactly the reason
/// `docs/section-camera-uniform.md` gives.
#[derive(Debug)]
pub(super) struct SectionGpu {
    pub(super) mesh: GpuMesh,
    pub(super) quad_count: usize,
    /// This section's slot in `RenderState::packed_origin_arena`, written once
    /// at upload and freed when the section is removed or remeshed away.
    pub(super) origin_alloc: ArenaAllocation,
}

/// One uploaded section of wide baked-model geometry (the vanilla path). Mirrors
/// [`SectionGpu`] but holds a [`GpuModelMesh`] and draws through the
/// [`ModelPipeline`].
///
/// Unlike [`SectionGpu`], this carries no camera buffer or bind group of its
/// own: [`ModelRenderer::cam_bind_group`] is shared by every section (and by
/// the dropped-item pass), and `origin_alloc` is only this section's *slot*
/// within [`ModelRenderer::origin_arena`] — its offset is what selects this
/// section's origin at draw time via `set_bind_group`'s dynamic offset. See
/// `docs/section-camera-uniform.md`.
#[derive(Debug)]
pub(super) struct ModelSectionGpu {
    /// Opaque block geometry (with lava merged in), if any.
    pub(super) mesh: Option<ResidentMesh>,
    pub(super) quad_count: usize,
    /// Translucent water surface geometry for this section, if any. Drawn on the
    /// fluid pass after all opaque geometry so the sea floor shows through.
    pub(super) water: Option<ResidentMesh>,
    pub(super) water_quad_count: usize,
    /// This section's slot in [`ModelRenderer::origin_arena`], written once at
    /// upload. Freed (via [`SectionOriginArena::free`]) when the section is
    /// removed or remeshed away to nothing.
    pub(super) origin_alloc: ArenaAllocation,
}

/// Where one section's uploaded geometry actually lives.
///
/// Almost always [`Arena`](Self::Arena): a span suballocated out of
/// [`ModelMeshArena`]'s shared blocks, so the draw loop binds vertex and index
/// buffers **once per block** instead of once per section — 4 encoder calls per
/// draw down to 2, which is the whole point of that fix's second half.
///
/// [`Dedicated`](Self::Dedicated) is the degrade path, not a second design: an
/// arena that cannot place a mesh (one larger than a whole block, or a device that
/// refused another block) returns `None` and this section keeps its own buffer
/// pair rather than becoming a hole in the world. It costs the old 4 calls for
/// that one section and nothing else.
#[derive(Debug)]
pub(super) enum ResidentMesh {
    Arena(ArenaMesh),
    Dedicated(GpuModelMesh),
}

/// One resolved terrain draw: everything the encoder needs, with the cull
/// already applied.
///
/// Collected into a `Vec` and sorted by [`block`](Self::block) before emission so
/// that consecutive draws share a buffer bind. Sorting ~3k small structs per frame
/// costs far less than the binds it removes — and dedicated meshes carry
/// `block == u32::MAX`, so they sort to the end and never split an arena run.
pub(super) struct TerrainDraw<'a> {
    pub(super) block: u32,
    pub(super) first_index: u32,
    pub(super) index_count: u32,
    pub(super) base_vertex: i32,
    /// This section's dynamic offset into [`SectionOriginArena`].
    pub(super) origin_offset: u32,
    /// `Some` only for [`ResidentMesh::Dedicated`], whose buffers are bound
    /// per draw.
    pub(super) dedicated: Option<&'a GpuModelMesh>,
    /// Squared distance from the camera to this section's centre, for the
    /// translucent pass's back-to-front order (U5). Left at `0.0` by the opaque
    /// pass, which orders by [`block`](Self::block) instead — see
    /// [`sort_back_to_front`].
    pub(super) sort_dist2: f32,
}

/// The `block` sentinel for a mesh in its own buffers — sorts last.
pub(super) const DEDICATED_BLOCK: u32 = u32::MAX;

impl<'a> TerrainDraw<'a> {
    pub(super) fn new(mesh: &'a ResidentMesh, origin_offset: u32) -> Self {
        match mesh {
            ResidentMesh::Arena(a) => TerrainDraw {
                block: a.block,
                first_index: a.first_index,
                index_count: a.index_count,
                base_vertex: a.base_vertex,
                origin_offset,
                dedicated: None,
                sort_dist2: 0.0,
            },
            ResidentMesh::Dedicated(m) => TerrainDraw {
                block: DEDICATED_BLOCK,
                first_index: 0,
                index_count: m.index_count,
                base_vertex: 0,
                origin_offset,
                dedicated: Some(m),
                sort_dist2: 0.0,
            },
        }
    }
}

/// Squared distance from `camera` to the centre of the section at `coord`.
///
/// The centre, not the near corner: vanilla sorts its translucent sections on
/// `RenderSection`'s own centre distance, and a near-corner metric flips the order
/// of two sections whose corners and centres disagree — which is a visible seam
/// rather than a subtle one, since the two draws blend into each other.
#[must_use]
pub(super) fn section_center_distance_sq(
    coord: lodestone_render::SectionCoord,
    camera: glam::Vec3,
) -> f32 {
    let centre = glam::Vec3::new(
        coord.0 as f32 * 16.0 + 8.0,
        coord.1 as f32 * 16.0 + 8.0,
        coord.2 as f32 * 16.0 + 8.0,
    );
    (centre - camera).length_squared()
}

/// Order a translucent pass's draws **farthest first** (U5).
///
/// `total_cmp` rather than `partial_cmp().unwrap()`: a `NaN` here would panic
/// inside the frame loop, and there is no sensible camera position that should
/// take the renderer down. `total_cmp` orders NaN deterministically instead, so
/// the worst case is one badly ordered water section.
pub(super) fn sort_back_to_front(draws: &mut [TerrainDraw<'_>]) {
    draws.sort_unstable_by(|a, b| b.sort_dist2.total_cmp(&a.sort_dist2));
}

/// Generous fixed capacity for [`SectionOriginArena`]; see that type's doc for
/// the sizing rationale.
pub(super) const MODEL_ORIGIN_ARENA_SLOTS: u64 = 131_072;

/// Capacity for the **packed** path's own [`SectionOriginArena`].
///
/// Two orders of magnitude smaller than [`MODEL_ORIGIN_ARENA_SLOTS`] on purpose:
/// the packed pipeline only ever holds the offline demo world, whose extent is
/// capped by `sim.rs`'s `MAX_WORLD_RADIUS = 6` at roughly 4056 sections. 8192
/// slots is a clean power of two comfortably above that, and costs 2 MiB at the
/// 256 B dynamic-offset stride rather than the model arena's 32 MiB — which
/// matters because this arena is allocated on **every** run, live play included,
/// where the packed table stays empty.
pub(super) const PACKED_ORIGIN_ARENA_SLOTS: u64 = 8_192;

/// Shared GPU storage for every live model section's world origin (group 0
/// binding 1 of the model/fluid pipelines), addressed by a dynamic offset at
/// draw time instead of one buffer + one bind group per section. See the module doc and
/// [`ModelSharedCameraUniform`](lodestone_render::ModelSharedCameraUniform)'s
/// doc for the profile that motivated it.
///
/// Slot 0 is permanently reserved and zeroed at construction: the
/// dropped-item and first-person-held-item passes bind it (their geometry
/// already carries world positions baked into its vertices, so their
/// "origin" is always zero), so they share this arena's buffer through
/// [`ModelRenderer::cam_bind_group`] rather than needing one of their own.
///
/// # Capacity
///
/// [`MODEL_ORIGIN_ARENA_SLOTS`] is a fixed ceiling, not a growable one. At the
/// device-reported dynamic-uniform-offset stride (`min_uniform_buffer_offset_alignment`,
/// 256 B on every backend this client targets), 131072 slots cost 32 MiB —
/// comfortably above both the demo world's own hard cap (its section count is
/// bounded by a `MAX_WORLD_RADIUS` of 6 chunks, ~4056 sections) and vanilla's
/// maximum view distance (32 chunks), whose worst case — every column
/// populated top to bottom — is still under 101k sections, the measured live
/// run peaked near 5000, and the vast majority of far columns are not fully
/// populated in practice. Should a pathological world still exhaust it,
/// [`alloc`](Self::alloc) returns `None` rather than panicking, and the caller
/// drops that one section's geometry (a visible gap, logged once) instead of
/// crashing the client — see `upload_section`.
#[derive(Debug)]
pub(super) struct SectionOriginArena {
    arena: ArenaBuffer,
    stride: u64,
    zero_slot: ArenaAllocation,
}

impl SectionOriginArena {
    /// `label` names the backing buffer so a GPU capture distinguishes the model
    /// path's arena from the packed path's — the two are separate instances with
    /// very different capacities ([`MODEL_ORIGIN_ARENA_SLOTS`] vs
    /// [`PACKED_ORIGIN_ARENA_SLOTS`]).
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &str,
        capacity_slots: u64,
    ) -> Self {
        // wgpu requires a dynamic uniform-buffer offset to be a multiple of
        // this device limit (typically 256 B; never below `ArenaBuffer`'s own
        // floor), so every slot is padded out to it — checking the *limit*,
        // not the adapter, per this repo's own hard-won rule about the
        // 4-bind-group floor.
        let stride =
            (device.limits().min_uniform_buffer_offset_alignment as u64).max(ArenaBuffer::MIN_ALIGN);
        let mut arena = ArenaBuffer::new(
            device,
            label,
            capacity_slots * stride,
            stride,
            wgpu::BufferUsages::UNIFORM,
        );
        let zero_slot = arena
            .allocate(stride)
            .expect("a freshly created arena has room for its one reserved slot");
        write_section_origin(queue, arena.buffer(), zero_slot.offset(), [0.0, 0.0, 0.0]);
        Self {
            arena,
            stride,
            zero_slot,
        }
    }

    /// The backing buffer, bound whole as group 0 binding 1; a draw selects
    /// its section by the dynamic offset passed to `set_bind_group`.
    pub(super) fn buffer(&self) -> &wgpu::Buffer {
        self.arena.buffer()
    }

    /// The dynamic offset selecting the permanent zero-origin slot.
    pub(super) fn zero_offset(&self) -> u32 {
        self.zero_slot.offset() as u32
    }

    /// Allocate and write a fresh slot for a newly uploaded section, returning
    /// both the allocation (to free later) and the dynamic offset to draw
    /// with. `None` if the arena is exhausted — see the type's doc.
    pub(super) fn alloc(
        &mut self,
        queue: &wgpu::Queue,
        origin: [f32; 3],
    ) -> Option<(ArenaAllocation, u32)> {
        let slot = self.arena.allocate(self.stride).ok()?;
        write_section_origin(queue, self.arena.buffer(), slot.offset(), origin);
        let offset = slot.offset() as u32;
        Some((slot, offset))
    }

    /// Return a section's slot to the free pool.
    pub(super) fn free(&mut self, alloc: ArenaAllocation) {
        let _ = self.arena.free(alloc);
    }

    /// Slot occupancy against this arena's fixed capacity — used/free byte and
    /// allocation counts from the underlying [`ArenaBuffer`]. A narrow
    /// accessor rather than making [`SectionOriginArena`] itself `pub`: issue
    /// That fix/that fix want to watch how close realistic render distances get to
    /// [`MODEL_ORIGIN_ARENA_SLOTS`]'s fixed ceiling, and this is the seam a
    /// `lodestone-shell` bench uses for that (via
    /// [`RenderState::model_origin_arena_stats`]/
    /// [`RenderState::packed_origin_arena_stats`]) instead of widening this
    /// type's visibility.
    pub(super) fn stats(&self) -> lodestone_render::AllocStats {
        self.arena.stats()
    }
}

/// GPU resources for the model render pass: the model pipeline, the complete
/// stitched block atlas it samples (distinct from the packed cube atlas — its
/// UVs are what the baked quads index), and a per-section table of uploaded
/// model meshes. Present only on the live vanilla path; `None` on the demo path,
/// which meshes full cubes through the packed [`BlockPipeline`].
#[derive(Debug)]
pub(super) struct ModelRenderer {
    pub(super) pipeline: ModelPipeline,
    /// The translucent fluid pipeline (no cutout discard, water tint, alpha
    /// blend, depth-test on / depth-write off). Shares the model camera and
    /// atlas bind groups.
    pub(super) water_pipeline: ModelPipeline,
    #[allow(dead_code)]
    pub(super) atlas: GpuAtlas,
    pub(super) atlas_bind_group: wgpu::BindGroup,
    /// The tint palette (group 2) uploaded once: one RGBA multiplier per palette
    /// index, resolved from the pack's real colormaps. The model shader looks it
    /// up per tinted quad so grass, foliage and every other source get their own
    /// colour instead of one hardcoded green.
    pub(super) palette_bind_group: wgpu::BindGroup,
    /// The buffer behind [`Self::palette_bind_group`], kept so other consumers of
    /// the model shader — the HUD's 3-D item pass — can build their **own** bind
    /// group over the *same* palette rather than uploading a second copy. A
    /// hotbar icon and the world block it depicts then cannot drift apart.
    pub(super) palette_buffer: wgpu::Buffer,
    /// The animated block sprites' timelines paired with each slot's normalised
    /// frame height, cloned from the block models so the per-slot animation
    /// uniform can be rebuilt from the current game tick each frame via
    /// [`RenderState::update_animation`]. Ordered by slot id (entry `i` is slot
    /// `i + 1`); empty when the pack has no animated block sprites.
    pub(super) animations: Vec<(SpriteAnimation, f32)>,
    /// The per-slot animation uniform buffer (one [`AnimSlotUniform`] per slot,
    /// slot 0 static). Rewritten each frame from the game tick; both shaders
    /// sample it to offset an animated quad's V into its current frame.
    pub(super) anim_buffer: wgpu::Buffer,
    /// The animation bind group for the opaque model pipeline (its group 3).
    pub(super) anim_bind_group: wgpu::BindGroup,
    /// The animation bind group for the fluid pipeline (its group 2). Wraps the
    /// same [`Self::anim_buffer`]; only the group index differs.
    pub(super) water_anim_bind_group: wgpu::BindGroup,
    /// The mining-crack overlay pipeline (alpha-blended, depth-test only, pulled
    /// toward the camera by a negative depth bias so the `destroy_stage` texels
    /// win the depth test against the coplanar block face without z-fighting).
    pub(super) crack_pipeline: CrackPipeline,
    /// Per-state baked quads + the ten `destroy_stage` rects, captured from the
    /// block models so the target block's crack mesh can be built at draw time
    /// after `BlockModels` itself is dropped. Follows the block's real geometry
    /// (slabs, stairs, crosses), never a synthetic full cube.
    pub(super) crack_resolver: CrackResolver,
    /// The crack pass's atlas bind group. The crack pipeline has its own bind
    /// group layout, so it needs its own bind group over the same stitched
    /// model atlas the opaque pass uses.
    pub(super) crack_atlas_bind_group: wgpu::BindGroup,
    /// The crack pass's camera buffer + bind group. Crack meshes carry
    /// world-space positions (section origin zero), rewritten with the current
    /// `view_proj` each frame like the section uniforms.
    pub(super) crack_cam_buffer: wgpu::Buffer,
    pub(super) crack_cam_bind_group: wgpu::BindGroup,
    /// Baked inventory geometry for every item that has some, snapshotted here
    /// while `BlockModels` is still borrowable (exactly as
    /// [`CrackResolver::from_models`] snapshots the per-state quads, and for the
    /// same reason: the atlas is dropped after construction, so a per-frame
    /// borrow is not available).
    ///
    /// This is what lets a dropped item be drawn from inside
    /// [`RenderState::render`] with **no** new argument threaded through
    /// `app.rs`: the geometry is already here, and the only thing a frame has to
    /// supply is which item each drop is carrying, which rides on
    /// [`EntityDraw::item`].
    ///
    /// **[`ItemVariants`], not `ItemGeometry`** — every form the item can take,
    /// resolved per draw against an
    /// [`ItemStateContext`](lodestone_render::ItemStateContext). Snapshotting one
    /// geometry per item is what made a spyglass in the hand draw its flat
    /// inventory sprite, so the snapshot has to carry the axis, not a point on it.
    pub(super) items: HashMap<ResourceLocation, ItemVariants>,
    /// The shared group-0 buffer (binding 0: view-projection + this frame's
    /// fog), written **once per frame** by `update_model_shared_camera_buffer`
    /// in [`RenderState::render_inner`] — replacing what used to be one
    /// `queue.write_buffer` per *section*, per frame.
    pub(super) shared_cam_buffer: wgpu::Buffer,
    /// The bind group over [`Self::shared_cam_buffer`] and
    /// [`Self::origin_arena`], built **once** at construction and shared by
    /// every opaque/fluid section draw and the dropped-item pass — all of
    /// which share the world camera and this frame's fog. A draw picks its
    /// section by the dynamic offset it passes to `set_bind_group`, not by
    /// rebuilding this bind group. Dropped items (whose geometry already
    /// carries world positions baked into its vertices, like the crack pass's)
    /// draw with [`SectionOriginArena::zero_offset`].
    pub(super) cam_bind_group: wgpu::BindGroup,
    /// Per-section world origins, addressed by a dynamic offset — see
    /// [`SectionOriginArena`]'s doc.
    pub(super) origin_arena: SectionOriginArena,
    /// Shared vertex/index arena blocks backing every section's geometry, so the
    /// per-draw encoder cost is a bind + a draw rather than a bind + two buffer
    /// binds + a draw. See [`ResidentMesh`].
    pub(super) mesh_arena: ModelMeshArena,
    /// The **first-person held item** pass's own shared-camera buffer + bind
    /// group. Its `view_proj` is [`hand_projection`] *alone* (no view matrix)
    /// because the pose `first_person_item_mesh` bakes in is already
    /// camera-space — the same reason `EntityRenderer::hand_cam_buffer` exists
    /// for the bare arm. This is a separate buffer from [`Self::shared_cam_buffer`]
    /// because its `view_proj` genuinely differs (no world position), but its
    /// bind group's binding 1 still points at [`Self::origin_arena`], drawn
    /// with [`SectionOriginArena::zero_offset`] like the drop pass.
    pub(super) hand_cam_buffer: wgpu::Buffer,
    pub(super) hand_cam_bind_group: wgpu::BindGroup,
    pub(super) sections: HashMap<SectionKey, ModelSectionGpu>,
}

/// Build the per-slot animation uniform array for game `tick` from the snapshot
/// of animated sprite timelines. Index 0 is the static sentinel; index `s`
/// (`1..=len`) is slot `s`, its sampled region resolved into a V offset by the
/// slot's normalised frame height. Always yields at least the sentinel, so the
/// uniform buffer is never zero-sized.
#[cfg(test)]
mod tests {
    use super::*;

    fn draw(block: u32, sort_dist2: f32) -> TerrainDraw<'static> {
        TerrainDraw {
            block,
            first_index: 0,
            index_count: 6,
            base_vertex: 0,
            origin_offset: 0,
            dedicated: None,
            sort_dist2,
        }
    }

    /// U5: the translucent pass must submit **farthest first**, and it must do so
    /// regardless of the order the `HashMap` walk handed the sections over in —
    /// which is the actual bug, since that order changes when a chunk loads rather
    /// than when the camera moves.
    #[test]
    fn water_draws_are_ordered_farthest_first_from_any_input_order() {
        // Two input permutations of the same three distances. Both must come out
        // identical. The arena blocks are assigned so that block order and
        // distance order genuinely **disagree** — block 1 holds the nearest
        // section — which is what makes the second assertion below a real control
        // rather than a coincidence. (An earlier draft numbered the blocks in
        // distance order and the control passed vacuously.)
        let expected = [400.0_f32, 100.0, 25.0];
        for input in [
            vec![draw(1, 25.0), draw(4, 400.0), draw(7, 100.0)],
            vec![draw(7, 100.0), draw(1, 25.0), draw(4, 400.0)],
        ] {
            let mut draws = input;
            sort_back_to_front(&mut draws);
            let order: Vec<f32> = draws.iter().map(|d| d.sort_dist2).collect();
            assert_eq!(order, expected);
            // The both-hypotheses half: the opaque pass's own order is a different
            // permutation, so this assertion cannot pass by accident on a sort that
            // was never changed.
            let mut by_block = draws;
            by_block.sort_unstable_by_key(|d| d.block);
            let block_order: Vec<f32> = by_block.iter().map(|d| d.sort_dist2).collect();
            assert_ne!(block_order, expected);
        }
    }

    /// The metric is the section **centre**, and the expected values are computed
    /// here from the grid arithmetic rather than from the function.
    #[test]
    fn section_centre_distance_is_measured_from_the_centre() {
        // Section (0,0,0) spans blocks 0..16, so its centre is (8,8,8).
        let camera = glam::Vec3::new(8.0, 8.0, 8.0);
        assert_eq!(section_center_distance_sq((0, 0, 0), camera), 0.0);
        // Section (1,0,0)'s centre is (24,8,8): 16 blocks away, 256 squared.
        assert_eq!(section_center_distance_sq((1, 0, 0), camera), 256.0);
        // Negative rows too — `min_y` is negative in the overworld, and a
        // truncating divide here would put two rows on top of each other.
        // Section (0,-1,0)'s centre is (8,-8,8): also 16 away.
        assert_eq!(section_center_distance_sq((0, -1, 0), camera), 256.0);
    }
}

impl super::RenderState {
    /// Slot occupancy of the **model** path's per-section origin arena
    /// against its fixed [`MODEL_ORIGIN_ARENA_SLOTS`] ceiling — a `pub`
    /// accessor so a `lodestone-shell` bench can watch how close
    /// realistic render distances get to a ceiling that
    /// `docs/section-camera-uniform.md` documents as silently dropping
    /// geometry, not panicking, when exhausted. `None` on the demo path,
    /// which has no model renderer.
    #[must_use]
    pub fn model_origin_arena_stats(&self) -> Option<lodestone_render::AllocStats> {
        self.model.as_ref().map(|m| m.origin_arena.stats())
    }

    /// Same, for the packed/demo path's own origin arena,
    /// against its much smaller [`PACKED_ORIGIN_ARENA_SLOTS`] ceiling.
    #[must_use]
    pub fn packed_origin_arena_stats(&self) -> lodestone_render::AllocStats {
        self.packed_origin_arena.stats()
    }
}

pub(super) fn anim_slots_at(animations: &[(SpriteAnimation, f32)], tick: u64) -> Vec<AnimSlotUniform> {
    let mut slots = Vec::with_capacity(animations.len() + 1);
    slots.push(AnimSlotUniform::static_slot());
    for (animation, frame_v) in animations {
        slots.push(AnimSlotUniform::from_sample(
            animation.sample(tick),
            *frame_v,
        ));
    }
    slots
}
