//! Shared vertex/index arenas for **live-vanilla** section meshes, so a terrain
//! draw needs no per-section buffer bind.
//!
//! # What this is for
//!
//! Before this, every resident section owned two `wgpu::Buffer`s and each draw
//! cost four encoder calls: `set_bind_group` (dynamic offset),
//! `set_vertex_buffer`, `set_index_buffer`, `draw_indexed`. At the shipped render
//! distance 8 that is ~931 sections at a measured **19,024 instructions per
//! section** — 17.7M per frame (issue #543). Suballocating every mesh out of a
//! handful of shared buffers moves the two buffer binds **out of the per-section
//! loop**: the caller binds once per arena block and then issues bind+draw per
//! section, and thousands of small buffer objects collapse into a handful of
//! large ones.
//!
//! This is deliberately *not* multi-draw-indirect. `crate::strategy`'s module doc
//! records the measurement: wgpu 30 on Metal CPU-emulates base multi-draw as a
//! per-draw loop and exposes no `MULTI_DRAW_INDIRECT_COUNT`, so `PerDraw` is the
//! right strategy on this backend and the only reduction available is the encoder
//! state per draw.
//!
//! # Blocks, not one arena
//!
//! [`ArenaBuffer`] has a fixed capacity, and the resident set at render distance
//! 32 is roughly twelve times the render-distance-8 set — sizing one arena for
//! the worst case would reserve most of a gigabyte on every run, including the
//! ones that never leave spawn. So the arena is a **growable list of fixed-size
//! blocks**: allocation walks existing blocks and appends a new one only when
//! none fits. A block is a vertex arena paired with an index arena, allocated and
//! freed together, so one `block` index selects both buffers and the draw loop
//! only has to group by that single key.
//!
//! # The exactness that makes `base_vertex`/`first_index` legal
//!
//! An indexed draw into a shared buffer needs element counts, not byte offsets.
//! The conversion is exact here by construction, and both halves are load-bearing:
//!
//! * The vertex arena's alignment **is** [`MODEL_BYTES_PER_VERTEX`] (32, a power
//!   of two — `model_pipeline`'s own `array_stride` assertion pins it), so every
//!   vertex offset the allocator can hand out is a whole number of vertices.
//! * The index arena's alignment is 4, and every index span is `n * 4` bytes.
//!
//! Both are debug-asserted in [`ModelMeshArena::upload`] rather than assumed: a
//! vertex offset one byte off does not fail to draw, it draws *shifted geometry*,
//! which reads as a meshing bug several layers away from the cause.
//!
//! # Degrade, never panic
//!
//! A mesh larger than a whole block, or a device that refuses another block, makes
//! [`upload`](ModelMeshArena::upload) return `None`. The caller's contract is to
//! fall back to a dedicated per-section buffer for that one section — the same
//! degrade shape `SectionOriginArena` documents, and the reason
//! `ResidentModelMesh` in the shell is an enum rather than a single arena handle.

use crate::arena::{ArenaAllocation, ArenaBuffer};
use crate::models::{MODEL_BYTES_PER_VERTEX, ModelMesh};

/// Bytes per mesh index (`u32`).
pub const INDEX_SIZE: u64 = core::mem::size_of::<u32>() as u64;

/// Default vertex bytes per arena block (32 MiB = 1,048,576 vertices =
/// 262,144 quads).
///
/// Sized against the one recorded live figure — 441k quads at render distance 8
/// (`45a93e4`) — so a default-distance session lands in two or three blocks and a
/// render-distance-32 session grows to roughly twenty. Larger blocks would waste
/// VRAM on a session that never leaves spawn; smaller ones cost an extra pair of
/// buffer binds per frame each, which is the thing this module exists to reduce.
pub const DEFAULT_VERTEX_BLOCK_BYTES: u64 = 32 * 1024 * 1024;

/// Default index bytes per arena block.
///
/// A quad is 4 vertices (128 B) and 6 indices (24 B), so indices run at
/// `3/16` of vertices; 8 MiB against a 32-MiB vertex block is that ratio with
/// ~30% headroom, because a block runs out of whichever arena fills first and a
/// too-small index arena would strand vertex space.
pub const DEFAULT_INDEX_BLOCK_BYTES: u64 = 8 * 1024 * 1024;

/// One block: a vertex arena and the index arena that pairs with it.
#[derive(Debug)]
struct Block {
    vertices: ArenaBuffer,
    indices: ArenaBuffer,
}

/// A section mesh resident in the arena: which block, the two spans to free, and
/// the three numbers an indexed draw needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArenaMesh {
    /// Which block's buffer pair this mesh lives in — the draw loop's grouping
    /// key.
    pub block: u32,
    /// Index of the first index, within the block's index buffer.
    pub first_index: u32,
    /// Number of indices to draw.
    pub index_count: u32,
    /// Value added to every index to reach this mesh's vertices within the
    /// block's vertex buffer.
    pub base_vertex: i32,
    vertex_span: ArenaAllocation,
    index_span: ArenaAllocation,
}

impl ArenaMesh {
    /// Bytes this mesh occupies across both arenas (aligned spans, i.e. the real
    /// footprint rather than the mesh's logical size).
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.vertex_span.size() + self.index_span.size()
    }
}

/// A growable set of shared vertex/index arena blocks backing every resident
/// model section mesh.
#[derive(Debug)]
pub struct ModelMeshArena {
    blocks: Vec<Block>,
    vertex_block_bytes: u64,
    index_block_bytes: u64,
    live_bytes: u64,
}

impl ModelMeshArena {
    /// A new, empty arena with the default block sizes. The first block is
    /// allocated lazily on the first [`upload`](Self::upload), so a run with no
    /// live world reserves no VRAM at all.
    #[must_use]
    pub fn new() -> Self {
        Self::with_block_sizes(DEFAULT_VERTEX_BLOCK_BYTES, DEFAULT_INDEX_BLOCK_BYTES)
    }

    /// A new, empty arena with explicit block sizes. Tests use small blocks to
    /// exercise the multi-block and exhaustion paths without allocating tens of
    /// megabytes.
    #[must_use]
    pub fn with_block_sizes(vertex_block_bytes: u64, index_block_bytes: u64) -> Self {
        Self {
            blocks: Vec::new(),
            vertex_block_bytes,
            index_block_bytes,
            live_bytes: 0,
        }
    }

    /// How many blocks exist — i.e. how many buffer-pair binds a frame that draws
    /// from every block will issue.
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Total VRAM reserved by the blocks (as opposed to
    /// [`live_bytes`](Self::live_bytes), which is what is actually occupied).
    #[must_use]
    pub fn reserved_bytes(&self) -> u64 {
        self.blocks.len() as u64 * (self.vertex_block_bytes + self.index_block_bytes)
    }

    /// Bytes currently handed out to live meshes.
    #[must_use]
    pub fn live_bytes(&self) -> u64 {
        self.live_bytes
    }

    /// The vertex buffer of `block`, to bind once before drawing every mesh in it.
    #[must_use]
    pub fn vertex_buffer(&self, block: u32) -> Option<&wgpu::Buffer> {
        self.blocks.get(block as usize).map(|b| b.vertices.buffer())
    }

    /// The index buffer of `block`, paired with [`vertex_buffer`](Self::vertex_buffer).
    #[must_use]
    pub fn index_buffer(&self, block: u32) -> Option<&wgpu::Buffer> {
        self.blocks.get(block as usize).map(|b| b.indices.buffer())
    }

    /// Suballocate and upload `mesh`, or `None` if it is empty, does not fit in a
    /// whole block, or no further block could be created.
    ///
    /// `None` is a degrade signal, not an error to swallow: the caller must fall
    /// back to a dedicated buffer for that section or it becomes a hole in the
    /// world.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mesh: &ModelMesh,
    ) -> Option<ArenaMesh> {
        if mesh.indices.is_empty() || mesh.vertices.is_empty() {
            return None;
        }
        let vertex_bytes = core::mem::size_of_val(mesh.vertices.as_slice()) as u64;
        let index_bytes = core::mem::size_of_val(mesh.indices.as_slice()) as u64;
        if vertex_bytes > self.vertex_block_bytes || index_bytes > self.index_block_bytes {
            return None;
        }

        // First-fit across existing blocks, then a fresh one. A failed vertex
        // allocation must not leave an orphaned index span behind, hence the
        // rollback.
        for block_index in 0..self.blocks.len() {
            if let Some(m) = self.try_place(queue, block_index, mesh, vertex_bytes, index_bytes) {
                return Some(m);
            }
        }
        let block_index = self.push_block(device);
        self.try_place(queue, block_index, mesh, vertex_bytes, index_bytes)
    }

    fn push_block(&mut self, device: &wgpu::Device) -> usize {
        let n = self.blocks.len();
        self.blocks.push(Block {
            vertices: ArenaBuffer::new(
                device,
                "lodestone-model-vertex-arena",
                self.vertex_block_bytes,
                // The alignment that makes `base_vertex` exact — see the module
                // doc. 32 is a power of two, which `Suballocator::new` requires.
                MODEL_BYTES_PER_VERTEX as u64,
                wgpu::BufferUsages::VERTEX,
            ),
            indices: ArenaBuffer::new(
                device,
                "lodestone-model-index-arena",
                self.index_block_bytes,
                INDEX_SIZE,
                wgpu::BufferUsages::INDEX,
            ),
        });
        n
    }

    fn try_place(
        &mut self,
        queue: &wgpu::Queue,
        block_index: usize,
        mesh: &ModelMesh,
        vertex_bytes: u64,
        index_bytes: u64,
    ) -> Option<ArenaMesh> {
        let block = self.blocks.get_mut(block_index)?;
        let vertex_span = block.vertices.allocate(vertex_bytes).ok()?;
        let index_span = match block.indices.allocate(index_bytes) {
            Ok(span) => span,
            Err(_) => {
                // Roll the vertex span back rather than stranding it: a block
                // whose index arena is full is otherwise slowly emptied of
                // usable vertex space by every subsequent attempt.
                let _ = block.vertices.free(vertex_span);
                return None;
            }
        };
        debug_assert_eq!(
            vertex_span.offset() % MODEL_BYTES_PER_VERTEX as u64,
            0,
            "vertex offset must be a whole number of vertices, or every index in \
             this mesh addresses a vertex straddling two real ones"
        );
        debug_assert_eq!(index_span.offset() % INDEX_SIZE, 0);
        let _ = block
            .vertices
            .write(queue, &vertex_span, bytemuck::cast_slice(&mesh.vertices));
        let _ = block
            .indices
            .write(queue, &index_span, bytemuck::cast_slice(&mesh.indices));
        self.live_bytes += vertex_span.size() + index_span.size();
        Some(ArenaMesh {
            block: block_index as u32,
            first_index: (index_span.offset() / INDEX_SIZE) as u32,
            index_count: mesh.indices.len() as u32,
            base_vertex: (vertex_span.offset() / MODEL_BYTES_PER_VERTEX as u64) as i32,
            vertex_span,
            index_span,
        })
    }

    /// Return a mesh's spans to the free pool (coalescing neighbours). Blocks are
    /// never released: a freed block would invalidate every later block index
    /// still held by a resident section, and the stream-in/stream-out churn of
    /// walking around reuses the space immediately anyway.
    pub fn free(&mut self, mesh: ArenaMesh) {
        if let Some(block) = self.blocks.get_mut(mesh.block as usize) {
            self.live_bytes = self.live_bytes.saturating_sub(mesh.bytes());
            let _ = block.vertices.free(mesh.vertex_span);
            let _ = block.indices.free(mesh.index_span);
        }
    }
}

impl Default for ModelMeshArena {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_alignment_is_the_stride_and_is_a_power_of_two() {
        // The invariant the whole `base_vertex` derivation rests on. If
        // `ModelVertex` ever grows to a non-power-of-two size, `Suballocator::new`
        // panics rather than silently handing out unaligned offsets — but this
        // fails first, and says why.
        assert_eq!(MODEL_BYTES_PER_VERTEX, 32);
        assert!((MODEL_BYTES_PER_VERTEX as u64).is_power_of_two());
    }

    #[test]
    fn index_ratio_headroom_holds_for_the_default_block_sizes() {
        // A quad is 4 vertices and 6 indices, so a full vertex block implies this
        // many index bytes. The default index block must exceed it, or vertex
        // space is stranded.
        let quads = DEFAULT_VERTEX_BLOCK_BYTES / (4 * MODEL_BYTES_PER_VERTEX as u64);
        let implied_index_bytes = quads * 6 * INDEX_SIZE;
        assert!(
            DEFAULT_INDEX_BLOCK_BYTES > implied_index_bytes,
            "a full vertex block needs {implied_index_bytes} index bytes but the block only \
             has {DEFAULT_INDEX_BLOCK_BYTES}"
        );
    }
}
