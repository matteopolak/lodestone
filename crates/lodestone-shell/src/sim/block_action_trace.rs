//! Opt-in timings at the shell's block-breaking and presentation boundaries.

use std::collections::VecDeque;
use std::fmt;

use lodestone_client::{BlockPos, ClientAction};
use lodestone_ecs::ChunkWorld;
use lodestone_ecs::ecs::resource::Resource;
use lodestone_model::{BlockActionKind, PredictionSequence};
use lodestone_world::{ChunkPos, World};

use crate::blocks::id;
use crate::mesher::SectionKey;
use crate::platform::Instant;

const MAX_ATTEMPTS: usize = 32;
const MAX_REPORTS: usize = 32;
const EXPIRY_MS: u64 = 120_000;
const MAX_AFFECTED_SECTIONS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TraceId(u64);

#[derive(Debug, Clone, Copy)]
pub(crate) enum AttemptOrigin {
    Press,
    Held,
    Intent,
}

#[derive(Debug, Clone, Default)]
struct Milestones {
    input: Option<u64>,
    start: Option<u64>,
    complete: Option<u64>,
    predicted: Option<u64>,
    egress: Option<u64>,
    ack: Option<u64>,
    observed: Option<u64>,
    handoff: Option<u64>,
    present: Option<u64>,
}

#[derive(Debug, Clone)]
struct Attempt {
    id: TraceId,
    pos: BlockPos,
    origin: AttemptOrigin,
    created: u64,
    sequence: Option<PredictionSequence>,
    milestones: Milestones,
    observed_state: Option<u32>,
    restored: bool,
    waiting: [Option<SectionKey>; MAX_AFFECTED_SECTIONS],
    keys_registered: bool,
    empty_renderer_settlements: u8,
}

/// Missing milestones remain `None`, including a held attempt's input time.
#[derive(Debug, Clone)]
pub(crate) struct BlockActionTraceReport {
    attempt: Attempt,
    outcome: &'static str,
    dropped_reports: u64,
}

fn interval(from: Option<u64>, to: Option<u64>) -> Option<u64> {
    to?.checked_sub(from?)
}

impl fmt::Display for BlockActionTraceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let a = &self.attempt;
        let m = &a.milestones;
        write!(
            f,
            "block-action id={} pos=({},{},{}) origin={:?} outcome={} completion_sequence={:?} input_ms={:?} mining_start_ms={:?} complete_ms={:?} predicted_ms={:?} egress_attempt_ms={:?} completion_ack_ms={:?} authoritative_state_observed_ms={:?} observed_state={:?} restored={} current_mesh_settled_ms={:?} empty_renderer_settlements={} following_present_submission_ms={:?} input_to_start_ms={:?} mining_duration_ms={:?} complete_to_prediction_ms={:?} prediction_to_egress_ms={:?} egress_to_ack_ms={:?} state_to_mesh_ms={:?} mesh_to_present_ms={:?} dropped_reports={}",
            a.id.0, a.pos.x, a.pos.y, a.pos.z, a.origin, self.outcome,
            a.sequence.map(PredictionSequence::raw), m.input, m.start, m.complete,
            m.predicted, m.egress, m.ack, m.observed, a.observed_state, a.restored,
            m.handoff, a.empty_renderer_settlements, m.present,
            interval(m.input, m.start), interval(m.start, m.complete),
            interval(m.complete, m.predicted), interval(m.predicted, m.egress),
            interval(m.egress, m.ack), interval(m.observed, m.handoff),
            interval(m.handoff, m.present), self.dropped_reports,
        )
    }
}

#[derive(Debug, Default)]
struct TraceModel {
    next_id: u64,
    attempts: Vec<Attempt>,
    reports: VecDeque<BlockActionTraceReport>,
    dropped_reports: u64,
}

impl TraceModel {
    fn finish(&mut self, index: usize, outcome: &'static str) {
        if self.reports.len() == MAX_REPORTS {
            self.reports.pop_front();
            self.dropped_reports += 1;
        }
        let report = BlockActionTraceReport {
            attempt: self.attempts.remove(index),
            outcome,
            dropped_reports: self.dropped_reports,
        };
        tracing::info!(target: "lodestone_block_action_trace", report = %report, "block action trace");
        self.reports.push_back(report);
    }

    fn create(&mut self, pos: BlockPos, origin: AttemptOrigin, now: u64) -> TraceId {
        self.expire(now);
        if self.attempts.len() == MAX_ATTEMPTS {
            self.finish(0, "capacity-evicted");
        }
        self.next_id = self.next_id.wrapping_add(1);
        let trace_id = TraceId(self.next_id);
        self.attempts.push(Attempt {
            id: trace_id,
            pos,
            origin,
            created: now,
            sequence: None,
            milestones: Milestones::default(),
            observed_state: None,
            restored: false,
            waiting: [None; MAX_AFFECTED_SECTIONS],
            keys_registered: false,
            empty_renderer_settlements: 0,
        });
        trace_id
    }

    fn reject(&mut self, trace_id: TraceId, reason: &'static str) {
        if let Some(index) = self.attempts.iter().position(|a| a.id == trace_id) {
            self.finish(index, reason);
        }
    }

    fn mining_actions(
        &mut self,
        press: Option<TraceId>,
        origin: AttemptOrigin,
        actions: &[ClientAction],
        now: u64,
    ) {
        let mut started_press = false;
        for action in actions {
            let ClientAction::BlockAction { action, pos, .. } = action else { continue };
            match action {
                BlockActionKind::AbortDestroy => {
                    if let Some(index) = self.attempts.iter().position(|a| {
                        a.pos == *pos && a.milestones.start.is_some() && a.milestones.complete.is_none()
                    }) {
                        self.finish(index, "aborted");
                    }
                }
                BlockActionKind::StartDestroy => {
                    let trace_id = if let Some(trace_id) = press {
                        started_press = true;
                        trace_id
                    } else {
                        self.create(*pos, origin, now)
                    };
                    if let Some(attempt) = self.attempts.iter_mut().find(|a| a.id == trace_id) {
                        attempt.milestones.start = Some(now);
                    }
                }
                BlockActionKind::StopDestroy => {}
            }
        }
        if !started_press && let Some(press) = press {
            self.reject(press, "press-did-not-start");
        }
    }

    fn completed(&mut self, pos: BlockPos, sequence: PredictionSequence, now: u64) {
        let Some(index) = self.attempts.iter().position(|a| {
            a.pos == pos && a.milestones.start.is_some() && a.milestones.complete.is_none()
        }) else { return };
        self.attempts[index].sequence = Some(sequence);
        self.attempts[index].milestones.complete = Some(now);
        // A changed-coordinate signal has no attempt identity. Repeated
        // outstanding completions at one position cannot share its evidence.
        if self.attempts.iter().enumerate().any(|(other, a)| {
            other != index && a.pos == pos && a.milestones.complete.is_some()
        }) {
            for index in (0..self.attempts.len()).rev() {
                if self.attempts[index].pos == pos {
                    self.finish(index, "overlapping-position");
                }
            }
        }
    }

    fn arm_keys(attempt: &mut Attempt, keys: [Option<SectionKey>; MAX_AFFECTED_SECTIONS]) {
        attempt.keys_registered = keys.iter().any(Option::is_some);
        attempt.waiting = keys;
        attempt.milestones.handoff = None;
        attempt.milestones.present = None;
        attempt.empty_renderer_settlements = 0;
    }

    fn acknowledge(&mut self, sequence: PredictionSequence, now: u64) {
        for attempt in &mut self.attempts {
            if attempt.sequence.is_some_and(|completion| completion.is_acknowledged_by(sequence)) {
                attempt.milestones.ack.get_or_insert(now);
            }
        }
        self.finish_observed();
    }

    fn handed_off(&mut self, key: SectionKey, now: u64) {
        self.settled(key, now, false);
    }

    fn settled(&mut self, key: SectionKey, now: u64, empty_renderer_absent: bool) {
        for attempt in &mut self.attempts {
            if !attempt.keys_registered { continue; }
            let mut matched = false;
            for waiting in &mut attempt.waiting {
                if *waiting == Some(key) {
                    *waiting = None;
                    matched = true;
                }
            }
            if matched && empty_renderer_absent {
                attempt.empty_renderer_settlements += 1;
            }
            if attempt.waiting.iter().all(Option::is_none) {
                attempt.milestones.handoff.get_or_insert(now);
            }
        }
    }

    fn presented(&mut self, now: u64) {
        for attempt in &mut self.attempts {
            if attempt.milestones.handoff.is_some() {
                attempt.milestones.present.get_or_insert(now);
            }
        }
        self.finish_observed();
        self.expire(now);
    }

    fn finish_observed(&mut self) {
        for index in (0..self.attempts.len()).rev() {
            let attempt = &self.attempts[index];
            if attempt.milestones.ack.is_some() && attempt.milestones.observed.is_some()
                && attempt.milestones.present.is_some()
            {
                let outcome = if attempt.observed_state == Some(id::AIR) {
                    "air-observed"
                } else {
                    "non-air-observed"
                };
                self.finish(index, outcome);
            }
        }
    }

    fn expire(&mut self, now: u64) {
        for index in (0..self.attempts.len()).rev() {
            if now.saturating_sub(self.attempts[index].created) >= EXPIRY_MS {
                self.finish(index, "expired");
            }
        }
    }
}

/// No clock, allocation, action scan or world access occurs while disabled.
#[derive(Debug, Resource)]
pub struct BlockActionTrace {
    started: Option<Instant>,
    model: TraceModel,
}

impl Default for BlockActionTrace {
    fn default() -> Self {
        let mut trace = Self { started: None, model: TraceModel::default() };
        #[cfg(not(target_arch = "wasm32"))]
        let enabled = std::env::var_os("LODESTONE_BLOCK_ACTION_TRACE")
            .is_some_and(|value| !value.is_empty() && value != "0");
        #[cfg(target_arch = "wasm32")]
        let enabled = false;
        trace.set_enabled(enabled);
        trace
    }
}

impl BlockActionTrace {
    pub(crate) fn enabled(&self) -> bool { self.started.is_some() }

    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        if enabled == self.enabled() { return; }
        self.reset("trace-disabled");
        self.started = enabled.then(Instant::now);
    }

    fn now(&self) -> Option<u64> {
        Some(self.started?.elapsed().as_millis() as u64)
    }

    pub(crate) fn delivered_input(&mut self, pos: BlockPos) -> Option<TraceId> {
        let now = self.now()?;
        let trace_id = self.model.create(pos, AttemptOrigin::Press, now);
        self.model.attempts.last_mut()?.milestones.input = Some(now);
        Some(trace_id)
    }

    pub(crate) fn rejected_input(&mut self, press: Option<TraceId>, reason: &'static str) {
        if !self.enabled() { return; }
        if let Some(press) = press { self.model.reject(press, reason); }
    }

    pub(crate) fn mining_actions(
        &mut self, press: Option<TraceId>, origin: AttemptOrigin, actions: &[ClientAction],
    ) {
        let Some(now) = self.now() else { return };
        self.model.mining_actions(press, origin, actions, now);
    }

    pub(crate) fn completed(&mut self, pos: BlockPos, sequence: PredictionSequence) {
        let Some(now) = self.now() else { return };
        self.model.completed(pos, sequence, now);
    }

    pub(crate) fn predicted(&mut self, pos: BlockPos, store: &ChunkWorld) {
        let Some(now) = self.now() else { return };
        let Some(attempt) = self.model.attempts.iter_mut().find(|a| {
            a.pos == pos && a.milestones.complete.is_some()
        }) else { return };
        attempt.milestones.predicted = Some(now);
        TraceModel::arm_keys(attempt, affected_keys(&store.read(), pos));
    }

    pub(crate) fn egress_attempt(&mut self, actions: &[ClientAction]) {
        let Some(now) = self.now() else { return };
        for action in actions {
            let ClientAction::BlockAction { action: BlockActionKind::StartDestroy | BlockActionKind::StopDestroy, pos, sequence, .. } = action else { continue };
            let sequence = PredictionSequence::from_wire(*sequence);
            if let Some(attempt) = self.model.attempts.iter_mut().find(|a| a.pos == *pos && a.sequence == Some(sequence)) {
                attempt.milestones.egress.get_or_insert(now);
            }
        }
    }

    pub(crate) fn acknowledged(&mut self, sequence: PredictionSequence) {
        let Some(now) = self.now() else { return };
        self.model.acknowledge(sequence, now);
    }

    /// Samples the shared world after a changed-coordinate notification. The
    /// notification itself carries no immutable state or action identity.
    pub(crate) fn observe_changed(
        &mut self, sx: i32, sy: i32, sz: i32, blocks: &[[u8; 3]], store: &ChunkWorld,
    ) {
        let Some(now) = self.now() else { return };
        let affected = |a: &Attempt| {
            a.milestones.complete.is_some() && a.pos.x >> 4 == sx
                && a.pos.y >> 4 == sy && a.pos.z >> 4 == sz
                && blocks.iter().any(|&[x, y, z]| {
                    a.pos.x & 15 == i32::from(x) && a.pos.y & 15 == i32::from(y)
                        && a.pos.z & 15 == i32::from(z)
                })
        };
        if !self.model.attempts.iter().any(affected) { return; }
        let world = store.read();
        for attempt in self.model.attempts.iter_mut().filter(|a| affected(a)) {
            if let Some(state) = world.block_state_at(attempt.pos.x, attempt.pos.y, attempt.pos.z) {
                attempt.milestones.observed = Some(now);
                attempt.observed_state = Some(state);
                TraceModel::arm_keys(attempt, affected_keys(&world, attempt.pos));
            }
        }
    }

    pub(crate) fn restored(&mut self, pos: BlockPos, store: &ChunkWorld) {
        if !self.enabled() { return; }
        if let Some(attempt) = self.model.attempts.iter_mut().find(|a| a.pos == pos) {
            attempt.restored = true;
            TraceModel::arm_keys(attempt, affected_keys(&store.read(), pos));
        }
    }

    /// Called only for successful current uploads or an explicit empty-section
    /// renderer removal; a CPU mesh or failed upload does not reach this seam.
    pub(crate) fn mesh_handed_off(&mut self, key: SectionKey) {
        let Some(now) = self.now() else { return };
        self.model.handed_off(key, now);
    }

    fn empty_renderer_settled(&mut self, key: SectionKey) {
        let Some(now) = self.now() else { return };
        self.model.settled(key, now, true);
    }

    pub(crate) fn present_submitted(&mut self) {
        let Some(now) = self.now() else { return };
        self.model.presented(now);
    }

    pub(crate) fn reset(&mut self, reason: &'static str) {
        if !self.enabled() { return; }
        while !self.model.attempts.is_empty() { self.model.finish(0, reason); }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn take_report(&mut self) -> Option<BlockActionTraceReport> {
        self.model.reports.pop_front()
    }
}

fn affected_keys(world: &World, pos: BlockPos) -> [Option<SectionKey>; MAX_AFFECTED_SECTIONS] {
    let mut keys = [None; MAX_AFFECTED_SECTIONS];
    let Some(column) = world.values().next().map(|chunk| &chunk.column) else { return keys };
    let min_y = column.min_y();
    let section_count = column.section_count();
    let dirty = super::meshing::dirty_sections_for_blocks(
        pos.x >> 4, pos.y >> 4, pos.z >> 4,
        &[[pos.x.rem_euclid(16) as u8, pos.y.rem_euclid(16) as u8, pos.z.rem_euclid(16) as u8]],
    );
    let mut next = 0;
    for (cx, sy, cz) in dirty {
        let si = (sy * 16 - min_y).div_euclid(16);
        if si < 0 || si as usize >= section_count || !world.contains(ChunkPos::new(cx, cz)) { continue; }
        keys[next] = Some(SectionKey { cx, cz, si: si as usize, min_y });
        next += 1;
    }
    keys
}

impl super::Sim {
    /// Enable only this diagnostic; browser debug logging is independent.
    pub fn set_block_action_trace_enabled(&mut self, enabled: bool) {
        if enabled == self.block_action_trace_enabled {
            return;
        }
        self.block_action_trace_enabled = self.write(|world| {
            if let Some(mut trace) = world.get_resource_mut::<BlockActionTrace>() {
                trace.set_enabled(enabled);
                trace.enabled()
            } else {
                false
            }
        });
    }

    pub fn block_action_trace_enabled(&self) -> bool {
        self.block_action_trace_enabled
    }

    pub(crate) fn trace_block_action_updates(
        &mut self, sx: i32, sy: i32, sz: i32, blocks: &[[u8; 3]],
    ) {
        if !self.block_action_trace_enabled { return; }
        self.write(|world| {
            if !world.get_resource::<BlockActionTrace>().is_some_and(BlockActionTrace::enabled) {
                return;
            }
            let store = world.resource::<ChunkWorld>().clone();
            world.resource_mut::<BlockActionTrace>().observe_changed(sx, sy, sz, blocks, &store);
        });
    }

    pub(crate) fn trace_block_action_restored(&mut self, pos: BlockPos) {
        if !self.block_action_trace_enabled { return; }
        self.write(|world| {
            if !world.get_resource::<BlockActionTrace>().is_some_and(BlockActionTrace::enabled) {
                return;
            }
            let store = world.resource::<ChunkWorld>().clone();
            world.resource_mut::<BlockActionTrace>().restored(pos, &store);
        });
    }

    pub(crate) fn trace_block_action_mesh_handoff(&mut self, key: SectionKey) {
        if !self.block_action_trace_enabled { return; }
        self.write(|world| {
            if let Some(mut trace) = world.get_resource_mut::<BlockActionTrace>() {
                trace.mesh_handed_off(key);
            }
        });
    }

    pub(crate) fn pending_block_action_mesh_keys(&self) -> Vec<SectionKey> {
        if !self.block_action_trace_enabled { return Vec::new(); }
        self.read(|world| {
            let mut keys = Vec::new();
            if let Some(trace) = world.get_resource::<BlockActionTrace>() {
                for attempt in &trace.model.attempts {
                    for key in attempt.waiting.iter().flatten() {
                        if !keys.contains(key) { keys.push(*key); }
                    }
                }
            }
            keys
        })
    }

    /// The caller must have observed renderer absence after its normal drains.
    pub(crate) fn trace_block_action_empty_settled(&mut self, key: SectionKey) {
        if !self.block_action_trace_enabled { return; }
        self.write(|world| {
            let store = world.resource::<ChunkWorld>();
            if !store.extent().is_some_and(|extent| {
                extent.min_y == key.min_y && key.si < extent.section_count
            }) || !store.contains_column(key.cx, key.cz) {
                return;
            }
            let current_empty = world.resource::<crate::mesher::TerrainMesh>()
                .current_empty_section_settled(key);
            if current_empty && let Some(mut trace) = world.get_resource_mut::<BlockActionTrace>() {
                trace.empty_renderer_settled(key);
            }
        });
    }

    pub(crate) fn trace_block_action_mesh_removed(&mut self, key: SectionKey) {
        if !self.block_action_trace_enabled { return; }
        self.write(|world| {
            let waiting = world.get_resource::<BlockActionTrace>().is_some_and(|trace| {
                trace.model.attempts.iter().any(|attempt| attempt.waiting.contains(&Some(key)))
            });
            if !waiting { return; }
            let store = world.resource::<ChunkWorld>().clone();
            let resident_empty = {
                let blocks = store.read();
                blocks.get(ChunkPos::new(key.cx, key.cz)).is_some_and(|chunk| {
                    let column = &chunk.column;
                    column.min_y() == key.min_y && key.si < column.section_count()
                        && column.section(key.si).is_none_or(|section| section.is_air_only())
                })
            };
            if resident_empty {
                world.resource_mut::<BlockActionTrace>().mesh_handed_off(key);
            }
        });
    }

    pub(crate) fn trace_block_action_egress_attempt(&mut self, actions: &[ClientAction]) {
        if !self.block_action_trace_enabled { return; }
        self.write(|world| {
            if let Some(mut trace) = world.get_resource_mut::<BlockActionTrace>() {
                trace.egress_attempt(actions);
            }
        });
    }

    pub(crate) fn trace_block_action_acknowledged(&mut self, sequence: PredictionSequence) {
        if !self.block_action_trace_enabled { return; }
        self.write(|world| {
            if let Some(mut trace) = world.get_resource_mut::<BlockActionTrace>() {
                trace.acknowledged(sequence);
            }
        });
    }

    pub(crate) fn trace_block_action_present_submitted(&mut self) {
        if !self.block_action_trace_enabled { return; }
        self.write(|world| {
            if let Some(mut trace) = world.get_resource_mut::<BlockActionTrace>() {
                trace.present_submitted();
            }
        });
    }

    pub(crate) fn reset_block_action_trace(&mut self, reason: &'static str) {
        if !self.block_action_trace_enabled { return; }
        self.write(|world| {
            if let Some(mut trace) = world.get_resource_mut::<BlockActionTrace>() {
                trace.reset(reason);
            }
        });
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn take_block_action_trace_report(&mut self) -> Option<String> {
        self.write(|world| {
            world.get_resource_mut::<BlockActionTrace>()?.take_report().map(|report| report.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::BlockFace;

    fn action(kind: BlockActionKind, pos: BlockPos, sequence: u32) -> ClientAction {
        ClientAction::BlockAction { action: kind, pos, face: BlockFace::Up, sequence: sequence as i32 }
    }

    #[test]
    fn milestones_keep_mining_time_and_current_mesh_evidence_separate() {
        let pos = BlockPos::new(33, 81, -19);
        let key = SectionKey { cx: 2, cz: -2, si: 9, min_y: -64 };
        let mut model = TraceModel::default();
        let press = model.create(pos, AttemptOrigin::Press, 101);
        model.attempts[0].milestones.input = Some(101);
        model.mining_actions(Some(press), AttemptOrigin::Press,
            &[action(BlockActionKind::StartDestroy, pos, 40)], 137);
        model.completed(pos, PredictionSequence::new(41), 1137);
        model.attempts[0].milestones.predicted = Some(1140);
        model.attempts[0].milestones.egress = Some(1144);
        model.acknowledge(PredictionSequence::new(40), 1180);
        assert_eq!(model.attempts[0].milestones.ack, None);
        model.acknowledge(PredictionSequence::new(41), 1181);
        model.attempts[0].milestones.observed = Some(1187);
        model.attempts[0].observed_state = Some(id::AIR);
        let mut keys = [None; MAX_AFFECTED_SECTIONS];
        keys[0] = Some(key);
        let empty_key = SectionKey { cx: 1, ..key };
        keys[1] = Some(empty_key);
        TraceModel::arm_keys(&mut model.attempts[0], keys);
        model.handed_off(SectionKey { cx: 7, ..key }, 1190);
        model.presented(1197);
        assert_eq!(model.attempts[0].milestones.handoff, None);
        model.handed_off(key, 1206);
        model.presented(1207);
        assert_eq!(model.attempts[0].milestones.handoff, None);
        model.settled(empty_key, 1212, true);
        model.presented(1221);
        let report = model.reports.pop_front().unwrap();
        let m = report.attempt.milestones;
        assert_eq!(interval(m.input, m.start), Some(36));
        assert_eq!(interval(m.start, m.complete), Some(1000));
        assert_eq!(interval(m.complete, m.predicted), Some(3));
        assert_eq!(interval(m.egress, m.ack), Some(37));
        assert_eq!(interval(m.observed, m.handoff), Some(25));
        assert_eq!(interval(m.handoff, m.present), Some(9));
        assert_eq!(report.outcome, "air-observed");
        assert_eq!(report.attempt.empty_renderer_settlements, 1);
    }

    #[test]
    fn hold_attempt_has_no_input_and_repeated_completions_are_ambiguous() {
        let pos = BlockPos::new(-31, 67, 34);
        let mut model = TraceModel::default();
        model.mining_actions(None, AttemptOrigin::Held,
            &[action(BlockActionKind::StartDestroy, pos, u32::MAX)], 9);
        model.completed(pos, PredictionSequence::new(u32::MAX), 10);
        model.acknowledge(PredictionSequence::new(0), 17);
        assert_eq!(model.attempts[0].milestones.input, None);
        assert_eq!(model.attempts[0].milestones.ack, Some(17));
        model.mining_actions(None, AttemptOrigin::Held,
            &[action(BlockActionKind::StartDestroy, pos, 1)], 23);
        model.completed(pos, PredictionSequence::new(1), 24);
        assert!(model.attempts.is_empty());
        assert_eq!(model.reports.len(), 2);
        assert!(model.reports.iter().all(|r| r.outcome == "overlapping-position"));
    }

    #[test]
    fn expiry_and_capacity_reports_preserve_missing_milestones() {
        let mut model = TraceModel::default();
        for x in 0..=MAX_ATTEMPTS { model.create(BlockPos::new(x as i32, 5, 7), AttemptOrigin::Press, 3); }
        assert_eq!(model.attempts.len(), MAX_ATTEMPTS);
        assert_eq!(model.reports[0].outcome, "capacity-evicted");
        model.presented(3 + EXPIRY_MS);
        assert!(model.attempts.is_empty());
        assert_eq!(model.reports.len(), MAX_REPORTS);
        assert_eq!(model.dropped_reports, 1);
        assert!(model.reports.iter().all(|r| r.attempt.milestones.present.is_none()));
    }
}
