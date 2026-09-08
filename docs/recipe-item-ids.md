# Recipe item IDs

## What it is

Recipe displays, ghost previews, and recipe property sets carry item-registry
numbers. `lodestone_model::ItemId` keeps each number together with whether it
has been validated against this build's canonical item census or remains owned
by a protocol/session registry.

## How it works

The 26.2 adapter reads each synchronized recipe item number as an unsigned
domain value. A number present in the generated item census becomes
`ItemId::Canonical`; any other non-negative number becomes
`ItemId::ProtocolLocal` and remains available to a dynamic-registry consumer. Negative wire values are
rejected as malformed registry ids. Recipe synchronization compares the typed
values, so equal numbers from different registries cannot accidentally match.

Consumers that need a canonical item name, prototype, or sprite first require
`ItemId::canonical_raw()` and then perform the generated-table lookup. A
protocol-local value must be resolved by the registry that supplied it; it must
not be range-cast into the built-in table.

## How to change it

Extend `lodestone_model::ItemId` when another item-id source needs to be
represented. Keep source classification at the protocol or synchronized-data
boundary, and keep canonical table access at the consumer boundary. Recipe
field changes affect `ClientEvent` and `lodestone_game::recipe_sync`; update
their consumers together so source provenance is not erased by a conversion.

## Configuration

There is no runtime configuration. The canonical item census is generated for
the supported 26.2 data version; protocol-local values are intentionally not
bounded by that census.

## Dependencies

`ItemId` is defined by `lodestone-model` and has no protocol or data-table
dependency. The 26.2 adapter uses `lodestone-data` only to classify known ids;
the recipe synchronization store itself remains version-free.
