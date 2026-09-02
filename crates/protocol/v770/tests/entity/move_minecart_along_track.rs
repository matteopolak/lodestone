//! Hermetic tests for protocol 776's `move_minecart_along_track`, the sole
//! movement channel `NewMinecartBehavior` uses once a minecart exists (it no
//! longer sends ordinary `move_entity_*` packets for cart entities).
//!
//! The golden byte vector is hand-built from the wire specification —
//! `ClientboundMoveMinecartPacket`/`vanilla's own new minecart behavior's own minecart step` in the
//! 26.2 decompiled Mojang source — not round-tripped through this crate's own
//! encoder, so a self-consistent misreading cannot pass silently. Every test
//! asserts zero trailing bytes via `ensure_empty`.

use lodestone_model::{ClientEvent, ConnectionState, Directive, EntityMovement, Rotation, Vec3, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

fn handle(adapter: &V770Adapter, packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle packet")
}

fn expect_err(adapter: &V770Adapter, packet_id: i32, payload: &[u8]) {
    let result =
        adapter.handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload);
    assert!(
        result.is_err(),
        "expected packet {packet_id} to be rejected"
    );
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

/// Encodes one `MinecartStep`: `vanilla's own vec3's own stream codec` position (3 f64 BE),
/// `vanilla's own vec3's own stream codec` movement (3 f64 BE), `ROTATION_BYTE` yaw, `ROTATION_BYTE`
/// pitch, `f32` weight — in that field order.
fn step(pos: (f64, f64, f64), vel: (f64, f64, f64), yaw_byte: i8, pitch_byte: i8, weight: f32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&pos.0.to_be_bytes());
    out.extend_from_slice(&pos.1.to_be_bytes());
    out.extend_from_slice(&pos.2.to_be_bytes());
    out.extend_from_slice(&vel.0.to_be_bytes());
    out.extend_from_slice(&vel.1.to_be_bytes());
    out.extend_from_slice(&vel.2.to_be_bytes());
    out.push(yaw_byte as u8);
    out.push(pitch_byte as u8);
    out.extend_from_slice(&weight.to_be_bytes());
    out
}

#[test]
fn single_step_applies_as_absolute_move_and_velocity() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(7); // entity id
    payload.extend(var_i32(1)); // one lerp step
    // yaw byte 64 -> 90.0deg, pitch byte 32 -> 45.0deg (256 steps / 360deg).
    payload.extend(step((1.0, 65.0, -2.0), (0.1, 0.0, -0.2), 64, 32, 1.0));

    let directives = handle(
        &adapter,
        play::clientbound::MOVE_MINECART_ALONG_TRACK,
        &payload,
    );
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::EntityMoved {
            entity_id,
            movement,
            rotation,
            on_ground,
        }), Directive::Emit(ClientEvent::EntityVelocity {
            entity_id: vel_entity_id,
            velocity,
        })] => {
            assert_eq!(*entity_id, 7);
            assert_eq!(*vel_entity_id, 7);
            assert_eq!(*movement, EntityMovement::Absolute(Vec3::new(1.0, 65.0, -2.0)));
            assert_eq!(*rotation, Some(Rotation::new(90.0, 45.0)));
            assert!(!*on_ground);
            assert_eq!(*velocity, Vec3::new(0.1, 0.0, -0.2));
        }
        other => panic!("expected EntityMoved + EntityVelocity, got {other:?}"),
    }
}

#[test]
fn multiple_steps_apply_only_the_terminal_one() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(9);
    payload.extend(var_i32(2));
    payload.extend(step((0.0, 64.0, 0.0), (0.0, 0.0, 0.0), 0, 0, 0.5));
    payload.extend(step((5.0, 64.0, 5.0), (0.2, 0.0, 0.2), 64, 0, 0.5));

    let directives = handle(
        &adapter,
        play::clientbound::MOVE_MINECART_ALONG_TRACK,
        &payload,
    );
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::EntityMoved { movement, rotation, .. }), Directive::Emit(ClientEvent::EntityVelocity { velocity, .. })] =>
        {
            assert_eq!(*movement, EntityMovement::Absolute(Vec3::new(5.0, 64.0, 5.0)));
            assert_eq!(*rotation, Some(Rotation::new(90.0, 0.0)));
            assert_eq!(*velocity, Vec3::new(0.2, 0.0, 0.2));
        }
        other => panic!("expected EntityMoved + EntityVelocity, got {other:?}"),
    }
}

#[test]
fn empty_step_list_emits_nothing() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(3);
    payload.extend(var_i32(0));
    let directives = handle(
        &adapter,
        play::clientbound::MOVE_MINECART_ALONG_TRACK,
        &payload,
    );
    assert_eq!(directives, Vec::new());
}

#[test]
fn rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend(var_i32(1));
    payload.extend(step((0.0, 0.0, 0.0), (0.0, 0.0, 0.0), 0, 0, 0.0));
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::MOVE_MINECART_ALONG_TRACK, &payload);
}

#[test]
fn rejects_truncated_step() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend(var_i32(1));
    let mut full_step = step((0.0, 0.0, 0.0), (0.0, 0.0, 0.0), 0, 0, 0.0);
    full_step.pop(); // drop the last byte of `weight`
    payload.extend(full_step);
    expect_err(&adapter, play::clientbound::MOVE_MINECART_ALONG_TRACK, &payload);
}
