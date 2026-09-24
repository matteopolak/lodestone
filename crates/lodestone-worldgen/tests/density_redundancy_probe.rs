//! Bounded, ignored measurement of density-evaluator visit redundancy.
//!
//! One counter window covers the complete contiguous target stream. Its
//! two-chunk pre-ore halo is prepared while every selected target remains
//! unprepared, but an earlier `column` call can prepare a later target through
//! its own closure. Reported totals are therefore normalized by submitted
//! targets, not independent cold-call samples. Field own-sampler counts span a
//! `Scratch` lifetime, not one `Field::eval` call.
//!
//! Run two contiguous targets with:
//! ```text
//! LODESTONE_DENSITY_PROBE_COLUMNS=2 cargo test --release -p lodestone-worldgen \
//!   --features gen-counters --test density_redundancy_probe \
//!   redundancy_per_interior_column -- --ignored --exact --nocapture --test-threads=1
//! ```
//!
//! `LODESTONE_DENSITY_PROBE_COLUMNS` accepts `1..=100` and defaults to 100.

use lodestone_worldgen::density::Density;
use lodestone_worldgen::engine::redundancy_probe as probe;

const SEED: i64 = 42;
const SIDE: i32 = 12;
const DEFAULT_COLUMNS: usize = 100;
const TARGET_HALO_RADIUS: i32 = 2;

fn probe_column_count() -> usize {
    let requested = match std::env::var("LODESTONE_DENSITY_PROBE_COLUMNS") {
        Ok(value) => value.parse::<usize>().unwrap_or_else(|error| {
            panic!("LODESTONE_DENSITY_PROBE_COLUMNS must be an integer: {error}")
        }),
        Err(std::env::VarError::NotPresent) => DEFAULT_COLUMNS,
        Err(error) => panic!("reading LODESTONE_DENSITY_PROBE_COLUMNS: {error}"),
    };
    assert!(
        (1..=DEFAULT_COLUMNS).contains(&requested),
        "LODESTONE_DENSITY_PROBE_COLUMNS must be in 1..={DEFAULT_COLUMNS}, got {requested}"
    );
    requested
}

fn contiguous_interior_targets(columns: usize) -> Vec<(i32, i32)> {
    (1..SIDE - 1)
        .flat_map(|cz| (1..SIDE - 1).map(move |cx| (cx, cz)))
        .take(columns)
        .collect()
}

#[test]
#[ignore = "measurement probe; needs --features gen-counters and is driven by hand"]
fn redundancy_per_interior_column() {
    let requested_columns = probe_column_count();
    let targets = contiguous_interior_targets(requested_columns);
    assert_eq!(targets.len(), requested_columns, "target lattice is too small");

    let generator = lodestone_server::overworld_generator(SEED);

    // Prepare the production five-by-five pre-ore halo while excluding every
    // submitted target. The first stream element is cold; later elements can
    // become ready through that stream's earlier column closures.
    let mut halo = Vec::new();
    for &(cx, cz) in &targets {
        for dz in -TARGET_HALO_RADIUS..=TARGET_HALO_RADIUS {
            for dx in -TARGET_HALO_RADIUS..=TARGET_HALO_RADIUS {
                let position = (cx + dx, cz + dz);
                if !targets.contains(&position) && !halo.contains(&position) {
                    halo.push(position);
                }
            }
        }
    }
    probe::reset();
    probe::disable();
    let warmed = generator.prepare_pre_ore_targets_with_radius(&halo, 0);
    assert_eq!(warmed, halo.len(), "warm-up skipped a halo prefix");
    assert!(
        halo.iter().all(|position| !targets.contains(position)),
        "warm-up must not prepare a measured target"
    );

    probe::reset();
    probe::enable();

    lodestone_worldgen::density::xz_memo::reset_stats();
    lodestone_worldgen::engine::reset_leaf_memo_stats();
    for &(cx, cz) in &targets {
        let column = generator.column(cx, cz);
        std::hint::black_box(column.non_air_count());
    }
    probe::disable();
    let agg = probe::snapshot();
    let (memo_hits, memo_misses) = lodestone_worldgen::density::xz_memo::stats();
    let (leaf_hits, leaf_misses) = lodestone_worldgen::engine::leaf_memo_stats();

    assert!(
        agg.point_total() > 0
            || agg.field_total() > 0
            || agg.compiled_point_scalar_total() > 0
            || agg.compiled_point_batch_total() > 0,
        "the probe recorded nothing — this build has no `gen-counters` feature, so \
         every number below would be a vacuous zero"
    );

    let n = requested_columns as f64;
    println!(
        "\n== redundancy over one {requested_columns}-target contiguous stream of a {SIDE}x{SIDE} lattice, seed {SEED} =="
    );
    println!(
        "  pre-ore halo: radius {TARGET_HALO_RADIUS}, target prefixes excluded before the stream; later targets can warm through earlier closures"
    );
    let memo_total = memo_hits + memo_misses;
    if memo_total > 0 {
        println!(
            "  xz_memo: {:>10.0} lookups/submitted target, hit rate {:>6.2}%  ({memo_hits} hits, {memo_misses} misses)",
            memo_total as f64 / n,
            100.0 * memo_hits as f64 / memo_total as f64,
        );
    } else {
        println!("  xz_memo: no lookups — no node carries a memo id (the memo is an island)");
    }
    let leaf_total = leaf_hits + leaf_misses;
    if leaf_total > 0 {
        println!(
            "  leaf_memo: {:>9.0} lookups/submitted target, hit rate {:>6.2}%  ({leaf_hits} hits, {leaf_misses} misses)",
            leaf_total as f64 / n,
            100.0 * leaf_hits as f64 / leaf_total as f64,
        );
    } else {
        println!("  leaf_memo: no lookups — the field evaluator's leaf memo is an island");
    }
    println!(
        "  point-interpreter visits/submitted target : {:>12.0}",
        agg.point_total() as f64 / n
    );
    println!(
        "  field-evaluator  visits/submitted target  : {:>12.0}",
        agg.field_total() as f64 / n
    );
    println!(
        "  compiled point scalar visits/submitted target : {:>9.0}",
        agg.compiled_point_scalar_total() as f64 / n
    );
    println!(
        "  compiled point batch  visits/submitted target : {:>9.0}",
        agg.compiled_point_batch_total() as f64 / n
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
        println!(
            "\n  {label} — stream totals normalized per submitted target and hit rates of three hypothetical memos"
        );
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
            "kind", "visits/target", "1slot%", "mapxz%", "mapxyz%"
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
    println!(
        "    field own-sampler counters span one Scratch lifetime, not one Field::eval call"
    );
    let report_compiled_point = |label: &str,
                                 visits: &[u64; Density::KIND_COUNT],
                                 exact_scope_hits: &[u64; Density::KIND_COUNT],
                                 scopes: u64| {
        let total_visits: u64 = visits.iter().sum();
        if total_visits == 0 {
            println!("\n  {label}: no visits");
            return;
        }
        let total_hits: u64 = exact_scope_hits.iter().sum();
        println!(
            "\n  {label} — exact (graph, node, x, y, z) repeats within one evaluation"
        );
        println!(
            "    visits {:>12.0}   scopes {:>9.0}   readiness candidates {:>6.2}%",
            total_visits as f64 / n,
            scopes as f64 / n,
            100.0 * total_hits as f64 / total_visits as f64,
        );
        let mut rows: Vec<(usize, u64)> = visits.iter().copied().enumerate().collect();
        rows.sort_by(|a, b| exact_scope_hits[b.0].cmp(&exact_scope_hits[a.0]));
        println!("    {:<18} {:>12} {:>12} {:>12}", "kind", "visits/target", "hits/target", "hits%");
        for (kind, visits) in rows.into_iter().filter(|&(_, visits)| visits > 0) {
            println!(
                "    {:<18} {:>12.0} {:>12.0} {:>11.2}%",
                Density::KIND_NAMES[kind],
                visits as f64 / n,
                exact_scope_hits[kind] as f64 / n,
                100.0 * exact_scope_hits[kind] as f64 / visits as f64,
            );
        }
    };
    report_compiled_point(
        "COMPILED POINT SCALAR",
        &agg.compiled_point_scalar_visits,
        &agg.compiled_point_scalar_xyz_scope_hits,
        agg.compiled_point_scalar_scopes,
    );
    report_compiled_point(
        "COMPILED POINT BATCH",
        &agg.compiled_point_batch_visits,
        &agg.compiled_point_batch_xyz_scope_hits,
        agg.compiled_point_batch_scopes,
    );

    // The cross-sampler split. A `Scratch` is per-sampler, so of the `map(x,y,z)`
    // duplication above only the part that is *not* also duplicated within one
    // sampler is reachable by sharing a scratch — everything else a per-sampler
    // memo already answers (or already refuses). Printing the unscoped rate alone
    // is what makes the field evaluator's 46.9% `flat_cache` row unreadable.
    println!(
        "\n  CROSS-SAMPLER SPLIT (field evaluator) — {:.1} distinct samplers/submitted target",
        agg.field_scopes as f64 / n
    );
    println!(
        "    {:<18} {:>12} {:>12} {:>12} {:>12} {:>12}",
        "kind", "visits/target", "dup(x,y,z)", "own-sampler", "cross-sampler", "1slot(xyz)"
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
        "TOTAL cross/target", "", "", "", cross_total as f64 / n
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
            "    {:<18} visits/target {:>10.0}   1slot(x,y,z) {:>6.2}%",
            Density::KIND_NAMES[kind],
            v as f64 / n,
            100.0 * s1 as f64 / v as f64,
        );
    }

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
/// [`OverworldGenerator::column_timed`] runs the identical live stages `column`
/// does — `benches/generation.rs`'s own block-for-block anti-drift control is what
/// makes that claim checkable — and reports a `Duration` per stage. Density
/// evaluation lives in `aquifer` (building the three samplers, plus
/// `max_preliminary_surface_level`) and `shape` (98,304 `AquiferSystem::block_at`
/// calls, i.e. every `Field::eval` in the column bar the carvers'), so
/// `aquifer + shape` is an **upper bound** on the density engine's share.
///
/// **The warm-up is load-bearing and the first version of this probe did not have
/// it.** `column_timed`'s `vegetation` bucket times unified FEATURES, which reads
/// the 5×5 terrain rim — and on a cold store that *computes* those
/// neighbours, so their entire terrain-prefix pipelines land in the `vegetation`
/// row. Measured both ways: cold store reports vegetation **51.6%** and
/// `aquifer + shape` 14.7%, which invites exactly the wrong conclusion, because
/// most of that 51.6% is other chunks' `shape`. Sweeping with `column()` first
/// leaves every neighbour's terrain prefix in the store, so each row times only its own
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
/// The sweep's terrain-prefix and FEATURES entries suggest weighting the prefix
/// by the wider dependency ratio — those are sweep *averages*, dominated by the leading
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
