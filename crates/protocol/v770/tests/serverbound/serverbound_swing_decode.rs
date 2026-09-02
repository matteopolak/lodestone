//! Hermetic byte-exact test for `V770ServerProtocol::decode`'s `SWING` arm.
//!
//! Layout verified independently against 26.2's own serverbound swing packet
//! (confirmed against the decompiled 26.2 source): a single VarInt `InteractionHand` ordinal
//! (`0` main hand, `1` off hand), read via vanilla's own enum reader. Both
//! ordinals fit in one VarInt byte, so the wire body here is one byte.

use lodestone_server::{ServerBound, ServerProtocol};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::play;

#[test]
fn swing_decodes_main_hand() {
    let proto = V770ServerProtocol;
    let decoded = proto.decode(lodestone_core::State::Play, play::serverbound::SWING, &[0]);
    assert!(
        matches!(decoded, ServerBound::Swing { hand: 0 }),
        "expected Swing{{ hand: 0 }}, got {decoded:?}"
    );
}

#[test]
fn swing_decodes_off_hand() {
    let proto = V770ServerProtocol;
    let decoded = proto.decode(lodestone_core::State::Play, play::serverbound::SWING, &[1]);
    assert!(
        matches!(decoded, ServerBound::Swing { hand: 1 }),
        "expected Swing{{ hand: 1 }}, got {decoded:?}"
    );
}

/// The control: an empty frame must not decode to a well-formed variant
/// carrying a fabricated hand value — proving the arm actually consumes a
/// byte rather than defaulting one that was never on the wire.
#[test]
fn swing_rejects_an_empty_frame() {
    let proto = V770ServerProtocol;
    let decoded = proto.decode(lodestone_core::State::Play, play::serverbound::SWING, &[]);
    assert!(
        matches!(decoded, ServerBound::Ignored),
        "an empty frame must decode to Ignored, got {decoded:?}"
    );
}

/// A second control in the other direction: trailing garbage after the one
/// expected byte must also be rejected — proving the arm's `ensure_empty`
/// (inside `decode_full`) actually runs rather than only reading a prefix.
#[test]
fn swing_rejects_a_frame_with_trailing_bytes() {
    let proto = V770ServerProtocol;
    let decoded = proto.decode(
        lodestone_core::State::Play,
        play::serverbound::SWING,
        &[0, 0xff],
    );
    assert!(
        matches!(decoded, ServerBound::Ignored),
        "a frame with trailing bytes must decode to Ignored, got {decoded:?}"
    );
}
