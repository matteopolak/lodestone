# Generated-column compact storage

## What it is

`lodestone_worldgen::generated_storage::CompactBlockStorage` stores a generated
column's palette indices in 16-row sections, each either one repeated value or a
packed `u16` index stream. The server builds its chunk block storage
(`lodestone_server::chunk_blocks`) on it, together with the heightmaps and
per-section palette histograms the same pass derives.

## How it works

Uniform sections own no packed payload. Mixed sections derive the smallest
width needed by their largest column-palette index and pack values into `u64`
words without crossing word boundaries. A write widens or promotes only its
section and never narrows it.

Packing with summaries also builds the three client heightmaps and a
per-section histogram of palette indices (`GeneratedColumnSummaries`).
The stored heightmap values are first-free rows relative to `min_y`, preserving
zero for an empty column and the existing optional motion sidecar when no
generation predicate facts are available. The server consumes the histogram
to classify ticking cells from its palette metadata, so generated-column
adoption performs no second 98,304-cell observer scan. `from_flat` remains
available for callers that need only storage.

`CompactBlockStorage::from_section_fn_with_predicates` lets a consumer fill one
reused 4,096-index scratch section before section analysis and packing. The
consumer owns palette remapping, padding, and ordered replacements; storage
observes final cells to build the transient histogram and requested motion maps.
The callback receives only real rows, including a partial top section. It either
fills every cell and returns `None`, or returns `Some(index)` as proof the section
is uniform. Uniform sections update histograms arithmetically and update applicable
maps with the section's last real row; even index zero follows the supplied motion
predicates independently of surface-air rules. The scratch contents are ignored
for a uniform return. The server's column adoption (`lodestone_server::chunk`)
uses this boundary without retaining a second full-window field.
The explicit `with_summaries` request controls metadata allocation and returns
an optional summary. Disabled requests require absent heightmap predicates;
they pack the same final section indices without histogram allocation or any
summary observations.

## How to change it

Keep the palette separate from section storage: section indices are local only
to the column-wide palette, while raw IDs identify states globally. Preserve
introductions even when their final cells are overwritten. Any layout change
must compare every compact read
against an independently populated flat field, including a negative minimum Y,
uniform and mixed sections, width transitions, and a single-cell mutation.
Summary changes must likewise compare against independent scalar controls,
including empty columns, partial top sections, and a negative `min_y`; changing
the predicate must not alter the non-air summary, and disabling it must retain a
`None` motion sidecar. The server handoff control should keep the legacy
observer at 98,304 reads and the production path at zero for a 384-row column.

For section callbacks, intern every supplied palette entry before remapping and
intern replacement states in their original order, even if their final cells are
overwritten. Apply all replacements before summary observation. Raw dimension
adapters preserve missing/invalid source-index fallback to zero and clip spills
against the receiving window.

## Configuration

There are no storage environment variables or feature flags. Sections are 16 rows and
their indices are `u16`; widths are derived from the largest index present.

## Dependencies

The storage module uses the canonical state registry, standard library, and
worldgen counter boundary. Its consumers are the server's chunk block storage and
column adoption.
