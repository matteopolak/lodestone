# Riptide and the elytra firework boost

## What it is

Two item-driven velocity impulses: the riptide-trident launch (#208) and the
elytra firework-rocket glide boost (#206). The **arithmetic** lives in
`lodestone-physics` and the **triggers** — item use, held duration, the wet
gate, the enchantment level, the glide flag, the rocket's duration — live in
the shell and `lodestone-ecs`, because none of them is physics state.

**This doc used to say the triggers did not exist**, and for two commits that
was true: both functions were islands, reachable only from their own tests, and
using a firework while gliding or releasing a Riptide trident in water did
nothing at all. The drivers landed with #206/#208's second pass and have their
own section below. What is still missing is enumerated there rather than in
prose: the spin-attack damage sweep (server-side), the rocket's random lifetime
term (server-side RNG), and a real enchantment-registry name lookup.

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

`lodestone_physics::riptide_spin_attack_strength(level)` resolves a Riptide
level into that `strength`, read out of
`data/minecraft/enchantment/riptide.json` — `1.5 + 0.75 * (level - 1)`, so
**1.5 / 2.25 / 3.0**. The per-level term is `0.75`, *not* the `0.5` a
half-remembered `1.5, 2.0, 2.5` ladder implies, and the difference at Riptide
III is a full block per tick. `tests/riptide.rs` asserts the whole ladder and
explicitly excludes the `0.5` hypothesis.

**Still not modelled**: the attack-damage half of `startAutoSpinAttack`
(`autoSpinAttackDmg`, the entity-hit sweep in `checkAutoSpinAttack`). That is
`hurtServer` on the server side, and this client applies no damage anywhere —
the spin *state* and *pose* are client-side and are modelled.

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

The rocket is its own entity in vanilla, ticked by the level's normal entity
loop, and `apply_firework_boost` deliberately models only the one line above.
The driver is `lodestone_ecs::player::FireworkBoost` — a **per-use tick
countdown**, not a tracked entity, because this client does not decode the
rocket's `DATA_ATTACHED_TO_TARGET` entity data and so cannot see the
attachment. See "The drivers" below for the duration it predicts.

Vanilla itself does not pin the boost's tick order relative to the player's own
travel (the rocket ticks in level entity-iteration order, independent of the
player), so there is no "before or after `tick_elytra`" answer to reproduce.
`tick_firework_boost` runs **before** `player_physics`, which is the
lower-latency of the two indistinguishable choices.

### The drivers

Both triggers are **client-predicted**, because vanilla's are. A vanilla client
runs `TridentItem.releaseUsing` itself (`MultiPlayerGameMode.releaseUsingItem` →
`LivingEntity.releaseUsingItem`) and applies the launch locally, and its copy of
the firework rocket applies the boost from the rocket's own client-side `tick`.
That is why both feel instant, and why doing them server-only would feel wrong
even if it were possible here.

#### Riptide: the release edge

`Sim::use_item_live` arms `ItemUseTicks(Some(0))`; `tick_item_use` advances it
once per 20 Hz tick; `Sim::end_use_live` takes it and calls `Sim::maybe_riptide`,
**before** sending `RELEASE_USE_ITEM` — vanilla's own order. The three gates:

| vanilla | here |
|---|---|
| `timeHeld >= 10` (`TridentItem.THROW_THRESHOLD_TIME`) | `ItemUseTicks`, in ticks not frames |
| `getTridentSpinAttackStrength(stack, player) > 0.0F` | `Sim::riptide_level` × `riptide_spin_attack_strength` |
| `isInWaterOrRain() && !isPassenger()` | `Sim::is_in_water_or_rain` |

A dry-land release with a Riptide trident does nothing, exactly as in vanilla —
the wet gate is a gate, not a decoration on the impulse.

**Two known wrong edges, both recorded rather than hidden:**

- **`is_in_water_or_rain` has no `canSeeSky`.** Vanilla's `Level.isRainingAt` is
  `isRaining() && canSeeSky(pos) && precipitationAt(pos) == RAIN`; this client
  has neither (`app/weather.rs`'s own doc records the same gap for the
  rain-muffling sound path), so a non-zero rain level stands in for the whole
  predicate. It fails toward *allowing* a riptide under a roof, costing one
  corrective teleport in that case. Refusing in the open — where riptide is
  actually used — would make the feature unreachable, which is the worse error.
- **`riptide_level` compares a hardcoded registry id.** `ItemEnchantment` carries
  the session-scoped `minecraft:enchantment` registry id, not a name, and nothing
  in this client surfaces that registry: the id → name table *is* decoded (the
  v770 adapter's `ClientRegistries::entry_names`) but is never emitted as a
  `ClientEvent`. So the id is derived the one way the wire allows — dynamic
  registries arrive **sorted by resource location** (measured on the creative
  oracle for `dimension_type`, see `crates/protocol/v770/src/packets/registry.rs`),
  and `riptide` is the 33rd of 26.2's 43 built-in enchantments alphabetically,
  holder id **32**.

  A data pack that adds or removes an enchantment sorting before `riptide` shifts
  every id, and this would then read some other enchantment's level — failing
  toward launching on the *wrong* trident. **The fix is a protocol change, not a
  change here**: emit `ClientEvent::EnchantmentRegistryNames { names }` at Login
  from `registries.entry_names("minecraft:enchantment")`, exactly as
  `BiomeRegistryNames` already does, then match on `"minecraft:riptide"` and
  delete the constant.

#### Elytra firework boost: the use edge

`Sim::use_item_live` → `start_firework_boost_if_gliding`: held item is
`minecraft:firework_rocket` and `PlayerState::fall_flying` is set, so
`FireworkBoost(20)`. `tick_firework_boost` spends one per tick and applies the
impulse only while still gliding (`FireworkRocketEntity.tick`'s attached branch is
gated on `attachedToEntity.isFallFlying()`, so a rocket on a player who stops
gliding keeps ticking down and boosts nothing).

**The 20 is the deterministic floor of vanilla's lifetime, and that is a
deliberate under-prediction.** Vanilla's rocket lives
`10 * flightCount + random.nextInt(6) + random.nextInt(7)` ticks with
`flightCount = 1 + fireworks.flightDuration()`. The two random terms are rolled on
the *server's* RNG — the vanilla client never computes them at all, because its
rocket arrives with `lifetime = 0` and simply keeps boosting until the server
removes the entity. With no rocket entity here, `10 * 2 = 20` is what can be
predicted honestly: about five ticks short of vanilla's average, never long. The
player is authoritative over their own position, so there is nothing to desync —
only a slightly weaker boost. `flightDuration` is itself an undecoded
`minecraft:fireworks` component, hence the standard 1-gunpowder rocket's `2`.

#### Getting into a glide at all

`fall_flying` had **zero writers** in this tree before #206 — `tick_elytra` was
reachable only from `lodestone_physics::tick`'s dispatch on a flag nothing set. The
state machine is `lodestone_ecs::player::update_fall_flying_state`; see
[`local-player-components.md`](./local-player-components.md)'s "Glide state is
client-authoritative on the way in and predicted on the way out".

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
- `lodestone-ecs` — `player::{FireworkBoost, ItemUseTicks, GliderEquipped,
  tick_firework_boost, tick_item_use, update_fall_flying_state,
  send_fall_flying_command}`.
- `lodestone-shell` — `sim/actions.rs`'s `maybe_riptide`,
  `start_firework_boost_if_gliding`, `riptide_level`, `is_in_water_or_rain`,
  `glider_equipped`; `sim/step.rs` pushes `GliderEquipped` once per tick.
- `lodestone-model` — `PlayerCommand::StartFallFlying` (whose **first producer**
  is `send_fall_flying_command`; four adapters encoded it and nothing sent it),
  `ItemEnchantment`.
