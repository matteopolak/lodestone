//! Issue #273's online-mode login sequence, driven end to end against the
//! real [`V770ServerProtocol`] over an in-memory duplex: a hand-written
//! "client" completes the real RSA/AES handshake (the same
//! `lodestone_net::rsa_encrypt` the real client driver calls), and
//! [`OnlineModeConfig::for_test`] substitutes a fixture for the
//! session-server call — the seam that exists specifically so this test does
//! not do what the near-miss `CLAUDE.md` records warns about (a test reaching
//! a real external service the moment online-mode auth is wired into a code
//! path tests already call). No network call anywhere in this file, by
//! construction. See `docs/server-online-mode.md`.
//!
//! An external integration test rather than a unit test inside
//! `lodestone-server::server`, and that's load-bearing, not a style choice:
//! `lodestone-v770` has a *normal* dependency on `lodestone-server` for the
//! `ServerProtocol` trait, so adding it as a *dev*-dependency for a unit test
//! would make this crate's own lib-test compilation and the copy
//! `lodestone-v770` links against two different instantiations of the same
//! trait (measured: `V770ServerProtocol: ServerProtocol` reported
//! unimplemented against the crate's own trait). An external test binary
//! depends on this crate exactly once, normally, so the self-reference never
//! arises — see [`lodestone_server::OnlineModeConfig::for_test`]'s own doc
//! comment for the full account.

use std::sync::Arc;

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_net::{Connection, Transport, memory_pair, rsa_encrypt};
use lodestone_server::{
    BlockEntityHandle, BlockTickFeed, ChunkColumn, ChunkSource, CommandDispatch, ExplosionFeed,
    MobHandle, NoEntities, OnlineModeConfig, PluginChannelRegistry, ResourcePackPushFeed,
    ServeSummary, ServerError, TicketStoreHandle, access::AccessHandle,
    serve_connection_with_online_mode, world_state::WorldStateHandle,
};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::{handshaking, login};
use lodestone_v26_2::packets::handshake::Intention;
use lodestone_v26_2::packets::login::{EncryptionRequest, EncryptionResponse, LoginFinished, LoginHello};
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 776 };

/// A `ChunkSource` that is never actually queried: every test here stops
/// reading once `LOGIN_FINISHED` arrives, before any chunk streaming begins.
struct UnusedSource;
impl ChunkSource for UnusedSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        unimplemented!("this test never reaches chunk streaming")
    }
    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        unimplemented!("this test never reaches chunk streaming")
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        unimplemented!("this test never reaches chunk streaming")
    }
}

/// Writes the handshake + `LoginHello`, matching `V770ServerProtocol::decode`'s
/// real wire expectations exactly.
async fn write_login_start<T: Transport>(client: &mut Connection<T>, username: &str, profile_id: Uuid) {
    let mut w = Writer::default();
    Intention {
        protocol_version: 776,
        host: "localhost".to_owned(),
        port: 25565,
        next_state: 2,
    }
    .encode(&mut w, CTX)
    .unwrap();
    client
        .write_packet(handshaking::serverbound::INTENTION, w.as_slice())
        .await
        .unwrap();

    let mut w = Writer::default();
    LoginHello { name: username.to_owned(), profile_id }.encode(&mut w, CTX).unwrap();
    client.write_packet(login::serverbound::HELLO, w.as_slice()).await.unwrap();
}

/// Reads the `EncryptionRequest`, answers it with a real RSA-wrapped secret
/// and the request's own echoed challenge (exactly what a real client does),
/// and enables the client-side cipher.
async fn complete_encryption<T: Transport>(client: &mut Connection<T>) {
    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, login::clientbound::HELLO, "expected an EncryptionRequest");
    let mut r = Reader::new(&payload);
    let request = EncryptionRequest::decode(&mut r, CTX).unwrap();
    r.ensure_empty().unwrap();
    assert_eq!(request.server_id, "", "vanilla's own server-id is always empty");
    assert!(request.should_authenticate);

    let secret = lodestone_net::generate_shared_secret();
    let enc_secret = rsa_encrypt(&request.public_key, &secret).unwrap();
    let enc_token = rsa_encrypt(&request.public_key, &request.challenge).unwrap();

    let mut w = Writer::default();
    EncryptionResponse { shared_secret: enc_secret, verify_token: enc_token }
        .encode(&mut w, CTX)
        .unwrap();
    client.write_packet(login::serverbound::KEY, w.as_slice()).await.unwrap();
    client.enable_encryption(&secret).unwrap();
}

/// The full sequence, parameterised by the fixture `verify` closure so every
/// case below shares one body.
///
/// Returns the still-running server task alongside the client connection,
/// **not** its awaited result: a verified join keeps `serve_connection_inner`
/// running (waiting for `LoginAcknowledged`, exactly as a real client would
/// send next), so awaiting the task before the caller has even read
/// `LOGIN_FINISHED` would deadlock. Each test decides for itself whether the
/// task is expected to have already finished (the refusal cases) or is still
/// alive (the success case, where the caller reads `LOGIN_FINISHED` and then
/// simply drops both ends without completing the rest of login).
async fn run_online_login(
    client_username: &str,
    client_uuid: Uuid,
    verify: impl Fn(String, String) -> lodestone_auth::Result<Option<lodestone_auth::HasJoinedProfile>>
    + Send
    + Sync
    + 'static,
) -> (
    tokio::task::JoinHandle<Result<ServeSummary, ServerError>>,
    Connection<tokio::io::DuplexStream>,
) {
    let (client_end, server_end) = memory_pair();
    let source: Arc<UnusedSource> = Arc::new(UnusedSource);
    let world = WorldStateHandle::default();
    let online_mode = OnlineModeConfig::for_test(verify);

    let server = tokio::spawn(async move {
        let mut server_conn = Connection::new(server_end);
        serve_connection_with_online_mode(
            &mut server_conn,
            &V770ServerProtocol,
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
            &world,
            &AccessHandle::default(),
            None,
            &online_mode,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    write_login_start(&mut client, client_username, client_uuid).await;
    complete_encryption(&mut client).await;

    (server, client)
}

/// A verified join: the session server's identity (not the client's
/// self-reported one) is what reaches `LOGIN_FINISHED` — the entire point of
/// the check.
#[tokio::test]
async fn a_verified_join_sends_the_session_servers_identity_not_the_clients() {
    let claimed_uuid = Uuid::from_u128(1); // what the client offers
    let real_uuid = Uuid::from_u128(2); // what the session server says
    assert_ne!(claimed_uuid, real_uuid, "fixture must be distinguishable");

    let (server, mut client) = run_online_login("ClaimedName", claimed_uuid, move |user, hash| {
        assert_eq!(user, "ClaimedName");
        assert!(!hash.is_empty(), "a real server_hash was computed");
        Ok(Some(lodestone_auth::HasJoinedProfile {
            id: real_uuid,
            name: "RealName".to_owned(),
            properties: Vec::new(),
        }))
    })
    .await;

    // `login_success` sends `LOGIN_COMPRESSION` before `LOGIN_FINISHED`
    // (`docs/server-login-compression.md`) — activate the same threshold on
    // this end before reading further, or `LOGIN_FINISHED` (sent compressed)
    // decodes as garbage.
    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, login::clientbound::LOGIN_COMPRESSION, "expected the compression packet first");
    let threshold = Reader::new(&payload).var_i32().unwrap();
    client.set_compression(threshold);

    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, login::clientbound::LOGIN_FINISHED);
    let mut r = Reader::new(&payload);
    let finished = LoginFinished::decode(&mut r, CTX).unwrap();
    assert_eq!(finished.profile_id, real_uuid, "must use the session server's uuid");
    assert_eq!(finished.name, "RealName", "must use the session server's name");

    // The server keeps running past this point (waiting for
    // `LoginAcknowledged`, exactly as a real client would send next) — this
    // test only asserts the login sequence itself, not the rest of the join.
    // Aborting rather than awaiting avoids leaking a background task that
    // will otherwise sit blocked on `read_packet` until the runtime shuts
    // down.
    server.abort();
    drop(client);
}

/// An unverified join: the session server says "no", and the connection is
/// refused rather than let in under the claimed name.
#[tokio::test]
async fn an_unverified_join_is_refused() {
    let (server, _client) =
        run_online_login("Someone", Uuid::from_u128(3), |_user, _hash| Ok(None)).await;
    let result = server.await.expect("server task panicked");
    assert!(
        matches!(result, Err(ServerError::UnverifiedUsername)),
        "expected UnverifiedUsername, got {result:?}"
    );
}

/// A session-server transport failure is a distinct outcome from "verified:
/// no" — see `ServerError::AuthServiceUnavailable`'s own doc comment for why
/// the two must not be folded together.
#[tokio::test]
async fn a_session_server_error_is_reported_distinctly_from_unverified() {
    let (server, _client) = run_online_login("Someone", Uuid::from_u128(4), |_user, _hash| {
        Err(lodestone_auth::AuthError::Service {
            step: "has_joined",
            message: "simulated outage".to_owned(),
        })
    })
    .await;
    let result = server.await.expect("server task panicked");
    assert!(
        matches!(result, Err(ServerError::AuthServiceUnavailable(_))),
        "expected AuthServiceUnavailable, got {result:?}"
    );
}
