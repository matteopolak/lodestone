//! A proposed player game-mode change must be adjudicated, queued for the
//! selected connection, and emitted by that connection's authoritative effect
//! consumer.

use std::time::Duration;

use bevy_app::{App, Plugin};
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::ResMut;
use bevy_ecs::schedule::IntoScheduleConfigs;
use lodestone_core::State;
use lodestone_model::{GameMode, Vec3};
use lodestone_net::Connection;
use lodestone_server::ecs::{
    GameTick, ProposalVerdict, ServerApp, ServerProposal, ServerProposalAction,
    ServerProposalDecisions, TickSet,
};
use lodestone_server::{
    Abilities, ChunkColumn, ChunkSource, IntegratedServer, PlayerGameModeRefusal, ServerBound,
    ServerDirective, ServerProtocol,
};
use tokio::io::DuplexStream;
use uuid::Uuid;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
const GAME_MODE_S2C: i32 = 91;

#[derive(Debug, Default)]
struct ModeProtocol;

impl ServerProtocol for ModeProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => {
                ServerBound::Handshake { next_state: State::Login }
            }
            State::Login if packet_id == LOGIN_START => {
                let mut reader = lodestone_core::Reader::new(payload);
                ServerBound::LoginStart {
                    username: reader.string(16).expect("username"),
                    uuid: Uuid::nil(),
                }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
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

    fn begin_play_at(&self, _view_radius: i32, _spawn: Vec3, mode: GameMode) -> Vec<ServerDirective> {
        vec![mode_directive(mode)]
    }

    fn encode_game_mode(&self, mode: GameMode) -> ServerDirective {
        mode_directive(mode)
    }

    fn encode_player_abilities(&self, _abilities: Abilities) -> ServerDirective {
        ServerDirective::None
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

fn mode_directive(mode: GameMode) -> ServerDirective {
    let mode = match mode {
        GameMode::Survival => 0,
        GameMode::Creative => 1,
        GameMode::Adventure => 2,
        GameMode::Spectator => 3,
    };
    ServerDirective::Send { packet_id: GAME_MODE_S2C, payload: vec![mode] }
}

#[derive(Debug, Default)]
struct FlatWorld;

impl ChunkSource for FlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }

    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:air".to_string()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

struct GameModePolicy;

impl Plugin for GameModePolicy {
    fn build(&self, app: &mut App) {
        app.add_systems(GameTick, adjudicate_game_mode.in_set(TickSet::Adjudicate));
    }
}

fn adjudicate_game_mode(
    mut proposals: MessageReader<ServerProposal>,
    mut decisions: ResMut<ServerProposalDecisions>,
) {
    for proposal in proposals.read() {
        if matches!(
            &proposal.action,
            ServerProposalAction::SetPlayerGameMode { mode: GameMode::Creative, .. }
        ) {
            decisions.decide(proposal.id(), 0, ProposalVerdict::Deny);
        } else if matches!(
            &proposal.action,
            ServerProposalAction::SetPlayerGameMode { mode: GameMode::Adventure, .. }
        ) {
            decisions.decide(
                proposal.id(),
                0,
                ProposalVerdict::Replace(ServerProposalAction::SpawnMob {
                    entity_type: "minecraft:cow".parse().expect("entity key"),
                    pos: Vec3::new(1.0, 2.0, 3.0),
                }),
            );
        }
    }
}

async fn join(
    server: &IntegratedServer,
    mut client: Connection<DuplexStream>,
) -> (Connection<DuplexStream>, Uuid) {
    client.write_packet(HANDSHAKE, &[2]).await.expect("handshake");
    let mut login = lodestone_core::Writer::default();
    login.string("Joiner");
    client
        .write_packet(LOGIN_START, login.as_slice())
        .await
        .expect("login start");
    client.read_packet().await.expect("login success").expect("login success frame");
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login acknowledgement");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("configuration finished");

    assert_eq!(read_mode(&mut client).await, GameMode::Survival);

    for _ in 0..1000 {
        if let Some(player) = server
            .players()
            .expect("shared player registry")
            .candidates()
            .into_iter()
            .next()
        {
            return (client, player.uuid);
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("player was not registered after the join sequence");
}

async fn read_mode(client: &mut Connection<DuplexStream>) -> GameMode {
    loop {
        let (packet_id, payload) = client
            .read_packet()
            .await
            .expect("game-mode packet")
            .expect("connection ended before game-mode packet");
        if packet_id != GAME_MODE_S2C {
            continue;
        }
        return match payload.as_slice() {
            [0] => GameMode::Survival,
            [1] => GameMode::Creative,
            [2] => GameMode::Adventure,
            [3] => GameMode::Spectator,
            other => panic!("unexpected game-mode payload: {other:?}"),
        };
    }
}

async fn read_mode_with_timeout(client: &mut Connection<DuplexStream>) -> Option<GameMode> {
    tokio::time::timeout(Duration::from_millis(150), read_mode(client))
        .await
        .ok()
}

fn open_server(server_app: ServerApp) -> (IntegratedServer, DuplexStream) {
    IntegratedServer::open_in_memory_with_mobs_and_server_app(
        ModeProtocol,
        FlatWorld,
        (0..=0, 0..=0),
        (0, 0),
        0,
        1,
        server_app,
    )
}

#[tokio::test]
async fn accepted_player_game_mode_change_reaches_the_authoritative_wire_path() {
    let (server, client_end) = open_server(ServerApp::bootstrap());
    let (mut client, target) = join(&server, Connection::new(client_end)).await;

    assert_eq!(
        server
            .set_player_game_mode_proposed(target, GameMode::Creative)
            .await,
        Ok(())
    );
    assert_eq!(read_mode(&mut client).await, GameMode::Creative);
    assert_eq!(
        server
            .players()
            .expect("shared player registry")
            .candidates()
            .into_iter()
            .find(|player| player.uuid == target)
            .expect("target remains connected")
            .game_mode,
        GameMode::Creative
    );

    server.shutdown().await;
}

#[tokio::test]
async fn denied_or_unknown_player_game_mode_change_emits_no_mode_packet() {
    let (server, client_end) = open_server(ServerApp::bootstrap_with(|app| {
        app.add_plugins(GameModePolicy);
    }));
    let (mut client, target) = join(&server, Connection::new(client_end)).await;

    assert_eq!(
        server
            .set_player_game_mode_proposed(target, GameMode::Creative)
            .await,
        Err(PlayerGameModeRefusal::Denied)
    );
    assert_eq!(read_mode_with_timeout(&mut client).await, None);

    assert_eq!(
        server
            .set_player_game_mode_proposed(target, GameMode::Adventure)
            .await,
        Err(PlayerGameModeRefusal::MismatchedAction)
    );
    assert_eq!(read_mode_with_timeout(&mut client).await, None);

    let unknown = Uuid::from_u128(1);
    assert_ne!(unknown, target);
    assert_eq!(
        server
            .set_player_game_mode_proposed(unknown, GameMode::Survival)
            .await,
        Err(PlayerGameModeRefusal::UnknownPlayer)
    );
    assert_eq!(read_mode_with_timeout(&mut client).await, None);

    server.shutdown().await;
}
