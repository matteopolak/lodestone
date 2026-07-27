//! Hermetic tests for protocol 776 chat acknowledgement wire handling.
//!
//! Covers the serverbound `chat_ack` encode (the standalone acknowledgement
//! that drains the server's pending-signed-message list and prevents the
//! 4096-message disconnect), plus clientbound `player_chat` / `disguised_chat`
//! decode into version-free [`ClientEvent::Chat`] carriers.
//!
//! Payloads are hand-built from the 26.2 wire layout so a misparse cannot slip
//! through `ensure_empty`; every decode test also asserts a trailing byte fails.

use lodestone_model::{
    ChatAckInfo, ChatKind, ClientAction, ClientEvent, ConnectionState, Directive, VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

/// A network-NBT bare string component: `TAG_String`, big-endian u16 length,
/// then the UTF-8 bytes.
fn nbt_string(text: &str) -> Vec<u8> {
    let mut out = vec![0x08];
    out.extend_from_slice(&(text.len() as u16).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    out
}

/// A VarInt-prefixed UTF-8 string (`FriendlyByteBuf.writeUtf`).
fn mc_string(text: &str) -> Vec<u8> {
    let mut out = var_i32(text.len() as i32);
    out.extend_from_slice(text.as_bytes());
    out
}

/// Minimal VarInt encoder for building golden payloads.
fn var_i32(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value as u32;
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v != 0 {
            out.push(byte | 0x80);
        } else {
            out.push(byte);
            break;
        }
    }
    out
}

/// A registry-reference `ChatType.Bound`: a non-zero holder VarInt (`id + 1`),
/// a trusted NBT name component, and an absent optional target name.
fn chat_type_bound(sender_name: &str) -> Vec<u8> {
    let mut out = var_i32(1); // holder id 0 -> wire value 1 (registry ref, no inline body)
    out.extend_from_slice(&nbt_string(sender_name)); // name component
    out.push(0x00); // optional target name absent
    out
}

#[test]
fn chat_ack_encodes_offset_as_varint() {
    let adapter = V770Adapter::new();
    let out = adapter
        .encode_action(ConnectionState::Play, &ClientAction::ChatAck { offset: 300 })
        .expect("encode chat_ack");
    let (id, payload) = out.expect("chat_ack must produce a packet");
    assert_eq!(id, play::serverbound::CHAT_ACK);
    assert_eq!(payload, var_i32(300));
}

#[test]
fn chat_ack_not_encoded_outside_play() {
    let adapter = V770Adapter::new();
    let out = adapter
        .encode_action(
            ConnectionState::Configuration,
            &ClientAction::ChatAck { offset: 1 },
        )
        .expect("encode outside play");
    assert!(out.is_none(), "chat_ack only exists in play, got {out:?}");
}

/// A signed player-chat body: content string, 8-byte timestamp, 8-byte salt,
/// then an empty last-seen collection (VarInt `0`).
fn signed_body(content: &str) -> Vec<u8> {
    let mut out = mc_string(content);
    out.extend_from_slice(&0i64.to_be_bytes()); // timestamp
    out.extend_from_slice(&0i64.to_be_bytes()); // salt
    out.extend_from_slice(&var_i32(0)); // last-seen count
    out
}

/// Builds a `player_chat` payload.
///
/// `signature` present -> a 256-byte signature block preceded by its presence
/// flag; `unsigned` present -> a trusted NBT component preceded by its flag;
/// `filter_ordinal` selects the FilterMask (0 = pass-through, 1 = fully
/// filtered, 2 = partially filtered which appends an empty bitset).
fn player_chat(
    global_index: i32,
    signature: Option<[u8; 256]>,
    content: &str,
    unsigned: Option<&str>,
    filter_ordinal: i32,
) -> Vec<u8> {
    let mut out = var_i32(global_index);
    out.extend_from_slice(&[0u8; 16]); // sender UUID
    out.extend_from_slice(&var_i32(0)); // index
    match signature {
        Some(sig) => {
            out.push(0x01);
            out.extend_from_slice(&sig);
        }
        None => out.push(0x00),
    }
    out.extend_from_slice(&signed_body(content));
    match unsigned {
        Some(text) => {
            out.push(0x01);
            out.extend_from_slice(&nbt_string(text));
        }
        None => out.push(0x00),
    }
    out.extend_from_slice(&var_i32(filter_ordinal));
    if filter_ordinal == 2 {
        out.extend_from_slice(&var_i32(0)); // empty partially-filtered bitset
    }
    out.extend_from_slice(&chat_type_bound("Sender"));
    out
}

#[test]
fn player_chat_signed_surfaces_ack_info() {
    let adapter = V770Adapter::new();
    let sig = [0x42u8; 256];
    let payload = player_chat(7, Some(sig), "hello world", None, 0);
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::PLAYER_CHAT,
            &payload,
        )
        .expect("handle player_chat");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Chat { text, kind, ack })] => {
            assert_eq!(text.to_plain_string(), "hello world");
            assert_eq!(*kind, ChatKind::Chat);
            let ChatAckInfo {
                signature,
                global_index,
                was_shown,
            } = ack.as_ref().expect("signed chat carries ack info");
            assert_eq!(signature.as_slice(), &sig[..]);
            assert_eq!(*global_index, 7);
            assert!(*was_shown, "pass-through filter is shown");
        }
        other => panic!("expected one signed chat event, got {other:?}"),
    }
}

#[test]
fn player_chat_unsigned_prefers_unsigned_content_and_empty_signature() {
    let adapter = V770Adapter::new();
    let payload = player_chat(3, None, "raw", Some("decorated"), 1);
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::PLAYER_CHAT,
            &payload,
        )
        .expect("handle player_chat");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Chat { text, ack, .. })] => {
            assert_eq!(text.to_plain_string(), "decorated");
            let info = ack.as_ref().expect("chat carries ack info");
            assert!(info.signature.is_empty(), "unsigned chat has no signature");
            assert_eq!(info.global_index, 3);
            assert!(!info.was_shown, "fully-filtered chat is not shown");
        }
        other => panic!("expected one chat event, got {other:?}"),
    }
}

#[test]
fn player_chat_partially_filtered_is_shown() {
    let adapter = V770Adapter::new();
    let payload = player_chat(1, Some([0u8; 256]), "hi", None, 2);
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::PLAYER_CHAT,
            &payload,
        )
        .expect("handle player_chat");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Chat { ack, .. })] => {
            assert!(ack.as_ref().expect("ack info").was_shown);
        }
        other => panic!("expected one chat event, got {other:?}"),
    }
}

#[test]
fn player_chat_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = player_chat(1, None, "hi", None, 0);
    payload.push(0xFF);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::PLAYER_CHAT,
        &payload,
    );
    assert!(result.is_err(), "trailing byte must fail, got {result:?}");
}

#[test]
fn player_chat_inline_chat_type_errors_loudly() {
    let adapter = V770Adapter::new();
    let mut payload = var_i32(1);
    payload.extend_from_slice(&[0u8; 16]);
    payload.extend_from_slice(&var_i32(0));
    payload.push(0x00); // no signature
    payload.extend_from_slice(&signed_body("hi"));
    payload.push(0x00); // no unsigned content
    payload.extend_from_slice(&var_i32(0)); // pass-through
    payload.extend_from_slice(&var_i32(0)); // holder id 0 -> inline chat type
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::PLAYER_CHAT,
        &payload,
    );
    assert!(
        result.is_err(),
        "inline chat-type definitions must fail loudly, got {result:?}"
    );
}

#[test]
fn disguised_chat_emits_chat_without_ack() {
    let adapter = V770Adapter::new();
    let mut payload = nbt_string("a disguise");
    payload.extend_from_slice(&chat_type_bound("Server"));
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::DISGUISED_CHAT,
            &payload,
        )
        .expect("handle disguised_chat");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Chat { text, kind, ack })] => {
            assert_eq!(text.to_plain_string(), "a disguise");
            assert_eq!(*kind, ChatKind::Chat);
            assert!(ack.is_none(), "disguised chat is unsigned, no ack");
        }
        other => panic!("expected one disguised chat event, got {other:?}"),
    }
}

#[test]
fn disguised_chat_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = nbt_string("x");
    payload.extend_from_slice(&chat_type_bound("S"));
    payload.push(0xFF);
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::DISGUISED_CHAT,
        &payload,
    );
    assert!(result.is_err(), "trailing byte must fail, got {result:?}");
}
