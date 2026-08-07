//! Unit 8's acceptance criterion: a **warm** vegetal-decoration pass performs
//! **zero heap allocations** in the placement engine, and it really does run the
//! sites that claim to have been fixed.
//!
//! # Why this is its own test binary
//!
//! It installs a `#[global_allocator]`, which is per-binary — the same reason
//! `engine_clone_allocs.rs` is its own binary and `benches/generation.rs` keeps
//! its `CountingAllocator` to itself. It also reads `ids`' thread-local
//! fast/slow counters, and `docs/worldgen-staged-store.md` records the
//! measurement that forced the rule: a counter gate sharing a binary read 502
//! against a true 256.
//!
//! # What it measures, and why a *pass* rather than a *column*
//!
//! The plan's headline figure is `steady_state_heap_allocs_per_column`, which
//! lives in `benches/generation.rs` because it needs the embedded production data
//! and a 12×12 warm sweep. That is a bench, not a gate. This binary gates the part
//! Unit 8 owns — the placement engine itself — at a cost a test can pay, by
//! running one 3×3 `VEGETAL_DECORATION` pass twice and measuring the second.
//!
//! The recorded column figures either side of Unit 8, for context (release,
//! embedded data, seed 42, counting allocator, `--features gen-counters`):
//!
//! | sha | total | vegetation | intern | other |
//! |---|---|---|---|---|
//! | `5344b8ad` (pre-U8) | 20,678 | 20,621 | 41 | 19 |
//! | `1519464d` (post-U8) | 87 | 30 | 41 | 16 |
//!
//! `intern` is the returned column's own palette/blocks buffers — the plan's
//! explicit O(1) output allowance — and reads 41 in both, so Unit 8 neither
//! improved nor disturbed it.
//!
//! # The two arms, and why warm is the honest one
//!
//! Arm 1 is cold: it interns every state the scene produces, binds the tag
//! bitsets, fills the property-rewrite memos and grows every thread-local scratch
//! buffer. All of that is warmup by construction — the interner outlives every
//! column a generator serves (`docs/worldgen-state-interning.md`) — so arm 1 is
//! expected to allocate and is measured only to prove the instrument is live.
//!
//! Arm 2 is the steady state the budget is written against. **It is a fresh
//! `VegGrid` on the same interner and the same `VegTags`**, which is exactly the
//! production relationship: `OverworldGenerator` owns one interner and one
//! `VegTags` and builds a new grid per served column.
//!
//! # What is NOT zero, named rather than rounded away
//!
//! Arm 2 does not reach literal zero, and the residual is `VegGrid`'s own
//! containers, not the placement engine: the write overlay (`HashMap`) and the
//! `dirty` log (`Vec`) both start empty on a fresh grid and grow geometrically as
//! the pass writes into them, which is `O(log writes)` allocations — a dozen or so
//! each. `docs/worldgen-in-place-decoration.md` states outright that Unit 7 chose
//! *not* to pool them ("There is no buffer pool... If scratch reuse is ever needed
//! here it goes in a `thread_local` free-list"), so removing these is a change to
//! that unit's medium and is left as a named follow-up rather than smuggled in
//! here. `allocations_are_geometric_growth_not_per_write` is the assertion that
//! keeps the distinction honest: it fails if the count ever becomes proportional
//! to the number of writes again, which is what a reintroduced per-block
//! allocation would look like.

// The counting allocator needs `unsafe impl GlobalAlloc`, and the workspace sets
// `unsafe_code = "deny"`. Same exemption and same reason as
// `tests/engine_clone_allocs.rs` and `benches/generation.rs`: there is no safe way
// to observe real allocation counts, and an allocation claim asserted from
// structure rather than measured is exactly the kind this repo has had to retract.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};

use lodestone_worldgen::compose::build_biome_vegetation;
use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::feature::vegetation::{
    PlacedRef, VegGrid, VegTags, apply_vegetal_decoration_step_3x3_per_source, build_veg_tags,
    census, ids, is_air,
};
use lodestone_worldgen::feature::{REGION_MAX, REGION_MIN};
use lodestone_worldgen::rng::{WorldgenRandom, XoroshiroRandomSource};
use serde_json::Value;

thread_local! {
    /// `const`-initialised so touching it never allocates and so cannot recurse
    /// through the allocator that is touching it.
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    /// Gate, so the harness's own allocations and the (large) scene setup are not
    /// attributed to the pass under measurement.
    static ON: Cell<bool> = const { Cell::new(false) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with`, not `with`: an allocation during thread teardown happens
        // after TLS destruction, and a panic from inside the allocator is not
        // recoverable. No measurement can be in flight then anyway.
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

/// Runs `f` with counting on for this thread, returning its value and the count.
fn allocs_of<T>(f: impl FnOnce() -> T) -> (T, u64) {
    ALLOCS.set(0);
    ON.set(true);
    let out = f();
    ON.set(false);
    (out, ALLOCS.get())
}

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data")
}

/// The same fixture resolver `vegetation_parity.rs` uses, and deliberately the
/// same shape: shape generation is never reached, so those two methods panic
/// rather than silently answering `Null`.
struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    fn try_json(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        std::fs::read_to_string(&path)
            .ok()
            .map(|text| {
                serde_json::from_str(&text)
                    .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
            })
            .unwrap_or(Value::Null)
    }
}

impl Resolver for FsResolver {
    fn density_function(&self, id: &str) -> Value {
        panic!("vegetation_allocs never generates shape; unexpected density_function({id})");
    }
    fn noise(&self, id: &str) -> NoiseParams {
        panic!("vegetation_allocs never generates shape; unexpected noise({id})");
    }
    fn biome_document(&self, id: &str) -> Value {
        self.try_json("biome", id)
    }
    fn configured_feature(&self, id: &str) -> Value {
        self.try_json("configured_feature", id)
    }
    fn placed_feature(&self, id: &str) -> Value {
        self.try_json("placed_feature", id)
    }
    fn block_tag(&self, id: &str) -> Value {
        self.try_json("tags/block", id)
    }
}

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const GROUND_Y: i32 = 70;
/// Savanna, not plains. `vegetation_parity.rs` measured that **both** plains
/// fixtures place zero logs and zero leaves — `trees_plains` rolls no attempt
/// ~95% of the time — so a plains scene would exercise neither `place_tree`, nor
/// the leaf `distance=N` rewrite, nor the `waterlogged` fix-up, and this gate
/// would be the *world* species of vacuous test: green while claiming to cover
/// three sites it never reached. Savanna's `trees_savanna` places acacia at 80%
/// and oak at ~19.75%. The census assertions below make that a measurement rather
/// than a belief.
const BIOME: &str = "minecraft:savanna";

/// A grid whose whole driven region (plus the vegetation padding ring) is flat
/// grass under open air — enough for trees and grass patches to place, seeded
/// through `seed`, which is the same overlay write path production's sources fall
/// through to (`docs/worldgen-in-place-decoration.md`).
fn seed_flat_region(grid: &mut VegGrid, base_x: i32, base_z: i32) {
    let pad = 8;
    for lx in (REGION_MIN - pad)..(REGION_MAX + pad) {
        for lz in (REGION_MIN - pad)..(REGION_MAX + pad) {
            for y in (GROUND_Y - 3)..=GROUND_Y {
                grid.seed(
                    base_x + lx,
                    y,
                    base_z + lz,
                    "minecraft:grass_block[snowy=false]".to_string(),
                );
            }
        }
    }
}

/// A seeded grid ready to decorate — **built outside every measured window**.
///
/// This split is not cosmetic. The first version of this file measured setup and
/// pass together and read 16,411 allocations for 2,499 writes; 16,384 of those were
/// exactly [`seed_flat_region`]'s own `to_string()` per cell over a 64 × 64 × 4
/// region, i.e. the harness, not the engine. It also read *identically* 16,411
/// across two scenes with different write counts, which is what gave it away — a
/// constant where a per-write cost was hypothesised. Keep the seeding out here.
fn seeded_grid(
    interner: &std::sync::Arc<lodestone_worldgen::interner::StateInterner>,
    chunk_x: i32,
    chunk_z: i32,
) -> VegGrid {
    let base_x = chunk_x * 16;
    let base_z = chunk_z * 16;
    let mut grid = VegGrid::with_footprint_interned(
        interner.clone(),
        MIN_Y,
        HEIGHT,
        base_x,
        base_z,
        REGION_MIN,
        REGION_MAX,
    );
    seed_flat_region(&mut grid, base_x, base_z);
    grid
}

/// One full 3×3 `VEGETAL_DECORATION` pass — the production driver, unmodified, and
/// the only thing a measured window should ever contain.
fn run_pass(
    grid: &mut VegGrid,
    tags: &VegTags,
    features: &[(usize, PlacedRef)],
    chunk_x: i32,
    chunk_z: i32,
) {
    let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
    let features_for_source = |_x: i32, _z: i32| -> &[(usize, PlacedRef)] { features };
    apply_vegetal_decoration_step_3x3_per_source(
        &mut random,
        42,
        chunk_x,
        chunk_z,
        grid,
        tags,
        &features_for_source,
    );
}

/// The whole acceptance criterion, in one test because the two arms must share one
/// process and one thread (both counters are thread-local, and arm 2's whole point
/// is that arm 1 already warmed the shared state).
#[test]
fn a_warm_vegetal_decoration_pass_allocates_only_its_grids_own_container_growth() {
    let resolver = FsResolver { root: data_dir() };
    let tags = build_veg_tags(&resolver);
    let features = build_biome_vegetation(&resolver, BIOME);
    assert!(
        !features.is_empty(),
        "{BIOME} must resolve a non-empty VEGETAL_DECORATION list, or this gate \
         measures an empty pipeline"
    );
    // One interner shared by every grid, exactly as `OverworldGenerator` shares one
    // across every column it serves.
    let interner = std::sync::Arc::new(lodestone_worldgen::interner::StateInterner::new());

    // ---- Arm 1: cold. Interning, binding, memo fills, scratch growth. --------
    let mut cold_grid = seeded_grid(&interner, 0, 0);
    census::reset();
    ids::reset_counts();
    let (_, cold_allocs) = allocs_of(|| run_pass(&mut cold_grid, &tags, &features, 0, 0));
    let cold_writes = cold_grid.dirty_len();
    let cold_census = census::snapshot();
    assert!(
        cold_allocs > 0,
        "the counting allocator reported ZERO allocations for a cold pass that had \
         to intern every state in the scene. The instrument is not live — every \
         assertion below would be vacuous."
    );

    // ---- The scene really does reach the sites this unit changed -------------
    // Without these, a green result would be indistinguishable from a pass that
    // placed nothing. `apply_freeze_top_layer`'s recorded trap is exactly this
    // shape: real sites, invisible scene.
    assert!(
        cold_census.tree > 0,
        "no tree was dispatched, so place_tree, the leaf distance=N rewrite and \
         the waterlogged fix-up were all unexercised: {cold_census:?}"
    );
    assert!(
        cold_census.writes > 0,
        "the pass wrote no blocks at all: {cold_census:?}"
    );
    assert!(
        cold_census.simple_block > 0,
        "no SimpleBlock terminal ran, so the supports_vegetation ground check — the \
         most-executed rejection in the engine — was unexercised: {cold_census:?}"
    );
    assert!(
        cold_census.block_predicate_filter_in > 0,
        "no position reached a BlockPredicateFilter: {cold_census:?}"
    );
    assert!(cold_writes > 0, "the grid recorded no dirty cells");

    // ---- Arm 2: warm. The steady state the budget is written against. --------
    let mut warm_grid = seeded_grid(&interner, 0, 0);
    census::reset();
    ids::reset_counts();
    let (_, warm_allocs) = allocs_of(|| run_pass(&mut warm_grid, &tags, &features, 0, 0));
    let warm_writes = warm_grid.dirty_len();
    let warm_census = census::snapshot();
    let fast = ids::fast_hits();
    let slow = ids::slow_hits();

    assert_eq!(
        warm_census.writes, cold_census.writes,
        "the two arms must place identically — same seed, same features, same scene. \
         A difference means the warm arm is not measuring the same work."
    );

    // ---- The bitsets are actually answering ---------------------------------
    assert!(
        fast > 0,
        "not one tag query took the bitset path on a warm pass, so the O(1) \
         membership test this unit exists to install is not being used"
    );
    assert_eq!(
        slow, 0,
        "{slow} of {} tag queries fell back to the string path on a WARM pass. \
         A silent fallback is indistinguishable from a working fast path, which is \
         why this is asserted rather than assumed — the interner should mint no new \
         state by the second pass.",
        fast + slow
    );

    // ---- The allocation criterion, against both hypotheses -----------------
    // Computed from outside this code, per CLAUDE.md's *magnitude* rule: assert
    // where the number lands, not merely that it went down.
    //
    //  * PRE-U8 hypothesis: every placed block allocated a `String` (the provider
    //    clone) and every placement modifier allocated a `Vec` per attempt, so the
    //    count was at LEAST the number of writes, and in practice several times it.
    //  * POST-U8 hypothesis: the placement engine allocates nothing, and the only
    //    allocations are the fresh grid's overlay and dirty-log geometric growth,
    //    which is O(log writes) — for a few thousand writes, tens.
    let pre_u8_floor = u64::try_from(warm_census.writes).expect("writes fits u64");
    let geometric_ceiling = 4 * (64 - pre_u8_floor.max(1).leading_zeros() as u64) + 32;
    assert!(
        warm_allocs < pre_u8_floor,
        "a warm pass allocated {warm_allocs} for {} writes. The pre-U8 engine \
         allocated at least one String per write; landing at or above that floor \
         means the per-block allocation is back.",
        warm_census.writes
    );
    assert!(
        warm_allocs <= geometric_ceiling,
        "a warm pass allocated {warm_allocs}, above the {geometric_ceiling} a \
         purely geometric container growth predicts for {} writes. Something in the \
         placement engine is allocating per placement again — check the two \
         thread-local scratch buffers in `place.rs`, `tree.rs`'s BFS scratch, and \
         `ids`' rewrite memo (a memo miss allocates, and a miss on a WARM pass means \
         the memo key is wrong).",
        warm_census.writes
    );
    assert_eq!(warm_writes, cold_writes, "both arms must write the same cells");

    // ---- Detector control ---------------------------------------------------
    // Everything above is an assertion of near-absence, so it is worth exactly as
    // much as the evidence the mechanism would have fired. Observe it firing.
    let (sink, control_allocs) = allocs_of(|| {
        let mut v: Vec<Box<u64>> = Vec::new();
        for i in 0..100u64 {
            v.push(Box::new(i));
        }
        v.len()
    });
    assert_eq!(sink, 100);
    assert!(
        control_allocs >= 100,
        "the detector control made 100 boxed allocations inside a measured window \
         and the allocator counted only {control_allocs}. The near-zero numbers \
         above therefore prove nothing."
    );

    println!(
        "U8 vegetal-decoration pass allocations: cold {cold_allocs}, warm \
         {warm_allocs} (ceiling {geometric_ceiling}) for {} writes; tag queries \
         warm: {fast} bitset / {slow} string; trees {}, simple_block {}, \
         filter_in {}",
        warm_census.writes, warm_census.tree, warm_census.simple_block,
        warm_census.block_predicate_filter_in
    );
}

/// The residual is geometric container growth, not per-write allocation — and the
/// discriminating measurement is how the count responds to **more writes**.
///
/// A per-write allocation scales linearly; geometric `Vec`/`HashMap` growth scales
/// logarithmically. Two passes over scenes of very different size therefore
/// separate the two hypotheses in a way one absolute number cannot, which is the
/// point: this is the assertion that would catch a future change reintroducing an
/// allocation into the placement engine, even if the absolute count still looked
/// small.
#[test]
fn allocations_are_geometric_growth_not_per_write() {
    let resolver = FsResolver { root: data_dir() };
    let tags = build_veg_tags(&resolver);
    let features = build_biome_vegetation(&resolver, BIOME);
    let interner = std::sync::Arc::new(lodestone_worldgen::interner::StateInterner::new());

    // Warm everything first; the first pass on a thread is warmup by definition.
    let mut warmup = seeded_grid(&interner, 0, 0);
    run_pass(&mut warmup, &tags, &features, 0, 0);

    // Four further passes at different chunk coordinates. Different decoration
    // seeds mean genuinely different write counts. The grid is built and seeded
    // OUTSIDE the measured window — see `seeded_grid`.
    let mut samples: Vec<(usize, u64)> = Vec::new();
    for (cx, cz) in [(0, 0), (7, 11), (-3, 5), (20, -5)] {
        let mut grid = seeded_grid(&interner, cx, cz);
        census::reset();
        let (_, allocs) = allocs_of(|| run_pass(&mut grid, &tags, &features, cx, cz));
        samples.push((census::snapshot().writes, allocs));
    }
    samples.sort_unstable();
    let (min_writes, min_allocs) = samples[0];
    let (max_writes, max_allocs) = samples[samples.len() - 1];
    assert!(
        max_writes > min_writes,
        "the four scenes produced no spread in write counts ({samples:?}), so this \
         test cannot distinguish linear from logarithmic growth and is vacuous"
    );

    // Linear would predict allocs scaling like writes. Logarithmic predicts a
    // ratio near 1. Require the alloc ratio to sit far below the write ratio.
    let write_ratio = max_writes as f64 / min_writes.max(1) as f64;
    let alloc_ratio = max_allocs as f64 / min_allocs.max(1) as f64;
    assert!(
        alloc_ratio < write_ratio.max(1.5),
        "allocations scaled like writes ({min_allocs} -> {max_allocs}, ratio \
         {alloc_ratio:.2}) as the scene grew ({min_writes} -> {max_writes}, ratio \
         {write_ratio:.2}). That is the signature of a per-write allocation, which \
         is exactly what Unit 8 removed. Samples: {samples:?}"
    );
    println!(
        "writes {min_writes} -> {max_writes} (x{write_ratio:.2}) but allocs \
         {min_allocs} -> {max_allocs} (x{alloc_ratio:.2}) — logarithmic, not linear"
    );
}

/// Differential control for the one change Unit 8 made inside `grid.rs`:
/// `height_world_surface` now tests air by comparing against three cached
/// `StateId`s instead of resolving each cell's name through the interner.
///
/// That is only exact because air carries no block-state properties. This
/// reimplements the *old* string algorithm over a real, decorated scene and
/// requires the two to agree at every column — so if the id shortcut is ever wrong
/// (someone adds a property-carrying state to `is_air`), this fails rather than
/// silently shifting every heightmap-placed feature by a block.
#[test]
fn the_id_based_air_test_answers_what_the_string_scan_answered() {
    let resolver = FsResolver { root: data_dir() };
    let tags = build_veg_tags(&resolver);
    let features = build_biome_vegetation(&resolver, BIOME);
    let interner = std::sync::Arc::new(lodestone_worldgen::interner::StateInterner::new());

    let (base_x, base_z) = (0, 0);
    let mut grid = VegGrid::with_footprint_interned(
        interner.clone(),
        MIN_Y,
        HEIGHT,
        base_x,
        base_z,
        REGION_MIN,
        REGION_MAX,
    );
    seed_flat_region(&mut grid, base_x, base_z);
    run_pass(&mut grid, &tags, &features, 0, 0);
    assert!(
        grid.dirty_len() > 0,
        "nothing was decorated, so the columns compared below are all bare ground \
         and the control is premise-false"
    );

    // The deleted algorithm, rebuilt: topmost cell whose BASE NAME is not air.
    let string_scan = |x: i32, z: i32| -> i32 {
        for y in (MIN_Y..MIN_Y + HEIGHT).rev() {
            let state = grid.get(x, y, z);
            let base = state.split('[').next().unwrap_or(state);
            if !is_air(base) {
                return y + 1;
            }
        }
        MIN_Y
    };

    let mut compared = 0usize;
    let mut above_ground = 0usize;
    for lx in REGION_MIN..REGION_MAX {
        for lz in REGION_MIN..REGION_MAX {
            let (x, z) = (base_x + lx, base_z + lz);
            let by_id = grid.height_world_surface(x, z);
            let by_string = string_scan(x, z);
            assert_eq!(
                by_id, by_string,
                "height_world_surface disagrees with the string scan at ({x}, {z}): \
                 id path says {by_id}, name path says {by_string}"
            );
            compared += 1;
            if by_id > GROUND_Y + 1 {
                above_ground += 1;
            }
        }
    }
    assert_eq!(compared, 48 * 48, "the whole driven region must be compared");
    assert!(
        above_ground > 0,
        "every column measured exactly bare ground, so the comparison never saw a \
         decorated column and could not have distinguished the two algorithms on \
         anything but air"
    );
    println!(
        "height_world_surface: {compared} columns agree with the string scan, \
         {above_ground} of them carrying decoration"
    );
}
