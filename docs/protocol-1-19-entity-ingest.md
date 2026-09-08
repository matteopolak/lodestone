# Protocol 1.19 entity ingest

## What it is

This document records the protocol-762 entity fixture boundary for metadata, equipment, and attribute updates. It proves that bytes decoded by `lodestone-v1-19` reach the production ECS components through `NetIngest`.

## How it works

`crates/versions/1.19/tests/metadata_attributes_ecs.rs` supplies independent `spawn_entity`, `entity_metadata`, `entity_equipment`, and `entity_update_attributes` bodies. The adapter emits version-free client events, and `lodestone_ecs::ingest::IngestPlugin` indexes the spawned pig before folding the flags, slot updates, and canonical attribute snapshot.

The fixture keeps the main hand explicitly empty while populating the head slot. It also uses the legacy textual attribute spelling and protocol-762 UUID modifier form, so both registry canonicalisation and modifier identity are checked at the ECS boundary rather than only at the decoder boundary.

## How to change it

Extend the literal bodies when a newly supported protocol-762 entity field gains a production consumer. Keep expected values independent of packet encoders, and assert the final ECS component. Add or update a wrong-value control whenever changing a field offset or registry mapping; a control that still passes after mutating the wire value means the fixture is not observing that field.

## Configuration

The test uses protocol 762 (`PROTOCOL_1_19_4`) and the generated protocol-762 clientbound packet ids. It runs as the `lodestone-v1-19` integration test target with the crate's normal test features.

## Dependencies

The fixture depends on `lodestone-v1-19`, `lodestone-model`, `lodestone-world`, and `lodestone-ecs`. It exercises the production adapter trait and `NetIngest` schedule; no live server, packet encoder, or external capture is required.
