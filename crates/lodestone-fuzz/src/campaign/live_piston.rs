//! Finite direct-power piston interruption and commit comparisons.

use std::io;

use super::block_lane::{AIR, STONE, CleanupStep, command, run_cleanup_steps, verify_block};
use super::generation::GenerationDomain;
use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::redstone::RedstoneModelOracle;
use lodestone_fuzz::differential::{
    DifferentialOutcome, OracleFailure, OracleFailureKind, Script, Side, WorldOracle,
    run_differential,
};

pub(crate) const SCENARIO: &str = "generated-live-piston";
pub(crate) const ORIGIN: (i32, i32, i32) = (120_000, 200, 48_064);
pub(crate) const BASE: (i32, i32, i32) = (0, 1, 0);
pub(crate) const TRIGGER: (i32, i32, i32) = (-1, 1, 0);
pub(crate) const ARM: (i32, i32, i32) = (0, 1, 1);
pub(crate) const POWER: &str = "minecraft:redstone_block";
pub(crate) const PISTON: &str = "minecraft:piston[facing=south,extended=false]";
pub(crate) const EXTENDED: &str = "minecraft:piston[facing=south,extended=true]";
pub(crate) const MOVING: &str = "minecraft:moving_piston[facing=south,type=normal]";
pub(crate) const HEAD: &str = "minecraft:piston_head[facing=south,type=normal,short=false]";
pub(crate) const DIRT: &str = "minecraft:dirt";
pub(crate) const SETTLE_TICKS: u64 = 6;
const RESET_TICKS: u64 = 6;
const PASSWORD: &str = "lodestone";

pub(crate) fn domain() -> GenerationDomain {
    GenerationDomain::new(vec![TRIGGER], vec![AIR.to_owned(), POWER.to_owned()], 3, 3)
        .expect("the generated piston domain is valid")
}

pub(crate) fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    let front = vec![
        AIR.to_owned(), DIRT.to_owned(), MOVING.to_owned(), HEAD.to_owned(),
        "minecraft:piston_head[facing=south,type=normal,short=true]".to_owned(),
    ];
    vec![
        (BASE, vec![PISTON.to_owned(), EXTENDED.to_owned(), MOVING.to_owned(), AIR.to_owned()]),
        (TRIGGER, vec![AIR.to_owned(), POWER.to_owned()]),
        (ARM, front.clone()),
        ((0, 1, 2), front.clone()),
        ((0, 1, 3), front),
    ]
}

pub(crate) fn build_model() -> RedstoneModelOracle {
    let mut model = RedstoneModelOracle::new(ORIGIN, 0, STONE);
    model.place_static(BASE, PISTON);
    model.place_static(ARM, DIRT);
    model
}

fn clear_lane(oracle: &mut RconOracle) -> Result<(), io::Error> {
    let (x, y, z) = ORIGIN;
    command(oracle, format!("fill {} {y} {} {} {} {} {AIR}",
        x - 1, z - 1, x + 1, y + 2, z + 4), "clear the generated-piston lane")
}

pub(crate) fn verify_live_baseline<O: WorldOracle>(oracle: &mut O) -> Result<(), io::Error> {
    for x in -1..=1 {
        for z in -1..=4 {
            verify_block(oracle, (x, 0, z), STONE, AIR)?;
            for y in 1..=2 {
                let pos = (x, y, z);
                let expected = if pos == BASE { PISTON } else if pos == ARM { DIRT } else { AIR };
                let alternative = if pos == BASE { EXTENDED } else { MOVING };
                verify_block(oracle, pos, expected, alternative)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn reset_and_build_live(oracle: &mut RconOracle) -> Result<(), io::Error> {
    let (x, y, z) = ORIGIN;
    command(oracle, format!("forceload add {} {} {} {}", x - 1, z - 1, x + 1, z + 4),
        "force-load the generated-piston lane")?;
    clear_lane(oracle)?;
    oracle.reset_baseline()?;
    for _ in 0..RESET_TICKS { oracle.advance_tick()?; }
    command(oracle, format!("fill {} {y} {} {} {y} {} {STONE}", x - 1, z - 1, x + 1, z + 4),
        "lay the generated-piston floor")?;
    command(oracle, format!("setblock {x} {} {z} {PISTON}", y + 1), "place the fixed piston")?;
    command(oracle, format!("setblock {x} {} {} {DIRT}", y + 1, z + 1), "place the pushed dirt")?;
    verify_live_baseline(oracle)?;
    oracle.reset_baseline()
}

pub(crate) fn tear_down(oracle: &mut RconOracle) -> Result<(), io::Error> {
    let (x, _, z) = ORIGIN;
    run_cleanup_steps(RESET_TICKS, |step| match step {
        CleanupStep::Clear => clear_lane(oracle),
        CleanupStep::ResetClock => oracle.reset_baseline(),
        CleanupStep::Advance => oracle.advance_tick(),
        CleanupStep::Release => command(oracle,
            format!("forceload remove {} {} {} {}", x - 1, z - 1, x + 1, z + 4),
            "release the generated-piston lane"),
    })
}

fn failure(tick: u64, error: io::Error, context: &str) -> DifferentialOutcome {
    DifferentialOutcome::OracleFailed(OracleFailure {
        tick, side: Side::Right,
        kind: if error.kind() == io::ErrorKind::TimedOut {
            OracleFailureKind::Timeout
        } else { OracleFailureKind::Failure },
        message: format!("{context}: {error}"),
    })
}

pub(crate) fn evaluate_at(
    script: &Script,
    probes: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    endpoint: &str,
) -> DifferentialOutcome {
    let mut live = match RconOracle::connect(endpoint, PASSWORD, ORIGIN) {
        Ok(oracle) => oracle,
        Err(error) => return failure(0, error, "connect to the live piston oracle"),
    };
    let final_tick = script.last_tick() + settle_ticks;
    let mut outcome = match reset_and_build_live(&mut live) {
        Err(error) => failure(0, error, "reset the live piston candidate"),
        Ok(()) => run_differential(script, probes, &mut build_model(), &mut live, settle_ticks),
    };
    if live.missed_deadlines() != 0 && !matches!(&outcome, DifferentialOutcome::OracleFailed(_)) {
        outcome = failure(final_tick, io::Error::new(io::ErrorKind::TimedOut,
            format!("live piston reference missed {} tick boundaries", live.missed_deadlines())),
            "reject a timing-contended candidate");
    }
    if let Err(error) = tear_down(&mut live) {
        match &mut outcome {
            DifferentialOutcome::OracleFailed(failure) => {
                failure.message.push_str(&format!("; cleanup also failed: {error}"));
            }
            DifferentialOutcome::Agreed | DifferentialOutcome::Diverged(_) => {
                outcome = failure(final_tick, error, "tear down the live piston candidate");
            }
        }
    }
    outcome
}
