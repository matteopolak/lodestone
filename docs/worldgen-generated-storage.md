# Generated-column compact storage

## What it is

`lodestone-worldgen` returns an immutable `GeneratedColumn` whose block-state
palette is assigned in dense first-write order and whose block indices are
stored in 16-row sections. A section is either uniform or a packed `u16`
index stream, while biome, entity, heightmap, spawn, and stage products remain
sidecars on the column.

## How it works

The final dense grid is converted once at the output boundary. Its palette
order and flat `(y, z, x)` cell order are retained exactly; compaction only
changes the cell carrier. Uniform sections own no packed payload. Mixed
sections derive the smallest width needed by their largest column-palette index
and pack values into `u64` words without crossing word boundaries. A write
widens or promotes only its section and never narrows it.

The output boundary uses the summary-aware constructor so the section
inspection pass also retains the highest non-air and motion-blocking-or-fluid
cell for each `(x, z)` column. The stored values are first-free rows relative
to `min_y`, preserving zero for an empty column and the existing optional
motion sidecar when no predicate facts are available. This removes the two
post-packing strided full-column reads while leaving `from_flat` available for
callers that need only storage.

`GeneratedColumn::into_compact` moves the compact carrier and all sidecars to
the lifecycle consumer. `into_raw` remains a compatibility adapter and
explicitly expands the sections into the historical flat vector. Conversion
counters account for cells covered by dense-to-compact, section widening, and
flat compatibility expansion.

## How to change it

Keep the palette separate from section storage: section indices are local only
to the column-wide palette. Any layout change must compare every compact read
against an independently populated flat field, including a negative minimum Y,
uniform and mixed sections, width transitions, and a single-cell mutation.
Summary changes must likewise compare against independent scalar controls,
including empty columns, partial top sections, and a negative `min_y`; changing
the predicate must not alter the non-air summary, and disabling it must retain a
`None` motion sidecar.
Consumers that can adopt section storage should use `into_compact` and move
packed word buffers through `CompactBlockStorage::into_sections`; callers that
need the old contiguous carrier may continue using `into_raw`.

## Configuration

There are no environment variables or feature flags. Sections are 16 rows and
their indices are `u16`; widths are derived from the largest index present.

## Dependencies

The storage module uses only the standard library and the worldgen counter
boundary. `GeneratedColumn` additionally carries the existing biome, block
entity, heightmap, spawn, and stage sidecars; the server lifecycle owns the
next representation after consuming the compact handoff.
