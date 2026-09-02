# Overworld biome assignment and surface material

## What it is

How a generated column gets a real biome instead of one hardcoded value, and how that biome then
picks surface material, terracotta banding and snow/ice cover. Four things share this doc because
they form one pipeline: climate sampling and search (`biome/`), a full 3-D biome grid plus ore veins
(`overworld/biome_cells.rs`, `overworld/veins.rs`), surface rule application (`surface/`), and the
final `TOP_LAYER_MODIFICATION` decoration step (`feature/top_layer.rs`).

## How it works

### Climate search

Each column samples six climate noises (temperature, humidity, continentalness, erosion, depth,
weirdness) already computed for terrain shape, and finds the nearest match by squared distance in a
~7,594-row `(climate range, biome)` table (`biome_parameters/overworld.json`, dumped once from
vanilla's own biome-parameter-list bootstrap, since the ~700-row table it logically represents
is built by 1,124 lines of Java control flow rather than shipped as data). The search is a real port
of vanilla's own climate-space R-tree (fan-out 6, built by vanilla's own splitting heuristic so node
order is
reproducible), not the brute-force reference next to it — the two structures agree on the minimum
distance always, but resolve an exact **tie** differently, and vanilla's tree is the one a real
server runs. A tie is the only place they can disagree, and it is real: vanilla's tie-break depends
on the previous search on the same thread (a thread-local "last result" field on the tree), which this engine
deliberately does not reproduce — a demand-ordered, potentially reordered generator cannot have
history-dependent output. The fresh-instance answer (first leaf reached with no seed) is what's
implemented; a per-source-chunk memo (thread-local, direct-mapped on the low bits of chunk
coordinates, collision-free for any window the carver/ore drivers actually use) avoids repeating the
search across a 17×17/3×3 neighbourhood walk.

**Sampling height matters and is per-consumer, not unified.** Carver and ore selection resolve a
source chunk's biome at `y = 0`; vegetation resolves at the column's own generated surface height.
At `y = 0` the `depth` channel's gradient is already ≈ +1.0 (climate-space "deep cave"), so a surface
`dark_forest` column can resolve as `lush_caves` there — correct for carve/ore (which care about
underground content) and wrong for vegetation (which cares what grows on top). Collapsing these two
conventions to one is the trap that has shipped before.

### 3-D biome cells and ore veins

`overworld::biome_cells::BiomeCells` samples a full 4×4×4-per-chunk quart grid (not one climate
sample per horizontal quart broadcast vertically), which is what makes cave-only biomes
(`lush_caves`, `dripstone_caves`, `deep_dark`) reachable at all — their climate region lives at low
Y, which a surface-height-only sample never queries. `overworld::veins` composes `OreVeinifier`
(copper/iron ore-vein noise channels the settings document already carried) into the fill stage,
applied only where the fill left the *default* block — since surface rules only ever rewrite a
cell that still holds the default block, a vein must be applied inside `materialize_world` in the
right order or the surface pass silently erases it.

### Surface rules

`SurfaceSystem` interprets vanilla's `surface_rule` tree per column, now against a per-column `Ctx`
carrying the real sampled biome and temperature (before biome variety existed, these were build-time
constants for one fixed biome). Vanilla's Badlands surface rule (its own terracotta-banding
lookup) is ported — `Rule::Bandlands`/`BandBlocks`, a 192-entry table built once per
world seed and now interned to `StateId`s rather than re-derived per probe, with `math::round`
providing Java's half-up rounding semantics `f64::round`'s half-away-from-zero does not match.

Performance-wise, the surface stage historically dominated worldgen's heap allocation (every probe
built and cloned `String`s for `pre`/`biome_at`/matched-rule results); it is now interned end to end
— `PreState` carries a `StateId` plus a cheap `PreClass` (air/fluid/stone) rather than deriving
classification from a string per probe, and the diff handed to materialisation is a `FastMap` keyed
by position, read only by point lookup (never iterated) so hasher choice cannot leak into palette
order.

### Freeze-top-layer (snow and ice)

The final decoration step, `TOP_LAYER_MODIFICATION`, walks every column and (a) freezes the block
below the water surface to ice if the biome's `shouldFreeze` predicate is true, and (b) places a
snow layer on top if `shouldSnow` is true, flipping the block below to its `snowy` variant. It
consumes **no RNG** (vanilla's own feature draws nothing), so it cannot desync anything upstream or
downstream, and it must run after vegetation (snow sits on top of a tree canopy, not the pre-tree
terrain). The one real trap is that vanilla's temperature test is **height-adjusted** — biome
`temperature` alone is not enough above `sea_level + 17`, a noise-shaped correction lowers the
effective temperature with altitude, and a frozen-ocean biome's `FROZEN` modifier further blends a
patchy ice mask rather than freezing solid. Whether a block can hold a snow layer or freeze is not a
data property; it comes from four jar-dumped per-block-state facts (`lodestone_data::snow_support`,
`block_solidity::blocks_motion`), because collision/support geometry is code, not JSON.

## How to change it, and the gotchas

- **Do not unify the y=0 and surface-height biome sampling conventions.** They answer genuinely
  different questions for genuinely different consumers.
- **Never seed the biome search.** A "pruning hint" reproducing vanilla's `lastResult` carry-over
  makes output depend on search history, which is incompatible with a generator whose columns can be
  requested in any order on any thread.
- **A biome resolved per chunk position should go through the existing thread-local memo**, keyed by
  table identity as well as coordinates (two generators on one thread must never share biomes), not a
  second cache — see `docs/worldgen.md`'s memoisation guidance.
- **`freeze_top_layer` must stay after vegetation** in the stage order, since its height read
  (topmost non-air) includes anything vegetation placed.
- **A vein or any other fill-stage write must land before the surface diff is applied**, or the
  surface pass's "only touch cells still holding the default block" rule silently erases it.
- **Adding a `Cond`/`Rule` variant to the surface interpreter**: intern any result state in the
  parser, never inside the per-probe scan; ask whether the value set the new rule computes over is
  finite (most are) before assuming it can't be interned.
- **cold_enough_to_snow is an approximation** (declared biome temperature against a fixed threshold,
  no per-block height adjustment or `temperature_modifier`) reused from before real biome variety
  existed; revisit if a snow-line seam near `sea_level + 17` ever needs the exact vanilla answer.

## Configuration

No env vars or feature flags. The climate table, temperature table and per-biome surface/freeze data
all come from bundled JSON (`biome_parameters/overworld.json`, `biome/*.json`) through `Resolver`;
regenerate the climate table from a fresh `BiomeOracle table` dump if the underlying game data
changes. `gen-counters` (default off) is required for the biome-search and cache counters.

## Dependencies

`lodestone-worldgen-core`'s `density`/`counters`; `lodestone-worldgen`'s `biome`, `surface`,
`overworld::{biome_cells, veins}`, `feature::top_layer`; `lodestone_data::{snow_support,
block_solidity}` for the freeze-layer per-block facts; `lodestone-server`'s `EmbeddedResolver` for the
bundled 26.2 data and the served biome quart grid (`ChunkColumn::biome_state`); `protocol/v26-2`'s
`server_protocol::build_world_column` for the wire biome container. Oracle provenance:
`scripts/worldgen-oracle/{BiomeOracle,TopLayerOracle}.java`, both run via `scripts/worldgen-oracle/run.sh`.
