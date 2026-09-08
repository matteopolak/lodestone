//! Literal serverbound death/respawn controls for hosted protocols 756 and
//! 758.
//!
//! `client_command` is a single VarInt action. The zero byte is the external
//! wire fixture for perform-respawn; this test deliberately does not encode a
//! packet through the version crate before handing it to the registry-selected
//! server protocol.

use lodestone_core::State;
use lodestone_server::{ServerBound, ServerProtocol};
use lodestone_v1_17::{packet_ids, packet_ids_758, PROTOCOL_1_17_1, PROTOCOL_1_18_2};

fn assert_respawn(protocol: i32, packet_id: i32) {
    let server = lodestone_registry::server_protocol_for_protocol(protocol)
        .expect("every hosted v1.17-era row must select a server protocol");
    assert_eq!(
        server.decode(State::Play, packet_id, &[0x00]),
        ServerBound::ClientCommand { action: 0 }
    );
}

#[test]
fn hosted_756_and_758_accept_literal_perform_respawn() {
    assert_respawn(
        PROTOCOL_1_17_1,
        packet_ids::play::serverbound::CLIENT_COMMAND,
    );
    assert_respawn(
        PROTOCOL_1_18_2,
        packet_ids_758::play::serverbound::CLIENT_COMMAND,
    );
}

#[test]
fn client_command_requires_an_exact_body() {
    let server = lodestone_registry::server_protocol_for_protocol(PROTOCOL_1_18_2)
        .expect("protocol 758 must select a server protocol");
    assert_eq!(
        server.decode(
            State::Play,
            packet_ids_758::play::serverbound::CLIENT_COMMAND,
            &[0x00, 0xff],
        ),
        ServerBound::Ignored,
        "a trailing byte must not reach the respawn consumer"
    );
}
