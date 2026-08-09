# Experience

## What it is

The XP level curve, the orb denomination ladder and a player's experience state.
`crates/lodestone-server/src/experience.rs` holds all three as pure arithmetic;
`ServerProtocol::encode_set_experience` puts the result on the wire. The one
production producer today is furnace smelting, paid out when the player closes the
menu.

## How it works

### The level curve is three regimes, and the boundaries are the bug

`Player.getXpNeededForNextLevel`, transcribed as `level_up_cost`:

| level range | cost of the next level |
|---|---|
| `0..15` | `7 + level * 2` |
| `15..30` | `37 + (level - 15) * 5` |
| `>= 30` | `112 + (level - 30) * 9` |

Both seams are **inclusive** — level 15 is in the middle regime, level 30 in the top
one. The trap is that at level 15 the two hypotheses *coincide*
(`7 + 15*2 == 37 + 0*5 == 37`), so an exclusive-boundary implementation is only
distinguishable at level 16: inclusive gives 42, exclusive gives 39. The gate uses
16 and 31 for exactly that reason.

30 levels is **1395** points; the first level is 7.

### Orb denominations are greedy change-making, not division

`ExperienceOrb.getExperienceValue` returns the largest entry of
`[2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3, 1]` that fits, and
`awardWithDirection` loops. So 100 becomes `[73, 17, 7, 3]` — **four** orbs, and the
tail is `3` (a denomination in its own right) rather than `1+1+1`. A uniform-cap
implementation emits a different orb *count* for almost every amount, and orb count
is player-visible.

The ladder is roughly doubling but irregular (`3 → 7` is ×2.33, `7 → 17` is ×2.43),
so it is transcribed rather than generated.

### `give_points` re-expresses its carry

The overflow fraction is multiplied back by the **old** level's cost to recover
points, the level increments, and it is divided by the **new** cost. Leaving it as
`progress - 1.0` charges every subsequent level the first level's price of 7, so a
single large award over-levels badly. The downward loop is not symmetric: at level 0
it zeroes progress and total rather than borrowing, so XP cannot go negative.

`total` is genuinely a third piece of state, not derived: vanilla zeroes it on a level
underflow and never recomputes it, so a player who has enchanted has a `total` that no
longer matches their level.

### The one wired producer

Furnace smelting. `Furnace::take_recipes_used` banks `(recipe_key, count)` per cook
and had **no caller**; `experience_for_recipes` now drains it in `server.rs`'s
`ContainerClosed` arm, which is vanilla's own
`awardUsedRecipesAndPopExperience` trigger. The recipe key is
`"<table>:<ingredient>"` and the ingredient is itself namespaced, so **split on the
first colon only** — splitting anywhere else yields a table name no lookup knows and
the function silently returns zero for every entry, which looks like "no XP yet"
rather than a failure.

## How to change it

* **A new XP source** (breeding, fishing, ore-breaking, mob death): call
  `PlayerExperience::give_points` and send `encode_set_experience`. **Do not add a
  second curve.**
* **Spending XP** (enchanting): `PlayerExperience::take_levels`, which zeroes progress
  and total on underflow — clamping only the level leaves a full bar at level 0.
* **Persistence**: `XpLevel` / `XpP` / `XpTotal`, vanilla's own names, via
  `PlayerExperience::restored`. **`PlayerData` does not read or write them yet**, so
  XP resets on rejoin.

### Gotchas

* `SET_EXPERIENCE`'s wire order is **progress, level, total** — not declaration order
  and not alphabetical. The client-side decoder already carried that warning before
  anything encoded the packet.
* `restored` clamps progress strictly **below** 1.0. Exactly 1.0 is the state the
  carry loop exists to resolve, so keeping it would level the player up on their next
  award of nothing.

## What is not here, and what it needs

**The orb entity.** `ExperienceOrb` is a real entity with a value, an age, a pickup
radius, an absorption animation, and a merge rule keyed on the *entity id*
(`(orb.getId() - id) % 40 == 0 && orb.getValue() == value`, plus a `nextInt(40)`
draw), which is not reproducible without one. `MobSim` has no orb variant and streams
no orb metadata. `orb_denominations` returns the values a spawner would use, and is
exact to the integer without an entity existing.

The consequence: XP from smelting goes **straight to the player's bar** rather than
popping an orb they walk into. That is the difference between "no XP exists" and "XP
exists without a flying orb"; the second is the honest subset.

**Mob-death XP** needs the same entity plus a mob-death drain out of `MobSim`, which
does not exist (`take_detonations` and `take_grazes` are the pattern it would follow).

## Dependencies

None inside `experience.rs` — pure arithmetic, usable from any thread with no setup.
The producer path depends on `crate::furnace` and `crate::block_entities`.
