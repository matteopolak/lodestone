//! Literal serverbound death/respawn controls for hosted protocols 756 and
//! 758.
//!
//! `client_command` is a single VarInt action. The zero byte is the external
//! wire fixture for perform-respawn; this test deliberately does not encode a
//! packet through the version crate before handing it to the registry-selected
//! server protocol.

use lodestone_core::State;
use lodestone_model::{ClientEvent, ConnectionState, Directive, Text, Vec3, VersionAdapter};
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_17::{packet_ids, packet_ids_758, PROTOCOL_1_17_1, PROTOCOL_1_18_2};
use lodestone_world::World;

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

#[test]
fn production_death_and_respawn_encoders_match_independent_wire_controls() {
    let death_body = [
        0x01, 0xff, 0xff, 0xff, 0xff, 0x14, 0x7b, 0x22, 0x74, 0x65, 0x78, 0x74, 0x22, 0x3a,
        0x22, 0x59, 0x6f, 0x75, 0x20, 0x64, 0x69, 0x65, 0x64, 0x22, 0x7d,
    ];
    let position_body = [
        0x3f, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x40, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xc0, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x2a, 0x00,
    ];

    for &(protocol, death_id, respawn_id, position_id) in &[
        (
            PROTOCOL_1_17_1,
            packet_ids::play::clientbound::DEATH_COMBAT_EVENT,
            packet_ids::play::clientbound::RESPAWN,
            packet_ids::play::clientbound::POSITION,
        ),
        (
            PROTOCOL_1_18_2,
            packet_ids_758::play::clientbound::DEATH_COMBAT_EVENT,
            packet_ids_758::play::clientbound::RESPAWN,
            packet_ids_758::play::clientbound::POSITION,
        ),
    ] {
        let server = lodestone_registry::server_protocol_for_protocol(protocol)
            .expect("hosted protocol must select its server implementation");
        let ServerDirective::Send { packet_id, payload } =
            server.encode_player_combat_kill(1, &Text::literal("You died"))
        else {
            panic!("death must be emitted for protocol {protocol}");
        };
        assert_eq!((packet_id, payload), (death_id, death_body.to_vec()));

        let packets = server.encode_respawn_with_teleport_id(42, Vec3::new(1.5, 64.0, -2.25));
        assert_eq!(packets.len(), 2);
        let [
            ServerDirective::Send {
                packet_id: actual_respawn_id,
                payload: respawn,
            },
            ServerDirective::Send {
                packet_id: actual_position_id,
                payload: position,
            },
        ] = packets.as_slice()
        else {
            panic!("respawn must emit state and position packets for protocol {protocol}");
        };
        assert_eq!((*actual_respawn_id, respawn.len()), (respawn_id, respawn.len()));
        assert_eq!(*actual_position_id, position_id);
        assert_eq!(position, &position_body);

        let directives = lodestone_v1_17::adapter_for(protocol)
            .handle_packet(&mut World::new(), ConnectionState::Play, *actual_respawn_id, respawn)
            .expect("production respawn payload must reach the adapter");
        assert!(matches!(
            directives.as_slice(),
            [Directive::Emit(ClientEvent::Respawned { .. })]
        ));
    }
}
