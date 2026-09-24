//! Non-hermetic online-mode encryption smoke test.
//!
//! This test is `#[ignore]`d because it requires a live `online-mode=true`
//! vanilla server on `127.0.0.1:25572` (stand one up with, e.g.
//! `docker run --rm -d --name lodestone-mc-online -p 25572:25565 ...`). Run it
//! with `cargo test -p lodestone-net --test online_handshake -- --ignored`.
//!
//! We cannot complete an authenticated join without Microsoft credentials, so
//! the *measurement here is the failure mode*. We drive a real handshake, send a
//! real RSA-encrypted shared secret + verify token, switch on AES-128-CFB8 in
//! both directions, and then read the next packet. Reaching a cleanly-decrypted
//! login-disconnect whose reason is "verify username" proves the whole crypto
//! path worked: the server accepted our RSA-wrapped secret, matched the verify
//! token, and enciphered its reply with the same stream cipher we set up — only
//! the session-server ownership lookup (which we never performed) failed.
//!
//! A framing or decrypt error here instead would mean the cipher is wrong, and
//! would tell us which half (encrypt vs decrypt) broke.

use lodestone_core::{Reader, Writer};
use lodestone_net::{Connection, generate_shared_secret, rsa_encrypt};
use tokio::net::TcpStream;
use uuid::Uuid;

const PROTOCOL_776: i32 = 776;
const ADDR: &str = "127.0.0.1";
const PORT: u16 = 25572;

// Serverbound handshake `intention` id, and login `hello` / `key` ids.
const ID_INTENTION: i32 = 0;
const ID_LOGIN_HELLO: i32 = 0;
const ID_LOGIN_KEY: i32 = 1;
// Clientbound login ids.
const ID_LOGIN_DISCONNECT: i32 = 0;
const ID_ENCRYPTION_REQUEST: i32 = 1;
const ID_LOGIN_COMPRESSION: i32 = 3;

fn write_prefixed_bytes(w: &mut Writer, data: &[u8]) {
    w.var_i32(i32::try_from(data.len()).expect("byte array length fits in i32"));
    w.bytes(data);
}

#[tokio::test]
#[ignore = "requires a live online-mode server on 127.0.0.1:25572"]
async fn online_mode_handshake_reaches_username_verification() {
    let stream = TcpStream::connect((ADDR, PORT))
        .await
        .expect("connect to online-mode server on :25572");
    let mut conn = Connection::new(stream);

    // 1. Handshake: intention -> login.
    let mut hs = Writer::default();
    hs.var_i32(PROTOCOL_776);
    hs.string(ADDR);
    hs.u16(PORT);
    hs.var_i32(2); // next state = login
    conn.write_packet(ID_INTENTION, &hs.into_vec())
        .await
        .expect("send handshake");

    // 2. Login start (hello): username + offline-style profile uuid.
    let mut hello = Writer::default();
    hello.string("Lodestone");
    hello.uuid(Uuid::new_v4());
    conn.write_packet(ID_LOGIN_HELLO, &hello.into_vec())
        .await
        .expect("send login hello");

    // 3. Read packets until the encryption request arrives (a server may send a
    //    compression packet first).
    let (server_id, public_key, challenge) = loop {
        let (id, body) = conn
            .read_packet()
            .await
            .expect("read login packet")
            .expect("connection closed before encryption request");
        if id == ID_LOGIN_COMPRESSION {
            let mut r = Reader::new(&body);
            let threshold = r.var_i32().expect("compression threshold");
            conn.set_compression(threshold);
            continue;
        }
        if id == ID_LOGIN_DISCONNECT {
            let mut r = Reader::new(&body);
            let reason = r.string(262_144).expect("disconnect reason");
            panic!("server disconnected before encryption request: {reason}");
        }
        assert_eq!(
            id, ID_ENCRYPTION_REQUEST,
            "expected encryption request, got id {id}"
        );
        let mut r = Reader::new(&body);
        let server_id = r.string(20).expect("server id");
        let pk_len = r.var_i32().expect("public key length") as usize;
        let public_key = r.bytes(pk_len).expect("public key bytes").to_vec();
        let ch_len = r.var_i32().expect("challenge length") as usize;
        let challenge = r.bytes(ch_len).expect("challenge bytes").to_vec();
        break (server_id, public_key, challenge);
    };
    assert!(!public_key.is_empty(), "server sent an empty public key");

    // 4. Generate the shared secret and RSA-wrap the secret + verify token.
    let secret = generate_shared_secret();
    let enc_secret = rsa_encrypt(&public_key, &secret).expect("rsa-encrypt shared secret");
    let enc_token = rsa_encrypt(&public_key, &challenge).expect("rsa-encrypt verify token");

    // 5. Send the encryption response (`key`) in cleartext...
    let mut key = Writer::default();
    write_prefixed_bytes(&mut key, &enc_secret);
    write_prefixed_bytes(&mut key, &enc_token);
    conn.write_packet(ID_LOGIN_KEY, &key.into_vec())
        .await
        .expect("send encryption response");

    // 6. ...then flip the cipher on in both directions. The server switches its
    //    cipher the instant it accepts the response, so everything from here is
    //    enciphered.
    conn.enable_encryption(&secret)
        .expect("enable connection encryption");
    assert!(conn.is_encrypted());

    // 7. Read the next packet. If our crypto is correct this decrypts cleanly to
    //    a login-disconnect explaining the (expected) auth failure. If the
    //    crypto were wrong we would get a framing/EOF/decode error instead.
    let (id, body) = conn
        .read_packet()
        .await
        .expect("read post-encryption packet (a decrypt error here means the cipher is wrong)")
        .expect("connection closed with no post-encryption packet");

    assert_eq!(
        id, ID_LOGIN_DISCONNECT,
        "expected an (encrypted) login disconnect, got id {id}"
    );
    let mut r = Reader::new(&body);
    let reason = r.string(262_144).expect("decode disconnect reason");
    eprintln!("server_id={server_id:?}");
    eprintln!("post-encryption disconnect reason: {reason}");

    let lowered = reason.to_lowercase();
    assert!(
        lowered.contains("verify") || lowered.contains("unverified") || lowered.contains("auth"),
        "expected a username-verification failure proving the crypto worked, got: {reason}"
    );
}
