# Held items

## What it is

What the local player's hand shows and how it moves: the first-person item and arm draw, the dip-and-raise when the held item changes, the arm pose while an item is in use (drawing a bow, blocking), the swing animation shared by mining, attacking and placing, and items (chests, skulls) with no baked model that need a block-entity rig in hand.

## How it works

### First-person held item

A hand holding a stack draws the stack's geometry instead of the bare arm, never both. The pose chain is the hand offset `T(±0.56, -0.52 - 0.6·h, -0.72)`, the swing translation, the swing rotations, then the stack's first-person display transform (`first_person_item_chain` / `first_person_item_matrix`). Its constants differ slightly from the bare-arm chain's (close enough to look like rounding, enough to clip the frame edge), so the two share only the swing-progress scalar.

- The pass runs last with depth cleared, or the item vanishes whenever geometry is within about 0.75 blocks of the eye (exactly while mining). Its camera group is a projection alone with no view matrix (the item is posed in camera space; the ordinary view-projection parks it at the world origin), with a fixed 70 degree FOV so the item keeps constant apparent size while sprinting.
- It draws through the same `ModelPipeline` (stitched atlas, tint palette, animation slots) as terrain; a second texture group is impossible since the model shader is at wgpu's four-bind-group floor.
- Both hands draw in one pass with depth cleared once, so an off-hand shield and main-hand sword depth-test together. The off hand is the main chain with the arm sign flipped (`Arm::Left` for a right-handed player) and the stack's first-person left-hand display slot. A hand showing a stack draws no arm; an empty main hand draws the bare arm but an empty off hand draws nothing. The off hand never swings or takes an item-use pose. A drawn bow or crossbow hides the off hand for the use (`hands_to_render`). A special-rendered item with no baked geometry falls back to the bare arm in the main hand and nothing in the off hand. Handedness is `FirstPersonHandsFrame::main_arm`; production always installs `Arm::Right`.
- Only the `WHACK` swing type is modelled; `STAB` (spear) and `NONE` are identity at rest, so a mid-swing spear gets the generic swing.

### Equip animation

Hand state lives in `Sim` (`sim/first_person_hands.rs`), advanced in the player's tick right after the attack counters (two inputs, cooldown scale and hands-busy, are tick-rate facts). Each frame `Sim::first_person_hands_sample` interpolates at the partial tick, `hands_frame` builds a `FirstPersonHandsFrame` (each stack via the same `stack_icon` record the hotbar draws), and `RenderState::set_first_person_hands` installs it. The renderer only draws.

Per hand per tick: save last height; if the shown stack matches the held one (same item and count, all components equal except `minecraft:damage`) adopt the held one; step height toward target by at most `0.4`; once height is below `0.1`, exchange shown for held. Target is `0` while they differ, else `1`. A full swap is `0.6, 0.2, 0.0` down (exchange on the third tick) and `0.4, 0.8, 1.0` up (300 ms); hands swap independently. The drawn lowering is `1 - lerp(partial, previous, height)` times `-0.6` blocks. Damage is excluded on purpose (durability loss while mining must not dip; eating one of a stack does). A fresh `Sim` starts both hands empty and lowered. `RenderState::set_main_hand_source` is a convenience for GPU gates with no `Sim`.

**Attack-cooldown dip.** The main hand's matching-stack target is `scale^3`, `scale = clamp((swap_ticks + 1) / delay, 0, 1)`, `delay = 20 / attack_speed`. `swap_ticks` is `lodestone_ecs::ItemSwapTicker`, the second of two counters advanced by `tick_attack_strength`:

| event | `AttackStrengthTicker` | `ItemSwapTicker` |
|---|---|---|
| entity attack, swing at nothing, aborted dig | reset | reset |
| main hand changes to a different item | reset | reset |
| piercing weapon's stab | reset | kept |
| count/component change of the same item | kept | kept |

`Sim::reset_attack_strength_ticker` / `reset_only_attack_strength_ticker` are the only writers. With a sword (1.6, delay 12.5) an attack from rest gives heights `0.6, 0.2, 0.032768, 0.064, ...` and `1.0` on the twelfth tick; unarmed (4.0, delay 5) `0.6, 0.216, 0.512, 0.912, 1.0`. The off hand ignores cooldown; the swing arc is separate (`HandSwingSource`).

**Item used.** A successful use snaps the used hand's height and previous height to `0` (immediate, not eased), then it rises `0.4, 0.8, 1.0`. Callers are in `sim/actions.rs`: a predicted placement, and a generic use that starts a held use (eat, drink, draw, block; the gate that arms `UsingItem`), equips armour or swings (a thrown item). An item with no use of its own (a sword) leaves the hand up. Holding use through a finished bite snaps again.

**Hands busy.** Controlling a boat with a movement key held (`ControlledVehicle` is a boat and `MovementIntent` has forward or strafe, the bits the paddle input uses) lowers both hands by `0.4` a tick to `0` and makes `begin_attack_live` and `use_item_live` return early; releasing raises them. Passengers and land mounts are never busy.

Gaps: a block interaction consuming the held stack without placing (bone meal, bucket) does not snap (the later count change dips it as a swap); the off hand never snaps; an off-hand filled map draws nothing and a main-hand map always takes the two-handed pose; per-item swap-animation scale and opt-out are not read (scale `1`).

### Item-use arm poses

A humanoid's pose comes from two independent bits on different bytes: a player's from the living-entity using-item bit, a mob's (a skeleton's ranged attack) from the mob-flags aggressive bit (a skeleton's AI never sets using-item, so keying everything off it poses a player and no mob). The bow-while-aggressive override is keyed per renderer (skeleton family), not per model: an aggressive zombie with a bow does not get it, and a zombie's forward-arms animation overwrites any item pose, which is correct.

Both bits sit at metadata indices that are ambiguous on the wire (an arrow's crit flag shares the using-item index; an armour stand's show-arms flag and a display entity's billboard mode share the aggressive index), so surfacing either needs the concrete class first via an `is_living`/`is_mob` census column generated from a jar dump, never hand-counted: pick the narrowest column that separates true claimants.

A raised-arm pose merely for being held applies only to player/avatar-family renderers; an ordinary mob holding the item keeps its arms down (backwards raises the arm of every armed mob and decorative armour stand). The use-progress fraction is not on the wire, so the client counts ticks, seeded and reset only on a genuine rising edge of the flag (resetting on every repeated metadata byte, which servers resend routinely, leaves a bow permanently undrawn).

### Arm swing

One scalar, `attack_anim` (0.0..=1.0), drives the first-person arm, the local third-person body, and (via a wire-driven per-entity clock) other entities' swings. It is a sawtooth that drops to 0 in one tick, so partial-tick interpolation needs a forward-wrapped delta (a plain lerp runs the arm backwards through the arc for a frame at every restart, most of hold-to-mine). Drive it per tick, never per frame.

Left-click always swings, including a miss. Right-click swings only when the locally computed interaction result says so (most uses do not: raised shield, drawn bow, eating), approximated by per-item tables; right-clicking an entity always swings (a deliberate over-approximation).

Legacy remote animation events keep `AttackSwing` (main-hand filter, six-tick clock). An explicit `EntitySwingAnimation` carries hand, `ItemAnimationKind` and signed duration into `ExplicitAttackSwing`. Starting one snapshots the effect-adjusted duration (Haste or Conduit Power subtracts `1 + amplifier`; otherwise Mining Fatigue adds `2 * (1 + amplifier)`). A restart is accepted only after half the current duration or before its first tick. Changed descriptions reset both samples; identical ones preserve them and wrap forward during interpolation. Non-positive durations never divide, and the description clears at its boundary.

Extraction carries kind and physical arm in `AnimInput` with the duration-derived fraction. `Whack` uses the selected arm's melee arc, `Stab` separate preparation, thrust and return curves, `None` the shared torso orbit without a hand arc. Body draw and `Skeleton::translate_to_hand` consume the same posed skeleton so held items follow the arm. Off-hand is opposite the main arm; mob left-handed metadata is supported, remote player main-arm metadata is not (right-handed default). Species overrides (undead arm pose) keep precedence; held spear rest and use poses are separate work.

### Held block-entity items

Chests, shulker boxes, skulls, banners, decorated pots and tridents have no item or block model in the reference; all triangles come from a block-entity-style renderer. Drawing one in hand, dropped, in an entity's slot or in a frame means resolving a rig plus a standalone (non-atlas) sheet. This needs: an item-model resolver that can answer "no baked geometry, needs a special rig" from every surface drawing item geometry, a shared `(kind, item path)` to `(rig, sheet)` lookup, and a shared placement function turning a surface pose matrix into a posed instance. There is no flat-sprite fallback (the base models carry only a `display` transform map).

The hand draws these through the block-entity render pass, not the held-item model pass: their sheets are standalone and the model pipeline has no spare bind-group slot, while the block-entity pipeline has room. The pose is the ordinary swing/dip chain with the rig's rest-pose part transforms and no per-item override (a held chest's lid never opens).

## How to change it

- A use-pose or swing/equip variant: add the enum arm, the pose/animation branch and the selection rule deciding which entities get it, all three, or it compiles and tests green while reaching zero mobs (a gate downstream of selection cannot see a wrong selection).
- A special-rig `kind`: check the corpus for an existing rig and sheet first (several were resolver gaps, not missing geometry; a second copy of a working rig is worse than none).
- Per-tick interpolated scalars (swing, walk distance, hurt time) each need their own named read rule from the reference expression; some use a wrap, extrapolation or bare subtraction, and one generic interpolator has reintroduced the bug it was meant to prevent.
- First-person hand state (`sim/first_person_hands.rs`): unit tests hold hand-derived sequences, `sim/tests/first-person-hands.rs` follows production `Sim` paths (attack, stab, swap, eat, paddle, held uses) to the drawn pose, and `tests/gpu/first_person_hands_pixels.rs` (`--ignored`, GPU) gates pixels (off-hand mirror and swap, the cooldown dip against a projection-derived line, main-hand use poses unaffected). New attack-counter resets go through the two `Sim::reset_*` functions, never a direct write.
- A metadata bit selecting a pose is almost always index-ambiguous: check the jar's per-index claimants.

## Configuration

No runtime flags. Swing amplitudes, equip-dip timing and use-pose timing are fixed in `lodestone-render` and `lodestone-shell`. Remote swing duration comes from the packet and the entity's effects at animation start.

## Dependencies

`lodestone-render` (pose chains, `Skeleton::pose_arms_for_item`, special-item rig lookup in `entity.rs`, `entity_anim.rs`, `block_entity.rs`); `lodestone-entity`/`lodestone-ecs` (`pose::EntityPose`, `AttackSwing`, `ExplicitAttackSwing`, `ItemUse`, `MobState`); `lodestone-data` (`is_living`, `is_mob` columns); version adapters (metadata and animation decoding; the 26.3 adapter supplies explicit swing kind, hand and duration); `lodestone-shell` (`sim/first_person_hands.rs`, `gpu/first_person.rs`, `entities.rs`, `sim.rs`/`interact.rs`).
