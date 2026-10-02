# Live mob simulation

## What it is

The server's living-mob tick combines a stable terrain snapshot for navigation with a live
per-block-state collision pass before entity snapshots are streamed to clients. This keeps a
mob's AI search deterministic while ensuring command, plugin, combat, leash, crowd, and piston
motion cannot cross a block that changed after that search snapshot was made.

## How it works

`MobHandle` owns `MobSim`, and `crate::tick::run_tick_loop` calls
`MobSim::tick_with_terrain` with the live `ChunkSource` `StateId` reader. Navigation runs first
against its bounded `ChunkWorld`; `LiveBlockCollision` resolves each queried state through
`lodestone_data::collision_shapes`, then `settle_mob` sweeps the species' `MobShape` width,
height, and step height across those real world-space boxes. A `moving_piston` is the narrow
dynamic-shape exception: its static census shape is empty, while a live server move treats its
cell as a full cube until that move completes.

Each [dimension runtime](dimension-runtime.md) owns the mob handle and publication used by
its tick loop and by connections currently visiting that dimension. The live collision reader
comes from that same dimension's authoritative terrain source. Sibling navigation snapshots
start empty; rolling resident navigation and sibling entity persistence remain separate work.

The first pass accepts AI movement, records grounded state, cancels blocked velocity components,
and starts a fall when a formerly supporting live block was removed. A final pass after combat,
leash, crowd, warden, and piston effects clips their deferred impulses too, without applying
gravity a second time that tick. Instant teleports reset the collision origin, while a ridden-mob
report bypasses the sweep entirely because the rider's client owns that position. A command or
plugin spawn that begins inside a live shape is moved upward to the highest overlapping shape
before its next movement is published.

`MobHandle` implements `EntitySource`; production `EntityStreamer` consumes its runtime's
`LiveMobSource` publication in the connection loop. The command integration test exercises summon, live ticking, and the snapshot
surface that streaming diffs, rather than inspecting a private simulation record.

`LiveMobSource` publishes entity snapshots with a revision under one lock. Each connection
captures and diffs that publication only when its revision changes. Composed player-aware
sources forward this capability; the streamer keeps mob/item and player last-sent state in
separate maps, with each entity retained once. Player roster, movement and metadata remain
packet-driven and are sampled independently on every stream pass. Both scopes' removals are
batched before updates and spawns. Sources without publication revisions still compare every
pass. Boss bars are also sampled independently and forwarded by the composed source, so an
unchanged entity publication cannot suppress a changed bar.

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

Keep publication identity and snapshots from the same atomic read when extending streaming.
Do not feed an omitted, unchanged mob publication into a full removal diff. The focused
`player_view_changes_stream_without_a_mob_publication` control holds one mob and one item
through peer join, movement, metadata and leave; it predicts one capture until the next
publication, then exactly one update capture and one removal capture.

## Configuration

There is no runtime switch. Collision geometry is the generated 26.2 state census in
`lodestone-data`; changing that generated data changes every live collision consumer. The
initial-overlap escape has a fixed 512-step safety bound, larger than the supported loaded
vertical span, to prevent malformed block-state readers from making a tick unbounded.

Set `LODESTONE_ENTITY_PUBLICATION_PERF_ITERATIONS` to `1..=128` when running
`player_view_changes_stream_without_a_mob_publication` alone in release mode with
`--nocapture`. It compares 512 unchanged entities through actual `stream_pass`, using five
alternating pairs. One composed source advertises publication revisions; an otherwise equal
unversioned source deliberately recaptures on each pass. Initial spawns are outside measurement.
The report includes snapshot reads, elapsed time and supported process instruction/cycle counts.
This is a bounded busy-connection stress control, not a historical binary comparison or a
prediction of normal-world frame rate. Ordinary test runs skip the measurement.

## Dependencies

- `lodestone-server::mobs::{MobSim, MobHandle, LiveBlockCollision}` supplies simulation and the
  live state adapter.
- `lodestone-entity::ai::NavigatingMob` supplies AI motion, body shape, and collision-origin
  bookkeeping.
- `lodestone-physics::{collision, EntityDimensions}` supplies swept AABB resolution.
- `lodestone-data::{block_states, collision_shapes}` supplies per-state collision boxes.
- `lodestone-server::{tick, server::EntityStreamer}` supplies the live source and packet-facing
  snapshot consumer.
