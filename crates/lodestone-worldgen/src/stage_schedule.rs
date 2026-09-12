//! Explicit world-generation pass order.
//!
//! The generators deliberately keep dimension-specific implementations, but
//! their pass order is still a contract.  Keeping that contract in one small
//! typed table makes it possible for production dispatchers and parity replay
//! to name the same stages instead of repeating an undocumented list of loops.
//!
//! This module contains no generation work and therefore cannot alter terrain.
//! A generator uses [`StageExecutor`] at the boundaries between helper
//! functions; it fails immediately if a refactor enters a pass out of order or
//! forgets one. [`LifecycleSchedule`] separately records the
//! externally observable status/dependency graph through packet finalization,
//! so an internal helper split cannot be mistaken for a chunk lifecycle edge.

/// Dimension whose pass schedule is being described.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dimension {
    Overworld,
    Nether,
    End,
}

/// A named world-generation pass.
///
/// Heightmaps and other read-only products are intentionally not stages: they
/// are values derived inside the pass that consumes them.  `Output` names the
/// conversion from the mutable generation field into the packet-ready column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ColumnStage {
    StructureStarts,
    StructureReferences,
    StructureInfluence,
    Fill,
    Biomes,
    Surface,
    Materialize,
    Carvers,
    StructurePlacement,
    Features,
    TopLayer,
    Output,
}

/// A dimension-qualified column-stage key.
///
/// `ColumnStage` is intentionally shared by the three generators, but a
/// stage name without its dimension is not enough to validate a persisted
/// frontier.  Keeping the pair typed also prevents an End record from being
/// accidentally resumed against the Nether's stage order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StageKey {
    dimension: Dimension,
    stage: ColumnStage,
}

impl StageKey {
    #[must_use]
    pub const fn new(dimension: Dimension, stage: ColumnStage) -> Self {
        Self { dimension, stage }
    }

    #[must_use]
    pub const fn dimension(self) -> Dimension {
        self.dimension
    }

    #[must_use]
    pub const fn stage(self) -> ColumnStage {
        self.stage
    }
}

/// Typed immutable products named by a stage descriptor.
///
/// These names describe the hand-off contract, not a storage implementation.
/// A generator may keep a product in an `Arc`, a resident column, or a
/// serialized record; the frontier only commits it after the producer has
/// supplied the product and every declared sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceKey {
    DensityField,
    BiomeQuarts,
    SurfaceDiff,
    MaterializedWorld,
    StructureStarts,
    StructureBlocks,
    ResidentRegion,
    ResidentOverlay,
    OutputColumn,
}

/// Typed metadata retained with a completed stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SidecarKey {
    StructureReferences,
    BlockEntityEvents,
    DecorationSpills,
    Gateways,
    ClientHeightmaps,
    SpawnCandidates,
}

/// Scope of the random stream consumed by a stage.
///
/// A stage descriptor records scope even when the current implementation
/// derives the stream inside a helper.  This makes a resume or a stage-outer
/// execution plan name the same seed boundary as the source-major path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SeedScope {
    None,
    TerrainChunk,
    BiomeChunk,
    SurfaceChunk,
    StructureOrigin,
    StructureChunk,
    SourceFeature,
}

/// Required synchronization boundary for a stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BarrierPolicy {
    /// The stage only reads immutable inputs and can commit independently.
    Pure,
    /// Source completions mutate one resident region in a canonical order.
    SourceOrdered,
    /// The stage's output is exposed only after all jobs in its domain commit.
    StageWide,
}

/// Where cancellation is allowed for one stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CancellationPolicy {
    /// Cancellation before the atomic commit leaves no frontier record.
    BeforeCommit,
    /// Mutable writes are private until the declared transaction commits.
    TransactionBoundary,
}

/// Conservative horizontal dependency or write footprint, in chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Radius2d {
    chunks: u8,
}

impl Radius2d {
    #[must_use]
    pub const fn chunks(chunks: u8) -> Self {
        Self { chunks }
    }

    #[must_use]
    pub const fn chunks_value(self) -> u8 {
        self.chunks
    }
}

/// One typed pass contract in a dimension's ordered schedule.
///
/// The schedule owns order; descriptors own the dependency, resident-write,
/// sidecar, seed, and cancellation contract for that order.  Keeping the two
/// layers separate lets the same stage table serve a scalar generator, a
/// bounded concurrent producer, and a parity replay without a second list of
/// pass names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageDescriptor {
    key: StageKey,
    prerequisites: &'static [StageKey],
    read_radius: Radius2d,
    write_radius: Radius2d,
    inputs: &'static [ResourceKey],
    outputs: &'static [ResourceKey],
    retained_sidecars: &'static [SidecarKey],
    seed_scope: SeedScope,
    barrier: BarrierPolicy,
    cancellation: CancellationPolicy,
}

impl StageDescriptor {
    #[must_use]
    pub const fn new(
        key: StageKey,
        prerequisites: &'static [StageKey],
        read_radius: Radius2d,
        write_radius: Radius2d,
        inputs: &'static [ResourceKey],
        outputs: &'static [ResourceKey],
        retained_sidecars: &'static [SidecarKey],
        seed_scope: SeedScope,
        barrier: BarrierPolicy,
        cancellation: CancellationPolicy,
    ) -> Self {
        Self {
            key,
            prerequisites,
            read_radius,
            write_radius,
            inputs,
            outputs,
            retained_sidecars,
            seed_scope,
            barrier,
            cancellation,
        }
    }

    #[must_use]
    pub const fn key(self) -> StageKey {
        self.key
    }

    #[must_use]
    pub const fn prerequisites(self) -> &'static [StageKey] {
        self.prerequisites
    }

    #[must_use]
    pub const fn read_radius(self) -> Radius2d {
        self.read_radius
    }

    #[must_use]
    pub const fn write_radius(self) -> Radius2d {
        self.write_radius
    }

    #[must_use]
    pub const fn inputs(self) -> &'static [ResourceKey] {
        self.inputs
    }

    #[must_use]
    pub const fn outputs(self) -> &'static [ResourceKey] {
        self.outputs
    }

    #[must_use]
    pub const fn retained_sidecars(self) -> &'static [SidecarKey] {
        self.retained_sidecars
    }

    #[must_use]
    pub const fn seed_scope(self) -> SeedScope {
        self.seed_scope
    }

    #[must_use]
    pub const fn barrier(self) -> BarrierPolicy {
        self.barrier
    }

    #[must_use]
    pub const fn cancellation(self) -> CancellationPolicy {
        self.cancellation
    }
}

/// A data/configuration option that may make a scheduled pass a no-op.
///
/// The pass remains part of the schedule even when its input data is absent;
/// this metadata records the reason an implementation may have nothing to do.
/// Keeping this typed prevents a production entrypoint from inventing a
/// private string flag whose relationship to the central schedule cannot be
/// checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StageOption {
    Structures,
    Decorations,
}

/// One declared option gate for a column stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StageGate {
    stage: ColumnStage,
    option: StageOption,
}

impl StageGate {
    #[must_use]
    pub const fn new(stage: ColumnStage, option: StageOption) -> Self {
        Self { stage, option }
    }

    #[must_use]
    pub const fn stage(self) -> ColumnStage {
        self.stage
    }

    #[must_use]
    pub const fn option(self) -> StageOption {
        self.option
    }
}

/// Runtime choices that can turn a gated generation pass into a typed no-op.
///
/// The schedule never changes when an option is disabled.  That is important
/// for incremental generation: a far column and a near column still have the
/// same frontier shape, even when a pack or a test deliberately omits
/// structures or decorations.  The default keeps both passes enabled, which
/// is the normal world-generation configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PipelineOptions {
    structures: bool,
    decorations: bool,
}

impl PipelineOptions {
    /// The ordinary world-generation options.
    pub const ALL: Self = Self {
        structures: true,
        decorations: true,
    };

    /// Construct an option set without exposing the representation to callers.
    #[must_use]
    pub const fn new(structures: bool, decorations: bool) -> Self {
        Self {
            structures,
            decorations,
        }
    }

    #[must_use]
    pub const fn structures(self) -> bool {
        self.structures
    }

    #[must_use]
    pub const fn decorations(self) -> bool {
        self.decorations
    }

    /// Return whether the option named by a stage gate is enabled.
    #[must_use]
    pub const fn enables(self, option: StageOption) -> bool {
        match option {
            StageOption::Structures => self.structures,
            StageOption::Decorations => self.decorations,
        }
    }

    /// A compact stable identity for persistence and cache keys.
    #[must_use]
    pub const fn fingerprint(self) -> u8 {
        (self.structures as u8) | ((self.decorations as u8) << 1)
    }
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self::ALL
    }
}

/// The two generation products exposed by the server's current source seam.
///
/// This is deliberately a worldgen-side vocabulary. The server maps its
/// `ChunkGenerationStage` to this target when it selects a generator entry
/// point; keeping the target here lets tests compare a source's shaped prefix
/// with the one central schedule without copying a second list of stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GenerationTarget {
    Shaped,
    Full,
}

/// Compact logical fidelity level for an incremental column request.
///
/// These levels are names for prefixes of a dimension's existing schedule,
/// not additional execution paths. `Packet` remains outside this enum because
/// light settlement and packet encoding are server lifecycle work after
/// `ColumnStage::Output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum GenerationLevel {
    Terrain = 0,
    Structures = 1,
    Decorated = 2,
    Output = 3,
}

impl GenerationLevel {
    /// The terminal stage for this logical level in a dimension schedule.
    ///
    /// `None` means the dimension has not yet established an independently
    /// measured closure for that level. The End and current server tiers are
    /// fully covered; callers must not silently substitute a nearby stage.
    #[must_use]
    pub const fn terminal_stage(self, schedule: StageSchedule) -> Option<ColumnStage> {
        match (schedule.dimension(), self) {
            (_, Self::Output) => Some(ColumnStage::Output),
            (Dimension::Overworld, Self::Terrain) | (Dimension::Nether, Self::Terrain) => {
                Some(ColumnStage::Surface)
            }
            (Dimension::End, Self::Terrain) => Some(ColumnStage::Surface),
            (Dimension::Overworld, Self::Structures) | (Dimension::Nether, Self::Structures) => {
                Some(ColumnStage::StructurePlacement)
            }
            (Dimension::End, Self::Structures) => Some(ColumnStage::StructurePlacement),
            (Dimension::Overworld, Self::Decorated) => Some(ColumnStage::TopLayer),
            (Dimension::Nether, Self::Decorated) | (Dimension::End, Self::Decorated) => {
                Some(ColumnStage::Features)
            }
        }
    }

    /// The level represented by a completed terminal stage, if one exists.
    #[must_use]
    pub fn from_completed_stage(schedule: StageSchedule, stage: ColumnStage) -> Option<Self> {
        let mut level = None;
        let levels = [Self::Terrain, Self::Structures, Self::Decorated, Self::Output];
        let mut index = 0;
        while index < levels.len() {
            if levels[index].terminal_stage(schedule) == Some(stage) {
                level = Some(levels[index]);
            }
            index += 1;
        }
        level
    }
}

/// A named, externally observable chunk-lifecycle phase.
///
/// These names deliberately do not mirror [`ColumnStage`] one-for-one. A
/// column stage is an implementation boundary inside a dimension generator;
/// a lifecycle phase is a scheduler boundary with a dependency radius and a
/// block-write radius. `PacketFinalization` is Lodestone's serving boundary,
/// after the generated column has been finalized and its centre light has
/// settled. Keeping it in this typed graph prevents packet parity from being
/// described as if it ended at [`ColumnStage::Output`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LifecyclePhase {
    Empty,
    StructureStarts,
    StructureReferences,
    Biomes,
    Noise,
    Surface,
    Carvers,
    Features,
    InitializeLight,
    Light,
    Spawn,
    Full,
    PacketFinalization,
}

/// One required predecessor of a lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseDependency {
    phase: LifecyclePhase,
    radius: u8,
}

impl PhaseDependency {
    #[must_use]
    pub const fn new(phase: LifecyclePhase, radius: u8) -> Self {
        Self { phase, radius }
    }

    #[must_use]
    pub const fn phase(self) -> LifecyclePhase {
        self.phase
    }

    #[must_use]
    pub const fn radius(self) -> u8 {
        self.radius
    }
}

/// Static contract for one lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseContract {
    phase: LifecyclePhase,
    dependencies: &'static [PhaseDependency],
    block_write_radius: u8,
}

impl PhaseContract {
    #[must_use]
    pub const fn new(
        phase: LifecyclePhase,
        dependencies: &'static [PhaseDependency],
        block_write_radius: u8,
    ) -> Self {
        Self {
            phase,
            dependencies,
            block_write_radius,
        }
    }

    #[must_use]
    pub const fn phase(self) -> LifecyclePhase {
        self.phase
    }

    #[must_use]
    pub const fn dependencies(self) -> &'static [PhaseDependency] {
        self.dependencies
    }

    #[must_use]
    pub const fn block_write_radius(self) -> u8 {
        self.block_write_radius
    }
}

/// The ordered lifecycle and dependency graph shared by all dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleSchedule {
    phases: &'static [PhaseContract],
}

impl LifecycleSchedule {
    #[must_use]
    pub const fn new(phases: &'static [PhaseContract]) -> Self {
        Self { phases }
    }

    #[must_use]
    pub const fn phases(self) -> &'static [PhaseContract] {
        self.phases
    }

    #[must_use]
    pub const fn contract(self, phase: LifecyclePhase) -> Option<PhaseContract> {
        let mut index = 0;
        while index < self.phases.len() {
            if self.phases[index].phase as u8 == phase as u8 {
                return Some(self.phases[index]);
            }
            index += 1;
        }
        None
    }

    /// Validate uniqueness, dependency direction, and the serving boundary.
    pub fn validate(self) {
        assert!(!self.phases.is_empty(), "worldgen lifecycle is empty");
        let mut index = 0;
        while index < self.phases.len() {
            let contract = self.phases[index];
            let mut previous = 0;
            while previous < index {
                assert_ne!(
                    self.phases[previous].phase, contract.phase,
                    "worldgen lifecycle repeats {:?}",
                    contract.phase
                );
                previous += 1;
            }
            for dependency in contract.dependencies {
                let dependency_index = self
                    .index_of(dependency.phase)
                    .expect("lifecycle dependency must name a scheduled phase");
                assert!(
                    dependency_index < index,
                    "lifecycle dependency {:?} must precede {:?}",
                    dependency.phase,
                    contract.phase
                );
            }
            index += 1;
        }
        assert_eq!(
            self.phases.last().map(|contract| contract.phase),
            Some(LifecyclePhase::PacketFinalization),
            "packet finalization must be the terminal lifecycle boundary"
        );
    }

    #[must_use]
    pub const fn index_of(self, phase: LifecyclePhase) -> Option<usize> {
        let mut index = 0;
        while index < self.phases.len() {
            if self.phases[index].phase as u8 == phase as u8 {
                return Some(index);
            }
            index += 1;
        }
        None
    }
}

const STARTS_8: &[PhaseDependency] = &[PhaseDependency::new(LifecyclePhase::StructureStarts, 8)];
const STARTS_8_BIOMES_1: &[PhaseDependency] = &[
    PhaseDependency::new(LifecyclePhase::StructureStarts, 8),
    PhaseDependency::new(LifecyclePhase::Biomes, 1),
];
const STARTS_8_CARVERS_1: &[PhaseDependency] = &[
    PhaseDependency::new(LifecyclePhase::StructureStarts, 8),
    PhaseDependency::new(LifecyclePhase::Carvers, 1),
];
const INITIALIZE_LIGHT_1: &[PhaseDependency] =
    &[PhaseDependency::new(LifecyclePhase::InitializeLight, 1)];
const BIOMES_1: &[PhaseDependency] = &[PhaseDependency::new(LifecyclePhase::Biomes, 1)];
const FULL_0_LIGHT_1: &[PhaseDependency] = &[
    PhaseDependency::new(LifecyclePhase::Full, 0),
    PhaseDependency::new(LifecyclePhase::Light, 1),
];

const LIFECYCLE_PHASES: &[PhaseContract] = &[
    PhaseContract::new(LifecyclePhase::Empty, &[], 0),
    PhaseContract::new(LifecyclePhase::StructureStarts, &[], 0),
    PhaseContract::new(LifecyclePhase::StructureReferences, STARTS_8, 0),
    PhaseContract::new(LifecyclePhase::Biomes, STARTS_8, 0),
    PhaseContract::new(LifecyclePhase::Noise, STARTS_8_BIOMES_1, 0),
    PhaseContract::new(LifecyclePhase::Surface, STARTS_8_BIOMES_1, 0),
    PhaseContract::new(LifecyclePhase::Carvers, STARTS_8, 0),
    PhaseContract::new(LifecyclePhase::Features, STARTS_8_CARVERS_1, 1),
    PhaseContract::new(LifecyclePhase::InitializeLight, &[], 0),
    PhaseContract::new(LifecyclePhase::Light, INITIALIZE_LIGHT_1, 0),
    PhaseContract::new(LifecyclePhase::Spawn, BIOMES_1, 0),
    PhaseContract::new(LifecyclePhase::Full, &[], 0),
    PhaseContract::new(LifecyclePhase::PacketFinalization, FULL_0_LIGHT_1, 0),
];

/// The common externally observable chunk lifecycle.
pub const LIFECYCLE: LifecycleSchedule = LifecycleSchedule::new(LIFECYCLE_PHASES);

/// The stable numeric decoration steps used by configured feature lists.
///
/// JSON and registry adapters still decode the external ordinal at the
/// boundary, but generation code can name the step it is dispatching. The
/// ordinal is part of the seed derivation, so it remains available through
/// [`Self::ordinal`] without making raw integers the primary representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum DecorationStep {
    RawGeneration = 0,
    Lakes = 1,
    LocalModifications = 2,
    UndergroundStructures = 3,
    SurfaceStructures = 4,
    Strongholds = 5,
    UndergroundOres = 6,
    UndergroundDecoration = 7,
    FluidSprings = 8,
    VegetalDecoration = 9,
    TopLayerModification = 10,
}

impl DecorationStep {
    #[must_use]
    pub const fn ordinal(self) -> i32 {
        self as i32
    }

    /// Decode the external numeric ordinal once, at a data boundary.
    #[must_use]
    pub const fn from_ordinal(ordinal: i32) -> Option<Self> {
        Some(match ordinal {
            0 => Self::RawGeneration,
            1 => Self::Lakes,
            2 => Self::LocalModifications,
            3 => Self::UndergroundStructures,
            4 => Self::SurfaceStructures,
            5 => Self::Strongholds,
            6 => Self::UndergroundOres,
            7 => Self::UndergroundDecoration,
            8 => Self::FluidSprings,
            9 => Self::VegetalDecoration,
            10 => Self::TopLayerModification,
            _ => return None,
        })
    }
}

/// Configured feature steps visited by the Overworld decoration dispatcher.
/// The absent stronghold step is intentional: it has its own structure path.
pub const OVERWORLD_DECORATION_STEPS: &[DecorationStep] = &[
    DecorationStep::RawGeneration,
    DecorationStep::Lakes,
    DecorationStep::LocalModifications,
    DecorationStep::UndergroundStructures,
    DecorationStep::SurfaceStructures,
    DecorationStep::UndergroundOres,
    DecorationStep::UndergroundDecoration,
    DecorationStep::FluidSprings,
    DecorationStep::VegetalDecoration,
];

/// Configured feature steps visited by the Nether's mixed decoration stream.
/// The schedule preserves the raw ordinals; interpretation of entries at each
/// ordinal remains in the Nether adapter.
pub const NETHER_DECORATION_STEPS: &[DecorationStep] = &[
    DecorationStep::LocalModifications,
    DecorationStep::SurfaceStructures,
    DecorationStep::UndergroundDecoration,
    DecorationStep::VegetalDecoration,
];

/// Candidate source chunks for a mutable neighbourhood dispatch.
///
/// This is deliberately separate from [`StageSchedule`]: admission order is a
/// scheduler concern, while the column stages below describe one source's
/// contents. A source order can therefore be tested and changed without
/// pretending that it changes the fill/biome/surface pass order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSchedule {
    dimension: Dimension,
    completion: SourceCompletion,
}

/// How a source window completes.
///
/// The offsets are part of the `Fixed` variant rather than a field on the
/// enclosing schedule. This makes it impossible to accidentally read a
/// centre-last (or any other) permutation from a dimension whose completion
/// is admission-dependent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceCompletion {
    /// The producer and replay comparator may use `offsets` directly.
    Fixed(&'static [(i32, i32)]),
    /// The completion order is derived from the admitted/resident state for
    /// this request; no one 3x3 permutation is valid for every request.
    AdmissionDependent,
}

/// Side length of one admission tile in the chunk wavefront.
pub const ADMISSION_TILE_SIDE: i32 = 16;

/// Horizontal source radius consumed by the mixed Nether feature pass.
pub const NETHER_FEATURE_SOURCE_RADIUS: i32 = 1;

/// Horizontal write radius covered by one Nether source completion.
pub const NETHER_FEATURE_WRITE_RADIUS: i32 = 2;

/// A requested target rectangle and the dependency halo admitted with it.
///
/// The request is the input to the mutable worldgen lifecycle.  It is kept
/// separate from [`StageSchedule`] because a request controls *which* source
/// columns become residents, while a stage schedule controls the passes inside
/// one source column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRequest {
    target_min_x: i32,
    target_max_x: i32,
    target_min_z: i32,
    target_max_z: i32,
    dependency_radius: i32,
}

impl ChunkRequest {
    /// Construct a rectangular request. `dependency_radius` expands the
    /// rectangle before admission; the expanded rectangle is the complete
    /// resident wavefront used by source completion.
    #[must_use]
    pub const fn new(
        target_min_x: i32,
        target_max_x: i32,
        target_min_z: i32,
        target_max_z: i32,
        dependency_radius: i32,
    ) -> Self {
        Self {
            target_min_x,
            target_max_x,
            target_min_z,
            target_max_z,
            dependency_radius,
        }
    }

    /// Construct the request used by the one-column production API.
    #[must_use]
    pub const fn single(cx: i32, cz: i32, dependency_radius: i32) -> Self {
        Self::new(cx, cx, cz, cz, dependency_radius)
    }

    /// The request's admitted chunk order: tile-z, tile-x, local-z,
    /// local-x. The minimum admitted coordinate anchors the tiles, so two
    /// otherwise identical source windows can have different orders when
    /// they belong to different requests.
    #[must_use]
    pub fn admission_order(self) -> Vec<(i32, i32)> {
        let wavefront = self.admission_wavefront();
        let min_x = wavefront.origin_x;
        let min_z = wavefront.origin_z;
        let max_x = self.target_max_x + self.dependency_radius;
        let max_z = self.target_max_z + self.dependency_radius;
        assert!(min_x <= max_x, "chunk request has an inverted x range");
        assert!(min_z <= max_z, "chunk request has an inverted z range");
        let width = i64::from(max_x) - i64::from(min_x) + 1;
        let height = i64::from(max_z) - i64::from(min_z) + 1;
        let capacity = usize::try_from(width * height)
            .expect("chunk request admission rectangle is too large");
        let tiles_x = (max_x - min_x).div_euclid(ADMISSION_TILE_SIDE) + 1;
        let tiles_z = (max_z - min_z).div_euclid(ADMISSION_TILE_SIDE) + 1;
        let mut order = Vec::with_capacity(capacity);
        for tile_z in 0..tiles_z {
            let z0 = min_z + tile_z * ADMISSION_TILE_SIDE;
            for tile_x in 0..tiles_x {
                let x0 = min_x + tile_x * ADMISSION_TILE_SIDE;
                for z in z0..=max_z.min(z0 + ADMISSION_TILE_SIDE - 1) {
                    for x in x0..=max_x.min(x0 + ADMISSION_TILE_SIDE - 1) {
                        order.push((x, z));
                    }
                }
            }
        }
        order
    }

    /// Derive the admission wavefront that owns this request's coordinate
    /// order. The wavefront origin is the minimum admitted chunk, not a global
    /// world-grid origin.
    #[must_use]
    pub const fn admission_wavefront(self) -> AdmissionWavefront {
        AdmissionWavefront {
            origin_x: self.target_min_x - self.dependency_radius,
            origin_z: self.target_min_z - self.dependency_radius,
        }
    }

    /// Return the source completion order for one target in this request.
    #[must_use]
    pub fn source_completion_order(self, center: (i32, i32)) -> SourceCompletionOrder {
        self.admission_wavefront().source_completion_order(center)
    }
}

/// The tiled admission wavefront derived from one [`ChunkRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionWavefront {
    origin_x: i32,
    origin_z: i32,
}

impl AdmissionWavefront {
    /// Construct a wavefront anchored at the first admitted chunk.
    #[must_use]
    pub const fn new(origin_x: i32, origin_z: i32) -> Self {
        Self { origin_x, origin_z }
    }

    /// Return the external scheduler's lexicographic key for one chunk.
    ///
    /// The tuple is named to keep the four transitions visible at call sites:
    /// tile-z, tile-x, local-z, then local-x.
    #[must_use]
    pub fn order_key(self, chunk: (i32, i32)) -> (i32, i32, i32, i32) {
        let tile_x = (chunk.0 - self.origin_x).div_euclid(ADMISSION_TILE_SIDE);
        let tile_z = (chunk.1 - self.origin_z).div_euclid(ADMISSION_TILE_SIDE);
        let local_x = (chunk.0 - self.origin_x).rem_euclid(ADMISSION_TILE_SIDE);
        let local_z = (chunk.1 - self.origin_z).rem_euclid(ADMISSION_TILE_SIDE);
        (tile_z, tile_x, local_z, local_x)
    }

    /// Sort the fixed three-by-three candidate set by this request's
    /// admission key. No global centre-first, centre-last, or axis-major
    /// permutation is implied: the request anchor decides the result.
    #[must_use]
    pub fn source_completion_order(self, center: (i32, i32)) -> SourceCompletionOrder {
        let mut offsets = *SOURCE_WINDOW_OFFSETS;
        offsets.sort_unstable_by_key(|&(dx, dz)| {
            self.order_key((center.0 + dx, center.1 + dz))
        });
        SourceCompletionOrder { offsets }
    }
}

/// The source offsets after one request's admission wavefront has ordered
/// their completion. The offsets remain relative to the target centre so the
/// production dispatcher and lifecycle comparator cannot disagree about the
/// same source window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceCompletionOrder {
    offsets: [(i32, i32); 9],
}

impl SourceCompletionOrder {
    /// Consume the order into its fixed-size offset array.
    #[must_use]
    pub const fn offsets(self) -> [(i32, i32); 9] {
        self.offsets
    }
}

impl SourceSchedule {
    pub const fn fixed(dimension: Dimension, offsets: &'static [(i32, i32)]) -> Self {
        Self {
            dimension,
            completion: SourceCompletion::Fixed(offsets),
        }
    }

    pub const fn admission_dependent(dimension: Dimension) -> Self {
        Self {
            dimension,
            completion: SourceCompletion::AdmissionDependent,
        }
    }

    #[must_use]
    pub const fn dimension(self) -> Dimension {
        self.dimension
    }

    #[must_use]
    pub const fn completion(self) -> SourceCompletion {
        self.completion
    }

    /// Resolve this schedule's source order for one admitted request.
    ///
    /// Fixed schedules copy their declared candidate offsets. Admission-based
    /// schedules derive the order from the request's wavefront, so callers do
    /// not need a second three-by-three loop (or a coordinate-specific
    /// permutation) in a production dispatcher.
    #[must_use]
    pub fn order_for(self, request: ChunkRequest, center: (i32, i32)) -> [(i32, i32); 9] {
        match self.completion {
            SourceCompletion::Fixed(offsets) => {
                assert_eq!(offsets.len(), 9, "source schedule must declare nine candidates");
                std::array::from_fn(|index| offsets[index])
            }
            SourceCompletion::AdmissionDependent => {
                request.source_completion_order(center).offsets()
            }
        }
    }
}

pub(crate) const OVERWORLD_SOURCE_OFFSETS: &[(i32, i32); 9] = &[
    (-1, -1),
    (-1, 0),
    (-1, 1),
    (0, -1),
    (0, 0),
    (0, 1),
    (1, -1),
    (1, 0),
    (1, 1),
];

// Nether uses the same candidate neighbourhood; only its admission key is
// dynamic. Keeping the candidate set named independently prevents callers from
// mistaking the Overworld's fixed order for Nether completion order.
const SOURCE_WINDOW_OFFSETS: &[(i32, i32); 9] = OVERWORLD_SOURCE_OFFSETS;

pub(crate) const END_SOURCE_OFFSETS: &[(i32, i32); 9] = OVERWORLD_SOURCE_OFFSETS;

/// Current production source order for the Overworld mutable feature window.
pub const OVERWORLD_SOURCES: SourceSchedule =
    SourceSchedule::fixed(Dimension::Overworld, OVERWORLD_SOURCE_OFFSETS);
/// The Nether's mutable feature window. Its completion order is derived from
/// the admitted/resident state; a fixed centre-last or centre-first permutation
/// is not valid for every request.
pub const NETHER_SOURCES: SourceSchedule =
    SourceSchedule::admission_dependent(Dimension::Nether);
/// End source order used when applying the three-by-three decoration region.
pub const END_SOURCES: SourceSchedule =
    SourceSchedule::fixed(Dimension::End, END_SOURCE_OFFSETS);

/// One dimension's configured feature-step sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureSchedule {
    dimension: Dimension,
    steps: &'static [DecorationStep],
}

impl FeatureSchedule {
    pub const fn new(dimension: Dimension, steps: &'static [DecorationStep]) -> Self {
        Self { dimension, steps }
    }

    #[must_use]
    pub const fn dimension(self) -> Dimension {
        self.dimension
    }

    #[must_use]
    pub const fn steps(self) -> &'static [DecorationStep] {
        self.steps
    }
}

/// Feature steps consumed by the Overworld dispatcher.
pub const OVERWORLD_FEATURES: FeatureSchedule =
    FeatureSchedule::new(Dimension::Overworld, OVERWORLD_DECORATION_STEPS);
/// Feature steps consumed by the Nether dispatcher.
pub const NETHER_FEATURES: FeatureSchedule =
    FeatureSchedule::new(Dimension::Nether, NETHER_DECORATION_STEPS);

/// The immutable description of one dimension's ordered passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StageSchedule {
    dimension: Dimension,
    stages: &'static [ColumnStage],
    shaped_boundary: ColumnStage,
    gates: &'static [StageGate],
}

impl StageSchedule {
    /// Construct a schedule for a static stage slice.
    pub const fn new(
        dimension: Dimension,
        stages: &'static [ColumnStage],
        shaped_boundary: ColumnStage,
    ) -> Self {
        Self {
            dimension,
            stages,
            shaped_boundary,
            gates: &[],
        }
    }

    /// Construct a schedule with explicit declarations for data-dependent
    /// stage gates. Every gate must name a stage in `stages`; `validate`
    /// checks that relationship and rejects duplicate declarations.
    pub const fn with_gates(
        dimension: Dimension,
        stages: &'static [ColumnStage],
        shaped_boundary: ColumnStage,
        gates: &'static [StageGate],
    ) -> Self {
        Self {
            dimension,
            stages,
            shaped_boundary,
            gates,
        }
    }

    #[must_use]
    pub const fn dimension(self) -> Dimension {
        self.dimension
    }

    /// Wrap this schedule in the production-facing typed pipeline contract.
    #[must_use]
    pub const fn pipeline(self) -> DimensionPipeline {
        DimensionPipeline::new(self)
    }

    #[must_use]
    pub const fn stages(self) -> &'static [ColumnStage] {
        self.stages
    }

    /// First stage not represented by this dimension's shaped-column value.
    ///
    /// The boundary is dimension-specific: Overworld and End shaped columns
    /// already include structure placement, while Nether placement belongs to
    /// source completion. Callers must ask the schedule instead of assigning a
    /// universal meaning to a `Shaped` label.
    #[must_use]
    pub const fn shaped_boundary(self) -> ColumnStage {
        self.shaped_boundary
    }

    /// The option gates declared for this dimension's stages.
    #[must_use]
    pub const fn gates(self) -> &'static [StageGate] {
        self.gates
    }

    /// Return the option gate attached to a stage, if any.
    #[must_use]
    pub const fn gate_for(self, stage: ColumnStage) -> Option<StageOption> {
        let mut index = 0;
        while index < self.gates.len() {
            let gate = self.gates[index];
            if gate.stage() as u8 == stage as u8 {
                return Some(gate.option());
            }
            index += 1;
        }
        None
    }

    /// Whether a stage should execute for the selected options.
    ///
    /// A stage without a gate is always enabled.  Gated stages remain in the
    /// cursor and frontier when disabled; only their operation is skipped.
    #[must_use]
    pub const fn stage_enabled(self, stage: ColumnStage, options: PipelineOptions) -> bool {
        match self.gate_for(stage) {
            Some(option) => options.enables(option),
            None => true,
        }
    }

    /// The terminal pass represented by a public generation target.
    #[must_use]
    pub const fn target_stage(self, target: GenerationTarget) -> ColumnStage {
        let index = match target {
            GenerationTarget::Shaped => self.shaped_boundary_index() - 1,
            GenerationTarget::Full => self.stages.len() - 1,
        };
        self.stages[index]
    }

    /// The canonical stage prefix represented by a public generation target.
    ///
    /// `Shaped` stops immediately before `shaped_boundary`; `Full` includes
    /// every pass. The returned slice is borrowed directly from the schedule,
    /// so callers cannot accidentally mutate or reorder the contract.
    #[must_use]
    pub fn stages_for(self, target: GenerationTarget) -> &'static [ColumnStage] {
        let end = match target {
            GenerationTarget::Shaped => self.shaped_boundary_index(),
            GenerationTarget::Full => self.stages.len(),
        };
        &self.stages[..end]
    }

    /// The canonical prefix for a logical incremental generation level.
    #[must_use]
    pub fn stages_for_level(self, level: GenerationLevel) -> Option<&'static [ColumnStage]> {
        let terminal = level.terminal_stage(self)?;
        let end = self.index_of(terminal)? + 1;
        Some(&self.stages[..end])
    }

    /// Return the descriptor for one stage in this schedule.
    ///
    /// The descriptor table is dimension-specific even though the stage enum
    /// is shared for compatibility with older callers.  An absent stage is
    /// rejected here, so a Nether caller cannot accidentally request the
    /// Overworld-only top-layer pass (and an End caller cannot request
    /// carvers).
    #[must_use]
    pub const fn descriptor(self, stage: ColumnStage) -> Option<StageDescriptor> {
        if self.index_of(stage).is_none() {
            return None;
        }
        match self.dimension {
            Dimension::End => end_stage_descriptor(stage),
            Dimension::Overworld => overworld_stage_descriptor(stage),
            Dimension::Nether => nether_stage_descriptor(stage),
        }
    }

    /// Return descriptors in exactly the schedule's stage order.
    ///
    /// This method derives order from `stages`; it is intentionally not backed
    /// by a second descriptor-order slice.
    #[must_use]
    pub fn descriptors(self) -> Vec<StageDescriptor> {
        self.stages
            .iter()
            .copied()
            .filter_map(|stage| self.descriptor(stage))
            .collect()
    }

    /// Index where a shaped-column producer must stop its cursor.
    #[must_use]
    pub const fn shaped_boundary_index(self) -> usize {
        match self.index_of(self.shaped_boundary) {
            Some(index) => index,
            None => panic!("shaped boundary must belong to the dimension schedule"),
        }
    }

    /// Return the first position of `stage`, if this dimension runs it.
    #[must_use]
    pub const fn index_of(self, stage: ColumnStage) -> Option<usize> {
        let mut index = 0;
        while index < self.stages.len() {
            if self.stages[index] as u8 == stage as u8 {
                return Some(index);
            }
            index += 1;
        }
        None
    }

    /// Start checking from the beginning of the schedule.
    #[must_use]
    pub const fn cursor(self) -> StageCursor {
        StageCursor {
            schedule: self,
            next: 0,
        }
    }

    /// Start checking at a known prefix boundary.
    ///
    /// This is used by APIs that return a cached prefix (for example, terrain
    /// through structures) and resume with decoration in a later call.
    #[must_use]
    pub const fn cursor_at(self, next: usize) -> StageCursor {
        assert!(next <= self.stages.len());
        StageCursor {
            schedule: self,
            next,
        }
    }

    /// Start the canonical executor at the beginning of this schedule.
    ///
    /// Unlike the older cursor guard, the executor is the production-facing
    /// entry point for migrated generators: it checks the order in release
    /// builds too, and can optionally record the typed pass keys for a
    /// diagnostic or parity trace. A trace is opt-in, so ordinary generation
    /// does not allocate or call a tracing callback.
    #[must_use]
    pub const fn executor(self) -> StageExecutor<'static> {
        StageExecutor {
            schedule: self,
            next: 0,
            trace: None,
            options: PipelineOptions::ALL,
        }
    }

    /// Start the production executor with explicit option gates.
    #[must_use]
    pub const fn executor_with_options(self, options: PipelineOptions) -> StageExecutor<'static> {
        StageExecutor {
            schedule: self,
            next: 0,
            trace: None,
            options,
        }
    }

    /// Start the canonical executor and append each entered pass to `trace`.
    #[must_use]
    pub fn executor_with_trace<'trace>(
        self,
        trace: &'trace mut Vec<StageKey>,
    ) -> StageExecutor<'trace> {
        StageExecutor {
            schedule: self,
            next: 0,
            trace: Some(trace),
            options: PipelineOptions::ALL,
        }
    }

    /// Start a traced executor with explicit option gates.
    #[must_use]
    pub fn executor_with_options_and_trace<'trace>(
        self,
        options: PipelineOptions,
        trace: &'trace mut Vec<StageKey>,
    ) -> StageExecutor<'trace> {
        StageExecutor {
            schedule: self,
            next: 0,
            trace: Some(trace),
            options,
        }
    }

    /// Start the canonical executor at a cached-prefix boundary.
    #[must_use]
    pub const fn executor_at(self, next: usize) -> StageExecutor<'static> {
        assert!(next <= self.stages.len());
        StageExecutor {
            schedule: self,
            next,
            trace: None,
            options: PipelineOptions::ALL,
        }
    }

    /// Start an executor at a cached prefix with explicit option gates.
    #[must_use]
    pub const fn executor_at_with_options(
        self,
        next: usize,
        options: PipelineOptions,
    ) -> StageExecutor<'static> {
        assert!(next <= self.stages.len());
        StageExecutor {
            schedule: self,
            next,
            trace: None,
            options,
        }
    }

    /// Start a traced executor at a cached-prefix boundary.
    #[must_use]
    pub fn executor_at_with_trace<'trace>(
        self,
        next: usize,
        trace: &'trace mut Vec<StageKey>,
    ) -> StageExecutor<'trace> {
        assert!(next <= self.stages.len());
        StageExecutor {
            schedule: self,
            next,
            trace: Some(trace),
            options: PipelineOptions::ALL,
        }
    }

    /// Validate the static table itself.  This is kept as a runtime helper so
    /// tests can report a useful dimension when a table is edited.
    pub fn validate(self) {
        assert!(!self.stages.is_empty(), "{} schedule is empty", self.dimension_name());
        let mut left = 0;
        while left < self.stages.len() {
            let mut right = left + 1;
            while right < self.stages.len() {
                assert_ne!(
                    self.stages[left], self.stages[right],
                    "{} schedule repeats {:?}",
                    self.dimension_name(),
                    self.stages[left]
                );
                right += 1;
            }
            left += 1;
        }
        assert!(
            self.shaped_boundary_index() > 0,
            "{} shaped prefix is empty",
            self.dimension_name()
        );
        let mut left = 0;
        while left < self.gates.len() {
            assert!(
                self.index_of(self.gates[left].stage()).is_some(),
                "{} gate names a stage outside its schedule: {:?}",
                self.dimension_name(),
                self.gates[left].stage()
            );
            let mut right = left + 1;
            while right < self.gates.len() {
                assert_ne!(
                    self.gates[left].stage(),
                    self.gates[right].stage(),
                    "{} schedule repeats a gate for {:?}",
                    self.dimension_name(),
                    self.gates[left].stage()
                );
                right += 1;
            }
            left += 1;
        }
        let mut index = 0;
        while index < self.stages.len() {
            let stage = self.stages[index];
            let descriptor = self
                .descriptor(stage)
                .expect("dimension schedule must describe every named stage");
            assert_eq!(
                descriptor.key(),
                StageKey::new(self.dimension, stage),
                "descriptor key must match its central stage"
            );
            for prerequisite in descriptor.prerequisites() {
                let prerequisite_index = self
                    .index_of(prerequisite.stage())
                    .expect("stage prerequisite must name a scheduled pass");
                assert_eq!(
                    prerequisite.dimension(),
                    self.dimension,
                    "stage prerequisite must stay in its dimension"
                );
                assert!(
                    prerequisite_index < index,
                    "stage prerequisite {:?} must precede {:?}",
                    prerequisite.stage(),
                    stage
                );
            }
            index += 1;
        }
    }

    fn dimension_name(self) -> &'static str {
        match self.dimension {
            Dimension::Overworld => "overworld",
            Dimension::Nether => "nether",
            Dimension::End => "end",
        }
    }
}

/// The executable, dimension-specific world-generation contract.
///
/// `StageSchedule` remains a small compatibility value for callers that only
/// need the ordered stage names.  A `DimensionPipeline` is the production
/// boundary: it validates that every scheduled stage has a descriptor, owns
/// option-gated executors, and creates frontiers with the same dimension and
/// schedule identity.  There is no second order list here; all prefixes and
/// executors are derived from the schedule's single `stages` slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DimensionPipeline {
    schedule: StageSchedule,
}

impl DimensionPipeline {
    /// Construct a pipeline around a central schedule.
    ///
    /// The public constructor is useful for versioned extensions that build a
    /// schedule from generated data.  Call [`Self::validate`] before exposing
    /// such a pipeline to a production dispatcher.
    #[must_use]
    pub const fn new(schedule: StageSchedule) -> Self {
        Self { schedule }
    }

    #[must_use]
    pub const fn schedule(self) -> StageSchedule {
        self.schedule
    }

    #[must_use]
    pub const fn dimension(self) -> Dimension {
        self.schedule.dimension()
    }

    #[must_use]
    pub const fn stages(self) -> &'static [ColumnStage] {
        self.schedule.stages()
    }

    #[must_use]
    pub const fn gates(self) -> &'static [StageGate] {
        self.schedule.gates()
    }

    #[must_use]
    pub const fn descriptor(self, stage: ColumnStage) -> Option<StageDescriptor> {
        self.schedule.descriptor(stage)
    }

    #[must_use]
    pub fn descriptors(self) -> Vec<StageDescriptor> {
        self.schedule.descriptors()
    }

    /// Return a compact identity for persisted or cached generation state.
    #[must_use]
    pub const fn identity(self, options: PipelineOptions) -> PipelineIdentity {
        PipelineIdentity {
            dimension: self.dimension(),
            schedule_version: STAGE_SCHEDULE_VERSION,
            options,
        }
    }

    /// Validate the complete dimension contract before it is used.
    pub fn validate(self) {
        self.schedule.validate();
        assert_eq!(
            self.descriptors().len(),
            self.stages().len(),
            "{} pipeline must describe every scheduled stage",
            self.dimension_name()
        );
    }

    /// Start the release-checked executor with all default options enabled.
    #[must_use]
    pub const fn executor(self) -> StageExecutor<'static> {
        self.schedule.executor()
    }

    /// Start an executor with explicit option gates.
    #[must_use]
    pub const fn executor_with_options(self, options: PipelineOptions) -> StageExecutor<'static> {
        self.schedule.executor_with_options(options)
    }

    /// Resume an executor after a cached prefix.
    #[must_use]
    pub const fn executor_at(self, next: usize) -> StageExecutor<'static> {
        self.schedule.executor_at(next)
    }

    /// Resume an executor after a cached prefix with explicit options.
    #[must_use]
    pub const fn executor_at_with_options(
        self,
        next: usize,
        options: PipelineOptions,
    ) -> StageExecutor<'static> {
        self.schedule.executor_at_with_options(next, options)
    }

    /// Create a resumable frontier using this pipeline's identity.
    #[must_use]
    pub fn frontier(self, coordinate: (i32, i32), options: PipelineOptions) -> StageFrontier {
        StageFrontier::with_pipeline(self, coordinate, options)
    }

    fn dimension_name(self) -> &'static str {
        match self.dimension() {
            Dimension::Overworld => "overworld",
            Dimension::Nether => "nether",
            Dimension::End => "end",
        }
    }
}

/// Compact identity carried by a resumable generation frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PipelineIdentity {
    dimension: Dimension,
    schedule_version: u32,
    options: PipelineOptions,
}

impl PipelineIdentity {
    #[must_use]
    pub const fn dimension(self) -> Dimension {
        self.dimension
    }

    #[must_use]
    pub const fn schedule_version(self) -> u32 {
        self.schedule_version
    }

    #[must_use]
    pub const fn options(self) -> PipelineOptions {
        self.options
    }
}

/// The production executor for one dimension's typed stage schedule.
///
/// A cached prefix is represented by constructing this executor at the first
/// unfinished index; the prefix is still validated by the caller's persisted
/// frontier, while every newly executed stage is checked against the same
/// central order. `trace` is deliberately a borrowed optional sink rather
/// than a logger: tracing a hot generation path is explicit and cannot
/// accidentally print large implementation details to normal output.
#[derive(Debug)]
pub struct StageExecutor<'trace> {
    schedule: StageSchedule,
    next: usize,
    trace: Option<&'trace mut Vec<StageKey>>,
    options: PipelineOptions,
}

impl<'trace> StageExecutor<'trace> {
    /// Mark one pass complete and return its result.
    #[inline]
    pub fn run<T>(&mut self, stage: ColumnStage, operation: impl FnOnce() -> T) -> T {
        self.enter(stage);
        operation()
    }

    /// Enter a stage and run its operation only when its central option gate
    /// is enabled. Disabled stages still advance the executor, preserving the
    /// same frontier shape for every option set.
    #[inline]
    pub fn run_if<T>(
        &mut self,
        stage: ColumnStage,
        operation: impl FnOnce() -> T,
    ) -> Option<T> {
        self.enter(stage);
        self.schedule
            .stage_enabled(stage, self.options)
            .then(operation)
    }

    #[must_use]
    pub const fn options(&self) -> PipelineOptions {
        self.options
    }

    #[must_use]
    pub const fn stage_enabled(&self, stage: ColumnStage) -> bool {
        self.schedule.stage_enabled(stage, self.options)
    }

    /// Mark one pass complete after its operation has already run.
    #[inline]
    pub fn enter(&mut self, stage: ColumnStage) {
        let expected = self.schedule.stages.get(self.next).copied();
        assert!(
            self.schedule.descriptor(stage).is_some(),
            "{} generation stage is not part of its dimension pipeline: {:?}",
            self.schedule.dimension_name(),
            stage
        );
        assert_eq!(
            expected,
            Some(stage),
            "{} generation stage out of order: expected {:?}, entered {:?}",
            self.schedule.dimension_name(),
            expected,
            stage
        );
        if let Some(trace) = self.trace.as_deref_mut() {
            trace.push(StageKey::new(self.schedule.dimension, stage));
        }
        self.next += 1;
    }

    /// Assert that all passes in the selected schedule segment were completed.
    #[inline]
    pub fn finish(self) {
        assert_eq!(
            self.next,
            self.schedule.stages.len(),
            "{} generation schedule stopped before {:?}",
            self.schedule.dimension_name(),
            self.schedule.stages.get(self.next)
        );
    }

    /// Assert that a cached-prefix boundary was reached.
    #[inline]
    pub fn finish_prefix(self, boundary: usize) {
        assert_eq!(
            self.next, boundary,
            "{} generation prefix stopped at {}, expected {}",
            self.schedule.dimension_name(),
            self.next,
            boundary
        );
    }
}

/// A small debug guard for code whose stages are split across helper methods.
#[derive(Debug, Clone, Copy)]
pub struct StageCursor {
    schedule: StageSchedule,
    next: usize,
}

impl StageCursor {
    /// Mark one pass complete and return its result.
    #[inline]
    pub fn run<T>(&mut self, stage: ColumnStage, operation: impl FnOnce() -> T) -> T {
        self.enter(stage);
        operation()
    }

    /// Mark one pass complete after its operation has already run.
    #[inline]
    pub fn enter(&mut self, stage: ColumnStage) {
        let expected = self.schedule.stages.get(self.next).copied();
        debug_assert_eq!(
            expected,
            Some(stage),
            "{} generation stage out of order: expected {:?}, entered {:?}",
            self.schedule.dimension_name(),
            expected,
            stage
        );
        self.next += 1;
    }

    /// Assert that all passes in the selected schedule segment were completed.
    #[inline]
    pub fn finish(self) {
        debug_assert_eq!(
            self.next,
            self.schedule.stages.len(),
            "{} generation schedule stopped before {:?}",
            self.schedule.dimension_name(),
            self.schedule.stages.get(self.next)
        );
    }

    /// Assert that a cached-prefix boundary was reached.
    ///
    /// The production generators use one cursor for the immutable prefix and
    /// a second cursor for the suffix when the prefix is memoised. Keeping the
    /// boundary explicit prevents a cached result from silently skipping a
    /// named pass while still allowing the prefix and suffix to be computed by
    /// different calls.
    #[inline]
    pub fn finish_prefix(self, boundary: usize) {
        debug_assert_eq!(
            self.next, boundary,
            "{} generation prefix stopped at {}, expected {}",
            self.schedule.dimension_name(),
            self.next,
            boundary
        );
    }
}

/// Version of the typed descriptor/frontier contract.
pub const STAGE_SCHEDULE_VERSION: u32 = 1;

/// The durable evidence recorded when one stage completes for one chunk.
///
/// The generator owns the actual products. A frontier stores the fingerprints
/// and typed retention declarations that let a caller decide whether those
/// products can be reused after a pause or process restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRecord {
    key: StageKey,
    input_fingerprint: [u8; 32],
    output_fingerprint: [u8; 32],
    retained_products: Vec<ResourceKey>,
    retained_sidecars: Vec<SidecarKey>,
    executor_version: u32,
}

impl StageRecord {
    /// Create a record from a descriptor after the executor has produced its
    /// fingerprints. Retention declarations come from the central descriptor,
    /// not from an executor-specific list.
    #[must_use]
    pub fn for_descriptor(
        descriptor: StageDescriptor,
        input_fingerprint: [u8; 32],
        output_fingerprint: [u8; 32],
        executor_version: u32,
    ) -> Self {
        Self {
            key: descriptor.key(),
            input_fingerprint,
            output_fingerprint,
            retained_products: descriptor.outputs().to_vec(),
            retained_sidecars: descriptor.retained_sidecars().to_vec(),
            executor_version,
        }
    }

    /// Rehydrate a record whose retention declarations were persisted by a
    /// caller. `StageFrontier::from_records` validates the declarations before
    /// accepting it into a frontier.
    #[must_use]
    pub fn new(
        key: StageKey,
        input_fingerprint: [u8; 32],
        output_fingerprint: [u8; 32],
        retained_products: Vec<ResourceKey>,
        retained_sidecars: Vec<SidecarKey>,
        executor_version: u32,
    ) -> Self {
        Self {
            key,
            input_fingerprint,
            output_fingerprint,
            retained_products,
            retained_sidecars,
            executor_version,
        }
    }

    #[must_use]
    pub const fn key(&self) -> StageKey {
        self.key
    }

    #[must_use]
    pub const fn input_fingerprint(&self) -> [u8; 32] {
        self.input_fingerprint
    }

    #[must_use]
    pub const fn output_fingerprint(&self) -> [u8; 32] {
        self.output_fingerprint
    }

    #[must_use]
    pub fn retained_products(&self) -> &[ResourceKey] {
        &self.retained_products
    }

    #[must_use]
    pub fn retained_sidecars(&self) -> &[SidecarKey] {
        &self.retained_sidecars
    }

    #[must_use]
    pub const fn executor_version(&self) -> u32 {
        self.executor_version
    }
}

/// Why a persisted stage record cannot be admitted to a frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontierError {
    /// The frontier's descriptor version differs from the record's version.
    ScheduleVersionMismatch { expected: u32, found: u32 },
    /// The record belongs to another dimension.
    ForeignDimension { expected: Dimension, found: Dimension },
    /// The record names no stage in the selected schedule.
    UnsupportedStage(StageKey),
    /// A record was supplied after the frontier had already advanced past it.
    OutOfOrder {
        expected: Option<StageKey>,
        found: StageKey,
    },
    /// The same typed product was declared more than once.
    DuplicateProduct { stage: StageKey, product: ResourceKey },
    /// A required product was not retained by the stage record.
    MissingProduct { stage: StageKey, product: ResourceKey },
    /// A required sidecar was not retained by the stage record.
    MissingSidecar { stage: StageKey, sidecar: SidecarKey },
    /// A stage consumes a product that no earlier record retained.
    MissingInput { stage: StageKey, product: ResourceKey },
}

/// Typed, resumable completion state for one chunk.
///
/// Records are accepted only in the exact central schedule order. This
/// intentionally gives a chunk a single monotone frontier; a future lifecycle
/// graph with independent lighting branches can use a separate frontier rather
/// than weakening this column-generation invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageFrontier {
    pipeline: DimensionPipeline,
    options: PipelineOptions,
    schedule_version: u32,
    coordinate: (i32, i32),
    records: Vec<StageRecord>,
}

impl StageFrontier {
    /// Start an empty frontier for one chunk.
    #[must_use]
    pub fn new(schedule: StageSchedule, coordinate: (i32, i32)) -> Self {
        Self::with_pipeline(schedule.pipeline(), coordinate, PipelineOptions::ALL)
    }

    /// Start an empty frontier with an explicit pipeline identity.
    #[must_use]
    pub fn with_pipeline(
        pipeline: DimensionPipeline,
        coordinate: (i32, i32),
        options: PipelineOptions,
    ) -> Self {
        pipeline.validate();
        Self {
            pipeline,
            options,
            schedule_version: STAGE_SCHEDULE_VERSION,
            coordinate,
            records: Vec::new(),
        }
    }

    /// Restore a frontier from persisted records, validating every record as
    /// an atomic prefix. No partially restored state is returned on failure.
    pub fn from_records(
        schedule: StageSchedule,
        coordinate: (i32, i32),
        schedule_version: u32,
        records: Vec<StageRecord>,
    ) -> Result<Self, FrontierError> {
        Self::from_records_with_options(
            schedule,
            coordinate,
            schedule_version,
            PipelineOptions::ALL,
            records,
        )
    }

    /// Restore a frontier while checking its option-gated pipeline identity.
    pub fn from_records_with_options(
        schedule: StageSchedule,
        coordinate: (i32, i32),
        schedule_version: u32,
        options: PipelineOptions,
        records: Vec<StageRecord>,
    ) -> Result<Self, FrontierError> {
        if schedule_version != STAGE_SCHEDULE_VERSION {
            return Err(FrontierError::ScheduleVersionMismatch {
                expected: STAGE_SCHEDULE_VERSION,
                found: schedule_version,
            });
        }
        let mut frontier = Self::with_pipeline(schedule.pipeline(), coordinate, options);
        for record in records {
            frontier.commit(record)?;
        }
        Ok(frontier)
    }

    #[must_use]
    pub const fn schedule(&self) -> StageSchedule {
        self.pipeline.schedule()
    }

    #[must_use]
    pub const fn pipeline(&self) -> DimensionPipeline {
        self.pipeline
    }

    #[must_use]
    pub const fn options(&self) -> PipelineOptions {
        self.options
    }

    #[must_use]
    pub const fn identity(&self) -> PipelineIdentity {
        self.pipeline.identity(self.options)
    }

    #[must_use]
    pub const fn schedule_version(&self) -> u32 {
        self.schedule_version
    }

    #[must_use]
    pub const fn coordinate(&self) -> (i32, i32) {
        self.coordinate
    }

    #[must_use]
    pub fn records(&self) -> &[StageRecord] {
        &self.records
    }

    /// The next stage that must be executed, if the target is incomplete.
    #[must_use]
    pub fn next_stage(&self) -> Option<StageKey> {
        self.pipeline
            .schedule()
            .stages()
            .get(self.records.len())
            .copied()
            .map(|stage| StageKey::new(self.pipeline.dimension(), stage))
    }

    /// The next typed descriptor that can be executed.
    #[must_use]
    pub fn resume_stage(&self) -> Option<StageDescriptor> {
        let stage = self.next_stage()?.stage();
        self.pipeline.descriptor(stage)
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.records.len() == self.pipeline.stages().len()
    }

    /// Whether the frontier contains the complete prefix ending at `stage`.
    #[must_use]
    pub fn is_complete_through(&self, stage: ColumnStage) -> bool {
        let Some(index) = self.pipeline.schedule().index_of(stage) else {
            return false;
        };
        self.records.len() > index
    }

    /// The compact completed-stage mask used by a scheduler or cache index.
    ///
    /// The detailed records remain available for validation and resume; this
    /// mask is a cheap summary for admission and never replaces those records.
    #[must_use]
    pub fn completed_stage_mask(&self) -> u16 {
        self.records.iter().fold(0u16, |mask, record| {
            let index = self
                .pipeline
                .schedule()
                .index_of(record.key().stage())
                .unwrap_or(0);
            mask | (1u16 << index)
        })
    }

    /// Whether this frontier has reached the requested logical level.
    #[must_use]
    pub fn is_complete_at_level(&self, level: GenerationLevel) -> bool {
        level
            .terminal_stage(self.pipeline.schedule())
            .is_some_and(|stage| self.is_complete_through(stage))
    }

    /// The highest logical level whose entire prefix is retained.
    #[must_use]
    pub fn highest_level(&self) -> Option<GenerationLevel> {
        [
            GenerationLevel::Output,
            GenerationLevel::Decorated,
            GenerationLevel::Structures,
            GenerationLevel::Terrain,
        ]
        .into_iter()
        .find(|&level| self.is_complete_at_level(level))
    }

    /// Commit one stage using the descriptor's typed retention declaration.
    pub fn commit_stage(
        &mut self,
        stage: ColumnStage,
        input_fingerprint: [u8; 32],
        output_fingerprint: [u8; 32],
        executor_version: u32,
    ) -> Result<(), FrontierError> {
        let Some(descriptor) = self.pipeline.descriptor(stage) else {
            return Err(FrontierError::UnsupportedStage(StageKey::new(
                self.pipeline.dimension(),
                stage,
            )));
        };
        self.commit(StageRecord::for_descriptor(
            descriptor,
            input_fingerprint,
            output_fingerprint,
            executor_version,
        ))
    }

    /// Atomically admit one persisted record after checking order, products,
    /// sidecars, and prior retained inputs.
    pub fn commit(&mut self, record: StageRecord) -> Result<(), FrontierError> {
        let key = record.key();
        if key.dimension() != self.pipeline.dimension() {
            return Err(FrontierError::ForeignDimension {
                expected: self.pipeline.dimension(),
                found: key.dimension(),
            });
        }
        let Some(descriptor) = self.pipeline.descriptor(key.stage()) else {
            return Err(FrontierError::UnsupportedStage(key));
        };
        if self.next_stage() != Some(key) {
            return Err(FrontierError::OutOfOrder {
                expected: self.next_stage(),
                found: key,
            });
        }

        for (index, product) in record.retained_products.iter().enumerate() {
            if record.retained_products[..index].contains(product) {
                return Err(FrontierError::DuplicateProduct {
                    stage: key,
                    product: *product,
                });
            }
        }
        for product in descriptor.outputs() {
            if !record.retained_products.contains(product) {
                return Err(FrontierError::MissingProduct {
                    stage: key,
                    product: *product,
                });
            }
        }
        for sidecar in descriptor.retained_sidecars() {
            if !record.retained_sidecars.contains(sidecar) {
                return Err(FrontierError::MissingSidecar {
                    stage: key,
                    sidecar: *sidecar,
                });
            }
        }

        for input in descriptor.inputs() {
            if !self
                .records
                .iter()
                .any(|prior| prior.retained_products.contains(input))
            {
                return Err(FrontierError::MissingInput {
                    stage: key,
                    product: *input,
                });
            }
        }
        self.records.push(record);
        Ok(())
    }
}

const OVERWORLD_STAGES: &[ColumnStage] = &[
    ColumnStage::StructureStarts,
    ColumnStage::StructureReferences,
    ColumnStage::StructureInfluence,
    ColumnStage::Fill,
    ColumnStage::Biomes,
    ColumnStage::Surface,
    ColumnStage::Materialize,
    ColumnStage::Carvers,
    ColumnStage::StructurePlacement,
    ColumnStage::Features,
    ColumnStage::TopLayer,
    ColumnStage::Output,
];

const NETHER_STAGES: &[ColumnStage] = &[
    ColumnStage::StructureStarts,
    ColumnStage::StructureReferences,
    ColumnStage::StructureInfluence,
    ColumnStage::Fill,
    ColumnStage::Biomes,
    ColumnStage::Surface,
    ColumnStage::Materialize,
    ColumnStage::Carvers,
    ColumnStage::StructurePlacement,
    ColumnStage::Features,
    ColumnStage::Output,
];

const END_STAGES: &[ColumnStage] = &[
    ColumnStage::Fill,
    ColumnStage::Biomes,
    ColumnStage::Surface,
    ColumnStage::Materialize,
    ColumnStage::StructureStarts,
    ColumnStage::StructurePlacement,
    ColumnStage::Features,
    ColumnStage::Output,
];

/// The complete Overworld order.
const OVERWORLD_GATES: &[StageGate] = &[
    StageGate::new(ColumnStage::StructureStarts, StageOption::Structures),
    StageGate::new(ColumnStage::StructureReferences, StageOption::Structures),
    StageGate::new(ColumnStage::StructureInfluence, StageOption::Structures),
    StageGate::new(ColumnStage::StructurePlacement, StageOption::Structures),
    StageGate::new(ColumnStage::Features, StageOption::Decorations),
    StageGate::new(ColumnStage::TopLayer, StageOption::Decorations),
];

const NETHER_GATES: &[StageGate] = &[
    StageGate::new(ColumnStage::StructureStarts, StageOption::Structures),
    StageGate::new(ColumnStage::StructureReferences, StageOption::Structures),
    StageGate::new(ColumnStage::StructureInfluence, StageOption::Structures),
    StageGate::new(ColumnStage::StructurePlacement, StageOption::Structures),
    StageGate::new(ColumnStage::Features, StageOption::Decorations),
];

const END_GATES: &[StageGate] = &[
    StageGate::new(ColumnStage::StructureStarts, StageOption::Structures),
    StageGate::new(ColumnStage::StructurePlacement, StageOption::Structures),
    StageGate::new(ColumnStage::Features, StageOption::Decorations),
];

pub const OVERWORLD: StageSchedule = StageSchedule::with_gates(
    Dimension::Overworld,
    OVERWORLD_STAGES,
    ColumnStage::Features,
    OVERWORLD_GATES,
);
/// The complete Nether order.
pub const NETHER: StageSchedule = StageSchedule::with_gates(
    Dimension::Nether,
    NETHER_STAGES,
    ColumnStage::StructurePlacement,
    NETHER_GATES,
);
/// The complete End order.
pub const END: StageSchedule = StageSchedule::with_gates(
    Dimension::End,
    END_STAGES,
    ColumnStage::Features,
    END_GATES,
);

/// The executable Overworld pipeline.
pub const OVERWORLD_PIPELINE: DimensionPipeline = DimensionPipeline::new(OVERWORLD);
/// The executable Nether pipeline.
pub const NETHER_PIPELINE: DimensionPipeline = DimensionPipeline::new(NETHER);
/// The executable End pipeline.
pub const END_PIPELINE: DimensionPipeline = DimensionPipeline::new(END);

const STRUCTURE_STARTS_OUTPUTS: &[ResourceKey] = &[ResourceKey::StructureStarts];
const STRUCTURE_REFERENCES_SIDECARS: &[SidecarKey] = &[SidecarKey::StructureReferences];
const FILL_OUTPUTS: &[ResourceKey] = &[ResourceKey::DensityField];
const BIOMES_INPUTS: &[ResourceKey] = &[ResourceKey::DensityField];
const BIOMES_OUTPUTS: &[ResourceKey] = &[ResourceKey::BiomeQuarts];
const SURFACE_INPUTS: &[ResourceKey] = &[ResourceKey::DensityField, ResourceKey::BiomeQuarts];
const SURFACE_OUTPUTS: &[ResourceKey] = &[ResourceKey::SurfaceDiff];
const MATERIALIZE_INPUTS: &[ResourceKey] =
    &[ResourceKey::DensityField, ResourceKey::SurfaceDiff];
const MATERIALIZE_OUTPUTS: &[ResourceKey] = &[ResourceKey::MaterializedWorld];
const CARVERS_INPUTS: &[ResourceKey] = &[ResourceKey::MaterializedWorld];
const CARVERS_OUTPUTS: &[ResourceKey] = &[ResourceKey::MaterializedWorld];
const STRUCTURE_PLACEMENT_INPUTS: &[ResourceKey] = &[
    ResourceKey::MaterializedWorld,
    ResourceKey::StructureStarts,
];
const STRUCTURE_PLACEMENT_OUTPUTS: &[ResourceKey] = &[ResourceKey::StructureBlocks];
const FEATURES_INPUTS: &[ResourceKey] = &[
    ResourceKey::MaterializedWorld,
    ResourceKey::BiomeQuarts,
    ResourceKey::StructureBlocks,
];
const FEATURES_OUTPUTS: &[ResourceKey] = &[ResourceKey::ResidentOverlay];
const FEATURES_SIDECARS: &[SidecarKey] = &[
    SidecarKey::DecorationSpills,
    SidecarKey::ClientHeightmaps,
];
const TOP_LAYER_INPUTS: &[ResourceKey] = &[ResourceKey::ResidentOverlay];
const TOP_LAYER_OUTPUTS: &[ResourceKey] = &[ResourceKey::ResidentRegion];
const OVERWORLD_OUTPUT_INPUTS: &[ResourceKey] = &[ResourceKey::ResidentRegion];
const NETHER_OUTPUT_INPUTS: &[ResourceKey] = &[ResourceKey::ResidentOverlay];
const OUTPUT_OUTPUTS: &[ResourceKey] = &[ResourceKey::OutputColumn];
const OUTPUT_SIDECARS: &[SidecarKey] = &[SidecarKey::ClientHeightmaps];

/// Descriptor contracts shared by the Overworld and Nether terrain stages.
///
/// These dimensions use the same immutable product hand-offs for terrain, but
/// their stage slices remain independent: `StageSchedule::descriptor` filters
/// this table through the selected schedule, so an absent pass is never
/// representable through a dimension pipeline.
const fn terrain_stage_descriptor(
    dimension: Dimension,
    stage: ColumnStage,
) -> Option<StageDescriptor> {
    let key = StageKey::new(dimension, stage);
    match stage {
        ColumnStage::StructureStarts => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(8),
            Radius2d::chunks(0),
            &[],
            STRUCTURE_STARTS_OUTPUTS,
            &[],
            SeedScope::StructureOrigin,
            BarrierPolicy::Pure,
            CancellationPolicy::BeforeCommit,
        )),
        ColumnStage::StructureReferences => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(8),
            Radius2d::chunks(0),
            &[ResourceKey::StructureStarts],
            &[],
            STRUCTURE_REFERENCES_SIDECARS,
            SeedScope::StructureChunk,
            BarrierPolicy::Pure,
            CancellationPolicy::BeforeCommit,
        )),
        ColumnStage::StructureInfluence => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(8),
            Radius2d::chunks(0),
            &[ResourceKey::StructureStarts],
            &[],
            &[],
            SeedScope::StructureChunk,
            BarrierPolicy::SourceOrdered,
            CancellationPolicy::TransactionBoundary,
        )),
        ColumnStage::Fill => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(0),
            Radius2d::chunks(0),
            &[],
            FILL_OUTPUTS,
            &[],
            SeedScope::TerrainChunk,
            BarrierPolicy::Pure,
            CancellationPolicy::BeforeCommit,
        )),
        ColumnStage::Biomes => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(0),
            Radius2d::chunks(0),
            BIOMES_INPUTS,
            BIOMES_OUTPUTS,
            &[],
            SeedScope::BiomeChunk,
            BarrierPolicy::Pure,
            CancellationPolicy::BeforeCommit,
        )),
        ColumnStage::Surface => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(0),
            Radius2d::chunks(0),
            SURFACE_INPUTS,
            SURFACE_OUTPUTS,
            &[],
            SeedScope::SurfaceChunk,
            BarrierPolicy::Pure,
            CancellationPolicy::BeforeCommit,
        )),
        ColumnStage::Materialize => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(0),
            Radius2d::chunks(0),
            MATERIALIZE_INPUTS,
            MATERIALIZE_OUTPUTS,
            &[],
            SeedScope::TerrainChunk,
            BarrierPolicy::Pure,
            CancellationPolicy::BeforeCommit,
        )),
        ColumnStage::Carvers => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(8),
            Radius2d::chunks(1),
            CARVERS_INPUTS,
            CARVERS_OUTPUTS,
            &[],
            SeedScope::SourceFeature,
            BarrierPolicy::SourceOrdered,
            CancellationPolicy::TransactionBoundary,
        )),
        ColumnStage::StructurePlacement => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(16),
            Radius2d::chunks(0),
            STRUCTURE_PLACEMENT_INPUTS,
            STRUCTURE_PLACEMENT_OUTPUTS,
            &[],
            SeedScope::StructureChunk,
            BarrierPolicy::Pure,
            CancellationPolicy::BeforeCommit,
        )),
        ColumnStage::Features => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(1),
            Radius2d::chunks(1),
            FEATURES_INPUTS,
            FEATURES_OUTPUTS,
            FEATURES_SIDECARS,
            SeedScope::SourceFeature,
            BarrierPolicy::SourceOrdered,
            CancellationPolicy::TransactionBoundary,
        )),
        ColumnStage::TopLayer => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(1),
            Radius2d::chunks(1),
            TOP_LAYER_INPUTS,
            TOP_LAYER_OUTPUTS,
            &[],
            SeedScope::SourceFeature,
            BarrierPolicy::SourceOrdered,
            CancellationPolicy::TransactionBoundary,
        )),
        ColumnStage::Output => Some(StageDescriptor::new(
            key,
            &[],
            Radius2d::chunks(0),
            Radius2d::chunks(0),
            match dimension {
                Dimension::Overworld => OVERWORLD_OUTPUT_INPUTS,
                Dimension::Nether | Dimension::End => NETHER_OUTPUT_INPUTS,
            },
            OUTPUT_OUTPUTS,
            OUTPUT_SIDECARS,
            SeedScope::None,
            BarrierPolicy::StageWide,
            CancellationPolicy::BeforeCommit,
        )),
    }
}

const fn overworld_stage_descriptor(stage: ColumnStage) -> Option<StageDescriptor> {
    terrain_stage_descriptor(Dimension::Overworld, stage)
}

const fn nether_stage_descriptor(stage: ColumnStage) -> Option<StageDescriptor> {
    terrain_stage_descriptor(Dimension::Nether, stage)
}

const END_FILL_KEY: StageKey = StageKey::new(Dimension::End, ColumnStage::Fill);
const END_BIOMES_KEY: StageKey = StageKey::new(Dimension::End, ColumnStage::Biomes);
const END_SURFACE_KEY: StageKey = StageKey::new(Dimension::End, ColumnStage::Surface);
const END_MATERIALIZE_KEY: StageKey = StageKey::new(Dimension::End, ColumnStage::Materialize);
const END_STRUCTURE_STARTS_KEY: StageKey =
    StageKey::new(Dimension::End, ColumnStage::StructureStarts);
const END_STRUCTURE_PLACEMENT_KEY: StageKey =
    StageKey::new(Dimension::End, ColumnStage::StructurePlacement);
const END_FEATURES_KEY: StageKey = StageKey::new(Dimension::End, ColumnStage::Features);
const END_OUTPUT_KEY: StageKey = StageKey::new(Dimension::End, ColumnStage::Output);

const END_FILL_OUTPUTS: &[ResourceKey] = &[ResourceKey::DensityField];
const END_BIOMES_PREREQUISITES: &[StageKey] = &[END_FILL_KEY];
const END_BIOMES_INPUTS: &[ResourceKey] = &[ResourceKey::DensityField];
const END_BIOMES_OUTPUTS: &[ResourceKey] = &[ResourceKey::BiomeQuarts];
const END_SURFACE_PREREQUISITES: &[StageKey] = &[END_BIOMES_KEY];
const END_SURFACE_INPUTS: &[ResourceKey] = &[ResourceKey::DensityField, ResourceKey::BiomeQuarts];
const END_SURFACE_OUTPUTS: &[ResourceKey] = &[ResourceKey::SurfaceDiff];
const END_MATERIALIZE_PREREQUISITES: &[StageKey] = &[END_SURFACE_KEY];
const END_MATERIALIZE_INPUTS: &[ResourceKey] = &[
    ResourceKey::DensityField,
    ResourceKey::SurfaceDiff,
];
const END_MATERIALIZE_OUTPUTS: &[ResourceKey] = &[ResourceKey::MaterializedWorld];
const END_STRUCTURE_STARTS_PREREQUISITES: &[StageKey] = &[END_MATERIALIZE_KEY];
const END_STRUCTURE_STARTS_INPUTS: &[ResourceKey] = &[ResourceKey::BiomeQuarts];
const END_STRUCTURE_STARTS_OUTPUTS: &[ResourceKey] = &[ResourceKey::StructureStarts];
const END_STRUCTURE_PLACEMENT_PREREQUISITES: &[StageKey] = &[END_STRUCTURE_STARTS_KEY];
const END_STRUCTURE_PLACEMENT_INPUTS: &[ResourceKey] = &[
    ResourceKey::MaterializedWorld,
    ResourceKey::StructureStarts,
];
const END_STRUCTURE_PLACEMENT_OUTPUTS: &[ResourceKey] = &[ResourceKey::StructureBlocks];
const END_STRUCTURE_PLACEMENT_SIDECARS: &[SidecarKey] = &[
    SidecarKey::StructureReferences,
    SidecarKey::BlockEntityEvents,
];
const END_FEATURES_PREREQUISITES: &[StageKey] = &[END_STRUCTURE_PLACEMENT_KEY];
const END_FEATURES_INPUTS: &[ResourceKey] = &[
    ResourceKey::MaterializedWorld,
    ResourceKey::BiomeQuarts,
    ResourceKey::StructureBlocks,
];
const END_FEATURES_OUTPUTS: &[ResourceKey] = &[ResourceKey::ResidentOverlay];
const END_FEATURES_SIDECARS: &[SidecarKey] = &[
    SidecarKey::DecorationSpills,
    SidecarKey::Gateways,
    SidecarKey::ClientHeightmaps,
];
const END_OUTPUT_PREREQUISITES: &[StageKey] = &[END_FEATURES_KEY];
const END_OUTPUT_INPUTS: &[ResourceKey] = &[ResourceKey::ResidentOverlay];
const END_OUTPUT_OUTPUTS: &[ResourceKey] = &[ResourceKey::OutputColumn];
const END_OUTPUT_SIDECARS: &[SidecarKey] = &[
    SidecarKey::BlockEntityEvents,
    SidecarKey::Gateways,
    SidecarKey::ClientHeightmaps,
];

const END_STAGE_DESCRIPTORS: &[StageDescriptor] = &[
    StageDescriptor::new(
        END_FILL_KEY,
        &[],
        Radius2d::chunks(0),
        Radius2d::chunks(0),
        &[],
        END_FILL_OUTPUTS,
        &[],
        SeedScope::TerrainChunk,
        BarrierPolicy::Pure,
        CancellationPolicy::BeforeCommit,
    ),
    StageDescriptor::new(
        END_BIOMES_KEY,
        END_BIOMES_PREREQUISITES,
        Radius2d::chunks(0),
        Radius2d::chunks(0),
        END_BIOMES_INPUTS,
        END_BIOMES_OUTPUTS,
        &[],
        SeedScope::BiomeChunk,
        BarrierPolicy::Pure,
        CancellationPolicy::BeforeCommit,
    ),
    StageDescriptor::new(
        END_SURFACE_KEY,
        END_SURFACE_PREREQUISITES,
        Radius2d::chunks(0),
        Radius2d::chunks(0),
        END_SURFACE_INPUTS,
        END_SURFACE_OUTPUTS,
        &[],
        SeedScope::SurfaceChunk,
        BarrierPolicy::Pure,
        CancellationPolicy::BeforeCommit,
    ),
    StageDescriptor::new(
        END_MATERIALIZE_KEY,
        END_MATERIALIZE_PREREQUISITES,
        Radius2d::chunks(0),
        Radius2d::chunks(0),
        END_MATERIALIZE_INPUTS,
        END_MATERIALIZE_OUTPUTS,
        &[],
        SeedScope::TerrainChunk,
        BarrierPolicy::Pure,
        CancellationPolicy::BeforeCommit,
    ),
    StageDescriptor::new(
        END_STRUCTURE_STARTS_KEY,
        END_STRUCTURE_STARTS_PREREQUISITES,
        Radius2d::chunks(0),
        Radius2d::chunks(0),
        END_STRUCTURE_STARTS_INPUTS,
        END_STRUCTURE_STARTS_OUTPUTS,
        &[],
        SeedScope::StructureOrigin,
        BarrierPolicy::Pure,
        CancellationPolicy::BeforeCommit,
    ),
    StageDescriptor::new(
        END_STRUCTURE_PLACEMENT_KEY,
        END_STRUCTURE_PLACEMENT_PREREQUISITES,
        Radius2d::chunks(16),
        Radius2d::chunks(0),
        END_STRUCTURE_PLACEMENT_INPUTS,
        END_STRUCTURE_PLACEMENT_OUTPUTS,
        END_STRUCTURE_PLACEMENT_SIDECARS,
        SeedScope::StructureChunk,
        BarrierPolicy::Pure,
        CancellationPolicy::BeforeCommit,
    ),
    StageDescriptor::new(
        END_FEATURES_KEY,
        END_FEATURES_PREREQUISITES,
        Radius2d::chunks(1),
        Radius2d::chunks(1),
        END_FEATURES_INPUTS,
        END_FEATURES_OUTPUTS,
        END_FEATURES_SIDECARS,
        SeedScope::SourceFeature,
        BarrierPolicy::SourceOrdered,
        CancellationPolicy::TransactionBoundary,
    ),
    StageDescriptor::new(
        END_OUTPUT_KEY,
        END_OUTPUT_PREREQUISITES,
        Radius2d::chunks(0),
        Radius2d::chunks(0),
        END_OUTPUT_INPUTS,
        END_OUTPUT_OUTPUTS,
        END_OUTPUT_SIDECARS,
        SeedScope::None,
        BarrierPolicy::StageWide,
        CancellationPolicy::BeforeCommit,
    ),
];

const fn end_stage_descriptor(stage: ColumnStage) -> Option<StageDescriptor> {
    let mut index = 0;
    while index < END_STAGE_DESCRIPTORS.len() {
        let descriptor = END_STAGE_DESCRIPTORS[index];
        if descriptor.key().stage() as u8 == stage as u8 {
            return Some(descriptor);
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        BarrierPolicy, CancellationPolicy, ChunkRequest, ColumnStage, DecorationStep, Dimension,
        END, END_SOURCES, GenerationLevel, GenerationTarget, LIFECYCLE, LifecyclePhase, NETHER,
        NETHER_FEATURES, NETHER_SOURCES, OVERWORLD, OVERWORLD_FEATURES, OVERWORLD_SOURCES,
        END_PIPELINE, NETHER_PIPELINE, OVERWORLD_PIPELINE, PipelineOptions, ResourceKey,
        SeedScope, SidecarKey, SourceCompletion, StageFrontier, StageKey, StageOption, StageRecord,
        STAGE_SCHEDULE_VERSION,
    };

    #[test]
    fn every_dimension_pipeline_is_the_single_descriptor_order() {
        for (pipeline, schedule) in [
            (OVERWORLD_PIPELINE, OVERWORLD),
            (NETHER_PIPELINE, NETHER),
            (END_PIPELINE, END),
        ] {
            pipeline.validate();
            assert_eq!(pipeline.schedule(), schedule);
            assert_eq!(pipeline.descriptors().len(), pipeline.stages().len());
            assert!(pipeline
                .descriptors()
                .iter()
                .zip(pipeline.stages())
                .all(|(descriptor, stage)| descriptor.key()
                    == StageKey::new(pipeline.dimension(), *stage)));
        }
        assert!(NETHER_PIPELINE.descriptor(ColumnStage::TopLayer).is_none());
        assert!(END_PIPELINE.descriptor(ColumnStage::Carvers).is_none());
    }

    #[test]
    fn executor_applies_option_gates() {
        let options = PipelineOptions::new(false, true);
        let mut executor = OVERWORLD_PIPELINE.executor_with_options(options);
        for stage in OVERWORLD_PIPELINE.stages() {
            let ran = std::cell::Cell::new(false);
            let result = executor.run_if(*stage, || {
                ran.set(true);
            });
            assert_eq!(ran.get(), OVERWORLD.stage_enabled(*stage, options));
            assert_eq!(result.is_some(), OVERWORLD.stage_enabled(*stage, options));
        }
        executor.finish();
    }

    #[test]
    #[should_panic(expected = "generation stage out of order")]
    fn executor_rejects_order_drift() {
        let mut executor = OVERWORLD_PIPELINE.executor();
        executor.enter(ColumnStage::Fill);
    }

    #[test]
    fn frontier_identity_carries_dimension_schedule_and_options() {
        let options = PipelineOptions::new(false, true);
        let frontier = OVERWORLD_PIPELINE.frontier((4, -9), options);
        assert_eq!(frontier.pipeline(), OVERWORLD_PIPELINE);
        assert_eq!(frontier.options(), options);
        assert_eq!(frontier.identity().dimension(), Dimension::Overworld);
        assert_eq!(frontier.identity().schedule_version(), STAGE_SCHEDULE_VERSION);
        assert_eq!(frontier.identity().options(), options);
    }

    #[test]
    fn every_dimension_has_a_unique_readable_schedule() {
        for schedule in [OVERWORLD, NETHER, END] {
            schedule.validate();
            assert!(!schedule.gates().is_empty());
            assert!(schedule.index_of(ColumnStage::Fill).is_some());
            assert!(schedule.index_of(ColumnStage::Output).is_some());
        }
        assert_eq!(OVERWORLD.dimension(), Dimension::Overworld);
        assert_eq!(NETHER.dimension(), Dimension::Nether);
        assert_eq!(END.dimension(), Dimension::End);
        assert!(OVERWORLD.gates().iter().any(|gate| {
            gate.stage() == ColumnStage::Features && gate.option() == StageOption::Decorations
        }));
        assert!(NETHER.gates().iter().any(|gate| {
            gate.stage() == ColumnStage::StructurePlacement && gate.option() == StageOption::Structures
        }));
    }

    #[test]
    fn dimension_specific_passes_are_explicit() {
        assert!(OVERWORLD.index_of(ColumnStage::TopLayer).is_some());
        assert!(NETHER.index_of(ColumnStage::TopLayer).is_none());
        assert!(END.index_of(ColumnStage::Carvers).is_none());
        assert!(OVERWORLD.index_of(ColumnStage::StructureStarts).unwrap() < OVERWORLD.index_of(ColumnStage::Fill).unwrap());
        assert!(OVERWORLD.index_of(ColumnStage::StructureReferences).unwrap() < OVERWORLD.index_of(ColumnStage::Fill).unwrap());
        assert!(OVERWORLD.index_of(ColumnStage::StructureInfluence).unwrap() < OVERWORLD.index_of(ColumnStage::Fill).unwrap());
        assert!(END.index_of(ColumnStage::StructureStarts).unwrap() > END.index_of(ColumnStage::Materialize).unwrap());
        assert!(OVERWORLD.index_of(ColumnStage::StructurePlacement).unwrap() < OVERWORLD.index_of(ColumnStage::Features).unwrap());
        assert!(NETHER.index_of(ColumnStage::StructurePlacement).unwrap() < NETHER.index_of(ColumnStage::Features).unwrap());
        assert!(END.index_of(ColumnStage::StructurePlacement).unwrap() < END.index_of(ColumnStage::Features).unwrap());
    }

    #[test]
    fn shaped_prefixes_name_each_dimensions_actual_resume_boundary() {
        assert_eq!(OVERWORLD.shaped_boundary(), ColumnStage::Features);
        assert_eq!(NETHER.shaped_boundary(), ColumnStage::StructurePlacement);
        assert_eq!(END.shaped_boundary(), ColumnStage::Features);
        assert_eq!(OVERWORLD.shaped_boundary_index(), 9);
        assert_eq!(NETHER.shaped_boundary_index(), 8);
        assert_eq!(END.shaped_boundary_index(), 6);
        assert_eq!(
            END.stages_for(GenerationTarget::Shaped),
            &END.stages()[..6]
        );
        assert_eq!(
            END.target_stage(GenerationTarget::Shaped),
            ColumnStage::StructurePlacement
        );
        assert_eq!(END.target_stage(GenerationTarget::Full), ColumnStage::Output);
        assert_eq!(
            END.stages_for_level(GenerationLevel::Terrain),
            Some(&END.stages()[..3])
        );
        assert_eq!(
            END.stages_for_level(GenerationLevel::Structures),
            Some(&END.stages()[..6])
        );
        assert_eq!(
            END.stages_for_level(GenerationLevel::Decorated),
            Some(&END.stages()[..7])
        );
    }

    #[test]
    fn end_has_one_descriptor_per_canonical_stage() {
        END.validate();
        let descriptors = END.descriptors();
        assert_eq!(descriptors.len(), END.stages().len());
        assert_eq!(
            descriptors
                .iter()
                .map(|descriptor| descriptor.key().stage())
                .collect::<Vec<_>>(),
            END.stages()
        );
        let placement = END
            .descriptor(ColumnStage::StructurePlacement)
            .expect("End structure placement descriptor");
        assert_eq!(placement.read_radius().chunks_value(), 16);
        assert_eq!(placement.write_radius().chunks_value(), 0);
        assert_eq!(placement.barrier(), BarrierPolicy::Pure);
        assert_eq!(placement.seed_scope(), SeedScope::StructureChunk);
        assert!(placement
            .retained_sidecars()
            .contains(&SidecarKey::BlockEntityEvents));

        let features = END
            .descriptor(ColumnStage::Features)
            .expect("End feature descriptor");
        assert_eq!(features.read_radius().chunks_value(), 1);
        assert_eq!(features.write_radius().chunks_value(), 1);
        assert_eq!(features.barrier(), BarrierPolicy::SourceOrdered);
        assert_eq!(features.cancellation(), CancellationPolicy::TransactionBoundary);
        assert_eq!(features.seed_scope(), SeedScope::SourceFeature);
        assert!(features.outputs().contains(&ResourceKey::ResidentOverlay));
        assert!(features.retained_sidecars().contains(&SidecarKey::Gateways));
    }

    #[test]
    fn end_frontier_is_chunk_resumable_and_atomic() {
        let mut frontier = StageFrontier::new(END, (-7, 11));
        assert_eq!(frontier.coordinate(), (-7, 11));
        assert_eq!(
            frontier.next_stage(),
            Some(StageKey::new(Dimension::End, ColumnStage::Fill))
        );

        let fingerprint = |value: u8| [value; 32];
        frontier
            .commit_stage(
                ColumnStage::Fill,
                fingerprint(0),
                fingerprint(1),
                26,
            )
            .expect("fill commit");
        assert_eq!(frontier.completed_stage_mask(), 1);
        assert_eq!(frontier.highest_level(), None);
        assert_eq!(
            frontier.resume_stage().map(|descriptor| descriptor.key().stage()),
            Some(ColumnStage::Biomes)
        );

        let out_of_order = StageRecord::for_descriptor(
            END.descriptor(ColumnStage::Surface).expect("surface descriptor"),
            fingerprint(2),
            fingerprint(3),
            26,
        );
        let before = frontier.records().to_vec();
        assert!(matches!(
            frontier.commit(out_of_order),
            Err(super::FrontierError::OutOfOrder { .. })
        ));
        assert_eq!(frontier.records(), before.as_slice());

        for (index, stage) in END.stages().iter().copied().enumerate().skip(1) {
            frontier
                .commit_stage(stage, fingerprint(index as u8), fingerprint((index + 1) as u8), 26)
                .expect("ordered End stage commit");
        }
        assert!(frontier.is_complete());
        assert_eq!(frontier.highest_level(), Some(GenerationLevel::Output));
        assert!(frontier.is_complete_at_level(GenerationLevel::Terrain));
        assert!(frontier.is_complete_at_level(GenerationLevel::Structures));
        assert!(frontier.is_complete_at_level(GenerationLevel::Decorated));
        assert!(frontier.is_complete_through(ColumnStage::Features));
        assert_eq!(frontier.next_stage(), None);
        assert_eq!(frontier.schedule_version(), super::STAGE_SCHEDULE_VERSION);

        let restored = StageFrontier::from_records(
            END,
            (-7, 11),
            super::STAGE_SCHEDULE_VERSION,
            frontier.records().to_vec(),
        )
        .expect("restore complete End frontier");
        assert_eq!(restored, frontier);
    }

    #[test]
    fn end_frontier_rejects_missing_contract_products_and_versions() {
        let mut frontier = StageFrontier::new(END, (0, 0));
        let fill_key = StageKey::new(Dimension::End, ColumnStage::Fill);
        let missing_output = StageRecord::new(
            fill_key,
            [0; 32],
            [1; 32],
            Vec::new(),
            Vec::new(),
            26,
        );
        assert_eq!(
            frontier.commit(missing_output),
            Err(super::FrontierError::MissingProduct {
                stage: fill_key,
                product: ResourceKey::DensityField,
            })
        );
        assert!(frontier.records().is_empty());

        let mut features = StageFrontier::new(END, (0, 0));
        let error = StageFrontier::from_records(
            END,
            (0, 0),
            super::STAGE_SCHEDULE_VERSION + 1,
            Vec::new(),
        )
        .expect_err("future schedule version must not restore");
        assert_eq!(
            error,
            super::FrontierError::ScheduleVersionMismatch {
                expected: super::STAGE_SCHEDULE_VERSION,
                found: super::STAGE_SCHEDULE_VERSION + 1,
            }
        );

        let foreign = StageRecord::for_descriptor(
            super::StageDescriptor::new(
                StageKey::new(Dimension::Nether, ColumnStage::Fill),
                &[],
                super::Radius2d::chunks(0),
                super::Radius2d::chunks(0),
                &[],
                &[ResourceKey::DensityField],
                &[],
                super::SeedScope::TerrainChunk,
                super::BarrierPolicy::Pure,
                super::CancellationPolicy::BeforeCommit,
            ),
            [0; 32],
            [1; 32],
            26,
        );
        assert!(matches!(
            features.commit(foreign),
            Err(super::FrontierError::ForeignDimension { .. })
        ));
    }

    #[test]
    fn lifecycle_dependencies_are_ordered_and_packet_finalization_is_terminal() {
        LIFECYCLE.validate();
        let features = LIFECYCLE
            .contract(LifecyclePhase::Features)
            .expect("features lifecycle contract");
        assert_eq!(features.block_write_radius(), 1);
        assert!(features.dependencies().iter().any(|dependency| {
            dependency.phase() == LifecyclePhase::Carvers && dependency.radius() == 1
        }));
        let packet = LIFECYCLE
            .contract(LifecyclePhase::PacketFinalization)
            .expect("packet lifecycle contract");
        assert!(packet.dependencies().iter().any(|dependency| {
            dependency.phase() == LifecyclePhase::Light && dependency.radius() == 1
        }));
        assert_eq!(
            LIFECYCLE.phases().last().map(|contract| contract.phase()),
            Some(LifecyclePhase::PacketFinalization)
        );
    }

    #[test]
    fn cursor_can_resume_after_a_cached_prefix() {
        let structure = OVERWORLD.index_of(ColumnStage::StructurePlacement).unwrap();
        let mut cursor = OVERWORLD.cursor_at(structure);
        cursor.enter(ColumnStage::StructurePlacement);
        cursor.enter(ColumnStage::Features);
        cursor.enter(ColumnStage::TopLayer);
        cursor.enter(ColumnStage::Output);
        cursor.finish();
    }

    #[test]
    fn source_order_is_separate_from_column_order() {
        assert_eq!(OVERWORLD_SOURCES.dimension(), Dimension::Overworld);
        assert_eq!(NETHER_SOURCES.dimension(), Dimension::Nether);
        assert_eq!(END_SOURCES.dimension(), Dimension::End);
        let SourceCompletion::Fixed(overworld_offsets) = OVERWORLD_SOURCES.completion() else {
            panic!("the Overworld source window must have a fixed order");
        };
        assert_eq!(NETHER_SOURCES.completion(), SourceCompletion::AdmissionDependent);
        let SourceCompletion::Fixed(end_offsets) = END_SOURCES.completion() else {
            panic!("the End source window must have a fixed order");
        };
        assert!(overworld_offsets.contains(&(0, 0)));
        assert_ne!(overworld_offsets.last(), Some(&(0, 0)));
        assert_eq!(overworld_offsets.len(), 9);
        // This is the byte-order contract consumed by the production
        // Overworld dispatcher. Keep the expected sequence here as a negative
        // drift control: changing the source permutation must fail this test,
        // rather than merely continuing to generate a plausible column.
        assert_eq!(
            overworld_offsets,
            &[
                (-1, -1),
                (-1, 0),
                (-1, 1),
                (0, -1),
                (0, 0),
                (0, 1),
                (1, -1),
                (1, 0),
                (1, 1),
            ]
        );
        assert_eq!(end_offsets, overworld_offsets);
    }

    #[test]
    fn feature_steps_are_named_and_keep_seed_ordinals() {
        assert_eq!(DecorationStep::UndergroundOres.ordinal(), 6);
        assert_eq!(DecorationStep::VegetalDecoration.ordinal(), 9);
        assert_eq!(DecorationStep::TopLayerModification.ordinal(), 10);
        assert_eq!(DecorationStep::from_ordinal(7), Some(DecorationStep::UndergroundDecoration));
        assert_eq!(DecorationStep::from_ordinal(11), None);
        // The dispatcher must consume this schedule directly. The explicit
        // sequence is a cheap control for accidental insertion/reordering;
        // byte identity is exercised by the shaped/full integration gate.
        assert_eq!(
            OVERWORLD_FEATURES.steps(),
            &[
                DecorationStep::RawGeneration,
                DecorationStep::Lakes,
                DecorationStep::LocalModifications,
                DecorationStep::UndergroundStructures,
                DecorationStep::SurfaceStructures,
                DecorationStep::UndergroundOres,
                DecorationStep::UndergroundDecoration,
                DecorationStep::FluidSprings,
                DecorationStep::VegetalDecoration,
            ]
        );
        assert!(OVERWORLD_FEATURES
            .steps()
            .windows(2)
            .all(|steps| steps[0] < steps[1]));
        assert!(NETHER_FEATURES
            .steps()
            .windows(2)
            .all(|steps| steps[0] < steps[1]));
        assert_eq!(
            NETHER_FEATURES
                .steps()
                .iter()
                .map(|step| step.ordinal())
                .collect::<Vec<_>>(),
            [2, 4, 7, 9]
        );
    }

    #[test]
    fn nether_completion_order_is_derived_from_the_request_wavefront() {
        let request = ChunkRequest::new(32, 39, 48, 55, 2);
        assert_eq!(request.admission_wavefront().order_key((30, 46)), (0, 0, 0, 0));
        let admissions = request.admission_order();
        assert_eq!(admissions.len(), 12 * 12);
        assert_eq!(&admissions[..4], &[(30, 46), (31, 46), (32, 46), (33, 46)]);
        assert_eq!(
            request.source_completion_order((32, 48)).offsets(),
            [
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (0, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ]
        );
    }

    #[test]
    fn nether_source_controls_follow_completion_not_a_global_permutation() {
        let offsets = ChunkRequest::single(32, 48, 2)
            .source_completion_order((32, 48))
            .offsets();
        let position = |offset| {
            offsets
                .iter()
                .position(|candidate| *candidate == offset)
                .expect("source offset must be present")
        };
        assert!(position((0, -1)) < position((0, 0)));
        assert!(position((0, 0)) < position((0, 1)));
        assert!(position((1, 0)) < position((0, 1)));
    }

    #[test]
    fn a_tile_boundary_changes_source_order_without_a_coordinate_rule() {
        let request = ChunkRequest::new(2, 16, 2, 16, 2);
        assert_eq!(
            request.source_completion_order((16, 16)).offsets(),
            [
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (-1, 1),
                (0, 0),
                (1, 0),
                (0, 1),
                (1, 1),
            ]
        );
        assert_ne!(
            request.source_completion_order((16, 16)).offsets(),
            ChunkRequest::single(32, 48, 2)
                .source_completion_order((32, 48))
                .offsets()
        );
    }
}
