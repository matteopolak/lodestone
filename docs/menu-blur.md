# The background blur behind an open in-game menu

## What it is

Vanilla blurs whatever is already on screen behind most menu screens before
drawing the screen's own widgets on top — a six-pass separable box blur,
`Screen::extractBackground` → `extractBlurredBackground` →
`GuiRenderState::blurBeforeThisStratum` → `GameRenderer::processBlurEffect`
running the `minecraft:blur` post chain
(`assets/minecraft/post_effect/blur.json`). This client had the accompanying
dim wash (`MenuBackdrop::Dim`) but not the blur; this is the missing half —
[`crates/lodestone-shell/src/menu/render/blur.rs`](../crates/lodestone-shell/src/menu/render/blur.rs)
plus
[`src/shaders/menu_blur.wgsl`](../crates/lodestone-shell/src/shaders/menu_blur.wgsl).

## How it works

`MenuBlur` (owned by `MenuRenderer`, built eagerly since it needs no atlas,
font or jar) runs a **six-pass box blur** — three horizontal+vertical pairs,
matching `blur.json`'s own structure — over the texture backing the current
frame, immediately before `MenuRenderer::draw` paints a frame's widgets:

1. `MenuRenderer::begin_frame` captures the texture behind this frame's
   render target once, right after the frame is acquired (`app/redraw.rs`,
   before any `render`/`render_overlay` call this frame) — see that method's
   own doc for why a single per-frame capture was chosen over threading a new
   parameter through every `render_overlay` call site.
2. `MenuRenderer::draw` runs `MenuBlur::run` when the frame it is about to
   draw has `MenuFrame::blur == true` **and** the pass is a `render_overlay`
   (`Load`) pass — never a `Clear`-pass screen, which has nothing behind it
   yet.
3. `MenuBlur::run` copies the captured texture into a scratch texture, then
   alternates five passes between two ping-pong scratch targets, letting the
   sixth (a vertical pass) write straight into the caller's real render
   target — no seventh copy back.

The box filter itself (`menu_blur.wgsl`) is hand-expanded from
`assets/minecraft/shaders/post/box_blur.fsh`, not transliterated: bilinear
sampling lets one tap cover two texels, so the loop advances in steps of two
starting half a texel off-centre, with the leftover odd tap folded back in at
half weight. The radius (`BLUR_RADIUS = 5.0`) is vanilla's own
`Options.BLURRINESS_DEFAULT_VALUE`, `0..=10`, default `5`. Vanilla animates
nothing here — `extractBlurredBackground` applies the option's raw value
every frame with no fade-in — so neither does this port.

**`MenuFrame::blur` is a separate axis from `MenuBackdrop`, not implied by
`Dim`.** Vanilla's real fork is `Screen::isInGameUi()`: `false` for
Pause/in-world Options/Statistics/Social/Server Links/the in-world
resource-pack prompt (blur runs), `true` for `AbstractContainerScreen` and its
sign-edit/command-block-edit siblings (flat translucent gradient only, no
blur) — even though both groups use `MenuBackdrop::Dim` in this client. Each
overlay-frame builder therefore sets `blur` by hand, the same way each already
sets `backdrop` by hand.

## How to change it

- The radius is a constant (`menu/render/blur.rs::BLUR_RADIUS`), not a live
  setting — `crate::config::Options` is outside this feature's file ownership
  boundary for the session that built it. Wiring a real
  `menuBackgroundBlurriness`-equivalent option is a matter of threading a
  radius into `MenuBlur::config_h`/`config_v` instead of the constant.
- To add blur to a new overlay screen, set `frame.blur = true` in that
  screen's `*_overlay_frame` builder (`menu/nav.rs`), the same place its
  `backdrop` is already set to `Dim`. To keep a new overlay screen *without*
  blur (an `isInGameUi() == true`-shaped screen), simply never set the flag —
  it defaults to `false`.
- The pass uses exactly two bind groups (the sampled texture+sampler, and the
  direction/radius uniform), checked in `blur.rs`'s own
  `bind_group_count_is_within_the_four_group_floor` test against
  `wgpu::Limits::downlevel_webgl2_defaults().max_bind_groups` rather than this
  machine's adapter (see `CLAUDE.md`'s own note on the model shader's
  4-bind-group floor).

## Configuration

None exposed to players yet — see "How to change it" above.

## Dependencies

`wgpu` only. `lodestone_render::AcquiredFrame::colour_texture` (added
alongside this feature) supplies the source texture for both the real
swapchain path and a `HeadlessTarget`-based GPU test — unlike
`AcquiredFrame::texture`, which is deliberately `None` for a headless target.
`HeadlessTarget::USAGE` now also carries `COPY_DST` so a test can seed known
content into it before exercising a pass that reads "whatever was already
drawn".
