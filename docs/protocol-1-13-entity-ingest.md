# Protocol 1.13 entity ingest

## What it is

Protocol 404 still carries initial entity metadata on living-entity and player spawn packets, while equipment and attribute updates use the flattened item registry, textual attribute names, and UUID modifiers. This document records the independent fixture boundary that proves those packets reach the ECS entity components.

## How it works

`lodestone-v1-13` decodes the protocol-specific packet bytes and emits version-free `EntitySpawned`, `EntityMetadataUpdated`, `EntityEquipmentUpdated`, and `EntityAttributesUpdated` events. The normal `NetIngest` schedule indexes the spawned entity, folds the base metadata fields, merges the head-slot update, and merges attribute snapshots by canonical attribute id.

`crates/versions/1.13/tests/metadata_attributes_ecs.rs` supplies literal packet bodies rather than encoding packet structs. It asserts the resulting flags, custom name, visibility, head equipment, canonical attribute id, base value, modifier UUID, amount, and operation on the ECS entity. Separate wrong-value and truncated-equipment controls ensure the item count is observed and malformed input cannot enter the ingest queue.

## How to change it

Extend the literal fixtures when adding a protocol-404 field that has a consumer. Keep expected values independent of the packet codec, and assert the final ECS component rather than stopping at a decoded event. Preserve a wrong-value and truncation control when changing a wire offset. If a field is not consumed by `NetIngest`, add its consumer in `lodestone-ecs` before expanding the fixture.

## Configuration

The test uses protocol 404 (`PROTOCOL_1_13_2`) and the generated protocol-404 clientbound packet ids. It runs with the `lodestone-v1-13` crate's normal test target and its `lodestone-ecs` development dependency.

## Dependencies

The fixture depends on `lodestone-v1-13`, `lodestone-model`, `lodestone-world`, and `lodestone-ecs`. It exercises the production adapter trait and ECS ingest schedule; no live server or external capture is required.
