# Arm poses (bow and crossbow)

## What it is

The chain that turns a synced metadata bit into a visibly drawn bow or a winding
crossbow. There are **two different bits, on two different bytes, and which one
applies depends on what kind of entity it is**:

| entity | bit | vanilla source |
|---|---|---|
| a player, or a remote player | `LivingEntity` **using-item**, index 8 `0x01` | `AvatarRenderer.getArmPose` |
| a **mob** | `Mob` **aggressive**, index 15 `0x04` | `AbstractSkeletonRenderer.getArmPose` |

[Issue #57](https://github.com/matteopolak/lodestone/issues/57) landed the first
row and the whole pose machinery.
[Issue #379](https://github.com/matteopolak/lodestone/issues/379) landed the
second, because the first **reaches zero mobs** — see
[Two mechanisms](#two-mechanisms-and-why-the-first-one-covers-no-mobs). The local
player's own first-person view is still not covered — see
[Not done](#not-done).

Before #57, nothing in the tree decoded either bit. The only trace of the
mechanism was a doc comment in `lodestone-render/src/entity.rs` describing what
vanilla does with it.

## Two mechanisms, and why the first one covers no mobs

#57 selected the pose from the using-item bit alone. That is exactly right for a
player: `startUsingItem` sets the bit, and `AvatarRenderer` reads it.

It is the wrong mechanism for a mob, and not by a little. A skeleton's ranged
attack goal calls `performRangedAttack` — it **never enters the item-use state** —
so `LivingEntity`'s using-item bit is `false` for the entire life of every
skeleton that has ever shot at anyone. Vanilla's mob renderers do not read it;
they read `Mob.isAggressive()` (`AbstractSkeletonRenderer.java:38`):

```java
return mob.getMainArm() == arm && mob.isAggressive() && mob.getMainHandItem().is(Items.BOW)
       ? HumanoidModel.ArmPose.BOW_AND_ARROW : super.getArmPose(mob, arm);
```

So after #57 the pose was implemented, unit-tested, proven to reach pixels by its
own GPU gate — and drawn on nothing. That gate sets `AnimInput::arm_pose`
*directly* and is structurally blind to it: it starts downstream of the decision
about which entities get the pose. **A gate that starts below the selection cannot
see a wrong selection**, which is the general lesson.

The override is per **renderer**, not per model, so it is keyed on the entity
type by `lodestone_render::mob_draws_bow_when_aggressive` — every
`AbstractSkeletonRenderer` subclass in 26.2: `skeleton`, `wither_skeleton`,
`stray`, `bogged`, `parched`. An aggressive *zombie* holding a bow gets no such
pose in vanilla, and a pillager's arms come from a different enum on a different
model class (see [Not done](#not-done)).

### The second island the same flag was hiding

`AnimInput::aggressive` already existed and `Skeleton::animate_zombie_arms`
already consumed it — `AnimationUtils.animateZombieArms`' arm drop is `-PI/1.5`
when aggressive and `-PI/2.25` when not. **Every call site in the shell passed a
hardcoded `false`**, with a comment saying the bit was undecoded. So an aggressive
zombie's raised arms were dead code too, and #379 closed both with one decode.
`zombified_piglin` was also missing from `humanoid_arms_for`'s zombie family
(`ZombifiedPiglinModel:14` calls `animateZombieArms`), so it was getting a plain
player arm swing; that is fixed in the same change.

## How it works

The whole chain, producer to pixels:

| stage | player path (index 8) | mob path (index 15) |
|---|---|---|
| `set_entity_data` byte | `IDX_LIVING_FLAGS` | `IDX_MOB_FLAGS` — both in `protocol/v770/src/packets/metadata.rs` |
| the ambiguity guard | `TrackedEntity::living` | `TrackedEntity::mob` — same file, both populated at `add_entity` in `adapter.rs` |
| version-free field | `EntityMetadataUpdate::living_flags` | `::mob_flags` — `lodestone-model/src/event.rs` |
| bit meanings | `LivingEntityFlags` | `MobFlags` — both `lodestone-entity/src/metadata.rs` |
| fold to a component | `ingest::apply_entity_item_use` (+ `tick_entity_item_use`) | `ingest::apply_entity_metadata` |
| the component | `ItemUse` | `MobState` — both `lodestone-ecs/src/entity.rs` |
| which types the rule applies to | every living entity | `lodestone_render::mob_draws_bow_when_aggressive` |
| pick a pose | `entities::arm_pose_for` in `lodestone-shell` — the aggressive branch first, then the using-item one, matching vanilla's `? :` over `super` | |
| carry it to the renderer | `AnimInput::arm_pose` / `arm_pose_left_hand`, plus `AnimInput::aggressive` | |
| apply it to the arms | `Skeleton::pose_arms_for_item`, and `animate_zombie_arms` for the zombie family — `lodestone-render/src/entity_anim.rs` | |
| draw | the ordinary entity pass — `EntityInstance::new` already composes posed parts, and `hand_transform` follows, so the **held bow model follows the posed arm for free** | |

Both bytes ride the same `ClientEvent::EntityMetadataUpdated`, so
`ingest::handles_event` needed **no new arm** for either. That is asserted anyway,
once per byte (`the_metadata_event_carrying_living_flags_is_claimed_by_this_module`
and `..._mob_flags_...`), because that switch is this repo's island factory and
"no change required" is exactly the state in which a later narrowing deletes a
feature silently. It was checked *before* the fold was written, not after.

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

### Index 15 is ambiguous too, and `is_living` is **not** enough for it

The mob-flags byte has the same problem one notch tighter, and the interesting part
is that the obvious guard does not work. The jar dump reports three claimants on
index 15, all `EntityDataSerializers.BYTE`:

| owner | field | `0x04` means |
|---|---|---|
| `Mob` | `DATA_MOB_FLAGS_ID` | **aggressive** |
| `ArmorStand` | `DATA_CLIENT_FLAGS` | **show arms** (`CLIENT_FLAG_SHOW_ARMS`) |
| `Display` | `DATA_BILLBOARD_RENDER_CONSTRAINTS_ID` | an enum ordinal |

**`ArmorStand` is a `LivingEntity`.** So unlike index 8, where the collision was
living-vs-non-living and `is_living` resolved it, this one is living-vs-living: an
armour stand with arms shown — the ordinary decorative case — would report itself
as an aggressive mob and, holding a bow, draw it. The guard is therefore
`entity_census::is_mob`, a **third** census column, strictly narrower than
`is_living` and generated from a `mob` column added to the same JVM dump. The
three living non-mobs in 26.2 are `armor_stand`, `mannequin` and `player`,
asserted by name rather than by count, because the identity of the gap is the
finding and a later version adding one must be looked at rather than absorbed.

### The index came from the jar, not from a count

Both indices were originally *hand counted* over `SynchedEntityData.defineId`'s
per-hierarchy declaration-order counter — "8 fields on `Entity`, 7 on
`LivingEntity`, so `Mob`'s only one is 15". Both counts are right, and looking for
a way to *check* them turned up **two others that were wrong**:
`Sheep.DATA_WOOL_ID` and `Horse.DATA_ID_TYPE_VARIANT` were each off by one because
nobody counted `AgeableMob.AGE_LOCKED` (index 17). See
`crates/protocol/v770/oracle-java/EntityDataIndexOracle.java` and
`tests/support/entity_data_index_jvm.txt`: a headless-server dump of every
`EntityDataAccessor` in the game, sorted by index so **collisions are adjacent
lines**, with `every_metadata_index_constant_matches_the_jar_dump` asserting all
ten constants against it — index *and* serializer, since a right index with a
wrong serializer arm silently never matches, which is exactly how the sheep defect
behaved.

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

### Aggressive-driven poses deliberately left (#379)

- **A drowned's `THROW_TRIDENT`** (`DrownedRenderer.java:54`: aggressive + a
  trident). The pose body is two lines (`HumanoidModel.java:359`), and it is left
  anyway because it is vanilla's first **one-handed** pose —
  `ArmPose.THROW_TRIDENT(false, true)` — where every pose modelled here is
  two-handed. One-handed means `HumanoidModel.setupAnim`'s `affectsOffhandPose`
  fork actually branches, and `Skeleton::pose_arms_for_item` does not implement
  that fork; adding the pose without it would silently pose the *wrong arm* on an
  off-hand trident. That is the same defect class as folding the bow's two branches
  into one signed expression, which already happened once here and looked
  plausible. **It needs the one-handed dispatch first, not a new pose branch.**
- **Every illager pose.** `IllagerRenderer:27` does copy `isAggressive` into its
  render state, but an illager's arms are driven by
  `AbstractIllager.IllagerArmPose` — a *different enum*, on `IllagerModel`, a
  different model class — and the value is computed per subclass
  (`Vindicator.java:107` → `ATTACKING` when aggressive; `Pillager.java:135` the
  same behind two crossbow cases) rather than being a metadata bit at all.
  Reaching it needs an illager arm family in `lodestone-render/src/entity_anim.rs`.
- **`Mob.isLeftHanded`** (bit `0x02` of the same byte) is decoded by `MobFlags` and
  consumed by nothing. It flips `getMainArm()`, which flips which arm every pose
  applies to, so a left-handed skeleton draws with the wrong arm. Vanilla sets it
  for about 5% of mobs at spawn. Plumbing a main-arm through the pose chain is
  wider than the bit that would feed it, so it is recorded rather than guessed.
- **`Mob.isNoAi`** (bit `0x01`) is decoded and unused, and correctly so — it is not
  a render fact. It is modelled because the alternative is a bare `0x04` mask with
  no name for the bits either side of it. Note for anyone building a live fixture:
  `NoAI:1b` also **stops a skeleton drawing a bow**, so an aggressive-flag fixture
  has to set the flag directly rather than provoke real AI.

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

### The #379 gate: `crates/lodestone-shell/tests/aggressive_bow_pose_pixels.rs`

```
cargo test -p lodestone-shell --test aggressive_bow_pose_pixels -- --ignored --nocapture
```

The gate above is not enough, because it sets `AnimInput::arm_pose` **directly**.
This one starts at a `ClientEvent` in `IngestQueue` and ends at texels: through the
production `IngestPlugin` + `EntityInterpPlugin` pair, the real
`extract_entity_draws`, and `RenderState::render` — `app.rs`'s own frame call.
Reference run (broadside, 320x240):

```
subject : aggressive=true  arm_pose=BowAndArrow
control : aggressive=false arm_pose=Empty
zombie  : aggressive=true  arm_pose=Empty
rest silhouette : rows 65..=174 cols 146..=173 (28x110, 1544 px)
bow  silhouette : rows 65..=174 cols 146..=194 (49x110, 1692 px)
zombie rest sil : rows 65..=174 cols 146..=197 (52x110, 2481 px)
zombie angry sil: rows 65..=174 cols 146..=195 (50x110, 2381 px)
changed by pose : rows 91..=129 cols 153..=194 (42x39, 564 px)
calm control    : identical (correct)
zombie arm lift : rows 73..=112 cols 150..=197 (48x40, 829 px)
width gain      : skeleton +21 px, zombie -2 px
```

Four controls, each run and watched to fail:

| control | how it was broken | what it printed |
|---|---|---|
| the selection reaches pixels | `arm_pose_for`'s aggressive branch made unreachable | `arm_pose=Empty`, `left: Empty right: BowAndArrow` |
| the *pose* reaches pixels | `pose_arms_for_item` early-returns | `changed by pose : NOTHING` |
| specificity | `"zombie"` added to `mob_draws_bow_when_aggressive` | zombie `arm_pose=BowAndArrow`, specificity assert red |
| the decoder guard | `if mob` deleted from the index-15 arm | `left: Some(4), right: None`, two tests red |

Each file was restored by `cp` from a scratchpad backup with an md5 check, never by
`git checkout`.

#### Two of this gate's own premises were false, and both failed *safely*

Worth recording, because both fired an assertion **on correct rendering** — the
failure direction that looks like a bug in the feature:

1. **"Everything unlike the frame's corner pixel is the mob."** It reported the
   silhouette as `rows 65..=239 cols 146..=319` — the whole lower-right quadrant,
   clipped against two borders — and tripped `assert_unclipped`. The cause is
   `CLAUDE.md`'s *ask what else already paints here*: the **sky is a gradient**, so
   the corner is not the colour of the rest of the sky. Fixed by differencing
   against a real entity-free frame rendered through the identical path, which
   needs no assumption about the background at all.
2. **"The bow draw must change more pixels than the zombie's arm lift."** False:
   the measured run has the zombie at 829 px against the skeleton's 564. A zombie's
   arms already point forward, so lifting them sweeps a wide arc high up. **Area is
   not the discriminator**; the *directional* broadside **width gain** is
   (`+21 px` vs `−2 px`), which is what the assertion now reads. This is
   `CLAUDE.md`'s magnitude species caught in the gate rather than in production.

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

- `lodestone-data`'s `entity_census::is_living` and `::is_mob` — the two ambiguity
  guards, for index 8 and index 15 respectively.
- `lodestone-entity`'s `LivingEntityFlags` / `UsedHand` / `MobFlags` — bit meanings.
- `lodestone-model`'s `EntityMetadataUpdate::living_flags` and `::mob_flags` — the
  version-free seam.
- `lodestone-ecs`'s `ItemUse` and its two systems, plus `MobState` folded by
  `apply_entity_metadata`.
- `lodestone-render`'s `ArmPose`, `AnimInput`, `Skeleton::pose_arms_for_item`,
  `Skeleton::animate_zombie_arms` and `mob_draws_bow_when_aggressive`.
- `lodestone-shell`'s `entities::arm_pose_for` and `extract_entity_draws`.
- Reference only, never transliterated: `.cache/mc/26.2/{src,client-src}`'s
  `LivingEntity`, `AbstractArrow`, `HumanoidModel`, `AnimationUtils`, `AvatarRenderer`,
  `AbstractSkeletonRenderer`, `AbstractZombieModel`, `CrossbowItem`, `BowItem`.
