//! Hermetic tests for protocol 776 `player_info_remove`/`player_info_update`
//! dispatch.
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

/// Issue #283's real gap, closed: `INITIALIZE_CHAT`'s session used to reach
/// `PlayerInfoEntry` (see `player_info.rs`'s own
/// `initialize_chat_is_kept_not_discarded`) and stop there — `adapter::player`
/// dropped it converting to the canonical `lodestone_model::event::PlayerListEntry`,
/// which had no field to carry it into. This drives the real
/// `player_info_update` packet through `V770Adapter::handle_packet` and
/// asserts the session survives all the way to the emitted
/// `ClientEvent::PlayerListUpdate` entry — not just the intermediate decode.
#[test]
fn player_info_update_carries_the_chat_session_into_the_model_event() {
    let adapter = V770Adapter::new();
    let sender = Uuid::from_u128(42);
    let session_id = Uuid::from_u128(0xaabb_ccdd_eeff_0011_2233_4455_6677_8899);
    let public_key: Vec<u8> = vec![0x30, 0x81, 0x9f, 0x02, 0x81, 0x81];
    let key_signature: Vec<u8> = vec![0x01, 0x02, 0x03, 0x04, 0x05];
    let expires_at: i64 = 1_700_000_000_123;

    // action bitmask: bit 1 (INITIALIZE_CHAT) only.
    let mut payload = vec![1u8 << 1];
    payload.extend_from_slice(&var_i32(1)); // one entry
    payload.extend_from_slice(sender.as_u128().to_be_bytes().as_slice());
    payload.push(0x01); // INITIALIZE_CHAT present
    payload.extend_from_slice(session_id.as_u128().to_be_bytes().as_slice());
    payload.extend_from_slice(&expires_at.to_be_bytes());
    payload.extend_from_slice(&var_i32(public_key.len() as i32));
    payload.extend_from_slice(&public_key);
    payload.extend_from_slice(&var_i32(key_signature.len() as i32));
    payload.extend_from_slice(&key_signature);

    let directives = handle(&adapter, play::clientbound::PLAYER_INFO_UPDATE, &payload);
    let [Directive::Emit(ClientEvent::PlayerListUpdate { entries })] = directives.as_slice() else {
        panic!("expected one PlayerListUpdate directive, got {directives:?}");
    };
    assert_eq!(entries.len(), 1);
    let session = entries[0]
        .chat_session
        .as_ref()
        .expect("the announced chat session must reach the model event");
    assert_eq!(session.session_id, session_id);
    assert_eq!(session.public_key, public_key);
    assert_eq!(session.expires_at, expires_at);
}
