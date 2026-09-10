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
    ChunkColumn, ChunkGenerationStage, ChunkSource, EndChunkSource, NetherChunkSource,
    OverworldChunkSource,
};
use lodestone_worldgen::overworld::{GeneratedBlockEntity, OverworldGenerator};

/// A chunk coordinate used by lifecycle replay.
pub type ChunkPos = (i32, i32);

/// An absolute block coordinate used by lifecycle replay.
pub type AbsoluteCell = (i32, i32, i32);

/// Horizontal chunk radius sampled by an initial packet's light encoder.
pub const PACKET_LIGHT_RADIUS: i32 = 1;

/// Maximum horizontal chunk radius touched by one FEATURES source write.
pub const FEATURES_WRITE_RADIUS: i32 = 2;

/// Horizontal chunk radius whose source bodies are dispatched for one target.
pub const FEATURES_SOURCE_RADIUS: i32 = 1;

/// Maximum source-to-source distance of a mutable dependency.
pub const MUTABLE_READ_RADIUS: i32 = 4;

// The reverse frontier already accounts for the selected source's write halo;
// the remaining two chunks are the audited mutable-read contribution.
const BACKWARD_FRONTIER_RADIUS: i32 = MUTABLE_READ_RADIUS - FEATURES_WRITE_RADIUS;
const ADMITTED_DESTINATION_RADIUS: i32 = 2;

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
    /// Whether this source-crossing fungus write was transient during feature
    /// execution. Ordered lifecycle replay retains it for later reads and
    /// packet materialization.
    pub transient: bool,
}

/// One complete FEATURES result emitted by a production source dispatcher.
#[derive(Debug, Default)]
pub struct LifecycleFeatureResult {
    /// Block-state transitions, including writes into neighbouring chunks.
    pub spills: Vec<LifecycleSpill>,
    /// Generated block entities carried with the source result.
    pub block_entities: Vec<GeneratedBlockEntity>,
    /// End return-gateway sidecars emitted with the source's block writes.
    pub end_gateways: Vec<lodestone_worldgen::end::EndGateway>,
}

/// One authenticated FEATURES event accepted by a target replay plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleReplayEvent {
    pub source: ChunkPos,
    pub stage: LifecycleCompletion,
    pub sequence: u64,
}

/// Static, validated dependency closure for one target packet.
#[derive(Debug, Clone)]
pub struct LifecycleReplayPlan {
    target: ChunkPos,
    admissions: Vec<ChunkPos>,
    feature_events: Vec<LifecycleReplayEvent>,
}

impl LifecycleReplayPlan {
    /// Build a target plan from a complete authenticated admission/event stream.
    /// The reverse walk preserves event order while selecting only earlier
    /// sources whose writes can affect the packet or a selected dependency.
    pub fn for_target(
        target: ChunkPos,
        admissions: &[ChunkPos],
        feature_events: &[LifecycleReplayEvent],
    ) -> Result<Self, String> {
        if admissions.is_empty() {
            return Err("lifecycle replay capture has no admissions".to_owned());
        }
        let admitted = admissions.iter().copied().collect::<BTreeSet<_>>();
        if admitted.len() != admissions.len() {
            return Err("lifecycle replay admissions contain a duplicate coordinate".to_owned());
        }
        let min_x = admitted.iter().map(|&(x, _)| x).min().unwrap();
        let max_x = admitted.iter().map(|&(x, _)| x).max().unwrap();
        let min_z = admitted.iter().map(|&(_, z)| z).min().unwrap();
        let max_z = admitted.iter().map(|&(_, z)| z).max().unwrap();
        let expected_count = usize::try_from(i64::from(max_x - min_x + 1) * i64::from(max_z - min_z + 1))
            .map_err(|_| "lifecycle replay admission rectangle is too large".to_owned())?;
        if expected_count != admissions.len()
            || (min_z..=max_z)
                .flat_map(|z| (min_x..=max_x).map(move |x| (x, z)))
                .any(|chunk| !admitted.contains(&chunk))
        {
            return Err(format!(
                "lifecycle replay admissions are not a complete rectangle ({min_x}..={max_x}, {min_z}..={max_z})",
            ));
        }
        let mut packet_domain = BTreeSet::new();
        for x in target.0 - PACKET_LIGHT_RADIUS..=target.0 + PACKET_LIGHT_RADIUS {
            for z in target.1 - PACKET_LIGHT_RADIUS..=target.1 + PACKET_LIGHT_RADIUS {
                packet_domain.insert((x, z));
            }
        }
        if packet_domain.iter().any(|chunk| !admitted.contains(chunk)) {
            return Err(format!("target {target:?} does not have a complete admitted 3x3 packet-light domain"));
        }
        if feature_events.len() != admissions.len() {
            return Err(format!(
                "expected one FEATURES event per admission, got {} events for {} admissions",
                feature_events.len(), admissions.len()
            ));
        }
        let mut seen_sources = BTreeSet::new();
        for (rank, event) in feature_events.iter().enumerate() {
            if event.stage != LifecycleCompletion::Features {
                return Err(format!("lifecycle replay event {} is not FEATURES", event.sequence));
            }
            if rank > 0 && feature_events[rank - 1].sequence >= event.sequence {
                return Err(format!("lifecycle replay completion order is not increasing at sequence {}", event.sequence));
            }
            if admissions[rank] != event.source {
                return Err(format!("lifecycle replay event {} source {:?} disagrees with admission {:?}", event.sequence, event.source, admissions[rank]));
            }
            if !seen_sources.insert(event.source) {
                return Err(format!("lifecycle replay source {:?} appears more than once", event.source));
            }
        }

        let mut event_frontier = packet_domain.clone();
        let mut selected_sources = BTreeSet::new();
        for event in feature_events.iter().rev() {
            let reaches = event_frontier.iter().any(|destination| {
                (event.source.0 - destination.0).abs().max((event.source.1 - destination.1).abs())
                    <= FEATURES_WRITE_RADIUS
            });
            if !reaches {
                continue;
            }
            selected_sources.insert(event.source);
            for x in event.source.0 - BACKWARD_FRONTIER_RADIUS..=event.source.0 + BACKWARD_FRONTIER_RADIUS {
                for z in event.source.1 - BACKWARD_FRONTIER_RADIUS..=event.source.1 + BACKWARD_FRONTIER_RADIUS {
                    event_frontier.insert((x, z));
                }
            }
        }
        let mut destination_frontier = packet_domain;
        for event in feature_events.iter().filter(|event| selected_sources.contains(&event.source)) {
            for x in event.source.0 - ADMITTED_DESTINATION_RADIUS..=event.source.0 + ADMITTED_DESTINATION_RADIUS {
                for z in event.source.1 - ADMITTED_DESTINATION_RADIUS..=event.source.1 + ADMITTED_DESTINATION_RADIUS {
                    destination_frontier.insert((x, z));
                }
            }
        }
        let admissions = admissions.iter().copied().filter(|chunk| destination_frontier.contains(chunk)).collect::<Vec<_>>();
        let feature_events = feature_events.iter().copied().filter(|event| selected_sources.contains(&event.source)).collect::<Vec<_>>();
        let destination_set = admissions.iter().copied().collect::<BTreeSet<_>>();
        if selected_sources.iter().any(|source| !destination_set.contains(source)) {
            return Err(format!("target {target:?} dependency closure has an unadmitted source destination"));
        }
        Ok(Self { target, admissions, feature_events })
    }

    #[must_use]
    pub const fn target(&self) -> ChunkPos { self.target }

    #[must_use]
    pub fn admissions(&self) -> &[ChunkPos] { &self.admissions }

    #[must_use]
    pub fn feature_events(&self) -> &[LifecycleReplayEvent] { &self.feature_events }
}

/// Production source boundary consumed by the lifecycle materializer.
///
/// Implementations provide the shaped prefix and invoke the existing
/// source-filtered worldgen dispatcher.  The replay state machine owns only
/// admission, completion deduplication and applying the resulting transitions.
pub trait LifecycleWorldgenSource {
    /// Prepare immutable source caches for the complete admitted replay. The
    /// default keeps sources with no replay-specific cache unchanged.
    fn prepare_lifecycle_replay(&mut self, _admissions: &[ChunkPos]) {}

    /// Optional source-specific computation count for replay controls.
    fn lifecycle_pre_decoration_computations(&self) -> Option<usize> {
        None
    }

    /// Materialize the source's shaped prefix.
    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn;

    /// Run one source's complete FEATURES body against resident overrides.
    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, String>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult;

    /// Run one source's FEATURES body for a requested target packet.  The
    /// source controls its decoration seed, while the requested target owns
    /// the read context and resident view.  Direct completion controls retain
    /// the historical source-centred default; authenticated target replay
    /// supplies this distinction explicitly.
    fn feature_result_for_target(
        &self,
        _target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, String>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        self.feature_result(source, overrides, resident)
    }

    /// Run any source-local stage after FEATURES. The Nether has no such
    /// stage, so its implementation keeps the default empty result.
    fn post_features_spills(
        &self,
        _source: ChunkPos,
        _overrides: &BTreeMap<AbsoluteCell, String>,
    ) -> Vec<LifecycleSpill> {
        Vec::new()
    }

    /// Attach End-only gateway metadata after its source completion.
    fn attach_end_gateways(
        &self,
        _column: &mut ChunkColumn,
        gateways: &[lodestone_worldgen::end::EndGateway],
    ) {
        assert!(gateways.is_empty(), "non-End lifecycle source emitted End gateways");
    }

    /// Attach source-owned save sidecars after the source's feature body.
    ///
    /// Most sources have no sidecars at this boundary. End structures carry
    /// their starts, references, and container entities through the source
    /// rather than through block-state spills, so the End adapter overrides
    /// this hook.
    fn attach_source_sidecars(&self, _source: ChunkPos, _column: &mut ChunkColumn) {}

    /// Finalize the detached packet snapshot after every admitted source has
    /// completed. Sources may keep richer runtime sidecars in their resident
    /// columns while the wire view uses the state-owned records that existed
    /// before gameplay-only payload generation.
    fn finalize_packet_snapshot(&self, _target: ChunkPos, _column: &mut ChunkColumn) {}
}

fn override_vec(overrides: &BTreeMap<AbsoluteCell, String>) -> Vec<(i32, i32, i32, String)> {
    overrides
        .iter()
        .map(|(&(x, y, z), state)| (x, y, z, state.clone()))
        .collect()
}

impl LifecycleWorldgenSource for OverworldChunkSource {
    fn prepare_lifecycle_replay(&mut self, admissions: &[ChunkPos]) {
        self.generator().prepare_lifecycle_replay(admissions);
    }

    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.column_at(cx, cz, ChunkGenerationStage::Shaped)
    }

    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, String>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
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
                    transient: false,
                })
                .collect(),
            block_entities: result.block_entities,
            end_gateways: Vec::new(),
        }
    }

    fn feature_result_for_target(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, String>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        let overrides = override_vec(overrides);
        let result = self.generator().parity_source_decoration_with_overrides(
            target.0,
            target.1,
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
                    transient: false,
                })
                .collect(),
            block_entities: result.block_entities,
            end_gateways: Vec::new(),
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
    fn prepare_lifecycle_replay(&mut self, admissions: &[ChunkPos]) {
        self.generator().prepare_lifecycle_replay(admissions);
    }

    fn lifecycle_pre_decoration_computations(&self) -> Option<usize> {
        Some(self.generator().pre_decoration_computations())
    }

    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
        ChunkColumn::from_nether_at(
            self.generator().column_shaped(cx, cz),
            Self::WINDOW_HEIGHT,
            ChunkGenerationStage::Shaped,
        )
    }

    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, String>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        let interner = std::sync::Arc::clone(self.generator().interner());
        let spills = self
            .generator()
            .parity_source_spills_with_resident(source.0, source.1, |cx, cz| {
                let column = resident.get(&(cx, cz))?;
                Some(lodestone_worldgen::dense_grid::DenseBlockGrid::from_canonical_states(
                    std::sync::Arc::clone(&interner),
                    cx * 16,
                    0,
                    cz * 16,
                    16,
                    NetherChunkSource::WINDOW_HEIGHT,
                    16,
                    |x, y, z| {
                        column.resolved_block_state_id(
                            x.rem_euclid(16),
                            y,
                            z.rem_euclid(16),
                        )
                    },
                ))
            })
            .into_iter()
            .map(|spill| LifecycleSpill {
                source: spill.source,
                position: spill.position,
                state: spill.state,
                transient: spill.transient,
            })
            .collect();
        LifecycleFeatureResult {
            spills,
            block_entities: Vec::new(),
            end_gateways: Vec::new(),
        }
    }
}

impl LifecycleWorldgenSource for EndChunkSource {
    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.shaped_column(cx, cz)
    }

    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, String>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        let overrides = override_vec(overrides);
        let result = self.generator().parity_source_decoration_with_overrides(
            source.0,
            source.1,
            &overrides,
        );
        LifecycleFeatureResult {
            spills: result.spills.into_iter().map(|spill| LifecycleSpill {
                source: spill.source,
                position: spill.position,
                state: spill.state,
                transient: false,
            }).collect(),
            block_entities: Vec::new(),
            end_gateways: result.gateways,
        }
    }

    fn attach_end_gateways(
        &self,
        column: &mut ChunkColumn,
        gateways: &[lodestone_worldgen::end::EndGateway],
    ) {
        self.attach_generated_gateways(column, gateways);
    }

    fn attach_source_sidecars(&self, source: ChunkPos, column: &mut ChunkColumn) {
        self.attach_structures(column, source.0, source.1);
    }

    fn finalize_packet_snapshot(&self, target: ChunkPos, column: &mut ChunkColumn) {
        self.finalize_packet_snapshot_for_packet(column, target.0, target.1);
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
            transient: false,
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

    /// Prepare source-local immutable caches before admission begins. The
    /// caller supplies the authenticated replay admissions, not the bounded
    /// packet target prefix: all source completions still run in order.
    pub fn prepare_lifecycle_replay(&mut self, admissions: &[ChunkPos]) {
        self.source.prepare_lifecycle_replay(admissions);
    }

    /// Clear the resident replay state before starting another bounded batch.
    ///
    /// The production source remains owned by this materializer, so its staged
    /// generation caches survive between batches while resident columns and
    /// read-after-write overrides retain the same batch boundary as a fresh
    /// materializer. Call [`Self::prepare_lifecycle_replay`] after this reset.
    pub fn reset_for_lifecycle_replay(&mut self) {
        self.resident.clear();
        self.completions.clear();
        self.overrides.clear();
    }

    /// Read the optional source computation counter used by lifecycle parity
    /// controls. It has no effect on replay state or generated output.
    #[must_use]
    pub fn lifecycle_pre_decoration_computations(&self) -> Option<usize> {
        self.source.lifecycle_pre_decoration_computations()
    }

    /// Admit and apply a validated target plan without changing its order.
    pub fn replay_plan(&mut self, plan: &LifecycleReplayPlan) {
        for &admission in plan.admissions() {
            self.admit(admission);
        }
        for event in plan.feature_events() {
            self.complete_for_target(plan.target(), event.source, event.stage, event.sequence);
        }
    }

    /// Admit one shaped resident column.
    pub fn admit(&mut self, chunk: ChunkPos) {
        let column = self.source.shaped_column(chunk.0, chunk.1);
        self.admit_shaped(chunk, column);
    }

    /// Generate independent shaped admissions on the server's process-wide
    /// worker dispatcher, then commit them in the supplied canonical order.
    /// No mutable lifecycle state is visible to workers: FEATURES completions
    /// still run serially through [`Self::complete`] because each one observes
    /// the prior source's spills.
    pub fn admit_many_parallel(&mut self, chunks: &[ChunkPos])
    where
        S: Sync,
    {
        let source = &self.source;
        let jobs = chunks.to_vec();
        let columns = lodestone_server::run_worldgen_jobs(jobs.clone(), |(cx, cz)| {
            source.shaped_column(cx, cz)
        });
        assert_eq!(columns.len(), jobs.len(), "worldgen dispatcher changed admission count");
        for (chunk, column) in jobs.into_iter().zip(columns) {
            self.admit_shaped(chunk, column);
        }
    }

    fn admit_shaped(&mut self, chunk: ChunkPos, column: ChunkColumn) {
        assert!(
            self.resident.insert(chunk, column).is_none(),
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
        self.complete_observing(source, stage, sequence, |_| {});
    }

    /// Apply one captured completion event and observe every resulting state
    /// transition in application order. The observer is diagnostic only: it
    /// cannot alter the resident world or the read-after-write overlay.
    pub fn complete_observing(
        &mut self,
        source: ChunkPos,
        stage: LifecycleCompletion,
        sequence: u64,
        mut observe: impl FnMut(&LifecycleSpill),
    ) {
        self.complete_observing_for_target(source, source, stage, sequence, observe);
    }

    fn complete_for_target(
        &mut self,
        target: ChunkPos,
        source: ChunkPos,
        stage: LifecycleCompletion,
        sequence: u64,
    ) {
        self.complete_observing_for_target(target, source, stage, sequence, |_| {});
    }

    fn complete_observing_for_target(
        &mut self,
        target: ChunkPos,
        source: ChunkPos,
        stage: LifecycleCompletion,
        sequence: u64,
        mut observe: impl FnMut(&LifecycleSpill),
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

        // Heightmaps become live when the source enters FEATURES, before its
        // own feature body writes anything. A neighbour that entered earlier
        // already has live maps, so the same spill updates that destination
        // incrementally through `ChunkColumn::set_block` below.
        self.resident
            .get_mut(&source)
            .expect("lifecycle source was checked resident above")
            .prime_client_heightmaps();

        let result = self.source.feature_result_for_target(
            target,
            source,
            &self.overrides,
            &self.resident,
        );
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
        for gateway in &result.end_gateways {
            let destination = (gateway.pos.0.div_euclid(16), gateway.pos.2.div_euclid(16));
            if let Some(column) = self.resident.get_mut(&destination) {
                self.source.attach_end_gateways(column, std::slice::from_ref(gateway));
            }
        }
        for entity in &result.block_entities {
            let (x, _y, z) = entity.position();
            let destination = (x.div_euclid(16), z.div_euclid(16));
            let Some(column) = self.resident.get_mut(&destination) else {
                continue;
            };
            column.add_generated_block_entities(std::slice::from_ref(entity));
        }
        for spill in result.spills {
            observe(&spill);
            let destination = (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            );
            // Keep every source write in the read-after-write map, including
            // writes outside the admitted destination rectangle.  The
            // materializer admits only the packet target's bounded halo, but
            // a source on that halo's edge can spill one chunk farther.  That
            // outside write is still part of the shared resident world: a
            // later source can read it through its wider feature context even
            // though the final packet never encodes that destination.
            self.overrides.insert(spill.position, spill.state.clone());
            if let Some(column) = self.resident.get_mut(&destination) {
                column.set_block(
                    spill.position.0.rem_euclid(16),
                    spill.position.1,
                    spill.position.2.rem_euclid(16),
                    &spill.state,
                );
            }
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
            observe(&spill);
            let destination = (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            );
            self.overrides.insert(spill.position, spill.state.clone());
            if let Some(column) = self.resident.get_mut(&destination) {
                column.set_block(
                    spill.position.0.rem_euclid(16),
                    spill.position.1,
                    spill.position.2.rem_euclid(16),
                    &spill.state,
                );
            }
        }
        self.source.attach_source_sidecars(
            source,
            self.resident
                .get_mut(&source)
                .expect("lifecycle source was admitted before sidecar attachment"),
        );
    }

    /// Borrow one admitted resident column for neighbour-aware encoding.
    #[must_use]
    pub fn resident_column(&self, chunk: ChunkPos) -> Option<&ChunkColumn> {
        self.resident.get(&chunk)
    }

    /// Clone one admitted resident column for packet encoding.
    #[must_use]
    pub fn snapshot_for_packet(&self, target: ChunkPos) -> ChunkColumn {
        let mut snapshot = self
            .resident
            .get(&target)
            .cloned()
            .unwrap_or_else(|| panic!("lifecycle target {target:?} was not admitted before encoding"));
        // A later source completion can write a state-owned block entity into
        // this target after the target's own sidecar hook ran. Finalize only
        // the detached packet snapshot so the resident lifecycle remains the
        // authenticated replay state and richer generated payloads survive.
        self.source.finalize_packet_snapshot(target, &mut snapshot);
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use super::*;

    struct CountingSource {
        feature_calls: Rc<Cell<usize>>,
    }

    struct SpillSource {
        expected_override: Rc<Cell<bool>>,
        post_features: bool,
    }

    struct StateOnlySpillSource;

    struct HeightmapLifecycleSource;

    impl LifecycleWorldgenSource for StateOnlySpillSource {
        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(-64, 384)
        }

        fn feature_result(
            &self,
            source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, String>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            LifecycleFeatureResult {
                spills: (source == (0, 0)).then(|| LifecycleSpill {
                    source,
                    position: (4, 14, 1),
                    state: "minecraft:spawner".to_owned(),
                    transient: false,
                }).into_iter().collect(),
                block_entities: Vec::new(),
            end_gateways: Vec::new(),
            }
        }
    }

    impl LifecycleWorldgenSource for HeightmapLifecycleSource {
        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            let mut column = ChunkColumn::new(0, 8);
            column.set_block(0, 0, 0, "minecraft:stone");
            column
        }

        fn feature_result(
            &self,
            source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, String>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            let position = match source {
                // The first source writes into itself and the unprimed east
                // neighbour. The second source later writes back into the
                // already-primed west neighbour.
                (0, 0) => (16, 4, 0),
                (1, 0) => (0, 6, 0),
                _ => return LifecycleFeatureResult::default(),
            };
            LifecycleFeatureResult {
                spills: vec![LifecycleSpill {
                    source,
                    position,
                    state: "minecraft:stone".to_owned(),
                    transient: false,
                }],
                block_entities: Vec::new(),
            end_gateways: Vec::new(),
            }
        }
    }

    fn world_surface(column: &ChunkColumn) -> u32 {
        column
            .client_heightmaps()
            .expect("FEATURES entry primes client maps")
            .get(1)
            .expect("WORLD_SURFACE map")
            .get(0, 0)
    }

    #[test]
    fn heightmaps_prime_at_own_features_entry_and_track_later_neighbour_writes() {
        let mut materializer = LifecycleMaterializer::new(HeightmapLifecycleSource);
        materializer.admit((0, 0));
        materializer.admit((1, 0));

        materializer.complete((0, 0), LifecycleCompletion::Features, 0);
        assert_eq!(world_surface(materializer.resident_column((0, 0)).unwrap()), 1);
        assert!(
            materializer
                .resident_column((1, 0))
                .unwrap()
                .client_heightmaps()
                .is_none(),
            "a source write must not prime an unentered destination"
        );

        materializer.complete((1, 0), LifecycleCompletion::Features, 1);
        assert_eq!(
            world_surface(materializer.resident_column((1, 0)).unwrap()),
            5,
            "own entry must include writes that arrived before priming"
        );
        assert_eq!(
            world_surface(materializer.resident_column((0, 0)).unwrap()),
            7,
            "a later neighbour write must update an already-primed map"
        );
    }

    #[test]
    fn state_only_spill_does_not_invent_a_lifecycle_block_entity() {
        let mut materializer = LifecycleMaterializer::new(StateOnlySpillSource);
        materializer.admit((0, 0));
        materializer.complete((0, 0), LifecycleCompletion::Features, 0);

        let column = materializer.resident_column((0, 0)).expect("admitted source column");
        assert_eq!(column.block_state(4, 14, 1), "minecraft:spawner");
        assert!(column.block_entities().is_empty());
    }

    impl LifecycleWorldgenSource for SpillSource {
        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn feature_result(
            &self,
            source: ChunkPos,
            overrides: &BTreeMap<AbsoluteCell, String>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            if source == (0, 1) {
                self.expected_override.set(
                    overrides.get(&(16, 0, 0)).map(String::as_str) == Some("minecraft:stone"),
                );
            }
            let spills = if source == (0, 0) && !self.post_features {
                vec![LifecycleSpill {
                    source,
                    position: (16, 0, 0),
                    state: "minecraft:stone".to_owned(),
                    transient: false,
                }]
            } else {
                Vec::new()
            };
            LifecycleFeatureResult { spills, block_entities: Vec::new(), end_gateways: Vec::new() }
        }

        fn post_features_spills(
            &self,
            source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, String>,
        ) -> Vec<LifecycleSpill> {
            if source == (0, 0) && self.post_features {
                vec![LifecycleSpill {
                    source,
                    position: (16, 0, 0),
                    state: "minecraft:stone".to_owned(),
                    transient: false,
                }]
            } else {
                Vec::new()
            }
        }
    }

    impl LifecycleWorldgenSource for CountingSource {
        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn feature_result(
            &self,
            _source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, String>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            let calls = self.feature_calls.get() + 1;
            self.feature_calls.set(calls);
            assert_eq!(calls, 1, "source body ran twice");
            LifecycleFeatureResult::default()
        }
    }

    struct AdmissionSource;

    impl LifecycleWorldgenSource for AdmissionSource {
        fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
            let mut column = ChunkColumn::new(0, 1);
            let state = if (cx + cz) & 1 == 0 {
                "minecraft:stone"
            } else {
                "minecraft:dirt"
            };
            column.set_block(0, 0, 0, state);
            column
        }

        fn feature_result(
            &self,
            _source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, String>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            LifecycleFeatureResult::default()
        }
    }

    struct ReorderSource;

    impl LifecycleWorldgenSource for ReorderSource {
        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn feature_result(
            &self,
            source: ChunkPos,
            overrides: &BTreeMap<AbsoluteCell, String>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            let state = match source {
                (0, 0) => "minecraft:stone",
                (1, 0) if overrides.contains_key(&(16, 0, 0)) => "minecraft:diamond_block",
                (1, 0) => "minecraft:dirt",
                _ => return LifecycleFeatureResult::default(),
            };
            LifecycleFeatureResult {
                spills: vec![LifecycleSpill {
                    source,
                    position: (16, 0, 0),
                    state: state.to_owned(),
                    transient: false,
                }],
                block_entities: Vec::new(),
            end_gateways: Vec::new(),
            }
        }
    }

    #[test]
    fn parallel_admissions_match_serial_generation_and_commit_order() {
        let chunks = vec![(-2, -1), (-1, -1), (0, -1), (1, -1), (-2, 0), (-1, 0), (0, 0), (1, 0)];
        let mut serial = LifecycleMaterializer::new(AdmissionSource);
        for &chunk in &chunks {
            serial.admit(chunk);
        }
        let mut parallel = LifecycleMaterializer::new(AdmissionSource);
        parallel.admit_many_parallel(&chunks);
        for &chunk in &chunks {
            assert_eq!(
                serial.resident_column(chunk).unwrap().block_state(0, 0, 0),
                parallel.resident_column(chunk).unwrap().block_state(0, 0, 0),
                "parallel admission changed shaped output at {chunk:?}",
            );
        }
    }

    #[test]
    fn reordered_feature_commit_changes_state_and_is_not_accepted_as_canonical() {
        let mut canonical = LifecycleMaterializer::new(ReorderSource);
        canonical.admit((0, 0));
        canonical.admit((1, 0));
        canonical.complete((0, 0), LifecycleCompletion::Features, 0);
        canonical.complete((1, 0), LifecycleCompletion::Features, 1);

        let mut reordered = LifecycleMaterializer::new(ReorderSource);
        reordered.admit_many_parallel(&[(0, 0), (1, 0)]);
        reordered.complete((1, 0), LifecycleCompletion::Features, 0);
        reordered.complete((0, 0), LifecycleCompletion::Features, 1);

        let canonical_state = canonical.resident_column((1, 0)).unwrap().block_state(0, 0, 0);
        let reordered_state = reordered.resident_column((1, 0)).unwrap().block_state(0, 0, 0);
        assert_eq!(canonical_state, "minecraft:diamond_block");
        assert_eq!(reordered_state, "minecraft:stone");
        assert_ne!(canonical_state, reordered_state, "reordered mutable commits must change the control output");
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

    #[test]
    fn feature_spills_outside_the_admitted_rectangle_stay_visible_to_later_sources() {
        let expected_override = Rc::new(Cell::new(false));
        let mut materializer = LifecycleMaterializer::new(SpillSource {
            expected_override: Rc::clone(&expected_override),
            post_features: false,
        });
        materializer.admit((0, 0));
        materializer.admit((0, 1));
        materializer.complete((0, 0), LifecycleCompletion::Features, 0);
        assert!(materializer.resident_column((1, 0)).is_none());
        materializer.complete((0, 1), LifecycleCompletion::Features, 1);
        assert!(
            expected_override.get(),
            "a later source must read a feature spill even when its destination is not resident"
        );
    }

    #[test]
    fn post_features_spills_outside_the_admitted_rectangle_stay_visible_to_later_sources() {
        let expected_override = Rc::new(Cell::new(false));
        let mut materializer = LifecycleMaterializer::new(SpillSource {
            expected_override: Rc::clone(&expected_override),
            post_features: true,
        });
        materializer.admit((0, 0));
        materializer.admit((0, 1));
        materializer.complete((0, 0), LifecycleCompletion::Features, 0);
        materializer.complete((0, 1), LifecycleCompletion::Features, 1);
        assert!(
            expected_override.get(),
            "a later source must read a post-FEATURES spill even when its destination is not resident"
        );
    }

    #[test]
    fn target_plan_matches_audited_corner_closure() {
        let mut admissions = Vec::with_capacity(18 * 18);
        for tile_z in 0..=1 {
            for tile_x in 0..=1 {
                let (min_x, max_x) = if tile_x == 0 { (-9, 6) } else { (7, 8) };
                let (min_z, max_z) = if tile_z == 0 { (-9, 6) } else { (7, 8) };
                for z in min_z..=max_z {
                    for x in min_x..=max_x {
                        admissions.push((x, z));
                    }
                }
            }
        }
        let events = admissions.iter().enumerate().map(|(sequence, &source)| LifecycleReplayEvent {
            source,
            stage: LifecycleCompletion::Features,
            sequence: sequence as u64,
        }).collect::<Vec<_>>();
        let plan = LifecycleReplayPlan::for_target((-8, -8), &admissions, &events)
            .expect("complete tiled capture must produce a target plan");
        assert_eq!(plan.feature_events().len(), 59);
        assert_eq!(plan.admissions().len(), 105);

        let expected_source_rows = [(-9, 6), (-9, 6), (-9, 3), (-9, -1), (-9, -5)];
        for (z, (min_x, max_x)) in (-9..=-5).zip(expected_source_rows) {
            let row = plan.feature_events().iter().filter(|event| event.source.1 == z).map(|event| event.source.0).collect::<Vec<_>>();
            assert_eq!(row, (min_x..=max_x).collect::<Vec<_>>(), "selected source row at z={z}");
        }
        let expected_admission_rows = [(-9, 8), (-9, 8), (-9, 8), (-9, 8), (-9, 5), (-9, 1), (-9, -3)];
        for (z, (min_x, max_x)) in (-9..=-3).zip(expected_admission_rows) {
            let row = plan.admissions().iter().filter(|&&(_, admission_z)| admission_z == z).map(|&(x, _)| x).collect::<Vec<_>>();
            assert_eq!(row, (min_x..=max_x).collect::<Vec<_>>(), "admitted destination row at z={z}");
        }
        assert!(plan.feature_events().windows(2).all(|pair| pair[0].sequence < pair[1].sequence));
        assert!((-9..=-7).flat_map(|z| (-9..=-7).map(move |x| (x, z))).all(|chunk| plan.admissions().contains(&chunk)));
        assert!(plan.feature_events().iter().all(|event| plan.admissions().contains(&event.source)));
    }

    #[test]
    fn target_plan_rejects_mutated_event_order() {
        let admissions = (-3..=3)
            .flat_map(|z| (-3..=3).map(move |x| (x, z)))
            .collect::<Vec<_>>();
        let mut events = admissions.iter().enumerate().map(|(sequence, &source)| LifecycleReplayEvent {
            source,
            stage: LifecycleCompletion::Features,
            sequence: sequence as u64,
        }).collect::<Vec<_>>();
        events.swap(0, 1);
        let error = LifecycleReplayPlan::for_target((0, 0), &admissions, &events)
            .expect_err("mutated authenticated event order must fail closed");
        assert!(error.contains("order") || error.contains("disagrees"));
    }

}
