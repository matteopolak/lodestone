# Nether fortress generation

## What it is

The Nether fortress generator constructs the entire recursive bridge-and-castle piece tree for a placed start. Its output is a set of oriented, collision-free bounding boxes plus eager masonry, air, fence, stair, chest, garden, and spawner block lists translated together into the Nether's permitted structure height interval.

## How it works

Generation begins at block-local `(2, 64, 2)` in the placement chunk. A random pending-child queue grows two separate weighted tables: bridge pieces can transition into castle pieces, while castle pieces keep their own limits. Candidate boxes must be above the minimum floor and must not intersect an existing box. Once the queue is exhausted, the union of every box determines the one vertical translation into the inclusive 48..70 range. The translated nodes emit `CodedBlock` lists in local-coordinate operation order, which the placement stage clips to each receiving chunk; chest nodes also attach a `CodedLoot` record. The placement-time replay reads the post-carve chunk grid for downward masonry supports, so a column stops at the terrain boundary rather than overwriting solid terrain.

## How to change it

`structure/fortress.rs` keeps the random stream order beside each branch. Do not generate a final Y from the root before growing children: tall rooms change the completed union and therefore the valid translation range. If a new piece is added, give it a geometry entry, a child-growth arm, support behavior where it has a foundation, and an explicit table entry in its appropriate family. Keep `generate` and `place_for_chunk` on the same tree stream; the latter may differ only where it queries the receiving grid for supports.

## Configuration

The depth cap is 30, the horizontal spread is 112 blocks from the start box, and the final vertical interval is 48..70. These are generation constants rather than data-pack settings.

## Dependencies

The generator uses the structure stage's legacy random stream, `BoundingBox` collision tests, `DenseBlockGrid`, and `StructurePiece` records. The Nether and Overworld structure placement stages consume the block lists; their fortress branch supplies the receiving grid used by foundations.
