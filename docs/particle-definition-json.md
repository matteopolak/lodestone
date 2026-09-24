# Particle definition JSON

## What it is

`lodestone-assets::ParticleDefinition` parses `assets/<namespace>/particles/*.json`
into the ordered resource locations that name a particle's sprite frames. It is
the resource-pack boundary used by particle-atlas discovery.

## How it works

The document has one closed, typed field: `textures`, an array of strings. A
missing field or explicit `null` lowers to an empty list because code-defined
particle types may have no authored sprite list. Every present string crosses
`ResourceLocation::parse`, so an invalid namespace or path is reported before
atlas lookup. Unknown document fields are rejected rather than silently
ignored, which keeps a misspelled field from producing an apparently valid but
empty particle sheet.

The parser uses strict end-of-document JSON parsing. Particle definitions are
single-winner resources in a pack stack, so a higher-priority file replaces the
whole definition; atlas source lists have a separate merged-stack path.

## How to change it

Extend `ParticleDocument` only when the on-disk schema gains a field that this
client can consume. Add a fixture, an unknown-field negative case, and a DTO
round-trip test. Keep the public `ParticleDefinition` focused on validated
resource locations; do not expose transport strings to the atlas or renderer.

## Configuration

There are no environment variables or runtime flags. `ParticleAtlas` discovers
definition files through `ResourceManager` and resolves their locations under
`textures/particle/`.

## Dependencies

- `serde` and `serde_json` deserialize the closed definition DTO.
- `ResourceLocation` validates authored texture identifiers.
- `ResourceManager` discovers and reads definition files.
- `ParticleAtlas` and `AtlasBuilder` consume the resulting frame locations.
