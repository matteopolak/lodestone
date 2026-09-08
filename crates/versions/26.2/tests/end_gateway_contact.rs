//! Real protocol/client acceptance for End-gateway contact.
//!
//! The source is deliberately a small in-memory End world. A real client joins
//! it, sends a real movement packet into the gateway, receives the production
//! teleport packet, and then observes the destination view replacing the source
//! view. The controls use the same path with the gateway metadata absent and
//! with the connection's gateway cooldown still active.

use std::time::Duration;

use lodestone_client::{
    ChunkPos, ClientBuilder, ClientEvent, ClientHandle, EventStream, LoginProfile, ServerAddress,
};
use lodestone_model::{Rotation, Vec3};
use lodestone_server::dimension::Dimension;
use lodestone_server::{
    BlockEntity, BlockEntityHandle, ChunkColumn, ChunkSource, ChunkEncodeError, MobHandle,
    NoEntities, ServerBound, ServerDirective, ServerError, ServerProtocol, ServeSummary,
    serve_connection,
};
use lodestone_v26_2::{V770ServerProtocol, adapter};
use lodestone_core::State;
use uuid::Uuid;

const GATEWAY: (i32, i32, i32) = (12, 100, 12);
const EXIT: (i32, i32, i32) = (40, 100, 40);
const EXIT_POSITION: Vec3 = Vec3::new(40.5, 100.0, 40.5);

/// Keeps the real v26 join/client framing while asking the production server to
/// encode this compact fixture as the ordinary client join shape. The source is
/// End-labelled for the contact decision, but the join packet remains the
/// protocol's normal Overworld frame because this gate does not cover a
/// dimension-change packet.
struct FixtureProtocol(V770ServerProtocol);

impl ServerProtocol for FixtureProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        self.0.decode(state, packet_id, payload)
    }

    fn login_success(&self, username: &str, uuid: uuid::Uuid) -> Vec<ServerDirective> {
        self.0.login_success(username, uuid)
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        self.0.begin_configuration()
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        self.0.begin_play(view_radius)
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        self.0.begin_chunk_batch()
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.0.encode_chunk(cx, cz, column)
    }

    fn try_encode_chunk_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
        _dimension: Dimension,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        self.0.try_encode_chunk(cx, cz, column)
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        self.0.end_chunk_batch(batch_size)
    }
}

/// A small world whose origin has a solid floor for the server's initial-spawn
/// search. The gateway block and optional metadata are carried by the source's
/// column, matching the generated-world fallback used when a chunk is first
/// hydrated.
struct GatewayWorld {
    gateway: bool,
    metadata: bool,
}

impl ChunkSource for GatewayWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        // Use the ordinary 26.2 build-height window: the client chooses its
        // chunk decoder shape from the join dimension, while the production
        // encoder chooses from the source column. Keeping them equal is what
        // lets this real-client fixture apply the chunk instead of dropping it
        // as a section-count mismatch.
        let mut column = ChunkColumn::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 99, z, "minecraft:stone");
            }
        }
        if self.gateway && (cx, cz) == (0, 0) {
            column.set_block(GATEWAY.0, GATEWAY.1, GATEWAY.2, "minecraft:end_gateway");
        }
        if self.gateway && self.metadata && (cx, cz) == (0, 0) {
            column.set_block_entities(vec![(
                lodestone_model::BlockPos::new(GATEWAY.0, GATEWAY.1, GATEWAY.2),
                BlockEntity::EndGateway {
                    exit: Some(lodestone_model::BlockPos::new(EXIT.0, EXIT.1, EXIT.2)),
                    exact: true,
                },
            )]);
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}

    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<BlockEntity> {
        (self.gateway && self.metadata && (x, y, z) == GATEWAY).then(|| BlockEntity::EndGateway {
            exit: Some(lodestone_model::BlockPos::new(EXIT.0, EXIT.1, EXIT.2)),
            exact: true,
        })
    }

    fn dimension(&self) -> Option<Dimension> {
        Some(Dimension::End)
    }
}

fn profile(name: &str) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid: Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

async fn connect(gateway: bool, metadata: bool) -> (
    lodestone_client::ClientHandle,
    EventStream,
    tokio::task::JoinHandle<Result<ServeSummary, ServerError>>,
) {
    let source = GatewayWorld { gateway, metadata };
    let (client_io, server_io) = lodestone_net::memory_pair();
    let block_entities = BlockEntityHandle::default();
    let server_task = tokio::spawn(async move {
        let mut conn = lodestone_net::Connection::new(server_io);
        serve_connection(
            &mut conn,
            &FixtureProtocol(V770ServerProtocol),
            &source,
            &NoEntities,
            0,
            &block_entities,
            &MobHandle::default(),
        )
        .await
    });
    let (handle, events) = ClientBuilder::new(
        address(),
        profile(if metadata { "Gateway" } else { "GatewayNoMetadata" }),
        Box::new(adapter()),
    )
        .connect_with(client_io);
    (handle, events, server_task)
}

async fn wait_for_spawn_and_acknowledge(handle: &ClientHandle) {
    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    let position = handle.position().expect("spawn wait supplied a position");
    handle
        .acknowledge_teleport_correction(position, handle.rotation())
        .expect("client still connected");
}

async fn walk_into_gateway(handle: &ClientHandle) {
    let start = handle.position().expect("spawn wait supplied a position");
    let rotation = handle.rotation();
    let target = Vec3::new(
        f64::from(GATEWAY.0) + 0.25,
        f64::from(GATEWAY.1),
        f64::from(GATEWAY.2) + 0.25,
    );
    for step in 1..=20 {
        let fraction = f64::from(step) / 20.0;
        let position = Vec3::new(
            start.x + (target.x - start.x) * fraction,
            target.y,
            start.z + (target.z - start.z) * fraction,
        );
        handle
            .move_to(position, rotation, true, false)
            .expect("send gateway movement");
        tokio::time::sleep(Duration::from_millis(75)).await;
    }
}

async fn wait_for_gateway_event(
    events: &mut EventStream,
) -> bool {
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = events.recv().await {
            if let ClientEvent::TeleportPlayer { pos, .. } = event {
                if pos == EXIT_POSITION {
                    return true;
                }
            }
        }
        false
    })
    .await
    .expect("gateway teleport event timed out")
}

#[tokio::test]
async fn gateway_contact_reaches_the_real_client_and_recentres_destination_view() {
    let (handle, mut events, server_task) = connect(true, true).await;
    wait_for_spawn_and_acknowledge(&handle).await;
    handle
        .wait_for_chunk(ChunkPos::new(0, 0), Duration::from_secs(30))
        .await
        .expect("source chunk never arrived");
    walk_into_gateway(&handle).await;
    assert!(wait_for_gateway_event(&mut events).await);
    assert_eq!(handle.position(), Some(EXIT_POSITION));
    handle
        .acknowledge_teleport_correction(EXIT_POSITION, Rotation::new(0.0, 0.0))
        .expect("acknowledge gateway teleport");

    handle
        .wait_for_chunk(ChunkPos::new(2, 2), Duration::from_secs(30))
        .await
        .expect("destination chunk never arrived");
    handle
        .wait_for(Duration::from_secs(30), |h| !h.is_chunk_loaded(ChunkPos::new(0, 0)))
        .await
        .expect("source chunk was not forgotten after gateway recenter");
    assert_eq!(handle.loaded_chunk_count(), 1);

    drop(handle);
    let _ = server_task.await;
}

#[tokio::test]
async fn gateway_contact_controls_missing_metadata_and_connection_cooldown() {
    let (handle, mut events, server_task) = connect(true, false).await;
    wait_for_spawn_and_acknowledge(&handle).await;
    handle
        .wait_for_chunk(ChunkPos::new(0, 0), Duration::from_secs(30))
        .await
        .expect("source chunk never arrived");
    walk_into_gateway(&handle).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_ne!(handle.position(), Some(EXIT_POSITION));
    assert!(!wait_for_gateway_event_short(&mut events).await);
    drop(handle);
    let _ = server_task.await;

    let (handle, mut events, server_task) = connect(true, true).await;
    wait_for_spawn_and_acknowledge(&handle).await;
    handle
        .wait_for_chunk(ChunkPos::new(0, 0), Duration::from_secs(30))
        .await
        .expect("source chunk never arrived");
    walk_into_gateway(&handle).await;
    assert!(wait_for_gateway_event(&mut events).await);
    handle
        .acknowledge_teleport_correction(EXIT_POSITION, Rotation::new(0.0, 0.0))
        .expect("acknowledge gateway teleport");
    handle
        .move_to(
            Vec3::new(
                f64::from(GATEWAY.0) + 0.25,
                f64::from(GATEWAY.1),
                f64::from(GATEWAY.2) + 0.25,
            ),
            Rotation::new(0.0, 0.0),
            true,
            false,
        )
        .expect("send cooldown movement");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        handle.position(),
        Some(Vec3::new(
            f64::from(GATEWAY.0) + 0.25,
            f64::from(GATEWAY.1),
            f64::from(GATEWAY.2) + 0.25,
        ))
    );
    drop(handle);
    let _ = server_task.await;
}

async fn wait_for_gateway_event_short(
    events: &mut EventStream,
) -> bool {
    let deadline = tokio::time::sleep(Duration::from_millis(200));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return false,
            event = events.recv() => {
                let Some(ClientEvent::TeleportPlayer { pos, .. }) = event else { return false };
                if pos == EXIT_POSITION { return true; }
            }
        }
    }
}
