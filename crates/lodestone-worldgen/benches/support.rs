//! Shared bench-recording helper for `lodestone-worldgen`'s criterion
//! benches. Deliberately small and self-contained (not a crate, not a
//! workspace dependency) — per the harness scope in `docs/roadmap/benchmarks.md`,
//! creating a new shared crate is out of scope for this pass, and duplicating
//! ~100 lines twice (worldgen + v26-2) is cheaper than the coordination cost of
//! a third crate mid-epic. If a third bench site needs this, promoting it to a
//! real crate is the right move then — see the harness patch note in the
//! epic report for what that would need from the workspace root.
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

/// Units that make a metric an **absolute timing**.
///
/// Deliberately absolute time only. `stage_<name>_pct` (unit `%`) and
/// `linearity_ratio_vs_expected` (unit `x`) are *ratios*, and the stage split is
/// one of the things you specifically want to read *with* counters on — so they
/// are a considered carve-out, not an oversight. Structural counts (`calls`)
/// are likewise unaffected.
///
/// Measured against a real `--features gen-counters --bench generation -- --test`
/// run, not inferred from a grep: **33 metrics refused, 18 still recorded.** Of
/// the 9 units this bench uses, 3 are blocked (`ns`, `us`, `s`) and 6 are not
/// (`%`, `x`, `calls`, `allocs`, `draws`, `bytes`) — so the guard is neither
/// vacuous nor total. `µs`/`ms` are listed ahead of need, because adding a metric
/// in them is likelier than remembering this table exists.
///
/// Note when re-checking this: metrics are recorded through *two* call shapes —
/// an explicit `unit: "us"` field and a `("name", value, "us")` tuple loop — and
/// grepping only the first undercounts the units in use (it misses `ns` and
/// `bytes` entirely). Read the run, not the grep.
const ABSOLUTE_TIME_UNITS: &[&str] = &["ns", "us", "µs", "ms", "s"];

/// Units measuring **work the process actually performed**, which the counters
/// themselves add to.
///
/// `instructions` (and `cycles`, listed ahead of need) belong here and **not**
/// with the structural counts above, and the distinction is the whole point:
/// `allocs`, `calls` and `draws` count events in the pipeline, and the counter
/// hooks add none of those. Retired instructions count *every* instruction the
/// process executed, and a `bump` is `fetch_add` plus a thread-local read at
/// hundreds of thousands of sites per column — so an instruction count from a
/// `gen-counters` build is inflated by the instrument, in the same way and for a
/// stronger reason than a timing is.
///
/// This mattered immediately: `i_ss_median_instructions_per_column` was added in
/// a unit whose whole method was running with counters on, and without this list
/// its first recorded value would have been a counters-build number that
/// `bench-compare` would later ratio against clean runs forever.
const WORK_PERFORMED_UNITS: &[&str] = &["instructions", "cycles"];

/// Whether recording `unit` while the structural counters are compiled in would
/// write a number that is not comparable to anything.
///
/// **Counters-on inflates a burst by roughly 3×**, so a counter run and a timing
/// run must never be the same run. That was a calibration two units of this
/// drive had to rediscover from memory; encoding it here makes it structural
/// instead. Split out as a pure function of its inputs so the rule can be read
/// and checked without running a bench.
///
/// Covers both [`ABSOLUTE_TIME_UNITS`] and [`WORK_PERFORMED_UNITS`]; the function
/// keeps its name because "timing" is what every caller is protecting, and an
/// instruction count is a timing that happens to be reproducible.
pub fn timing_is_poisoned_by_counters(unit: &str, counters_enabled: bool) -> bool {
    counters_enabled
        && (ABSOLUTE_TIME_UNITS.contains(&unit) || WORK_PERFORMED_UNITS.contains(&unit))
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
///
/// **Refuses to record an absolute timing from a `gen-counters` build.** See
/// [`timing_is_poisoned_by_counters`]: the counters inflate a burst ~3×, and a
/// recorded timing is worse than a missing one because `bench-compare` will
/// happily ratio it against a clean run and report a 3× "regression". The
/// refusal is loud on stderr and skips only that one metric — structural counts
/// and ratios from the same run still record, which is the point of running with
/// counters at all.
pub fn record(rec: Record<'_>) {
    if timing_is_poisoned_by_counters(rec.unit, lodestone_worldgen::counters::enabled()) {
        eprintln!(
            "[bench-support] REFUSING to record {:?} ({} {}): this build has \
             `--features gen-counters`, which inflates a burst by roughly 3×, so the \
             number is not comparable to a clean run. Counter runs and timing runs must \
             be separate runs. Re-run without `--features gen-counters` for timings.",
            rec.metric, rec.value, rec.unit
        );
        return;
    }
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
