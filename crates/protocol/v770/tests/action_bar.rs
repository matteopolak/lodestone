//! Hermetic tests for protocol 776 `set_action_bar_text` dispatch.
//!
//! The action bar carries a single trusted network-NBT text component and
//! always renders as an overlay, so it maps to a `GameInfo` chat event. Payloads
//! are hand-built from the wire spec (`TAG_String` root), and a trailing byte is
//! asserted to fail decode so a misparse cannot slip through `ensure_empty`.

use lodestone_model::{ChatKind, ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

/// Network-NBT bare string component: `TAG_String` tag, big-endian u16 length,
/// then the UTF-8 bytes.
fn nbt_string(text: &str) -> Vec<u8> {
    let mut out = vec![0x08];
    out.extend_from_slice(&(text.len() as u16).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    out
}

#[test]
fn set_action_bar_text_emits_game_info_chat() {
    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SET_ACTION_BAR_TEXT,
            &nbt_string("Go!"),
        )
        .expect("handle set_action_bar_text");
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::Chat { text, kind })] => {
            assert_eq!(text.to_plain_string(), "Go!");
            assert_eq!(*kind, ChatKind::GameInfo);
        }
        other => panic!("expected a single GameInfo chat event, got {other:?}"),
    }
}

#[test]
fn set_action_bar_text_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = nbt_string("hi");
    payload.push(0xFF); // one stray byte past the component
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_ACTION_BAR_TEXT,
        &payload,
    );
    assert!(
        result.is_err(),
        "a trailing byte must fail decode, got {result:?}"
    );
}
