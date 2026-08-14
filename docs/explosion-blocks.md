# Explosion block destruction

## What it is

The half of an explosion that removes blocks: a port of 26.2's
`ServerExplosion::calculateExplodedPositions` into
`crates/lodestone-server/src/explosion_blocks.rs`, on real per-block blast
resistance dumped from the jar, so a creeper leaves a crater instead of only
hurting whoever was standing in it.

Entity exposure, damage and knockback already existed in `lodestone-entity`'s
`explosion` module, and `MobSim::explode` already fired on the tick a creeper's
fuse completed — so a detonation already hurt the player and already reached the
client as an `EXPLODE` packet. What it did not do was remove a single block: the
word `resistance` appeared nowhere in the crate.

## How it works

**The ray sampling.** 1352 rays — every cell on the surface of a 16×16×16 grid,
`16³ − 14³` — each normalised to a unit direction and given a power of
`radius * (0.7 + nextFloat() * 0.6)`. Each ray marches in `0.3`-block steps from
the centre while power remains:

1. read the cell's resistance; if the cell is neither air nor fluid, subtract
   `(resistance + 0.3) * 0.3`;
2. if power is **still** positive, the cell joins the destroyed set;
3. advance `0.3` along the direction and subtract a further `0.22500001`.

**Two costs, and conflating them is the easiest mistake.** A cell that is truly
air with no fluid yields `Optional.empty()`, so the resistance term is skipped and
the step costs only `0.225`. A cell holding a *block* of resistance `r` — including
a zero-resistance one like `short_grass` — costs `(r + 0.3) * 0.3 + 0.225`, so even
resistance `0.0` costs `0.315`.

**A step is `0.3` blocks, not one block.** A creeper (`radius` 3.0) has ray powers
in `[2.1, 3.9)` and therefore reaches roughly 2.0 to 3.7 blocks through empty air —
the ~3-block crater a creeper actually leaves, and a useful sanity check on the
whole port.

**The destruction.** `destroy_blocks` writes air into every destroyed position and
returns the changes for the caller to publish. Air positions are skipped, matching
`BlockBehaviour::onExplosionHit`'s own `!state.isAir()` guard — so the reported set
is the cells that actually changed.

## What the arithmetic predicts, exactly

These are the gates, and they are arithmetic rather than statistical:

| claim | derivation |
|---|---|
| a creeper in solid stone destroys only Chebyshev-adjacent cells | a ray spends ≥ `2 × 0.225` leaving a one-cell pocket, the first stone cell costs `1.89`, a second would need `0.45 + 2×1.89 + 0.225 = 4.455` > `3.9` |
| every face neighbour is always destroyed | needs only `p > 2.34`, which is 87% of `[2.1, 3.9)`, and hundreds of rays point at each face |
| **no creeper ray can ever destroy obsidian** | `step_cost(1200) = 360.315` ≫ `3.9` |
| a blast through zero-resistance blocks reaches coordinate 4 and never 5 | `(3.9 − 0.09) / 0.315 = 12.09` steps × `0.3` = `3.6` blocks |
| a blast in pure air reaches coordinate 5 and never 6 | `3.9 / 0.225 = 17.3` steps × `0.3` = `5.1` blocks |

Which of the 12 edge and 8 corner cells a given seed reaches is *not* predictable
from the constants (they cost three pocket steps rather than two), so the total
count is bracketed at `20..=27` while the Chebyshev bound and the six face
neighbours are pinned exactly. Predict what you can, bracket the rest.

## Where RNG enters, and the one thing that is not portable

`ServerExplosion::explode` runs `calculateExplodedPositions` (1352 `nextFloat`
draws, one per ray), then `hurtEntities` (no RNG of its own — `getSeenPercent` is a
deterministic grid sample), then `interactWithBlocks`, then `createFire` if the
blast's `fire` flag is set.

`interactWithBlocks` opens with `Util.shuffle(targetBlocks, level.random)`, and
that matters twice. `Util::shuffle` is Fisher–Yates, so it consumes exactly
`n − 1` `nextInt` draws for `n` destroyed blocks — draws that shift every later
value in the stream, including `createFire`'s. `explosion_blocks::shuffle_draws`
exists so a caller that models fire can keep the stream aligned.

And the list it shuffles is built from a `HashSet<BlockPos>`, so its input order is
**Java hash-iteration order**. The conclusion is worth stating plainly rather than
discovering later: **vanilla's explosion drop order is not reproducible outside the
JVM.** A future drop implementation can match the multiset of items and the
per-block loot rolls, but not the sequence in which they are emitted. It is
unobservable today because nothing drops. Our own destroyed set is returned sorted
rather than in hash order, which is strictly better than an arbitrary order given
that.

## The data, and why it is a jar dump

`lodestone_data::block_blast`, from `oracle-java/BlastFireOracle.java`, which boots
the real 26.2 server headlessly and reads `Block::getExplosionResistance` for all
1196 registered blocks. `blocks.json` has no such field, and `minecraft-data` has
no 26.x data at all.

**Per block type, not per block state.** All four columns in that table are `Block`
fields, so a per-state table would be 32,366 rows of at most 1,196 distinct values.
The one exception is a *derived* per-state array,
`explosion_resistance_for_state_id`, which exists purely because the ray walk needs
a flat index rather than a name comparison — see the performance notes. It folds in
the calculator's `max(block, fluid)` term (so a waterlogged fence reads `100.0`) and
vanilla's `Optional.empty()` sentinel.

There are 34 distinct resistances in 26.2; the extremes are `0.0`
(`air`, `tnt`, `short_grass`), `6.0` (stone, 304 blocks), `1200.0` (obsidian) and
`3600000.0` (bedrock).

## What is deliberately not modelled

- **Drops.** `BlockBehaviour::onExplosionHit` rolls the block's loot table with
  `LootContextParams.EXPLOSION_RADIUS` set for a `DESTROY_WITH_DECAY` blast (a
  creeper's). `crate::loot` has no `EXPLOSION_RADIUS` parameter — its own module
  doc lists `survives_explosion` as unconditionally `true` and `explosion_decay` as
  a no-op — so rolling here would drop **every** block at full rate instead of
  vanilla's `1/radius`. Dropping nothing is the inert direction; duplicating items
  into a player's inventory is not. Closing this needs `EXPLOSION_RADIUS` in
  `loot.rs`, not a change here.
- **`shouldBlockExplode`.** The base implementation is unconditionally `true`, and
  a creeper is the only producer, so `true` is exact today.
- **Fire.** `createFire` runs only for a fire-flagged blast and a creeper's flag is
  `false`. `fire_positions` implements it anyway, for whichever producer sets it.
- **Block entities.** A destroyed chest's contents are not spilled.
- **`wasExploded` (TNT chain reaction) — landed, but not in this module.**
  `destroy_blocks` here still has no loot/drop knowledge and is not the
  production path; `crate::block_drops::drop_explosion_loot_in_blast` is, and
  that is where a destroyed `minecraft:tnt` block is chain-primed
  (`crate::mobs::MobSim::spawn_tnt_short_fuse`) instead of looted.

## How to change it, and the gotchas

- **Every world read goes through `cell_resistance`.** That is the single caching
  point a future section-level dense cache drops into, and it is also what keeps the
  march off `ChunkColumn::block_state`'s unguarded index: a blast on the world floor
  marches *down* past `min_y` on its first steps. Vanilla is safe for the mirror
  reason — `Level::getBlockState` answers `VOID_AIR` out of bounds and
  `calculateExplodedPositions` reads *then* breaks on `isInWorldBounds`, discarding
  what it read — so checking first and breaking is exactly equivalent.
- **The ray count, the step size and the exposure sampling are physics, not
  tunables.** Do not approximate any of them to make a blast cheaper. See
  [explosion performance](./explosion-performance.md) for what may be changed.
- **`BlastEnv` comes from a real column's `min_y`/`height`,** not from 26.2
  literals, for the same reason `fluid::FluidEnv` does.

## Configuration

Nothing tunable. `radius` comes from the producer (`CREEPER_EXPLOSION_RADIUS` for
every producer today) and `BlastEnv` from the dimension's build height.

## Dependencies

`lodestone_data::block_blast` (resistance),
`lodestone_data::block_states::state_id` (the string→id resolution on the read
path), `lodestone_data::block_solidity::legacy_solid` (the fire-placement support
test), `crate::chunk::ChunkSource`, `crate::mob_spawn::SpawnRng`. Its consumer is
the detonation drain in `tick::run_tick_loop`, fed by `MobSim::take_detonations`.
