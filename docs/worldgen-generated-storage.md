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

`DenseBlockGrid` has explicit indexed, raw, and borrowed-region storage
variants. End lifecycle source replay uses a borrowed 48-by-256-by-48 region
with nine immutable `Arc` column slots in x-major, z-fast order. A point read
checks transient writes, then resolves the matching source slot with that
source's own vertical bounds; uncovered rows are air. Overrides are installed
before the existing net-change journal begins. Returning to the source state
removes the transient entry, while ordered structure provenance still records
every accepted write. The region lives for one source replay and adds no cache.

Point reads, structure processors, template placement, and bulk copies keep
using the same grid interface. Whole-lane consumers explicitly materialize the
borrowed region once in original chunk and y-z-x copy order, then append
transient state introductions in encounter order. This preserves dense palette
history even when a state was overwritten. Box conversions read only their
requested box. Ordinary dense constructors and shared-carrier copy-on-write
remain indexed or raw; no point operation silently materializes a region.

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

Keep End source-window construction in
`EndGenerator::parity_decoration_grid_for_target` and borrowed addressing in
`DenseBlockGrid`. Changes must preserve source-height clipping, palette history,
baseline overrides, sorted net spills, and structure mutation ordinals. The
bounded grid control covers differing source heights and indexed/raw sources;
the End replay control independently constructs the former dense window for
platform and city witnesses. `end_borrowed_region_counters` is a separate
process so its exact counter assertions cannot absorb other tests' traffic.

## Configuration

There are no storage environment variables or feature flags. Sections are 16 rows and
their indices are `u16`; widths are derived from the largest index present.
The diagnostic `gen-counters` feature records End base-copy cells and bytes,
base reads, and transient-entry insertions in the `EndReplayBase` and
`EndReplayOverlay` memory boundaries. Entry insertions are cumulative across
removal and reinsertion, rather than a retained-entry gauge. A full dense
48-by-256-by-48 stitch copies 589,824 cells / 1,179,648 bytes; borrowed replay
copies zero base cells. The isolated platform gate predicts 201 base reads and
26 entry insertions for target `(6,0)`, and 121 reads and 16 insertions for
target `(7,1)`, with the same two accepted baseline overrides.

## Dependencies

The storage module uses the canonical state registry, standard library, and
worldgen counter boundary. `GeneratedColumn` additionally carries the biome, block
entity, heightmap, spawn, and stage sidecars; the server lifecycle owns the
next representation after consuming the compact handoff.
