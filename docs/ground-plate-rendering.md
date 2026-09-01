# Ground-plate rendering

## What it is

Flat, ground-hugging blocks — leaf litter, carpets, moss carpets, snow layers,
pressure plates, rails, lily pads, frogspawn, redstone dust — render as a thin
horizontal plate a fraction of a block above the floor of their own cell, which
puts them within millimetres of the top face of the block underneath. This page
records what that geometry actually is, how far the depth buffer can separate
it, and which of the obvious explanations for "flat blocks flicker" have been
measured and ruled out.

It exists because the owner reported *"some blocks are popping in and out
weirdly, like z-fighting-ish — for example, the leaves on the ground"*, and the
first two hypotheses anyone reaches for turn out to be measurably wrong.

## How it works

A ground plate is ordinary model geometry: `BlockModels` bakes the vanilla
model, `mesh_models` emits its quads into the section's opaque/cutout mesh, and
`ModelPipeline::for_layer` draws it with `depth_compare: LessEqual` and
back-face culling — the same path every non-cube block takes. Nothing about the
family is special-cased anywhere.

What *is* special is the geometry. Several of these models are **degenerate**:
a single element with `from.y == to.y`, carrying an `up` face and a `down` face
on exactly the same plane. `template_leaf_litter_4` is
`from [0, 0.25, 0] .. to [16, 0.25, 16]` — zero thickness, one plane at
`0.25/16 = 0.015625` blocks. The two coincident faces are wound opposite, so
back-face culling drops one of them; they do not fight each other.

Offsets above the block floor, read out of the real bake by
`lodestone-render`'s `ground_plane_coplanarity_census`:

| block | offset | model |
|---|---|---|
| leaf litter, lily pad, frogspawn, redstone dust | `0.25/16 = 0.015625` | degenerate, no `cullface` |
| carpets, moss carpet, rails, pressure plates | `1/16 = 0.0625` | rails degenerate; carpets a real box |
| snow layer (1) | `2/16 = 0.125` | a real box |
| coral fans | `0.0` | degenerate, **on** the boundary |
| tripwire | `1.5/16` | degenerate |

A carpet's *bottom* face is at `y = 0`, exactly coplanar with the block below's
top face — but it declares `cullface: down`, so it is dropped whenever that
neighbour occludes, which is the only case where there is a face to fight.
Uncullable quads have no such escape, which is why the census gate asserts on
those alone.

## What was measured, and what it rules out

### Depth precision is not the mechanism

This renderer's projection is forward `[0, 1]` (`Camera::projection_matrix`,
`near = 0.05`), whose precision at range is far worse than vanilla's
reversed-Z — `CLAUDE.md` records that a `0.00025`-block separation is
unresolvable here past ~14 blocks. A ground plate's separation is much larger.
Through `Depth32Float`, for the smallest offset in the family (`0.015625`):

| view distance | ULPs of depth separation |
|---|---|
| 2 | 3253 |
| 8 | 205 |
| 16 | 52 |
| 32 | 13 |
| 64 | 3 |
| 128 | 0 |

Only beyond ~100 blocks does it collapse, and a grazing view makes it *better*,
not worse: standing at eye height `h` looking at ground `d` away, the
separation along the ray is `offset · d / h` rather than `offset`, so the ULP
count goes as `near · offset / (d · h)` — around **8 ULPs at 1000 blocks**.
Straight down from 100+ blocks up is the only geometry that loses, and a leaf
litter block is sub-pixel there.

### Coplanar full-block plates do not speckle — they flip wholesale

The instinctive detector for z-fighting is a sub-pixel camera nudge. It was
built, and its control was run: the same production mesh with the plate snapped
**exactly** onto the block boundary moved 37 of 196,608 pixels (0.02%) — it did
not fire. The reason is structural, not a threshold problem. A plate quad and
the grass block's top quad have identical `x`/`z` extents, so once their depths
collapse to one value the rasteriser interpolates *identical* depth across
both, and `LessEqual` hands the pixel to whichever was drawn later — the same
one, every frame, at every camera.

So the axis a coplanar surface is unstable along is **draw order**, and the gate
measures that instead: render one camera through two independently built
`RenderState`s, which is what a remesh or a chunk reload produces live. With the
plate snapped onto the boundary the two frames disagree on **2.7–5.4%** of the
frame at all three pitches. With the real geometry, leaf litter, white carpet,
snow, an oak pressure plate and a rail all disagree on **0–2 pixels** — three
orders of magnitude apart.

### The plates do not dissolve at range

The other reading of "popping in and out" is that the plate simply stops being
drawn. Diffing a plated world against the identical world with no plate, per
horizontal band of the frame, with the horizon row *derived from the camera*
rather than eyeballed: every ground band shows the plate painting, out to the
edge of an 8-chunk world.

### Alpha-coverage preservation was never missing

An earlier version of this page listed "we do not do vanilla's
`scaleAlphaToCoverage`" as the leading suspect, on the strength of
`leaf_litter.png`'s baked coverage running `0.484, 0.495, 0.535, 0.812` down
levels 0–3. Both halves of that are wrong.

`lodestone-assets`'s `mipmap.rs` **is** a faithful port of 26.2's
`MipmapGenerator`, checked clause by clause against the decompiled source: the
five-iteration bisection on alpha scale, the `+ 0.025` bias added to every
texel after it, `solidify` before the base coverage is taken, the linear-light
`meanLinear` downsample, and the `width - 1` by `height - 1` sweep. Every
cutout sprite in the block atlas goes through it.

**But for a long time it went through it under the wrong strategy.** Vanilla
picks the downsample per *sprite*, from that sprite's own `*.png.mcmeta`
`texture` section: `SpriteContents` reads `TextureMetadataSection`'s
`mipmap_strategy` and `alpha_cutoff_bias` and hands both to
`MipmapGenerator.generateMipLevels`. `TextureMeta` did not parse that section
at all (it only recorded `"texture"` as a present-but-uninterpreted key), and
`AtlasBuilder::build` passed `MipStrategy::Auto` and a `0.0` bias
unconditionally — so `MipStrategy::StrictCutout`, `DarkCutout` and an explicit
`Mean` were implemented, tested, and had **no producer anywhere**.

Counted straight out of the 26.2 jar's own 102 block-texture `.mcmeta` files,
that was the wrong strategy for **45 sprites**:

| `mipmap_strategy` | count | sprites |
|---|---|---|
| `strict_cutout` | 27 | every small flower, the amethyst buds and cluster, both mushrooms, both fungi, `nether_sprouts`, `sweet_berry_bush_stage0` |
| `dark_cutout` | 13 | every `*_leaves`, plus `mangrove_roots_top`/`_side` |
| `mean` | 5 | `glass`, `redstone_dust_dot`/`_line0`/`_line1`/`_overlay` |
| `alpha_cutoff_bias: 0.1` | 5 | `cactus_side`, `cactus_top`, `kelp`, `kelp_plant`, `tripwire` |

The difference is not cosmetic for exactly the reason the rest of this page is
about: `strict_cutout` preserves coverage against a **0.3** alpha reference
rather than `0.5`, `dark_cutout` fills the transparent areas with a darkened
colour and blends only non-transparent texels, `mean` does neither, and the
bias is added to every texel after the rescale. All four change the alpha the
terrain shader then thresholds at `0.5`, so the wrong one shows up as texels
winking in and out under minification — the family this page exists for.

Both are now parsed (`TextureMeta::texture`, `TextureSection`) and threaded
per sprite through `AtlasBuilder::build`. `atlas_mips.rs`'s
`a_sprites_mcmeta_mipmap_strategy_selects_its_downsample` is the wiring gate,
and it is a wiring gate rather than an algorithm one on purpose: it asserts
each of the four inputs changes the produced chain, collecting the failures
rather than asserting inside its loop, and under a neuter that restores the
hardcoded `Auto`/`0.0` it reports **all four** as unwired.

Note `leaf_litter.png` itself carries no `.mcmeta`, so it is `auto` in vanilla
too and this changes nothing for it. The measured residuals below are
unaffected; what changes is the rest of the cutout family the owner's report
names by example.

The drifting numbers are the *estimator*, not the data. `alphaTestCoverage`
bilinearly supersamples each 2×2 quad, so at level 3 (a 2×2 image) it has
exactly **one** quad to average and at level 4 (1×1) it has none. Measured on
the real sprite, the fraction of texels that actually pass the `0.5` discard
runs:

| level | size | `alphaTestCoverage` | texels passing `alpha >= 0.5` |
|---|---|---|---|
| 0 | 16×16 | 0.4844 | 117/256 = 0.457 |
| 1 | 8×8 | 0.4949 | 36/64 = 0.563 |
| 2 | 4×4 | 0.5347 | 8/16 = 0.500 |
| 3 | 2×2 | 0.8125 | 2/4 = 0.500 |
| 4 | 1×1 | 0.0000 | 1/1 = 1.000 |

Coverage is held. The `0.812` is one sample quad, and the `0.0000` at 1×1 is
the empty sweep — vanilla reaches the same `bestAlphaScale = 1.0` there via a
`0.0 / 0` NaN that fails every comparison in its bisection.

One real defect did come out of re-reading that port: `darkened_alpha_blend`
transcribed vanilla's `ARGB.color(a, r, g, b)` argument order literally and so
rotated every channel by one, painting alpha into blue. It is fixed — and it
was fixed while nothing selected `MipStrategy::DarkCutout`, so it never reached
a pixel until the `.mcmeta` `texture` section above was wired. Every `*_leaves`
sprite now takes that path, which means `darkened_alpha_blend` is live code for
the first time and its correctness is load-bearing rather than latent.

### Sampling was the mechanism, and vanilla's *default* sampler is not the fix

`model.wgsl` used to take a single plain `textureSample`. Vanilla's
`terrain.fsh` never does: it takes `sampleNearest` (rescale the bilinear
weight about `0.5` by screen-pixels-per-texel, then `textureGrad`) when
`textureFiltering` is `NONE`, and `sampleRGSS` (four rotated-grid sub-texel
taps at two mip levels, blended, cross-faded back to `sampleNearest` while
magnified) when it is `RGSS`. `NONE` is vanilla's shipped default.

For a **cutout** the difference is not cosmetic: the `tex.a < 0.5` discard
turns filtered alpha into a visibility decision, so how the alpha is filtered
decides which texels exist. A ground plate at a grazing angle is the most
minified geometry in a scene, which is why the family shows it first — but
every cutout in the game is on the same path.

Measured on a leaf-litter plate at pitch 12°, as each band's painted area over
a **4×-supersampled render of the same camera** (see the instrument below);
`jitter` is that area's mean second difference across a sub-block camera sweep,
over the area:

| band | plain `textureSample` | `sampleNearest` | `sampleRGSS` |
|---|---|---|---|
| 3 (most minified) | 0.401, 0.0130 | 0.399, 0.0102 | **0.779, 0.0089** |
| 4 | 0.653, 0.0065 | 0.658, 0.0100 | 0.619, 0.0074 |
| 5 | 0.924, 0.0117 | 0.944, 0.0114 | 0.943, 0.0112 |
| 6 (magnified) | 0.959, 0.0047 | 0.959, 0.0047 | 0.959, 0.0047 |
| 7 (nearest) | 1.030, 0.0038 | 1.030, 0.0038 | 1.030, 0.0038 |

So the far plate was painting **40%** of the area it should, and porting
vanilla's default sampler faithfully moved that by two parts in a thousand.
Only the supersampling arm helps, and `model.wgsl` therefore takes
`sample_rgss` unconditionally rather than reproducing vanilla's `NONE`
default — with an early return to `sample_nearest` while magnified, which is
the same value the cross-fade would produce and skips eight taps for the
majority of the screen. If the `textureFiltering` video row is ever wired,
this is the function it selects and `sample_nearest` is its `NONE` arm; the
two already compose exactly as vanilla composes them.

Band 4 stays at 0.62–0.66 under every sampler. That is unexplained and is
recorded as an open residual rather than folded into a threshold.

### The `mipmapLevels` setting was not reaching the atlas terrain samples

Found while building the control above, and it is the reason the first version
of that control read **byte-identical** for a 4-level and a 0-level atlas.
There are two stitched atlases. `BlockAtlas::build_with_mip_levels` honours the
setting and feeds the packed cube pipeline; `BlockModels` built its **own**
complete atlas at a hardcoded `BLOCK_ATLAS_MIP_LEVELS`, and that is the one the
model pass binds — which is what draws terrain in a live session. So dragging
the slider rebuilt an atlas nothing sampled.

`BlockModels::build_with_mip_levels` now takes the depth (it also sets the
stitcher's gutter, `1 << levels`, so it has to be chosen before packing rather
than adjusted after), and `resources.rs` passes the same `mipmap_levels()` to
both. The gate that existed for this asserted on `BlockAtlas`'s `mip_count`,
which is not the atlas terrain reads — a parity check pointed one object to the
left of the one that matters.

## How to change it

* **The sampler is `model.wgsl`'s, not `GpuAtlas`'s.** `sample_rgss` and
  `sample_nearest` do the filtering the wgpu sampler is not asked to do:
  `GpuAtlas` binds `mag_filter: Nearest, min_filter: Linear,
  mipmap_filter: Linear` with `anisotropy_clamp` at 1, and wgpu will not accept
  anisotropy above 1 unless all three filters are `Linear` — which would make
  every magnified block blurry were the shader not already doing its own
  point-sampling. That is the order to change things in if anisotropic
  filtering is ever wanted: shader first, sampler second.
* **Do not reach for a depth bias.** The table above says the family's real
  offsets resolve throughout the render distance, so a bias would be tuning
  against a mechanism that is not firing — and `CLAUDE.md` records that a bias
  that hides an artefact at one distance fails at another under this
  projection. If a future report really is depth, the durable fix is
  reversed-Z, which is its own change with its own sign flips.
* **Geometry comes from the jar.** `crates/lodestone-assets`'s `bake.rs`
  handles a degenerate element correctly (`calculate_facing` derives ±Y from
  the cross product, `recalculate_winding` rebuilds each face in its own
  corner order), so an `up`/`down` pair on one plane stays oppositely wound.
  If you touch either function, the census gate below is what catches a
  regression.
* **Adding a block to the family** means adding it to `FAMILY` in the census
  gate with its offset read from the model JSON — never from another value
  this renderer produced.

## Instruments

```bash
# every 26.2 state's horizontal quads, by distance to a block boundary,
# plus the gate on the family's own offsets
cargo test -p lodestone-render --test ground_plane_coplanarity_census -- --ignored --nocapture

# the same family rendered through the production mesher and pipeline:
# stability across a second independent upload, and coverage out to range
cargo test -p lodestone-shell --test ground_plate_z_fight_pixels -- --ignored --nocapture

# the temporal one: a sub-block camera sweep over a leaf-litter plate, per-band
# painted area against a 4x-supersampled reference, and the no-mip-chain control
cargo test -p lodestone-shell --test cutout_minification_flicker_pixels -- --ignored --nocapture
```

`cutout_minification_flicker_pixels` is the instrument the sampling table above
came from, and its shape is the point: a **single frame cannot see this bug**.
One static frame of a shimmering surface and one of a stable surface look
identical, so the gate sweeps the eye an eighth of a block and measures the
*second* difference of each band's painted area — a legitimate sub-pixel
translation moves that area smoothly, and a smooth series has zero second
difference, so only a quantity that jumps between neighbouring frames survives.
Its magnitude half compares the 1× area against a 4×-supersampled render of the
same camera, which is an expectation from outside the sampler under test. Read
`a_mipless_atlas_flickers` before believing either: it rebuilds the same corpus
with no mip chain at all and must show materially more jitter, and it is what
caught the two-atlas island above by reporting a control that changed nothing.

Both are `#[ignore]`d and fail closed — a missing `client.jar` or GPU adapter
is a panic, not a skip. The pixel gate's control (`a_coplanar_plate_is_detected`)
must be read before any clean result from it is believed; it has been observed
to fire in 3 of 3 configurations.

## Configuration

* `mipmapLevels` (`Options`, consumed by `resources::mipmap_levels`) rebuilds
  the block atlas at a new mip depth through the `PACK_GENERATION` reload.
* A sprite's own `*.png.mcmeta` `texture` section (`mipmap_strategy`,
  `alpha_cutoff_bias`) selects how its mip chain is built — parsed by
  `TextureMeta` into `TextureSection` and consumed per sprite by
  `AtlasBuilder::build`. A resource pack can therefore change a sprite's
  downsample without changing a pixel of its base texture, and an
  unrecognised `mipmap_strategy` fails the whole `.mcmeta` exactly as vanilla's
  codec does.
* `biomeBlendRadius` reaches the mesher as `mesh_snapshot_models_at`'s
  `blend_radius` and decides the plate's tint, not its geometry.

## Dependencies

* `lodestone-assets` — model parsing (`model.rs`), baking (`bake.rs`), atlas
  stitching and vanilla-parity mip generation (`atlas.rs`, `mipmap.rs`).
* `lodestone-render` — `block_models.rs` (per-state bake, render layer,
  occlusion, and the *complete* atlas the model pass binds), `models.rs`
  (`mesh_models`), `model_pipeline.rs` (depth and cull state),
  `texture.rs` (`GpuAtlas`'s sampler and mip upload), `shaders/model.wgsl`
  (the sampling functions and the cutout discard).
* `lodestone-shell` — `mesher.rs` (`snapshot_section`, `mesh_snapshot_models`)
  and `gpu` (`RenderState::upload_section`, `render`).
