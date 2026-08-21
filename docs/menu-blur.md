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
half weight. The radius is the **live** `options.menuBackgroundBlurriness`
(`IntRange(0, 10)`, default `5` — vanilla's `Options.BLURRINESS_DEFAULT_VALUE`,
which `BLUR_RADIUS = 5.0` still supplies as the boot value). Vanilla animates
nothing here — `extractBlurredBackground` applies the option's raw value
every frame with no fade-in — so neither does this port.

**Radius `0` skips the pass entirely**, which is vanilla's own gate rather than
an optimisation bolted on top: `Screen.extractBlurredBackground` calls
`blurBeforeThisStratum()` only at `blurRadius >= 1.0F`, and a zero-radius box
filter is an identity convolution — running it would be six full-screen passes
that reproduce the source. The option's stringifier agrees: it is
`genericValueOrOffLabel`, so `0` reads **OFF**, not `0`.

**`MenuFrame::blur` is a separate axis from `MenuBackdrop`, not implied by
`Dim`.** Vanilla's real fork is `Screen::isInGameUi()`: `false` for
Pause/in-world Options/Statistics/Social/Server Links/the in-world
resource-pack prompt (blur runs), `true` for `AbstractContainerScreen` and its
sign-edit/command-block-edit siblings (flat translucent gradient only, no
blur) — even though both groups use `MenuBackdrop::Dim` in this client. Each
overlay-frame builder therefore sets `blur` by hand, the same way each already
sets `backdrop` by hand.

## How to change it

- The radius is live. `app/redraw.rs` polls
  `MenuNav::options().menu_background_blurriness` once per presented frame,
  beside `MenuRenderer::begin_frame`, and forwards it through
  `MenuRenderer::set_blur_radius` → `MenuBlur::set_radius`. The two config bind
  groups (`MenuBlur::config_h`/`config_v`) are rebuilt lazily inside
  `MenuBlur::run` when the value has actually moved, so the per-frame poll costs
  one float comparison rather than two buffer allocations. `BLUR_RADIUS` is now
  only the boot value.

  A poll, not a push, for the reason `Sim::set_cutout_leaves` is: the setting is
  written from two different settings pages (Video and Accessibility, both of
  which vanilla also carries), and a poll cannot forget one of them.
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

`options.menuBackgroundBlurriness`, persisted as
`crate::config::Options::menu_background_blurriness` (`IntRange(0, 10)`,
default `5`, `0` = OFF). Two rows drive it — Video and Accessibility — as in
vanilla.

## Dependencies

`wgpu` only. `lodestone_render::AcquiredFrame::colour_texture` (added
alongside this feature) supplies the source texture for both the real
swapchain path and a `HeadlessTarget`-based GPU test — unlike
`AcquiredFrame::texture`, which is deliberately `None` for a headless target.
`HeadlessTarget::USAGE` now also carries `COPY_DST` so a test can seed known
content into it before exercising a pass that reads "whatever was already
drawn".
