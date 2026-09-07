# Live mob simulation

## What it is

The server's living-mob tick combines a stable terrain snapshot for navigation with a live
per-block-state collision pass before entity snapshots are streamed to clients. This keeps a
mob's AI search deterministic while ensuring command, plugin, combat, leash, crowd, and piston
motion cannot cross a block that changed after that search snapshot was made.

## How it works

`MobHandle` owns `MobSim`, and `crate::tick::run_tick_loop` calls
`MobSim::tick_with_terrain` with the live `ChunkSource` state-name reader. Navigation runs first
against its bounded `ChunkWorld`; `LiveBlockCollision` resolves each queried state through
`lodestone_data::collision_shapes`, then `settle_mob` sweeps the species' `MobShape` width,
height, and step height across those real world-space boxes. A `moving_piston` is the narrow
dynamic-shape exception: its static census shape is empty, while a live server move treats its
cell as a full cube until that move completes.

The first pass accepts AI movement, records grounded state, cancels blocked velocity components,
and starts a fall when a formerly supporting live block was removed. A final pass after combat,
leash, crowd, warden, and piston effects clips their deferred impulses too, without applying
gravity a second time that tick. Instant teleports and ridden-mob reports deliberately reset the
collision origin: they are authority boundaries rather than physical motion. A command or plugin
spawn that begins inside a live shape is moved upward to the highest overlapping shape before its
next movement is published.

`MobHandle` implements `EntitySource`; `EntityStreamer` consumes its snapshots in the connection
loop. The command integration test therefore exercises summon, live ticking, and the snapshot
surface that streaming diffs, rather than inspecting a private simulation record.

## How to change it

Keep pathfinding and collision separate. `ChunkWorld` may remain a stable, bounded navigation
view, but any new `SimMob` movement that changes `NavigatingMob::position` must either enter
through `apply_knockback` and the final sweep or call `settle_mob` after its own phase. Do not
replace the shape adapter with a solid/air predicate: slabs, fences, and non-colliding plants are
required discriminators.

If a new pose changes body dimensions, update the shape source before the sweep so collision uses
the pose's actual bounds. Add a test that predicts an exact rest surface from a collision shape,
plus a command-to-`EntitySource` test when adding a new spawn path. Intentional relocation APIs
must continue to reset the collision origin rather than being silently converted into physical
motion.

## Configuration

There is no runtime switch. Collision geometry is the generated 26.2 state census in
`lodestone-data`; changing that generated data changes every live collision consumer. The
initial-overlap escape has a fixed 512-step safety bound, larger than the supported loaded
vertical span, to prevent malformed block-state readers from making a tick unbounded.

## Dependencies

- `lodestone-server::mobs::{MobSim, MobHandle, LiveBlockCollision}` supplies simulation and the
  live state adapter.
- `lodestone-entity::ai::NavigatingMob` supplies AI motion, body shape, and collision-origin
  bookkeeping.
- `lodestone-physics::{collision, EntityDimensions}` supplies swept AABB resolution.
- `lodestone-data::{block_states, collision_shapes}` supplies per-state collision boxes.
- `lodestone-server::{tick, server::EntityStreamer}` supplies the live source and packet-facing
  snapshot consumer.
