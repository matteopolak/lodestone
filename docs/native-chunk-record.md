# Native chunk records

## What it is

The native server storage seam persists one complete typed chunk replacement through `NativeDirtyChunkRecord` and reopens it as `NativeChunkRecord`. The boundary keeps block, biome, heightmap, resident block-entity, canonical light, and pending block/fluid-tick state together so a partial save or load cannot silently erase a field.

## How it works

Construct `NativeDirtyChunkRecord::new` with the chunk coordinates, a `ChunkColumn`, its `ColumnLight`, and the live `ScheduledTickHandle`. `WorldStorage::write_dirty_chunk` snapshots both pending queues without draining them, validates the complete column and light shape, and commits one replacement envelope to the selected native store. The Anvil selection returns `AnvilDoesNotAcceptTypedRecords` before conversion and is not redirected through this path.

`WorldStorage::load_chunk` validates the requested dimension extent, key, current game-data version, contiguous block/light sections, palettes, biome grids, heightmap, entity NBT roots, and both tick lists before returning an owned `NativeChunkRecord`. A record with no persisted light returns `MissingStoredLight`; unsupported or malformed data is never converted into a partial result. `NativeChunkRecord::stage_scheduled_ticks` hands both decoded queues to the live scheduler at the next scheduler access while retaining each stored insertion order.

The persistent integrated-server constructor wires this boundary into the production lifecycle when `LodestoneNative` is selected. Autosave and clean shutdown snapshot the same dirty set before the established Anvil writer drains it, compute canonical light through the live protocol, and commit complete records in one native transaction. `IntegratedServer::reopen_native_chunk` returns the owned record and stages both tick queues into the live scheduler. Anvil remains first-class and continues to write its compatibility world unchanged.

## How to change it

Change `NativeDirtyChunkRecord` and `NativeChunkRecord` together when adding a chunk field. The write value must require every payload that the reader returns. Extend `encode_chunk_inner` and `decode_chunk` only after adding a typed schema field and validation for its shape; a decoder must reject an unsupported field rather than drop it. New scheduled actions require a schema enum value, lossless kind conversion, and a restart test whose ordering distinguishes trigger tick, priority, and insertion order.

Do not add a terrain-only or light-only native chunk loader. If a production column has no derived heightmap or the live protocol cannot compute canonical light, the native save fails visibly and the Anvil path remains available. Keep backend checks ahead of conversion so selecting Anvil remains a fail-closed no-op for native records.

## Configuration

`WorldStorageBackend::LodestoneNative { directory }` selects the native segment. Loading also requires the active dimension's 16-aligned `min_y` and positive `height`; the record itself stores section coordinates but not a dimension definition. There are no feature flags or runtime settings for the combined chunk boundary.

## Dependencies

The adapter uses `lodestone-storage` and `lodestone-storage-schema` for atomic typed envelopes, `ChunkColumn` for blocks/biomes/entities, `lodestone_world::ColumnLight` for canonical light, and `ScheduledTickHandle` for non-destructive queue snapshots and restart handoff.
