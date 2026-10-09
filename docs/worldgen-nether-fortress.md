# Nether fortress generation

## What it is

The Nether fortress generator builds the whole recursive bridge-and-castle piece tree for a placed start. Its output is oriented, collision-free bounding boxes plus eager masonry, air, fence, stair, chest, garden and spawner block lists, translated together into the Nether's permitted structure height interval.

## How it works

- Generation starts at block-local `(2, 64, 2)` in the placement chunk. A random pending-child queue grows two weighted tables: bridge pieces can transition into castle pieces, which keep their own limits. A candidate box must be above the minimum floor and must not intersect an existing box. When the queue empties, the union of all boxes fixes one vertical translation into the inclusive 48..70 range.
- Translated nodes replay fifteen local masonry programs (crossings, straight spans, rooms, stairwells, spawner room, entrance and garden halls, castle corridors and turns, balcony, seeded terminal fill). Block states mirror and rotate with their piece, including fence connections and stair facings.
- Placement replays the same tree against each post-carve receiving chunk during that chunk's structure decoration step (not in the shaped column), then extends each piece's documented foundation cells down through replaceable material. A chest is created only when its position is in the receiving chunk; its facing comes from the four final horizontal neighbours and its loot seed from that chunk's xoroshiro structure-placement stream at decoration step 7, runtime structure index 1. `Terrain263::place_structures` returns the resolved loot with the block writes, and the Nether chunk source sends a Blaze `SpawnerState` sidecar with each spawner.
- Immutable piece states are strings only at the cross-generator cache boundary; each pass resolves distinct states once into the receiving generator's `StateId` interner and writes ids thereafter, keeping palette order and the explicit fallback for unknown plugin states without a per-block interner lock.
- The chunk source attaches structure blocks and block-entity sidecars only for a FULL column. Shaped admissions stay terrain-only dependencies; sending a chest or spawner sidecar from that stage would expose it before the structure completes.
- The eager piece list keeps chest positions, but its seed is not a packet source (no receiving grid, no placement stream). Packets must use the loot `place_structures` returns; re-deriving a position hash or reading `StructurePiece::loot` changes all four captured seeds.
- Evidence: `tests/support/nether_fortress_seed42_external.txt` transcribes independent seed-42 diagnostics: the full 90-child order, boxes, orientations, depths, all fourteen persisted piece ids and four orientation values, a terminal-fill box and seed, four chest positions/facings/loot seeds, two foundation columns, and hashes of spawner, chest and terminal packet captures. It comes from the source's NBT and packet output, not a sealed world root. `tests/fortress_piece_tree.rs` exercises the tree, terminal fill, directional fence, foundation walk and spawner packet provenance.

## How to change it

- `structure/fortress.rs` keeps random stream order beside each branch. Never pick a final Y from the root before growing children (tall rooms change the union and valid range). A new piece needs a geometry entry, a child-growth arm, an exact local writer, support behaviour if it has a foundation, and a table entry in its family.
- Keep `generate` and `place_for_chunk` on the same tree stream but tree construction and receiving-chunk placement as two random sources. Draw a chest seed only after the container passes the chunk clip and the existing-chest check. Update `fortress_piece_tree`'s captured rows only from a fresh external packet capture.
- Attach generated block entities from referenced starts, not only the origin start, so border-crossing payloads reach the receiving packet. Foundation replacement must read resident state at completion time, including an earlier neighbour's spill.

## Configuration

Constants, not data-pack settings: depth cap 30, horizontal spread 112 blocks from the start box, vertical interval 48..70. The bundled structure order for decoration step 7 is ancient city, fortress, Nether fossil; changing it changes fortress loot seeds.

## Dependencies

The structure stage's legacy random stream, `BoundingBox`, `DenseBlockGrid` and `StructurePiece`. Nether and Overworld placement consume the block lists; the fortress branch supplies the receiving grid for foundations.
