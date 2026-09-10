# Large dripstone configuration parsing

## What it is

The large-dripstone feature parser turns its JSON float providers into the typed runtime values used by generation. Its provider discriminator is a closed Serde enum, so an unsupported provider cannot silently select a different implementation.

## How it works

The parser accepts either a scalar float or an object with `type` set to `minecraft:constant` or `minecraft:uniform` (the unqualified spellings remain accepted for compatibility). Serde rejects unknown discriminator values and unexpected object fields before the feature-specific range checks run. The runtime then stores only `Constant` or `Uniform` values and samples them without inspecting JSON or allocating strings.

## How to change it

Add a new provider variant to `FloatProviderKind` and update the conversion in `FloatRange::try_parse` together with a fixture that proves its draw and bounds behavior. Keep the `deny_unknown_fields` guard: adding permissive fields can make malformed world-generation data look valid and alter parity. `#[serde(untagged)]` is limited to the two genuinely different JSON shapes (number versus provider object).

## Configuration

`height_scale`, `stalactite_bluntness`, `stalagmite_bluntness`, and `wind_speed` use the scalar-or-provider shape. Provider ranges are validated by the feature parser after deserialization; malformed or out-of-range values cause the configured feature to be skipped.

## Dependencies

The parser uses `serde`/`serde_json`, while `LargeDripstoneCfg` feeds the vegetation feature dispatcher and `VegGrid`. No runtime world-generation path retains the JSON discriminator.
