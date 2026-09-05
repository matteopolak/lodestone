# World-storage engine decision prototype

## What it is

`lodestone-storage-prototype` is an isolated, non-production comparison between a purpose-built append/index segment and redb. It supplies a small, reproducible workload for the storage-engine choice without selecting a world format or connecting to server save/load code.

## How it works

The harness models independently dirty records rather than serializing a whole column for every save. `RecordKey` identifies chunk, entity, block-entity, player, and global records with a column coordinate, local numeric ID, and explicit kind. The benchmark first seeds 128 chunk records of 24 KiB, then times one 24 KiB dirty-chunk replacement plus one 192-byte block-entity replacement. Setup is outside Criterion's timed closure, so the result answers the incremental-write question rather than initial world creation.

The custom `AppendIndexStore` appends one self-contained record per replacement and reconstructs an in-memory latest-record index when opened. It calls `sync_data` before publishing a record in that index. Opening truncates an incomplete final header or payload back to the last complete record, which is the only crash-tail recovery promised by this slice. A malformed complete record, unsupported version, reserved-field value, invalid kind, or checksum mismatch refuses to open the segment; silently skipping completed corruption would hide a damaged save.

The segment record has this externally specified, version-1 little-endian layout. No production schema is implied by these test records.

| byte range | field | meaning |
|---|---|---|
| `0..4` | magic | ASCII `LSRP` |
| `4..6` | version | `u16`, currently `1` |
| `6` | kind | explicit record-kind discriminant |
| `7..11` | x | signed `i32` column coordinate |
| `11..15` | z | signed `i32` column coordinate |
| `15..19` | id | `u32` local record identifier |
| `19..27` | generation | reserved `u64`, must be zero in version 1 |
| `27..31` | payload length | `u32` |
| `31..35` | checksum | IEEE CRC-32 of exactly the payload bytes |
| `35..` | payload | opaque benchmark fixture bytes |

`compact` is a size-reclamation control only: it writes the newest value for each key to a sibling file, syncs that file, and renames it over the segment. This prototype deliberately does not define directory-swap recovery, snapshots, compaction scheduling, protobuf records, extension IDs, migration, or Anvil conversion. Those must be specified by the eventual selected engine rather than inferred from this experiment.

`RedbStore` uses the exact same fixed `RecordKey` byte representation and replaces the same values through one committed redb write transaction per logical record. The comparison is therefore between write/index/compaction behaviour, not between unrelated key shapes or payload encodings.

## How to change it

Keep the workload representative and bounded. If a changed fixture is intended to stand for a new world-state category, add a distinct `RecordKind` value and explain its size, update rate, and keying in this document. Do not turn a benchmark payload into a save schema; generated protobuf types and external byte fixtures belong to the later format-design phase.

Run the comparison explicitly on an otherwise idle machine; it is not an ordinary CI command and no duration threshold is committed:

```bash
cargo bench -p lodestone-storage-prototype --bench incremental -- --sample-size 20
```

Record both durations, on-disk growth after the replacement loop, compaction cost and reclaimed bytes, recovery outcome for an interrupted append, and the environment. A decision needs repeated same-machine comparisons and recovery/corruption results, not one timing. Before promoting either store, add tests for concurrent readers/snapshots, interrupted compaction, and the actual integrated-server dirty-record consumer.

## Configuration

There are no runtime flags. The benchmark constants are `COLUMNS = 128`, `CHUNK_BYTES = 24 KiB`, and a 192-byte block-entity mutation in `benches/incremental.rs`. They are deliberately visible and fixed so a result reports a known workload.

## Dependencies

- `redb` is the comparison implementation only; no product crate depends on it.
- `criterion` and `tempfile` are development-only dependencies used by the explicit benchmark and fast isolated tests.
- The standard library provides filesystem synchronization and the local CRC-32 implementation.
