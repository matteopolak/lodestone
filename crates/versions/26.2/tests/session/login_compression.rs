//! The compression half: `V770ServerProtocol::login_success` must
//! enable compression in the same order vanilla does — the
//! `login_compression` packet **uncompressed**, then the switch, then
//! `login_finished` **compressed** — or the two sides disagree about which
//! layer came first. See `docs/server-login-compression.md`.

use lodestone_core::Reader;
use lodestone_server::{ServerDirective, ServerProtocol};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::login::clientbound::{LOGIN_COMPRESSION, LOGIN_FINISHED};
use uuid::Uuid;

#[test]
fn login_success_sends_compression_before_login_finished_and_activates_the_same_threshold() {
    let directives = V770ServerProtocol.login_success("Steve", Uuid::from_u128(11));

    assert_eq!(
        directives.len(),
        3,
        "login_compression send, the codec switch, then login_finished send"
    );

    let (compression_packet_id, compression_payload) = match &directives[0] {
        ServerDirective::Send { packet_id, payload } => (*packet_id, payload.clone()),
        other => panic!("expected directives[0] to be a Send, got {other:?}"),
    };
    assert_eq!(compression_packet_id, LOGIN_COMPRESSION);

    // Wire layout is a single VarInt threshold (`packets::login::LoginCompression`).
    let mut reader = Reader::new(&compression_payload);
    let threshold = reader.var_i32().expect("threshold varint");
    reader
        .ensure_empty()
        .expect("login_compression payload must decode with zero trailing bytes");

    match &directives[1] {
        ServerDirective::SetCompression(activated) => assert_eq!(
            *activated, threshold,
            "the codec must switch to the exact threshold just announced on the wire, \
             not a second hardcoded value that could drift from it"
        ),
        other => panic!("expected directives[1] to be SetCompression, got {other:?}"),
    }

    match &directives[2] {
        ServerDirective::Send { packet_id, .. } => {
            assert_eq!(*packet_id, LOGIN_FINISHED);
        }
        other => panic!("expected directives[2] to be a Send, got {other:?}"),
    }

    // Matches vanilla's own default (`network-compression-threshold=256`,
    // measured identical across every `server.properties` under `.cache/mc/`).
    assert_eq!(threshold, 256);
}
