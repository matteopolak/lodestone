use std::sync::Arc;

use lodestone_data::block_states::StateId;
use lodestone_worldgen::overworld::{
    DirectDecorationResult, MixedReplayContext, OverworldGenerator, RegionFeatureEpoch,
    SparseDirectDecorationResult,
};

use crate::worldgen_lifecycle::{ChunkPos, LifecycleCompletionMode};

pub struct TargetFeatureKernel {
    generator: Arc<OverworldGenerator>,
    context: Arc<MixedReplayContext>,
    target: ChunkPos,
    mode: LifecycleCompletionMode,
}

impl std::fmt::Debug for TargetFeatureKernel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("TargetFeatureKernel")
            .field("target", &self.target).field("mode", &self.mode).finish_non_exhaustive()
    }
}

impl TargetFeatureKernel {
    pub(crate) fn new(
        generator: Arc<OverworldGenerator>,
        context: Arc<MixedReplayContext>,
        target: ChunkPos,
        mode: LifecycleCompletionMode,
    ) -> Self {
        Self { generator, context, target, mode }
    }

    pub(crate) fn with_state(
        self,
        epoch: RegionFeatureEpoch,
        revisions: Vec<((i32, i32, i32), StateId)>,
    ) -> TargetFeatureWork {
        TargetFeatureWork { kernel: self, epoch, revisions }
    }
}

pub(crate) struct TargetFeatureWork {
    kernel: TargetFeatureKernel,
    epoch: RegionFeatureEpoch,
    revisions: Vec<((i32, i32, i32), StateId)>,
}

#[derive(Debug)]
pub enum TargetFeatureOutput {
    Full(DirectDecorationResult),
    Sparse(SparseDirectDecorationResult),
}

#[cfg(test)]
pub(crate) fn test_work(mode: LifecycleCompletionMode) -> (TargetFeatureWork, usize) {
    let source = crate::worldgen_data::overworld_chunk_source(42);
    let generator = source.generator_arc();
    let target = (0, 0);
    let context = generator.lifecycle_replay_context(target.0, target.1);
    let epoch = generator.begin_region_feature_epoch_from_context(&context, &[target]);
    let mut revisions = Vec::with_capacity(8);
    revisions.push(((3, 67, 3), lodestone_data::block::Block::GoldBlock.default_state()));
    let allocation = revisions.as_ptr() as usize;
    (TargetFeatureKernel::new(generator, context, target, mode).with_state(epoch, revisions), allocation)
}

pub(crate) struct TargetFeatureCompletion {
    pub(crate) target: ChunkPos,
    pub(crate) mode: LifecycleCompletionMode,
    pub(crate) epoch: RegionFeatureEpoch,
    pub(crate) revisions: Vec<((i32, i32, i32), StateId)>,
    pub(crate) output: TargetFeatureOutput,
}

impl TargetFeatureWork {
    pub(crate) fn run(self) -> TargetFeatureCompletion {
        let Self { kernel, mut epoch, revisions } = self;
        let output = match kernel.mode {
            LifecycleCompletionMode::Full => TargetFeatureOutput::Full(
                kernel.generator.complete_region_feature_epoch_target_from_context_with_override_events(
                    &mut epoch, kernel.target, &kernel.context, &revisions,
                ),
            ),
            LifecycleCompletionMode::SparsePadding => TargetFeatureOutput::Sparse(
                kernel.generator.complete_region_feature_epoch_target_sparse_from_context_with_override_events(
                    &mut epoch, kernel.target, &kernel.context, &revisions,
                ),
            ),
        };
        TargetFeatureCompletion {
            target: kernel.target, mode: kernel.mode, epoch, revisions, output,
        }
    }
}
