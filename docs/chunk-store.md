# The server-side chunk store

## What it is

`ChunkStore` (`crates/lodestone-server/src/chunk_store.rs`) is a bounded,
least-recently-used cache of generated chunk columns that wraps any
`ChunkSource` and *is* a `ChunkSource`. It exists because the integrated server
had no column cache at all, so a column was re-generated from scratch on every
request — and two different repeating 50 ms timers were requesting them, which
made singleplayer effectively unplayable. It is unit **U3** of
[`plans/chunk-lifecycle.md`](./plans/chunk-lifecycle.md) (issue #289).

## The bug it fixes, and why the code said it was harmless

Two doc comments in this crate stated, correctly at the time, that regenerating
an unedited column was a cost rather than a defect:

- `OverworldChunkSource` retains **only edited** columns, arguing that because
  the generator is deterministic, "regenerate on every request" and "cache
  forever" are observationally identical.
- `run_tick_loop` called the per-tick regeneration *"a real, documented
  performance gap … not a correctness one."*

Both were reasoning about a *cheap* generator. Generation then composed in
carvers, ores and vegetation (vegetation alone is ~62% of the cost, ore ~18%).
Measured in release, on four cold columns from four independently constructed
sources, at load average 3.7:

```text
column 0: 803.0ms   column 1: 840.8ms   column 2: 1.001s   column 3: 991.4ms
mean: 909.2ms
```

A 20 Hz tick has a **50 ms** budget. One regeneration is therefore ~18 tick
budgets, and two separate consumers were paying it on a repeating timer:

| site | cadence | columns per firing | task starved |
|---|---|---|---|
| `run_tick_loop`'s random-tick loop | every tick (50 ms) | the whole `tick_area` — **49** | the world tick |
| `serve_connection`'s `vitals_tick` submersion probe | every 50 ms, once the client has sent a position | 1, to read a **single block** | the *connection*, i.e. chunk streaming |

That is ~44.5 s of generation per 50 ms world-tick budget (about **0.022 TPS**)
plus ~909 ms per 50 ms on the connection task.

The second row is the one worth remembering, because no call site looks wrong:
**`ChunkSource::block_state`'s default implementation is
`self.column(cx, cz).block_state(..)`**, so reading one block regenerates a
whole 16×384×16 column. It fires only once `player_pos` is `Some`, which is
exactly why the reported symptom was "chunks stop generating *after the first
load*" rather than "no chunks ever arrive". `ChunkStore` overrides
`block_state` as well as `column`.

## How it works

One `Mutex<Cache>` over a `HashMap<(i32, i32), Entry>` plus a monotonic
use-stamp per entry. Eviction is a linear scan for the smallest stamp, which
runs only on a miss — a miss has just paid a generation four orders of magnitude
more expensive than 512 integer comparisons, so an intrusive LRU list would be
optimising the cheap half.

Three properties are load-bearing:

1. **Generation happens with the lock released.** A miss unlocks, calls
   `source.column()`, then re-locks to insert. Holding the lock across a 909 ms
   generation would serialise `generate_columns_parallel`'s scoped thread
   fan-out and undo issue #414.
2. **The insert after that window never overwrites.** Another thread may have
   inserted in the gap, and its entry may carry a `set_block` edit that this
   thread's freshly generated column predates. First writer wins.
3. **Eviction is lossless, so the bound needs no exception for edited columns.**
   `set_block` writes through to the inner source *first*;
   `OverworldChunkSource::edits` retains it there permanently. Dropping a cache
   entry therefore costs a regeneration and never a block, because the
   regeneration goes back through `OverworldChunkSource::column`, which consults
   `edits`.

Property 3 is the difference between this unit and the plan's U6 (unloading).
U6 drops the *authoritative* copy and so needs the much more careful "refuse to
drop an edited column" rule; this only drops a cache.

## How to change it, and the gotchas

**The wiring is three lines, all in `integrated.rs`**, one per constructor that
builds a source (`open_in_memory_with_entities`, `open_in_memory_with_mobs`,
`bind`): `Arc::new(source)` became `Arc::new(ChunkStore::new(source))`.
`tick.rs` and `server.rs` needed **no** edits, because the store satisfies
`ChunkSource`. That was deliberate — `tick.rs` is one of the most contended
files in the repo.

`world_source` in `open_in_memory_with_mobs` is deliberately **not** wrapped:
`MobHandle::seeded` reads each column exactly once, so retention buys it
nothing.

**Do not add a `with_column_mut(cx, cz, f)` closure API.** It is the obvious way
to avoid the clone that `column()` returns, and it **deadlocks**:
`run_tick_loop` mutates its column (`random_tick::tick_chunk` takes
`&mut ChunkColumn`) *and* calls `world.set_block` for the same chunk in the same
breath, so a lock held across `f` re-enters. The `try_lock` workaround silently
skips a cache update on genuine contention and then serves a stale block, which
is worse than the deadlock.

**The clone is deliberate and measured.** `ChunkSource::column` returns by
value, so a store read is a ~192 KiB `memcpy` — **3.1 µs**, measured, against
the 909 ms it replaces. To make reads a refcount bump instead, the trait
signature has to change; that is the plan's U8, together with sectioned
storage.

## Configuration

`DEFAULT_CAPACITY` (currently **512**) is the only knob. Measured cost, by
`/usr/bin/time -l` on the release test binary:

| arm | peak RSS |
|---|---|
| 512 columns retained | 105.4 MiB |
| the same 512 touched, retention off | 7.8 MiB |
| **delta** | **97.6 MiB**, i.e. 195.5 KiB per column |

That is within 2% of the 192 KiB `size_of` arithmetic; the remainder is the
palette and biome `String`s and the map. **The two arms are each other's
control** — a delta near zero would mean the columns were dropped in both arms,
or that the pages were never faulted in, and the run would be a failure to
measure rather than evidence that residency is free. `touched_column` in the
test module exists for that second reason: `ChunkColumn::new` allocates through
`alloc_zeroed`, and pages that are never written do not appear in RSS.

Lowering the capacity to 128 (~24 MiB) **still fixes the reported bug
completely**: the starvation fix needs only the 49-column `tick_area` resident.
512 was chosen to also cover the default streamed view (`render_distance` 8 ⇒
`view_radius` 9 ⇒ 361 columns), so walking in a circle does not pay 909 ms per
column again.

## Gates

In `chunk_store.rs`'s own test module, because they need `pub(crate)` access to
`run_tick_loop`:

- `the_store_generates_each_column_exactly_once_across_many_ticks` — the
  load-bearing gate. **A count, not a duration**: counts are immune to machine
  load and durations are not. 12 ticks over the 49-column tick area must produce
  exactly 49 generations, and the worst *per-chunk* count must be 1.
- `without_retention_every_chunk_is_regenerated_every_tick` — the negative
  control, as a real configuration (`with_capacity(source, 0)`) rather than a
  temporary neuter, so it is permanent. It measures exactly 49 × 12 = 588.
  Observed failing the positive assertion before the fix: chunk `(3, 3)`
  generated **12** times where the fix requires **1**.
- `repeated_single_block_probes_generate_one_column_not_forty` — the
  `block_state` half, with the unwrapped source as an in-body control.
- `edits_survive_both_a_reread_and_an_eviction` — property 3 above, against
  `OverworldChunkSource` because it is the only source in the crate with real
  retention beneath. Testing it against a source whose `set_block` is the no-op
  default would be a world-species vacuity.
- `a_miss_does_not_hold_the_lock_across_generation` — the one duration gate
  here, because "does a lock serialise this" has no count. The two hypotheses
  are 2× apart (parallel < 240 ms, serialised ≥ 480 ms) and the threshold sits
  between them.
- `measure_rss_with_retention` / `measure_rss_without_retention` /
  `measure_real_column_generation_cost` — `#[ignore]`d measurement tools, not
  assertions. Release profile only.

**The trap every count gate here avoids:** `OverworldGenerator` carries a
per-instance 512-entry memo cache, so a generation-count gate built on
`overworld_chunk_source` passes *even with a completely broken store*. Every
count above is taken on a hand-written `CountingSource` with no cache of any
kind. The same vacuity was found and fixed once already in `chunk.rs`'s
`parallel_generation_is_deterministic_and_matches_serial`.

## Known remaining gaps (not fixed by this)

- **The initial join burst is still slow.** The store removes *re*-generation,
  not first generation. A `view_radius` 9 join is 361 first-time columns, and
  `server.rs` generates the whole set before encoding any of them, in raster
  order from the `(-9, -9)` corner — the player's own column is item ~180 of
  361. That ordering defect is separate from this one and is what "chunks are
  not close to me" is really about.
- **Nothing moves the player down.** The server runs no player gravity and
  sends no corrective teleport (`fall.rs` says so explicitly); the client
  freezes the player when its column is unloaded
  (`lodestone-shell/src/sim/collide.rs`'s `is_chunk_loaded` early return, via
  `PlayerCollision::Pending`). So "stuck in the air" is a *consequence* of
  starvation, and resolves when the spawn column arrives.

## Dependencies

None new. Standard library only (`HashMap`, `Mutex`), over
`crate::chunk::{ChunkColumn, ChunkSource}`. Consumed by
`crate::integrated`; read by `crate::tick::run_tick_loop` and
`crate::server`'s connection loop through the `ChunkSource` trait.
