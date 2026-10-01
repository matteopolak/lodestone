//! Finite fluid scenario layout, reset and model construction.

use std::io;

use super::generation::GenerationDomain;
pub(crate) use super::fluid_lane::{AIR, STONE, WATER, CleanupStep, run_cleanup_steps};
use super::fluid_lane::{FluidLane, ReadFault, command, evaluate_lane_at, prepare_comparison, verify_block};
use lodestone_fuzz::differential::fluid::FluidModelOracle;
use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::{DifferentialOutcome, Script, WorldOracle};

pub(crate) const SCENARIO: &str = "generated-live-fluid";
pub(crate) const ORIGIN: (i32, i32, i32) = (120_000, 200, 48_000);
pub(crate) const CELLS: i32 = 3;
pub(crate) const RESET_TICKS: u64 = 6;
pub(crate) const SETTLE_TICKS: u64 = 6;


pub(crate) fn domain() -> GenerationDomain {
    GenerationDomain::new(
        vec![(0, 0, 0)],
        vec![AIR.to_owned(), WATER.to_owned(), WATER.to_owned()],
        3,
        3,
    )
    .expect("the generated live-fluid domain is valid")
}


pub(crate) fn candidates() -> Vec<String> {
    vec![AIR.to_owned(), "minecraft:water".to_owned()]
}

pub(crate) fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    (0..=CELLS).map(|x| ((x, 0, 0), candidates())).collect()
}

pub(crate) fn verify_live_baseline<O: WorldOracle>(oracle: &mut O) -> Result<(), io::Error> {
    for x in 0..=CELLS {
        verify_block(oracle, (x, 0, 0), AIR, "minecraft:water")?;
        for pos in [(x, -1, 0), (x, 1, 0), (x, 0, -1), (x, 0, 1)] {
            verify_block(oracle, pos, STONE, AIR)?;
        }
    }
    Ok(())
}

pub(crate) fn fill_box(oracle: &mut RconOracle, state: &str) -> Result<(), io::Error> {
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

pub(crate) fn reset_and_build_vanilla(oracle: &mut RconOracle) -> Result<(), io::Error> {
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

    prepare_comparison(oracle)
}

pub(crate) fn build_model() -> FluidModelOracle {
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

pub(crate) fn tear_down(oracle: &mut RconOracle) -> Result<(), io::Error> {
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

pub(crate) fn evaluate_at(
    script: &Script,
    region: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    faulty: bool,
    endpoint: &str,
) -> DifferentialOutcome {
    evaluate_lane_at(script, region, settle_ticks, endpoint, FluidLane {
        origin: ORIGIN,
        reset: reset_and_build_vanilla,
        model: build_model,
        cleanup: tear_down,
        fault: faulty.then_some(ReadFault {
            pos: (1, 0, 0), observed: "minecraft:water", replacement: AIR,
        }),
    })
}
