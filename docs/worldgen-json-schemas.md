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

## Feature schema boundaries

The worldgen feature interpreters use strict serde schemas at the configured-feature, placed-feature, ore, and top-layer fact boundaries. This catches malformed records and unknown fields before feature code walks them while leaving versioned block-property and tag payloads open.

Ore configurations deserialize into closed records for sizes, targets, rule-test discriminators, and block states. Placement modifiers use a closed field envelope with explicit conversion for count, rarity, square, height, and biome modifiers; height anchors and integer providers reject unknown shapes and report their serde path. Top-layer fact documents similarly validate their fixed predicate columns while keeping state ids as dynamic map keys.

Vegetation records validate the placed-feature envelope and modifier field names, plus the fixed top-level fields for trees, geodes, empty dungeon configurations, and multiface growth. Their provider, state, and holder-set payloads remain dynamic where registry data defines the keys. Unsupported vegetation kinds continue to degrade to an explicit unsupported feature rather than aborting generation.

Add a field to the schema next to the parser that consumes it, and decide whether omission is part of the data format before adding `#[serde(default)]`. Keep `#[serde(deny_unknown_fields)]` on closed records. For a new discriminator, add a conversion arm and an external malformed/unknown control in `crates/lodestone-worldgen/tests/typed_feature_json.rs`; do not turn a provider or block-property map into a fixed list unless the registry format guarantees that list.

The feature schemas are compiled into `lodestone-worldgen`; no runtime flag changes validation. A malformed production vegetation record is represented as `ConfiguredFeature::Unsupported` with a schema diagnostic, while malformed ore and placed-ore records retain their existing embedded-data panic contract with a path-bearing message. The boundary uses `serde` derive and `serde_json`; feature behavior still depends on the worldgen-core RNG/math, the version-free resolver, and registry-provided block states and tags.
