# Burning

## What it is

Entity burning: ignition from fire and lava, the fire-tick damage interval, the
lava-vs-fire duration distinction, and Fire Resistance immunity.
`crates/lodestone-server/src/burning.rs` holds the rules as a pure value type;
`server.rs`'s vitals timer is both the ignition producer and the burn consumer.

**Fire *spread* between blocks is `docs/fire-spread.md`'s.** This is the entity-facing
half: standing in fire sets a counter, the counter deals damage on an interval, and it
keeps burning after the entity walks out.

## How it works

### The burn tick, and the lava guard

`Entity.baseTick`:

```java
if (this.remainingFireTicks > 0) {
   if (this.fireImmune()) {
      this.clearFire();
   } else {
      if (this.remainingFireTicks % 20 == 0 && !this.isInLava()) {
         this.hurtServer(serverLevel, this.damageSources().onFire(), 1.0F);
      }
      this.setRemainingFireTicks(this.remainingFireTicks - 1);
   }
}
```

Three details, each of which changes a number:

* **The counter counts down and the modulo is on the remaining value.** An 8-second
  ignition is 160 ticks, so hits land at remaining 160, 140, …, 20 — **exactly 8**, and
  none at 0 because the outer `> 0` fails first.
* **`&& !this.isInLava()`.** While standing in lava the burn deals **no** damage of its
  own; lava's 4.0 per tick is the damage. Without the guard an entity in lava takes
  5.0. The counter still ticks down, so leaving lava leaves the remainder burning.
* **`fireImmune()` calls `clearFire()`, which is `min(0, remaining)` — not `0`.** A
  negative counter is a grace period and must survive.

### Ignition only ever raises

`igniteForTicks` is guarded by `if (this.remainingFireTicks < numberOfTicks)`. So
stepping out of lava (300 ticks) into fire (160) does **not** shorten the burn. A plain
assignment makes "walking through a campfire puts out your lava burn", which looks like
nothing happening.

| source | vanilla call | ticks | contact damage |
|---|---|---|---|
| fire | `BaseFireBlock.fireIgnite` → `igniteForSeconds(8.0F)` | 160 | 1.0 |
| soul fire | same | 160 | **2.0** |
| lava | `Entity.lavaIgnite` → `igniteForSeconds(15.0F)` | 300 | **4.0** |

Soul fire shares the *duration* and doubles the *contact damage* — `SoulFireBlock`
passes `2.0F` to `BaseFireBlock`'s constructor where `FireBlock` passes `1.0F`.

### The negative counter is a grace period, and the ramp is player-only

`BaseFireBlock.fireIgnite` ramps a **player**'s counter by `nextInt(1, 3)` (1 or 2) per
contact tick and only ignites once it is non-negative. That is why running across one
fire block can leave you unburnt while standing still cannot. A **non-player** entity at
a non-negative counter skips the ramp entirely (`else if (entity instanceof
ServerPlayer)`) and ignites at once.

### Fire Resistance is a damage-source check, not a counter check

`LivingEntity.hurt`: `source.is(DamageTypeTags.IS_FIRE) && hasEffect(FIRE_RESISTANCE)`
→ immune. So the counter **still runs** and the entity still visibly burns; only the
damage is refused. Clearing the counter would put the fire out — visibly different, and
it would also lose the burn when the effect expires.

The `#minecraft:is_fire` tag is `in_fire`, `campfire`, `on_fire`, `lava`, `hot_floor`,
`sulfur_cube_hot`, `unattributed_fireball`, `fireball`. Note `on_fire` (the burn tick)
and `in_fire` (standing in the block) are **two** entries — missing either makes fire
resistance half-work.

## How to change it

* **Another ignition source** (a fireball, a flint-and-steel hit): call
  `BurnState::ignite_for_seconds`, which raises rather than assigns.
* **Extinguishing**: `BurnState::clear`, and note it is `min(0, …)`. Water and rain are
  not wired — the feet-cell fluid read the caller already does for hunger is where it
  would go.
* **A new death message**: `DeathCause::OnFire`'s `message_id` is **`onFire`**,
  camelCase, read out of `on_fire.json`. The same trap `outsideBorder` set.

### Gotchas

* The feet cell, not the eye. `entityInside` fires for any cell the bounding box
  overlaps; reading the eye lets a player stand in fire unharmed up to the chin.
* `DeathCause::OnFire` covers all three sources, and `DeathCause::Wither` covers poison
  and wither. Both collapse only the death *message*; the amounts are distinguished by
  `BurnSource` and by `mob_effects` respectively.
* A creative player is passed as `fire_immune`, not as a separate flag. The observable is
  the same (the fire goes out and nothing hurts) and this crate has no per-entity-type
  immunity table to consult.

## What is not here

* **Lightning.** `LightningBolt` is an entity with weather-gated target selection, a
  `nextInt` draw, direct-hit damage and the creeper→charged / villager→witch
  transformations. It needs an entity type `MobSim` does not have and a per-species
  transformation table. The issue groups it here because a strike's entity-facing effect
  *is* ignition plus a damage instance, and that part is this module.
* **Mob burning.** `MobSim` has no burn state and streams no `on_fire` metadata flag, so
  this is player-only — exactly as `PlayerVitals` was for drowning.
* **Water and rain extinguishing**, and the `on_fire` metadata flag so the client
  renders flames (`BurnTick::on_fire_changed` reports the edge; nothing sends it).
* **Per-entity-type fire immunity** (`Entity.fireImmune`) and `getFireImmuneTicks`.
* **Campfires and magma blocks**, which are `hot_floor`/`campfire` damage types with
  their own contact rules.

## Configuration

None. The interval, durations and damages are all vanilla constants; game mode gates
the whole thing through `Abilities::for_mode().invulnerable`.

## Dependencies

`burning.rs` depends on nothing beyond `std`. The wiring depends on `crate::vitals`
(for `apply_effect_damage`), `crate::mob_effects` (for the Fire Resistance read) and
`crate::mob_spawn::SpawnRng` (for the player ramp draw).
