//! Entities that structures spawn in the 26.3 Overworld the game serves.
//!
//! A structure's entities are decided where its blocks are and spawn once, when the chunk holding
//! their block cell is generated. The served column carries them as one-shot generation spawns;
//! these tests read them back off the column the production source returns.

use std::io::Read as _;

use lodestone_core::{Nbt, Reader};
use lodestone_server::{ChunkColumn, ChunkSource, Terrain263ChunkSource, WorldType, overworld_chunk_source_of_type};
use lodestone_worldgen::structure::{StructureEntity, StructureStart};

/// Swamp huts of seed 42 (`fixtures/world-oracle-26-3/structure-starts-42.txt`): the chunk and
/// the horizontal box the real 26.3 server recorded.
const SWAMP_HUTS: [((i32, i32), [i32; 4]); 2] = [((-51, -23), [-816, -368, -808, -362]), ((-17, 22), [-272, 352, -264, 358])];

fn source() -> lodestone_server::Terrain263ChunkSource {
    overworld_chunk_source_of_type(42, WorldType::Overworld)
}

/// The structure entities of a freshly generated column, draining its batch.
fn structure_entities(column: &ChunkColumn) -> Vec<StructureEntity> {
    column
        .generation_spawn_batch()
        .map(|batch| batch.take_legacy())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|spawn| spawn.structure.map(|entity| (*entity).clone()))
        .collect()
}

fn flag(entity: &StructureEntity, key: &str) -> bool {
    matches!(entity.get(key), Some(Nbt::Byte(1)))
}

#[test]
fn a_swamp_hut_spawns_a_persistent_witch_and_black_cat_in_its_hut() {
    let source = source();
    for ((cx, cz), [min_x, min_z, max_x, max_z]) in SWAMP_HUTS {
        let column = source.column(cx, cz);
        let start = column
            .structure_starts()
            .iter()
            .find(|start| start.structure == "minecraft:swamp_hut")
            .expect("the hut starts in its chunk");
        let piece = &start.pieces[0];
        let (box_min_y, orientation) = (piece.bounding_box.min[1], piece.orientation.expect("a hut is oriented"));
        // Local (2, 2, 5) of a 7-wide, 9-deep hut: the world cell for each facing (0 south,
        // 1 west, 2 north, 3 east), from the recorded horizontal box.
        let (x, z) = match orientation {
            0 => (min_x + 2, min_z + 5),
            2 => (min_x + 2, max_z - 5),
            1 => (max_x - 5, min_z + 2),
            3 => (min_x + 5, min_z + 2),
            other => panic!("unexpected orientation {other}"),
        };
        let spawned = structure_entities(&column);
        let ids: Vec<&str> = spawned.iter().map(StructureEntity::id).collect();
        assert_eq!(ids, ["minecraft:witch", "minecraft:cat"], "hut at {:?}", (cx, cz));
        for entity in &spawned {
            assert_eq!(entity.pos(), [f64::from(x) + 0.5, f64::from(box_min_y + 2), f64::from(z) + 0.5]);
            assert!(flag(entity, "PersistenceRequired"));
        }
        assert_eq!(spawned[1].get("variant"), Some(&Nbt::String("minecraft:black".into())));
    }
}

/// The ledger of generated chunks is per world: a column generated a second time proposes no
/// structure entities, so a witch is not respawned.
#[test]
fn a_regenerated_column_spawns_no_structure_entity_twice() {
    let source = source();
    let (cx, cz) = SWAMP_HUTS[0].0;
    assert_eq!(structure_entities(&source.column(cx, cz)).len(), 2);
    assert!(structure_entities(&source.column(cx, cz)).is_empty());
}

/// Every structure entity of the columns covering the horizontal box `[x0, z0, x1, z1]`, with the
/// chunk that spawned each.
fn spawned_in(source: &Terrain263ChunkSource, [x0, z0, x1, z1]: [i32; 4]) -> Vec<((i32, i32), StructureEntity)> {
    let mut out = Vec::new();
    for cz in (z0 >> 4)..=(z1 >> 4) {
        for cx in (x0 >> 4)..=(x1 >> 4) {
            out.extend(structure_entities(&source.column(cx, cz)).into_iter().map(|entity| ((cx, cz), entity)));
        }
    }
    out
}

/// A template file's decoded root, read straight from the bundled assets.
fn template_root(id: &str) -> Vec<(String, Nbt)> {
    let path = format!("{}/assets/structure/{}.nbt", env!("CARGO_MANIFEST_DIR"), id.strip_prefix("minecraft:").unwrap_or(id));
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(&path).unwrap_or_else(|e| panic!("{path}: {e}")))
        .read_to_end(&mut raw)
        .expect("gzip template");
    match lodestone_core::read_named_nbt(&mut Reader::new(&raw)).expect("template nbt").1 {
        Nbt::Compound(fields) => fields,
        other => panic!("template root is {other:?}"),
    }
}

fn list<'a>(fields: &'a [(String, Nbt)], key: &str) -> &'a [Nbt] {
    match fields.iter().find(|(name, _)| name == key) {
        Some((_, Nbt::List { elements, .. })) => elements,
        _ => &[],
    }
}

/// How many entities a template file lists.
fn template_entity_count(id: &str) -> usize {
    list(&template_root(id), "entities").len()
}

/// How many DATA markers of a template file have metadata starting with `prefix`.
fn marker_count(id: &str, prefix: &str) -> usize {
    let root = template_root(id);
    list(&root, "blocks")
        .iter()
        .filter(|block| {
            let Nbt::Compound(block) = block else { return false };
            let Some((_, Nbt::Compound(nbt))) = block.iter().find(|(name, _)| name == "nbt") else { return false };
            matches!(nbt.iter().find(|(name, _)| name == "metadata"), Some((_, Nbt::String(text))) if text.starts_with(prefix))
        })
        .count()
}

fn start_of<'a>(column: &'a ChunkColumn, structure: &str) -> &'a StructureStart {
    column.structure_starts().iter().find(|start| start.structure == structure).unwrap_or_else(|| panic!("no {structure} start"))
}

/// The ocean monument at chunk (49, -11) of seed 42 holds one elder guardian in its penthouse
/// and one in each wing room, at the cell the room's design names, centred in the cell.
#[test]
fn an_ocean_monument_spawns_an_elder_guardian_in_its_penthouse_and_each_wing_room() {
    let source = source();
    // Horizontal box the real 26.3 server recorded for the start.
    let spawned = spawned_in(&source, [755, -205, 812, -148]);
    let elders: Vec<&StructureEntity> = spawned.iter().map(|(_, e)| e).filter(|e| e.id() == "minecraft:elder_guardian").collect();
    assert_eq!(elders.len(), spawned.len(), "a monument spawns nothing but elders");

    let column = source.column(49, -11);
    let start = start_of(&column, "minecraft:monument");
    // The cell a local position lands in: x runs along the box for a north or south piece and
    // across it for a west or east one.
    let cell = |piece: &lodestone_worldgen::structure::StructurePiece, [x, y, z]: [i32; 3]| {
        let (min, max) = (piece.bounding_box.min, piece.bounding_box.max);
        let (wx, wz) = match piece.orientation.expect("a monument room is oriented") {
            0 => (min[0] + x, min[2] + z),
            2 => (min[0] + x, max[2] - z),
            1 => (max[0] - z, min[2] + x),
            _ => (min[0] + z, min[2] + x),
        };
        [f64::from(wx) + 0.5, f64::from(min[1] + y), f64::from(wz) + 0.5]
    };
    let mut expected = Vec::new();
    for piece in &start.pieces {
        match piece.id.as_str() {
            "minecraft:ompenthouse" => expected.push(vec![cell(piece, [6, 1, 6])]),
            // Design 0 spawns at (11, 2, 16), design 1 at (11, 5, 13).
            "minecraft:omwr" => expected.push(vec![cell(piece, [11, 2, 16]), cell(piece, [11, 5, 13])]),
            _ => {}
        }
    }
    assert!(expected.len() >= 2, "a monument has a penthouse and wing rooms");
    assert_eq!(elders.len(), expected.len());
    for candidates in &expected {
        assert!(
            elders.iter().any(|elder| candidates.contains(&elder.pos())),
            "no elder guardian at any of {candidates:?}; got {:?}",
            elders.iter().map(|e| e.pos()).collect::<Vec<_>>()
        );
    }
    assert!(elders.iter().all(|e| !flag(e, "PersistenceRequired")), "elder guardians despawn like any monster");
}

/// A mineshaft corridor's chest minecart rides a rail cell it just placed and holds its loot
/// table unrolled with the container's seed.
#[test]
fn mineshaft_chest_minecarts_ride_rails_and_hold_the_mineshaft_loot_table() {
    let source = source();
    // Seed 42's mineshaft at chunk (-3, -17); the real server recorded its box.
    let carts = spawned_in(&source, [-131, -344, 31, -180]);
    assert!(carts.len() >= 5, "the shaft holds several carts, got {}", carts.len());
    for ((cx, cz), cart) in &carts {
        assert_eq!(cart.id(), "minecraft:chest_minecart");
        let [x, y, z] = cart.block();
        assert_eq!((x >> 4, z >> 4), (*cx, *cz), "spawned by the chunk holding its cell");
        let [px, py, pz] = cart.pos();
        assert_eq!((px - px.floor(), py - py.floor(), pz - pz.floor()), (0.5, 0.5, 0.5), "set at the rail cell's centre height");
        let column = source.column(*cx, *cz);
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        assert_eq!(column.block_state_id(lx, y, lz).block(), lodestone_data::block::Block::Rail, "{cart:?}");
        assert_ne!(column.block_state_id(lx, y - 1, lz).block(), lodestone_data::block::Block::Air);
        assert_eq!(cart.get("LootTable"), Some(&Nbt::String("minecraft:chests/abandoned_mineshaft".into())));
        assert!(matches!(cart.get("LootTableSeed"), Some(Nbt::Long(_))));
    }
    let seeds: std::collections::BTreeSet<i64> = carts
        .iter()
        .filter_map(|(_, cart)| match cart.get("LootTableSeed") {
            Some(Nbt::Long(seed)) => Some(*seed),
            _ => None,
        })
        .collect();
    assert_eq!(seeds.len(), carts.len(), "every container draws its own seed");
}

/// An ocean ruin spawns one persistent drowned per drowned marker in its templates, and the
/// marker cell becomes water below sea level.
#[test]
fn an_ocean_ruin_spawns_a_persistent_drowned_per_marker_and_floods_the_marker() {
    let source = source();
    // Collected first: a column regenerated later spawns nothing, the world's once-only ledger.
    let drowned = spawned_in(&source, [-340, -1010, -220, -910]);
    let column = source.column(-17, -60);
    let start = start_of(&column, "minecraft:ocean_ruin_cold");
    let markers: usize = start
        .pieces
        .iter()
        .filter_map(|piece| piece.template.as_deref())
        .map(|template| marker_count(template, "drowned"))
        .sum();
    assert!(markers > 0, "the ruin's templates carry drowned markers");
    // The recorded horizontal box of the real server's start.
    assert_eq!(drowned.len(), markers);
    let mut flooded = 0;
    for ((cx, cz), entity) in &drowned {
        assert_eq!(entity.id(), "minecraft:drowned");
        assert!(flag(entity, "PersistenceRequired"));
        let [x, y, z] = entity.block();
        let block = source.column(*cx, *cz).block_state_id(x.rem_euclid(16), y, z.rem_euclid(16)).name();
        assert_ne!(block, "minecraft:structure_block", "marker consumed at {:?}", (x, y, z));
        if block == "minecraft:water" {
            flooded += 1;
        } else {
            // Only a cell another piece or the seabed rewrote later may differ.
            assert!(block != "minecraft:air" || y > 63, "{block} at {:?}", (x, y, z));
        }
    }
    assert!(flooded + 1 >= markers, "markers below sea level are flooded, {flooded}/{markers}");
}

/// A village golem lands where the template's own position, turned by the piece's rotation about
/// the placement position, puts it. The expected cell is derived here from the template file's
/// numbers and the quarter-turn rule, not from the engine's transform.
#[test]
fn a_village_template_entity_is_turned_with_its_piece() {
    let source = source();
    let spawned = spawned_in(&source, [559, -396, 732, -216]);
    let golems: Vec<&StructureEntity> = spawned.iter().map(|(_, e)| e).filter(|e| e.id() == "minecraft:iron_golem").collect();
    assert_eq!(golems.len(), 1);
    let column = source.column(41, -19);
    let start = start_of(&column, "minecraft:village_plains");
    let piece = start
        .pieces
        .iter()
        .find(|piece| piece.template.as_deref() == Some("minecraft:village/common/iron_golem"))
        .expect("the village has its golem piece");
    let placement = piece.placement.as_ref().expect("a template piece");
    // Template file: iron_golem.nbt, entity pos [0.55, 1.0, 0.44] in a 1x3x1 template.
    let (fx, fy, fz) = (0.5499999999999972_f64, 1.0_f64, 0.4400000000000013_f64);
    let [px, py, pz] = placement.position;
    let (dx, dz) = match format!("{:?}", placement.settings.rotation).as_str() {
        "None" => (fx, fz),
        "Cw90" => (1.0 - fz, fx),
        "Cw180" => (1.0 - fx, 1.0 - fz),
        "Ccw90" => (fz, 1.0 - fx),
        other => panic!("rotation {other}"),
    };
    assert_eq!(format!("{:?}", placement.settings.mirror), "None");
    let want = [f64::from(px) + dx, f64::from(py) + fy, f64::from(pz) + dz];
    let got = golems[0].pos();
    for axis in 0..3 {
        assert!((got[axis] - want[axis]).abs() < 1e-9, "axis {axis}: got {got:?}, want {want:?}");
    }
}

/// A bastion's mob pieces spawn exactly the entities their template files list, in the Nether.
#[test]
fn bastion_mob_templates_spawn_their_listed_entities() {
    let nether = lodestone_server::nether_chunk_source(42);
    // Bastion start at chunk (-25, -43); the box covers its chunks.
    let spawned = spawned_in(&nether, [-447, -708, -400, -659]);
    let column = nether.column(-25, -43);
    let start = start_of(&column, "minecraft:bastion_remnant");
    let listed: usize = start
        .pieces
        .iter()
        .filter_map(|piece| piece.template.as_deref())
        .map(template_entity_count)
        .sum();
    assert!(listed > 20, "the bastion has many mob pieces, got {listed}");
    assert_eq!(spawned.len(), listed);
    assert!(spawned.iter().all(|(_, e)| matches!(e.id(), "minecraft:piglin" | "minecraft:piglin_brute" | "minecraft:hoglin")));
}

/// An End city spawns a shulker for each `Sentry` marker its templates carry.
#[test]
fn an_end_city_spawns_a_shulker_per_sentry_marker() {
    let end = lodestone_server::end_chunk_source(42);
    let spawned = spawned_in(&end, [-660, -1330, -480, -1150]);
    let column = end.column(-36, -78);
    let start = start_of(&column, "minecraft:end_city");
    let sentries: usize = start
        .pieces
        .iter()
        .filter_map(|piece| piece.template.as_deref())
        .map(|template| marker_count(template, "Sentry"))
        .sum();
    assert!(sentries > 0);
    assert_eq!(spawned.len(), sentries);
    assert!(spawned.iter().all(|(_, e)| e.id() == "minecraft:shulker" && !flag(e, "PersistenceRequired")));
}
