//! Stage 0 of `docs/plans/progressive-chunk-generation.md`: a go/no-go
//! measurement, not a build. Nothing here is production code.
//!
//! # What it is
//!
//! The plan's existing cost figures are mutually inconsistent by ~27x and
//! straddle the worldgen rewrite (`docs/plans/progressive-chunk-generation.md`'s
//! own "What exists today" section). This file re-measures, on the real
//! embedded production worldgen data (`lodestone_server::overworld_generator`,
//! not the in-crate fixture tree — see `profile_columns_report.rs`'s module
//! doc for why a fixture-tree generator is the "world" species of vacuous
//! benchmark here), three things:
//!
//! 1. Per-stage cost, cold, over three census-verified terrains (forest,
//!    mountains, ocean) — `stage0_per_stage_shaped_vs_full_cost`. This produces
//!    the headline **shaped/full ratio** the plan's go/no-go threshold reads.
//! 2. Upgrade cost, warm (the staged store still holds `pre_ore`) vs evicted
//!    (a fresh generator, i.e. regenerate from scratch) —
//!    `stage0_upgrade_cost_warm_vs_evicted`, plus a counter-backed control that
//!    the "warm" arm really did hit the memo rather than silently recomputing.
//! 3. Store pressure — `store_len()`/`store_evictions()` and RSS growth — over
//!    a full raster sweep at several render distances, including rd 32 and 64
//!    — `stage0_store_pressure_rd*`.
//!
//! # What this does NOT measure, and why
//!
//! There is no public `column_shaped`-style seam yet (that is Stage 1's job,
//! not this one's), so nothing here can generate a *true* Shaped column and
//! measure its real packed byte size or its real (smaller) intern cost. Two
//! consequences, both deliberate rather than oversights:
//!
//! * "Shaped" cost is reconstructed from `OverworldGenerator::column_timed`'s
//!   existing ten-bucket breakdown by summing the buckets that stage 0a-4
//!   already are: `aquifer + shape + biome + surface + materialize + carve`.
//!   `structure_place_stage` runs *inside* the `carve` bucket (see
//!   `column_timed`'s own body), so this sum really is "structures, fill,
//!   surface, carve" with nothing missing and nothing borrowed from `ore`/
//!   `vegetation`/`top_layer`.
//! * The `intern` bucket is charged identically to both the shaped and the
//!   full total (see `shaped_serve_us`/`full_us` below), because the only
//!   measured intern cost is interning a *full* dense grid (more distinct
//!   block states than a shaped one would have: ore, leaves, logs, saplings).
//!   A real Shaped column would likely intern for less, not more, so charging
//!   it the full column's intern cost is a conservative bias — it can only
//!   make the shaped/full ratio look *worse* than reality, never better. This
//!   is called out again at the print site, not just here.
//! * Store-pressure numbers are today's (all-Full) behaviour, not a simulated
//!   band, because there is nothing to send at a reduced stage yet. They are
//!   the pre-Stage-1 baseline the plan's own "at scale" risk item (its
//!   `STORE_RETENTION` note) asks for.
//!
//! # How it works
//!
//! Terrain selection is a two-step census, not a hardcoded chunk coordinate
//! table: [`find_columns`] does a cheap scan with
//! `OverworldGenerator::biome_at_quart` (climate + table lookup, no dense
//! grid), then every column actually used for a timed measurement is
//! cross-checked against its own real, materialized `biome_state` after a
//! full `column()` call — [`census_cross_check_refuses_a_mismatched_biome`]
//! is the executed control proving that cross-check can fail, not merely
//! pass by construction (`#[should_panic]`, per this repo's own rule that a
//! hand-rolled `catch_unwind` in a test is unreliable under Cranelift).
//!
//! Every terrain's timed measurement runs on its own **fresh** generator
//! (`overworld_generator(SEED)` called again), so no terrain's structure-start
//! closure or biome cache can warm a later terrain's numbers — the same
//! "cache-cold, on purpose" property `column_timed`'s own module doc
//! describes, taken one step further across terrains rather than just across
//! stages.
//!
//! # How to change it
//!
//! Add a terrain by adding a `(name, predicate)` pair to the `terrains` array
//! in `stage0_per_stage_shaped_vs_full_cost` and a matching `fn is_*` predicate
//! near [`is_forest`]. Add a store-pressure radius by adding another
//! `#[ignore]`d `stage0_store_pressure_rd*` wrapper around
//! [`sweep_store_pressure`] — do not inline a new radius into an existing test,
//! since each one is deliberately its own `#[ignore]`d, individually-runnable
//! unit (a 129x129 sweep at rd 64 takes minutes; nothing should force it to run
//! alongside a cheap one).
//!
//! # Configuration
//!
//! `SEED` is fixed at 42 to match the seed most other `lodestone-worldgen`
//! integration tests already use (`aquifer_parity.rs`, `surface_parity.rs`,
//! `carver_parity.rs`, `vegetation_seam_consistency.rs`, `ore_stage_profile.rs`)
//! — not load-bearing here (no oracle fixture is pinned to it), but keeping it
//! means a census coordinate found by one file's search is comparable to
//! another's. Everything else is a named constant next to its use.
//!
//! # Dependencies
//!
//! `lodestone_server::overworld_generator` (dev-dependency cycle, already
//! established by `ore_stage_profile.rs`/`chunk_memory.rs`/
//! `benches/generation.rs` — see this crate's `Cargo.toml` for why that is
//! sound), `lodestone_worldgen::profile::aggregate_stage_samples`,
//! `lodestone_worldgen::counters` (only under `--features gen-counters`), and
//! the `memory-stats` dev-dependency already used by `benches/generation.rs`'s
//! `rss_bytes` for the same OS counter `/usr/bin/time -l` reports.
//!
//! Every test in this file is `#[ignore]`d except the two fast, deterministic
//! census controls — run explicitly, in release, one at a time on an
//! otherwise-idle machine:
//!
//! ```text
//! cargo test --release -p lodestone-worldgen --test stage0_shaped_vs_full_cost \
//!     -- --ignored --test-threads=1 --nocapture <test name>
//! ```

// `proc_pid_rusage` is an `extern "C"` call and the workspace denies unsafe code.
// Scoped as narrowly as the lint allows, matching `explosion_cost_profile.rs`
// and `join_parallel_efficiency.rs`'s opt-out against the same function.
#![allow(unsafe_code)]

use lodestone_server::overworld_generator;
use lodestone_worldgen::overworld::{OverworldGenerator, StageTimes};
use lodestone_worldgen::profile::aggregate_stage_samples;

const SEED: i64 = 42;

// ===========================================================================
// Instructions-retired instrument (Darwin only) — same shape as
// `explosion_cost_profile.rs`, `join_parallel_efficiency.rs` and
// `benches/generation.rs`'s own copies. Each measurement file here keeps its
// own copy rather than sharing one, matching this repo's established
// precedent (an integration test binary is its own crate and cannot import
// from another one's `tests/` file, or from a `benches/` file at all).
// ===========================================================================

#[cfg(target_os = "macos")]
const RUSAGE_INFO_V4: i32 = 4;

/// `struct rusage_info_v4` from macOS `<sys/resource.h>`, field-by-field in
/// declaration order so `ri_instructions` is reached **by name**.
#[repr(C)]
#[derive(Default, Clone, Copy)]
#[allow(non_snake_case, dead_code)]
struct RusageInfoV4 {
    ri_uuid: [u8; 16],
    ri_user_time: u64,
    ri_system_time: u64,
    ri_pkg_idle_wkups: u64,
    ri_interrupt_wkups: u64,
    ri_pageins: u64,
    ri_wired_size: u64,
    ri_resident_size: u64,
    ri_phys_footprint: u64,
    ri_proc_start_abstime: u64,
    ri_proc_exit_abstime: u64,
    ri_child_user_time: u64,
    ri_child_system_time: u64,
    ri_child_pkg_idle_wkups: u64,
    ri_child_interrupt_wkups: u64,
    ri_child_pageins: u64,
    ri_child_elapsed_abstime: u64,
    ri_diskio_bytesread: u64,
    ri_diskio_byteswritten: u64,
    ri_cpu_time_qos_default: u64,
    ri_cpu_time_qos_maintenance: u64,
    ri_cpu_time_qos_background: u64,
    ri_cpu_time_qos_utility: u64,
    ri_cpu_time_qos_legacy: u64,
    ri_cpu_time_qos_user_initiated: u64,
    ri_cpu_time_qos_user_interactive: u64,
    ri_billed_system_time: u64,
    ri_serviced_system_time: u64,
    ri_logical_writes: u64,
    ri_lifetime_max_phys_footprint: u64,
    ri_instructions: u64,
    ri_cycles: u64,
    ri_billed_energy: u64,
    ri_serviced_energy: u64,
    ri_interval_max_phys_footprint: u64,
    ri_runnable_time: u64,
    ri_flags: u64,
}

/// What the transcription must weigh if every field is present and correctly
/// typed: a 16-byte UUID and 36 `u64`s. Derived from the field list, not
/// measured.
#[cfg(target_os = "macos")]
const RUSAGE_INFO_V4_SIZE: usize = 16 + 36 * 8;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
}

/// Non-Darwin arm. Panics rather than returning a plausible zero — see
/// `explosion_cost_profile.rs`'s identical function for why. Only the
/// `#[ignore]`d instructions-retired tests reach this.
#[cfg(not(target_os = "macos"))]
fn instructions_now() -> u64 {
    unimplemented!(
        "instructions retired is read through proc_pid_rusage(RUSAGE_INFO_V4), which exists \
         only on Darwin; this measurement has no counter to report on this target"
    )
}

#[cfg(target_os = "macos")]
fn instructions_now() -> u64 {
    assert_eq!(
        size_of::<RusageInfoV4>(),
        RUSAGE_INFO_V4_SIZE,
        "the rusage_info_v4 transcription is the wrong size, so `ri_instructions` is not the \
         field being read"
    );
    let mut info = RusageInfoV4::default();
    let rc = unsafe {
        proc_pid_rusage(
            i32::try_from(std::process::id()).expect("pid fits in i32"),
            RUSAGE_INFO_V4,
            (&raw mut info).cast::<core::ffi::c_void>(),
        )
    };
    assert_eq!(rc, 0, "proc_pid_rusage(RUSAGE_INFO_V4) failed with {rc}");
    info.ri_instructions
}

/// Runs `body` and returns instructions retired.
fn measure_instructions(body: impl FnOnce()) -> u64 {
    let before = instructions_now();
    body();
    instructions_now().saturating_sub(before)
}

/// Current process resident set in bytes — the same `memory-stats` call
/// `benches/generation.rs`'s `rss_bytes` uses, reading the same OS counter
/// `/usr/bin/time -l` reports, no subprocess and no output-parsing pipeline.
fn rss_bytes() -> u64 {
    memory_stats::memory_stats().map_or(0, |s| s.physical_mem as u64)
}

// ===========================================================================
// Terrain census
// ===========================================================================

/// `minecraft:forest`, `minecraft:birch_forest`, `minecraft:flower_forest`, …
/// but not `minecraft:windswept_forest` (mountains family — see
/// [`is_mountain`]), so the two census predicates never both match the same
/// biome.
fn is_forest(biome: &str) -> bool {
    biome.contains("forest") && !biome.contains("windswept")
}

/// `minecraft:ocean`, `minecraft:cold_ocean`, `minecraft:deep_lukewarm_ocean`, …
fn is_ocean(biome: &str) -> bool {
    biome.contains("ocean")
}

/// The peaks/windswept-hills family — real asset names confirmed against
/// `crates/lodestone-server/assets/worldgen/biome/` (`frozen_peaks.json`,
/// `jagged_peaks.json`, `stony_peaks.json`, `windswept_hills.json`,
/// `windswept_gravelly_hills.json`). Deliberately excludes
/// `windswept_forest`/`windswept_savanna`, which are forest/savanna variants,
/// not the mountain census this file wants.
fn is_mountain(biome: &str) -> bool {
    biome.contains("peaks") || biome.contains("windswept_hills") || biome.contains("windswept_gravelly")
}

/// Scans a `(2*radius+1)^2` square around the origin with the cheap
/// `biome_at_quart` lookup (climate + table lookup, no dense grid — nothing
/// like the cost of a real `column()` call) and returns up to `count` chunk
/// coordinates whose centre quart satisfies `matches`. Y is fixed at quart 15
/// (world y ~60, sea-level-ish) — overworld biome search has some y variation
/// but every terrain census here only needs *a* representative column, not an
/// exhaustive one.
fn find_columns(generator: &OverworldGenerator, matches: impl Fn(&str) -> bool, count: usize, radius: i32) -> Vec<(i32, i32)> {
    let mut found = Vec::new();
    for cz in -radius..=radius {
        for cx in -radius..=radius {
            let biome = generator.biome_at_quart(cx * 4 + 2, 15, cz * 4 + 2);
            if matches(&biome) {
                found.push((cx, cz));
                if found.len() >= count {
                    return found;
                }
            }
        }
    }
    found
}

/// The cross-check every timed sample goes through: the cheap `biome_at_quart`
/// search and the real, materialized `biome_state` after a full `column()`
/// call must agree. Panics naming the mismatch rather than returning a bool —
/// every caller wants this to fail loudly, including the control below, which
/// wants it to fail on purpose.
fn assert_census_match(terrain: &str, pred: impl Fn(&str) -> bool, cx: i32, cz: i32, biome: &str) {
    assert!(
        pred(biome),
        "STAGE0 census control failed: column ({cx},{cz}) picked for {terrain} via \
         biome_at_quart has real biome {biome:?} after full generation — the cheap climate \
         lookup and the materialized biome disagree here"
    );
}

/// Fast, deterministic control: the two forest/ocean predicates never agree
/// on real, unrelated biome names — proves [`is_forest`]/[`is_ocean`]/
/// [`is_mountain`] actually discriminate rather than being permissive
/// tautologies. No generation involved, so this is cheap enough to run
/// un-ignored.
#[test]
fn terrain_predicates_do_not_overlap() {
    assert!(!is_forest("minecraft:ocean"));
    assert!(!is_forest("minecraft:windswept_forest"));
    assert!(!is_ocean("minecraft:forest"));
    assert!(!is_ocean("minecraft:windswept_hills"));
    assert!(!is_mountain("minecraft:forest"));
    assert!(!is_mountain("minecraft:ocean"));
    assert!(!is_mountain("minecraft:windswept_forest"));
    assert!(is_forest("minecraft:forest"));
    assert!(is_ocean("minecraft:cold_ocean"));
    assert!(is_mountain("minecraft:jagged_peaks"));
    assert!(is_mountain("minecraft:windswept_hills"));
}

/// The executed control for [`assert_census_match`], per this repo's evidence
/// rule that an absence/refusal assertion needs a control proving the
/// detector actually fires, observed failing rather than merely described.
/// Deliberately checks a real ocean column against [`is_forest`].
///
/// Not `#[ignore]`d: one real `column()` call, same cost class as
/// `chunk_memory.rs`'s un-ignored tests.
#[test]
#[should_panic(expected = "STAGE0 census control failed")]
fn census_cross_check_refuses_a_mismatched_biome() {
    let search_gen = overworld_generator(SEED);
    let coords = find_columns(&search_gen, is_ocean, 1, 64);
    assert!(!coords.is_empty(), "setup: no ocean column found near origin — control cannot run");
    let (cx, cz) = coords[0];

    let verify_gen = overworld_generator(SEED);
    let column = verify_gen.column(cx, cz);
    let biome = column.biome_state(8, 8).to_string();
    assert_census_match("forest", is_forest, cx, cz, &biome); // deliberately the wrong predicate
}

// ===========================================================================
// Shaped/full split
// ===========================================================================

/// Stages 0a-4 (structures, fill, surface, carve) — `column_timed`'s first six
/// buckets. `structure_place_stage` runs inside the `carve` bucket, so this
/// really is the whole of what `docs/plans/progressive-chunk-generation.md`
/// calls `GenStage::Shaped`, nothing borrowed from `ore`/`vegetation`/
/// `top_layer`.
fn shaped_stage_us(t: &StageTimes) -> u64 {
    (t.aquifer + t.shape + t.biome + t.surface + t.materialize + t.carve).as_micros() as u64
}

/// What a Shaped column skips: ore, vegetation, top-layer freeze.
fn post_shaped_stage_us(t: &StageTimes) -> u64 {
    (t.ore + t.vegetation + t.top_layer).as_micros() as u64
}

fn intern_us(t: &StageTimes) -> u64 {
    t.intern.as_micros() as u64
}

// ===========================================================================
// Measurement 1: per-stage cost, cold, shaped vs full
// ===========================================================================

struct TerrainSample {
    name: &'static str,
    coords: Vec<(i32, i32)>,
    times: Vec<StageTimes>,
}

/// Real, per-stage, cold-generation cost over three census-verified terrains —
/// the headline shaped/full ratio the plan's go/no-go threshold reads. See
/// the module doc for what "shaped" means here and what it does not measure.
#[test]
#[ignore = "measurement, release-profile, prints STAGE0_ lines; run with \
            `cargo test --release -p lodestone-worldgen --test stage0_shaped_vs_full_cost \
            -- --ignored --test-threads=1 --nocapture stage0_per_stage_shaped_vs_full_cost`"]
fn stage0_per_stage_shaped_vs_full_cost() {
    const SAMPLES_PER_TERRAIN: usize = 4;
    const SEARCH_RADIUS: i32 = 64;

    let terrains: [(&str, fn(&str) -> bool); 3] =
        [("forest", is_forest as fn(&str) -> bool), ("mountains", is_mountain), ("ocean", is_ocean)];

    let search_gen = overworld_generator(SEED);
    let mut samples: Vec<TerrainSample> = Vec::new();

    for &(name, pred) in &terrains {
        let coords = find_columns(&search_gen, pred, SAMPLES_PER_TERRAIN, SEARCH_RADIUS);
        assert_eq!(
            coords.len(),
            SAMPLES_PER_TERRAIN,
            "STAGE0 census failure: found only {}/{SAMPLES_PER_TERRAIN} {name} columns within \
             radius {SEARCH_RADIUS} of seed {SEED} — this measurement's fixture guard; widen \
             SEARCH_RADIUS or re-derive the predicate rather than trusting a partial sample",
            coords.len()
        );

        // Cross-check every picked coordinate's real, materialized biome
        // against the cheap search predicate, on a throwaway generator so this
        // does not warm the timed generator's store below.
        let verify_gen = overworld_generator(SEED);
        for &(cx, cz) in &coords {
            let biome = verify_gen.column(cx, cz).biome_state(8, 8).to_string();
            assert_census_match(name, pred, cx, cz, &biome);
        }

        // The actual cold measurement, on its own fresh generator so no
        // terrain's structure-start closure warms another terrain's numbers.
        let terrain_gen = overworld_generator(SEED);
        let times: Vec<StageTimes> =
            coords.iter().map(|&(cx, cz)| terrain_gen.column_timed(cx, cz).1).collect();
        samples.push(TerrainSample { name, coords, times });
    }

    println!("STAGE0_HEADER profile=release-if-invoked-with---release seed={SEED}");

    let mut all_shaped_serve_us = 0u64;
    let mut all_full_us = 0u64;
    let mut all_ratios: Vec<f64> = Vec::new();

    for sample in &samples {
        let stage_samples: Vec<((i32, i32), StageTimes)> =
            sample.coords.iter().copied().zip(sample.times.iter().copied()).collect();
        let dist = aggregate_stage_samples(&stage_samples);
        for stage in &dist.per_stage {
            println!(
                "STAGE0_STAGE terrain={:<9} stage={:<11} p50_us={:>8} max_us={:>8} total_us={:>9}",
                sample.name, stage.stage, stage.p50_us, stage.max_us, stage.total_us
            );
        }

        let mut terrain_shaped_serve = 0u64;
        let mut terrain_full = 0u64;
        for t in &sample.times {
            let shaped = shaped_stage_us(t);
            let post = post_shaped_stage_us(t);
            let intern = intern_us(t);
            // See module doc: intern is charged identically to both totals,
            // which is a conservative (pro-"don't build it") bias since a real
            // Shaped column's own intern would likely be cheaper.
            let shaped_serve = shaped + intern;
            let full = shaped + post + intern;
            terrain_shaped_serve += shaped_serve;
            terrain_full += full;
            all_ratios.push(shaped_serve as f64 / full as f64);
        }
        all_shaped_serve_us += terrain_shaped_serve;
        all_full_us += terrain_full;

        println!(
            "STAGE0_TERRAIN name={:<9} columns={} shaped_serve_us_total={} full_us_total={} \
             ratio={:.4}",
            sample.name,
            sample.coords.len(),
            terrain_shaped_serve,
            terrain_full,
            terrain_shaped_serve as f64 / terrain_full as f64
        );
    }

    all_ratios.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in a duration ratio"));
    let median_ratio = all_ratios[all_ratios.len() / 2];
    let overall_ratio = all_shaped_serve_us as f64 / all_full_us as f64;

    println!(
        "STAGE0_HEADLINE columns={} shaped_serve_us_total={all_shaped_serve_us} \
         full_us_total={all_full_us} overall_ratio={overall_ratio:.4} \
         median_column_ratio={median_ratio:.4} \
         go_no_go_threshold=0.50 verdict={}",
        all_ratios.len(),
        if overall_ratio <= 0.50 { "GO (shaped <= ~50% of full)" } else { "NO-GO (shaped > ~50% of full)" }
    );

    // Control: the instrument must be able to tell terrain apart, or every
    // number above is noise wearing a report's clothes. Forest's vegetation
    // stage cost must clearly exceed ocean's.
    let veg_us_total = |name: &str| -> u64 {
        samples
            .iter()
            .find(|s| s.name == name)
            .expect("terrain present")
            .times
            .iter()
            .map(|t| t.vegetation.as_micros() as u64)
            .sum()
    };
    let forest_veg = veg_us_total("forest");
    let ocean_veg = veg_us_total("ocean");
    println!("STAGE0_CONTROL forest_vegetation_us_total={forest_veg} ocean_vegetation_us_total={ocean_veg}");
    assert!(forest_veg > 0, "STAGE0 control: forest vegetation stage cost was zero across all samples — vacuous");
    assert!(
        forest_veg > ocean_veg,
        "STAGE0 control failed: forest vegetation cost ({forest_veg}us) is not greater than \
         ocean's ({ocean_veg}us) — either the census picked the wrong columns or vegetation \
         attribution is broken"
    );

    assert!(
        overall_ratio > 0.0 && overall_ratio < 1.0,
        "STAGE0: shaped/full ratio {overall_ratio} is out of the range a strict subset of \
         stages must land in"
    );
}

// ===========================================================================
// Measurement 2: upgrade cost, warm vs evicted
// ===========================================================================

/// Primes the `pre_ore` memo slot for every chunk a subsequent `column(cx, cz)`
/// call will need — simulating "this column, and its neighbourhood, were
/// already requested at Shaped" — through the one already-public entry point
/// that touches `pre_ore` without also touching `post_ore`:
/// `ore_stage_for_profiling`, which calls `pre_ore_stage` for its own 3x3
/// (`post_ore_world`'s own doc: "this stage's computation calls
/// `Self::pre_ore_stage` ... for its own chunk and, via `Self::ore_stage`, for
/// its 3x3") and then a private, unmemoised `ore_stage` directly, never
/// `post_ore_world`.
///
/// **One call at `(cx, cz)` alone is not enough**, and this was found by the
/// counter control below, not assumed: `column`'s `vegetation_stage` reads a
/// wider 5x5 rim (`COLUMN_CLOSURE_RADIUS` = 2) than `ore_stage`'s 3x3, so a
/// single priming call left 16 of the 25 needed `pre_ore` entries cold and
/// `stage0_upgrade_warm_hits_the_pre_ore_memo_by_counter` caught it directly
/// (`pre_ore_computed` read 16, not 0, on first write of this file). Calling
/// `ore_stage_for_profiling` at the 9 points of a `{-2, 0, 2} x {-2, 0, 2}`
/// grid tiles radius-1 boxes with no gap out to radius 3 — a superset of the
/// radius-2 rim `column()` actually reads, so this over-primes slightly rather
/// than under-primes.
fn warm_pre_ore_neighborhood(generator: &OverworldGenerator, cx: i32, cz: i32) {
    for dz in [-2, 0, 2] {
        for dx in [-2, 0, 2] {
            std::hint::black_box(generator.ore_stage_for_profiling(cx + dx, cz + dz));
        }
    }
}

/// Upgrade cost when the staged store already holds every `pre_ore` entry a
/// subsequent Full request needs (a prior Shaped request, simulated by
/// [`warm_pre_ore_neighborhood`]) versus a fully evicted/never-generated
/// column (a fresh generator).
///
/// Instructions retired, not wall clock — CLAUDE.md's own rule, sharpened by
/// this exact repo's measurement that this host reproduces wall clock to
/// 11-19% at best with sibling agents compiling and instructions retired to
/// 0.16-0.21% (`explosion_cost_profile.rs`'s module doc).
#[test]
#[ignore = "measurement, Darwin-only counter, release-profile; run with \
            `cargo test --release -p lodestone-worldgen --test stage0_shaped_vs_full_cost \
            -- --ignored --test-threads=1 --nocapture stage0_upgrade_cost_warm_vs_evicted`"]
fn stage0_upgrade_cost_warm_vs_evicted() {
    let search_gen = overworld_generator(SEED);
    let coords = find_columns(&search_gen, is_forest, 1, 64);
    assert!(!coords.is_empty(), "STAGE0: no forest column found for the upgrade-cost measurement");
    let (cx, cz) = coords[0];

    let evicted_gen = overworld_generator(SEED);
    let evicted_insns = measure_instructions(|| {
        std::hint::black_box(evicted_gen.column(cx, cz).non_air_count());
    });

    let warm_gen = overworld_generator(SEED);
    warm_pre_ore_neighborhood(&warm_gen, cx, cz);
    let warm_insns = measure_instructions(|| {
        std::hint::black_box(warm_gen.column(cx, cz).non_air_count());
    });

    let ratio = warm_insns as f64 / evicted_insns as f64;
    println!(
        "STAGE0_UPGRADE column=({cx},{cz}) evicted_instructions={evicted_insns} \
         warm_instructions={warm_insns} warm_over_evicted_ratio={ratio:.4}"
    );

    assert!(
        warm_insns < evicted_insns,
        "STAGE0: warm upgrade ({warm_insns} instructions) was not cheaper than evicted \
         ({evicted_insns} instructions) — either priming did not warm the memo, or the two \
         arms are not measuring what they claim to"
    );
}

/// Counter-backed control for the test above: proves the "warm" arm really
/// did hit the `pre_ore` memo (zero recomputation) and the "evicted" arm
/// really did miss it (at least one recomputation), rather than trusting the
/// instruction-count gap to mean that on its own. Only meaningful with
/// `gen-counters` on.
#[cfg(feature = "gen-counters")]
#[test]
#[ignore = "measurement, gen-counters build; run with \
            `cargo test --release -p lodestone-worldgen --features gen-counters \
            --test stage0_shaped_vs_full_cost -- --ignored --test-threads=1 --nocapture \
            stage0_upgrade_warm_hits_the_pre_ore_memo_by_counter`"]
fn stage0_upgrade_warm_hits_the_pre_ore_memo_by_counter() {
    use lodestone_worldgen::counters;

    let search_gen = overworld_generator(SEED);
    let coords = find_columns(&search_gen, is_forest, 1, 64);
    assert!(!coords.is_empty(), "STAGE0: no forest column found for the upgrade-counter control");
    let (cx, cz) = coords[0];

    let warm_gen = overworld_generator(SEED);
    warm_pre_ore_neighborhood(&warm_gen, cx, cz);
    counters::reset();
    std::hint::black_box(warm_gen.column(cx, cz).non_air_count());
    let warm_snapshot = counters::snapshot();
    let warm_pre_ore_computed = warm_snapshot.pre_ore_computed;

    let evicted_gen = overworld_generator(SEED);
    counters::reset();
    std::hint::black_box(evicted_gen.column(cx, cz).non_air_count());
    let evicted_snapshot = counters::snapshot();
    let evicted_pre_ore_computed = evicted_snapshot.pre_ore_computed;

    // Not asserted on (structure closure width is `overworld::STRUCTURE_CLOSURE_RADIUS`'s
    // concern, not this test's), but printed because it is the single biggest
    // confound in the instruction-count comparison above: a genuinely fresh
    // generator's first `column()` call near unexplored territory pays the full
    // 21x21 structure-starts closure once, and priming through
    // `warm_pre_ore_neighborhood`'s 9 calls (each with its own wide structure
    // closure) pays most of that cost too, just outside the timed region. So the
    // warm/evicted gap in `stage0_upgrade_cost_warm_vs_evicted` reflects
    // structure-closure amortisation as much as it reflects ore/vegetation/
    // top-layer savings — a different, larger axis of saving than measurement 1's
    // per-stage split, which should not be read as the same number on a smaller
    // scale.
    println!(
        "STAGE0_UPGRADE_COUNTER warm_pre_ore_computed={warm_pre_ore_computed} \
         evicted_pre_ore_computed={evicted_pre_ore_computed} \
         warm_structure_starts_computed={} evicted_structure_starts_computed={}",
        warm_snapshot.structure_starts_computed, evicted_snapshot.structure_starts_computed
    );
    assert_eq!(
        warm_pre_ore_computed, 0,
        "the 'warm' arm recomputed pre_ore ({warm_pre_ore_computed} times) — the memo was not \
         actually warm, which invalidates the instruction-count comparison in \
         stage0_upgrade_cost_warm_vs_evicted"
    );
    assert!(
        evicted_pre_ore_computed >= 1,
        "the 'evicted' arm never computed pre_ore — the counter itself is not wired, or \
         column() stopped calling pre_ore_stage"
    );
}

// ===========================================================================
// Measurement 3: store pressure at scale
// ===========================================================================

/// Full raster sweep of a `(2*radius+1)^2` square with a single generator,
/// reporting the staged store's own diagnostics (`store_len`/
/// `store_evictions` — exact counters, not RSS) plus RSS growth from a
/// post-warm-up baseline, matching `benches/generation.rs`'s `bench_region_rss`
/// methodology (delta from a baseline taken after two warm-up columns, peak
/// sampled every column, never an absolute reading — see that function's own
/// doc for why an absolute RSS reading here would be the "duration species" of
/// vacuous test).
///
/// This is **today's (all-Full) behaviour**, not a simulated band — see the
/// module doc's "What this does NOT measure" section.
fn sweep_store_pressure(radius: i32) -> (usize, usize, u64, usize) {
    let generator = overworld_generator(SEED);
    for cx in 0..2 {
        std::hint::black_box(generator.column(cx, 0).non_air_count());
    }
    let baseline = rss_bytes();
    let mut peak = baseline;
    let mut non_air = 0usize;
    let mut columns = 0usize;
    for cz in -radius..=radius {
        for cx in -radius..=radius {
            non_air += generator.column(cx, cz).non_air_count();
            columns += 1;
            peak = peak.max(rss_bytes());
        }
    }
    assert!(non_air > 0, "STAGE0 store-pressure sweep at rd={radius} generated only air — nothing measured");
    (generator.store_len(), generator.store_evictions(), peak.saturating_sub(baseline), columns)
}

fn report_store_pressure(radius: i32) {
    let (store_len, evictions, rss_growth, columns) = sweep_store_pressure(radius);
    println!(
        "STAGE0_STORE_PRESSURE radius={radius} columns={columns} store_len={store_len} \
         evictions={evictions} rss_growth_bytes={rss_growth} \
         rss_growth_mib={:.1} rss_bytes_per_column={:.0}",
        rss_growth as f64 / (1024.0 * 1024.0),
        rss_growth as f64 / columns as f64,
    );
}

#[test]
#[ignore = "measurement, release-profile; ~289 columns"]
fn stage0_store_pressure_rd8() {
    report_store_pressure(8);
}

#[test]
#[ignore = "measurement, release-profile; ~1,089 columns"]
fn stage0_store_pressure_rd16() {
    report_store_pressure(16);
}

#[test]
#[ignore = "measurement, release-profile; ~2,401 columns"]
fn stage0_store_pressure_rd24() {
    report_store_pressure(24);
}

#[test]
#[ignore = "measurement, release-profile; ~4,225 columns — this is one of the two radii \
            docs/plans/progressive-chunk-generation.md's Stage 0 explicitly asks for"]
fn stage0_store_pressure_rd32() {
    report_store_pressure(32);
}

#[test]
#[ignore = "measurement, release-profile; ~16,641 columns, minutes — this is the other radius \
            docs/plans/progressive-chunk-generation.md's Stage 0 explicitly asks for"]
fn stage0_store_pressure_rd64() {
    report_store_pressure(64);
}
