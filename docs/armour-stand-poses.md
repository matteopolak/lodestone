# Armour stand poses

## What it is

The chain that turns an armour stand's six synced part rotations into the pose it is drawn in —
and, just as importantly, the reason **every** armour stand is posed whether or not a server
ever sent one. Without it a stand animates as a walking humanoid: a stand carried along by a
moving contraption swings its arms like a running player, and an item in its hand swings off
the same arm.

## How it works

### Vanilla computes the walk cycle and then throws it away

`ArmorStandArmorModel.setupAnim` is two statements: `super.setupAnim(state)` — the entire
`HumanoidModel` pass, head tracking, walk cycle, crouch, item pose, attack swing and idle bob —
followed by an **unconditional assignment** of `head`, `body`, both arms and both legs from the
stand's six pose accessors. `ArmorStandModel.setupAnim` extends that with the three body sticks
(`right_body_stick`, `left_body_stick`, `shoulder_stick`), all driven from the **body** pose.

So the assignment is not a decoration layered on top of an animation. It *is* the animation:
everything the base pass wrote for those joints is discarded on every frame, for every armour
stand in the game.

Two consequences that decide the shape of everything below:

- **A stand that has never reported a pose still has one.** `ArmorStand`'s `defineEntityData`
  registers each accessor with a `DEFAULT_*_POSE` constant, and those are **not zero** — the arms
  and legs carry a small authored splay. Treating "no pose reported" as "do not overwrite" leaves
  the walk cycle standing, which is exactly the reported defect.
- **The assignment covers rotations only.** Vanilla never touches a part's translation, so the
  crouch's `y` offsets and the attack swing's arm orbit survive underneath it.

### The chain

| hop | symbol |
|---|---|
| wire | `ROTATIONS` (serializer 9), indices 16–21 |
| decode | `lodestone_v770::packets::metadata::decode_value`'s `SER_ROTATIONS` arm → `Value::Rotations` |
| version-free | `lodestone_model::ArmorStandPoseUpdate` on `EntityMetadataUpdate::armor_stand_pose` |
| fold | `lodestone_ecs::ingest::apply_entity_metadata` → `lodestone_ecs::entity::ArmorStandPose` |
| extract | `lodestone_shell::entities::extract_entity_draws` → `AnimInput::armor_stand_pose` |
| rig | `lodestone_render::entity_anim::Skeleton::pose_armor_stand` |
| held item | `Skeleton::translate_to_hand`, which re-derives the same posed parts |

### The decode needs no class guard, and adding one would be a regression

Index 16 alone has 29 claimants in the committed jar dump
(`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`), which is why
`IDX_CREEPER_SWELL_DIR` and `IDX_DRAGON_PHASE` each need a `MetadataClass`. The *serializer*
settles these six: grep that dump for `ROTATIONS` and it returns exactly six lines, all six
`ArmorStand`. So the `(index, Value::Rotations(_))` pair is unambiguous on the value shape alone
— the same property that lets `VECTOR3`/`QUATERNION` skip a guard — and the index is only being
asked *which part* moved.

A class guard here would not be belt-and-braces: it would silently drop the pose for any stand
whose spawn packet the adapter could not resolve a class from.

The decode also reproduces `Rotations`' own compact constructor per component — non-finite
becomes `0.0`, finite is reduced modulo 360. Vanilla applies it inside the record's constructor,
so every `Rotations` the client holds has been through it. The modulo is cosmetically inert; the
non-finite clamp is not, and this is the only place that can stop a `NaN` poisoning every matrix
composed from that part.

### The fold merges; every other arm in `apply_entity_metadata` replaces

A metadata packet mentions only the accessors that *changed*, so an update nudging one arm must
leave the other five parts alone — vanilla's per-accessor `SynchedEntityData` semantics. That is
why `EntityMetadataUpdate` carries six independently-optional parts (`ArmorStandPoseUpdate`)
rather than a whole `ArmorStandPose`: a consumer must be able to tell an *unreported* part from
one explicitly set back to its default.

The merge uses `EntityCommands::entry(…).or_default().and_modify(…)` rather than a
read-then-`insert`. `Commands` is deferred, so a `Query` read at fold time would see the
*pre-batch* pose and two updates to the same stand in one batch would each merge onto the same
stale base, silently losing the first. `and_modify` runs when the command is applied, in command
order.

### The extract step gates on the entity **type**, not on the component

`extract_entity_draws` sets `AnimInput::armor_stand_pose` for every entity whose type path is
`armor_stand`, falling back to `ArmorStandPose::VANILLA_DEFAULT` when no `ArmorStandPose`
component exists. Reading the component alone is the plausible wrong fix: it populates the field
for posed stands, passes a single-subject test, and leaves every unposed stand animating.

`None` on that field therefore means "**this is not an armour stand**", never "this stand has no
pose".

### Why the walk cycle is computed and discarded rather than skipped

An entity-type gate on `AnimFamily`, or a third `HumanoidArms` variant, would be cheaper and
would look equivalent. It is not what vanilla does, and the difference shows: the base pass
writes part *translations* as well as rotations, the assignment covers rotations only, so those
translations survive in vanilla and would vanish under a gate. `HumanoidArms::Zombie` is the one
place this crate does take the skip, and it can because the terms it drops provably leave no such
residue.

## How to change it

- **A seventh accessor, or a change to which parts the sticks follow.** Add the field to
  `ArmorStandPoseUpdate` and `ArmorStandPose` together, then the decode arm, then the assignment
  list in `Skeleton::pose_armor_stand`. `ArmorStandPoseUpdate::is_empty` is the one switch the
  fold consults, so it must learn the new part or the fold will ignore packets carrying only it.
- **Never write the six as an array or a positional tuple.** Six same-typed triples in a row is
  the shape a transposition survives every round trip; the only symptom is a stand whose left arm
  sits where its right leg should be. Every list of them in this chain is named-pairs for that
  reason, and the fixtures use pairwise-distinct values so a swap cannot pass.
- **`ArmorStandPose::default()` is `VANILLA_DEFAULT`, not zeroes.** Deliberate: every caller that
  reaches for a default wants "the pose an unposed stand is in". A derived `Default` would
  silently straighten every stand's arms and legs.
- **Adding a part to the `armor_stand` model** in `lodestone_assets::entity_models` does not
  automatically pose it — `Skeleton::slots` has to resolve it and `pose_armor_stand` has to name
  it.

### Gotchas

- **The base plate does not stay square.** `ArmorStandModel.setupAnim` sets
  `basePlate.yRot = -state.yRot`, cancelling the stand's body rotation so the plate keeps its
  world alignment. That needs the entity's **absolute** yaw, which `AnimInput` does not carry —
  it holds head yaw *relative to the body*, by contract, and the whole-entity yaw is applied
  downstream by `entity_model_matrix`. So the plate currently rotates with the stand. Stated as a
  gap rather than approximated from the head yaw, which is a different angle.
- **The rest-pose AABB is very slightly off.** `Skeleton::rest_pose` feeds the mesh's local
  bounding box, and the default pose splays the arms and legs by a few degrees after it. Unlike
  `HumanoidArms::Zombie`, whose resting arm angles *are* baked into the skeleton, this pose is
  per-entity and cannot be. The effect is a cull bound a fraction of a texel small.
- **`is_small` is not read here.** `resolve_entity_facts` already folds it into `EntityDraw::scale`
  as a uniform half-scale, approximating vanilla's separate small-model bake.

## Configuration

None. No feature flag, no constant to tune. The six `DEFAULT_*_POSE` values are transcribed from
`ArmorStand`'s own fields into `ArmorStandPose::VANILLA_DEFAULT`.

## Dependencies

- `lodestone-model` — `ArmorStandPose`, `ArmorStandPoseUpdate`, `Vec3f`; the version-free
  vocabulary both the ECS and the renderer read.
- `lodestone-v770` — the only family that decodes the six accessors. The legacy families
  (`v47`/`v340`/`v735`) do not, so a legacy session's stands still take the walk cycle.
- `lodestone-ecs` — the `ArmorStandPose` component and the merging fold.
- `lodestone-render` — `AnimInput`, `Skeleton`; `entity_anim`'s own tests carry the rig-level
  gates.
- `lodestone-shell` — `extract_entity_draws`; `tests/armor_stand_pose_wire.rs` carries the
  producer-level gates, which the rig's own gates are structurally blind to because they install
  their own `AnimInput`.
