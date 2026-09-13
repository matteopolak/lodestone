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
palettes. The first is the protocol's non-empty count, excluding `air`,
`cave_air`, and `void_air`; the second counts every state with a non-empty
fluid state, including waterlogged blocks. The version-specific count is
computed at the wire boundary so an all-cave-air section can retain its payload
while reporting zero. Fluid block-state ids remain unchanged in the block palette, so a
flowing-water or lava `level` property reaches the client as well as the
section's aggregate count. Immediately before serialization, both containers
are rebuilt from their decoded-value order. This removes palette entries no
longer referenced after edits, remaps packed indices without changing any
decoded cell, and collapses uniform sections to the single-value form. The
order is the wire container's `y`, `z`, `x` traversal, so unused leading or
middle entries cannot make otherwise identical packets diverge.

The final light payload is computed from the version's state-id opacity and
emission census. Light-relevant edits pass each already-resident member of the
3x3 neighbourhood to `V770ServerProtocol::compute_column_light_with_neighbours`;
its result is exact for the centre chunk when all eight are present because
light cannot cross more than one 16-block chunk boundary at its maximum range.
A missing neighbour is an opaque seam, never a request to generate it. For an
initial Nether chunk, the centre block-light layer is retained while neighbour
emission waits for the subsequent seam-aware update; the 3x3 footprint still
determines allocation-only zero masks. The worker-facing `ChunkEncoder` remains
a one-column contract, so this family deliberately uses the synchronous path.
Initial chunk encoding elides uniformly zero block-light sections only when
their storage is unallocated; explicit zero sections remain available to
light-update packets that clear an existing value.

The decoder in `packets::chunk::LevelChunkWithLight` consumes both section
counters for alignment, bounds the length-prefixed section blob, and applies
the decoded heightmaps and light through `V770Adapter`'s world sink.

The clientbound `block_destruction` frame is decoded as a VarInt breaker id,
a packed block position, and one raw stage byte. `V770Adapter` emits those
fields as `ClientEvent::BlockDestruction` without interpreting the stage, so
the shared session overlay can distinguish visible stages from reset values.
The literal dispatch fixture in
`crates/versions/26.2/tests/chunk_world/world_events.rs` also rejects trailing
bytes, keeping the packet boundary independent of the encoder.

Entity attribute updates are decoded by `V770Adapter`'s entity dispatcher as an
entity id followed by registry-id snapshots, each with an `f64` base and
textual modifier records. The adapter emits
`ClientEvent::EntityAttributesUpdated`; the ECS ingest schedule resolves the
id through `EntityIndex` and merges each snapshot into the entity's
`Attributes` component, replacing only a matching attribute key. The
adapter-to-ingest fixture in
`crates/versions/26.2/tests/entity/attributes_ingest.rs` uses literal packet
bytes and checks the resulting component, so a decoder that emits nothing or a
route that stops before ECS state cannot pass.

The server-side facade keeps `V770ServerProtocol` and its single
`ServerProtocol` implementation in `src/server_protocol.rs`, while private
siblings group the phase work: `serverbound.rs` contains strict decode
primitives, `clientbound.rs` contains small hand-written outbound bodies,
`registry.rs` owns captured Configuration registry payloads, and `chunk.rs`
owns terrain conversion, light settlement, and the `ChunkEncoder` boundary.
The public type and source-based wiring tools therefore keep their historical
path while the large implementation is navigable by protocol direction.

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

When changing `update_attributes`, keep its registry-id table and field order
aligned with the protocol capture. Update the literal adapter-to-ingest fixture
with an independently calculated byte vector and a value that distinguishes the
base from every modifier operation under test; do not replace it with an
encode/decode round trip. If the event shape changes, update the model event,
the `IngestSet::Apply` fold, and this end-to-end fixture together.

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

When adding a packet helper, place strict serverbound readers in
`src/server_protocol/serverbound.rs` and raw clientbound bodies in
`src/server_protocol/clientbound.rs`; keep the trait dispatch in the facade so
`cargo xtask connectedness` can continue to classify every serverbound arm.
Chunk and light changes belong in `chunk.rs`, and captured Configuration bytes
belong in `registry.rs`.

## Dependencies

- `lodestone-server` for `ChunkColumn`, chunk scheduling, and resident
  neighbourhoods used by relight.
- `lodestone-world` for palette containers, packed heightmaps, and light
  propagation.
- `lodestone-data` for validated block-state, fluid, leaf, opacity, and
  emission facts.
- The external 26.2 chunk-content capture and the checked-in 26.2 generated
  registry reports for wire and registry evidence.
