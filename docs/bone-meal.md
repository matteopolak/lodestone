# Bone meal

## What it is

The instant-growth right-click: a port of 26.2's `BoneMealItem::useOn` and the
per-block `isValidBonemealTarget` / `isBonemealSuccess` / `performBonemeal` triples
it dispatches to, in `crates/lodestone-server/src/bone_meal.rs`. Bone meal on wheat,
carrots, potatoes or beetroots jumps the crop several growth stages; on a sapling it
advances the stage 45% of the time and is consumed either way.

The *growth* half of this family already existed — `growth_tick` holds the
random-tick probability rules for crops, saplings and leaves, and `random_tick`
drives them every tick. What did not exist was bone meal: the word appeared in the
crate only in the composter's *output* paths, so the one item whose entire purpose is
to grow a plant did nothing.

## How it works

`apply_bone_meal(state, above_state, rng) -> BoneMealOutcome` is the whole rule. It
touches no world: the caller passes the clicked block's state and the state directly
above it, and applies the outcome. That is the same decide-then-apply split
`hand_use` uses, so the rule is testable with no `ChunkSource` in scope.

| family | `isValidBonemealTarget` | `isBonemealSuccess` | `performBonemeal` |
|---|---|---|---|
| `CropBlock` (wheat, carrots, potatoes) | `!isMaxAge` | `true`, no draw | `age += Mth.nextInt(random, 2, 5)`, clamped to 7 |
| `BeetrootBlock` | same | same | the same draw **divided by 3**, so `+0` or `+1`, clamped to 3 |
| `SaplingBlock` | inside build height | `nextFloat() < 0.45` | stage 0 → 1, else grow a tree |
| `GrassBlock` | the cell above is air | `true`, no draw | place up to 128 vegetation features |

Four outcomes:

- `NotBonemealable` — vanilla's `PASS`. Nothing consumed, no RNG drawn, caller falls
  through to whatever a right-click would otherwise do.
- `ConsumedNoChange` — a valid target whose success roll failed. **One bone meal is
  consumed and the block is unchanged.** Only saplings produce this.
- `Grew { state }` — one bone meal consumed, the block becomes `state`.
- `NotModelled { reason }` — a valid vanilla target this crate cannot grow. Treated
  as `PASS` and consumes nothing, because consuming an item for an effect we did not
  produce is worse than doing nothing.

**The item is consumed even when the success roll fails.**
`BoneMealItem::growCrop` shrinks the stack outside the `isBonemealSuccess` branch, so
a sapling eats bone meal 55% of the time for nothing. Getting this wrong would give
players free bone meal.

## The RNG draws are the specification

Exactly one draw per use, and which one depends on the family:

- a crop draws `nextInt(4)` once — `Mth::nextInt(random, 2, 5)` is
  `nextInt(max - min + 1) + min` — and **nothing else**, because
  `isBonemealSuccess` is a constant `true` with no draw at all;
- a sapling draws `nextFloat()` once for the 0.45 gate and, on a hit, no further draw
  (the stage-0 advance is a plain `cycle(STAGE)`);
- a use on a non-target draws nothing, so a failed click cannot shift the stream for
  the next one.

Beetroot is the one that looks like it should differ and does not: its
`getBonemealAgeIncrease` is `super.getBonemealAgeIncrease(level) / 3`, so it is the
*same single draw*, divided. `(nextInt(4) + 2) / 3` is `0` for one of the four
outcomes and `1` for the other three — a 3-in-4 chance of a single stage, never two.
`beetroot_advances_by_zero_or_one_from_one_draw` asserts that 1-in-4 distribution
over 4,000 uses, and `one_crop_use_draws_exactly_one_value` (with its own
one-fewer-draw control) pins the count.

## Two named gaps, both because the growth they need does not exist here

- **`GrassBlock::performBonemeal`** places vegetation *features* — the
  `grass_bonemeal` placed feature plus the biome's own bone-meal features — across
  128 attempts, and each attempt's offset walk and each feature placement draw from
  the same RNG. `lodestone-worldgen` has no feature placer, and a partial version
  (say, dropping one `short_grass` where the feature would have gone) would consume a
  *different* number of draws and so corrupt every later attempt in the same call.
  That is the "plausible world that is not vanilla's" failure mode, so this reports
  `NotModelled` rather than inventing a sequence.
- **A stage-1 sapling** needs `TreeGrower::growTree` — the same missing feature
  placer, already documented as an uncloseable gap for the random-tick path in
  `growth_tick`.

`growWaterPlant` (seagrass and coral from bone meal on water) is out of scope for the
same reason plus a second: it needs biome tags this crate does not carry.

## How to change it, and the gotchas

- **Adding a family** means an arm in `apply_bone_meal` plus its own predicate. If
  the new family's `performBonemeal` draws, transcribe the draw *count* first and
  assert it — the surrounding outcome is easy to eyeball and the draw count is not.
- **Do not fold `isBonemealSuccess` into a generic "roll for success".** Three of the
  four families do not roll at all, and a spurious draw shifts every later value in
  the stream.
- **The `NotModelled` arm must not consume.** It is the honest half of the gap; a
  version that consumed would look like vanilla to a casual player and quietly eat
  their bone meal.

## Configuration

None. Nothing here reads a game rule; `random_tick_speed` affects the *random-tick*
growth path in `growth_tick`, not this.

## Dependencies

`crate::growth_tick` (the crop/sapling predicates, ages and stages),
`crate::random_tick::is_air_variant` (the grass target test),
`crate::mob_spawn::SpawnRng`. Its producer is the right-click handler in `server.rs`,
which resolves the held item and applies the outcome.
