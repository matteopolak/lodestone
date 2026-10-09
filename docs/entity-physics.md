# Entity physics

## What it is

General entity and block physics: per-block-state collision geometry, block movement constants, entity pushing, vehicles (riding, minecarts, boats, leashing), dropped-item physics and pickup flight, and falling blocks. It ports 26.x behaviour through the `VersionAdapter` seam and degrades to unit-cube collision when no family is compiled in.

## How it works

### Ordering and ownership

- Each fixed tick the controller computes one `MovementIntent`, applies it, then queues the changed input bitset, any sprint edge, then `Move` (sneak cancelling sprint sends input, `StopSprinting`, then movement). The order lives in `lodestone_controller::ecs::ControllerPlugin` and the shell interaction plugin and relies on `TickSet`/`ActionQueue` preserving insertion order.
- Raw shift and slowdown are separate: shift immediately drives edge back-off, bounce suppression, water descent and pose, while slow-down is fit-gated (`lodestone_physics::player::should_move_slowly`). Test both shift edges and the fit gate when changing either.
- Experience-orb motion is planned from a tick-start chunk owner and applied by one writer (`mobs::orbs::OrbTickEffect`) that rejects stale, duplicate or incomplete plans; the boundary regression starts at a negative chunk deliberately (truncating division picks the wrong owner).
- Sine/cosine index the checked-in bit table directly; never materialise it lazily (the 256 KiB temporary can blow the browser Wasm stack).
- `Sim::live_collision` takes a 3x3 section snapshot plus a loaded-column mask: a missing section in a loaded column is air, an unloaded neighbour supplies boxes at its boundary, and if the player's own column disappears physics waits. Neither a missing section nor an empty palette proves a column unloaded.

### Collision shapes and block constants

- Per-state geometry comes from the generated `collision_shapes` census in `lodestone-data` (all 32,366 states of a real server) via `VersionAdapter::block_collision`; `collision_boxes` is total over a validated `StateId` and an empty slice means no collision. Typed ids (`StateId`, `EntityType`) are validated once at the input boundary; capability predicates default-deny unknown or custom types.
- Six answers are keyed by block name: `friction`, `speed_factor`, `jump_factor`, `bounce_restitution`, `stuck_multiplier`, `is_climbable`; only 23 of 1,196 blocks differ from defaults (beds, ice family, slime, soul sand, honey).
- `blocks_motion` cannot be derived from geometry (forced-solid/non-solid lists and dynamic shapes make a geometry derivation wrong for 2,618 states across 202 blocks). `stuck_multiplier` cannot be dumped, so only its candidate block set is checked.

### Pushing and hard collision

Soft push and hard collide are independent predicates; only boats, shulkers and happy ghasts hard-collide, players and mobs pass through each other.

```
m    = max(|dx|, |dz|)               // Chebyshev
push = (dx/m, dz/m) * 0.05 * min(sqrt(m), 1.0)    // only when m >= 0.01
```

No falloff past `m = 1`; Y is untouched. A client only pushes the local player, so every felt push is a remote entity's tick; the server runs both sides of a player-mob pair, so that pair is pushed twice per tick, deliberately. `MobSim::push_entities` separates mobs but cannot shove a player (velocity is client-authoritative). Hard collision inflates its query by `1.0E-7` and is gathered before block colliders; climbing vetoes being pushed; spectators neither push nor collide. `Sim::tick_nearby_entities` pre-filters with a radius from `lodestone_data::entity_census::movement_collision_max_dimensions` (floor 16 blocks) and `lodestone_physics::push::pair_admitted` decides contribution.

### Riding

- The set-passengers packet is absolute, not a delta (diff against the previous list to find dismounts); it folds into per-entity `Passengers`/`Vehicle` and the local-player `Riding` scalar (read `Riding`, not `Vehicle`, to know if the player rides).
- Seat: `vehicle.pos + vehicle PASSENGER attachment (yaw-rotated) - rider VEHICLE attachment (0, 0.6, 0)`, fallback `(0, height, 0)`; boats and camels bypass the table (`lodestone_ecs::riding::camel_passenger_attachment`). Pin the rider last in the physics tick after the vehicle moves; `on_ground` is forced false for passengers. Mounting ray-casts entities before blocks and sends `Interact`; dismount is inferred server-side from the sneak bit.
- Boats and land mounts are client-authoritative while ridden (rules in `lodestone_physics::vehicle`): the server does not simulate them and a `VehicleMoved` reply is always a rejection snapping with zero velocity.

| boat clause | value | wrong reading |
|---|---|---|
| gravity | 0.04 | living 0.08 sinks twice as fast |
| drag order | float drags first, then turn impulse | ~11% slower by tick five |
| turn bonus | turning, no forward, no back (three conjuncts) | forward turns also get it |
| yaw commit | between bonus and forward accel | boat drifts on old yaw |

Rider yaw is clamped to 105 degrees of heading on the wrapped difference.

| mount rule | types | input | speed |
|---|---|---|---|
| horse | horse family, llama | sideways /2, reverse /4 | attribute |
| steered | pig, strider | constant forward | attribute x 0.225 / 0.55 |
| camel | camel | horse rule | attribute + 0.1 sprinting |

Step height is 1.0 while player-ridden; a mount with no movement-speed attribute does not move; the horse jump ramp peaks at ten ticks then decays toward 0.8.

**Minecarts** use classic rail behaviour: max speed 0.4 land / 0.2 water, boost 0.06, slide impulse 0.0078125, slowdown 0.997 ridden / 0.96 unridden. Unlike boats a ridden cart is not client-authoritative. Furnace carts burn 3600 ticks per item (cap 32000); TNT carts prime only off an activator rail with an 80-tick fuse; chest and hopper carts have no GUI.

**Boat placement** ray-casts outline shapes and fluids (open water lands at `y + 8/9`), reach 4.5 (+0.5 creative); boats live in a separate `TrackedVehicle` registry, are not persisted, and placement ignores other entities. **Leashing:** leashable means "not hostile-tagged"; a fence anchor is a bare `LeashHolder::Fence(pos)` (no knot entity); at 6 blocks a one-shot straight pull starts (no spring or torque), at 12 the lead snaps; the rope renders as a debug line.

### Dropped items

- Drawn through the item-model pipeline with the model's `display.ground` transform: hover `sin(age/10 + phase) * 0.1 + 0.1`, spin `age/20 + phase` (phase hashes the entity id); copy count steps at 1/16/32/48; solid models jitter, flat sprites fan along z.
- Real swept collision: box 0.25x0.25, step height 0, gravity 0.04, drag 0.98, despawn 64 below min Y; per-block friction is not wired. The ground/water/inside-block check scans every integer cell crossed this tick, not only the destination; settling costs a bounded 36 probes per item per tick. NBT field names are not a safe cross-type key (`Age`, `Health` differ in type between items and mobs): exclude a field from a schema only when its decode failed to consume it.
- Pickup removes the entity and flies a frozen render copy for 3 ticks with quadratic ease-in toward the collector's `(x, (y + eyeY)/2, z)` (`eyeY` absolute), reusing the dropped-item `EntityDraw` and resolving the local player from live physics state. No sound; orbs not modelled.

### Falling blocks

Placement and neighbour update only schedule a tick at the block's own position; the fall runs from the scheduled-tick drain (inline settling would teleport). `v_n = 0.98 * v_(n-1) - 0.04` with displacement before drag; landing height is resolved once at spawn; falls cap at 600 ticks; a placed block waits 2 ticks. The imitated state travels only in the add-entity Object Data field (a client ignoring it draws state 0 silently). Spawn (clear origin, then spawn) and landing (place, then discard) are ordered pairs.

## How to change it

- New name-keyed block constant: a match arm in the shared `block_physics` lookup, never a private table; a per-version change needs a `VersionAdapter` override. A data bump regenerates the collision census and solidity bitsets and updates the dump comparisons.
- Never cap `max_y` at 1.0 (fences are 1.5) or synthesise `blocks_motion` from geometry; the unit-cube fallback is for the no-version-data path only.
- New serverbound riding action: confirm a production producer exists, not just an encoder. New rideable families default to server-simulated; only those needing client authority get local prediction. Seat heights come from the attachment table by entity type; whole-function overrides (boat, camel) need their own arm. Leashable exceptions go in the leashability check's small table.
- New gravity block: widen the explicit three-name table and gate (concrete powder, anvils, dripstone have unported rules).
- For recurrence physics (boat drag, falls, minecart slowdown) re-derive the closed form in a separate script. Keep `movement_collision_max_dimensions` the only source of the broad-phase radius.

## Configuration

No flags except the version seam (without a family, collision is unit cubes). The rest are fixed constants (`BOAT_GRAVITY`, `BOAT_RIDER_YAW_CLAMP_DEGREES`, leash 6.0/12.0, pickup 3 ticks, falling delay 2).

## Dependencies

`lodestone-data` (collision census, solidity bitsets, `StateId`, entity tables); `lodestone-model` (`BlockAabb`, `VersionAdapter`, block-physics lookup); `lodestone-physics` (`CollisionView`, push, `vehicle`, `move_entity`); `lodestone-ecs`, `-entity`, `-server`, `-render`, `-shell` (components, item motion, tick registries, drawing, the two `CollisionView` adapters); `lodestone-v26-2` (the only family with a working solidity census and falling-block Object Data).
