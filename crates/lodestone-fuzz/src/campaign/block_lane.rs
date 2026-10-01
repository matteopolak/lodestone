//! Shared live block-state baseline probes, commands and bounded cleanup.

use std::io;

use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::{Action, WorldOracle};

pub(crate) const AIR: &str = "minecraft:air";
pub(crate) const STONE: &str = "minecraft:stone";

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

pub(crate) fn command(oracle: &mut RconOracle, command: String, context: &str) -> Result<(), io::Error> {
    oracle
        .apply(&Action::RunCommand(command))
        .map_err(|error| io::Error::new(error.kind(), format!("{context}: {error}")))
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
