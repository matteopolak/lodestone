# Nether glowstone decoration

## What it is

This fixture covers the Nether's step-7 glowstone decoration in the production generator. It ties the `glowstone_extra` placement entry to 152 independently captured block cells in the authenticated seed-42 51×51 region.

## How it works

`NetherGenerator` consumes the biome's raw step-7 entries. The integration test resolves the bundled worldgen documents from the server asset tree, then compares a normal column with a control that replaces only the first `minecraft:glowstone_extra` entry. The expected local `(x, y, z)` cells live in `tests/support/nether_glowstone_external.txt`, so the assertion is against captured block data rather than a second implementation of the placement algorithm.

The fixture uses chunk `(9, -21)` and records the full column. The capture is bounded by `(-26..=26, -26..=26)` and is identified by freeze digest `8e3156fc42dff5e344520f75ba08b5e29a2e08eacdf839bb33ab2cce2ef9a257`.

## How to change it

Keep the control selective: the Nether feature list contains two glowstone placement entries, and changing both would hide whether the raw index under test reaches the consumer. If the asset data or generator changes, regenerate the fixture from an independently authenticated region and update the metadata and cell list together. Do not derive the expected cells by running the production generator.

## Configuration

The selected entry is `features[7]` in the Nether biome document. `minecraft:glowstone_extra` uses a biased-bottom count from 0 through 9 and a uniform height range four blocks from each vertical boundary; its configured feature writes `minecraft:glowstone` blocks. The test accepts `LODESTONE_WORLDGEN_ASSETS` to point at an alternate worldgen asset root; without it, it uses the repository's server assets.

## Dependencies

The test depends on `lodestone-worldgen::nether::NetherGenerator`, the density `Resolver` interface, the bundled server worldgen JSON, and the authenticated region capture represented by the external fixture. It does not depend on server protocol or lighting code.
