# Entity presentation modules

## What it is

The shell-side entity presentation code turns network-backed ECS tracks into
render-ready entity draws. It is split into cohesive modules while retaining
the existing `crate::entities::*` API.

## How it works

`entities/mod.rs` owns ingest folding, track lifecycle, and the shared wiring.
`remote_body.rs` owns the client-side body-yaw state and tick system for remote
players. `interpolation.rs` advances frame clocks, eases poses, and builds
animation inputs. `physics.rs` integrates locally simulated dropped items and
projectiles against the shared collision/profile inputs. `extraction.rs`
converts ECS state to `EntityDraw` values and drives pickup flights.
`render_input.rs` defines `EntityDraw`, the plain render-facing POD consumed by
the GPU preparation paths.

The parent module re-exports the moved public types and systems, so downstream
callers do not need to know the file layout. Internal helpers are exposed only
to the parent and its tests. The extraction schedule remains ordered after
interpolation and before pickup append, preserving the existing frame flow.

Stepped network movement preserves each waypoint's duration. Ingest resolves
relative steps sequentially, publishes the final endpoint in `Position` for
headless callers, and retains ordered `EntityMovementPath` batches for the
presentation fold. The fold drains those batches once and queues their targets
in `InterpClock`; a packet arriving during an active path appends its waypoints.
Non-positive durations advance immediately, and each batch interpolates its
rotation from the preceding queued target over its own total duration. Ordinary
movement after a stepped path contributes a three-tick segment. Entities that
have only received ordinary movement keep their existing three-tick ease.
The presentation registration installs `EntityMovementPathRetention`; a
headless world without that resource updates semantic endpoints without
retaining a render queue.

`render_feet`, `render_yaw`, and `render_pitch` sample the same timed path for
draw extraction, walk animation, pickup anchors, and remote vehicle seats.
Dropped items and locally simulated projectiles retain their local physics;
their authoritative corrections use the final reported endpoint. Locally
controlled vehicles retain fixed-tick pose sampling with the shared frame
residual. No second timer is created for path movement.

## How to change it

Keep network easing and local physics independent: a change to one should not
change the other entity families. Add fields to `EntityDraw` and update every
constructor in `extraction.rs` (including synthetic pickup draws). When moving
helpers between modules, preserve the parent re-export if tests or another
shell subsystem uses the old path. Keep GPU-free ECS logic in these modules;
renderer-specific batching belongs in `crates/lodestone-shell/src/gpu/` or
`lodestone-render`.

When extending movement, retain batch boundaries as well as waypoints: a later
rotation target must not change the angles of already queued segments. Reset
the ingest path component on a teleport correction. A path with identical
first and final positions can still travel between them, so endpoint equality
must not suppress a newly received path.

## Configuration

The `EntityInterpPlugin` schedule registration and the `FrameDelta`,
`ItemCollision`, and `Profile` resources control execution. No new
environment variables or feature flags were added.

## Dependencies

The modules depend on `lodestone-ecs` for schedules and components,
`lodestone-physics` for collision and profiles, `lodestone-entity` for local
motion rules, and `lodestone-render`/`lodestone-model` for render input types.
The split does not change those dependency boundaries.
