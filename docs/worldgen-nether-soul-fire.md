# Nether soul-fire decoration

## What it is

This fixture covers the Nether's step-7 soul-fire decoration in the production generator. It ties the `patch_soul_fire` placement entry to eight independently captured `minecraft:soul_fire` cells in the authenticated seed-42 51×51 region.

## How it works

`NetherGenerator` consumes the biome's raw step-7 entries. The integration test resolves the bundled worldgen documents from the server asset tree, then compares a normal column with a control that replaces only the first `minecraft:patch_soul_fire` entry. The expected local `(x, y, z)` cells live in `tests/support/nether_soul_fire_external.txt`, so the assertion is against captured block data rather than a second implementation of the placement algorithm.

The fixture uses chunk `(20, 25)`, whose 16 biome quart cells are `minecraft:soul_sand_valley`, and records the full column. The capture is bounded by `(-26..=26, -26..=26)` and is identified by freeze digest `8e3156fc42dff5e344520f75ba08b5e29a2e08eacdf839bb33ab2cce2ef9a257`.

## How to change it

Keep the control selective: it replaces one raw feature-list entry and leaves the rest of the decoration schedule intact. If the asset data or generator changes, regenerate the fixture from an independently authenticated region and update its metadata and cell list together. Do not derive the expected cells by running the production generator.

## Configuration

The selected entry is `features[7]` in the soul-sand valley biome document. `minecraft:patch_soul_fire` uses a uniform count from 0 through 5, a four-block boundary margin, 96 random offsets, and a soul-soil support predicate. Its configured feature writes the `minecraft:soul_fire` state. The test accepts `LODESTONE_WORLDGEN_ASSETS` to point at an alternate worldgen asset root; without it, it uses the repository's server assets.

## Dependencies

The test depends on `lodestone-worldgen::nether::NetherGenerator`, the density `Resolver` interface, the bundled server worldgen JSON, and the authenticated region capture represented by the external fixture. It does not depend on server protocol or lighting code.
