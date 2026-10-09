# Live mob simulation

## What it is

The server's living-mob tick pairs a stable terrain snapshot for navigation with a live per-block-state collision pass before entity snapshots stream to clients. Navigation stays deterministic while command, plugin, combat, leash, crowd and piston motion cannot cross a block changed after the snapshot.

## How it works

- `crate::tick::run_tick_loop` calls `MobSim::tick_with_terrain` with the live `ChunkSource` `StateId` reader. Navigation runs first on its bounded `ChunkWorld`; `LiveBlockCollision` resolves each state via `lodestone_data::collision_shapes`, and `settle_mob` sweeps the species' `MobShape` width, height and step height across real boxes. A `moving_piston` is the dynamic exception: its census shape is empty, but a live move treats its cell as a full cube until the move completes.
- The first pass accepts AI movement, records grounded state, cancels blocked components and starts a fall if a supporting block vanished. A final pass after combat, leash, crowd, warden and piston effects clips deferred impulses without a second gravity application. Teleports reset the collision origin; a ridden-mob report bypasses the sweep (the rider's client owns the position). A command or plugin spawn inside a live shape moves up to the highest overlapping shape before publishing.
- Each [dimension runtime](dimension-runtime.md) owns the `MobHandle` and publication used by its tick loop and visiting connections, and its collision reader comes from that dimension's source.
- `MobHandle` implements `EntitySource`; `EntityStreamer` consumes its `LiveMobSource` in the connection loop. `LiveMobSource` publishes snapshots with a revision under one lock; each connection captures and diffs only when the revision changes. Composed player-aware sources forward the capability, with mob/item and player last-sent state in separate maps (each entity once). Player roster, movement and metadata stay packet-driven and sampled every pass; removals from both scopes are batched before updates and spawns. Unversioned sources compare every pass. Boss bars are sampled independently so an unchanged publication cannot hide a changed bar.

### Terrain that is not loaded

`MobSim::tick_with_terrain` reads blocks through `mobs::TerrainRead`, which answers `None` for an unloaded column: absent is never air. A mob whose own column is absent skips navigation and collision (entities in non-ticking chunks do not tick), so a mob summoned over an unloaded column holds position and lands when the column arrives. Every other absent-cell read (a mob sweeping across a column edge, an item, orb, cart or bomb) sees an unbreakable full cube. The production oracle is `tick.rs`'s resident read, which never generates; `MobSim::tick` answers the same `None` for a column the sim does not hold. `mob_terrain_tests` checks it through the real tick loop on generated terrain.

Not covered: pathfinding, projectiles and fishing still read the bounded `ChunkWorld`, where an absent column is air (collision stays authoritative). Items, orbs and carts in an absent column do not fall but are not frozen like mobs.

## How to change it

- Keep pathfinding and collision separate. Any new movement that changes `NavigatingMob::position` must go through `apply_knockback` and the final sweep or call `settle_mob` after its phase. Never replace the shape adapter with a solid/air predicate: slabs, fences and plants are the discriminators.
- A pose that changes body size must update the shape source before the sweep. Add a test predicting an exact rest surface from a collision shape, plus a command-to-`EntitySource` test for a new spawn path. Relocation APIs must keep resetting the collision origin.
- When extending streaming, read publication identity and snapshots atomically and never feed an omitted, unchanged publication into a full removal diff. `player_view_changes_stream_without_a_mob_publication` holds one mob and one item through peer join, movement, metadata and leave and predicts one capture, then one update capture and one removal capture.

## Configuration

No runtime switch. Collision geometry is the generated 26.2 state census in `lodestone-data`. The initial-overlap escape is bounded at 512 steps (above the loaded vertical span) so malformed readers cannot make a tick unbounded.

`LODESTONE_ENTITY_PUBLICATION_PERF_ITERATIONS` (`1..=128`) enables a release-mode stress measurement when running `player_view_changes_stream_without_a_mob_publication` alone with `--nocapture`: 512 unchanged entities through real `stream_pass`, five alternating pairs, a versioned source against an equal unversioned one that recaptures each pass. It reports snapshot reads, time and instruction and cycle counts; it is a busy-connection control, not a frame-rate prediction. Ordinary runs skip it.

## Dependencies

`lodestone-server::mobs::{MobSim, MobHandle, LiveBlockCollision}`, `lodestone-entity::ai::NavigatingMob`, `lodestone-physics::{collision, EntityDimensions}`, `lodestone-data::{block_states, collision_shapes}`, `lodestone-server::{tick, server::EntityStreamer}`.
