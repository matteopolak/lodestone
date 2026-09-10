# Atlas source-list JSON

## What it is

`lodestone-assets::AtlasDefinition` reads `atlases/<id>.json` source lists into
typed `AtlasSource` values before the atlas resolver turns them into sprite
paths. The boundary covers directory, single, and paletted-permutation sources
while preserving source kinds this client does not yet resolve.

## How it works

The parser first deserializes a closed top-level document and then lowers its
private transport DTOs into the public resource-location model. Each known
source has a closed shape: directory sources require `source` and `prefix`,
single sources require `resource` and may override `sprite`, and
paletted-permutation sources carry a typed list, palette key, separator, and a
documented dynamic map of permutation names to resource locations.

An unrecognised `type` is the one intentional open branch. Its type token is
retained as `AtlasSource::Unknown`, and resolution skips it so a newer source
kind does not discard the rest of an atlas. Known source fields and top-level
fields reject typos. `load_stacked` still merges descriptor layers in priority
order; that composition happens after each layer passes this boundary.

## How to change it

Add a new source kind to the transport enum, its custom tagged-map decoder,
the lowering function, and the resolver only when the source has a concrete
pixel or sprite-path consumer. Add a fixture covering its exact wire shape, a
negative test for an unexpected field on a closed kind, and a round-trip test
for the DTO. Keep permutation keys dynamic because they are authored names;
keep their values typed as resource-location strings.

Do not make an unknown source fail parsing merely because its payload is not
understood. If a known kind gains a field, update its closed DTO and its
serializer together so tests cover both decoding and re-encoding.

## Configuration

There are no environment variables or runtime flags. The parser receives
resource-pack bytes from `ResourceManager`; atlas layer merging is selected by
the caller through `AtlasDefinition::load_stacked`.

## Dependencies

- `serde` and `serde_json` deserialize the bounded transport DTOs.
- `lodestone-assets::ResourceLocation` validates every referenced texture.
- `ResourceManager` supplies pack-stack listings and bytes to resolution.
- `AtlasBuilder` consumes the resolved sprite entries for stitching.
