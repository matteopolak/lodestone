//! Diagnostic seam for source-ordered Overworld FEATURES settlement replay.
//!
//! This module deliberately does not participate in the production target-owned
//! path. It folds a requested target set into one canonical absolute source
//! wavefront over one request-scoped mutable region, keeping the experimental
//! cost and expected shared-region divergence visible for differential tests.

#[cfg(test)]
use std::collections::BTreeSet;

use super::{GeneratedColumn, MixedReplayBatch, OverworldGenerator};
use lodestone_data::block_states::StateId;
#[cfg(test)]
use crate::stage_schedule::{OVERWORLD_SOURCES, SourceCompletion};

/// One ordered state transition produced by a source completion.
///
/// The source coordinate is retained on every event rather than inferred from the
/// destination.  A source can write into any of the nine columns in its writer
/// window, so destination ownership is not provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceOnceMutation {
    /// Absolute source chunk whose feature body emitted the transition.
    pub source: (i32, i32),
    /// Absolute block position of the final value for this transition.
    pub position: (i32, i32, i32),
    /// Canonical block-state id from the bundled registry.
    pub state: StateId,
}

/// One source execution in the canonical settlement wavefront.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceOnceExecution {
    /// Absolute source chunk executed once.
    pub source: (i32, i32),
    /// Requested target whose immutable context owned this execution.
    pub owner_target: (i32, i32),
    /// Ordered final transitions returned by this source body in test traces;
    /// production retains only the source identity and execution count.
    pub mutations: Vec<SourceOnceMutation>,
}

/// A requested target's exact full projection from the shared source stream.
#[derive(Debug)]
pub struct SourceOnceTargetResult {
    pub(crate) target: (i32, i32),
    pub(crate) column: GeneratedColumn,
}

impl SourceOnceTargetResult {
    /// Requested target coordinate.
    #[must_use]
    pub const fn target(&self) -> (i32, i32) {
        self.target
    }

    /// Final full output projected from the source-once stream.
    #[must_use]
    pub fn column(&self) -> &GeneratedColumn {
        &self.column
    }

    /// Moves the coordinate and generated column out of this result.
    #[must_use]
    pub fn into_parts(self) -> ((i32, i32), GeneratedColumn) {
        (self.target, self.column)
    }
}

/// Result of the bounded source-once experiment.
///
/// `requested` contains only the full 16×height×16 target outputs.  Writes whose
/// destination is outside the requested set are retained separately in
/// `padding_mutations`, so a caller measuring settlement memory does not have to
/// materialize padded columns merely to preserve their mutation stream.
#[derive(Debug)]
pub struct SourceOnceBatchResult {
    pub(crate) requested: Vec<SourceOnceTargetResult>,
    pub(crate) executions: Vec<SourceOnceExecution>,
    pub(crate) global_overrides: Vec<SourceOnceMutation>,
    pub(crate) padding_mutations: Vec<SourceOnceMutation>,
    pub(crate) execution_counts: Vec<SourceExecutionCount>,
    pub(crate) mutable_writes: usize,
    pub(crate) region_retained_bytes: usize,
}

impl SourceOnceBatchResult {
    /// Requested targets in canonical `(z, x)` order.
    #[must_use]
    pub fn requested(&self) -> &[SourceOnceTargetResult] {
        &self.requested
    }

    /// Source completions in their canonical execution order.
    #[must_use]
    pub fn executions(&self) -> &[SourceOnceExecution] {
        &self.executions
    }

    /// Bounded diagnostic transition trace. This is populated only when
    /// provenance capture is requested.
    #[must_use]
    pub fn global_overrides(&self) -> &[SourceOnceMutation] {
        &self.global_overrides
    }

    /// Compact transitions whose destination is a padding column.
    #[must_use]
    pub fn padding_mutations(&self) -> &[SourceOnceMutation] {
        &self.padding_mutations
    }

    /// Number of absolute sources executed by this request.
    #[must_use]
    pub fn source_execution_count(&self) -> usize {
        self.executions.len()
    }

    #[must_use]
    pub const fn unique_source_count(&self) -> usize {
        self.executions.len()
    }

    /// Per-source execution counts.  The experiment intentionally produces one
    /// entry per source with count one; retaining this as a table makes a future
    /// control that accidentally repeats a source observable.
    #[must_use]
    pub fn source_execution_counts(&self) -> &[SourceExecutionCount] {
        &self.execution_counts
    }

    #[must_use]
    pub const fn mutable_write_count(&self) -> usize {
        self.mutable_writes
    }

    #[must_use]
    pub const fn mutable_writes(&self) -> usize {
        self.mutable_writes
    }

    #[must_use]
    pub const fn region_retained_bytes(&self) -> usize {
        self.region_retained_bytes
    }

    /// Moves every result component out without cloning generated columns or
    /// diagnostic mutation streams.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Vec<SourceOnceTargetResult>,
        Vec<SourceOnceExecution>,
        Vec<SourceOnceMutation>,
        Vec<SourceOnceMutation>,
        Vec<SourceExecutionCount>,
        usize,
        usize,
    ) {
        (
            self.requested,
            self.executions,
            self.global_overrides,
            self.padding_mutations,
            self.execution_counts,
            self.mutable_writes,
            self.region_retained_bytes,
        )
    }

    pub(crate) fn from_region_run(
        requested: Vec<SourceOnceTargetResult>,
        executions: Vec<SourceOnceExecution>,
        global_overrides: Vec<SourceOnceMutation>,
        padding_mutations: Vec<SourceOnceMutation>,
        execution_counts: Vec<SourceExecutionCount>,
        mutable_writes: usize,
        region_retained_bytes: usize,
    ) -> Self {
        Self {
            requested,
            executions,
            global_overrides,
            padding_mutations,
            execution_counts,
            mutable_writes,
            region_retained_bytes,
        }
    }
}

/// Count for one absolute source in a source-once result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceExecutionCount {
    /// Absolute source chunk.
    pub source: (i32, i32),
    /// Number of feature-body executions in this batch.
    pub count: usize,
}

impl MixedReplayBatch {
    /// Executes the batch's absolute Overworld sources once, using a canonical
    /// owner context and one ordered override stream.
    ///
    /// The method is intentionally an experiment seam: the scalar production
    /// path still completes each target independently.  `self.targets()` may be
    /// in caller order; this method canonicalizes it before selecting owners.
    #[must_use]
    pub fn source_once_features(&self, generator: &OverworldGenerator) -> SourceOnceBatchResult {
        self.source_once_features_with_capture(generator, false)
    }

    /// Source-once execution with optional final-writer provenance capture.
    #[must_use]
    pub fn source_once_features_with_capture(
        &self,
        generator: &OverworldGenerator,
        capture_provenance: bool,
    ) -> SourceOnceBatchResult {
        let targets = canonical_targets(self.targets());
        if targets.is_empty() {
            return SourceOnceBatchResult::from_region_run(
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                0,
                0,
            );
        }
        generator.source_once_features_region(&targets, self, capture_provenance)
    }
}

impl OverworldGenerator {
    /// Runs the bounded source-ordered FEATURES experiment for `targets`.
    /// Targets are canonicalized by `(z, x)` before source execution is
    /// selected. Production `column` calls use the target-owned dispatcher;
    /// this remains a diagnostic comparator only.
    #[must_use]
    pub fn source_once_features(&self, targets: &[(i32, i32)]) -> SourceOnceBatchResult {
        self.source_once_features_with_capture(targets, false)
    }

    /// Source-once execution with optional final-writer provenance capture.
    #[must_use]
    pub fn source_once_features_with_capture(
        &self,
        targets: &[(i32, i32)],
        capture_provenance: bool,
    ) -> SourceOnceBatchResult {
        let canonical = canonical_targets(targets);
        if canonical.is_empty() {
            return SourceOnceBatchResult::from_region_run(
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                0,
                0,
            );
        }
        let batch = self.mixed_replay_batch_with_radius(&canonical, 3);
        batch.source_once_features_with_capture(self, capture_provenance)
    }
}

fn canonical_targets(targets: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut canonical = targets.to_vec();
    canonical.sort_unstable_by_key(|&(x, z)| (z, x));
    canonical.dedup();
    canonical
}

#[cfg(test)]
fn canonical_sources(targets: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut seen = BTreeSet::new();
    let mut sources = Vec::with_capacity(targets.len() * 9);
    let offsets = match OVERWORLD_SOURCES.completion() {
        SourceCompletion::Fixed(offsets) => offsets,
        SourceCompletion::AdmissionDependent => {
            unreachable!("Overworld source-once experiment requires a fixed source order")
        }
    };
    for &(target_x, target_z) in targets {
        for &(offset_x, offset_z) in offsets {
            let source = (target_x + offset_x, target_z + offset_z);
            if seen.insert(source) {
                sources.push(source);
            }
        }
    }
    sources
}

#[cfg(test)]
fn source_in_target(source: (i32, i32), target: (i32, i32)) -> bool {
    (source.0 - target.0).abs() <= 1 && (source.1 - target.1).abs() <= 1
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use lodestone_data::block_states::StateId;

    use crate::density::{NoiseParams, Resolver};
    use super::{canonical_sources, canonical_targets, source_in_target};
    use crate::overworld::{GeneratedColumn, OverworldGenerator};


    struct FsResolver {
        root: PathBuf,
    }

    impl FsResolver {
        fn read(&self, kind: &str, id: &str) -> Value {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let path = self.root.join(kind).join(format!("{name}.json"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
        }

        fn try_read(&self, kind: &str, id: &str) -> Value {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let path = self.root.join(kind).join(format!("{name}.json"));
            std::fs::read_to_string(&path)
                .ok()
                .map(|text| {
                    serde_json::from_str(&text)
                        .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
                })
                .unwrap_or(Value::Null)
        }
    }

    impl Resolver for FsResolver {
        fn density_function(&self, id: &str) -> Value {
            self.read("density_function", id)
        }

        fn noise(&self, id: &str) -> NoiseParams {
            let value = self.read("noise", id);
            NoiseParams {
                first_octave: value["firstOctave"].as_i64().expect("firstOctave") as i32,
                amplitudes: value["amplitudes"]
                    .as_array()
                    .expect("amplitudes")
                    .iter()
                    .map(|amplitude| amplitude.as_f64().expect("amplitude"))
                    .collect(),
            }
        }

        fn biome_parameters(&self) -> Value {
            self.read("biome_parameters", "overworld")
        }

        fn biome_temperatures(&self) -> Value {
            self.read("biome_parameters", "overworld_temperature")
        }

        fn biome_document(&self, id: &str) -> Value {
            self.try_read("biome", id)
        }

        fn configured_carver(&self, id: &str) -> Value {
            self.try_read("configured_carver", id)
        }

        fn configured_feature(&self, id: &str) -> Value {
            self.try_read("configured_feature", id)
        }

        fn placed_feature(&self, id: &str) -> Value {
            self.try_read("placed_feature", id)
        }

        fn block_tag(&self, id: &str) -> Value {
            self.try_read("tags/block", id)
        }
    }

    fn generator() -> OverworldGenerator {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../lodestone-server/assets/worldgen");
        let resolver = FsResolver { root: root.clone() };
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("noise_settings/overworld.json"))
                .expect("read Overworld settings"),
        )
        .expect("parse Overworld settings");
        OverworldGenerator::new(42, &settings, &resolver, "minecraft:plains", false)
    }

    fn digest(column: &GeneratedColumn) -> [u8; 32] {
        let mut digest = Sha256::new();
        for y in column.min_y()..column.min_y() + column.height() {
            for z in 0..16 {
                for x in 0..16 {
                    digest.update(column.block_state_id(x, y, z).raw().to_le_bytes());
                    digest.update([0]);
                }
            }
        }
        digest.finalize().into()
    }

    fn first_state_mismatch(
        expected: &GeneratedColumn,
        actual: &GeneratedColumn,
    ) -> Option<(usize, i32, usize, StateId, StateId)> {
        for y in expected.min_y()..expected.min_y() + expected.height() {
            for z in 0..16 {
                for x in 0..16 {
                    let expected_state = expected.block_state_id(x, y, z);
                    let actual_state = actual.block_state_id(x, y, z);
                    if expected_state != actual_state {
                        return Some((x, y, z, expected_state, actual_state));
                    }
                }
            }
        }
        None
    }

    #[test]
    fn canonical_settlement_order_and_source_union_are_bounded() {
        let targets = canonical_targets(&[(1, 1), (0, 0), (1, 0), (0, 1), (1, 1)]);
        assert_eq!(targets, [(0, 0), (1, 0), (0, 1), (1, 1)]);
        let sources = canonical_sources(&targets);
        assert_eq!(sources.len(), 16);
        assert_eq!(sources[0], (-1, -1));
        assert_eq!(sources[9], (2, -1));
        assert!(source_in_target((-1, 1), (0, 0)));
        assert!(!source_in_target((-2, 0), (0, 0)));
    }

    #[test]
    fn source_once_2x2_differential_reports_state_and_mutation_order() {
        let targets = [(0, 0), (1, 0), (0, 1), (1, 1)];
        let scalar_generator = generator();
        let scalar = targets
            .iter()
            .copied()
            .map(|target| (target, scalar_generator.column(target.0, target.1)))
            .collect::<Vec<_>>();
        let source_once = generator().source_once_features(&targets);

        assert_eq!(source_once.requested().len(), targets.len());
        assert_eq!(source_once.source_execution_count(), 16);
        assert!(source_once
            .source_execution_counts()
            .iter()
            .all(|entry| entry.count == 1));

        for ((target, expected), actual) in scalar.iter().zip(source_once.requested()) {
            let mismatch = first_state_mismatch(expected, actual.column());
            eprintln!(
                "source-once differential target={target:?} scalar_digest={:02x?} shared_digest={:02x?} first_mismatch={mismatch:?}",
                digest(expected),
                digest(actual.column()),
            );
            if *target == (0, 1) {
                assert!(
                    mismatch.is_some(),
                    "the shared absolute source contract is expected to diverge from scalar"
                );
            }
        }
        assert!(source_once.mutable_write_count() > 0);
        assert!(source_once.region_retained_bytes() > 0);
    }

    #[test]
    fn source_once_split_and_input_order_are_invariant() {
        let ordered = [(0, 0), (1, 0), (0, 1), (1, 1)];
        let split = [(1, 1), (0, 1), (1, 0), (0, 0), (1, 1)];
        let first = generator().source_once_features(&ordered);
        let second = generator().source_once_features(&split);
        assert_eq!(first.source_execution_count(), second.source_execution_count());
        assert_eq!(first.executions(), second.executions());
        assert_eq!(first.global_overrides(), second.global_overrides());
        let first_digests = first
            .requested()
            .iter()
            .map(|result| (result.target(), digest(result.column())))
            .collect::<Vec<_>>();
        let second_digests = second
            .requested()
            .iter()
            .map(|result| (result.target(), digest(result.column())))
            .collect::<Vec<_>>();
        assert_eq!(first_digests, second_digests);
    }

    #[test]
    fn source_once_capture_is_optional_and_keeps_one_negative_coordinate_last_writer() {
        assert_eq!(
            std::mem::size_of::<super::SourceOnceMutation>(),
            24,
            "source-once provenance should remain coordinates, position, and canonical id",
        );
        let targets = [(-1, -1), (0, -1)];
        let without_capture = generator().source_once_features(&targets);
        assert!(without_capture.global_overrides().is_empty());
        assert!(without_capture
            .executions()
            .iter()
            .all(|execution| execution.mutations.is_empty()));

        let captured = generator().source_once_features_with_capture(&targets, true);
        assert!(!captured.global_overrides().is_empty());
        assert!(captured.mutable_writes() > captured.global_overrides().len());
        assert!(captured
            .global_overrides()
            .iter()
            .all(|mutation| mutation.state.name().starts_with("minecraft:")));
        let mut positions = HashSet::with_capacity(captured.global_overrides().len());
        for mutation in captured.global_overrides() {
            assert!(positions.insert(mutation.position));
            assert!(captured
                .executions()
                .iter()
                .any(|execution| execution.source == mutation.source
                    && execution.mutations.contains(mutation)));
        }
        assert_eq!(
            captured
                .executions()
                .iter()
                .map(|execution| execution.mutations.len())
                .sum::<usize>(),
            captured.global_overrides().len()
        );
        let uncaptured_digests = without_capture
            .requested()
            .iter()
            .map(|result| (result.target(), digest(result.column())))
            .collect::<Vec<_>>();
        let captured_digests = captured
            .requested()
            .iter()
            .map(|result| (result.target(), digest(result.column())))
            .collect::<Vec<_>>();
        assert_eq!(uncaptured_digests, captured_digests);
    }

}
