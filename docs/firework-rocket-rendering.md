# Firework rocket rendering

## What it is

The flying firework rocket entity: a billboarded item model, spun onto its
flight axis when it was fired from a crossbow, and suppressed entirely when it
is the elytra boost riding inside a gliding player.

## How it works

### The chain

| link | symbol |
| --- | --- |
| wire | `DATA_ID_FIREWORKS_ITEM` (8, `ITEM_STACK`), `DATA_ATTACHED_TO_TARGET` (9, `OPTIONAL_UNSIGNED_INT`), `DATA_SHOT_AT_ANGLE` (10, `BOOLEAN`) |
| decode | `lodestone_v770`'s `IDX_FIREWORK_ATTACHED` / `IDX_FIREWORK_SHOT_AT_ANGLE` |
| event | `EntityMetadataUpdate::firework_attached` / `firework_shot_at_angle` |
| ECS | `lodestone_ecs::entity::FireworkFlags` |
| draw record | `EntityDraw::item` (already carried) + `EntityDraw::firework` |
| draw | `PreparedItems::merge_firework_rocket` |

The **stack needed nothing new**. `ITEM_STACK` is self-identifying by
serializer, so the decode routes it ahead of the index match and a firework's
real stack has been reaching `EntityDraw::item` all along. Only the two flags and
the draw itself were missing.

### Why this is not a row in `thrown_item_for`

`FireworkEntityRenderer` draws a billboarded item model in
`ItemDisplayContext.GROUND`, exactly as `ThrownItemRenderer` does, so adding a
row to that table looks like the obvious one-line fix. It is wrong, and the
reasoning was already on record in `thrown_item_for`'s own doc before this
landed:

* the table means **"entity types registered to `ThrownItemRenderer` in
  `EntityRenderers`"**, and a firework is not one of them;
* its membership is checked against the vanilla registration list by a parity
  gate in `crates/lodestone-render/tests/thrown_and_held_item_pixels.rs`, which
  explicitly asserts `firework_rocket` is absent — so the change would fail
  loudly, which is the good case;
* the two mechanical differences would not fit anyway. A firework has **no scale
  term** (`ThrownItemRenderer` scales before the billboard; this does not), and
  it carries a rotation the table has no column for.

So it gets its own path and its own `world-coverage` claim, exactly as the three
display renderers do.

### The three rotations

`FireworkEntityRenderer.submit` applies, **after** the camera orientation and
only when the angle bit is set:

```text
Axis.ZP 180 deg  ·  Axis.YP 180 deg  ·  Axis.XP 90 deg
```

These are composed into the `orientation` matrix handed to `thrown_item_mesh`,
which takes it as an opaque `Mat4` — so no signature grew.

The visible effect is worth stating because the measurement looks wrong at first
glance: the rotations tip the flat item sprite about the **camera's own X axis**,
so a camera looking straight at an angled rocket sees it nearly edge-on. The
pixel gate measures 821 covered pixels plain against 140 angled. That is the
correct result, not a lost draw.

### The two suppressions

* **Attached** — `FireworkRocketEntity.shouldRender` returns false when the
  rocket is riding a gliding player, because that is the elytra boost rather
  than a rocket in flight. Without it a boosting player has a rocket sprite
  hanging inside them for the duration.
* **Stack fallback** — a rocket whose item field was never marked dirty falls
  back to a plain `minecraft:firework_rocket`. That is faithful rather than a
  papering-over: vanilla's accessor is *initialised* to that stack, so such a
  rocket genuinely draws as a plain one.

### The one field that needed a class guard

Index 10's `BOOLEAN` has three claimants in the committed jar dump —
`AbstractArrow.IN_GROUND`, `Interaction.DATA_RESPONSE_ID` and
`FireworkRocketEntity.DATA_SHOT_AT_ANGLE` — and **none of the three is a living
entity**, so neither the `living` nor the `mob` census separates them. That is
why `MetadataClass::FireworkRocket` exists. Ungated, an arrow stuck in the ground
would report itself as crossbow-fired.

Its two siblings need no class. The stack is self-identifying by serializer, and
index 9's `OPTIONAL_UNSIGNED_INT` has exactly one claimant *at that index* (the
other three sit at 19 and 20).

## How to change it

* **`EntityDraw::firework` being `None` does not mean "do not draw".** A plain
  shot rocket reports neither flag, so the draw site keys on the entity **type**
  and reads the flags with their vanilla defaults. Only `attached` suppresses.
* `FireworkFlags` is folded with `entry`/`and_modify`, not `insert`: the two
  flags arrive as separate metadata fields and a packet mentions only what
  changed, so replacing would clear the other. In practice vanilla sets at most
  one per rocket, so the merge guards against a plugin rather than against
  vanilla — it is still the honest fold.
* The **firework explosion particles** are a separate feature and not this one;
  this doc covers the flying rocket only.

## Configuration

None.

## Verification

`crates/lodestone-shell/tests/firework_rocket_pixels.rs`, through the real
`RenderState::render`, with three arms:

| arm | measured |
| --- | --- |
| it draws | 821 px, `projectiles_drawn` 1 |
| the angle bit changes the pose | 865 px differ from plain; angled coverage 140 px |
| attached draws nothing | 0 px, `projectiles_drawn` 0 |

Coverage alone cannot see the second: both poses draw the same item at the same
place and size, so a build ignoring `DATA_SHOT_AT_ANGLE` would pass every
coverage check.

The neuter was observed: disabling the type-check branch took every arm to 0.
Note the gate needs a real baked atlas — `RenderState::new(.., None)` installs no
`ModelRenderer` and the whole item path returns before any of this, which reads
exactly like a dead draw path. It was written that way first and reported a flat
zero.

Decode side, in `crates/protocol/v770`:
`index_10_bool_is_the_angle_bit_only_for_a_firework` (with an arrow as the
control) and `firework_attached_distinguishes_empty_from_present` (wire `0` is
empty, wire `n + 1` is present — a decoder that dropped the `- 1` would suppress
every rocket in the game).

The pixel gate installs its own `EntityDraw`, so it verifies the draw only; the
decode gates cover the producer separately.

## Dependencies

`crates/protocol/v770` (the two flag decodes and the class), `lodestone-model`
and `lodestone-ecs` (the event fields and the component), and
`lodestone-shell`'s `gpu/world_items.rs`, which owns the item-billboard machinery
this reuses wholesale.
