//! Production-boundary proof for plugin command suggestions.
//!
//! A real Play connection sends a suggestion request through the server loop;
//! the installed `CommandDispatch` supplies permission-filtered plugin
//! candidates, and the protocol encoder returns the transaction/range payload.
//! This is intentionally separate from the registry tests: those cannot prove
//! that a live connection asks the host for completions.

use std::sync::Arc;

use lodestone_core::{Reader, State, Writer};
use lodestone_model::command_tree::CommandSuggestionsResponse;
use lodestone_net::{memory_pair, Connection};
use lodestone_server::plugin_commands::{
    ServerCommandOutcome, ServerCommandRegistry, ServerPermissions, ServerPluginCommand,
};
use lodestone_server::{
    serve_connection_with_commands, BlockEntityHandle, BlockTickFeed, ChunkColumn, ChunkSource,
    CommandDispatch, ExplosionFeed, MobHandle, NoEntities, ServerBound, ServerDirective,
    ServerProtocol,
};
use uuid::Uuid;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
const SUGGESTION_REQUEST: i32 = 42;
const SUGGESTION_RESPONSE: i32 = 43;

struct EmptyWorld;

impl ChunkSource for EmptyWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 1)
    }

    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:air".to_owned()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// A tiny value-only protocol: its custom suggestion packet makes the host
/// response observable without relying on any version-specific wire format.
struct SuggestionProtocol;

impl ServerProtocol for SuggestionProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => {
                ServerBound::Handshake { next_state: State::Login }
            }
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
            State::Play if packet_id == SUGGESTION_REQUEST => {
                let mut reader = Reader::new(payload);
                ServerBound::CommandSuggestion {
                    id: reader.var_i32().expect("suggestion transaction id"),
                    command: reader.string(256).expect("suggestion input"),
                }
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        vec![ServerDirective::Send { packet_id: LOGIN_SUCCESS, payload: Vec::new() }]
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

    fn encode_command_suggestions(&self, response: &CommandSuggestionsResponse) -> ServerDirective {
        let mut writer = Writer::default();
        writer.var_i32(response.id);
        writer.var_i32(response.start);
        writer.var_i32(response.length);
        writer.var_i32(response.suggestions.len() as i32);
        for suggestion in &response.suggestions {
            writer.string(&suggestion.text);
        }
        ServerDirective::Send {
            packet_id: SUGGESTION_RESPONSE,
            payload: writer.as_slice().to_vec(),
        }
    }
}

fn suggestion_dispatch() -> CommandDispatch {
    let mut command = ServerPluginCommand::new("suggest-probe");
    command.alias("sp");
    let branch = command.literal(command.root(), "branch");
    command.on_execute(branch, |_| ServerCommandOutcome::ran(1));

    let mut registry = ServerCommandRegistry::default();
    registry.register(command).expect("suggestion command registration");
    let permissions = ServerPermissions::default();
    Arc::new(registry).into_dispatch(Arc::new(permissions))
}

/// A plugin completion must survive the real handshake, Play packet dispatch,
/// host suggestion fallback, and protocol response encoding.
#[tokio::test]
async fn plugin_suggestion_reaches_the_real_play_connection() {
    let (client_io, server_io) = memory_pair();
    let mut client = Connection::new(client_io);
    let mut server = Connection::new(server_io);
    let protocol = SuggestionProtocol;
    let source = EmptyWorld;
    let entities = NoEntities;
    let block_entities = BlockEntityHandle::default();
    let mobs = MobHandle::default();
    let block_ticks = BlockTickFeed::default();
    let explosions = ExplosionFeed::default();
    let commands = suggestion_dispatch();

    let serve = serve_connection_with_commands(
        &mut server,
        &protocol,
        &source,
        &entities,
        0,
        &block_entities,
        &mobs,
        &block_ticks,
        &explosions,
        &commands,
    );
    let client_flow = async {
        client.write_packet(HANDSHAKE, &[2]).await.expect("handshake");
        let mut login = Writer::default();
        login.string("Joiner");
        client
            .write_packet(LOGIN_START, login.as_slice())
            .await
            .expect("login start");
        client.read_packet().await.expect("login success read").expect("login success");
        client
            .write_packet(LOGIN_ACKNOWLEDGED, &[])
            .await
            .expect("login acknowledgement");
        client
            .write_packet(FINISH_CONFIGURATION, &[])
            .await
            .expect("finish configuration");

        let mut request = Writer::default();
        request.var_i32(91);
        request.string("/sp ");
        client
            .write_packet(SUGGESTION_REQUEST, request.as_slice())
            .await
            .expect("suggestion request");

        loop {
            let (packet_id, payload) = client
                .read_packet()
                .await
                .expect("suggestion response read")
                .expect("connection closed before suggestion response");
            if packet_id != SUGGESTION_RESPONSE {
                continue;
            }
            let mut reader = Reader::new(&payload);
            let id = reader.var_i32().expect("response transaction id");
            let start = reader.var_i32().expect("response start");
            let length = reader.var_i32().expect("response length");
            let count = reader.var_i32().expect("response count");
            let suggestions = (0..count)
                .map(|_| reader.string(256).expect("suggestion text"))
                .collect::<Vec<_>>();
            return (id, start, length, suggestions);
        }
    };

    let (response, result) = tokio::join!(client_flow, serve);
    result.expect("production Play loop must serve the connection");
    assert_eq!(response, (91, 4, 0, vec!["branch".to_owned()]));
}
