# World text and its background plate blend on gamma bytes

## What it is

The three world-space flat-colour text passes — entity nametags
(`gpu/nametag.rs`), sign text (`gpu/sign_text.rs`) and `text_display` glyphs and
panels (`gpu/display_text.rs`) — composite into a **raw, non-sRGB view** of the
same colour texture the rest of the world draws into, because vanilla is not
colour-managed and blends text and its background plate directly on the
framebuffer's stored gamma bytes. This is the world-space sibling of the HUD fix
`docs/tab-list.md` records; the mechanism differs because a world pass cannot
just be handed a second view, it needs a second *render pass*.

## How it works

### Why the three are one problem

All three passes share one shader — `crates/lodestone-shell/src/shaders/nametag.wgsl`,
a `view_proj` uniform, a flat vertex colour and no texture at all. Every colour
any of them submits is therefore a vanilla gamma byte:

| pass | the value |
|---|---|
| nametag | the plate, black at `ARGB.color(0.25F, -16777216)` = `0x40000000`; plus each span's resolved ink colour |
| sign text | `ARGB.scaleRGB(dye, 0.4)` for an unlit side, the full dye when glowing, or a run's own explicit colour |
| `text_display` | `DEFAULT_BACKGROUND_ARGB` (`0x3F000000`) or a synced `text_background_color`; plus the ink and its drop shadow |

Fixing one alone would leave a sign's dye and a nametag's plate visibly
disagreeing on the same screen, which is worse than a uniform error.

### The divergence, measured

A native swapchain here is viewed as sRGB (`lodestone_render::target`'s
`SurfaceTarget`), so `ALPHA_BLENDING` makes the hardware **decode** the
destination byte, blend in linear light and re-encode. Vanilla multiplies the
stored byte. For the nametag plate (black at `64/255`, so `0.749·bg`),
re-derived from the sRGB transfer function in a standalone script rather than
eyeballed:

| backdrop byte | 0 | 64 | 128 | 255 |
|---|---|---|---|---|
| `encode(0.749·decode(bg)) − 0.749·bg` | 0 | +7 | +16 | +33 |

Near-linear in backdrop brightness. **Black is the only fixed point** — white is
not, unlike the tab-list case, whose foreground is white and therefore agrees at
both ends. The plate reads too weak against sky and about right against stone at
night.

Confirmed live at `Bgra8UnormSrgb` against a rendered sky backdrop of **181**
(`tests/world_text_gamma_blend_pixels.rs`): the plate composites to **136**
through the raw view — exactly vanilla's `181 × 0.749` — and to **159** through
the sRGB view. Both hypotheses were computed from the sRGB standard and the
plate's own alpha constant before the measurement, and the two arms land on one
each.

### Why a separate render pass

A `wgpu` render pass fixes **one attachment format for every pipeline in it**.
There is no way to have the nametag pipeline blend on raw bytes while the
terrain, entity and item pipelines beside it keep the corrected view. The HUD's
first attempt at this changed the whole renderer's format and had to be reverted
because the flat-colour stream shared a pass with the sprite, glint and model
pipelines, which could then not draw at all — inventory icons and air bubbles
vanished. The world's answer is the same as the HUD's second attempt: give the
flat-colour geometry its own pass, on its own view, and leave everything else
alone.

`RenderState::render_inner` therefore encodes up to four passes where it used to
encode one:

| pass | attachment | contents |
|---|---|---|
| `block pass` | the target's own view | terrain, entities, block entities, banner layers |
| `world text pass` | **raw** view | sign text, then `text_display`'s four ranges |
| `block pass (translucent and overlays)` | the target's own view | beacon beams, end portal, translucent terrain, particles, weather, outline, debug lines, plugin billboards |
| `world nametag pass` | **raw** view | the nametag normal and see-through pipelines |

The two boundaries are where they are because **draw order is load-bearing** and
nothing about it changed:

- Sign text and `text_display` go after the block entities (a sign's board is
  real terrain, already in the depth buffer for the polygon-offset bias to win
  against) and **before** the translucent water, the particles and the weather.
  Moving them to the end would put a raindrop in front of a sign *behind* it.
- Nametags stay last of all the world's colour work.
- Within the nametag pass both pipelines still draw in one pass in the original
  order, so the plate still paints over the opaque normal-pass glyphs exactly as
  `SubmitNodeCollection`'s `nameTags, seeThroughNameTags` phase list does.

**Neither text pass is opened when it has nothing to draw.** An empty render
pass still stores and reloads a colour and a depth attachment, so a frame with
no signs, holograms or nametags encodes exactly the passes it always did.

### The format and the view are decided in one expression

`RenderState::set_world_text_view(device, frame)` does both: it derives the raw
counterpart with `RenderState::gamma_text_format` (`remove_srgb_suffix`),
re-points the three renderers' pipelines at it the first time it is called
(`set_color_format` on each — the pipelines only, keeping the jar-loaded font,
the ink caches and every vertex buffer), and stores `frame.create_view(raw)` for
that frame. A pipeline format and an attachment view that disagree is a `wgpu`
abort, and the HUD's own history is of exactly that pair living in two files and
drifting; asking the renderer that already knows the answer removes the chance.

The view is **taken**, not borrowed, by `render_inner`: a swapchain image is
presented at the end of the frame, so a view of it must not survive into the
next one. `app/redraw.rs` calls the setter once per frame, immediately before
`render_with_crack_and_effects`.

### The `"world"` GPU timing span

Splitting the block pass would have silently redefined the `"world"` timestamp
segment as "the first segment of the world". `GpuQueryTimer::writes_begin` /
`writes_end` write the two edges on two different passes instead, so the span
still covers all of the world's colour work with real begin-/end-of-pass
timestamps. Exactly one of the last two passes writes the end edge, chosen on
whether the nametag pass runs — a query written zero times resolves from
whatever the previous frame left in it.

## How to change it, and the gotchas

- **Do not tune the colour constants.** They are vanilla's own
  (`0x40000000`, `0x3F000000`, `scaleRGB(dye, 0.4)`) and were never the bug. A
  tuned constant fixes one backdrop and breaks every other, because the error is
  a function of the backdrop.
- **Adding a fourth flat-colour world-text pass?** Build its pipelines from the
  same `RenderState` format field and draw it inside one of the two raw passes,
  not in the block pass. A new pipeline built at the *target's* format and drawn
  in a raw pass is a validation abort at `set_pipeline`, not a colour bug.
- **A caller that never calls `set_world_text_view` keeps the old behaviour, on
  purpose.** Every headless pixel gate uses `Rgba8Unorm`, where the raw view and
  the target's view are the *same format*, so the blend there was always on
  gamma bytes and calling the setter changes nothing; `tests/capture_screenshots.rs`
  uses `Bgra8UnormSrgb` and does not call it, so its text draws through the
  target's own view exactly as before. If the pipelines have been re-pointed and
  a later frame supplies no view, `render_inner` drops the text for that frame
  and logs once rather than aborting the frame.
- **This is invisible to every existing world-text gate**, and that is
  structural rather than an oversight: `nametag_pixels`, `sign_text_pixels`,
  `text_display_pixels` and `world_text_over_geometry_pixels` all build
  `Rgba8Unorm` targets, so the corrected and raw views coincide for all of them.
  The whole corpus shares that one fixture value. `world_text_gamma_blend_pixels`
  exists because of it and asserts the formats differ before measuring anything.
- **The browser is already correct and must stay that way.** A WebGPU canvas
  structurally has no sRGB format — `wgpu`'s `WebSurface::get_capabilities`
  never lists one — so `config.format` there is already raw and
  `gamma_text_format` is a no-op on it. Naming a format the swapchain never
  declared in `view_formats` would be a validation abort on the one platform no
  gate here runs on, which is why the decision is asserted as a pure function
  for both shapes in `gpu::state`'s unit tests.

## Configuration

None. There is no option, no feature flag and no environment variable: the
behaviour follows entirely from the render target's own colour format.

## Dependencies

- `lodestone_render::target` — `RenderTarget::raw_view_format` and
  `AcquiredFrame::create_view`, which both target implementations already
  declare in `view_formats` up front, plus the `choose_view_format` decision and
  its gates.
- `crates/lodestone-shell/src/gpu/{state,frame,nametag,sign_text,display_text}.rs`.
- `crates/lodestone-shell/src/app/redraw.rs` — the one production caller.
- `crates/lodestone-shell/src/gpu/gpu_timing.rs` — the split `"world"` span.

## Verification

```bash
# the decision, no GPU
cargo test -p lodestone-shell --lib gpu::state::tests

# the blend, at production's own surface format
cargo test -p lodestone-shell --test world_text_gamma_blend_pixels -- --ignored --nocapture

# the passes that must not regress
cargo test -p lodestone-shell --no-fail-fast \
  --test nametag_pixels --test sign_text_pixels --test text_display_pixels \
  --test world_text_over_geometry_pixels --test container_item_pixels \
  --test container_item_pixels_scaled --test hotbar_special_item_pixels \
  --test hotbar_block_item_pixels --test air_bubble_pixels \
  --test container_background_pixels -- --ignored
```
