# Riptide and the elytra firework boost

## What it is

Two item-driven velocity impulses in `lodestone-physics`: the riptide-trident
launch (#208) and the elytra firework-rocket glide boost (#206). Both are
landed as **physics-only, partial** — the arithmetic is implemented and
tested against the decompiled source; the *trigger* (item use, held-duration,
weather, rocket entity lifetime) is not, because none of it is physics state
this crate models. See "What is not modelled" in each section — this is not
an oversight, it is the scope boundary `lodestone-physics` draws everywhere
else in this codebase (equipment, entity spawning and item state all live
outside it).

## How it works

### Riptide (#208)

`TridentItem.releaseUsing` (`TridentItem.java:88-104`), reached when a
player releases a Riptide-enchanted trident after holding it at least 10
ticks while in water or rain:

```java
float xd = -sin(yRot * pi/180) * cos(xRot * pi/180);
float yd = -sin(xRot * pi/180);
float zd = cos(yRot * pi/180) * cos(xRot * pi/180);
float dist = sqrt(xd*xd + yd*yd + zd*zd);
player.push(xd * strength/dist, yd * strength/dist, zd * strength/dist);
player.startAutoSpinAttack(20, 8.0F, itemStack);
if (player.onGround()) player.move(SELF, (0, 1.1999999, 0));
```

`lodestone_physics::apply_riptide(state, view, profile, strength)`
reproduces exactly this: the impulse (via the crate's existing, bit-exact
`Mth` sine table), `PlayerState::auto_spin_attack_ticks = 20`, and — if
`on_ground` — a real collision-resolving pop-up via `move_entity` (so a
riptide fired under a low ceiling stops at the ceiling rather than clipping
through it).

`auto_spin_attack_ticks` decrements once per tick, unconditionally, from
inside `travel_and_check_inside_blocks` (matching
`LivingEntity.aiStep`'s unconditional `autoSpinAttackTicks--`), and
`PlayerState::is_auto_spin_attack()` (`> 0`) feeds a new `Pose::SpinAttack`
variant in `crate::pose`, inserted into `desired_pose`'s priority exactly
where vanilla's `getDesiredPose` puts it: after `SWIMMING`/`FALL_FLYING`,
before the crouch/stand pair.

**What is not modelled**: the three gates vanilla checks before reaching
this code at all (`EnchantmentHelper.getTridentSpinAttackStrength(...) >
0.0F` — equipment/enchantment data; `timeHeld >= 10` — item-use duration;
`isInWaterOrRain()` — fluid presence *or* weather, and this crate has no
weather concept) — a driver must evaluate all three and call
`apply_riptide` once, on the release edge, with `strength` already resolved.
Also not modelled: the attack-damage half of `startAutoSpinAttack`
(`autoSpinAttackDmg`, the entity-hit sweep in `checkAutoSpinAttack`) — this
crate applies no damage anywhere.

### Elytra firework boost (#206)

`FireworkRocketEntity.tick`'s attached-to-a-glider branch
(`FireworkRocketEntity.java:122-137`), applied every tick a firework rocket
entity is attached to a fall-flying player:

```java
Vec3 lookAngle = attachedToEntity.getLookAngle();
Vec3 movement = attachedToEntity.getDeltaMovement();
attachedToEntity.setDeltaMovement(movement.add(
    lookAngle.x * 0.1 + (lookAngle.x * 1.5 - movement.x) * 0.5,
    lookAngle.y * 0.1 + (lookAngle.y * 1.5 - movement.y) * 0.5,
    lookAngle.z * 0.1 + (lookAngle.z * 1.5 - movement.z) * 0.5
));
```

`lodestone_physics::apply_firework_boost(state)` reproduces this line
exactly, reusing the crate's existing (private) `calculate_view_vector` —
the same `Entity.getLookAngle()` port `update_fall_flying_movement` already
uses for the elytra glide itself, so there is one look-vector
implementation in the crate, not two.

**What is not modelled, and could not be without more than this crate
owns**: the rocket is its own entity, ticked independently by the level's
normal entity loop. Spawning it on right-click, tracking the "attached"
relationship, and its `life` counter (which decides how many ticks the
boost lasts before the rocket detonates) are entity/item state this crate
has no model of. A driver must spawn/track the rocket (or an equivalent
per-use counter) and call `apply_firework_boost` once per tick for as long
as vanilla's attached rocket would still be ticking, with
`PlayerState::fall_flying` already `true`. Vanilla itself does not pin the
boost's tick order relative to the player's own travel (the rocket ticks in
level entity-iteration order, independent of the player), so there is no
"before or after `tick_elytra`" answer to reproduce either.

## How to change it

- Both functions are pure-ish (`apply_riptide` additionally needs a
  `CollisionView`/`PhysicsProfile` for the on-ground pop-up move). Neither
  reads or writes anything about items, enchantments or entities — if you
  need the trigger side, it belongs in the interaction/entity layer that
  already owns item-use and entity spawning, not here.
- `tests/riptide.rs` and `tests/firework_boost.rs` predict exact values from
  the decompiled formulas (trig identities for the trivial cases, so the
  expected numbers do not originate from this crate's own trig table except
  where a second, cross-checking case deliberately reuses it against a
  different yaw/pitch).

## Configuration

None.

## Dependencies

- `lodestone-physics` — `apply_riptide`, `apply_firework_boost`,
  `PlayerState::{auto_spin_attack_ticks, is_auto_spin_attack}`,
  `Pose::SpinAttack`, `entity::move_entity`, `mth::{sin, cos}`.
- Not yet wired to any consumer outside `lodestone-physics` — see "What is
  not modelled" above. The next owner is whichever layer handles item use
  and entity spawning (outside this crate's cluster in this repo).
