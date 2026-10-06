//! Release benchmark of the production 26.3 Overworld source over one spawn-area patch: cold
//! serial per-column latency (p50/p99), a warm repeat, the shaped and decoration halves, and cold
//! parallel throughput through `ChunkSource::columns`.
//!
//!   cargo run --release -p lodestone-server --example bench_overworld_263 -- [seed] [radius] [new]
//!
//! `new` runs the cold parallel batch only, so `/usr/bin/time -l` reports its CPU time, which
//! holds steady on a loaded machine where wall-clock numbers do not.

use std::time::Instant;

use lodestone_server::{ChunkSource, overworld_chunk_source};

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)]
}

fn measure(label: &str, source: &dyn ChunkSource, coords: &[(i32, i32)]) {
    let mut lat = Vec::new();
    let t = Instant::now();
    for &(cx, cz) in coords {
        let s = Instant::now();
        std::hint::black_box(source.column(cx, cz));
        lat.push(s.elapsed().as_secs_f64() * 1000.0);
    }
    let serial = t.elapsed().as_secs_f64();
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "{label} cold serial: {:.1} chunks/s  p50 {:.1} ms  p99 {:.1} ms  max {:.1} ms",
        coords.len() as f64 / serial,
        pct(&lat, 0.5),
        pct(&lat, 0.99),
        lat[lat.len() - 1]
    );
    let t = Instant::now();
    for &(cx, cz) in coords {
        std::hint::black_box(source.column(cx, cz));
    }
    println!("{label} warm serial: {:.1} chunks/s", coords.len() as f64 / t.elapsed().as_secs_f64());
}

fn parallel(label: &str, source: &dyn ChunkSource, coords: &[(i32, i32)]) {
    let t = Instant::now();
    std::hint::black_box(source.columns(coords));
    println!("{label} cold parallel batch: {:.1} chunks/s ({:.2} s for {})", coords.len() as f64 / t.elapsed().as_secs_f64(), t.elapsed().as_secs_f64(), coords.len());
}

fn main() {
    let seed: i64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(42);
    let radius: i32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(4);
    let coords: Vec<(i32, i32)> = (-radius..=radius).flat_map(|z| (-radius..=radius).map(move |x| (x, z))).collect();
    println!("seed {seed}, {} columns", coords.len());
    if std::env::args().nth(3).as_deref() == Some("new") {
        parallel("26.3", &overworld_chunk_source(seed), &coords);
        return;
    }
    let build = Instant::now();
    let new_a = overworld_chunk_source(seed);
    println!("26.3 build {:.2} s", build.elapsed().as_secs_f64());
    measure("26.3", &new_a, &coords);
    {
        let split = overworld_chunk_source(seed);
        let t = Instant::now();
        let n = (2 * radius + 5).pow(2);
        for z in -radius - 2..=radius + 2 {
            for x in -radius - 2..=radius + 2 {
                std::hint::black_box(split.terrain().shaped(x, z));
            }
        }
        println!("26.3 shaped only: {n} chunks in {:.2} s = {:.1} ms each", t.elapsed().as_secs_f64(), t.elapsed().as_secs_f64() * 1000.0 / n as f64);
        let t = Instant::now();
        for &(cx, cz) in &coords {
            std::hint::black_box(split.column(cx, cz));
        }
        println!("26.3 decoration over warm shaped: {:.1} ms per column", t.elapsed().as_secs_f64() * 1000.0 / coords.len() as f64);
    }
    let new_b = overworld_chunk_source(seed);
    parallel("26.3", &new_b, &coords);
}
