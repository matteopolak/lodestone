//! A like-for-like incremental world-save comparison for the native store and redb.
//!
//! Run locally on an otherwise idle machine:
//! `cargo bench -p lodestone-storage --bench incremental -- --sample-size 20`.
//! Criterion reports logical write throughput. Before timing, this harness also
//! prints each engine's seeded size, post-save size, and growth for the exact
//! workload; it makes no engine-selection decision from one run.

use std::{fs, time::Instant};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use lodestone_storage::{ExtensionRegistration, NativeStore, RecordKey, RecordKind, RecordWrite};
use lodestone_storage_schema::{
    ChunkRecord, ChunkSection, EntityRecord, ExtensionValue, GeneralRecord, PlayerRecord,
    ScheduledTick, ScheduledTickKind, ScheduledTickPriority, StorageRecord, WorldProperties,
    generated::{general_record, storage_record},
    validate_record,
};
use prost::Message;
use redb::{ReadableDatabase, TableDefinition};
use tempfile::TempDir;

const COLUMNS: i32 = 128;
const CHUNK_BYTES: usize = 24 * 1024;
const PLAYER_EXTENSION_BYTES: usize = 192;
const BLOCK_ENTITY_BYTES: usize = 768;
const RECORDS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("records");

fn chunk_key(column: i32) -> RecordKey {
    RecordKey::chunk(column, column / 16)
}

fn deterministic_bytes(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|offset| seed.wrapping_add((offset % 251) as u8))
        .collect()
}

fn chunk(column: i32, seed: u8) -> StorageRecord {
    StorageRecord {
        format_version: 1,
        record: Some(storage_record::Record::Chunk(ChunkRecord {
            column_x: column,
            column_z: column / 16,
            game_data_version: 46_002,
            sections: vec![ChunkSection {
                section_y: 0,
                palette_bits: 1,
                palette_state_ids: vec![u32::from(seed) + 1],
                block_state_indices: deterministic_bytes(seed, CHUNK_BYTES),
                sky_light: vec![],
                block_light: vec![],
            }],
            biome_sections: vec![],
            surface_biome_ids: vec![],
            motion_blocking_heights: vec![],
            block_entity_nbt: vec![],
            block_scheduled_ticks: vec![],
            extensions: vec![],
            fluid_scheduled_ticks: vec![],
            light_sections: vec![],
        })),
    }
}

fn general(key: RecordKey, record: general_record::Record) -> RecordWrite {
    RecordWrite::new(
        key,
        StorageRecord {
            format_version: 1,
            record: Some(storage_record::Record::General(GeneralRecord {
                record: Some(record),
                extensions: vec![],
            })),
        },
    )
}

fn player_update() -> RecordWrite {
    RecordWrite::new(
        RecordKey {
            column_x: 37,
            column_z: 2,
            local_id: 17,
            kind: RecordKind::General,
        },
        StorageRecord {
            format_version: 1,
            record: Some(storage_record::Record::General(GeneralRecord {
                record: Some(general_record::Record::Player(PlayerRecord {
                    player_uuid: deterministic_bytes(17, 16),
                    dimension: 1,
                    x_fixed: 37 * 4096,
                    y_fixed: 64 * 4096,
                    z_fixed: 2 * 4096,
                    yaw_millidegrees: 90_000,
                    pitch_millidegrees: 0,
                    game_mode: 1,
                })),
                extensions: vec![ExtensionValue {
                    local_id: 1,
                    payload: deterministic_bytes(19, PLAYER_EXTENSION_BYTES),
                }],
            })),
        },
    )
}

fn entity_update(seed: u8) -> RecordWrite {
    general(
        RecordKey::general(37, 2, 18),
        general_record::Record::Entity(EntityRecord {
            entity_uuid: deterministic_bytes(seed, 16),
            entity_type: "minecraft:pig".to_owned(),
            dimension: 1,
            x: 37.5,
            y: 64.0,
            z: 2.5,
            yaw: 90.0,
            pitch: 0.0,
            ..EntityRecord::default()
        }),
    )
}

fn world_update(seed: i64) -> RecordWrite {
    general(
        RecordKey::general(i32::MIN, i32::MIN, u32::MAX),
        general_record::Record::WorldProperties(WorldProperties {
            game_data_version: 46_002,
            seed,
            spawn_dimension: 1,
            spawn_x: 0,
            spawn_y: 64,
            spawn_z: 0,
            day_time: 6_000,
            default_game_mode: 1,
        }),
    )
}

fn changed_chunk() -> RecordWrite {
    let mut record = chunk(37, 201);
    let Some(storage_record::Record::Chunk(chunk)) = record.record.as_mut() else {
        unreachable!("chunk fixture always has a chunk body")
    };
    chunk.block_entity_nbt = vec![deterministic_bytes(31, BLOCK_ENTITY_BYTES)];
    chunk.block_scheduled_ticks = vec![ScheduledTick {
        x: 37 * 16 + 5,
        y: 64,
        z: 2 * 16 + 7,
        kind: ScheduledTickKind::Repeater as i32,
        trigger_tick: 18_002,
        priority: ScheduledTickPriority::High as i32,
        insertion_order: 91,
    }];
    RecordWrite::new(chunk_key(37), record)
}

fn dirty_save() -> Vec<RecordWrite> {
    vec![
        changed_chunk(),
        player_update(),
        entity_update(29),
        world_update(-9_876_543_210),
    ]
}

fn seed_native(directory: &TempDir) -> NativeStore {
    let mut store = NativeStore::open(directory.path()).unwrap();
    store
        .register_extensions([ExtensionRegistration::new("benchmark", "player", 1)])
        .unwrap();
    let mut records: Vec<_> = (0..COLUMNS)
        .map(|column| RecordWrite::new(chunk_key(column), chunk(column, column as u8)))
        .collect();
    records.extend([entity_update(28), world_update(-9_876_543_209)]);
    store.write_transaction(records).unwrap();
    store
}

fn encode(write: &RecordWrite) -> Vec<u8> {
    validate_record(&write.record).unwrap();
    write.record.encode_to_vec()
}

fn seed_redb(directory: &TempDir) -> redb::Database {
    let database = redb::Database::create(directory.path().join("world.redb")).unwrap();
    let write = database.begin_write().unwrap();
    {
        let mut table = write.open_table(RECORDS).unwrap();
        for column in 0..COLUMNS {
            let key = chunk_key(column).to_bytes();
            let payload = encode(&RecordWrite::new(chunk_key(column), chunk(column, column as u8)));
            table.insert(key.as_slice(), payload.as_slice()).unwrap();
        }
        for record in [entity_update(28), world_update(-9_876_543_209)] {
            let key = record.key.to_bytes();
            let payload = encode(&record);
            table.insert(key.as_slice(), payload.as_slice()).unwrap();
        }
    }
    write.commit().unwrap();
    database
}

fn write_redb(database: &redb::Database, writes: &[RecordWrite]) {
    let write = database.begin_write().unwrap();
    {
        let mut table = write.open_table(RECORDS).unwrap();
        for record in writes {
            let key = record.key.to_bytes();
            let payload = encode(record);
            table.insert(key.as_slice(), payload.as_slice()).unwrap();
        }
    }
    write.commit().unwrap();
}

fn report_space_sample(writes: &[RecordWrite]) {
    let native_directory = TempDir::new().unwrap();
    let mut native = seed_native(&native_directory);
    let native_path = native.segment_path().to_owned();
    let native_seeded = fs::metadata(&native_path).unwrap().len();
    native.write_transaction(writes.to_vec()).unwrap();
    let native_after = fs::metadata(&native_path).unwrap().len();
    drop(native);
    let mut reopened_native = NativeStore::open(native_directory.path()).unwrap();
    assert_eq!(reopened_native.get(writes[0].key).unwrap(), Some(writes[0].record.clone()));
    let native_compaction_started = Instant::now();
    let native_compaction = reopened_native.compact().unwrap();
    let native_compaction_micros = native_compaction_started.elapsed().as_micros();

    let redb_directory = TempDir::new().unwrap();
    let redb_path = redb_directory.path().join("world.redb");
    let database = seed_redb(&redb_directory);
    let redb_seeded = fs::metadata(&redb_path).unwrap().len();
    write_redb(&database, writes);
    let redb_after = fs::metadata(&redb_path).unwrap().len();
    drop(database);
    let mut reopened_redb = redb::Database::open(&redb_path).unwrap();
    {
        let read = reopened_redb.begin_read().unwrap();
        let table = read.open_table(RECORDS).unwrap();
        let key = writes[0].key.to_bytes();
        assert!(table.get(key.as_slice()).unwrap().is_some());
    }
    let redb_compaction_started = Instant::now();
    let redb_compacted = reopened_redb.compact().unwrap();
    let redb_compaction_micros = redb_compaction_started.elapsed().as_micros();
    let redb_compacted_bytes = fs::metadata(&redb_path).unwrap().len();

    eprintln!(
        "incremental-space-sample engine=native seeded_bytes={native_seeded} after_bytes={native_after} growth_bytes={} compacted_bytes={} reclaimed_bytes={} compaction_micros={native_compaction_micros}",
        native_after - native_seeded,
        native_compaction.after_bytes,
        native_compaction.before_bytes - native_compaction.after_bytes,
    );
    eprintln!(
        "incremental-space-sample engine=redb seeded_bytes={redb_seeded} after_bytes={redb_after} growth_bytes={} compacted={redb_compacted} compacted_bytes={redb_compacted_bytes} reclaimed_bytes={} compaction_micros={redb_compaction_micros}",
        redb_after - redb_seeded,
        redb_after.saturating_sub(redb_compacted_bytes),
    );
}

fn incremental_world_save(c: &mut Criterion) {
    let writes = dirty_save();
    let logical_bytes = writes.iter().map(encode).map(|bytes| bytes.len() as u64).sum();
    report_space_sample(&writes);

    let mut group = c.benchmark_group("incremental_world_save");
    group.throughput(Throughput::Bytes(logical_bytes));
    group.bench_function("native_append_index", |bench| {
        bench.iter_batched(
            || {
                let directory = TempDir::new().unwrap();
                let store = seed_native(&directory);
                (directory, store)
            },
            |(_directory, mut store)| store.write_transaction(writes.clone()).unwrap(),
            BatchSize::SmallInput,
        );
    });
    group.bench_function("redb", |bench| {
        bench.iter_batched(
            || {
                let directory = TempDir::new().unwrap();
                let database = seed_redb(&directory);
                (directory, database)
            },
            |(_directory, database)| write_redb(&database, &writes),
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, incremental_world_save);
criterion_main!(benches);
