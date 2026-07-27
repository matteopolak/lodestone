//! Hermetic tests for protocol 776 `add_entity` dispatch.
//!
//! `add_entity`'s wire layout (`ClientboundAddEntityPacket`) is VarInt entity
//! id, UUID, VarInt entity-type registry id, position `f64`×3, a low-precision
//! velocity, three signed-byte angles (pitch, yaw, head yaw), and a VarInt data
//! field. Head yaw travels separately from body yaw and is surfaced through the
//! same `EntityHeadRotation` outlet `rotate_head` uses, so a spawn must emit
//! both a spawn event and a head-rotation event — losing either one strands the
//! renderer with a wrong-looking mob.

use lodestone_model::{ClientEvent, ConnectionState, Directive, Rotation, Vec3, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;
use uuid::Uuid;

fn handle(adapter: &V770Adapter, packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle packet")
}

/// Independent VarInt encoder (not the codec under test).
fn var_i32(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value as u32;
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out
}

/// Independent angle unpacker (`Mth.unpackDegrees`): a signed byte over a
/// 256-step circle.
fn unpack_degrees(packed: i8) -> f32 {
    f32::from(packed) * 360.0 / 256.0
}

/// Builds an `add_entity` payload. `head_yaw`/`yaw`/`pitch` are raw signed
/// angle bytes; velocity is the single-byte zero-vector encoding of `LpVec3`.
fn add_entity_bytes(
    entity_id: i32,
    uuid: Uuid,
    type_id: i32,
    pos: (f64, f64, f64),
    pitch: i8,
    yaw: i8,
    head_yaw: i8,
) -> Vec<u8> {
    let mut bytes = var_i32(entity_id);
    bytes.extend_from_slice(&uuid.as_u128().to_be_bytes());
    bytes.extend_from_slice(&var_i32(type_id));
    bytes.extend_from_slice(&pos.0.to_be_bytes());
    bytes.extend_from_slice(&pos.1.to_be_bytes());
    bytes.extend_from_slice(&pos.2.to_be_bytes());
    bytes.push(0x00); // LpVec3 zero-vector sentinel
    bytes.push(pitch as u8);
    bytes.push(yaw as u8);
    bytes.push(head_yaw as u8);
    bytes.extend_from_slice(&var_i32(0)); // data
    bytes
}

#[test]
fn add_entity_emits_spawn_and_head_rotation() {
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(42);
    let payload = add_entity_bytes(7, uuid, 100, (1.0, 64.0, -2.0), 10, 20, 64);
    let directives = handle(&adapter, play::clientbound::ADD_ENTITY, &payload);
    assert_eq!(
        directives,
        vec![
            Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: 7,
                uuid: Some(uuid),
                entity_type: "minecraft:pig".parse().unwrap(),
                pos: Vec3::new(1.0, 64.0, -2.0),
                rotation: Rotation::new(unpack_degrees(20), unpack_degrees(10)),
                velocity: Some(Vec3::new(0.0, 0.0, 0.0)),
            }),
            Directive::Emit(ClientEvent::EntityHeadRotation {
                entity_id: 7,
                head_yaw: unpack_degrees(64), // 64 * 360/256 = 90 degrees
            }),
        ]
    );
}

#[test]
fn add_entity_head_yaw_diverges_from_body_yaw() {
    // A mob looking sideways while walking forward: body yaw 0, head yaw 90 —
    // the two must not collapse into a single rotation.
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(1);
    let payload = add_entity_bytes(1, uuid, 100, (0.0, 0.0, 0.0), 0, 0, 64);
    let directives = handle(&adapter, play::clientbound::ADD_ENTITY, &payload);
    match directives.as_slice() {
        [
            Directive::Emit(ClientEvent::EntitySpawned { rotation, .. }),
            Directive::Emit(ClientEvent::EntityHeadRotation { head_yaw, .. }),
        ] => {
            assert_eq!(rotation.yaw, 0.0, "body yaw unaffected by head yaw");
            assert_eq!(*head_yaw, 90.0);
        }
        other => panic!("expected [EntitySpawned, EntityHeadRotation], got {other:?}"),
    }
}

#[test]
fn add_entity_rejects_unknown_entity_type() {
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(1);
    let payload = add_entity_bytes(1, uuid, 1_000_000, (0.0, 0.0, 0.0), 0, 0, 0);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::ADD_ENTITY,
        &payload,
    );
    assert!(result.is_err(), "an unknown entity-type id must be rejected");
}

#[test]
fn add_entity_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(1);
    let mut payload = add_entity_bytes(1, uuid, 100, (0.0, 0.0, 0.0), 0, 0, 0);
    payload.push(0xFF);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::ADD_ENTITY,
        &payload,
    );
    assert!(result.is_err(), "a misaligned add_entity must be rejected");
}

#[test]
fn add_entity_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let uuid = Uuid::from_u128(1);
    let mut payload = add_entity_bytes(1, uuid, 100, (0.0, 0.0, 0.0), 0, 0, 0);
    payload.truncate(payload.len() - 1); // drop the data varint
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::ADD_ENTITY,
        &payload,
    );
    assert!(
        result.is_err(),
        "a truncated add_entity must be rejected, not panic"
    );
}
