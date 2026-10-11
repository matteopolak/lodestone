//! **Does a raised shield stop a mob's melee hit on a real connection, and does an
//! axe take the shield away?**
//!
//! A real [`IntegratedServer`] joins one connection, gives the player a shield with
//! a creative slot write, switches to survival and raises it with a use-item
//! packet. A zombie or vindicator then spawns through the production roster and
//! the tick loop does the rest. Observations are the encode side of the protocol
//! double: set-health values, window-0 slot updates (the shield's durability
//! damage), item-cooldown packets and sounds.
//!
//! Expected values are the reference's arithmetic: a zombie hits for 3 and a
//! vindicator for 5 plus 8 from its iron axe; a blocked hit of at least 3 wears the shield by
//! `floor(1 + blocked)`, so 4 and 14; an axe disables the shield for 5 seconds,
//! which is 100 ticks. The player faces +z (yaw 0), so a mob at +z is in front.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::State;
use lodestone_data::block_states::StateId;
use lodestone_model::{ItemStack, ResourceKey, SoundCategory, Vec3};
use lodestone_net::Connection;
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const FLOOR: i32 = 70;
/// A roof so a daytime zombie cannot burn to death and be misread as a hit.
const CEILING: i32 = FLOOR + 4;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
const MOVE_PLAYER: i32 = 40;
const GIVE_SHIELD: i32 = 41;
const SURVIVAL: i32 = 43;
const CREATIVE: i32 = 44;
const RAISE: i32 = 45;

const HOTBAR_ZERO: i16 = 36;
const PLAYER_X: f64 = 8.5;
const PLAYER_Z: f64 = 8.5;

fn fixture_state(name: &str) -> StateId {
    StateId::from_state_str(name).expect("fixture block state exists")
}

#[derive(Debug, Default)]
struct Observed {
    in_play: AtomicBool,
    health: Mutex<Vec<f32>>,
    /// The shield slot's durability damage after each update.
    shield_damage: Mutex<Vec<Option<u32>>>,
    cooldowns: Mutex<Vec<(String, i32)>>,
    sounds: Mutex<Vec<String>>,
}

impl Observed {
    fn lowest_health(&self) -> f32 {
        self.health.lock().expect("health lock").iter().copied().fold(20.0, f32::min)
    }
}

#[derive(Debug)]
struct WatchingProtocol(Arc<Observed>);

impl ServerProtocol for WatchingProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake { next_state: State::Login },
            State::Login if packet_id == LOGIN_START => {
                ServerBound::LoginStart { username: "Shielded".to_string(), uuid: Uuid::nil() }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => ServerBound::ConfigurationFinished,
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
            State::Play if packet_id == GIVE_SHIELD => {
                let shield = ItemStack::new(ResourceKey::new("minecraft", "shield").expect("valid key"), 1);
                ServerBound::CreativeModeSlotSet { slot: HOTBAR_ZERO, item: Some(shield) }
            }
            State::Play if packet_id == SURVIVAL => {
                ServerBound::ChatCommand { command: "gamemode survival".to_string() }
            }
            State::Play if packet_id == CREATIVE => {
                ServerBound::ChatCommand { command: "gamemode creative".to_string() }
            }
            State::Play if packet_id == RAISE => {
                ServerBound::UseItem { hand: 0, sequence: None, yaw: 0.0, pitch: 0.0 }
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
    fn encode_set_health(&self, health: f32, _food: i32, _saturation: f32) -> ServerDirective {
        self.0.health.lock().expect("health lock").push(health);
        ServerDirective::None
    }
    fn encode_container_slot(
        &self,
        window_id: i32,
        _state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        if window_id == 0 && slot == i32::from(HOTBAR_ZERO) {
            self.0.shield_damage.lock().expect("slot lock").push(item.map(|stack| stack.components.damage.unwrap_or(0)));
        }
        ServerDirective::None
    }
    fn encode_item_cooldown(&self, group: &str, ticks: i32) -> ServerDirective {
        self.0.cooldowns.lock().expect("cooldown lock").push((group.to_owned(), ticks));
        ServerDirective::None
    }
    fn encode_sound(
        &self,
        sound: &str,
        _category: SoundCategory,
        _pos: Vec3,
        _volume: f32,
        _pitch: f32,
        _seed: i64,
    ) -> ServerDirective {
        self.0.sounds.lock().expect("sound lock").push(sound.to_owned());
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
                    column.set_block_id(x, y, z, fixture_state("minecraft:stone"));
                }
                column.set_block_id(x, FLOOR, z, fixture_state("minecraft:stone"));
                column.set_block_id(x, CEILING, z, fixture_state("minecraft:stone"));
            }
        }
        column
    }
}

impl ChunkSource for RoofedRoom {
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


struct Scene {
    observed: Arc<Observed>,
    server: IntegratedServer,
    client: Connection<tokio::io::DuplexStream>,
    nudge: u8,
}

impl Scene {
    /// Joins, gives the player a shield in survival and, with `raise`, holds it up.
    async fn join(raise: bool) -> Self {
        let observed = Arc::new(Observed::default());
        let (server, client) = IntegratedServer::open_in_memory_with_mobs(
            WatchingProtocol(Arc::clone(&observed)),
            RoofedRoom,
            (-2..=2, -2..=2),
            (8, 8),
            3,
        );
        server.world_state().set_rule("spawn_mobs", "false").expect("spawn_mobs is a known rule");
        let mut client = Connection::new(client);
        client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
        client.write_packet(LOGIN_START, &[0]).await.expect("login start");
        client.read_packet().await.unwrap().unwrap();
        client.write_packet(LOGIN_ACKNOWLEDGED, &[]).await.expect("login ack");
        client.write_packet(FINISH_CONFIGURATION, &[]).await.expect("finish configuration");
        let mut scene = Self { observed, server, client, nudge: 0 };
        let start = tokio::time::Instant::now();
        while !(scene.observed.in_play.load(Ordering::SeqCst) && scene.server.world_state().initial_seed_landed()) {
            assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for the join");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        scene.run_for(300).await;
        scene.client.write_packet(CREATIVE, &[]).await.expect("creative");
        scene.run_for(300).await;
        scene.client.write_packet(GIVE_SHIELD, &[]).await.expect("give");
        scene.run_for(300).await;
        scene.client.write_packet(SURVIVAL, &[]).await.expect("survival");
        scene.run_for(300).await;
        if raise {
            scene.client.write_packet(RAISE, &[]).await.expect("raise");
            // Past the quarter-second warm-up, counted in world ticks: the clock
            // is the server's, not the wall's.
            let raised_at = scene.server.world_state().time().game_time;
            let start = tokio::time::Instant::now();
            while scene.server.world_state().time().game_time < raised_at + 10 {
                assert!(start.elapsed() < Duration::from_secs(30), "the world clock never advanced");
                scene.run_for(100).await;
            }
        }
        scene
    }

    /// Keeps the player registered with the sim for `millis`.
    async fn run_for(&mut self, millis: u64) {
        let end = tokio::time::Instant::now() + Duration::from_millis(millis);
        while tokio::time::Instant::now() < end {
            self.nudge = self.nudge.wrapping_add(1);
            let _ = self.client.write_packet(MOVE_PLAYER, &[self.nudge]).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn spawn(&self, species: &str, z: f64) {
        let mobs = self.server.mobs().expect("the server runs a mob sim");
        mobs.with(|sim| {
            sim.spawn_species(
                ResourceKey::new("minecraft", species).expect("valid key"),
                Vec3::new(PLAYER_X, f64::from(FLOOR) + 1.0, z),
            );
        });
    }

    fn worn(&self) -> Vec<u32> {
        self.observed.shield_damage.lock().expect("slot lock").iter().flatten().copied().filter(|d| *d > 0).collect()
    }
}

/// The zombie in front is stopped: no health is lost, the shield wears by 4 per
/// hit, and the block sound plays.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_raised_shield_stops_a_zombie_in_front() {
    let mut scene = Scene::join(true).await;
    scene.spawn("zombie", PLAYER_Z + 1.0);
    scene.run_for(6000).await;
    let worn = scene.worn();
    assert!(!worn.is_empty(), "the zombie never struck, so nothing was tested");
    assert!(worn.iter().all(|d| d % 4 == 0), "each blocked 3-damage hit wears floor(1 + 3) = 4: {worn:?}");
    assert_eq!(scene.observed.lowest_health(), 20.0, "a blocked hit costs no health");
    assert!(scene.observed.sounds.lock().expect("sound lock").iter().any(|s| s == "minecraft:item.shield.block"));
    scene.server.shutdown().await;
}

/// The same zombie behind the player is outside the 90 degree arc and lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_raised_shield_does_not_cover_a_zombie_behind() {
    let mut scene = Scene::join(true).await;
    scene.spawn("zombie", PLAYER_Z - 1.0);
    scene.run_for(6000).await;
    assert!(scene.observed.lowest_health() < 20.0, "a hit from behind lands");
    assert!(scene.worn().is_empty(), "and wears nothing");
    scene.server.shutdown().await;
}

/// A shield that was never raised blocks nothing, which is the control for the
/// first test: the zombie in front does hurt a player who is not blocking.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shield_that_is_not_raised_blocks_nothing() {
    let mut scene = Scene::join(false).await;
    scene.spawn("zombie", PLAYER_Z + 1.0);
    scene.run_for(6000).await;
    assert!(scene.observed.lowest_health() < 20.0);
    assert!(scene.worn().is_empty());
    scene.server.shutdown().await;
}

/// A vindicator's axe takes the shield down for 5 seconds (100 ticks): the first
/// hit is blocked (5 + 8 for the iron axe = 13 damage wears 14), the cooldown reaches the client, the break
/// sound plays, and the next hit lands on a lowered shield.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_axe_disables_the_shield_for_five_seconds() {
    let mut scene = Scene::join(true).await;
    scene.spawn("vindicator", PLAYER_Z + 1.0);
    scene.run_for(8000).await;
    assert!(scene.worn().contains(&14), "the first hit (5 base + 8 for the iron axe = 13) is blocked and wears the shield by 14: {:?}", scene.worn());
    assert!(scene.observed.cooldowns.lock().expect("cooldown lock").contains(&("minecraft:shield".to_owned(), 100)));
    assert!(scene.observed.sounds.lock().expect("sound lock").iter().any(|s| s == "minecraft:item.shield.break"));
    assert!(scene.observed.lowest_health() < 20.0, "once disabled the shield no longer blocks");
    scene.server.shutdown().await;
}
