//! Common fluid comparison and clock admission over shared block-lane helpers.

use std::io;

use lodestone_fuzz::differential::fluid::FluidModelOracle;
use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, OracleFailure, OracleFailureKind, Script, Side,
    TICK_MILLIS, WorldOracle, run_differential,
};

const PASSWORD: &str = "lodestone";
pub(crate) use super::block_lane::{AIR, STONE, CleanupStep, command, run_cleanup_steps, verify_block};
pub(crate) const WATER: &str = "minecraft:water[level=0]";

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

pub(crate) fn prepare_comparison(oracle: &mut RconOracle) -> Result<(), io::Error> {
    oracle.advance_tick()?;
    std::thread::sleep(TICK_MILLIS / 2);
    oracle.reset_baseline()?;
    Ok(())
}

struct FaultyRead {
    inner: FluidModelOracle,
    fault: ReadFault,
}

#[derive(Clone, Copy)]
pub(crate) struct ReadFault {
    pub(crate) pos: (i32, i32, i32),
    pub(crate) observed: &'static str,
    pub(crate) replacement: &'static str,
}

pub(crate) struct FluidLane {
    pub(crate) origin: (i32, i32, i32),
    pub(crate) reset: fn(&mut RconOracle) -> Result<(), io::Error>,
    pub(crate) model: fn() -> FluidModelOracle,
    pub(crate) cleanup: fn(&mut RconOracle) -> Result<(), io::Error>,
    pub(crate) fault: Option<ReadFault>,
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
        if pos == self.fault.pos && observed.as_deref() == Some(self.fault.observed) {
            return Ok(Some(self.fault.replacement.to_owned()));
        }
        Ok(observed)
    }
}

pub(crate) fn evaluate_lane_at(
    script: &Script,
    region: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    endpoint: &str,
    lane: FluidLane,
) -> DifferentialOutcome {
    let mut vanilla = match RconOracle::connect(endpoint, PASSWORD, lane.origin) {
        Ok(oracle) => oracle,
        Err(error) => return failure(0, error, "connect to the live reference oracle"),
    };
    let final_tick = script.last_tick() + settle_ticks;
    let mut outcome = match (lane.reset)(&mut vanilla) {
        Err(error) => failure(0, error, "reset the live candidate"),
        Ok(()) => {
            let model = (lane.model)();
            let comparison = if let Some(fault) = lane.fault {
                let mut model = FaultyRead { inner: model, fault };
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
    if let Err(error) = (lane.cleanup)(&mut vanilla) {
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
