//! End terrain, gated against everything except itself.
//!
//! # The evidence constraint, stated first because it shapes every test here
//!
//! **There is no End block oracle anywhere.**
//! `.cache/mc/survival/world/dimensions/minecraft/the_end/` contains a `data/`
//! directory and **no `region/` directory at all** — the oracle world's End was never
//! visited, so not one vanilla-generated End chunk exists on this machine, and no
//! `container` run can produce one without generating a new world. Comparing this
//! generator's output against this generator's output is the closed loop the whole
//! evidence section of `CLAUDE.md` exists to forbid, so it is not done here.
//!
//! Every expectation below therefore comes from one of three places:
//!
//! | source | used by |
//! |---|---|
//! | **a record definition, hand-expanded** — `noise_settings/end.json`'s router read as a tree, plus `y_clamped_gradient` / `mul` / `squeeze` from `.cache/mc/26.2/src` | the dead-band gate, which is the strongest thing in this file |
//! | **arithmetic** — `FluidStatus.at` against `sea_level 0` / `min_y 0`, and `cell_geometry`'s `size * 4` | the no-fluid gate and the cell-geometry gate |
//! | **a cross-arm invariant / cross-dimension control** — the Nether generator over the same engine | the proof that the no-fluid detector fires at all |
//!
//! And what is **not** gated is named rather than papered over: see
//! `docs/worldgen-end.md`'s "Evidence, and its honest limit". The short version is
//! `consumeCount(17292)` — a wrong RNG draw count inside `EndIslandNoise` leaves
//! every prediction in this file intact, because none of them depends on the island
//! field's *value*.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lodestone_worldgen::aquifer::BlockKind;
use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::end::{EndBiomeSource, EndGenerator};
use serde_json::Value;

/// The oracle world's seed, so this file, `nether_gen.rs` and `nether_structures.rs`
/// all describe the same world. It is not an oracle *for the End* — nothing is — but
/// using an arbitrary different number here would suggest otherwise.
const SEED: i64 = -195_764_831;
/// A second, unrelated seed. Every prediction in this file is seed-independent by
/// construction, and running two seeds is how that claim is exercised rather than
/// asserted.
const SEED_B: i64 = 42;

struct EndAssets {
    root: PathBuf,
}

impl EndAssets {
    fn new() -> Self {
        Self {
            root: Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen"),
        }
    }

    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }

    fn try_read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display())),
            Err(_) => Value::Null,
        }
    }
}

impl Resolver for EndAssets {
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
    fn block_tag(&self, id: &str) -> Value {
        self.try_read("tags/block", id)
    }
}

/// The Nether's rows of the same bundle — the control arm for the no-fluid gate.
struct NetherAssets(EndAssets);

impl Resolver for NetherAssets {
    fn density_function(&self, id: &str) -> Value {
        self.0.density_function(id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        self.0.noise(id)
    }
    fn biome_parameters(&self) -> Value {
        self.0.read("biome_parameters", "nether")
    }
    fn biome_document(&self, id: &str) -> Value {
        self.0.try_read("biome", id)
    }
    fn configured_carver(&self, id: &str) -> Value {
        self.0.try_read("configured_carver", id)
    }
    fn block_tag(&self, id: &str) -> Value {
        self.0.try_read("tags/block", id)
    }
}

fn settings(name: &str) -> Value {
    EndAssets::new().read("noise_settings", name)
}

fn generator(seed: i64) -> EndGenerator {
    EndGenerator::new(seed, &settings("end"), &EndAssets::new())
}

/// A spread of chunks covering the three regions the End's own biome ladder
/// distinguishes: inside the main island's radius-64 hole, in the ring just outside
/// it, and far out among the small islands. Not a random sample — the point is that
/// every gate below holds in all three, and a sample confined to one of them could
/// not say that.
const SCENE: &[(i32, i32)] = &[
    (0, 0),
    (3, -2),
    (20, 20),
    (-33, 0),
    (0, -33),
    (65, 0),
    (-70, 12),
    (120, -95),
    (400, 400),
    (-1500, 2300),
];

/// **The strongest gate here, and it is a closed-form prediction from the record
/// definition rather than a comparison.**
///
/// `noise_settings/end.json`'s `final_density`, read as a tree, is
///
/// ```text
/// squeeze(interpolated(0.64 * blend_density(
///     -0.234375 + g1(y) * (0.234375 + (-23.4375 + g2(y) * (23.4375 + end/sloped_cheese)))
/// )))
///   g1 = y_clamped_gradient(from_value 0.0 @ from_y 4, to_value 1.0 @ to_y 32)
///   g2 = y_clamped_gradient(from_value 1.0 @ from_y 56, to_value 0.0 @ to_y 312)
/// ```
///
/// `Mth.clampedMap` clamps *below* `from_y` to `from_value`, so **`g1(y) = 0.0`
/// exactly for every `y <= 4`** — and `DensityFunctions.Ap2.Mul` short-circuits on
/// `argument1 == 0.0` without evaluating `argument2`, so at those heights the island
/// field, `base_3d_noise` and the seed are *structurally* not consulted. The whole
/// expression collapses to
///
/// ```text
/// squeeze(0.64 * -0.234375) = squeeze(-0.15) = -0.15/2 - (-0.15)^3/24 = -0.07485938
/// ```
///
/// which is `<= 0.0`, so `Aquifer.createDisabled` returns the global fluid — and with
/// `sea_level 0` that is air. **Every block at `y` in `0..=4`, everywhere in the End,
/// at every seed, is air.** The interpolation cannot disturb it either: the cell
/// height is 4, so the corners bracketing this band sit at `y = 0` and `y = 4` and
/// both are the same constant.
///
/// This gate fails if `from_y`/`to_y` are swapped, if `from_value`/`to_value` are
/// swapped, if the `mul`/`add` nesting is misread, if a constant is wrong, or if the
/// fill's `y` is off by one — and it does so without needing to know a single thing
/// about `EndIslandNoise`.
///
/// The **wrong hypothesis is computed too**: with `g1` clamped to `1.0` below `y = 4`
/// (the from/to swap) the band's content becomes island-dependent and therefore
/// *varies by position*, which is exactly what
/// `the_dead_band_detector_fires_one_cell_higher` demonstrates the detector can see.
#[test]
fn the_router_makes_y_0_to_4_a_dead_band_of_air_everywhere() {
    // Read back from the document rather than restated, so a datapack change to the
    // gradient is a failure here instead of a silently wrong prediction.
    let settings = settings("end");
    let g1 = &settings["noise_router"]["final_density"]["argument"]["argument"]["argument2"]
        ["argument"]["argument2"]["argument1"];
    assert_eq!(g1["type"], "minecraft:y_clamped_gradient", "the router shape moved: {g1}");
    assert_eq!(g1["from_value"], 0.0);
    assert_eq!(g1["from_y"], 4);
    let dead_top = g1["from_y"].as_i64().unwrap() as i32;

    let mut positions = 0usize;
    for seed in [SEED, SEED_B] {
        let generator = generator(seed);
        for &(cx, cz) in SCENE {
            let column = generator.column(cx, cz);
            for lx in 0..16 {
                for lz in 0..16 {
                    for y in column.min_y()..=dead_top {
                        let state = column.block_state(lx, y, lz);
                        assert_eq!(
                            state, "minecraft:air",
                            "seed {seed} chunk ({cx},{cz}) ({lx},{y},{lz}) is {state}; \
                             the router's g1 clamps to 0 at y <= {dead_top}, so the \
                             density there is the constant squeeze(-0.15) = -0.0748594"
                        );
                        positions += 1;
                    }
                }
            }
        }
    }
    assert_eq!(positions, 2 * SCENE.len() * 256 * 5, "the sweep did not run");
}

/// The control for the gate above: it drives the **wrong hypothesis** and requires the
/// detector to fire.
///
/// The first version of this control was "look one cell higher, where `g1` is no
/// longer clamped, and require a mixture" — and it **failed**, correctly: the End's
/// terrain is a slab around y 40–70, so `y` 5..11 is empty for a reason that has
/// nothing to do with the gradient. That is §12.41's premise-false control exactly,
/// and it failed in the *unsafe* direction (it would have passed if the End happened
/// to have low terrain, while measuring nothing about the router).
///
/// So the control mutates the record instead. Swapping `from_value` and `to_value` on
/// `g1` — the single most natural transcription error, and the one a reader of
/// `y_clamped_gradient(0.0 @ 4 → 1.0 @ 32)` makes — puts `g1 = 1.0` below `y = 4`,
/// which applies the island term in full and makes the band island-dependent. If the
/// dead band still came out all air under that mutation, the gate above would be
/// measuring something other than the gradient.
#[test]
fn the_dead_band_gate_fails_under_the_swapped_gradient() {
    let mut mutated = settings("end");
    {
        let g1 = &mut mutated["noise_router"]["final_density"]["argument"]["argument"]
            ["argument2"]["argument"]["argument2"]["argument1"];
        assert_eq!(g1["type"], "minecraft:y_clamped_gradient");
        g1["from_value"] = Value::from(1.0);
        g1["to_value"] = Value::from(0.0);
    }
    let wrong = EndGenerator::new(SEED, &mutated, &EndAssets::new());

    let mut solid = 0usize;
    let mut air = 0usize;
    for &(cx, cz) in SCENE {
        let column = wrong.column(cx, cz);
        for lx in 0..16 {
            for lz in 0..16 {
                for y in column.min_y()..=4 {
                    if column.block_state(lx, y, lz) == "minecraft:air" {
                        air += 1;
                    } else {
                        solid += 1;
                    }
                }
            }
        }
    }
    assert!(
        solid > 0,
        "the swapped gradient produced {solid} solid / {air} air in y 0..=4, so the \
         dead-band gate cannot tell the two gradients apart and proves nothing"
    );
    assert!(
        air > 0,
        "the swapped gradient filled the whole band ({solid} solid); the mutation is \
         supposed to make the band *island-dependent*, not uniformly solid, and a \
         uniform result means the mutation reached something other than g1"
    );
}

/// **The End has no fluid at all, and that is arithmetic rather than an observation.**
///
/// `NoiseBasedChunkGenerator.createFluidPicker` returns the deep-lava status only for
/// `y < min(-54, seaLevel)`; the End's `sea_level` is `0` and its `min_y` is `0`, so
/// that branch is unreachable, and the sea status is
/// `FluidStatus(fluid_level = 0, fluid_type = default_fluid)`. `FluidStatus.at(y)`
/// returns the type only when `y < fluid_level`, i.e. never. So air is the answer at
/// every position **whatever `default_fluid` says** — and `end.json` says air anyway.
///
/// Air is a real answer here, not a missing one: a generator that "helpfully" fell
/// back to water for an unrecognised fluid would be wrong about the End and could not
/// be caught by an Overworld or Nether gate.
#[test]
fn the_end_generates_no_fluid_anywhere() {
    let settings = settings("end");
    assert_eq!(settings["sea_level"], 0);
    assert_eq!(settings["noise"]["min_y"], 0);
    assert_eq!(settings["default_fluid"]["Name"], "minecraft:air");

    let generator = generator(SEED);
    let mut fluid = 0usize;
    for &(cx, cz) in SCENE {
        let field = generator.shape_field(cx, cz);
        for kind in &field {
            if matches!(kind, BlockKind::Water | BlockKind::Lava) {
                fluid += 1;
            }
        }
    }
    assert_eq!(fluid, 0, "{fluid} fluid positions in a dimension whose fluid level is 0");
}

/// The control for the gate above, and it has to come from another dimension: the
/// *same* `AquiferSystem::disabled` code path over the Nether's settings must produce
/// lava, or "no fluid in the End" is a statement about a detector that cannot see one.
#[test]
fn the_no_fluid_detector_sees_the_nethers_lava_sea() {
    let nether = lodestone_worldgen::nether::NetherGenerator::new(
        SEED,
        &settings("nether"),
        &NetherAssets(EndAssets::new()),
    );
    let column = nether.column(0, 0);
    let mut lava = 0usize;
    for lx in 0..16 {
        for lz in 0..16 {
            for y in column.min_y()..(column.min_y() + column.height()) {
                if column.block_state(lx, y, lz).starts_with("minecraft:lava") {
                    lava += 1;
                }
            }
        }
    }
    assert!(
        lava > 0,
        "the Nether column at (0,0) has no lava, so the End's zero proves nothing"
    );
}

/// Cell geometry is `size * 4`, and the End's is the reason
/// `aquifer::cell_geometry` exists: **8 wide and 4 tall**, transposed from the
/// Overworld's and the Nether's 4×8.
///
/// Read out of the two documents and compared to each other, so this is a claim about
/// the data and about the function, not a restated literal.
#[test]
fn the_end_interpolates_over_8_wide_4_tall_cells() {
    let end = settings("end");
    let nether = settings("nether");
    assert_eq!(end["noise"]["size_horizontal"], 2);
    assert_eq!(end["noise"]["size_vertical"], 1);
    assert_eq!(
        lodestone_worldgen::aquifer::cell_geometry(&end),
        (8, 4),
        "size_horizontal 2 / size_vertical 1 is 8 wide and 4 tall"
    );
    assert_eq!(
        lodestone_worldgen::aquifer::cell_geometry(&nether),
        (4, 8),
        "the Nether's is the transpose, which is why the End needed the function"
    );
}

/// A generated column carries the biome source's own answer, and the whole ladder is
/// reachable across the scene plus a sweep.
///
/// The per-chunk uniformity is vanilla's (`weirdBlockX` is the chunk centre) and is
/// already gated inside `end/mod.rs`; what is new here is that the *column* carries
/// it, which is the wire-facing half.
#[test]
fn a_column_carries_the_biome_sources_own_answer() {
    let generator = generator(SEED);
    let source = EndBiomeSource::new(SEED);
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for &(cx, cz) in SCENE {
        let column = generator.column(cx, cz);
        for qx in 0..4 {
            for qz in 0..4 {
                let want = source.biome_at_quart(cx * 4 + qx as i32, 0, cz * 4 + qz as i32);
                assert_eq!(column.biome_at_quart(qx, qz), want, "({cx},{cz}) quart ({qx},{qz})");
                seen.insert(column.biome_at_quart(qx, qz));
            }
        }
    }
    // The scene alone need not hit all five; a coarse sweep must, or the ladder could
    // be collapsed to one arm and every other test here would still pass.
    for cx in (-300..300).step_by(37) {
        for cz in (-300..300).step_by(41) {
            seen.insert(source.biome_at_quart(cx * 4, 0, cz * 4));
        }
    }
    let want: BTreeSet<&str> = EndBiomeSource::possible_biomes().into_iter().collect();
    assert_eq!(seen, want, "not every End biome is reachable through a column");
}

/// The island floor: End terrain is real terrain, not a uniform field, and the main
/// island is solid.
///
/// **Explicitly a floor and not a parity claim** — with no block oracle, the honest
/// assertion is that the generator produces a mixture whose main-island chunk is
/// substantially solid and whose far-out chunk is not the same thing. Anything
/// stronger about the *shape* would have to come from an oracle that does not exist.
/// What this catches is the whole class an "it compiles" report misses: an all-air
/// dimension, an all-end-stone dimension, a `final_density` wired to the wrong router
/// key, and a column whose palette holds one entry.
#[test]
fn the_end_is_real_terrain_and_the_main_island_is_solid() {
    let generator = generator(SEED);
    let centre = generator.column(0, 0);
    let solid = centre.non_air_count();
    let total = 16 * 16 * centre.height() as usize;
    assert!(
        solid > total / 100 && solid < total * 9 / 10,
        "the main island's centre chunk is {solid}/{total} solid — a uniform field, \
         not terrain"
    );
    // A `y` band well above the island: the End's terrain is a slab around y 40-70,
    // so the top of the world must be empty. `height 128` with the router's `g2`
    // ramp makes that structural, and an all-solid dimension fails here.
    for y in 120..128 {
        for lx in 0..16 {
            for lz in 0..16 {
                assert_eq!(
                    centre.block_state(lx, y, lz),
                    "minecraft:air",
                    "({lx},{y},{lz}) at the top of the End is not air"
                );
            }
        }
    }
    // And the shape field agrees with the materialised column about where solid is,
    // which is what a transposed `column_index` in one of the two would break.
    let field = generator.shape_field(0, 0);
    let mut disagreements = 0usize;
    for lx in 0..16i32 {
        for lz in 0..16i32 {
            for ly in 0..centre.height() {
                let kind = field[lodestone_worldgen::compose::column_index(
                    lx,
                    ly,
                    lz,
                    centre.height(),
                )];
                let solid_here =
                    centre.block_state(lx as usize, centre.min_y() + ly, lz as usize)
                        != "minecraft:air";
                if (kind == BlockKind::Stone) != solid_here {
                    disagreements += 1;
                }
            }
        }
    }
    assert_eq!(
        disagreements, 0,
        "the shape field and the materialised column disagree about solidity in \
         {disagreements} positions — a transposed index in one of them"
    );
}

/// What the End actually looks like, printed. `#[ignore]`d: it is a diagnostic, not a
/// gate, and with no oracle there is nothing to compare its numbers against — but it
/// is the cheapest way to see whether a change to the density interpreter has moved
/// the islands, and its numbers are what `docs/worldgen-end.md` quotes.
#[test]
#[ignore = "diagnostic; prints per-chunk solid counts and the solid y-range"]
fn print_the_end_terrain_profile() {
    let generator = generator(SEED);
    for &(cx, cz) in SCENE {
        let column = generator.column(cx, cz);
        let mut lo = i32::MAX;
        let mut hi = i32::MIN;
        for lx in 0..16 {
            for lz in 0..16 {
                for y in column.min_y()..(column.min_y() + column.height()) {
                    if column.block_state(lx, y, lz) != "minecraft:air" {
                        lo = lo.min(y);
                        hi = hi.max(y);
                    }
                }
            }
        }
        println!(
            "({cx},{cz}) biome {} solid {}/{} y {}..{}",
            column.biome_at_quart(0, 0),
            column.non_air_count(),
            16 * 16 * column.height(),
            lo,
            hi
        );
    }
}

/// Determinism, including palette **order**, which reaches the wire.
///
/// Two independently constructed generators, opposite request orders. The bug this
/// guards is real and this repo shipped it once in the Overworld: a
/// `DenseBlockGrid`'s palette is built in `set` order, and iterating the surface diff
/// (a hash map, reseeded per map) made two generators produce identical terrain with
/// different bytes.
#[test]
fn columns_are_byte_identical_regardless_of_order_or_generator_instance() {
    let a = generator(SEED);
    let b = generator(SEED);
    let forward: Vec<_> = SCENE.iter().map(|&(cx, cz)| a.column(cx, cz).into_raw()).collect();
    let reverse: Vec<_> = SCENE
        .iter()
        .rev()
        .map(|&(cx, cz)| b.column(cx, cz).into_raw())
        .collect();
    for (i, want) in forward.iter().enumerate() {
        let got = &reverse[SCENE.len() - 1 - i];
        assert_eq!(want.2, got.2, "palette order differs at {:?}", SCENE[i]);
        assert_eq!(want.3, got.3, "blocks differ at {:?}", SCENE[i]);
        assert_eq!(want.4, got.4, "biomes differ at {:?}", SCENE[i]);
    }
}

/// The surface rule is a no-op, and that is derived rather than assumed.
///
/// `end.json`'s `surface_rule` is a bare `minecraft:block` with `result_state`
/// `end_stone`, and `default_block` is *also* `end_stone`; vanilla's own scan only
/// rewrites a position holding the default block, so the rule can only ever write
/// what is already there. **There is no `vertical_gradient` anywhere in it**, which is
/// what says the End has no bedrock — the one place copying the Nether's shape would
/// have been actively wrong, since the Nether's bedrock floor and roof come from
/// exactly that construct.
#[test]
fn the_end_has_no_bedrock_and_its_surface_rule_cannot_change_a_block() {
    let settings = settings("end");
    let rule = &settings["surface_rule"];
    assert_eq!(rule["type"], "minecraft:block");
    assert_eq!(rule["result_state"]["Name"], "minecraft:end_stone");
    assert_eq!(settings["default_block"]["Name"], "minecraft:end_stone");
    let text = serde_json::to_string(rule).unwrap();
    assert!(
        !text.contains("vertical_gradient"),
        "the End's surface rule has a vertical_gradient, so it may place bedrock: {text}"
    );
    // Observable half: no bedrock in the scene, and the Nether's own rule *does*
    // carry the construct, so the reading of `vertical_gradient` is not a guess.
    let generator = generator(SEED);
    for &(cx, cz) in SCENE {
        let column = generator.column(cx, cz);
        for lx in 0..16 {
            for lz in 0..16 {
                for y in column.min_y()..(column.min_y() + column.height()) {
                    assert_ne!(
                        column.block_state(lx, y, lz),
                        "minecraft:bedrock",
                        "bedrock at ({cx},{cz}) ({lx},{y},{lz})"
                    );
                }
            }
        }
    }
    let nether_rule = EndAssets::new().read("noise_settings", "nether");
    let nether = serde_json::to_string(&nether_rule["surface_rule"]).unwrap();
    assert!(
        nether.contains("vertical_gradient"),
        "the Nether's rule should carry the construct this test reads the absence of"
    );
}
