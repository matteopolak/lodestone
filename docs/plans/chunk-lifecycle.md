# Chunk lifecycle: tickets, status, unloading, and asynchronous generation

## What it is

The design for server-side chunk residency. It defines how tickets determine
which chunks are retained or simulated, how generation is scheduled without
stalling the runtime, and how chunks leave memory safely.

## How it works

### Residency levels

Each resident chunk has one integer level. Lower levels represent stronger
requirements:

| level | required state |
|---|---|
| `<= 31` | fully generated and entity-simulating |
| `32` | fully generated and block-ticking |
| `33` | fully generated but not ticking |
| `> loaded ceiling` | not resident |

The loaded ceiling is `33 + generation_dependency_radius`. The full reference
pipeline derives that radius from per-stage neighbour requirements and needs at
least eight rings. Lodestone's current one-step generator has no intermediate
statuses, so its initial `generation_dependency_radius` is zero and its loaded
ceiling is 33. Keep this distinction explicit: using 33 as a general constant
would silently widen retained residency when multi-stage generation arrives.

A chunk's level determines both its residency and its generation target. Do
not add a second, unrelated generation-priority score.

### Tickets and propagation

A ticket contains a category, a level, and remaining lifetime. Categories
independently describe whether a ticket persists, keeps a chunk loading, keeps
it simulating, keeps its dimension active, and may expire before a chunk is
ready to save.

Adding a ticket with radius stores one ticket at its centre with level
`33 - radius`; it does not write tickets into adjacent chunks. Levels propagate
by Chebyshev distance:

```
effective_level(chunk) = min(ticket.level + chebyshev(ticket.chunk, chunk))
```

Run this propagator twice: once for loading tickets and once for simulation
tickets. The split is required for a chunk that is resident but must not tick.
For example, the temporary player-spawn ticket has a 20-tick lifetime, loading
permission only, and radius 3. It makes the distance-three ring resident at
level 33 without making it simulate. A forced ticket has level 31 and both
loading and simulation permission.

Expire ticket lifetimes once per world tick. A persistent ticket must be
serialised with its category, level, and remaining lifetime. A ticket that
cannot expire while unloaded remains until the chunk is ready for persistence.

The ticket category is data, not a caller convention. Keep the flags
independent so the loading and simulation graphs can filter them without
inventing one category per combination.

| category | lifetime (ticks) | flag mask | flags | purpose |
|---|---:|---:|---|---|
| player-spawn | 20 | 2 | loading | temporary join-area residency |
| spawn-search | 1 | 2 | loading | short search-area residency |
| dragon-fight | persistent | 6 | loading, simulation | encounter activity |
| player-loading | persistent | 2 | loading | view delivery without simulation |
| player-simulation | persistent | 12 | simulation, dimension-active | simulation around a player |
| forced | persistent | 15 | persistent, loading, simulation, dimension-active | explicit retained area |
| portal | 300 | 15 | persistent, loading, simulation, dimension-active | temporary cross-dimension retention |
| ender-pearl | 40 | 14 | loading, simulation, dimension-active | keep a travelling entity's area active |
| unknown/fallback | 1 | 18 | loading, may-expire-unloaded | safe handling for an unknown category |

### Generation and serving

Chunk generation runs off the current-thread runtime's core thread. The
blocking worker fan-out is wrapped in `tokio::task::spawn_blocking`, then its
result returns to the async task. Calling the synchronous fan-out directly on
the runtime thread stalls network timers and the world tick until every worker
joins.

`ChunkStore` owns resident entries and a per-coordinate in-flight map. Its
read API returns `Arc<ChunkColumn>`, not an owned clone. Multiple requests for
the same missing coordinate share one in-flight generation and one stored
column. The packet encoder, scheduled-tick path, random-tick path, and block
write path must all read or modify the same store; a store with no live
consumer is incomplete.

The first storage status is deliberately `Empty -> Full`. It cannot represent
generation dependency rings, cross-border feature writes, or a separate
disk-restoration pipeline. Revisit the status model when the generator exposes
stage boundaries.

### Unloading and saving

When a loading level exceeds the loaded ceiling, move the entry to a pending
unload set. Do not release it while a save is in flight. Once saving completes,
release the stored data within the tick's unload budget.

There is no fixed delay between removing a ticket and releasing a chunk. Drain
pending unloads while tick time remains, or while the queue exceeds 2,000
entries. Routine saving is capped at 200 chunks per tick; eager saving is
capped at 20 per tick, has at most 128 writes in flight, and gives each chunk a
10-second eager-save cooldown. Re-arm an entry whenever its save is still in
flight. These thresholds bound tick and write pressure; changing one requires
a mass-unload measurement and a control that observes the queue.

The initial save boundary is a `ChunkSink` trait. Until a durable chunk-schema
writer is available, an unedited column may be discarded because it is
regenerable from the seed; an edited column must remain resident. A no-op sink
must log a warning and must never permit edited data loss. Chunk NBT schema and
region-format decisions belong to the persistence subsystem, not the lifecycle
store.

### Memory model

Retaining generated data changes the memory cost materially. A dense
`Vec<u16>` covering 16 by 384 by 16 blocks is about 192 KiB per column before
palette and allocation overhead: approximately 54 MiB for 289 columns at
radius 8, 204 MiB for 1089 at radius 16, and 792 MiB for 4225 at radius 32.
These are arithmetic estimates, not measurements.

Measure peak RSS in release mode with one benchmark arm retaining generated
columns and one dropping them. The two modes are also the instrument control:
an approximately zero RSS difference means the measurement is not observing
retention. If retained memory requires compaction, use section-level
copy-on-write `Arc` storage and packed palettes from `lodestone-world`; do not
clone section data when encoding or ticking.

## Implementation sequence

1. Offload generation and prove the runtime tick advances during a generation
   burst. The negative control calls the synchronous path directly on a
   current-thread runtime and must show no timer progress.
2. Add the retained-versus-dropped release benchmark before widening
   residency.
3. Add `ChunkStore` with in-flight deduplication, then route live generation,
   packet encoding, scheduled ticks, random ticks, and writes through it.
4. Add the two ticket propagators and push their levels into `ChunkStore` once
   per tick.
5. Replace view-driven residency with player loading and simulation tickets.
   Replication remains per connection: it tracks which chunks a client has
   received, while tickets define which chunks exist in memory.
6. Add pending unloads and the guarded save boundary.
7. Add temporary player-spawn and persistent forced tickets once the world
   spawn position and ticket persistence are available.

`ChunkStore` and the ticket store are plain structs behind `Arc` handles. This
allows connection tasks to access chunk data without taking an ECS `World`
lock, while retaining a clean path to later resource ownership. Return section
handles across that boundary rather than copying columns.

## Verification

- On a current-thread runtime, an offloaded generation burst advances a local
  timer counter. The direct synchronous control advances approximately zero
  times. Use a counter scoped to the test, not an accumulated server metric.
- A counting `ChunkSource` must generate each distinct chunk once across
  repeated ticks. Construct fresh sources for positive and negative arms: the
  generator's memo cache would otherwise hide duplicate work.
- A level-31 player-simulation ticket at `(0, 0)` yields levels 31 at `(0, 0)`,
  33 at `(0, -2)`, and 34 at `(3, 0)`. Two tickets take the minimum level.
- A loading-only player-spawn ticket produces resident, non-simulating chunks.
  A deliberately single-tracker control must fail that assertion.
- Radius 3 gives the spawn centre level 30, its distance-three ring level 33,
  and the distance-four ring level 34. Without refresh it expires after 20
  ticks. A forced level-31 ticket simulates with no connected player.
- Moving a ticket outward lowers resident count after the unload drain. An
  edited column remains resident while the sink cannot save it.

## How to change it

- Extend ticket categories by defining their flags, timeout, persistence
  encoding, and tests for both propagation graphs.
- Add a generation status only when the generator can produce and consume that
  intermediate state, including its neighbour radius and storage transition.
- Keep `tick.rs` changes narrow and anchored: ticket expiry, propagation, and
  unload processing occur after due block and fluid ticks and before random
  ticking selects its resident set.
- Treat sectioned storage as a measurement-driven change. Update the retained
  memory benchmark and its byte budget with the observed representation cost.

## Configuration

The initial status model uses a loaded ceiling of 33 because the current
generator has no dependency radius. Ticket radii and lifetimes are encoded by
their categories; the temporary player-spawn ticket is radius 3 for 20 ticks.
Runtime generation offload requires Tokio's blocking pool and works with the
shell's current-thread runtime.

## Dependencies

- `lodestone-server`'s generation source, tick loop, replication path, and
  world persistence boundary.
- `tokio::task::spawn_blocking` for non-blocking runtime integration.
- `lodestone-world` for optional packed, copy-on-write section storage.
