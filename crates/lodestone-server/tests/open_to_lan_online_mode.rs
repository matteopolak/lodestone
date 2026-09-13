//! `LanConfig::online_mode` reaches an accepted connection through
//! `IntegratedServer::open_to_lan`, not only through a direct call to
//! `serve_connection_with_online_mode`. The tests exercise that wiring one
//! layer up over a real TCP loopback socket, matching the entry point used by
//! the server's Open to LAN path.
//!
//! Two gates cover the observable behavior of the wiring:
//!
//! - [`default_lan_config_stays_offline_no_network_call`]: `LanConfig::default()`
//!   (what `bind` and its callers build) completes a join
//!   with **no** `EncryptionRequest` ever sent — the discriminating half this
//!   file exists for, since an online-only test cannot show that the default
//!   singleplayer/LAN behavior remains plaintext.
//! - [`lan_config_online_mode_demands_encryption_and_substitutes_identity`]:
//!   setting `LanConfig::online_mode` to `Some` makes the exact same listener
//!   demand the real RSA/AES-128-CFB8 handshake and hand back the session
//!   server's identity, not the client's self-reported one.
//!
//! No external network call occurs in this file: [`OnlineModeConfig::for_test`]
//! substitutes a fixture for the session-server `hasJoined` check. The fixture
//! keeps the accepted-connection test deterministic while the loopback socket
//! still exercises the real listener and login wiring.

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

/// Never queried: both gates below stop reading once `LOGIN_FINISHED` arrives,
/// before chunk streaming starts.
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

/// `LanConfig::default()` (`online_mode: None`) completes a plaintext login:
/// `LOGIN_FINISHED` follows `LoginHello`, and the client's claimed name and
/// uuid are echoed unchanged. This is the default behavior for callers that do
/// not opt into online-mode authentication.
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
    // unconditional), never
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
        // Deliberately differs from what the client claims: a value equal to the
        // input cannot demonstrate substitution. The fixture's own `username`
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

    // LOGIN_COMPRESSION, then LOGIN_FINISHED, both enciphered. The client side
    // decrypts transparently once `enable_encryption` above has run, but it must
    // also activate the compression threshold before decoding LOGIN_FINISHED,
    // which is sent compressed.
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
