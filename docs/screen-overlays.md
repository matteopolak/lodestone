# The screen overlays: underwater, fire, pumpkin, freeze, spyglass, confusion, portal

## What it is

Seven full-screen (or near-full-screen) post-hand-pass effects, issues #108,
#112, #185, #139, #154, #144 and #149: a blue-ish tint plus a scrolling
`misc/underwater.png` texture when the camera's eye is submerged, a looping
flame texture across the bottom of the screen while the local player is on
fire, a static full-screen `misc/pumpkinblur.png` vignette while a carved
pumpkin is worn in the helmet slot, a `misc/powder_snow_outline.png`
vignette that ramps in with `Entity.getPercentFrozen()` while freezing in
powder snow, a `misc/spyglass_scope.png` lens with four black letterbox bars
while scoping with a spyglass, a green-tinted `misc/nausea.png` "confusion"
overlay while the Nausea effect is active, and an animated
`block/nether_portal.png` swirl while near a nether/end portal (portal takes
priority over confusion when both are active). Underwater and fire come from
one vanilla class, `ScreenEffectRenderer.submit`
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/
ScreenEffectRenderer.java:55-83`); pumpkin/freeze/spyglass/confusion/portal
are all vanilla's *different* mechanism, `Hud.extractCameraOverlays`
(`Hud.java:269-309` — see the pumpkin section below for why it shares this
pipeline anyway) but share the identical "textured, alpha-blended,
screen-space quad after the hand pass" shape closely enough that all seven
landed in one pass rather than inventing a second pipeline:
`lodestone_render::ScreenEffectRenderer`.

Confusion/portal additionally drive a **world-projection warp** — see
"The nausea/portal projection warp" section below — which is not a
screen-space quad at all and lives in `camera.rs`, not `screen_effects.rs`.
Spyglass additionally has an **FOV-zoom** component not yet wired anywhere in
this codebase — see its own section for exactly what and where.

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

`brightness` is vanilla's `Lightmap.getBrightness`, which **is** now ported:
`underwater_brightness` delegates to `lodestone_render::light::light_term`, the
same function the terrain shaders mirror (see [light-ramp.md](./light-ramp.md)),
rather than inventing a second curve shape. It used to approximate it with a local
`0.2 + 0.8 * max(sky, block)` ramp; the consequence of dropping that ramp's `0.2`
floor is that a fully dark cell now tints **black** rather than at 20%. It passes
`sky_darken = 1.0`, because this pass has no clock — the same gap it has for fog. `packed_light`
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

### Closed: a "hurt flash" and "screen shake" (issue #98)

**Issue #98 is closed.** What survives of it: the per-entity hurt/death red
overlay below is the real vanilla mechanism and it shipped; the full-screen
tint the issue's title asked for does not exist in vanilla at all (confirmed
twice, independently, in two different sessions); "screen shake" is not a
real vanilla mechanic under any name (`grep -rn "[Ss]hake"` across
`client-src` finds only an unrelated item-wobble in `ItemInHandRenderer.java`
— no camera reference anywhere). The issue was retitled to stop naming two
effects that were never both real, and closed rather than left open on an
unbuilt, non-vanilla ask with no code dependency on anything else in the
tracker. If explosion camera shake is still wanted, it needs a **fresh**
issue scoped explicitly as new game-feel work, not vanilla parity — nothing
here builds it speculatively. The rest of this section is kept as the
historical record of *why*.

Issue #98 asked for a full-screen red tint on taking damage, framed as
plausibly belonging in this pass — this file's own module doc, and the fact
that this pass already draws "exactly this shape of thing," is why a later
agent's task briefing pointed here first. It does not belong here, and nothing
was added: `ScreenEffectRenderer.java` (this pass's own vanilla source) has
**zero** references to `hurt`/`hurtTime` anywhere in it, and `Gui.java`/
`LevelRenderer.java`/`GameRenderer.java` were grepped clean too. Vanilla's only
local-player-facing responses to taking damage are `bobHurt` (a camera roll,
issue #58's scope, not a screen-space overlay) and a per-entity model overlay
that is invisible on the local player's own first-person view (that overlay's
render mechanism, and the full jar citations, are in `docs/combat.md`'s "The
per-entity hurt/death red overlay (issue #98, entity half)" section). A
full-screen tint here would be **invented**, not ported — if one is still
wanted, it needs to be scoped as new game-feel work, not vanilla parity, and
this file's `ScreenEffects` struct is still the right place to add it *if*
that decision is made (one more `bool`, one more mix in `OVERLAY_WGSL`,
exactly `on_fire`'s shape) — it just was not built speculatively here.

**Update: the per-entity overlay is now wired end to end, and it stayed out of
this pass.** `HurtTime` reaches pixels through the *entity* pipeline
(`ClientEvent::EntityHurtAnimation` → `HurtTime` → `EntityDraw::hurt` →
`prepare_entities`' hurt/not-hurt split → `InstanceTint::with_hurt` → the
entity shader's gamma-space blend), gated by
`crates/lodestone-shell/tests/hurt_overlay_pixels.rs`. That gate asserts
**zero** changed pixels outside the mob's own silhouette, which is the
mechanical proof it has not quietly become the screen-space tint this section
says it must not be — if someone later folds a hurt flash into
`ScreenEffects`, that assertion is the one that will notice.

Note also what this pass's `on_fire` still lacks and the hurt overlay now has:
a *per-entity* value cannot ride a single session-scoped `bool` on
`ScreenEffects`. That is why the two were wired differently rather than
symmetrically, and why the hurt half did not wait on `on_fire`'s
`entity_view()` reachability fix.

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

### Pumpkin: not `ScreenEffectRenderer` at all in vanilla, but the same shape

Issue #185's overlay is **not** part of `ScreenEffectRenderer.java` — grepping
that file for `pumpkin` returns nothing. It is a generic mechanism in
`Hud.java`: `extractCameraOverlays`
(`.cache/mc/26.2/client-src/net/minecraft/client/gui/Hud.java:269-291`) walks
every `EquipmentSlot`, and for any equipped `ItemStack` whose
`DataComponents.EQUIPPABLE` component has a `cameraOverlay` set, blits that
texture full-screen at alpha `1.0`
(`extractTextureOverlay`, `Hud.java:1026-1031`:
`graphics.blit(RenderPipelines.GUI_TEXTURED, texture, 0, 0, 0, 0, guiWidth, guiHeight, guiWidth, guiHeight, ARGB.white(1.0F))`).
Carved pumpkin is simply the **one item in the game** that ships this
component populated:

```json
// .cache/mc/26.2/generated/reports/minecraft/components/item/carved_pumpkin.json
"minecraft:equippable": {
  "camera_overlay": "minecraft:misc/pumpkinblur",
  "slot": "head",
  "swappable": false
}
```

So vanilla's real mechanism is a per-item lookup table with exactly one
populated entry today, not a hardcoded pumpkin check. This port takes the
one-entry version of that table rather than building unused generality:
`ScreenEffects::wearing_pumpkin` is `true` iff the head slot holds
`minecraft:carved_pumpkin`, computed in `app.rs`'s `redraw()` from the
already-in-scope `player_menu` (native inventory index 39 — the same head
slot `Sim::third_person_body_state`'s `ARMOUR_NATIVE_SLOTS` table uses for
armour rendering). If a second item ever ships a `camera_overlay`, the
right generalisation is a `ResourceLocation -> texture` table read off each
item's own components, not a second bool.

**No tint, no scroll, no tiling.** `pumpkin_overlay_triangles()` is a static
full-NDC quad built once at construction and never rewritten — unlike
underwater's per-frame UV scroll and fire's per-frame frame-index rebuild,
there is nothing here that varies frame to frame, so `draw_pumpkin` takes no
per-frame arguments beyond the encoder/view. The vertex colour is opaque
white (`PUMPKIN_TINT = [1,1,1,1]`, matching vanilla's `ARGB.white(1.0F)`):
the pumpkin-shaped vignette silhouette comes entirely from
`pumpkinblur.png`'s own alpha channel, the same way underwater's blue cast
comes entirely from its own texture rather than the vertex tint.

**Gating is a simplification, not a transcription.** Vanilla's
`extractCameraOverlays` guards the whole per-slot loop on
`getCameraType().isFirstPerson()` only — no explicit spectator check is
visible in that method. This port folds `wearing_pumpkin` into the same
`any_active` gate as underwater/fire (first-person **and** not-spectator),
matching this codebase's existing "nothing about my own body renders in
spectator" convention rather than vanilla's literal per-pass gate list. If a
future gate audit finds vanilla *does* show camera overlays to a spectator
wearing a pumpkin somewhere else in the call chain, that is the one deviation
to revisit here.

### Freeze (issue #139): the mechanic is real, borrowed, and coordinated

Vanilla's freeze vignette is the `player.getTicksFrozen() > 0` branch of
`Hud.extractCameraOverlays` (`Hud.java:293-295`):
`extractTextureOverlay(POWDER_SNOW_OUTLINE_LOCATION, player.getPercentFrozen())`
— exactly pumpkin's static-quad shape, but with a *variable* alpha
(`player.getPercentFrozen()`, `0.0..=1.0`) instead of pumpkin's fixed `1.0`.
`freeze_overlay_triangles(percent)` in `screen_effects.rs` is that: same
UVs/positions as `pumpkin_overlay_triangles`, tint `[1,1,1,percent]`.

**The underlying mechanic (issue #212) is not this issue's own work, and was
not duplicated here.** `lodestone_physics::player::PlayerState::frozen_ticks`/
`percent_frozen()`/`is_freezing()` (`crates/lodestone-physics/src/player.rs:431-568`)
already exist, already tick (`update_freezing`, called from `tick` at
`player.rs:2783`), and are already reachable from the shell with **no new
`Sim` accessor** — `Sim::player()` already returns the physics crate's
`PlayerState` by value (`sim.rs:1792`), so `self.sim.player().percent_frozen()`
is a real, tested input today, not a stub. This also answers issue #139's own
first scope checkbox ("determine whether the freezing state is
server-authoritative or client-computed"): it is **client-computed**, the
same swept-collision-per-tick shape as `fall_distance`/sprinting/every other
field on `PlayerState` — vanilla's own `LivingEntity.aiStep` runs identically
on every client (including the local player's own prediction) and on the
server, not just server-side-and-synced, so this is not a gap, it is the
correct port of how vanilla itself computes it.

**Not first-person-gated** — see "Draw order and gating" below for why this
differs from pumpkin/spyglass and how `RenderState::render_inner` reflects
it.

### Spyglass (issue #154): a dedicated method, not the generic `camera_overlay` table

**Answering the open question the briefing asked to settle: no, this is not
a two-line addition to the generic `camera_overlay` table pumpkin uses.**
`Hud.extractCameraOverlays` branches on `player.isScoping()` *before* it ever
reaches that per-slot loop (`Hud.java:277-291`):

```java
if (player.isScoping()) {
    this.extractSpyglassOverlay(graphics, this.scopeScale);
} else {
    // the generic camera_overlay per-slot loop pumpkin uses
}
```

`extractSpyglassOverlay` (`Hud.java:1033-1048`) is its own method with its
own geometry: a centred `spyglass_scope.png` lens sized by a scale factor,
surrounded by **four separate opaque-black fills** (`graphics.fill`, not a
texture) covering whatever the lens does not. `spyglass_lens_triangles`/
`spyglass_letterbox_triangles` transcribe exactly that:

- `spyglass_lens_half_extent(aspect)` derives the lens's NDC half-extent
  algebraically from `Hud`'s own `srcWidth = srcHeight = min(guiWidth,
  guiHeight)` / `ratio = min(...) * scale` — the smaller screen dimension
  always gets half-extent `SPYGLASS_SCALE = 1.125` (vanilla's settled
  `Hud.scopeScale` lerp target, `Hud.java:276`; the few-frame ease-in ramp
  toward it is dropped as a deliberate simplification, same spirit as the
  fire overlay's placement), the larger one is compressed by `aspect`. On a
  landscape screen this overflows the lens past the top/bottom of NDC —
  intentional, matching vanilla (no top/bottom bars in landscape).
- The four bars are drawn with the pipeline's own procedural 1x1 opaque-white
  texture (`ScreenEffectRenderer`'s `white_bind_group`), tinted pure black
  (`LETTERBOX_TINT`), rather than a second pipeline or a loaded asset —
  multiplying any texel by a zero-RGB tint is black regardless of what is
  sampled.

**The FOV-zoom half is real, tested, and unwired — the vignette above is only
half of #154.** `AbstractClientPlayer.getFieldOfViewModifier`
(`AbstractClientPlayer.java:92-114`) returns `0.1F` outright (a 10x zoom,
overriding every other FOV modifier) when `firstPerson && isScoping()`.
`lodestone_render::spyglass_fov_modifier(scoping: bool) -> f32` in
`camera.rs` models exactly that (`0.1` scoping, `1.0` otherwise) and is unit
tested, but **nothing in this codebase multiplies `Camera::fov_y_degrees` by
it yet**: that value is assigned in `crates/lodestone-shell/src/
camera_rig.rs` (`FOV_Y_DEGREES`/per-frame construction), a file outside this
change's ownership (see `CLAUDE.md`'s file-ownership section — it is also
outside the brokered set, unlike `app.rs`/`sim.rs`, since it is another
agent's active territory). Whoever owns `camera_rig.rs` needs one
multiplication: `camera.fov_y_degrees *= spyglass_fov_modifier(scoping)` (or
vanilla's own smoothed `Camera.fovModifier` lerp, if that ever gets built for
sprint/flying too) composed with whatever else already produces
`fov_y_degrees` there — not overwriting it, per issue #154's own scope note
about this exact trap.

**`scoping`'s input is real, not stubbed**, once the two-line broker patch to
`sim.rs` lands (see "The spyglass flag's route to the shell" below):
`Player.isScoping()` is `isUsingItem() && getUseItem().is(Items.SPYGLASS)`
(`Player.java:1936-1938`), and both halves already exist here —
`Sim`'s own `UsingItem` ECS resource (armed by `start_use`/cleared by
`end_use`, `sim.rs`) for the first half, and the already-computed `held`
`ResourceLocation` in `app.rs`'s `redraw()` (used for
`set_main_hand_source`) for the second. Issues #54/#57 (held-item pose,
bow/crossbow draw) are what this depended on and both are closed, so nothing
here is blocked on missing state — only on the two-line `sim.rs` accessor
`Sim::using_item` did not have before this change.

### Confusion and portal (issues #144, #149): one screen-space pair, one shared projection warp

Vanilla ties nausea and portal together in **two** places, and this port
keeps them together in the same two places rather than treating them as
unrelated features that happen to look similar:

1. **The screen-space overlay**, mutually exclusive, portal winning
   (`Hud.java:297-308`):
   ```java
   if (portalIntensity > 0.0F) {
       this.extractPortalOverlay(graphics, portalIntensity);
   } else if (nauseaIntensity > 0.0F) {
       // screenEffectScale < 1.0 check, then extractConfusionOverlay
   }
   ```
   `confusion_overlay_triangles(strength)` transcribes `extractConfusionOverlay`
   (`Hud.java:1109-1132`): the quad is scaled about the screen centre by
   `size = 2.0 - strength` (vanilla's `Mth.lerp(strength, 2.0F, 1.0F)`,
   always `>= 1.0` for valid `strength`, so it always at least covers the
   full screen — the rasterizer clips the rest, the same free-clip
   [`spyglass_lens_triangles`] relies on), tinted a green-biased
   `(0.2, 0.4, 0.2) * strength` at alpha `1.0`. `portal_overlay_triangles`/
   `portal_overlay_alpha` transcribe `extractPortalOverlay`
   (`Hud.java:1097-1107`): a full-screen quad sampling the **same
   32-frame-strip shape** as the fire overlay (`nether_portal.png` is
   `16x512`, `{"animation": {}}`, identical to `fire_1.png` — `fire_frame_count`
   applies unchanged), tinted white at `alpha^4 * 0.8 + 0.2` below `1.0`
   (vanilla's own floor-curve, never fully transparent).

2. **The world-projection "spinning" warp**
   (`GameRenderer.renderLevel`, `.cache/mc/26.2/client-src/net/minecraft/
   client/renderer/GameRenderer.java:543-552`), which is **not** screen-space
   geometry at all — it rotates and shears the *world's own projection
   matrix* before every world-space draw call that frame, which is why
   `crate::camera::nausea_portal_warp` lives in `camera.rs`, not
   `screen_effects.rs`. This answers #144's own briefing question ("a
   projection-matrix effect, establish where it belongs before writing it"):
   it belongs on `Camera`, specifically as a post-multiply on
   `view_projection()` (`Camera::view_projection_warped`), because
   `RenderState::render_inner` already rewrites **one** shared `view_proj`
   matrix at the top of the function into every world-space uniform
   (sections, the model shared camera buffer, the outline pass, debug lines)
   — injecting the warp there reaches everything vanilla's own
   `RenderSystem.setProjectionMatrix` call reaches (the whole world pass) and
   nothing it does not (the HUD/GUI and this crate's own overlay pipeline
   read no camera matrix at all), with a single call-site change. See
   `camera.rs`'s own doc on `nausea_portal_warp`/`view_projection_warped` for
   the transcribed formula and the determinant/fixed-axis tests that pin it
   down without a screenshot.

   Vanilla accumulates the warp's spin angle as a per-tick integral
   (`GameRenderer.tick`, lines 261-270) that only advances while either
   intensity is positive. This crate's `RenderState::render_inner` takes
   `&self`, not `&mut self` — there is nowhere to store that integral, and
   every other "how far has this animation progressed" input in this pass
   (e.g. fire's `tick`) is threaded in from outside rather than accumulated
   internally. `spinning_effect_angle_degrees(tick, portal_intensity,
   nausea_intensity)` is the deliberate substitute: a **pure function of the
   current game tick** at vanilla's own blended per-tick speed
   (`(portal*20 + nausea*7) / (portal+nausea)`), which reaches the identical
   steady-state rotation *rate* the instant either effect is active, just at
   a different absolute phase than vanilla's own freeze-on-deactivate
   accumulator — imperceptible for a continuously looping spin with no fixed
   start reference. Documented as a simplification, not silently taken.

**Neither intensity has a live producer yet, and that is a real, honestly
reported gap — not a stub.** `ScreenEffects::nausea_intensity`/
`portal_intensity` default to `0.0`, at which `view_projection_warped` is
*provably* identical to plain `view_projection`
(`view_projection_warped_matches_plain_view_projection_when_inactive`), so
every existing caller is unaffected. But unlike freeze (#139, whose mechanic
already existed in `lodestone-physics`) or spyglass (#154, whose mechanic
already existed via `UsingItem`/held-item), **no potion-effect-duration
tracker and no nether-portal-proximity tracker exist anywhere in this
codebase** to compute vanilla's `getEffectBlendFactor(NAUSEA, ...)` or
`Entity.portalEffectIntensity`. Both live in `lodestone-ecs`/
`lodestone-physics`, outside this change's ownership, and neither is
currently claimed by another agent the way #212 (freezing) was. The render
mechanism — geometry, pipeline, projection warp, gating, mutual exclusion —
is real, tested (including two GPU-gated pixel gates and eight unit tests
across `camera.rs`/`screen_effects.rs`) and wired all the way to
`RenderState::render_inner`; it is simply never told the truth yet, the same
shape `on_fire` was in before issue #112 closed it.

### Draw order and gating

`GameRenderer.java:568-577`: the hand pass, then
`screenEffectRenderer.submit`, then the HUD/feature renderers. This port's
overlay draw sits in `RenderState::render_inner`, immediately after
`draw_first_person_hand` and before `queue.submit` — the shell's own HUD
draws afterward, in a separate pass in `app.rs`, so the ordering matches.
(The pumpkin overlay's *vanilla* source is `Hud.java`, drawn as part of the
HUD proper, not this pass — see above for why it landed here anyway.)

Gating mirrors `ScreenEffectRenderer.submit`'s
`isFirstPerson && !isSleeping && !isSpectator` — but as of freeze/confusion/
portal, **not with one gate**. `Hud.extractCameraOverlays` itself has two
groups, checked directly against the jar rather than assumed: pumpkin/
spyglass sit inside its `if (getCameraType().isFirstPerson())` block
(`Hud.java:277-291`), while freeze (`Hud.java:293-295`) and confusion/portal
(`Hud.java:297-308`) are **siblings** of that block, not nested in it — they
draw in third person too. `ScreenEffects::any_active` folds both groups into
one bool (the outer short-circuit `render_inner` checks before opening any
pass), but exposes each group separately —
[`ScreenEffects::first_person_group_active`] (`eye_in_water || on_fire ||
wearing_pumpkin || scoping`, gated on first-person **and** not-spectator) and
[`ScreenEffects::camera_agnostic_group_active`] (`freeze_percent > 0.0 ||
nausea_intensity > 0.0 || portal_intensity > 0.0`, gated on not-spectator
only) — and `render_inner` re-checks each group at its own dispatch site
rather than trusting the outer `any_active` alone, precisely because a
freeze-only frame in third person must not also fire an eye-in-water flag
left set from a stale earlier value. There is no "sleeping" conjunct
anywhere: this crate has no sleeping state yet, and its absence can only be a
false *negative* miss (a sleeping player who should not see an overlay but
does), never a false positive that hides a working feature.
`!spectator` on the camera-agnostic group is this codebase's own convention,
not a vanilla literal — `Hud.java` has no explicit spectator check anywhere
in `extractCameraOverlays`, for *any* of the seven effects — but nothing
here has reason to be the first exception to "nothing about my own body
renders in spectator". See `spectator_suppresses_both_overlays`/
`spectator_suppresses_the_camera_agnostic_group_too`/
`freeze_confusion_and_portal_survive_third_person_unlike_the_others` in
`crates/lodestone-shell/tests/screen_overlay_pixels.rs`, and the `any_active`/
`first_person_group_active`/`camera_agnostic_group_active` unit tests in
`crates/lodestone-shell/src/gpu/screen_effects.rs`.

Portal/confusion mutual exclusion (`Hud.java:300-302`'s `if`/`else if`) is
reproduced as an `if`/`else if` in `render_inner`'s own dispatch, not two
independent `if`s — see `stats.confusion_overlay_drawn`'s doc.

## The on-fire flag's route to the shell (closed, issue #112)

**Closed.** `apply_local_player_on_fire` now folds the bit into
`Vitals::on_fire`, `PlayerSnapshot::on_fire` carries it, and `app.rs` reads it —
the overlay is live. The analysis below is kept because it explains *why* a
dedicated fold was the only route, which is not obvious from the code.
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

> **Historical, as of the "Closed" heading above.** That last paragraph describes
> the state *before* #112 landed. `app.rs`'s `redraw()` now reads
> `PlayerSnapshot::on_fire` off the shared handle
> (`crates/lodestone-shell/src/app.rs:1543`), so production does pass `true`. It
> matters for issue #390 below: the flag reaches real pixels, which is what made
> a *stale* flag a real defect rather than a dormant one.

### The patch, as applied

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

## The pumpkin flag's route to the shell (closed, issue #185)

**Closed — stale "patch pending" heading corrected.** `app.rs`'s `redraw()`
already computes `wearing_pumpkin` from `player_menu.player_native(39)` and
feeds it into the `screen_effects` literal (`app.rs:2323-2333` as of this
writing). `ScreenEffects::wearing_pumpkin`, `stats.pumpkin_overlay_drawn`,
`ScreenEffectRenderer::draw_pumpkin`, `pumpkin_overlay_triangles`, and
`load_pumpkin_overlay_texture` are all real and wired end to end through
`RenderState::render_inner` — proved by the pipeline-level GPU gate above.

## The freeze/spyglass/confusion/portal flags' route to the shell (issues #139, #144, #149, #154)

All four render mechanisms (geometry, pipeline, `ScreenEffects` fields,
`render_inner` dispatch, stats counters) are real and wired end to end,
proved by the pipeline-level GPU gate above, **and `gpu.rs`'s own dispatch is
now landed too** (the `first_person_group_active`/`camera_agnostic_group_active`
split and the five draw calls this doc used to describe as "held back" —
`a2e13c6` deliberately did not commit them because the working tree mixed
them with another agent's unrelated armour-dye refactor in the same file;
they were reconstructed from `HEAD` plus only the overlay hunks, verified by
diffing the result against both `HEAD` and the working tree, and landed
separately). The pixel-producing half is entirely done.

**Freeze's input is landed too** — `app.rs`'s `screen_effects` construction
now reads `let freeze_percent = self.sim.player().percent_frozen();`, no
`sim.rs` change needed since `Sim::player()` already returns
`lodestone_physics::player::PlayerState` by value.

**Spyglass's FOV-zoom half is landed, but only its composable half.**
`crates/lodestone-shell/src/camera_rig.rs::apply_spyglass_fov(camera, scoping)`
exists and is unit-tested (the `0.1` scaling while scoping, no-op while not,
and a composition check that a non-default `fov_y_degrees` scales relative to
itself rather than being reset to an absolute constant) — but nothing calls
it yet. `build_camera`'s only production call site is `Sim::camera` in
`sim.rs`, outside `camera_rig.rs`'s ownership, so threading a real `scoping`
bool through to a call is a patch for whoever owns `sim.rs`/`app.rs`, not
something forced through here.

**What is still not landed is `scoping` itself — one accessor, contended.**
`Player.isScoping()` is `isUsingItem() && getUseItem().is(Items.SPYGLASS)`
(`Player.java:1936-1938`); the held-item half is already computed in
`app.rs`'s `redraw()` (the `held` local, used a few lines above for
`set_main_hand_source`), but the `isUsingItem()` half needs a two-line
accessor on `Sim` that does not exist yet:

```rust
/// Whether the local player currently has an item "in use" — the
/// right-mouse-held input state `UsingItem` mirrors (armed by `start_use`,
/// cleared by `end_use`), not a re-derivation of vanilla's own
/// `LivingEntity.isUsingItem()` tick counter. Exists so a caller can derive
/// `Player.isScoping()` (`isUsingItem() && getUseItem().is(Items.SPYGLASS)`,
/// `Player.java:1936-1938`) without reaching into the ECS resource directly
/// — see issue #154.
#[must_use]
pub fn using_item(&self) -> bool {
    self.read(|w| w.resource::<UsingItem>().0)
}
```

next to `Self::player` (`sim.rs:1792`, immediately after its closing brace;
`UsingItem` is already imported there, so no new `use` line is needed). This
was not applied directly because `sim.rs` is contended (another agent's
in-flight work sat there at the time — the held-item name highlight, issue
#126, unrelated to overlays); it is a prepared patch handed off instead of
landed by force. Once it exists, `app.rs`'s `screen_effects` construction
becomes:

```rust
let scoping = self.sim.using_item()
    && held
        .as_ref()
        .is_some_and(|loc| loc.namespace() == "minecraft" && loc.path() == "spyglass");
```

(`held` needs capturing before it moves into `set_main_hand_source` a few
lines above, the same "capture before move" shape `wearing_pumpkin` already
uses for its own lookup) plus one line in `camera_rig.rs`'s caller —
`apply_spyglass_fov(build_camera(...), scoping)` — or the equivalent inline
multiplication, wherever `Sim::camera`/`WindowApp`'s camera construction
calls `build_camera` today.

**`nausea_intensity`/`portal_intensity` remain honestly at `0.0`** — no
potion-effect-duration tracker or nether-portal-proximity tracker exists
anywhere in this codebase yet to compute vanilla's
`getEffectBlendFactor(NAUSEA, ...)`/`Entity.portalEffectIntensity`. Both
would be `lodestone-ecs`/`lodestone-physics` work, outside every file this
section discusses. Exactly `on_fire`'s pre-#112 shape.

## A session-scoped flag needs an explicit reset (issue #390)

`Vitals::on_fire` is written by exactly one thing —
`apply_local_player_on_fire`, off entity metadata naming our own id — and
metadata only arrives when the server has something to say. So the field is
**sticky**: whatever the last packet said stays true until the next one
contradicts it, and a respawn does not produce a contradicting packet on its own.

Vanilla never hits this because a respawn is a *new entity on both sides*:
`PlayerList.respawn` does `new ServerPlayer(...)` (`PlayerList.java:393`), and
the client throws away its `LocalPlayer` and builds another via
`gameMode.createPlayer` (`ClientPacketListener.handleRespawn`, `:1286`), keeping
only the entity id. The fresh entity's synched data starts at `Entity`'s
declared defaults — shared flags `0`, air `getMaxAirSupply()`
(`Entity.java:319`).

We keep one long-lived entity across the whole session, so the clear has to be
written down. `session::apply_local_player_state`'s `Respawned` arm now sets
both `Vitals::on_fire` and `Vitals::air` back to `None`.

**`None`, not `Some(false)`.** `None` is the documented "no reading yet" state
and already reads as not-burning downstream; a literal would be us inventing a
report the server never sent. Air's sibling bug is the visible one — see
[`sky-and-air-bubbles.md`](./sky-and-air-bubbles.md#a-respawn-clears-the-metadata-fed-vitals-issue-390).

**If you add another metadata-fed session field, add it to that arm.** The
routing switch is the usual island factory here; this is the second one, and the
failure is silent in both directions (`on_fire`'s absence reads as `false`, so
nothing looks wrong until a player dies burning).

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
  cached (`ScreenEffectRenderer::fire_frame_count()`/`portal_frame_count()`).
  A resource pack with a differently-sized `fire_1.png`/`nether_portal.png`
  still animates correctly; it would only break if a pack used a
  non-16px-wide strip, which vanilla's own asset pipeline does not support
  either.
- **The letterbox bars' texture is procedural, not loaded.** `white_bind_group`
  is a 1x1 opaque-white texture built at `ScreenEffectRenderer::new` with no
  backing asset — see `spyglass_letterbox_triangles`'s doc for why a flat
  colour fill needs no real texture content. If a future overlay needs
  another flat fill, reuse this bind group rather than adding a second
  procedural texture.
- **Two independent gate groups, not one.** Freeze/confusion/portal are
  **not** first-person-gated, unlike every overlay before them — see "Draw
  order and gating" above. Adding a new overlay means deciding which group it
  belongs to by checking the jar (`Hud.extractCameraOverlays`'s nesting), not
  by assuming it follows the existing five.
- **The nausea/portal projection warp has its own home, `camera.rs`, not
  this file.** A future change to the warp's formula
  (`nausea_portal_warp`/`spinning_effect_angle_degrees`) touches `camera.rs`
  and its one call site in `RenderState::render_inner` (`let view_proj =
  camera.view_projection_warped(...)`) — nothing in `screen_effects.rs`
  needs to change for it.

## Configuration

None. Both textures load from whichever `client.jar` `crate::resources::asset_root`/
`open_client_jar` already resolve (same as the sky pass); there is no env var
or flag specific to this pass. `crate::resources::load_screen_effects` is
`None`-returning and fail-open exactly like `load_sky`: a jar-less run or a
pack missing either texture leaves `RenderState` with no overlay pass
installed, not a startup failure.

## Dependencies

- `lodestone-assets::screen_effects` — `load_underwater_texture`/
  `load_fire_texture`/`fire_frame_count`/`load_pumpkin_overlay_texture`/
  `load_freeze_overlay_texture`/`load_spyglass_scope_texture`/
  `load_nausea_overlay_texture`/`load_portal_overlay_texture`, the same
  "plain unatlased `Image`" loader shape `sky::load_cloud_texture` uses, and
  for the same reason: each texture's own addressing (4×4 tiling with
  wraparound for underwater; independent-frame-slice, no wraparound for
  fire/portal; a single static image for pumpkin/freeze/spyglass/nausea)
  would conflict with an atlas's per-sprite padding.
- `lodestone-render::screen_effects` — the pure geometry
  (`underwater_overlay_quad`/`triangles`, `fire_overlay_triangles`,
  `pumpkin_overlay_triangles`, `freeze_overlay_triangles`,
  `spyglass_lens_triangles`/`spyglass_letterbox_triangles`/
  `spyglass_lens_half_extent`, `confusion_overlay_triangles`,
  `portal_overlay_triangles`/`portal_overlay_alpha`, `underwater_brightness`)
  and the GPU-owning `ScreenEffectRenderer` (`draw_underwater`/`draw_fire`/
  `draw_pumpkin`/`draw_freeze`/`draw_spyglass`/`draw_confusion`/
  `draw_portal`).
- `lodestone-render::camera` — `nausea_portal_warp`/
  `spinning_effect_angle_degrees`/`Camera::view_projection_warped` (issues
  #144/#149's shared world-projection warp) and `spyglass_fov_modifier`
  (issue #154's FOV-zoom half, not yet wired to any live `Camera` — see the
  "Spyglass" section above).
- `lodestone-shell::gpu` — `RenderState::screen_effects`,
  `install_screen_effects`/`has_screen_effects`, the draw calls inside
  `render_inner` (including `stats.pumpkin_overlay_drawn`/
  `spyglass_overlay_drawn`/`freeze_overlay_drawn`/`confusion_overlay_drawn`/
  `portal_overlay_drawn`, and the `view_projection_warped` call that replaces
  plain `view_projection` at the top of `render_inner`), and
  `gpu::screen_effects::ScreenEffects` (the per-frame argument type,
  including `wearing_pumpkin`/`freeze_percent`/`scoping`/`nausea_intensity`/
  `portal_intensity`, and the `first_person_group_active`/
  `camera_agnostic_group_active` split).
- `lodestone-shell::resources::load_screen_effects` — the `client.jar` IO,
  mirroring `load_sky` exactly; `ScreenEffectRenderer::new` loads every
  texture in one call, so there is nothing per-overlay to add here.
- `lodestone-shell::sim` — `Sim::player().percent_frozen()` (already public,
  no patch needed, issue #139) and `Sim::using_item()` (issue #154, patch
  pending — see "The freeze/spyglass/confusion/portal flags' route to the
  shell").
- `lodestone-physics::player` — `PlayerState::frozen_ticks`/`percent_frozen`/
  `is_freezing` (issue #212's mechanic, consumed not duplicated here).
- `lodestone-shell::app::redraw` — computes `wearing_pumpkin` from the
  already-in-scope `player_menu`'s native slot 39 (head, landed), and (patch
  pending) `freeze_percent`/`scoping` the same way.

## The gates, and what they printed

Both are `#[ignore]`d GPU gates (no GPU on CI); run with `--ignored
--nocapture`. Every number below was actually run on this machine, not
predicted.

**Pipeline level** (`cargo test -p lodestone-render --test
screen_effects_pipeline_gpu -- --ignored --nocapture`) — proves each pass
paints pixels at all, via a synthetic in-memory pack, independent of the
shell. All 8 tests pass, four added for freeze/spyglass/confusion/portal:

| test | result |
|---|---|
| `control_an_untouched_target_reads_back_as_black` | pass (negative control) |
| `underwater_overlay_paints_the_whole_frame` | pass |
| `fire_overlay_paints_only_the_bottom_strip` | pass, explicit top/bottom row-band split |
| `pumpkin_overlay_paints_the_whole_frame_at_full_strength` | pass, magnitude: opaque untinted `(40,200,40)` source → avg green > 150 |
| `freeze_overlay_paints_the_whole_frame_at_the_predicted_half_alpha` | pass, magnitude: `percent=0.5` over a light-blue `(200,230,255)` source predicts a half-blended readback, not the full source colour (wrong-alpha-1.0) or near-black (wrong-alpha-0.0), plus a channel-order check (blue > red, matching the source) |
| `spyglass_overlay_paints_a_grey_lens_surrounded_by_black_bars` | pass, **location**, not average: screen centre (inside the lens) is non-black, far-left edge at mid-height (inside a letterbox bar at 16:9) is pure black |
| `confusion_overlay_paints_the_whole_frame_with_a_green_biased_tint` | pass, magnitude: green channel > 1.3x red/blue at `strength=1.0`, matching the tint's own `0.4`-vs-`0.2` ratio |
| `portal_overlay_paints_the_whole_frame_at_full_intensity` | pass, frame-selection (32-frame strip) **and** magnitude: opaque magenta `(200,40,200)` source at full intensity → avg red/blue > 130, avg green < 80 |

Every pipeline-level GPU test above ran clean on this machine (`8 passed; 0
failed`), captured directly from `cargo test`'s own exit status, not read off
a filtered pipeline.

**Camera-side (no GPU needed)**: `cargo test -p lodestone-render --lib
camera::` — 22 tests, including the warp/FOV-modifier additions:
`nausea_portal_warp_is_identity_at_zero_or_negative_intensity`,
`nausea_portal_warp_at_max_intensity_matches_hand_computed_skew` (determinant
check against a hand-derived `skew^2 = 0.629378`, since a rotation has
determinant 1 and cannot change it), `nausea_portal_warp_angle_zero_is_a_pure_x_axis_scale`,
`nausea_portal_warp_preserves_the_rotation_axis` (a vector along the warp's
own rotation axis must be fixed by it — an algebraic check, no screenshot
needed), `view_projection_warped_matches_plain_view_projection_when_inactive`/
`_differs_when_active`, `spinning_effect_angle_uses_vanillas_blended_speed`
(20/tick portal-only, 7/tick nausea-only, 13.5/tick blended — vanilla's own
constants), `spyglass_fov_modifier_is_a_tenth_while_scoping`. All pass.

**Shell level** (`cargo test -p lodestone-shell --test screen_overlay_pixels
-- --ignored --nocapture`) — proves `RenderState::render_inner` actually
checks `self.screen_effects`/`ScreenEffects`, through the real
`render_with_effects` path, with executed negative controls, including a new
`freeze_confusion_and_portal_survive_third_person_unlike_the_others` control
(installs a third-person body source, then asserts freeze/portal still draw
while underwater/fire/pumpkin/spyglass do not, and portal wins over
confusion when both are positive). **Now run, on this machine's real
adapter, once `gpu.rs`'s wiring and `app.rs`'s field upgrade both landed**
— all 5 tests pass in 60.52s:

```text
=== spectator control ===
spectator=true, every flag set: underwater=false, fire=false, pumpkin=false, spyglass=false, freeze=false, confusion=false, portal=false

=== third-person split control ===
third_person_body_drawn=true: underwater=false, fire=false, pumpkin=false, spyglass=false, freeze=true, confusion=false, portal=true

=== fire overlay pixel gate (through RenderState::render_with_effects) ===
on_fire=true: fire_overlay_drawn=true, top rows differ 0.6%, bottom rows differ 100.0%
control (installed, on_fire=false): fire_overlay_drawn=false

=== underwater overlay pixel gate (through RenderState::render_with_effects) ===
eye_in_water=true: underwater_overlay_drawn=true, differs from dry control by 100.0%
control A (installed, eye_in_water=false): underwater_overlay_drawn=false
control B (not installed, eye_in_water=true): underwater_overlay_drawn=false, differs from wet by 100.0%

=== pumpkin overlay pixel gate (through RenderState::render_with_effects) ===
wearing_pumpkin=true: pumpkin_overlay_drawn=true, differs from bare control by 100.0%
control A (installed, wearing_pumpkin=false): pumpkin_overlay_drawn=false
control B (not installed, wearing_pumpkin=true): pumpkin_overlay_drawn=false, differs from worn by 100.0%
```

The third-person split control is the one that actually exercises the new
`gpu.rs` dispatch, not just the pipeline: with a third-person body source
installed, `underwater`/`fire`/`pumpkin`/`spyglass` (the first-person-only
group) correctly stay `false` while `freeze`/`portal` (the camera-agnostic
group) still draw — proving `first_person_group_active`/
`camera_agnostic_group_active` are wired to the right gate, not merged into
one check that would have hidden this. Spyglass and confusion themselves
have no dedicated shell-level pixel test yet (`scoping` has no live input to
drive one, and `nausea_intensity`/`portal_intensity` share portal's fixture
in the third-person control above); the pipeline-level gate below already
proves their geometry independent of the shell.

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
non-default values for `freeze_percent`/`scoping`/`nausea_intensity`/
`portal_intensity`. Two of the four (`freeze_percent`, `scoping`) have a real
patch waiting to be applied; two (`nausea_intensity`, `portal_intensity`)
have no producer anywhere in this codebase yet — see "The freeze/spyglass/
confusion/portal flags' route to the shell" above. The gates prove the
mechanism; three-quarters of the mechanism is one merge away from being told
the truth, one-quarter needs new `lodestone-ecs`/`lodestone-physics` work
first.
