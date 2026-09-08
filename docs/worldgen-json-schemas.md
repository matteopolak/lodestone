# World-generation JSON schemas

## What it is

World-generation density functions and surface rules are parsed through strict serde schemas before they are instantiated. Typed discriminated unions reject unknown fields, unknown type tags, and implicit numeric/string conversions at the data boundary.

## How it works

`lodestone-worldgen-core::density::Builder` accepts the existing resolver value seam, then immediately deserializes each node as a number, registry reference, or closed `type`-tagged object. Nested nodes use the same schema, and `serde_path_to_error` includes the structural location in construction errors. `lodestone-worldgen::surface::SurfaceSystem` applies the same treatment to the surface-rule and condition trees. A block state's property object remains a map because its keys come from the block registry and cannot be enumerated by this crate.

The random-source selector also deserializes its boolean flag into a named serde field; a quoted boolean or other implicit conversion is rejected.

## How to change it

Add a new density or surface type as a serde enum variant and a `deny_unknown_fields` payload, then add a conversion arm in the corresponding builder/parser. Add malformed, unknown-field, and unknown-discriminator controls before changing acceptance. Keep the numeric/reference alternatives untagged only where the registry format actually defines those alternatives.

## Configuration

The schemas apply to `density_function/*` documents and the `surface_rule`, `default_block`, and nested condition documents in `noise_settings/*`. No runtime flag changes strictness.

## Dependencies

The schemas use `serde`, `serde_json`, and `serde_path_to_error`. Runtime construction still uses the existing resolver, noise, random-source, block interner, and canonical block-state table interfaces.
