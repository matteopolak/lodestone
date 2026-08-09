//! **Does natural mob spawning actually put a mob on a client's screen?**
//!
//! # Why this file exists when `natural_spawn.rs` already passes
//!
//! `tests/natural_spawn.rs` drives `MobSim::run_spawn_cycle` and
//! `NaturalSpawner` **directly**, over a hand-built `ChunkWorld`, and asserts on
//! `MobSim::iter`. Every one of its claims is about the engine. None of them is
//! about the engine being *reached*: the whole production chain between the tick
//! loop and a packet — `WorldStateHandle::spawn_mobs`, the non-empty player list
//! that `MobSim::set_players` only ever gets from an inbound movement packet,
//! `MobFeed`, `EntityStreamer::sync` and `ServerProtocol::encode_add_entity` —
//! is invisible to it. That is the island shape this repo keeps paying for: a
//! subsystem individually green and consuming nothing.
//!
//! So this gate starts a real [`IntegratedServer`] with a real tick loop, joins
//! a real connection through the duplex, moves the player once (the only thing
//! that registers a player with the sim at all), and counts
//! `encode_add_entity` calls **for entities the seed did not place**.
//!
//! # The fixture, and why it is hand-built
//!
//! Same reasoning as `tests/natural_spawn.rs`: the *surface* is stubbed, the
//! biome list and the light engine are real. `ChunkColumn::new` biomes every
//! quart as `minecraft:plains`, so `NaturalSpawner` consults the genuine bundled
//! plains spawn list. A grass surface under open sky is the cheapest input the
//! creature pass accepts.
//!
//! # What "did not come from the seed" means, and why it is load-bearing
//!
//! `open_in_memory_with_mobs`' seed task places `demo_mob_count(mob_count)`
//! mobs, and those stream through the same `encode_add_entity`. A gate counting
//! spawn packets would therefore pass with natural spawning entirely dead.
//! `mob_count` is **0** here for that reason, and the negative control below
//! turns the `spawn_mobs` game rule off through the same path and requires the
//! count to go to zero — so a spawn packet from any other producer (a dropped
//! item, a projectile, the player's own avatar) cannot read as a pass either.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::State;
use lodestone_net::Connection;
use lodestone_server::{
    ChunkColumn, ChunkSource, EntitySnapshot, IntegratedServer, ServerBound, ServerDirective,
    ServerProtocol,
};
use uuid::Uuid;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
/// Surface height: above sea level so the water spawn lists do not compete, and
/// inside the band the plains creature rules accept.
const FLOOR: i32 = 70;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
/// Our own id for the movement packet this file synthesises. The protocol double
/// decides what a packet id means, so this only has to avoid the four above.
const MOVE_PLAYER: i32 = 40;

/// Bounded, and long: the spawn cycle runs once per 50 ms tick and the cluster
/// loop is probabilistic, so this is a deadline the loop below polls against —
/// never a sleep whose expiry is itself the assertion.
const DEADLINE: Duration = Duration::from_secs(30);

/// Every `encode_add_entity` the server made, by type key.
#[derive(Debug, Default)]
struct Observed {
    spawned: Mutex<Vec<String>>,
    /// Set once the connection has reached Play, so the test can move the player
    /// only after there is a session to move.
    in_play: AtomicBool,
}

#[derive(Debug)]
struct WatchingProtocol(Arc<Observed>);

impl ServerProtocol for WatchingProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
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
            // The one packet that registers a player with `MobSim` — the natural
            // spawn cycle is skipped entirely while the player list is empty, so
            // without this arm the gate would measure nothing at all.
            State::Play if packet_id == MOVE_PLAYER => {
                // A one-byte nudge along x, so successive samples differ and the
                // server's own dirty checks do not collapse them.
                let step = f64::from(payload.first().copied().unwrap_or(0)) * 0.01;
                ServerBound::PlayerMoved {
                    x: 8.5 + step,
                    y: f64::from(FLOOR) + 1.0,
                    z: 8.5,
                    rotation: None,
                    on_ground: true,
                }
            }
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
    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
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

    /// The observation point: the packet that makes a mob exist for a client.
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
                for y in FLOOR - 4..FLOOR {
                    column.set_block(x, y, z, "minecraft:stone");
                }
                column.set_block(x, FLOOR, z, "minecraft:grass_block[snowy=false]");
            }
        }
        column
    }
}

impl ChunkSource for PlainsWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self.build()
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.build()
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// Runs a real server for up to [`DEADLINE`], moving the player every 100 ms, and
/// returns every entity type an `ADD_ENTITY` was encoded for.
///
/// `spawn_mobs` is set through the world's own game-rule path when
/// `spawn_mobs == false`, which is what makes the negative control travel the
/// same wire as the gate rather than being a different program.
async fn run(spawn_mobs: bool, deadline: Duration) -> Vec<String> {
    let observed = Arc::new(Observed::default());
    let (server, client) = IntegratedServer::open_in_memory_with_mobs(
        WatchingProtocol(Arc::clone(&observed)),
        PlainsWorld,
        // The same 7×7 shape the shell uses at its clamped mob radius.
        (-3..=3, -3..=3),
        (8, 8),
        // **Zero seeded mobs.** Every spawn packet observed below is therefore
        // one the natural spawner produced.
        0,
        3,
    );
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
    let mut nudge: u8 = 0;
    while start.elapsed() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if observed.in_play.load(Ordering::SeqCst) {
            nudge = nudge.wrapping_add(1);
            // A movement packet per poll: this is what keeps a player registered
            // with the sim, and it is also the cadence `serve_play` runs its
            // entity streaming pass on.
            let _ = client.write_packet(MOVE_PLAYER, &[nudge]).await;
        }
        if !observed.spawned.lock().expect("spawn lock").is_empty() {
            // Keep going a little past the first spawn so the report is not a
            // single sample, then stop — the assertion is on what was seen.
            tokio::time::sleep(Duration::from_millis(500)).await;
            break;
        }
    }
    server.shutdown().await;
    let seen = observed.spawned.lock().expect("spawn lock").clone();
    seen
}

/// **The gate.** A joined player standing on a lit plain must receive
/// `ADD_ENTITY` for at least one mob nothing seeded — i.e. the natural spawn
/// cycle reaches the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn natural_spawning_reaches_a_client_as_add_entity() {
    let spawned = run(true, DEADLINE).await;
    let kinds: HashSet<&str> = spawned.iter().map(String::as_str).collect();
    // Printed rather than only asserted: the useful evidence from this gate is
    // *what* a plains world populates with, and a passing test that prints
    // nothing tells the next reader only that some number was non-zero.
    eprintln!("natural spawn reached the wire with {} ADD_ENTITY: {kinds:?}", spawned.len());
    assert!(
        !spawned.is_empty(),
        "no entity spawn packet at all in {DEADLINE:?} with zero seeded mobs: the \
         natural spawn cycle is not reaching the wire, so a singleplayer world is \
         empty forever no matter what the engine's own tests say"
    );
    // The plains creature list is what must have been consulted; a spawn of
    // something outside it would mean the wire is carrying a fixture, not the
    // spawner's answer.
    let listed = plains_creature_list();
    for kind in &kinds {
        assert!(
            listed.iter().any(|s| s == kind),
            "{kind} reached the wire but is not in the plains creature list {listed:?}"
        );
    }
}

/// **The negative control, and it must observe zero.** With `spawn_mobs` off and
/// nothing seeded, no entity spawn packet may reach the client — otherwise the
/// gate above could be passing on a spawn packet from some entirely different
/// producer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_mobs_off_sends_no_entity_spawn_at_all() {
    let spawned = run(false, Duration::from_secs(8)).await;
    assert!(
        spawned.is_empty(),
        "spawn_mobs is off and nothing was seeded, yet these spawned: {spawned:?}"
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
