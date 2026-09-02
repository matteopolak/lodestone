//! Issue #273's remaining half, closed: `LanConfig::online_mode` actually
//! reaches an accepted connection through `IntegratedServer::open_to_lan`,
//! not just through `serve_connection_with_online_mode` called directly (that
//! function-level proof is `tests/online_mode.rs`; this file proves the
//! *wiring* one layer up, over a real TCP loopback socket — the same
//! entry point `net.rs`'s "Open to LAN" caller and any future dedicated-server
//! binary would use).
//!
//! Two gates, matching `docs/server-online-mode.md`'s own framing of what a
//! wiring change must show:
//!
//! - [`default_lan_config_stays_offline_no_network_call`]: `LanConfig::default()`
//!   (what `bind` and every pre-existing caller still build) completes a join
//!   with **no** `EncryptionRequest` ever sent — the discriminating half this
//!   file exists for, since a test that only exercises the online path cannot
//!   show the default singleplayer/LAN behaviour survived.
//! - [`lan_config_online_mode_demands_encryption_and_substitutes_identity`]:
//!   setting `LanConfig::online_mode` to `Some` makes the exact same listener
//!   demand the real RSA/AES-128-CFB8 handshake and hand back the session
//!   server's identity, not the client's self-reported one.
//!
//! No real network call anywhere in this file: [`OnlineModeConfig::for_test`]
//! substitutes a fixture for the session-server `hasJoined` check, the same
//! seam `tests/online_mode.rs` uses and for the same reason `CLAUDE.md`
//! records (a pre-existing test reaching a real external service the moment
//! online-mode auth is wired into a code path tests already call). This file
//! adds no new seam; it exercises the existing one one layer further out.

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_net::{Connection, Transport, generate_shared_secret, rsa_encrypt};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer, LanConfig, OnlineModeConfig};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::{handshaking, login};
use lodestone_v26_2::packets::handshake::Intention;
use lodestone_v26_2::packets::login::{EncryptionRequest, EncryptionResponse, LoginFinished, LoginHello};
use tokio::net::TcpStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 776 };

/// Never actually queried: both gates below stop reading once `LOGIN_FINISHED`
/// arrives, before chunk streaming starts — same as `tests/online_mode.rs`'s
/// own `UnusedSource`.
#[derive(Clone, Default)]
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

/// `LanConfig::default()` (`online_mode: None`) reproduces exactly the
/// pre-#273-wiring behaviour: `LOGIN_FINISHED` arrives straight off
/// `LoginHello`, over a plaintext connection, with the client's own claimed
/// name and uuid echoed back unchanged. Every pre-existing `bind`/`open_to_lan`
/// caller builds `LanConfig` this way (see `docs/open-to-lan.md`), so this is
/// the gate proving they are all still untouched by this change.
#[tokio::test]
async fn default_lan_config_stays_offline_no_network_call() {
    let server = IntegratedServer::open_to_lan(
        "127.0.0.1:0",
        V770ServerProtocol,
        UnusedSource,
        LanConfig { view_radius: 0, ..LanConfig::default() },
    )
    .await
    .expect("open_to_lan must bind");
    let addr = server.local_addr().expect("bound listener has a local address");

    let socket = TcpStream::connect(addr).await.expect("loopback connect");
    let mut client = Connection::new(socket);
    let username = "OfflinePlayer";
    let uuid = Uuid::from_u128(0x1111_1111_1111_1111_1111_1111_1111_1111);
    write_login_start(&mut client, username, uuid).await;

    // Offline: the very next packet is LOGIN_COMPRESSION (compression is
    // unconditional, `docs/server-login-compression.md`), never
    // `login::clientbound::HELLO` (the EncryptionRequest) — no RSA keypair
    // was generated, no session-server call was ever possible, because
    // `online_mode` was never `Some` anywhere on this path.
    let (id, payload) = client.read_packet().await.unwrap().expect("server closed early");
    assert_eq!(
        id,
        login::clientbound::LOGIN_COMPRESSION,
        "default LanConfig must go straight to compression, never an EncryptionRequest"
    );
    let threshold = Reader::new(&payload).var_i32().unwrap();
    client.set_compression(threshold);

    let (id, payload) = client.read_packet().await.unwrap().expect("server closed early");
    assert_eq!(id, login::clientbound::LOGIN_FINISHED);
    let mut r = Reader::new(&payload);
    let finished = LoginFinished::decode(&mut r, CTX).unwrap();
    r.ensure_empty().unwrap();
    assert_eq!(finished.name, username, "offline login trusts the client's own claimed name");
    assert_eq!(finished.profile_id, uuid, "offline login trusts the client's own claimed uuid");

    drop(server);
}

/// `LanConfig { online_mode: Some(..), .. }` makes the same listener demand
/// the real handshake and substitute the session server's identity — proving
/// the field actually reaches the accepted connection through `open_to_lan`'s
/// accept loop, not only through calling `serve_connection_with_online_mode`
/// directly.
#[tokio::test]
async fn lan_config_online_mode_demands_encryption_and_substitutes_identity() {
    let claimed_username = "ClaimedName";
    let claimed_uuid = Uuid::from_u128(0x2222_2222_2222_2222_2222_2222_2222_2222);
    let real_username = "RealMojangName";
    let real_uuid = Uuid::from_u128(0x3333_3333_3333_3333_3333_3333_3333_3333);

    let online_mode = OnlineModeConfig::for_test(move |username, _hash| {
        // Deliberately differs from what the client claims (per this repo's
        // evidence standard: a value equal to the neighbour cannot show a
        // substitution happened at all), and the fixture's own `username`
        // argument is asserted to be the client's claimed one — the same
        // input `has_joined` would really be called with.
        assert_eq!(username, claimed_username);
        Ok(Some(lodestone_auth::HasJoinedProfile {
            id: real_uuid,
            name: real_username.to_owned(),
            properties: Vec::new(),
        }))
    });

    let server = IntegratedServer::open_to_lan(
        "127.0.0.1:0",
        V770ServerProtocol,
        UnusedSource,
        LanConfig { view_radius: 0, online_mode: Some(online_mode), ..LanConfig::default() },
    )
    .await
    .expect("open_to_lan must bind");
    let addr = server.local_addr().expect("bound listener has a local address");

    let socket = TcpStream::connect(addr).await.expect("loopback connect");
    let mut client = Connection::new(socket);
    write_login_start(&mut client, claimed_username, claimed_uuid).await;

    // Online: the next packet must be the EncryptionRequest, not
    // LOGIN_FINISHED — the discriminating assertion for "this host demands
    // encryption".
    let (id, payload) = client.read_packet().await.unwrap().expect("server closed early");
    assert_eq!(id, login::clientbound::HELLO, "online LanConfig must send an EncryptionRequest");
    let mut r = Reader::new(&payload);
    let request = EncryptionRequest::decode(&mut r, CTX).unwrap();
    r.ensure_empty().unwrap();
    assert!(request.should_authenticate);

    let secret = generate_shared_secret();
    let enc_secret = rsa_encrypt(&request.public_key, &secret).unwrap();
    let enc_token = rsa_encrypt(&request.public_key, &request.challenge).unwrap();
    let mut w = Writer::default();
    EncryptionResponse { shared_secret: enc_secret, verify_token: enc_token }
        .encode(&mut w, CTX)
        .unwrap();
    client.write_packet(login::serverbound::KEY, w.as_slice()).await.unwrap();
    client.enable_encryption(&secret).unwrap();

    // LOGIN_COMPRESSION, then LOGIN_FINISHED, both now enciphered — the same
    // ordering `docs/server-login-compression.md`/`docs/server-online-mode.md`
    // document. `Connection::read_packet` decrypts transparently once
    // `enable_encryption` above has run, but this end must also activate the
    // same compression threshold before decoding `LOGIN_FINISHED` (sent
    // compressed), exactly as `tests/online_mode.rs`'s own gate does.
    let (id, payload) = client.read_packet().await.unwrap().expect("server closed early");
    assert_eq!(id, login::clientbound::LOGIN_COMPRESSION);
    let threshold = Reader::new(&payload).var_i32().unwrap();
    client.set_compression(threshold);
    let (id, payload) = client.read_packet().await.unwrap().expect("server closed early");
    assert_eq!(id, login::clientbound::LOGIN_FINISHED);
    let mut r = Reader::new(&payload);
    let finished = LoginFinished::decode(&mut r, CTX).unwrap();
    r.ensure_empty().unwrap();
    assert_eq!(
        finished.name, real_username,
        "online login must hand back the session server's name, not the client's claim"
    );
    assert_eq!(
        finished.profile_id, real_uuid,
        "online login must hand back the session server's uuid, not the client's claim"
    );

    drop(server);
}
