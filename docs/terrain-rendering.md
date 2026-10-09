# Terrain rendering

## What it is

Everything between "a chunk section changed" and "its quads are the right shape, in the right place, drawn or correctly culled": meshing and invalidation as chunks stream in, culling, the shared GPU arena draws come from, the camera uniform, translucent and fluid ordering and depth, and the pixel diagnostics built to chase "sky shows through the blocks" reports.

## How it works

### Frame structure

Each frame classifies a resident model section once, then reuses borrowed visible sections for the opaque, water and translucent passes (never kept across a redraw or upload/removal). Opaque draws sort by arena block; water and translucent sort independently by section-centre distance, farthest first.

`RenderState::render_inner` keeps one world pass open from opaque terrain through water, translucent, particles, weather and overlays when there is no sign/display text. Non-empty text with a compatible target closes the pass, draws on the raw colour view, then resumes with colour and depth loaded. Nametags are a separate final pass and the first-person hand keeps its own depth clear. Change pass ownership only there; keep text before tail geometry and reset `terrain_cam_group_last` only when a new world pass begins. The GPU `world` timestamp interval runs from the first world pass to the last world or nametag pass; `world_pass_begins`, `world_text_pass_begins` and `nametag_pass_begins` count real begins.

`RenderState::mesh_storage_bytes` reports occupied (arena live bytes) and reserved (arena capacity) terrain bytes in one walk, tied to residency, not the visible draw set.

### Meshing and invalidation

A section's mesh depends on its whole 3x3x3 neighbourhood (face culling, AO and smooth light, fluid corners). `crates/lodestone-shell/src/mesher.rs` therefore distinguishes, per slot, "nothing is the truth" from "not told yet":

```rust
pub enum ColumnSource { Complete, Streaming }
pub enum Neighbour { Present(Arc<ChunkSection>), Air, Unloaded }
pub enum SnapshotOutcome { Ready(SectionSnapshot), Empty, Deferred(SectionSnapshot) }
```

- `Air` is true air (past the world edge, build limits, an elided section, any absent column in a `Complete` world). `Unloaded` is a guess; any `Unloaded` slot makes `snapshot_section_in` return `Deferred`.
- `TerrainMesh::route` submits `Ready`, queues GPU removal for `Empty`, submits `Deferred` only if the coord was already uploaded (else the outer ring blinks out and frontier edits never show), and otherwise holds it back and bumps a deferred counter without queuing removal.
- A column can also be absent because it left the view, learned from the unload signal. `forget_column`/`force_neighbours_of_departed` force a re-mesh of its loaded neighbours, gated on `all_absent_neighbours_departed`; without the gate the server's outermost buffer ring drags in and seams bake against air (blocky water along chunk borders). `mark_neighbours_dirty` (arrival) and this departure path are the two invalidation mechanisms.
- Column eviction invalidates every submitted section key for that coordinate; native workers may finish an invalidated job but its generation is stale and the result is discarded. Browser teardown clears queues without meshing; native teardown flushes.
- The integrated client requests two rings beyond the visible radius (the first for the 3x3 dependency, the second so it stays resident across a crossing); multiplayer requests one.
- The heal queue is priority-ordered by `(Chebyshev distance from the player's column, in-frustum penalty, cx, cz)`, re-keyed once a frame only when the column or one of 16 quantised yaw sectors changes. Distance dominates so a slow spin cannot starve what is behind. `DIRTY_COLUMN_BUDGET` bounds columns re-meshed per frame.

**Empty-column diagnostics.** The network path validates a column before writing it to the client `World`; `ChunkLoaded` carries only the position. `ChunkColumn` elides all-air sections but may keep a biome-only one, so `TerrainMesh::mesh_column_inner` uses `ColumnBlockSummary` to count real non-air blocks. A column with none is intentionally empty: removals queue but `TerrainMesh::drops` does not increment. If every snapshot is `Empty` while the summary has non-air blocks, the input is inconsistent: `drops` increments and a warning (sampled at power-of-two occurrences) reports coordinates and counts. A missing column or a `Deferred` snapshot returns before this check. Extend `ColumnBlockSummary` and its controls together when storage changes.

### Culling and the visibility graph

`lodestone-render`'s `cull.rs` `TerrainCull` is built once per frame and used by all three terrain loops so they cannot disagree. `classify(section_coord)` returns the first test that fires, cheapest first: `Distance` (rounded circle with a one-chunk buffer, `max(0,|d|-1)^2` summed below `rd^2`), `Frustum` (camera-cube-offset so the section you stand in does not flicker), `Occlusion`, else `Visible`.

Occlusion is cave culling and the only test that removes underground while standing on the surface. The mesh worker floods each section's non-opaque cells and records which of 15 face pairs connect; every meshed section (including empty sealed ones) enters a graph on `RenderState`; a per-8-block-cell walk from the camera, cached by `(camera cell, graph generation)`, decides reachability. The walk must treat coords absent from the graph as open air, or it dies at the first unmeshed gap and silently draws everything. `RenderStats::occlusion_graph_sections` must stay at least the drawn count.

Diagnostic levers for a vanishing-terrain report: `TerrainOcclusion::Shadow` (walk and count, cull nothing), `Off` (frustum and distance only), `set_terrain_culling(false)` (full kill switch). `occlusion_walk=debug` times cache misses.

Section meshes are suballocated from shared GPU arena blocks (32 MiB vertex plus 8 MiB index); consecutive draws from one block share bindings.

### Camera uniform and section origins

Camera matrices and fog share one per-frame uniform; origins are written only on upload. Model terrain reads the origin arena as a padded instance vertex stream: each non-empty layer binds camera and origin once and selects a section with `first_instance = origin_offset / origin_stride` (the arena allocation alignment, not the 16-byte payload). The origin's fourth lane holds the section fade start. Slot 0 is reserved for geometry already in world or camera coordinates. Packed terrain has its own smaller arena and dynamic-offset origins, as do devices whose vertex limits cannot fit the stream; base-vertex support is required.

To change the representation, update `ModelPipeline`'s terrain constructors, the shared model/fluid vertex helpers, `SectionOriginArena` and `emit_terrain_draws` together. The sparse-origin pixel control covers translation, fade, composition and dedicated buffers. `terrain_camera_bind_calls`, `terrain_origin_vertex_binds`, `terrain_indexed_draw_calls` and `terrain_buffer_bind_pairs` count real encoder calls (the older group-switch counter tracks object identity only).

### Mining-crack sampling

`CrackPipeline` shares the atlas texture and mips but not the terrain sampler: clamped, nearest magnification (discrete crack texels), linear minification. It receives only a `TextureView` via `CrackPipeline::atlas_bind_group`, which keeps terrain's magnification from blurring the overlay. Change it in `crack_sampler_descriptor` (and its unit test) in `lodestone-render::crack_pipeline`.

### Translucency

- An interior face between two translucent blocks of the same kind (glass, ice, honey, slime) is removed by a same-block skip (`skips_rendering_against`), since these blocks never occlude by shape. The translucent pipeline keeps back-face culling on; without it a cube's far face double-composites partial alpha.
- Slime and honey have an outer cube plus a nested inner cube of unculled faces. An isolated block keeps all six inner faces; against an identical neighbour the shared inner pair is suppressed too. The check covers only complete rectangles strictly inside the block, so diagonal blades and portal panels stay visible.
- The translucent block-model pipeline writes depth with a nearer-or-equal comparison, so a nearer sheet rejects a later farther face. Draw order still matters for two visible layers (farther first). Fluids differ: alpha-blended water keeps depth writes off so the seabed shows, relying on an explicit reverse-winding copy and back-to-front section order.
- Thin overlay depth (a map over its wall, sign glow over ink) comes from the projection, not bias. Depth is reversed `[0,1]` `Depth32Float`; separation from a fixed clearance degrades as `1/distance`. A polygon-offset `constant` is a ULP count at the primitive's own binade; `slope_scale` grows with depth slope and dominates by orders of magnitude a few degrees off head-on, so never compare them by eye. `CAMERA_DEPTH_BIAS = (constant: 10, slope_scale: 1.0)` is used for sign text; a map board doubles the constant and keeps the slope equal. Terrain pipelines carry zero bias, so any overlay's whole bias is a real advantage.

### Fluid classification

`classify_fluid` in `crates/lodestone-render/src/block_models.rs` is the single "does this state carry water or lava" rule, evaluated once per state at load and shared by the mesher and physics (swim, fog, overlay, sounds); when they disagreed a player could stand in rendered water, unable to swim. It covers the fluids' own `level` property, any `waterlogged=true` state, and five classes with no property that hold a water source (`kelp`, `kelp_plant`, `seagrass`, `tall_seagrass`, `bubble_column`). It answers a different question from what to draw.

State-keyed `BlockModels` lookups take `lodestone_data::block_states::StateId`. The snapshot view validates canonical values once at ingress; out-of-census or protocol-local values stay unresolved and take the empty/open fallback instead of an unrelated model.

### Fluid rendering

Fluids bypass the block-model pipeline (their blockstate models are empty); the surface is built at mesh time from the neighbourhood. Pure math and UV/winding layout live in `lodestone_assets::fluid`; the gather is `mesh_fluids`, backed by `SnapshotFluidView` over a 3x3x3 snapshot. The neighbourhood resolves once per cell into an 18^3 packed grid (`FluidGrid`) rather than per probe.

- **Corner heights are not four independent averages.** Every corner is 1.0 whenever the fluid's own rendered height is already 1.0, which happens only when a solid or same-fluid cell is directly above. Averaging unconditionally leaves a falling column a sixth short and a visible wedge. `corner_heights` is the whole rule; `corner_height` is only the averaging half.
- Face emission uses three different predicates:

| face | condition |
|---|---|
| up | not same fluid above, and the fluid's own face is not occluded (corners sit at 8/9, so water under stone still draws into the gap) |
| down | passes the shared render-face test and the block below does not occlude it |
| sides | passes the render-face test, the neighbour does not occlude it, drawn at the taller of the two corner heights on that edge |

- The render-face test is "not the same fluid next door, and the fluid's own containing block does not occlude that face". The own-cell part is easy to miss and is why a waterlogged stair's water used to z-fight on its solid side.
- Textures: a level top uses the still sprite, a flowing one the flow sprite rotated by flow angle; sides use a quarter of the flow sprite magnified 2x (so they read as waterfalls), or an overlay material with no back face against glass, ice and leaves.
- **Neighbour occlusion is per face, not a whole-block flag.** A block's occlusion is a hand-set property invisible in data reports and cannot be derived from "every quad's sprite is opaque" (a grass block bakes ten quads including four coplanar overlay decals). A face occludes when some quad's `cullface` is coplanar and spans the whole boundary square and that quad's sprite is opaque. `powder_snow` needs a veto, since its model draws its interior on thin shells.
- The fluid pipeline keeps back-face culling on and `bake_fluid` emits reversed-winding copies. Disabling culling would blend both copies along one ray, turning alpha `a` into `1-(1-a)^2`. `fluid_gate::reverse_copy_is_not_a_second_layer` guards this against bright and dark backgrounds with a two-layer control.
- Partial-occluder culling (path, farmland or slab bank against a fluid side) works for scoped single-box full-footprint cases; multi-box shapes (stairs, fences, walls) fall back to the coarse boolean. An animated sprite samples mip level 0 unconditionally, so a distant animated block shimmers.

### Sky-holes diagnostics, filtering and alpha cutout

A recurring "sky colour comes through the blocks" report produced pixel gates that render the same camera with and without terrain and classify the diff against an independent ray cast through real block data, at far-flat, far-uneven, far-grazing and near-grazing regimes. Two confounds must be neutralised first: the section fade clock (un-advanced, every section renders as fog colour) and legitimate render-distance fog. These gates ruled out missing geometry, all three culls, depth test, cutout discard on opaque sprites, atlas gutter bleed and fog, and found two real defects:

- **Alpha-cutout threshold is per pipeline.** Solid terrain runs no alpha test, cutout tests at 0.5, translucent at 0.1; stained glass sits near 0.4 alpha, so one hardcoded 0.5 discarded about three quarters of every glass face. It is now a pipeline-overridable shader constant (0.1 for `Translucent`, 0.5 otherwise); the combined opaque pass takes the stricter of solid and cutout.
- **Render layer is per quad, not per block state.** A block that took the most transparent layer across its faces gave `grass_block`'s six opaque faces an alpha test from its four overlay decals. Layer is now resolved per quad from its own sprite. Invisible on stock assets but load-bearing once a resource pack mixes sprites.

Texture filtering has two switches, each read once per process. The terrain shader's sampling is `none` (plain isotropic, the default) or `rgss` (supersampled, anisotropy-aware, which undersamples on a hardware sampler without real anisotropic filtering and aliases grazing surfaces into a lattice). Terrain magnification is `nearest` (default) or diagnostic `linear`. Anisotropic filtering itself is unported (needs an `anisotropy_clamp` and a gutter that grows with it).

## How to change it

- **Add a cull** as a `CullVerdict` variant in `cull.rs` plus a counter arm in `frame.rs`'s opaque loop, never a second predicate at a call site.
- **Add a reason a neighbourhood slot can be empty** as a new `Neighbour` variant, not a convention.
- **A section renders nothing where terrain should be:** check the deferred counter. One that keeps climbing while nothing loads means arrival-driven invalidation stopped re-driving a deferred section.
- **There are two meshers.** `--headless` and the demo world use `mesh_simple` (no fluid path, separate AO); live terrain uses `mesh_models` plus `mesh_fluids`. Assert water, biome tint or vanilla-style AO only through the live path.
- **A fluid face wrong at a boundary:** check the neighbour's per-face occlusion (`occludes_at`) then `mesh_fluids`'s `emit` closure; if the cell is waterlogged, ask about its own occlusion first. A defect confined to chunk boundaries is the invalidation frontier, not culling.
- Keep `DEFAULT_INDEX_BLOCK_BYTES` above 3/16 of `DEFAULT_VERTEX_BLOCK_BYTES` (4 vertices, 6 indices per quad) or vertex space strands; a unit test asserts the ratio.
- Multi-draw indirect is not worth adding; it is CPU-emulated per draw on both targets (see `docs/architecture.md`).

## Configuration

- `config::MIN_RENDER_DISTANCE..=MAX_RENDER_DISTANCE` is the one typed range for `Options::render_distance`, CLI validation and the Video slider (maximum 256; out-of-range persisted values use `DEFAULT_RENDER_DISTANCE`). `Config::render_distance` reaches `RenderState` each frame via `set_fog`; 0 disables the distance cull (a default `RenderState` holds zero, and a cull that blanks it looks like a broken renderer).
- `RenderState::set_terrain_culling(bool)` and `set_terrain_occlusion(TerrainOcclusion)` (`On`/`Shadow`/`Off`).
- `RUST_LOG=terrain_cull=debug` is an edge-triggered probe; `LODESTONE_TERRAIN_CULL_PROBE_SECTION=x,y,z` pins the sampled section.
- `DIRTY_COLUMN_BUDGET`: columns re-meshed per frame.
- `LODESTONE_TEXTURE_FILTERING=none|rgss` and `LODESTONE_TERRAIN_MAG_FILTER=linear|nearest`; unrecognised values fall back to the default.
- `LODESTONE_MAP_DISABLE_DEPTH*`, `LODESTONE_SIGN_OUTLINE_*`, `LODESTONE_SIGN_TEXT_LIFT_PROBE=<blocks>`: native diagnostics isolating a coplanar-overlay artefact to geometry, depth test, depth write or bias.

## Dependencies

- `lodestone_render::camera::{Camera, Frustum}` (Gribb-Hartmann planes for `[0,1]` depth), `visibility`/`cull::reachable_from_camera` (in the render crate so its gate exercises the real walk), `arena`/`suballoc` (address-ordered first-fit).
- `lodestone_assets::fluid`; `lodestone_data::outline_shapes` (per-state outline geometry for the partial-occluder fix, not `collision_shapes`, which disagrees for about half of states); `lodestone_data::shade_brightness`.
- `lodestone-shell`'s `mesher.rs`: `SnapshotModelView`/`SnapshotFluidView`, plus the version adapters, `WorldSink` and `lodestone-world` for ingestion and section counts.
