//! Sizes the evaluation redundancy in both density evaluators, per interior
//! column of the same 12×12 sweep §12.130's `C_ss`/`I_ss` are the median of.
//!
//! # What it is
//!
//! DESIGN.md §12.134 measured a 4.87× `noise_scaled` redundancy ratio per column
//! and established that the `Op`-table node-sharing pass could not collect it,
//! because neither evaluator has a per-node memo. Before designing one, this
//! answers *which* memo would hit: it reports, per node kind and per evaluator,
//! how many visits a **one-slot last-`(x, z)`** memo (vanilla's
//! `NoiseChunk.Cache2D`), a full **`(node, x, z)`** map, and a full
//! **`(node, x, y, z)`** map would have answered — all three simultaneously, from
//! one run, without changing a value.
//!
//! # How it works
//!
//! `#[ignore]`d and printing rather than asserting: it is a measurement, and the
//! numbers it produces belong in DESIGN.md, not in a threshold. The window is one
//! **column**, reset between columns, because that is the widest scope a
//! per-chunk memo could have — a probe left running across the whole sweep would
//! report a hit rate no real cache could deliver.
//!
//! ```text
//! cargo test --release -p lodestone-worldgen --features gen-counters \
//!   --test density_redundancy_probe -- --ignored --nocapture
//! ```
//!
//! Without `gen-counters` the probe is inert and the test reports zero visits,
//! which it fails on rather than passing vacuously.
//!
//! # How to change it
//!
//! `SIDE` and the interior definition must stay identical to
//! `benches/generation.rs`'s, or the ratios stop being comparable to `I_ss`.

use lodestone_worldgen::density::Density;
use lodestone_worldgen::engine::redundancy_probe as probe;

const SEED: i64 = 42;
const SIDE: i32 = 12;

#[test]
#[ignore = "measurement probe; needs --features gen-counters and is driven by hand"]
fn redundancy_per_interior_column() {
    let generator = lodestone_server::overworld_generator(SEED);
    probe::enable();

    let mut per_column: Vec<(u64, u64)> = Vec::new();
    let mut agg = probe::snapshot();
    agg = {
        let mut z = agg.clone();
        for i in 0..Density::KIND_COUNT {
            z.point_visits[i] = 0;
            z.field_visits[i] = 0;
        }
        z
    };
    let mut interior = 0usize;

    let mut memo_hits = 0u64;
    let mut memo_misses = 0u64;
    for cz in 0..SIDE {
        for cx in 0..SIDE {
            probe::reset();
            lodestone_worldgen::density::xz_memo::reset_stats();
            let column = generator.column(cx, cz);
            std::hint::black_box(column.non_air_count());
            let is_interior = cx > 0 && cz > 0 && cx < SIDE - 1 && cz < SIDE - 1;
            if is_interior {
                let s = probe::snapshot();
                per_column.push((s.point_total(), s.field_total()));
                agg.accumulate(&s);
                let (h, m) = lodestone_worldgen::density::xz_memo::stats();
                memo_hits += h;
                memo_misses += m;
                interior += 1;
            }
        }
    }
    probe::disable();

    assert_eq!(interior, 100, "the interior definition drifted from the bench's");
    assert!(
        agg.point_total() > 0 || agg.field_total() > 0,
        "the probe recorded nothing — this build has no `gen-counters` feature, so \
         every number below would be a vacuous zero"
    );

    let n = interior as f64;
    println!("\n== redundancy over {interior} interior columns of a {SIDE}x{SIDE} sweep, seed {SEED} ==");
    let memo_total = memo_hits + memo_misses;
    if memo_total > 0 {
        println!(
            "  xz_memo: {:>10.0} lookups/column, hit rate {:>6.2}%  ({memo_hits} hits, {memo_misses} misses)",
            memo_total as f64 / n,
            100.0 * memo_hits as f64 / memo_total as f64,
        );
    } else {
        println!("  xz_memo: no lookups — no node carries a memo id (the memo is an island)");
    }
    println!(
        "  point-interpreter visits/column : {:>12.0}",
        agg.point_total() as f64 / n
    );
    println!(
        "  field-evaluator  visits/column  : {:>12.0}",
        agg.field_total() as f64 / n
    );

    let report = |label: &str,
                  visits: &[u64; Density::KIND_COUNT],
                  single: &[u64; Density::KIND_COUNT],
                  xz: &[u64; Density::KIND_COUNT],
                  xyz: &[u64; Density::KIND_COUNT]| {
        let tv: u64 = visits.iter().sum();
        if tv == 0 {
            println!("\n  {label}: no visits");
            return;
        }
        let ts: u64 = single.iter().sum();
        let txz: u64 = xz.iter().sum();
        let txyz: u64 = xyz.iter().sum();
        println!("\n  {label} — totals per column and hit rates of three hypothetical memos");
        println!(
            "    visits {:>12.0}   1-slot(x,z) {:>6.2}%   map(x,z) {:>6.2}%   map(x,y,z) {:>6.2}%",
            tv as f64 / n,
            100.0 * ts as f64 / tv as f64,
            100.0 * txz as f64 / tv as f64,
            100.0 * txyz as f64 / tv as f64,
        );
        let mut rows: Vec<(usize, u64)> = visits.iter().copied().enumerate().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        println!(
            "    {:<18} {:>12} {:>10} {:>10} {:>10}",
            "kind", "visits/col", "1slot%", "mapxz%", "mapxyz%"
        );
        for (kind, v) in rows.into_iter().filter(|&(_, v)| v > 0) {
            println!(
                "    {:<18} {:>12.0} {:>9.1}% {:>9.1}% {:>9.1}%",
                Density::KIND_NAMES[kind],
                v as f64 / n,
                100.0 * single[kind] as f64 / v as f64,
                100.0 * xz[kind] as f64 / v as f64,
                100.0 * xyz[kind] as f64 / v as f64,
            );
        }
    };

    report(
        "POINT INTERPRETER (Density::compute — everything under a leaf)",
        &agg.point_visits,
        &agg.point_xz_single_hits,
        &agg.point_xz_map_hits,
        &agg.point_xyz_map_hits,
    );
    report(
        "FIELD EVALUATOR (Field::eval — the compiled Op graph)",
        &agg.field_visits,
        &agg.field_xz_single_hits,
        &agg.field_xz_map_hits,
        &agg.field_xyz_map_hits,
    );

    per_column.sort();
    let med = per_column[per_column.len() / 2];
    println!(
        "\n  median column: point {} visits, field {} visits\n",
        med.0, med.1
    );
}
