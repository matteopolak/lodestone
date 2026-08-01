# The underwater and fire screen overlays

## What it is

Two full-screen post-hand-pass overlays, issues #108 and #112: a blue-ish
tint plus a scrolling `misc/underwater.png` texture when the camera's eye is
submerged, and a looping flame texture across the bottom of the screen while
the local player is on fire. Vanilla draws both from one class,
`ScreenEffectRenderer.submit` (`.cache/mc/26.2/client-src/net/minecraft/
client/renderer/ScreenEffectRenderer.java:55-83`), so they landed as one pass:
`lodestone_render::ScreenEffectRenderer`.

## How it works

### The pipeline (`crates/lodestone-render/src/screen_effects.rs`)

One `wgpu::RenderPipeline` draws both overlays — vanilla's own two pipelines,
`BLOCK_SCREEN_EFFECT` and `FIRE_SCREEN_EFFECT`
(`RenderPipelines.java:713-718`), are textually identical builds of the same
`GUI_TEXTURED_SNIPPET` base (`RenderPipelines.java:153-161`: position+uv+colour
vertex format, `BlendFunction.TRANSLUCENT`, no depth state), so there is
nothing to differentiate beyond which texture bind group is active.

**One bind group** — a texture + sampler, nothing else. There is no camera
uniform at all: both quads are built directly in NDC (`x, y` in
`-1.0..1.0`) on the CPU each frame and uploaded as a plain (non-indexed)
6-vertex-per-quad list, the same "small, CPU-rebuilt every frame" choice
`sky_pipeline.rs`'s disc/star passes make. This is deliberately **the**
place to add a pass that must never be the one to push an adapter over
wgpu's 4-bind-group floor — see "Bind groups" below.

Both `ScreenEffectRenderer::draw_underwater`/`draw_fire` open their **own**
render pass with `LoadOp::Load`, never `Clear` — they run after the world,
entities and the first-person hand, and must not erase them. No depth
attachment either, for the same reason `sky_pipeline.rs` gives: this
project's depth is `[0,1]` DirectX-style, not vanilla's reversed-Z, and a
pass that takes no depth attachment cannot get a comparison sign backwards
because there is no comparison.

### Underwater: texture, scroll and tint

`submitWater` (`ScreenEffectRenderer.java:155-166`) tints `underwater.png`
by `ARGB.colorFromFloat(0.1F, brightness, brightness, brightness)` — **not
blue**. The blue cast the real texture has comes entirely from the PNG's own
pixels; the vertex colour is a flat grayscale at a fixed `0.1` alpha. UVs
scroll by look direction: `u0 = -yRot/64`, `v0 = xRot/64`, tiled 4×4
(`uvSize = 4.0F`). `underwater_overlay_quad`/`underwater_overlay_triangles`
transcribe this exactly, including vanilla's slightly surprising
far-corner-first UV assignment in `buildQuad` (see the code comment there —
it is checkable against the source line, not renamed for clarity).

`brightness` is vanilla's `Lightmap.getBrightness`, a per-dimension
gamma-corrected curve table this codebase has not ported. `underwater_brightness`
approximates it by reusing the **same** `0.2 + 0.8 * max(sky, block)` floor
the block shader's `light_term` already applies to packed light
(`model_pipeline.rs`), rather than inventing a second curve shape. `packed_light`
comes from `RenderState::entity_light` (`self.entity_light.sample(camera.position)`
in `render_inner`) — the same per-frame sampler the entity pass already
uses, at the camera's eye position rather than a mob's feet.

### Underwater tint vs. dimension fog — they are unrelated, on purpose

This was the open question the briefing asked to settle: **both exist, and
they are not the same mechanism.** `crate::fog` (this codebase) fades *world
geometry* toward a colour with view distance — it changes what the terrain
pass outputs. `ScreenEffectRenderer.submitWater` is a flat, non-fading,
**screen-space** quad composited *after* the world, the hand and (in this
port) after the fog has already been applied to whatever the block pass
drew. Vanilla runs both at once when submerged (short water fog *and* the
tinted overlay); nothing here changes `fog.rs`, and this pass does not read
`FogSettings` at all.

### Fire: a real animated texture, a simplified placement

`submitFire` (`ScreenEffectRenderer.java:168-180`) draws two quads sampling
`ModelBakery.FIRE_1` (`"block/fire_1"`, `ModelBakery.java:50`), each
translated `±0.24` on X, `-0.3` on Y, and rotated `∓10°` about Y, at vertex
colour `-436207617` = ARGB `(229, 255, 255, 255)` (white, alpha `229/255` —
`FIRE_TINT` in this port). `fire_1.png` is a **16×512** strip: 32 stacked
16×16 frames (`fire_frame_count`), vanilla's default animation metadata
(`fire_1.png.mcmeta` is `{"animation": {}}`, i.e. one frame per tick,
looping) — this is genuinely the "looping flame texture" issue #112 asks
for, not a hand-authored UV scroll.

**Placement is a deliberate simplification.** Reproducing vanilla's exact
`PoseStack`/`hud3dProjection` transform for two small rotated 3-D quads would
buy nothing here — there is no other 3-D content in this pass to interact
with — so `fire_overlay_triangles` instead tiles [`FIRE_TILE_COUNT`] (4)
plain NDC quads across a horizontal strip from the bottom edge up to
[`FIRE_STRIP_TOP`] (`y = -0.3`, i.e. the bottom ~35% of the frame), which is
what the issue's own description asks for ("flame texture across the bottom
of the screen"). Alternate tiles are horizontally mirrored purely so four
copies of one 16×16 frame do not read as an obviously repeated stamp — a
cosmetic choice vanilla's two-quad layout has no need for. The **texture,
its 32-frame animation and the alpha blend are all real**; only the
silhouette's exact screen position differs from vanilla's.

The strip samples with `FilterMode::Nearest` and `AddressMode::ClampToEdge`,
not `Linear`/`Repeat` like the underwater texture: `fire_1.png` is a strip of
independent frames, not a tileable pattern, so linear filtering or
wraparound at a frame's top/bottom edge would blend in the neighbouring
frame.

### Draw order and gating

`GameRenderer.java:568-577`: the hand pass, then
`screenEffectRenderer.submit`, then the HUD/feature renderers. This port's
overlay draw sits in `RenderState::render_inner`, immediately after
`draw_first_person_hand` and before `queue.submit` — the shell's own HUD
draws afterward, in a separate pass in `app.rs`, so the ordering matches.

Gating mirrors `ScreenEffectRenderer.submit`'s
`isFirstPerson && !isSleeping && !isSpectator`:
[`ScreenEffects::any_active`] checks first-person (`!stats.third_person_body_drawn`
— reusing the existing signal the hand pass already computes, rather than a
second "am I first person" input) and not-spectator. There is no "sleeping"
conjunct: this crate has no sleeping state yet, and its absence can only be a
false *negative* miss (a sleeping player who should not see the overlay but
does), never a false positive that hides a working feature — see
`spectator_suppresses_both_overlays` and the `any_active` unit tests in
`crates/lodestone-shell/src/gpu/screen_effects.rs`.

## What does not reach the shell yet: the on-fire flag

**This is the one gap issue #112 flagged as the real risk, and it is real.**
The shared-entity-flags byte (`Entity.FLAG_ONFIRE = 0`,
`Entity.java:261`) **does** decode: `protocol/v770/src/packets/metadata.rs`
already parses `IDX_SHARED_FLAGS` into `EntityMetadata::flags: Option<u8>`
(`decodes_air_supply_at_index_1`'s sibling test at line 592 pins index 0 to
`0x01`), and `lodestone-ecs/src/ingest.rs::apply_entity_metadata` already
folds it into a generic `EntityFlags(u8)` ECS component for **any** tracked
entity (line 567-568).

It does not reach the **local player**, and this is by explicit design, not
an oversight: `apply_local_player_login`'s own doc
(`lodestone-ecs/src/ingest.rs:156-165`) states that the local player
deliberately gets *only* `MinecraftEntityId` and `Attributes` — no
`EntityKind`/`Position`/`Rotation`/`HeadYaw` — specifically *because*
`lodestone_client::state::entity_view` requires all four and its absence is
what keeps the local player out of `ClientHandle::entities()` (a self-model
would otherwise render at the camera's own eye). So even though
`apply_entity_metadata` does set `EntityFlags` on the local player's own ECS
entity (the event is in `ingest::handles_event`'s routing switch, and
`apply_local_player_login` does index our own id), `entity_view()` can never
surface it: the early `?` on `EntityKind` returns `None` before `flags` is
even read.

This is exactly the shape `air_supply` was in before issue #60: metadata
that arrives for any entity but has to reach the **session**-scoped
`PlayerSnapshot`, not the generic per-entity view. `air_supply` got a
dedicated fold, `apply_local_player_air_supply`
(`lodestone-ecs/src/ingest.rs:617-639`), off the *same* `EntityMetadataUpdated`
event, writing into `crate::session::Vitals` instead. **No equivalent fold
exists for the on-fire bit.**

`crates/lodestone-shell/src/gpu/screen_effects.rs::ScreenEffects::on_fire` is
wired all the way through the render pass (see the pixel gate below), but
`app.rs`'s real per-frame call always passes `on_fire: false`, with a comment
pointing back here. This is not a placeholder pretending to work — the
render mechanism is real and gated correctly, but there is genuinely no data
to feed it yet.

### The patch this needs (spec only — not applied; touches forbidden crates)

1. **`lodestone-ecs/src/session.rs`**: add a field to `Vitals` (or a sibling
   component), e.g. `on_fire: Option<bool>`, following `air`'s own doc comment
   there almost verbatim (`Vitals::air`'s doc already explains why this class
   of field is session-scoped rather than folded by `apply_local_player_state`).
2. **`lodestone-ecs/src/ingest.rs`**: a new system,
   `apply_local_player_on_fire`, copy-shaped from
   `apply_local_player_air_supply` (lines 617-639): same
   `Query<&mut Vitals, With<LocalPlayer>>`, same `EntityIndex` lookup, but
   reading `metadata.flags` and testing bit `0x01`
   (`lodestone_entity::SharedEntityFlags::from_bits(flags as i8).on_fire()`
   already exists and does exactly this — see
   `crates/lodestone-entity/src/metadata.rs:206-224`) instead of
   `metadata.air_supply`. Register it next to `apply_local_player_air_supply`
   in the same system set (line ~783).
3. **`lodestone-client/src/state.rs`**: add `on_fire: bool` to
   `PlayerSnapshot`, folded from `Vitals::on_fire` in `ClientHandle::player()`
   the same way `air` is (`vitals.on_fire.unwrap_or(false)` — unreported reads
   as "not on fire", the safe default, unlike `air`'s "reads as full").
4. **`crates/lodestone-shell/src/app.rs`**: change the one line
   `on_fire: false` (in the `redraw()` `ScreenEffects` construction, next to
   `eye_in_water`) to read the new `PlayerSnapshot::on_fire` off
   `self.sim.net()`'s shared handle — the same shape the new `spectator`
   lookup added alongside it already uses.

No change needed in `lodestone-shell/src/gpu.rs`, `gpu/screen_effects.rs`, or
`lodestone-render` — the render half of this feature does not know or care
where `on_fire` came from.

## How to change it, and the gotchas

- **Bind groups: check the limit, not the adapter.** This pass has exactly
  one bind group and must stay that way — see `sky_pipeline.rs`'s own
  module doc for the concrete failure mode (a 5th model-shader bind group
  validates on this machine's M5, which reports 8, and crashes on any
  4-group adapter). If a future change needs a second texture, prefer a
  second draw call over a second bind group before reaching for more state.
- **No double quotes in `OVERLAY_WGSL`, ever** — it lives in a Rust `r"…"`
  raw string; use backticks in shader comments.
- **Gamma space, not linear.** `OVERLAY_WGSL`'s fragment shader round-trips
  the sampled texel through `linear_to_srgb`/`srgb_to_linear` around the tint
  multiply, the same pattern `model_pipeline.rs`/`entity_pipeline.rs` use.
  Only the tint's RGB goes through the round-trip; alpha is coverage, not
  colour, and is never gamma-encoded. Doing the tint multiply in linear light
  instead would wash out both overlays (most visibly the underwater tint,
  whose alpha is already a subtle `0.1`).
- **`render`/`render_with_crack` are unchanged.** Rather than adding a new
  required parameter to two widely-called methods (~15 existing test files),
  the overlay input is a separate pair of methods,
  `render_with_effects`/`render_with_crack_and_effects`, that forward to the
  same private `render_inner` with a real `ScreenEffects` instead of
  `ScreenEffects::default()`. Only `app.rs`'s two real per-frame call sites
  were switched over; every other caller (every existing pixel-gate test)
  is untouched and still gets `ScreenEffects::default()` — no overlay, the
  pre-existing behaviour.
- **`ScreenEffects` is a plain argument, not a `*Source`.** Unlike
  `EntityLightSource`/`SkyDarkenSource`/etc. in `gpu/sources.rs`, this is not
  a boxed closure installed once at connect time. Those exist because
  `RenderState` has no way to reach `Sim` or the network handle itself;
  `eye_in_water`/`spectator`/`tick` are already synchronously available
  wherever `app.rs` calls `render`, the same place `outline` is computed —
  see `gpu/screen_effects.rs`'s module doc.
- **The headless single-frame smoke path (`run_headless` in `app.rs`) still
  calls plain `render`, not `render_with_effects`.** That path has no live
  water/fire state to feed anyway (`Sim::with_demo_world`, no network) and
  exists to prove the acquire→record→submit→present path works, not to
  exercise every feature — left alone deliberately, not missed.
- **Frame count is read from the texture, not hardcoded.** `fire_frame_count`
  is `image.height / 16`, computed once at `ScreenEffectRenderer::new` and
  cached (`ScreenEffectRenderer::fire_frame_count()`). A resource pack with a
  differently-sized `fire_1.png` still animates correctly; it would only
  break if a pack used a non-16px-wide strip, which vanilla's own asset
  pipeline does not support either.

## Configuration

None. Both textures load from whichever `client.jar` `crate::resources::asset_root`/
`open_client_jar` already resolve (same as the sky pass); there is no env var
or flag specific to this pass. `crate::resources::load_screen_effects` is
`None`-returning and fail-open exactly like `load_sky`: a jar-less run or a
pack missing either texture leaves `RenderState` with no overlay pass
installed, not a startup failure.

## Dependencies

- `lodestone-assets::screen_effects` — `load_underwater_texture`/
  `load_fire_texture`/`fire_frame_count`, the same "plain unatlased `Image`"
  loader shape `sky::load_cloud_texture` uses, and for the same reason: each
  texture's own addressing (4×4 tiling with wraparound for the underwater
  texture; independent-frame-slice, no wraparound for the fire strip) would
  conflict with an atlas's per-sprite padding.
- `lodestone-render::screen_effects` — the pure geometry
  (`underwater_overlay_quad`/`triangles`, `fire_overlay_triangles`,
  `underwater_brightness`) and the GPU-owning `ScreenEffectRenderer`.
- `lodestone-shell::gpu` — `RenderState::screen_effects`,
  `install_screen_effects`/`has_screen_effects`, the draw call inside
  `render_inner`, and `gpu::screen_effects::ScreenEffects` (the per-frame
  argument type).
- `lodestone-shell::resources::load_screen_effects` — the `client.jar` IO,
  mirroring `load_sky` exactly.

## The gates, and what they printed

Both are `#[ignore]`d GPU gates (no GPU on CI); run with `--ignored
--nocapture`. Every number below was actually run on this machine, not
predicted.

**Pipeline level** (`cargo test -p lodestone-render --test
screen_effects_pipeline_gpu -- --ignored --nocapture`) — proves the pass
paints pixels at all, via a synthetic in-memory pack, independent of the
shell:

| test | result |
|---|---|
| `control_an_untouched_target_reads_back_as_black` | pass (negative control) |
| `underwater_overlay_paints_the_whole_frame` | pass |
| `fire_overlay_paints_only_the_bottom_strip` | pass, with an explicit top/bottom row-band split (not a frame average) |

**Shell level** (`cargo test -p lodestone-shell --test screen_overlay_pixels
-- --ignored --nocapture`) — proves `RenderState::render_inner` actually
checks `self.screen_effects`/`ScreenEffects`, through the real
`render_with_effects` path, with executed negative controls:

```text
=== underwater overlay pixel gate (through RenderState::render_with_effects) ===
eye_in_water=true: underwater_overlay_drawn=true, differs from dry control by 100.0%
control A (installed, eye_in_water=false): underwater_overlay_drawn=false
control B (not installed, eye_in_water=true): underwater_overlay_drawn=false, differs from wet by 100.0%

=== fire overlay pixel gate (through RenderState::render_with_effects) ===
on_fire=true: fire_overlay_drawn=true, top rows differ 0.6%, bottom rows differ 100.0%
control (installed, on_fire=false): fire_overlay_drawn=false

=== spectator control ===
spectator=true, eye_in_water=true, on_fire=true: underwater_overlay_drawn=false, fire_overlay_drawn=false
```

The fire gate's `top rows differ 0.6%` (not exactly `0.0%`) is the strip's
own top edge landing on a partial pixel row (`FIRE_STRIP_TOP = -0.3` does not
divide the frame height evenly) — well under the `< 1%` assertion, and
exactly the kind of thing a frame-average could have hidden and a row-band
bounding-box check catches.

**Every control above is executed, not described**, per `CLAUDE.md`'s rule
that an absence assertion is only as good as evidence the detector would
have fired: control A and the spectator test prove the *flag* gates the
draw (not just installation), and control B proves an uninstalled pass never
draws regardless of the flag.

**What these gates do not, and cannot, prove**: that production ever passes
`on_fire: true`. `app.rs` always passes `false` — see "What does not reach
the shell yet" above. The gates prove the mechanism; the mechanism is simply
never told the truth yet.
