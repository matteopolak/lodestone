#![cfg(not(target_arch = "wasm32"))]

//! The bounded Anvil import consumer reads checked-in source files, writes one
//! typed native world-properties record, and leaves the existing Anvil codec
//! usable for the same source bytes.

use std::{collections::BTreeMap, path::PathBuf};

use lodestone_core::{Nbt, Reader, read_named_nbt};
use lodestone_anvil::import_preflight::{ImportAuthorization, LossDecision, PreflightReport};
use lodestone_anvil::{CompressionScheme, level_dat, region, world_gen_settings};
use lodestone_server::anvil_import::{
    Error, WORLD_PROPERTIES_KEY, import_chunk_bytes, import_region_file, import_world_properties,
    preflight_chunk, preflight_region_file,
};
use lodestone_server::world_storage::{WorldStorage, WorldStorageBackend};
use lodestone_storage::NativeStore;
use lodestone_storage_schema::{BuiltinDimension, GameMode, generated::{general_record, storage_record}};

const LEVEL_DAT_FIXTURE: &[u8] =
    include_bytes!("../../lodestone-anvil/tests/support/level_dat_26_2_vanilla.dat");
const WORLD_GEN_FIXTURE: &[u8] = include_bytes!(
    "../../lodestone-anvil/tests/support/world_gen_settings_26_2_vanilla.dat"
);
const CHUNK_FIXTURE: &[u8] =
    include_bytes!("support/vanilla_26_2_block_entity_chunk.nbt");

fn scratch(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "lodestone-anvil-import-{name}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create native import scratch directory");
    path
}

fn source() -> (
    level_dat::LevelDat,
    world_gen_settings::WorldGenSettings,
    ImportAuthorization,
) {
    let level = level_dat::read(LEVEL_DAT_FIXTURE).expect("checked-in level fixture decodes");
    let settings = world_gen_settings::read(WORLD_GEN_FIXTURE)
        .expect("checked-in world-gen fixture decodes");
    let mut builder = PreflightReport::builder();
    builder.inspect_level_dat(&level);
    builder.inspect_world_gen_settings(&settings);
    let report = builder.finish();
    assert!(report.blockers().is_empty(), "fixture must be importable");
    let authorization = report.decide(LossDecision::ProceedAndDiscardUnsupported);
    assert!(authorization.permits_conversion());
    (level, settings, authorization)
}

fn chunk_source() -> (Nbt, ImportAuthorization, PreflightReport) {
    let mut reader = Reader::new(CHUNK_FIXTURE);
    let (name, chunk) = read_named_nbt(&mut reader).expect("checked-in chunk fixture decodes");
    assert!(name.is_empty(), "chunk fixture uses an empty named-NBT root");
    reader.ensure_empty().expect("chunk fixture has no trailing bytes");
    let report = preflight_chunk("minecraft:overworld", 6, 12, &chunk);
    assert!(report.blockers().is_empty(), "fixture must be importable");
    let authorization = report.decide(LossDecision::ProceedAndDiscardUnsupported);
    assert!(authorization.permits_conversion());
    (chunk, authorization, report)
}

fn chunk_at(column_x: i32, column_z: i32) -> Nbt {
    let (mut chunk, _, _) = chunk_source();
    let Nbt::Compound(fields) = &mut chunk else {
        panic!("checked-in chunk fixture must have a compound root");
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

fn with_invalid_in_range_block_palette(mut chunk: Nbt) -> Nbt {
    let Nbt::Compound(root) = &mut chunk else {
        panic!("checked-in chunk fixture must have a compound root");
    };
    let Some((_, Nbt::List { elements, .. })) = root
        .iter_mut()
        .find(|(name, _)| name == "sections")
    else {
        panic!("checked-in chunk fixture must have sections");
    };
    for section in elements {
        let Nbt::Compound(fields) = section else {
            continue;
        };
        let y = fields
            .iter()
            .find(|(name, _)| name == "Y")
            .and_then(|(_, value)| match value {
                Nbt::Byte(y) => Some(i32::from(*y)),
                _ => None,
            });
        if !y.is_some_and(|y| (-4..20).contains(&y)) {
            continue;
        }
        if let Some((_, block_states)) = fields
            .iter_mut()
            .find(|(name, _)| name == "block_states")
        {
            *block_states = Nbt::Compound(Vec::new());
            return chunk;
        }
    }
    panic!("fixture must contain one in-range section with block states");
}

fn write_region(chunks: &BTreeMap<(i32, i32), Nbt>) -> (PathBuf, PathBuf) {
    let directory = scratch("region-source");
    let built = region::build_region_from_nbt(chunks, CompressionScheme::Zlib, 1)
        .expect("build checked-in chunks into an Anvil region");
    assert!(built.external.is_empty(), "small fixture chunks stay inline");
    let path = directory.join("r.0.0.mca");
    std::fs::write(&path, built.bytes).expect("write source region");
    (directory, path)
}

#[test]
fn checked_in_anvil_world_metadata_becomes_one_typed_native_record() {
    let (level, settings, authorization) = source();
    let level_before = level.clone();
    let settings_before = settings.clone();
    let directory = scratch("world-properties");

    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: directory.clone(),
    })
    .expect("open native backend");
    assert_eq!(
        import_world_properties(&storage, &level, &settings, Some(authorization))
            .expect("import one world-properties record"),
        1
    );
    assert_eq!(level, level_before, "conversion must not mutate Anvil metadata");
    assert_eq!(
        settings, settings_before,
        "conversion must not mutate Anvil world-generation metadata"
    );
    drop(storage);

    let mut native = NativeStore::open(&directory).expect("reopen native backend");
    let Some(record) = native
        .get(WORLD_PROPERTIES_KEY)
        .expect("read imported record")
    else {
        panic!("import must commit the bounded world-properties record");
    };
    let Some(storage_record::Record::General(general)) = record.record else {
        panic!("import must emit a general record");
    };
    assert!(general.extensions.is_empty(), "unknown Anvil fields are not extensions");
    let Some(general_record::Record::WorldProperties(properties)) = general.record else {
        panic!("import must emit world properties");
    };
    assert_eq!(properties.game_data_version, 4_903);
    assert_eq!(properties.seed, -195_764_831);
    assert_eq!(
        properties.spawn_dimension,
        BuiltinDimension::Overworld as i32
    );
    assert_eq!(
        (properties.spawn_x, properties.spawn_y, properties.spawn_z),
        (0, -60, 0)
    );
    assert_eq!(properties.default_game_mode, GameMode::Survival as i32);
    assert_eq!(properties.day_time, 0, "unsupported total-age field is not retained");

    // The same source bytes remain valid Anvil read/write inputs. This is a
    // structural round trip, not a claim that native storage replaces Anvil.
    let level_round_trip = level_dat::read(
        &level_dat::write(&level).expect("write original Anvil level metadata"),
    )
    .expect("read original Anvil level metadata again");
    let settings_round_trip = world_gen_settings::read(
        &world_gen_settings::write(&settings).expect("write original Anvil world-gen metadata"),
    )
    .expect("read original Anvil world-gen metadata again");
    assert_eq!(level_round_trip, level_before);
    assert_eq!(settings_round_trip, settings_before);

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn missing_or_stale_authorization_refuses_before_native_write() {
    let (level, settings, authorization) = source();
    let directory = scratch("authorization");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: directory.clone(),
    })
    .expect("open native backend");

    assert!(matches!(
        import_world_properties(&storage, &level, &settings, None),
        Err(Error::MissingAuthorization)
    ));
    assert!(matches!(
        import_world_properties(&storage, &level, &settings, Some(ImportAuthorization::Lossless)),
        Err(Error::AuthorizationMismatch { .. })
    ));
    assert!(authorization.permits_conversion());
    assert_eq!(
        std::fs::metadata(directory.join("world.ls"))
            .expect("native backend creates an empty segment on open")
            .len(),
        0,
        "authorization refusal must not append a native record"
    );

    drop(storage);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn anvil_backend_is_not_redirected_into_native_conversion() {
    let (level, settings, authorization) = source();
    let storage = WorldStorage::open(WorldStorageBackend::Anvil).expect("open Anvil backend");

    assert!(matches!(
        import_world_properties(&storage, &level, &settings, Some(authorization)),
        Err(Error::Storage(
            lodestone_server::world_storage::Error::AnvilDoesNotAcceptTypedRecords
        ))
    ));
}

#[test]
fn checked_in_anvil_chunk_maps_supported_terrain_and_reports_dropped_payloads() {
    let (_chunk, authorization, expected_report) = chunk_source();
    let directory = scratch("chunk");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: directory.clone(),
    })
    .expect("open native backend");

    let result = import_chunk_bytes(
        &storage,
        "minecraft:overworld",
        6,
        12,
        CHUNK_FIXTURE,
        -64,
        384,
        Some(authorization),
    )
    .expect("import one authorized chunk");
    assert_eq!(result.records_written, 1);
    assert_eq!(result.report, expected_report);
    let dropped: Vec<&str> = result
        .report
        .unsupported()
        .iter()
        .map(|item| item.location.path.as_str())
        .collect();
    for path in [
        "block_entities",
        "block_ticks",
        "fluid_ticks",
        "structures",
        "entities",
        "PostProcessing",
    ] {
        assert!(
            dropped.contains(&path),
            "unsupported source field {path:?} must be reported before it is dropped"
        );
    }
    drop(storage);

    let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative { directory: directory.clone() })
        .expect("reopen native backend");
    let loaded = reopened
        .load_chunk(6, 12, -64, 384)
        .expect("load imported chunk")
        .expect("imported chunk exists");
    assert_eq!(
        loaded.column.block_state(1, -59, 7),
        "minecraft:blast_furnace[facing=south,lit=false]"
    );
    assert!(
        loaded.column.motion_blocking().is_some(),
        "MOTION_BLOCKING from the checked-in chunk must reach the native record"
    );
    assert_eq!(loaded.light.light_section_count(), 26);
    assert!(
        (0..loaded.light.light_section_count())
            .all(|section| matches!(loaded.light.sky(section), lodestone_world::LightData::Missing)
                && matches!(loaded.light.block(section), lodestone_world::LightData::Missing)),
        "the fixture's absent light payload must remain explicit Missing light"
    );
    assert!(
        loaded.column.block_entities().is_empty(),
        "unsupported block-entity payload is reported and omitted from this bounded record"
    );

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn chunk_authorization_and_anvil_backend_are_fail_closed() {
    let (_chunk, authorization, _report) = chunk_source();
    let directory = scratch("chunk-authorization");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: directory.clone(),
    })
    .expect("open native backend");
    assert!(matches!(
        import_chunk_bytes(
            &storage,
            "minecraft:overworld",
            6,
            12,
            CHUNK_FIXTURE,
            -64,
            384,
            None,
        ),
        Err(Error::MissingAuthorization)
    ));
    assert!(matches!(
        import_chunk_bytes(
            &storage,
            "minecraft:overworld",
            6,
            12,
            CHUNK_FIXTURE,
            -64,
            384,
            Some(ImportAuthorization::Lossless),
        ),
        Err(Error::AuthorizationMismatch { .. })
    ));
    drop(storage);

    let storage = WorldStorage::open(WorldStorageBackend::Anvil).expect("open Anvil backend");
    assert!(matches!(
        import_chunk_bytes(
            &storage,
            "minecraft:overworld",
            6,
            12,
            CHUNK_FIXTURE,
            -64,
            384,
            Some(authorization),
        ),
        Err(Error::Storage(
            lodestone_server::world_storage::Error::AnvilDoesNotAcceptTypedRecords
        ))
    ));
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn region_file_import_uses_one_aggregate_loss_authorization_and_one_native_batch() {
    let mut chunks = BTreeMap::new();
    chunks.insert((6, 12), chunk_at(6, 12));
    chunks.insert((7, 12), chunk_at(7, 12));
    let (source_directory, region_path) = write_region(&chunks);
    let report = preflight_region_file("minecraft:overworld", 0, 0, &region_path)
        .expect("walk source region into one aggregate preflight");
    assert!(report.blockers().is_empty(), "both fixture chunks are importable");
    let (_one_chunk, _one_chunk_authorization, one_chunk_report) = chunk_source();
    assert_eq!(
        report.supported().len(),
        one_chunk_report.supported().len() * 2,
        "both region entries must contribute their typed fields to the aggregate report"
    );
    assert_eq!(
        report.unsupported().len(),
        one_chunk_report.unsupported().len() * 2,
        "loss acknowledgement must cover every dropped field in both region members"
    );
    let authorization = report.decide(LossDecision::ProceedAndDiscardUnsupported);
    let ImportAuthorization::LossAccepted { discarded_entries } = authorization else {
        panic!("fixture region deliberately has dropped block-entity and tick payloads");
    };
    assert_eq!(discarded_entries, report.unsupported().len());

    let native_directory = scratch("region-native");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_directory.clone(),
    })
    .expect("open native backend");
    let result = import_region_file(
        &storage,
        "minecraft:overworld",
        0,
        0,
        &region_path,
        -64,
        384,
        Some(authorization),
    )
    .expect("aggregate authorization imports both source chunks");
    assert_eq!(result.report, report);
    assert_eq!(result.chunks_seen, 2);
    assert_eq!(result.records_written, 2);
    drop(storage);

    let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_directory.clone(),
    })
    .expect("reopen native backend");
    for (column_x, column_z) in [(6, 12), (7, 12)] {
        let loaded = reopened
            .load_chunk(column_x, column_z, -64, 384)
            .expect("load imported region member")
            .expect("every present region member becomes a native record");
        assert_eq!(
            loaded.column.block_state(1, -59, 7),
            "minecraft:blast_furnace[facing=south,lit=false]"
        );
        assert!(loaded.column.block_entities().is_empty());
    }

    let _ = std::fs::remove_dir_all(source_directory);
    let _ = std::fs::remove_dir_all(native_directory);
}

#[test]
fn region_import_rejects_one_chunk_authorization_before_writing_any_member() {
    let mut chunks = BTreeMap::new();
    chunks.insert((6, 12), chunk_at(6, 12));
    chunks.insert((7, 12), chunk_at(7, 12));
    let (source_directory, region_path) = write_region(&chunks);
    let (_one_chunk, one_chunk_authorization, _report) = chunk_source();
    let native_directory = scratch("region-stale-authorization");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_directory.clone(),
    })
    .expect("open native backend");

    assert!(matches!(
        import_region_file(
            &storage,
            "minecraft:overworld",
            0,
            0,
            &region_path,
            -64,
            384,
            Some(one_chunk_authorization),
        ),
        Err(Error::AuthorizationMismatch { .. })
    ));
    assert_eq!(
        std::fs::metadata(native_directory.join("world.ls"))
            .expect("native backend creates an empty segment on open")
            .len(),
        0,
        "a non-aggregate authorization must fail before either region member is written"
    );

    drop(storage);
    let _ = std::fs::remove_dir_all(source_directory);
    let _ = std::fs::remove_dir_all(native_directory);
}

#[test]
fn region_import_prepares_every_member_before_starting_the_native_batch() {
    let mut chunks = BTreeMap::new();
    chunks.insert((6, 12), chunk_at(6, 12));
    chunks.insert(
        (7, 12),
        with_invalid_in_range_block_palette(chunk_at(7, 12)),
    );
    let (source_directory, region_path) = write_region(&chunks);
    let report = preflight_region_file("minecraft:overworld", 0, 0, &region_path)
        .expect("the coarse field preflight can classify both compound block-state fields");
    assert!(
        report.blockers().is_empty(),
        "the conversion control must reach preparation after aggregate authorization"
    );
    let authorization = report.decide(LossDecision::ProceedAndDiscardUnsupported);
    let native_directory = scratch("region-prepare-failure");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: native_directory.clone(),
    })
    .expect("open native backend");

    assert!(matches!(
        import_region_file(
            &storage,
            "minecraft:overworld",
            0,
            0,
            &region_path,
            -64,
            384,
            Some(authorization),
        ),
        Err(Error::Chunk(_))
    ));
    assert_eq!(
        std::fs::metadata(native_directory.join("world.ls"))
            .expect("native backend creates an empty segment on open")
            .len(),
        0,
        "a bad later region member must not leave its already-prepared predecessor committed"
    );

    drop(storage);
    let _ = std::fs::remove_dir_all(source_directory);
    let _ = std::fs::remove_dir_all(native_directory);
}
