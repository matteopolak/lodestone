# Display entity orientation

## What it is

`lodestone_render::display`: the shared geometry every `text_display`/
`item_display`/`block_display` entity carries — the billboard orientation
that decides which way it faces, and the `translation`/`left_rotation`/
`scale`/`right_rotation` transformation on top of it. A faithful port of
`DisplayRenderer.calculateOrientation` and `Transformation.compose` (`26.2`).

**`text_display` is live end to end; `item_display`/`block_display` are
decoded and extracted but not yet drawn.** The full chain is:
`crates/protocol/v770/src/packets/metadata.rs` decodes billboard/
translation/scale/rotation/text/block-state/item-context off
`set_entity_data` → `lodestone_ecs::ingest::apply_display_metadata` folds
them into `Display*` components → `lodestone_shell::display_entities`
extracts a `DisplayDraw` per tracked entity, every field defaulted to
vanilla's own accessor default when unreported → `gpu/display_text.rs`
reads the `text_display` ones and draws glyphs and a background panel
through the real `RenderState::render` path. `crates/lodestone-shell/tests/
text_display_pixels.rs` is the pixel gate proving that last hop: it renders
through the entire chain and reads back real pixels, with a
no-draws-installed control that must paint nothing in the entity's screen
rect — watched to fail with the draw call itself commented out, restored
from an md5-checked backup.

`item_display`/`block_display` reach `DisplayDraw` (their block state / item
stack and `ItemDisplayContext` ordinal are decoded and carried all the way
through) but no GPU pass reads them yet — the same disclosed, "extracted but
not drawn" state this repo's `EntityDraw::wool` field already documents for
sheep wool, not a silent gap. `gpu/moving_blocks.rs`'s existing
`(state id, transform, light) → merge_moving_block` seam is the intended
`block_display` consumer (posing its transform via `display_placement_matrix`
instead of `falling_block_pose`); `gpu/world_items.rs`'s
`mesh_item_quads_with_light` is the intended `item_display` one.

## How it works

Vanilla places a display entity in two composed steps
(`DisplayRenderer.submit`):

```text
pose = T(anchor) * orientation(billboard, entityYaw, entityPitch, cameraYaw, cameraPitch)
           * Transformation(translation, leftRotation, scale, rightRotation)
```

`display_orientation` is the first factor, `DisplayTransformation::to_matrix`
is the second, and `display_placement_matrix` composes both against a
world-space anchor. Everything here is pure geometry with no GPU dependency
and no asset-manager dependency — every input is a value a caller would
already have in hand (entity rotation off a spawn/rotate packet, camera
yaw/pitch, the synced transformation fields), which is what makes it fully
unit-testable with no device at all.

### The four billboard modes

`Display.BillboardConstraints` answers one question differently per mode:
**which yaw, and which pitch, does the entity face with?**

| mode | yaw source | pitch source |
|---|---|---|
| `Fixed` | the entity's own reported yaw | the entity's own reported pitch |
| `Horizontal` | the entity's own reported yaw | the **camera's** pitch |
| `Vertical` | the **camera's** yaw | the entity's own reported pitch |
| `Center` | the **camera's** yaw | the **camera's** pitch |

`Fixed` therefore never rotates to face the viewer at all — a billboard
nailed to whatever orientation the entity itself carries (`0, 0` unless a
summon command sets `Rotation`) — while `Center` is a full camera-facing
sprite, and `Horizontal`/`Vertical` each track exactly one axis while holding
the other at the entity's own value.

The owner's own discriminating test pins the sharpest pair directly: feeding
`Fixed` and `Center` the *same* entity rotation and the *same* (genuinely
different) camera rotation, and requiring the two outputs to actually
differ — the "an input where both hypotheses coincide is not a test"
failure mode this repo's evidence standards warn about, made concrete. It
was watched to fail under a neutered orientation function before being
restored, so the control itself is verified, not merely asserted.

### `DisplayTransformation`

The four synced fields (`translation`, `left_rotation`, `scale`,
`right_rotation`) are shared by **every** `Display` subtype — this is the
"field declared on a base record, inherited by every variant" shape this
codebase has been burned by before (a shield's `ItemModel.Unbaked`
transformation, ported once and read only on `special` nodes, silently
dropping it everywhere else). `DisplayTransformation` is read unconditionally
off every display variant here, not gated behind "looks like it needs
scaling". `to_matrix` composes them in vanilla's own order — translate, then
left-rotate, then scale, then right-rotate — pinned by a test that would
fail if scale and the left rotation were accidentally swapped.

## How to change it

`display_orientation` is a direct transcription of
`DisplayRenderer.calculateOrientation` — do not "simplify" the per-mode
yaw/pitch source table above; that table *is* the four modes' entire
behavioural difference, and collapsing it loses the distinction the modes
exist to draw. `transform_camera_yaw`/`transform_camera_pitch` carry the
`- 180`/negation vanilla applies to the raw camera angles before they enter
the same `rotationYXZ` call the entity's own yaw/pitch use — dropping either
offset makes `Center` face 180° away from the viewer, or invert its
head-tilt tracking, while still looking plausible in a screenshot taken from
directly in front of the entity. That is exactly the kind of defect a
screenshot cannot catch and a unit test can.

### What would need to exist for `item_display`/`block_display` to draw

Both already reach `DisplayDraw` (`lodestone_shell::display_entities`) with
every field decoded — nothing left to add in the protocol or ECS layers for
either. What is missing is purely a render-side consumer, one per subtype:

1. **`block_display`**: a producer for `gpu/moving_blocks.rs`'s existing
   `(state id, transform, light)` seam, reading `DisplayDraw::block_state`
   and posing it with `display_placement_matrix(draw.position,
   display_orientation(draw.billboard, …), &draw.transform)` in place of
   `falling_block_pose`. `merge_moving_block` is already generic over the
   producer — see that file's module doc for the falling-block/piston/TNT
   precedent this would be a fourth instance of.
2. **`item_display`**: a producer alongside `gpu/world_items.rs`'s dropped-item
   path, reading `DisplayDraw::item`/`item_display_context` and calling
   `mesh_item_quads_with_light(quads, display_placement_matrix(…), gui_light,
   light)` — the same primitive `vault_display_item_mesh` already wraps for a
   vault's floating reward, which is the closest existing precedent (an item
   posed by an arbitrary transform matrix, not a dropped-item bob/spin).
3. Wiring either into `gpu/state.rs`/`gpu/frame.rs` reads `RenderState::
   display_draws` (installed by `set_display_draws`, already wired for the
   text pass) — no new per-frame plumbing needed, only a new merge call
   alongside the existing falling-block/dropped-item ones.

**Do not wire an inert field in the meantime.** Both fields already carry
real, decoded data all the way to `DisplayDraw` — the remaining gap is
narrowly "no GPU pass reads this yet", which is the same disclosed state
`EntityDraw::wool` already documents, not something to paper over with a
placeholder consumer.

## Configuration

None — every input is a per-frame value a caller already holds.

## Dependencies

`glam` for the geometry (`lodestone-render`, no GPU device, no asset
manager); `lodestone-model`'s own `Vec3f`/`Quat` types carry the same values
version-free through `crates/protocol/v770` and `lodestone-ecs`, converted to
`glam` only at the `lodestone-shell::display_entities` extract boundary — see
that module's own doc for why it needs no render-side interpolation track
the way `lodestone_shell::entities::extract_entity_draws` does for every
other entity kind.

## Verification

```bash
cargo test -p lodestone-render --lib --no-fail-fast -- display::
cargo test -p lodestone-v770 --lib --no-fail-fast -- packets::metadata::
cargo test -p lodestone-ecs --lib --no-fail-fast -- ingest::
cargo test -p lodestone-shell --lib --no-fail-fast -- display_entities:: gpu::display_text::
# GPU pixel gate (needs a real adapter and the vanilla client.jar):
cargo test -p lodestone-shell --test text_display_pixels -- --ignored --nocapture
```
