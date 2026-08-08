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

### The join no longer stands in front of the play loop

*"For the integrated server, I don't think it should be generating all chunks on
join before doing anything else. For example I can't break blocks, take damage,
etc. until it finishes."* — and that was structural. Everything above is about the
*order* of the burst; none of it moved the burst out of the way. All `(2r + 1)²`
columns (1,089 at `view_radius = 16`) were generated **and encoded** inline in
`serve_connection_inner`'s `ConfigurationFinished` arm, before control ever reached
`serve_play`, the loop that dispatches play packets. So every interaction queued
behind the whole initial generation burst.

The split now is:

| | where | how much |
|---|---|---|
| pre-stream | inline, before `serve_play` | rings `0..=JOIN_PRESTREAM_RADIUS` — **9 columns** |
| the rest | a `select!` branch *inside* `serve_play` | everything else, in `JOIN_STREAM_BATCH_COLUMNS`-sized batches |

That is vanilla's shape: `PlayerList.placeNewPlayer` adds the player to the level
and `PlayerChunkSender` feeds chunks over subsequent ticks. Nine columns rather
than vanilla's one because this crate has already paid for a spawn-safety bug
once (player spawns above terrain, falls, reaches zero health with no death
screen), so the column the player stands on *and* the eight they can step onto are
on the wire before anything they do can matter.

The carrier is `join_scheduler::JoinChunkStream`, one variant per `SourceRef` arm,
and `serve_play`'s branch is disabled on `is_done()` so a drained stream is not a
branch that returns `None` forever. Both arms' `next` had to become
**cancel-safe**, because a `select!` drops the losing branch's future: the windowed
arm now awaits its front `JoinHandle` *by reference* and pops only once the column
is in hand. Popping first — fine while it was only ever driven to completion —
would silently lose a column from the wire on every cancellation.

**One batch became many, deliberately.** A single `begin`/…/`end` pair cannot span
a stream that outlives the join without wrapping everything else the play loop
sends, so the deferred half is batched — which is vanilla's own shape, and what
`ChunkBatchSizeCalculator` exists to pace (our client answers each
`chunk_batch_finished` with its desired rate: `ChunkBatchState` in
`crates/protocol/v770/src/adapter.rs`). The deferred batches are **not** gated on
`awaiting_chunk_batch_ack`: that gate exists so a *reactive*, unbounded stream
cannot outrun a client, whereas the join is a finite set the client is already
owed, and gating it would make delivering the world depend on a reply that no
`ServerProtocol` fixture in this crate sends — a hang rather than a mismatch.

### Generating where the player is looking, and re-sorting when they move

*"For chunkgen we need it to be smarter — it should generate chunks first where
the user is looking, and if the player moves it should properly generate the
closer chunks first."*

Two blockers had to go first: nothing threaded the player's yaw into view
tracking, and `ColumnPipeline` walked a *fixed* list, so "the player moved,
re-sort" had nowhere to happen. Both are gone — the pipeline now drains a
`join_scheduler::ColumnQueue`, and `serve_play` re-keys it from the pose each
inbound packet delivers.

The key is `(Chebyshev distance, in-frustum penalty, ring-walk index)`:

- **distance is primary, and that is the anti-starvation property.** A column at
  distance `d` *behind* the player still precedes every column at distance
  `d + 1`, in view or not. Pure frustum-first would let a slowly spinning player
  starve what is behind them and then show a hole when they turn round; vanilla is
  deliberately distance-based for the same reason.
- **the frustum bonus reorders within a ring only** — a 120° cone (generous
  against vanilla's ~106° horizontal FOV, so a column about to rotate into view is
  already generated), with rings 0 and 1 always counted as in view.
- **the tie-break is the ring-walk index**, which is why *this changed no existing
  wire order*: with no rotation known — the state at join, and the state of every
  ordering gate in this crate — the key reduces to the ring walk exactly.

Re-prioritisation is called on every inbound packet and must therefore be cheap:
it re-sorts only when the player's centre chunk changes or their yaw crosses one
of 16 quantised sectors, and a sort of ≤ 1,089 integer keys is microseconds. The
in-flight columns are deliberately not re-ordered — there are at most
`generation_window()` of them and they were the best choice when they were
spawned — so the granularity of a re-prioritisation is one window, which is also
what keeps the emitted order a function of the queue rather than of which worker
finished first.

`ViewTracker::build_batch` — the move-time counterpart — now orders on the same
key rather than its old lexicographic `sort_unstable`, so walking into new terrain
fills nearest-first exactly like joining does.

**Content determinism is untouched by all of this**: a column's content is a
function of its coordinates and the seed, never of generation order.

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
- **`ViewTracker::build_batch` orders by distance now, and the old rule here said
  not to.** It read *"do not re-sort `build_batch`; its lexicographic
  `sort_unstable` is there for byte-reproducibility"* — which conflated
  *reproducible* with *lexicographic*. What byte-reproducibility needs is a **total
  order that is a function of the pose**, and `view_order_key` is one; the
  lexicographic sort merely happened to be the first such order anyone wrote. If
  you change this, keep that property: never order by anything a `HashSet`
  iteration or a completion time can influence.
- **A join's batch count is an implementation detail; its column count is not.**
  Any test that reads a fixed number of `CHUNK` packets and then expects the
  marker is asserting the pre-split shape. Drain until the columns are accounted
  for instead (`drain_join_view` in `serve_play.rs` and
  `view_radius_store_capacity.rs`, `collect_join_chunks` for the counter-carrying
  probe protocol).
- **Do not gate the deferred join stream on `ChunkBatchAcknowledged`.** It looks
  like the missing half of issue #270 and it is not — see above. The failure mode
  is a 30-second test timeout, the least diagnosable shape available.
- **`JoinChunkStream::next` and `ColumnPipeline::next` are `select!` branches, so
  cancel safety is a correctness requirement, not a nicety.** Anything that
  removes work from the stream before that work has been *emitted* loses a column
  silently: the client is short one chunk, no gate in this crate counts packets
  per batch on the production path, and the hole appears in the world.
- **Order within a ring must stay a pure function of `view_radius`.** It is the
  same `dz`-outer/`dx`-inner walk, filtered — which is what keeps the emitted
  byte sequence independent of thread scheduling and of which `SourceRef` arm
  generated it.
- **The inner rings under-use the fan-out.** Rings 0 and 1 are smaller than
  `available_parallelism`, so total generation costs slightly more than one
  361-column batch. Quantified at 16 workers: one batch is `ceil(361/16)` = **23**
  serial column-times, ring-by-ring is `sum(ceil(8r/16))` = **26** — three extra,
  about **0.67 s** at 222 ms. That is the deliberate trade, and it is the right
  one: time-to-first-chunk falls from 23 column-times (~5 s) to **one** (~0.22 s).
- **A negative `view_radius` must yield no rings, not ring 0.** This one bit
  already, in the first draft: `(0..=view_radius.max(0))` reads as a harmless
  clamp and is a behaviour change, because the raster walk it replaced built
  `(-r..=r)` — an *empty* range for `r < 0` — so a negative radius sent zero
  chunks. Clamping sends one, while `ViewTracker::new` still records an empty
  loaded set for the same input, so the tracker and the wire disagree about a
  column the client actually has. Nothing produces a negative radius today
  (`dispatch_play_packet` clamps with `view_radius.max(0)` precisely as an
  invariant against it), which is exactly why it went unnoticed and why it now has
  a test rather than a reading. **The general shape: an expression can be
  accidentally correct about an input its author never considered, and "tidying"
  it is how that gets lost.**
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
| `serve_play.rs`: `a_play_packet_is_serviced_before_the_last_join_chunk` | a difficulty change sent immediately after configuration is answered after **< 40** of 361 join chunks (observed ~9), and the view still arrives whole and in order | **361** — the reply cannot precede the last chunk, because the loop that produces it has not started |
| `serve_play.rs`: `a_move_streams_the_new_columns_nearest_first` | a diagonal jump's added columns arrive non-decreasing in distance from the *new* centre, nearest first | first column `(4, -1)` at distance 3 |
| `join_scheduler.rs`: `distance_is_the_primary_key_so_a_spinning_player_cannot_starve_a_ring` | a near column behind the player beats a far one in front | — (unit) |
| `join_scheduler.rs`: `the_facing_cone_orders_within_a_ring` | the in-frustum half of a ring is its prefix — the control for the row above, which would otherwise pass on an ordering that ignores rotation entirely | — (unit) |
| `join_scheduler.rs`: `an_unknown_facing_emits_the_ring_order_unchanged` | with no rotation, the priority queue *is* the ring walk — what lets the two wire-order gates keep asserting a fixed sequence | — (unit) |
| `server.rs`: `join_view_rings_partitions_the_square_exactly` | ring sizes `1, 8, …, 8r` summing to `(2r+1)²`, no column on two rings | — (unit) |
| `server.rs`: `join_view_rings_at_radius_zero_is_a_single_column`, `…_at_a_negative_radius_is_empty` | the two edge radii | — (unit) |

The two unit tests are not redundant with the end-to-end gate: the end-to-end one
only ever runs radius 9, and a ring walk that double-counted a corner or skipped
an edge would still be non-decreasing in distance, so no ordering assertion can
see it. The end-to-end gate does check set equality against the old square, which
covers radius 9; the unit tests cover the edges and the ring-size arithmetic.

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
