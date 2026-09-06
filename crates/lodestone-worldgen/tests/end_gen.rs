//! End terrain, including a block-level gate against a separately-run server oracle.
//!
//! # The evidence constraint, stated first because it shapes every test here
//!
//! The committed `support/end_chunk_jvm.txt` fixture is emitted by
//! `scripts/worldgen-oracle/EndChunkOracle.java`, which drives the bundled 26.2
//! server classes directly. It covers the main island, an outer-ring transition,
//! and a distant small-islands biome across two seeds. The test below compares every block
//! and every quart biome from that independent output.
//!
//! The remaining focused expectations come from three complementary places:
//!
//! | source | used by |
//! |---|---|
//! | **a record definition, hand-expanded** — `noise_settings/end.json`'s router read as a tree, plus `y_clamped_gradient` / `mul` / `squeeze` from `.cache/mc/26.2/src` | the dead-band gate, which is the strongest thing in this file |
//! | **arithmetic** — vanilla's own fluid-status accessor against `sea_level 0` / `min_y 0`, and `cell_geometry`'s `size * 4` | the no-fluid gate and the cell-geometry gate |
//! | **a cross-arm invariant / cross-dimension control** — the Nether generator over the same engine | the proof that the no-fluid detector fires at all |
//!
//! The fixture is the guard for the island field and its RNG stream; the focused
//! tests retain structural checks that make a mismatch easier to localize.

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
    fn biome_document(&self, id: &str) -> Value {
        self.try_read("biome", id)
    }
    fn configured_feature(&self, id: &str) -> Value {
        self.try_read("configured_feature", id)
    }
    fn placed_feature(&self, id: &str) -> Value {
        self.try_read("placed_feature", id)
    }
    fn structure_set_ids(&self) -> Vec<String> {
        vec!["minecraft:end_cities".to_owned()]
    }
    fn structure_set(&self, id: &str) -> Value {
        self.read("structure_set", id)
    }
    fn structure(&self, id: &str) -> Value {
        self.read("structure", id)
    }
    fn biome_tag(&self, id: &str) -> Value {
        self.try_read("tags/worldgen/biome", id)
    }
    fn structure_template(&self, id: &str) -> Option<Vec<u8>> {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        std::fs::read(self.root.parent()?.join("structure").join(format!("{name}.nbt"))).ok()
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

/// The fixed End platform comes from the End biome's placed-feature document,
/// not from portal arrival.  It has to appear even when the serving path asks
/// for its chunk before any player crosses a portal.
#[test]
fn fixed_end_platform_is_composed_from_the_biome_feature() {
    let generator = generator(SEED);
    let mut columns = std::collections::BTreeMap::new();
    let mut writes = 0usize;
    for line in include_str!("support/end_platform_jvm.txt").lines() {
        let mut words = line.split_whitespace();
        assert_eq!(words.next(), Some("block"), "malformed platform fixture: {line}");
        let coordinates = words.next().expect("platform coordinates");
        let state = words.next().expect("platform state");
        assert!(words.next().is_none(), "trailing platform fixture data: {line}");
        let mut coordinates = coordinates.split(',');
        let x: i32 = coordinates.next().expect("x").parse().expect("integer x");
        let y: i32 = coordinates.next().expect("y").parse().expect("integer y");
        let z: i32 = coordinates.next().expect("z").parse().expect("integer z");
        assert!(coordinates.next().is_none(), "extra coordinate: {line}");
        let chunk = (x.div_euclid(16), z.div_euclid(16));
        if !columns.contains_key(&chunk) {
            columns.insert(chunk, generator.column(chunk.0, chunk.1));
        }
        let column = columns.get(&chunk).expect("generated platform chunk");
        assert_eq!(
            column.block_state(x.rem_euclid(16) as usize, y, z.rem_euclid(16) as usize),
            state,
            "platform write ({x},{y},{z})"
        );
        writes += 1;
    }
    assert_eq!(writes, 100, "fixture must contain the complete 5x5x4 platform");
}

/// The city fixture is a positive capture from an independently generated End
/// region. It gates the start location, all emitted piece templates, and two
/// post-placement controls through the production [`EndGenerator::column`] path.
#[test]
fn end_city_pieces_are_composed_into_end_columns() {
    let generator = generator(SEED);
    let mut lines = include_str!("support/end_city_jvm.txt").lines().filter(|line| !line.starts_with('#') && !line.is_empty());
    let start = lines.next().expect("city start fixture").split_whitespace().collect::<Vec<_>>();
    assert_eq!(start[0], "start");
    let cx: i32 = start[1].parse().expect("city chunk x");
    let cz: i32 = start[2].parse().expect("city chunk z");
    let origin = [
        start[3].parse::<i32>().expect("city origin x"),
        start[4].parse::<i32>().expect("city origin y"),
        start[5].parse::<i32>().expect("city origin z"),
    ];
    assert_eq!(start[6], "cw90");
    let expected_pieces = &start[7..];

    let starts = generator.structure_starts(cx, cz);
    assert_eq!(starts.len(), 1, "fixture must name one city start");
    let city = &starts[0];
    assert_eq!(city.structure, "minecraft:end_city");
    assert!(city.pieces_complete, "city producer must return real pieces");
    assert_eq!(
        city.pieces
            .first()
            .and_then(|piece| piece.placement.as_ref())
            .map(|placement| placement.position),
        Some(origin),
        "fixture origin is the first template placement position, not its rotated bounding-box minimum",
    );
    let piece_names: Vec<_> = city
        .pieces
        .iter()
        .map(|piece| piece.template.as_deref().expect("city template").rsplit('/').next().expect("template name"))
        .collect();
    assert_eq!(piece_names, expected_pieces, "fixture piece order");

    let column = generator.column(cx, cz);
    for line in lines {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        assert_eq!(fields.len(), 5, "malformed city block fixture: {line}");
        assert_eq!(fields[0], "block");
        let x: i32 = fields[1].parse().expect("integer x");
        let y: i32 = fields[2].parse().expect("integer y");
        let z: i32 = fields[3].parse().expect("integer z");
        assert_eq!(x.div_euclid(16), cx, "fixture control must lie in served city chunk");
        assert_eq!(z.div_euclid(16), cz, "fixture control must lie in served city chunk");
        assert_eq!(column.block_state(x.rem_euclid(16) as usize, y, z.rem_euclid(16) as usize), fields[4], "city control ({x}, {y}, {z})");
    }
}

#[derive(Debug)]
struct OracleRun<'a> {
    x: usize,
    z: usize,
    y: i32,
    count: i32,
    state: &'a str,
}

#[derive(Debug)]
struct OracleCase<'a> {
    seed: i64,
    chunk_x: i32,
    chunk_z: i32,
    min_y: i32,
    height: i32,
    runs: Vec<OracleRun<'a>>,
    biomes: [&'a str; 16],
}

fn parse_oracle() -> Vec<OracleCase<'static>> {
    let mut lines = include_str!("support/end_chunk_jvm.txt").lines().peekable();
    let mut cases = Vec::new();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        assert_eq!(fields.first(), Some(&"case"), "unexpected fixture row: {line}");
        assert_eq!(fields.len(), 6, "malformed case row: {line}");
        let (seed, chunk_x, chunk_z, min_y, height) = (
            fields[1].parse().unwrap_or_else(|e| panic!("invalid fixture seed {}: {e}", fields[1])),
            fields[2].parse().unwrap_or_else(|e| panic!("invalid fixture chunk x {}: {e}", fields[2])),
            fields[3].parse().unwrap_or_else(|e| panic!("invalid fixture chunk z {}: {e}", fields[3])),
            fields[4].parse().unwrap_or_else(|e| panic!("invalid fixture min y {}: {e}", fields[4])),
            fields[5].parse().unwrap_or_else(|e| panic!("invalid fixture height {}: {e}", fields[5])),
        );
        let mut runs = Vec::with_capacity(256);
        while lines.peek().is_some_and(|row| row.starts_with("run ")) {
            let row = lines.next().unwrap();
            let fields: Vec<_> = row.split_whitespace().collect();
            assert_eq!(fields.len(), 6, "malformed run row: {row}");
            runs.push(OracleRun {
                x: fields[1].parse().unwrap_or_else(|e| panic!("invalid fixture x {}: {e}", fields[1])),
                z: fields[2].parse().unwrap_or_else(|e| panic!("invalid fixture z {}: {e}", fields[2])),
                y: fields[3].parse().unwrap_or_else(|e| panic!("invalid fixture y {}: {e}", fields[3])),
                count: fields[4].parse().unwrap_or_else(|e| panic!("invalid fixture run length {}: {e}", fields[4])),
                state: fields[5],
            });
        }
        let mut covered = [0i32; 256];
        for run in &runs {
            assert!(run.x < 16 && run.z < 16, "out-of-range fixture column: {run:?}");
            assert!(run.count > 0, "empty fixture run: {run:?}");
            assert!(run.y >= min_y && run.y + run.count <= min_y + height, "out-of-range fixture run: {run:?}");
            covered[run.z * 16 + run.x] += run.count;
        }
        assert!(covered.iter().all(|&count| count == height), "fixture case ({chunk_x},{chunk_z}) does not cover every column exactly once");
        let mut biomes = [""; 16];
        for _ in 0..16 {
            let row = lines.next().expect("fixture ended before the quart biome grid");
            let fields: Vec<_> = row.split_whitespace().collect();
            assert_eq!(fields.len(), 4, "malformed biome row: {row}");
            let qx: usize = fields[1].parse().unwrap_or_else(|e| panic!("invalid fixture quart x {}: {e}", fields[1]));
            let qz: usize = fields[2].parse().unwrap_or_else(|e| panic!("invalid fixture quart z {}: {e}", fields[2]));
            assert!(qx < 4 && qz < 4, "out-of-range fixture quart: {row}");
            biomes[qz * 4 + qx] = fields[3];
        }
        assert!(biomes.iter().all(|biome| !biome.is_empty()), "incomplete biome grid at ({chunk_x},{chunk_z})");
        cases.push(OracleCase { seed, chunk_x, chunk_z, min_y, height, runs, biomes });
    }
    cases
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

/// A whole-column, independent comparison. `EndChunkOracle` constructs the
/// dimension's native biome source and generator from the bundled server
/// registries, then captures the fill-and-surface output before any gameplay
/// furniture. This catches density routing, interpolation, the island RNG stream,
/// materialization, and quart-biome propagation together.
#[test]
fn end_columns_match_the_independent_server_fixture() {
    let cases = parse_oracle();
    assert_eq!(cases.len(), 3, "the fixture lost a scene");
    assert!(cases.iter().any(|case| case.seed == SEED));
    assert!(cases.iter().any(|case| case.seed == SEED_B));
    assert!(cases.iter().any(|case| case.chunk_x == 0 && case.chunk_z == 0));
    assert!(cases.iter().any(|case| case.chunk_x.abs() > 64 || case.chunk_z.abs() > 64));

    for case in cases {
        let column = generator(case.seed).column(case.chunk_x, case.chunk_z);
        assert_eq!(column.min_y(), case.min_y, "seed {} chunk ({},{})", case.seed, case.chunk_x, case.chunk_z);
        assert_eq!(column.height(), case.height, "seed {} chunk ({},{})", case.seed, case.chunk_x, case.chunk_z);
        for run in case.runs {
            for y in run.y..run.y + run.count {
                assert_eq!(
                    column.block_state(run.x, y, run.z),
                    run.state,
                    "seed {} chunk ({},{}) local ({},{},{})",
                    case.seed,
                    case.chunk_x,
                    case.chunk_z,
                    run.x,
                    y,
                    run.z,
                );
            }
        }
        for qz in 0..4 {
            for qx in 0..4 {
                assert_eq!(
                    column.biome_at_quart(qx, qz),
                    case.biomes[qz * 4 + qx],
                    "seed {} chunk ({},{}) quart ({qx},{qz})",
                    case.seed,
                    case.chunk_x,
                    case.chunk_z,
                );
            }
        }
    }
}

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
/// Vanilla's own math-helper clamped-map clamps *below* `from_y` to `from_value`, so **`g1(y) = 0.0`
/// exactly for every `y <= 4`** — and vanilla's own binary-multiply node short-circuits on
/// `argument1 == 0.0` without evaluating `argument2`, so at those heights the island
/// field, `base_3d_noise` and the seed are *structurally* not consulted. The whole
/// expression collapses to
///
/// ```text
/// squeeze(0.64 * -0.234375) = squeeze(-0.15) = -0.15/2 - (-0.15)^3/24 = -0.07485938
/// ```
///
/// which is `<= 0.0`, so vanilla's own disabled-aquifer constructor returns the global fluid — and with
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
/// Vanilla's own chunk-generator "create fluid picker" returns the deep-lava status only for
/// `y < min(-54, seaLevel)`; the End's `sea_level` is `0` and its `min_y` is `0`, so
/// that branch is unreachable, and the sea status is
/// `FluidStatus(fluid_level = 0, fluid_type = default_fluid)`. Vanilla's own fluid-status accessor at `(y)`
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
