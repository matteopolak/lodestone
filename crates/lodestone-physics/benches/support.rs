//! Shared bench-recording helper for `lodestone-physics`'s criterion benches.
//! Deliberately small and self-contained (not a crate, not a workspace
//! dependency) — per the harness scope in `docs/roadmap/benchmarks.md`,
//! creating a new shared crate is out of scope for this pass, and duplicating
//! ~100 lines five times (worldgen, v770, world, entity, physics — otherwise
//! byte-for-byte identical) is cheaper than the coordination cost of a
//! shared crate mid-epic. **This is the fifth copy** — `docs/benchmark-
//! harness.md` names five as the threshold where promoting this to a real
//! crate stops being premature. It is not done in this pass: doing it here
//! would mean editing `worldgen`'s and `entity`'s `Cargo.toml`/`mod support;`
//! lines while those crates are held by concurrent agents, which is a bigger
//! blast radius than one more copy. Flagged in `docs/benchmark-harness.md`
//! as the next thing to do once those crates are free.
//!
//! # What this does
//!
//! Appends one JSON object per recorded metric to
//! `<workspace-root>/bench-results/<file>.jsonl` (gitignored — measurement
//! data, not source), carrying the metadata the evidence standard in
//! `CLAUDE.md` requires: machine, git sha, build profile, and a scene
//! description, alongside the metric itself. Then, if a prior run recorded the
//! same `(scene, metric)` key on the same machine and profile, prints a
//! same-machine ratio against it — never a bare cross-machine absolute number,
//! and never a pass/fail: this is advisory output for a developer to read, not
//! a gate. Nothing here is wired into `cargo test`/CI.

#![allow(dead_code)] // not every bench file exercises every helper

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Walks up from `CARGO_MANIFEST_DIR` to find the workspace root (the
/// directory whose `Cargo.toml` contains a `[workspace]` table). Falls back to
/// `CARGO_MANIFEST_DIR` itself if none is found (e.g. crate built standalone),
/// so recording still works, just scoped to the crate directory.
fn workspace_root() -> PathBuf {
    let start = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut dir = start.as_path();
    loop {
        let candidate = dir.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            if text.contains("[workspace]") {
                return dir.to_path_buf();
            }
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return start,
        }
    }
}

/// Best-effort short git SHA of `HEAD`. `"unknown"` if `git` is unavailable or
/// this isn't a checkout (e.g. a packaged crate) — recording must never fail
/// the bench over this.
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

/// Best-effort machine identifier: hostname if available, else `"unknown"`.
/// Used only to scope regression comparisons to the *same* machine — per the
/// evidence standard, a number is not comparable across machines at all.
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

/// One recorded measurement: a named metric (e.g. `"column_us_median"`), a
/// value, and the scene it was measured under (e.g. `"seed=42 radius=8"`).
/// `bench` is the bench-binary name (`generation`, `chunk_light`, …), used to
/// pick the output file so different benches don't interleave in one log.
#[derive(Debug)]
pub struct Record<'a> {
    pub bench: &'a str,
    pub metric: &'a str,
    pub scene: &'a str,
    pub value: f64,
    pub unit: &'a str,
}

/// Appends `rec` to `bench-results/<bench>.jsonl` and prints a same-machine,
/// same-profile, same-scene, same-metric ratio against the most recent prior
/// entry, if one exists. Never panics on I/O failure (a missing/unwritable
/// `bench-results/` directory degrades to "recording skipped", not a bench
/// failure) — these are local dev artifacts, not gated correctness.
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

    // Read prior entries with the same key before we append this one, so the
    // comparison is against the *previous* run, not itself.
    let previous = last_matching(&path, &machine, profile, rec.scene, rec.metric);

    let line = serde_json::json!({
        "timestamp": ts,
        "git_sha": sha,
        "machine": machine,
        "profile": profile,
        "scene": rec.scene,
        "metric": rec.metric,
        "value": rec.value,
        "unit": rec.unit,
    });

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
        // ±25% tolerance band, per docs/roadmap/benchmarks.md's stated policy.
        // Advisory only — printed, never asserted.
        let flag = if !(0.75..=1.25).contains(&ratio) { " *** OUTSIDE ±25% BAND ***" } else { "" };
        println!(
            "[bench-support] vs previous same-machine/profile/scene run: {:.3}{} -> {:.3}{} ratio={ratio:.3}{flag}",
            prev, rec.unit, rec.value, rec.unit
        );
    } else {
        println!("[bench-support] no prior same-machine/profile/scene baseline yet — this run establishes one");
    }
}

/// Finds the most recent prior JSONL line matching `(machine, profile, scene,
/// metric)`, returning its `value`. Linear scan — `bench-results/*.jsonl`
/// files are local dev logs, expected to stay small (dozens to low hundreds of
/// runs), not a database.
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
