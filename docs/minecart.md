# Minecarts

## What it is

The `minecraft:minecart` entity family — plain, chest, hopper, furnace and
TNT minecarts — plus rail-following physics, riding, and the placement
producers (item use on a rail, and a dispenser). A port of
`AbstractMinecart`/`OldMinecartBehavior` and the three subclass overrides
(`Minecart`, `MinecartFurnace`, `MinecartTNT`), living in
`crates/lodestone-server/src/mobs/minecart.rs`.

Before this, minecarts did not exist anywhere in the crate: no entity, no
rail following, no riding, and a dispenser loaded with any minecart item
plain-tossed it instead of placing one (`crate::redstone_dispenser`'s own
module doc named the gap).

## How it works

**The entity.** `TrackedMinecart` (`mobs/mod.rs`) is a plain `HashMap<i32,
TrackedMinecart>` sidecar on `MobSim`, the same shape `TrackedVehicle`
(boats) and `TrackedTnt` use — no AI, no attributes, no `SimMob` goal
machinery, because rail-following is not a goal.

**Which behaviour is ported.** 26.2 ships two physics models behind
`AbstractMinecart.useExperimentalMovement`
(`level.enabledFeatures().contains(FeatureFlags.MINECART_IMPROVEMENTS)`), and
that feature flag is its own opt-in datapack, not part of any vanilla
world's default feature set. `OldMinecartBehavior` — the classic
rail-follow model — is what an ordinary world actually runs, and it is the
one ported here, not `NewMinecartBehavior`.

**Motion.** `MobSim::tick_minecarts` transcribes `AbstractMinecart.tick`/
`OldMinecartBehavior.tick` in vanilla's own order: gravity, then either
`move_along_track` (on a rail cell) or `come_off_track` (not on one), then
the yaw/flip bookkeeping that keeps a cart's sprite pointed the way it is
actually travelling. `move_along_track` is `OldMinecartBehavior
.moveAlongTrack` in full: the powered-rail boost/brake read, the
ascending-rail slide impulse, the exit-pair geometry that snaps a cart's
`(x, z)` onto the rail's own centreline for all ten `RailShape` values (six
straight, four curved), the `move_entity` collision nudge (the same shared
integrator `tick_vehicles`/`tick_tnt` already use), the hill-speed
adjustment, and the powered-rail end-of-function boost or two-conductor
brake nudge.

**Riding.** Only a plain minecart is rideable
(`AbstractMinecart.isRideable()`). `MobSim::mount_minecart`/
`dismount_minecart_rider` mirror `mount_vehicle`/`dismount_rider`'s shape
for boats, and boarding sends the same `SET_PASSENGERS` packet. Unlike a
boat, a ridden minecart is **not** client-authoritative in the old
behaviour model — the server ticks every minecart identically whether
ridden or not, so no `MoveVehicle`-style client report is needed at all.
Seat positioning is already fully wired client-side:
`lodestone_ecs::riding::declared_passenger_attachment` already carries a
`0.1875`-height entry for every minecart type, so a mounted player's seat
math is correct the moment `SET_PASSENGERS` lands — only the cart's own
mesh is missing (see "What does not draw yet" below).

**Furnace minecart.** Burns coal/charcoal (`MinecartKind::is_furnace`,
`add_minecart_fuel`) for `FURNACE_FUEL_TICKS_PER_ITEM` (3600) ticks each, up
to `FURNACE_MAX_FUEL_TICKS` (32000), and self-propels via a stored push
vector re-aimed toward the current direction of travel each tick
(`calculate_new_push_along`). `MetadataField::MinecartFuel` streams whether
it is currently lit, for the client's smoke particle.

**TNT minecart.** Primed only by an activator rail
(`apply_activation`/`MinecartKind::Tnt`), counting down
`TNT_MINECART_FUSE` (80) ticks before detonating through the **same**
`MobSim::explode`/`pending_detonations` pipeline primed TNT
(`crate::mobs::tnt`) already uses — a third producer into an existing
pipeline, not a new consumer.

**Placement, two producers:**

| producer | where | rail required |
|---|---|---|
| right-click a rail with a minecart item | `crate::server::apply_use_item_on` | yes — `FAIL` (no placement, no fallback) off a rail |
| a dispenser loaded with a minecart item | `crate::redstone_dispenser::minecart_dispense`, wired in `tick.rs`'s `TICK_DISPENSER_FIRE` arm | a rail directly ahead, or air over a rail one cell down; otherwise falls back to a plain toss |

Powered, activator and detector rails' own `POWERED` machinery
(`crate::redstone_rail`, `crate::redstone::is_detector_rail`) already
existed — this feature is a second *reader* of powered/activator rail
state, not a second writer.

## How to change it, and the gotchas

- **Every rail-following constant is a record value, not a guess** —
  `MAX_SPEED_LAND`/`MAX_SPEED_WATER` (0.4/0.2), the powered-rail boost
  (0.06), the slide impulse (0.0078125), the two slowdown factors
  (0.997 ridden / 0.96 unridden). See `minecart.rs`'s own constant docs for
  each one's exact vanilla source.
- **`RailShape` here is not `crate::redstone_rail::RailShape`.** The
  powered/activator-rail module's own `RailShape` is deliberately narrowed
  to the six straight shapes those two blocks can hold; a minecart also has
  to follow the four curves a plain `minecraft:rail` (or a detector rail)
  can be in, so `mobs::minecart::RailShape` is the full ten-value enum,
  parsed independently from the same raw `shape` state property.
- **`is_rail_block`/`rail_shape`/`placement_position`/`MinecartKind` are
  `pub(crate)`** specifically so `crate::redstone_dispenser` and
  `crate::server` can recognise a rail and derive a spawn position without
  this module reaching back into either of them.

### What is deliberately simplified

- **No rider movement nudge.** Vanilla reads the controlling player's
  `getLastClientMoveIntent()` and adds a tiny push when the cart's own
  speed is near zero — this is what lets a player free a stalled cart by
  walking against it. Wiring a live per-tick input vector from a connection
  into `MobSim` is a materially separate seam this crate does not have
  today; cut, not attempted.
- **No entity pushing/auto-mount** (`pushAndPickupEntities`). Mounting is
  explicit right-click only, as it already is for boats.
- **No fluid-current push, no `applyEffectsFromBlocks`** — the same two
  cuts `crate::mobs::tnt`'s own module doc makes for primed TNT, for the
  identical reasons.
- **Detector rail has no *producer* here.** The `POWERED` *read* already
  existed; nothing yet sets a detector rail's `POWERED` when a minecart
  sits on it — that needs a live `ChunkSource` and scheduled-tick queue
  this sim's spawn-time `world` snapshot does not have.
- **TNT-minecart ignition is activator-rail only** — not a burning-arrow
  hit, an explosion, fire, or a hard fall, none of which this crate's
  minecart tick has a signal for (no combat-vs-vehicle model exists at
  all).
- **Chest/hopper minecart inventories are real storage with no GUI.**
  `TrackedMinecart::slots` is a genuine, correctly-sized
  `Vec<Option<ItemStack>>` and round-trips through the sim, but nothing
  opens a menu against it: the container-click/window-id machinery
  (`crate::container_click`, `crate::block_entities::BlockEntityRegistry`)
  is keyed to a `BlockPos`-addressed container everywhere it is called
  from today, and re-keying that seam to also address a live entity id is
  a materially larger change than this feature. A hopper minecart's own
  pull from/into a world hopper is the same class of gap.

## What does not draw yet

A spawned, moving, correctly-oriented minecart reaches the wire — spawn
packet, position/velocity updates, `SET_ENTITY_DATA` for a furnace's fuel —
and a right-click mount correctly seats the rider (the passenger-attachment
table already has an entry for every minecart type). **Nothing renders the
cart itself.** `lodestone-render`'s `entity.rs` has a placement-offset arm
for the species name `"minecart"` (`non_living_vehicle_matrix`, landed
alongside a broader non-living-entity placement fix) but its
`model_for_type`/`entity_models()` corpus has no `"minecart"` mesh entry at
all, so `model_for_type("minecart")` returns `None` and the renderer skips
the entity outright.

This is a different, and narrower, gap than primed TNT's own used to be:
vanilla's `TntRenderer` draws a primed TNT as a literal block model (a cube
with a translate/rotate dance for the fuse flash), so
`lodestone-shell`'s `merge_primed_tnt` (`gpu/moving_blocks.rs`) could pose
it beside the falling-block path with no baked rig at all — landed, and
`model_for_type("tnt")` is *still*, correctly, `None`, because that route
never goes through the corpus. A minecart's own renderer
(`AbstractMinecartRenderer`) is not a bare block, though — it is a real
cart-frame mesh, the same shape a boat's hull is — so the fix here is a
corpus entry (`model_for_type`/`non_living_vehicle_matrix`'s counterpart
for boats), not a `merge_primed_tnt`-style shortcut. Neither exists yet for
a minecart, which is why it still draws zero pixels.

## Configuration

None — no game rule or config gates any of this beyond `tnt_explodes`
(read the same way `crate::mobs::tnt` reads it, for a TNT minecart's own
detonation).

## Dependencies

- `lodestone_physics::entity::move_entity` — the shared collision
  integrator, also used by `MobSim::tick_vehicles`/`tick_tnt`.
- `crate::redstone::{base_name, get_bool_property, get_str_property,
  is_redstone_conductor}` — reading a rail/powered-rail/activator-rail's own
  state string.
- `crate::mobs::tnt`'s `tnt_rng`/`MobSim::explode`/`pending_detonations` —
  a TNT minecart's blast reuses primed TNT's own isolated RNG stream and
  detonation pipeline.
- `crates/protocol/v770/src/server_protocol.rs`'s `MetadataField::MinecartFuel`
  encoder (index 13) — the furnace-fuel wire half, guarded against the
  committed jar dump (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`)
  in `server_protocol.rs`'s own `index_thirteen_tests`.
