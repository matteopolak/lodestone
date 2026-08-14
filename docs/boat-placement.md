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

* **The `USE_ITEM_ON` → `USE_ITEM` fall-through is now wired, and it is *conditional*.** This
  was the gap that made a boat aimed at land do nothing while a boat aimed at open water past
  block reach worked (the crosshair ray ignores fluids, misses, and the no-target branch
  already fell through). `USE_ITEM` is the only way `BoatItem.use` ever runs, because the
  server's `ServerPlayerGameMode.useItemOn` never reaches `Item.use`.

  The correction worth carrying: **vanilla's `case BLOCK` is not a `break` like `case ENTITY`'s.**
  `Minecraft.startUseItem` `return`s on `InteractionResult.Success` *and* on
  `InteractionResult.Fail`, and reaches `gameMode.useItem` only for a non-consuming result. So
  `Sim::use_item_live` falls through only when its `UseOnDecision` is `Nothing` **and** the held
  item is not a placeable block — the shell's stand-in for "this item has no `useOn` of its own",
  which is what `MultiPlayerGameMode.performUseItemOn` would have answered `PASS` for. An
  unconditional call would have been a real defect, not merely over-eager: a **carved pumpkin**
  is both a placeable block and `equippable`, so a refused placement would equip it onto the
  player's head. `sim::tests::{use_item_live_falls_through_to_generic_use_with_a_block_targeted,
  a_placeable_item_on_a_block_does_not_also_send_the_generic_use}` are the two arms, and each
  fails under the other's neuter.

  **The fix is worth more than boats.** Every `USE_ITEM` outcome — `apply_use_item`'s
  `Consuming` (eating, drinking), `Equipped` (equip-on-use), `Draw` (bow/crossbow) and this
  module's boat arm — was reachable only with the crosshair off a block.
* **A placed boat used to be invisible, for an unrelated reason.**
  `lodestone_render::entity::model_for_type` resolves an entity-type path against the
  `entity_models` corpus, which names the four rigs by vanilla *class* (`boat`, `chest_boat`,
  `raft`, `chest_raft`) while the registry has twenty *types*. With no alias
  `model_for_type("oak_boat")` was `None` and `resolve_animated` skipped the entity, so the
  server streamed a boat nobody could see. `entity::boat_model_name` maps the twenty onto the
  four; the traps are that `_chest_boat` must be tested before `_boat` (every chest boat ends
  with both), that `bamboo_raft`/`bamboo_chest_raft` carry no `_boat` suffix at all, and that
  the corpus lookup must run **before** the suffix rules or the literal corpus name
  `"chest_boat"` resolves to the plain `boat` rig.
  `crates/lodestone-render/tests/boat_model_resolution.rs` gates all three.
* **A placed boat now renders 1.126 blocks too high, and it is a rendering bug, not a
  placement one.** Traced end to end while chasing *"it's floating way above the water and i
  cant enter it"*: the raytrace's hit point (`crate::boat::clip`), the position stored on
  `MobSim::spawn_vehicle`, the `ADD_ENTITY` bytes `V770ServerProtocol` encodes, and the
  `Position` component `lodestone_ecs::ingest::apply_entity_spawn` writes on the client are all
  the identical, correct value — verified by reading each hop, not assumed. The gap is one
  step later, in `lodestone_render::entity`: `EntityModelSet::resolve_animated` (called for
  every entity from `crate::gpu::entity_passes` in `lodestone-shell`, boats included, since a
  boat matches no `projectile_pitch_offset_deg` name) always places through
  `dying_entity_model_matrix`, which lifts by the `LivingEntityRenderer`-style
  `MODEL_FEET_OFFSET` (`1.501`). Vanilla's boat never goes through `LivingEntityRenderer` at
  all — `AbstractBoatRenderer.submit` does its own `poseStack.translate(0.0F, 0.375F, 0.0F)`
  before the yaw rotate and the `scale(-1, -1, 1)` flip, with no `1.501` lift anywhere. Since a
  Y-axis rotation cannot touch the Y component, the two placements differ by exactly one
  constant: the boat model draws `1.501 − 0.375 = 1.126` blocks above its real position.
  The interaction ray does not share this bug — `Sim::update_entity_target` in
  `lodestone-shell/src/sim/step.rs` builds its hitbox from the raw `Position` component and
  `VersionData::entity_facts` dimensions, the same correct value the server sent — so the
  boat's actual clickable box sits at the water, 1.126 blocks below where the model is drawn.
  A player aiming at what they see is aiming at empty air, which is the second half of the
  report. Fixing this needs a boat-specific placement matrix in `lodestone-render` (the same
  shape `projectile_model_matrix` already is for arrows/tridents), not a change anywhere in
  this doc's own module or in `lodestone-entity`; `boat_model_resolution.rs`'s existing gates
  only check *which* rig resolves, never its placement, so nothing caught this.
* **Every wood species draws the oak hull.** Each corpus entry carries a single
  `EntityTexture::Fixed` (`entity/boat/oak`, `entity/chest_boat/oak`, `entity/boat/bamboo`,
  `entity/chest_boat/bamboo`), and vanilla's species *is* a texture rather than geometry, so a
  spruce boat is oak-coloured. Fixing it means a variant texture on the four `lodestone-assets`
  entries, not a change to the name mapping.
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
decode. The client half is `lodestone_ecs::{session, riding, vehicle}` for the mount and the
local vehicle simulation, `lodestone_shell::sim::Sim::use_item_live` for the `USE_ITEM` send,
and `lodestone_render::entity::{model_for_type, boat_model_name}` for the rig.
