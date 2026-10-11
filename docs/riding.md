# Riding

## What it is

One rider-to-vehicle relation, `MobSim::riders` (`mobs/riders.rs`), covers players and mobs on boats, minecarts, cushions and mobs. Nothing else records who sits where.

## How it works

- **Relation.** `Ride { rider, player, vehicle }` links in boarding order. `riders_of`, `vehicle_of` and `controlling_player` are the readers. A boarding player goes ahead of mobs unless a player already leads; the first rider steers when it is a player, and a steered vehicle skips its own tick (`controlled_vehicles`).
- **Boarding.** Interaction (`mount_vehicle`, `mount_minecart`, `mount_cushion`, `mount_mob`) keeps its kind rules (sneaking, capsized boat, non-rideable cart) and capacity: two for a boat or raft, one for everything else. `/ride` uses `mount_forced`, which skips all of that and refuses only a seat cycle and a rider already on that vehicle.
- **Upkeep.** `sync_riders` runs each sim tick: it drops links whose rider or vehicle is gone, carries mob riders to the seat (a mob vehicle's seat is 0.75 of its height) and skips their goals. A disconnected player is dropped by `clear_disconnected_riders`; a dead one at respawn. An activator rail under a plain cart, a buck and a broken seat just remove links.
- **Wire.** `EntitySnapshot::passengers` carries every rider. `EntityStreamer` is the only sender of the passenger-list packet: after a pass's spawns, whenever a list changes, naming the connection's own player `LOCAL_PLAYER_ENTITY_ID` (`EntityStreamer::viewer`).
- **Sneak dismount.** `queue_dismount_placement` holds the boat-side position; `stream_and_place` sends it only after the client has been told it left, because a seated client ignores a position packet.
- **Saving.** See [Entity persistence](entity-persistence.md): mob riders nest as `Passengers`.

## How to change it

A new vehicle kind needs a seat in `seat_origin`, a capacity in `seat_capacity` and a snapshot; a new rule on who may board belongs in its `mount_*` wrapper, not in `board`. Do not add a per-kind rider field or a packet sent from a command or dispatch arm.

## Configuration

None.

## Dependencies

`crate::server::EntityStreamer`, `crate::mobs::sim_persistence`, `crate::commands::ride`.
