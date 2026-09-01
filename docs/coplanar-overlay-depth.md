# Coplanar overlay depth

## What it is

Several subsystems draw a thin quad a small **world-space** distance in front of another surface and
rely on the depth test to keep it there: a filled map's picture over an item frame or its attachment
wall, a sign's glyph ink and its glowing outline over the sign board, an item-frame body over the wall
it hangs on. This document is the measurement of how much depth separation those overlays actually
have in this renderer, and what the polygon offsets attached to them really contribute on the device
we ship on. The instrument is
`crates/lodestone-render/tests/coplanar_overlay_depth_survey.rs`.

## How it works

### The two quantities

An overlay's separation from the surface behind it has two independent parts, and they behave
completely differently:

1. **Geometric.** The world-space clearance, converted to depth by `Camera::view_projection`. This
   renderer's depth is forward `[0, 1]` `Depth32Float`, which spends almost the whole float32
   mantissa near the near plane, so a fixed world clearance buys a number of representable depth
   values that **collapses with distance**.
2. **The polygon offset**, a `wgpu::DepthBiasState` on the pipeline. Its `constant` term and its
   `slope_scale` term are measured separately below, because they scale with completely different
   things.

### Geometric separation, measured

The depth test compares two points **on the same ray**, not two points at the same world `x`/`y`.
That distinction inverts the angle conclusion and is worth stating plainly, because the first version
of the survey got it wrong: comparing same-`x`/`y` points measures `clearance * cos(theta)`, which
*shrinks* at grazing angles, where the real quantity is `clearance / cos(theta)`, which **grows**. The
two disagree by a factor of 130 at 85 degrees off the normal.

Worst float32 ULP separation over a one-block patch, for a map's `1.01 / 128` clearance, through the
real projection at render distance 12:

| distance | 0° | 30° | 45° | 60° | 75° | 85° |
| --- | --- | --- | --- | --- | --- | --- |
| 2 | 1661 | 1707 | 2001 | 2742 | 5229 | 15923 |
| 4 | 414 | 452 | 540 | 747 | 1436 | 4319 |
| 8 | 103 | 117 | 141 | 194 | 378 | 1129 |
| 12 | 47 | 52 | 62 | 89 | 170 | 511 |
| 16 | 26 | 29 | 36 | 51 | 97 | 289 |
| 24 | 11 | 13 | 15 | 22 | 42 | 130 |
| 32 | 6 | 7 | 8 | 12 | 23 | 73 |
| 64 | 1 | 1 | 1 | 2 | 5 | 18 |

A sign's text plane (`1/256 + 2/2048`) is a little under two thirds of those numbers; the glowing
outline and the ordinary ink share **one** world plane, so their geometric separation *from each
other* is exactly zero at every distance and angle, and the whole ordering between them is their
polygon offset.

Two things to take from the table. **Separation improves with angle**, monotonically, by more than an
order of magnitude from head-on to 85 degrees — so an artefact that is worse at oblique views is not
explained by geometric depth precision. And **it collapses with distance**: head-on, a map has 1 ULP
at 64 blocks, which is the regime `fluid_coplanar_depth_gate` already documents as a tie.

### What a polygon offset actually contributes

Measured on the device rather than taken from a specification, by rendering one quad with a bias and
the identical quad without one and differencing the read-back `Depth32Float`. On Metal:

| question | measured |
| --- | --- |
| is `constant` a raw float add or a ULP count? | **a ULP count** — `constant * 2^(exp(primitive max depth) - 23)`, so `-10` moves a fragment ten representable values at its own binade, independent of distance |
| is the ported sign right? | **yes** — a negative `constant` reduces the written depth, i.e. moves toward the eye, which is what a forward `[0,1]` buffer needs |
| does `constant` scale linearly? | **yes** — `-20` gives exactly twice `-10`'s offset, which is the property `MAP_SURFACE_DEPTH_BIAS` relies on to sit one step ahead of `CAMERA_DEPTH_BIAS` |
| how big is the `slope_scale` term? | **very large, and unbounded** — at a window-space depth slope of `1.56e-5` (about 75 degrees off the normal at 16 blocks) `slope_scale: -1.0` is worth **394 ULP**, against the constant term's 10–20; it grows linearly with the slope |
| what happens when the bias saturates? | **it clamps, it does not discard** — a fragment biased past `0.0` is written at `0.0` and still shades |

That last row retires a plausible-sounding hypothesis: an unbounded slope-scaled offset at a grazing
angle does **not** make a primitive vanish. It only ever wins harder.

The consequence for reading any of this code is that a bias pair like `(constant: -10,
slope_scale: -1.0)` is not two comparable numbers. At head-on the slope term is zero and the constant
is everything; a few degrees off the normal and the slope term is one to three orders of magnitude
larger. **A surface whose competitor has `slope_scale: 0.0` gains enormously at oblique angles**, and
two surfaces with equal `slope_scale` cancel that term entirely and are ordered by the constant alone.

### The ported constants

`lodestone_render::model_pipeline::CAMERA_DEPTH_BIAS` is `(constant: -10, slope_scale: -1.0)`. That is
vanilla's `TEXT_POLYGON_OFFSET` depth-stencil state sign-flipped for forward depth. Read the record
definition, not the call site: the record's fields are `(depthTest, writeDepth, depthBiasScaleFactor,
depthBiasConstant)`, so the literal pair `1.0F, 10.0F` is **scale 1, constant 10**, not the other way
round. `MAP_SURFACE_DEPTH_BIAS` doubles only the constant, deliberately leaving the slope term equal to
the frame body's so the two cancel and the relative ordering does not vary with projected slope.

The terrain pipelines — both `BlockPipeline` and `ModelPipeline::for_layer` — carry
`wgpu::DepthBiasState::default()`, a plain zero. Anything drawn over ordinary terrain therefore has its
full bias as an advantage, not as parity. A comment in the sign text renderer claimed the opposite for
a long time; it was wrong, and nothing failed because the constants it was justifying were fine
anyway.

## How to change it

* **Re-run the survey before reasoning about any of these constants.**
  `cargo test -p lodestone-render --test coplanar_overlay_depth_survey -- --nocapture` prints the
  geometric table; adding `--ignored` runs the device calibration, which needs a GPU adapter and is
  the only thing that can tell you what a bias is worth.
* **Do not compare a `constant` against a `slope_scale` by eye.** See the table above.
* **Adding a clearance is not interchangeable with adding a bias.** A world-space clearance is
  distance-dependent and angle-*independent* in the helpful direction; a constant bias is
  distance-independent; a slope-scaled bias is angle-dependent and unbounded. Pick the one whose
  shape matches the failure.
* The survey's angle axis is only correct because it walks one ray to both planes. If you extend it,
  keep that property — the same-`x`/`y` shortcut looks right and reverses the conclusion.

### Live switches

Native only, read once at process start; any non-empty value other than `0` enables one. None of them
changes the default renderer and none exists on wasm.

| switch | removes |
| --- | --- |
| `LODESTONE_MAP_DISABLE_DEPTH=1` | the map pipeline's depth comparison, its depth write **and** its polygon offset — all three at once |
| `LODESTONE_MAP_DISABLE_DEPTH_TEST=1` | only the comparison (`Always` instead of `LessEqual`) |
| `LODESTONE_MAP_DISABLE_DEPTH_WRITE=1` | only the depth write |
| `LODESTONE_MAP_DISABLE_DEPTH_BIAS=1` | only `MAP_SURFACE_DEPTH_BIAS` |
| `LODESTONE_SIGN_OUTLINE_MATCH_GLYPH_DEPTH=1` | the glowing outline's own bias, giving it the ink's instead |
| `LODESTONE_SIGN_OUTLINE_VANILLA_DEPTH=1` | the glowing outline's bias entirely, which is vanilla's own value for that pass |
| `LODESTONE_SIGN_TEXT_LIFT_PROBE=<blocks>` | nothing — it *adds* world clearance to the plane **both** the outline and the ink sit on, leaving their ordering untouched |

The first row is the reason the next three exist. A live run under it changed three things
simultaneously, so a run that fixed the picture could not say **which** of the three was responsible —
and the three have different fixes. Run the narrow ones singly.

### The glowing sign outline is the same fragile arrangement as a framed map

The live report is that plain sign text is fixed and confirmed good while the **glow** variant's outline
still fights the board at some angles, independently of every `LODESTONE_MAP_*` flag. Reading the emit path
rather than the symptom localises it without a run:

* `push_side_layers_with_state` passes **one** clearance to both layers, so the outline and the ink are on
  the same world plane and their geometric separation from each other is exactly zero at every distance and
  angle.
* `outline_mask_minus_ink` emits the dilated mask **minus** the ink rects, so the two are disjoint in the
  layout and no outline fragment is ever behind an ink fragment. **The outline and the ink cannot fight each
  other at all**; the only surface either contests is the sign board, which is ordinary terrain and carries
  `DepthBiasState::default()`.
* That plane is `1/256 + 2/2048` — **4.9 mm**, *less* than the 7.9 mm a framed map has.
* Plain ink takes `(-20, -2.0)`. The glowing outline takes `(-10, -1.0)`.

So the difference between the arm that works and the arm that is reported broken is **exactly half the
polygon offset, on the same plane, against the same board** — and half of a slope term that is already zero
head-on, which is the shape the framed-map board turned out to have. At 4.9 mm the clearance is worth
`4096 / d^2` representable depth values, so past about 20 blocks the outline is carried by its `-10`
constant alone.

Because outline and ink are disjoint, **both existing switches vary a relationship no artefact can be
about.** `LODESTONE_SIGN_TEXT_LIFT_PROBE` is the one that does not: it moves outline and ink together, so it
separates "the text plane is too close to the board" from "the outline is fighting something other than the
board". Run it first, and read the smallest value that clears the artefact as the deficit — a value that has
to exceed 4.9 mm is saying the clearance is not what orders these surfaces at all. The two bias switches
stay useful afterwards as brackets: `MATCH_GLYPH` gives the outline the ink's full offset (if that fixes it
while the lift does not, the outline needed offset rather than clearance, and the disjointness argument
above is wrong somewhere), and `VANILLA_DEPTH` removes the offset entirely and must make it **strictly
worse** — an improvement there would retire this whole model.

## Configuration

None. The constants above are compile-time; the switches are diagnostics, not settings.

## Dependencies

* `lodestone_render::Camera` — the projection every depth number here comes from.
* `lodestone_render::model_pipeline` — `CAMERA_DEPTH_BIAS`, `MAP_SURFACE_DEPTH_BIAS`,
  `MapDepthDiagnostic`, and the pipeline builders that consume them.
* `crates/lodestone-render/tests/fluid_coplanar_depth_gate.rs` — the precedent for the ULP method and
  the source of the four-ULP floor this document's numbers are read against.
* `docs/filled-map-rendering.md` and `docs/item-frame-rendering.md` — the subsystems whose overlays
  these constants order.
