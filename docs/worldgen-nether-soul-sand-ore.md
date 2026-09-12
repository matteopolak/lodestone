# Nether soul-sand ore coverage

## What it is

The `ore_soul_sand` entry is the soul-sand valley's biome-specific connected
ore body. Its focused parity fixture proves that the configured target reaches
the production `NetherGenerator` column rather than existing only in the asset
parser or feature table.

## How it works

The step-7 Nether dispatcher preserves the placed entry's source index and
sends the ordinary ore configuration through the shared connected-ore engine.
The integration test runs one complete production column at seed `42` twice:
once with the bundled entry and once with only that entry withheld. The
feature-only difference is compared with the 65 local cells recorded in
`crates/lodestone-worldgen/tests/support/nether_soul_sand_ore_external.txt`.

The expected cells come from the authenticated frozen Nether materialization
whose chunk bounds are `-26..26` on both axes. The fixture records its freeze
digest, seed, step, feature id and target block so a changed capture cannot be
silently treated as the same evidence.

## How to change it

If the placed-feature list, ore configuration, feature seed or source-index
handling changes, regenerate the expected cells from a new authenticated
capture and update the fixture metadata together with the focused test. Keep
the withholding resolver limited to `ore_soul_sand`; withholding a whole
decoration step would no longer identify this consumer.

The target block also occurs in the soul-sand valley's terrain, so comparing
all final soul-sand cells is not sufficient. The test compares the complete
column with and without this exact placed entry and asserts the resulting
feature-only set.

## Configuration

The source data is under `crates/lodestone-server/assets/worldgen/`:

- `biome/soul_sand_valley.json` owns the step-7 entry.
- `placed_feature/ore_soul_sand.json` supplies count, square spreading,
  height `0..=31`, and biome filtering.
- `configured_feature/ore_soul_sand.json` supplies connected size `12`,
  netherrack matching and the soul-sand target state.

There are no runtime flags.

## Dependencies

The coverage relies on `lodestone_worldgen::nether::NetherGenerator`, the
shared ore parser and connected-ore engine, the bundled world-generation
documents, and the authenticated external fixture described above.
