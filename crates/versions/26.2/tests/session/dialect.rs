use lodestone_model::{
    ClientAction, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_v26_2::dialect::{PacketTables, ProtocolDialect, StatePackets, V26_2_PACKET_TABLES};
use lodestone_v26_2::V770Adapter;
use lodestone_world::World;
use uuid::Uuid;

// Synthetic identifiers deliberately collide across states and directions.
fn synthetic_tables() -> PacketTables {
    PacketTables {
        handshaking: StatePackets {
            clientbound: &[],
            serverbound: &[("minecraft:intention", 20)],
        },
        login: StatePackets {
            clientbound: &[("minecraft:login_finished", 62)],
            serverbound: &[
                ("minecraft:hello", 60),
                ("minecraft:key", 61),
                ("minecraft:login_acknowledged", 63),
            ],
        },
        configuration: StatePackets {
            clientbound: &[
                ("minecraft:keep_alive", 40),
                ("minecraft:finish_configuration", 41),
                ("minecraft:registry_data", 42),
                ("minecraft:update_tags", 43),
                ("test:new_packet", 4),
            ],
            serverbound: &[
                ("minecraft:client_information", 60),
                ("minecraft:keep_alive", 61),
                ("minecraft:finish_configuration", 62),
            ],
        },
        ..V26_2_PACKET_TABLES
    }
}

fn synthetic_adapter() -> V770Adapter {
    V770Adapter::with_connection_dialect(
        ProtocolDialect::connection_only(19001, &["synthetic"], synthetic_tables()).unwrap(),
    )
}

#[test]
fn selected_identity_reaches_literal_handshake_and_state_scoped_login() {
    let adapter = synthetic_adapter();
    assert_eq!(adapter.protocol_version(), 19001);
    assert_eq!(adapter.minecraft_versions(), &["synthetic"]);
    assert!(adapter.supports(19001));
    assert!(!adapter.supports(776));
    let directives = adapter
        .begin_login(
            &LoginProfile {
                username: "A".into(),
                uuid: Uuid::nil(),
            },
            &ServerAddress {
                host: "x".into(),
                port: 25565,
            },
        )
        .unwrap();
    assert_eq!(
        directives[0],
        Directive::Send {
            packet_id: 20,
            payload: vec![0xb9, 0x94, 0x01, 1, b'x', 0x63, 0xdd, 2],
        }
    );
    assert_eq!(directives[1], Directive::SetState(ConnectionState::Login));
    let mut hello = vec![1, b'A'];
    hello.extend_from_slice(&[0; 16]);
    assert_eq!(
        directives[2],
        Directive::Send { packet_id: 60, payload: hello }
    );
}

#[test]
fn login_finished_routes_acknowledgement_then_configuration_settings() {
    // UUID, one-character profile name, zero properties, session UUID.
    let mut payload = vec![0; 16];
    payload.extend_from_slice(&[1, b'A', 0]);
    payload.extend_from_slice(&[0; 16]);
    let directives = synthetic_adapter()
        .handle_packet(&mut World::new(), ConnectionState::Login, 62, &payload)
        .unwrap();
    assert_eq!(
        directives[0],
        Directive::Send { packet_id: 63, payload: vec![] }
    );
    assert_eq!(
        directives[1],
        Directive::SetState(ConnectionState::Configuration)
    );
    assert!(matches!(
        &directives[2],
        Directive::Send { packet_id: 60, payload } if !payload.is_empty()
    ));
}

#[test]
fn keep_alive_uses_both_directional_tables_and_rejects_numeric_alias() {
    let adapter = synthetic_adapter();
    let payload = [1, 2, 3, 4, 5, 6, 7, 8];
    let expected = Directive::Send {
        packet_id: 61,
        payload: payload.to_vec(),
    };
    assert_eq!(
        adapter.handle_packet(
            &mut World::new(),
            ConnectionState::Configuration,
            40,
            &payload,
        ).unwrap(),
        vec![expected]
    );
    assert_eq!(
        adapter.encode_action(
            ConnectionState::Configuration,
            &ClientAction::KeepAliveResponse { id: 0x0102_0304_0506_0708 },
        ).unwrap(),
        Some((61, payload.to_vec()))
    );

    // The baseline's keep-alive number names an unrelated packet in this table.
    let error = adapter
        .handle_packet(&mut World::new(), ConnectionState::Configuration, 4, &payload)
        .unwrap_err();
    assert!(error.to_string().contains("unmapped packet 4"));
    assert_eq!(
        V770Adapter::new().handle_packet(
            &mut World::new(),
            ConnectionState::Configuration,
            4,
            &payload,
        ).unwrap(),
        vec![Directive::Send { packet_id: 4, payload: payload.to_vec() }]
    );
}

#[test]
fn encryption_reply_uses_login_identifiers() {
    assert_eq!(
        synthetic_adapter()
            .build_encryption_response(&[0x12], &[0x34, 0x56])
            .unwrap(),
        Directive::Send {
            packet_id: 61,
            payload: vec![1, 0x12, 2, 0x34, 0x56],
        }
    );
}

#[test]
fn unreviewed_registry_and_play_paths_fail_before_side_effects() {
    let adapter = synthetic_adapter();
    for id in [41, 42, 43] {
        let error = adapter
            .handle_packet(&mut World::new(), ConnectionState::Configuration, id, &[])
            .unwrap_err();
        assert!(error.to_string().contains("no reviewed"), "{error}");
    }
    assert!(adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, 0, &[])
        .is_err());
    assert!(adapter
        .decode_chunk_packet(ConnectionState::Play, 0, &[])
        .is_err());
    let action = ClientAction::KeepAliveResponse { id: 1 };
    assert!(adapter.encode_action(ConnectionState::Play, &action).is_err());
    assert!(adapter
        .encode_correction_echo(ConnectionState::Play, &action)
        .is_err());
    assert!(V770Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Configuration, 3, &[])
        .unwrap()
        .contains(&Directive::SetState(ConnectionState::Play)));
}

#[test]
fn ambiguous_or_invalid_identifiers_are_rejected_at_construction() {
    for entries in [
        &[("a", 1), ("b", 1)][..],
        &[("a", 1), ("a", 2)][..],
        &[("a", -1)][..],
        &[("", 1)][..],
    ] {
        let mut tables = synthetic_tables();
        tables.configuration.clientbound = entries;
        assert!(ProtocolDialect::connection_only(19001, &["synthetic"], tables).is_err());
    }
    assert!(ProtocolDialect::connection_only(19001, &["synthetic"], synthetic_tables()).is_ok());
}
