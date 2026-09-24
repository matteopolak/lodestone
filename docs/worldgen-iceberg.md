# Worldgen iceberg features

## What it is

The iceberg configured-feature path grows the packed-ice and blue-ice masses used by frozen-ocean decoration. It is consumed by the unified Overworld FEATURES dispatcher through the local-modification step.

## How it works

The parser preserves the configured block state and the placer resets the feature origin to sea level. It consumes the feature's random stream while choosing an ellipse or round profile, grows the above-water and underwater portions, removes unsupported edge blocks, and optionally carves an air cavity above water or a water cavity below it. Writes are bounded by `VegGrid`, so a source feature can spill only into the driver's existing footprint.

The body is checked against `tests/support/iceberg_feature_jvm.txt`, captured by `scripts/worldgen-oracle/IcebergOracle.java` from the compiled 26.2 server with a water/air level control. The fixture includes the full changed-cell map for seed zero and packed ice.

## How to change it

Keep the random draw order in `iceberg::place_iceberg` stable: later configured features share the same feature seed stream. If the shape or replacement rules change, regenerate the external fixture and update the exact-map test together. Add any new configured state family to the parser validation rather than defaulting it.

## Configuration

`configured_feature/iceberg_blue.json` supplies `minecraft:blue_ice`; `iceberg_packed.json` supplies `minecraft:packed_ice`. Sea level is the current Overworld constant used by the vegetation feature adapter.

## Dependencies

The parser uses the canonical block-state table. Placement depends on `VegGrid` for clamped reads and bounded overlay writes, the shared `RandomSource` contract, and the Overworld decoration catalog/FEATURES dispatcher.
