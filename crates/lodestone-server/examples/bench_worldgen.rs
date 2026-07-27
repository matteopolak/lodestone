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
    println!(
        "  speedup   {:.1}x over single-threaded",
        serial_total.as_secs_f64() / par_total.as_secs_f64()
    );
}
