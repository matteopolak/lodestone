//! Configuration packet bytes captured from the official 26.3 server.

use lodestone_model::{ConnectionState, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_3::{connection_dialect, packet_ids};
use lodestone_world::World;

// Complete 67-byte world_clock registry body from the protocol 777 capture.
const WORLD_CLOCK_HEX: &str = "156d696e6563726166743a776f726c645f636c6f636b02136d696e6563726166743a6f766572776f726c64010a00116d696e6563726166743a7468655f656e64010a00";

fn captured_world_clock() -> Vec<u8> {
    WORLD_CLOCK_HEX
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn captured_registry_body_reaches_shared_configuration_decoder() {
    let adapter = V770Adapter::with_connection_dialect(connection_dialect());
    let body = captured_world_clock();
    assert_eq!(body.len(), 67);
    assert_eq!(packet_ids::configuration::clientbound::REGISTRY_DATA, 7);
    assert!(adapter
        .handle_packet(&mut World::new(), ConnectionState::Configuration, 7, &body)
        .unwrap()
        .is_empty());

    let mut trailing = body.clone();
    trailing.push(0);
    assert!(adapter
        .handle_packet(&mut World::new(), ConnectionState::Configuration, 7, &trailing)
        .is_err());

    let mut wrong_count = body;
    wrong_count[22] = 3;
    assert!(adapter
        .handle_packet(&mut World::new(), ConnectionState::Configuration, 7, &wrong_count)
        .is_err());
}

#[test]
fn finish_configuration_admits_play_and_tags_reach_their_decoder() {
    let adapter = V770Adapter::with_connection_dialect(connection_dialect());
    assert_eq!(packet_ids::configuration::clientbound::UPDATE_TAGS, 14);
    let finish = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Configuration,
            packet_ids::configuration::clientbound::FINISH_CONFIGURATION,
            &[],
        )
        .expect("26.3 finish_configuration is admitted");
    assert!(!finish.is_empty(), "finishing Configuration must acknowledge and enter Play");
    // An empty body is not a valid tag packet; the error must come from the
    // decoder, not the admission gate.
    let error = adapter
        .handle_packet(&mut World::new(), ConnectionState::Configuration, 14, &[])
        .unwrap_err();
    assert!(!error.to_string().contains("no reviewed"), "{error}");
}
