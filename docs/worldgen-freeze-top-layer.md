# Snow layers and surface ice: `freeze_top_layer` (`TOP_LAYER_MODIFICATION`)

## What it is

The port of vanilla's `freeze_top_layer` — the whole of the eleventh and last
decoration step, `TOP_LAYER_MODIFICATION` — into `lodestone-worldgen`, composed
into the live generator, and gated bit-exactly against the real 26.2 server.
This is issue #404's unit U2. Before it, nothing in this engine ever wrote
`minecraft:snow` or surface ice: every snowy biome generated bare, which the
parity census called the most player-visible absent feature in worldgen.

## How it works

### The vanilla algorithm

`SnowAndFreezeFeature.place`
(`.cache/mc/26.2/src/net/minecraft/world/level/levelgen/feature/SnowAndFreezeFeature.java:20-49`)
walks the chunk's 16×16 columns, `dx` outer and `dz` inner, and per column:

1. `int y = level.getHeight(Heightmap.Types.MOTION_BLOCKING, x, z)` — the first
   **free** Y above the column's topmost motion-blocking-or-fluid block.
2. `topPos = (x, y, z)`, `belowPos = topPos.below()`.
3. `biome = level.getBiome(topPos).value()`.
4. **Ice**: `if (biome.shouldFreeze(level, belowPos, false)) setBlock(belowPos, ICE)`.
5. **Snow**: `if (biome.shouldSnow(level, topPos)) { setBlock(topPos, SNOW); }` and
   then, if the block at `belowPos` has the `snowy` property, set it `true`
   (`SnowAndFreezeFeature.java:40-43`).

The predicates are on `Biome`: `shouldFreeze` at `Biome.java:145-169`, `shouldSnow`
at `Biome.java:183-196`, `warmEnoughToRain` (`>= 0.15F`) at `Biome.java:175-177`,
`getPrecipitationAt` at `Biome.java:104-110`, and `getHeightAdjustedTemperature`
at `Biome.java:112-121`. Snow survival is `SnowLayerBlock.canSurvive`
(`SnowLayerBlock.java:76-86`).

### Where it sits in this engine

`OverworldGenerator::column` runs `pre_ore_stage → ore_stage → vegetation_stage →
top_layer_stage → intern`. The step must come **after** vegetation, because the
`MOTION_BLOCKING` height it reads includes leaves and logs — snow sits on a
spruce canopy — and running it earlier would place snow at the pre-tree surface
and then bury it.

**There is no 3×3 neighbourhood driver, and that is vanilla's own behaviour, not
a narrowing.** The feature's loops are `0..16` from the chunk origin and every
write is at `(x, y, z)` or `(x, y - 1, z)` of that same column, so unlike ores
and vegetation there is no `blockStateWriteRadius(1)` spill for a neighbour
driver to model: a centre-only pass **is** the full behaviour. The stage costs
one 256-column scan over a grid already in hand.

### The step consumes zero randomness

`SnowAndFreezeFeature.java` does not contain the string `random` — not a field, a
parameter, or an unused import. Its placed feature is
`[{"type": "minecraft:biome"}]` and nothing else, and `BiomeFilter.shouldPlace`
never touches the `RandomSource` it is handed (`BiomeFilter.java:20-26`,
`PlacementFilter.java:8-11`).

So the draw-count trap that broke vegetation parity — a `TrapezoidInt`
approximated as `Uniform`, same distribution, one draw instead of two, every
later call desynchronised — has **no analogue here**. Adding the step also cannot
desynchronise an earlier one: vanilla reseeds per feature with
`setFeatureSeed(decorationSeed, globalFeatureIndex, stepIndex)`
(`ChunkGenerator.java:389`), and this step's index is 10, past everything this
engine composes.

### The temperature source is the whole trap

`Biome.warmEnoughToRain` reads `getTemperature(pos, seaLevel)`, which is
**height-adjusted**:

```
adjusted = temperatureModifier.modifyTemperature(pos, baseTemperature)
snowLevel = seaLevel + 17                       // 80 in the overworld
if (pos.getY() > snowLevel):
    v = (float)(TEMPERATURE_NOISE.getValue(pos.getX() / 8.0F, pos.getZ() / 8.0F, false) * 8.0)
    return adjusted - (v + pos.getY() - snowLevel) * 0.05F / 40.0F
return adjusted
```

Using the flat biome `temperature` field instead is a plausible-looking error
with a very visible signature. `windswept_hills` declares `temperature = 0.2`,
comfortably **above** the `0.15` rain threshold, so a flat reading says "never
snows" and deletes vanilla's snow caps entirely.

Two details that are not incidental:

- **Every arithmetic step above is Java `float`**, and the noise *input* is a
  float divide widened to double: `pos.getX() / 8.0F` is
  `(double)((float) x / 8.0f)`, **not** `(double) x / 8.0`. Computing the whole
  expression in `f64` shifts the snow line by a fraction of a block near the
  threshold, which moves which columns snow.
- **`TemperatureModifier.FROZEN`** (`Biome.java:395-409`) blends three noise
  reads into an ice-patch mask and clamps to a hard `0.2F` — *above* the
  threshold, i.e. **warmer** — inside a patch. So a frozen ocean is ice with
  noise-shaped open gaps, not solid ice. Measured: 36 of 256 columns froze at
  chunk (-600, 0). Only `frozen_ocean` and `deep_frozen_ocean` carry the
  modifier in 26.2.

### This is not all the ice in the game

`freeze_top_layer` writes **one** ice block per qualifying column, at the water
surface, and nothing else. The other cold-biome content is a different set of
features in *different* steps and is still absent (`docs/plans/worldgen-parity.md`
census rows 6d and 6f, unit U4): `iceberg_packed`/`iceberg_blue` are
`LOCAL_MODIFICATIONS` (step 2) and `blue_ice` is `SURFACE_STRUCTURES` (step 4),
all three frozen-ocean-only. So a frozen ocean generated today is flat surface ice
with no icebergs — correct for this step, incomplete for the biome. `packed_ice`
and `blue_ice` also appear in *surface rules* (already composed, a different
mechanism entirely), which is why they show up in generated terrain despite U4
being open.

`crate::biome::cold_enough_to_snow` — the pre-existing flat-temperature helper —
is deliberately **not** reused. Surface rules ask a different question at a
different Y, and answering it with the height-adjusted value would change
composed surface output.

### The five per-block-state facts, and why they are dumped

`canSurvive` and the `MOTION_BLOCKING` heightmap need facts that are in no
datapack: collision geometry is code, not data. They come from
`lodestone_data::snow_support` (four columns) plus
`lodestone_data::block_solidity::blocks_motion`, all dumped from the real 26.2
server by `crates/lodestone-data/oracle-java/SnowSupportOracle.java`.

Dumping rather than deriving was not caution. On first run the dump contradicted
four separate hand guesses:

| guess | measured |
|---|---|
| "most blocks are full cubes, so `isFaceFull(UP)` is common" | **6,359 of 32,366** states, a minority — and 159 blocks are full-faced *without* being a unit box, so a derivation from `collision_shapes` would refuse snow on every one |
| "water freezes" | true for **exactly one state**, `water[level=0]`: flowing water is `Fluids.FLOWING_WATER` and a waterlogged block is not a `LiquidBlock` |
| "`snow[layers=8]` has a full top face" | **false for all eight snow states** — a full snow layer is 14/16 tall, which is why `canSurvive` carries an explicit `layers == 8` clause the geometry never satisfies |
| "`chorus_plant` is the `dynamicShape()` block to worry about" | it is not `dynamicShape()` at all; **`powder_snow`** is, and it is the one such block worldgen exposes at a surface top |

Both snow-support tags are load-bearing, in **opposite** directions:
`ice`/`packed_ice` genuinely *have* a full UP face and are kept snow-free only by
`#cannot_support_snow_layer`, while `mud`/`honey_block`/`soul_sand` have *no* full
UP face and support snow only via `#support_override_snow_layer`. The tag checks
therefore have to run before the geometry check, or every frozen ocean gets a
snow blanket on its ice.

### Two `±1`s that cancel

The height the feature reads is the product of two cancelling offsets: `Heightmap`
stores `topMatchingY + 1` (`Heightmap.java:64`), `getHighestTaken` subtracts one
(`Heightmap.java:114`) and `ChunkAccess.getHeight` routes to it, then
`WorldGenRegion.getHeight` adds it back (`WorldGenRegion.java:435`). Net: the
feature receives the first *free* Y. Dropping either offset puts **every** snow
layer one block out, so `motion_blocking_heightmap_matches_vanilla_per_column`
gates the heightmap separately from the writes — 1,024 columns compared against
vanilla's own `getHeight`.

### Block light is not modelled, and does not need to be

Both predicates gate on `level.getBrightness(LightLayer.BLOCK, pos) < 10`. At the
`features` chunk status that value is **always 0**: `initialize_light` runs
strictly after `features` (`ChunkStatus.java:28-30`) and
`BlockLightSectionStorage.getLightValue` returns `0` for a section with no
`DataLayer` (`BlockLightSectionStorage.java:16-26`). So the gate is
unconditionally satisfied in vanilla too. **This is agreement, not a shortcut —
do not "improve" it by supplying real light.**

## How to change it

Files, in the order a change usually touches them:

- `crates/lodestone-worldgen/src/feature/top_layer.rs` — the engine. Predicates,
  the temperature port, `canSurvive`, `SnowSupport`, `FreezeCounts`.
- `crates/lodestone-worldgen/src/noise/perlin_simplex.rs` — the multi-octave
  `PerlinSimplexNoise` and `ClimateNoise` (`TEMPERATURE_NOISE` seed 1234 octaves
  `[0]`; `FROZEN_TEMPERATURE_NOISE` seed 3456 octaves `[-2,-1,0]`;
  `BIOME_INFO_NOISE` seed 2345 octaves `[0]`). Scoped to non-positive octaves and
  **panics** rather than silently mis-scaling outside that.
- `crates/lodestone-worldgen/src/overworld.rs` — `top_layer_stage`, the
  `column()` hook, and the `biome_climates`/`freeze_biomes`/`snow_support`/
  `climate_noise` fields.
- `crates/lodestone-worldgen/src/compose.rs` — `biome_lists_freeze_top_layer`.
- `crates/lodestone-server/src/worldgen_data.rs` — `block_freeze_facts`'s
  implementation, plus the parity gate and both timing/sweep reports.
- `crates/lodestone-data/{src/snow_support.rs, oracle-java/SnowSupportOracle.java}`
  — the jar-dumped facts.
- `scripts/worldgen-oracle/TopLayerOracle.java` — the parity oracle.

### Gotchas

- **`column_timed` is a second copy of the pipeline.** It is not a wrapper around
  `column()`; it re-lists the stages so it can time them. Adding a stage to
  `column()` and forgetting `column_timed` makes the timed path silently diverge
  from the real one — which happened once during this unit's own work and was
  caught only because the new `StageTimes.top_layer` field would have read zero.
- **The oracle's proxy must throw, never default.** `TopLayerOracle`'s
  `WorldGenLevel` dynamic proxy raises `UnsupportedOperationException` for any
  method it does not model. That is the fix for the precedent that cost this repo
  a whole vegetation gate: `VegetationOracle`'s proxy lacked `isStateAtPosition`,
  its default arm returned `false`, and **no tree ever placed a block through it**
  while the harness reported success. The throwing arm fired three times for real
  while `TopLayerOracle` was written, and all three are still latent in
  `VegetationOracle`: `getRandom()` (returns `null` there, so every tree's
  `updateShapeAtEdge` gets a null `RandomSource`), `scheduleTick` (waterlogged
  leaves), and `getRawBrightness` (mushroom survival). Worth a follow-up on the
  vegetation oracle.
- **`chunk.setPersistedStatus(ChunkStatus.FEATURES)` is required in the oracle**
  before capturing the heightmap. Left at `EMPTY`, only the `*_WG` heightmaps are
  maintained, so a grass block placed by `VEGETAL_DECORATION` does not raise
  `MOTION_BLOCKING` — and this feature's entire input is
  `getHeight(MOTION_BLOCKING, x, z)`.
- **A snowy-biome fixture cannot detect the flat-temperature error.**
  `snowy_plains` (`temperature = 0.0`) snows at every altitude under both
  readings. Any new fixture set must keep a biome whose declared temperature is
  between `0.15` and roughly `0.5`.
- **Do not threshold on altitude.** In the `windswept_hills` fixture the snowed
  and bare columns' heights **fully overlap** (119–125 for both). The
  `TEMPERATURE_NOISE` term contributes `±8` against a `(y − 120) / 800` altitude
  term, so the step produces a *speckle* at every height, not a snow line.

## Gates, and their observed controls

All in `crates/lodestone-server/src/worldgen_data.rs`'s `top_layer_parity`
module. The gate loads vanilla's own post-vegetation field from the fixture, runs
our engine on it, and requires the **same writes at the same coordinates with the
same block states** — nothing upstream is involved, so a residual can only be
this step's.

| fixture | biome | vanilla | what it can catch |
|---|---|---|---|
| `snowy_plains` (-1200,-2400) | temp 0.0 | 250 snow, 250 `snowy` flips | the ordinary path and the flip |
| `frozen_ocean` (-600,0) | temp 0.0, `frozen` | 36 ice, **0 snow** | the ice-before-snow write order; the `FROZEN` modifier |
| `windswept_hills` (0,240) | temp 0.2 | 115 snow of 256 columns | **the height-adjusted temperature** |
| `desert` (-160,-240) | temp 2.0, no precipitation | **0 cells** | the negative fixture |

Four controls, all **run and observed**, not described:

1. **Flat temperature.** Pushing `sea_level` above the build limit makes the
   height-adjustment branch unreachable, reproducing the trap without touching
   the engine. `windswept_hills` drops from 115 snow cells to **0** and the parity
   assertion fails; `snowy_plains` is **unchanged**, which is the entire argument
   for the `windswept_hills` fixture existing.
2. **`#cannot_support_snow_layer` emptied.** `frozen_ocean`'s ice becomes
   snow-covered and parity fails — so the tag, not geometry, is what keeps frozen
   oceans bare.
3. **Step disabled.** An empty diff must not match any non-empty fixture.
4. **`SNOW_LAYER` neutered to `layers=2`** (a two-minute manual neuter, restored
   from a scratchpad copy with an md5 check). Observed: `250 vanilla cells
   wrong/absent, 0 extra`, naming `(0,69,0) minecraft:snow[layers=1]` vs ours
   `[layers=2]`. This is the only control that proves the *block state*, not just
   the position, is compared.

Plus the desert fixture's zero has an independent non-vacuity proof: its
`meta.proxyCall` list never reaches `getBrightness` **or** `isInsideBuildHeight`
— the two methods whose wrong default could manufacture a false zero — because
`has_precipitation: false` short-circuits first, and step 10 still reports
`placedTrue=1`, so the feature ran and wrote nothing.

## Performance

**The first release-profile figure for the composed pipeline.** Every number
previously on file for it (`docs/plans/worldgen-parity.md` §6: the 144-chunk
sweep at ~68 s pre-ore and 700.57 s after) is **debug** profile.

Measured by `freeze_stage_release_timing` over 8 chunks (snowy, frozen and warm),
from `StageTimes.top_layer`:

| | release |
|---|---|
| composed `column()` | **853.5 ms/chunk** |
| `top_layer` stage | **2.196 ms/chunk** |
| share | **0.257 %** |

That is comfortably inside §6's `<5 %` prediction for a new decoration step, and
the test asserts the ceiling so a regression fails rather than being absorbed.
The specific shape to guard against is a per-column `ClimateNoise::new()` — about
780 RNG draws per column instead of per generator.

## Configuration

- `LODESTONE_REGEN=1` — regenerate `lodestone-data`'s committed snow-support
  table (`just regen-snow-support`).
- `LODESTONE_FREEZE_DISABLE_DEBUG` — skip the stage entirely. Debug-only escape
  hatch, mirroring `LODESTONE_ORE_SINGLE_SOURCE_DEBUG` /
  `LODESTONE_VEG_SINGLE_SOURCE_DEBUG`; never on a production path. Note a timing
  A/B built on it must construct a **fresh generator per arm**, because
  `pre_ore_cache`/`post_ore_cache` are per-generator and would make the second arm
  recompute nothing.
- `just oracle-snow-support`, `just regen-snow-support`, `just oracle-top-layer`
  — re-dump the block facts, regenerate the table, re-dump the four parity
  fixtures. Oracles need Apple `container` (`docs/oracle-runtimes.md`).
- `SWEEP_SEED` / `SWEEP_EXTENT` / `SWEEP_STEP` — the `freeze_coordinate_sweep`
  report's range.

## Dependencies

`lodestone-worldgen` (engine, `Resolver` seam), `lodestone-data`
(`snow_support`, `block_solidity`, `block_states`), `lodestone-server`
(`worldgen_data`'s `block_freeze_facts` and the gates),
`scripts/worldgen-oracle/TopLayerOracle.java` +
`.cache/mc/26.2/{src,versions/26.2/server-26.2.jar}`. Issues #404 (epic),
#405/#295/#406/#427 (the stages this composes after).
