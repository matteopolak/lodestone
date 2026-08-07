//! **Attribution instrument for U18**: what, exactly, the ore path's remaining
//! heap allocations are — by call site, with shares — rather than by hypothesis.
//!
//! # Why this exists as its own binary
//!
//! U15 took ore-path allocations from 2,989,074 to 2,228,910 and its own doc
//! ends by naming `in_tag` as "the next lookup on this path". That is a *CPU*
//! lead (two string hashes per target test) and it is **not** an allocation at
//! all: `in_tag` is `HashMap<String, HashSet<String>>::get(&str)` plus
//! `HashSet<String>::contains(&str)`, both of which borrow. So the 2.2M had to
//! be attributed before anything was changed, and `benches/generation.rs`'s
//! counting allocator bins by [`Stage`] only — it can say *ore* but not *where
//! in ore*.
//!
//! This binary installs a `#[global_allocator]` that, while armed, walks a
//! backtrace per allocation and aggregates by the innermost
//! `lodestone_worldgen` frames. It is a diagnostic, so it is `#[ignore]`d: a
//! backtrace per allocation costs ~20 µs and the scene makes ~10^5 of them.
//! The *gate* that keeps the win is `ore_allocs.rs`; this file is how the
//! target was chosen.
//!
//! # How to run it
//!
//! ```text
//! cargo test --release -p lodestone-worldgen --features gen-counters \
//!     --test ore_alloc_attribution -- --ignored --nocapture
//! ```
//!
//! `--features gen-counters` is required, not optional: without it
//! [`counters::current_stage`] is a constant `Stage::Other` and every row is
//! attributed to one bucket, which reads as a working instrument reporting a
//! surprising answer. `attribution_requires_counters` fails loudly instead.

// The counting allocator needs `unsafe impl GlobalAlloc`, and the workspace sets
// `unsafe_code = "deny"`. Same exemption and same reason as
// `tests/vegetation_allocs.rs`, `tests/engine_clone_allocs.rs` and
// `benches/generation.rs`: there is no safe way to observe real allocation
// counts, and an allocation claim asserted from structure rather than measured is
// exactly the kind this repo has had to retract.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use lodestone_worldgen_core::counters::{self, STAGE_NAMES, STAGE_COUNT, Stage};

thread_local! {
    /// Armed only around the work under measurement, so the harness's own
    /// setup (which is large — the embedded generator parses every worldgen
    /// document) is never attributed to a stage.
    static ON: Cell<bool> = const { Cell::new(false) };
    /// Re-entrancy guard. Capturing a backtrace allocates, and so does the
    /// aggregation map; both run *inside* `alloc`. Without this the first
    /// counted allocation recurses until the stack dies.
    static BUSY: Cell<bool> = const { Cell::new(false) };
    /// Total counted allocations, split by the stage the thread was in.
    static BY_STAGE: [Cell<u64>; STAGE_COUNT] =
        [const { Cell::new(0) }; STAGE_COUNT];
    /// Whether to pay for a backtrace. Off for the cheap total-only arm.
    static CAPTURE: Cell<bool> = const { Cell::new(false) };
    /// Backtrace capture is **sampled**, 1 in [`SAMPLE_EVERY`]. Symbolising a
    /// backtrace against this binary's debuginfo costs milliseconds, not
    /// microseconds — capturing every allocation ran for over eight minutes
    /// without finishing. The per-stage totals and the size histogram below are
    /// **not** sampled, so the sampling affects only the site table's shares,
    /// where a few thousand samples pins a 40%-scale share to well under a
    /// point.
    static SAMPLE_TICK: Cell<u64> = const { Cell::new(0) };
    /// Number of sampled backtraces actually taken, so a share can be reported
    /// against its own denominator rather than against the unsampled total.
    static SAMPLED: Cell<u64> = const { Cell::new(0) };
    /// `site signature -> (count, stage)`. `RefCell<HashMap>` allocates, which
    /// is why every touch of it is under [`BUSY`].
    static SITES: RefCell<HashMap<(usize, String), u64>> =
        RefCell::new(HashMap::new());
    /// Allocation *sizes*, as a cheap independent cross-check on the backtrace
    /// attribution: `size -> count`, restricted to the stage under study.
    static SIZES: RefCell<HashMap<usize, u64>> = RefCell::new(HashMap::new());
    /// The stage the size histogram is restricted to. Overridable with
    /// `LODESTONE_ALLOC_SIZE_STAGE=<stage name>` so the cheap, **unsampled**
    /// cross-check can be pointed at whichever stage the site table says
    /// matters, rather than only at the one this file was named after.
    static SIZE_STAGE: Cell<usize> = Cell::new(size_stage_from_env());
}

/// Sample one backtrace in this many counted allocations. See [`SAMPLE_TICK`].
const SAMPLE_EVERY: u64 = 64;

/// Which stage the unsampled size histogram covers, from
/// `LODESTONE_ALLOC_SIZE_STAGE` (a [`STAGE_NAMES`] entry), defaulting to `ore`.
fn size_stage_from_env() -> usize {
    match std::env::var("LODESTONE_ALLOC_SIZE_STAGE") {
        Ok(name) => STAGE_NAMES
            .iter()
            .position(|&s| s == name)
            .unwrap_or_else(|| panic!("LODESTONE_ALLOC_SIZE_STAGE={name} is not one of {STAGE_NAMES:?}")),
        Err(_) => Stage::Ore as usize,
    }
}

struct Attributing;

unsafe impl GlobalAlloc for Attributing {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with` throughout: an allocation during thread teardown happens
        // after TLS destruction, and a panic from inside the allocator is not
        // recoverable. No measurement can be in flight then anyway.
        let armed = ON.try_with(Cell::get).unwrap_or(false);
        let busy = BUSY.try_with(Cell::get).unwrap_or(true);
        if armed && !busy {
            let _ = BUSY.try_with(|b| b.set(true));
            let stage = counters::current_stage() as usize;
            let _ = BY_STAGE.try_with(|bins| {
                if let Some(c) = bins.get(stage) {
                    c.set(c.get().wrapping_add(1));
                }
            });
            let size_stage = SIZE_STAGE.try_with(Cell::get).unwrap_or(usize::MAX);
            if stage == size_stage {
                let _ = SIZES.try_with(|m| {
                    *m.borrow_mut().entry(layout.size()).or_insert(0) += 1;
                });
            }
            if CAPTURE.try_with(Cell::get).unwrap_or(false) {
                let tick = SAMPLE_TICK.try_with(|t| {
                    let n = t.get().wrapping_add(1);
                    t.set(n);
                    n
                });
                if tick.is_ok_and(|n| n % SAMPLE_EVERY == 0) {
                    let _ = SAMPLED.try_with(|c| c.set(c.get().wrapping_add(1)));
                    let sig = site_signature();
                    let _ = SITES.try_with(|m| {
                        *m.borrow_mut().entry((stage, sig)).or_insert(0) += 1;
                    });
                }
            }
            let _ = BUSY.try_with(|b| b.set(false));
        }
        // SAFETY: `layout` is forwarded unchanged to the system allocator, which
        // upholds `GlobalAlloc`'s contract for it.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` came from `Self::alloc`, i.e. from `System`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: Attributing = Attributing;

/// The innermost few `lodestone_worldgen` frames, joined innermost-first.
///
/// A single frame is not enough to act on: `Vec::reserve` under
/// `Placement::get_positions` and under `RegionView`'s overlay are the same leaf
/// and completely different fixes. Three frames separates every candidate this
/// unit had to tell apart.
fn site_signature() -> String {
    let bt = std::backtrace::Backtrace::force_capture();
    let text = format!("{bt}");
    let mut frames: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // `Backtrace`'s Display puts the symbol after the frame index, as
        // `N: some::path::symbol`. Take the path half.
        let Some((_, sym)) = line.split_once(": ") else {
            continue;
        };
        if !sym.contains("lodestone_worldgen") {
            continue;
        }
        // Trim the hash suffix rustc appends and the generic noise, so the same
        // site aggregates into one row.
        let sym = sym.split("::h").next().unwrap_or(sym);
        let short = sym
            .rsplit_once("lodestone_worldgen")
            .map_or(sym, |(_, rest)| rest)
            .trim_start_matches("::");
        if frames.last() == Some(&short) {
            continue;
        }
        frames.push(short);
        if frames.len() == 3 {
            break;
        }
    }
    if frames.is_empty() {
        "<no lodestone_worldgen frame>".to_string()
    } else {
        frames.join(" <- ")
    }
}

/// Runs `f` armed, returning its value plus the per-stage counts.
fn armed<T>(capture: bool, f: impl FnOnce() -> T) -> (T, [u64; STAGE_COUNT]) {
    BY_STAGE.with(|bins| {
        for c in bins {
            c.set(0);
        }
    });
    SITES.with(|m| m.borrow_mut().clear());
    SIZES.with(|m| m.borrow_mut().clear());
    SAMPLE_TICK.set(0);
    SAMPLED.set(0);
    CAPTURE.set(capture);
    ON.set(true);
    let out = f();
    ON.set(false);
    CAPTURE.set(false);
    let counts = BY_STAGE.with(|bins| std::array::from_fn(|i| bins[i].get()));
    (out, counts)
}

/// Without `gen-counters` every allocation lands in `Stage::Other` and the whole
/// table is one row — an instrument that looks like it works. Fail instead.
#[test]
fn attribution_requires_counters() {
    assert!(
        counters::enabled(),
        "ore_alloc_attribution is meaningless without --features gen-counters: \
         `current_stage()` is a constant `Stage::Other`, so every allocation \
         would be attributed to one bucket and the table would read as a \
         working instrument reporting a surprising answer."
    );
}

/// Print the ore stage's allocation sites, ranked, over a sweep whose ore passes
/// really do run.
///
/// # The scene
///
/// A `SIDE × SIDE` sweep on the embedded production generator. `Stage::Ore` runs
/// once per chunk of the `(SIDE + 2)²` post-ore closure — the bench asserts that
/// exact identity — so the per-ore-pass figure below is a division by a count
/// this pipeline gates elsewhere, not an estimate.
#[test]
#[ignore = "diagnostic: ~20 µs of backtrace per allocation; run explicitly"]
fn where_the_ore_stages_allocations_come_from() {
    const SEED: i64 = 42;
    const SIDE: i32 = 3;

    let generator = lodestone_server::overworld_generator(SEED);
    // Warm the store so neighbours' pre-ore stages are not attributed here.
    for cz in -1..=1 {
        for cx in -1..=1 {
            std::hint::black_box(generator.column(cx, cz).non_air_count());
        }
    }

    let coords: Vec<(i32, i32)> = (0..SIDE)
        .flat_map(|cz| (0..SIDE).map(move |cx| (cx + 40, cz + 40)))
        .collect();

    counters::reset();
    let (_, by_stage) = armed(true, || {
        let mut acc = 0usize;
        for &(cx, cz) in &coords {
            acc += generator.column(cx, cz).non_air_count();
        }
        acc
    });
    let snap = counters::snapshot();

    let total: u64 = by_stage.iter().sum();
    let ore = by_stage[Stage::Ore as usize];
    let ore_passes = snap.stage_entered[Stage::Ore as usize];

    println!("\n=== U18 ore allocation attribution ===");
    println!("  scene: seed={SEED}, {SIDE}x{SIDE} sweep, embedded production data");
    println!("  total counted allocations : {total}");
    println!("  ore-stage allocations     : {ore}  ({:.2}%)", pct(ore, total));
    println!("  ore stage entered         : {ore_passes} times");
    if ore_passes > 0 {
        println!("  per ore pass              : {}", ore / ore_passes);
    }
    println!("  rng_draws[Ore]            : {}", snap.rng_draws[Stage::Ore as usize]);

    println!("\n  -- allocations by stage --");
    let mut ranked: Vec<(usize, u64)> = by_stage.iter().copied().enumerate().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    for (stage, n) in &ranked {
        if *n == 0 {
            continue;
        }
        println!("     {:<14} {n:>10}  ({:>5.2}%)", STAGE_NAMES[*stage], pct(*n, total));
    }

    let sites = SITES.with(|m| {
        let mut v: Vec<((usize, String), u64)> =
            m.borrow().iter().map(|(k, &n)| (k.clone(), n)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    });
    // Shares are against the number of samples taken *in this stage*, not
    // against the unsampled ore total — dividing a sampled numerator by an
    // unsampled denominator would report every share as ~1/64 of the truth,
    // which is the kind of arithmetic that reads as a surprising measurement.
    // Every stage that allocated at all gets its own site table. Printing only
    // the ore stage is how the first run of this instrument nearly shipped a
    // conclusion about the wrong stage: ore is 5% of this scene's allocations
    // and `surface` is 92%, and a report scoped to ore cannot show that.
    for (stage, n) in ranked.iter().filter(|(_, n)| *n > 0) {
        let stage_samples: u64 = sites
            .iter()
            .filter(|((s, _), _)| s == stage)
            .map(|(_, c)| *c)
            .sum();
        if stage_samples == 0 {
            continue;
        }
        println!(
            "\n  -- {}-stage allocation sites (innermost 3 worldgen frames) --\n\
               \x20    sampled 1 in {SAMPLE_EVERY}: {stage_samples} samples over {n} allocations",
            STAGE_NAMES[*stage].to_uppercase()
        );
        for ((_, sig), c) in sites.iter().filter(|((s, _), _)| s == stage).take(10) {
            println!(
                "     {c:>7} samples  ({:>5.2}% of stage)  {sig}",
                pct(*c, stage_samples)
            );
        }
    }

    // The size histogram is UNSAMPLED, so it is an independent check on the
    // sampled site table rather than a restatement of it: a site's byte size is
    // derivable from the source (`BlockPos` is 12 bytes; `do_place`'s `data` is
    // `size * 4 * 8`), so agreement between a share-by-symbol and a
    // share-by-size is two instruments, not one.
    let size_stage = SIZE_STAGE.get();
    let size_stage_total = by_stage[size_stage];
    println!(
        "\n  -- {}-stage allocation SIZES, unsampled (independent cross-check) --",
        STAGE_NAMES[size_stage].to_uppercase()
    );
    let sizes = SIZES.with(|m| {
        let mut v: Vec<(usize, u64)> = m.borrow().iter().map(|(&s, &n)| (s, n)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    });
    for (size, n) in sizes.iter().take(20) {
        println!(
            "     {n:>9}  ({:>5.2}% of stage)  {size} bytes",
            pct(*n, size_stage_total)
        );
    }

    assert!(ore > 0, "the ore stage allocated nothing — scene or arming is wrong");
    assert!(ore_passes > 0, "the ore stage never ran; this is the wrong scene");
}

fn pct(n: u64, d: u64) -> f64 {
    if d == 0 { 0.0 } else { 100.0 * n as f64 / d as f64 }
}
