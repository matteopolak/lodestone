# Distant terrain

## What it is

Distant terrain is a bounded, coarse heightfield visual horizon beyond the real streamed-chunk
radius. It is a local integrated-Overworld feature, not a chunk cache: it cannot request, retain,
or mesh ordinary chunks.

## How it works

`lodestone_render::DistantTerrain` holds a world-aligned 9×9 square of 64×64-cell tiles. Each
cell covers 16×16 blocks and stores terrain height, optional water height, RGB565 surface colour,
and flags in eight bytes. At the 256-chunk visual horizon this has a fixed 2,654,208-byte CPU
ceiling. Recentring replaces tile coordinates and clears samples without growing the allocation.

For an eligible local world, the net thread publishes `HorizonSurfaceQuery`, a separate immutable
`OverworldGenerator` estimate after the selected source has opened successfully. Its sample path
uses `preliminary_surface_level` and sea level only; it does not enter the generated-column store.
Remote sessions, source overrides, unsupported world presets, and non-Overworld dimensions have no
query and skip this pass.

`DistantTerrainRenderer` owns two fixed 576×576 `R32Uint` atlases for height/water and
colour/flags (another 2,654,208 bytes on the GPU). The redraw path installs it only while Distant
Horizon is enabled, recentres it around the camera, applies the configured outer radius, and uploads
at most one 64×64-cell tile per redraw. The shader clips both the near streamed field and the outer
horizon circle. Tile residency considers only tiles intersecting that circle, so a small setting
does not gradually populate the full 9×9 atlas. Shrinking the radius stops submitting outer tiles;
growing it reuses resident inner tiles and fills newly eligible tiles without growing the atlas.
Dry cells draw the terrain surface, while wet cells draw at their stored water-surface height.

## How to change it

Keep `HORIZON_CELL_BLOCKS`, `HORIZON_TILE_CELLS`, and the tile radius derived together: changing
any one changes the fixed memory budget and coverage. Maintain the floor-division behavior in
`HorizonTileCoord::containing_block`; truncating negative positions would shift a horizon tile at
the world origin. Keep the query boundary narrow: do not expose `ChunkSource` to rendering and do
not add this representation to `TerrainCull`. The visual camera needs a far plane large enough for
the selected coarse radius, but picking, audio, normal streaming, and normal terrain culling must
continue to use their existing render-distance camera or radius. Set the near clip from the real
chunk radius so the coarse pass cannot paint inside the streamed disk. The pixel gate must prove
pixels beyond the real chunk field and include the omitted-tile detector control: a synthetic draw
submission for an unpopulated slot must fail.

## Configuration

`Options::horizon_distance_chunks` is persisted as `horizon_distance_chunks`, ranges from `0` to
`MAX_HORIZON_DISTANCE_CHUNKS` (256), and defaults to `0` (OFF). The Video screen names it
**Distant Horizon**, separate from **Render Distance**. It is deliberately independent of the real
render-distance setting and does not alter server view radius, streamed chunks, mesh queues, or the
fixed atlas size. The CPU/GPU atlases exist only while the option is nonzero in an eligible local
Overworld session. Values are shown in chunks and the cycle control advances in 16-chunk coarse cells.

## Dependencies

The model is pure Rust. Its query source is `lodestone-worldgen`'s query-only overworld surface
estimate; the shell bridge uses `wgpu` and `crates/lodestone-render/src/shaders/lod_terrain.wgsl`.
