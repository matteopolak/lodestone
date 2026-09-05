//! A like-for-like incremental world-save comparison for the native store and redb.
//!
//! Run locally on an otherwise idle machine:
//! `cargo bench -p lodestone-storage --bench incremental -- --sample-size 20`.
//! Criterion reports logical write throughput. Before timing, this harness also
//! prints each engine's seeded size, post-save size, and growth for the exact
//! workload; it makes no engine-selection decision from one run.

use std::fs;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use lodestone_storage::{NativeStore, RecordKey, RecordKind, RecordWrite};
use lodestone_storage_schema::{
    ChunkRecord, ChunkSection, ExtensionValue, GeneralRecord, PlayerRecord, StorageRecord,
    generated::{general_record, storage_record},
    validate_record,
};
use prost::Message;
use redb::{ReadableDatabase, TableDefinition};
use tempfile::TempDir;

const COLUMNS: i32 = 128;
const CHUNK_BYTES: usize = 24 * 1024;
const PLAYER_EXTENSION_BYTES: usize = 192;
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
            extensions: vec![],
        })),
    }
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
                })),
                extensions: vec![ExtensionValue {
                    local_id: 1,
                    payload: deterministic_bytes(19, PLAYER_EXTENSION_BYTES),
                }],
            })),
        },
    )
}

fn dirty_save() -> Vec<RecordWrite> {
    vec![
        RecordWrite::new(chunk_key(37), chunk(37, 201)),
        player_update(),
    ]
}

fn seed_native(directory: &TempDir) -> NativeStore {
    let mut store = NativeStore::open(directory.path()).unwrap();
    let records = (0..COLUMNS).map(|column| RecordWrite::new(chunk_key(column), chunk(column, column as u8)));
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

    let redb_directory = TempDir::new().unwrap();
    let redb_path = redb_directory.path().join("world.redb");
    let database = seed_redb(&redb_directory);
    let redb_seeded = fs::metadata(&redb_path).unwrap().len();
    write_redb(&database, writes);
    let redb_after = fs::metadata(&redb_path).unwrap().len();
    drop(database);
    let reopened_redb = redb::Database::open(&redb_path).unwrap();
    let read = reopened_redb.begin_read().unwrap();
    let table = read.open_table(RECORDS).unwrap();
    let key = writes[0].key.to_bytes();
    assert!(table.get(key.as_slice()).unwrap().is_some());

    eprintln!(
        "incremental-space-sample engine=native seeded_bytes={native_seeded} after_bytes={native_after} growth_bytes={}",
        native_after - native_seeded,
    );
    eprintln!(
        "incremental-space-sample engine=redb seeded_bytes={redb_seeded} after_bytes={redb_after} growth_bytes={}",
        redb_after - redb_seeded,
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
