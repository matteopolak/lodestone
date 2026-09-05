#![cfg(not(target_arch = "wasm32"))]

//! Independent entity-sidecar fixture checks for the bounded native importer.

use std::{collections::BTreeMap, path::{Path, PathBuf}};

use lodestone_anvil::{CompressionScheme, region};
use lodestone_core::{Nbt, NbtTag};
use lodestone_model::{ResourceKey, Rotation, Vec3};
use lodestone_server::{
    anvil_native_entity_import::{
        EntityImportAuthorization, EntityImportBlocker, EntityLossDecision, Error,
        UnsupportedEntityData, import_entity_chunk, preflight_entities,
    },
    entity_storage::{EntityStorage, SavedEntity},
    world_storage::{NativeEntityRecord, WorldStorage, WorldStorageBackend},
};
use lodestone_storage_schema::BuiltinDimension;
use uuid::Uuid;

fn scratch(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "lodestone-anvil-native-entity-import-{name}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create scratch directory");
    path
}

fn fixture_entity() -> Nbt {
    Nbt::Compound(vec![
        ("id".to_owned(), Nbt::String("minecraft:zombie".to_owned())),
        (
            "UUID".to_owned(),
            Nbt::IntArray(vec![
                0x0102_0304,
                0x0506_0708,
                0x090a_0b0c,
                0x0d0e_0f10,
            ]),
        ),
        (
            "Pos".to_owned(),
            Nbt::List {
                element_type: NbtTag::Double,
                elements: vec![Nbt::Double(96.25), Nbt::Double(64.5), Nbt::Double(192.75)],
            },
        ),
        (
            "Motion".to_owned(),
            Nbt::List {
                element_type: NbtTag::Double,
                elements: vec![Nbt::Double(0.1), Nbt::Double(-0.08), Nbt::Double(0.0)],
            },
        ),
        (
            "Rotation".to_owned(),
            Nbt::List {
                element_type: NbtTag::Float,
                elements: vec![Nbt::Float(-37.5), Nbt::Float(12.25)],
            },
        ),
        ("Health".to_owned(), Nbt::Float(17.0)),
        ("CustomName".to_owned(), Nbt::String("fixture sentinel".to_owned())),
    ])
}

fn fixture_chunk() -> Nbt {
    Nbt::Compound(vec![
        ("Position".to_owned(), Nbt::IntArray(vec![6, 12])),
        (
            "DataVersion".to_owned(),
            Nbt::Int(lodestone_anvil::level_dat::DATA_VERSION_26_2),
        ),
        (
            "Entities".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: vec![fixture_entity()],
            },
        ),
    ])
}

fn write_fixture_sidecar(world: &Path) -> EntityStorage {
    let sidecar = EntityStorage::new(world).expect("create source entity sidecar");
    let chunks = BTreeMap::from([((6, 12), fixture_chunk())]);
    let built = region::build_region_from_nbt(&chunks, CompressionScheme::Zlib, 1)
        .expect("build independent entity region fixture");
    assert!(built.external.is_empty(), "fixture stays in the region container");
    let path = world
        .join("dimensions/minecraft/overworld/entities/r.0.0.mca");
    std::fs::write(path, built.bytes).expect("write entity-sidecar fixture");
    sidecar
}

#[test]
fn independent_entity_sidecar_fixture_imports_native_pose_after_explicit_loss_acceptance() {
    let directory = scratch("fixture");
    let world = directory.join("world");
    let sidecar = write_fixture_sidecar(&world);
    let source_path = world.join("dimensions/minecraft/overworld/entities/r.0.0.mca");
    let source_before = std::fs::read(&source_path).expect("read fixture before import");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: directory.join("native"),
    })
    .expect("open native storage");

    let source = sidecar.load_chunk(6, 12).expect("decode fixture through entity codec");
    let report = preflight_entities(6, 12, -64, 384, &source);
    assert!(report.blockers().is_empty(), "fixture pose is resident");
    assert_eq!(
        report.unsupported(),
        &[
            UnsupportedEntityData::Motion { entity_index: 0 },
            UnsupportedEntityData::Health { entity_index: 0 },
            UnsupportedEntityData::PreservedFields {
                entity_index: 0,
                fields: 1,
            },
        ],
        "the fixture's discarded state must remain visible",
    );
    let authorization = report.decide(EntityLossDecision::ProceedAndDiscardUnsupported);

    let result = import_entity_chunk(&storage, &sidecar, 6, 12, -64, 384, Some(authorization))
        .expect("authorized fixture import");
    assert_eq!(result.entities_seen, 1);
    assert_eq!(result.records_written, 1);
    let uuid = Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10);
    assert_eq!(
        storage.load_entity(*uuid.as_bytes(), 6, 12, -64, 384).expect("load imported pose"),
        Some(NativeEntityRecord {
            uuid: *uuid.as_bytes(),
            entity_type: "minecraft:zombie".parse::<ResourceKey>().expect("fixture type key"),
            dimension: BuiltinDimension::Overworld,
            position: Vec3::new(96.25, 64.5, 192.75),
            rotation: Rotation::new(-37.5, 12.25),
        }),
        "native expectations are fixed independently of the conversion mapping",
    );
    assert_eq!(
        std::fs::read(source_path).expect("read fixture after import"),
        source_before,
        "native conversion must not rewrite the Anvil source sidecar",
    );

    drop(storage);
    std::fs::remove_dir_all(directory).expect("remove scratch directory");
}

#[test]
fn missing_or_stale_authorization_never_writes_a_native_entity() {
    let directory = scratch("authorization");
    let world = directory.join("world");
    let sidecar = write_fixture_sidecar(&world);
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: directory.join("native"),
    })
    .expect("open native storage");
    let uuid = Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10);

    assert!(matches!(
        import_entity_chunk(&storage, &sidecar, 6, 12, -64, 384, None),
        Err(Error::MissingAuthorization)
    ));
    assert!(matches!(
        import_entity_chunk(
            &storage,
            &sidecar,
            6,
            12,
            -64,
            384,
            Some(EntityImportAuthorization::Lossless),
        ),
        Err(Error::AuthorizationMismatch { .. })
    ));
    assert!(storage
        .load_entity(*uuid.as_bytes(), 6, 12, -64, 384)
        .expect("read native storage")
        .is_none());

    drop(storage);
    std::fs::remove_dir_all(directory).expect("remove scratch directory");
}

#[test]
fn preflight_blocks_an_entity_that_is_not_resident_in_the_selected_column() {
    let entity = SavedEntity {
        id: "minecraft:zombie".parse().expect("valid fixture type"),
        uuid: Uuid::from_u128(1),
        pos: Vec3::new(16.0, 64.0, 0.5),
        motion: Vec3::new(0.0, 0.0, 0.0),
        rotation: Rotation::new(0.0, 0.0),
        health: None,
        item: None,
        age: None,
        pickup_delay: None,
        extra: Vec::new(),
    };
    let report = preflight_entities(0, 0, -64, 384, &[entity]);
    assert_eq!(
        report.blockers(),
        &[EntityImportBlocker::OutsideColumn {
            entity_index: 0,
            x: 16,
            z: 0,
            expected_x: 0,
            expected_z: 0,
        }],
        "the preflight must reject the same residency mismatch as native storage",
    );
    assert_eq!(
        report.decide(EntityLossDecision::ProceedAndDiscardUnsupported),
        EntityImportAuthorization::Blocked { blockers: 1 },
    );
}
