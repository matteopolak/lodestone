//! Request-scoped world-generation state.
//!
//! A [`GenerationSession`] is the additive boundary between a dimension
//! generator and a future production scheduler. It owns the admitted target
//! halo, retains immutable products by stage, queues mutable source work in a
//! canonical order, and produces detached packet snapshots after light has
//! settled. It does not create workers or alter the existing [`ChunkSource`]
//! entry points.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(target_arch = "wasm32")]
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use crate::chunk::{ChunkColumn, ChunkGenerationStage, ColumnLightSettlement};
use lodestone_data::block_states::StateId;
use lodestone_worldgen::hash::FastSet;
use lodestone_worldgen::stage_schedule::{
    BarrierPolicy, ChunkRequest, ColumnStage, Dimension, DimensionPipeline, END_PIPELINE,
    GenerationTarget, PipelineOptions, ResourceKey, StageDescriptor, StageFrontier, StageKey,
    StageRecord, SidecarKey, NETHER_PIPELINE, OVERWORLD_PIPELINE,
};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(target_arch = "wasm32")]
static BROWSER_WORKER_EPOCH: AtomicU32 = AtomicU32::new(0);
#[cfg(target_arch = "wasm32")]
static BROWSER_CANCEL_EPOCH: AtomicU32 = AtomicU32::new(0);

#[cfg(target_arch = "wasm32")]
pub fn register_browser_worker_epoch(epoch: u32) {
    BROWSER_CANCEL_EPOCH.store(0, Ordering::Release);
    BROWSER_WORKER_EPOCH.store(epoch, Ordering::Release);
}

#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn cancel_browser_worker_epoch(epoch: u32) -> bool {
    if BROWSER_WORKER_EPOCH.load(Ordering::Acquire) != epoch || epoch == 0 {
        return false;
    }
    BROWSER_CANCEL_EPOCH.store(epoch, Ordering::Release);
    true
}

/// A chunk coordinate in the horizontal world grid.
pub type ChunkCoordinate = (i32, i32);

/// An absolute block destination for a provenance-bearing mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockCoordinate {
    x: i32,
    y: i32,
    z: i32,
}

impl BlockCoordinate {
    #[must_use]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub const fn x(self) -> i32 {
        self.x
    }

    #[must_use]
    pub const fn y(self) -> i32 {
        self.y
    }

    #[must_use]
    pub const fn z(self) -> i32 {
        self.z
    }
}

/// The revision assigned to a committed session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionRevision(u64);

impl SessionRevision {
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Identity for one in-memory generation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(u64);

impl SessionId {
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// A cancellation token that can be shared with submitted worker jobs.
#[derive(Clone, Debug)]
pub struct RequestCancellation {
    cancelled: Arc<AtomicBool>,
}

impl RequestCancellation {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || browser_worker_cancelled()
    }
}

#[cfg(target_arch = "wasm32")]
fn browser_worker_cancelled() -> bool {
    let epoch = BROWSER_WORKER_EPOCH.load(Ordering::Acquire);
    epoch != 0 && BROWSER_CANCEL_EPOCH.load(Ordering::Acquire) == epoch
}

#[cfg(not(target_arch = "wasm32"))]
fn browser_worker_cancelled() -> bool {
    false
}

impl Default for RequestCancellation {
    fn default() -> Self {
        Self::new()
    }
}

/// Limits for one request's retained generation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionBudget {
    max_product_entries: usize,
    max_sidecar_entries: usize,
    max_mutation_entries: usize,
    max_retained_bytes: usize,
}

impl SessionBudget {
    pub const DEFAULT: Self = Self {
        max_product_entries: 4096,
        max_sidecar_entries: 2048,
        max_mutation_entries: 131_072,
        max_retained_bytes: 64 * 1024 * 1024,
    };

    #[must_use]
    pub const fn new(
        max_product_entries: usize,
        max_sidecar_entries: usize,
        max_mutation_entries: usize,
        max_retained_bytes: usize,
    ) -> Self {
        Self {
            max_product_entries,
            max_sidecar_entries,
            max_mutation_entries,
            max_retained_bytes,
        }
    }

    #[must_use]
    pub const fn max_product_entries(self) -> usize {
        self.max_product_entries
    }

    #[must_use]
    pub const fn max_sidecar_entries(self) -> usize {
        self.max_sidecar_entries
    }

    #[must_use]
    pub const fn max_mutation_entries(self) -> usize {
        self.max_mutation_entries
    }

    #[must_use]
    pub const fn max_retained_bytes(self) -> usize {
        self.max_retained_bytes
    }
}

impl Default for SessionBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Current accounting for a session's accepted and committed values.
///
/// While cancellation is set, pending worker values are no longer reported
/// as usable session state; they are reclaimed when `cancel` is called or the
/// session is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionUsage {
    product_entries: usize,
    sidecar_entries: usize,
    mutation_entries: usize,
    retained_bytes: usize,
}

impl SessionUsage {
    #[must_use]
    pub const fn product_entries(self) -> usize {
        self.product_entries
    }

    #[must_use]
    pub const fn sidecar_entries(self) -> usize {
        self.sidecar_entries
    }

    #[must_use]
    pub const fn mutation_entries(self) -> usize {
        self.mutation_entries
    }

    #[must_use]
    pub const fn retained_bytes(self) -> usize {
        self.retained_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetKind {
    Products,
    Sidecars,
    Mutations,
    RetainedBytes,
}

/// A request for one target column and its dependency halo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationRequest {
    dimension: Dimension,
    target: ChunkCoordinate,
    generation_target: GenerationTarget,
    dependency_radius: u8,
}

impl GenerationRequest {
    #[must_use]
    pub const fn new(
        dimension: Dimension,
        target: ChunkCoordinate,
        generation_target: GenerationTarget,
        dependency_radius: u8,
    ) -> Self {
        Self {
            dimension,
            target,
            generation_target,
            dependency_radius,
        }
    }

    #[must_use]
    pub const fn dimension(self) -> Dimension {
        self.dimension
    }

    #[must_use]
    pub const fn target(self) -> ChunkCoordinate {
        self.target
    }

    #[must_use]
    pub const fn generation_target(self) -> GenerationTarget {
        self.generation_target
    }

    #[must_use]
    pub const fn dependency_radius(self) -> u8 {
        self.dependency_radius
    }
}

/// The admitted coordinates held by a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaloPlan {
    coordinates: Arc<[ChunkCoordinate]>,
}

impl HaloPlan {
    fn new(request: GenerationRequest) -> Self {
        let coordinates = ChunkRequest::single(
            request.target.0,
            request.target.1,
            i32::from(request.dependency_radius),
        )
        .admission_order();
        Self {
            coordinates: Arc::from(coordinates),
        }
    }

    #[must_use]
    pub fn coordinates(&self) -> &[ChunkCoordinate] {
        &self.coordinates
    }

    #[must_use]
    pub fn contains(&self, coordinate: ChunkCoordinate) -> bool {
        self.coordinates.contains(&coordinate)
    }
}

/// The key for a retained immutable product.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProductKey {
    coordinate: ChunkCoordinate,
    stage: StageKey,
    resource: ResourceKey,
}

impl ProductKey {
    #[must_use]
    pub const fn new(
        coordinate: ChunkCoordinate,
        stage: StageKey,
        resource: ResourceKey,
    ) -> Self {
        Self {
            coordinate,
            stage,
            resource,
        }
    }

    #[must_use]
    pub const fn coordinate(self) -> ChunkCoordinate {
        self.coordinate
    }

    #[must_use]
    pub const fn stage(self) -> StageKey {
        self.stage
    }

    #[must_use]
    pub const fn resource(self) -> ResourceKey {
        self.resource
    }
}

/// A typed immutable value retained at a stage boundary.
pub struct ImmutableProduct {
    resource: ResourceKey,
    value: Arc<dyn Any + Send + Sync>,
    retained_bytes: usize,
}

impl fmt::Debug for ImmutableProduct {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableProduct")
            .field("resource", &self.resource)
            .finish_non_exhaustive()
    }
}

impl Clone for ImmutableProduct {
    fn clone(&self) -> Self {
        Self {
            resource: self.resource,
            value: Arc::clone(&self.value),
            retained_bytes: self.retained_bytes,
        }
    }
}

impl ImmutableProduct {
    #[must_use]
    pub fn new<T>(resource: ResourceKey, value: T) -> Self
    where
        T: Any + Send + Sync,
    {
        Self::new_with_retained_bytes(resource, value, size_of::<T>())
    }

    #[must_use]
    pub fn new_with_retained_bytes<T>(resource: ResourceKey, value: T, retained_bytes: usize) -> Self
    where
        T: Any + Send + Sync,
    {
        Self {
            resource,
            value: Arc::new(value),
            retained_bytes,
        }
    }

    #[must_use]
    pub fn from_arc<T>(resource: ResourceKey, value: Arc<T>, retained_bytes: usize) -> Self
    where
        T: Any + Send + Sync,
    {
        Self {
            resource,
            value,
            retained_bytes,
        }
    }

    #[must_use]
    pub const fn resource(&self) -> ResourceKey {
        self.resource
    }

    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    #[must_use]
    pub fn get<T>(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        Arc::downcast::<T>(Arc::clone(&self.value)).ok()
    }
}

/// A typed sidecar retained with a completed stage.
pub struct ImmutableSidecar {
    sidecar: SidecarKey,
    value: Arc<dyn Any + Send + Sync>,
    retained_bytes: usize,
}

impl fmt::Debug for ImmutableSidecar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImmutableSidecar")
            .field("sidecar", &self.sidecar)
            .finish_non_exhaustive()
    }
}

impl Clone for ImmutableSidecar {
    fn clone(&self) -> Self {
        Self {
            sidecar: self.sidecar,
            value: Arc::clone(&self.value),
            retained_bytes: self.retained_bytes,
        }
    }
}

impl ImmutableSidecar {
    #[must_use]
    pub fn new<T>(sidecar: SidecarKey, value: T) -> Self
    where
        T: Any + Send + Sync,
    {
        Self::new_with_retained_bytes(sidecar, value, size_of::<T>())
    }

    #[must_use]
    pub fn new_with_retained_bytes<T>(sidecar: SidecarKey, value: T, retained_bytes: usize) -> Self
    where
        T: Any + Send + Sync,
    {
        Self {
            sidecar,
            value: Arc::new(value),
            retained_bytes,
        }
    }

    #[must_use]
    pub const fn sidecar(&self) -> SidecarKey {
        self.sidecar
    }

    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    #[must_use]
    pub fn get<T>(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        Arc::downcast::<T>(Arc::clone(&self.value)).ok()
    }
}

/// One completed immutable stage, allowed to arrive before its predecessors.
#[derive(Debug, Clone)]
pub struct ImmutableStageCompletion {
    coordinate: ChunkCoordinate,
    stage: StageKey,
    input_fingerprint: [u8; 32],
    output_fingerprint: [u8; 32],
    executor_version: u32,
    products: Vec<ImmutableProduct>,
    sidecars: Vec<ImmutableSidecar>,
}

impl ImmutableStageCompletion {
    #[must_use]
    pub fn new(
        coordinate: ChunkCoordinate,
        stage: StageKey,
        input_fingerprint: [u8; 32],
        output_fingerprint: [u8; 32],
        executor_version: u32,
        products: Vec<ImmutableProduct>,
        sidecars: Vec<ImmutableSidecar>,
    ) -> Self {
        Self {
            coordinate,
            stage,
            input_fingerprint,
            output_fingerprint,
            executor_version,
            products,
            sidecars,
        }
    }

    #[must_use]
    pub const fn coordinate(&self) -> ChunkCoordinate {
        self.coordinate
    }

    #[must_use]
    pub const fn stage(&self) -> StageKey {
        self.stage
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
    pub const fn executor_version(&self) -> u32 {
        self.executor_version
    }

    #[must_use]
    pub fn products(&self) -> &[ImmutableProduct] {
        &self.products
    }

    #[must_use]
    pub fn sidecars(&self) -> &[ImmutableSidecar] {
        &self.sidecars
    }
}

/// The key attached to one mutable write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MutationProvenance {
    target: ChunkCoordinate,
    source: ChunkCoordinate,
    stage: StageKey,
    ordinal: u32,
    destination: BlockCoordinate,
    revision: SessionRevision,
}

impl MutationProvenance {
    #[must_use]
    pub const fn target(self) -> ChunkCoordinate {
        self.target
    }

    #[must_use]
    pub const fn source(self) -> ChunkCoordinate {
        self.source
    }

    #[must_use]
    pub const fn stage(self) -> StageKey {
        self.stage
    }

    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }

    #[must_use]
    pub const fn destination(self) -> BlockCoordinate {
        self.destination
    }

    #[must_use]
    pub const fn revision(self) -> SessionRevision {
        self.revision
    }
}

/// A typed mutation held by a source transaction or committed overlay.
pub struct ProvenanceMutation {
    provenance: MutationProvenance,
    value: Arc<dyn Any + Send + Sync>,
    retained_bytes: usize,
}

impl fmt::Debug for ProvenanceMutation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProvenanceMutation")
            .field("provenance", &self.provenance)
            .finish_non_exhaustive()
    }
}

impl Clone for ProvenanceMutation {
    fn clone(&self) -> Self {
        Self {
            provenance: self.provenance,
            value: Arc::clone(&self.value),
            retained_bytes: self.retained_bytes,
        }
    }
}

impl ProvenanceMutation {
    #[cfg(test)]
    pub(crate) fn test_block_state(
        target: ChunkCoordinate,
        source: ChunkCoordinate,
        stage: StageKey,
        ordinal: u32,
        destination: BlockCoordinate,
        revision: u64,
        value: StateId,
    ) -> Self {
        Self {
            provenance: MutationProvenance {
                target,
                source,
                stage,
                ordinal,
                destination,
                revision: SessionRevision(revision),
            },
            retained_bytes: size_of::<StateId>(),
            value: Arc::new(value),
        }
    }

    #[must_use]
    pub const fn provenance(&self) -> MutationProvenance {
        self.provenance
    }

    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    #[must_use]
    pub fn get<T>(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        Arc::downcast::<T>(Arc::clone(&self.value)).ok()
    }
}

/// A transaction containing private mutable writes from one source.
pub struct MutableTransaction {
    session: SessionId,
    target: ChunkCoordinate,
    source: ChunkCoordinate,
    stage: StageKey,
    source_order: u64,
    revision: SessionRevision,
    writes: Vec<ProvenanceMutation>,
    provenance_index: BTreeSet<MutationProvenance>,
}

impl fmt::Debug for MutableTransaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MutableTransaction")
            .field("target", &self.target)
            .field("source", &self.source)
            .field("stage", &self.stage)
            .field("source_order", &self.source_order)
            .field("revision", &self.revision)
            .field("writes", &self.writes.len())
            .finish()
    }
}

impl MutableTransaction {
    #[must_use]
    pub const fn source_order(&self) -> u64 {
        self.source_order
    }

    #[must_use]
    pub const fn target(&self) -> ChunkCoordinate {
        self.target
    }

    #[must_use]
    pub const fn source(&self) -> ChunkCoordinate {
        self.source
    }

    #[must_use]
    pub const fn stage(&self) -> StageKey {
        self.stage
    }

    #[must_use]
    pub const fn revision(&self) -> SessionRevision {
        self.revision
    }

    #[must_use]
    pub fn writes(&self) -> &[ProvenanceMutation] {
        &self.writes
    }

    /// Add a write without exposing the session's revision allocator.
    pub fn push<T>(
        &mut self,
        ordinal: u32,
        destination: BlockCoordinate,
        value: T,
    ) -> Result<(), SessionError>
    where
        T: Any + Send + Sync,
    {
        self.push_with_retained_bytes(ordinal, destination, value, size_of::<T>())
    }

    /// Add a write with the complete retained size charged to the session
    /// budget when its transaction is submitted.
    pub fn push_with_retained_bytes<T>(
        &mut self,
        ordinal: u32,
        destination: BlockCoordinate,
        value: T,
        retained_bytes: usize,
    ) -> Result<(), SessionError>
    where
        T: Any + Send + Sync,
    {
        let provenance = MutationProvenance {
            target: self.target,
            source: self.source,
            stage: self.stage,
            ordinal,
            destination,
            revision: self.revision,
        };
        if !self.provenance_index.insert(provenance) {
            return Err(SessionError::DuplicateMutation(provenance));
        }
        self.writes.push(ProvenanceMutation {
            provenance,
            value: Arc::new(value),
            retained_bytes,
        });
        Ok(())
    }
}

/// A read-only view over the session's current resident revision.
#[derive(Debug, Clone)]
pub struct ResidentRead {
    coordinate: ChunkCoordinate,
    revision: SessionRevision,
    products: Vec<(ProductKey, ImmutableProduct)>,
    sidecars: Vec<(SidecarProductKey, ImmutableSidecar)>,
}

impl ResidentRead {
    #[must_use]
    pub const fn coordinate(&self) -> ChunkCoordinate {
        self.coordinate
    }

    #[must_use]
    pub const fn revision(&self) -> SessionRevision {
        self.revision
    }

    #[must_use]
    pub fn product<T>(&self, resource: ResourceKey) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        self.products
            .iter()
            .rev()
            .find(|(key, _)| key.resource() == resource)
            .and_then(|(_, product)| product.get::<T>())
    }

    #[must_use]
    pub fn product_at<T>(&self, key: ProductKey) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        self.products
            .iter()
            .find(|(candidate, _)| *candidate == key)
            .and_then(|(_, product)| product.get::<T>())
    }

    #[must_use]
    pub fn sidecar<T>(&self, sidecar: SidecarKey) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        self.sidecars
            .iter()
            .rev()
            .find(|(key, _)| key.sidecar() == sidecar)
            .and_then(|(_, value)| value.get::<T>())
    }
}

/// The key for a retained sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SidecarProductKey {
    coordinate: ChunkCoordinate,
    stage: StageKey,
    sidecar: SidecarKey,
}

/// One ordered mutable source completion retained by the world-owned ledger.
///
/// The source order belongs to the target request, not to a session identity;
/// this is what lets a later request skip work that an earlier request already
/// committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct SourceCompletionRecord {
    stage: StageKey,
    source_order: u64,
    source: ChunkCoordinate,
}

impl SourceCompletionRecord {
    #[must_use]
    pub(crate) const fn new(
        stage: StageKey,
        source_order: u64,
        source: ChunkCoordinate,
    ) -> Self {
        Self {
            stage,
            source_order,
            source,
        }
    }

    #[must_use]
    pub(crate) const fn stage(self) -> StageKey {
        self.stage
    }

    #[must_use]
    pub(crate) const fn source_order(self) -> u64 {
        self.source_order
    }

    #[must_use]
    pub(crate) const fn source(self) -> ChunkCoordinate {
        self.source
    }
}

impl SidecarProductKey {
    #[must_use]
    pub const fn new(
        coordinate: ChunkCoordinate,
        stage: StageKey,
        sidecar: SidecarKey,
    ) -> Self {
        Self {
            coordinate,
            stage,
            sidecar,
        }
    }

    #[must_use]
    pub const fn sidecar(self) -> SidecarKey {
        self.sidecar
    }

    #[must_use]
    pub const fn coordinate(self) -> ChunkCoordinate {
        self.coordinate
    }

    #[must_use]
    pub const fn stage(self) -> StageKey {
        self.stage
    }
}

/// A packet neighbour retained with a detached target snapshot.
///
/// Shaped generated columns remain in their compact typed representation until
/// a packet consumer asks for a materialized [`ChunkColumn`] view. This keeps the
/// detached snapshot's radius and readiness contract while avoiding an eager
/// conversion for callers that only need the target column.
#[derive(Debug)]
pub struct PacketNeighbour {
    coordinate: ChunkCoordinate,
    column: PacketNeighbourColumn,
}

#[derive(Debug)]
enum PacketNeighbourColumn {
    Materialized(Arc<ChunkColumn>),
    Generated {
        column: Arc<lodestone_worldgen::overworld::GeneratedColumn>,
        overlay: Arc<[(i32, i32, i32, StateId)]>,
        sidecar: Option<Arc<ChunkColumn>>,
        terminal: bool,
        materialized: Arc<std::sync::OnceLock<ChunkColumn>>,
    },
}

impl Clone for PacketNeighbour {
    fn clone(&self) -> Self {
        let column = match &self.column {
            PacketNeighbourColumn::Materialized(column) => {
                PacketNeighbourColumn::Materialized(column.clone())
            }
            PacketNeighbourColumn::Generated {
                column,
                overlay,
                sidecar,
                terminal,
                materialized,
            } => {
                PacketNeighbourColumn::Generated {
                    column: Arc::clone(column),
                    overlay: Arc::clone(overlay),
                    sidecar: sidecar.as_ref().map(Arc::clone),
                    terminal: *terminal,
                    materialized: Arc::clone(materialized),
                }
            }
        };
        Self {
            coordinate: self.coordinate,
            column,
        }
    }
}

impl PacketNeighbour {
    pub(crate) fn materialized(coordinate: ChunkCoordinate, column: ChunkColumn) -> Self {
        Self::shared_materialized(coordinate, Arc::new(column))
    }

    pub(crate) fn shared_materialized(
        coordinate: ChunkCoordinate,
        column: Arc<ChunkColumn>,
    ) -> Self {
        Self {
            coordinate,
            column: PacketNeighbourColumn::Materialized(column),
        }
    }

    pub(crate) fn generated_with_id_overlay(
        coordinate: ChunkCoordinate,
        column: Arc<lodestone_worldgen::overworld::GeneratedColumn>,
        overlay: Vec<(i32, i32, i32, StateId)>,
    ) -> Self {
        Self {
            coordinate,
            column: PacketNeighbourColumn::Generated {
                column,
                overlay: Arc::from(overlay),
                sidecar: None,
                terminal: false,
                materialized: Arc::new(std::sync::OnceLock::new()),
            },
        }
    }

    pub(crate) fn generated_with_id_overlay_cached(
        coordinate: ChunkCoordinate,
        column: Arc<lodestone_worldgen::overworld::GeneratedColumn>,
        overlay: Vec<(i32, i32, i32, StateId)>,
        cache: &mut BTreeMap<ChunkCoordinate, Self>,
    ) -> Self {
        let mut neighbour = Self::generated_with_id_overlay(coordinate, column, overlay);
        if let Some(cached) = cache.get(&coordinate) {
            if let (
                PacketNeighbourColumn::Generated {
                    column: cached_column,
                    overlay: cached_overlay,
                    sidecar: cached_sidecar,
                    terminal: cached_terminal,
                    materialized: cached_materialized,
                },
                PacketNeighbourColumn::Generated {
                    column,
                    overlay,
                    sidecar,
                    terminal,
                    materialized,
                },
            ) = (&cached.column, &mut neighbour.column)
            {
                if Arc::ptr_eq(cached_column, column)
                    && cached_overlay.as_ref() == overlay.as_ref()
                    && match (cached_sidecar, sidecar) {
                        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                        (None, None) => true,
                        _ => false,
                    }
                    && cached_terminal == terminal
                {
                    *materialized = Arc::clone(cached_materialized);
                }
            }
        }
        cache.insert(coordinate, neighbour.clone());
        neighbour
    }

    pub(crate) fn materialized_with_id_overlay(
        coordinate: ChunkCoordinate,
        mut column: ChunkColumn,
        overlay: &[(i32, i32, i32, StateId)],
    ) -> Self {
        apply_packet_id_overlay(&mut column, overlay);
        Self::materialized(coordinate, column)
    }

    #[must_use]
    pub const fn coordinate(&self) -> ChunkCoordinate {
        self.coordinate
    }

    /// Borrow the compact generated product without crossing into the materialized
    /// [`ChunkColumn`] carrier. Packet encoders that only need typed terrain
    /// metadata can retain this handle; [`Self::column`] remains the explicit
    /// conversion boundary for light and wire consumers that require the
    /// mutable server representation.
    #[must_use]
    pub fn generated_column(
        &self,
    ) -> Option<Arc<lodestone_worldgen::overworld::GeneratedColumn>> {
        match &self.column {
            PacketNeighbourColumn::Materialized(_) => None,
            PacketNeighbourColumn::Generated { column, .. } => Some(Arc::clone(column)),
        }
    }

    #[must_use]
    pub fn column(&self) -> &ChunkColumn {
        match &self.column {
            PacketNeighbourColumn::Materialized(column) => column,
            PacketNeighbourColumn::Generated {
                column,
                overlay,
                sidecar,
                terminal,
                materialized,
            } => materialized.get_or_init(|| {
                materialize_generated_packet_column(
                    (**column).clone(),
                    overlay,
                    sidecar.as_deref(),
                    *terminal,
                )
            }),
        }
    }

    /// Consume this neighbour into a mutable server column.
    ///
    /// If no borrowed consumer forced conversion, the generated compact
    /// column can be moved directly when its shared handle is unique.
    #[must_use]
    pub fn into_column(self) -> ChunkColumn {
        match self.column {
            PacketNeighbourColumn::Materialized(column) => Arc::unwrap_or_clone(column),
            PacketNeighbourColumn::Generated {
                column,
                overlay,
                sidecar,
                terminal,
                materialized,
            } => {
                match Arc::try_unwrap(materialized) {
                    Ok(materialized) => {
                        if let Some(column) = materialized.into_inner() {
                            return column;
                        }
                    }
                    Err(materialized) => {
                        if let Some(column) = materialized.get() {
                            return column.clone();
                        }
                        return materialized
                            .get_or_init(|| {
                                materialize_generated_packet_column(
                                    (*column).clone(),
                                    &overlay,
                                    sidecar.as_deref(),
                                    terminal,
                                )
                            })
                            .clone();
                    }
                }
                let generated = Arc::try_unwrap(column).unwrap_or_else(|column| (*column).clone());
                materialize_generated_packet_column(
                    generated,
                    &overlay,
                    sidecar.as_deref(),
                    terminal,
                )
            }
        }
    }
}

fn materialize_generated_packet_column(
    generated: lodestone_worldgen::overworld::GeneratedColumn,
    overlay: &[(i32, i32, i32, StateId)],
    sidecar: Option<&ChunkColumn>,
    terminal: bool,
) -> ChunkColumn {
    #[cfg(test)]
    crate::chunk::record_generated_materialization();
    let mut column = ChunkColumn::from_generated(generated);
    apply_packet_id_overlay(&mut column, overlay);
    if let Some(sidecar) = sidecar {
        column.set_structures(
            sidecar.structure_starts().to_vec(),
            sidecar.structure_references().clone(),
        );
        let mut entities = column.block_entities().to_vec();
        entities.extend(sidecar.block_entities().iter().cloned());
        column.set_block_entities(entities);
    }
    if terminal {
        column.prime_client_heightmaps();
        column.mark_generation_stage(ChunkGenerationStage::Full);
    }
    column
}

fn apply_packet_id_overlay(column: &mut ChunkColumn, overlay: &[(i32, i32, i32, StateId)]) {
    column.apply_ordered_block_id_batch(overlay);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FeatureSettlementProof {
    output: ChunkCoordinate,
    min: ChunkCoordinate,
    max: ChunkCoordinate,
}

impl FeatureSettlementProof {
    #[must_use]
    pub(crate) const fn square(output: ChunkCoordinate, radius: i32) -> Self {
        Self {
            output,
            min: (output.0.saturating_sub(radius), output.1.saturating_sub(radius)),
            max: (output.0.saturating_add(radius), output.1.saturating_add(radius)),
        }
    }

    #[must_use]
    pub(crate) const fn output(self) -> ChunkCoordinate {
        self.output
    }

    #[must_use]
    pub(crate) const fn contains(self, coordinate: ChunkCoordinate) -> bool {
        coordinate.0 >= self.min.0
            && coordinate.0 <= self.max.0
            && coordinate.1 >= self.min.1
            && coordinate.1 <= self.max.1
    }

    pub(crate) fn matches_square(self, output: ChunkCoordinate, radius: i32) -> bool {
        self == Self::square(output, radius)
    }

    fn has_complete_square(self, coordinates: &BTreeSet<ChunkCoordinate>) -> bool {
        let width = i64::from(self.max.0) - i64::from(self.min.0) + 1;
        let depth = i64::from(self.max.1) - i64::from(self.min.1) + 1;
        let Some(expected) = width.checked_mul(depth).and_then(|area| usize::try_from(area).ok()) else {
            return false;
        };
        coordinates.len() == expected
            && (self.min.1..=self.max.1).all(|z| {
                (self.min.0..=self.max.0).all(|x| coordinates.contains(&(x, z)))
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TargetFeatureWrite {
    owner: ChunkCoordinate,
    source: ChunkCoordinate,
    ordinal: u32,
    destination: BlockCoordinate,
    state: StateId,
}

impl TargetFeatureWrite {
    #[must_use]
    pub(crate) const fn new(
        owner: ChunkCoordinate,
        source: ChunkCoordinate,
        ordinal: u32,
        destination: BlockCoordinate,
        state: StateId,
    ) -> Self {
        Self {
            owner,
            source,
            ordinal,
            destination,
            state,
        }
    }

    #[must_use]
    pub(crate) const fn owner(self) -> ChunkCoordinate {
        self.owner
    }

    #[must_use]
    pub(crate) const fn source(self) -> ChunkCoordinate {
        self.source
    }

    #[must_use]
    pub(crate) const fn ordinal(self) -> u32 {
        self.ordinal
    }

    #[must_use]
    pub(crate) const fn destination(self) -> BlockCoordinate {
        self.destination
    }

    #[must_use]
    pub(crate) const fn state(self) -> StateId {
        self.state
    }
}

pub(crate) fn target_feature_write_precedes_mutation(
    write: TargetFeatureWrite,
    provenance: MutationProvenance,
) -> bool {
    provenance.stage().stage() == ColumnStage::Features
        && (write.owner(), write.source(), write.ordinal())
            < (provenance.target(), provenance.source(), provenance.ordinal())
}

/// A detached target and its concrete neighbour columns.
///
/// The snapshot retains every column it exposes, so encoding can run after
/// the generation session releases its admission halo.
pub struct PacketSnapshot {
    coordinate: ChunkCoordinate,
    target: GenerationTarget,
    revision: SessionRevision,
    column: Arc<ChunkColumn>,
    neighbours: Vec<PacketNeighbour>,
    light_settlement: std::sync::OnceLock<ColumnLightSettlement>,
}

/// The terminal value of one high-level generation request.
///
/// `Existing` is deliberately not represented as a completed session. A
/// loaded or edited column already has authoritative state, so inventing a
/// generated frontier for it would make a later request overwrite persistence
/// precedence. `Generated` carries an owning snapshot that may be encoded
/// after the request has released its locks.
#[derive(Debug)]
pub enum GenerationRequestResult {
    /// A cache, edit ledger, or persisted region supplied the answer.
    Existing(ChunkColumn),
    /// Generation completed and detached a snapshot awaiting packet-light preparation.
    Generated(PacketSnapshot),
}

/// Failure returned by the object-safe high-level source capability.
#[derive(Debug, thiserror::Error)]
pub enum GenerationRequestError {
    /// The source has no request-scoped capability; callers should use the
    /// legacy scalar `column`/`column_at` path.
    #[error("source has no request-scoped generation capability")]
    Unsupported,
    /// The request state machine rejected a stage, cancellation, or payload.
    #[error("generation request failed: {0}")]
    Session(#[from] SessionError),
    /// The source's persistence/cache boundary rejected an otherwise valid
    /// request. The boundary keeps its concrete error private while exposing
    /// a stable object-safe error to type-erased callers.
    #[error("generation request boundary failed: {0}")]
    Boundary(String),
}

/// Committed state that can be moved into another request with the same
/// pipeline identity. Pending worker completions are intentionally omitted.
#[derive(Debug, Clone)]
pub struct GenerationCheckpoint {
    request: GenerationRequest,
    pipeline_identity: lodestone_worldgen::stage_schedule::PipelineIdentity,
    frontiers: Vec<(ChunkCoordinate, Vec<StageRecord>)>,
    products: Vec<(ProductKey, ImmutableProduct)>,
    sidecars: Vec<(SidecarProductKey, ImmutableSidecar)>,
    aggregates: Vec<(ChunkCoordinate, AggregatePrefix)>,
    committed_mutations: Vec<ProvenanceMutation>,
    committed_mutation_order: Vec<MutationProvenance>,
    source_completions: Vec<SourceCompletionRecord>,
    feature_settlement: Option<FeatureSettlementProof>,
    feature_winner_receipts: Vec<TargetFeatureWrite>,
    current_revision: SessionRevision,
    packet_neighbour_domain: Option<BTreeSet<ChunkCoordinate>>,
}

/// One externally produced shaped prefix retained as a single real product.
///
/// The stage frontier records covered by this value describe the authenticated
/// work boundary; they are not additional copies of the aggregate column.
#[derive(Debug, Clone)]
pub struct AggregatePrefix {
    coordinate: ChunkCoordinate,
    boundary: ColumnStage,
    product: ImmutableProduct,
    input_fingerprint: [u8; 32],
    output_fingerprint: [u8; 32],
    executor_version: u32,
}

impl AggregatePrefix {
    #[must_use]
    pub fn new(
        coordinate: ChunkCoordinate,
        boundary: ColumnStage,
        product: ImmutableProduct,
        input_fingerprint: [u8; 32],
        output_fingerprint: [u8; 32],
        executor_version: u32,
    ) -> Self {
        Self {
            coordinate,
            boundary,
            product,
            input_fingerprint,
            output_fingerprint,
            executor_version,
        }
    }

    #[must_use]
    pub const fn coordinate(&self) -> ChunkCoordinate {
        self.coordinate
    }

    #[must_use]
    pub const fn boundary(&self) -> ColumnStage {
        self.boundary
    }

    #[must_use]
    pub fn product(&self) -> &ImmutableProduct {
        &self.product
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
    pub const fn executor_version(&self) -> u32 {
        self.executor_version
    }
}

impl GenerationCheckpoint {
    pub(crate) fn from_ledger(
        request: GenerationRequest,
        pipeline_identity: lodestone_worldgen::stage_schedule::PipelineIdentity,
        frontiers: Vec<(ChunkCoordinate, Vec<StageRecord>)>,
        products: Vec<(ProductKey, ImmutableProduct)>,
        sidecars: Vec<(SidecarProductKey, ImmutableSidecar)>,
        aggregates: Vec<(ChunkCoordinate, AggregatePrefix)>,
        committed_mutations: Vec<ProvenanceMutation>,
        source_completions: Vec<SourceCompletionRecord>,
        feature_settlement: Option<FeatureSettlementProof>,
        feature_winner_receipts: Vec<TargetFeatureWrite>,
        current_revision: u64,
    ) -> Self {
        let mut committed_mutations = committed_mutations;
        committed_mutations.sort_unstable_by_key(ProvenanceMutation::provenance);
        let committed_mutation_order = committed_mutations
            .iter()
            .map(ProvenanceMutation::provenance)
            .collect();
        Self {
            request,
            pipeline_identity,
            frontiers,
            products,
            sidecars,
            aggregates,
            committed_mutations,
            committed_mutation_order,
            source_completions,
            feature_settlement,
            feature_winner_receipts,
            current_revision: SessionRevision(current_revision),
            packet_neighbour_domain: Some(BTreeSet::new()),
        }
    }

    #[must_use]
    pub const fn request(&self) -> GenerationRequest {
        self.request
    }

    #[must_use]
    pub const fn pipeline_identity(
        &self,
    ) -> lodestone_worldgen::stage_schedule::PipelineIdentity {
        self.pipeline_identity
    }

    #[must_use]
    pub fn frontiers(&self) -> &[(ChunkCoordinate, Vec<StageRecord>)] {
        &self.frontiers
    }

    #[must_use]
    pub fn products(&self) -> &[(ProductKey, ImmutableProduct)] {
        &self.products
    }

    #[must_use]
    pub fn sidecars(&self) -> &[(SidecarProductKey, ImmutableSidecar)] {
        &self.sidecars
    }

    #[must_use]
    pub fn aggregates(&self) -> &[(ChunkCoordinate, AggregatePrefix)] {
        &self.aggregates
    }

    #[must_use]
    pub(crate) fn source_completions(&self) -> &[SourceCompletionRecord] {
        &self.source_completions
    }

    #[must_use]
    pub(crate) const fn feature_settlement(&self) -> Option<FeatureSettlementProof> {
        self.feature_settlement
    }

    #[must_use]
    pub(crate) fn feature_winner_receipts(&self) -> &[TargetFeatureWrite] {
        &self.feature_winner_receipts
    }

    #[must_use]
    pub fn committed_mutations(&self) -> &[ProvenanceMutation] {
        &self.committed_mutations
    }

    #[must_use]
    pub fn committed_mutation_order(&self) -> &[MutationProvenance] {
        &self.committed_mutation_order
    }

    #[must_use]
    pub const fn current_revision(&self) -> SessionRevision {
        self.current_revision
    }

    #[must_use]
    pub fn packet_neighbour_domain(&self) -> Option<&BTreeSet<ChunkCoordinate>> {
        self.packet_neighbour_domain.as_ref()
    }
}

impl fmt::Debug for PacketSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PacketSnapshot")
            .field("coordinate", &self.coordinate)
            .field("target", &self.target)
            .field("revision", &self.revision)
            .field("neighbours", &self.neighbours.len())
            .field("light_settled", &self.is_light_settled())
            .finish_non_exhaustive()
    }
}

impl PacketSnapshot {
    #[cfg(test)]
    pub(crate) fn for_test(column: ChunkColumn) -> Self {
        Self {
            coordinate: (0, 0),
            target: GenerationTarget::Full,
            revision: SessionRevision(0),
            column: Arc::new(column),
            neighbours: Vec::new(),
            light_settlement: std::sync::OnceLock::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn column_handle(&self) -> Arc<ChunkColumn> {
        Arc::clone(&self.column)
    }

    #[must_use]
    pub const fn coordinate(&self) -> ChunkCoordinate {
        self.coordinate
    }

    #[must_use]
    pub const fn target(&self) -> GenerationTarget {
        self.target
    }

    #[must_use]
    pub const fn revision(&self) -> SessionRevision {
        self.revision
    }

    #[must_use]
    pub fn column(&self) -> &ChunkColumn {
        self.column.as_ref()
    }

    #[must_use]
    pub fn neighbours(&self) -> &[PacketNeighbour] {
        &self.neighbours
    }

    /// Returns the packet's computed initial-light product, when prepared.
    #[must_use]
    pub fn light_settlement(&self) -> Option<&ColumnLightSettlement> {
        self.light_settlement.get()
    }

    /// Whether this snapshot owns a computed initial-light product.
    #[must_use]
    pub fn is_light_settled(&self) -> bool {
        self.light_settlement.get().is_some()
    }

    /// Installs the packet-local product exactly once.
    pub fn install_light_settlement(
        &self,
        settlement: ColumnLightSettlement,
    ) -> Result<(), ColumnLightSettlement> {
        self.light_settlement.set(settlement)
    }
}

/// Dimension-specific production generation behind the optional source
/// capability. Implementations submit stage results to the supplied session
/// and return its detached concrete packet snapshot.
pub trait RequestStageDriver: Send + Sync {
    fn generate(&self, session: &mut GenerationSession) -> Result<PacketSnapshot, SessionError>;

    #[cfg(target_arch = "wasm32")]
    fn generate_yielding<'a>(
        &'a self,
        session: &'a mut GenerationSession,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<PacketSnapshot, SessionError>> + 'a>,
    > {
        Box::pin(async move { self.generate(session) })
    }

    fn generate_with_executor(
        &self,
        session: &mut GenerationSession,
        _executor: &dyn crate::worldgen_lifecycle::ImmutableComputeExecutor,
    ) -> Result<PacketSnapshot, SessionError> {
        self.generate(session)
    }
}

/// Why an operation could not advance a session.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("worldgen session is cancelled")]
    Cancelled,
    #[error("coordinate {0:?} is outside the admitted generation halo")]
    OutsideHalo(ChunkCoordinate),
    #[error("stage {found:?} belongs to another dimension")]
    ForeignStage { found: StageKey },
    #[error("stage {0:?} is not in this dimension's pipeline")]
    UnsupportedStage(StageKey),
    #[error("immutable completion for {coordinate:?} {stage:?} is not a pure stage")]
    MutableStage {
        coordinate: ChunkCoordinate,
        stage: StageKey,
    },
    #[error("stage {stage:?} was already committed or queued for {coordinate:?}")]
    DuplicateCompletion {
        coordinate: ChunkCoordinate,
        stage: StageKey,
    },
    #[error("unexpected immutable product {resource:?} for stage {stage:?}")]
    UnexpectedProduct { stage: StageKey, resource: ResourceKey },
    #[error("missing immutable product {resource:?} for stage {stage:?}")]
    MissingProduct { stage: StageKey, resource: ResourceKey },
    #[error("duplicate immutable product {resource:?} for stage {stage:?}")]
    DuplicateProduct { stage: StageKey, resource: ResourceKey },
    #[error("unexpected sidecar {sidecar:?} for stage {stage:?}")]
    UnexpectedSidecar { stage: StageKey, sidecar: SidecarKey },
    #[error("missing sidecar {sidecar:?} for stage {stage:?}")]
    MissingSidecar { stage: StageKey, sidecar: SidecarKey },
    #[error("duplicate sidecar {sidecar:?} for stage {stage:?}")]
    DuplicateSidecar { stage: StageKey, sidecar: SidecarKey },
    #[error("stage frontier rejected {0:?}")]
    Frontier(lodestone_worldgen::stage_schedule::FrontierError),
    #[error("mutable transaction belongs to another session")]
    ForeignTransaction,
    #[error("mutable source order {0} was already queued")]
    DuplicateSourceOrder(u64),
    #[error("mutable source stage changed from {expected:?} to {found:?}")]
    MutableStageChanged { expected: StageKey, found: StageKey },
    #[error("mutable stage {0:?} cannot accept source transactions")]
    NotMutableStage(StageKey),
    #[error("mutable stage {found:?} is not the target frontier's next stage; expected {expected:?}")]
    MutableStageNotReady {
        expected: Option<StageKey>,
        found: StageKey,
    },
    #[error("duplicate mutation {0:?}")]
    DuplicateMutation(MutationProvenance),
    #[error("mutable stage {0:?} still has uncommitted source work")]
    MutableWorkPending(StageKey),
    #[error("packet snapshot is not ready: {0}")]
    PacketNotReady(&'static str),
    #[error("duplicate packet neighbour {0:?}")]
    DuplicatePacketNeighbour(ChunkCoordinate),
    #[error("mutable stage {stage:?} has no declared source set")]
    MissingMutableSourcePlan { stage: StageKey },
    #[error("mutable source plan for {stage:?} is empty")]
    EmptyMutableSourcePlan { stage: StageKey },
    #[error("mutable source plan for {stage:?} has a non-contiguous order")]
    NonContiguousMutableSourcePlan { stage: StageKey },
    #[error("mutable source {coordinate:?} at order {source_order} is not in the declared plan for {stage:?}")]
    UnexpectedMutableSource {
        stage: StageKey,
        source_order: u64,
        coordinate: ChunkCoordinate,
    },
    #[error("mutable source coordinate {0:?} is repeated in the declared plan")]
    DuplicateMutableSource(ChunkCoordinate),
    #[error("mutable stage {stage:?} is incomplete: expected {expected} sources, committed {committed}")]
    MutableSourcesIncomplete {
        stage: StageKey,
        expected: usize,
        committed: usize,
    },
    #[error("mutation destination {destination:?} exceeds the {stage:?} write domain for source {source_coordinate:?}")]
    MutationOutsideWriteDomain {
        stage: StageKey,
        source_coordinate: ChunkCoordinate,
        destination: BlockCoordinate,
    },
    #[error("{kind:?} budget exceeded: limit {limit}, current {current}, requested {requested}")]
    BudgetExceeded {
        kind: BudgetKind,
        limit: usize,
        current: usize,
        requested: usize,
    },
    #[error("packet neighbour domain has not been declared")]
    PacketNeighbourDomainNotDeclared,
    #[error("packet neighbour {0:?} is outside the admitted halo")]
    PacketNeighbourOutsideHalo(ChunkCoordinate),
    #[error("packet neighbour domain differs from the declared domain")]
    PacketNeighbourDomainMismatch,
    #[error("packet neighbour {coordinate:?} has not reached the requested generation target")]
    PacketNeighbourNotReady { coordinate: ChunkCoordinate },
    #[error("checkpoint belongs to a different request")]
    CheckpointRequestMismatch,
    #[error("checkpoint has an incompatible pipeline identity")]
    CheckpointPipelineMismatch,
    #[error("checkpoint contains invalid retained state")]
    InvalidCheckpoint,
}

/// A report from an immutable or mutable advancement attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvanceReport {
    committed_stages: Vec<StageKey>,
    committed_source_orders: Vec<u64>,
    pending_mutable_sources: usize,
    revision: SessionRevision,
}

impl AdvanceReport {
    #[must_use]
    pub fn committed_stages(&self) -> &[StageKey] {
        &self.committed_stages
    }

    #[must_use]
    pub fn committed_source_orders(&self) -> &[u64] {
        &self.committed_source_orders
    }

    #[must_use]
    pub const fn pending_mutable_sources(&self) -> usize {
        self.pending_mutable_sources
    }

    #[must_use]
    pub const fn revision(&self) -> SessionRevision {
        self.revision
    }
}

/// Request-scoped immutable and mutable world-generation state.
pub struct GenerationSession {
    id: SessionId,
    request: GenerationRequest,
    pipeline: DimensionPipeline,
    budget: SessionBudget,
    usage: SessionUsage,
    halo: HaloPlan,
    cancellation: RequestCancellation,
    frontiers: BTreeMap<ChunkCoordinate, StageFrontier>,
    products: BTreeMap<ProductKey, ImmutableProduct>,
    sidecars: BTreeMap<SidecarProductKey, ImmutableSidecar>,
    aggregates: BTreeMap<ChunkCoordinate, AggregatePrefix>,
    pending_immutable: BTreeMap<(ChunkCoordinate, usize), ImmutableStageCompletion>,
    pending_mutable: BTreeMap<u64, MutableTransaction>,
    mutable_stage: Option<StageKey>,
    mutable_source_plan: Option<BTreeMap<u64, ChunkCoordinate>>,
    committed_source_orders: BTreeSet<u64>,
    committed_source_completions: Vec<SourceCompletionRecord>,
    hydrated_source_completions: BTreeMap<(StageKey, u64), ChunkCoordinate>,
    next_mutable_order: u64,
    committed_mutations: BTreeMap<MutationProvenance, ProvenanceMutation>,
    committed_mutation_order: Vec<MutationProvenance>,
    next_revision: u64,
    current_revision: SessionRevision,
    mutation_index: FastSet<MutationProvenance>,
    has_foreign_feature_mutations: bool,
    light_domain_revision: Option<SessionRevision>,
    packet_neighbour_domain: Option<BTreeSet<ChunkCoordinate>>,
    feature_settlement: Option<FeatureSettlementProof>,
    feature_winner_receipts: BTreeMap<BlockCoordinate, TargetFeatureWrite>,
}

impl fmt::Debug for GenerationSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GenerationSession")
            .field("id", &self.id)
            .field("request", &self.request)
            .field("resident_columns", &self.frontiers.len())
            .field("usage", &self.usage)
            .field("pending_immutable", &self.pending_immutable.len())
            .field("pending_mutable", &self.pending_mutable.len())
            .field("current_revision", &self.current_revision)
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish()
    }
}

impl GenerationSession {
    #[must_use]
    pub fn new(request: GenerationRequest) -> Self {
        Self::with_budget(request, SessionBudget::DEFAULT)
    }

    #[must_use]
    pub fn with_budget(request: GenerationRequest, budget: SessionBudget) -> Self {
        Self::with_budget_and_cancellation(request, budget, RequestCancellation::new())
    }

    #[must_use]
    pub fn with_cancellation(
        request: GenerationRequest,
        cancellation: RequestCancellation,
    ) -> Self {
        Self::with_budget_and_cancellation(request, SessionBudget::DEFAULT, cancellation)
    }

    #[must_use]
    pub fn with_budget_and_cancellation(
        request: GenerationRequest,
        budget: SessionBudget,
        cancellation: RequestCancellation,
    ) -> Self {
        let pipeline = pipeline_for(request.dimension());
        pipeline.validate();
        let halo = HaloPlan::new(request);
        let frontiers = halo
            .coordinates()
            .iter()
            .copied()
            .map(|coordinate| (coordinate, pipeline.frontier(coordinate, PipelineOptions::ALL)))
            .collect();
        Self {
            id: SessionId(NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed)),
            request,
            pipeline,
            budget,
            usage: SessionUsage {
                product_entries: 0,
                sidecar_entries: 0,
                mutation_entries: 0,
                retained_bytes: 0,
            },
            halo,
            cancellation,
            frontiers,
            products: BTreeMap::new(),
            sidecars: BTreeMap::new(),
            aggregates: BTreeMap::new(),
            pending_immutable: BTreeMap::new(),
            pending_mutable: BTreeMap::new(),
            mutable_stage: None,
            mutable_source_plan: None,
            committed_source_orders: BTreeSet::new(),
            committed_source_completions: Vec::new(),
            hydrated_source_completions: BTreeMap::new(),
            next_mutable_order: 0,
            committed_mutations: BTreeMap::new(),
            committed_mutation_order: Vec::new(),
            next_revision: 1,
            current_revision: SessionRevision(0),
            mutation_index: FastSet::default(),
            has_foreign_feature_mutations: false,
            light_domain_revision: None,
            packet_neighbour_domain: Some(BTreeSet::new()),
            feature_settlement: None,
            feature_winner_receipts: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    #[must_use]
    pub const fn request(&self) -> GenerationRequest {
        self.request
    }

    #[must_use]
    pub(crate) const fn feature_settlement(&self) -> Option<FeatureSettlementProof> {
        self.feature_settlement
    }

    #[must_use]
    pub(crate) fn feature_winner_receipts(
        &self,
    ) -> impl Iterator<Item = &TargetFeatureWrite> {
        self.feature_winner_receipts.values()
    }

    #[must_use]
    pub const fn pipeline(&self) -> DimensionPipeline {
        self.pipeline
    }

    #[must_use]
    pub const fn halo(&self) -> &HaloPlan {
        &self.halo
    }

    #[must_use]
    pub fn cancellation(&self) -> RequestCancellation {
        self.cancellation.clone()
    }

    #[must_use]
    pub const fn budget(&self) -> SessionBudget {
        self.budget
    }

    #[must_use]
    pub fn usage(&self) -> SessionUsage {
        if !self.cancellation.is_cancelled() {
            return self.usage;
        }
        let product_entries = self.products.len().saturating_add(self.aggregates.len());
        let sidecar_entries = self.sidecars.len();
        let mutation_entries = self.committed_mutations.len();
        let retained_bytes = self
            .products
            .values()
            .map(ImmutableProduct::retained_bytes)
            .chain(self.aggregates.values().map(|aggregate| aggregate.product().retained_bytes()))
            .chain(self.sidecars.values().map(ImmutableSidecar::retained_bytes))
            .chain(
                self.committed_mutations
                    .values()
                    .map(ProvenanceMutation::retained_bytes),
            )
            .fold(0usize, usize::saturating_add);
        SessionUsage {
            product_entries,
            sidecar_entries,
            mutation_entries,
            retained_bytes,
        }
    }

    #[must_use]
    pub fn admission_order(&self) -> &[ChunkCoordinate] {
        self.halo.coordinates()
    }

    #[must_use]
    pub fn frontier(&self, coordinate: ChunkCoordinate) -> Option<&StageFrontier> {
        self.frontiers.get(&coordinate)
    }

    pub fn stage_read_radius(&self, stage: StageKey) -> Result<u8, SessionError> {
        Ok(self.descriptor(stage)?.task_read_radius().chunks_value())
    }

    pub fn stage_mutable_write_radius(&self, stage: StageKey) -> Result<u8, SessionError> {
        Ok(self
            .descriptor(stage)?
            .mutable_write_radius()
            .chunks_value())
    }

    #[must_use]
    pub fn mutable_source_plan(&self) -> Option<&BTreeMap<u64, ChunkCoordinate>> {
        self.mutable_source_plan.as_ref()
    }

    /// Whether a source order was committed by the currently active mutable
    /// stage. Drivers use this to avoid redoing a prefix restored from the
    /// world-owned ledger.
    #[must_use]
    pub fn source_order_committed(&self, source_order: u64) -> bool {
        self.committed_source_orders.contains(&source_order)
    }

    #[must_use]
    pub const fn current_revision(&self) -> SessionRevision {
        self.current_revision
    }

    #[must_use]
    pub fn committed_mutations(&self) -> impl Iterator<Item = &ProvenanceMutation> {
        self.committed_mutations.values()
    }

    pub(crate) fn feature_overlay_destinations(
        &self,
        output: ChunkCoordinate,
    ) -> BTreeSet<BlockCoordinate> {
        let mut destinations = BTreeSet::new();
        if !self.has_foreign_feature_mutations {
            return destinations;
        }
        let stage = StageKey::new(self.pipeline.dimension(), ColumnStage::Features);
        for mutation in self.committed_mutations.values() {
            let provenance = mutation.provenance();
            if provenance.stage() != stage {
                continue;
            }
            let destination = provenance.destination();
            if destination.x().div_euclid(16) == output.0
                && destination.z().div_euclid(16) == output.1
            {
                destinations.insert(destination);
            }
        }
        destinations
    }

    #[must_use]
    pub fn committed_mutation_order(&self) -> &[MutationProvenance] {
        &self.committed_mutation_order
    }

    pub(crate) fn commit_target_feature_settlement(
        &mut self,
        proof: FeatureSettlementProof,
        completed_owners: &[ChunkCoordinate],
        writes: &[TargetFeatureWrite],
        overlay_conflict_winners: &[TargetFeatureWrite],
        foreign_winner_count: usize,
    ) -> Result<(), SessionError> {
        if proof.output() != self.request.target
            || self.feature_settlement.is_some_and(|current| current != proof)
        {
            return Err(SessionError::InvalidCheckpoint);
        }
        let stage = StageKey::new(self.pipeline.dimension(), ColumnStage::Features);
        let descriptor = self.descriptor(stage)?;
        if !proof.matches_square(
            self.request.target,
            i32::from(descriptor.mutable_write_radius().chunks_value()),
        ) {
            return Err(SessionError::InvalidCheckpoint);
        }
        let target_frontier = self
            .frontiers
            .get(&self.request.target)
            .expect("the target is always in its own halo");
        if !target_frontier.records().iter().any(|record| record.key() == stage) {
            return Err(SessionError::InvalidCheckpoint);
        }

        let mut owners = BTreeSet::new();
        if completed_owners
            .iter()
            .any(|owner| !owners.insert(*owner) || !self.halo.contains(*owner))
            || !proof.has_complete_square(&owners)
        {
            return Err(SessionError::InvalidCheckpoint);
        }
        let mut logical_writes = FastSet::default();
        let mut pending = Vec::with_capacity(writes.len());
        let mut next_revision = self.next_revision;
        let mut existing_writes = BTreeMap::new();
        let mut existing_feature_winners = BTreeMap::new();
        if self.has_foreign_feature_mutations {
            for (provenance, mutation) in &self.committed_mutations {
                if provenance.stage() == stage {
                    if let Some(state) = mutation.get::<StateId>() {
                        let destination = provenance.destination();
                        if destination.x().div_euclid(16) == self.request.target.0
                            && destination.z().div_euclid(16) == self.request.target.1
                        {
                            let current = existing_feature_winners
                                .entry(destination)
                                .or_insert((*provenance, *state));
                            if provenance < &current.0 {
                                *current = (*provenance, *state);
                            }
                        }
                        existing_writes.entry((
                            provenance.target(),
                            provenance.source(),
                            provenance.stage(),
                            provenance.ordinal(),
                            provenance.destination(),
                        )).or_insert(*state);
                    }
                }
            }
        }
        let mut overlay_winners_by_destination = BTreeMap::new();
        for winner in overlay_conflict_winners {
            let destination = winner.destination();
            let destination_chunk = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            if !owners.contains(&winner.owner())
                || !proof.contains(winner.source())
                || destination_chunk != self.request.target
                || !descriptor.mutable_write_radius().contains_offset(
                    destination_chunk.0 - winner.source().0,
                    destination_chunk.1 - winner.source().1,
                )
                || !existing_feature_winners.contains_key(&destination)
                || overlay_winners_by_destination
                    .insert(destination, *winner)
                    .is_some()
            {
                return Err(SessionError::InvalidCheckpoint);
            }
        }
        for write in writes {
            let destination = write.destination();
            let destination_chunk = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            if write.owner() == self.request.target
                || write.owner() != write.source()
                || !owners.contains(&write.owner())
                || !proof.contains(write.source())
                || !self.halo.contains(destination_chunk)
                || destination_chunk != self.request.target
                || !descriptor.mutable_write_radius().contains_offset(
                    destination_chunk.0 - write.source().0,
                    destination_chunk.1 - write.source().1,
                )
            {
                return Err(SessionError::InvalidCheckpoint);
            }
            if overlay_winners_by_destination
                .get(&destination)
                .is_some_and(|winner| winner.owner() == self.request.target || winner != write)
            {
                return Err(SessionError::InvalidCheckpoint);
            }
            let logical_key = (
                write.owner(),
                write.source(),
                stage,
                write.ordinal(),
                destination,
            );
            if !logical_writes.insert(logical_key) {
                return Err(SessionError::InvalidCheckpoint);
            }
            if let Some(existing) = existing_writes.get(&logical_key) {
                if *existing == write.state() {
                    continue;
                }
                return Err(SessionError::InvalidCheckpoint);
            }
            let revision = SessionRevision(next_revision);
            next_revision = next_revision.saturating_add(1);
            let provenance = MutationProvenance {
                target: write.owner(),
                source: write.source(),
                stage,
                ordinal: write.ordinal(),
                destination,
                revision,
            };
            pending.push(ProvenanceMutation {
                provenance,
                value: Arc::new(write.state()),
                retained_bytes: size_of::<StateId>(),
            });
            existing_writes.insert(logical_key, write.state());
        }
        if writes.len() != foreign_winner_count {
            return Err(SessionError::InvalidCheckpoint);
        }
        for winner in overlay_winners_by_destination.values() {
            if winner.owner() != self.request.target
                && !logical_writes.contains(&(
                    winner.owner(),
                    winner.source(),
                    stage,
                    winner.ordinal(),
                    winner.destination(),
                ))
            {
                return Err(SessionError::InvalidCheckpoint);
            }
        }
        if self
            .feature_winner_receipts
            .iter()
            .any(|(destination, receipt)| {
                overlay_winners_by_destination.get(destination) != Some(receipt)
            })
        {
            return Err(SessionError::InvalidCheckpoint);
        }
        let mut new_receipts = Vec::new();
        for (&destination, winner) in &overlay_winners_by_destination {
            if winner.owner() != self.request.target
                || self.feature_winner_receipts.contains_key(&destination)
            {
                continue;
            }
            let (existing, state) = existing_feature_winners
                .get(&destination)
                .ok_or(SessionError::InvalidCheckpoint)?;
            if state == &winner.state() {
                continue;
            }
            if !target_feature_write_precedes_mutation(*winner, *existing) {
                return Err(SessionError::InvalidCheckpoint);
            }
            new_receipts.push(*winner);
        }
        for receipt in &new_receipts {
            let destination = receipt.destination();
            if let Some(existing) = self.feature_winner_receipts.get(&destination)
                && existing != receipt
            {
                return Err(SessionError::InvalidCheckpoint);
            }
        }
        let receipt_bytes = new_receipts
            .len()
            .saturating_mul(size_of::<TargetFeatureWrite>());
        let retained_bytes = pending
            .iter()
            .map(ProvenanceMutation::retained_bytes)
            .fold(receipt_bytes, usize::saturating_add);
        let mutation_count = pending.len().saturating_add(new_receipts.len());
        self.check_usage(0, 0, mutation_count, retained_bytes)?;
        self.feature_settlement = Some(proof);
        self.next_revision = next_revision;
        for receipt in new_receipts {
            self.feature_winner_receipts
                .insert(receipt.destination(), receipt);
        }
        for mutation in pending {
            let provenance = mutation.provenance();
            self.mutation_index.insert(provenance);
            self.committed_mutation_order.push(provenance);
            self.committed_mutations.insert(provenance, mutation);
            self.has_foreign_feature_mutations = true;
        }
        if retained_bytes != 0 {
            self.add_usage(0, 0, mutation_count, retained_bytes);
            self.bump_revision();
            self.light_domain_revision = None;
        }
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), SessionError> {
        if self.cancellation.is_cancelled() {
            Err(SessionError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn ensure_coordinate(&self, coordinate: ChunkCoordinate) -> Result<(), SessionError> {
        self.frontiers
            .contains_key(&coordinate)
            .then_some(())
            .ok_or(SessionError::OutsideHalo(coordinate))
    }

    fn check_budget(
        &self,
        kind: BudgetKind,
        current: usize,
        requested: usize,
        limit: usize,
    ) -> Result<(), SessionError> {
        let total = current.checked_add(requested).ok_or(SessionError::BudgetExceeded {
            kind,
            limit,
            current,
            requested,
        })?;
        if total > limit {
            return Err(SessionError::BudgetExceeded {
                kind,
                limit,
                current,
                requested,
            });
        }
        Ok(())
    }

    fn check_usage(
        &self,
        products: usize,
        sidecars: usize,
        mutations: usize,
        retained_bytes: usize,
    ) -> Result<(), SessionError> {
        self.check_budget(
            BudgetKind::Products,
            self.usage.product_entries,
            products,
            self.budget.max_product_entries,
        )?;
        self.check_budget(
            BudgetKind::Sidecars,
            self.usage.sidecar_entries,
            sidecars,
            self.budget.max_sidecar_entries,
        )?;
        self.check_budget(
            BudgetKind::Mutations,
            self.usage.mutation_entries,
            mutations,
            self.budget.max_mutation_entries,
        )?;
        self.check_budget(
            BudgetKind::RetainedBytes,
            self.usage.retained_bytes,
            retained_bytes,
            self.budget.max_retained_bytes,
        )
    }

    fn add_usage(
        &mut self,
        products: usize,
        sidecars: usize,
        mutations: usize,
        retained_bytes: usize,
    ) {
        self.usage.product_entries += products;
        self.usage.sidecar_entries += sidecars;
        self.usage.mutation_entries += mutations;
        self.usage.retained_bytes += retained_bytes;
    }

    fn subtract_usage(
        &mut self,
        products: usize,
        sidecars: usize,
        mutations: usize,
        retained_bytes: usize,
    ) {
        self.usage.product_entries -= products;
        self.usage.sidecar_entries -= sidecars;
        self.usage.mutation_entries -= mutations;
        self.usage.retained_bytes -= retained_bytes;
    }

    fn completion_usage(completion: &ImmutableStageCompletion) -> (usize, usize, usize) {
        (
            completion.products.len(),
            completion.sidecars.len(),
            completion
                .products
                .iter()
                .map(ImmutableProduct::retained_bytes)
                .chain(completion.sidecars.iter().map(ImmutableSidecar::retained_bytes))
                .fold(0usize, usize::saturating_add),
        )
    }

    fn transaction_usage(transaction: &MutableTransaction) -> (usize, usize) {
        (
            transaction.writes.len(),
            transaction
                .writes
                .iter()
                .map(ProvenanceMutation::retained_bytes)
                .fold(0usize, usize::saturating_add),
        )
    }

    fn descriptor(&self, key: StageKey) -> Result<StageDescriptor, SessionError> {
        if key.dimension() != self.pipeline.dimension() {
            return Err(SessionError::ForeignStage { found: key });
        }
        self.pipeline
            .descriptor(key.stage())
            .ok_or(SessionError::UnsupportedStage(key))
    }

    /// Import one authenticated shaped prefix as a single real product.
    ///
    /// The frontier records are coverage declarations for the externally
    /// completed prefix. Only `aggregate` is retained as a product value, so
    /// an aggregate terrain column is never copied into per-stage stand-ins.
    pub fn import_aggregate_prefix(
        &mut self,
        coordinate: ChunkCoordinate,
        boundary: ColumnStage,
        aggregate: ImmutableProduct,
        sidecars: impl IntoIterator<Item = (StageKey, ImmutableSidecar)>,
        input_fingerprint: [u8; 32],
        output_fingerprint: [u8; 32],
        executor_version: u32,
    ) -> Result<(), SessionError> {
        self.ensure_active()?;
        self.ensure_coordinate(coordinate)?;
        let expected_boundary = self
            .pipeline
            .schedule()
            .target_stage(GenerationTarget::Shaped);
        if boundary != expected_boundary
            || aggregate.resource() != ResourceKey::MaterializedWorld
            || (aggregate.get::<ChunkColumn>().is_none()
                && aggregate
                    .get::<lodestone_worldgen::overworld::GeneratedColumn>()
                    .is_none())
        {
            return Err(SessionError::InvalidCheckpoint);
        }
        if self.aggregates.contains_key(&coordinate) {
            return Err(SessionError::DuplicateCompletion {
                coordinate,
                stage: StageKey::new(self.pipeline.dimension(), boundary),
            });
        }
        let current = self
            .frontiers
            .get(&coordinate)
            .expect("coordinate was checked above");
        if !current.records().is_empty() {
            return Err(SessionError::DuplicateCompletion {
                coordinate,
                stage: StageKey::new(self.pipeline.dimension(), boundary),
            });
        }

        let mut supplied = BTreeMap::new();
        for (stage, sidecar) in sidecars {
            if stage.dimension() != self.pipeline.dimension()
                || self
                    .pipeline
                    .descriptor(stage.stage())
                    .is_none_or(|descriptor| !descriptor.retained_sidecars().contains(&sidecar.sidecar()))
                || supplied
                    .insert((stage, sidecar.sidecar()), sidecar)
                    .is_some()
            {
                return Err(SessionError::InvalidCheckpoint);
            }
        }

        let mut candidate = current.clone();
        let prefix = self
            .pipeline
            .schedule()
            .stages_for(GenerationTarget::Shaped);
        let mut stage_sidecars = Vec::new();
        for &stage in prefix {
            let descriptor = self
                .pipeline
                .descriptor(stage)
                .expect("shaped stage is described by its pipeline");
            let key = StageKey::new(self.pipeline.dimension(), stage);
            let retained = descriptor
                .retained_sidecars()
                .iter()
                .copied()
                .map(|sidecar| {
                    supplied
                        .remove(&(key, sidecar))
                        .ok_or(SessionError::MissingSidecar { stage: key, sidecar })
                })
                .collect::<Result<Vec<_>, _>>()?;
            stage_sidecars.extend(
                retained
                    .iter()
                    .cloned()
                    .map(|sidecar| (SidecarProductKey::new(coordinate, key, sidecar.sidecar()), sidecar)),
            );
            candidate
                .commit(StageRecord::for_descriptor(
                    descriptor,
                    input_fingerprint,
                    output_fingerprint,
                    executor_version,
                ))
                .map_err(SessionError::Frontier)?;
        }
        if !supplied.is_empty() {
            return Err(SessionError::InvalidCheckpoint);
        }

        let aggregate = AggregatePrefix::new(
            coordinate,
            boundary,
            aggregate,
            input_fingerprint,
            output_fingerprint,
            executor_version,
        );
        let sidecar_count = stage_sidecars.len();
        let retained_bytes = aggregate.product().retained_bytes()
            + stage_sidecars
                .iter()
                .map(|(_, sidecar)| sidecar.retained_bytes())
                .sum::<usize>();
        self.check_usage(1, stage_sidecars.len(), 0, retained_bytes)?;
        self.frontiers.insert(coordinate, candidate);
        self.aggregates.insert(coordinate, aggregate);
        for (key, sidecar) in stage_sidecars {
            self.sidecars.insert(key, sidecar);
        }
        self.add_usage(1, sidecar_count, 0, retained_bytes);
        self.bump_revision();
        self.light_domain_revision = None;
        Ok(())
    }

    /// Read the retained aggregate product for one admitted resident.
    #[must_use]
    pub fn aggregate_prefix(&self, coordinate: ChunkCoordinate) -> Option<Arc<ChunkColumn>> {
        self.aggregates
            .get(&coordinate)
            .and_then(|aggregate| aggregate.product().get::<ChunkColumn>())
    }

    /// Whether this coordinate has an authenticated shaped aggregate, without
    /// forcing a typed generated product into the server column carrier.
    #[must_use]
    pub fn has_aggregate_prefix(&self, coordinate: ChunkCoordinate) -> bool {
        self.aggregates.contains_key(&coordinate)
    }

    /// Return the authenticated shaped aggregate in its original typed form.
    #[must_use]
    pub fn aggregate_prefix_product(&self, coordinate: ChunkCoordinate) -> Option<ImmutableProduct> {
        self.aggregates
            .get(&coordinate)
            .map(|aggregate| aggregate.product().clone())
    }

    /// Queue a pure stage result. Arrival order is independent of commit order.
    pub fn complete_immutable(
        &mut self,
        completion: ImmutableStageCompletion,
    ) -> Result<(), SessionError> {
        self.ensure_active()?;
        self.ensure_coordinate(completion.coordinate())?;
        let descriptor = self.descriptor(completion.stage())?;
        if descriptor.barrier() == BarrierPolicy::SourceOrdered {
            return Err(SessionError::MutableStage {
                coordinate: completion.coordinate(),
                stage: completion.stage(),
            });
        }
        self.validate_payloads(&completion, descriptor)?;
        let index = self
            .pipeline
            .schedule()
            .index_of(completion.stage().stage())
            .ok_or(SessionError::UnsupportedStage(completion.stage()))?;
        let frontier = self
            .frontiers
            .get(&completion.coordinate())
            .expect("coordinate was checked above");
        if frontier.records().iter().any(|record| record.key() == completion.stage())
            || self
                .pending_immutable
                .contains_key(&(completion.coordinate(), index))
        {
            return Err(SessionError::DuplicateCompletion {
                coordinate: completion.coordinate(),
                stage: completion.stage(),
            });
        }
        let (products, sidecars, retained_bytes) = Self::completion_usage(&completion);
        self.check_usage(products, sidecars, 0, retained_bytes)?;
        self.pending_immutable
            .insert((completion.coordinate(), index), completion);
        self.add_usage(products, sidecars, 0, retained_bytes);
        Ok(())
    }

    /// Commit every queued immutable stage whose predecessors are now ready.
    pub fn advance_ready_immutable(&mut self) -> Result<AdvanceReport, SessionError> {
        self.ensure_active()?;
        let mut committed_stages = Vec::new();
        loop {
            let mut progressed = false;
            for admission_index in 0..self.halo.coordinates().len() {
                self.ensure_active()?;
                let coordinate = self.halo.coordinates()[admission_index];
                let Some(frontier) = self.frontiers.get(&coordinate) else {
                    continue;
                };
                let Some(key) = frontier.next_stage() else {
                    continue;
                };
                let Some(index) = self.pipeline.schedule().index_of(key.stage()) else {
                    continue;
                };
                let pending_key = (coordinate, index);
                let Some(completion) = self.pending_immutable.remove(&pending_key) else {
                    continue;
                };
                let record = StageRecord::for_descriptor(
                    self.descriptor(completion.stage())?,
                    completion.input_fingerprint,
                    completion.output_fingerprint,
                    completion.executor_version,
                );
                let frontier = self
                    .frontiers
                    .get_mut(&coordinate)
                    .expect("coordinate was checked above");
                if let Err(error) = frontier.commit(record) {
                    self.pending_immutable.insert(pending_key, completion);
                    return Err(SessionError::Frontier(error));
                }
                self.retain_completion(&completion);
                self.bump_revision();
                self.light_domain_revision = None;
                committed_stages.push(key);
                progressed = true;
            }
            if !progressed {
                break;
            }
        }
        Ok(AdvanceReport {
            committed_stages,
            committed_source_orders: Vec::new(),
            pending_mutable_sources: self.pending_mutable.len(),
            revision: self.current_revision,
        })
    }

    fn validate_payloads(
        &self,
        completion: &ImmutableStageCompletion,
        descriptor: StageDescriptor,
    ) -> Result<(), SessionError> {
        let stage = completion.stage();
        let mut products = BTreeSet::new();
        for product in &completion.products {
            if !products.insert(product.resource()) {
                return Err(SessionError::DuplicateProduct {
                    stage,
                    resource: product.resource(),
                });
            }
            if !descriptor.outputs().contains(&product.resource()) {
                return Err(SessionError::UnexpectedProduct {
                    stage,
                    resource: product.resource(),
                });
            }
        }
        for &resource in descriptor.outputs() {
            if !products.contains(&resource) {
                return Err(SessionError::MissingProduct { stage, resource });
            }
        }
        let mut sidecars = BTreeSet::new();
        for sidecar in &completion.sidecars {
            if !sidecars.insert(sidecar.sidecar()) {
                return Err(SessionError::DuplicateSidecar {
                    stage,
                    sidecar: sidecar.sidecar(),
                });
            }
            if !descriptor.retained_sidecars().contains(&sidecar.sidecar()) {
                return Err(SessionError::UnexpectedSidecar {
                    stage,
                    sidecar: sidecar.sidecar(),
                });
            }
        }
        for &sidecar in descriptor.retained_sidecars() {
            if !sidecars.contains(&sidecar) {
                return Err(SessionError::MissingSidecar { stage, sidecar });
            }
        }
        Ok(())
    }

    fn retain_completion(&mut self, completion: &ImmutableStageCompletion) {
        for product in &completion.products {
            self.products.insert(
                ProductKey::new(completion.coordinate, completion.stage, product.resource()),
                product.clone(),
            );
        }
        for sidecar in &completion.sidecars {
            self.sidecars.insert(
                SidecarProductKey::new(completion.coordinate, completion.stage, sidecar.sidecar()),
                sidecar.clone(),
            );
        }
    }

    /// Obtain a revisioned read over retained products in one resident column.
    pub fn resident_read(
        &self,
        coordinate: ChunkCoordinate,
    ) -> Result<ResidentRead, SessionError> {
        self.ensure_coordinate(coordinate)?;
        Ok(ResidentRead {
            coordinate,
            revision: self.current_revision,
            products: self
                .products
                .iter()
                .filter(|(key, _)| key.coordinate() == coordinate)
                .map(|(key, product)| (*key, product.clone()))
                .collect::<Vec<_>>()
                .into_iter()
                .chain(self.aggregates.get(&coordinate).map(|aggregate| {
                    (
                        ProductKey::new(
                            coordinate,
                            StageKey::new(
                                self.pipeline.dimension(),
                                aggregate.boundary(),
                            ),
                            aggregate.product().resource(),
                        ),
                        aggregate.product().clone(),
                    )
                }))
                .collect(),
            sidecars: self
                .sidecars
                .iter()
                .filter(|(key, _)| key.coordinate == coordinate)
                .map(|(key, sidecar)| (*key, sidecar.clone()))
                .collect(),
        })
    }

    /// Declare the complete canonical source set before submitting mutable
    /// work. Orders must be a contiguous zero-based prefix and each source
    /// coordinate must be resident in this request's halo.
    pub fn declare_mutable_sources(
        &mut self,
        stage: StageKey,
        sources: impl IntoIterator<Item = (u64, ChunkCoordinate)>,
    ) -> Result<(), SessionError> {
        self.ensure_active()?;
        let descriptor = self.descriptor(stage)?;
        if descriptor.barrier() != BarrierPolicy::SourceOrdered {
            return Err(SessionError::NotMutableStage(stage));
        }
        let expected = self
            .frontiers
            .get(&self.request.target)
            .and_then(StageFrontier::next_stage);
        if expected != Some(stage) {
            return Err(SessionError::MutableStageNotReady {
                expected,
                found: stage,
            });
        }
        if self.mutable_stage.is_some() || self.mutable_source_plan.is_some() {
            return Err(SessionError::MutableStageChanged {
                expected: self.mutable_stage.unwrap_or(stage),
                found: stage,
            });
        }
        let mut plan = BTreeMap::new();
        for (source_order, source) in sources {
            if !self.halo.contains(source) {
                return Err(SessionError::OutsideHalo(source));
            }
            if plan.insert(source_order, source).is_some() {
                return Err(SessionError::DuplicateSourceOrder(source_order));
            }
        }
        if plan.is_empty() {
            return Err(SessionError::EmptyMutableSourcePlan { stage });
        }
        if plan.keys().copied().enumerate().any(|(index, order)| {
            u64::try_from(index).ok() != Some(order)
        }) {
            return Err(SessionError::NonContiguousMutableSourcePlan { stage });
        }
        let mut seen_sources = BTreeSet::new();
        for source in plan.values().copied() {
            if !seen_sources.insert(source) {
                return Err(SessionError::DuplicateMutableSource(source));
            }
        }
        let hydrated = self
            .hydrated_source_completions
            .iter()
            .filter(|((hydrated_stage, _), _)| *hydrated_stage == stage)
            .map(|((_, source_order), source)| (*source_order, *source))
            .collect::<BTreeMap<_, _>>();
        if self
            .hydrated_source_completions
            .keys()
            .any(|(hydrated_stage, _)| *hydrated_stage != stage)
        {
            return Err(SessionError::MutableStageChanged {
                expected: self
                    .hydrated_source_completions
                    .keys()
                    .next()
                    .map(|(hydrated_stage, _)| *hydrated_stage)
                    .unwrap_or(stage),
                found: stage,
            });
        }
        if hydrated
            .iter()
            .any(|(source_order, source)| plan.get(source_order) != Some(source))
        {
            return Err(SessionError::InvalidCheckpoint);
        }
        self.mutable_source_plan = Some(plan);
        self.mutable_stage = Some(stage);
        self.next_mutable_order = 0;
        self.committed_source_orders.clear();
        while hydrated.contains_key(&self.next_mutable_order) {
            self.committed_source_orders.insert(self.next_mutable_order);
            self.next_mutable_order += 1;
        }
        self.hydrated_source_completions.clear();
        Ok(())
    }

    /// Begin one private mutable source transaction.
    pub fn begin_mutable_source(
        &mut self,
        source: ChunkCoordinate,
        stage: StageKey,
        source_order: u64,
    ) -> Result<MutableTransaction, SessionError> {
        self.ensure_active()?;
        self.ensure_coordinate(source)?;
        let descriptor = self.descriptor(stage)?;
        if descriptor.barrier() != BarrierPolicy::SourceOrdered {
            return Err(SessionError::NotMutableStage(stage));
        }
        let expected = self
            .frontiers
            .get(&self.request.target)
            .and_then(StageFrontier::next_stage);
        if expected != Some(stage) {
            return Err(SessionError::MutableStageNotReady {
                expected,
                found: stage,
            });
        }
        if self.mutable_stage != Some(stage) {
            return Err(SessionError::MissingMutableSourcePlan { stage });
        }
        let plan = self
            .mutable_source_plan
            .as_ref()
            .ok_or(SessionError::MissingMutableSourcePlan { stage })?;
        if plan.get(&source_order).copied() != Some(source) {
            return Err(SessionError::UnexpectedMutableSource {
                stage,
                source_order,
                coordinate: source,
            });
        }
        if source_order < self.next_mutable_order || self.pending_mutable.contains_key(&source_order) {
            return Err(SessionError::DuplicateSourceOrder(source_order));
        }
        let revision = SessionRevision(self.next_revision);
        self.next_revision += 1;
        Ok(MutableTransaction {
            session: self.id,
            target: self.request.target,
            source,
            stage,
            source_order,
            revision,
            writes: Vec::new(),
            provenance_index: BTreeSet::new(),
        })
    }

    /// Queue a source completion and commit every contiguous source prefix.
    pub fn complete_mutable_source(
        &mut self,
        transaction: MutableTransaction,
    ) -> Result<AdvanceReport, SessionError> {
        self.ensure_active()?;
        if transaction.session != self.id {
            return Err(SessionError::ForeignTransaction);
        }
        let stage = self
            .mutable_stage
            .ok_or(SessionError::MissingMutableSourcePlan {
                stage: transaction.stage,
            })?;
        if transaction.stage != stage {
            return Err(SessionError::MutableStageChanged {
                expected: stage,
                found: transaction.stage,
            });
        }
        let plan = self
            .mutable_source_plan
            .as_ref()
            .ok_or(SessionError::MissingMutableSourcePlan { stage })?;
        if plan.get(&transaction.source_order).copied() != Some(transaction.source) {
            return Err(SessionError::UnexpectedMutableSource {
                stage,
                source_order: transaction.source_order,
                coordinate: transaction.source,
            });
        }
        let descriptor = self.descriptor(stage)?;
        for write in &transaction.writes {
            let destination = write.provenance().destination();
            let destination_chunk = (destination.x().div_euclid(16), destination.z().div_euclid(16));
            if !descriptor.mutable_write_radius().contains_offset(
                destination_chunk.0 - transaction.source.0,
                destination_chunk.1 - transaction.source.1,
            ) {
                return Err(SessionError::MutationOutsideWriteDomain {
                    stage,
                    source_coordinate: transaction.source,
                    destination,
                });
            }
        }
        if self.committed_source_orders.contains(&transaction.source_order) {
            return Err(SessionError::DuplicateSourceOrder(transaction.source_order));
        }
        if self
            .committed_mutations
            .keys()
            .any(|key| key.revision == transaction.revision)
        {
            return Err(SessionError::ForeignTransaction);
        }
        if transaction.source_order < self.next_mutable_order
            || self.pending_mutable.contains_key(&transaction.source_order)
        {
            return Err(SessionError::DuplicateSourceOrder(transaction.source_order));
        }
        let (mutation_entries, retained_bytes) = Self::transaction_usage(&transaction);
        self.check_usage(0, 0, mutation_entries, retained_bytes)?;

        for write in &transaction.writes {
            if self.mutation_index.contains(&write.provenance()) {
                return Err(SessionError::DuplicateMutation(write.provenance()));
            }
        }

        for write in &transaction.writes {
            self.mutation_index.insert(write.provenance());
        }
        self.pending_mutable
            .insert(transaction.source_order, transaction);
        self.add_usage(0, 0, mutation_entries, retained_bytes);

        let mut order = self.next_mutable_order;
        let mut ready_orders = Vec::new();
        while self.pending_mutable.contains_key(&order) {
            ready_orders.push(order);
            order += 1;
        }
        let mut committed_source_orders = Vec::new();
        for order in ready_orders {
            let transaction = self
                .pending_mutable
                .remove(&order)
                .expect("preflighted mutable source remains queued");
            let source_order = transaction.source_order;
            for write in transaction.writes {
                let provenance = write.provenance();
                self.has_foreign_feature_mutations |=
                    provenance.stage() == stage && provenance.target() != self.request.target;
                self.committed_mutation_order.push(write.provenance);
                self.committed_mutations.insert(write.provenance, write);
            }
            if transaction.revision > self.current_revision {
                self.current_revision = transaction.revision;
            }
            self.next_mutable_order += 1;
            self.committed_source_orders.insert(source_order);
            self.committed_source_completions.push(SourceCompletionRecord::new(
                transaction.stage,
                source_order,
                transaction.source,
            ));
            self.light_domain_revision = None;
            committed_source_orders.push(source_order);
        }
        Ok(AdvanceReport {
            committed_stages: Vec::new(),
            committed_source_orders,
            pending_mutable_sources: self.pending_mutable.len(),
            revision: self.current_revision,
        })
    }

    /// Commit the stage result after all source transactions have settled.
    pub fn commit_mutable_stage(
        &mut self,
        stage: StageKey,
        input_fingerprint: [u8; 32],
        output_fingerprint: [u8; 32],
        executor_version: u32,
        products: Vec<ImmutableProduct>,
        sidecars: Vec<ImmutableSidecar>,
    ) -> Result<(), SessionError> {
        self.ensure_active()?;
        if self.mutable_stage != Some(stage) {
            return Err(SessionError::NotMutableStage(stage));
        }
        if !self.pending_mutable.is_empty() {
            return Err(SessionError::MutableWorkPending(stage));
        }
        let expected_sources = self
            .mutable_source_plan
            .as_ref()
            .ok_or(SessionError::MissingMutableSourcePlan { stage })?;
        if self.committed_source_orders.len() != expected_sources.len()
            || self.next_mutable_order != expected_sources.len() as u64
        {
            return Err(SessionError::MutableSourcesIncomplete {
                stage,
                expected: expected_sources.len(),
                committed: self.committed_source_orders.len(),
            });
        }
        let descriptor = self.descriptor(stage)?;
        let completion = ImmutableStageCompletion::new(
            self.request.target,
            stage,
            input_fingerprint,
            output_fingerprint,
            executor_version,
            products,
            sidecars,
        );
        self.validate_payloads(&completion, descriptor)?;
        let (products, sidecars, retained_bytes) = Self::completion_usage(&completion);
        self.check_usage(products, sidecars, 0, retained_bytes)?;
        let frontier = self
            .frontiers
            .get_mut(&self.request.target)
            .expect("the target is always in its own halo");
        let record = StageRecord::for_descriptor(
            descriptor,
            input_fingerprint,
            output_fingerprint,
            executor_version,
        );
        frontier.commit(record).map_err(SessionError::Frontier)?;
        self.retain_completion(&completion);
        self.add_usage(products, sidecars, 0, retained_bytes);
        self.bump_revision();
        self.light_domain_revision = None;
        self.mutable_stage = None;
        self.mutable_source_plan = None;
        self.committed_source_orders.clear();
        self.next_mutable_order = 0;
        Ok(())
    }

    /// Discard an unsubmitted transaction. Committed overlays are unchanged.
    pub fn rollback_transaction(
        &self,
        transaction: MutableTransaction,
    ) -> Result<(), SessionError> {
        if transaction.session != self.id {
            return Err(SessionError::ForeignTransaction);
        }
        Ok(())
    }

    /// Set the exact neighbour domain that packet finalization must receive.
    /// Every coordinate is checked against the admitted halo immediately;
    /// supplied columns are checked against this set at finalization.
    pub fn declare_packet_neighbours(
        &mut self,
        neighbours: impl IntoIterator<Item = ChunkCoordinate>,
    ) -> Result<(), SessionError> {
        self.ensure_active()?;
        let mut domain = BTreeSet::new();
        for coordinate in neighbours {
            if coordinate == self.request.target {
                return Err(SessionError::DuplicatePacketNeighbour(coordinate));
            }
            if !self.halo.contains(coordinate) {
                return Err(SessionError::PacketNeighbourOutsideHalo(coordinate));
            }
            if !domain.insert(coordinate) {
                return Err(SessionError::DuplicatePacketNeighbour(coordinate));
            }
        }
        self.packet_neighbour_domain = Some(domain);
        Ok(())
    }

    #[must_use]
    pub fn packet_neighbour_domain(&self) -> Option<&BTreeSet<ChunkCoordinate>> {
        self.packet_neighbour_domain.as_ref()
    }

    /// Export committed state for a scheduler-owned cache or a later session.
    #[must_use]
    pub fn export_checkpoint(&self) -> GenerationCheckpoint {
        GenerationCheckpoint {
            request: self.request,
            pipeline_identity: self.pipeline.identity(PipelineOptions::ALL),
            frontiers: self
                .frontiers
                .iter()
                .map(|(coordinate, frontier)| (*coordinate, frontier.records().to_vec()))
                .collect(),
            products: self
                .products
                .iter()
                .map(|(key, product)| (*key, product.clone()))
                .collect(),
            sidecars: self
                .sidecars
                .iter()
                .map(|(key, sidecar)| (*key, sidecar.clone()))
                .collect(),
            aggregates: self
                .aggregates
                .iter()
                .map(|(coordinate, aggregate)| (*coordinate, aggregate.clone()))
                .collect(),
            committed_mutations: self.committed_mutations.values().cloned().collect(),
            committed_mutation_order: self.committed_mutation_order.clone(),
            source_completions: self.committed_source_completions.clone(),
            feature_settlement: self.feature_settlement,
            feature_winner_receipts: self.feature_winner_receipts.values().copied().collect(),
            current_revision: self.current_revision,
            packet_neighbour_domain: self.packet_neighbour_domain.clone(),
        }
    }

    /// Restore a session from a checkpoint after validating request, pipeline,
    /// frontier order, and every retained product and sidecar declaration.
    pub fn from_checkpoint(checkpoint: GenerationCheckpoint) -> Result<Self, SessionError> {
        Self::from_checkpoint_with_budget(checkpoint, SessionBudget::DEFAULT)
    }

    pub fn from_checkpoint_with_budget(
        checkpoint: GenerationCheckpoint,
        budget: SessionBudget,
    ) -> Result<Self, SessionError> {
        Self::from_checkpoint_with_budget_and_cancellation(
            checkpoint,
            budget,
            RequestCancellation::new(),
        )
    }

    pub(crate) fn from_checkpoint_with_budget_and_cancellation(
        checkpoint: GenerationCheckpoint,
        budget: SessionBudget,
        cancellation: RequestCancellation,
    ) -> Result<Self, SessionError> {
        let mut session = Self::with_budget_and_cancellation(checkpoint.request, budget, cancellation);
        if checkpoint.pipeline_identity != session.pipeline.identity(PipelineOptions::ALL)
            || checkpoint.request != session.request
        {
            return Err(if checkpoint.request != session.request {
                SessionError::CheckpointRequestMismatch
            } else {
                SessionError::CheckpointPipelineMismatch
            });
        }
        if checkpoint
            .feature_settlement
            .is_some_and(|proof| proof.output() != session.request.target)
        {
            return Err(SessionError::InvalidCheckpoint);
        }
        session.feature_settlement = checkpoint.feature_settlement;
        if !checkpoint.feature_winner_receipts.is_empty() {
            let Some(proof) = session.feature_settlement else {
                return Err(SessionError::InvalidCheckpoint);
            };
            let stage = StageKey::new(session.pipeline.dimension(), ColumnStage::Features);
            let descriptor = session.descriptor(stage)?;
            for receipt in checkpoint.feature_winner_receipts {
                let destination = receipt.destination();
                if receipt.owner() != session.request.target
                    || !proof.contains(receipt.source())
                    || (destination.x().div_euclid(16), destination.z().div_euclid(16))
                        != session.request.target
                    || !descriptor.mutable_write_radius().contains_offset(
                        session.request.target.0 - receipt.source().0,
                        session.request.target.1 - receipt.source().1,
                    )
                    || session
                        .feature_winner_receipts
                        .insert(destination, receipt)
                        .is_some()
                {
                    return Err(SessionError::InvalidCheckpoint);
                }
            }
            let retained_bytes = session
                .feature_winner_receipts
                .len()
                .saturating_mul(size_of::<TargetFeatureWrite>());
            session.check_usage(
                0,
                0,
                session.feature_winner_receipts.len(),
                retained_bytes,
            )?;
            session.add_usage(0, 0, session.feature_winner_receipts.len(), retained_bytes);
        }
        let mut restored_frontiers = BTreeMap::new();
        for (coordinate, records) in checkpoint.frontiers {
            if !session.halo.contains(coordinate)
                || restored_frontiers
                    .insert(
                        coordinate,
                        StageFrontier::from_records_with_options(
                            session.pipeline.schedule(),
                            coordinate,
                            lodestone_worldgen::stage_schedule::STAGE_SCHEDULE_VERSION,
                            PipelineOptions::ALL,
                            records,
                        )
                        .map_err(|_| SessionError::InvalidCheckpoint)?,
                    )
                    .is_some()
            {
                return Err(SessionError::InvalidCheckpoint);
            }
        }
        if restored_frontiers.len() != session.frontiers.len()
            || session
                .frontiers
                .keys()
                .any(|coordinate| !restored_frontiers.contains_key(coordinate))
        {
            return Err(SessionError::InvalidCheckpoint);
        }
        session.frontiers = restored_frontiers;
        for (coordinate, aggregate) in checkpoint.aggregates {
            let shaped_prefix = session
                .pipeline
                .schedule()
                .stages_for(GenerationTarget::Shaped);
            let frontier_valid = session.frontiers.get(&coordinate).is_some_and(|frontier| {
                frontier.records().len() >= shaped_prefix.len()
                    && shaped_prefix.iter().enumerate().all(|(index, stage)| {
                        let record = &frontier.records()[index];
                        record.key()
                            == StageKey::new(session.pipeline.dimension(), *stage)
                            && record.input_fingerprint() == aggregate.input_fingerprint()
                            && record.output_fingerprint() == aggregate.output_fingerprint()
                            && record.executor_version() == aggregate.executor_version()
                    })
            });
            if !session.halo.contains(coordinate)
                || aggregate.coordinate() != coordinate
                || aggregate.boundary()
                    != session
                        .pipeline
                        .schedule()
                        .target_stage(GenerationTarget::Shaped)
                || aggregate.product().resource() != ResourceKey::MaterializedWorld
                || (aggregate.product().get::<ChunkColumn>().is_none()
                    && aggregate
                        .product()
                        .get::<lodestone_worldgen::overworld::GeneratedColumn>()
                        .is_none())
                || !frontier_valid
                || session.aggregates.insert(coordinate, aggregate).is_some()
            {
                return Err(SessionError::InvalidCheckpoint);
            }
        }
        for (key, product) in checkpoint.products {
            let Some(frontier) = session.frontiers.get(&key.coordinate()) else {
                return Err(SessionError::InvalidCheckpoint);
            };
            let Some(descriptor) = session.pipeline.descriptor(key.stage().stage()) else {
                return Err(SessionError::InvalidCheckpoint);
            };
            if !frontier.records().iter().any(|record| record.key() == key.stage())
                || !descriptor.outputs().contains(&key.resource())
                || session.products.insert(key, product).is_some()
            {
                return Err(SessionError::InvalidCheckpoint);
            }
        }
        for (key, sidecar) in checkpoint.sidecars {
            let Some(frontier) = session.frontiers.get(&key.coordinate()) else {
                return Err(SessionError::InvalidCheckpoint);
            };
            let Some(descriptor) = session.pipeline.descriptor(key.stage().stage()) else {
                return Err(SessionError::InvalidCheckpoint);
            };
            if !frontier.records().iter().any(|record| record.key() == key.stage())
                || !descriptor.retained_sidecars().contains(&key.sidecar())
                || session.sidecars.insert(key, sidecar).is_some()
            {
                return Err(SessionError::InvalidCheckpoint);
            }
        }
        for (coordinate, frontier) in &session.frontiers {
            for record in frontier.records() {
                let Some(descriptor) = session.pipeline.descriptor(record.key().stage()) else {
                    return Err(SessionError::InvalidCheckpoint);
                };
                for &resource in descriptor.outputs() {
                    let aggregate_covered = session
                        .aggregates
                        .get(coordinate)
                        .is_some_and(|aggregate| {
                            session
                                .pipeline
                                .schedule()
                                .index_of(aggregate.boundary())
                                .zip(session.pipeline.schedule().index_of(record.key().stage()))
                                .is_some_and(|(boundary, stage)| stage <= boundary)
                        });
                    if !aggregate_covered && !session.products.contains_key(&ProductKey::new(
                        *coordinate,
                        record.key(),
                        resource,
                    )) {
                        return Err(SessionError::InvalidCheckpoint);
                    }
                }
                for &sidecar in descriptor.retained_sidecars() {
                    if !session.sidecars.contains_key(&SidecarProductKey::new(
                        *coordinate,
                        record.key(),
                        sidecar,
                    )) {
                        return Err(SessionError::InvalidCheckpoint);
                    }
                }
            }
        }
        let mut source_completions = checkpoint.source_completions;
        source_completions.sort_unstable();
        let mut source_by_stage = BTreeMap::<StageKey, BTreeMap<u64, ChunkCoordinate>>::new();
        for completion in &source_completions {
            let Some(descriptor) = session.pipeline.descriptor(completion.stage.stage()) else {
                return Err(SessionError::InvalidCheckpoint);
            };
            if completion.stage.dimension() != session.pipeline.dimension()
                || descriptor.barrier() != BarrierPolicy::SourceOrdered
                || !session.halo.contains(completion.source)
            {
                return Err(SessionError::InvalidCheckpoint);
            }
            let target_frontier = session
                .frontiers
                .get(&session.request.target)
                .expect("the target is always in its own halo");
            let stage_is_committed = target_frontier
                .records()
                .iter()
                .any(|record| record.key() == completion.stage);
            let stage_is_next = target_frontier.next_stage() == Some(completion.stage);
            if !stage_is_committed && !stage_is_next {
                return Err(SessionError::InvalidCheckpoint);
            }
            let by_order = source_by_stage.entry(completion.stage).or_default();
            if by_order.insert(completion.source_order, completion.source).is_some()
                || by_order.values().filter(|source| **source == completion.source).count() > 1
            {
                return Err(SessionError::InvalidCheckpoint);
            }
        }
        let mut active_source_stage = None;
        for (&stage, by_order) in &source_by_stage {
            if by_order.keys().copied().enumerate().any(|(index, order)| {
                u64::try_from(index).ok() != Some(order)
            }) {
                return Err(SessionError::InvalidCheckpoint);
            }
            let target_frontier = session
                .frontiers
                .get(&session.request.target)
                .expect("the target is always in its own halo");
            if target_frontier.next_stage() == Some(stage) {
                if active_source_stage.replace(stage).is_some() {
                    return Err(SessionError::InvalidCheckpoint);
                }
                session.hydrated_source_completions.extend(
                    by_order
                        .iter()
                        .map(|(&order, &source)| ((stage, order), source)),
                );
            }
        }
        session.committed_source_completions = source_completions;

        let mutations = checkpoint.committed_mutations;
        let features_stage = StageKey::new(session.pipeline.dimension(), ColumnStage::Features);
        for mutation in mutations {
            let provenance = mutation.provenance();
            let Some(descriptor) = session.pipeline.descriptor(provenance.stage().stage()) else {
                return Err(SessionError::InvalidCheckpoint);
            };
            let destination = provenance.destination();
            let destination_chunk = (
                destination.x().div_euclid(16),
                destination.z().div_euclid(16),
            );
            let target_frontier = session
                .frontiers
                .get(&session.request.target)
                .expect("the target is always in its own halo");
            let stage_is_committed = target_frontier
                .records()
                .iter()
                .any(|record| record.key() == provenance.stage());
            let stage_is_active = active_source_stage == Some(provenance.stage());
            let foreign_target_overlay = provenance.target() != session.request.target;
            if provenance.stage().dimension() != session.pipeline.dimension()
                || !session.halo.contains(destination_chunk)
                || descriptor.barrier() != BarrierPolicy::SourceOrdered
                || !descriptor.mutable_write_radius().contains_offset(
                    destination_chunk.0 - provenance.source().0,
                    destination_chunk.1 - provenance.source().1,
                )
                || (!foreign_target_overlay
                    && !session.halo.contains(provenance.source()))
                || (!foreign_target_overlay
                    && !stage_is_committed
                    && !stage_is_active)
            {
                return Err(SessionError::InvalidCheckpoint);
            }
            if session.mutation_index.contains(&provenance)
                || session.committed_mutations.contains_key(&provenance)
            {
                return Err(SessionError::InvalidCheckpoint);
            }
            session.has_foreign_feature_mutations |=
                provenance.stage() == features_stage && foreign_target_overlay;
            session.committed_mutations.insert(provenance, mutation);
            session.mutation_index.insert(provenance);
        }
        session.committed_mutation_order = checkpoint.committed_mutation_order;
        let ordered_mutations = session
            .committed_mutation_order
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if ordered_mutations.len() != session.committed_mutation_order.len()
            || ordered_mutations.len() != session.committed_mutations.len()
            || session
                .committed_mutation_order
                .iter()
                .any(|provenance| !session.committed_mutations.contains_key(provenance))
        {
            return Err(SessionError::InvalidCheckpoint);
        }
        if let Some(proof) = session.feature_settlement {
            let features_stage =
                StageKey::new(session.pipeline.dimension(), ColumnStage::Features);
            let output_stage = StageKey::new(session.pipeline.dimension(), ColumnStage::Output);
            let output_key = ProductKey::new(
                session.request.target,
                output_stage,
                ResourceKey::OutputColumn,
            );
            let radius = i32::from(
                session
                    .descriptor(features_stage)?
                    .mutable_write_radius()
                    .chunks_value(),
            );
            let output_frontier = session
                .frontiers
                .get(&session.request.target)
                .is_some_and(|frontier| {
                    frontier
                        .records()
                        .iter()
                        .any(|record| record.key() == output_stage)
                });
            let output_product = session
                .products
                .get(&output_key)
                .and_then(|product| product.get::<ChunkColumn>())
                .is_some();
            if !proof.matches_square(session.request.target, radius)
                || output_frontier != output_product
                || !session
                    .frontiers
                    .get(&session.request.target)
                    .is_some_and(|frontier| {
                        frontier
                            .records()
                            .iter()
                            .any(|record| record.key() == features_stage)
                    })
            {
                return Err(SessionError::InvalidCheckpoint);
            }
        }
        if let Some(domain) = &checkpoint.packet_neighbour_domain {
            if domain
                .iter()
                .any(|coordinate| *coordinate == session.request.target
                    || !session.halo.contains(*coordinate))
            {
                return Err(SessionError::InvalidCheckpoint);
            }
        }
        session.packet_neighbour_domain = checkpoint.packet_neighbour_domain;
        let mutation_revision = session
            .committed_mutations
            .keys()
            .map(|provenance| provenance.revision().value())
            .max()
            .unwrap_or(0);
        session.current_revision = SessionRevision(
            checkpoint
                .current_revision
                .value()
                .max(mutation_revision),
        );
        session.next_revision = session.current_revision.0.saturating_add(1);
        let aggregate_entries = session.aggregates.len();
        let aggregate_bytes = session
            .aggregates
            .values()
            .map(|aggregate| aggregate.product().retained_bytes())
            .fold(0usize, usize::saturating_add);
        let products = session.products.len().saturating_add(aggregate_entries);
        let bytes = session
            .products
            .values()
            .map(|product| product.retained_bytes())
            .fold(aggregate_bytes, usize::saturating_add);
        let (sidecar_entries, bytes) = session
            .sidecars
            .values()
            .map(|sidecar| sidecar.retained_bytes())
            .fold((0usize, bytes), |(entries, bytes), size| {
                (entries.saturating_add(1), bytes.saturating_add(size))
            });
        let (mutations, bytes) = session
            .committed_mutations
            .values()
            .map(ProvenanceMutation::retained_bytes)
            .fold((0usize, bytes), |(entries, bytes), size| {
                (entries.saturating_add(1), bytes.saturating_add(size))
            });
        session.check_usage(products, sidecar_entries, mutations, bytes)?;
        session.usage = SessionUsage {
            product_entries: products,
            sidecar_entries,
            mutation_entries: mutations,
            retained_bytes: bytes,
        };
        Ok(session)
    }

    /// Cancel later work while retaining already committed reusable products.
    pub fn cancel(&mut self) {
        let (products, sidecars, retained_bytes) = self
            .pending_immutable
            .values()
            .map(Self::completion_usage)
            .fold((0usize, 0usize, 0usize), |(p, s, b), (np, ns, nb)| {
                (p.saturating_add(np), s.saturating_add(ns), b.saturating_add(nb))
            });
        let (mutations, mutation_bytes) = self
            .pending_mutable
            .values()
            .map(Self::transaction_usage)
            .fold((0usize, 0usize), |(m, b), (nm, nb)| {
                (m.saturating_add(nm), b.saturating_add(nb))
            });
        for transaction in self.pending_mutable.values() {
            for write in &transaction.writes {
                self.mutation_index.remove(&write.provenance());
            }
        }
        self.subtract_usage(
            products,
            sidecars,
            mutations,
            retained_bytes.saturating_add(mutation_bytes),
        );
        self.pending_immutable.clear();
        self.pending_mutable.clear();
        self.mutable_source_plan = None;
        self.committed_source_orders.clear();
        self.mutable_stage = None;
        self.cancellation.cancel();
        self.light_domain_revision = None;
    }

    /// Mark the admitted light domain complete for the current revision.
    pub fn complete_light_domain(&mut self) -> Result<SessionRevision, SessionError> {
        self.ensure_active()?;
        self.light_domain_revision = Some(self.current_revision);
        Ok(self.current_revision)
    }

    /// Detach a packet column and its neighbour columns after the requested
    /// target and light are ready.
    pub fn finalize_packet_snapshot(
        &self,
        column: ChunkColumn,
        neighbours: impl IntoIterator<Item = (ChunkCoordinate, ChunkColumn)>,
    ) -> Result<PacketSnapshot, SessionError> {
        self.finalize_packet_snapshot_through(
            Arc::new(column),
            neighbours
                .into_iter()
                .map(|(coordinate, column)| PacketNeighbour::materialized(coordinate, column)),
            GenerationTarget::Full,
        )
    }

    /// Detach a full target together with shaped dependency columns.
    ///
    /// A packet's radius-one light domain is an immutable input to its wire
    /// encoder, but those dependencies are not themselves packet-generation
    /// targets. This variant requires their authenticated shaped prefixes,
    /// while [`Self::finalize_packet_snapshot`] remains strict for callers
    /// that supply full neighbours.
    pub fn finalize_packet_snapshot_with_dependencies(
        &self,
        column: ChunkColumn,
        neighbours: impl IntoIterator<Item = (ChunkCoordinate, ChunkColumn)>,
    ) -> Result<PacketSnapshot, SessionError> {
        self.finalize_packet_snapshot_through(
            Arc::new(column),
            neighbours
                .into_iter()
                .map(|(coordinate, column)| PacketNeighbour::materialized(coordinate, column)),
            GenerationTarget::Shaped,
        )
    }

    /// Detach a packet using typed shaped neighbours.
    ///
    /// Generated neighbours stay compact until [`PacketNeighbour::column`]
    /// or [`PacketNeighbour::into_column`] is called by a consumer that
    /// actually needs the mutable server carrier.
    pub(crate) fn finalize_packet_snapshot_with_packet_neighbours(
        &self,
        column: Arc<ChunkColumn>,
        neighbours: impl IntoIterator<Item = PacketNeighbour>,
    ) -> Result<PacketSnapshot, SessionError> {
        self.finalize_packet_snapshot_through(column, neighbours, GenerationTarget::Shaped)
    }

    fn finalize_packet_snapshot_through(
        &self,
        column: Arc<ChunkColumn>,
        neighbours: impl IntoIterator<Item = PacketNeighbour>,
        neighbour_target: GenerationTarget,
    ) -> Result<PacketSnapshot, SessionError> {
        self.ensure_active()?;
        if self.request.generation_target() != GenerationTarget::Full {
            return Err(SessionError::PacketNotReady(
                "packet snapshots require the full generation target",
            ));
        }
        let frontier = self
            .frontiers
            .get(&self.request.target)
            .expect("the target is always in its own halo");
        let terminal = self
            .pipeline
            .schedule()
            .target_stage(self.request.generation_target());
        let neighbour_terminal = self.pipeline.schedule().target_stage(neighbour_target);
        if !frontier.is_complete_through(terminal) {
            return Err(SessionError::PacketNotReady("generation target is incomplete"));
        }
        let Some(output_descriptor) = self.pipeline.descriptor(terminal) else {
            return Err(SessionError::PacketNotReady("output stage is unavailable"));
        };
        for &resource in output_descriptor.outputs() {
            if !self.products.contains_key(&ProductKey::new(
                self.request.target,
                StageKey::new(self.pipeline.dimension(), terminal),
                resource,
            )) {
                return Err(SessionError::PacketNotReady("output product is missing"));
            }
        }
        for &sidecar in output_descriptor.retained_sidecars() {
            if !self.sidecars.contains_key(&SidecarProductKey::new(
                self.request.target,
                StageKey::new(self.pipeline.dimension(), terminal),
                sidecar,
            )) {
                return Err(SessionError::PacketNotReady("packet sidecar is missing"));
            }
        }
        if self.light_domain_revision != Some(self.current_revision) {
            return Err(SessionError::PacketNotReady("light is not settled for this revision"));
        }
        let Some(declared_domain) = self.packet_neighbour_domain.as_ref() else {
            return Err(SessionError::PacketNeighbourDomainNotDeclared);
        };
        let mut packet_neighbours = Vec::new();
        let mut supplied_domain = BTreeSet::new();
        for neighbour in neighbours {
            let coordinate = neighbour.coordinate();
            if coordinate == self.request.target
                || !supplied_domain.insert(coordinate)
            {
                return Err(SessionError::DuplicatePacketNeighbour(coordinate));
            }
            if !declared_domain.contains(&coordinate) {
                return Err(SessionError::PacketNeighbourDomainMismatch);
            }
            let Some(neighbour_frontier) = self.frontiers.get(&coordinate) else {
                return Err(SessionError::PacketNeighbourOutsideHalo(coordinate));
            };
            if !neighbour_frontier.is_complete_through(neighbour_terminal) {
                return Err(SessionError::PacketNeighbourNotReady { coordinate });
            }
            packet_neighbours.push(neighbour);
        }
        if supplied_domain != *declared_domain {
            return Err(SessionError::PacketNeighbourDomainMismatch);
        }
        self.ensure_active()?;
        Ok(PacketSnapshot {
            coordinate: self.request.target,
            target: self.request.generation_target(),
            revision: self.current_revision,
            column,
            neighbours: packet_neighbours,
            light_settlement: std::sync::OnceLock::new(),
        })
    }

    /// Convenience form for a target-only packet snapshot.
    pub fn finalize_packet(&self, column: ChunkColumn) -> Result<PacketSnapshot, SessionError> {
        self.finalize_packet_snapshot(column, [])
    }

    fn bump_revision(&mut self) {
        self.current_revision = SessionRevision(self.next_revision);
        self.next_revision += 1;
    }
}

fn pipeline_for(dimension: Dimension) -> DimensionPipeline {
    match dimension {
        Dimension::Overworld => OVERWORLD_PIPELINE,
        Dimension::Nether => NETHER_PIPELINE,
        Dimension::End => END_PIPELINE,
    }
}

#[cfg(test)]
mod foreign_feature_mutation_tests {
    use super::*;

    #[test]
    fn foreign_feature_index_is_derived_from_restored_mutations() {
        let request = GenerationRequest::new(
            Dimension::Overworld,
            (0, 0),
            GenerationTarget::Full,
            2,
        );
        let session = GenerationSession::new(request);
        assert!(!session.has_foreign_feature_mutations);

        let mut checkpoint = session.export_checkpoint();
        let stage = StageKey::new(Dimension::Overworld, ColumnStage::Features);
        let mutation = ProvenanceMutation::test_block_state(
            (1, 0),
            (1, 0),
            stage,
            0,
            BlockCoordinate::new(15, 0, 8),
            1,
            StateId::from_state_str("minecraft:stone").unwrap(),
        );
        checkpoint.committed_mutation_order.push(mutation.provenance());
        checkpoint.committed_mutations.push(mutation);

        let restored = GenerationSession::from_checkpoint(checkpoint).unwrap();
        assert!(restored.has_foreign_feature_mutations);
    }
}

#[cfg(test)]
mod packet_snapshot_tests {
    use super::*;
    use lodestone_world::{ColumnLight, LightData};

    #[test]
    fn generated_neighbour_cache_requires_matching_product_and_overlay() {
        crate::chunk::reset_generated_materializations();
        let generated = Arc::new(crate::overworld_generator(42).column_shaped(0, 0));
        let mut cache = BTreeMap::new();
        let stone = StateId::from_state_str("minecraft:stone").unwrap();
        let gold = StateId::from_state_str("minecraft:gold_block").unwrap();
        let first = PacketNeighbour::generated_with_id_overlay_cached(
            (0, 0),
            Arc::clone(&generated),
            vec![(0, -63, 0, stone)],
            &mut cache,
        );
        let same = PacketNeighbour::generated_with_id_overlay_cached(
            (0, 0),
            Arc::clone(&generated),
            vec![(0, -63, 0, stone)],
            &mut cache,
        );
        assert!(std::ptr::eq(first.column(), same.column()));
        assert_eq!(crate::chunk::generated_materializations(), 1);

        let changed_overlay = PacketNeighbour::generated_with_id_overlay_cached(
            (0, 0),
            Arc::clone(&generated),
            vec![(0, -63, 0, gold)],
            &mut cache,
        );
        assert!(!std::ptr::eq(first.column(), changed_overlay.column()));
        assert_eq!(first.column().block_state_id(0, -63, 0), stone);
        assert_eq!(changed_overlay.column().block_state_id(0, -63, 0), gold);
        assert_eq!(crate::chunk::generated_materializations(), 2);
        let mut detached = changed_overlay.clone().into_column();
        detached.set_block_id(0, -63, 0, StateId::AIR);
        assert_eq!(changed_overlay.column().block_state_id(0, -63, 0), gold);

        let different_product = PacketNeighbour::generated_with_id_overlay_cached(
            (0, 0),
            Arc::new(crate::overworld_generator(42).column_shaped(0, 0)),
            vec![(0, -63, 0, gold)],
            &mut cache,
        );
        assert!(!std::ptr::eq(changed_overlay.column(), different_product.column()));
        assert_eq!(changed_overlay.column().block_state_id(0, -63, 0), gold);
        assert_eq!(different_product.column().block_state_id(0, -63, 0), gold);
        assert_eq!(crate::chunk::generated_materializations(), 3);
    }

    #[test]
    fn shared_neighbours_detach_when_consumed_for_mutation() {
        let stone = StateId::from_state_str("minecraft:stone").unwrap();
        let mut column = ChunkColumn::new(0, 16);
        column.set_block_id(0, 0, 0, stone);
        column.prime_client_heightmaps();
        let first = PacketNeighbour::materialized((0, 0), column);
        let second = first.clone();
        assert!(std::ptr::eq(first.column(), second.column()));
        let original_maps = first.column().client_heightmaps_raw();

        let mut detached = second.into_column();
        detached.set_block_id(0, 0, 0, StateId::AIR);
        assert_eq!(detached.block_state_id(0, 0, 0), StateId::AIR);
        assert_eq!(first.column().block_state_id(0, 0, 0), stone);
        assert_eq!(first.column().client_heightmaps_raw(), original_maps);
    }

    #[test]
    fn packet_light_product_is_absent_until_prepared_and_cannot_be_replaced() {
        let snapshot = PacketSnapshot {
            coordinate: (0, 0),
            target: GenerationTarget::Full,
            revision: SessionRevision(1),
            column: Arc::new(ChunkColumn::new(0, 16)),
            neighbours: Vec::new(),
            light_settlement: std::sync::OnceLock::new(),
        };
        assert!(!snapshot.is_light_settled());

        let mut light = ColumnLight::new(1);
        *light.sky_mut(0) = LightData::Uniform(7);
        let settlement = ColumnLightSettlement::centre(light);
        assert!(snapshot.install_light_settlement(settlement).is_ok());
        assert!(snapshot.is_light_settled());
        assert_eq!(
            snapshot
                .light_settlement()
                .unwrap()
                .centre_light()
                .sky(0),
            &LightData::Uniform(7)
        );

        let replacement = ColumnLightSettlement::centre(ColumnLight::new(1));
        assert!(snapshot.install_light_settlement(replacement).is_err());
        assert_eq!(
            snapshot
                .light_settlement()
                .unwrap()
                .centre_light()
                .sky(0),
            &LightData::Uniform(7)
        );
    }
}
