//! Independent wire fixtures for the player-readiness marker.
//!
//! The packet has a genuinely empty body. The trailing-byte control ensures
//! the decoder does not turn malformed readiness signals into state changes.

use lodestone_core::State;
use lodestone_server::{ServerBound, ServerProtocol};
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::V770ServerProtocol;

#[test]
fn player_loaded_decodes_empty_body() {
    assert!(matches!(
        V770ServerProtocol.decode(State::Play, play::serverbound::PLAYER_LOADED, &[]),
        ServerBound::PlayerLoaded
    ));
}

#[test]
fn player_loaded_rejects_trailing_bytes() {
    assert!(matches!(
        V770ServerProtocol.decode(State::Play, play::serverbound::PLAYER_LOADED, &[0]),
        ServerBound::Ignored
    ));
}
