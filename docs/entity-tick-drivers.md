# Entity tick drivers: `ProjectileRegistry` and `ItemEntityRegistry`

## What it is

Two small per-tick driver types in `lodestone-entity` that own a *collection*
of already-correct, previously-unconsumed entity mechanics and advance all of
them through one call: [`ProjectileRegistry`](../crates/lodestone-entity/src/projectile.rs)
for ballistic motion (arrows, snowballs, ender pearls, …) and
[`ItemEntityRegistry`](../crates/lodestone-entity/src/item_entity.rs) for
dropped-item age/pickup-delay/merge.

Both exist because `projectile.rs` and `item_entity.rs` were confirmed
islands (tracker #211, #215): the per-entity math was correct and unit-tested,
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
`lodestone-server` (`crates/lodestone-server/src/mobs.rs`) so a caller can key
both off the same network entity id space.

`ItemEntityRegistry::merge` also carries a correctness fix found while
re-verifying `try_merge` against `ItemEntity.java:261-267`: only the
surviving `to` side picks up state from the merge — `pickup_delay` becomes
`max(to, from)` and **`age` becomes `min(to, from)`**, resetting the survivor
to the younger of the two ages. The pre-existing `try_merge` neither touched
`age` nor limited the `pickup_delay` write to `to` alone; both are fixed.

## How to change it, and the gotchas

- **Neither registry is instantiated in production yet.** `lodestone-server`
  has no live per-tick loop that drives `MobSim` either — `IntegratedServer`'s
  singleplayer path calls `open_in_memory` (no entities), not
  `open_in_memory_with_entities`, so `MobSim::tick()` itself currently has
  zero production callers. That gap is tracked separately (#217, framed there
  as "no encoder wiring exists" for streaming positions to a client — the
  more fundamental fact is there is no tick loop driving entity sim at all
  yet). Once that loop exists, wiring these two registries into it is small:
  a field each, a `.tick()` call alongside `MobSim::tick()`, and a
  spawn/remove call at the appropriate packet/action boundary.
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
