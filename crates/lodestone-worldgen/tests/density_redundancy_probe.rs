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
    let mut leaf_hits = 0u64;
    let mut leaf_misses = 0u64;
    for cz in 0..SIDE {
        for cx in 0..SIDE {
            probe::reset();
            lodestone_worldgen::density::xz_memo::reset_stats();
            lodestone_worldgen::engine::reset_leaf_memo_stats();
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
                let (lh, lm) = lodestone_worldgen::engine::leaf_memo_stats();
                leaf_hits += lh;
                leaf_misses += lm;
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
    let leaf_total = leaf_hits + leaf_misses;
    if leaf_total > 0 {
        println!(
            "  leaf_memo: {:>9.0} lookups/column, hit rate {:>6.2}%  ({leaf_hits} hits, {leaf_misses} misses)",
            leaf_total as f64 / n,
            100.0 * leaf_hits as f64 / leaf_total as f64,
        );
    } else {
        println!("  leaf_memo: no lookups — the field evaluator's leaf memo is an island");
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

    // The cross-sampler split. A `Scratch` is per-sampler, so of the `map(x,y,z)`
    // duplication above only the part that is *not* also duplicated within one
    // sampler is reachable by sharing a scratch — everything else a per-sampler
    // memo already answers (or already refuses). Printing the unscoped rate alone
    // is what makes the field evaluator's 46.9% `flat_cache` row unreadable.
    println!(
        "\n  CROSS-SAMPLER SPLIT (field evaluator) — {:.1} distinct samplers/column",
        agg.field_scopes as f64 / n
    );
    println!(
        "    {:<18} {:>12} {:>12} {:>12} {:>12} {:>12}",
        "kind", "visits/col", "dup(x,y,z)", "own-sampler", "cross-sampler", "1slot(xyz)"
    );
    let mut rows: Vec<(usize, u64)> = agg.field_visits.iter().copied().enumerate().collect();
    rows.sort_by(|a, b| {
        let ca = agg.field_xyz_map_hits[a.0] - agg.field_xyz_own_hits[a.0];
        let cb = agg.field_xyz_map_hits[b.0] - agg.field_xyz_own_hits[b.0];
        cb.cmp(&ca)
    });
    let mut cross_total = 0u64;
    for (kind, v) in rows.into_iter().filter(|&(_, v)| v > 0) {
        let dup = agg.field_xyz_map_hits[kind];
        let own = agg.field_xyz_own_hits[kind];
        let cross = dup - own;
        cross_total += cross;
        if cross == 0 && dup == 0 {
            continue;
        }
        println!(
            "    {:<18} {:>12.0} {:>12.0} {:>12.0} {:>12.0} {:>12.0}",
            Density::KIND_NAMES[kind],
            v as f64 / n,
            dup as f64 / n,
            own as f64 / n,
            cross as f64 / n,
            agg.field_xyz_single_hits[kind] as f64 / n,
        );
    }
    println!(
        "    {:<18} {:>12} {:>12} {:>12} {:>12.0}",
        "TOTAL cross/col", "", "", "", cross_total as f64 / n
    );

    // The point interpreter's own one-slot-`(x, y, z)` column, for the leaf kinds
    // the field evaluator reaches through `graph.leaf(..).compute(..)`. This is the
    // number that says whether a duplicate pair is adjacent in the walk.
    println!("\n  POINT INTERPRETER one-slot(x,y,z) — is a duplicate pair adjacent?");
    for kind in 0..Density::KIND_COUNT {
        let v = agg.point_visits[kind];
        if v == 0 {
            continue;
        }
        let s1 = agg.point_xyz_single_hits[kind];
        if s1 == 0 {
            continue;
        }
        println!(
            "    {:<18} visits/col {:>10.0}   1slot(x,y,z) {:>6.2}%",
            Density::KIND_NAMES[kind],
            v as f64 / n,
            100.0 * s1 as f64 / v as f64,
        );
    }

    per_column.sort();
    let med = per_column[per_column.len() / 2];
    println!(
        "\n  median column: point {} visits, field {} visits\n",
        med.0, med.1
    );
}

/// Where a steady-state column's cost actually is, by stage — the measurement
/// that says whether the density engine is still the right place to work.
///
/// # What it is
///
/// §12.130's `I_ss` is one number for a whole column, and every worldgen perf unit
/// since has spent itself inside the density evaluators on the strength of
/// §12.134's 4.87× redundancy ratio. That ratio was real and is now collected
/// (§12.140, −11.22%), but a *ratio inside one subsystem* says nothing about that
/// subsystem's share of the column. This reports the share.
///
/// ```text
/// cargo test --release -p lodestone-worldgen \
///   --test density_redundancy_probe -- --ignored --nocapture stage_share
/// ```
///
/// # How it works
///
/// [`OverworldGenerator::column_timed`] runs the identical ten stages `column`
/// does — `benches/generation.rs`'s own block-for-block anti-drift control is what
/// makes that claim checkable — and reports a `Duration` per stage. Density
/// evaluation lives in `aquifer` (building the three samplers, plus
/// `max_preliminary_surface_level`) and `shape` (98,304 `AquiferSystem::block_at`
/// calls, i.e. every `Field::eval` in the column bar the carvers'), so
/// `aquifer + shape` is an **upper bound** on the density engine's share.
///
/// **The warm-up is load-bearing and the first version of this probe did not have
/// it.** `column_timed`'s `vegetation` bucket times `vegetation_stage`, which reads
/// the post-ore world of its 3×3 — and on a cold store that *computes* those
/// neighbours, so their entire pre-ore and ore pipelines land in the `vegetation`
/// row. Measured both ways: cold store reports vegetation **51.6%** and
/// `aquifer + shape` 14.7%, which invites exactly the wrong conclusion, because
/// most of that 51.6% is other chunks' `shape`. Sweeping with `column()` first
/// leaves every neighbour's post-ore in the store, so each row times only its own
/// stage. **A stage-attribution bucket that can contain another chunk's whole
/// pipeline is not an attribution.**
///
/// # How to change it
///
/// This is wall clock, and DESIGN.md §12.140 measured 50–124% within-arm `C_ss`
/// spread on a loaded machine — so a *share* is what is reported and a duration is
/// not. A share is a ratio inside a single run, which is exactly the shape that
/// survives machine load, and the median over 100 columns is taken per stage.
/// **Do not turn this into a before/after comparator**; `I_ss` is that.
///
/// The shares are **unweighted**, and that was checked rather than assumed.
/// §12.130's counts over this sweep (`pre_ore_computed` 256, `post_ore_computed`
/// 196, vegetation 144, over 144 columns) suggest weighting each pre-ore stage by
/// 1.78 and ore by 1.36 — those are sweep *averages*, dominated by the leading
/// edge filling its 5×5 closure, and the median interior column is not the average.
/// The control decides it: the unweighted total lands within ~2% of `C_ss` while the
/// weighted one overshoots by ~1.4×, so a median interior column pays about one
/// pass of each stage. The total is printed for exactly that comparison — **an
/// attribution whose parts do not sum to the whole is not an attribution.**
#[test]
#[ignore = "measurement probe; driven by hand"]
fn stage_share_of_a_steady_state_column() {
    let generator = lodestone_server::overworld_generator(SEED);

    // See the doc comment: without this the `vegetation` row times its
    // neighbours' pre-ore and ore stages as well as its own.
    for cz in -1..=SIDE {
        for cx in -1..=SIDE {
            std::hint::black_box(generator.column(cx, cz).non_air_count());
        }
    }

    let mut rows: Vec<(&str, Vec<f64>)> = vec![
        ("aquifer", Vec::new()),
        ("shape", Vec::new()),
        ("biome", Vec::new()),
        ("surface", Vec::new()),
        ("materialize", Vec::new()),
        ("carve", Vec::new()),
        ("ore", Vec::new()),
        ("vegetation", Vec::new()),
        ("top_layer", Vec::new()),
        ("intern", Vec::new()),
    ];
    let mut totals: Vec<f64> = Vec::new();

    for cz in 0..SIDE {
        for cx in 0..SIDE {
            let (col, t) = generator.column_timed(cx, cz);
            std::hint::black_box(col.non_air_count());
            if !(cx > 0 && cz > 0 && cx < SIDE - 1 && cz < SIDE - 1) {
                continue;
            }
            let us = |d: std::time::Duration| d.as_secs_f64() * 1e6;
            let each = [
                us(t.aquifer),
                us(t.shape),
                us(t.biome),
                us(t.surface),
                us(t.materialize),
                us(t.carve),
                us(t.ore),
                us(t.vegetation),
                us(t.top_layer),
                us(t.intern),
            ];
            for (row, v) in rows.iter_mut().zip(each) {
                row.1.push(v);
            }
            totals.push(each.iter().sum());
        }
    }

    assert_eq!(totals.len(), 100, "the interior definition drifted from the bench's");
    let median = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (v[49] + v[50]) / 2.0
    };
    let total_med = median(&mut totals);
    assert!(total_med > 0.0, "every stage measured zero — nothing was timed");

    println!("\n== stage share of a steady-state column, {SIDE}x{SIDE} sweep, seed {SEED} ==");
    println!("   store warmed with column() first; median of the 100 interior columns");
    println!("   {:<14} {:>11} {:>9}", "stage", "median us", "share");
    let mut medians: Vec<(&str, f64)> = Vec::new();
    for (name, mut v) in rows {
        medians.push((name, median(&mut v)));
    }
    let sum: f64 = medians.iter().map(|&(_, m)| m).sum();
    let mut density = 0.0;
    for &(name, m) in &medians {
        if name == "aquifer" || name == "shape" {
            density += m;
        }
        println!("   {name:<14} {m:>11.0} {:>8.1}%", 100.0 * m / sum);
    }
    println!(
        "\n   stages sum to {sum:.0} us (column_timed's own total {total_med:.0} us) — compare\n   \
         against C_ss from benches/generation.rs. Agreement is this attribution's control:\n   \
         parts that do not sum to the whole are not an attribution."
    );
    println!(
        "   aquifer + shape = {:.1}% of a column, and that is an UPPER bound on the density\n   \
         engine: `shape` is also the four-way BlockKind fill and the heightmap scan.",
        100.0 * density / sum
    );
}
