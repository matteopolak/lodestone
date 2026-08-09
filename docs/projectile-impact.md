# Projectile impact and the player's own launch

## What it is

The half of the projectile system that made arrows matter: hit detection against
terrain and mobs, the per-projectile damage arithmetic, and the serverbound path by
which a **player** launches a bow shot, a snowball, an egg or an ender pearl. Before
this, every projectile in the game came from a mob goal, flew through anything in its
way, and hurt nothing.

## How it works

Three pieces, in two crates.

### The version-free arithmetic — `lodestone_entity::projectile`

Pure functions, no world access:

| function | vanilla source |
|---|---|
| `clip_aabb` | `AABB.clip` — exact slab intersection, returns the entry parameter in `0..=1` |
| `hitbox_margin` | `ProjectileUtil.computeMargin` — `clamp((tickCount - 2) / 20, 0, 0.3)` |
| `impact_effect` | each projectile's own `onHitEntity` |
| `arrow_impact_damage` | `ceil(clamp(speed * baseDamage, 0, i32::MAX))` |
| `bow_power_for_time` | `BowItem.getPowerForTime` — `(t² + 2t) / 3`, clamped at 1 |
| `launch_velocity` | `Projectile.shootFromRotation` ∘ `getMovementToShoot` |

`clip_aabb` is exact rather than sampled on purpose. A bow arrow travels 3.0 blocks
per tick and a mob is 0.6 wide, so any sample spacing cheap enough to use steps over
the target; and the *exact* entry parameter is what lets a block hit at `t = 0.7` and
an entity hit at `t = 0.4` be ordered correctly.

### The search — `MobSim::resolve_projectile_impacts`

Runs **before** `ProjectileRegistry::tick`, once per server tick. This ordering is
`AbstractArrow.tick`'s: it clips the segment it is *about* to travel and only calls
`setPos` if nothing was hit. Resolving after the move would put every impact one tick
late and let an arrow settle on the far side of a wall.

```text
for each tracked projectile:
  segment = position .. position + velocity
  entity_t = nearest mob box (inflated by hitbox_margin, owner excluded) clipped
  block_t  = first solid cell along the segment, quarter-block sampled
  nearer of the two wins
    entity -> impact_effect -> SimMob::apply_damage -> note_hurt -> vocalisation
    block  -> remove
```

Blocks are sampled at quarter-block spacing — the same resolution
`RayView::is_clear` uses on `ChunkWorld`, sound because a collision cell is a full
block wide. Entities are **not** sampled, for the reason above.

`ProjectileMeta::owner` carries the launching entity id. A projectile is created
inside its shooter's own bounding box, so without it a skeleton's first arrow strikes
the skeleton; the zero hitbox margin for the first two ticks is the other half of
vanilla's guard.

### The player's launch — `crates/lodestone-server/src/server.rs`

Two new `ServerBound` variants:

* `UseItem { hand, yaw, pitch }` from `minecraft:use_item`. A throwable is released
  immediately; a bow starts a draw. The packet's own yaw/pitch is why a throw has a
  direction without this crate tracking rotation per connection.
* `ReleaseUseItem` from `PLAYER_ACTION` ordinal **5**, which used to decode to
  `Ignored` — the reason a player could draw a bow and never fire.

`BowDraw` records the `MobSim::tick_count` the draw began on. The charge is a
difference of tick counts, never a `Duration`: this crate links into a wasm32 bundle
where `Instant::now()` compiles and then panics at runtime with no log line.

| item | power | notes |
|---|---|---|
| bow | `getPowerForTime(held) * 3.0` | refused below `0.1`; consumes one `arrow` |
| snowball / egg / ender_pearl / experience_bottle | `1.5` | instant |
| splash_potion / lingering_potion | `0.5`, pitch offset `-20.0` | the offset lifts the arc |

## How to change it

* **A new projectile's damage** is a row in `impact_effect`. Its *damage type* is a
  row in `projectile_damage_type` (`mobs.rs`) — that one is registry data this crate
  owns, which is why it is not folded into the version-free function.
* **A rule that depends on the target's species** cannot live in `impact_effect`,
  which is a pure function of the projectile. `Snowball.onHitEntity`'s
  `entity instanceof Blaze ? 3 : 0` is applied in `MobSim::resolve_projectile_hit`
  for exactly this reason; a second such rule goes beside it.
* **A new launchable item** is a row in `launch_intent`. A crossbow is deliberately
  *not* folded in with the bow: its charge lives in an item component this crate does
  not model, and firing it like a bow is wrong in a way that looks right.

### Gotchas

* **The impact pass must stay before the motion tick.** Swapping them is a one-line
  change that produces plausible-looking behaviour and lets arrows pass through
  walls.
* **A zombie has 2 points of armour** (`Zombie.createAttributes`), so a health delta
  against a zombie reads `5.904` for a raw `6.0` arrow, not `6.0`. Use a species with
  no armour (a cow) to read raw damage. This cost four failed assertions.
* **A plain arrow's knockback is genuinely `0.0`** — `AbstractArrow.doKnockback`
  multiplies by an enchantment-derived value that is zero without Punch. An arrow hit
  correctly does not shove.
* **`ceil`, not truncation.** A spent arrow at `0.2` blocks/tick deals `1`, not `0.4`.

## Disclosed gaps

Each has a reason rather than a shrug:

* **A fireball's five seconds of fire are not applied.** `SimMob` has no burning
  state at all — `SimMob::ignite` is the creeper fuse, a different mechanic sharing a
  verb. The `5.0` damage does land.
* **Players are not impact candidates.** `MobSim` knows player *positions*
  (`PlayerPerception`) and neither their entity ids nor their `PlayerVitals`, which
  live per-connection. Mob-on-player damage has no path anywhere in this workspace
  yet — melee included — so this is the pre-existing seam, not one introduced here.
* **A splash potion applies no effect.** There is no per-mob status-effect store for
  an area effect to land in; the effect model that exists is the player's.
* **Launch inaccuracy is not modelled.** Vanilla adds
  `random.triangle(0.0, 0.0172275 * uncertainty)` per axis, which needs
  `RandomSource.triangle`'s exact distribution *and* draw order. A deterministic
  launch is also what lets a gate predict a value rather than assert a direction.
* **Piercing, critical arrows and Punch knockback** are enchantment- or
  charge-derived, and there is no enchantment model.

## Configuration

None. Damage constants are `pub` in `lodestone_entity::projectile`
(`ARROW_BASE_DAMAGE`, `TRIDENT_BASE_DAMAGE`, `SMALL_FIREBALL_DAMAGE`,
`SNOWBALL_BLAZE_DAMAGE`, `BOW_ARROW_SPEED`, `THROWABLE_SHOOT_POWER`).

## Dependencies

`lodestone_entity::projectile` for the arithmetic, `lodestone_entity::damage` for the
reduction pipeline, `lodestone_data::damage_types` for the per-type bypass flags,
`ChunkWorld` for the terrain query. `crates/lodestone-server/tests/projectile_impact.rs`
is the acceptance gate, including the end-to-end skeleton case that drives nothing but
`MobSim::tick`.
