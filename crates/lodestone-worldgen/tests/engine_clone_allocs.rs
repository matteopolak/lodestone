//! Diagnostic D3, measured: the per-chunk router-tree clones allocate nothing.
//!
//! `build_aquifer` runs once per chunk and used to `.clone()` eight `Density`
//! trees. `Density` is a `Box`-linked enum whose every node is 232 bytes wide
//! (`engine::graph::tests::density_node_is_much_wider_than_an_op`), so each clone
//! was a recursive deep copy with one allocation per node. U4 changed the field
//! types to `Program` (`Arc<Graph>` + a root index) and `Arc<Density>`, making
//! every one of those clones a refcount bump.
//!
//! # Why this is its own binary, and why the counter is thread-local
//!
//! It installs a `#[global_allocator]`, which is per-binary — so it has to be its
//! own test binary, and it contains exactly one test. The counter is
//! **thread-local** rather than global because the test harness allocates on its
//! own thread while the test runs, and a global counter would fold that noise
//! into a measurement whose expected value is exactly zero. `const`-initialised,
//! because a lazily-initialised `thread_local!` allocates on first touch and
//! would recurse through the allocator that is touching it.
//!
//! # Both hypotheses, computed from outside this code
//!
//! An `== 0` assertion is worthless without evidence the instrument fires, and
//! "allocations went down" is worthless without knowing what the old number was.
//! So the test measures **both** arms in one process:
//!
//! | arm | expectation |
//! |---|---|
//! | clone the compiled `Program` / `Arc<Density>` (post-U4) | exactly **0** allocations |
//! | clone the underlying `Density` tree (pre-U4) | hundreds per clone |
//!
//! The second arm is the control *and* the pre-U4 baseline: it is the literal
//! operation `build_aquifer` used to perform, still reachable because
//! `Builder::build` still returns a plain `Density`.

// The counting allocator below needs `unsafe impl GlobalAlloc`, and the workspace
// sets `unsafe_code = "deny"`. Same exemption, and the same reason, as
// `benches/generation.rs`'s `CountingAllocator`: there is no safe way to observe
// real allocation counts, and an allocation claim asserted from structure rather
// than measured is exactly the kind this repo has had to retract before. The
// unsafety is confined to two forwarding calls into `System`.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lodestone_worldgen::density::{Builder, Density, NoiseParams, Resolver};
use lodestone_worldgen::engine::Program;
use serde_json::Value;

thread_local! {
    /// `const`-initialised so touching it never allocates — see the module doc.
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with`, not `with`: during thread teardown the TLS is gone, and a
        // panic from inside the allocator is not recoverable.
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Runs `f` and returns how many allocations it made on this thread.
fn allocs_of<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let before = ALLOCS.with(Cell::get);
    let out = f();
    let after = ALLOCS.with(Cell::get);
    (out, after - before)
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

/// The eight router routes `build_aquifer` clones per chunk, in its order.
const ROUTES: [&str; 8] = [
    "final_density",
    "erosion",
    "depth",
    "barrier",
    "fluid_level_floodedness",
    "fluid_level_spread",
    "lava",
    "preliminary_surface_level",
];

#[test]
fn cloning_the_per_chunk_router_trees_allocates_nothing() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    let router = &settings["noise_router"];

    let builder = Builder::new(42, &resolver);
    let trees: Vec<Density> = ROUTES.iter().map(|r| builder.build(&router[r])).collect();
    assert_eq!(trees.len(), 8, "all eight routes must be built");

    // Post-U4 shapes: three compiled programs, five Arc-wrapped point trees —
    // exactly what `AquiferTrees` now holds. Fixed-size **arrays**, not `Vec`s:
    // an array's `Clone` allocates nothing, so the expected value below is an
    // exact zero rather than a zero plus a harness allowance. (The first version
    // of this test used `Vec`s and had to permit 16 allocations of container
    // overhead, which is exactly the sort of fudge that later gets widened.)
    let programs: [Program; 3] = [
        Program::compile(&trees[0]),
        Program::compile(&trees[1]),
        Program::compile(&trees[2]),
    ];
    let shared: [Arc<Density>; 5] = [
        Arc::new(trees[3].clone()),
        Arc::new(trees[4].clone()),
        Arc::new(trees[5].clone()),
        Arc::new(trees[6].clone()),
        Arc::new(trees[7].clone()),
    ];

    const CHUNKS: usize = 8;

    // --- control / pre-U4 baseline: what a deep clone actually costs -------
    // Measured first, so a zero in the real arm cannot be explained by a dead
    // instrument. Both sinks are allocated to capacity *outside* the measurement
    // window so no container growth lands inside it.
    let mut deep_sink: Vec<Density> = Vec::with_capacity(CHUNKS * 8);
    let (_, deep) = allocs_of(|| {
        for _ in 0..CHUNKS {
            for t in &trees {
                deep_sink.push(t.clone());
            }
        }
    });
    assert!(
        deep > 1_000,
        "control: deep-cloning eight router trees {CHUNKS} times allocated only \
         {deep} times. Either the allocator hook is not counting or `Density` \
         stopped being a Box-linked tree; a zero in the measurement below would \
         then prove nothing."
    );

    // --- the measurement: the shapes `build_aquifer` clones today ----------
    let mut cheap_sink: Vec<([Program; 3], [Arc<Density>; 5])> = Vec::with_capacity(CHUNKS);
    let (_, cheap) = allocs_of(|| {
        for _ in 0..CHUNKS {
            cheap_sink.push((programs.clone(), shared.clone()));
        }
    });
    assert_eq!(
        cheap, 0,
        "cloning the compiled/shared trees {CHUNKS} times allocated {cheap} \
         times. A `Program` clone is an Arc bump plus a u32 copy and an \
         `Arc<Density>` clone is a refcount bump, so this must be exactly zero. \
         Deep cloning the same eight trees the same number of times costs {deep}."
    );
    drop(cheap_sink);
    drop(deep_sink);

    // The ratio, stated so the commit's claim has a number behind it rather than
    // a direction.
    let per_chunk_deep = deep / CHUNKS as u64;
    println!(
        "D3, per chunk: deep-cloning the eight router trees costs {per_chunk_deep} \
         allocations ({deep} over {CHUNKS} chunks); cloning the compiled/shared \
         form costs exactly {cheap}"
    );

    // --- and the graphs really are shared, not copied ---------------------
    // A `Program` clone that deep-copied the graph would still allocate, so the
    // assertion above already covers it; this pins the *intent* so a future
    // change to `Program`'s Clone has to break a named assertion.
    let p0 = &programs[0];
    let p1 = p0.clone();
    assert_eq!(
        p0.node_count(),
        p1.node_count(),
        "a cloned Program must describe the same graph"
    );
    assert!(
        p0.node_count() > 100,
        "the compiled final_density should be a real graph; got {}",
        p0.node_count()
    );
}
