# Distant horizon profiling

## What it is

`horizon-profile` is a finite, headless Samply input for the coarse distant-terrain path. It requests
exactly 256 far columns through the staged reduced-generation seam and exercises 128-, 192-, and
256-chunk candidate selection plus bounded tile updates without creating a window, GPU adapter, or
network connection.

## How it works

`lodestone::horizon_profile::run_horizon_profile` first recentres one fixed 9×9 `DistantTerrain`
grid at three world locations. It uses the same `horizon_tile_intersects_radius` predicate as the
renderer, then writes at most six 64×64-cell tiles per location from the generator's preliminary
surface query. It then requests a fixed 16×16 square at chunk coordinates 240..255 by -8..7 through
`OverworldChunkSource::column_at(..., ChunkGenerationStage::Shaped)`, the same reduced-generation
path used by far streaming. Each returned column is checked for the `Shaped` stage and contributes
its solid-block count, while staged store entries and evictions are reported after the pass.

The executable prints separate `far-columns` and `horizon` phase lines. The far line includes
requested, shaped, and full-column counts, solid blocks, and staged-store entries/evictions. The
horizon line includes candidates, updated/skipped tiles, written cells, and the fixed atlas byte
counters. Skipped tiles are eligible work deliberately deferred by the six-tile per-location budget;
they keep a capture focused and make the bound visible. Both atlas counters remain 2,654,208 bytes
because the CPU grid and two packed GPU texture lanes have the same fixed footprint.

## How to change it

Keep `PROFILE_HORIZON_DISTANCE_CHUNKS` at the supported 256-chunk tier and
`PROFILE_FAR_CHUNKS` at 256 unless the underlying fixed grid or profiling question changes too.
Change `PROFILE_TILE_UPDATE_BUDGET` only with the workload checks: it must still update real tiles
while skipping candidates. Keep the far square outside `PROFILE_NEAR_DISTANCE_CHUNKS`, and use the
shared renderer predicate rather than a profiling-only radius calculation, or the capture will no
longer describe what the renderer selects.

## Configuration

The workload seed is fixed at `42`; it accepts no workload-size environment variables and writes no
files. Run a direct witness with `just profile-distant-horizon`. To record a local capture, use:

```text
LODESTONE_TARGET_DIR=/private/tmp/lodestone-horizon-target LODESTONE_JOBS=2 \
  just samply-distant-horizon bench-results/profiles/distant-horizon.json.gz
```

The capture location is caller-selected and is not tracked. The command requires a local Samply
installation; the workload itself does not require a GPU adapter. Counters prove path coverage and
the bound; CPU samples and elapsed time are machine-load-sensitive observations and must not be
treated as run-to-run comparable results.

## Dependencies

The workload depends on `lodestone_render::DistantTerrain` and its shared tile-candidate predicate,
`lodestone_server::OverworldChunkSource`/`ChunkSource` for preliminary samples and staged
`ChunkGenerationStage::Shaped` requests, and the staged world-generation store behind that seam.
The optional recorder is Samply.
