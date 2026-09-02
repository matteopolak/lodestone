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
        .encode_action(
            ConnectionState::Play,
            &ClientAction::ChatAck { offset: 300 },
        )
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
///
/// Timestamp and salt are **pairwise-distinct** non-zero values, not `0` —
/// `CLAUDE.md`'s own trap: two adjacent same-typed fields (both `i64` on the
/// wire here) transpose without a trace when a fixture sets them equal, and
/// `0` for both would hide exactly that.
fn signed_body(content: &str) -> Vec<u8> {
    let mut out = mc_string(content);
    out.extend_from_slice(&1_700_000_000_123i64.to_be_bytes()); // timestamp (millis)
    out.extend_from_slice(&99_887_766i64.to_be_bytes()); // salt
    out.extend_from_slice(&var_i32(0)); // last-seen count
    out
}

/// Builds a `player_chat` payload.
///
/// `signature` present -> a 256-byte signature block preceded by its presence
/// flag; `unsigned` present -> a trusted NBT component preceded by its flag;
/// `filter_ordinal` selects the FilterMask (0 = pass-through, 1 = fully
/// filtered, 2 = partially filtered which appends an empty bitset). The
/// chain index is fixed at `3` (distinct from the timestamp/salt/global_index
/// values used around it) so `player_chat_signed_surfaces_ack_info` can pin
/// it as a real, non-zero field rather than an untested `0`.
fn player_chat(
    global_index: i32,
    signature: Option<[u8; 256]>,
    content: &str,
    unsigned: Option<&str>,
    filter_ordinal: i32,
) -> Vec<u8> {
    let mut out = var_i32(global_index);
    out.extend_from_slice(&[0u8; 16]); // sender UUID — nil, pinned in the decode test below
    out.extend_from_slice(&var_i32(3)); // index (SignedMessageLink.index)
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
        [Directive::Emit(ClientEvent::Chat {
            text,
            kind,
            ack,
            sender,
            ..
        })] => {
            assert_eq!(text.to_plain_string(), "hello world");
            assert_eq!(*kind, ChatKind::Chat);
            // The wire sender UUID must reach `ClientEvent::Chat` — it is
            // the chat-ack filter key. `player_chat` writes a nil sender, so the
            // expected value is exact, not a `is_some` direction.
            assert_eq!(*sender, Some(uuid::Uuid::nil()));
            let ChatAckInfo {
                signature,
                global_index,
                was_shown,
                message_index,
                timestamp_millis,
                salt,
                raw_content,
                last_seen,
                verified,
            } = ack.as_ref().expect("signed chat carries ack info");
            assert_eq!(signature.as_slice(), &sig[..]);
            assert_eq!(*global_index, 7);
            assert!(*was_shown, "pass-through filter is shown");
            // The fields this fix stopped discarding — each pinned to the
            // exact (pairwise-distinct) value `player_chat`/`signed_body`
            // wrote, not merely asserted non-zero.
            assert_eq!(*message_index, 3, "SignedMessageLink.index");
            assert_eq!(*timestamp_millis, 1_700_000_000_123);
            assert_eq!(*salt, 99_887_766);
            assert_eq!(raw_content, "hello world", "the signed body's raw content");
            assert!(last_seen.is_empty(), "this fixture's last-seen list is empty");
            assert!(
                !*verified,
                "the adapter never verifies — only the driver can, once it \
                 has the sender's public key"
            );
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
        [Directive::Emit(ClientEvent::Chat { text, kind, ack, .. })] => {
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

/// A network-NBT compound text component `{"text": <text>, "color": <color>}`.
/// Root compound has no name (network NBT), one `text` and one `color` string
/// field, then `TAG_End`. Used to prove the adapter preserves colour/style
/// instead of flattening the component to a bare literal.
fn nbt_colored_component(text: &str, color: &str) -> Vec<u8> {
    fn named_string(name: &str, value: &str) -> Vec<u8> {
        let mut out = vec![0x08];
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value.as_bytes());
        out
    }
    let mut out = vec![0x0A]; // TAG_Compound, nameless root
    out.extend_from_slice(&named_string("text", text));
    out.extend_from_slice(&named_string("color", color));
    out.push(0x00); // TAG_End
    out
}

#[test]
fn system_chat_preserves_component_colour() {
    // Regression: the adapter used to flatten styled components to a bare
    // literal, dropping colour before it crossed `ClientEvent::Chat`.
    let adapter = V770Adapter::new();
    let mut payload = nbt_colored_component("hi", "red");
    payload.push(0x00); // overlay = false
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SYSTEM_CHAT,
            &payload,
        )
        .expect("handle system_chat");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Chat { text, .. })] => {
            assert_eq!(text.to_plain_string(), "hi");
            assert!(
                text.to_legacy_string().starts_with("§c"),
                "red colour must survive, got {:?}",
                text.to_legacy_string()
            );
        }
        other => panic!("expected one system chat event, got {other:?}"),
    }
}

/// `ClientAction::SendSignedChat` must land on the same `chat` packet id as
/// unsigned `SendChat`, with the signature populated and the ack fields in
/// `ChatMessage`'s own field order (`message, timestamp, salt, signature,
/// last_seen_offset, acknowledged, checksum`).
///
/// `timestamp_millis` and `salt` are pairwise-distinct i64s so a transposition
/// of the two would be visible; `last_seen_offset`/`checksum` are likewise
/// distinct from both and from each other. This asserts the wire's own
/// epoch-**millis** unit — the payload `lodestone-auth` signs over is a
/// *different*, epoch-**seconds** value derived from this one and is not on
/// this packet at all, so there is nothing here for it to transpose with; see
/// `lodestone_auth::build_signature_payload`'s doc for that distinction.
#[test]
fn send_signed_chat_encodes_millis_timestamp_and_ack_fields_in_order() {
    let adapter = V770Adapter::new();
    let mut signature = [0u8; 256];
    for (i, b) in signature.iter_mut().enumerate() {
        *b = i as u8;
    }
    let out = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SendSignedChat {
                text: "hello".to_owned(),
                timestamp_millis: 1_700_000_000_123,
                salt: 42,
                signature: signature.to_vec(),
                last_seen_offset: 5,
                acknowledged: [0b0000_0001, 0b0000_0010, 0b0000_0100],
                checksum: -7,
            },
        )
        .expect("encode signed chat");
    let (id, payload) = out.expect("signed chat must produce a packet");
    assert_eq!(id, play::serverbound::CHAT, "same packet id as unsigned chat");

    let mut expected = mc_string("hello");
    expected.extend_from_slice(&1_700_000_000_123i64.to_be_bytes());
    expected.extend_from_slice(&42i64.to_be_bytes());
    expected.push(0x01); // signature present
    expected.extend_from_slice(&signature);
    expected.extend_from_slice(&var_i32(5));
    expected.extend_from_slice(&[0b0000_0001, 0b0000_0010, 0b0000_0100]);
    expected.push((-7i8) as u8);
    assert_eq!(payload, expected);
}

#[test]
fn send_signed_chat_not_encoded_outside_play() {
    let adapter = V770Adapter::new();
    let out = adapter
        .encode_action(
            ConnectionState::Configuration,
            &ClientAction::SendSignedChat {
                text: "hi".to_owned(),
                timestamp_millis: 1,
                salt: 2,
                signature: vec![0u8; 256],
                last_seen_offset: 0,
                acknowledged: [0; 3],
                checksum: 0,
            },
        )
        .expect("encode outside play");
    assert!(out.is_none(), "signed chat only exists in play, got {out:?}");
}

/// `ClientAction::AnnounceChatSession` must encode `chat_session_update`'s
/// field order exactly: session UUID, expiry (epoch millis), then the
/// varint-length-prefixed public key and key signature — mirroring
/// `RemoteChatSession.Data.write`. Public key and key signature are given
/// different lengths and different bytes so a transposition of the two
/// varint-prefixed blocks would be visible.
#[test]
fn announce_chat_session_encodes_uuid_millis_key_then_signature() {
    let adapter = V770Adapter::new();
    let session_id = uuid::Uuid::from_u128(0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
    let out = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::AnnounceChatSession {
                session_id,
                expires_at_millis: 1_700_000_000_123,
                public_key: vec![0x30, 0x81, 0x9f],
                key_signature: vec![0xAA, 0xBB, 0xCC, 0xDD],
            },
        )
        .expect("encode chat session announce");
    let (id, payload) = out.expect("chat session announce must produce a packet");
    assert_eq!(id, play::serverbound::CHAT_SESSION_UPDATE);

    let mut expected = session_id.as_bytes().to_vec();
    expected.extend_from_slice(&1_700_000_000_123i64.to_be_bytes());
    expected.extend_from_slice(&var_i32(3));
    expected.extend_from_slice(&[0x30, 0x81, 0x9f]);
    expected.extend_from_slice(&var_i32(4));
    expected.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(payload, expected);
}

#[test]
fn disguised_chat_preserves_component_colour() {
    let adapter = V770Adapter::new();
    let mut payload = nbt_colored_component("psst", "gold");
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
        [Directive::Emit(ClientEvent::Chat { text, .. })] => {
            assert_eq!(text.to_plain_string(), "psst");
            assert!(
                text.to_legacy_string().starts_with("§6"),
                "gold colour must survive, got {:?}",
                text.to_legacy_string()
            );
        }
        other => panic!("expected one disguised chat event, got {other:?}"),
    }
}
