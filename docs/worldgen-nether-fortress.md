# Nether fortress generation

## What it is

The Nether fortress generator constructs the entire recursive bridge-and-castle piece tree for a placed start. Its output is a set of oriented, collision-free bounding boxes plus eager masonry, air, fence, stair, chest, garden, and spawner block lists translated together into the Nether's permitted structure height interval.

## How it works

Generation begins at block-local `(2, 64, 2)` in the placement chunk. A random pending-child queue grows two separate weighted tables: bridge pieces can transition into castle pieces, while castle pieces keep their own limits. Candidate boxes must be above the minimum floor and must not intersect an existing box. Once the queue is exhausted, the union of every box determines the one vertical translation into the inclusive 48..70 range.

The translated nodes replay all fifteen local masonry programs: crossings, straight spans, rooms, stairwells, the spawner room, entrance and garden halls, castle corridors and turns, balcony, and the seeded terminal fill. Local block states are mirrored and rotated with their piece, including fence connection properties and stair facings. The placement stage replays the same tree against each post-carve receiving chunk, then extends only each piece's documented foundation cells down through replaceable material. A chest is created only when its position belongs to that receiving chunk. Its facing is resolved from the four final horizontal neighbors, and its loot seed is drawn from that chunk's xoroshiro structure-placement stream at decoration step 7, runtime structure index 1. `NetherColumn::placement_loot` carries that resolved sidecar alongside the blocks. The Nether chunk source also sends a Blaze `SpawnerState` sidecar with each emitted spawner block.

Receiving-chunk placement keeps the immutable piece states as strings only at the cross-generator cache boundary. Each piece pass resolves distinct states once into the receiving generator's `StateId` interner and writes ids thereafter, preserving palette order and the explicit fallback for unknown plugin states without taking the interner lock once per block.

`tests/support/nether_fortress_seed42_external.txt` is a compact, committed transcription of independent seed-42 diagnostics: it fixes the full 90-child order, boxes, orientations, depths, all fourteen persisted piece ids and all four orientation values; it also records a terminal-fill box and seed, four chest positions/facings/loot seeds, two foundation columns, and hashes of spawner, chest, and terminal packet captures. The source materializer had an in-flight journal, so the fixture is evidence from its NBT and packet output, not a reusable or sealed world root. The controls exercise the tree, terminal random fill, directional fence, foundation walk, and the exact Blaze spawner sidecar through `NetherChunkSource`.

The eager persisted piece list retains chest positions, but its seed is not a packet source: it has neither a receiving grid nor the placement stream. Packet construction must consume `NetherColumn::placement_loot`, which is the result produced inside the real grid-aware placement pass. Re-deriving a position hash, or reading `StructurePiece::loot`, changes all four captured seeds.

## How to change it

`structure/fortress.rs` keeps the random stream order beside each branch. Do not generate a final Y from the root before growing children: tall rooms change the completed union and therefore the valid translation range. If a new piece is added, give it a geometry entry, a child-growth arm, an exact local writer, support behavior where it has a foundation, and an explicit table entry in its appropriate family. Keep `generate` and `place_for_chunk` on the same tree stream, but keep tree construction and receiving-chunk placement as two distinct random sources. A chest seed is drawn only after the container position passes the chunk clip and an existing chest check. `fortress_piece_tree` carries the captured seed-42 start, piece boxes, masonry/fence sentinels, and four exact chest state/seed rows from an external chunk; update that control only from a fresh external packet capture. When adding a generated block entity, attach it from referenced starts, never only the origin start, so border-crossing payloads remain present in the receiving chunk packet.

## Configuration

The depth cap is 30, the horizontal spread is 112 blocks from the start box, and the final vertical interval is 48..70. These are generation constants rather than data-pack settings. The bundled runtime structure order for decoration step 7 is ancient city, fortress, Nether fossil; changing that registry order changes fortress loot seeds.

## Dependencies

The generator uses the structure stage's legacy random stream, `BoundingBox` collision tests, `DenseBlockGrid`, and `StructurePiece` records. The Nether and Overworld structure placement stages consume the block lists; their fortress branch supplies the receiving grid used by foundations.
