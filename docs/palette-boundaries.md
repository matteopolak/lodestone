# Palette boundaries

## What it is

Chunk palettes distinguish release-specific wire identities from the canonical block-state census. Translation consumes the decoded container before a section reaches gameplay or rendering.

## How it works

The packet decoder selects the release's global storage width: 15 bits for 26.2 and 16 for 26.3. Modern fixed-size arrays use this selected width for direct storage, regardless of the direct-width header. Indirect widths are clamped to the kind's minimum. Older length-prefixed arrays retain their existing width rules.

`PalettedContainer::try_map_values` translates one value for uniform storage and each palette entry for indirect storage. It transfers the existing packed indices without copying them. Direct storage is repacked once at the destination width, translating each cell without constructing an intermediate dense array or hash map. A failed mapping returns no partially translated container.

Canonical 26.3 block sections require 16-bit direct storage. A container's geometry and indirect thresholds must agree across translation. Biome identity and framing remain separate from block-state translation.

## How to change it

Select wire kinds in the version adapter, not from the canonical census. Keep older protocol widths explicit when extending canonical storage. Extend the consuming mapper rather than cloning sections after decoding.

The focused controls check packed-index allocation identity, mapping counts, direct-width arithmetic, minimum indirect widths and an actual mapping rejection. Chunk fixtures additionally verify selected wire IDs before section construction.

## Configuration

`PaletteKind` carries direct width, indirect thresholds, entry count and long-array framing. The selected `GameDataVersion` supplies the modern wire width; there is no runtime environment setting.

## Dependencies

`lodestone-world` owns packed storage and framing. `lodestone-data` supplies selected identity maps. Version adapters decode and translate before publishing canonical sections.
