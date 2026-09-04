# Entity physics

## What it is

General entity and block physics: per-block-state collision geometry, the movement constants a block
applies to whatever stands on it, entity-versus-entity pushing and hard collision, vehicles (riding,
minecarts, boats, leashing), dropped-item physics and the pickup-flight animation, and falling
sand/gravel. All of it ports vanilla 26.2 behaviour rather than approximating it, and most of it is
reached only through the `VersionAdapter` seam so it degrades cleanly when no version family is compiled
in.

## How it works

### Collision shapes and block-physics constants

Per-state collision geometry comes from a **census** (`lodestone-data`'s generated `collision_shapes`
table, a dump of `getCollisionShape().toAabbs()` over all 32,366 states of a real 26.2 server), reached
zero-copy through `VersionAdapter::block_collision(state_id)`. Two `CollisionView` adapters
(`crates/lodestone-shell/src/collision.rs`) answer it for an offline demo world and a live server
snapshot respectively, both delegating to one shared free function so they cannot disagree.

Six other answers are keyed by block **name**, not state id, because they are `final`
`BlockBehaviour.Properties` fields rather than geometry — putting them behind the version seam would need
an identical copy per protocol crate: `friction`, `speed_factor`, `jump_factor`, `bounce_restitution`,
`stuck_multiplier`, `is_climbable`. Only 23 of 26.2's 1,196 blocks set a non-default value: the 16 dyed
beds (restitution 0.75), ice family (friction 0.98, blue ice 0.989), slime block (friction 0.8,
restitution 1.0), soul sand (speed factor 0.4), honey block (speed factor 0.4, jump factor 0.5).
`bounce_restitution` is already net of `SUPPRESSES_BOUNCE`, whose only 26.2 member (honey) sets no
restitution, so today the subtraction is a no-op.

`blocks_motion` looks like it should derive from the collision shape but can't: vanilla's own
`calculateSolid()` is five branches and only the last is geometry — `forceSolidOn` (237 blocks) and
`forceSolidOff` (8 blocks) short-circuit with no getter or data-file field, 23 `dynamicShape()` blocks
cache `false`, and only the residual falls to `bounds.getSize() >= 0.729166... || bounds.getYsize() >=
1.0` (exactly a ladder's mean extent — `Blocks.LADDER` forces itself off *because* the threshold is wrong
for it). A geometry-only derivation is wrong for 2,618 of 32,366 states across 202 blocks.
`stuck_multiplier` can't be dumped at all — the vector is built in imperative per-block code (`WebBlock`,
`SweetBerryBushBlock`, powder snow) — so only the *candidate set* (blocks overriding `entityInside`) is
checked exhaustively.

The migrated complete 26.2 state tables in `lodestone-data` use `block_states::StateId` as their public
boundary: both complete `block_solidity` columns and `path_types::path_type` return totals once a raw id
has been validated with `StateId::new`. Raw compatibility entry points retain `Option` only where callers still
own an unvalidated wire id; `PathTypeRegistry` and `VersionAdapter` are version-free boundaries. Keep
that conversion at the boundary so generated-table indexing cannot receive an out-of-range value.

The block-break hardness census follows the same rule: `hardness(StateId)` returns its complete
`Hardness` record without an `Option`, while `hardness_raw` is the explicit fallible boundary for
unvalidated ids. Consumers such as tool evaluation, server break validation, and the 26.2 adapter
validate once and retain the typed id for the table read.

### Entity-versus-entity pushing and hard collision

Two independent predicates decide what happens when two entity boxes overlap:

| | predicate | base entity default | living-entity default |
|---|---|---|---|
| soft push | is-pushable | `false` | alive, not spectator, not on a climbable |
| hard collide | can-be-collided-with | `false` | not overridden |

The only hard-collide overrides in 26.2 are boats (always `true`), shulkers (alive check), and
happy ghasts (a state machine) — **players and mobs pass through each other by design**, while a boat is
both collidable and pushable and a shulker blocks without shoving.

Vanilla's own entity-push routine computes one horizontal vector and applies `-v`/`+v` symmetrically, gated per side
on "not a vehicle and is-pushable"; Y is never touched, and there's no ordering rule because nothing is
read after it's written — the impulse lands on velocity and integrates next tick, so simultaneous pushes
commute. Writing `m = max(|dx|, |dz|)` (Chebyshev, not Euclidean):

```
push = (dx/m, dz/m) · 0.05f · min(√m, 1.0)     — gated on m >= 0.01f
```

The normaliser is `√m` on the Chebyshev distance (~6% off Euclidean, off-axis); there is **no distance
falloff** past `m = 1` — flat at `0.05f` from there; `min(√m, 1.0)` is a soft start near contact, not a
blow-up cap. No per-entity push cap; crowding is bounded only by per-pair magnitude and drag.

A client-side pushee must be the local player, so a vanilla client never *initiates* a push — every push
felt comes from a remote entity's own tick. On the server both sides of a player-mob pair run
`pushEntities`, so the pair is pushed **twice** per tick where a client applies it once — a stated
asymmetry. `MobSim::push_entities` reuses the same formula server-side to separate mobs from each other
and from an overlapping player, but cannot shove a player back (velocity is client-authoritative; that
clientbound half isn't wired). Hard collision inflates its query by `1.0E-7` (push uses no epsilon) and is
gathered once from the movement box, ahead of block colliders. `onClimbable()` vetoes `isPushable()`, so a
mob crush cannot shove a player off a ladder.

`Sim::tick_nearby_entities` applies a deliberately broad, per-axis candidate filter before the actual
overlap predicate. Its radius is cached once from
`lodestone_data::entity_census::movement_collision_max_dimensions`, the union of types that push the
player and types that can hard-collide with movement. The current 16.0-wide dragon establishes the
16-block radius while the 12-high giant remains covered vertically; future wider hard-only colliders are
included too. The 16-block last-known-safe value is also a floor: an empty or malformed census cannot
silently narrow the filter and lose a real candidate; a future wider generated entry expands it
automatically. This is a performance pre-filter only — `lodestone_physics::push::pair_admitted` still
decides whether the candidate can contribute an impulse.

### Vehicles

**Riding: seat, camera, mount/dismount.** `ClientboundSetPassengersPacket` is absolute, not a delta, and
folds into two disjoint facts: per-entity `Passengers`/`Vehicle` state and a local-player `Riding`
session scalar. A dismount is the same packet with the rider absent, so the fold must diff against the
previous list first. The seat position:

```
passenger.pos = vehicle.position()
              + vehicle's PASSENGER attachment at the seat index, yaw-rotated
              - passenger's own VEHICLE attachment, yaw-rotated
```

`PASSENGER`'s fallback is `(0, height, 0)` — the box top, not `× 0.85` (eye height, a different
quantity). The player's own `VEHICLE` attachment is `(0, 0.6, 0)`, **subtracted** — omitting it floats
the rider 0.6 above every saddle. Per-type `y`: minecart family 0.1875, horse 1.44375, donkey 1.1125,
mule 1.2125, skeleton/zombie horse 1.31875, pig 0.86875, llama `(0, 1.37, -0.3)`. Boats bypass the
table: `height/3` (`× 0.888...` for rafts) plus a Z offset (0 / 0.15 chest boat / −0.6 past the first
seat).

The camera needs no riding-specific code — no `SITTING` pose, eye height unchanged — so pinning the feet
to the seat each tick is the whole mechanism, run **last** in the physics tick, after the vehicle's own
tick moves it (or the camera lags a tick). `on_ground` is forced `false` for any passenger — not to
dodge a kick counter (which exempts passengers anyway) but so local readers (pose, jump, flight-cancel)
don't treat a seated player as standing on something. Mounting checks the entity ray before the block
ray and sends `Interact`, not `InteractAt`. Dismounting needs no new code — it's inferred server-side
from the sneak bit already sent every tick, as vanilla itself does.

**Every vehicle is client-authoritative while ridden**: the server accepts `ServerboundMoveVehiclePacket`
and does not simulate a ridden vehicle at all — true of horses as much as boats. What makes a mount move
is a local simulation (`lodestone_physics::vehicle`) sending `MoveVehicle`/`PaddleBoat` every tick; a
`VehicleMoved` reply is always a **rejection** that snaps to the server's position with zero velocity,
never a nudge.

**Boats.** `AbstractBoat` never calls the shared `travel`: classify surroundings, apply buoyancy/drag,
turn and accelerate off raw key bits, move directly. Easy to get backwards:

| clause | value | wrong reading |
|---|---|---|
| gravity | `0.04` (not the living `0.08`) | sinks twice as fast |
| drag order | float drags **first**, then the turn impulse is added | ~11% slower by tick five |
| turning bonus | needs **three** conjuncts (turning, no forward, no back) | every forward turn also gets it |
| yaw commit | `setYRot` runs **between** the bonus and forward acceleration | a turning boat drifts along the old yaw |

Rotation itself decays at 0.9 on water, halved per tick on land with a rider. Rider yaw is clamped to
±105° of the boat's heading, on the *wrapped* difference.

**Land mounts** are `LivingEntity`s sharing the player's own integrator, so the two never disagree about
slabs or ice — but `getRiddenInput`/`getRiddenSpeed` are **three different rules**:

| rule | types | input | speed |
|---|---|---|---|
| `Horse` | horse, donkey, mule, skeleton/zombie horse, llama | sideways ÷2, reverse ÷4 | mount's own attribute |
| `Steered` | pig, strider | ignored — constant `(0, 0, 1)` | attribute × 0.225 / × 0.55 |
| `Camel` | camel | the horse rule | attribute **+ 0.1** sprinting |

Reading a pig as a horse makes it strafeable, reversible, ~4.4× too fast, and looks fine at the call site
(only a strafe or reverse discriminates the rules). Step height is forced to `1.0` while
**player**-ridden regardless of the mount's own attribute — what lets a ridden pig or strider clear a
whole block. The jump ramp (horse family only) peaks at ten ticks (`1.0`) then **decays** toward `0.8` —
ticks 9 and 11 give the same result. A mount with no reported movement-speed attribute simply doesn't
move rather than guessing a generic default (~3× too fast).

**Minecarts** port the original `OldMinecartBehavior` (not the newer opt-in "minecart improvements"
physics no ordinary world runs): gravity, then `move_along_track` (powered-rail boost/brake,
ascending-rail slide impulse, exit-pair centreline snap across all ten `RailShape` values) or
`come_off_track`, then yaw/flip bookkeeping. Max speed 0.4 land / 0.2 water, powered boost 0.06, slide
impulse 0.0078125, slowdown 0.997 ridden / 0.96 unridden. Only a plain minecart is rideable, and — unlike
a boat — a ridden one is **not** client-authoritative; every cart ticks identically whether ridden or
not. A furnace minecart burns fuel (3600 ticks/item, cap 32000) and self-propels; a TNT minecart primes
only off an activator rail, 80-tick fuse into the same detonation pipeline as primed TNT. Placement:
right-click a rail, or a dispenser loaded with one (falls back to a plain toss with no rail ahead). No
rider-movement nudge, no auto-mount; chest/hopper carts are real storage with no GUI.

**Boat placement and boarding.** Vanilla's own boat-item-use handler runs its own raytrace, not a
block-relative placement:
outline shapes (grass, flowers, lily pads are hittable), any fluid shape (open water lands a boat at
`y + 8/9`), the player's own yaw, reach 4.5 (+0.5 creative). Boats are a separate `TrackedVehicle`
registry, not routed through mob simulation (which would wander it) — the passenger lives on the vehicle
so the tick can skip simulating a ridden hull. Boarding is vanilla's own start-riding call, ahead of
the generic mob
interact chain, and respects the sneak-click "don't board" flag. Dismounting searches a real point
outside the hull against real collision shapes, falling back to the hull's own top; only boats use this
resolver. Placement's obstruction test doesn't check other entities (a boat can overlap a mob), there's
no rejection for a bad `MoveVehicle`, and a boat isn't persisted across a restart.

**Leashing.** Vanilla's own can-be-leashed check's real default is "not one of vanilla's hostile-tagged
mobs" —
every non-hostile species by default, not a curated allowlist. Attaching mirrors vanilla's two branches
(already leashed here → detach, drop a lead unless creative; else unheld-elsewhere, holding a lead,
leashable, within 12 blocks → attach), checked before the taming chain. A fence anchor moves every mob
leashed to a holder onto `LeashHolder::Fence(pos)` — a bare block position, not a real knot entity, so
there's nothing to render or right-click there yet. Per-tick: 6 blocks starts a one-shot straight-line
pull toward the holder (a disclosed simplification of vanilla's real multi-point spring/torque model — no
yaw torque, no per-entity width subtraction), 12 blocks snaps the lead and drops it; an unresolvable
holder silently drops it with no item. The wire carries the resolved link diffed like metadata, so a late
joiner still sees the rope; it renders as a flat debug line, not vanilla's textured rope.

### Items

**Dropped-item render and physics.** An item entity isn't a cuboid rig — it draws through the same
item-model pipeline the hotbar's 3-D icons use. Pose: hover `sin(age/10 + phase) * 0.1 + 0.1` (always
`0.0..=0.2`), spin `age/20 + phase` radians, composed with the item's own declared `display.ground`
transform (falling back to vanilla's generated constants only when a model declares no `ground` slot —
posing with the *gui* transform instead is visibly wrong: both tilted and 2.5× too large). Phase is
hashed from the entity id, since the client can't observe vanilla's RNG seed. Stack count drives
`rendered_amount` (1 copy up to 1, 2 above that, 3 above 16, 4 above 32, 5 above 48): a solid model
jitters ±0.15 on all axes, a flat sprite (posed z-extent under 0.0625) instead fans copies along z with
tighter jitter — reversed, a block stack looks like a fan and a stick stack a cloud. Enchanted items draw
a second, depth-equal glint mesh.

Resting height is real swept collision against the same per-state shapes collision uses, not a solid/air
boolean — the old boolean floated an item a full block high on grass and half a block high on a slab,
while an uncapped shape like a fence (1.5 tall) put it too *low*. The item box is 0.25 × 0.25 with **step
height 0.0**. Gravity/air-drag are the vanilla item values (0.04 / 0.98); an item 64 blocks below the
world's min Y despawns. Per-block friction isn't wired yet. **General rule this exercises: a per-tick
ground/water/inside-block check must scan every integer cell the movement crossed this tick, not just the
destination** — sampling only the post-move position can tunnel through thin geometry at high speed, and
falling blocks share the same shape. Settling costs a bounded, linear 36 probes per item per tick.

NBT field names are not a safe cross-type key: `Age` is a `Short` on `minecraft:item` (ticks alive) but
an `Int` on a mob (breeding age, negative for a baby), and `Health` is a `Float` on a mob but a constant
`Short` on an item. A round-trip schema keyed on field name must exclude a field only when its decode
**failed to consume it** for that specific type — never because the name merely appears on another
type's modelled-field table.

**Item pickup animation.** The flight toward a collector is not the item entity retargeted and lerped —
vanilla removes the entity immediately and animates a frozen copy of its render state as a particle,
which is why the bob/spin phase freezes at pickup. Three ticks (150 ms), **quadratic ease-in**
(`t = (life + partial)/3; t *= t` — half the flight covers a quarter of the distance), target the
collector's `(x, (y + eyeY)/2, z)` where `eyeY` is *absolute*, not an offset. The collector is any living
entity, not just the local player. The animation reuses the dropped-item draw path exactly — an ordinary
item `EntityDraw` with the render definition frozen — needs no new GPU code, must trigger before the
entity's render track is pruned, and must resolve the local player via its live physics state (it has no
render track to read a position from otherwise). No sound; experience orbs and partial-pickup amounts
are not modelled (the authoritative inventory count is a separate wire path and must never be derived
from this one).

### Falling blocks

Both jar triggers (placement, neighbour update) only **schedule** a tick — settling must never happen
inline off the neighbour path, or a block whose support was removed teleports instead of falling, and
the schedule must fire at the block's own position, not through a helper that notifies neighbours and not
the origin. The fall runs once, from the scheduled-tick drain.

Physics: `v_n = 0.98 * v_(n-1) - 0.04`, displacement applied **before** drag (gravity 0.04, drag 0.98) —
tick one moves exactly 0.04, not 0.0392; the drag-first reading differs by under 2%, so only a
hand-solved closed form can tell them apart. Landing height is resolved **once**, at spawn, against the
world as it stood then — a block appearing underneath mid-flight is fallen through, not landed on.
Falls cap at 600 ticks; a placed block waits 2 ticks before a neighbour-triggered fall can start.

The imitated block state travels only in the `ADD_ENTITY` packet's Object Data field, never per-tick
metadata — a client ignoring that field draws every falling block as state id 0, with nothing logged.
Spawn and landing are each two ordered effects (clear origin, then spawn; place landed block, then
discard entity) — reversing either shows both old and new block at once, or neither.

## How to change it

- **Adding a name-keyed block-physics constant**: a match arm in the shared `block_physics` lookup, never
  a private table in `collision.rs` — every `CollisionView` adapter and any plugin must read the one
  function, or they will disagree.
- **A data bump (new MC version)**: regenerate the collision census and solidity bitsets from a fresh
  dump; a changed name-keyed value needs a per-version `VersionAdapter` override, never an edit to the
  shared table.
- **`max_y` must never be capped at 1.0** — a fence is 1.5 tall and the 0.6 auto-step cannot mount it;
  clamping makes fences look step-able.
- **Occlusion is not collision, and `blocks_motion` must never be synthesised from the collision shape.**
  The unit-cube fallback is legitimate only as the no-version-data degraded path; `Option<bool>`'s value
  is that "no census" stays distinguishable from "the census says false".
- **Adding a serverbound vehicle/riding action**: check for a real *producer*, not only an encoder —
  every outbound riding action here was once encoded with zero production callers before client vehicle
  authority landed.
- **Adding a rideable vehicle family** defaults to server-simulated (deny local prediction) unless the
  vehicle genuinely needs client authority — minecarts stay server-driven because their motion is
  rail-following, and predicting it locally would fight the broadcast.
- **Adding a vehicle's seat height**: the declared-attachment table keyed by entity type; a vehicle
  overriding the whole attachment function (boat, camel) needs its own arm instead.
- **Consuming "are we riding"**: read the local-player `Riding` session scalar, not the per-entity
  `Vehicle` component — the local player is structurally excluded from the entity read-model.
- **Adding a leashable exception**: extend the small table inside the leashability check, not the
  hostility predicate it wraps.
- **Adding another gravity block**: widen the explicit three-name table *and* its own gate together —
  concrete powder, anvils and pointed dripstone have extra vanilla behaviour this port doesn't have.
- **Changing any recurrence-based physics** (boat drag, falling-block fall, minecart slowdown): re-derive
  the closed form in a separate script rather than adjusting expected numbers to match new code.
- **Changing movement-collision capabilities**: keep
  `lodestone_data::entity_census::movement_collision_max_dimensions` as the single source for the
  shell's broad-phase radius. It deliberately includes both crowd pushers and hard colliders; do not
  reintroduce a second literal beside `Sim::tick_nearby_entities`, because the floor is only a
  non-shrinking fallback.
- **A per-tick ground/water/inside-block check must scan every integer cell the movement crossed this
  tick**, not just the destination — falling blocks and item settling both depend on this.
- **A name-keyed persistence schema must exclude a field only when its decode failed to consume it for
  that type** — never because the name merely appears on a different type (mob `Age`/`Health` collide in
  type with item `Age`/`Health`).

## Configuration

No feature flags gate any rule here except the version seam itself: compiling in a real version family
supplies the real collision/solidity census; without one, collision degrades to unit cubes and solidity
falls back to the (wrong-for-202-blocks) geometry derivation. Everything else below is a fixed vanilla
constant, not a tunable:

| area | constants |
|---|---|
| entity push | none — `MAX_ENTITY_CRAMMING`'s damage is a server game-rule concern, not read here |
| riding | seat-attachment table, `BOAT_GRAVITY` 0.04, `BOAT_RIDER_YAW_CLAMP_DEGREES` 105 |
| minecarts | max speed 0.4/0.2, boost 0.06, slide impulse 0.0078125, slowdown 0.997/0.96 |
| boat placement | reach 4.5 (+0.5 creative) |
| leashing | elastic distance 6.0, snap distance 12.0 |
| dropped items | hover/bob/spin constants, flat-item depth threshold 0.0625 |
| item physics | item box 0.25×0.25/step 0.0, void despawn depth 64, gravity/drag 0.04/0.98 |
| pickup animation | flight length 3 ticks, target eye fraction 0.5, remote-collector eye height 1.62 |
| falling blocks | delay after place 2, gravity 0.04, drag 0.98, max fall ticks 600 |

## Dependencies

- `lodestone-data` — collision-shape census, block solidity bitsets, `block_states::state_id`, entity
  type/dimension tables.
- `lodestone-model` — `BlockAabb`, `VersionAdapter`, the block-physics lookup and its default record, the
  riding/vehicle event and action variants.
- `lodestone-registry` — resolves the compiled-in version adapter.
- `lodestone-physics` — `CollisionView`, the shared push/collision implementation, vehicle rules
  (`lodestone_physics::vehicle`), and the `move_entity` integrator every vehicle, item and falling block
  reuses.
- `lodestone-entity` — item motion, item gravity/drag constants.
- `lodestone-ecs` — per-entity passenger/vehicle components, the local-player riding scalar, pickup
  animation state.
- `lodestone-server` — vehicle/minecart/falling-block tick registries, leash logic, boat placement.
- `lodestone-render` — item-model pipeline, non-living vehicle placement matrices.
- `lodestone-shell` — the two `CollisionView` adapters, `sim`/`net` wiring for vehicles and pickups,
  moving-block rendering.
- `lodestone-v26-2` — the only family with a working solidity census, boat metadata, and the
  falling-block Object Data field; legacy families discard it.

## See also

- [`docs/combat.md`](./combat.md) — knockback shares the same physics-owner handoff leashing's pull
  reuses.
- [`docs/autonomous-navigation.md`](./autonomous-navigation.md) — the pathfinder most likely to consume
  `blocks_motion` and block-physics constants next.
- [`docs/plugin-api.md`](./plugin-api.md) — why the name-keyed block-physics table is version-free and
  reachable outside `lodestone-shell`.
- [`docs/entity-rendering.md`](./entity-rendering.md) — the per-entity ingest set riding and vehicles add
  components to.
- [`docs/player-simulation.md`](./player-simulation.md) — the other server-granted local-player scalars
  (creative flight among them) driving physics the same way `Riding` does.
