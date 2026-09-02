# Projectile and effect rendering

## What it is

The draw paths for entities that are neither an ordinary mob rig nor a plain billboard: velocity-aligned
projectiles (arrow, spectral arrow, trident), firework rockets, lightning bolts, paintings, item frames
(and whatever hangs in one, including filled maps), and the three `Display` entity subtypes
(`text_display`/`item_display`/`block_display`). Each needed its own placement logic because none of them
is posed the way a `LivingEntityRenderer` mob is.

## How it works

### Projectiles (arrow, spectral arrow, trident)

A code-built cuboid rig aligned to the projectile's **velocity**, drawn through
`projectile_model_matrix(pos, yaw, pitch, scale) = T(pos) · Ry(yaw − 90°) · Rz(pitch) · S(scale)`.
This is *not* the mob placement matrix: `ArrowRenderer`/`ThrownTridentRenderer` extend `EntityRenderer`,
not `LivingEntityRenderer`, so there is no Y-flip and no `+1.501` ground lift — these rigs are authored
`+Y` up, unlike every mob rig. `projectile_pitch_offset_deg(model_name)` is the switch that selects this
placement (`0.0` for arrow/spectral_arrow, `90.0` for trident, `None` for everything else); adding a new
projectile's rig without adding an arm here draws it 1.5 blocks high and mirrored, and every mesh test
still passes.

Yaw/pitch come from the server's own `atan2(velocity)`-derived rotation, eased 20%/tick server-side and
quantised to the wire; the client keeps a small local physics simulation to animate flight between
reports and reconciles fully from each server packet. A projectile's `yRot` convention is the *opposite*
of a mob's body yaw (`+X` motion is `yRot = +90`, where a player facing `−X` also reads `+90`) — treat
the sign as one fact checked against a live server, not two independent guesses.

### Firework rockets

A billboarded item model — but **not** a row in the `ThrownItemRenderer` table other billboarded
projectiles use, because a firework has no scale term and carries a rotation that table has no column
for; it gets its own producer. Two metadata flags drive it: `attached` (suppresses the draw entirely
while the rocket is the elytra boost riding a gliding player) and `shot_at_angle` (a fixed three-axis
rotation composed after the camera-billboard orientation, only when the bit is set). The item stack
itself needs no wiring — `ITEM_STACK` is self-identifying by serializer and reaches the draw record
before any index match runs.

### Lightning bolts

Entirely client-side: `LightningBolt` declares no synced data at all, so a bolt puts nothing on the wire
beyond its spawn position, and its shape seed is rolled independently on each side — there is no
captured-bytes oracle for this feature and never can be; gates have to check structural invariants
instead. The geometry is four concentric hollow tubes traced along one seeded random walk and rebuilt
every frame, blended **additively** (`(SRC_ALPHA, ONE)`) — ordinary alpha blending over the same geometry
reads as a dull grey flash rather than white, because the bolt's own base colour is a dim blue-grey and
the white comes from four passes stacking. No hitbox means no frustum culling; the cost ceiling is a
fixed-capacity buffer. Not ported: the mid-strike reseed between flashes (needs per-bolt life/flash state
that isn't on the wire) — a bolt here holds one shape for its whole life, seeded from its entity id.

### Paintings

A flat slab, sized per-variant, drawn through its own pass rather than the mob corpus or a billboard.
Facing needs no extra decode: `HangingEntity` already writes its wall direction into the entity's
ordinary yaw, which the spawn packet already carries. 51 variants reduce to 9 distinct `(width, height)`
shapes. The wire's variant id indexes the *registry's* order, which is **alphabetical** — not the order
the bootstrap class registers entries in; this is a recurring trap for any dynamic (data-pack) registry
and must be settled against a captured `registry_data` fixture, never against the registration source.
The default variant sits at its accessor's default value, so an unmodified painting never gets a
variant field on the wire at all — the decoder must synthesize the default at spawn, the same shape as
the sheep-wool and creeper-flag defaults. The mesh is two parts: the front face samples the variant's own
sprite in a per-cell grid (an exact match to vanilla's cell-by-cell sampling), while the back and edge
faces tile one shared texture — collapsing either into a single stretched quad is visibly wrong on a
large painting. Light is sampled once per painting instance, not per cell, which is a known, deliberate
gap for a torch-lit wall behind a big painting.

### Item frames

The frame's own body is a **block model** baked at an entity's position — the same shape as a falling
block or a piston head — not a `ModelPart` rig, which is why it was originally missing from the mob
corpus entirely and needed its own producer rather than going through either the mob-rig path or the
ordinary block-state draw. What hangs in the frame (an ordinary item, a `minecraft:special` rig, or a
filled map) is four separate producers sharing one pose chain,
`item_frame_space = T(floor(anchor) + (0.5,0.5,0.5)) · Rx(pitch) · Ry(180 − yaw)`. Two things in that
chain are easy to get backwards and invisible when wrong: the anchor is the attachment block's *centre*
(two vanilla offsets — the packet's integer corner and the render dispatcher's own offset — cancel to
put it there, not at the entity's own centre), and the `180 −` term is required or the frame's back
plate faces into the room with its contents hidden behind it. Rotation is a separate metadata field,
gated by a dedicated `MetadataClass` because its wire index collides with an unrelated `Display` field.
Light differs three ways: the body (a floor for a glow frame), an ordinary item's contents, and a glow
frame's contents (full bright) each read a different constant.

**A single filter written for one purpose silently blanked every consumer of `EntityDraw::item` for this
whole entity type for as long as it existed** — the general lesson is to check who else reads a shared
input field and whether production ever assigns it anything but the default, not just whether some
producer writes it.

### Filled maps

The palette: a packed byte is `id << 2 | brightness`; the high six bits index a fixed base-colour table,
the low two select one of four integer brightness scalers whose enum order is **not ascending**
(the dimmest, `LOWEST`, is index 3) — sorting the table by "brightness" inverts every terrain contour.
Id 0 is fully transparent, so an unexplored cell is a hole, not a black square. The GPU texture is
retained and only rebuilt when a map's own colour revision changes, so an unrelated map's update costs
nothing. Two draw sites: held in the hand (forked *before* the ordinary item-render path, or it falls
back to the flat, blank `filled_map` sprite) and in an item frame (grouped by map id into one mesh, since
several frames can hold different maps). The map's picture stands a fixed, sub-block clearance in front
of its backing surface — vanilla's own physical separation — expressed here as two raster-depth steps
under one shared view-projection rather than as a second, independently-scaled camera matrix; the latter
loses precision at real in-game coordinates and visibly reverses the frame/map draw order as the camera
moves. The integrated server currently has no map-data store at all, so a singleplayer world never
receives map contents — a server-side gap, not a rendering fault.

### Display entities (`text_display` / `item_display` / `block_display`)

All three share one placement:
`pose = T(anchor) · orientation(billboard_mode, entity_yaw, entity_pitch, camera_yaw, camera_pitch) · Transformation(translation, left_rotation, scale, right_rotation)`,
composed in that exact order (translate, then left-rotate, then scale, then right-rotate — swapping
scale and the left rotation is invisible in a screenshot and simply wrong). The four billboard modes
each answer "which yaw, and which pitch, does this face with" differently:

| mode | yaw source | pitch source |
|---|---|---|
| `Fixed` | entity's own | entity's own |
| `Horizontal` | entity's own | camera's |
| `Vertical` | camera's | entity's own |
| `Center` | camera's | camera's |

That table *is* the entire behavioural difference between the modes — do not collapse it. The four
`Transformation` fields are declared once on the shared `Display` record and inherited by every subtype;
read them unconditionally rather than gating on "this subtype looks like it needs scaling" — a field
declared on a base schema type and wired for only the one subtype that obviously needs it is a recurring
mistake in this codebase (the same shape as a shield's inherited transform elsewhere).

`text_display` layers more on top: multi-line centring must measure each line's *styled* width (bold
widens the glyph advance), not a plain-text walk, or a bold line's centring drifts. Its drop shadow is
separated from the ink by *doubling* the polygon-offset slope term, not by vanilla's own geometric z-nudge
— that nudge encodes a fixed front/back assumption that is wrong for a panel visible from both sides, and
a faithful port of it measurably loses more ink from behind than omitting it entirely. `FLAG_SEE_THROUGH`
routes to a distinct no-depth-test/no-depth-write pipeline drawn last, so a hologram embedded in geometry
does not fight it. A brightness-override field packs sky/block nibbles and, when set, wins over the
sampled world light outright. `block_display` poses with no extra shift — it does *not* borrow the
falling-block entity's `-0.5` centering, which belongs to that entity's own spawn convention.
`item_display` adds a 180° turn plus the item's own per-context transform; `ItemDisplayContext::None` is
a real context meaning "identity pose", not "draw nothing" — treating it as `Fixed` silently applies a
different slot's scale to every context-less hologram.

## How to change it

* **A new projectile type needs both a rig entry and an arm in `projectile_pitch_offset_deg`.** The rig
  alone bakes, uploads and draws — 1.5 blocks high and mirrored — with every geometry test green.
* **A dynamic (data-pack) registry's wire order is alphabetical, not the bootstrap class's registration
  order.** Settle any such variant table against a captured `registry_data` fixture from a real server,
  never against the source that registers the entries.
* **A field whose value equals its own accessor default is never sent on the wire.** Any variant/flag
  decoder needs to synthesize the idle default at spawn rather than assume "absent" means "default-like".
* **Metadata index collisions recur through this whole cluster** (firework's angle bit, item-frame
  rotation, display's brightness override all share an index with an unrelated field on another entity
  type). Run the jar's entity-data-index dump rather than hand-counting, and pick a `MetadataClass` guard
  that actually separates the real claimants — a living/mob census column is usually the wrong axis here,
  since none of these types are living.
* **Check who reads a shared draw field, and whether production ever assigns it a non-default value** —
  not just whether some producer technically writes it. A filter added for one entity kind can silently
  blank the same field for every other consumer.
* **A field declared on a shared base record must be read unconditionally by every subtype that inherits
  it**, never gated on "does this subtype look like it needs it".

## Configuration

None of these subsystems has a feature flag. Two live-diagnosis knobs exist for the map/item-frame depth
seam specifically: `RUST_LOG=maps=debug` (and `pack_trace=debug` for placement) trace candidate
resolution frame by frame, and a handful of `LODESTONE_MAP_DISABLE_*` process-start switches (frustum
cull, back-face cull, depth test/write/bias independently) exist to bisect a live depth-ordering report
one axis at a time — see `gpu/maps.rs`'s own module doc for the full switch list.

## Dependencies

* `lodestone-assets` — projectile/painting rig and mesh data, block-model baking for item frames.
* `lodestone-render` — placement matrices and geometry (`entity::projectile_model_matrix`,
  `painting::painting_mesh`/`painting_matrix`, `entity::item_frame_*`, `lightning_bolt`, `display`,
  `map_item`).
* `lodestone-ecs` — the per-entity components these producers read (`FireworkFlags`, `PaintingVariant`,
  `ItemFrameRotation`, `Display*`), folded by `ingest::apply_entity_metadata`/`apply_display_metadata`.
* `lodestone-game::maps` — the map colour store (`MapStore`/`MapState`), independent of any render code.
* `crates/protocol/v770` — the metadata decodes and class guards for every field named above.
* `lodestone-shell`'s `gpu/` submodules — `entity_passes.rs`, `world_items.rs`, `moving_blocks.rs`,
  `maps.rs`, `lightning_bolt.rs`, `display_text.rs` each own one subsystem's actual draw call.
