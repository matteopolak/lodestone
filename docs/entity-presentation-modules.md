# Entity presentation modules

## What it is

The shell-side entity presentation code turns network-backed ECS tracks into
render-ready entity draws. It is split into cohesive modules while retaining
the existing `crate::entities::*` API.

## How it works

`entities/mod.rs` owns ingest folding, track lifecycle, shared components and
resources. `interpolation.rs` advances frame clocks, eases poses, and builds
animation inputs. `physics.rs` integrates locally simulated dropped items and
projectiles against the shared collision/profile inputs. `extraction.rs`
converts ECS state to `EntityDraw` values and drives pickup flights.
`render_input.rs` defines `EntityDraw`, the plain render-facing POD consumed by
the GPU preparation paths.

The parent module re-exports the moved public types and systems, so downstream
callers do not need to know the file layout. Internal helpers are exposed only
to the parent and its tests. The extraction schedule remains ordered after
interpolation and before pickup append, preserving the existing frame flow.

## How to change it

Keep network easing and local physics independent: a change to one should not
change the other entity families. Add fields to `EntityDraw` and update every
constructor in `extraction.rs` (including synthetic pickup draws). When moving
helpers between modules, preserve the parent re-export if tests or another
shell subsystem uses the old path. Keep GPU-free ECS logic in these modules;
renderer-specific batching belongs in `crates/lodestone-shell/src/gpu/` or
`lodestone-render`.

## Configuration

The `EntityInterpPlugin` schedule registration and the `FrameDelta`,
`ItemCollision`, and `Profile` resources control execution. No new
environment variables or feature flags were added.

## Dependencies

The modules depend on `lodestone-ecs` for schedules and components,
`lodestone-physics` for collision and profiles, `lodestone-entity` for local
motion rules, and `lodestone-render`/`lodestone-model` for render input types.
The split does not change those dependency boundaries.
