//! Production-path control for cancelled resident-block proposals.
//!
//! The Paper-shaped event bus runs before the integrated server writes its
//! authoritative source or publishes a block update. This gate drives that
//! route through a real `IntegratedServer`: a listener-present request must be
//! denied and leave the stone target unchanged, while the no-listener control
//! must write air. The denied branch also has no outbound block-change record.
//!
//! The player-break case below drives the same proposal owner through an
//! in-memory connection. Its cancellation branch returns the original stone
//! state to the client, while the no-listener control returns air, proving the
//! corrective output is attached to the production `BlockAction` consumer.
//!
#![cfg(not(target_arch = "wasm32"))]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy_app::App;
use lodestone_core::{Reader, State, Writer};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, GameMode};
use lodestone_net::Connection;
use lodestone_server::ecs::{
    PaperEvent, PaperEventBus, PaperEventKind, PaperEventPriority, ServerApp,
};
use lodestone_server::{
    BlockMutationRefusal, ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective,
    ServerProtocol,
};
use tokio::io::DuplexStream;
use uuid::Uuid;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const TARGET: BlockPos = BlockPos::new(1, 64, 0);
const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
const BLOCK_ACTION_C2S: i32 = 100;
const BLOCK_UPDATE_S2C: i32 = 101;

#[derive(Clone)]
struct FlatSource {
    writes: Arc<Mutex<Vec<(BlockPos, String)>>>,
}

impl FlatSource {
    fn new() -> Self {
        Self {
            writes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn column_with_target(&self) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        column.set_block(
            TARGET.x.rem_euclid(16),
            TARGET.y,
            TARGET.z.rem_euclid(16),
            "minecraft:stone",
        );
        column
    }
}

impl ChunkSource for FlatSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self.column_with_target()
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.column_with_target()
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, state: &str) {
        self.writes
            .lock()
            .expect("source write log")
            .push((BlockPos::new(x, y, z), state.to_owned()));
    }
}

#[derive(Debug)]
struct SilentProtocol;

impl ServerProtocol for SilentProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => {
                let mut reader = Reader::new(payload);
                ServerBound::LoginStart {
                    username: reader.string(16).expect("username"),
                    uuid: Uuid::nil(),
                }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            State::Play if packet_id == BLOCK_ACTION_C2S => {
                let mut reader = Reader::new(payload);
                let action = match reader.u8().expect("block action") {
                    0 => BlockActionKind::StartDestroy,
                    1 => BlockActionKind::AbortDestroy,
                    _ => BlockActionKind::StopDestroy,
                };
                ServerBound::BlockAction {
                    action,
                    pos: BlockPos::new(
                        reader.i32().expect("x"),
                        reader.i32().expect("y"),
                        reader.i32().expect("z"),
                    ),
                    face: BlockFace::Up,
                    sequence: 0,
                }
            }
            _ => ServerBound::Ignored,
        }
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

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        let mut writer = Writer::default();
        writer.i32(x);
        writer.i32(y);
        writer.i32(z);
        writer.string(state);
        ServerDirective::Send {
            packet_id: BLOCK_UPDATE_S2C,
            payload: writer.as_slice().to_vec(),
        }
    }
}

fn state(value: &str) -> StateId {
    StateId::from_state_str(value).expect("fixture block state")
}

fn cancelling_server_app() -> ServerApp {
    let air = state("minecraft:air");
    ServerApp::bootstrap_with(|app: &mut App| {
        app.world_mut()
            .resource_mut::<PaperEventBus>()
            .register(
                PaperEventKind::ResidentBlockChange,
                PaperEventPriority::Normal,
                "cancel-block-change",
                move |event| {
                    let PaperEvent::ResidentBlockChange { pos, state, .. } = event else {
                        unreachable!("event kind was filtered at registration");
                    };
                    if *pos == TARGET && *state == air {
                        event.cancel();
                    }
                },
            )
            .expect("resident block change is supported");
    })
}

fn cancelling_block_break_server_app() -> ServerApp {
    let stone = state("minecraft:stone");
    ServerApp::bootstrap_with(|app: &mut App| {
        app.world_mut()
            .resource_mut::<PaperEventBus>()
            .register(
                PaperEventKind::BlockBreak,
                PaperEventPriority::Normal,
                "cancel-block-break",
                move |event| {
                    let PaperEvent::BlockBreak { pos, state, .. } = event else {
                        unreachable!("event kind was filtered at registration");
                    };
                    assert_eq!(*pos, TARGET);
                    assert_eq!(*state, stone);
                    event.cancel();
                },
            )
            .expect("block break is supported");
    })
}

async fn drive_silent_join(client: &mut Connection<DuplexStream>) {
    client
        .write_packet(HANDSHAKE, &[2])
        .await
        .expect("handshake");
    let mut login = Writer::default();
    login.string("Breaker");
    client
        .write_packet(LOGIN_START, login.as_slice())
        .await
        .expect("login start");
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login acknowledgement");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("configuration finished");
}

async fn send_creative_start_destroy(client: &mut Connection<DuplexStream>) {
    let mut action = Writer::default();
    action.u8(0);
    action.i32(TARGET.x);
    action.i32(TARGET.y);
    action.i32(TARGET.z);
    client
        .write_packet(BLOCK_ACTION_C2S, action.as_slice())
        .await
        .expect("block action");
}

async fn read_block_update(client: &mut Connection<DuplexStream>) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (packet_id, payload) = client
                .read_packet()
                .await
                .expect("block update read")
                .expect("connection ended before block update");
            if packet_id != BLOCK_UPDATE_S2C {
                continue;
            }
            let mut reader = Reader::new(&payload);
            assert_eq!(reader.i32().expect("update x"), TARGET.x);
            assert_eq!(reader.i32().expect("update y"), TARGET.y);
            assert_eq!(reader.i32().expect("update z"), TARGET.z);
            return reader.string(128).expect("update state");
        }
    })
    .await
    .expect("the production break path must emit a corrective block update")
}

async fn wait_for_target(server: &IntegratedServer, expected: StateId) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if server.resident_block_state_id(TARGET.x, TARGET.y, TARGET.z) == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "integrated server did not retain target state {expected:?}; ticks={:?}",
            server.tick_stats().map(|stats| stats.tick_count),
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A denied production proposal must stop before both authoritative state and
/// the outbound block-change feed. The no-listener control proves that this is
/// a cancellation boundary rather than a permanently inert setter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_resident_change_does_not_publish_or_mutate() {
    let stone = state("minecraft:stone");
    let air = state("minecraft:air");
    let source = FlatSource::new();
    let writes = Arc::clone(&source.writes);
    let (server, client) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        SilentProtocol,
        source,
        (0..=0, 0..=0),
        (0, 0),
        0,
        0,
        cancelling_server_app(),
    );
    std::mem::forget(client);
    wait_for_target(&server, stone).await;
    server
        .block_ticks()
        .expect("integrated server owns a block-change feed")
        .drain_all();
    writes.lock().expect("source write log").clear();

    let result = server.set_resident_block_state_proposed(TARGET, air).await;
    assert_eq!(result, Err(BlockMutationRefusal::Denied));
    assert_eq!(
        server.resident_block_state_id(TARGET.x, TARGET.y, TARGET.z),
        Some(stone),
        "cancellation must leave the authoritative target unchanged"
    );
    assert!(
        writes.lock().expect("source write log").is_empty(),
        "a denied proposal must not call the source writer"
    );
    assert!(
        server
            .block_ticks()
            .expect("integrated server owns a block-change feed")
            .drain_all()
            .is_empty(),
        "a denied proposal must not publish an outbound block update"
    );
    server.shutdown().await;

    // Independent no-listener control: the same production API must reach the
    // source when no Paper listener claims the adjudication window.
    let source = FlatSource::new();
    let (server, client) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        SilentProtocol,
        source,
        (0..=0, 0..=0),
        (0, 0),
        0,
        0,
        ServerApp::bootstrap(),
    );
    std::mem::forget(client);
    wait_for_target(&server, stone).await;
    assert_eq!(
        server.set_resident_block_state_proposed(TARGET, air).await,
        Ok(())
    );
    wait_for_target(&server, air).await;
    server.shutdown().await;
}

/// The player path uses the same integrated proposal owner as resident-block
/// mutations: cancellation happens before `destroy_block`, and the acting
/// connection receives the source's current state as a corrective packet. The
/// no-listener control sends the identical packet sequence through the same
/// production connection and observes air instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_player_break_returns_authoritative_correction() {
    let stone = state("minecraft:stone");
    let source = FlatSource::new();
    let writes = Arc::clone(&source.writes);
    let (server, client_end) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        SilentProtocol,
        source,
        (0..=0, 0..=0),
        (0, 0),
        0,
        0,
        cancelling_block_break_server_app(),
    );
    server
        .world_state()
        .set_default_game_mode(GameMode::Creative);
    let mut client = Connection::new(client_end);
    drive_silent_join(&mut client).await;
    send_creative_start_destroy(&mut client).await;
    assert_eq!(read_block_update(&mut client).await, "minecraft:stone");
    assert_eq!(
        server.resident_block_state_id(TARGET.x, TARGET.y, TARGET.z),
        Some(stone),
        "cancellation must retain the authoritative target"
    );
    assert!(
        writes.lock().expect("source write log").is_empty(),
        "cancellation must stop before the source writer"
    );
    server.shutdown().await;

    let source = FlatSource::new();
    let (server, client_end) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        SilentProtocol,
        source,
        (0..=0, 0..=0),
        (0, 0),
        0,
        0,
        ServerApp::bootstrap(),
    );
    server
        .world_state()
        .set_default_game_mode(GameMode::Creative);
    let mut client = Connection::new(client_end);
    drive_silent_join(&mut client).await;
    send_creative_start_destroy(&mut client).await;
    assert_eq!(read_block_update(&mut client).await, "minecraft:air");
    assert_eq!(
        server.resident_block_state_id(TARGET.x, TARGET.y, TARGET.z),
        Some(state("minecraft:air")),
        "the no-listener control must apply the break"
    );
    server.shutdown().await;
}
