//! Shared finite fluid lane reset, comparison and cleanup.

use std::io;

use super::generation::GenerationDomain;
use lodestone_fuzz::differential::fluid::FluidModelOracle;
use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, OracleFailure, OracleFailureKind, Script, Side,
    TICK_MILLIS, WorldOracle, run_differential,
};

pub(crate) const PASSWORD: &str = "lodestone";
pub(crate) const SCENARIO: &str = "generated-live-fluid";
pub(crate) const ORIGIN: (i32, i32, i32) = (120_000, 200, 48_000);
pub(crate) const CELLS: i32 = 3;
pub(crate) const RESET_TICKS: u64 = 6;
pub(crate) const SETTLE_TICKS: u64 = 6;
pub(crate) const AIR: &str = "minecraft:air";
pub(crate) const STONE: &str = "minecraft:stone";
pub(crate) const WATER: &str = "minecraft:water[level=0]";


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

pub(crate) fn verify_block<O: WorldOracle>(
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

pub(crate) fn verify_live_baseline<O: WorldOracle>(oracle: &mut O) -> Result<(), io::Error> {
    for x in 0..=CELLS {
        verify_block(oracle, (x, 0, 0), AIR, "minecraft:water")?;
        for pos in [(x, -1, 0), (x, 1, 0), (x, 0, -1), (x, 0, 1)] {
            verify_block(oracle, pos, STONE, AIR)?;
        }
    }
    Ok(())
}

pub(crate) fn failure(tick: u64, error: io::Error, context: &str) -> DifferentialOutcome {
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

pub(crate) fn command(oracle: &mut RconOracle, command: String, context: &str) -> Result<(), io::Error> {
    oracle
        .apply(&Action::RunCommand(command))
        .map_err(|error| io::Error::new(error.kind(), format!("{context}: {error}")))
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

    // Move away from the tick boundary before anchoring the candidate. The
    // fixed live comparisons use the same half-tick margin.
    oracle.advance_tick()?;
    std::thread::sleep(TICK_MILLIS / 2);
    oracle.reset_baseline()?;
    Ok(())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanupStep {
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

pub(crate) fn run_cleanup_steps<F>(reset_ticks: u64, mut run: F) -> Result<(), io::Error>
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

pub(crate) struct FaultyRead {
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


pub(crate) fn evaluate_at(
    script: &Script,
    region: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    faulty: bool,
    endpoint: &str,
) -> DifferentialOutcome {
    let mut vanilla = match RconOracle::connect(endpoint, PASSWORD, ORIGIN) {
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
            if vanilla.missed_deadlines() == 0
                || matches!(&comparison, DifferentialOutcome::OracleFailed(_)) {
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
