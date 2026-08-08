# 3-D biome cells and ore veins

## What it is

Two independent worldgen additions that share a page because both changed what a
generated column *contains* rather than how it is driven: the full 4×4×4 biome grid
(issue #512) and `OreVeinifier` (issue #496).

## How it works

### Biome cells (#512)

`overworld::biome_cells::BiomeCells` holds one biome id per `QuartPos` cell —
`16 × height/4`, so 1,536 for a 384-block column, against the 16 the generator used to
produce. Ids are interned into a small per-column palette (`Vec<String>` +
`Vec<u16>`), which keeps the struct at ~3 KB and gives a section encoder its palette
for free.

`biome_cells_stage` samples the grid; `biome_stage` now takes the grid as a parameter
and *reads* the surface layer out of it. That is exact rather than approximate: the
surface sample height `(h >> 2) << 2` is already quart-aligned, so it is by
construction one of the grid's own layers.

Before this, one climate sample per horizontal quart was taken at that quart's surface
height and broadcast over all 384 blocks. `lush_caves`, `dripstone_caves` and
`deep_dark` are selected by the `depth` channel at low Y, so their climate region was
never queried: all three bundled, all three unreachable. Measured after the change, at
seed 42 over a 3×3 patch: surface biomes `{beach, dark_forest, river}`, 3-D biomes
those three **plus `lush_caves`**, occupying y −44..36 of column (0,0)'s first quart.

### Ore veins (#496)

`overworld::veins` — a per-generator `VeinPrograms` (three compiled router programs,
the `oreRandom` positional factory, pre-interned states for both `VeinType`s) and a
per-chunk `VeinChunk`. Applied in `materialize_world` for every cell the fill stage
reported as the default block, which is where vanilla's `MaterialRuleList` reaches
`OreVeinifier`. No feature-step RNG is involved, so veins structurally cannot desync
the ore or vegetation streams.

`overworld.json` already carried `ore_veins_enabled: true` and all three router
channels and nothing read any of them, so this was a live parity defect rather than a
missing feature.

## How to change it, and the gotchas

* **A vein wins over the surface diff.** Vanilla runs `buildSurface` *after* the fill
  that placed the vein, but `SurfaceSystem.buildSurface` opens with an
  `old == defaultBlock` test (`SurfaceSystem.java:151`) — a cell holding copper ore or
  granite is skipped by every surface rule. Applying the diff unconditionally erases
  **every** vein: the overworld surface rules write `deepslate` over the whole column
  below y ≈ 0, so a vein at y = −40 came back out as deepslate and the served chunk
  was byte-identical with veins on and off, with 491 confirmed `OreVeinifier` writes
  per sweep reaching zero blocks. Every counter along the way looked right; only a
  forced-write stress test separated "the predicate never fires" from "the write is
  erased".
* **A vein-negative fixture reads exactly like a broken feature.** The first sweep
  used chunks 0..4, which contain no veins at seed 42, and reported 0 differing cells.
  Widened to 8×8: 1308 cells differ — 1137 `deepslate → tuff`, 141 `→
  deepslate_iron_ore`, 6 `→ raw_iron_block`, 1 `stone → granite`.
* **`vein_toggle`/`vein_ridged` are `minecraft:interpolated`**, so they go through
  `NoiseChunkSampler` (one per chunk, cell caches assume it) rather than a pointwise
  `Density::compute`. A pointwise evaluation puts veins in roughly the right places
  with the wrong shape — the hard kind of wrong to see.
* **The divergent biome sampling heights must not be unified.** Carver and ore
  selection resolve at `y = 0`; vegetation at the surface. See `crate::biome`'s "y = 0
  trap": at `y = 0` the `depth` gradient is already ≈ +1.0, so a surface `dark_forest`
  chunk resolves as `lush_caves`. Having a 3-D grid gives each consumer its own correct
  Y; it does not license collapsing them.
* **Biome cost is 96× the samples per column.** That is what vanilla does and it is
  affordable only because the indexed `Climate.RTree` replaced the brute-force table
  scan. `biome_search_counters.rs` carries the derived prediction
  (`16 × 96 × pre-ore chunks`).
* **Neither has a JVM fixture.** Vein thresholds and block choices are transcribed from
  `OreVeinifier.java` and reviewed, not measured; a vein-positive dump is still the
  right gate. `surface_diff` is also computed from the pre-vein field, so vanilla's
  `stone_depth_above/below` counters see vein blocks and ours do not — narrow, and not
  measured.

## Configuration

`ore_veins_enabled` plus the three `noise_router` channels in the `noise_settings`
document; absent or false yields `VeinPrograms::build → None` and a vein-free world.
Biome cells need no configuration.

## Dependencies

Both consume `Resolver` data only. **The consumer half of #512 is not in this crate**:
`ChunkColumn` still carries 16 surface quarts and the chunk-data encoder still hoists
one 16-entry array out of the section loop, so a per-section biome container — and the
save round-trip that currently collapses cave biomes — needs a `lodestone-server` and
`protocol/v770` patch. `GeneratedColumn::biome_cells()` is the seam.
