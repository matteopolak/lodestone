# Chunk lifecycle: tickets, status, unloading, and asynchronous generation

## What it is

The design for server-side chunk residency: how tickets decide which chunks are retained or simulated, and how chunks leave memory safely. The shipped store, ticket graph and streaming behaviour are in [`chunk-lifecycle.md`](../chunk-lifecycle.md); this plan keeps the constants and the work still open.

## How it works

### Residency levels

| level | required state |
|---|---|
| `<= 31` | fully generated and entity-simulating |
| `32` | fully generated and block-ticking |
| `33` | fully generated but not ticking |
| `> loaded ceiling` | not resident |

The loaded ceiling is `33 + generation_dependency_radius`. The one-step generator has no intermediate statuses, so the radius is zero and the ceiling 33; a multi-stage generator would need at least eight rings, so do not use 33 as a general constant. A chunk's level determines both residency and generation target; add no second priority score.

### Tickets

A ticket has a category, a level and a remaining lifetime. A radius ticket stores one ticket at its centre with level `33 - radius`; levels propagate by Chebyshev distance (`min(ticket.level + chebyshev)`), run once for loading tickets and once for simulation tickets so a chunk can be resident without ticking. Lifetimes expire once per world tick; a persistent ticket serialises category, level and remaining lifetime. Category flags stay independent so the two graphs can filter them.

| category | lifetime (ticks) | flags | purpose |
|---|---:|---|---|
| player-spawn | 20 | loading | join-area residency (radius 3: ring at distance 3 is level 33) |
| spawn-search | 1 | loading | short search residency |
| dragon-fight | persistent | loading, simulation | encounter activity |
| player-loading | persistent | loading | view delivery without simulation |
| player-simulation | persistent | simulation, dimension-active | simulation around a player |
| forced | persistent | persistent, loading, simulation, dimension-active | explicit retained area (level 31) |
| portal | 300 | persistent, loading, simulation, dimension-active | cross-dimension retention |
| ender-pearl | 40 | loading, simulation, dimension-active | travelling entity's area |
| unknown | 1 | loading, may-expire-unloaded | safe fallback |

Expected values for tests: a level-31 simulation ticket at `(0, 0)` gives 31 at `(0, 0)`, 33 at `(0, -2)`, 34 at `(3, 0)`; two tickets take the minimum; radius 3 gives centre 30, distance three 33, distance four 34.

### Open work

- **Unloading and saving.** When a loading level exceeds the ceiling, move the entry to a pending-unload set and do not release it while a save is in flight. There is no fixed delay: drain pending unloads while tick time remains or the queue exceeds 2,000. Routine saving is capped at 200 chunks per tick; eager saving at 20 per tick, 128 writes in flight and a 10-second per-chunk cooldown; re-arm an entry whose save is still in flight. Changing a threshold needs a mass-unload measurement and a control observing the queue. The save boundary is a `ChunkSink` trait; an unedited column may be discarded (regenerable), an edited one must stay resident, and a no-op sink must warn and never lose edits. Chunk NBT and region format belong to persistence, not the store.
- **Status model.** Storage is `Empty -> Full`. Add a generation status only when the generator can produce and consume the intermediate state, including its neighbour radius and storage transition.
- **Sectioned storage.** A dense `Vec<u16>` for 16x384x16 is about 192 KiB per column (about 54 MiB for 289 columns, 204 MiB for 1089, 792 MiB for 4225; arithmetic, not measurement). Measure peak release RSS with one arm retaining columns and one dropping them (near-zero difference means the instrument is not observing retention) before moving to section-level copy-on-write `Arc` storage and packed palettes from `lodestone-world`.

## How to change it

- New ticket category: define flags, timeout, persistence encoding and tests for both propagation graphs.
- Keep `tick.rs` changes narrow: ticket expiry, propagation and unload processing run after due block and fluid ticks and before random ticking picks its resident set.
- `ChunkStore` and the ticket store are plain structs behind `Arc`; return section handles across that boundary rather than copying columns.

## Configuration

Loaded ceiling 33 (no dependency radius). Radii and lifetimes live in the categories above. Generation offload needs Tokio's blocking pool and works on the shell's current-thread runtime.

## Dependencies

`lodestone-server` (generation source, tick loop, replication, persistence boundary), `tokio::task::spawn_blocking`, `lodestone-world` for packed copy-on-write storage.
