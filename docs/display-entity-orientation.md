# Display entity orientation

## What it is

`lodestone_render::display`: the shared geometry every `text_display`/
`item_display`/`block_display` entity carries — the billboard orientation
that decides which way it faces, and the `translation`/`left_rotation`/
`scale`/`right_rotation` transformation on top of it. A faithful port of
`DisplayRenderer.calculateOrientation` and `Transformation.compose` (`26.2`).

**This module has no producer yet, and that is deliberate, not an
oversight.** `crates/protocol/v770` has no clientbound metadata decode for
the `Display` entity family at all — no `MetadataField` variant, no adapter
arm — so nothing anywhere in this codebase currently constructs a
`text_display`/`item_display`/`block_display` entity outside this module's
own unit tests. There is, right now, no way for a `text_display` entity to
exist client-side, let alone reach this geometry. See "What would need to
exist for this to go live" below.

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

### What would need to exist for this to go live

Three things, none of which live in this session's writable crates:

1. A `MetadataField` variant (or several) for the `Display` family's synced
   entity-data indices — billboard mode, the four transformation fields,
   view-range/shadow-radius/shadow-strength, etc. — in `crates/protocol/`.
2. An adapter arm in `crates/protocol/v770` that decodes those indices off
   `SET_ENTITY_DATA` into the new field(s).
3. A shell-side consumer that reads the decoded fields, calls
   `display_orientation`/`display_placement_matrix`, and feeds the result
   into an `EntityDraw`/GPU pass — the same shape `entities.rs`'s existing
   `extract_entity_draws` already gives every other entity kind.

**Do not wire an inert field to this module in the meantime.** A struct or
component that exists but that nothing constructs outside a test is exactly
the island shape CLAUDE.md's evidence standards call out repeatedly — the
correct state until the protocol half lands is "real, tested geometry with a
named, disclosed absence of a producer", not a half-wired field that looks
connected but never receives real data. Do not delete this module either:
the geometry is real, tested, and will not need re-deriving once the
protocol half exists.

## Configuration

None — every input is a per-frame value a caller already holds.

## Dependencies

`glam` only. No GPU device, no asset manager — see "How it works" above for
why that is deliberate; a wire producer is the missing dependency, tracked
above rather than pretended-around.

## Verification

```bash
cargo test -p lodestone-render --lib --no-fail-fast -- display::
```
