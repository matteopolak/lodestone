# Mob spawn equipment: what a mob spawns holding and wearing

## What it is

Vanilla's `Mob.populateDefaultEquipmentSlots` and its per-species overrides, ported so
a mob can spawn holding a weapon or wearing armour at all. Before this, nothing in the
workspace produced an `(EquipmentSlot, item id)` pair for a *mob* —
[`equipment-combat-stats.md`](./equipment-combat-stats.md)'s attribute-modifier fold
existed and worked, but its only producer was a player's inventory. A drowned's
`RangedAttackGoal` trident builder had existed for a while with zero producers of "is
this drowned holding a trident", and a naturally-armoured zombie was not a thing this
sim could represent.

## How it works

```text
populate_default_equipment_slots(species, rng, special_multiplier, hard)
  -> EquipmentSlots { main_hand, off_hand, head, chest, legs, feet }
  -> .iter()                        (slot, item id) pairs
  -> equipment::apply_equipment     the SAME fold equipment-combat-stats.md uses
  -> defenses_from_attributes / attack_damage_from_attributes / knockback_resistance_from_attributes
  -> SimMob::{defenses, attack_damage, knockback_resistance}
```

`lodestone_entity::spawn_equipment` is one function per species family, table-shaped
like `lodestone_entity::ai::roster`'s goal tables, transcribed from each class's own
`populateDefaultEquipmentSlots(RandomSource, DifficultyInstance)`:

| species | calls `super`? (gets the base armour roll) | own addition |
|---|---|---|
| unlisted (the fallback) | — | `base_armor_roll` alone |
| zombie / husk / zombie_villager | yes | 1%/5%(Hard) chance of an iron sword/spear/shovel at 1/6, 1/6, 4/6 |
| drowned | **no** | 10% chance of a weapon, 10/16 of that a trident (**6.25%** overall), else a fishing rod |
| skeleton / stray / bogged / parched | yes | an unconditional bow |
| wither_skeleton | **no** | an unconditional stone sword |
| pillager | **no** | an unconditional crossbow |

`base_armor_roll` is `Mob`'s own generic roll: a `0.15 * special_multiplier` chance of
any armour at all, then an `armor_type` in `0..=5` (`nextInt(3)` plus up to three
`+1` bumps at `10.87%` each), then a walk over `[Head, Chest, Legs, Feet]` that stops
early after the first slot with a per-difficulty chance (`10%` Hard, `25%` otherwise)
and never overwrites an occupied slot.

`EquipRandom` is the RNG seam (`next_f32`/`next_int`, matching
`lodestone_entity::ai::MobController`'s own shape) so this crate stays free of any
concrete RNG type. `lodestone-server`'s `SpawnRng` implements it via a plain `impl
EquipRandom for SpawnRng` in `mobs/mod.rs` (a local `impl` of a foreign trait for a
local type — always allowed).

### The drowned's trident, end to end

Vanilla always registers `DrownedTridentAttackGoal` (`Drowned.addBehaviourGoals`) and
gates it at *runtime* via `canUse`'s `getMainHandItem().is(Items.TRIDENT)` — not by
conditionally registering the goal. This is reproduced exactly:

* `MobController::main_hand_item()` (new, default `None`) exposes what a mob holds,
  backed by a `NavigatingMob` field set once at spawn.
* `RangedAttackGoal::with_required_main_hand("trident")` adds an optional gate to the
  generic ranged-attack goal; `trident_attack` is the one caller that sets it.
* `hostile_melee::DROWNED`'s trident row registers `ranged::trident_attack`
  unconditionally, same priority as the melee row — vanilla resolves the conflict by
  whichever `can_use` returns true, not by precedence.

### The join to the server

`MobSim::spawn_species` rolls equipment on its own RNG stream
(`equipment_rng`, seeded by `EQUIPMENT_ROLL_SEED`) so an equipment roll cannot shift
which roll a despawn check or an orb merge sees — the same isolation
`orb_rng`/`tame_rng`/`patrol_rng` already have. `MobSim::set_spawn_difficulty` is the
injection point for `special_multiplier`/`hard`; left at the `0.0`/`false` defaults, no
armour ever rolls (correct for a fresh world's effective difficulty), and the
drowned's trident roll is unaffected either way since `Drowned` does not call `super`.

## How to change it

Add a species by reading its own `populateDefaultEquipmentSlots` override in
`.cache/mc/26.2/src/net/minecraft/world/entity/`: check whether it calls `super`
(gets `base_armor_roll` first) and transcribe the rest in the same RNG call order —
a reordered pair of `next_f32`/`next_int` calls changes what a fixed seed produces,
even though this crate makes no promise of a byte-identical stream with a real vanilla
server (`SpawnRng` is a different generator entirely).

### Gotchas

* **`EQUIPMENT_POPULATION_ORDER` is `[Head, Chest, Legs, Feet]`, and the loop's
  `partial_chance` check runs *before* `first = false`**, so a break on the second
  slot's check means that slot is never set either — the break happens ahead of the
  slot-fill, not after.
* **The armour tier ladder is not monotonic in real defence**: `leather(0), copper(1),
  golden(2), chainmail(3), iron(4), diamond(5)` is vanilla's own order, and a golden
  helmet (tier 2) is weaker than a chainmail one (tier 3) despite gold usually reading
  as "better" than nothing in casual intuition.
* **`Drowned`, `WitherSkeleton` and `Pillager` do not call `super`** — transcribing
  them as "the zombie/skeleton family's base roll plus an override" gives them armour
  vanilla never puts on them.

## Disclosed gaps

* **`populateDefaultEquipmentEnchantments` (enchanted spawn gear) is not modelled at
  all** — there is no enchantment model in this workspace, the same gap
  `equipment-combat-stats.md` already discloses for `Defenses::enchant_protection`.
* **`SavedEntity` carries no equipment NBT.** A mob's rolled equipment does not
  survive a save/load round trip.
* **`Items.IRON_SPEAR`** (a 26.2 addition, one of the zombie family's three possible
  weapon rolls) has no attack-damage entry in `equipment::weapon_attack_damage` — an
  honest gap in the *combat-stats* side, not in the roll itself, which still equips it
  correctly.

## Configuration

`EQUIPMENT_ROLL_SEED` (`mobs/mod.rs`) is the default RNG seed;
`MobSim::set_equipment_rng` overrides it for a gate that needs a known first draw.
`MobSim::set_spawn_difficulty(special_multiplier, hard)` is the only other knob.

## Dependencies

`lodestone_entity::equipment` for the fold (see
[`equipment-combat-stats.md`](./equipment-combat-stats.md)), `lodestone_entity::ai`
for `MobController`/`RangedAttackGoal`. No world or protocol access — pure RNG in,
`EquipmentSlots` out.
