# Mob ranged attacks

## What it is

The goal family that makes a mob shoot: a bow goal, vanilla's generic
`RangedAttackGoal`, and the blaze's three-fireball burst, plus the
`ProjectileLaunch` intent that carries a shot from the AI layer out to whoever
owns entity ids. Issue [#227](https://github.com/matteopolak/lodestone/issues/227)
unit B3a. Before it, `RangedAttackGoal` and `BowAttack` were zero hits tree-wide
and no mob in this repo could throw anything.

## How it works

Everything lives in `crates/lodestone-entity/src/ai/roster/ranged.rs` except the
intent type, which is in `crates/lodestone-entity/src/ai/mob.rs` next to the
trait that carries it.

A goal never spawns an entity. It computes vanilla's aiming maths, then calls
`MobController::launch_projectile` — deliberately the same shape
`MobController::attack` already had for melee, because `lodestone-entity` has no
world, no id allocator and no projectile registry. `NavigatingMob` accumulates
the launches; a host drains them with `take_new_launches()` once per tick.

```
BlazeFireballGoal::tick
  -> MobController::launch_projectile(ProjectileLaunch { kind, origin, velocity })
  -> NavigatingMob.launches                              (lodestone-entity ends here)
  -> MobSim::tick drains take_new_launches()              <-- NOT WIRED YET (#460)
  -> MobSim::spawn_projectile -> ProjectileRegistry
  -> MobSim::snapshots() lowers it to an EntitySnapshot   (mobs.rs:2281-2310)
  -> LiveMobSource -> EntityStreamer::sync -> encode_add_entity  (server.rs:248-262)
  -> a real client sees the projectile appear
```

### Why a mob does not visibly shoot in a running game

Both remaining reasons are outside `ranged.rs`, and both must close before
"skeletons shoot players" is true. Stating them together because each one alone
looks like the only blocker:

1. **Nothing drains the launches** —
   [#460](https://github.com/matteopolak/lodestone/issues/460). The fourth line
   above does not exist. `MobSim::tick` resolves melee intents (`mobs.rs:1578`)
   but not launches, so `ProjectileRegistry` stays empty. Outside `ranged.rs`'s
   own tests, `take_new_launches` has **zero** callers tree-wide. One patch in
   `mobs.rs`; see *How to change it*.
2. **No ranged species is spawned** —
   [#457](https://github.com/matteopolak/lodestone/issues/457).
   `seed_demo_mobs` seeds `minecraft:zombie` only, so there is no blaze or
   skeleton in a running world to shoot at all.

**Closed since this doc was drafted:** target acquisition. B3 listed
[#455](https://github.com/matteopolak/lodestone/issues/455) here as a third
blocker — `NavigatingMob::find_nearest_target` returned `self.attack_target`, the
field written by the only goal that calls it, so the loop could not bootstrap and
every `can_use` in this family was false in practice. It now reads the
`nearest_player` feed and applies vanilla's follow-range cut. The goals here no
longer need a target handed to them, though every *gate* here still sets one
explicitly, because a hermetic `PathWorld` has no player in it.

The projectile *wire* path is **not** among them. It has worked since issue
\#211 — `MobSim::snapshots` already lowers tracked projectiles onto the same
`ADD_ENTITY` path a mob uses. A3's survey recorded `ProjectileRegistry` as
"permanently empty at runtime", which is true, but the cause is only (1).

### The goals

| goal | vanilla | shape |
|---|---|---|
| `RangedBowAttackGoal` | `ai/goal/RangedBowAttackGoal.java` | 20-tick draw, release, then an interval; walks in when out of radius |
| `RangedAttackGoal` | `ai/goal/RangedAttackGoal.java` | no draw phase; interval lerps with range between min and max |
| `BlazeFireballGoal` | `monster/Blaze.java:156-252` | bursts of **three**, 6 ticks apart, after a 60-tick wind-up, then a 100-tick pause; melees below 2 blocks |

The blaze's step machine is the one worth re-reading before touching: `attackStep
> 1` is tested *after* the reset to 0, which is why step 5 fires nothing and a
burst is three fireballs rather than four.

### Ballistics

`ProjectileLaunch::aimed` is vanilla `Projectile.getMovementToShoot`
(`projectile/Projectile.java:130-139`): normalise the direction, scale by power.

* Arrows, tridents, snowballs, potions: power **1.6**, with vanilla's
  `horizontalDistance * 0.2` arc lift added to the vertical component.
* Small fireballs: power **0.1**, and **no arc lift** — `SmallFireball` is an
  `AbstractHurtingProjectile`, whose constructor is
  `direction.normalize().scale(accelerationPower)` with `accelerationPower`
  defaulting to 0.1 (`AbstractHurtingProjectile.java:24`, `:180-183`). It
  accelerates in flight rather than falling. Sixteen times slower off the muzzle
  than an arrow, and using 1.6 here is the obvious mistake.

## How to change it

**Adding a species:** add its path to `SPECIES` and an arm to `lookup`, with the
full `addGoal` multiset from the jar — every row, including `Missing` ones. The
multiset gate compares against a cited `File.java:line`, so an omitted row fails
rather than passing quietly.

**Adding a projectile:** add a `ProjectileKind` variant, its registry path to
`projectile_entity_type`, and its integration family to `integrates_as_arrow`.
Both are gated.

**Gotchas, each of which cost something:**

* **A wrong registry name in `projectile_entity_type` does not fail — it streams
  the wrong entity.** `encode_add_entity_body` resolves the type with
  `entity_type_id(name).unwrap_or(0)`
  (`crates/protocol/v770/src/server_protocol.rs:1117`), and 0 is a real entity
  the client renders happily. `every_projectile_kind_names_a_real_entity_type`
  exists for exactly this and checks against `lodestone-data`'s generated
  registry, not against a list in this repo.
* **`SpeedProbe`'s default 4-block target records no `move_to` for a blaze.** Four
  blocks is inside its follow range and outside its 2-block melee radius, so
  vanilla has it stand still and shoot. Reading that as "the builder passes no
  speed" would be wrong; drive the distance per species.
* **Do not test these against `ScriptMob`.** It overrides all eight perception
  methods, which is how [#441](https://github.com/matteopolak/lodestone/issues/441)'s
  island stayed hidden — eight goals with green tests and a constant-false
  `can_use` in production. Every gate here drives a real `NavigatingMob`.
* **Skeleton, stray, bogged, parched and drowned are not in this family** — they
  resolve through `hostile_melee.rs`, which claimed them first. `bow_attack` and
  `trident_attack` are `pub` so their rows can be registered from there, and
  `bow_attack` now is: the shared `SKELETON` table's priority-4 row is
  `RangedBowAttackGoal`, covering skeleton, stray, bogged and parched
  ([#226](https://github.com/matteopolak/lodestone/issues/226)).
* **The skeleton's weapon branch is decided by equipment, not by a goal
  override.** `AbstractSkeleton.populateDefaultEquipmentSlots` hands out a `BOW`
  unconditionally (`:109-112`), so `reassessWeaponGoal`'s `is(Items.BOW)` test at
  `:137` is always true and the melee `else` at `:146` is unreachable — the table
  carried that dead branch until #226. `WitherSkeleton` is the sole exception, and
  only because it overrides the *equipment* method with a `STONE_SWORD`
  (`WitherSkeleton.java:74-76`); it never overrides `reassessWeaponGoal`. If you
  are deciding whether a variant shoots, read `populateDefaultEquipmentSlots`, not
  the goal registration.
* **`trident_attack` is deliberately *not* registered.** The drowned's row stays
  `Missing`: only ~15% of drowned hold a trident and this repo has no inventory
  model, so promoting it would make *every* drowned a thrower — and put a second
  priority-2 MOVE claimant alongside `DrownedAttackGoal`. Same trap the skeleton
  fix avoided by *replacing* its row rather than adding one.
* **A priority multiset cannot see a wrong occupant of the right slot.** Swapping
  melee for bow at priority 4 changes only a class-name string, so the table gates
  may or may not fire. The gate that catches it asserts behaviour through a real
  `NavigatingMob`: launches recorded, no melee attack, and a stop at the bow's
  15.0 radius rather than at contact —
  `a_skeleton_shoots_from_range_and_a_wither_skeleton_closes_and_punches` in
  `hostile_melee.rs`. Note the distance half needs a target **beyond** 15 blocks:
  from 6 blocks a correct bow goal still walks in, because it only parks once
  `seeTime` reaches 20, and 20 ticks at 0.25 blocks/tick is 5 blocks.

### The remaining `mobs.rs` patch

Tracked as [#460](https://github.com/matteopolak/lodestone/issues/460), and not
applied here because `mobs.rs` was held by another agent when this landed.

The drain, mirroring how `take_detonations` is drained from `tick.rs:545`. Inside
`MobSim::tick`'s existing per-mob loop, alongside the `take_new_attacks()` drain
at `mobs.rs:1578`, collect `(id, launch)` pairs, then after the loop:

```rust
for (_shooter, launch) in launches {
    let path = lodestone_entity::ai::roster::ranged::projectile_entity_type(launch.kind);
    let projectile = if lodestone_entity::ai::roster::ranged::integrates_as_arrow(launch.kind) {
        Projectile::arrow(launch.origin, launch.velocity)
    } else {
        Projectile::throwable(launch.origin, launch.velocity)
    };
    self.spawn_projectile(
        ResourceKey::from_str(&format!("minecraft:{path}")).expect("static key"),
        projectile,
    );
}
```

Collect-then-spawn in two passes for the reason the melee resolution already
does: `spawn_projectile` takes `&mut self` and the loop holds a borrow into
`self.mobs`.

## Configuration

None. No env vars, no features. Priorities and every numeric constant are
transcribed from the jar with the citation inline; there is no tuning surface and
adding one would make the multiset gate meaningless.

## Dependencies

* `crates/lodestone-entity/src/ai/mob.rs` — `MobController`, `ProjectileLaunch`,
  `ProjectileKind`.
* `crates/lodestone-entity/src/ai/navigating_mob.rs` — the production controller
  and the `launches` accumulator.
* `crates/lodestone-entity/src/ai/roster/mod.rs` — `Registration`, `Selector`,
  `Coverage`, the shared builders, and `goals_for`, the entry point
  `MobSim::spawn_species` calls.
* `crates/lodestone-entity/src/projectile.rs` — `Projectile`,
  `ProjectileRegistry`, the per-tick integration the host drives.
* `lodestone-data` — the generated entity-type registry the name gate checks
  against.
* `.cache/mc/26.2/src/net/minecraft/world/entity/` — every number here is cited
  to it.
