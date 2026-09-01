# Terrain sky holes

## What it is

The record of a recurring owner report — *"the sky colour comes through the
blocks"* — and of the four pixel gates built to reproduce it. Each gate owns a
different regime (far and flat, far and uneven, far and grazing, **near** and
grazing), and each has ruled out a different set of causes. This page exists so
the next reader can tell which regime is already answered from which is not,
without re-running four twenty-minute suites to find out.

## How it works

Every gate here shares one shape: render the same camera twice, once with
terrain uploaded and once without, classify each pixel as *painted* or
*background* by diffing the two, and then decide whether a background pixel is
legitimate using something that is **not** the renderer. What differs between
them is the world, the range, and what "not the renderer" means.

| gate | world | range | verdict |
|---|---|---|---|
| `distant_flat_terrain_holes` | flat floor | 10 chunks | clean |
| `uneven_terrain_holes` | stepped ziggurat | 10 chunks | 213/215 legal, **2 residual** |
| `far_grazing_ceiling_floor_holes` | floor + ceiling | 24 chunks, pitch to 1° | clean |
| `near_grazing_face_coverage_pixels` | floor + a wall + floating blocks | **20 blocks, eye level, 82°** | clean |
| `translucent_alpha_cutout_pixels` | a stained-glass wall and a stone control | 20 blocks | **found a defect** |

All five are `#[ignore]`d, need a GPU adapter and the vanilla `client.jar`, and
fail closed rather than skipping.

### The three far gates

The first three answer *"is a pixel that shows background legitimately
background"* with an independent ray cast — a direction transcribed from
`Camera::basis`'s closed form, marched through `World::block_state_at` by an
Amanatides-Woo voxel walk, with `lodestone_render::cull::within_view_distance`
deciding whether a hit was in range. No mesher, no rasteriser, no shader.

Two confounds had to be removed before any of them could reach a verdict, and
both are worth knowing because they present as renderer bugs:

* **The section fade clock.** `model.wgsl` mixes a fragment toward the fog
  colour by `section_visibility(now, build_time)`, and at `visibility == 0` the
  output **is** the fog colour byte-for-byte — indistinguishable from the sky by
  construction. A harness that uploads and renders without calling
  `RenderState::update_animation` leaves `now == build_time == 0` forever, so
  every section renders as sky. The fix is to advance the clock past the
  `0.75` s fade window before rendering.
* **Fog.** At 24 chunks the render-distance ramp runs 345.6 to 384 blocks while
  `within_view_distance` calls a chunk in range out to 419 blocks Euclidean, so
  there is a permanent annulus where the oracle says "resident geometry" and the
  renderer correctly says "fog colour". `far_grazing_ceiling_floor_holes`
  neutralises fog rather than widening an exculpation, and the swap moved every
  raw count down by *precisely* the number of pixels the oracle had called
  genuine, in all five non-zero configs.

`uneven_terrain_holes`'s two residual pixels are the only unexplained result in
the far set, and they are not a logic bug: at ~170 blocks and a grazing pitch,
one whole 16-block chunk of world depth projects to under one screen pixel of
height, which a single-sample rasteriser can legitimately miss. Adding 4x MSAA
made the aggregate *worse*, so a coverage fix is not the answer either.

### The near gate

`near_grazing_face_coverage_pixels` exists because none of the three above can
speak to the sharper form of the report — *"at the same level that I'm standing
but just 20ish blocks away … sometimes only 60% of the block"*. At 20 blocks no
fog term is anywhere near onset, and "only 60%" is a **magnitude** claim that a
presence test satisfies at 60% exactly as it satisfies at 100%.

Two things about it are worth copying rather than re-deriving:

* **Near and grazing are in tension on a plane.** A planar surface seen at
  incidence `θ` from perpendicular offset `p` is `p / cos θ` away, so a grazing
  view of something 20 blocks off needs the surface to pass within a few blocks
  of the eye. The fixture's wall sits 2.5 blocks to the side, which puts its
  vertical face at **82°** where it is 20 blocks out, and its top face 0.62
  blocks below eye level — 1.8°, the report's own "same level as me".
* **Do not erode the silhouette away.** The first version of the gate cast one
  ray per pixel and eroded the mask by one pixel, so a half-pixel disagreement
  between a centre sample and the rasteriser's coverage rule could not produce a
  false failure. That is sound and it is also blind to the report: a face at 82°
  is *mostly* silhouette, so erosion asserts only on the part of the image least
  able to fail. The shipped version re-casts every non-uniform pixel on a 4x4
  sub-grid for its sub-pixel coverage and compares **painted area against oracle
  area**.

Measured, over `stone` and `grass_block`, six cameras, six surfaces each:

| surface | painted / oracle area |
|---|---|
| wall side face, 82° incidence at 20 blocks | 1.0000 |
| floating blocks straddling eye level, 15–30 blocks | 0.9798 – 0.9882 |
| floor, 10–20 / 20–30 / 30–45 / 45–60 blocks | 1.0000 |

The floaters' 2% is their own outline: painted area is counted per whole pixel
while oracle area is sub-pixel, and each floater is a single block whose
perimeter is a large fraction of its area. Its bounding box is a thin horizontal
band across those blocks' own rows and its contiguity is 0.97–1.00.

The control skips one section's upload and fires at **0.712** on the floaters
and 0.9914 on the wall, both at **contiguity 1.000** — which is what calibrates
the shape statistic: a lost *region* reads as 1.0, so a low contiguity in a
failing run would mean scattered per-fragment loss rather than missing geometry.

## What is ruled out, and what is not

For **full-cube opaque blocks** at the report's own conditions, the near gate
rules out missing geometry, all three culls, the depth test, `model.wgsl`'s
cutout discard firing on an opaque sprite, atlas gutter bleed, and fog.

What it does **not** cover, in the order worth trying next:

1. **Cutout blocks at *far* range.** Near range is covered and clean: `oak_leaves`
   in FANCY mode, two blocks tall, straddling eye level and silhouetted against
   the sky, measured **0.995 / 1.000 / 1.011 / 1.037 / 0.995** of a 4x-supersampled
   render of the same camera at 15 / 20 / 25 / 30 / 35 blocks — indistinguishable
   from a `stone` control in the same fixture (0.996 / 0.999 / 1.010 / 1.035 /
   0.993). What is *not* covered is minification proper:
   `cutout_minification_flicker_pixels` measures a painted ratio of **0.62–0.66**
   in its second-most-minified band on a ground plate, and records it as an open
   residual. That is the only number in this repo close to the report's
   "only 60%", but the measurement above says it does not reach 20 blocks.
2. **Non-cube models** — stairs, slabs, walls. The oracle above assumes a hit
   cell is a full cube, so extending it means teaching it the bake.
3. **World coordinates far from the origin.** Every gate here, and every other
   terrain gate in this corpus, spawns at chunk `(0, 0)`. `model.wgsl`'s
   `vs_main` computes `world = position + origin.section_origin.xyz` in `f32`
   and then multiplies by an `f32` `view_proj`, so the shared spawn point is
   exactly the value at which that arithmetic is exact.
4. **Live-session-only state** — a resource-pack reload, arena slot recycling,
   the section fade during real streaming. Every gate builds its `RenderState`
   once and renders one frame.

## The opaque discard cannot fire on a vanilla opaque sprite — measured, not argued

The safety argument below (*"a solid sprite's filtered alpha cannot reach the
threshold from any direction"*) is a claim about **data**, so it is checkable
against the data rather than reasoned about. Every `assets/minecraft/textures/
block/*.png` in the 26.2 jar — 1,269 sprites — was decoded and its alpha
histogram taken:

| class | count | consequence |
|---|---|---|
| any texel `0 < a < 255` | 49 | `RenderLayer::Translucent` — glass, ice, water, the crack stages, `nether_portal`, `slime_block`, `honey_block_top`, `tripwire` |
| else any texel `a == 0` | 309 | `RenderLayer::Cutout` — plants, bars, doors, saplings, `dirt_path_side`, `glass_pane_top` |
| every texel `a == 255` | 911 | `RenderLayer::Solid` |

**No ordinary building block is in either of the first two rows.** `stone`,
`andesite`, `polished_andesite`, `smooth_stone`, `stone_bricks`, `cobblestone`,
`gravel` and the rest are all-255, at every texel. Combined with
`AtlasBuilder`'s gutter — `1 << mip_levels` texels wide, re-extruded from the
sprite's own edge at *every* level, so at the deepest level a 16×16 sprite is
one texel with one texel of its own colour on each side and a bilinear tap can
reach 0.5 texels — an opaque road surface's sampled alpha is `1.0` on every
path `model.wgsl` can take, at every mip level, from any direction. The
discard structurally cannot fire on it.

So for a report of *sky pinpricks on an ordinary stone or andesite surface*,
the cutout discard is excluded, and so is atlas gutter bleed. The one thing
that reopens it is a **server resource pack**: a replaced texture with stray
transparent texels is reclassified by `RenderLayer::from_sprite_alpha` and then
genuinely is alpha-tested. That is worth asking about before re-opening this
line of enquiry, because none of the gates above load a pack.

Two other candidates for the same report were closed the same way, by reading
an outside source rather than by running a gate:

* **The sampling port is byte-faithful.** `model.wgsl`'s `sample_nearest` and
  `sample_rgss` were diffed clause by clause against `terrain.fsh` in the jar,
  including the rotated-grid offsets, the `smoothstep(minPixelSize,
  minPixelSize * 2, maxTexelSize)` cross-fade, and the **geometric-mean**
  `effectiveDerivative` LOD (`sqrt(min * max)`) — which is vanilla's own, not
  an invention of ours. The only deliberate divergence is that we take
  `sampleRGSS` unconditionally where vanilla's shipped `textureFiltering`
  default selects `sampleNearest`.
* **Adjacent full cubes share bit-identical edges.** Positions are `f32`,
  never quantised, on an exact multiple of `1/16`; the section origin is an
  exact multiple of `16`; `world = position + section_origin` is exact for
  every coordinate a player reaches below ~524,288 blocks; and two quads
  meeting at a block boundary therefore feed identical operands to identical
  arithmetic and produce identical clip coordinates. There is no greedy merge,
  no T-junction handling and no positional inset on the live path, so there is
  no mechanism for a rasterisation crack between two full cubes.

What that leaves of the report is the list at the end of the previous section,
with one item promoted: **every gate in this corpus builds a world out of a
single block type**, and the owner's report says the artefact *"can get pretty
egregious when the blocks aren't all the same"*. A mixed-block world is the
discriminating fixture nothing here has, in the same way chunk `(0, 0)` is the
shared spawn point nothing here varies.

## The alpha test is per pipeline, and we had one value for all three

Found by clause-diffing `terrain.fsh` against `model.wgsl` rather than by any
of the gates above, and it is a real, shipped defect.

`terrain.fsh`'s discard is `#ifdef ALPHA_CUTOUT`, and `RenderPipelines` hands
the three terrain pipelines three different answers:

| vanilla pipeline | `ALPHA_CUTOUT` |
|---|---|
| `SOLID_TERRAIN` | not defined — **no test at all** |
| `CUTOUT_TERRAIN` | `0.5` |
| `TRANSLUCENT_TERRAIN` | `0.1` |

`model.wgsl` hardcoded `0.5` for every pass. Correct for cutout; five times too
strict for translucent, and real translucent block textures sit squarely in the
gap. Read straight out of the 26.2 jar, `block/white_stained_glass.png` carries
exactly three distinct alpha values — `102`, `155`, `163` — and **191 of its 256
texels are `102`**, i.e. `0.400`. So three quarters of every stained-glass face
was discarded and whatever was behind it painted instead: for glass silhouetted
against the sky, that is the sky, in a scattered per-texel pattern.

The fix is a pipeline-overridable constant — `override alpha_cutout: f32 = 0.5;`
in `model.wgsl`, bound per pipeline through `PipelineCompilationOptions`, which
is exactly what `withShaderDefine("ALPHA_CUTOUT", ..)` is on vanilla's side.
`ModelPipeline::for_layer` binds `0.1` for `Translucent` and `0.5` otherwise.
`translucent_alpha_cutout_pixels` measures it, over 12,110 px of oracle area:
**0.2423** painted before, **0.9991** after, with the stone control wall in the
same frame at **0.9992 in both runs** — four decimal places of "the control did
not move", which is what localises the change to the translucent pass rather
than to the gate. The before-figure was observed by running the neuter, not
constructed afterwards, and it is near what the texture's histogram predicts
unaided (65 of 256 texels clear `0.5`, i.e. 0.254; the mip chain accounts for
the rest by averaging `102` in with its neighbours at deeper levels).

The **solid** row stays as vanilla is not: our opaque pass carries solid and
cutout geometry in one mesh, so it must take the stricter of the two. The
section above measures the two facts that make that harmless rather than merely
tolerable, against the real jar. It is the thing that would bite first if
either of them changed — and a server resource pack is enough to change the
first one.

There is a second half to that divergence, and it is the durable fix if this
ever needs one. Vanilla's layer is **per quad**, not per block:
`SectionCompiler` buckets on `quad.materialInfo().layer()`, which
`ChunkSectionLayer.byTransparency` derives from that quad's own sprite. Ours is
one `RenderLayer` per block state, documented in `block_models.rs` as *"the most
transparent layer across its faces"* — so a `grass_block`'s fully opaque top and
bottom quads inherit `Cutout` from `grass_block_side_overlay` and get an alpha
test vanilla never applies to them. `ModelVertex::cutout_bypass` already exists
and already skips the discard (it carries vanilla's FAST leaves today), and
`BakedQuad` already carries a sprite index, so reproducing vanilla's split is a
matter of stamping that byte per quad from the quad's own sprite layer rather
than adding a pipeline or a mesh. It was deliberately **not** done here: with
vanilla assets it is provably a no-op (see the histogram above), and it touches
`models.rs`/`block_models.rs`/`mesher.rs`, which is not a change to make on a
hypothesis.

## One divergence left, deliberately unfixed

**An animated sprite never reaches the mip chain.** `fs_main`'s `anim_idx != 0`
branch replaces the `sample_rgss` result with two `textureSampleLevel` taps at
level `0.0`, so an animated block gets neither mipmapping nor supersampling at
any distance, while `terrain.fsh` has one sampling path for every sprite and
lets vanilla's per-frame atlas upload carry the animation. The symptom is
shimmer on distant animated blocks, not a hole, so it is recorded here rather
than changed alongside an unrelated fix.

## How to change it

* **The gates are cheap to extend by *class*, not by config.** The near gate
  classifies each oracle hit into a named surface (`CLASSES`) and reports each
  separately; an aggregate over the whole frame is dominated by the floor under
  the camera's nose and would bury a 60%-covered grazing face in six figures of
  healthy pixels. Add a class, not another camera.
* **Never believe a clean run without reading the control's numbers**, and never
  reach for a depth bias — see `docs/ground-plate-rendering.md` for the ULP
  table showing the depth buffer resolves everything in this range, and
  `CLAUDE.md` for why a bias tuned at one distance fails at another under this
  projection.

## Configuration

* `mipmapLevels` reaches the model atlas through
  `BlockModels::build_with_mip_levels`, which also sets the stitcher's gutter to
  `1 << levels`. `block_texture_gate`'s
  `the_live_mipmap_levels_setting_changes_the_built_atlas_mip_count` is what
  proves the setting reaches the atlas terrain is actually sampled from.
* `cutoutLeaves` reaches the mesher as `mesh_snapshot_models`'s third argument
  and decides whether leaves carry `ModelVertex::cutout_bypass`.

## Dependencies

* `lodestone-render` — `cull.rs` (`within_view_distance`), `model_pipeline.rs`,
  `block_models.rs`, `shaders/model.wgsl`.
* `lodestone-shell` — `mesher.rs` (`snapshot_section`, `mesh_snapshot_models`,
  `snapshot_visibility`) and `gpu` (`RenderState::upload_section`, `render`).
* `lodestone-world` — `World::block_state_at`, which the oracle reads directly.
