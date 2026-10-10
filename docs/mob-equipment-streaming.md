# Mob equipment streaming

## What it is

Held items, armour and the raid banner on mobs are sent to clients with the set-equipment packet, on spawn and whenever a slot changes. Without it a zombie's weapon, a skeleton's bow and the captain's banner are invisible.

## How it works

`SimMob::snapshot` fills `EntitySnapshot::equipment` from `equipment_snapshot()`. `EntityStreamer::sync_updates` (`server/entity_streaming.rs`) sends one equipment directive after the spawn and metadata directives for every occupied slot, then on later ticks only the slots that differ from `last_sent`; a slot that empties is sent as empty.

The wire encoding is `ServerProtocol::encode_set_equipment` (default `None`, so families without the packet send nothing). `V770ServerProtocol` implements it: entity id, then entries of a slot byte (high bit means more follow) and an item stack with its component patch. The slot byte is the position in `EquipmentSlot::ALL`. `write_item_component_patch` writes `banner_patterns` layers inline (asset id, translation key, dye colour), so no synchronised pattern registry is needed.

## How to change it

- A new equipped item component needs an arm in `write_item_component_patch` (`crates/versions/26.2/src/server_protocol.rs`).
- A new `EntitySnapshot` constructor must set `equipment` (use `Vec::new()` when the entity has none).
- Players are not streamed this way.

## Tests

`crates/versions/26.2/tests/entity/entity_encoders.rs` round-trips the packet; `tests/server/entity_streaming_live.rs` drives a real client and checks the decoded equipment reaches its ECS, with a control that sends nothing.

## Configuration

None.

## Dependencies

`lodestone-server` (`EntityStreamer`, `EntitySnapshot`), `lodestone-v26-2` (wire encoding, shared by 26.3).
