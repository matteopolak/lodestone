# Terrain rendering

## What it is

Everything between "a chunk section changed" and "its quads are the right shape, in
the right place, drawn or correctly not drawn, on screen": meshing and mesh
invalidation as chunks stream in, frustum/distance/occlusion culling and the shared
GPU arena draws come from, the camera uniform every terrain pipeline reads, how
translucent and fluid surfaces are culled, ordered and depth-tested, and the pixel
diagnostics built to chase a recurring "sky shows through the blocks" class of
report.

## How it works

### Meshing and mesh invalidation

A section's mesh is a function of its whole 3×3×3 = 27-section neighbourhood, not
just itself: face culling reads the six orthogonal neighbours, ambient occlusion and
smooth-light corners read edges and corners too, and fluid corner heights/flow read a
3×3 ring. `crates/lodestone-shell/src/mesher.rs` therefore has to answer, per
neighbourhood slot, not just "what is there" but "is *nothing* the truth, or have I
just not been told yet":

```rust
pub enum ColumnSource { Complete, Streaming }         // is an absent column the edge of the world, or in flight?
pub enum Neighbour { Present(Arc<ChunkSection>), Air, Unloaded }
pub enum SnapshotOutcome { Ready(SectionSnapshot), Empty, Deferred(SectionSnapshot) }
```

`Air` means air is the truth (past the world edge, above/below the build limits, an
elided all-air section, or any absent column in a `Complete` world). `Unloaded` means
air is a *guess*; `snapshot_section_in` returns `Deferred` whenever any slot is
`Unloaded`. `TerrainMesh::route` submits a `Ready` mesh, queues a GPU removal for
`Empty`, submits a `Deferred` mesh anyway if that coord was already uploaded before
(vanilla's own "already compiled" exemption — otherwise the outer view ring blinks
out and edits at the frontier never show), and otherwise holds it back and bumps a
deferred counter without queuing a removal.

A column can also be absent for a *third* reason: it left the view. That is learned
from the unload signal, not the store (both look identical there). `TerrainMesh::
forget_column`/`force_neighbours_of_departed` push a departing column's still-loaded
neighbours into a forced re-mesh queue, gated on `all_absent_neighbours_departed` —
without that predicate, forcing every neighbour of a departure also drags in the
outermost buffer ring the server streams one column past the view, and a section
meshed against that ring's absence bakes its seam against air (the "blocky water
along chunk boundaries" report). `mark_neighbours_dirty` (on arrival) and this
departure path are the two invalidation mechanisms; between them an interior seam
heals within a frame or two of its neighbour changing, in either direction.

The heal queue is priority-ordered, not FIFO: `(Chebyshev distance from the player's
column, in-frustum penalty, cx, cz)`, re-keyed once a frame only when the player's
column or one of 16 quantised yaw sectors changes. Distance is primary (so a slow
spin cannot starve what's behind the player), the frustum penalty only reorders
within one distance band. `DIRTY_COLUMN_BUDGET` bounds how many columns are re-meshed
per frame.

### Culling and the section-visibility graph

`crates/lodestone-render/src/cull.rs`'s `TerrainCull` is built once per frame and
consulted by all three terrain loops (packed, live opaque, live water), so they can
never disagree about what exists. `classify(section_coord)` returns the first test
that fires, cheapest first: `Distance` (a rounded circle with a one-chunk buffer,
`max(0,|d|-1)² summed < rd²` — vanilla's own shape, not the streamed square),
`Frustum` (against a camera-cube-offset frustum, which prevents the section you're
standing in from flickering at cell boundaries), `Occlusion`, else `Visible`.

The occlusion graph is vanilla's cave culling, and the only one of the three that can
remove the underground while standing on the surface (frustum and distance keep it
all). Three parts: the mesh worker floods each section's non-opaque cells and records
which of the 15 face pairs connect; every *meshed* section (including ones with no
geometry — a sealed underground section is exactly the blocker that should stop the
walk) is inserted into a graph on `RenderState`; and a per-8-block-cell walk from the
camera, cached by `(camera cell, graph generation)`, decides reachability. The walk
must treat any coord absent from the graph as air (open), or it dies at the first
unmeshed air gap above terrain and quietly draws everything forever while looking
correct. `RenderStats::occlusion_graph_sections` must stay **≥** the drawn section
count for exactly this reason. Three escalating levers exist for diagnosing a
terrain-vanishing report: `TerrainOcclusion::Shadow` (walks and counts but culls
nothing — the soak test), `Off` (frustum ∩ distance only), and
`set_terrain_culling(false)` (the full `smartCull`-equivalent kill switch).

Every live section mesh is suballocated out of shared GPU arena blocks (32 MiB vertex
+ 8 MiB index) rather than owning its own buffer pair, so a draw is one dynamic-offset
bind plus `draw_indexed` instead of four separate calls. The opaque loop sorts draws
by arena block (cheap, since depth already sorts the pixels); the water loop sorts
back-to-front by section-centre distance, because for a translucent pass submission
order *is* the visible result.

### The shared camera uniform

Every terrain pipeline (`ModelPipeline`, and — since a later pass — `BlockPipeline`)
reads a group-0 binding split into a **shared** per-frame half (view-projection +
fog, written once) and a **per-section** half (world origin) living in one
dynamic-offset GPU arena, addressed at draw time instead of rewriting a whole camera
uniform per section per frame. Before this split, every resident section carried its
own camera buffer and bind group, and `render_inner` rewrote all of them — `view_proj`
included, though it is identical for every section every frame — with one
`queue.write_buffer` call each; at a few thousand resident sections that dominated
frame time. One bind group is now built once, over the shared buffer and the whole
origin arena, and every draw varies only the dynamic offset. Slot 0 of the arena is
permanently zeroed for passes whose geometry already bakes world position into its
vertices (dropped items, the first-person hand, mining cracks).

The packed/demo path mirrors the same shape with its own (much smaller) origin arena,
primarily so a future fog/sky-darken uniform has somewhere to live without re-opening
the old per-section-write cost, not because the demo world's own frame time mattered.

### Translucency: culling and depth

Two independent rules govern an interior face between two translucent blocks of the
same kind (glass, ice, honey, slime): a neighbour whose face fully occludes culls the
face (vanilla's ordinary occlusion-shape test — never true for these blocks, since
they're `noOcclusion()`), and a *same-block* skip (`skips_rendering_against`, keyed on
vanilla's `HalfTransparentBlock` identity list) removes the interior wall a stack of
identical translucent blocks would otherwise show. Separately, the translucent
terrain pipeline keeps ordinary back-face culling **on**, matching vanilla — with it
off, a solid cube's far face drew too, double-compositing the same partial alpha
along one view ray.

Depth precision for a thin overlay (a filled map over its wall, a sign's glowing
outline over its ink) comes from the projection, not a bias. This renderer's depth is
reversed `[0,1]` `Depth32Float` like vanilla's; the geometric separation a fixed
world-space clearance buys degrades only as `1/distance`, where the forward `[0,1]`
projection this replaced collapsed as `distance²`. A polygon-offset `constant` is a
**ULP count** at the primitive's own binade (so a roughly constant *relative*
separation under reversed-Z, not a fixed absolute one); `slope_scale` is unbounded and
grows with the fragment's depth slope, one to three orders of magnitude larger than
the constant term a few degrees off head-on — the two are never comparable by eye.
`CAMERA_DEPTH_BIAS = (constant: 10, slope_scale: 1.0)` is vanilla's own sign-text
constant, transcribed with no flip because both sides are reversed-Z; a map's board
overlay simply doubles the constant, keeping the slope term equal so the two cancel
regardless of viewing angle. The terrain pipelines themselves carry a plain zero
bias, so anything drawn over ordinary terrain has its whole configured bias as a real
advantage, not merely parity.

### Fluid classification

"Does this state carry water/lava" is one rule, `classify_fluid` in
`crates/lodestone-render/src/block_models.rs`, evaluated once per state at load time
and shared by the mesher (draws the surface) and physics (swim/fog/overlay/sounds) —
before it existed the two disagreed, so a player could stand inside rendered water,
unable to swim, with dry fog and sounds. It covers three cases a bare block-id match
can't: `minecraft:water`/`lava`'s own `level` property; **any** state with
`waterlogged=true`; and five classes with no blockstate property at all whose
`getFluidState` hardcodes a water source (`kelp`, `kelp_plant`, `seagrass`,
`tall_seagrass`, `bubble_column`). This is a different question from "given that a
cell carries water, what do we draw" — see fluid rendering below; the two were
conflated once in a shoreline-lighting report that turned out to be entirely about
the *bank* block's occlusion, with the classifier innocent throughout.

### Fluid rendering

Vanilla renders fluids outside the block-model pipeline entirely — their blockstate
models are empty, and the surface is built at mesh time from the cell's
neighbourhood (`FluidRenderer` in the 26.2 jar). This is split the same way: pure
math and UV/winding layout (`lodestone_assets::fluid`, knows nothing about the world)
versus the neighbourhood gather (`mesh_fluids`, backed live by
`SnapshotFluidView` in the shell's mesher, reading real state ids out of a 3×3×3
snapshot). The neighbourhood is resolved once per cell into an 18³ packed grid
(`FluidGrid`) rather than re-resolved per probe — the naive version cost roughly
14,000 instructions per fluid cell before this and the biome-tint lookup beside it
were both fixed.

**Corner heights are not four independent averages.** Vanilla short-circuits every
corner to `1.0` whenever the fluid's *own* rendered height is already `1.0` — which
only happens through `hasSameAbove` (a solid or same-fluid cell directly above), never
from a fluid's own amount alone. Averaging unconditionally instead (the historical
bug here) put a falling column a sixth of a block short on every cell and produced a
visible wedge, because a corner facing a wall excludes the solid neighbour from the
average while a corner facing air includes it — two different heights on one quad is
a sloped surface. `corner_heights` is the named function for the whole rule;
`corner_height` alone is only the averaging half.

Which face is emitted, straight from vanilla's `tesselate` (three genuinely different
predicates, not one applied per direction):

| face | condition |
|---|---|
| up | not same fluid above, and the fluid's own face isn't occluded (rarely — corners sit at 8/9, not 1.0, so water under stone still draws into the gap) |
| down | passes the shared `shouldRenderFace` test **and** isn't occluded by the block below |
| sides | passes `shouldRenderFace` **and** isn't occluded by that neighbour, at the taller of the two corner heights on that edge |

`shouldRenderFace` itself is "not the same fluid next door, **and** the fluid's own
containing block doesn't occlude that face" (`isFaceOccludedBySelf` — a
same-*cell* question, easy to miss because every other occlusion query on the trait
is about a neighbour; this is why a waterlogged stair's water face on the stair's own
solid side used to z-fight instead of simply not being drawn). Texture selection: a
level top surface samples the still sprite, a flowing one samples the flow sprite
rotated by the flow angle; sides sample a quarter of the flow sprite magnified 2× by
default (which is why a fluid side face reads as a waterfall — vanilla really does
draw that), or a dedicated overlay material with no back face against glass/ice/
leaves.

**Fluid occlusion against a neighbour depends on that neighbour's per-face occlusion,
not on a whole-block flag derived from its texture.** The original shoreline bug (a
pond drawing flowing side-faces against its own grass banks) was exactly this: a
block's occlusion is vanilla's own hand-set `Properties` flag, invisible in any data
report, and cannot be derived from "is every quad's sprite opaque" — a grass block
bakes ten quads (six cube faces plus four coplanar overlay decals) and inherited the
overlay decal's cutout classification for the whole block. The fix asks the question
per face: a face occludes when some quad's `cullface` is coplanar with and spans the
whole boundary square *and* that quad's own sprite is opaque. One block needs a veto
on top of that rule (`powder_snow`, whose model draws its own interior on thin
shells and would otherwise wrongly occlude blocks behind it).

Known, documented gaps: back-face culling is disabled on the fluid pipeline **and**
`bake_fluid` still emits vanilla's reversed-winding back-face copies (which exist
*because* vanilla's pipeline culls back faces) — with both true at once every open
fluid side face blends twice, at `1-(1-a)²` instead of `a`, reading more opaque than
vanilla's. The fix is restoring back-face culling, not removing the baked copies.
Partial-occluder culling (a `dirt_path`/farmland/slab bank against a fluid side face)
is closed for the scoped single-box, full-footprint case; the general multi-box case
(stairs, fences, walls) needs real voxel-shape slice-and-compare and remains
unmodelled, falling back to the coarser whole-block occlusion boolean. An animated
sprite never reaches the mip chain (it samples level 0 unconditionally), so a distant
animated block shimmers rather than resolving smoothly — left alone deliberately
rather than fixed alongside an unrelated change.

### Sky-holes diagnostics, atlas filtering and the alpha cutout

A recurring owner report — "the sky colour comes through the blocks" — produced a
family of pixel gates, each rendering the same camera twice (with and without
terrain) and classifying the diff against an independent ray cast through real block
data, at a different range/angle regime each time (far-flat, far-uneven, far-grazing,
near-grazing). Two confounds have to be neutralised before any of them can reach a
verdict: the section fade clock (an un-advanced clock renders every section as its
own fog colour, indistinguishable from sky) and ordinary render-distance fog (which
legitimately claims an annulus the oracle would otherwise call a hole).

For **ordinary opaque blocks** the near/far gates rule out missing geometry, all
three culls, the depth test, the cutout discard firing on an opaque sprite, atlas
gutter bleed, and fog. Two real defects were found this way instead:

- **The alpha-cutout threshold is per pipeline in vanilla and was one hardcoded value
  here.** Vanilla's solid terrain pipeline runs no alpha test at all, cutout terrain
  tests at `0.5`, translucent terrain at `0.1` — and stained glass sits mostly around
  `0.4` alpha, so testing it at `0.5` discarded roughly three quarters of every glass
  face. The fix is a pipeline-overridable shader constant bound per pipeline
  (`0.1` for `Translucent`, `0.5` otherwise); the combined opaque pass still has to
  take the *stricter* of solid/cutout since it carries both geometries in one mesh.
- **Render layer (solid/cutout/translucent) is per *quad* in vanilla, and was per
  *block state* here** — a block took the most-transparent layer across all its
  faces, so `grass_block`'s six opaque cube faces inherited an alpha test from its
  four coplanar overlay decals. Fixed by resolving layer per quad from that quad's
  own sampled sprite. Invisible on stock vanilla assets (no ordinary building block
  mixes opaque and cutout sprites in one state) but load-bearing the moment a
  resource pack does.

Texture filtering has two independently switchable axes, both read once per process
and both defaulting to vanilla: which sampling function the terrain shader takes
(`none` — a plain isotropic sample vanilla actually ships by default — versus `rgss`,
a supersampled mode this client shipped unconditionally for a while, which is
anisotropy-aware and therefore *undersamples* on a hardware sampler with no real
anisotropic filtering, aliasing a distant grazing surface into a visible lattice); and
the terrain atlas's magnification filter (`linear`, vanilla's default, versus
`nearest`). A separate, now-fixed defect at `mipmapLevels = 0` invented mip levels no
sprite could support with an unwritten (fully transparent) gutter — real, but
reachable only at that one slider position and not the cause of the pinprick reports.
What remains genuinely unported is anisotropic filtering itself (needs a real
`anisotropy_clamp` and a gutter that grows with it, doubling ours at vanilla's
default).

## How to change it, and the gotchas

- **Adding a cull** goes in `cull.rs` as a new `CullVerdict` variant plus a counter
  arm in `frame.rs`'s opaque loop — never a second predicate at a call site, or the
  three terrain loops can disagree again.
- **Adding a reason a neighbourhood slot can be empty** needs a new `Neighbour`
  variant, not a convention; that is the whole reason the type exists instead of an
  `Option`.
- **A section renders nothing where terrain should be** → check the deferred
  counter. A count that keeps climbing while nothing is loading means the
  arrival-driven invalidation has stopped re-driving a deferred section — the
  opposite failure direction from a stale seam, and the price of getting the healing
  half wrong.
- **There are two meshers.** `--headless`/the demo world go through `mesh_simple`
  (no fluid path, its own separate AO), live terrain through `mesh_models` +
  `mesh_fluids`. Anything asserted about water, biome tint or vanilla-style AO has to
  go through the live path.
- **A fluid face wrong at a boundary** — check `occludes_at`/the per-face occlusion
  table for the *neighbour* first, then `mesh_fluids`'s `emit` closure. But if the
  cell itself is waterlogged, ask about the cell's own occlusion first — reaching for
  the neighbourhood by reflex is how the self-occlusion rule went unported for as
  long as it did. A fluid defect confined to chunk boundaries is not a culling bug at
  all — that is the mesh-invalidation frontier above, not this file.
- **Do not compare a polygon offset's `constant` against its `slope_scale` by eye**;
  at any non-trivial angle the slope term dominates by orders of magnitude. Re-run
  the coplanar-depth survey before reasoning about either.
- **The block sizing constants for the arena** (`DEFAULT_VERTEX_BLOCK_BYTES`/
  `DEFAULT_INDEX_BLOCK_BYTES`) must keep the index block above 3/16 of the vertex
  block (4 vertices, 6 indices per quad) or vertex space strands; a unit test asserts
  the ratio.
- **Multi-draw indirect is not worth adding** — see `docs/architecture.md`; it is
  CPU-emulated as a per-draw loop on both this project's targets and saves nothing.

## Configuration

- `Config::render_distance` reaches `RenderState` every frame via `set_fog`;
  `render_distance_chunks == 0` disables the distance cull rather than culling
  everything (a default-constructed `RenderState` holds zero, and a cull that blanks
  a default state is indistinguishable from a broken renderer).
- `RenderState::set_terrain_culling(bool)` (on by default) and
  `set_terrain_occlusion(TerrainOcclusion)` (`On`/`Shadow`/`Off`).
- `RUST_LOG=terrain_cull=debug` — an edge-triggered live probe (camera pose, a 3×3
  section neighbourhood, aggregate cull counts). `LODESTONE_TERRAIN_CULL_PROBE_SECTION=x,y,z`
  pins the sampled section while the camera moves.
- `DIRTY_COLUMN_BUDGET` (mesher) — columns re-meshed per frame by the heal drain.
- `LODESTONE_TEXTURE_FILTERING=none|rgss`, `LODESTONE_TERRAIN_MAG_FILTER=linear|nearest`
  — the two sampling switches above; unrecognised values fall back to the default
  rather than failing to start.
- `LODESTONE_MAP_DISABLE_DEPTH*`, `LODESTONE_SIGN_OUTLINE_*`,
  `LODESTONE_SIGN_TEXT_LIFT_PROBE=<blocks>` — native-only diagnostics for isolating a
  coplanar-overlay depth artefact to geometry, depth test, depth write, or bias; see
  the coplanar-overlay-depth survey test for what each removes.

## Dependencies

- `lodestone_render::camera::{Camera, Frustum}` — Gribb–Hartmann planes for the
  `[0,1]` depth convention.
- `lodestone_render::visibility`/`cull::reachable_from_camera` — the occlusion walk,
  living in `lodestone-render` rather than the shell so its own gate exercises the
  real thing.
- `lodestone_render::arena`/`suballoc` — the GPU-backed vertex/index arena and its
  address-ordered first-fit allocator.
- `lodestone_assets::fluid`, `lodestone_data::outline_shapes` (the real per-state
  jar-dumped **outline** geometry the partial-occluder fix needs — not
  `collision_shapes`, which disagrees for roughly half of all states) and
  `lodestone_data::shade_brightness`.
- `lodestone-shell`'s `mesher.rs` — `SnapshotModelView`/`SnapshotFluidView`, the live
  implementors of every trait this doc describes.
