use super::*;
use crate::worldgen_session::{GenerationRequestError, GenerationRequestResult};

#[cfg(any(target_arch = "wasm32", test))]
const COHORT_OCCUPIED_BUDGET: std::time::Duration = std::time::Duration::from_millis(1);
#[cfg(any(target_arch = "wasm32", test))]
const COHORT_OPERATION_BUDGET: u32 = 64;

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Default)]
struct CohortCooperationBudget {
    occupied: std::time::Duration,
    operations: u32,
}

#[cfg(any(target_arch = "wasm32", test))]
impl CohortCooperationBudget {
    fn complete_operation(&mut self, occupied: std::time::Duration) -> bool {
        self.occupied = self.occupied.saturating_add(occupied);
        self.operations = self.operations.saturating_add(1);
        self.occupied >= COHORT_OCCUPIED_BUDGET || self.operations >= COHORT_OPERATION_BUDGET
    }

    fn reset(&mut self) {
        self.occupied = std::time::Duration::ZERO;
        self.operations = 0;
    }
}

#[cfg(any(target_arch = "wasm32", test))]
async fn cooperate_after_cohort_operation<C, Y>(
    budget: &mut CohortCooperationBudget,
    started: lodestone_time::Instant,
    cooperate: &mut C,
) -> lodestone_time::Instant
where
    C: FnMut() -> Y,
    Y: std::future::Future<Output = ()>,
{
    let completed = lodestone_time::Instant::now();
    if budget.complete_operation(completed.duration_since(started)) {
        cooperate().await;
        budget.reset();
        lodestone_time::Instant::now()
    } else {
        completed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CohortAction {
    MutableSession { target: ChunkCoordinate, index: usize },
    SparseOwner { target: ChunkCoordinate, sequence: usize },
    FinalizeOutput { coordinate: ChunkCoordinate, index: usize },
    Done,
}

#[derive(Clone, Copy)]
enum OwnerPhase {
    Mutable,
    Sparse,
    Outputs,
}

struct TargetOwnedCohortCursor {
    plan: TargetSettlementPlan,
    requested_outputs: BTreeMap<ChunkCoordinate, usize>,
    next_owner: usize,
    owner_phase: OwnerPhase,
    active_session: Option<usize>,
    next_output: usize,
    next_output_slot: usize,
    outcomes: Vec<Option<Result<(), GenerationRequestError>>>,
    packet_columns: BTreeMap<ChunkCoordinate, Arc<ChunkColumn>>,
    generated_packet_columns: BTreeMap<ChunkCoordinate, PacketNeighbour>,
}

impl TargetOwnedCohortCursor {
    fn new(plan: TargetSettlementPlan, sessions: &[GenerationSession]) -> Self {
        let requested_outputs = plan.outputs.iter().enumerate()
            .map(|(index, output)| (output.coordinate, index))
            .collect();
        let outcomes = sessions.iter()
            .map(|session| {
                session.cancellation().is_cancelled()
                    .then(|| Err(SessionError::Cancelled.into()))
            })
            .collect();
        Self {
            plan,
            requested_outputs,
            next_owner: 0,
            owner_phase: OwnerPhase::Mutable,
            active_session: None,
            next_output: 0,
            next_output_slot: 0,
            outcomes,
            packet_columns: BTreeMap::new(),
            generated_packet_columns: BTreeMap::new(),
        }
    }

    fn next_action(&mut self, sessions: &[GenerationSession]) -> CohortAction {
        while let Some(&target) = self.plan.targets.get(self.next_owner) {
            match self.owner_phase {
                OwnerPhase::Mutable => {
                    self.active_session = self.requested_outputs.get(&target)
                        .and_then(|&output_index| {
                            self.plan.outputs[output_index].session_indices.iter().copied().find(|&index| {
                                self.outcomes[index].is_none()
                                    && !sessions[index].cancellation().is_cancelled()
                            })
                        });
                    self.owner_phase = OwnerPhase::Sparse;
                    if let Some(index) = self.active_session {
                        return CohortAction::MutableSession { target, index };
                    }
                }
                OwnerPhase::Sparse => {
                    self.owner_phase = OwnerPhase::Outputs;
                    let needs_sparse = self.active_session.is_none()
                        || self.requested_outputs.get(&target).is_some_and(|&output_index| {
                            self.plan.outputs[output_index].session_indices.iter().all(|&index| {
                                self.outcomes[index].is_some()
                                    || sessions[index].cancellation().is_cancelled()
                            })
                        });
                    if needs_sparse && settlement_padding_needed_by_live_target(target, sessions) {
                        return CohortAction::SparseOwner {
                            target,
                            sequence: self.next_owner,
                        };
                    }
                }
                OwnerPhase::Outputs => {
                    while let Some(output) = self.plan.outputs.get(self.next_output)
                        .filter(|output| output.last_packet_owner_index <= self.next_owner)
                    {
                        while let Some(&index) = output.session_indices.get(self.next_output_slot) {
                            self.next_output_slot += 1;
                            if self.outcomes[index].is_some() {
                                continue;
                            }
                            if sessions[index].cancellation().is_cancelled() {
                                self.outcomes[index] = Some(Err(SessionError::Cancelled.into()));
                                continue;
                            }
                            return CohortAction::FinalizeOutput {
                                coordinate: output.coordinate,
                                index,
                            };
                        }
                        self.next_output += 1;
                        self.next_output_slot = 0;
                    }
                    self.next_owner += 1;
                    self.owner_phase = OwnerPhase::Mutable;
                }
            }
        }
        CohortAction::Done
    }

    fn into_outcomes(self) -> Result<Vec<Result<(), GenerationRequestError>>, GenerationRequestError> {
        if self.next_output != self.plan.outputs.len() || self.outcomes.iter().any(Option::is_none) {
            return Err(SessionError::InvalidCheckpointAt(
                "cohort ended before every output fence settled",
            ).into());
        }
        Ok(self.outcomes.into_iter()
            .map(|outcome| outcome.expect("all cohort outputs have passed their writer fence"))
            .collect())
    }
}

#[derive(Clone, Copy)]
enum CohortGoal {
    Mutable,
    Packet,
}

enum CohortAdvance {
    Pending,
    Complete(Option<PacketSnapshot>),
}

impl<S, P> GenerationStateMachine<'_, '_, S, P>
where
    S: LifecycleWorldgenSource + Sync,
    P: DimensionPolicy<S>,
{
    fn advance_cohort(
        &mut self,
        goal: CohortGoal,
        executor: &dyn ImmutableComputeExecutor,
    ) -> Result<CohortAdvance, SessionError> {
        match (goal, self.phase) {
            (CohortGoal::Mutable, GenerationPhase::PacketDeferred | GenerationPhase::SettlementPending) => {
                return Ok(CohortAdvance::Complete(None));
            }
            (CohortGoal::Packet, GenerationPhase::PacketDeferred) => {
                self.phase = if self.output.is_some() {
                    GenerationPhase::PacketNeighbours
                } else {
                    GenerationPhase::Output
                };
            }
            (CohortGoal::Packet, GenerationPhase::SettlementPending) => {
                return Err(SessionError::InvalidCheckpointAt(
                    "cohort finalization reached an unsettled mutable target",
                ));
            }
            _ => {}
        }
        match self.step(executor)? {
            Some(snapshot) => Ok(CohortAdvance::Complete(Some(snapshot))),
            None => Ok(CohortAdvance::Pending),
        }
    }
}

impl<'a, S> ProductionGenerationRegion<'a, S>
where
    S: RegionGenerationSource,
{
    fn prepare_target_owned_cohort(
        &mut self,
        sessions: &[GenerationSession],
        plan: TargetSettlementPlan,
        executor: &dyn ImmutableComputeExecutor,
    ) -> Result<TargetOwnedCohortCursor, SessionError> {
        #[cfg(feature = "worldgen-stage-pmu")]
        let _admission = RegionGuard::enter(RegionPhase::Admission);
        self.admit_chunks_with_context_executor(
            &plan.targets,
            &plan.context,
            &plan.targets,
            TARGET_FEATURE_RADIUS,
            executor,
        )?;
        #[cfg(feature = "worldgen-stage-pmu")]
        drop(_admission);
        self.finish_target_owned_cohort(sessions, plan)
    }

    #[cfg(any(target_arch = "wasm32", test))]
    async fn prepare_target_owned_cohort_yielding(
        &mut self,
        sessions: &[GenerationSession],
        plan: TargetSettlementPlan,
        executor: &dyn ImmutableComputeExecutor,
    ) -> Result<TargetOwnedCohortCursor, SessionError> {
        let cancellations = sessions.iter()
            .map(GenerationSession::cancellation)
            .collect::<Vec<_>>();
        crate::immutable_admission::check_cancellations(&cancellations)?;
        self.admit_chunks_with_context_yielding(
            &plan.targets,
            &plan.context,
            &plan.targets,
            TARGET_FEATURE_RADIUS,
            &cancellations,
            executor,
        ).await?;
        crate::immutable_admission::check_cancellations(&cancellations)?;
        self.finish_target_owned_cohort(sessions, plan)
    }

    fn finish_target_owned_cohort(
        &mut self,
        sessions: &[GenerationSession],
        plan: TargetSettlementPlan,
    ) -> Result<TargetOwnedCohortCursor, SessionError> {
        self.admission_counts = RegionAdmissionCounts {
            requested: sessions.len(),
            mutable: plan.targets.len(),
            read_only: plan.context.len().saturating_sub(plan.targets.len()),
            ..RegionAdmissionCounts::default()
        };
        self.refresh_admission_counts();
        let boundary = sessions[0].pipeline().schedule().target_stage(GenerationTarget::Shaped);
        prime_shared_prefixes::<S, S::Policy>(
            self.source,
            &plan.targets,
            boundary,
            &mut self.materializer,
            &mut self.shared_prefixes,
        )?;
        preseed_retained_mutations(&mut self.materializer, sessions)?;
        #[cfg(feature = "worldgen-stage-pmu")]
        let _replay_context = RegionGuard::enter(RegionPhase::ReplayContext);
        {
            let _timing = PhaseTimer::start(
                WorldgenTimingPhase::MutableSettlement,
                plan.targets.len().min(u32::MAX as usize) as u32,
            );
            self.materializer.prepare_lifecycle_replay_contexts_prepared(&plan.targets);
        }
        #[cfg(feature = "worldgen-stage-pmu")]
        drop(_replay_context);
        self.settlement_padding = plan.padding.clone();
        self.settlement_targets = plan.targets.iter().copied().collect();
        self.materializer.declare_mutable_targets(plan.targets.iter().copied());
        self.materializer.declare_sparse_padding_targets(plan.padding.iter().copied());
        Ok(TargetOwnedCohortCursor::new(plan, sessions))
    }

    fn cohort_machine<'m>(
        &'m mut self,
        sessions: &'m mut [GenerationSession],
        cursor: &'m mut TargetOwnedCohortCursor,
        action: CohortAction,
    ) -> Result<(GenerationStateMachine<'a, 'm, S, S::Policy>, CohortGoal), SessionError> {
        let (index, goal) = match action {
            CohortAction::MutableSession { index, .. } => (index, CohortGoal::Mutable),
            CohortAction::FinalizeOutput { index, .. } => (index, CohortGoal::Packet),
            _ => unreachable!("only session actions borrow the stage machine"),
        };
        #[cfg(feature = "worldgen-stage-pmu")]
        let _machine_rebuild = matches!(goal, CohortGoal::Packet)
            .then(|| RegionGuard::enter(RegionPhase::MachineRebuild));
        let mut machine = GenerationStateMachine::<S, S::Policy>::new(
            self.source,
            &mut sessions[index],
            &mut self.materializer,
            &mut self.shared_prefixes,
            true,
        )?;
        machine.padding_targets = Some(&self.settlement_padding);
        machine.settlement_targets = Some(&self.settlement_targets);
        if matches!(goal, CohortGoal::Packet) {
            machine.packet_columns = Some(&mut cursor.packet_columns);
            machine.generated_packet_columns = Some(&mut cursor.generated_packet_columns);
        }
        Ok((machine, goal))
    }

    fn complete_sparse_owner(&mut self, target: ChunkCoordinate, sequence: usize) {
        if !self.materializer.target_features_completed(target) {
            #[cfg(feature = "worldgen-stage-pmu")]
            let _mutable_padding = RegionGuard::enter(RegionPhase::MutablePadding);
            let _timing = PhaseTimer::start(WorldgenTimingPhase::MutableSettlement, 1);
            self.materializer.complete_target_features_with_mode_observing(
                target,
                sequence as u64,
                LifecycleCompletionMode::SparsePadding,
                |_| {},
            );
            self.materializer.finish_target(target);
        }
    }

    fn finish_cohort_action<F>(
        &mut self,
        sessions: &[GenerationSession],
        cursor: &mut TargetOwnedCohortCursor,
        action: CohortAction,
        result: Result<Option<PacketSnapshot>, SessionError>,
        on_stable: &mut F,
    ) -> Result<(), GenerationRequestError>
    where
        F: FnMut(usize, &GenerationSession, GenerationRequestResult) -> Result<(), GenerationRequestError>,
    {
        match action {
            CohortAction::MutableSession { target, index } => {
                if let Err(error) = result {
                    self.materializer.abort_target(target);
                    if sessions[index].cancellation().is_cancelled() {
                        cursor.outcomes[index] = Some(Err(SessionError::Cancelled.into()));
                    } else {
                        return Err(error.into());
                    }
                }
            }
            CohortAction::FinalizeOutput { coordinate, index } => {
                match result {
                    Ok(Some(snapshot)) => {
                        if snapshot.coordinate() != coordinate {
                            return Err(SessionError::InvalidCheckpointAt(
                                "cohort snapshot coordinate differed from its output fence",
                            ).into());
                        }
                        on_stable(index, &sessions[index], GenerationRequestResult::Generated(snapshot))?;
                        cursor.outcomes[index] = Some(Ok(()));
                    }
                    Err(error) => cursor.outcomes[index] = Some(Err(error.into())),
                    Ok(None) => unreachable!("packet goal must produce a snapshot"),
                }
            }
            _ => unreachable!("only session actions produce stage-machine results"),
        }
        Ok(())
    }

    pub(super) fn generate_target_owned_cohort_with_executor<F>(
        &mut self,
        sessions: &mut [GenerationSession],
        plan: TargetSettlementPlan,
        executor: &dyn ImmutableComputeExecutor,
        mut on_stable: F,
    ) -> Result<Vec<Result<(), GenerationRequestError>>, GenerationRequestError>
    where
        F: FnMut(usize, &GenerationSession, GenerationRequestResult) -> Result<(), GenerationRequestError>,
    {
        let mut cursor = self.prepare_target_owned_cohort(sessions, plan, executor)?;
        loop {
            let action = cursor.next_action(sessions);
            if let CohortAction::SparseOwner { target, sequence } = action {
                self.complete_sparse_owner(target, sequence);
                continue;
            }
            if action == CohortAction::Done {
                return cursor.into_outcomes();
            }
            #[cfg(feature = "worldgen-stage-pmu")]
            let _mutable_target = matches!(action, CohortAction::MutableSession { .. })
                .then(|| RegionGuard::enter(RegionPhase::MutableTarget));
            let result = {
                let (mut machine, goal) = self.cohort_machine(sessions, &mut cursor, action)?;
                #[cfg(feature = "worldgen-stage-pmu")]
                let _snapshot_finalization = matches!(action, CohortAction::FinalizeOutput { .. })
                    .then(|| RegionGuard::enter(RegionPhase::SnapshotFinalization));
                loop {
                    match machine.advance_cohort(goal, executor) {
                        Ok(CohortAdvance::Complete(snapshot)) => break Ok(snapshot),
                        Ok(CohortAdvance::Pending) => {},
                        Err(error) => break Err(error),
                    }
                }
            };
            #[cfg(feature = "worldgen-stage-pmu")]
            drop(_mutable_target);
            self.finish_cohort_action(sessions, &mut cursor, action, result, &mut on_stable)?;
        }
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(super) async fn generate_target_owned_cohort_with_cooperation<F, C, Y>(
        &mut self,
        sessions: &mut [GenerationSession],
        plan: TargetSettlementPlan,
        executor: &dyn ImmutableComputeExecutor,
        mut on_stable: F,
        mut cooperate: C,
    ) -> Result<Vec<Result<(), GenerationRequestError>>, GenerationRequestError>
    where
        F: FnMut(usize, &GenerationSession, GenerationRequestResult) -> Result<(), GenerationRequestError>,
        C: FnMut() -> Y,
        Y: std::future::Future<Output = ()>,
    {
        let mut cursor = self.prepare_target_owned_cohort_yielding(sessions, plan, executor).await?;
        let mut budget = CohortCooperationBudget::default();
        let mut operation_started = lodestone_time::Instant::now();
        loop {
            let action = cursor.next_action(sessions);
            if let CohortAction::SparseOwner { target, sequence } = action {
                self.complete_sparse_owner(target, sequence);
                operation_started = cooperate_after_cohort_operation(
                    &mut budget, operation_started, &mut cooperate,
                ).await;
                continue;
            }
            if action == CohortAction::Done {
                return cursor.into_outcomes();
            }
            let result = {
                let (mut machine, goal) = self.cohort_machine(sessions, &mut cursor, action)?;
                loop {
                    match machine.advance_cohort(goal, executor) {
                        Ok(CohortAdvance::Complete(snapshot)) => break Ok(snapshot),
                        Ok(CohortAdvance::Pending) => {
                            operation_started = cooperate_after_cohort_operation(
                                &mut budget, operation_started, &mut cooperate,
                            ).await;
                        }
                        Err(error) => break Err(error),
                    }
                }
            };
            self.finish_cohort_action(sessions, &mut cursor, action, result, &mut on_stable)?;
            operation_started = cooperate_after_cohort_operation(
                &mut budget, operation_started, &mut cooperate,
            ).await;
        }
    }
}

#[cfg(test)]
mod cooperation_budget_tests {
    use super::CohortCooperationBudget;
    use std::time::Duration;

    #[test]
    fn occupied_work_uses_independent_elapsed_arithmetic() {
        let mut budget = CohortCooperationBudget::default();
        let exhausted = [173, 289, 463, 75].map(|micros| {
            budget.complete_operation(Duration::from_micros(micros))
        });
        assert_eq!(exhausted, [false, false, false, true]);
        assert_eq!(budget.occupied, Duration::from_micros(1000));
        assert_eq!(budget.operations, 4);
    }

    #[test]
    fn operation_count_bounds_work_with_no_clock_progress() {
        let mut budget = CohortCooperationBudget::default();
        for operation in 1..64 {
            assert!(!budget.complete_operation(Duration::ZERO), "operation {operation}");
        }
        assert!(budget.complete_operation(Duration::ZERO));
        assert_eq!(budget.occupied, Duration::ZERO);
        assert_eq!(budget.operations, 64);
    }

    #[test]
    fn cooperation_resets_both_occupied_work_and_operation_count() {
        let mut budget = CohortCooperationBudget::default();
        assert!(budget.complete_operation(Duration::from_micros(1014)));
        budget.reset();
        assert_eq!(budget.occupied, Duration::ZERO);
        assert_eq!(budget.operations, 0);
        assert!(!budget.complete_operation(Duration::from_micros(463)));
        assert!(!budget.complete_operation(Duration::from_micros(463)));
        assert_eq!(budget.occupied, Duration::from_micros(926));
        assert!(budget.complete_operation(Duration::from_micros(89)));
        budget.reset();
        for operation in 1..64 {
            assert!(!budget.complete_operation(Duration::ZERO), "operation {operation}");
        }
        assert!(budget.complete_operation(Duration::ZERO));
    }

    #[test]
    fn a_single_operation_is_only_bounded_after_it_completes() {
        let mut budget = CohortCooperationBudget::default();
        assert!(budget.complete_operation(Duration::from_micros(27_031)));
        assert_eq!(budget.occupied, Duration::from_micros(27_031));
        assert_eq!(budget.operations, 1);
        budget.reset();
        assert!(!budget.complete_operation(Duration::from_micros(89)));
    }
}
