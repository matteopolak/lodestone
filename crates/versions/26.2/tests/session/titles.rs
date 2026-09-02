//! Hermetic tests for the protocol 776 title packets `set_title_text`,
//! `set_subtitle_text`, `clear_titles`, and `set_titles_animation`.
//!
//! Title/subtitle text carry a single trusted network-NBT text component
//! (same shape as `set_action_bar_text`); payloads are hand-built from the
//! wire spec (`TAG_String` root), and a trailing byte is asserted to fail
//! decode so a misparse cannot slip through `ensure_empty`.

use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
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

/// Network-NBT bare string component: `TAG_String` tag, big-endian u16 length,
/// then the UTF-8 bytes.
fn nbt_string(text: &str) -> Vec<u8> {
    let mut out = vec![0x08];
    out.extend_from_slice(&(text.len() as u16).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    out
}

// ---- set_title_text ---------------------------------------------------

#[test]
fn set_title_text_emits_title_event() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::SET_TITLE_TEXT,
        &nbt_string("Victory"),
    );
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::TitleText { text })] => {
            assert_eq!(text.to_plain_string(), "Victory");
        }
        other => panic!("expected a single TitleText event, got {other:?}"),
    }
}

#[test]
fn set_title_text_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = nbt_string("hi");
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::SET_TITLE_TEXT, &payload);
}

// ---- set_subtitle_text --------------------------------------------------

#[test]
fn set_subtitle_text_emits_subtitle_event() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        play::clientbound::SET_SUBTITLE_TEXT,
        &nbt_string("Defeat"),
    );
    match directives.as_slice() {
        [Directive::Emit(ClientEvent::SubtitleText { text })] => {
            assert_eq!(text.to_plain_string(), "Defeat");
        }
        other => panic!("expected a single SubtitleText event, got {other:?}"),
    }
}

#[test]
fn set_subtitle_text_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = nbt_string("hi");
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::SET_SUBTITLE_TEXT, &payload);
}

// ---- clear_titles --------------------------------------------------------

#[test]
fn clear_titles_true_resets_times() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::CLEAR_TITLES, &[1]);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::TitlesCleared {
            reset_times: true
        })]
    );
}

#[test]
fn clear_titles_false_keeps_times() {
    let adapter = V770Adapter::new();
    let directives = handle(&adapter, play::clientbound::CLEAR_TITLES, &[0]);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::TitlesCleared {
            reset_times: false
        })]
    );
}

#[test]
fn clear_titles_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    expect_err(&adapter, play::clientbound::CLEAR_TITLES, &[0, 0xFF]);
}

// ---- set_titles_animation --------------------------------------------------

#[test]
fn set_titles_animation_decodes_raw_ints() {
    let adapter = V770Adapter::new();
    let mut payload = 10i32.to_be_bytes().to_vec();
    payload.extend_from_slice(&70i32.to_be_bytes());
    payload.extend_from_slice(&20i32.to_be_bytes());
    let directives = handle(&adapter, play::clientbound::SET_TITLES_ANIMATION, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::TitlesAnimation {
            fade_in: 10,
            stay: 70,
            fade_out: 20,
        })]
    );
}

#[test]
fn set_titles_animation_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    let mut payload = 0i32.to_be_bytes().to_vec();
    payload.extend_from_slice(&0i32.to_be_bytes());
    payload.extend_from_slice(&0i32.to_be_bytes());
    payload.push(0xFF);
    expect_err(&adapter, play::clientbound::SET_TITLES_ANIMATION, &payload);
}

#[test]
fn set_titles_animation_rejects_truncated_payload() {
    let adapter = V770Adapter::new();
    let mut payload = 0i32.to_be_bytes().to_vec();
    payload.extend_from_slice(&0i32.to_be_bytes());
    // missing fade_out
    expect_err(&adapter, play::clientbound::SET_TITLES_ANIMATION, &payload);
}
