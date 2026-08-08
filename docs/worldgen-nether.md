# Nether worldgen

## What it is

The composed Nether generator — `lodestone_worldgen::nether::NetherGenerator` —
and the engine work it needed: a selectable RNG family, the two bespoke Nether
biome noises, a disabled-aquifer fill, and the `minecraft:nether_cave` carver. It
is the second dimension this engine produces real terrain for, and it is verified
against a real Mojang server's own Nether, not against itself.

## How it works

`NetherGenerator::new(seed, settings, resolver)` once per world;
`column(cx, cz)` per chunk. `column` runs vanilla's own order:

| stage | what | source |
|---|---|---|
| starts | `createStructures` over the sets this dimension can place | `ChunkGenerator.java`, `ChunkGeneratorStructureState.java:52-67` |
| refs | `createReferences`' 17×17 walk, widened to the beardifier's reach | `ChunkGenerator.createReferences` |
| beard | `Beardifier.forStructuresInChunk` — empty today, see below | `Beardifier.java` |
| fill | `Aquifer.createDisabled` over the interpolated `final_density` | `Aquifer.java:30-41` |
| biome | one climate sample per horizontal quart → the 5-row parameter list | `MultiNoiseBiomeSource.java:59-67` |
| surface | `SurfaceRuleData.nether()`, unchanged engine | `SurfaceRuleData.java:300-387` |
| carve | `applyCarvers` with `NetherWorldCarver` | `NetherWorldCarver.java` |
| place | every piece intersecting this chunk, `surface_structures` slot | `StructureStart.placeInChunk` |

The only thing that memoises across chunks is the starts map (a pure function of
`(seed, cx, cz)`, so eviction can only cost time), and no stage reads a
neighbour's *terrain* product, so `column` is a pure function of `(seed, cx, cz)`
and the join scheduler may ask for columns in any order on any thread.

### The RNG family is data (the item everything else waited on)

`noise_settings/nether.json` and `end.json` set `legacy_random_source: true`;
`overworld.json` does not. Vanilla switches the **entire** noise stack on it
(`RandomState.java:36`), so this was never a tuning flag.

* `crate::rng::Algorithm` is `WorldgenRandom.Algorithm`, and
  `AnyRandomSource` / `AnyPositionalFactory` are the two-variant enums it
  produces. `PositionalRandomFactory` is not object-safe — associated `Source`
  type, returns `Self::Source` by value — so an enum was the only choice that did
  not make a generic parameter viral through `SurfaceSystem`, `AquiferSystem` and
  every stage struct that stores a factory by value.
* **`AnyPositionalFactory` is `Copy` because both variants are.** That single
  property is what kept the change to a type *name* in four files
  (`surface/mod.rs`, `aquifer/mod.rs`, `overworld/fill.rs`, `overworld/veins.rs`)
  rather than a signature rewrite: every use site already went through the trait.
* `density::Builder::with_algorithm(seed, algorithm, resolver)` is the
  dimension-aware constructor. `Builder::new` is that call with
  `Algorithm::Xoroshiro`, so the Overworld is byte-identical across the change —
  the whole worldgen suite, `region_parity` / `density_parity` / `noise_parity`
  against the JVM dumps included, is green and unchanged.
* **`from_seed` is not the same operation in the two families and must not be
  unified.** `LegacyPositionalRandomFactory.fromSeed(s)` discards its own seed and
  returns `new LegacyRandomSource(s)`; the xoroshiro one XORs `s` into both halves
  of its 128-bit state. `rng/any.rs` delegates each arm for that reason and has a
  test that would fail if they were merged.

### The two Nether noises are seeded by id, not by the flag

`RandomState.NoiseWiringHelper.visitNoise` (`RandomState.java:53-65`)
special-cases `minecraft:nether/temperature` and `minecraft:nether/vegetation` —
**before** `getOrCreateNoise` is reached and **regardless** of
`legacy_random_source`:

```java
NormalNoise.createLegacyNetherBiome(new LegacyRandomSource(seed + 0L), params)  // temperature
NormalNoise.createLegacyNetherBiome(new LegacyRandomSource(seed + 1L), params)  // vegetation
```

That is the **raw world seed plus 0 and 1**, not a positional fork, and
`createLegacyNetherBiome` is `new NormalNoise(random, params, useNewInitialization
= false)`, i.e. `PerlinNoise.createLegacyForLegacyNetherBiome` twice. It lives in
`Builder::instantiate_noise`, keyed on the id.

**The draw count is not the octave count, and this is the trap.** Both noises are
`firstOctave -7`, amplitudes `[1.0, 1.0]` — so `zeroOctaveIndex` is 7 while there
are only 2 octaves. Vanilla therefore builds an `ImprovedNoise` for the zero
octave and **throws it away**, then `skipOctave`s (262 discarded `nextInt`s) five
times, then builds the two levels it keeps. A port that built two levels and
stopped would consume the wrong amount of randomness and produce a Nether that
looks entirely plausible and is not vanilla's.

`BlendedNoise` takes the same seed under legacy init:
`useLegacyInit ? newLegacyInstance(0L) : random.fromHashOf("terrain")`
(`RandomState.java:70-73`), in `Builder::instantiate_blended`.

### Temperature and humidity *are* the map

The Nether router zeroes continentalness, erosion, depth and weirdness
(`NoiseRouterData.java:390-411`), and the parameter list's five rows are all
degenerate points — no `Parameter.span` anywhere, unlike the Overworld's 7,594
rows. So a Nether biome is a pure function of two noises, and it is
**two-dimensional**: both live channels are `shifted_noise` with
`y_scale: 0.0` and `shift_y: 0.0`, so the `y` argument to the underlying noise is
the constant `0.0`.

`NetherColumn` therefore carries 16 biomes (one per horizontal quart) rather than
a 4×4×4 grid. Two independent things say that is right: the code
(`nether_biomes_do_not_vary_with_y`) and the oracle world (the extractor asserts
that every section of every one of the 1,116 biome-bearing chunks stores the same
4×4 grid, and it does). **Do not copy this shape into a dimension with a real
depth channel** — issue #512 is the record of what broadcasting a biome vertically
costs when it is not y-invariant.

### The lava sea is the global fluid picker, not an aquifer

`aquifers_enabled` is `false`, so `NoiseChunk.java:145-152` takes
`Aquifer.createDisabled`, whose whole body is

```text
density > 0.0 ? null : fluidRule.computeFluid(x, y, z).at(y)
```

`AquiferSystem::disabled` is that, plus the bounded `final_density` sampler.
Every noise field and grid cache in `AquiferSystem` is a stub in this mode and
`enabled: false` makes `compute_substance` return before any of them is read.

Two corrections worth keeping, because the plan got both wrong first:

* **The `-54` lava status is an Overworld feature and is unreachable here.**
  `createFluidPicker` (`NoiseBasedChunkGenerator.java:68-80`) returns the deep-lava
  status when `y < min(-54, seaLevel)`; for the Nether that is `y < -54` against a
  `min_y 0` dimension. Everything resolves to `FluidStatus(32, LAVA)`.
* **`default_fluid` was hardcoded to water** in `AquiferSystem::global_fluid`,
  which is right for the Overworld and would have filled the Nether with water.
  It is now a field, read from the settings via `aquifer::fluid_from_settings`.
  Read the `Name` **and the properties**: `nether.json` says
  `{"Name": "minecraft:lava", "Properties": {"level": "0"}}`, and taking only the
  name yields `minecraft:lava`, a different string from the
  `minecraft:lava[level=0]` the carver writes — two palette entries for one state
  and a missed match in every downstream full-state comparison.

### `nether_cave` is four overrides, three of which are draws

`NetherWorldCarver extends CaveWorldCarver` and shares its codec exactly, so
`CaveConfig` parses both and carries a `nether: bool`.

| | `cave` | `nether_cave` |
|---|---|---|
| `getCaveBound()` | 15 | **10** |
| `getThickness(random)` | 2 floats + `nextInt(10)` + conditionally 2 more | **exactly 2 floats**, `(f*2 + f) * 2.0` |
| `getYScale()` | 1.0 | **5.0** (trunk only; the recursive splits pass 1.0 in both) |
| `carveBlock` | aquifer, grass tracking, `topMaterial` re-cap | **`y <= minGenY + 31 ? LAVA : CAVE_AIR`**, nothing else |

The thickness difference is the dangerous one: routing a nether carver through
the Overworld formula desyncs the RNG stream on the very first tunnel, and every
later cave in the chunk is then wrong. `CaveConfig::thickness` forks on the flag.

`min_gen_y + 31` is **hardcoded in vanilla and is not `sea_level`** — at `min_y 0`
it means y ≤ 31, one below the `sea_level 32` the fill uses. Do not unify them.
`carveBlock` also writes `minecraft:cave_air`, which the Overworld path never
does (its air comes from the aquifer as plain `minecraft:air`).

### Structures: the stage that was missing, and the one thing it had to do differently

`NetherGenerator` originally composed **no structure stage at all**, and that made
`bastion_remnant` a textbook island: its template pools loaded, its jigsaw assembly
was gated, it was absent from the unsupported ledger's per-structure rows, and it
placed **zero blocks anywhere in the game** — because its biome tag is Nether-only,
so the Overworld's stage (the only one that existed) could never accept it.
`fortress`, `nether_fossil` and `ruined_portal_nether` sat in the same position.

The machinery is shared, not new: `overworld/structures.rs`'s `StructureRefs`
product, `REFS_RADIUS`/`BEARD_REACH`, `structure::beardifier` and
`StructureRegistry` are all dimension-agnostic. Four things are dimension-shaped
and live in `nether/mod.rs`:

* **Which sets exist here.** `StructureRegistry::new_for_biomes(seed, resolver,
  Some(&possible))` is vanilla's `hasBiomesForStructureSet`
  (`ChunkGeneratorStructureState.java:52-67`), and `possible` is derived from the
  parameter table's own biome names rather than written down. A filtered Nether
  registry keeps `nether_complexes`, `nether_fossils` and `ruined_portals` and loads
  `bastion`'s pool graph only — not every village's. **It cannot change which chunk
  gets which structure**: `starts_at` re-seeds its weighted walk per set, so
  dropping a set shifts no other set's stream, and a dropped set's structures would
  have failed the biome filter anyway. The Overworld deliberately stays on the
  unfiltered `new`, because there the filter would drop exactly the Nether/End sets
  and change nothing but the ledger's keys.
* **The height probe.** `NetherStartSampler` samples `AquiferSystem::disabled` over
  *this* dimension's `final_density` at `min_y 0`, height 128. An Overworld-shaped
  probe would site a bastion plausibly and wrongly. Note it reads the **pre-surface
  fill**, so it never sees the bedrock roof — which is vanilla's own `getBaseHeight`
  behaviour, not a gap, and bastion probes nothing anyway
  (`start_height: {absolute: 33}`).
* **The biome the filter reads.** The `StartContext` spells the sample with the real
  `quartY`, unlike `biome_quarts`' constant 0; the two agree by the y-invariance
  `nether_biomes_do_not_vary_with_y` pins, and writing it the vanilla way keeps the
  structure filter from inheriting a neighbouring optimisation's assumption.
* **Memoisation.** A bounded `Mutex<HashMap>` on the generator, not the Overworld's
  staged store (whose entry type is that generator's stage set and whose retention is
  sized against a 37×37 pinned closure). Clearing it wholesale is sound *here* and
  would not be there: it holds only starts, each a pure function of `(seed, cx, cz)`,
  so a miss returns the identical value.

**The beardifier is empty for every Nether chunk today, and that is the negative
control for the whole change.** `nether_fossil` (`beard_thin`) is the dimension's
only adaptation-bearing structure and has no piece generator, so `fill_stage` takes
its no-beard branch — which is *why* the biome and bedrock parity below is unchanged
by construction rather than by measurement. `the_beardifier_is_empty_because_no_nether_structure_bears_adaptation_yet`
pins that, so a future `nether_fossil` generator has to update it deliberately.

**Where the fortress goes.** `nether_complexes` carries `fortress` at weight 2 and
`bastion_remnant` at weight 3. `fortress` has no piece generator, so it yields an
*advisory* start (`pieces_complete: false`, zero blocks) — and crucially
`StructureKind::validity` reports `Unknown` rather than `Invalid` for it, so the
weighted walk **stops** there exactly as vanilla's does. A fortress cell is not
silently promoted to a bastion, and the RNG stream is vanilla's.

## Evidence

`crates/lodestone-worldgen/tests/nether_structures.rs` gates the structure stage.
Measured at seed −195764831: `bastion_remnant` starts at chunk **(8, 7)** — ring 0,
the first candidate cell, because `has_structure/bastion_remnant` covers four of the
five Nether biomes — with **89 pieces** in box `[128,32,67]..[177,87,133]`, placing
**15,405** bastion-only blocks against **0** in a structure-free control over
identical data. The discriminating names are the *measured* with-minus-without
palette difference narrowed to those absent from the Nether's own `surface_rule`
(`basalt`, `blackstone` and `nether_wart` are excluded because a real Nether column
can produce them, and a gate built on either of the first two would have passed in
the control arm too).

`crates/lodestone-worldgen/tests/nether_gen.rs`, against
`tests/support/nether_vanilla_oracle.txt` — extracted from
`.cache/mc/survival/world/dimensions/minecraft/the_nether/region/*.mca`, four
region files a vanilla 26.2 server wrote at seed **−195764831**. The seed is read
from `world/data/minecraft/world_gen_settings.dat`; 26.2 does not keep it in
`level.dat`.

Measured: **17,855 of 17,856 quarts agree**, and **20,480 of 20,480 bedrock-shell
positions agree exactly**. The one biome quart that differs is not a defect and
cannot be fixed — see "The one tie" below.

| gate | scope | what it can see |
|---|---|---|
| `nether_biomes_match_the_vanilla_oracle_world` | 1,116 chunks × 16 quarts, element-wise | the whole noise stack: family, legacy-init `NormalNoise`, `seed+0`/`seed+1`, quantization, the parameter search |
| `the_census_matches_the_oracle_world` | 487 / 327 / 255 / 172, and warped_forest **0** | a wholesale name swap, reduced to four numbers |
| `nether_bedrock_shell_matches_the_vanilla_oracle_world` | 8 `full` chunks × 256 columns × 10 y | `vertical_gradient` off the **surface** system's positional factory, which is the LCG under legacy init |
| `nether_biomes_do_not_vary_with_y` | 4 columns × 6 y | a router change that introduces a depth channel |
| `a_nether_column_is_real_terrain_not_a_uniform_field` | one column | the island floor: all-air, all-solid, water instead of lava, a wrong fluid level |
| `columns_are_byte_identical_regardless_of_order_or_generator_instance` | 3 columns, both orders, two generators | palette-order nondeterminism (a real Overworld bug) |

Why biomes are the decisive gate: in this dimension the biome *is* the noise. Two
`NormalNoise`s that only exist if `legacy_random_source`,
`createLegacyNetherBiome` and the `seed + n` seeding are simultaneously right
produce it, and every other climate channel is a literal `0.0`. There is no way to
get 17,856 quart comparisons right with any of those three wrong.

Why bedrock: it is the one surface-rule product later stages cannot touch. It is
absent from `#minecraft:nether_carver_replaceables` so no carver may replace it,
and no Nether decoration feature places or removes it — which makes it exactly
comparable against a `full` vanilla chunk even though this generator does not run
decoration. The gate compares the full 5-bit masks rather than "is y = 0 bedrock",
because y = 0 and y = 127 are the gradient's saturated ends and come out right
under the *wrong* RNG family too.

**The excluded chunks are not cherry-picked.** The oracle world stores 2,444
Nether chunks; 1,328 sit at `Status: minecraft:structure_starts`, a step before
`fillBiomesFromNoise`, and their biome container is still the registry placeholder
— all 64 cells `minecraft:plains`, in the Nether. The extractor *asserts* that
rather than filtering on the name, and drops them.

### The one tie, and why no implementation can close it

Chunk (−20, −21) quart (0, 1) resolves to `nether_wastes` here and
`crimson_forest` in the vanilla world. It is an **exact fitness tie**: the climate
target is `temperature 2000, humidity 1457`, and `nether_wastes` (a degenerate
point at `0, 0`) and `crimson_forest` (at `4000, 0`) are both
`2000² + 1457² = 6,122,849` away — 2000 is the exact midpoint of 0 and 4000.

At an exact tie **vanilla's answer is a function of the previous query on the same
thread, not of the target**. `Climate.RTree.search` seeds the descent with
`this.lastResult.get()`, a `ThreadLocal<Leaf>`, and `SubTree.search` compares with
a strict `minDistance > childDistance`, so a tied candidate never displaces the
incumbent (`Climate.java:389-392, 443-460`). The incumbent is whatever the
previously sampled position resolved to — and that `ThreadLocal` persists **across
chunks**. The neighbouring quart at (−80, −84) really is `crimson_forest`.

So vanilla's own answer at a tie depends on its chunk *and* quart iteration order.
A demand-ordered generator cannot have that, and must not:
`columns_are_byte_identical_regardless_of_order_or_generator_instance` is the
stronger requirement, and the join scheduler is view-first and re-sortable.
`BiomeTable::nearest_row_seeded` exists for exactly this (it takes the previous
candidate) and threading it through would trade a 0.0056% divergence for
order-dependent output. **That is the wrong trade** — this is the one place where
being bit-identical to vanilla and being deterministic are incompatible, and
determinism wins.

The gate therefore **classifies** instead of tolerating: a disagreement is
admissible only when the two biomes' fitnesses are *equal* — a derived condition on
the data, not a threshold — and both the admissible count and its position are
pinned, so any change to either fails.

**`warped_forest` occurs 0 times, and that is a world-species limit.** Its
parameter row is the only one with a non-zero humidity-side offset
(`0, 5000, offset 3750`) and this seed's generated chunks never sample there. The
census test pins the 0 explicitly so a future reader does not read the absence as
a defect.

## How to change it

* **Anything that changes an RNG draw count changes the world.** The two Nether
  noises' 5 × 262 skipped draws, `getThickness`'s 2-vs-3-to-5 draws, and
  `getCaveBound`'s bound are all specification, not implementation detail. The
  biome gate catches the noise ones immediately; the carver ones are only visible
  in a block comparison, which this suite does not yet do.
* **A biome name this generator can produce must have its carver list resolved in
  `new`** (the `carvers_by_biome` walk over the parameter table), or its columns
  silently never carve.
* **There is no fixed-biome fallback, deliberately.** `NetherGenerator::new`
  panics on an empty `biome_parameters()`. A Nether without its 5-row table is not
  a degraded world, it is a misconfigured one, and a fallback would produce uniform
  `nether_wastes` that looks fine in a screenshot.
* **Rebuilding the fixture**: the extractor is
  `docs/worldgen-nether.md`'s companion script, kept out of the repo because it
  runs once per oracle world. Its algorithm is in this doc's Evidence section and
  in `nether_gen.rs`'s module doc; the one non-obvious trap is that a Python
  NBT reader must read a compound entry's **name before its payload**
  (`out[r.s()] = payload(r, tt)` evaluates the right-hand side first and desyncs
  the stream — that cost a run).
* **Cell geometry comes from the settings now** (`aquifer::cell_geometry`, i.e.
  `size * 4`). The Nether's `1, 2` matches the Overworld's 4-wide/8-tall cell so
  nothing moved; the End's `2, 1` is 8-wide/4-tall and is why the function exists.

## What is not here

* **Decoration.** The `UNDERGROUND_ORES` / `VEGETAL_DECORATION` /
  `SURFACE_STRUCTURES` steps that place glowstone, fire, nether wart, quartz and
  gold ore, magma, the crimson/warped vegetation and basalt pillars are not
  composed. All 226 configured and 262 placed features are bundled and the five
  biome documents already carry the step wiring, so this is composition work in
  `crate::feature`, not missing data.
* **Fortress, nether fossil, ruined portal** — their *placement* is composed and
  each yields an advisory start; none has a piece generator, so each places zero
  blocks and each carries its own row on the unsupported ledger. `bastion_remnant`
  is done (see Structures above).
* **Reaching it from the game.** `lodestone-server`'s `EmbeddedResolver`
  hardcodes the Overworld documents and `OverworldChunkSource` is the only chunk
  source, so **a portal trip does not land in this terrain yet**: the generator
  exists and is verified, and the server-side wiring (a per-dimension resolver, a
  `NetherChunkSource`, and dimension-aware `ChunkSource` selection) is a
  `lodestone-server` change. Issue #330 is the portal/dimension-registry half.

## Configuration

None at runtime. The data is embedded at build time by
`crates/lodestone-server/build.rs` from `assets/worldgen/`; the documents this
generator needs are `noise_settings/nether`, `noise/nether/{temperature,
vegetation}`, `biome_parameters/nether`, `density_function/nether/base_3d_noise`,
the five `biome/*` documents, `configured_carver/nether_cave` and
`tags/block/nether_carver_replaceables`.

## Dependencies

`lodestone_worldgen::{aquifer, biome, carver, compose, surface, dense_grid,
interner}` and `lodestone-worldgen-core`'s `density` / `engine` / `noise` / `rng`.
Nothing version-specific. Sibling docs:
[`worldgen-dimensions.md`](./worldgen-dimensions.md) (the per-dimension engine
deficit this closes half of), [`worldgen-parity.md`](./worldgen-parity.md),
[`worldgen-biomes.md`](./worldgen-biomes.md).
