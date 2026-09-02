//! Template-driven structures reach **blocks**.
//!
//! The one thing about this unit that playing the game cannot spot-check: that a
//! served chunk at a known structure start really differs from the same chunk
//! generated with no structure data at all. Both arms are the production
//! generator over the real bundled assets, at a seed and chunk taken from the
//! vanilla-authored census in `support/structure_starts_survival.txt` — so the
//! *where* comes from outside this repo even though the *what* is ours.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use serde_json::Value;

/// The survival oracle world's seed, and a chunk its `structures.starts` says
/// carries `minecraft:ocean_ruin_cold` (a cold ruin is the strongest arm: three
/// stacked templates and a `BlockRotProcessor` on each).
const SEED: i64 = -195_764_831;
const RUIN_CHUNK: (i32, i32) = (5, 3);

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

fn generator(resolver: &dyn Resolver, settings: &Value) -> OverworldGenerator {
    OverworldGenerator::new(SEED, settings, resolver, "minecraft:plains", false)
}

fn settings() -> Value {
    let assets = ServerAssets::new();
    serde_json::from_str(
        &std::fs::read_to_string(assets.worldgen.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap()
}

/// Every template a supported kind can name is bundled and decodes, and the
/// shipwreck ones really do carry the 8 palettes a single-`palette` reader would
/// have missed.
#[test]
fn the_supported_kinds_load_their_templates() {
    let registry =
        lodestone_worldgen::structure::StructureRegistry::new(SEED, &ServerAssets::new());
    assert!(registry.unsupported().contains_key("minecraft:ruined_portal_nether"));
    assert!(!registry.unsupported().contains_key("minecraft:ruined_portal"));
    let templates = registry.templates();
    // 20 ocean + 11 beached shipwreck (9 shared), 4 warm + 4 big-warm, cold
    // brick/cracked/mossy 8+8+8 and big 4+4+4, plus 3 igloo parts.
    assert!(
        templates.len() > 60,
        "only {} templates loaded — a supported kind lost its list",
        templates.len()
    );
    let mast = templates
        .get("minecraft:shipwreck/with_mast")
        .expect("shipwreck/with_mast is bundled");
    assert_eq!(mast.size(), [9, 21, 28]);
}

/// All three wired kinds build pieces and those pieces write blocks.
///
/// Terrain-free: a flat [`StartContext`] over one biome, and the placement grid is
/// a bare chunk-sized box. That is deliberately *not* the gate below (a fake
/// context cannot tell you the pipeline calls any of this) — it is here because
/// the igloo path has no representative in the oracle world's census, and the
/// three kinds' settings differ in every field that matters: pivot, processor
/// chain, waterlogging, and how many pieces one start carries.
#[test]
fn every_wired_kind_writes_blocks() {
    use lodestone_worldgen::structure::{HeightmapKind, StartContext, StructureRegistry};

    struct Flat(&'static str);
    impl StartContext for Flat {
        fn first_occupied_height(&self, _x: i32, _z: i32, heightmap: HeightmapKind) -> i32 {
            match heightmap {
                // A shallow sea floor at 56 under water to 62, so an ocean
                // structure has somewhere to sit and a beached one does not
                // sink out of the grid.
                HeightmapKind::OceanFloorWg => 56,
                HeightmapKind::WorldSurfaceWg => 62,
            }
        }
        fn biome_at_quart(&self, _qx: i32, _qy: i32, _qz: i32) -> String {
            self.0.to_string()
        }
        fn sea_level(&self) -> i32 {
            63
        }
    }

    let registry = StructureRegistry::new(SEED, &ServerAssets::new());
    for (set_id, structure_id, biome) in [
        ("minecraft:shipwrecks", "minecraft:shipwreck", "minecraft:ocean"),
        ("minecraft:ocean_ruins", "minecraft:ocean_ruin_cold", "minecraft:cold_ocean"),
        ("minecraft:igloos", "minecraft:igloo", "minecraft:snowy_plains"),
    ] {
        let set = registry
            .sets()
            .iter()
            .find(|s| s.id == set_id)
            .unwrap_or_else(|| panic!("{set_id} is bundled"));
        let ctx = Flat(biome);
        // The first placement chunk of this set — pure RNG, no terrain.
        let (cx, cz) = (0..256)
            .flat_map(|x| (0..256).map(move |z| (x, z)))
            .find(|&(x, z)| registry.is_structure_chunk(set, x, z))
            .expect("some chunk in a 256x256 window is a placement chunk");
        let starts = registry.starts_at(cx, cz, &ctx);
        let start = starts
            .iter()
            .find(|s| s.structure == structure_id)
            .unwrap_or_else(|| panic!("{structure_id} did not start at its own placement chunk"));
        assert!(start.pieces_complete, "{structure_id} reports incomplete pieces");

        let mut written = 0usize;
        for piece in &start.pieces {
            let placement = piece
                .placement
                .as_ref()
                .unwrap_or_else(|| panic!("{structure_id} piece {} has no placement", piece.id));
            // One grid per piece, positioned on the piece rather than on the
            // chunk, so this measures the template and not the clip.
            let mut grid = lodestone_worldgen::dense_grid::DenseBlockGrid::new(
                piece.bounding_box.min[0],
                piece.bounding_box.min[1],
                piece.bounding_box.min[2],
                piece.bounding_box.max[0] - piece.bounding_box.min[0] + 1,
                piece.bounding_box.max[1] - piece.bounding_box.min[1] + 1,
                piece.bounding_box.max[2] - piece.bounding_box.min[2] + 1,
                "minecraft:air",
            );
            let origin = lodestone_worldgen::structure::template::PlaceOrigin {
                position: placement.position,
                reference: lodestone_worldgen::structure::jigsaw::reference_position(&start.pieces),
                seed: SEED,
            };
            written += placement
                .template
                .place(origin, &placement.settings, &mut grid);
        }
        assert!(
            written > 0,
            "{structure_id} generated {} pieces but wrote no blocks",
            start.pieces.len()
        );
    }
}

/// The assertion this unit exists for: at a chunk the vanilla oracle says has an
/// ocean ruin, the generated column contains ruin blocks, and the same chunk with
/// no structure data does not.
///
#[test]
fn a_start_chunk_gains_blocks_a_structureless_chunk_does_not() {
    let settings = settings();
    let with = generator(&ServerAssets::new(), &settings);
    let (cx, cz) = RUIN_CHUNK;

    let starts = with.structure_starts(cx, cz);
    let ruin = starts
        .iter()
        .find(|s| s.structure == "minecraft:ocean_ruin_cold")
        .expect("the oracle world has an ocean_ruin_cold start in this chunk");
    assert!(
        ruin.pieces.iter().any(|p| p.template.is_some() && p.placement.is_some()),
        "the start has no template-driven piece: {:?}",
        ruin.pieces.iter().map(|p| &p.template).collect::<Vec<_>>()
    );

    // The chunks the pieces actually cover. A rotated ocean ruin hangs off its
    // origin chunk — vanilla's settings give it no pivot, so a `CLOCKWISE_180`
    // ruin extends *negatively* from the chunk's min corner — so sweeping only
    // `(cx, cz)` would measure one column of gravel and read as a failure.
    let bb = ruin.bounding_box;
    let chunks: Vec<(i32, i32)> = ((bb.min[0] >> 4)..=(bb.max[0] >> 4))
        .flat_map(|x| ((bb.min[2] >> 4)..=(bb.max[2] >> 4)).map(move |z| (x, z)))
        .collect();

    // Cold-ruin materials, none of which terrain, surface rules, carvers, ores or
    // vegetation can produce.
    let ruin_blocks: HashSet<&str> = [
        "minecraft:stone_bricks",
        "minecraft:cracked_stone_bricks",
        "minecraft:mossy_stone_bricks",
        "minecraft:sea_lantern",
    ]
    .into_iter()
    .collect();
    let count = |generator: &OverworldGenerator| {
        let mut n = 0usize;
        for &(x, z) in &chunks {
            let column = generator.column(x, z);
            for lx in 0..16 {
                for lz in 0..16 {
                    for y in column.min_y()..(column.min_y() + column.height()) {
                        if ruin_blocks.contains(column.block_state(lx, y, lz)) {
                            n += 1;
                        }
                    }
                }
            }
        }
        n
    };

    let placed = count(&with);
    let control = count(&generator(&NoStructures(ServerAssets::new()), &settings));
    assert!(
        placed > 0 && control == 0,
        "ruin blocks over the start's {} chunks: {placed} (expected > 0), in the structureless \
         control: {control} (expected 0)",
        chunks.len()
    );
}
