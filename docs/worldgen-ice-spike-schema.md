# Packed-ice spike JSON schema

## What it is

The packed-ice spike configured feature is the frozen-ocean decoration that grows a tapered packed-ice column from a snow-block support. Its small configuration is decoded as a closed Serde model at the vegetation feature boundary.

## How it works

`feature::vegetation::ice_spike` accepts three fields: a matching-blocks support predicate, a matching-block-tag replacement predicate, and a canonical block-state object. Resource keys, state properties, and the built-in state id are validated before the resolver expands the replacement tag. Unknown fields or discriminators demote the configured feature instead of silently changing its placement rules.

The typed state model sorts properties through `BTreeMap` and resolves the resulting canonical spelling through the generated block-state table. Placement then hands that validated state to the generator-local interner, while the existing external taper fixture checks the draw order and changed cells.

## How to change it

Extend `IceSpikeConfigDocument` only when the asset schema gains a real field. Add a tagged enum variant for a new predicate type and a focused positive, malformed, round-trip, or external control with it. Keep registry references as `ResourceKey`; do not reintroduce a JSON map lookup or make an unknown predicate permissive, because either would fabricate support or replacement cells.

The resolver remains a JSON-valued cross-registry seam. That is intentional: this feature's local document is closed, while tag expansion still consumes the resolver's shared registry payload. The integration test exercises both boundaries.

## Configuration

The source asset is `worldgen/configured_feature/ice_spike.json`. The relevant shape is:

```json
{
  "can_place_on": {"type": "minecraft:matching_blocks", "blocks": "minecraft:snow_block"},
  "can_replace": {"type": "minecraft:matching_block_tag", "tag": "minecraft:ice_spike_replaceable"},
  "state": {"Name": "minecraft:packed_ice"}
}
```

## Dependencies

The parser uses Serde, the generated `lodestone-data` block-state table, and `Resolver`/`compose::resolve_block_tag` for the replacement-tag closure. Placement depends on `VegGrid`, `VegTags`, and the shared random-source contract. `tests/ice_spike_config_schema.rs` verifies the bundled asset and the malformed-schema control; the source module verifies the external geometry fixture.
