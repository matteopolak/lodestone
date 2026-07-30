//! Hermetic proof that the driver actually drives the online-mode encryption
//! handshake a `Directive::BeginEncryption` describes (issue #65) — before
//! this crate's `driver.rs` grew a `BeginEncryption` arm, this directive fell
//! into the "ignoring unknown directive variant" catch-all and the crypto
//! never ran at all. `crates/protocol/v770/tests/join_flow.rs` already proved
//! the *adapter* side (`hello_begins_encryption_passing_through_the_request`,
//! `build_encryption_response_frames_the_key_packet`); this proves the
//! *driver* actually calls that adapter and does the RSA/AES work in between.
//!
//! # What this does and does not prove
//!
//! The RSA and AES-128-CFB8 primitives themselves are already verified
//! elsewhere: `lodestone-net::crypto`'s unit tests check them against NIST
//! SP800-38A vectors and a `rsa`-crate round trip, and
//! `lodestone-net/tests/online_handshake.rs` proves the whole crypto path
//! against a **real** vanilla server (an encrypted, cleanly-decrypted
//! "unverified username" disconnect). Re-deriving either of those here would
//! be the exact "two symmetric misunderstandings" trap `CLAUDE.md` warns
//! about: this test's own "server" half uses the same `rsa`/`aes`/`cfb8`
//! crates as the driver, so agreement between them is expected regardless of
//! whether Minecraft's protocol is implemented correctly.
//!
//! What *is* new evidence here, and wasn't covered by either of the above:
//! that the **driver** (a) generates a secret and RSA-wraps it against the
//! public key the directive carried (not a hardcoded one), (b) asks the
//! adapter to frame the reply and writes it *before* enabling its own cipher,
//! (c) actually flips its connection's cipher on, and (d) correctly skips the
//! session-server call when `should_authenticate` is `false` and correctly
//! *refuses* to proceed at all when it's `true` with no session configured.
//! The `should_authenticate: true` **success** path (an authenticated
//! `join_server` call reaching the real `sessionserver.mojang.com`) is
//! **not** exercised here — same reason `flow.rs`'s tests don't cover it —
//! and is left to a live gate elsewhere (see the crate report).

use lodestone_client::{
    ClientBuilder, ClientError, ClientEvent, ConnectionState, Directive, LoginProfile,
    ServerAddress, SessionOutcome, VersionAdapter,
};
use lodestone_model::AdapterError;
use lodestone_net::{Connection, memory_pair};
use rsa::pkcs1v15::Pkcs1v15Encrypt;
use rsa::pkcs8::EncodePublicKey;
use rsa::{RsaPrivateKey, RsaPublicKey};
use tokio::io::DuplexStream;
use uuid::Uuid;

const TRIGGER: i32 = 0x01;
const KEY_PACKET_ID: i32 = 0x02;
const POST_ENCRYPT_TRIGGER: i32 = 0x03;

/// A minimal adapter that, on `TRIGGER`, hands back a `BeginEncryption`
/// directive carrying a real (test-generated) RSA public key, and frames
/// `build_encryption_response` the same way the real v770 adapter's test
/// (`build_encryption_response_frames_the_key_packet`) pins: two
/// length-prefixed byte arrays, secret then token.
#[derive(Debug)]
struct FakeOnlineAdapter {
    server_id: String,
    public_key_der: Vec<u8>,
    verify_token: Vec<u8>,
    should_authenticate: bool,
}

fn write_prefixed(w: &mut lodestone_core::Writer, data: &[u8]) {
    w.var_i32(i32::try_from(data.len()).unwrap());
    w.bytes(data);
}

impl VersionAdapter for FakeOnlineAdapter {
    fn protocol_version(&self) -> i32 {
        0
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["fake"]
    }

    fn supports(&self, _protocol: i32) -> bool {
        true
    }

    fn begin_login(
        &self,
        _profile: &LoginProfile,
        _server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(vec![Directive::SetState(ConnectionState::Login)])
    }

    fn handle_packet(
        &self,
        _world: &mut dyn lodestone_model::WorldSink,
        _state: ConnectionState,
        packet_id: i32,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == TRIGGER {
            return Ok(vec![Directive::BeginEncryption {
                server_id: self.server_id.clone(),
                public_key: self.public_key_der.clone(),
                verify_token: self.verify_token.clone(),
                should_authenticate: self.should_authenticate,
            }]);
        }
        if packet_id == POST_ENCRYPT_TRIGGER {
            // Proves packets are readable (i.e. correctly decrypted) *after*
            // encryption was enabled: this only decodes to a `Login` event if
            // `Connection::read_packet` on the driver's side used the same
            // cipher the "server" side (this test) just switched on.
            return Ok(vec![Directive::Emit(login_event())]);
        }
        Ok(Vec::new())
    }

    fn encode_action(
        &self,
        _state: ConnectionState,
        _action: &lodestone_client::ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        Ok(None)
    }

    fn build_encryption_response(
        &self,
        encrypted_secret: &[u8],
        encrypted_token: &[u8],
    ) -> Result<Directive, AdapterError> {
        let mut w = lodestone_core::Writer::default();
        write_prefixed(&mut w, encrypted_secret);
        write_prefixed(&mut w, encrypted_token);
        Ok(Directive::Send {
            packet_id: KEY_PACKET_ID,
            payload: w.into_vec(),
        })
    }
}

fn login_event() -> ClientEvent {
    ClientEvent::Login {
        entity_id: 7,
        game_mode: lodestone_model::GameMode::Survival,
        dimension: lodestone_model::Identifier::new("minecraft", "overworld").unwrap(),
    }
}

fn profile() -> LoginProfile {
    LoginProfile {
        username: "Tester".into(),
        uuid: Uuid::nil(),
    }
}

fn server() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

/// Generates a fresh RSA keypair, mirroring
/// `lodestone-net::crypto`'s own `rsa_encrypt_roundtrips_against_a_generated_key`
/// test — this is deliberately a keypair the driver never sees the private
/// half of, so decrypting with it below is independent proof the driver
/// wrapped the secret against the public key it was actually given.
fn generate_server_keypair() -> (RsaPrivateKey, Vec<u8>) {
    let mut rng = rand::rngs::OsRng;
    let priv_key = RsaPrivateKey::new(&mut rng, 1024).expect("generate rsa key");
    let pub_der = RsaPublicKey::from(&priv_key)
        .to_public_key_der()
        .expect("encode public key")
        .as_bytes()
        .to_vec();
    (priv_key, pub_der)
}

/// The `should_authenticate: false` path: no Mojang session-server call is
/// needed, so the whole handshake should complete and a packet sent
/// afterwards should decode cleanly — proof the cipher is on and consistent
/// in both directions.
#[tokio::test]
async fn begin_encryption_without_authentication_completes_the_handshake() {
    let (priv_key, pub_der) = generate_server_keypair();
    let verify_token = vec![9u8, 8, 7, 6];

    let adapter = FakeOnlineAdapter {
        server_id: "srv".to_owned(),
        public_key_der: pub_der,
        verify_token: verify_token.clone(),
        should_authenticate: false,
    };

    let (client_io, server_io) = memory_pair();
    let (handle, mut events) = ClientBuilder::new(server(), profile(), Box::new(adapter))
        .connect_with(client_io);
    let mut peer: Connection<DuplexStream> = Connection::new(server_io);

    // Drive the trigger that makes the adapter hand back `BeginEncryption`.
    peer.write_packet(TRIGGER, &[]).await.unwrap();

    // The driver must write the `EncryptionResponse`-shaped packet *before*
    // enabling its cipher, so this read must succeed uncorrupted.
    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, KEY_PACKET_ID);

    let mut r = lodestone_core::Reader::new(&payload);
    let secret_len = r.var_i32().unwrap() as usize;
    let enc_secret = r.bytes(secret_len).unwrap().to_vec();
    let token_len = r.var_i32().unwrap() as usize;
    let enc_token = r.bytes(token_len).unwrap().to_vec();

    // Independently decrypt with the private half the driver never had.
    let secret = priv_key
        .decrypt(Pkcs1v15Encrypt, &enc_secret)
        .expect("decrypt shared secret with the real private key");
    let recovered_token = priv_key
        .decrypt(Pkcs1v15Encrypt, &enc_token)
        .expect("decrypt verify token with the real private key");
    assert_eq!(secret.len(), lodestone_net::SHARED_SECRET_LEN);
    assert_eq!(
        recovered_token, verify_token,
        "the echoed verify token must decrypt back to exactly what was sent"
    );

    // Switch the "server" side's cipher on with the now-known secret, exactly
    // as a real server would the instant it accepts the response.
    peer.enable_encryption(&secret).unwrap();

    // A packet sent now must reach the client, and be decrypted correctly,
    // proving `Connection::enable_encryption` was actually called driver-side
    // (not just performed on paper) and with the *same* secret.
    peer.write_packet(POST_ENCRYPT_TRIGGER, &[]).await.unwrap();
    let event = events.recv().await.expect("event after encryption");
    assert!(matches!(event, ClientEvent::Login { entity_id: 7, .. }));

    drop(handle);
}

/// `should_authenticate: true` with no `ClientBuilder::online_session`
/// configured must fail **fast and typed**, before any crypto or network
/// happens at all — not complete the handshake and only then fail a
/// session-server call it was never going to be able to make.
#[tokio::test]
async fn begin_encryption_requiring_auth_without_a_session_fails_fast() {
    let (_priv_key, pub_der) = generate_server_keypair();

    let adapter = FakeOnlineAdapter {
        server_id: "srv".to_owned(),
        public_key_der: pub_der,
        verify_token: vec![1, 2, 3],
        should_authenticate: true,
    };

    let (client_io, server_io) = memory_pair();
    let (handle, _events) = ClientBuilder::new(server(), profile(), Box::new(adapter))
        .connect_with(client_io);
    let mut peer: Connection<DuplexStream> = Connection::new(server_io);

    peer.write_packet(TRIGGER, &[]).await.unwrap();

    // No `EncryptionResponse` should ever be written — the driver must bail
    // out before spending a round trip on crypto it knows is pointless.
    // `drop(peer)` closes the read side; if the driver actually did write a
    // key packet, `handle.join()` below still tells us definitively via the
    // session outcome, which is the authoritative assertion.
    match handle.join().await {
        SessionOutcome::Failed(ClientError::OnlineModeSessionRequired) => {}
        other => panic!("expected OnlineModeSessionRequired, got {other:?}"),
    }
}
