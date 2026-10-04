# 26.3 density engine

## What it is

`lodestone_worldgen_core::engine::release26_3` evaluates the 26.3 `noise_settings` terrain shape: the router's density functions in 32-bit float, plus the noise-based aquifer and the per-chunk fill that turns density into stone, air, water and lava (before surface rules). It is bit-identical to the real 26.3 server on every oracle fixture.

## How it works

Pipeline: JSON, then `tree::Tree`, then `compile::Compiler`, then `sampler::Program`.

- `tree` parses a function document into a hash-consed arena (structural equality is id equality; floats compare by bits). It also computes `interval::Interval` ranges and per-node axis-variation masks.
- `compile` rewrites the tree the way the real compiler does: holder references are inlined, each cache is deduplicated by its raw input, axes a function does not vary along get zero-coordinate slices, and constant folding and range shortcuts are applied. Several folds change rounding (subtracting a constant becomes adding its negation; dividing becomes multiplying by the reciprocal), so they are part of the contract, not optimisations.
- `sampler::Program` evaluates through `value` (scalar) and `volume` (bulk). The two paths round differently, notably in Perlin noise (bulk splits the XZ dot product from the Y gradient and caches per floor-Y) and in interpolation (bulk adds an incremental step down each cell column). `Ctx` holds the per-chunk cache cells; their lookup order (last scalar key, then retained volume, then compute) decides bits, so call order must match.
- `noise` holds Perlin, normal noise (octave table, Kahan-summed normalisation, second stack at frequency x 1.0181268882175227), the legacy nether-biome noise, blended old noise and end-island simplex.
- `aquifer::Aquifer` is the fluid-level model on a 16x12x16 jittered grid. Its construction samples the surface-level function once as a bulk volume; later scalar requests hit that cache. It runs in `f64` after reading `f32` samples.
- `settings::TerrainGenerator::load` compiles the eight router roots and the optional aquifer functions for a seed. `fill_chunk` samples final density as a 16 x height x 16 volume (the aquifer is built first, as in the reference order), then asks the aquifer for each cell in z, x, descending-y order. `ChunkFill` reports density, a `Substance` per cell (default block or a fluid) and the cells whose fluid needs a scheduled update.

## How to change it

- New density-function type: add it to `tree::Node` parsing, `compile::Compiler::compile` and `sampler::Program::value`/`volume`. Write the scalar and bulk arithmetic separately and add an oracle fixture that exercises both.
- Never "simplify" arithmetic. An algebraically equal rewrite usually differs in the last bit and fails the hash comparison.
- Beardifier (structure density) is a hook: set `Ctx::beardifier` to a `BeardifierSource`. Nothing sets it yet; chunks generated without structures match the oracle, which also runs with none. Blending is not modelled (new worlds only).
- Material rules and surface are not part of this module; see [surface control flow](worldgen-surface-cfg.md).

## Configuration

None at runtime. Settings come from the embedded [26.3 worldgen data bundle](worldgen-data-bundle-26-3.md). Tests: `cargo test -p lodestone-worldgen-core --release --test release26_3_density --test release26_3_chunks`; throughput: add `-- --ignored --nocapture`.

## Dependencies

`serde_json` for parsing and the crate's own `rng` module. Verification uses JVM oracles in `scripts/worldgen-oracle-26-3/` (`run.sh` runs a class in a temurin container against the cached server jar). `DensityOracle263` hashes every router, aquifer and registry function over a scalar grid and seven volume shapes, uncached. `ChunkOracle263` runs the real chunk fill and prints density, substance and fluid-update hashes. Fixtures live in `crates/lodestone-worldgen-core/tests/fixtures/release26_3/`; regenerate them with the oracle commands in each test's header after a version bump. Each suite has a wrong-seed control that must fail.
