#![cfg(not(target_arch = "wasm32"))]

//! The bounded Anvil import consumer reads checked-in source files, writes one
//! typed native world-properties record, and leaves the existing Anvil codec
//! usable for the same source bytes.

use std::path::PathBuf;

use lodestone_anvil::import_preflight::{ImportAuthorization, LossDecision, PreflightReport};
use lodestone_anvil::{level_dat, world_gen_settings};
use lodestone_server::anvil_import::{
    Error, WORLD_PROPERTIES_KEY, import_world_properties,
};
use lodestone_server::world_storage::{WorldStorage, WorldStorageBackend};
use lodestone_storage::NativeStore;
use lodestone_storage_schema::{BuiltinDimension, GameMode, generated::{general_record, storage_record}};

const LEVEL_DAT_FIXTURE: &[u8] =
    include_bytes!("../../lodestone-anvil/tests/support/level_dat_26_2_vanilla.dat");
const WORLD_GEN_FIXTURE: &[u8] = include_bytes!(
    "../../lodestone-anvil/tests/support/world_gen_settings_26_2_vanilla.dat"
);

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
