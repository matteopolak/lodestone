# Item-use arm poses (bow and crossbow)

## What it is

The chain that turns `LivingEntity`'s synced **using-item** bit into a visibly drawn
bow or a winding crossbow on a mob or a remote player. Closes the mob half of
[issue #57](https://github.com/matteopolak/lodestone/issues/57); the local player's
own first-person view is **not** covered — see [Not done](#not-done).

Before this, nothing in the tree decoded the bit at all. The only trace of the
mechanism was a doc comment in `lodestone-render/src/entity.rs` describing what
vanilla does with it.

## How it works

The whole chain, producer to pixels:

| stage | where |
|---|---|
| `set_entity_data` index 8, a `BYTE` | `protocol/v770/src/packets/metadata.rs` (`IDX_LIVING_FLAGS`) |
| gated on the entity being a `LivingEntity` | `TrackedEntity` in the same file, populated at `add_entity` in `adapter.rs` |
| version-free `EntityMetadataUpdate::living_flags` | `lodestone-model/src/event.rs` |
| bit meanings | `LivingEntityFlags` in `lodestone-entity/src/metadata.rs` |
| fold into a component + local tick counter | `ingest::apply_entity_item_use`, `ingest::tick_entity_item_use` |
| the component | `ItemUse` in `lodestone-ecs/src/entity.rs` |
| pick a pose from the held item | `entities::arm_pose_for` in `lodestone-shell` |
| carry it to the renderer | `AnimInput::arm_pose` / `arm_pose_left_hand` |
| apply it to the arms | `Skeleton::pose_arms_for_item` in `lodestone-render/src/entity_anim.rs` |
| draw | the ordinary entity pass — `EntityInstance::new` already composes posed parts, and `hand_transform` follows, so the **held bow model follows the posed arm for free** |

`ingest::handles_event`'s routing switch needed **no new arm**: living flags ride
`EntityMetadataUpdated`, which the switch already claimed. That is asserted anyway
by `the_metadata_event_carrying_living_flags_is_claimed_by_this_module`, because
that switch is this repo's island factory and a later narrowing of it would delete
the pose silently.

### Metadata index 8 is ambiguous, and the wire cannot resolve it

This is the load-bearing subtlety. `LivingEntity.DATA_LIVING_ENTITY_FLAGS`
(`LivingEntity.java:179`) is `LivingEntity`'s first `defineId`, so
`SynchedEntityData`'s declaration-order counter puts it at index **8**.
`AbstractArrow.ID_FLAGS` (`AbstractArrow.java:66`) is *also* index 8 — `Projectile`
declares no synched data of its own — and **both are
`EntityDataSerializers.BYTE`**. Unlike an item stack, which self-identifies by
serializer, nothing on the wire distinguishes them.

An arrow's bit `0x01` is its **crit** flag. So a decoder that surfaced every
index-8 byte would report every critical arrow in flight as drawing a bow. The
byte is therefore only surfaced when the adapter can establish the entity is a
`LivingEntity`, resolved from the concrete type at `add_entity`.

`entity_census::is_living` supplies that. Its table is generated from the *same*
authoritative headless-26.2-server dump the push census uses — the dump already
carried a `living` column that the generator was reducing away — so it needed no
new JVM run. **It is not derivable from `ENTITY_PUSHES_PLAYERS`:** `armor_stand`,
`bat` and `parrot` are `LivingEntity` subclasses that do not push, so reading the
push table as an is-living test misclassifies exactly the entities whose arm poses
matter.

### The draw fraction is not on the wire, so we keep our own counter

The flags byte is a **boolean** plus a hand. `useItemRemaining` is *never* synced.
Vanilla's own client seeds a countdown when the bit flips on
(`LivingEntity.java:3521-3529`, `isClientSide()` only) and decrements it locally.

`ItemUse` does the same but counts **up**, because counting up *is*
`getTicksUsingItem()` (`LivingEntity.java:3594` — `duration - remaining`), which is
the quantity every pose and every draw-power formula actually reads. Counting up
also removes the need for `getUseDuration` entirely: a bow's is `72000` ticks, a
number no pose uses, and a crossbow's depends on Quick Charge.

**A repeated metadata byte is not a rising edge.** Servers re-send metadata freely
— on re-track, on any other field in the same packet changing, on entering range.
`ItemUse::apply_flags` resets the counter only on `!was_using && using` (or the hand
changing), mirroring `startUsingItem`'s `!this.isUsingItem()` guard. Resetting per
packet pins every bow permanently un-drawn while looking, at the wire level,
perfectly correct.

### The pose itself

`Skeleton::pose_arms_for_item` ports `HumanoidModel.poseRightArm`/`poseLeftArm`'s
`BOW_AND_ARROW` and `CROSSBOW_*` cases plus `AnimationUtils.animateCrossbowCharge`/
`animateCrossbowHold`. Three things about it are easy to get wrong:

1. **It assigns; it must not use this module's `set_*_rot` helpers**, which do `+=`
   despite their names. Summing leaves the walk swing inside the hold and makes the
   bow wobble with the legs.
2. **Ordering is vanilla's exactly** — after the walk swing, before
   `setupAttackAnimation` (`HumanoidModel.java:248-273`). The pose must overwrite
   the former and be overwritten by the latter.
3. **It reads the *posed* head**, not `head_yaw_deg`/`head_pitch_deg`. Head rotation
   is *added* to the model's authored pose, so for a rig authoring a non-zero head
   the two differ and vanilla reads the part.

## How to change it, and the gotchas

- **Adding a pose** (`ITEM`, `BLOCK`, `SPYGLASS`, …): add an `ArmPose` variant, a
  branch in `pose_arms_for_item`, and a selection rule in `entities::arm_pose_for`.
  Check `ArmPose::is_two_handed` — every pose modelled today is two-handed, which is
  not a coincidence but also not a rule.
- **A zombie rig deliberately loses the pose.** `animate_zombie_arms` assigns over
  both arms afterwards, so a bow-holding zombie keeps the arms-forward zombie pose —
  which is vanilla's behaviour too (`AbstractZombieModel.setupAnim` calls
  `super.setupAnim` then `animateZombieArms` unconditionally). This looks exactly
  like the wiring having failed. It is asserted by
  `a_zombie_rig_overwrites_the_item_pose_as_vanilla_does`, with a skeleton control
  in the same test, and it is the specificity control in the pixel gate.
- **Do not fold the bow's two branches into one signed expression.** The first
  attempt did, put the 0.4 rad splay on the wrong arm in the off-hand case, and
  still produced a plausible-looking bow draw. It is written longhand with the
  mirror case asserted independently.
- **`arm_pose_left_hand` mirrors rather than breaks.** A wrong value still looks
  like a bow draw, so it is a named field rather than an assumption.

## Not done

- **The local player in first person.** It has no `EntityKind`/`Position`/
  `Rotation`/`HeadYaw` — deliberately; that absence is what keeps a self-model off
  `ClientHandle::entities()` and out of the camera's own eye — so
  `lodestone_client::state::entity_view()`'s early `?` returns before the flags are
  read, and no amount of correct generic folding surfaces it. It needs a
  session-scoped fold of the same shape as `ingest::apply_local_player_on_fire`
  (`7822a60`, the worked precedent for `Vitals::on_fire`), a `PlayerSnapshot` field,
  and `ItemInHandRenderer`'s own bow transform in
  [`first-person-held-item.md`](./first-person-held-item.md). A remote player
  rendered in third person **does** get the pose; so does the local player's own
  body in third person once `Sim::third_person_body_state` is fed (it currently
  passes `ArmPose::Empty` with that gap noted at the site).
- **`ArmPose::CrossbowHold` is implemented and tested but unreachable.** It is not
  an in-use pose: vanilla selects it from `CrossbowItem.isCharged`, i.e. the stack's
  `minecraft:charged_projectiles` component, which `ItemComponents` does not model
  (an unrecognised component sets `has_unmodeled` and halts the patch decode). A
  charged crossbow is indistinguishable here from an empty one. Guessing "charged"
  would make every crossbow in the world hold the shooting pose permanently — more
  wrong, more often, than the resting pose it gets today. **Reaching it needs a
  `charged_projectiles` decode arm, not render work.**
- **Quick Charge is not modelled**, so `arm_pose_for` supplies `ticks / 25`
  (`CROSSBOW_CHARGE_TICKS`) — exact for an unenchanted crossbow, visually slow for an
  enchanted one. `CrossbowItem.getChargeDuration` is `25 - 5 * level`; reading the
  level needs full stacks where `RenderEquipment` has narrowed to bare item ids.
- `ITEM`, `BLOCK`, `SPYGLASS`, `TOOT_HORN`, `BRUSH`, `THROW_TRIDENT`, `SPEAR` are all
  `Empty`. `ITEM` needs only "is something held", which equipment already says, so
  it is a cheap separate follow-up.

## Proof

`crates/lodestone-render/tests/bow_draw_pose_pixels.rs`, `#[ignore]`d:

```
cargo test -p lodestone-render --test bow_draw_pose_pixels -- --ignored --nocapture
```

It measures **locations**, never a differing-pixel fraction — every failure prints a
bounding box. Reference run (broadside skeleton, 256x256):

```
rest silhouette : rows 59..=192 cols 109..=146 (38x134, 2662 px)
bow  silhouette : rows 59..=192 cols 109..=169 (61x134, 3054 px)
changed by pose : rows 93..=144 cols 119..=169 (51x52, 919 px)
determinism ctl : identical (correct)
zombie control  : identical (correct)
```

Assertions: the change is non-empty, lies inside the mob's own rect, *begins* in the
upper half (shoulders), does not reach past the arms' reach, widens the broadside
profile by >= 6 px, and does not move the soles.

Controls, all run: rest-vs-rest is byte-identical; the **zombie is byte-identical**
(the specificity control — a gate firing on the zombie too would be measuring "some
`AnimInput` field changed"); and the crossbow's `progress` moves pixels between 0.0,
0.5 and 1.0, which catches the `pose_swelling`-ignored-its-argument defect in
crossbow form.

**The negative control was run and watched to fail.** With `pose_arms_for_item`
early-returning, both gates fail reporting `changed by pose : NOTHING`. Separately,
deleting the `if living` guard in the decoder turns
`index_8_on_a_non_living_entity_is_consumed_but_not_surfaced` red with
`left: Some(1), right: None`.

### A gate assertion whose own premise was false

The first form of the arm-height assertion was "nothing below the waist differs",
and it **failed on a working pose** — the changed box reached row 144 against a waist
of 126. The premise was false, and false before the feature existed: an arm is 12
texels long and hangs *downward* from the shoulder, so rotating it forward vacates
every row its resting form occupied, a full arm's length below the shoulder. On this
framing the resting hand sits near row 148, exactly where the measured change stops.
`CLAUDE.md`'s rule about a control's premise failing in the safe-looking direction
applies to a gate's own assertions too. Both replacement bounds are derived from the
measured silhouette rather than restated constants.

## Configuration

None. No env vars, no features. `CROSSBOW_CHARGE_TICKS = 25.0` in
`lodestone-shell/src/entities.rs` is the only tunable constant, and it is vanilla's
no-Quick-Charge duration rather than a preference.

## Dependencies

- `lodestone-data`'s `entity_census::is_living` — the ambiguity guard.
- `lodestone-entity`'s `LivingEntityFlags` / `UsedHand` — bit meanings.
- `lodestone-model`'s `EntityMetadataUpdate::living_flags` — the version-free seam.
- `lodestone-ecs`'s `ItemUse` and its two systems.
- `lodestone-render`'s `ArmPose`, `AnimInput`, `Skeleton::pose_arms_for_item`.
- `lodestone-shell`'s `entities::arm_pose_for` and `extract_entity_draws`.
- Reference only, never transliterated: `.cache/mc/26.2/{src,client-src}`'s
  `LivingEntity`, `AbstractArrow`, `HumanoidModel`, `AnimationUtils`, `AvatarRenderer`,
  `AbstractSkeletonRenderer`, `AbstractZombieModel`, `CrossbowItem`, `BowItem`.
