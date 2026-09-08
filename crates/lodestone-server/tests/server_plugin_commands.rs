//! Production-boundary proof for the dedicated server plugin command registry.
//!
//! The command is registered with `plugin_commands`, adapted to the server's
//! existing dispatch seam, and sent through the real loopback RCON listener.
//! The response is read with the shared RCON client, so this covers the
//! registry -> sink -> listener -> wire response path rather than only a
//! parser unit test.

use std::sync::Arc;

use lodestone_command::{IntegerArgument, ParsedValue};
use lodestone_server::plugin_commands::{
    ServerCommandOutcome, ServerCommandRegistry, ServerPermissionDefault,
    ServerPermissions, ServerPluginCommand,
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
}

fn plugin_dispatch() -> CommandDispatch {
    let mut command = ServerPluginCommand::new("probe");
    command.alias("p").permission("example.probe");
    let count = command.argument(command.root(), "count", Arc::new(IntegerArgument::bounded(1, 9)));
    command.on_execute(|invocation| {
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
