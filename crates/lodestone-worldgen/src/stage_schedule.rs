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

/// Source-chunk completion order for a mutable neighbourhood dispatch.
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
        ColumnStage, DecorationStep, Dimension, END, END_SOURCES, NETHER, NETHER_FEATURES,
        NETHER_SOURCES, OVERWORLD, OVERWORLD_FEATURES, OVERWORLD_SOURCES, SourceCompletion,
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
}
