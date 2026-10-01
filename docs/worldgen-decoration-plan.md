# Worldgen decoration plan

## What it is

The decoration plan is the generator-scoped, numeric execution index for configured placed features. It removes per-column string sets and repeated per-step index reconstruction while preserving the source data's global `(step, index)` seed identity.

## How it works

Catalog construction still resolves the ordered feature graph once. Each resulting entry stores its decoration step, its precomputed raw index within that step, and its shared parsed feature object. Each biome stores a compact bitset over those entries. A source selection ORs the bitsets for its biome union and walks set bits in ascending catalog order, so unsupported entries still occupy their original indices without scanning strings or rebuilding a step counter.

The existing feature-to-biome map remains available for candidate-position biome gates. The numeric plan is consumed by the Overworld replay and decoration preparation paths through the existing catalog selectors, so no alternate generation implementation is introduced.

Those entries share `VegGrid`'s mutable decoration surface. Reads outside its vertical window return air; in-range ore reads probe the overlay once, then fall back to the immutable source owning the column. Overlay hits increment the ore overlay-read diagnostic, while only source reads enter the source-owner census. Source routing retains Euclidean chunk division for negative coordinates.

Height-lane scans accumulate visited, primary, and companion-tail cell counts locally and publish them to the vegetation census once per completed scan. Source-read accounting still records each source cell actually consumed. Live lanes observe prior overlay writes; world-generation lanes retain their immutable baseline.

## How to change it

Update `compose.rs` when changing catalog ordering, membership compilation, or selector output. Keep the entry index calculation tied to the final ordered graph, and add a selector test whenever a new feature category is added. Do not sort selected entries by feature name: ascending catalog position is the observable execution order.

Change `feature::vegetation::VegGrid` for shared read precedence or height accounting. Preserve its vertical guard, overlay-first reads, source census, and ore entry's `(x, z, y)` dirty transfer order. A repeated overlay-identical ore write adds no dirty cell, but the first write matching an immutable source still creates an overlay entry.

## Configuration

The plan has no runtime flags. It is rebuilt when a generator is constructed from the supplied resolver and biome order.

The `gen-counters` Cargo feature enables source-owner and ore-read diagnostics. The vegetation census, including height-scan totals, remains available without that feature.

## Dependencies

The plan uses `Resolver`, `PlacedRef`, and `PlacedOre` from the worldgen feature parser. The Overworld decoration dispatcher consumes its selector results, while vegetation candidate gates continue to use the existing biome membership view.
