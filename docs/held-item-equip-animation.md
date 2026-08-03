# Held-item equip animation

## What it is

The dip-and-raise the first-person hand makes when the held item changes — vanilla's
`ItemInHandRenderer` swap animation. Issue #366, reported from live play as
"switching the held item produces no animation, the new item simply appears".

Companion docs: [First-person held item](./first-person-held-item.md) for the pass
this animates, [Arm swing animation](./arm-swing-animation.md) for the **separate**
mechanism it must not be conflated with (vanilla tracks the two independently and
they can overlap), and [Item use arm poses](./item-use-arm-poses.md) for the
third-person poses that share nothing with either.

## What vanilla actually does

`ItemInHandRenderer.tick()`, 26.2, main hand only:

```java
this.oMainHandHeight = this.mainHandHeight;
ItemStack nextMainHand = player.getMainHandItem();
if (this.shouldInstantlyReplaceVisibleItem(this.mainHandItem, nextMainHand)) {
   this.mainHandItem = nextMainHand;
}
…
float attackAnim = player.getItemSwapScale(1.0F);
float mainHandTargetHeight = this.mainHandItem != nextMainHand ? 0.0F : attackAnim * attackAnim * attackAnim;
this.mainHandHeight = this.mainHandHeight + Mth.clamp(mainHandTargetHeight - this.mainHandHeight, -0.4F, 0.4F);
…
if (this.mainHandHeight < 0.1F) {
   this.mainHandItem = nextMainHand;
}
```

and in `submitHandsWithItems`:

```java
float mainhandInverseArmHeight = this.itemModelResolver.swapAnimationScale(this.mainHandItem)
   * (1.0F - Mth.lerp(frameInterp, this.oMainHandHeight, this.mainHandHeight));
```

which reaches the pose as a single translation term:

```java
// applyItemArmTransform  — the item branch
poseStack.translate(invert * 0.56F, -0.52F + inverseArmHeight * -0.6F, -0.72F);
// renderPlayerArm        — the bare-arm branch, same coefficient
poseStack.translate(…, ySwingPosition + -0.6F + inverseArmHeight * -0.6F, …);
```

### The real constants

| quantity | value | source |
|---|---|---|
| per-tick ramp | **±0.4 per tick** (8.0/s) | `Mth.clamp(target - height, -0.4F, 0.4F)` |
| full raise `0 → 1` | 2.5 ticks = **125 ms** | `1 / 0.4` |
| full swap (down, exchange, up) | 6 ticks = **300 ms** | heights `0.6, 0.2, 0.0` then `0.4, 0.8, 1.0` |
| visible-item exchange | when `height < 0.1` | `ItemInHandRenderer.tick()`'s tail |
| dip coefficient on `y` | **−0.6** for both the item and the arm | `ITEM_HEIGHT_SCALE`, `ARM_HEIGHT_SCALE` |
| `swapAnimationScale` default | **1.0** | `ItemModelResolver.swapAnimationScale` returns `1.0F` with no `minecraft:item_model` component |
| off-hand | **independent** `offHandHeight`/`oOffHandHeight`, target a plain `1.0` | same `tick()` |

Note the fields are **`mainHandItem` / `mainHandHeight` / `oMainHandHeight`**. Issue
#366 (and every pre-1.17-era reference) calls the pair
`equippedProgress` / `oldEquippedProgress`; there is no field by either name in this
jar, so grepping for them finds nothing and reads as "the mechanism is absent".

### What resets it — the predicate is the subtle part

`shouldInstantlyReplaceVisibleItem(visible, expected)` is
`ItemStack.matchesIgnoringComponents(visible, expected, ignoreSwapAnimation)` **or**
`!shouldPlaySwapAnimation(expected)`. When it is true the visible item is adopted
with no animation. So the animation fires when the *value* of the stack changed, and
`matchesIgnoringComponents` compares **count first** (`ItemStack.java:651`) and then
the component map — not only the item. Vanilla therefore re-triggers on:

- a different item (the hotbar scroll — the obvious case);
- the **same** item at a different count (eating one bread out of a stack);
- the same item with changed components (a tool taking durability damage).

And deliberately does *not* re-trigger when the stack is value-equal but a different
`ItemStack` **object** — which is what any inventory resync produces. That is why the
value check runs before the `mainHandItem != nextMainHand` reference compare: the
reference test is only reached once the value test has declined.

`shouldPlaySwapAnimation` is the per-item-model opt-out (`handAnimationOnSwap` in the
item definition), and `getItemSwapScale(1.0F)³` is a *second* animation sharing the
same field — the attack-cooldown dip, driven by `itemSwapTicker`.

## How it works here

```text
app.rs, once per in-world frame
  → RenderState::set_main_hand_source(|| selected_hotbar_item)
      → HeldItemEquip::advance(selected)        -- steps the 20 Hz swap clock
  → RenderState::prepare_first_person_hand
      HeldItemEquip::visible()             → decides the arm/item fork
      HeldItemEquip::inverse_arm_height()   → the dip, for BOTH branches
      → first_person_item_mesh(…, inverse_arm_height, …)
      → first_person_arm_pose_with_equip(…, inverse_arm_height)
```

`HeldItemEquip` lives in `crates/lodestone-shell/src/gpu/first_person.rs`.

### Three decisions worth the words

**The state lives in the renderer, because that is where vanilla puts it.**
`mainHandItem`/`mainHandHeight` are `ItemInHandRenderer` fields, not `LocalPlayer`
ones: the player owns the *selected slot*, and the renderer owns the lag between
that and what is drawn. Putting it in `Sim` instead would have needed a second
per-frame source *and* a new `app.rs` install to read it — and a source nobody
installs draws nothing, which is `CLAUDE.md` §1's island.

**It is stepped inside `set_main_hand_source`, a setter with a side effect.** That
call is the only per-frame `&mut self` hop on this path (`RenderState::render` takes
`&self`), and it already carries exactly the observation the state machine needs:
this frame's selected item. The source is stored first and read back through
`MainHandSource::value()`, so the equip state sees the identical value
`prepare_first_person_hand` would — one spelling of "the selected item", not two.

**The fork is on the *visible* item, not the selected one.** `submitArmWithItem`
branches on `this.mainHandItem`. Branching on the selection instead is the natural
mistake — `main_hand` is right there and reads like the answer — and it produces a
recognisably wrong animation: you watch the **new** item drop out of frame and come
back, instead of the old one leaving. It also breaks the item↔arm transition:
selecting an empty slot must lower the *item* and then raise a bare arm.

### Timing: whole ticks off the wall clock

`advance` accumulates real elapsed time and consumes **whole** 20 Hz steps, never a
fraction of the `0.4`, so the animation takes the same wall time at 30 fps as at
240 — the frame-rate dependence `Sim::step`'s note on `chest_lids.tick()` records,
avoided the same way. The leftover accumulator is the partial tick for the
`lerp(frameInterp, oHeight, height)`.

A useful property falls out of that lerp: because each step is clamped to `±0.4`,
`lerp(p, previous, height)` is `previous ± 0.4p` — **a straight line of slope 0.4 per
tick with no discontinuity at a tick boundary.** So predicting the drawn value is
arithmetic, not a simulation, which is what makes the gates below assertions on a
number rather than on a sign.

A catch-up cap of 20 steps bounds the loop across a tab-out or a menu; 20 is past
the 6 ticks a full swap needs, so a long gap lands on the *finished* state rather
than somewhere arbitrary in the middle.

## How to change it, and the gotchas

- **`HeldItemEquip` must not `#[derive(Default)]`.** A derived default zeroes
  `height`, and `inverse_arm_height` is `1 - height`, so a `RenderState` on which
  nobody ever installs a main-hand source would draw its bare arm **permanently
  0.6 blocks lower** — mostly off the bottom of frame. That is every headless and
  GPU test that renders a hand without opting into a held item, and it looks exactly
  like "the first-person arm stopped rendering". `Default` is hand-written to the
  rested state, and `an_uninstalled_equip_state_is_fully_equipped` pins it.
- **The first observation seeds at rest rather than animating up from zero.** Vanilla
  starts at `height = 0` and raises the item into view on the first tick in a world.
  Reproducing that would make any single-frame caller — every `#[ignore]`d GPU gate,
  and the first frame after a pass rebuild — render a permanently dipped hand. The
  join flourish is traded for that; the trade is in `HeldItemEquip::last`'s doc.
- **The retrigger predicate is narrowed to the item id.** The shell's main-hand
  source is a bare `ResourceLocation` (built in `app.rs` from `HotbarSlot::item`),
  so count and component changes are invisible here: a tool taking damage or a stack
  running down does not dip. That is the conservative direction — over-triggering
  would dip the hand on every durability tick while mining. Widening it means
  widening `MainHandSource` to carry a stack, which is an `app.rs` change.
- **The attack-cooldown dip is absent.** Vanilla's rest target is
  `getItemSwapScale(1.0F)³`, so the hand also dips briefly after an attack and rises
  as the cooldown recovers. Neither `itemSwapTicker` nor
  `getCurrentItemAttackStrengthDelay()` is modelled, so the rest target is the
  steady-state `1.0`. Guessing a cooldown would dip the hand on a schedule unrelated
  to the player's real attack speed.
- **`isHandsBusy()` is absent.** Vanilla lowers *both* hands at 0.4/tick with no rise
  while the hands are busy (using an item, riding). Needs local-player use state on
  this path.
- **`itemUsed(hand)` is absent** — vanilla slams the height to `0` when an item is
  used, so eating visibly re-raises the hand.
- **The off hand is not animated because it is not drawn.** `prepare_first_person_hand`
  only ever asks for `Arm::Right`; see
  [first-person-held-item.md](./first-person-held-item.md). Adding it needs a second
  height *and* a second mesh.
- **`swapAnimationScale` is 1.0 for every item.** The per-item-model override lives
  in item-model definitions, which the item pipeline does not read.
- **`first_person_arm_chain` / `first_person_arm_pose` were kept as `0.0` wrappers**
  over the new `*_with_equip` forms, so no existing caller or gate changed behaviour
  or expected matrix. Prefer adding to the `_with_equip` pair.

## Configuration

None. `EQUIP_RATE_PER_TICK`, `EQUIP_SWAP_BELOW` and `EQUIP_REST_HEIGHT` are vanilla
constants in `crates/lodestone-shell/src/gpu/first_person.rs`;
`FIRST_PERSON_ITEM_EQUIP_DIP` and `FIRST_PERSON_ARM_EQUIP_DIP` are in
`crates/lodestone-render/src/entity.rs`. The two dip constants are numerically equal
(`-0.6`) and deliberately separate: they live in different vanilla methods over
different base offsets (`-0.52` for the item, `-0.6` for the arm), so the equality is
a coincidence of 26.2's numbers rather than a shared rule.

## Dependencies

- `lodestone-render` — `entity::{first_person_item_mesh, first_person_arm_pose_with_equip,
  first_person_arm_chain_with_equip, FIRST_PERSON_ITEM_EQUIP_DIP,
  FIRST_PERSON_ARM_EQUIP_DIP}`.
- `lodestone-ecs` — `TICK_PERIOD`, for the 20 Hz step.
- `lodestone-assets` — `ResourceLocation`, the item identity the predicate compares.
- No new `app.rs` wiring: `set_main_hand_source` was already installed every frame.

## Gates

In `crates/lodestone-shell/src/gpu/first_person.rs`'s test module. `advance` is split
into `advance` (reads `Instant::now()`) and `advance_by(dt, …)` purely so magnitude is
assertable at all — a state machine whose only input is the clock can be tested for
direction and never for value.

- `an_uninstalled_equip_state_is_fully_equipped` — the `Default` trap above.
- `the_first_observation_seeds_at_rest`.
- `the_swap_ramp_steps_by_exactly_the_vanilla_rate` — **magnitude on the ramp.** The
  height sequence from rest is `0.6, 0.2`; two wrong readings of the same `clamp`
  line are computed and excluded: `0.4` of the *remaining* distance per tick gives
  `0.6, 0.36` (same first value, and it never reaches the bottom, so the item would
  never be exchanged), and half the rate gives `0.8, 0.6`.
- `the_partial_tick_lerp_lands_on_the_predicted_value` — **magnitude on what reaches
  the pose matrix.** Measured a *quarter* tick past the first step, where the truth is
  `0.1`: a reversed lerp gives `0.3`, no lerp at all gives `0.4`, and an unsubtracted
  `height` gives `0.9`. A *half* tick cannot separate the reversed lerp (both read
  `0.2`), which is why the quarter is deliberate.
- `the_item_is_exchanged_at_the_bottom_and_the_hand_rises_again` — the exchange lands
  on the third tick, and the full six-tick height sequence
  `0.6, 0.2, 0.0, 0.4, 0.8, 1.0` matches.
- `holding_the_same_item_never_dips` — 40 ticks of re-installing the same item, which
  is what `app.rs` does every frame; without
  `shouldInstantlyReplaceVisibleItem`'s value check the hand would dip continuously.
- `putting_an_item_away_lowers_it_before_the_arm_appears` — the arm/item fork follows
  the visible item.
- `a_long_frame_gap_completes_the_swap` — the catch-up cap does not truncate a swap a
  tab-out spanned.

**No new pixel gate.** The dip is one additive term in a translation the existing
`#[ignore]`d gates already render (`thrown_and_held_item_pixels.rs`,
`first_person_hand_light_pixels.rs`), and both of those hold at `inverse_arm_height ==
0.0` by construction — neither installs a hotbar change, and `Default` plus the
seed-at-rest rule keep them at exactly the value they measured before. Stated as a
deliberate scope decision, not as verification performed.
