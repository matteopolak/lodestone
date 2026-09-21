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
changes the cell carrier. Shaped prefixes retain the dense `u16` carrier and
share it across clones; section words are created only if a section-oriented
consumer asks for them. Full outputs still compact immediately. Uniform
sections own no packed payload. Mixed sections derive the smallest width needed
by their largest column-palette index and pack values into `u64` words without
crossing word boundaries. A write widens or promotes only its section and
never narrows it.

The output boundary retains all three client heightmaps and a per-section
histogram of palette indices while it traverses the dense field. Full outputs
fold that observation into section packing; shaped outputs run the small
summary pass without allocating section words. The stored values are first-
free rows relative to `min_y`, preserving zero for an empty column and the
existing optional motion sidecar when no generation predicate facts are
available. The server consumes the histogram to classify ticking cells from
its palette metadata, so generated-column adoption performs no second
98,304-cell observer scan. `from_flat` remains available for callers that need
only storage.

`GeneratedColumn::into_compact` moves the carrier and all sidecars to the
lifecycle consumer, compacting a shaped dense carrier at that boundary when
needed. Read-only `get`, `for_each_section`, and non-zero-count operations stay
on the dense carrier. `into_raw` remains a compatibility adapter and expands
only an already compact carrier; a lazy dense carrier can move its existing
flat buffer directly. Conversion counters therefore distinguish retained dense
products from actual section packing, widening, and compatibility expansion.

## How to change it

Keep the palette separate from section storage: section indices are local only
to the column-wide palette. Any layout change must compare every compact read
against an independently populated flat field, including a negative minimum Y,
uniform and mixed sections, width transitions, and a single-cell mutation.
Summary changes must likewise compare against independent scalar controls,
including empty columns, partial top sections, and a negative `min_y`; changing
the predicate must not alter the non-air summary, and disabling it must retain a
`None` motion sidecar. The server handoff control should keep the legacy
observer at 98,304 reads and the production path at zero for a 384-row column.
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
