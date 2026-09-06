# The 26.2 protocol era: generated chunks on the wire

## What it is

`crates/versions/26.2` (package `lodestone-v26-2`, registry feature `v26-2`)
hosts protocol 776. Its `level_chunk_with_light` encoder turns a server
`ChunkColumn` into a complete 26.2 chunk body: state and biome sections,
client heightmaps, block entities, and light.

## How it works

The packet starts with chunk coordinates, then a typed list of the three
client-visible heightmaps: world surface, motion blocking, and motion blocking
without leaves. Their registry ids are 1, 4, and 5. Each value is the first
free Y relative to the dimension minimum. `served_heightmaps` scans the same resolved state ids that
`build_world_column` writes, so generated terrain,
imported terrain, and later block edits have one source of truth. The all-air
answer is zero for every map.

Each section then writes two signed 16-bit counters before its block and biome
palettes. The first is the non-air count supplied by `ChunkSection`; the
second counts every state with a non-empty fluid state, including waterlogged
blocks. Fluid block-state ids remain unchanged in the block palette, so a
flowing-water or lava `level` property reaches the client as well as the
section's aggregate count.

The final light payload is computed from the version's state-id opacity and
emission census. For initial chunks and light-relevant edits, the server passes
each already-resident member of the 3x3 neighbourhood to
`V770ServerProtocol::compute_column_light_with_neighbours`; its result is exact
for the centre chunk when all eight are present because light cannot cross more
than one 16-block chunk boundary at its maximum range. A missing neighbour is
an opaque seam, never a request to generate it. The worker-facing
`ChunkEncoder` remains a one-column contract, so this family deliberately uses
the synchronous neighbourhood path instead of silently emitting isolated light
from a worker.

The decoder in `packets::chunk::LevelChunkWithLight` consumes both section
counters for alignment, bounds the length-prefixed section blob, and applies
the decoded heightmaps and light through `V770Adapter`'s world sink.

## How to change it

Keep `encode_column_body` and `LevelChunkWithLight::decode` in matching wire
order. A changed section prefix must be covered by a test that reads the raw
prefix: a decode/encode cycle cannot prove a discarded counter was truthful.
Use a state fixture with distinct fluid levels when changing fluid handling,
and test waterlogged states separately when changing the fluid predicate.

If a new heightmap is sent, add its explicit registry id and predicate to
`served_heightmaps`, then use inputs where its answer differs from every
existing map. Do not infer a predicate from a visually similar material; use
the checked-in per-state census or an external packet/chunk capture.

For initial neighbour-aware light, preserve the server's resident-only rule:
pass a consistent centre plus any resident neighbours all the way to
`V770ServerProtocol`, but do not generate neighbours merely because a join
spiral is sending a chunk. If the worker encoder is extended to carry a full
neighbourhood, retain byte identity with the resident path. Keep the focused
wire tests and the external chunk-capture replay in
`crates/lodestone-fuzz/tests/fixtures/chunk_content_26_2.json` green, then run
`cargo xtask connectedness` to confirm the packet still has its registered
encoder and adapter consumer.

## Configuration

There is no feature flag for chunk counters, heightmaps, or lighting. The
dimension shape comes from the synchronized dimension type: its minimum Y and
height choose the section count and heightmap bit width. The current host maps
the standard overworld window and the shared Nether/End window in
`shape_for_column`.

## Dependencies

- `lodestone-server` for `ChunkColumn`, chunk scheduling, and resident
  neighbourhoods used by relight.
- `lodestone-world` for palette containers, packed heightmaps, and light
  propagation.
- `lodestone-data` for validated block-state, fluid, leaf, opacity, and
  emission facts.
- The external 26.2 chunk-content capture and the checked-in 26.2 generated
  registry reports for wire and registry evidence.
