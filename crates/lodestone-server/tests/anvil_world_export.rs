#![cfg(not(target_arch = "wasm32"))]

//! Filesystem-backed controls for native terrain export staging and reopening.

use std::path::{Path, PathBuf};

use lodestone_anvil::{
    CompressionScheme,
    region::RegionFile,
};
use lodestone_core::{Nbt, Reader, read_named_nbt};
use lodestone_server::{
    ChunkColumn, ScheduledTickHandle, TickPriority,
    anvil_world_export::{
        ChunkCoordinate, Error, WorldExportInput, WorldExportLossDecision, export_world_directory,
        export_native_world_snapshot, preflight_native_world_export, preflight_world_export,
        snapshot_native_world_export, snapshot_world_export,
    },
    world_storage::{NativeDirtyChunkRecord, WorldStorage, WorldStorageBackend},
};

fn scratch(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "lodestone-anvil-world-export-{name}-{}-{unique}",
        std::process::id()
    ))
}

fn write_chunk(storage: &WorldStorage, x: i32, z: i32, state: &str, impossible_tick: bool) {
    let mut column = ChunkColumn::new(0, 16);
    column.set_block(1, 1, 2, state);
    let light = lodestone_world::ColumnLight::new(column.section_count());
    let scheduled = ScheduledTickHandle::new();
    if impossible_tick {
        scheduled.with(|queues| {
            assert!(queues.fluid.schedule(
                (x * 16, 1, z * 16),
                lodestone_server::fluid::TICK_FLUID.to_owned(),
                u64::MAX,
                TickPriority::Normal,
            ));
        });
    }
    storage
        .write_dirty_chunk(NativeDirtyChunkRecord::new(x, z, &column, &light, &scheduled))
        .expect("fixture typed chunk writes");
}

fn field<'a>(compound: &'a Nbt, name: &str) -> &'a Nbt {
    let Nbt::Compound(fields) = compound else {
        panic!("expected compound while finding {name}");
    };
    fields
        .iter()
        .find(|(field, _)| field == name)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("missing field {name}"))
}

fn contains_string(nbt: &Nbt, expected: &str) -> bool {
    match nbt {
        Nbt::String(value) => value == expected,
        Nbt::Compound(fields) => fields.iter().any(|(_, value)| contains_string(value, expected)),
        Nbt::List { elements, .. } => elements.iter().any(|value| contains_string(value, expected)),
        _ => false,
    }
}

fn read_chunk(path: &Path, local_x: u8, local_z: u8) -> Nbt {
    let region = RegionFile::read_from_file(path).expect("published region reopens");
    let bytes = region
        .read_chunk_nbt_bytes(local_x, local_z)
        .expect("published chunk reads")
        .expect("selected chunk is present");
    let mut reader = Reader::new(&bytes);
    let (name, chunk) = read_named_nbt(&mut reader).expect("published named NBT decodes");
    assert!(name.is_empty(), "terrain chunks use an empty named root");
    reader.ensure_empty().expect("published NBT has no trailing bytes");
    chunk
}

#[test]
fn explicit_multi_region_export_publishes_reopenable_terrain_in_coordinate_order() {
    let native_directory = scratch("native");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_directory.clone(),
    })
    .expect("open native fixture store");
    write_chunk(&storage, 32, 0, "minecraft:gold_block", false);
    write_chunk(&storage, 0, 0, "minecraft:diamond_block", false);

    let input = WorldExportInput::new(
        vec![ChunkCoordinate { x: 32, z: 0 }, ChunkCoordinate { x: 0, z: 0 }],
        0,
        16,
        400,
        CompressionScheme::Zlib,
        1_700_000_000,
    )
    .expect("fixture selection is valid");
    let report = preflight_world_export(&storage, &input).expect("selected chunks preflight");
    assert_eq!(
        report
            .chunks()
            .iter()
            .map(|chunk| chunk.coordinate)
            .collect::<Vec<_>>(),
        vec![ChunkCoordinate { x: 0, z: 0 }, ChunkCoordinate { x: 32, z: 0 }],
        "selection order cannot change region output order"
    );
    assert_eq!(report.unsupported_count(), 0);

    let destination = scratch("published");
    let result = export_world_directory(
        &storage,
        &input,
        &destination,
        Some(report.decide(WorldExportLossDecision::ProceedAndDiscardUnsupported)),
    )
    .expect("authorized selected terrain publishes");
    assert_eq!((result.chunks_exported, result.regions_published), (2, 2));
    assert!(destination.is_dir(), "the final rename publishes one directory");
    assert!(
        !destination
            .with_file_name(".lodestone-anvil-world-export-published.lodestone-export-staging")
            .exists(),
        "the published directory is not a reused staging tree"
    );

    for (region_x, expected_x, expected_state) in [
        (0, 0, "minecraft:diamond_block"),
        (1, 32, "minecraft:gold_block"),
    ] {
        let chunk = read_chunk(
            &destination.join("region").join(format!("r.{region_x}.0.mca")),
            0,
            0,
        );
        assert_eq!(field(&chunk, "xPos"), &Nbt::Int(expected_x));
        assert_eq!(field(&chunk, "zPos"), &Nbt::Int(0));
        assert!(
            contains_string(&chunk, expected_state),
            "reopened region {region_x} retains its selected block state"
        );
    }

    drop(storage);
    std::fs::remove_dir_all(native_directory).expect("remove native fixture store");
    std::fs::remove_dir_all(destination).expect("remove published fixture world");
}

#[test]
fn all_native_snapshot_exports_the_reviewed_records_after_a_later_native_write() {
    let native_directory = scratch("snapshot-native");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_directory.clone(),
    })
    .expect("open native fixture store");
    write_chunk(&storage, 32, 0, "minecraft:gold_block", false);
    write_chunk(&storage, 0, 0, "minecraft:diamond_block", false);

    let snapshot = snapshot_native_world_export(
        &storage,
        0,
        16,
        400,
        CompressionScheme::Zlib,
        1_700_000_000,
    )
    .expect("capture complete native terrain records");
    assert_eq!(
        snapshot.chunks(),
        &[ChunkCoordinate { x: 0, z: 0 }, ChunkCoordinate { x: 32, z: 0 }],
        "the native index order becomes the captured export order"
    );
    let report = preflight_native_world_export(&snapshot);
    assert_eq!(report.unsupported_count(), 0);

    write_chunk(&storage, 0, 0, "minecraft:emerald_block", false);
    let changed = storage
        .load_chunk(0, 0, 0, 16)
        .expect("read changed source")
        .expect("changed source remains present");
    assert_eq!(
        changed.column.block_state(1, 1, 2),
        "minecraft:emerald_block",
        "the control proves the store no longer contains the reviewed state"
    );

    let destination = scratch("snapshot-published");
    let result = export_native_world_snapshot(
        &snapshot,
        &destination,
        Some(report.decide(WorldExportLossDecision::ProceedAndDiscardUnsupported)),
    )
    .expect("the captured reviewed terrain publishes");
    assert_eq!((result.chunks_exported, result.regions_published), (2, 2));
    let chunk = read_chunk(&destination.join("region/r.0.0.mca"), 0, 0);
    assert!(
        contains_string(&chunk, "minecraft:diamond_block"),
        "publication must consume the reviewed point-in-time record"
    );
    assert!(
        !contains_string(&chunk, "minecraft:emerald_block"),
        "re-reading the later native replacement would fail this control"
    );

    drop(storage);
    std::fs::remove_dir_all(native_directory).expect("remove native fixture store");
    std::fs::remove_dir_all(destination).expect("remove published fixture world");
}

#[test]
fn explicit_snapshot_exports_its_reviewed_selection_after_later_native_writes() {
    let native_directory = scratch("selected-snapshot-native");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_directory.clone(),
    })
    .expect("open native fixture store");
    write_chunk(&storage, 0, 0, "minecraft:diamond_block", false);
    write_chunk(&storage, 32, 0, "minecraft:gold_block", false);
    let input = WorldExportInput::new(
        vec![ChunkCoordinate { x: 0, z: 0 }],
        0,
        16,
        400,
        CompressionScheme::Zlib,
        1_700_000_000,
    )
    .expect("one selected chunk is valid");
    let snapshot = snapshot_world_export(&storage, &input)
        .expect("capture the explicit reviewed selection");
    let report = preflight_native_world_export(&snapshot);
    assert_eq!(report.unsupported_count(), 0);

    write_chunk(&storage, 0, 0, "minecraft:emerald_block", false);
    write_chunk(&storage, 32, 0, "minecraft:netherite_block", false);
    let changed = storage
        .load_chunk(0, 0, 0, 16)
        .expect("read changed source")
        .expect("changed source remains present");
    assert_eq!(
        changed.column.block_state(1, 1, 2),
        "minecraft:emerald_block",
        "the control proves the selected source was replaced after capture"
    );

    let destination = scratch("selected-snapshot-published");
    let result = export_native_world_snapshot(
        &snapshot,
        &destination,
        Some(report.decide(WorldExportLossDecision::ProceedAndDiscardUnsupported)),
    )
    .expect("the captured explicit selection publishes");
    assert_eq!((result.chunks_exported, result.regions_published), (1, 1));
    let chunk = read_chunk(&destination.join("region/r.0.0.mca"), 0, 0);
    assert!(
        contains_string(&chunk, "minecraft:diamond_block"),
        "publication must consume the selected reviewed record"
    );
    assert!(
        !contains_string(&chunk, "minecraft:emerald_block"),
        "re-reading the selected replacement would fail this control"
    );
    assert!(
        !destination.join("region/r.1.0.mca").exists(),
        "the snapshot must not expand to terrain outside its explicit selection"
    );

    drop(storage);
    std::fs::remove_dir_all(native_directory).expect("remove native fixture store");
    std::fs::remove_dir_all(destination).expect("remove published fixture world");
}

#[test]
fn all_native_snapshot_refuses_an_empty_store() {
    let native_directory = scratch("empty-snapshot-native");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_directory.clone(),
    })
    .expect("open empty native fixture store");

    assert!(matches!(
        snapshot_native_world_export(&storage, 0, 16, 0, CompressionScheme::Zlib, 1),
        Err(Error::EmptySelection)
    ));
    drop(storage);
    std::fs::remove_dir_all(native_directory).expect("remove native fixture store");
}

#[test]
fn later_conversion_failure_does_not_create_a_published_or_staging_world() {
    let native_directory = scratch("bad-native");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_directory.clone(),
    })
    .expect("open native fixture store");
    write_chunk(&storage, 0, 0, "minecraft:diamond_block", false);
    write_chunk(&storage, 32, 0, "minecraft:gold_block", true);
    let input = WorldExportInput::new(
        vec![ChunkCoordinate { x: 0, z: 0 }, ChunkCoordinate { x: 32, z: 0 }],
        0,
        16,
        0,
        CompressionScheme::Zlib,
        1,
    )
    .expect("fixture selection is valid");
    let report = preflight_world_export(&storage, &input).expect("tick loss preflights");
    assert_eq!(report.unsupported_count(), 1, "the queued tick needs review");
    let destination = scratch("failed-publish");

    assert!(matches!(
        export_world_directory(
            &storage,
            &input,
            &destination,
            Some(report.decide(WorldExportLossDecision::ProceedAndDiscardUnsupported)),
        ),
        Err(Error::Chunk {
            coordinate: ChunkCoordinate { x: 32, z: 0 },
            source: lodestone_server::anvil_export::Error::TickDelayOutOfRange { .. },
        })
    ));
    assert!(
        !destination.exists(),
        "a later conversion failure cannot publish the earlier converted region"
    );
    assert!(
        !destination
            .with_file_name(".lodestone-anvil-world-export-failed-publish.lodestone-export-staging")
            .exists(),
        "conversion happens before staging-directory creation"
    );

    drop(storage);
    std::fs::remove_dir_all(native_directory).expect("remove native fixture store");
}

#[test]
fn stale_aggregate_authorization_refuses_before_creating_an_output_world() {
    let native_directory = scratch("stale-native");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_directory.clone(),
    })
    .expect("open native fixture store");
    write_chunk(&storage, 0, 0, "minecraft:diamond_block", false);
    write_chunk(&storage, 32, 0, "minecraft:gold_block", false);
    let input = WorldExportInput::new(
        vec![ChunkCoordinate { x: 0, z: 0 }, ChunkCoordinate { x: 32, z: 0 }],
        0,
        16,
        0,
        CompressionScheme::Zlib,
        1,
    )
    .expect("fixture selection is valid");
    let reviewed = preflight_world_export(&storage, &input).expect("lossless source preflights");
    let authorization = reviewed.decide(WorldExportLossDecision::ProceedAndDiscardUnsupported);

    write_chunk(&storage, 32, 0, "minecraft:gold_block", true);
    let destination = scratch("stale-publish");
    assert!(matches!(
        export_world_directory(&storage, &input, &destination, Some(authorization)),
        Err(Error::AuthorizationMismatch { .. })
    ));
    assert!(
        !destination.exists(),
        "a reviewed lossless report cannot authorize a newly lossy selection"
    );

    drop(storage);
    std::fs::remove_dir_all(native_directory).expect("remove native fixture store");
}
