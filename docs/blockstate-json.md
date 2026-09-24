# Blockstate JSON models

## What it is

`lodestone-assets::BlockStates` reads blockstate resource-pack files into the
typed `BlockStateDefinition` model used by model selection and baking. The
loader covers both property-keyed `variants` and conditional `multipart`
definitions.

## How it works

The JSON boundary first deserializes a closed document shape: model references,
their rotation/weight options, and multipart cases are named Serde structs.
Single-model and weighted-list forms use a tagged-by-shape enum. A `when`
condition is a genuinely recursive schema, so it is represented by a typed
recursive struct with `OR` and `AND` branches plus an open map of property
names whose values are restricted to strings, booleans, or JSON numbers.

After deserialization, resource locations are validated and the transport
model is lowered into `BlockStateDefinition`. Variant selection and multipart
matching operate only on that public model; no JSON tree reaches the renderer.

## How to change it

Extend the private `*Document` structs when the on-disk blockstate schema gains
a known field, then add a fixture and a rejection test for malformed input.
Keep `WhenDocument::properties` open because property names are supplied by
block definitions, but keep each property value typed. Unknown fields on the
closed document, model-reference, and multipart-case shapes are rejected so a
typo cannot silently alter model selection.

The parser gives `variants` precedence if both top-level forms are present.
`ResourceLocation::parse` remains the validation boundary for model names;
callers should not bypass it by exposing the transport strings.

## Configuration

There are no environment variables or runtime flags. The parser accepts the
resource-pack JSON bytes supplied by `ResourceManager`.

## Dependencies

- `serde` and `serde_json` deserialize the bounded transport model.
- `lodestone-assets::ResourceLocation` validates model identifiers.
- `BlockStateDefinition` is consumed by the asset model resolver and renderer.
