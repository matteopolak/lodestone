//! Native-only JSONL recording for Criterion benchmark measurements.
//!
//! The recorder writes gitignored local evidence and prints an advisory ratio
//! against the previous matching run. It deliberately has no dependency on a
//! benchmark's owning crate, so all benchmark families can share the schema.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Walks up from `CARGO_MANIFEST_DIR` to find the workspace root. Falls back
/// to the manifest directory when the crate is built outside a checkout.
fn workspace_root() -> PathBuf {
    let start = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut dir = start.as_path();
    loop {
        let candidate = dir.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&candidate)
            && text.contains("[workspace]")
        {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return start,
        }
    }
}

/// Best-effort short git SHA of `HEAD`.
fn git_sha() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(workspace_root())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Best-effort machine identifier used to scope comparisons to one machine.
fn machine_id() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn build_profile() -> &'static str {
    if cfg!(debug_assertions) { "debug" } else { "release" }
}

/// One recorded measurement.
#[derive(Debug)]
pub struct Record<'a> {
    pub bench: &'a str,
    pub metric: &'a str,
    pub scene: &'a str,
    pub value: f64,
    pub unit: &'a str,
}

fn serialized_line(rec: &Record<'_>, timestamp: u64, machine: &str, profile: &str, sha: &str) -> String {
    serde_json::json!({
        "timestamp": timestamp,
        "git_sha": sha,
        "machine": machine,
        "profile": profile,
        "scene": rec.scene,
        "metric": rec.metric,
        "value": rec.value,
        "unit": rec.unit,
    })
    .to_string()
}

/// Appends `rec` to `bench-results/<bench>.jsonl` and prints a same-machine,
/// same-profile, same-scene, same-metric advisory ratio against the latest
/// prior entry. Recording failures never fail the benchmark.
pub fn record(rec: Record<'_>) {
    let root = workspace_root();
    let dir = root.join("bench-results");
    if std::fs::create_dir_all(&dir).is_err() {
        eprintln!("[bench-support] could not create {}; skipping recording", dir.display());
        return;
    }
    let path: PathBuf = dir.join(format!("{}.jsonl", rec.bench));

    let machine = machine_id();
    let profile = build_profile();
    let sha = git_sha();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let previous = last_matching(&path, &machine, profile, rec.scene, rec.metric);
    let line = serialized_line(&rec, ts, &machine, profile, &sha);

    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            let _ = writeln!(f, "{line}");
        }
        Err(e) => {
            eprintln!("[bench-support] could not append to {}: {e}", path.display());
        }
    }

    println!(
        "[bench-support] recorded {}={:.3}{} scene={:?} machine={machine} profile={profile} sha={sha} -> {}",
        rec.metric,
        rec.value,
        rec.unit,
        rec.scene,
        path.display()
    );

    if let Some(prev) = previous {
        let ratio = rec.value / prev;
        let flag = if !(0.75..=1.25).contains(&ratio) { " *** OUTSIDE ±25% BAND ***" } else { "" };
        println!(
            "[bench-support] vs previous same-machine/profile/scene run: {:.3}{} -> {:.3}{} ratio={ratio:.3}{flag}",
            prev, rec.unit, rec.value, rec.unit
        );
    } else {
        println!("[bench-support] no prior same-machine/profile/scene baseline yet — this run establishes one");
    }
}

fn last_matching(path: &Path, machine: &str, profile: &str, scene: &str, metric: &str) -> Option<f64> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut found = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v.get("machine").and_then(|x| x.as_str()) == Some(machine)
            && v.get("profile").and_then(|x| x.as_str()) == Some(profile)
            && v.get("scene").and_then(|x| x.as_str()) == Some(scene)
            && v.get("metric").and_then(|x| x.as_str()) == Some(metric)
        {
            found = v.get("value").and_then(|x| x.as_f64());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_line_preserves_the_jsonl_schema() {
        let line = serialized_line(
            &Record {
                bench: "smoke",
                metric: "blocks",
                scene: "fixed",
                value: 7.5,
                unit: "calls",
            },
            123,
            "machine",
            "debug",
            "deadbeef",
        );
        let value: serde_json::Value = serde_json::from_str(&line).expect("valid JSONL record");
        assert_eq!(value["timestamp"], 123);
        assert_eq!(value["git_sha"], "deadbeef");
        assert_eq!(value["machine"], "machine");
        assert_eq!(value["profile"], "debug");
        assert_eq!(value["scene"], "fixed");
        assert_eq!(value["metric"], "blocks");
        assert_eq!(value["value"], 7.5);
        assert_eq!(value["unit"], "calls");
    }
}
