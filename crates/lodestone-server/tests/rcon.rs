//! Issue #331: the RCON listener, driven through its production entry point
//! (`IntegratedServer::start_rcon`) with the same `AsyncRconClient` the live
//! oracle tests use against vanilla — the verification the issue body asks
//! for. The listener, the auth state machine and the command execution are
//! exercised over a real loopback socket; nothing here reaches a live server.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_server::{
    ChunkColumn, ChunkSource, CommandCaller, CommandDispatch, CommandResponse, CommandSink,
    IntegratedServer, LanConfig, RconConfig, ServerBound, ServerDirective, ServerProtocol,
    UNKNOWN_COMMAND,
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

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
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

/// An all-air world that *records* every write, keyed by position — the
/// production `set_block` forwarding `ChunkStore::set_block` does
/// (`self.source.set_block(...)`, before it ever touches its own cache), so
/// this is what a real `IntegratedServer::open_to_lan`-hosted `/setblock`/
/// `/fill` over RCON actually reaches.
#[derive(Default)]
struct RecordingWorld(Arc<Mutex<HashMap<(i32, i32, i32), String>>>);

impl ChunkSource for RecordingWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }
    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.0
            .lock()
            .expect("recording world lock poisoned")
            .get(&(x, y, z))
            .cloned()
            .unwrap_or_else(|| "minecraft:air".to_string())
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }
    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.0
            .lock()
            .expect("recording world lock poisoned")
            .insert((x, y, z), name.to_string());
    }
}

/// A LAN-hosted server with an RCON listener — the *realistic* pairing
/// (`LanConfig::rcon`), unlike [`rcon_server`] above, which uses the simpler
/// [`IntegratedServer::open_in_memory`] that builds no live `MobHandle`/
/// `ChunkSource`/`BorderFeed` for RCON to reach at all. This is the
/// constructor `/setblock`/`/fill`/`/summon`/`/worldborder` over RCON need to
/// have anything real behind them.
///
/// `LanConfig::rcon` is left `None` and [`IntegratedServer::start_rcon`] is
/// called separately, exactly like [`rcon_server`] above, rather than through
/// `open_to_lan`'s own internal call: that call's return value (the real
/// bound address, needed because port `0` lets the OS choose) is discarded
/// inside `open_to_lan` itself, so there is no way to learn it from the
/// outside otherwise.
async fn lan_rcon_server(
    edits: Arc<Mutex<HashMap<(i32, i32, i32), String>>>,
) -> (IntegratedServer, std::net::SocketAddr) {
    let mut server = IntegratedServer::open_to_lan(
        "127.0.0.1:0",
        SilentProtocol,
        RecordingWorld(edits),
        LanConfig {
            view_radius: 0,
            // Off, so this measures RCON's own wiring and binds no UDP port a
            // parallel test could contend for.
            query: false,
            ..LanConfig::default()
        },
    )
    .await
    .expect("open_to_lan must bind");
    let rcon_addr = server
        .start_rcon(RconConfig::new(([127, 0, 0, 1], 0).into(), "hunter2", CommandDispatch::none()))
        .expect("bind the RCON listener");
    (server, rcon_addr)
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

    // `warp` names no built-in root (`crate::commands::ServerCommands` has
    // none by that name — see `tests/builtin_commands.rs`'s own use of
    // `"warp spawn"` as the same placeholder), so it is guaranteed to fall
    // through to the host sink under test here rather than being answered by
    // a real built-in. `say` served this purpose until the built-in command
    // set grew to include it.
    let response = client
        .command("warp hello")
        .await
        .expect("command round-trips");
    assert_eq!(response, "ran `warp hello`");

    // Vanilla's `trimOptionalPrefix`: a leading slash is the same command.
    let response = client
        .command("/warp hello")
        .await
        .expect("slash-prefixed command round-trips");
    assert_eq!(response, "ran `warp hello`");

    // The command reached the host sink as the console ("Rcon"), not as a
    // player — the identity seam the whole command path depends on.
    let seen = sink.0.lock().unwrap();
    assert_eq!(seen.len(), 2, "two commands must have been dispatched");
    assert!(
        seen.iter().all(|(name, _)| name == "Rcon"),
        "every command must arrive as the console, got {seen:?}"
    );
    assert_eq!(seen[0].1, "warp hello");
    assert_eq!(seen[1].1, "warp hello");

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

/// `/setblock` over RCON now actually writes — driven through the real
/// `open_to_lan` entry point so `RconConfig::world_source`/`block_ticks` are
/// the same handles the tick loop and any real connection would share, not a
/// test double standing in for them.
#[tokio::test]
async fn rcon_setblock_writes_through_the_real_world_source() {
    let edits: Arc<Mutex<HashMap<(i32, i32, i32), String>>> = Arc::default();
    let (server, addr) = lan_rcon_server(edits.clone()).await;

    let mut client = AsyncRconClient::connect(addr, "hunter2")
        .await
        .expect("connect and authenticate");
    let response = client
        .command("setblock 5 64 -3 minecraft:stone")
        .await
        .expect("command round-trips");
    assert_eq!(response, "Changed the block at 5, 64, -3 to minecraft:stone");

    assert_eq!(
        edits.lock().unwrap().get(&(5, 64, -3)),
        Some(&"minecraft:stone".to_string()),
        "the write must reach the world source `set_block` a real connection's own \
         ChatCommand arm would call, not be dropped as it was before RconConfig \
         carried one"
    );

    server.shutdown().await;
}

/// `/fill` over RCON, the same wiring as `/setblock` but exercising the
/// publish-per-block loop and a volume greater than one.
#[tokio::test]
async fn rcon_fill_writes_every_position_and_reports_the_real_count() {
    let edits: Arc<Mutex<HashMap<(i32, i32, i32), String>>> = Arc::default();
    let (server, addr) = lan_rcon_server(edits.clone()).await;

    let mut client = AsyncRconClient::connect(addr, "hunter2")
        .await
        .expect("connect and authenticate");
    let response = client
        .command("fill 0 64 0 1 64 1 minecraft:glass")
        .await
        .expect("command round-trips");
    assert_eq!(response, "Successfully filled 4 block(s) with minecraft:glass");

    let recorded = edits.lock().unwrap();
    for x in 0..=1 {
        for z in 0..=1 {
            assert_eq!(recorded.get(&(x, 64, z)), Some(&"minecraft:glass".to_string()), "({x}, 64, {z})");
        }
    }
    assert_eq!(recorded.len(), 4, "exactly the four cells in the box, no more");
    drop(recorded);

    server.shutdown().await;
}

/// `/summon` over RCON reaches the same live `MobHandle` the tick loop
/// advances — before `RconConfig::mobs` existed this refused unconditionally
/// (`CommandWorld::mobs` was always `None`), so the mob was never even
/// attempted.
#[tokio::test]
async fn rcon_summon_spawns_into_the_live_mob_handle() {
    let edits: Arc<Mutex<HashMap<(i32, i32, i32), String>>> = Arc::default();
    let (server, addr) = lan_rcon_server(edits).await;

    let mut client = AsyncRconClient::connect(addr, "hunter2")
        .await
        .expect("connect and authenticate");
    let response = client
        .command("summon minecraft:pig 12 64 12")
        .await
        .expect("command round-trips");
    assert!(response.contains("pig"), "unexpected response: {response}");

    let mobs = server.mobs().expect("open_to_lan must build a live MobHandle");
    let snapshots = mobs.with(|sim| sim.snapshots());
    assert!(
        snapshots.iter().any(|s| s.entity_type.to_string() == "minecraft:pig"),
        "the summoned pig must actually be in the simulation the tick loop ticks, \
         not a throwaway sim nothing streams: {snapshots:?}"
    );

    server.shutdown().await;
}

/// `/worldborder` over RCON now reads and mutates the *same* handle the tick
/// loop ticks (`IntegratedServer::border`), not a value with nowhere to go —
/// verified as a set-then-get round trip since that is the only externally
/// observable surface RCON has, and the honest scope this closes (see
/// `RconConfig::border`'s own doc comment for what remains open: no accepted
/// LAN connection reads this feed yet).
#[tokio::test]
async fn rcon_worldborder_set_and_get_round_trip_the_real_shared_border() {
    let edits: Arc<Mutex<HashMap<(i32, i32, i32), String>>> = Arc::default();
    let (server, addr) = lan_rcon_server(edits).await;

    let mut client = AsyncRconClient::connect(addr, "hunter2")
        .await
        .expect("connect and authenticate");
    let set = client
        .command("worldborder set 250")
        .await
        .expect("command round-trips");
    assert_eq!(set, "Set the world border to 250.0 blocks wide");

    let get = client
        .command("worldborder get")
        .await
        .expect("command round-trips");
    assert_eq!(
        get, "The world border is currently 250 blocks wide",
        "a second RCON command reading the border must see the first command's write, \
         which only holds if both go through the same stored handle"
    );

    server.shutdown().await;
}
