# World spawn resolution

## What it is

Selects and persists one safe Overworld spawn anchor per world, with a cancellation-safe coordinator so concurrent joins share one terrain search.

## How it works

- `find_initial_spawn` asks the source for its climate-targeted origin (`ChunkSource::spawn_origin_block`, backed by `TerrainGenerator::spawn_origin` in `lodestone-worldgen-core`), then walks the bounded 11x11 candidate spiral around that chunk. Sources without spawn targets (flat and test worlds, other dimensions) start at chunk `(0, 0)`. Without the climate step, an ocean origin spawned the player in water.
- The climate search reads the noise settings' `spawn_target` list: each point gives closed intervals for continentalness, erosion, ridges, temperature and vegetation (the Overworld has two: inland continentalness with strongly negative or strongly positive ridges). A column's fitness is the minimum over points of the summed squared distance from each quantised sample (`(v * 10000) as i64`, taken at the quart-aligned column and height zero) to its interval. Score is `fitness * 2048^2 + x^2 + z^2`, so distance breaks ties. The search scores `(0, 0)`, then rings of radius 512 to 2048 in steps of 512, then rings 32 to 512 in steps of 32 around the best. Ring points use single-precision angle and radius arithmetic because positions depend on the rounding; keep it `f32` in `radial_spawn_search`. If no candidate is valid, fall back to the found chunk's centre column at height 64 (raised to the first clear height).
- Spiral and fallback height match the reference server, but its per-player scatter within the `respawn_radius` rule on first join is not implemented: every new player starts at the world spawn block.
- Cheap horizon samples reject wholly fluid candidates before materialising a column; generated columns start each surface query at the motion-blocking heightmap, while columns without it scan fully. Accepted candidates still use the complete block and body-clearance predicates. Counters accumulate once per completed search.
- `WorldStateHandle::resolve_world_spawn` is the one-time decision boundary: the first caller runs the async search and commits to shared world state, others await and reuse it, and dropping the leader releases the claim so a later join retries. The persisted anchor is authoritative after reload.
- `WorldStateHandle::prefetch_world_spawn` uses the same boundary for a fresh Overworld source: a tracked startup task runs it during configuration and login, a racing join waits for the one result, and the future stays with the caller so shutdown can cancel it. Persisted spawns and other dimensions return immediately. The join session owns its shaped light footprint, avoiding eight redundant feature waves. The integrated constructor starts the prefetch once the shared source exists, in the server's warm-up task slot, with no second worldgen owner and no pre-admitted second 3x3 footprint. On the browser, candidates advance through the yielding generation boundary without changing order, predicate or result.

## How to change it

- Keep candidate order and the full-column predicate together in `world_spawn.rs`. A fast path must be a conservative rejection or an already-authoritative resident result, never a replacement for the final candidate check. Route new callers through `resolve_world_spawn`; startup callers use `prefetch_world_spawn` in an existing tracked task, never a search per connection.
- Compare candidate counts, full-column requests, horizon work, fallbacks, elapsed time and request counters (raw ensure calls, session leaders, existing hits, packet-neighbour admissions) with `spawn_search_metrics`. Fixtures cover land and all-fluid fallback worlds; use an independent seed in each arm.
- `REFERENCE_WORLD_SPAWNS` in the `world_spawn.rs` tests holds `(seed, x, y, z)` read from `level.dat` after booting the reference server once per seed (Apple `container`, cached `server.jar`; see [oracles-and-benchmarks](oracles-and-benchmarks.md)). Regenerate after a version bump and compare; never edit a value to pass.

## Configuration

The `spawn_target` list in the noise settings (data) selects climate targets; no feature flags. Search metrics are process-wide atomics read via `spawn_search_metrics`; browser mounts with `logLevel: "debug"` report time, counts, horizon samples, result and fallback.

## Dependencies

`ChunkSource` and `ChunkColumn` from the integrated server, `lodestone-data` collision and fluid tables, `lodestone-time`, and the world scalar store in `world_state.rs`. Per-player respawn points are covered in [respawn](respawn.md) and [respawn-anchor](respawn-anchor.md).
