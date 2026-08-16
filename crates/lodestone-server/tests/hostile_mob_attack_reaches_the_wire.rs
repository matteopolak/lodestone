//! **Does a hostile mob's melee attack actually reach a real player's health
//! bar, through the production tick loop?**
//!
//! `tests/mob_melee_damages_player.rs` (issue #625's own acceptance gate)
//! proves the chain goal → attack → position → player identity is wired —
//! but it drives `MobSim::tick_with_terrain` and reads
//! `MobSim::take_player_hits` **directly**, in a loop the test itself owns.
//! It never proves `crate::tick::run_tick_loop_with_weather` (the loop a real
//! world actually runs) reaches that same call, nor that the per-connection
//! drain at `server.rs`'s `!invulnerable` arm (`for hit in
//! mobs.with(|sim| sim.take_player_hits())`) is ever reached with a real
//! connection in Play. A hermetic harness proving the engine works is not
//! the same claim as the wire carrying it — this repo's own dominant defect
//! shape (`DESIGN.md` §12's "island").
//!
//! So this starts a real [`IntegratedServer`], joins a real connection,
//! spawns a zombie one block from the player through the real
//! [`MobSim::spawn_species`] roster (the same call `mob_melee_damages_player.rs`
//! uses, just reached through the live handle instead of a test-owned
//! `MobSim`), and watches for `encode_set_health` reporting less than the
//! full 20.0 starting health over the real tick loop.
//!
//! A solid ceiling over both positions keeps the zombie out of daylight, so a
//! burn death cannot be mistaken for the melee hit this file exists to prove.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::State;
use lodestone_net::Connection;
use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol};
use uuid::Uuid;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const FLOOR: i32 = 70;
/// A roof a few blocks above the floor so neither the player nor the zombie
/// ever has sky access — a daytime zombie must not catch fire and die of
/// burn damage, which this gate would otherwise misread as a melee hit.
const CEILING: i32 = FLOOR + 4;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
/// Our own id for the movement packet this file synthesises, matching
/// `tests/natural_spawn_reaches_the_wire.rs`'s reasoning: the protocol double
/// picks any id past the four real login/config ones above.
const MOVE_PLAYER: i32 = 40;

/// Bounded and generous — `NearestAttackableTargetGoal`'s own random search
/// throttle plus `MeleeAttackGoal`'s swing cooldown need real ticks, not a
/// single poll.
const DEADLINE: Duration = Duration::from_secs(30);

const PLAYER_X: f64 = 8.5;
const PLAYER_Z: f64 = 8.5;
/// One block over — inside `MeleeAttackGoal` reach the instant a target is
/// acquired, so this gate is not also (re-)proving pathfinding closes a gap;
/// `mob_melee_damages_player.rs`'s own positive case already covers that.
const ZOMBIE_X: f64 = 9.5;
const ZOMBIE_Z: f64 = 8.5;

#[derive(Debug, Default)]
struct Observed {
    in_play: AtomicBool,
    /// Every `encode_set_health` value seen, in order.
    health: Mutex<Vec<f32>>,
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
                username: "AttackWatch".to_string(),
                uuid: Uuid::nil(),
            },
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            // The one packet that registers a player with `MobSim` — see
            // `server.rs`'s `set_players` call site, fed only from an inbound
            // movement packet.
            State::Play if packet_id == MOVE_PLAYER => {
                let step = f64::from(payload.first().copied().unwrap_or(0)) * 0.001;
                ServerBound::PlayerMoved {
                    x: PLAYER_X + step,
                    y: f64::from(FLOOR) + 1.0,
                    z: PLAYER_Z,
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

    fn encode_set_health(&self, health: f32, _food: i32, _saturation: f32) -> ServerDirective {
        self.0.health.lock().expect("health lock").push(health);
        ServerDirective::None
    }
}

/// A solid floor at [`FLOOR`] plus a roof at [`CEILING`] — the "closed room"
/// shape, repeated for every chunk the same way
/// `natural_spawn_reaches_the_wire.rs`'s `PlainsWorld` does.
#[derive(Debug)]
struct RoofedRoom;

impl RoofedRoom {
    fn build(&self) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                for y in FLOOR - 4..FLOOR {
                    column.set_block(x, y, z, "minecraft:stone");
                }
                column.set_block(x, FLOOR, z, "minecraft:stone");
                column.set_block(x, CEILING, z, "minecraft:stone");
            }
        }
        column
    }
}

impl ChunkSource for RoofedRoom {
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

/// **The gate.** A zombie spawned one block from a joined, registered player
/// must land a melee hit within [`DEADLINE`] real ticks of the production
/// loop, observed as an `encode_set_health` below the full 20.0.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zombie_next_to_a_real_player_damages_them_through_the_production_loop() {
    let observed = Arc::new(Observed::default());
    let (server, client) = IntegratedServer::open_in_memory_with_mobs(
        WatchingProtocol(Arc::clone(&observed)),
        RoofedRoom,
        (-2..=2, -2..=2),
        (8, 8),
        0,
        3,
    );
    // No natural spawns competing for the health-drop signal.
    server
        .world_state()
        .set_rule("spawn_mobs", "false")
        .expect("spawn_mobs is a known rule");

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
    let mut spawned_zombie = false;
    let mut nudge: u8 = 0;
    while start.elapsed() < DEADLINE {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !observed.in_play.load(Ordering::SeqCst) {
            continue;
        }
        nudge = nudge.wrapping_add(1);
        // Keeps the player registered with the sim, and is also what drains
        // `take_player_hits` on the connection side (`server.rs`'s per-tick
        // player-position arm).
        let _ = client.write_packet(MOVE_PLAYER, &[nudge]).await;

        if !spawned_zombie {
            // Issue #303's documented race: the mob-seeding task replaces the
            // whole sim once its own terrain snapshot is ready, discarding
            // anything spawned before that. Poll `next_id` past its `1000`
            // floor first, exactly as `IntegratedServer::mobs`'s own doc
            // comment prescribes.
            if let Some(mobs) = server.mobs() {
                let ready = mobs.with(|sim| sim.next_id()) >= 1000;
                if ready {
                    mobs.with(|sim| {
                        sim.spawn_species(
                            ResourceKey::new("minecraft", "zombie").expect("valid key"),
                            Vec3::new(ZOMBIE_X, f64::from(FLOOR) + 1.0, ZOMBIE_Z),
                        );
                    });
                    spawned_zombie = true;
                }
            }
        }

        let lowest = observed
            .health
            .lock()
            .expect("health lock")
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min);
        if lowest < 20.0 {
            break;
        }
    }
    server.shutdown().await;

    let samples = observed.health.lock().expect("health lock").clone();
    let lowest = samples.iter().copied().fold(f32::INFINITY, f32::min);
    eprintln!("hostile_mob_attack_reaches_the_wire: health samples = {samples:?}");
    assert!(
        spawned_zombie,
        "the mob-seeding race never cleared in {DEADLINE:?} — next_id never reached 1000, \
         so this gate never got to spawn its zombie at all"
    );
    assert!(
        lowest < 20.0,
        "a zombie spawned one block from a real, joined, registered player must land a \
         melee hit within {DEADLINE:?} of the production tick loop — health samples were \
         {samples:?} (empty means encode_set_health was never even called)"
    );
}
