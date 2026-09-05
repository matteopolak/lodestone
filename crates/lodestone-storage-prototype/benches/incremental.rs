//! Explicit, local comparison of incremental dirty-record replacement.
//!
//! This is intentionally not a CI gate. Run it on an otherwise idle machine:
//! `cargo bench -p lodestone-storage-prototype --bench incremental -- --sample-size 20`.

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_storage_prototype::{AppendIndexStore, RecordKey, RecordKind, RedbStore};
use tempfile::TempDir;

const COLUMNS: i32 = 128;
const CHUNK_BYTES: usize = 24 * 1024;

fn chunk_key(column: i32) -> RecordKey {
    RecordKey {
        x: column,
        z: column / 16,
        id: 0,
        kind: RecordKind::Chunk,
    }
}

fn payload(seed: u8) -> Vec<u8> {
    (0..CHUNK_BYTES)
        .map(|offset| seed.wrapping_add((offset % 251) as u8))
        .collect()
}

fn seed_append(directory: &TempDir) -> AppendIndexStore {
    let mut store = AppendIndexStore::open(directory.path()).unwrap();
    for column in 0..COLUMNS {
        store.put(chunk_key(column), &payload(column as u8)).unwrap();
    }
    store
}

fn seed_redb(directory: &TempDir) -> RedbStore {
    let store = RedbStore::open(directory.path().join("comparison.redb")).unwrap();
    for column in 0..COLUMNS {
        store.put(chunk_key(column), &payload(column as u8)).unwrap();
    }
    store
}

fn incremental_replacement(c: &mut Criterion) {
    let dirty_chunk = chunk_key(37);
    let dirty_block_entity = RecordKey {
        x: 37,
        z: 2,
        id: 17,
        kind: RecordKind::BlockEntity,
    };
    let chunk_payload = payload(201);
    let block_entity_payload = vec![17; 192];

    c.bench_function("append_index/incremental_chunk_and_block_entity", |bench| {
        bench.iter_batched(
            || {
                let directory = TempDir::new().unwrap();
                let store = seed_append(&directory);
                (directory, store)
            },
            |(_directory, mut store)| {
                store.put(dirty_chunk, &chunk_payload).unwrap();
                store
                    .put(dirty_block_entity, &block_entity_payload)
                    .unwrap();
            },
            criterion::BatchSize::SmallInput,
        );
    });
    c.bench_function("redb/incremental_chunk_and_block_entity", |bench| {
        bench.iter_batched(
            || {
                let directory = TempDir::new().unwrap();
                let store = seed_redb(&directory);
                (directory, store)
            },
            |(_directory, store)| {
                store.put(dirty_chunk, &chunk_payload).unwrap();
                store
                    .put(dirty_block_entity, &block_entity_payload)
                    .unwrap();
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, incremental_replacement);
criterion_main!(benches);
