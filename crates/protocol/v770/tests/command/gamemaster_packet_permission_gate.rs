//! The low-permission refusal path for the six `COMMANDS_GAMEMASTER_LEVEL`
//! packets — `DifficultyChanged`, `DifficultyLockChanged`, `GameRuleChanged`,
//! `SetCommandBlock`, `ChangeGameMode` and `REQUEST_GAMERULE_VALUES`.
//!
//! # The hole this closes
//!
//! Every existing connection-level harness (`open_in_memory_with_mobs` and
//! friends) reaches its `dispatch_play_packet` through
//! `AccessHandle::default()` — an *unconfigured* [`AccessLists`], which
//! `AccessLists::command_permission_level`'s own doc explains resolves to
//! [`lodestone_server::MAX_PERMISSION_LEVEL`] for **every** caller (the
//! singleplayer-owner shape: a world with no operator model at all must not
//! lock its one player out of their own game). So the six gates above have
//! been asserted only in the direction that was already passing — "a
//! privileged caller may do this" — and never in the direction the gate
//! exists for. This is exactly the "assertions of an absence need a control
//! proving the detector works" rule: a refusal nothing can observe failing is
//! not tested.
//!
//! # The harness
//!
//! [`lodestone_server::serve_connection_with_access_and_state`] is new here
//! (added alongside this gate): every pre-existing public constructor that
//! takes a real [`AccessHandle`] builds its `WorldStateHandle`/
//! `BlockEntityHandle` privately and never hands them back, so nothing
//! outside the function could observe what a connection actually *did* to
//! them — only that the connection did not error. This constructor takes
//! both as caller-supplied handles, mirroring `serve_connection_with_access`
//! exactly except for that.
//!
//! Two [`AccessLists`] per test, differing only in who the *owner* is: `low`
//! names a uuid other than the connecting player (so the player resolves to
//! permission level 0 — not unconfigured, and not an op), `high` names the
//! connecting player itself (level 4, `MAX_PERMISSION_LEVEL`). Both are
//! deliberately *configured* — `AccessLists::is_unconfigured` is false for
//! either — which is the property no existing harness has.
//!
//! `REQUEST_GAMERULE_VALUES` has no [`ClientAction`] producer at all (it is
//! one ordinal on `client_command`'s shared `action` byte, alongside
//! `PERFORM_RESPAWN`/`REQUEST_STATS`, and nothing in `lodestone-model` — a
//! read-only crate from this side of the ownership split — encodes it). So
//! that one test builds its own frame with the real
//! [`lodestone_net::Connection::write_packet`] encoder (not a hand-rolled
//! byte layout) and splices it into what the server reads via [`Splice`],
//! leaving the real client's own traffic on the same duplex untouched in
//! both directions.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::{
    BlockPos, ClientAction, ClientEvent, CommandBlockMode, Difficulty, GameMode, ResourceKey,
};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::commands::registrar::RuleStore;
use lodestone_server::world_state::WorldStateHandle;
use lodestone_server::{
    AccessHandle, AccessLists, BlockEntity, BlockEntityHandle, ChunkColumn, ChunkSource,
    CommandBlockData, NoEntities, WorldgenChunkSource, serve_connection_with_access_and_state,
};
use lodestone_v770::packet_ids::play;
use lodestone_v770::{V770ServerProtocol, adapter};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, DuplexStream, ReadBuf};
use tokio::sync::mpsc;
use uuid::Uuid;

fn address() -> ServerAddress {
    ServerAddress { host: "memory".into(), port: 0 }
}

fn profile(name: &str, uuid: Uuid) -> LoginProfile {
    LoginProfile { username: name.into(), uuid }
}

/// Deterministic, noise-free terrain, matching `command_wire_path.rs`'s own
/// `cheap_source` — content is irrelevant to every test here except the
/// command-block one.
fn cheap_source() -> WorldgenChunkSource {
    WorldgenChunkSource::new(
        lodestone_worldgen::density::Density::YClampedGradient {
            from_y: -64.0,
            to_y: 64.0,
            from_value: 1.0,
            to_value: -1.0,
        },
        -64,
        384,
    )
}

/// An `AccessLists` with `owner` set and nobody else configured — real
/// "configured" state (`is_unconfigured() == false`), unlike every existing
/// harness's `AccessHandle::default()`. A caller whose uuid is not `owner`
/// resolves to permission level 0; `owner` itself resolves to level 4.
fn access_with_owner(owner: Uuid) -> AccessHandle {
    let mut lists = AccessLists::new();
    lists.set_owner(Some(owner));
    AccessHandle::new(lists)
}

/// Polls `f` until it returns `want` or `timeout` elapses, checking every 20ms.
/// Bounded so a broken gate reads as "value never changed" rather than a hang.
async fn poll_until<T: PartialEq + Clone>(timeout: Duration, mut f: impl FnMut() -> T, want: &T) -> T {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let v = f();
        if &v == want || tokio::time::Instant::now() >= deadline {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wraps the server half of an in-memory duplex so a test can splice one
/// extra, fully-framed byte sequence into what the server reads —
/// modelling "the client also sent this frame" for `REQUEST_GAMERULE_VALUES`,
/// which no `ClientAction` encodes. Writes pass straight through untouched,
/// so the server's real replies still reach the real client on the other end
/// of the same pair; only reads are spliced, and only ahead of the real
/// stream's own bytes.
struct Splice {
    inner: DuplexStream,
    pending: VecDeque<u8>,
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl AsyncRead for Splice {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        loop {
            match this.rx.poll_recv(cx) {
                Poll::Ready(Some(chunk)) => this.pending.extend(chunk),
                Poll::Ready(None) | Poll::Pending => break,
            }
        }
        if !this.pending.is_empty() {
            let n = buf.remaining().min(this.pending.len());
            let drained: Vec<u8> = this.pending.drain(..n).collect();
            buf.put_slice(&drained);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Splice {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, data)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Builds the exact bytes a real client would send for
/// `client_command(action)`, through the crate's own [`Connection`] encoder
/// (compression-framed at the same threshold `V770ServerProtocol` negotiates
/// at login) rather than a hand-rolled byte layout — the framing is not the
/// thing under test.
async fn encode_client_command(action: i32) -> Vec<u8> {
    let (a, mut b) = memory_pair();
    let mut conn = Connection::new(a);
    conn.set_compression(256);
    conn.write_packet(play::serverbound::CLIENT_COMMAND, &[u8::try_from(action).unwrap()])
        .await
        .expect("encode client_command");
    let mut buf = vec![0u8; 256];
    let n = b.read(&mut buf).await.expect("read the encoded frame back");
    buf.truncate(n);
    buf
}

// ---------------------------------------------------------------------------
// ChangeGameMode
// ---------------------------------------------------------------------------

/// The F4 switcher (`ServerBound::ChangeGameMode`). The server always echoes
/// back the connection's *actual* current mode, refused or not, so the
/// client's own reported mode is the observable: unchanged under refusal,
/// `Creative` once accepted.
#[tokio::test]
async fn change_game_mode_refuses_a_low_permission_caller_and_succeeds_for_the_owner() {
    async fn run(access: AccessHandle, uuid: Uuid) -> Option<GameMode> {
        let (client_io, server_io) = memory_pair();
        let source = cheap_source();
        let server = tokio::spawn(async move {
            let mut conn = Connection::new(server_io);
            serve_connection_with_access_and_state(
                &mut conn,
                &V770ServerProtocol,
                &source,
                &NoEntities,
                0,
                &access,
                &WorldStateHandle::new(),
                &BlockEntityHandle::default(),
                None,
            )
            .await
        });

        let (mut handle, _events) =
            ClientBuilder::new(address(), profile("GamemodeSwitcher", uuid), Box::new(adapter()))
                .connect_with(client_io);
        handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");
        assert_eq!(handle.game_mode(), Some(GameMode::Survival), "join default");

        handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Creative }).expect("send action");
        let result = poll_until(Duration::from_secs(3), || handle.game_mode(), &Some(GameMode::Creative)).await;

        handle.shutdown();
        server.abort();
        result
    }

    let stranger = Uuid::from_u128(1);
    let owner = Uuid::from_u128(2);
    assert_eq!(
        run(access_with_owner(owner), stranger).await,
        Some(GameMode::Survival),
        "a non-owner's request must be refused, leaving the mode unchanged"
    );
    // The control: the identical request from the owner is accepted, so the
    // refusal above was about permission and not about the request itself.
    assert_eq!(
        run(access_with_owner(owner), owner).await,
        Some(GameMode::Creative),
        "the owner's identical request must succeed"
    );
}

// ---------------------------------------------------------------------------
// DifficultyChanged + DifficultyLockChanged
// ---------------------------------------------------------------------------

/// Both packets share one gate and one confirmation path
/// (`apply_difficulty_change`), so one connection drives both.
#[tokio::test]
async fn difficulty_change_and_lock_refuse_a_low_permission_caller_and_succeed_for_the_owner() {
    async fn run(access: AccessHandle, uuid: Uuid) -> (Difficulty, bool) {
        let (client_io, server_io) = memory_pair();
        let source = cheap_source();
        let world = WorldStateHandle::new();
        let server_world = world.clone();
        let server = tokio::spawn(async move {
            let mut conn = Connection::new(server_io);
            serve_connection_with_access_and_state(
                &mut conn,
                &V770ServerProtocol,
                &source,
                &NoEntities,
                0,
                &access,
                &server_world,
                &BlockEntityHandle::default(),
                None,
            )
            .await
        });

        let (mut handle, _events) =
            ClientBuilder::new(address(), profile("DifficultyAdmin", uuid), Box::new(adapter()))
                .connect_with(client_io);
        handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");
        assert_eq!(world.difficulty(), (Difficulty::Normal, false), "default world state");

        handle.send_action(ClientAction::ChangeDifficulty { difficulty: Difficulty::Hard }).expect("send action");
        handle.send_action(ClientAction::LockDifficulty { locked: true }).expect("send action");
        // Poll on the *pair* changing, since either field alone could be a
        // partial application; the owner case needs both true.
        let result = poll_until(
            Duration::from_secs(3),
            || world.difficulty(),
            &(Difficulty::Hard, true),
        )
        .await;

        handle.shutdown();
        server.abort();
        result
    }

    let stranger = Uuid::from_u128(3);
    let owner = Uuid::from_u128(4);
    assert_eq!(
        run(access_with_owner(owner), stranger).await,
        (Difficulty::Normal, false),
        "a non-owner's requests must be refused, leaving difficulty and lock unchanged"
    );
    assert_eq!(
        run(access_with_owner(owner), owner).await,
        (Difficulty::Hard, true),
        "the owner's identical requests must succeed"
    );
}

// ---------------------------------------------------------------------------
// GameRuleChanged
// ---------------------------------------------------------------------------

#[tokio::test]
async fn game_rule_changed_refuses_a_low_permission_caller_and_succeeds_for_the_owner() {
    async fn run(access: AccessHandle, uuid: Uuid) -> Option<lodestone_server::game_rules::GameRuleValue> {
        let (client_io, server_io) = memory_pair();
        let source = cheap_source();
        let world = WorldStateHandle::new();
        let server_world = world.clone();
        let server = tokio::spawn(async move {
            let mut conn = Connection::new(server_io);
            serve_connection_with_access_and_state(
                &mut conn,
                &V770ServerProtocol,
                &source,
                &NoEntities,
                0,
                &access,
                &server_world,
                &BlockEntityHandle::default(),
                None,
            )
            .await
        });

        let (mut handle, _events) =
            ClientBuilder::new(address(), profile("RuleAdmin", uuid), Box::new(adapter()))
                .connect_with(client_io);
        handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");
        // `GameRules::get`'s own doc: an unset rule resolves to its vanilla
        // *default*, not `None` — `None` means an unknown identifier, which
        // `keep_inventory` is not. `keep_inventory`'s default is `false`, so
        // "true" (what the packet below requests) is pairwise-distinct from
        // it, which is what makes "unchanged" and "changed" different values.
        assert_eq!(
            world.get_rule("keep_inventory"),
            Some(lodestone_server::game_rules::GameRuleValue::Bool(false)),
            "keep_inventory's vanilla default, before anything is set"
        );

        let key = ResourceKey::new("minecraft", "keep_inventory").expect("valid resource key");
        handle.send_action(ClientAction::SetGameRules { entries: vec![(key, "true".to_string())] }).expect("send");
        let result = poll_until(
            Duration::from_secs(3),
            || world.get_rule("keep_inventory"),
            &Some(lodestone_server::game_rules::GameRuleValue::Bool(true)),
        )
        .await;

        handle.shutdown();
        server.abort();
        result
    }

    let stranger = Uuid::from_u128(5);
    let owner = Uuid::from_u128(6);
    assert_eq!(
        run(access_with_owner(owner), stranger).await,
        Some(lodestone_server::game_rules::GameRuleValue::Bool(false)),
        "a non-owner's rule change must be refused, leaving the rule at its default"
    );
    assert_eq!(
        run(access_with_owner(owner), owner).await,
        Some(lodestone_server::game_rules::GameRuleValue::Bool(true)),
        "the owner's identical rule change must succeed"
    );
}

// ---------------------------------------------------------------------------
// SetCommandBlock
// ---------------------------------------------------------------------------

const COMMAND_BLOCK_POS: BlockPos = BlockPos { x: 8, y: 64, z: 8 };

/// All-air except one fixed `minecraft:command_block` cell, so
/// `crate::command_block::facing`/`state_with` (read off the block state) see
/// a real command-block family string.
#[derive(Clone, Default)]
struct CommandBlockSource;

impl ChunkSource for CommandBlockSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(-64, 384)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        if (x, y, z) == (COMMAND_BLOCK_POS.x, COMMAND_BLOCK_POS.y, COMMAND_BLOCK_POS.z) {
            "minecraft:command_block".to_string()
        } else {
            "minecraft:air".to_string()
        }
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // Edits are not retained — this test only reads back the block
        // *entity* registry, never a re-read of this source.
    }
}

/// This gate is `vanilla's own player's own can use game master blocks`: creative mode **and**
/// `COMMANDS_GAMEMASTER_LEVEL`, both. `WorldStateHandle::default_game_mode`
/// exists but — measured, not assumed — has **no production reader**: every
/// connection's `game_mode` local starts hardcoded `GameMode::Survival`
/// (`serve_connection_inner`'s own comment says so: "this crate persists no
/// per-player game type … a runtime switch … moves it from there"), so
/// setting the store here would change nothing this test could observe. The
/// real, reachable way into creative is the gated `ChangeGameMode` packet
/// itself, sent first — which faithfully reproduces vanilla's actual shape
/// (a non-operator cannot legitimately reach creative mode either, so the
/// two gates are correlated in real play, not an artifact of this harness).
#[tokio::test]
async fn set_command_block_refuses_a_low_permission_caller_and_succeeds_for_the_owner() {
    async fn run(access: AccessHandle, uuid: Uuid) -> String {
        let (client_io, server_io) = memory_pair();
        let source = CommandBlockSource;
        let block_entities = BlockEntityHandle::default();
        block_entities.with(|reg| reg.insert(COMMAND_BLOCK_POS, BlockEntity::CommandBlock(CommandBlockData::new())));
        let server_world = WorldStateHandle::new();
        let server_block_entities = block_entities.clone();
        let server = tokio::spawn(async move {
            let mut conn = Connection::new(server_io);
            serve_connection_with_access_and_state(
                &mut conn,
                &V770ServerProtocol,
                &source,
                &NoEntities,
                0,
                &access,
                &server_world,
                &server_block_entities,
                None,
            )
            .await
        });

        let (mut handle, _events) =
            ClientBuilder::new(address(), profile("CmdBlockAdmin", uuid), Box::new(adapter()))
                .connect_with(client_io);
        handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");

        handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Creative }).expect("send action");
        let _ = poll_until(Duration::from_secs(2), || handle.game_mode(), &Some(GameMode::Creative)).await;

        handle
            .send_action(ClientAction::SetCommandBlock {
                pos: COMMAND_BLOCK_POS,
                command: "say hi".to_string(),
                mode: CommandBlockMode::Redstone,
                track_output: true,
                conditional: false,
                automatic: false,
            })
            .expect("send action");

        let result = poll_until(
            Duration::from_secs(3),
            || {
                block_entities.with(|reg| match reg.get(COMMAND_BLOCK_POS) {
                    Some(BlockEntity::CommandBlock(d)) => d.command.clone(),
                    _ => String::new(),
                })
            },
            &"say hi".to_string(),
        )
        .await;

        handle.shutdown();
        server.abort();
        result
    }

    let stranger = Uuid::from_u128(7);
    let owner = Uuid::from_u128(8);
    assert_eq!(
        run(access_with_owner(owner), stranger).await,
        "",
        "a non-owner's command-block edit must be refused, leaving the command empty"
    );
    assert_eq!(
        run(access_with_owner(owner), owner).await,
        "say hi",
        "the owner's identical edit must succeed"
    );
}

// ---------------------------------------------------------------------------
// REQUEST_GAMERULE_VALUES (client_command action 2)
// ---------------------------------------------------------------------------

/// The one packet with no `ClientAction` producer, so this splices a real,
/// crate-encoded frame into the server's read side via [`Splice`] rather than
/// driving it through `ClientHandle`. On success the server replies with
/// every rule ever explicitly set (`game_rule_values`, decoded client-side as
/// `ClientEvent::GameRulesChanged`); on refusal vanilla — and this port —
/// sends nothing at all, so the control is a bounded wait that must time out
/// rather than a value comparison.
#[tokio::test]
async fn request_gamerule_values_refuses_a_low_permission_caller_and_succeeds_for_the_owner() {
    async fn run(access: AccessHandle, uuid: Uuid) -> Option<Vec<(ResourceKey, String)>> {
        let (client_io, server_io) = memory_pair();
        let source = cheap_source();
        let world = WorldStateHandle::new();
        world.set_rule("keep_inventory", "true").expect("valid rule");
        let server_world = world.clone();
        let (inject_tx, inject_rx) = mpsc::unbounded_channel();
        let splice = Splice { inner: server_io, pending: VecDeque::new(), rx: inject_rx };
        let server = tokio::spawn(async move {
            let mut conn = Connection::new(splice);
            serve_connection_with_access_and_state(
                &mut conn,
                &V770ServerProtocol,
                &source,
                &NoEntities,
                0,
                &access,
                &server_world,
                &BlockEntityHandle::default(),
                None,
            )
            .await
        });

        let (mut handle, mut events) =
            ClientBuilder::new(address(), profile("RuleReader", uuid), Box::new(adapter()))
                .connect_with(client_io);
        handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");

        let frame = encode_client_command(2).await;
        inject_tx.send(frame).expect("inject the request_gamerule_values frame");

        let mut found = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline && found.is_none() {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(ClientEvent::GameRulesChanged { values })) => found = Some(values),
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }

        handle.shutdown();
        server.abort();
        found
    }

    let stranger = Uuid::from_u128(9);
    let owner = Uuid::from_u128(10);

    let refused = run(access_with_owner(owner), stranger).await;
    assert!(refused.is_none(), "a non-owner's query must get no reply at all, not an empty one: {refused:?}");

    let granted = run(access_with_owner(owner), owner).await.expect("the owner's query must get a reply");
    assert!(
        granted.iter().any(|(key, value)| key.path() == "keep_inventory" && value == "true"),
        "the reply must actually carry the rule this world has set: {granted:?}"
    );
}
