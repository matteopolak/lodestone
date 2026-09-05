#![cfg(not(target_arch = "wasm32"))]

//! Filesystem-backed checks for deterministic aggregate terrain import.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use lodestone_anvil::{
    CompressionScheme,
    import_preflight::{ImportSource, LossDecision},
    region,
};
use lodestone_core::{Nbt, Reader, read_named_nbt};
use lodestone_server::{
    anvil_world_import::{Error, import_world_directory, preflight_world_directory},
    world_storage::{WorldStorage, WorldStorageBackend},
};

const CHUNK_FIXTURE: &[u8] = include_bytes!("support/vanilla_26_2_block_entity_chunk.nbt");

fn scratch(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "lodestone-anvil-world-import-{name}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create independent import fixture directory");
    path
}

fn fixture_chunk(column_x: i32, column_z: i32) -> Nbt {
    let mut reader = Reader::new(CHUNK_FIXTURE);
    let (name, mut chunk) = read_named_nbt(&mut reader).expect("checked-in fixture decodes");
    assert!(name.is_empty(), "fixture uses an unnamed root");
    reader.ensure_empty().expect("fixture has no trailing bytes");
    let Nbt::Compound(fields) = &mut chunk else {
        panic!("fixture root is a compound");
    };
    for (name, value) in fields {
        match name.as_str() {
            "xPos" => *value = Nbt::Int(column_x),
            "zPos" => *value = Nbt::Int(column_z),
            _ => {}
        }
    }
    chunk
}

fn invalid_palette(mut chunk: Nbt) -> Nbt {
    let Nbt::Compound(root) = &mut chunk else {
        panic!("fixture root is a compound");
    };
    let Some((_, Nbt::List { elements, .. })) = root
        .iter_mut()
        .find(|(name, _)| name == "sections")
    else {
        panic!("fixture contains sections");
    };
    for section in elements {
        let Nbt::Compound(fields) = section else {
            continue;
        };
        let in_extent = fields.iter().any(|(name, value)| {
            name == "Y" && matches!(value, Nbt::Byte(y) if (-4..20).contains(&i32::from(*y)))
        });
        if !in_extent {
            continue;
        }
        let Some((_, states)) = fields.iter_mut().find(|(name, _)| name == "block_states") else {
            continue;
        };
        *states = Nbt::Compound(Vec::new());
        return chunk;
    }
    panic!("fixture has an in-range block-state section");
}

fn write_region(
    world: &Path,
    region_x: i32,
    region_z: i32,
    chunks: BTreeMap<(i32, i32), Nbt>,
) {
    let built = region::build_region_from_nbt(&chunks, CompressionScheme::Zlib, 1)
        .expect("build independently selected region fixture");
    assert!(built.external.is_empty(), "fixture chunks stay inline");
    let directory = world.join("region");
    std::fs::create_dir_all(&directory).expect("create terrain region directory");
    std::fs::write(directory.join(format!("r.{region_x}.{region_z}.mca")), built.bytes)
        .expect("write terrain region fixture");
}

#[test]
fn independent_multi_region_fixture_imports_in_coordinate_order_under_one_authorization() {
    let world = scratch("source");
    let mut later = BTreeMap::new();
    later.insert((32, 0), fixture_chunk(32, 0));
    write_region(&world, 1, 0, later);
    let mut first = BTreeMap::new();
    first.insert((0, 0), fixture_chunk(0, 0));
    write_region(&world, 0, 0, first);

    let report = preflight_world_directory("minecraft:overworld", &world)
        .expect("walk independent filesystem regions");
    assert!(report.blockers().is_empty(), "fixture is representable");
    assert!(matches!(
        report.supported().first().map(|item| &item.location.source),
        Some(ImportSource::Chunk { x: 0, z: 0, .. })
    ));
    assert!(matches!(
        report.supported().last().map(|item| &item.location.source),
        Some(ImportSource::Chunk { x: 32, z: 0, .. })
    ));
    let authorization = report.decide(LossDecision::ProceedAndDiscardUnsupported);
    let native = scratch("native");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native.clone(),
    })
    .expect("open native backend");

    let result = import_world_directory(
        &storage,
        "minecraft:overworld",
        &world,
        -64,
        384,
        Some(authorization),
    )
    .expect("one aggregate authorization imports both regions");
    assert_eq!(result.report, report);
    assert_eq!(
        (result.regions_seen, result.chunks_seen, result.records_written),
        (2, 2, 2)
    );
    drop(storage);

    let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native.clone(),
    })
    .expect("reopen native destination");
    for (column_x, expected) in [
        (0, "minecraft:blast_furnace[facing=south,lit=false]"),
        (32, "minecraft:blast_furnace[facing=south,lit=false]"),
    ] {
        let chunk = reopened
            .load_chunk(column_x, 0, -64, 384)
            .expect("read imported terrain")
            .expect("every fixture chunk is committed");
        assert_eq!(chunk.column.block_state(1, -59, 7), expected);
    }

    let _ = std::fs::remove_dir_all(world);
    let _ = std::fs::remove_dir_all(native);
}

#[test]
fn later_region_preparation_failure_commits_no_earlier_region() {
    let world = scratch("bad-source");
    let mut valid = BTreeMap::new();
    valid.insert((0, 0), fixture_chunk(0, 0));
    write_region(&world, 0, 0, valid);
    let mut invalid = BTreeMap::new();
    invalid.insert((32, 0), invalid_palette(fixture_chunk(32, 0)));
    write_region(&world, 1, 0, invalid);
    let report = preflight_world_directory("minecraft:overworld", &world)
        .expect("coarse report reaches conversion preparation");
    assert!(report.blockers().is_empty());
    let native = scratch("bad-native");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native.clone(),
    })
    .expect("open native backend");

    assert!(matches!(
        import_world_directory(
            &storage,
            "minecraft:overworld",
            &world,
            -64,
            384,
            Some(report.decide(LossDecision::ProceedAndDiscardUnsupported)),
        ),
        Err(Error::Region(lodestone_server::anvil_import::Error::Chunk(_)))
    ));
    assert_eq!(
        std::fs::metadata(native.join("world.ls"))
            .expect("native open creates an empty segment")
            .len(),
        0,
        "the valid earlier region must not commit before later preparation succeeds"
    );

    drop(storage);
    let _ = std::fs::remove_dir_all(world);
    let _ = std::fs::remove_dir_all(native);
}

#[test]
fn noncanonical_terrain_file_is_not_silently_skipped() {
    let world = scratch("bad-name");
    let directory = world.join("region");
    std::fs::create_dir_all(&directory).expect("create region directory");
    std::fs::write(directory.join("terrain.mca"), []).expect("write ambiguous terrain file");

    assert!(matches!(
        preflight_world_directory("minecraft:overworld", &world),
        Err(Error::UnexpectedRegionFile { .. })
    ));
    let _ = std::fs::remove_dir_all(world);
}
