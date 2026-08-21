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

### Two things that remain divergent from vanilla

Neither is proven to be behind the report, and both are worth knowing:

* **Sampling.** Vanilla's `terrain.fsh` never plain-samples the atlas. It uses
  `sampleNearest` (snap the UV toward the texel centre by the texel's on-screen
  size, then `textureGrad`) or `sampleRGSS` (four rotated-grid samples across
  two mip levels). `model.wgsl` does a single `textureSample` with
  `min_filter: Linear, mipmap_filter: Linear`. For a **cutout** texture that
  matters more than for an opaque one, because the `tex.a < 0.5` discard turns
  filtered alpha into a visibility decision: a flat ground plate at a grazing
  angle is the most minified geometry in the scene, so its discard boundary
  moves with the mip level. This is the strongest remaining candidate for
  camera-dependent shimmer on exactly this family.
* **Cutout alpha down the mip chain.** `leaf_litter.png` is a greyscale PNG
  with a `tRNS` colour key — 117 of 256 texels opaque, **45.7%**. Its baked
  alpha-test coverage at the `0.5` reference runs `0.484, 0.495, 0.535, 0.812`
  down levels 0–3. Vanilla's `MipmapGenerator.scaleAlphaToCoverage` exists to
  hold that constant, and the drift at level 3 has **not** been compared
  against a JVM oracle, so it is a lead rather than a finding. (A first probe
  reported `0.000` at level 4; that was the probe, not the data — vanilla's
  `alphaTestCoverage` iterates `width - 1` by `height - 1`, which is empty for
  the 1×1 top level.)

## How to change it

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
```

Both are `#[ignore]`d and fail closed — a missing `client.jar` or GPU adapter
is a panic, not a skip. The pixel gate's control (`a_coplanar_plate_is_detected`)
must be read before any clean result from it is believed; it has been observed
to fire in 3 of 3 configurations.

## Configuration

* `mipmapLevels` (`Options`, consumed by `resources::mipmap_levels`) rebuilds
  the block atlas at a new mip depth through the `PACK_GENERATION` reload.
* `biomeBlendRadius` reaches the mesher as `mesh_snapshot_models_at`'s
  `blend_radius` and decides the plate's tint, not its geometry.

## Dependencies

* `lodestone-assets` — model parsing (`model.rs`), baking (`bake.rs`), atlas
  stitching and vanilla-parity mip generation (`atlas.rs`, `mipmap.rs`).
* `lodestone-render` — `block_models.rs` (per-state bake, render layer,
  occlusion), `models.rs` (`mesh_models`), `model_pipeline.rs` (depth and cull
  state), `shaders/model.wgsl` (the cutout discard).
* `lodestone-shell` — `mesher.rs` (`snapshot_section`, `mesh_snapshot_models`)
  and `gpu` (`RenderState::upload_section`, `render`).
