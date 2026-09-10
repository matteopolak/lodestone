//! Production-path control for cancelled resident-block proposals.
//!
//! The Paper-shaped event bus runs before the integrated server writes its
//! authoritative source or publishes a block update. This gate drives that
//! route through a real `IntegratedServer`: a listener-present request must be
//! denied and leave the stone target unchanged, while the no-listener control
//! must write air. The denied branch also has no outbound block-change record.
//!
//! This is deliberately narrower than a player `BlockBreakEvent` wire probe.
//! The connection-side player break path still needs its own Paper event
//! adapter and a client/proxy that captures the corrective packet.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy_app::App;
use lodestone_core::State;
use lodestone_data::block_states::StateId;
use lodestone_model::BlockPos;
use lodestone_server::ecs::{
    PaperEvent, PaperEventBus, PaperEventKind, PaperEventPriority, ServerApp,
};
use lodestone_server::{
    BlockMutationRefusal, ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective,
    ServerProtocol,
};
use uuid::Uuid;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const TARGET: BlockPos = BlockPos::new(1, 64, 0);

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
    fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
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
