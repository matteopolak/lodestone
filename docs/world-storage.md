# World-storage engine decision prototype

## What it is

`lodestone-storage-prototype` is an isolated, non-production comparison between a purpose-built append/index segment and redb. It supplies a small, reproducible workload for the storage-engine choice without selecting a production engine. `lodestone-storage-schema` is the separate version-1 Protobuf vocabulary for the native backend: it commits generated Rust types, a descriptor, and byte fixtures, but performs no I/O itself. `lodestone-storage` is the native append/index layer that persists validated envelopes as atomically committed replacement batches and contains the direct native-versus-redb workload. `lodestone_server::world_storage` is the integrated-server seam: a host explicitly selects `Anvil` or `LodestoneNative`, then an attached server may save and reopen a bounded, terrain-only `ChunkColumn` through `IntegratedServer::write_dirty_native_chunk` and `IntegratedServer::load_native_chunk`.

## How it works

The native comparison harness models independently dirty records rather than serializing a whole column for every save. It first seeds 128 typed chunk envelopes whose deterministic state-index streams are 24 KiB. It then writes one changed chunk and a player envelope carrying a 192-byte extension payload together as one logical save transaction. Both engines receive the same fixed 13-byte keys, validated Protobuf envelopes, replacement order, and transaction boundary. Setup is outside Criterion's timed closure, so the reported logical-byte throughput answers the incremental-write question rather than initial world creation. Before timing, the harness reopens both stores and prints each engine's seeded size, post-save size, and growth; those values are samples to record with the host environment, not a conclusion about the eventual engine.

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

`compact` is a size-reclamation control only: it writes the newest value for each key to a sibling file, syncs that file, and renames it over the segment. This prototype deliberately does not define directory-swap recovery, snapshots, compaction scheduling, migration, or Anvil conversion. Those must be specified by the eventual selected engine rather than inferred from this experiment.

The schema's `StorageRecord` envelope carries an explicit format version and selects either a `ChunkRecord` or a typed `GeneralRecord`. A chunk stores signed column coordinates, the target game-data version, per-section numeric state palettes, palette bit widths, and packed state-index/light byte streams; core chunk state never repeats resource-location strings. General state has distinct typed forms for world properties, players, and entities. An engine may save an envelope only after `validate_record` accepts the representation-level invariants that Protobuf cannot state, such as a known format version and a non-empty palette with a width from 1 through 15.

`lodestone_storage::NativeStore` groups one or more `RecordWrite` values into a transaction. Each write carries a fixed 13-byte `RecordKey` — signed column coordinates, a `u32` local ID, and an explicit chunk/general discriminant — plus one validated `StorageRecord` envelope. Thus a hot record does not carry names to identify itself: a future producer allocates compact local IDs for its general records, while a whole-column chunk uses local ID zero. The record-kind discriminator must agree with the envelope's `oneof`, so a byte stream cannot accidentally index a chunk payload as general state.

The on-disk stream is a sequence of version-1 transactions. A transaction has a `LSTB` start header (record count, encoded-body length, and CRC-32), fixed key/length/CRC frames whose payloads are Protobuf envelopes, and a matching `LSTC` commit header. The writer syncs the start and body before appending and syncing the commit marker, then publishes all new index entries together. On open, only a transaction with a complete matching commit marker contributes to the latest-record index. An incomplete final start, body, or commit marker is truncated back to the preceding transaction boundary. A complete transaction with a mismatched marker, checksum failure, invalid Protobuf, invalid schema invariant, duplicate key, or trailing bytes refuses to open instead of silently losing durable state. Reads check the indexed payload checksum again before decoding.

Extensions are registered once in an `ExtensionTable`. A registration has a non-zero local numeric ID, namespace, name, and schema version; a record carries just the local ID and opaque extension bytes. `validate_record_with_extensions` proves every record reference resolves through its table. This keeps the string-bearing registry outside hot core records while still making a save's extension contract explicit.

The first production chunk adapter is intentionally narrower than the schema's eventual scope. `WorldStorage::write_dirty_chunk` converts a server `ChunkColumn` into one independently replaceable `ChunkRecord`: it emits a per-section local palette of built-in numeric block-state IDs, a one-to-fifteen-bit little-endian packed local-index stream, and no light bytes. `WorldStorage::load_chunk` opens the same `NativeStore` index, validates the requested key, current game-data version, section sequence, packed lengths and palette indices, then reconstructs a real `ChunkColumn`; the integrated-server methods are thin consumers of those two operations. The reopen test reads blocks through `ChunkColumn::block_state` after constructing a second server, so this is not merely a segment-level round trip.

This is **not** a complete native world format yet. Before writing, the adapter refuses any column with non-default three-dimensional biome cells, block entities, structures, a retained motion-blocking heightmap, shaped-only generation, or undrained generation-spawn candidates. On read it also refuses extension values and stored light bytes, rather than decoding a partial column and discarding information on its next save. The caller supplies `min_y` and `height` when loading because version 1 stores section coordinates but no dimension definition; minimum Y must be aligned to 16. The existing Anvil `RegionChunkSource` remains the live terrain/entity/metadata loader and is deliberately not redirected through this partial adapter.

`crates/lodestone-storage-schema/tests/fixtures/` contains externally specified version-1 hexadecimal byte fixtures for a chunk envelope, a world-properties envelope, and an extension table. The tests decode each fixture into generated types, assert every semantic field, then re-encode it byte-for-byte. They are intentionally not generated by the Rust encoder, so a symmetric encoder/decoder mistake cannot approve a wire-layout change.

`RedbStore` uses the exact same fixed `RecordKey` byte representation and replaces the same values through one committed redb write transaction per logical save. The direct `lodestone-storage` benchmark repeats that comparison against `NativeStore`, including the production envelope validation and encoding path, so it does not compare unrelated key shapes, opaque payloads, or atomicity boundaries.

## How to change it

Keep the workload representative and bounded. If a changed fixture is intended to stand for a new world-state category, add a distinct `RecordKind` value and explain its size, update rate, and keying in this document. Do not turn a benchmark payload into a save schema.

To change the native vocabulary, edit `crates/lodestone-storage-schema/proto/lodestone/storage/v1/storage.proto`. Preserve field numbers and use new optional fields or `oneof` members for compatible additions; incompatible semantics require a new envelope version. Regenerate both committed artifacts with:

```bash
LODESTONE_STORAGE_SCHEMA_REGENERATE=1 cargo check -p lodestone-storage-schema
```

The build script always recompiles the schema with a vendored Protobuf compiler and compares the result with `src/generated/lodestone.storage.v1.rs` and `storage.fds.bin`. Normal `cargo check` and `cargo test` therefore fail on an unregenerated schema or generator drift. Update the independent fixture only after explicitly specifying its new bytes and field meanings; never rewrite it from `encode_to_vec`.

`NativeStore::write_transaction` is the boundary a dirty-record producer calls through `WorldStorage::write_dirty`. Keep writes small and group only state that must become visible together; a batch is durable only once its commit marker is synced. `WorldStorageBackend::Anvil` deliberately refuses typed-record writes instead of dropping them: the established `RegionChunkSource`/`WorldSaveHandle` path remains first-class and keeps its dirty-column behaviour. `IntegratedServer::open_persistent_with_mobs_and_storage` attaches a selected record backend while retaining that Anvil terrain/entity/metadata path. Use `write_dirty_native_chunk` only after handling its explicit loss result, and use `load_native_chunk` only as the bounded terrain record consumer described above; neither method swaps the live source away from Anvil. Do not change the transaction-header constants or key byte representation in place: a format change requires a new storage version and an explicit reader/migration policy. The crate currently has one mutable file handle, so it is a single-writer/single-handle layer rather than the concurrent snapshot interface requested for the final engine. Compaction, extension-table lifecycle, schema negotiation, migration, Anvil loss preflight, native support for biomes/lights/entities/structures and automatic source selection are follow-up work. It intentionally provides no rollback guarantee.

Run the comparison explicitly on an otherwise idle machine; it is not an ordinary CI command and no duration threshold is committed:

```bash
cargo bench -p lodestone-storage --bench incremental -- --sample-size 20
```

Record both logical-byte throughputs, the printed seeded/post-save/growth byte counts, recovery outcome for an interrupted append, and the environment. The existing prototype benchmark remains useful for early append/index experiments, but it is not the engine-selection run. A decision needs repeated same-machine comparisons, compaction cost and reclaimed bytes, and recovery/corruption results, not one timing. Before promoting either store, add tests for concurrent readers/snapshots, interrupted compaction, and the actual integrated-server dirty-record consumer.

## Configuration

There are no runtime flags. A host constructs `WorldStorage::open(WorldStorageBackend::Anvil)` to select the existing compatibility backend, or `WorldStorage::open(WorldStorageBackend::LodestoneNative { directory })` before calling `IntegratedServer::open_persistent_with_mobs_and_storage`. `NativeStore::open(directory)` uses `world.ls` within that directory. Native chunk loading also requires the active dimension's `min_y` and `height`; the current adapter accepts a positive height and a 16-aligned minimum Y only. The benchmark constants are `COLUMNS = 128`, `CHUNK_BYTES = 24 KiB`, and `PLAYER_EXTENSION_BYTES = 192` in `crates/lodestone-storage/benches/incremental.rs`. They are deliberately visible and fixed so a result reports a known workload.

## Dependencies

- `redb` is a development-only comparison dependency; no product path depends on it.
- `lodestone-storage` depends on `lodestone-storage-schema` and `prost` for the validated envelope boundary. It uses only the standard library for segment I/O and CRC-32.
- `criterion` and `tempfile` are development-only dependencies used by the explicit benchmark and fast isolated tests.
- The standard library provides filesystem synchronization and the local CRC-32 implementation.
- `prost` is the generated-type runtime, while `prost-build` and `protoc-bin-vendored` produce the checked Rust source and descriptor at build time. The vendored compiler avoids making the drift gate depend on a host-installed Protobuf executable.
