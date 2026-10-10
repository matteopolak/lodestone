//! Natural-spawn publication reaches the server's entity encoder.
//!
//! # Why this file exists when `natural_spawn.rs` already passes
//!
//! `tests/natural_spawn.rs` drives `MobSim::run_spawn_cycle` and
//! `NaturalSpawner` **directly**, over a hand-built `ChunkWorld`, and asserts on
//! `MobSim::iter`. Every one of its claims is about the engine. None of them is
//! about the engine being *reached*: the whole production chain between the tick
//! loop and an encoder call — `WorldStateHandle::spawn_mobs`, the player list
//! registered at Play join, `MobFeed`, `EntityStreamer::sync` and
//! `ServerProtocol::encode_add_entity` —
//! is invisible to it. That is the island shape this repo keeps paying for: a
//! subsystem individually green and consuming nothing.
//!
//! So this gate starts a real [`IntegratedServer`] with a real tick loop, joins
//! a connection through the duplex, acknowledges client readiness, stays
//! stationary, and counts
//! `encode_add_entity` calls produced by the natural spawn cycle.
//! The protocol double returns no AddEntity bytes. Real transport, decode and
//! shared client ECS acceptance lives in the v26 integration test
//! `natural_spawn_reaches_the_client`; neither fixture asserts rendered pixels.
//!
//! # The fixture, and why it is hand-built
//!
//! Same reasoning as `tests/natural_spawn.rs`: the *surface* is stubbed, the
//! biome list and the light engine are real. `ChunkColumn::new` biomes every
//! quart as `minecraft:plains`, so `NaturalSpawner` consults the genuine bundled
//! plains spawn list. A grass surface under open sky is the cheapest input the
//! creature pass accepts.
//!
//! The test starts with an empty entity simulation, then turns the `spawn_mobs`
//! game rule off in its negative control. This ensures the observed entities come
//! from the natural spawn path rather than another producer.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::State;
use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;
use lodestone_net::Connection;
use bevy_app::{App, Plugin};
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::{Res, ResMut, Resource};
use bevy_ecs::schedule::IntoScheduleConfigs;
use lodestone_server::ecs::{
    GameTick, ProposalVerdict, ServerApp, ServerProposal, ServerProposalAction,
    ServerProposalDecisions, TickSet,
};
use lodestone_server::{
    ChunkColumn, ChunkSource, EntitySnapshot, IntegratedServer, ServerBound, ServerDirective,
    ServerProtocol,
};
use uuid::Uuid;

/// A shallow world: a natural attempt's start height is uniform between the world
/// floor and one above the surface, and creatures only attempt every 400th tick,
/// so a deep column would make the one useful height a one-in-136 draw.
const MIN_Y: i32 = 60;
const HEIGHT: i32 = 32;
/// Surface height: above sea level so the water spawn lists do not compete, and
/// inside the band the plains creature rules accept.
const FLOOR: i32 = 70;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
/// Our own id for the readiness packet this file synthesises. The protocol double
/// decides what a packet id means, so this only has to avoid the four above.
const PLAYER_LOADED: i32 = 40;

/// Bounded, and long: the spawn cycle runs once per 50 ms tick and the cluster
/// loop is probabilistic, so this is a deadline the loop below polls against —
/// never a sleep whose expiry is itself the assertion. A plains surface in
/// daylight only spawns creatures, which open on game ticks divisible by 400
/// (20 s apart). The window ends before the server drops the silent connection.
const DEADLINE: Duration = Duration::from_secs(28);

/// Every `encode_add_entity` the server made, by type key.
#[derive(Debug, Default)]
struct Observed {
    spawned: Mutex<Vec<String>>,
    /// Set once the connection has reached Play, before readiness is announced.
    in_play: AtomicBool,
}

#[derive(Debug)]
struct WatchingProtocol(Arc<Observed>);

impl ServerProtocol for WatchingProtocol {
    fn decode(&self, state: State, packet_id: i32, _payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => ServerBound::LoginStart {
                username: "SpawnWatch".to_string(),
                uuid: Uuid::nil(),
            },
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            State::Play if packet_id == PLAYER_LOADED => ServerBound::PlayerLoaded,
            _ => ServerBound::Ignored,
        }
    }
    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: LOGIN_SUCCESS,
            payload: Vec::new(),
        }]
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_play(&self, _join: &lodestone_server::JoinGame) -> Vec<ServerDirective> {
        self.0.in_play.store(true, Ordering::SeqCst);
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

    /// Observes the encoder invocation, before any transport bytes exist.
    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        self.0
            .spawned
            .lock()
            .expect("spawn lock")
            .push(entity.entity_type.to_string());
        ServerDirective::None
    }
}

/// Grass at [`FLOOR`] over stone, open sky above, `minecraft:plains` everywhere
/// (`ChunkColumn::new`'s default biome). The spawnable surface, and nothing else.
#[derive(Debug)]
struct PlainsWorld;

impl PlainsWorld {
    fn build(&self) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                for y in FLOOR - 8..FLOOR {
                    column.set_block_id(x, y, z, Block::Stone.default_state());
                }
                column.set_block_id(
                    x,
                    FLOOR,
                    z,
                    StateId::from_state_str("minecraft:grass_block[snowy=false]")
                        .expect("grass fixture state exists"),
                );
            }
        }
        column
    }
}

impl ChunkSource for PlainsWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self.build()
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        self.build()
            .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.build()
            .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
}

/// Runs a real server for up to [`DEADLINE`] with stationary joined presence,
/// and returns every entity type the AddEntity encoder was asked to encode.
///
/// `spawn_mobs` is set through the world's own game-rule path when
/// `spawn_mobs == false`, which is what makes the negative control travel the
/// same tick and publication path as the gate.
async fn run(spawn_mobs: bool, deadline: Duration) -> Vec<String> {
    run_with_server_app(spawn_mobs, deadline, None, None).await
}

/// `stop_when` ends the run shortly after that counter first becomes non-zero: the
/// connection is silent and the server drops it after about half a minute, so a
/// run that waits for something other than an encoded entity must not outlive that.
async fn run_with_server_app(
    spawn_mobs: bool,
    deadline: Duration,
    server_app: Option<ServerApp>,
    stop_when: Option<Arc<AtomicUsize>>,
) -> Vec<String> {
    let observed = Arc::new(Observed::default());
    let protocol = WatchingProtocol(Arc::clone(&observed));
    let area = (-3..=3, -3..=3);
    let (server, client) = if let Some(server_app) = server_app {
        IntegratedServer::open_in_memory_with_mobs_and_server_app(
            protocol, PlainsWorld, area, (8, 8), 3, server_app,
        )
    } else {
        IntegratedServer::open_in_memory_with_mobs(protocol, PlainsWorld, area, (8, 8), 3)
    };
    if !spawn_mobs {
        server
            .world_state()
            .set_rule("spawn_mobs", "false")
            .expect("spawn_mobs is a known rule");
    }

    let mut client = Connection::new(client);
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    client
        .write_packet(LOGIN_START, &[0])
        .await
        .expect("login start");
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let start = tokio::time::Instant::now();
    let mut loaded = false;
    while start.elapsed() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !loaded && observed.in_play.load(Ordering::SeqCst) {
            client.write_packet(PLAYER_LOADED, &[]).await.expect("player loaded");
            loaded = true;
        }
        if stop_when.as_ref().is_some_and(|witness| witness.load(Ordering::SeqCst) > 0) {
            tokio::time::sleep(Duration::from_millis(500)).await;
            break;
        }
        if !observed.spawned.lock().expect("spawn lock").is_empty() {
            // Keep going a little past the first spawn so the report is not a
            // single sample, then stop — the assertion is on what was seen.
            tokio::time::sleep(Duration::from_millis(500)).await;
            break;
        }
    }
    assert!(loaded, "the connection must reach Play before measuring spawning");
    let ticks = server.tick_stats().expect("integrated tick clock").tick_count;
    assert!(ticks > 0, "initial client and seed holds must clear; ticks={ticks}");
    let players = server.world_state().player_registry().perceptions(
        lodestone_server::dimension::Dimension::Overworld,
    );
    assert_eq!(players.len(), 1, "Play join must register stationary presence");
    server.shutdown().await;
    let seen = observed.spawned.lock().expect("spawn lock").clone();
    seen
}

struct DenyNaturalSpawns(Arc<AtomicUsize>);

impl Plugin for DenyNaturalSpawns {
    fn build(&self, app: &mut App) {
        app.insert_resource(NaturalDenyWitness(Arc::clone(&self.0)));
        app.add_systems(GameTick, deny_natural_spawns.in_set(TickSet::Adjudicate));
    }
}

#[derive(Resource)]
struct NaturalDenyWitness(Arc<AtomicUsize>);

fn deny_natural_spawns(
    mut proposals: MessageReader<ServerProposal>,
    mut decisions: ResMut<ServerProposalDecisions>,
    seen: Res<NaturalDenyWitness>,
) {
    for proposal in proposals.read() {
        if matches!(proposal.action, ServerProposalAction::NaturalSpawnMob { .. }) {
            seen.0.fetch_add(1, Ordering::SeqCst);
            decisions.decide(proposal.id(), 0, ProposalVerdict::Deny);
        }
    }
}

/// A stationary player on a lit plain must cause AddEntity encoding.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stationary_natural_spawning_reaches_add_entity_encoder() {
    let spawned = run(true, DEADLINE).await;
    let kinds: HashSet<&str> = spawned.iter().map(String::as_str).collect();
    // Printed rather than only asserted: the useful evidence from this gate is
    // *what* a plains world populates with, and a passing test that prints
    // nothing tells the next reader only that some number was non-zero.
    eprintln!("natural spawn requested {} AddEntity encodings: {kinds:?}", spawned.len());
    assert!(
        !spawned.is_empty(),
        "no AddEntity encoder invocation in {DEADLINE:?}: \
         stationary natural spawning did not reach connection publication"
    );
    // The plains creature list is what must have been consulted; a spawn of
    // something outside it would mean publication is carrying a fixture, not the
    // spawner's answer.
    let listed = plains_creature_list();
    for kind in &kinds {
        assert!(
            listed.iter().any(|s| s == kind),
            "{kind} reached the encoder but is not in the plains creature list {listed:?}"
        );
    }
}

/// The counter is a control: an empty encoder list alone could mean natural
/// spawning never ran. A non-zero count proves the primary tick staged real
/// candidates through `TickSet::Adjudicate` before the plugin denied them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_plugin_denial_keeps_observed_natural_spawns_out_of_the_encoder() {
    let seen = Arc::new(AtomicUsize::new(0));
    let server_app = ServerApp::bootstrap_with(|app| {
        app.add_plugins(DenyNaturalSpawns(Arc::clone(&seen)));
    });
    let spawned = run_with_server_app(true, Duration::from_secs(28), Some(server_app), Some(Arc::clone(&seen))).await;
    assert!(
        seen.load(Ordering::SeqCst) > 0,
        "control: the native plugin must observe at least one naturally planned action"
    );
    assert!(
        spawned.is_empty(),
        "denied natural candidates must not reach ADD_ENTITY: {spawned:?}"
    );
}

/// With `spawn_mobs` off, no entity may reach the encoder — otherwise the
/// gate above could be passing on an entity from some entirely different
/// producer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_mobs_off_requests_no_entity_encoding() {
    let spawned = run(false, Duration::from_secs(8)).await;
    assert!(
        spawned.is_empty(),
        "spawn_mobs is off, yet these spawned: {spawned:?}"
    );
}

/// The bundled plains creature list, read from the data rather than restated.
fn plains_creature_list() -> Vec<String> {
    let spawner = lodestone_server::natural_spawn::NaturalSpawner::new(
        lodestone_server::bundled_biome_spawners().clone(),
        0,
    );
    let mut listed: Vec<String> = Vec::new();
    for category in lodestone_server::MobCategory::SPAWNING {
        listed.extend(
            spawner
                .species_for("minecraft:plains", category)
                .into_iter()
                .map(str::to_owned),
        );
    }
    assert!(
        !listed.is_empty(),
        "the bundled plains document must name spawners"
    );
    listed
}
