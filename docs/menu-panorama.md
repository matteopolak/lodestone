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

## The mistake worth reading this doc for: `client.jar` is not the whole pack

**The panorama shipped for one commit as a flat grey sky, from an entirely correct
measurement of the wrong file.**

The first port of this feature decoded the six faces out of `client.jar`, found
69-byte 1×1 solid-grey PNGs, verified they really were 1×1 with a program, checked
that older jars (1.8.9, 1.12.2) carry real 256×256 faces, and concluded that
Mojang had placeholdered the 26.2 panorama. Every step of that was sound. The
conclusion was wrong, and the commit message asserted it in bold.

**The real faces are delivered through the asset index, not the jar.** From
`.cache/mc/26.2/asset-index-32.json`:

| name | `client.jar` | asset store |
|---|---|---|
| `…/panorama_0.png` | 69 B, 1×1 grey | **547,239 B, 1024×1024** |
| `…/panorama_1.png` | 69 B | 294,940 B |
| `…/panorama_2.png` | 69 B | 425,769 B |
| `…/panorama_3.png` | 69 B | 461,522 B |
| `…/panorama_4.png` | 69 B | 738,917 B |
| `…/panorama_5.png` | 69 B | 118,484 B |
| `minecraft/font/include/unifont.json` | 29 B | 3,993 B |
| `…/panorama_overlay.png` | 68 B | 86 B |

So the jar ships **deliberate stubs** for files the object store overrides. This
is not a stale extraction — the jar was re-read with `zipfile` rather than through
the `client-src` tree, and it genuinely contains the stubs.

Three things make this worth writing down rather than just fixing:

1. **The failure mode is invisible.** A flat-grey cubemap binds, uploads, samples
   and draws perfectly. Every "the panorama reached pixels" assertion passes. The
   only symptom is that the sky is boring and the spin does nothing — which the
   port had *already explained away* with a correct fact (2°/s is a three-minute
   revolution, so "it looks static" is expected). Two independent true statements
   combined into a wrong conclusion.
2. **The scope is tiny, so there is no pipeline to rebuild.** Of 5057 index
   objects, exactly **8** share a name with a jar entry, and those 8 are the table
   above. Every other index object is index-only: 4871 `.ogg`, 146 `.json`, 32
   `.png` that shadow nothing, 5 `.zip`, 2 `.icns`, 1 `.mcmeta`. The panorama and
   `unifont.json` are the *only* jar entries in the game a store object overrides.
3. **The rule is one line.** For any name in both, prefer the object store. That
   is what `panorama::load` does, per face, and what `crate::asset_objects` exists
   to make cheap.

### What the real faces actually measure

Decoded from the object store, in cubemap layer order:

| layer | face | size | distinct RGB | lum mean | lum stdev |
|---|---|---|---|---|---|
| 0 `+X` | `panorama_1` | 1024×1024 | 743 | 17.8 | 5.2 |
| 1 `-X` | `panorama_3` | 1024×1024 | 7577 | 17.3 | 7.7 |
| 2 `+Y` up | `panorama_5` | 1024×1024 | 355 | 24.0 | 3.7 |
| 3 `-Y` down | `panorama_4` | 1024×1024 | 8566 | 23.8 | 13.6 |
| 4 `+Z` | `panorama_0` | 1024×1024 | 9753 | 22.2 | 9.1 |
| 5 `-Z` | `panorama_2` | 1024×1024 | 4648 | 18.0 | 6.0 |

All six square and equal, so the cube texture is legal. All six **dark** (mean
17–24) — it is a night panorama, which is worth knowing before reporting "the
title screen is too dark" as a bug. And all six richly varied, which is what makes
a non-uniformity gate viable: a stub face reads stdev **exactly 0.0**, vanilla's
flattest real face reads **3.7**.

The 25 MB of stacked RGBA this implies is held only until the upload; see
`PanoramaFaces::rgba`.

### The background textures, which really are flat

Decoded from `client.jar`, and unaffected by the above — none of these names
appears in the asset index:

| file | size | content |
|---|---|---|
| `gui/menu_background.png` | 16×16 | **one** distinct RGBA: `(0, 0, 0, 64)` |
| `gui/inworld_menu_background.png` | 16×16 | byte-identical to the above |
| `gui/{header,footer}_separator.png` | 32×2 | two colours: black α191, white α51 |
| `gui/inworld_{header,footer}_separator.png` | 32×2 | identical to the non-inworld pair |

So: **there is no dirt texture to reproduce.** `menu_background.png` is flat, so
vanilla's tiled 32 px blit is a 25 %-black wash and one quad is pixel-identical to
tiling. The `inworld_` variant being byte-identical means the
`minecraft.level == null` fork at `Screen.java:418` is, in 26.2, a distinction
without a difference — do not spend effort on it.

### `panorama_overlay.png` is inert, and this time it is the real file

The overlay is **not drawn**, and the reason has now been checked against the
asset-store object rather than the jar stub — which is exactly the check the
panorama faces did not get.

Object `9dd32387135eefa7ab95996d52a5ca4cec8a3b30`, 86 bytes, decodes to **1×1
RGBA, one distinct value `(255, 255, 255, 0)`, alpha extrema `(0, 0)`**. Confirmed
by hexdump: `IHDR` is `00000001 × 00000001`, colour type 6, and the entire `IDAT`
inflates to `ff ff ff 00`. The 86 versus the jar copy's 68 bytes is a `gAMA`
chunk, not content.

Vanilla blits it at texture size 16×128 tiled to the full screen
(`Panorama.java:31`); tiling a 1×1 fully transparent texture cannot change a
pixel, so implementing the blit would be provable dead code. If a future version
or a resource pack makes it real, it is one textured quad on the existing
menu-sprite pipeline — not another pass.

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

### Where the bytes come from

`resources::load_panorama` opens **two** sources over one root: the jar (for the
fallback) and an
[`AssetObjectStore`](../crates/lodestone-shell/src/asset_objects.rs) (for the real
faces). `panorama::load` prefers the store per face and counts how many it got,
which lands in `PanoramaFaces::from_object_store` and is surfaced as
`MenuRenderer::panorama_faces_from_object_store()`.

**6 means the real art; 0 means six jar stubs and a flat sky.** A gate that means
to measure the real panorama must assert that count — `panorama_attached()` is not
enough, because the stubs bind and draw perfectly. The loader logs at `info` when
it got all six from the store and at `warn`, naming the fix, when it did not.

Populate the store with:

```bash
cargo run -p xtask -- fetch-assets --version 26.2
```

which downloads exactly the eight shadowed objects (~2.6 MB), verifying each
against its index SHA-1. It deliberately does not fetch the 5049 index-only
objects.

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
- **Any gate with an absolute backdrop luminance now has a confound, and it is
  not fixable by re-calibrating.** The backdrop of an out-of-world menu is no
  longer `BG` — it is 1024×1024 of varied night sky whose sampled value depends on
  where in the frame you look, on the spin at that instant, and on whether the
  object store is populated at all (unpopulated gives flat grey stubs, a different
  number again). `menu_button_pixels.rs` calls `detach_panorama()` before its first
  draw for exactly this reason: its `PLAIN_BORDER_MAX`, its "backdrop control above
  the logo", and its "nothing is drawn in the gap below the button" bound (`< 40.0`,
  against a *button interior* of ~39.8) were all calibrated against a compile-time
  constant. **Prefer `detach_panorama()` to re-calibrating**: a button-chrome gate
  wants a known background, not an asset.
- **Two things a background gate must not do**: compare one background patch to
  another (the sky is not uniform, so two patches legitimately differ), or assert
  an absolute luminance (see above). Comparing *bound versus detached* is the shape
  that works, and it is what the non-uniformity gate does.

## Configuration

No option and no env var of its own. The cubemap comes from the same resolved pack
every other asset uses (`LODESTONE_ASSETS`, or the default `.cache/mc/*` search in
`resources::asset_root`), and the object store is read from that *same* directory.

One inconsistency to be aware of rather than surprised by: `crate::audio` resolves
its asset root from a **different** variable, `LODESTONE_ASSET_ROOT`, and will not
follow `LODESTONE_ASSETS`. Unifying them is part of the same follow-up as
unifying the two object-store implementations.

`resources::load_panorama` is **fail-open** like every sibling loader: a jar-less
run, an unopenable object store, a missing face, or faces that disagree in size
logs a warning and leaves the menu on its flat backdrop rather than failing
startup. An *unpopulated* store is the softest failure of the set — it loads the
jar stubs and warns, so the game runs with a flat sky.

## Dependencies

- `lodestone-assets` for `ResourceManager` and `Image::decode_png` (which expands
  the greyscale and RGB faces to RGBA8 for us).
- `crate::asset_objects` for the asset-index → object-store resolution, and
  `serde_json` beneath it.
- `glam` for the matrices, `bytemuck` for the uniform, `wgpu` for the cube
  texture.
- `menu/render.rs` for the draw site; `resources.rs` for the loader; `xtask`'s
  `fetch-assets` for populating the store. Nothing in `app.rs` changed.

## Audio: the same store, and the actual remaining gap

**All 4871 game sounds live in this same asset index**, so audio and the panorama
are the same plumbing problem. `crate::audio` had a private copy of the index
reader (`find_asset_index`, `parse_asset_index`, `AssetObjectSource`); it was
**extracted into `crate::asset_objects` and audio now runs through that shared
type** rather than a second parser. Two readers of one index is exactly the drift
this repo forbids, and the extraction also handed audio a length check it did not
have before — a truncated object now reads as *absent* instead of reaching the Ogg
decoder as if it were a sample.

Migrated with the code: the index-parsing, object-path and `assets/`-prefix-strip
tests. `audio.rs` keeps a comment naming their new homes.

### What is actually missing, measured

Worth stating precisely, because the obvious guess is wrong in both directions:

| | state |
|---|---|
| `minecraft/sounds.json` | **present**, 626,160 B, parses to **1968** sound events |
| `.ogg` samples | **11 of 4871** present locally; the corpus is **375 MB** |
| `LODESTONE_ASSET_ROOT` | must be set, or audio is disabled by design |

So audio does **not** fail at startup for want of `sounds.json` — it is on disk and
`ShellAudio::load_from_root` gets past its eager check. The gap is the *samples*:
with 11 of 4871 present, the engine comes up, the registry resolves, and virtually
every event finds no object and plays nothing. That is the "connected but silent"
state `audio.rs`'s own module docs warn about, and it is a worse failure than a
hard error because it looks like it works.

`sounds.json` is in `REQUIRED_OBJECT_NAMES` anyway, so `fetch-assets` keeps it
verified rather than assuming it stays there.

### What closes it

`xtask::ensure_object` — *given a logical asset name, make the object present and
verify its SHA-1 against the index* — is the general primitive, and fetching the
sample corpus is a loop over the index's `.ogg` names calling it. It is
deliberately **not** wired into `fetch-assets`: 375 MB in a command every
contributor runs is a different decision from 3.2 MB, and unlike a stub a missing
sample fails honestly. That is a judgement to revisit, not an oversight.

One inconsistency to fix while you are there: audio resolves its root from
`LODESTONE_ASSET_ROOT` and everything else from `LODESTONE_ASSETS`, so a
correctly-configured pack can still have silent audio.

## Residual risk

**The within-face `(u, v)` orientation is not verified by any gate here.** The
face-order gate uses six solid colours, so it pins layer *selection* only; the
hermetic `assemble` test pins the flip on the CPU side. What neither can see is
whether wgpu's cube-face `(sc, tc) → (u, v)` mapping matches GL's for a given
layer. The argument that it does: the face-selection tables in the GL, Vulkan and
WebGPU specs are the same table, and wgpu's NDC has y **up** exactly as GL's does
(only the depth range differs), so there is no framebuffer flip to compensate for.
That argument is not a measurement.

This is now *observable*, which it was not while the faces were stubs: with real
art a within-face error shows up as a sky mirrored or rotated inside one face while
the faces themselves sit in the right places. The cheapest check is a human looking
at the title screen for a horizon that runs the wrong way.

## Tests

- `crates/lodestone-shell/src/asset_objects.rs` — hermetic: index parsing (with
  the real `panorama_0` hash and its 547,239-byte size as the fixture), rejection
  of an empty or all-unusable index, the two-character path fan-out, a short object
  reading as *absence* with a control proving the same store reads it when the
  index agrees, and refusing to guess between two indexes with a control that
  resolves after one is removed.
- `crates/lodestone-shell/src/menu/panorama.rs` — hermetic, no GPU: face order,
  the vertical flip (with a control proving an unflipped stack would fail it),
  size/squareness rejection, the index-key-versus-jar-path prefix split, all six
  keys distinct, `assemble` defaulting `from_object_store` to 0, the 36-vertex
  expansion, `wrap_degrees`, the 2°/s spin rate, and the `dim_for_screen` fork.
- `crates/lodestone-shell/tests/menu_panorama_pixels.rs` — four gates:
  - **face order** (`#[ignore]`, GPU): attaches a synthetic six-colour cubemap,
    renders the real `frame_for` title screen, asserts `panorama_0` straight ahead,
    `panorama_1` right, `panorama_3` left. Probe band derived from `title_slot` and
    checked against every widget rect; failure output prints a bounding box;
    negative control is `detach_panorama`, which must make all six colours vanish.
  - **real art** (`#[ignore]`, no GPU): `from_object_store == 6`, face size not a
    stub, and per-layer luminance stdev above 1.0 — predicted against the measured
    3.7 (flattest real face) versus a stub's exact 0.0. Includes a detector control
    over a deliberately flat buffer that must report 0.
  - **non-uniform sky** (`#[ignore]`, GPU): with the real cubemap bound the probe
    band's luminance stdev must exceed the detached backdrop's, and the control
    (detached) must itself be flat — checked *first*, because a non-flat "flat"
    backdrop would make the comparison meaningless.
  - **spin rate** (always runs, no GPU): 2°/s and a 180 s revolution, with the
    plausible-wrong hypothesis (delta read as seconds, not ticks) excluded by a
    factor of twenty.
