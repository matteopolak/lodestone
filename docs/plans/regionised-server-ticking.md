# Plan: regionised server ticking

## What it is

A plan for splitting the server's single-threaded world tick into independently ticked regions (Folia's model: groups of nearby chunks, each on its own thread, with explicit hand-off across boundaries). Only one slice is built: dense entity pushing runs on a bounded chunk-owner executor. The rest is open work, to be chosen by profiling named scenes.

## How it works

### What is built

- Above 128 mobs on native builds, `MobSim` snapshots positions, widths, players and ids, drops mutable access, and runs source-chunk jobs on at most four worker lanes. A central writer validates the complete owner set and restores serial slots before changing velocities. Browser builds and smaller scenes stay serial.
- Parity: one-lane and four-lane results are identical for an interleaved 160-entity scene, matching the original pair-at-a-time algorithm.
- Measured by the ignored `measure_dense_entity_push_region_workers`: 2,048 mobs across four owners, 75.5 ms serial vs 57.1 ms parallel (1.32x). A scene-bound number, not a constant.
- Ownership scaffolding is documented in [tick region ownership](../tick-region-ownership.md) and [entity ownership transfer](../entity-ownership-transfer.md).
- `lodestone_server::lock_order` asserts a lock acquisition order in debug and test builds for the callback-held handles. Order: scheduled queues, staged ticks, block entities, mobs, border, game rules, world state, access lists. A new handle that calls another while locked must get a class there first.

### State partitioning

There is no single world lock: `lodestone_world::World` is an unlocked map, and the server holds many independent handles. The natural partition axis is `ChunkStore`'s resident-column set.

| state | region model |
|---|---|
| Resident chunks, scheduled ticks, block entities | partition by chunk |
| World border, game rules | stay global |
| Time, weather, difficulty | global by default; Folia regionises some, so decide deliberately |
| Mobs crossing a boundary | need a per-region-pair hand-off queue |

The primary tick loop owns a server ECS world, but it is a scheduling shell; mobs, block entities and scheduled ticks still live behind the handles above. Whether to use one ECS world per region or one partitioned world should wait until a real simulation concern has moved through `GameTick`.

## Open work

- Profile each phase on a named scene (idle, one explorer, clustered players, redstone-heavy) before migrating it. Dominant cost is scene-dependent; the idle-world floor says nothing about a populated world.
- Cross-region hand-off for fire, redstone, fluids, falling blocks, vehicles, TNT blasts and entities crossing edges.
- Scheduled-tick and block-entity partitioning.
- Durable asynchronous entity hand-off for kinds beyond dropped items.
- A populated-world server-tick benchmark. `crates/lodestone-server/benches/server_tick.rs` drives the real loop but uses a paused clock, so its phase times are a wiring control, not a profile.

## Gotchas

- The scheduled-tick mutex is held across most of the tick body. A past self-deadlock came from a re-entrant call through it; N region locks enlarge that hazard class.
- The dragon fight is a global singleton and needs a special case.
- Regions help spread-out load only. Clustered players in one region stay single-thread bound; profile a clustered scene before committing.

## Dependencies

[`tick-scheduling.md`](../tick-scheduling.md) for the profiling instruments, and [`server-ecs-migration.md`](server-ecs-migration.md) for the ECS substrate.
