//! Jigsaw structures reach **blocks**.
//!
//! The `where` comes from outside this repo: the two `minecraft:village_plains`
//! starts below are read out of the vanilla-authored survival oracle world's own
//! `structures.starts` NBT (`support/structure_starts_survival.txt`, seed
//! −195764831), and S1's placement gate already proves this engine puts a start in
//! exactly those chunks. What this file adds is that the start now *assembles* —
//! many pieces, a real joint graph — and that the assembled pieces write village
//! blocks into the generated column, with a structure-free generator over
//! identical data as the control.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use lodestone_worldgen::structure::StructureRegistry;
use serde_json::Value;

const SEED: i64 = -195_764_831;
/// `minecraft:village_plains` origin chunks, from the oracle census.
const VILLAGE_CHUNKS: [(i32, i32); 2] = [(-67, -57), (4, -44)];

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

/// Every bundled jigsaw structure loads its whole pool graph and is **absent**
/// from the ledger, and the engine gaps that are not structures are **present** on
/// it, each with a reason.
///
/// The negative half is the load-bearing half: a registry that silently demoted
/// `village_plains` would still pass every "the ledger names its gaps" assertion.
/// As of S5's Part A there is no jigsaw structure left to demote — the three that
/// were (`trial_chambers` on `pool_aliases`, `trail_ruins` on `capped`,
/// `bastion_remnant` on `axis_aligned_linear_pos`) are asserted **supported** here,
/// which is what stops a regression in any of the three from looking like data.
#[test]
fn the_jigsaw_structures_s4_models_are_not_on_the_ledger() {
    let registry = StructureRegistry::new(SEED, &ServerAssets::new());
    let ledger = registry.unsupported();
    for supported in [
        "minecraft:village_plains",
        "minecraft:village_desert",
        "minecraft:village_savanna",
        "minecraft:village_snowy",
        "minecraft:village_taiga",
        "minecraft:pillager_outpost",
        // Supported despite vanilla's own dangling wall reference — see the
        // `dangling:` ledger row asserted below.
        "minecraft:ancient_city",
        // The three S5 Part A closed, each by one named blocker.
        "minecraft:trial_chambers",
        "minecraft:trail_ruins",
        "minecraft:bastion_remnant",
    ] {
        assert!(
            !ledger.contains_key(supported),
            "{supported} was demoted: {:?}",
            ledger.get(supported)
        );
    }
    for (unsupported, expected) in [
        // The block-entity half of a `capped` archaeology rule: the suspicious
        // block is placed, its loot table is not.
        ("block_entity:append_loot", "append_loot"),
        // Vanilla's own data references
        // `ancient_city/walls/intact_horizontal_wall_stairs_5`
        // (vanilla's own ancient-city structure-pool data), of which only `_1`..`_4` ship in
        // `.cache/mc/26.2/src/data/minecraft/structure/`. Vanilla tolerates it
        // because its own template-manager get-or-create call invents an *empty*
        // template; so does this engine, and it says so here rather than deleting
        // the structure. The row exists **and** `ancient_city` is supported — both
        // halves matter.
        (
            "dangling:minecraft:ancient_city/walls/intact_horizontal_wall_stairs_5",
            "empty template",
        ),
    ] {
        let why = ledger
            .get(unsupported)
            .unwrap_or_else(|| panic!("{unsupported} must be named on the ledger"));
        assert!(
            why.contains(expected),
            "{unsupported} is ledgered for the wrong reason: {why}"
        );
    }
    // The weight expansion really happened, against a count derived from the
    // pool document rather than from this engine: `village/plains/town_centers`
    // has four elements at weight 50 and four at weight 1.
    let town_centers = registry
        .pools()
        .get("minecraft:village/plains/town_centers")
        .expect("the plains town-centre pool is loaded");
    assert_eq!(
        town_centers.size(),
        4 * 50 + 4,
        "the element list is not weight-expanded, so every shuffle of it draws the \
         wrong number of times"
    );
    // A pool three edges deep from the start pool, to show the closure is
    // transitive and not one ring.
    assert!(
        registry
            .pools()
            .get("minecraft:village/plains/houses")
            .is_some_and(|p| p.size() > 0),
        "the transitive closure stopped before the house pool"
    );
}

/// A village start assembles into **many** pieces with a real joint graph, and the
/// pieces carry the `PieceBeard` S3's beardifier reads.
#[test]
fn a_village_start_assembles_many_pieces_with_junctions() {
    let settings = settings();
    let generator = generator(&ServerAssets::new(), &settings);
    for (cx, cz) in VILLAGE_CHUNKS {
        let starts = generator.structure_starts(cx, cz);
        let village = starts
            .iter()
            .find(|s| s.structure == "minecraft:village_plains")
            .unwrap_or_else(|| {
                panic!("the oracle world has a village_plains start in chunk ({cx}, {cz})")
            });
        assert!(village.pieces_complete);
        assert!(
            village.pieces.len() > 5,
            "village at ({cx}, {cz}) assembled only {} piece(s) — the BFS stopped at \
             the town centre, which is what a broken `canAttach` or an unloaded pool \
             looks like",
            village.pieces.len()
        );
        let junctions: usize = village
            .pieces
            .iter()
            .filter_map(|p| p.beard.as_ref())
            .map(|b| b.junctions.len())
            .sum();
        assert!(
            junctions >= village.pieces.len(),
            "{} junctions over {} pieces: every attachment records one on each side, \
             so this cannot be fewer than the piece count",
            junctions,
            village.pieces.len()
        );
        // A village sprawls: its box must be wider than the origin chunk, which is
        // the property that makes `structure_references` (not `structure_starts`)
        // the right reader for jigsaw.
        let bb = village.bounding_box;
        assert!(
            bb.max[0] - bb.min[0] > 16 || bb.max[2] - bb.min[2] > 16,
            "village box {bb:?} fits inside one chunk"
        );
        // Both projections are exercised, or the gravity path is untested by this.
        let rigid = village
            .pieces
            .iter()
            .filter(|p| p.beard.as_ref().is_some_and(|b| b.rigid))
            .count();
        assert!(rigid > 0 && rigid < village.pieces.len(), "rigid {rigid}");
    }
}

/// Assembly is a pure function of `(seed, chunk)`: two independently constructed
/// generators produce byte-identical piece lists.
///
/// The point is not that our code is deterministic in the trivial sense — it is
/// that nothing in the BFS reads a `HashMap` iteration order or a shared mutable
/// cache. `PoolStore` is a `HashMap` and the free-space arena is index-based, so
/// this is a real question about this implementation and not a tautology.
#[test]
fn assembly_is_reproducible_across_generators() {
    let settings = settings();
    let describe = |g: &OverworldGenerator, cx: i32, cz: i32| {
        g.structure_starts(cx, cz)
            .iter()
            .flat_map(|s| {
                s.pieces.iter().map(move |p| {
                    (
                        s.structure.clone(),
                        p.template.clone(),
                        p.bounding_box,
                        p.beard.as_ref().map(|b| b.junctions.clone()),
                    )
                })
            })
            .collect::<Vec<_>>()
    };
    let (cx, cz) = VILLAGE_CHUNKS[1];
    let first = describe(&generator(&ServerAssets::new(), &settings), cx, cz);
    let second = describe(&generator(&ServerAssets::new(), &settings), cx, cz);
    assert!(!first.is_empty(), "nothing to compare");
    assert_eq!(first.len(), second.len());
    assert!(
        first == second,
        "two generators at the same seed produced different pieces"
    );
}

/// The assertion this unit exists for: at a chunk the vanilla oracle says has a
/// village, the generated columns contain village blocks, and the same columns with
/// no structure data do not.
///
/// The block set is chosen so that terrain, surface rules, carvers, ores and
/// vegetation cannot produce any of it — a village is the only source of a
/// `dirt_path` or an `oak_stairs` in this world.
#[test]
fn a_village_chunk_gains_village_blocks_a_structureless_chunk_does_not() {
    let settings = settings();
    let with = generator(&ServerAssets::new(), &settings);
    let without = generator(&NoStructures(ServerAssets::new()), &settings);
    let (cx, cz) = VILLAGE_CHUNKS[1];

    let village = with
        .structure_starts(cx, cz)
        .into_iter()
        .find(|s| s.structure == "minecraft:village_plains")
        .expect("village_plains start");
    assert!(
        village
            .pieces
            .iter()
            .any(|p| p.template.is_some() && p.placement.is_some()),
        "the start has no template-driven piece"
    );

    let village_blocks: HashSet<&str> = [
        "minecraft:dirt_path",
        "minecraft:cobblestone",
        "minecraft:mossy_cobblestone",
        "minecraft:oak_planks",
        "minecraft:oak_stairs",
        "minecraft:oak_fence",
        "minecraft:hay_block",
        "minecraft:bell",
        "minecraft:composter",
    ]
    .into_iter()
    .collect();
    // Only the chunks the pieces actually cover: a village's origin chunk is not
    // necessarily the one carrying most of it.
    let bb = village.bounding_box;
    let chunks: Vec<(i32, i32)> = ((bb.min[0] >> 4)..=(bb.max[0] >> 4))
        .flat_map(|x| ((bb.min[2] >> 4)..=(bb.max[2] >> 4)).map(move |z| (x, z)))
        .collect();
    let count = |g: &OverworldGenerator| {
        let mut n = 0usize;
        for &(x, z) in &chunks {
            let column = g.column(x, z);
            for lx in 0..16 {
                for lz in 0..16 {
                    for y in column.min_y()..(column.min_y() + column.height()) {
                        let state = column.block_state(lx, y, lz);
                        let name = state.split_once('[').map_or(state, |(n, _)| n);
                        if village_blocks.contains(name) {
                            n += 1;
                        }
                    }
                }
            }
        }
        n
    };
    let placed = count(&with);
    let control = count(&without);
    assert!(
        placed > 200 && control == 0,
        "village blocks over the start's {} chunks: {placed} (expected > 200), in the \
         structureless control: {control} (expected 0)",
        chunks.len()
    );

    // No jigsaw block survives placement — `JigsawReplacementProcessor` is not
    // optional, and its absence would leave command-block-textured jigsaws in
    // every wall.
    let mut jigsaws = 0usize;
    for &(x, z) in &chunks {
        let column = with.column(x, z);
        for lx in 0..16 {
            for lz in 0..16 {
                for y in column.min_y()..(column.min_y() + column.height()) {
                    if column.block_state(lx, y, lz).starts_with("minecraft:jigsaw") {
                        jigsaws += 1;
                    }
                }
            }
        }
    }
    assert_eq!(jigsaws, 0, "{jigsaws} jigsaw blocks survived placement");
}

/// S3's beardifier now actually fires: a chunk inside a village's reach reports a
/// non-empty beard, and the same chunk with no structure data reports an empty one.
///
/// Until S4 this was empty for **every** chunk in the world (S3's own negative
/// control asserted exactly that), so this is the assertion that S3 stopped being a
/// dormant subsystem.
#[test]
fn the_beardifier_is_non_empty_inside_a_village() {
    let settings = settings();
    let with = generator(&ServerAssets::new(), &settings);
    let (cx, cz) = VILLAGE_CHUNKS[1];
    let village = with
        .structure_starts(cx, cz)
        .into_iter()
        .find(|s| s.structure == "minecraft:village_plains")
        .expect("village_plains start");
    let bb = village.bounding_box;
    let mut bearded = 0usize;
    let mut with_rigid = 0usize;
    for x in (bb.min[0] >> 4)..=(bb.max[0] >> 4) {
        for z in (bb.min[2] >> 4)..=(bb.max[2] >> 4) {
            let beard = with.beardifier(x, z);
            if !beard.is_empty() {
                bearded += 1;
                with_rigid += usize::from(beard.rigid_count() > 0);
            }
        }
    }
    assert!(
        bearded > 0 && with_rigid > 0,
        "the beardifier is empty over the whole village: {bearded} bearded chunks, \
         {with_rigid} with a rigid box — S3 is still dormant"
    );
    // The control: the same chunks with no structure data have no beard at all, so
    // what was measured is the village and not a property of every chunk.
    let without = generator(&NoStructures(ServerAssets::new()), &settings);
    for x in (bb.min[0] >> 4)..=(bb.max[0] >> 4) {
        for z in (bb.min[2] >> 4)..=(bb.max[2] >> 4) {
            assert!(without.beardifier(x, z).is_empty());
        }
    }
}

/// `pillager_outpost` is the only supported structure with a `list_pool_element`
/// (its watchtower is two templates at one position), so it is the only thing that
/// exercises [`StructurePiece::extra_placements`].
///
/// The chunk is found by asking the *placement* engine which chunks its set
/// nominates and then walking them in order — the oracle world's generated area
/// contains no outpost, so there is no census entry to quote. Placement itself is
/// already gated against the oracle by S1; what is new here is the assembly.
#[test]
fn a_pillager_outpost_assembles_and_carries_a_list_element() {
    let registry = StructureRegistry::new(SEED, &ServerAssets::new());
    let towers = registry
        .pools()
        .get("minecraft:pillager_outpost/towers")
        .expect("the outpost tower pool is loaded");
    let lists = towers
        .expanded
        .iter()
        .filter(|e| {
            matches!(&***e, lodestone_worldgen::structure::pool::PoolElement::List { elements, .. }
                if elements.len() > 1)
        })
        .count();
    assert!(lists > 0, "the tower pool lost its list_pool_element");

    let set = registry
        .sets()
        .iter()
        .find(|s| s.id == "minecraft:pillager_outposts")
        .expect("the outpost set is bundled");
    let settings = settings();
    let generator = generator(&ServerAssets::new(), &settings);
    let mut tried = 0;
    // A wide window on purpose: `frequency: 0.2` with `legacy_type_1` reduction and
    // an exclusion zone against villages leaves only a few percent of the 32-chunk
    // grid, so a 128-chunk window really can contain none.
    for cx in -256..256 {
        for cz in -256..256 {
            if !registry.is_structure_chunk(set, cx, cz) {
                continue;
            }
            tried += 1;
            assert!(tried < 200, "200 placement chunks and no valid outpost biome");
            let Some(outpost) = generator
                .structure_starts(cx, cz)
                .into_iter()
                .find(|s| s.structure == "minecraft:pillager_outpost")
            else {
                continue;
            };
            assert!(outpost.pieces.len() > 2, "{} pieces", outpost.pieces.len());
            let extra: usize = outpost.pieces.iter().map(|p| p.extra_placements.len()).sum();
            assert!(
                extra > 0,
                "the outpost assembled {} pieces but no list element placed its \
                 second template",
                outpost.pieces.len()
            );
            return;
        }
    }
    panic!("no pillager_outpost placement chunk in the searched window");
}

/// `ancient_city` assembles too — the deep-slate arm of the same engine, and the
/// only supported structure that reaches it through vanilla's `start_jigsaw_name`
/// anchor rather than through the plain start position.
///
/// Also the only one whose `start_height` is a negative `absolute` and whose
/// `project_start_to_heightmap` is **absent**, so it is the arm that would catch a
/// centre piece projected onto the surface by mistake: a city at y ≈ −27 that
/// appeared at y ≈ 70 would still assemble and still place blocks.
#[test]
fn an_ancient_city_assembles_underground() {
    let registry = StructureRegistry::new(SEED, &ServerAssets::new());
    let set = registry
        .sets()
        .iter()
        .find(|s| s.id == "minecraft:ancient_cities")
        .expect("the ancient-city set is bundled");
    let settings = settings();
    let generator = generator(&ServerAssets::new(), &settings);
    let mut tried = 0;
    for cx in -256..256 {
        for cz in -256..256 {
            if !registry.is_structure_chunk(set, cx, cz) {
                continue;
            }
            tried += 1;
            assert!(tried < 400, "400 placement chunks and no deep_dark biome");
            let Some(city) = generator
                .structure_starts(cx, cz)
                .into_iter()
                .find(|s| s.structure == "minecraft:ancient_city")
            else {
                continue;
            };
            assert!(city.pieces.len() > 5, "{} pieces", city.pieces.len());
            // Deep underground, not on the surface: the city centre sits at
            // `start_height = -27` with no heightmap projection.
            assert!(
                city.bounding_box.min[1] < 0,
                "ancient city box {:?} is not underground — `project_start_to_heightmap` \
                 was applied where vanilla has none",
                city.bounding_box
            );
            return;
        }
    }
    panic!("no ancient_city placement chunk in the searched window");
}
