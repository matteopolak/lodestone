# Random ticks, scheduled ticks, and neighbour-update propagation

Issues [#307](https://github.com/matteopolak/lodestone/issues/307) (random-tick
scheduler) and [#308](https://github.com/matteopolak/lodestone/issues/308)
(scheduled-tick queue and neighbour-update propagation) — the foundation layer
every block-tick feature (crop/sapling growth/leaf decay #310, gravity blocks
#311, fluid flow #309, fire spread #312, the redstone family #314-#322)
builds on. [#310](https://github.com/matteopolak/lodestone/issues/310) and
[#311](https://github.com/matteopolak/lodestone/issues/311) have since landed
on top of this foundation — see their own sections below.

## What it is

Five modules in `crates/lodestone-server/src/`, each a generic, vanilla-shaped
primitive with its own test suite, wired into `tick::run_tick_loop` (issue
#284):

- [`random_tick.rs`](../crates/lodestone-server/src/random_tick.rs) — issue
  #307. [`RandomTickScheduler`] replicates `ServerLevel::tickChunk`'s block
  selection (`ServerLevel.java:495-538`) and the position-pick LCG
  (`Level.getBlockRandomPos`, `Level.java:1064-1068`) exactly, and dispatches
  every randomly-ticking block family this crate models to its own handler:
  grass turning to dirt when the block above fully dampens light or drowns it,
  and dirt turning to grass when adjacent to a live grass block
  (`SpreadingSnowyBlock.randomTick`, `SpreadingSnowyBlock.java:48-64`) directly
  in this module, plus crop
  growth/sapling growth/leaf decay (issue #310) in
  [`growth_tick.rs`](../crates/lodestone-server/src/growth_tick.rs).
- [`scheduled_tick.rs`](../crates/lodestone-server/src/scheduled_tick.rs) —
  issue #308, first half. [`ScheduledTickQueue<T>`] mirrors vanilla's
  `LevelTicks`/`LevelChunkTicks`/`ScheduledTick` drain order (trigger tick,
  then priority, then insertion order).
- [`neighbor_update.rs`](../crates/lodestone-server/src/neighbor_update.rs) —
  issue #308, second half. [`NeighborPropagator`] mirrors
  `NeighborUpdater.UPDATE_ORDER` and `CollectingNeighborUpdater`'s
  depth-first cascade semantics — the "vanilla update-order quirks" issue
  #316 refers to. As of issue #311 this has a real production caller — see
  below.
- [`growth_tick.rs`](../crates/lodestone-server/src/growth_tick.rs) — issue
  #310. Crop growth (`CropBlock.randomTick`, `CropBlock.java:78-89`, with
  beetroot's own extra gate, `BeetrootBlock.java:45-49`), sapling growth
  (`SaplingBlock.java:45-57`, the real stage-0→1 cycle only — see its own
  section below for why the tree-growth half is a named gap), and leaf decay
  (`LeavesBlock.java:61-76`), all dispatched from `random_tick.rs`.
- [`gravity_tick.rs`](../crates/lodestone-server/src/gravity_tick.rs) — issue
  #311. Sand/gravel settling once unsupported (`FallingBlock.java:28-65`),
  triggered through `NeighborPropagator`'s first real production call — see
  its own section below for the two named deviations this landing accepts.

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
- **Section eligibility.** `LevelChunkSection::isRandomlyTicking` is
  `tickingBlockCount > 0`, an incrementally maintained counter, and **this crate
  now keeps the same one**: `ChunkColumn::section_ticking`, a `u16` per implicit
  16-row window, maintained in `ChunkColumn::set_block` (`±1` when the leaving
  and arriving states differ in classification) and recomputed once per adopted
  grid by `recalc_ticking_counts`. `ChunkColumn` still has no per-section
  *struct* — sections are 16-row windows of one flat grid — so the counter is a
  per-column vector rather than a field on a section object.
  [`RandomTickScheduler::tick_chunk`] reads it with one integer compare.

  It was a per-block scan until issue #507. That scan ran the **string**
  predicate on all 4096 blocks of every section, of every column, every tick;
  `sample(1)` put **97.6%** of the integrated server's tick thread in it and
  chunk delivery starved so badly that rings 5–8 of a 289-column view never
  arrived (`mesh-fill-rate.md`).

  **Get the budget arithmetic's multiplier right if you requote it.** This loop
  iterates `tick_area`, *not* the streamed view: `tick_area` is `mob_area`
  (`integrated.rs:520`) at radius `view_radius.clamp(1, 3)` (`net.rs:1773`), a
  7×7 square — **49 columns**, which `integrated.rs:538` also states. At the
  measured 2.108 ms/column that is **103 ms per 50 ms tick, 2.07× over budget**
  against a `50 / 2.108 = 23.7`-column headroom. Earlier records (including
  `bdf93a28`'s commit message and, until this change, `mesh-fill-rate.md`)
  multiplied by the 361-column view and reported 761 ms / 15.2×; those two
  numbers are wrong and must not be requoted. The starvation conclusion is not
  affected — 49 still exceeds 23.7.

  Two properties keep the counter honest, and both
  are gated in `crates/lodestone-server/tests/random_tick_section_counters.rs`:
  the counters equal an independent recount at every step of a mutation storm
  and across an NBT round trip, and the O(1) decision leaves the position LCG on
  the exact sequence the definitional scan would have (draw **order and count**
  is the spec, not just the resulting world). A `debug_assert!` inside
  `tick_chunk` re-checks the decision against that scan on every debug run.

  Two gotchas if you touch it. **Decrement with `-=` behind a `debug_assert!`,
  never `saturating_sub`** — saturation hides exactly the maintenance bug the
  counter exists to prevent. And the counters are **derived state, never
  serialized**: `chunk_nbt` does not write them, so widening
  [`is_randomly_ticking`] cannot strand a stale persisted count.
- **Fluids are out of scope, deliberately.** Vanilla's section gate is
  `isRandomlyTickingBlocks() || isRandomlyTickingFluids()`, and lava is the one
  fluid that ticks (`LavaFluid.java:221` overrides `Fluid.java:79`). This crate
  models no fluid random ticks, so a `tickingFluidCount` today would have zero
  producers and zero consumers. The disclosed consequence: our LCG position
  stream is not vanilla-comparable for a section whose only ticking content is
  lava. A comment at the gate site in `tick_chunk` says what a future lava
  handler must add; see `docs/plans/random-tick-counter.md` §"Fluids".
- **Grass ↔ dirt**, cited from `SpreadingSnowyBlock.randomTick`
  (`SpreadingSnowyBlock.java:48-64`): not `canStayAlive` → convert to dirt,
  zero further draws. `canStayAlive` **and**
  `getMaxLocalRawBrightness(pos.above()) >= 9` → four spread attempts, three
  `nextInt` draws each (offset `x/y/z`), regardless of hits.

  `canStayAlive` (`:29-41`) is now the **real predicate** (issue #544), in this
  order: snow with `LAYERS == 1` → alive; a **full** fluid state above → dead;
  otherwise `getLightDampeningInto(...) < 15`, which for two full-cube states is
  the above block's own `getLightDampening()` —
  `lodestone_data::light_props::dampening`'s column exactly.

  It used to be proxied by **"the block directly above is bare air"**, and that
  proxy was a **shipped, owner-visible bug**: `minecraft:short_grass` is non-air,
  vanilla's own vegetation step places short grass on top of grass blocks, so
  every decorated grass patch turned to dirt on its first random tick. The proxy
  existed because there was no dampening census; `light_props` (landed
  `3f26be21`) is that census. Generation was innocent — `feature/top_layer.rs`
  and `feature/vegetation/` place `grass_block` with `short_grass` above it, as
  vanilla does.

  **A different simplification survives**: the
  `getMaxLocalRawBrightness(pos.above()) >= 9` gate on the *spread* branch. The
  driver holds a `ChunkColumn`, not a light map, so a live grass block always
  attempts a spread regardless of time of day. It can never make grass die
  wrongly.

  Two consequences worth stating, because both are the kind of thing a
  self-consistent test hides:

  - **The draw count now depends on which block is above, not merely whether one
    is.** Grass under short grass consumes 12 behaviour draws where it consumed
    0. That is vanilla's count *for the same above-block*, which is the standard;
    self-consistency is not.
  - **One branch of `getLightDampeningInto` is not modelled**: its hard `16` when
    the two states' *occlusion shapes* merge to a fully-occluding face, reachable
    only for an occluding non-full-cube above (stairs, some slabs). This crate has
    no occlusion-shape census — collision shapes are a different question (glass
    has a full collision box and occludes no light) — so those fall through to
    their `dampening` column. That can only make grass **survive** where vanilla
    kills it, never the reverse.

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

**As of issue #311, this is no longer true — gravity blocks are
`NeighborPropagator`'s first real production caller.** Every mutation
`crate::random_tick::RandomTickScheduler::tick_randomly_ticking_block` makes
(grass↔dirt, crop growth, sapling growth, leaf decay — #310, above) now
calls `NeighborPropagator::propagate` on the mutated position, mirroring
vanilla's own `setBlockAndUpdate` always notifying neighbours after a
change. The one reaction modeled today is a sand/gravel block settling once
its support disappears (`crate::gravity_tick`, cited from
`FallingBlock.java`); a settled block's old position is re-notified from
directly above so a stacked column of gravity blocks collapses one at a
time, depth-first — the exact cascade shape this primitive exists to
provide. See `crate::gravity_tick`'s own module doc for the full citation
and, importantly, the **two named deviations** this landing accepts rather
than leaving gravity blocks a further island: no `FallingBlockEntity` (the
block moves directly, one computed step, not a smoothly-animated entity —
this crate has no free-entity-simulation seam for a temporary block-shaped
entity), and no 2-tick scheduled delay (the settle runs synchronously inside
`propagate`'s own notify closure, because `ScheduledTickQueue`'s drain
dispatch lives in `tick.rs`, a file this landing's task brief did not permit
editing directly — see that module doc for the exact brokered-edit note).
**The trigger surface is still narrower than vanilla's**: it fires only when
one of `crate::random_tick`'s own mutations happens to be adjacent to an
unsupported gravity block, not on every block change in the world — the far
more common vanilla trigger (a player mining the block below a sand column)
is `server.rs`'s block-break handling, which does not call `propagate` yet
and was off-limits to this task. **As of #314/#315/#317, redstone dust,
torches, repeaters, comparators, and observers are real consumers of this
exact call site** — see [`docs/redstone.md`](./redstone.md) — inheriting the
depth-first ordering guarantee unchanged. Pistons (#316) and the remaining
redstone children (#318-#322) have not landed.

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
- **Crop growth, sapling growth, and leaf decay (#310) reach a client through
  the exact same path, with zero changes to `tick.rs` or `server.rs`.**
  `RandomTickScheduler::tick_chunk`'s caller already forwards whatever `Vec`
  it returns, one block-state string at a time, regardless of which family
  produced it — so extending the dispatch inside `random_tick.rs` (a file
  this landing owns) was sufficient. **Unlike grass, nothing in this crate's
  worldgen places crops, saplings, or leaves naturally** (`crate::chunk`'s own
  module doc: no vegetation at all), so today's demonstration is
  `ChunkColumn`-level (a block placed directly into the shared source ticks
  and mutates exactly like grass does), not a fully natural in-terrain one.
- **Gravity blocks (#311) reach a client the same way, and are additionally
  the first real caller of `NeighborPropagator`.** Sand and gravel are
  genuinely placed by this crate's worldgen (`crates/lodestone-worldgen/src/surface/mod.rs`'s
  own module doc: "sand near water, gravel on the ocean floor"), so this is
  the first block family in this landing where the *material* occurs
  naturally — but the *trigger* (a neighbour notification) still does not
  fire from anything a player does yet, only from this crate's own
  random-tick mutations landing next to an unsupported gravity block. See
  `crate::gravity_tick`'s module doc for that scope note in full, including
  the two named deviations (no `FallingBlockEntity`, no 2-tick delay).
- **The block-tick queue has real producers as of #314/#315/#317** — redstone
  torches/repeaters/comparators/observers schedule delayed rechecks into it
  from `propagate_and_react`, and `tick::run_tick_loop`'s drain dispatches
  each one to its own family's `run_scheduled_tick`. See
  [`docs/redstone.md`](./redstone.md). The **fluid**-tick queue is still an
  acknowledged island — nothing calls `schedule` on it. Fluid flow (#309) and
  fire (#312) are its next candidates.
- **`tick_area` is a small, fixed chunk range** — the same
  `(cx_range, cz_range)` `open_in_memory_with_mobs` already threads through
  as `mob_area`, reused rather than adding a second "which chunks are
  loaded" concept (this crate has none — see `chunk.rs`'s own module doc).
  Every chunk in it is re-fetched via `ChunkSource::column` **every tick**;
  for an unedited column this re-runs the generator, a real, documented
  performance gap for anything wider than a handful of chunks.

## How to change it, and the gotchas

- **The two queues are still `run_tick_loop` locals, and that is the one thing
  standing between scheduled ticks and the disk.** Issue
  [#468](https://github.com/matteopolak/lodestone/issues/468) built the whole
  persistence path for them — `chunk_nbt::SavedTick` for the schema and
  `region_source::ScheduledTickHandle` for the shared queues plus the game tick
  their `trigger_tick`s are measured against — and both halves are gated
  (`tests/scheduled_tick_persistence.rs`,
  `tests/chunk_extras_vanilla_oracle.rs`). What is missing is that
  `tick::run_tick_loop` still writes:

  ```rust
  let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
  let mut fluid_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
  ```

  so the queues persistence can read are always empty in production, and a pending
  repeater tick is still lost on quit. The wiring is deliberately shaped to be a
  **wrapper rather than a rewrite**, because this function is where the redstone
  work lives:

  1. Add a parameter `scheduled: crate::region_source::ScheduledTickHandle`, and
     pass `persistent.scheduled_ticks()` from
     `IntegratedServer::open_persistent_with_mobs` (the in-memory constructor
     passes `ScheduledTickHandle::default()`, exactly as it now does for
     `BlockEntityHandle`).
  2. Delete the two `let mut` lines above.
  3. Wrap the existing scheduled-tick section — from the
     `block_tick_out.drain_scheduled_ticks()` adoption loop through the last use
     of `fluid_ticks` — in one `scheduled.with(|queues| { … })`, binding
     `let block_ticks = &mut queues.block; let fluid_ticks = &mut queues.fluid;`
     at the top so **every existing use site is textually unchanged**.
  4. Add `scheduled.set_game_tick(game_tick);` right after `game_tick += 1`.

  Two things to know before doing it. `with` hands over both queues in one
  **synchronous** closure on purpose: a closure cannot contain an `.await`, so
  the compiler — not a reviewer — guarantees the `MutexGuard` is never held
  across a suspension point, which would make the tick task non-`Send`. And the
  game tick must come from *this* counter and not be re-derived: a second clock
  here is issue [#323](https://github.com/matteopolak/lodestone/issues/323)'s
  bug in a new place, where `SET_TIME` decoded and really did darken the sky with
  every link in the wire green while the value was wall-clock
  elapsed-since-join.
- **`ChunkColumn`'s `block_state`/`set_block` take LOCAL x/z; every tick
  handler is handed ABSOLUTE x/z plus `min_x`/`min_z` to convert with.** Mixing
  the two was issue #472: grass propagation bounds-checked the local `tlz` and
  then passed the absolute `tz` to `block_state`. The guard reads as present
  and correct — the defect is the argument passed *after* it. `index` is
  `((y_local * 16 + z) * 16 + x)` with a `debug_assert` on `z`, so an absolute
  `z` panics only in a debug build; in the release build that actually ships it
  silently aliases onto local `(x, y + cz, z)`, `cz` y-levels too high, and
  grass still spreads — just not from where it should. **Test any coordinate
  handling at a chunk with a non-zero `min_z`**: at chunk `(0, 0)` local and
  absolute coincide, so the obvious fixture structurally cannot fail. The gates
  are `random_tick.rs`'s `grass_spreads_at_a_chunk_whose_local_and_absolute_z_differ`
  and `an_absolute_z_misread_would_convert_a_non_dirt_block_at_the_correct_coordinate`,
  both at chunk `(2, 3)`, and both fail in *both* profiles if the mix-up returns.
- **Adding a new randomly-ticking block**: extend [`is_randomly_ticking`] and
  add a branch to `RandomTickScheduler::tick_randomly_ticking_block`'s
  dispatch (grass lives directly in this module; crop/sapling/leaf logic
  lives in `growth_tick.rs` and is called from a `tick_*_block` sibling —
  follow whichever of those two shapes your new block is closer to).
- **Adding a real scheduled-tick producer**: call
  `ScheduledTickQueue::schedule` from wherever a block decides "run again in
  N ticks" (vanilla's own `level.scheduleTick`). The queue does not care what
  `T` is — this crate keys it by canonical block-state-name `String` to match
  `ChunkColumn`'s own representation, not by a `Block`/`Fluid` registry
  object like vanilla, since this crate has no such registry. **Still
  nothing calls this** — `tick_randomly_ticking_block`'s gravity settle
  (below) runs synchronously instead, specifically because this queue's own
  drain dispatch lives in the brokered `tick.rs`; a landing that *can* edit
  `tick.rs` should prefer scheduling a real delayed tick over another
  synchronous settle.
- **Adding a real neighbour-update producer**: call
  `NeighborPropagator::propagate` with a `notify` closure that mutates the
  world and returns any further single-target notifications that mutation
  itself triggers — do **not** call `propagate` recursively from inside
  `notify`; the propagator's own stack already handles cascading, and a
  second nested call would double-count `max_chained`.
  `RandomTickScheduler::tick_randomly_ticking_block`'s own call
  (`propagate_and_react`, `random_tick.rs` — renamed from
  `propagate_and_settle_gravity` once redstone, #314, became a second real
  reaction; see [`docs/redstone.md`](./redstone.md)) is the worked example:
  it is called once per mutated position, with a `notify` closure that
  dispatches to whichever reaction's predicates match (gravity settle first,
  then dust/torch/diode/observer) and, on a settle, returns a single
  `Direction::Down` re-notification of the vacated position's old neighbour
  above it — that one extra `Notification` is what makes a stacked column of
  gravity blocks collapse one at a time within the *same* `propagate` call,
  rather than needing a second call per block in the stack.
- **Adding another neighbour-update reaction**: extend `propagate_and_react`'s
  `notify` closure dispatch (the same function gravity/redstone both already
  extend, per the "don't generalize before a second case" reasoning
  `is_randomly_ticking`'s own history already gives, now exercised a second
  time going from one reaction to four) — the call site, the depth-first
  ordering, and the cross-chunk-neighbour limitation below are all already
  handled; only the reaction itself is new work.
- **The cross-chunk-neighbour gap is real and applies to gravity and
  redstone alike**: a neighbour notification landing outside the ticked
  column's 16×16 footprint is silently skipped (`propagate_and_react`'s own
  bounds check) — the identical limitation `tick_grass_block`'s own spread
  already accepts, for the identical reason (`tick_chunk` has no
  neighbouring-column access). A gravity block, or a redstone wire/diode,
  one block-column away from the mutation that should have triggered it
  will not react until a real per-tick multi-column cache exists (see
  "widening `tick_area`" below).
- **Widening `tick_area` beyond a handful of chunks**: add a real per-tick
  column cache first (`OverworldChunkSource`'s `edits` map only helps
  already-edited columns); this landing deliberately did not build one,
  since #307/#308's job is the scheduler, not chunk-loading infrastructure.
- **If you make per-position light available to the driver**: replace the
  remaining `can_stay_alive`-doubles-as-brightness use in `grass_random_tick`
  with a real `getMaxLocalRawBrightness(pos.above()) >= 9` check. The draw
  pattern (12 draws in the live branch) does not change, only which positions
  qualify. `canStayAlive` itself no longer needs anything — it reads
  `lodestone_data::light_props::dampening` (#544).
- **If you add an occlusion-shape census**: close `getLightDampeningInto`'s
  merged-face `16` branch in `grass_can_stay_alive`. Do **not** substitute
  `collision_shapes` for it; they answer a different question.

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
