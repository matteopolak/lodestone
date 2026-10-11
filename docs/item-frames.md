# Item frames

## What it is

Item frames and glow item frames on the integrated server: placing a frame item, putting an item in the frame, turning it, knocking it out, persistence, and the frame marker a framed filled map gains. Rules live in `lodestone_server::item_frame` (pure) and `lodestone_server::mobs::MobSim` (state, `mobs/item_frame.rs`); drawing is [filled-map-rendering](./filled-map-rendering.md) and the item-frame render path.

## How it works

* **Placement.** `UseItemOn` with a frame item reaches `item_frame::apply_frame_item`. The frame hangs in the cell in front of the clicked face (any of the six faces), faces out, and needs: a builder game mode, a cell inside the dimension's height, no block collider in its 0.75 x 0.75 x 1/16 slab, a solid block behind it (a repeater or comparator also counts for a horizontal frame), and no other frame facing the same way overlapping it. A refusal keeps the stack and ends the click. One item is consumed outside creative.
* **Geometry.** The box is a 1/16 slab on the wall side of the cell, 0.75 square, widening to a full block while it holds a map. Survival always tests the 0.75 box.
* **Use.** `InteractEntity` on an empty frame inserts one held item; on a filled frame it turns the item one eighth (8 steps wrap). A map already carrying more than 256 markers is refused. A fixed frame ignores use.
* **Hit.** An attack with an item in the frame pops the item (the frame stays, `ItemDropChance` rolls the drop); an attack on an empty frame breaks it and drops the frame item. Creative breakers drop nothing; a fixed frame yields only to creative. Adventure and spectator players do neither.
* **Support.** Every 101st tick `plan_frame_checks` reports frames that no longer survive; `tick.rs` breaks them (frame item and framed stack drop) once the world read was complete.
* **Sounds.** `effects::item_frame_sound`: place, add item, rotate, remove item and break, neutral category at full volume and pitch, published to every player by `item_frame::publish_sound`.
* **Sync.** Three entity-data fields, indices taken from the committed jar dump (`crates/versions/26.3/tests/support/entity_data_index_jvm.txt`): facing at 8 (`DIRECTION`, the 3D value down 0, up 1, north 2, south 3, west 4, east 5), item at 9 (`ITEM_STACK`, the empty stack when empty), rotation at 10 (`INT`). The spawn packet carries the attachment cell's corner as its position and the facing as its object data; yaw is the quarter turn from south (east 270) and pitch is 90 for a floor frame down and -90 for a ceiling frame. An invisible frame also sets the shared invisible flag.
* **Persistence.** `minecraft:item_frame` / `minecraft:glow_item_frame` records with the box centre as `Pos`, `Item`, `ItemRotation`, `ItemDropChance`, `Facing` (3D value), `Invisible`, `Fixed` and `block_pos`. The Anvil reader leaves a non-item entity's `Item` in `extra`, so `restore_frame` reads either the record's stack or that field. The native store takes the same record.
* **Maps.** `MobSim::framed_maps` lists frames holding a filled map. `MapSession::tick` shows each to the connection every 10th tick: `MapData::tick_in_frame` adds a `frame` decoration at the cell's integer coordinates, rotated by the facing (-90 degrees for floor and ceiling frames), replaces another frame's marker at the same cell, and drops the viewer's own holder marker once they no longer hold the map. Breaking a frame or popping its map calls `MapData::removed_from_frame`. Every player in the dimension receives a framed map, held or not.

## How to change it

* Rules: `item_frame.rs` and its tests; the use-on-block, interact, attack and tick wiring only call into it.
* New entity data: `MetadataField` in `protocol/session.rs` plus the 26.2 family encoder constants (`METADATA_IDX_*` in `crates/versions/26.2/src/server_protocol.rs`), checked against the jar dump by `item_frame_fields_encode_at_the_26_3_dump_rows`.
* The end-to-end gate is `crates/versions/26.2/tests/singleplayer_lan/singleplayer_item_frame.rs`.

## Gaps

* No comparator output from the frame's rotation: the redstone input query reads block state only, with no entity hook.
* No world-border test on the placement box, no custom name from the frame item, no `entity_drops` rule, and no breaking by projectiles, explosions or fire.
* Nothing summons a frame except a saved record.

## Configuration

None.

## Dependencies

`lodestone-data` (collision shapes, block solidity), `lodestone-entity` (item entity lifecycle), `crate::maps` (frame markers), `crate::cushion::Aabb` (box maths), the 26.2 family encoder for the wire.
