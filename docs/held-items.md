# Held items

## What it is

Everything about what the local player's hand shows and how it moves: the first-person item/arm draw
itself, the dip-and-raise when the held item changes, the arm pose vanilla selects while an item is in
use (drawing a bow, blocking with a shield), the swing animation shared by mining/attacking/placing, and
the special case of items (chests, skulls) that have no baked model at all and need a block-entity rig in
hand.

## How it works

### First-person held item

A hand holding a stack draws the stack's geometry instead of the bare first-person arm — never both at
once, it's a fork. The pose chain is the hand offset `T(±0.56, -0.52 - 0.6·h, -0.72)`, then the swing
translation, then the swing rotations, then the stack's own first-person display transform
(`first_person_item_chain` / `first_person_item_matrix`), ported term-for-term — its translation and
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

Both hands draw, in one pass with depth cleared once, so an off-hand shield and a main-hand sword
depth-test against each other. The off hand is the main hand's chain with the arm sign flipped
(`Arm::Left` for a right-handed player) and the stack's own first-person **left**-hand display slot. A
hand showing a stack draws that stack and no arm; an empty **main** hand draws the bare arm, but an empty
**off** hand draws nothing — the asymmetry is the reference client's. The off hand never swings (this
client only swings the main hand) and never takes an item-use pose (only the main hand is ever used). A
drawn bow or crossbow hides the off hand for as long as the use lasts (`hands_to_render`). An item with
no baked geometry (a special-rendered item — see below) in the main hand falls back to the bare arm
rather than nothing; in the off hand it draws nothing. Handedness is a field of the frame
(`FirstPersonHandsFrame::main_arm`) and the whole draw path follows it, but this client has no main-arm
option, so production always installs `Arm::Right`.

The first-person pass models only the `WHACK` swing-animation type; `STAB` (spear) and `NONE` read as
identity at rest, so a resting hand looks right for every item, but a mid-swing spear currently gets
the generic swing rather than its own thrust.

### Held-item equip animation

The hand state lives in `Sim` (`sim/first_person_hands.rs`), not the renderer: the reference client
keeps it on the local player and advances it in the player's own tick, right after the attack counters
advance, and two of its inputs (the cooldown scale and "hands busy") are tick-rate player facts. Each
frame `Sim::first_person_hands_sample` interpolates it at the frame's partial tick, `hands_frame` turns
it into a `FirstPersonHandsFrame` (each shown stack resolved through the same `stack_icon` record the
hotbar draws), and `RenderState::set_first_person_hands` installs it. The renderer only draws.

Per hand and per tick: save last tick's height; if the shown stack still **matches** the held one (same
item, same count, every component equal except `minecraft:damage`) adopt the held one at once; step the
height toward its target by at most `0.4`; and once the height is below `0.1`, exchange the shown stack
for the held one. The target is `0` while the shown stack differs from the held one, else `1`. So a
full swap is `0.6, 0.2, 0.0` down (exchange on the third tick, out of sight) and `0.4, 0.8, 1.0` back
up — 300 ms — and the two hands swap independently. The drawn lowering is `1 - lerp(partial, previous,
height)`, times `-0.6` blocks.

Damage is excluded on purpose: a pickaxe losing durability while mining must not dip, while eating one
bread of a stack (a count change) does. A fresh `Sim` starts both hands empty and fully lowered, so the
hand rises into view over the first three ticks in a world — the reference client's own start.
`RenderState::set_main_hand_source` survives as a convenience for GPU gates with no `Sim`: it installs a
resting right-handed frame holding one main-hand stack.

**Attack-cooldown dip.** The main hand's target, when its stack matches, is not `1` but `scale³`, where
`scale = clamp((swap_ticks + 1) / delay, 0, 1)` and `delay = 20 / attack_speed` ticks — the same delay
the crosshair indicator divides by. `swap_ticks` is `lodestone_ecs::ItemSwapTicker`, the second of two
attack counters advanced together by `tick_attack_strength`. They reset differently:

| event | `AttackStrengthTicker` | `ItemSwapTicker` |
|---|---|---|
| entity attack, swing at nothing, aborted dig | reset | reset |
| main hand changes to a *different item* (`tick_first_person_hands`) | reset | reset |
| piercing weapon's stab | reset | kept |
| count/component change of the same item | kept | kept |

`Sim::reset_attack_strength_ticker` / `reset_only_attack_strength_ticker` are the only writers. With a
sword (`1.6`, delay `12.5`) an attack from rest gives heights `0.6, 0.2, 0.032768, 0.064, …` — down at the
`0.4` step limit, then back up along the cube — and `1.0` again on the twelfth tick. Unarmed (`4.0`,
delay `5`) it is `0.6, 0.216, 0.512, 0.912, 1.0`. The off hand ignores the cooldown. The swing arc is
separate state (`HandSwingSource`) and is not touched by any of this.

**Item used.** A successful use snaps the used hand's height (and its previous height, so the snap is
immediate rather than eased between frames) to `0`; the ordinary step raises it `0.4, 0.8, 1.0`. The
calls are in `sim/actions.rs`: a predicted block placement, and the generic use when it starts a held use
(eating, drinking, drawing, blocking — the same gate that arms `UsingItem`), equips armour, or is an
item whose use swings (a thrown item). An item with no use of its own (a sword) passes and its hand stays
up. Holding use through a finished bite re-runs the press path, so each new bite snaps again. None of
this touches the swing.

**Hands busy.** Controlling a boat with a movement key held (`ControlledVehicle` is a boat and this
tick's `MovementIntent` has a forward or strafe component — the same bits the boat's paddle input is
built from) lowers both hands by `0.4` a tick to `0`, whatever is held, and makes `begin_attack_live`
and `use_item_live` return before doing anything. Letting go raises both by the ordinary rule. A
passenger, or a land mount, is never busy.

Known gaps: a block interaction that consumes the held stack without placing (bone meal, a bucket) does
not snap the hand; the count change still dips it as an ordinary swap when the server's slot update
lands. The off hand is never used, so it never snaps. An off-hand filled map draws nothing (the one-handed map pose is not ported), and a main-hand
map always takes the two-handed pose even with something in the off hand. The per-item
`swap_animation_scale` and swap-animation opt-out of an item-model definition are not read; every item
animates at scale `1`.

### Item-use arm poses

Vanilla derives a humanoid's arm pose (drawing a bow, holding a shield up, aiming a crossbow, etc.) from
two independent bits, on two different bytes, depending on the kind of entity: a **player**'s pose comes
from the ordinary living-entity using-item bit, but a **mob**'s (e.g. a skeleton's ranged attack) comes
from its separate mob-flags aggressive bit — a skeleton's ranged-attack AI never sets the using-item bit at
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

Legacy remote animation events retain `AttackSwing`, including its main-hand
filter and six-tick clock. An explicit `EntitySwingAnimation` carries a hand,
`ItemAnimationKind`, and signed duration into `ExplicitAttackSwing` on living
entities. Starting a description snapshots its effect-adjusted duration: Haste
or Conduit Power subtracts one plus its amplifier; otherwise Mining Fatigue
adds twice one plus its amplifier. A restart is accepted only after half of the
current duration, or before its first tick. Changed descriptions reset both
animation samples; identical descriptions preserve the samples and wrap a
restart forward during partial-tick interpolation. Non-positive durations never
divide, and the description clears after its duration boundary.

Extraction carries the supplied kind and physical arm in `AnimInput` alongside
the duration-derived fraction. A humanoid's `Whack` uses its selected arm's
melee arc. `Stab` uses separate preparation, thrust, and return curves; `None`
keeps the shared torso orbit without a hand arc. Both the body draw and
`Skeleton::translate_to_hand` consume the same posed skeleton, so held items
follow that arm. Off-hand selection is opposite the known main arm. Mob
left-handed metadata is supported; remote player main-arm metadata is not yet
available and defaults to right-handed. Species arm overrides, including the
undead arm pose, still have their existing precedence; held spear rest/use
poses and their override gates are separate work.

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
* **Changing the first-person hand state** (`sim/first_person_hands.rs`): the per-tick rule has unit
  tests with hand-derived sequences, `sim/tests/first-person-hands.rs` follows the production `Sim`
  paths (attack, stab, swap, eat, paddle, held uses) to the pose the hand pass draws, and
  `tests/gpu/first_person_hands_pixels.rs` (`--ignored`, needs a GPU) gates the pixels: the off hand's
  mirror and its own swap, the cooldown dip against a projection-derived line, and the main hand's use
  poses being unaffected by the off hand. A new reset site for the attack counters belongs in
  `Sim::reset_attack_strength_ticker` or `reset_only_attack_strength_ticker`, never a direct write.
* **A metadata bit that selects a pose is almost always index-ambiguous** — check the real jar's
  per-index claimant list before trusting an existing census column to separate a new case.

## Configuration

None of these subsystems has a runtime flag. Swing amplitudes, equip-dip timing,
and use-pose timing are fixed in `lodestone-render` and `lodestone-shell`.
Explicit remote swing duration comes from the packet and the entity's active
effect state at animation start.

## Dependencies

* `lodestone-render` — the first-person item/arm pose chains, `Skeleton::pose_arms_for_item`, the
  special-item rig lookup and placement (`entity.rs`, `entity_anim.rs`, `block_entity.rs`).
* `lodestone-entity` / `lodestone-ecs` — the local player's tick-driven swing/pose clocks
  (`pose::EntityPose`) and the remote-entity equivalents (`AttackSwing`, `ExplicitAttackSwing`, `ItemUse`, `MobState`), folded
  from decoded metadata.
* `lodestone-data` — the entity census columns (`is_living`, `is_mob`) that disambiguate colliding
  metadata indices.
* Version adapters — metadata and legacy remote-animation decoding; the 26.3
  adapter additionally supplies explicit swing kind, hand, and duration.
* `lodestone-shell` — `sim/first_person_hands.rs` (per-hand shown stack and height, ticked in
  `Sim::step`), `gpu/first_person.rs` (both hands' draws from the installed frame), `entities.rs`
  (pose/swing extraction into `EntityDraw`), `sim.rs`/`interact.rs` (local swing producers).
