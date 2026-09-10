# Nether gold ore

## What it is

Nether gold ore is the standard underground ore entry in the Nether's mixed step-7 decoration pass. Focused parity coverage keeps an external light-free stream witness attached to the production source-spill path.

## How it works

The Nether feature catalog preserves the external raw feature index, then the mixed dispatcher routes the entry through the standard ore placement adapter. Source completion records writes that cross into the target chunk, and lifecycle replay folds those spills in completion order. The fixture records one external gold-ore cell at absolute position `(-10, 18, -160)` and attributes it to step 7, catalog index 19.

## How to change it

Change the configured or placed feature data under `crates/lodestone-server/assets/worldgen/` only when the external witness changes too. Keep the negative resolver control focused on `minecraft:ore_gold_nether`; replacing the entry with an unknown identifier must remove the witness from the production spill stream. Changes to source completion order belong in the Nether lifecycle materializer, not in this feature-specific coverage.

## Configuration

The generator uses the Nether noise settings and biome feature lists supplied through the `Resolver` implementation. The fixture fixes seed 42, target and source chunk `(-1, -10)`, and the external stream digest.

## Dependencies

This path depends on the Nether generator, the typed stage schedule, the composed decoration catalog, standard ore placement, and the external JVM stream oracle used to produce the fixture.
