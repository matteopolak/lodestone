# Projectiles

## What it is

Everything that leaves an entity's hand and travels under its own motion on the integrated server and
client: thrown-item ballistics and impact resolution (snowballs, eggs, ender pearls, potions, fireballs,
arrows), the fishing rod's bobber, the riptide trident and elytra firework boost impulses, primed TNT,
and the block-destroying half of an explosion.

## How it works

### Ballistics and impact

Projectile motion itself (gravity, drag, arrow/trident flight) lives in `lodestone_physics`/
`lodestone_entity`; the interesting part is impact resolution, in `lodestone_entity::projectile` (pure,
version-free arithmetic — exact AABB-slab clipping for terrain, a growing hitbox margin for entities,
the bow draw-time-to-power curve, launch velocity from rotation) and `MobSim::resolve_projectile_impacts`
(the per-tick search, run **before** the projectile motion tick so a hit is detected on the segment about
to be travelled rather than one tick late). Terrain is sampled along the segment at quarter-block spacing;
entities are tested with an exact clip against an inflated box rather than sampled, because a fast arrow
can otherwise step clean over a narrow target. Whichever hit is nearer wins; on an entity hit, damage is
`ceil(clamp(speed * baseDamage, 0, max))` — ceiling, not truncation, so a spent arrow at low speed still
deals at least 1 — and a plain arrow's knockback is genuinely zero unless enchanted with Punch.

The player's own launch (drawing and releasing a bow, throwing a snowball/egg/pearl/potion) is a
serverbound `UseItem`/`ReleaseUseItem` pair; a throwable releases immediately and a bow starts a tick-count
based draw (never a wall-clock duration, since this crate also targets wasm32 where clock APIs trap).

A potion's raw item-component registry number is validated once as `lodestone_data::potion::PotionId` when
the server launches it. `ProjectileMeta`, the impact staging record, and `mob_effects::potion_splash_effects`
then retain that type, so the built-in effect-table lookup is total. Absent, extension, and out-of-range
component values remain `None` at the launch/consume boundary and produce no built-in effect rather than
being mistaken for a valid potion.

### Fishing

The fishing rod is ported as a bobber entity with its own cast/bob/nibble/bite state machine
(`MobSim::cast_fishing_bobber`/`tick_fishing_bobbers`/`retrieve_fishing_bobber`), real per-tick physics,
and vanilla's exact three-pool loot table (fish/junk/treasure) with Luck of the Sea shifting weight
toward treasure via the same integer weight formula every loot table uses. A caught fish or item reels in
as a real item entity plus a real experience orb, reusing the sim's existing item/orb producers rather
than a bespoke reward path.

### Riptide and the elytra firework boost

Two item-driven velocity impulses, each split between pure arithmetic (in `lodestone-physics`) and a
trigger (item-use state, held duration, wet/gliding gates, enchantment level — none of which is physics
state, so it lives in the shell/ECS layer instead). Riptide launches the player along their look vector
at a magnitude derived from the trident's Riptide level and starts a spin-attack pose/state; it requires
having held the trident at least 10 ticks while in water or rain. The elytra boost nudges the player's
velocity toward their look vector by a fixed amount every tick a firework rocket is attached while
gliding. Both are client-predicted, matching vanilla's own client-side prediction for both effects — a
server-authoritative version would feel wrong even where it's possible.

### Primed TNT

The `minecraft:tnt` entity is a lightweight, non-AI sidecar (gravity, a shared collision integrator, a
characteristic bounce-and-friction multiply on landing, then a fuse countdown) that on expiry feeds the
same detonation pipeline a creeper's fuse already drives — entity damage/knockback plus the block-removal
pass below. It can be ignited by flint and steel/fire charge on a TNT block, a redstone signal, fire
consuming a neighbouring TNT block, a dispenser, or chain-reaction from another explosion; all producers
funnel through one constructor so the random launch direction is always drawn from one isolated RNG
stream.

### Explosion block destruction

An explosion's block-removal half is a ray-marched sample: 1352 rays evenly covering the surface of a
16x16x16 grid around the blast centre, each starting with a randomized power and marching outward in
fixed steps, subtracting a per-cell cost derived from that block's blast resistance (even a
zero-resistance block costs more per step than true air) until the ray's power runs out; every cell a ray
still had positive power for when it entered joins the destroyed set. This reproduces vanilla's real
crater shape and resistance behavior exactly — a creeper's blast can never destroy obsidian, and a solid
stone room only ever loses the six Chebyshev-adjacent cells to a centred blast — because both are direct
consequences of the same per-step arithmetic. Per-block-type blast resistance is a generated table pulled
from a real headless server (`blocks.json` has no such field), keyed to a flat per-state array for the
hot ray-march loop. That array accepts `lodestone_data::block_states::StateId`; the world-read boundary
validates its raw palette id once, while `None` remains reserved for a valid air state with no fluid.

Entity exposure, damage and knockback are a separate, older pipeline that already existed; this only adds
the "does a block actually disappear" half. Loot drops from destroyed blocks, block entities, and
explosion-triggered fire are each separate, narrower pieces layered on top rather than part of the ray
march itself.

## How to change it, and the gotchas

- **Vanilla's own sin/cos helpers are a quantized lookup table, not `f32::sin`/`f32::cos`** — substituting the
  standard library diverges exactly at the poles. Any ballistics code porting a vanilla trig call
  (bobber launch angle, riptide direction, thrown-item velocity) must use `lodestone_physics::mth`, never
  `f32::sin`/`f32::cos`; a fixture at a cardinal angle or zero-crossing is exactly the input that exposes
  a standard-library substitution, a mid-range angle will not.
- **A per-tick "am I on the ground / in water / inside a block" check must scan every integer cell the
  entity's movement crossed during the tick, not just sample the post-move position** — sampling only the
  destination can tunnel through thin geometry at high speed. Found on the fishing bobber's settling code:
  a fast fall could cross a one-block floor within a tick and land on neither side of a single-cell
  sample, dropping into the void.
- The impact search must stay before the motion tick — swapping the order produces plausible-looking
  behavior and lets projectiles pass through walls.
- A projectile carries the launching entity's id so it does not immediately hit its own shooter (it
  spawns inside the shooter's own bounding box); a zero hitbox margin for the first couple of ticks is the
  complementary guard.
- Rules that depend on the *target's* species (a snowball dealing extra damage to a blaze, for instance)
  cannot live in the pure per-projectile damage function — they belong beside the impact search that has
  the target entity in scope.
- Every ignition producer for TNT and every ray-march cell read must go through the same bounds-checked
  world accessor — an explosion on the world floor marches downward past the world's minimum height on its
  first steps, and an unguarded index there panics the tick thread.
- The block-explosion ray count, step size, and entity-exposure sampling are physics, not performance
  tunables — do not approximate them to make a blast cheaper.
- Vanilla's block-destruction shuffle before drop rolls consumes RNG draws in Java hash-iteration order,
  which is not reproducible outside a JVM — a from-scratch port can match the multiset of dropped items
  and each item's own loot roll, never the exact emission sequence.
- An entity data index is reused across unrelated vanilla classes (the same numeric index backs an
  experience orb's value, TNT's fuse, a fishing hook's target reference, a vehicle's hurt state, and a
  display entity's interpolation delay) — the *producer*, not a shared census helper, is what has to
  disambiguate which one a given entity type is sending.

## Configuration

None of these systems expose runtime configuration; every number (fuse times, bounce/drag constants,
riptide strength per enchantment level, blast resistance per block, ray count/step size) is a fixed
vanilla constant or a generated data table. The one game-facing toggle is the `tnt_explodes` game rule,
which every TNT ignition producer checks before priming (except the direct-ignition arm, which has a
narrower, documented gap).

## Dependencies

- `lodestone_entity::projectile`/`::damage` — ballistics and damage-reduction arithmetic.
- `lodestone_data::damage_types`/`block_blast`/`entity_dimensions`/`entity_types` — per-type damage
  bypass flags, blast resistance, and entity registry data, all generated from a real headless server
  rather than transcribed from `blocks.json`.
- `lodestone-physics` — trident/firework impulse arithmetic, the shared `move_entity` collision
  integrator TNT and vehicles both use, `lodestone_physics::mth`'s quantized trig table.
- `crate::explosion_blocks`/`crate::block_drops` — the block-removal and loot pipeline every detonation
  producer (creeper, TNT, chain reaction) shares.
- `crate::fluid::fluid_state_of` — water/source detection for fishing's open-water and bobbing checks.
- `crate::mob_spawn::SpawnRng` — the isolated RNG streams for TNT launch direction and fishing's loot
  roll.
