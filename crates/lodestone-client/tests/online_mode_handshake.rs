//! Hermetic proof that the driver actually drives the online-mode encryption
//! handshake a `Directive::BeginEncryption` describes — before
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
//! (c) actually flips its connection's cipher on, and (d) correctly treats an
//! explicit offline identity differently from a failed online account: offline
//! skips Mojang for either authentication flag, while the latter refuses a
//! requested proof before sending a response.
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

/// An explicit offline intent never calls Mojang. It still completes the
/// encryption exchange for either value a server gives `should_authenticate`,
/// which is necessary for hybrid/cracked servers that encrypt their transport
/// without requiring a Mojang proof.
async fn assert_offline_encryption_completes(should_authenticate: bool) {
    let (priv_key, pub_der) = generate_server_keypair();
    let verify_token = vec![9u8, 8, 7, 6];

    let adapter = FakeOnlineAdapter {
        server_id: "srv".to_owned(),
        public_key_der: pub_der,
        verify_token: verify_token.clone(),
        should_authenticate,
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

#[tokio::test]
async fn offline_encryption_without_authentication_completes_the_handshake() {
    assert_offline_encryption_completes(false).await;
}

#[tokio::test]
async fn offline_encryption_with_authentication_flag_completes_without_mojang() {
    assert_offline_encryption_completes(true).await;
}

/// A selected online account that could not refresh is retained rather than
/// retried as offline. If the server does not request a Mojang proof, its
/// encryption handshake still completes under the caller-provided profile.
#[tokio::test]
async fn unavailable_online_account_encrypts_when_authentication_is_not_requested() {
    let (_priv_key, pub_der) = generate_server_keypair();
    let adapter = FakeOnlineAdapter {
        server_id: "srv".to_owned(),
        public_key_der: pub_der,
        verify_token: vec![1, 2, 3],
        should_authenticate: false,
    };

    let (client_io, server_io) = memory_pair();
    let (handle, _events) = ClientBuilder::new(server(), profile(), Box::new(adapter))
        .online_session_unavailable(
            "Steve".to_owned(),
            "the saved Microsoft session has expired".to_owned(),
        )
        .connect_with(client_io);
    let mut peer: Connection<DuplexStream> = Connection::new(server_io);

    peer.write_packet(TRIGGER, &[]).await.unwrap();
    let (id, _) = peer
        .read_packet()
        .await
        .unwrap()
        .expect("encryption response for a server that did not request Mojang proof");
    assert_eq!(id, KEY_PACKET_ID);
    drop(handle);
}

/// The same fail-fast path, but for a caller that *had* an account and could not
/// resolve a session for it — `ClientBuilder::online_session_unavailable`.
///
/// # Why this is a separate variant and not a better sentence
///
/// The two situations have different remedies. "Nobody is signed in" means add
/// an account; "your saved session expired" means re-authorise a specific,
/// nameable account. Collapsing them is what let a player with a working premium
/// account — visible and correct in the account switcher — read *"no Microsoft
/// session was configured for this connection"*, which sounds like a broken
/// build and points at nothing they can act on.
///
/// So this asserts the discriminant **and** that the payload carries the account
/// name through to the message: a variant that dropped `account` would be the old
/// vague text wearing a new type.
#[tokio::test]
async fn begin_encryption_with_an_unusable_account_names_it_instead_of_blaming_configuration() {
    let (_priv_key, pub_der) = generate_server_keypair();

    let adapter = FakeOnlineAdapter {
        server_id: "srv".to_owned(),
        public_key_der: pub_der,
        verify_token: vec![1, 2, 3],
        should_authenticate: true,
    };

    let (client_io, server_io) = memory_pair();
    let (handle, _events) = ClientBuilder::new(server(), profile(), Box::new(adapter))
        .online_session_unavailable(
            "Steve".to_owned(),
            "the saved Microsoft session has expired".to_owned(),
        )
        .connect_with(client_io);
    let mut peer: Connection<DuplexStream> = Connection::new(server_io);

    peer.write_packet(TRIGGER, &[]).await.unwrap();

    let outcome = handle.join().await;
    let SessionOutcome::Failed(error) = outcome else {
        panic!("expected a failed session, got {outcome:?}");
    };
    let ClientError::OnlineModeSessionUnavailable { account, detail } = &error else {
        panic!("expected OnlineModeSessionUnavailable, got {error:?}");
    };
    assert_eq!(account, "Steve");
    assert_eq!(detail, "the saved Microsoft session has expired");

    // The text a disconnect screen shows. Both halves must survive into it —
    // the account (so the player knows *which* sign-in to repeat) and the
    // reason. Asserted on the rendered string, not just the fields, because the
    // `#[error(...)]` format is what the screen actually reads and a variant can
    // carry a field it never prints.
    let shown = error.cause_chain();
    assert!(
        shown.contains("Steve") && shown.contains("expired"),
        "the message must name the account and the reason, got {shown:?}"
    );
    // And it must NOT be the sentence for "nobody is signed in", which is the
    // whole distinction. This is the arm that fails if someone later merges the
    // two variants back together for tidiness.
    assert!(
        !shown.contains("no Microsoft account is signed in"),
        "an expired session must not be reported as nobody being signed in, got {shown:?}"
    );
}

/// The three online-mode failure kinds are three distinct values, checked
/// against each other in one place so a future merge of any two is a red test.
///
/// A server *kicking* us for an unauthenticated join is deliberately not in this
/// list: that arrives as `ClientEvent::Disconnect` carrying the server's own
/// `Text`, travels a different path entirely (`NetUpdate::Disconnected`, titled
/// `disconnect.lost`), and must keep the server's wording rather than any of
/// ours. The owner's report of a *"failed premium challenge"* kick was exactly
/// that — a server's message, not this crate's; the string "premium" appears
/// nowhere in this repo.
#[test]
fn the_online_mode_failures_are_three_distinguishable_messages() {
    let required = ClientError::OnlineModeSessionRequired.cause_chain();
    let unavailable = ClientError::OnlineModeSessionUnavailable {
        account: "Steve".to_owned(),
        detail: "the saved Microsoft session has expired".to_owned(),
    }
    .cause_chain();
    // The Mojang-side failure. `NoMinecraftProfile` is the cheapest real
    // `AuthError` to build (no reqwest error needed) and is itself a genuine
    // outcome of this path: an account that authenticated but owns no copy of
    // the game.
    let mojang =
        ClientError::Auth(lodestone_auth::AuthError::NoMinecraftProfile).cause_chain();

    let all = [&required, &unavailable, &mojang];
    for (i, a) in all.iter().enumerate() {
        assert!(!a.is_empty(), "message {i} is empty");
        for (j, b) in all.iter().enumerate() {
            if i != j {
                assert_ne!(a, b, "messages {i} and {j} are the same sentence");
            }
        }
    }

    // Distinct strings is the floor, not the bar: each must also be *about* its
    // own cause, or three different ways of saying "authentication failed" would
    // pass the loop above.
    assert!(
        required.contains("no Microsoft account is signed in"),
        "got {required:?}"
    );
    assert!(required.contains("Accounts"), "must say where to fix it: {required:?}");
    assert!(unavailable.contains("Steve"), "got {unavailable:?}");
    assert!(
        mojang.contains("does not own a minecraft profile"),
        "the Mojang-side failure must surface Mojang's own reason: {mojang:?}"
    );
}
