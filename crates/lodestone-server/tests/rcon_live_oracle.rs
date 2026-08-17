//! `rcon.rs`'s framing checked against a **real vanilla RCON
//! server**, not just our own client/server pair talking to each other.
//!
//! `tests/rcon.rs` proves our listener and our own `AsyncRconClient` agree —
//! which is exactly the closed loop CLAUDE.md's evidence standard warns
//! about: `decode(encode(x)) == x` is satisfied by two symmetric
//! misunderstandings. This file drives the same raw frame bytes at the
//! flat-creative oracle's real vanilla RCON endpoint (`127.0.0.1:25571`,
//! `scripts/live-oracles/creative.sh`) and at our own listener, and asserts
//! both answer with the same wire *shape* for the same request — an
//! expectation that originates outside our own encoder.
//!
//! Run with:
//!
//! ```text
//! ./scripts/live-oracles/creative.sh
//! cargo test -p lodestone-server --test rcon_live_oracle -- --ignored --nocapture
//! ```

use std::io::ErrorKind;

use lodestone_server::{
    ChunkColumn, ChunkSource, CommandDispatch, IntegratedServer, RconConfig, ServerBound,
    ServerDirective, ServerProtocol,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;

/// The oracle's RCON endpoint and password — `scripts/live-oracles/creative.sh`'s
/// own documented values, not chosen by this test.
const ORACLE_ADDR: &str = "127.0.0.1:25571";
const ORACLE_PASSWORD: &str = "lodestone";

const REPAIR: &str = "recreate the flat creative oracle with: \
    ./scripts/live-oracles/creative.sh (expected a vanilla 26.2 server's RCON \
    on 127.0.0.1:25571, password \"lodestone\")";

/// One decoded RCON frame, for comparing shape rather than content.
struct DecodedFrame {
    body_len: i32,
    id: i32,
    packet_type: i32,
    payload_len: usize,
    terminator: [u8; 2],
    total_wire_len: usize,
}

async fn write_frame(stream: &mut TcpStream, id: i32, packet_type: i32, payload: &str) {
    let body_len = 4 + 4 + payload.len() + 2;
    let mut frame = Vec::with_capacity(4 + body_len);
    frame.extend_from_slice(&(body_len as i32).to_le_bytes());
    frame.extend_from_slice(&id.to_le_bytes());
    frame.extend_from_slice(&packet_type.to_le_bytes());
    frame.extend_from_slice(payload.as_bytes());
    frame.extend_from_slice(&[0, 0]);
    stream.write_all(&frame).await.expect("write frame");
}

/// Reads one frame with `read_exact` (robust on the *reading* side is fine —
/// the one-write rule this issue cares about is asserted separately, over our
/// own listener, in `tests/rcon.rs`'s single-`read()` control).
async fn read_frame(stream: &mut TcpStream) -> DecodedFrame {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.expect("read length");
    let body_len = i32::from_le_bytes(len_buf);
    let mut body = vec![0u8; body_len as usize];
    stream.read_exact(&mut body).await.expect("read body");
    let id = i32::from_le_bytes(body[0..4].try_into().expect("4 bytes"));
    let packet_type = i32::from_le_bytes(body[4..8].try_into().expect("4 bytes"));
    let terminator = [body[body.len() - 2], body[body.len() - 1]];
    DecodedFrame {
        body_len,
        id,
        packet_type,
        payload_len: body.len() - 10,
        terminator,
        total_wire_len: 4 + body.len(),
    }
}

/// Connects and returns the AUTH_RESPONSE frame for a **correct** password.
async fn auth_success_frame(addr: &str, password: &str, req_id: i32) -> (TcpStream, DecodedFrame) {
    let mut stream = TcpStream::connect(addr)
        .await
        .unwrap_or_else(|e| panic!("connect to {addr}: {e}. {REPAIR}"));
    write_frame(&mut stream, req_id, 3, password).await;
    let frame = read_frame(&mut stream).await;
    (stream, frame)
}

/// A protocol/world pair for spinning up our own listener; the RCON tests
/// never drive the game connection, so nothing here is ever exercised beyond
/// construction.
struct SilentProtocol;

impl ServerProtocol for SilentProtocol {
    fn decode(&self, _state: lodestone_core::State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
        ServerBound::Ignored
    }
    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }
    fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
        ServerDirective::None
    }
    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
}

struct EmptyWorld;

impl ChunkSource for EmptyWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 1)
    }
    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:air".to_string()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// AUTH_RESPONSE for a correct password: real vanilla and our own listener
/// must produce the **identical shape** — length 10, the request id echoed,
/// type 2, empty payload, two null bytes. Nothing about this frame is
/// version- or implementation-specific, so an exact match is the right bar
/// (not merely "close"), and using a value distinct from every other field in
/// this test (`req_id = 4242`) means a transposition of id/type could not
/// hide behind a coincidental equal value.
#[tokio::test]
#[ignore = "needs the flat creative oracle: ./scripts/live-oracles/creative.sh"]
async fn auth_success_framing_matches_real_vanilla_rcon() {
    let (_vanilla_stream, vanilla) = auth_success_frame(ORACLE_ADDR, ORACLE_PASSWORD, 4242).await;

    let (mut server, _client_io) = IntegratedServer::open_in_memory(SilentProtocol, EmptyWorld, 0);
    let addr = server
        .start_rcon(RconConfig::new(
            ([127, 0, 0, 1], 0).into(),
            "hunter2",
            CommandDispatch::none(),
        ))
        .expect("bind our own listener");
    let (_ours_stream, ours) = auth_success_frame(&addr.to_string(), "hunter2", 4242).await;

    assert_eq!(vanilla.body_len, 10, "vanilla's own AUTH_RESPONSE body length");
    assert_eq!(ours.body_len, vanilla.body_len, "body length must match vanilla's");
    assert_eq!(ours.id, vanilla.id, "both echo the same request id");
    assert_eq!(ours.id, 4242);
    assert_eq!(ours.packet_type, vanilla.packet_type, "both answer TYPE_AUTH_RESPONSE (2)");
    assert_eq!(ours.packet_type, 2);
    assert_eq!(ours.payload_len, vanilla.payload_len, "both carry an empty payload");
    assert_eq!(ours.payload_len, 0);
    assert_eq!(ours.terminator, vanilla.terminator, "both null-terminate identically");
    assert_eq!(ours.terminator, [0, 0]);
    assert_eq!(
        ours.total_wire_len, vanilla.total_wire_len,
        "identical frame shape means identical total byte count"
    );

    server.shutdown().await;
}

/// The negative case, so "auth succeeded" above is not the only behaviour
/// checked: a wrong password against the real oracle closes the connection
/// (vanilla's own client-refusal behaviour, which is what
/// `AsyncRconClient::connect` — and this crate's own auth arm — reproduce).
/// This is the control that proves the success case above is actually
/// discriminating: if a wrong password produced the *same* framing as a
/// right one, the success assertions would not be evidence of anything.
#[tokio::test]
#[ignore = "needs the flat creative oracle: ./scripts/live-oracles/creative.sh"]
async fn a_wrong_password_is_refused_by_real_vanilla_rcon_too() {
    let mut stream = TcpStream::connect(ORACLE_ADDR)
        .await
        .unwrap_or_else(|e| panic!("connect to {ORACLE_ADDR}: {e}. {REPAIR}"));
    write_frame(&mut stream, 99, 3, "definitely-not-the-password").await;
    let frame = read_frame(&mut stream).await;

    assert_eq!(frame.id, -1, "vanilla answers a failed auth with request id -1");
    assert_eq!(frame.packet_type, 2, "still TYPE_AUTH_RESPONSE");

    // And the connection is now unusable for a command — vanilla's
    // `RconClient` requires re-authentication, exactly like ours.
    write_frame(&mut stream, 100, 2, "list").await;
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {
            let body_len = i32::from_le_bytes(len_buf);
            let mut body = vec![0u8; body_len as usize];
            stream.read_exact(&mut body).await.expect("read body");
            let id = i32::from_le_bytes(body[0..4].try_into().expect("4 bytes"));
            assert_eq!(id, -1, "a command on an unauthenticated connection is still refused");
        }
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
            // Also acceptable: vanilla closed the socket outright.
        }
        Err(e) => panic!("unexpected error reading post-auth-failure response: {e}"),
    }
}
