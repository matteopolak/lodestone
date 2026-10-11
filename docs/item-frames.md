# Item frames

## What it is

Item frames and glow item frames on the integrated server: placing a frame item, putting an item in the frame, turning it, knocking it out, damage from projectiles, blasts and fire, `/summon` and `/kill`, the comparator signal, persistence, and the frame marker a framed filled map gains. Rules live in `lodestone_server::item_frame` (pure) and `lodestone_server::mobs::MobSim` (state, `mobs/item_frame.rs`); drawing is [filled-map-rendering](./filled-map-rendering.md) and the item-frame render path.

## How it works

* **Placement.** `UseItemOn` with a frame item reaches `item_frame::apply_frame_item`. The frame hangs in the cell in front of the clicked face (any of the six faces), faces out, and needs: a builder game mode, a cell inside the dimension's height, no block collider in its 0.75 x 0.75 x 1/16 slab, a solid block behind it (a repeater or comparator also counts for a horizontal frame), and no other frame facing the same way overlapping it. A refusal keeps the stack and ends the click. One item is consumed outside creative.
* **Geometry.** The box is a 1/16 slab on the wall side of the cell, 0.75 square, widening to a full block while it holds a map. Survival always tests the 0.75 box.
* **Use.** `InteractEntity` on an empty frame inserts one held item; on a filled frame it turns the item one eighth (8 steps wrap). A map already carrying more than 256 markers is refused. A fixed frame ignores use.
* **Hit.** An attack with an item in the frame pops the item (the frame stays, `ItemDropChance` rolls the drop); an attack on an empty frame breaks it and drops the frame item. Creative breakers drop nothing; a fixed frame yields only to creative. Adventure and spectator players do neither.
* **Other damage.** `MobSim::hurt_frame_with(id, FrameDamage)` is the one rule table; non-player outcomes queue in `take_frame_events` and `tick.rs` sounds them and forgets framed maps. A projectile that hurts what it strikes (arrows, tridents, snowballs, eggs, pearls, fireballs, skulls, wind charges) pops the item, even a snowball dealing 0, then breaks the empty frame; potions only splash. An explosion breaks the frame outright (frame and item drop) when its centre is within twice its power, walls notwithstanding. Standing in fire or soul fire hurts a frame every tick (pop, then break). A fixed frame ignores all of these, an `Invulnerable` one too; only a creative player gets through. With mob griefing off a mob's arrow or blast is refused, and a blast that breaks no terrain reaches frames only while griefing is on.
* **Summon and selectors.** `/summon minecraft:item_frame <pos> {nbt}` (also `glow_item_frame`) builds the frame from the same record a save reads: `Facing`, `Item`, `ItemRotation`, `ItemDropChance`, `Invisible`, `Fixed`, `Invulnerable`, `block_pos`. A missing `Facing` hangs down, the cell is the one containing `<pos>`, and nothing checks the wall (the support check breaks a frame with none). Other types refuse NBT. `Ctx::resolve_targets` adds `MobSim::frame_command_entities` to the player roster for selectors that reach entities (`@e`), so `@e[type=item_frame,distance=..5]` finds them and `/kill` (now an entities argument) removes one with no drops and a map marker cleanup.
* **Comparator.** Each frame publishes `0` (empty) or `rotation + 1` as an entity analog output keyed by `(cell, facing)` in `entity_analog::EntityAnalogOutputs`, held in the block-entity registry; `MobSim::take_frame_signal_changes` feeds it from the tick loop. `redstone::comparator_input_signal` reads `RedstoneLookup::entity_analog_output` one cell past a conductor it faces, for a frame pointing the same way (exactly one such frame; an empty one reads 0 and overrides the input), maxed with a state-analog block there. Every change also calls `random_tick::notify_block` on the horizontal comparators around the frame cell (adjacent, or past one conductor).
* **Support.** Every 101st tick `plan_frame_checks` reports frames that no longer survive; `tick.rs` breaks them (frame item and framed stack drop) once the world read was complete.
* **Sounds.** `effects::item_frame_sound`: place, add item, rotate, remove item and break, neutral category at full volume and pitch, published to every player by `item_frame::publish_sound`.
* **Sync.** Three entity-data fields, indices taken from the committed jar dump (`crates/versions/26.3/tests/support/entity_data_index_jvm.txt`): facing at 8 (`DIRECTION`, the 3D value down 0, up 1, north 2, south 3, west 4, east 5), item at 9 (`ITEM_STACK`, the empty stack when empty), rotation at 10 (`INT`). The spawn packet carries the attachment cell's corner as its position and the facing as its object data; yaw is the quarter turn from south (east 270) and pitch is 90 for a floor frame down and -90 for a ceiling frame. An invisible frame also sets the shared invisible flag.
* **Persistence.** `minecraft:item_frame` / `minecraft:glow_item_frame` records with the box centre as `Pos`, `Item`, `ItemRotation`, `ItemDropChance`, `Facing` (3D value), `Invisible`, `Fixed` and `block_pos`. The Anvil reader leaves a non-item entity's `Item` in `extra`, so `restore_frame` reads either the record's stack or that field. The native store takes the same record.
* **Maps.** `MobSim::framed_maps` lists frames holding a filled map. `MapSession::tick` shows each to the connection every 10th tick: `MapData::tick_in_frame` adds a `frame` decoration at the cell's integer coordinates, rotated by the facing (-90 degrees for floor and ceiling frames), replaces another frame's marker at the same cell, and drops the viewer's own holder marker once they no longer hold the map. Breaking a frame or popping its map calls `MapData::removed_from_frame`. Every player in the dimension receives a framed map, held or not.

## How to change it

* Rules: `item_frame.rs` and its tests; the use-on-block, interact, attack and tick wiring only call into it.
* Another entity-backed comparator source: publish `(cell, facing, strength)` through `BlockEntityRegistry::publish_entity_analog` from the tick loop, as the frame block does after `take_frame_signal_changes`, and call `recheck_comparators_around` for the cell.
* A new damage source: add a `FrameDamage` variant and call `hurt_frame_with`; a source that is not a player's swing gets its sounds from `take_frame_events` for free.
* New entity data: `MetadataField` in `protocol/session.rs` plus the 26.2 family encoder constants (`METADATA_IDX_*` in `crates/versions/26.2/src/server_protocol.rs`), checked against the jar dump by `item_frame_fields_encode_at_the_26_3_dump_rows`.
* The end-to-end gate is `crates/versions/26.2/tests/singleplayer_lan/singleplayer_item_frame.rs`.

## Gaps

* A projectile whose owner is a creative player does not break a fixed frame, a deflected arrow is simply spent, a burning arrow does not ignite a frame, lava does not burn one, and a blast from a source in water is not ignored.
* Selectors see frames only; other non-player entities are still invisible to `@e` and `/kill`.
* No world-border test on the placement box, no custom name from the frame item, no `entity_drops` rule.

## Configuration

None.

## Dependencies

`lodestone-data` (collision shapes, block solidity), `crate::redstone` and `crate::random_tick` (comparator reads and rechecks), `lodestone-entity` (item entity lifecycle), `crate::maps` (frame markers), `crate::cushion::Aabb` (box maths), the 26.2 family encoder for the wire.
