//! Generated and semantically-shrunk fluid scripts against a live reference
//! server, with every candidate run from a checked clean baseline.
//!
//! # Running it
//!
//! ```text
//! ./scripts/live-oracles/creative.sh
//! cargo test -p lodestone-fuzz --features rcon-oracle \
//!     --test differential_live_generated_fluid -- --ignored --nocapture
//! ```
//!
//! `LODESTONE_DIFFERENTIAL_RCON` overrides the default `127.0.0.1:25571`
//! endpoint. `LODESTONE_DIFFERENTIAL_REPLAY` may name a replay JSON file; in
//! that mode generation is skipped and the explicit minimized script in the
//! file is run against a freshly reset lane.
#![cfg(feature = "rcon-oracle")]

// This integration target consumes the live-only half of the shared support;
// its hermetic sibling exercises the remaining public helpers.
#[allow(dead_code)]
#[path = "support/differential_generation.rs"]
mod differential_generation;

use std::collections::HashMap;
use std::io;

use differential_generation::{
    GenerationDomain, ReplayCase, SearchBudget, SearchOutcome, retry_oracle_timeouts,
    sample_scripts, search_and_shrink_with,
};
use lodestone_fuzz::differential::fluid::FluidModelOracle;
use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, Divergence, OracleFailure, OracleFailureKind, Script, Side,
    TICK_MILLIS, WorldOracle, run_differential,
};

const DEFAULT_ADDR: &str = "127.0.0.1:25571";
const PASSWORD: &str = "lodestone";
const REPLAY_ENV: &str = "LODESTONE_DIFFERENTIAL_REPLAY";
const SCENARIO: &str = "generated-live-fluid";
const CONTROL_SCENARIO: &str = "generated-live-fluid-control";
const ORIGIN: (i32, i32, i32) = (120_000, 200, 48_000);
const CELLS: i32 = 3;
const RESET_TICKS: u64 = 6;
const SETTLE_TICKS: u64 = 6;
const TIMING_ATTEMPTS: u32 = 3;
const AIR: &str = "minecraft:air";
const STONE: &str = "minecraft:stone";
const WATER: &str = "minecraft:water[level=0]";

fn endpoint() -> String {
    std::env::var("LODESTONE_DIFFERENTIAL_RCON").unwrap_or_else(|_| DEFAULT_ADDR.to_owned())
}

fn domain() -> GenerationDomain {
    GenerationDomain::new(
        vec![(0, 0, 0)],
        vec![AIR.to_owned(), WATER.to_owned(), WATER.to_owned()],
        3,
        3,
    )
    .expect("the generated live-fluid domain is valid")
}

fn budget() -> SearchBudget {
    SearchBudget {
        seed: 0x549_11e,
        cases: 8,
        shrink_attempts: 32,
    }
}

fn candidates() -> Vec<String> {
    vec![AIR.to_owned(), "minecraft:water".to_owned()]
}

fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    (0..=CELLS).map(|x| ((x, 0, 0), candidates())).collect()
}

fn verify_block<O: WorldOracle>(
    oracle: &mut O,
    pos: (i32, i32, i32),
    expected: &str,
    alternative: &str,
) -> Result<(), io::Error> {
    let candidates = vec![expected.to_owned(), alternative.to_owned()];
    let observed = oracle
        .block_state(pos, &candidates)
        .map_err(|error| io::Error::other(format!("probe {pos:?}: {error}")))?;
    if observed.as_deref() != Some(expected) {
        return Err(io::Error::other(format!(
            "baseline probe {pos:?} expected {expected:?}, observed {observed:?}"
        )));
    }
    Ok(())
}

fn verify_live_baseline<O: WorldOracle>(oracle: &mut O) -> Result<(), io::Error> {
    for x in 0..=CELLS {
        verify_block(oracle, (x, 0, 0), AIR, "minecraft:water")?;
        for pos in [(x, -1, 0), (x, 1, 0), (x, 0, -1), (x, 0, 1)] {
            verify_block(oracle, pos, STONE, AIR)?;
        }
    }
    Ok(())
}

fn failure(tick: u64, error: io::Error, context: &str) -> DifferentialOutcome {
    let kind = if error.kind() == io::ErrorKind::TimedOut {
        OracleFailureKind::Timeout
    } else {
        OracleFailureKind::Failure
    };
    DifferentialOutcome::OracleFailed(OracleFailure {
        tick,
        side: Side::Right,
        kind,
        message: format!("{context}: {error}"),
    })
}

fn command(oracle: &mut RconOracle, command: String, context: &str) -> Result<(), io::Error> {
    oracle
        .apply(&Action::RunCommand(command))
        .map_err(|error| io::Error::new(error.kind(), format!("{context}: {error}")))
}

fn fill_box(oracle: &mut RconOracle, state: &str) -> Result<(), io::Error> {
    let (ox, oy, oz) = ORIGIN;
    command(
        oracle,
        format!(
            "fill {} {} {} {} {} {} {state}",
            ox - 1,
            oy - 1,
            oz - 1,
            ox + CELLS + 2,
            oy + 1,
            oz + 1
        ),
        "clear the generated-fluid lane",
    )
}

fn reset_and_build_vanilla(oracle: &mut RconOracle) -> Result<(), io::Error> {
    let (ox, oy, oz) = ORIGIN;
    command(
        oracle,
        format!("forceload add {ox} {oz} {} {oz}", ox + CELLS + 2),
        "force-load the generated-fluid lane",
    )?;

    fill_box(oracle, AIR)?;
    oracle.reset_baseline()?;
    for _ in 0..RESET_TICKS {
        oracle.advance_tick()?;
    }

    fill_box(oracle, STONE)?;
    command(
        oracle,
        format!("fill {ox} {oy} {oz} {} {oy} {oz} {AIR}", ox + CELLS + 1),
        "carve the generated-fluid channel",
    )?;

    verify_live_baseline(oracle)?;

    // Move away from the tick boundary before anchoring the candidate. The
    // fixed live comparisons use the same half-tick margin.
    oracle.advance_tick()?;
    std::thread::sleep(TICK_MILLIS / 2);
    oracle.reset_baseline()?;
    Ok(())
}

fn build_model() -> FluidModelOracle {
    let mut model = FluidModelOracle::new((0, 0, 0), -1, STONE);
    for x in -1..=CELLS + 2 {
        for z in [-1, 1] {
            for y in [0, 1] {
                model.place_static((x, y, z), STONE);
            }
        }
        model.place_static((x, 1, 0), STONE);
    }
    model
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupStep {
    Clear,
    ResetClock,
    Advance,
    Release,
}

impl CleanupStep {
    fn label(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::ResetClock => "reset clock",
            Self::Advance => "advance",
            Self::Release => "release force-load",
        }
    }
}

fn run_cleanup_steps<F>(reset_ticks: u64, mut run: F) -> Result<(), io::Error>
where
    F: FnMut(CleanupStep) -> Result<(), io::Error>,
{
    let mut failures = Vec::new();
    for step in [CleanupStep::Clear, CleanupStep::ResetClock] {
        if let Err(error) = run(step) {
            failures.push(format!("{}: {error}", step.label()));
        }
    }
    for _ in 0..reset_ticks {
        if let Err(error) = run(CleanupStep::Advance) {
            failures.push(format!("{}: {error}", CleanupStep::Advance.label()));
            break;
        }
    }
    if let Err(error) = run(CleanupStep::Release) {
        failures.push(format!("{}: {error}", CleanupStep::Release.label()));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(failures.join("; ")))
    }
}

fn tear_down(oracle: &mut RconOracle) -> Result<(), io::Error> {
    let (ox, _, oz) = ORIGIN;
    run_cleanup_steps(RESET_TICKS, |step| match step {
        CleanupStep::Clear => fill_box(oracle, AIR),
        CleanupStep::ResetClock => oracle.reset_baseline(),
        CleanupStep::Advance => oracle.advance_tick(),
        CleanupStep::Release => command(
            oracle,
            format!("forceload remove {ox} {oz} {} {oz}", ox + CELLS + 2),
            "release the generated-fluid lane",
        ),
    })
}

struct FaultyRead {
    inner: FluidModelOracle,
}

impl WorldOracle for FaultyRead {
    type Error = std::convert::Infallible;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        self.inner.apply(action)
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        self.inner.advance_tick()
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        let observed = self.inner.block_state(pos, candidates)?;
        if pos == (1, 0, 0) && observed.as_deref() == Some("minecraft:water") {
            return Ok(Some(AIR.to_owned()));
        }
        Ok(observed)
    }
}

fn evaluate(
    script: &Script,
    region: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    faulty: bool,
) -> DifferentialOutcome {
    let mut vanilla = match RconOracle::connect(endpoint(), PASSWORD, ORIGIN) {
        Ok(oracle) => oracle,
        Err(error) => return failure(0, error, "connect to the live reference oracle"),
    };
    let final_tick = script.last_tick() + settle_ticks;
    let mut outcome = match reset_and_build_vanilla(&mut vanilla) {
        Err(error) => failure(0, error, "reset the live candidate"),
        Ok(()) => {
            let model = build_model();
            let comparison = if faulty {
                let mut model = FaultyRead { inner: model };
                run_differential(script, region, &mut model, &mut vanilla, settle_ticks)
            } else {
                let mut model = model;
                run_differential(script, region, &mut model, &mut vanilla, settle_ticks)
            };
            if vanilla.missed_deadlines() == 0 {
                comparison
            } else {
                let missed = vanilla.missed_deadlines();
                DifferentialOutcome::OracleFailed(OracleFailure {
                    tick: final_tick,
                    side: Side::Right,
                    kind: OracleFailureKind::Timeout,
                    message: format!(
                        "live reference crossed {missed} unobserved tick deadlines; rerun without host contention"
                    ),
                })
            }
        }
    };
    if let Err(error) = tear_down(&mut vanilla) {
        match &mut outcome {
            DifferentialOutcome::OracleFailed(failure) => {
                failure.message.push_str(&format!("; cleanup also failed: {error}"));
            }
            DifferentialOutcome::Agreed | DifferentialOutcome::Diverged(_) => {
                outcome = failure(final_tick, error, "tear down the live candidate");
            }
        }
    }
    outcome
}

fn evaluate_stable(
    script: &Script,
    region: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    faulty: bool,
) -> DifferentialOutcome {
    retry_oracle_timeouts(TIMING_ATTEMPTS, || {
        evaluate(script, region, settle_ticks, faulty)
    })
}

fn replay_live_case_with<E>(
    replay: &ReplayCase,
    expected_scenario: &str,
    evaluate: E,
) -> Result<DifferentialOutcome, String>
where
    E: FnMut(&Script, &[((i32, i32, i32), Vec<String>)], u64) -> DifferentialOutcome,
{
    replay.replay_generated_with(
        expected_scenario,
        &domain(),
        &region(),
        SETTLE_TICKS,
        evaluate,
    )
}

fn assert_replay(replay: &ReplayCase, expected_scenario: &str, faulty: bool) {
    let replayed = replay_live_case_with(replay, expected_scenario, |script, region, settle_ticks| {
        evaluate_stable(script, region, settle_ticks, faulty)
    })
    .expect("the replay must satisfy the live scenario policy");
    let DifferentialOutcome::Diverged(divergence) = replayed else {
        panic!("the minimized replay did not reproduce its gameplay divergence: {replayed:?}");
    };
    assert_eq!(divergence, replay.expected_divergence());
}

#[test]
fn the_fixed_live_search_stream_reaches_the_delayed_fluid_control() {
    let scripts = sample_scripts(&domain(), budget()).expect("sample the fixed live stream");
    assert!(
        scripts.iter().any(|script| {
            script.steps.last().is_some_and(|step| {
                step.action
                    == (Action::SetBlock {
                        pos: (0, 0, 0),
                        state: WATER.to_owned(),
                    })
            })
        }),
        "the fixed stream must leave water in the source long enough for the downstream fault control"
    );
}

#[test]
fn untrusted_replay_is_rejected_before_the_live_evaluator_starts() {
    let base = serde_json::json!({
        "format_version": 1,
        "scenario": SCENARIO,
        "seed": 0,
        "case_index": 0,
        "settle_ticks": SETTLE_TICKS,
        "region": [
            { "pos": [0, 0, 0], "candidates": [AIR, "minecraft:water"] },
            { "pos": [1, 0, 0], "candidates": [AIR, "minecraft:water"] },
            { "pos": [2, 0, 0], "candidates": [AIR, "minecraft:water"] },
            { "pos": [3, 0, 0], "candidates": [AIR, "minecraft:water"] }
        ],
        "steps": [{
            "tick": 0,
            "action": { "kind": "set_block", "pos": [0, 0, 0], "state": WATER }
        }],
        "divergence": {
            "tick": 5,
            "pos": [1, 0, 0],
            "left": AIR,
            "right": "minecraft:water"
        }
    });
    let mut raw_command = base.clone();
    raw_command["steps"][0]["action"] = serde_json::json!({
        "kind": "run_command",
        "command": "fill 0 0 0 100 100 100 minecraft:lava"
    });
    let mut out_of_lane = base.clone();
    out_of_lane["steps"][0]["action"]["pos"] = serde_json::json!([1, 0, 0]);

    let mut evaluator_starts = 0;
    for input in [raw_command, out_of_lane] {
        let replay = ReplayCase::from_json(&serde_json::to_string(&input).expect("serialize replay"))
            .expect("the unsafe input still uses replay format v1");
        replay_live_case_with(&replay, SCENARIO, |_script, _region, _settle_ticks| {
            evaluator_starts += 1;
            DifferentialOutcome::Agreed
        })
        .expect_err("unsafe replay must be rejected before live setup");
    }
    assert_eq!(evaluator_starts, 0, "rejection must happen before any RCON work");

    let valid = ReplayCase::from_json(&serde_json::to_string(&base).expect("serialize valid replay"))
        .expect("decode valid replay");
    let accepted = replay_live_case_with(&valid, SCENARIO, |_script, _region, _settle_ticks| {
        evaluator_starts += 1;
        DifferentialOutcome::Agreed
    })
    .expect("the matching artifact must reach the live evaluator boundary");
    assert!(matches!(accepted, DifferentialOutcome::Agreed));
    assert_eq!(
        evaluator_starts, 1,
        "the detector control must not reject every replay"
    );
}

#[derive(Default)]
struct BaselineWorld {
    blocks: HashMap<(i32, i32, i32), String>,
    probed: Vec<(i32, i32, i32)>,
}

impl WorldOracle for BaselineWorld {
    type Error = std::convert::Infallible;

    fn apply(&mut self, _action: &Action) -> Result<(), Self::Error> {
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        self.probed.push(pos);
        let actual = self.blocks.get(&pos).map_or(AIR, String::as_str);
        Ok(candidates.iter().find(|candidate| candidate.as_str() == actual).cloned())
    }
}

fn complete_baseline() -> BaselineWorld {
    let mut world = BaselineWorld::default();
    for x in 0..=CELLS {
        for pos in [(x, -1, 0), (x, 1, 0), (x, 0, -1), (x, 0, 1)] {
            world.blocks.insert(pos, STONE.to_owned());
        }
    }
    world
}

#[test]
fn baseline_validation_covers_every_watched_cell_wall_floor_and_roof() {
    let mut complete = complete_baseline();
    verify_live_baseline(&mut complete).expect("the complete lane is valid");
    let expected = (0..=CELLS)
        .flat_map(|x| [(x, 0, 0), (x, -1, 0), (x, 1, 0), (x, 0, -1), (x, 0, 1)])
        .collect::<Vec<_>>();
    assert_eq!(complete.probed, expected);

    let mut stale_downstream = complete_baseline();
    stale_downstream.blocks.insert((CELLS, 0, 0), WATER.to_owned());
    let error = verify_live_baseline(&mut stale_downstream)
        .expect_err("stale water in the last watched cell must fail reset validation");
    assert!(error.to_string().contains(&format!("({CELLS}, 0, 0)")));

    let mut missing_wall = complete_baseline();
    missing_wall.blocks.remove(&(CELLS, 0, 1));
    let error = verify_live_baseline(&mut missing_wall)
        .expect_err("a missing far wall must fail reset validation");
    assert!(error.to_string().contains(&format!("({CELLS}, 0, 1)")));
}

#[test]
fn cleanup_attempts_clear_and_release_even_when_intermediate_steps_fail() {
    let mut attempted = Vec::new();
    let error = run_cleanup_steps(RESET_TICKS, |step| {
        attempted.push(step);
        match step {
            CleanupStep::Clear | CleanupStep::Advance => Err(io::Error::other("injected cleanup failure")),
            CleanupStep::ResetClock | CleanupStep::Release => Ok(()),
        }
    })
    .expect_err("the injected failures must be reported");

    assert_eq!(
        attempted,
        vec![
            CleanupStep::Clear,
            CleanupStep::ResetClock,
            CleanupStep::Advance,
            CleanupStep::Release,
        ]
    );
    assert!(error.to_string().contains("clear"));
    assert!(error.to_string().contains("advance"));
}

#[test]
#[ignore = "needs a live vanilla 26.2 RCON oracle"]
fn generated_fluid_candidates_are_isolated_shrunk_and_replayable() {
    if let Some(path) = std::env::var_os(REPLAY_ENV) {
        let json = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read replay JSON from {path:?}: {error}"));
        let replay = ReplayCase::from_json(&json).expect("decode replay JSON");
        assert_replay(&replay, SCENARIO, false);
        return;
    }

    let control = search_and_shrink_with(
        &domain(),
        budget(),
        &region(),
        SETTLE_TICKS,
        |script, region, settle_ticks| evaluate_stable(script, region, settle_ticks, true),
    );
    let SearchOutcome::Found(control) = control else {
        panic!("the faulty-read control must produce a gameplay divergence, got {control:?}");
    };
    assert_eq!(control.original_divergence.tick, control.minimal_divergence.tick);
    let control_replay = ReplayCase::from_found(
        CONTROL_SCENARIO,
        budget().seed,
        SETTLE_TICKS,
        region(),
        &control,
    );
    let control_json = control_replay.to_json_pretty().expect("encode control replay JSON");
    let decoded = ReplayCase::from_json(&control_json).expect("decode control replay JSON");
    assert_replay(&decoded, CONTROL_SCENARIO, true);

    let outcome = search_and_shrink_with(
        &domain(),
        budget(),
        &region(),
        SETTLE_TICKS,
        |script, region, settle_ticks| evaluate_stable(script, region, settle_ticks, false),
    );
    match outcome {
        SearchOutcome::NoDivergence { cases_run } => assert_eq!(cases_run, budget().cases),
        SearchOutcome::Found(found) => {
            let replay = ReplayCase::from_found(
                SCENARIO,
                budget().seed,
                SETTLE_TICKS,
                region(),
                &found,
            );
            let json = replay.to_json_pretty().expect("encode replay JSON");
            assert_replay(
                &ReplayCase::from_json(&json).expect("decode replay JSON"),
                SCENARIO,
                false,
            );
            panic!("gameplay divergence; minimized replay JSON follows:\n{json}");
        }
        SearchOutcome::OracleFailed {
            case_index,
            during_shrink,
            failure,
        } => panic!(
            "oracle {:?} on {:?} at tick {} (case {case_index}, during_shrink={during_shrink}): {}",
            failure.kind, failure.side, failure.tick, failure.message
        ),
        SearchOutcome::InvalidConfiguration { message } => {
            panic!("invalid generated-live configuration: {message}")
        }
    }
}

/// The fixed-tree half of the historical-reversion control. It runs the same
/// bounded generated stream as the mutation half, but requires every candidate
/// to agree with the live oracle before the wrapper applies any mutation.
#[test]
#[ignore = "needs a live vanilla 26.2 RCON oracle"]
fn fixed_generated_fluid_stream_has_no_live_divergence() {
    let outcome = search_and_shrink_with(
        &domain(),
        budget(),
        &region(),
        SETTLE_TICKS,
        |script, region, settle_ticks| evaluate_stable(script, region, settle_ticks, false),
    );
    match outcome {
        SearchOutcome::NoDivergence { cases_run } => assert_eq!(cases_run, budget().cases),
        other => panic!("the fixed generated-fluid stream must agree with live vanilla: {other:?}"),
    }
}

/// A historical-reversion control for the generated live search. The wrapper
/// in `scripts/historical-fluid-reversion.sh` runs this only after restoring
/// the former seven-cell delay-one seed in a detached worktree. It must then
/// find the first downstream cell one elapsed tick ahead of the live oracle,
/// shrink the generated script without changing that first disagreement, and
/// replay the serialized result from a fresh lane.
#[test]
#[ignore = "requires the historical fluid seed reversion in a detached worktree and a live vanilla 26.2 RCON oracle"]
fn historical_reversion_is_found_shrunk_and_replayed_against_live_vanilla() {
    let outcome = search_and_shrink_with(
        &domain(),
        budget(),
        &region(),
        SETTLE_TICKS,
        |script, region, settle_ticks| evaluate_stable(script, region, settle_ticks, false),
    );
    let SearchOutcome::Found(found) = outcome else {
        panic!(
            "the restored delay-one seven-cell seed must diverge from the live oracle: {outcome:?}"
        );
    };

    let expected = Divergence {
        tick: 0,
        pos: (1, 0, 0),
        left: Some("minecraft:water".to_owned()),
        right: Some(AIR.to_owned()),
    };
    assert_eq!(
        found.original_divergence, expected,
        "the historical seed reaches the first downstream cell after one elapsed tick"
    );
    assert_eq!(
        found.minimal_divergence, expected,
        "shrinking must preserve the recorded gameplay divergence, not merely find another one"
    );
    assert!(
        found.shrink_attempts > 0,
        "the bounded search must exercise at least one semantic shrink candidate"
    );
    assert_ne!(
        found.minimal_script.steps, found.original_script.steps,
        "the recorded minimal script must be an accepted semantic shrink, not the original candidate"
    );

    let replay = ReplayCase::from_found(
        "generated-live-fluid-historical-reversion",
        budget().seed,
        SETTLE_TICKS,
        region(),
        &found,
    );
    let json = replay.to_json_pretty().expect("encode historical replay JSON");
    let replay = ReplayCase::from_json(&json).expect("decode historical replay JSON");
    assert_replay(&replay, "generated-live-fluid-historical-reversion", false);
}
