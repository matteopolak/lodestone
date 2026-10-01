#![cfg(feature = "rcon-oracle")]

#[path = "support/differential_generation.rs"]
mod generation;
#[path = "../src/campaign/block_lane.rs"]
mod block_lane;
#[path = "../src/campaign/live_piston.rs"]
mod scenario;

use std::convert::Infallible;

use generation::{ReplayCase, SearchBudget, SearchOutcome, sample_scripts, search_and_shrink_with};
use block_lane::AIR;
use lodestone_fuzz::differential::redstone::RedstoneModelOracle;
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, Script, ScriptStep, WorldOracle, run_differential,
};
use scenario::{ARM, BASE, DIRT, EXTENDED, HEAD, MOVING, PISTON, POWER, SETTLE_TICKS, TRIGGER, build_model, domain, region};

fn budget() -> SearchBudget {
    SearchBudget { seed: 0x549_9157, cases: 64, shrink_attempts: 32 }
}

fn step(tick: u64, state: &str) -> ScriptStep {
    ScriptStep { tick, action: Action::SetBlock { pos: TRIGGER, state: state.to_owned() } }
}

/// Deliberately reports the source arm's stale head after a mid-extension
/// retraction, while the underlying production model continues to run.
struct StaleArm {
    inner: RedstoneModelOracle,
    powered: bool,
    interrupted: bool,
}

impl WorldOracle for StaleArm {
    type Error = Infallible;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        if let Action::SetBlock { pos, state } = action {
            if *pos == TRIGGER {
                self.interrupted |= self.powered && state == AIR && self.inner.tick() == 1;
                self.powered = state == POWER;
            }
        }
        self.inner.apply(action)
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> { self.inner.advance_tick() }

    fn block_state(&mut self, pos: (i32, i32, i32), candidates: &[String]) -> Result<Option<String>, Self::Error> {
        let observed = self.inner.block_state(pos, candidates)?;
        if self.interrupted && pos == ARM && observed.as_deref() == Some(AIR) {
            return Ok(Some(HEAD.to_owned()));
        }
        Ok(observed)
    }
}

fn evaluate_control(script: &Script, probes: &[((i32, i32, i32), Vec<String>)], settle: u64) -> DifferentialOutcome {
    let mut wrong = StaleArm { inner: build_model(), powered: false, interrupted: false };
    run_differential(script, probes, &mut wrong, &mut build_model(), settle)
}

#[test]
fn placement_move_phase_matches_the_external_commit_trace_and_cancels_a_same_tick_pulse() {
    let probes = region();
    let mut model = build_model();
    model.apply(&step(0, POWER).action).expect("place power");
    assert_eq!(model.block_state(BASE, &probes[0].1).expect("base before move").as_deref(), Some(PISTON));
    assert_eq!(model.block_state(ARM, &probes[2].1).expect("dirt before move").as_deref(), Some(DIRT));
    for (counter, arm, pushed) in [(1, MOVING, MOVING), (2, MOVING, MOVING), (3, HEAD, DIRT)] {
        model.advance_tick().expect("advance production scheduler");
        assert_eq!(model.tick(), counter);
        assert_eq!(model.block_state(BASE, &probes[0].1).expect("extended base").as_deref(), Some(EXTENDED));
        assert_eq!(model.block_state(ARM, &probes[2].1).expect("arm trace").as_deref(), Some(arm), "counter {counter}");
        assert_eq!(model.block_state((0, 1, 2), &probes[3].1).expect("carried dirt trace").as_deref(), Some(pushed), "counter {counter}");
    }
    let mut pulse = build_model();
    pulse.apply(&step(0, POWER).action).expect("pulse on");
    pulse.apply(&step(0, AIR).action).expect("pulse off before move phase");
    for _ in 0..4 { pulse.advance_tick().expect("drain canceled pulse"); }
    for (index, expected) in [(0, PISTON), (1, AIR), (2, DIRT), (3, AIR), (4, AIR)] {
        assert_eq!(pulse.block_state(probes[index].0, &probes[index].1).expect("pulse baseline").as_deref(), Some(expected));
    }
    let mut reverse = build_model();
    reverse.apply(&step(0, POWER).action).expect("begin extension");
    reverse.advance_tick().expect("extension move phase");
    reverse.apply(&step(1, AIR).action).expect("interrupt signal");
    reverse.advance_tick().expect("retraction move phase");
    reverse.apply(&step(2, POWER).action).expect("power during base animation");
    for (counter, base, arm) in [(3, MOVING, AIR), (4, PISTON, AIR), (5, EXTENDED, MOVING),
        (6, EXTENDED, MOVING), (7, EXTENDED, HEAD)] {
        reverse.advance_tick().expect("advance restored-base recheck");
        assert_eq!(reverse.tick(), counter);
        assert_eq!(reverse.block_state(BASE, &probes[0].1).expect("base recheck trace").as_deref(), Some(base), "counter {counter}");
        assert_eq!(reverse.block_state(ARM, &probes[2].1).expect("arm recheck trace").as_deref(), Some(arm), "counter {counter}");
    }
}

#[test]
fn stale_interrupted_arm_control_is_found_shrunk_and_replayed() {
    let scripts = sample_scripts(&domain(), budget()).expect("bounded stream");
    assert_eq!(scripts, sample_scripts(&domain(), budget()).expect("repeat stream"));
    assert_eq!(domain().max_steps(), 3);
    assert_eq!(domain().max_tick_gap(), 3);
    for script in &scripts {
        assert!((1..=3).contains(&script.steps.len()));
        assert_eq!(script.steps[0].tick, 0);
        assert!(script.last_tick() <= 6);
        for pair in script.steps.windows(2) { assert!(pair[1].tick - pair[0].tick <= 3); }
        for entry in &script.steps {
            let Action::SetBlock { pos, state } = &entry.action else { panic!("generated command") };
            assert_eq!(*pos, TRIGGER);
            assert!([AIR, POWER].contains(&state.as_str()));
        }
    }
    let directed = Script::new(vec![step(0, POWER), step(1, AIR)]);
    let directed_outcome = evaluate_control(&directed, &region(), SETTLE_TICKS);
    let DifferentialOutcome::Diverged(expected) = directed_outcome else {
        panic!("the stale source arm must diverge: {directed_outcome:?}");
    };
    assert_eq!(expected.tick, 1);
    assert_eq!(expected.pos, ARM);
    assert_eq!(expected.left.as_deref(), Some(HEAD));
    assert_eq!(expected.right.as_deref(), Some(AIR));

    let outcome = search_and_shrink_with(&domain(), budget(), &region(), SETTLE_TICKS, evaluate_control);
    let SearchOutcome::Found(found) = outcome else { panic!("generated stale arm control: {outcome:?}") };
    assert_eq!(found.original_divergence, expected);
    assert_eq!(found.minimal_divergence, expected);
    assert!(found.shrink_attempts > 0 && found.shrink_attempts <= budget().shrink_attempts);
    assert!(!found.minimal_script.steps.is_empty());
    assert!(found.minimal_script.steps.len() <= found.original_script.steps.len());
    let replay = ReplayCase::from_found(scenario::SCENARIO, budget().seed, SETTLE_TICKS, region(), &found);
    let json = replay.to_json_pretty().expect("encode stale arm replay");
    let decoded = ReplayCase::from_json(&json).expect("decode stale arm replay");
    assert!(matches!(decoded.replay_generated_with(scenario::SCENARIO, &domain(), &region(), SETTLE_TICKS,
        evaluate_control).expect("validate and replay"),
        DifferentialOutcome::Diverged(divergence) if divergence == expected));
    let fixed = decoded.replay_generated_with(scenario::SCENARIO, &domain(), &region(), SETTLE_TICKS,
        |script, probes, settle| run_differential(script, probes, &mut build_model(), &mut build_model(), settle))
        .expect("same replay without the control fault");
    assert!(matches!(fixed, DifferentialOutcome::Agreed), "{fixed:?}");

    let mut forbidden: serde_json::Value = serde_json::from_str(&json).expect("JSON");
    forbidden["steps"][0]["action"]["pos"] = serde_json::json!([0, 1, 0]);
    let forbidden = ReplayCase::from_json(&forbidden.to_string()).expect("valid replay shape");
    let mut setups = 0;
    assert!(forbidden.replay_generated_with(scenario::SCENARIO, &domain(), &region(), SETTLE_TICKS,
        |_, _, _| { setups += 1; DifferentialOutcome::Agreed }).is_err());
    assert_eq!(setups, 0);
    decoded.replay_generated_with(scenario::SCENARIO, &domain(), &region(), SETTLE_TICKS,
        |_, _, _| { setups += 1; DifferentialOutcome::Agreed }).expect("valid policy control");
    assert_eq!(setups, 1);
}

#[cfg(feature = "differential-campaign")]
#[test]
fn piston_campaign_resume_uses_the_same_domain_and_rejects_timing_coverage() {
    use lodestone_fuzz::campaign::{CampaignConfig, CampaignStatus, Checkpoint, Scenario, advance_with};
    use lodestone_fuzz::differential::{OracleFailure, OracleFailureKind, Side};
    let config = CampaignConfig {
        scenario: Scenario::Piston, endpoint: "127.0.0.1:25571".to_owned(),
        seed: Scenario::Piston.default_seed(), cases: 4, shrink_attempts: 8, timing_attempts: 2,
    };
    let mut full = Checkpoint::new(config.clone()).expect("piston campaign");
    let mut expected = Vec::new();
    advance_with(&mut full, 4, |script, probes, settle| {
        assert_eq!(probes, region());
        assert_eq!(settle, SETTLE_TICKS);
        expected.push(script.clone());
        DifferentialOutcome::Agreed
    }, |_| Ok(())).expect("full stream");
    let mut split = Checkpoint::new(config.clone()).expect("split campaign");
    let mut observed = Vec::new();
    advance_with(&mut split, 2, |script, _, _| {
        observed.push(script.clone()); DifferentialOutcome::Agreed
    }, |_| Ok(())).expect("first slice");
    let mut resumed = Checkpoint::from_json(&split.to_json_pretty().expect("checkpoint"), &config)
        .expect("resume piston scenario");
    advance_with(&mut resumed, 2, |script, _, _| {
        observed.push(script.clone()); DifferentialOutcome::Agreed
    }, |_| Ok(())).expect("second slice");
    assert_eq!(observed, expected);
    assert_eq!(resumed, full);
    assert_eq!(full.status, CampaignStatus::Complete);
    let mut failed = Checkpoint::new(config).expect("timing campaign");
    advance_with(&mut failed, 1, |_, _, _| DifferentialOutcome::OracleFailed(OracleFailure {
        tick: 1, side: Side::Right, kind: OracleFailureKind::Timeout,
        message: "injected tick crossing at the arm probe".to_owned(),
    }), |_| Ok(())).expect("bounded timeouts");
    assert_eq!(failed.status, CampaignStatus::OracleFailed);
    assert_eq!(failed.search.oracle_attempts, 2);
    assert_eq!(failed.search.timing_failures, 2);
    assert_eq!(failed.search.accepted_evaluations, 0);
    assert_eq!(failed.search.accepted_ticks, 0);
    assert_eq!(failed.search.accepted_actions, 0);
    assert_eq!(failed.accepted_cases, 0);
}

fn live(script: &Script, probes: &[((i32, i32, i32), Vec<String>)], settle: u64) -> DifferentialOutcome {
    let endpoint = std::env::var("LODESTONE_DIFFERENTIAL_RCON")
        .unwrap_or_else(|_| "127.0.0.1:25571".to_owned());
    generation::retry_oracle_timeouts(3, || scenario::evaluate_at(script, probes, settle, &endpoint))
}

#[test]
#[ignore = "needs a live 26.2 RCON oracle; exact interruption and commit counters"]
fn directed_power_removal_interrupts_the_arm_before_its_commit() {
    let outcome = live(&Script::new(vec![step(0, POWER), step(1, AIR)]), &region(), SETTLE_TICKS);
    assert!(matches!(outcome, DifferentialOutcome::Agreed), "{outcome:?}");
    let pulse = live(&Script::new(vec![step(0, POWER), step(0, AIR)]), &region(), SETTLE_TICKS);
    assert!(matches!(pulse, DifferentialOutcome::Agreed), "same-tick pulse: {pulse:?}");
}

#[test]
#[ignore = "needs a live 26.2 RCON oracle"]
fn generated_piston_candidates_are_shrunk_and_replayed_from_a_fresh_lane() {
    let outcome = search_and_shrink_with(&domain(), SearchBudget { cases: 8, ..budget() },
        &region(), SETTLE_TICKS, live);
    match outcome {
        SearchOutcome::NoDivergence { cases_run } => assert_eq!(cases_run, 8),
        SearchOutcome::Found(found) => {
            let replay = ReplayCase::from_found(scenario::SCENARIO, budget().seed, SETTLE_TICKS, region(), &found);
            let json = replay.to_json_pretty().expect("encode live finding");
            let decoded = ReplayCase::from_json(&json).expect("decode live finding");
            let replayed = decoded.replay_generated_with(scenario::SCENARIO, &domain(), &region(), SETTLE_TICKS,
                live).expect("validate live finding");
            assert!(matches!(replayed, DifferentialOutcome::Diverged(value) if value == found.minimal_divergence));
            panic!("piston divergence; minimized replay JSON follows:\n{json}");
        }
        other => panic!("piston live oracle did not complete: {other:?}"),
    }
}
