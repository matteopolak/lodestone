# Anvil native chunk export

## What it is

`lodestone_server::anvil_export` converts one complete `NativeChunkRecord` into the NBT tree for an Anvil terrain-region slot. It is a bounded interoperability seam, not a world-directory exporter: callers select the destination region slot, write the returned named-NBT tree through the existing Anvil container API, and retain responsibility for world metadata, player data, and resident-entity region files.

## How it works

`preflight_chunk` reports native fields with no Anvil counterpart before conversion. The only current loss is each non-empty scheduled-tick queue's world-wide insertion sequence: the destination has per-queue tick-list order but no durable sequence field. `ChunkExportReport::decide` binds an explicit `ExportAuthorization` to that report. `export_chunk` refuses a missing, declined, or stale authorization; an accepted report writes the tick lists while reporting the discarded sequence values.

The exporter starts with `chunk_nbt::column_to_nbt_with`, so block state identifiers and every property, all three-dimensional biome cells, block entities, tick kind/position/priority, and the destination's normal section palette framing use the established chunk schema. It adds the native `MOTION_BLOCKING` map as a packed `Heightmaps` entry and materializes each present light layer as its 2 KiB section byte array, including the section below and above the build range. Missing light remains absent. Native absolute tick triggers become signed destination delays relative to the caller-supplied game time; a difference outside `i32` refuses the whole export.

## How to change it

Add a preflight report entry before discarding any newly typed native field. If the Anvil tree has a true representation, emit it and cover the field with an NBT-level fixture assertion rather than only exporting and reimporting it. Keep `write_light` aligned with `ColumnLight`'s boundary-section convention (`min_section - 1` through one section above the column). A new destination-side tick ordering field would remove the current loss report only after an external fixture demonstrates it survives a real load.

## Configuration

There is no backend switch or environment variable. The caller supplies chunk coordinates, the complete record, its destination world's current game time, and the authorization derived from the current report. `game_time` determines every emitted tick delay.

## Dependencies

The module uses `NativeChunkRecord` and `PersistedScheduledTick` from the native storage boundary, `chunk_nbt` for the existing Anvil chunk representation, and `lodestone_world` for packed heightmaps and light layers. It does not write region files itself; `lodestone-anvil` remains the container writer.
