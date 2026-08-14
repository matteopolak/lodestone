# Entity tick drivers: `ProjectileRegistry` and `ItemEntityRegistry`

## What it is

Two small per-tick driver types in `lodestone-entity` that own a *collection*
of already-correct, previously-unconsumed entity mechanics and advance all of
them through one call: [`ProjectileRegistry`](../crates/lodestone-entity/src/projectile.rs)
for ballistic motion (arrows, snowballs, ender pearls, …) and
[`ItemEntityRegistry`](../crates/lodestone-entity/src/item_entity.rs) for
dropped-item age/pickup-delay/merge. Both are now constructed and ticked in
production by [`MobSim`](../crates/lodestone-server/src/mobs/mod.rs) — see
"Production wiring" below; this doc's earlier revision described them as
unwired, which is no longer true and is corrected here rather than left to
mislead the next reader.

Both exist because `projectile.rs` and `item_entity.rs` were confirmed
islands: the per-entity math was correct and unit-tested,
but nothing outside their own test modules ever called it — no registry, no
driver, `grep -rn 'projectile::Projectile'` outside the crate was empty. The
registries are the missing "something owns many of these across ticks" piece.

## How it works

Both follow the same shape, deliberately mirroring
[`spawn::SpawnEnvironment`](../crates/lodestone-entity/src/spawn.rs)'s
world-free seam: the registry stores pure per-tick state and exposes a driver
method; anything that needs live world/entity data (hit detection, pickup
overlap, merge adjacency) stays the caller's job.

```text
ProjectileRegistry                     ItemEntityRegistry
  spawn(id, Projectile)                  spawn(id, ItemLifecycle)
  tick()  -> advances every entry        tick()  -> advances every entry,
             one server tick                        returns ids that crossed
                                                     DESPAWN_AGE (removed)
  set_in_water(id, bool)                 merge(to_id, from_id) -> bool
  remove(id) / get(id) / iter()          remove(id) / get(id) / iter()
```

Entity ids are plain `i32`, matching `SimMob`'s numbering convention in
`lodestone-server` (`crates/lodestone-server/src/mobs/mod.rs`) so a caller can key
both off the same network entity id space.

`ItemEntityRegistry::merge` also carries a correctness fix found while
re-verifying `try_merge` against `ItemEntity`'s `merge(ItemEntity, ItemStack, ItemEntity, ItemStack)` overload: only the
surviving `to` side picks up state from the merge — `pickup_delay` becomes
`max(to, from)` and **`age` becomes `min(to, from)`**, resetting the survivor
to the younger of the two ages. The pre-existing `try_merge` neither touched
`age` nor limited the `pickup_delay` write to `to` alone; both are fixed.

## Production wiring

A prior fix closed the "is anything ticking `MobSim` at all" gap first (see
`docs/live-mob-sim.md`); this closed the next one out. `MobSim`
(`crates/lodestone-server/src/mobs/mod.rs`) now owns one `ProjectileRegistry` and
one `ItemEntityRegistry` as fields (plus a small `HashMap` each of wire
metadata — uuid and canonical entity-type key — since both registries stay
deliberately version/wire-free, the same split `SimMob` already makes for
mobs). `MobSim::tick()` calls `self.projectiles.tick()` and
`self.items.tick()` every server tick, so the server's unified tick loop
(`tick::run_tick_loop` — before that, `run_mob_tick_loop`) — the
same background task `IntegratedServer::open_in_memory_with_mobs` spawns for
singleplayer — advances both automatically, with no new task.

```text
MobSim::spawn_projectile(entity_type, Projectile) -> id   // launch
MobSim::spawn_item(item, position, velocity, ItemLifecycle) -> id  // drop
MobSim::tick()                                             // every server tick:
  self.projectiles.tick();
  for id in self.items.tick() { self.item_state.remove(&id); }  // despawn
  for state in self.item_state.values_mut() { state.motion.tick(); }
MobSim::snapshots() -> Vec<EntitySnapshot>                 // mobs + projectiles + items
MobSim::remove_projectile(id) / remove_item(id)             // impact / pickup
```

`tick::run_tick_loop` (previously `run_mob_tick_loop`) publishes
`sim.snapshots()` (not just `sim.iter().map(SimMob::snapshot)`)
to `LiveMobSource`, which is the part that actually gets a projectile or
dropped item onto the same `add_entity`/`move_entity`/`remove_entity` wire
path mobs already proved reaches a real client — ticking the registries
without this would still be an island that ticks correctly and reaches zero
pixels. See `crates/lodestone-server/tests/projectile_and_item_registries.rs`
for the acceptance gate: it drives `MobSim::spawn_projectile`/`spawn_item` +
`MobSim::tick_for`, never `ProjectileRegistry::tick`/`ItemEntityRegistry::tick`
directly, and asserts exact predicted positions/counters computed independently
from the real 26.2 jar (`.cache/mc/26.2/src`), not a direction-only "it moved".

**Still explicit scope cuts, not silent gaps**: hit detection (arrow vs.
terrain/entity), the resulting damage/area-effect, and pickup-on-overlap are
*not* done by this wiring — `spawn_projectile`/`spawn_item`'s own doc
comments say so. Nothing in `lodestone-server` yet calls `spawn_projectile`/
`spawn_item` from a real gameplay action (bow-firing, item drop) either —
that "action boundary" call is follow-up work once those packets exist; this
closed the "nothing constructs or ticks the registry" island specifically.

## How to change it, and the gotchas

- **The registries are version/wire-free by design; `MobSim` supplies the
  rest.** Do not add a `uuid`/`entity_type` field to `TrackedProjectile`/
  `TrackedItem` themselves — that would break the seam
  `spawn::SpawnEnvironment` and `SimMob` both already use. Keep wire identity
  in `MobSim`'s own `ProjectileMeta`/`ItemState` maps instead.
- **A despawned/removed entry must be dropped from *both* the registry and
  its metadata map**, or `snapshots()` — which only reads the registry, not
  the map — will simply omit it (harmless) while the metadata map leaks. See
  `MobSim::tick`'s `for despawned_item_id in self.items.tick() {
  self.item_state.remove(...) }` and `remove_projectile`/`remove_item` for the
  pattern.
- **Hit detection, pickup-on-overlap and merge-adjacency are NOT here.** This
  crate deliberately has no world handle (see the module doc on
  [`spawn::SpawnEnvironment`](../crates/lodestone-entity/src/spawn.rs)), so a
  registry's `tick()` only ever advances motion/counters. The caller finds
  collisions/overlaps/adjacent pairs and calls back in
  (`ProjectileRegistry::remove` on impact, `ItemEntityRegistry::merge` for a
  pair it already knows are close).
- **The despawn/merge tests exercise the driver method, not the underlying
  pure function.** `item_entity::tests::registry_tick_despawns_only_the_item_that_reaches_despawn_age`
  and `projectile::tests::registry_tick_advances_every_tracked_projectile_through_one_call`
  call `.tick()` on the registry, never `ItemLifecycle::tick`/`Projectile::tick`
  directly — a test that only calls the pure function proves nothing about
  whether a driver exists, which is exactly how these two survived as islands
  in the first place.

## Configuration

None — both types are plain in-memory collections with no feature flags or
constants of their own (the vanilla constants they operate on,
`DESPAWN_AGE`/`INFINITE_LIFETIME_AGE`/`NEVER_PICKUP_DELAY` and the
gravity/drag profiles, are documented on `item_entity.rs` and `projectile.rs`
directly).

## Dependencies

- `lodestone_model::Vec3` for position/velocity.
- No dependency on `lodestone-world`, `lodestone-physics`, or
  `lodestone-server` — that is the point of the seam.
