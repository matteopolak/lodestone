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
| `Occlusion` | membership of an installed reachable set | the occlusion graph's camera walk — see below |
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

### The occlusion graph — `crates/lodestone-render/src/visibility.rs` + `crates/lodestone-shell/src/gpu/occlusion.rs`

Vanilla's cave culling, and the only one of the three that can remove the **underground**: standing on
the surface, the frustum contains the whole column below you and the distance circle keeps all of it.

Three parts, in the order the data flows:

1. **Producer, in the mesh worker.** `mesher::snapshot_visibility` floods the section's non-opaque cells
   (`compute_visibility_from` over `BlockModels::occludes`, vanilla's `isSolidRender` family) and records
   which of the 15 face pairs connect. It rides on `SectionGeometry::Model { .., visibility }` rather than
   on `Meshed`, which is what lets it reach the graph with **no** change to the three `upload_section`
   call sites in `app/`.
2. **Graph, on `RenderState`.** `upload_section` inserts, `remove_section` drops. Every *meshed* section
   is in it **including the ones with no geometry** — a fully-enclosed underground section meshes to
   nothing and is exactly the blocker that stops the walk descending.
3. **Walk, per 8-block cell.** `lodestone_render::reachable_from_camera` (the whole decision, so the
   angle-sweep gate drives production and not a copy of it) → cached in `gpu/occlusion.rs` keyed on
   `(camera 8-block cell, graph generation)` → `TerrainCull::with_reachable_mode`. Vanilla's cadence:
   the frustum is **not** in the walk, so turning on the spot re-walks nothing.

**The trap, and why it fails silently.** `walk_visible` stops at any coord the graph does not hold, and
**all-air sections are never meshed** (`snapshot_section_in` returns `SnapshotOutcome::Empty`, which never
reaches a worker). A graph of meshed sections alone therefore dies at the first air gap above the terrain,
the reachable set comes back nearly empty, and every safety valve then does the safe thing — which is to
draw *more*. The result looks like it works and costs exactly what it cost before, forever.
`walk_visible_bounded` is the fix and it is vanilla's own model, not a workaround: `ViewArea` holds a
render section for every section in the cylinder regardless of content, so an absent in-bounds coord is
air, i.e. `SectionVisibility::all()`. The walk's bounds are the view circle × the graph's `y_extent`
(monotonically widened, never narrowed — a too-wide range costs a few empty rows and draws more).

**Assert the graph's size, not its behaviour.** `RenderStats::occlusion_graph_sections` must be **≥**
`section_count()` — the graph holds strictly more than the sections that draw. A value that tracks
`sections_drawn` means the empty sections are missing and the walk has no floor.
`RenderStats::occlusion_active` is the other half: every failure mode here draws more, so
`sections_culled_occlusion == 0` on its own cannot distinguish an open surface from a graph that refused
to walk.

**Three levers, weakest first.** `TerrainOcclusion::Shadow` walks and reports
`sections_occlusion_shadow` without culling anything — the soak arm, and the first thing to switch to if
terrain ever vanishes, because it keeps the rest of the frame identical. `TerrainOcclusion::Off` is
frustum ∩ distance. `set_terrain_culling(false)` (vanilla's `smartCull`) is the big hammer and turns all
three culls off.

Counters land in `RenderStats` (`gpu/stats.rs`): `sections_drawn`, `sections_culled_distance`,
`sections_culled_frustum`, `sections_culled_occlusion`, `sections_occlusion_shadow`,
`occlusion_active` / `occlusion_graph_sections` / `occlusion_walks`, and `water_sections_drawn` /
`water_sections_culled`. The invariant closes **per pass**, not globally: a water-only section carries
`mesh: None`, so it never reaches `sections_drawn` while still issuing a draw (measured 189
`sections_drawn` against 195 uploads and 304 `draw_calls` before culling existed). A single combined
invariant reads as a cull bug on a perfectly healthy frame.

### The arena — `crates/lodestone-render/src/model_arena.rs`

Every live section mesh is suballocated out of shared **blocks** (a 32 MiB vertex arena paired with an
8 MiB index arena) instead of owning two `wgpu::Buffer`s. Blocks are appended on demand, never
released. A draw is then `set_bind_group` (dynamic origin offset) + `draw_indexed` — two encoder calls
instead of four, because `set_vertex_buffer`/`set_index_buffer` moved out of the per-section loop.

`gpu/frame.rs`'s `emit_terrain_draws` takes an **already-ordered** `Vec<TerrainDraw>` and binds each
block's buffer pair once. Collect-then-emit rather than one loop for two reasons: the cull runs exactly
once per section, and consecutive draws share the bind. The bind count is reported as
`RenderStats::terrain_buffer_binds` — low tens at any render distance, where it used to equal
`sections_drawn + water_sections_drawn`.

The two passes order that `Vec` **differently**, and `emit_terrain_draws` deliberately does not sort:

* **opaque** sorts by arena `block`, because depth sorts the pixels and grouping is free;
* **water** sorts back-to-front by section-centre distance (`terrain::sort_back_to_front`), because for
  a translucent pass the order *is* the output. Before this, `model.sections` being a `HashMap` meant
  water was submitted in **hash iteration order**, so two water surfaces overlapping along the view axis
  blended in whatever order the hasher produced — and that order changed when a chunk loaded rather than
  when the camera moved. It costs the block grouping for that pass, which is the trade culling bought
  room for: `water_sections_drawn` is now a small fraction of the resident set.

Vanilla's *intra*-section resort (`translucency.rs`'s `TranslucentMesh`/`SortViewpoint`, re-uploading one
section's index order on an octant change) is still **not** wired. Deliberately: within a section, water
quads are near-coplanar top faces in the overwhelming case, so cross-section order is the half that
produces the visible artefact — and re-uploading an index span that now lives inside a shared arena block
is its own unit.

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
* **Changing the occlusion graph** — the three traps, all now handled:
  * `walk_visible` **merges source faces on re-reach** (vanilla's
    `SectionOcclusionGraph.addNeighbors:342-344`). The earlier single-entry-face version over-culled —
    a section reachable through two faces but first visited through the wrong one lost its exits,
    which is terrain vanishing at specific angles. `walk_merges_source_faces_reached_by_a_second_path`
    pins it, in both axis arms, because which face is "first" depends on `Face::ALL`'s order.
  * `with_reachable(None)` degrades to frustum ∩ distance, which draws *more*, never less. So does an
    absent graph entry, and so does a stale cached set from a neighbouring cell. **Every failure mode
    here is silent by construction** — hence `occlusion_active`.
  * The graph must contain every *meshed* section including the geometry-less ones, and the walk must
    treat an absent in-bounds coord as air. See the occlusion section above; this is the one that
    degrades to pure frustum forever while looking healthy.
  * An **adjacent blocker is reachable and is drawn** — the wall you are looking at is the surface you
    see. A gate asserting "the section behind the wall is unreachable" must pick a section *two* steps
    out, or it fails on correct code (it did, while this was being written).
  * `classify` reports the **first** test that fires and the frustum runs before occlusion, so a gate
    asserting `CullVerdict::Occlusion` at every orientation asserts something false: looking away from
    the subject legitimately reports `Frustum`. Assert `!= Visible` everywhere and count the
    `Occlusion` verdicts, requiring at least one — otherwise the gate can pass while measuring the
    frustum. `tests/occlusion_angle_sweep.rs` is the shape.
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
* `RenderState::set_terrain_occlusion(TerrainOcclusion)` — `On` by default, and harmless before anything
  populates the graph (an empty graph produces no reachable set, which is the pre-U3 cull exactly).
  `Shadow` counts without culling; `Off` is frustum ∩ distance. `render_distance_chunks == 0` and
  `terrain_culling == false` both suppress the walk entirely, so the `smartCull`-off arm really is the
  pre-cull frame rather than the pre-cull frame plus a walk nobody reads.

## Dependencies

* `lodestone_render::camera::{Camera, Frustum}` — Gribb–Hartmann planes for the `[0,1]` depth
  convention.
* `lodestone_render::visibility` — the `VisGraph` port, the camera walk and its sparse-graph variant.
* `lodestone_render::cull::reachable_from_camera` — the walk's production bounds, in this crate rather
  than the shell so `tests/occlusion_angle_sweep.rs` exercises the real thing.
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
