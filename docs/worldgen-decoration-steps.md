# Worldgen decoration steps and feature types

## What it is

The `GenerationStep.Decoration` driver: which of vanilla's 11 decoration steps this
engine runs, which `configured_feature` types it can place, and which placement
modifiers it understands. Issue #513 took this from **1 step and 7 types** to
**8 steps and 30 types**. Companion docs: [`worldgen-vegetation.md`](./worldgen-vegetation.md)
for the placement engine itself, [`worldgen-parity.md`](./worldgen-parity.md) for what
the JVM fixtures measure.

## How it works

`compose::build_biome_decoration` resolves a biome's `features` array over
`compose::DRIVEN_STEPS` — steps 0 (`RAW_GENERATION`), 1 (`LAKES`), 2
(`LOCAL_MODIFICATIONS`), 3 (`UNDERGROUND_STRUCTURES`), 4 (`SURFACE_STRUCTURES`), 7
(`UNDERGROUND_DECORATION`), 8 (`FLUID_SPRINGS`), 9 (`VEGETAL_DECORATION`) — and
returns `(step, index-within-that-step, PlacedRef)` in step order.

`feature::vegetation::apply_decoration_steps` walks it: **one**
`set_decoration_seed` per chunk, shared across all steps (vanilla derives it once in
`applyBiomeDecoration`), then `set_feature_seed(decoration_seed, index, step)` per
feature. The `(index, step)` pair is what isolates a feature's RNG stream, which is
why the list carries both and must never be flattened into a running index.
`apply_decoration_steps_3x3_per_source` is the neighbourhood driver, identical in
shape to the vegetation one it replaced.

Not driven here: step 6 `UNDERGROUND_ORES` (its own engine, `feature/mod.rs`), step
10 `TOP_LAYER_MODIFICATION` (`feature/top_layer.rs`), step 5 `STRONGHOLDS` (zero
entries across all 66 bundled biomes).

**One stated deviation.** Because ore runs as its own earlier stage, steps 0–4 run
*after* ores here and *before* them in vanilla. Nothing in steps 0–4 reads a block
that ore placement writes — ore replaces stone with ore in place, so every
solidity/air question those steps ask answers the same either way — so this is a real
ordering difference with no known observable consequence. "No known consequence" is
not "none".

## Feature types and placement modifiers

Bodies live in `feature/vegetation/features.rs`, one `place_*` per vanilla
`Feature<…>` subclass, each reproducing its `place()` body's RNG draw order. The
world-state narrowings (`isFaceSturdy`/`isSolid`/`canSurvive`/`getSeaLevel`,
`scheduleTick`, block entities) are tabulated in that module's own doc comment.

Placement modifiers: 15 of vanilla's 15, plus `height_range` which previously existed
only in the ore engine — 86 bundled placed features use it, so before #513 every one
of them reached a decoration step and was silently dropped. `Positions::List` is the
fan-out variant `count_on_every_layer` and `fixed_placement` need.

Still `ConfiguredFeature::Unsupported`, and each is a real gap rather than an
oversight: `multiface_growth` (see below), `fallen_tree`, `bamboo`, `huge_fungus`,
`huge_*_mushroom`, `iceberg`, `geode`, `speleothem_cluster`, `large_dripstone`,
`root_system`, `scattered_ore`, `monster_room`, `fossil`, `basalt_columns`,
`delta_feature`, the coral set and the End set.

**`multiface_growth` (glow lichen, sculk vein) is ported but deliberately not
selected.** The port reproduces the search loop and every draw, and matched the JVM
SINGLE-mode dump on 23 of 24 cells at `vegetation_plains_land_jvm.txt` — one extra
`glow_lichen[up=true]`. 23 exact matches means the draw counts already agree, so what
is missing is a predicate, almost certainly `MultifaceBlock.canAttachTo`'s full-face
test, which this crate has no block-support-shape table to answer. Flipping it on is a
one-line change in `parse_configured_feature_doc` once that exists.

## How to change it, and the gotchas

* **Adding a type is three edits**: a `ConfiguredFeature` variant, a
  `parse_configured_feature_doc` arm, a body in `features.rs`. The `_ =>` in that
  parse is the island factory — a variant added without an arm parses to
  `Unsupported` and is silently never reached.
* **Never delete a variant to simplify.** An entry removed from a biome's step list
  shifts every later `set_feature_seed` index. `Unsupported` exists precisely so an
  unmodelled type stays in the list and stays inert.
* **`config::blocks_motion` is a deny-list and the default direction is
  load-bearing.** `VegGrid::height_ocean_floor` used to answer "topmost non-air,
  non-fluid"; seagrass is neither, so an already-placed plant counted as the ocean
  floor and the next placement on that column stacked on top of it — seagrass floating
  in open water, intermittently, because it needs two placements on one column.
  Vanilla's `OCEAN_FLOOR` tests `blocksMotion()`. There is no per-block-state property
  table in this crate, so `blocks_motion` lists vanilla's own `#minecraft:replaceable`
  members plus the non-motion-blocking states this engine places that the tag omits
  (kelp, sea pickles, sugar cane, flowers, nether vines, mushrooms). **Anything
  unlisted blocks motion**, which is byte-for-byte the pre-fix behaviour, so extending
  the list is the only way to change behaviour. Measured: 708 stacked seagrass cells
  over a 24×24 chunk sweep before, 0 after.
* **The height scans read the currently-mutating grid by design.** That is what gives
  a later feature write-visibility of an earlier one, and it is also what let the wrong
  predicate above *compound* rather than merely be wrong once. Any future height-scan
  change should be checked against a second placement on the same column, not a first.
* **`vegetation_seam_consistency.rs`'s totals are measurements, not floors.** They
  moved 44 → 77 (new modifiers draw, so trees downstream of one land elsewhere) and
  then 77 → 64 with the `blocks_motion` fix, while the narrow control went 94 → 120 →
  129. The two arms moving *further apart* is the direction that says the 5×5 read
  neighbourhood still buys what it claims; re-measure and record, do not widen.

## Configuration

None. Everything is read from `configured_feature`/`placed_feature`/`biome` documents
through `Resolver`. `LODESTONE_VEG_STRICT=1` turns an unmodelled dispatch into a panic
naming the type; `LODESTONE_VEG_SINGLE_SOURCE_DEBUG=1` runs the centre chunk only.

## Dependencies

`lodestone-worldgen-core` for RNG and density; the bundled
`crates/lodestone-server/assets/worldgen` tree as the production data source.
`lodestone_server::worldgen_data`'s `KNOWN_VEGETATION_GAPS` is the allow-list that
makes the remaining `Unsupported` set loud rather than silent — it must be updated
whenever a type lands here.
