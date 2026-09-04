//! Deterministic generated-script search support for differential tests.
//!
//! This stays under `tests/support`: `proptest` supplies a semantic value
//! tree for generation and shrinking, but the production differential loop
//! has no reason to depend on a property-test runtime. Every candidate is run
//! against a newly-created pair of oracles, and transport/instrument failures
//! abort rather than masquerading as gameplay counterexamples.

use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, Divergence, OracleFailure, Script, ScriptStep, WorldOracle, run_differential,
};
use proptest::collection;
use proptest::prelude::{BoxedStrategy, Strategy};
use proptest::sample;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestRunner};
use serde::{Deserialize, Serialize};

const REPLAY_FORMAT_VERSION: u32 = 1;
pub const MAX_ORACLE_TICKS: u64 = 4_096;

/// A finite alphabet supplied by the scenario outside the model under test.
///
/// Repeating an entry gives it more generation weight without changing the
/// domain. Entries should be ordered from simplest to most specific because
/// the underlying index strategy shrinks toward the front.
#[derive(Debug, Clone)]
pub struct GenerationDomain {
    positions: Vec<(i32, i32, i32)>,
    states: Vec<String>,
    max_steps: usize,
    max_tick_gap: u64,
}

impl GenerationDomain {
    pub fn new(
        positions: Vec<(i32, i32, i32)>,
        states: Vec<String>,
        max_steps: usize,
        max_tick_gap: u64,
    ) -> Result<Self, &'static str> {
        if positions.is_empty() {
            return Err("a generation domain needs at least one position");
        }
        if states.is_empty() {
            return Err("a generation domain needs at least one block state");
        }
        if max_steps == 0 {
            return Err("a generation domain needs at least one script step");
        }
        Ok(Self {
            positions,
            states,
            max_steps,
            max_tick_gap,
        })
    }

    pub fn positions(&self) -> &[(i32, i32, i32)] {
        &self.positions
    }

    pub fn states(&self) -> &[String] {
        &self.states
    }

    pub fn max_steps(&self) -> usize {
        self.max_steps
    }

    pub fn max_tick_gap(&self) -> u64 {
        self.max_tick_gap
    }
}

/// Deterministic work limits. Neither generation nor shrinking has an
/// elapsed-time cutoff.
#[derive(Debug, Clone, Copy)]
pub struct SearchBudget {
    pub seed: u64,
    pub cases: u32,
    pub shrink_attempts: u32,
}

#[derive(Debug, Clone)]
pub struct FoundDivergence {
    pub case_index: u32,
    pub original_script: Script,
    pub minimal_script: Script,
    pub original_divergence: Divergence,
    pub minimal_divergence: Divergence,
    pub shrink_attempts: u32,
}

#[derive(Debug, Clone)]
pub enum SearchOutcome {
    InvalidConfiguration {
        message: String,
    },
    NoDivergence {
        cases_run: u32,
    },
    Found(FoundDivergence),
    OracleFailed {
        case_index: u32,
        during_shrink: bool,
        failure: OracleFailure,
    },
}

fn bounded_oracle_ticks(last_tick: u64, settle_ticks: u64, context: &str) -> Result<u64, String> {
    let oracle_ticks = last_tick
        .checked_add(settle_ticks)
        .and_then(|last_tick| last_tick.checked_add(1))
        .ok_or_else(|| format!("{context} differential tick horizon overflows u64"))?;
    if oracle_ticks > MAX_ORACLE_TICKS {
        return Err(format!(
            "{context} differential horizon of {oracle_ticks} ticks exceeds the {MAX_ORACLE_TICKS}-tick cap"
        ));
    }
    Ok(oracle_ticks)
}

fn generated_oracle_ticks(domain: &GenerationDomain, settle_ticks: u64) -> Result<u64, String> {
    let gap_count = u64::try_from(domain.max_steps - 1)
        .map_err(|_| "generated differential tick horizon overflows u64".to_owned())?;
    let last_tick = domain
        .max_tick_gap
        .checked_mul(gap_count)
        .ok_or_else(|| "generated differential tick horizon overflows u64".to_owned())?;
    bounded_oracle_ticks(last_tick, settle_ticks, "generated")
}

fn runner(budget: SearchBudget) -> TestRunner {
    TestRunner::new(Config {
        cases: budget.cases,
        failure_persistence: None,
        max_shrink_iters: budget.shrink_attempts,
        max_shrink_time: 0,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(budget.seed),
        ..Config::default()
    })
}

fn script_strategy(domain: &GenerationDomain) -> BoxedStrategy<Script> {
    let gap = 0..=domain.max_tick_gap;
    let pos = sample::select(domain.positions.clone());
    let state = sample::select(domain.states.clone());
    collection::vec((gap, pos, state), 1..=domain.max_steps)
        .prop_map(|rows| {
            let mut tick = 0_u64;
            let steps = rows
                .into_iter()
                .enumerate()
                .map(|(index, (gap, pos, state))| {
                    if index != 0 {
                        tick = tick
                            .checked_add(gap)
                            .expect("the generated tick horizon was validated before sampling");
                    }
                    ScriptStep {
                        tick,
                        action: Action::SetBlock { pos, state },
                    }
                })
                .collect();
            Script::new(steps)
        })
        .boxed()
}

pub fn sample_scripts(domain: &GenerationDomain, budget: SearchBudget) -> Result<Vec<Script>, String> {
    generated_oracle_ticks(domain, 0)?;
    let strategy = script_strategy(domain);
    let mut runner = runner(budget);
    let mut scripts = Vec::with_capacity(budget.cases as usize);
    for case_index in 0..budget.cases {
        let tree = strategy
            .new_tree(&mut runner)
            .map_err(|error| format!("case {case_index}: {error}"))?;
        scripts.push(tree.current());
    }
    Ok(scripts)
}

fn same_divergence_class(first: &Divergence, candidate: &Divergence) -> bool {
    first.pos == candidate.pos && first.left == candidate.left && first.right == candidate.right
}

fn evaluate_fresh<L, R, F>(
    fresh_oracles: &mut F,
    script: &Script,
    region: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
) -> DifferentialOutcome
where
    L: WorldOracle,
    R: WorldOracle,
    F: FnMut() -> (L, R),
{
    let (mut left, mut right) = fresh_oracles();
    run_differential(script, region, &mut left, &mut right, settle_ticks)
}

pub fn search_and_shrink<L, R, F>(
    domain: &GenerationDomain,
    budget: SearchBudget,
    region: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    mut fresh_oracles: F,
) -> SearchOutcome
where
    L: WorldOracle,
    R: WorldOracle,
    F: FnMut() -> (L, R),
{
    if let Err(message) = generated_oracle_ticks(domain, settle_ticks) {
        return SearchOutcome::InvalidConfiguration { message };
    }
    let strategy = script_strategy(domain);
    let mut runner = runner(budget);

    for case_index in 0..budget.cases {
        let mut tree = strategy
            .new_tree(&mut runner)
            .expect("a validated finite generation domain must build a value tree");
        let original_script = tree.current();
        let original_divergence = match evaluate_fresh(
            &mut fresh_oracles,
            &original_script,
            region,
            settle_ticks,
        ) {
            DifferentialOutcome::Agreed => continue,
            DifferentialOutcome::OracleFailed(failure) => {
                return SearchOutcome::OracleFailed {
                    case_index,
                    during_shrink: false,
                    failure,
                };
            }
            DifferentialOutcome::Diverged(divergence) => divergence,
        };

        let mut minimal_script = original_script.clone();
        let mut minimal_divergence = original_divergence.clone();
        let mut shrink_attempts = 0_u32;
        let mut has_candidate = tree.simplify();
        while has_candidate && shrink_attempts < budget.shrink_attempts {
            shrink_attempts += 1;
            let candidate = tree.current();
            match evaluate_fresh(&mut fresh_oracles, &candidate, region, settle_ticks) {
                DifferentialOutcome::OracleFailed(failure) => {
                    return SearchOutcome::OracleFailed {
                        case_index,
                        during_shrink: true,
                        failure,
                    };
                }
                DifferentialOutcome::Diverged(divergence)
                    if same_divergence_class(&original_divergence, &divergence) =>
                {
                    minimal_script = candidate;
                    minimal_divergence = divergence;
                    has_candidate = tree.simplify();
                }
                DifferentialOutcome::Agreed | DifferentialOutcome::Diverged(_) => {
                    has_candidate = tree.complicate();
                }
            }
        }

        return SearchOutcome::Found(FoundDivergence {
            case_index,
            original_script,
            minimal_script,
            original_divergence,
            minimal_divergence,
            shrink_attempts,
        });
    }

    SearchOutcome::NoDivergence {
        cases_run: budget.cases,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayCase {
    format_version: u32,
    scenario: String,
    seed: u64,
    case_index: u32,
    settle_ticks: u64,
    region: Vec<ReplayProbe>,
    steps: Vec<ReplayStep>,
    divergence: ReplayDivergence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReplayProbe {
    pos: (i32, i32, i32),
    candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReplayStep {
    tick: u64,
    action: ReplayAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ReplayAction {
    SetBlock {
        pos: (i32, i32, i32),
        state: String,
    },
    RunCommand {
        command: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReplayDivergence {
    tick: u64,
    pos: (i32, i32, i32),
    left: Option<String>,
    right: Option<String>,
}

impl ReplayCase {
    pub fn from_found(
        scenario: &str,
        seed: u64,
        settle_ticks: u64,
        region: Vec<((i32, i32, i32), Vec<String>)>,
        found: &FoundDivergence,
    ) -> Self {
        Self {
            format_version: REPLAY_FORMAT_VERSION,
            scenario: scenario.to_owned(),
            seed,
            case_index: found.case_index,
            settle_ticks,
            region: region
                .into_iter()
                .map(|(pos, candidates)| ReplayProbe { pos, candidates })
                .collect(),
            steps: found
                .minimal_script
                .steps
                .iter()
                .map(|step| ReplayStep {
                    tick: step.tick,
                    action: match &step.action {
                        Action::SetBlock { pos, state } => ReplayAction::SetBlock {
                            pos: *pos,
                            state: state.clone(),
                        },
                        Action::RunCommand(command) => ReplayAction::RunCommand {
                            command: command.clone(),
                        },
                    },
                })
                .collect(),
            divergence: ReplayDivergence {
                tick: found.minimal_divergence.tick,
                pos: found.minimal_divergence.pos,
                left: found.minimal_divergence.left.clone(),
                right: found.minimal_divergence.right.clone(),
            },
        }
    }

    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(json: &str) -> Result<Self, String> {
        let replay: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        if replay.format_version != REPLAY_FORMAT_VERSION {
            return Err(format!(
                "unsupported differential replay format {}, expected {REPLAY_FORMAT_VERSION}",
                replay.format_version
            ));
        }
        Ok(replay)
    }

    pub fn script(&self) -> Script {
        Script::new(
            self.steps
                .iter()
                .map(|step| ScriptStep {
                    tick: step.tick,
                    action: match &step.action {
                        ReplayAction::SetBlock { pos, state } => Action::SetBlock {
                            pos: *pos,
                            state: state.clone(),
                        },
                        ReplayAction::RunCommand { command } => Action::RunCommand(command.clone()),
                    },
                })
                .collect(),
        )
    }

    pub fn replay<L, R, F>(&self, mut fresh_oracles: F) -> Result<DifferentialOutcome, String>
    where
        L: WorldOracle,
        R: WorldOracle,
        F: FnMut() -> (L, R),
    {
        let script = self.script();
        bounded_oracle_ticks(script.last_tick(), self.settle_ticks, "replay")?;
        let region = self
            .region
            .iter()
            .map(|probe| (probe.pos, probe.candidates.clone()))
            .collect::<Vec<_>>();
        Ok(evaluate_fresh(
            &mut fresh_oracles,
            &script,
            &region,
            self.settle_ticks,
        ))
    }

    pub fn expected_divergence(&self) -> Divergence {
        Divergence {
            tick: self.divergence.tick,
            pos: self.divergence.pos,
            left: self.divergence.left.clone(),
            right: self.divergence.right.clone(),
        }
    }
}
