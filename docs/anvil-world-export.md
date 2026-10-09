# Anvil world terrain export

## What it is

`lodestone_server::anvil_world_export` exports one explicitly selected batch of complete native terrain records into a new Anvil world directory. It is terrain-only: the result holds `region/` files, not metadata, players, entities, POI or auxiliary data.

## How it works

- `WorldExportInput` names every chunk and supplies the native vertical extent, tick-conversion game time, region compression scheme and timestamp. Its named builder validates and canonicalises coordinates (the positional `new` is a compatibility wrapper), so neither caller order nor the clock affects output. The conversion command's `--all-terrain` snapshots the native store's recovered latest chunk-key index before building the same input; native keys carry a dimension and only Overworld columns are exported.
- `preflight_world_export` loads every named record and aggregates per-chunk loss reports. `export_world_directory` repeats the load as its snapshot, checks one `WorldExportAuthorization` against the aggregate and converts every chunk in memory before touching the filesystem. The one existing loss is pending tick insertion order (list order is kept, the native scheduler sequence is not).
- `snapshot_world_export` gives an in-process caller a point-in-time snapshot of a reviewed explicit selection: it holds the storage lock through every typed decode and owns the records for `preflight_native_world_export` and `export_native_world_snapshot`. `snapshot_native_world_export` is the all-native variant: it captures the Overworld's `WorldStorage::native_chunk_records` under the lock, owns every decoded `NativeChunkRecord` and rejects empty or incomplete terrain. A later incremental write cannot replace a reviewed chunk; both are values, not live views.
- After all conversions succeed, region files and oversized-chunk sidecars are written under a same-parent staging directory (`.<destination>.lodestone-export-staging`) that is renamed to the previously absent destination only if every write succeeded. A conversion failure publishes nothing; an interrupted write leaves a staging directory the next run refuses to reuse.

## How to change it

- Use the named builder at new call sites; `new` must keep the same validation. `--all-terrain` takes one copied key snapshot via `WorldStorage::native_chunk_coordinates` and must not turn an empty snapshot into a successful empty export. Use `snapshot_world_export` for a reviewed subset and `snapshot_native_world_export` for all committed terrain.
- Add a lossy native field to the one-chunk exporter first so `WorldExportReport::unsupported_count` counts it through the per-chunk report. No opaque payload copying: fields have a typed Anvil conversion or are reported and discarded under authorization.
- Keep all conversion before `publish_regions`. Replacing an existing destination needs its own recovery protocol and is rejected. A new output directory type is written under staging before the rename, with a filesystem reopen control.

## Configuration

No environment variables or defaults: callers choose coordinates, `min_y`, `height`, `game_time`, `CompressionScheme` and timestamp, plus an absent destination path.

`lodestone-server anvil-convert export` takes `--source` with a matching `--native-path`, a distinct absent `--destination`, `--min-y`, `--height`, `--game-time`, `--timestamp`, `--compression`, and either one or more `--chunk x,z` or `--all-terrain` (exclusive; the latter selects committed native keys in `(x, z)` order without decoding, and preview prints the exact count). The first run omits `--apply`, prints a payload-free report and review token and creates no directory. A lossless report applies with `--apply`; a lossy one also needs its exact `--acknowledge` token.

## Dependencies

`world_storage::WorldStorage`, `anvil_export` (chunk NBT conversion and loss semantics), `lodestone_anvil::region` (compression, layout, sidecars) and `bon` for the builder.
