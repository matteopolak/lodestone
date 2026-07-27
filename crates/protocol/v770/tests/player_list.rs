//! Hermetic tests for protocol 776 `player_info_remove` dispatch.
//!
//! `player_info_remove` carries a VarInt-prefixed list of profile UUIDs.
//! Golden bytes are hand-built (two raw UUIDs as `w.uuid()` would write them:
//! big-endian most-significant-then-least-significant 64-bit halves), and a
//! trailing byte is asserted to fail decode so a misparse cannot slip through
//! `ensure_empty`.

use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
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

#[test]
fn player_info_remove_emits_uuid_list() {
    let adapter = V770Adapter::new();
    let a = Uuid::from_u128(7);
    let b = Uuid::from_u128(8);
    let mut payload = var_i32(2);
    payload.extend_from_slice(a.as_u128().to_be_bytes().as_slice());
    payload.extend_from_slice(b.as_u128().to_be_bytes().as_slice());
    let directives = handle(&adapter, play::clientbound::PLAYER_INFO_REMOVE, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::PlayerListRemove {
            profile_ids: vec![a, b],
        })]
    );
}

#[test]
fn player_info_remove_handles_empty_list() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::PLAYER_INFO_REMOVE, &var_i32(0));
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::PlayerListRemove {
            profile_ids: vec![],
        })]
    );
}

#[test]
fn player_info_remove_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let a = Uuid::from_u128(1);
    let mut payload = var_i32(1);
    payload.extend_from_slice(a.as_u128().to_be_bytes().as_slice());
    payload.push(0xFF);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::PLAYER_INFO_REMOVE,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned player_info_remove must be rejected"
    );
}

#[test]
fn player_info_remove_rejects_truncated_uuid_list() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(2); // claims two UUIDs
    payload.extend_from_slice(Uuid::from_u128(1).as_u128().to_be_bytes().as_slice()); // only one supplied
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::PLAYER_INFO_REMOVE,
        &payload,
    );
    assert!(
        result.is_err(),
        "a truncated player_info_remove must be rejected, not panic"
    );
}
