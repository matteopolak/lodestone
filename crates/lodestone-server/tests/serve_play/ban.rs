/// **A banned uuid is refused at login**, before `login_success`, with
/// the canonical refusal translation key on the wire — and the identical connection is
/// admitted once the ban is lifted.
///
/// The lifted-ban arm is the control: without it, a test that only asserts the
/// refusal cannot tell "the ban was enforced" from "this fixture never joins".
#[tokio::test]
async fn a_banned_uuid_is_refused_at_login_and_admitted_once_pardoned() {
    use lodestone_server::access::{AccessHandle, BanEntry};

    // `FakeProtocol` decodes every login as `Uuid::nil()`, so that is the identity
    // to ban.
    let access = AccessHandle::default();
    access.with(|lists| {
        lists.ban(
            Uuid::nil(),
            BanEntry::permanent("tester", "gate", "no reason at all"),
        );
    });

    let (client_end, server_end) = memory_pair();
    let mut client = Connection::new(client_end);
    let serving = {
        let access = access.clone();
        tokio::spawn(async move {
            let mut conn = Connection::new(server_end);
            lodestone_server::serve_connection_with_access(
                &mut conn,
                &FakeProtocol,
                &AirSource,
                &NoEntities,
                0,
                &access,
                None,
            )
            .await
        })
    };

    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("tester");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");

    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(
        id, DISCONNECT_S2C,
        "a banned uuid must be disconnected, not sent login_success"
    );
    let mut r = Reader::new(&payload);
    let reason = r.string(256).expect("reason");
    assert!(
        reason.starts_with("multiplayer.disconnect.banned.reason"),
        "the refusal must carry vanilla's own translation key; got {reason:?}"
    );
    assert!(reason.contains("no reason at all"), "and the ban's reason: {reason:?}");

    let outcome = serving.await.expect("server task panicked");
    assert!(
        matches!(outcome, Err(ServerError::AccessDenied(_))),
        "the refusal must be reported as AccessDenied; got {outcome:?}"
    );

    // Control: pardon and the same sequence joins.
    access.with(|lists| assert!(lists.pardon(Uuid::nil())));
    let (client_end, server_end) = memory_pair();
    let mut client = Connection::new(client_end);
    let serving = {
        let access = access.clone();
        tokio::spawn(async move {
            let mut conn = Connection::new(server_end);
            lodestone_server::serve_connection_with_access(
                &mut conn,
                &FakeProtocol,
                &AirSource,
                &NoEntities,
                0,
                &access,
                None,
            )
            .await
        })
    };
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("tester");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    let (id, _payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(
        id, LOGIN_SUCCESS,
        "a pardoned uuid must reach login_success"
    );
    drop(client);
    let _ = serving.await.expect("server task panicked");
}
