# Fluid spread

## What it is

Water and lava flowing on the integrated server: a port of 26.2's `FlowingFluid`
family into `crates/lodestone-server/src/fluid.rs`, driven by the scheduled-tick
queue, so a water source spreads seven cells on flat ground, a broken block under
an ocean floods, and lava meeting water makes obsidian, cobblestone or stone.

## How it works

Three pieces: a decision module, a driver, and a seeding hook.

**1. The module** — `crates/lodestone-server/src/fluid.rs`, read out of
`.cache/mc/26.2/src/net/minecraft/world/level/material/{FlowingFluid,WaterFluid,LavaFluid}.java`
plus the two halves of `block/LiquidBlock.java` that drive them. Its entry point
is `run_scheduled_tick`, which is `FlowingFluid.tick` with
`LiquidBlock.shouldSpreadLiquid` folded in front:

1. quench first (`quench_lava`) — lava with water above or beside it becomes
   obsidian if it is a source, cobblestone if it is flowing, basalt over soul soil
   next to blue ice. A quenched cell does not spread;
2. recompute a **non-source** cell from its neighbours (`new_liquid`, vanilla's
   `getNewLiquid`) and rewrite it, to air if the answer is empty;
3. spread (`spread`) — down first; sideways only when down is refused, the cell is
   a source, or the cell below is not a hole;
4. sideways spread scores every horizontal direction by `slope_distance` (how many
   steps to a hole, capped at `getSlopeFindDistance`) and fluid goes **only to the
   joint minimum**. That is why water on flat ground spreads evenly and water one
   cell from a pit flows only toward the pit.

Nothing draws from an RNG. Vanilla fluid spread is deterministic; the one RNG
consumer in the family is a *delay*, not a decision, and it is a named gap below.

**2. The driver** — `crate::tick::run_tick_loop`'s `fluid_ticks.drain_due` loop,
which had an empty body until this landed. It runs against the `ChunkSource` in
**world coordinates**, not against one `ChunkColumn`, because spread crosses chunk
borders; every other reaction path in `lodestone-server` (`propagate_and_react`,
`react_at_placement`) is column-bounded and would stop dead at the seam. Every
cell written is forwarded to clients through `BlockTickFeed::publish`.

**3. The seeding hook** — `fluid::ticks_after_edit(pos)`, called from
`crate::server`'s `destroy_block` and its placement handler. It stands in for
vanilla's `LiquidBlock.onPlace`/`neighborChanged`/`updateShape`, which this crate
has no block-lifecycle equivalent for. Two properties are deliberate:

- it **reads nothing and filters nothing** — the edited cell plus all six
  neighbours, unconditionally. Vanilla decides at schedule time whether a position
  holds a liquid; we decide at run time. That costs up to seven no-op drains per
  edit and buys a hook that works across a chunk border without loading the
  neighbouring column. A no-op drain schedules nothing, so there is no runaway;
- it is **not** folded into `server::propagate_placement`, whose return value
  `redstone_placement_gate` asserts on exactly. It is a separate request against
  the same `BlockTickFeed`.

`tick.rs`'s rebase loop routes by `kind`: `fluid::TICK_FLUID` goes into the fluid
queue, everything else into the block queue. `BlockTickFeed` carries one
relative-delay stream because it is one channel from the connection tasks, and
that loop is the only place both queues are in scope.

### The numbers, which are per-fluid and per-dimension

| | `getDropOff` | `getSlopeFindDistance` | `getTickDelay` |
|---|---|---|---|
| water | 1 | 4 | 5 |
| lava, overworld | 2 | 2 | 30 |
| lava, nether (`FAST_LAVA`) | 1 | 4 | 10 |

`getDropOff` fixes the reach. A source has `amount = 8`, each horizontal step
costs `dropOff`, and `amount <= 0` is empty — so **water reaches 7 cells** and
**overworld lava reaches 3**. `FluidEnv::OVERWORLD` / `FluidEnv::NETHER` carry
these; `FAST_LAVA` is a *dimension attribute*, not a gamerule.

### The level encoding, which is the easiest thing here to get backwards

The block carries `level` in `0..=15`; the fluid carries `amount` in `1..=8` plus
`falling`. `getLegacyLevel` and `LiquidBlock`'s `stateCache` are the two halves:

| block `level` | fluid |
|---|---|
| `0` (and a bare `minecraft:water`) | source, `amount = 8` |
| `1..=7` | flowing, `amount = 8 - level` |
| `8..=15` | **falling**, `amount = 8` (`getFluidState` clamps with `min(level, 8)`) |

`level` counts *down* from a source, so `level=1` is the wettest flowing state.
A falling cell is `amount == 8` and **not** a source — treating it as one makes a
waterfall self-sustaining so it never drains.

## How to change it, and the gotchas

**Every world read goes through `block_at`, and that is an invariant.** The spread
reads the cell *below* whatever it looks at, so a fluid resting on the floor of
the world asks for `min_y - 1`; `ChunkColumn::block_state` indexes unguarded and
would panic on the world tick thread. `block_at` answers air outside the build
height, which is also what `Level.getBlockState` does. `write_block` guards the
same way (`Level.setBlock` opens with `isOutsideBuildHeight`). This is why
`FluidEnv` carries `min_y`/`height` at all, and why `run_tick_loop` builds it with
`FluidEnv::overworld_in(column.min_y, column.height)` from a real column rather
than using the `-64..320` constant — every test double in this crate is shorter
than 384 rows.

**The neighbour notification is what makes a flow *drain*, not an optimisation.**
`run_scheduled_tick` ends by scheduling every cell it wrote **and all six
neighbours of each**. That is `Level.setBlock`'s flag-2 half
(`updateNeighborsAt` → `LiquidBlock.neighborChanged` → `scheduleTick`). Water
never replaces water horizontally (`WaterFluid.canBeReplacedWith` is
`direction == DOWN && !other.is(WATER)`), so a receding flow cannot be pushed back
by the cell behind it — each cell has to re-evaluate its *own* `getNewLiquid` and
shrink. Measured: with only the written cells rescheduled, removing a source left
the ramp frozen at `level=3` forever.

**Do not `return` early from `run_scheduled_tick` after writing.** The quench
branch and the drained-to-air branch both used to, and both stranded every
neighbour of the cell that had just stopped being a fluid — the same frozen-ramp
symptom, from a second cause. The `still_fluid` flag exists so the notify loop is
always reached.

**`can_pass_through_wall` is the one reduction rather than a transliteration.**
It is `Shapes.mergedFaceOccludes` evaluated over `lodestone_data::collision_shapes`'
axis-aligned box lists, with an **exact coordinate-sweep** coverage test rather
than a rasterisation (the census carries non-sixteenth coordinates — a cauldron's
`0.1875`, a lily pad's `0.09375` — that a 16×16 grid would round). It is exact for
a static shape and **wrong for a neighbour-dependent one**: our census is keyed by
block state, and vanilla's `getCollisionShape` for stairs/fences/walls/panes
consults the neighbours. Water flowing against a fence corner may disagree.

Do not "fix" that by loosening the predicate. It currently fails toward *not*
spreading; a loosened version fails toward water leaking through walls, which is
unrecoverable in a saved world.

**A source spreads sideways even when it has somewhere to fall.** `spread`'s
fall-through is `if (fluidState.isSource() || !isWaterHole(...))`, so once the
column below a source is established the down branch is refused (water never
replaces water) and the source makes a seven-wide sheet at its own level which
then falls in seven places. This is correct vanilla behaviour and it caught a test
whose *expectation* was wrong, not the code — `a_falling_column_spreads_seven_cells_at_its_base`
walls its shaft for exactly this reason.

### The named gaps

Each is chosen so the error direction is inert rather than plausible-looking:

- **`LavaFluid.getSpreadDelay`'s RNG quadrupling** is not modelled. It multiplies
  the delay by 4 with probability 3/4 when a non-falling lava cell's height
  *rises*, and this crate's fluid tick has no RNG in scope (the tick loop's lives
  inside `RandomTickScheduler`). Affects lava's timing while deepening, never the
  final pattern.
- **`beforeDestroyingBlock`** is a plain overwrite. Vanilla drops the destroyed
  block's loot for water and plays the `1501` fizz level-event for lava; we do
  neither.
- **`shouldSpreadLiquid` runs at tick time, not edit time.** Same outcome, one
  scheduled-tick delay later.
- **Bubble columns** are not modelled — there is no `BubbleColumnBlock` here.
- **Waterlogging is one-directional.** `spread_to` waterlogs a
  `SimpleWaterloggedBlock` target instead of replacing it, and `fluid_state_of`
  reads `waterlogged=true` back as a water source; nothing *un*-waterlogs.
- **Nothing places a fluid by hand.** There is no bucket item behaviour in this
  crate, so a player's route to a new source is `/setblock` or breaking into an
  existing body of water.

## Configuration

None. No feature flags and no env vars: fluid ticks are drained unconditionally.
The two gamerules the algorithm reads — `water_source_conversion` (vanilla default
`true`) and `lava_source_conversion` (default `false`) — are fields on `FluidEnv`
rather than reads of `crate::game_rules`, because `run_tick_loop` builds the env
once and the world's rule registry is not in scope at that point. Wiring them to
the live registry is a small, obvious change if `/gamerule lava_source_conversion`
ever needs to work.

## Dependencies

- `crate::scheduled_tick` — `ScheduledTickQueue`, the fluid queue, and the
  `(pos, kind)` dedup that absorbs overlapping neighbourhoods.
- `crate::chunk` — `ChunkSource` (world-coordinate reads and writes),
  `resolve_palette_state_id`, `AIR`.
- `crate::neighbor_update::Direction` — the six directions and `relative`.
- `crate::redstone::with_property` — writing `waterlogged=true`.
- `lodestone-data` — `collision_shapes` (the face-occlusion geometry) and
  `block_solidity::blocks_motion` (`canHoldAnyFluid`).
- `.cache/mc/26.2/src` — the record definitions, cited by file and line
  throughout `fluid.rs`. A cache, not repo state.
