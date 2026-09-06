# Decoration: features, vegetation, ores and generation-time mob spawns

## What it is

Everything that runs after terrain shape and biome assignment to make a chunk look inhabited:
the `GenerationStep.Decoration` driver and its placement-modifier interpreter, the vegetation engine
(grass, flowers, trees), ore-vein placement and allocation, and the one-shot animal spawn vanilla
performs at chunk generation. All four are interpreters over the same kind of data (`configured_feature`
/ `placed_feature` / per-biome step lists) and share one seeding discipline: one `set_decoration_seed`
per chunk, then `set_feature_seed(decoration_seed, index, step)` per feature, so a feature's `(step,
index)` pair — never a flattened running count — is what isolates its RNG stream from its neighbours'.

## How it works

### Decoration steps and feature types

`compose::build_biome_decoration` resolves a biome's `features` array over the driven steps —
`RAW_GENERATION`, `LAKES`, `LOCAL_MODIFICATIONS`, `UNDERGROUND_STRUCTURES`, `SURFACE_STRUCTURES`,
`UNDERGROUND_DECORATION`, `FLUID_SPRINGS`, `VEGETAL_DECORATION` — into `(step, index, PlacedRef)`
triples in step order. `UNDERGROUND_ORES` and `TOP_LAYER_MODIFICATION` are separate engines with
their own docs (ore allocation below; freeze/snow in `worldgen-biomes.md`); `STRONGHOLDS` has zero
entries across every bundled biome and is not driven.

Placement modifiers (count, in_square, heightmap, biome, rarity_filter,
surface_water_depth_filter, noise_threshold_count, random_offset, block_predicate_filter,
height_range, and the list fan-out `Positions::List`/`count_on_every_layer`/`fixed_placement` need)
compose as a depth-first flat-map, exactly reproducing vanilla's `Stream` pipeline's draw order.
Configured-feature bodies (`simple_block`, `tree`, `random_selector`/`simple_random_selector`,
`speleothem`, and the vegetation-specific ones below) each reproduce their vanilla `place()` body's exact draw
sequence. An unmodelled feature type or placement modifier degrades to a silent, RNG-free no-op
(`ConfiguredFeature::Unsupported`) rather than a panic — the census resolves every bundled biome's
step list at generator construction time, including biomes nobody has tested yet, so a hard failure
on one unmodelled type would break every biome's world generation, not just the untested one.
Currently unmodelled: `huge_fungus`, the End feature set, and several
rarer single-use types — each
tracked by name in `lodestone_server::worldgen_data::KNOWN_VEGETATION_GAPS`, which must be updated
whenever a type lands so a regression (or a fixed gap that should be pruned) is loud rather than
silent.

Simple-block state providers include fixed, weighted, threshold-noise, noise and dual-noise forms.
The two noise forms construct their deterministic fields from the bundled seed and octave data at
selection time; dual noise first selects the fast-field frequency, then selects the output state.
Block columns also accept weighted nested height providers and randomized integer state properties,
which covers the hanging cave-vine records; bamboo uses the configured floor tag, stalk states and
optional podzol disk.

The single speleothem feature resolves its anchor-holder tag at construction, chooses an upward or
downward point from the two adjacent anchor candidates, then writes its base patch before its one-
or two-segment pointed state. Its horizontal patch branches and their nested direction draws occur
whether or not a candidate cell can be replaced; that draw order is part of the feature's result.
It runs through the same 3×3 per-source decoration driver as every other configured feature, so an
anchor patch from a source at a chunk edge may legitimately write into its neighbouring chunk. The
external `speleothem_feature_jvm.txt` fixture places at the east edge and asserts the western spill,
the tag-backed cinnabar-to-sulfur replacement, and the generated pointed-state properties together.

**One stated ordering deviation from vanilla**: because ore runs as its own earlier stage here,
decoration steps 0–4 run after ores in this engine and before them in vanilla. Nothing in those
steps reads a block ore placement writes in a way that changes an answer (ore replaces stone with
ore in place, so every solidity/air test upstream sees the same thing either way) — a real
difference with no known observable consequence, which is not the same claim as "none".

### Vegetation

`feature/vegetation/` places grass, flowers and trees over a real 3×3 neighbourhood (a tree or patch
straddling a chunk edge genuinely spills into whichever chunk generates it, matching vanilla's own
cross-chunk decoration spill) via `VegGrid`, a mutable chunk-local block field seeded from the
composed post-ore grid and folded back afterward. Trunk placers cover straight, forking, dark-oak
2×2, giant/mega-jungle, fancy, cherry and mangrove's upwards-branching shape; foliage placers cover
the vanilla equivalents plus cherry's hanging-leaves pass and mangrove's dart-throw scatter; a
mangrove root placer and a fallen-tree feature (stump + horizontal log, sharing decorator machinery
with standing trees) are also modelled. Huge red and brown mushrooms use their configured ground
tags, a random four-to-six-block stem height, and their distinct directional cap layouts (brown's cornerless
square versus red's three rim layers and smaller filled top). Every reachable overworld biome's tree content is now
covered by a real placer. Multiface growth writes complete directional state, checks its support,
and performs its one seeded outward spread; the external single-source and 3×3 fixtures compare that
layout exactly. The fixed-seed `vegetation_mushroom_fields_neg1_0_jvm.txt` and
`vegetation_mushroom_fields_5_5_jvm.txt` external captures exercise the production mushroom-fields
selector; their composed 3×3 replays contain both cap variants and their stems, so the selector path
cannot regress while feature-local geometry tests remain green. The
remaining named gaps degrade individually rather than disabling the whole tree.
Root systems scan upward for a valid nested feature site, scatter the root-column replacement only
after that nested placement succeeds, then independently scatter hanging roots from supported ceilings.
Coral tree, claw and mushroom forms share the tagged coral state choice and water gate but retain
their distinct trunk, branched and hollow-shell geometries.

The direct compiled-server maps for root systems and all three coral forms are exact, including
their blocked-origin and dry-water controls. Their real-biome composition captures remain an
explicit production gap: `warm_ocean` currently diverges on seagrass and multiface writes, and
`lush_caves` remains pending an exact composed replay. Keep `coral_*` and `root_system` in
`KNOWN_VEGETATION_GAPS` until the ignored composed-fixture gate in
`vegetation_parity.rs` passes; a successful feature-local map is not permission to remove that
end-to-end ledger entry.

Placement is off block-state strings only at the edges: tag-membership questions (17 of them — 11
registry tags plus 6 base-name equalities) are answered by fixed bitsets indexed by `StateId`,
exact and never needing to grow since a `StateId` is a `u16`. A bit above the interner's watermark
(minted *during* the current decoration pass — a rewritten leaf's `distance=N` state, for instance)
falls back to the pre-bitset string path, which is a correctness requirement, not a slow path: an
unexamined id would answer every tag query `false`, which changes what decorates where. Two derived
per-position values (`distance=N` leaf rewrite, `waterlogged` fix-up) are memoised `id -> id`
lookups rather than re-derived per call.

### Ore allocation

`feature/mod.rs`'s ore engine (`UNDERGROUND_ORES`) is the same placement-modifier/positions shape as
vegetation, composed into `column()` over the real vanilla 3×3 `blockStateWriteRadius(1)` driver, per
-source biome resolution included. `OrePositions::{None, One, Repeat}` replaces a
per-attempt-allocated `Vec<BlockPos>`, matching vegetation's `Positions` shape; per-blob scratch (the
sphere-fill table and its visited-bitset) is taken from and returned to thread-local free lists rather
than allocated fresh per ore blob. **A recycled visited-bitset must be cleared before resize, not
just resized** — `Vec::resize` only zeroes newly-added elements, so a buffer recycled from a larger
blob can carry stale set bits into a smaller one, which makes the placer skip a position it must
place at — a dropped ore, not a slow one. RNG draw order and count are unaffected by any of the
allocation work above; the surface stage (see `worldgen-biomes.md`), not the ore engine, is where
worldgen's remaining string-classification cost actually lives.

### Generation-time mob spawns

Vanilla's `ChunkStatus.SPAWN` step places one weighted-species animal pack, once, the moment a chunk
first generates — `spawn_stage::spawn_candidates_for_chunk` is the pure, version-free pick (one
species from the biome's `spawners.creature` list, one pack, one position), riding on
`GeneratedColumn::spawn_candidates`. It is deliberately **not** light-aware: `lodestone-worldgen` has
no light engine, so the raw candidates are re-validated server-side
(`natural_spawn::validate_generation_spawns`) against the same per-species `SpawnRule` and real
column light the tick-driven spawn cycle already uses, before anything is actually spawned. This is
genuinely one-shot: `ChunkColumn::generation_spawns` is populated only in `ChunkColumn::from_generated`,
which only runs on a true disk-miss, so a reloaded chunk never re-proposes candidates, and any mob
that does spawn is covered by the same entity persistence every other mob uses — no bespoke
persistence was needed. Known scope cuts: only the mob-simulation's fixed initial snapshot area gets
generation-time spawns (chunks streamed in later as a player walks do not, matching the existing
tick-driven spawner's own scope), one pick per chunk rather than vanilla's bounded retry loop, and a
group's wander clamps to its own chunk rather than reading a neighbour.

## How to change it, and the gotchas

- **Adding a feature type or placement modifier is a fixed three-edit shape**: a variant, a parse
  arm, a body. The parse function's catch-all is the island factory — a variant added without an arm
  silently becomes `Unsupported`.
- **Never delete or renumber a step-list entry to "clean it up".** Every entry's raw array position
  feeds `set_feature_seed`; removing one shifts every later feature's seed and changes the whole
  chunk downstream of it. An unmodelled type stays in the list as `Unsupported` for exactly this
  reason.
- **A height-scan default direction is load-bearing.** The ocean-floor height scan answers "topmost
  non-motion-blocking" using a deny-list of known exceptions (matching vanilla's `blocksMotion`) —
  anything unlisted counts as solid ground, so extending the list (not narrowing it) is the only safe
  way to fix a plant that floats or double-places.
- **Height scans read the currently-mutating grid on purpose**, which is what lets a later feature in
  the same pass see an earlier one's writes — exactly like vanilla. A wrong predicate here compounds
  rather than merely repeating, so verify against a *second* placement on the same column, not just
  the first.
- **Adding a modifier or fan-out that can produce genuinely different positions must get its own
  `Positions`/`OrePositions` variant.** `Repeat(pos, n)` means the same position n times; smuggling a
  real fan-out into it silently changes what a whole class of placements does.
- **`LODESTONE_VEG_STRICT=1`** turns any unmodelled dispatch into a named panic instead of a silent
  no-op — use it when developing a new placer so a missing arm surfaces immediately rather than as a
  quietly-wrong world.

## Configuration

None beyond debug/test escape hatches: `LODESTONE_VEG_STRICT=1` (panic on unmodelled dispatch),
`LODESTONE_VEG_SINGLE_SOURCE_DEBUG=1` / `LODESTONE_ORE_SINGLE_SOURCE_DEBUG=1` (run the centre chunk
only, bypassing the 3×3 driver — debug-only, never on a production path). Everything else is read
from bundled `configured_feature`/`placed_feature`/`biome` JSON through `Resolver`.

## Dependencies

`lodestone-worldgen-core`'s `rng`/`density`/`counters`; `lodestone-worldgen`'s `compose` (tag
resolution and per-biome step-list parsing, shared verbatim between the ore and vegetation engines),
`feature::region_view` (the 3×3 read/write routing both drivers share), `interner`
(`StateId`/`StateInterner`). `lodestone_entity::spawn` (`SpawnConditions`/`MobCategory`) and
`lodestone-server`'s `natural_spawn` for generation-time spawn validation — see
`docs/natural-mob-spawning.md` and `docs/biome-spawners.md` for the tick-driven spawn cycle this
reuses. Verified against vanilla via `scripts/worldgen-oracle/VegetationOracle.java` and the ore
engine's JVM fixtures under each crate's `tests/support/`.
