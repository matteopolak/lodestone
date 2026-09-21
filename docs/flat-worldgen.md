# Flat world generation

## What it is

The flat generator expands a preset's layer stack into one immutable row per
height position. Layer blocks are resolved to canonical `StateId` values while
the preset is parsed, so generated columns never carry or compare block-state
text.

## How it works

`FlatLevelGeneratorSettings::try_from_json` parses the preset and rejects an
unknown built-in layer block. `FlatLevelSource` expands each layer once and
shares the resulting rows across all coordinates. `FlatColumn::block_state_id`
returns the row state or canonical air outside the configured stack.

The only text retained by the settings object is configuration data such as the
biome and structure-set identifiers. Block-state text is an input-boundary
value only; the generated column and its row palette are numeric.

## How to change it

Keep `default_state_id` backed by the generated block registry so property
defaults remain data-driven. Keep layer order and first-use row order
unchanged. Callers should use `block_state_id` and `rows`; converting a state
back to text belongs at an explicit storage or protocol boundary.

## Configuration

Presets use the JSON `layers` array with `block` and `height` fields. Missing
optional biome, feature, lake, and structure settings retain their existing
defaults. `try_from_json` is the fallible entry point for callers that need to
report invalid presets; `from_json` is for trusted embedded data and rejects an
unknown block by failing immediately.

## Dependencies

The generator uses `lodestone-data`'s built-in block-state table and the shared
`DenseBlockGrid` adapter for plugin-facing chunk output. It does not use noise,
random state, or feature placement for its terrain field.
