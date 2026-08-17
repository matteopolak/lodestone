//! A fresh join must actually read `WorldStateHandle::default_game_mode` —
//! not merely store what `/defaultgamemode` writes into it.
//!
//! # The defect this gate exists for
//!
//! `WorldStateHandle::default_game_mode`'s own doc claimed a consumer that did
//! not exist: `crate::server::serve_connection_inner`'s join arm hardcoded
//! `GameMode::Survival` rather than reading the handle at all, so
//! `/defaultgamemode creative` changed a store nothing downstream ever
//! consulted — a zero-production-reader field, confirmed by grep before this
//! fix (`default_game_mode()` had callers only in `#[cfg(test)]` code and the
//! dedicated-server binary's *write* side). `serve_connection_inner` now reads
//! `world.default_game_mode()` as the fallback a saved per-player value
//! overrides.
//!
//! # Where the expected value comes from
//!
//! The two arms of this gate compare a real join's delivered game mode
//! against the value written into `WorldStateHandle` moments earlier through
//! its own public setter — the same handle a production join now reads, via
//! the one public entry point (`serve_connection_with_access_and_state`) that
//! threads a caller-supplied `WorldStateHandle` at all. No world file, no
//! saved player data (there is none in this fixture — a fresh join), so the
//! only source `game_mode` can come from is the handle.

use std::sync::Mutex;

use lodestone_core::{Reader, State, Writer};
use lodestone_model::{GameMode, Vec3};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    AccessHandle, AccessLists, Abilities, BlockEntityHandle, ChunkColumn, ChunkSource,
    NoEntities, ServerBound, ServerDirective, ServerProtocol,
    serve_connection_with_access_and_state, world_state::WorldStateHandle,
};
use uuid::Uuid;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
const GAME_MODE_S2C: i32 = 91;

/// Flat, ungenerated terrain — this gate is about which mode a join delivers,
/// not about world content.
struct FlatSource;

impl ChunkSource for FlatSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }

    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:air".to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// Emits only what this gate reads: the game mode `begin_play_at` was called
/// with. A double that overrode nothing else would record nothing, exactly
/// like `join_view_centre.rs`'s `CoordProto`.
#[derive(Default)]
struct ModeProto {
    delivered_mode: Mutex<Option<GameMode>>,
}

impl ServerProtocol for ModeProto {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => {
                ServerBound::Handshake { next_state: State::Login }
            }
            State::Login if packet_id == LOGIN_START => {
                let mut r = Reader::new(payload);
                ServerBound::LoginStart { username: r.string(16).expect("username"), uuid: Uuid::nil() }
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
        *self.delivered_mode.lock().expect("mode lock") = Some(mode);
        let mut w = Writer::default();
        w.u8(mode as u8);
        vec![ServerDirective::Send { packet_id: GAME_MODE_S2C, payload: w.as_slice().to_vec() }]
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

/// Drives one real join through `serve_connection_with_access_and_state` and
/// returns the `GameMode` `begin_play_at` was actually called with.
async fn join_and_read_mode(world: &WorldStateHandle) -> GameMode {
    let (client_end, server_end) = memory_pair();
    let source = FlatSource;
    let proto = ModeProto::default();
    let access = AccessHandle::new(AccessLists::default());

    let mut client = Connection::new(client_end);
    let handshake = async {
        client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
        let mut w = Writer::default();
        w.string("Joiner");
        client.write_packet(LOGIN_START, w.as_slice()).await.expect("login start");
        client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
        client.write_packet(LOGIN_ACKNOWLEDGED, &[]).await.expect("login ack");
        client
            .write_packet(FINISH_CONFIGURATION, &[])
            .await
            .expect("finish configuration");

        loop {
            let Ok(Some((id, payload))) = client.read_packet().await else {
                panic!("connection closed before the game-mode packet arrived");
            };
            if id == GAME_MODE_S2C {
                let mut r = Reader::new(&payload);
                return match r.u8().expect("mode byte") {
                    0 => GameMode::Survival,
                    1 => GameMode::Creative,
                    2 => GameMode::Adventure,
                    3 => GameMode::Spectator,
                    other => panic!("unexpected game mode byte {other}"),
                };
            }
        }
    };

    let mut conn = Connection::new(server_end);
    let block_entities = BlockEntityHandle::default();
    let server = serve_connection_with_access_and_state(
        &mut conn,
        &proto,
        &source,
        &NoEntities,
        2,
        &access,
        world,
        &block_entities,
        None,
    );

    let (mode, _) = tokio::join!(handshake, server);
    mode
}

/// The control: with the store at its untouched default, a fresh join must
/// deliver vanilla's own default (`Survival`) — the value this gate would
/// also see if the store were never consulted at all, so this alone cannot
/// distinguish "wired" from "still hardcoded". The next test is the
/// discriminating one.
#[tokio::test]
async fn fresh_join_with_untouched_store_gets_survival() {
    let world = WorldStateHandle::new();
    assert_eq!(world.default_game_mode(), GameMode::Survival);
    assert_eq!(join_and_read_mode(&world).await, GameMode::Survival);
}

/// **The gate.** `/defaultgamemode`'s write side
/// (`WorldStateHandle::set_default_game_mode`) must change what a *subsequent*
/// real join actually delivers — Creative and Spectator both, so a
/// coincidental match on one value cannot pass this by accident (the
/// `Survival`-only case above is exactly the value a still-hardcoded join
/// would also produce).
#[tokio::test]
async fn fresh_join_reads_default_game_mode_from_the_store() {
    let world = WorldStateHandle::new();

    world.set_default_game_mode(GameMode::Creative);
    assert_eq!(join_and_read_mode(&world).await, GameMode::Creative);

    world.set_default_game_mode(GameMode::Spectator);
    assert_eq!(join_and_read_mode(&world).await, GameMode::Spectator);
}
