# Riding

Getting into a boat, minecart or saddled animal, having the camera sit on the seat, and
getting back out. Tier 1 item 8 of [`backlog.md`](./backlog.md).

**Scope landed:** mount, seat position, camera, `on_ground` override, dismount.
**Scope deferred:** the vehicle *moving*. See [What is deferred](#what-is-deferred) — the reason
is one sentence long and it is not "ran out of time".

---

## What it is

`ClientboundSetPassengersPacket` tells the client which entities are riding which. Before this
change it was a **complete island**: decoded at `crates/protocol/v770/src/adapter.rs`'s
`SET_PASSENGERS` arm, round-tripped by `crates/protocol/v770/tests/entity_events.rs`, and a
tree-wide grep for `EntityPassengersChanged` returned exactly **four** hits — the decode, those
two tests, and the `ClientEvent` variant. Zero consumers, no arm in either `handles_event`
router, and no producer of any of the serverbound riding actions either.

That was true of all six riding-shaped items on the wire. Checked with a working detector (the
same grep found 19 hits for a known-wired `ClientAction::SwingArm`):

| item | direction | status before |
|---|---|---|
| `ClientEvent::EntityPassengersChanged` | in | decoded, 0 consumers |
| `ClientEvent::VehicleMoved` | in | decoded, 0 consumers |
| `ClientEvent::MountScreenOpened` | in | decoded, 0 consumers |
| `ClientAction::MoveVehicle` | out | encoded, 0 producers |
| `ClientAction::PaddleBoat` | out | encoded, 0 producers |
| `PlayerCommand::{StartRidingJump, StopRidingJump}` | out | encoded, 0 producers |

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

`Entity.positionRider` (`Entity.java:2399-2403`), `getPassengerRidingPosition` (`:2412-2414`),
`getDefaultPassengerAttachmentPoint` (`:2420-2423`), `getVehicleAttachmentPoint` (`:2408-2410`),
rotation at `EntityAttachments.java:78-80` (`point.yRot(-rotY * π/180)`, i.e. `Vec3.yRot`,
`world/phys/Vec3.java:241-248`).

Two constants worth stating because the plausible neighbour is wrong:

- **The `PASSENGER` fallback is `(0, height, 0)` — `height × 1.0`**, the top of the vehicle's box
  (`EntityAttachment.java:7` `PASSENGER(Fallback.AT_HEIGHT)`, `:25`). The tempting `× 0.85` is a
  *different* quantity, `EntityDimensions.defaultEyeHeight` (`EntityDimensions.java:11-13`).
- **The player's `VEHICLE` attachment is not zero.** `Avatar.DEFAULT_VEHICLE_ATTACHMENT =
  Vec3(0.0, 0.6, 0.0)` (`Avatar.java:17`, wired at `EntityTypes.java:1143`) and it is
  *subtracted*, lowering the rider 0.6 below the seat point. Dropping it floats the player above
  every saddle — the single largest error available in this function.

Declared per-type points, cited in the source: minecart family `0.1875`
(`EntityTypes.java:667`, repeated verbatim for chest/furnace/hopper/tnt/spawner/command-block),
horse `1.44375` (`:531`), donkey `1.1125` (`:338`), mule `1.2125` (`:675`), skeleton/zombie horse
`1.31875` (`:852`, `:1104`), pig `0.86875` (`:754`), llama `(0, 1.37, -0.3)` (`:620`, `:973`).

**The boat family bypasses the attachment table entirely.** `AbstractBoat`
(`vehicle/boat/AbstractBoat.java:135-152`) builds the point from `rideHeight(dimensions)` and a
**Z** offset. `rideHeight` is `height / 3` for boats and chest boats (`Boat.java:16`,
`ChestBoat.java:16`) and `height × 0.8888889` for rafts (`Raft.java:16`, `ChestRaft.java:16`) —
`0.1875` vs `0.5` at the shared `sized(1.375, 0.5625)` box, nearly a factor of three, so one rule
for both is visible. The Z offset is `0.0` (`:613`), `0.15` for chest boats
(`AbstractChestBoat.java:44`), `-0.6` for any seat past the first (`:143`). Note vanilla names the
field `getSinglePassengerXOffset` and applies it to Z; the name is kept so the citation matches.

### The camera, which needed no code

26.2's `Camera.alignWithEntity` (`client-src/net/minecraft/client/Camera.java:246-264`) has **no
`isPassenger()` branch** except a lerp fix-up for new-behaviour minecarts (`:247-256`), and riding
changes neither the pose nor the eye height (`Player.updatePlayerPose`, `Player.java:343-357`, has
no riding case; there is no `SITTING` pose, so a mounted player keeps
`Avatar.DEFAULT_EYE_HEIGHT = 1.62`, `Avatar.java:16`).

So the whole mechanism is `pin_passenger_to_vehicle` moving the player's **feet**.
`Sim::camera` reads `PhysicsState` exactly as it does for a walking player, and the eye, the
block-target ray origin and the audio listener all move together. Adding a passenger branch to
`camera_rig.rs` would double-apply the attachment.

### Where the pin sits in the tick, and why

`TickSet::Physics`, chained **last**: `apply_creative_flight_input → player_physics →
cancel_flight_on_landing → pin_passenger_to_vehicle`.

Vanilla runs a passenger's whole tick — travel included, with the same `xxa`/`zza` the vehicle
reads — and only then overwrites the position: `rideTick()` is
`setDeltaMovement(ZERO); this.tick(); vehicle.positionRider(this)` (`Entity.java:2385-2390`), and
`LivingEntity.aiStep` still reaches `travel(input)` for a passenger because
`canSimulateMovement()` is `isLocalInstanceAuthoritative()`, true for the local player
(`LivingEntity.java:3127-3131`, `Entity.java:3594-3609`, `Player.java:1281`). So a mounted
player's one tick of drift out of the seat really does happen and really is thrown away. The one
divergence: velocity is zeroed at the *end* rather than the top, which differs only on the first
tick after mounting and is discarded by that same tick's snap.

### `on_ground` while riding — and the reason usually given for it is wrong

`Player.java:232-236` forces `onGround = false` for a spectator or passenger, unconditionally,
before anything else in `tick()`. This closes the `spectator_or_passenger_note` contract that had
sat in `lodestone-physics/tests/on_ground.rs` since before riding existed.

`PlayerState::on_ground`'s docs frame the flag as a wire contract policed by the server's
`aboveGroundTickCount` / `multiplayer.disconnect.flying` counter, which would make this
kick-avoidance. **It is not, and the check was worth running:** the server's float check is
explicitly `&& !this.player.isPassenger()` (`ServerGamePacketListenerImpl.java:323`), and its
move handler discards a passenger's reported position outright, keeping only the rotation
(`:1086-1088`). A mounted client cannot be kicked over this flag. The override is for the
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
STOP_RIDING_JUMP, OPEN_INVENTORY, START_FALL_FLYING`
(`ServerboundPlayerCommandPacket.java:60-68`). Dismount is inferred server-side from the sneak
bit of `ServerboundPlayerInputPacket`: `handlePlayerInput` → `setShiftKeyDown`
(`ServerGamePacketListenerImpl.java:421-428`), then `Player.rideTick`'s
`if (!isClientSide() && wantsToStopRiding() && isPassenger()) stopRiding()`
(`Player.java:432-439`), with `wantsToStopRiding()` = `isShiftKeyDown()` (`:296-298`).

`lodestone_controller::ecs::send_player_input` already sends that bit, edge-triggered, and is
unconditional on ride state — so dismount works with no new code, and the vanilla client likewise
**does not predict it** (note the `!isClientSide()` guard). If a future change ever gates
`send_player_input` on riding, dismount breaks silently; that is the thing to watch.

---

## What is deferred

**Every vehicle is client-authoritative while a player rides it, so none of them move until we
port their physics.** `Entity.isClientAuthoritative()` delegates to the controlling passenger
(`Entity.java:3603-3606`) and `Player.isClientAuthoritative()` is `true`
(`Player.java:1275-1277`), so the server's `travelRidden` takes the
`setDeltaMovement(Vec3.ZERO)` branch (`LivingEntity.java:2630-2639`) and simply accepts
`ServerboundMoveVehiclePacket`. This is true of horses as much as boats — the earlier assumption
that a horse would steer for free off the already-sent `PlayerInput` bitfield is **wrong**, and
worth recording because it is the intuitive answer.

So, deferred, each blocked on that one thing:

- **Boat steering.** `AbstractBoat.controlBoat` + `floatBoat` (`AbstractBoat.java:578-609`:
  `deltaRotation ∓1` per turn key, `acceleration += 0.04` forward, `-= 0.005` back, `+= 0.005`
  turning only) plus buoyancy, then `ClientAction::MoveVehicle` once per tick.
- **`ClientAction::PaddleBoat`.** Trivial once boat input state exists:
  `left = inputRight && !inputLeft || inputUp`, `right = inputLeft && !inputRight || inputUp`
  (`AbstractBoat.java:608`), sent **every tick** while we own the boat (`:239`), not on change.
- **Horse jump.** The charge ramp is client-side and exact:
  `jumpRidingScale = ticks * 0.1` for ticks 1..9, then `0.8 + (2.0 / (ticks - 9)) * 0.1`
  (`client-src/.../LocalPlayer.java:882-908`), released on the jump key's falling edge as
  `PlayerCommand::StartRidingJump { boost: floor(scale * 100) }` (`:390-393`). Note
  `STOP_RIDING_JUMP` exists on the wire and the vanilla client **never sends it**. The server's
  `handleStartJump` is cosmetic only (`AbstractHorse.java:897-902`); the impulse is simulated by
  the authoritative client, so this is blocked on the same vehicle physics.
- **The horse jump bar.** The HUD element itself is a small addition, but `HudFrame` is built at
  `crates/lodestone-shell/src/app.rs:1857` and `app.rs` was held by another agent during this
  change. No jump-bar sprites are referenced anywhere in the tree yet either.
- **`ClientEvent::VehicleMoved`.** The clientbound correction for a vehicle we are authoritative
  over — it cannot fire until we are.
- **`ClientEvent::MountScreenOpened`** (the horse inventory) and `PlayerCommand::OpenInventory`.
- **Rider yaw clamping.** A boat clamps its passenger's yaw to ±105° of the boat's heading and
  carries it as the boat turns (`AbstractBoat.java:621-631`, `:671-683`, hooked from
  `Entity.turn` at `Entity.java:498-508`). Free look while seated is currently unrestricted.
- **The minecart camera lerp fix-up** (`Camera.java:247-256`). Needs per-vehicle interpolation
  state the ECS does not hold. Symptom is camera stutter on a *moving* vehicle, not a wrong seat.
- **The full attachment table.** ~70 entity types declare a `PASSENGER` point in
  `EntityTypes.java`. `lodestone_ecs::riding` carries the rideable subset with citations;
  everything else falls through to vanilla's own `AT_HEIGHT` fallback computed from the real
  generated height, so an unlisted mount is a few centimetres high rather than wrong-shaped. The
  right home is a jar-generated table in `lodestone-data` beside `entity_dimensions`, from the
  same `Bootstrap.bootStrap()` walk the collision-shape and hardness censuses use.

Also not modelled, each a per-instance animation on top of the static point rather than a
different rule, and each needing state that is not on the wire: the horse's rear-up nudge
(`AbstractHorse.java:1041-1044`), the strider's walk bob (`Strider.java:190-200`, explicitly
client-cosmetic), the camel's sit/stand interpolation (`Camel.java:495-512`,
`SITTING_HEIGHT_DIFFERENCE = 1.43`), the minecart's villager-only lowered point
(`AbstractMinecart.java:178-182`), and the second boat seat's `Animal` nudge (`:146-148`).

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
  outbound riding actions were encoded by the v770 adapter with zero producers before this change
  — the `ClientAction::SetFlying` shape, which got us kicked for flying.

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
- **`EntityAttachments.getClamped` clamps, it does not throw** (`EntityAttachments.java:74`,
  `Mth.clamp(index, 0, size - 1)`). A third rider on a two-seat vehicle shares the last seat in
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

---

## Configuration

None. No feature flags, no constants to tune. `PASSENGER_HEIGHT_FACTOR` and
`PLAYER_VEHICLE_ATTACHMENT_Y` in `lodestone_ecs::riding` are vanilla values, not settings.

## Dependencies

- `lodestone-model` — `ClientEvent::EntityPassengersChanged`, `ClientAction::InteractEntity`,
  and the `VersionAdapter::entity_facts` seam that supplies the vehicle's base box height.
- `lodestone-physics` — `Vec3d`, and `PlayerState` as the thing the seat pin writes.
- `lodestone-ecs` — `entity::{Passengers, Vehicle, EntityIndex}`, `session::Riding`,
  `player::pin_passenger_to_vehicle`, `riding`.
- `lodestone-shell` — `sim::Sim::{use_item_live, interact_entity}` (mount), and `camera_rig`
  only as the reader that gets the seat for free.
- `lodestone-controller` — `send_player_input`, unchanged, which is what makes dismount work.

## See also

- [`docs/creative-flight.md`](./creative-flight.md) — the other place a server-granted
  local-player scalar drives physics, and the island that produced the `SetFlying` lesson.
- [`docs/entity-rendering.md`](./entity-rendering.md) — the per-entity ingest set this adds two
  components to.
