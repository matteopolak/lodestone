//! Issue #273's encryption half: `V770ServerProtocol::encode_encryption_request`
//! builds vanilla's exact `hello` wire shape, `decode` lifts a client's `key`
//! reply into `ServerBound::EncryptionResponse` with no crypto of its own, and
//! the RSA math on both ends of that wire round-trips through `lodestone-net`'s
//! real crypto types — not a self-authored fixture, but the same
//! `ServerKeyPair`/`rsa_encrypt` pair the actual server and client use. See
//! `docs/server-online-mode.md`.

use lodestone_core::{Ctx, Decode, Encode, Reader, State, Writer};
use lodestone_net::{ServerKeyPair, generate_shared_secret, generate_verify_token, rsa_encrypt};
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::login::{clientbound, serverbound};
use lodestone_v770::packets::login::{EncryptionRequest, EncryptionResponse};

const CTX: Ctx = Ctx { version: 776 };

#[test]
fn encode_encryption_request_matches_vanillas_hello_wire_shape() {
    // Pairwise-distinct: the public key and verify token are two adjacent
    // byte-array fields in the same packet, so they must differ from each
    // other for a transposition to be visible.
    let public_key = vec![1u8, 2, 3, 4, 5, 6, 7];
    let verify_token = vec![9u8, 8, 7, 6];
    assert_ne!(public_key, verify_token, "fixture must be distinguishable");

    let directive = V770ServerProtocol.encode_encryption_request(&public_key, &verify_token);
    let (packet_id, payload) = match directive {
        ServerDirective::Send { packet_id, payload } => (packet_id, payload),
        other => panic!("expected a Send directive, got {other:?}"),
    };
    assert_eq!(packet_id, clientbound::HELLO);

    let mut reader = Reader::new(&payload);
    let request = EncryptionRequest::decode(&mut reader, CTX).expect("hello payload decodes");
    reader
        .ensure_empty()
        .expect("hello payload must decode with zero trailing bytes");

    // Vanilla's own server-id is always the empty string
    // (`ServerLoginPacketListenerImpl.serverId`).
    assert_eq!(request.server_id, "");
    assert_eq!(request.public_key, public_key);
    assert_eq!(request.challenge, verify_token);
    // Vanilla never constructs `ClientboundHelloPacket` with `false` — see
    // `encode_encryption_request`'s own doc comment.
    assert!(request.should_authenticate);
}

#[test]
fn decode_lifts_a_key_packet_into_encryption_response_with_no_crypto() {
    // Pairwise-distinct ciphertext-shaped fixtures (real ciphertext isn't
    // needed here: `decode` performs no crypto, only framing).
    let shared_secret = vec![0xAAu8; 128];
    let verify_token = vec![0xBBu8; 64];
    assert_ne!(shared_secret[..4], verify_token[..4]);

    let mut writer = Writer::default();
    EncryptionResponse {
        shared_secret: shared_secret.clone(),
        verify_token: verify_token.clone(),
    }
    .encode(&mut writer, CTX)
    .expect("encode must not fail on well-formed fixture bytes");

    let decoded = V770ServerProtocol.decode(State::Login, serverbound::KEY, writer.as_slice());
    match decoded {
        ServerBound::EncryptionResponse {
            shared_secret: got_secret,
            verify_token: got_token,
        } => {
            assert_eq!(got_secret, shared_secret);
            assert_eq!(got_token, verify_token);
        }
        other => panic!("expected ServerBound::EncryptionResponse, got {other:?}"),
    }
}

#[test]
fn decode_rejects_a_key_packet_with_trailing_bytes() {
    let mut writer = Writer::default();
    EncryptionResponse {
        shared_secret: vec![1, 2, 3],
        verify_token: vec![4, 5],
    }
    .encode(&mut writer, CTX)
    .unwrap();
    let mut payload = writer.as_slice().to_vec();
    payload.push(0xFF); // trailing junk

    let decoded = V770ServerProtocol.decode(State::Login, serverbound::KEY, &payload);
    assert_eq!(decoded, ServerBound::Ignored);
}

/// The full handshake's crypto, exercised end to end through the real wire
/// types on both sides: `V770ServerProtocol::encode_encryption_request`
/// carries a real `ServerKeyPair`'s public key onto the wire, a simulated
/// client RSA-encrypts a real shared secret and verify token against exactly
/// those bytes with `lodestone_net::rsa_encrypt` (the same function the real
/// client driver calls), the reply is framed as a real `EncryptionResponse`
/// and decoded back through `V770ServerProtocol::decode`, and the server
/// recovers the *exact* original values with `ServerKeyPair::decrypt`. No
/// step reuses another step's output as its own "expected" value — the
/// expected values (`secret`, `verify_token`) are generated before either
/// side of the handshake runs.
#[test]
fn a_simulated_client_and_the_real_server_protocol_agree_on_the_secret_and_token() {
    let server_key = ServerKeyPair::generate().expect("keypair generation");
    let verify_token = generate_verify_token();

    let directive =
        V770ServerProtocol.encode_encryption_request(server_key.public_key_der(), &verify_token);
    let ServerDirective::Send { payload, .. } = directive else {
        panic!("expected a Send directive");
    };
    let mut reader = Reader::new(&payload);
    let request = EncryptionRequest::decode(&mut reader, CTX).unwrap();
    reader.ensure_empty().unwrap();

    // The "client": encrypts a fresh secret and echoes the request's own
    // challenge back, against exactly the public key bytes it read off the
    // wire — not the `ServerKeyPair` directly, to prove the DER round-trips.
    let secret = generate_shared_secret();
    let enc_secret = rsa_encrypt(&request.public_key, &secret).unwrap();
    let enc_token = rsa_encrypt(&request.public_key, &request.challenge).unwrap();

    let mut writer = Writer::default();
    EncryptionResponse {
        shared_secret: enc_secret,
        verify_token: enc_token,
    }
    .encode(&mut writer, CTX)
    .unwrap();

    let decoded = V770ServerProtocol.decode(State::Login, serverbound::KEY, writer.as_slice());
    let ServerBound::EncryptionResponse {
        shared_secret: got_enc_secret,
        verify_token: got_enc_token,
    } = decoded
    else {
        panic!("expected ServerBound::EncryptionResponse");
    };

    // The "server": decrypts with the private half of the exact keypair whose
    // public half was advertised.
    let dec_secret = server_key.decrypt(&got_enc_secret).unwrap();
    let dec_token = server_key.decrypt(&got_enc_token).unwrap();

    assert_eq!(dec_secret, secret, "server must recover the client's exact shared secret");
    assert_eq!(
        dec_token, verify_token,
        "server must recover its own exact verify token, echoed back by the client"
    );
    assert_ne!(dec_secret, dec_token, "the two recovered buffers must not collide");
}
