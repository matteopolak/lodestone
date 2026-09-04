//! Protocol 5 player-list identity tests from literal wire bodies.

use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_world::World;

fn player_info_body(name: &str, online: bool, ping: i16) -> Vec<u8> {
    assert!(name.len() < 128, "fixture uses a one-byte string length");
    let mut body = Vec::with_capacity(name.len() + 4);
    body.push(name.len() as u8);
    body.extend_from_slice(name.as_bytes());
    body.push(u8::from(online));
    body.extend_from_slice(&ping.to_be_bytes());
    body
}

#[test]
fn add_and_remove_keep_the_name_only_identity_shape() {
    let adapter = lodestone_v1_7::adapter_for(lodestone_v1_7::PROTOCOL);
    let mut world = World::new();

    let add = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            lodestone_v1_7::packet_ids::play::clientbound::PLAYER_INFO,
            &player_info_body("Legacy", true, 37),
        )
        .expect("add packet decodes");
    match add.as_slice() {
        [Directive::Emit(ClientEvent::PlayerListUpdate { entries })] => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].uuid, None);
            assert_eq!(entries[0].name.as_deref(), Some("Legacy"));
            assert_eq!(entries[0].latency, Some(37));
        }
        other => panic!("expected one player-list update, got {other:?}"),
    }

    let remove = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            lodestone_v1_7::packet_ids::play::clientbound::PLAYER_INFO,
            &player_info_body("Legacy", false, 0),
        )
        .expect("remove packet decodes");
    assert_eq!(
        remove,
        vec![Directive::Emit(ClientEvent::PlayerListRemoveByName {
            profile_names: vec!["Legacy".into()],
        })]
    );
}

#[test]
fn named_entity_spawn_preserves_the_only_honest_name_to_uuid_correlation() {
    let adapter = lodestone_v1_7::adapter_for(lodestone_v1_7::PROTOCOL);
    let mut world = World::new();
    let uuid = uuid::Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0);
    let mut body = Vec::new();
    body.push(7); // one-byte VarInt entity id
    body.push(36); // UUID string length
    body.extend_from_slice(uuid.hyphenated().to_string().as_bytes());
    body.push(6); // player-name length
    body.extend_from_slice(b"Legacy");
    body.push(0); // profile-property count
    body.extend_from_slice(&[0; 12]); // fixed-point x/y/z
    body.extend_from_slice(&[0, 0]); // yaw/pitch
    body.extend_from_slice(&0_i16.to_be_bytes()); // held item
    body.push(0x7f); // empty metadata terminator

    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            lodestone_v1_7::packet_ids::play::clientbound::NAMED_ENTITY_SPAWN,
            &body,
        )
        .expect("named player spawn decodes");

    assert!(directives.iter().any(|directive| {
        matches!(
            directive,
            Directive::Emit(ClientEvent::PlayerProfileNamed {
                entity_id: 7,
                profile_name,
            }) if profile_name == "Legacy"
        )
    }));
}
