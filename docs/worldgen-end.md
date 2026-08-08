# End worldgen

## What it is

The composed End generator — `lodestone_worldgen::end::EndGenerator` — plus the two
pieces it is built on (`EndIslandNoise`, the `minecraft:end_islands` algorithm, and
`EndBiomeSource`, `TheEndBiomeSource`). It is the third dimension this engine produces
real terrain for, and the only one with **no vanilla block oracle anywhere**, so this
doc is as much about what its gates can and cannot claim as about how it works.

## How it works

### The biome source is complete and needs no interpreter

`TheEndBiomeSource.getNoiseBiome` samples the router's **`erosion`** channel and
nothing else, and for the End that channel is literally
`{"type": "minecraft:cache_2d", "argument": {"type": "minecraft:end_islands"}}`
(bundled `noise_settings/end.json`; `NoiseRouterData.java:433,443`). Every other
channel except `final_density` is `zero()`. So the End's whole biome layout is a
pure function of `EndIslandNoise`, and `EndBiomeSource` is built straight on it —
no `Density`, no `Resolver`.

```text
chunkX² + chunkZ² <= 4096  ->  the_end            (radius 64 chunks — the main island)
erosion >  0.25            ->  end_highlands
erosion >= -0.0625         ->  end_midlands
erosion <  -0.21875        ->  small_end_islands
otherwise                  ->  end_barrens
```

Two details are load-bearing:

* **The sample position is the chunk centre, not the quart.** Vanilla computes
  `weirdBlockX = (chunkX * 2 + 1) * 8` = `chunkX * 16 + 8` (the decompiled source
  really does call it `weird`), so all 16 quarts of a chunk share one erosion
  sample and every End chunk is biome-uniform. Sampling at the quart's own block
  position would give a finer and wrong map, and it would still look like a
  plausible End.
* **The 4096 gate is `i64`,** and it is the *same* constant `end_islands`' own
  centre hole uses — which is why `the_end` covers precisely the region that can
  never carry an island.

The five biome ids are **not data**: `TheEndBiomeSource` serialises to an empty
object and its holders come from the registry (`TheEndBiomeSource.java:14-23`), so
they are constants in `end/mod.rs` rather than a resolver lookup.

### `end_islands`, and the six ways to get it wrong

`crates/lodestone-worldgen-core/src/noise/end_islands.rs`. It is a
`SimpleFunction` — no children, no arguments — whose codec is
`MapCodec.unit(new EndIslandDensityFunction(0L))`, so the JSON is always
`{"type": "minecraft:end_islands"}` and `RandomState.java:74` substitutes the real
world seed at wiring time.

**Its seeding does not consult `legacy_random_source`.** It always constructs
`new LegacyRandomSource(seed)` (`DensityFunctions.java:498`) regardless of the
flag. The End happens to set the flag as well, but that is a coincidence and the
two are independent — which is what made this implementable and gate-able before
the family selector landed.

The traps, each of which produces a plausible-looking but wrong End:

| trap | correct |
|---|---|
| `consumeCount(17292)` | 17,292 discarded `nextInt()`s, **before** `SimplexNoise`'s own three `nextDouble`s and 256-step Fisher–Yates |
| `sectionX / 2`, `% 2`, `blockX() / 8` | Java **truncating** division; Rust's `/` and `%` match, so `div_euclid` is wrong. For negatives `subSection ∈ {−1, 0, 1}` |
| `sectionX² + sectionZ²` | computed in **`i32`**, only then widened for a `f32` sqrt |
| `totalChunkX² + totalChunkZ² > 4096` | separately **`i64`** |
| `islandSize` | a **slope**, not a radius; range `[9, 22)` |
| `islandSize` / `xd` / `zd` / `newDoffs` | all **`f32`**; `Mth.sqrt` is a `f64` sqrt narrowed back to `f32` |

Loop bounds are `-12..=12` on both axes — **625 candidate chunks per call**, which
is why the router wraps it in `cache_2d`.

### `EndGenerator` is the Nether generator with four substitutions

Every one of them is data rather than code, and everything not in this table is
shared: `legacy_random_source: true` through `crate::rng::Algorithm`,
`aquifers_enabled: false` through `aquifer::AquiferSystem::disabled`, and the fill /
`solidTop` heightmap / materialise stages through `crate::compose`'s
`fill_column` / `solid_top_heights` / `materialize_column` (extracted from the
Nether's private methods when this landed, so the two dimensions cannot drift apart
about the `-0.0` branch or the palette-order rule).

| | Nether | End |
|---|---|---|
| biome | 5-row multi-noise parameter table | `EndBiomeSource` — one erosion sample per **chunk** |
| `default_fluid` | `minecraft:lava[level=0]`, `sea_level 32` | **`minecraft:air`**, `sea_level 0` |
| cell geometry | `size_horizontal 1, size_vertical 2` → 4 wide, 8 tall | **`2, 1`** → **8 wide, 4 tall** |
| carver | `nether_cave` | **none** — `configured_carver` has four entries and no End biome document names one |

Two consequences worth stating because a generator that got them wrong would still
produce something End-shaped:

* **The End has no fluid at all, and air is a real answer rather than a missing one.**
  `createFluidPicker`'s deep-lava branch needs `y < min(-54, seaLevel)`, and against
  `min_y 0` / `sea_level 0` that is unreachable; the sea status is then
  `FluidStatus(fluid_level = 0, …)` and `FluidStatus.at(y)` returns its type only for
  `y < 0`, i.e. never. So the answer is air at every position *whatever*
  `default_fluid` says. A generator that fell back to water for an unrecognised fluid
  would be wrong here and no Overworld or Nether gate could see it — this is the same
  latent shape §12.134's `global_fluid` bug had.
* **The surface rule is a no-op and there is no bedrock.** `end.json`'s
  `surface_rule` is a bare `minecraft:block` writing `end_stone`, and `default_block`
  is already `end_stone`; vanilla's scan only rewrites a position holding the default
  block, so the rule cannot change anything. It is composed anyway rather than
  special-cased, because the rule is data a datapack may replace. The important half
  is the absence: there is **no `vertical_gradient` anywhere** in the End's rule,
  which is exactly where copying the Nether's shape would have been actively wrong —
  the Nether's bedrock floor and roof come from that construct.

There is **no structure stage**, deliberately: `end_city` is the End's only structure
and has no piece generator, so a stage today would place starts nothing could build.
The Nether's stage is the template when it lands.

## Evidence, and its honest limit

**There is no block oracle for the End anywhere.** `.cache/mc/survival/world`'s
`dimensions/minecraft/the_end/` contains a `data/` directory and **no `region/`
directory at all** — the world's End was never visited, so there is not one
vanilla-generated End chunk on this machine. So End work cannot be compared against
vanilla output, and comparing it against our own would be the closed loop the evidence
rules exist to forbid. Every gate therefore derives its expectation from a **record
definition**, from **arithmetic**, or from a **cross-dimension control**.

### The terrain gates (`crates/lodestone-worldgen/tests/end_gen.rs`)

The strongest is a **closed-form prediction from the router read as a tree**, and it
needs to know nothing about `EndIslandNoise` at all. `end.json`'s `final_density` is

```text
squeeze(interpolated(0.64 * blend_density(
    -0.234375 + g1(y) * (0.234375 + (-23.4375 + g2(y) * (23.4375 + end/sloped_cheese)))
)))
  g1 = y_clamped_gradient(from_value 0.0 @ from_y 4,  to_value 1.0 @ to_y 32)
  g2 = y_clamped_gradient(from_value 1.0 @ from_y 56, to_value 0.0 @ to_y 312)
```

`Mth.clampedMap` clamps below `from_y` to `from_value`, so **`g1(y) = 0.0` exactly for
every `y <= 4`** — and `DensityFunctions.Ap2.Mul` short-circuits on
`argument1 == 0.0` without evaluating `argument2`, so at those heights the island
field, `base_3d_noise` and the seed are *structurally* not consulted. The expression
collapses to `squeeze(0.64 × −0.234375) = squeeze(−0.15) = −0.07485938`, which is
`<= 0`, so the disabled aquifer returns the global fluid, which is air. The cell height
is 4, so the interpolation's bracketing corners are `y = 0` and `y = 4` and both are
that same constant. **Every block at y ∈ [0, 4], everywhere in the End, at every seed,
is air** — and the gate sweeps 2 seeds × 10 chunks × 256 columns × 5 heights = 25,600
positions to say so. It fails on a swapped `from_y`/`to_y`, a swapped
`from_value`/`to_value`, a misread `mul`/`add` nesting, a wrong constant, or an
off-by-one in the fill's `y`.

**Its control had a false premise on the first try, and the correction is the record.**
The first control was "look one cell higher, where `g1` is unclamped, and require a
mixture" — and it failed, correctly: the End's terrain is a slab around y 12–62, so
y 5..11 is empty for a reason that has nothing to do with the gradient. That is the
§12.41 shape, and it failed in the *unsafe* direction: had the End happened to have low
terrain it would have passed while measuring nothing. The control now **mutates the
record** — swaps `from_value` and `to_value` on `g1`, the single most natural
transcription error — builds a second generator from the mutated settings, and requires
the band to become island-dependent (some solid *and* some air). A dead-band gate whose
control cannot tell the two gradients apart proves nothing.

The rest:

| gate | expectation comes from |
|---|---|
| `the_end_generates_no_fluid_anywhere` | **arithmetic**: `FluidStatus.at(y)` against `fluid_level 0` and `min_y 0`, so no `y` can carry fluid whatever `default_fluid` says |
| `the_no_fluid_detector_sees_the_nethers_lava_sea` | its control, and it has to be **another dimension**: the same `AquiferSystem::disabled` path over `nether.json` must produce lava, or the End's zero is a statement about a blind detector |
| `the_end_interpolates_over_8_wide_4_tall_cells` | the two documents, compared to each other — the End's 8×4 is the **transpose** of the Nether's 4×8, which is why `cell_geometry` exists |
| `the_end_has_no_bedrock_and_its_surface_rule_cannot_change_a_block` | the **record**: no `vertical_gradient` in the End's rule, and the Nether's rule asserted to still carry the construct so the reading cannot go stale |
| `a_column_carries_the_biome_sources_own_answer` | the already-gated `EndBiomeSource`, plus a sweep requiring all five biomes — the wire-facing half of a source whose own tests are in `end/mod.rs` |
| `the_end_is_real_terrain_and_the_main_island_is_solid` | the **island floor**, stated as a floor: all-air, all-end-stone, a one-entry palette, a `final_density` on the wrong router key, and a transposed `column_index` between the shape field and the materialised column |
| `columns_are_byte_identical_regardless_of_order_or_generator_instance` | palette-order nondeterminism — a real Overworld bug this repo shipped once |

Measured profile at seed −195764831 (`print_the_end_terrain_profile`, `#[ignore]`d,
a diagnostic and **not** a parity claim): the main island's centre chunk (0, 0) is
12,146 / 32,768 solid over y 12..60, (3, −2) is 10,918 over y 15..59, an outer
`end_midlands` chunk at (400, 400) is 10,000 over y 14..62, and every sampled
`small_end_islands` chunk plus the void inside the radius-64 `the_end` biome is
**0** — which is right: small end islands are a *feature* (`end_island_decorated`),
not terrain.

### What none of this can see, named rather than papered over

* **`consumeCount(17292)`.** A wrong RNG draw count inside `EndIslandNoise` leaves
  every prediction above intact, because not one of them depends on the island field's
  *value* — the dead band is structurally independent of it, the fluid and bedrock
  gates are about the settings, and the profile numbers are a diagnostic. The only
  thing that closes it is a `DensityOracle` dump of `end_islands` at known
  coordinates; `scripts/worldgen-oracle/DensityOracle.java` and `run.sh` exist and run
  under Apple `container`, so this is a small, unblocked addition.
* **The block field itself.** Whether the main island's silhouette is vanilla's is not
  gated and cannot be until an End region file exists. The numbers above are plausible
  and are not evidence.
* **`base_3d_noise`'s contribution.** Its magnitude is not bounded anywhere in this
  doc, which is why no gate here brackets a density value that depends on it.

### The `EndIslandNoise` and `EndBiomeSource` gates (unchanged, in-crate)

These predate the generator and are untouched by it. They are in
`crates/lodestone-worldgen-core/src/noise/end_islands.rs` and
`crates/lodestone-worldgen/src/end/mod.rs`.

| gate | expectation comes from |
|---|---|
| `inside_the_centre_hole_the_height_is_the_closed_form_plateau` | **geometry**: inside the hole every one of the 625 candidates fails `totalChunk² > 4096`, so the height *is* `clamp(100 − sqrt(sx² + sz²) · 8, −100, 80)` and the simplex noise is not consulted at all |
| `outside_the_centre_hole_islands_actually_raise_the_field` | the control for it — islands must demonstrably fire outside, and must not fire everywhere |
| `compute_is_the_affine_map_of_the_height_field_and_respects_its_bounds` | **arithmetic**: `minValue`/`maxValue` are the two `clamp` limits through `(h − 8)/128` |
| `negative_coordinates_truncate_toward_zero` | the **record definition** — Java's `/` and `%` semantics, hand-expanded |
| `the_main_island_is_exactly_chunk_radius_64` | the geometric predicate, evaluated independently of the branch under test, with both arms required to fire |
| `all_five_biomes_are_reachable` | the island floor: a collapsed threshold ladder passes every other test here |
| `the_threshold_ladder_matches_the_erosion_value` | an independent construction — `EndIslandNoise::compute` plus the four constants transcribed by hand from `TheEndBiomeSource.java` — with all four outer arms required to fire |

**The premise assertion in the plateau test earned its place.** The first
derivation of its safe window was wrong: reading the radius off one axis gives
`|chunk| <= 52`, but the binding candidate is the *diagonal* corner
`(chunk + 12, chunk + 12)`, so the real condition is `2 · (|chunk| + 12)² <= 4096`,
i.e. `|chunk| <= 33`. The test asserted its own precondition and failed on the
first run rather than silently comparing against a formula that no longer applied
— which is the shape a control that can be premise-false needs. The dead-band
control above is the second instance in this one doc, which is why it is written up
rather than quietly fixed.

## What the End does not do

* **No decoration.** All six End features (`end_platform`, `end_spike`,
  `end_gateway_return`, `chorus_plant`, `end_island`) are bundled configured *and*
  placed, and the five biome documents already carry the step wiring (`the_end`:
  step 4 `end_spike`, step 10 `end_platform`; `end_highlands`: step 4
  `end_gateway_return`, step 9 `chorus_plant`; `small_end_islands`: step 0
  `end_island_decorated`). So this is `crate::feature` step work with **zero data
  missing** — and it is why every sampled `small_end_islands` chunk generates zero
  blocks: the small islands are a feature, not terrain.
* **No structure stage.** `end_city` is the End's only structure and has no piece
  generator, so a stage would place starts nothing could build. The Nether's stage
  (`nether/mod.rs`) is the template when one lands; `end_city` is a template-piece
  structure rather than jigsaw (`EndCityStructure.java:14-45`).
* **Not gameplay, despite looking like worldgen** (each of these was checked
  against the jar, and getting it wrong is how a worldgen plan inflates):
  `EndPodiumFeature` — the exit portal — is a `Feature` subclass that is **never
  registered** in `Feature.java` or `EndFeatures.java`; only
  `EnderDragonFight.spawnExitPortal` instantiates it. The obsidian pillars and the
  end platform each have *two* placers, one worldgen and one gameplay
  (`DragonRespawnStage.java:60-79`, `EndPortalBlock.java:90`). Gateways have
  *three* paths and only `end_gateway_return` (rarity 700 in `end_highlands`) is
  worldgen. Corroborating from data alone: `end_gateway_delayed` has a configured
  feature and **no placed feature**, because nothing in worldgen places it.
* **Not reachable from the game.** `lodestone-server` has one chunk source and its
  `EmbeddedResolver` hardcodes the Overworld documents, so `EndGenerator` exists,
  is gated and is not selected by anything — the same server-side gap
  `worldgen-nether.md` records. A per-dimension resolver plus an
  `EndChunkSource`/`NetherChunkSource` is a `lodestone-server` change.

## Configuration

None at runtime. `noise_settings/end.json`, `density_function/end/{sloped_cheese,
base_3d_noise}` and the five `biome/end_*` documents are all bundled under
`crates/lodestone-server/assets/worldgen/` and embedded by that crate's
`build.rs`.

## How to change it

* **Do not add a gate whose expected value came from this code.** There is no End
  oracle; the three admissible sources are the ones the Evidence section names.
* **The dead-band gate is the cheapest strong check in the dimension** and it reads
  `from_y`/`from_value` back out of the document rather than restating them, so a
  datapack change to the gradient is a *failure* there instead of a silently wrong
  prediction. Keep that property if you touch it.
* **`crate::compose`'s `fill_column` / `solid_top_heights` / `materialize_column` are
  shared with the Nether.** A change to any of them needs `nether_gen.rs` re-run —
  that file carries the only real vanilla-block comparison either dimension has.
* Adding a carver would be wrong until a datapack asks for one: no End biome document
  names a `configured_carver`, and composing one "for symmetry" would consume RNG that
  vanilla does not.

## Dependencies

`lodestone_worldgen::{aquifer, compose, surface, dense_grid, interner}` and
`lodestone_worldgen_core::{density, engine, noise::SimplexNoise,
rng::LegacyRandomSource}`; the biome source alone needs no resolver. Sibling docs:
[`worldgen-dimensions.md`](./worldgen-dimensions.md) (the per-dimension deficit
inventory), [`worldgen-nether.md`](./worldgen-nether.md) (the shared engine work,
which landed there).
