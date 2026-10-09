# Projectiles

## What it is

Everything that leaves an entity's hand and travels under its own motion, on the integrated server and client: thrown-item ballistics and impact (snowballs, eggs, ender pearls, potions, fireballs, arrows), the fishing bobber, riptide and elytra firework impulses, primed TNT, and the block-destroying half of an explosion.

## How it works

### Ballistics and impact

Motion (gravity, drag, flight) lives in `lodestone_physics`/`lodestone_entity`. Impact resolution is `lodestone_entity::projectile` (pure arithmetic: exact AABB-slab clipping for terrain, a growing hitbox margin for entities, the bow draw-to-power curve, launch velocity from rotation) and `MobSim::resolve_projectile_impacts`, run before the motion tick so a hit is found on the segment about to be travelled. Terrain is sampled at quarter-block spacing; entities are clipped exactly against an inflated box (sampling lets a fast arrow skip a narrow target). The nearer hit wins. Entity damage is `ceil(clamp(speed * baseDamage, 0, max))` (ceiling, so a spent arrow still deals at least 1); a plain arrow's knockback is zero without Punch.

A player launch is a serverbound `UseItem`/`ReleaseUseItem` pair: throwables release immediately, a bow starts a tick-count draw (never wall-clock, since wasm32 clock APIs trap). A potion's raw component number is validated once as `lodestone_data::potion::PotionId` at launch; `ProjectileMeta`, the impact staging record and `mob_effects::potion_splash_effects` keep the type, so the lookup is total, and absent, extension or out-of-range values give no built-in effect.

### Stuck arrows, pickup and saving

An arrow, spectral arrow or trident that hits a block freezes (`Projectile::frozen`, zero velocity). `MobSim::tick_stuck_arrows` counts its 7-tick rattle (`shake`) down and `life` up, despawns at 1200 (a reclaimable trident never) and releases it if its block is gone. Other thrown projectiles are destroyed by blocks.

`ArrowPickup` (saved as the `pickup` byte 0/1/2) decides who may take an embedded arrow: `Disallowed` for mob shots, `Allowed` for a survival player's (returns the arrow item), `CreativeOnly` for a creative player's (cleared by a creative player, returns nothing). A rattling or flying arrow is never takeable. `server::pickups::collect_nearby_items` runs the rule each tick, banks the item before removing the entity, and leaves the arrow when the inventory is full.

Saving (`MobSim::saved_projectiles`, `restore_projectile`) uses the save-format keys `Owner`, `LeftOwner`, `HasBeenShot` on all; `life`, `inBlockState`, `shake`, `inGround`, `pickup`, `damage`, `crit`, `PierceLevel`, `item` on the arrow family; `DealtDamage` on a trident; `Item` (with `minecraft:potion_contents` for potions) on a thrown item. The shooter is stored by UUID and re-resolved after the batch loads. The native store carries these through `NativeEntityState::fields` ([entity persistence](entity-persistence.md)), and `SavedEntity::from_nbt` keeps `Item` in `extra` for anything but `minecraft:item` (a projectile's stack has components the typed field drops). Not modelled: tipped-arrow potion contents, `weapon`, `SoundEvent`. A loaded-chunk check keeps arrows in unloaded columns from being released.

### Fishing

A bobber entity with its own cast, bob, nibble and bite state machine (`MobSim::cast_fishing_bobber`/`tick_fishing_bobbers`/`retrieve_fishing_bobber`), per-tick physics and a roll of the bundled `gameplay/fishing` loot table. The roll context carries combined luck (rod plus player), the biome under the bobber and its open-water state (read by the quality weights, jungle-only bamboo and open-water-only treasure), and table functions damage and enchant treasure. A catch reels in as a real item entity plus experience orb through the sim's existing producers.

With at least 2,048 live bobbers, ticking uses a two-stage owner plan: entity-id order first consumes the isolated random stream into immutable per-bobber decisions, bounded native owner lanes then do the water scan and physics from cloned tick-start state, and the central apply step validates plan generation, owner coverage and serial slots before writing the live map. Browser and smaller sims stay serial. `measure_dense_fishing_owner_workers` is the evidence for the cutoff; parity tests cover interleaved owners, a negative chunk coordinate and bite, lure and reset transitions.

### Riptide and firework boost

Two item-driven impulses, arithmetic in `lodestone-physics` and triggers (use state, held duration, wet and gliding gates, enchantment level) in the shell/ECS layer. Riptide launches along the look vector at a magnitude from the Riptide level and starts a spin-attack pose; it needs the trident held at least 10 ticks in water or rain. The boost nudges velocity toward the look vector each tick a rocket is attached while gliding. Both are client-predicted, as in the reference.

### Primed TNT

A lightweight non-AI sidecar (gravity, the shared collision integrator, a bounce-and-friction multiply on landing, then a fuse) that on expiry feeds the detonation pipeline a creeper's fuse uses (entity damage and knockback plus block removal). Ignition producers (flint and steel or fire charge, redstone, fire consuming a neighbour, a dispenser, chain reaction) all go through one constructor so the random launch direction comes from one isolated RNG stream.

### Explosion block destruction

1352 rays evenly cover the surface of a 16x16x16 grid around the centre, each starting at a randomised power and marching in fixed steps, subtracting a per-cell cost from blast resistance (even zero-resistance blocks cost more per step than air) until power runs out; every cell with positive power on entry joins the destroyed set. This reproduces the reference crater and resistance behaviour (a creeper cannot destroy obsidian; a solid stone room loses only the six adjacent cells to a centred blast). Blast resistance is a generated table from a real headless server (`blocks.json` has none) in a flat per-state array for the hot loop, accepting `lodestone_data::block_states::StateId`; the world-read boundary validates the raw palette id once, and `None` means a valid air state with no fluid.

Entity exposure, damage and knockback are an older, separate pipeline; loot drops, block entities and explosion fire are narrower layers on top.

## How to change it

- The reference trig helpers are a quantised lookup table, not `f32::sin`/`cos`; standard-library substitution diverges exactly at the poles. Ballistics porting a trig call (bobber angle, riptide direction, thrown velocity) uses `lodestone_physics::mth`, with a fixture at a cardinal angle or zero crossing (mid-range angles hide it).
- A per-tick ground, water or inside-block check must scan every integer cell the movement crossed, not only the destination (a fast fall tunnelled through a one-block floor in the bobber's settling code).
- Keep the impact search before the motion tick (swapping looks fine and lets projectiles pass through walls).
- A projectile carries its launcher's id so it does not hit its shooter on spawn (it starts inside the shooter's box); a zero hitbox margin for the first couple of ticks is the complementary guard.
- Rules depending on the target's species (extra snowball damage to a blaze) belong beside the impact search, not in the pure damage function.
- TNT ignition producers and every ray-march read go through the same bounds-checked world accessor (a floor explosion marches below minimum height and an unguarded index panics the tick thread).
- Ray count, step size and entity-exposure sampling are physics, not tunables.
- The block-destruction shuffle before drop rolls consumes draws in Java hash-iteration order, not reproducible off the JVM: a port can match the multiset of drops and each loot roll, not the emission sequence.
- One entity data index serves unrelated classes (experience orb value, TNT fuse, fishing hook target, vehicle hurt state, display interpolation delay), so the producer must disambiguate, not a shared census helper.

## Configuration

No runtime configuration; fuse times, bounce and drag constants, riptide strength, blast resistance and ray parameters are fixed reference values or generated tables. The one toggle is the `tnt_explodes` game rule, checked by every ignition producer (the direct-ignition arm has a narrower, documented gap).

## Dependencies

`lodestone_entity::{projectile, damage}`; `lodestone_data::{damage_types, block_blast, entity_dimensions, entity_types}` (generated from a real server); `lodestone-physics` (impulses, the shared `move_entity` integrator, `mth` trig); `crate::explosion_blocks`/`crate::block_drops`; `crate::fluid::fluid_state_of`; `crate::mob_spawn::SpawnRng` (isolated streams for TNT direction and fishing loot).
