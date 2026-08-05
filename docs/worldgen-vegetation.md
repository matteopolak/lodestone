# Vegetal decoration: grass, flowers and trees

## What it is

`crates/lodestone-worldgen/src/feature/vegetation.rs` is the engine that places grass, flowers and
trees (oak, birch, spruce, plus spruce's own `pine` sibling) during world generation — issue #406,
epic #404's Phase 3. It is wired into `OverworldGenerator::column` as the last composed stage,
`VEGETAL_DECORATION`, right after #295's ore-feature stage.

## How it works

Vanilla ships `configured_feature/*.json` and `placed_feature/*.json` as data (Mojang's own
generator output, already embedded — see `crates/lodestone-server/src/worldgen_data.rs`'s
`EmbeddedResolver`), plus a per-biome ordered list of which placed features run in each generation
step. This module is a small interpreter over that data, reproducing vanilla's exact RNG-consumption
order:

- **Placement modifiers** (`VegPlacement`): `count`, `in_square`, `heightmap`, `biome`,
  `rarity_filter`, `surface_water_depth_filter`, `noise_threshold_count`, `random_offset`,
  `block_predicate_filter`. Composed as a depth-first `flatMap`, exactly like the ore engine
  (`crate::feature::Placement`) — draws interleave across candidates in the same order vanilla's own
  `Stream` pipeline does.
- **Configured features** (`ConfiguredFeature`): `simple_block` (grass/flowers — one block, gated by
  `VegetationBlock.canSurvive`), `tree` (`TreeConfig`: a straight trunk placer plus a blob/spruce/pine
  foliage placer, matching `TreeFeature.doPlace`'s exact algorithm including the "not enough room"
  space-check that can silently reject a placement), `random_selector`/`simple_random_selector`
  (vanilla's own per-attempt branching between tree variants, e.g. oak vs. fancy-oak vs. fallen-oak).
- **`VegGrid`**: the mutable, chunk-local (`0..16 × 0..16`, absolute `y`) block field this stage reads
  and writes, seeded from the post-ore composed grid and folded back afterward.
- A single `beehive` tree decorator (the only one oak/birch's common `_bees_*` variants use).

`crate::compose::build_biome_vegetation` resolves one biome's `VEGETAL_DECORATION` step
(`GenerationStep.Decoration.ordinal() == 9`) into `(raw step index, PlacedRef)` pairs, preserving each
entry's raw array position for `setFeatureSeed` — the same "preserve index, not count" convention
`build_biome_ores` already established. `OverworldGenerator::vegetation_stage` resolves each source
chunk's biome from its own **surface-height** per-quart map (`biome_stage`'s quart 0 — the source's
min-block corner at its generated surface height), NOT `biome_for_carver_source`'s y=0 answer: issue
#480, the `crate::biome` module doc's "y = 0 trap". At y=0 the `depth` gradient is already ≈ +1.0, so
surface dark_forest chunks resolved as lush_caves and decorated with that biome's all-silent feature
list (vines/vegetation_patch/root_system) — zero grass, zero trees, even after the dark oak placer
(#428). Carver selection and ore placement keep the y=0 `biome_for_carver_source` convention; only
vegetation selects its list at the surface the player sees. The stage builds a `VegGrid` from the
chunk's post-ore terrain, runs the step, and folds only the written cells back (`VegGrid::dirty_cells`).

## How to change it, and the gotchas

- **Anything this module doesn't implement degrades to a silent no-op, never a panic.** This is
  deliberate: `build_biome_vegetation` resolves *every* biome's vegetation list at generator
  construction time, including biomes nobody asked for yet (jungle's giant trunks, dark oak's fancy
  trunk, acacia's rotated-block trunk, `FallenTreeFeature`, azalea's environment-scan placement). A
  `panic!` on any one of those would break world generation for every biome, not just the untested
  one. When adding a new trunk placer / foliage placer / feature size / block-state provider /
  placement modifier, its `try_parse` must return `Option`/degrade on anything unrecognised — follow
  the existing pattern, don't add a bare `panic!`.
- **Single-chunk only — no cross-chunk feature spill**, unlike the ore engine's real 3×3
  `blockStateWriteRadius(1)` driver (`crate::feature::apply_ore_step_3x3_per_source`). A tree or grass
  patch near a chunk edge that would spill into a neighbour in vanilla simply doesn't here — a write
  outside the local `0..16` footprint is dropped (`VegGrid::set_if_in_bounds`), not clamped, since
  there is no neighbour grid to write into. Extending this to a real 3×3+ driver (mirroring the ore
  stage's own shape) is the natural next increment and is not attempted yet.
- **No JVM oracle validates this stage yet.** `docs/worldgen-parity.md`'s harness composes shape +
  aquifer + biome + surface + carve + ore against a real vanilla dump, but vegetation is not part of
  `ComposedChunkOracle.java`, and no isolated oracle for it exists in `scripts/worldgen-oracle/`
  either. Every test in `vegetation.rs` and `worldgen_data.rs` checks internal consistency (does the
  engine do what the JSON says, does a control actually fire) rather than live-vanilla parity — see
  `vegetation.rs`'s own module doc "Scope" section for the full list of named approximations
  (`canSurvive` modelled uniformly as `VegetationBlock`'s rule, heightmap types collapsed from five to
  two scans, the beehive decorator's hive-row selection approximated).
- **Named per-species gaps within oak/birch/spruce themselves**: real vanilla plains/taiga roll a
  `random_selector` between several tree variants per attempt. Oak's `fancy_oak_bees_*` branch
  (~33% of attempts) and every species' `fallen_*_tree` branch are `ConfiguredFeature::Unsupported` —
  those attempts place nothing. Birch (only `fallen_birch_tree`, ~1.25%, unsupported) and
  spruce+pine together (only `fallen_spruce_tree`, ~0.8%, unsupported) are the most complete.

## Configuration

No env vars or flags. The per-biome vegetation list comes entirely from the embedded
`configured_feature/*.json` / `placed_feature/*.json` / `biome/*.json` data
(`crates/lodestone-server/assets/worldgen/`) via whatever `Resolver` the caller supplies —
`lodestone_server::worldgen_data::EmbeddedResolver` for the bundled singleplayer/integrated-server
generator, a test fixture `Resolver` for hermetic unit tests.

## Dependencies

- `crate::compose::resolve_block_tag` / `build_biome_vegetation` (this crate) — tag-closure resolution
  and per-biome step-list parsing, mirroring the ore engine's own `build_biome_ores`.
- `crate::noise::simplex` (this crate) — `SimplexNoise`/`biome_info_noise_value`, a separate noise
  primitive from the density-function router's `NormalNoise`, needed for
  `NoiseThresholdCountPlacement`'s grass/flower density gate.
- `crate::rng::{LegacyRandomSource, WorldgenRandom, RandomSource}` (this crate) — the same seeded-RNG
  machinery every other decoration stage uses.
- `lodestone_server::worldgen_data::EmbeddedResolver` — the real 26.2 data this stage's `Resolver`
  reads from in the bundled generator.
