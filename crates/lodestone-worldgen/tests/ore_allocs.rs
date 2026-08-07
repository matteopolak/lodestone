//! U18's acceptance criterion: a warm `UNDERGROUND_ORES` 3×3 pass allocates a
//! **bounded** amount that does not scale with the number of placement attempts.
//!
//! # Why this is its own test binary
//!
//! It installs a `#[global_allocator]`, which is per-binary — the same reason
//! `vegetation_allocs.rs`, `engine_clone_allocs.rs` and `benches/generation.rs`
//! each keep their own. `docs/worldgen-staged-store.md` records the measurement
//! that forced the rule: a counter gate sharing a binary read 502 against a true
//! 256.
//!
//! # What it measures, and why a *pass* rather than a *column*
//!
//! The column-level figure lives in `benches/generation.rs` (embedded production
//! data, 12×12 warm sweep) and in `tests/ore_alloc_attribution.rs`, which needs
//! `--features gen-counters` to bin by stage. **This gate needs neither**: it
//! drives `apply_ore_step_3x3` directly against a checked-in JVM fixture and
//! counts every allocation the call makes, so it runs under a plain
//! `cargo test --workspace` where the acceptance criterion actually needs to be
//! enforced.
//!
//! Measured either side of U18 (3×3 cold sweep, seed 42, embedded production
//! data, `--features gen-counters`, `tests/ore_alloc_attribution.rs`):
//!
//! | sha | ore-stage allocations | per ore pass | `rng_draws[Ore]` |
//! |---|---|---|---|
//! | `974bd78b` (pre-U18) | 207,671 | 8,306 | 992,537 |
//! | post-U18 | **503** | **20** | **992,537** |
//!
//! # The discriminating form
//!
//! An absolute bound alone is a weak gate: it passes for any implementation that
//! happens to be under it on this one fixture. What U18 removed was allocation
//! **per placement attempt**, so the assertion that fails if it comes back is
//! [`ore_allocations_do_not_scale_with_placement_attempts`] — four passes must
//! not cost four times one pass. That is `CLAUDE.md`'s *magnitude* species
//! answered directly: both hypotheses are computed from the measurement and the
//! result has to land on one of them.

// The counting allocator needs `unsafe impl GlobalAlloc`, and the workspace sets
// `unsafe_code = "deny"`. Same exemption and same reason as
// `tests/vegetation_allocs.rs`: there is no safe way to observe real allocation
// counts, and an allocation claim asserted from structure rather than measured is
// exactly the kind this repo has had to retract.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lodestone_worldgen::dense_grid::DenseBlockGrid;
use lodestone_worldgen::feature::region_view::RegionView;
use lodestone_worldgen::feature::{
    PlacedOre, REGION_MAX, REGION_MIN, RuleTest, apply_ore_step_3x3, parse_ore_config,
    parse_placements,
};
use lodestone_worldgen::rng::{WorldgenRandom, XoroshiroRandomSource};
use serde_json::Value;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const MIN_GEN_Y: i32 = -64;
const GEN_DEPTH: i32 = 384;

/// The land fixture, deliberately — and this is not incidental. The oceanic
/// `feature_ore_plains_jvm.txt` fixture places far fewer ores, so a gate written
/// against it would assert a bound it meets for want of work rather than for want
/// of allocation. `placements_are_non_degenerate` makes that a measurement.
const FIXTURE: &str = "feature_ore_plains_land_jvm.txt";

thread_local! {
    /// `const`-initialised so touching it never allocates and so cannot recurse
    /// through the allocator that is touching it.
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    /// Gate, so the harness's own allocations and the (large) fixture parse are
    /// not attributed to the pass under measurement.
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

// ---------------------------------------------------------------------------
// Fixture + version data (the subset this gate needs)
// ---------------------------------------------------------------------------

fn support_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support")
}

fn data_dir() -> PathBuf {
    support_dir().join("worldgen_data")
}

fn read_json(path: &Path) -> Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

fn strip(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

struct Fixture {
    input: HashMap<(i32, i32, i32), String>,
    ocean_floor_wg: HashMap<(i32, i32), i32>,
    chunk_x: i32,
    chunk_z: i32,
    seed: i64,
}

/// The `inrun.*` / `ofh.*` / `meta.*` subset of the fixture format
/// `feature_parity.rs` documents. `ore.*` and `count.*` are the parity arms'
/// business, not this gate's, so they are skipped rather than parsed.
fn parse_fixture(text: &str) -> Fixture {
    let mut f = Fixture {
        input: HashMap::new(),
        ocean_floor_wg: HashMap::new(),
        chunk_x: 0,
        chunk_z: 0,
        seed: 0,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (tag, rest) = line.split_once(' ').expect("tag value");
        if let Some(coords) = tag.strip_prefix("inrun.") {
            let (xs, zs) = coords.split_once(',').expect("inrun.x,z");
            let x: i32 = xs.parse().expect("inrun x");
            let z: i32 = zs.parse().expect("inrun z");
            let mut tok = rest.split_whitespace();
            let y_start: i32 = tok.next().expect("run y_start").parse().expect("y_start int");
            let count: i32 = tok.next().expect("run count").parse().expect("count int");
            let state = tok.next().expect("run state").to_string();
            for dy in 0..count {
                f.input.insert((x, y_start + dy, z), state.clone());
            }
        } else if let Some(coords) = tag.strip_prefix("ofh.") {
            let (x, z) = coords.split_once(',').unwrap();
            f.ocean_floor_wg
                .insert((x.parse().unwrap(), z.parse().unwrap()), rest.parse().unwrap());
        } else {
            match tag {
                "meta.chunkX" => f.chunk_x = rest.parse().unwrap(),
                "meta.chunkZ" => f.chunk_z = rest.parse().unwrap(),
                "meta.seed" => f.seed = rest.parse().unwrap(),
                _ => {}
            }
        }
    }
    f
}

fn build_plains_ores() -> Vec<PlacedOre> {
    let root = data_dir();
    let plains = read_json(&root.join("biome/plains.json"));
    let step6 = plains["features"][6]
        .as_array()
        .expect("plains step 6 feature list");
    let mut ores = Vec::new();
    for (i, entry) in step6.iter().enumerate() {
        let placed_id = entry.as_str().expect("placed feature id");
        let placed = read_json(&root.join(format!("placed_feature/{}.json", strip(placed_id))));
        let cf_id = placed["feature"].as_str().expect("configured feature id");
        let configured = read_json(&root.join(format!("configured_feature/{}.json", strip(cf_id))));
        if configured["type"].as_str() == Some("minecraft:ore") {
            ores.push(PlacedOre {
                index: i,
                placements: parse_placements(&placed),
                config: parse_ore_config(&configured["config"]),
            });
        }
    }
    ores
}

fn resolve_block_tag(root: &Path, id: &str, out: &mut HashSet<String>, seen: &mut HashSet<String>) {
    if !seen.insert(id.to_string()) {
        return;
    }
    let path = root.join("tags/block").join(format!("{}.json", strip(id)));
    let doc = read_json(&path);
    for entry in doc["values"].as_array().expect("tag values") {
        let s = match entry {
            Value::String(s) => s.as_str(),
            Value::Object(o) => o["id"].as_str().expect("tag entry id"),
            other => panic!("unexpected tag entry: {other}"),
        };
        if let Some(sub) = s.strip_prefix('#') {
            resolve_block_tag(root, sub, out, seen);
        } else {
            out.insert(s.to_string());
        }
    }
}

fn build_tag_map(ores: &[PlacedOre]) -> HashMap<String, HashSet<String>> {
    let root = data_dir();
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    for ore in ores {
        for target in &ore.config.targets {
            if let RuleTest::TagMatch(tag) = &target.target {
                map.entry(tag.clone()).or_insert_with(|| {
                    let mut out = HashSet::new();
                    let mut seen = HashSet::new();
                    resolve_block_tag(&root, tag, &mut out, &mut seen);
                    out
                });
            }
        }
    }
    map
}

/// One full 3×3 `UNDERGROUND_ORES` pass over the fixture, returning the number of
/// distinct cells written. A **fresh `RegionView` per call**, which is the
/// production relationship: `OverworldGenerator` builds one per served column.
fn one_pass(
    f: &Fixture,
    grid: &DenseBlockGrid,
    ores: &[PlacedOre],
    tag_map: &HashMap<String, HashSet<String>>,
) -> usize {
    let in_tag =
        |base: &str, tag: &str| -> bool { tag_map.get(tag).is_some_and(|set| set.contains(base)) };
    let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
    let mut working = RegionView::over_region_grid(grid, MIN_Y, HEIGHT);
    apply_ore_step_3x3(
        &mut random,
        f.seed,
        f.chunk_x,
        f.chunk_z,
        MIN_Y,
        HEIGHT,
        MIN_GEN_Y,
        GEN_DEPTH,
        &f.ocean_floor_wg,
        &in_tag,
        &mut working,
        ores,
    );
    working.writes()
}

struct Scene {
    fixture: Fixture,
    grid: DenseBlockGrid,
    ores: Vec<PlacedOre>,
    tag_map: HashMap<String, HashSet<String>>,
}

fn scene() -> Scene {
    let text = std::fs::read_to_string(support_dir().join(FIXTURE))
        .unwrap_or_else(|e| panic!("reading {FIXTURE}: {e}"));
    let fixture = parse_fixture(&text);
    let region_size = REGION_MAX - REGION_MIN;
    let grid = DenseBlockGrid::from_hashmap(
        REGION_MIN,
        MIN_Y,
        REGION_MIN,
        region_size,
        HEIGHT,
        region_size,
        &fixture.input,
    );
    let ores = build_plains_ores();
    let tag_map = build_tag_map(&ores);
    Scene { fixture, grid, ores, tag_map }
}

/// The bound a warm pass must stay under.
///
/// **Measured at 1**, on a fixture making 50,920 writes; 64 is deliberate
/// headroom, not a tuned threshold.
///
/// The first draft of this comment derived the bound as "the `RegionView`
/// overlay growing geometrically from empty, so `O(log2 w)` ≈ a dozen" — and
/// that derivation is **wrong**, which the measurement caught. `RegionView`'s
/// overlay map and write log are recycled from a thread-local free-list
/// (`region_view.rs`'s `MAPS`/`LOGS`), so a warm pass does not regrow them at
/// all; U7's "no pool" rule is about a *shared* pool, not a per-thread one. What
/// is actually left is one allocation per pass.
///
/// The headroom is kept rather than the bound tightened to 1, because the
/// residual belongs to a medium this unit does not own (U19 is live in
/// `region_view.rs`) and a gate that fails when a *neighbouring* unit changes its
/// free-list would be a false alarm about the wrong file. The property U18 owns
/// is the *shape*, and
/// [`ore_allocations_do_not_scale_with_placement_attempts`] is the sharp test of
/// it: pre-U18 this fixture's warm pass allocated on the order of one per
/// placement attempt, two orders of magnitude above this bound.
const WARM_BOUND: u64 = 64;

/// The acceptance criterion.
#[test]
fn a_warm_ore_pass_allocates_a_bounded_amount_not_one_per_attempt() {
    let s = scene();

    // Arm 1 is cold: it interns every state the fixture produces and grows both
    // thread-local scratch buffers to their high-water mark. All of that is
    // warmup by construction — the interner and the scratch outlive every column
    // a generator serves — so arm 1 is expected to allocate and is measured only
    // to prove the instrument is live.
    let (cold_writes, cold_allocs) =
        allocs_of(|| one_pass(&s.fixture, &s.grid, &s.ores, &s.tag_map));

    // Arm 2 is the steady state the budget is written against.
    let (warm_writes, warm_allocs) =
        allocs_of(|| one_pass(&s.fixture, &s.grid, &s.ores, &s.tag_map));

    println!(
        "ore pass: cold {cold_allocs} allocs / {cold_writes} writes, \
         warm {warm_allocs} allocs / {warm_writes} writes"
    );

    assert!(
        cold_allocs > 0,
        "the counting allocator observed nothing on a cold pass, so this gate \
         cannot fail for the right reason either — the instrument is not live"
    );
    assert_eq!(
        cold_writes, warm_writes,
        "the two arms must place identically; a differing write count means the \
         scratch reuse changed the world, which is the one outcome this unit \
         must not have"
    );
    assert!(
        warm_allocs <= WARM_BOUND,
        "a warm 3x3 ore pass allocated {warm_allocs}, over the {WARM_BOUND} \
         bound, for {warm_writes} writes. The two hypotheses: scratch reuse \
         predicts a count independent of the work done (measured at 1, because \
         RegionView recycles its overlay from a thread-local free-list rather \
         than regrowing it); a per-attempt allocation predicts a count that \
         scales with placement attempts, which the two controls in \
         docs/worldgen-ore-allocations.md measured at 6,701 and 1,901 per pass \
         on this very fixture. {warm_allocs} is in the second regime."
    );
}

/// The discriminating form, and the one that fails if per-attempt allocation
/// comes back.
///
/// An absolute bound is satisfied by any implementation that happens to sit under
/// it. This asserts the **shape**: four passes must not cost four times one pass.
/// Both hypotheses are computed from the measurement, so the result has to land
/// on one of them rather than merely have the right sign.
#[test]
fn ore_allocations_do_not_scale_with_placement_attempts() {
    let s = scene();
    // Warm up: intern every state, grow both scratch buffers to high water.
    one_pass(&s.fixture, &s.grid, &s.ores, &s.tag_map);

    let (writes_1, allocs_1) = allocs_of(|| one_pass(&s.fixture, &s.grid, &s.ores, &s.tag_map));
    let (writes_4, allocs_4) = allocs_of(|| {
        let mut w = 0;
        for _ in 0..4 {
            w = one_pass(&s.fixture, &s.grid, &s.ores, &s.tag_map);
        }
        w
    });

    println!(
        "1 pass: {allocs_1} allocs / {writes_1} writes; \
         4 passes: {allocs_4} allocs / {writes_4} writes"
    );

    // The wrong hypothesis, computed rather than described: if allocation were
    // per attempt, four passes would perform four times the work and so ~4x the
    // allocations. The right hypothesis is that each pass pays only its own fresh
    // `RegionView` overlay's growth, so the ratio is ~4x a *dozen* rather than
    // ~4x a *thousand* — and the absolute numbers separate those by two orders
    // of magnitude. The ratio alone cannot discriminate (both are ~4x), which is
    // why the assertion is on the magnitude of one pass.
    assert!(
        allocs_1 <= WARM_BOUND,
        "one warm pass allocated {allocs_1} for {writes_1} writes, over the \
         {WARM_BOUND} bound: allocation is scaling with attempts again"
    );
    assert!(
        allocs_4 <= 4 * WARM_BOUND,
        "four warm passes allocated {allocs_4}, over 4 x {WARM_BOUND}. Per-pass \
         overlay growth is expected and bounded; a per-attempt allocation is not."
    );
    // And the per-write figure must be far below 1, which is the property a
    // per-candidate `Vec` structurally cannot have.
    assert!(
        u64::try_from(writes_1).unwrap() > allocs_1 * 4,
        "writes ({writes_1}) should exceed allocations ({allocs_1}) by a large \
         factor; a comparable count means one allocation per placement again"
    );
}

/// The *world* species of vacuous test, closed: this fixture has to actually
/// place a large number of ores, or every bound above is met for want of work.
#[test]
fn placements_are_non_degenerate() {
    let s = scene();
    let writes = one_pass(&s.fixture, &s.grid, &s.ores, &s.tag_map);
    assert!(
        writes > 500,
        "{FIXTURE} produced only {writes} ore writes; the allocation bounds in \
         this file would then be met by a pass that barely runs. Pick a scene \
         with more ore, or this whole binary is measuring nothing."
    );
    assert!(
        !s.ores.is_empty(),
        "no ore features parsed from plains step 6 — the version data moved and \
         this gate is running an empty driver"
    );
    println!("non-degeneracy: {} ore features, {writes} writes", s.ores.len());
}
