# Nether and End worldgen

## What it is

The composed Nether and End generators (`lodestone_worldgen::nether::NetherGenerator`,
`::end::EndGenerator`) and the engine-level differences from the Overworld that make them possible:
a selectable RNG family, two bespoke Nether biome noises, a disabled-aquifer fill, dimension-specific
cell geometry, the `minecraft:end_islands` density function, and the End's non-multi-noise biome
source. All bundled 26.2 data for both dimensions is complete; every remaining gap is engine
(unwritten decoration/structures) or gameplay (portal travel, the dragon fight) rather than missing
data.

## How it works

### What is shared, and the one flag everything else waited on

`noise_settings/{nether,end}.json` both set `legacy_random_source: true` (the Overworld does not),
which switches vanilla's **entire** noise stack to the legacy LCG family rather than xoroshiro —
`rng::Algorithm` and the `Copy` two-variant enums `AnyRandomSource`/`AnyPositionalFactory` make this a
per-dimension constructor argument (`density::Builder::with_algorithm`) rather than a generic
parameter threaded through every stage; the Overworld's own output is unchanged and byte-identical
across the change. Both dimensions also set `aquifers_enabled: false`, which is a bypass rather than
new logic — vanilla's disabled aquifer is just `density > 0 ? solid : globalFluid.at(y)` — and both
their noise settings feed a cell geometry derived from `size_horizontal`/`size_vertical` rather than
the Overworld's hardcoded 4-wide/8-tall assumption (the End's `2, 1` gives an **8-wide/4-tall** cell,
the transpose of the Overworld/Nether's `1, 2`).

Both generators run vanilla's own stage order: structure starts → refs → beardifier → fill (shape,
with the disabled-aquifer fluid picker) → biome → surface → carve → structure placement. Neither
stage reads a neighbour's *terrain* product (only the starts map, a pure function of `(seed, chunk)`,
is memoised), so both are pure functions of `(seed, chunk)` and can be generated in any order on any
thread.

### Nether

Biome assignment is two-dimensional and *is* the map: the Nether's climate-parameter table
(5 rows, one per biome, derived from vanilla's registry rather than shipped as reusable JSON) has all
non-temperature/humidity channels zeroed and every row a degenerate point (no ranges), and the two
noises behind temperature and humidity (`nether/temperature`, `nether/vegetation`) are seeded
specially — `LegacyRandomSource(seed + 0)` / `(seed + 1)`, the raw world seed plus a small offset,
**not** a positional fork, regardless of the legacy-random-source flag. Both noises declare only two
octaves but a nonzero `firstOctave`, so vanilla constructs and discards a zero'th-octave noise and
skips several more before building the two it keeps — porting the octave *count* without the
skipped-draw count consumes the wrong amount of randomness and produces a plausible but wrong Nether.
The lava "sea" (`sea_level 32`, `default_fluid` lava) is the disabled aquifer's global fluid picker,
not a real aquifer — and the Overworld's `-54` deep-lava threshold is unreachable at the Nether's
`min_y 0`, so it is not "an aquifer whose second fluid is lava," it is a flat fluid boundary at y=32.
Surface rules use a strict subset of the Overworld's condition types (no new condition type needed).
The `nether_cave` carver shares `CaveWorldCarver`'s codec with the Overworld's `cave` carver but
overrides thickness, Y-scale and its block-write rule — routing it through the Overworld's thickness
formula desyncs the RNG stream on the very first tunnel.

Structures: the same `structure::beardifier`/`StructureRegistry` machinery the Overworld uses,
filtered per-dimension (`StructureRegistry::new_for_biomes`) to only the sets whose biomes exist here,
plus a dimension-specific height probe over this dimension's own density field. `bastion_remnant`
places real blocks (jigsaw); `nether_fossil` places real blocks (template, and the dimension's only
adaptation-bearing structure, so it is what first made the Nether's beardifier observably non-empty);
`fortress` and `ruined_portal_nether` have no piece generator yet and yield advisory (blockless)
starts.

A biome resolution tie at one recorded coordinate disagrees with the real vanilla world by
construction — an exact climate-distance tie where vanilla's answer depends on the previous query on
the same thread (see `worldgen-biomes.md`'s tie-break discussion), which this engine's
demand-ordered, reorderable generation model cannot and must not reproduce. This is the one place
where matching vanilla bit-for-bit and being deterministic are incompatible, and determinism wins;
the parity gate classifies such disagreements as admissible only when the two candidate biomes are an
exact fitness tie, rather than tolerating them by threshold.

### End

The biome source is not multi-noise at all: `TheEndBiomeSource` is a small closed-form function of
chunk position (a radius-64-chunk main-island hole) and one density sample (the `erosion` router
slot, which for the End is exactly `cache_2d(end_islands)`), thresholded into five biomes. All five
biome ids are constants (the registry, not JSON). `minecraft:end_islands` is the one density-function
type the engine still needs to port — a seedless-looking `SimpleFunction` whose real seed (the raw
world seed, via `LegacyRandomSource`, **independent of** the legacy-random-source flag) is substituted
at wiring time, followed by 17,292 discarded RNG draws before a `SimplexNoise` is built. Its integer
division must stay Java-truncating (not `div_euclid`), several intermediate values are `f32` not
`f64`, and two boundary tests operate at different integer widths (`i32` vs `i64`) — six independent
ways to port it plausibly wrong. It is deliberately not yet implemented, sequenced instead as part of
the density-engine rewrite so it is written once against the final interpreter rather than ported
twice.

The End has no fluid at all (its sea level and fluid-level settings make the disabled aquifer's fluid
picker return air everywhere, regardless of what `default_fluid` names) and no bedrock (its surface
rule is a same-value no-op and, unlike the Nether, contains no `vertical_gradient` construct at all —
copying the Nether's floor/roof shape here would be actively wrong). There is no carver (no bundled
End biome names one) and no structure stage (`end_city`'s only structure has no piece generator yet,
so a stage today would place starts nothing could build).

**The End has no vanilla block oracle at all** — the reference save never visited it — so every End
gate derives its expectation from a record definition, arithmetic, or a cross-dimension control
(comparing against the Nether's own gates on the same mechanism) rather than a captured vanilla dump.
Treat End-specific numeric claims as weaker evidence than the Nether's or Overworld's for this reason.

### Decoration and structures, remaining

Every biome document for both dimensions already carries its full decoration step-list wiring and
every referenced `configured_feature`/`placed_feature` is bundled — glowstone, nether wart, crimson/
warped vegetation, basalt pillars, obsidian pillars, chorus plants, gateways-as-features are all
**composition work in the shared decoration engine** (see `worldgen-decoration.md`), not missing
data. `end_city` and the Nether's `fortress` are the remaining structures with no piece generator.
Portal travel, the dimension registry, and reaching either dimension from a live server are
`lodestone-server`-side gaps: `EmbeddedResolver` still hardcodes the Overworld's documents for the
default singleplayer path, though `EndChunkSource`/`NetherChunkSource`-shaped wrappers and a real
`Dimension` enum variant exist for both — see `docs/nether-portals.md` for the remaining diff.

## How to change it

- **Never assume a dimension can reuse the Overworld's fixed 4×8 cell geometry** — read it from the
  settings document (`aquifer::cell_geometry`) per dimension.
- **A biome name a dimension's generator can produce must have its carver/feature lists resolved at
  construction**, or its columns silently never carve/decorate.
- **The Nether has no fixed-biome fallback, deliberately** — an empty biome-parameter table panics
  rather than degrading to a uniform biome that looks fine in a screenshot but is wrong.
- **Do not copy the Nether's bedrock floor/roof shape into a new dimension without checking its
  surface rule for a `vertical_gradient` construct first** — the End's absence of one is the tell that
  it has no bedrock at all.
- **Any RNG draw-count change (skipped octaves, carver thickness draws, structure draws) changes the
  generated world** even when every affected value still looks plausible; the biome gates catch a
  wrong noise seeding immediately, but a wrong carver draw count is only visible in a full block
  comparison.
- **Data refresh after a version bump**: re-extract via the `#[ignore]`d gate in
  `crates/lodestone-data/tests/worldgen_dimension_data.rs`
  (`LODESTONE_REGEN=1 cargo test -p lodestone-data --test worldgen_dimension_data … -- --ignored`);
  the Nether's climate-parameter table has no jar entry to copy and is regenerated from
  `NetherParametersOracle` instead.

## Configuration

No runtime configuration; both dimensions' data is embedded at build time by
`lodestone-server`'s `build.rs` under `assets/worldgen/` (`noise_settings/{nether,end}`,
`noise/nether/{temperature,vegetation}`, `biome_parameters/nether`, the five Nether and five End
biome documents, `density_function/{nether,end}/base_3d_noise`, `end/sloped_cheese`,
`configured_carver/nether_cave`).

## Dependencies

`lodestone-worldgen`'s `aquifer`, `biome`, `compose`, `surface`, `dense_grid`, `interner`,
`structure::beardifier`; `lodestone-worldgen-core`'s `density`, `engine`, `noise::SimplexNoise`,
`rng::{Algorithm, LegacyRandomSource}`. Evidence: the Nether is verified against a real vanilla
26.2 server's own generated region files (`.cache/mc/survival/world/dimensions/minecraft/the_nether`,
seed −195764831) for both biome assignment and bedrock shell; the End has no such oracle (see above).
`scripts/worldgen-oracle/{NetherParametersOracle,DensityOracle}.java` for the Nether's climate table
and future `end_islands` verification. See `docs/worldgen.md` for the shared density/RNG engine,
`docs/worldgen-biomes.md` for the Overworld's own climate search and tie-break behaviour this doc's
Nether section builds on, and `docs/worldgen-structures.md` for the shared structure machinery.
