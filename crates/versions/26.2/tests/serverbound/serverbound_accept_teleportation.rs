//! Byte-exact decode coverage for the serverbound acknowledgement of a
//! player-position correction. The id is deliberately non-zero: the former
//! discarded marker made an all-zero fixture unable to distinguish a decoded
//! id from an invented default.

use lodestone_core::{Reader, State, Writer};
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::V770ServerProtocol;

#[test]
fn accept_teleportation_carries_the_wire_id_to_the_server() {
    let id = 0x1f_2345;
    let mut payload = Writer::default();
    payload.var_i32(id);

    let decoded = V770ServerProtocol.decode(
        State::Play,
        play::serverbound::ACCEPT_TELEPORTATION,
        &payload.into_vec(),
    );

    assert!(
        matches!(decoded, ServerBound::TeleportationAccepted { id: actual } if actual == id),
        "expected acknowledgement id {id}, got {decoded:?}"
    );
}

#[test]
fn accept_teleportation_rejects_a_truncated_varint() {
    let decoded = V770ServerProtocol.decode(
        State::Play,
        play::serverbound::ACCEPT_TELEPORTATION,
        &[0x80],
    );

    assert!(
        matches!(decoded, ServerBound::Ignored),
        "a truncated acknowledgement must not invent an id, got {decoded:?}"
    );
}

#[test]
fn server_selected_id_leads_the_position_correction() {
    let id = 0x1f_2345;
    let directive = V770ServerProtocol.encode_teleport_with_id(id, 1.5, 64.0, -3.25, 90.0, -15.0);
    let ServerDirective::Send { packet_id, payload } = directive else {
        panic!("teleport encoder must emit one player-position packet");
    };
    assert_eq!(packet_id, play::clientbound::PLAYER_POSITION);
    assert_eq!(
        Reader::new(&payload).var_i32(),
        Ok(id),
        "the acknowledgement id must precede all position fields"
    );
}
