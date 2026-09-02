# Screen effects

## What it is

`lodestone_render::ScreenEffectRenderer` draws the client's full-screen and near-full-screen
post-hand-pass overlays: underwater tint and scroll, fire, a carved-pumpkin vignette, freezing in
powder snow, the spyglass scope, the nausea "confusion" swirl, and the nether/end portal swirl
(portal wins when both are active), plus the world-border warning's cyan vignette tint. In vanilla
these come from two different mechanisms — `ScreenEffectRenderer.submit` for underwater/fire, and
`Hud.extractCameraOverlays` for the rest — but all share one "textured, alpha-blended, screen-space
quad after the hand pass" shape, so this port draws all of them through one pipeline. Confusion and
portal additionally drive a world-space projection warp that lives in `camera.rs`, not in this pass.

## How it works

### The pipeline

One `wgpu::RenderPipeline`, one bind group (a texture plus a sampler, nothing else — no camera
uniform), draws every overlay: each quad is built directly in NDC on the CPU per frame and uploaded
as a small non-indexed triangle list, the same "rebuilt every frame" choice the sky pass makes. Each
draw opens its own render pass with `LoadOp::Load` (never `Clear`) and no depth attachment, since it
runs after the world, entities and the first-person hand and must not erase them or take a
depth-comparison sign it does not need. This is deliberately the one pass that must never be the one
to push an adapter over the renderer's 4-bind-group floor — see `docs/architecture.md`'s hard
constraints before adding a second texture here; prefer a second draw call over a second bind group.

### Per-effect mechanism

- **Underwater**: a flat grayscale tint (not blue — the blue cast is entirely the texture's own
  pixels) at a fixed low alpha, with UVs scrolling by look direction and tiled 4x4. The brightness
  term delegates to the same lightmap curve the terrain shaders use (`docs/lighting-and-sky.md`)
  rather than a second approximation. **The underwater tint and dimension fog are deliberately
  unrelated**: fog fades world geometry with view distance and is applied by the terrain pass; this
  overlay is a flat, non-fading, screen-space quad composited after the world and after fog has
  already been applied. Vanilla runs both at once when submerged and nothing here reads fog state.
- **Fire**: exactly two 1x1 quads, each translated and rotated to vanilla's own transform and then
  flattened from perspective to NDC (an orthographic flatten, not vanilla's 3D HUD projection, for
  the same "no camera uniform" reason every overlay here stays flat), scaled so the pair's combined
  width fills NDC. The texture is a 32-frame vertical strip sampled nearest/clamp, not linear/repeat,
  because it is independent frames rather than a tileable pattern.
- **Pumpkin**: not part of vanilla's `ScreenEffectRenderer` at all — it is vanilla's generic
  per-item `camera_overlay` component mechanism in `Hud`, and the carved pumpkin is simply the one
  item that ships it populated today. This port takes the one-entry version of that table (a direct
  helmet-slot item check) rather than building unused generality; a second item shipping the
  component would be the reason to generalise to a real lookup. The quad is static — built once, no
  per-frame tint or scroll — and its silhouette comes entirely from the texture's own alpha.
- **Freeze**: the same static-quad shape as pumpkin, but alpha tracks a real, already-existing,
  client-computed freeze percentage rather than a fixed `1.0`.
- **Spyglass**: vanilla branches on scoping *before* it reaches the generic per-slot overlay loop,
  with its own dedicated method and geometry — a centred lens sized by a scale factor plus four
  separate opaque letterbox bars covering whatever the lens doesn't, rather than a texture-table
  entry. The bars reuse the pipeline's own procedural 1x1 white texture tinted black, so no second
  texture or pipeline is needed for a flat fill. Spyglass also has an FOV-zoom half wired through the
  camera (see `docs/camera-and-view.md`), independent of this vignette.
- **Confusion and portal**: mutually exclusive in the overlay (portal wins outright, never
  blended), and separately tied together by a shared world-projection warp that rotates and shears
  the world's own projection matrix rather than drawing screen-space geometry — which is why it
  lives on `Camera`, not in this file. The two effects blend differently depending on which quantity
  is asked: overlay alpha takes the winning effect's raw value with no blend, warp amount is the max
  of the two, and warp *speed* is a weighted blend of vanilla's own per-effect constants. One scalar
  cannot drive all three through one multiply without being visibly wrong at the ends, which is why
  each intensity is carried raw rather than pre-multiplied into one "strength".
- **The portal curve is deliberately asymmetric**: the effect ramps in over four seconds while
  standing in a portal cell and decays over one second everywhere else — a 4:1 ratio, not a symmetric
  fade — because a symmetric curve reads as sluggish on exit and abrupt on entry. It is stepped at
  the tick rate rather than per frame (a per-frame ramp would be frame-rate dependent) and read back
  interpolated between the previous and current tick rather than sampled raw, which would paint a
  visible tick-stepped staircase across the ramp.

### Draw order and gating

This pass draws immediately after the first-person hand and before the HUD, matching vanilla's own
ordering. Gating is not one flag: pumpkin/spyglass/underwater/fire are first-person-and-not-spectator
only, while freeze/confusion/portal are spectator-gated but draw in third person too — vanilla's own
`Hud` method nests the first group inside a first-person check and leaves the second group as
siblings of it, so the two groups are tracked and re-checked separately rather than folded into one
bool, precisely so a freeze-only third-person frame cannot also fire a stale first-person-only flag.

### A session-scoped flag needs an explicit reset

Values fed into this pass from per-entity metadata (on-fire) or session state are sticky: they hold
their last reported value until a new packet contradicts it. Vanilla never hits this because a
respawn is a brand new entity on both client and server; this client keeps one long-lived local-player
entity across a whole session, so a respawn does not itself produce a contradicting packet. Any field
fed this way needs an explicit reset on the respawn path, written back to "no reading yet" rather than
a literal `false` — a literal would be inventing a report the server never sent. Whenever a new
metadata-fed session field is added, it needs to join that same reset arm; the failure otherwise is
silent, because the field's absence already reads as the safe default and nothing looks wrong until
the stale value actually diverges from reality.

### The world-border warning

Vanilla's border warning is not one of this module's overlays — it is a cyan tint applied to the
existing vignette texture inside `Hud.extractVignette`, derived from distance to the border, the
border's warning-blocks setting, and how fast the border is currently moving. It shares this doc
because it is the same family of "screen darkening keyed off world state" effect and because its
formula carries a real unit hazard worth recording: one of vanilla's own inputs to the "how fast is
the border moving" term is denominated in ticks, not milliseconds, and if a port stores that duration
in milliseconds instead the moving term comes out twenty times too small. It fails safe rather than
wrong: a `max` against the static warning-blocks floor still makes the tint appear at the right
distance, so only the *early* warning for an incoming shrink is foreshortened, and the static case
(no border currently resizing) is exact either way.

## How to change it

- **Bind groups**: see `docs/architecture.md` — this pass sits at the renderer's 4-bind-group floor
  by design; add a second draw call before reaching for a second bind group.
- **Gamma space, not linear**: the tint multiply happens in gamma space with an explicit
  linear-to-sRGB round trip around it, the same convention `docs/colour-and-tint.md` documents for
  the rest of the renderer. Only RGB goes through the round trip; alpha is coverage and is never
  gamma-encoded. Doing the multiply in linear light washes out both overlays, most visibly
  underwater's already-subtle tint.
- **Two independent gate groups, not one** — see "Draw order and gating" above. A new overlay's
  gating should be decided by checking which of vanilla's two `Hud` groups it belongs to, not by
  assuming it follows the majority.
- **The projection warp lives in `camera.rs`, not here** — a formula change to the confusion/portal
  warp or the spyglass FOV modifier touches the camera module and its one call site where the shared
  view-projection matrix is built each frame; nothing in this pass's own module needs to change for
  it. See `docs/camera-and-view.md`.
- **Frame count is read from the loaded texture, not hardcoded** — a resource pack shipping a
  differently-tall fire or portal strip still animates correctly, as long as the strip stays the
  vanilla-mandated width.
- **The letterbox bars' texture is procedural** — a 1x1 opaque-white texture built once at
  construction, with no backing asset. Reuse it for any future flat-colour fill rather than adding a
  second procedural texture.

## Configuration

None. Every texture loads from whichever `client.jar`/resource pack the renderer's asset root
already resolves, the same as the sky pass, and loading is fail-open: a jar-less run or a pack
missing one of these textures leaves the renderer with no overlay pass installed rather than failing
to start. There is no env var or flag specific to this pass.

## Dependencies

- `lodestone-render`'s screen-effects module — the pure per-effect geometry functions and the
  GPU-owning `ScreenEffectRenderer` with one draw method per effect.
- `lodestone-render`'s camera module — the shared confusion/portal world-projection warp and the
  spyglass FOV modifier (`docs/camera-and-view.md`).
- The shell's GPU state — holds the optional `ScreenEffectRenderer`, dispatches each effect's draw
  call inside the main render pass behind its own gate, and tracks per-effect "did this draw" stats
  used by its own pixel-level tests.
- The shell's per-frame input construction — computes each effect's live value (eye-in-water,
  on-fire, wearing-pumpkin, freeze percentage, spyglass scoping, nausea/portal intensity, spectator
  state) from session and simulation state each frame and feeds it into the render call.
- `lodestone-physics`'s player state — owns the freeze mechanic (ticks frozen, percent frozen),
  consumed here rather than duplicated.
- `docs/lighting-and-sky.md` — the lightmap curve underwater's brightness term reuses.
- `docs/colour-and-tint.md` — the gamma-space tint/shade convention this pass follows.
- `docs/architecture.md` — the bind-group budget and other renderer-wide hard constraints this pass
  is built against.
