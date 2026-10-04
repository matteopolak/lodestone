//! Section meshing over **copy-on-write snapshots**.
//!
//! The rule from the design plan is absolute: *the world is never locked while
//! meshing*. So the pipeline is split in two:
//!
//! 1. On the owning thread, [`snapshot_section`] clones the 3×3×3 = 27 sections
//!    around a target section into an owned, `Send` [`SectionSnapshot`]. The
//!    neighbourhood is 27, not 6, because ambient occlusion and smooth light
//!    read diagonal neighbours across section edges *and* corners.
//!
//!    Because it is 27 and not 6, **a snapshot taken before its neighbours
//!    arrived is wrong in more ways than a missing face** — see [`Neighbour`] and
//!    [`SnapshotOutcome`] for the typed distinction between "air, and air is the
//!    truth" and "air, and air is a guess", and why the second one defers the
//!    build instead of baking it.
//! 2. [`mesh_snapshot`] turns a snapshot into a [`lodestone_render::Mesh`]
//!    with no access to the live world at all.
//!
//! [`MeshScheduler`] uses native workers or a browser frame budget for that split.
//!
//! Meshing uses [`lodestone_render::mesh_simple`] (one quad per visible face)
//! rather than the greedy mesher: the shell's atlas packs many sprites into one
//! 2-D texture, and greedy-merged quads tile UVs past a single sprite's cell,
//! which would bleed neighbouring sprites. Per-face quads keep every tile
//! coordinate in `{0,1}`, mapping exactly onto each sprite rect. (A texture-array
//! atlas would allow greedy meshing again.)

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::AtomicU64;
use std::time::Duration;
#[cfg(all(test, not(target_arch = "wasm32")))]
use std::sync::Mutex;
// The worker pool's plumbing. Native-only: `MeshScheduler`'s browser arm has no
// threads and no channels — it meshes in-frame under a time budget. See that type.
#[cfg(not(target_arch = "wasm32"))]
use std::thread::{self, JoinHandle};

use bevy_ecs::query::With;
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_ecs::system::{Query, Res, ResMut};
use lodestone_data::block_states::StateId;
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::{ChunkWorld, ChunkWorldWrite, FrameSet, LocalPlayer, PhysicsState, Update};
use lodestone_render::{
    BlockClassifier, BlockModels, ChunkSectionView, Face, FluidCell, FluidKind, FluidMeshes,
    FluidNeighborCell, FluidSectionView, FluidSprites, Mesh, ModelMesh, ModelSectionView,
    SectionLight,
    SectionNeighborhood, SkyDefault, UniformLight, WorldSectionLight, biome_tint_kind_for_slot,
    face_of_direction, mesh_fluids, mesh_models, mesh_simple,
};
use lodestone_render::biome_tint::{
    BLEND_RADIUS, BlendedTintCursor, NamedBiomeTint, rgb_to_bytes,
};
use lodestone_assets::{BakedQuad, Direction};
use lodestone_model::BlockPos;
use lodestone_world::{
    ChunkColumn, ChunkPos, ChunkSection, PaletteKind, SectionLight as SectionLightData, World,
};

#[cfg(not(target_arch = "wasm32"))]
/// Approximate upload-time budget; the observed cost is updated after each redraw.
const MESH_HANDOFF_TIME_BUDGET_NS: u64 = 2_000_000;
#[cfg(not(target_arch = "wasm32"))]
/// The handoff cannot exceed one frame's snapshot admission even when uploads are cheap.
const MESH_HANDOFF_MAX_COUNT: usize = MESH_SNAPSHOT_SECTION_BUDGET;
#[cfg(not(target_arch = "wasm32"))]
/// Approximate maximum geometry payload handed to the renderer in one frame.
const MESH_HANDOFF_BYTE_BUDGET: usize = 16 * 1024 * 1024;
#[cfg(not(target_arch = "wasm32"))]
const MESH_HANDOFF_INITIAL_NS_PER_RESULT: u64 = 50_000;
#[cfg(not(target_arch = "wasm32"))]
static MESH_UPLOAD_NS_PER_RESULT: AtomicU64 =
    AtomicU64::new(MESH_HANDOFF_INITIAL_NS_PER_RESULT);

#[cfg(not(target_arch = "wasm32"))]
fn mesh_handoff_count_budget(ns_per_result: u64) -> usize {
    (MESH_HANDOFF_TIME_BUDGET_NS / ns_per_result.max(1))
        .clamp(1, MESH_HANDOFF_MAX_COUNT as u64) as usize
}

#[cfg(not(target_arch = "wasm32"))]
pub fn record_native_mesh_upload_cost(elapsed: Duration, result_count: usize) {
    if result_count == 0 {
        return;
    }
    let observed = (elapsed.as_nanos() / result_count as u128)
        .clamp(1, u64::MAX as u128) as u64;
    let _ = MESH_UPLOAD_NS_PER_RESULT.try_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
        Some(((old as u128 * 3 + observed as u128) / 4) as u64)
    });
}

use crate::blocks::{ShellClassifier, id};
use crate::net::NetClient;
use crate::platform::Instant;

mod face;
mod fluid;
mod model;
mod measurement;
#[cfg(not(target_arch = "wasm32"))]
mod native_timing;
mod arrival_measurement;
mod light_reads;
mod priority;
mod readiness;
mod snapshot;
#[cfg(any(target_arch = "wasm32", test))]
mod browser_queue;

#[cfg(target_arch = "wasm32")]
use browser_queue::{BrowserMeshBacklog, CaptureSource, SectionIntent};

use readiness::ColumnSectionSet;
#[cfg(not(target_arch = "wasm32"))]
use priority::FairMeshOrder;
use priority::MeshPriority;
#[cfg(not(target_arch = "wasm32"))]
use native_timing::{NativeMeshDisposition, NativeMeshResultTiming, NativeMeshTiming};
#[cfg(not(target_arch = "wasm32"))]
pub use native_timing::NativeMeshTimingSnapshot;

pub use face::mesh_snapshot;
pub use fluid::{mesh_snapshot_fluids, mesh_snapshot_fluids_at, snapshot_visibility};
pub use model::{mesh_snapshot_models, mesh_snapshot_models_at, mesh_snapshot_models_layers};
pub use model::TintProbe;
pub use measurement::{
    MeshCauseMeasurement, MeshHandoffOutcome, MeshMeasurementSnapshot,
    MeshPhaseMeasurement, MeshRequestCause,
};
pub use snapshot::{
    ColumnSource, Neighbour, SectionKey, SectionSnapshot, SnapshotOutcome,
    snapshot_section, snapshot_section_in, snapshot_section_live, sky_default_for_dimension,
};
pub(crate) use snapshot::{
    ColumnBlockSummary, SnapLight, SnapshotLight, should_report_empty_column,
};
pub(crate) use model::{biome_name_at, split16, take_tint_probe};
#[cfg(test)]
use snapshot::air_section;
#[cfg(test)]
use model::ao_occludes_raw_state;

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
        /// Translucent **block** geometry — stained glass, ice, the nether
        /// portal swirl and anything else `BlockModels::layer` classifies as
        /// [`RenderLayer::Translucent`](lodestone_render::RenderLayer::Translucent)
        /// from real per-texel alpha. Kept separate from `water` (a different
        /// pipeline: `FLUID_WGSL` tints untinted quads with the water colour
        /// and carries no palette bind group, wrong for a palette-tinted
        /// translucent block) and drawn through its own
        /// `ModelPipeline::for_layer(.., RenderLayer::Translucent)` pass — see
        /// `gpu/frame.rs`.
        translucent_blocks: ModelMesh,
        /// This section's face connectivity for the occlusion graph (U3), from
        /// [`snapshot_visibility`].
        ///
        /// It rides on the *geometry* rather than on [`Meshed`] deliberately, and
        /// that is load-bearing rather than tidy: `RenderState::upload_section`
        /// takes `&SectionGeometry`, so putting it here reaches the graph with
        /// **no** change to the three `upload_section` call sites in
        /// `app/{redraw,runners,lifecycle}.rs`. It also means a section whose
        /// geometry is *empty* still carries its connectivity — which is the
        /// whole point for a fully-enclosed underground section, the very
        /// sections that block the walk and make the underground free.
        visibility: lodestone_render::SectionVisibility,
    },
}

impl SectionGeometry {
    pub(crate) fn fingerprint(&self) -> u128 {
        use xxhash_rust::xxh3::Xxh3;

        fn model(hash: &mut Xxh3, mesh: &ModelMesh) {
            hash.update(&(mesh.vertices.len() as u64).to_le_bytes());
            hash.update(bytemuck::cast_slice(&mesh.vertices));
            hash.update(&(mesh.indices.len() as u64).to_le_bytes());
            hash.update(bytemuck::cast_slice(&mesh.indices));
        }

        let mut hash = Xxh3::new();
        match self {
            Self::Packed(mesh) => {
                hash.update(&[0]);
                hash.update(&(mesh.vertices.len() as u64).to_le_bytes());
                hash.update(bytemuck::cast_slice(&mesh.vertices));
                hash.update(&(mesh.indices.len() as u64).to_le_bytes());
                hash.update(bytemuck::cast_slice(&mesh.indices));
            }
            Self::Model {
                opaque,
                water,
                translucent_blocks,
                visibility,
            } => {
                hash.update(&[1]);
                model(&mut hash, opaque);
                model(&mut hash, water);
                model(&mut hash, translucent_blocks);
                let faces = [Face::NegX, Face::PosX, Face::NegY, Face::PosY, Face::NegZ, Face::PosZ];
                let mut connectivity = [0u8; 36];
                for (a_index, a) in faces.into_iter().enumerate() {
                    for (b_index, b) in faces.into_iter().enumerate() {
                        connectivity[a_index * 6 + b_index] = u8::from(visibility.connects(a, b));
                    }
                }
                hash.update(&connectivity);
            }
        }
        hash.digest128()
    }

    /// The merged quad count, for stats/overlay parity across both paths.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        match self {
            SectionGeometry::Packed(m) => m.quad_count(),
            SectionGeometry::Model {
                opaque,
                water,
                translucent_blocks,
                ..
            } => opaque.quad_count() + water.quad_count() + translucent_blocks.quad_count(),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn upload_bytes(&self) -> usize {
        fn bytes(vertices: usize, indices: usize, vertex_size: usize) -> usize {
            vertices
                .saturating_mul(vertex_size)
                .saturating_add(indices.saturating_mul(std::mem::size_of::<u32>()))
        }

        match self {
            SectionGeometry::Packed(mesh) => bytes(
                mesh.vertices.len(),
                mesh.indices.len(),
                std::mem::size_of::<lodestone_render::PackedVertex>(),
            ),
            SectionGeometry::Model {
                opaque,
                water,
                translucent_blocks,
                ..
            } => [opaque, water, translucent_blocks]
                .into_iter()
                .map(|mesh| {
                    bytes(
                        mesh.vertices.len(),
                        mesh.indices.len(),
                        std::mem::size_of::<lodestone_render::ModelVertex>(),
                    )
                })
                .fold(0usize, usize::saturating_add),
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
    pub(crate) fingerprint: u128,
    measurement_cause: Option<MeshRequestCause>,
    light_revision: Option<u64>,
    light_inputs: Option<light_reads::LightInputs>,
    #[cfg(not(target_arch = "wasm32"))]
    native_timing: Option<(MeshPriority, NativeMeshResultTiming)>,
}

impl Meshed {
    fn new(key: SectionKey, mesh: SectionGeometry) -> Self {
        let fingerprint = mesh.fingerprint();
        Self {
            key, mesh, fingerprint, measurement_cause: None,
            light_revision: None, light_inputs: None,
            #[cfg(not(target_arch = "wasm32"))]
            native_timing: None,
        }
    }

    pub(crate) fn upload_timing_started(&self) -> Option<Instant> {
        #[cfg(not(target_arch = "wasm32"))]
        { self.native_timing.map(|_| Instant::now()) }
        #[cfg(target_arch = "wasm32")]
        { None }
    }
}

#[cfg(not(target_arch = "wasm32"))]
enum Job {
    /// Immutable inputs, submission generation, and its cancellation token.
    Mesh(SectionSnapshot, bool, i32, u64, Arc<AtomicBool>, Option<Instant>),
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct NativeGeneration {
    number: u64,
    cancelled: Arc<AtomicBool>,
    /// Survives replacement until the current result is handed off.
    pending_priority: Option<MeshPriority>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeGeneration {
    #[cfg(test)]
    fn new(number: u64) -> Self {
        Self::with_priority(number, MeshPriority::Background)
    }

    fn with_priority(number: u64, priority: MeshPriority) -> Self {
        Self {
            number,
            cancelled: Arc::new(AtomicBool::new(false)),
            pending_priority: Some(priority),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn receive_mesh_lane<T>(
    receivers: &[crossbeam_channel::Receiver<T>; 2],
    preferred: MeshPriority,
) -> Option<(MeshPriority, T)> {
    let other = match preferred {
        MeshPriority::Background => MeshPriority::Edit,
        MeshPriority::Edit => MeshPriority::Background,
    };
    let first = &receivers[preferred.index()];
    let second = &receivers[other.index()];
    if let Ok(value) = first.try_recv() {
        return Some((preferred, value));
    }
    if let Ok(value) = second.try_recv() {
        return Some((other, value));
    }
    crossbeam_channel::select_biased! {
        recv(first) -> value => value.ok().map(|value| (preferred, value)),
        recv(second) -> value => value.ok().map(|value| (other, value)),
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
enum NativeMeshCompletion {
    Built(Meshed, u64),
    Skipped(Option<NativeMeshResultTiming>),
}

/// Native scheduler lifetime work; skipped jobs never enter geometry computation.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NativeMeshWorkCounters {
    pub submitted: u64,
    pub started: u64,
    pub skipped_before_mesh: u64,
    pub stale_results_discarded: u64,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
struct NativeWorkerCounters {
    started: AtomicU64,
    skipped_before_mesh: AtomicU64,
    #[cfg(test)]
    started_keys: Mutex<Vec<SectionKey>>,
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[derive(Clone)]
struct NativeWorkerGate {
    entered: crossbeam_channel::Sender<()>,
    release: crossbeam_channel::Receiver<()>,
    ignore_cancellation: bool,
}

/// Shared meshing implementation for native workers and browser frame drains.
fn mesh_one(
    snap: SectionSnapshot,
    classifier: &ShellClassifier,
    cutout_leaves: bool,
    blend_radius: i32,
) -> Meshed {
    mesh_one_measured(snap, classifier, cutout_leaves, blend_radius, false).0
}

fn mesh_one_measured(
    snap: SectionSnapshot,
    classifier: &ShellClassifier,
    cutout_leaves: bool,
    blend_radius: i32,
    measure: bool,
) -> (Meshed, MeshCauseMeasurement) {
    let _span = tracing::trace_span!(
        "mesh_section",
        cx = snap.key.cx, cz = snap.key.cz, si = snap.key.si,
    ).entered();
    let biome_names_len = snap.biome_names.len();
    let mut passes = MeshCauseMeasurement::default();
    let retain_inputs = snap.light_revision.is_some();
    let light_reads = ((measure || retain_inputs) && classifier.models().is_some())
        .then(|| light_reads::LightReadProbe::with_measurement(measure));
    let _ = take_tint_probe();
    let mesh = match classifier.models() {
        Some(models) => {
            let (light, (mut opaque, translucent_blocks)) = measurement::phase(
                measure, &mut passes.models,
                || {
                    let light = SnapshotLight::new(&snap).with_read_probe(light_reads.as_ref());
                    let meshes = model::mesh_snapshot_models_layers_with_light(
                        &snap, models, cutout_leaves, blend_radius, &light,
                    );
                    (light, meshes)
                },
            );
            // Lava is opaque and full-bright: fold it into the opaque pass. Water
            // and translucent blocks (glass, ice, the nether portal swirl) are
            // translucent and drawn separately, each through its own pipeline.
            let fluids = measurement::phase(measure, &mut passes.fluids, || {
                let fluids = fluid::mesh_snapshot_fluids_with_light(&snap, models, blend_radius, &light);
                opaque.merge(&fluids.lava);
                fluids
            });
            SectionGeometry::Model {
                opaque,
                water: fluids.water,
                translucent_blocks,
                visibility: measurement::phase(
                    measure, &mut passes.visibility, || snapshot_visibility(&snap, models),
                ),
            }
        }
        None => SectionGeometry::Packed(measurement::phase(
            measure, &mut passes.packed, || mesh_snapshot(&snap, classifier),
        )),
    };
    if measure && let Some(probe) = light_reads.as_ref() {
        passes.light_reads.record(probe.summary());
    }
    report_tint_probe(
        snap.key,
        biome_names_len,
        mesh.quad_count(),
        matches!(mesh, SectionGeometry::Packed(_)),
        take_tint_probe(),
    );
    let mut meshed = measurement::phase(
        measure, &mut passes.fingerprint, || Meshed::new(snap.key, mesh),
    );
    meshed.light_revision = snap.light_revision;
    if retain_inputs && let Some(probe) = light_reads {
        let light = SnapshotLight::new(&snap);
        meshed.light_inputs = measurement::phase(measure, &mut passes.light_inputs, || {
            probe.finish(|[x, y, z]| {
                let (sky, block) = light.levels_at(x, y, z);
                sky << 4 | block
            })
        });
    }
    (meshed, passes)
}

/// Report one section's [`TintProbe`], so a tint that resolved to *nothing* is
/// distinguishable from a block that simply has no tint.
///
/// `names` is how many biome names the snapshot could see — `0` means
/// `biome_name_at` fell back to `FALLBACK_BIOME_NAMES` rather than the
/// server's own `registry_data` order, which is a *different* answer, not a
/// missing one.
///
/// Two sinks on purpose. `tracing` is the shipped one, and it is a `warn!`
/// only for the bucket that is a silent downgrade (a blended kind whose
/// colormaps were absent); everything else is `debug!`. The stderr line is for
/// a harness that installs no subscriber at all — the screenshot capture is
/// one — and stays off unless `LODESTONE_TINT_PROBE` is set, because this
/// fires once per meshed section and a render-distance-8 join meshes thousands.
fn report_tint_probe(
    key: SectionKey,
    names: usize,
    quads: usize,
    packed: bool,
    probe: TintProbe,
) {
    let skipped = probe.no_colormaps + probe.unresolved;
    if skipped > 0 {
        if !BIOME_TINT_SKIP_WARNING.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                target: "mesh",
                cx = key.cx,
                cz = key.cz,
                si = key.si,
                no_colormaps = probe.no_colormaps,
                unresolved = probe.unresolved,
                "biome tint skipped for a blended quad: it keeps the frame-shared \
                 palette's plains default instead of this position's own biome \
                 colour. Logged once per process; set LODESTONE_TINT_PROBE for \
                 per-section counts"
            );
        }
    }
    tracing::trace!(
        target: "mesh",
        cx = key.cx,
        cz = key.cz,
        si = key.si,
        names,
        quads,
        resolved = probe.resolved,
        unresolved = probe.unresolved,
        untinted = probe.untinted,
        "section meshed"
    );
    static STDERR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *STDERR.get_or_init(|| std::env::var_os("LODESTONE_TINT_PROBE").is_some()) {
        eprintln!(
            "tint-probe cx={} cz={} si={} path={} names={names} quads={quads} \
offered={} resolved={} unresolved={} no_colormaps={} not_blended={} untinted={}",
            key.cx,
            key.cz,
            key.si,
            if packed { "packed" } else { "model" },
            probe.quads,
            probe.resolved,
            probe.unresolved,
            probe.no_colormaps,
            probe.not_blended,
            probe.untinted,
        );
    }
}

/// Whether the once-per-process warning for a skipped blended-kind quad has
/// fired. Skips come from absent colormaps or a blend that returned nothing.
static BIOME_TINT_SKIP_WARNING: AtomicBool = AtomicBool::new(false);

/// Worker threads mesh immutable snapshots without locking the live world.
/// Each worker and the result handoff independently prefer edits, serving
/// available background work after at most four consecutive edit selections.
/// Frame drains never block; [`Self::drain_blocking`] is for headless work.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Resource)]
pub struct MeshScheduler {
    job_tx: [crossbeam_channel::Sender<Job>; 2],
    /// Built results and cancellation acknowledgements, each settling one job.
    result_rx: [crossbeam_channel::Receiver<NativeMeshCompletion>; 2],
    /// Completed results held until a frame has capacity to upload them.
    ready: [std::collections::VecDeque<(Meshed, u64)>; 2],
    handoff_order: FairMeshOrder,
    workers: Vec<JoinHandle<()>>,
    pending: usize,
    submitted: u64,
    worker_counters: Arc<NativeWorkerCounters>,
    stale_results_discarded: u64,
    native_timing: NativeMeshTiming,
    column_source: ColumnSource,
    spatial_light_air: Option<u32>,
    /// The live `options.cutoutLeaves` value, stamped onto each [`Job::Mesh`]
    /// at [`Self::submit`] time (a plain field, not shared state: `submit`
    /// already needs `&mut self`, and a worker thread never reads this field
    /// at all — only the value its own job carried).
    cutout_leaves: bool,
    /// The live `options.biomeBlendRadius` value, stamped onto each
    /// [`Job::Mesh`] beside [`Self::cutout_leaves`] and for the same reason.
    blend_radius: i32,
    /// Latest submission per section, owned by the scheduling thread.
    /// Its token prevents superseded queued work from starting. Drain-time
    /// generation checks also reject work superseded after computation starts.
    latest_generation: HashMap<SectionKey, NativeGeneration>,
    /// Monotonic counter [`Self::submit`] draws from to stamp each job.
    next_generation: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl MeshScheduler {
    /// Spawn `worker_count` (min 1) meshing threads, each meshing with a clone of
    /// `classifier`. The classifier picks the id space: a
    /// [`ShellClassifier::Demo`] pool meshes the offline demo world, a
    /// [`ShellClassifier::Vanilla`] pool meshes the live server's vanilla world.
    /// The atlas behind the vanilla variant is `Arc`-shared, so a per-worker
    /// clone is a refcount bump.
    ///
    /// # The classifier also fixes the [`ColumnSource`]
    ///
    /// The id space and the *provenance* of the columns are the same choice made
    /// twice: `ShellClassifier::is_vanilla`'s own docs state the invariant — "the
    /// session meshes the live world only under this variant and the demo world
    /// only under `Demo`" — and `Sim::build` holds it with a `debug_assert!`.
    /// Deriving it here rather than taking a fourth argument keeps the two from
    /// ever being set inconsistently, which is the failure this would otherwise
    /// invite: a `Streaming` demo world blanks its outer ring forever, a
    /// `Complete` live world is the seam-baked-against-air defect unfixed.
    ///
    /// The one non-obvious case is the fallback session — vanilla assets failed
    /// to load, so a live connection meshes under `Demo`. `Complete` is still
    /// right there, because `MeshPolicy::id_spaces_agree` is `false` and
    /// [`TerrainMesh::mesh_column`] meshes nothing at all.
    #[must_use]
    pub fn new(worker_count: usize, classifier: ShellClassifier) -> Self {
        Self::new_inner(
            worker_count,
            classifier,
            #[cfg(test)]
            None,
        )
    }

    fn new_inner(
        worker_count: usize,
        classifier: ShellClassifier,
        #[cfg(test)] worker_gate: Option<NativeWorkerGate>,
    ) -> Self {
        let column_source = if classifier.is_vanilla() {
            ColumnSource::Streaming
        } else {
            ColumnSource::Complete
        };
        let worker_count = worker_count.max(1);
        let [(background_result_tx, background_result_rx), (edit_result_tx, edit_result_rx)] =
            std::array::from_fn(|_| crossbeam_channel::unbounded::<NativeMeshCompletion>());
        let result_tx = [background_result_tx, edit_result_tx];
        let result_rx = [background_result_rx, edit_result_rx];
        let worker_counters = Arc::new(NativeWorkerCounters::default());

        let [(background_job_tx, background_job_rx), (edit_job_tx, edit_job_rx)] =
            std::array::from_fn(|_| crossbeam_channel::unbounded::<Job>());
        let job_tx = [background_job_tx, edit_job_tx];
        let job_rx = [background_job_rx, edit_job_rx];

        let mut workers = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let rx = job_rx.clone();
            let result_tx = result_tx.clone();
            let classifier = classifier.clone();
            let counters = Arc::clone(&worker_counters);
            #[cfg(test)]
            let gate = worker_gate.clone();
            workers.push(thread::spawn(move || {
                #[cfg(test)]
                let ignore_cancellation = if let Some(gate) = gate {
                    gate.entered.send(()).expect("worker gate listener closed");
                    gate.release.recv().expect("worker gate release closed");
                    gate.ignore_cancellation
                } else {
                    false
                };
                let mut order = FairMeshOrder::default();
                while let Some((priority, job)) = receive_mesh_lane(&rx, order.preferred()) {
                    order.served(priority);
                    let Job::Mesh(snap, cutout_leaves, blend_radius, generation, token, submitted_at) = job;
                    let received_at = submitted_at.map(|_| Instant::now());
                    let cancelled = token.load(Ordering::Acquire);
                    #[cfg(test)]
                    let cancelled = cancelled && !ignore_cancellation;
                    let completion = if cancelled {
                        counters.skipped_before_mesh.fetch_add(1, Ordering::Relaxed);
                        NativeMeshCompletion::Skipped(submitted_at.zip(received_at).map(|(submitted, received)| {
                            NativeMeshResultTiming::new(submitted, received, None, Instant::now())
                        }))
                    } else {
                        counters.started.fetch_add(1, Ordering::Relaxed);
                        #[cfg(test)]
                        counters.started_keys.lock().unwrap().push(snap.key);
                        let started_at = submitted_at.map(|_| Instant::now());
                        let mut meshed = mesh_one(snap, &classifier, cutout_leaves, blend_radius);
                        meshed.native_timing = submitted_at.zip(received_at).map(|(submitted, received)| {
                            (priority, NativeMeshResultTiming::new(submitted, received, started_at, Instant::now()))
                        });
                        NativeMeshCompletion::Built(meshed, generation)
                    };
                    if result_tx[priority.index()].send(completion).is_err() {
                        break;
                    }
                }
            }));
        }

        Self {
            job_tx,
            result_rx,
            ready: std::array::from_fn(|_| std::collections::VecDeque::new()),
            handoff_order: FairMeshOrder::default(),
            workers,
            pending: 0,
            submitted: 0,
            worker_counters,
            stale_results_discarded: 0,
            native_timing: NativeMeshTiming::new(tracing::enabled!(target: "frame_profile", tracing::Level::DEBUG)),
            column_source,
            spatial_light_air: spatial_light_air(&classifier),
            cutout_leaves: true,
            blend_radius: BLEND_RADIUS,
            latest_generation: HashMap::new(),
            next_generation: 0,
        }
    }

    /// Sets the `options.cutoutLeaves` value future [`Self::submit`] calls
    /// stamp onto their jobs. Does **not** itself re-mesh anything already
    /// queued or uploaded — see `Sim::set_cutout_leaves` for the caller that
    /// forces a remesh of every loaded column, vanilla's own
    /// `operateOnLevelExtractor(LevelExtractor::allChanged)`.
    pub fn set_cutout_leaves(&mut self, value: bool) {
        self.cutout_leaves = value;
    }

    /// The `options.cutoutLeaves` value new jobs are currently stamped with.
    #[must_use]
    pub fn cutout_leaves(&self) -> bool {
        self.cutout_leaves
    }

    /// Sets the `options.biomeBlendRadius` value future [`Self::submit`] calls
    /// stamp onto their jobs. Same shape and same caveat as
    /// [`Self::set_cutout_leaves`]: it does not itself re-mesh anything, which
    /// is `TerrainMesh::set_blend_radius`'s job.
    pub fn set_blend_radius(&mut self, value: i32) {
        self.blend_radius = value;
    }

    /// The `options.biomeBlendRadius` value new jobs are currently stamped with.
    #[must_use]
    pub fn blend_radius(&self) -> i32 {
        self.blend_radius
    }

    /// Whether the world this pool meshes has all its columns already. See
    /// [`MeshScheduler::new`] for why the classifier decides this.
    #[must_use]
    pub fn column_source(&self) -> ColumnSource {
        self.column_source
    }

    /// Queue a snapshot with a new submission generation, cancelling its predecessor.
    pub fn submit(&mut self, snapshot: SectionSnapshot) {
        self.submit_with_priority(snapshot, MeshPriority::Background);
    }

    fn outstanding_priority(&self, key: &SectionKey, priority: MeshPriority) -> MeshPriority {
        self.latest_generation.get(key)
            .and_then(|current| current.pending_priority)
            .map_or(priority, |pending| priority.max(pending))
    }

    fn submit_with_priority(&mut self, snapshot: SectionSnapshot, priority: MeshPriority) {
        let priority = self.outstanding_priority(&snapshot.key, priority);
        let snapshot_key = snapshot.key;
        self.pending += 1;
        self.next_generation += 1;
        let generation = self.next_generation;
        let current = NativeGeneration::with_priority(generation, priority);
        let token = Arc::clone(&current.cancelled);
        if let Some(previous) = self.latest_generation.insert(snapshot.key, current) {
            previous.cancelled.store(true, Ordering::Release);
        }
        if self
            .job_tx[priority.index()]
            .send(Job::Mesh(
                snapshot,
                self.cutout_leaves,
                self.blend_radius,
                generation,
                token,
                self.native_timing.enabled().then(Instant::now),
            ))
            .is_err()
        {
            self.pending -= 1;
            self.latest_generation.remove(&snapshot_key);
        } else {
            self.submitted += 1;
        }
    }

    #[cfg(test)]
    fn submit_current(&mut self, snapshot: SectionSnapshot) {
        self.submit_current_with_priority(snapshot, MeshPriority::Background);
    }

    fn submit_current_with_priority(&mut self, snapshot: SectionSnapshot, priority: MeshPriority) {
        let priority = self.outstanding_priority(&snapshot.key, priority);
        self.forget_generation(&snapshot.key);
        self.submit_with_priority(snapshot, priority);
    }

    /// Number of submitted jobs not yet drained.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending
    }

    /// Sample worker atomics and owner-thread counters without inspecting queues.
    #[must_use]
    pub fn native_work_counters(&self) -> NativeMeshWorkCounters {
        NativeMeshWorkCounters {
            submitted: self.submitted,
            started: self.worker_counters.started.load(Ordering::Relaxed),
            skipped_before_mesh: self.worker_counters.skipped_before_mesh.load(Ordering::Relaxed),
            stale_results_discarded: self.stale_results_discarded,
        }
    }

    /// Drop `key`'s current generation. Any completion already in flight is no
    /// longer authoritative until a later [`Self::submit`] records a new
    /// generation. This is used both when a newer snapshot supersedes an older
    /// one and when a column leaves the view.
    pub fn forget_generation(&mut self, key: &SectionKey) {
        if let Some(previous) = self.latest_generation.remove(key) {
            previous.cancelled.store(true, Ordering::Release);
        }
        for ready in &mut self.ready {
            let before = ready.len();
            ready.retain(|(meshed, _)| {
                if meshed.key != *key { return true }
                if let Some((priority, timing)) = meshed.native_timing {
                    self.native_timing.settled(priority, Some(timing), NativeMeshDisposition::Stale);
                }
                false
            });
            self.pending -= before - ready.len();
            self.stale_results_discarded += (before - ready.len()) as u64;
        }
    }

    pub fn forget_column(&mut self, cx: i32, cz: i32) {
        let keys: Vec<_> = self
            .latest_generation
            .keys()
            .filter(|key| key.cx == cx && key.cz == cz)
            .copied()
            .collect();
        for key in keys {
            self.forget_generation(&key);
        }
    }

    /// Collect any finished meshes without blocking. A completion whose
    /// generation is not this key's *latest* submitted one is a stale mesh a
    /// later `submit` has already superseded — dropped here rather than
    /// handed to the caller, per [`Self::latest_generation`]'s doc.
    pub fn drain(&mut self) -> Vec<Meshed> {
        let mut out = Vec::new();
        while let Some((priority, completion)) = self.next_completion(false) {
            self.handoff_order.served(priority);
            if let Some(meshed) = self.settle_completion(priority, completion) {
                out.push(meshed);
            }
        }
        out
    }

    fn next_completion(&mut self, blocking: bool) -> Option<(MeshPriority, NativeMeshCompletion)> {
        let preferred = self.handoff_order.preferred();
        let other = match preferred {
            MeshPriority::Background => MeshPriority::Edit,
            MeshPriority::Edit => MeshPriority::Background,
        };
        for priority in [preferred, other] {
            let index = priority.index();
            if let Some((meshed, generation)) = self.ready[index].pop_front() {
                return Some((priority, NativeMeshCompletion::Built(meshed, generation)));
            }
            if let Ok(completion) = self.result_rx[index].try_recv() {
                return Some((priority, completion));
            }
        }
        if blocking {
            receive_mesh_lane(&self.result_rx, preferred)
        } else {
            None
        }
    }

    fn settle_completion(&mut self, priority: MeshPriority, completion: NativeMeshCompletion) -> Option<Meshed> {
        self.pending -= 1;
        let (meshed, generation) = match completion {
            NativeMeshCompletion::Built(meshed, generation) => (meshed, generation),
            NativeMeshCompletion::Skipped(timing) => {
                self.native_timing.settled(priority, timing, NativeMeshDisposition::Skipped);
                return None;
            }
        };
        let timing = meshed.native_timing.map(|(_, timing)| timing);
        if let Some(current) = self.latest_generation.get_mut(&meshed.key)
            && current.number == generation
        {
            current.pending_priority = None;
            self.native_timing.settled(priority, timing, NativeMeshDisposition::Built);
            Some(meshed)
        } else {
            self.stale_results_discarded += 1;
            self.native_timing.settled(priority, timing, NativeMeshDisposition::Stale);
            None
        }
    }

    /// Hand off a bounded batch of completed results, retaining overflow.
    pub fn drain_frame(&mut self) -> Vec<Meshed> {
        let count_budget = mesh_handoff_count_budget(
            MESH_UPLOAD_NS_PER_RESULT.load(Ordering::Relaxed),
        );
        self.drain_frame_with_limit(count_budget)
    }

    fn drain_frame_with_limit(&mut self, count_budget: usize) -> Vec<Meshed> {
        let mut out = Vec::new();
        let mut bytes = 0usize;
        let mut examined = 0usize;
        while examined < count_budget && out.len() < count_budget {
            let Some((priority, completion)) = self.next_completion(false) else {
                break;
            };
            examined += 1;
            let NativeMeshCompletion::Built(meshed, generation) = completion else {
                self.handoff_order.served(priority);
                let _ = self.settle_completion(priority, completion);
                continue;
            };
            if !self.latest_generation.get(&meshed.key)
                .is_some_and(|current| current.number == generation)
            {
                self.handoff_order.served(priority);
                let _ = self.settle_completion(priority, NativeMeshCompletion::Built(meshed, generation));
                continue;
            }
            let mesh_bytes = meshed.mesh.upload_bytes();
            if !out.is_empty()
                && bytes.saturating_add(mesh_bytes) > MESH_HANDOFF_BYTE_BUDGET
            {
                self.ready[priority.index()].push_front((meshed, generation));
                break;
            }
            bytes = bytes.saturating_add(mesh_bytes);
            self.handoff_order.served(priority);
            if let Some(meshed) = self.settle_completion(priority, NativeMeshCompletion::Built(meshed, generation)) {
                out.push(meshed);
            }
        }
        out
    }

    /// Block until at least `n` *current* results are available (or every
    /// currently-pending job — stale or not — has completed), returning
    /// everything collected. Used by tests and headless runs.
    ///
    /// Skipped jobs and stale built results settle pending work without counting
    /// toward `n`, so cancellation cannot strand a headless drain.
    pub fn drain_blocking(&mut self, n: usize) -> Vec<Meshed> {
        let mut out = Vec::new();
        while out.len() < n && self.pending > 0 {
            let Some((priority, completion)) = self.next_completion(true) else {
                break;
            };
            self.handoff_order.served(priority);
            if let Some(meshed) = self.settle_completion(priority, completion) {
                out.push(meshed);
            }
        }
        out
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for MeshScheduler {
    fn drop(&mut self) {
        for generation in self.latest_generation.values() {
            generation.cancelled.store(true, Ordering::Release);
        }
        self.job_tx = std::array::from_fn(|_| crossbeam_channel::unbounded().0);
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

// ---------------------------------------------------------------------------
// The browser scheduler (`wasm32`)
// ---------------------------------------------------------------------------

/// Browser capture and meshing share this deadline, checked after each section
/// so even a section exceeding the budget makes progress.
#[cfg(target_arch = "wasm32")]
const BROWSER_MESH_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);

/// Browser meshing on the owning render thread. Production terrain requests hold
/// section intents; [`TerrainMesh::drain_meshes_with_world`] captures each current
/// neighbourhood immediately before meshing. Explicit [`Self::submit`] callers
/// can still provide owned snapshots and drain without a world handle.
#[cfg(target_arch = "wasm32")]
#[derive(Debug, Resource)]
pub struct MeshScheduler {
    /// Replacements retain their position within a priority and invalidate ready results.
    backlog: BrowserMeshBacklog,
    classifier: ShellClassifier,
    column_source: ColumnSource,
    spatial_light_air: Option<u32>,
    /// Live render options, read when the queued section is meshed.
    cutout_leaves: bool,
    blend_radius: i32,
}

#[cfg(target_arch = "wasm32")]
impl MeshScheduler {
    /// Build the browser scheduler; `worker_count` is retained for API compatibility.
    #[must_use]
    pub fn new(worker_count: usize, classifier: ShellClassifier) -> Self {
        let column_source = if classifier.is_vanilla() {
            ColumnSource::Streaming
        } else {
            ColumnSource::Complete
        };
        tracing::info!(
            target: "mesh",
            requested_workers = worker_count,
            budget_ms = BROWSER_MESH_BUDGET.as_millis(),
            "browser mesh scheduler: no worker threads (thread::spawn traps on wasm32);              meshing in-frame under a time budget"
        );
        Self {
            backlog: BrowserMeshBacklog::default(),
            spatial_light_air: spatial_light_air(&classifier),
            classifier,
            column_source,
            cutout_leaves: true,
            blend_radius: BLEND_RADIUS,
        }
    }

    /// Whether the world this scheduler meshes has all its columns already.
    #[must_use]
    pub fn column_source(&self) -> ColumnSource {
        self.column_source
    }

    /// See the native scheduler's method of the same name.
    pub fn set_cutout_leaves(&mut self, value: bool) {
        self.cutout_leaves = value;
    }

    /// See the native scheduler's method of the same name.
    #[must_use]
    pub fn cutout_leaves(&self) -> bool {
        self.cutout_leaves
    }

    /// See the native scheduler's method of the same name.
    pub fn set_blend_radius(&mut self, value: i32) {
        self.blend_radius = value;
    }

    /// See the native scheduler's method of the same name.
    #[must_use]
    pub fn blend_radius(&self) -> i32 {
        self.blend_radius
    }

    /// Queue an explicit background snapshot, retaining any outstanding edit priority.
    pub fn submit(&mut self, snapshot: SectionSnapshot) {
        self.backlog.submit(snapshot);
    }

    /// Number of submitted jobs not yet drained.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.backlog.pending()
    }

    /// Remove queued and completed work superseded by an empty or deferred outcome.
    pub fn forget_generation(&mut self, key: &SectionKey) {
        self.backlog.forget_generation(key);
    }

    pub fn forget_column(&mut self, cx: i32, cz: i32) {
        self.backlog.forget_column(cx, cz);
    }

    pub fn discard_pending(&mut self) {
        self.backlog.discard_pending();
    }

    /// Drain explicit snapshots under the browser deadline. A capture intent
    /// requires [`TerrainMesh::drain_meshes_with_world`] and remains queued here.
    pub fn drain(&mut self) -> Vec<Meshed> {
        let mut out = std::mem::take(&mut self.backlog.ready);
        let mut now = crate::platform::Instant::now();
        let deadline = now + BROWSER_MESH_BUDGET;
        while let Some(snap) = self.backlog.pop_snapshot(now) {
            out.push(mesh_one(
                snap,
                &self.classifier,
                self.cutout_leaves,
                self.blend_radius,
            ));
            now = crate::platform::Instant::now();
            if now >= deadline {
                break;
            }
        }
        out
    }

    /// The browser's meshing drain already has its own time bound.
    pub fn drain_frame(&mut self) -> Vec<Meshed> {
        self.drain()
    }

    /// Mesh until at least `n` results exist (or the queue empties), ignoring the
    /// budget.
    ///
    /// The budget is not applied: headless one-shot work needs all requested
    /// meshes, and a partial result would leave no way to make progress.
    pub fn drain_blocking(&mut self, n: usize) -> Vec<Meshed> {
        while self.backlog.ready.len() < n {
            let Some(snap) = self.backlog.pop_snapshot(crate::platform::Instant::now()) else {
                break;
            };
            let meshed = mesh_one(
                snap,
                &self.classifier,
                self.cutout_leaves,
                self.blend_radius,
            );
            self.backlog.ready.push(meshed);
        }
        std::mem::take(&mut self.backlog.ready)
    }
}

// ---------------------------------------------------------------------------
// Terrain meshing as ECS state (Stage 4)
// ---------------------------------------------------------------------------

/// Legacy queue bound retained for queue-focused diagnostics. The live drain is
/// bounded by [`MESH_SNAPSHOT_SECTION_BUDGET`] because one column can contain
/// more sections than another.
pub const DIRTY_COLUMN_BUDGET: usize = 64;

/// Maximum section visits admitted by [`heal_dirty_columns`] in one frame.
/// Native visits capture snapshots; browser visits enqueue capture intents.
pub const MESH_SNAPSHOT_SECTION_BUDGET: usize = 96;
const PROVISIONAL_FIRST_MESH_RADIUS: i32 = 1;

/// Half-angle, in degrees, of the horizontal cone [`DirtyColumns`] treats as
/// "the player is looking at this column".
///
/// The same 60° (120° total) the server's join scheduler picks
/// (`lodestone_server::join_scheduler::FRUSTUM_HALF_ANGLE_DEGREES`), and for the
/// same reason: vanilla's default 70° *vertical* FOV is about 106° horizontal at
/// 16:9, so this is the real view plus a margin — a column about to rotate into
/// view should already have been meshed. **Deliberately a second copy of that
/// constant rather than an import**: the shell must not depend on
/// `lodestone-server` (the version seam, and singleplayer is the only build where
/// both exist), so what is shared is the semantics, stated here and in
/// `docs/section-mesh-invalidation.md`.
const MESH_FRUSTUM_HALF_ANGLE_DEGREES: f32 = 60.0;

/// How finely a yaw is quantised before it counts as "the player turned" — 16
/// sectors of 22.5°, again mirroring the server's `YAW_SECTORS`.
///
/// A *re-sort trigger*, not part of the ordering: the frustum test uses the raw
/// yaw. Quantising is what keeps re-prioritisation off the per-frame path — a
/// player panning smoothly re-keys the queue ~16 times per revolution instead of
/// on every frame.
const MESH_YAW_SECTORS: f32 = 16.0;

/// Chebyshev (chess-king) ring index of `coord` around `centre`.
#[must_use]
fn column_ring_distance(centre: (i32, i32), coord: (i32, i32)) -> i32 {
    (coord.0 - centre.0).abs().max((coord.1 - centre.1).abs())
}

/// Whether `coord` lies in the horizontal cone a player at column `centre`
/// facing `yaw_degrees` can see. Vanilla's yaw convention — 0 looks towards
/// `+Z`, 90 towards `−X`, the same one [`lodestone_physics::PlayerState::yaw`]
/// and `camera_rig`'s `Camera::forward` use.
///
/// The player's own column and its eight neighbours are always in view: the
/// direction to them is degenerate or dominated by where in the column the
/// player is standing, and they are the ground under their feet either way.
#[must_use]
fn column_in_frustum(centre: (i32, i32), yaw_degrees: f32, coord: (i32, i32)) -> bool {
    if column_ring_distance(centre, coord) <= 1 {
        return true;
    }
    if !yaw_degrees.is_finite() {
        return true;
    }
    let yaw = yaw_degrees.to_radians();
    let (fx, fz) = (-yaw.sin(), yaw.cos());
    let (dx, dz) = ((coord.0 - centre.0) as f32, (coord.1 - centre.1) as f32);
    let len = (dx * dx + dz * dz).sqrt();
    if len == 0.0 {
        return true;
    }
    ((fx * dx + fz * dz) / len) >= MESH_FRUSTUM_HALF_ANGLE_DEGREES.to_radians().cos()
}

/// The mesh queue's ordering: **distance first, facing cone second**.
///
/// A sort key rather than a comparator, so the order is a total, deterministic
/// function of integers — `(ring, penalty, cx, cz)`, the shape of the server's
/// `join_scheduler::view_order_key` (a *set* ordering: there is no prior walk to
/// inherit as a tie-break, so the coordinate is one).
///
/// * `ring` — Chebyshev distance from the player's column. **Primary, and that
///   is the anti-starvation property**: a column at distance `d` behind the
///   player (`(d, 1, …)`) still precedes every column at distance `d + 1`,
///   in view or not (`(d + 1, 0, …)`). Pure frustum-first would let a slow spin
///   starve what is behind the player, who then turns round into a hole.
/// * `penalty` — `0` inside the facing cone, `1` outside. The whole of "mesh
///   where the player is looking": it reorders *within* one ring and can never
///   promote a far column over a near one.
/// * `(cx, cz)` — a deterministic tie-break. With `facing: None` this key is
///   `(ring, 0, cx, cz)`, i.e. ring-by-ring and lexicographic inside a ring.
#[must_use]
fn mesh_order_key(
    centre: (i32, i32),
    facing: Option<f32>,
    coord: (i32, i32),
) -> (i32, u8, i32, i32) {
    let penalty = match facing {
        Some(yaw) if column_in_frustum(centre, yaw, coord) => 0,
        Some(_) => 1,
        None => 0,
    };
    (
        column_ring_distance(centre, coord),
        penalty,
        coord.0,
        coord.1,
    )
}

/// The quantised yaw sector a rotation falls in — see [`MESH_YAW_SECTORS`].
#[must_use]
fn mesh_yaw_sector(yaw_degrees: f32) -> i32 {
    if !yaw_degrees.is_finite() {
        return 0;
    }
    let wrapped = yaw_degrees.rem_euclid(360.0);
    (wrapped / (360.0 / MESH_YAW_SECTORS)).floor() as i32
}

/// The set of columns whose boundary geometry is stale, ordered so the
/// [`DIRTY_COLUMN_BUDGET`] is spent **near the player and in front of them
/// first**.
///
/// # Why this is not a `BTreeSet` any more
///
/// It was one, drained with `pop_first()` — i.e. lexicographically, smallest
/// `cx` then smallest `cz`. That is a corner of the world, not a place the
/// player is: a backlog (which `heal_dirty_columns` logs, and which forms on
/// every join) was therefore worked from `−x/−z` outward regardless of where the
/// camera pointed, so the server streaming its columns view-first
/// (`lodestone_server::join_scheduler`) reached no pixels in that order. The
/// visible symptom is chunks appearing behind you while you stare at a hole.
///
/// # Structure, and why this one
///
/// A `BinaryHeap` of [`mesh_order_key`] keys plus a `HashSet` of membership:
///
/// * the set is the **truth**, and it is what keeps "a column enqueued twice is
///   meshed once" — the property the `BTreeSet` gave for free. `insert` only
///   pushes a key when the set did not already hold the coordinate, and
///   [`pop_next`](Self::pop_next) is the only thing that takes one out;
/// * [`remove`](Self::remove) (a column that left the view) drops from the set
///   and leaves the heap entry as a **tombstone**, skipped on pop and compacted
///   when the heap grows past twice the live count. A heap has no cheap
///   arbitrary erase and this is a queue, so paying for one on every unload
///   would be the wrong trade;
/// * re-keying is a **rebuild**, not a per-frame sort, and it only happens when
///   the player's column or quantised yaw sector actually changed
///   ([`reprioritise`](Self::reprioritise)). At a 32-chunk view that is ≤ 4,225
///   integer keys, microseconds, ~16 times per revolution.
///
/// A plain sorted `Vec` was the alternative and loses: dirty columns arrive
/// continuously (every chunk arrival dirties up to eight), and an insert into a
/// sorted `Vec` is `O(n)` where the heap's is `O(log n)`.
#[derive(Debug, Default)]
pub struct DirtyColumns {
    /// The live set — membership, dedup, and [`len`](Self::len).
    queued: HashSet<(i32, i32)>,
    /// Best-first over [`mesh_order_key`]. May hold tombstones for coordinates
    /// no longer in `queued`; see the type doc.
    heap: BinaryHeap<Reverse<(i32, u8, i32, i32)>>,
    /// The player's column, as the keys in `heap` were computed against.
    centre: (i32, i32),
    /// The player's yaw in degrees, or `None` for "no rotation known" — a
    /// distinct state from any particular yaw, under which the ordering is
    /// distance-only and the facing cone is inert.
    facing: Option<f32>,
    /// The quantised sector `facing` was last re-keyed at — see
    /// [`MESH_YAW_SECTORS`].
    sector: Option<i32>,
}

impl DirtyColumns {
    /// Queue `coord` for a boundary re-mesh. Idempotent: a coordinate already
    /// queued is not queued twice, and will be meshed once.
    pub fn insert(&mut self, coord: (i32, i32)) -> bool {
        if !self.queued.insert(coord) {
            return false;
        }
        self.heap
            .push(Reverse(mesh_order_key(self.centre, self.facing, coord)));
        true
    }

    /// Drop `coord` from the queue — the column left the view, so the budget
    /// spent on it would go to a `mesh_column` that early-returns.
    pub fn remove(&mut self, coord: (i32, i32)) -> bool {
        if !self.queued.remove(&coord) {
            return false;
        }
        // Bound the tombstones: an unload sweep names a whole strip, and without
        // this the heap would keep every one of them for the session.
        if self.heap.len() > 2 * self.queued.len().max(32) {
            self.rebuild();
        }
        true
    }

    /// The highest-priority column, or `None` when the queue is empty.
    ///
    /// Skips tombstones left by [`remove`](Self::remove) — a popped key whose
    /// coordinate is no longer in the live set is not a column, it is a hole.
    pub fn pop_next(&mut self) -> Option<(i32, i32)> {
        while let Some(Reverse((_, _, cx, cz))) = self.heap.pop() {
            if self.queued.remove(&(cx, cz)) {
                return Some((cx, cz));
            }
        }
        None
    }

    /// Re-key the queue for a player who has moved to column `centre` or turned
    /// to `facing` (degrees of yaw), returning whether anything was re-ordered.
    ///
    /// **A no-op — and specifically not a rebuild — when neither the centre
    /// column nor the quantised yaw sector changed**, which is the common case on
    /// the frame this is called from. That gate is the whole of "re-prioritisation
    /// must be cheap".
    pub fn reprioritise(&mut self, centre: (i32, i32), facing: Option<f32>) -> bool {
        let sector = facing.map(mesh_yaw_sector);
        if self.centre == centre && self.sector == sector {
            // Keep the *old* yaw: it is what the current keys were computed
            // from, and storing a sub-sector nudge would make them disagree with
            // the ordering they are supposed to describe.
            return false;
        }
        self.centre = centre;
        self.facing = facing;
        self.sector = sector;
        self.rebuild();
        true
    }

    /// How many columns are queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queued.len()
    }

    /// Whether anything is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    /// Whether `coord` is queued.
    #[must_use]
    pub fn contains(&self, coord: (i32, i32)) -> bool {
        self.queued.contains(&coord)
    }

    /// Forget everything queued (session teardown).
    pub fn clear(&mut self) {
        self.queued.clear();
        self.heap.clear();
    }

    /// Re-derive every heap key from the live set — the only way the heap
    /// changes shape, and it drops tombstones on the way through.
    fn rebuild(&mut self) {
        let (centre, facing) = (self.centre, self.facing);
        self.heap = self
            .queued
            .iter()
            .map(|&coord| Reverse(mesh_order_key(centre, facing, coord)))
            .collect();
    }
}

/// The two facts terrain meshing needs that the [`ChunkWorld`] store cannot
/// answer, because they are properties of the *session* rather than of the
/// chunks.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshPolicy {
    /// How an **absent** sky sample resolves — the connected dimension's
    /// `has_skylight`. See [`sky_default_for_dimension`].
    pub sky_default: SkyDefault,
    /// Whether the id space the worker pool's classifier was built for is the id
    /// space the store actually holds.
    ///
    /// The demo palette and the vanilla registry are disjoint block-id spaces, so
    /// meshing one with the other's classifier does not fail — it draws garbage,
    /// or nothing. `false` is the "vanilla assets failed to load but we joined a
    /// server anyway" session, which used to `return` silently on a `world.get`
    /// miss and render an empty world with a clean log. It now counts into
    /// [`TerrainMesh::drops`] and warns.
    pub id_spaces_agree: bool,
}

impl Default for MeshPolicy {
    fn default() -> Self {
        Self {
            sky_default: SkyDefault::Full,
            id_spaces_agree: true,
        }
    }
}

/// Per-frame relighting work reported to the live benchmark dump.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RelightWorkload {
    pub skipped_unchanged: usize,
    pub skipped_light_equivalent: usize,
    pub input_blocks: usize,
    pub input_sections: usize,
    pub cells_visited: usize,
    pub cells_changed: usize,
    pub dirty_sections: usize,
    pub remesh_invalidations_enqueued: usize,
    pub remesh_invalidations_coalesced: usize,
    pub remesh_sections_submitted: usize,
}

/// All terrain-meshing state, as one `Resource`.
///
/// Stage 4 of `docs/bevy-migration.md` moves `Sim`'s `scheduler`,
/// `dirty_columns`, `pending_removals`, `uploaded_sections` and `mesh_drops`
/// here. **One** resource rather than five because they are one subsystem's
/// state and every operation touches several at once: a column that snapshots to
/// nothing pushes a removal and a drained mesh records an upload. Five `ResMut`s
/// would be five borrows of one invariant.
///
/// The worker pool is still a worker pool ([`MeshScheduler`]'s docs say why that
/// matters). Systems here only enqueue and drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshBacklog {
    pub ready_columns: usize,
    pub waiting_columns: usize,
    pub forced_columns: usize,
    pub pending_sections: usize,
    pub browser_queue: Option<BrowserMeshQueueStats>,
}

/// Browser queue lifetime totals and waiting time, sampled without walking its
/// entries.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BrowserMeshQueueStats {
    pub insertions: u64,
    pub replacements: u64,
    pub cancellations: u64,
    pub pops: u64,
    pub queued_keys: usize,
    /// Maximum live queued-key count since this scheduler was created.
    pub high_water_keys: usize,
    /// Time since the front key's first submission; replacements preserve it.
    pub oldest_wait: Duration,
    pub max_pop_wait: Duration,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MeshWorkCounters {
    pub light_input_checks: usize,
    pub light_input_reads: usize,
    pub light_input_skips: usize,
    pub light_input_retained_bytes: usize,
    pub light_input_retained_peak: usize,
    #[cfg(not(target_arch = "wasm32"))]
    pub native_scheduler: NativeMeshWorkCounters,
    #[cfg(not(target_arch = "wasm32"))]
    pub native_timing: NativeMeshTimingSnapshot,
    pub column_arrivals: usize,
    pub redecoded_column_arrivals: usize,
    pub column_snapshot_sections: usize,
    pub column_absorbed_light_sections: usize,
    pub neighbor_dirty_admissions: usize,
    pub light_patch_calls: usize,
    pub light_patch_invalidations: usize,
    pub light_patch_boundary_skips: usize,
    pub light_patch_spatial_reads: usize,
    pub light_patch_absorbed_sections: usize,
    pub local_light_admission: LightAdmissionCounters,
    pub light_section_snapshots: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LightAdmissionCounters {
    pub calls: usize,
    pub candidates: usize,
    pub queued: usize,
    pub spatial_rejected: usize,
    pub input_rejected: usize,
    pub spatial_reads: usize,
    pub absorbed: usize,
    pub coalesced: usize,
}

impl LightAdmissionCounters {
    fn merge(&mut self, other: Self) {
        self.calls += other.calls;
        self.candidates += other.candidates;
        self.queued += other.queued;
        self.spatial_rejected += other.spatial_rejected;
        self.input_rejected += other.input_rejected;
        self.spatial_reads += other.spatial_reads;
        self.absorbed += other.absorbed;
        self.coalesced += other.coalesced;
    }
}

fn spatial_light_air(classifier: &ShellClassifier) -> Option<u32> {
    match classifier {
        ShellClassifier::Demo(_) => Some(id::AIR),
        ShellClassifier::Vanilla(_) => {
            let models = classifier.models()?;
            let air = lodestone_data::block::Block::Air.default_state();
            (models.quads(air).is_empty() && models.fluid(air).is_none()).then_some(air.raw())
        }
    }
}

fn light_inputs_enabled() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var_os("LODESTONE_MESH_LIGHT_INPUTS").is_some())
    }
    #[cfg(target_arch = "wasm32")]
    { option_env!("LODESTONE_MESH_LIGHT_INPUTS") == Some("1") }
}

fn section_touches_light_patch(
    section: &ChunkSection,
    lo: [usize; 3],
    hi: [usize; 3],
) -> (bool, usize) {
    if lo == [0; 3] && hi == [15; 3] {
        return (true, 0);
    }
    let mut reads = 0;
    for y in lo[1]..=hi[1] {
        for z in lo[2]..=hi[2] {
            for x in lo[0]..=hi[0] {
                reads += 1;
                if section.get_block(x, y, z) != section.air_id() {
                    return (true, reads);
                }
            }
        }
    }
    (false, reads)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshColumnStatus {
    pub chunk: (i32, i32),
    pub missing_sections: Vec<usize>,
    pub prior_presentations: usize,
    pub waiting_for_halo: bool,
    pub absent_halo: Vec<(i32, i32)>,
}

#[derive(Resource, Debug)]
pub struct TerrainMesh {
    /// The off-thread worker pool.
    pub scheduler: MeshScheduler,
    /// Loaded columns whose *boundary* geometry is stale because a horizontal
    /// neighbour arrived after they were meshed, coalesced.
    ///
    /// A section's mesh depends on its whole 3×3×3 neighbourhood (face culling,
    /// AO, and — most visibly — fluid corner heights and flow faces), so loading
    /// column P invalidates the boundary of P's eight horizontal neighbours too.
    /// Meshing only P leaves every already-meshed neighbour believing there is air
    /// across the seam: **water grows a falling "wall" at each chunk border** and
    /// cross-chunk AO stays wrong. Re-meshing the eight eagerly on every arrival
    /// would be 9× the work, so they coalesce here and drain on a budget.
    ///
    /// Ordered **near the player and in front of them first**, not
    /// lexicographically — see [`DirtyColumns`] for what that fixed and why the
    /// container is no longer a `BTreeSet`.
    pub dirty_columns: DirtyColumns,
    /// Newly arrived columns waiting for a complete horizontal neighbourhood or
    /// near-player provisional admission. Waiting arrivals are deliberately not
    /// members of [`Self::dirty_columns`].
    pending_arrivals: HashSet<(i32, i32)>,
    /// Near-player columns painted early and awaiting one halo-complete rebuild.
    provisional_columns: HashSet<(i32, i32)>,
    /// Columns that must be meshed **even if a horizontal neighbour is missing**,
    /// because a neighbour is missing for a reason that will never resolve: it
    /// left the tracking view.
    ///
    /// This is the permanent-missing-neighbour case, unlike
    /// [`SnapshotOutcome::Deferred`], which means "a neighbour column has not arrived
    /// *yet*", and [`Self::accept_snapshot`] drops a deferred section that has never
    /// reached the GPU, relying on [`Self::mark_neighbours_dirty`] to re-drive it
    /// when the missing column lands. For a column on the **trailing** edge of the
    /// view the missing column never lands — it already came and went — so the
    /// deferral is permanent and nothing ever re-queues it.
    ///
    /// Whether that happens is a race between the heal budget and the player's
    /// speed, which is why it presents as "chunks stop drawing the further you
    /// go": a column that the snapshot-work budget reaches while the column
    /// behind it is still loaded gets uploaded and is thereafter exempt (the
    /// `uploaded_sections` clause);
    /// one it reaches later is dropped **forever**. Measured with counts and no
    /// timing at all in `standing_still_drains_the_heal_backlog_and_no_column_is_lost`:
    /// walking twelve steps at the real budget left the whole trailing column
    /// strip missing, and standing still afterwards never brought it back.
    ///
    /// So [`Self::forget_column`] enqueues the departing column's loaded
    /// neighbours here, where "missing" is known *at that moment* to mean gone
    /// rather than pending. Forcing can bake one seam against air if some *other*
    /// neighbour is genuinely still in flight, and that is the right trade: a
    /// wrong seam is corrected by the ordinary arrival signal, whereas invisible
    /// terrain you can still walk into is not corrected by anything.
    ///
    /// Coalesced and budgeted exactly like `dirty_columns` rather than re-meshed
    /// eagerly — an unload sweep names up to eight columns and a single step of
    /// the player unloads a whole strip.
    pub forced_columns: BTreeSet<(i32, i32)>,
    /// Columns known to have **left** the view, as opposed to not having arrived
    /// yet. The two are indistinguishable in the store — both are simply absent —
    /// and telling them apart is what makes [`Self::forced_columns`] safe.
    ///
    /// Without this the force is too broad, and the harness caught it: forcing
    /// every loaded neighbour of a departing column also drags in the
    /// **outermost** ring of the view, whose missing neighbour is missing because
    /// it is beyond the view entirely. That ring is exactly the buffer
    /// singleplayer streams beyond `render_distance` to keep *off* screen
    /// (`app/session.rs`), because a section meshed without its outer neighbour
    /// bakes its seam against air — the "blocky water far away" report. So a
    /// forced column is only really forced when **every** neighbour it still
    /// lacks is in this set.
    ///
    /// Bounded, not a second leak: an entry is dropped as soon as it has no
    /// loaded neighbour left, at which point it cannot affect any decision.
    pub departed: HashSet<(i32, i32)>,
    /// Sections whose light changed, coalesced and drained on a budget by
    /// [`remesh_light_dirty_sections`].
    ///
    /// Separate from [`Self::dirty_columns`] because the granularity is the point.
    /// A relight around one broken block touches a handful of sections; expressing
    /// that as a column dirty would re-mesh 24 sections each snapshotting a
    /// 27-section neighbourhood, per break, which is the cost the whole bounded-box
    /// design exists to avoid.
    ///
    /// Absolute `(chunk_x, chunk_z, section_y)`, exactly what
    /// [`lodestone_world::Relit::dirty_sections`] reports — converted to a
    /// [`SectionKey`] at drain time, when the store's extent is known.
    pub light_dirty_sections: BTreeSet<(i32, i32, i32)>,
    work_counters: MeshWorkCounters,
    mesh_measurement: MeshMeasurementSnapshot,
    light_inputs: HashMap<SectionKey, (u64, Option<light_reads::LightInputs>)>,
    light_revision: u64,
    light_inputs_enabled: bool,
    arrival_measurement: arrival_measurement::ArrivalMeasurement,
    arrival_reported_at: Option<crate::platform::Instant>,
    /// Light computation and section-capture work since the app sampled it.
    relight_workload: RelightWorkload,
    /// Sections whose geometry vanished (all-air after an edit, or a column that
    /// unloaded) and must be dropped from the GPU. Drained by the app each frame.
    pub pending_removals: Vec<SectionKey>,
    /// Sections whose latest non-empty mesh was handed to the renderer. This is
    /// deliberately separate from [`Self::uploaded_sections`]: that set records
    /// the scheduler result as soon as the app drains it so a deferred neighbour
    /// may be rebuilt, while this set advances only after `RenderState` receives
    /// the result. The loading gate must use the latter boundary, or one frame
    /// can be presented with a CPU result that has not reached the GPU yet.
    rendered_sections: ColumnSectionSet,
    /// Renderer-owned geometry remains present while its replacement is pending.
    presented_sections: ColumnSectionSet,
    /// Sections whose latest snapshot explicitly returned [`SnapshotOutcome::Empty`].
    /// An all-air section is a settled result, but only after the snapshot path
    /// says so; deriving emptiness from a resident-column count would let a
    /// decoded column release the loading screen before its sections were
    /// examined.
    empty_sections: ColumnSectionSet,
    /// Columns with at least one settled section. This column-level index keeps
    /// arrival admission O(1) instead of scanning every section result.
    built_columns: HashSet<(i32, i32)>,
    /// Every `SectionKey` this session has handed out for GPU upload and that has
    /// not yet come back out through a removal.
    ///
    /// `RenderState`'s GPU-side section map has no session id in its key, so
    /// without tracking this a quit-to-title followed by a reconnect would leave
    /// the *previous* server's terrain rendered until a new chunk happened to land
    /// on the exact same key.
    pub uploaded_sections: HashSet<SectionKey>,
    /// Count of loaded columns that failed a meshing guard (id spaces disagreed,
    /// or block storage reported non-air data but no section snapshot was
    /// eligible). A deliberately all-air column is valid and is not counted.
    /// Surfaced in the debug HUD next to `live_cols` so this defect class is a
    /// one-line diagnosis instead of a play-test archaeology session. Should
    /// stay `0` in a healthy session.
    pub drops: u64,
    /// Number of non-air columns for which a dirty signal yielded no meshable
    /// section. Kept separate from [`Self::drops`] so the diagnostic can sample
    /// warnings without making the id-space guard's count ambiguous.
    non_air_empty_columns: u64,
    /// Number of columns rejected because the classifier and storage use
    /// different block-id spaces. Kept separate so its warning can be sampled
    /// independently from the non-air content diagnostic.
    id_space_mismatch_columns: u64,
    /// Number of consecutive frames whose ready dirty queue remained non-empty.
    /// Reset when the ready queue drains so a warning describes current latency.
    backlog_frames: u64,
    /// Whether an absent neighbour column means "edge of the world" or "not here
    /// yet", taken from the worker pool's classifier — see
    /// [`MeshScheduler::new`].
    pub column_source: ColumnSource,
    /// How many times a section's **first** build was held back because a
    /// horizontal neighbour column had not arrived.
    ///
    /// Expected to be non-zero and rising during chunk streaming — every column
    /// on the frontier defers until the ring beyond it lands — and to stop rising
    /// once the player stops moving. A count that keeps climbing while nothing
    /// loads means the dirty-propagation half ([`Self::mark_neighbours_dirty`])
    /// has stopped re-driving deferred sections, which would show as terrain
    /// missing rather than terrain wrong.
    pub deferred: u64,
    /// The session facts meshing cannot read off the store.
    pub policy: MeshPolicy,
    /// The live biome registry's ordered entry names (follow-up),
    /// refreshed alongside [`Self::policy`] by `Sim::refresh_mesh_policy` and
    /// attached to every section this pool snapshots
    /// ([`SnapshotOutcome::with_biome_names`]) so `mesher::biome_name_at`
    /// resolves against the *real* server registry instead of the
    /// alphabetical `FALLBACK_BIOME_NAMES` table. Empty before any
    /// `registry_data` (no connection, the offline demo world, or a
    /// version/server that sends none) — [`biome_name_at`] treats that as
    /// "use the fallback", never as "holder id 0".
    pub biome_names: Arc<[&'static str]>,
}

impl TerrainMesh {
    /// Build the state around a freshly spawned worker pool.
    #[must_use]
    pub fn new(scheduler: MeshScheduler) -> Self {
        Self {
            column_source: scheduler.column_source(),
            scheduler,
            dirty_columns: DirtyColumns::default(),
            pending_arrivals: HashSet::new(),
            provisional_columns: HashSet::new(),
            forced_columns: BTreeSet::new(),
            departed: HashSet::new(),
            light_dirty_sections: BTreeSet::new(),
            work_counters: MeshWorkCounters::default(),
            mesh_measurement: MeshMeasurementSnapshot::default(),
            light_inputs: HashMap::new(),
            light_revision: 0,
            light_inputs_enabled: light_inputs_enabled(),
            arrival_measurement: arrival_measurement::ArrivalMeasurement::new({
                #[cfg(all(target_arch = "wasm32", feature = "runtime-presentation"))]
                { measurement::enabled() }
                #[cfg(all(target_arch = "wasm32", not(feature = "runtime-presentation")))]
                { false }
                #[cfg(not(target_arch = "wasm32"))]
                { tracing::enabled!(target: "frame_profile", tracing::Level::DEBUG) }
            }),
            arrival_reported_at: None,
            relight_workload: RelightWorkload::default(),
            pending_removals: Vec::new(),
            rendered_sections: ColumnSectionSet::new(),
            presented_sections: ColumnSectionSet::new(),
            empty_sections: ColumnSectionSet::new(),
            built_columns: HashSet::new(),
            uploaded_sections: HashSet::new(),
            drops: 0,
            non_air_empty_columns: 0,
            id_space_mismatch_columns: 0,
            backlog_frames: 0,
            deferred: 0,
            policy: MeshPolicy::default(),
            biome_names: Arc::from([]),
        }
    }

    /// Consume relighting work completed since the previous frame sample.
    pub(crate) fn take_relight_workload(&mut self) -> RelightWorkload {
        std::mem::take(&mut self.relight_workload)
    }

    /// Submit an accepted snapshot; unseen sections wait for their halo unless
    /// forced, while prior geometry can rebuild across a missing neighbour.
    #[cfg(not(target_arch = "wasm32"))]
    fn route(&mut self, key: SectionKey, outcome: SnapshotOutcome, force: bool) -> bool {
        self.route_with_priority(key, outcome, force, MeshPriority::Background)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn route_with_priority(
        &mut self,
        key: SectionKey,
        outcome: SnapshotOutcome,
        force: bool,
        priority: MeshPriority,
    ) -> bool {
        let Some(snapshot) = self.accept_snapshot(key, outcome, force) else {
            return false;
        };
        self.scheduler.submit_current_with_priority(snapshot, priority);
        true
    }

    fn accept_snapshot(
        &mut self,
        key: SectionKey,
        outcome: SnapshotOutcome,
        force: bool,
    ) -> Option<SectionSnapshot> {
        self.forget_light_inputs(&key);
        let mut accepted = match outcome {
            SnapshotOutcome::Ready(snap) => {
                self.rendered_sections.remove(&key);
                self.empty_sections.remove(&key);
                Some(snap)
            }
            // A single empty section is routine (sky/void sections have no
            // geometry): remove prior GPU geometry, if any.
            SnapshotOutcome::Empty => {
                // An older non-empty result must not arrive after this explicit
                // empty result and re-settle the section through
                // `mark_mesh_uploaded`.
                self.scheduler.forget_generation(&key);
                self.rendered_sections.remove(&key);
                self.presented_sections.remove(&key);
                let newly_empty = self.empty_sections.insert(key);
                self.built_columns.insert((key.cx, key.cz));
                if newly_empty && self.uploaded_sections.contains(&key) {
                    self.pending_removals.push(key);
                }
                None
            }
            SnapshotOutcome::Deferred(snap) => {
                self.rendered_sections.remove(&key);
                self.empty_sections.remove(&key);
                if force || self.uploaded_sections.contains(&key) {
                    Some(snap)
                } else {
                    self.scheduler.forget_generation(&key);
                    // Deliberately *not* a removal: there is nothing on the GPU
                    // for this key, and queueing one would make the deferral
                    // look like an unload to the app's drain.
                    self.deferred = self.deferred.saturating_add(1);
                    None
                }
            }
        };
        if self.light_inputs_enabled && let Some(snapshot) = accepted.as_mut() {
            self.light_revision += 1;
            snapshot.light_revision = Some(self.light_revision);
            self.light_inputs.insert(key, (self.light_revision, None));
        }
        accepted
    }

    fn forget_light_inputs(&mut self, key: &SectionKey) {
        if let Some((_, Some(inputs))) = self.light_inputs.remove(key) {
            self.work_counters.light_input_retained_bytes -= inputs.retained_bytes();
        }
    }

    fn forget_light_inputs_column(&mut self, cx: i32, cz: i32) {
        self.light_inputs.retain(|key, (_, inputs)| {
            if key.cx != cx || key.cz != cz { return true }
            if let Some(inputs) = inputs {
                self.work_counters.light_input_retained_bytes -= inputs.retained_bytes();
            }
            false
        });
    }

    #[cfg(any(target_arch = "wasm32", test))]
    fn accept_browser_capture(
        &mut self,
        key: SectionKey,
        outcome: SnapshotOutcome,
        force: bool,
        source: Option<browser_queue::CaptureSource>,
    ) -> Option<SectionSnapshot> {
        let captured_current = source.is_some() && matches!(&outcome, SnapshotOutcome::Ready(_));
        let snapshot = self.accept_snapshot(key, outcome, force)?;
        if captured_current {
            // Late capture reads all current light, including corrections still
            // waiting for the separate light-invalidation admission budget.
            self.light_dirty_sections.remove(&(
                key.cx, key.cz, key.min_y.div_euclid(16) + key.si as i32,
            ));
        }
        Some(snapshot)
    }

    #[cfg(any(target_arch = "wasm32", test))]
    fn prepare_browser_column(
        &mut self,
        column: &ChunkColumn,
        cx: i32,
        cz: i32,
        extent: lodestone_ecs::WorldExtent,
        force: bool,
    ) -> Vec<browser_queue::SectionIntent> {
        let mut intents = Vec::new();
        for si in 0..extent.section_count {
            let key = SectionKey { cx, cz, si, min_y: extent.min_y };
            if column.section(si).is_some_and(|section| !section.is_air_only()) {
                intents.push(browser_queue::SectionIntent {
                    key, section_count: extent.section_count, force,
                    source: browser_queue::CaptureSource::Column,
                    priority: MeshPriority::Background,
                });
            } else {
                // Empty geometry needs neither a neighbourhood snapshot nor a
                // queued capture, but must still cancel old work and remove it.
                let _ = self.accept_snapshot(key, SnapshotOutcome::Empty, force);
            }
            if self.light_dirty_sections.remove(&(
                cx, cz, extent.min_y.div_euclid(16) + si as i32,
            )) {
                self.work_counters.column_absorbed_light_sections += 1;
            }
        }
        intents
    }

    /// Forget the prior settlement observations for a column that has just been
    /// decoded again. The old GPU geometry may remain until the normal remesh
    /// drain, but it cannot satisfy the new column's initial loading milestone.
    pub fn reset_column_readiness(&mut self, cx: i32, cz: i32) {
        self.forget_light_inputs_column(cx, cz);
        self.rendered_sections.remove_column(cx, cz);
        self.presented_sections.remove_column(cx, cz);
        self.empty_sections.remove_column(cx, cz);
        self.built_columns.remove(&(cx, cz));
        self.provisional_columns.remove(&(cx, cz));
    }

    /// Queue a newly decoded column for admission after its horizontal halo is
    /// resident. Keeping this state separate from ordinary remesh requests lets
    /// an arrival wait without taking the snapshot lock or allocating doomed
    /// section snapshots.
    pub fn queue_column_arrival(&mut self, cx: i32, cz: i32) {
        if self.arrival_measurement.enabled() {
            self.arrival_measurement.observed((cx, cz), crate::platform::Instant::now());
        }
        self.work_counters.column_arrivals += 1;
        self.work_counters.redecoded_column_arrivals += usize::from(
            self.pending_arrivals.contains(&(cx, cz))
                || self.provisional_columns.contains(&(cx, cz))
                || self.built_columns.contains(&(cx, cz)),
        );
        self.reset_column_readiness(cx, cz);
        #[cfg(target_arch = "wasm32")]
        self.scheduler.forget_column(cx, cz);
        // A re-decoded column may still have an older boundary-heal request.
        // Admission owns the readiness transition, so remove that stale ready
        // entry before putting the column in the waiting set.
        self.dirty_columns.remove((cx, cz));
        self.pending_arrivals.insert((cx, cz));
    }

    fn mark_arrival_eligible(&mut self, cx: i32, cz: i32) {
        if self.arrival_measurement.contains((cx, cz)) {
            self.arrival_measurement.eligible((cx, cz), crate::platform::Instant::now());
        }
    }

    fn report_arrival_measurement(&mut self) {
        if !self.arrival_measurement.enabled() { return }
        let now = crate::platform::Instant::now();
        if self.arrival_reported_at.is_some_and(|last| now.duration_since(last) < Duration::from_secs(1)) {
            return;
        }
        self.arrival_reported_at = Some(now);
        let summary = self.arrival_measurement.snapshot();
        let (eligibility, admission) = self.arrival_measurement.oldest_waits(now);
        let eligibility = eligibility.map(|wait| wait.as_secs_f64() * 1e3);
        let admission = admission.map(|wait| wait.as_secs_f64() * 1e3);
        #[cfg(all(target_arch = "wasm32", feature = "runtime-presentation"))]
        crate::net::browser_diagnostic(format_args!(
            "{summary} oldest_eligibility_ms={eligibility:?} oldest_admission_ms={admission:?}",
        ));
        #[cfg(any(not(target_arch = "wasm32"), not(feature = "runtime-presentation")))]
        tracing::debug!(target: "frame_profile",
            "{summary} oldest_eligibility_ms={eligibility:?} oldest_admission_ms={admission:?}");
    }

    fn horizontal_halo_ready(store: &ChunkWorld, cx: i32, cz: i32) -> bool {
        (-1..=1).all(|dx| {
            (-1..=1).all(|dz| store.contains_column(cx + dx, cz + dz))
        })
    }

    fn column_has_prior_result(&self, cx: i32, cz: i32) -> bool {
        self.built_columns.contains(&(cx, cz))
    }

    fn promote_ready_arrival(&mut self, store: &ChunkWorld, cx: i32, cz: i32) -> bool {
        if !self.pending_arrivals.contains(&(cx, cz))
            || !store.contains_column(cx, cz)
            || (self.column_source == ColumnSource::Streaming
                && !Self::horizontal_halo_ready(store, cx, cz))
        {
            return false;
        }
        self.mark_arrival_eligible(cx, cz);
        self.pending_arrivals.remove(&(cx, cz));
        self.dirty_columns.insert((cx, cz));
        true
    }

    /// Admit one arrival from the coalesced heal queue. Near-player arrivals
    /// may build provisionally against an incomplete horizontal neighbourhood;
    /// other first builds wait for the full halo.
    pub(crate) fn mesh_arriving_column(
        &mut self,
        store: &ChunkWorld,
        cx: i32,
        cz: i32,
        allow_provisional: bool,
    ) -> usize {
        if !self.pending_arrivals.contains(&(cx, cz)) {
            return self.mesh_column(store, cx, cz);
        }
        if !store.contains_column(cx, cz) {
            self.pending_arrivals.remove(&(cx, cz));
            if self.arrival_measurement.contains((cx, cz)) {
                self.arrival_measurement.forget((cx, cz));
            }
            return 0;
        }
        let provisional = self.column_source == ColumnSource::Streaming
            && !self.column_has_prior_result(cx, cz)
            && !Self::horizontal_halo_ready(store, cx, cz);
        if provisional && !allow_provisional {
            return 0;
        }
        self.mark_arrival_eligible(cx, cz);
        self.pending_arrivals.remove(&(cx, cz));
        if provisional {
            self.provisional_columns.insert((cx, cz));
            return self.mesh_column_inner(store, cx, cz, true);
        }
        self.mesh_column(store, cx, cz)
    }

    /// Re-snapshot and re-schedule every section of the column at `(cx, cz)`.
    ///
    /// One implementation for both worlds, which is the point of Stage 4: before
    /// it, this branched on `vanilla_atlas.is_some() && net.is_some() &&
    /// world_dimensions().is_some()` and read one of two `World`s. Now there is
    /// one store, so there is one path.
    ///
    /// An **unloaded** column is a silent no-op: it will be queued for real by its
    /// own arrival, and counting it would drown the drop counter in noise. A
    /// loaded column whose stored block data is all air is also a valid no-op;
    /// biome-only sections and elided sky sections are expected to produce no
    /// geometry. Only a loaded column whose storage contains non-air blocks but
    /// yields no eligible snapshot reaches the "invisible blocks" alarm.
    pub fn mesh_column(&mut self, store: &ChunkWorld, cx: i32, cz: i32) -> usize {
        self.pending_arrivals.remove(&(cx, cz));
        self.mesh_column_inner(store, cx, cz, false)
    }

    /// [`Self::mesh_column`], but a section whose neighbourhood is incomplete is
    /// submitted anyway instead of held back.
    ///
    /// For columns drained from [`Self::forced_columns`] — those whose missing
    /// neighbour has *left the view* and is therefore never arriving. See that
    /// field's doc for why waiting on it is waiting forever.
    /// Forces only when [`Self::all_absent_neighbours_departed`] agrees. A column
    /// still genuinely waiting on an arrival remains in the admission-waiting
    /// set rather than being put back into the ready queue, which keeps the
    /// outermost buffer ring off screen without starving eligible work.
    pub fn mesh_column_forced(&mut self, store: &ChunkWorld, cx: i32, cz: i32) -> usize {
        let force = self.all_absent_neighbours_departed(store, cx, cz);
        if !force
            && self.pending_arrivals.contains(&(cx, cz))
            && store.contains_column(cx, cz)
            && self.column_source == ColumnSource::Streaming
            && !self.column_has_prior_result(cx, cz)
            && !Self::horizontal_halo_ready(store, cx, cz)
        {
            // The column remains in `pending_arrivals`; a future arrival will
            // promote it through `mark_neighbours_dirty`.
            return 0;
        }
        if self.arrival_measurement.contains((cx, cz))
            && (force || Self::horizontal_halo_ready(store, cx, cz))
        {
            self.mark_arrival_eligible(cx, cz);
        }
        self.pending_arrivals.remove(&(cx, cz));
        self.mesh_column_inner(store, cx, cz, force)
    }

    /// Sets `options.cutoutLeaves` and, only on a real change, re-meshes
    /// every currently-loaded column with the new value — vanilla's own
    /// `operateOnLevelExtractor(LevelExtractor::allChanged)` for this option.
    ///
    /// **The equality guard is load-bearing, not an optimisation.** Called
    /// every frame ([`crate::sim::Sim::set_cutout_leaves`]'s own doc), so
    /// without it every frame would re-mesh every loaded column — the
    /// present-mode poll's exact reasoning, applied to a far more expensive
    /// operation.
    ///
    /// Derives "every currently-loaded column" from
    /// [`Self::uploaded_sections`]'s keys rather than tracking a separate
    /// column set: that field is already every `SectionKey` this session has
    /// handed out for GPU upload and not yet had removed (its own doc), so a
    /// second list would be one more thing that could drift from it.
    pub fn set_cutout_leaves(&mut self, value: bool, store: &ChunkWorld) {
        if self.scheduler.cutout_leaves() == value {
            return;
        }
        self.scheduler.set_cutout_leaves(value);
        self.remesh_every_loaded_column(store);
    }

    /// Sets `options.biomeBlendRadius` and, only on a real change, re-meshes
    /// every currently-loaded column — vanilla's own
    /// `operateOnLevelExtractor(LevelExtractor::allChanged)` for this option
    /// too, and the same equality-guard reasoning as
    /// [`Self::set_cutout_leaves`]: this is polled every frame, so without the
    /// guard every frame would re-mesh the world.
    ///
    /// The blend window is per-*vertex* tint state baked into the mesh, so
    /// unlike a uniform there is no way to apply a new radius to geometry
    /// already uploaded — a remesh is the mechanism, not a heavy-handed
    /// version of one.
    pub fn set_blend_radius(&mut self, value: i32, store: &ChunkWorld) {
        if self.scheduler.blend_radius() == value {
            return;
        }
        self.scheduler.set_blend_radius(value);
        self.remesh_every_loaded_column(store);
    }

    /// Force-remesh every column this session currently has uploaded.
    ///
    /// Extracted because three callers now want exactly this — the two option
    /// setters above and [`Self::reload_classifier`] — and a fourth copy of the
    /// "derive the loaded set from `uploaded_sections`" walk would be one more
    /// thing that could drift from the others. `uploaded_sections` is every
    /// `SectionKey` this session has handed out for GPU upload and not yet had
    /// removed (its own doc), so it is the loaded set rather than a second
    /// list tracking it.
    fn remesh_every_loaded_column(&mut self, store: &ChunkWorld) {
        self.light_inputs.clear();
        self.work_counters.light_input_retained_bytes = 0;
        let columns: std::collections::BTreeSet<(i32, i32)> = self
            .uploaded_sections
            .iter()
            .map(|key| (key.cx, key.cz))
            .collect();
        for (cx, cz) in columns {
            self.mesh_column_forced(store, cx, cz);
        }
    }

    /// Respawns the worker pool against `classifier` — and therefore whatever
    /// atlas it carries — and force-remeshes every currently loaded column
    /// against it. The mesh-side half of a live resource-pack reload
    /// (`crate::sim::Sim::reload_resource_pack_atlas` is the caller); same
    /// "derive the loaded set from `uploaded_sections`, force every column"
    /// shape as [`Self::set_cutout_leaves`] just above, for the same reason:
    /// a fresh atlas moves every sprite's UVs, so re-submitting the *same*
    /// baked geometry without re-meshing would leave the world sampling the
    /// new atlas at the old atlas's coordinates — a visibly wrong texture,
    /// not a missing one.
    ///
    /// Unlike `set_cutout_leaves` there is no cheap equality guard here:
    /// `ShellClassifier` carries an `Arc<BlockAtlas>` with no `PartialEq`, and
    /// the caller already gates this on the pack-selection generation
    /// actually changing (`crate::resources::pack_generation`), so a second
    /// guard here would only ever see `true`.
    ///
    /// The old pool's worker threads are joined by [`MeshScheduler`]'s own
    /// `Drop` the moment the assignment below replaces `self.scheduler` — any
    /// job still queued or in flight for the *old* atlas is simply abandoned;
    /// every section it would have produced is re-submitted below against the
    /// *new* one, so nothing is lost, only redone. `cutout_leaves` is read
    /// and `blend_radius` are read off the outgoing scheduler and carried onto
    /// the new one so a live pack reload cannot silently reset the user's
    /// FAST-leaves or Biome Blend settings back to `MeshScheduler::new`'s
    /// defaults.
    pub fn reload_classifier(
        &mut self,
        store: &ChunkWorld,
        worker_count: usize,
        classifier: ShellClassifier,
    ) {
        let cutout_leaves = self.scheduler.cutout_leaves();
        let blend_radius = self.scheduler.blend_radius();
        let mut scheduler = MeshScheduler::new(worker_count, classifier);
        scheduler.set_cutout_leaves(cutout_leaves);
        scheduler.set_blend_radius(blend_radius);
        self.column_source = scheduler.column_source();
        self.scheduler = scheduler;
        self.remesh_every_loaded_column(store);
    }

    fn mesh_column_inner(
        &mut self,
        store: &ChunkWorld,
        cx: i32,
        cz: i32,
        force: bool,
    ) -> usize {
        if !self.policy.id_spaces_agree {
            self.reset_column_readiness(cx, cz);
            self.id_space_mismatch_columns += 1;
            self.drops += 1;
            if self.id_space_mismatch_columns.is_power_of_two() {
                tracing::warn!(
                    cx,
                    cz,
                    branch = "id-space-mismatch",
                    occurrences = self.id_space_mismatch_columns,
                    "column skipped: the mesh classifier's block-id space is not the store's \
                     (vanilla assets missing on a live session, or the reverse)"
                );
            }
            return 0;
        }
        let Some(extent) = store.extent() else {
            return 0;
        };

        if self.arrival_measurement.contains((cx, cz)) {
            self.arrival_measurement.admitted((cx, cz), crate::platform::Instant::now());
        }
        #[cfg(target_arch = "wasm32")]
        {
            let (summary, column_section_count, intents) = {
                let world = store.read();
                let Some(chunk) = world.get(ChunkPos::new(cx, cz)) else {
                    return 0;
                };
                (
                    ColumnBlockSummary::from_column(&chunk.column),
                    chunk.column.section_count(),
                    self.prepare_browser_column(&chunk.column, cx, cz, extent, force),
                )
            };
            if should_report_empty_column(summary, !intents.is_empty(), false) {
                self.report_empty_column(cx, cz, summary, column_section_count, extent.section_count);
            }
            for intent in intents {
                self.enqueue_browser_section(
                    intent.key, intent.section_count, intent.force, intent.source, intent.priority,
                );
            }
            extent.section_count
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.capture_column(store, cx, cz, force, extent)
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn capture_column(
        &mut self,
        store: &ChunkWorld,
        cx: i32,
        cz: i32,
        force: bool,
        extent: lodestone_ecs::WorldExtent,
    ) -> usize {
        // One lock for the whole column — the snapshots are owned and `Send`, so
        // the guard is dropped before anything is submitted and the world is
        // never locked while meshing.
        let mut jobs: Vec<(SectionKey, SnapshotOutcome)> =
            Vec::with_capacity(extent.section_count);
        let (summary, column_section_count) = {
            let world = store.read();
            // The arrival/unload signal and this read can race. Do not turn an
            // already-unloaded column into a false non-air diagnostic between
            // the extent read above and this snapshot lock.
            let Some(chunk) = world.get(ChunkPos::new(cx, cz)) else {
                return 0;
            };
            let summary = ColumnBlockSummary::from_column(&chunk.column);
            let column_section_count = chunk.column.section_count();
            for si in 0..extent.section_count {
                let key = SectionKey {
                    cx,
                    cz,
                    si,
                    min_y: extent.min_y,
                };
                jobs.push((
                    key,
                    snapshot_section_in(
                        &world,
                        key,
                        Some(extent.section_count),
                        self.policy.sky_default,
                        self.column_source,
                    )
                    .with_biome_names(Arc::clone(&self.biome_names)),
                ));
                if self.light_dirty_sections.remove(&(
                    cx,
                    cz,
                    extent.min_y.div_euclid(16) + si as i32,
                )) {
                    self.work_counters.column_absorbed_light_sections += 1;
                }
            }
            (summary, column_section_count)
        };

        let mut meshed_any = false;
        let mut deferred_any = false;
        self.work_counters.column_snapshot_sections += jobs.len();
        for (key, outcome) in jobs {
            deferred_any |= matches!(outcome, SnapshotOutcome::Deferred(_));
            meshed_any |= self.route(key, outcome, force);
        }
        if should_report_empty_column(summary, meshed_any, deferred_any) {
            self.report_empty_column(cx, cz, summary, column_section_count, extent.section_count);
        }
        extent.section_count
    }

    fn report_empty_column(
        &mut self,
        cx: i32,
        cz: i32,
        summary: ColumnBlockSummary,
        column_section_count: usize,
        mesh_section_count: usize,
    ) {
        self.non_air_empty_columns += 1;
        self.drops += 1;
        if self.non_air_empty_columns.is_power_of_two() {
            tracing::warn!(
                cx,
                cz,
                branch = "non-air-loaded-column",
                occurrences = self.non_air_empty_columns,
                allocated_sections = summary.allocated_sections,
                non_air_sections = summary.non_air_sections,
                non_air_blocks = summary.non_air_blocks,
                column_section_count,
                mesh_section_count,
                "loaded column contains non-air blocks but produced no eligible mesh section"
            );
        }
    }

    /// Invalidate edited geometry. Native capture is immediate; browser capture
    /// is deferred until [`Self::drain_meshes_with_world`]. Empty outcomes remove
    /// prior geometry; unseen incomplete neighbourhoods wait for their halo.
    pub fn mesh_section(&mut self, store: &ChunkWorld, key: SectionKey, section_count: usize) {
        self.request_section(store, key, section_count, false, MeshPriority::Edit);
    }

    fn request_section(
        &mut self,
        store: &ChunkWorld,
        key: SectionKey,
        section_count: usize,
        force: bool,
        priority: MeshPriority,
    ) {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = store;
            self.enqueue_browser_section(key, section_count, force, CaptureSource::Section, priority);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let outcome = {
                let world = store.read();
                snapshot_section_in(
                    &world,
                    key,
                    Some(section_count),
                    self.policy.sky_default,
                    self.column_source,
                )
                .with_biome_names(Arc::clone(&self.biome_names))
            };
            self.route_with_priority(key, outcome, force, priority);
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn enqueue_browser_section(
        &mut self,
        key: SectionKey,
        section_count: usize,
        force: bool,
        source: CaptureSource,
        priority: MeshPriority,
    ) {
        self.forget_light_inputs(&key);
        self.rendered_sections.remove(&key);
        self.empty_sections.remove(&key);
        self.scheduler.backlog.submit_intent(SectionIntent {
            key, section_count, force, source, priority,
        });
    }

    /// Retry through normal invalidation; residency comes from the renderer,
    /// since readiness can reset while prior GPU geometry remains resident.
    pub fn retry_mesh_upload(&mut self, store: &ChunkWorld, key: SectionKey, had_resident: bool) {
        if had_resident {
            self.uploaded_sections.insert(key);
        } else {
            self.uploaded_sections.remove(&key);
        }
        self.rendered_sections.remove(&key);
        self.empty_sections.remove(&key);
        if let Some(extent) = store.extent()
            && extent.min_y == key.min_y
            && key.si < extent.section_count
            && store.contains_column(key.cx, key.cz)
        {
            let force = self.provisional_columns.contains(&(key.cx, key.cz))
                || self.all_absent_neighbours_departed(store, key.cx, key.cz);
            self.request_section(store, key, extent.section_count, force, MeshPriority::Background);
        }
    }

    /// Queue meshes that can sample light sections overwritten by a server patch.
    pub fn queue_light_update(
        &mut self,
        store: &ChunkWorld,
        cx: i32,
        cz: i32,
        light_sections: &[usize],
    ) -> usize {
        let changes = light_sections.iter().copied()
            .map(lodestone_world::LightSectionChange::whole_section).collect::<Vec<_>>();
        self.queue_light_changes(store, cx, cz, &changes)
    }

    /// Queue only sections whose padded light domain intersects changed nibbles.
    pub fn queue_light_changes(
        &mut self,
        store: &ChunkWorld,
        cx: i32,
        cz: i32,
        changes: &[lodestone_world::LightSectionChange],
    ) -> usize {
        let Some(extent) = store.extent() else {
            return 0;
        };
        let world = store.read();
        let counters = self.admit_light_changes(&world, extent, cx, cz, changes.iter().copied());
        self.work_counters.light_patch_calls += counters.calls;
        self.work_counters.light_patch_invalidations += counters.queued;
        self.work_counters.light_patch_boundary_skips += counters.spatial_rejected;
        self.work_counters.light_patch_spatial_reads += counters.spatial_reads;
        self.work_counters.light_patch_absorbed_sections += counters.absorbed;
        counters.queued
    }

    fn queue_local_light_changes(&mut self, store: &ChunkWorld, relit: &lodestone_world::Relit) -> usize {
        if relit.light_changes.is_empty() { return 0 }
        let Some(extent) = store.extent() else { return 0 };
        let world = store.read();
        let mut counters = LightAdmissionCounters::default();
        for (&(cx, cz, section_index), &affected) in &relit.light_changes {
            counters.merge(self.admit_light_changes(&world, extent, cx, cz,
                [lodestone_world::LightSectionChange { section_index, affected }],
            ));
        }
        self.work_counters.local_light_admission.merge(counters);
        counters.queued
    }

    fn admit_light_changes(
        &mut self,
        world: &World,
        extent: lodestone_ecs::WorldExtent,
        cx: i32,
        cz: i32,
        changes: impl IntoIterator<Item = lodestone_world::LightSectionChange>,
    ) -> LightAdmissionCounters {
        let mut counters = LightAdmissionCounters::default();
        if extent.section_count == 0 || !world.contains(ChunkPos::new(cx, cz)) {
            return counters;
        }
        let base_si = extent.min_y.div_euclid(16);
        counters.calls = 1;
        for change in changes {
            let light_si = change.section_index;
            if light_si >= extent.section_count.saturating_add(2) {
                continue;
            }
            let source_light = self.light_inputs_enabled.then(|| world.get(ChunkPos::new(cx, cz))
                .filter(|chunk| light_si < chunk.light.light_section_count())
                .map(|chunk| chunk.light.section_light(light_si))).flatten();
            let resolved_light = source_light.as_ref()
                .map(|light| WorldSectionLight::new(light, self.policy.sky_default));
            let block_si = light_si as i32 - 1;
            for dy in -1..=1 {
                let si = block_si + dy;
                if si < 0 || si >= extent.section_count as i32 {
                    continue;
                }
                for dx in -1..=1 {
                    for dz in -1..=1 {
                        let Some((lo, hi)) = change.affected.affected_blocks(dx, dy, dz) else {
                            continue;
                        };
                        let (nx, nz) = (cx + dx, cz + dz);
                        let Some(section) = world
                            .get(ChunkPos::new(nx, nz))
                            .and_then(|chunk| chunk.column.section(si as usize))
                            .filter(|section| !section.is_air_only())
                        else {
                            continue;
                        };
                        counters.candidates += 1;
                        if self.pending_arrivals.contains(&(nx, nz))
                            || self.dirty_columns.contains((nx, nz))
                            || self.forced_columns.contains(&(nx, nz))
                        {
                            counters.absorbed += 1;
                            continue;
                        }
                        let destination = (nx, nz, base_si + si);
                        if self.light_dirty_sections.contains(&destination) {
                            counters.coalesced += 1;
                            continue;
                        }
                        let (touches, reads) = if self.scheduler.spatial_light_air == Some(section.air_id()) {
                            section_touches_light_patch(section, lo, hi)
                        } else {
                            (true, 0)
                        };
                        counters.spatial_reads += reads;
                        if !touches {
                            counters.spatial_rejected += 1;
                            continue;
                        }
                        let key = SectionKey { cx: nx, cz: nz, si: si as usize, min_y: extent.min_y };
                        if self.rendered_sections.contains(&key)
                            && let Some((_, Some(inputs))) = self.light_inputs.get(&key)
                            && let Some((lo, hi)) = change.affected.changed_cells(dx, dy, dz)
                        {
                            let (unchanged, reads) = inputs.unchanged(lo, hi, |[x, y, z]| {
                                let [x, y, z] = [x + dx * 16, y + dy * 16, z + dz * 16]
                                    .map(|value| value as usize);
                                resolved_light.as_ref().map_or(0xF0, |light| {
                                    light.sky_light(x, y, z) << 4 | light.block_light(x, y, z)
                                })
                            });
                            self.work_counters.light_input_checks += 1;
                            self.work_counters.light_input_reads += reads;
                            if unchanged {
                                counters.input_rejected += 1;
                                self.work_counters.light_input_skips += 1;
                                continue;
                            }
                        }
                        self.light_dirty_sections.insert(destination);
                        counters.queued += 1;
                    }
                }
            }
        }
        counters
    }

    #[cfg(test)]
    pub(crate) fn has_pending_arrival(&self, cx: i32, cz: i32) -> bool {
        self.pending_arrivals.contains(&(cx, cz))
    }

    /// Queue the eight **loaded** horizontal neighbours of `(cx, cz)` for a
    /// boundary re-mesh. The centre is meshed by the caller, immediately, for load
    /// responsiveness; the neighbours coalesce.
    ///
    /// This is the same eight columns vanilla's
    /// own enable-chunk-light routine dirties on chunk arrival
    /// (`setSectionRangeDirty(x-1, minSectionY, z-1, x+1, maxSectionY, z+1)` —
    /// the 3×3 column footprint over the whole vertical range), and it is *also*
    /// the mechanism that un-defers: a section held back by
    /// [`SnapshotOutcome::Deferred`] is re-snapshotted here the moment the column
    /// it was waiting for lands. Without this the deferral would be permanent
    /// rather than a wait.
    pub fn mark_neighbours_dirty(&mut self, store: &ChunkWorld, cx: i32, cz: i32) {
        for dx in -1..=1 {
            for dz in -1..=1 {
                let (nx, nz) = (cx + dx, cz + dz);
                if !store.contains_column(nx, nz) {
                    continue;
                }
                // Admission waiters are promoted only by the event that can
                // satisfy their halo. This keeps them out of the ready queue
                // while one or more dependencies are still absent.
                if self.pending_arrivals.contains(&(nx, nz)) {
                    self.promote_ready_arrival(store, nx, nz);
                    continue;
                }
                if dx == 0 && dz == 0 {
                    continue;
                }
                if self.provisional_columns.contains(&(nx, nz)) {
                    if !Self::horizontal_halo_ready(store, nx, nz) {
                        continue;
                    }
                    self.provisional_columns.remove(&(nx, nz));
                }
                self.work_counters.neighbor_dirty_admissions +=
                    usize::from(self.dirty_columns.insert((nx, nz)));
            }
        }
    }

    /// Drop every GPU section belonging to the column at `(cx, cz)`, which the
    /// server has just told us to forget.
    ///
    /// **This is the mesh side of an eviction that only ever had a collision
    /// side.** The adapter answers `forget_level_chunk` by calling
    /// `WorldSink::unload`, so the one [`ChunkWorld`] store loses the column
    /// immediately — and collision, which re-reads the store every tick
    /// (`sim/collide.rs`), tracks that for free. Nothing did the same for the
    /// renderer: `ClientEvent::ChunkUnloaded` had four producers and **zero**
    /// consumers, so [`Self::uploaded_sections`], `RenderState`'s section map
    /// and the fixed-capacity section-origin arena only ever grew, for the
    /// whole session, while the store shrank underneath them. Walking in one
    /// direction therefore accumulated every column ever visited as live draw
    /// calls — `gpu/frame.rs` iterates `model.sections` with no distance or
    /// frustum cull — and ended at an exhausted origin arena, whose failure
    /// mode is `upload_section` returning early: **new terrain stops drawing
    /// while its collision is perfectly present.** That is the reported symptom.
    ///
    /// Derived from [`Self::uploaded_sections`] rather than from the store,
    /// deliberately: by the time this runs the column is *already gone* from
    /// the store (the adapter unloads before it emits), so `store.extent()` and
    /// `contains_column` cannot enumerate what to drop. The uploaded set is the
    /// only record of what this column put on the GPU.
    ///
    /// The loaded neighbours go into [`Self::forced_columns`], **not**
    /// [`Self::dirty_columns`]. That distinction is essential for permanent
    /// evictions. An ordinary dirty signal re-snapshots them and then *drops the result*
    /// unless they already reached the GPU, because a missing neighbour reads as
    /// "not arrived yet". Here we know better: this very call is the neighbour
    /// leaving. A column on the trailing edge of the view that has never been
    /// uploaded would otherwise stay deferred forever, since the column it waits
    /// on already came and went and nothing re-queues it — measured as a whole
    /// missing column strip that standing still never recovers. See
    /// [`Self::forced_columns`].
    pub fn forget_column(&mut self, cx: i32, cz: i32) {
        self.forget_light_inputs_column(cx, cz);
        if self.arrival_measurement.contains((cx, cz)) {
            self.arrival_measurement.forget((cx, cz));
        }
        self.dirty_columns.remove((cx, cz));
        self.forced_columns.remove(&(cx, cz));
        self.pending_arrivals.remove(&(cx, cz));
        self.provisional_columns.remove(&(cx, cz));
        self.rendered_sections.remove_column(cx, cz);
        self.presented_sections.remove_column(cx, cz);
        self.empty_sections.remove_column(cx, cz);
        self.built_columns.remove(&(cx, cz));
        self.scheduler.forget_column(cx, cz);
        let gone: Vec<SectionKey> = self
            .uploaded_sections
            .iter()
            .filter(|key| key.cx == cx && key.cz == cz)
            .copied()
            .collect();
        for key in gone {
            self.uploaded_sections.remove(&key);
            self.pending_removals.push(key);
        }
    }

    /// Queue the loaded horizontal neighbours of a **departing** column for a
    /// forced re-mesh. Split from [`Self::forget_column`] because it needs the
    /// store and that one deliberately does not take it — see its doc.
    pub fn force_neighbours_of_departed(&mut self, store: &ChunkWorld, cx: i32, cz: i32) {
        self.departed.insert((cx, cz));
        for dx in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let (nx, nz) = (cx + dx, cz + dz);
                if store.contains_column(nx, nz) {
                    self.dirty_columns.remove((nx, nz));
                    self.pending_arrivals.remove(&(nx, nz));
                    self.forced_columns.insert((nx, nz));
                }
            }
        }
        // Keep `departed` bounded: an entry with no loaded neighbour can no
        // longer be the reason any column is forced, so it is pure growth.
        self.departed
            .retain(|&(dx, dz)| Self::has_loaded_neighbour(store, dx, dz));
    }

    fn has_loaded_neighbour(store: &ChunkWorld, cx: i32, cz: i32) -> bool {
        (-1..=1).any(|dx| {
            (-1..=1).any(|dz| {
                (dx, dz) != (0, 0) && store.contains_column(cx + dx, cz + dz)
            })
        })
    }

    /// Whether every horizontal neighbour `(cx, cz)` is missing has been
    /// confirmed to have *left*, rather than merely not arrived yet.
    ///
    /// The predicate that keeps the force narrow. `true` means nothing this column
    /// waits for is ever coming, so holding its geometry back is holding it back
    /// forever; `false` means it is an ordinary streaming frontier column and the
    /// existing deferral is right.
    fn all_absent_neighbours_departed(&self, store: &ChunkWorld, cx: i32, cz: i32) -> bool {
        for dx in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let n = (cx + dx, cz + dz);
                if !store.contains_column(n.0, n.1) && !self.departed.contains(&n) {
                    return false;
                }
            }
        }
        true
    }

    /// Re-snapshot and re-schedule the section holding `block`, plus any
    /// neighbour section that shares the boundary the block sits on (a face on
    /// a section edge changes the neighbour's mesh via culling/AO). Sections
    /// that became all-air are queued for GPU removal instead — `mesh_section`
    /// already routes that through [`Self::pending_removals`].
    ///
    /// Moved here from `Sim::remesh_around` (`sim/meshing.rs`), which had
    /// reduced to pure `ChunkWorld`/`TerrainMesh` math and no other `Sim`
    /// state — see `docs/plugin-api.md`'s re-mesh-seam note. `Sim::remesh_around`
    /// is now a one-line delegation through `Sim::terrain_and_world`; this is
    /// the version usable from anywhere that already holds a `&ChunkWorld`
    /// and a `&mut TerrainMesh` and nothing else, which is deliberately as far
    /// as this reaches: **not** a `RemeshRequest` resource or event a plugin
    /// could call — re-meshing stays a consequence of a sanctioned world
    /// write, never a plugin-callable verb (`docs/plugin-api.md`'s "what not
    /// to build" note).
    pub fn remesh_around(&mut self, store: &ChunkWorld, block: [i32; 3]) {
        let Some(extent) = store.extent() else {
            return;
        };
        let (min_y, section_count) = (extent.min_y, extent.section_count);
        let cx = block[0].div_euclid(16);
        let cz = block[2].div_euclid(16);
        let lx = block[0].rem_euclid(16);
        let lz = block[2].rem_euclid(16);
        let si = (block[1] - min_y).div_euclid(16);
        let ly = (block[1] - min_y).rem_euclid(16);

        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if (dx == -1 && lx != 0) || (dx == 1 && lx != 15) {
                        continue;
                    }
                    if (dy == -1 && ly != 0) || (dy == 1 && ly != 15) {
                        continue;
                    }
                    if (dz == -1 && lz != 0) || (dz == 1 && lz != 15) {
                        continue;
                    }
                    let nsi = si + dy;
                    if nsi < 0 || nsi as usize >= section_count {
                        continue;
                    }
                    let key = SectionKey {
                        cx: cx + dx,
                        cz: cz + dz,
                        si: nsi as usize,
                        min_y,
                    };
                    self.mesh_section(store, key, section_count);
                }
            }
        }
    }

    /// Collect finished meshes for the caller to upload, recording each key into
    /// [`Self::uploaded_sections`].
    pub fn drain_meshes(&mut self) -> Vec<Meshed> {
        let meshes = self.scheduler.drain_frame();
        self.uploaded_sections.extend(meshes.iter().map(|m| m.key));
        meshes
    }

    /// Drain native completions or capture current browser section intents.
    pub fn drain_meshes_with_world(&mut self, store: &ChunkWorld) -> Vec<Meshed> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = store;
            self.drain_meshes()
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.drain_browser_requests(store, Some(BROWSER_MESH_BUDGET))
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn drain_browser_requests(&mut self, store: &ChunkWorld, budget: Option<Duration>) -> Vec<Meshed> {
        let mut out = std::mem::take(&mut self.scheduler.backlog.ready);
        let measure = measurement::enabled();
        let mut now = crate::platform::Instant::now();
        let deadline = budget.map(|budget| now + budget);
        while let Some(request) = self.scheduler.backlog.queue.pop_front(now) {
            let key = request.key();
            let capture_started = measure.then(crate::platform::Instant::now);
            let (outcome, force, source) = {
                let world = store.read();
                request.capture(
                    &world,
                    self.policy.sky_default,
                    self.column_source,
                    Arc::clone(&self.biome_names),
                )
            };
            let cause = match source {
                Some(CaptureSource::Column) => MeshRequestCause::Column,
                Some(CaptureSource::Section) => MeshRequestCause::Section,
                Some(CaptureSource::Light) => MeshRequestCause::Light,
                None => MeshRequestCause::Explicit,
            };
            if let Some(started) = capture_started {
                self.mesh_measurement.capture(cause, started.elapsed());
            }
            match source {
                Some(CaptureSource::Column) => self.work_counters.column_snapshot_sections += 1,
                Some(CaptureSource::Light) => self.work_counters.light_section_snapshots += 1,
                _ => {}
            }
            if let Some(snapshot) = self.accept_browser_capture(key, outcome, force, source) {
                let (mut meshed, passes) = mesh_one_measured(
                    snapshot,
                    &self.scheduler.classifier,
                    self.scheduler.cutout_leaves,
                    self.scheduler.blend_radius,
                    measure,
                );
                if measure {
                    meshed.measurement_cause = Some(cause);
                    self.mesh_measurement.built(cause, passes);
                }
                out.push(meshed);
            }
            now = crate::platform::Instant::now();
            if deadline.is_some_and(|deadline| now >= deadline) {
                break;
            }
        }
        self.uploaded_sections.extend(out.iter().map(|mesh| mesh.key));
        out
    }

    #[must_use]
    pub fn backlog(&self) -> MeshBacklog {
        MeshBacklog {
            ready_columns: self.dirty_columns.len(),
            waiting_columns: self.pending_arrivals.len(),
            forced_columns: self.forced_columns.len(),
            pending_sections: self.scheduler.pending(),
            browser_queue: {
                #[cfg(target_arch = "wasm32")]
                {
                    Some(self.scheduler.backlog.stats())
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    None
                }
            },
        }
    }

    #[must_use]
    pub fn work_counters(&self) -> MeshWorkCounters {
        MeshWorkCounters {
            #[cfg(not(target_arch = "wasm32"))]
            native_scheduler: self.scheduler.native_work_counters(),
            #[cfg(not(target_arch = "wasm32"))]
            native_timing: self.scheduler.native_timing.snapshot(),
            ..self.work_counters
        }
    }

    /// Fixed-size debug-browser totals; unchanged outputs still ran every mesh pass.
    #[must_use]
    pub fn mesh_measurement(&self) -> MeshMeasurementSnapshot {
        self.mesh_measurement
    }

    /// Call once for the actual renderer result of each returned mesh.
    pub fn record_mesh_handoff(
        &mut self,
        meshed: &mut Meshed,
        outcome: MeshHandoffOutcome,
        upload_timing: Option<(Instant, Instant)>,
    ) {
        if let Some(revision) = meshed.light_revision
            && let Some((current, inputs)) = self.light_inputs.get_mut(&meshed.key)
            && revision == *current
        {
            if let Some(previous) = inputs.take() {
                self.work_counters.light_input_retained_bytes -= previous.retained_bytes();
            }
            if outcome != MeshHandoffOutcome::Failed {
                *inputs = meshed.light_inputs.take();
                if let Some(inputs) = inputs {
                    self.work_counters.light_input_retained_bytes += inputs.retained_bytes();
                    self.work_counters.light_input_retained_peak = self.work_counters
                        .light_input_retained_peak.max(self.work_counters.light_input_retained_bytes);
                }
            }
        }
        self.mesh_measurement.handoff(meshed, outcome);
        #[cfg(not(target_arch = "wasm32"))]
        if let (Some((priority, timing)), Some((started, finished))) = (meshed.native_timing, upload_timing) {
            self.scheduler.native_timing.uploaded(priority, Some(timing), started, finished, outcome);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = upload_timing;
    }

    #[must_use]
    pub fn column_status(&self, store: &ChunkWorld, cx: i32, cz: i32) -> Option<MeshColumnStatus> {
        let extent = store.extent()?;
        if !store.contains_column(cx, cz) {
            return None;
        }
        let mut missing_sections = Vec::new();
        let mut prior_presentations = 0;
        for si in 0..extent.section_count {
            let key = SectionKey {
                cx,
                cz,
                si,
                min_y: extent.min_y,
            };
            if self.rendered_sections.contains(&key) || self.empty_sections.contains(&key) {
                continue;
            }
            missing_sections.push(si);
            prior_presentations += usize::from(self.presented_sections.contains(&key));
        }
        let mut absent_halo = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                if !store.contains_column(cx + dx, cz + dz) {
                    absent_halo.push((cx + dx, cz + dz));
                }
            }
        }
        Some(MeshColumnStatus {
            chunk: (cx, cz),
            missing_sections,
            prior_presentations,
            waiting_for_halo: self.pending_arrivals.contains(&(cx, cz)),
            absent_halo,
        })
    }

    /// Record the renderer hand-off for one completed mesh.
    ///
    /// `drain_meshes` intentionally records only the CPU-side scheduler result
    /// in [`Self::uploaded_sections`], because the scheduler needs that fact to
    /// decide whether a deferred rebuild is allowed. The loading screen has a
    /// stricter boundary: it may clear only after the app has called
    /// `RenderState::upload_section`. `Sim::mark_mesh_uploaded` is the shell
    /// facade used by the frame loop immediately after that call.
    pub fn mark_mesh_uploaded(&mut self, key: SectionKey) {
        self.empty_sections.remove(&key);
        self.rendered_sections.insert(key);
        self.presented_sections.insert(key);
        self.built_columns.insert((key.cx, key.cz));
    }

    /// Current snapshot evidence for empty geometry. The renderer must also
    /// prove absence before this can settle a presentation-side observation.
    #[must_use]
    pub(crate) fn current_empty_section_settled(&self, key: SectionKey) -> bool {
        self.empty_sections.contains(&key) && !self.pending_removals.contains(&key)
    }

    /// Whether every section in one decoded column has reached a settled result.
    ///
    /// This is intentionally per-column and per-section. A non-empty section
    /// must be in [`Self::rendered_sections`], proving both CPU meshing and the
    /// renderer hand-off; an empty section must be in [`Self::empty_sections`],
    /// proving the snapshot path explicitly classified it as empty. Any section
    /// absent from both sets is still unknown, deferred, or queued and therefore
    /// keeps the loading gate closed. No global scheduler count or visible
    /// section count participates in this decision.
    #[must_use]
    pub fn column_mesh_settled(&self, store: &ChunkWorld, cx: i32, cz: i32) -> bool {
        let Some(extent) = store.extent() else {
            return false;
        };
        if !store.contains_column(cx, cz) {
            return false;
        }
        self.resident_column_mesh_settled(extent, cx, cz)
    }

    #[must_use]
    pub fn resident_column_mesh_settled(
        &self,
        extent: lodestone_ecs::WorldExtent,
        cx: i32,
        cz: i32,
    ) -> bool {
        (0..extent.section_count).all(|si| {
            let key = SectionKey {
                cx,
                cz,
                si,
                min_y: extent.min_y,
            };
            self.rendered_sections.contains(&key) || self.empty_sections.contains(&key)
        })
    }

    #[must_use]
    pub fn resident_column_presented(
        &self,
        extent: lodestone_ecs::WorldExtent,
        cx: i32,
        cz: i32,
    ) -> bool {
        (0..extent.section_count).all(|si| {
            let key = SectionKey {
                cx,
                cz,
                si,
                min_y: extent.min_y,
            };
            self.presented_sections.contains(&key) || self.empty_sections.contains(&key)
        })
    }

    /// Block until every scheduled mesh is ready. Headless runs and tests only —
    /// never the frame loop.
    pub fn drain_all_meshes(&mut self) -> Vec<Meshed> {
        let n = self.scheduler.pending();
        let meshes = self.scheduler.drain_blocking(n);
        self.uploaded_sections.extend(meshes.iter().map(|m| m.key));
        meshes
    }

    /// Resolve all browser intents for headless callers; native behavior is unchanged.
    pub fn drain_all_meshes_with_world(&mut self, store: &ChunkWorld) -> Vec<Meshed> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = store;
            self.drain_all_meshes()
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.drain_browser_requests(store, None)
        }
    }

    /// Sections the app should remove from the GPU.
    pub fn drain_removals(&mut self) -> Vec<SectionKey> {
        let removed = std::mem::take(&mut self.pending_removals);
        for key in &removed {
            self.uploaded_sections.remove(key);
        }
        removed
    }

    /// Session teardown: discard in-flight jobs rather than letting them land in
    /// whatever session comes next, and queue every section this session uploaded
    /// for removal through the app's ordinary drain path.
    pub fn end_session(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let pending = self.scheduler.pending();
            if pending > 0 {
                let _ = self.scheduler.drain_blocking(pending);
            }
        }
        #[cfg(target_arch = "wasm32")]
        self.scheduler.discard_pending();
        self.dirty_columns.clear();
        self.forced_columns.clear();
        self.pending_arrivals.clear();
        self.provisional_columns.clear();
        self.departed.clear();
        self.light_dirty_sections.clear();
        self.work_counters = MeshWorkCounters::default();
        self.mesh_measurement = MeshMeasurementSnapshot::default();
        self.arrival_measurement.clear();
        self.arrival_reported_at = None;
        self.report_arrival_measurement();
        self.arrival_measurement = arrival_measurement::ArrivalMeasurement::new(
            self.arrival_measurement.enabled(),
        );
        self.arrival_reported_at = None;
        self.drops = 0;
        self.non_air_empty_columns = 0;
        self.id_space_mismatch_columns = 0;
        self.backlog_frames = 0;
        self.deferred = 0;
        self.rendered_sections.clear();
        self.presented_sections.clear();
        self.empty_sections.clear();
        self.built_columns.clear();
        self.pending_removals.extend(self.uploaded_sections.drain());
    }
}

/// `Update` / [`FrameSet::Terrain`]: re-mesh queued columns whose boundary went
/// stale, bounded by [`MESH_SNAPSHOT_SECTION_BUDGET`] section visits. Native
/// visits capture worker snapshots; browser visits enqueue late-capture intents.
/// [`TerrainMesh::forced_columns`] is drained first so a departure cannot leave
/// an invisible trailing column behind.
/// Those columns are waiting on a neighbour that has already left the view, so
/// unlike an ordinary boundary heal they are not merely stale — they are
/// invisible until this runs, and a shared budget would put them behind whatever
/// backlog the ordinary queue happens to hold. That backlog reached 45 columns in
/// a twelve-step walk, i.e. eleven frames of latency, which is exactly the window
/// in which the old code lost them for good.
///
/// # Where the budget is spent
///
/// The queue is re-keyed here, once per frame, from the local player's own pose
/// (`lodestone_physics::PlayerState`'s `position` and `yaw` — the same values
/// mouse-look writes each frame, so this is the *live* facing and not last
/// tick's). [`DirtyColumns::reprioritise`] is a no-op unless the player's column
/// or yaw sector actually moved, so this costs a comparison on a typical frame.
///
/// This is the client half of the server's view-first streaming
/// (`lodestone_server::join_scheduler`): with the queue keyed lexicographically
/// the server's careful ordering reached no pixels, because a backlog was worked
/// from the `−x/−z` corner of the world whatever the camera was pointing at.
pub fn heal_dirty_columns(
    store: Res<ChunkWorld>,
    mut terrain: ResMut<TerrainMesh>,
    view: Query<&PhysicsState, With<LocalPlayer>>,
) {
    let view_center = view.iter().next().map(|state| {
        let center = (
            (state.0.position.x.floor() as i32).div_euclid(16),
            (state.0.position.z.floor() as i32).div_euclid(16),
        );
        terrain
            .dirty_columns
            .reprioritise(center, Some(state.0.yaw));
        center
    });
    if terrain.arrival_measurement.enabled() {
        if let Some((cx, cz)) = view_center {
            for dz in -PROVISIONAL_FIRST_MESH_RADIUS..=PROVISIONAL_FIRST_MESH_RADIUS {
                for dx in -PROVISIONAL_FIRST_MESH_RADIUS..=PROVISIONAL_FIRST_MESH_RADIUS {
                    let (nx, nz) = (cx + dx, cz + dz);
                    if terrain.pending_arrivals.contains(&(nx, nz)) && store.contains_column(nx, nz) {
                        terrain.mark_arrival_eligible(nx, nz);
                    }
                }
            }
        }
    }
    let mut snapshot_sections = 0usize;
    let mut eligible_columns = 0usize;
    let forced_attempts = terrain.forced_columns.len();
    for _ in 0..forced_attempts {
        if snapshot_sections >= MESH_SNAPSHOT_SECTION_BUDGET {
            break;
        }
        let Some((cx, cz)) = terrain.forced_columns.pop_first() else {
            break;
        };
        eligible_columns += 1;
        snapshot_sections += terrain.mesh_column_forced(&store, cx, cz);
    }
    if let Some((center_x, center_z)) = view_center {
        'near: for distance in 0..=PROVISIONAL_FIRST_MESH_RADIUS {
            for dz in -distance..=distance {
                for dx in -distance..=distance {
                    if snapshot_sections >= MESH_SNAPSHOT_SECTION_BUDGET {
                        break 'near;
                    }
                    if dx.abs().max(dz.abs()) != distance {
                        continue;
                    }
                    let (cx, cz) = (center_x + dx, center_z + dz);
                    if terrain.pending_arrivals.contains(&(cx, cz)) {
                        eligible_columns += 1;
                        snapshot_sections += terrain.mesh_arriving_column(&store, cx, cz, true);
                    }
                }
            }
        }
    }
    let dirty_attempts = terrain.dirty_columns.len();
    for _ in 0..dirty_attempts {
        if snapshot_sections >= MESH_SNAPSHOT_SECTION_BUDGET {
            break;
        }
        let Some((cx, cz)) = terrain.dirty_columns.pop_next() else {
            break;
        };
        eligible_columns += 1;
        snapshot_sections +=
            terrain.mesh_arriving_column(&store, cx, cz, view_center == Some((cx, cz)));
    }
    // The budget was depleted with work still queued, i.e. this frame's terrain is
    // a *choice* of which columns to mesh — which is what the ordering above is
    // for. Checked after the loop, not inside its `else`: in the `else` the queue
    // is empty by construction, so the old placement could never fire.
    if terrain.dirty_columns.is_empty() {
        terrain.backlog_frames = 0;
    } else {
        terrain.backlog_frames = terrain.backlog_frames.saturating_add(1);
        // Throttle: roughly every two seconds at 120 fps. Unlike the old
        // process-wide counter, this is the current contiguous backlog.
        if terrain.backlog_frames % 240 == 0 {
            tracing::warn!(
                "mesh column backlog: {} ready dirty columns waiting (snapshot budget is {} \
                 sections), {} deferred arrivals, {} eligible columns attempted, {} \
                 snapshot sections, {} consecutive backlogged frames",
                terrain.dirty_columns.len(),
                MESH_SNAPSHOT_SECTION_BUDGET,
                terrain.pending_arrivals.len(),
                eligible_columns,
                snapshot_sections,
                terrain.backlog_frames,
            );
        }
    }
    terrain.report_arrival_measurement();
}

/// Maximum section captures per frame in [`remesh_light_dirty_sections`].
///
/// Independent of [`MESH_SNAPSHOT_SECTION_BUDGET`] because the unit is smaller —
/// a *section*, not a whole column — and because the latency matters more: a black hole where a block
/// used to be is the symptom the relight exists to remove, so the sections around the
/// break should land in the frame after the break rather than queue behind a streaming
/// backlog. One break typically reports fewer sections than this, so the budget only
/// engages on a bulk edit.
pub const LIGHT_DIRTY_SECTION_BUDGET: usize = 24;

/// The 26.2 [`lodestone_world::LightProperties`] for a live session: the per-state
/// dampening and emission census, read straight out of rodata.
///
/// A zero-sized adapter rather than a table, so the relight has no per-call setup.
/// See [`lodestone_data::light_props`] for the provenance argument, and in particular
/// that every gap in it darkens rather than brightens — which is why a state we cannot
/// resolve can never fake a bright cell.
#[derive(Debug)]
struct VanillaLightProps;

impl lodestone_world::LightProperties for VanillaLightProps {
    fn opacity(&self, state: u32) -> u8 {
        lodestone_data::block_states::StateId::new(state)
            .map_or(0, lodestone_data::light_props::dampening)
    }

    fn emission(&self, state: u32) -> u8 {
        lodestone_data::block_states::StateId::new(state)
            .map_or(0, lodestone_data::light_props::emission)
    }
}

/// `Update` / [`FrameSet::Terrain`]: compute local light changes before snapshots.
///
/// Block updates need local relighting because a server may send light patches
/// only to players tracking that column at their outer view boundary.
///
/// Relighting can reach beyond the edited block's geometry neighbourhood, up to
/// [`lodestone_world::relight::AFFECTED_RADIUS`]. Queue those changed sections for
/// capture after full-column admission, so computed light reaches geometry.
///
/// The optional write handle belongs to the session owner; read-only harnesses
/// still drain server light intents through [`remesh_light_dirty_sections`].
pub fn relight_changed_blocks(
    write: Option<Res<ChunkWorldWrite>>,
    mut terrain: ResMut<TerrainMesh>,
    mut last_corrections: bevy_ecs::system::Local<(u64, u64)>,
) {
    let Some(write) = write else {
        return;
    };
    // Use the same block-id space and sky policy as the mesh classifier.
    let vanilla_ids = terrain.column_source == ColumnSource::Streaming;
    let has_skylight = matches!(terrain.policy.sky_default, SkyDefault::Full);

    let (relit, corrections) = {
        // Release the world write guard before snapshot capture takes a read lock.
        let mut world = write.write();
        let relit = if vanilla_ids {
            world.run_pending_relight(&VanillaLightProps, has_skylight)
        } else {
            world.run_pending_relight(&crate::blocks::DemoLightProps, has_skylight)
        };
        // Read under the same guard as the drain, so the pair describes one moment.
        (relit, world.light_correction_counts())
    };
    let merged = corrections.0.saturating_sub(last_corrections.0);
    let cancelled = corrections.1.saturating_sub(last_corrections.1);
    *last_corrections = corrections;

    if relit.dropped > 0 {
        tracing::warn!(
            target: "light",
            dropped = relit.dropped,
            "client relight dropped a job above its cell ceiling; a shaft that deep \
             stays lit by whatever the server last sent"
        );
    }
    if relit.jobs > 0 {
        tracing::debug!(
            target: "light",
            jobs = relit.jobs,
            cells_visited = relit.cells_visited,
            cells_changed = relit.cells_changed,
            sections = relit.dirty_sections.len(),
            deferred = relit.deferred,
            // Which props table the recompute read. A live session that reports
            // `false` here is lighting 26.2 block-state ids from the offline demo
            // table, which is a whole-world wrong answer rather than a small one.
            vanilla_ids,
            has_skylight,
            // Authoritative patches and local jobs they cancelled since the last drain.
            merged,
            cancelled,
            "client relight"
        );
        // One line per job, because the aggregate above cannot distinguish a
        // recompute that correctly darkened a hole from one that flooded an enclosed
        // room with daylight: `cells_changed` is the same number either way. The
        // signed split and the sky-source provenance are the discriminators.
        for job in &relit.detail {
            tracing::debug!(
                target: "light",
                at = ?job.change,
                changes = job.changes,
                region_min = ?job.region_min,
                region_max = ?job.region_max,
                sky_raised = job.sky_raised,
                sky_lowered = job.sky_lowered,
                max_sky_gain = job.max_sky_gain,
                block_raised = job.block_raised,
                block_lowered = job.block_lowered,
                max_block_gain = job.max_block_gain,
                sky_source_columns = job.sky_source_columns,
                // Sky sources this engine invented out of an absent section rather
                // than reading out of data the server sent. Non-zero underground,
                // beside a large `sky_raised`, is the flood.
                sky_sources_from_missing = job.sky_source_columns_from_missing,
                "client relight job"
            );
        }
    }
    let dirty_sections = relit.dirty_sections.len();
    let coalesced_before = terrain.work_counters.local_light_admission.coalesced;
    let remesh_invalidations_enqueued = terrain.queue_local_light_changes(&write.read_handle(), &relit);
    let remesh_invalidations_coalesced =
        terrain.work_counters.local_light_admission.coalesced - coalesced_before;
    let workload = &mut terrain.relight_workload;
    workload.skipped_unchanged += relit.skipped_unchanged;
    workload.skipped_light_equivalent += relit.skipped_light_equivalent;
    workload.input_blocks += relit.input_blocks;
    workload.input_sections += relit.jobs;
    workload.cells_visited += relit.cells_visited;
    workload.cells_changed += relit.cells_changed;
    workload.dirty_sections += dirty_sections;
    workload.remesh_invalidations_enqueued += remesh_invalidations_enqueued;
    workload.remesh_invalidations_coalesced += remesh_invalidations_coalesced;
}

/// Capture light-dirty sections not covered by this frame's column snapshots.
pub fn remesh_light_dirty_sections(store: Res<ChunkWorld>, mut terrain: ResMut<TerrainMesh>) {
    if terrain.light_dirty_sections.is_empty() {
        return;
    }
    let Some(extent) = store.extent() else {
        // No extent means no loaded column to mesh; keeping the queue would spend
        // the budget every frame on sections that cannot resolve.
        terrain.light_dirty_sections.clear();
        return;
    };
    let base_si = extent.min_y.div_euclid(16);
    // Missing-column snapshots use the light bridge. Count those rebuilds
    // separately from computed light changes to diagnose bright frontier faces.
    let mut bridged = 0usize;
    let mut meshed = 0usize;
    for _ in 0..LIGHT_DIRTY_SECTION_BUDGET {
        let Some((cx, cz, sy)) = terrain.light_dirty_sections.pop_first() else {
            break;
        };
        let si = sy - base_si;
        if si < 0 || si as usize >= extent.section_count {
            continue;
        }
        let key = SectionKey {
            cx,
            cz,
            si: si as usize,
            min_y: extent.min_y,
        };
        meshed += 1;
        #[cfg(not(target_arch = "wasm32"))]
        {
            terrain.work_counters.light_section_snapshots += 1;
        }
        if (-1..=1).any(|dx| {
            (-1..=1).any(|dz| !store.contains_column(cx + dx, cz + dz))
        }) {
            bridged += 1;
        }
        #[cfg(not(target_arch = "wasm32"))]
        terrain.request_section(&store, key, extent.section_count, false, MeshPriority::Background);
        #[cfg(target_arch = "wasm32")]
        terrain.enqueue_browser_section(
            key, extent.section_count, false, CaptureSource::Light, MeshPriority::Background,
        );
    }
    terrain.relight_workload.remesh_sections_submitted += meshed;
    if bridged > 0 {
        tracing::debug!(
            target: "light",
            bridged,
            meshed,
            "relight re-meshed sections whose neighbourhood is short a column; their \
             absent slots light at full sky"
        );
    }
}

/// Registers the terrain presentation systems and a default chunk read handle.
/// The session owner inserts [`TerrainMesh`] with its block classifier.
#[derive(Debug, Default)]
pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChunkWorld>();
        add_presentation_systems(app.world_mut());
    }
}

/// Adds ordered terrain systems to the owning ECS world's presentation schedule.
pub(crate) fn add_presentation_systems(world: &mut bevy_ecs::world::World) {
    let mut schedules = world.resource_mut::<bevy_ecs::schedule::Schedules>();
    schedules.add_systems(
        Update,
        (
            relight_changed_blocks,
            heal_dirty_columns,
            remesh_light_dirty_sections,
        )
            .chain()
            .in_set(FrameSet::Terrain)
            .in_set(crate::sim::presentation::PresentationSet),
    );
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod native_scheduler_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::DemoClassifier;

    fn sampled_input(position: [i32; 3], levels: u8) -> light_reads::LightInputs {
        let probe = light_reads::LightReadProbe::new();
        probe.observe(position[0], position[1], position[2], levels >> 4, levels & 15);
        probe.finish(|_| levels).unwrap()
    }

    #[test]
    fn light_inputs_publish_only_current_successful_handoffs_and_retire_on_capture() {
        let mut terrain = TerrainMesh::new(MeshScheduler::new(1, ShellClassifier::Demo(DemoClassifier)));
        terrain.light_inputs_enabled = true;
        let snapshot = floor_snapshot(3, true);
        let key = snapshot.key;
        let make = |snapshot: &SectionSnapshot| {
            let mut result = Meshed::new(key, SectionGeometry::Packed(Mesh::default()));
            result.light_revision = snapshot.light_revision;
            result.light_inputs = Some(sampled_input([7, 10, 5], 0x30));
            result
        };
        let first = terrain.accept_snapshot(key, SnapshotOutcome::Ready(snapshot), false).unwrap();
        let mut old = make(&first);
        terrain.record_mesh_handoff(&mut old, MeshHandoffOutcome::Applied, None);
        assert_eq!(terrain.work_counters.light_input_retained_bytes, 4);
        let next = terrain.accept_snapshot(key, SnapshotOutcome::Ready(floor_snapshot(11, true)), false).unwrap();
        assert!(terrain.light_inputs[&key].1.is_none(), "pending capture cannot use old values");
        let mut delayed = make(&first);
        terrain.record_mesh_handoff(&mut delayed, MeshHandoffOutcome::Applied, None);
        assert!(terrain.light_inputs[&key].1.is_none(), "stale renderer acknowledgement cannot revive inputs");
        let mut latest = make(&next);
        terrain.record_mesh_handoff(&mut latest, MeshHandoffOutcome::Unchanged, None);
        assert_eq!(terrain.work_counters.light_input_retained_bytes, 4);
        terrain.record_mesh_handoff(&mut latest, MeshHandoffOutcome::Failed, None);
        assert_eq!(terrain.work_counters.light_input_retained_bytes, 0);
        let mut latest = make(&next);
        terrain.reset_column_readiness(key.cx, key.cz);
        terrain.record_mesh_handoff(&mut latest, MeshHandoffOutcome::Applied, None);
        assert!(!terrain.light_inputs.contains_key(&key));
        assert_eq!(terrain.work_counters.light_input_retained_peak, 4);
    }

    #[test]
    fn light_inputs_admission_reads_values_not_just_changed_cell_bounds() {
        use lodestone_world::{ColumnLight, Heightmaps, LightBoundaryMask, LightSectionChange, LoadedChunk};
        let mut column = ChunkColumn::new(0, 1, PaletteKind::block_states(), PaletteKind::biomes(), id::AIR, 0);
        column.set_block(7, 9, 5, id::STONE);
        let mut world = World::new();
        world.load(ChunkPos::new(0, 0), LoadedChunk::new(column, ColumnLight::new(1), Heightmaps::new(), Vec::new()));
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let mut terrain = TerrainMesh::new(MeshScheduler::new(1, ShellClassifier::Demo(DemoClassifier)));
        terrain.light_inputs_enabled = true;
        terrain.policy.sky_default = SkyDefault::None;
        let key = SectionKey { cx: 0, cz: 0, si: 0, min_y: 0 };
        terrain.mark_mesh_uploaded(key);
        terrain.light_inputs.insert(key, (1, Some(sampled_input([7, 10, 5], 0))));
        terrain.work_counters.light_input_retained_bytes = 4;
        let change = |y| [LightSectionChange { section_index: 1, affected: LightBoundaryMask::for_cell(7, y, 5) }];
        assert_eq!(terrain.queue_light_changes(&store, 0, 0, &change(9)), 0);
        assert_eq!(terrain.work_counters.light_input_reads, 0);
        assert_eq!(terrain.queue_light_changes(&store, 0, 0, &change(10)), 0);
        assert_eq!(terrain.work_counters.light_input_reads, 1);
        terrain.light_inputs.insert(key, (1, Some(sampled_input([7, 10, 5], 0xB0))));
        assert_eq!(terrain.queue_light_changes(&store, 0, 0, &change(10)), 1, "changed sampled light must rebuild");
        terrain.light_dirty_sections.clear();
        terrain.forget_light_inputs(&key);
        assert_eq!(terrain.queue_light_changes(&store, 0, 0, &change(9)), 1, "unknown inputs must retain conservative admission");
        assert_eq!(terrain.work_counters.light_input_skips, 2);
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn snapshot_is_send() {
        // Compile-time proof that snapshots can cross to worker threads.
        assert_send::<SectionSnapshot>();
        assert_send::<Meshed>();
    }

    #[test]
    fn mesh_fingerprint_includes_visibility_and_geometry_kind() {
        let empty = || ModelMesh::default();
        let model = |visibility| SectionGeometry::Model {
            opaque: empty(),
            water: empty(),
            translucent_blocks: empty(),
            visibility,
        };
        assert_ne!(
            model(lodestone_render::SectionVisibility::NONE).fingerprint(),
            model(lodestone_render::SectionVisibility::all()).fingerprint()
        );
        assert_ne!(
            model(lodestone_render::SectionVisibility::NONE).fingerprint(),
            SectionGeometry::Packed(Mesh::default()).fingerprint()
        );
    }

    #[test]
    fn ao_occluder_census_accepts_a_leaf_and_rejects_state_count() {
        let leaf = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&raw| {
                lodestone_data::block_states::block_name(raw)
                    .is_some_and(|name| name.ends_with("_leaves"))
            })
            .expect("the block-state census contains a leaf state");

        assert!(ao_occludes_raw_state(leaf));
        assert!(!ao_occludes_raw_state(
            lodestone_data::block_states::STATE_COUNT
        ));
    }

    /// A dimension type as the server would send it, with only `has_skylight`
    /// varied — every other field is irrelevant to this policy and is set to a
    /// value that would be *wrong* for the overworld, so a test that accidentally
    /// read one of them fails.
    fn dim_type(name: &str, has_skylight: bool) -> lodestone_client::DimensionTypeInfo {
        lodestone_client::DimensionTypeInfo {
            name: name.parse().unwrap(),
            has_skylight,
            has_ceiling: true,
            has_fixed_time: true,
            coordinate_scale: 8.0,
            min_y: 0,
            height: 256,
            logical_height: 128,
            ambient_light: 0.1,
            // Irrelevant to this fixture's own policy, same rule as the other
            // fields this doc comment calls out.
            ambient_light_color: None,
            environment_attributes: Vec::new(),
            fog_color: None,
            sky_color: None,
            cloud_color: None,
            sky_light_factor: None,
        }
    }

    #[test]
    fn sky_default_is_full_for_overworld_and_end_none_for_nether_and_unknown() {
        use lodestone_client::DimensionId;

        let overworld: DimensionId = "minecraft:overworld".parse().unwrap();
        let the_nether: DimensionId = "minecraft:the_nether".parse().unwrap();
        let the_end: DimensionId = "minecraft:the_end".parse().unwrap();
        let custom: DimensionId = "somemod:cave_dimension".parse().unwrap();

        // Every case here passes `None` for the dimension type: this is the
        // name-match fallback, kept for servers that send no
        // `registry_data`.
        assert_eq!(
            sky_default_for_dimension(None, None),
            SkyDefault::Full,
            "pre-login: keep the full-bright default"
        );
        assert_eq!(
            sky_default_for_dimension(Some(&overworld), None),
            SkyDefault::Full
        );
        // The falsifying case this function exists for: the End has real sky
        // light (`has_skylight: true`) exactly like the overworld, and must
        // not be defaulted to `0` just because it isn't the overworld.
        assert_eq!(
            sky_default_for_dimension(Some(&the_end), None),
            SkyDefault::Full
        );
        assert_eq!(
            sky_default_for_dimension(Some(&the_nether), None),
            SkyDefault::None
        );
        assert_eq!(
            sky_default_for_dimension(Some(&custom), None),
            SkyDefault::None
        );
    }

    #[test]
    fn a_server_declared_dimension_type_overrides_the_level_name_match() {
        use lodestone_client::DimensionId;

        let overworld: DimensionId = "minecraft:overworld".parse().unwrap();
        let custom: DimensionId = "mypack:mine".parse().unwrap();

        // The name match and the registry disagree
        // in each case, and the registry must win — a test where they agree
        // would pass with the registry lookup deleted.
        assert_eq!(
            sky_default_for_dimension(
                Some(&overworld),
                Some(&dim_type("mypack:dark_overworld", false)),
            ),
            SkyDefault::None,
            "a level called minecraft:overworld with a skylight-less type must be dark"
        );
        assert_eq!(
            sky_default_for_dimension(
                Some(&custom),
                Some(&dim_type("minecraft:overworld", true)),
            ),
            SkyDefault::Full,
            "a datapack level pointing at a skylit type must be lit — this is the \
             case whose name match fell through to None before #288"
        );
    }

    // -----------------------------------------------------------------------
    // A seam meshed against a not-yet-loaded neighbour.
    // -----------------------------------------------------------------------

    /// Section count and `min_y` for the seam fixture: two sections, content in
    /// the lower one, so the upper is elided air and the `si == -1` slot is
    /// genuinely out of the world.
    const SEAM_SECTIONS: usize = 2;

    /// One fixture column. `water_over` decides, per `(x, z)`, whether section 0
    /// holds water at every `y` or nothing at all.
    fn seam_column(water_over: &dyn Fn(usize, usize) -> bool) -> lodestone_world::LoadedChunk {
        use lodestone_world::{ChunkColumn, ColumnLight, Heightmaps, LoadedChunk};

        let mut column = ChunkColumn::new(
            0,
            SEAM_SECTIONS,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            id::AIR,
            0,
        );
        for x in 0..16usize {
            for z in 0..16usize {
                if !water_over(x, z) {
                    continue;
                }
                for y in 0..16i32 {
                    column.set_block(x, y, z, id::WATER);
                }
            }
        }
        LoadedChunk::new(
            column,
            ColumnLight::new(SEAM_SECTIONS),
            Heightmaps::new(),
            Vec::new(),
        )
    }

    /// Which column plays the part of the east neighbour at `(1, 0)`.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum EastNeighbour {
        /// Not in the store at all — the chunk that has not arrived.
        Absent,
        /// Water for `z >= 8`, nothing for `z < 8`. The half-and-half split is
        /// what makes the converged answer a *number* rather than zero: a mesher
        /// that emitted no boundary faces at all would fail the same assertion a
        /// mesher that emitted all of them fails.
        HalfWater,
        /// All air. The **fixture control**: two columns with no shared water
        /// boundary structurally cannot exercise this bug, and this proves the
        /// measurement notices.
        AllAir,
    }

    /// The world for the seam gate: a 3×3 of all-water columns around `(0, 0)`,
    /// with `(1, 0)` replaced per `east`.
    ///
    /// All nine columns are populated (bar the absent case) so that **the east
    /// neighbour is the only variable**. With only the subject and its east
    /// neighbour in the store, seven other columns would be missing and every
    /// measurement would be `Deferred` — the variable would not be under test.
    fn seam_world(east: EastNeighbour) -> World {
        let mut world = World::new();
        for dx in -1..=1i32 {
            for dz in -1..=1i32 {
                if (dx, dz) == (1, 0) {
                    match east {
                        EastNeighbour::Absent => continue,
                        EastNeighbour::HalfWater => {
                            world.load(
                                ChunkPos::new(1, 0),
                                seam_column(&|_x, z| z >= 8),
                            );
                        }
                        EastNeighbour::AllAir => {
                            // A column that exists but holds nothing. `World::load`
                            // still makes it *present*, which is the point: the
                            // deferral is about presence, this control is about
                            // content.
                            world.load(ChunkPos::new(1, 0), seam_column(&|_x, _z| false));
                        }
                    }
                    continue;
                }
                world.load(ChunkPos::new(dx, dz), seam_column(&|_x, _z| true));
            }
        }
        world
    }

    /// The subject: section 0 of column `(0, 0)`.
    fn seam_key() -> SectionKey {
        SectionKey {
            cx: 0,
            cz: 0,
            si: 0,
            min_y: 0,
        }
    }

    /// Quads lying on the section's **east** boundary plane (`x == 16`) — the
    /// faces whose existence is decided by the column at `(1, 0)` — reported as
    /// a count plus the `(z, y)` bounding box they occupy.
    ///
    /// A count alone cannot tell a uniformly-wrong seam from a localised one, and
    /// `CLAUDE.md`'s "measure by location, never by frame average" applies to a
    /// mesh just as much as to a frame. The box is what turns a failure into a
    /// diagnosis.
    fn east_boundary(mesh: &Mesh) -> (usize, String) {
        use lodestone_render::Face;

        let mut count = 0usize;
        let (mut z0, mut z1, mut y0, mut y1) = (u32::MAX, 0u32, u32::MAX, 0u32);
        for quad in mesh.vertices.chunks_exact(4) {
            let fields: Vec<_> = quad.iter().map(|v| v.unpack()).collect();
            if !fields
                .iter()
                .all(|f| f.pos[0] == 16 && f.normal == Face::PosX)
            {
                continue;
            }
            count += 1;
            for f in &fields {
                z0 = z0.min(f.pos[2]);
                z1 = z1.max(f.pos[2]);
                y0 = y0.min(f.pos[1]);
                y1 = y1.max(f.pos[1]);
            }
        }
        let box_ = if count == 0 {
            "none".to_string()
        } else {
            format!("z {z0}..{z1}, y {y0}..{y1}")
        };
        (count, box_)
    }

    /// Mesh the subject section out of `world`, under `columns`, returning the
    /// outcome discriminant, the east-boundary count and its bounding box.
    fn seam_measure(world: &World, columns: ColumnSource) -> (&'static str, usize, String) {
        let outcome =
            snapshot_section_in(world, seam_key(), Some(SEAM_SECTIONS), SkyDefault::Full, columns);
        let label = match &outcome {
            SnapshotOutcome::Ready(_) => "Ready",
            SnapshotOutcome::Empty => "Empty",
            SnapshotOutcome::Deferred(_) => "Deferred",
        };
        let snap = outcome.any().expect("the subject section holds water");
        let (count, box_) = east_boundary(&mesh_snapshot(&snap, &DemoClassifier));
        (label, count, box_)
    }

    /// **Anti-vacuity, and the `CLAUDE.md` *world* species specifically.** The
    /// flaw this gate hunts lives in the input data, so the input data is
    /// asserted: the subject's east-most cells and the neighbour's west-most
    /// cells must both be water over the same `(y, z)` range, or nothing below
    /// can distinguish a fix from a fixture with no seam in it.
    #[test]
    fn the_seam_fixture_really_has_water_on_both_sides() {
        let world = seam_world(EastNeighbour::HalfWater);
        let subject = world
            .section(ChunkPos::new(0, 0), 0)
            .expect("subject section present");
        let east = world
            .section(ChunkPos::new(1, 0), 0)
            .expect("east neighbour present");

        let mut shared = 0usize;
        let mut subject_only = 0usize;
        for y in 0..16usize {
            for z in 0..16usize {
                let a = subject.get_block(15, y, z) == id::WATER;
                let b = east.get_block(0, y, z) == id::WATER;
                assert!(a, "the subject must be water across its whole east face");
                if b {
                    shared += 1;
                } else {
                    subject_only += 1;
                }
            }
        }
        assert_eq!(
            shared, 128,
            "half the seam must be water-against-water (the faces that should be culled)"
        );
        assert_eq!(
            subject_only, 128,
            "the other half must be water-against-air (the faces that should survive)"
        );
    }

    /// **The convergence gate.** Both halves, and the second one is the
    /// load-bearing one.
    ///
    /// 1. Meshing the subject with its east neighbour **absent** emits more
    ///    boundary faces than meshing it once the neighbour has arrived — the
    ///    count *drops*.
    /// 2. And the count after the neighbour arrives **equals** the count from
    ///    meshing with the neighbour present from the start. Without this, a
    ///    mesher that converged on some other wrong answer would pass: "it
    ///    changed" is not "it is right".
    #[test]
    fn a_seam_meshed_without_its_neighbour_converges_on_the_neighbour_present_answer() {
        // Meshed while the east column is still in flight. `ColumnSource::Complete`
        // is used here on purpose: it reproduces the earlier seam behavior exactly, which
        // had no other option — this is the *stale* mesh, measured, not described.
        let (stale_label, stale, stale_box) =
            seam_measure(&seam_world(EastNeighbour::Absent), ColumnSource::Complete);
        // The same section re-meshed after the column landed — what
        // `mark_neighbours_dirty` → `heal_dirty_columns` re-drives.
        let (healed_label, healed, healed_box) =
            seam_measure(&seam_world(EastNeighbour::HalfWater), ColumnSource::Streaming);
        // And the same section meshed once, with the neighbour there all along.
        let (fresh_label, fresh, fresh_box) =
            seam_measure(&seam_world(EastNeighbour::HalfWater), ColumnSource::Complete);

        assert_eq!(
            stale_label, "Ready",
            "the pre-fix policy must mesh the incomplete neighbourhood — otherwise this \
             is not the stale case"
        );
        assert_eq!(healed_label, "Ready", "a complete neighbourhood meshes");
        assert_eq!(fresh_label, "Ready", "a complete neighbourhood meshes");

        assert!(
            healed < stale,
            "half 1: the boundary face count must DROP once the neighbour arrives — \
             stale {stale} ({stale_box}) vs healed {healed} ({healed_box})"
        );
        assert_eq!(
            healed, fresh,
            "half 2 (load-bearing): re-meshing after the neighbour arrives must land on \
             exactly the from-the-start answer — healed {healed} ({healed_box}) vs fresh \
             {fresh} ({fresh_box})"
        );
        assert_eq!(
            (stale, healed),
            (256, 128),
            "the fixture's arithmetic: 16×16 boundary faces against air, half of them \
             culled by the neighbour's water — stale box {stale_box}, healed box {healed_box}"
        );
        // The neighbour holds water at `z >= 8`, so *those* seam faces are the
        // ones culled and the survivors are `z < 8` (a quad at cell `z = 7` spans
        // `z 7..8`, hence the exclusive upper bound). Asserting the box and not
        // just the count is what makes "128 faces survived" mean "the right 128":
        // the first version of this assertion had the halves the wrong way round
        // and the printed box is what said so.
        assert_eq!(
            healed_box, "z 0..8, y 0..16",
            "the surviving faces must be exactly the half with no water across the seam; \
             a count that matched with the wrong faces surviving would be a different bug"
        );
    }

    /// **The fix, at the seam that used to bake it silently.** With the world
    /// declared `Streaming`, the same absent neighbour that produced a `Ready`
    /// mesh above produces `Deferred` — and the snapshot says, in the type, how
    /// much of its neighbourhood is a guess.
    #[test]
    fn an_absent_neighbour_column_defers_the_build_and_is_typed_as_unloaded() {
        let absent = snapshot_section_in(
            &seam_world(EastNeighbour::Absent),
            seam_key(),
            Some(SEAM_SECTIONS),
            SkyDefault::Full,
            ColumnSource::Streaming,
        );
        assert!(
            matches!(absent, SnapshotOutcome::Deferred(_)),
            "a missing neighbour column must defer, not mesh"
        );
        assert!(
            absent.ready().is_none(),
            "`ready()` must refuse a deferred snapshot — this is what keeps the wrong \
             geometry off the screen"
        );

        let present = snapshot_section_in(
            &seam_world(EastNeighbour::HalfWater),
            seam_key(),
            Some(SEAM_SECTIONS),
            SkyDefault::Full,
            ColumnSource::Streaming,
        );
        let snap = match present {
            SnapshotOutcome::Ready(snap) => snap,
            other => panic!("a complete neighbourhood must be Ready, got {other:?}"),
        };
        assert_eq!(
            snap.unloaded_neighbours(),
            0,
            "a Ready snapshot must contain no guessed slot"
        );

        // The vertical boundary is *not* a guess: `si == -1` is below the world
        // and section 1 is an elided all-air section inside a column that has
        // arrived. Both are `Neighbour::Air` — air is the truth there — which is
        // why a column at the bottom of the world does not defer forever.
        let deferred = snapshot_section_in(
            &seam_world(EastNeighbour::Absent),
            seam_key(),
            Some(SEAM_SECTIONS),
            SkyDefault::Full,
            ColumnSource::Streaming,
        )
        .any()
        .expect("subject holds water");
        assert_eq!(
            deferred.unloaded_neighbours(),
            3,
            "exactly the three dy slots of the one absent column are guesses — not the \
             six vertical/elided ones, which are genuinely air"
        );
    }

    /// **Control 1, executed: a `Complete` world must NOT defer.** The offline
    /// demo world's outer ring has no neighbours and never will; deferring it
    /// would trade a temporary seam artifact for a permanent hole. If this ever starts
    /// deferring, `Sim::build`'s demo terrain loses its rim.
    #[test]
    fn control_a_complete_world_never_defers_an_absent_neighbour() {
        let outcome = snapshot_section_in(
            &seam_world(EastNeighbour::Absent),
            seam_key(),
            Some(SEAM_SECTIONS),
            SkyDefault::Full,
            ColumnSource::Complete,
        );
        assert!(
            matches!(outcome, SnapshotOutcome::Ready(_)),
            "a complete world's edge is the edge: air across it is the truth"
        );
        let snap = outcome.ready().expect("Ready");
        assert_eq!(
            snap.unloaded_neighbours(),
            0,
            "nothing in a complete world is 'not loaded yet'"
        );
    }

    /// **Control 2, executed: a fixture with no water across the seam cannot see
    /// this bug at all.** Same three measurements, same code, an east neighbour
    /// that is present but empty — and the count does *not* drop. This is the
    /// `CLAUDE.md` *world* species made to fire: had the real fixture been built
    /// this way, every assertion above would have passed with the fix reverted.
    #[test]
    fn control_a_seamless_fixture_shows_no_convergence() {
        let (_, stale, stale_box) =
            seam_measure(&seam_world(EastNeighbour::Absent), ColumnSource::Complete);
        let (_, healed, healed_box) =
            seam_measure(&seam_world(EastNeighbour::AllAir), ColumnSource::Streaming);
        assert_eq!(
            (stale, healed),
            (256, 256),
            "control: an all-air neighbour culls nothing, so arrival changes nothing — \
             stale {stale_box}, healed {healed_box}. A gate built on this fixture would be \
             blind to #389."
        );
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

    /// **A section with nothing in it must produce no snapshot at all.**
    ///
    /// The premise — *which* section is empty — is the whole difficulty, and this
    /// test got it wrong twice in the same way. It used to hard-code a section
    /// index; that was fixed to derive one from `surface_height(0, 0)`, which
    /// reads better but is the same mistake: a section is **16×16 columns**, and
    /// `surface_height(0, 0)` is the height of *one* of them. Any column in the
    /// chunk that reaches higher than the origin's puts geometry in the
    /// "guaranteed sky" section, and the feature stage duly did it — a birch tree
    /// at local (3, 13) reaches y82, twelve blocks above the origin's ground, so
    /// section 5 (y80–96) held leaves and the assertion failed with a real
    /// snapshot in hand.
    ///
    /// Deriving one index from one column cannot be repaired by picking a taller
    /// column either: chunk (0,0)'s own canopy reaches y82, so at
    /// `SECTION_COUNT = 6` (window y0–96) that chunk has **no** empty section at
    /// all. Any "the section above the terrain is sky" arithmetic is guessing.
    ///
    /// So the subject is *found* rather than computed — every section of every
    /// loaded chunk is classified all-air or not by direct scan, which makes the
    /// premise true by construction instead of by inference — and then both
    /// directions are asserted over the whole 3×3 world at once: every empty
    /// section must yield `None`, every non-empty one must yield `Some`. The two
    /// counts are asserted non-zero, because either half alone is satisfied by a
    /// `snapshot_section` that answers the same way always, and a world of all
    /// terrain or all sky would make one of them vacuous without saying so.
    #[test]
    fn empty_sky_section_is_skipped() {
        // Radius 1: 3×3 columns, so a chunk whose terrain does not reach the top
        // of the window is available even when the origin's does.
        let world = crate::worldgen::generate(1);
        let min_y = crate::worldgen::MIN_Y;

        let mut empty_sections = 0usize;
        let mut occupied_sections = 0usize;

        for cz in -1..=1 {
            for cx in -1..=1 {
                let loaded = world
                    .get(ChunkPos::new(cx, cz))
                    .expect("generate(1) loads the whole 3×3");
                let column = &loaded.column;
                for si in 0..crate::worldgen::SECTION_COUNT {
                    let base_y = min_y + (si as i32) * 16;
                    let mut occupied = None;
                    'scan: for lx in 0..16usize {
                        for lz in 0..16usize {
                            for ly in 0..16i32 {
                                let block = column.get_block(lx, base_y + ly, lz);
                                if block != crate::blocks::id::AIR {
                                    occupied = Some((lx, base_y + ly, lz, block));
                                    break 'scan;
                                }
                            }
                        }
                    }

                    let key = SectionKey {
                        cx,
                        cz,
                        si,
                        min_y,
                    };
                    let snapshot = snapshot_section(&world, key);
                    match occupied {
                        None => {
                            empty_sections += 1;
                            assert!(
                                snapshot.is_none(),
                                "section ({cx},{cz},{si}) is all air by direct \
                                 scan, so it must produce no snapshot — meshing \
                                 it costs a job and an upload per empty section \
                                 of every column"
                            );
                        }
                        Some((lx, y, lz, block)) => {
                            occupied_sections += 1;
                            assert!(
                                snapshot.is_some(),
                                "section ({cx},{cz},{si}) holds id {block} at \
                                 ({lx},{y},{lz}), so it must produce a snapshot — \
                                 skipping it is the invisible-terrain defect, not \
                                 an optimisation"
                            );
                        }
                    }
                }
            }
        }

        assert!(
            empty_sections > 0,
            "no loaded section was all air, so the skip path was never exercised"
        );
        assert!(
            occupied_sections > 0,
            "control: no loaded section had geometry, so `is_none()` above is \
             satisfied by a function that always returns None"
        );
    }

    /// Independent control for the column-level diagnostic. An allocated
    /// biome-only section is still intentionally empty geometry, while one
    /// non-air block must make the same column actionable. The direct scan is
    /// deliberately separate from `ChunkSection::non_air_count`, so this test
    /// checks the cached count rather than merely repeating it.
    #[test]
    fn column_summary_distinguishes_biome_only_air_from_non_air_storage() {
        fn direct_non_air_blocks(column: &ChunkColumn) -> usize {
            (0..column.section_count())
                .filter_map(|section_index| column.section(section_index))
                .map(|section| {
                    (0..section.block_states().entry_count())
                        .filter(|&index| {
                            section.block_states().get(index) != section.air_id()
                        })
                        .count()
                })
                .sum()
        }

        let mut column = ChunkColumn::new(
            0,
            2,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            id::AIR,
            0,
        );
        // A non-default biome allocates storage without adding a block.
        column.set_biome(0, 0, 0, 1);
        let air = ColumnBlockSummary::from_column(&column);
        assert_eq!(air.allocated_sections, 1);
        assert_eq!(air.non_air_sections, 0);
        assert_eq!(air.non_air_blocks, 0);
        assert_eq!(direct_non_air_blocks(&column), 0);
        assert!(air.is_all_air());
        assert!(
            !should_report_empty_column(air, false, false),
            "biome-only storage is a valid all-air mesh result"
        );

        column.set_block(0, 0, 0, id::WATER);
        let non_air = ColumnBlockSummary::from_column(&column);
        assert_eq!(non_air.allocated_sections, 1);
        assert_eq!(non_air.non_air_sections, 1);
        assert_eq!(non_air.non_air_blocks, 1);
        assert_eq!(direct_non_air_blocks(&column), 1);
        assert!(!non_air.is_all_air());
        assert!(
            should_report_empty_column(non_air, false, false),
            "a loaded non-air column with no eligible snapshot is actionable"
        );
        assert!(
            !should_report_empty_column(non_air, false, true),
            "a streaming deferral is not a dropped column"
        );
        assert!(
            !should_report_empty_column(non_air, true, false),
            "a submitted section is not an empty-column diagnostic"
        );
    }

    /// A loaded column can be all air while still carrying biome-only section
    /// storage. It must queue ordinary GPU removals without incrementing the
    /// dropped-column diagnostic that is reserved for non-air data.
    #[test]
    fn loaded_all_air_column_is_an_expected_mesh_noop() {
        use lodestone_world::{ColumnLight, Heightmaps, LoadedChunk};

        let mut column = ChunkColumn::new(
            0,
            2,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            id::AIR,
            0,
        );
        column.set_biome(0, 0, 0, 1);
        let mut world = World::new();
        world.load(
            ChunkPos::new(0, 0),
            LoadedChunk::new(column, ColumnLight::new(2), Heightmaps::new(), Vec::new()),
        );
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let mut terrain = TerrainMesh::new(MeshScheduler::new(
            1,
            ShellClassifier::Demo(DemoClassifier),
        ));
        let previously_uploaded = SectionKey {
            cx: 0,
            cz: 0,
            si: 0,
            min_y: 0,
        };
        terrain.uploaded_sections.insert(previously_uploaded);

        terrain.mesh_column(&store, 0, 0);

        assert_eq!(terrain.drops, 0);
        assert_eq!(terrain.non_air_empty_columns, 0);
        assert_eq!(terrain.pending_removals, vec![previously_uploaded]);
        terrain.mesh_column(&store, 0, 0);
        assert_eq!(terrain.pending_removals, vec![previously_uploaded]);
    }

    #[test]
    fn light_patch_queues_only_sections_with_geometry() {
        use lodestone_world::{ColumnLight, Heightmaps, LoadedChunk};

        let mut world = World::new();
        for cx in -1..=1 {
            for cz in -1..=1 {
                let mut column = ChunkColumn::new(
                    0,
                    2,
                    PaletteKind::block_states(),
                    PaletteKind::biomes(),
                    id::AIR,
                    0,
                );
                if (cx, cz) == (0, 0) {
                    column.set_block(0, 0, 0, id::STONE);
                } else if (cx, cz) == (1, 0) {
                    column.set_block(8, 8, 8, id::STONE);
                } else if (cx, cz) == (-1, 0) {
                    column.set_block(15, 8, 8, id::STONE);
                } else if (cx, cz) == (1, 1) {
                    column.set_block(0, 16, 0, id::STONE);
                }
                world.load(
                    ChunkPos::new(cx, cz),
                    LoadedChunk::new(column, ColumnLight::new(2), Heightmaps::new(), Vec::new()),
                );
            }
        }
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let mut terrain = TerrainMesh::new(MeshScheduler::new(
            1,
            ShellClassifier::Demo(DemoClassifier),
        ));

        assert_eq!(terrain.queue_light_update(&store, 0, 0, &[1]), 3);
        assert_eq!(
            terrain.light_dirty_sections,
            BTreeSet::from([(-1, 0, 0), (0, 0, 0), (1, 1, 1)])
        );
        assert_eq!(terrain.work_counters.light_patch_boundary_skips, 1);
        terrain.light_dirty_sections.clear();
        assert_eq!(terrain.queue_light_update(&store, -1, -1, &[0]), 1);
        assert_eq!(terrain.light_dirty_sections, BTreeSet::from([(0, 0, 0)]));

        terrain.light_dirty_sections.clear();
        terrain.pending_arrivals.insert((0, 0));
        terrain.dirty_columns.insert((-1, 0));
        assert_eq!(terrain.queue_light_update(&store, 0, 0, &[1]), 1);
        assert_eq!(terrain.light_dirty_sections, BTreeSet::from([(1, 1, 1)]));
        assert_eq!(terrain.work_counters.light_patch_absorbed_sections, 2);

        terrain.light_dirty_sections.clear();
        terrain.uploaded_sections.insert(SectionKey {
            cx: 0,
            cz: 0,
            si: 0,
            min_y: 0,
        });
        assert_eq!(terrain.queue_light_update(&store, 0, 0, &[1]), 1);
        assert_eq!(terrain.light_dirty_sections, BTreeSet::from([(1, 1, 1)]));
        assert_eq!(terrain.work_counters.light_patch_absorbed_sections, 4);

        terrain.light_dirty_sections.clear();
        terrain.pending_arrivals.remove(&(0, 0));
        terrain.dirty_columns.remove((-1, 0));
        assert_eq!(terrain.queue_light_update(&store, 0, 0, &[1]), 3);
    }

    #[test]
    fn precise_light_fanout_matches_padded_domains_including_sentinels() {
        use lodestone_world::{ColumnLight, Heightmaps, LightBoundaryMask, LightSectionChange, LoadedChunk};
        let mut world = World::new();
        for cx in -4..=-2 {
            for cz in 6..=8 {
                let mut column = ChunkColumn::new(
                    -16, 3, PaletteKind::block_states(), PaletteKind::biomes(), id::AIR, 0,
                );
                for si in 0..3 {
                    for x in 0..16 {
                        for y in 0..16 {
                            for z in 0..16 {
                                column.set_block(x, -16 + si * 16 + y, z, id::STONE);
                            }
                        }
                    }
                }
                world.load(ChunkPos::new(cx, cz), LoadedChunk::new(
                    column, ColumnLight::new(3), Heightmaps::new(), Vec::new(),
                ));
            }
        }
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let mut terrain = TerrainMesh::new(MeshScheduler::new(1, ShellClassifier::Demo(DemoClassifier)));
        let cases = [
            (2, [7, 9, 5], 1),
            (2, [0, 9, 5], 2), (2, [15, 9, 5], 2),
            (2, [1, 9, 5], 2), (2, [14, 9, 5], 2),
            (2, [7, 0, 5], 2), (2, [7, 15, 5], 2),
            (2, [7, 9, 0], 2), (2, [7, 9, 15], 2),
            (2, [0, 15, 5], 4), (2, [15, 0, 15], 8),
            (0, [7, 9, 5], 0), (0, [7, 15, 5], 1),
            (4, [7, 9, 5], 0), (4, [7, 0, 5], 1),
        ];
        for (light_si, [x, y, z], expected_count) in cases {
            terrain.light_dirty_sections.clear();
            let change = LightSectionChange {
                section_index: light_si,
                affected: LightBoundaryMask::for_cell(x, y, z),
            };
            assert_eq!(terrain.queue_light_changes(&store, -3, 7, &[change]), expected_count);
            let source_y = (light_si as i32 - 1) * 16 + y as i32;
            let mut expected = BTreeSet::new();
            for dx in -1..=1 {
                for dz in -1..=1 {
                    for target_si in 0..3 {
                        if (-2..=17).contains(&(x as i32 - dx * 16))
                            && (-2..=17).contains(&(z as i32 - dz * 16))
                            && (-2..=17).contains(&(source_y - target_si * 16))
                        {
                            expected.insert((-3 + dx, 7 + dz, -1 + target_si));
                        }
                    }
                }
            }
            assert_eq!(terrain.light_dirty_sections, expected, "light_si={light_si} cell={x},{y},{z}");
        }
        terrain.light_dirty_sections.clear();
        assert_eq!(terrain.queue_light_update(&store, -3, 7, &[2]), 27);
        assert_ne!(terrain.light_dirty_sections.len(), 1, "whole-section control must expose extra work");
        terrain.light_dirty_sections.clear();
        let change = LightSectionChange { section_index: 2, affected: LightBoundaryMask::for_cell(7, 9, 5) };
        terrain.dirty_columns.insert((-3, 7));
        assert_eq!(terrain.queue_light_changes(&store, -3, 7, &[change]), 0);
        assert_eq!(terrain.work_counters.light_patch_absorbed_sections, 1);
        terrain.dirty_columns.remove((-3, 7));
        assert_eq!(terrain.queue_light_changes(&store, -3, 7, &[change]), 1);
        assert_eq!(terrain.queue_light_changes(&store, -3, 7, &[change]), 0);
    }

    #[test]
    fn light_patch_spatial_filter_preserves_nearby_and_radius_two_reads() {
        use lodestone_world::{ColumnLight, Heightmaps, LightBoundaryMask, LightSectionChange, LoadedChunk};
        let mut world = World::new();
        for (cx, block) in [(0, [2, 2, 2]), (1, [0, 7, 8])] {
            let mut column = ChunkColumn::new(
                0, 1, PaletteKind::block_states(), PaletteKind::biomes(), id::AIR, 0,
            );
            column.set_block(block[0], block[1] as i32, block[2], id::STONE);
            world.load(ChunkPos::new(cx, 0), LoadedChunk::new(
                column, ColumnLight::new(1), Heightmaps::new(), Vec::new(),
            ));
        }
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let mut terrain = TerrainMesh::new(MeshScheduler::new(1, ShellClassifier::Demo(DemoClassifier)));
        let change = |x, y, z| LightSectionChange {
            section_index: 1, affected: LightBoundaryMask::for_cell(x, y, z),
        };
        assert_eq!(terrain.queue_light_changes(&store, 0, 0, &[change(12, 12, 12)]), 0);
        assert_eq!(terrain.work_counters.light_patch_spatial_reads, 125);
        assert_eq!(terrain.queue_light_changes(&store, 0, 0, &[change(2, 3, 2)]), 1);
        assert_eq!(terrain.light_dirty_sections, BTreeSet::from([(0, 0, 0)]));
        terrain.light_dirty_sections.clear();
        assert_eq!(terrain.queue_light_changes(&store, 0, 0, &[change(14, 7, 8)]), 1);
        assert_eq!(terrain.light_dirty_sections, BTreeSet::from([(1, 0, 0)]));
        terrain.light_dirty_sections.clear();
        assert_eq!(terrain.queue_light_update(&store, 0, 0, &[1]), 2);
        assert_ne!(terrain.light_dirty_sections.len(), 0, "whole-section control must rebuild");
    }

    #[test]
    fn local_light_admission_uses_spatial_bounds_without_restoring_legacy_destinations() {
        use lodestone_world::{ColumnLight, Heightmaps, LightBoundaryMask, LoadedChunk, Relit};
        let mut world = World::new();
        for (cx, block) in [(0, [2, 2, 2]), (1, [0, 7, 8])] {
            let mut column = ChunkColumn::new(
                0, 1, PaletteKind::block_states(), PaletteKind::biomes(), id::AIR, 0,
            );
            column.set_block(block[0], block[1] as i32, block[2], id::STONE);
            world.load(ChunkPos::new(cx, 0), LoadedChunk::new(
                column, ColumnLight::new(1), Heightmaps::new(), Vec::new(),
            ));
        }
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let mut terrain = TerrainMesh::new(MeshScheduler::new(1, ShellClassifier::Demo(DemoClassifier)));
        let relit = |x, y, z| Relit {
            light_changes: std::collections::BTreeMap::from([((0, 0, 1), LightBoundaryMask::for_cell(x, y, z))]),
            dirty_sections: BTreeSet::from([(0, 0, 0)]),
            ..Relit::default()
        };
        let far = relit(12, 12, 12);
        assert_eq!(terrain.queue_local_light_changes(&store, &far), 0);
        assert!(terrain.light_dirty_sections.is_empty());
        assert_eq!(terrain.work_counters.local_light_admission, LightAdmissionCounters {
            calls: 1, candidates: 1, spatial_rejected: 1, spatial_reads: 125,
            ..LightAdmissionCounters::default()
        });
        terrain.light_dirty_sections.extend(far.dirty_sections);
        assert!(!terrain.light_dirty_sections.is_empty(), "legacy insertion must defeat rejection");
        terrain.light_dirty_sections.clear();
        let near = relit(2, 3, 2);
        assert_eq!(terrain.queue_local_light_changes(&store, &near), 1);
        assert_eq!(terrain.queue_local_light_changes(&store, &near), 0);
        assert_eq!(terrain.work_counters.local_light_admission.coalesced, 1);
        terrain.light_dirty_sections.clear();
        assert_eq!(terrain.queue_local_light_changes(&store, &relit(14, 7, 8)), 1);
        assert_eq!(terrain.light_dirty_sections, BTreeSet::from([(1, 0, 0)]));
        terrain.light_dirty_sections.clear();
        terrain.dirty_columns.insert((0, 0));
        assert_eq!(terrain.queue_local_light_changes(&store, &near), 0);
        assert_eq!(terrain.work_counters.local_light_admission.absorbed, 1);
        assert_eq!(terrain.work_counters.light_patch_calls, 0);
    }

    /// A small two-section fixture for the readiness controls below. One block
    /// is enough to make section zero non-empty; section one remains a genuine
    /// all-air result rather than an inferred sky shortcut.
    fn readiness_column(non_air: bool) -> lodestone_world::LoadedChunk {
        use lodestone_world::{ColumnLight, Heightmaps, LoadedChunk};

        let mut column = ChunkColumn::new(
            0,
            2,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            id::AIR,
            0,
        );
        if non_air {
            column.set_block(0, 0, 0, id::STONE);
        }
        LoadedChunk::new(
            column,
            ColumnLight::new(2),
            Heightmaps::new(),
            Vec::new(),
        )
    }

    fn set_readiness_sky(world: &mut World, sky: u8) {
        for cx in -1..=1 {
            for cz in -1..=1 {
                let mut patch = lodestone_world::LightPatch::new();
                for si in 0..4 {
                    patch.set_sky(si, lodestone_world::LightData::Uniform(sky));
                    patch.set_block(si, lodestone_world::LightData::Uniform(0));
                }
                world.merge_light(ChunkPos::new(cx, cz), patch);
            }
        }
    }

    #[test]
    fn column_capture_absorbs_light_intent_and_later_patch_still_rebuilds() {
        let mut world = World::new();
        for cx in -1..=1 {
            for cz in -1..=1 {
                world.load(
                    ChunkPos::new(cx, cz),
                    readiness_column(cx == 0 && cz == 0),
                );
            }
        }
        set_readiness_sky(&mut world, 3);
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let extent = store.extent().unwrap();
        let mut terrain = streaming_terrain();
        terrain.mesh_column(&store, 0, 0);
        let initial = terrain.drain_all_meshes();
        assert_eq!(initial.len(), 1);
        terrain.mark_mesh_uploaded(initial[0].key);
        terrain.work_counters = MeshWorkCounters::default();
        assert_eq!(terrain.queue_light_update(&store, 0, 0, &[1]), 1);
        terrain.dirty_columns.insert((0, 0));

        let mut app = App::new();
        app.insert_resource(store.clone());
        app.insert_resource(write.clone());
        app.insert_resource(terrain);
        add_presentation_systems(app.world_mut());
        app.update();

        let terrain = app.world_mut().resource_mut::<TerrainMesh>().into_inner();
        assert_eq!(terrain.work_counters.column_snapshot_sections, 2);
        assert_eq!(terrain.work_counters.column_absorbed_light_sections, 1);
        assert_eq!(terrain.work_counters.light_section_snapshots, 0);
        assert_eq!(terrain.scheduler.pending(), 1);
        assert!(terrain.resident_column_presented(extent, 0, 0));
        assert!(!terrain.resident_column_mesh_settled(extent, 0, 0));
        let captured = terrain.drain_all_meshes();
        assert_eq!(captured.len(), 1);
        let SectionGeometry::Packed(mesh) = &captured[0].mesh else {
            panic!("the demo fixture must use packed geometry");
        };
        // Uniform light expands from 0..=15 to byte brightness: 255 / 15 = 17.
        assert_eq!(max_vertex_sky(mesh), 3 * 17);
        terrain.mark_mesh_uploaded(captured[0].key);

        set_readiness_sky(&mut write.write(), 11);
        assert_eq!(terrain.queue_light_update(&store, 0, 0, &[1]), 1);
        app.update();

        let terrain = app.world_mut().resource_mut::<TerrainMesh>().into_inner();
        assert_eq!(terrain.work_counters.column_snapshot_sections, 2);
        assert_eq!(terrain.work_counters.column_absorbed_light_sections, 1);
        assert_eq!(terrain.work_counters.light_section_snapshots, 1);
        assert_eq!(terrain.scheduler.pending(), 1);
        let corrected = terrain.drain_all_meshes();
        assert_eq!(corrected.len(), 1);
        let SectionGeometry::Packed(mesh) = &corrected[0].mesh else {
            panic!("the demo fixture must use packed geometry");
        };
        assert_eq!(max_vertex_sky(mesh), 11 * 17);
        terrain.mark_mesh_uploaded(corrected[0].key);
        assert!(terrain.resident_column_mesh_settled(extent, 0, 0));
    }

    #[test]
    fn browser_late_capture_consumes_current_light_and_preserves_later_corrections() {
        use browser_queue::{BrowserMeshBacklog, BrowserMeshRequest, CaptureSource, SectionIntent};
        use crate::platform::Instant;
        use lodestone_world::{ColumnLight, Heightmaps, LightData, LoadedChunk};

        let key = SectionKey { cx: 2, cz: -3, si: 1, min_y: -64 };
        let dirty = (2, -3, -3);
        let intent = SectionIntent {
            key, section_count: 3, force: false, source: CaptureSource::Section,
            priority: MeshPriority::Edit,
        };
        let mut world = World::new();
        for cx in 1..=3 {
            for cz in -4..=-2 {
                let mut column = ChunkColumn::new(
                    -64, 3, PaletteKind::block_states(), PaletteKind::biomes(), id::AIR, 0,
                );
                if (cx, cz) == (2, -3) {
                    column.set_block(8, -40, 8, id::STONE);
                }
                let mut light = ColumnLight::new(3);
                for si in 0..5 {
                    *light.sky_mut(si) = LightData::Uniform(1);
                    *light.block_mut(si) = LightData::Uniform(0);
                }
                world.load(ChunkPos::new(cx, cz), LoadedChunk::new(
                    column, light, Heightmaps::new(), Vec::new(),
                ));
            }
        }
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let mut terrain = TerrainMesh::new(MeshScheduler::new(
            1, ShellClassifier::Demo(DemoClassifier),
        ));
        let mut backlog = BrowserMeshBacklog::default();
        backlog.submit_intent(intent);
        let mut patch = lodestone_world::LightPatch::new();
        patch.set_sky(2, LightData::Uniform(3));
        let changed = write.write().merge_light_changed(ChunkPos::new(2, -3), patch);
        assert_eq!(changed, vec![2]);
        assert_eq!(terrain.queue_light_update(&store, 2, -3, &changed), 1);
        assert_eq!(terrain.light_dirty_sections, BTreeSet::from([dirty]));

        let capture = |request: BrowserMeshRequest| {
            request.capture(&store.read(), SkyDefault::Full, ColumnSource::Complete, Arc::from([]))
        };
        let (outcome, force, source) = capture(backlog.queue.pop_front(Instant::now()).unwrap());
        let current = terrain.accept_browser_capture(key, outcome, force, source).unwrap();
        assert_eq!(max_vertex_sky(&mesh_snapshot(&current, &DemoClassifier)), 51);
        assert!(terrain.light_dirty_sections.is_empty());
        assert_eq!(backlog.stats().pops, 1);

        let mut app = App::new();
        app.insert_resource(store.clone());
        app.insert_resource(terrain);
        add_presentation_systems(app.world_mut());
        app.update();
        let terrain = app.world_mut().resource_mut::<TerrainMesh>().into_inner();
        assert_eq!(terrain.scheduler.pending(), 0, "no duplicate light rebuild after capture");

        let mut patch = lodestone_world::LightPatch::new();
        patch.set_sky(2, LightData::Uniform(11));
        let changed = write.write().merge_light_changed(ChunkPos::new(2, -3), patch);
        assert_eq!(changed, vec![2]);
        assert_eq!(terrain.queue_light_update(&store, 2, -3, &changed), 1);
        assert!(terrain.light_dirty_sections.contains(&dirty));

        let (outcome, force, source) = capture(BrowserMeshRequest::Snapshot(current));
        let stale = terrain.accept_browser_capture(key, outcome, force, source).unwrap();
        assert_eq!(max_vertex_sky(&mesh_snapshot(&stale, &DemoClassifier)), 51);
        assert!(terrain.light_dirty_sections.contains(&dirty), "owned old light is no correction");

        let (outcome, force, source) = capture(BrowserMeshRequest::Capture(intent));
        let corrected = terrain.accept_browser_capture(key, outcome, force, source).unwrap();
        assert_eq!(max_vertex_sky(&mesh_snapshot(&corrected, &DemoClassifier)), 187);
        assert!(terrain.light_dirty_sections.is_empty());

        write.write().unload(ChunkPos::new(3, -3));
        terrain.light_dirty_sections.insert(dirty);
        for force in [false, true] {
            let (outcome, force, source) = BrowserMeshRequest::Capture(SectionIntent { force, ..intent })
                .capture(&store.read(), SkyDefault::Full, ColumnSource::Streaming, Arc::from([]));
            assert!(matches!(&outcome, SnapshotOutcome::Deferred(_)));
            let accepted = terrain.accept_browser_capture(key, outcome, force, source);
            assert_eq!(accepted.is_some(), force);
            assert!(terrain.light_dirty_sections.contains(&dirty), "deferred capture keeps correction");
        }
        write.write().unload(ChunkPos::new(2, -3));
        let (outcome, force, source) = capture(BrowserMeshRequest::Capture(intent));
        assert!(terrain.accept_browser_capture(key, outcome, force, source).is_none());
        assert!(terrain.light_dirty_sections.contains(&dirty), "dropped capture keeps correction");
    }

    #[test]
    fn browser_column_admission_settles_empty_sections_and_preserves_edits_and_removals() {
        use crate::platform::Instant;
        use browser_queue::{BrowserMeshBacklog, CaptureSource, SectionIntent};
        use lodestone_world::{ColumnLight, Heightmaps, LightData, LoadedChunk};

        let key = SectionKey { cx: 2, cz: -3, si: 5, min_y: -64 };
        let mut column = ChunkColumn::new(
            -64, 24, PaletteKind::block_states(), PaletteKind::biomes(), id::AIR, 0,
        );
        column.set_block(8, 24, 8, id::STONE);
        let mut world = World::new();
        world.load(ChunkPos::new(2, -3), LoadedChunk::new(
            column, ColumnLight::new(24), Heightmaps::new(), Vec::new(),
        ));
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let extent = store.extent().unwrap();
        let mut terrain = TerrainMesh::new(MeshScheduler::new(
            1, ShellClassifier::Demo(DemoClassifier),
        ));
        let intents = {
            let world = store.read();
            terrain.prepare_browser_column(
                &world.get(ChunkPos::new(2, -3)).unwrap().column, 2, -3, extent, false,
            )
        };
        let mut old_control = BrowserMeshBacklog::default();
        for si in 0..24 {
            old_control.submit_intent(SectionIntent {
                key: SectionKey { si, ..key }, section_count: 24, force: false,
                source: CaptureSource::Column,
                priority: MeshPriority::Background,
            });
        }
        assert_eq!(old_control.pending(), 24);
        let mut backlog = BrowserMeshBacklog::default();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].key, key);
        assert_eq!(intents[0].source, CaptureSource::Column);
        for intent in intents {
            backlog.submit_intent(intent);
        }
        assert_eq!(backlog.pending(), 1);
        assert_eq!((0..24).filter(|&si| {
            terrain.empty_sections.contains(&SectionKey { si, ..key })
        }).count(), 23);
        assert!(!terrain.column_mesh_settled(&store, 2, -3));
        let (outcome, force, source) = backlog.queue.pop_front(Instant::now()).unwrap().capture(
            &store.read(), SkyDefault::Full, ColumnSource::Complete, Arc::from([]),
        );
        assert_eq!(source, Some(CaptureSource::Column));
        let captured = terrain.accept_browser_capture(key, outcome, force, source).unwrap();
        assert_eq!(mesh_snapshot(&captured, &DemoClassifier).quad_count(), 6);
        assert_eq!(backlog.stats().insertions, 1);
        assert_eq!(backlog.stats().pops, 1);
        terrain.uploaded_sections.insert(key);
        terrain.mark_mesh_uploaded(key);
        assert!(terrain.column_mesh_settled(&store, 2, -3));

        let edited = SectionKey { si: 17, ..key };
        write.write().set_block(40, 216, -40, id::STONE);
        terrain.remesh_around(&store, [40, 216, -40]);
        let meshes = terrain.drain_all_meshes();
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].key, edited);
        assert_eq!(meshes[0].mesh.quad_count(), 6);
        terrain.mark_mesh_uploaded(edited);
        assert!(!terrain.empty_sections.contains(&edited));

        let mut patch = lodestone_world::LightPatch::new();
        patch.set_sky(18, LightData::Uniform(7));
        let changed = write.write().merge_light_changed(ChunkPos::new(2, -3), patch);
        assert_eq!(changed, vec![18]);
        assert_eq!(terrain.queue_light_update(&store, 2, -3, &changed), 1);
        let mut app = App::new();
        app.insert_resource(store.clone());
        app.insert_resource(terrain);
        add_presentation_systems(app.world_mut());
        app.update();
        let terrain = app.world_mut().resource_mut::<TerrainMesh>().into_inner();
        let relit = terrain.drain_all_meshes();
        assert_eq!(relit.len(), 1);
        assert_eq!(relit[0].key, edited);
        let SectionGeometry::Packed(mesh) = &relit[0].mesh else {
            panic!("the demo fixture must use packed geometry");
        };
        assert_eq!(max_vertex_sky(mesh), 119);
        terrain.mark_mesh_uploaded(edited);

        write.write().set_block(40, 24, -40, id::AIR);
        let intents = {
            let world = store.read();
            terrain.prepare_browser_column(
                &world.get(ChunkPos::new(2, -3)).unwrap().column, 2, -3, extent, false,
            )
        };
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].key, edited);
        assert!(terrain.empty_sections.contains(&key));
        assert!(!terrain.presented_sections.contains(&key));
        assert_eq!(terrain.drain_removals(), vec![key]);
    }

    #[test]
    fn deferred_column_capture_keeps_the_arrival_retry_and_uploaded_rebuild() {
        for previously_uploaded in [false, true] {
            let mut world = World::new();
            for cx in -1..=1 {
                for cz in -1..=1 {
                    if (cx, cz) != (1, 0) {
                        world.load(
                            ChunkPos::new(cx, cz),
                            readiness_column(cx == 0 && cz == 0),
                        );
                    }
                }
            }
            let write = ChunkWorldWrite::new(world);
            let store = write.read_handle();
            let mut terrain = streaming_terrain();
            if previously_uploaded {
                terrain.mesh_column_inner(&store, 0, 0, true);
                let initial = terrain.drain_all_meshes();
                assert_eq!(initial.len(), 1);
                terrain.mark_mesh_uploaded(initial[0].key);
            }
            terrain.work_counters = MeshWorkCounters::default();
            terrain.light_dirty_sections.insert((0, 0, 0));
            terrain.mesh_column(&store, 0, 0);

            assert_eq!(terrain.work_counters.column_absorbed_light_sections, 1);
            assert!(terrain.light_dirty_sections.is_empty());
            let deferred = terrain.drain_all_meshes();
            assert_eq!(deferred.len(), usize::from(previously_uploaded));
            assert!(!terrain.column_mesh_settled(&store, 0, 0));

            write.write().load(ChunkPos::new(1, 0), readiness_column(false));
            terrain.queue_column_arrival(1, 0);
            terrain.mark_neighbours_dirty(&store, 1, 0);
            assert!(terrain.dirty_columns.remove((0, 0)));
            terrain.mesh_arriving_column(&store, 0, 0, false);
            let completed = terrain.drain_all_meshes();
            assert_eq!(completed.len(), 1);
            terrain.mark_mesh_uploaded(completed[0].key);
            assert!(terrain.column_mesh_settled(&store, 0, 0));
        }
    }

    /// **Readiness control: absent neighbours with no queued jobs still hold.**
    /// The centre section has geometry, but its streaming snapshot is deferred
    /// because the eight horizontal neighbours are absent. The scheduler is
    /// idle after that decision; a global pending count of zero is therefore a
    /// deliberately wrong release signal.
    #[test]
    fn a_non_air_center_with_absent_neighbours_is_not_mesh_settled() {
        let mut world = World::new();
        world.load(ChunkPos::new(0, 0), readiness_column(true));
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let mut terrain = streaming_terrain();

        terrain.mesh_column(&store, 0, 0);

        assert_eq!(terrain.scheduler.pending(), 0, "control: no mesh job remains queued");
        assert!(
            !terrain.column_mesh_settled(&store, 0, 0),
            "a deferred non-air centre keeps the player's column loading even with zero jobs"
        );
    }

    /// A completed CPU result is still not enough. This positive control loads
    /// the complete horizontal neighbourhood, drains the one non-empty mesh,
    /// and checks that only the renderer-handoff acknowledgement settles it.
    #[test]
    fn a_non_air_center_settles_only_after_renderer_handoff() {
        let mut world = World::new();
        for cx in -1..=1 {
            for cz in -1..=1 {
                world.load(
                    ChunkPos::new(cx, cz),
                    readiness_column(cx == 0 && cz == 0),
                );
            }
        }
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let mut terrain = streaming_terrain();

        terrain.mesh_column(&store, 0, 0);
        let meshes = terrain.drain_all_meshes();
        assert_eq!(meshes.len(), 1, "the centre's one non-air section is meshed");
        assert!(
            !terrain.column_mesh_settled(&store, 0, 0),
            "a CPU result has not crossed the renderer boundary yet"
        );
        for meshed in meshes {
            terrain.mark_mesh_uploaded(meshed.key);
        }
        assert!(terrain.column_mesh_settled(&store, 0, 0));
    }

    #[test]
    fn presented_column_remains_visible_during_remesh_but_not_after_redecode() {
        let mut world = World::new();
        for cx in -1..=1 {
            for cz in -1..=1 {
                world.load(
                    ChunkPos::new(cx, cz),
                    readiness_column(cx == 0 && cz == 0),
                );
            }
        }
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let extent = store.extent().expect("readiness fixture has an extent");
        let mut terrain = streaming_terrain();

        terrain.mesh_column(&store, 0, 0);
        let meshes = terrain.drain_all_meshes();
        assert!(!terrain.resident_column_presented(extent, 0, 0));
        for mesh in meshes {
            terrain.mark_mesh_uploaded(mesh.key);
        }
        assert!(terrain.resident_column_presented(extent, 0, 0));

        terrain.mesh_column(&store, 0, 0);
        assert!(!terrain.resident_column_mesh_settled(extent, 0, 0));
        assert!(terrain.resident_column_presented(extent, 0, 0));

        terrain.queue_column_arrival(0, 0);
        assert!(!terrain.resident_column_presented(extent, 0, 0));
    }

    /// **Readiness control: all-air is a real settled result.** Every section
    /// enters the explicit `SnapshotOutcome::Empty` ledger, so no renderer
    /// upload is required and the column can settle without a fabricated mesh.
    #[test]
    fn an_all_air_center_settles_empty() {
        let mut world = World::new();
        world.load(ChunkPos::new(0, 0), readiness_column(false));
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let mut terrain = streaming_terrain();

        terrain.mesh_column(&store, 0, 0);

        assert_eq!(terrain.scheduler.pending(), 0);
        assert!(
            terrain.column_mesh_settled(&store, 0, 0),
            "explicit empty outcomes settle every all-air section"
        );
    }

    /// **Readiness control: another column cannot satisfy this one.** Populate
    /// the renderer-handoff ledger for an unrelated column while the player's
    /// non-air centre remains deferred. The local predicate is false despite a
    /// complete unrelated upload set.
    #[test]
    fn an_unrelated_uploaded_column_cannot_release_the_center_gate() {
        let mut world = World::new();
        world.load(ChunkPos::new(0, 0), readiness_column(true));
        world.load(ChunkPos::new(4, 4), readiness_column(false));
        let write = ChunkWorldWrite::new(world);
        let store = write.read_handle();
        let extent = store.extent().expect("readiness fixture has an extent");
        let mut terrain = streaming_terrain();

        terrain.mesh_column(&store, 0, 0);
        for si in 0..extent.section_count {
            terrain.mark_mesh_uploaded(SectionKey {
                cx: 4,
                cz: 4,
                si,
                min_y: extent.min_y,
            });
        }

        assert!(terrain.column_mesh_settled(&store, 4, 4));
        assert!(
            !terrain.column_mesh_settled(&store, 0, 0),
            "an unrelated uploaded column must not release the centre gate"
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

    #[cfg(not(target_arch = "wasm32"))]
    mod native_priority {
        use super::*;

        struct WorkerRelease(Option<crossbeam_channel::Sender<()>>);

        impl WorkerRelease {
            fn release(mut self) {
                self.0.take().unwrap().send(()).unwrap();
            }
        }

        impl Drop for WorkerRelease {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.try_send(());
                }
            }
        }

        fn held_scheduler(ignore_cancellation: bool) -> (MeshScheduler, WorkerRelease) {
            let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
            let (release_tx, release_rx) = crossbeam_channel::bounded(1);
            let scheduler = MeshScheduler::new_inner(
                1,
                ShellClassifier::Demo(DemoClassifier),
                Some(NativeWorkerGate {
                    entered: entered_tx,
                    release: release_rx,
                    ignore_cancellation,
                }),
            );
            let release = WorkerRelease(Some(release_tx));
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            (scheduler, release)
        }

        fn cubes(cx: i32, count: usize) -> SectionSnapshot {
            let mut column = ChunkColumn::new(
                0, 1, PaletteKind::block_states(), PaletteKind::biomes(), id::AIR, 0,
            );
            for x in [2, 6, 10].into_iter().take(count) {
                column.set_block(x, 5, 8, id::STONE);
            }
            let mut world = World::new();
            world.load(ChunkPos::new(cx, 0), lodestone_world::LoadedChunk::new(
                column,
                lodestone_world::ColumnLight::new(1),
                lodestone_world::Heightmaps::new(),
                Vec::new(),
            ));
            snapshot_section(&world, SectionKey { cx, cz: 0, si: 0, min_y: 0 }).unwrap()
        }

        #[test]
        fn held_native_priority_computes_edits_first_and_serves_background() {
            for edit_priority in [MeshPriority::Background, MeshPriority::Edit] {
                let (mut scheduler, release) = held_scheduler(false);
                for cx in [0, 1] {
                    scheduler.submit(cubes(cx, 1));
                }
                for cx in 7..=11 {
                    scheduler.submit_with_priority(cubes(cx, 1), edit_priority);
                }
                release.release();
                assert_eq!(scheduler.drain_blocking(7).len(), 7);
                let computed: Vec<_> = scheduler.worker_counters.started_keys.lock().unwrap()
                    .iter().map(|key| key.cx).collect();
                let expected = if edit_priority == MeshPriority::Edit {
                    [7, 8, 9, 10, 0, 11, 1]
                } else {
                    [0, 1, 7, 8, 9, 10, 11]
                };
                assert_eq!(computed, expected);
                assert_eq!(computed == [7, 8, 9, 10, 0, 11, 1], edit_priority == MeshPriority::Edit,
                    "FIFO negative control must fail the edit-order detector");
                assert_eq!(scheduler.pending(), 0);
            }
        }

        #[test]
        fn native_priority_survives_background_replacement_and_expires_at_settlement() {
            for ignore_cancellation in [true, false] {
                let (mut scheduler, release) = held_scheduler(ignore_cancellation);
                scheduler.native_timing = NativeMeshTiming::new(true);
                scheduler.submit(cubes(9, 1));
                let edited = cubes(0, 1);
                let key = edited.key;
                scheduler.submit_current_with_priority(edited, MeshPriority::Edit);
                let old_token = Arc::clone(&scheduler.latest_generation[&key].cancelled);
                scheduler.submit_current(cubes(0, 3));
                assert!(old_token.load(Ordering::Acquire));
                assert_eq!(scheduler.latest_generation[&key].pending_priority, Some(MeshPriority::Edit));
                assert_eq!(scheduler.pending(), 3);
                release.release();
                let results = scheduler.drain_blocking(3);
                assert_eq!(results.iter().map(|mesh| mesh.key.cx).collect::<Vec<_>>(), [0, 9]);
                assert_eq!(results[0].mesh.quad_count(), 18);
                assert_eq!(results[1].mesh.quad_count(), 6);
                assert_eq!(scheduler.pending(), 0);
                assert_eq!(scheduler.native_work_counters(), NativeMeshWorkCounters {
                    submitted: 3,
                    started: if ignore_cancellation { 3 } else { 2 },
                    skipped_before_mesh: if ignore_cancellation { 0 } else { 1 },
                    stale_results_discarded: if ignore_cancellation { 1 } else { 0 },
                });
                let timing = scheduler.native_timing.snapshot();
                assert_eq!(timing.by_priority[0].built, 1);
                let edits = timing.by_priority[1];
                assert_eq!(edits.submit_to_receive.calls, 2);
                assert_eq!(edits.mesh_compute.calls, if ignore_cancellation { 2 } else { 1 });
                assert_eq!(edits.stale, u64::from(ignore_cancellation));
                assert_eq!(edits.skipped, u64::from(!ignore_cancellation));
                assert_eq!(edits.completed_to_upload.calls, 0);
                assert_eq!(edits.invalid_timestamps, 0);
                assert_eq!(scheduler.latest_generation[&key].pending_priority, None);
                scheduler.submit_current(cubes(0, 2));
                assert_eq!(scheduler.latest_generation[&key].pending_priority, Some(MeshPriority::Background));
                assert_eq!(scheduler.drain_blocking(1)[0].mesh.quad_count(), 12);
                assert_eq!(scheduler.latest_generation[&key].pending_priority, None);
            }
        }

        fn completion(scheduler: &mut MeshScheduler, cx: i32, priority: MeshPriority) -> NativeMeshCompletion {
            let key = SectionKey { cx, cz: 0, si: 0, min_y: 0 };
            let generation = cx as u64 + 1;
            scheduler.latest_generation.insert(key, NativeGeneration::with_priority(generation, priority));
            scheduler.pending += 1;
            NativeMeshCompletion::Built(Meshed::new(key, SectionGeometry::Packed(Mesh::default())), generation)
        }

        #[test]
        fn native_priority_handoff_bypasses_retained_background_and_bounds_edit_bursts() {
            for edit_priority in [MeshPriority::Background, MeshPriority::Edit] {
                let mut scheduler = MeshScheduler::new(1, ShellClassifier::Demo(DemoClassifier));
                let [(background_tx, background_rx), (edit_tx, edit_rx)] =
                    std::array::from_fn(|_| crossbeam_channel::unbounded());
                scheduler.result_rx = [background_rx, edit_rx];
                let NativeMeshCompletion::Built(mesh, generation) = completion(&mut scheduler, 0, MeshPriority::Background)
                    else { unreachable!() };
                scheduler.ready[0].push_back((mesh, generation));
                background_tx.send(completion(&mut scheduler, 1, MeshPriority::Background)).unwrap();
                for cx in 7..=11 {
                    let sender = if edit_priority == MeshPriority::Edit { &edit_tx } else { &background_tx };
                    sender.send(completion(&mut scheduler, cx, edit_priority)).unwrap();
                }
                let mut actual = Vec::new();
                for remaining in (0..7).rev() {
                    let results = scheduler.drain_frame_with_limit(1);
                    assert_eq!(results.len(), 1);
                    actual.push(results[0].key.cx);
                    assert_eq!(scheduler.pending(), remaining);
                    assert_eq!(scheduler.latest_generation[&results[0].key].pending_priority, None);
                }
                let expected = if edit_priority == MeshPriority::Edit {
                    [7, 8, 9, 10, 0, 11, 1]
                } else {
                    [0, 1, 7, 8, 9, 10, 11]
                };
                assert_eq!(actual, expected);
                assert_eq!(actual == [7, 8, 9, 10, 0, 11, 1], edit_priority == MeshPriority::Edit,
                    "FIFO negative control must fail the handoff-order detector");
            }
        }

        #[test]
        fn native_priority_skips_stale_work_within_examination_budget() {
            let mut scheduler = MeshScheduler::new(1, ShellClassifier::Demo(DemoClassifier));
            let (sender, receiver) = crossbeam_channel::unbounded();
            scheduler.result_rx[MeshPriority::Edit.index()] = receiver;
            scheduler.pending += 1;
            sender.send(NativeMeshCompletion::Skipped(None)).unwrap();
            let current = completion(&mut scheduler, 7, MeshPriority::Edit);
            sender.send(current).unwrap();
            assert!(scheduler.drain_frame_with_limit(1).is_empty());
            assert_eq!(scheduler.pending(), 1);
            assert_eq!(scheduler.drain_frame_with_limit(1)[0].key.cx, 7);
            assert_eq!(scheduler.pending(), 0);
        }

        #[test]
        fn native_priority_retained_edit_replacement_keeps_pending_accounting() {
            let (mut scheduler, release) = held_scheduler(false);
            let NativeMeshCompletion::Built(mesh, generation) = completion(&mut scheduler, 7, MeshPriority::Edit)
                else { unreachable!() };
            scheduler.ready[MeshPriority::Edit.index()].push_back((mesh, generation));
            scheduler.submit_current(cubes(7, 2));
            let key = SectionKey { cx: 7, cz: 0, si: 0, min_y: 0 };
            assert_eq!(scheduler.pending(), 1);
            assert_eq!(scheduler.latest_generation[&key].pending_priority, Some(MeshPriority::Edit));
            assert!(scheduler.ready[MeshPriority::Edit.index()].is_empty());
            assert_eq!(scheduler.native_work_counters().stale_results_discarded, 1);
            release.release();
            assert_eq!(scheduler.drain_blocking(1)[0].mesh.quad_count(), 12);
            assert_eq!(scheduler.pending(), 0);
        }
    }

    #[test]
    fn frame_mesh_handoff_keeps_byte_budget_overflow_for_later_frames() {
        for priority in [MeshPriority::Background, MeshPriority::Edit] {
            let mut scheduler = MeshScheduler::new(1, ShellClassifier::Demo(DemoClassifier));
            let vertex_count = (MESH_HANDOFF_BYTE_BUDGET / 2)
                / std::mem::size_of::<lodestone_render::PackedVertex>();
            let vertices = vec![lodestone_render::PackedVertex { words: [0; 3] }; vertex_count];
            for index in 0..3 {
                let key = SectionKey {
                    cx: index,
                    cz: 0,
                    si: 0,
                    min_y: 0,
                };
                let generation = index as u64 + 1;
                scheduler.latest_generation.insert(key, NativeGeneration::with_priority(generation, priority));
                scheduler.pending += 1;
                scheduler.ready[priority.index()].push_back((
                    Meshed::new(
                        key,
                        SectionGeometry::Packed(Mesh {
                            vertices: vertices.clone(),
                            indices: Vec::new(),
                        }),
                    ),
                    generation,
                ));
            }

            let first_frame = scheduler.drain_frame_with_limit(MESH_HANDOFF_MAX_COUNT);
            assert_eq!(first_frame.len(), 2, "two sections fit within the byte budget");
            assert_eq!(scheduler.pending(), 1, "the overflow stays pending");
            let retained = SectionKey { cx: 2, cz: 0, si: 0, min_y: 0 };
            assert_eq!(scheduler.latest_generation[&retained].pending_priority, Some(priority));
            let next_frame = scheduler.drain_frame_with_limit(MESH_HANDOFF_MAX_COUNT);
            assert_eq!(next_frame.len(), 1, "the retained section progresses next frame");
            assert_eq!(next_frame[0].key.cx, 2, "FIFO handoff does not lose the tail");
            assert_eq!(scheduler.pending(), 0);
            assert_eq!(scheduler.latest_generation[&retained].pending_priority, None);
        }
    }

    #[test]
    fn frame_mesh_handoff_caps_result_count_and_retains_the_tail() {
        let mut scheduler = MeshScheduler::new(1, ShellClassifier::Demo(DemoClassifier));
        const TEST_LIMIT: usize = 8;
        for index in 0..(TEST_LIMIT + 2) {
            let key = SectionKey {
                cx: index as i32,
                cz: 0,
                si: 0,
                min_y: 0,
            };
            let generation = index as u64 + 1;
            scheduler.latest_generation.insert(key, NativeGeneration::new(generation));
            scheduler.pending += 1;
            scheduler.ready[MeshPriority::Background.index()].push_back((
                Meshed::new(key, SectionGeometry::Packed(Mesh::default())),
                generation,
            ));
        }

        assert_eq!(scheduler.drain_frame_with_limit(TEST_LIMIT).len(), TEST_LIMIT);
        assert_eq!(scheduler.pending(), 2);
        let tail = scheduler.drain_frame_with_limit(TEST_LIMIT);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[1].key.cx, (TEST_LIMIT + 1) as i32);
        assert_eq!(scheduler.pending(), 0);
    }

    #[test]
    fn frame_mesh_handoff_adapts_count_to_observed_upload_cost() {
        assert_eq!(mesh_handoff_count_budget(50_000), 40);
        assert_eq!(mesh_handoff_count_budget(1_000), MESH_HANDOFF_MAX_COUNT);
        assert_eq!(mesh_handoff_count_budget(1_000_000), 2);
        assert_eq!(mesh_handoff_count_budget(2_000_000), 1);
    }

    #[test]
    fn forgetting_a_retained_result_adjusts_pending_count() {
        let mut scheduler = MeshScheduler::new(1, ShellClassifier::Demo(DemoClassifier));
        let keys = [0, 1].map(|cx| SectionKey {
            cx,
            cz: 0,
            si: 0,
            min_y: 0,
        });
        for (index, key) in keys.iter().copied().enumerate() {
            let generation = index as u64 + 1;
            scheduler.latest_generation.insert(key, NativeGeneration::new(generation));
            scheduler.pending += 1;
            scheduler.ready[MeshPriority::Background.index()].push_back((
                Meshed::new(key, SectionGeometry::Packed(Mesh::default())),
                generation,
            ));
        }

        scheduler.forget_generation(&keys[0]);
        assert_eq!(scheduler.pending(), 1);
        let remaining = scheduler.drain_frame_with_limit(2);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].key, keys[1]);
        assert_eq!(scheduler.pending(), 0);
    }

    #[test]
    fn resetting_column_readiness_preserves_unrelated_column_results() {
        let mut terrain = TerrainMesh::new(MeshScheduler::new(
            1,
            ShellClassifier::Demo(DemoClassifier),
        ));
        let key = SectionKey { cx: 2, cz: -3, si: 0, min_y: -64 };
        let other_height = SectionKey { min_y: 0, ..key };
        let empty = SectionKey { si: 1, ..key };
        let unrelated = SectionKey { cx: 3, ..key };
        let unrelated_empty = SectionKey { si: 1, ..unrelated };
        for rendered in [key, other_height, unrelated] {
            terrain.mark_mesh_uploaded(rendered);
        }
        terrain.empty_sections.insert(empty);
        terrain.empty_sections.insert(unrelated_empty);
        terrain.built_columns.insert((empty.cx, empty.cz));
        terrain.provisional_columns.insert((key.cx, key.cz));
        terrain.provisional_columns.insert((unrelated.cx, unrelated.cz));

        terrain.reset_column_readiness(key.cx, key.cz);
        terrain.reset_column_readiness(key.cx, key.cz);

        for removed in [key, other_height] {
            assert!(!terrain.rendered_sections.contains(&removed));
            assert!(!terrain.presented_sections.contains(&removed));
        }
        assert!(!terrain.empty_sections.contains(&empty));
        assert!(!terrain.built_columns.contains(&(key.cx, key.cz)));
        assert!(!terrain.provisional_columns.contains(&(key.cx, key.cz)));
        assert!(terrain.rendered_sections.contains(&unrelated));
        assert!(terrain.presented_sections.contains(&unrelated));
        assert!(terrain.empty_sections.contains(&unrelated_empty));
        assert!(terrain.built_columns.contains(&(unrelated.cx, unrelated.cz)));
        assert!(terrain.provisional_columns.contains(&(unrelated.cx, unrelated.cz)));
        assert!(terrain.pending_removals.is_empty());
    }

    #[test]
    fn forgetting_a_column_discards_never_uploaded_sections_too() {
        let mut terrain = TerrainMesh::new(MeshScheduler::new(
            1,
            ShellClassifier::Demo(DemoClassifier),
        ));
        let keys = [
            SectionKey {
                cx: 2,
                cz: -3,
                si: 0,
                min_y: 0,
            },
            SectionKey {
                cx: 2,
                cz: -3,
                si: 1,
                min_y: 0,
            },
            SectionKey {
                cx: 3,
                cz: -3,
                si: 0,
                min_y: 0,
            },
        ];
        for (index, key) in keys.iter().copied().enumerate() {
            let generation = index as u64 + 1;
            terrain.scheduler.latest_generation.insert(key, NativeGeneration::new(generation));
            terrain.scheduler.pending += 1;
            terrain.scheduler.ready[MeshPriority::Background.index()].push_back((
                Meshed::new(key, SectionGeometry::Packed(Mesh::default())),
                generation,
            ));
        }
        terrain.uploaded_sections.insert(keys[0]);
        terrain.mark_mesh_uploaded(keys[0]);
        terrain.mark_mesh_uploaded(keys[2]);
        terrain.empty_sections.insert(keys[1]);
        let unrelated_empty = SectionKey { si: 1, ..keys[2] };
        terrain.empty_sections.insert(unrelated_empty);

        terrain.forget_column(2, -3);
        terrain.forget_column(2, -3);

        assert!(!terrain.rendered_sections.contains(&keys[0]));
        assert!(!terrain.presented_sections.contains(&keys[0]));
        assert!(!terrain.empty_sections.contains(&keys[1]));
        assert!(terrain.rendered_sections.contains(&keys[2]));
        assert!(terrain.presented_sections.contains(&keys[2]));
        assert!(terrain.empty_sections.contains(&unrelated_empty));
        assert_eq!(terrain.drain_removals(), vec![keys[0]]);
        assert_eq!(terrain.scheduler.pending(), 1);
        let remaining = terrain.scheduler.drain_frame_with_limit(2);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].key, keys[2]);
    }

    #[test]
    fn empty_or_unuploaded_deferred_outcomes_invalidate_older_meshes() {
        let mut terrain = TerrainMesh::new(MeshScheduler::new(
            1,
            ShellClassifier::Demo(DemoClassifier),
        ));
        let snapshot = platform_snapshot(None);
        let key = snapshot.key;
        for outcome in [SnapshotOutcome::Empty, SnapshotOutcome::Deferred(snapshot)] {
            terrain.scheduler.latest_generation.insert(key, NativeGeneration::new(1));
            terrain.scheduler.pending += 1;
            terrain.scheduler.ready[MeshPriority::Background.index()].push_back((
                Meshed::new(key, SectionGeometry::Packed(Mesh::default())),
                1,
            ));
            assert_eq!(terrain.scheduler.pending(), 1);

            assert!(!terrain.route(key, outcome, false));
            assert_eq!(terrain.scheduler.pending(), 0);
            assert!(terrain.scheduler.drain_frame_with_limit(2).is_empty());
        }
    }

    /// **The bug 1 (grief-protection) reproduction.** Two jobs submitted for
    /// the *same* section — a client-predicted change, then a correction
    /// moments later — must never let the caller see the stale one,
    /// regardless of which order the pool's workers happen to finish them
    /// in. [`MeshScheduler`]'s own `latest_generation` field names the
    /// real-world trigger this guards: a predicted block break gets
    /// remeshed once immediately, then again when the server denies it and
    /// restores the original — and the pool's own doc already admits
    /// completion order is not submission order ("the channel distributes
    /// by actual work completion").
    ///
    /// This does not need to win a real thread race to make the point: a
    /// result superseded by a *later* `submit` for the same key is stale
    /// the moment that second `submit` happens, whether its own completion
    /// arrives before or after — so a single worker (strictly FIFO, no race
    /// needed at all) already exercises the generation check on its own.
    #[test]
    fn a_stale_completion_never_overwrites_a_fresher_one_for_the_same_section() {
        let classifier = ShellClassifier::Demo(DemoClassifier);
        assert_eq!(
            platform_snapshot(None).key,
            platform_snapshot(Some([12, 7, 12])).key,
            "fixture precondition: same section"
        );

        // Ground truth, meshed directly with no scheduler involved, so the
        // assertion below is a prediction rather than a tautology.
        let expected_stale = mesh_one(platform_snapshot(None), &classifier, true, BLEND_RADIUS)
            .mesh
            .quad_count();
        let expected_fresh =
            mesh_one(platform_snapshot(Some([12, 7, 12])), &classifier, true, BLEND_RADIUS)
            .mesh
            .quad_count();
        assert_ne!(
            expected_stale, expected_fresh,
            "fixture precondition: the two snapshots must mesh to different \
             geometry, or a version that always kept the *first* result would \
             pass this test too"
        );

        let mut scheduler = MeshScheduler::new(1, classifier);
        scheduler.submit(platform_snapshot(None));
        scheduler.submit(platform_snapshot(Some([12, 7, 12])));
        let results = scheduler.drain_blocking(2);

        assert_eq!(
            results.len(),
            1,
            "the stale completion must be dropped, not handed to the caller: \
             got {results:?}"
        );
        assert_eq!(
            results[0].mesh.quad_count(),
            expected_fresh,
            "the surviving mesh must be the later submission's geometry \
             ({expected_fresh} quads), not the superseded one ({expected_stale})"
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
                    sections.push(Neighbour::Present(Arc::new(sec)));
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
            biome_names: Arc::from([]),
            light_revision: None,
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

    // -----------------------------------------------------------------------
    // The model path's face-light rule, and the placement defect it caused
    // -----------------------------------------------------------------------

    /// A unit cube's six baked quads: one per direction, each culled by its own
    /// facing, positioned on that face of the block. Enough geometry to carry a
    /// light byte through [`mesh_models`] and be identified again by centroid.
    fn cube_quads() -> Vec<BakedQuad> {
        const DIRS: [Direction; 6] = [
            Direction::Down,
            Direction::Up,
            Direction::North,
            Direction::South,
            Direction::West,
            Direction::East,
        ];
        DIRS.iter()
            .map(|&d| {
                let n = face_of_direction(d).normal();
                // The face plane: the fixed axis sits at 0 or 1, the other two
                // sweep the unit square.
                let (axis, plane) = match d {
                    Direction::West => (0usize, 0.0f32),
                    Direction::East => (0, 1.0),
                    Direction::Down => (1, 0.0),
                    Direction::Up => (1, 1.0),
                    Direction::North => (2, 0.0),
                    Direction::South => (2, 1.0),
                };
                let (a, b) = match axis {
                    0 => (1usize, 2usize),
                    1 => (0, 2),
                    _ => (0, 1),
                };
                let corners = [[0.0f32, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
                let mut positions = [[0.0f32; 3]; 4];
                for (i, c) in corners.iter().enumerate() {
                    positions[i][axis] = plane;
                    positions[i][a] = c[0];
                    positions[i][b] = c[1];
                }
                let _ = n;
                BakedQuad {
                    positions,
                    uvs: [[0.0, 0.0]; 4],
                    direction: d,
                    cullface: Some(d),
                    tint_index: None,
                    shade: true,
                    shade_direction: None,
                    layer: 0,
                    anim: 0,
                    sprite: 0,
                }
            })
            .collect()
    }

    /// Which light rule a [`ProbeView`] applies — the shipped face rule, or the
    /// **pre-fix** own-cell rule kept verbatim as the negative control.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum LightRule {
        /// `SnapshotModelView`'s rule: the cell the face opens into.
        FaceNeighbour,
        /// The rule this test exists to retire: `(sky << 4) | block` read at the
        /// block's **own** cell, which is `0` inside every opaque block.
        OwnCell,
    }

    /// A [`ModelSectionView`] over a snapshot that emits a full cube for every
    /// non-air cell and resolves light through the real [`SnapshotLight`] — so
    /// the assertions below run the shipped resolver, not a copy of it.
    struct ProbeView<'a> {
        snapshot: &'a SectionSnapshot,
        light: SnapshotLight<'a>,
        quads: Vec<BakedQuad>,
        empty: Vec<BakedQuad>,
        rule: LightRule,
    }

    impl ModelSectionView for ProbeView<'_> {
        fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
            if self.snapshot.at(0, 0, 0).get_block(x, y, z) == id::AIR {
                &self.empty
            } else {
                &self.quads
            }
        }

        fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
            let (dx, lx) = split16(x);
            let (dy, ly) = split16(y);
            let (dz, lz) = split16(z);
            if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dy) || !(-1..=1).contains(&dz) {
                return false;
            }
            self.snapshot.at(dx, dy, dz).get_block(lx, ly, lz) != id::AIR
        }

        fn light_at(&self, x: usize, y: usize, z: usize) -> u8 {
            self.light.max_light(x, y, z)
        }

        fn face_light_at(&self, x: usize, y: usize, z: usize, dir: Direction) -> u8 {
            match self.rule {
                LightRule::FaceNeighbour => self
                    .light
                    .face_light(x, y, z, face_of_direction(dir).normal()),
                LightRule::OwnCell => {
                    let (sky, block) = self.light.levels_at(x as i32, y as i32, z as i32);
                    (sky << 4) | block
                }
            }
        }

        /// **Do not delete this, and do not let it drift from
        /// [`SnapshotModelView::corner_light_at`].**
        ///
        /// `mesh_models` grew per-vertex smooth lighting in `1b8e46b`, which added
        /// a *fourth* light hook to [`ModelSectionView`] —
        /// [`ModelSectionView::corner_light_at`], sampling the two edge-adjacent
        /// cells and the diagonal around each quad corner. Its trait default is
        /// **full-bright `0xF0`**, for views that model no neighbourhood at all
        /// (GUI items). The shipped view implements it; this probe did not, so
        /// every AO corner read `0xF0` and each measurement below came out as
        /// `round((centre + 15 + 15 + 15) / 4)` — 11 where 0 was expected, which is
        /// exactly the `176` these three tests reported for four commits.
        ///
        /// That is the **world** species of vacuous test from `CLAUDE.md`: the
        /// assertions were exemplary and the fixture had stopped containing the
        /// structure the code under test needs. A `ProbeView` that omits a light
        /// hook does not measure the shipped resolver, it measures a trait default.
        fn corner_light_at(&self, x: i32, y: i32, z: i32) -> u8 {
            let (sky, block) = self.light.levels_at(x, y, z);
            (sky << 4) | block
        }
    }

    /// The packed light byte carried by the quad of block `b` facing `dir`,
    /// located by its centroid in the emitted mesh. `None` when that face was
    /// culled (so "the face is missing" can never read as "the face is dark").
    fn quad_light(mesh: &ModelMesh, b: [usize; 3], dir: Direction) -> Option<u8> {
        let n = face_of_direction(dir).normal();
        let want = [
            b[0] as f32 + 0.5 + 0.5 * n[0] as f32,
            b[1] as f32 + 0.5 + 0.5 * n[1] as f32,
            b[2] as f32 + 0.5 + 0.5 * n[2] as f32,
        ];
        for quad in mesh.vertices.chunks_exact(4) {
            let mut c = [0.0f32; 3];
            for v in quad {
                for a in 0..3 {
                    c[a] += v.position[a] / 4.0;
                }
            }
            if (0..3).all(|a| (c[a] - want[a]).abs() < 1e-4) {
                let light = quad[0].light;
                assert!(
                    quad.iter().all(|v| v.light == light),
                    "a flat-lit quad must carry one light on all four vertices"
                );
                return Some(light);
            }
        }
        None
    }

    /// The fixture: a one-block-thick stone platform at `y = 6` with a dark cave
    /// beneath it, a stone roof over the `x < 8, z < 8` quadrant at `y = 12`,
    /// and open sky everywhere else.
    ///
    /// Three *different* light populations, which is what stops this gate being
    /// the "fully sunlit flat world" species of vacuous test:
    ///
    /// | region | sky | block |
    /// |---|---|---|
    /// | open air (`y >= 7`, outside the roofed quadrant) | 15 | 0 |
    /// | roofed pocket (`y 7..=11`, `x < 8 && z < 8`) | 0 | 11 |
    /// | cave under the platform (`y <= 5`) | 0 | 0 |
    /// | any solid cell | 0 | 0 |
    ///
    /// `placed` optionally turns one air cell to stone **without touching the
    /// light field** — precisely the optimistic-placement window, where the
    /// block is known and the server's relight has not arrived.
    fn platform_snapshot(placed: Option<[usize; 3]>) -> SectionSnapshot {
        use lodestone_world::{LightData, NibbleArray};

        let roofed = |x: usize, z: usize| x < 8 && z < 8;
        let solid = |x: usize, y: usize, z: usize| y == 6 || (y == 12 && roofed(x, z));

        let mut centre = air_section();
        for x in 0..16 {
            for y in 0..16 {
                for z in 0..16 {
                    if solid(x, y, z) {
                        centre.set_block(x, y, z, id::STONE);
                    }
                }
            }
        }
        // The light field describes the world *before* the placement.
        let mut sky = LightData::Uniform(0);
        let mut block = LightData::Uniform(0);
        for x in 0..16 {
            for y in 0..16 {
                for z in 0..16 {
                    if solid(x, y, z) || y <= 5 {
                        continue;
                    }
                    let i = NibbleArray::index(x, y, z);
                    if roofed(x, z) && y <= 11 {
                        block.set(i, 11);
                    } else {
                        sky.set(i, 15);
                    }
                }
            }
        }
        if let Some([x, y, z]) = placed {
            assert_eq!(
                centre.get_block(x, y, z),
                id::AIR,
                "the fixture must place into an air cell"
            );
            centre.set_block(x, y, z, id::STONE);
        }

        let light = SectionLightData { sky, block };
        let mut sections = Vec::with_capacity(27);
        let mut lights = Vec::with_capacity(27);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    sections.push(if (dx, dy, dz) == (0, 0, 0) {
                        Neighbour::Present(Arc::new(centre.clone()))
                    } else {
                        // `Air`, not `Unloaded`: this fixture's neighbourhood is
                        // deliberately empty, not deliberately unknown.
                        Neighbour::Air
                    });
                    // Every slot carries real light, so the absent-neighbour
                    // bridge cannot leak full-bright into this measurement.
                    lights.push(Some(light.clone()));
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
            biome_names: Arc::from([]),
            light_revision: None,
        }
    }

    fn probe(snapshot: &SectionSnapshot, rule: LightRule) -> ModelMesh {
        let view = ProbeView {
            snapshot,
            light: SnapshotLight::new(snapshot),
            quads: cube_quads(),
            empty: Vec::new(),
            rule,
        };
        mesh_models(&view)
    }

    /// Anti-vacuity: the fixture really does hold a lit/shadowed distinction, and
    /// the shipped rule resolves *the same block's* two faces differently.
    ///
    /// Without this, every assertion below could be satisfied by a constant.
    #[test]
    fn face_light_distinguishes_sunlit_shadowed_and_torchlit_faces() {
        let snap = platform_snapshot(None);
        let mesh = probe(&snap, LightRule::FaceNeighbour);

        // The platform block under open sky: bright on top (opens into sky-15
        // air), dark underneath (opens into the unlit cave). One block, two
        // values — a constant light field cannot produce this.
        let open_top = quad_light(&mesh, [12, 6, 12], Direction::Up).expect("open top face");
        let open_bottom = quad_light(&mesh, [12, 6, 12], Direction::Down).expect("open bottom");
        assert_eq!(open_top, 0xF0, "a sunlit top face carries sky 15");
        assert_eq!(open_bottom, 0x00, "the cave-side face carries no light");

        // The platform under the roof: its top opens into the torchlit pocket,
        // so it is neither 15 nor 0 — a third, independently sourced population.
        let roofed_top = quad_light(&mesh, [2, 6, 2], Direction::Up).expect("roofed top face");
        assert_eq!(
            roofed_top, 0x0B,
            "a roofed top face carries the pocket's block light 11 and sky 0"
        );
    }

    /// **The defect.** A block placed into open sky must mesh with the same light
    /// as the terrain beside it. Before the fix it did not: the model path lit
    /// every block from its own cell, which the light engine stores as `0` for a
    /// solid, while the just-placed block's cell still held the sky-15 of the air
    /// it replaced. The new block rendered at the shader's maximum against
    /// neighbours at its minimum — the player-reported "super bright".
    ///
    /// Asserted as a *relationship* (placed == its neighbours), not an absolute,
    /// so it cannot be satisfied by clamping everything to one value: the
    /// shadowed half of the same fixture is checked in the same test.
    #[test]
    fn a_placed_block_meshes_with_its_neighbours_light_not_full_bright() {
        // Before: bare platform. After: one stone dropped on top of it, with the
        // pre-placement light field still in force (the optimistic window).
        let before = platform_snapshot(None);
        let after = platform_snapshot(Some([12, 7, 12]));

        let neighbour_before = quad_light(
            &probe(&before, LightRule::FaceNeighbour),
            [11, 6, 12],
            Direction::Up,
        )
        .expect("neighbouring platform top");

        let after_mesh = probe(&after, LightRule::FaceNeighbour);
        let placed_top =
            quad_light(&after_mesh, [12, 7, 12], Direction::Up).expect("placed block top");
        let placed_side =
            quad_light(&after_mesh, [12, 7, 12], Direction::East).expect("placed block side");
        let neighbour_after = quad_light(&after_mesh, [11, 6, 12], Direction::Up)
            .expect("neighbouring platform top, after");

        assert_eq!(
            neighbour_before, neighbour_after,
            "placing a block must not change the light of the terrain beside it"
        );
        assert_eq!(
            placed_top, neighbour_after,
            "the placed block's top must match the sunlit terrain beside it \
             ({placed_top:#04x} vs {neighbour_after:#04x})"
        );
        assert_eq!(
            placed_side, neighbour_after,
            "the placed block's side must match too ({placed_side:#04x})"
        );

        // And the same placement in shadow must land *dark*, so "matches its
        // neighbours" cannot be met by returning full-bright everywhere.
        let shadowed = platform_snapshot(Some([2, 7, 2]));
        let shadow_mesh = probe(&shadowed, LightRule::FaceNeighbour);
        let shadow_top =
            quad_light(&shadow_mesh, [2, 7, 2], Direction::Up).expect("shadowed placed top");
        assert_eq!(
            shadow_top, 0x0B,
            "a block placed in the torchlit, roofed pocket takes the pocket's \
             light (sky 0, block 11) — not sky 15"
        );
        assert!(
            shadow_top < placed_top,
            "the shadowed placement must be measurably darker than the sunlit one"
        );
    }

    /// The negative control, run rather than described: with the pre-fix
    /// own-cell rule restored **and nothing else changed**, the same placement
    /// renders brighter than the terrain it sits on. If this ever stops failing
    /// the way it does here, the assertion above has gone vacuous.
    ///
    /// # Smooth lighting diluted this control, and the numbers say by how much
    ///
    /// When this test was written `mesh_models` was flat-lit, so an own-cell face
    /// carried its cell's stored light outright: a solid cell reads `0`, and the
    /// defect was a 15-vs-0 contrast. `1b8e46b` added per-vertex smooth lighting,
    /// which averages the face cell with three corner neighbours — so the centre
    /// contributes only **a quarter** of the result and an opaque cell's `0` is
    /// pulled up by whatever surrounds it. Measured here:
    ///
    /// | face | own-cell rule | shipped rule |
    /// |---|---|---|
    /// | just-placed block, open sky | `0xF0` | `0xF0` |
    /// | established terrain beside it | `0xB0` (`round(45/4) = 11`) | `0xF0` |
    /// | sunlit platform top, no placement | `0xB0` | `0xF0` |
    /// | roofed platform top | `0x08` (`round(33/4) = 8`) | `0x0B` |
    ///
    /// So the defect is still there and still visible — a seam between a placed
    /// block and its neighbours — but it is sky 11 vs 15, not 0 vs 15. Two claims
    /// this control used to make are now simply **false** and are replaced rather
    /// than patched: "own-cell reads 0 inside every opaque block" (it reads the
    /// smoothed average) and "own-cell cannot tell a sunlit face from a roofed
    /// one" (the corner samples leak that distinction back in). What is asserted
    /// instead is the *relationship between the two rules* at the same faces,
    /// which cannot be satisfied by a constant and cannot be satisfied by the
    /// shipped rule.
    #[test]
    fn control_own_cell_light_makes_the_placed_block_brighter_than_its_neighbours() {
        let after = platform_snapshot(Some([12, 7, 12]));
        let mesh = probe(&after, LightRule::OwnCell);

        let placed_top = quad_light(&mesh, [12, 7, 12], Direction::Up).expect("placed block top");
        let neighbour = quad_light(&mesh, [11, 6, 12], Direction::Up).expect("neighbour top");

        assert_eq!(
            placed_top, 0xF0,
            "control: own-cell sampling reads the stale air light of the cell the \
             block replaced — full bright"
        );
        assert_eq!(
            neighbour, 0xB0,
            "control: own-cell sampling reads 0 inside the opaque neighbour, \
             smoothed against its three sky-15 corners"
        );
        assert!(
            placed_top > neighbour,
            "control must reproduce the reported defect: a just-placed block \
             brighter than the terrain it sits on"
        );

        // The whole world, not just the placement: under the old rule *every*
        // solid face is darker than it should be, sunlit and roofed alike, because
        // its own unlit cell is a quarter of every corner average. Measured on the
        // bare platform, where the sunlit top face is not covered by the placement,
        // and compared against the shipped rule at the same two faces — a
        // rule-versus-rule assertion the shipped rule cannot satisfy.
        let bare_own = probe(&platform_snapshot(None), LightRule::OwnCell);
        let bare_face = probe(&platform_snapshot(None), LightRule::FaceNeighbour);
        for (block, label) in [([12usize, 6, 12], "sunlit"), ([2, 6, 2], "roofed")] {
            let own = quad_light(&bare_own, block, Direction::Up)
                .expect("control: the platform top face must exist");
            let shipped = quad_light(&bare_face, block, Direction::Up)
                .expect("the platform top face must exist");
            assert!(
                own < shipped,
                "control: the {label} top face must read darker under own-cell \
                 sampling ({own:#04x}) than under the shipped face-neighbour rule \
                 ({shipped:#04x})"
            );
        }
        assert_eq!(
            quad_light(&bare_own, [12, 6, 12], Direction::Up),
            Some(0xB0),
            "control: sunlit top face, own cell 0 smoothed against three sky-15 corners"
        );
        assert_eq!(
            quad_light(&bare_own, [2, 6, 2], Direction::Up),
            Some(0x08),
            "control: roofed top face, own cell 0 smoothed against three block-11 corners"
        );
    }

    // -----------------------------------------------------------------------
    // Walk harness for bounded tracking-view eviction.
    // -----------------------------------------------------------------------

    /// Chebyshev radius of the simulated tracking view, in columns, for the
    /// eviction pair. Small on purpose — the defect is about *unbounded growth*,
    /// which a short walk already separates from a bounded window by a factor of
    /// two.
    ///
    /// The heal-backlog test uses [`BACKLOG_RD`] instead, which is larger and
    /// derived; see its doc for why one radius could not serve both, and note
    /// that widening this one would cost every test in this harness real
    /// worldgen time (≈0.15 s per column, and the column count grows as
    /// `(WALK_STEPS + 2·rd + 1)(2·rd + 1)`).
    const WALK_RD: i32 = 3;
    /// How many columns the simulated player advances in `+x`.
    const WALK_STEPS: i32 = 12;

    /// Chebyshev view radius for
    /// [`standing_still_drains_the_heal_backlog_and_no_column_is_lost`], derived
    /// from [`DIRTY_COLUMN_BUDGET`] rather than chosen.
    ///
    /// # Why this cannot be a constant
    ///
    /// A backlog only forms if columns are dirtied faster than one frame's budget
    /// clears them, and at one frame per step there is exactly one moment in the
    /// walk when that can happen: **step 0**, where the whole view loads at once
    /// and every column of it is dirtied by a neighbour's arrival, offering
    /// `(2·rd + 1)²` columns against a single budget. Every later step is a
    /// frontier of `2·(2·rd + 1)` columns — 14 at `WALK_RD` — which no budget
    /// above about 15 can fall behind no matter how many steps are added. So the
    /// scenario size is a function of the budget, and `WALK_STEPS` is not a lever
    /// on it.
    ///
    /// Hard-coding `3` here would silently void the test when the budget grows:
    /// a 49-column window would drain inside one frame, `max_backlog` would go
    /// from 45 (= 49 − 4) to **0**, and the vacuity guard would fire rather than
    /// the test passing quietly. This solves for `rd` instead, so
    /// the next budget change scales the scenario instead of breaking it:
    ///
    /// ```text
    /// (2·rd + 1)² > 2 · DIRTY_COLUMN_BUDGET
    /// ```
    ///
    /// One budget to drain in the first frame and more than one still queued
    /// after it, which is exactly what the guard requires. At 64 that gives
    /// `rd = 6` — a 169-column window, 325 columns visited, and a first-frame
    /// backlog of 105. A view distance of 6 is also a real setting a player can
    /// pick, so the scenario is not absurd; if a future budget pushed this past
    /// vanilla's maximum of 32 the honest conclusion would be that no reachable
    /// view distance can back the queue up, and the deferral bug it guards could
    /// no longer occur by that route.
    ///
    /// Floored at `WALK_RD` so a *smaller* budget keeps the original geometry.
    const BACKLOG_RD: i32 = {
        let mut rd = WALK_RD;
        while ((2 * rd + 1) * (2 * rd + 1)) as usize <= 2 * DIRTY_COLUMN_BUDGET {
            rd += 1;
        }
        rd
    };

    /// The columns a tracking view of radius `rd` centred on `(ccx, 0)` holds.
    fn window_rd(ccx: i32, rd: i32) -> BTreeSet<(i32, i32)> {
        let mut out = BTreeSet::new();
        for cx in (ccx - rd)..=(ccx + rd) {
            for cz in -rd..=rd {
                out.insert((cx, cz));
            }
        }
        out
    }

    /// [`window_rd`] at [`WALK_RD`], the eviction pair's radius.
    fn window(ccx: i32) -> BTreeSet<(i32, i32)> {
        window_rd(ccx, WALK_RD)
    }

    /// Every column that at some point during a radius-`rd` walk had all eight of
    /// its horizontal neighbours resident at once — i.e. exactly the columns that
    /// can ever have escaped [`SnapshotOutcome::Deferred`] and reached the GPU.
    ///
    /// Derived from the walk's geometry rather than measured, so it is an
    /// *outside* expectation: the two frontier columns in `x` (the first view's
    /// trailing ring, which is unloaded before the view ever advances past it,
    /// and the last view's leading ring) never qualify, and neither do the
    /// `z = ±rd` rows, since the view never moves in `z`. That is
    /// `(WALK_STEPS + 2·rd − 1) × (2·rd − 1)` columns out of
    /// `(WALK_STEPS + 2·rd + 1) × (2·rd + 1)` visited: 17 × 5 = 85 of 133 at
    /// [`WALK_RD`], 23 × 11 = 253 of 325 at [`BACKLOG_RD`].
    fn ever_interior_rd(rd: i32) -> BTreeSet<(i32, i32)> {
        let mut out = BTreeSet::new();
        for cx in (-rd + 1)..=(WALK_STEPS + rd - 1) {
            for cz in (-rd + 1)..=(rd - 1) {
                out.insert((cx, cz));
            }
        }
        out
    }

    /// [`ever_interior_rd`] at [`WALK_RD`], the eviction pair's radius.
    fn ever_interior() -> BTreeSet<(i32, i32)> {
        ever_interior_rd(WALK_RD)
    }

    /// A `TerrainMesh` whose deferral rule is the **live** one.
    ///
    /// `ColumnSource` is derived from the classifier, and the demo classifier
    /// yields `Complete` — under which nothing ever defers and the frontier
    /// behaviour this harness exists to model does not exist. Overriding the
    /// field is how a hermetic test reaches the `Streaming` rule without the
    /// vanilla atlas; it is the one production fact the demo classifier cannot
    /// supply. The eviction path under test (`forget_column` →
    /// `pending_removals` → `drain_removals`) is id-space agnostic.
    fn streaming_terrain() -> TerrainMesh {
        let mut terrain =
            TerrainMesh::new(MeshScheduler::new(2, ShellClassifier::Demo(DemoClassifier)));
        terrain.column_source = ColumnSource::Streaming;
        terrain
    }

    #[test]
    fn nearby_arrivals_get_first_mesh_without_waiting_for_padding() {
        let write = ChunkWorldWrite::new(World::new());
        for cx in [1, 2] {
            write
                .write()
                .load(ChunkPos::new(cx, 0), seam_column(&|_, _| true));
        }
        let store = write.read_handle();
        let mut terrain = streaming_terrain();
        terrain.queue_column_arrival(1, 0);
        terrain.queue_column_arrival(2, 0);

        let mut app = App::new();
        app.insert_resource(store);
        app.insert_resource(terrain);
        app.world_mut().spawn((
            LocalPlayer,
            PhysicsState(lodestone_physics::PlayerState::at(
                lodestone_physics::Vec3d::new(0.5, 4.0, 0.5),
                0.0,
            )),
        ));
        app.add_systems(Update, heal_dirty_columns);
        app.update();

        let terrain = app.world().resource::<TerrainMesh>();
        assert!(terrain.provisional_columns.contains(&(1, 0)));
        assert!(terrain.pending_arrivals.contains(&(2, 0)));
    }

    #[test]
    fn an_arriving_column_without_a_horizontal_halo_does_not_snapshot() {
        let write = ChunkWorldWrite::new(World::new());
        write
            .write()
            .load(ChunkPos::new(0, 0), seam_column(&|_, _| true));
        let store = write.read_handle();
        let mut terrain = streaming_terrain();
        terrain.queue_column_arrival(0, 0);

        assert!(terrain.dirty_columns.is_empty());
        assert!(terrain.pending_arrivals.contains(&(0, 0)));
        assert_eq!(
            terrain.mesh_arriving_column(&store, 0, 0, false),
            0,
            "a missing halo must not spend snapshot work or re-enter ready work"
        );
        assert!(terrain.dirty_columns.is_empty());
        assert!(terrain.pending_arrivals.contains(&(0, 0)));
        assert_eq!(terrain.scheduler.pending(), 0);
    }

    #[test]
    fn a_waiting_arrival_enters_ready_work_on_the_last_halo_dependency() {
        let write = ChunkWorldWrite::new(World::new());
        write
            .write()
            .load(ChunkPos::new(0, 0), seam_column(&|_, _| true));
        let store = write.read_handle();
        let mut terrain = streaming_terrain();
        terrain.queue_column_arrival(0, 0);

        let neighbours = [
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ];
        for &(cx, cz) in &neighbours[..neighbours.len() - 1] {
            write
                .write()
                .load(ChunkPos::new(cx, cz), seam_column(&|_, _| true));
            terrain.mark_neighbours_dirty(&store, cx, cz);
            assert!(terrain.pending_arrivals.contains(&(0, 0)));
            assert!(!terrain.dirty_columns.contains((0, 0)));
        }

        let &(cx, cz) = neighbours.last().expect("the fixture has a final dependency");
        write
            .write()
            .load(ChunkPos::new(cx, cz), seam_column(&|_, _| true));
        terrain.mark_neighbours_dirty(&store, cx, cz);
        assert!(!terrain.pending_arrivals.contains(&(0, 0)));
        assert!(terrain.dirty_columns.contains((0, 0)));
    }

    #[test]
    fn the_view_center_is_meshed_before_its_horizontal_halo_arrives() {
        let write = ChunkWorldWrite::new(World::new());
        write
            .write()
            .load(ChunkPos::new(0, 0), seam_column(&|_, _| true));
        let store = write.read_handle();
        let mut terrain = streaming_terrain();
        terrain.queue_column_arrival(0, 0);

        assert!(terrain.mesh_arriving_column(&store, 0, 0, true) > 0);
        assert!(terrain.scheduler.pending() > 0);
        assert!(!terrain.pending_arrivals.contains(&(0, 0)));
    }

    #[test]
    fn a_provisional_view_center_rebuilds_once_when_its_halo_completes() {
        let write = ChunkWorldWrite::new(World::new());
        write
            .write()
            .load(ChunkPos::new(0, 0), seam_column(&|_, _| true));
        let store = write.read_handle();
        let mut terrain = streaming_terrain();
        terrain.queue_column_arrival(0, 0);
        terrain.mesh_arriving_column(&store, 0, 0, true);
        let initial = terrain.drain_all_meshes();
        assert!(!initial.is_empty());
        for mesh in initial {
            terrain.mark_mesh_uploaded(mesh.key);
        }

        let neighbours = [
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ];
        for (index, (cx, cz)) in neighbours.into_iter().enumerate() {
            write
                .write()
                .load(ChunkPos::new(cx, cz), seam_column(&|_, _| true));
            terrain.mark_neighbours_dirty(&store, cx, cz);
            assert_eq!(
                terrain.dirty_columns.contains((0, 0)),
                index + 1 == neighbours.len(),
            );
        }
        assert!(!terrain.provisional_columns.contains(&(0, 0)));
    }

    #[test]
    fn a_completed_horizontal_halo_presents_the_same_mesh_as_direct_admission() {
        let write = ChunkWorldWrite::new(World::new());
        for cx in -1..=1 {
            for cz in -1..=1 {
                write
                    .write()
                    .load(ChunkPos::new(cx, cz), seam_column(&|_, _| true));
            }
        }
        let store = write.read_handle();

        let mut admitted = streaming_terrain();
        admitted.queue_column_arrival(0, 0);
        assert!(admitted.promote_ready_arrival(&store, 0, 0));
        assert!(
            admitted.mesh_arriving_column(&store, 0, 0, false) > 0,
            "a complete halo must admit the arriving column"
        );
        let actual = admitted.drain_all_meshes();
        for mesh in &actual {
            admitted.mark_mesh_uploaded(mesh.key);
        }
        assert!(admitted.column_mesh_settled(&store, 0, 0));

        let mut direct = streaming_terrain();
        direct.column_source = ColumnSource::Complete;
        direct.mesh_column(&store, 0, 0);
        let expected = direct.drain_all_meshes();

        assert_eq!(actual.len(), expected.len());
        for (got, want) in actual.iter().zip(expected.iter()) {
            assert_eq!(got.key, want.key);
            match (&got.mesh, &want.mesh) {
                (SectionGeometry::Packed(got), SectionGeometry::Packed(want)) => {
                    assert_eq!(got, want);
                }
                _ => panic!("the hermetic demo fixture must use packed geometry"),
            }
        }
    }

    /// Walk `+x` across `WALK_STEPS` columns behind a moving tracking view,
    /// returning the columns still resident on the (modelled) GPU at the end.
    ///
    /// `evict` is the switch this test and its control share: `true` is the
    /// fixed client, `false` reproduces the client without unload handling — the
    /// arrival half wired and the unload half absent. Everything else is
    /// identical, so the two runs differ only in the thing under test.
    fn walk(evict: bool) -> (BTreeSet<(i32, i32)>, BTreeSet<(i32, i32)>) {
        // The test edits the store, so it holds the write handle and
        // hands the paired read handle to the mesher — the same split production
        // (`drive_placement`) observes.
        let write = ChunkWorldWrite::new(World::new());
        let store = write.read_handle();
        let mut terrain = streaming_terrain();
        // The modelled GPU: what `RenderState`'s section map would hold, driven
        // by the same two drains `app/redraw.rs` calls, in the same order.
        let mut gpu: HashSet<SectionKey> = HashSet::new();
        let mut visited: BTreeSet<(i32, i32)> = BTreeSet::new();

        let mut live = BTreeSet::new();
        for step in 0..=WALK_STEPS {
            let next = window(step);
            // Arrivals: the adapter writes the column, then the shell is told.
            for &(cx, cz) in next.difference(&live) {
                write
                    .write()
                    .load(ChunkPos::new(cx, cz), crate::worldgen::generate_column(cx, cz));
                visited.insert((cx, cz));
            }
            for &(cx, cz) in next.difference(&live) {
                terrain.mesh_column(&store, cx, cz);
                terrain.mark_neighbours_dirty(&store, cx, cz);
            }
            // Departures: the adapter unloads the column *before* it emits, so
            // the store has already lost it when `forget_column` runs. Modelling
            // that order is the point — a `forget_column` that tried to read the
            // store would enumerate nothing.
            for &(cx, cz) in live.difference(&next) {
                write.write().unload(ChunkPos::new(cx, cz));
                if evict {
                    terrain.forget_column(cx, cz);
                    terrain.force_neighbours_of_departed(&store, cx, cz);
                }
            }
            live = next;

            // Drain the heal queue completely rather than at
            // `DIRTY_COLUMN_BUDGET`: the subject is eviction, and leaving a
            // backlog would let a *starved* run masquerade as a bounded one.
            while let Some((cx, cz)) = terrain.dirty_columns.pop_next() {
                terrain.mesh_column(&store, cx, cz);
            }
            for key in terrain.drain_removals() {
                gpu.remove(&key);
            }
            for meshed in terrain.drain_all_meshes() {
                gpu.insert(meshed.key);
            }
        }

        let resident: BTreeSet<(i32, i32)> = gpu.iter().map(|k| (k.cx, k.cz)).collect();
        (resident, visited)
    }

    /// **The invariant gate.** Nothing may stay on the GPU for a column the
    /// client no longer has.
    ///
    /// Stated as a *predicted magnitude*, not a direction: the two hypotheses
    /// are computed from the walk's own geometry and the measurement has to land
    /// on one of them. Bounded (correct) is at most `window()`'s 49 columns;
    /// unbounded is every column the walk ever visited, 126 at these
    /// constants. "Fewer than it visited" would be satisfied by both, so it is
    /// not what this asserts.
    #[test]
    fn walking_away_evicts_meshes_for_columns_the_client_dropped() {
        let (resident, visited) = walk(true);
        let live = window(WALK_STEPS);

        // Not vacuous: the walk has to have drawn something, and to have moved
        // far enough that the two hypotheses are actually distinguishable.
        assert!(
            visited.len() >= live.len() * 2,
            "harness must outrun its own window for the bound to mean anything: \
             visited {} vs window {}",
            visited.len(),
            live.len()
        );
        // Both hypotheses, computed from outside the code under test, so the
        // measurement has to land on one of them. Correct: the columns that ever
        // escaped deferral *and* are still in view. Without eviction handling,
        // every column that
        // ever escaped deferral, in view or not.
        let correct: BTreeSet<(i32, i32)> =
            ever_interior().intersection(&live).copied().collect();
        let leaked = ever_interior();
        assert!(
            correct.len() * 2 < leaked.len(),
            "the two hypotheses must be far apart or this asserts nothing: \
             {} vs {}",
            correct.len(),
            leaked.len()
        );
        assert!(
            !correct.is_empty(),
            "the interior of the final view must be drawn, else this gate would \
             pass on a client that renders nothing at all"
        );

        let stale: Vec<(i32, i32)> = resident.difference(&live).copied().collect();
        assert!(
            stale.is_empty(),
            "{} column(s) still hold GPU geometry after the client dropped them; \
             x range {:?}..={:?} — the renderer is drawing blocks the store no \
             longer has, and its section-origin arena never gets those slots back. \
             resident={} live-window={} ever-visited={}",
            stale.len(),
            stale.iter().map(|c| c.0).min(),
            stale.iter().map(|c| c.0).max(),
            resident.len(),
            live.len(),
            visited.len()
        );
        assert_eq!(
            resident, correct,
            "residency must be exactly the in-view columns that escaped deferral \
             ({} of them); the leak hypothesis is {} columns",
            correct.len(),
            leaked.len()
        );
    }

    /// **The control for the gate above, and it must fail that gate's
    /// assertion.** Without the eviction call the identical walk leaves every
    /// column it ever visited resident — so the assertion is answering the
    /// question it claims to, rather than passing because the harness never
    /// evicts anything or never draws at all.
    #[test]
    fn control_without_eviction_the_walk_leaks_every_column_it_visited() {
        let (resident, visited) = walk(false);
        let live = window(WALK_STEPS);

        let stale: Vec<(i32, i32)> = resident.difference(&live).copied().collect();
        assert!(
            !stale.is_empty(),
            "premise check: with eviction suppressed the walk MUST leak, or the \
             gate above proves nothing — resident {} vs window {}",
            resident.len(),
            live.len()
        );
        // The leak is unbounded, not merely present: residency tracks the whole
        // walk rather than the view. This is the number that turns into an
        // exhausted origin arena and a silently dropped section.
        assert!(
            resident.len() > live.len(),
            "the pre-#479 leak grows past the view: resident {} vs window {}",
            resident.len(),
            live.len()
        );
        // And it leaks by exactly the predicted amount: residency tracks the
        // *walk*, not the view. 85 columns at these constants, against a 49-column
        // view — after twelve steps. This is the growth that ends at an exhausted
        // section-origin arena, whose failure mode is `upload_section` returning
        // early and new terrain never drawing.
        assert_eq!(
            resident,
            ever_interior(),
            "the pre-#479 leak is every column that ever escaped deferral: \
             resident {} vs view {} vs visited {}",
            resident.len(),
            live.len(),
            visited.len()
        );
    }

    /// **Is the mesh *scheduling* path lossy, or only slow?** Walks at the real
    /// [`DIRTY_COLUMN_BUDGET`] instead of draining the heal queue, then stands
    /// still.
    ///
    /// This is the discriminator between the two explanations that look identical
    /// from outside the process — a queue that never drains, and workers that are
    /// merely starved. It answers it with **counts of frames and columns**, never
    /// a duration: a millisecond figure taken on a shared machine gets attributed
    /// to the wrong cause, and this repo's record has a 585× instance of exactly
    /// that.
    ///
    /// The isolation is deliberate and is what makes the result mean something:
    /// mesh workers are drained to completion every frame, i.e. modelled as
    /// **infinitely fast**. So worker throughput cannot influence the outcome, and
    /// what is left under test is purely the enqueue/defer/heal *scheduling*. A
    /// pass therefore says something quite specific: **no column is lost or
    /// permanently deferred, so the mesh path is not lossy and any real-world
    /// shortfall is throughput or latency** — which is a worldgen/CPU question,
    /// not a client scheduling bug. A failure would have said the opposite.
    ///
    /// # The scenario is sized from the budget, not chosen
    ///
    /// This test runs at [`BACKLOG_RD`], not [`WALK_RD`], and that indirection is
    /// the whole reason it still means anything: with a hard-coded radius it went
    /// silently vacuous the moment `17c786e` raised [`DIRTY_COLUMN_BUDGET`] from
    /// 4 to 64, because a 49-column window drains inside one frame. Read
    /// `BACKLOG_RD`'s doc before touching either number.
    ///
    /// One consequence is worth stating rather than leaving to be discovered: at
    /// a 64-column budget the backlog is built by the *initial* view load and is
    /// gone within about three of the walk's own frames, so the standing-still
    /// phase now has nothing left to drain and reports **0 frames**. That is the
    /// truth at this budget, not a broken test — the frontier of a moving view is
    /// `2·(2·rd + 1)` columns and only a radius of 16 or more would outpace 64 per
    /// frame, which is a 1,089-column window and roughly four minutes of real
    /// worldgen. The standing loop stays as the bounded safety net that would
    /// catch a genuinely stuck queue, `frames_with_backlog` is what proves the
    /// queue carried work across frames, and `resident == expected` — the
    /// permanent-deferral evidence remains unaffected either way and
    /// now covers 132 columns rather than 30.
    #[test]
    fn standing_still_drains_the_heal_backlog_and_no_column_is_lost() {
        // One frame per step is deliberately the *worst* case the shell can
        // present: a column arrives and the heal system gets a single
        // `DIRTY_COLUMN_BUDGET` before the player has moved again. If the backlog
        // is going to diverge, it diverges here.
        const FRAMES_PER_STEP: usize = 1;

        // Predicted from outside the code under test, before it runs: step 0
        // loads the whole view at once and every column of it is dirtied by a
        // neighbour's arrival, so the first frame is offered `window` columns and
        // clears exactly `DIRTY_COLUMN_BUDGET` of them. Later frontiers are
        // `2·(2·rd+1)` columns, well inside one budget, so the queue only shrinks
        // from here and this is also the maximum over the whole walk.
        let view_columns = window_rd(0, BACKLOG_RD).len();
        // Checked before the subtraction, because an undersized scenario makes
        // that subtraction underflow and "attempt to subtract with overflow" is a
        // useless thing to hand whoever next changes the budget. Observed: with
        // `BACKLOG_RD` forced to `WALK_RD` this fires and names the cause, where
        // the bare subtraction panicked with nothing to act on.
        assert!(
            view_columns > 2 * DIRTY_COLUMN_BUDGET,
            "premise: a {view_columns}-column view cannot leave more than one \
             further {DIRTY_COLUMN_BUDGET}-column budget queued after the first \
             frame, so no backlog forms and this test would measure nothing. \
             `BACKLOG_RD` ({BACKLOG_RD}) must solve \
             (2·rd + 1)² > 2 · DIRTY_COLUMN_BUDGET."
        );
        let predicted_first_frame_backlog = view_columns - DIRTY_COLUMN_BUDGET;

        let write = ChunkWorldWrite::new(World::new());
        let store = write.read_handle();
        let mut terrain = streaming_terrain();
        let mut gpu: HashSet<SectionKey> = HashSet::new();
        let mut max_backlog = 0usize;
        // Frames that ended with work still queued — i.e. frames across which the
        // heal queue actually carried a column. See the assertion below.
        let mut frames_with_backlog = 0usize;

        let mut live = BTreeSet::new();
        for step in 0..=WALK_STEPS {
            let next = window_rd(step, BACKLOG_RD);
            for &(cx, cz) in next.difference(&live) {
                write
                    .write()
                    .load(ChunkPos::new(cx, cz), crate::worldgen::generate_column(cx, cz));
            }
            for &(cx, cz) in next.difference(&live) {
                terrain.mesh_column(&store, cx, cz);
                terrain.mark_neighbours_dirty(&store, cx, cz);
            }
            for &(cx, cz) in live.difference(&next) {
                write.write().unload(ChunkPos::new(cx, cz));
                terrain.forget_column(cx, cz);
                terrain.force_neighbours_of_departed(&store, cx, cz);
            }
            live = next;

            for _ in 0..FRAMES_PER_STEP {
                // `heal_dirty_columns`, by hand at its real budget.
                for _ in 0..DIRTY_COLUMN_BUDGET {
                    let Some((cx, cz)) = terrain.forced_columns.pop_first() else {
                        break;
                    };
                    terrain.mesh_column_forced(&store, cx, cz);
                }
                for _ in 0..DIRTY_COLUMN_BUDGET {
                    let Some((cx, cz)) = terrain.dirty_columns.pop_next() else {
                        break;
                    };
                    terrain.mesh_column(&store, cx, cz);
                }
                for key in terrain.drain_removals() {
                    gpu.remove(&key);
                }
                for meshed in terrain.drain_all_meshes() {
                    gpu.insert(meshed.key);
                }
                max_backlog = max_backlog.max(terrain.dirty_columns.len());
                if !terrain.dirty_columns.is_empty() {
                    frames_with_backlog += 1;
                }
            }
        }
        let backlog_while_walking = terrain.dirty_columns.len();

        // Now stand still. Nothing arrives and nothing unloads; only the heal
        // budget runs. Bounded so a genuinely stuck queue fails instead of
        // looping forever.
        let mut frames_to_drain = 0usize;
        for frame in 1..=512 {
            if terrain.dirty_columns.is_empty() && terrain.forced_columns.is_empty() {
                frames_to_drain = frame - 1;
                break;
            }
            for _ in 0..DIRTY_COLUMN_BUDGET {
                let Some((cx, cz)) = terrain.forced_columns.pop_first() else {
                    break;
                };
                terrain.mesh_column_forced(&store, cx, cz);
            }
            for _ in 0..DIRTY_COLUMN_BUDGET {
                let Some((cx, cz)) = terrain.dirty_columns.pop_next() else {
                    break;
                };
                terrain.mesh_column(&store, cx, cz);
            }
            for key in terrain.drain_removals() {
                gpu.remove(&key);
            }
            for meshed in terrain.drain_all_meshes() {
                gpu.insert(meshed.key);
            }
            frames_to_drain = frame;
        }

        let resident: BTreeSet<(i32, i32)> = gpu.iter().map(|k| (k.cx, k.cz)).collect();
        let expected: BTreeSet<(i32, i32)> = ever_interior_rd(BACKLOG_RD)
            .intersection(&window_rd(WALK_STEPS, BACKLOG_RD))
            .copied()
            .collect();
        eprintln!(
            "backlog: max {max_backlog} (predicted {predicted_first_frame_backlog} \
             from a {view_columns}-column view against a {DIRTY_COLUMN_BUDGET}-column \
             budget at rd {BACKLOG_RD}), carried across {frames_with_backlog} \
             frames, {backlog_while_walking} on arrival at the last step, drained \
             in {frames_to_drain} standing frames; resident {} of {} expected",
            resident.len(),
            expected.len()
        );

        assert!(
            terrain.dirty_columns.is_empty() && terrain.forced_columns.is_empty(),
            "the heal queue never drained while standing still ({} dirty + {} forced \
             columns left) — that is a genuine queue bug, not starvation",
            terrain.dirty_columns.len(),
            terrain.forced_columns.len()
        );
        // Not vacuous: there has to have *been* a backlog for draining it to be
        // evidence of anything.
        //
        // The *predicted* count, not merely "more than a budget": asserting the
        // sign of the thing would be satisfied by any backlog at all, and would
        // have gone on passing through a change that shrank the real one to a
        // single column. The two competing hypotheses are computed from outside
        // constants and this has to land on one of them — `view_columns −
        // DIRTY_COLUMN_BUDGET` if the first frame is offered the whole view (the
        // mechanism `BACKLOG_RD` is derived from), or `0` if the window drains
        // inside one frame, which is what the 4 → 64 budget change did.
        assert_eq!(
            max_backlog, predicted_first_frame_backlog,
            "the backlog peak must be the whole view minus one frame's budget \
             ({view_columns} − {DIRTY_COLUMN_BUDGET}). A max of 0 means the window \
             drains inside a single frame and this test exercised no queue at all \
             — re-derive `BACKLOG_RD` from `DIRTY_COLUMN_BUDGET` rather than \
             relaxing this."
        );
        assert!(
            max_backlog > DIRTY_COLUMN_BUDGET,
            "the walk never built a backlog past one frame's budget (max \
             {max_backlog}), so this test did not exercise the queue it claims to"
        );
        // And the queue really carried work *across frame boundaries*, which is
        // what "exercised the queue" has to mean — a peak measured inside a single
        // frame would say nothing about deferral surviving a frame. The floor is
        // derived, not picked: a `predicted_first_frame_backlog`-column queue
        // against a `DIRTY_COLUMN_BUDGET`-column budget cannot be cleared in
        // fewer than `ceil(predicted / budget)` further frames even with nothing
        // else arriving.
        let minimum_carrying_frames = predicted_first_frame_backlog.div_ceil(DIRTY_COLUMN_BUDGET);
        assert!(
            frames_with_backlog >= minimum_carrying_frames,
            "the backlog was carried across only {frames_with_backlog} frames, \
             but {predicted_first_frame_backlog} columns against a \
             {DIRTY_COLUMN_BUDGET}-column budget needs at least \
             {minimum_carrying_frames}"
        );
        assert_eq!(
            resident, expected,
            "a column was lost or permanently deferred: {} resident vs {} expected. \
             With mesh workers modelled as infinitely fast, that could only be the \
             scheduling path itself.",
            resident.len(),
            expected.len()
        );
    }

    // -----------------------------------------------------------------------
    // The mesh drain's order
    // -----------------------------------------------------------------------

    /// Every column of a Chebyshev-radius-`r` window about the origin, generated
    /// in the **lexicographic** order the old `BTreeSet` drain used — so the
    /// insertion order below is exactly the order this queue has to *not* keep.
    fn window_columns(r: i32) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        for cx in -r..=r {
            for cz in -r..=r {
                out.push((cx, cz));
            }
        }
        out
    }

    /// **Where the heal budget goes.** Near-and-in-front first, and — the
    /// property that matters more — a near column *behind* the player still beats
    /// a far one in front of them, so no amount of looking one way starves the
    /// other.
    #[test]
    fn the_mesh_drain_prefers_near_and_in_front_but_never_starves_what_is_behind() {
        let radius = 5;
        let mut dirty = DirtyColumns::default();
        for coord in window_columns(radius) {
            dirty.insert(coord);
        }
        // Yaw 0 is due +Z in vanilla's convention, so this player is looking at
        // the columns with positive `cz`.
        assert!(dirty.reprioritise((0, 0), Some(0.0)));

        let mut order = Vec::new();
        while let Some(coord) = dirty.pop_next() {
            order.push(coord);
        }
        assert_eq!(
            order.len(),
            ((2 * radius + 1) * (2 * radius + 1)) as usize,
            "every queued column must come back out exactly once"
        );

        // 1. Distance is the primary key, stated as the property: the ring bands
        //    are contiguous, so nothing at distance `d + 1` can precede anything
        //    at distance `d`. Its concrete form — and the one a pure
        //    frustum-first drain fails — is the pair below.
        let mut previous = 0;
        for &coord in &order {
            let distance = column_ring_distance((0, 0), coord);
            assert!(
                distance >= previous,
                "{coord:?} at distance {distance} follows distance {previous}: the facing \
                 bonus must never promote a far column over a near one, or a player who \
                 turns round finds a hole that was deprioritised for as long as they looked \
                 away"
            );
            previous = distance;
        }
        let behind = order
            .iter()
            .position(|&c| c == (0, -3))
            .expect("the column three behind the player is in the window");
        let ahead = order
            .iter()
            .position(|&c| c == (0, 4))
            .expect("the column four in front of the player is in the window");
        assert!(
            behind < ahead,
            "a near column behind the player must be meshed before a far one in front \
             (behind at {behind}, ahead at {ahead})"
        );

        // 2. …and within one ring the facing cone really does win, or the feature
        //    is inert and assertion 1 would be satisfied by an ordering that
        //    ignores the player's rotation entirely. The whole in-frustum half of
        //    the ring precedes the whole out-of-frustum half.
        let ring: Vec<(i32, i32)> = order
            .iter()
            .copied()
            .filter(|&c| column_ring_distance((0, 0), c) == radius)
            .collect();
        let split = ring
            .iter()
            .position(|&c| !column_in_frustum((0, 0), 0.0, c))
            .expect("a 120° cone cannot contain a whole ring");
        assert!(
            ring[..split]
                .iter()
                .all(|&c| column_in_frustum((0, 0), 0.0, c)),
            "the in-frustum columns of a ring must form its prefix; ring 5 drained as {ring:?}"
        );
        assert_eq!(
            order.first(),
            Some(&(0, 0)),
            "the player's own column is meshed first; the old lexicographic drain started \
             at {:?} instead",
            (-radius, -radius)
        );

        // A column dirtied *after* the queue was keyed is keyed too, rather than
        // appended: this is what makes the ordering hold during streaming, when
        // arrivals and drains interleave every frame.
        dirty.insert((0, -5));
        dirty.insert((0, 2));
        assert_eq!(
            dirty.pop_next(),
            Some((0, 2)),
            "a fresh dirty signal joins the order at its priority, not at the end"
        );
    }

    /// Dedup and unload, the two properties the `BTreeSet` gave for free and a
    /// heap does not: a column dirtied twice is meshed once, and one that left the
    /// view is not meshed at all — its tombstone must not resurface as a phantom
    /// pop.
    #[test]
    fn a_column_dirtied_twice_is_meshed_once_and_an_unloaded_one_not_at_all() {
        let mut dirty = DirtyColumns::default();
        assert!(dirty.insert((2, 3)));
        assert!(!dirty.insert((2, 3)), "a repeat dirty signal must coalesce");
        assert_eq!(dirty.len(), 1);

        assert!(dirty.insert((4, 0)));
        assert!(dirty.remove((4, 0)), "the column left the view");
        assert!(!dirty.remove((4, 0)), "and it is gone only once");
        assert_eq!(dirty.len(), 1);
        assert!(dirty.contains((2, 3)));

        assert_eq!(dirty.pop_next(), Some((2, 3)));
        assert_eq!(
            dirty.pop_next(),
            None,
            "the removed column must not come back out of the heap"
        );
        assert!(dirty.is_empty());
    }

    /// Re-keying runs on every frame, so the common case must not rebuild: it
    /// fires when the player crosses a chunk boundary or turns into a new yaw
    /// sector, and does nothing otherwise.
    #[test]
    fn rekeying_only_fires_when_the_column_or_the_yaw_sector_moves() {
        let mut dirty = DirtyColumns::default();
        for coord in window_columns(3) {
            dirty.insert(coord);
        }
        assert!(
            dirty.reprioritise((0, 0), Some(0.0)),
            "the first known rotation is a change: the default is no facing at all"
        );
        assert!(
            !dirty.reprioritise((0, 0), Some(0.0)),
            "an identical column and yaw must not rebuild"
        );
        assert!(
            !dirty.reprioritise((0, 0), Some(10.0)),
            "a sub-sector nudge (10° of 22.5°) must not rebuild"
        );
        assert!(
            dirty.reprioritise((0, 0), Some(90.0)),
            "a quarter turn is a new sector"
        );
        assert!(
            dirty.reprioritise((1, 0), Some(90.0)),
            "crossing a chunk boundary re-centres the whole ordering"
        );

        // And the new centre is what the order is keyed on afterwards.
        let mut previous = 0;
        while let Some(coord) = dirty.pop_next() {
            let distance = column_ring_distance((1, 0), coord);
            assert!(distance >= previous, "{coord:?} is out of order about (1, 0)");
            previous = distance;
        }
    }
}
