# Root-system world generation

## What it is

The root-system configured feature grows an elevated nested feature through a cave ceiling, replaces eligible material in the column below it, and scatters hanging roots around the original candidate. It is the root-column path used by the lush-cave decoration data.

## How it works

`vegetation::root_system::place_root_system` first rejects an occupied origin. It then scans upward one block at a time, stopping at the world-surface height boundary and accepting the first candidate that satisfies the configured predicate, vertical clearance, level probes, and a solid non-lava support below. The parent dispatcher invokes the nested placed feature at that candidate on the supplied mutable random source. Only a nested call that produces an overlay write enables the two root passes.

Column-root attempts use four bounded random draws for each `(x, z)` offset and replace only the configured base ids. Hanging-root attempts use six bounded draws; after an air check they select the configured provider state before testing that state’s survival rule. This order is material because a weighted provider consumes a draw even when its selected cell cannot survive.

All writes use `VegGrid::set_id_if_in_bounds`. The region decoration driver supplies a footprint that includes neighbouring source chunks, so a valid root write across a chunk edge reaches the shared overlay; a write outside that footprint is intentionally rejected rather than clamped into a different column.

## How to change it

Keep the nested-placement callback on the caller-owned random stream. Do not replace it with a reseeded source or a direct configured-feature call: modifiers on the nested placed feature are part of its behavior. When adding a new hanging-provider state family, add its explicit survival rule in `hanging_state_can_survive`; unknown families must not silently place.

The module-local fixture tests compare the full expected block map from the compiled 26.2 runtime and retain an occupied-origin negative control. Update both the external oracle fixture and those assertions when a behavioral change is intentional.

## Configuration

The configured-feature data supplies the nested placed feature, vertical-space and level-test limits, root and hanging-root radii/attempt counts, replacement ids, two state providers, permitted water height, and the candidate predicate. The only implicit setting is the `VegGrid` footprint chosen by the vegetation-region driver.

## Dependencies

This feature relies on `VegGrid` for terrain reads and bounded overlay writes, `VegTags` plus `BlockPredicate` for configured state tests, `BlockStateProvider` for provider selection, and the parent vegetation dispatcher for nested placed-feature execution.
