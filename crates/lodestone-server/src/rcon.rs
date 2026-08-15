//! Source RCON listener (issue #331) — the server half of the remote console.
//!
//! # What it is
//!
//! An optional TCP listener speaking the Source RCON protocol, authenticating
//! with a shared-secret password and executing server commands remotely. This
//! is the counterpart to `lodestone-testsupport`'s `RconClient`/
//! `AsyncRconClient` (and `scripts/live-oracles/rcon-op.py`), which until now
//! had only vanilla oracles to talk to.
//!
//! # How it works
//!
//! Each frame is `[length i32 LE][request id i32 LE][packet type i32 LE]
//! [payload][0x00 0x00]`, where `length` counts the body *after* itself (so it
//! is `4 + 4 + payload.len() + 2`). The per-connection flow mirrors vanilla's
//! `RconClient` (`net/minecraft/server/rcon/thread/RconClient.java`):
//!
//! * a `TYPE_AUTH` (3) frame carrying the right password answers with
//!   `TYPE_AUTH_RESPONSE` (2) echoing the request id and an empty payload, and
//!   marks the connection authenticated;
//! * a wrong password — or any command before authentication — answers with
//!   request id `-1` and type 2;
//! * a `TYPE_COMMAND` (2) frame runs the command and answers with type 0,
//!   splitting responses longer than 4096 characters across frames exactly as
//!   vanilla's `sendCmdResponse` does (empty responses still send one empty
//!   frame);
//! * anything else answers `Unknown request <packet-type-in-hex>`, matching
//!   vanilla's default arm.
//!
//! # The one-write rule
//!
//! CLAUDE.md's live-server hazard for *consuming* this protocol says vanilla's
//! RCON client performs exactly one `read()` per request and closes the socket
//! unless the whole frame arrived in it. The server-side twin, which this
//! module owns, is on the **write** side: [`write_frame`] builds the complete
//! frame as one contiguous buffer and sends it with a single `write_all`. A
//! real RCON client — including our own `RconClient`, which the integration
//! test points at this listener — may not tolerate a frame split across writes
//! any better than vanilla tolerates a split read.
//!
//! # How to change it
//!
//! * **Auth/response behaviour:** the per-connection state machine is
//!   [`handle_connection`]; the packet-type constants at the top of the module
//!   are keyed to vanilla's `RconClient`.
//! * **What a command does:** the **built-in tree** in [`crate::commands`] is
//!   consulted first, with the console identity (see [`rcon_caller`]) at
//!   permission level 4; only a root it does not own falls through to the host
//!   [`CommandDispatch`](crate::CommandDispatch) seam `crate::server`'s
//!   `ChatCommand` arm uses. That ordering is the same one the chat arm applies,
//!   deliberately: one entry point, so a command cannot behave differently
//!   depending on which transport typed it. Before this, RCON called the host
//!   sink *only* and bypassed the built-ins entirely — so `/gamerule` over RCON
//!   was answered by whatever the host did with unknown input.
//!
//!   **What RCON cannot do here is apply a per-connection effect.** It has no
//!   `ServerProtocol` and no transport of its own, so an [`Effect`](crate::Effect)
//!   aimed at a player is queued on the shared [`crate::PlayerRegistry`] and
//!   applied by that player's own loop; an effect aimed at nobody (the console has
//!   no body) has no target and is dropped. `/gamemode creative` with no argument
//!   therefore fails for RCON exactly as it does in vanilla
//!   (`getPlayerOrException`), rather than silently doing nothing.
//!
//!   **`/summon` is also unreachable from RCON**, for the same reason
//!   `/setblock`/`/fill` are: it needs a resource this listener does not carry.
//!   `CommandWorld::mobs` is `None` here (RCON builds no `MobHandle`), which the
//!   command refuses honestly rather than spawning into a throwaway sim nothing
//!   ticks or streams — see that field's own doc for why `None` is the correct
//!   answer rather than a `MobHandle::default()`. `/tp` is **not** in this
//!   list: a teleport is an ordinary directed [`Effect`](crate::Effect) exactly
//!   like `/gamemode <target>`, so `/tp <targets> <location>` reaches a
//!   connected player fine over RCON — only the bare, caller-implicit form
//!   (`/tp <location>`) fails, and only because the console has no body to move.
//!
//! # Configuration
//!
//! [`RconConfig`] holds the bind address, the password, the built-in command
//! tree, the world's rule store and player registry, and the host command
//! dispatch; [`IntegratedServer::start_rcon`](crate::IntegratedServer::start_rcon)
//! is the production wiring — it binds synchronously (so a port conflict fails
//! fast, before any task spawns) and runs the accept loop as a task racing the
//! server's own shutdown signal.
//!
//! # Dependencies
//!
//! `tokio::net` (native targets only — the whole module is `cfg`-gated off
//! wasm, like [`crate::region_source`]) and the crate's own
//! [`CommandDispatch`](crate::CommandDispatch) seam. No `lodestone-ecs`, no
//! protocol crate, nothing new in the browser bundle.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::command::{CommandCaller, CommandDispatch, CommandResponse};
use crate::commands::{CommandSource, CommandWorld, ServerCommands};
use crate::spawn::{Task, spawn};

/// Vanilla's default RCON port (`DedicatedServerProperties.rconPort`, the
/// game port plus one).
pub const DEFAULT_RCON_PORT: u16 = 25575;

const TYPE_AUTH: i32 = 3;
const TYPE_COMMAND: i32 = 2;
const TYPE_AUTH_RESPONSE: i32 = 2;
const TYPE_RESPONSE: i32 = 0;
/// The request id vanilla answers auth failure with (`RconClient.java:121`).
const AUTH_FAILURE_ID: i32 = -1;
/// Vanilla's per-frame response cap (`RconClient.sendCmdResponse`).
const MAX_RESPONSE_CHARS: usize = 4096;
/// Frames longer than this are rejected rather than buffered. Vanilla's own
/// cap is 1460 bytes (`PktUtils.MAX_PACKET_SIZE`); this is a generous ceiling
/// that still stops a hostile length field from allocating unbounded memory.
const MAX_FRAME_LENGTH: i32 = 1 << 20;
/// The console's name, matching vanilla's `RconConsoleSource` ("Rcon").
const RCON_NAME: &str = "Rcon";

/// Everything the listener needs to serve one RCON endpoint.
///
/// `Clone` because every accepted connection's task owns one. `Debug` for the
/// repo-wide lint.
#[derive(Debug, Clone)]
pub struct RconConfig {
    /// The address to bind, including the port (default [`DEFAULT_RCON_PORT`]).
    ///
    /// Use port `0` to let the OS pick one — what the integration tests do, and
    /// what a caller must do before it can read the bound address back from
    /// [`IntegratedServer::start_rcon`](crate::IntegratedServer::start_rcon).
    pub addr: SocketAddr,
    /// The shared-secret password. An empty password is never accepted (a
    /// "set a password" state, like vanilla refusing to enable RCON with an
    /// empty `rcon.password`), and a wrong one fails closed.
    pub password: String,
    /// The seam commands execute through, with the console's identity — the
    /// same [`CommandDispatch`] `crate::server`'s `ChatCommand` arm consults,
    /// so RCON and an in-game player run through one dispatcher.
    pub commands: CommandDispatch,
    /// The server's own built-in commands, consulted before `commands`.
    pub builtins: ServerCommands,
    /// The world's shared state, for the rules `/gamerule` reads and writes.
    ///
    /// The *shared* handle, not a fresh one: a per-listener store is the bug
    /// issues #327 and #328 were both reported for, and it would be invisible
    /// here — `/gamerule keep_inventory true` over RCON would report success and
    /// change nothing anyone reads.
    pub world: crate::world_state::WorldStateHandle,
    /// The connected-player registry, for selector resolution and for the
    /// directed effect queue. `None` for a server with no registry, where RCON
    /// can still read and set game rules but has nobody to target.
    pub players: Option<crate::PlayerRegistry>,
}

impl RconConfig {
    /// A config on loopback at the default port: the integrated server's admin
    /// console, not a dedicated box's. `loopback` rather than `0.0.0.0` is
    /// deliberate — singleplayer's RCON should not be reachable from the LAN
    /// unless someone explicitly widens `addr`.
    #[must_use]
    pub fn localhost(password: impl Into<String>, commands: CommandDispatch) -> Self {
        Self {
            addr: SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, DEFAULT_RCON_PORT)),
            password: password.into(),
            commands,
            builtins: ServerCommands::new(),
            world: crate::world_state::WorldStateHandle::default(),
            players: None,
        }
    }

    /// A config at an explicit address, with the built-in tree and an empty
    /// world/registry.
    ///
    /// Prefer this to a struct literal: the built-in tree, rule store and player
    /// registry are all defaulted here, so adding a fourth thing the listener
    /// needs does not break every caller. Chain
    /// [`with_world`](Self::with_world) to point it at a real world —
    /// [`IntegratedServer::start_rcon`](crate::IntegratedServer::start_rcon)
    /// substitutes its own shared `WorldStateHandle` regardless, so a host
    /// cannot accidentally give RCON a private copy of the game rules.
    #[must_use]
    pub fn new(addr: SocketAddr, password: impl Into<String>, commands: CommandDispatch) -> Self {
        Self {
            addr,
            password: password.into(),
            commands,
            builtins: ServerCommands::new(),
            world: crate::world_state::WorldStateHandle::default(),
            players: None,
        }
    }

    /// The same config pointed at a *shared* world and player registry — what
    /// [`IntegratedServer::start_rcon`](crate::IntegratedServer::start_rcon)
    /// builds, and the only shape in which `/gamerule` and `/give` over RCON
    /// affect the running world.
    #[must_use]
    pub fn with_world(
        mut self,
        world: crate::world_state::WorldStateHandle,
        players: Option<crate::PlayerRegistry>,
    ) -> Self {
        self.world = world;
        self.players = players;
        self
    }
}

/// Binds `config.addr` and spawns the accept loop as a task owned by the
/// caller, racing `shutdown`.
///
/// Binding is synchronous (a `std` listener converted to tokio), so a port
/// conflict is reported **before** any task spawns and before
/// [`IntegratedServer::start_rcon`](crate::IntegratedServer::start_rcon)
/// returns — the same "bind before returning" contract the LAN entry point
/// [`IntegratedServer::bind`](crate::IntegratedServer::bind) has.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn_listener(
    shutdown: Arc<Notify>,
    config: RconConfig,
) -> std::io::Result<(Task, SocketAddr)> {
    let std_listener = std::net::TcpListener::bind(config.addr)?;
    std_listener.set_nonblocking(true)?;
    let local_addr = std_listener.local_addr()?;
    let listener = TcpListener::from_std(std_listener)?;
    tracing::info!(target: "server", %local_addr, "RCON listening");
    let task = spawn(run_listener(shutdown, listener, config));
    Ok((task, local_addr))
}

/// The accept loop. Ends on shutdown (so the listener cannot outlive the
/// server that owns it) or when the listener itself fails (socket closed).
#[cfg(not(target_arch = "wasm32"))]
async fn run_listener(shutdown: Arc<Notify>, listener: TcpListener, config: RconConfig) {
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else { break };
                let config = config.clone();
                let signal = shutdown.clone();
                // Fire-and-forget, routing through the crate's spawn seam so
                // every task spawn stays confined to `crate::spawn`. The
                // connection races the *same* shutdown the listener does, so a
                // live console session cannot outlive the server; dropping the
                // returned `Task` handle detaches rather than aborts it.
                drop(spawn(async move {
                    tokio::select! {
                        _ = signal.notified() => {}
                        _ = handle_connection(stream, &config) => {}
                    }
                }));
            }
        }
    }
}

/// The per-connection state machine: read a frame, answer it, repeat.
///
/// Returns on a clean close between frames (`Ok(())`) or on a transport or
/// protocol error (`Err`), both of which drop the connection. Mirrors the
/// authentication state machine of vanilla's `RconClient.run`.
#[cfg(not(target_arch = "wasm32"))]
async fn handle_connection(stream: TcpStream, config: &RconConfig) -> std::io::Result<()> {
    let mut stream = stream;
    let mut authed = false;
    while let Some(frame) = read_frame(&mut stream).await? {
        match frame.packet_type {
            TYPE_AUTH => {
                // Vanilla (`RconClient.java:79-90`): an empty password never
                // matches, and a failed attempt de-authenticates a connection
                // that had previously succeeded.
                let ok = !frame.payload.is_empty() && frame.payload == config.password;
                authed = ok;
                let id = if ok { frame.id } else { AUTH_FAILURE_ID };
                write_frame(&mut stream, id, TYPE_AUTH_RESPONSE, "").await?;
            }
            TYPE_COMMAND => {
                if !authed {
                    write_frame(&mut stream, AUTH_FAILURE_ID, TYPE_AUTH_RESPONSE, "").await?;
                    continue;
                }
                // Vanilla's `Commands.trimOptionalPrefix`: RCON clients send
                // `/op Steve` and `op Steve` alike, and both must run.
                let command = strip_optional_slash(&frame.payload);
                let response = run_command(config, command);
                write_response(&mut stream, frame.id, &join_response(&response)).await?;
            }
            other => {
                // Vanilla's default arm (`RconClient.java:92`): the packet
                // type, in hex.
                write_response(
                    &mut stream,
                    frame.id,
                    &format!("Unknown request {:x}", other as u32),
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// One decoded inbound frame.
#[cfg(not(target_arch = "wasm32"))]
struct Frame {
    id: i32,
    packet_type: i32,
    payload: String,
}

/// Reads one complete frame, or `None` on a clean close between frames.
///
/// Length-then-body via `read_exact` rather than vanilla's single `read`:
/// robustness is the server's job, and nothing here assumes the whole frame
/// landed in one system call the way vanilla's `RconClient` requires of its
/// *clients* (see the module doc's one-write rule for why the write side keeps
/// that discipline instead).
#[cfg(not(target_arch = "wasm32"))]
async fn read_frame(stream: &mut TcpStream) -> std::io::Result<Option<Frame>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        // `tokio`'s `read_exact` reports the count (which equals the buffer
        // length on success); the value itself is not needed here.
        Ok(_) => {}
        // A clean close between frames is a normal disconnect, not an error.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = i32::from_le_bytes(len_buf);
    if !(10..=MAX_FRAME_LENGTH).contains(&len) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid RCON frame length {len}"),
        ));
    }
    let mut body = vec![0u8; len as usize];
    stream.read_exact(&mut body).await?;
    let id = i32::from_le_bytes(body[0..4].try_into().expect("frame length checked"));
    let packet_type = i32::from_le_bytes(body[4..8].try_into().expect("frame length checked"));
    // The trailing two bytes are the payload's null terminator (`RconClient`
    // writes `0 0` after the string), the same shape the client-side
    // `decode_rcon_response` in `lodestone-testsupport` trims off.
    let payload_end = body.len().saturating_sub(2);
    let payload = String::from_utf8_lossy(&body[8..payload_end]).into_owned();
    Ok(Some(Frame { id, packet_type, payload }))
}

/// Writes one complete frame in a single `write_all` — the module's one-write
/// rule (see the module doc for why a split write is as dangerous as a split
/// read is to vanilla).
#[cfg(not(target_arch = "wasm32"))]
async fn write_frame(
    stream: &mut TcpStream,
    id: i32,
    packet_type: i32,
    payload: &str,
) -> std::io::Result<()> {
    let body_len = 4 + 4 + payload.len() + 2;
    let mut frame = Vec::with_capacity(4 + body_len);
    frame.extend_from_slice(&(body_len as i32).to_le_bytes());
    frame.extend_from_slice(&id.to_le_bytes());
    frame.extend_from_slice(&packet_type.to_le_bytes());
    frame.extend_from_slice(payload.as_bytes());
    frame.extend_from_slice(&[0, 0]);
    stream.write_all(&frame).await
}

/// Writes a command response, chunking exactly as vanilla's `sendCmdResponse`
/// does: at most [`MAX_RESPONSE_CHARS`] characters per frame, and one empty
/// frame for an empty response (its do-while runs once).
#[cfg(not(target_arch = "wasm32"))]
async fn write_response(
    stream: &mut TcpStream,
    id: i32,
    response: &str,
) -> std::io::Result<()> {
    for chunk in response_chunks(response) {
        write_frame(stream, id, TYPE_RESPONSE, chunk).await?;
    }
    Ok(())
}

/// Splits a response into [`MAX_RESPONSE_CHARS`]-character frames. Split at a
/// `char` boundary so a multi-byte character is never torn across frames.
fn response_chunks(response: &str) -> Vec<&str> {
    if response.is_empty() {
        return vec![""];
    }
    let mut chunks = Vec::new();
    let mut rest = response;
    while !rest.is_empty() {
        let split = rest
            .char_indices()
            .nth(MAX_RESPONSE_CHARS)
            .map(|(idx, _)| idx)
            .unwrap_or(rest.len());
        chunks.push(&rest[..split]);
        rest = &rest[split..];
    }
    chunks
}

/// Strips one optional leading slash, exactly vanilla's
/// `Commands.trimOptionalPrefix`.
#[must_use]
fn strip_optional_slash(command: &str) -> &str {
    command.strip_prefix('/').unwrap_or(command)
}

/// The stdin console's own entry point (dedicated-server binary): built-ins
/// first, host sink for a root they do not own, identity "Server" at
/// permission level 4 — vanilla's own dedicated-server console identity,
/// distinct from RCON's "Rcon" (`RconConsoleSource` vs `MinecraftServer`
/// itself as a `CommandSource`, both level 4/`LEVEL_OWNERS`). Reuses
/// [`run_command_as`] rather than reimplementing the built-in-then-host-sink
/// fallback a second time.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn run_console_command(config: &RconConfig, command: &str) -> String {
    let command = strip_optional_slash(command);
    join_response(&run_command_as(config, "Server", RCON_PERMISSION_LEVEL, command))
}

/// Flattens a [`CommandResponse`] into the text an RCON admin reads back.
///
/// Each `lines()` entry is one system-chat line; RCON joins them with newlines
/// so a multi-line response reads the way it would to the player who typed it.
fn join_response(response: &CommandResponse) -> String {
    response.lines().join("\n")
}

/// The console's permission level.
///
/// Vanilla's `RconConsoleSource` builds its `CommandSourceStack` with
/// `Commands.LEVEL_OWNERS` (4) — the console is a full owner. This is the one
/// caller in this crate that is not a player, and it is deliberately the highest
/// level rather than a bypass: the built-in tree's permission filter runs for RCON
/// exactly as it does for a player, so a future level-restricted command is
/// gated by one mechanism and not two.
const RCON_PERMISSION_LEVEL: u8 = 4;

/// Runs one RCON command: built-ins first, host sink for a root they do not own.
///
/// Cross-player effects are queued on the shared registry; an effect aimed at the
/// console itself cannot exist, because [`CommandSource::console`] has no entity
/// and therefore no uuid for a command to address.
#[cfg(not(target_arch = "wasm32"))]
fn run_command(config: &RconConfig, command: &str) -> CommandResponse {
    run_command_as(config, RCON_NAME, RCON_PERMISSION_LEVEL, command)
}

/// [`run_command`], generalised over the caller's console identity — the
/// shared body `crate::console`'s stdin runner reuses rather than
/// reimplementing (name "Server", the same permission level 4 vanilla's
/// dedicated-server console operates at). See that module's own doc comment
/// for why a second, un-networked console needs this at all.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn run_command_as(
    config: &RconConfig,
    caller_name: &str,
    permission_level: u8,
    command: &str,
) -> CommandResponse {
    let candidates = config.players.as_ref().map(crate::PlayerRegistry::candidates).unwrap_or_default();
    // `mobs: None` — RCON has no `MobHandle` in scope at all (see
    // `crate::commands::registrar::CommandWorld::mobs`'s own doc for why that
    // is the honest answer rather than a throwaway sim), so `/summon` refuses
    // over RCON exactly as `/setblock`/`/fill` already do for a different
    // missing resource.
    let world = CommandWorld {
        rules: &config.world,
        players: &candidates,
        state: &config.world,
        mobs: None,
        // RCON has no live border to reach — same honest gap as `mobs: None`
        // just above.
        border: None,
    };
    let source = CommandSource::console(
        caller_name,
        crate::commands::overworld_dimension(),
        permission_level,
    );
    match config.builtins.run(&world, &source, command) {
        Some(outcome) => {
            for directed in outcome.effects {
                match directed.effect {
                    // `/say`/`/me` need no player *target* at all — see this
                    // module's own doc for why they are the one self-targeted
                    // effect kind RCON can still deliver: unlike `SetBlock`/
                    // `Fill` (which need a chunk source RCON does not have) and
                    // `SetRespawnPoint` (a connection-local variable), a
                    // broadcast only needs the registry this function already
                    // holds.
                    crate::commands::Effect::Broadcast { sender, message } => {
                        if let Some(players) = config.players.as_ref() {
                            players.say(&sender, &message);
                        }
                    }
                    // `SetBlock`/`Fill`/`SetRespawnPoint` are dropped here by
                    // construction: they are always targeted at the console's
                    // own (nonexistent) uuid or refuse outright before reaching
                    // this point (`/setblock`/`/fill`'s own `ctx.source.uuid()`
                    // guard), so this arm exists only so a future one added to
                    // `Effect` is not silently queued at a target nobody reads.
                    crate::commands::Effect::SetBlock { .. }
                    | crate::commands::Effect::Fill { .. }
                    | crate::commands::Effect::SetRespawnPoint { .. } => {}
                    effect => {
                        if let Some(players) = config.players.as_ref() {
                            players.push_effect(directed.target, effect);
                        }
                    }
                }
            }
            outcome.response
        }
        None => config
            .commands
            .run(&CommandCaller::new(Uuid::nil(), caller_name), command),
    }
}

/// The identity commands executed over RCON present to the host sink.
///
/// Vanilla names the console "Rcon" (`RconConsoleSource`, a full-owner
/// `CommandSource` with **no player** behind it). This crate's seam carries no
/// permissions — the host [`CommandSink`](crate::CommandSink) resolves them
/// from the identity — so the *name* is what marks the console, exactly as it
/// is in vanilla. A sink that looks the uuid up as a player will find no one,
/// so it must special-case the console by name before that lookup; the nil
/// uuid keeps the identity from ever colliding with a real player.
#[must_use]
fn rcon_caller() -> CommandCaller {
    CommandCaller::new(Uuid::nil(), RCON_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_slash_is_stripped_like_vanillas_trim_optional_prefix() {
        assert_eq!(strip_optional_slash("/op Steve"), "op Steve");
        assert_eq!(strip_optional_slash("op Steve"), "op Steve");
        assert_eq!(strip_optional_slash("/"), "");
    }

    #[test]
    fn responses_are_chunked_at_4096_characters_like_vanillas_send_cmd_response() {
        // An empty response still yields one empty frame (the do-while runs
        // once in `sendCmdResponse`).
        assert_eq!(response_chunks(""), vec![""]);
        assert_eq!(response_chunks("hello"), vec!["hello"]);

        let long = "x".repeat(MAX_RESPONSE_CHARS + 7);
        let chunks = response_chunks(&long);
        assert_eq!(chunks.len(), 2, "one full frame and a 7-char tail");
        assert_eq!(chunks[0].len(), MAX_RESPONSE_CHARS);
        assert_eq!(chunks[0].len() + chunks[1].len(), long.len());
    }

    #[test]
    fn chunking_never_splits_a_multi_byte_character() {
        // Exactly one frame's worth of two-byte chars: a single chunk, intact.
        let wide = "€".repeat(MAX_RESPONSE_CHARS);
        assert_eq!(response_chunks(&wide), vec![wide.as_str()]);

        // One over the cap: the split must land on a char boundary, so no
        // chunk contains a torn codepoint.
        let wide = "€".repeat(MAX_RESPONSE_CHARS + 1);
        let chunks = response_chunks(&wide);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].is_char_boundary(chunks[0].len()));
        assert_eq!(chunks[0].chars().count(), MAX_RESPONSE_CHARS);
        assert_eq!(chunks[1].chars().count(), 1);
    }

    #[test]
    fn the_console_caller_is_vanillas_rcon_name_with_a_nil_uuid() {
        let caller = rcon_caller();
        assert_eq!(caller.username, "Rcon");
        assert_eq!(caller.uuid, Uuid::nil());
    }
}
