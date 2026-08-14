//! Issue #331: the RCON listener, driven through its production entry point
//! (`IntegratedServer::start_rcon`) with the same `AsyncRconClient` the live
//! oracle tests use against vanilla — the verification the issue body asks
//! for. The listener, the auth state machine and the command execution are
//! exercised over a real loopback socket; nothing here reaches a live server.

use std::sync::{Arc, Mutex};

use lodestone_server::{
    ChunkColumn, ChunkSource, CommandCaller, CommandDispatch, CommandResponse, CommandSink,
    IntegratedServer, RconConfig, ServerBound, ServerDirective, ServerProtocol, UNKNOWN_COMMAND,
};
use lodestone_testsupport::{AsyncRconClient, rcon_frame};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

/// Records every command, and the identity it arrived as, so the tests can
/// assert both the round-trip and that the console identity reached the sink.
#[derive(Default)]
struct ConsoleSink(Mutex<Vec<(String, String)>>);

impl CommandSink for ConsoleSink {
    fn run(&self, caller: &CommandCaller, command: &str) -> CommandResponse {
        self.0
            .lock()
            .unwrap()
            .push((caller.username.clone(), command.to_owned()));
        CommandResponse::Ran {
            feedback: vec![format!("ran `{command}`")],
        }
    }
}

/// A protocol that never decodes anything — the RCON tests never drive the
/// game connection, so no protocol method is ever called.
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

/// An all-air world; nothing is ever read from it by these tests.
struct EmptyWorld;

impl ChunkSource for EmptyWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 1)
    }
    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

/// A server with an RCON listener bound to an OS-assigned loopback port.
///
/// Port `0` is what makes the address usable: `start_rcon` binds synchronously
/// and returns the real bound address, so connecting to it cannot race the
/// accept loop.
fn rcon_server(
    commands: CommandDispatch,
) -> (IntegratedServer, std::net::SocketAddr) {
    let (mut server, _client_io) = IntegratedServer::open_in_memory(SilentProtocol, EmptyWorld, 0);
    let addr = server
        .start_rcon(RconConfig::new(([127, 0, 0, 1], 0).into(), "hunter2", commands))
        .expect("bind the RCON listener");
    (server, addr)
}

/// The round-trip, driven with the exact client the live oracle tests use
/// against vanilla — pointed at our own listener.
#[tokio::test]
async fn rcon_round_trips_a_command_as_the_console() {
    let sink = Arc::new(ConsoleSink::default());
    let (server, addr) = rcon_server(CommandDispatch::installed(sink.clone()));

    let mut client = AsyncRconClient::connect(addr, "hunter2")
        .await
        .expect("connect and authenticate");

    let response = client
        .command("say hello")
        .await
        .expect("command round-trips");
    assert_eq!(response, "ran `say hello`");

    // Vanilla's `trimOptionalPrefix`: a leading slash is the same command.
    let response = client
        .command("/say hello")
        .await
        .expect("slash-prefixed command round-trips");
    assert_eq!(response, "ran `say hello`");

    // The command reached the host sink as the console ("Rcon"), not as a
    // player — the identity seam the whole command path depends on.
    let seen = sink.0.lock().unwrap();
    assert_eq!(seen.len(), 2, "two commands must have been dispatched");
    assert!(
        seen.iter().all(|(name, _)| name == "Rcon"),
        "every command must arrive as the console, got {seen:?}"
    );
    assert_eq!(seen[0].1, "say hello");
    assert_eq!(seen[1].1, "say hello");

    server.shutdown().await;
}

/// A wrong password fails authentication with the error a client
/// (`AsyncRconClient::connect`) reports as `PermissionDenied` — the same
/// refusal shape pointing at vanilla produces.
#[tokio::test]
async fn a_wrong_password_is_refused() {
    let (server, addr) = rcon_server(CommandDispatch::none());

    let err = AsyncRconClient::connect(addr, "wrong-password")
        .await
        .expect_err("a wrong password must fail authentication");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);

    server.shutdown().await;
}

/// A command sent before any authentication is refused with request id `-1`
/// and packet type 2, exactly vanilla's `sendAuthFailure`
/// (`RconClient.java:121-123`). Sent as a raw frame because every
/// `AsyncRconClient` authenticates on connect.
#[tokio::test]
async fn a_command_before_authentication_is_refused() {
    let (server, addr) = rcon_server(CommandDispatch::none());

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect");
    stream
        .write_all(&rcon_frame(7, 2, "say hi"))
        .await
        .expect("send command frame");

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.expect("read length");
    let len = i32::from_le_bytes(len_buf);
    assert!(len >= 10, "response frame must carry a body");
    let mut body = vec![0u8; len as usize];
    stream.read_exact(&mut body).await.expect("read body");
    let id = i32::from_le_bytes(body[0..4].try_into().expect("length checked"));
    let packet_type = i32::from_le_bytes(body[4..8].try_into().expect("length checked"));
    assert_eq!(id, -1, "pre-auth commands answer with the auth-failure id");
    assert_eq!(packet_type, 2, "pre-auth commands answer with type AUTH_RESPONSE");

    server.shutdown().await;
}

/// The fail-closed property of the command seam, observable over the wire:
/// with no sink installed, an *authenticated* command is refused with
/// [`UNKNOWN_COMMAND`] rather than echoing back what it would have run.
#[tokio::test]
async fn an_authed_command_with_no_sink_installed_is_refused() {
    let (server, addr) = rcon_server(CommandDispatch::none());

    let mut client = AsyncRconClient::connect(addr, "hunter2")
        .await
        .expect("connect and authenticate");
    let response = client
        .command("warp spawn")
        .await
        .expect("command round-trips");
    assert_eq!(response, UNKNOWN_COMMAND);

    server.shutdown().await;
}

/// The module's one-write rule, driven exactly as vanilla's `RconClient`
/// drives it: **one `read()` call**, not the `read_exact`-in-two-steps every
/// other test here uses through `AsyncRconClient`/`decode_rcon_response`
/// (both of which would tolerate a length prefix and a body arriving as two
/// separate reads, so neither can see this bug class).
///
/// A single `TcpStream::read` on a fresh connection returns whatever is
/// available when the kernel wakes it, so this is only a faithful control
/// when the whole frame really did arrive as one contiguous write: a
/// response split across two `write_all` calls on loopback reliably shows up
/// as two deliveries once there is any scheduling gap between them (proven
/// by hand against a deliberately split `write_frame` while writing this
/// test — the single `read()` below returned only the first partial frame
/// and every subsequent assertion failed). That is the failure this test
/// exists to catch: a response built and sent as more than one `write_all`
/// looks identical to a correct implementation under `read_exact`, and only
/// a single-syscall read can tell the two apart.
#[tokio::test]
async fn one_write_delivers_the_whole_auth_response_to_a_single_read() {
    let (server, addr) = rcon_server(CommandDispatch::none());

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect");
    stream
        .write_all(&rcon_frame(11, 3, "hunter2"))
        .await
        .expect("send auth frame");

    // Vanilla's AUTH_RESPONSE for a correct password: length 10, this id,
    // type 2 (TYPE_AUTH_RESPONSE), empty payload, two null bytes — 14 bytes
    // on the wire in total. No delay before the read: waiting first would
    // give a genuinely split write time to finish arriving and defeat the
    // control (checked by hand — a delayed read passes even against a
    // deliberately split `write_frame`, because both writes are long since
    // sitting in the receive buffer by the time it runs). Racing the read
    // against the write is what makes a split show up as a short read.
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).await.expect("one read");

    assert_eq!(n, 14, "the whole AUTH_RESPONSE frame must land in one read");
    assert_eq!(&buf[0..4], &10i32.to_le_bytes(), "body length");
    assert_eq!(&buf[4..8], &11i32.to_le_bytes(), "echoed request id");
    assert_eq!(&buf[8..12], &2i32.to_le_bytes(), "TYPE_AUTH_RESPONSE");
    assert_eq!(&buf[12..14], &[0, 0], "null-terminated empty payload");

    server.shutdown().await;
}
