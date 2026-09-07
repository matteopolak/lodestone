//! Replays a captured chunk-generation lifecycle against the production
//! worldgen sources.
//!
//! The lifecycle capture and manifest formats remain gate-specific, but the
//! source adapters and resident materializer are reusable parity tooling.  A
//! source completion runs the same worldgen dispatcher that the integrated
//! server uses, then applies its absolute transitions to every resident column
//! reached by the write.  State strings cross the generator boundary because
//! palette ids belong to one generator instance.

use std::collections::{BTreeMap, BTreeSet};

use lodestone_server::{
    ChunkColumn, ChunkGenerationStage, ChunkSource, NetherChunkSource, OverworldChunkSource,
};
use lodestone_worldgen::overworld::{GeneratedBlockEntity, OverworldGenerator};

/// A chunk coordinate used by lifecycle replay.
pub type ChunkPos = (i32, i32);

/// An absolute block coordinate used by lifecycle replay.
pub type AbsoluteCell = (i32, i32, i32);

/// A completion stage captured from the external scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LifecycleCompletion {
    /// The source's feature body completed and its writes are observable.
    Features,
    /// The source reached the final status used by capture telemetry.
    Full,
}

/// One final write emitted by a production source completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleSpill {
    /// The source chunk whose body produced this write.
    pub source: ChunkPos,
    /// The absolute block coordinate of the write.
    pub position: AbsoluteCell,
    /// The canonical block-state string after the write.
    pub state: String,
}

/// One complete FEATURES result emitted by a production source dispatcher.
#[derive(Debug, Default)]
pub struct LifecycleFeatureResult {
    /// Block-state transitions, including writes into neighbouring chunks.
    pub spills: Vec<LifecycleSpill>,
    /// Generated block entities carried with the source result.
    pub block_entities: Vec<GeneratedBlockEntity>,
}

/// Production source boundary consumed by the lifecycle materializer.
///
/// Implementations provide the shaped prefix and invoke the existing
/// source-filtered worldgen dispatcher.  The replay state machine owns only
/// admission, completion deduplication and applying the resulting transitions.
pub trait LifecycleWorldgenSource {
    /// Materialize the source's shaped prefix.
    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn;

    /// Run one source's complete FEATURES body against resident overrides.
    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, String>,
    ) -> LifecycleFeatureResult;

    /// Run any source-local stage after FEATURES. The Nether has no such
    /// stage, so its implementation keeps the default empty result.
    fn post_features_spills(
        &self,
        _source: ChunkPos,
        _overrides: &BTreeMap<AbsoluteCell, String>,
    ) -> Vec<LifecycleSpill> {
        Vec::new()
    }
}

fn override_vec(overrides: &BTreeMap<AbsoluteCell, String>) -> Vec<(i32, i32, i32, String)> {
    overrides
        .iter()
        .map(|(&(x, y, z), state)| (x, y, z, state.clone()))
        .collect()
}

impl LifecycleWorldgenSource for OverworldChunkSource {
    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.column_at(cx, cz, ChunkGenerationStage::Shaped)
    }

    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, String>,
    ) -> LifecycleFeatureResult {
        let overrides = override_vec(overrides);
        let result = self.generator().parity_source_decoration_with_overrides(
            source.0,
            source.1,
            source.0,
            source.1,
            &overrides,
        );
        LifecycleFeatureResult {
            spills: result
                .spills
                .into_iter()
                .map(|spill| LifecycleSpill {
                    source: spill.source,
                    position: spill.position,
                    state: spill.state,
                })
                .collect(),
            block_entities: result.block_entities,
        }
    }

    fn post_features_spills(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, String>,
    ) -> Vec<LifecycleSpill> {
        top_layer_spills(self.generator(), source, overrides)
    }
}

impl LifecycleWorldgenSource for NetherChunkSource {
    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
        ChunkColumn::from_nether(
            self.generator().column_shaped(cx, cz),
            Self::WINDOW_HEIGHT,
        )
    }

    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, String>,
    ) -> LifecycleFeatureResult {
        let overrides = override_vec(overrides);
        let spills = self
            .generator()
            .parity_source_spills_with_overrides(
                source.0,
                source.1,
                source.0,
                source.1,
                &overrides,
            )
            .into_iter()
            .map(|spill| LifecycleSpill {
                source: spill.source,
                position: spill.position,
                state: spill.state,
            })
            .collect();
        LifecycleFeatureResult {
            spills,
            block_entities: Vec::new(),
        }
    }
}

/// Run one source-local Overworld top-layer pass with resident overrides.
///
/// This is the parity library's narrow top-layer seam. It deliberately calls
/// the same production stage as the lifecycle source adapter; it does not
/// duplicate the top-layer algorithm or provide a cold convenience path.
#[must_use]
pub fn top_layer_spills(
    generator: &OverworldGenerator,
    source: ChunkPos,
    overrides: &BTreeMap<AbsoluteCell, String>,
) -> Vec<LifecycleSpill> {
    let overrides = override_vec(overrides);
    generator
        .parity_source_top_layer_spills_with_overrides(source.0, source.1, &overrides)
        .into_iter()
        .map(|spill| LifecycleSpill {
            source: spill.source,
            position: spill.position,
            state: spill.state,
        })
        .collect()
}

/// Stateful resident-column materializer for one authenticated lifecycle
/// capture.
///
/// `complete` runs one globally unique FEATURES event. The source body runs
/// once against its source-centred production dispatcher, and each emitted
/// transition is then applied to the resident destination column. The caller
/// admits the complete halo before replay so a source may write to a neighbour
/// whose explicit ticket appears later in the capture.
pub struct LifecycleMaterializer<S> {
    source: S,
    resident: BTreeMap<ChunkPos, ChunkColumn>,
    /// Completion identity is the source and stage. The captured sequence is
    /// telemetry and must not allow the same stage to run twice.
    completions: BTreeSet<(ChunkPos, LifecycleCompletion)>,
    overrides: BTreeMap<AbsoluteCell, String>,
}

impl<S> std::fmt::Debug for LifecycleMaterializer<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LifecycleMaterializer")
            .field("resident_columns", &self.resident.len())
            .field("completions", &self.completions.len())
            .field("overrides", &self.overrides.len())
            .finish_non_exhaustive()
    }
}

impl<S: LifecycleWorldgenSource> LifecycleMaterializer<S> {
    /// Create an empty materializer around one production source.
    #[must_use]
    pub fn new(source: S) -> Self {
        Self {
            source,
            resident: BTreeMap::new(),
            completions: BTreeSet::new(),
            overrides: BTreeMap::new(),
        }
    }

    /// Admit one shaped resident column.
    pub fn admit(&mut self, chunk: ChunkPos) {
        assert!(
            self.resident
                .insert(chunk, self.source.shaped_column(chunk.0, chunk.1))
                .is_none(),
            "duplicate lifecycle admission for {chunk:?}"
        );
    }

    /// Apply one captured completion event.
    pub fn complete(
        &mut self,
        source: ChunkPos,
        stage: LifecycleCompletion,
        sequence: u64,
    ) {
        assert!(
            self.resident.contains_key(&source),
            "lifecycle completion source {source:?} was not admitted before sequence {sequence}"
        );
        assert!(
            self.completions.insert((source, stage)),
            "duplicate lifecycle completion for {source:?} at sequence {sequence} ({stage:?})"
        );
        if stage == LifecycleCompletion::Full {
            return;
        }

        let result = self.source.feature_result(source, &self.overrides);
        for spill in &result.spills {
            assert_eq!(
                spill.source, source,
                "production spill source disagrees with completion source"
            );
            let destination = (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            );
            if let Some(column) = self.resident.get(&destination) {
                assert!(
                    column.contains_y(spill.position.1),
                    "production spill {spill:?} is outside resident column {destination:?}"
                );
            }
        }
        for entity in &result.block_entities {
            let (x, y, z) = entity.position();
            let destination = (x.div_euclid(16), z.div_euclid(16));
            if let Some(column) = self.resident.get(&destination) {
                assert!(
                    column.contains_y(y),
                    "production block entity {:?} is outside resident column {destination:?}",
                    entity.type_id()
                );
            }
        }
        for spill in result.spills {
            let destination = (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            );
            let Some(column) = self.resident.get_mut(&destination) else {
                continue;
            };
            column.set_block(
                spill.position.0.rem_euclid(16),
                spill.position.1,
                spill.position.2.rem_euclid(16),
                &spill.state,
            );
            self.overrides.insert(spill.position, spill.state);
        }
        for entity in result.block_entities {
            let (x, _y, z) = entity.position();
            let destination = (x.div_euclid(16), z.div_euclid(16));
            let Some(column) = self.resident.get_mut(&destination) else {
                continue;
            };
            column.add_generated_block_entities(std::slice::from_ref(&entity));
        }

        let post_features_spills = self.source.post_features_spills(source, &self.overrides);
        for spill in &post_features_spills {
            assert_eq!(
                spill.source, source,
                "production post-FEATURES spill source disagrees with completion source"
            );
            let destination = (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            );
            if let Some(column) = self.resident.get(&destination) {
                assert!(
                    column.contains_y(spill.position.1),
                    "production post-FEATURES spill {spill:?} is outside resident column {destination:?}"
                );
            }
        }
        for spill in post_features_spills {
            let destination = (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            );
            let Some(column) = self.resident.get_mut(&destination) else {
                continue;
            };
            column.set_block(
                spill.position.0.rem_euclid(16),
                spill.position.1,
                spill.position.2.rem_euclid(16),
                &spill.state,
            );
            self.overrides.insert(spill.position, spill.state);
        }
    }

    /// Borrow one admitted resident column for neighbour-aware encoding.
    #[must_use]
    pub fn resident_column(&self, chunk: ChunkPos) -> Option<&ChunkColumn> {
        self.resident.get(&chunk)
    }

    /// Clone one admitted resident column for packet encoding.
    #[must_use]
    pub fn snapshot_for_packet(&self, target: ChunkPos) -> ChunkColumn {
        self.resident
            .get(&target)
            .cloned()
            .unwrap_or_else(|| panic!("lifecycle target {target:?} was not admitted before encoding"))
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use super::*;

    struct CountingSource {
        feature_calls: Rc<Cell<usize>>,
    }

    impl LifecycleWorldgenSource for CountingSource {
        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn feature_result(
            &self,
            _source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, String>,
        ) -> LifecycleFeatureResult {
            let calls = self.feature_calls.get() + 1;
            self.feature_calls.set(calls);
            assert_eq!(calls, 1, "source body ran twice");
            LifecycleFeatureResult::default()
        }
    }

    #[test]
    #[should_panic(expected = "duplicate lifecycle completion")]
    fn duplicate_features_with_a_new_sequence_are_rejected_before_replay() {
        let feature_calls = Rc::new(Cell::new(0));
        let mut materializer = LifecycleMaterializer::new(CountingSource {
            feature_calls: Rc::clone(&feature_calls),
        });
        materializer.admit((0, 0));
        materializer.complete((0, 0), LifecycleCompletion::Features, 10);
        assert_eq!(feature_calls.get(), 1);
        materializer.complete((0, 0), LifecycleCompletion::Features, 11);
    }
}
