//! The Nether's structure stage reaches blocks — `bastion_remnant` in particular.
//!
//! # What was wrong, and why every other instrument said it was fine
//!
//! `NetherGenerator` composed no structure stage at all: no starts, no references,
//! no beardifier, no place step. `bastion_remnant`'s template pools loaded, its
//! jigsaw assembly was gated against the Overworld's stage, and it was absent from
//! the unsupported ledger's per-structure rows — and it placed **zero blocks
//! anywhere in the game**, because its biome tag is Nether-only and the Overworld's
//! biome filter can therefore never accept it. A closed loop of healthy counters.
//!
//! # What the gates here rest on
//!
//! Not on a block oracle: the vanilla oracle world's Nether region files stop at
//! `Status: minecraft:full` chunks whose structures this generator does not claim to
//! reproduce block-for-block, and `nether_gen.rs` already carries the two
//! comparisons that *are* against vanilla's own bytes (17,856 biome quarts and
//! 20,480 bedrock positions).
//!
//! These gates are the established structure shape instead — **a structure places
//! its own blocks at a real start, and zero in a structure-free control over
//! identical data** — plus two expectations that come from outside this crate:
//!
//! * the *set* of structure sets a Nether registry may hold is recomputed here from
//!   the bundled `structure/*.json` `biomes` tags against the bundled
//!   `biome_parameters/nether.json`, i.e. from the data rather than from the
//!   registry, and both arms of the inclusion are required to be non-empty;
//! * the discriminating block names are checked to be absent from the Nether's own
//!   `surface_rule`, so "only a bastion can produce these" is a derived claim about
//!   the data and not an assumption about what a bastion looks like.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::nether::NetherGenerator;
use serde_json::Value;

/// The vanilla-authored oracle world's seed, so this file and `nether_gen.rs`
/// describe the same Nether.
const SEED: i64 = -195_764_831;

/// A [`Resolver`] over `crates/lodestone-server/assets/` — the same bundle the
/// integrated server embeds, JSON *and* the NBT templates, with the **Nether's**
/// rows of it.
struct NetherAssets {
    worldgen: PathBuf,
    structures: PathBuf,
}

impl NetherAssets {
    fn new() -> Self {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets");
        Self {
            worldgen: assets.join("worldgen"),
            structures: assets.join("structure"),
        }
    }

    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.worldgen.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }

    fn try_read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.worldgen.join(kind).join(format!("{name}.json"));
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display())),
            Err(_) => Value::Null,
        }
    }
}

impl Resolver for NetherAssets {
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
    fn biome_parameters(&self) -> Value {
        self.read("biome_parameters", "nether")
    }
    fn biome_document(&self, id: &str) -> Value {
        self.try_read("biome", id)
    }
    fn configured_carver(&self, id: &str) -> Value {
        self.try_read("configured_carver", id)
    }
    fn block_tag(&self, id: &str) -> Value {
        self.try_read("tags/block", id)
    }
    fn structure_set_ids(&self) -> Vec<String> {
        let dir = self.worldgen.join("structure_set");
        let mut ids: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
            .filter_map(|e| {
                let path = e.ok()?.path();
                let stem = path.file_stem()?.to_str()?;
                (path.extension()? == "json").then(|| format!("minecraft:{stem}"))
            })
            .collect();
        ids.sort();
        ids
    }
    fn structure_set(&self, id: &str) -> Value {
        self.try_read("structure_set", id)
    }
    fn structure(&self, id: &str) -> Value {
        self.try_read("structure", id)
    }
    fn biome_tag(&self, id: &str) -> Value {
        self.try_read("tags/worldgen/biome", id)
    }
    fn structure_template(&self, id: &str) -> Option<Vec<u8>> {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        std::fs::read(self.structures.join(format!("{name}.nbt"))).ok()
    }
    fn template_pool(&self, id: &str) -> Value {
        self.try_read("template_pool", id)
    }
    fn processor_list(&self, id: &str) -> Value {
        self.try_read("processor_list", id)
    }
}

/// The control arm: identical density, surface, biome and carver data, and **no
/// structure sets**, so the registry is inert and `NetherGenerator` takes every
/// early return in its structure stages.
struct NoStructures(NetherAssets);

impl Resolver for NoStructures {
    fn density_function(&self, id: &str) -> Value {
        self.0.density_function(id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        self.0.noise(id)
    }
    fn biome_parameters(&self) -> Value {
        self.0.biome_parameters()
    }
    fn biome_document(&self, id: &str) -> Value {
        self.0.biome_document(id)
    }
    fn configured_carver(&self, id: &str) -> Value {
        self.0.configured_carver(id)
    }
    fn block_tag(&self, id: &str) -> Value {
        self.0.block_tag(id)
    }
}

fn settings() -> Value {
    let assets = NetherAssets::new();
    serde_json::from_str(
        &std::fs::read_to_string(assets.worldgen.join("noise_settings/nether.json")).unwrap(),
    )
    .unwrap()
}

/// The nearest `nether_complexes` cell to the origin at which `bastion_remnant`
/// really starts with a complete piece list, **measured** by
/// [`find_the_nearest_bastion`] rather than guessed.
///
/// `nether_complexes` is `random_spread` with `spacing 27`, and it carries
/// `fortress` (weight 2) as well — a cell whose weighted walk picks the fortress
/// stops there, exactly as vanilla's does, and yields an advisory start with no
/// pieces. So "the nearest placement cell" and "the nearest bastion" are different
/// questions and the second is the one this constant answers.
///
/// **Measured: ring 0, the very first candidate cell.** Unlike the Overworld's
/// coded structures (§12.139's desert pyramid at 234 cells, swamp hut at 211) the
/// search bound here is not the trap, and the reason is data:
/// `has_structure/bastion_remnant` covers four of the dimension's five biomes, so
/// the biome filter almost never rejects. A brief that warned "widen before
/// concluding absence" is still right — it just does not bite here, and recording
/// *why* is what stops the next reader assuming the bound is generous.
const BASTION_CHUNK: (i32, i32) = (8, 7);

/// The search that produced [`BASTION_CHUNK`]. `#[ignore]`d because its answer is a
/// function of the seed alone.
///
/// **The walk is over placement cells, not chunks**, and the candidate comes from
/// `Placement::potential_structure_chunk` — production's own function. Re-deriving
/// the grid arithmetic in a test helper is what turns a failing gate into a hanging
/// one.
#[test]
#[ignore = "column sampling; run to re-measure a stale constant"]
fn find_the_nearest_bastion() {
    let resolver = NetherAssets::new();
    let settings = settings();
    let generator = NetherGenerator::new(SEED, &settings, &resolver);
    let registry = lodestone_worldgen::structure::StructureRegistry::new(SEED, &resolver);
    let set = registry
        .sets()
        .iter()
        .find(|s| s.id == "minecraft:nether_complexes")
        .expect("nether_complexes is bundled");
    let mut cells = 0usize;
    'rings: for ring in 0..40i32 {
        for gx in -ring..=ring {
            for gz in -ring..=ring {
                if gx.abs() != ring && gz.abs() != ring {
                    continue;
                }
                let Some((cx, cz)) = set.placement.potential_structure_chunk(SEED, gx * 27, gz * 27)
                else {
                    continue;
                };
                cells += 1;
                let starts = generator.structure_starts(cx, cz);
                if starts.iter().any(|s| s.structure == "minecraft:bastion_remnant") {
                    println!("bastion at ({cx},{cz}), ring {ring}, after {cells} cells");
                    break 'rings;
                }
            }
        }
    }
}

/// Every block name in the columns the start's bounding box covers.
fn palette_over(generator: &NetherGenerator, bb: lodestone_worldgen::structure::BoundingBox) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for cx in (bb.min[0] >> 4)..=(bb.max[0] >> 4) {
        for cz in (bb.min[2] >> 4)..=(bb.max[2] >> 4) {
            let column = generator.column(cx, cz);
            for lx in 0..16 {
                for lz in 0..16 {
                    for y in column.min_y()..(column.min_y() + column.height()) {
                        let state = column.block_state(lx, y, lz);
                        let name = state.split_once('[').map_or(state, |(n, _)| n);
                        names.insert(name.to_string());
                    }
                }
            }
        }
    }
    names
}

/// Counts, over every chunk the start's box covers, how many blocks are in `names`.
fn count_blocks(
    generator: &NetherGenerator,
    bb: lodestone_worldgen::structure::BoundingBox,
    names: &HashSet<&str>,
) -> usize {
    let mut n = 0usize;
    for cx in (bb.min[0] >> 4)..=(bb.max[0] >> 4) {
        for cz in (bb.min[2] >> 4)..=(bb.max[2] >> 4) {
            let column = generator.column(cx, cz);
            for lx in 0..16 {
                for lz in 0..16 {
                    for y in column.min_y()..(column.min_y() + column.height()) {
                        let state = column.block_state(lx, y, lz);
                        let name = state.split_once('[').map_or(state, |(n, _)| n);
                        if names.contains(name) {
                            n += 1;
                        }
                    }
                }
            }
        }
    }
    n
}

/// Prints the palette a bastion adds to its own box. `#[ignore]`d; it is how
/// [`BASTION_ONLY`] was chosen, and it is also the cheapest way to see whether a
/// change to the jigsaw engine has silently changed what the structure is made of.
#[test]
#[ignore = "diagnostic; prints the with-minus-without palette difference"]
fn print_the_bastion_palette_difference() {
    let settings = settings();
    let with = NetherGenerator::new(SEED, &settings, &NetherAssets::new());
    let without = NetherGenerator::new(SEED, &settings, &NoStructures(NetherAssets::new()));
    let start = bastion_start(&with);
    let a = palette_over(&with, start.bounding_box);
    let b = palette_over(&without, start.bounding_box);
    println!("box = {:?}", start.bounding_box);
    println!("pieces = {}", start.pieces.len());
    for name in a.difference(&b) {
        println!("only with structures: {name}");
    }
    for name in b.difference(&a) {
        println!("only without structures: {name}");
    }
}

/// Names no Nether *terrain* stage can produce, so a non-zero count inside the
/// start box is a structure and nothing else.
///
/// **The claim is checked rather than assumed** — `the_discriminating_blocks_are_not_terrain`
/// asserts each of these is absent from `noise_settings/nether.json`'s own
/// `surface_rule` text and from `default_block`/`default_fluid`, which is the only
/// other thing that writes a block in this dimension. That matters because the
/// obvious candidates are wrong: `minecraft:blackstone` and `minecraft:basalt` are
/// both products of vanilla's own bundled Nether surface-rule data (basalt deltas' floor and pillars), and
/// a gate built on either would pass in the control arm too.
/// The list is the *measured* with-minus-without palette difference narrowed to the
/// names that cannot be terrain, not a guess: `print_the_bastion_palette_difference`
/// reports 16 names at [`BASTION_CHUNK`], of which `basalt`, `blackstone` and
/// `nether_wart` are excluded here because a different Nether column really can
/// produce them.
const BASTION_ONLY: &[&str] = &[
    "minecraft:polished_blackstone_bricks",
    "minecraft:cracked_polished_blackstone_bricks",
    "minecraft:polished_blackstone_brick_stairs",
    "minecraft:chiseled_polished_blackstone",
    "minecraft:gilded_blackstone",
    "minecraft:gold_block",
];

/// Names no Nether terrain stage can produce, for the fossil arm.
///
/// `bone_block` is the only block a `nether_fossils/fossil_*` template contains, and
/// `dried_ghast` is the coin-flip extra. Both are checked against the Nether's own
/// `surface_rule` by `the_discriminating_blocks_are_not_terrain`, the same way
/// [`BASTION_ONLY`] is — and unlike `basalt`, neither is a product of vanilla's own bundled Nether surface-rule data,
/// which is what makes this list two names rather than a narrowing exercise.
const FOSSIL_ONLY: &[&str] = &["minecraft:bone_block", "minecraft:dried_ghast"];

/// The nearest `nether_fossil` placement cell that really starts one, **measured** by
/// [`find_the_nearest_fossil`].
///
/// `nether_fossils` is `random_spread` with `spacing 2`, so cells are dense — but
/// vanilla's own "find generation point" can still return `Optional.empty()` for any of them, because
/// the downward walk gives up at sea level. So "a placement cell" and "a fossil" are
/// different questions here for a reason that has nothing to do with biomes.
///
/// **Measured: ring 1, the 9th candidate cell** — box `[39, 88, 37]..[42, 91, 41]`,
/// template `nether_fossils/fossil_1`. Far nearer than the Overworld's coded
/// structures (a desert pyramid needs 234 cells at the same seed), and the reason is
/// again data: the fossil's biome tag covers `soul_sand_valley`, which this Nether has
/// plenty of.
const FOSSIL_CHUNK: (i32, i32) = (2, 2);

/// The nearest fossil whose dried ghast **survives into the world** — ring 3, cell 32,
/// box `[-86, 83, 97]..[-80, 89, 102]`, template `fossil_14`.
///
/// A separate constant because the ghast is a coin flip on a *positional fork of the
/// world seed*, so the nearest fossil need not have one — [`FOSSIL_CHUNK`] does not,
/// which is what makes the pair a two-sided gate on the fork rather than a one-sided
/// "a ghast appears somewhere".
///
/// **And "on the piece" is not "in the world".** The nearest ghast-*bearing* fossil is
/// at (−4, 2), and its ghast is overwritten by the template's own bone block: the place
/// stage writes `blocks` before `placement`, which is how vanilla's post-template
/// `isAir()` rejection is expressed as an ordering rather than as a read this engine
/// cannot make at start time. Measuring the piece instead of the world would have
/// gated the wrong thing and passed.
const FOSSIL_GHAST_CHUNK: (i32, i32) = (-6, 6);

/// The search that produced [`FOSSIL_CHUNK`], over placement cells and through
/// production's own `potential_structure_chunk`.
#[test]
#[ignore = "column sampling; run to re-measure a stale constant"]
fn find_the_nearest_fossil() {
    let resolver = NetherAssets::new();
    let settings = settings();
    let generator = NetherGenerator::new(SEED, &settings, &resolver);
    let registry = lodestone_worldgen::structure::StructureRegistry::new(SEED, &resolver);
    let set = registry
        .sets()
        .iter()
        .find(|s| s.id == "minecraft:nether_fossils")
        .expect("nether_fossils is bundled");
    let mut cells = 0usize;
    let mut first: Option<(i32, i32)> = None;
    let mut ghast_bearing: Option<(i32, i32)> = None;
    'rings: for ring in 0..60i32 {
        for gx in -ring..=ring {
            for gz in -ring..=ring {
                if gx.abs() != ring && gz.abs() != ring {
                    continue;
                }
                let Some((cx, cz)) = set.placement.potential_structure_chunk(SEED, gx * 2, gz * 2)
                else {
                    continue;
                };
                cells += 1;
                let starts = generator.structure_starts(cx, cz);
                if let Some(start) =
                    starts.iter().find(|s| s.structure == "minecraft:nether_fossil")
                {
                    let ghast = start.pieces[0].blocks.as_ref().map_or(0, |b| b.len());
                    println!(
                        "nether_fossil at ({cx}, {cz}) after {cells} cells, ring {ring}, box {:?}, \
                         template {:?}, dried ghast blocks {ghast}",
                        start.bounding_box,
                        start.pieces[0].template,
                    );
                    if first.is_none() {
                        first = Some((cx, cz));
                    }
                    // The dried ghast is a coin flip on a positional fork, so the
                    // first fossil found need not carry one. Keep walking until one
                    // does, because that is the arm that gates the fork.
                    //
                    // And a ghast on the piece is not a ghast in the world: the
                    // place stage writes `blocks` **before** the template, so a
                    // ghast the template covers is overwritten — which is exactly
                    // vanilla's `isAir()` rejection expressed as an ordering. So the
                    // search asks the world, not the piece.
                    if ghast > 0 && ghast_bearing.is_none() {
                        let names: HashSet<&str> =
                            ["minecraft:dried_ghast"].into_iter().collect();
                        let in_world = count_blocks(&generator, start.bounding_box, &names);
                        println!("  in world: {in_world}");
                        if in_world == 1 {
                            ghast_bearing = Some((cx, cz));
                        }
                    }
                    if first.is_some() && ghast_bearing.is_some() {
                        break 'rings;
                    }
                }
            }
        }
    }
    println!("first = {first:?}, first with a dried ghast = {ghast_bearing:?}");
}

/// **`nether_fossil` places blocks in a generated Nether column**, and the same data
/// with no structure sets places none of them.
///
/// The same shape as the bastion arm, on the structure that was the cheapest in the
/// corpus and still reached zero blocks until the Nether pipeline gained a structure
/// stage — a reminder that "small" and "reachable" are different axes.
#[test]
fn nether_fossil_places_its_own_blocks_in_a_nether_column() {
    let settings = settings();
    let with = NetherGenerator::new(SEED, &settings, &NetherAssets::new());
    let without = NetherGenerator::new(SEED, &settings, &NoStructures(NetherAssets::new()));
    let start = with
        .structure_starts(FOSSIL_CHUNK.0, FOSSIL_CHUNK.1)
        .into_iter()
        .find(|s| s.structure == "minecraft:nether_fossil")
        .unwrap_or_else(|| {
            panic!(
                "no nether_fossil start at {FOSSIL_CHUNK:?} — the measured constant is \
                 stale, re-run find_the_nearest_fossil"
            )
        });
    assert!(start.pieces_complete, "the fossil reports no pieces");
    let piece = &start.pieces[0];
    assert!(
        piece.placement.is_some(),
        "a fossil is a template piece and must carry a placement"
    );
    // The walk gives up at sea level, so a start at or below it should not exist.
    // The expectation comes from the settings document rather than from the
    // generator, so it is the data's number and not a re-read of the code's.
    let sea_level = settings["sea_level"].as_i64().expect("sea_level") as i32;
    assert!(
        start.bounding_box.min[1] > sea_level,
        "fossil box min Y {} is not above the Nether sea level {sea_level}",
        start.bounding_box.min[1]
    );

    let names: HashSet<&str> = FOSSIL_ONLY.iter().copied().collect();
    let placed = count_blocks(&with, start.bounding_box, &names);
    let control = count_blocks(&without, start.bounding_box, &names);
    assert_eq!(control, 0, "the structureless control holds {control}");
    assert!(
        placed > 0,
        "the fossil placed {placed} of its own blocks at {FOSSIL_CHUNK:?}"
    );
    // This fossil's coin flip came up tails, which is half of what makes the ghast
    // arm below a real test of the positional fork rather than of a constant.
    assert!(
        start.pieces[0].blocks.is_none(),
        "the measured fossil at {FOSSIL_CHUNK:?} carries no dried ghast; if that \
         changed, FOSSIL_GHAST_CHUNK's contrast is gone"
    );
}

/// The dried ghast: a coin flip on a **positional fork of the world seed**, so it
/// costs the structure's own stream nothing and is a pure function of `(seed, box)`.
///
/// Paired with the assertion above that [`FOSSIL_CHUNK`]'s fossil has none: one arm
/// alone is satisfied both by a correct fork and by a constant `true`.
#[test]
fn a_fossils_dried_ghast_comes_from_a_positional_fork() {
    let settings = settings();
    let with = NetherGenerator::new(SEED, &settings, &NetherAssets::new());
    let without = NetherGenerator::new(SEED, &settings, &NoStructures(NetherAssets::new()));
    let start = with
        .structure_starts(FOSSIL_GHAST_CHUNK.0, FOSSIL_GHAST_CHUNK.1)
        .into_iter()
        .find(|s| s.structure == "minecraft:nether_fossil")
        .unwrap_or_else(|| {
            panic!("no nether_fossil at {FOSSIL_GHAST_CHUNK:?} — the constant is stale")
        });
    let blocks = start.pieces[0]
        .blocks
        .as_ref()
        .expect("this fossil's coin flip came up heads and must carry a ghast");
    assert_eq!(blocks.len(), 1, "exactly one dried ghast per fossil");
    assert!(
        blocks[0].state.starts_with("minecraft:dried_ghast["),
        "unexpected coded block: {}",
        blocks[0].state
    );
    // `y = fossilBB.minY()` — the box floor, not its centre and not the surface.
    assert_eq!(blocks[0].pos[1], start.bounding_box.min[1]);

    let names: HashSet<&str> = ["minecraft:dried_ghast"].into_iter().collect();
    let placed = count_blocks(&with, start.bounding_box, &names);
    let control = count_blocks(&without, start.bounding_box, &names);
    assert_eq!(control, 0, "the structureless control holds {control}");
    assert_eq!(placed, 1, "the world holds {placed} dried ghasts, expected 1");
}

fn bastion_start(
    generator: &NetherGenerator,
) -> std::sync::Arc<lodestone_worldgen::structure::StructureStart> {
    let start = generator
        .structure_starts(BASTION_CHUNK.0, BASTION_CHUNK.1)
        .into_iter()
        .find(|s| s.structure == "minecraft:bastion_remnant")
        .unwrap_or_else(|| {
            panic!(
                "no bastion_remnant start at {BASTION_CHUNK:?} — \
                 the measured constant is stale, re-run find_the_nearest_bastion"
            )
        });
    assert!(start.pieces_complete, "the bastion reports no pieces");
    assert!(!start.pieces.is_empty(), "the bastion has an empty piece list");
    start
}

/// The gate this whole unit exists for: **`bastion_remnant` places blocks in a
/// generated Nether column, and the same data with no structure sets places none of
/// them.**
#[test]
fn bastion_remnant_places_its_own_blocks_in_a_nether_column() {
    let settings = settings();
    let with = NetherGenerator::new(SEED, &settings, &NetherAssets::new());
    let without = NetherGenerator::new(SEED, &settings, &NoStructures(NetherAssets::new()));
    let start = bastion_start(&with);
    let names: HashSet<&str> = BASTION_ONLY.iter().copied().collect();

    let placed = count_blocks(&with, start.bounding_box, &names);
    let control = count_blocks(&without, start.bounding_box, &names);
    println!(
        "bastion at {BASTION_CHUNK:?}: {} pieces, box {:?}, {placed} discriminating blocks \
         with structures and {control} without",
        start.pieces.len(),
        start.bounding_box
    );
    assert!(
        placed > 500,
        "a size-6 bastion over {} pieces placed only {placed} discriminating blocks",
        start.pieces.len()
    );
    // The control's premise: the same box, the same seed, the same terrain — the one
    // difference is that the resolver serves no structure sets.
    assert_eq!(
        control, 0,
        "the structure-free arm placed {control} bastion-only blocks, so the \
         discriminator is not discriminating and `placed` proves nothing"
    );
}

/// The premise of the gate above: none of [`BASTION_ONLY`] is something the Nether's
/// terrain stages can write.
///
/// Derived from the data, not asserted about the game: the only writers in this
/// dimension are `default_block`, `default_fluid`, the `surface_rule` tree and the
/// carver's own two states, so a name absent from all of those cannot appear without
/// a structure.
#[test]
fn the_discriminating_blocks_are_not_terrain() {
    let assets = NetherAssets::new();
    let settings = settings();
    let surface = serde_json::to_string(&settings["surface_rule"]).unwrap();
    let default_block = settings["default_block"]["Name"].as_str().unwrap();
    let default_fluid = settings["default_fluid"]["Name"].as_str().unwrap();
    let carver = serde_json::to_string(&assets.try_read("configured_carver", "nether_cave")).unwrap();
    for name in BASTION_ONLY.iter().chain(FOSSIL_ONLY.iter()) {
        assert!(
            !surface.contains(name),
            "{name} is a nether surface-rule product and cannot discriminate"
        );
        assert_ne!(*name, default_block);
        assert_ne!(*name, default_fluid);
        assert!(!carver.contains(name), "{name} appears in nether_cave");
    }
    // And the two names that look like the obvious choice really are terrain here,
    // which is why this test is not a formality.
    assert!(
        surface.contains("minecraft:blackstone") || surface.contains("minecraft:basalt"),
        "the nether surface rule should name blackstone/basalt; if it no longer \
         does, this test's own warning has gone stale: {surface}"
    );
}

/// Vanilla's `hasBiomesForStructureSet` filter, recomputed from the bundled data.
///
/// The expectation comes from the JSON — each structure document's `biomes` tag
/// closure against `biome_parameters/nether.json`'s own biome names — rather than
/// from the registry, and **both arms are required to be non-empty**: a filter that
/// kept everything and a filter that kept nothing would each pass a one-sided
/// version of this.
#[test]
fn the_nether_registry_holds_exactly_the_sets_this_dimension_can_place() {
    let assets = NetherAssets::new();
    // The dimension's own possible biomes, straight out of the parameter table.
    let table = lodestone_worldgen::biome::parse_table(&assets.biome_parameters());
    let possible: BTreeSet<String> = table.iter().map(|p| p.biome.clone()).collect();
    assert_eq!(possible.len(), 5, "the Nether table should name five biomes: {possible:?}");

    let mut want_kept: BTreeSet<String> = BTreeSet::new();
    let mut want_dropped: BTreeSet<String> = BTreeSet::new();
    for set_id in assets.structure_set_ids() {
        let document = assets.structure_set(&set_id);
        let mut reachable = false;
        for entry in document["structures"].as_array().into_iter().flatten() {
            let structure_id = entry["structure"].as_str().unwrap();
            let doc = assets.structure(structure_id);
            // Resolved through the same tag documents, walked here independently.
            let mut closure: BTreeSet<String> = BTreeSet::new();
            collect_biomes(&assets, &doc["biomes"], &mut closure, &mut BTreeSet::new());
            if closure.iter().any(|b| possible.contains(b)) {
                reachable = true;
            }
        }
        if reachable { want_kept.insert(set_id) } else { want_dropped.insert(set_id) };
    }
    assert!(!want_kept.is_empty() && !want_dropped.is_empty(), "one-sided filter");

    let registry = lodestone_worldgen::structure::StructureRegistry::new_for_biomes(
        SEED,
        &assets,
        Some(&possible.iter().cloned().collect()),
    );
    let got: BTreeSet<String> = registry.sets().iter().map(|s| s.id.clone()).collect();
    assert_eq!(got, want_kept, "the filtered registry disagrees with the data");
    // Named explicitly as well as computed, so a change to either side is visible.
    assert!(got.contains("minecraft:nether_complexes"));
    assert!(got.contains("minecraft:nether_fossils"));
    assert!(!got.contains("minecraft:villages"));
    assert!(!got.contains("minecraft:mineshafts"));
    // The unfiltered constructor is what the Overworld uses and must be unchanged.
    let unfiltered = lodestone_worldgen::structure::StructureRegistry::new(SEED, &assets);
    assert!(unfiltered.sets().len() > got.len(), "the filter removed nothing");
}

/// `resolve_biome_set` re-implemented over the raw tag documents, so the expectation
/// in the test above is not the production walk.
fn collect_biomes(
    assets: &NetherAssets,
    value: &Value,
    out: &mut BTreeSet<String>,
    seen: &mut BTreeSet<String>,
) {
    match value {
        Value::String(s) => match s.strip_prefix('#') {
            Some(tag) => {
                if !seen.insert(tag.to_string()) {
                    return;
                }
                let doc = assets.biome_tag(tag);
                for entry in doc["values"].as_array().into_iter().flatten() {
                    collect_biomes(assets, entry, out, seen);
                }
            }
            None => {
                out.insert(s.clone());
            }
        },
        Value::Array(items) => {
            for item in items {
                collect_biomes(assets, item, out, seen);
            }
        }
        Value::Object(o) => {
            if let Some(id) = o.get("id") {
                collect_biomes(assets, id, out, seen);
            }
        }
        _ => {}
    }
}

/// The ledger says what is still missing, and does **not** say what has been fixed.
#[test]
fn the_nether_ledger_names_the_remaining_gaps_and_not_the_closed_one() {
    let settings = settings();
    let generator = NetherGenerator::new(SEED, &settings, &NetherAssets::new());
    let ledger = generator.structure_ledger();
    assert!(
        !ledger.contains_key("minecraft:bastion_remnant"),
        "bastion_remnant assembles and now places: {:?}",
        ledger.get("minecraft:bastion_remnant")
    );
    for id in ["minecraft:fortress", "minecraft:ruined_portal_nether"] {
        assert!(ledger.contains_key(id), "{id} has no piece generator and must be ledgered");
    }
    // S7: `nether_fossil` places 14 possible bone templates and a coin-flip dried
    // ghast, so it comes **off** the ledger. Asserted as an absence for the same
    // reason the bastion row above is: a registry that quietly demoted it for an
    // unloadable template would satisfy every "the ledger names its gaps" claim.
    assert!(
        !ledger.contains_key("minecraft:nether_fossil"),
        "nether_fossil places blocks now: {:?}",
        ledger.get("minecraft:nether_fossil")
    );
    // The dimension row must no longer claim there is no structure stage.
    let row = ledger
        .get("dimension:nether_structures")
        .expect("the remaining dimension-level gap must be named");
    assert!(
        row.contains("places blocks"),
        "the row still describes the closed gap: {row}"
    );
    assert!(
        row.contains("chunk source") || row.contains("ChunkSource"),
        "the row must name what is still missing — nothing serves this dimension: {row}"
    );
    // An Overworld-only structure is not in a Nether registry's ledger at all, which
    // is the filter's observable consequence.
    assert!(
        !ledger.contains_key("minecraft:mineshaft"),
        "a Nether registry should not carry Overworld rows"
    );
}

/// Structures do not cost the Nether its determinism: two independently constructed
/// generators produce byte-identical columns over the bastion's own box, in opposite
/// request orders.
///
/// The palette of a [`DenseBlockGrid`] is built in `set` order, so this is the gate
/// that a structure place step iterating a hash map would fail.
#[test]
fn structure_bearing_columns_are_byte_identical_regardless_of_order() {
    let settings = settings();
    let a = NetherGenerator::new(SEED, &settings, &NetherAssets::new());
    let b = NetherGenerator::new(SEED, &settings, &NetherAssets::new());
    let start = bastion_start(&a);
    let chunks: Vec<(i32, i32)> = ((start.bounding_box.min[0] >> 4)..=(start.bounding_box.max[0] >> 4))
        .flat_map(|cx| {
            ((start.bounding_box.min[2] >> 4)..=(start.bounding_box.max[2] >> 4))
                .map(move |cz| (cx, cz))
        })
        .collect();
    let forward: Vec<_> = chunks.iter().map(|&(cx, cz)| a.column(cx, cz).into_raw()).collect();
    let reverse: Vec<_> = chunks
        .iter()
        .rev()
        .map(|&(cx, cz)| b.column(cx, cz).into_raw())
        .collect();
    for (i, want) in forward.iter().enumerate() {
        let got = &reverse[chunks.len() - 1 - i];
        assert_eq!(want.2, got.2, "palette order differs at {:?}", chunks[i]);
        assert_eq!(want.3, got.3, "blocks differ at {:?}", chunks[i]);
        assert_eq!(want.4, got.4, "biomes differ at {:?}", chunks[i]);
    }
}

/// The beard branch, asserted rather than inferred — and **inverted by S7**.
///
/// This test used to say the beardifier was empty for every Nether chunk, because
/// `nether_fossil` is the dimension's only adaptation-bearing structure and had no
/// piece generator. Its own comment said that if a generator landed, this test was
/// the record and should be updated deliberately. It landed, so here is that update:
/// the seam is **live**, and the assertion is now that a chunk in reach of a real
/// fossil has a non-empty beard while a chunk far from any fossil does not.
///
/// The negative arm is load-bearing in a way the old test's four chunks were not:
/// `nether_fossils` is `spacing 2`, so a chunk with *no* fossil within the
/// beardifier's 12-block reach is genuinely hard to find, and a version of this test
/// with only the positive arm would be satisfied by a beardifier that returned every
/// start in the world.
#[test]
fn the_nether_beard_is_live_at_a_fossil_and_empty_away_from_one() {
    let settings = settings();
    let generator = NetherGenerator::new(SEED, &settings, &NetherAssets::new());
    let start = generator
        .structure_starts(FOSSIL_CHUNK.0, FOSSIL_CHUNK.1)
        .into_iter()
        .find(|s| s.structure == "minecraft:nether_fossil")
        .expect("the measured fossil constant is stale");
    assert_eq!(
        start.terrain_adaptation,
        lodestone_worldgen::structure::TerrainAdjustment::BeardThin,
        "the fossil document says beard_thin; if it no longer does, this whole test \
         is measuring the wrong thing"
    );
    // The chunk the fossil's own box sits in.
    let (fx, fz) = (start.bounding_box.min[0] >> 4, start.bounding_box.min[2] >> 4);
    assert!(
        !generator.beardifier(fx, fz).is_empty(),
        "({fx},{fz}) holds a beard_thin fossil box and its beard is empty"
    );
    // The control: a chunk chosen because the engine reports no adaptation-bearing
    // start in reach of it, found by asking rather than by guessing — a hardcoded
    // "far away" chunk in a spacing-2 grid is as likely as not to have one.
    let mut empty_chunk = None;
    for cx in 200..320 {
        if generator.beardifier(cx, 200).is_empty() {
            empty_chunk = Some((cx, 200));
            break;
        }
    }
    let (ex, ez) = empty_chunk.expect(
        "no chunk in a 120-chunk sweep has an empty beard, so the negative arm cannot \
         be built and the positive arm proves nothing",
    );
    assert!(generator.beardifier(ex, ez).is_empty());
}
