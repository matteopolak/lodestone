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

### `text_display` style and multi-line centring

`gpu/display_text.rs::push_text_display_quads` lays out each `\n`-split line
with `gpu/nametag.rs::layout_styled_ink_runs` (via a `StyledInkLayoutCache`),
the same styled ink-run walk `gpu/nametag.rs`'s own player/mob nametags use —
see `docs/entity-nametags.md`'s "Style" section for the walk itself
(colour, bold, italic, underline, strikethrough; `§k` not implemented).
Two things specific to this pass:

- **Centring must measure the *styled* width, not the plain one.** Vanilla
  centres each line independently against the block's own max line width
  (`Display.TextDisplay.getAlign`/`TextDisplayRenderer.submitInner`:
  `offset = width / 2.0F - line.width() / 2.0F` for `CENTER`), and
  `line.width()` is `Font.width()` on the *styled* `FormattedCharSequence` —
  bold widens the measured advance (`GlyphInfo.getAdvance(bold)`). Measuring
  width from an unstyled walk instead under-reports a bold line's real
  width, so that line's centring offset comes out too large and the line
  reads as shifted toward one side — this was a real, reported defect here,
  fixed by switching the width computation (not just the glyph colour) to
  `layout_styled_ink_runs`.
- **`DisplayDraw::text` is a real `lodestone_model::Text`.** The upstream
  decode (`crates/protocol/v770/src/packets/metadata.rs`'s `Value::Text` arm)
  and `crate::display_entities::extract_display_draws` both carry the
  component tree through unflattened, so `push_text_display_quads` calls
  `Text::to_spans()` on it directly and `split_spans_into_lines` (this file)
  breaks the result on literal `\n`s while keeping each run's own resolved
  style. Colour — a hex `TextColor::Rgb` included — bold, italic, underline
  and strikethrough all reach the drawn glyph: a hex colour is the one thing
  a `to_legacy_string`/`Text::from_legacy` round trip could never carry
  (legacy `§` codes are a fixed 16-entry palette with no hex form), which is
  why this pass no longer bridges through one. `text_glyph_color`'s white is
  only the *fallback* `Font.java::getTextColor` uses when a span's own
  colour is unset, not an unconditional hardcode.

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

### The glyph pipeline's polygon offset is load-bearing — do not merge it away

`gpu/display_text.rs` builds **two** pipelines from one descriptor: the
background panel through `RenderPipelines.TEXT_BACKGROUND`'s plain
`DepthStencilState.DEFAULT`, and the glyphs through
`RenderPipelines.TEXT_POLYGON_OFFSET`
(`DepthStencilState(GREATER_THAN_OR_EQUAL, true, 1.0F, 10.0F)`, which flips
to `constant: -10, slope_scale: -1.0` in this project's `[0,1]` depth). They
look identical apart from that bias, which is exactly why one earlier version
collapsed them into a single unbiased pipeline with the comment "nothing
coplanar to fight".

There is something coplanar to fight, and it is the pass's own panel.
`TextDisplayRenderer.submitInner` puts the panel at `-0.01` in **local glyph
space**, which the `0.025` text scale reduces to **0.00025 blocks** — a
couple of `f32` ULP in the depth buffer at any real range. Head-on the glyphs
still win, because `LessEqual` passes a tie and the glyphs are submitted
second; but the panel is one large quad and each glyph is a tiny one, so
their interpolated depth diverges as the plane goes oblique, and a yaw-only
(`BillboardMode::Vertical`) hologram is oblique whenever the camera is
pitched. Measured by
`crates/lodestone-shell/tests/world_text_over_geometry_pixels.rs`, looking up
at 40°: **389** glyph px with the panel against **438** without, restored to
**438/438** once the glyphs got their own biased pipeline. The loss is
per-pixel scatter across the whole block, not a clean edge, which is why it
reads as "the text is broken at some angles" rather than as a depth bug.

`text_display_pixels.rs` cannot see this and never could: it asserts only
that more than 100 non-sky pixels land in the entity's rect, which the panel
alone satisfies. Any gate for this has to measure the **ink** against a
reference with the panel switched off.

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
# GPU pixel gates (need a real adapter and the vanilla client.jar):
cargo test -p lodestone-shell --test text_display_pixels -- --ignored --nocapture
cargo test -p lodestone-shell --test world_text_over_geometry_pixels -- --ignored --nocapture
```
