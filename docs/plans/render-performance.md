# Plan: render performance - culling first, then submission

## What it is

The plan for scaling terrain rendering from render distance 8 to 16 and 32. Frustum culling, the circular view-distance cull, the section occlusion graph and cross-section water ordering are shipped; this document keeps the remaining open work and the rejected alternatives so they are not re-derived.

## How it works

Culling lives in `render_inner` in `crates/lodestone-shell/src/gpu/frame.rs`. It calls `TerrainCull::classify` per section and feeds `RenderStats` counters (`sections_drawn`, `sections_culled_distance`, `sections_culled_frustum`, `sections_culled_occlusion`, `water_sections_drawn`, `water_sections_culled`). `crates/lodestone-shell/src/gpu/occlusion.rs` is the camera-walk consumer of the occlusion graph.

Water sections are sorted back to front by section-centre distance each frame (`TerrainDraw::sort_dist2`).

### Circular distance cull (U2 table)

A column is in range when `max(|dx|-1, 0)^2 + max(|dz|-1, 0)^2 < rd^2`, a rounded circle with a 1-chunk buffer. Expected membership was computed from the reference expression, independently of the Rust; `lodestone_render::cull` tests assert it.

| rd | streamed square | drawn circle | culled |
|---|---|---|---|
| 8 | 361 | 257 | 104 (29%) |
| 16 | 1,225 | 921 | 304 (25%) |
| 32 | 4,489 | 3,461 | 1,028 (23%) |

The boundary column `(9,0)` at rd 8 pins the strict inequality (`64 < 64` is false).

## Open work

- **U4, draw-submission reduction.** Suballocate every `ModelSectionGpu` mesh from shared vertex and index arenas per pass, following the `section_arena.rs` pattern. Target: encoder calls from about `4 x drawn` to `2 x drawn + 4`, and buffer objects from `2 x sections` to 4, with `draw_calls` unchanged. Gate it with a pixel-identical A/B against the per-buffer path; no production code uses `SectionArena` yet.
  - Optional second step, only if the origin bind measures hot: an origin array indexed by `instance_index`. Check non-zero `first_instance` on the wgpu Metal backend first.
- **U5 remainder, intra-section water resort.** `SortViewpoint` and `TranslucentMesh` exist but production water keeps mesh-time index order within a section. Resort on octant change and re-upload the section's index buffer. Cadence: nearby sections when the camera block changes, plus a round-robin `max(visible/8, 15)` per frame. A gate must cross an octant boundary and see exactly one resort.

## Constraints

- Four bind groups is the floor and the model shader uses all four; no unit may add a fifth.
- Depth is reversed-Z `[0,1]`; CPU culling is depth-agnostic.
- Vanilla parity is the premise: a cull must hide only what vanilla does not draw, or be provably pixel-identical.
- State wins as draw/section/quad counters. Wall-clock deltas on the dev machine reproduce only to about 11%.

## Rejected

| candidate | killing fact |
|---|---|
| Multi-draw indirect (zero-instance or count) | Metal in wgpu emulates base multi-draw as a CPU loop; `MULTI_DRAW_INDIRECT_COUNT` is absent. `PerDraw` is the right strategy here |
| GPU compute culling | its output is consumable only via multi-draw, which is emulated |
| Depth prepass | doubles draw calls; Apple-silicon TBDR already removes opaque overdraw |
| Second reversed-Z flip | maximal churn, no observed z-fighting |
| Front-to-back opaque sort | TBDR already provides the win |
| Instanced section drawing | every section mesh is unique |
| LOD / reduced far detail | visual change; parity is the premise |
| Fog-derived "fully fogged" cull | not pixel-identical against the sky gradient; the circle cull is the parity-correct version |
| Vanilla's long-range ray-march cull | deferred; known false-cull history. Revisit with occlusion counters if the reachable set at rd 32 still hurts |

## Dependencies

`lodestone-render` (`cull`, `translucency`, `section_arena`), `lodestone-shell` `gpu/frame.rs` and `gpu/occlusion.rs`, and wgpu's Metal backend capabilities.
