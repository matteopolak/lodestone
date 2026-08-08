# Nether and End worldgen

## What it is

The bundled 26.2 data for the Nether and the End, and the precise engine deficit
that still stands between that data and a generating dimension. Only the
Overworld generates today. This doc is the jar-derived answer to "what is
actually missing", per dimension, with each item classed **[data]** (absent from
the bundle), **[engine]** (absent engine primitive), **[unwritten]** (nothing
blocks it), **[gameplay]** (cannot finish even with perfect worldgen) or
**[structures]** (belongs to the structure corpus, not terrain). Phase NE-data
of [`plans/worldgen-rewrite.md`](./plans/worldgen-rewrite.md); the data half is
landed, the engine half is not started.

**The headline: after this phase there is no `[data]` item left for either
dimension.** Every remaining gap is engine or gameplay. That is a narrower
deficit than the plan's inventory reads, and the reason is that the biome,
feature and density-function corpora were already complete.

## Data: what is bundled, measured

Counts re-measured against the jar on 2026-08-07, not inherited — a
non-recursive `ls` of `density_function/` reports 9 because the files sit under
`overworld/`, `nether/`, `end/`, `overworld_amplified/` and
`overworld_large_biomes/`, and that is how the count gets misreported.

| registry | jar | bundled | note |
|---|---|---|---|
| `biome` | 66 | 66 | complete; all five Nether and all five End biomes included |
| `configured_feature` / `placed_feature` | 226 / 262 | 226 / 262 | complete |
| `density_function` | 35 | 35 | complete, byte-identical all 35 (re-diffed this phase) |
| `configured_carver` | 4 | 4 | complete |
| `noise` | 63 | **63** | was 61; this phase added the 2 below |
| `noise_settings` | 7 | **7** | was 1; this phase added `nether`, `end` |
| `multi_noise_...parameter_list` | 2 | **2** | this phase added `nether` |
| `tags` | — | 261 | |

The two noises that were absent, identified by set difference against the jar
rather than guessed:

* `minecraft:nether/temperature`
* `minecraft:nether/vegetation`

Both are 63-byte documents (`firstOctave -7`, amplitudes `[1.0, 1.0]`) and both
are reached only from `noise_settings/nether.json`. Nothing in the tree noticed
they were missing because no code loads `nether.json` yet — the island shape, in
data rather than code.

**Transitive reference closure** of `nether.json` + `end.json`, followed to
fixpoint: 3 density functions (`nether/base_3d_noise`, `end/sloped_cheese`, and
`end/base_3d_noise` reached through it), 8 noises, 5 surface-rule biomes. Every
one except the two above was already bundled.

### The parameter list is not a file copy

The jar's `multi_noise_biome_source_parameter_list/nether.json` is **37 bytes**:
`{"preset": "minecraft:nether"}`. So is `overworld.json`. The codec only ever
serialises the preset id (`MultiNoiseBiomeSourceParameterList.java:24-30`) and
the table is Java-hardcoded (`:51-67`). There is nothing to copy, so
`biome_parameters/nether.json` comes from
`scripts/worldgen-oracle/NetherParametersOracle.java`, which reads
`MultiNoiseBiomeSourceParameterList.knownPresets()` — public, and needing no
registry resolution because the values are plain identity-mapped
`ResourceKey<Biome>`. Five rows:

| biome | temperature | humidity | offset |
|---|---|---|---|
| `nether_wastes` | 0 | 0 | 0 |
| `soul_sand_valley` | 0 | −5000 | 0 |
| `crimson_forest` | 4000 | 0 | 0 |
| `warped_forest` | 0 | 5000 | 3750 |
| `basalt_deltas` | −5000 | 0 | 1750 |

Quantized by 10000 (`Climate.java:27`). Every channel is a **degenerate point**
— the Nether list never uses `Parameter.span`, unlike the Overworld's 7,594
rows — and continentalness/erosion/depth/weirdness are all zero because the
Nether router zeroes those channels (`NoiseRouterData.java:390-411`). The
consequence is worth stating plainly: **temperature and humidity are the entire
Nether biome layout**, so the two noises above are not a detail, they are the
map.

**`biome_parameters/overworld_temperature.json` is not from this registry** —
the registry has only two entries, and neither is it. Treat its provenance as
unverified; this phase did not touch it.

## The engine deficit

### Shared by both dimensions

> **Items 1–3 landed.** `crate::rng::Algorithm` (`rng/any.rs`) is
> `WorldgenRandom.Algorithm`, and `AnyRandomSource`/`AnyPositionalFactory` are
> the two-variant enums it produces. `density::Builder::with_algorithm(seed,
> algorithm, resolver)` is the dimension-aware constructor;
> `Builder::new` is now that call with `Algorithm::Xoroshiro`, so the Overworld
> is unchanged (230 worldgen tests, including `region_parity`/`density_parity`/
> `noise_parity` against the JVM dumps, are byte-identical across the change).
> `Builder::positional_factory()` returns `AnyPositionalFactory`, which is
> `Copy` because both variants are — that is what kept the ripple to a type name
> in four files (`surface/mod.rs`, `aquifer/mod.rs`, `overworld/fill.rs`,
> `overworld/veins.rs`) instead of a generic parameter through every stage
> struct. The bespoke Nether noises and the legacy `BlendedNoise` arm are in
> `Builder::instantiate_noise` / `instantiate_blended`; see
> [`worldgen-nether.md`](./worldgen-nether.md).

**1. `legacy_random_source: true`** — [engine], and the largest item.

Both `nether.json` and `end.json` set it; `overworld.json` does not. Vanilla
switches the *entire* noise stack on it:

```java
// RandomState.java:36
this.random = settings.getRandomSource().newInstance(seed).forkPositional();
// NoiseGeneratorSettings.java:73-75
return this.useLegacyRandomSource ? WorldgenRandom.Algorithm.LEGACY
                                  : WorldgenRandom.Algorithm.XOROSHIRO;
```

**This is a wiring gap, not a missing implementation, and the distinction sets
the size of the job.** Say it precisely, because the loose version ("the legacy
RNG is unimplemented") is false and was in an earlier revision of this doc:

* `LegacyRandomSource` is **fully implemented and in production use** —
  `rng/legacy.rs:16` (struct), `:41` (`impl RandomSource`), `:116`
  (`consume_count`), `:125` `LegacyPositionalFactory`, `:137`
  (`impl PositionalRandomFactory`). Live callers: `noise/perlin_simplex.rs` (14
  references), `noise/simplex.rs` (3), `feature/vegetation.rs` (5),
  `carver/mod.rs` (3).
* `consume_count` is on the `RandomSource` **trait** (`rng/mod.rs:78`), not a
  concrete-type convenience, and is implemented for both algorithms
  (`legacy.rs:116`, `xoroshiro.rs:166`).
* What is missing is that **the flag is read nowhere**: `legacy_random_source` has
  **zero occurrences under `crates/lodestone-worldgen/`**. (Tree-wide it has 3, all
  of them in this phase's own
  `crates/lodestone-data/tests/worldgen_dimension_data.rs` — so quote the engine
  scope, not "tree-wide", or the number looks like progress.)
* `density::Builder::new` hardcodes the other branch at `density/mod.rs:708`:
  `XoroshiroRandomSource::new(seed).fork_positional()`.
* **Why it is more than a one-liner**: `Builder`'s `master` field is the
  *concrete* type `crate::rng::XoroshiroPositionalFactory` (`density/mod.rs:700`),
  and `Builder::positional_factory` (`:750`) *returns* that concrete type too. So
  there are **two** sites to make polymorphic (an enum or a boxed trait object),
  not one, and the accessor's return type is part of `density/`'s public surface.

Not cosmetic: it changes every noise value in both dimensions, so no amount of
correct data produces correct terrain until it lands. Bounded, though — the change
is confined to `density/` plus threading the flag from `noise_settings`. Note
`density/` sits inside the proposed `lodestone-worldgen-core` leaf crate and is
U4's rewrite target, so sequencing matters.

**2. The two Nether noises need bespoke instantiation** — [engine].

They are special-cased in `RandomState.java:55-61`, which is *why* they are the
only two noises the bundle lacked:

```java
if (noiseData.is(Noises.TEMPERATURE_NETHER)) {
   NormalNoise newNoise = NormalNoise.createLegacyNetherBiome(this.newLegacyInstance(0L), noiseData.value());
} else if (noiseData.is(Noises.VEGETATION_NETHER)) {
   NormalNoise newNoise = NormalNoise.createLegacyNetherBiome(this.newLegacyInstance(1L), noiseData.value());
}
```

`newLegacyInstance(n)` is `new LegacyRandomSource(seed + n)` (`:50-52`) — the raw
world seed plus 0 and 1, *not* a positional fork. `createLegacyNetherBiome` is
`new NormalNoise(random, parameters, useNewInitialization = false)`
(`NormalNoise.java:26-28`). Our `PerlinNoise::new_legacy` exists but is private
and reachable only via `create_legacy_for_blended_noise`; there is no
`NormalNoise` legacy-init path.

**3. `BlendedNoise` under legacy init** — [engine, small].

`RandomState.java:70-73`: `useLegacyInit ? newLegacyInstance(0L) :
random.fromHashOf("terrain")`. Both dimensions use `old_blended_noise`, so both
take the legacy arm. Falls out of item 1.

**4. `aquifers_enabled: false`** — [engine, small].

`NoiseChunk.java:145-152` picks `Aquifer.createDisabled(globalFluidPicker)`, which
is trivial (`Aquifer.java:30-41`): solid where `density > 0`, else
`globalFluid.at(y)`. Our aquifer always runs — `aquifers_enabled` has zero
occurrences in the engine. **The entire `NoiseBasedAquifer` machinery is
Overworld-only**, so this is a bypass to add, not logic to port.

**5. Cell geometry is hardcoded** — [engine, small], **End only**.

Vanilla derives cell size as `QuartPos.toBlock(noiseSize*)` = `size * 4`
(`NoiseSettings.java:46-52`). The Overworld's `size_horizontal 1, size_vertical 2`
gives the familiar 4-wide/8-tall cell; **the End's `2, 1` gives an 8-wide/4-tall
cell**, and the Nether matches the Overworld. `CELL_WIDTH`/`CELL_HEIGHT` are
`const 4`/`const 8` (`aquifer/mod.rs:44-45`), and `size_horizontal` has zero
occurrences in the engine. `NoiseChunkSampler::new` already *takes* cell
dimensions as parameters, so this is plumbing, not a missing primitive.

### Nether

| item | class | note |
|---|---|---|
| noise settings, parameter list, 2 noises | ~~[data]~~ | **landed this phase** |
| 5 biomes (wastes, soul sand valley, crimson, warped, basalt deltas) | — | already bundled, 66/66 |
| legacy RNG + bespoke nether noises | [engine] | items 1–3 above |
| aquifer bypass | [engine, small] | item 4 |
| multi-noise biome source | **no gap** | see below |
| surface rules | [unwritten] | uses a strict *subset* of Overworld condition types |
| fortress, bastion, nether fossil, ruined portal | [structures] | sibling's group S |
| dimension registry / portal travel | [gameplay] | #330; the generator is oracle-testable without it |

**The lava "sea" is not an aquifer.** `sea_level` is **32** (Overworld 63) and
`default_fluid` is **lava**, but `aquifers_enabled` is **false**, so the lava
comes from the global fluid picker. The `-54` constant lives in the chunk
generator, not `Aquifer.java` (`NoiseBasedChunkGenerator.java:68-80`):

```java
Aquifer.FluidStatus lavaStatus = new Aquifer.FluidStatus(-54, Blocks.LAVA.defaultBlockState());
Aquifer.FluidStatus seaStatus  = new Aquifer.FluidStatus(settings.seaLevel(), settings.defaultFluid());
return (x, y, z) -> y < Math.min(-54, seaLevel) ? lavaStatus : seaStatus;
```

For the Nether `min(-54, 32) = -54`, and the Nether's noise range is `min_y 0,
height 128`, so the `y < -54` branch is **unreachable**. Everything resolves to
`FluidStatus(32, LAVA)` → lava below y=32, air above. The `-54` lava sea is an
*Overworld* feature (deep-dark lava below y=−54); the Nether's coincidental
agreement on the fluid type is not the same mechanism, and modelling the Nether
as "an aquifer whose second fluid is lava" would be wrong.

**Surface rules need no new condition type.** `SurfaceRuleData.nether()`
(`SurfaceRuleData.java:300-387`) uses `stone_depth` (including
`CaveSurface.CEILING` for the `UNDER_CEILING` ceiling rules), `y_above` in both
`yBlockCheck` and `yStartCheck` forms, `not`, `hole`, 2-D `noise_threshold`,
`vertical_gradient` and `biome` — every one of which the Overworld also uses. The
Overworld additionally uses `water`, `temperature`, `steep`,
`above_preliminary_surface` and the `bandlands` rule, so the Nether is a strict
subset. Bedrock floor is `verticalGradient("bedrock_floor", bottom(),
aboveBottom(5))` (y 0–4) and the **ceiling** is `not(verticalGradient(
"bedrock_roof", belowTop(5), top()))` (y 123–127), both hardcoded at `:317-318`
rather than flag-gated as in the Overworld.

**The multi-noise biome source needs nothing.** `MultiNoiseBiomeSource.getNoiseBiome`
is a straight `parameters().findValue(sampler.sample(...))`
(`MultiNoiseBiomeSource.java:59-67`), which we implement. One thing was worth
checking and came back clean: vanilla's `fitness` adds `Mth.square(this.offset)`
as a flat penalty rather than a distance (`Climate.java:231-238`), while
`biome.rs:155` loops all seven channels through `Parameter::distance`. For a real
climate sample the target's offset channel is always 0, and `distance(0)` against
a degenerate point equals `|offset|` for either sign — so the two formulations
agree exactly and **no metric change is needed**.

### End

| item | class | note |
|---|---|---|
| `noise_settings/end.json` | ~~[data]~~ | **landed this phase** |
| 5 biomes, all 6 End features, biome step wiring | — | already bundled |
| `minecraft:end_islands` density function | [engine] | the one missing DF type; algorithm below |
| `TheEndBiomeSource` | [engine, small] | ~20 lines; see below |
| legacy RNG, aquifer bypass, 8-wide cell | [engine] | items 1, 4, 5 above |
| obsidian pillars, chorus plants, return gateways, end platform | [unwritten] | features; data complete |
| end city | [structures] | sibling's group S |
| exit portal / podium, post-dragon gateways, platform re-place | [gameplay] | not worldgen at all |
| dragon fight and respawn | [gameplay] | confirmed, not scoped here |

**`TheEndBiomeSource` is not multi-noise** (`TheEndBiomeSource.java:60-81`) and
serialises to an empty object — its five biome holders come from the registry,
not JSON (`:14-23`). The logic is a pure function of position plus one density
sample:

```java
if ((long)chunkX * chunkX + (long)chunkZ * chunkZ <= 4096L) return this.end;
int weirdBlockX = (SectionPos.blockToSectionCoord(blockX) * 2 + 1) * 8;   // == chunkX*16 + 8
double heightValue = sampler.erosion().compute(new SinglePointContext(weirdBlockX, blockY, weirdBlockZ));
if (heightValue >  0.25)    return this.highlands;
if (heightValue >= -0.0625) return this.midlands;
return heightValue < -0.21875 ? this.islands : this.barrens;
```

It samples the **`erosion` slot**, which for the End router holds
`cache2d(end_islands)` (`NoiseRouterData.java:433,443`) — every other channel
except `finalDensity` is `zero()`. Requires no mutable state; `cache2d` is an
optimisation, not semantics. Inside chunk radius 64 (`chunkX² + chunkZ² <= 4096`)
it always returns `the_end`, matching the `> 4096L` gate inside the density
function.

**Every End feature is already bundled** — `end_platform`, `end_spike`,
`end_gateway_return`, `chorus_plant`, `end_island` all have configured *and*
placed documents, and the biome documents already carry the step wiring
(`the_end`: step 4 `end_spike`, step 10 `end_platform`; `end_highlands`: step 4
`end_gateway_return`, step 9 `chorus_plant`; `small_end_islands`: step 0
`end_island_decorated`). So End decoration is **[unwritten] step work with zero
data missing**.

Corroborating the gameplay split from data alone: `end_gateway_delayed` has a
configured feature but **no placed feature**, because nothing in worldgen places
it — `EnderDragonFight.spawnNewGateway()` does
(`EnderDragonFight.java:423-441`).

**What is gameplay, not worldgen** — this is where a plan inflates if nobody
checks:

* **The exit portal / `EndPodiumFeature`** is a `Feature` subclass that is
  **never registered** in `Feature.java` or `EndFeatures.java`. Only
  `EnderDragonFight.spawnExitPortal(boolean)` instantiates it
  (`EnderDragonFight.java:443-460`). It looks like worldgen and is not.
* **The obsidian pillars have two placers.** Worldgen places them via the
  `end_spike` feature; `DragonRespawnStage.java:60-79` re-places them during the
  respawn sequence with `crystalInvulnerable = true`. The worldgen half is
  [unwritten], the respawn half is [gameplay].
* **The end platform likewise.** Worldgen places it at a fixed `(100, 49, 0)`
  (`ServerLevel.END_SPAWN_POINT` is `(100, 50, 0)`), and `EndPortalBlock.java:90`
  re-creates it on every entry into the End.
* **Gateways have three paths**: the worldgen `end_gateway_return` (rarity 700 in
  `end_highlands`) is ours; `EnderDragonFight.spawnNewGateway` and
  `TheEndGatewayBlockEntity.findOrCreateValidTeleportPos` are both [gameplay].
* **The dragon fight and respawn mechanics** are [gameplay], as the plan already
  ruled. Not scoped here.

**End city is a structure, not terrain** — a real `Structure`
(`EndCityStructure.java:14-45`, `StructureType.java:26`), and notably
**template-piece based rather than jigsaw**: `findGenerationPoint` picks a
rotation, uses `getLowestYIn5by5BoxOffset7Blocks`, rejects `y < 60`, and delegates
to `EndCityPieces.startHouseTower`. It belongs to the structure group's S2 phase,
not S4.

## `minecraft:end_islands` — the algorithm

The only density-function type the engine lacks. It appears **twice**, not once
as the plan's inventory says: inline in `noise_settings/end.json` *and* inside the
already-bundled `density_function/end/sloped_cheese.json`. Anyone implementing it
must handle both sites.

It is a `SimpleFunction` — no children, no arguments. The codec is
`MapCodec.unit(new EndIslandDensityFunction(0L))` (`DensityFunctions.java:493-495`),
so the JSON is literally `{"type": "minecraft:end_islands"}` and always
deserialises with seed 0. **The seed is substituted at runtime**
(`RandomState.java:74`):

```java
return function instanceof DensityFunctions.EndIslandDensityFunction
    ? new DensityFunctions.EndIslandDensityFunction(seed) : function;
```

where `seed` is the raw world seed. Construction (`DensityFunctions.java:496-503`):

```java
private static final float ISLAND_THRESHOLD = -0.9F;
public EndIslandDensityFunction(final long seed) {
   RandomSource islandRandom = new LegacyRandomSource(seed);
   islandRandom.consumeCount(17292);
   this.islandNoise = new SimplexNoise(islandRandom);
}
```

`consumeCount(17292)` is 17,292 `nextInt()` calls. `SimplexNoise(RandomSource)`
then consumes three `nextDouble()` for `xo/yo/zo` (unused on the 2-D path but
they consume randomness) and builds the 256-entry permutation by Fisher–Yates
with `nextInt(256 - i)` (`SimplexNoise.java:33-48`).

The height field (`DensityFunctions.java:505-529`):

```java
private static float getHeightValue(final SimplexNoise islandNoise, final int sectionX, final int sectionZ) {
   int chunkX = sectionX / 2;
   int chunkZ = sectionZ / 2;
   int subSectionX = sectionX % 2;
   int subSectionZ = sectionZ % 2;
   float doffs = 100.0F - Mth.sqrt(sectionX * sectionX + sectionZ * sectionZ) * 8.0F;
   doffs = Mth.clamp(doffs, -100.0F, 80.0F);
   for (int xo = -12; xo <= 12; xo++) {
      for (int zo = -12; zo <= 12; zo++) {
         long totalChunkX = chunkX + xo;
         long totalChunkZ = chunkZ + zo;
         if (totalChunkX * totalChunkX + totalChunkZ * totalChunkZ > 4096L
             && islandNoise.getValue(totalChunkX, totalChunkZ) < -0.9F) {
            float islandSize = (Mth.abs((float)totalChunkX) * 3439.0F + Mth.abs((float)totalChunkZ) * 147.0F) % 13.0F + 9.0F;
            float xd = subSectionX - xo * 2;
            float zd = subSectionZ - zo * 2;
            float newDoffs = 100.0F - Mth.sqrt(xd * xd + zd * zd) * islandSize;
            newDoffs = Mth.clamp(newDoffs, -100.0F, 80.0F);
            doffs = Math.max(doffs, newDoffs);
         }
      }
   }
   return doffs;
}
```

```java
public double compute(final FunctionContext context) {
   return (getHeightValue(this.islandNoise, context.blockX() / 8, context.blockZ() / 8) - 8.0) / 128.0;
}
public double minValue() { return -0.84375; }   // (-100 - 8) / 128
public double maxValue() { return  0.5625;  }   // ( 80 - 8) / 128
```

**Porting traps**, all of which would produce a plausible-looking but wrong End:

* `sectionX / 2`, `sectionX % 2` and `blockX() / 8` are **Java truncating**
  integer division, not floor-div. For negative coordinates
  `subSectionX ∈ {−1, 0, 1}`. Rust's `/` and `%` truncate the same way, so use
  them directly and do **not** reach for `div_euclid`.
* `sectionX * sectionX + sectionZ * sectionZ` is computed in **`int`** before
  widening for a **`float`** sqrt.
* `islandSize` is a *slope*, not a radius; range `[9, 22)`.
* All of `islandSize`/`xd`/`zd`/`newDoffs` is **`f32`**, not `f64`.
* Loop bounds are `-12..=12` on both axes — 625 candidate chunks per call, which
  is why the router wraps it in `cache2d`.
* The centre hole is `totalChunkX² + totalChunkZ² > 4096` in **long** arithmetic:
  chunks within radius 64 never spawn an island, and that region is the main
  island's plateau produced by the first `doffs` term.
* `islandNoise.getValue` is sampled at **integer chunk coordinates**, and
  `SimplexNoise`'s 2-D path calls the 3-D corner routine with `z = 0.0` and base
  `0.5`, output scaled by `70.0` (`SimplexNoise.java:73-104`).

Wiring, for whoever implements it: `NoiseRouterData.java:127` is
`end/sloped_cheese = add(endIslands(0L), BASE_3D_NOISE_END)`, and `:432-452` puts
`cache2d(endIslands(0L))` in the router's **erosion** slot — which is exactly
where `TheEndBiomeSource` reads it from.

### When to implement it

**Not yet.** U4 of the rewrite plan replaces the boxed-enum density interpreter
and will port every DF type; a version written against today's interpreter is
thrown away, and the traps above are the kind that get re-introduced by a
port-of-a-port. `crates/lodestone-worldgen/**` also has a live owner.

**The reason is U4's rewrite, and nothing else.** An earlier revision of this doc
also argued it was "untestable in isolation right now" because the legacy RNG was
missing. **That was wrong, and the correction matters more than the claim did**:
every primitive `end_islands` needs already exists and is in production use —
`LegacyRandomSource` (`rng/legacy.rs:16`, `impl RandomSource` at `:41`),
`consume_count` on the `RandomSource` *trait* (`rng/mod.rs:78`, legacy impl at
`legacy.rs:116`), and `SimplexNoise::new<R: RandomSource>` (`noise/simplex.rs:59`),
which is generic and so takes a `LegacyRandomSource` directly.

Crucially, `EndIslandDensityFunction`'s seeding **does not consult
`legacy_random_source` at all** — it always constructs `new
LegacyRandomSource(seed)` (`DensityFunctions.java:498`) regardless of the setting.
So the density function is independently constructible and gate-able **today**,
against a `DensityOracle` dump at known coordinates. Item 1 is not a prerequisite
of testing it; the two are independent.

What remains is the ordinary argument: U4 replaces the boxed-enum interpreter, a
version written against today's one is thrown away, and the traps above are
exactly the kind that get re-introduced by a port-of-a-port. Sequence it inside
U4. If a reason ever appears to want it sooner, that is now a schedule question
with no technical blocker behind it.

## How to change it

* **Data refresh after a version bump**: the four jar-copied documents are
  re-extracted by the `#[ignore]`d gate in
  `crates/lodestone-data/tests/worldgen_dimension_data.rs`:

  ```text
  LODESTONE_REGEN=1 cargo test -p lodestone-data --test worldgen_dimension_data \
      bundled_dimension_files_match_the_jar -- --ignored --nocapture
  ```

  The Nether parameter table is *not* one of them — it has no jar entry to copy
  and is refreshed from the oracle instead:

  ```text
  LODESTONE_REGEN=1 cargo test -p lodestone-data --test worldgen_dimension_data \
      nether_biome_parameters_match_the_jvm_oracle -- --ignored --nocapture
  ```

* **Adding another dimension's data**: add it to `DIMENSION_FILES` and
  `DIMENSION_SETTINGS` in that test. The reference-closure gate then covers it
  automatically, which is the point — a dangling reference is the defect this
  phase existed to fix, and it was invisible because no code loaded the document.

* **Gotcha, if you write another oracle**: do not write progress output to
  `System.err` to keep stdout clean. `Bootstrap.bootStrap()` installs log4j over
  `System.err` and the line comes back out on **stdout** as
  `[..] [main/INFO]: [STDERR]: ...`, corrupting the document. Measured — the
  first run of `NetherParametersOracle` did exactly that.

* **Gotcha, on the scalar pins**: `dimension_settings_carry_the_engine_relevant_scalars`
  pins `sea_level`, `aquifers_enabled`, `legacy_random_source`,
  `ore_veins_enabled`, `default_fluid` and the derived cell geometry for all
  three dimensions, with the Overworld row as the control. Those values are what
  this doc's engine deficit is *derived from*. If a data bump moves one, that
  test fails rather than this doc quietly becoming wrong.

## Configuration

No runtime configuration. The data is embedded at build time by
`crates/lodestone-server/build.rs`, which walks `assets/worldgen/` and keys each
document by its bundle-relative path without the extension (so
`noise_settings/nether`, `noise/nether/temperature`,
`biome_parameters/nether`). `EmbeddedResolver` in
`crates/lodestone-server/src/worldgen_data.rs` is the read side; it currently
hardcodes the Overworld documents, so **none of the new files are read by
anything yet** — deliberately, since the engine cannot consume them.

## Dependencies

* **The 26.2 server jar** at `.cache/mc/26.2/versions/26.2/server-26.2.jar` —
  gitignored, which is why both provenance gates are `#[ignore]`d.
* **`scripts/worldgen-oracle/run.sh`** and Apple `container` for the parameter
  table; it uses an ephemeral `eclipse-temurin:25-jdk` image, so **no JDK on
  `PATH` is required**. See [`oracle-runtimes.md`](./oracle-runtimes.md).
* `lodestone_worldgen::biome::parse_table` defines the 14-column parameter-table
  shape the oracle emits.
* Sibling docs: [`worldgen-parity.md`](./worldgen-parity.md),
  [`worldgen-biomes.md`](./worldgen-biomes.md),
  [`plans/worldgen-rewrite.md`](./plans/worldgen-rewrite.md).
