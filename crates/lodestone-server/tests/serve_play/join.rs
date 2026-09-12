use super::common::*;

/// A protocol from before the Configuration phase enters Play immediately
/// after login success. The server must use the capability rather than waiting
/// for acknowledgements that this wire cannot carry.
#[tokio::test]
async fn legacy_protocol_enters_play_without_configuration_acknowledgements() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &LegacyProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
    });

    let mut client = Connection::new(client_end);
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("Legacy");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");

    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, LOGIN_SUCCESS);
    assert_eq!(Reader::new(&payload).string(16).unwrap(), "Legacy");

    let (id, _payload) = tokio::time::timeout(Duration::from_secs(1), client.read_packet())
        .await
        .expect("legacy login should enter Play without waiting for an ack")
        .expect("read")
        .expect("packet");
    assert_eq!(
        id, SET_TIME_S2C,
        "the normal Play join sequence must begin after legacy login"
    );
    let batches = drain_join_view(&mut client, 1).await;
    assert_eq!(batches, vec![1]);

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// Online-mode legacy login applies encryption before login success, then
/// enters the same single Play join sequence without configuration acks.
#[tokio::test]
async fn encrypted_legacy_protocol_enters_play_without_configuration_acknowledgements() {
    let profile_id = Uuid::from_u128(77);
    let online_mode = OnlineModeConfig::for_test(move |username, hash| {
        assert_eq!(username, "EncryptedLegacy");
        assert!(!hash.is_empty(), "the server must compute a session hash");
        Ok(Some(lodestone_auth::HasJoinedProfile {
            id: profile_id,
            name: "VerifiedLegacy".to_owned(),
            properties: Vec::new(),
        }))
    });
    let (client_end, server_end) = memory_pair();
    let source = Arc::new(AirSource);
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection_with_online_mode(
            &mut conn,
            &OnlineLegacyProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
            &TicketStoreHandle::default(),
            &BlockTickFeed::default(),
            &ExplosionFeed::default(),
            &CommandDispatch::none(),
            &ResourcePackPushFeed::default(),
            &PluginChannelRegistry::default(),
            &WorldStateHandle::default(),
            &AccessHandle::default(),
            None,
            &online_mode,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut writer = Writer::default();
    writer.string("EncryptedLegacy");
    client
        .write_packet(LOGIN_START, writer.as_slice())
        .await
        .expect("login start");

    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, ENCRYPTION_REQUEST_S2C);
    let mut reader = Reader::new(&payload);
    let public_key = reader.var_bytes(4096).expect("public key").to_vec();
    let challenge = reader.var_bytes(4096).expect("challenge").to_vec();
    reader.ensure_empty().expect("encryption request payload");

    let secret = generate_shared_secret();
    let encrypted_secret = rsa_encrypt(&public_key, &secret).expect("encrypt shared secret");
    let encrypted_challenge = rsa_encrypt(&public_key, &challenge).expect("encrypt challenge");
    let mut writer = Writer::default();
    writer
        .var_bytes(&encrypted_secret)
        .expect("encrypted shared secret length");
    writer
        .var_bytes(&encrypted_challenge)
        .expect("encrypted challenge length");
    client
        .write_packet(ENCRYPTION_RESPONSE_C2S, writer.as_slice())
        .await
        .expect("encryption response");
    client.enable_encryption(&secret).expect("enable client encryption");

    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, LOGIN_SUCCESS);
    assert_eq!(Reader::new(&payload).string(16).unwrap(), "VerifiedLegacy");

    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, SET_TIME_S2C);
    assert_eq!(drain_join_view(&mut client, 1).await, vec![1]);
    assert!(
        matches!(
            client
                .read_packet_timeout(Duration::from_millis(50))
                .await,
            Err(NetError::Timeout { .. })
        ),
        "legacy login must emit no configuration acknowledgements or duplicate join"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **Negative control**: the default capability keeps modern protocols in the
/// acknowledgement-driven Configuration phase until both packets arrive.
#[tokio::test(start_paused = true)]
async fn configuration_protocol_still_waits_for_both_acknowledgements() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
    });

    let mut client = Connection::new(client_end);
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("Modern");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, LOGIN_SUCCESS);

    assert!(
        tokio::time::timeout(Duration::from_millis(50), client.read_packet())
            .await
            .is_err(),
        "a default modern protocol must not enter Play before acknowledgements"
    );
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, SET_TIME_S2C);
    assert_eq!(drain_join_view(&mut client, 1).await, vec![1]);

    drop(client);
    let _ = server.await.expect("server task panicked");
}
