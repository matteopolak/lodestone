#![cfg(feature = "rcon-oracle")]

#[path = "support/differential_generation.rs"]
mod generation;
#[path = "../src/campaign/fluid_lane.rs"]
mod fluid_lane;
#[path = "../src/campaign/live_waterlogging.rs"]
mod scenario;

use std::collections::HashMap;
use std::convert::Infallible;

use generation::{ReplayCase, SearchBudget, SearchOutcome, sample_scripts, search_and_shrink_with};
use fluid_lane::{AIR, STONE, WATER};
use lodestone_fuzz::differential::fluid::FluidModelOracle;
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, Script, ScriptStep, WorldOracle, run_differential,
};
use scenario::{SLAB, SLAB_DRY, SLAB_WET, SETTLE_TICKS, build_model, domain, region};

fn budget() -> SearchBudget {
    SearchBudget { seed: 0x549_a7e, cases: 64, shrink_attempts: 32 }
}

struct WrongSlab {
    inner: FluidModelOracle,
}

impl WorldOracle for WrongSlab {
    type Error = Infallible;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        self.inner.apply(action)
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        self.inner.advance_tick()
    }

    fn block_state(&mut self, pos: (i32, i32, i32), candidates: &[String]) -> Result<Option<String>, Self::Error> {
        let observed = self.inner.block_state(pos, candidates)?;
        if pos == SLAB && observed.as_deref() == Some(SLAB_DRY) {
            return Ok(Some(SLAB_WET.to_owned()));
        }
        Ok(observed)
    }
}

fn evaluate_control(script: &Script, probes: &[((i32, i32, i32), Vec<String>)], settle: u64) -> DifferentialOutcome {
    run_differential(script, probes, &mut build_model(), &mut WrongSlab { inner: build_model() }, settle)
}

#[test]
fn generated_trench_edits_are_bounded_repeatable_and_keep_the_slab_fixed() {
    let scripts = sample_scripts(&domain(), budget()).expect("finite stream");
    assert_eq!(scripts, sample_scripts(&domain(), budget()).expect("repeat the stream"));
    assert_eq!(scripts.len(), 64);
    assert_eq!(domain().max_steps(), 6);
    assert_eq!(domain().max_tick_gap(), 6);
    for script in &scripts {
        assert!((1..=6).contains(&script.steps.len()));
        assert_eq!(script.steps[0].tick, 0);
        assert!(script.last_tick() <= 30);
        for steps in script.steps.windows(2) {
            assert!((0..=6).contains(&(steps[1].tick - steps[0].tick)));
        }
        for step in &script.steps {
            let Action::SetBlock { pos, state } = &step.action else { panic!("no generated commands") };
            assert!([(0, 0, 0), (2, 0, 0)].contains(pos));
            assert!([AIR, WATER].contains(&state.as_str()));
        }
    }
    for pos in [(0, 0, 0), (2, 0, 0)] {
        assert!(scripts.iter().flat_map(|script| &script.steps).any(|step|
            step.action == (Action::SetBlock { pos, state: WATER.to_owned() })));
    }
    let probes = region();
    assert_eq!(probes.len(), 3);
    assert_eq!(probes[1], (SLAB, vec![SLAB_DRY.to_owned(), SLAB_WET.to_owned()]));
    for index in [0, 2] {
        assert_eq!(probes[index].1.len(), 17);
        assert_eq!(probes[index].1[0], AIR);
        for level in 0..=15 {
            assert_eq!(probes[index].1[level + 1], format!("minecraft:water[level={level}]"));
        }
    }
}

#[test]
fn the_wrong_slab_control_is_shrunk_and_replayed_with_its_first_divergence() {
    let outcome = search_and_shrink_with(&domain(), budget(), &region(), SETTLE_TICKS, evaluate_control);
    let SearchOutcome::Found(found) = outcome else { panic!("the slab read control must diverge: {outcome:?}") };
    assert_eq!(found.original_divergence.tick, 0);
    assert_eq!(found.original_divergence.pos, SLAB);
    assert_eq!(found.original_divergence.left.as_deref(), Some(SLAB_DRY));
    assert_eq!(found.original_divergence.right.as_deref(), Some(SLAB_WET));
    assert_eq!(found.minimal_divergence, found.original_divergence);
    assert!(found.shrink_attempts > 0 && found.shrink_attempts <= budget().shrink_attempts);
    let replay = ReplayCase::from_found(scenario::SCENARIO, budget().seed, SETTLE_TICKS, region(), &found);
    let decoded = ReplayCase::from_json(&replay.to_json_pretty().expect("encode control")).expect("decode control");
    assert!(matches!(decoded.replay_generated_with(scenario::SCENARIO, &domain(), &region(), SETTLE_TICKS,
        evaluate_control).expect("admit matching policy"),
        DifferentialOutcome::Diverged(divergence) if divergence == found.minimal_divergence));
    let agreement = decoded.replay_generated_with(scenario::SCENARIO, &domain(), &region(), SETTLE_TICKS,
        |script, probes, settle| run_differential(script, probes, &mut build_model(), &mut build_model(), settle))
        .expect("remove the fault from the same replay");
    assert!(matches!(agreement, DifferentialOutcome::Agreed), "{agreement:?}");
}

#[test]
fn waterlogging_replay_cannot_edit_the_slab_or_add_flowing_water() {
    let outcome = search_and_shrink_with(&domain(), budget(), &region(), SETTLE_TICKS, evaluate_control);
    let SearchOutcome::Found(found) = outcome else { panic!("control finding: {outcome:?}") };
    let replay = ReplayCase::from_found(scenario::SCENARIO, budget().seed, SETTLE_TICKS, region(), &found);
    let base: serde_json::Value = serde_json::from_str(&replay.to_json_pretty().expect("encode")).expect("JSON");
    let mut changed_slab = base.clone();
    changed_slab["steps"][0]["action"]["pos"] = serde_json::json!([1, 0, 0]);
    let mut flowing_water = base.clone();
    flowing_water["steps"][0]["action"]["state"] = serde_json::json!("minecraft:water[level=3]");
    let mut changed_type = base.clone();
    changed_type["region"][1]["candidates"][0] = serde_json::json!("minecraft:oak_slab[type=top,waterlogged=false]");
    let mut setups = 0;
    for value in [changed_slab, flowing_water, changed_type] {
        let decoded = ReplayCase::from_json(&value.to_string()).expect("valid JSON shape");
        assert!(decoded.replay_generated_with(scenario::SCENARIO, &domain(), &region(), SETTLE_TICKS,
            |_, _, _| { setups += 1; DifferentialOutcome::Agreed }).is_err());
    }
    assert_eq!(setups, 0);
    replay.replay_generated_with(scenario::SCENARIO, &domain(), &region(), SETTLE_TICKS,
        |_, _, _| { setups += 1; DifferentialOutcome::Agreed }).expect("matching policy control");
    assert_eq!(setups, 1);
}

#[derive(Default)]
struct BaselineWorld {
    blocks: HashMap<(i32, i32, i32), String>,
}

impl WorldOracle for BaselineWorld {
    type Error = Infallible;
    fn apply(&mut self, _: &Action) -> Result<(), Self::Error> { Ok(()) }
    fn advance_tick(&mut self) -> Result<(), Self::Error> { Ok(()) }
    fn block_state(&mut self, pos: (i32, i32, i32), candidates: &[String]) -> Result<Option<String>, Self::Error> {
        let state = self.blocks.get(&pos).map_or(AIR, String::as_str);
        Ok(candidates.iter().find(|candidate| candidate.as_str() == state).cloned())
    }
}

fn baseline() -> BaselineWorld {
    let mut world = BaselineWorld::default();
    for x in 0..=2 {
        for pos in [(x, -1, 0), (x, 0, -1), (x, 0, 1)] {
            world.blocks.insert(pos, STONE.to_owned());
        }
    }
    for x in [-1, 3] { world.blocks.insert((x, 0, 0), STONE.to_owned()); }
    world.blocks.insert(SLAB, SLAB_DRY.to_owned());
    world
}

#[test]
fn baseline_validation_detects_stale_water_wrong_slab_type_and_closed_top() {
    scenario::verify_live_baseline(&mut baseline()).expect("valid open-top baseline");
    for (pos, state) in [
        ((2, 0, 0), "minecraft:water[level=6]"),
        (SLAB, "minecraft:oak_slab[type=top,waterlogged=false]"),
        ((1, 1, 0), STONE),
        ((2, 0, 1), AIR),
        ((3, 0, 0), AIR),
    ] {
        let mut changed = baseline();
        changed.blocks.insert(pos, state.to_owned());
        assert!(scenario::verify_live_baseline(&mut changed).is_err(), "missed {pos:?}={state}");
    }
}

fn step(tick: u64, pos: (i32, i32, i32), state: &str) -> ScriptStep {
    ScriptStep { tick, action: Action::SetBlock { pos, state: state.to_owned() } }
}

fn live(script: &Script) -> DifferentialOutcome {
    let endpoint = std::env::var("LODESTONE_DIFFERENTIAL_RCON").unwrap_or_else(|_| "127.0.0.1:25571".to_owned());
    generation::retry_oracle_timeouts(3, || scenario::evaluate_at(script, &region(), SETTLE_TICKS, false, &endpoint))
}

#[test]
#[ignore = "needs a live 26.2 RCON oracle; directed hydration hypothesis"]
fn directed_opposing_sources_preserve_and_waterlog_the_bottom_slab() {
    let outcome = live(&Script::new(vec![step(0, (0, 0, 0), WATER), step(0, (2, 0, 0), WATER)]));
    assert!(matches!(outcome, DifferentialOutcome::Agreed), "{outcome:?}");
}

#[test]
#[ignore = "needs a live 26.2 RCON oracle; isolated flowing-water decay hypothesis"]
/// Compares isolated level-three water beside the slab. There is no upstream
/// source, so this fixture does not measure a sustained source's flow front.
fn directed_level_three_flow_keeps_the_slab_dry() {
    let outcome = live(&Script::new(vec![step(0, (0, 0, 0), "minecraft:water[level=3]")]));
    assert!(matches!(outcome, DifferentialOutcome::Agreed), "{outcome:?}");
}

#[test]
#[ignore = "needs a live 26.2 RCON oracle; probes the wet-slab refill reduction"]
fn directed_wet_slab_refills_the_level_six_neighbor() {
    let outcome = live(&Script::new(vec![
        step(0, SLAB, SLAB_WET), step(0, (2, 0, 0), "minecraft:water[level=6]"),
    ]));
    assert!(matches!(outcome, DifferentialOutcome::Agreed), "{outcome:?}");
}

#[test]
#[ignore = "needs a live 26.2 RCON oracle"]
fn captured_source_removal_replay_keeps_the_waterlogged_slab_spreading() {
    let outcome = live(&Script::new(vec![
        step(0, (0, 0, 0), AIR),
        step(5, (0, 0, 0), AIR),
        step(7, (2, 0, 0), WATER),
        step(10, (0, 0, 0), WATER),
        step(14, (0, 0, 0), AIR),
    ]));
    assert!(matches!(outcome, DifferentialOutcome::Agreed), "{outcome:?}");
}

#[test]
#[ignore = "needs a live 26.2 RCON oracle"]
fn generated_waterlogging_candidates_are_shrunk_and_replayable() {
    let outcome = search_and_shrink_with(&domain(), SearchBudget { cases: 8, ..budget() }, &region(), SETTLE_TICKS,
        |script, probes, settle| {
            let endpoint = std::env::var("LODESTONE_DIFFERENTIAL_RCON").unwrap_or_else(|_| "127.0.0.1:25571".to_owned());
            generation::retry_oracle_timeouts(3, || scenario::evaluate_at(script, probes, settle, false, &endpoint))
        });
    match outcome {
        SearchOutcome::NoDivergence { cases_run } => assert_eq!(cases_run, 8),
        SearchOutcome::Found(found) => {
            let replay = ReplayCase::from_found(scenario::SCENARIO, budget().seed, SETTLE_TICKS, region(), &found);
            let json = replay.to_json_pretty().expect("encode live finding");
            let decoded = ReplayCase::from_json(&json).expect("decode live finding");
            let replayed = decoded.replay_generated_with(scenario::SCENARIO, &domain(), &region(), SETTLE_TICKS,
                |script, _, _| live(script)).expect("validate live finding");
            assert!(matches!(replayed, DifferentialOutcome::Diverged(value) if value == found.minimal_divergence));
            panic!("waterlogging divergence; minimized replay JSON follows:\n{json}");
        }
        other => panic!("waterlogging oracle did not complete: {other:?}"),
    }
}
