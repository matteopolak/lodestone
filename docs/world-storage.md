# World-storage engine

## What it is

The native (`LodestoneNative`) world backend: a purpose-built append-only segment store (`lodestone-storage`) holding validated Protobuf envelopes (`lodestone-storage-schema`), reached by the integrated server through `lodestone_server::world_storage::WorldStorage`. A host selects `Anvil` or `LodestoneNative` explicitly; Anvil stays the compatibility writer and the live terrain/entity loader.

## How it works

### Crates

- `lodestone-storage-schema`: version-1 Protobuf vocabulary (generated types, descriptor, byte fixtures; no I/O).
- `lodestone-storage`: `NativeStore` commits groups of `RecordWrite` as atomic transactions and keeps a latest-record index in memory.
- `lodestone-storage-prototype`: comparison harness (redb kept there as a dev-only comparator).
- `WorldStorage` (server): typed adapter. Chunks go in as `NativeDirtyChunkRecord` and out as `NativeChunkRecord`, each carrying blocks, biomes, heightmap, block entities, canonical light and pending block/fluid ticks together so no partial loader can drop a field.

### Engine decision

Three ten-sample runs on idle arm64 macOS (not CI thresholds), 128 seeded chunks then one transaction replacing a chunk, a player, an entity and world properties:

| engine | incremental median | throughput | compacted bytes | compaction |
|---|---:|---:|---:|---:|
| purpose-built segment | 8.08-8.77 ms | 2.80-3.04 MiB/s | 3,153,146 | 169-178 ms |
| redb 3.1.3 | 19.01-20.03 ms | 1.23-1.29 MiB/s | 4,263,936 | 50-52 ms |

The segment store wins on incremental latency and steady-state size; redb's faster compaction is an offline path. Reads are coherent owned snapshots under the `WorldStorage` mutex, not long-lived transactions; if snapshot serialization profiles badly, add a read-only immutable-index handle and re-run the comparison rather than switching engines on redb's concurrency alone. The ignored gate `integrated::tests::measure_generated_mutation_heavy_native_save` saves a 3x3 area (576 edits, nine block entities, nine ticks) at 253,433 bytes in 88,899 microseconds (one debug sample).

### On-disk format

Segment files are a sequence of transactions: an `LSTB` start header (record count, body length, CRC-32), key/length/CRC frames of Protobuf envelopes, then an `LSTC` commit header. The writer syncs start and body, then the commit marker, then publishes all index entries together. On open, an incomplete final transaction is truncated to the previous boundary; anything else wrong in a complete transaction (marker mismatch, CRC, bad Protobuf, schema violation, duplicate key, trailing bytes) refuses to open. Reads re-check the payload CRC.

`RecordKey` is 13 bytes: signed column x/z, `u32` local ID, chunk/general discriminant (must match the envelope's `oneof`). Segment format is version 2: a chunk key's local ID is its `BuiltinDimension` (1 to 3), so one column in two dimensions is two records. Version-1 transactions read as Overworld through `migrate_legacy_key`; legacy bytes are not rewritten on open, while new transactions and `compact` write version 2 (fixture `crates/lodestone-storage/tests/fixtures/chunk-segment-v1.hex`).

### Chunk record

`StorageRecord` carries a format version and a `ChunkRecord` or typed `GeneralRecord`. A chunk holds: column coordinates and target game-data version; per-section palettes (numeric state IDs, width 1-15) with packed index streams; 4x4x4 built-in biome grids plus a 4x4 surface-biome grid; an optional 16x16 motion-blocking heightmap; complete named-NBT roots for block entities (an opaque entity survives verbatim); an optional light stream per layer (`Missing`, four-bit `uniform`, or exactly 2048 nibbles, including the sections below and above the range, like `lodestone_world::ColumnLight`); and structure starts and references (placement data, block lists, loot and refinement reload as absent).

Chunks with structure data are `CHUNK_FORMAT_VERSION_V2` (general records stay at 1). A v1 record migrates to no structures; a v1 record carrying structure fields is rejected by `validate_record` and the server decoder (fixture `crates/lodestone-storage-schema/tests/fixtures/chunk-v2-structures.hex`). `validate_record_with_extensions` gates every save: known version, non-empty palette, no legacy raw light, ordered typed light, aligned biomes, a 256-entry `u16`-range heightmap or none, a registered extension ID.

### WorldStorage operations

| operation | notes |
|---|---|
| `write_dirty_chunk` / `load_chunk` | terrain only; `load_chunk` refuses a record with light |
| `..._with_light` | takes a `ColumnLight` of `section_count + 2`; an older terrain-only record returns `MissingStoredLight`, never "dark" |
| `..._with_scheduled_ticks` | separate block/fluid tick lists with absolute trigger tick, priority and insertion order; extension actions keep their name in `extension_kind` |
| `write_dirty_chunks` | encodes every dirty column before committing, so one unrepresentable column fails that dimension's whole transaction |
| `native_chunk_coordinates` / `native_chunk_records` / `native_general_records` | deterministic sorted snapshots for export under one lock; any bad record rejects the selection |
| `write_dirty_world_properties` / `load_world_properties` | the one envelope at `WORLD_PROPERTIES_KEY` |
| `compact_native`, `register_native_extensions` | native backend only |

Loading needs the caller's dimension `min_y` (16-aligned) and `height`; the record stores no dimension definition. Malformed data is an error, never a dropped entity or light layer. Native saves are additive to Anvil, whose writer drains the same dirty set right after; a failed native transaction loses nothing in the running game but those columns are not retried natively. Writing refuses columns with shaped-only generation, unacknowledged generation-spawn candidates, or biomes outside the built-in census.

### Compaction and extensions

`NativeStore::compact` writes the latest value per key to `world.ls.compacting`, renames `world.ls` to `world.ls.previous`, publishes the replacement as `world.ls`, syncing the directory each step. Reopen resolves any interruption (removes an unpublished replacement, restores `.previous` if the active name vanished, keeps a published replacement); zero or all three names present is corruption and refuses to open. `compact_native` returns retained count and before/after bytes, serializing in-process handles only.

An `ExtensionTable` maps non-zero local IDs to (namespace, name, schema version); records carry only the ID and opaque bytes. The table lives in the atomically replaced `extensions.ls` sidecar, restored before the segment is validated. `register_extensions` sorts and dedups, assigns the lowest unused IDs, keeps them across reopen and refuses a second version of a name. General-record extensions are refused by the player and entity readers.

### Players

`NativePlayerRecord` is a locator: 16-byte UUID, dimension, three signed fixed-point (thousandths of a block) coordinates, millidegree yaw/pitch. `NativePlayerData` (`write_dirty_player_data`/`load_player_data`) adds game mode and optional groups: runtime (health, air, three experience values) and a sparse inventory (selected slot, occupied slots each with item key, count, `custom_data` and other components as one network-NBT compound; [item save format](./item-save-format.md)). A component with no saved form fails the write. Motion, fire, fall, ground state and opaque root fields are Anvil-import losses. Player and entity general keys reserve one local-ID bit as a type domain; the full UUID is compared on every read and write.

With `LodestoneNative`: login reads a valid locator (missing falls back to world spawn/Anvil); a Nether or End locator resolves that sibling source before the teleport and the dimension-change frame precedes chunks. Native game mode, vitals, XP and inventory join state only when complete Anvil player data is absent; otherwise Anvil wins. Periodic, disconnect and cancellation-safe snapshots write live values. A corrupt or extension-bearing locator, unsupported component, missing sibling or unencodable dimension is logged and blocks native overwrite for that session. Singleplayer shutdown publishes an in-memory native-data slot, joins the connection task, then commits the last record.

### Entities

`NativeEntityRecord`: UUID, resource-key type, dimension, exact feet position, yaw/pitch, motion, and a typed durable-state arm (living health, or dropped item key/count/age/pickup delay). A missing arm keeps old pose-only records readable but never a live entity. The UUID-derived key ignores the resident chunk, so a moved entity replaces its record.

`replace_live_entities` sorts the snapshot by UUID, writes changed bodies and commits them with one per-dimension `EntityRoster`, the liveness set (an omitted entity's body is never restored). An absent roster allows the Anvil fallback; a present empty roster is an authoritative empty population. The roster also carries the dimension's active raids (`RaidRecord`, `NativeRoster`); `replace_live_entities` writes none and exists for tests. `replace_live_entity_rosters` commits several dimensions in one transaction and refuses a UUID listed twice, keeping a dimension-changing entity consistent across a crash.

The seed task reads all three rosters after `MobHandle::replace_world`: Overworld into the primary sim, non-empty Nether/End rosters into `WorldStateHandle::ensure_dimension_runtime`, restoring via `spawn_species` and `spawn_item`. Saves cover every dimension with a live runtime; others keep their stored roster. Species AI memory, projectile ownership and opaque Anvil fields are outside the vocabulary.

Roster replacement requires adoption: the seed task publishes a monotonic `AdoptedEntityRosters` set after a successful native load or Anvil fallback (empty counts). A Nether/End roster that fails to load is left out so saves never overwrite it; an absent sibling roster is adopted as empty; a corrupt native roster is a logged failure and does not trigger Anvil fallback. Autosave, `save_native_now` and shutdown may write terrain before adoption but never the roster; keep this independent of startup tick holds. Anvil fallback uses the same ownership rule: `EntityStorage::save_owned` takes the owned UUID set (including just-despawned ids), removes owned UUIDs absent from the snapshot and preserves all other records byte-for-byte; `DimensionEntityStores` holds one `entities/` set per dimension under `dimensions/minecraft/<dimension>/`.

### Save lifecycle

`IntegratedServer::save_native_now` (and autosave/shutdown) snapshots pending columns, live block entities and both scheduler queues before the Anvil writer drains them, for the primary world and each sibling (`DimensionSaveHandles`). Dimensions commit separately: a failure is logged, the first error returned, the others proceed. The seed task acknowledges its coordinate set only after reseeding and entity restoration, so an early shutdown cannot publish terrain while losing the creature handoff. Missing heightmaps or light are errors. `reopen_native_chunk` takes a dimension and stages ticks into that scheduler. Tests: `integrated::tests::native_chunks_keep_each_dimension_at_the_same_column`, `native_chunks_keep_structure_starts_in_every_dimension`.

### Export to Anvil

- `anvil_entity_export::export_entities`: one dimension to staged sidecar regions, published with one rename of `entities`; refuses existing sidecars.
- `anvil_player_export::export_all_players`: staged player files with new-player defaults for unrepresented fields; one rename of `players/data`; refuses an existing target.
- `anvil_metadata_export::export_metadata`: replaces seed, spawn, default game mode, name and last-played in a same-version template, writing `world_gen_settings.dat` before the final `level.dat`; refuses existing metadata; a nonzero `day_time` fails closed.

`anvil-convert import-entities` is the only writer of resident-entity locators today; the metadata importer calls `write_dirty_world_properties` only after its loss preflight and authorization.

## How to change it

- New stored chunk field: extend the schema, bump `CHUNK_FORMAT_VERSION` so older readers refuse instead of losing data, add a migration and a hex fixture, extend `validate_record_with_extensions`.
- Key layout change: bump `FORMAT_VERSION` and add its migration step beside `migrate_legacy_key`.
- New typed general record: give it a key domain and cover the exporter, reader and adoption rules.

## Configuration

Backend selection is explicit per host (`Anvil` or `LodestoneNative`). Files: `world.ls`, `extensions.ls`, transient `world.ls.compacting` and `world.ls.previous`.

## Dependencies

`prost`, CRC-32, `lodestone_world` (`ColumnLight`), the item save format, the Anvil crates for import/export; `redb` for the prototype comparison only.
