//! Hermetic tests for the clientbound `start_configuration` transition.
//!
//! The server can push a live client from play back into configuration
//! mid-session (resource-pack/datapack reloads and `transfer` flows). The
//! packet body is empty; the client must acknowledge it with a serverbound
//! `configuration_acknowledged` (also empty) on the play protocol and then
//! switch its own state to `Configuration` so subsequent packets decode
//! correctly. Getting either half wrong desyncs the stream and every following
//! packet misparses.

use lodestone_model::{ConnectionState, Directive, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

#[test]
fn start_configuration_acks_and_switches_to_configuration() {
    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::START_CONFIGURATION,
            &[],
        )
        .expect("start_configuration handled");

    match directives.as_slice() {
        [Directive::Send { packet_id, payload }, Directive::SetState(next)] => {
            assert_eq!(
                *packet_id,
                play::serverbound::CONFIGURATION_ACKNOWLEDGED,
                "must acknowledge on the play protocol"
            );
            assert!(
                payload.is_empty(),
                "configuration_acknowledged has an empty body, got {} bytes",
                payload.len()
            );
            assert_eq!(
                *next,
                ConnectionState::Configuration,
                "must switch back into configuration"
            );
        }
        other => panic!(
            "expected [Send(configuration_acknowledged), SetState(Configuration)], got {other:?}"
        ),
    }
}

#[test]
fn start_configuration_rejects_trailing_bytes() {
    let adapter = V770Adapter::new();
    // The packet is a unit codec: any payload is a misparse and must fail loudly
    // rather than silently advancing the state machine.
    let result = adapter.handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::START_CONFIGURATION,
        &[0x00],
    );
    assert!(
        result.is_err(),
        "a trailing byte after the empty body must fail, got {result:?}"
    );
}
