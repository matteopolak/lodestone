# Display entity orientation

## What it is

`lodestone_render::display`: the shared geometry every `text_display`/
`item_display`/`block_display` entity carries — the billboard orientation
that decides which way it faces, and the `translation`/`left_rotation`/
`scale`/`right_rotation` transformation on top of it. A faithful port of
`DisplayRenderer.calculateOrientation` and `Transformation.compose` (`26.2`).

**All three subtypes are live end to end.** The full chain is:
`crates/protocol/v770/src/packets/metadata.rs` decodes billboard/
translation/scale/rotation/brightness/text/block-state/item-stack/
item-context off `set_entity_data` → `lodestone_ecs::ingest::apply_display_metadata`
folds them into `Display*` components → `lodestone_shell::display_entities`
extracts a `DisplayDraw` per tracked entity, every field defaulted to
vanilla's own accessor default when unreported → one GPU consumer per
subtype reads `RenderState::display_draws` and puts geometry on screen:

| subtype | consumer | seam it reuses |
|---|---|---|
| `text_display` | `gpu/display_text.rs`'s `push_text_display_quads` | its own glyph/panel pipeline |
| `block_display` | `gpu/moving_blocks.rs`'s `merge_block_displays` | `merge_moving_block`, shared with the falling block, the piston head, primed TNT and a minecart's contents |
| `item_display` | `gpu/world_items.rs`'s `merge_item_displays` | the model-pipeline item mesh a dropped, held, framed or campfire item uses |

Neither of the two new ones is new *rendering*: both are producers for a
consumer that already existed, which is what made them a wiring job rather
than a port. `DisplayDraw::placement` is the shared
`T(position) · orientation · transformation` composition, extracted as a
named symbol so three files cannot apply it three ways — the defect shape
this repo has already paid for once, in a composition of two individually
correct halves.

Two pixel gates prove the last hop. `crates/lodestone-shell/tests/text_display_pixels.rs`
covers the text variant; `crates/lodestone-shell/tests/display_entity_pixels.rs`
covers the other two, with a control per producer (a real display entity
whose payload has never been reported must render byte-identically to an
empty scene) and a scale arm that would still pass for a producer ignoring
`placement` entirely. Both were **watched to fail**: with each merge call
neutered in turn and restored from an md5-checked backup, the block arm
reported `moving_blocks_drawn` 0 against 1, and the item arm reported 0
changed pixels. Note what those gates do *not* cover — they install their own
`DisplayDraw`, so they say nothing about the ECS/wire producer above them;
that half is gated in `display_entities`'s own extract tests and in
`lodestone_v770::packets::metadata`'s index-16 and index-23 arms.

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

### The style flags decide the pipeline, and two of them used to decide nothing

`Display.TextDisplay` carries five bits. `FLAG_ALIGN_LEFT`/`RIGHT` were consumed;
`FLAG_SEE_THROUGH` and `FLAG_USE_DEFAULT_BACKGROUND` were decoded, carried all the way to the draw
site, and dropped — disclosed in `gpu/display_text.rs`'s own module doc as a simplification, and
tracked by nothing.

`FLAG_SEE_THROUGH` is not cosmetic. `TextDisplayRenderer.submitInner` picks
`Font.DisplayMode.SEE_THROUGH` and `RenderTypes.textBackgroundSeeThrough()` for it, resolving to
`TEXT_SEE_THROUGH`/`TEXT_BACKGROUND_SEE_THROUGH` — both `withDepthStencilState(Optional.empty())`,
i.e. **no depth test and no depth write**. An entity carrying that flag is deliberately placed inside
or flush against geometry, which is what a server-side hologram usually is, so drawing it through the
depth-tested pipelines occludes or fights it for exactly as long as the flag is set. There is a third
pipeline for it now (the two vanilla see-through pipelines have identical depth state and differ only
in a shader this pass does not port), drawn **last** so a range that writes no depth cannot occlude
the two that do.

`FLAG_USE_DEFAULT_BACKGROUND` resolves to `(int)(getBackgroundOpacity(0.25F) * 255) << 24` =
`0x3F000000`. Note that is **one alpha step** from `Display.TextDisplay`'s own accessor default of
`0x40000000`, which `display_entities.rs` already carries under a similar name: two different
defaults, and folding them together would have been invisible. `Options.getBackgroundOpacity` returns
its fallback whenever `backgroundForChatOnly` is set and vanilla defaults that on, so the fallback is
used unconditionally rather than reading an accessibility option this client does not model.

Which pipeline a display lands on is decided by `partition_display_text`, a free function precisely
so it is assertable with no GPU. Its gate is a pair: the identical display differing only in the flag
must put every vertex in the depth-tested ranges and none in the see-through one, then the exact
reverse.

### The glyph pipeline's polygon offset is load-bearing — do not merge it away

`gpu/display_text.rs` builds **four** pipelines from one descriptor: the
background panel through `RenderPipelines.TEXT_BACKGROUND`'s plain
`DepthStencilState.DEFAULT`, the glyphs' drop shadows through
`RenderPipelines.TEXT_POLYGON_OFFSET`
(`DepthStencilState(GREATER_THAN_OR_EQUAL, true, 1.0F, 10.0F)`, which flips
to `constant: -10, slope_scale: -1.0` in this project's `[0,1]` depth), and
the ink itself through **two** of that same offset — see the drop-shadow
section below. (The fourth is the see-through pipeline above.) They look
identical apart from the bias, which is exactly why one earlier version
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

**How little room there actually is, measured.** That 0.00025-block separation, expressed in ULPs of
the stored `f32` through this renderer's `Depth32Float` and **forward** `[0,1]` projection (near
`0.05`, far `512`): **53** at 2 blocks, **9** at 5, **3** at 8, **2** at 10, **1** at 14, **1** at 20,
**0** at 64 — bit-identical. Through vanilla's *reversed*-Z it never falls below **53**. So the bias
is the only thing separating the two planes past about ten blocks, and the consequence generalises
past this pass: **any ported sub-millimetre depth separation is unresolvable here at ordinary viewing
distance**, because reversed-Z is the arrangement that makes vanilla's constants work and this
renderer does not have it. Pinned as an exact per-distance curve in `lodestone_render::display`'s own
tests, with the reversed-Z column as the control — computed straight from the two planes, not as
`1.0 - forward`, which inherits the forward value's rounding and reads `0` ULPs.

`text_display_pixels.rs` cannot see this and never could: it asserts only
that more than 100 non-sky pixels land in the entity's rect, which the panel
alone satisfies. Any gate for this has to measure the **ink** against a
reference with the panel switched off.

### The drop shadow is separated by polygon offset, and vanilla's geometric offset is deliberately *not* ported

Owner report, once the drop shadow landed: *"the shadow text is z-fighting with the real text in
places where both are on the same 'pixel' for holograms"*.

A shadow is the same ink rect one font pixel away **in the text's own plane**. An in-plane
translation only changes depth when the plane is oblique to the view — which is a hologram seen from
anywhere but straight on, and never `gpu/nametag.rs`'s always-camera-facing plane, which is why the
report named holograms. Where the two quads overlap they are two *different* triangles interpolating
window `z` at one pixel, so float rounding decides the winner per fragment. The symptom being
**speckle** is itself the diagnosis: two exactly parallel planes one representable step apart flip
whole, so per-pixel fighting means the surfaces are not parallel.

Vanilla separates them geometrically — `BakedSheetGlyph.renderChar` emits the shadow at local
`z = 0` and a shadowed glyph at `0.03`. **That port was written, measured, and removed**, for two
reasons in order of weight:

- It **encodes which side is the front into the geometry**, and a `text_display` is visible from
  both. `0.03` local is `0.00075 · scale` blocks along the plane's own normal, so it moves the glyph
  toward the eye from the front and *away* from it from behind, where it swamps any ULP-denominated
  correction because it is orders of magnitude larger.
- It is **under a ULP where it matters anyway** — three times the panel-versus-ink separation the
  table above already measures at `0` ULP past 64 blocks.

What replaces it: the shadow keeps vanilla's `TEXT_POLYGON_OFFSET` unchanged (so text flush against a
block face still beats the face), and the ink takes that offset **twice, in both terms**. The slope
term is doubled and not just the constant because the constant is denominated in ULPs of the
primitive's own depth — view-angle-blind — while the rounding a near-grazing plane has to beat grows
with its depth *gradient*, which only the slope term tracks. A polygon offset is measured from the
camera rather than baked into the geometry, which is precisely why it survives being walked around.

Measured, twelve headless configurations spanning face-on to 85° oblique, front and back, 3 to 24
blocks at constant angular size. Ink lost of ~15–18k drawn, worst row per group:

| shadow/ink separation | 70° back | 80° back | 85° back | 80° front |
|---|---|---|---|---|
| one pipeline, no geometry — as shipped | 1,014 | 1,141 | 2,883 | — |
| constant term only, no geometry | 4 | 34 | 1,204 | — |
| constant + slope, **plus** vanilla's `0.03` | 101 | 297 | 3,120 | 189 |
| **constant + slope, no geometry** | **0** | **0** | **0** | **0** |

Row three is the one worth reading twice: **the faithful port made things worse than doing nothing**
from behind. Row two is why the slope term is doubled.

Two mechanisms considered and not taken. Drawing the shadow with depth *write* off removes the
contest outright and is precision-proof — but the shadow range is batched across every display in the
frame, so a near display's shadow would stop occluding a far display's ink drawn later in the same
pass; keeping the write is what lets the four ranges stay global instead of per-display. And giving
the shadow *no* offset (leaving the ink on vanilla's single step) loses the shadow's own protection
against world geometry, which is what vanilla's offset on text exists for.

The gate is
`world_text_over_geometry_pixels.rs::a_glyph_wins_against_its_own_drop_shadow_at_every_distance_and_angle`,
which sweeps **obliquity as well as distance** — the axis every other `text_display` fixture in this
tree was holding fixed. At the ~40° look those fixtures use, a build with no separation at all loses
**1 ink pixel of 438**; at 80–85° the same build loses **983–2,974 of ~15–18k**.

### How the two model consumers are posed

Both compose vanilla's own chain and differ only in what sits on the right of
it.

**`block_display`** (`DisplayRenderer.BlockDisplayRenderer`) is
`placement` and nothing else: `submitInner` is one `blockModel.submit` at the
pose the base `submit` composed, and the block model's quads are block-local
`0..1`. There is **no `-0.5` shift** — that belongs to the falling block,
whose entity spawns at the cell *centre*; borrowing it puts every hologram
half a cell north-west, which reads as a model-origin bug. `merge_block_displays`
asserts this against the falling-block hypothesis explicitly.

**`item_display`** (`DisplayRenderer.ItemDisplayRenderer`) is `placement`,
then `Axis.YP.rotation(PI)`, then the item's own `display` transform for its
`ItemDisplayContext` — the last applied inside `ItemStackRenderState.submit`,
which is why `display_matrix` composes on the right exactly as it does for a
framed or campfire item. The half-turn is easy to drop and nearly invisible,
since an item model is close to symmetric about its own Y axis.

**`ItemDisplayContext.NONE` is a real context, not a missing one.**
`Display.ItemDisplay`'s accessor default is `NONE`, and
`ItemTransforms.getTransform` answers it with `ItemTransform.NO_TRANSFORM` —
the identity pose. So `/summon item_display {item:{…}}` with no
`item_display` tag draws its model unscaled and unrotated, filling the whole
block. An earlier version of this seam defaulted to `FIXED` instead, on a
stated (and false) belief that `NONE` "draws nothing at all"; that would have
silently applied the item frame's half-scale pose to every context-less
hologram. `lodestone_assets::DisplaySlot` deliberately has no `NONE` variant
for the same reason — that context selects no `display` key — so
`display_slot_for_context` returns `Option<DisplaySlot>` and the `None` arm
poses with `DisplayTransform::default()`.

### Brightness override

`Display.DATA_BRIGHTNESS_OVERRIDE_ID` (index 16, `INT`) carries
`Brightness.pack()`'s `block << 4 | sky << 20`, or `-1` for "none".
`DisplayRenderer.getSkyLightLevel`/`getBlockLightLevel` take its nibbles
*instead of* the sampled lightmap whenever it is set — which is what makes a
server's `brightness:{sky:15,block:15}` hologram readable in a dark room —
so `DisplayDraw::override_light` repacks it into this renderer's own
`sky << 4 | block` byte and both merge sites prefer it over
`EntityLightSource::sample`. The two layouts differ, so this is a repack and
not a passthrough; the gate uses `(block 7, sky 12)` rather than a symmetric
`(15, 15)` precisely so a swap cannot pass.

Index 16 has six `INT` claimants in the committed
`EntityDataIndexOracle` dump (`Creeper.DATA_SWELL_DIR`,
`EnderDragon.DATA_PHASE`, `Phantom.ID_SIZE`, `Warden.CLIENT_ANGER_LEVEL`,
`WitherBoss.DATA_TARGET_A`), none of them a `Display`, so the decode arm is
gated on the whole family rather than per subtype — the field is declared on
the base class. The `-1` sentinel is carried through to the consumer rather
than folded to absence at the decode, because "explicitly cleared" and "never
reported" are different states and a real all-zero override packs to `0`.

### Named deviations, shared by both new consumers

* **No interpolation.** `Display.RenderState`'s getters are read at
  `interpolationProgress`; this seam has no interpolation clock, so a display
  told to move over 20 ticks snaps. Disclosed in `display_entities`'s module
  doc as a fidelity loss, not a correctness one.
* **No `viewRange` cull.** `Display.shouldRender` scales render distance by
  `DATA_VIEW_RANGE_ID` (index 17), which nothing decodes yet. The frustum
  test is the only cull, so a short-view-range display draws further out than
  vanilla would draw it — more geometry, never less.
* **No `glowColorOverride` outline**, on either — `MovingBlock` carries no
  outline channel, the same gap primed TNT's white flash already records.
* **A `minecraft:special` item in an `item_display` draws its inventory
  form**, because `ItemVariants::resolve` answers a `Special` output with the
  GUI fallback. Routing it to a block-entity rig is `entity_passes.rs`'s
  `special_item_instances`, which this seam does not own.

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
