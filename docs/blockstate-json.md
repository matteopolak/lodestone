# Blockstate JSON models

## What it is

`lodestone-assets::BlockStates` reads blockstate resource-pack files into the
typed `BlockStateDefinition` model used by model selection and baking. The
loader covers both property-keyed `variants` and conditional `multipart`
definitions. Runtime model baking also maps report identities into this build's
fixed canonical state census.

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

### Report identities at model ingress

`lodestone_render::BlocksJsonRegistry::from_slice` parses the generated
`blocks.json` report through typed state records. Block names must be fully
namespaced, IDs must fit `u32`, and actual state property values must be strings.
Duplicate object keys, numeric IDs, and name-plus-property identities fail.
Empty reports fail, as does a maximum ID whose increment cannot fit the
registry trait's `u32` count.

The raw registry preserves report IDs for report and oracle consumers. Its
sparse map stores only supplied states; its `state_count` remains the maximum
report ID plus one. Iterating a raw registry therefore still scales with that
numeric span. It is not a runtime content registry.

`BlocksJsonRegistry::into_canonical` consumes the parsed entries and moves their
owned identifiers and property maps into a vector of exactly 32,366 slots.
`lodestone_data::block_states::StateId::from_exact_parts` searches only the
named block's generated span and requires equality of the complete property
set. Missing, extra, or invalid properties remain unsupported; the abbreviated
state-string lookup is deliberately not involved. Unsupported identities leave
no slot and are returned as a count plus at most four examples. A missing
canonical identity leaves a hole rather than receiving substitute content.

The native and browser asset loader canonicalizes once after acquiring report
bytes and before passing the same registry to both the block atlas and model
baker. The official reports demonstrate why numeric report IDs cannot bypass
this step: water with `level=0` is 86 in 26.2 and 89 in 26.3; oak log with
`axis=y` is 137 and 140. Both reports contain all 32,366 current canonical
identities, while the 26.3 report contains 3,357 unsupported identities. Poplar
planks at report ID 27 cannot replace canonical bamboo planks at ID 27.

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

Keep raw report loading and canonical model ingestion as separate APIs. Extend
the generated canonical census only through its reviewed data-generation
workflow; accepting a newer report does not enable its new content or protocol.
Change `StateDocument` and its malformed-input controls together if the report
schema changes. The hermetic ingress tests include a shifted report that fails
the canonical witness detector before conversion. The ignored
`official_reports_fill_the_same_canonical_census` control checks both cached
reports across every current canonical slot without generating data.
The staged-pack consumer control also bakes a canonicalized newer report and
checks water classification and the vertical log's end-versus-side sprites.

## Configuration

There are no environment variables or runtime flags. The parser accepts the
resource-pack JSON bytes supplied by `ResourceManager`.
Report bytes come from the selected native asset root or installed browser
asset bundle; there is no flag that bypasses canonicalization for runtime
model baking.

## Dependencies

- `serde` and `serde_json` deserialize the bounded transport model.
- `lodestone-assets::ResourceLocation` validates model identifiers.
- `BlockStateDefinition` is consumed by the asset model resolver and renderer.
- `lodestone-data` supplies the generated canonical block spans and exact
  `StateId` membership used by report ingestion.
- `lodestone-model::BlockStateRegistry` is the shared atlas and model-baker
  interface for both raw oracle registries and canonical runtime registries.
