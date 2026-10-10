//! Production-boundary proof for the dedicated server plugin command registry.
//!
//! The command is registered with `plugin_commands`, adapted to the server's
//! existing dispatch seam, and sent through the real loopback RCON listener.
//! The response is read with the shared RCON client, so this covers the
//! registry -> sink -> listener -> wire response path rather than only a
//! parser unit test.

use std::sync::{Arc, Mutex};

use bevy_app::App;
use bevy_ecs::prelude::{ResMut, Resource};
use bevy_ecs::schedule::IntoScheduleConfigs;

use lodestone_command::{IntegerArgument, ParsedValue};
use lodestone_data::block_states::StateId;
use lodestone_server::ecs::{
    run_server_tasks, GameTick, ServerApp, ServerTaskScheduler, TickSet,
};
use lodestone_server::plugin_commands::{
    ServerCommandOutcome, ServerCommandQueue, ServerCommandRegistry, ServerCommandRequest,
    ServerCommandSource, ServerPermissionDefault, ServerPermissions, ServerPluginCommand,
};
use lodestone_server::{
    ChunkColumn, ChunkSource, CommandDispatch, IntegratedServer, RconConfig, ServerBound,
    ServerDirective, ServerProtocol,
};
use lodestone_testsupport::AsyncRconClient;
use uuid::Uuid;

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

    fn begin_play(&self, _join: &lodestone_server::JoinGame) -> Vec<ServerDirective> {
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

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
}

fn plugin_dispatch() -> CommandDispatch {
    let mut command = ServerPluginCommand::new("probe");
    command.alias("p").permission("example.probe");
    let count = command.argument(command.root(), "count", Arc::new(IntegerArgument::bounded(1, 9)));
    command.on_execute(count, |invocation| {
        let count = match invocation.parsed.argument("count") {
            Some(ParsedValue::Integer(value)) => *value,
            _ => return ServerCommandOutcome::refused("missing count"),
        };
        ServerCommandOutcome::Ran {
            feedback: vec![format!("plugin probe {count}")],
            result: count,
        }
    });

    let mut registry = ServerCommandRegistry::default();
    registry.register(command).expect("plugin command registration");
    let mut permissions = ServerPermissions::default();
    permissions.declare("example.probe", ServerPermissionDefault::True);
    Arc::new(registry).into_dispatch(Arc::new(permissions))
}

#[tokio::test]
async fn registered_server_plugin_command_reaches_rcon_response() {
    let (mut server, _client_io) =
        IntegratedServer::open_in_memory(SilentProtocol, EmptyWorld, 0);
    let address = server
        .start_rcon(RconConfig::new(
            ([127, 0, 0, 1], 0).into(),
            "hunter2",
            plugin_dispatch(),
        ))
        .expect("bind RCON listener");

    let mut client = AsyncRconClient::connect(address, "hunter2")
        .await
        .expect("authenticate RCON client");
    assert_eq!(client.command("/p 7").await.expect("run plugin command"), "plugin probe 7");

    server.shutdown().await;
}

#[derive(Resource)]
struct AsyncCommandDriver {
    queue: Mutex<ServerCommandQueue>,
    registry: Arc<ServerCommandRegistry>,
    permissions: Arc<ServerPermissions>,
}

fn drain_async_commands(
    driver: ResMut<AsyncCommandDriver>,
    mut scheduler: ResMut<ServerTaskScheduler>,
) {
    let registry = Arc::clone(&driver.registry);
    let permissions = Arc::clone(&driver.permissions);
    driver
        .queue
        .lock()
        .expect("async command queue lock")
        .drain_async(&mut scheduler, registry, permissions, usize::MAX);
}

/// The asynchronous command path must be live on the production server, not
/// just in the queue's unit tests: the real primary tick owner admits the
/// request, and the scheduler's hand-back is the only way its ticket resolves.
#[tokio::test]
async fn async_plugin_command_reaches_the_production_tick_owner() {
    let mut command = ServerPluginCommand::new("async-probe");
    command.permission("example.async");
    let root = command.root();
    command.on_execute(root, |_| ServerCommandOutcome::ran(73));

    let mut registry = ServerCommandRegistry::default();
    registry.register(command).expect("async command registration");
    let mut permissions = ServerPermissions::default();
    permissions.declare("example.async", ServerPermissionDefault::True);

    let (queue, handle) = ServerCommandQueue::with_capacity(1);
    let registry = Arc::new(registry);
    let permissions = Arc::new(permissions);
    let server_app = ServerApp::bootstrap_with(|app: &mut App| {
        app.insert_resource(AsyncCommandDriver {
            queue: Mutex::new(queue),
            registry: Arc::clone(&registry),
            permissions: Arc::clone(&permissions),
        });
        app.add_systems(
            GameTick,
            drain_async_commands
                .in_set(TickSet::Drain)
                .before(run_server_tasks),
        );
    });
    let (server, _client_io) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        SilentProtocol,
        EmptyWorld,
        (0..=0, 0..=0),
        (0, 0),
        1,
        server_app,
    );

    let ticket = handle
        .try_enqueue(ServerCommandRequest {
            source: ServerCommandSource::new(lodestone_server::CommandCaller::new(
                Uuid::from_u128(91),
                "async-player",
            )),
            input: "async-probe".to_owned(),
        })
        .expect("production command queue accepts one request");
    assert_eq!(ticket.try_recv().expect("ticket remains connected"), None);

    // The tick loop runs on its own OS thread with its own runtime, so this
    // test's runtime clock cannot drive it. No client joins, so lift the holds
    // that wait for one, then wait in real time for the ticket to resolve:
    // the scheduler's hand-back after a completed tick is its only path.
    server.world_state().release_initial_tick_holds();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let response = loop {
        if let Some(response) = ticket.try_recv().expect("ticket remains connected") {
            break Some(response);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the production primary loop never resolved the command ticket; ticks: {:?}",
            server.tick_stats().map(|stats| stats.tick_count)
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert!(
        server.tick_stats().is_some_and(|stats| stats.tick_count >= 1),
        "the ticket resolves only through a completed production tick"
    );

    server.shutdown().await;
    assert_eq!(
        response,
        Some(Ok(ServerCommandOutcome::ran(73))),
        "the production tick owner must be the only completion path for async command work"
    );
}
