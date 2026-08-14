# Riding

Getting into a boat, minecart or saddled animal, having the camera sit on the seat, and
getting back out. Tier 1 item 8 of [`backlog.md`](./backlog.md).

**Scope landed:** mount, seat position, camera, `on_ground` override, dismount, **and the vehicle
moving** — boat steering, land-mount steering, the horse jump, paddles, rider yaw clamping, and the
server's correction. See [Client authority](#client-authority-the-half-that-makes-anything-move).
**Scope deferred:** the horse jump bar, the minecart camera lerp, the jar-generated attachment
table, the per-instance mount animations, and the horse inventory screen. See
[What is deferred](#what-is-deferred).

---

## What it is

`ClientboundSetPassengersPacket` tells the client which entities are riding which. Before this
change it was a **complete island**: decoded at `crates/protocol/v770/src/adapter/entity.rs`'s
`SET_PASSENGERS` arm, round-tripped by `crates/protocol/v770/tests/entity_events.rs`, and a
tree-wide grep for `EntityPassengersChanged` returned exactly **four** hits — the decode, those
two tests, and the `ClientEvent` variant. Zero consumers, no arm in either `handles_event`
router, and no producer of any of the serverbound riding actions either.

That was true of all six riding-shaped items on the wire. Checked with a working detector (the
same grep found 19 hits for a known-wired `ClientAction::SwingArm`):

| item | direction | status before | status now |
|---|---|---|---|
| `ClientEvent::EntityPassengersChanged` | in | decoded, 0 consumers | folded by both routers |
| `ClientEvent::VehicleMoved` | in | decoded, 0 consumers | `ingest::apply_vehicle_moved` |
| `ClientEvent::MountScreenOpened` | in | decoded, 0 consumers | still an island — needs the horse inventory screen |
| `ClientAction::MoveVehicle` | out | encoded, 0 producers | `vehicle::send_vehicle_actions`, once per tick |
| `ClientAction::PaddleBoat` | out | encoded, 0 producers | same system, once per tick |
| `PlayerCommand::StartRidingJump` | out | encoded, 0 producers | `vehicle::charge_riding_jump`, on the release edge |
| `PlayerCommand::StopRidingJump` | out | encoded, 0 producers | **still zero, and permanently** — the vanilla client has no sender for it |

---

## How it works

### The two halves of one packet, and which router owns each

`SET_PASSENGERS` feeds two disjoint facts, and `SharedState::apply` routes on the **union** of
`ingest::handles_event` and `session::handles_event`, so an arm in only one of them leaves the
other half as a fold that never fires.

| fact | home | why |
|---|---|---|
| which entity rides which | `lodestone_ecs::entity::{Passengers, Vehicle}`, folded by `ingest::apply_entity_passengers` | per-entity ECS state, keyed by server id |
| am I riding, and what | `lodestone_ecs::session::Riding`, folded by `session::apply_local_player_state` | a local-player scalar that drives physics and the camera |

Both routers claim the event. That is the same deliberate double-claim `Login` already uses: two
disjoint writes off one event, reaching the schedule once.

`Passengers` holds **raw server ids**, not `bevy_ecs::Entity`s, because `SET_PASSENGERS` can name
a passenger the client has not spawned yet — resolving at fold time would lose the seat
permanently. `Vehicle` (the reverse edge) *is* resolved, and is therefore best-effort for an
unspawned id; it re-arrives with the next `SET_PASSENGERS`.

**The packet is absolute, and a dismount has no event of its own** — it is the same packet with
the rider gone. So `apply_entity_passengers` does three things in order: diff against the
previous list and remove `Vehicle` from whoever left, replace the vehicle's list, then insert
`Vehicle` on the new list. Skipping the first step strands a dismounted rider naming a vehicle
nothing can ever free them from.

### The seat, and vanilla's 26.2 attachment rule

`lodestone_ecs::riding` is a pure port of the rule, unit-tested with no `World`. Read out of
`.cache/mc/26.2`:

```text
passenger.pos = vehicle.position()
              + PASSENGER attachment of the vehicle, at the rider's seat index,
                rotated by the vehicle's yaw
              - VEHICLE attachment of the passenger, rotated by its own yaw
```

`Entity.positionRider`, `getPassengerRidingPosition`,
`getDefaultPassengerAttachmentPoint`, `getVehicleAttachmentPoint`,
rotation in `EntityAttachments.transformPoint` (`point.yRot(-rotY * π/180)`, i.e. `Vec3.yRot`).

Two constants worth stating because the plausible neighbour is wrong:

- **The `PASSENGER` fallback is `(0, height, 0)` — `height × 1.0`**, the top of the vehicle's box
  (`EntityAttachment.PASSENGER`'s `Fallback.AT_HEIGHT`). The tempting `× 0.85` is a
  *different* quantity, `EntityDimensions.defaultEyeHeight`.
- **The player's `VEHICLE` attachment is not zero.** `Avatar.DEFAULT_VEHICLE_ATTACHMENT =
  Vec3(0.0, 0.6, 0.0)`, wired at `EntityTypes.PLAYER`'s builder, and it is
  *subtracted*, lowering the rider 0.6 below the seat point. Dropping it floats the player above
  every saddle — the single largest error available in this function.

Declared per-type points, cited in the source: minecart family `0.1875`
(`EntityTypes.MINECART`, repeated verbatim for chest/furnace/hopper/tnt/spawner/command-block),
horse `1.44375` (`EntityTypes.HORSE`), donkey `1.1125` (`EntityTypes.DONKEY`), mule `1.2125` (`EntityTypes.MULE`), skeleton/zombie horse
`1.31875` (`EntityTypes.SKELETON_HORSE`, `EntityTypes.ZOMBIE_HORSE`), pig `0.86875` (`EntityTypes.PIG`), llama `(0, 1.37, -0.3)` (`EntityTypes.LLAMA`, `EntityTypes.TRADER_LLAMA`).

**The boat family bypasses the attachment table entirely.** `AbstractBoat.getPassengerAttachmentPoint`
builds the point from `rideHeight(dimensions)` and a
**Z** offset. `rideHeight` is `height / 3` for boats and chest boats (`Boat.rideHeight`,
`ChestBoat.rideHeight`) and `height × 0.8888889` for rafts (`Raft.rideHeight`, `ChestRaft.rideHeight`) —
`0.1875` vs `0.5` at the shared `sized(1.375, 0.5625)` box, nearly a factor of three, so one rule
for both is visible. The Z offset is `0.0` (`AbstractBoat.getSinglePassengerXOffset`), `0.15` for chest boats
(`AbstractChestBoat.getSinglePassengerXOffset`), `-0.6` for any seat past the first (`AbstractBoat.getPassengerAttachmentPoint`). Note vanilla names the
field `getSinglePassengerXOffset` and applies it to Z; the name is kept so the citation matches.

### The camera, which needed no code

26.2's `Camera.alignWithEntity` has **no
`isPassenger()` branch** except a lerp fix-up for new-behaviour minecarts, and riding
changes neither the pose nor the eye height (`Player.updatePlayerPose` has
no riding case; there is no `SITTING` pose, so a mounted player keeps
`Avatar.DEFAULT_EYE_HEIGHT = 1.62`).

So the whole mechanism is `pin_passenger_to_vehicle` moving the player's **feet**.
`Sim::camera` reads `PhysicsState` exactly as it does for a walking player, and the eye, the
block-target ray origin and the audio listener all move together. Adding a passenger branch to
`camera_rig.rs` would double-apply the attachment.

### Where the pin sits in the tick, and why

`TickSet::Physics`, chained **last**: `apply_creative_flight_input → player_physics →
cancel_flight_on_landing → pin_passenger_to_vehicle`.

Vanilla runs a passenger's whole tick — travel included, with the same `xxa`/`zza` the vehicle
reads — and only then overwrites the position: `rideTick()` is
`setDeltaMovement(ZERO); this.tick(); vehicle.positionRider(this)`, and
`LivingEntity.aiStep` still reaches `travel(input)` for a passenger because
`canSimulateMovement()` is `isLocalInstanceAuthoritative()`, true for the local player
(`Entity.isLocalInstanceAuthoritative`/`Entity.canSimulateMovement`, overridden by
`Player.canSimulateMovement`). So a mounted
player's one tick of drift out of the seat really does happen and really is thrown away. The one
divergence: velocity is zeroed at the *end* rather than the top, which differs only on the first
tick after mounting and is discarded by that same tick's snap.

### `on_ground` while riding — and the reason usually given for it is wrong

`Player.tick` forces `onGround = false` for a spectator or passenger, unconditionally,
before anything else in `tick()`. This closes the `spectator_or_passenger_note` contract that had
sat in `lodestone-physics/tests/on_ground.rs` since before riding existed.

`PlayerState::on_ground`'s docs frame the flag as a wire contract policed by the server's
`aboveGroundTickCount` / `multiplayer.disconnect.flying` counter, which would make this
kick-avoidance. **It is not, and the check was worth running:** the server's float check is
explicitly `&& !this.player.isPassenger()` (`ServerGamePacketListenerImpl.tickPlayer`), and its
move handler (`ServerGamePacketListenerImpl.handleMovePlayer`) discards a passenger's reported
position outright, keeping only the rotation. A mounted client cannot be kicked over this flag. The override is for the
**local** readers — pose, view bob, jump, flight cancel — which would otherwise treat a seated
player as standing on something.

The riding state is *not* a `PlayerState` field, deliberately: it is a session fact, and a
`passenger: bool` in the physics engine would give it something it can neither set nor act on
beyond forcing one flag, plus a second writer of `on_ground` per tick.

### Mount

`Sim::use_item_live` gained an **entity branch, before the block branch** — vanilla's
`Minecraft.startUseItem` switches on `hitResult.getType()` with `case ENTITY` first, the same
priority `begin_attack_live` already gave the left button off the same `EntityRayTarget`. Before
this, `use_item_live` returned early when `self.target()` was `None` and never consulted the
entity ray, so right-clicking a boat sent nothing.

`Sim::interact_entity` sends `ClientAction::InteractEntity { interaction: Interact { hand: Main } }`
plus a `SwingArm`. **`Interact`, not `InteractAt`**: `InteractAt` carries the entity-local hit
position, and `update_entity_target` keeps only the winning entity's id, not the ray's hit point
on its box. The server dispatches mounting off the plain `Interact` (`Entity.interact` calls
`player.startRiding`); `InteractAt` matters only for per-part hits. Refining it needs the ray to
start reporting its hit position, not a guess at the call site.

Entity-first is correct rather than merely a choice: `update_entity_target` already clamps the
entity search to the block hit's own entry distance, so an entity is reported only when it is
closer than the block.

### Dismount — no code at all

There is **no dismount packet and no `STOP_RIDING` action**. `ServerboundPlayerCommandPacket`'s
complete enum is `STOP_SLEEPING, START_SPRINTING, STOP_SPRINTING, START_RIDING_JUMP,
STOP_RIDING_JUMP, OPEN_INVENTORY, START_FALL_FLYING`. Dismount is inferred server-side from the sneak
bit of `ServerboundPlayerInputPacket`: `handlePlayerInput` → `setShiftKeyDown`, then `Player.rideTick`'s
`if (!isClientSide() && wantsToStopRiding() && isPassenger()) stopRiding()`, with
`wantsToStopRiding()` = `isShiftKeyDown()`.

`lodestone_controller::ecs::send_player_input` already sends that bit, edge-triggered, and is
unconditional on ride state — so dismount works with no new code, and the vanilla client likewise
**does not predict it** (note the `!isClientSide()` guard). If a future change ever gates
`send_player_input` on riding, dismount breaks silently; that is the thing to watch.

### Client authority: the half that makes anything move

**Every vehicle is client-authoritative while a player rides it, so none of them move until we
simulate them locally.** `Entity.isClientAuthoritative` delegates to the controlling passenger and
`Player.isClientAuthoritative` is `true`, so the server's `LivingEntity.travelRidden` takes the
`setDeltaMovement(Vec3.ZERO)` branch and simply **accepts** `ServerboundMoveVehiclePacket`. This is
true of horses as much as boats — the assumption that a horse would steer for free off the
already-sent `PlayerInput` bitfield is **wrong**, and is kept here because it is the intuitive
answer.

The simulation is `lodestone_physics::vehicle` (pure functions, no ECS) driven by
`lodestone_ecs::vehicle` (four systems). `ClientAction::MoveVehicle` and `ClientAction::PaddleBoat`
had been encoded byte-exactly by the v770 adapter with **zero producers** the whole time — the
`ClientAction::SetFlying` shape — so the deliverable was measured by a packet reaching the queue,
not by a function computing a velocity.

```text
GameTick / TickSet::Physics
  … player_physics → cancel_flight_on_landing
  → charge_riding_jump        the jump ramp, and START_RIDING_JUMP on the release edge
  → tick_controlled_vehicle   one vehicle tick; writes the vehicle's Position/Rotation
  → pin_passenger_to_vehicle  (unchanged) the rider snaps onto the seat
  → send_fall_flying_command
  → send_vehicle_actions      MoveVehicle every tick, PaddleBoat every tick for a boat

NetIngest / IngestSet::Apply
  → apply_vehicle_moved       the server's *rejection* snap
```

Three orderings in there are behaviour, not style:

- **The vehicle tick is before the seat pin.** The pin reads the vehicle's `Position`, so moving the
  vehicle afterwards would leave the camera one tick behind the boat it is sitting in. This is the
  whole mechanism by which riding reaches pixels.
- **The jump charge is before the vehicle tick**, because vanilla's charge block is in
  `LocalPlayer.aiStep` and travel comes after it — the scale released this tick has to reach this
  tick's impulse.
- **`send_vehicle_actions` is in `TickSet::Physics`, not `TickSet::Send`.** Not a vanilla-ordering
  claim: it writes `ResMut<ActionQueue>`, which `lodestone_controller`'s two `Send` systems also
  write, and `lodestone-ecs` cannot name them to order against. An unordered second writer there
  fails the schedule's own ambiguity check. `send_fall_flying_command` is in this chain for exactly
  the same reason.

#### Boats: `AbstractBoat`, which never calls `travel`

A boat classifies its surroundings, applies buoyancy and per-status drag, turns and accelerates off
the raw key bits, and then calls `move(MoverType.SELF, getDeltaMovement())` directly. Every clause
is transcribed in `lodestone_physics::vehicle`; the four that are easy to get wrong:

| clause | value | what the plausible-wrong reading costs |
|---|---|---|
| gravity | `getDefaultGravity()` = **0.04** | the living `0.08` sinks a boat twice as fast |
| drag order | `floatBoat` drags **first**, `controlBoat` adds the impulse after | `(v + 0.04)·0.9` is 11% slower by tick five |
| turning bonus | `inputRight != inputLeft && !inputUp && !inputDown` — **three** conjuncts | one clause makes every forward turn `0.045` instead of `0.04` |
| yaw commit | `setYRot` runs **between** the bonus and the forward acceleration | applying the impulse along the old yaw makes a turning boat drift |

`invFriction` also decays `deltaRotation`, which is why a boat keeps turning after the key is
released on water (`0.9`) and stops almost immediately on land (`landFriction`, halved every tick a
player is aboard).

Rider yaw clamping landed with the boats, since `AbstractBoat.positionRider` is where it lives: the
rider is carried by `deltaRotation` and then clamped to ±105° of the boat's heading. The clamp is on
the **wrapped** difference, so 200° against a boat at 0° resolves to −105 and yields 255, not 105.

#### Land mounts: `travelRidden`, which is an ordinary mob travel

A mount is a `LivingEntity`, so it routes through `lodestone_physics::travel_in_air` — the same
integrator the player uses, which is what stops the two disagreeing about slabs and ice.
`AbstractHorse.getRiddenInput` rewrites the rider's keys (sideways **halved**, reverse
**quartered**), `getRiddenRotation` copies the rider's yaw and **halves** its pitch, and
`getRiddenSpeed` is the mount's own `minecraft:movement_speed` — never the rider's.

**`AbstractHorse`'s rule is not universal, and this was the one thing the brief for this work got
wrong** — "land mount (horse and friends) — the ridden-travel path" reads as one rule and it is
three. `MountRule` in `lodestone_physics::vehicle` carries them:

| rule | types | `getRiddenInput` | `getRiddenSpeed` |
|---|---|---|---|
| `Horse` | horse, donkey, mule, skeleton/zombie horse, llama | sideways ÷2, reverse ÷4 | the attribute |
| `Steered` | **pig**, **strider** | a constant `(0, 0, 1)` — the keys are ignored entirely | attribute × `0.225` / × `0.55` |
| `Camel` | camel | the horse rule | attribute **+ 0.1** while sprinting |

The `Steered` row is the one that matters: a pig read as a horse is strafeable, reversible and moves
**4.4× too fast**, and nothing at the call site would look wrong. Note the coincidence trap —
forward-only input gives `(0, 1)` under *both* rules, so only a strafe or a reverse separates them.
The camel bonus is additive, so `base + 0.1` and `base × 1.1` differ (0.3 versus 0.22 at a base of
0.2); both are asserted.

`ItemBasedSteering.boostFactor()` (the carrot/fungus-on-a-stick boost) is `1.0F` for every state
this client can observe, since `DATA_BOOST_TIME` is not decoded — so `1.0` is correct here rather
than a stand-in. `Strider.isSuffocating()`'s `0.35F` arm and `Camel.refuseToMove()` are unmodelled;
see the deferred list.

Step height is `1.0` while ridden, and the reason is `LivingEntity.maxUpStep`'s
`getControllingPassenger() instanceof Player ? Math.max(step, 1.0F) : step`. A horse's own
`STEP_HEIGHT` attribute is already `1.0`, so the `max` looks inert — it is not: it is what lets a
ridden **pig** or strider clear a whole block.

The jump ramp is client-side and has **two arms with a discontinuity**:
`jumpRidingScale = ticks * 0.1` while `ticks < 10`, then `0.8 + 2.0/(ticks − 9) * 0.1`. So it peaks
at exactly ten ticks (`1.0`) and then **decays** back toward `0.8` — "hold longer, jump higher" is
wrong past ten, and a fixture at five ticks cannot see the second arm at all. Ticks 9 and 11 both
give `0.9`, which is why neither is a discriminating input. The release edge sends
`PlayerCommand::StartRidingJump { boost: floor(scale * 100) }`; `STOP_RIDING_JUMP` exists on the
wire and the vanilla client **has no sender for it**, so this client does not send it either.

#### `ClientEvent::VehicleMoved` is a rejection, not a sync

The server sends `ClientboundMoveVehiclePacket` from exactly two places in
`ServerGamePacketListenerImpl.handleMoveVehicle` — "moved too quickly" and "moved wrongly / collided
with something new" — and both are followed by `vehicle.absSnapTo(old…)`. So receiving one means our
prediction was **refused**, and `apply_vehicle_moved` rebuilds the local motion at the server's
position with zero velocity rather than nudging it: the velocity is exactly what was refused.

**It folds in `ingest`, and the reasoning is worth keeping because the packet argues the other way.**
It carries no entity id, which reads like a local-player scalar. But the rule is about what a fold
*writes*, and what this writes is the vehicle's own `Position`/`Rotation` — per-entity components
`ingest::apply_entity_movement` owns the sole writer of. `session::Riding` supplies the subject,
exactly as the seat pin already resolves its vehicle from that same scalar. No session fold has (or
should have) a `Query<&mut Position>`.

---

## What is deferred

Nothing on this list is blocked on vehicle authority any more; each is blocked on its own thing.

- **The horse jump bar.** The HUD element itself is small, but `HudFrame` is built in
  `app.rs::WindowApp::redraw`, a contended choke-point file, and no jump-bar sprites are referenced
  anywhere in the tree yet.
- **`ClientEvent::MountScreenOpened`** (the horse inventory) and `PlayerCommand::OpenInventory` —
  both need the screen.
- **Minecarts are deliberately not simulated.** `VehicleFamily::for_type_path` returns `None` for
  the whole family, so a ridden minecart is left to the server. Its motion is rail-following
  (`NewMinecartBehavior`) and the server broadcasts it through `ClientboundMoveMinecartPacket`;
  predicting it as a land mount would fight that broadcast with plain gravity. This is a
  default-deny, so any type not named in that switch behaves the same way.
- **The minecart camera lerp fix-up** (`Camera.alignWithEntity`'s new-behaviour-minecart branch).
  Needs per-vehicle interpolation state the ECS does not hold. Symptom is camera stutter on a
  *moving* vehicle, not a wrong seat.
- **The full attachment table.** ~70 entity types declare a `PASSENGER` point in `EntityTypes`.
  `lodestone_ecs::riding` carries the rideable subset with citations; everything else falls through
  to vanilla's own `AT_HEIGHT` fallback computed from the real generated height, so an unlisted mount
  is a few centimetres high rather than wrong-shaped. The right home is a jar-generated table in
  `lodestone-data` beside `entity_dimensions`, from the same `Bootstrap.bootStrap()` walk the
  collision-shape and hardness censuses use.
- **`AbstractBoat.getGroundFriction`'s lily-pad exclusion.** Vanilla skips `LilyPadBlock` when
  averaging the friction under the hull; there is no lily-pad predicate on `CollisionView` and adding
  one means a new seam plus a shell-side implementation. Near-unobservable: `getStatus` consults the
  water checks first and a lily pad only exists on water, so the boat is classified `IN_WATER` before
  that function is reached.
- **A boat's `outOfControlTicks` ejection.** Tracked in `BoatState` for parity but not acted on: the
  60-tick `ejectPassengers` is a server decision and arrives back as `SET_PASSENGERS`.
- **`isStanding()` / `allowStandSliding`.** `AbstractHorse.getRiddenInput`'s first clause (a reared
  mount on the ground returns `Vec3.ZERO`) is implemented and its `standing` argument is currently
  always `false`, because rear-up is animation state with no wire field this client decodes. The
  clause is present rather than absent so wiring the state later is a call-site change.
- **The saddle gate.** Vanilla's charge block requires `jumpableVehicle()`, i.e. `canJump()`, i.e.
  `isSaddled()`. This client does not decode the equine saddle flag, so the gate is the weaker "we
  control a land mount" and a `START_RIDING_JUMP` can be sent for an unsaddled mount — which the
  server's own `AbstractHorse.onPlayerJump` discards under its `isSaddled()` check. The safe
  direction, named rather than left silent.
- **`getBlockJumpFactor` and `getJumpBoostPower`.** Passed explicitly as `1.0` / `0.0` into
  `horse_jump_impulse` so the vanilla formula stays intact; a honey block under a jumping horse and
  a Jump Boost effect on it are both unmodelled.
- **`ItemBasedSteering.boostFactor()`, `Strider.isSuffocating()` and `Camel.refuseToMove()`.** The
  boost factor is `1.0F` unless a carrot/fungus-on-a-stick boost is live, whose duration arrives as
  the `DATA_BOOST_TIME` entity-data field this client does not decode — so `1.0` is correct for every
  observable state rather than a placeholder. The strider's suffocating `0.35F` speed arm and the
  camel's sitting/pose-transition freeze both need decoded state too; a sitting camel currently
  walks, and `Camel.getRiddenRotation`'s matching freeze is absent with it.
- **A ridden mount with no reported `minecraft:movement_speed` does not walk.** `mount_speed` returns
  `None` and the whole tick declines, because a horse's speed is generated per instance
  (`AbstractHorse.generateSpeed`) and `attribute_value`'s own no-snapshot answer is the **generic
  mob** `0.7` — three times the fastest real horse. Declining is visible and diagnosable ("the horse
  turns but does not move"); guessing produces a mount that outruns every server correction.

Also not modelled, each a per-instance animation on top of the static seat point rather than a
different rule, and each needing state that is not on the wire: the horse's rear-up nudge
(`AbstractHorse.getPassengerAttachmentPoint`), the strider's walk bob (explicitly client-cosmetic —
the server returns plain `super`), the camel's sit/stand interpolation
(`SITTING_HEIGHT_DIFFERENCE = 1.43`), the minecart's villager-only lowered point, and the second boat
seat's `Animal` nudge. Boat cosmetics are likewise absent: the bubble-column column, the paddle sound
clock, and `Entity.baseTick`'s fire/portal/freezing pass.

---

## How to change it

- **Adding a vehicle's seat height:** `declared_passenger_attachment` in
  `crates/lodestone-ecs/src/riding.rs`. Cite the `EntityTypes.java` line. If it has a non-zero X
  or Z, return the whole `Vec3d` — the tail of that function is `y`-only.
- **A vehicle that overrides `getPassengerAttachmentPoint` entirely** (boat, camel) does not
  belong in that table; it needs its own arm in `passenger_attachment_local`, like the boat family
  has.
- **Anything that consumes "are we riding"** reads `lodestone_ecs::session::Riding`, not the
  `Vehicle` component: the local player carries no `Position`/`EntityKind` by design and is
  structurally excluded from the entity read-model.
- **Adding a serverbound riding action:** check for a *producer*, not only an encoder. All four
  outbound riding actions were encoded by the v770 adapter with zero producers before client
  authority landed — the `ClientAction::SetFlying` shape, which got us kicked for flying.
- **Adding a rideable family:** `lodestone_ecs::vehicle::VehicleFamily::for_type_path`. It is
  default-deny, so a type you do not name is simply left to the server. Add the *rule* in
  `lodestone_physics::vehicle` first; the ECS side is a dispatch arm.
- **Changing a boat clause:** `lodestone_physics::vehicle`, and the gates in
  `crates/lodestone-physics/tests/vehicle.rs` predict each value from the vanilla literals rather
  than from the code, so a wrong constant fails with both hypotheses printed.
- **Wiring the saddle flag, rear-up state, honey jump factor or Jump Boost:** all four are already
  parameters of the functions that need them (`ridden_input`'s `standing`, `horse_jump_impulse`'s
  `block_jump_factor`/`jump_boost_power`, and the gate in `charge_riding_jump`). Each is a call-site
  change, not a signature change.

### Gotchas

- **The `else if` in the `Riding` fold is load-bearing.** `SET_PASSENGERS` is broadcast for every
  vehicle in view distance, so "our id is not in this list" only means "we dismounted" when the
  list belongs to the vehicle we are in. Assigning `None` unconditionally ejects the player from a
  boat every time any mob anywhere is mounted. Pinned by
  `another_vehicles_passenger_list_does_not_dismount_us`, with its own negative control.
- **A respawn must clear `Riding` explicitly.** Vanilla builds a brand-new `ServerPlayer`
  (`PlayerList.respawn`) which is never a passenger, and no `SET_PASSENGERS` follows. We keep one
  long-lived entity, so without the clear the seat pin holds a respawned player at a vehicle
  nothing can free them from. Same shape as `Vitals::air`.
- **`EntityAttachments.getClamped` clamps, it does not throw** (`Mth.clamp(index, 0, size - 1)`).
  A third rider on a two-seat vehicle shares the last seat in
  vanilla too, so the clamp is agreement, not defensiveness. An `indexOf` miss (`-1`) feeding that
  clamp is seat 0, which is why the unresolvable case here reads as seat 0.
- **The yaw rotation looks inert and is not.** It is a no-op for every `y`-only attachment, which
  is most of them — but boats and llamas have a Z component, and getting the sign backwards
  mirrors a boat seat from bow to stern, which reads as a plausible seat facing the wrong way.
  `vehicle_yaw_rotates_a_horizontal_attachment_and_leaves_a_vertical_one_alone` feeds both cases
  for that reason.
- **`LocalPlayerPlugin` now `init_resource`s `EntityIndex`.** `pin_passenger_to_vehicle` resolves
  the vehicle id through it, and that plugin is usable without `IngestPlugin` (headless physics
  harness, `lodestone-controller`'s tests, the offline fixture world), where a `Res<EntityIndex>`
  would panic on the first `GameTick`. `init_resource` and not `insert_resource` so installing
  both plugins in either order leaves ingest's populated index alone.
- **`ControlledVehicle` is a resource and both `LocalPlayerPlugin` and `IngestPlugin` add it.**
  `ControlledVehiclePlugin` exists purely so the double-add is idempotent, for the same reason
  `EntityIndex` above needed care. A resource rather than a component on the vehicle because there
  is exactly one controlled vehicle per client and a stale component left on a vehicle we stopped
  riding would keep simulating it; the resource is rebuilt whenever `Riding` names a different id.
- **The boat's input bits come from `MovementIntent`'s signs, not from a key state.** `lodestone-ecs`
  cannot depend on `lodestone-controller`, so the four booleans are derived as `strafe > 0.0` etc. —
  the same derivation `send_player_input` already uses for the `PlayerInput` bitfield. The
  consequence is that this client's boat input is unscaled `±1` where vanilla's own `xxa`/`zza` carry
  `modifyInput`'s `0.98`; vanilla passes raw key presses to `AbstractBoat.setInput` so the boat half
  is exact, but a **land mount** reads the rider's scaled input and therefore accelerates about 2%
  faster here than in vanilla. That is this repo's existing input-model divergence, not a riding one.
- **Vanilla sends only `MovePlayerPacket.Rot` while a passenger; we still send the full move.**
  `send_move_action` is unconditional on ride state, which is safe because the server's move handler
  discards a passenger's reported position outright and keeps only the rotation. Gating it would be
  the same silent-dismount-break shape as gating `send_player_input`.

---

## Configuration

None. No feature flags, no constants to tune. `PASSENGER_HEIGHT_FACTOR` and
`PLAYER_VEHICLE_ATTACHMENT_Y` in `lodestone_ecs::riding`, and `BOAT_GRAVITY` /
`BOAT_FORWARD_ACCELERATION` / `RIDDEN_MOUNT_STEP_HEIGHT` / `BOAT_RIDER_YAW_CLAMP_DEGREES` in
`lodestone_physics::vehicle`, are vanilla values, not settings.

## Dependencies

- `lodestone-model` — `ClientEvent::{EntityPassengersChanged, VehicleMoved}`,
  `ClientAction::{InteractEntity, MoveVehicle, PaddleBoat}`, `PlayerCommand::StartRidingJump`, and
  the `VersionAdapter::entity_facts` seam that supplies the vehicle's base box height (which the
  boat's buoyancy divisor genuinely reads, so an unknown type declines rather than guessing).
- `lodestone-physics` — `Vec3d`, `PlayerState` as the thing the seat pin writes, `vehicle` for the
  ported rules, and `EntityMotion`/`move_entity`/`travel_in_air` as the shared integrator a vehicle
  and a player must not diverge on.
- `lodestone-entity` — `attribute::{attribute_value, movement_speed_key}` for a mount's own speed and
  jump strength.
- `lodestone-ecs` — `entity::{Passengers, Vehicle, EntityIndex, Attributes}`, `session::Riding`,
  `player::pin_passenger_to_vehicle`, `riding`, `vehicle`.
- `lodestone-shell` — `sim::Sim::{use_item_live, interact_entity}` (mount), and `camera_rig`
  only as the reader that gets the seat for free. **No shell edit was needed for authority**: the
  systems ride `LocalPlayerPlugin`'s existing chain and reuse the `PlayerCollision` view the shell
  already refreshes per tick.
- `lodestone-controller` — `send_player_input`, unchanged, which is what makes dismount work.

## See also

- [`docs/creative-flight.md`](./creative-flight.md) — the other place a server-granted
  local-player scalar drives physics, and the island that produced the `SetFlying` lesson.
- [`docs/entity-rendering.md`](./entity-rendering.md) — the per-entity ingest set this adds two
  components to.
