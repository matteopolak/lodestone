//! Replays a captured chunk-generation lifecycle against the production
//! worldgen sources at the server's `ChunkSource` boundary.
//!
//! The lifecycle capture and manifest formats remain gate-specific, but the
//! source adapters and resident materializer are reusable lifecycle tooling.  A
//! source completion runs the same worldgen dispatcher that the integrated
//! server uses, then applies its absolute transitions to every resident column
//! reached by the write. All lifecycle state transitions use canonical ids.

use std::collections::{btree_map::Entry, BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::{
    ChunkColumn, ChunkGenerationStage, ChunkSource, EndChunkSource, NetherChunkSource,
    OverworldChunkSource,
};
use crate::worldgen_session::ProvenanceMutation;
use lodestone_data::block_states::StateId;
use lodestone_worldgen::overworld::{GeneratedBlockEntity, OverworldGenerator};
use lodestone_worldgen::structure::StructureBlocks;
use lodestone_worldgen::stage_schedule::ColumnStage;
use sha2::{Digest, Sha256};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static GENERATED_RESIDENT_UNWRAPS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
fn record_generated_resident_unwrap() {
    GENERATED_RESIDENT_UNWRAPS.with(|count| count.set(count.get() + 1));
}

#[cfg(test)]
fn reset_generated_resident_unwraps() {
    GENERATED_RESIDENT_UNWRAPS.with(|count| count.set(0));
}

#[cfg(test)]
fn generated_resident_unwraps() -> u64 {
    GENERATED_RESIDENT_UNWRAPS.with(Cell::get)
}

/// A chunk coordinate used by lifecycle replay.
pub type ChunkPos = (i32, i32);

/// An absolute block coordinate used by lifecycle replay.
pub type AbsoluteCell = (i32, i32, i32);

/// Executes independent immutable admission work while preserving submission
/// order in the returned columns. Mutable lifecycle commits never use this
/// seam.
pub trait ImmutableComputeExecutor: Send + Sync {
    fn execute_shaped(
        &self,
        jobs: Vec<ChunkPos>,
        work: &(dyn Fn(ChunkPos) -> ChunkColumn + Send + Sync),
    ) -> Vec<ChunkColumn>;

    /// Executes shaped jobs that can stay in the generator's typed immutable
    /// representation. `None` keeps the ordinary source fallback for edited
    /// or persisted columns whose server carrier is already authoritative.
    fn execute_generated_shaped(
        &self,
        jobs: Vec<ChunkPos>,
        work: &(dyn Fn(ChunkPos) -> Option<lodestone_worldgen::overworld::GeneratedColumn>
            + Send
            + Sync),
    ) -> Vec<Option<lodestone_worldgen::overworld::GeneratedColumn>> {
        jobs.into_iter().map(work).collect()
    }
}

/// The native persistent dispatcher and the browser's serial fallback.
#[derive(Debug, Clone, Copy, Default)]
pub struct PersistentWorldgenExecutor;

impl ImmutableComputeExecutor for PersistentWorldgenExecutor {
    fn execute_shaped(
        &self,
        jobs: Vec<ChunkPos>,
        work: &(dyn Fn(ChunkPos) -> ChunkColumn + Send + Sync),
    ) -> Vec<ChunkColumn> {
        crate::run_worldgen_jobs(jobs, |chunk| work(chunk))
    }

    fn execute_generated_shaped(
        &self,
        jobs: Vec<ChunkPos>,
        work: &(dyn Fn(ChunkPos) -> Option<lodestone_worldgen::overworld::GeneratedColumn>
            + Send
            + Sync),
    ) -> Vec<Option<lodestone_worldgen::overworld::GeneratedColumn>> {
        crate::run_worldgen_jobs(jobs, |chunk| work(chunk))
    }
}

/// Horizontal chunk radius sampled by an initial packet's light encoder.
pub const PACKET_LIGHT_RADIUS: i32 = 1;

/// Horizontal radius used by the target-owned FEATURES lifecycle. The
/// dispatcher is the authority for this footprint; settlement and replay
/// contexts must use the same value.
pub const TARGET_FEATURE_RADIUS: i32 = lodestone_worldgen::overworld::TARGET_DECORATION_RADIUS;

// Source-ordered replay remains available for Nether/End captures. These
// constants describe that legacy capture closure, not target-owned Overworld
// settlement geometry.
const SOURCE_FEATURE_WRITE_RADIUS: i32 = 2;
const SOURCE_MUTABLE_READ_RADIUS: i32 = 4;

// The reverse frontier already accounts for the selected source's write halo;
// the remaining two chunks are the audited mutable-read contribution.
const BACKWARD_FRONTIER_RADIUS: i32 = SOURCE_MUTABLE_READ_RADIUS - SOURCE_FEATURE_WRITE_RADIUS;
const ADMITTED_DESTINATION_RADIUS: i32 = 2;

/// A completion stage captured from the external scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LifecycleCompletion {
    /// The source's feature body completed and its writes are observable.
    Features,
    /// The source reached the final status used by capture telemetry.
    Full,
}

/// Ownership boundary for a dimension's FEATURES operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleFeatureDispatch {
    /// The target owns one complete dispatcher invocation.
    TargetOwned,
    /// The caller supplies the ordered source views separately.
    SourceOrdered,
}

/// Selects the target-owned completion boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleCompletionMode {
    /// Build the complete target product and its sidecars.
    Full,
    /// Run a padding writer while retaining only sparse state and outward writes.
    SparsePadding,
}

#[cfg(test)]
thread_local! {
    static COMPLETION_MODE_COUNTS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    static REGION_FEATURE_EPOCH_COUNTS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    static REGION_FEATURE_SCALAR_FALLBACKS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn record_completion_mode(mode: LifecycleCompletionMode) {
    COMPLETION_MODE_COUNTS.with(|counts| {
        let (full, sparse) = counts.get();
        counts.set(match mode {
            LifecycleCompletionMode::Full => (full + 1, sparse),
            LifecycleCompletionMode::SparsePadding => (full, sparse + 1),
        });
    });
}

#[cfg(test)]
pub(crate) fn reset_completion_mode_counts() {
    COMPLETION_MODE_COUNTS.with(|counts| counts.set((0, 0)));
}

#[cfg(test)]
pub(crate) fn completion_mode_counts() -> (usize, usize) {
    COMPLETION_MODE_COUNTS.with(Cell::get)
}

#[cfg(test)]
fn record_region_feature_epoch(mode: LifecycleCompletionMode) {
    REGION_FEATURE_EPOCH_COUNTS.with(|counts| {
        let (full, sparse) = counts.get();
        counts.set(match mode {
            LifecycleCompletionMode::Full => (full + 1, sparse),
            LifecycleCompletionMode::SparsePadding => (full, sparse + 1),
        });
    });
}

#[cfg(test)]
pub(crate) fn reset_region_feature_epoch_counts() {
    REGION_FEATURE_EPOCH_COUNTS.with(|counts| counts.set((0, 0)));
    REGION_FEATURE_SCALAR_FALLBACKS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn region_feature_epoch_counts() -> (usize, usize) {
    REGION_FEATURE_EPOCH_COUNTS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn region_feature_scalar_fallbacks() -> usize {
    REGION_FEATURE_SCALAR_FALLBACKS.with(Cell::get)
}

#[cfg(test)]
fn record_region_feature_scalar_fallback() {
    REGION_FEATURE_SCALAR_FALLBACKS.with(|count| count.set(count.get() + 1));
}

/// Generation status tracked for one resident lifecycle column.
///
/// Client heightmaps are deliberately not part of this status: a source that
/// has entered FEATURES may initialize a still-CARVERS destination by writing
/// across the chunk boundary. The destination remains CARVERS until its own
/// completion event, while its retained maps are already maintained
/// incrementally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LifecycleResidentStage {
    /// Terrain is resident through the carving boundary.
    Carvers,
    /// The column's own FEATURES body has completed.
    Features,
    /// The column reached the final lifecycle status.
    Full,
}

/// The three client-visible heightmaps in wire registry order:
/// `WORLD_SURFACE` (1), `MOTION_BLOCKING` (4), and
/// `MOTION_BLOCKING_NO_LEAVES` (5).
pub type LifecycleClientHeightmaps = [[u16; 256]; 3];

/// An authenticated resident-state transition captured at one lifecycle
/// boundary.
///
/// `client_heightmaps` is the exact map seed to install before the source
/// body runs. It is intentionally independent from the resident block field:
/// an external chunk may instantiate a map, or replace two map cells at its
/// own FEATURES boundary, without a captured block transition in that band.
/// The materializer then maintains the installed maps through ordinary
/// incremental writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleResidentTransition {
    /// Resident column whose status/maps were observed.
    pub resident: ChunkPos,
    /// Status after this boundary has been crossed.
    pub stage: LifecycleResidentStage,
    /// Exact raw map cells at the boundary, in the order documented by
    /// [`LifecycleClientHeightmaps`]. `None` means the resident remains
    /// unprimed at this boundary.
    pub client_heightmaps: Option<LifecycleClientHeightmaps>,
}

/// A canonical cross-column write retained through lifecycle settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleSpill {
    /// The source chunk whose body produced this write.
    pub source: ChunkPos,
    /// The absolute block coordinate of the write.
    pub position: AbsoluteCell,
    /// The validated canonical block-state id after the write.
    pub state: StateId,
    /// Whether this source-crossing write is transient during a transaction.
    pub transient: bool,
}

/// One complete FEATURES result emitted by a production source dispatcher.
#[derive(Debug, Default)]
pub struct LifecycleFeatureResult {
    /// Block-state transitions, including writes into neighbouring chunks.
    pub spills: Vec<LifecycleSpill>,
    /// Generated block entities carried with the source result.
    pub block_entities: Vec<GeneratedBlockEntity>,
    /// Typed structure writes/loot emitted by the same Features stream.
    pub structure_blocks: StructureBlocks,
    /// End return-gateway sidecars emitted with the source's block writes.
    pub end_gateways: Vec<lodestone_worldgen::end::EndGateway>,
}

fn authenticated_features_identity(generated: [u8; 32], prefix: [u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"lodestone-worldgen-authenticated-features-v2");
    hasher.update(generated);
    hasher.update(prefix);
    hasher.finalize().into()
}

/// A target-owned source may return its finished target column directly when
/// its feature and post-feature stages share one dense working field.
#[derive(Debug)]
pub struct LifecycleTargetFeatureResult {
    pub column: ChunkColumn,
    pub spills: Vec<LifecycleSpill>,
    /// Target-local final FEATURES writes retained by direct target output.
    /// The materializer seeds these into later target read views without
    /// replaying them into the already-finished target column.
    pub local_features: Vec<LifecycleSpill>,
}

/// Sparse target-owned result used for settlement padding.
#[derive(Debug)]
pub struct LifecycleSparseTargetFeatureResult {
    pub spills: Vec<LifecycleSpill>,
    pub local_features: Vec<LifecycleSpill>,
    pub block_entities: Vec<GeneratedBlockEntity>,
}

/// One authenticated FEATURES event accepted by a target replay plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleReplayEvent {
    pub source: ChunkPos,
    pub stage: LifecycleCompletion,
    pub sequence: u64,
    /// Per-resident status/map observations from the authenticated capture.
    /// Empty is retained for legacy captures whose format predates map data;
    /// production adapters then provide their own exact shaped seed through
    /// [`LifecycleWorldgenSource::lifecycle_client_heightmaps`].
    pub resident_transitions: Vec<LifecycleResidentTransition>,
}

/// Static, validated dependency closure for one target packet.
#[derive(Debug, Clone)]
pub struct LifecycleReplayPlan {
    target: ChunkPos,
    admissions: Vec<ChunkPos>,
    feature_events: Vec<LifecycleReplayEvent>,
}

impl LifecycleReplayPlan {
    /// Build a target plan from an authenticated admission/event stream. The
    /// event stream may be sparse when a status task completes only the target
    /// while its admitted dependency columns remain CARVERS inputs. The
    /// reverse walk preserves event order while selecting only earlier sources
    /// whose writes can affect the packet or a selected dependency.
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
        if feature_events.is_empty() || feature_events.len() > admissions.len() {
            return Err(format!(
                "FEATURES event stream has {} events for {} admissions",
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
            if feature_events.len() == admissions.len() && admissions[rank] != event.source {
                return Err(format!("lifecycle replay event {} source {:?} disagrees with admission {:?}", event.sequence, event.source, admissions[rank]));
            }
            if !admitted.contains(&event.source) {
                return Err(format!("lifecycle replay event {} source {:?} is not admitted", event.sequence, event.source));
            }
            if !seen_sources.insert(event.source) {
                return Err(format!("lifecycle replay source {:?} appears more than once", event.source));
            }
            let mut seen_residents = BTreeSet::new();
            for transition in &event.resident_transitions {
                if !admitted.contains(&transition.resident) {
                    return Err(format!(
                        "lifecycle replay event {} carries state for unadmitted resident {:?}",
                        event.sequence, transition.resident,
                    ));
                }
                if !seen_residents.insert(transition.resident) {
                    return Err(format!(
                        "lifecycle replay event {} repeats resident {:?}",
                        event.sequence, transition.resident,
                    ));
                }
            }
        }

        let mut event_frontier = packet_domain.clone();
        let mut selected_sources = BTreeSet::new();
        for event in feature_events.iter().rev() {
            let reaches = event_frontier.iter().any(|destination| {
                (event.source.0 - destination.0).abs().max((event.source.1 - destination.1).abs())
                    <= SOURCE_FEATURE_WRITE_RADIUS
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
        let feature_events = feature_events
            .iter()
            .cloned()
            .filter(|event| selected_sources.contains(&event.source))
            .collect::<Vec<_>>();
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
    /// Immutable product shared by every source completion for one target.
    type ReplayContext: Send + Sync + 'static;

    /// Select the lifecycle operation boundary for FEATURES.
    fn feature_dispatch(&self) -> LifecycleFeatureDispatch {
        LifecycleFeatureDispatch::SourceOrdered
    }

    /// Prepare immutable source caches for the complete admitted replay. The
    /// default keeps sources with no replay-specific cache unchanged.
    fn prepare_lifecycle_replay(&mut self, _admissions: &[ChunkPos]) {}

    /// Build the request/target-owned immutable read context once before its
    /// ordered source wavefront starts.
    fn lifecycle_replay_context(&self, target: ChunkPos) -> Arc<Self::ReplayContext>;

    /// Prepare target contexts once for a request. The default preserves the
    /// scalar path; sources with overlapping windows may override it with a
    /// request-owned batch product.
    fn lifecycle_replay_contexts(
        &self,
        targets: &[ChunkPos],
    ) -> BTreeMap<ChunkPos, Arc<Self::ReplayContext>> {
        targets
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|target| (target, self.lifecycle_replay_context(target)))
            .collect()
    }

    fn lifecycle_replay_contexts_prepared(
        &self,
        _targets: &[ChunkPos],
    ) -> Option<BTreeMap<ChunkPos, Arc<Self::ReplayContext>>> {
        None
    }

    /// Start an optional request-owned mutable decoration epoch after all
    /// target contexts have been prepared. Sources without a shared region
    /// keep the scalar completion path.
    fn begin_region_feature_epoch(
        &self,
        _targets: &[ChunkPos],
        _contexts: &BTreeMap<ChunkPos, Arc<Self::ReplayContext>>,
    ) -> Option<lodestone_worldgen::overworld::RegionFeatureEpoch> {
        None
    }

    /// Optional source-specific computation count for replay controls.
    fn lifecycle_pre_decoration_computations(&self) -> Option<usize> {
        None
    }

    /// Materialize the source's shaped prefix.
    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn;

    /// Return a typed generated shaped prefix when no persisted or edited
    /// server column takes precedence. The lifecycle materializer retains this
    /// product without constructing a `ChunkColumn`; the default preserves
    /// existing source implementations.
    fn generated_shaped_column(
        &self,
        _cx: i32,
        _cz: i32,
    ) -> Option<lodestone_worldgen::overworld::GeneratedColumn> {
        None
    }

    /// Build a complete pristine shaped batch under one source-owned store
    /// lease. `None` preserves per-coordinate precedence for edited or
    /// hydrated columns and falls back to [`Self::generated_shaped_column`].
    fn generated_shaped_columns(
        &self,
        _chunks: &[ChunkPos],
    ) -> Option<Vec<lodestone_worldgen::overworld::GeneratedColumn>> {
        None
    }

    /// Build shaped products after warming the exact prefix consumed by the
    /// replay wavefront. Sources without a shared prefix retain their batch
    /// implementation.
    fn generated_shaped_columns_with_prefix(
        &self,
        chunks: &[ChunkPos],
        _prefix_targets: &[ChunkPos],
        _prefix_radius: i32,
    ) -> Option<Vec<lodestone_worldgen::overworld::GeneratedColumn>> {
        self.generated_shaped_columns(chunks)
    }

    /// Build carriers for `chunks` while keeping the source lease over the
    /// complete read context. Read-only context coordinates are warmed but
    /// never converted into packet carriers. Sources without this split
    /// retain their ordinary batch path when both coordinate lists match.
    fn generated_shaped_columns_with_context(
        &self,
        chunks: &[ChunkPos],
        lease_chunks: &[ChunkPos],
        prefix_targets: &[ChunkPos],
        prefix_radius: i32,
    ) -> Option<Vec<lodestone_worldgen::overworld::GeneratedColumn>> {
        (chunks == lease_chunks)
            .then(|| self.generated_shaped_columns_with_prefix(chunks, prefix_targets, prefix_radius))
            .flatten()
    }

    /// Return the source's exact lifecycle map seed for one resident column.
    ///
    /// This is a source boundary, not a reconstruction from the resident
    /// block field. Captures that carry an authenticated
    /// [`LifecycleResidentTransition`] take precedence; this hook keeps older
    /// captures and direct controls on the same no-rescan path.
    fn lifecycle_client_heightmaps(
        &self,
        _cx: i32,
        _cz: i32,
    ) -> Option<LifecycleClientHeightmaps> {
        None
    }

    /// Run one source's complete FEATURES body against resident overrides.
    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
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
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        self.feature_result(source, overrides, resident)
    }

    /// Run one complete target-owned FEATURES dispatcher.
    fn target_feature_result(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        self.feature_result_for_target(target, target, overrides, resident)
    }

    /// Optional direct target output for dimensions whose target-owned
    /// dispatcher can retain its final center column without a spill replay.
    fn target_feature_result_direct(
        &self,
        _target: ChunkPos,
        _overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> Option<LifecycleTargetFeatureResult> {
        None
    }

    /// Direct target output using a context prepared for the target. The
    /// default keeps dimensions without context-aware direct output unchanged.
    fn target_feature_result_direct_with_replay_context(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        _context: &Self::ReplayContext,
    ) -> Option<LifecycleTargetFeatureResult> {
        self.target_feature_result_direct(target, overrides, resident)
    }

    /// Run direct target output through the request-owned Overworld epoch when
    /// one is installed. The default preserves the existing source boundary.
    fn target_feature_result_direct_with_replay_context_and_epoch(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        _epoch: &mut lodestone_worldgen::overworld::RegionFeatureEpoch,
    ) -> Option<LifecycleTargetFeatureResult> {
        self.target_feature_result_direct_with_replay_context(
            target, overrides, resident, context,
        )
    }

    /// Direct target output using the materializer's append-only override
    /// revisions. The default preserves non-Overworld source adapters.
    fn target_feature_result_direct_with_replay_context_and_epoch_revisions(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        epoch: &mut lodestone_worldgen::overworld::RegionFeatureEpoch,
        _revisions: &[((i32, i32, i32), StateId)],
    ) -> Option<LifecycleTargetFeatureResult> {
        self.target_feature_result_direct_with_replay_context_and_epoch(
            target, overrides, resident, context, epoch,
        )
    }

    /// Run a target-owned padding writer without constructing a target product.
    fn target_feature_result_sparse_with_replay_context(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        _context: &Self::ReplayContext,
    ) -> Option<LifecycleSparseTargetFeatureResult> {
        let mut result = self.target_feature_result(target, overrides, resident);
        result.spills.extend(self.post_features_spills(target, overrides));
        let mut spills = Vec::new();
        let mut local_features = Vec::new();
        for spill in result.spills {
            if (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            ) == target {
                local_features.push(spill);
            } else {
                spills.push(spill);
            }
        }
        Some(LifecycleSparseTargetFeatureResult {
            spills,
            local_features,
            block_entities: result.block_entities,
        })
    }

    /// Run sparse target output through the request-owned Overworld epoch when
    /// one is installed. The default preserves the existing source boundary.
    fn target_feature_result_sparse_with_replay_context_and_epoch(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        _epoch: &mut lodestone_worldgen::overworld::RegionFeatureEpoch,
    ) -> Option<LifecycleSparseTargetFeatureResult> {
        self.target_feature_result_sparse_with_replay_context(
            target, overrides, resident, context,
        )
    }

    /// Sparse target output using append-only override revisions. The default
    /// keeps dimensions without a region epoch on their existing boundary.
    fn target_feature_result_sparse_with_replay_context_and_epoch_revisions(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        epoch: &mut lodestone_worldgen::overworld::RegionFeatureEpoch,
        _revisions: &[((i32, i32, i32), StateId)],
    ) -> Option<LifecycleSparseTargetFeatureResult> {
        self.target_feature_result_sparse_with_replay_context_and_epoch(
            target, overrides, resident, context, epoch,
        )
    }

    /// Whether direct target output is safe only for a marked authenticated
    /// generated prefix. Production Overworld sources enable this because an
    /// edited or hydrated `ChunkColumn` must outrank deterministic output;
    /// small lifecycle fixtures retain their historical direct hook by
    /// leaving the default disabled.
    fn direct_target_output_requires_authentication(&self) -> bool {
        false
    }

    /// Whether direct target output can run while its generated shaped prefix
    /// remains compact and absent from the mutable resident map.
    fn direct_target_output_from_generated_prefix(&self) -> bool {
        false
    }

    /// Run one source body using the immutable product shared by this target's
    /// source wavefront. The default keeps dimensions without a mixed context
    /// on their existing source boundary.
    fn feature_result_for_target_with_replay_context(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        _context: &Self::ReplayContext,
    ) -> LifecycleFeatureResult {
        self.feature_result_for_target(target, source, overrides, resident)
    }

    fn feature_result_for_target_with_replay_context_and_completed(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        _completed_sources: &BTreeSet<ChunkPos>,
    ) -> (LifecycleFeatureResult, Vec<ChunkPos>) {
        (
            self.feature_result_for_target_with_replay_context(
                target, source, overrides, resident, context,
            ),
            Vec::new(),
        )
    }

    /// Run one target FEATURES body while preserving any source identities
    /// discovered by that body. Most dimensions have one source identity and
    /// use the ordinary target result; source-ordered adapters may report
    /// additional identities for target-local deduplication.
    fn feature_result_for_target_with_completed(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        _completed_sources: &BTreeSet<ChunkPos>,
    ) -> (LifecycleFeatureResult, Vec<ChunkPos>) {
        (self.feature_result_for_target(target, source, overrides, resident), Vec::new())
    }

    /// Whether target-scoped FEATURES bodies read the resident CARVERS view
    /// rather than the packet's fully decorated state. The Overworld region
    /// exposes this lower view to a body that reads an already-completed
    /// neighbour; other dimensions use their target-specific adapter view.
    fn target_feature_reads_carvers(&self) -> bool {
        false
    }

    /// Whether writes emitted while replaying a target are part of the
    /// resident world's durable source state even when their destination has
    /// not reached FEATURES yet. End decoration is source-owned and its
    /// cross-chunk plant writes must remain visible to the later destination
    /// packet; dimensions with target-local speculative writes retain the
    /// default rollback behavior.
    fn target_spills_persist(&self) -> bool {
        false
    }

    /// Run any source-local stage after FEATURES. The Nether has no such
    /// stage, so its implementation keeps the default empty result.
    fn post_features_spills(
        &self,
        _source: ChunkPos,
        _overrides: &BTreeMap<AbsoluteCell, StateId>,
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

/// Borrowed view of a lifecycle source.
///
/// A materializer may borrow a source when the source is already prepared by
/// its owner. Preparation is intentionally a mutable-only operation, so the
/// borrowed implementation leaves that hook unchanged and delegates every
/// read/materialization boundary to the underlying source.
impl<T: LifecycleWorldgenSource + ?Sized> LifecycleWorldgenSource for &T {
    type ReplayContext = T::ReplayContext;

    fn feature_dispatch(&self) -> LifecycleFeatureDispatch {
        LifecycleWorldgenSource::feature_dispatch(*self)
    }

    fn lifecycle_pre_decoration_computations(&self) -> Option<usize> {
        LifecycleWorldgenSource::lifecycle_pre_decoration_computations(*self)
    }

    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
        LifecycleWorldgenSource::shaped_column(*self, cx, cz)
    }

    fn generated_shaped_column(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<lodestone_worldgen::overworld::GeneratedColumn> {
        LifecycleWorldgenSource::generated_shaped_column(*self, cx, cz)
    }

    fn generated_shaped_columns(
        &self,
        chunks: &[ChunkPos],
    ) -> Option<Vec<lodestone_worldgen::overworld::GeneratedColumn>> {
        LifecycleWorldgenSource::generated_shaped_columns(*self, chunks)
    }

    fn generated_shaped_columns_with_prefix(
        &self,
        chunks: &[ChunkPos],
        prefix_targets: &[ChunkPos],
        prefix_radius: i32,
    ) -> Option<Vec<lodestone_worldgen::overworld::GeneratedColumn>> {
        LifecycleWorldgenSource::generated_shaped_columns_with_prefix(
            *self,
            chunks,
            prefix_targets,
            prefix_radius,
        )
    }

    fn generated_shaped_columns_with_context(
        &self,
        chunks: &[ChunkPos],
        lease_chunks: &[ChunkPos],
        prefix_targets: &[ChunkPos],
        prefix_radius: i32,
    ) -> Option<Vec<lodestone_worldgen::overworld::GeneratedColumn>> {
        LifecycleWorldgenSource::generated_shaped_columns_with_context(
            *self,
            chunks,
            lease_chunks,
            prefix_targets,
            prefix_radius,
        )
    }

    fn lifecycle_replay_context(&self, target: ChunkPos) -> Arc<Self::ReplayContext> {
        LifecycleWorldgenSource::lifecycle_replay_context(*self, target)
    }

    fn lifecycle_replay_contexts(
        &self,
        targets: &[ChunkPos],
    ) -> BTreeMap<ChunkPos, Arc<Self::ReplayContext>> {
        LifecycleWorldgenSource::lifecycle_replay_contexts(*self, targets)
    }

    fn lifecycle_replay_contexts_prepared(
        &self,
        targets: &[ChunkPos],
    ) -> Option<BTreeMap<ChunkPos, Arc<Self::ReplayContext>>> {
        LifecycleWorldgenSource::lifecycle_replay_contexts_prepared(*self, targets)
    }

    fn begin_region_feature_epoch(
        &self,
        targets: &[ChunkPos],
        contexts: &BTreeMap<ChunkPos, Arc<Self::ReplayContext>>,
    ) -> Option<lodestone_worldgen::overworld::RegionFeatureEpoch> {
        LifecycleWorldgenSource::begin_region_feature_epoch(*self, targets, contexts)
    }

    fn lifecycle_client_heightmaps(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<LifecycleClientHeightmaps> {
        LifecycleWorldgenSource::lifecycle_client_heightmaps(*self, cx, cz)
    }

    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        LifecycleWorldgenSource::feature_result(*self, source, overrides, resident)
    }

    fn feature_result_for_target(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        LifecycleWorldgenSource::feature_result_for_target(
            *self,
            target,
            source,
            overrides,
            resident,
        )
    }

    fn target_feature_result(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        LifecycleWorldgenSource::target_feature_result(*self, target, overrides, resident)
    }

    fn target_feature_result_direct(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> Option<LifecycleTargetFeatureResult> {
        LifecycleWorldgenSource::target_feature_result_direct(
            *self,
            target,
            overrides,
            resident,
        )
    }

    fn target_feature_result_direct_with_replay_context(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
    ) -> Option<LifecycleTargetFeatureResult> {
        LifecycleWorldgenSource::target_feature_result_direct_with_replay_context(
            *self,
            target,
            overrides,
            resident,
            context,
        )
    }

    fn target_feature_result_direct_with_replay_context_and_epoch(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        epoch: &mut lodestone_worldgen::overworld::RegionFeatureEpoch,
    ) -> Option<LifecycleTargetFeatureResult> {
        LifecycleWorldgenSource::target_feature_result_direct_with_replay_context_and_epoch(
            *self,
            target,
            overrides,
            resident,
            context,
            epoch,
        )
    }

    fn target_feature_result_sparse_with_replay_context(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
    ) -> Option<LifecycleSparseTargetFeatureResult> {
        LifecycleWorldgenSource::target_feature_result_sparse_with_replay_context(
            *self,
            target,
            overrides,
            resident,
            context,
        )
    }

    fn target_feature_result_sparse_with_replay_context_and_epoch(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        epoch: &mut lodestone_worldgen::overworld::RegionFeatureEpoch,
    ) -> Option<LifecycleSparseTargetFeatureResult> {
        LifecycleWorldgenSource::target_feature_result_sparse_with_replay_context_and_epoch(
            *self,
            target,
            overrides,
            resident,
            context,
            epoch,
        )
    }

    fn direct_target_output_requires_authentication(&self) -> bool {
        LifecycleWorldgenSource::direct_target_output_requires_authentication(*self)
    }

    fn direct_target_output_from_generated_prefix(&self) -> bool {
        LifecycleWorldgenSource::direct_target_output_from_generated_prefix(*self)
    }

    fn feature_result_for_target_with_replay_context(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
    ) -> LifecycleFeatureResult {
        LifecycleWorldgenSource::feature_result_for_target_with_replay_context(
            *self,
            target,
            source,
            overrides,
            resident,
            context,
        )
    }

    fn feature_result_for_target_with_replay_context_and_completed(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        completed_sources: &BTreeSet<ChunkPos>,
    ) -> (LifecycleFeatureResult, Vec<ChunkPos>) {
        LifecycleWorldgenSource::feature_result_for_target_with_replay_context_and_completed(
            *self,
            target,
            source,
            overrides,
            resident,
            context,
            completed_sources,
        )
    }

    fn feature_result_for_target_with_completed(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        completed_sources: &BTreeSet<ChunkPos>,
    ) -> (LifecycleFeatureResult, Vec<ChunkPos>) {
        LifecycleWorldgenSource::feature_result_for_target_with_completed(
            *self,
            target,
            source,
            overrides,
            resident,
            completed_sources,
        )
    }

    fn target_feature_reads_carvers(&self) -> bool {
        LifecycleWorldgenSource::target_feature_reads_carvers(*self)
    }

    fn target_spills_persist(&self) -> bool {
        LifecycleWorldgenSource::target_spills_persist(*self)
    }

    fn post_features_spills(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
    ) -> Vec<LifecycleSpill> {
        LifecycleWorldgenSource::post_features_spills(*self, source, overrides)
    }

    fn attach_end_gateways(
        &self,
        column: &mut ChunkColumn,
        gateways: &[lodestone_worldgen::end::EndGateway],
    ) {
        LifecycleWorldgenSource::attach_end_gateways(*self, column, gateways);
    }

    fn attach_source_sidecars(&self, source: ChunkPos, column: &mut ChunkColumn) {
        LifecycleWorldgenSource::attach_source_sidecars(*self, source, column);
    }

    fn finalize_packet_snapshot(&self, target: ChunkPos, column: &mut ChunkColumn) {
        LifecycleWorldgenSource::finalize_packet_snapshot(*self, target, column);
    }
}

fn override_vec(overrides: &BTreeMap<AbsoluteCell, StateId>) -> Vec<(i32, i32, i32, StateId)> {
    overrides
        .iter()
        .map(|(&(x, y, z), state)| (x, y, z, state.clone()))
        .collect()
}

fn overworld_override_vec(
    overrides: &BTreeMap<AbsoluteCell, StateId>,
    target: ChunkPos,
    generator: &lodestone_worldgen::overworld::OverworldGenerator,
) -> Vec<(i32, i32, i32, StateId)> {
    // Keep the server-side overlay window derived from the same radius as
    // target settlement and the worldgen dispatcher: overrides are live for
    // every cell in C, including writes made by an earlier neighbouring W.
    overworld_override_vec_with_radius(overrides, target, generator, TARGET_FEATURE_RADIUS)
}

fn overworld_override_vec_with_radius(
    overrides: &BTreeMap<AbsoluteCell, StateId>,
    target: ChunkPos,
    generator: &lodestone_worldgen::overworld::OverworldGenerator,
    radius: i32,
) -> Vec<(i32, i32, i32, StateId)> {
    let min_x = (target.0 - radius) * 16
        - lodestone_worldgen::feature::vegetation::GEODE_PADDING;
    let max_x = (target.0 + radius + 1) * 16
        + lodestone_worldgen::feature::vegetation::GEODE_PADDING;
    let min_z = (target.1 - radius) * 16
        - lodestone_worldgen::feature::vegetation::GEODE_PADDING;
    let max_z = (target.1 + radius + 1) * 16
        + lodestone_worldgen::feature::vegetation::GEODE_PADDING;
    let min_y = generator.min_y();
    let max_y = min_y + generator.height();
    overrides
        .range((min_x, i32::MIN, i32::MIN)..(max_x, i32::MIN, i32::MIN))
        .filter(|((_, y, z), _)| (min_y..max_y).contains(y) && (min_z..max_z).contains(z))
        .map(|(&(x, y, z), state)| (x, y, z, state.clone()))
        .collect()
}

impl LifecycleWorldgenSource for OverworldChunkSource {
    type ReplayContext = lodestone_worldgen::overworld::MixedReplayContext;

    fn feature_dispatch(&self) -> LifecycleFeatureDispatch {
        LifecycleFeatureDispatch::TargetOwned
    }

    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.column_at(cx, cz, ChunkGenerationStage::Shaped)
    }

    fn generated_shaped_column(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<lodestone_worldgen::overworld::GeneratedColumn> {
        crate::chunk::OverworldChunkSource::generated_shaped_column(self, cx, cz)
    }

    fn generated_shaped_columns(
        &self,
        chunks: &[ChunkPos],
    ) -> Option<Vec<lodestone_worldgen::overworld::GeneratedColumn>> {
        crate::chunk::OverworldChunkSource::generated_shaped_columns(self, chunks)
    }

    fn generated_shaped_columns_with_prefix(
        &self,
        chunks: &[ChunkPos],
        prefix_targets: &[ChunkPos],
        prefix_radius: i32,
    ) -> Option<Vec<lodestone_worldgen::overworld::GeneratedColumn>> {
        crate::chunk::OverworldChunkSource::generated_shaped_columns_with_prefix(
            self,
            chunks,
            prefix_targets,
            prefix_radius,
        )
    }

    fn generated_shaped_columns_with_context(
        &self,
        chunks: &[ChunkPos],
        lease_chunks: &[ChunkPos],
        prefix_targets: &[ChunkPos],
        prefix_radius: i32,
    ) -> Option<Vec<lodestone_worldgen::overworld::GeneratedColumn>> {
        crate::chunk::OverworldChunkSource::generated_shaped_columns_with_context(
            self,
            chunks,
            lease_chunks,
            prefix_targets,
            prefix_radius,
        )
    }

    fn lifecycle_replay_context(&self, target: ChunkPos) -> Arc<Self::ReplayContext> {
        self.generator().lifecycle_replay_context(target.0, target.1)
    }

    fn lifecycle_replay_contexts(
        &self,
        targets: &[ChunkPos],
    ) -> BTreeMap<ChunkPos, Arc<Self::ReplayContext>> {
        let batch = self
            .generator()
            .mixed_replay_batch_with_radius(
                targets,
                TARGET_FEATURE_RADIUS,
            );
        targets
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|target| batch.context_arc(target).map(|context| (target, context)))
            .collect()
    }

    fn lifecycle_replay_contexts_prepared(
        &self,
        targets: &[ChunkPos],
    ) -> Option<BTreeMap<ChunkPos, Arc<Self::ReplayContext>>> {
        let batch = self
            .generator()
            .mixed_replay_batch_with_radius_prepared(targets, TARGET_FEATURE_RADIUS);
        Some(
            targets
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter_map(|target| batch.context_arc(target).map(|context| (target, context)))
                .collect(),
        )
    }

    fn begin_region_feature_epoch(
        &self,
        targets: &[ChunkPos],
        contexts: &BTreeMap<ChunkPos, Arc<Self::ReplayContext>>,
    ) -> Option<lodestone_worldgen::overworld::RegionFeatureEpoch> {
        let context = contexts.values().next()?;
        Some(
            self.generator()
                .begin_region_feature_epoch_from_context(context, targets),
        )
    }

    fn lifecycle_client_heightmaps(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<LifecycleClientHeightmaps> {
        self.column_at(cx, cz, ChunkGenerationStage::Shaped)
            .client_heightmaps_raw()
    }

    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        let overrides = overworld_override_vec(overrides, source, self.generator());
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
            structure_blocks: StructureBlocks::default(),
            end_gateways: Vec::new(),
        }
    }

    fn target_feature_result(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        let overrides = overworld_override_vec(overrides, target, self.generator());
        let result = self
            .generator()
            .parity_features_with_overrides(target.0, target.1, &overrides);
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
            structure_blocks: StructureBlocks::default(),
            end_gateways: Vec::new(),
        }
    }

    fn target_feature_result_direct(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> Option<LifecycleTargetFeatureResult> {
        let overrides = overworld_override_vec(overrides, target, self.generator());
        let result = self
            .generator()
            .direct_decoration_with_overrides(target.0, target.1, &overrides);
        let mut column = ChunkColumn::from_generated(result.column);
        self.attach_structures(&mut column, target.0, target.1);
        column.populate_missing_block_entity_states(target.0, target.1);
        Some(LifecycleTargetFeatureResult {
            column,
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
            local_features: result
                .local_features
                .into_iter()
                .map(|spill| LifecycleSpill {
                    source: spill.source,
                    position: spill.position,
                    state: spill.state,
                    transient: false,
                })
                .collect(),
        })
    }

    fn target_feature_result_direct_with_replay_context_and_epoch_revisions(
        &self,
        target: ChunkPos,
        _overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        epoch: &mut lodestone_worldgen::overworld::RegionFeatureEpoch,
        revisions: &[((i32, i32, i32), StateId)],
    ) -> Option<LifecycleTargetFeatureResult> {
        let result = self
            .generator()
            .complete_region_feature_epoch_target_from_context_with_override_events(
                epoch,
                target,
                context,
                revisions,
            );
        let mut column = ChunkColumn::from_generated(result.column);
        self.attach_structures(&mut column, target.0, target.1);
        column.populate_missing_block_entity_states(target.0, target.1);
        Some(LifecycleTargetFeatureResult {
            column,
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
            local_features: result
                .local_features
                .into_iter()
                .map(|spill| LifecycleSpill {
                    source: spill.source,
                    position: spill.position,
                    state: spill.state,
                    transient: false,
                })
                .collect(),
        })
    }

    fn target_feature_result_direct_with_replay_context_and_epoch(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        epoch: &mut lodestone_worldgen::overworld::RegionFeatureEpoch,
    ) -> Option<LifecycleTargetFeatureResult> {
        let result = self
            .generator()
            .complete_region_feature_epoch_target_from_context_with_overrides(
                epoch,
                target,
                context,
                overrides,
            );
        let mut column = ChunkColumn::from_generated(result.column);
        self.attach_structures(&mut column, target.0, target.1);
        column.populate_missing_block_entity_states(target.0, target.1);
        Some(LifecycleTargetFeatureResult {
            column,
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
            local_features: result
                .local_features
                .into_iter()
                .map(|spill| LifecycleSpill {
                    source: spill.source,
                    position: spill.position,
                    state: spill.state,
                    transient: false,
                })
                .collect(),
        })
    }

    fn target_feature_result_direct_with_replay_context(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
    ) -> Option<LifecycleTargetFeatureResult> {
        let overrides = overworld_override_vec(overrides, target, self.generator());
        let result = self.generator().direct_source_decoration_with_context(
            target.0,
            target.1,
            &overrides,
            context,
        );
        let mut column = ChunkColumn::from_generated(result.column);
        self.attach_structures(&mut column, target.0, target.1);
        column.populate_missing_block_entity_states(target.0, target.1);
        Some(LifecycleTargetFeatureResult {
            column,
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
            local_features: result
                .local_features
                .into_iter()
                .map(|spill| LifecycleSpill {
                    source: spill.source,
                    position: spill.position,
                    state: spill.state,
                    transient: false,
                })
                .collect(),
        })
    }

    fn target_feature_result_sparse_with_replay_context(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
    ) -> Option<LifecycleSparseTargetFeatureResult> {
        let overrides = overworld_override_vec(overrides, target, self.generator());
        let result = self.generator().sparse_source_decoration_with_context(
            target.0,
            target.1,
            &overrides,
            context,
        );
        Some(LifecycleSparseTargetFeatureResult {
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
            local_features: result
                .local_features
                .into_iter()
                .map(|spill| LifecycleSpill {
                    source: spill.source,
                    position: spill.position,
                    state: spill.state,
                    transient: false,
                })
                .collect(),
            block_entities: result.block_entities,
        })
    }

    fn target_feature_result_sparse_with_replay_context_and_epoch(
        &self,
        target: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        epoch: &mut lodestone_worldgen::overworld::RegionFeatureEpoch,
    ) -> Option<LifecycleSparseTargetFeatureResult> {
        let result = self
            .generator()
            .complete_region_feature_epoch_target_sparse_from_context_with_overrides(
                epoch,
                target,
                context,
                overrides,
            );
        Some(LifecycleSparseTargetFeatureResult {
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
            local_features: result
                .local_features
                .into_iter()
                .map(|spill| LifecycleSpill {
                    source: spill.source,
                    position: spill.position,
                    state: spill.state,
                    transient: false,
                })
                .collect(),
            block_entities: result.block_entities,
        })
    }

    fn target_feature_result_sparse_with_replay_context_and_epoch_revisions(
        &self,
        target: ChunkPos,
        _overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
        epoch: &mut lodestone_worldgen::overworld::RegionFeatureEpoch,
        revisions: &[((i32, i32, i32), StateId)],
    ) -> Option<LifecycleSparseTargetFeatureResult> {
        let result = self
            .generator()
            .complete_region_feature_epoch_target_sparse_from_context_with_override_events(
                epoch,
                target,
                context,
                revisions,
            );
        Some(LifecycleSparseTargetFeatureResult {
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
            local_features: result
                .local_features
                .into_iter()
                .map(|spill| LifecycleSpill {
                    source: spill.source,
                    position: spill.position,
                    state: spill.state,
                    transient: false,
                })
                .collect(),
            block_entities: result.block_entities,
        })
    }

    fn direct_target_output_requires_authentication(&self) -> bool {
        true
    }

    fn direct_target_output_from_generated_prefix(&self) -> bool {
        true
    }

    fn feature_result_for_target_with_replay_context(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        context: &Self::ReplayContext,
    ) -> LifecycleFeatureResult {
        let overrides = overworld_override_vec_with_radius(
            overrides,
            target,
            self.generator(),
            lodestone_worldgen::feature::region_view::WIDE_RADIUS,
        );
        let result = self
            .generator()
            .parity_source_decoration_with_context(
                target.0,
                target.1,
                source.0,
                source.1,
                &overrides,
                context,
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
            structure_blocks: StructureBlocks::default(),
            end_gateways: Vec::new(),
        }
    }

    fn feature_result_for_target(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        let overrides = overworld_override_vec_with_radius(
            overrides,
            target,
            self.generator(),
            lodestone_worldgen::feature::region_view::WIDE_RADIUS,
        );
        let result = self
            .generator()
            .parity_source_decoration_with_overrides(
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
            structure_blocks: StructureBlocks::default(),
            end_gateways: Vec::new(),
        }
    }

    fn target_spills_persist(&self) -> bool {
        false
    }

    fn target_feature_reads_carvers(&self) -> bool {
        true
    }

    fn post_features_spills(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
    ) -> Vec<LifecycleSpill> {
        top_layer_spills(self.generator(), source, overrides)
    }
}

impl LifecycleWorldgenSource for NetherChunkSource {
    type ReplayContext = ();

    fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
        Arc::new(())
    }

    fn prepare_lifecycle_replay(&mut self, admissions: &[ChunkPos]) {
        self.generator().prepare_lifecycle_replay(admissions);
    }

    fn lifecycle_pre_decoration_computations(&self) -> Option<usize> {
        Some(self.generator().pre_decoration_computations())
    }

    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.column_at(cx, cz, ChunkGenerationStage::Shaped)
    }

    fn lifecycle_client_heightmaps(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<LifecycleClientHeightmaps> {
        self.column_at(cx, cz, ChunkGenerationStage::Shaped)
        .client_heightmaps_raw()
    }

    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        self.feature_result_for_target(source, source, overrides, resident)
    }

    fn feature_result_for_target(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        let overrides = override_vec(overrides);
        let result = if target == source {
            self.generator().parity_target_pass_with_resident(
                target.0,
                target.1,
                &overrides,
                &BTreeSet::new(),
                |cx, cz| {
                    let column = resident.get(&(cx, cz))?;
                    Some(lodestone_worldgen::dense_grid::DenseBlockGrid::from_canonical_states(
                        cx * 16,
                        0,
                        cz * 16,
                        16,
                        NetherChunkSource::WINDOW_HEIGHT,
                        16,
                        |x, y, z| {
                            column.block_state_id(
                                x.rem_euclid(16),
                                y,
                                z.rem_euclid(16),
                            )
                        },
                    ))
                },
            )
        } else {
            self.generator().parity_source_pass_with_resident(
                target.0,
                target.1,
                source.0,
                source.1,
                &overrides,
                |cx, cz| {
                    let column = resident.get(&(cx, cz))?;
                    Some(lodestone_worldgen::dense_grid::DenseBlockGrid::from_canonical_states(
                        cx * 16,
                        0,
                        cz * 16,
                        16,
                        NetherChunkSource::WINDOW_HEIGHT,
                        16,
                        |x, y, z| {
                            column.block_state_id(
                                x.rem_euclid(16),
                                y,
                                z.rem_euclid(16),
                            )
                        },
                    ))
                },
            )
        };
        let spills = result
            .spills
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
            structure_blocks: result.structure_blocks,
            end_gateways: Vec::new(),
        }
    }

    fn feature_result_for_target_with_completed(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        completed_sources: &BTreeSet<ChunkPos>,
    ) -> (LifecycleFeatureResult, Vec<ChunkPos>) {
        if completed_sources.contains(&source) {
            return (LifecycleFeatureResult::default(), Vec::new());
        }
        (
            self.feature_result_for_target(target, source, overrides, resident),
            vec![source],
        )
    }

    fn feature_result_for_target_with_replay_context_and_completed(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        resident: &BTreeMap<ChunkPos, ChunkColumn>,
        _context: &Self::ReplayContext,
        completed_sources: &BTreeSet<ChunkPos>,
    ) -> (LifecycleFeatureResult, Vec<ChunkPos>) {
        self.feature_result_for_target_with_completed(
            target,
            source,
            overrides,
            resident,
            completed_sources,
        )
    }

    fn target_spills_persist(&self) -> bool {
        true
    }
}

impl LifecycleWorldgenSource for EndChunkSource {
    type ReplayContext = ();

    fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
        Arc::new(())
    }

    fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.column_at(cx, cz, ChunkGenerationStage::Shaped)
    }

    fn lifecycle_client_heightmaps(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<LifecycleClientHeightmaps> {
        self.column_at(cx, cz, ChunkGenerationStage::Shaped)
            .client_heightmaps_raw()
            .or_else(|| Some(*self.generator().column_shaped(cx, cz).client_heightmaps()))
    }

    fn feature_result(
        &self,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        let overrides = override_vec(overrides);
        let result = self.generator().parity_source_decoration_for_target_with_overrides(
            source.0,
            source.1,
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
            structure_blocks: result.structure_blocks,
            end_gateways: result.gateways,
        }
    }

    fn feature_result_for_target(
        &self,
        target: ChunkPos,
        source: ChunkPos,
        overrides: &BTreeMap<AbsoluteCell, StateId>,
        _resident: &BTreeMap<ChunkPos, ChunkColumn>,
    ) -> LifecycleFeatureResult {
        let overrides = override_vec(overrides);
        let result = self.generator().parity_source_decoration_for_target_with_overrides(
            target.0,
            target.1,
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
            structure_blocks: result.structure_blocks,
            end_gateways: result.gateways,
        }
    }

    fn target_spills_persist(&self) -> bool {
        true
    }

    fn attach_end_gateways(
        &self,
        column: &mut ChunkColumn,
        gateways: &[lodestone_worldgen::end::EndGateway],
    ) {
        self.attach_generated_gateways(column, gateways);
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
    overrides: &BTreeMap<AbsoluteCell, StateId>,
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
/// `complete` runs one FEATURES event. Targeted replay records the requested
/// target separately from the emitted source coordinate: target-owned replay
/// invokes one target body, while source-ordered replay keeps each source's
/// decoration seed. Each emitted transition is applied to its resident
/// destination column. The caller admits the complete halo before replay so a
/// body may write to a neighbour whose explicit ticket appears later in the
/// capture.
pub struct LifecycleMaterializer<S: LifecycleWorldgenSource> {
    source: S,
    replay_context: Option<Arc<S::ReplayContext>>,
    replay_contexts: BTreeMap<ChunkPos, Arc<S::ReplayContext>>,
    region_feature_epoch: Option<lodestone_worldgen::overworld::RegionFeatureEpoch>,
    resident: BTreeMap<ChunkPos, ChunkColumn>,
    /// Typed shaped products remain here until a lifecycle operation needs the
    /// mutable server carrier. The map is request/region scoped, so it cannot
    /// become a completed-column cache.
    generated_resident: BTreeMap<ChunkPos, Arc<lodestone_worldgen::overworld::GeneratedColumn>>,
    /// Provenance digests for pristine generated prefixes. The marker is
    /// separate from `generated_resident`: a typed product can still come
    /// from a dynamic resolver, for which only the exact content fallback is
    /// safe.
    authenticated_prefixes: BTreeMap<ChunkPos, [u8; 32]>,
    /// Authenticated identities after a generated target has completed its
    /// target-owned FEATURES body. This is kept separate from the shaped
    /// prefix identity so a later stage can chain sparse cross-target writes
    /// without pretending the shaped field was rescanned.
    authenticated_stages: BTreeMap<(ChunkPos, LifecycleCompletion), [u8; 32]>,
    shared_prefixes: BTreeMap<(ChunkPos, ColumnStage), SharedPrefix>,
    /// Per-column lifecycle status. This is separate from the retained client
    /// maps because a cross-chunk FEATURES write can materialize maps before
    /// the destination's own status transition.
    resident_stages: BTreeMap<ChunkPos, LifecycleResidentStage>,
    /// Completion identity for source-centred replays. The captured sequence
    /// is telemetry and must not allow the same stage to run twice.
    completions: BTreeSet<(ChunkPos, LifecycleCompletion)>,
    /// Target-scoped completion identity. The target key records the admission
    /// event, while the source set below deduplicates nested work for that
    /// target only.
    target_completions: BTreeSet<(ChunkPos, ChunkPos, LifecycleCompletion)>,
    /// Internal source bodies already executed by the active source-ordered
    /// completion. This is scoped to one target: a source can be replayed for
    /// another target because its resident read view and write ownership may
    /// differ.
    completed_feature_sources: BTreeSet<ChunkPos>,
    /// The packet target whose ordered source wavefront is currently being
    /// replayed. Writes into a not-yet-mutable resident destination are
    /// visible to later source bodies in this wavefront. Once a source body has
    /// an authenticated completion owner, its cross-target writes remain
    /// resident for the later target rather than being replayed speculatively.
    active_target: Option<ChunkPos>,
    /// Chunks whose status is currently mutable for the packet lifecycle.
    /// Shaped columns may be resident before their target ticket reaches this
    /// boundary, but feature writes must not mutate them early.
    mutable_targets: BTreeSet<ChunkPos>,
    /// Padding targets whose local state remains sparse until a packet needs it.
    sparse_padding_targets: BTreeSet<ChunkPos>,
    /// Temporary writes to resident dependencies during the active target
    /// transaction. They are visible to later source bodies in that
    /// transaction and rolled back before the next target is finalized.
    temporary_spills: BTreeMap<AbsoluteCell, (ChunkPos, Option<StateId>, Option<StateId>)>,
    /// Temporary CARVERS-view entries for the same transaction. Sparse local
    /// padding writes are not entered here and therefore remain visible.
    temporary_carvers_overrides: BTreeMap<AbsoluteCell, Option<StateId>>,
    /// Writes retained for sparse padding destinations, grouped by destination.
    sparse_padding_overrides: BTreeMap<ChunkPos, BTreeMap<AbsoluteCell, StateId>>,
    sparse_completed_targets: BTreeSet<ChunkPos>,
    /// Canonical FEATURES writes by destination column, then absolute cell.
    /// Region traversal order can differ from the ledger's provenance order,
    /// so outputs apply each target's winners only after every requested and
    /// sparse writer has completed.
    target_feature_winners: BTreeMap<ChunkPos, BTreeMap<AbsoluteCell, TargetFeatureWinner>>,
    /// Writes visible through the CARVERS read view. A target-scoped source
    /// body sees every preceding authenticated FEATURES write, including
    /// writes into a source's own column; source-local top-layer writes are
    /// kept out because they occur after the FEATURES wavefront.
    carvers_overrides: BTreeMap<AbsoluteCell, StateId>,
    overrides: BTreeMap<AbsoluteCell, StateId>,
    /// Append-only revision streams consumed by the production region epoch.
    /// The maps above remain the source boundary for adapters and controls;
    /// these vectors prevent each target from rescanning those maps.
    override_revisions: Vec<(AbsoluteCell, StateId)>,
    carvers_override_revisions: Vec<(AbsoluteCell, StateId)>,
    direct_target_output: bool,
    /// Structure placement output accumulated from each source body without
    /// replaying the mixed stream.
    feature_structure_blocks: StructureBlocks,
}

#[derive(Clone, Copy)]
struct TargetFeatureWinner {
    target: ChunkPos,
    source: ChunkPos,
    ordinal: u32,
    state: StateId,
}

fn target_feature_winner_precedes(candidate: TargetFeatureWinner, current: TargetFeatureWinner) -> bool {
    // Target-owned FEATURES use the same minimum ordering as
    // MutationProvenance: target, source, stage, then per-source ordinal. The
    // stage and destination are identical for candidates stored at one cell.
    (candidate.target, candidate.source, candidate.ordinal)
        < (current.target, current.source, current.ordinal)
}

struct SharedPrefix {
    column: Arc<ChunkColumn>,
    fingerprint: [u8; 32],
    retained_bytes: usize,
}

impl<S: LifecycleWorldgenSource> std::fmt::Debug for LifecycleMaterializer<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LifecycleMaterializer")
            .field("resident_columns", &self.resident_count())
            .field(
                "resident_stages",
                &self.resident_stages,
            )
            .field(
                "completions",
                &(self.completions.len() + self.target_completions.len()),
            )
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
            replay_context: None,
            replay_contexts: BTreeMap::new(),
            region_feature_epoch: None,
            resident: BTreeMap::new(),
            generated_resident: BTreeMap::new(),
            authenticated_prefixes: BTreeMap::new(),
            authenticated_stages: BTreeMap::new(),
            shared_prefixes: BTreeMap::new(),
            resident_stages: BTreeMap::new(),
            completions: BTreeSet::new(),
            target_completions: BTreeSet::new(),
            completed_feature_sources: BTreeSet::new(),
            active_target: None,
            mutable_targets: BTreeSet::new(),
            sparse_padding_targets: BTreeSet::new(),
            temporary_spills: BTreeMap::new(),
            temporary_carvers_overrides: BTreeMap::new(),
            sparse_padding_overrides: BTreeMap::new(),
            sparse_completed_targets: BTreeSet::new(),
            target_feature_winners: BTreeMap::new(),
            carvers_overrides: BTreeMap::new(),
            overrides: BTreeMap::new(),
            override_revisions: Vec::new(),
            carvers_override_revisions: Vec::new(),
            direct_target_output: false,
            feature_structure_blocks: StructureBlocks::default(),
        }
    }

    /// Prepare source-local immutable caches before admission begins. The
    /// caller supplies the authenticated replay admissions, not the bounded
    /// packet target prefix: all source completions still run in order.
    pub fn prepare_lifecycle_replay(&mut self, admissions: &[ChunkPos]) {
        self.source.prepare_lifecycle_replay(admissions);
    }

    /// Prepare and retain one target context per request target. Overworld
    /// sources use a shared mixed batch; other sources use scalar defaults.
    pub fn prepare_lifecycle_replay_contexts(&mut self, targets: &[ChunkPos]) {
        self.replay_contexts = self.source.lifecycle_replay_contexts(targets);
        self.region_feature_epoch = self
            .source
            .begin_region_feature_epoch(targets, &self.replay_contexts);
    }

    pub fn prepare_lifecycle_replay_contexts_prepared(&mut self, targets: &[ChunkPos]) {
        self.replay_contexts = self
            .source
            .lifecycle_replay_contexts_prepared(targets)
            .unwrap_or_else(|| self.source.lifecycle_replay_contexts(targets));
        self.region_feature_epoch = self
            .source
            .begin_region_feature_epoch(targets, &self.replay_contexts);
    }

    /// Install an already prepared target context before its completion.
    pub fn install_lifecycle_replay_context(
        &mut self,
        target: ChunkPos,
        context: Arc<S::ReplayContext>,
    ) {
        self.replay_contexts.insert(target, context);
    }

    /// Clear the resident replay state before starting another bounded batch.
    ///
    /// The production source remains owned by this materializer, so its staged
    /// generation caches survive between batches while resident columns and
    /// read-after-write overrides retain the same batch boundary as a fresh
    /// materializer. Call [`Self::prepare_lifecycle_replay`] after this reset.
    pub fn reset_for_lifecycle_replay(&mut self) {
        self.resident.clear();
        self.generated_resident.clear();
        self.authenticated_prefixes.clear();
        self.authenticated_stages.clear();
        self.shared_prefixes.clear();
        self.replay_context = None;
        self.replay_contexts.clear();
        self.region_feature_epoch = None;
        self.resident_stages.clear();
        self.completions.clear();
        self.target_completions.clear();
        self.completed_feature_sources.clear();
        self.active_target = None;
        self.mutable_targets.clear();
        self.sparse_padding_targets.clear();
        self.temporary_spills.clear();
        self.temporary_carvers_overrides.clear();
        self.sparse_padding_overrides.clear();
        self.sparse_completed_targets.clear();
        self.target_feature_winners.clear();
        self.carvers_overrides.clear();
        self.overrides.clear();
        self.override_revisions.clear();
        self.carvers_override_revisions.clear();
        self.direct_target_output = false;
        self.feature_structure_blocks = StructureBlocks::default();
    }

    fn record_target_feature_winner(
        &mut self,
        target: ChunkPos,
        source: ChunkPos,
        ordinal: u32,
        position: AbsoluteCell,
        state: StateId,
    ) {
        let candidate = TargetFeatureWinner {
            target,
            source,
            ordinal,
            state,
        };
        let destination = (position.0.div_euclid(16), position.2.div_euclid(16));
        match self
            .target_feature_winners
            .entry(destination)
            .or_default()
            .entry(position)
        {
            Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            Entry::Occupied(mut entry)
                if target_feature_winner_precedes(candidate, *entry.get()) =>
            {
                entry.insert(candidate);
            }
            Entry::Occupied(_) => {}
        }
    }

    #[inline]
    fn set_override(&mut self, position: AbsoluteCell, state: StateId) {
        match self.overrides.entry(position) {
            Entry::Vacant(entry) => {
                entry.insert(state);
                self.override_revisions.push((position, state));
            }
            Entry::Occupied(mut entry) if *entry.get() != state => {
                entry.insert(state);
                self.override_revisions.push((position, state));
            }
            Entry::Occupied(_) => {}
        }
    }

    #[inline]
    fn set_carvers_override(&mut self, position: AbsoluteCell, state: StateId) {
        match self.carvers_overrides.entry(position) {
            Entry::Vacant(entry) => {
                entry.insert(state);
                self.carvers_override_revisions.push((position, state));
            }
            Entry::Occupied(mut entry) if *entry.get() != state => {
                entry.insert(state);
                self.carvers_override_revisions.push((position, state));
            }
            Entry::Occupied(_) => {}
        }
    }

    /// Read the optional source computation counter used by lifecycle parity
    /// controls. It has no effect on replay state or generated output.
    #[must_use]
    pub fn lifecycle_pre_decoration_computations(&self) -> Option<usize> {
        self.source.lifecycle_pre_decoration_computations()
    }

    #[must_use]
    pub fn feature_dispatch(&self) -> LifecycleFeatureDispatch {
        self.source.feature_dispatch()
    }

    /// Return `(override_entries_consumed, target_completions)` for the
    /// request-owned region epoch. A healthy production epoch consumes each
    /// revision once while the target count continues to grow.
    #[must_use]
    pub fn region_feature_override_counts(&self) -> Option<(usize, usize)> {
        self.region_feature_epoch
            .as_ref()
            .map(lodestone_worldgen::overworld::RegionFeatureEpoch::override_application_counts)
    }

    /// Take the typed structure trace emitted by completed FEATURES bodies.
    /// The caller commits it as the Features `StructureBlocks` product.
    pub fn take_feature_structure_blocks(&mut self) -> StructureBlocks {
        std::mem::take(&mut self.feature_structure_blocks)
    }

    /// Admit and apply a validated target plan without changing its order.
    pub fn replay_plan(&mut self, plan: &LifecycleReplayPlan) {
        for &admission in plan.admissions() {
            self.admit(admission);
        }
        match self.feature_dispatch() {
            LifecycleFeatureDispatch::TargetOwned => {
                assert_eq!(
                    plan.feature_events().len(),
                    1,
                    "target-owned FEATURES replay requires exactly one target event",
                );
                let event = &plan.feature_events()[0];
                assert_eq!(
                    event.source,
                    plan.target(),
                    "target-owned FEATURES replay event must name its target",
                );
                self.complete_target_features_with_residents(
                    plan.target(),
                    event.sequence,
                    &event.resident_transitions,
                );
            }
            LifecycleFeatureDispatch::SourceOrdered => {
                for event in plan.feature_events() {
                    self.complete_for_target_with_residents(
                        plan.target(),
                        event.source,
                        event.stage,
                        event.sequence,
                        &event.resident_transitions,
                    );
                }
            }
        }
        self.finish_target(plan.target());
    }

    /// Admit one shaped resident column.
    pub fn admit(&mut self, chunk: ChunkPos) {
        if let Some(column) = self.source.generated_shaped_column(chunk.0, chunk.1) {
            self.admit_generated(chunk, Arc::new(column));
        } else {
            let column = self.source.shaped_column(chunk.0, chunk.1);
            self.admit_shaped(chunk, column);
        }
    }

    /// Admit a previously authenticated shaped resident without invoking the
    /// source generator again.
    pub fn admit_existing(&mut self, chunk: ChunkPos, column: ChunkColumn) {
        self.admit_shaped(chunk, column);
    }

    /// Makes batch targets retain cross-target writes through packet finalization.
    pub fn declare_mutable_targets(
        &mut self,
        targets: impl IntoIterator<Item = ChunkPos>,
    ) {
        for target in targets {
            assert!(
                self.is_admitted(target),
                "lifecycle batch target {target:?} was not admitted"
            );
            self.mutable_targets.insert(target);
        }
    }

    /// Mark targets whose local completion must remain sparse.
    pub fn declare_sparse_padding_targets(
        &mut self,
        targets: impl IntoIterator<Item = ChunkPos>,
    ) {
        self.sparse_padding_targets.clear();
        self.sparse_padding_targets.extend(targets);
    }

    #[must_use]
    pub fn target_features_completed(&self, target: ChunkPos) -> bool {
        self.target_completions
            .contains(&(target, target, LifecycleCompletion::Features))
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
        self.admit_many_parallel_with(chunks, &PersistentWorldgenExecutor);
    }

    /// Admit shaped residents through a caller-selected immutable executor.
    pub fn admit_many_parallel_with(
        &mut self,
        chunks: &[ChunkPos],
        executor: &dyn ImmutableComputeExecutor,
    ) where
        S: Sync,
    {
        self.admit_region_with(chunks, executor);
    }

    /// Admit shaped residents after warming a source-owned replay prefix.
    pub fn admit_region_with_prefix(
        &mut self,
        chunks: &[ChunkPos],
        prefix_targets: &[ChunkPos],
        prefix_radius: i32,
        executor: &dyn ImmutableComputeExecutor,
    ) -> usize
    where
        S: Sync,
    {
        self.admit_region_with_context(
            chunks,
            chunks,
            prefix_targets,
            prefix_radius,
            executor,
        )
    }

    /// Admit carriers for `chunks` while allowing the source to lease a
    /// larger immutable read context without retaining carriers for it.
    pub fn admit_region_with_context(
        &mut self,
        chunks: &[ChunkPos],
        lease_chunks: &[ChunkPos],
        prefix_targets: &[ChunkPos],
        prefix_radius: i32,
        executor: &dyn ImmutableComputeExecutor,
    ) -> usize
    where
        S: Sync,
    {
        let mut seen = BTreeSet::new();
        let jobs = chunks
            .iter()
            .copied()
            .filter(|chunk| seen.insert(*chunk) && !self.is_admitted(*chunk))
            .collect::<Vec<_>>();
        if jobs.is_empty() {
            return 0;
        }
        let admitted = jobs.len();
        let source = &self.source;
        let generated = source.generated_shaped_columns_with_context(
            &jobs,
            lease_chunks,
            prefix_targets,
            prefix_radius,
        );
        let (jobs, generated) = match generated {
            Some(columns) => (
                jobs,
                columns.into_iter().map(Some).collect::<Vec<_>>(),
            ),
            None => {
                let mut seen = BTreeSet::new();
                let fallback_jobs = lease_chunks
                    .iter()
                    .copied()
                    .filter(|chunk| seen.insert(*chunk) && !self.is_admitted(*chunk))
                    .collect::<Vec<_>>();
                let generated = executor.execute_generated_shaped(
                    fallback_jobs.clone(),
                    &|(cx, cz)| source.generated_shaped_column(cx, cz),
                );
                (fallback_jobs, generated)
            }
        };
        assert_eq!(generated.len(), jobs.len(), "worldgen dispatcher changed admission count");
        let fallback_jobs = jobs
            .iter()
            .copied()
            .zip(generated.iter())
            .filter_map(|(chunk, generated)| generated.is_none().then_some(chunk))
            .collect::<Vec<_>>();
        let fallback = executor.execute_shaped(
            fallback_jobs.clone(),
            &|(cx, cz)| source.shaped_column(cx, cz),
        );
        assert_eq!(fallback.len(), fallback_jobs.len(), "worldgen dispatcher changed fallback count");
        let mut fallback = fallback.into_iter();
        for (chunk, generated) in jobs.into_iter().zip(generated) {
            if let Some(generated) = generated {
                self.admit_generated(chunk, Arc::new(generated));
            } else {
                self.admit_shaped(
                    chunk,
                    fallback
                        .next()
                        .expect("every non-generated admission has one fallback column"),
                );
            }
        }
        admitted
    }

    /// Admit the missing portion of a shared generation region.
    pub fn admit_region_with(
        &mut self,
        chunks: &[ChunkPos],
        executor: &dyn ImmutableComputeExecutor,
    ) -> usize
    where
        S: Sync,
    {
        self.admit_region_with_prefix(chunks, chunks, 0, executor)
    }

    /// Whether this materializer holds a shaped resident.
    #[must_use]
    pub fn is_admitted(&self, chunk: ChunkPos) -> bool {
        self.resident.contains_key(&chunk) || self.generated_resident.contains_key(&chunk)
    }

    /// Number of shaped residents retained by this materializer.
    #[must_use]
    pub fn resident_count(&self) -> usize {
        self.resident.len() + self.generated_resident.len()
    }

    /// Number of admitted shaped products that still use the generator's
    /// compact representation.
    #[must_use]
    pub fn generated_resident_count(&self) -> usize {
        self.generated_resident.len()
    }

    /// Number of admitted products converted to the mutable server carrier.
    #[must_use]
    pub fn materialized_resident_count(&self) -> usize {
        self.resident.len()
    }

    fn admit_shaped(&mut self, chunk: ChunkPos, column: ChunkColumn) {
        assert!(
            self.resident.insert(chunk, column).is_none(),
            "duplicate lifecycle admission for {chunk:?}"
        );
        assert!(
            self.resident_stages
                .insert(chunk, LifecycleResidentStage::Carvers)
                .is_none(),
            "duplicate lifecycle status for {chunk:?}"
        );
    }

    fn admit_generated(
        &mut self,
        chunk: ChunkPos,
        column: Arc<lodestone_worldgen::overworld::GeneratedColumn>,
    ) {
        assert!(
            self.generated_resident.insert(chunk, column).is_none(),
            "duplicate lifecycle admission for {chunk:?}"
        );
        assert!(
            self.resident_stages
                .insert(chunk, LifecycleResidentStage::Carvers)
                .is_none(),
            "duplicate lifecycle status for {chunk:?}"
        );
    }

    /// Admit a generated product restored from a session aggregate.
    pub fn admit_generated_existing(
        &mut self,
        chunk: ChunkPos,
        column: Arc<lodestone_worldgen::overworld::GeneratedColumn>,
    ) {
        self.admit_generated(chunk, column);
    }

    /// Whether a coordinate is still represented by its authenticated typed
    /// shaped product rather than a persisted or edited carrier.
    #[must_use]
    pub fn has_generated_resident(&self, chunk: ChunkPos) -> bool {
        self.generated_resident.contains_key(&chunk) && !self.resident.contains_key(&chunk)
    }

    /// Materialize one typed shaped product exactly once, preserving the
    /// existing `ChunkColumn` APIs for mutation, lighting and packet code.
    fn materialize_resident(&mut self, chunk: ChunkPos) {
        if self.resident.contains_key(&chunk) {
            return;
        }
        let generated = self
            .generated_resident
            .remove(&chunk)
            .expect("generated resident was admitted before materialization");
        #[cfg(test)]
        crate::chunk::record_generated_materialization();
        let generated = match Arc::try_unwrap(generated) {
            Ok(generated) => {
                #[cfg(test)]
                record_generated_resident_unwrap();
                generated
            }
            Err(generated) => (*generated).clone(),
        };
        self.resident
            .insert(chunk, ChunkColumn::from_generated(generated));
        self.apply_sparse_padding_overrides(chunk);
    }

    #[cfg(test)]
    pub(crate) fn materialize_resident_for_test(&mut self, chunk: ChunkPos) {
        self.materialize_resident(chunk);
    }

    /// Materialize one admitted column for a packet neighbour or another
    /// boundary that consumes the stable server carrier.
    pub fn materialize_admitted(&mut self, chunk: ChunkPos) {
        self.materialize_resident(chunk);
    }

    /// A target-owned FEATURES body has no later source body that can read its
    /// dependency writes. When those writes are explicitly transactional, keep
    /// them in the ordered override log and avoid converting an untouched
    /// compact generated neighbour. The existing carrier remains authoritative
    /// whenever the destination was already materialized or the source marks
    /// its spills persistent.
    fn defer_target_spill(
        &self,
        target_scoped: bool,
        target_owned: bool,
        destination: ChunkPos,
    ) -> bool {
        target_scoped
            && target_owned
            && !self.source.target_spills_persist()
            && !self.mutable_targets.contains(&destination)
            && !self.resident.contains_key(&destination)
    }

    /// A sparse padding source can run before the requested target that will
    /// consume its revision stream directly. Keep that target compact until
    /// its direct completion folds the ordered write into the final column.
    fn defer_sparse_write_for_direct_target(
        &self,
        mode: LifecycleCompletionMode,
        destination: ChunkPos,
        sparse_padding_destination: bool,
    ) -> bool {
        matches!(mode, LifecycleCompletionMode::SparsePadding)
            && !sparse_padding_destination
            && self.mutable_targets.contains(&destination)
            && self.generated_resident.contains_key(&destination)
            && !self.resident.contains_key(&destination)
            && self.has_authenticated_target_output(destination)
            && self.source.direct_target_output_from_generated_prefix()
            && self.replay_contexts.contains_key(&destination)
            && !self.target_features_completed(destination)
    }

    /// Share one immutable shaped resident and its derived metadata across
    /// every target session in the current batch. The mutable resident remains
    /// authoritative; this cache is dropped when a target transaction finishes.
    pub fn shared_resident_prefix(
        &mut self,
        chunk: ChunkPos,
        boundary: ColumnStage,
        fingerprint: impl FnOnce(&ChunkColumn) -> [u8; 32],
    ) -> Option<(Arc<ChunkColumn>, [u8; 32], usize)> {
        let key = (chunk, boundary);
        if let Some(prefix) = self.shared_prefixes.get(&key) {
            return Some((
                Arc::clone(&prefix.column),
                prefix.fingerprint,
                prefix.retained_bytes,
            ));
        }
        let column = Arc::new(self.resident.get(&chunk)?.clone());
        let prefix = SharedPrefix {
            fingerprint: fingerprint(&column),
            retained_bytes: column.memory_census().logical_total(),
            column,
        };
        let result = (
            Arc::clone(&prefix.column),
            prefix.fingerprint,
            prefix.retained_bytes,
        );
        self.shared_prefixes.insert(key, prefix);
        Some(result)
    }

    /// Share one typed shaped resident without forcing the server carrier.
    /// The fingerprint is supplied by the caller because the lifecycle ledger
    /// owns its product identity independently of the source representation.
    pub fn shared_generated_prefix(
        &mut self,
        chunk: ChunkPos,
        _boundary: ColumnStage,
        fingerprint: impl FnOnce(&lodestone_worldgen::overworld::GeneratedColumn) -> [u8; 32],
    ) -> Option<(
        Arc<lodestone_worldgen::overworld::GeneratedColumn>,
        [u8; 32],
        usize,
    )> {
        if self.resident.contains_key(&chunk) {
            return None;
        }
        let generated = Arc::clone(self.generated_resident.get(&chunk)?);
        let retained_bytes = std::mem::size_of_val(generated.as_ref())
            + generated.height() as usize * 16 * 16 * std::mem::size_of::<u16>();
        let result = (Arc::clone(&generated), fingerprint(&generated), retained_bytes);
        Some(result)
    }

    /// Record the digest of a prefix whose generator identity authenticates
    /// every immutable input that can affect its bytes. Dynamic, edited, and
    /// persisted columns deliberately never call this method and therefore
    /// retain the exact content-digest fallback.
    pub fn mark_authenticated_prefix(&mut self, chunk: ChunkPos, digest: [u8; 32]) {
        assert!(
            self.generated_resident.contains_key(&chunk),
            "authenticated prefix {chunk:?} was not admitted as a generated resident"
        );
        self.authenticated_prefixes.insert(chunk, digest);
    }

    /// Return the authenticated prefix identity for one resident, if any.
    #[must_use]
    pub fn authenticated_prefix_digest(&self, chunk: ChunkPos) -> Option<[u8; 32]> {
        self.authenticated_prefixes.get(&chunk).copied()
    }

    /// Replace the shaped identity with the authenticated FEATURES identity
    /// produced by a direct target-owned dispatcher. The dispatcher has
    /// already supplied the complete target column; this small identity is
    /// therefore cheaper and safer than hashing that column again.
    pub fn mark_authenticated_features(&mut self, chunk: ChunkPos, digest: [u8; 32]) {
        assert!(
            self.has_direct_target_output(),
            "authenticated FEATURES require direct target output"
        );
        let prefix = self
            .authenticated_prefixes
            .get(&chunk)
            .copied()
            .expect("authenticated FEATURES require an authenticated shaped prefix");
        self.authenticated_stages.insert(
            (chunk, LifecycleCompletion::Features),
            authenticated_features_identity(digest, prefix),
        );
    }

    /// Return the authenticated FEATURES identity, when the target has one.
    #[must_use]
    pub fn authenticated_features_digest(&self, chunk: ChunkPos) -> Option<[u8; 32]> {
        self.authenticated_stages
            .get(&(chunk, LifecycleCompletion::Features))
            .copied()
    }

    fn record_authenticated_write(
        &mut self,
        target: ChunkPos,
        source: ChunkPos,
        destination: ChunkPos,
        position: (i32, i32, i32),
        state: StateId,
        transient: bool,
    ) {
        // A direct target result already contains its own target writes. The
        // target's digest is replaced with that complete-result identity once
        // the dispatcher returns; folding those same writes here would count
        // them twice. Cross-target writes still extend the destination's
        // sparse authenticated chain when that destination is mutable.
        if transient || (target == source && destination == target) {
            return;
        }
        let update = |digest: &mut [u8; 32]| {
            let mut hasher = Sha256::new();
            hasher.update(b"lodestone-worldgen-authenticated-write-v1");
            hasher.update(&*digest);
            hasher.update(position.0.to_le_bytes());
            hasher.update(position.1.to_le_bytes());
            hasher.update(position.2.to_le_bytes());
            hasher.update(state.raw().to_le_bytes());
            *digest = hasher.finalize().into();
        };
        if let Some(digest) = self.authenticated_prefixes.get_mut(&destination) {
            update(digest);
        }
        if let Some(digest) = self
            .authenticated_stages
            .get_mut(&(destination, LifecycleCompletion::Features))
        {
            update(digest);
        }
    }

    fn retain_sparse_padding_write(
        &mut self,
        _mode: LifecycleCompletionMode,
        destination: ChunkPos,
        position: AbsoluteCell,
        state: StateId,
    ) -> bool {
        if self.sparse_padding_targets.contains(&destination) {
            self.sparse_padding_overrides
                .entry(destination)
                .or_default()
                .insert(position, state);
            return self.resident.contains_key(&destination);
        }
        false
    }

    fn retain_temporary_carvers_override(
        &mut self,
        mode: LifecycleCompletionMode,
        position: AbsoluteCell,
    ) {
        if matches!(mode, LifecycleCompletionMode::SparsePadding) {
            return;
        }
        self.temporary_carvers_overrides
            .entry(position)
            .or_insert_with(|| self.carvers_overrides.get(&position).cloned());
    }

    /// Whether a generated prefix is authenticated for direct target output.
    /// Ordered overrides are replayed into the direct completion itself.
    #[must_use]
    pub fn has_authenticated_target_output(&self, chunk: ChunkPos) -> bool {
        self.authenticated_prefixes.contains_key(&chunk)
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
        observe: impl FnMut(&LifecycleSpill),
    ) {
        self.complete_observing_for_target(
            source,
            source,
            stage,
            sequence,
            &[],
            false,
            false,
            LifecycleCompletionMode::Full,
            observe,
        );
    }

    /// Apply one completion with authenticated per-resident status/map
    /// transitions and observe every resulting write.
    pub fn complete_observing_with_residents(
        &mut self,
        source: ChunkPos,
        stage: LifecycleCompletion,
        sequence: u64,
        resident_transitions: &[LifecycleResidentTransition],
        observe: impl FnMut(&LifecycleSpill),
    ) {
        self.complete_observing_for_target(
            source,
            source,
            stage,
            sequence,
            resident_transitions,
            false,
            false,
            LifecycleCompletionMode::Full,
            observe,
        );
    }

    pub fn complete_for_target(
        &mut self,
        target: ChunkPos,
        source: ChunkPos,
        stage: LifecycleCompletion,
        sequence: u64,
    ) {
        self.complete_for_target_observing(target, source, stage, sequence, |_| {});
    }

    /// Apply one target-scoped completion while exposing its ordered writes.
    pub fn complete_for_target_observing(
        &mut self,
        target: ChunkPos,
        source: ChunkPos,
        stage: LifecycleCompletion,
        sequence: u64,
        observe: impl FnMut(&LifecycleSpill),
    ) {
        self.begin_target(target);
        self.complete_observing_for_target(
            target,
            source,
            stage,
            sequence,
            &[],
            true,
            false,
            LifecycleCompletionMode::Full,
            observe,
        );
    }

    /// Apply one target-scoped completion with externally observed resident
    /// status/map transitions. The stream comparator uses this boundary for
    /// End events; keeping it public prevents the version-specific parser
    /// from reaching into the materializer's resident state.
    pub fn complete_for_target_with_residents(
        &mut self,
        target: ChunkPos,
        source: ChunkPos,
        stage: LifecycleCompletion,
        sequence: u64,
        resident_transitions: &[LifecycleResidentTransition],
    ) {
        self.begin_target(target);
        self.complete_observing_for_target(
            target,
            source,
            stage,
            sequence,
            resident_transitions,
            true,
            false,
            LifecycleCompletionMode::Full,
            |_| {},
        );
    }

    /// Apply one target-owned FEATURES dispatcher and expose its ordered
    /// mutation stream.
    pub fn complete_target_features_observing(
        &mut self,
        target: ChunkPos,
        sequence: u64,
        observe: impl FnMut(&LifecycleSpill),
    ) {
        self.complete_target_features_with_residents_observing(target, sequence, &[], observe);
    }

    /// Apply one target-owned FEATURES dispatcher with authenticated resident
    /// status/map transitions and expose its ordered mutation stream.
    pub fn complete_target_features_with_residents_observing(
        &mut self,
        target: ChunkPos,
        sequence: u64,
        resident_transitions: &[LifecycleResidentTransition],
        mut observe: impl FnMut(&LifecycleSpill),
    ) {
        #[cfg(test)]
        record_completion_mode(LifecycleCompletionMode::Full);
        assert_eq!(
            self.source.feature_dispatch(),
            LifecycleFeatureDispatch::TargetOwned,
            "target-owned FEATURES was requested for a source-ordered dimension",
        );
        self.begin_target_mode(target, true, LifecycleCompletionMode::Full);
        self.complete_observing_for_target(
            target,
            target,
            LifecycleCompletion::Features,
            sequence,
            resident_transitions,
            true,
            true,
            LifecycleCompletionMode::Full,
            &mut observe,
        );
    }

    /// Apply one target-owned FEATURES dispatcher with authenticated resident
    /// status/map transitions.
    pub fn complete_target_features_with_residents(
        &mut self,
        target: ChunkPos,
        sequence: u64,
        resident_transitions: &[LifecycleResidentTransition],
    ) {
        self.complete_target_features_with_residents_observing(
            target,
            sequence,
            resident_transitions,
            |_| {},
        );
    }

    /// Apply one target-owned sparse padding writer and expose its mutations.
    pub fn complete_target_features_sparse_observing(
        &mut self,
        target: ChunkPos,
        sequence: u64,
        observe: impl FnMut(&LifecycleSpill),
    ) {
        #[cfg(test)]
        record_completion_mode(LifecycleCompletionMode::SparsePadding);
        assert_eq!(
            self.source.feature_dispatch(),
            LifecycleFeatureDispatch::TargetOwned,
            "sparse padding requires target-owned FEATURES",
        );
        self.begin_target_mode(target, true, LifecycleCompletionMode::SparsePadding);
        self.complete_observing_for_target(
            target,
            target,
            LifecycleCompletion::Features,
            sequence,
            &[],
            true,
            true,
            LifecycleCompletionMode::SparsePadding,
            observe,
        );
    }

    /// Apply one target-owned completion with an explicit mode.
    pub fn complete_target_features_with_mode_observing(
        &mut self,
        target: ChunkPos,
        sequence: u64,
        mode: LifecycleCompletionMode,
        observe: impl FnMut(&LifecycleSpill),
    ) {
        match mode {
            LifecycleCompletionMode::Full => {
                self.complete_target_features_observing(target, sequence, observe)
            }
            LifecycleCompletionMode::SparsePadding => {
                self.complete_target_features_sparse_observing(target, sequence, observe)
            }
        }
    }

    /// Marks a packet target mutable and starts its packet-local transaction.
    pub fn begin_target(&mut self, target: ChunkPos) {
        self.begin_target_mode(target, false, LifecycleCompletionMode::Full);
    }

    fn begin_target_mode(
        &mut self,
        target: ChunkPos,
        target_owned: bool,
        mode: LifecycleCompletionMode,
    ) {
        assert!(
            self.is_admitted(target),
            "lifecycle target {target:?} was not admitted before completion"
        );
        let promote_sparse = target_owned
            && matches!(mode, LifecycleCompletionMode::Full)
            && self.sparse_completed_targets.contains(&target);
        let direct_generated = target_owned
            && matches!(mode, LifecycleCompletionMode::Full)
            && !promote_sparse
            && self.generated_resident.contains_key(&target)
            && self.has_authenticated_target_output(target)
            && self.source.direct_target_output_from_generated_prefix()
            && self.replay_contexts.contains_key(&target);
        if promote_sparse {
            self.promote_sparse_target(target);
        } else if !matches!(mode, LifecycleCompletionMode::SparsePadding) && !direct_generated {
            self.materialize_resident(target);
        }
        assert!(
            self.active_target.is_none() || self.active_target == Some(target),
            "lifecycle target {:?} was not finished before starting {:?}",
            self.active_target,
            target,
        );
        if self.active_target != Some(target) {
            self.completed_feature_sources.clear();
            self.replay_context = self
                .replay_contexts
                .get(&target)
                .cloned()
                .or_else(|| (!target_owned).then(|| self.source.lifecycle_replay_context(target)));
            self.direct_target_output = false;
        }
        self.active_target = Some(target);
        self.mutable_targets.insert(target);
        if promote_sparse {
            self.direct_target_output = true;
        }
    }

    /// Ends a packet-local transaction, restoring incidental writes to future
    /// targets while retaining all writes to targets whose status is mutable.
    pub fn finish_target(&mut self, target: ChunkPos) {
        assert_eq!(self.active_target, Some(target), "finished lifecycle target out of order");
        let temporary_spills = std::mem::take(&mut self.temporary_spills);
        let temporary_carvers_overrides = std::mem::take(&mut self.temporary_carvers_overrides);
        let mut restores = BTreeMap::<ChunkPos, Vec<(i32, i32, i32, StateId)>>::new();
        for (position, (destination, previous, previous_override)) in temporary_spills {
            if let Some(previous) = previous {
                restores.entry(destination).or_default().push((
                    position.0.rem_euclid(16),
                    position.1,
                    position.2.rem_euclid(16),
                    previous,
                ));
            }
            match previous_override {
                Some(value) => {
                    self.set_override(position, value);
                }
                None => {
                    self.overrides.remove(&position);
                }
            }
        }
        for (destination, writes) in restores {
            self.materialize_resident(destination);
            if let Some(column) = self.resident.get_mut(&destination) {
                column.apply_ordered_block_id_batch(&writes);
            }
        }
        for (position, previous) in temporary_carvers_overrides {
            match previous {
                Some(value) => {
                    self.set_carvers_override(position, value);
                }
                None => {
                    self.carvers_overrides.remove(&position);
                }
            }
        }
        self.active_target = None;
        self.replay_context = None;
        self.shared_prefixes.clear();
    }

    /// Rolls back an active target transaction.
    pub fn abort_target(&mut self, target: ChunkPos) {
        if self.active_target == Some(target) {
            self.finish_target(target);
        }
    }

    /// Whether the active target already includes its complete post-feature
    /// column and should skip recomputation of the top-layer body.
    #[must_use]
    pub fn has_direct_target_output(&self) -> bool {
        self.direct_target_output
    }

    /// Run the source-local post-FEATURES pass once for a target after all of
    /// its ordered FEATURES sources have committed.
    pub fn complete_target_post_features(
        &mut self,
        target: ChunkPos,
        mut observe: impl FnMut(&LifecycleSpill),
    ) {
        self.begin_target(target);
        let spills = self.source.post_features_spills(target, &self.overrides);
        for spill in &spills {
            assert_eq!(spill.source, target);
            let destination = (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            );
            assert_eq!(destination, target);
            assert!(self.resident_contains_y(destination, spill.position.1));
        }
        let mut writes = Vec::new();
        for spill in &spills {
            observe(spill);
            self.set_override(spill.position, spill.state);
            writes.push((
                spill.position.0.rem_euclid(16),
                spill.position.1,
                spill.position.2.rem_euclid(16),
                spill.state,
            ));
        }
        if !writes.is_empty() {
            self.ensure_client_heightmaps(target);
            self.resident
                .get_mut(&target)
                .expect("post-FEATURES target was admitted")
                .apply_ordered_block_id_batch(&writes);
        }
        self.finish_target(target);
    }

    fn complete_observing_for_target(
        &mut self,
        target: ChunkPos,
        source: ChunkPos,
        stage: LifecycleCompletion,
        sequence: u64,
        resident_transitions: &[LifecycleResidentTransition],
        target_scoped: bool,
        target_owned: bool,
        mode: LifecycleCompletionMode,
        mut observe: impl FnMut(&LifecycleSpill),
    ) {
        assert!(
            self.is_admitted(source),
            "lifecycle completion source {source:?} was not admitted before sequence {sequence}"
        );
        let generated_target = target_owned
            && source == target
            && (self.generated_resident.contains_key(&source)
                || self.has_authenticated_target_output(source));
        let authenticated_target_output = target_owned
            && source == target
            && if generated_target {
                self.has_authenticated_target_output(source)
            } else {
                !self.source.direct_target_output_requires_authentication()
            };
        let direct_from_generated_prefix = matches!(mode, LifecycleCompletionMode::Full)
            && target_owned
            && source == target
            && generated_target
            && authenticated_target_output
            && self.source.direct_target_output_from_generated_prefix()
            && self.replay_context.is_some()
            && !resident_transitions.iter().any(|transition| transition.resident == source);
        if !matches!(mode, LifecycleCompletionMode::SparsePadding)
            && !direct_from_generated_prefix
        {
            self.materialize_resident(source);
        }
        assert!(!target_owned || target_scoped, "target-owned FEATURES must be target-scoped");
        let promoting_sparse = target_owned
            && target_scoped
            && source == target
            && matches!(mode, LifecycleCompletionMode::Full)
            && self.sparse_completed_targets.remove(&target);
        let inserted = if target_scoped {
            promoting_sparse || self.target_completions.insert((target, source, stage))
        } else {
            self.completions.insert((source, stage))
        };
        assert!(
            inserted,
            "duplicate lifecycle completion for target {target:?}, source {source:?} at sequence {sequence} ({stage:?})"
        );
        self.apply_resident_transitions(resident_transitions);
        self.advance_resident_stage(source, stage);
        if stage == LifecycleCompletion::Full {
            return;
        }
        // A source that reaches FEATURES is itself mutable even when it was
        // admitted as a dependency of another packet.  Its own writes must
        // persist in that resident column; only writes crossing into a later
        // status boundary are transaction-local.
        if target_scoped {
            self.mutable_targets.insert(source);
            // A preceding source may have written into this column before it
            // entered FEATURES. That write is now part of the retained
            // source state, not an incidental future-target write.
            self.temporary_spills
                .retain(|_, (destination, _, _)| *destination != source);
            self.temporary_carvers_overrides.retain(|position, _| {
                (position.0.div_euclid(16), position.2.div_euclid(16)) != source
            });
        }

        // Heightmaps become live when the source enters FEATURES, before its
        // own feature body writes anything. A neighbour that entered earlier
        // already has live maps, so the same spill updates that destination
        // incrementally through `ChunkColumn::set_block` below.
        if !matches!(mode, LifecycleCompletionMode::SparsePadding)
            && !direct_from_generated_prefix
        {
            self.ensure_client_heightmaps(source);
        }

        let mut target_local_features = Vec::new();
        let mut dirty_sparse_residents = BTreeSet::new();
        let (result, completed_feature_sources) = if target_owned {
            assert_eq!(source, target, "target-owned FEATURES source must be its target");
            if promoting_sparse {
                (LifecycleFeatureResult::default(), Vec::new())
            } else if matches!(mode, LifecycleCompletionMode::SparsePadding) {
                let context = self
                    .replay_context
                    .as_deref()
                    .expect("target replay context is built at target admission");
                let feature_overrides = if target_scoped && self.source.target_feature_reads_carvers() {
                    &self.carvers_overrides
                } else {
                    &self.overrides
                };
                let override_revisions =
                    if target_scoped && self.source.target_feature_reads_carvers() {
                        &self.carvers_override_revisions
                    } else {
                        &self.override_revisions
                    };
                let sparse = if let Some(epoch) = self.region_feature_epoch.as_mut() {
                    let result = self.source
                        .target_feature_result_sparse_with_replay_context_and_epoch_revisions(
                            target,
                            feature_overrides,
                            &self.resident,
                            context,
                            epoch,
                            override_revisions,
                        );
                    #[cfg(test)]
                    if result.is_some() {
                        record_region_feature_epoch(LifecycleCompletionMode::SparsePadding);
                    }
                    result
                } else {
                    #[cfg(test)]
                    record_region_feature_scalar_fallback();
                    self.source
                        .target_feature_result_sparse_with_replay_context(
                            target,
                            feature_overrides,
                            &self.resident,
                            context,
                        )
                }
                .expect("target-owned source must provide sparse padding completion");
                target_local_features = sparse.local_features;
                (
                    LifecycleFeatureResult {
                        spills: sparse.spills,
                        block_entities: sparse.block_entities,
                        ..LifecycleFeatureResult::default()
                    },
                    Vec::new(),
                )
            } else if authenticated_target_output {
                let direct = {
                    let feature_overrides =
                        if target_scoped && self.source.target_feature_reads_carvers() {
                            &self.carvers_overrides
                        } else {
                            &self.overrides
                        };
                    let override_revisions =
                        if target_scoped && self.source.target_feature_reads_carvers() {
                            &self.carvers_override_revisions
                        } else {
                            &self.override_revisions
                        };
                    self.replay_context
                        .as_deref()
                        .and_then(|context| {
                            if let Some(epoch) = self.region_feature_epoch.as_mut() {
                                let result = self.source
                                    .target_feature_result_direct_with_replay_context_and_epoch_revisions(
                                        target,
                                        feature_overrides,
                                        &self.resident,
                                        context,
                                        epoch,
                                        override_revisions,
                                    );
                                #[cfg(test)]
                                if result.is_some() {
                                    record_region_feature_epoch(LifecycleCompletionMode::Full);
                                }
                                result
                            } else {
                                #[cfg(test)]
                                record_region_feature_scalar_fallback();
                                self.source.target_feature_result_direct_with_replay_context(
                                    target,
                                    feature_overrides,
                                    &self.resident,
                                    context,
                                )
                            }
                        })
                        .or_else(|| {
                            #[cfg(test)]
                            record_region_feature_scalar_fallback();
                            self.source.target_feature_result_direct(
                                target,
                                feature_overrides,
                                &self.resident,
                            )
                        })
                };
                if let Some(direct) = direct {
                    let retained_heightmaps = self
                        .resident
                        .get(&target)
                        .and_then(ChunkColumn::client_heightmaps_raw);
                    let mut column = direct.column;
                    if let Some(heightmaps) = retained_heightmaps {
                        column.install_client_heightmaps_raw(heightmaps);
                    }
                    self.resident.insert(target, column);
                    self.direct_target_output = true;
                    target_local_features = direct.local_features;
                    (
                        LifecycleFeatureResult {
                            spills: direct.spills,
                            ..LifecycleFeatureResult::default()
                        },
                        Vec::new(),
                    )
                } else {
                    if direct_from_generated_prefix {
                        self.materialize_resident(source);
                        self.ensure_client_heightmaps(source);
                    }
                    let feature_overrides =
                        if target_scoped && self.source.target_feature_reads_carvers() {
                            &self.carvers_overrides
                        } else {
                            &self.overrides
                        };
                    (
                        self.source
                            .target_feature_result(target, feature_overrides, &self.resident),
                        Vec::new(),
                    )
                }
            } else {
                let feature_overrides = if target_scoped && self.source.target_feature_reads_carvers() {
                    &self.carvers_overrides
                } else {
                    &self.overrides
                };
                (
                    self.source
                        .target_feature_result(target, feature_overrides, &self.resident),
                    Vec::new(),
                )
            }
        } else if target_scoped {
            let feature_overrides = if target_scoped && self.source.target_feature_reads_carvers() {
                &self.carvers_overrides
            } else {
                &self.overrides
            };
            let context = self
                .replay_context
                .as_deref()
                .expect("target replay context is built at target admission");
            self.source.feature_result_for_target_with_replay_context_and_completed(
                target,
                source,
                feature_overrides,
                &self.resident,
                context,
                &self.completed_feature_sources,
            )
        } else {
            (
                self.source
                    .feature_result(source, &self.overrides, &self.resident),
                Vec::new(),
            )
        };
        if matches!(mode, LifecycleCompletionMode::SparsePadding) {
            self.sparse_completed_targets.insert(target);
        }
        for (ordinal, local) in target_local_features.iter().enumerate() {
            if target_owned && stage == LifecycleCompletion::Features {
                self.record_target_feature_winner(
                    target,
                    local.source,
                    ordinal as u32,
                    local.position,
                    local.state,
                );
            }
            assert_eq!(
                local.source, source,
                "direct target local FEATURES write disagrees with completion source"
            );
            let destination = (
                local.position.0.div_euclid(16),
                local.position.2.div_euclid(16),
            );
            assert_eq!(
                destination, target,
                "direct target local FEATURES write left its target column"
            );
            assert!(
                self.resident_contains_y(destination, local.position.1),
                "direct target local FEATURES write is outside target column"
            );
            if self.source.target_feature_reads_carvers() {
                self.set_carvers_override(local.position, local.state);
            }
            self.set_override(local.position, local.state);
            if self.retain_sparse_padding_write(
                mode,
                destination,
                local.position,
                local.state,
            ) {
                dirty_sparse_residents.insert(destination);
            }
        }
        self.feature_structure_blocks.append(result.structure_blocks);
        self.completed_feature_sources
            .extend(completed_feature_sources);
        for spill in &result.spills {
            assert_eq!(
                spill.source, source,
                "production spill source disagrees with completion source"
            );
            let destination = (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            );
            if self.is_admitted(destination) {
                assert!(
                    self.resident_contains_y(destination, spill.position.1),
                    "production spill {spill:?} is outside resident column {destination:?}"
                );
            }
        }
        for entity in &result.block_entities {
            let (x, y, z) = entity.position();
            let destination = (x.div_euclid(16), z.div_euclid(16));
            if self.is_admitted(destination) {
                assert!(
                    self.resident_contains_y(destination, y),
                    "production block entity {:?} is outside resident column {destination:?}",
                    entity.type_id()
                );
            }
        }
        for gateway in &result.end_gateways {
            let destination = (gateway.pos.0.div_euclid(16), gateway.pos.2.div_euclid(16));
            if self.is_admitted(destination) {
                self.materialize_resident(destination);
                let column = self
                    .resident
                    .get_mut(&destination)
                    .expect("gateway destination was materialized above");
                self.source.attach_end_gateways(column, std::slice::from_ref(gateway));
            }
        }
        for entity in &result.block_entities {
            let (x, _y, z) = entity.position();
            let destination = (x.div_euclid(16), z.div_euclid(16));
            // A feature can create a block entity while writing a future
            // dependency. The external lifecycle leaves that entity as a
            // deferred sidecar when the dependency has already crossed
            // FEATURES; the initial chunk record enumerates only entities
            // materialized while the requested centre is being completed. A
            // target-scoped replay has no later source event to promote such
            // a deferred record, so attaching it merely because the
            // destination is mutable would make a future packet report an
            // entity the external record omits. Direct source completion
            // retains the historical source-owned behavior because it has no
            // target transaction.
            if target_scoped
                && destination != target
                && (matches!(mode, LifecycleCompletionMode::Full)
                    || matches!(mode, LifecycleCompletionMode::SparsePadding)
                    || destination == source
                    || self.sparse_padding_targets.contains(&destination)
                    || (matches!(mode, LifecycleCompletionMode::SparsePadding)
                        && !self.mutable_targets.contains(&destination)))
            {
                continue;
            }
            if !self.is_admitted(destination) {
                continue;
            }
            self.materialize_resident(destination);
            let column = self
                .resident
                .get_mut(&destination)
                .expect("entity destination was materialized above");
            column.add_generated_block_entities(std::slice::from_ref(entity));
        }
        let mut writes = BTreeMap::<ChunkPos, Vec<(i32, i32, i32, StateId)>>::new();
        let mut source_ordinals = BTreeMap::<ChunkPos, u32>::new();
        for spill in &result.spills {
            let ordinal = source_ordinals.entry(spill.source).or_default();
            let spill_ordinal = *ordinal;
            *ordinal = spill_ordinal.saturating_add(1);
            let destination = (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            );
            if target_owned
                && stage == LifecycleCompletion::Features
                && self.mutable_targets.contains(&destination)
            {
                self.record_target_feature_winner(
                    target,
                    spill.source,
                    spill_ordinal,
                    spill.position,
                    spill.state,
                );
            }
            // Sparse padding writes stay deferred, except when they cross
            // into another requested target in this same settlement wave.
            let sparse_padding_destination = self.sparse_padding_targets.contains(&destination);
            let sparse_requested_destination = matches!(mode, LifecycleCompletionMode::SparsePadding)
                && destination != target
                && self.mutable_targets.contains(&destination)
                && !sparse_padding_destination;
            if matches!(mode, LifecycleCompletionMode::SparsePadding)
                && destination != target
                && !sparse_requested_destination
            {
                observe(spill);
                continue;
            }
            let transient = target_scoped
                && !self.source.target_spills_persist()
                && (!self.mutable_targets.contains(&destination)
                    || sparse_padding_destination
                    || (matches!(mode, LifecycleCompletionMode::SparsePadding)
                        && destination != target
                        && !sparse_requested_destination));
            let defer_direct_target = self.defer_sparse_write_for_direct_target(
                mode,
                destination,
                sparse_padding_destination,
            );
            let mut deferred = defer_direct_target;
            if transient {
                deferred |= sparse_padding_destination
                    || self.defer_target_spill(target_scoped, target_owned, destination);
                if deferred && !defer_direct_target {
                    self.retain_temporary_carvers_override(mode, spill.position);
                }
                if !deferred {
                    self.materialize_resident(destination);
                    let previous = self.resident.get(&destination).map(|column| {
                        column
                            .block_state_id(
                                spill.position.0.rem_euclid(16),
                                spill.position.1,
                                spill.position.2.rem_euclid(16),
                            )
                            .to_owned()
                    });
                    let previous_override = self.overrides.get(&spill.position).cloned();
                    self.temporary_spills
                        .entry(spill.position)
                        .or_insert((destination, previous, previous_override));
                } else if !matches!(mode, LifecycleCompletionMode::SparsePadding) {
                    let previous = self.resident.get(&destination).map(|column| {
                        column
                            .block_state_id(
                                spill.position.0.rem_euclid(16),
                                spill.position.1,
                                spill.position.2.rem_euclid(16),
                            )
                            .to_owned()
                    });
                    let previous_override = self.overrides.get(&spill.position).cloned();
                    self.temporary_spills
                        .entry(spill.position)
                        .or_insert((destination, previous, previous_override));
                }
            }
            observe(spill);
            if self.retain_sparse_padding_write(
                mode,
                destination,
                spill.position,
                spill.state,
            ) {
                dirty_sparse_residents.insert(destination);
            }
            if target_scoped && self.source.target_feature_reads_carvers() {
                self.set_carvers_override(spill.position, spill.state);
            }
            self.set_override(spill.position, spill.state);
            self.record_authenticated_write(
                target,
                source,
                destination,
                spill.position,
                spill.state,
                transient,
            );
            if self.is_admitted(destination)
                && !deferred
                && (!matches!(mode, LifecycleCompletionMode::SparsePadding)
                    || sparse_requested_destination)
            {
                self.materialize_resident(destination);
                self.ensure_client_heightmaps(destination);
                writes.entry(destination).or_default().push((
                    spill.position.0.rem_euclid(16),
                    spill.position.1,
                    spill.position.2.rem_euclid(16),
                    spill.state,
                ));
            }
        }
        for (destination, writes) in writes {
            self.materialize_resident(destination);
            self.resident
                .get_mut(&destination)
                .expect("resident destination was checked above")
                .apply_ordered_block_id_batch(&writes);
        }
        for destination in dirty_sparse_residents {
            self.apply_sparse_padding_overrides(destination);
        }
        let post_features_spills = if target_scoped {
            Vec::new()
        } else {
            self.source.post_features_spills(source, &self.overrides)
        };
        for spill in &post_features_spills {
            assert_eq!(
                spill.source, source,
                "production post-FEATURES spill source disagrees with completion source"
            );
            let destination = (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            );
            if self.is_admitted(destination) {
                assert!(
                    self.resident_contains_y(destination, spill.position.1),
                    "production post-FEATURES spill {spill:?} is outside resident column {destination:?}"
                );
            }
        }
        let mut writes = BTreeMap::<ChunkPos, Vec<(i32, i32, i32, StateId)>>::new();
        for spill in &post_features_spills {
            let destination = (
                spill.position.0.div_euclid(16),
                spill.position.2.div_euclid(16),
            );
            let transient = target_scoped
                && !self.source.target_spills_persist()
                && !self.mutable_targets.contains(&destination);
            let mut deferred = false;
            if transient {
                deferred = self.defer_target_spill(target_scoped, target_owned, destination);
            }
            if transient && !deferred {
                self.materialize_resident(destination);
                    let previous = self.resident.get(&destination).map(|column| {
                        column
                            .block_state_id(
                            spill.position.0.rem_euclid(16),
                            spill.position.1,
                            spill.position.2.rem_euclid(16),
                        )
                        .to_owned()
                });
                let previous_override = self.overrides.get(&spill.position).cloned();
                self.temporary_spills
                    .entry(spill.position)
                    .or_insert((destination, previous, previous_override));
            }
            observe(spill);
            self.set_override(spill.position, spill.state);
            self.record_authenticated_write(
                target,
                source,
                destination,
                spill.position,
                spill.state,
                transient,
            );
            if self.is_admitted(destination) && !deferred {
                self.materialize_resident(destination);
                self.ensure_client_heightmaps(destination);
                writes.entry(destination).or_default().push((
                    spill.position.0.rem_euclid(16),
                    spill.position.1,
                    spill.position.2.rem_euclid(16),
                    spill.state,
                ));
            }
        }
        for (destination, writes) in writes {
            self.materialize_resident(destination);
            self.resident
                .get_mut(&destination)
                .expect("resident destination was checked above")
                .apply_ordered_block_id_batch(&writes);
        }
        if !matches!(mode, LifecycleCompletionMode::SparsePadding) {
            self.materialize_resident(source);
            let column = self
                .resident
                .get_mut(&source)
                .expect("lifecycle source was admitted before sidecar attachment");
            self.source.attach_source_sidecars(source, column);
        }
    }

    pub fn restore_committed_mutations<'a>(
        &mut self,
        mutations: impl IntoIterator<Item = &'a ProvenanceMutation>,
    ) {
        let mut writes = BTreeMap::<ChunkPos, Vec<(i32, i32, i32, StateId)>>::new();
        for mutation in mutations {
            let state = mutation
                .get::<StateId>()
                .expect("committed worldgen mutations carry StateId");
            let position = mutation.provenance().destination();
            let cell = (position.x(), position.y(), position.z());
            if mutation.provenance().stage().stage() == ColumnStage::Features {
                self.record_target_feature_winner(
                    mutation.provenance().target(),
                    mutation.provenance().source(),
                    mutation.provenance().ordinal(),
                    cell,
                    *state,
                );
            }
            if self.source.target_feature_reads_carvers() {
                self.set_carvers_override(cell, *state);
            }
            self.set_override(cell, *state);
            let destination = (cell.0.div_euclid(16), cell.2.div_euclid(16));
            self.record_authenticated_write(
                mutation.provenance().target(),
                mutation.provenance().source(),
                destination,
                cell,
                *state,
                false,
            );
            if self.is_admitted(destination) {
                writes.entry(destination).or_default().push((
                    cell.0.rem_euclid(16),
                    cell.1,
                    cell.2.rem_euclid(16),
                    *state,
                ));
            }
        }
        for (destination, writes) in writes {
            self.materialize_resident(destination);
            let column = self
                .resident
                .get(&destination)
                .expect("restored mutation destination was checked above");
            // A target's detached direct output already contains its own
            // ordered feature and top-layer writes. Keep the provenance log
            // authoritative, but avoid replaying a write when the final
            // value for that cell is already resident. Grouping by final
            // value is important: filtering individual writes would drop an
            // intermediate feature write followed by a later top-layer write
            // and could change the observable order.
            let writes = writes
                .iter()
                .filter(|(x, y, z, _)| {
                    writes
                        .iter()
                        .rev()
                        .find(|(fx, fy, fz, _)| (*fx, *fy, *fz) == (*x, *y, *z))
                        .is_some_and(|(_, _, _, state)| {
                            *state != column.block_state_id(*x, *y, *z)
                        })
                })
                .map(|(x, y, z, state)| (*x, *y, *z, *state))
                .collect::<Vec<_>>();
            if !writes.is_empty() {
                self.resident
                    .get_mut(&destination)
                    .expect("restored mutation destination was checked above")
                    .apply_ordered_block_id_batch(&writes);
            }
        }
    }

    /// Settle each target cell from the minimum-provenance target-owned
    /// FEATURES writer before its immutable output snapshot is captured.
    pub fn apply_canonical_target_feature_winners(&mut self, target: ChunkPos) {
        let winners = self
            .target_feature_winners
            .get(&target)
            .into_iter()
            .flat_map(|winners| winners.iter())
            .map(|(&cell, winner)| (cell, winner.state))
            .collect::<Vec<_>>();
        if winners.is_empty() {
            return;
        }
        self.materialize_resident(target);
        let column = self
            .resident
            .get(&target)
            .expect("canonical FEATURES target was admitted");
        let writes = winners
            .into_iter()
            .filter_map(|(cell, state)| {
                let local = (cell.0.rem_euclid(16), cell.1, cell.2.rem_euclid(16));
                (column.block_state_id(local.0, local.1, local.2) != state)
                    .then_some((local.0, local.1, local.2, state))
            })
            .collect::<Vec<_>>();
        if !writes.is_empty() {
            self.resident
                .get_mut(&target)
                .expect("canonical FEATURES target was admitted")
                .apply_ordered_block_id_batch(&writes);
        }
    }

    /// Borrow one admitted resident column for neighbour-aware encoding.
    #[must_use]
    pub fn resident_column(&self, chunk: ChunkPos) -> Option<&ChunkColumn> {
        self.resident.get(&chunk)
    }

    /// Borrow an admitted typed shaped product without crossing into the
    /// mutable server carrier. Read-only block consumers can use this seam;
    /// mutation, heightmap, lighting, and packet consumers use
    /// [`Self::materialize_admitted`].
    #[must_use]
    pub fn generated_resident_column(
        &self,
        chunk: ChunkPos,
    ) -> Option<&lodestone_worldgen::overworld::GeneratedColumn> {
        self.generated_resident.get(&chunk).map(Arc::as_ref)
    }

    /// Clone the shared handle for a typed shaped product without
    /// materializing its compact block sections.
    #[must_use]
    pub fn generated_resident_handle(
        &self,
        chunk: ChunkPos,
    ) -> Option<Arc<lodestone_worldgen::overworld::GeneratedColumn>> {
        self.generated_resident.get(&chunk).cloned()
    }

    /// Read one block from either resident representation without forcing
    /// server materialization.
    #[must_use]
    pub fn resident_block_state(
        &self,
        chunk: ChunkPos,
        lx: usize,
        y: i32,
        lz: usize,
    ) -> Option<StateId> {
        self.resident
            .get(&chunk)
            .map(|column| column.block_state_id(lx as i32, y, lz as i32))
    }

    /// Read one canonical state id without materializing a generated resident.
    #[must_use]
    pub fn resident_block_state_id(
        &self,
        chunk: ChunkPos,
        lx: usize,
        y: i32,
        lz: usize,
    ) -> Option<StateId> {
        self.resident
            .get(&chunk)
            .map(|column| column.block_state_id(lx as i32, y, lz as i32))
            .or_else(|| {
                self.generated_resident
                    .get(&chunk)
                    .map(|column| column.block_state_id(lx, y, lz))
            })
    }

    /// Return final canonical local writes retained for a sparse packet
    /// neighbour. Transaction-local future-target spills are not included.
    pub fn sparse_padding_overlay_for_packet(
        &self,
        chunk: ChunkPos,
    ) -> Option<Vec<(i32, i32, i32, StateId)>> {
        if !self.sparse_padding_targets.contains(&chunk) {
            return None;
        }
        Some(
            self.sparse_padding_overrides
                .get(&chunk)
                .into_iter()
                .flat_map(|writes| writes.iter())
                .map(|(&(x, y, z), &state)| {
                    (x.rem_euclid(16), y, z.rem_euclid(16), state)
                })
                .collect(),
        )
    }

    fn apply_sparse_padding_overrides(&mut self, chunk: ChunkPos) {
        let writes = self.sparse_padding_overlay_for_packet(chunk).unwrap_or_default();
        if !writes.is_empty() {
            self.resident
                .get_mut(&chunk)
                .expect("sparse padding overrides require a resident")
                .apply_ordered_block_id_batch(&writes);
        }
    }

    fn promote_sparse_target(&mut self, chunk: ChunkPos) {
        self.materialize_resident(chunk);
        let writes = self
            .sparse_padding_overrides
            .remove(&chunk)
            .into_iter()
            .flat_map(|writes| writes.into_iter())
            .map(|((x, y, z), state)| (x.rem_euclid(16), y, z.rem_euclid(16), state))
            .collect::<Vec<_>>();
        if !writes.is_empty() {
            self.resident
                .get_mut(&chunk)
                .expect("promoted sparse target was materialized")
                .apply_ordered_block_id_batch(&writes);
        }
    }

    /// Return the lifecycle status of one admitted resident column.
    #[must_use]
    pub fn resident_stage(&self, chunk: ChunkPos) -> Option<LifecycleResidentStage> {
        self.resident_stages.get(&chunk).copied()
    }

    fn advance_resident_stage(&mut self, source: ChunkPos, completion: LifecycleCompletion) {
        let next = match completion {
            LifecycleCompletion::Features => LifecycleResidentStage::Features,
            LifecycleCompletion::Full => LifecycleResidentStage::Full,
        };
        let current = self
            .resident_stages
            .get(&source)
            .copied()
            .expect("lifecycle source status was checked resident above");
        // A target-scoped event may arrive after the resident has crossed a
        // later authenticated boundary. Explicit resident transitions remain
        // authoritative for the retained lifecycle status; the dispatch mode
        // decides separately whether the body has any work left to execute.
        if next > current {
            self.resident_stages.insert(source, next);
        }
    }

    fn apply_resident_transitions(&mut self, transitions: &[LifecycleResidentTransition]) {
        for transition in transitions {
            let current = self
                .resident_stages
                .get(&transition.resident)
                .copied()
                .unwrap_or_else(|| {
                    panic!(
                        "lifecycle resident {:?} was not admitted before its state transition",
                        transition.resident
                    )
                });
            assert!(
                transition.stage >= current,
                "lifecycle resident {:?} regressed from {current:?} to {:?}",
                transition.resident,
                transition.stage,
            );
            self.resident_stages
                .insert(transition.resident, transition.stage);
            self.materialize_resident(transition.resident);
            let column = self
                .resident
                .get_mut(&transition.resident)
                .expect("resident stage exists only for an admitted column");
            if let Some(maps) = transition.client_heightmaps {
                let needs_install = column
                    .client_heightmaps_raw()
                    .is_none_or(|current_maps| current_maps != maps);
                if needs_install {
                    column.install_client_heightmaps_raw(maps);
                }
            }
        }
    }

    fn resident_contains_y(&self, chunk: ChunkPos, y: i32) -> bool {
        self.resident
            .get(&chunk)
            .map_or_else(
                || {
                    self.generated_resident.get(&chunk).is_some_and(|column| {
                        (column.min_y()..column.min_y() + column.height()).contains(&y)
                    })
                },
                |column| column.contains_y(y),
            )
    }

    fn ensure_client_heightmaps(&mut self, chunk: ChunkPos) {
        self.materialize_resident(chunk);
        if self
            .resident
            .get(&chunk)
            .expect("heightmap initialization requires a resident column")
            .client_heightmaps()
            .is_some()
        {
            return;
        }
        let maps = self.source.lifecycle_client_heightmaps(chunk.0, chunk.1).unwrap_or_else(|| {
            panic!(
                "lifecycle resident {chunk:?} reached a map boundary without an authenticated map seed"
            )
        });
        self.resident
            .get_mut(&chunk)
            .expect("heightmap initialization requires a resident column")
            .install_client_heightmaps_raw(maps);
    }

    /// Clone one admitted resident column for packet encoding.
    #[must_use]
    pub fn snapshot_for_packet(&mut self, target: ChunkPos) -> ChunkColumn {
        self.materialize_resident(target);
        let mut snapshot = self
            .resident
            .get(&target)
            .cloned()
            .unwrap_or_else(|| panic!("lifecycle target {target:?} was not admitted before encoding"));
        // Packet finalization may reconcile state-owned sidecars by walking
        // the detached block field or apply a dimension-specific packet
        // filter. Lifecycle heightmaps are authenticated external transition
        // data, so preserve that snapshot across the sidecar pass instead of
        // allowing a later scan to replace it.
        let retained_heightmaps = snapshot.client_heightmaps_raw();
        // A later source completion can write a state-owned block entity into
        // this target after the target's own sidecar hook ran. Finalize only
        // the detached packet snapshot so the resident lifecycle remains the
        // authenticated replay state and richer generated payloads survive.
        self.source.finalize_packet_snapshot(target, &mut snapshot);
        if let Some(heightmaps) = retained_heightmaps {
            snapshot.install_client_heightmaps_raw(heightmaps);
        }
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sha2::{Digest, Sha256};

    use crate::chunk::ChunkSource;

    use super::*;

    fn sid(name: &str) -> StateId {
        StateId::from_state_str(name).expect("test state must be canonical")
    }

    struct CountingSource {
        feature_calls: Rc<Cell<usize>>,
    }

    struct SpillSource {
        expected_override: Rc<Cell<bool>>,
        post_features: bool,
    }

    struct StateOnlySpillSource;

    struct GeneratedEntitySpillSource;

    struct HeightmapLifecycleSource;

    struct DirectHeightmapSource;

    struct SparseBeforeDirectSource;

    struct TargetLocalReadSource {
        local_marker: bool,
        top_layer_marker: bool,
        outward_marker: bool,
    }

    struct TargetReplaySource;

    struct CrossTargetSpillSource;

    #[derive(Clone)]
    struct RegionAdmissionSource {
        shaped_calls: Arc<AtomicUsize>,
    }

    struct DispatchCountingSource {
        dispatch: LifecycleFeatureDispatch,
        target_calls: Arc<AtomicUsize>,
        source_calls: Arc<AtomicUsize>,
    }

    struct ContextDirectSource {
        direct_calls: Arc<AtomicUsize>,
    }

    fn test_heightmaps() -> Option<LifecycleClientHeightmaps> {
        Some([[0u16; 256]; 3])
    }

    fn fixture_result(source: ChunkPos) -> LifecycleFeatureResult {
        LifecycleFeatureResult {
            spills: [1, 2]
                .into_iter()
                .map(|x| LifecycleSpill {
                    source,
                    position: (source.0 * 16 + x, 0, source.1 * 16 + 1),
                    state: if x == 1 {
                        sid("minecraft:stone")
                    } else {
                        sid("minecraft:dirt")
                    },
                    transient: false,
                })
                .collect(),
            block_entities: Vec::new(),
            structure_blocks: StructureBlocks::default(),
            end_gateways: Vec::new(),
        }
    }

    impl LifecycleWorldgenSource for DirectHeightmapSource {
        type ReplayContext = ();

        fn feature_dispatch(&self) -> LifecycleFeatureDispatch {
            LifecycleFeatureDispatch::TargetOwned
        }

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            _source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            LifecycleFeatureResult::default()
        }

        fn target_feature_result_direct(
            &self,
            target: ChunkPos,
            overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> Option<LifecycleTargetFeatureResult> {
            let mut column = ChunkColumn::new(0, 1);
            for (&(x, y, z), state) in overrides {
                if (x.div_euclid(16), z.div_euclid(16)) == target {
                    column.set_block_id(x.rem_euclid(16), y, z.rem_euclid(16), *state);
                }
            }
            column.install_client_heightmaps_raw([[0u16; 256]; 3]);
            Some(LifecycleTargetFeatureResult {
                column,
                spills: Vec::new(),
                local_features: Vec::new(),
            })
        }

        fn target_feature_reads_carvers(&self) -> bool {
            true
        }
    }

    impl LifecycleWorldgenSource for SparseBeforeDirectSource {
        type ReplayContext = ();

        fn feature_dispatch(&self) -> LifecycleFeatureDispatch {
            LifecycleFeatureDispatch::TargetOwned
        }

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            _source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            LifecycleFeatureResult::default()
        }

        fn target_feature_result_direct(
            &self,
            target: ChunkPos,
            overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> Option<LifecycleTargetFeatureResult> {
            assert_eq!(target, (1, 0));
            let mut column = ChunkColumn::new(0, 1);
            column.set_block_id(
                0,
                0,
                0,
                *overrides
                    .get(&(16, 0, 0))
                    .expect("the earlier sparse write must reach the direct read view"),
            );
            column.install_client_heightmaps_raw([[0; 256]; 3]);
            Some(LifecycleTargetFeatureResult {
                column,
                spills: Vec::new(),
                local_features: Vec::new(),
            })
        }

        fn target_feature_result_sparse_with_replay_context(
            &self,
            target: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
            _context: &Self::ReplayContext,
        ) -> Option<LifecycleSparseTargetFeatureResult> {
            assert_eq!(target, (0, 0));
            Some(LifecycleSparseTargetFeatureResult {
                spills: vec![LifecycleSpill {
                    source: target,
                    position: (16, 0, 0),
                    state: sid("minecraft:dirt"),
                    transient: false,
                }],
                local_features: Vec::new(),
                block_entities: Vec::new(),
            })
        }

        fn target_feature_reads_carvers(&self) -> bool {
            true
        }

        fn direct_target_output_requires_authentication(&self) -> bool {
            true
        }

        fn direct_target_output_from_generated_prefix(&self) -> bool {
            true
        }
    }

    impl LifecycleWorldgenSource for TargetLocalReadSource {
        type ReplayContext = ();

        fn feature_dispatch(&self) -> LifecycleFeatureDispatch {
            LifecycleFeatureDispatch::TargetOwned
        }

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            _source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            LifecycleFeatureResult::default()
        }

        fn target_feature_result_direct(
            &self,
            target: ChunkPos,
            overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> Option<LifecycleTargetFeatureResult> {
            let mut column = ChunkColumn::new(0, 1);
            let mut local_features = Vec::new();
            if target == (0, 0) && (self.local_marker || self.top_layer_marker) {
                let state = if self.top_layer_marker {
                    "minecraft:snow[layers=1]"
                } else {
                    "minecraft:stone"
                };
                column.set_block_id(15, 0, 0, sid(state));
                local_features.push(LifecycleSpill {
                    source: target,
                    position: (15, 0, 0),
                    state: sid(state),
                    transient: false,
                });
            }
            if target == (1, 0) && overrides.contains_key(&(15, 0, 0)) {
                column.set_block_id(0, 0, 0, sid("minecraft:diamond_block"));
            }
            if target == (1, 0) && overrides.contains_key(&(16, 0, 0)) {
                column.set_block_id(1, 0, 0, sid("minecraft:gold_block"));
            }
            let spills = if target == (0, 0) && self.outward_marker {
                vec![LifecycleSpill {
                    source: target,
                    position: (16, 0, 0),
                    state: sid("minecraft:emerald_block"),
                    transient: false,
                }]
            } else {
                Vec::new()
            };
            column.install_client_heightmaps_raw([[0u16; 256]; 3]);
            Some(LifecycleTargetFeatureResult {
                column,
                spills,
                local_features,
            })
        }

        fn target_feature_reads_carvers(&self) -> bool {
            true
        }
    }

    impl LifecycleWorldgenSource for DispatchCountingSource {
        type ReplayContext = ();

        fn feature_dispatch(&self) -> LifecycleFeatureDispatch {
            self.dispatch
        }

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            self.source_calls.fetch_add(1, Ordering::Relaxed);
            fixture_result(source)
        }

        fn target_feature_result(
            &self,
            target: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            self.target_calls.fetch_add(1, Ordering::Relaxed);
            fixture_result(target)
        }
    }

    impl LifecycleWorldgenSource for ContextDirectSource {
        type ReplayContext = usize;

        fn feature_dispatch(&self) -> LifecycleFeatureDispatch {
            LifecycleFeatureDispatch::TargetOwned
        }

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(7)
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            _source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            LifecycleFeatureResult::default()
        }

        fn target_feature_result_direct_with_replay_context(
            &self,
            _target: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
            context: &Self::ReplayContext,
        ) -> Option<LifecycleTargetFeatureResult> {
            assert_eq!(*context, 7);
            self.direct_calls.fetch_add(1, Ordering::Relaxed);
            Some(LifecycleTargetFeatureResult {
                column: ChunkColumn::new(0, 1),
                spills: Vec::new(),
                local_features: Vec::new(),
            })
        }
    }

    impl LifecycleWorldgenSource for TargetReplaySource {
        type ReplayContext = ();

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            let (position, state) = match source {
                (0, 0) => ((16, 0, 0), "minecraft:stone"),
                (1, 0) => ((32, 0, 0), "minecraft:dirt"),
                _ => return LifecycleFeatureResult::default(),
            };
            LifecycleFeatureResult {
                spills: vec![LifecycleSpill {
                    source,
                    position,
                    state: sid(state),
                    transient: false,
                }],
                block_entities: Vec::new(),
                structure_blocks: StructureBlocks::default(),
                end_gateways: Vec::new(),
            }
        }

        fn feature_result_for_target_with_completed(
            &self,
            target: ChunkPos,
            source: ChunkPos,
            overrides: &BTreeMap<AbsoluteCell, StateId>,
            resident: &BTreeMap<ChunkPos, ChunkColumn>,
            completed_sources: &BTreeSet<ChunkPos>,
        ) -> (LifecycleFeatureResult, Vec<ChunkPos>) {
            if completed_sources.contains(&source) {
                return (LifecycleFeatureResult::default(), Vec::new());
            }
            (
                self.feature_result_for_target(target, source, overrides, resident),
                vec![source],
            )
        }
    }

    impl LifecycleWorldgenSource for CrossTargetSpillSource {
        type ReplayContext = ();

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            _source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            LifecycleFeatureResult::default()
        }

        fn feature_result_for_target(
            &self,
            target: ChunkPos,
            source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            let spills = (target == (2, 0) && source == (2, 0))
                .then(|| LifecycleSpill {
                    source,
                    position: (16, 0, 0),
                    state: sid("minecraft:diamond_block"),
                    transient: false,
                })
                .into_iter()
                .collect();
            LifecycleFeatureResult {
                spills,
                block_entities: Vec::new(),
                structure_blocks: StructureBlocks::default(),
                end_gateways: Vec::new(),
            }
        }
    }

    impl LifecycleWorldgenSource for RegionAdmissionSource {
        type ReplayContext = ();

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
            self.shaped_calls.fetch_add(1, Ordering::Relaxed);
            let mut column = ChunkColumn::new(0, 1);
            let state = if (cx + cz) & 1 == 0 {
                "minecraft:stone"
            } else {
                "minecraft:dirt"
            };
            column.set_block_id(0, 0, 0, sid(state));
            column
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            _source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            LifecycleFeatureResult::default()
        }
    }

    fn column_digest(column: &ChunkColumn) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(column.min_y.to_le_bytes());
        digest.update(column.height.to_le_bytes());
        for id in column.palette() {
            digest.update(id.raw().to_le_bytes());
        }
        for section in 0..column.section_count() {
            column.for_each_section_palette_index(section, |cell, palette_id| {
                digest.update((cell as u16).to_le_bytes());
                digest.update(palette_id.to_le_bytes());
            });
        }
        digest.finalize().into()
    }

    impl LifecycleWorldgenSource for StateOnlySpillSource {
        type ReplayContext = ();

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(-64, 384)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            LifecycleFeatureResult {
                spills: (source == (0, 0)).then(|| LifecycleSpill {
                    source,
                    position: (4, 14, 1),
                    state: sid("minecraft:spawner"),
                    transient: false,
                }).into_iter().collect(),
                block_entities: Vec::new(),
                structure_blocks: StructureBlocks::default(),
                end_gateways: Vec::new(),
            }
        }
    }

    impl LifecycleWorldgenSource for GeneratedEntitySpillSource {
        type ReplayContext = ();

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 8)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            if source != (0, 0) {
                return LifecycleFeatureResult::default();
            }
            LifecycleFeatureResult {
                spills: vec![LifecycleSpill {
                    source,
                    position: (16, 4, 0),
                    state: sid("minecraft:spawner"),
                    transient: false,
                }],
                block_entities: vec![GeneratedBlockEntity::DungeonSpawner {
                    x: 16,
                    y: 4,
                    z: 0,
                    entity_type: lodestone_data::entity_type::EntityType::Zombie.into(),
                }],
                structure_blocks: StructureBlocks::default(),
                end_gateways: Vec::new(),
            }
        }
    }

    impl LifecycleWorldgenSource for HeightmapLifecycleSource {
        type ReplayContext = ();

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            let mut column = ChunkColumn::new(0, 8);
            column.set_block_id(0, 0, 0, sid("minecraft:stone"));
            column
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            let mut maps = [[0u16; 256]; 3];
            for map in &mut maps {
                map[0] = 1;
            }
            Some(maps)
        }

        fn feature_result(
            &self,
            source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
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
                    state: sid("minecraft:stone"),
                    transient: false,
                }],
                block_entities: Vec::new(),
                structure_blocks: StructureBlocks::default(),
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
    fn borrowed_source_delegates_lifecycle_boundaries() {
        let source = HeightmapLifecycleSource;
        let mut materializer = LifecycleMaterializer::new(&source);
        materializer.admit((0, 0));
        materializer.complete((0, 0), LifecycleCompletion::Features, 0);

        assert_eq!(
            materializer.resident_stage((0, 0)),
            Some(LifecycleResidentStage::Features),
            "an immutable source borrow must drive the same lifecycle transition"
        );
        assert_eq!(world_surface(materializer.resident_column((0, 0)).unwrap()), 1);
    }

    fn later_target_state(source: TargetLocalReadSource, local_x: i32) -> StateId {
        let local_marker = source.local_marker;
        let top_layer_marker = source.top_layer_marker;
        let mut materializer = LifecycleMaterializer::new(source);
        materializer.admit((0, 0));
        materializer.admit((1, 0));

        materializer.begin_target((0, 0));
        materializer.complete_target_features_with_residents((0, 0), 0, &[]);
        materializer.finish_target((0, 0));
        if top_layer_marker {
            assert_eq!(
                materializer.resident_column((0, 0)).unwrap().block_state_id(15, 0, 0),
                sid("minecraft:snow[layers=1]"),
                "the direct target column owns its local top-layer write",
            );
        } else if local_marker {
            assert_eq!(
                materializer.resident_column((0, 0)).unwrap().block_state_id(15, 0, 0),
                sid("minecraft:stone"),
                "the direct target column owns its local final FEATURES write",
            );
        }
        if local_marker || top_layer_marker {
            assert!(
                materializer.temporary_spills.is_empty(),
                "target-local writes must not enter the cross-target spill ledger",
            );
        }

        materializer.begin_target((1, 0));
        materializer.complete_target_features_with_residents((1, 0), 1, &[]);
        materializer.finish_target((1, 0));
        materializer
            .resident_column((1, 0))
            .expect("later target was admitted")
            .block_state_id(local_x, 0, 0)
    }

    #[test]
    fn direct_target_local_features_reach_later_neighbour_without_spill_ledger() {
        assert_eq!(
            later_target_state(
                TargetLocalReadSource {
                    local_marker: true,
                    top_layer_marker: false,
                    outward_marker: false,
                },
                0,
            ),
            sid("minecraft:diamond_block"),
            "a local east-edge FEATURES write must seed the later target read view",
        );
        assert_eq!(
            later_target_state(
                TargetLocalReadSource {
                    local_marker: false,
                    top_layer_marker: false,
                    outward_marker: false,
                },
                0,
            ),
            sid("minecraft:air"),
            "the no-marker control must remain distinguishable",
        );
        assert_eq!(
            later_target_state(
                TargetLocalReadSource {
                    local_marker: false,
                    top_layer_marker: false,
                    outward_marker: true,
                },
                1,
            ),
            sid("minecraft:gold_block"),
            "an actual cross-column FEATURES spill must remain observable",
        );
    }

    #[test]
    fn direct_target_top_layer_state_reaches_later_neighbour_without_marker_control() {
        assert_eq!(
            later_target_state(
                TargetLocalReadSource {
                    local_marker: false,
                    top_layer_marker: true,
                    outward_marker: false,
                },
                0,
            ),
            sid("minecraft:diamond_block"),
            "a target-local top-layer state must seed the later target read view",
        );
        assert_eq!(
            later_target_state(
                TargetLocalReadSource {
                    local_marker: false,
                    top_layer_marker: false,
                    outward_marker: false,
                },
                0,
            ),
            sid("minecraft:air"),
            "the top-layer no-marker control must remain distinguishable",
        );
    }

    #[test]
    fn heightmaps_prime_at_own_features_entry_and_track_later_neighbour_writes() {
        let mut materializer = LifecycleMaterializer::new(HeightmapLifecycleSource);
        materializer.admit((0, 0));
        materializer.admit((1, 0));

        materializer.complete((0, 0), LifecycleCompletion::Features, 0);
        assert_eq!(world_surface(materializer.resident_column((0, 0)).unwrap()), 1);
        assert!(
            materializer.resident_column((1, 0)).unwrap().client_heightmaps().is_some(),
            "a cross-chunk write must instantiate the destination maps"
        );
        assert_eq!(
            materializer.resident_stage((1, 0)),
            Some(LifecycleResidentStage::Carvers),
            "destination map instantiation must not advance its lifecycle stage"
        );
        assert_eq!(world_surface(materializer.resident_column((1, 0)).unwrap()), 5);

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
    fn reversed_completion_keeps_cross_chunk_heightmaps_incremental() {
        let mut materializer = LifecycleMaterializer::new(HeightmapLifecycleSource);
        materializer.admit((0, 0));
        materializer.admit((1, 0));

        // The west write arrives while (0, 0) is still CARVERS. It primes
        // that destination at y=6 and must leave its retained map live.
        materializer.complete((1, 0), LifecycleCompletion::Features, 0);
        assert_eq!(
            materializer.resident_stage((0, 0)),
            Some(LifecycleResidentStage::Carvers)
        );
        assert_eq!(world_surface(materializer.resident_column((0, 0)).unwrap()), 7);

        // Entering FEATURES later preserves the map and applies the east
        // write incrementally; a second full scan would erase the earlier y=6
        // result in this control.
        materializer.complete((0, 0), LifecycleCompletion::Features, 1);
        assert_eq!(
            materializer.resident_stage((0, 0)),
            Some(LifecycleResidentStage::Features)
        );
        assert_eq!(world_surface(materializer.resident_column((0, 0)).unwrap()), 7);
        assert_eq!(world_surface(materializer.resident_column((1, 0)).unwrap()), 5);
    }

    #[test]
    fn end_cross_chunk_spill_materializes_carvers_target_maps() {
        let target = (280, 78);
        let source_chunk = (280, 77);
        let source = crate::end_chunk_source(42);
        let mut materializer = LifecycleMaterializer::new(source);
        for x in 278..=282 {
            for z in 76..=80 {
                materializer.admit((x, z));
            }
        }

        let mut observed = Vec::new();
        materializer.complete_observing(
            source_chunk,
            LifecycleCompletion::Features,
            0,
            |spill| observed.push(spill.clone()),
        );
        let target_spills = observed
            .iter()
            .filter(|spill| {
                (spill.position.0.div_euclid(16), spill.position.2.div_euclid(16)) == target
            })
            .collect::<Vec<_>>();
        assert_eq!(
            target_spills.iter().map(|spill| spill.position.1).collect::<Vec<_>>(),
            (63..=67).collect::<Vec<_>>(),
            "authenticated source must retain all five target writes"
        );
        assert_eq!(
            materializer.resident_stage(target),
            Some(LifecycleResidentStage::Carvers),
            "destination remains CARVERS after a neighbour's write"
        );
        let maps = materializer
            .resident_column(target)
            .expect("admitted target")
            .client_heightmaps()
            .expect("cross-chunk write must materialize target maps");
        let actual = (
            maps.get(1).unwrap().get(2, 0),
            maps.get(4).unwrap().get(2, 0),
            maps.get(5).unwrap().get(2, 0),
        );
        assert_eq!(actual, (68, 58, 58), "heightmaps use the encoded top+1 convention");
    }

    fn end_oracle_transition(
        source: &crate::EndChunkSource,
        resident: ChunkPos,
        stage: LifecycleResidentStage,
        focus: [u16; 3],
    ) -> LifecycleResidentTransition {
        let mut maps = *source.generator().column_shaped(resident.0, resident.1).client_heightmaps();
        for (index, value) in focus.into_iter().enumerate() {
            maps[index][2] = value;
        }
        LifecycleResidentTransition {
            resident,
            stage,
            client_heightmaps: Some(maps),
        }
    }

    fn end_map_focus(
        materializer: &LifecycleMaterializer<crate::EndChunkSource>,
        target: ChunkPos,
    ) -> (u32, u32, u32) {
        let maps = materializer
            .resident_column(target)
            .expect("admitted End target")
            .client_heightmaps()
            .expect("authenticated End map seed");
        (
            maps.get(1).expect("WORLD_SURFACE map").get(2, 0),
            maps.get(4).expect("MOTION_BLOCKING map").get(2, 0),
            maps.get(5).expect("MOTION_BLOCKING_NO_LEAVES map").get(2, 0),
        )
    }

    #[test]
    fn end_authenticated_canonical_five_by_five_map_transition_is_literal() {
        // These cells are externally captured raw-map values (+1 encoded for
        // the retained Rust representation), not values derived from the
        // final block field. The focus-band oracle reported no Y=57 write.
        let target = (280, 78);
        let source_chunk = crate::end_chunk_source(42);
        let carvers_seed = end_oracle_transition(
            &source_chunk,
            target,
            LifecycleResidentStage::Carvers,
            [58, 58, 58],
        );
        let features_seed = end_oracle_transition(
            &source_chunk,
            target,
            LifecycleResidentStage::Features,
            [68, 56, 56],
        );
        let mut materializer = LifecycleMaterializer::new(source_chunk);
        for z in 76..=80 {
            for x in 278..=282 {
                materializer.admit((x, z));
            }
        }
        let order = (76..=80)
            .flat_map(|z| (278..=282).map(move |x| (x, z)))
            .collect::<Vec<_>>();
        for (sequence, source) in order.into_iter().enumerate() {
            let transitions = match source {
                (280, 76) => vec![carvers_seed],
                (280, 78) => vec![features_seed],
                _ => Vec::new(),
            };
            materializer.complete_observing_with_residents(
                source,
                LifecycleCompletion::Features,
                sequence as u64,
                &transitions,
                |_| {},
            );
        }
        assert_eq!(
            materializer.resident_stage(target),
            Some(LifecycleResidentStage::Features)
        );
        assert_eq!(end_map_focus(&materializer, target), (68, 56, 56));
    }

    #[test]
    fn end_authenticated_reversed_five_by_five_is_negative_control() {
        let target = (280, 78);
        let source_chunk = crate::end_chunk_source(42);
        let features_seed = end_oracle_transition(
            &source_chunk,
            target,
            LifecycleResidentStage::Features,
            [58, 58, 58],
        );
        let mut materializer = LifecycleMaterializer::new(source_chunk);
        for z in 76..=80 {
            for x in 278..=282 {
                materializer.admit((x, z));
            }
        }
        let order = (76..=80)
            .rev()
            .flat_map(|z| (278..=282).rev().map(move |x| (x, z)))
            .collect::<Vec<_>>();
        for (sequence, source) in order.into_iter().enumerate() {
            let transitions = if source == target {
                vec![features_seed]
            } else {
                Vec::new()
            };
            materializer.complete_observing_with_residents(
                source,
                LifecycleCompletion::Features,
                sequence as u64,
                &transitions,
                |_| {},
            );
        }
        assert_eq!(end_map_focus(&materializer, target), (68, 58, 58));
    }

    #[test]
    fn target_scoped_completion_replays_a_source_after_cross_target_rollback() {
        let mut materializer = LifecycleMaterializer::new(TargetReplaySource);
        materializer.admit((0, 0));
        materializer.admit((1, 0));
        materializer.admit((2, 0));

        materializer.begin_target((0, 0));
        materializer.complete_for_target(
            (0, 0),
            (0, 0),
            LifecycleCompletion::Features,
            0,
        );
        materializer.complete_for_target(
            (0, 0),
            (1, 0),
            LifecycleCompletion::Features,
            1,
        );
        materializer.finish_target((0, 0));
        assert_eq!(
            materializer.resident_column((1, 0)).unwrap().block_state_id(0, 0, 0),
            sid("minecraft:stone"),
            "a write into a source that entered FEATURES must be retained",
        );
        assert_eq!(
            materializer.resident_column((2, 0)).unwrap().block_state_id(0, 0, 0),
            sid("minecraft:air"),
            "a write into a later source must not leak into the next packet target",
        );

        materializer.begin_target((1, 0));
        materializer.complete_for_target(
            (1, 0),
            (0, 0),
            LifecycleCompletion::Features,
            2,
        );
        materializer.finish_target((1, 0));
        assert_eq!(
            materializer.resident_column((1, 0)).unwrap().block_state_id(0, 0, 0),
            sid("minecraft:stone"),
            "the later target must replay the source whose write it owns",
        );
    }

    #[test]
    fn later_target_spill_invalidates_an_early_packet_snapshot() {
        let mut materializer = LifecycleMaterializer::new(CrossTargetSpillSource);
        materializer.admit((1, 0));
        materializer.admit((2, 0));
        materializer.declare_mutable_targets([(1, 0), (2, 0)]);

        materializer.begin_target((1, 0));
        materializer.complete_for_target((1, 0), (1, 0), LifecycleCompletion::Features, 0);
        materializer.finish_target((1, 0));
        let early = materializer.snapshot_for_packet((1, 0));
        assert_eq!(early.block_state_id(0, 0, 0), sid("minecraft:air"));

        materializer.begin_target((2, 0));
        materializer.complete_for_target((2, 0), (2, 0), LifecycleCompletion::Features, 1);
        materializer.finish_target((2, 0));
        let late = materializer.snapshot_for_packet((1, 0));
        assert_eq!(late.block_state_id(0, 0, 0), sid("minecraft:diamond_block"));
        assert_ne!(column_digest(&early), column_digest(&late));
    }

    #[test]
    fn state_only_spill_does_not_invent_a_lifecycle_block_entity() {
        let mut materializer = LifecycleMaterializer::new(StateOnlySpillSource);
        materializer.admit((0, 0));
        materializer.complete((0, 0), LifecycleCompletion::Features, 0);

        let column = materializer.resident_column((0, 0)).expect("admitted source column");
        assert_eq!(column.block_state_id(4, 14, 1), sid("minecraft:spawner"));
        assert!(column.block_entities().is_empty());
    }

    #[test]
    fn future_target_entity_spill_is_deferred_but_current_target_is_materialized() {
        let mut future = LifecycleMaterializer::new(GeneratedEntitySpillSource);
        future.admit((0, 0));
        future.admit((1, 0));
        future.complete_for_target((0, 0), (0, 0), LifecycleCompletion::Features, 0);
        future.finish_target((0, 0));
        assert_eq!(future.resident_column((1, 0)).unwrap().block_state_id(0, 4, 0), sid("minecraft:air"));
        assert!(
            future.resident_column((1, 0)).unwrap().block_entities().is_empty(),
            "a future dependency must not expose a deferred generated entity in its packet state",
        );

        let mut current = LifecycleMaterializer::new(GeneratedEntitySpillSource);
        current.admit((0, 0));
        current.admit((1, 0));
        current.complete_for_target((1, 0), (0, 0), LifecycleCompletion::Features, 0);
        current.finish_target((1, 0));
        let column = current.resident_column((1, 0)).unwrap();
        assert_eq!(column.block_state_id(0, 4, 0), sid("minecraft:spawner"));
        assert_eq!(column.block_entities().len(), 1);
    }

    impl LifecycleWorldgenSource for SpillSource {
        type ReplayContext = ();

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            source: ChunkPos,
            overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            if source == (0, 1) {
                self.expected_override.set(
                    overrides
                        .get(&(16, 0, 0))
                        .is_some_and(|&state| state == sid("minecraft:stone")),
                );
            }
            let spills = if source == (0, 0) && !self.post_features {
                vec![LifecycleSpill {
                    source,
                    position: (16, 0, 0),
                    state: sid("minecraft:stone"),
                    transient: false,
                }]
            } else {
                Vec::new()
            };
            LifecycleFeatureResult {
                spills,
                block_entities: Vec::new(),
                structure_blocks: StructureBlocks::default(),
                end_gateways: Vec::new(),
            }
        }

        fn post_features_spills(
            &self,
            source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
        ) -> Vec<LifecycleSpill> {
            if source == (0, 0) && self.post_features {
                vec![LifecycleSpill {
                    source,
                    position: (16, 0, 0),
                    state: sid("minecraft:stone"),
                    transient: false,
                }]
            } else {
                Vec::new()
            }
        }
    }

    impl LifecycleWorldgenSource for CountingSource {
        type ReplayContext = ();

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            _source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
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
        type ReplayContext = ();

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn {
            let mut column = ChunkColumn::new(0, 1);
            let state = if (cx + cz) & 1 == 0 {
                "minecraft:stone"
            } else {
                "minecraft:dirt"
            };
            column.set_block_id(0, 0, 0, sid(state));
            column
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            _source: ChunkPos,
            _overrides: &BTreeMap<AbsoluteCell, StateId>,
            _resident: &BTreeMap<ChunkPos, ChunkColumn>,
        ) -> LifecycleFeatureResult {
            LifecycleFeatureResult::default()
        }
    }

    struct ReorderSource;

    impl LifecycleWorldgenSource for ReorderSource {
        type ReplayContext = ();

        fn lifecycle_replay_context(&self, _target: ChunkPos) -> Arc<Self::ReplayContext> {
            Arc::new(())
        }

        fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 1)
        }

        fn lifecycle_client_heightmaps(
            &self,
            _cx: i32,
            _cz: i32,
        ) -> Option<LifecycleClientHeightmaps> {
            test_heightmaps()
        }

        fn feature_result(
            &self,
            source: ChunkPos,
            overrides: &BTreeMap<AbsoluteCell, StateId>,
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
                    state: sid(state),
                    transient: false,
                }],
                block_entities: Vec::new(),
                structure_blocks: StructureBlocks::default(),
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
                serial.resident_column(chunk).unwrap().block_state_id(0, 0, 0),
                parallel.resident_column(chunk).unwrap().block_state_id(0, 0, 0),
                "parallel admission changed shaped output at {chunk:?}",
            );
        }
    }

    #[test]
    fn shared_region_admission_reuses_overlap_without_changing_bytes() {
        let first = (-1..=1)
            .flat_map(|z| (-1..=1).map(move |x| (x, z)))
            .collect::<Vec<_>>();
        let second = (0..=2)
            .flat_map(|z| (-1..=1).map(move |x| (x, z)))
            .collect::<Vec<_>>();
        let union = first
            .iter()
            .chain(&second)
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let shaped_calls = Arc::new(AtomicUsize::new(0));
        let source = RegionAdmissionSource {
            shaped_calls: Arc::clone(&shaped_calls),
        };
        let mut independent_first = LifecycleMaterializer::new(source.clone());
        independent_first.admit_region_with(&first, &PersistentWorldgenExecutor);
        let first_digest = column_digest(independent_first.resident_column((0, 0)).unwrap());
        let mut independent_second = LifecycleMaterializer::new(source.clone());
        independent_second.admit_region_with(&second, &PersistentWorldgenExecutor);
        let second_digest = column_digest(independent_second.resident_column((1, 0)).unwrap());
        let independent_calls = shaped_calls.load(Ordering::Relaxed);

        let mut shared = LifecycleMaterializer::new(source);
        assert_eq!(
            shared.admit_region_with(&union, &PersistentWorldgenExecutor),
            union.len(),
            "the first region admission should compute every union resident",
        );
        let shared_first_digest = column_digest(shared.resident_column((0, 0)).unwrap());
        let shared_second_digest = column_digest(shared.resident_column((1, 0)).unwrap());

        assert_eq!(independent_calls, 18, "the negative control must regenerate the overlap");
        assert_eq!(
            shaped_calls.load(Ordering::Relaxed),
            30,
            "the shared region must add only the twelve union residents",
        );
        assert_ne!(independent_calls, union.len(), "separate target sessions must be distinguishable");
        assert_eq!(first_digest, shared_first_digest, "shared admission changed the first bytes");
        assert_eq!(second_digest, shared_second_digest, "shared admission changed the second bytes");
    }

    #[test]
    fn shared_prefixes_keep_one_copy_until_the_mutable_boundary() {
        let mut materializer = LifecycleMaterializer::new(AdmissionSource);
        materializer.admit((0, 0));

        let first = materializer
            .shared_resident_prefix((0, 0), ColumnStage::Fill, |_| [0; 32])
            .expect("admitted column must be shareable");
        let second = materializer
            .shared_resident_prefix((0, 0), ColumnStage::Fill, |_| [1; 32])
            .expect("shared prefix must remain available");
        assert!(Arc::ptr_eq(&first.0, &second.0));
        assert_eq!(first.1, second.1, "cached metadata must not recompute");

        materializer.begin_target((0, 0));
        materializer.finish_target((0, 0));
        let after_commit = materializer
            .shared_resident_prefix((0, 0), ColumnStage::Fill, |_| [2; 32])
            .expect("admitted column must remain available after commit");
        assert!(!Arc::ptr_eq(&first.0, &after_commit.0));
        assert_eq!(after_commit.1, [2; 32]);
    }

    #[test]
    fn overworld_overrides_are_bounded_to_the_dispatch_window() {
        let generator = crate::overworld_generator(42);
        let target = (-2, 3);
        let origin_x = target.0 * 16;
        let origin_z = target.1 * 16;
        let min_x = origin_x + lodestone_worldgen::feature::REGION_MIN
            - lodestone_worldgen::feature::vegetation::GEODE_PADDING;
        let max_x = origin_x + lodestone_worldgen::feature::REGION_MAX
            + lodestone_worldgen::feature::vegetation::GEODE_PADDING;
        let min_z = origin_z + lodestone_worldgen::feature::REGION_MIN
            - lodestone_worldgen::feature::vegetation::GEODE_PADDING;
        let max_z = origin_z + lodestone_worldgen::feature::REGION_MAX
            + lodestone_worldgen::feature::vegetation::GEODE_PADDING;
        let mut overrides = BTreeMap::new();
        overrides.insert(
            (min_x, generator.min_y(), min_z),
            sid("minecraft:stone"),
        );
        overrides.insert(
            (
                max_x - 1,
                generator.min_y() + generator.height() - 1,
                max_z - 1,
            ),
            sid("minecraft:dirt"),
        );
        overrides.insert((min_x - 1, 0, min_z), sid("minecraft:granite"));
        overrides.insert((min_x, 0, max_z), sid("minecraft:andesite"));
        overrides.insert((min_x, generator.min_y() - 1, min_z), sid("minecraft:deepslate"));

        let bounded = overworld_override_vec(&overrides, target, &generator);
        assert_eq!(bounded.len(), 2);
        assert_eq!(bounded[0].3, sid("minecraft:stone"));
        assert_eq!(bounded[1].3, sid("minecraft:dirt"));
        assert_eq!(override_vec(&overrides).len(), 5);
    }

    #[test]
    fn bounded_overworld_overrides_preserve_direct_output() {
        let generator = crate::overworld_generator(42);
        let target = (0, 0);
        let context = generator.lifecycle_replay_context(target.0, target.1);
        let mut overrides = BTreeMap::new();
        overrides.insert((0, 63, 0), sid("minecraft:cobblestone"));
        overrides.insert((10_000, 63, 10_000), sid("minecraft:diamond_block"));
        let full = override_vec(&overrides);
        let bounded = overworld_override_vec(&overrides, target, &generator);

        let expected = generator.direct_source_decoration_with_context(
            target.0,
            target.1,
            &full,
            &context,
        );
        let actual = generator.direct_source_decoration_with_context(
            target.0,
            target.1,
            &bounded,
            &context,
        );
        assert_eq!(
            column_digest(&ChunkColumn::from_generated(expected.column)),
            column_digest(&ChunkColumn::from_generated(actual.column)),
        );
        assert_eq!(actual.spills, expected.spills);
        assert_eq!(actual.local_features, expected.local_features);
    }

    #[test]
    fn generated_shaped_admission_materializes_once_at_the_server_boundary() {
        crate::chunk::reset_generated_materializations();
        reset_generated_resident_unwraps();
        let source = OverworldChunkSource::new(crate::overworld_generator(42));
        let mut materializer = LifecycleMaterializer::new(source);

        assert_eq!(
            materializer.admit_region_with(&[(0, 0)], &PersistentWorldgenExecutor),
            1
        );
        assert_eq!(
            crate::chunk::generated_materializations(),
            0,
            "typed shaped admission must not construct a ChunkColumn"
        );
        assert!(materializer.resident_column((0, 0)).is_none());
        assert!(materializer.generated_resident_column((0, 0)).is_some());
        let generated_state = materializer
            .generated_resident_column((0, 0))
            .expect("typed shaped product remains readable")
            .block_state_id(0, 0, 0);
        assert_eq!(
            materializer.resident_block_state((0, 0), 0, 0, 0),
            Some(generated_state),
            "read-only block access must not force server materialization",
        );

        materializer.materialize_resident_for_test((0, 0));
        materializer.materialize_resident_for_test((0, 0));
        assert_eq!(
            crate::chunk::generated_materializations(),
            1,
            "repeated consumers must share one materialization per coordinate"
        );
        assert_eq!(
            generated_resident_unwraps(),
            1,
            "the sole generated handle should be consumed without cloning"
        );
        assert!(materializer.resident_column((0, 0)).is_some());
    }

    #[test]
    fn shared_generated_materialization_matches_unique_consumption() {
        let target = (0, 0);

        reset_generated_resident_unwraps();
        let mut unique = LifecycleMaterializer::new(OverworldChunkSource::new(
            crate::overworld_generator(42),
        ));
        unique.admit(target);
        unique.materialize_resident_for_test(target);
        let unique_digest = column_digest(unique.resident_column(target).unwrap());
        assert_eq!(generated_resident_unwraps(), 1);

        reset_generated_resident_unwraps();
        let mut shared = LifecycleMaterializer::new(OverworldChunkSource::new(
            crate::overworld_generator(42),
        ));
        shared.admit(target);
        let retained_handle = shared
            .generated_resident_handle(target)
            .expect("admission must retain a generated handle");
        shared.materialize_resident_for_test(target);
        let shared_digest = column_digest(shared.resident_column(target).unwrap());

        assert_eq!(generated_resident_unwraps(), 0);
        assert_eq!(unique_digest, shared_digest);
        drop(retained_handle);
    }

    #[test]
    fn authenticated_generated_target_uses_direct_output_before_materialization() {
        let target = (0, 0);
        let expected = OverworldChunkSource::new(crate::overworld_generator(42)).column(0, 0);
        crate::chunk::reset_generated_materializations();
        let source = OverworldChunkSource::new(crate::overworld_generator(42));
        let context = source.generator().lifecycle_replay_context(0, 0);
        let mut materializer = LifecycleMaterializer::new(source);
        materializer.admit(target);
        materializer.mark_authenticated_prefix(target, [4; 32]);
        materializer.install_lifecycle_replay_context(target, context);
        materializer.declare_mutable_targets([target]);

        materializer.complete_target_features_observing(target, 0, |_| {});
        assert_eq!(crate::chunk::generated_materializations(), 0);
        assert!(materializer.has_direct_target_output());
        materializer.finish_target(target);

        let actual = materializer.snapshot_for_packet(target);
        assert_eq!(column_digest(&actual), column_digest(&expected));
        assert_eq!(actual.client_heightmaps_raw(), expected.client_heightmaps_raw());
        assert_eq!(actual.structure_starts().len(), expected.structure_starts().len());
        assert_eq!(actual.block_entities().len(), expected.block_entities().len());
    }

    #[test]
    fn sparse_write_reaches_a_later_direct_target_without_materializing_it() {
        crate::chunk::reset_generated_materializations();
        let source = SparseBeforeDirectSource;
        let generator = crate::overworld_generator(42);
        let mut materializer = LifecycleMaterializer::new(source);
        for target in [(0, 0), (1, 0)] {
            materializer.admit_generated_existing(
                target,
                Arc::new(generator.column_shaped(target.0, target.1)),
            );
            materializer.install_lifecycle_replay_context(target, Arc::new(()));
        }
        materializer.mark_authenticated_prefix((1, 0), [7; 32]);
        materializer.declare_mutable_targets([(0, 0), (1, 0)]);
        materializer.declare_sparse_padding_targets([(0, 0)]);

        materializer.complete_target_features_sparse_observing((0, 0), 0, |_| {});
        materializer.finish_target((0, 0));
        assert_eq!(
            crate::chunk::generated_materializations(),
            0,
            "the future direct target stays compact after a sparse spill",
        );

        materializer.complete_target_features_observing((1, 0), 1, |_| {});
        assert!(materializer.has_direct_target_output());
        assert_eq!(
            materializer
                .resident_column((1, 0))
                .expect("direct completion installs its output")
                .block_state_id(0, 0, 0),
            sid("minecraft:dirt"),
            "the direct read view retains the earlier sparse write",
        );
        assert_eq!(
            crate::chunk::generated_materializations(),
            0,
            "direct output must not materialize the shaped prefix",
        );
    }

    #[test]
    fn unauthenticated_generated_target_keeps_the_materialization_boundary() {
        crate::chunk::reset_generated_materializations();
        let target = (0, 0);
        let source = OverworldChunkSource::new(crate::overworld_generator(42));
        let context = source.generator().lifecycle_replay_context(0, 0);
        let mut materializer = LifecycleMaterializer::new(source);
        materializer.admit(target);
        materializer.install_lifecycle_replay_context(target, context);
        materializer.declare_mutable_targets([target]);

        materializer.complete_target_features_observing(target, 0, |_| {});
        assert_eq!(crate::chunk::generated_materializations(), 1);
        assert!(!materializer.has_direct_target_output());
        materializer.finish_target(target);
        assert!(materializer.resident_column(target).is_some());
    }

    #[test]
    fn generated_batch_preserves_hydrated_precedence_per_coordinate() {
        crate::chunk::reset_generated_materializations();
        let source = OverworldChunkSource::new(crate::overworld_generator(42));
        let hydrated = ChunkColumn::new(-64, 384);
        assert!(source.retain_generation_input(1, 0, &hydrated));
        let mut materializer = LifecycleMaterializer::new(source);

        assert_eq!(
            materializer.admit_region_with(&[(0, 0), (1, 0)], &PersistentWorldgenExecutor),
            2
        );
        assert!(
            materializer.generated_resident_column((0, 0)).is_some(),
            "the pristine coordinate retains its compact product"
        );
        assert_eq!(
            materializer
                .resident_column((1, 0))
                .expect("hydrated coordinate must use its server carrier")
                .block_state_id(0, -64, 0),
            sid("minecraft:air")
        );
        assert_eq!(
            crate::chunk::generated_materializations(),
            0,
            "batch fallback must not materialize the pristine neighbour"
        );
    }

    #[test]
    fn target_owned_features_run_once_per_target() {
        let target_calls = Arc::new(AtomicUsize::new(0));
        let source_calls = Arc::new(AtomicUsize::new(0));
        let source = DispatchCountingSource {
            dispatch: LifecycleFeatureDispatch::TargetOwned,
            target_calls: Arc::clone(&target_calls),
            source_calls: Arc::clone(&source_calls),
        };
        let targets = [(0, 0), (1, 0), (2, 0), (3, 0)];
        let mut materializer = LifecycleMaterializer::new(source);
        for target in targets {
            materializer.admit(target);
        }
        materializer.declare_mutable_targets(targets);
        for (sequence, target) in targets.into_iter().enumerate() {
            materializer.complete_target_features_observing(target, sequence as u64, |_| {});
            materializer.finish_target(target);
        }
        assert_eq!(target_calls.load(Ordering::Relaxed), targets.len());
        assert_eq!(source_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[should_panic(expected = "target-owned FEATURES replay requires exactly one target event")]
    fn target_owned_trace_rejects_neighbor_origin_bodies() {
        let target = (0, 0);
        let admissions = (-1..=1)
            .flat_map(|z| (-1..=1).map(move |x| (x, z)))
            .collect::<Vec<_>>();
        let events = admissions
            .iter()
            .copied()
            .enumerate()
            .map(|(sequence, source)| LifecycleReplayEvent {
                source,
                stage: LifecycleCompletion::Features,
                sequence: sequence as u64,
                resident_transitions: Vec::new(),
            })
            .collect::<Vec<_>>();
        let plan = LifecycleReplayPlan::for_target(target, &admissions, &events)
            .expect("the nine-origin trace is structurally valid");
        let source = DispatchCountingSource {
            dispatch: LifecycleFeatureDispatch::TargetOwned,
            target_calls: Arc::new(AtomicUsize::new(0)),
            source_calls: Arc::new(AtomicUsize::new(0)),
        };
        let mut materializer = LifecycleMaterializer::new(source);
        materializer.replay_plan(&plan);
    }

    #[test]
    fn injected_target_context_reaches_direct_completion() {
        let direct_calls = Arc::new(AtomicUsize::new(0));
        let target = (7, -3);
        let mut materializer = LifecycleMaterializer::new(ContextDirectSource {
            direct_calls: Arc::clone(&direct_calls),
        });
        materializer.admit(target);
        materializer.install_lifecycle_replay_context(target, Arc::new(7));
        materializer.complete_target_features_observing(target, 0, |_| {});
        materializer.finish_target(target);
        assert_eq!(direct_calls.load(Ordering::Relaxed), 1);
        assert!(materializer.has_direct_target_output());
    }

    #[test]
    fn source_ordered_features_keep_the_scalar_fallback() {
        let target_calls = Arc::new(AtomicUsize::new(0));
        let source_calls = Arc::new(AtomicUsize::new(0));
        let source = DispatchCountingSource {
            dispatch: LifecycleFeatureDispatch::SourceOrdered,
            target_calls: Arc::clone(&target_calls),
            source_calls: Arc::clone(&source_calls),
        };
        let sources = (-1..=1)
            .flat_map(|z| (-1..=1).map(move |x| (x, z)))
            .collect::<Vec<_>>();
        let mut materializer = LifecycleMaterializer::new(source);
        for source in sources.iter().copied() {
            materializer.admit(source);
        }
        materializer.begin_target((0, 0));
        for (sequence, source) in sources.into_iter().enumerate() {
            materializer.complete_for_target(
                (0, 0),
                source,
                LifecycleCompletion::Features,
                sequence as u64,
            );
        }
        materializer.finish_target((0, 0));
        assert_eq!(source_calls.load(Ordering::Relaxed), 9);
        assert_eq!(target_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn target_owned_features_match_scalar_fixture_bytes() {
        let target = (7, -3);
        let target_calls = Arc::new(AtomicUsize::new(0));
        let source_calls = Arc::new(AtomicUsize::new(0));
        let mut target_owned = LifecycleMaterializer::new(DispatchCountingSource {
            dispatch: LifecycleFeatureDispatch::TargetOwned,
            target_calls: Arc::clone(&target_calls),
            source_calls: Arc::clone(&source_calls),
        });
        target_owned.admit(target);
        target_owned.complete_target_features_observing(target, 0, |_| {});
        target_owned.finish_target(target);
        let target_digest = column_digest(&target_owned.snapshot_for_packet(target));

        let mut scalar = LifecycleMaterializer::new(DispatchCountingSource {
            dispatch: LifecycleFeatureDispatch::SourceOrdered,
            target_calls: Arc::clone(&target_calls),
            source_calls: Arc::clone(&source_calls),
        });
        scalar.admit(target);
        scalar.complete(target, LifecycleCompletion::Features, 0);
        let scalar_digest = column_digest(&scalar.snapshot_for_packet(target));
        assert_eq!(target_digest, scalar_digest);
        assert_eq!(target_calls.load(Ordering::Relaxed), 1);
        assert_eq!(source_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn direct_target_output_preserves_authenticated_heightmaps() {
        let target = (0, 0);
        let expected = [[1u16; 256]; 3];
        let mut materializer = LifecycleMaterializer::new(DirectHeightmapSource);
        materializer.admit(target);
        materializer
            .resident
            .get_mut(&target)
            .expect("target is resident")
            .install_client_heightmaps_raw(expected);
        materializer.complete_target_features_observing(target, 0, |_| {});
        assert_eq!(
            materializer.snapshot_for_packet(target).client_heightmaps_raw(),
            Some(expected),
        );
    }

    #[test]
    fn direct_target_output_preserves_restored_leading_edge_write() {
        use lodestone_worldgen::stage_schedule::{Dimension, StageKey};

        let target = (0, 0);
        let mut control = LifecycleMaterializer::new(DirectHeightmapSource);
        control.admit(target);
        control.complete_target_features_observing(target, 0, |_| {});
        assert_eq!(control.snapshot_for_packet(target).block_state_id(0, 0, 0), sid("minecraft:air"));

        let mutation = ProvenanceMutation::test_block_state(
            (-1, 0),
            (-1, 0),
            StageKey::new(Dimension::Overworld, ColumnStage::Features),
            0,
            crate::worldgen_session::BlockCoordinate::new(0, 0, 0),
            1,
            sid("minecraft:stone"),
        );
        let mut materializer = LifecycleMaterializer::new(DirectHeightmapSource);
        materializer.admit(target);
        materializer.restore_committed_mutations([&mutation]);
        materializer.complete_target_features_observing(target, 0, |_| {});
        assert_eq!(
            materializer.snapshot_for_packet(target).block_state_id(0, 0, 0),
            sid("minecraft:stone"),
        );
    }

    #[test]
    fn restored_feature_mutation_remains_the_canonical_winner() {
        use lodestone_worldgen::stage_schedule::{Dimension, StageKey};

        let target = (0, 0);
        let cell = (0, 0, 0);
        let restored = ProvenanceMutation::test_block_state(
            (-1, 0),
            (-1, 0),
            StageKey::new(Dimension::Overworld, ColumnStage::Features),
            0,
            crate::worldgen_session::BlockCoordinate::new(cell.0, cell.1, cell.2),
            1,
            sid("minecraft:stone"),
        );
        let mut materializer = LifecycleMaterializer::new(DirectHeightmapSource);
        materializer.admit(target);
        materializer.restore_committed_mutations([&restored]);

        materializer.record_target_feature_winner(
            target,
            target,
            0,
            cell,
            sid("minecraft:gold_block"),
        );
        materializer
            .resident
            .get_mut(&target)
            .expect("target is resident")
            .set_block_id(0, 0, 0, sid("minecraft:gold_block"));
        materializer.apply_canonical_target_feature_winners(target);

        assert_eq!(
            materializer.snapshot_for_packet(target).block_state_id(0, 0, 0),
            sid("minecraft:stone"),
        );
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

        let canonical_state = canonical.resident_column((1, 0)).unwrap().block_state_id(0, 0, 0);
        let reordered_state = reordered.resident_column((1, 0)).unwrap().block_state_id(0, 0, 0);
        assert_eq!(canonical_state, sid("minecraft:diamond_block"));
        assert_eq!(reordered_state, sid("minecraft:stone"));
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
            resident_transitions: Vec::new(),
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
            resident_transitions: Vec::new(),
        }).collect::<Vec<_>>();
        events.swap(0, 1);
        let error = LifecycleReplayPlan::for_target((0, 0), &admissions, &events)
            .expect_err("mutated authenticated event order must fail closed");
        assert!(error.contains("order") || error.contains("disagrees"));
    }

    #[test]
    fn authenticated_write_digest_changes_for_one_block_control() {
        let mut materializer = LifecycleMaterializer::new(CountingSource {
            feature_calls: Rc::new(Cell::new(0)),
        });
        let destination = (0, 0);
        materializer
            .authenticated_prefixes
            .insert(destination, [7; 32]);
        let before = materializer
            .authenticated_prefixes
            .get(&destination)
            .copied()
            .expect("test identity was installed");
        materializer.record_authenticated_write(
            (1, 0),
            (1, 0),
            destination,
            (3, 4, 5),
            sid("minecraft:stone"),
            false,
        );
        let after = materializer
            .authenticated_prefixes
            .get(&destination)
            .copied()
            .expect("destination identity remains resident");
        assert_ne!(before, after, "one persistent block write must change its identity");
    }

    #[test]
    fn authenticated_features_seed_retains_prior_write_identity() {
        let mut materializer = LifecycleMaterializer::new(CountingSource {
            feature_calls: Rc::new(Cell::new(0)),
        });
        let destination = (0, 0);
        let generated = [5; 32];
        let initial_prefix = [7; 32];
        materializer
            .authenticated_prefixes
            .insert(destination, initial_prefix);
        materializer.record_authenticated_write(
            (1, 0),
            (1, 0),
            destination,
            (3, 4, 5),
            sid("minecraft:stone"),
            false,
        );
        let updated_prefix = materializer.authenticated_prefixes[&destination];
        assert_ne!(initial_prefix, updated_prefix);
        assert_ne!(
            authenticated_features_identity(generated, initial_prefix),
            authenticated_features_identity(generated, updated_prefix),
        );
    }

    #[test]
    fn authenticated_features_identity_tracks_later_write() {
        let mut materializer = LifecycleMaterializer::new(CountingSource {
            feature_calls: Rc::new(Cell::new(0)),
        });
        let destination = (0, 0);
        materializer
            .authenticated_stages
            .insert((destination, LifecycleCompletion::Features), [8; 32]);
        let before = materializer.authenticated_stages
            [&(destination, LifecycleCompletion::Features)];
        materializer.record_authenticated_write(
            (1, 0),
            (1, 0),
            destination,
            (3, 4, 5),
            sid("minecraft:stone"),
            false,
        );
        assert_ne!(
            before,
            materializer.authenticated_stages
                [&(destination, LifecycleCompletion::Features)],
        );
    }

    #[test]
    fn temporary_write_does_not_change_authenticated_identity() {
        let mut materializer = LifecycleMaterializer::new(CountingSource {
            feature_calls: Rc::new(Cell::new(0)),
        });
        let destination = (0, 0);
        materializer
            .authenticated_prefixes
            .insert(destination, [9; 32]);
        let before = materializer
            .authenticated_prefixes
            .get(&destination)
            .copied()
            .expect("test identity was installed");
        materializer.record_authenticated_write(
            (1, 0),
            (1, 0),
            destination,
            (3, 4, 5),
            sid("minecraft:stone"),
            true,
        );
        assert_eq!(
            materializer.authenticated_prefixes.get(&destination),
            Some(&before),
            "speculative writes must remain outside the authenticated identity",
        );
    }

}
