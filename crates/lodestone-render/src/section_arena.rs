//! GPU residency for section meshes: suballocated upload and eviction.
//!
//! [`build_batch`](crate::mesher::build_batch) produces [`BuiltSection`]s off the
//! render thread; this module is where they become GPU-resident and turn into
//! the [`DrawRegion`]s [`WorldScene`](crate::scene::WorldScene) culls. Every
//! section's vertices and indices are suballocated out of two shared
//! [`ArenaBuffer`]s rather than getting a `wgpu::Buffer` each — one buffer per
//! section would be thousands of tiny allocations and thousands of bind points.
//!
//! ## Why two arenas and how the [`DrawRegion`] is derived
//!
//! An indexed draw needs three numbers into the shared buffers: `base_vertex`
//! (in *vertex* units), `first_index` (in *index* units), and `index_count`. The
//! arenas hand out *byte* offsets, so [`draw_region_for`] converts them. That
//! conversion is exact only if vertex offsets are multiples of
//! [`BYTES_PER_VERTEX`] and index offsets are multiples of
//! [`INDEX_SIZE`](self::INDEX_SIZE) — which holds because a mesh's vertex byte
//! length is inherently `n * BYTES_PER_VERTEX` and its index byte length is
//! `n * 4`, and the arena's alignment (4) divides both. The arithmetic is a pure
//! function, tested without a GPU; only the `queue.write_buffer` path needs one.
//!
//! ## Upload without stalling; free on unload
//!
//! [`SectionArena::upload`] writes through `Queue::write_buffer`, which stages
//! into wgpu's internal ring and copies at the next submit — it does not block
//! on the GPU, so meshes built on a worker land without a pipeline stall.
//! [`SectionArena::evict`] returns both spans to the free pool (coalescing
//! neighbours), so an unloaded chunk's memory is reclaimed for the next stream-in.
//! Re-uploading a coord that is already resident evicts the stale span first, so
//! a remesh (the dirty-propagation case) never leaks.

use std::collections::HashMap;

use crate::arena::{ArenaAllocation, ArenaBuffer, ArenaError};
use crate::mesh::Mesh;
use crate::strategy::DrawRegion;
use crate::vertex::BYTES_PER_VERTEX;
use crate::visibility::SectionCoord;

/// Bytes per mesh index (`u32`).
pub const INDEX_SIZE: u64 = core::mem::size_of::<u32>() as u64;

/// Derive the indexed-draw [`DrawRegion`] for a mesh whose vertex bytes start at
/// `vertex_offset` and index bytes at `index_offset` within the shared arenas.
///
/// `base_vertex` and `first_index` are element counts, so the byte offsets are
/// divided by the element size. The conversion is exact because section meshes
/// only ever allocate whole-element spans (see the module docs).
///
/// # Panics
/// Debug-panics if an offset is not a whole number of elements, which would mean
/// the arena alignment invariant was violated upstream.
#[must_use]
pub fn draw_region_for(
    vertex_offset: u64,
    index_offset: u64,
    index_count: u32,
    instance: u32,
) -> DrawRegion {
    debug_assert_eq!(
        vertex_offset % BYTES_PER_VERTEX as u64,
        0,
        "vertex offset must be a whole number of vertices"
    );
    debug_assert_eq!(
        index_offset % INDEX_SIZE,
        0,
        "index offset must be a whole number of indices"
    );
    DrawRegion {
        first_index: (index_offset / INDEX_SIZE) as u32,
        index_count,
        base_vertex: (vertex_offset / BYTES_PER_VERTEX as u64) as i32,
        instance,
        visible: false,
    }
}

/// The two arena spans backing one resident section, plus its draw region.
#[derive(Debug, Clone, Copy)]
struct Residence {
    vertices: ArenaAllocation,
    indices: ArenaAllocation,
    region: DrawRegion,
}

/// Shared GPU residency for all section meshes: a vertex arena, an index arena,
/// and the per-section spans carved from them.
///
/// This is the mesh-lifecycle's GPU half. Its inputs are [`Mesh`]es (built
/// off-thread); its outputs are [`DrawRegion`]s to register with
/// [`WorldScene`](crate::scene::WorldScene). It performs no culling and holds no
/// connectivity — that is the scene's job.
#[derive(Debug)]
pub struct SectionArena {
    vertices: ArenaBuffer,
    indices: ArenaBuffer,
    resident: HashMap<SectionCoord, Residence>,
}

impl SectionArena {
    /// Create arenas sized for `vertex_capacity` and `index_capacity` bytes.
    ///
    /// Both capacities are rounded up so they hold a whole number of elements.
    /// Both arenas use 4-byte ([`INDEX_SIZE`]) alignment — the largest power of
    /// two that divides both element sizes. The vertex-offset-divisible-by-12
    /// invariant [`draw_region_for`] relies on comes not from arena alignment
    /// but from every vertex span being `n * BYTES_PER_VERTEX` bytes: a sum of
    /// multiples of 12 (which 4-rounding never disturbs) keeps every offset a
    /// multiple of 12. The vertex arena is created with `VERTEX`, the index arena
    /// with `INDEX` usage (both gain `COPY_DST` for uploads).
    #[must_use]
    pub fn new(device: &wgpu::Device, vertex_capacity: u64, index_capacity: u64) -> Self {
        let vcap = round_up(vertex_capacity, BYTES_PER_VERTEX as u64);
        let icap = round_up(index_capacity, INDEX_SIZE);
        Self {
            vertices: ArenaBuffer::new(
                device,
                "section-vertices",
                vcap,
                INDEX_SIZE,
                wgpu::BufferUsages::VERTEX,
            ),
            indices: ArenaBuffer::new(
                device,
                "section-indices",
                icap,
                INDEX_SIZE,
                wgpu::BufferUsages::INDEX,
            ),
            resident: HashMap::new(),
        }
    }

    /// Upload `mesh` for `coord`, returning its [`DrawRegion`].
    ///
    /// If `coord` was already resident its old spans are freed first, so a
    /// remesh replaces in place without leaking. An empty mesh (an air section,
    /// or one fully occluded away) allocates nothing and yields a zero-count
    /// region — [`WorldScene`](crate::scene::WorldScene) treats that as "loaded
    /// but nothing to draw", which still routes the visibility walk.
    ///
    /// `instance` is the per-section instance index the draw is issued with
    /// (e.g. into a per-section transform array).
    ///
    /// # Errors
    /// Returns [`ArenaError`] if either arena is exhausted. On a vertex-side
    /// failure nothing is left allocated; on an index-side failure the vertex
    /// span is rolled back so a failed upload never half-occupies the arenas.
    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        coord: SectionCoord,
        mesh: &Mesh,
        instance: u32,
    ) -> Result<DrawRegion, ArenaError> {
        self.evict(coord);

        if mesh.indices.is_empty() || mesh.vertices.is_empty() {
            let region = DrawRegion {
                first_index: 0,
                index_count: 0,
                base_vertex: 0,
                instance,
                visible: false,
            };
            return Ok(region);
        }

        let vertex_bytes: &[u8] = bytemuck::cast_slice(&mesh.vertices);
        let index_bytes: &[u8] = bytemuck::cast_slice(&mesh.indices);

        let v_alloc = self.vertices.allocate(vertex_bytes.len() as u64)?;
        let i_alloc = match self.indices.allocate(index_bytes.len() as u64) {
            Ok(a) => a,
            Err(e) => {
                // Roll back the vertex span so a failed upload leaves no residue.
                let _ = self.vertices.free(v_alloc);
                return Err(e);
            }
        };

        self.vertices.write(queue, &v_alloc, vertex_bytes)?;
        self.indices.write(queue, &i_alloc, index_bytes)?;

        let region = draw_region_for(
            v_alloc.offset(),
            i_alloc.offset(),
            mesh.indices.len() as u32,
            instance,
        );
        self.resident.insert(
            coord,
            Residence {
                vertices: v_alloc,
                indices: i_alloc,
                region,
            },
        );
        Ok(region)
    }

    /// Free `coord`'s spans if resident. Returns `true` if it was resident.
    pub fn evict(&mut self, coord: SectionCoord) -> bool {
        if let Some(res) = self.resident.remove(&coord) {
            let _ = self.vertices.free(res.vertices);
            let _ = self.indices.free(res.indices);
            true
        } else {
            false
        }
    }

    /// The resident draw region for `coord`, if any.
    #[must_use]
    pub fn region(&self, coord: SectionCoord) -> Option<DrawRegion> {
        self.resident.get(&coord).map(|r| r.region)
    }

    /// Number of resident (non-empty) sections.
    #[must_use]
    pub fn resident_len(&self) -> usize {
        self.resident.len()
    }

    /// The shared vertex buffer, to bind before drawing.
    #[must_use]
    pub fn vertex_buffer(&self) -> &wgpu::Buffer {
        self.vertices.buffer()
    }

    /// The shared index buffer, to bind before drawing.
    #[must_use]
    pub fn index_buffer(&self) -> &wgpu::Buffer {
        self.indices.buffer()
    }

    /// Byte occupancy of the vertex arena.
    #[must_use]
    pub fn vertex_stats(&self) -> crate::suballoc::AllocStats {
        self.vertices.stats()
    }

    /// Byte occupancy of the index arena.
    #[must_use]
    pub fn index_stats(&self) -> crate::suballoc::AllocStats {
        self.indices.stats()
    }
}

/// Round `value` up to the next multiple of `mult` (a power-of-two-free helper;
/// `mult` is small and non-zero here).
fn round_up(value: u64, mult: u64) -> u64 {
    value.div_ceil(mult) * mult
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_region_converts_byte_offsets_to_element_counts() {
        // 10 vertices in = offset 120 bytes -> base_vertex 10; 30 indices in =
        // offset 120 bytes -> first_index 30.
        let region = draw_region_for(120, 120, 36, 7);
        assert_eq!(region.base_vertex, 10, "120 / 12 bytes-per-vertex");
        assert_eq!(region.first_index, 30, "120 / 4 bytes-per-index");
        assert_eq!(region.index_count, 36);
        assert_eq!(region.instance, 7);
        assert!(!region.visible, "regions start not-yet-culled");
    }

    #[test]
    fn draw_region_at_origin_is_all_zero_offsets() {
        let region = draw_region_for(0, 0, 6, 0);
        assert_eq!(region.base_vertex, 0);
        assert_eq!(region.first_index, 0);
        assert_eq!(region.index_count, 6);
    }

    #[test]
    fn round_up_snaps_to_element_multiples() {
        assert_eq!(round_up(0, 12), 0);
        assert_eq!(round_up(1, 12), 12);
        assert_eq!(round_up(12, 12), 12);
        assert_eq!(round_up(13, 12), 24);
        assert_eq!(round_up(7, 4), 8);
    }
}
