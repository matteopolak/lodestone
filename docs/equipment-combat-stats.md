# Equipment combat stats: the feed the damage pipeline never had

## What it is

What an equipped item contributes to a living entity's combat attributes — armour
points, armour toughness, knockback resistance, weapon attack damage — and the wiring
that makes a player's held sword and worn armour reach the damage pipeline. The
reduction formulas were live-verified against a real vanilla 26.2 server long before
anything populated a `Defenses`: armour reduced a number that came from a per-species
base attribute and never from a helmet, and every player attack dealt a flat
bare-hand `1.0`.

## How it works

`lodestone_entity::equipment` emits vanilla's own attribute **modifiers** into the
existing `AttributeMap`, rather than carrying a parallel arithmetic:

```text
(slot, item id) pairs
  -> item_modifiers          rows read from ArmorMaterials / ToolMaterial / Items
  -> apply_equipment         inserts Modifier { id, amount, AddValue } into AttributeMap
  -> AttributeMap::value     vanilla's AddValue -> AddMultipliedBase -> AddMultipliedTotal fold
  -> defenses_from_attributes / attack_damage_from_attributes
```

There is no new attribute system. `lodestone_entity::attribute` already modelled
vanilla's arithmetic exactly, and vanilla's equipment stats *are* nothing but
attribute modifiers (`ArmorMaterial.createAttributes`,
`ToolMaterial.createSwordAttributes`), so this module is a table plus a fold. A future
source of modifiers — an attribute-flavoured status effect, `/attribute`, an
enchantment — lands in the same map.

Two properties fall out of using vanilla's real modifier ids
(`minecraft:armor.helmet`, `minecraft:base_attack_damage`) rather than inventing
per-item ones. `AttributeInstance::add_or_update` is keyed by id, so **two helmets
cannot stack** and a sword replacing an axe **overwrites** rather than accumulates —
both the vanilla behaviour.

### The join to the server

`PlayerInventory::combat_equipment` yields the six combat slots; `combat_stats` folds
them. Native indices are feet `36`, legs `37`, chest `38`, head `39`, off-hand `40`,
and the main hand is the **selected** hotbar slot — not native `0`, or a player
holding a sword in slot 3 punches for `1.0`.

| consumer | what it reads |
|---|---|
| `apply_attack` (`server.rs`) | `attack_damage` — a diamond sword now deals `7.0`, a fist `1.0` |
| `PlayerVitals::apply_damage` | `defenses` — worn armour reduces an incoming hit |

## How to change it

Adding an item is a row in `item_modifiers`. Adding a *stat* is another `Modifier`
emitted from that same function — nothing downstream enumerates attributes, so a new
one flows through `apply_equipment` and out of `AttributeMap::value` with no other
edit.

### Gotchas

* **`ItemModifier::slot` is load-bearing.** A modifier only applies in the slot
  vanilla publishes it for, which is why `apply_equipment` takes `(slot, item)` pairs
  rather than a bare item list. Without the check, a sword in the off-hand or a helmet
  in the hotbar would add its stats anyway.
* **`makeDefense`'s argument order is boots-first**: `makeDefense(boots, legs, chest,
  helm, body)`. Reading it head-first — which its own signature invites — swaps a
  helmet's `1` with a boot's `3`. For iron the *total* is 15 either way, so only a
  per-piece assertion can see it.
* **A weapon's published modifier is `attackDamageBaseline + material.attackDamageBonus`.**
  A diamond sword is `3.0 + 3.0`, not `3.0`. A hoe's baseline is *negative* on four
  tiers. A trident (`8.0`) and a mace (`5.0`) are flat literals, not tier-derived, and
  neither item id contains an underscore — so a `rsplit_once('_')?` ahead of the
  flat-literal branch reports that a trident publishes no attack damage at all.
* **The player's `attack_damage` base is `1.0`, not the registry's `2.0`.**
  `Player.createAttributes` overrides it; `player_attributes()` exists so that cannot
  be got wrong by accident. Taking the registry default doubles every punch.
* **Adding real weapon damage breaks gates written against the flat `1.0`.** Grep for
  the constant rather than discovering them.

## Disclosed gaps

* **Enchantment protection.** `Defenses::enchant_protection` and
  `enchant_effectiveness` are per-hit fields and there is no enchantment model to
  derive an EPF from; they stay at their neutral defaults, which is an accurate
  statement of what currently reduces damage rather than a stub.
* **Mob equipment is now modelled, and it feeds this exact module.** See
  [`mob-spawn-equipment.md`](./mob-spawn-equipment.md):
  `lodestone_entity::spawn_equipment::populate_default_equipment_slots` rolls what a
  mob spawns holding and wearing, and `MobSim::spawn_species` folds the result through
  `apply_equipment`/`defenses_from_attributes` — the same functions this module
  exports — so a naturally-armoured zombie or a drowned with a trident really does
  fight differently now. `SavedEntity` still carries no equipment NBT (a saved mob's
  gear does not survive a save/load round trip yet), which is the honest remaining
  gap.
* **Attack-cooldown scaling and critical hits.** `Player.attack`'s
  `baseDamageScaleFactor()` needs a server-tracked attack-strength ticker, which does
  not exist.

## Configuration

None. `PLAYER_BASE_ATTACK_DAMAGE` and `BASE_ATTACK_DAMAGE_ID` are `pub`.

## Dependencies

`lodestone_entity::attribute` for the fold, `lodestone_entity::damage` for the
`Defenses` shape, `lodestone_model::Identifier` for the keys. The gates live in the
module itself plus `server.rs`'s own test module (the two production consumers).
