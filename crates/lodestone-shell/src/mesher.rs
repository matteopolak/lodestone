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

use lodestone_render::{
    BlockClassifier, BlockModels, ChunkSectionView, FluidCell, FluidKind, FluidMeshes,
    FluidSectionView, FluidSprites, Mesh, ModelMesh, ModelSectionView, SectionLight,
    SectionNeighborhood, SkyDefault, UniformLight, WorldSectionLight, mesh_fluids, mesh_models,
    mesh_simple,
};
use lodestone_assets::BakedQuad;
use lodestone_world::{
    ChunkPos, ChunkSection, PaletteKind, SectionLight as SectionLightData, World,
};

use crate::blocks::{ShellClassifier, id};
use crate::net::NetClient;

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
    /// Per-neighbour light, indexed identically to `sections`
    /// (`[dx+1][dy+1][dz+1]`). `None` where the neighbour column or light
    /// section is absent (edge of world / below the world). Those slots fall
    /// back to the full-bright bridge in [`mesh_snapshot`]; every present slot
    /// carries the world's real sky/block light.
    lights: Vec<Option<SectionLightData>>,
    /// How to resolve *absent* (`Missing`) sky light, chosen per dimension by the
    /// producer. The shell's local world is the overworld, so this is
    /// [`SkyDefault::Full`]; a nether/end driver would pass [`SkyDefault::None`]
    /// so absent sky stays `0` rather than defaulting up to daylight.
    sky_default: SkyDefault,
}

impl SectionSnapshot {
    fn at(&self, dx: i32, dy: i32, dz: i32) -> &ChunkSection {
        let i = ((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize;
        &self.sections[i]
    }

    fn light_at(&self, dx: i32, dy: i32, dz: i32) -> Option<&SectionLightData> {
        let i = ((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize;
        self.lights[i].as_ref()
    }

    /// The number of merged quads this snapshot would emit — a cheap coverage
    /// proxy for gates that need to prove a live neighbourhood produced
    /// non-trivial geometry (an empty world meshes to zero).
    #[must_use]
    pub fn quad_count<C: BlockClassifier>(&self, classifier: &C) -> usize {
        mesh_snapshot(self, classifier).quad_count()
    }

    /// A copy of this snapshot with **all light stripped** (every neighbour slot
    /// `None`), so [`mesh_snapshot`] falls back to the full-bright
    /// [`UniformLight::pre_light_bridge`] for the whole neighbourhood.
    ///
    /// This is the *control* for lighting gates: it reproduces exactly what the
    /// retired full-bright path rendered, letting a test prove that real light
    /// differs from it (the shadowed-darker-than-open-sky assertion the bridge
    /// cannot satisfy). It carries no meaning on the render path.
    #[must_use]
    pub fn full_bright_control(&self) -> SectionSnapshot {
        SectionSnapshot {
            key: self.key,
            sections: self.sections.clone(),
            lights: (0..self.sections.len()).map(|_| None).collect(),
            sky_default: self.sky_default,
        }
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
    let mut lights = Vec::with_capacity(27);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let pos = ChunkPos {
                    x: key.cx + dx,
                    z: key.cz + dz,
                };
                let col = world.get(pos);
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

                // Light is LIGHT-section indexed: block section `si` reads light
                // section `si + 1` (light section 0 is the boundary below the
                // world). This is an off-by-one *by design*, not a bug — do not
                // "correct" it. `section_light` returns `None` for an absent
                // column or an out-of-range light section; those slots keep the
                // bridge in `mesh_snapshot`.
                let light = if si + 1 < 0 {
                    None
                } else {
                    world.section_light(pos, (si + 1) as usize)
                };
                lights.push(light);
            }
        }
    }

    Some(SectionSnapshot {
        key,
        sections,
        lights,
        // The local world is the overworld: absent sky light is full daylight.
        sky_default: SkyDefault::Full,
    })
}

/// Build a [`SectionSnapshot`] for `key` from the **live client world**, reading
/// blocks and light for the whole 27-section neighbourhood under one lock via
/// [`NetClient::sections_and_light_at`]. Returns `None` when the centre section
/// holds no geometry (unloaded or all-air), exactly like [`snapshot_section`].
///
/// `section_count` is the column's block-section count from
/// [`NetClient::world_dimensions`]; `key.min_y` must be the dimension's `min_y`.
/// Light is **server-authoritative**: this never recomputes it (recomputing on
/// multiplayer would overwrite the server's seam-complete cross-chunk light with
/// a partial result — a divergence bug). Light-section indexing is the
/// off-by-one-by-design `(n, n + 1)` the handle documents.
///
/// Only vertically in-range neighbours (`0 <= si + dy < section_count`) are
/// requested; a neighbour above the top or below the bottom of the world is an
/// air section with the full-bright bridge for light (open sky above is bright
/// anyway, and below-world is rarely visible) — the same absent-neighbour policy
/// [`mesh_snapshot`] applies at horizontal world edges.
#[must_use]
pub fn snapshot_section_live(
    net: &NetClient,
    key: SectionKey,
    section_count: usize,
) -> Option<SectionSnapshot> {
    let idx = |dx: i32, dy: i32, dz: i32| ((dx + 1) * 9 + (dy + 1) * 3 + (dz + 1)) as usize;

    // Gather requests only for vertically in-range neighbours; remember which of
    // the 27 slots each result belongs to so the snapshot stays aligned. The
    // request key is the client's `ChunkPos` (the network world's id type), which
    // is distinct from `lodestone_world::ChunkPos` used for the local world.
    let mut reqs: Vec<(lodestone_client::ChunkPos, usize, usize)> = Vec::with_capacity(27);
    let mut slot_of_req: Vec<usize> = Vec::with_capacity(27);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let bsec = key.si as i32 + dy;
                if bsec >= 0 && (bsec as usize) < section_count {
                    let pos = lodestone_client::ChunkPos {
                        x: key.cx + dx,
                        z: key.cz + dz,
                    };
                    // Block section `bsec` reads light section `bsec + 1`
                    // (off-by-one BY DESIGN — do not "align" it).
                    reqs.push((pos, bsec as usize, (bsec + 1) as usize));
                    slot_of_req.push(idx(dx, dy, dz));
                }
            }
        }
    }

    let results = net.sections_and_light_at(&reqs);

    // Absent slots (out-of-range vertical neighbours) start as lit air + bridge.
    let mut sections: Vec<ChunkSection> = (0..27).map(|_| air_section()).collect();
    let mut lights: Vec<Option<SectionLightData>> = (0..27).map(|_| None).collect();
    for ((block, light), &slot) in results.into_iter().zip(slot_of_req.iter()) {
        if let Some(block) = block {
            // Clone the section out of the shared Arc so the snapshot is owned and
            // `Send`; the live world is never locked while meshing.
            sections[slot] = (*block).clone();
        }
        lights[slot] = light;
    }

    // Nothing to mesh if the centre is unloaded / all-air.
    if is_all_air(&sections[idx(0, 0, 0)]) {
        return None;
    }

    Some(SectionSnapshot {
        key,
        sections,
        lights,
        // The live path currently drives the overworld; a nether/end driver would
        // pass `SkyDefault::None` here (the too-bright-nether guard).
        sky_default: SkyDefault::Full,
    })
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

/// A section's light source for the mesh pass: either the world's real light
/// (via [`WorldSectionLight`]) or, for a genuinely-absent neighbour (edge of
/// world / below world), the full-bright bridge.
///
/// The bridge lives on **only** in the absent branch: a present section always
/// carries real light. Keeping the two in one enum lets every view share a
/// single concrete `SectionLight` type so the neighbourhood stays monomorphic
/// (no boxing) while still mixing real and fallback light per slot.
enum SnapLight<'a> {
    World(WorldSectionLight<'a>),
    /// Full-bright fallback for an absent neighbour section only.
    Bridge(UniformLight),
}

impl SectionLight for SnapLight<'_> {
    fn block_light(&self, x: usize, y: usize, z: usize) -> u8 {
        match self {
            SnapLight::World(w) => w.block_light(x, y, z),
            SnapLight::Bridge(b) => b.block_light(x, y, z),
        }
    }

    fn sky_light(&self, x: usize, y: usize, z: usize) -> u8 {
        match self {
            SnapLight::World(w) => w.sky_light(x, y, z),
            SnapLight::Bridge(b) => b.sky_light(x, y, z),
        }
    }
}

/// Mesh a snapshot into geometry. Pure and thread-safe: touches only the owned
/// snapshot and a stateless classifier. Generic over the [`BlockClassifier`] so
/// the same code meshes the demo world (via [`crate::blocks::DemoClassifier`])
/// and the live vanilla world (via a [`crate::blocks::ShellClassifier::Vanilla`]
/// atlas) without duplication.
#[must_use]
pub fn mesh_snapshot<C: BlockClassifier>(snapshot: &SectionSnapshot, classifier: &C) -> Mesh {
    // Real per-section light, replacing the retired full-bright bridge. Each
    // present neighbour forwards the world's resolved sky/block levels verbatim
    // (with the dimension's `SkyDefault` applied only to genuinely-absent sky);
    // an absent neighbour — and *only* an absent neighbour — keeps the bridge, so
    // air at the edge of the loaded world stays lit rather than rendering black.
    let mut srcs: Vec<SnapLight<'_>> = Vec::with_capacity(27);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let src = match snapshot.light_at(dx, dy, dz) {
                    Some(world_light) => {
                        SnapLight::World(WorldSectionLight::new(world_light, snapshot.sky_default))
                    }
                    None => SnapLight::Bridge(UniformLight::pre_light_bridge()),
                };
                srcs.push(src);
            }
        }
    }

    // Build a view per neighbour section, then assemble the neighbourhood.
    let mut views: Vec<ChunkSectionView<'_, C, SnapLight<'_>>> = Vec::with_capacity(27);
    let mut i = 0usize;
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                views.push(ChunkSectionView::new(
                    snapshot.at(dx, dy, dz),
                    classifier,
                    &srcs[i],
                ));
                i += 1;
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

/// A [`ModelSectionView`] over a [`SectionSnapshot`], driving the model mesh
/// path for the live vanilla world.
///
/// `quads_at`/`occludes_at` read vanilla block-state ids straight out of the
/// snapshot's paletted sections and look up baked geometry/occlusion in
/// [`BlockModels`]; `light_at` reads the centre section's real sky/block light.
/// This is the model-path counterpart to the packed [`ChunkSectionView`].
struct SnapshotModelView<'a> {
    snapshot: &'a SectionSnapshot,
    models: &'a BlockModels,
    light: SnapLight<'a>,
}

/// Split a signed section coordinate into a neighbour offset (`dx ∈ {-1,0,1}`)
/// and a section-local index (`0..16`). Used to resolve a `cullface` probe that
/// steps one block past a section edge into the adjacent snapshot section.
fn split16(v: i32) -> (i32, usize) {
    (v.div_euclid(16), v.rem_euclid(16) as usize)
}

impl ModelSectionView for SnapshotModelView<'_> {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        let id = self.snapshot.at(0, 0, 0).get_block(x, y, z);
        self.models.quads(id)
    }

    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        // Only the 3×3×3 neighbourhood is snapshotted; a probe further out (never
        // emitted by a single one-block cullface step) reads as non-occluding.
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return false;
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        self.models.occludes(id)
    }

    fn light_at(&self, x: usize, y: usize, z: usize) -> u8 {
        let sky = SectionLight::sky_light(&self.light, x, y, z);
        let block = SectionLight::block_light(&self.light, x, y, z);
        (sky << 4) | block
    }
}

/// Mesh a snapshot into wide baked-model geometry — the live vanilla path.
///
/// Every block (full cubes included) is emitted from its baked model quads,
/// face-culled against neighbours' [`BlockModels::occludes`]. This is what lets
/// cross-plants, slabs, stairs and translucent blocks render as their true
/// geometry instead of synthetic full cubes. Pure and thread-safe like
/// [`mesh_snapshot`].
#[must_use]
pub fn mesh_snapshot_models(snapshot: &SectionSnapshot, models: &BlockModels) -> ModelMesh {
    let light = match snapshot.light_at(0, 0, 0) {
        Some(world_light) => {
            SnapLight::World(WorldSectionLight::new(world_light, snapshot.sky_default))
        }
        None => SnapLight::Bridge(UniformLight::pre_light_bridge()),
    };
    let view = SnapshotModelView {
        snapshot,
        models,
        light,
    };
    mesh_models(&view)
}

/// The mesher's fluid view over a snapshot: resolves each cell's fluid (if any)
/// and occlusion out of the paletted sections, and reads the centre section's
/// light. Fluids need the same signed neighbourhood as the model path (one cell
/// past a section edge) to cull shared faces and slope corners.
struct SnapshotFluidView<'a> {
    snapshot: &'a SectionSnapshot,
    models: &'a BlockModels,
    light: SnapLight<'a>,
}

impl FluidSectionView for SnapshotFluidView<'_> {
    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return None;
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        self.models.fluid(id)
    }

    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        let (dx, lx) = split16(x);
        let (dy, ly) = split16(y);
        let (dz, lz) = split16(z);
        if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
            return false;
        }
        let id = self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz);
        self.models.occludes(id)
    }

    fn light_at(&self, x: usize, y: usize, z: usize) -> u8 {
        let sky = SectionLight::sky_light(&self.light, x, y, z);
        let block = SectionLight::block_light(&self.light, x, y, z);
        (sky << 4) | block
    }

    fn fluid_sprites(&self, kind: FluidKind) -> FluidSprites {
        self.models.fluid_sprites(kind)
    }
}

/// Mesh a snapshot's fluid cells into water (translucent) and lava (opaque,
/// full-bright) geometry. Runs alongside [`mesh_snapshot_models`]; the block path
/// emits no quads for fluid cells, so the two never double-render.
#[must_use]
fn mesh_snapshot_fluids(snapshot: &SectionSnapshot, models: &BlockModels) -> FluidMeshes {
    let light = match snapshot.light_at(0, 0, 0) {
        Some(world_light) => {
            SnapLight::World(WorldSectionLight::new(world_light, snapshot.sky_default))
        }
        None => SnapLight::Bridge(UniformLight::pre_light_bridge()),
    };
    let view = SnapshotFluidView {
        snapshot,
        models,
        light,
    };
    mesh_fluids(&view)
}

/// The geometry a worker produced for one section.
///
/// The demo world meshes to a packed full-cube [`Mesh`]; the live vanilla world
/// meshes to wide baked-model [`ModelMesh`] geometry. The two never mix within a
/// session (the classifier picks one id space), but both flow through the same
/// [`Meshed`]/upload seam so the GPU side can dispatch on the variant.
#[derive(Debug)]
pub enum SectionGeometry {
    /// Packed full-cube geometry (demo world).
    Packed(Mesh),
    /// Wide baked-model geometry (live vanilla world): opaque terrain (blocks +
    /// lava, drawn on the opaque model pass) plus translucent water (drawn on the
    /// fluid pass after opaque geometry, so the sea floor shows through).
    Model {
        /// Opaque block geometry, with lava merged in.
        opaque: ModelMesh,
        /// Translucent water surface geometry.
        water: ModelMesh,
    },
}

impl SectionGeometry {
    /// The merged quad count, for stats/overlay parity across both paths.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        match self {
            SectionGeometry::Packed(m) => m.quad_count(),
            SectionGeometry::Model { opaque, water } => opaque.quad_count() + water.quad_count(),
        }
    }
}

/// A finished mesh with its key, handed back from a worker.
#[derive(Debug)]
pub struct Meshed {
    /// Which section this mesh is for.
    pub key: SectionKey,
    /// The geometry (packed demo cubes or vanilla baked models).
    pub mesh: SectionGeometry,
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
    /// Spawn `worker_count` (min 1) meshing threads, each meshing with a clone of
    /// `classifier`. The classifier picks the id space: a
    /// [`ShellClassifier::Demo`] pool meshes the offline demo world, a
    /// [`ShellClassifier::Vanilla`] pool meshes the live server's vanilla world.
    /// The atlas behind the vanilla variant is `Arc`-shared, so a per-worker
    /// clone is a refcount bump.
    #[must_use]
    pub fn new(worker_count: usize, classifier: ShellClassifier) -> Self {
        let worker_count = worker_count.max(1);
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (result_tx, result_rx) = mpsc::channel::<Meshed>();
        let job_rx = Arc::new(Mutex::new(job_rx));

        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let job_rx = Arc::clone(&job_rx);
            let result_tx = result_tx.clone();
            let classifier = classifier.clone();
            workers.push(thread::spawn(move || {
                loop {
                    let job = {
                        let lock = job_rx.lock().expect("mesh job queue poisoned");
                        lock.recv()
                    };
                    match job {
                        Ok(Job::Mesh(snap)) => {
                            // The vanilla classifier carries baked models → mesh
                            // through the model path; the demo classifier has none
                            // → mesh through the packed full-cube path.
                            let mesh = match classifier.models() {
                                Some(models) => {
                                    let mut opaque = mesh_snapshot_models(&snap, models);
                                    let fluids = mesh_snapshot_fluids(&snap, models);
                                    // Lava is opaque and full-bright: fold it into
                                    // the opaque pass. Water is translucent and
                                    // drawn separately.
                                    opaque.merge(&fluids.lava);
                                    SectionGeometry::Model {
                                        opaque,
                                        water: fluids.water,
                                    }
                                }
                                None => SectionGeometry::Packed(mesh_snapshot(&snap, &classifier)),
                            };
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
    use crate::blocks::DemoClassifier;

    fn assert_send<T: Send>() {}

    #[test]
    fn snapshot_is_send() {
        // Compile-time proof that snapshots can cross to worker threads.
        assert_send::<SectionSnapshot>();
        assert_send::<Meshed>();
    }

    #[test]
    fn split16_maps_signed_probes_to_neighbour_and_local() {
        // In-section coordinates stay in the centre neighbour (offset 0).
        assert_eq!(split16(0), (0, 0));
        assert_eq!(split16(15), (0, 15));
        // One block below/west of the section wraps into the -1 neighbour at
        // local index 15 — exactly the cullface probe across a section edge.
        assert_eq!(split16(-1), (-1, 15));
        // One block past the far edge wraps into the +1 neighbour at local 0.
        assert_eq!(split16(16), (1, 0));
        assert_eq!(split16(17), (1, 1));
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
        let mut scheduler = MeshScheduler::new(3, ShellClassifier::Demo(DemoClassifier));
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

    /// A hand-built snapshot: a full stone floor in the centre section with air
    /// above and around it, so every exposed face samples a neighbouring air
    /// cell for its light. `sky` is the uniform sky level fed to the whole
    /// neighbourhood; `lights_present` toggles between the real light field and
    /// the absent-neighbour bridge (all `None`).
    fn floor_snapshot(sky: u8, lights_present: bool) -> SectionSnapshot {
        use lodestone_world::LightData;

        let mut sections = Vec::with_capacity(27);
        let mut lights = Vec::with_capacity(27);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let mut sec = air_section();
                    if dx == 0 && dy == 0 && dz == 0 {
                        for x in 0..16 {
                            for z in 0..16 {
                                sec.set_block(x, 0, z, id::STONE);
                            }
                        }
                    }
                    sections.push(sec);
                    lights.push(if lights_present {
                        Some(SectionLightData {
                            sky: LightData::Uniform(sky),
                            block: LightData::Uniform(0),
                        })
                    } else {
                        None
                    });
                }
            }
        }
        SectionSnapshot {
            key: SectionKey {
                cx: 0,
                cz: 0,
                si: 1,
                min_y: 0,
            },
            sections,
            lights,
            sky_default: SkyDefault::Full,
        }
    }

    fn max_vertex_sky(mesh: &Mesh) -> u8 {
        mesh.vertices
            .iter()
            .map(|v| v.unpack().sky_light)
            .max()
            .unwrap_or(0)
    }

    /// The load-bearing lighting proof: a shadowed neighbourhood (stored sky
    /// `0`) must mesh **measurably darker** than an open-sky one (sky `15`), and
    /// the retired full-bright bridge must be **unable to tell them apart** — the
    /// exact assertion the old `UniformLight::default()` path fails.
    ///
    /// "It still draws" proves nothing here: full-bright and correct lighting
    /// both emit the same geometry. This asserts on the *vertex light bytes*, so
    /// it fails if the mesher ever silently reverts to a constant light field.
    #[test]
    fn shadowed_meshes_darker_than_open_sky_and_the_bridge_cannot_tell() {
        let open = mesh_snapshot(&floor_snapshot(15, true), &DemoClassifier);
        let shadow = mesh_snapshot(&floor_snapshot(0, true), &DemoClassifier);

        let open_sky = max_vertex_sky(&open);
        let shadow_sky = max_vertex_sky(&shadow);

        assert!(open.quad_count() > 0 && shadow.quad_count() > 0, "geometry");
        assert!(
            shadow_sky < open_sky,
            "shadowed sky light ({shadow_sky}) must be darker than open sky ({open_sky}); \
             a constant/full-bright light field would make these equal"
        );
        assert_eq!(open_sky, 255, "open sky should reach full brightness");
        assert_eq!(shadow_sky, 0, "stored sky 0 must stay dark, not default up");

        // Control: with the absent-neighbour bridge (lights all `None`) the mesher
        // falls back to full-bright, so the SAME two inputs become
        // indistinguishable — this is precisely the assertion the pre-light-bridge
        // path fails, demonstrating the swap is what put real light on screen.
        let bridge_open = mesh_snapshot(&floor_snapshot(15, false), &DemoClassifier);
        let bridge_shadow = mesh_snapshot(&floor_snapshot(0, false), &DemoClassifier);
        assert_eq!(
            max_vertex_sky(&bridge_open),
            max_vertex_sky(&bridge_shadow),
            "the full-bright bridge cannot distinguish shadow from open sky"
        );
        assert_eq!(
            max_vertex_sky(&bridge_shadow),
            255,
            "the bridge renders everything full-bright"
        );
    }

    /// End-to-end producer check: worldgen now computes real column light, so a
    /// snapshot pulled from the generated world carries a **non-uniform** sky
    /// field — a cave/underground cell is darker than an exposed one. Guards
    /// against a regression where generation reverts to all-`Missing` light,
    /// which would render the whole world flat full-bright again.
    #[test]
    fn generated_world_snapshot_has_real_light_gradient() {
        let world = crate::worldgen::generate(1);
        // Walk sections at the origin column from the bottom up; the first that
        // meshes holds terrain with sky above and rock below — a genuine
        // gradient across its faces.
        let mut saw_dark = false;
        let mut saw_bright = false;
        for si in 0..crate::worldgen::SECTION_COUNT {
            let key = SectionKey {
                cx: 0,
                cz: 0,
                si,
                min_y: crate::worldgen::MIN_Y,
            };
            if let Some(snap) = snapshot_section(&world, key) {
                let mesh = mesh_snapshot(&snap, &DemoClassifier);
                for v in &mesh.vertices {
                    let s = v.unpack().sky_light;
                    if s == 0 {
                        saw_dark = true;
                    }
                    if s > 200 {
                        saw_bright = true;
                    }
                }
            }
        }
        assert!(
            saw_dark && saw_bright,
            "generated terrain should mesh both fully-shadowed (0) and sky-lit \
             (>200) faces; saw_dark={saw_dark} saw_bright={saw_bright} — an \
             all-Missing (flat full-bright) world would have no dark faces"
        );
    }
}
