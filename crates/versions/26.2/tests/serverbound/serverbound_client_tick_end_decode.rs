//! Independent wire fixtures for the client tick boundary marker.
//!
//! The marker's body is empty. The trailing-byte control proves that a frame
//! with data appended does not advance the connection's movement boundary.

use lodestone_core::State;
use lodestone_server::{ServerBound, ServerProtocol};
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::V770ServerProtocol;

#[test]
fn client_tick_end_decodes_empty_body() {
    assert!(matches!(
        V770ServerProtocol.decode(State::Play, play::serverbound::CLIENT_TICK_END, &[]),
        ServerBound::ClientTickEnded
    ));
}

#[test]
fn client_tick_end_rejects_trailing_bytes() {
    assert!(matches!(
        V770ServerProtocol.decode(State::Play, play::serverbound::CLIENT_TICK_END, &[0]),
        ServerBound::Ignored
    ));
}
