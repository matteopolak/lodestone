//! Unit 19's acceptance criterion at the **served column** level: no decoration
//! medium on a warm serve path allocates its containers, so whatever allocations
//! remain are attributable to something that is not this unit's medium.
//!
//! # Why this exists next to `vegetation_allocs.rs`
//!
//! `vegetation_allocs.rs` measures one 3×3 `VEGETAL_DECORATION` *pass* against a
//! hand-seeded fixture scene. That is the right subject for the placement engine,
//! and it reads **0** allocations warm. But the number the rewrite plan ratchets is
//! `steady_state_heap_allocs_per_column` — a whole served column through
//! `OverworldGenerator::column`, with the store warm, the ore stage running, and
//! the returned chunk's own output buffers included. Those are different subjects,
//! and only the second one exercises `RegionView`'s overlay at all (the ore
//! driver's medium; the pass-level gate never constructs one).
//!
//! The plan's own column figure lives in `benches/generation.rs`, which is a bench
//! and not a gate. So a warm column can regress with every test in the tree green.
//! This binary closes that specifically, and narrowly.
//!
//! # What it asserts, and the one thing it deliberately does not
//!
//! It does **not** assert a total. A warm column legitimately allocates for its
//! *output* — `docs/plans/worldgen-rewrite.md` allows O(1) buffers for the returned
//! column, measured at 41 for the palette/blocks pair, plus the private copy of the
//! centre's post-ore grid that `overworld/decorate.rs` names as the one copy the
//! vegetation stage still makes. Pinning a total here would make this gate fail on
//! any unrelated unit's output-side change, which is how a gate becomes something
//! people delete.
//!
//! What it asserts is **attribution**: `feature::region_view::scratch_misses()` is
//! zero across a warm column, so not one of the three containers Unit 19 pooled
//! (`RegionView`'s overlay, `VegGrid`'s overlay, `VegGrid`'s write log) allocated.
//! A residual total therefore cannot be blamed on, or hidden inside, this unit's
//! medium — the next unit to look at the column figure gets a measured starting
//! point instead of a suspect list.
//!
//! Measured at the landing commit, seed 42, release, `--features gen-counters`:
//! 64 allocations per warm column (41 intern + 16 other + 7 vegetation), against
//! 87 before this unit (41 + 16 + 30). `scratch_misses` reads 0 for every warm
//! column and the whole of the 23-allocation reduction is the three containers.

// Same exemption and same reason as `tests/vegetation_allocs.rs`,
// `tests/engine_clone_allocs.rs` and `benches/generation.rs`: there is no safe way
// to observe real allocation counts, and an allocation claim asserted from
// structure rather than measured is exactly the kind this repo has had to retract.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use lodestone_worldgen::feature::region_view::{
    drain_scratch_free_lists, reset_scratch_misses, scratch_misses,
};

thread_local! {
    /// `const`-initialised so touching it never allocates and so cannot recurse
    /// through the allocator that is touching it.
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static ON: Cell<bool> = const { Cell::new(false) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with`, not `with`: an allocation during thread teardown happens
        // after TLS destruction, and a panic from inside the allocator is not
        // recoverable.
        let _ = ON.try_with(|on| {
            if on.get() {
                let _ = ALLOCS.try_with(|c| c.set(c.get().wrapping_add(1)));
            }
        });
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
static A: Counting = Counting;

fn allocs_of<T>(f: impl FnOnce() -> T) -> (T, u64) {
    ALLOCS.set(0);
    ON.set(true);
    let out = f();
    ON.set(false);
    (out, ALLOCS.get())
}

const SEED: i64 = 42;

/// The acceptance criterion, plus its control, in one test: both arms need one
/// process and one thread, because the free-list and the miss counter are both
/// thread-local (`docs/worldgen-staged-store.md` records the measurement that
/// forced that rule — a counter gate sharing a binary read 502 against a true 256).
#[test]
#[ignore = "serves real production columns; minutes in the dev profile — run with --release"]
fn a_warm_served_column_takes_every_decoration_container_from_the_free_list() {
    let generator = lodestone_server::overworld_generator(SEED);

    // Warm the store the way a sweep does. Only an interior column has all nine of
    // its ore and vegetation sources really computed, so a column measured without
    // this would be paying for its neighbours inside the measured window and would
    // not be the steady state at all.
    for cz in -2..=2 {
        for cx in -2..=2 {
            let _ = generator.column(cx, cz);
        }
    }

    // ---- Arm A: the claim. A warm interior column. ---------------------------
    // Four distinct interior columns, not one: a single column could take its
    // containers from the free-list by luck of ordering, and the invariant is
    // meant to hold for every column a thread serves in sequence.
    let mut samples: Vec<(i32, i32, u64, u64)> = Vec::new();
    for (cx, cz) in [(0, 0), (1, 0), (0, 1), (-1, -1)] {
        reset_scratch_misses();
        let (column, allocs) = allocs_of(|| generator.column(cx, cz));
        let misses = scratch_misses();
        assert!(
            column.non_air_count() > 0,
            "column ({cx}, {cz}) served nothing but air, so it exercised no \
             decoration medium and this measurement is premise-false"
        );
        samples.push((cx, cz, allocs, misses));
    }

    for &(cx, cz, allocs, misses) in &samples {
        assert_eq!(
            misses, 0,
            "serving warm column ({cx}, {cz}) built {misses} decoration container(s) \
             from scratch instead of taking them from this thread's free-list. Unit \
             19's whole claim is that the ore overlay, the vegetation overlay and the \
             vegetation write log are recycled across columns; a non-zero here means \
             one of them is escaping — check that its `Drop` runs (a `mem::forget`, a \
             leaked `Box`, or a container moved out of the struct would all do it) and \
             that the medium is dropped before the next column is built. Total \
             allocations for this column: {allocs}."
        );
    }

    // A total is reported but not gated — see the module doc. It must be non-zero,
    // though: a warm column returns a fresh palette and blocks buffer, so zero here
    // would mean the instrument is not live rather than that the column is free.
    let total: u64 = samples.iter().map(|&(_, _, a, _)| a).sum();
    assert!(
        total > 0,
        "four warm columns allocated nothing at all. The counting allocator is not \
         observing this thread — every assertion above is vacuous."
    );

    // ---- Arm B: the control. Drain the free-list, serve again. ---------------
    // Same binary, same generator, same coordinate, one variable. Without this,
    // `misses == 0` is satisfied just as well by a counter that never increments,
    // which is the exact shape of the vacuous guard `DESIGN.md` §12.104 records.
    drain_scratch_free_lists();
    reset_scratch_misses();
    let (control_column, control_allocs) = allocs_of(|| generator.column(2, 2));
    let control_misses = scratch_misses();
    assert!(control_column.non_air_count() > 0);
    assert!(
        control_misses > 0,
        "the free-list was drained and the very next served column still reported \
         zero container takes from empty. The miss counter is therefore inert, and \
         the zeros asserted above say nothing."
    );

    println!(
        "U19 warm column: misses=0 at every one of {} interior columns \
         (allocations {:?}); control after draining the free-list: {control_misses} \
         misses, {control_allocs} allocations",
        samples.len(),
        samples.iter().map(|&(_, _, a, _)| a).collect::<Vec<_>>(),
    );
}
