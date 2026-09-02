//! Hermetic tests for the protocol 776 world-border packets:
//! `set_border_center`, `set_border_lerp_size`, `set_border_size`,
//! `set_border_warning_delay`, `set_border_warning_distance`, and
//! `initialize_border`.
//!
//! All six packets are flat sequences of fixed-width fields (doubles,
//! VarLongs, VarInts) with no branching, so golden bytes are hand-built
//! directly from `ClientboundInitializeBorderPacket` et al.'s field order, and
//! every decode asserts zero trailing bytes.

use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
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

// ---- set_border_center ------------------------------------------------

#[test]
fn set_border_center_decodes_x_and_z() {
    let adapter = V770Adapter::new();
    let mut payload = 100.5f64.to_be_bytes().to_vec();
    payload.extend_from_slice(&(-200.25f64).to_be_bytes());
    let directives = handle(&adapter, play::clientbound::SET_BORDER_CENTER, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::WorldBorderCenterChanged {
            x: 100.5,
            z: -200.25,
        })]
    );
}

#[test]
fn set_border_center_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = 0.0f64.to_be_bytes().to_vec();
    payload.extend_from_slice(&0.0f64.to_be_bytes());
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::SET_BORDER_CENTER, &payload);
}

#[test]
fn set_border_center_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let payload = 0.0f64.to_be_bytes().to_vec(); // missing z
    expect_err(&adapter, play::clientbound::SET_BORDER_CENTER, &payload);
}

// ---- set_border_lerp_size -----------------------------------------------

#[test]
fn set_border_lerp_size_decodes_sizes_and_time() {
    let adapter = V770Adapter::new();
    let mut payload = 200.0f64.to_be_bytes().to_vec();
    payload.extend_from_slice(&100.0f64.to_be_bytes());
    payload.push(0xB0); // VarLong 30000: 0xB0 0xEA 0x01
    payload.push(0xEA);
    payload.push(0x01);
    let directives = handle(&adapter, play::clientbound::SET_BORDER_LERP_SIZE, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::WorldBorderSizeLerping {
            old_size: 200.0,
            new_size: 100.0,
            lerp_time_ms: 30_000,
        })]
    );
}

#[test]
fn set_border_lerp_size_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = 0.0f64.to_be_bytes().to_vec();
    payload.extend_from_slice(&0.0f64.to_be_bytes());
    payload.push(0x00);
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::SET_BORDER_LERP_SIZE, &payload);
}

// ---- set_border_size ------------------------------------------------------

#[test]
fn set_border_size_decodes_size() {
    let adapter = V770Adapter::new();
    let payload = 60_000_000.0f64.to_be_bytes().to_vec();
    let directives = handle(&adapter, play::clientbound::SET_BORDER_SIZE, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::WorldBorderSizeChanged {
            size: 60_000_000.0,
        })]
    );
}

#[test]
fn set_border_size_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = 0.0f64.to_be_bytes().to_vec();
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::SET_BORDER_SIZE, &payload);
}

// ---- set_border_warning_delay --------------------------------------------

#[test]
fn set_border_warning_delay_decodes_seconds() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::SET_BORDER_WARNING_DELAY, &[15]);
    assert_eq!(
        directives,
        vec![Directive::Emit(
            ClientEvent::WorldBorderWarningDelayChanged { warning_time: 15 }
        )]
    );
}

#[test]
fn set_border_warning_delay_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    expect_err(
        &adapter,
        play::clientbound::SET_BORDER_WARNING_DELAY,
        &[15, 0xFF],
    );
}

// ---- set_border_warning_distance -----------------------------------------

#[test]
fn set_border_warning_distance_decodes_blocks() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::SET_BORDER_WARNING_DISTANCE,
        &[5],
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(
            ClientEvent::WorldBorderWarningDistanceChanged { warning_blocks: 5 }
        )]
    );
}

#[test]
fn set_border_warning_distance_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    expect_err(
        &adapter,
        play::clientbound::SET_BORDER_WARNING_DISTANCE,
        &[5, 0xFF],
    );
}

// ---- initialize_border ---------------------------------------------------

fn initialize_border_bytes() -> Vec<u8> {
    let mut payload = 10.0f64.to_be_bytes().to_vec(); // x
    payload.extend_from_slice(&(-10.0f64).to_be_bytes()); // z
    payload.extend_from_slice(&500.0f64.to_be_bytes()); // old_size
    payload.extend_from_slice(&1000.0f64.to_be_bytes()); // new_size
    payload.extend_from_slice(&[0xB0, 0xEA, 0x01]); // VarLong lerp_time 30000
    payload.extend_from_slice(&[0xC0, 0x84, 0x3D]); // VarInt absolute_max_size 1_000_000
    payload.push(10); // warning_blocks
    payload.push(20); // warning_time
    payload
}

#[test]
fn initialize_border_decodes_all_fields_in_order() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::INITIALIZE_BORDER,
        &initialize_border_bytes(),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::WorldBorderInitialized {
            x: 10.0,
            z: -10.0,
            old_size: 500.0,
            new_size: 1000.0,
            lerp_time_ms: 30_000,
            absolute_max_size: 1_000_000,
            warning_blocks: 10,
            warning_time: 20,
        })]
    );
}

#[test]
fn initialize_border_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = initialize_border_bytes();
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::INITIALIZE_BORDER, &payload);
}

#[test]
fn initialize_border_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut payload = initialize_border_bytes();
    payload.pop(); // drop the final warning_time byte
    expect_err(&adapter, play::clientbound::INITIALIZE_BORDER, &payload);
}
