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
//! # Arm 2 now reads literal zero — Unit 19
//!
//! Until Unit 19 arm 2 read **13**, and the residual was `VegGrid`'s own
//! containers rather than the placement engine: the write overlay and the `dirty`
//! log both started empty on a fresh grid and grew geometrically as the pass wrote
//! into them, `O(log writes)` allocations each. Unit 8 left them because
//! `docs/worldgen-in-place-decoration.md` records Unit 7 deliberately *not* pooling
//! them ("There is no buffer pool... If scratch reuse is ever needed here it goes in
//! a `thread_local` free-list") and a non-owned file's lifecycle was not Unit 8's to
//! change. Unit 19 owns both files and took exactly the escape hatch that doc named:
//! `feature::region_view::scratch`'s per-thread free-list. Arm 2 reads **0**.
//!
//! Three things about that number are worth keeping, because each was a wrong
//! answer first:
//!
//! * **`drop(cold_grid)` between the arms is load-bearing.** Production builds one
//!   `VegGrid` per served column and drops it before building the next; that drop is
//!   what returns the buffers. Holding both arms' grids alive — what this file did
//!   before — makes arm 2 draw from an *empty* free-list, so it measured a cold
//!   growth while calling itself warm and still read 13 after the pooling landed.
//! * **`census::reset()` must not happen inside a measured region.** The census's
//!   `unsupported` map is a `BTreeMap<String, usize>`; clearing it costs a `String`
//!   clone plus a node on the first unmodelled dispatch of each distinct reason —
//!   measured as exactly **2**, flat across four scenes of 2,086–2,499 writes.
//!   `OverworldGenerator` never resets the census, so those 2 are the instrument,
//!   not the engine. The arms take deltas instead.
//! * **Zero is asserted as an equality and paired with a causal control.**
//!   `a_warm_pass_allocates_zero_at_every_scene_size` replaces the old
//!   `allocations_are_geometric_growth_not_per_write` — whose ratio form divided by
//!   a floored zero and *failed against a correct implementation* — and
//!   `recycling_is_what_removes_the_containers_and_draining_the_free_list_puts_them_back`
//!   drains the free-list and observes the allocations return (0 → 11 for the same
//!   2,499-write pass), so "zero" is attributed to the mechanism rather than assumed.

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

    // ---- The production lifecycle, and why this `drop` is load-bearing -------
    // `OverworldGenerator` builds one `VegGrid` per served column and drops it
    // before building the next, which is what returns its containers to Unit 19's
    // per-thread free-list. Holding two grids alive across the two arms — what
    // this file did before U19 — means arm 2 draws from an *empty* free-list and
    // measures a cold container growth while calling itself warm. That is not a
    // detail of the harness: it is the difference between measuring production's
    // steady state and measuring its first column.
    drop(cold_grid);
    assert_eq!(
        lodestone_worldgen::feature::region_view::scratch_free_list_lengths(),
        (1, 1),
        "dropping the cold grid must have returned its overlay map and its write log \
         to this thread's free-list; without that, arm 2 below is a second cold arm \
         wearing a warm label"
    );

    // ---- Arm 2: warm. The steady state the budget is written against. --------
    // **No `census::reset()` here, and that is a measurement decision.** The census
    // accumulates; `reset()` clears its `unsupported` `BTreeMap<String, usize>`,
    // and re-filling that map costs a `String` clone plus a tree node on the first
    // unmodelled dispatch of each distinct reason — measured as exactly 2
    // allocations, flat across all four scenes, once U19 pooled the containers.
    // Those 2 belong to the *instrument*: `OverworldGenerator` never resets the
    // census, so from its second column onward every reason key is already present
    // and the cost is zero. Resetting here and then calling the residual "engine
    // allocations" would have attributed the harness to the subject — the same
    // mistake this file's own `seeded_grid` doc records for the 16,384 `to_string()`s.
    // A delta across the window measures the pass without disturbing the map.
    let mut warm_grid = seeded_grid(&interner, 0, 0);
    ids::reset_counts();
    let before = census::snapshot();
    let (_, warm_allocs) = allocs_of(|| run_pass(&mut warm_grid, &tags, &features, 0, 0));
    let after = census::snapshot();
    let warm_writes = warm_grid.dirty_len();
    let warm_census = census::VegCensus {
        writes: after.writes - before.writes,
        tree: after.tree - before.tree,
        simple_block: after.simple_block - before.simple_block,
        block_predicate_filter_in: after.block_predicate_filter_in - before.block_predicate_filter_in,
        ..census::VegCensus::default()
    };
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
    //  * U19 hypothesis: **exactly zero**. The two containers are recycled through
    //    a per-thread free-list, so a warm grid takes buffers that are already at
    //    the high-water capacity of the scene arm 1 ran, and nothing in the pass
    //    can grow them.
    //
    // Zero is asserted as an equality, not as a ceiling, deliberately: a ceiling
    // of "a dozen or so" is satisfied by a partially-broken free-list, and the
    // whole point of this unit is that there is no residual left to hide in.
    let pre_u8_floor = u64::try_from(warm_census.writes).expect("writes fits u64");
    let geometric_ceiling = 4 * (64 - pre_u8_floor.max(1).leading_zeros() as u64) + 32;
    assert!(
        warm_allocs < pre_u8_floor,
        "a warm pass allocated {warm_allocs} for {} writes. The pre-U8 engine \
         allocated at least one String per write; landing at or above that floor \
         means the per-block allocation is back.",
        warm_census.writes
    );
    assert_eq!(
        warm_allocs, 0,
        "a warm pass allocated {warm_allocs} for {} writes, against U19's predicted \
         zero (the pre-U19 geometric-growth hypothesis predicted up to \
         {geometric_ceiling}). Either the placement engine is allocating per \
         placement again — check `place.rs`/`tree.rs`'s thread-local scratch and \
         `ids`' rewrite memo — or the grid's containers are no longer being recycled \
         through `feature::region_view`'s free-list. \
         `recycling_is_what_removes_the_containers_and_draining_the_free_list_puts_them_back` \
         separates those two causes.",
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

/// Zero is zero **at every scene size**, which is what separates it from a small
/// number that happens to look like zero.
///
/// # What this replaced, and why the old shape had to go
///
/// Until Unit 19 this test was `allocations_are_geometric_growth_not_per_write`,
/// and it argued from the *shape* of the residual: a per-write allocation scales
/// linearly with writes, geometric `Vec`/`HashMap` growth scales logarithmically,
/// so requiring `alloc_ratio < write_ratio` separated the two hypotheses without
/// needing an absolute number. That was the right test for a residual that
/// existed. With the containers recycled there is no residual to characterise, and
/// the ratio form actively misfires: `min_allocs` reaches 0, `max(1)` floors the
/// denominator, and the very first (still-cold) scene divides by it — the shape
/// **failed against a correct implementation**, reporting `2 -> 13, ratio 6.50`
/// against a write ratio of 1.20.
///
/// The replacement is strictly stronger rather than merely different. A
/// reintroduced per-write allocation — the thing the old form existed to catch —
/// makes a warm count non-zero at *every* scene, and the largest scene worst, so
/// this fails on it too; but this also fails on a residual the old form was built
/// to tolerate. The spread assertion is kept, because without it four identical
/// scenes would pass vacuously.
#[test]
fn a_warm_pass_allocates_zero_at_every_scene_size() {
    let resolver = FsResolver { root: data_dir() };
    let tags = build_veg_tags(&resolver);
    let features = build_biome_vegetation(&resolver, BIOME);
    let interner = std::sync::Arc::new(lodestone_worldgen::interner::StateInterner::new());

    const SCENES: [(i32, i32); 4] = [(0, 0), (7, 11), (-3, 5), (20, -5)];

    // Warm the whole thread, including the free-list's capacity high-water mark.
    // Each grid is dropped at the end of its own iteration — production's
    // lifecycle — so the buffers come back before the next take. Two rounds, not
    // one: round 1 grows each buffer to whatever *its* scene needed, and only
    // after every scene has been seen once is the retained capacity guaranteed to
    // cover the largest of them. That convergence is the honest cost model — the
    // growth is paid once per thread, not once per column — and it is why this is
    // self-tuning rather than a `with_capacity` guess. `docs/worldgen-fast-hashing.md`
    // explicitly declined to guess a number here for want of a measured target.
    for _ in 0..2 {
        for (cx, cz) in SCENES {
            let mut grid = seeded_grid(&interner, cx, cz);
            run_pass(&mut grid, &tags, &features, cx, cz);
        }
    }

    // Deltas, never `census::reset()` inside a measured region — see arm 2 of the
    // test above for the 2 allocations a reset costs and why they are the
    // instrument rather than the engine.
    let mut samples: Vec<(usize, u64)> = Vec::new();
    for (cx, cz) in SCENES {
        let mut grid = seeded_grid(&interner, cx, cz);
        let before = census::snapshot().writes;
        let (_, allocs) = allocs_of(|| run_pass(&mut grid, &tags, &features, cx, cz));
        samples.push((census::snapshot().writes - before, allocs));
    }
    samples.sort_unstable();
    let (min_writes, _) = samples[0];
    let (max_writes, _) = samples[samples.len() - 1];
    assert!(
        max_writes > min_writes,
        "the four scenes produced no spread in write counts ({samples:?}), so a \
         zero result here says nothing about whether the count responds to scene \
         size and this test is vacuous"
    );
    let worst = samples.iter().map(|&(_, a)| a).max().expect("four samples");
    assert_eq!(
        worst, 0,
        "a warm pass allocated up to {worst} across four scenes of {min_writes}..\
         {max_writes} writes. A count that is non-zero and rises with writes is a \
         per-write allocation back in the placement engine; a count that is non-zero \
         and flat is a container escaping the free-list. Samples (writes, allocs): \
         {samples:?}"
    );
    println!(
        "warm passes over {min_writes}..{max_writes} writes: 0 allocations at every \
         scene size. Samples: {samples:?}"
    );
}

/// The control for every zero above: **the free-list is what removes them**, and
/// draining it puts them back.
///
/// Two assertions of near-absence sit above this one, and CLAUDE.md's rule is that
/// each is worth exactly as much as the evidence its mechanism would have fired.
/// The mechanism here is `feature::region_view`'s per-thread free-list, and the
/// discriminating experiment is available *in this process, on this thread, with
/// one variable*: run the identical pass over the identical scene twice, dropping
/// the free-list in between. Same binary, same code path, same scene, same seed.
///
/// This is not the detector control (that one proves the allocator counts at all,
/// and lives in the test above). This is the causal one: it rules out "the warm
/// count was zero for some unrelated reason and the free-list is inert".
#[test]
fn recycling_is_what_removes_the_containers_and_draining_the_free_list_puts_them_back() {
    use lodestone_worldgen::feature::region_view::{
        drain_scratch_free_lists, scratch_free_list_lengths,
    };

    let resolver = FsResolver { root: data_dir() };
    let tags = build_veg_tags(&resolver);
    let features = build_biome_vegetation(&resolver, BIOME);
    let interner = std::sync::Arc::new(lodestone_worldgen::interner::StateInterner::new());

    // Converge the thread, exactly as the test above does.
    for _ in 0..2 {
        let mut grid = seeded_grid(&interner, 0, 0);
        run_pass(&mut grid, &tags, &features, 0, 0);
    }

    // ---- Arm A: free-list populated. The claim. ------------------------------
    assert_eq!(
        scratch_free_list_lengths(),
        (1, 1),
        "the warmup must have left one recycled buffer of each shape on this thread"
    );
    let mut recycled = seeded_grid(&interner, 0, 0);
    let before_a = census::snapshot().writes;
    let (_, recycled_allocs) = allocs_of(|| run_pass(&mut recycled, &tags, &features, 0, 0));
    let recycled_writes = census::snapshot().writes - before_a;
    drop(recycled);

    // ---- Arm B: free-list drained. The control. -----------------------------
    drain_scratch_free_lists();
    assert_eq!(
        scratch_free_list_lengths(),
        (0, 0),
        "the drain did not empty the free-list, so arm B is a second arm A"
    );
    let mut fresh = seeded_grid(&interner, 0, 0);
    let before_b = census::snapshot().writes;
    let (_, fresh_allocs) = allocs_of(|| run_pass(&mut fresh, &tags, &features, 0, 0));
    let fresh_writes = census::snapshot().writes - before_b;

    assert_eq!(
        recycled_writes, fresh_writes,
        "the two arms must place identically — same scene, same seed, same features. \
         A difference means they are not the same experiment and the comparison below \
         is meaningless."
    );
    assert_eq!(
        recycled_allocs, 0,
        "arm A (free-list populated) allocated {recycled_allocs}, not zero"
    );
    assert!(
        fresh_allocs > 0,
        "arm B drained the free-list and the identical pass STILL allocated nothing. \
         The free-list is therefore not what makes arm A zero — something else is, \
         and every zero in this file is unexplained. Check that `VegGrid`'s overlay \
         and write log really are `region_view`'s pooled types and that their `Drop` \
         returns them."
    );
    println!(
        "free-list control: recycled {recycled_allocs} vs drained {fresh_allocs} \
         allocations for the same {recycled_writes}-write pass"
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
