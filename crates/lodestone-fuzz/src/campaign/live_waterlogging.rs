//! Waterlogging scenarios over the shared fluid comparison pipeline.

use std::io;

use super::generation::GenerationDomain;
use super::fluid_lane::{
    AIR, STONE, WATER, CleanupStep, FluidLane, ReadFault, command, evaluate_lane_at,
    prepare_comparison, run_cleanup_steps, verify_block,
};
use lodestone_fuzz::differential::fluid::FluidModelOracle;
use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::{DifferentialOutcome, Script, WorldOracle};

pub(crate) const SCENARIO: &str = "generated-live-waterlogging";
pub(crate) const ORIGIN: (i32, i32, i32) = (120_000, 200, 48_032);
pub(crate) const SLAB: (i32, i32, i32) = (1, 0, 0);
pub(crate) const SLAB_DRY: &str = "minecraft:oak_slab[type=bottom,waterlogged=false]";
pub(crate) const SLAB_WET: &str = "minecraft:oak_slab[type=bottom,waterlogged=true]";
pub(crate) const SETTLE_TICKS: u64 = 12;
const RESET_TICKS: u64 = 12;

pub(crate) fn domain() -> GenerationDomain {
    GenerationDomain::new(
        vec![(0, 0, 0), (2, 0, 0)],
        vec![AIR.to_owned(), WATER.to_owned()],
        6,
        6,
    ).expect("the generated waterlogging domain is valid")
}

pub(crate) fn water_candidates() -> Vec<String> {
    std::iter::once(AIR.to_owned())
        .chain((0..=15).map(|level| format!("minecraft:water[level={level}]")))
        .collect()
}

pub(crate) fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    vec![
        ((0, 0, 0), water_candidates()),
        (SLAB, vec![SLAB_DRY.to_owned(), SLAB_WET.to_owned()]),
        ((2, 0, 0), water_candidates()),
    ]
}

pub(crate) fn verify_live_baseline<O: WorldOracle>(oracle: &mut O) -> Result<(), io::Error> {
    for x in 0..=2 {
        if x == SLAB.0 {
            verify_block(oracle, SLAB, SLAB_DRY, SLAB_WET)?;
        } else {
            verify_block(oracle, (x, 0, 0), AIR, "minecraft:water")?;
        }
        verify_block(oracle, (x, 1, 0), AIR, STONE)?;
        for pos in [(x, -1, 0), (x, 0, -1), (x, 0, 1)] {
            verify_block(oracle, pos, STONE, AIR)?;
        }
    }
    for x in [-1, 3] {
        verify_block(oracle, (x, 0, 0), STONE, AIR)?;
    }
    Ok(())
}

fn fill_box(oracle: &mut RconOracle, state: &str, top: i32) -> Result<(), io::Error> {
    let (ox, oy, oz) = ORIGIN;
    command(oracle, format!(
        "fill {} {} {} {} {} {} {state}",
        ox - 1, oy - 1, oz - 1, ox + 3, oy + top, oz + 1,
    ), "fill the generated-waterlogging trench")
}

pub(crate) fn reset_and_build_live(oracle: &mut RconOracle) -> Result<(), io::Error> {
    let (ox, oy, oz) = ORIGIN;
    command(oracle, format!("forceload add {} {} {} {}", ox - 1, oz - 1, ox + 3, oz + 1),
        "force-load the generated-waterlogging trench")?;
    fill_box(oracle, AIR, 1)?;
    oracle.reset_baseline()?;
    for _ in 0..RESET_TICKS {
        oracle.advance_tick()?;
    }
    fill_box(oracle, STONE, 0)?;
    command(oracle, format!("fill {ox} {oy} {oz} {} {oy} {oz} {AIR}", ox + 2),
        "carve the open-top waterlogging trench")?;
    command(oracle, format!("setblock {} {oy} {oz} {SLAB_DRY}", ox + SLAB.0),
        "place the fixed bottom slab")?;
    verify_live_baseline(oracle)?;
    prepare_comparison(oracle)
}

pub(crate) fn build_model() -> FluidModelOracle {
    let mut model = FluidModelOracle::new((0, 0, 0), -1, STONE);
    for x in -1..=3 {
        for z in [-1, 1] {
            model.place_static((x, 0, z), STONE);
        }
    }
    for x in [-1, 3] {
        model.place_static((x, 0, 0), STONE);
    }
    model.place_static(SLAB, SLAB_DRY);
    model
}

pub(crate) fn tear_down(oracle: &mut RconOracle) -> Result<(), io::Error> {
    let (ox, _, oz) = ORIGIN;
    run_cleanup_steps(RESET_TICKS, |step| match step {
        CleanupStep::Clear => fill_box(oracle, AIR, 1),
        CleanupStep::ResetClock => oracle.reset_baseline(),
        CleanupStep::Advance => oracle.advance_tick(),
        CleanupStep::Release => command(oracle,
            format!("forceload remove {} {} {} {}", ox - 1, oz - 1, ox + 3, oz + 1),
            "release the generated-waterlogging trench"),
    })
}

pub(crate) fn evaluate_at(
    script: &Script,
    probes: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    faulty: bool,
    endpoint: &str,
) -> DifferentialOutcome {
    evaluate_lane_at(script, probes, settle_ticks, endpoint, FluidLane {
        origin: ORIGIN,
        reset: reset_and_build_live,
        model: build_model,
        cleanup: tear_down,
        fault: faulty.then_some(ReadFault {
            pos: SLAB, observed: SLAB_WET, replacement: SLAB_DRY,
        }),
    })
}
