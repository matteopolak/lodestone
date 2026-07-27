//! Off-main-thread section meshing over **copy-on-write snapshots**.
//!
//! The rule from the design plan is absolute: *the world is never locked while
//! meshing*. So the pipeline is split in two:
//!
//! 1. On the owning thread, [`snapshot_section`] clones the 3×3×3 = 27 sections
//!    around a target section into an owned, `Send` [`SectionSnapshot`]. The
//!    neighbourhood is 27, not 6, because ambient occlusion and smooth light
//!    read diagonal neighbours across section edges *and* corners.
//! 2. On worker threads, [`mesh_snapshot`] turns a snapshot into a
//!    [`lodestone_render::Mesh`] with no access to the live world at all.
//!
//! [`MeshScheduler`] is a tiny fixed worker pool wrapping that split.
//!
//! Meshing uses [`lodestone_render::mesh_simple`] (one quad per visible face)
//! rather than the greedy mesher: the shell's atlas packs many sprites into one
//! 2-D texture, and greedy-merged quads tile UVs past a single sprite's cell,
//! which would bleed neighbouring sprites. Per-face quads keep every tile
//! coordinate in `{0,1}`, mapping exactly onto each sprite rect. (A texture-array
//! atlas would let greedy back in — noted in the report.)

use std::sync::{
    Arc, Mutex,
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};

use lodestone_render::{ChunkSectionView, Mesh, SectionNeighborhood, UniformLight, mesh_simple};
use lodestone_world::{ChunkPos, ChunkSection, PaletteKind, World};

use crate::blocks::{DemoClassifier, id};

/// Identifies one 16³ section: its column plus the section index within that
/// column (`0` is the lowest section).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SectionKey {
    /// Column X (chunk coordinate).
    pub cx: i32,
    /// Column Z (chunk coordinate).
    pub cz: i32,
    /// Section index within the column.
    pub si: usize,
    /// Lowest world-y of the column (needed to place the section in world space).
    pub min_y: i32,
}

impl SectionKey {
    /// World-space origin (minimum corner) of this section.
    #[must_use]
    pub fn origin(&self) -> [i32; 3] {
        [self.cx * 16, self.min_y + self.si as i32 * 16, self.cz * 16]
    }
}

/// An owned, `Send` copy of the 27-section neighbourhood around one section.
///
/// Index `[dx+1][dy+1][dz+1]` for `dx,dy,dz ∈ {-1,0,1}`; the centre is `[1][1][1]`.
/// Missing neighbours (edge of world, above/below the column) are all-air
/// sections so the mesher still sees lit air there rather than an unlit void.
#[derive(Debug)]
pub struct SectionSnapshot {
    /// Which section this is.
    pub key: SectionKey,
    sections: Vec<ChunkSection>,
}

impl SectionSnapshot {
    fn at(&self, dx: i32, dy: i32, dz: i32) -> &ChunkSection {
        let i = ((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize;
        &self.sections[i]
    }
}

fn air_section() -> ChunkSection {
    ChunkSection::new(
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        id::AIR,
        0,
    )
}

/// Clone the 27-section neighbourhood around `key` out of the world, if the
/// centre section actually holds geometry. Returns `None` when the centre is
/// absent or entirely air (nothing to mesh).
#[must_use]
pub fn snapshot_section(world: &World, key: SectionKey) -> Option<SectionSnapshot> {
    let centre_col = world.get(ChunkPos {
        x: key.cx,
        z: key.cz,
    })?;
    // Skip empty centres so we don't schedule work that produces no geometry.
    let centre = centre_col.column.section(key.si)?;
    if is_all_air(centre) {
        return None;
    }

    let mut sections = Vec::with_capacity(27);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let col = world.get(ChunkPos {
                    x: key.cx + dx,
                    z: key.cz + dz,
                });
                let si = key.si as i32 + dy;
                // `World::get` now hands back an owned `Arc<LoadedChunk>`, so the
                // section clone must happen while that Arc is still alive inside
                // the closure — returning a `&ChunkSection` would dangle.
                let section = col.and_then(|c| {
                    if si < 0 {
                        None
                    } else {
                        c.column.section(si as usize).cloned()
                    }
                });
                sections.push(section.unwrap_or_else(air_section));
            }
        }
    }

    Some(SectionSnapshot { key, sections })
}

fn is_all_air(section: &ChunkSection) -> bool {
    // A cheap proxy: scan is unnecessary because ChunkSection tracks non-air.
    // We conservatively mesh any section that has at least one non-air block.
    for x in 0..16 {
        for y in 0..16 {
            for z in 0..16 {
                if section.get_block(x, y, z) != id::AIR {
                    return false;
                }
            }
        }
    }
    true
}

/// Mesh a snapshot into geometry. Pure and thread-safe: touches only the owned
/// snapshot and a stateless classifier.
#[must_use]
pub fn mesh_snapshot(snapshot: &SectionSnapshot, classifier: &DemoClassifier) -> Mesh {
    // Full sky light everywhere: the shell has no server light for its local
    // world, and air must carry light or every face renders black.
    let light = UniformLight::default();

    // Build a view per neighbour section, then assemble the neighbourhood.
    let mut views: Vec<ChunkSectionView<'_, DemoClassifier, UniformLight>> = Vec::with_capacity(27);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                views.push(ChunkSectionView::new(
                    snapshot.at(dx, dy, dz),
                    classifier,
                    &light,
                ));
            }
        }
    }
    let idx = |dx: i32, dy: i32, dz: i32| ((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize;

    let mut hood = SectionNeighborhood::centre_only(&views[idx(0, 0, 0)]);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                hood.set(dx, dy, dz, Some(&views[idx(dx, dy, dz)]));
            }
        }
    }

    mesh_simple(&hood)
}

/// A finished mesh with its key, handed back from a worker.
#[derive(Debug)]
pub struct Meshed {
    /// Which section this mesh is for.
    pub key: SectionKey,
    /// The geometry.
    pub mesh: Mesh,
}

enum Job {
    Mesh(SectionSnapshot),
    Stop,
}

/// A fixed pool of worker threads that mesh snapshots off the main thread.
#[derive(Debug)]
pub struct MeshScheduler {
    job_tx: Sender<Job>,
    result_rx: Receiver<Meshed>,
    workers: Vec<JoinHandle<()>>,
    pending: usize,
}

impl MeshScheduler {
    /// Spawn `worker_count` (min 1) meshing threads.
    #[must_use]
    pub fn new(worker_count: usize) -> Self {
        let worker_count = worker_count.max(1);
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (result_tx, result_rx) = mpsc::channel::<Meshed>();
        let job_rx = Arc::new(Mutex::new(job_rx));

        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let job_rx = Arc::clone(&job_rx);
            let result_tx = result_tx.clone();
            workers.push(thread::spawn(move || {
                let classifier = DemoClassifier;
                loop {
                    let job = {
                        let lock = job_rx.lock().expect("mesh job queue poisoned");
                        lock.recv()
                    };
                    match job {
                        Ok(Job::Mesh(snap)) => {
                            let mesh = mesh_snapshot(&snap, &classifier);
                            if result_tx
                                .send(Meshed {
                                    key: snap.key,
                                    mesh,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(Job::Stop) | Err(_) => break,
                    }
                }
            }));
        }

        Self {
            job_tx,
            result_rx,
            workers,
            pending: 0,
        }
    }

    /// Queue a snapshot for meshing.
    pub fn submit(&mut self, snapshot: SectionSnapshot) {
        self.pending += 1;
        // Send failure only happens if all workers died; drop the job then.
        if self.job_tx.send(Job::Mesh(snapshot)).is_err() {
            self.pending -= 1;
        }
    }

    /// Number of submitted jobs not yet drained.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending
    }

    /// Collect any finished meshes without blocking.
    pub fn drain(&mut self) -> Vec<Meshed> {
        let mut out = Vec::new();
        while let Ok(meshed) = self.result_rx.try_recv() {
            self.pending -= 1;
            out.push(meshed);
        }
        out
    }

    /// Block until at least `n` results are available (or all pending done),
    /// returning everything collected. Used by tests and headless runs.
    pub fn drain_blocking(&mut self, n: usize) -> Vec<Meshed> {
        let mut out = Vec::new();
        while out.len() < n && self.pending > 0 {
            match self.result_rx.recv() {
                Ok(meshed) => {
                    self.pending -= 1;
                    out.push(meshed);
                }
                Err(_) => break,
            }
        }
        out
    }
}

impl Drop for MeshScheduler {
    fn drop(&mut self) {
        for _ in &self.workers {
            let _ = self.job_tx.send(Job::Stop);
        }
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn snapshot_is_send() {
        // Compile-time proof that snapshots can cross to worker threads.
        assert_send::<SectionSnapshot>();
        assert_send::<Meshed>();
    }

    #[test]
    fn snapshot_and_mesh_a_ground_section() {
        let world = crate::worldgen::generate(0);
        // Section 2 straddles sea level / surface, so it has terrain.
        let key = SectionKey {
            cx: 0,
            cz: 0,
            si: 2,
            min_y: crate::worldgen::MIN_Y,
        };
        let snap = snapshot_section(&world, key).expect("centre section has geometry");
        let mesh = mesh_snapshot(&snap, &DemoClassifier);
        assert!(mesh.quad_count() > 0, "ground section should emit faces");
    }

    #[test]
    fn empty_sky_section_is_skipped() {
        let world = crate::worldgen::generate(0);
        // Pick the first section that starts strictly above the generated
        // surface at the origin, so it is guaranteed sky. Deriving the index
        // from `surface_height` keeps this honest as the terrain generator
        // changes underneath us (real vanilla terrain lifted the origin surface
        // to ~y71, which used to be hard-coded sky).
        let surface = crate::worldgen::surface_height(0, 0);
        let si = ((surface - crate::worldgen::MIN_Y) / 16 + 1) as usize;
        assert!(
            si < crate::worldgen::SECTION_COUNT,
            "surface {surface} leaves no sky section in the window"
        );
        let key = SectionKey {
            cx: 0,
            cz: 0,
            si,
            min_y: crate::worldgen::MIN_Y,
        };
        assert!(
            snapshot_section(&world, key).is_none(),
            "all-air section produces no snapshot"
        );
    }

    #[test]
    fn scheduler_meshes_many_sections() {
        let world = crate::worldgen::generate(1);
        let mut scheduler = MeshScheduler::new(3);
        let mut submitted = 0;
        for cz in -1..=1 {
            for cx in -1..=1 {
                for si in 0..crate::worldgen::SECTION_COUNT {
                    let key = SectionKey {
                        cx,
                        cz,
                        si,
                        min_y: crate::worldgen::MIN_Y,
                    };
                    if let Some(snap) = snapshot_section(&world, key) {
                        scheduler.submit(snap);
                        submitted += 1;
                    }
                }
            }
        }
        assert!(submitted > 0, "should have scheduled some sections");
        let results = scheduler.drain_blocking(submitted);
        assert_eq!(results.len(), submitted, "every job returns a mesh");
        assert!(
            results.iter().any(|m| m.mesh.quad_count() > 0),
            "at least one section has geometry"
        );
    }
}
