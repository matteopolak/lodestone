//! Versioned, externally captured, short tick traces for differential tests.
//!
//! The corpus deliberately carries observations rather than deriving an
//! expectation from the Rust-side oracle.  The live capture script is the
//! producer; this module only validates and consumes its JSON.

use lodestone_fuzz::differential::{Action, OracleFailure, Side, WorldOracle};
use serde::Deserialize;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_TICKS: u64 = 16;
pub const MAX_STEPS: usize = 8;
pub const MAX_PROBES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TickCorpus {
    format_version: u32,
    scenario: String,
    provenance: Provenance,
    settle_ticks: u64,
    region: Vec<Probe>,
    steps: Vec<Step>,
    observations: Vec<Observation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Provenance {
    source: String,
    minecraft_version: String,
    capture_command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Probe {
    pos: (i32, i32, i32),
    candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Step {
    tick: u64,
    action: SetBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SetBlock {
    SetBlock { pos: (i32, i32, i32), state: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Observation {
    tick: u64,
    game_time: i64,
    states: Vec<Option<String>>,
}

#[derive(Debug, Clone)]
pub enum CorpusOutcome {
    Agreed,
    Diverged {
        tick: u64,
        pos: (i32, i32, i32),
        expected: Option<String>,
        actual: Option<String>,
    },
    OracleFailed(OracleFailure),
}

impl TickCorpus {
    pub fn from_json(json: &str) -> Result<Self, String> {
        let corpus: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        corpus.validate()?;
        Ok(corpus)
    }

    pub fn scenario(&self) -> &str {
        &self.scenario
    }

    fn validate(&self) -> Result<(), String> {
        if self.format_version != FORMAT_VERSION {
            return Err(format!(
                "unsupported differential tick corpus format {}, expected {FORMAT_VERSION}",
                self.format_version
            ));
        }
        if self.scenario.trim().is_empty() {
            return Err("differential tick corpus scenario must not be empty".to_owned());
        }
        if self.provenance.source != "real-java-rcon" {
            return Err("differential tick corpus must name real-java-rcon as its expectation source".to_owned());
        }
        if self.provenance.minecraft_version.trim().is_empty()
            || self.provenance.capture_command.trim().is_empty()
        {
            return Err("differential tick corpus provenance needs a version and capture command".to_owned());
        }
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS {
            return Err(format!(
                "differential tick corpus needs 1..={MAX_STEPS} SetBlock steps"
            ));
        }
        if self.region.is_empty() || self.region.len() > MAX_PROBES {
            return Err(format!(
                "differential tick corpus needs 1..={MAX_PROBES} probes"
            ));
        }
        for (index, probe) in self.region.iter().enumerate() {
            if probe.candidates.is_empty() {
                return Err(format!("differential tick corpus probe {index} has no candidates"));
            }
        }
        if self.steps[0].tick != 0 {
            return Err("differential tick corpus must begin with an action at tick 0".to_owned());
        }
        for (index, step) in self.steps.iter().enumerate().skip(1) {
            if step.tick < self.steps[index - 1].tick {
                return Err(format!("differential tick corpus step {index} moves backward in time"));
            }
        }
        let last_tick = self.steps.last().expect("nonempty steps checked above").tick;
        let total_ticks = last_tick
            .checked_add(self.settle_ticks)
            .and_then(|ticks| ticks.checked_add(1))
            .ok_or_else(|| "differential tick corpus horizon overflows u64".to_owned())?;
        if total_ticks > MAX_TICKS {
            return Err(format!(
                "differential tick corpus horizon of {total_ticks} exceeds the {MAX_TICKS}-tick cap"
            ));
        }
        if self.observations.len() != total_ticks as usize {
            return Err(format!(
                "differential tick corpus has {} observations, expected {total_ticks}",
                self.observations.len()
            ));
        }
        for (tick, observation) in self.observations.iter().enumerate() {
            let tick = tick as u64;
            if observation.tick != tick {
                return Err(format!(
                    "differential tick corpus observation {tick} is labelled tick {}",
                    observation.tick
                ));
            }
            if observation.states.len() != self.region.len() {
                return Err(format!(
                    "differential tick corpus observation {tick} has {} states for {} probes",
                    observation.states.len(),
                    self.region.len()
                ));
            }
            if tick > 0
                && observation.game_time != self.observations[tick as usize - 1].game_time + 1
            {
                return Err(format!(
                    "differential tick corpus observation {tick} does not advance game time by exactly one"
                ));
            }
            for (probe, state) in self.region.iter().zip(&observation.states) {
                if let Some(state) = state
                    && !probe.candidates.contains(state)
                {
                    return Err(format!(
                        "differential tick corpus observation {tick} contains state {state:?} outside its probe alphabet"
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn compare<O: WorldOracle>(&self, oracle: &mut O) -> CorpusOutcome {
        for observation in &self.observations {
            for step in self.steps.iter().filter(|step| step.tick == observation.tick) {
                let action = match &step.action {
                    SetBlock::SetBlock { pos, state } => Action::SetBlock {
                        pos: *pos,
                        state: state.clone(),
                    },
                };
                if let Err(error) = oracle.apply(&action) {
                    return failure::<O>(observation.tick, error);
                }
            }
            if let Err(error) = oracle.advance_tick() {
                return failure::<O>(observation.tick, error);
            }
            for (index, probe) in self.region.iter().enumerate() {
                let actual = match oracle.block_state(probe.pos, &probe.candidates) {
                    Ok(state) => state,
                    Err(error) => return failure::<O>(observation.tick, error),
                };
                let expected = observation.states[index].clone();
                if actual != expected {
                    return CorpusOutcome::Diverged {
                        tick: observation.tick,
                        pos: probe.pos,
                        expected,
                        actual,
                    };
                }
            }
        }
        CorpusOutcome::Agreed
    }
}

fn failure<O: WorldOracle>(tick: u64, error: O::Error) -> CorpusOutcome {
    CorpusOutcome::OracleFailed(OracleFailure {
        tick,
        side: Side::Left,
        kind: O::classify_error(&error),
        message: error.to_string(),
    })
}
