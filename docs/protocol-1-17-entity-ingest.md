# Protocol 1.17 entity ingest

## What it is

This document records the protocol-756 entity fixture boundary for metadata, equipment, and attribute updates. It proves that bytes decoded by `lodestone-v1-17` reach the production ECS components through `NetIngest`.

## How it works

`crates/versions/1.17/tests/metadata_attributes_ecs.rs` supplies independent `spawn_entity_living`, `entity_metadata`, `entity_equipment`, and `entity_update_attributes` bodies. The adapter emits version-free client events, and `lodestone_ecs::ingest::IngestPlugin` indexes the spawned pig before folding the flags, slot updates, and canonical attribute snapshot.

The fixture keeps the main hand explicitly empty while populating the head slot. It also uses the legacy textual attribute spelling and UUID modifier form, so both registry canonicalisation and modifier identity are checked at the ECS boundary rather than only at the decoder boundary.

## How to change it

Extend the literal bodies when a newly supported protocol-756 entity field gains a production consumer. Keep expected values independent of packet encoders, and assert the final ECS component. Add or update a wrong-value control whenever changing a field offset or registry mapping; a control that still passes after mutating the wire value means the fixture is not observing that field.

## Configuration

The test uses protocol 756 (`PROTOCOL_1_17_1`) and the generated protocol-756 clientbound packet ids. It runs as the `lodestone-v1-17` integration test target with the crate's normal test features.

## Dependencies

The fixture depends on `lodestone-v1-17`, `lodestone-model`, `lodestone-world`, and `lodestone-ecs`. It exercises the production adapter trait and `NetIngest` schedule; no live server, packet encoder, or external capture is required.
