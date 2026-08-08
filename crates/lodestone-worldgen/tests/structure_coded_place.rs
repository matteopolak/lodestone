//! **Coded** structure pieces reach blocks (issue #514's S5).
//!
//! `swamp_hut` and `desert_pyramid` have no `.nbt` template: their blocks are Java
//! statements, ported in `structure/coded.rs` and resolved eagerly at start time.
//! What this file gates is the end of that path — that a generated chunk at a real
//! placement chunk of the real bundled data contains blocks only these structures
//! can produce, and that the identical world with no structure data contains none.
//!
//! Neither structure appears in the survival oracle's generated area, so the chunks
//! come from the *placement* engine instead — already gated against that oracle by
//! S1 — walked outward in rings until the biome filter lets one through, and then
//! recorded as constants. The seed is still the vanilla-authored world's, and the
//! control arm is the structure-free resolver over identical data.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use lodestone_worldgen::structure::StructureRegistry;
use serde_json::Value;

const SEED: i64 = -195_764_831;

/// A [`Resolver`] over `crates/lodestone-server/assets/` — the same bundle the
/// integrated server embeds, JSON *and* the NBT templates.
struct ServerAssets {
    worldgen: PathBuf,
    structures: PathBuf,
}

impl ServerAssets {
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

impl Resolver for ServerAssets {
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
        self.read("biome_parameters", "overworld")
    }
    fn biome_temperatures(&self) -> Value {
        self.read("biome_parameters", "overworld_temperature")
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

/// The control arm: identical data, no structure sets, so the registry is inert.
struct NoStructures(ServerAssets);

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
    fn biome_temperatures(&self) -> Value {
        self.0.biome_temperatures()
    }
}

fn settings() -> Value {
    let assets = ServerAssets::new();
    serde_json::from_str(
        &std::fs::read_to_string(assets.worldgen.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap()
}

fn generator(resolver: &dyn Resolver, settings: &Value) -> OverworldGenerator {
    OverworldGenerator::new(SEED, settings, resolver, "minecraft:plains", false)
}

/// The nearest placement chunk to the origin at which each structure really
/// starts, **measured** by walking `is_structure_chunk` outward in rings at this
/// seed: `desert_pyramid` needs 234 candidate cells and `swamp_hut` 211 before the
/// biome filter lets one through, so neither is anywhere near the origin and a
/// bounded search would report "not implemented" for a working generator.
///
/// Recorded as constants rather than re-searched: the search costs a column sample
/// per candidate and its answer is a function of the seed alone.
const PYRAMID_CHUNK: (i32, i32) = (-243, 149);
const HUT_CHUNK: (i32, i32) = (-95, 234);
/// Measured the same way, by [`find_the_nearest_start_chunks`] — grid ring **5**,
/// which is far nearer than either of the two above and is the reason a bounded
/// search that happened to stop at ring 4 would have reported the jungle temple
/// missing as well.
const JUNGLE_CHUNK: (i32, i32) = (-152, -160);

/// The search that produced the constants above. `#[ignore]`d because it is
/// minutes of column sampling and its answer is a function of the seed alone —
/// re-run it (`-- --ignored --nocapture`) if a constant ever goes stale, which
/// `start_at`'s panic says explicitly.
///
/// **The walk is over placement *cells*, not chunks.** A `random_spread` set
/// nominates exactly one chunk per `spacing × spacing` cell, so walking chunks
/// would be `spacing²` times more work — 224 million chunks to reach the desert
/// pyramid's cell 234. The candidate chunk comes from
/// `Placement::potential_structure_chunk`, i.e. production's own function, rather
/// than a re-derivation of the grid maths here: a test helper that duplicates
/// production logic is what turns a failing gate into a hanging one.
#[test]
#[ignore = "minutes of column sampling; run to re-measure a stale constant"]
fn find_the_nearest_start_chunks() {
    let settings = settings();
    let with = generator(&ServerAssets::new(), &settings);
    let registry = StructureRegistry::new(SEED, &ServerAssets::new());
    for (set_id, structure_id) in [
        ("minecraft:jungle_temples", "minecraft:jungle_pyramid"),
        ("minecraft:desert_pyramids", "minecraft:desert_pyramid"),
        ("minecraft:swamp_huts", "minecraft:swamp_hut"),
    ] {
        let set = registry
            .sets()
            .iter()
            .find(|s| s.id == set_id)
            .unwrap_or_else(|| panic!("{set_id} is not a bundled set"));
        let mut found = None;
        'rings: for ring in 0..300i32 {
            for gx in -ring..=ring {
                for gz in -ring..=ring {
                    if gx.abs() != ring && gz.abs() != ring {
                        continue;
                    }
                    // `spacing` is private, and any literal here would be a second
                    // copy of the set's own data: nominate the cell by a chunk
                    // inside it and let production pick the candidate.
                    let Some((cx, cz)) =
                        set.placement.potential_structure_chunk(SEED, gx * 32, gz * 32)
                    else {
                        continue;
                    };
                    if with
                        .structure_starts(cx, cz)
                        .iter()
                        .any(|s| s.structure == structure_id && !s.pieces.is_empty())
                    {
                        found = Some((cx, cz, ring));
                        break 'rings;
                    }
                }
            }
        }
        println!("{structure_id}: {found:?}");
    }
}

/// The start of `structure_id` at `chunk`, which must exist and must be complete.
fn start_at(
    generator: &OverworldGenerator,
    chunk: (i32, i32),
    structure_id: &str,
) -> std::sync::Arc<lodestone_worldgen::structure::StructureStart> {
    let start = generator
        .structure_starts(chunk.0, chunk.1)
        .into_iter()
        .find(|s| s.structure == structure_id)
        .unwrap_or_else(|| {
            panic!("no {structure_id} start at {chunk:?} — the measured constant is stale")
        });
    assert!(start.pieces_complete, "{structure_id} reports no pieces");
    assert!(!start.pieces.is_empty(), "{structure_id} has an empty piece list");
    start
}

/// Counts, over every chunk the start's box covers, how many blocks are in `names`.
fn count_blocks(
    generator: &OverworldGenerator,
    bb: lodestone_worldgen::structure::BoundingBox,
    names: &HashSet<&str>,
) -> usize {
    let mut n = 0usize;
    for x in (bb.min[0] >> 4)..=(bb.max[0] >> 4) {
        for z in (bb.min[2] >> 4)..=(bb.max[2] >> 4) {
            let column = generator.column(x, z);
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

/// `swamp_hut` and `desert_pyramid` are **absent** from the ledger, and the coded
/// generators that have not landed are still **present** on it.
///
/// The negative half is the load-bearing half: a registry that quietly demoted
/// `desert_pyramid` for an unloadable something would pass every "the ledger names
/// its gaps" assertion on its own.
#[test]
fn the_coded_structures_s5_models_are_not_on_the_ledger() {
    let registry = StructureRegistry::new(SEED, &ServerAssets::new());
    let ledger = registry.unsupported();
    for id in [
        "minecraft:swamp_hut",
        "minecraft:desert_pyramid",
        "minecraft:jungle_pyramid",
    ] {
        assert!(
            !ledger.contains_key(id),
            "{id} is on the ledger: {:?}",
            ledger.get(id)
        );
    }
    for id in [
        "minecraft:mineshaft",
        "minecraft:stronghold",
        "minecraft:monument",
        "minecraft:ruined_portal",
    ] {
        assert!(ledger.contains_key(id), "{id} should still be ledgered");
    }
    // Every deviation is named, not just the absences.
    for key in [
        "coded:average_ground_height",
        "coded:region_random",
        "coded:worldgen_entities",
        "coded:chests",
        "coded:chest_reorient",
        "coded:decoration_random",
        "coded:buried_treasure_chest",
        "dimension:nether_structures",
    ] {
        assert!(ledger.contains_key(key), "{key} is not on the ledger");
    }
    // `bastion_remnant` is **supported** (its pools load, it assembles) and reaches
    // zero blocks in a served world, because it is Nether-only and `NetherGenerator`
    // has no structure stage. Both halves are asserted: the structure absent from
    // the per-structure rows, *and* the reachability row present naming it. A gate
    // that only checked the first would report a healthy bastion, which is what
    // every other instrument here already does.
    assert!(
        !ledger.contains_key("minecraft:bastion_remnant"),
        "bastion_remnant assembles; its problem is reachability, not support"
    );
    let nether_row = ledger
        .get("dimension:nether_structures")
        .expect("the Nether reachability gap must be named");
    assert!(
        nether_row.contains("bastion_remnant") && nether_row.contains("NetherGenerator"),
        "the row must name the structure and where the fix goes: {nether_row}"
    );
    // The two rows S6 corrected. `template:data_markers` claimed shipwreck / igloo /
    // ocean-ruin loot chests were not placed at all; `lodestone_server`'s
    // `structure_loot` has been rolling them since #337, so the row named a closed
    // gap and hid the open one. It must be **gone**, and its replacement present:
    // the 132 templates whose loot lives in a block's own `nbt` compound.
    assert!(
        !ledger.contains_key("template:data_markers"),
        "template:data_markers described a closed gap and must not come back"
    );
    let nbt_row = ledger
        .get("template:block_entity_nbt")
        .expect("the real template-loot gap must be named");
    assert!(
        nbt_row.contains("132"),
        "the row should carry the measured template count: {nbt_row}"
    );
    let brush_row = ledger
        .get("block_entity:append_loot")
        .expect("append_loot stays on the ledger");
    assert!(
        brush_row.contains("brush"),
        "append_loot's blocker is that nothing brushes, not that worldgen lacks \
         block entities: {brush_row}"
    );
}

/// A desert pyramid puts its own blocks in the world, and a structure-free world
/// with identical data puts none there.
///
/// `chiseled_sandstone`, `cut_sandstone` and `orange_terracotta` have no other
/// source in this world: no surface rule, carver, ore or feature produces any of
/// them, so a non-zero control would mean the counter is measuring terrain.
#[test]
fn a_desert_pyramid_chunk_gains_pyramid_blocks_a_structureless_chunk_does_not() {
    let settings = settings();
    let with = generator(&ServerAssets::new(), &settings);
    let without = generator(&NoStructures(ServerAssets::new()), &settings);
    let start = start_at(&with, PYRAMID_CHUNK, "minecraft:desert_pyramid");
    let piece = &start.pieces[0];
    let blocks = piece
        .blocks
        .as_ref()
        .expect("a coded piece carries a resolved block list");
    assert!(
        blocks.len() > 3_000,
        "a pyramid should be thousands of blocks, got {}",
        blocks.len()
    );

    let pyramid_blocks: HashSet<&str> = [
        "minecraft:chiseled_sandstone",
        "minecraft:cut_sandstone",
        "minecraft:orange_terracotta",
        "minecraft:blue_terracotta",
        "minecraft:sandstone_stairs",
        "minecraft:tnt",
        "minecraft:stone_pressure_plate",
        "minecraft:suspicious_sand",
    ]
    .into_iter()
    .collect();
    // **The expected count is predicted, not observed.** The piece's own resolved
    // list is collapsed last-write-wins into a final state per position, and the
    // signature blocks in *that* are what the world must contain. So the number
    // comes from the start stage and the measurement from the placement stage —
    // two different stages, which is what makes a partially-written pyramid fail
    // rather than merely score lower.
    let mut final_state: std::collections::HashMap<[i32; 3], &str> =
        std::collections::HashMap::new();
    for block in blocks.iter() {
        final_state.insert(block.pos, block.state.as_str());
    }
    let expected = final_state
        .values()
        .filter(|state| {
            let name = state.split_once('[').map_or(**state, |(n, _)| n);
            pyramid_blocks.contains(name)
        })
        .count();
    assert!(expected > 300, "the piece itself carries only {expected} signature blocks");

    let placed = count_blocks(&with, start.bounding_box, &pyramid_blocks);
    let control = count_blocks(&without, start.bounding_box, &pyramid_blocks);
    assert_eq!(
        placed, expected,
        "the world holds {placed} of the piece's {expected} signature blocks"
    );
    assert_eq!(control, 0, "the structureless control holds {control}");
}

/// A swamp hut likewise, and its stilts reach the ground.
///
/// `spruce_planks`, `spruce_stairs`, `cauldron` and `potted_red_mushroom` have no
/// other source in a generated swamp.
#[test]
fn a_swamp_hut_chunk_gains_hut_blocks_a_structureless_chunk_does_not() {
    let settings = settings();
    let with = generator(&ServerAssets::new(), &settings);
    let without = generator(&NoStructures(ServerAssets::new()), &settings);
    let start = start_at(&with, HUT_CHUNK, "minecraft:swamp_hut");

    let hut_blocks: HashSet<&str> = [
        "minecraft:spruce_planks",
        "minecraft:spruce_stairs",
        "minecraft:cauldron",
        "minecraft:crafting_table",
        "minecraft:potted_red_mushroom",
    ]
    .into_iter()
    .collect();
    let placed = count_blocks(&with, start.bounding_box, &hut_blocks);
    let control = count_blocks(&without, start.bounding_box, &hut_blocks);
    assert!(
        placed > 80 && control == 0,
        "hut blocks: {placed} (expected > 80), structureless control: {control} \
         (expected 0)"
    );
}

/// A jungle temple puts its own blocks in the world, and a structure-free world
/// with identical data puts none there.
///
/// The signature set is chosen for having **no other source in a generated
/// jungle**: `chiseled_stone_bricks`, `cobblestone_stairs`, `lever`, `repeater`,
/// `sticky_piston`, `dispenser` and `tripwire_hook` are produced by no surface
/// rule, carver, ore or feature anywhere in this generator. `mossy_cobblestone` is
/// deliberately *excluded* — the temple's commonest block, but also a
/// `simple_dungeon`-adjacent one, so a non-zero control could be terrain rather
/// than a leak.
///
/// The expected count is predicted from the *start* stage the same way the pyramid
/// gate's is, and the two chests are asserted separately: they are the one thing
/// last-write-wins could silently swallow, since the alcove writes and the chest
/// share a position in the pyramid's case.
#[test]
fn a_jungle_temple_chunk_gains_temple_blocks_a_structureless_chunk_does_not() {
    let settings = settings();
    let with = generator(&ServerAssets::new(), &settings);
    let without = generator(&NoStructures(ServerAssets::new()), &settings);
    let start = start_at(&with, JUNGLE_CHUNK, "minecraft:jungle_pyramid");
    let piece = &start.pieces[0];
    let blocks = piece
        .blocks
        .as_ref()
        .expect("a coded piece carries a resolved block list");

    let temple_blocks: HashSet<&str> = [
        "minecraft:chiseled_stone_bricks",
        "minecraft:cobblestone_stairs",
        "minecraft:lever",
        "minecraft:repeater",
        "minecraft:sticky_piston",
        "minecraft:dispenser",
        "minecraft:tripwire_hook",
        "minecraft:tripwire",
        "minecraft:chest",
    ]
    .into_iter()
    .collect();
    let mut final_state: std::collections::HashMap<[i32; 3], &str> =
        std::collections::HashMap::new();
    for block in blocks.iter() {
        final_state.insert(block.pos, block.state.as_str());
    }
    let expected = final_state
        .values()
        .filter(|state| {
            let name = state.split_once('[').map_or(**state, |(n, _)| n);
            temple_blocks.contains(name)
        })
        .count();
    // 3 chiseled + 3 lever + 1 repeater + 3 piston + 2 dispenser + 4 hook +
    // 5 tripwire + 2 chest + 14 stairs (`5,9,6`..`7,4,5` and the 8 descending
    // south stairs) — all in distinct positions, so nothing collapses. The literal
    // is a floor on that hand count, not a guess at the piece's size.
    assert!(
        expected >= 30,
        "the piece itself carries only {expected} signature blocks"
    );

    let placed = count_blocks(&with, start.bounding_box, &temple_blocks);
    let control = count_blocks(&without, start.bounding_box, &temple_blocks);
    assert_eq!(
        placed, expected,
        "the world holds {placed} of the piece's {expected} signature blocks"
    );
    assert_eq!(control, 0, "the structureless control holds {control}");
}

/// A coded piece's containers carry their loot table and vanilla's roll seed, and
/// the chest **block** really lands.
///
/// This is the gate on `coded:chests`' *corrected* claim. It fails in three
/// independent ways: a missing `StructurePiece::loot` entry, a chest block that the
/// alcove/air writes overwrote (last-write-wins order), and a wrong loot table id.
#[test]
fn a_coded_container_carries_its_loot_table_and_its_block() {
    let settings = settings();
    let with = generator(&ServerAssets::new(), &settings);
    for (chunk, structure, expected) in [
        (
            PYRAMID_CHUNK,
            "minecraft:desert_pyramid",
            vec!["minecraft:chests/desert_pyramid"; 4],
        ),
        (
            JUNGLE_CHUNK,
            "minecraft:jungle_pyramid",
            vec![
                "minecraft:chests/jungle_temple_dispenser",
                "minecraft:chests/jungle_temple_dispenser",
                "minecraft:chests/jungle_temple",
                "minecraft:chests/jungle_temple",
            ],
        ),
    ] {
        let start = start_at(&with, chunk, structure);
        let piece = &start.pieces[0];
        let tables: Vec<&str> = piece.loot.iter().map(|l| l.table.as_str()).collect();
        assert_eq!(tables, expected, "{structure}'s container loot tables");
        // Every seed is a distinct `nextLong()` off one stream; two equal seeds
        // would mean a re-seed, and two chests rolling identically.
        let mut seeds: Vec<i64> = piece.loot.iter().map(|l| l.seed).collect();
        seeds.sort_unstable();
        seeds.dedup();
        assert_eq!(seeds.len(), piece.loot.len(), "{structure} reused a roll seed");
        // The block is in the piece's *final* state at that position, not merely
        // written at some point.
        let mut final_state: std::collections::HashMap<[i32; 3], &str> =
            std::collections::HashMap::new();
        for block in piece.blocks.as_ref().expect("blocks").iter() {
            final_state.insert(block.pos, block.state.as_str());
        }
        for entry in &piece.loot {
            let state = final_state
                .get(&entry.pos)
                .unwrap_or_else(|| panic!("{structure} has no block at its loot pos {:?}", entry.pos));
            assert!(
                state.starts_with("minecraft:chest[")
                    || state.starts_with("minecraft:dispenser["),
                "{structure}'s loot at {:?} sits on {state}",
                entry.pos
            );
        }
    }
}

/// A coded piece is reproducible across two independently constructed generators —
/// the property the per-chunk clip rests on, and the one vanilla's
/// `level.getRandom()` cellar draws do **not** have.
#[test]
fn a_coded_piece_is_identical_across_generators() {
    let settings = settings();
    let a = generator(&ServerAssets::new(), &settings);
    let b = generator(&ServerAssets::new(), &settings);
    let first = start_at(&a, PYRAMID_CHUNK, "minecraft:desert_pyramid");
    let second = start_at(&b, PYRAMID_CHUNK, "minecraft:desert_pyramid");
    let left = first.pieces[0].blocks.as_ref().expect("blocks");
    let right = second.pieces[0].blocks.as_ref().expect("blocks");
    assert_eq!(left.len(), right.len());
    for (l, r) in left.iter().zip(right.iter()) {
        assert_eq!((l.pos, &l.state), (r.pos, &r.state));
    }
}
