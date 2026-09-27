# Generated-column compact storage

## What it is

`lodestone-worldgen` returns an immutable `GeneratedColumn` with a block-state
palette in first-introduction order. Full columns store palette indices in
16-row sections; shaped Overworld columns may retain canonical state IDs until
a section-oriented consumer needs them. Biome, entity, heightmap, spawn, and
stage products remain sidecars.

## How it works

The production Overworld materializer rewrites its packed fill carrier into
canonical `u16` state IDs in place. It records palette introductions in the
existing observable `z, x, y` order, which differs from the carrier's flat
`(y, z, x)` layout. Shaped prefixes share the raw carrier across clones and
serve state reads without building a second palette-index field. If a shaped
consumer asks for sections, a direct state-to-palette lookup packs them on
demand. Full outputs pack immediately and do not retain the raw carrier. The
older indexed-grid path remains available to other callers. Uniform
sections own no packed payload. Mixed sections derive the smallest width needed
by their largest column-palette index and pack values into `u64` words without
crossing word boundaries. A write widens or promotes only its section and
never narrows it.

Full output packing also builds the three client heightmaps and a per-section
histogram of palette indices. Shaped outputs defer those summaries until a
heightmap, count, or server-column consumer asks for them. Intermediate shaped
products that remain typed therefore avoid a full-field summary scan; shaped
packet neighbours still build summaries when converted to server columns.
The stored heightmap values are first-free rows relative to `min_y`, preserving
zero for an empty column and the existing optional motion sidecar when no
generation predicate facts are available. The server consumes the histogram
to classify ticking cells from its palette metadata, so generated-column
adoption performs no second 98,304-cell observer scan. `from_flat` remains
available for callers that need only storage.

`GeneratedColumn::into_compact` moves the packed carrier and all sidecars to the
lifecycle consumer, packing a shaped raw carrier at that boundary when needed.
Direct state reads and the fallback content fingerprint use canonical IDs
without forcing that conversion. `into_raw` remains a compatibility adapter;
indexed shaped carriers can move their existing flat buffer directly.
Conversion counters distinguish retained raw products from section packing,
widening, and compatibility expansion.

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
`None` motion sidecar. A shaped block read must not force summaries, while a
subsequent heightmap read must produce the same values as eager construction.
The server handoff control should keep the legacy
observer at 98,304 reads and the production path at zero for a 384-row column.
Consumers that can adopt section storage should use `into_compact` and move
packed word buffers through `CompactBlockStorage::into_sections`; callers that
need the old contiguous carrier may continue using `into_raw`.

## Configuration

There are no environment variables or feature flags. Sections are 16 rows and
their indices are `u16`; widths are derived from the largest index present.

## Dependencies

The storage module uses the canonical state registry, standard library, and
worldgen counter boundary. `GeneratedColumn` additionally carries the biome, block
entity, heightmap, spawn, and stage sidecars; the server lifecycle owns the
next representation after consuming the compact handoff.
