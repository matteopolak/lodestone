# Held items

## What it is

Everything about what the local player's hand shows and how it moves: the first-person item/arm draw
itself, the dip-and-raise when the held item changes, the arm pose vanilla selects while an item is in
use (drawing a bow, blocking with a shield), the swing animation shared by mining/attacking/placing, and
the special case of items (chests, skulls) that have no baked model at all and need a block-entity rig in
hand.

## How it works

### First-person held item

The local player's hand replaces the bare first-person arm with the selected item's geometry — never
both at once, it's a fork. The pose chain is `T(...) · swingArm(...) · itemArmAttackTransform(...) ·
display_matrix(firstperson_righthand)`, transcribed term-for-term from vanilla — its translation and
swing-amplitude constants are *different numbers* from the bare-arm chain's own constants (close enough
to look like a rounding difference, off by enough to clip the frame edge), so the two chains share no
code beyond the swing-progress scalar. The pass runs at the very end of the frame with **depth cleared**
(vanilla does the same), or the item is invisible whenever world geometry is within about 0.75 blocks of
the eye — exactly while mining. Its camera group is a projection matrix alone with **no view matrix**,
since the item is posed directly in camera space; feeding it the ordinary view-projection parks the item
at the world origin. The projection's FOV is a fixed 70°, not the player's real FOV, so the item keeps a
constant apparent size while sprinting. The held item draws through the same `ModelPipeline` (same
stitched atlas, tint palette, animation slots) that terrain and block items use — introducing a second
texture group here isn't possible; the model shader already sits at wgpu's four-bind-group floor.

Only the main hand is drawn; the off hand has no source at all. An item with no baked geometry (a
special-rendered item — see below) falls back to the bare arm rather than nothing, which is closer to
correct than an empty screen. Only the `WHACK` swing-animation type is modelled; `STAB` (spear) and
`NONE` read as identity at rest, so a resting hand looks right for every item, but a mid-swing spear
currently gets the generic swing rather than its own thrust.

### Held-item equip animation

Switching the visible item triggers a dip-and-raise, driven by a small state machine that lives in the
renderer (not the player), because that's genuinely where vanilla's `ItemInHandRenderer` keeps it — the
player only owns the *selected slot*; the renderer owns the lag between selection and what's drawn. It
steps in whole 20 Hz ticks off the wall clock (never a fraction), so the animation takes the same time at
30 fps as at 240. A full swap (dip fully, exchange the visible item, raise fully) is 6 ticks (300 ms); the
dip rate is a fixed ±0.4 of full height per tick.

The fork on **which item is currently visible** (not which is selected) matters: branching on the
selection instead produces a visibly wrong animation — you'd see the *new* item drop and return, instead
of the old one leaving. The retrigger condition is vanilla's own: it fires on any value change of the
visible stack (different item, same item at a different count, same item with changed durability/other
components), and deliberately does *not* fire on an inventory resync that leaves the value unchanged but
swaps the underlying object identity.

Known gaps: no attack-cooldown dip (needs per-item attack-speed state not currently tracked), no
"hands busy while using an item" freeze, no re-raise on item use completing, and the off hand isn't
animated because it isn't drawn at all.

### Item-use arm poses

Vanilla derives a humanoid's arm pose (drawing a bow, holding a shield up, aiming a crossbow, etc.) from
two independent bits, on two different bytes, depending on the kind of entity: a **player**'s pose comes
from the ordinary `LivingEntity` using-item bit, but a **mob**'s (e.g. a skeleton's ranged attack) comes
from its separate `Mob` aggressive bit — a skeleton's ranged-attack AI never sets the using-item bit at
all, so keying every entity's pose off that one bit correctly poses a player and silently poses *no
mob at all*. The override that draws a bow pose while aggressive is keyed per-renderer (a specific set of
skeleton-family renderers), not per-model — an aggressive zombie holding a bow does not get this pose in
vanilla, and a zombie's own forward-arms animation always overwrites any item pose applied to it
afterward, which is correct, not a wiring failure.

Both the using-item bit and the aggressive bit sit at metadata indices that are **ambiguous on the
wire** — the same index is reused by unrelated fields on other entity types (an arrow's crit flag shares
the using-item byte's index; an armour stand's "show arms" flag and a display entity's billboard-mode
field share the aggressive byte's index). Surfacing either bit requires knowing the concrete entity's
class first (an `is_living`/`is_mob` census column, generated from a jar dump — never hand-counted;
metadata index collisions recur throughout this codebase and the fix is always the same: dump the real
jar, and pick the narrowest column that actually separates the true claimants at that index).

Vanilla also gives an item a raised-arm pose merely for being *held* (not in use) — but only for a
player/avatar-family renderer; an ordinary mob holding the same item keeps its arms down, because the
per-renderer method every humanoid mob overrides ends in a different fallback than the player/avatar
one. Getting this backwards raises the arm of every armed mob and every decorative armour stand holding
a weapon.

The draw fraction for "how far through the use action" is not sent over the wire, so the client keeps
its own tick counter, seeded (and reset) only on a genuine rising edge of the flag — resetting on every
repeated metadata byte (which servers resend routinely) would leave a bow permanently un-drawn while
still looking correct at the wire level.

### Arm swing animation

One scalar, `attack_anim` (0.0..=1.0), drives three separate consumers: the first-person arm, the local
player's own third-person body, and (via a separate wire-driven per-entity clock) every other tracked
entity's swing. It is a **sawtooth**: it climbs across a fixed duration and drops to 0 in a single tick,
so interpolating it for partial-tick rendering needs a forward-wrapped delta (vanilla's own rule) — a
plain lerp runs the arm backwards through the whole arc for one frame every time a swing restarts, which
during hold-to-mine is most of the animation. The clock must be driven per **tick**, never per frame, or
swing speed becomes frame-rate dependent.

Left-click always swings, unconditionally, including a miss. Right-click swings only when vanilla's own
locally-computed interaction result says so — most block/item uses do not swing at all (a raised shield,
a drawn bow, eating), and a couple of dedicated per-item tables approximate which items do. Right-clicking
an entity is left unconditionally swinging (a known, deliberate over-approximation — it needs client-side
per-entity interaction logic this client doesn't carry).

Remote entities get their own three-field swing clock (a deliberate subset of the local player's, since a
tracked entity's walk/head-orientation state already lives on a different ECS entity for the same mob) fed
by the `ANIMATE` packet's swing-main-hand action only — the other four documented actions (wake up,
off-hand swing, the two critical-hit variants) are each either not an animation this renderer draws, or
animate the arm this renderer doesn't model.

### Held block-entity items

Some items (chests, shulker boxes, skulls, banners, a decorated pot, a trident) have **no item model and
no block model at all** in vanilla — every triangle for them comes from a dedicated block-entity-style
renderer. Drawing one in hand (or dropped, or in another entity's hand or head slot, or in an item frame)
means resolving a rig and a standalone (non-atlas) texture sheet, not baked quads, so these items need a
completely separate resolution path from ordinary items: an item-model resolver that can answer "this
item has no baked geometry, it needs a *special* rig" reachable from every surface that draws item
geometry, a shared `(kind, item path) → (rig, sheet)` lookup, and a shared placement function that turns
a per-surface pose matrix into a posed instance. There is deliberately **no flat-sprite fallback** for
these — the base item models named in the jar for them carry no drawable geometry at all, only a
`display` transform map, so "fall back to a flat icon" was never actually available as a path.

The hand draws these through the **block-entity render pass**, not the ordinary held-item model pass,
because their sheets are standalone (not part of the stitched block atlas) and the model pipeline has no
spare bind-group slot for a second texture; the block-entity pass's own pipeline has room for one. The
pose applied is the ordinary held-item swing/dip chain, just with the special rig's own rest-pose part
transforms and no additional per-item pose override (a held chest's lid never opens).

## How to change it

* **Adding a use-pose or a swing/equip variant**: add the enum arm, then the branch in the pose/animation
  function, then the selection rule that decides which entities get it — all three steps, or the new arm
  compiles and tests green while reaching zero mobs (the shape of bug this whole cluster keeps
  rediscovering: a gate that starts downstream of the *selection* decision cannot see a wrong selection).
* **Adding a special-rig `kind`**: check whether the rig and sheet already exist in the corpus before
  writing new ones — several of these were resolver gaps, not missing geometry, and a second copy of a
  working rig is worse than none because both then look plausible.
* **Any per-tick interpolated scalar** (swing progress, walk distance, hurt-time, etc.) needs its own
  named read rule derived from vanilla's actual expression — do not assume a shared "lerp" abstraction
  covers all of them; several of vanilla's own per-tick values use a wrap, an extrapolation, or a bare
  subtraction instead of a plain lerp, and collapsing them into one generic interpolator has previously
  reintroduced the exact bug it was meant to prevent.
* **A metadata bit that selects a pose is almost always index-ambiguous** — check the real jar's
  per-index claimant list before trusting an existing census column to separate a new case.

## Configuration

None of these subsystems has a runtime flag. All constants (swing amplitudes, equip-dip rate and
duration, use-pose timing) are vanilla numbers hard-coded in `lodestone-render`/`lodestone-shell`.

## Dependencies

* `lodestone-render` — the first-person item/arm pose chains, `Skeleton::pose_arms_for_item`, the
  special-item rig lookup and placement (`entity.rs`, `entity_anim.rs`, `block_entity.rs`).
* `lodestone-entity` / `lodestone-ecs` — the local player's tick-driven swing/pose clocks
  (`pose::EntityPose`) and the remote-entity equivalents (`AttackSwing`, `ItemUse`, `MobState`), folded
  from decoded metadata.
* `lodestone-data` — the entity census columns (`is_living`, `is_mob`) that disambiguate colliding
  metadata indices.
* `crates/protocol/v770` — the metadata decodes feeding all of the above, and the `ANIMATE` packet decode
  for remote swings.
* `lodestone-shell` — `gpu/first_person.rs` (held item, equip state), `entities.rs` (pose/swing
  extraction into `EntityDraw`), `sim.rs`/`interact.rs` (local swing producers).
