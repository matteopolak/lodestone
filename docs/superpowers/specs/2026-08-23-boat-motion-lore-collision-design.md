# Boat motion, rider masking, lore, and dismount design

## What it is

This change fixes four player-visible defects: a locally controlled boat steps at the
20 Hz physics cadence, the boat's invisible water mask hides parts of its rider, generic
`minecraft:lore` never reaches item tooltips, and the integrated server removes a rider
without placing them at a safe vanilla-style dismount position. The work is split across
three independently testable owners: vehicle rendering, item-component presentation, and
entity collision/dismount authority.

The fixed simulation rate does not change. Vehicle physics and movement packets remain
20 Hz and therefore frame-rate independent; only the sampled render transform changes per
frame.

## How it works

### Controlled-vehicle render interpolation

`lodestone_ecs::vehicle::tick_controlled_vehicle` remains the only producer of locally
simulated vehicle motion. `ControlledVehicleState` records the vehicle pose at the start
and end of the most recently completed fixed tick. A newly mounted vehicle and a server
correction seed both endpoints to the authoritative pose, avoiding a fabricated transition.
Each subsequent tick copies current to previous before running the unchanged boat physics.

The shell samples those endpoints with `FrameClock::interp_alpha`, the same accumulator
residual used by the local player and other fixed-tick render sources. Position is linearly
interpolated and yaw takes the shortest wrapped path. The vehicle mesh, the camera seat, and
the local third-person body all use that one sampled pose. The 20 Hz `Position`, `Rotation`,
velocity, collision, paddle state, and outbound `MoveVehicle` packet remain untouched.

This replaces the controlled vehicle's use of the generic elapsed-time `InterpClock`.
That clock advances before the fixed-tick loop and is appropriate for irregular network
snapshots, but it is not phase-locked to the accumulator residual published after the loop.
A one-tick window reduces lag but can still reach an endpoint early or re-anchor late around
a tick boundary. Explicit previous/current state plus `interp_alpha` is phase-locked by
construction.

Remote and uncontrolled vehicles continue through the generic three-tick network
interpolator. A server correction of the controlled vehicle remains an authoritative snap
and resets both render endpoints rather than easing through rejected motion.

### Boat water-mask ordering

The `boat_water_patch` is invisible geometry that writes depth so translucent water drawn
later cannot appear inside a boat. Today it is batched with ordinary entities. Because
entity planning groups by model and material, a boat's mask may write depth before its rider
is drawn, hiding any rider fragments behind the invisible plane.

Entity preparation will keep visible entity batches and water-mask batches separate. The
opaque world pass draws, in order:

1. solid/cutout terrain;
2. all visible entity bodies, including boats and riders;
3. all colour-write-disabled boat water masks;
4. translucent water.

The visible hull still uses normal depth testing and therefore still occludes body geometry
that is genuinely behind a plank. Moving only the invisible masks after visible entities
prevents a non-visible surface from erasing the rider while preserving the existing water
suppression. Rafts continue to emit no mask.

### Styled item lore

Protocol v770 currently consumes every `minecraft:lore` NBT component only to maintain
packet alignment, then discards it. The canonical model gains an ordered `Vec<Text>` lore
field. The v770 decoder converts each network-NBT component through `Text::from_nbt` and
stores it; the model-to-game-stack conversion carries it through a typed component value.
No plain-string intermediate is introduced.

`container::tooltip::tooltip_lines` inserts lore immediately after the hover name, matching
`ItemStack.getTooltipLines`. Each lore entry becomes a styled span line. The default lore
presentation is dark purple and italic, while explicit nested text styling remains intact.
Potion, enchantment, and book-provided lines retain their current ordering after generic
lore. Advanced durability/id/component-count lines remain last.

Malformed NBT continues to fail the containing stack decode through the existing adapter
error path. The codec's 256-line cap remains enforced. An absent or removed lore component
produces an empty list.

### Boat collision and integrated-server dismount

The shell already gathers nearby entity dimensions and the physics crate already implements
`entity_collision_boxes`, `collide_among_entities`, and `move_entity_among_entities`. The
missing link is that the ordinary player travel path never passes hard entity colliders and
the shell currently filters its neighbourhood to crowd pushers. Boats are not crowd-push
producers in that pass, so they never enter the list.

The per-type facts seam will expose the hard-collidable capability separately from
`pushes_players`. The shell includes nearby boats with their real world-space AABBs, while
preserving the existing default-deny behavior for unknown types. Player movement gathers
eligible entity boxes once from the swept movement box and supplies them to the shared
movement integrator. This allows vertical collision and auto-step to land the player on a
boat without turning ordinary players, mobs, items, or arrows into solid obstacles. A rider
and its current vehicle are excluded through the existing `same_vehicle` gate.

On the integrated server, removing the passenger relationship is followed by authoritative
dismount placement. A pure resolver evaluates vanilla's ordered horizontal escape candidates
around the boat, tests the player's standing/crouching boxes against blocks and the boat,
and selects the first supported, collision-free location. The server updates its player
position and sends the normal position-sync directive in the same dismount transaction.
Only after the client receives that authoritative position does ordinary on-foot physics
continue there. If no candidate is safe, the resolver uses vanilla's fallback rather than
placing the player inside the hull.

External vanilla servers remain authoritative and require no client-selected teleport.
The newly wired boat collider also prevents a one-frame fall-through while an authoritative
dismount update is being applied.

## Error handling and edge cases

- Mounting or correcting a controlled vehicle seeds previous and current render poses to the
  same value; there is no interpolation from an unrelated vehicle or the origin.
- Catch-up frames may run several physics ticks. Each tick still advances the pose pair once,
  and the final `interp_alpha` samples only the latest pair.
- Wrapped yaw interpolation crosses `359° -> 1°` through `0°`, not the long way around.
- A missing adapter or unknown entity type supplies no boat collider.
- The current ridden vehicle never collides with its own rider during the seated tick.
- Empty lore adds no tooltip height. Multiple lines preserve wire order and styled children.
- Boat masks stay after the visible hull and all riders but before translucent water.
- Dismount candidates reject unloaded/unknown collision geometry rather than assuming air.

## Testing

Vehicle rendering gets deterministic tests that run a 20 Hz controlled boat under multiple
render rates (including a non-divisor such as 144 Hz), asserting smooth monotonic sampled
positions, exact tick endpoints, frame-rate-independent authoritative poses, shared boat/seat
coordinates, and shortest-path yaw. A correction test asserts that both endpoints reset.

Water-mask tests assert batch ordering independent of entity input/material grouping. The
existing boat-water pixel gate is extended with a rider crossing the mask plane: the rider
must remain visible while water inside the hull remains suppressed.

Lore tests begin at the v770 wire decoder with multiple styled NBT lines, assert the canonical
and game-stack representations, and then assert tooltip order, text, colour, italics, and box
height. The test is observed failing while the decoder still discards lore before production
code is changed.

Collision tests route an actual falling player movement through the player tick with a boat
AABB and assert that the feet settle on the boat's top face with `on_ground = true`. Negative
controls prove a non-collidable entity remains pass-through and the currently ridden boat is
excluded. Integrated-server tests dismount beside boats on flat ground, beside an obstructed
side, and in water, and assert both the empty passenger packet and authoritative player
position update.

## How to change it

- Change local vehicle interpolation in `crates/lodestone-ecs/src/vehicle.rs` and the shell's
  controlled-vehicle sampling in `crates/lodestone-shell/src/entities.rs` /
  `crates/lodestone-shell/src/sim/camera.rs`. Do not advance physics from a render call.
- Change boat-mask submission in `crates/lodestone-shell/src/gpu/entity_passes.rs` and
  `crates/lodestone-shell/src/gpu/frame.rs`. Preserve the visible-hull-before-mask invariant.
- Add or extend item component storage in `crates/lodestone-model/src/item.rs`, conversion in
  `crates/lodestone-game/src/item.rs`, v770 decode in
  `crates/protocol/v770/src/adapter/inventory.rs`, and rendering in
  `crates/lodestone-shell/src/container/tooltip.rs`.
- Extend entity collision through `lodestone-model`'s `EntityFacts`, v770/data lookup,
  `lodestone-shell/src/sim/collide.rs`, and the existing `lodestone-physics` movement entry
  points. Keep pushability and hard collision as separate predicates.
- Keep authoritative dismount resolution in `lodestone-server`; share only the pure geometric
  candidate/resolution helper where doing so avoids a second implementation.

## Configuration

None. There is no frame-rate switch, interpolation-duration setting, lore flag, or collision
compatibility toggle. Fixed vehicle physics remains `20 Hz` through `TICK_PERIOD`.

## Dependencies

- `lodestone-ecs::FrameClock` and controlled-vehicle state.
- `lodestone-physics` entity movement, AABBs, and collision predicates.
- `lodestone-model::Text` and the v770 network-NBT component decoder.
- `lodestone-shell` entity batching, camera/rider extraction, and tooltip renderer.
- `lodestone-server` world collision view and position-sync protocol directive.

