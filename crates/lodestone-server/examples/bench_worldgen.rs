//! Release-mode worldgen benchmark. Measures per-chunk generation time and a
//! coarse per-stage breakdown for the composed overworld generator (the real,
//! JVM-verified shape+fluid+surface pipeline the shell and integrated server
//! both drive).
//!
//! Run with:
//!   cargo run --release -p lodestone-server --example bench_worldgen
//!
//! This is a measurement tool, not a test. It builds the generator once (as
//! callers are told to) and times a fixed patch of chunks so the numbers are
//! comparable across runs. Timings are wall-clock `Instant`; the per-stage
//! split comes from `OverworldGenerator::column_timed`, an instrumentation twin
//! of `column` that does the identical work.
//!
//! The parallel section's speedup is recorded into the shared, gitignored
//! `bench-results/generation.jsonl` (see [`record`] below) — that file
//! already carries `lodestone-worldgen`'s own criterion-bench metrics for
//! this scene (`column_median_us`, `stage_*_pct`, …, written by that crate's
//! own `benches/support.rs`, which this file intentionally does not import:
//! per that file's own doc comment, a shared bench-recording crate is out of
//! scope for now, so this is a second, smaller, independent copy of the same
//! append-one-JSON-line-per-metric pattern, scoped to what this example
//! needs).

use std::time::Instant;

use lodestone_server::overworld_generator;

fn main() {
    let seed: i64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    // A render-distance-8 patch is 17×17 = 289 chunks; RD 16 is 33×33 = 1089.
    let radius: i32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    let build_start = Instant::now();
    let gtor = overworld_generator(seed);
    let build = build_start.elapsed();

    let coords: Vec<(i32, i32)> = (-radius..=radius)
        .flat_map(|cz| (-radius..=radius).map(move |cx| (cx, cz)))
        .collect();
    let n = coords.len();

    // Warm up (touch the caches / branch predictor) without counting it.
    for &(cx, cz) in coords.iter().take(8) {
        std::hint::black_box(gtor.column(cx, cz));
    }

    // Headline: single-threaded full-column time over the whole patch.
    let mut per_chunk_us: Vec<f64> = Vec::with_capacity(n);
    let serial_start = Instant::now();
    for &(cx, cz) in &coords {
        let t = Instant::now();
        let col = gtor.column(cx, cz);
        per_chunk_us.push(t.elapsed().as_nanos() as f64 / 1000.0);
        std::hint::black_box(col.non_air_count());
    }
    let serial_total = serial_start.elapsed();

    per_chunk_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = per_chunk_us.iter().sum::<f64>() / n as f64;
    let median = per_chunk_us[n / 2];
    let p95 = per_chunk_us[(n as f64 * 0.95) as usize];
    let min = per_chunk_us[0];
    let max = per_chunk_us[n - 1];

    // Per-stage breakdown was measured separately via an instrumented twin of
    // `column`; as of the FxHash corner-cache change the split at RD8/seed 3 was
    // roughly noise 40% / surface 51% / intern 7% / sampler-build+heightmap 2%.
    // The instrumented twin is not kept in-tree (it duplicated the verified
    // pipeline); re-add it locally if you need to re-measure the split.

    // Parallel wall-clock over the same patch (embarrassingly parallel: each
    // column builds a fresh sampler and reads only immutable generator state).
    let workers = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4);
    let batch = coords.len().div_ceil(workers);
    let par_start = Instant::now();
    let par_count: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = coords
            .chunks(batch.max(1))
            .map(|slice| {
                let gtor = &gtor;
                scope.spawn(move || {
                    slice
                        .iter()
                        .map(|&(cx, cz)| gtor.column(cx, cz).non_air_count())
                        .sum::<usize>()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum()
    });
    let par_total = par_start.elapsed();
    std::hint::black_box(par_count);

    println!("seed={seed} radius={radius} chunks={n} cores={workers}");
    println!("generator build: {build:?} (one-time, amortised across all chunks)");
    println!();
    println!("--- single-threaded full column() ---");
    println!("  total     {serial_total:?} for {n} chunks");
    println!(
        "  per chunk mean={mean:.0}us median={median:.0}us p95={p95:.0}us min={min:.0}us max={max:.0}us"
    );
    println!(
        "  throughput {:.0} chunks/s",
        n as f64 / serial_total.as_secs_f64()
    );
    println!();
    println!("--- parallel wall-clock ({workers} threads) ---");
    println!("  total     {par_total:?} for {n} chunks");
    println!(
        "  per chunk {:.0}us (wall/chunk)",
        par_total.as_nanos() as f64 / 1000.0 / n as f64
    );
    let speedup = serial_total.as_secs_f64() / par_total.as_secs_f64();
    println!("  speedup   {speedup:.2}x over single-threaded");

    // Record the speedup this run measured — issue #86 asked for the
    // parallel path to be benchmarked, but its speedup had never actually
    // been captured into bench-results/generation.jsonl. This is that
    // number, alongside the raw parallel per-chunk wall time. Both are
    // ratios/rates, not absolute-ms assertions — per CLAUDE.md's evidence
    // standard, an absolute figure on a shared, variably-loaded machine is
    // not a stable enough signal to gate anything on.
    let scene = format!("seed={seed} radius={radius}({n} chunks) workers={workers}");
    record(Record {
        bench: "generation",
        metric: "parallel_speedup_vs_serial",
        scene: &scene,
        value: speedup,
        unit: "x",
    });
    record(Record {
        bench: "generation",
        metric: "parallel_wall_us_per_chunk",
        scene: &scene,
        value: par_total.as_nanos() as f64 / 1000.0 / n as f64,
        unit: "us",
    });
}

// --- Minimal bench-result recorder -----------------------------------------
//
// A trimmed copy of the pattern in `crates/lodestone-worldgen/benches/support.rs`
// (see that file's own doc comment for why this is duplicated rather than
// shared): append one JSON line per metric to
// `<workspace-root>/bench-results/<bench>.jsonl`, carrying machine/git-sha/
// profile/scene metadata alongside the value, then print a same-machine,
// same-profile, same-scene, same-metric ratio against the previous run if one
// exists. Advisory only — never asserts, never gates, matches the upstream
// helper's own stated policy (`docs/roadmap/benchmarks.md`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

struct Record<'a> {
    bench: &'a str,
    metric: &'a str,
    scene: &'a str,
    value: f64,
    unit: &'a str,
}

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

fn record(rec: Record<'_>) {
    let root = workspace_root();
    let dir = root.join("bench-results");
    if std::fs::create_dir_all(&dir).is_err() {
        eprintln!(
            "[bench_worldgen] could not create {}; skipping recording",
            dir.display()
        );
        return;
    }
    let path: PathBuf = dir.join(format!("{}.jsonl", rec.bench));

    let machine = machine_id();
    let profile = build_profile();
    let sha = git_sha();
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

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

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            let _ = writeln!(f, "{line}");
        }
        Err(e) => {
            eprintln!(
                "[bench_worldgen] could not append to {}: {e}",
                path.display()
            );
        }
    }

    println!(
        "[bench_worldgen] recorded {}={:.3}{} scene={:?} machine={machine} profile={profile} sha={sha} -> {}",
        rec.metric,
        rec.value,
        rec.unit,
        rec.scene,
        path.display()
    );

    if let Some(prev) = previous {
        let ratio = rec.value / prev;
        println!(
            "[bench_worldgen] vs previous same-machine/profile/scene run: {:.3}{} -> {:.3}{} ratio={ratio:.3}",
            prev, rec.unit, rec.value, rec.unit
        );
    } else {
        println!(
            "[bench_worldgen] no prior same-machine/profile/scene baseline yet — this run establishes one"
        );
    }
}

fn last_matching(path: &Path, machine: &str, profile: &str, scene: &str, metric: &str) -> Option<f64> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut found = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
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
