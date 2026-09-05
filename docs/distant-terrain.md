# Distant terrain foundation

## What it is

The distant-terrain foundation is a bounded, coarse heightfield representation for a future visual
horizon beyond the real streamed chunk radius. It is not a chunk cache and it is not currently
drawn; the existing terrain path remains the only live renderer.

## How it works

`lodestone_render::DistantTerrain` holds a world-aligned 9×9 square of 64×64-cell tiles. Each
cell covers 16×16 blocks and stores terrain height, optional water height, RGB565 surface colour,
and flags in eight bytes. At the 256-chunk visual horizon this has a fixed 2,654,208-byte CPU
ceiling. Recentring replaces tile coordinates and clears samples without growing the allocation.

The future population path uses `OverworldGenerator::preliminary_surface_level`, biome lookup,
and sea level without generating a real chunk. A dedicated `lod_terrain.wgsl` shader validates now
and is designed for vertex-pulled height and colour textures, but no `wgpu` pipeline or shell draw
call is constructed yet.

## How to change it

Keep `HORIZON_CELL_BLOCKS`, `HORIZON_TILE_CELLS`, and the tile radius derived together: changing
any one changes the fixed memory budget and coverage. Maintain the floor-division behavior in
`HorizonTileCoord::containing_block`; truncating negative positions would shift a horizon tile at
the world origin. The first screen-visible integration must add a far pass outside the normal
chunk mesh loop and demonstrate a visible horizon plus an omitted-tile detector control.

## Configuration

`MAX_HORIZON_DISTANCE_CHUNKS` is 256, but it is a representation limit rather than a player
setting. A future separate horizon-distance option must leave real chunk render distance and the
server view radius unchanged. The default remains disabled until the shell owns a populated draw
path.

## Dependencies

The model is pure Rust. Its eventual data source is `lodestone-worldgen`'s query-only overworld
surface estimate; its eventual GPU consumer is a shell-owned `wgpu` pipeline using
`crates/lodestone-render/src/shaders/lod_terrain.wgsl`.
