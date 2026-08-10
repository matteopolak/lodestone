# Boats: placing, boarding and steering

## What it is

The server-side half of "put a boat down, get in it, row it". It covers
`BoatItem.use`'s raytrace (`lodestone_server::boat`), a rideable-vehicle registry that is
deliberately **not** the mob simulation (`lodestone_server::mobs`' `vehicles` field), the
`SET_PASSENGERS` producer that makes a client believe it is aboard, and the acceptance of
the client's authoritative `MoveVehicle`.

Before this, `USE_ITEM` with a boat in hand reached `apply_use_item`, fell past the eating
and equip arms and returned `Nothing` — the owner's report was *"i cant place a boat down
(and probably other things). i think this i related to 'use item'"*, and that was exactly
right.

## How it works

```
USE_ITEM ─ boat::apply_boat_item ─ clip (DDA over outline + fluid shapes)
                                 └ MobSim::spawn_vehicle ─ snapshots() ─ ADD_ENTITY

INTERACT ─ MobSim::mount_vehicle ─ ServerProtocol::encode_set_passengers
                                                   └ client: session::Riding
                                                       └ ecs::vehicle::tick_controlled_vehicle

MOVE_VEHICLE ─ MobSim::apply_vehicle_move  (the client owns the boat; we write it down)

world tick  ─ MobSim::tick_vehicles        (UNRIDDEN boats only: float_boat + move_entity)
```

### The raytrace

`BoatItem.use` does not place relative to a clicked block. It runs its own clip and puts the
boat at the exact **hit point**:

* from `player.getEyePosition()` to `eye + view * blockInteractionRange()`;
* `ClipContext.Block.OUTLINE` — the *outline* shape, so grass, flowers and lily pads are
  hittable and a boat rests on them;
* `ClipContext.Fluid.ANY` — fluids are hit, with shape `box(0, 0, 0, 1, height, 1)` where
  `height` is `1.0` if the same fluid is directly above and `amount / 9.0` otherwise. A boat
  aimed at open water therefore lands at `y + 8/9`, which is what makes one rule cover both
  water and land;
* the yaw is the **player's**, not the clicked face's;
* a `MISS` is `PASS` and an overlapping hull is `FAIL` — neither consumes the item.

Reach is `4.5`, plus `0.5` in creative
(`ServerPlayer.CREATIVE_BLOCK_INTERACTION_RANGE_MODIFIER`).

Two numbers that bit on first run and are worth knowing before predicting anything here:
`getOwnHeight()` is an **`f32`** divide, so the water surface is `0.88888889…` and not the
`f64` value of 8/9; and `short_grass`' outline is **13/16** tall, not the plausible 0.8.

### Boats are not mobs, and the registry says so

`MobSim::spawn_species` resolves attributes, a goal set, a pathfinding shape and a mob
category. A boat has none of those, so routing one through it produces a boat that *wanders*.
`TrackedVehicle` is its own registry entry beside the falling-block and orb maps, carrying
position, yaw, `BoatState` and the controlling passenger.

The passenger is on the **vehicle**, not on the connection, and that is load-bearing:
`tick_vehicles` must be able to ask "is anyone aboard" from inside the tick, because a ridden
boat must not be simulated server-side at all. `Entity.isClientAuthoritative()` delegates to
the controlling passenger and `Player.isClientAuthoritative()` is `true`, so vanilla's own
`travelRidden` zeroes the delta and the server only accepts what the rider reports. Ticking
the hull as well is what makes a boat fight the player.

The float pass uses `lodestone_physics::vehicle::{boat_status, float_boat}` and
`move_entity` — literally the functions the client's `tick_controlled_vehicle` calls, so a
boat cannot behave one way while watched and another while ridden.

### Boarding

`AbstractBoat.interact` → `player.startRiding(this)`. Handled **ahead of**
`MobSim::interact`, whose whole chain is `Animal.mobInteract` and has no arm for a boat.
`SET_PASSENGERS` carries the vehicle's *whole* passenger list, always — a dismount is the
same packet with an empty list.

This is also the first reader of `ServerBound::InteractEntity::using_secondary_action`:
`player.isSecondaryUseActive()` means sneak-clicking a boat must not board it.

## How to change it

* **A new boat species** needs nothing: the item id **is** the entity type id for all twenty
  registrations, validated against `lodestone_data::entity_types`. Add the item to
  `JAR_BOAT_ITEMS` so the count gate stays honest.
* **Boat metadata** (paddles, bubble time, hurt/damage) is not sent. Index 18 has 37
  claimants in the committed `EntityDataIndexOracle` dump, four of them `BYTE`, so a producer
  must know the species and no census column separates them — and the rider's own client
  animates its paddles from its local `PaddleBoat` simulation anyway. When a second player
  needs to watch someone row, add a `MetadataField` per accessor and check the guard against
  the dump, as `MetadataField::Item` and `ExperienceOrbValue` did.
* **A minecart must not simply join the `vehicles` map.** `tick_vehicles` applies
  `float_boat` to every entry, and a minecart's motion is rail-following
  (`NewMinecartBehavior`) broadcast through `ClientboundMoveMinecartPacket`. It needs a
  family tag on `TrackedVehicle` and its own tick arm; the client already default-denies
  minecart simulation (`lodestone_ecs::vehicle::VehicleFamily::for_type_path`) for the same
  reason.

## Gotchas, and the gaps

* **The client does not fall through from `USE_ITEM_ON` to `USE_ITEM`.** Vanilla's
  `Minecraft.startUseItem` sends the `use_item_on` packet, sees a non-consuming result, and
  then sends `use_item` unconditionally — which is the *only* way `BoatItem.use` ever runs,
  because the server's `ServerPlayerGameMode.useItemOn` never reaches `Item.use`. Our shell's
  `Sim::use_item_live` returns after the block branch, so a boat aimed at **land or shallow
  water** sends nothing this module can see. Aimed at water whose bed is out of block reach
  it works today: the crosshair ray ignores fluids, misses, and the existing no-target branch
  already calls `use_item_generic`. The fix is one call in the shell's block branch, matching
  what its own entity and no-target branches already do.
* **No dismount packet has a producer.** There is no shift-to-get-out send in the shell, so a
  rider stays aboard until they disconnect. `MobSim::dismount_rider` is ready for it.
* **A vanished rider is evicted by roster diff, not by a disconnect hook.** `tick_vehicles`
  clears a rider absent from `MobSim::set_players`, guarded on the roster being non-empty —
  `set_players` is position-driven and legitimately empty before anyone has moved, so an
  empty roster means "no information", not "nobody is connected". Without that guard the
  eviction fires the instant a player boards.
* **One seat, not two.** `getMaxPassengers()` is 2 for a boat and 1 for a chest boat; this
  seats one for every type. Seating two players at the same attachment would be worse than
  refusing.
* **The obstruction test ignores entities.** `level.noCollision(boat, box)` also excludes
  other entities, and there is no world-wide entity-box query at this seam, so a boat can be
  placed overlapping a mob. `BoatItem.use`'s `EntitySelector.CAN_BE_PICKED` sweep — which is
  what stops you placing a boat while standing inside one — is missing for the same reason.
* **No `MoveVehicle` rejection is sent.** Vanilla answers "moved too quickly" and "moved
  wrongly" with `absSnapTo` plus a clientbound `MOVE_VEHICLE`; nothing here does, so
  `ServerProtocol` has no clientbound producer. The client already handles one if it arrives
  (`lodestone_ecs::vehicle::apply_vehicle_moved`).
* **`PADDLE_BOAT` still decodes to `Ignored`.** It only drives the paddle-swing animation for
  *other* viewers, which needs the metadata above first.
* **Chest boats carry no chest.** They place, board and steer as a plain boat; there is no
  container behind them.
* **A boat is not persisted.** The vehicle registry is not part of
  `crate::entity_storage`'s round trip, so boats do not survive a restart.

## The rest of the entity-placing family, and what each still needs

| item | vanilla hook | why it is not here |
|---|---|---|
| minecarts | `MinecartItem.useOn` (needs a rail beneath) | rail-following motion and `MOVE_MINECART`; see above |
| armour stands | `ArmorStandItem.useOn` (yaw snapped to 45°) | a `LivingEntity` with no AI and an equipment model; neither registry fits |
| buckets of fish | `MobBucketItem.emptyContents` | spawns a real mob with a bucket-persisted variant, so it belongs on the `spawn_species` path plus NBT |
| end crystals | `EndCrystalItem.useOn` (bedrock/obsidian only) | needs the explosion-on-damage behaviour, or it is decorative |

Spawn eggs are already wired — see `docs/spawn-eggs.md` and `crate::spawn_egg`; that arm is
the precedent for where an item's own `useOn` goes, and this one is the precedent for `use`.

## Configuration

None. Reach follows the game mode; nothing is behind a feature flag or a game rule.

## Dependencies

`lodestone_data::{outline_shapes, collision_shapes, entity_types, block_states}` for the clip
and the obstruction test, `lodestone_server::fluid::fluid_state_of` for the surface height,
`lodestone_physics::vehicle` for `BoatState`/`boat_status`/`float_boat` and the shared
constants, and `lodestone_v770`'s `ServerProtocol` for `SET_PASSENGERS` and the `MOVE_VEHICLE`
decode. The client half is `lodestone_ecs::{session, riding, vehicle}` and needs no change
beyond the `USE_ITEM` fall-through noted above.
