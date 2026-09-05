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
and sea level without generating a real chunk. The shell now has an inert `DistantTerrainRenderer`
bridge: two fixed 576×576 `R32Uint` atlases store height/water and colour/flags (another 2,654,208
bytes on the GPU), and a vertex-pulled `lod_terrain.wgsl` pipeline can submit only tiles whose
surface samples were uploaded. It has no `RenderState` owner or redraw caller yet, so it still
cannot alter normal rendering.

## How to change it

Keep `HORIZON_CELL_BLOCKS`, `HORIZON_TILE_CELLS`, and the tile radius derived together: changing
any one changes the fixed memory budget and coverage. Maintain the floor-division behavior in
`HorizonTileCoord::containing_block`; truncating negative positions would shift a horizon tile at
the world origin. The first screen-visible integration must store this renderer as an optional
`RenderState` field, populate one tile per redraw from the query source, and submit it before
normal chunk meshes in the same depth-tested pass. Do not add it to `TerrainCull` or expand the
normal camera far plane. The pixel gate must prove pixels beyond the real chunk field and include
the supplied omitted-tile detector control: a synthetic draw submission for an unpopulated slot
must fail.

## Configuration

`MAX_HORIZON_DISTANCE_CHUNKS` is 256, but it is a representation limit rather than a player
setting. A future separate horizon-distance option must leave real chunk render distance and the
server view radius unchanged. The default remains disabled until the shell owns a populated draw
path.

## Dependencies

The model is pure Rust. Its query source is `lodestone-worldgen`'s query-only overworld surface
estimate; the shell bridge uses `wgpu` and `crates/lodestone-render/src/shaders/lod_terrain.wgsl`.
