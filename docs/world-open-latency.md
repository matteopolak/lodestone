# World-open latency in singleplayer

## What it is

The two orderings that made opening a singleplayer world feel broken — *"it
takes forever to load and the chunks generated are not close to me"* — and how
they were fixed. Mob seeding used to generate the whole tick area **inside the
constructor** from a second, independent generator (issue #454), and the join
used to generate all 361 columns of the view before encoding any, in raster
order from the far corner (issue #453). Both live in `lodestone-server`; both are
about *ordering*, not throughput. They are the tail of the same report the
[`ChunkStore`](./chunk-store.md) fix (#289 U3) opened.

## How it works

Three things happen when a shell opens a singleplayer world, in this order:

1. `lodestone-shell`'s `net.rs` calls
   `IntegratedServer::open_in_memory_with_mobs`, inside `runtime.block_on`.
2. That constructor wraps the caller's `ChunkSource` in a `ChunkStore`, builds
   the handles, and spawns three tasks: the connection, the world tick loop, and
   (since #454) mob seeding.
3. The client connects and `serve_connection` streams the initial view.

### #454 — nothing generates on the calling thread any more

`MobHandle::seeded` used to run at step 2, synchronously, before any task
spawned. It called `ChunkWorld::from_source`, a plain serial loop over the tick
area — and `net.rs` passes `mob_radius = view_radius.clamp(1, 3)`, so that area
is `-3..=3` on both axes: **49 columns**, not the 3×3 a quick read suggests.

Worse, it read them from `world_source`, a *second* `OverworldChunkSource`
constructed alongside the one the connection serves from. The two shared
nothing, so opening a world generated the same 49 columns **twice**.

The fix is both halves at once:

- the `ChunkStore` is now built **before** anything mob-related, and seeding
  reads through it, so a column is generated once per world;
- seeding moved to its own task (`seed_task`), which fetches its columns through
  `generate_columns_offloaded` — parallel *and* on the blocking pool — and then
  calls `MobHandle::reseed` to swap the terrain snapshot and population into the
  already-shared handle.

`world_source` is now ignored. It survives only because
`open_in_memory_with_mobs`'s signature is named by `net.rs`, a brokered choke
point in this repo; deleting the parameter is a behaviour-free follow-up
(issue #436).

Measured in release, at the shell's own parameters, on a quiet box:

| | wall clock |
|---|---|
| pre-fix synchronous seeding (49 columns, one fresh source) | **10.86 s** |
| post-fix constructor, end to end | **75.6 ms** |

**The issue predicted ~45 s and that was wrong — read this before quoting a
number.** 45 s is `49 × 909 ms`, and the 909 ms figure was measured across four
*independently constructed* sources precisely so the generator's 512-entry memo
cache would absorb nothing. Seeding is the opposite case: one source, 49
**contiguous** columns, so the memo absorbs a great deal of shared work and the
real per-column cost is about **222 ms**. The stall was real and worth removing;
it was 4× smaller than the arithmetic said. Durations here also showed a 2.3×
spread from machine load alone, so treat both figures as provisional.

### #453 — the view streams outward from the player, encoded as it goes

`serve_connection`'s join used to build every coordinate of
`[-view_radius, view_radius]²` in a `cz`-outer/`cx`-inner walk, `await` one
`generate` over all of them, and only then start encoding. At `view_radius = 9`
that is three compounding problems:

1. the player's own column was item **180 of 361** on the wire (asserted in the
   control test, not estimated);
2. nothing at all reached the client until the last column finished generating;
3. `ViewTracker::build_batch` sorts lexicographically, so the *recentring* path
   has the same shape.

`join_view_rings` (`server.rs`) replaces the flat walk with **Chebyshev rings**:
ring 0 is the single column the player stands in, ring `r > 0` holds `8r`
columns, and the join loop generates and encodes one ring before asking for the
next. So the first chunk is encoded after **one** column of generation instead of
361, and terrain grows outward from the player rather than inward from a corner.

Still one chunk batch, not one per ring — the `begin_chunk_batch` /
`end_chunk_batch` markers stay outside the loop, so the client's issue-#270 flow
control sees exactly the sequence it always did.

### The recovery loop this also unblocks

`lodestone-shell/src/sim/collide.rs:257-262` returns `None` for an unloaded
player column, which becomes `PlayerCollision::Pending`, zeroes velocity and
forces `on_ground = true` — vanilla's wait-for-chunks freeze. A frozen player
never crosses a chunk boundary, so `recenter`'s `(cx, cz) == center` guard
short-circuits and **nothing new is requested** until the player's own column
lands. Under the old raster order that was item ~180, so slow spawn terrain was
self-sustaining. `recenter` itself was never the bug and was ruled out before
either fix.

## How to change it, and the gotchas

- **Do not "simplify" `join_view_rings` into a sort of the flat walk.** Returning
  *groups* is the half that fixes latency; a flat proximity-sorted `Vec` would
  satisfy every ordering assertion and leave time-to-first-chunk exactly as bad.
  The gate below catches this specifically, via the generation counter.
- **Do not re-sort `ViewTracker::build_batch`.** Its lexicographic
  `sort_unstable` is there for byte-reproducibility. Proximity belongs at the
  enumeration/dispatch layer. The recentring path is left alone deliberately —
  its diffs are ~19 columns per boundary crossing, not 361.
- **Order within a ring must stay a pure function of `view_radius`.** It is the
  same `dz`-outer/`dx`-inner walk, filtered — which is what keeps the emitted
  byte sequence independent of thread scheduling and of which `SourceRef` arm
  generated it.
- **The inner rings under-use the fan-out.** Rings 0 and 1 are smaller than
  `available_parallelism`, so total generation costs slightly more than one
  361-column batch. That is the deliberate trade: a fraction of a second of
  throughput for time-to-first-chunk falling from the whole view to one column.
- **`MobHandle::reseed` replaces, it does not merge.** A mob spawned before the
  first reseed would vanish. Correct for the one caller (a `Default` handle has
  no population to lose) and *not* a general "load more terrain" primitive —
  widening the snapshot as the player walks needs a sim that can extend its
  world.
- **Seeding is a third task, not a prologue to the tick loop.** Putting an
  `.await` in front of `run_tick_loop` delays its first `Instant::now()`, which
  both re-introduces the stall and breaks `integrated_memory.rs`'s paused-clock
  gate ("5 tick periods must produce exactly 5 ticks" cannot hold if the loop has
  not started). `shutdown()` **aborts** the seed task rather than joining it, for
  the same reason.
- **Any generation-count gate must build a fresh source per arm.** The real
  generator's 512-entry memo makes a count measured above it vacuous — the same
  trap `chunk.rs`'s determinism test already documents. The gates below use a
  hand-written counting source for exactly this reason.

## The gates

Counts, not durations — durations here showed a 2.3× spread from machine load
alone on an identical release binary while counts stayed byte-identical.

| gate | asserts | observed pre-fix |
|---|---|---|
| `integrated.rs`: `world_open_generates_no_columns_at_all` | the constructor generates **0** columns | **49** |
| `integrated.rs`: `seeding_generates_each_tick_area_column_exactly_once` | every tick-area column generated exactly once | 2 each |
| `serve_play.rs`: `join_streams_the_view_outward_from_the_players_own_column` | `(0, 0)` encoded first; ≤1 column generated when it was; non-decreasing Chebyshev distance | first column `(-9, -9)` |

Each has a control that was **run and observed to fail**, not described:

- `control_the_old_synchronous_seeding_generates_the_whole_mob_area` — drives the
  surviving `MobHandle::seeded` over the same counting source and reads 49, which
  is what rules out a counter that silently counts nothing.
- `control_two_independent_sources_generate_the_tick_area_twice` — reproduces the
  pre-fix arrangement (a `ChunkStore` plus a separate `world_source`) and reads
  **98**, every coordinate at 2.
- `control_the_old_raster_order_fails_the_proximity_assertion` — feeds the literal
  pre-fix raster walk to the same detector and requires an `Err`, checking that
  the first-column rule and the distance rule each fire *independently* so
  relaxing one cannot quietly disarm the other.

Both fixes were additionally verified by temporarily neutering the real code and
watching the real gate fail: `got 49` for #454, and
`the player's own column must be encoded first; got (-9, -9)` for #453.

## Configuration

- `view_radius` — the shell's singleplayer value is 9 (361 columns). Also caps
  what a client's `ClientInformationChanged` can request.
- `mob_radius` — `view_radius.clamp(1, 3)` in `net.rs`; 49 columns at the
  default.
- `ChunkStore`'s `DEFAULT_CAPACITY` is 512, comfortably above 361, so the join
  view and the tick area are co-resident and seeding never re-generates what the
  connection already fetched.

## Dependencies

- [`chunk-store.md`](./chunk-store.md) — the retention layer both paths now share.
- [`server-chunk-generation-parallelism.md`](./server-chunk-generation-parallelism.md)
  — `generate_columns_parallel` / `generate_columns_offloaded`, which the seeding
  task and the ring loop both go through.
- [`server-tick-loop.md`](./server-tick-loop.md) — the tick task seeding must not
  delay.
- [`plans/chunk-lifecycle.md`](./plans/chunk-lifecycle.md) — issue #289; the ring
  ordering is a slice of its U4/U5, and vanilla's priority *is* the ticket level
  (`ChunkTaskDispatcher.java:62-69`), so there is no separate heuristic here.
