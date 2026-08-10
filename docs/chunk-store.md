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
value, so a store read is a deep copy — **3.1 µs**, measured, against the 909 ms
it replaces. Since issue #551 it copies ~24 KiB of packed sections rather than a
flat 192 KiB, so the trade got cheaper by the same factor residency did; it is now
`sections.len()` separate `Vec` allocations rather than one, which is the term to
watch if it ever shows up in a profile. To make reads a refcount bump instead, the
trait signature has to change — the remaining half of the plan's U8.

## Configuration

There are **two** capacity policies, and which one a constructor picks is a
question about whose memory is being spent.

| path | constructor | policy |
|---|---|---|
| singleplayer (`open_in_memory`, `open_in_memory_with_mobs`) | `ChunkStore::for_integrated_view_radius` | `integrated_capacity_for_view_radius` — **no ceiling** |
| open-to-LAN (`IntegratedServer::bind`) | `ChunkStore::for_view_radius` | `capacity_for_view_radius` — capped at `MAX_CAPACITY` |

Both are

```
view_columns(view_radius) + CONCURRENT_SCAN_COLUMNS      floored at DEFAULT_CAPACITY
```

with `view_columns(r) = (2r + 1)²`, `CONCURRENT_SCAN_COLUMNS = 50` and
`DEFAULT_CAPACITY = 512`; the hosted one additionally clamps to
`MAX_CAPACITY = 1275`. Each term has its own doc comment carrying its own
argument; the short version is below.

**Why singleplayer is uncapped.** The render distance is the player's own choice
about their own machine, and capping the *cache* under a view they are already
being streamed buys them nothing — the columns are generated and meshed either
way. What a short capacity costs is re-generation of the ground they are standing
on, because `join_view_rings` streams outward and the least-recently-used entry
is therefore the **innermost** ring. (At `render_distance` 10 the old 512-column
literal dropped 17 columns and they were rings 0–2, the band `vitals_tick` probes
every 50 ms.) A hosted server is the opposite case: the memory is an operator's,
spent on behalf of players who did not choose the setting, so `bind` keeps the
ceiling.

**The price of the uncapped path**, at the measured **31.1 KiB** per retained
column (it was 195.5 KiB before issue #551 packed the block grid per section —
see [`chunk-column-storage.md`](./chunk-column-storage.md)):

| `render_distance` | view columns | capacity | resident | pre-#551 |
|---|---|---|---|---|
| 8 (our default) | 361 | 512 (floor) | 15.5 MiB, measured | 97.4 MiB |
| 12 (vanilla default) | 729 | 779 | 23.7 MiB | 148 MiB |
| 16 | 1,225 | 1,275 | 38.9 MiB, measured | 242.0 MiB |
| 24 | 2,601 | 2,651 | 80.5 MiB | 506 MiB |
| **32 (slider max)** | **4,489** | **4,539** | **139.2 MiB, measured** | 867 MiB |

The rate is flat at 31.1–31.4 KiB per column across an **8.9×** range. The last
row used to be a 3.6× extrapolation and is now measured directly
(`measure_rss_at_the_singleplayer_slider_maximum`) — an arm that only became
affordable to run *because* of #551, and which landed within 1% of the
extrapolation it replaced.

**This table is why #551 was worth doing, and the change is in the argument as
much as in the numbers.** The uncapped singleplayer policy used to be a real
trade — 867 MiB is a genuine cost to a client process that also holds meshes,
textures and a GPU allocator — and "it is the user's call" was doing load-bearing
work. At 139 MiB it is barely a trade. The per-column *rate*, not the policy, was
the thing worth fixing.

Measured cost, by `/usr/bin/time -l` on the release lib-test binary, before and
after:

| arm | pre-#551 peak RSS | post-#551 peak RSS |
|---|---|---|
| 512 columns retained | 105.4 MiB | 24.0 MiB |
| the same 512 touched, retention off | 7.8 / 8.1 MiB | 8.4 MiB |
| **delta** | **97.6 MiB** = 195.5 KiB/column | **15.5 MiB** = **31.1 KiB/column** |

Of the 31.1 KiB, `ChunkColumn::blocks_heap_bytes` is ~24 KiB; the rest is the
palette `String`s, the 3-D biome grid (~3 KiB — now the *second* largest term and
the next thing to look at) and the map entry.

**The two arms are each other's control** — a delta near zero would mean the
columns were dropped in both arms, or that the pages were never faulted in, and
the run would be a failure to measure rather than evidence that residency is free.

**`touched_column`'s premise was falsified by #551, and that is worth reading.**
It wrote one cell per 8 y-rows — exactly right against a flat `vec![0u16; 98304]`,
which `alloc_zeroed` can serve from pages the process never faults in, so 48
scattered writes faulted every page of a contiguous 192 KiB allocation and the
column's cost was independent of its content. #551 made cost a *function* of
content, and the old fixture then packed to ~12 KiB a column: it would have
reported a saving no real column gets, while still running, still faulting pages
and still producing a plausible delta. That is CLAUDE.md's **world** species of
vacuity — the flaw is in the input data, not in any assertion. The fixture is now
terrain-shaped and *calibrated* against four real generated columns (mean 24,112
packed bytes). The two tables above remain directly comparable because the old
representation's cost did not depend on content, so the 195.5 KiB row is valid for
the new fixture too.

Lowering the capacity to 128 (~4 MiB) **still fixes the originally reported bug
completely**: the starvation fix needs only the 49-column `tick_area` resident.

### Why the capacity is a function of the view radius (issue #505)

512 was chosen to also cover the *default* streamed view — but as a bare literal,
in a different file from the render distance it was chosen for. The shell serves
`view_radius = render_distance + 1` (`app/session.rs`; the `+ 1` is vanilla's
`ChunkTrackingView` buffer ring and is correct), so the streamed square is
`(2·(rd+1) + 1)²`:

| `render_distance` | `view_radius` | view columns | vs the old 512 |
|---|---|---|---|
| 8 (our default) | 9 | 361 | fits |
| 9 | 10 | 441 | fits |
| **10** | 11 | **529** | **over** |
| **12** (vanilla's default) | 13 | **729** | **over** |
| 32 (slider max) | 33 | 4489 | over |

One notch of the slider past 9 and the literal was under the set it existed to
hold. The three terms of the replacement:

- **the view term** is the bug;
- **`CONCURRENT_SCAN_COLUMNS` (50)** is added *on top of* the view, not assumed
  inside it, plus the one column `vitals_tick` probes. **The original reason is now
  historical**: `run_tick_loop`'s 49-column tick area used to be centred on world
  spawn and never move, so once the player walked away it was 49 columns outside the
  view still touched at 20 Hz. It follows the players now
  (`docs/ticked-area-follows-the-player.md`), so in the steady state it is a subset
  of the view; the reserve stays for the transients — the area moves the tick a
  movement packet lands, before the new strip has streamed, and a teleport or the
  playerless fallback square puts it outside the view again. And
  frequency is not residency — #504 measured a 20 Hz-polled column being
  regenerated **12** times over 12 random-tick passes, because the pass touches
  49 columns *after* the poll and leaves the polled column holding the oldest
  stamp in the map;
- **the `DEFAULT_CAPACITY` floor** means the derivation can only move capacity
  *up*, so the default configuration is byte-for-byte the 512-column store every
  measurement in this doc was taken against;
- **the `MAX_CAPACITY` ceiling** stops the CPU cliff being traded for a memory
  one — `render_distance` 32 sizes the store at 4,539 columns, which was ≈867 MiB
  and is now 139.2 MiB measured. It applies to the **hosted** path only; see the fork at the
  top of this section. **A reader asking "why 17 and not 33?" should know the
  answer is now history, not 867 MiB.**

The cap's own cost was measured rather than extrapolated, since a `HashMap`
growing through several rehash thresholds with large values could plausibly have
been superlinear:

| arm | peak RSS | delta | per column | pre-#551 per column |
|---|---|---|---|---|
| retention off (shared control) | 8.4 MiB | — | — | — |
| 512 retained | 24.0 MiB | 15.5 MiB | 31.1 KiB | 194.8 KiB |
| **1,275 retained (`MAX_CAPACITY`)** | **47.3 MiB** | **38.9 MiB** | **31.2 KiB** | 194.4 KiB |

It is not: the rate is flat across a 2.5× range, so the interpolated rows in
`FULLY_RESIDENT_VIEW_RADIUS`'s table are safe to read.

**What degrades above the cap, precisely.** The store holds 1,275 of the view's
columns and no more, so a column outside that set costs a regeneration when
something asks for it again — a `block_state` probe from redstone, a fluid tick,
mob pathing, or the same column re-entering the view. It is **not** a per-access
cost on the whole view: `ViewTracker::recenter` *diffs* the window as the player
moves and only ever asks for columns that newly entered, so the view is streamed
once and incrementally extended, never rescanned. Nor does it touch the 20 Hz
scans, which `CONCURRENT_SCAN_COLUMNS` covers at every radius. The degradation is
bounded and localised; `tests/view_radius_store_capacity.rs` measures it as this
subsystem's permanent negative control.

To raise the cap, raise `FULLY_RESIDENT_VIEW_RADIUS`, re-run the RSS pair, and
put the new numbers in its table. Reducing the cost per column instead is unit U8
of [`plans/chunk-lifecycle.md`](./plans/chunk-lifecycle.md), and the storage half
of it is **done** — see [`chunk-column-storage.md`](./chunk-column-storage.md).

### The capacity follows a live radius change (issue #551)

Capacity used to be fixed at construction from the radius the connection
*joined* with, as a plain `usize` behind the `Arc`. Since `0c09f576` a client can
raise its render distance mid-session and the server honours it — so the streamed
view then exceeded the cache bound, and **the LRU victim under a short capacity is
the innermost ring**, because `join_view_rings` streams outward and leaves ring 0
holding the oldest stamp. Raising render distance therefore worked while quietly
regenerating the ground under the player's feet at ~909 ms a column.

The capacity now lives **inside the cache mutex**, and `ChunkSource` carries a
`set_retention_radius(view_radius)` hint (default: a no-op, exactly like
`unload`) that `ViewTracker::set_view_radius` calls after its clamp and *before*
streaming the new view — so `build_batch` never evicts a column it is about to
need. The store remembers which of the two capacity policies built it, in a
private `CapacityPolicy`, because "whose memory is this" does not change when the
slider moves.

**It grows only: capacity follows the session's high-water mark.** A lowering is
recorded as nothing, and that is deliberate rather than lazy. A shrinking policy
would evict 217 of the 729 columns at `view_radius` 13 — and by ring order those
217 are the innermost ones — so
`regrowing_the_render_distance_regenerates_nothing_at_vanillas_default`'s `== 0`
would become a non-zero, and rightly: nudging the slider down and back up would
cost a regeneration of the ground you are standing on. The gate is the argument
against a shrinking policy, not an obstacle to one. The memory it would reclaim is
also now small (139 MiB at the extreme, and only for a session that *did* ask for
`render_distance` 32). If it ever needs to change, the thing to add is a shrink
that refuses to drop a column *inside the current view* — not a plain
`evict_down_to`, which drops exactly the wrong ones.

Gated by `raising_the_render_distance_mid_session_resizes_the_store`
(`tests/view_radius_store_capacity.rs`): join at radius 9, raise to 15, then probe
residency with a down-and-up sweep, asserting **0** regenerations against a
computed floor of 449. The raise *alone* cannot see the bug — `set_view_radius`
diffs the window, so it asks for each newly-visible column exactly once and even a
hopelessly short store reports no repeats; the harm is paid by whatever asks
again. Confirmed by neutering `set_retention_radius` in place: the gate fails,
naming a twice-generated chunk.

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
- `measure_rss_with_retention` / `measure_rss_at_the_capacity_cap` /
  `measure_rss_without_retention` / `measure_real_column_generation_cost` —
  `#[ignore]`d measurement tools, not assertions. Release profile only.

In `crates/lodestone-server/tests/view_radius_store_capacity.rs`, its own binary
because it is a counter gate and because it must go through the *public* API to
reach the real streaming path:

- `regrowing_the_render_distance_regenerates_nothing_at_vanillas_default` — the
  subject. Joins at `view_radius = 13` (`render_distance` 12) through
  `IntegratedServer::open_in_memory`, drags the render-distance slider down to 0
  and back up over the wire (`ServerBound::ClientInformationChanged`, the real
  packet), and requires **zero** columns to be generated twice. The view size is
  *measured* from the chunk packets `serve_connection` emits, not restated, so the
  gate is a join between the policy and production rather than
  `decode(encode(x)) == x`.
- `past_the_hosted_capacity_cap_the_view_cannot_stay_resident` — the permanent
  negative control, as a real configuration of the shipped policy
  (`view_radius = 20`, past `FULLY_RESIDENT_VIEW_RADIUS`) rather than a temporary
  neuter. Asserts a *computed* floor, `view_columns(20) − MAX_CAPACITY = 406`.
  **Drives `IntegratedServer::bind` over a loopback socket, not
  `open_in_memory`**, because `bind` is now the only constructor that still caps —
  through the in-memory rig this arm would report 0 and look like a working cap
  while measuring its absence.
- `the_default_render_distance_is_under_the_old_ceiling_on_both_arms` — why the
  subject is not at our own default. 361 < 512, so a gate at `render_distance` 8
  would have passed before *and* after the fix: the **world** species of vacuity,
  kept as a test so it cannot quietly become false.
- `the_regeneration_curve_across_the_render_distance_slider` — the curve, with
  each row asserting the regime its capacity puts it in.

Observed on the unfixed wiring (`ChunkStore::new`, literal 512) in an isolated
detached worktree at `c77146d9`, against the same arms after the fix:

| `view_radius` | `rd` | view | old cap | regenerated, unfixed | new cap | regenerated, fixed |
|---|---|---|---|---|---|---|
| 9 | 8 | 361 | 512 | 0 | 512 | 0 |
| 11 | 10 | 529 | 512 | **92** | 579 | 0 |
| 13 | 12 | 729 | 512 | **451** | 779 | 0 |
| 20 | 19 | 1681 | 512 | 1595 | 1275 | 1077 (the cap's own degradation) |

The last row is the **hosted** policy; singleplayer at `view_radius = 20` now sizes
at 1,731 and regenerates 0, which is what `the_regeneration_curve_across_the_render_distance_slider`
asserts for that row.

The unfixed figures move a few percent between runs and the arithmetic floors do
not, because `generate_columns_offloaded` fans the re-grow out over the blocking
pool and scheduling decides which entry a given miss evicts. **That is why the
subject asserts 0 and the control asserts a computed floor — neither asserts an
observed number.**

**The trap every count gate here avoids:** `OverworldGenerator` carries a
per-instance 512-entry memo cache, so a generation-count gate built on
`overworld_chunk_source` passes *even with a completely broken store*. Every
count above is taken on a hand-written `CountingSource` with no cache of any
kind. The same vacuity was found and fixed once already in `chunk.rs`'s
`parallel_generation_is_deterministic_and_matches_serial`.

## Known remaining gaps (not fixed by this)

- **The initial join burst is still slow.** The store removes *re*-generation,
  not first generation. A `view_radius` 9 join is 361 first-time columns, and at
  `render_distance` 12 it is 729. The **ordering** half of this has since been
  fixed and this bullet used to describe the pre-fix state: issue #453 replaced
  the raster walk from the `(-9, -9)` corner with `join_view_rings`, and Unit 10's
  `join_scheduler` streams each column as it finishes rather than generating the
  whole set first, so the player's own column is item 1 on the wire instead of
  ~180 of 361 (`tests/serve_play.rs`'s `check_proximity_stream`). The remaining
  gap is the total first-generation cost, not the order.
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
