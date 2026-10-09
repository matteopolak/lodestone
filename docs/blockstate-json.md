# Blockstate JSON models

## What it is

`lodestone-assets::BlockStates` reads blockstate resource-pack files into the typed `BlockStateDefinition` used by model selection and baking, covering property-keyed `variants` and conditional `multipart`. Runtime baking also maps report identities into the build's fixed canonical state census.

## How it works

- The JSON boundary deserialises a closed document shape: model references with rotation and weight options, and multipart cases, as named Serde structs. Single-model and weighted-list forms are a shape-tagged enum. A `when` condition is a typed recursive struct with `OR` and `AND` branches plus an open map of property names whose values are strings, booleans or JSON numbers. Resource locations are then validated and the model lowered into `BlockStateDefinition`; selection and multipart matching use only that public model, so no JSON tree reaches the renderer. If both top-level forms exist, `variants` wins.
- **Report identities.** `lodestone_render::BlocksJsonRegistry::from_slice` parses the generated `blocks.json` through typed records: namespaced block names, IDs fitting `u32`, string property values. Duplicate keys, numeric IDs or name-plus-property identities fail, as do empty reports and a max ID whose increment overflows `u32`. The raw registry keeps report IDs for report and oracle consumers in a sparse map; `state_count` is max ID plus one, so iterating it scales with the numeric span. It is not a runtime content registry.
- `BlocksJsonRegistry::into_canonical` moves the parsed entries into a vector of exactly 32,366 slots. `lodestone_data::block_states::StateId::from_exact_parts` searches only the named block's span and demands the full property set (missing, extra or invalid properties are unsupported; the abbreviated state-string lookup is not used). Unsupported identities leave no slot and return a count plus at most four examples; a missing canonical identity leaves a hole, never substitute content.
- The native and browser asset loaders canonicalise once after acquiring report bytes, before handing one registry to both the block atlas and model baker. Numeric report IDs cannot bypass this: water `level=0` is 86 in 26.2 and 89 in 26.3, oak log `axis=y` 137 and 140. Both reports contain all 32,366 current canonical identities, 26.3 adds 3,357 unsupported ones, and Poplar planks at report ID 27 must not replace canonical bamboo planks at 27.

## How to change it

- Extend the private `*Document` structs when the on-disk schema gains a field, with a fixture and a malformed-input rejection test. Keep `WhenDocument::properties` open (names come from block definitions) but values typed. Unknown fields on the closed document, model-reference and multipart shapes are rejected so a typo cannot alter selection. `ResourceLocation::parse` stays the validation boundary for model names.
- Keep raw report loading and canonical ingestion as separate APIs. Extend the canonical census only through its reviewed data-generation workflow; accepting a newer report does not enable its content or protocol. Change `StateDocument` and its controls together. Hermetic tests include a shifted report that fails the canonical witness detector; the ignored `official_reports_fill_the_same_canonical_census` checks both cached reports across every canonical slot; the staged-pack control bakes a canonicalised newer report and checks water classification and a vertical log's end versus side sprites.

## Configuration

None. The parser takes pack JSON bytes from `ResourceManager`; report bytes come from the native asset root or browser bundle, with no bypass of canonicalisation.

## Dependencies

`serde`, `serde_json`, `lodestone-assets::ResourceLocation`, `BlockStateDefinition` (consumed by the model resolver and renderer), `lodestone-data` (canonical spans and exact `StateId` membership) and `lodestone-model::BlockStateRegistry` (the shared atlas and baker interface for raw and canonical registries).
