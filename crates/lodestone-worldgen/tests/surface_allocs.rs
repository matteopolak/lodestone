//! U21's acceptance criterion: a chunk's worth of surface rules
//! allocates a **bounded** amount that does not scale with the number of
//! pre-surface probes.
//!
//! # Why this is its own test binary
//!
//! It installs a `#[global_allocator]`, which is per-binary — the same reason
//! `ore_allocs.rs`, `vegetation_allocs.rs`, `engine_clone_allocs.rs` and
//! `benches/generation.rs` each keep their own. `docs/worldgen-staged-store.md`
//! records the measurement that forced the rule: a counter gate sharing a binary
//! read 502 against a true 256.
//!
//! # Why it drives `build_surface` directly rather than a column
//!
//! Two reasons, and the first is a trap `docs/worldgen-state-interning.md`
//! records explicitly: **most stages do not run on a warm column.** Fill,
//! surface, carve and ore all read **0** allocations on a steady-state column
//! because the staged store serves them from cache, so a gate aimed at the warm
//! per-column counter "will measure nothing and conclude wrongly". The stage
//! has to be driven cold, and driving it directly is the cheapest way to do so
//! deterministically.
//!
//! The second is that this way the gate needs **no feature flag**. The
//! stage-binned figures live in `tests/ore_alloc_attribution.rs`, which requires
//! `--features gen-counters`; this file runs under a plain
//! `cargo test --workspace`, which is where an acceptance criterion has to be
//! enforced to mean anything.
//!
//! # Measured either side of U21
//!
//! 3×3 cold sweep, seed 42, embedded production data, real `GlobalAlloc` calls
//! binned by innermost stage (`tests/ore_alloc_attribution.rs`), digit-stable
//! across two runs per arm:
//!
//! | arm | `surface` allocations | per stage entry |
//! |---|---|---|
//! | `eba23934` (pre-U21) | 3,847,972 | 78,530 |
//! | post-U21 | **690** | **14** |
//!
//! All nine other stages were digit-identical across the two arms, and the 45
//! columns of `tests/u15_column_dump.rs` were byte-identical (md5
//! `a9db7cf741214167db615fa8b9356fa8`) with a bit-flip detector control
//! observed firing.
//!
//! # The discriminating form
//!
//! An absolute bound alone is a weak gate: it passes for any implementation that
//! happens to sit under it on this one fixture. What U21 removed was allocation
//! **per pre-surface probe**, so this file counts the probes as well and
//! computes *both* hypotheses from the same run — `CLAUDE.md`'s *magnitude*
//! species answered directly, rather than asserting the sign of a change.
//!
//! [`nothing_is_interned_during_a_surface_scan`] is the other half, and it is
//! the more precise of the two: the reason the count is bounded is that the scan
//! resolves no strings at all, and `StateInterner::len` is the observable that
//! moves the moment someone puts an `id_of` back on this path.

// The counting allocator needs `unsafe impl GlobalAlloc`, and the workspace sets
// `unsafe_code = "deny"`. Same exemption and same reason as `tests/ore_allocs.rs`
// and `tests/vegetation_allocs.rs`: there is no safe way to observe real
// allocation counts, and an allocation claim asserted from structure rather than
// measured is exactly the kind this repo has had to retract.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lodestone_worldgen::density::{Builder, NoiseParams, Resolver};
use lodestone_worldgen::interner::StateInterner;
use lodestone_worldgen::surface::{BlockCanon, PreState, SurfaceSystem};
use serde_json::Value;

const SEED: i64 = 42;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    /// Armed only around the call under measurement — the fixture parsing below
    /// allocates far more than the subject does, and attributing that to the
    /// stage would read as a working instrument reporting a surprising answer.
    static ON: Cell<bool> = const { Cell::new(false) };
}

struct Counting;

// SAFETY: every allocation is forwarded unchanged to the system allocator, which
// upholds `GlobalAlloc`'s contract; the counter is a thread-local `Cell` touched
// before the forward and never reentered (nothing here allocates).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with`: an allocation during thread teardown happens after TLS
        // destruction, and a panic from inside the allocator is not recoverable.
        // No measurement can be in flight then anyway.
        if ON.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCS.try_with(|c| c.set(c.get().wrapping_add(1)));
        }
        // SAFETY: `layout` is forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` came from `Self::alloc`, i.e. from `System`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Runs `f` armed, returning its value and the allocations it made.
fn allocs_of<T>(f: impl FnOnce() -> T) -> (T, u64) {
    ALLOCS.set(0);
    ON.set(true);
    let out = f();
    ON.set(false);
    (out, ALLOCS.get())
}

struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }
}

impl Resolver for FsResolver {
    fn density_function(&self, id: &str) -> Value {
        self.read("density_function", id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        let v = self.read("noise", id);
        NoiseParams {
            first_octave: v["firstOctave"].as_i64().expect("firstOctave") as i32,
            amplitudes: v["amplitudes"]
                .as_array()
                .expect("amplitudes")
                .iter()
                .map(|a| a.as_f64().expect("amplitude"))
                .collect(),
        }
    }
}

/// The JVM fixture `surface_parity.rs` compares against, parsed for the parts
/// this gate needs. Deliberately the **same** checked-in dump: a scene invented
/// for an allocation gate could fail to contain the structure the code under test
/// exists to handle (`CLAUDE.md`'s *world* species), and this one is known to
/// drive the real rule tree because a parity test asserts its output block for
/// block.
struct Fixture {
    pre: HashMap<(i32, i32, i32), String>,
    hm: HashMap<(i32, i32), i32>,
    canon: BlockCanon,
    biome: String,
    chunk_x: i32,
    chunk_z: i32,
}

fn parse_fixture(text: &str) -> Fixture {
    let mut f = Fixture {
        pre: HashMap::new(),
        hm: HashMap::new(),
        canon: HashMap::new(),
        biome: String::new(),
        chunk_x: 0,
        chunk_z: 0,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (tag, rest) = line.split_once(' ').expect("tag value");
        if let Some(coords) = tag.strip_prefix("pre.") {
            let mut it = coords.split(',');
            let x = it.next().unwrap().parse().unwrap();
            let y = it.next().unwrap().parse().unwrap();
            let z = it.next().unwrap().parse().unwrap();
            f.pre.insert((x, y, z), rest.to_string());
        } else if let Some(coords) = tag.strip_prefix("hm.") {
            let (x, z) = coords.split_once(',').expect("hm x,z");
            f.hm.insert((x.parse().unwrap(), z.parse().unwrap()), rest.parse().unwrap());
        } else if let Some(part_key) = tag.strip_prefix("canonmap.") {
            f.canon.insert(part_key.to_string(), rest.to_string());
        } else if tag == "meta.biome" {
            f.biome = rest.to_string();
        } else if tag == "meta.chunkX" {
            f.chunk_x = rest.parse().unwrap();
        } else if tag == "meta.chunkZ" {
            f.chunk_z = rest.parse().unwrap();
        }
    }
    f
}

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data")
}

/// A built scene: the interpreter, its interner, and the fixture it scans.
struct Scene {
    surface: SurfaceSystem,
    interner: Arc<StateInterner>,
    fixture: Fixture,
    /// The fixture's pre-surface column, pre-resolved to [`PreState`]s. Resolved
    /// **outside** the measured window on purpose: production hands
    /// `build_surface` ids it already owns (`overworld/fill.rs` reads them off
    /// three fields), so a gate that interned inside the window would be
    /// measuring the harness rather than the stage.
    pre_states: HashMap<(i32, i32, i32), PreState>,
}

fn scene(text: &str) -> Scene {
    let root = data_dir();
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    let fixture = parse_fixture(text);
    let builder = Builder::new(SEED, &resolver);
    let interner = Arc::new(StateInterner::new());
    let surface = SurfaceSystem::new(&settings, &builder, &fixture.canon, &interner);
    let pre_states = fixture
        .pre
        .iter()
        .map(|(&k, name)| (k, PreState::from_name(&interner, name)))
        .collect();
    Scene {
        surface,
        interner,
        fixture,
        pre_states,
    }
}

/// Allocations, probes and rewrites for one whole-chunk `build_surface` call.
struct Run {
    allocs: u64,
    probes: u64,
    rewrites: usize,
    distinct_results: usize,
}

fn one_chunk(scene: &Scene) -> Run {
    let probes = Cell::new(0u64);
    let pre_fn = |x: i32, y: i32, z: i32| -> PreState {
        probes.set(probes.get() + 1);
        scene
            .pre_states
            .get(&(x, y, z))
            .copied()
            .unwrap_or(PreState::AIR)
    };
    let hm_fn = |x: i32, z: i32| -> i32 { *scene.fixture.hm.get(&(x, z)).expect("heightmap") };
    let biome_at = |_x: i32, _z: i32| -> (&str, bool) { (scene.fixture.biome.as_str(), false) };

    let (diff, allocs) = allocs_of(|| {
        scene.surface.build_surface(
            &pre_fn,
            &hm_fn,
            &biome_at,
            scene.fixture.chunk_x * 16,
            scene.fixture.chunk_z * 16,
        )
    });

    // The only place in the tree that iterates a `SurfaceDiff`. `SurfaceDiff` is
    // a `FastMap`, whose iteration order is not stable across commits, so this
    // takes the *second* form its doc allows: the consumer imposes a total order
    // of its own. The `sort_unstable` is therefore load-bearing, not tidiness —
    // without it this count would be order-dependent on a non-deterministic
    // hasher. It is also only ever reduced to a length, never compared.
    let mut distinct: Vec<_> = diff.values().copied().collect();
    distinct.sort_unstable();
    distinct.dedup();
    Run {
        allocs,
        probes: probes.get(),
        rewrites: diff.len(),
        distinct_results: distinct.len(),
    }
}

/// The bound. The residual is the diff map's own growth series and nothing else:
/// two independent instruments agree on that — the sampled backtrace table
/// attributes 100% of the post-U21 surface allocations to
/// `build_surface`'s `FastMap`, and the **unsampled** size histogram shows
/// exactly 14 distinct sizes (76, 144, 280, 552, 1096, 2184, 4360, 8712, 17416,
/// 34824, 69640, 139272, 278536, 557064 bytes — a doubling series for a 16-byte
/// entry) each occurring once per stage entry.
///
/// So the expected value is ⌈log2(rewrites)⌉-ish, ~14 for a full chunk, and the
/// bound is set at 64 to leave room for a fixture with more rewrites without
/// making the gate a false alarm. That headroom is deliberate and it is *why*
/// the per-probe assertion below carries the real discriminating power.
const PER_CHUNK_BOUND: u64 = 64;

#[test]
fn a_whole_chunk_of_surface_rules_allocates_a_bounded_amount_not_one_per_probe() {
    for (label, text) in [
        ("ocean", include_str!("support/surface_plains_jvm.txt")),
        ("land", include_str!("support/surface_plains_land_jvm.txt")),
    ] {
        let scene = scene(text);
        // Warm: the first call resolves nothing new, but it does let any lazy
        // one-time setup inside the density/noise machinery happen outside the
        // measured window. U19's scar is a "warm" arm that was warm only because
        // an earlier arm's object was still alive, so this is a real prior call
        // on the same object, not a hope.
        let warm = one_chunk(&scene);
        let run = one_chunk(&scene);

        // Non-degeneracy, before any bound is believed. A fixture that scanned
        // nothing, or that no rule ever fired on, would satisfy the bound for
        // want of work.
        assert!(
            run.probes > 20_000,
            "[{label}] only {} pre-surface probes — this fixture is not scanning a \
             whole chunk, so a bound on its allocations means nothing",
            run.probes
        );
        assert!(
            run.rewrites > 0,
            "[{label}] the surface rule rewrote nothing; no result state was ever \
             produced, so this scene cannot show anything about how they are carried"
        );
        assert!(
            run.distinct_results >= 4,
            "[{label}] only {} distinct result state(s) in the diff — the rule tree \
             is barely branching, so `Rule::Block` variety is untested here",
            run.distinct_results
        );

        // The magnitude. Both hypotheses are computed from *this run's* measured
        // probe count, not from a remembered constant.
        //
        //  * H_string — the pre-U21 representation allocated a `String` for
        //    every probe (`pre`, 77.08% of the stage) plus one per matched rule
        //    (`try_apply`, 21.92%), so it predicts at least `probes`.
        //  * H_id — nothing is allocated per probe or per match; only the diff
        //    map's growth series is, i.e. O(log rewrites).
        let h_string = run.probes;
        assert!(
            run.allocs <= PER_CHUNK_BOUND,
            "[{label}] surface allocations regressed: {} for {} probes and {} \
             rewrites.\n  H_id     (ids carried across the seam) predicts \
             ~{} — the diff map's growth series, ⌈log2({})⌉ doublings\n  \
             H_string (a String per probe, the pre-U21 shape) predicts >= {}\n  \
             measured {} allocations = {:.4} per 1000 probes; the id \
             representation reads well under 1 and the string one ~800.\n  \
             The measurement has landed on H_string, not H_id.",
            run.allocs,
            run.probes,
            run.rewrites,
            run.rewrites.max(1).ilog2() + 1,
            run.rewrites,
            h_string,
            run.allocs,
            1000.0 * run.allocs as f64 / run.probes as f64,
        );

        // And the same call twice must cost the same: an allocation that happens
        // only on the first call is a one-time table build, which is fine, but
        // one that happens on *every* call and merely looks small on this
        // fixture is not.
        assert_eq!(
            warm.probes, run.probes,
            "[{label}] two identical calls probed different numbers of positions"
        );

        println!(
            "surface allocations [{label}]: {} allocs over {} probes / {} rewrites \
             / {} distinct result states ({:.4} per 1000 probes)",
            run.allocs,
            run.probes,
            run.rewrites,
            run.distinct_results,
            1000.0 * run.allocs as f64 / run.probes as f64,
        );
    }
}

/// The precise statement of *why* the count above is bounded, and the one that
/// fails the instant an `id_of` returns to this path.
///
/// `StateInterner::len` is the observable: a scan that resolved a string would
/// have to intern it (or hit the table, taking the `RwLock` this conversion
/// exists to keep off a path shared by ~289 concurrent generator calls —
/// `4307b59`'s scar). Interning is also the failure mode a naive "return
/// `StateId` from `pre`" fix would have had: it would have moved the cost from
/// `String` to `id_of` rather than removing it, and the allocation counter alone
/// would have looked *better* while the lock traffic got worse.
#[test]
fn nothing_is_interned_during_a_surface_scan() {
    for (label, text) in [
        ("ocean", include_str!("support/surface_plains_jvm.txt")),
        ("land", include_str!("support/surface_plains_land_jvm.txt")),
    ] {
        let scene = scene(text);
        let before = scene.interner.len();
        assert!(
            before > 1,
            "[{label}] the interner holds only {before} state(s) after construction — \
             the surface rule's result states were not pre-interned, so this test \
             would pass by having nothing to intern"
        );
        let run = one_chunk(&scene);
        assert_eq!(
            scene.interner.len(),
            before,
            "[{label}] a surface scan interned {} new state(s) over {} probes. \
             Every state this engine can emit is resolved once in \
             `SurfaceSystem::new`; anything interned here is a string being \
             resolved inside the scan, which is the cost up-front resolution removed.",
            scene.interner.len() - before,
            run.probes
        );
        println!(
            "surface interning [{label}]: {before} states pre-interned, 0 new over {} probes",
            run.probes
        );
    }
}
