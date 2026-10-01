use std::path::PathBuf;
use std::process::ExitCode;

use lodestone_fuzz::campaign::{CampaignConfig, CampaignStatus, Scenario, replay_file, run_campaign};

const HELP: &str = "differential-campaign --scenario fluid|redstone|waterlogging|piston --output DIR [--seed U64]
    [--cases 1000] [--run-cases N] [--shrink-attempts 32] [--timing-attempts 3]
    [--endpoint 127.0.0.1:25571] [--resume]
differential-campaign --scenario fluid|redstone|waterlogging|piston --replay FILE [--timing-attempts 3]
    [--endpoint 127.0.0.1:25571]
Case and shrink budgets count deterministic work. Endpoints must be numeric loopback addresses.";

fn number(value: &str) -> Result<u64, String> {
    if let Some(hex) = value.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).map_err(|error| error.to_string())
    } else {
        value.parse().map_err(|error| format!("{error}"))
    }
}

fn run() -> Result<ExitCode, String> {
    let mut arguments = std::env::args().skip(1);
    let mut scenario = None;
    let mut output = None;
    let mut replay = None;
    let mut seed = None;
    let mut cases = 1_000;
    let mut run_cases = None;
    let mut shrink_attempts = 32;
    let mut timing_attempts = 3;
    let mut endpoint = "127.0.0.1:25571".to_owned();
    let mut resume = false;
    let mut seen = std::collections::HashSet::new();
    while let Some(flag) = arguments.next() {
        if flag == "--help" || flag == "-h" {
            println!("{HELP}");
            return Ok(ExitCode::SUCCESS);
        }
        if !seen.insert(flag.clone()) {
            return Err(format!("duplicate argument {flag}"));
        }
        if flag == "--resume" {
            resume = true;
            continue;
        }
        let value = arguments.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let bounded_number = |value: &str| -> Result<u32, String> {
            u32::try_from(number(value)?).map_err(|_| "integer exceeds u32".to_owned())
        };
        match flag.as_str() {
            "--scenario" => scenario = Some(match value.as_str() {
                "fluid" => Scenario::Fluid,
                "redstone" => Scenario::Redstone,
                "waterlogging" => Scenario::Waterlogging,
                "piston" => Scenario::Piston,
                _ => return Err("scenario must be fluid, redstone, waterlogging or piston".to_owned()),
            }),
            "--output" => output = Some(PathBuf::from(value)),
            "--replay" => replay = Some(PathBuf::from(value)),
            "--seed" => seed = Some(number(&value)?),
            "--cases" => cases = bounded_number(&value)?,
            "--run-cases" => run_cases = Some(bounded_number(&value)?),
            "--shrink-attempts" => shrink_attempts = bounded_number(&value)?,
            "--timing-attempts" => timing_attempts = bounded_number(&value)?,
            "--endpoint" => endpoint = value,
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    let scenario = scenario.ok_or("--scenario is required")?;
    let config = CampaignConfig {
        scenario, endpoint, seed: seed.unwrap_or_else(|| scenario.default_seed()),
        cases, shrink_attempts, timing_attempts,
    };
    if let Some(path) = replay {
        if output.is_some() || resume || run_cases.is_some() || seen.contains("--cases")
            || seen.contains("--seed") || seen.contains("--shrink-attempts") {
            return Err("replay accepts only scenario, endpoint, timing-attempts and replay arguments".to_owned());
        }
        let accounting = replay_file(&config, &path)?;
        println!("{}", serde_json::to_string_pretty(&accounting).map_err(|error| error.to_string())?);
        return Ok(ExitCode::SUCCESS);
    }
    let output = output.ok_or("--output is required for a campaign")?;
    let checkpoint = run_campaign(config, &output, run_cases.unwrap_or(cases), resume)?;
    println!("{}", checkpoint.to_json_pretty()?);
    Ok(match checkpoint.status {
        CampaignStatus::Ready | CampaignStatus::Complete => ExitCode::SUCCESS,
        CampaignStatus::Divergence => ExitCode::from(1),
        CampaignStatus::OracleFailed | CampaignStatus::ReplayUnconfirmed => ExitCode::from(2),
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}\n{HELP}");
            ExitCode::from(2)
        }
    }
}
