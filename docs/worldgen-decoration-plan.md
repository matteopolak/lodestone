# Worldgen decoration plan

## What it is

The decoration plan is the generator-scoped, numeric execution index for configured placed features. It removes per-column string sets and repeated per-step index reconstruction while preserving the source data's global `(step, index)` seed identity.

## How it works

Catalog construction still resolves the ordered feature graph once. Each resulting entry stores its decoration step, its precomputed raw index within that step, and its shared parsed feature object. Each biome stores a compact bitset over those entries. A source selection ORs the bitsets for its biome union and walks set bits in ascending catalog order, so unsupported entries still occupy their original indices without scanning strings or rebuilding a step counter.

The existing feature-to-biome map remains available for candidate-position biome gates. The numeric plan is consumed by the Overworld replay and decoration preparation paths through the existing catalog selectors, so no alternate generation implementation is introduced.

## How to change it

Update `compose.rs` when changing catalog ordering, membership compilation, or selector output. Keep the entry index calculation tied to the final ordered graph, and add a selector test whenever a new feature category is added. Do not sort selected entries by feature name: ascending catalog position is the observable execution order.

## Configuration

The plan has no runtime flags. It is rebuilt when a generator is constructed from the supplied resolver and biome order.

## Dependencies

The plan uses `Resolver`, `PlacedRef`, and `PlacedOre` from the worldgen feature parser. The Overworld decoration dispatcher consumes its selector results, while vegetation candidate gates continue to use the existing biome membership view.
