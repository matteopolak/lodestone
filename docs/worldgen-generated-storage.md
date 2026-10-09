# Generated-column compact storage

## What it is

`lodestone_worldgen::generated_storage::CompactBlockStorage` stores a generated column's palette indices in 16-row sections, each either one repeated value or a packed `u16` index stream. The server's chunk block storage (`lodestone_server::chunk_blocks`) is built on it.

## How it works

Uniform sections own no packed payload. Mixed sections use the smallest width that fits their largest column-palette index, packed into `u64` words without crossing word boundaries. A write widens or promotes only its own section and never narrows.

Packing can also produce `GeneratedColumnSummaries` in the same pass: the three client heightmaps (first-free rows relative to `min_y`, zero for an empty column) and a per-section histogram of palette indices. The server uses the histogram to classify ticking cells, so adoption needs no second 98,304-cell scan.

`from_section_fn_with_predicates` lets a consumer fill one reused 4,096-index scratch section. The callback sees only real rows (including a partial top section) and either fills every cell and returns `None`, or returns `Some(index)` to declare the section uniform. Uniform sections update histograms arithmetically and maps with the last real row. The consumer owns palette remapping, padding and ordered replacements. `lodestone_server::chunk` column adoption uses this path.

`with_summaries` controls metadata allocation; when disabled, heightmap predicates must be absent and no histogram is built.

## How to change it

- Keep the palette separate from section storage: indices are local to the column palette, raw IDs are global.
- In callbacks, intern every supplied palette entry before remapping, and replacement states in original order, even if overwritten. Apply all replacements before summary observation.
- Raw dimension adapters send missing or invalid source indices to zero and clip spills to the receiving window.
- Any layout change must compare every compact read against an independently populated flat field (negative minimum Y, uniform and mixed sections, width transitions, single-cell mutation). Summary changes need scalar controls (empty column, partial top section, negative `min_y`).

## Configuration

None. Sections are 16 rows with `u16` indices.

## Dependencies

The canonical state registry, the standard library and the worldgen counter boundary. Consumers: server chunk block storage and column adoption.
