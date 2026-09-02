//! Hermetic tests for protocol 776 clientbound `ping` decoding.
//!
//! `ping` (distinct from `keep_alive`) carries a single big-endian `i32`
//! challenge id and is emitted as `ClientEvent::Ping` in both the
//! configuration and play states; the caller is responsible for replying with
//! `ClientAction::PongResponse`. Confirmed against 26.2's
//! `ClientboundPingPacket` / `ServerboundPongPacket`.

use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::{configuration, play};
use lodestone_world::World;

fn handle(
    adapter: &V770Adapter,
    state: ConnectionState,
    id: i32,
    payload: &[u8],
) -> Vec<Directive> {
    adapter
        .handle_packet(&mut World::new(), state, id, payload)
        .expect("handle ping packet")
}

#[test]
fn play_ping_emits_client_event_ping() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        ConnectionState::Play,
        play::clientbound::PING,
        &42i32.to_be_bytes(),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::Ping { id: 42 })]
    );
}

#[test]
fn configuration_ping_emits_client_event_ping() {
    let adapter = V770Adapter::new();
    let directives = handle(
        &adapter,
        ConnectionState::Configuration,
        configuration::clientbound::PING,
        &7i32.to_be_bytes(),
    );
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::Ping { id: 7 })]
    );
}
