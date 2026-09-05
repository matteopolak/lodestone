# Anvil world terrain export

## What it is

`lodestone_server::anvil_world_export` exports one explicitly selected batch of complete native terrain records into a new Anvil world directory. It is terrain-only: the resulting directory contains `region/` terrain files, not metadata, players, entities, POI, or auxiliary data.

## How it works

`WorldExportInput` names every chunk to export and supplies the native vertical extent, tick-conversion game time, region compression scheme, and timestamp. The constructor canonicalizes chunk coordinates and rejects duplicates, so neither store enumeration nor the current clock can affect the resulting terrain files.

`preflight_world_export` loads every named record and aggregates the existing per-chunk loss reports. `export_world_directory` repeats that load as its source snapshot, checks one `WorldExportAuthorization` against the full aggregate, and converts every chunk in memory before touching the destination filesystem. The one existing loss is pending tick insertion order; the destination preserves list order but does not retain the native scheduler sequence.

After all conversions succeed, the coordinator writes all region files and oversized chunk sidecars under a same-parent staging directory. It renames that directory to the caller's previously absent destination only when every write has succeeded. A conversion failure therefore cannot publish a partly converted world; an interrupted filesystem write can leave only an explicit staging directory that the next invocation refuses to reuse.

## How to change it

Keep the selection explicit until native storage has a separately reviewed whole-world enumeration contract. Add any new lossy native field to the one-chunk exporter first, then ensure `WorldExportReport::unsupported_count` includes it through the per-chunk report. Do not add opaque payload copying here: source fields either have an existing typed Anvil conversion or are reported and discarded under authorization.

Keep all conversion before `publish_regions`. Publishing an existing destination would require a separately designed replacement/recovery protocol; this coordinator intentionally rejects it. If output contents gain another directory type, write it under staging before the final rename and add a filesystem reopen control.

## Configuration

There are no environment variables or implicit defaults. Callers choose chunk coordinates, `min_y`, `height`, tick `game_time`, `CompressionScheme`, and region timestamp in `WorldExportInput`, then pass a destination path that does not exist. The staging directory is a deterministic same-parent sibling named `.<destination>.lodestone-export-staging`.

## Dependencies

The module composes `world_storage::WorldStorage`, `anvil_export`, and `lodestone_anvil::region`. `anvil_export` owns chunk NBT conversion and loss semantics; the Anvil crate owns compression, region layout, and external sidecars.
