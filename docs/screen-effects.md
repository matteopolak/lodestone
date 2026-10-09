# Screen effects

## What it is

`lodestone_render::ScreenEffectRenderer` draws the full-screen overlays after the hand pass: underwater tint and scroll, fire, the carved-pumpkin vignette, powder-snow freezing, the spyglass scope, the nausea swirl, the nether/end portal swirl (portal wins when both are active), and the world-border warning's cyan vignette. Most share one alpha-blended textured pipeline; the border warning uses the same layout with a multiply-blend pipeline. Nausea and portal also drive a world-projection warp that lives in `camera.rs`, not here.

## How it works

### Pipeline

Two `wgpu::RenderPipeline`s share one bind-group layout (texture plus sampler, no camera uniform) and one shader, so the pass stays under the renderer's 4-bind-group floor. Overlays use standard alpha blending; the border warning uses `ZERO` / `ONE_MINUS_SRC_COLOR` for RGB and keeps destination alpha, making the vignette a multiply mask. Each quad is built in NDC on the CPU and uploaded as a small non-indexed triangle list. Every draw opens its own pass with `LoadOp::Load` and no depth attachment, since it runs after the world, entities and hand.

### Effects

- **Underwater**: a flat grayscale tint (the blue is in the texture) at low alpha, UVs scrolling by look direction and tiled 4x4, brightness from the terrain lightmap curve ([lighting and sky](lighting-and-sky.md)). It is unrelated to dimension fog, which the terrain pass applies; both run when submerged and nothing here reads fog state.
- **Fire**: two 1x1 quads, each transformed per the reference and flattened orthographically to NDC (no camera uniform), scaled so the pair fills NDC width. The texture is a 32-frame vertical strip sampled nearest/clamp (independent frames, not tileable). Frame count is read from the loaded texture, so a differently tall pack strip still animates (width must stay vanilla's).
- **Pumpkin**: the reference's per-item camera-overlay mechanism, of which the carved pumpkin is the one populated item; this is a direct helmet-slot check (generalise only if a second item ships the component). A static quad whose silhouette comes from the texture alpha.
- **Freeze**: the pumpkin's static quad with alpha tracking the client freeze percentage.
- **Spyglass**: branches on scoping before the generic overlay loop: a centred lens sized by a scale plus four opaque letterbox bars, reusing the pipeline's procedural 1x1 white texture tinted black (reuse it for any flat fill). Its FOV zoom is wired through the camera ([camera and view](camera-and-view.md)).
- **Nausea and portal**: mutually exclusive as an overlay (portal wins outright, no blend), tied together by a shared projection warp rotating and shearing the world matrix. Overlay alpha is the winning effect's raw value, warp amount the max of the two, warp speed a weighted blend of per-effect constants, so each intensity is carried raw rather than pre-multiplied into one strength.
- **Portal curve** is asymmetric: ramp in over four seconds in a portal cell, decay over one second elsewhere (a symmetric fade reads sluggish on exit and abrupt on entry). It steps at tick rate and is read interpolated between previous and current tick (raw sampling paints a staircase).
- **World-border warning**: `Sim::world_border_warning` yields distance, threshold and strength from server-derived border state; `WindowApp::redraw` samples it once, sends strength to `ScreenEffects` and reuses the tuple for the debug HUD. Positive strength draws `textures/misc/vignette.png` with the multiply pipeline, source tint `[1-strength, 1, 1, 1]`. Strength derives from distance, the warning-block setting and the border's speed; one input is in ticks, not milliseconds (storing ms makes the moving term twenty times too small, masked for a stationary border by the static floor, so tests need an incoming shrink). Settled brightness is `1.0`; ambient-light vignette darkening is not implemented.

### Order and gating

The pass draws right after the first-person hand and before the HUD. Gating has three independent groups, tracked separately so a freeze-only third-person frame cannot fire a stale first-person flag: pumpkin, spyglass, underwater and fire are first-person and non-spectator only; freeze, nausea and portal are spectator-gated but draw in third person; the border warning describes the world, so camera mode and spectator do not suppress it. `RenderStats::border_warning_overlay_drawn` records its draw separately. Decide a new overlay's group by which reference HUD group it belongs to.

### Session-scoped flags

Metadata-fed values (on-fire) and session state hold their last reported value until a packet contradicts them. The reference creates a new entity per respawn; this client keeps one local-player entity all session, so a respawn sends no contradiction. Every such field needs an explicit reset on the respawn path back to "no reading yet" (not a literal `false`, which invents a report). A new metadata-fed field must join that reset arm; the failure is silent because absence reads as the safe default.

## How to change it

- Bind groups: this pass is at the floor by design ([architecture](architecture.md)); add a draw call before a bind group.
- The tint multiply runs in gamma space with an explicit linear-to-sRGB round trip around RGB only (alpha is coverage); linear washes out both overlays ([colour and tint](colour-and-tint.md)).
- A confusion/portal warp or spyglass FOV change touches `camera.rs` and its one call site where the shared view-projection is built, not this module.
- Optional art owns an optional pass: nausea's bind group and vertex buffer are one `TexturedOverlay`, allocated only when its image decodes; `draw_confusion` returns whether it submitted. Missing art must not create a substitute texture or disable other effects.

## Configuration

Textures load from the resource-pack stack (built-in source `lodestone-resources.zip`). Nausea art is optional (missing skips confusion only); the other images are required by `ScreenEffectRenderer::new`, and the shell leaves the renderer uninstalled if one is missing. No env var or flag; the inactive Show Vignette menu row is not wired and does not control the warning.

The ignored `hud` gate `builtin_optional_texture_pixels` uses the staged archive with cloud and nausea reads withheld, drives the production path, checks a projected sun region, the fullbright water blend and fire's arithmetic extent, and verifies no confusion submission (withholding fire art is the negative control): set `LODESTONE_ASSETS` to the archive directory and run `cargo test -p lodestone-shell --test hud builtin_optional_texture_pixels -- --ignored --nocapture`.

## Dependencies

`lodestone-render`'s screen-effects module (geometry functions and `ScreenEffectRenderer`) and camera module (warp, spyglass FOV); the shell's GPU state (draws behind per-effect gates, per-effect stats) and per-frame input construction (eye-in-water, on-fire, pumpkin, freeze, scoping, nausea/portal intensity, border strength, spectator); `lodestone-physics` player state (freeze mechanic); [lighting and sky](lighting-and-sky.md), [colour and tint](colour-and-tint.md), [architecture](architecture.md).
