# Protocol 1.8 entity ingest

## What it is

This document records the protocol-47 fixture boundary for entity metadata, equipment, and attributes. It proves that the 1.8 family reaches the production ECS components despite its fixed-point entity coordinates and pre-flattening slot format.

## How it works

`crates/versions/1.8/tests/metadata_equipment_attributes_ecs.rs` feeds literal `spawn_entity_living`, `entity_metadata`, `entity_equipment`, and `update_attributes` bodies into `V47Adapter`. The adapter emits version-free events, and `lodestone_ecs::ingest::IngestPlugin` runs the normal `NetIngest` schedule before the test checks the pig's flags, head slot, and canonical movement-speed snapshot.

The fixture keeps the protocol-47 varint entity ids, fixed-point spawn fields, signed legacy slot shape, and mixed varint/i32 attribute counts visible. Wrong-value mutations must change the ECS result, while a truncated metadata body must be rejected before any event is emitted.

## How to change it

Extend the literal bodies when a protocol-47 entity field gains a production consumer. Keep expected values independent of packet encoders and assert the final ECS component. Add a wrong-value mutation for every newly observed field and keep a truncation control at the packet boundary.

## Configuration

The test uses protocol 47 (`lodestone_v1_8::PROTOCOL`) and the generated protocol-47 clientbound packet ids. It runs as the normal `lodestone-v1-8` integration test target with `lodestone-ecs` as a development dependency.

## Dependencies

The fixture depends on `lodestone-v1-8`, `lodestone-model`, `lodestone-world`, and `lodestone-ecs`. It exercises the production adapter trait and `NetIngest` schedule; no live server or packet encoder is required.
