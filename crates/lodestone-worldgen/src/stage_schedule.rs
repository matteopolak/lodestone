//! Explicit world-generation pass order.
//!
//! The generators deliberately keep dimension-specific implementations, but
//! their pass order is still a contract.  Keeping that contract in one small
//! typed table makes it possible for production dispatchers and parity replay
//! to name the same stages instead of repeating an undocumented list of loops.
//!
//! This module contains no generation work and therefore cannot alter terrain.
//! A generator can use [`StageCursor`] at the boundaries between helper
//! functions; a debug build then fails immediately if a refactor enters a pass
//! out of order or forgets one.  The static schedules are also useful to
//! parity tooling that needs to describe which prefix a captured column owns.

/// Dimension whose pass schedule is being described.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

pub(crate) const OVERWORLD_SOURCE_OFFSETS: &[(i32, i32); 9] = &[
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (0, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageSchedule {
    dimension: Dimension,
    stages: &'static [ColumnStage],
}

impl StageSchedule {
    /// Construct a schedule for a static stage slice.
    pub const fn new(dimension: Dimension, stages: &'static [ColumnStage]) -> Self {
        Self { dimension, stages }
    }

    #[must_use]
    pub const fn dimension(self) -> Dimension {
        self.dimension
    }

    #[must_use]
    pub const fn stages(self) -> &'static [ColumnStage] {
        self.stages
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
    }

    fn dimension_name(self) -> &'static str {
        match self.dimension {
            Dimension::Overworld => "overworld",
            Dimension::Nether => "nether",
            Dimension::End => "end",
        }
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
pub const OVERWORLD: StageSchedule = StageSchedule::new(Dimension::Overworld, OVERWORLD_STAGES);
/// The complete Nether order.
pub const NETHER: StageSchedule = StageSchedule::new(Dimension::Nether, NETHER_STAGES);
/// The complete End order.
pub const END: StageSchedule = StageSchedule::new(Dimension::End, END_STAGES);

#[cfg(test)]
mod tests {
    use super::{
        ChunkRequest, ColumnStage, DecorationStep, Dimension, END, END_SOURCES, NETHER,
        NETHER_FEATURES, NETHER_SOURCES, OVERWORLD, OVERWORLD_FEATURES, OVERWORLD_SOURCES,
        SourceCompletion,
    };

    #[test]
    fn every_dimension_has_a_unique_readable_schedule() {
        for schedule in [OVERWORLD, NETHER, END] {
            schedule.validate();
            assert!(schedule.index_of(ColumnStage::Fill).is_some());
            assert!(schedule.index_of(ColumnStage::Output).is_some());
        }
        assert_eq!(OVERWORLD.dimension(), Dimension::Overworld);
        assert_eq!(NETHER.dimension(), Dimension::Nether);
        assert_eq!(END.dimension(), Dimension::End);
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
        assert_eq!(end_offsets, overworld_offsets);
    }

    #[test]
    fn feature_steps_are_named_and_keep_seed_ordinals() {
        assert_eq!(DecorationStep::UndergroundOres.ordinal(), 6);
        assert_eq!(DecorationStep::VegetalDecoration.ordinal(), 9);
        assert_eq!(DecorationStep::TopLayerModification.ordinal(), 10);
        assert_eq!(DecorationStep::from_ordinal(7), Some(DecorationStep::UndergroundDecoration));
        assert_eq!(DecorationStep::from_ordinal(11), None);
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
