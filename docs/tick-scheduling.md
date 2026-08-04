# Random ticks, scheduled ticks, and neighbour-update propagation

Issues [#307](https://github.com/matteopolak/lodestone/issues/307) (random-tick
scheduler) and [#308](https://github.com/matteopolak/lodestone/issues/308)
(scheduled-tick queue and neighbour-update propagation) — the foundation layer
every block-tick feature (crop/sapling growth #310, fluid flow #309, fire
spread #312, gravity blocks #311, leaf decay, the redstone family #314-#322)
builds on.

## What it is

Three new modules in `crates/lodestone-server/src/`, each a generic,
vanilla-shaped primitive with its own test suite, wired into
`tick::run_tick_loop` (issue #284):

- [`random_tick.rs`](../crates/lodestone-server/src/random_tick.rs) — issue
  #307. [`RandomTickScheduler`] replicates `ServerLevel::tickChunk`'s block
  selection (`ServerLevel.java:495-538`) and the position-pick LCG
  (`Level.getBlockRandomPos`, `Level.java:1064-1068`) exactly, and drives the
  one block modeled end to end today: grass turning to dirt when covered, and
  dirt turning to grass when adjacent to an air-exposed grass block
  (`SpreadingSnowyBlock.randomTick`, `SpreadingSnowyBlock.java:44-64`).
- [`scheduled_tick.rs`](../crates/lodestone-server/src/scheduled_tick.rs) —
  issue #308, first half. [`ScheduledTickQueue<T>`] mirrors vanilla's
  `LevelTicks`/`LevelChunkTicks`/`ScheduledTick` drain order (trigger tick,
  then priority, then insertion order).
- [`neighbor_update.rs`](../crates/lodestone-server/src/neighbor_update.rs) —
  issue #308, second half. [`NeighborPropagator`] mirrors
  `NeighborUpdater.UPDATE_ORDER` and `CollectingNeighborUpdater`'s
  depth-first cascade semantics — the "vanilla update-order quirks" issue
  #316 refers to.

## How it works

### Random ticks (#307)

The real selection, cited directly from the decompiled 26.2 jar
(`.cache/mc/26.2/src/`):

- **How many picks, and where.** `ServerChunkCache.java:377,403`:
  `int tickSpeed = this.level.getGameRules().get(GameRules.RANDOM_TICK_SPEED);`
  then `this.chunkMap.forEachBlockTickingChunk(chunkx -> this.level.tickChunk(chunkx, tickSpeed))`.
  `RANDOM_TICK_SPEED`'s default is `3` (`GameRules.java:74`) —
  [`DEFAULT_RANDOM_TICK_SPEED`].
- **Per-section loop.** `ServerLevel::tickChunk` (`ServerLevel.java:508-535`):
  for every 16-block section with `section.isRandomlyTicking()` true
  (`tickingBlockCount > 0`, `LevelChunkSection.java:110-118`), draw
  `tickSpeed` positions, **unconditionally** — a miss still consumes a draw.
- **Two independent generators.** `getBlockRandomPos`
  (`Level.java:1064-1068`) advances `this.randValue`, a level-local 32-bit LCG
  — **not** `this.random`, which is what a block's own `randomTick` draws
  from. [`next_random_tick_pos`] is the LCG, bit-for-bit; [`RandomTickScheduler`]
  keeps a second, independent generator (`SpawnRng`, this crate's existing
  deterministic RNG from `mob_spawn.rs`) for behaviour draws.
- **Section eligibility.** `LevelChunkSection::isRandomlyTicking` is an
  incrementally maintained counter this crate has no equivalent field for
  (`ChunkColumn` has no per-section bookkeeping — see `chunk.rs`'s own module
  doc). [`RandomTickScheduler::tick_chunk`] computes the same boolean by
  scanning the section instead — the identical true/false answer, just
  computed differently.
- **Grass ↔ dirt**, cited from `SpreadingSnowyBlock.randomTick`
  (`SpreadingSnowyBlock.java:44-64`): not `canStayAlive` (block above is a
  full fluid, or fully light-dampened) → convert to dirt, zero further draws.
  `canStayAlive` **and** `getMaxLocalRawBrightness(pos.above()) >= 9` → four
  spread attempts, three `nextInt` draws each (offset `x/y/z`), regardless of
  hits. This crate has no light engine, so [`grass_random_tick`] uses **"the
  block directly above is bare air"** as the proxy for both checks — a named
  simplification (see [`is_air_variant`]'s doc comment), not a guess: the
  **draw pattern** (0 draws dead branch, exactly 12 live branch) is exact
  either way, which is what every test in `random_tick.rs` actually asserts.

### Scheduled ticks (#308)

`ServerLevel` keeps two `LevelTicks`, block before fluid
(`ServerLevel.java:209-210,388-391`):

```text
this.blockTicks.tick(tick, 65536, this::tickBlock);
this.fluidTicks.tick(tick, 65536, this::tickFluid);
```

`65536` is `MAX_SCHEDULED_TICKS_PER_TICK` (`ServerLevel.java:194`).
[`ScheduledTickQueue<T>`] is the single-container reduction of vanilla's
per-chunk `LevelTicks`/`LevelChunkTicks` split (this crate has no per-chunk
tick-container registry yet — see the module's own doc comment for why that
reduction is faithful, not invented). `drain_due(current_tick, max)`:

1. Drains every entry with `trigger_tick <= current_tick`, in
   `(trigger_tick, priority, sub_tick_order)` order — `TickPriority`'s seven
   variants are declared in the same order as vanilla's `-3..3` enum so
   Rust's derived `Ord` matches Java's `Enum::compareTo` for free.
2. Returns the whole `Vec` **before** the caller runs any callback — a tick
   scheduled while processing entry `N` cannot appear in entries `N+1..`
   of this same `Vec`, mirroring `LevelTicks::tick`'s own collect-then-run
   split.
3. A second `schedule` for a `(pos, kind)` pair already pending is a silent
   no-op — vanilla's own `ticksPerPosition` dedup
   (`LevelChunkTicks.java:53-57`).

`tick::run_tick_loop` drains `block_ticks` then `fluid_ticks`, every world
tick, in that order — **nothing schedules into either queue yet**. Stated
plainly: this half of #308 is a tested, correctly-ordered island until a
block behaviour (fluid flow #309, gravity blocks #311, redstone #314-#322)
calls `schedule`. It is not a *silent* island — `run_tick_loop`'s own doc
comment says so, and the queues are drained (proving the order holds) every
tick regardless of whether anything is in them.

### Neighbour-update propagation (#308)

`NeighborUpdater.UPDATE_ORDER` (`NeighborUpdater.java:18`): **west, east,
down, up, north, south** — [`UPDATE_ORDER`], verbatim. The propagation shape
is vanilla's `CollectingNeighborUpdater` (`CollectingNeighborUpdater.java`):
depth-first, not breadth-first. Notifying `west` and having that block's own
state change cascade into further notifications means every one of those
(and anything *they* cascade into) resolves **before `east` is ever
notified**. [`NeighborPropagator::propagate`] is that same algorithm as an
explicit stack, capped by `max_chained` (vanilla's
`maxChainedNeighborUpdates`).

No block in this crate has a real `neighborChanged` response yet (that is
#311/#314-#322's job) — `NeighborPropagator` is tested standalone today, with
no production caller. This is the one piece of this landing that is a
genuine island, named as such rather than hidden: the algorithm and its
ordering are correct and pinned by tests that reject the plausible-looking
wrong hypothesis (breadth-first), but nothing calls it in `tick.rs` yet.

## What actually reaches a client today

Per this repo's own rule ("nothing is done until something on screen
changes"), here is exactly what does and does not:

- **Random ticks are real, not simulated on a throwaway copy.**
  `IntegratedServer::open_in_memory_with_mobs` now wraps its `source`
  (the `ChunkSource` the connection actually serves chunks from) in an `Arc`
  and shares **the same instance** with the tick loop — not the separate,
  intentionally-unshared `world_source` mob pathing uses (see that
  constructor's own doc comment for why those two stay distinct). Every
  grass ↔ dirt conversion calls `ChunkSource::set_block` on that shared
  instance and publishes the change through [`BlockTickFeed`], which
  `serve_play`'s existing `container_sync_tick` timer (the same one that
  already forwards block-entity registry changes with no packet driving
  them) drains and forwards as `encode_block_update` packets.
- **The scheduled-tick queues and the neighbour-update propagator have no
  production producer yet.** Stated in `tick.rs`'s own doc comment, not
  hidden: the queues drain (proving order) every tick; the propagator has no
  call site in this crate at all.
- **`tick_area` is a small, fixed chunk range** — the same
  `(cx_range, cz_range)` `open_in_memory_with_mobs` already threads through
  as `mob_area`, reused rather than adding a second "which chunks are
  loaded" concept (this crate has none — see `chunk.rs`'s own module doc).
  Every chunk in it is re-fetched via `ChunkSource::column` **every tick**;
  for an unedited column this re-runs the generator, a real, documented
  performance gap for anything wider than a handful of chunks.

## How to change it, and the gotchas

- **Adding a new randomly-ticking block**: extend [`is_randomly_ticking`]
  and add a branch to `RandomTickScheduler::tick_grass_block`'s sibling (or
  generalize it into a per-block dispatch once a second block needs one —
  today it is grass-specific on purpose, since generalizing before a second
  real case existed would be guessing the shape).
- **Adding a real scheduled-tick producer**: call
  `ScheduledTickQueue::schedule` from wherever a block decides "run again in
  N ticks" (vanilla's own `level.scheduleTick`). The queue does not care what
  `T` is — this crate keys it by canonical block-state-name `String` to match
  `ChunkColumn`'s own representation, not by a `Block`/`Fluid` registry
  object like vanilla, since this crate has no such registry.
- **Adding a real neighbour-update producer**: call
  `NeighborPropagator::propagate` with a `notify` closure that mutates the
  world and returns any further single-target notifications that mutation
  itself triggers — do **not** call `propagate` recursively from inside
  `notify`; the propagator's own stack already handles cascading, and a
  second nested call would double-count `max_chained`.
- **Widening `tick_area` beyond a handful of chunks**: add a real per-tick
  column cache first (`OverworldChunkSource`'s `edits` map only helps
  already-edited columns); this landing deliberately did not build one,
  since #307/#308's job is the scheduler, not chunk-loading infrastructure.
- **If you add a light engine**: replace `is_air_variant`'s role in
  `grass_random_tick`/`can_propagate_onto` with a real brightness check —
  the draw pattern (12 draws in the live branch) does not change, only which
  positions qualify.

## Configuration

- [`DEFAULT_RANDOM_TICK_SPEED`] (`3`) — vanilla's `random_tick_speed`
  gamerule default (`GameRules.java:74`). This crate has no gamerule
  registry (`server.rs`'s own module doc explains why `GameRuleChanged` is
  currently echoed, not applied), so `run_tick_loop` passes this constant
  directly rather than reading a rule.
- `tick::RANDOM_TICK_POSITION_SEED` / `RANDOM_TICK_BEHAVIOR_SEED` — fixed
  literals seeding [`RandomTickScheduler`]'s two generators. Vanilla seeds its
  position LCG from an arbitrary thread-local draw at level creation; this
  crate has no per-world seed store to draw a "real" one from yet.
- `tick::MAX_SCHEDULED_TICKS_PER_TICK` (`65536`) — vanilla's
  `ServerLevel.MAX_SCHEDULED_TICKS_PER_TICK` (`ServerLevel.java:194`).
- `NeighborPropagator::max_chained` — per-call cap, `None` for unbounded
  (test-only; a live world should always cap it).

## Dependencies

- `crate::chunk::{ChunkColumn, ChunkSource}` — random ticks read and mutate
  through this seam.
- `crate::mob_spawn::SpawnRng` — reused as the random-tick behaviour
  generator rather than adding a second hand-rolled PRNG to this crate.
- `lodestone_model::BlockPos` — the position type `neighbor_update.rs`'s
  `Direction::relative` operates on.
- `tracing` — the neighbour-update propagator's "too many chained updates"
  log, same dependency `tick.rs`'s own overload warning already uses.

[`RandomTickScheduler`]: ../crates/lodestone-server/src/random_tick.rs
[`RandomTickScheduler::tick_chunk`]: ../crates/lodestone-server/src/random_tick.rs
[`next_random_tick_pos`]: ../crates/lodestone-server/src/random_tick.rs
[`grass_random_tick`]: ../crates/lodestone-server/src/random_tick.rs
[`is_air_variant`]: ../crates/lodestone-server/src/random_tick.rs
[`is_randomly_ticking`]: ../crates/lodestone-server/src/random_tick.rs
[`DEFAULT_RANDOM_TICK_SPEED`]: ../crates/lodestone-server/src/random_tick.rs
[`ScheduledTickQueue<T>`]: ../crates/lodestone-server/src/scheduled_tick.rs
[`NeighborPropagator`]: ../crates/lodestone-server/src/neighbor_update.rs
[`NeighborPropagator::propagate`]: ../crates/lodestone-server/src/neighbor_update.rs
[`UPDATE_ORDER`]: ../crates/lodestone-server/src/neighbor_update.rs
[`BlockTickFeed`]: ../crates/lodestone-server/src/tick.rs
