# Worldgen registry-consumption census

## What it is

`crates/lodestone-worldgen/tests/registry_consumption_census.rs` is a data-only coverage gate for
the configured-feature and placed-feature records reachable from the three bundled dimensions. It
checks that every declared placement modifier survives parsing, in order, so an unrecognised record
cannot quietly change the candidate-position stream.

## How it works

The test loads the bundled biome, placed-feature and configured-feature JSON directly through a
small `Resolver` implementation. The Overworld set comes from its biome-parameter table (55
biomes); Nether and End sets come from their biome tags (5 biomes each). The sets must be disjoint
and cover all 65 dimension biomes.

For each biome, the census follows every feature-step holder, resolves configured-feature selectors
and inline holders recursively, and records the configured type names it encounters. The raw type
set is compared with the common parser inventory and an explicit list for families consumed by a
separate dimension or stage path. Any new configured type fails with the dimension that reaches it.

The placed-feature check compares each raw `placement` array with the parsed `VegPlacement` array.
It checks both length and the type at every position. The current data exercises 15 modifier kinds:
biome, block-predicate filtering, count, count-on-every-layer, environment scanning, fixed
placement, height ranges, heightmaps, square spreading, noise-based counts, noise-threshold counts,
random offsets, rarity filters, surface-relative thresholds, and surface-water-depth filters.

The nested `clamped` integer provider is represented by `IntProvider::Clamped`; it samples its
source provider first and then applies the inclusive bounds. This matters for flower records whose
count provider is a clamped uniform source: treating it as an unsupported provider drops one
modifier and changes the stream. The focused parser test also verifies the nested shape and the
unit test checks lower, interior, and upper clamp values without relying on a random draw.

## How to change it

When adding a placed-feature modifier, extend `VegPlacement`, its parser and the census's exhaustive
`placement_type` mapping together. Add a representative bundled record or a focused parser test so
the raw-versus-parsed comparison proves both recognition and declaration order. When adding a
configured-feature family, update the appropriate parser or separate consumer and then move its
identifier into the common inventory only once the consumer is wired.

The out-of-band list is intentionally small and explicit. It currently covers End-specific records,
Nether scattered ore and ordinary ore, stage-specific records such as top-layer freezing and
fossils, and known composed vegetation gaps. Do not add a type merely to make a failure green:
first establish which production path consumes it or implement its parser/body. The test uses only
reachable biome entries, so a registry record that is not referenced by a dimension is not evidence
of runtime coverage.

## Configuration

The test reads from `crates/lodestone-server/assets/worldgen` relative to the worldgen crate's
manifest directory. It has no environment variables, network access, Java oracle, or container
dependency. Dimension membership is determined by the bundled Overworld parameter table and the
Nether/End biome tags.

## Dependencies

The census uses `lodestone-worldgen`'s public `Resolver`, `VegPlacement`, `ConfiguredFeature`,
`PlacedRef`, and `collect_unsupported` APIs, plus `serde_json` for raw registry records. Production
decoration remains responsible for applying the resolved records; this test only proves registry
reachability and parser consumption.
