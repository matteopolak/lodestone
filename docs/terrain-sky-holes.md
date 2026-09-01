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
| `per_quad_render_layer` | one `grass_block`, one `stone`, one stained glass | meshing only | **found a defect** |

All six are `#[ignore]`d and fail closed rather than skipping. All need the
vanilla `client.jar`; the five pixel gates additionally need a GPU adapter,
while `per_quad_render_layer` stops at the mesh and does not.

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
   `cutout_minification_flicker_pixels` measured a painted ratio of **0.62–0.66**
   in its second-most-minified band on a ground plate and recorded it as an open
   residual. Most of that is now explained and closed — see *Atlas level 0
   carried the raw PNG* below, which moves that band to **0.871** — and what is
   left is the most-minified band at 0.607 against a 4x reference. That was the
   only number in this repo close to the report's "only 60%", and the
   measurement above says it does not reach 20 blocks either way.
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
  an invention of ours. This once recorded, as the only deliberate
  divergence, that we took `sampleRGSS` unconditionally where vanilla's shipped
  `textureFiltering` default selects `sampleNearest`; that divergence is now a
  switch with vanilla's default — see *The terrain sampler shipped vanilla's
  RGSS mode, not vanilla's default* below.
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

## The render layer is per quad — landed

Vanilla's layer is **per quad**, not per block. `SectionCompiler`'s quad output
reads `quad.materialInfo().layer()`, which `ChunkSectionLayer.byTransparency`
derives from that quad's own sprite transparency. Ours was one `RenderLayer`
per block state — *"the most transparent layer across its faces"* — so a
`grass_block`'s six fully opaque cube faces inherited `Cutout` from the four
coplanar `grass_block_side_overlay` decals and got an alpha test vanilla never
applies to them.

That is now ported. `ModelSectionView::quad_layer` replaces the old per-block
`is_translucent_at`; `SnapshotModelView` answers it with
`BlockModels::sprite_layer(quad.sprite)`, a direct index into the same
per-sprite table `block_layer` was already rolling up. `mesh_models_layers`
uses the answer for both halves of what the layer decides: a `Translucent` quad
goes to the blended second mesh, and a `Solid` quad carries
`ModelVertex::cutout_bypass` — this renderer's stand-in for vanilla's
`SOLID_TERRAIN` pipeline, which defines no `ALPHA_CUTOUT` and runs no test at
all.

`per_quad_render_layer.rs` measures it through the production call
(`mesh_snapshot_models_layers`, i.e. the real `SnapshotModelView`), over a real
`BlockModels` baked from `client.jar`. Predicted from the jar rather than from
the code: `grass_block[snowy=false]` bakes ten quads, six sampling all-255
sprites and four sampling the 211-clear-texel overlay, so **(24 bypassed, 16
tested, 0 translucent)** vertices. Per-block-state predicts **(0, 40, 0)**. The
measurement lands on the first; the neuter — `quad_layer` forced to `None` —
lands on the second, exactly.

Two further things this fixed, both of which the per-block-state routing could
not express. A water cauldron's opaque body now writes depth on the solid pass
while its partial-alpha liquid blends on the translucent one, which is what
`BlockModels::is_cauldron`'s whole-block veto existed to approximate (the veto
survives, demoting only the liquid quad, so that block's behaviour is unchanged
pending its own gate). And `BlockModels::layer` is now used only for the
questions vanilla also answers per block — occlusion, the packed fast path, the
fluid shoreline test.

**On vanilla assets this is parity, not a fix for the pinprick report.** No
ordinary building block has a non-opaque sprite (see the histogram above), so
the discard could not have fired on a stone or andesite road either way. It
becomes load-bearing the moment a **resource pack** replaces a texture with one
carrying transparent texels, or ships a model that mixes an opaque sprite with
a cutout one in one state — which is exactly the condition the report's server
is under and none of the gates here reproduce.

### Remaining divergence, measured

Vanilla scopes the transparency scan to the quad's own UV window inside the
sprite (`SpriteContents.computeTransparency(u0, v0, u1, v1)`), short-circuiting
to the whole-sprite answer when the sprite is opaque or the window covers it
entirely — the overwhelming majority of quads. `sprite_layer` answers the
whole-sprite question only, so a quad sampling an opaque sub-rect of a cutout
sprite is `Cutout` here and `Solid` there.

Vanilla also unions the scan over every unique animation frame where ours reads
the first. Measured against the 26.2 jar: of 1,269 `textures/block/*.png`,
**zero** classify differently from their first frame than from the whole strip,
so that half is inert on vanilla assets. (That scan also puts the whole-image
split at 695 `Solid` / 517 `Cutout` / 57 `Translucent`, which does not agree
with the 911/309/49 recorded above; the two were taken by different scanners
and the disagreement is unresolved. The individual sprites this page names —
`stone`, `andesite`, `grass_block_top`, `grass_block_side`, `dirt`,
`cobblestone`, `gravel` all-255; `grass_block_side_overlay` at 0×211/255×45;
`white_stained_glass` at 102/155/163 — were re-checked one by one and all
hold.)

## Atlas level 0 carried the raw PNG, and that was the leaf-litter residual

`AtlasBuilder` blitted the **raw** decoded image at level 0 while every mip
level below it came from the *prepared* base (`solidify` for a cutout sprite,
`fill_empty_with_dark` for a `dark_cutout` one). Vanilla has one image:
`MipmapGenerator.generateMipLevels` runs `TextureUtil.solidify` on
`currentMips[0]` **in place** and then sets `result[0] = currentMips[0]`, and
that same `NativeImage` is what `SpriteContents.uploadFirstFrame` uploads at
level 0. So level 0 was the one level in our chain that disagreed with its own
successor.

The preparation never touches alpha — both passes rewrite only the RGB of
texels whose alpha is already `0` — so no cutout decision moved. What moved is
what a **bilinear** tap picks up beside a cutout edge: the model sampler is
`min_filter: Linear` with `mipmap_filter: Linear`, so at any LOD between 0 and
1 a tap straddling the edge blends the transparent neighbour's RGB in.
`block/leaf_litter.png`'s 139 transparent texels are pure **black** against
opaque texels at 125-167 grey, so every surviving fragment beside a hole was
dragged toward black as soon as the plate started to minify.

That is the residual `cutout_minification_flicker_pixels` had recorded as open
and unexplained. Both arms were run on this change, same fixture, same cameras:

| band | raw level 0 (1x / 4x / ratio) | prepared level 0 (1x / 4x / ratio) |
|---|---|---|
| 3 (most minified) | 1697.2 / 2179.2 / 0.779 | 1732.6 / 2856.0 / **0.607** |
| 4 | 2102.8 / 3396.8 / 0.619 | 2973.4 / 3414.9 / **0.871** |
| 5 | 3158.0 / 3349.1 / 0.943 | 3355.0 / 3349.1 / **1.002** |
| 6 (magnified) | 3378.0 / 3521.4 / 0.959 | 3378.0 / 3521.4 / 0.959 |
| 7 (nearest) | 3256.0 / 3161.9 / 1.030 | 3256.0 / 3161.9 / 1.030 |

Painted area rose in every minified band and bands 6 and 7 did not move a
single pixel, which localises the change to minification rather than to the
fixture. Band 3's *ratio* fell while its painted area rose, because the 4x
reference is itself a render through this renderer and gained 31%. The
per-quad layer change contributes **exactly zero** to this fixture — measured
by running it alone, byte-identical — because `leaf_litter` is a single-sprite
cutout block; the two changes were disentangled rather than credited together.

The change is scoped to atlases that actually request mips, so every non-mipped
atlas in the tree (GUI, items, particles) is byte-identical.
`atlas_mips.rs`'s `atlas_level_zero_carries_the_solidified_base_the_mip_chain_
was_built_from` is the gate, with a no-mips control and a neuter that lands on
the raw value.

## The terrain sampler shipped vanilla's *RGSS* mode, not vanilla's default

This is the answer to a **second** owner report, filed alongside the pinpricks
and distinct from them: *"when I look at a platform or whatever that's a bit far
away I can see a divider between each block, which disappears as I get close —
cave walls, stone floors, grass. It shows lines between each block when it
should be showing an unbroken surface."*

### What vanilla actually does, read out of the jar

Four facts, each read from the 26.2 decompile rather than inherited:

| question | vanilla | ours (before) |
|---|---|---|
| terrain sampler | `LevelRenderer` builds `chunkLayerSampler` as `CLAMP_TO_EDGE`, min `LINEAR`, mag `LINEAR`, `maxAnisotropy` (1 unless `ANISOTROPIC`), no LOD clamp | `GpuAtlas`: clamp, min `Linear`, mipmap `Linear`, **mag `Nearest`**, anisotropy 1 |
| atlas gutter | `Stitcher`'s `padding = 1 << mipLevel << clamp(anisotropyBit - 1, 0, 4)` | `AtlasBuilder::with_padding(1 << mip_levels)` — equal while anisotropy is 1 |
| what fills the gutter | `TextureAtlas.uploadInitialContents` draws a quad over the **padded** rect and `animate_sprite.vsh` pushes the sprite UV outward by `padding/width`, sampling the per-sprite scratch texture `CLAMP_TO_EDGE` — an edge extrude, at **every** mip level | `extrude_border`, at every level, `pad >> level` wide |
| the sprite's own UV | `u0 = (x + padding) / atlasWidth` | identical |
| shipped filtering method | `Options`' `textureFiltering` defaults to `TextureFilteringMethod.NONE`, and `OptionsRenderState` initialises its field to it; `GameRenderer` sets `UseRgss` only for `RGSS` | **`sample_rgss`, unconditionally** |

So the predecessor's claim that the gutter is reserved and re-extruded at every
level is **correct**, re-verified here against `Stitcher`,
`TextureAtlasSprite.uploadSpriteUbo` and `animate_sprite.vsh` rather than
inherited. The real block atlas built from the 26.2 jar is 2048×2048, 929
sprites, 5 mip levels, **no** mip cap (`mip_cap == None`, so no sprite reduced
the depth), `stone` at (64, 1376) with a 16-texel gutter on every side.

Two things were checked and found innocent while looking:

* **A tiled surface's boundary is not a special discontinuity.** Bilinear
  filtering across a repeated sprite holds the edge texel flat for half a texel
  on each side of a block boundary and then steps, where a true wrap would ramp.
  Simulated against the real atlas's real mip chain, over `stone`, the step at
  the boundary is **≤ 2/255** and at most 1.7× the largest interior step — and
  vanilla has the identical arrangement, so it is not a divergence either.
  Measured again on a rendered head-on stone wall at 8/12/16/24/32/48/64
  blocks: the phase-averaged column profile is flat to ±3%, with no systematic
  peak or dip at the boundary phase.
* **No per-block darkening on a flat floor.** A contrast-stretched render of a
  uniform stone floor shows no grid at any range.

### The divergence that does show, and where

`sample_rgss` picks its mip level from the **geometric mean** of the two
derivative lengths (`sqrt(min * max)`) — vanilla's own arithmetic, transcribed
correctly. That is an *anisotropy-aware* level: on a surface seen at a grazing
angle it lands up to half the log2 anisotropy ratio sharper than the isotropic
level the hardware would pick. Nothing in this renderer takes the extra taps
that would make that level correct — the atlas sampler's `anisotropy_clamp` is
1 — so the surplus detail is **undersampled rather than resolved**, and a
block-periodic texture aliases into block-periodic structure. `sample_nearest`,
by contrast, hands the *original* derivatives to `textureSampleGrad`, so it gets
the hardware's isotropic level and a distant grazing surface resolves toward a
smooth one.

Measured on a hermetic stone floor at 6° of pitch, 1024×768, as the standard
deviation of ground luminance per 8-row band (a band is a distance band):

| distance | `sample_rgss` (shipped) | `sample_nearest` (now default) | peak per-pixel Δ |
|---|---|---|---|
| 29.6 blocks | 1.83 | **0.31** | 3.7 |
| 23.4 | 2.04 | **0.70** | 5.7 |
| 19.3 | 3.19 | **1.09** | 9.7 |
| 16.4 | 4.15 | **1.60** | 11.7 |
| 14.3 | 6.24 | **1.94** | 14.7 |
| 12.6 | 6.42 | **2.03** | 17.3 |
| 11.3 | 7.75 | **2.94** | 19.3 |
| 10.3 | 7.33 | **3.19** | 18.3 |
| 9.4 | 5.51 | 4.17 | 11.7 |
| 8.0 | 5.62 | 5.42 | 3.0 |
| ≤ 7.4 | — | — | **0.0** |

The last row is what localises the change: every band nearer than ~7.4 blocks is
**byte-identical**, so this moved minification only — which is the report's own
*"disappears as I get close"*, and is what a fix for it has to look like.

### The fix, and what it costs

`model.wgsl` now declares `override use_rgss: f32 = 0.0;` and branches on it,
exactly as `terrain.fsh`'s `main` branches on the `UseRgss` uniform.
`ModelPipeline` binds it beside `alpha_cutout` (both live in `model.wgsl` and
neither in `fluid.wgsl`, so one `is_some()` gates both), from
`TextureFiltering::selected()` — read once per process from
`LODESTONE_TEXTURE_FILTERING`:

```text
LODESTONE_TEXTURE_FILTERING=rgss cargo run --release -p lodestone-shell --bin lodestone
```

`none` (or anything unrecognised, or the variable unset) is vanilla's default and
now ours. `rgss` reproduces the previous shipped image **byte-identically** —
verified, not asserted: the same fixture rendered under the env var compares
equal to the pre-change frame, which is also what proves the constant reaches
pixels rather than being an island.

**The cost is real and is the reason this is a switch rather than a deletion.**
The RGSS arm was landed to fix a different owner report — leaf litter winking in
and out under minification — and it does: on that gate's most minified band it
measured 0.779 of the supersampled reference against `sample_nearest`'s 0.399.
Taking vanilla's default back gives that regression back too, at vanilla's own
severity. Vanilla's answer to wanting both is its **third** value,
`ANISOTROPIC`, which is not implemented here: it needs `anisotropy_clamp > 1` on
the sampler and, as `Stitcher` does, an atlas gutter that grows with the
anisotropy bit (`1 << mipLevel << clamp(anisotropyBit - 1, 0, 4)` — 32 texels at
vanilla's default `maxAnisotropyBit` of 2, twice ours). That is the next piece of
work here, and it is the one that would let a cutout surface keep RGSS-grade
stability without an opaque one paying for it.

### What this does *not* explain

**The sky pinpricks are a separate report and remain open.** Nothing above
involves the alpha test, and the section above already measured that no ordinary
building block has a non-opaque texel to discard; this change alters *which mip
level an opaque texel is read from*, which cannot produce a background-coloured
fragment. The two reports arrived together and share a regime (distance,
grazing angles) but not, on this evidence, a mechanism.

### Still divergent, deliberately not changed here

`GpuAtlas`'s sampler uses `mag_filter: Nearest` where vanilla's chunk-layer
sampler uses `LINEAR`. It matters only under **magnification**, which is the one
regime both reports say is fine, and it is not free to change: `GpuAtlas` is the
shared upload path for the GUI, item, container and icon atlases too, where
`Nearest` magnification is correct (vanilla's `TextureAtlas` keeps its own
sampler at `getClampToEdge(FilterMode.NEAREST)`), so a terrain-only variant has
to be threaded through four call sites first. It is worth doing, because with
`use_rgss = 0` the whole of vanilla's `NONE` mode is `snap_uv`, and `snap_uv`'s
sub-texel rescale is a **no-op** against a `Nearest` sampler — the one-pixel
anti-aliased ramp at each texel edge that vanilla gets, we do not.

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
  `block_models.rs` (`BlockModels::sprite_layer`), `models.rs`
  (`ModelSectionView::quad_layer`, `mesh_models_layers`), `shaders/model.wgsl`.
* `lodestone-assets` — `atlas.rs` (`AtlasBuilder::build`'s level-0 blit) and
  `mipmap.rs` (`generate_mip_levels`, `solidify`, `fill_empty_with_dark`).
* `lodestone-shell` — `mesher.rs` (`snapshot_section`, `mesh_snapshot_models`,
  `SnapshotModelView::quad_layer`, `snapshot_visibility`) and `gpu`
  (`RenderState::upload_section`, `render`).
* `lodestone-world` — `World::block_state_at`, which the oracle reads directly.
