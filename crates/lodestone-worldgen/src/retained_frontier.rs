//! Typed products retained at an incremental world-generation boundary.
//!
//! [`crate::stage_schedule::StageFrontier`] records which stages are complete,
//! but it deliberately does not own the values those stages produced. This
//! module supplies the small ownership seam needed by a far-chunk cache: a
//! source can retain the typed products and sidecars next to the frontier,
//! then ask the same ordered executor to continue from the first missing
//! stage. The payload values are type-erased only at the storage boundary;
//! callers insert and retrieve them through generic typed methods.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::stage_schedule::{
    Dimension, FrontierError, GenerationTarget, ResourceKey, StageDescriptor, StageFrontier,
    StageKey, SidecarKey,
};

type ErasedPayload = Arc<dyn Any + Send + Sync>;

/// A product produced by one stage.
///
/// The key is checked against the stage descriptor before the stage is
/// committed. The value remains typed for callers; only the internal map uses
/// [`Any`] so one frontier can retain products with different concrete types.
#[derive(Clone)]
pub struct TypedProduct {
    key: ResourceKey,
    value: ErasedPayload,
}

impl TypedProduct {
    /// Retain `value` under the descriptor's typed product key.
    #[must_use]
    pub fn new<T>(key: ResourceKey, value: T) -> Self
    where
        T: Any + Send + Sync,
    {
        Self {
            key,
            value: Arc::new(value),
        }
    }

    #[must_use]
    pub const fn key(&self) -> ResourceKey {
        self.key
    }

    /// Downcast this retained value without cloning the underlying product.
    #[must_use]
    pub fn downcast<T>(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        Arc::downcast::<T>(Arc::clone(&self.value)).ok()
    }
}

impl fmt::Debug for TypedProduct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypedProduct")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// A sidecar produced by one stage.
///
/// Sidecars are kept separate from products because packet and lifecycle
/// consumers often need to retain metadata even when the main world column is
/// compacted. They use the same typed insertion and retrieval boundary.
#[derive(Clone)]
pub struct TypedSidecar {
    key: SidecarKey,
    value: ErasedPayload,
}

impl TypedSidecar {
    /// Retain `value` under the descriptor's typed sidecar key.
    #[must_use]
    pub fn new<T>(key: SidecarKey, value: T) -> Self
    where
        T: Any + Send + Sync,
    {
        Self {
            key,
            value: Arc::new(value),
        }
    }

    #[must_use]
    pub const fn key(&self) -> SidecarKey {
        self.key
    }

    /// Downcast this retained value without cloning the underlying sidecar.
    #[must_use]
    pub fn downcast<T>(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        Arc::downcast::<T>(Arc::clone(&self.value)).ok()
    }
}

impl fmt::Debug for TypedSidecar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypedSidecar")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// The products and sidecars returned by one stage execution.
#[derive(Debug, Clone)]
pub struct StageExecution {
    output_fingerprint: [u8; 32],
    products: Vec<TypedProduct>,
    sidecars: Vec<TypedSidecar>,
}

impl StageExecution {
    /// Starts an execution result with its externally computed output
    /// fingerprint. Products are added with [`Self::with_product`] and
    /// [`Self::with_sidecar`].
    #[must_use]
    pub const fn new(output_fingerprint: [u8; 32]) -> Self {
        Self {
            output_fingerprint,
            products: Vec::new(),
            sidecars: Vec::new(),
        }
    }

    /// Add one typed stage product.
    #[must_use]
    pub fn with_product(mut self, product: TypedProduct) -> Self {
        self.products.push(product);
        self
    }

    /// Add one typed stage sidecar.
    #[must_use]
    pub fn with_sidecar(mut self, sidecar: TypedSidecar) -> Self {
        self.sidecars.push(sidecar);
        self
    }

    #[must_use]
    pub const fn output_fingerprint(&self) -> [u8; 32] {
        self.output_fingerprint
    }

    #[must_use]
    pub fn products(&self) -> &[TypedProduct] {
        &self.products
    }

    #[must_use]
    pub fn sidecars(&self) -> &[TypedSidecar] {
        &self.sidecars
    }
}

/// Why a stage execution could not be retained atomically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedFrontierError {
    /// The underlying ordered frontier rejected the stage record.
    Frontier(FrontierError),
    /// The schedule has no typed descriptor for this stage yet.
    MissingDescriptor(StageKey),
    /// A stage supplied the same product key more than once.
    DuplicateProduct { stage: StageKey, key: ResourceKey },
    /// A stage supplied a product that its descriptor does not declare.
    UnexpectedProduct { stage: StageKey, key: ResourceKey },
    /// A required product was not supplied by the executor.
    MissingProduct { stage: StageKey, key: ResourceKey },
    /// A stage supplied the same sidecar key more than once.
    DuplicateSidecar { stage: StageKey, key: SidecarKey },
    /// A stage supplied a sidecar that its descriptor does not declare.
    UnexpectedSidecar { stage: StageKey, key: SidecarKey },
    /// A required sidecar was not supplied by the executor.
    MissingSidecar { stage: StageKey, key: SidecarKey },
    /// The requested target is not represented by the selected schedule.
    UnsupportedTarget {
        dimension: Dimension,
        target: GenerationTarget,
    },
}

impl From<FrontierError> for RetainedFrontierError {
    fn from(error: FrontierError) -> Self {
        Self::Frontier(error)
    }
}

/// The result of advancing a retained frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvanceReport {
    /// Number of stage callbacks that ran during this call.
    pub executed_stages: usize,
    /// Whether a changed compatibility fingerprint discarded the old prefix.
    pub reset_for_fingerprint: bool,
}

/// A resumable, typed product store for one chunk.
///
/// The store is intentionally independent of a particular `ChunkColumn` type:
/// the worldgen crate can retain a compact terrain field, a mutable overlay,
/// a packet column, or a sidecar defined by a version crate. A source calls
/// [`Self::advance_to`] with its existing dimension executor; the callback is
/// invoked only for stages absent from this frontier.
#[derive(Clone)]
pub struct RetainedStageFrontier {
    frontier: StageFrontier,
    compatibility_fingerprint: [u8; 32],
    products: BTreeMap<ResourceKey, ErasedPayload>,
    sidecars: BTreeMap<SidecarKey, ErasedPayload>,
}

impl fmt::Debug for RetainedStageFrontier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedStageFrontier")
            .field("frontier", &self.frontier)
            .field("compatibility_fingerprint", &self.compatibility_fingerprint)
            .field("products", &self.products.keys().collect::<Vec<_>>())
            .field("sidecars", &self.sidecars.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl RetainedStageFrontier {
    /// Start an empty retained frontier for one chunk and pipeline
    /// configuration fingerprint.
    #[must_use]
    pub fn new(
        schedule: crate::stage_schedule::StageSchedule,
        coordinate: (i32, i32),
        compatibility_fingerprint: [u8; 32],
    ) -> Self {
        Self {
            frontier: StageFrontier::new(schedule, coordinate),
            compatibility_fingerprint,
            products: BTreeMap::new(),
            sidecars: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn frontier(&self) -> &StageFrontier {
        &self.frontier
    }

    #[must_use]
    pub const fn compatibility_fingerprint(&self) -> [u8; 32] {
        self.compatibility_fingerprint
    }

    /// Reset all retained state when the generator/options fingerprint
    /// changes. Returns `true` when a reset happened.
    pub fn reset_if_fingerprint_changed(&mut self, fingerprint: [u8; 32]) -> bool {
        if self.compatibility_fingerprint == fingerprint {
            return false;
        }
        self.compatibility_fingerprint = fingerprint;
        self.clear_retained_state();
        true
    }

    /// Drop the retained prefix and all products, as a cache eviction would.
    /// The compatibility fingerprint remains valid for the next generation.
    pub fn evict(&mut self) {
        self.clear_retained_state();
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frontier.records().is_empty()
    }

    /// Return a retained product if it has the requested concrete type.
    #[must_use]
    pub fn product<T>(&self, key: ResourceKey) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        self.products
            .get(&key)
            .and_then(|payload| Arc::downcast::<T>(Arc::clone(payload)).ok())
    }

    /// Return a retained sidecar if it has the requested concrete type.
    #[must_use]
    pub fn sidecar<T>(&self, key: SidecarKey) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        self.sidecars
            .get(&key)
            .and_then(|payload| Arc::downcast::<T>(Arc::clone(payload)).ok())
    }

    #[must_use]
    pub fn product_keys(&self) -> impl Iterator<Item = ResourceKey> + '_ {
        self.products.keys().copied()
    }

    #[must_use]
    pub fn sidecar_keys(&self) -> impl Iterator<Item = SidecarKey> + '_ {
        self.sidecars.keys().copied()
    }

    /// Advance through the requested public target, retaining each typed
    /// output atomically with its [`StageFrontier`] record.
    ///
    /// A changed compatibility fingerprint invalidates the old prefix before
    /// the first callback. If the target is already complete, no callback is
    /// invoked. The callback receives the descriptor and the retained values
    /// from all prior stages, so a real executor can continue from its compact
    /// prefix rather than regenerating it.
    pub fn advance_to(
        &mut self,
        target: GenerationTarget,
        compatibility_fingerprint: [u8; 32],
        executor_version: u32,
        mut execute: impl FnMut(StageDescriptor, &Self) -> StageExecution,
    ) -> Result<AdvanceReport, RetainedFrontierError> {
        let reset_for_fingerprint =
            self.reset_if_fingerprint_changed(compatibility_fingerprint);
        let terminal = self
            .frontier
            .schedule()
            .target_stage(target);
        let Some(terminal_index) = self.frontier.schedule().index_of(terminal) else {
            return Err(RetainedFrontierError::UnsupportedTarget {
                dimension: self.frontier.schedule().dimension(),
                target,
            });
        };
        let mut executed_stages = 0;

        while self.frontier.records().len() <= terminal_index {
            let key = self
                .frontier
                .next_stage()
                .expect("a target below the complete frontier has a next stage");
            let descriptor = self
                .frontier
                .schedule()
                .descriptor(key.stage())
                .ok_or(RetainedFrontierError::MissingDescriptor(key))?;
            let execution = execute(descriptor, self);
            self.validate_payloads(key, descriptor, &execution)?;

            self.frontier.commit_stage(
                key.stage(),
                compatibility_fingerprint,
                execution.output_fingerprint(),
                executor_version,
            )?;
            for product in execution.products {
                self.products.insert(product.key(), product.value);
            }
            for sidecar in execution.sidecars {
                self.sidecars.insert(sidecar.key(), sidecar.value);
            }
            executed_stages += 1;
        }

        Ok(AdvanceReport {
            executed_stages,
            reset_for_fingerprint,
        })
    }

    fn validate_payloads(
        &self,
        stage: StageKey,
        descriptor: StageDescriptor,
        execution: &StageExecution,
    ) -> Result<(), RetainedFrontierError> {
        for (index, product) in execution.products.iter().enumerate() {
            if execution.products[..index]
                .iter()
                .any(|previous| previous.key() == product.key())
            {
                return Err(RetainedFrontierError::DuplicateProduct {
                    stage,
                    key: product.key(),
                });
            }
            if !descriptor.outputs().contains(&product.key()) {
                return Err(RetainedFrontierError::UnexpectedProduct {
                    stage,
                    key: product.key(),
                });
            }
        }
        for &key in descriptor.outputs() {
            if !execution.products.iter().any(|product| product.key() == key) {
                return Err(RetainedFrontierError::MissingProduct { stage, key });
            }
        }
        for (index, sidecar) in execution.sidecars.iter().enumerate() {
            if execution.sidecars[..index]
                .iter()
                .any(|previous| previous.key() == sidecar.key())
            {
                return Err(RetainedFrontierError::DuplicateSidecar {
                    stage,
                    key: sidecar.key(),
                });
            }
            if !descriptor.retained_sidecars().contains(&sidecar.key()) {
                return Err(RetainedFrontierError::UnexpectedSidecar {
                    stage,
                    key: sidecar.key(),
                });
            }
        }
        for &key in descriptor.retained_sidecars() {
            if !execution
                .sidecars
                .iter()
                .any(|sidecar| sidecar.key() == key)
            {
                return Err(RetainedFrontierError::MissingSidecar { stage, key });
            }
        }
        Ok(())
    }

    fn clear_retained_state(&mut self) {
        self.frontier = StageFrontier::new(self.frontier.schedule(), self.frontier.coordinate());
        self.products.clear();
        self.sidecars.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{RetainedStageFrontier, StageExecution, TypedProduct, TypedSidecar};
    use crate::stage_schedule::{
        ColumnStage, GenerationTarget, ResourceKey, SidecarKey, StageDescriptor, END,
    };

    const PIPELINE: [u8; 32] = [0x11; 32];

    fn product_marker(key: ResourceKey) -> u8 {
        match key {
            ResourceKey::DensityField => 1,
            ResourceKey::BiomeQuarts => 2,
            ResourceKey::SurfaceDiff => 3,
            ResourceKey::MaterializedWorld => 4,
            ResourceKey::StructureStarts => 5,
            ResourceKey::StructureBlocks => 6,
            ResourceKey::ResidentRegion => 7,
            ResourceKey::ResidentOverlay => 8,
            ResourceKey::OutputColumn => 9,
        }
    }

    fn sidecar_marker(key: SidecarKey) -> u8 {
        match key {
            SidecarKey::StructureReferences => 1,
            SidecarKey::BlockEntityEvents => 2,
            SidecarKey::DecorationSpills => 3,
            SidecarKey::Gateways => 4,
            SidecarKey::ClientHeightmaps => 5,
            SidecarKey::SpawnCandidates => 6,
        }
    }

    fn execute_fixture(descriptor: StageDescriptor) -> StageExecution {
        let stage = descriptor.key().stage() as u8;
        let mut execution = StageExecution::new([stage; 32]);
        for &key in descriptor.outputs() {
            execution = execution.with_product(TypedProduct::new(
                key,
                vec![product_marker(key), stage],
            ));
        }
        for &key in descriptor.retained_sidecars() {
            execution = execution.with_sidecar(TypedSidecar::new(
                key,
                vec![sidecar_marker(key), stage],
            ));
        }
        execution
    }

    fn assert_payloads_equal(left: &RetainedStageFrontier, right: &RetainedStageFrontier) {
        assert_eq!(
            left.product_keys().collect::<Vec<_>>(),
            right.product_keys().collect::<Vec<_>>()
        );
        for key in left.product_keys() {
            assert_eq!(
                left.product::<Vec<u8>>(key),
                right.product::<Vec<u8>>(key),
                "product {:?} differs",
                key
            );
        }
        assert_eq!(
            left.sidecar_keys().collect::<Vec<_>>(),
            right.sidecar_keys().collect::<Vec<_>>()
        );
        for key in left.sidecar_keys() {
            assert_eq!(
                left.sidecar::<Vec<u8>>(key),
                right.sidecar::<Vec<u8>>(key),
                "sidecar {:?} differs",
                key
            );
        }
    }

    #[test]
    fn shaped_then_full_reuses_prefix_and_matches_cold_full_with_sidecars() {
        let mut cold = RetainedStageFrontier::new(END, (4, -9), PIPELINE);
        let mut cold_calls = Vec::new();
        cold.advance_to(GenerationTarget::Full, PIPELINE, 26, |descriptor, _| {
            cold_calls.push(descriptor.key().stage());
            execute_fixture(descriptor)
        })
        .expect("cold full generation");

        let mut incremental = RetainedStageFrontier::new(END, (4, -9), PIPELINE);
        let mut incremental_calls = Vec::new();
        incremental
            .advance_to(GenerationTarget::Shaped, PIPELINE, 26, |descriptor, _| {
                incremental_calls.push(descriptor.key().stage());
                execute_fixture(descriptor)
            })
            .expect("shaped prefix generation");
        let shaped_call_count = incremental_calls.len();
        incremental
            .advance_to(GenerationTarget::Full, PIPELINE, 26, |descriptor, _| {
                incremental_calls.push(descriptor.key().stage());
                execute_fixture(descriptor)
            })
            .expect("full upgrade");

        assert_eq!(incremental_calls, cold_calls);
        assert_eq!(shaped_call_count, END.shaped_boundary_index());
        assert_payloads_equal(&cold, &incremental);
        assert_eq!(cold.frontier().records(), incremental.frontier().records());
    }

    #[test]
    fn repeated_full_is_a_cache_hit_and_invalidation_restarts_the_prefix() {
        let mut retained = RetainedStageFrontier::new(END, (0, 0), PIPELINE);
        let mut calls = 0;
        retained
            .advance_to(GenerationTarget::Full, PIPELINE, 26, |descriptor, _| {
                calls += 1;
                execute_fixture(descriptor)
            })
            .expect("initial full generation");
        let complete_calls = calls;

        let report = retained
            .advance_to(GenerationTarget::Full, PIPELINE, 26, |descriptor, _| {
                calls += 1;
                execute_fixture(descriptor)
            })
            .expect("repeated full request");
        assert_eq!(report.executed_stages, 0);
        assert!(!report.reset_for_fingerprint);
        assert_eq!(calls, complete_calls);

        let changed = [0x22; 32];
        let report = retained
            .advance_to(GenerationTarget::Full, changed, 26, |descriptor, _| {
                calls += 1;
                execute_fixture(descriptor)
            })
            .expect("changed pipeline fingerprint");
        assert!(report.reset_for_fingerprint);
        assert_eq!(report.executed_stages, complete_calls);
        assert_eq!(calls, complete_calls * 2);

        retained.evict();
        let report = retained
            .advance_to(GenerationTarget::Shaped, changed, 26, |descriptor, _| {
                calls += 1;
                execute_fixture(descriptor)
            })
            .expect("evicted shaped prefix");
        assert_eq!(report.executed_stages, END.shaped_boundary_index());
        assert!(!report.reset_for_fingerprint);
        assert_eq!(calls, complete_calls * 2 + END.shaped_boundary_index());
        assert!(!retained.is_empty());
    }

    #[test]
    fn missing_payload_is_rejected_without_advancing_the_frontier() {
        let mut retained = RetainedStageFrontier::new(END, (0, 0), PIPELINE);
        let error = retained
            .advance_to(GenerationTarget::Shaped, PIPELINE, 26, |descriptor, _| {
                let stage = descriptor.key().stage();
                assert_eq!(stage, ColumnStage::Fill);
                StageExecution::new([stage as u8; 32])
            })
            .expect_err("missing required product must reject");
        assert!(matches!(
            error,
            super::RetainedFrontierError::MissingProduct {
                stage: _,
                key: ResourceKey::DensityField
            }
        ));
        assert!(retained.is_empty());
    }
}
