# World spawn resolution

## What it is

World spawn resolution selects and persists one safe Overworld anchor for a
world. It also provides a cancellation-safe coordinator so concurrent joins can
share the one terrain search instead of each paying for it.

## How it works

`find_initial_spawn` checks the origin column, then walks the bounded candidate
spiral. Cheap horizon samples reject wholly fluid candidates before a full
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

## Configuration

There are no feature flags. Search measurements are process-wide cumulative
atomics and are read through `spawn_search_metrics`. Browser mounts with
`logLevel: "debug"` report elapsed time, candidate and column counts, horizon
samples, the result coordinate, and fallback status.

## Dependencies

The resolver uses `ChunkSource` and `ChunkColumn` from the integrated server,
the compact collision and fluid tables from `lodestone-data`, the portable
clock from `lodestone-time`, and the persisted world scalar store in
`world_state.rs`.
