# Protocol 1.7 entity ingest

## What it is

This document records the protocol-5 fixture boundary for entity metadata, equipment, and attributes. It proves that the oldest supported family reaches the production ECS components rather than stopping at decoded events.

## How it works

`crates/versions/1.7/tests/metadata_equipment_attributes_ecs.rs` feeds literal `spawn_entity_living`, `entity_metadata`, `entity_equipment`, and `update_attributes` bodies into `V5Adapter`. The adapter emits version-free events, and `lodestone_ecs::ingest::IngestPlugin` runs the normal `NetIngest` schedule before the test checks the pig's flags, head slot, and canonical movement-speed snapshot.

The fixture keeps the protocol-5 fixed-point spawn fields, raw i32 entity ids, signed legacy slot shape, and four-byte attribute count visible. Wrong-value mutations must change the ECS result, while a truncated metadata body must be rejected before any event is emitted.

## How to change it

Extend the literal bodies when a protocol-5 entity field gains a production consumer. Keep expected values independent of packet encoders and assert the final ECS component. Add a wrong-value mutation for every newly observed field and keep a truncation control at the packet boundary.

## Configuration

The test uses protocol 5 (`lodestone_v1_7::PROTOCOL`) and the generated protocol-5 clientbound packet ids. It runs as the normal `lodestone-v1-7` integration test target with `lodestone-ecs` as a development dependency.

## Dependencies

The fixture depends on `lodestone-v1-7`, `lodestone-model`, `lodestone-world`, and `lodestone-ecs`. It exercises the production adapter trait and `NetIngest` schedule; no live server or packet encoder is required.
