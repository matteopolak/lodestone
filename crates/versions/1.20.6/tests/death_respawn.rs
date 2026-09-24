//! Independent protocol-766 death notification fixtures.
//!
//! The packet body is written from the protocol shape (player id, killer
//! entity id, JSON component), rather than through a packet encoder. That
//! keeps the dispatch and text conversion checks useful if an encoder later
//! repeats the same mistake as the decoder.

use lodestone_model::{route, ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_v1_20_6::{adapter_for, packet_ids, PROTOCOL_1_20_6};
use lodestone_world::World;

fn hex(input: &str) -> Vec<u8> {
    assert_eq!(input.len() % 2, 0, "fixture has an odd number of hex digits");
    (0..input.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&input[index..index + 2], 16).expect("fixture is hex"))
        .collect()
}

#[test]
fn death_combat_event_emits_the_model_death_event() {
    // Player id 300 (ac 02), killer entity id 17, and a 59-byte JSON
    // component. These fields distinguish VarInt/i32/string decoding and the
    // dispatch id independently from any local packet encoder.
    let body = hex(
        "ac02000000113b7b227472616e736c617465223a2264656174682e61747461636b2e66616c6c222c2277697468223a5b7b2274657874223a225374657665227d5d7d",
    );
    let directives = adapter_for(PROTOCOL_1_20_6)
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            packet_ids::play::clientbound::DEATH_COMBAT_EVENT,
            &body,
        )
        .expect("literal protocol-766 death fixture decodes");

    let [Directive::Emit(event)] = directives.as_slice() else {
        panic!("expected one death event, got {directives:?}");
    };
    let ClientEvent::Death { message } = event else {
        panic!("expected Death, got {event:?}");
    };
    assert_eq!(message.to_plain_string(), "Steve fell from a high place");
    let routed = route(event);
    assert!(routed.session, "death must update session state");
    assert!(routed.shell, "death must reach the shell death screen");
    assert!(routed.client, "death must reach the client respawn policy");
}

#[test]
fn death_combat_event_rejects_trailing_bytes() {
    let mut body = hex(
        "01000000003b7b227472616e736c617465223a2264656174682e61747461636b2e66616c6c222c2277697468223a5b7b2274657874223a225374657665227d5d7d",
    );
    body.push(0xff);
    let error = adapter_for(PROTOCOL_1_20_6)
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            packet_ids::play::clientbound::DEATH_COMBAT_EVENT,
            &body,
        )
        .expect_err("trailing bytes must not be silently accepted");
    assert!(error.to_string().contains("trailing"), "unexpected error: {error}");
}
