//! Bounded, resumable campaigns over the shared live gameplay scenarios.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::differential::{DifferentialOutcome, OracleFailureKind, Script};
use generation::{GeneratedSearch, ReplayCase, SearchBudget, SearchOutcome, retry_oracle_timeouts};

pub mod generation;
use crate::redstone_contraption as contraption;
mod live_fluid;
mod live_redstone;

const FORMAT_VERSION: u32 = 1;
const GENERATION_VERSION: u32 = 2;
pub const MAX_JSON_BYTES: u64 = 1_048_576;
pub const MAX_CAMPAIGN_CASES: u32 = 1_000_000;
pub const MAX_SHRINK_ATTEMPTS: u32 = 4_096;
pub const MAX_TIMING_ATTEMPTS: u32 = 32;

type Region = Vec<((i32, i32, i32), Vec<String>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    Fluid,
    Redstone,
}

impl Scenario {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fluid => live_fluid::SCENARIO,
            Self::Redstone => live_redstone::SCENARIO,
        }
    }

    pub fn default_seed(self) -> u64 {
        match self {
            Self::Fluid => 0x549_11e,
            Self::Redstone => 0x549_0eed,
        }
    }

    fn domain(self) -> generation::GenerationDomain {
        match self {
            Self::Fluid => live_fluid::domain(),
            Self::Redstone => live_redstone::domain(),
        }
    }

    fn region(self) -> Region {
        match self {
            Self::Fluid => live_fluid::region(),
            Self::Redstone => live_redstone::region(),
        }
    }

    fn settle_ticks(self) -> u64 {
        match self {
            Self::Fluid => live_fluid::SETTLE_TICKS,
            Self::Redstone => live_redstone::SETTLE_TICKS,
        }
    }

    fn evaluate(self, endpoint: &str, script: &Script, region: &[((i32, i32, i32), Vec<String>)], settle: u64) -> DifferentialOutcome {
        match self {
            Self::Fluid => live_fluid::evaluate_at(script, region, settle, false, endpoint),
            Self::Redstone => live_redstone::evaluate_at(script, region, settle, false, endpoint),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignConfig {
    pub scenario: Scenario,
    pub endpoint: String,
    pub seed: u64,
    pub cases: u32,
    pub shrink_attempts: u32,
    pub timing_attempts: u32,
}

impl CampaignConfig {
    pub fn validate(&self) -> Result<(), String> {
        let address: SocketAddr = self.endpoint.parse()
            .map_err(|_| "campaign endpoint must be a numeric loopback IP:port".to_owned())?;
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err("campaign endpoint must be a numeric loopback IP with a nonzero port".to_owned());
        }
        if !(1..=MAX_CAMPAIGN_CASES).contains(&self.cases) {
            return Err(format!("cases must be in 1..={MAX_CAMPAIGN_CASES}"));
        }
        if self.shrink_attempts > MAX_SHRINK_ATTEMPTS {
            return Err(format!("shrink attempts must be in 0..={MAX_SHRINK_ATTEMPTS}"));
        }
        if !(1..=MAX_TIMING_ATTEMPTS).contains(&self.timing_attempts) {
            return Err(format!("timing attempts must be in 1..={MAX_TIMING_ATTEMPTS}"));
        }
        Ok(())
    }

    fn budget(&self) -> SearchBudget {
        SearchBudget { seed: self.seed, cases: self.cases, shrink_attempts: self.shrink_attempts }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accounting {
    pub oracle_attempts: u64,
    pub retry_attempts: u64,
    pub oracle_failures: u64,
    pub timing_failures: u64,
    pub accepted_evaluations: u64,
    pub accepted_ticks: u64,
    pub accepted_actions: u64,
}

impl Accounting {
    fn observe(&mut self, script: &Script, settle: u64, outcome: &DifferentialOutcome) {
        self.oracle_attempts += 1;
        let ticks = match outcome {
            DifferentialOutcome::Agreed => script.last_tick() + settle + 1,
            DifferentialOutcome::Diverged(divergence) => divergence.tick + 1,
            DifferentialOutcome::OracleFailed(failure) => {
                self.oracle_failures += 1;
                if failure.kind == OracleFailureKind::Timeout {
                    self.timing_failures += 1;
                }
                return;
            }
        };
        self.accepted_evaluations += 1;
        self.accepted_ticks += ticks;
        self.accepted_actions += script.steps.iter().filter(|step| step.tick < ticks).count() as u64;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignStatus {
    Ready,
    Complete,
    OracleFailed,
    Divergence,
    ReplayUnconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureRecord {
    pub phase: String,
    pub tick: u64,
    pub side: String,
    pub kind: String,
}

impl FailureRecord {
    fn from_outcome(phase: &str, outcome: &DifferentialOutcome) -> Option<Self> {
        let DifferentialOutcome::OracleFailed(failure) = outcome else { return None };
        Some(Self {
            phase: phase.to_owned(),
            tick: failure.tick,
            side: format!("{:?}", failure.side),
            kind: format!("{:?}", failure.kind),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    format_version: u32,
    generation_version: u32,
    pub config: CampaignConfig,
    pub next_case_index: u32,
    pub accepted_cases: u32,
    pub generated_evaluations: u64,
    pub shrink_evaluations: u64,
    pub search: Accounting,
    pub replay: Accounting,
    pub status: CampaignStatus,
    pub finding: Option<ReplayCase>,
    pub last_oracle_failure: Option<FailureRecord>,
}

impl Checkpoint {
    pub fn new(config: CampaignConfig) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            format_version: FORMAT_VERSION,
            generation_version: GENERATION_VERSION,
            config,
            next_case_index: 0,
            accepted_cases: 0,
            generated_evaluations: 0,
            shrink_evaluations: 0,
            search: Accounting::default(),
            replay: Accounting::default(),
            status: CampaignStatus::Ready,
            finding: None,
            last_oracle_failure: None,
        })
    }

    pub fn from_json(json: &str, config: &CampaignConfig) -> Result<Self, String> {
        if json.len() as u64 > MAX_JSON_BYTES {
            return Err("campaign checkpoint exceeds the JSON size cap".to_owned());
        }
        config.validate()?;
        let checkpoint: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        if checkpoint.format_version != FORMAT_VERSION || checkpoint.generation_version != GENERATION_VERSION {
            return Err("campaign checkpoint format or generator version changed".to_owned());
        }
        if &checkpoint.config != config {
            return Err("resume configuration differs from the saved campaign".to_owned());
        }
        if checkpoint.next_case_index > config.cases || checkpoint.accepted_cases != checkpoint.next_case_index {
            return Err("campaign checkpoint case accounting is inconsistent".to_owned());
        }
        let domain = config.scenario.domain();
        let horizon = (domain.max_steps() as u64 - 1) * domain.max_tick_gap()
            + config.scenario.settle_ticks() + 1;
        let reserve_candidates = u64::from(config.cases) * (u64::from(config.shrink_attempts) + 1);
        let reserve_attempts = (reserve_candidates + 1) * u64::from(config.timing_attempts);
        for accounting in [&checkpoint.search, &checkpoint.replay] {
            if accounting.accepted_evaluations.checked_add(accounting.oracle_failures) != Some(accounting.oracle_attempts)
                || accounting.timing_failures > accounting.oracle_failures
                || accounting.retry_attempts > accounting.oracle_failures
                || accounting.oracle_attempts.checked_add(reserve_attempts).is_none()
                || accounting.accepted_ticks.checked_add(reserve_attempts * horizon).is_none()
                || accounting.accepted_actions.checked_add(reserve_attempts * domain.max_steps() as u64).is_none()
                || accounting.accepted_ticks > accounting.accepted_evaluations.saturating_mul(horizon)
                || accounting.accepted_actions > accounting.accepted_evaluations.saturating_mul(domain.max_steps() as u64) {
                return Err("campaign checkpoint oracle accounting is inconsistent".to_owned());
            }
        }
        if checkpoint.generated_evaluations.checked_add(reserve_candidates).is_none()
            || checkpoint.shrink_evaluations.checked_add(reserve_candidates).is_none() {
            return Err("campaign checkpoint candidate counters cannot represent another bounded slice".to_owned());
        }
        if checkpoint.generated_evaluations.checked_add(checkpoint.shrink_evaluations)
            != checkpoint.search.oracle_attempts.checked_sub(checkpoint.search.retry_attempts) {
            return Err("campaign checkpoint candidate accounting is inconsistent".to_owned());
        }
        if (checkpoint.status == CampaignStatus::Complete) != (checkpoint.next_case_index == config.cases)
            && checkpoint.status != CampaignStatus::Divergence {
            return Err("campaign checkpoint completion status is inconsistent".to_owned());
        }
        if matches!(checkpoint.status, CampaignStatus::Divergence | CampaignStatus::ReplayUnconfirmed) != checkpoint.finding.is_some() {
            return Err("campaign checkpoint finding status is inconsistent".to_owned());
        }
        if let Some(replay) = &checkpoint.finding {
            let expected_index = if checkpoint.status == CampaignStatus::Divergence {
                checkpoint.next_case_index.checked_sub(1).ok_or("confirmed finding has no accepted case")?
            } else {
                checkpoint.next_case_index
            };
            if replay.seed() != config.seed || replay.case_index() != expected_index {
                return Err("campaign checkpoint finding provenance differs from its saved case".to_owned());
            }
            replay.replay_generated_with(config.scenario.name(), &config.scenario.domain(),
                &config.scenario.region(), config.scenario.settle_ticks(), |_, _, _| DifferentialOutcome::Agreed)?;
        }
        Ok(checkpoint)
    }

    pub fn to_json_pretty(&self) -> Result<String, String> {
        let json = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        if json.len() as u64 > MAX_JSON_BYTES {
            return Err("campaign checkpoint exceeds the JSON size cap".to_owned());
        }
        Ok(json)
    }
}

/// Runs a bounded slice of one campaign and saves only completed case boundaries.
/// A failed case retains its index so resume regenerates and resets that case.
pub fn advance_with<E, S>(
    checkpoint: &mut Checkpoint,
    run_cases: u32,
    mut evaluate: E,
    mut save: S,
) -> Result<(), String>
where
    E: FnMut(&Script, &[((i32, i32, i32), Vec<String>)], u64) -> DifferentialOutcome,
    S: FnMut(&Checkpoint) -> Result<(), String>,
{
    checkpoint.config.validate()?;
    if !(1..=MAX_CAMPAIGN_CASES).contains(&run_cases) {
        return Err(format!("run cases must be in 1..={MAX_CAMPAIGN_CASES}"));
    }
    if matches!(checkpoint.status, CampaignStatus::Complete | CampaignStatus::Divergence) {
        return Ok(());
    }
    let config = checkpoint.config.clone();
    let domain = config.scenario.domain();
    let region = config.scenario.region();
    let settle = config.scenario.settle_ticks();
    if checkpoint.status == CampaignStatus::ReplayUnconfirmed {
        let replay = checkpoint.finding.clone().ok_or("unconfirmed checkpoint has no replay")?;
        let outcome = replay.replay_generated_with(config.scenario.name(), &domain, &region, settle, |script, probes, settle| {
            evaluate_bounded(config.timing_attempts, &mut checkpoint.replay, script, probes, settle, &mut evaluate)
        })?;
        checkpoint.last_oracle_failure = FailureRecord::from_outcome("replay", &outcome);
        if matches!(outcome, DifferentialOutcome::Diverged(ref divergence) if *divergence == replay.expected_divergence()) {
            checkpoint.next_case_index += 1;
            checkpoint.accepted_cases += 1;
            checkpoint.status = CampaignStatus::Divergence;
        }
        save(checkpoint)?;
        return Ok(());
    }
    checkpoint.status = CampaignStatus::Ready;
    checkpoint.last_oracle_failure = None;
    let mut search = GeneratedSearch::new(&domain, config.budget(), checkpoint.next_case_index, settle)?;
    for _ in 0..run_cases {
        if search.next_index() == config.cases {
            break;
        }
        let mut first_evaluation = true;
        let outcome = search.next_with(&region, settle, |script, probes, settle| {
            if first_evaluation {
                checkpoint.generated_evaluations += 1;
                first_evaluation = false;
            } else {
                checkpoint.shrink_evaluations += 1;
            }
            evaluate_bounded(config.timing_attempts, &mut checkpoint.search, script, probes, settle, &mut evaluate)
        });
        match outcome {
            SearchOutcome::NoDivergence { cases_run: 1 } => {
                checkpoint.next_case_index += 1;
                checkpoint.accepted_cases += 1;
                if checkpoint.next_case_index == config.cases {
                    checkpoint.status = CampaignStatus::Complete;
                }
            }
            SearchOutcome::Found(found) => {
                let replay = ReplayCase::from_found(config.scenario.name(), config.seed, settle, region.clone(), &found);
                let replay = ReplayCase::from_json(&replay.to_json_pretty().map_err(|error| error.to_string())?)?;
                checkpoint.finding = Some(replay.clone());
                checkpoint.status = CampaignStatus::ReplayUnconfirmed;
                save(checkpoint)?;
                let outcome = replay.replay_generated_with(config.scenario.name(), &domain, &region, settle, |script, probes, settle| {
                    evaluate_bounded(config.timing_attempts, &mut checkpoint.replay, script, probes, settle, &mut evaluate)
                })?;
                checkpoint.last_oracle_failure = FailureRecord::from_outcome("replay", &outcome);
                if matches!(outcome, DifferentialOutcome::Diverged(ref divergence) if *divergence == replay.expected_divergence()) {
                    checkpoint.next_case_index += 1;
                    checkpoint.accepted_cases += 1;
                    checkpoint.status = CampaignStatus::Divergence;
                }
            }
            SearchOutcome::OracleFailed { during_shrink, failure, .. } => {
                checkpoint.last_oracle_failure = FailureRecord::from_outcome(
                    if during_shrink { "shrink" } else { "generation" },
                    &DifferentialOutcome::OracleFailed(failure),
                );
                checkpoint.status = CampaignStatus::OracleFailed;
            }
            SearchOutcome::InvalidConfiguration { message } => return Err(message),
            SearchOutcome::NoDivergence { .. } => return Err("single-case search reported an invalid case total".to_owned()),
        }
        save(checkpoint)?;
        if !matches!(checkpoint.status, CampaignStatus::Ready | CampaignStatus::Complete) {
            break;
        }
    }
    Ok(())
}

fn evaluate_bounded<E>(
    attempts: u32,
    accounting: &mut Accounting,
    script: &Script,
    region: &[((i32, i32, i32), Vec<String>)],
    settle: u64,
    evaluate: &mut E,
) -> DifferentialOutcome
where
    E: FnMut(&Script, &[((i32, i32, i32), Vec<String>)], u64) -> DifferentialOutcome,
{
    let mut attempt_index = 0;
    retry_oracle_timeouts(attempts, || {
        if attempt_index != 0 {
            accounting.retry_attempts += 1;
        }
        attempt_index += 1;
        let outcome = evaluate(script, region, settle);
        accounting.observe(script, settle, &outcome);
        outcome
    })
}

pub fn read_json(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_JSON_BYTES + 1).read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_JSON_BYTES {
        return Err("campaign JSON input exceeds the size cap".to_owned());
    }
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

fn atomic_json(path: &Path, json: &str) -> Result<(), String> {
    if json.len() as u64 > MAX_JSON_BYTES {
        return Err("campaign JSON output exceeds the size cap".to_owned());
    }
    let temporary = path.with_extension("json.tmp");
    let mut file = File::create(&temporary).map_err(|error| error.to_string())?;
    file.write_all(json.as_bytes()).and_then(|()| file.sync_all()).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())?;
    File::open(path.parent().ok_or("JSON output has no parent")?)
        .and_then(|directory| directory.sync_all()).map_err(|error| error.to_string())
}

fn lock(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(path).map_err(|error| format!("open campaign lock: {error}"))?;
    file.try_lock().map_err(|error| format!("campaign lock is unavailable: {error}"))?;
    Ok(file)
}

pub fn run_campaign(config: CampaignConfig, output: &Path, run_cases: u32, resume: bool) -> Result<Checkpoint, String> {
    config.validate()?;
    if !(1..=MAX_CAMPAIGN_CASES).contains(&run_cases) {
        return Err(format!("run cases must be in 1..={MAX_CAMPAIGN_CASES}"));
    }
    std::fs::create_dir_all(output).map_err(|error| error.to_string())?;
    let _output_lock = lock(&output.join("campaign.lock"))?;
    let address: SocketAddr = config.endpoint.parse().map_err(|error| format!("{error}"))?;
    let lane_lock = std::env::temp_dir().join(format!("lodestone-{}-{}.lock", config.scenario.name(), address.port()));
    let _lane_lock = lock(&lane_lock)?;
    let path = output.join("checkpoint.json");
    let mut checkpoint = if resume {
        Checkpoint::from_json(&read_json(&path)?, &config)?
    } else {
        if path.exists() {
            return Err("campaign checkpoint already exists; use --resume or a new output directory".to_owned());
        }
        let checkpoint = Checkpoint::new(config.clone())?;
        atomic_json(&path, &checkpoint.to_json_pretty()?)?;
        checkpoint
    };
    advance_with(&mut checkpoint, run_cases, |script, region, settle| {
        config.scenario.evaluate(&config.endpoint, script, region, settle)
    }, |state| {
        atomic_json(&path, &state.to_json_pretty()?)?;
        if let Some(replay) = &state.finding {
            atomic_json(&output.join("replay.json"), &replay.to_json_pretty().map_err(|error| error.to_string())?)?;
        }
        Ok(())
    })?;
    Ok(checkpoint)
}

pub fn replay_file(config: &CampaignConfig, path: &Path) -> Result<Accounting, String> {
    config.validate()?;
    let replay = ReplayCase::from_json(&read_json(path)?)?;
    let address: SocketAddr = config.endpoint.parse().map_err(|error| format!("{error}"))?;
    let lane_lock = std::env::temp_dir().join(format!("lodestone-{}-{}.lock", config.scenario.name(), address.port()));
    let _lane_lock = lock(&lane_lock)?;
    let mut accounting = Accounting::default();
    let outcome = replay.replay_generated_with(config.scenario.name(), &config.scenario.domain(),
        &config.scenario.region(), config.scenario.settle_ticks(), |script, region, settle| {
        evaluate_bounded(config.timing_attempts, &mut accounting, script, region, settle, &mut |script, region, settle| {
            config.scenario.evaluate(&config.endpoint, script, region, settle)
        })
    })?;
    if matches!(outcome, DifferentialOutcome::Diverged(ref divergence) if *divergence == replay.expected_divergence()) {
        return Ok(accounting);
    }
    Err(format!("recorded divergence was not reproduced: {outcome:?}; accounting={accounting:?}"))
}
