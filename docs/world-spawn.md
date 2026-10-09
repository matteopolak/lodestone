# World spawn resolution

## What it is

World spawn resolution selects and persists one safe Overworld anchor for a
world. It also provides a cancellation-safe coordinator so concurrent joins can
share the one terrain search instead of each paying for it.

## How it works

`find_initial_spawn` first asks the source for its climate-targeted origin
(`ChunkSource::spawn_origin_block`, backed by
`TerrainGenerator::spawn_origin` in `lodestone-worldgen-core`), then checks that
chunk's column and walks the bounded 11x11 candidate spiral around it. A source
with no spawn targets (flat and test worlds, non-Overworld dimensions) starts at
chunk `(0, 0)`. Without the climate step a world whose origin is ocean spawned
the player in open water, because the spiral never reached land.

The climate search reads the noise settings' `spawn_target` list: each point
gives closed intervals for continentalness, erosion, ridges, temperature and
vegetation (the shipped Overworld has two points, inland continentalness with
strongly negative or strongly positive ridges). A block column's fitness is the
smallest, over the points, of the summed squared distance from each quantised
sample (`(v * 10000) as i64`) to its interval, sampled at the quart-aligned
column and height zero. The score is `fitness * 2048^2 + x^2 + z^2`, so distance
from the origin breaks ties. The search scores `(0, 0)`, then rings of
radius 512 to 2048 in steps of 512 around it, then rings of radius 32 to 512 in
steps of 32 around the best so far. Ring sample points use single-precision
angle and radius arithmetic, because the sampled positions depend on that
rounding; keep it `f32` when editing `radial_spawn_search`. If no spiral
candidate is valid the fallback is the found chunk's centre column at height 64
(raised to the first clear height).

The initial spiral and fallback height match the reference server; its
per-player scatter within the `respawn_radius` rule on first join is not
implemented, so every new player starts at the world spawn block. Cheap horizon samples reject wholly fluid candidates before a full
column is materialized; generated columns use their motion-blocking heightmap
to begin each surface query at the highest possible support, while columns
without that metadata retain the complete scan. Accepted candidates still use
the complete block and body-clearance predicates. Search counters are
accumulated once per completed search, so instrumentation does not add
per-cell atomic traffic.

`WorldStateHandle::resolve_world_spawn` owns the one-time decision boundary.
The first caller performs the supplied asynchronous search and commits the
result to the shared world state. Other callers await a notification and reuse
the committed value. Dropping the leader releases the claim, allowing a later
join to retry. The persisted anchor remains the source of truth after reload.

`WorldStateHandle::prefetch_world_spawn` uses that same boundary for a fresh
Overworld source. A tracked server startup task can run it while configuration
and login proceed; a racing join waits for, and then consumes, the one result.
The future stays with the caller so the server's normal shutdown signal can
cancel it without leaving an unowned task behind. Persisted spawns and
non-Overworld sources return without touching terrain. The prefetch stops once
the spawn is published. The joining generation session owns its shaped light
footprint and final settlement, avoiding eight redundant full feature waves
that would otherwise compete with the same join.

The in-memory integrated constructor starts this prefetch as soon as its shared
source exists and retains it in the server's warm-up task slot. Authentication
and protocol setup therefore overlap the search without creating a second
worldgen owner. Configuration does not pre-admit a second 3x3 spawn footprint;
the normal join session owns that region. On the browser target, candidate
columns advance through the yielding generation boundary. Candidate order, the
authoritative full-column predicate, and the final committed spawn remain
unchanged while the worker can service ticks, packets, and cancellation between
stages.

## How to change it

Keep candidate order and the full-column predicate together in
`world_spawn.rs`. A new fast path must be a conservative rejection or an
already-authoritative resident result; it must not replace a final candidate
with a shaped column. Wire new callers through `resolve_world_spawn` so
cancellation and concurrent joins retain the same behavior.

Startup callers should use `prefetch_world_spawn` and store the future in an
existing tracked task. Do not create a separate spawn search for each
connection.

Use `spawn_search_metrics` to compare candidate counts, full-column requests,
horizon work, fallbacks, elapsed time, and the request counters (raw ensure
calls, request-session leaders, existing hits, and packet-neighbour admissions).
Existing world-generator fixtures
cover both land and all-fluid fallback worlds; keep an independent seed in each
arm when extending those checks.

`REFERENCE_WORLD_SPAWNS` in the `world_spawn.rs` tests holds `(seed, x, y, z)`
spawns read from `level.dat` after booting the reference server once per seed
(`level-seed` set, Apple `container`, cached `server.jar` of `mc-version`; see
`oracles-and-benchmarks.md`). Regenerate them after a version bump the same way
and compare, rather than editing a value to make the test pass.

## Configuration

The noise settings' `spawn_target` list (data, not code) selects the climate
targets. There are no feature flags. Search measurements are process-wide cumulative
atomics and are read through `spawn_search_metrics`. Browser mounts with
`logLevel: "debug"` report elapsed time, candidate and column counts, horizon
samples, the result coordinate, and fallback status.

## Dependencies

The resolver uses `ChunkSource` and `ChunkColumn` from the integrated server,
the compact collision and fluid tables from `lodestone-data`, the portable
clock from `lodestone-time`, and the persisted world scalar store in
`world_state.rs`.

Per-player respawn points (beds and respawn anchors) are resolved at death time; see [respawn.md](respawn.md) and [respawn-anchor.md](respawn-anchor.md).
