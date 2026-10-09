# Distant terrain LOD (the render-distance-512 tier)

## What it is

A design for extending the shipped coarse horizon ([`../distant-terrain.md`](../distant-terrain.md): one 9x9 atlas of 16-block cells out to 256 chunks, local Overworld only) to a multi-level world-anchored surface pyramid reaching 512 chunks, with wire streaming and chunk-derived detail. None of the pyramid, the `lodestone-lod` crate or the wire channel exists yet.

Real chunks at 512 are infeasible (about 32 GiB server residency, 195 GB client mesh, 58 GB wire, extrapolated from the measured 31.1 KiB per packed column). The far tier trades block identity for a heightfield, costing about 32 MiB CPU plus 32 MiB GPU and an estimated 4-8 MB on the wire.

## Design

### Representation

Four levels of 64x64-cell, world-aligned tiles, with cells of 2, 4, 8 and 16 blocks. Each level stores full coverage out to its own outer radius (0-64, 0-128, 0-256, 0-512 chunks), because the coarser level is the finer level's wire predictor and morph target; the overlap costs about 25%. Each level holds 1024x1024 cells, 8 MiB, so a further octave is a constant 8 MiB.

Per-cell record, 8 B: `u16` height (`y + 64`), `u16` water surface (`0xFFFF` = dry), RGB565 colour baked in gamma space with biome tint, and 2 B of flags (material class, reserved canopy offset). Normals are derived in the shader from neighbouring height texels.

The pyramid is shared per world; per-connection state is only which tiles were sent at which residual depth. Persistence would be a per-level tile cache beside the region files.

### Cell sources, in priority order

1. Saved chunk data (downsample the motion-blocking surface), so player builds appear at the resolution of their band.
2. Resident generated chunks, without a disk read.
3. The height query: `OverworldGenerator::preliminary_surface_level` plus `biome_at_quart` and `sea_level`, which never generates a chunk.

An edit dirties at most 4 covering tiles (one per level) via the `ChunkSource::set_block` choke point; a chunk-derived cell is never downgraded to query-derived.

### Fidelity

Kept: terrain silhouette, coastlines and water at real height, biome colour, day-night shading, fog continuity. Lost at distance: canopy geometry (forests read as green terrain), caves and overhangs, ores, light, entities. The End and Nether do not enable the tier. If block fidelity at 512 is required this is the wrong design; the alternative is chunk-derived LOD everywhere, which needs the progressive-generation plan and hours of compute.

### Compression

The lossy budget goes to colour (RGB565) and resolution (the pyramid), not height: heightfields are viewed edge-on, so lossy transform quantisation shows as silhouette wobble. Parent-predicted residuals plus zlib (`flate2`, already a server dependency) act as a wavelet decomposition with tile-local random access and cheap incremental update. Send the coarsest full-horizon level first, then refine inward. The uncompressed pyramid is already 32 MiB, so the codec is an optimisation, not a feasibility condition. A DCT-family codec, harder chunk compression, sparse voxel octrees, impostors and client-side seed generation were all rejected.

### Rendering

- A new pipeline (`lod_terrain.wgsl`) reuses bind group 0 (`Camera` + `Origin`, which carries fog) so near and far terrain cannot disagree about fog or the clock; group 1 is per-tile (height `R16Uint`, colour+flags `RGBA8`, water `R16Uint`, tile uniform).
- Vertex-pull heightfield: `textureLoad` in the vertex stage, one shared index grid (about 96 KiB), per-tile skirts. About 816 drawn tiles, roughly 1.7-3.3 M triangles per frame after culling (estimate).
- Depth is partitioned: draw the far tier with its own projection, clear depth, then draw the near scene. This avoids a 16k-block far plane in the near projection and the sign trap in depth bias (reversed-Z `[0,1]`, vanilla-positive bias is negative here).
- Seams: skip tiles fully inside the near field, draw LOD under real terrain across an overlap ring with a screen-door dither fade, morph edge vertices toward the parent level (clipmap style), and retarget the fog curve to the LOD horizon. Colour is baked in gamma space with the same tint and shade, or the seam shows as a brightness step.

### Wire and wasm

- A negotiated `lodestone:lod` channel over `CUSTOM_PAYLOAD` carries tiles; vanilla clients ignore unknown channels and receive none. Send coarse to fine, nearest first, one tile per `select!` pass, so the unserviced window holds at most one 4,096-cell encode.
- The builder is pure arithmetic (no clocks, threads or filesystem) with budgets in cells per tick, a counter. Browser singleplayer fills lazily nearest-first.

## Staged plan

Each stage lands independently and names a failing control for its gate.

0. **Measure first (go/no-go).** In an ignored `lodestone-worldgen` harness: cost per height query; query error versus the real generated surface over census-asserted terrains (error histogram and p95; control: a constant sea-level arm must report large error); real-terrain compression in bytes per cell; chunk downsample cost. If p95 land error exceeds about 2 L1 cells, L1 becomes chunk-derived only; if per-cell cost is 10x the 20 us illustrative figure, shrink bands or interpolate L3/L4.
1. **Name the seam.** `OverworldGenerator::surface_sample(x, z)` returning height, water surface and biome; golden-compared to generated heightmaps, with a perturbed-seed control.
2. **Dataset and builder.** A `lodestone-lod` crate: tile store, three-source builder, dirty tracking, persistence, codec. Gates: source priority (control: neuter the disk consult), edit marks at most 4 tiles, codec round-trip against captured real tile bytes, byte budget with a floor.
3. **Wire.** Negotiated channel and client decode. Gates: loopback byte diff, a play packet serviced mid-stream, and zero LOD payloads to a vanilla connection (control: remove negotiation).
4. **Heightfield pipeline.** Gates use rasterised readback only (a draw counter or a vertex-sampled probe is not evidence): synthetic tile coverage with a localising bounding box, a crack gate against a rendered no-LOD reference frame (control: disable skirts), a colour-seam gate (control: bake in linear space), and a depth-partition gate.
5. **Integration.** Hole tracking, crossfade, fog retarget, an independent LOD horizon setting. End-to-end pixel gate with a control routing tile streaming to `None`, which doubles as the island detector.
6. **Operating-point sweep.** Near-field N in {12, 16, 24, 32} x canopy offset x band radii; pick defaults interactively and record the curve in the constants' doc comments.

New counters (tiles resident, pyramid bytes, tiles sent) need an instrument-validation gate first: pure camera rotation must move them by exactly zero.

## Open decisions

- Canopy silhouettes beyond the chunk-derived band need a per-cell canopy offset from vegetation noise (reserved byte); measure first.
- Near-field vertex compression (per-quad instancing, about 4.5x over the 72 B packed quad) is an independent project, not on this critical path.
