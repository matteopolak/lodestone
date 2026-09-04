//! Source RCON listener — the server half of the remote console.
//!
//! # What it is
//!
//! An optional TCP listener speaking the Source RCON protocol, authenticating
//! with a shared-secret password and executing server commands remotely. This
//! is the counterpart to `lodestone-testsupport`'s synchronous and asynchronous
//! clients (and `scripts/live-oracles/rcon-op.py`).
//!
//! # How it works
//!
//! Each frame is `[length i32 LE][request id i32 LE][packet type i32 LE]
//! [payload][0x00 0x00]`, where `length` counts the body *after* itself (so it
//! is `4 + 4 + payload.len() + 2`). The per-connection flow is:
//!
//! * a `TYPE_AUTH` (3) frame carrying the right password answers with
//!   `TYPE_AUTH_RESPONSE` (2) echoing the request id and an empty payload, and
//!   marks the connection authenticated;
//! * a wrong password — or any command before authentication — answers with
//!   request id `-1` and type 2;
//! * a `TYPE_COMMAND` (2) frame runs the command and answers with type 0,
//!   splitting responses longer than 4096 characters across frames (empty
//!   responses still send one empty frame);
//! * anything else answers `Unknown request <packet-type-in-hex>`.
//!
//! # Frame construction
//!
//! [`write_frame`] builds each length-prefixed response as one contiguous
//! byte-stream frame before passing all bytes to `write_all`. TCP may fragment
//! that stream during delivery; the read side uses `read_exact` for the length
//! and body so fragmentation is tolerated without changing frame boundaries.
//!
//! # How to change it
//!
//! * **Auth/response behaviour:** the per-connection state machine is
//!   [`handle_connection`]; the packet-type constants at the top of the module
//!   are keyed to the protocol's packet type values.
//! * **What a command does:** the **built-in tree** in [`crate::commands`] is
//!   consulted first, with the console identity (see [`rcon_caller`]) at
//!   permission level 4; only a root it does not own falls through to the host
//!   [`CommandDispatch`](crate::CommandDispatch) seam `crate::server`'s
//!   `ChatCommand` arm uses. That ordering is the same one the chat arm applies,
//!   deliberately: one entry point, so a command cannot behave differently
//!   depending on which transport typed it. RCON first executes the built-in
//!   tree and only then delegates unknown roots to the host sink, so `/gamerule`
//!   follows the same command path as chat.
//!
//!   **What RCON cannot do here is apply a per-connection effect.** It has no
//!   `ServerProtocol` and no transport of its own, so an [`Effect`](crate::Effect)
//!   aimed at a player is queued on the shared [`crate::PlayerRegistry`] and
//!   applied by that player's own loop; an effect aimed at nobody (the console
//!   has no body) has no target and is dropped. `/gamemode creative` with no
//!   argument therefore fails for RCON rather than silently doing nothing.
//!
//!   **`/setblock`, `/fill`, `/summon` and `/worldborder` are reachable.**
//!   Each uses a stored world, block-tick, mob, or border resource supplied by
//!   [`IntegratedServer::open_to_lan`](crate::IntegratedServer::open_to_lan)/
//!   `open_in_memory_with_mobs_using` — the world, block-tick, mob, and border
//!   handles — and stores them in [`RconConfig`] through
//!   [`start_rcon`](crate::IntegratedServer::start_rcon). `/setblock`/
//!   `/fill` write through `world_source` and publish through `block_ticks`
//!   directly here (RCON has no per-connection command arm to apply them
//!   through), rather than being dropped as the always-self-targeted
//!   [`Effect::SetBlock`]/[`Effect::Fill`] used for a real
//!   connection — see `crate::commands::block_commands`'s own doc for why they
//!   carry no player identity. `/worldborder`'s remaining gap is disclosed on
//!   [`IntegratedServer::border`](crate::IntegratedServer)'s own doc: the
//!   handle is shared with the tick loop, but no *accepted LAN connection* reads
//!   it yet (that needs its own per-connection plumbing) — RCON's query/set is
//!   honest regardless, since it reads and
//!   mutates the actual state the loop advances. `/tp` was never in this list:
//!   a teleport is an ordinary directed [`Effect`](crate::Effect) like
//!   `/gamemode <target>`, so `/tp <targets> <location>` reaches a connected
//!   player fine over RCON — only the bare, caller-implicit form
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

/// Default RCON port: the game port plus one.
pub const DEFAULT_RCON_PORT: u16 = 25575;

const TYPE_AUTH: i32 = 3;
const TYPE_COMMAND: i32 = 2;
const TYPE_AUTH_RESPONSE: i32 = 2;
const TYPE_RESPONSE: i32 = 0;
/// Request id used for an authentication failure.
const AUTH_FAILURE_ID: i32 = -1;
/// Per-frame response cap required by the RCON protocol.
const MAX_RESPONSE_CHARS: usize = 4096;
/// Frames longer than this are rejected rather than buffered. The ceiling
/// stops a hostile length field from allocating unbounded memory.
const MAX_FRAME_LENGTH: i32 = 1 << 20;
/// The console identity presented to the command dispatcher.
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
    /// The shared-secret password. An empty password is rejected, and an
    /// incorrect password fails closed.
    pub password: String,
    /// The seam commands execute through, with the console's identity — the
    /// same [`CommandDispatch`] `crate::server`'s `ChatCommand` arm consults,
    /// so RCON and an in-game player run through one dispatcher.
    pub commands: CommandDispatch,
    /// The server's own built-in commands, consulted before `commands`.
    pub builtins: ServerCommands,
    /// The world's shared state, for the rules `/gamerule` reads and writes.
    ///
    /// The shared handle is required by the shared-world-state invariant. A
    /// per-listener store would be invisible to other readers, so `/gamerule
    /// keep_inventory true` could report success while changing nothing they
    /// observe.
    pub world: crate::world_state::WorldStateHandle,
    /// The connected-player registry, for selector resolution and for the
    /// directed effect queue. `None` for a server with no registry, where RCON
    /// can still read and set game rules but has nobody to target.
    pub players: Option<crate::PlayerRegistry>,
    /// The world's shared, type-erased chunk source — `/setblock`/`/fill`'s
    /// write surface. `pub(crate)` rather than `pub`: [`IntegratedServer::start_rcon`]
    /// is the only legitimate source for this (a host cannot construct a real
    /// one from outside this crate, the same reason `world` is always
    /// substituted there too), so exposing a setter would only invite a
    /// caller to hand RCON a source nothing else shares.
    pub(crate) world_source: Option<crate::integrated::ErasedChunkSource>,
    /// The tick loop's outbound block-change hub — `/setblock`/`/fill`'s
    /// publish surface, so a console edit reaches every connected player.
    pub(crate) block_ticks: Option<crate::tick::BlockTickFeed>,
    /// The live mob simulation — `/summon`'s spawn surface. `None` is the
    /// honest answer for a config with no live world behind it, same as
    /// [`CommandWorld::mobs`](crate::commands::registrar::CommandWorld::mobs)'s
    /// own doc states for the reason a throwaway `MobHandle::default()` would
    /// be worse than refusing.
    pub(crate) mobs: Option<crate::mobs::MobHandle>,
    /// The world border — `/worldborder`'s read/write surface. See this
    /// module's own doc for the scope this closes (RCON reads/mutates the
    /// real, tick-loop-shared state) and the scope it does not (no accepted
    /// LAN connection reads this feed yet).
    pub(crate) border: Option<crate::border::BorderFeed>,
    /// `/op`/`/deop`/`/whitelist`'s read/write surface — the *shared*
    /// [`crate::access::AccessHandle`], not a fresh one, for the same reason
    /// [`Self::world`] is: a private copy would let RCON report success
    /// while granting operator status nobody's join check ever reads. `None`
    /// for a config with no access list configured (every plain constructor
    /// below), matching this crate's default-permissive singleplayer shape —
    /// see `crate::access`'s own module doc.
    pub(crate) access: Option<crate::access::AccessHandle>,
}

impl RconConfig {
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
            world_source: None,
            block_ticks: None,
            mobs: None,
            border: None,
            access: None,
        }
    }

    /// Points this config at a *shared* [`crate::access::AccessHandle`] — the
    /// only shape in which `/op`/`/deop`/`/whitelist` over RCON affect the
    /// running world's real join checks and command permission gating,
    /// rather than a private copy nothing else reads.
    #[must_use]
    pub fn with_access(mut self, access: crate::access::AccessHandle) -> Self {
        self.access = Some(access);
        self
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
/// authentication state machine required by the RCON protocol.
#[cfg(not(target_arch = "wasm32"))]
async fn handle_connection(stream: TcpStream, config: &RconConfig) -> std::io::Result<()> {
    let mut stream = stream;
    let mut authed = false;
    while let Some(frame) = read_frame(&mut stream).await? {
        match frame.packet_type {
            TYPE_AUTH => {
                // An empty password never matches, and a failed attempt
                // de-authenticates a connection that had already succeeded.
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
                // RCON clients send `/op Steve` and `op Steve` alike, and both
                // forms must run.
                let command = strip_optional_slash(&frame.payload);
                let response = run_command(config, command);
                write_response(&mut stream, frame.id, &join_response(&response)).await?;
            }
            other => {
                // Report the unknown packet type in hexadecimal.
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
/// Length-then-body via `read_exact`: fragmented TCP delivery is valid, so the
/// parser waits for the complete frame before decoding it.
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
    // The trailing two bytes are the payload's null terminator. The same
    // framing is consumed by the test-support client.
    let payload_end = body.len().saturating_sub(2);
    let payload = String::from_utf8_lossy(&body[8..payload_end]).into_owned();
    Ok(Some(Frame { id, packet_type, payload }))
}

/// Encodes one complete length-prefixed frame and passes its bytes to
/// `write_all`. The TCP stream may fragment delivery; `read_frame` uses
/// `read_exact` to reconstruct the length and body before decoding.
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

/// Writes a command response in frames of at most
/// [`MAX_RESPONSE_CHARS`] characters, including one empty frame for an empty
/// response.
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

/// Strips one optional leading slash from a command.
#[must_use]
fn strip_optional_slash(command: &str) -> &str {
    command.strip_prefix('/').unwrap_or(command)
}

/// The stdin console entry point: built-ins first, then the host sink for a
/// root they do not own, with identity `Server` at permission level 4. It
/// reuses [`run_command_as`] so both console transports share dispatch.
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
/// The console is the one caller in this crate that is not a player, and it is
/// deliberately the highest level rather than a bypass: the built-in tree's
/// permission filter runs for RCON exactly as it does for a player, so a
/// level-restricted command is gated by one mechanism.
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

/// [`run_command`], generalised over the caller's console identity. The
/// `crate::console` stdin runner reuses this shared body.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn run_command_as(
    config: &RconConfig,
    caller_name: &str,
    permission_level: u8,
    command: &str,
) -> CommandResponse {
    let candidates = config.players.as_ref().map(crate::PlayerRegistry::candidates).unwrap_or_default();
    let world = CommandWorld {
        rules: &config.world,
        players: &candidates,
        state: &config.world,
        // `/summon`'s spawn surface — `Some` whenever `IntegratedServer::start_rcon`
        // was called on a world with a live `MobHandle` (every constructor
        // that builds one at all; see that field's own doc comment).
        mobs: config.mobs.as_ref(),
        // `/worldborder`'s read/write surface — see `RconConfig::border`'s
        // own doc for what reading/mutating it here does and does not reach.
        border: config.border.as_ref(),
        // `/op`/`/deop`/`/whitelist`'s read/write surface — RCON is the one
        // production caller that gets `Some` here; see `RconConfig::access`'s
        // own doc for why.
        access: config.access.as_ref(),
        // `/execute if`/`unless block`'s read-only surface — the same
        // `world_source` `/setblock`/`/fill` already write through when RCON
        // has one (see this module's own doc, "How to change it").
        blocks: config.world_source.as_ref().map(|s| &**s as &dyn crate::chunk::ChunkSource),
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
                    // effect kind RCON could always deliver.
                    crate::commands::Effect::Broadcast { sender, message } => {
                        if let Some(players) = config.players.as_ref() {
                            players.say(&sender, &message);
                        }
                    }
                    // `crate::commands::block_commands`'s own doc: the console
                    // has no uuid, so `/setblock`/`/fill` target `Uuid::nil()`
                    // rather than refusing — applied here exactly as a live
                    // connection's own `ChatCommand` arm applies the identical
                    // self-targeted effect
                    // (`crate::server::dispatch_play_packet`), just against
                    // this config's stored `world_source`/`block_ticks`
                    // instead of a per-connection `SourceRef`.
                    crate::commands::Effect::SetBlock { pos: (x, y, z), block } => {
                        if let (Some(source), Some(block_ticks)) =
                            (config.world_source.as_ref(), config.block_ticks.as_ref())
                        {
                            source.set_block(x, y, z, &block);
                            block_ticks.publish(x, y, z, block);
                        }
                    }
                    crate::commands::Effect::Fill { positions, block } => {
                        if let (Some(source), Some(block_ticks)) =
                            (config.world_source.as_ref(), config.block_ticks.as_ref())
                        {
                            for (x, y, z) in positions {
                                source.set_block(x, y, z, &block);
                                block_ticks.publish(x, y, z, block.clone());
                            }
                        }
                    }
                    // `SetRespawnPoint` is dropped here by construction: it is
                    // always targeted at the console's own (nonexistent) uuid
                    // — a connection-local variable RCON has no analogue for —
                    // so this arm exists only so a future one added to
                    // `Effect` is not silently queued at a target nobody reads.
                    crate::commands::Effect::SetRespawnPoint { .. } => {}
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
/// The RCON console has **no player** behind it. This crate's seam carries no
/// permissions — the host [`CommandSink`](crate::CommandSink) resolves them
/// from the identity — so the name marks the console before player lookup;
/// the nil uuid cannot collide with a real player.
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
        // An empty response still yields one empty frame; the chunking loop
        // executes once even when the response has no characters.
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
