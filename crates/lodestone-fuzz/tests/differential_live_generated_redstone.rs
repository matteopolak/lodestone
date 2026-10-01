#![cfg(feature = "rcon-oracle")]

#[path = "support/differential_generation.rs"]
mod differential_generation;
use differential_generation as generation;
mod contraption;
#[path = "../src/campaign/live_redstone.rs"]
mod scenario;
use scenario::*;
use differential_generation::{
    ReplayCase, SearchBudget, SearchOutcome, retry_oracle_timeouts, sample_scripts,
    search_and_shrink_with,
};
use lodestone_fuzz::differential::{Action, DifferentialOutcome, Script, WorldOracle, run_differential};
use lodestone_fuzz::differential::rcon::RconOracle;

fn endpoint() -> String {
    std::env::var("LODESTONE_DIFFERENTIAL_RCON").unwrap_or_else(|_| DEFAULT_ADDR.to_owned())
}

fn budget() -> SearchBudget {
    SearchBudget {
        seed: 0x549_0eed,
        cases: 8,
        shrink_attempts: 32,
    }
}

fn evaluate(
    script: &Script,
    probes: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    faulty: bool,
) -> DifferentialOutcome {
    evaluate_at(script, probes, settle_ticks, faulty, &endpoint())
}

fn evaluate_stable(
    script: &Script,
    probes: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    faulty: bool,
) -> DifferentialOutcome {
    retry_oracle_timeouts(TIMING_ATTEMPTS, || evaluate(script, probes, settle_ticks, faulty))
}

fn replay_live_case_with<E>(
    replay: &ReplayCase,
    scenario: &str,
    evaluate: E,
) -> Result<DifferentialOutcome, String>
where
    E: FnMut(&Script, &[((i32, i32, i32), Vec<String>)], u64) -> DifferentialOutcome,
{
    replay.replay_generated_with(scenario, &domain(), &region(), SETTLE_TICKS, evaluate)
}

fn assert_live_replay(replay: &ReplayCase, scenario: &str, faulty: bool) {
    let replayed = replay_live_case_with(replay, scenario, |script, probes, settle_ticks| {
        evaluate_stable(script, probes, settle_ticks, faulty)
    })
    .expect("the minimized replay satisfies the live scenario policy");
    let DifferentialOutcome::Diverged(divergence) = replayed else {
        panic!("the decoded replay did not reproduce its divergence: {replayed:?}");
    };
    assert_eq!(divergence, replay.expected_divergence());
}

const DEFAULT_ADDR: &str = "127.0.0.1:25571";
const REPLAY_ENV: &str = "LODESTONE_DIFFERENTIAL_REPLAY";
const CONTROL_SCENARIO: &str = "generated-live-redstone-faulty-read";
const TIMING_ATTEMPTS: u32 = 3;
const PROBE_LANE: i32 = 13;

#[test]
fn generated_redstone_domain_is_deterministic_and_bounded() {
    let scripts = sample_scripts(&domain(), budget()).expect("sample the fixed generated stream");
    assert_eq!(scripts.len(), budget().cases as usize);
    assert!(scripts.iter().all(|script| script.steps.len() <= domain().max_steps()));
    assert!(scripts.iter().flat_map(|script| &script.steps).all(|step| matches!(
        &step.action,
        Action::SetBlock { pos, state }
            if *pos == contraption::SOURCE && (state == AIR || state == SOURCE)
    )));
}

#[test]
fn live_probe_and_generated_runs_use_disjoint_lanes() {
    assert_ne!(GENERATED_LANE, PROBE_LANE);
    assert_ne!(origin(GENERATED_LANE), origin(PROBE_LANE));
}

#[test]
fn untrusted_redstone_replay_never_starts_the_live_evaluator() {
    let base = serde_json::json!({
        "format_version": 1,
        "scenario": SCENARIO,
        "seed": budget().seed,
        "case_index": 0,
        "settle_ticks": SETTLE_TICKS,
        "region": region().into_iter().map(|(pos, candidates)| serde_json::json!({"pos": pos, "candidates": candidates})).collect::<Vec<_>>(),
        "steps": [{"tick": 0, "action": {"kind": "set_block", "pos": contraption::SOURCE, "state": SOURCE}}],
        "divergence": {"tick": 0, "pos": contraption::PREDICTED[0].0, "left": "minecraft:redstone_wire[power=0]", "right": "minecraft:redstone_wire[power=15]"}
    });
    let mut wrong_scenario = base.clone();
    wrong_scenario["scenario"] = serde_json::json!("wrong");
    let mut command = base.clone();
    command["steps"][0]["action"] = serde_json::json!({"kind": "run_command", "command": "say unsafe"});
    let mut position = base.clone();
    position["steps"][0]["action"]["pos"] = serde_json::json!([1, 0, 0]);
    let mut state = base.clone();
    state["steps"][0]["action"]["state"] = serde_json::json!("minecraft:lava");
    let mut changed_region = base.clone();
    changed_region["region"][0]["candidates"] = serde_json::json!(["minecraft:air"]);
    let mut horizon = base.clone();
    horizon["divergence"]["tick"] = serde_json::json!(999);

    let mut evaluator_starts = 0;
    for json in [wrong_scenario, command, position, state, changed_region, horizon] {
        let replay = ReplayCase::from_json(&serde_json::to_string(&json).expect("serialize replay"))
            .expect("the malformed case remains replay format v1");
        replay_live_case_with(&replay, SCENARIO, |_script, _probes, _settle_ticks| {
            evaluator_starts += 1;
            DifferentialOutcome::Agreed
        })
        .expect_err("unsafe replay must be rejected before RCON setup");
    }
    assert_eq!(evaluator_starts, 0);

    let valid = ReplayCase::from_json(&serde_json::to_string(&base).expect("serialize valid replay"))
        .expect("decode valid replay");
    replay_live_case_with(&valid, SCENARIO, |_script, _probes, _settle_ticks| {
        evaluator_starts += 1;
        DifferentialOutcome::Agreed
    })
    .expect("the detector control accepts a valid replay");
    assert_eq!(evaluator_starts, 1);
}

#[test]
fn faulty_model_control_is_found_shrunk_and_json_replayable() {
    let outcome = search_and_shrink_with(&domain(), budget(), &region(), SETTLE_TICKS, |script, probes, settle| {
        let mut left = FaultyRead { inner: build_model(origin(GENERATED_LANE)) };
        let mut right = build_model(origin(GENERATED_LANE));
        run_differential(script, probes, &mut left, &mut right, settle)
    });
    let SearchOutcome::Found(found) = outcome else {
        panic!("faulty redstone read must be detected: {outcome:?}");
    };
    assert_eq!(found.original_divergence, found.minimal_divergence);
    assert!(found.shrink_attempts > 0);
    let replay = ReplayCase::from_found(CONTROL_SCENARIO, budget().seed, SETTLE_TICKS, region(), &found);
    let json = replay.to_json_pretty().expect("encode fault replay");
    let decoded = ReplayCase::from_json(&json).expect("decode fault replay");
    let outcome = replay_live_case_with(&decoded, CONTROL_SCENARIO, |script, probes, settle| {
        let mut left = FaultyRead { inner: build_model(origin(GENERATED_LANE)) };
        let mut right = build_model(origin(GENERATED_LANE));
        run_differential(script, probes, &mut left, &mut right, settle)
    })
    .expect("the fault control replay is trusted");
    let DifferentialOutcome::Diverged(divergence) = outcome else {
        panic!("the decoded fault control did not diverge: {outcome:?}");
    };
    assert_eq!(divergence, decoded.expected_divergence());
}

#[test]
#[ignore = "needs a live vanilla 26.2 RCON oracle"]
fn rcon_probe_distinguishes_powered_and_unpowered_dust() {
    let probe_origin = origin(PROBE_LANE);
    let mut live = RconOracle::connect(endpoint(), PASSWORD, probe_origin).expect("connect live oracle");
    build_live_rig(&mut live, probe_origin).expect("build live rig");
    live.apply(&Action::SetBlock { pos: contraption::SOURCE, state: SOURCE.to_owned() })
        .expect("energize source");
    let cell = contraption::PREDICTED[0].0;
    assert_eq!(live.block_state(cell, &["minecraft:redstone_wire[power=15]".to_owned()]).expect("probe power 15").as_deref(), Some("minecraft:redstone_wire[power=15]"));
    assert_eq!(live.block_state(cell, &["minecraft:redstone_wire[power=0]".to_owned()]).expect("probe power 0"), None);
    tear_down(&mut live, probe_origin).expect("tear down live rig");
}

#[test]
#[ignore = "needs a live vanilla 26.2 RCON oracle"]
fn generated_redstone_candidates_are_shrunk_and_replayed_from_a_fresh_lane() {
    if let Some(path) = std::env::var_os(REPLAY_ENV) {
        let json = std::fs::read_to_string(&path).expect("read replay JSON");
        let replay = ReplayCase::from_json(&json).expect("decode replay JSON");
        assert_live_replay(&replay, SCENARIO, false);
        return;
    }

    let control = search_and_shrink_with(&domain(), budget(), &region(), SETTLE_TICKS, |script, probes, settle| {
        evaluate_stable(script, probes, settle, true)
    });
    let SearchOutcome::Found(found) = control else {
        panic!("the faulty read must make the live generator diverge: {control:?}");
    };
    assert_eq!(found.original_divergence, found.minimal_divergence);
    assert!(found.shrink_attempts > 0);
    assert_ne!(found.original_script.steps, found.minimal_script.steps);
    let replay = ReplayCase::from_found(CONTROL_SCENARIO, budget().seed, SETTLE_TICKS, region(), &found);
    let decoded = ReplayCase::from_json(&replay.to_json_pretty().expect("encode fault replay"))
        .expect("decode fault replay");
    assert_live_replay(&decoded, CONTROL_SCENARIO, true);

    let fixed = search_and_shrink_with(&domain(), budget(), &region(), SETTLE_TICKS, |script, probes, settle| {
        evaluate_stable(script, probes, settle, false)
    });
    match fixed {
        SearchOutcome::NoDivergence { cases_run } => assert_eq!(cases_run, budget().cases),
        SearchOutcome::Found(found) => {
            let replay = ReplayCase::from_found(SCENARIO, budget().seed, SETTLE_TICKS, region(), &found);
            let json = replay.to_json_pretty().expect("encode replay");
            assert_live_replay(&ReplayCase::from_json(&json).expect("decode replay"), SCENARIO, false);
            panic!("gameplay divergence; minimized replay JSON follows:\n{json}");
        }
        other => panic!("live generated redstone oracle failure: {other:?}"),
    }
}
