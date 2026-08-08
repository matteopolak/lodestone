# Fire spread and burnout

## What it is

Fire behaving like fire on the integrated server: a port of 26.2's `FireBlock`
into `crates/lodestone-server/src/fire.rs`, driven by the block scheduled-tick
queue, so a fire ages toward burnout, eats the blocks around it, spreads onto
flammable neighbours at vanilla's own odds, is put out by rain, and burns forever
over netherrack. Lava's own random tick is what lights the first one.

Before this landed nothing in the crate ticked a fire at all: a `minecraft:fire`
block sat inert forever, spread to nothing and never went out.

## How it works

Four pieces: a data table, a decision module, a producer, and a driver.

**1. The data** — `lodestone_data::block_blast`, generated from a real headless
26.2 server (see [block-blast data](#the-data-and-why-it-is-a-jar-dump) below).
Fire needs three columns from it: `igniteOdds` (can fire start *on* this block),
`burnOdds` (can the block be consumed) and `ignitedByLava`.

**2. The module** — `crates/lodestone-server/src/fire.rs`, a transcription of
`FireBlock::tick`, `checkBurnOut`, `getIgniteOdds`, `isValidFireLocation`,
`canSurvive`, `getStateForPlacement` and `getFireTickDelay`, plus the two
`BaseFireBlock` statics they lean on. It owns no state: `run_scheduled_tick` takes
a `ChunkSource`, a `FireEnv` of world scalars, the block-tick queue and an RNG.

**3. The producer** — `random_tick::RandomTickScheduler::tick_lava`, a port of
`LavaFluid::randomTick`. This is the only thing in a generated world that creates
a fire block, because fire has no item (so a player cannot place one) and a
creeper's blast carries no fire flag. Adding it required adding
`minecraft:lava` to `random_tick::is_randomly_ticking`, which is correct —
`LavaFluid` is the one fluid that overrides `isRandomlyTicking` to `true` — and
which flows through to `ChunkColumn`'s per-section ticking counters automatically,
because those are computed from that same predicate.

**4. The driver** — the block-tick drain in `tick::run_tick_loop`, dispatching
`fire::TICK_FIRE` the way it already dispatches the redstone families.

Fire is **not** random-ticked. `Blocks.FIRE` is registered without
`randomTicks()`; it schedules itself, and the first statement of `FireBlock::tick`
is always another `scheduleTick`. So a fire block that loses its pending tick is
inert forever, which is why `fire::ticks_after_edit` exists and why
`random_tick::react_at_placement` schedules one for any fire written by a player
edit.

## The RNG draw sequence is the specification

A reordered or extra draw produces a plausible world that is not vanilla's. One
tick of one fire draws, in order:

| # | draw | when |
|---|---|---|
| 1 | `nextInt(10)` — the reschedule delay | **always**, before the spread gate |
| 2 | `nextFloat()` — the rain-out roll | only if not infiniburn **and** raining **and** near rain |
| 3 | `nextInt(3)` — the age advance, `age + n/2` | whenever draw 2 did not extinguish |
| 4 | `nextInt(4)` — the age-15 self-extinguish | only if `age == 15` and not infiniburn |
| 5–10 | `nextInt(300 or 250)` — one per neighbour burn-out check | **always**, six, in the order east, west, down, up, north, south |
| 5–10, on a hit | `nextInt(age + 10)`, then `nextInt(5)` | per consumed neighbour |
| 11… | `nextInt(rate)` — one per spread candidate with positive odds | over the 26-cell neighbourhood, in `x → z → y` order |
| 11…, on a hit | `nextInt(5)` — the spread age | per cell set alight |

Two of those are easy to get wrong, and both have their own test:

- **`checkBurnOut` draws even when the neighbour cannot burn.** The comparison is
  `nextInt(chance) < odds`, evaluated *after* the draw, so a fire surrounded by
  stone still consumes exactly six draws there.
- **The neighbourhood loop is `x`, then `z`, then `y`**, with `y` from `-1` to
  `4` — fire reaches four cells up and one down, not a symmetric cube.

`a_fire_over_netherrack_draws_exactly_eight_values` and
`a_fire_over_stone_returns_early_after_two_draws` pin the two ends of the tick's
control flow: over stone the tick returns early at `isValidFireLocation` having
drawn 2, over netherrack infiniburn skips that return and it draws 8. No outcome
assertion can see that fork.

## The odds arithmetic, and what it predicts

```
odds = (igniteOdds + 40 + difficulty * 7) / (age + 30)      // integer division
odds = odds / 2                                             // increased burnout only
rate = 100 + max(0, dy - 1) * 100
catches when nextInt(rate) <= odds
```

Both divisions truncate, and that truncation is the whole shape of fire's
behaviour:

| candidate | arithmetic | per-tick chance |
|---|---|---|
| oak planks, normal difficulty, fresh fire | `(5 + 40 + 14) / 30 = 1` | 2 in 100 |
| oak planks, normal difficulty, `age = 15` | `59 / 45 = 1` — *unchanged* | 2 in 100 |
| short grass, normal difficulty, fresh fire | `114 / 30 = 3` | 4 in 100 |
| short grass, **hard** difficulty | `121 / 30 = 4` | 5 in 100 |
| anything two cells up | `rate = 200` | halved |

`the_spread_rate_onto_a_named_candidate_matches_the_predicted_odds` measures the
plank and grass rates over 20,000 independent single ticks and requires them to
land on `0.02` and `0.04` — the prediction, not the direction. The burn-out gate
does the same for the cell below a fire: oak planks' `burnOdds` of `20` against
`nextInt(250)` is `0.08`, split evenly by the follow-up `nextInt(10) < 5` into
`0.04` becoming air and `0.04` becoming fire. Both halves are asserted, because a
port that removed the block on *every* hit would land on `0.08 / 0.00` and pass
any "the plank burns" assertion.

## The data, and why it is a jar dump

`crates/lodestone-data/src/block_blast.rs` plus its generated table, from
`oracle-java/BlastFireOracle.java`. `blocks.json` carries no flammability field at
all, and the fire odds are not even a block *property* — they live in two private
`Object2IntMap<Block>`s that `FireBlock::bootStrap` fills at boot, read here by
reflection.

Two measured facts worth keeping:

- **A source-only transcription would have missed 32 blocks.**
  `FireBlock::bootStrap` registers wool and carpet through `Blocks.WOOL.forEach` /
  `Blocks.CARPET.forEach` rather than by name.
- **`igniteOdds > 0` and `ignitedByLava` are different sets and neither contains
  the other.** 207 blocks are fire-flammable, 312 are lava-ignitable, and both
  differences are non-empty: every bed and `note_block` is lava-ignitable with no
  ignite odds, and every small flower, `hay_block`, `coal_block` and
  `scaffolding` is the reverse. Deriving either from the other is wrong in both
  directions.

`#minecraft:infiniburn_overworld` is read straight out of the jar's tag JSON and
is **`netherrack` and `magma_block`, not `bedrock`** — the intuitive guess is
backwards in both directions, so it has its own test.

## How to change it, and the gotchas

- **Every world read goes through `fire::block_at`**, which answers air outside
  build height. This is a hard invariant, not tidiness: the module reads the cell
  *below* whatever it inspects, so a fire on the world floor asks for
  `min_y - 1`, and `ChunkColumn::block_state` indexes unguarded — an unchecked
  read panics the world tick thread. `Level::getBlockState`'s own first line is
  the same guard. `fire_on_the_world_floor_does_not_panic` is the gate.
- **A new fire block always needs a scheduled tick.** Every write of a fire state
  must be accompanied by a `TICK_FIRE` schedule, or that fire is inert forever.
  There are three such sites: `fire::run_scheduled_tick`'s spread loop,
  `random_tick::tick_lava`, and `random_tick::react_at_placement`.
- **`FireEnv` is a value, not a handle.** Difficulty, rain and the spread
  permission are resolved by the caller from `world_state`/`weather`, so nothing
  here needs shared state. `spread_allowed` is 26.2's
  `fire_spread_radius_around_player` gamerule reduced to its answer — the old
  `doFireTick` boolean is gone.
- **Rain costs a sky scan.** `is_raining_at` walks upward to build height looking
  for a motion-blocking block, standing in for `canSeeSky` and the
  `MOTION_BLOCKING` heightmap at once. Everything is gated behind
  `FireEnv::raining`, so a dry world pays nothing; a raining one pays up to 130
  scans per fire tick. If that ever matters, the fix is a heightmap on
  `ChunkColumn`, not a looser rain test.

## Configuration

| knob | source | effect |
|---|---|---|
| `fire_spread_radius_around_player` | game rule, default 128 | `-1` freezes fire entirely; otherwise a player must be within it |
| difficulty | `world_state::difficulty` | the `difficulty * 7` term — hard spreads fire faster than easy |
| `random_tick_speed` | game rule, default 3 | how often lava gets a chance to light one |
| weather | `weather` module | rain extinguishes and blocks spread |

## Dependencies

`lodestone_data::block_blast` (ignite/burn odds, `ignitedByLava`),
`lodestone_data::snow_support::face_full_up` (`isFaceSturdy(UP)`),
`lodestone_data::block_solidity::blocks_motion` (the sky scan),
`crate::scheduled_tick` (the queue), `crate::mob_spawn::SpawnRng` (the RNG), and
`crate::chunk::ChunkSource` (the world).
