# Terrain culling and draw submission

## What it is

The per-frame decision about which resident chunk sections actually get a draw call, and the shared
vertex/index arena that makes each of those draws cheap. Together they are what makes render distance
30+ playable: before this, every resident section issued a draw at every heading, measured at
**19,024 instructions per section** — 17.7M per frame at the shipped render distance 8 (issue #543).

## How it works

Two independent halves, both landing in `crates/lodestone-shell/src/gpu/frame.rs`'s three terrain
loops (packed demo table, live opaque, live water).

### The cull — `crates/lodestone-render/src/cull.rs`

`TerrainCull::new(camera, render_distance_chunks)` is built **once per frame** in `render_inner` and
consulted by all three loops, so water and terrain physically cannot disagree about what exists.
`classify(section_coord)` returns one of four verdicts, cheapest test first:

| verdict | test | note |
|---|---|---|
| `Distance` | `within_view_distance` | vanilla's `ChunkTrackingView.isWithinDistance`, verbatim |
| `Frustum` | `Frustum::section_visible` | against the camera-cube-offset frustum |
| `Occlusion` | membership of an installed reachable set | only when `with_reachable(Some(..))` was called — nothing does yet |
| `Visible` | — | draw it |

`within_view_distance` is a **rounded circle with a one-chunk buffer**, `max(0,|d|-1)² summed < rd²`,
not the streamed square. It is heading-independent and removes 29% / 25% / 23% of the streamed square
at rd 8 / 16 / 32 (257 of 361, 921 of 1225, 3461 of 4489 columns — the unit test asserts those exact
counts, computed from the Java rather than from the Rust). The one-chunk buffer is what makes the
strict `<` safe: the ring at exactly `rd` is still kept, and `fog.rs`'s render-distance fog reaches its
end value at `rd·16` anyway.

`Frustum::offset_to_include_camera_cube(camera, 8.0)` is vanilla's
`Frustum.offsetToFullyIncludeCameraCube(8)`. Vanilla walks the frustum origin backwards in 4-block
steps until the camera's 8-block-aligned cube is fully inside; we do the closed form — push outward
only the planes that cut that cube, only as far as the deficit. **Without it the section you are
standing in flickers** as you cross cell boundaries, because the near plane slices it.

Counters land in `RenderStats` (`gpu/stats.rs`): `sections_drawn`, `sections_culled_distance`,
`sections_culled_frustum`, `sections_culled_occlusion`, and `water_sections_drawn` /
`water_sections_culled`. The invariant closes **per pass**, not globally: a water-only section carries
`mesh: None`, so it never reaches `sections_drawn` while still issuing a draw (measured 189
`sections_drawn` against 195 uploads and 304 `draw_calls` before culling existed). A single combined
invariant reads as a cull bug on a perfectly healthy frame.

### The arena — `crates/lodestone-render/src/model_arena.rs`

Every live section mesh is suballocated out of shared **blocks** (a 32 MiB vertex arena paired with an
8 MiB index arena) instead of owning two `wgpu::Buffer`s. Blocks are appended on demand, never
released. A draw is then `set_bind_group` (dynamic origin offset) + `draw_indexed` — two encoder calls
instead of four, because `set_vertex_buffer`/`set_index_buffer` moved out of the per-section loop.

`gpu/frame.rs`'s `emit_terrain_draws` collects the visible set into a `Vec<TerrainDraw>`, sorts it by
block, and binds each block's buffer pair once. Collect-then-emit rather than one loop for two reasons:
the cull runs exactly once per section, and consecutive draws share the bind. The bind count is
reported as `RenderStats::terrain_buffer_binds` — low tens at any render distance, where it used to
equal `sections_drawn + water_sections_drawn`.

`base_vertex`/`first_index` are element counts, so the byte offsets must divide exactly. That holds
because the vertex arena's alignment **is** `MODEL_BYTES_PER_VERTEX` (32, a power of two) and the index
arena's is 4. Both are debug-asserted: an offset one byte off does not fail to draw, it draws *shifted
geometry*, which reads as a meshing bug several layers from the cause.

A mesh the arena cannot place degrades to `ResidentMesh::Dedicated` (its own buffer pair, a logged
warning, `block == u32::MAX` so it sorts last) rather than becoming a hole in the world.

## How to change it

* **Adding a cull** — add a `CullVerdict` variant and a test in `cull.rs`, then a counter arm in
  `frame.rs`'s opaque loop. Do not add a second predicate at a call site: the single `TerrainCull`
  is what keeps the three loops in agreement.
* **Turning culling off** — `RenderState::set_terrain_culling(false)`. This is vanilla's `smartCull`
  equivalent and the one-call false-cull diagnosis: if missing terrain reappears with it off, a cull
  dropped it; if it does not, the section was never resident and the bug is upstream in streaming or
  meshing. `tests/client_chunk_cycles.rs` measures both arms with it.
* **Wiring the occlusion graph (U3)** — `visibility.rs`'s `walk_visible` produces the reachable set;
  hand it to `TerrainCull::with_reachable`. Two traps are already handled and one is not:
  * `walk_visible` **merges source faces on re-reach** (vanilla's
    `SectionOcclusionGraph.addNeighbors:342-344`). The earlier single-entry-face version over-culled —
    a section reachable through two faces but first visited through the wrong one lost its exits,
    which is terrain vanishing at specific angles. `walk_merges_source_faces_reached_by_a_second_path`
    pins it, in both axis arms, because which face is "first" depends on `Face::ALL`'s order.
  * `with_reachable(None)` degrades to frustum ∩ distance, which draws *more*, never less.
  * **Not handled:** the graph must contain **every** section of every resident column, empty ones as
    `SectionVisibility::all()`. `walk_visible` stops at any coord missing from the graph, and all-air
    sections are never uploaded as geometry — so a graph built only from meshed sections dies at the
    first air gap above the terrain and silently falls back to pure frustum forever.
* **Block sizing** — `DEFAULT_VERTEX_BLOCK_BYTES` / `DEFAULT_INDEX_BLOCK_BYTES`. The index block must
  exceed `3/16` of the vertex block (4 vertices and 6 indices per quad) or vertex space is stranded;
  a unit test asserts that ratio.
* **Do not propose multi-draw indirect.** `crates/lodestone-render/src/strategy.rs` measured it: wgpu
  30 on Metal CPU-emulates base multi-draw as a per-draw loop and exposes no
  `MULTI_DRAW_INDIRECT_COUNT`, so `PerDraw` is correct on this backend and the only saving available
  is encoder state per draw.

## Configuration

* `Config::render_distance` reaches `RenderState` through `set_fog(fog, render_distance_chunks)`
  (`app/redraw.rs`, every frame). `render_distance_chunks == 0` **disables** the distance test rather
  than culling everything — zero is what a default-constructed `RenderState` holds, and a cull that
  blanks the world on a default state is indistinguishable from a broken renderer.
* `RenderState::set_terrain_culling(bool)` — on by default.

## Dependencies

* `lodestone_render::camera::{Camera, Frustum}` — Gribb–Hartmann planes for the `[0,1]` depth
  convention.
* `lodestone_render::visibility` — the `VisGraph` port and the camera walk (built, not yet consumed in
  production).
* `lodestone_render::arena`/`suballoc` — the GPU-backed arena and its pure address-ordered first-fit
  allocator.
* `crate::mesher::SectionKey::coord()` — the `SectionKey` → section-grid conversion, via `div_euclid`
  because `min_y` is negative in the overworld.

## See also

* [`docs/plans/render-performance.md`](./plans/render-performance.md) — the sequenced plan (U1–U5) and
  the rejected candidates with the constraint that killed each.
* [`docs/client-chunk-cycles.md`](./client-chunk-cycles.md) — the instruction-counting method behind
  the 19,024-per-section figure.
* [`docs/section-camera-uniform.md`](./section-camera-uniform.md) — the earlier per-section-uniform fix
  (issues #75/#76) whose shared-bind-group shape this builds on.
