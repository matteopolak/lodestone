# Player capes

## What it is

The player cape overlay: a real, per-tick-lagged cloak that sways as the
wearer walks, turns and sprints, rather than a flat plane pinned to the back.
Before this landed, capes were an island — the profile texture was already
parsed and the options toggle already existed, and nothing drew.

The chain is four hops: `lodestone-assets` bakes the cape's geometry,
`lodestone-render` computes its per-frame rotation, `lodestone-shell` derives
that rotation from a lagged position it ticks every game tick, and
`lodestone-shell/src/gpu` uploads and draws it. Elytra is not part of this
chain at all — see "What this is not" below.

## How it works

### The mesh: a static cube with no baked rotation

`lodestone_assets::entity::player_cape_model` bakes one 10×16×1 cube, hung off
a bare `"body"` part in the same coordinate frame `player_model`'s own body
pivot uses, so a caller can pair the cape's `"cape"` part against the wearer's
`"body"` part transform exactly the way armour pairs against named body
parts.

The interesting fact, and the reason it is worth recording rather than just
reading off the code: vanilla's `PlayerCapeModel.createCapeLayer` declares a
static pose rotation, `PartPose.offsetAndRotation(0, 0, 2, 0, PI, 0)` — the
cape faces backward by a half-turn about Y. This baked mesh carries **none**
of that rotation, only the `z = 2` translation. It is not lost — it is
*cancelled*. `PlayerCapeModel.setupAnim` composes a per-frame quaternion onto
the part with `ModelPart.rotateBy`, which post-multiplies:
`new = old_rotation * new_quaternion`. The per-frame quaternion's own first
term is `Ry(-π)` (`cape.rotateBy(new Quaternionf().rotateY(-PI)...)`), so:

```text
new = Ry(π) * [Ry(-π) * Rx(θx) * Rz(θz) * Ry(θy2)]
    = [Ry(π) * Ry(-π)] * Rx(θx) * Rz(θz) * Ry(θy2)
    = Rx(θx) * Rz(θz) * Ry(θy2)
```

`Ry(π)` and `Ry(-π)` are exact inverses on the same axis and cancel
completely. Baking the now-cancelled static rotation into the mesh would
**double** it, not reproduce it — which is exactly why the mesh is baked
identity-rotated and the whole rotation is computed at draw time instead, in
`lodestone_render::entity::cape_local_rotation`. If you ever need to touch
this mesh, do not "helpfully" add the `Ry(π)` back; the cancellation is the
point, not an oversight.

### The rotation: `cape_local_rotation(lean, lean2, flap)`

`lean`/`lean2`/`flap` are vanilla's `capeLean`/`capeLean2`/`capeFlap`, in
degrees. `cape_local_rotation` builds `Rx(6 + lean/2 + flap) * Rz(lean2/2) *
Ry(180 - lean2/2)`, translated by the pivot the static pose used to declare.
This is a direct transcription of `PlayerCapeModel.setupAnim`'s three
`rotateX`/`rotateY`/`rotateZ` calls, composed in the same order.

### The sway: a real per-tick lagged position, not an approximation

The three lean/flap angles come from `lodestone_shell::entities::cape_sway`,
given this frame's interpolated gap between a **lagged** "cloak" position and
the entity's real feet position, plus the entity's body yaw and a walk-bob
amplitude. This is worth stating plainly because it would be easy to fake with
a constant plane: vanilla's `ClientAvatarState` tracks a `(xCloak, yCloak,
zCloak)` point per avatar that chases the real position at **25% per tick**,
snapping instantly on any single-axis delta over 10 blocks (a teleport, not a
walk). `CapeLag`/`tick_cape_lag` port that state and its tick exactly:

- `tick_cape_lag` runs in `GameTick`'s `TickSet::Animate`, once per tracked
  entity, unconditionally (see "Why every entity carries this" below).
- Each axis eases independently: `gap.abs() > 10.0` snaps that axis straight
  to the target, otherwise `cur + gap * 0.25`. Vanilla checks axes
  independently too, so a portal jump that moves only Y snaps only Y.
- A walk-bob amplitude (`bob`) also ticks here, eased 40% per tick toward a
  target clamped at `0.1` — zero while airborne, swimming or off the ground.

`cape_sway` then resolves the *current frame's* interpolated lag (blending
`cloak_o`→`cloak` by partial tick, the same way every other interpolated
render field in `entities.rs` resolves) against the entity's drawn feet and
body yaw, using `lodestone_physics::mth`'s quantised sin/cos rather than
`f32::sin`/`cos` — this repo's standing rule that the two diverge at cardinal
angles, and body yaw `0`/`90`/`180`/`270` is spawn-facing, not a rare
fixture.

### Why every tracked entity carries `CapeLag`, not just players

`CapeLag` is inserted unconditionally in `spawn_track`, the same "costs
nothing to carry" choice `SwimRamp` already makes: it is three `Vec3` pairs
and an `f32` pair per entity, and gating it by `RenderKind` would need the
same well-known-but-changeable-skin plumbing `RenderPlayerSkin` carries for
no measurable win. Only a `"player"` `EntityDraw::type_path` with a non-empty
`player_skin.cape` URL ever reads the derived sway — exactly like
`EntityDraw::swim_amount`'s player-only consumer.

### The draw: grouped by cape URL, gated on elytra

`RenderState::prepare_cape` (`gpu/entity_passes.rs`) is the GPU-side pass. It
skips an entity when: invisible, not `type_path == "player"`, no cape URL, the
cape URL's texture bind group is not installed yet in
`EntityRenderer::player_skins` (a fetch still in flight draws nothing that
frame, same fallback every remote skin gets), or the chest slot's item path is
`elytra`. That last check transcribes `CapeLayer.submit`'s real gate
(`!hasLayer(chestEquipment, WINGS)`) as a literal item-id string match rather
than a full `EquipmentClientInfo` asset lookup — disclosed as an
approximation that only diverges for a resource-pack-only custom chestplate
that also declares a wings layer, which this build has no path to represent
anyway.

Batching is by cape **URL**, not by part — unlike wool/armour, a cape's
texture is per-player rather than fixed, so the grouping key is the same one
`prepare_entities`' own skin batching already uses.

## What this is not: elytra is a separate chain

**Elytra does not fall out of any of this.** It is a different model, a
different pose, and a different draw gate (worn in the chest armour slot, not
derived from a cape URL) — the two share only the fact that a cape URL and an
elytra texture both ride the same remote-texture parse (`skin.rs`'s profile
metadata decode). Do not assume fixing or extending one touches the other.

## How to change it

- **The mesh** (`lodestone_assets::entity::player_cape_model`): geometry only.
  If you touch its pose, re-derive the cancellation above from
  `PlayerCapeModel.createCapeLayer`/`setupAnim` again — do not assume the
  static rotation is simply "missing".
- **The sway formula** (`cape_sway`, `crates/lodestone-shell/src/entities.rs`):
  ported from `AvatarRenderer.extractCapeState`. The one disclosed gap is
  `fall_flying_scale`, hardcoded to `0.0` (identity) because no draw path in
  this codebase currently resolves elytra-flight scale for a *remote* entity
  — correct for a grounded/walking/swimming player, a slightly wider lean than
  vanilla for someone actively gliding.
- **The draw gate** (`RenderState::prepare_cape`,
  `crates/lodestone-shell/src/gpu/entity_passes.rs`): if a future resource
  pack path needs the real `EquipmentClientInfo` wings-layer lookup instead of
  the literal `elytra` path check, that is where to widen it.
- **`showCape`** (the wearer's own cape-visibility toggle,
  `DATA_PLAYER_MODE_CUSTOMISATION`) is not decoded anywhere in
  `crates/protocol/v770` — every remote player draws as if it were `true`,
  matching vanilla's own default when the byte has never been reported. A
  clientbound decoder for that metadata byte is the only piece missing to
  respect a player's own toggle.

## Configuration

None — no flags or env vars gate this. A cape draws whenever the wearer's
profile carries a `CAPE` texture property and that texture has finished
fetching.

## Dependencies

- `lodestone_assets::entity::player_cape_model` — geometry.
- `lodestone_render::entity::{CapeMesh, cape_local_rotation}` — the baked mesh
  type and the per-frame rotation.
- `lodestone_render::entity_pipeline::GpuEntityModel::upload_cape` — GPU
  upload.
- `crates/lodestone-shell/src/entities.rs`'s `CapeLag`/`tick_cape_lag`/
  `cape_sway` — the per-tick lag state and its derivation into draw angles.
- `crates/lodestone-shell/src/gpu/entity_passes.rs`'s `RenderState::prepare_cape`
  and `crates/lodestone-shell/src/gpu/entities.rs`'s `EntityRenderer::cape_model`/
  `cape_gpu` — the GPU-side batching and draw.
- `crates/lodestone-shell/src/remote_skins.rs`'s `RemoteSkin::cape` — the cape
  texture URL, riding the same fetch/cache pipeline (`request`/`drain_ready`)
  a body skin URL already uses.

## Verification

```bash
cargo test -p lodestone-shell --lib --no-fail-fast -- entities:: cape
cargo test -p lodestone-render --lib --no-fail-fast -- entity:: cape
```
