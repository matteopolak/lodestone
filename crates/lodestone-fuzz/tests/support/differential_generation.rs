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

/// Re-evaluates the same candidate after a timing timeout, up to an explicit
/// attempt bound. Each call must perform its own fresh setup. Gameplay
/// divergences and non-timeout oracle failures are returned immediately.
pub fn retry_oracle_timeouts<E>(attempts: u32, mut evaluate: E) -> DifferentialOutcome
where
    E: FnMut() -> DifferentialOutcome,
{
    assert!(attempts > 0, "a retry budget needs at least one attempt");
    for attempt in 1..=attempts {
        let outcome = evaluate();
        if matches!(
            outcome,
            DifferentialOutcome::OracleFailed(OracleFailure {
                kind: lodestone_fuzz::differential::OracleFailureKind::Timeout,
                ..
            }) if attempt < attempts
        ) {
            continue;
        }
        return outcome;
    }
    unreachable!("a positive bounded attempt loop always returns on its last iteration")
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
    first.tick == candidate.tick
        && first.pos == candidate.pos
        && first.left == candidate.left
        && first.right == candidate.right
}

fn validate_probe_region(region: &[((i32, i32, i32), Vec<String>)]) -> Result<(), String> {
    if region.is_empty() {
        return Err("a differential comparison needs at least one probe".to_owned());
    }
    for (index, (_, candidates)) in region.iter().enumerate() {
        if candidates.is_empty() {
            return Err(format!("differential probe {index} needs at least one candidate state"));
        }
    }
    Ok(())
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
    search_and_shrink_with(domain, budget, region, settle_ticks, |script, region, settle_ticks| {
        evaluate_fresh(&mut fresh_oracles, script, region, settle_ticks)
    })
}

/// Searches and shrinks through a caller-owned candidate evaluator.
///
/// The factory-based [`search_and_shrink`] is the convenient hermetic form.
/// Live scenarios use this form so every evaluation can reset and verify its
/// remote rig before running, and can return setup failures as
/// [`DifferentialOutcome::OracleFailed`] instead of panicking or calling them
/// gameplay divergences.
pub fn search_and_shrink_with<E>(
    domain: &GenerationDomain,
    budget: SearchBudget,
    region: &[((i32, i32, i32), Vec<String>)],
    settle_ticks: u64,
    mut evaluate: E,
) -> SearchOutcome
where
    E: FnMut(&Script, &[((i32, i32, i32), Vec<String>)], u64) -> DifferentialOutcome,
{
    if budget.cases == 0 {
        return SearchOutcome::InvalidConfiguration {
            message: "a differential search needs at least one case".to_owned(),
        };
    }
    if let Err(message) = validate_probe_region(region) {
        return SearchOutcome::InvalidConfiguration { message };
    }
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
        let original_divergence = match evaluate(&original_script, region, settle_ticks) {
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
            match evaluate(&candidate, region, settle_ticks) {
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
        self.replay_with(|script, region, settle_ticks| {
            evaluate_fresh(&mut fresh_oracles, script, region, settle_ticks)
        })
    }

    /// Replays through a caller-owned evaluator so a live scenario can reset
    /// and validate its remote rig before applying the recorded script.
    pub fn replay_with<E>(&self, mut evaluate: E) -> Result<DifferentialOutcome, String>
    where
        E: FnMut(&Script, &[((i32, i32, i32), Vec<String>)], u64) -> DifferentialOutcome,
    {
        let script = self.script();
        bounded_oracle_ticks(script.last_tick(), self.settle_ticks, "replay")?;
        let region = self
            .region
            .iter()
            .map(|probe| (probe.pos, probe.candidates.clone()))
            .collect::<Vec<_>>();
        validate_probe_region(&region)?;
        Ok(evaluate(&script, &region, self.settle_ticks))
    }

    /// Validates an untrusted replay against one generated `SetBlock` domain
    /// before allowing the caller-owned evaluator to create an oracle.
    ///
    /// A live scenario should use this entry point rather than
    /// [`Self::replay_with`]: replay JSON can otherwise contain a raw command,
    /// a relative position outside the reset lane or probes that the scenario
    /// never clears.
    pub fn replay_generated_with<E>(
        &self,
        expected_scenario: &str,
        domain: &GenerationDomain,
        expected_region: &[((i32, i32, i32), Vec<String>)],
        expected_settle_ticks: u64,
        evaluate: E,
    ) -> Result<DifferentialOutcome, String>
    where
        E: FnMut(&Script, &[((i32, i32, i32), Vec<String>)], u64) -> DifferentialOutcome,
    {
        if self.scenario != expected_scenario {
            return Err(format!(
                "replay scenario {:?} does not match expected scenario {expected_scenario:?}",
                self.scenario
            ));
        }
        if self.settle_ticks != expected_settle_ticks {
            return Err(format!(
                "replay settle ticks {} do not match expected settle ticks {expected_settle_ticks}",
                self.settle_ticks
            ));
        }

        let replay_region = self
            .region
            .iter()
            .map(|probe| (probe.pos, probe.candidates.clone()))
            .collect::<Vec<_>>();
        if replay_region != expected_region {
            return Err("replay probe region does not match the live scenario lane".to_owned());
        }
        if self.steps.is_empty() {
            return Err("replay script must contain at least one step".to_owned());
        }
        if self.steps.len() > domain.max_steps {
            return Err(format!(
                "replay script has {} steps, exceeding the generated domain maximum of {}",
                self.steps.len(), domain.max_steps
            ));
        }
        if self.steps[0].tick != 0 {
            return Err("replay script must begin at tick 0".to_owned());
        }

        for (index, step) in self.steps.iter().enumerate() {
            if index != 0 {
                let previous_tick = self.steps[index - 1].tick;
                let Some(gap) = step.tick.checked_sub(previous_tick) else {
                    return Err(format!("replay step {index} moves backward in time"));
                };
                if gap > domain.max_tick_gap {
                    return Err(format!(
                        "replay step {index} has tick gap {gap}, exceeding the generated domain maximum of {}",
                        domain.max_tick_gap
                    ));
                }
            }

            match &step.action {
                ReplayAction::SetBlock { pos, state } => {
                    if !domain.positions.contains(pos) {
                        return Err(format!(
                            "replay step {index} position {pos:?} is outside the generated domain"
                        ));
                    }
                    if !domain.states.contains(state) {
                        return Err(format!(
                            "replay step {index} state {state:?} is outside the generated domain"
                        ));
                    }
                }
                ReplayAction::RunCommand { .. } => {
                    return Err(format!(
                        "replay step {index} is RunCommand; live generated replays allow SetBlock only"
                    ));
                }
            }
        }

        let script = self.script();
        let oracle_ticks = bounded_oracle_ticks(script.last_tick(), self.settle_ticks, "replay")?;
        if self.divergence.tick >= oracle_ticks {
            return Err(format!(
                "replay divergence tick {} is outside the {}-tick execution horizon",
                self.divergence.tick, oracle_ticks
            ));
        }
        let Some((_, candidates)) = expected_region
            .iter()
            .find(|(pos, _)| *pos == self.divergence.pos)
        else {
            return Err(format!(
                "replay divergence position {:?} is outside the probe region",
                self.divergence.pos
            ));
        };
        for (side, state) in [
            ("left", self.divergence.left.as_ref()),
            ("right", self.divergence.right.as_ref()),
        ] {
            if let Some(state) = state {
                if !candidates.contains(state) {
                    return Err(format!(
                        "replay divergence {side} state {state:?} is outside the probe alphabet"
                    ));
                }
            }
        }
        if self.divergence.left == self.divergence.right {
            return Err("replay records equal states instead of a divergence".to_owned());
        }

        self.replay_with(evaluate)
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
