//! Shared finite redstone lane reset, comparison and cleanup.

use std::convert::Infallible;
use std::io;


use super::contraption;

use super::generation::GenerationDomain;
use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::redstone::RedstoneModelOracle;
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, OracleFailure, OracleFailureKind, Script, Side, WorldOracle,
    run_differential,
};

pub(crate) const PASSWORD: &str = "lodestone";
pub(crate) const SCENARIO: &str = "generated-live-redstone";
pub(crate) const GENERATED_LANE: i32 = 12;
pub(crate) const SETTLE_TICKS: u64 = 20;
pub(crate) const QUIET_TICKS: u32 = 12;
pub(crate) const AIR: &str = "minecraft:air";
pub(crate) const SOURCE: &str = contraption::SOURCE_STATE;


pub(crate) fn origin(lane: i32) -> (i32, i32, i32) {
    contraption::origin_on_lane(lane)
}

pub(crate) fn domain() -> GenerationDomain {
    GenerationDomain::new(
        vec![contraption::SOURCE],
        vec![AIR.to_owned(), AIR.to_owned(), SOURCE.to_owned()],
        3,
        3,
    )
    .expect("the generated redstone domain is valid")
}


pub(crate) fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    contraption::region()
}

pub(crate) fn command(oracle: &mut RconOracle, command: String, context: &str) -> Result<(), io::Error> {
    oracle
        .apply(&Action::RunCommand(command))
        .map_err(|error| io::Error::new(error.kind(), format!("{context}: {error}")))
}

pub(crate) fn setup_command(oracle: &mut RconOracle, text: String, context: &str) -> Result<(), io::Error> {
    command(oracle, text, context)
}

pub(crate) fn clear_lane(oracle: &mut RconOracle, origin: (i32, i32, i32)) -> Result<(), io::Error> {
    let (ox, oy, oz) = origin;
    setup_command(
        oracle,
        format!(
            "fill {} {} {} {} {} {} {AIR}",
            ox - 1,
            oy + contraption::FLOOR_Y,
            oz - 1,
            ox + contraption::LAST_CELL + 1,
            oy + contraption::ROW_Y + 1,
            oz + 1
        ),
        "clear the generated-redstone lane",
    )
}

pub(crate) fn build_live_rig(oracle: &mut RconOracle, origin: (i32, i32, i32)) -> Result<(), io::Error> {
    let (ox, oy, oz) = origin;
    setup_command(
        oracle,
        format!("forceload add {ox} {oz} {} {oz}", ox + contraption::LAST_CELL + 2),
        "force-load the generated-redstone lane",
    )?;
    clear_lane(oracle, origin)?;
    setup_command(
        oracle,
        format!(
            "fill {} {} {} {} {} {} {}",
            ox - 1,
            oy + contraption::FLOOR_Y,
            oz - 1,
            ox + contraption::LAST_CELL + 1,
            oy + contraption::FLOOR_Y,
            oz + 1,
            contraption::FLOOR_STATE,
        ),
        "lay the generated-redstone floor",
    )?;
    for ((dx, dy, dz), state) in contraption::components() {
        setup_command(
            oracle,
            format!("setblock {} {} {} {state}", ox + dx, oy + dy, oz + dz),
            "lay a generated-redstone component",
        )?;
    }
    oracle.reset_baseline()?;
    settle_live_rig(oracle)?;
    oracle.reset_baseline()?;
    Ok(())
}

pub(crate) fn settle_live_rig(oracle: &mut RconOracle) -> Result<(), io::Error> {
    let mut quiet = 0;
    for _ in 0..96 {
        oracle.advance_tick()?;
        let mut all_quiet = true;
        for &x in &contraption::REPEATER_CELLS {
            let observed = oracle.block_state(
                (x, contraption::ROW_Y, 0),
                &[
                    "minecraft:repeater[facing=west,delay=1,locked=false,powered=false]"
                        .to_owned(),
                    "minecraft:repeater[facing=west,delay=4,locked=false,powered=false]"
                        .to_owned(),
                    "minecraft:repeater[facing=west,delay=2,locked=false,powered=false]"
                        .to_owned(),
                ],
            )?;
            if observed.is_none() {
                all_quiet = false;
            }
        }
        quiet = if all_quiet { quiet + 1 } else { 0 };
        if quiet == QUIET_TICKS {
            return Ok(());
        }
    }
    Err(io::Error::other("redstone lane did not become quiet within 96 observed ticks"))
}

pub(crate) fn tear_down(oracle: &mut RconOracle, origin: (i32, i32, i32)) -> Result<(), io::Error> {
    let (ox, _, oz) = origin;
    let clear = clear_lane(oracle, origin);
    let release = setup_command(
        oracle,
        format!("forceload remove {ox} {oz} {} {oz}", ox + contraption::LAST_CELL + 2),
        "release the generated-redstone lane",
    );
    clear.and(release)
}

pub(crate) fn build_model(origin: (i32, i32, i32)) -> RedstoneModelOracle {
    let mut model = RedstoneModelOracle::new(
        origin,
        contraption::FLOOR_Y,
        contraption::FLOOR_STATE,
    );
    for (pos, state) in contraption::components() {
        model.place_static(pos, &state);
    }
    model
}

pub(crate) fn failure(tick: u64, error: io::Error, context: &str) -> DifferentialOutcome {
    DifferentialOutcome::OracleFailed(OracleFailure {
        tick,
        side: Side::Right,
        kind: if error.kind() == io::ErrorKind::TimedOut {
            OracleFailureKind::Timeout
        } else {
            OracleFailureKind::Failure
        },
        message: format!("{context}: {error}"),
    })
}

pub(crate) struct FaultyRead {
    pub(crate) inner: RedstoneModelOracle,
}

impl WorldOracle for FaultyRead {
    type Error = Infallible;

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
        if pos == contraption::PREDICTED[0].0 && observed.as_deref() == Some("minecraft:redstone_wire[power=15]") {
            return Ok(Some("minecraft:redstone_wire[power=0]".to_owned()));
        }
        Ok(observed)
    }
}


pub(crate) fn evaluate_at(
    script: &Script,
    probes: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    faulty: bool,
    endpoint: &str,
) -> DifferentialOutcome {
    let candidate_origin = origin(GENERATED_LANE);
    let mut live = match RconOracle::connect(endpoint, PASSWORD, candidate_origin) {
        Ok(oracle) => oracle,
        Err(error) => return failure(0, error, "connect to the live reference oracle"),
    };
    let final_tick = script.last_tick() + settle_ticks;
    let mut outcome = match build_live_rig(&mut live, candidate_origin) {
        Err(error) => failure(0, error, "reset the live redstone lane"),
        Ok(()) => {
            let comparison = if faulty {
                let mut model = FaultyRead { inner: build_model(candidate_origin) };
                run_differential(script, probes, &mut model, &mut live, settle_ticks)
            } else {
                let mut model = build_model(candidate_origin);
                run_differential(script, probes, &mut model, &mut live, settle_ticks)
            };
            if live.missed_deadlines() == 0
                || matches!(&comparison, DifferentialOutcome::OracleFailed(_)) {
                comparison
            } else {
                failure(
                    final_tick,
                    io::Error::new(io::ErrorKind::TimedOut, format!(
                        "live reference missed {} tick deadlines", live.missed_deadlines()
                    )),
                    "reject a timing-contended candidate",
                )
            }
        }
    };
    if let Err(error) = tear_down(&mut live, candidate_origin) {
        if matches!(outcome, DifferentialOutcome::Agreed | DifferentialOutcome::Diverged(_)) {
            outcome = failure(final_tick, error, "tear down the live redstone lane");
        }
    }
    outcome
}
