//! Counter gate for U4's corner hoist: the *lookup* win, against predictions
//! derived from the cell geometry and the compiled graph's own structure.
//!
//! # Why this is its own binary
//!
//! The counters are process-global atomics, so a counter assertion sharing a
//! binary with any other test that touches the generator reads that test's
//! traffic too. This repo has already been bitten by exactly that (a shared
//! binary read 502 against a true 256), so this file contains **one** counting
//! test and nothing else.
//!
//! # What the hoist changes, and what it must not
//!
//! `interpolated` samples eight cell corners and trilinearly interpolates. U4
//! caches those eight values **per cell** rather than fetching them per block.
//! There are two independent quantities, and a plausible-looking implementation
//! can get one right and the other wrong, so both are asserted:
//!
//! | quantity | prediction | should the hoist change it? |
//! |---|---|---|
//! | corner **lookups** | `8 × cell_fills` | yes — that is the win |
//! | corner **evaluations** | the `5 × 49 × 5 = 1,225` lattice per entered node | **no** |
//!
//! Dropping the per-slot memo beneath the cell cache would leave lookups at the
//! winning number while quintupling evaluations, because adjacent cells share
//! corners.
//!
//! # The premise this test had to be corrected on
//!
//! The first version asserted the real `final_density` contained **one**
//! `interpolated` node and derived `768 × 8 = 6,144` from that. It contains
//! **five**, and only **two** are entered per block — the rest sit behind a `mul`
//! short-circuit or an untaken `range_choice` branch. That premise check fired on
//! its first run, and without it this file would have asserted a wrong literal
//! and then been "fixed" by relaxing it. So every prediction below is derived
//! from a *measured* structural fact (`interpolated` evaluations per block)
//! rather than from reading the router, and the structural facts are asserted
//! separately from the counter facts.

#![cfg(feature = "gen-counters")]

use std::path::{Path, PathBuf};

use lodestone_worldgen::counters;
use lodestone_worldgen::density::{Builder, NoiseChunkSampler, NoiseParams, Resolver};
use lodestone_worldgen::engine::Program;
use serde_json::Value;

const SEED: i64 = 42;
const CELL_WIDTH: i32 = 4;
const CELL_HEIGHT: i32 = 8;
const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// `16 × 16 × 384`.
const BLOCKS_PER_CHUNK: u64 = 16 * 16 * HEIGHT as u64;
/// `(16/4) × (384/8) × (16/4)` — the cells one chunk column touches.
const CELLS_PER_CHUNK: u64 =
    (16 / CELL_WIDTH) as u64 * (HEIGHT / CELL_HEIGHT) as u64 * (16 / CELL_WIDTH) as u64;
/// `5 × 49 × 5` — the distinct corner lattice for one chunk-bounded slot, which
/// is also vanilla's own `fillSlice` accounting: 245 per X-plane over 5 planes.
const CORNER_LATTICE: u64 =
    (16 / CELL_WIDTH + 1) as u64 * (HEIGHT / CELL_HEIGHT + 1) as u64 * (16 / CELL_WIDTH + 1) as u64;
/// Blocks in one `4 × 8 × 4` cell — the factor the hoist is worth.
const BLOCKS_PER_CELL: u64 = 4 * 8 * 4;

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

#[test]
fn corner_lookups_and_evaluations_match_the_cell_geometry() {
    // Geometry, as arithmetic, so every literal below has a visible origin.
    assert_eq!(BLOCKS_PER_CHUNK, 98_304);
    assert_eq!(CELLS_PER_CHUNK, 768);
    assert_eq!(CORNER_LATTICE, 1_225);
    assert_eq!(BLOCKS_PER_CELL, 128);
    assert_eq!(
        BLOCKS_PER_CHUNK / CELLS_PER_CHUNK,
        BLOCKS_PER_CELL,
        "blocks per chunk over cells per chunk must be blocks per cell"
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();

    let builder = Builder::new(SEED, &resolver);
    let final_density = builder
        .build(&settings["noise_router"]["final_density"])
        .expect("bundled final_density density-function document");

    // --- premise: the graph is real and does interpolate ------------------
    // Without this, a stub or a mis-rooted tree would measure zero lookups and
    // "0" would read as a spectacular win rather than as nothing happening.
    let program = Program::compile(&final_density);
    let interpolated_nodes = program.count_kind("interpolated");
    let structural_slots = program.interpolating_slots();
    assert!(
        program.node_count() > 100,
        "the compiled router should be a real graph, not a stub; got {} nodes",
        program.node_count()
    );
    assert!(
        interpolated_nodes >= 1,
        "no `interpolated` node in the compiled final_density — every \
         interpolation assertion below would be vacuous"
    );
    assert!(
        !structural_slots.is_empty(),
        "no `interpolated` node is reachable in an interpolating context, so \
         nothing would ever fill a cell"
    );

    let sampler = NoiseChunkSampler::new_bounded(
        final_density,
        builder.slot_count(),
        CELL_WIDTH,
        CELL_HEIGHT,
        (0, 15),
        (MIN_Y, MIN_Y + HEIGHT - 1),
        (0, 15),
    );

    counters::reset();
    // `overworld/fill.rs`'s exact nesting: lz outer, lx, ly innermost. The order
    // is load-bearing — a per-cell cache's hit rate depends on it, and measuring
    // in a convenient cell-major order would flatter an implementation the real
    // fill loop would not benefit from. Specifically the innermost axis is Y and
    // a cell spans only 8 of those, so a *single* last-cell memo would be evicted
    // 12,288 times per node per chunk and measure 8x worse than the assertion
    // below allows.
    let mut queried = 0u64;
    for lz in 0..16i32 {
        for lx in 0..16i32 {
            for ly in 0..HEIGHT {
                std::hint::black_box(sampler.final_density(lx, MIN_Y + ly, lz));
                queried += 1;
            }
        }
    }
    let s = counters::snapshot();
    assert_eq!(queried, BLOCKS_PER_CHUNK, "the sweep must cover the chunk");

    // --- the measured structural fact every prediction rests on -----------
    // How many `interpolated` nodes are actually entered per block. Measured, not
    // read off the router: the structural walk finds five reachable slots but
    // only some are entered, because `mul` short-circuits and `range_choice`
    // takes one branch per position.
    let interp_evals = s.density_evals[17];
    assert_eq!(
        interp_evals % BLOCKS_PER_CHUNK,
        0,
        "`interpolated` evaluations ({interp_evals}) should be a whole number of \
         nodes per block over {BLOCKS_PER_CHUNK} blocks; a remainder means some \
         blocks entered a different set of nodes than others and the \
         nodes-times-cells derivation below does not hold"
    );
    let interp_per_block = interp_evals / BLOCKS_PER_CHUNK;
    assert_eq!(
        interp_per_block,
        2,
        "the real overworld final_density enters {interp_per_block} `interpolated` \
         nodes per block; this gate was calibrated at 2 (of {} structurally \
         reachable slots {structural_slots:?}, {interpolated_nodes} nodes in the \
         data). If the router or the transparency rule changed, re-derive the \
         predictions below rather than adjusting this number to match them.",
        structural_slots.len()
    );

    // --- prediction 1: the lookup win ------------------------------------
    // Two hypotheses, both computed from the fact above, 128x apart — the blocks
    // in a cell. A prediction, not a direction-of-change assertion.
    let hypothesis_no_hoist = interp_evals * 8;
    let hypothesis_hoist = interp_per_block * CELLS_PER_CHUNK * 8;
    assert_eq!(hypothesis_no_hoist, 1_572_864, "8 per block, per node entered");
    assert_eq!(hypothesis_hoist, 12_288, "8 per cell, per node entered");
    assert_eq!(
        hypothesis_no_hoist / hypothesis_hoist,
        BLOCKS_PER_CELL,
        "the two hypotheses must sit a whole cell apart, or this is a tolerance"
    );
    assert_eq!(
        s.corner_lookups,
        hypothesis_hoist,
        "corner lookups measured {}. Hoist hypothesis {hypothesis_hoist}, \
         no-hoist hypothesis {hypothesis_no_hoist}. {}",
        s.corner_lookups,
        if s.corner_lookups == hypothesis_no_hoist {
            "This is exactly the no-hoist number: the per-cell corner cache is \
             not being consulted."
        } else if s.corner_lookups == interp_evals {
            "This is one cell fill per 8 blocks — the signature of a single \
             last-cell memo instead of a per-cell cache, evicted by the fill \
             loop's Y-innermost order."
        } else {
            "Neither hypothesis; the geometry or the traversal changed and the \
             derivation needs revisiting."
        }
    );

    // --- prediction 2: lookups are mechanically 8 per cell fill -----------
    assert_eq!(
        s.corner_lookups,
        s.cell_fills * 8,
        "every cell fill assembles exactly 8 corners: {} lookups over {} fills",
        s.corner_lookups,
        s.cell_fills
    );
    assert_eq!(
        s.cell_fills,
        interp_per_block * CELLS_PER_CHUNK,
        "each of the {interp_per_block} entered `interpolated` nodes should fill \
         all {CELLS_PER_CHUNK} of the chunk's cells exactly once"
    );

    // --- prediction 3: the evaluation count the hoist must NOT change -----
    assert_eq!(
        s.corner_evals,
        interp_per_block * CORNER_LATTICE,
        "corner evaluations measured {}; the distinct corner lattice is \
         {CORNER_LATTICE} (5 x 49 x 5) per entered node, so {} expected. A larger \
         number means the per-slot memo beneath the cell cache stopped \
         deduplicating the corners adjacent cells share; {} would be one \
         evaluation per lookup, i.e. no memo at all.",
        s.corner_evals,
        interp_per_block * CORNER_LATTICE,
        s.corner_lookups
    );
    assert!(
        s.corner_evals < s.corner_lookups,
        "corner evaluations ({}) must be strictly fewer than lookups ({}) — \
         adjacent cells share corners, so equality means the memo is dead",
        s.corner_evals,
        s.corner_lookups
    );

    println!(
        "U4 corner hoist, chunk (0,0) seed {SEED}, {interp_per_block} interpolated \
         nodes entered per block:\n  \
         corner lookups     {:>9}  (no-hoist hypothesis {hypothesis_no_hoist}, {}x fewer)\n  \
         cell fills         {:>9}  = {interp_per_block} x {CELLS_PER_CHUNK}\n  \
         corner evaluations {:>9}  = {interp_per_block} x {CORNER_LATTICE} lattice, unchanged by design\n  \
         slot hits          {:>9}   slot misses {}",
        s.corner_lookups,
        hypothesis_no_hoist / s.corner_lookups.max(1),
        s.cell_fills,
        s.corner_evals,
        s.slot_hits,
        s.slot_misses,
    );
}
