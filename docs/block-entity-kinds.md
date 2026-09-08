# Block-entity runtime keys

## What it is

`lodestone-server::block_entities::BlockEntityKind` is the typed runtime key for a block-entity record. Built-in records use enum variants, while plugin-defined and newer keys use `Extension(String)` so unknown values remain lossless.

## How it works

`BlockEntityKind::from_name` maps known registry keys to variants and stores every other key in the extension variant. `name` and `into_name` provide the canonical key spelling when a record crosses a persistence or protocol boundary. `BlockEntity::kind` derives the typed key from a live record; the older `type_id` accessor remains available for those boundary conversions.

The numeric `lodestone-data` block-entity registry identifier is version-specific wire data and is intentionally separate from this extensible runtime key.

## How to change it

Add a built-in variant and both directions in `BlockEntityKind::from_name` and `BlockEntityKind::name` when a new built-in becomes part of the server's modeled set. Internal gameplay decisions should match `BlockEntity::kind`; use `type_id`, `name`, or `into_name` only at a format or protocol boundary. Never discard an `Extension` value while decoding an opaque or container record.

## Configuration

There is no configuration. Keys are canonical namespaced strings supplied by block-entity records.

## Dependencies

The key lives beside `BlockEntity` in `lodestone-server`. NBT and protocol adapters consume its string conversion, while `lodestone-data` supplies only version-specific numeric wire registry mappings.
