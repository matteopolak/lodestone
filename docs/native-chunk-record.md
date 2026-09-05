# Native chunk records

## What it is

The native server storage seam persists one complete typed chunk replacement through `NativeDirtyChunkRecord` and reopens it as `NativeChunkRecord`. The boundary keeps block, biome, heightmap, resident block-entity, canonical light, and pending block/fluid-tick state together so a partial save or load cannot silently erase a field.

## How it works

Construct `NativeDirtyChunkRecord::new` with the chunk coordinates, a `ChunkColumn`, its `ColumnLight`, and the live `ScheduledTickHandle`. `WorldStorage::write_dirty_chunk` snapshots both pending queues without draining them, validates the complete column and light shape, and commits one replacement envelope to the selected native store. The Anvil selection returns `AnvilDoesNotAcceptTypedRecords` before conversion and is not redirected through this path.

`WorldStorage::load_chunk` validates the requested dimension extent, key, current game-data version, contiguous block/light sections, palettes, biome grids, heightmap, entity NBT roots, and both tick lists before returning an owned `NativeChunkRecord`. A record with no persisted light returns `MissingStoredLight`; unsupported or malformed data is never converted into a partial result. `NativeChunkRecord::stage_scheduled_ticks` hands both decoded queues to the live scheduler at the next scheduler access while retaining each stored insertion order.

The integrated-server methods `IntegratedServer::write_dirty_native_chunk` and `IntegratedServer::load_native_chunk` expose the same boundary. They are explicit native-record consumers; the established Anvil world save/load path remains responsible for complete compatibility persistence.

## How to change it

Change `NativeDirtyChunkRecord` and `NativeChunkRecord` together when adding a chunk field. The write value must require every payload that the reader returns. Extend `encode_chunk_inner` and `decode_chunk` only after adding a typed schema field and validation for its shape; a decoder must reject an unsupported field rather than drop it. New scheduled actions require a schema enum value, lossless kind conversion, and a restart test whose ordering distinguishes trigger tick, priority, and insertion order.

Do not add a terrain-only or light-only native chunk loader. If a caller does not have a canonical light or scheduler snapshot, it must stay on the Anvil path until it can provide the complete typed input. Keep backend checks ahead of conversion so selecting Anvil remains a fail-closed no-op for native records.

## Configuration

`WorldStorageBackend::LodestoneNative { directory }` selects the native segment. Loading also requires the active dimension's 16-aligned `min_y` and positive `height`; the record itself stores section coordinates but not a dimension definition. There are no feature flags or runtime settings for the combined chunk boundary.

## Dependencies

The adapter uses `lodestone-storage` and `lodestone-storage-schema` for atomic typed envelopes, `ChunkColumn` for blocks/biomes/entities, `lodestone_world::ColumnLight` for canonical light, and `ScheduledTickHandle` for non-destructive queue snapshots and restart handoff.
