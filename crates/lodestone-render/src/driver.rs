//! The world mesher: the driver that turns chunk-load and chunk-unload signals
//! into GPU-resident, cullable terrain.
//!
//! This is the seam that makes `lodestone-render` a *client* rather than a
//! demo. It owns the three pieces the mesh lifecycle needs and wires them into
//! one flow:
//!
//! ```text
//!   ChunkLoaded { pos }                     (dirty signal from the client)
//!        │
//!        ▼  dirty_jobs(source, cx, cz, ys)  gather 3×3×3 Arc snapshots off-lock
//!   [MeshJob]  ──► build_batch (rayon)  ──► [BuiltSection]   (meshed off-thread)
//!        │
//!        ▼  SectionArena::upload            suballocate + write, no GPU stall
//!   DrawRegion  ──► WorldScene::insert_section   (registered for culling)
//!        │
//!        ▼  WorldScene::plan_frame(camera)  frustum ∩ occlusion-walk
//!   FramePlan                                 (the visible draws)
//! ```
//!
//! It is deliberately generic over [`SectionSource`] and [`BlockClassifier`], so
//! it depends on neither `lodestone-client` nor a concrete block registry — an
//! application supplies both. The only GPU touch points are
//! [`WorldMesher::apply_built`] (arena uploads) and construction; the dirty-set
//! computation and instance-slot bookkeeping are pure and tested without a GPU.
//!
//! ## Instance slots
//!
//! Each resident section draws with a compact `instance` index so the shader can
//! read its per-section world transform from a tightly packed array.
//! [`InstanceTable`] hands out the lowest free index and recycles it on eviction,
//! keeping the array dense as chunks stream in and out.

use crate::camera::Camera;
use crate::mesher::SectionSource;
use crate::mesher::{BuiltSection, build_batch, dirty_jobs};
use crate::scene::{FramePlan, WorldScene};
use crate::section_arena::SectionArena;
use crate::visibility::SectionCoord;
use crate::world::BlockClassifier;

use std::collections::HashMap;

/// A dense allocator of per-section instance indices.
///
/// Draws issue with a small `instance` index into a per-section transform array;
/// this keeps that array packed by reusing the lowest freed slot before growing.
/// Pure and hermetically tested — it never touches the GPU.
#[derive(Debug, Default)]
pub struct InstanceTable {
    of_coord: HashMap<SectionCoord, u32>,
    free: Vec<u32>,
    next: u32,
}

impl InstanceTable {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The instance index for `coord`, allocating a fresh one if needed. Stable
    /// across re-uploads of the same coord (a remesh keeps its slot).
    pub fn get_or_insert(&mut self, coord: SectionCoord) -> u32 {
        if let Some(&i) = self.of_coord.get(&coord) {
            return i;
        }
        let i = self.free.pop().unwrap_or_else(|| {
            let i = self.next;
            self.next += 1;
            i
        });
        self.of_coord.insert(coord, i);
        i
    }

    /// Release `coord`'s slot back to the free pool. Returns the freed index.
    pub fn remove(&mut self, coord: SectionCoord) -> Option<u32> {
        let i = self.of_coord.remove(&coord)?;
        self.free.push(i);
        Some(i)
    }

    /// Number of live slots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.of_coord.len()
    }

    /// Whether no slots are live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.of_coord.is_empty()
    }

    /// The high-water mark: one past the largest index ever handed out. This is
    /// the length a per-section transform array must have to be indexable.
    #[must_use]
    pub fn capacity(&self) -> u32 {
        self.next
    }
}

/// Owns the mesh lifecycle for a live world: builds section meshes off-thread,
/// keeps them GPU-resident in a [`SectionArena`], and culls them through a
/// [`WorldScene`] each frame.
#[derive(Debug)]
pub struct WorldMesher {
    scene: WorldScene,
    arena: SectionArena,
    instances: InstanceTable,
    greedy: bool,
}

impl WorldMesher {
    /// Create a mesher with arenas sized for `vertex_capacity` / `index_capacity`
    /// bytes. `greedy` selects the merging mesher (the production choice) over the
    /// per-face reference mesher.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        vertex_capacity: u64,
        index_capacity: u64,
        greedy: bool,
    ) -> Self {
        Self {
            scene: WorldScene::new(),
            arena: SectionArena::new(device, vertex_capacity, index_capacity),
            instances: InstanceTable::new(),
            greedy,
        }
    }

    /// React to a chunk load at column `(cx, cz)` over vertical section range
    /// `section_ys`: gather the dirtied neighbourhood snapshots, mesh them (in
    /// parallel on native targets, serially on wasm), and upload the results.
    ///
    /// This is the whole load path in one call. It pulls snapshots through
    /// `source` (dropping any world lock immediately), meshes off the caller's
    /// thread, then uploads and registers each section for culling.
    pub fn load_column<S: SectionSource, C: BlockClassifier + Sync>(
        &mut self,
        source: &S,
        queue: &wgpu::Queue,
        classifier: &C,
        cx: i32,
        cz: i32,
        section_ys: core::ops::Range<i32>,
    ) {
        let jobs = dirty_jobs(source, cx, cz, section_ys);
        let built = build_batch(jobs, classifier, self.greedy);
        self.apply_built(queue, built);
    }

    /// Upload a batch of built sections and register them for culling.
    ///
    /// Separated from meshing so an application that meshes on its own schedule
    /// (e.g. a bounded per-frame upload budget to avoid hitching) can drive the
    /// GPU half directly. Re-uploading a resident coord replaces it in place.
    pub fn apply_built(&mut self, queue: &wgpu::Queue, built: Vec<BuiltSection>) {
        for b in built {
            let instance = self.instances.get_or_insert(b.coord);
            match self.arena.upload(queue, b.coord, &b.mesh, instance) {
                Ok(region) => self.scene.insert_section(b.coord, region, b.visibility),
                Err(_e) => {
                    // Arena exhausted: leave the section unregistered rather than
                    // drawing garbage. A later eviction frees space for a retry.
                    self.instances.remove(b.coord);
                }
            }
        }
    }

    /// Evict every loaded section in column `(cx, cz)` over `section_ys` on
    /// chunk unload: free its arena spans, drop it from the scene, and recycle
    /// its instance slot.
    pub fn unload_column(&mut self, cx: i32, cz: i32, section_ys: core::ops::Range<i32>) {
        for y in section_ys {
            let coord = (cx, y, cz);
            self.arena.evict(coord);
            self.scene.remove_section(coord);
            self.instances.remove(coord);
        }
    }

    /// Cull the resident world for `camera`.
    #[must_use]
    pub fn plan_frame(&self, camera: &Camera) -> FramePlan {
        self.scene.plan_frame(camera)
    }

    /// The underlying scene (for inspection/tests).
    #[must_use]
    pub fn scene(&self) -> &WorldScene {
        &self.scene
    }

    /// The underlying arena (to bind its buffers before drawing).
    #[must_use]
    pub fn arena(&self) -> &SectionArena {
        &self.arena
    }

    /// The instance table (for sizing the per-section transform array).
    #[must_use]
    pub fn instances(&self) -> &InstanceTable {
        &self.instances
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_table_hands_out_dense_indices() {
        let mut t = InstanceTable::new();
        assert_eq!(t.get_or_insert((0, 0, 0)), 0);
        assert_eq!(t.get_or_insert((1, 0, 0)), 1);
        assert_eq!(t.get_or_insert((2, 0, 0)), 2);
        // Re-request is stable.
        assert_eq!(t.get_or_insert((1, 0, 0)), 1);
        assert_eq!(t.len(), 3);
        assert_eq!(t.capacity(), 3);
    }

    #[test]
    fn instance_table_recycles_the_lowest_freed_slot() {
        let mut t = InstanceTable::new();
        t.get_or_insert((0, 0, 0)); // 0
        t.get_or_insert((1, 0, 0)); // 1
        t.get_or_insert((2, 0, 0)); // 2
        assert_eq!(t.remove((1, 0, 0)), Some(1));
        // The freed slot 1 is reused before growing.
        assert_eq!(t.get_or_insert((9, 9, 9)), 1);
        assert_eq!(t.capacity(), 3, "no growth while a slot was free");
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn instance_table_removing_absent_coord_is_none() {
        let mut t = InstanceTable::new();
        assert_eq!(t.remove((5, 5, 5)), None);
        assert!(t.is_empty());
    }
}
