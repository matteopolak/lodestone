# The menu background and the title-screen panorama

## What it is

Vanilla's out-of-world menu screens are drawn over a **spinning cubemap
panorama**, with a flat wash of `textures/gui/menu_background.png` composited on
top — except the title screen, which shows the raw panorama. This is the port of
both: [`crates/lodestone-shell/src/menu/panorama.rs`](../crates/lodestone-shell/src/menu/panorama.rs)
plus [`src/shaders/panorama.wgsl`](../crates/lodestone-shell/src/shaders/panorama.wgsl),
drawn from `MenuRenderer::draw` in `menu/render.rs`.

It replaces the flat `BG` fill for every screen in `owns_frame`. The pause menu
is untouched: it is an `overlay` frame drawn over a live world, and vanilla's
`PauseScreen.extractBackground` draws only the in-world menu background there,
which `menu/render.rs`'s `OVERLAY_BG` already reproduces exactly.

## The measurement that reframes the whole task

Both background textures were decoded straight out of 26.2's `client.jar`:

| file | size | content |
|---|---|---|
| `gui/menu_background.png` | 16×16 | **one** distinct RGBA: `(0, 0, 0, 64)` |
| `gui/inworld_menu_background.png` | 16×16 | byte-identical to the above |
| `gui/{header,footer}_separator.png` | 32×2 | two colours: black α191, white α51 |
| `gui/inworld_{header,footer}_separator.png` | 32×2 | identical to the non-inworld pair |
| `gui/title/background/panorama_{0..5}.png` | **1×1** | solid grey `(98, 111, 113)` |
| `gui/title/background/panorama_overlay.png` | **1×1** | `(255, 255, 255, 0)` |

Three consequences, and each one deletes work rather than creating it:

1. **There is no dirt texture to reproduce.** `menu_background.png` is flat, so
   vanilla's tiled 32 px blit is a 25 %-black wash and one quad is pixel-identical
   to tiling. The `inworld_` variant being byte-identical means the
   `minecraft.level == null` fork at `Screen.java:418` is, in 26.2, a distinction
   without a difference — do not spend effort on it.
2. **`panorama_overlay.png` is a provable no-op**, so it is not drawn. Adding it
   later is one textured quad on the existing menu-sprite pipeline.
3. **The shipped panorama is a flat grey.** Against the real jar this feature
   changes the title screen from `(26, 26, 31)` to `(98, 111, 113)` and nothing
   else — the spin is invisible, because a solid-coloured cube looks the same at
   every yaw. That is *correct* for this build, and it is why the pixel gate
   attaches a synthetic six-colour cubemap instead. Older jars in `.cache/mc`
   (1.8.9, 1.12.2) carry real 256×256 faces, which is what confirms the 26.2
   files are placeholders rather than a wrong path.

Anyone writing a gate here should note the trap: **a gate asserting the panorama
is non-uniform would fail against the real 26.2 pack**, and a gate asserting it is
uniform would pass with the cubemap unbound.

## How it works

### The six constants that decide the image

All read from `.cache/mc/26.2/client-src`, all `pub const` in `panorama.rs` with
their source line, because five of the six fail *plausibly*: a scrambled sky is
still a sky.

| what | value | source |
|---|---|---|
| face → layer order | `_1, _3, _5, _4, _0, _2` | `CubeMapTexture.java:14` |
| per-face flip | vertical (`swapY = true`) | `CubeMapTexture.java:28,49` |
| sampler | **Linear** (`blur = true`) | `CubeMapTexture.java:53` |
| projection | perspective, fovy 85°, near 0.05, far 10.0 | `CubeMap.java:29-31` |
| model-view | `rotationX(π) · rotateX(10°) · rotateY(spin)` | `CubeMap.java:57-59`, `GuiRenderer.java:120` |
| spin | `wrapDegrees(spin + realtimeDeltaTicks · speed · 0.1)`, speed default 1.0 | `Panorama.java:24-28`, `Options.java:313-320` |

A cubemap's layers are `+X, -X, +Y, -Y, +Z, -Z` — the same table in the GL,
Vulkan and WebGPU specs — so composing that with the suffix order gives
`panorama_1 = +X`, `panorama_3 = -X`, `panorama_5 = +Y`, `panorama_4 = -Y`,
`panorama_0 = +Z`, `panorama_2 = -Z`. **Layer order is the part that is
API-independent**; what is *not* independent is the within-face `(u, v)`
orientation, which is the one residual risk in this port (see below).

The spin is 0.1°/tick at speed 1.0, and a tick is 1/20 s: **2°/s, a three-minute
revolution.** Quote that when someone reports the panorama as static. It is also
*realtime* delta ticks, not game ticks — the title screen has no world clock — so
`PanoramaRenderer::advance` reads an `Instant` rather than taking a tick count.
Its first call establishes the baseline and advances nothing, so a fresh renderer
is always at spin 0.

### Which screens get the wash

`Screen.extractBackground` (`Screen.java:388-400`), out of world, draws panorama →
blur → `menu_background`. `TitleScreen` **overrides `extractBackground` with an
empty body** (`TitleScreen.java:330`) and draws the panorama itself from
`extractRenderState` (`:307`) — so the title screen alone shows the undimmed
cubemap. That fork is `panorama::dim_for_screen`, keyed on `MenuFrame::logo`,
which `frame_for` sets for `Screen::MainMenu` and nothing else.

### Why the dim is a uniform and not a quad

Compositing black at α = 64/255 with standard alpha blending is
`dst' = dst · (1 - α)`. The shader instead multiplies the sampled colour by
`1 - α` before writing. These are the *same* operation on both target kinds:

- on an `*UnormSrgb` target the hardware decodes `dst`, blends in linear, and
  re-encodes — so the blend is `dst_linear · (1 - α)`, and the shader's
  `sample_linear · (1 - α)` is the same value;
- on a plain `Rgba8Unorm` target both are a raw multiply.

So one uniform float replaces a second pipeline and a second full-screen quad.
This does mean the wash is a **linear**-space multiply where vanilla, not being
colour-managed, does it in gamma space — but that is not a regression introduced
here: the whole menu pass already blends in the target's space, `OVERLAY_BG`
included. Fixing it is a whole-pass decision, not a panorama one.

### Why no depth and no culling

The panorama draws first into the menu's existing pass, which has no depth
attachment, and the pipeline is `cull_mode: None`. That is a deliberate divergence
from `RenderPipelines.PANORAMA`, which leaves the builder's cull default on.

The reasoning: from a point inside a convex box, every ray in the frustum exits
through exactly **one** face, and everything on the near side of the camera is
removed by the near plane at 0.05 (the nearest surface is at distance 1). So no
pixel is covered twice and there is nothing for a depth test or a winding rule to
arbitrate. Relying on culling instead would mean betting on a screen-space
winding polarity, which this repo has got backwards before — see `CLAUDE.md` on
the GUI winding invariant being *negative*.

Depth conventions are likewise moot: glam's `perspective_rh` is `[0, 1]` where
JOML's may be `[-1, 1]`, but the difference lives in the z row and nothing reads
z here. The x/y rows are identical.

### Where it plugs in

`MenuRenderer` gained a `panorama: Option<PanoramaRenderer>` and a
`panorama_attempted` flag — the same lazy shape as `sprites`/`gui_attempted`, and
lazy for the same reason: the upload needs a `Queue`, which only the draw paths
have. `MenuRenderer::draw`, for `!frame.overlay`, advances the spin, writes the
uniform, draws the cube first, and **skips the flat backdrop quad**. That skip is
load-bearing: the backdrop is opaque and would hide the panorama completely.

`geometry`/`build` are untouched, so every hermetic layout test still sees the
same vertex stream it always did. The backdrop quad is still emitted and still
uploaded; it is simply not drawn when the panorama is bound.

## How to change it

- **Reordering `FACE_SUFFIXES` is the mistake to fear.** It compiles, it renders,
  and it looks like a sky. `panorama::tests::the_face_order_is_vanillas_suffix_table_not_zero_through_five`
  and the pixel gate's three-face survey are what stop it.
- **To draw `panorama_overlay.png`**, blit it as a textured quad on the existing
  menu-sprite pipeline at texture size 16×128 tiled over the full canvas
  (`Panorama.java:31`). It cannot be an atlas sprite: tiling needs `Repeat`, and a
  stitched sheet would sample its neighbours.
- **To wire `panoramaSpeed`**, call `PanoramaRenderer::set_speed`; 0.0 reproduces
  `Panorama.holdSpin`, which is what `AccessibilityOnboardingScreen` uses.
- **The header/footer separators are still not drawn.** They are real art (32×2,
  black α191 over white α51) and vanilla puts them at the top and bottom edge of
  every scrolling list (`AbstractSelectionList.java:219-224`) and under a
  `MenuTabBar` (`MenuTabBar.java:139-151`). They are not a background: they belong
  with whatever draws the list frame, which is why they are out of scope here.
- **To extend the panorama to the pause menu** — don't. Vanilla does not: with a
  level loaded, `extractBackground` takes the `isInGameUi` / in-world branch and
  the world is what shows through.
- **Any gate with an absolute backdrop luminance now has a confound.** The
  backdrop of an out-of-world menu is no longer `BG`. `menu_button_pixels.rs`
  calls `detach_panorama()` before its first draw for exactly this reason: its
  `PLAIN_BORDER_MAX`, its "backdrop control above the logo", and its "nothing is
  drawn in the gap below the button" bound (`< 40.0`) were all calibrated against
  `BG`'s ~28 in a linear target, and 26.2's flat grey panorama reads ~38 there —
  which that last bound has almost no room for, since a *button interior* reads
  ~39.8. Prefer `detach_panorama()` to re-calibrating: it makes the background of
  a button-chrome gate a known constant instead of an asset.
- **If a pack with real panorama art is ever used**, the backdrop stops being
  spatially uniform, and any gate that compares one background patch to another
  (rather than detaching) becomes wrong. 26.2's placeholder faces are the only
  reason such a comparison works today.

## Configuration

None. There is no option and no env var; the cubemap is loaded from the same
resolved pack every other asset uses (`LODESTONE_ASSETS`, or the default search in
`resources::asset_root`). `resources::load_panorama` is **fail-open** like every
sibling loader: a jar-less run, a missing face, or faces that disagree in size
logs a warning and leaves the menu on its flat backdrop rather than failing
startup.

## Dependencies

- `lodestone-assets` for `ResourceManager` and `Image::decode_png` (which expands
  the greyscale and RGB faces to RGBA8 for us).
- `glam` for the matrices, `bytemuck` for the uniform, `wgpu` for the cube
  texture.
- `menu/render.rs` for the draw site; `resources.rs` for the loader. Nothing in
  `app.rs` changed.

## Residual risk

**The within-face `(u, v)` orientation is not verified by any gate here.** The
pixel gate uses six solid faces, so it pins layer *selection* only; the hermetic
`assemble` test pins the flip on the CPU side. What neither can see is whether
wgpu's cube-face `(sc, tc) → (u, v)` mapping matches GL's for a given layer. The
argument that it does: the face-selection tables in the GL, Vulkan and WebGPU
specs are the same table, and wgpu's NDC has y **up** exactly as GL's does (only
the depth range differs), so there is no framebuffer flip to compensate for. That
argument is not a measurement. Against 26.2's flat grey faces the question is
unobservable in any case; it becomes observable the moment a pack with real
panorama art is used, and the symptom would be a sky that is mirrored or rotated
*within* one face while the faces themselves are in the right places.

## Tests

- `crates/lodestone-shell/src/menu/panorama.rs` — hermetic, no GPU: face order,
  the vertical flip (with a control proving an unflipped stack would fail it),
  size/squareness rejection, the 36-vertex expansion, `wrap_degrees`, the 2°/s
  spin rate, and the `dim_for_screen` fork.
- `crates/lodestone-shell/tests/menu_panorama_pixels.rs` — `#[ignore]`d GPU gate:
  attaches a synthetic six-colour cubemap, renders the real `frame_for` title
  screen, and asserts `panorama_0` straight ahead, `panorama_1` on the right edge
  and `panorama_3` on the left. Its probe band is derived from `title_slot` and
  checked against every widget rect, its failure output prints a bounding box, and
  its negative control is `detach_panorama`, which must make all six face colours
  disappear.
