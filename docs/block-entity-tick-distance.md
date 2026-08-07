# The block-entity scan is a cold-column term, but not a distance one

## What it is

The measured resolution of the block-entity lead in
[issue #503](https://github.com/matteopolak/lodestone/issues/503): whether `BlockEntityRegistry`'s
unfiltered 20 Hz scan is a **second, CPU-side, distance-dependent** cause of the game degrading as you
walk from spawn. The cold-column mechanism is real and costs **610 column regenerations per tick** past a
threshold, but it is flat in distance — what crosses the threshold is registry size, not travel.

The verdict is split, and the split is the useful part:

- **The mechanism is real and now measured.** A hopper whose column is not resident in `ChunkStore` costs
  a full cold column generation *on the tick thread*, and in the over-capacity regime that is **610 cold
  columns per tick** against a 50 ms budget.
- **The trigger in the lead is wrong.** Walking away does not cause it. Distance is **flat** — a
  walked-away hopper costs exactly **one** column generation for the whole session at every band from 64
  to 16,000,000 blocks out. What crosses the threshold is the *number of distinct hopper-bearing chunks*
  the registry has accumulated, because it has no unload path.

So this is **not** a second explanation for the owner's "exponentially slower as I walk away from spawn".
It is an unrelated latent cliff with a hard threshold, filed as its own issue. The instrument is the five
arms at the end of [`crates/lodestone-server/src/chunk_store.rs`][cs]'s test module.

## The four facts, in order

The lead was recorded as a chain. Facts 1–3 hold; the fourth link — the one that makes it a *distance*
term — is false.

### 1. Does the registry have an unload path? No (and one doc comment is stale)

`BlockEntityRegistry::remove` exists and **is** wired, but only to block *breaking* —
`server.rs`'s `apply_block_action`, `BlockActionKind::StopDestroy`. Its own doc comment still says
*"not done by this landing; `crate::server`'s `apply_block_action` does not call this yet"*, which is
**stale**: it does.

There is no *chunk*-unload path, which is the claim that matters. `ChunkStore` eviction calls
`ChunkSource::unload`, and neither implementor touches the registry: `OverworldChunkSource::unload` is
the no-op default, and `RegionChunkSource::unload` only inserts into `pending_unload` for the save path.
`restore_block_entities`' own doc says it outright — *"the registry has no eviction"* — and treats that as
deliberate, because a furnace must keep smelting while its chunk is out of the column cache.

**So the probed set only ever grows with exploration.** That is the load-bearing consequence, and it is
the one the threshold below is a function of.

### 2. Is it fully scanned per tick, unfiltered? Yes, and the scan is inert

`tick.rs`'s block-entity arm calls `tick_all_with_hopper_lock`, which collects **every** key into a fresh
`Vec<BlockPos>` at 20 Hz and dispatches on the variant. It does not filter, so a vanilla world's whole
block-entity population is walked every tick — including issue [#477][477]'s finding that **1,608 of
1,613** block entities in a real world are kinds this crate does not simulate and round-trips as
`BlockEntity::Opaque`.

But an `Opaque` entry costs a `Vec` push, two hash probes and an empty `tick_non_hopper` arm. **No
coordinate of it reaches the world.** Gated:
`sixteen_hundred_opaque_block_entities_never_reach_the_store` puts #477's own 1,608 figure into the
registry, spread over 1,608 distinct far-flung columns, and the store still generates only the 49-column
tick area. The competing hypothesis — a per-entry world probe — is `49 + 1608`.

**Only `BlockEntity::Hopper` probes the world**, because `tick_all_with_hopper_lock`'s `enabled` closure
is called for that variant and no other. So every threshold below is in *hoppers*, not block entities,
and hoppers sharing a chunk share one column.

### 3. Does the hopper path regenerate a column on a miss? Yes

`tick.rs` passes `|pos| hopper_enabled(&world.block_state(pos.x, pos.y, pos.z))`. In production `world` is
`Arc<ChunkStore<…>>` (`integrated.rs` wraps every source in `ChunkStore::new`), and
`ChunkStore::block_state` on a miss goes `ensure` → `self.source.column(cx, cz)` — a **whole column,
synchronously, on the tick thread**. Nothing about that is a stale trait default; it is the concrete
implementation, and the store's own docs measure a real column at 222–909 ms.

### 4. Does walking away cause the miss? No — this is where the chain breaks

Two arms, and they disagree with the lead by a factor of 52.

**Subject** (`a_walked_away_hopper_costs_one_column_generation_not_one_per_tick`): one hopper 1,600 blocks
out — the same stroll length the memory term was measured over — a 49-column tick area, and 52 ticks at
production capacity. The remote column is generated **exactly 1** time. Total session cost is `49 + 1`.

**Negative control** (`without_retention_a_remote_hopper_is_a_cold_column_every_single_tick`): the same
rig at `with_capacity(source, 0)`. The remote column is generated **exactly 52** times, one per tick —
which is the lead's own prediction, landed on by a real *configuration* of the shipped type rather than a
temporary neuter. **This is what makes the subject's `1` a measurement and not an absence**: without it,
a registry that was never scanned, a hopper that was never ticked and a closure that was never called
would all also report a low number.

**The curve** (`the_registry_scan_costs_the_same_at_every_distance_from_the_tick_area`): the same subject
with the hopper at 4, 100, 10,000 and 1,000,000 chunks out — 64 to 16,000,000 blocks. **1 at every band**,
total `49 + 1` at every band. The arms differ only in the hopper's coordinate, so any spread at all would
be a distance term. There is none, which matches
[`worldgen-store-distance-leak.md`](./worldgen-store-distance-leak.md)'s independent finding that
per-column cost is itself flat to 1,048,576 blocks.

## What the term actually is: a threshold on registry size

Since the probed set only grows (fact 1) and each probed position holds one column resident (fact 3),
exploration does not make any single call slower — it walks the **working set** across `ChunkStore`'s
fixed 512-column ceiling. The miss rate then goes from 0 to 1 over a narrow band, because a **cyclic**
scan of N positions through an LRU of capacity C < N is LRU's worst case: by the time the scan returns to
a position it has touched every other one, so *every* probe misses.

`the_miss_rate_crosses_from_zero_to_one_when_the_registry_outgrows_the_store`, byte-identical across three
runs:

| hoppers | resident set | ceiling | remote generations | evictions | cold columns / tick |
|---|---|---|---|---|---|
| 400 | 449 | 512 | **400** | 0 | 7.7 (amortised, one-off) |
| 600 | 649 | 512 | **31,739** | 31,276 | **610.4** |

Both regimes predicted from constants before measuring, not fitted: below the ceiling every column is
generated once for the whole session, so the total is `hoppers`; above it every probe misses, so the total
is `hoppers × ticks` = `600 × 52` = **31,200**. Measured 31,739 — the excess is the tick area churning in
the same pressure, and 1.7% agreement from an expectation computed outside the measurement is what makes
this a term rather than a correlation. The two regimes are a factor of 52 apart, not "more" and "less".

**610 cold columns per tick, at the 45–67 ms per column the walk investigation measured, is ~30 seconds of
generation per 50 ms tick.** That is a total freeze, not a slowdown.

### The threshold, derived

Production's resident set is `view + tick area + probed hopper columns`. At the default render distance 8
that is `289 + 49 = 338`, so the headroom is `512 - 338` = **174 distinct hopper-bearing chunks**.

Two things make that harder to reach than it looks, and both are worth stating so nobody panics: hoppers
in the same chunk share one column, and a fresh generated world contains **no** block entities at all —
`lodestone-worldgen` produces none, and the only two insert paths are player placement (`server.rs`) and
region-file restore (`region_source.rs`). Raising render distance eats the headroom directly: at 11
chunks the view alone is `23 × 23 = 529 > 512`.

### A wrong prediction, kept

The over-capacity arm was written predicting **1**, on the argument that `ChunkStore::read` refreshes
`last_used` on every hit, so a position polled at 20 Hz is permanently most-recently-used and
`evict_down_to` (which takes the *minimum*) would always prefer a stale tick-area column — i.e. that the
scan **pins** the columns it probes. It measured **12** and the argument is wrong: the scan runs once per
tick and the random-tick pass then touches 49 columns *after* it, so by the end of a pass the probed
column's stamp is the oldest in the map, not the newest. **Being polled at 20 Hz does not pin a column
when something else touches 49 columns in the same 50 ms.** The subject's flatness comes from headroom
alone, which is exactly why the over-capacity arm had to exist — without it, "no eviction" reads as a
property of polling when it is only a property of having room.

## This makes #481's stale claim wrong in a second way

[#481][481]'s `INITIAL_RANDOM_TICK_DEFERRAL_TICKS = 40` carries the claim that random ticks are *"the only
thing in this loop that touches `world.column()`"*, already recorded as stale as written. It is wrong
**again** here, and this instance is directly evidenced rather than inferred: in the zero-capacity control
the remote column is generated on all **52** ticks, including the **40** the deferral covers. The
block-entity scan reaches `world.column()` from **tick 1, with no deferral at all**, and unlike
`block_ticks.drain_due` it is not even in the deferred section of the loop — it runs above it.

`chunk_store.rs`'s own `RANDOM_TICK_PASSES` doc comment repeats the same claim, and it was *true* only
because `drive_tick_loop` passed `BlockEntityHandle::default()` — an empty registry. The new arms populate
it, and the claim is now false in that file too. This is the *world* species of vacuity in miniature: an
exemplary comment, correct about the input it was pointed at, wrong about production.

## How to change it

**Not fixed here — this is a diagnosis.** The fix is a gameplay-semantics decision, not a performance
tweak, so it is filed rather than landed.

The vanilla-correct shape: **vanilla ticks block entities per *loaded chunk*** (`LevelChunk`'s own
tick list, driven by the chunk map), not from one global registry. Bounding the scan by the loaded/view
set fixes both halves at once — the CPU cliff *and* the unbounded scan — and needs no eviction from the
registry, so it does not hit `restore_block_entities`' "a furnace must keep smelting" objection. It does
change behaviour: a furnace far from every player would stop advancing, which is what vanilla does.

Cheaper alternatives, if the semantics are contentious:

- **Make the probe non-generating.** A `block_state_if_resident`-shaped read that answers `None` on a miss
  and leaves the hopper's `enabled` at its last value. Removes the cold generation without changing which
  entities tick. Smallest patch; costs a stale redstone lock for a hopper nobody is near.
- **Cache the redstone lock.** `HopperBlock.checkPoweredState` already writes `ENABLED` on every neighbour
  change, so the tick loop does not need to *read* the world at 20 Hz — it needs to be *told* on change.
  This is the closest to what vanilla actually does with the flag.

Gotchas for whoever takes it:

- **The five arms in `chunk_store.rs` include two that characterise the defect** (the over-capacity arm
  and the over-capacity band of the curve). A fix turns them red **by design**. Rewrite them against the
  new bound; do not relax them.
- **Do not "fix" this by giving the registry an unload path on `ChunkStore` eviction.**
  `restore_block_entities` documents why: eviction is a *cache* event, the column comes back a moment
  later, and dropping live state on it rewinds the world every time a chunk leaves the cache.
- **A count is the instrument, not a duration.** This machine reproduces a worldgen wall-clock figure to
  only 10.8%; every count above was byte-identical across three runs.

## Configuration

- `DEFAULT_CAPACITY = 512` — [`chunk_store.rs`][cs]. The ceiling the threshold is measured against.
- `DEFAULT_RENDER_DISTANCE = 8` — `crates/lodestone-shell/src/config.rs`. Sets the 289-column view; 11 or
  more and the view alone exceeds the ceiling.
- `SHELL_TICK_RADIUS = 3` → 49-column tick area, transcribed from `net.rs`'s
  `mob_radius = view_radius.clamp(1, 3)`.
- `INITIAL_RANDOM_TICK_DEFERRAL_TICKS = 40` — `tick.rs`. Bounds the random-tick pass and **not** the
  block-entity scan, which is the finding above.
- The arms' own knobs are local `const`s: `REMOTE_CHUNK`, `BANDS`, `UNDER`/`OVER`, `OPAQUE_ENTITIES`.

## Dependencies

`lodestone-server` — `block_entities` (`BlockEntityRegistry`, `tick_all_with_hopper_lock`), `tick`
(`run_tick_loop`), `chunk_store` (`ChunkStore`, `ChunkSource`), `chunk` (`ChunkSource::unload`),
`region_source` (`restore_block_entities`), `redstone` (`hopper_enabled`), `hopper`. The arms use
`CountingSource`, which is hand-written precisely so no generator memo can absorb a second call — the real
`OverworldGenerator`'s 512-entry memo would make a count measured above it vacuous.

[503]: https://github.com/matteopolak/lodestone/issues/503
[481]: https://github.com/matteopolak/lodestone/issues/481
[477]: https://github.com/matteopolak/lodestone/issues/477
[cs]: ../crates/lodestone-server/src/chunk_store.rs
