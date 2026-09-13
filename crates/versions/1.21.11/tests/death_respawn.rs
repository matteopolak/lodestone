//! Protocol-774 death/respawn controls.
//!
//! The clientbound death body and serverbound respawn body are literal wire
//! captures. The server encoder checks below use a different direction and
//! prove that the production `ServerProtocol` reaches the same packet ids and
//! byte layouts.

use lodestone_core::State;
use lodestone_model::{route, ClientEvent, ConnectionState, Directive, Text, Vec3, VersionAdapter};
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v1_21_11::{adapter_for, packet_ids, V774ServerProtocol, PROTOCOL};
use lodestone_world::World;

fn hex(input: &str) -> Vec<u8> {
    input
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("fixture is hexadecimal"))
        .collect()
}

#[test]
fn captured_player_combat_kill_reaches_the_death_screen_route() {
    // Captured player id 1 plus a network-NBT compound containing the literal
    // death message. The adapter must consume the complete anonymous-NBT body.
    let body = hex("01 0a 08 00 04 74 65 78 74 00 08 59 6f 75 20 64 69 65 64 00");
    let directives = adapter_for(PROTOCOL)
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            packet_ids::play::clientbound::PLAYER_COMBAT_KILL,
            &body,
        )
        .expect("captured protocol-774 death body decodes");
    let [Directive::Emit(event)] = directives.as_slice() else {
        panic!("expected one death event, got {directives:?}");
    };
    let ClientEvent::Death { message } = event else {
        panic!("expected Death, got {event:?}");
    };
    assert_eq!(message.to_plain_string(), "You died");
    let routed = route(event);
    assert!(routed.session && routed.shell && routed.client);
}

#[test]
fn captured_client_command_perform_respawn_reaches_the_server_consumer() {
    let protocol = V774ServerProtocol;
    assert_eq!(
        protocol.decode(
            State::Play,
            packet_ids::play::serverbound::CLIENT_COMMAND,
            &[0x00],
        ),
        ServerBound::ClientCommand { action: 0 }
    );
    assert_eq!(
        protocol.decode(
            State::Play,
            packet_ids::play::serverbound::CLIENT_COMMAND,
            &[0x00, 0xff],
        ),
        ServerBound::Ignored,
        "trailing bytes must not reach the respawn consumer"
    );
}

#[test]
fn production_death_and_respawn_encoders_match_wire_controls() {
    let protocol = V774ServerProtocol;
    let ServerDirective::Send {
        packet_id,
        payload,
    } = protocol.encode_player_combat_kill(1, &Text::literal("You died"))
    else {
        panic!("death encoder must send a packet");
    };
    assert_eq!(packet_id, packet_ids::play::clientbound::PLAYER_COMBAT_KILL);
    assert_eq!(
        payload,
        hex("01 0a 08 00 04 74 65 78 74 00 08 59 6f 75 20 64 69 65 64 00")
    );

    let directives = protocol.encode_respawn_with_teleport_id(
        42,
        Vec3::new(1.5, 64.0, -2.25),
    );
    let [
        ServerDirective::Send {
            packet_id: respawn_id,
            payload: respawn,
        },
        ServerDirective::Send {
            packet_id: position_id,
            payload: position,
        },
    ] = directives.as_slice()
    else {
        panic!("respawn must send the state reset and placement pair");
    };
    assert_eq!(*respawn_id, packet_ids::play::clientbound::RESPAWN);
    assert_eq!(*position_id, packet_ids::play::clientbound::PLAYER_POSITION);
    assert_eq!(
        respawn,
        &hex("00 13 6d 69 6e 65 63 72 61 66 74 3a 6f 76 65 72 77 6f 72 6c 64 00 00 00 00 00 00 00 00 00 ff 00 01 00 00 3f 00")
    );
    assert_eq!(
        position,
        &hex("2a 3f f8 00 00 00 00 00 00 40 50 00 00 00 00 00 00 c0 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00")
    );
}
