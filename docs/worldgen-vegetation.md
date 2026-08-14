# Vegetal decoration: grass, flowers and trees

## What it is

`crates/lodestone-worldgen/src/feature/vegetation/` (a module, split by U16 Phase B into
`mod`/`config`/`grid`/`ids`/`place`/`tree` — the old single `vegetation.rs` file no longer exists) is
the engine that places grass, flowers and trees during world generation — issue #406, epic #404's
Phase 3, extended by issue #428. It is wired into `OverworldGenerator::column` as the last composed
stage, `VEGETAL_DECORATION`, right after #295's ore-feature stage, and — like the ore stage — over a
real 3×3 `center ± 1` neighbourhood (`apply_vegetal_decoration_step_3x3_per_source`), not a single
chunk in isolation: a tree or grass patch near a chunk edge really does spill into the neighbour that
generates it, matching vanilla's own cross-chunk decoration spill.

Trunk placers ported so far: `Straight` (oak/birch/spruce/pine's default), `Forking` (acacia),
`DarkOak` (dark oak, and pale oak for free — same placer, different providers), `Giant` (the shared
2×2 base under mega spruce/pine AND mega jungle), `MegaJungle` (`Giant`'s base plus radial branches),
`Fancy` (oak's `fancy_oak_*`/`fancy_oak_checked` branch, shared with jungle and dark_forest — the
slim-trunk-plus-scattered-limb shape, the single highest-value gap named in issue #428, closed in a
later pass). Foliage placers: `Blob`, `Spruce`, `Pine`, `Acacia`, `DarkOak`, `MegaJungle`, `MegaPine`,
`Bush`, `Fancy`. `FallenTreeFeature` — a real, distinct feature type, not a trunk/foliage placer at
all — is also modelled now: a stump plus a horizontal fallen log, reusing the same
`TrunkVineDecorator`/`AttachedToLogsDecorator` machinery `TreeConfig.decorators` already needed for
`jungle_tree`/`mega_jungle_tree`'s own `trunk_vine` entries (both landed together — `Decorator` is
shared across both feature types in real vanilla, not duplicated). Still unmodelled: mangrove's
`UpwardsBranchingTrunkPlacer`, `Cherry`, `Bending`.

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
  `VegetationBlock.canSurvive`), `tree` (`TreeConfig`: one of the trunk placers named above plus a
  matching foliage placer, `TreeFeature.doPlace`'s exact algorithm including the "not enough room"
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
  construction time, including biomes nobody asked for yet (mangrove's above-water-root trunk,
  cherry's trunk/foliage, azalea's environment-scan placement). A `panic!` on any one of those would
  break world generation for every biome, not just the untested one. When adding a new trunk placer /
  foliage placer / feature size / block-state provider / placement modifier / tree decorator, its
  `try_parse` must return `Option`/degrade on anything unrecognised — follow the existing pattern,
  don't add a bare `panic!`.
- **`TreeConfig.minimum_size`'s `min_clipped_height` is the one place a clipped tree still places.**
  Every species before `Fancy` only ever passed [`place_tree`]'s accept gate via `clipped ==
  tree_height` (no obstruction at all), so `clipped` and `tree_height` were interchangeable at every
  call site — until fancy oak's real `two_layers_feature_size` sets `min_clipped_height: 4`, meaning a
  *partially* obstructed fancy oak still grows, shorter, rather than being rejected outright. The
  trunk placer receives `clipped_tree_height` (NOT the original `tree_height`) for exactly this
  reason; `foliage_height`/`leaf_radius` still use the original, matching vanilla's own
  `TreeFeature.doPlace` evaluation order (both computed before the clip scan runs at all).
- **No JVM oracle validates this stage yet.** `docs/worldgen-parity.md`'s harness composes shape +
  aquifer + biome + surface + carve + ore against a real vanilla dump, but vegetation is not part of
  `ComposedChunkOracle.java`, and no isolated oracle for it exists in `scripts/worldgen-oracle/`
  either (`scripts/worldgen-oracle/VegetationOracle.java` is a *self-authored* oracle used by
  `tests/vegetation_parity.rs` — agreement with it is weaker evidence than a captured vanilla byte
  stream, see that test file's own doc). Every test in the `vegetation` module and `worldgen_data.rs`
  otherwise checks internal consistency (does the engine do what the JSON says, does a control
  actually fire) rather than live-vanilla parity — see the module's own doc "Scope"/"Approximations,
  named" sections for the full list of named approximations (`canSurvive` modelled uniformly as
  `VegetationBlock`'s rule, heightmap types collapsed from five to two scans, the beehive decorator's
  hive-row selection approximated).
- **Named per-species gaps, even within the implemented trunk/foliage placers**: real vanilla
  plains/taiga/jungle/etc. roll a `random_selector` between several tree variants per attempt. Now
  that `fancy_oak_*`/`fancy_oak_checked` and every species' `fallen_*_tree` branch are both modelled,
  the remaining `ConfiguredFeature::Unsupported` tree-shaped gaps are narrower: mangrove_swamp
  (`UpwardsBranchingTrunkPlacer`), cherry_grove (`CherryTrunkPlacer`) and any biome whose selector
  reaches a bare `BendingTrunkPlacer`. See `lodestone_server::worldgen_data::KNOWN_VEGETATION_GAPS`
  for the maintained, gate-enforced per-biome list of exactly which reasons remain — pruned to match
  `vegetation_placer_gaps_are_named_not_silent`'s own measured surface once `fancy_oak_*`/
  `fallen_*_tree` actually landed (every `fallen_tree` row is gone, and `"tree: unsupported..."`
  survives only where a biome's own placer is genuinely unported — mangrove_swamp and cherry_grove).
  See the `vegetation` module's own doc "Named per-branch gaps" section for the reasoning behind
  which placer landed when.
- **`TrunkVineDecorator`/`AttachedToLogsDecorator` are shared between `ConfiguredFeature::Tree` and
  `ConfiguredFeature::FallenTree`** — both are `TreeDecorator` subclasses in real vanilla, and
  `Decorator` (`config.rs`) models them once rather than per feature type. `jungle_tree`/
  `mega_jungle_tree`'s own `decorators` list (not `fallen_*_tree`'s `stump_decorators`/
  `log_decorators`) is what first exercises `TrunkVine` for a *vertical* log list — real vanilla's
  `TreeDecorator.Context` sorts by Y before either decorator runs, which is a no-op for a fallen tree's
  single-Y horizontal log but load-bearing there. See `place_trunk_vine_decorator`'s own doc comment.

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
