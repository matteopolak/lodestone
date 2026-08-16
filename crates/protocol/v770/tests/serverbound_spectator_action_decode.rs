//! Hermetic byte-exact test for `V770ServerProtocol::decode`'s
//! `SPECTATOR_ACTION` arm.
//!
//! Layout verified independently against 26.2's
//! `ServerboundSpectatorActionPacket` (`.cache/mc/26.2/src/net/minecraft/
//! network/protocol/game/ServerboundSpectatorActionPacket.java`): a single
//! `ByteBufCodecs.OPTIONAL_VAR_INT` — an offset-encoded VarInt where `0`
//! means "absent" and a present id `i` is written as `i + 1`. This is the
//! *opposite* convention from a bool-prefixed `Option`, which is exactly why
//! this arm hand-decodes rather than deriving.

use lodestone_server::{ServerBound, ServerProtocol};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::play;

fn var_i32_bytes(mut value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value = ((value as u32) >> 7) as i32;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
    out
}

#[test]
fn spectator_action_decodes_a_present_target() {
    let proto = V770ServerProtocol;
    // Entity id 41 (pairwise-distinct from the offset-encoded wire value 42,
    // so a forgotten `+1`/`-1` cannot survive unnoticed).
    let payload = var_i32_bytes(42);
    let decoded = proto.decode(
        lodestone_core::State::Play,
        play::serverbound::SPECTATOR_ACTION,
        &payload,
    );
    assert!(
        matches!(
            decoded,
            ServerBound::SpectatorAction {
                target_entity_id: Some(41)
            }
        ),
        "expected SpectatorAction{{ target_entity_id: Some(41) }}, got {decoded:?}"
    );
}

#[test]
fn spectator_action_decodes_an_absent_target() {
    let proto = V770ServerProtocol;
    let payload = var_i32_bytes(0);
    let decoded = proto.decode(
        lodestone_core::State::Play,
        play::serverbound::SPECTATOR_ACTION,
        &payload,
    );
    assert!(
        matches!(
            decoded,
            ServerBound::SpectatorAction {
                target_entity_id: None
            }
        ),
        "expected SpectatorAction{{ target_entity_id: None }}, got {decoded:?}"
    );
}

/// The control: trailing garbage after the one expected VarInt must not
/// decode to a well-formed variant — proving the arm actually validates the
/// frame was fully consumed rather than reading a prefix and ignoring the
/// rest.
#[test]
fn spectator_action_rejects_a_frame_with_trailing_bytes() {
    let proto = V770ServerProtocol;
    let mut payload = var_i32_bytes(42);
    payload.push(0xff);
    let decoded = proto.decode(
        lodestone_core::State::Play,
        play::serverbound::SPECTATOR_ACTION,
        &payload,
    );
    assert!(
        matches!(decoded, ServerBound::Ignored),
        "a frame with trailing bytes must decode to Ignored, got {decoded:?}"
    );
}
