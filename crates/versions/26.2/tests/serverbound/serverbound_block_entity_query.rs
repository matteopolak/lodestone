//! Operator query fixtures and the real client's entity-inspection response path.

use lodestone_core::{Nbt, State};
use lodestone_model::BlockPos;
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v26_2::{V770ServerProtocol, packet_ids::play};

#[test]
fn entity_tag_query_decodes_both_transaction_and_entity_ids() {
    let decoded = V770ServerProtocol.decode(
        State::Play, play::serverbound::ENTITY_TAG_QUERY, &[0xac, 0x02, 0xc1, 0x03],
    );
    assert!(matches!(decoded, ServerBound::EntityTagQuery {
        transaction_id: 300, entity_id: 449,
    }), "valid entity query was discarded or changed: {decoded:?}");
    for body in [&[][..], &[0xac][..], &[0xac, 2][..], &[0xac, 2, 0xc1][..], &[0xac, 2, 0xc1, 3, 0][..]] {
        assert!(matches!(V770ServerProtocol.decode(State::Play, play::serverbound::ENTITY_TAG_QUERY, body), ServerBound::Ignored));
    }
}

#[tokio::test]
async fn entity_tag_query_reaches_the_real_client_response_stream() {
    use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
    use lodestone_core::{Reader, read_network_nbt};
    use lodestone_model::{ClientAction, ClientEvent};
    use lodestone_server::{IntegratedServer, WorldgenChunkSource};
    use lodestone_worldgen::density::Density;
    use std::time::Duration;

    let source = WorldgenChunkSource::new(Density::YClampedGradient {
        from_y: -64.0, to_y: 64.0, from_value: 1.0, to_value: -1.0,
    }, -64, 384);
    let (server, io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol, source, (0..=0, 0..=0), (0, 0), 0, 0,
    );
    let (mut handle, mut events) = ClientBuilder::new(
        ServerAddress { host: "memory".into(), port: 0 },
        LoginProfile { username: "Inspector".into(), uuid: uuid::Uuid::new_v4() },
        Box::new(lodestone_v26_2::adapter()),
    ).connect_with(io);
    let mut stage = "waiting for terrain initialization";
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        while server.mobs().unwrap().with(|sim| sim.next_id()) < 1000 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let requested_id = server.spawn_mob(
            "minecraft:cow".parse().unwrap(), lodestone_model::Vec3::new(8.5, 2.0, 8.5),
        ).unwrap();
        stage = "waiting for mob spawn";
        let (entity_id, uuid) = loop {
            if let ClientEvent::EntitySpawned { entity_id, uuid, .. } =
                events.recv().await.expect("client stream stays open until spawn")
                && entity_id == requested_id
            {
                break (entity_id, uuid.unwrap());
            }
        };
        stage = "waiting for query response";
        handle.send_action(ClientAction::QueryEntityTag { transaction_id: 300, entity_id }).unwrap();
        loop {
            if let ClientEvent::TagQueryResponse { transaction_id: 300, tag } =
                events.recv().await.expect("client stream stays open until query response")
            {
                let bytes = tag.expect("live entity has a compound response");
                let mut reader = Reader::new(&bytes);
                let Nbt::Compound(fields) = read_network_nbt(&mut reader).unwrap() else {
                    panic!("entity query response must be a compound");
                };
                reader.ensure_empty().unwrap();
                assert!(fields.iter().any(|(key, value)| key == "Health" && matches!(value, Nbt::Float(_))));
                let expected_uuid = Nbt::IntArray(uuid.as_bytes().chunks_exact(4)
                    .map(|bytes| i32::from_be_bytes(bytes.try_into().unwrap())).collect());
                assert!(fields.iter().any(|(key, value)| key == "UUID" && *value == expected_uuid));
                assert!(!fields.iter().any(|(key, _)| key == "id"));
                break;
            }
        }
    }).await;
    handle.shutdown();
    server.shutdown().await;
    assert!(result.is_ok(), "entity query timed out: {stage}");
}

#[test]
fn block_entity_query_decodes_position_and_transaction() {
    // x=-3, z=5, y=-17, in the 26/26/12-bit packed position layout.
    let packed = (((-3_i64) & 0x3ff_ffff) << 38) | (5_i64 << 12) | 0xfef;
    let mut body = vec![0xac, 0x02]; // 300
    body.extend_from_slice(&packed.to_be_bytes());
    assert!(matches!(
        V770ServerProtocol.decode(State::Play, play::serverbound::BLOCK_ENTITY_TAG_QUERY, &body),
        ServerBound::BlockEntityTagQuery { transaction_id: 300, pos }
            if pos == BlockPos::new(-3, -17, 5)
    ));
    for length in 0..body.len() {
        assert!(matches!(
            V770ServerProtocol.decode(State::Play, play::serverbound::BLOCK_ENTITY_TAG_QUERY, &body[..length]),
            ServerBound::Ignored
        ));
    }
    body.push(0);
    assert!(matches!(
        V770ServerProtocol.decode(State::Play, play::serverbound::BLOCK_ENTITY_TAG_QUERY, &body),
        ServerBound::Ignored
    ));
}

#[test]
fn block_entity_query_reply_is_nullable_network_nbt_through_boxed_protocol() {
    let proto: Box<dyn ServerProtocol> = Box::new(V770ServerProtocol);
    assert_eq!(proto.encode_tag_query(300, None), ServerDirective::Send {
        packet_id: play::clientbound::TAG_QUERY,
        payload: vec![0xac, 0x02, 0],
    });
    let tag = Nbt::Compound(vec![("v".into(), Nbt::Int(37))]);
    assert_eq!(proto.encode_tag_query(300, Some(&tag)), ServerDirective::Send {
        packet_id: play::clientbound::TAG_QUERY,
        // Unnamed compound, integer named v, value 37, compound terminator.
        payload: vec![0xac, 0x02, 10, 3, 0, 1, b'v', 0, 0, 0, 37, 0],
    });
}
