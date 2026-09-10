# Texture metadata JSON

## What it is

`lodestone-assets::TextureMeta` parses a texture's sibling `*.png.mcmeta`
document into animation scheduling and per-sprite sampling metadata. The
decoded result feeds atlas frame tables and mipmap generation without carrying
a JSON tree into those consumers.

## How it works

The `animation` and `texture` sections are named, closed DTOs. Animation uses
an untagged frame shape—either a numeric index or an object with `index` and an
optional positive `time`—plus positive dimensions and a positive default frame
duration. Texture uses typed booleans, a closed mipmap-strategy enum, and a
numeric alpha-cutoff bias. Unknown nested fields, zero durations/dimensions,
explicit nulls for known scalar fields, and unknown strategy names fail at the
asset boundary.

Top-level sections outside `animation` and `texture` are intentionally open:
their payloads belong to other asset subsystems, so this parser consumes them
as arbitrary JSON and records only their names in `TextureMeta::other_sections`.
Pack-authored metadata keeps the existing lenient trailing-content rule: the
first JSON value is deserialized and bytes after it are ignored, while a
malformed value still fails.

## How to change it

Add a field to the corresponding private `*DocumentFields` DTO, preserve its
default and validation rule in the lowering conversion, and add both a valid
fixture and a rejection test. Keep the object-shape guard around all-default
sections; otherwise Serde can accept positional JSON arrays as structs. Add a
round-trip test whenever a transport shape changes. Only add a top-level
section to the public model when a consumer needs its payload; presence-only
sections must remain arbitrary and must not reintroduce `serde_json::Value` to
the known schema structs.

## Configuration

There are no environment variables or runtime flags. `TextureMeta::parse` is
called by `AtlasBuilder` when a sibling metadata resource exists.

## Dependencies

- `serde` and `serde_json` deserialize the typed metadata DTOs.
- `crate::json::from_slice_lenient` preserves the pack metadata trailing-content
  tolerance.
- `MipStrategy` receives the typed strategy conversion.
- `AtlasBuilder` and the mipmap module consume the lowered public metadata.
