# End world generation

## What it is

The End generator produces terrain prefixes and completed columns with typed
biomes, supplied client heightmaps, and structure/feature sidecars. Its compact
producer boundary preserves final-cell palette order without building a second
flat block-index field for the server.

## How it works

`EndGenerator::column_shaped` reads the same immutable base world used by End
feature replay. Terrain occupies the configured noise height; the retained
dimension window is 256 rows. Terrain configured above that window is cropped,
including its supplied map extent. The shaped producer only reads the terrain rows
and emits the upper default-air rows as uniform compact sections. Completed
columns read the entire retained window because structures and features can
write above the terrain ceiling.

`DenseBlockGrid::into_compact_column_box` folds a 16-by-16 crop in final-cell
`y,z,x` order. Canonical air is palette index zero. States are introduced when
first encountered in the cropped final field, so overwritten transient states
and states outside the crop do not enter its palette. Each section is packed
from one reusable section-sized buffer; uniform sections need no packed words.
Partial sections and a source row followed by padding share the same exact
index layout.

`EndColumn::into_compact_parts` moves the section storage, palette, biome quarts,
supplied maps, gateway exits, entity creation events, and structure mutations.
The server's `ChunkColumn::from_end` consumes this handoff. Compatibility callers
can still use `EndColumn::into_raw`, which explicitly expands the compact field
to the original flat layout.

Heightmaps and creation events are supplied products, not reconstructed final
state summaries. An overwritten chest can disappear from the final palette
while its earlier creation event remains present. Lifecycle admission retains
the shaped map seed separately and exposes maps at the existing feature
boundary; storage changes do not alter spill replay or map installation.

Pristine shaped identity requires the resolver's explicit
`immutable_shaped_asset_fingerprint` contract. Forwarding only the general asset
fingerprint does not qualify a wrapper. End shaped generation uses constructor
settings and immutable ID-keyed assets, not the resolver's selected singleton
biome parameter view. Dynamic resolvers and authoritative edited/retained
columns continue to use exact content identity.

## How to change it

Change cropped extraction in `DenseBlockGrid::into_compact_column_box` and the
producer in `EndGenerator::finish_column` together. Preserve final-cell palette
encounter order rather than transferring the working grid's introduction
history. The explicit source height is a crop plus default padding, not a
general terrain-emptiness certificate; never pass the noise height for a
completed feature output.

Keep `EndColumn::into_raw` compatible for fixture and diagnostic consumers, and
keep production consumers on `into_compact_parts`. Preserve supplied maps and
all sidecars when extending the parts type. Bump
`EndGenerationIdentity::SHAPED_VERSION` when terrain, biome, surface, or shaped
storage rules change. Revision 2 identifies the compact shaped-storage contract.

The compact crop controls predict palette indices independently, including
overwritten states, a cropped source, nonzero origins, uniform sections, and
partial padding. The End parts control carries deliberately distinct supplied
maps and an overwritten entity creation event. Existing End fixture, upper-city,
source-order, and lifecycle spill-map controls cover the wider generation chain.

## Configuration

Constructor settings supply the noise height, origin, cell geometry, surface
rules, and block/fluid defaults. The End requires the legacy random source.
`WORLD_HEIGHT` in the producer is 256, distinct from the usual 128-row terrain
height. The server serves the End from the 26.3 source instead
([26.3 world source](worldgen-world-263.md)).
There is no compact-handoff flag or additional cache.

## Dependencies

The producer uses `DenseBlockGrid`, shared `CompactBlockStorage`, the density and
surface pipeline, End biome sampling, and typed structure/decoration products.
The server consumer uses its existing section storage, palette metadata,
heightmap installation, and lifecycle admission/revocation paths. See
[generated storage](worldgen-generated-storage.md) and
[End decoration order](end-decoration-order-oracle.md).
