use lodestone_model::{AdapterError, ConnectionState, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_3::{connection_dialect, packet_ids};
use lodestone_world::World;

#[test]
fn selected_configuration_post_effects_decode_strictly_and_are_accepted() {
    let adapter = V770Adapter::with_connection_dialect(connection_dialect());
    assert_eq!(packet_ids::configuration::clientbound::POST_EFFECTS, 10);
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/new_packet_wire_controls_26_3.json",
    )).unwrap();
    let encoded = fixture["cases"]["post_effects_ordered"]["body_hex"].as_str().unwrap();
    let body: Vec<u8> = encoded.as_bytes().chunks_exact(2).map(|pair| {
        u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()
    }).collect();
    assert!(adapter.handle_packet(&mut World::new(), ConnectionState::Configuration, 10, &body)
        .unwrap().is_empty());
    assert!(matches!(adapter.handle_packet(&mut World::new(), ConnectionState::Configuration, 10, &body[..body.len() - 1]),
        Err(AdapterError::Decode(_))));
}

/// 26.3 acknowledges a teleport with the pose in the acknowledgement itself and
/// sends no movement after it; the server disconnects a client that sends two
/// positioned movement packets in one tick. The 26.2 dialect is the control: it
/// still owes the echo.
#[test]
fn teleport_corrections_send_no_movement_echo_on_26_3() {
    use lodestone_model::{ClientAction, Rotation, Vec3};
    let action = ClientAction::Move {
        pos: Vec3::new(-472.5, 69.0, -392.5),
        rotation: Rotation::new(0.0, 0.0),
        on_ground: false,
        horizontal_collision: false,
    };
    let latest = V770Adapter::with_connection_dialect(connection_dialect());
    assert_eq!(latest.encode_correction_echo(ConnectionState::Play, &action).unwrap(), None);
    let previous = V770Adapter::new();
    assert!(previous.encode_correction_echo(ConnectionState::Play, &action).unwrap().is_some());
}
