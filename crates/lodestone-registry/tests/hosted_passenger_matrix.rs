//! Cross-family invariant for riding on the wire: every hosted protocol that
//! can spawn an entity must also carry that entity's passenger list.
//!
//! The entity streamer sends passenger lists through one route
//! (`ServerProtocol::encode_set_passengers`), whose default emits nothing. A
//! family that streams entities but keeps the default would show riders
//! standing beside their vehicle with no error anywhere, so this walks every
//! hosted row and compares the two encoders.

use lodestone_model::{Rotation, Vec3};
use lodestone_server::{EntitySnapshot, ServerDirective};
use uuid::Uuid;

fn snapshot() -> EntitySnapshot {
    EntitySnapshot {
        id: 7,
        uuid: Uuid::nil(),
        entity_type: "minecraft:oak_boat".parse().expect("a valid key"),
        position: Vec3::new(0.0, 64.0, 0.0),
        rotation: Rotation::new(0.0, 0.0),
        head_yaw: 0.0,
        velocity: Vec3::new(0.0, 0.0, 0.0),
        on_ground: false,
        metadata: Vec::new(),
        object_data: 0,
        equipment: Vec::new(),
        leash_link: None,
        passengers: Vec::new(),
    }
}

#[test]
fn a_hosted_family_that_spawns_entities_also_encodes_passenger_lists() {
    let hosted = lodestone_registry::hosted_protocols();
    let mut passenger_hosts = Vec::new();
    for protocol in &hosted {
        let proto = lodestone_registry::server_protocol_for_protocol(*protocol)
            .expect("a hosted protocol resolves");
        let spawns = proto.encode_add_entity(&snapshot()) != ServerDirective::None;
        let carries = proto.encode_set_passengers(7, &[8]) != ServerDirective::None;
        if carries {
            passenger_hosts.push(*protocol);
        }
        assert!(
            carries || !spawns,
            "protocol {protocol} spawns entities but drops passenger lists"
        );
    }
    // Hosts known to carry the list; losing one would leave riders unseated on
    // that wire without tripping the rule above.
    for protocol in [404, 776, 777] {
        if hosted.contains(&protocol) {
            assert!(
                passenger_hosts.contains(&protocol),
                "protocol {protocol} must encode passenger lists"
            );
        }
    }
}
