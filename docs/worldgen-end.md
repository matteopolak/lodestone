# End worldgen

## What it is

What the End generates today and what it does not. Two pieces landed —
`lodestone_worldgen_core::noise::EndIslandNoise` (the `minecraft:end_islands`
algorithm) and `lodestone_worldgen::end::EndBiomeSource` (`TheEndBiomeSource`,
complete) — and End *terrain* is blocked on exactly one thing: an `end_islands`
leaf in the density interpreter, which lives in another cluster's file. This doc
says precisely what that patch is so it can be applied without re-deriving
anything.

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

### Everything else the End needs already landed with the Nether

| need | state |
|---|---|
| `legacy_random_source: true` | `crate::rng::Algorithm`, landed |
| `aquifers_enabled: false` | `aquifer::AquiferSystem::disabled`, landed |
| `default_fluid: minecraft:air` (with `sea_level: 0`) | `aquifer::fluid_from_settings`, landed — air is a real answer here, not a missing one, so the End has no fluid at all |
| 8-wide/4-tall cells (`size_horizontal 2, size_vertical 1`) | `aquifer::cell_geometry`, landed |
| surface rule | a single `minecraft:block` end_stone rule — the engine's simplest possible case, already handled |
| `TheEndBiomeSource` | `end::EndBiomeSource`, landed |
| `end_islands` the algorithm | `noise::EndIslandNoise`, landed |
| `end_islands` as a `Density` leaf | **not landed** — see below |

## The one missing patch

`noise_settings/end.json`'s `final_density` reaches
`density_function/end/sloped_cheese.json`, which is
`add(minecraft:end_islands, end/base_3d_noise)`. So `density::Builder::build`
panics on the unknown type and there is no `EndGenerator`. The patch is four
edits, all in `lodestone-worldgen-core`'s `density/` and `engine/`:

1. **`density/mod.rs`** — a variant on `Density`, **appended at the end of the
   enum, never inserted in the middle**:

   ```rust
   /// `minecraft:end_islands`. Behind `Arc` because the noise carries a
   /// 256-byte permutation and the End router holds two of these (inline in
   /// `end.json`'s erosion and inside `end/sloped_cheese`).
   EndIslands(std::sync::Arc<crate::noise::EndIslandNoise>),
   ```

2. **`density/mod.rs`, `Density::compute`** —
   `Density::EndIslands(n) => n.compute(ctx.x, ctx.z),`

3. **`density/mod.rs`, `Builder::build_object`** — one arm:

   ```rust
   "minecraft:end_islands" => Density::EndIslands(Arc::new(
       crate::noise::EndIslandNoise::new(self.seed),
   )),
   ```

   `self.seed` is the raw world seed field the legacy-RNG work already added, and
   it is the right one: `RandomState.java:74` substitutes exactly that, ignoring
   the codec's 0. **Build it once per `Builder`** rather than per occurrence — the
   two sites are the same function in vanilla (`RandomState`'s `wrapped`
   `computeIfAbsent` map dedupes them) and each construction burns 17,292 + ~259
   RNG draws.

4. **`engine/graph.rs` + `engine/field.rs`** — an `OpKind` appended to *both*
   tables, whose discriminant equals the new variant's `Density::kind_index()`.
   `engine/mod.rs`'s own module doc spells this out: two of the three edits are
   compile errors and the third is only caught by
   `graph::tests::op_kind_discriminants_match_density_kind_index`. Treating it as
   a **leaf** (like `spline` / `old_blended_noise` / `find_top_surface`) is
   correct and simplest — it has no children to flatten.

Once that lands, an `EndGenerator` is the Nether generator with four
substitutions: `EndBiomeSource` in place of the multi-noise table,
`BlockKind::Air` as the disabled aquifer's fluid, `cell_geometry`'s 8×4, and no
carver (the End has none — `configured_carver` has four entries and none is
reachable from an End biome document). The Nether's `column` is the template.

## Evidence, and its honest limit

**There is no block oracle for the End anywhere.** `.cache/mc/survival/world`'s
`dimensions/minecraft/the_end/` contains a `data/` directory and **no `region/`
directory at all** — the world's End was never visited, so there is not one
generated End chunk on this machine. So End work cannot be compared against
vanilla output, and comparing it against our own would be the closed loop the
evidence rules exist to forbid.

What the gates therefore rest on:

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
— which is the shape a control that can be premise-false needs.

**What none of this can see** is a wrong RNG *draw count* that happens to leave the
plateau and the bounds intact. `consumeCount(17292)` is exactly that kind of
number. The only thing that would close it is a `DensityOracle` dump of
`end_islands` at known coordinates: `scripts/worldgen-oracle/DensityOracle.java`
and `run.sh` exist and run under Apple `container`, so this is a small, unblocked
addition — it was not done here because the density leaf it would gate has not
landed.

## What the End does not do

* **No terrain.** See the patch above.
* **No decoration.** All six End features (`end_platform`, `end_spike`,
  `end_gateway_return`, `chorus_plant`, `end_island`) are bundled configured *and*
  placed, and the five biome documents already carry the step wiring (`the_end`:
  step 4 `end_spike`, step 10 `end_platform`; `end_highlands`: step 4
  `end_gateway_return`, step 9 `chorus_plant`; `small_end_islands`: step 0
  `end_island_decorated`). So this is `crate::feature` step work with **zero data
  missing**.
* **`end_city` is a structure**, and a template-piece one rather than jigsaw
  (`EndCityStructure.java:14-45`) — the structure group's phase S2.
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
  `EmbeddedResolver` hardcodes the Overworld documents.

## Configuration

None at runtime. `noise_settings/end.json`, `density_function/end/{sloped_cheese,
base_3d_noise}` and the five `biome/end_*` documents are all bundled under
`crates/lodestone-server/assets/worldgen/` and embedded by that crate's
`build.rs`.

## Dependencies

`lodestone_worldgen_core::{noise::SimplexNoise, rng::LegacyRandomSource}` for the
island field; nothing else, and no resolver. Sibling docs:
[`worldgen-dimensions.md`](./worldgen-dimensions.md) (the per-dimension deficit
inventory), [`worldgen-nether.md`](./worldgen-nether.md) (the shared engine work,
which landed there).
