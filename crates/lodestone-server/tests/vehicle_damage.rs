//! **Do hits on a boat or minecart rock it on the wire, and does enough damage
//! break it into its drop?**
//!
//! Every test starts a real [`IntegratedServer`], joins one connection through
//! the protocol double and drives attack packets (or, for blasts and
//! projectiles, the sim a tick loop drives). Observations are the encode side
//! of the double: the entity-data hurt triple, the remove-entity ids, the item
//! entities that appear and the passenger lists.
//!
//! The expected numbers are the reference's arithmetic, not the code's: a hit
//! adds `damage x 10` to the rock amplitude and the hull breaks only when the
//! total is **above** 40, so a wooden sword (4 damage, exactly 40) rocks the
//! hull and a diamond sword (7 damage, 70) breaks it. An arrow's damage is
//! `ceil(speed x 2)`, so speed 2 deals 4 (rocks) and speed 3 deals 6 (breaks).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::State;
use lodestone_data::block_states::StateId;
use lodestone_entity::projectile::Projectile;
use lodestone_model::{ItemStack, ResourceKey, Vec3};
use lodestone_net::Connection;
use lodestone_server::{
    ChunkColumn, ChunkSource, EntitySnapshot, IntegratedServer, MetadataField, MinecartKind, ServerBound,
    ServerDirective, ServerProtocol,
};
use uuid::Uuid;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const FLOOR: i32 = 70;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
/// Synthetic play packet ids the protocol double decodes.
const MOVE_PLAYER: i32 = 40;
const GIVE: i32 = 41;
const INTERACT: i32 = 42;
const SURVIVAL: i32 = 43;
const CREATIVE: i32 = 44;
const ATTACK: i32 = 46;

const HOTBAR_ZERO: i16 = 36;
const DEADLINE: Duration = Duration::from_secs(30);
const LOCAL_PLAYER: i32 = 1;

/// Payload byte of [`GIVE`] naming the weapon.
const WOODEN_SWORD: u8 = 0;
const DIAMOND_SWORD: u8 = 1;

fn fixture_state(name: &str) -> StateId {
    StateId::from_state_str(name).expect("fixture block state exists")
}

#[derive(Debug, Default)]
struct Observed {
    in_play: AtomicBool,
    added: Mutex<Vec<EntitySnapshot>>,
    removed: Mutex<Vec<i32>>,
    /// `(entity id, time, dir, damage)` from every streamed hurt triple.
    hurts: Mutex<Vec<(i32, i32, i32, f32)>>,
    passengers: Mutex<Vec<(i32, Vec<i32>)>>,
}

impl Observed {
    fn items_added(&self, item: &str) -> usize {
        self.added
            .lock()
            .expect("added lock")
            .iter()
            .filter(|e| {
                e.entity_type.to_string() == "minecraft:item"
                    && e.metadata
                        .iter()
                        .any(|m| matches!(m, MetadataField::Item { item: held, .. } if held.to_string() == item))
            })
            .count()
    }

    fn was_removed(&self, id: i32) -> bool {
        self.removed.lock().expect("removed lock").contains(&id)
    }

    fn hurts_of(&self, id: i32) -> Vec<(i32, i32, f32)> {
        self.hurts
            .lock()
            .expect("hurts lock")
            .iter()
            .filter(|h| h.0 == id)
            .map(|h| (h.1, h.2, h.3))
            .collect()
    }

    fn note_hurt(&self, id: i32, fields: &[MetadataField]) {
        for field in fields {
            if let MetadataField::VehicleHurt { time, dir, damage } = field {
                self.hurts.lock().expect("hurts lock").push((id, *time, *dir, *damage));
            }
        }
    }
}

#[derive(Debug)]
struct WatchingProtocol(Arc<Observed>);

impl ServerProtocol for WatchingProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        let entity = || i32::from_be_bytes(payload[..4].try_into().expect("entity id"));
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake { next_state: State::Login },
            State::Login if packet_id == LOGIN_START => {
                ServerBound::LoginStart { username: "Wrecker".to_string(), uuid: Uuid::nil() }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => ServerBound::ConfigurationFinished,
            State::Play if packet_id == MOVE_PLAYER => {
                ServerBound::PlayerMoved { x: 8.5, y: f64::from(FLOOR) + 1.0, z: 8.5, rotation: None, on_ground: true }
            }
            State::Play if packet_id == GIVE => {
                let name = if payload[0] == DIAMOND_SWORD { "diamond_sword" } else { "wooden_sword" };
                let sword = ItemStack::new(ResourceKey::new("minecraft", name).expect("valid key"), 1);
                ServerBound::CreativeModeSlotSet { slot: HOTBAR_ZERO, item: Some(sword) }
            }
            State::Play if packet_id == INTERACT => {
                ServerBound::InteractEntity { entity_id: entity(), hand: 0, using_secondary_action: false }
            }
            State::Play if packet_id == ATTACK => ServerBound::Attack { entity_id: entity() },
            State::Play if packet_id == SURVIVAL => {
                ServerBound::ChatCommand { command: "gamemode survival".to_string() }
            }
            State::Play if packet_id == CREATIVE => {
                ServerBound::ChatCommand { command: "gamemode creative".to_string() }
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
    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        self.0.note_hurt(entity.id, &entity.metadata);
        self.0.added.lock().expect("added lock").push(entity.clone());
        ServerDirective::None
    }
    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        self.0.removed.lock().expect("removed lock").extend_from_slice(ids);
        ServerDirective::None
    }
    fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
        self.0.note_hurt(entity_id, fields);
        ServerDirective::None
    }
    fn encode_set_passengers(&self, vehicle_id: i32, passenger_ids: &[i32]) -> ServerDirective {
        self.0.passengers.lock().expect("passengers lock").push((vehicle_id, passenger_ids.to_vec()));
        ServerDirective::None
    }
}

/// A stone floor at [`FLOOR`] with stone under it, air above.
#[derive(Debug)]
struct Floor;

impl Floor {
    fn build(&self) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                for y in FLOOR - 4..=FLOOR {
                    column.set_block_id(x, y, z, fixture_state("minecraft:stone"));
                }
            }
        }
        column
    }
}

impl ChunkSource for Floor {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self.build()
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        self.build().block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.build().biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16)).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let start = tokio::time::Instant::now();
    while !done() {
        assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

struct Scene {
    observed: Arc<Observed>,
    server: IntegratedServer,
    client: Connection<tokio::io::DuplexStream>,
}

impl Scene {
    /// Joins one player standing on the floor, in creative.
    async fn join() -> Self {
        let observed = Arc::new(Observed::default());
        let (server, client) = IntegratedServer::open_in_memory_with_mobs(
            WatchingProtocol(Arc::clone(&observed)),
            Floor,
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
        until("the join", || observed.in_play.load(Ordering::SeqCst) && server.world_state().initial_seed_landed())
            .await;
        // Registers the player with the sim; the first write may precede the seed.
        client.write_packet(MOVE_PLAYER, &[]).await.expect("move");
        client.write_packet(CREATIVE, &[]).await.expect("creative");
        settle().await;
        Self { observed, server, client }
    }

    async fn hold(&mut self, weapon: u8) {
        self.client.write_packet(GIVE, &[weapon]).await.expect("give");
        settle().await;
    }

    async fn survival(&mut self) {
        self.client.write_packet(SURVIVAL, &[]).await.expect("survival");
        settle().await;
    }

    async fn creative(&mut self) {
        self.client.write_packet(CREATIVE, &[]).await.expect("creative");
        settle().await;
    }

    async fn attack(&mut self, id: i32) {
        self.client.write_packet(ATTACK, &id.to_be_bytes()).await.expect("attack");
    }

    fn spawn_boat(&self, x: f64, kind: &str) -> i32 {
        let mobs = self.server.mobs().expect("the server runs a mob sim");
        mobs.with(|sim| {
            sim.spawn_vehicle(ResourceKey::new("minecraft", kind).expect("valid key"), Vec3::new(x, 71.0, 8.5), 0.0)
        })
    }

    fn spawn_cart(&self, x: f64, kind: MinecartKind) -> i32 {
        let mobs = self.server.mobs().expect("the server runs a mob sim");
        mobs.with(|sim| sim.spawn_minecart(kind, Vec3::new(x, 71.0, 8.5)))
    }

    async fn streamed(&self, id: i32) {
        until("the entity to stream", || {
            self.observed.added.lock().expect("added lock").iter().any(|e| e.id == id)
        })
        .await;
    }

    fn is_tracked(&self, id: i32) -> bool {
        self.server.mobs().expect("mobs").with(|sim| sim.is_vehicle(id))
    }
}

/// A wooden sword's 40 is not above 40, so the first hit rocks and the second
/// breaks; the rider is lifted off and the boat drops as itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_boat_rocks_on_the_threshold_then_breaks_into_its_item_and_dismounts_its_rider() {
    let mut scene = Scene::join().await;
    scene.hold(WOODEN_SWORD).await;
    scene.survival().await;
    let boat = scene.spawn_boat(12.5, "oak_boat");
    scene.streamed(boat).await;
    scene.client.write_packet(INTERACT, &boat.to_be_bytes()).await.expect("board");
    until("the rider", || {
        scene.observed.passengers.lock().expect("passengers lock").contains(&(boat, vec![LOCAL_PLAYER]))
    })
    .await;
    assert_eq!(scene.observed.hurts_of(boat).first(), Some(&(0, 1, 0.0)), "the resting triple is (0, 1, 0)");

    scene.attack(boat).await;
    until("the rock", || scene.observed.hurts_of(boat).iter().any(|h| h.1 == -1)).await;
    let rock = scene.observed.hurts_of(boat).into_iter().find(|h| h.1 == -1).expect("rock");
    // 4 damage x 10 = 40, minus at most one decay tick before it streamed.
    assert!((8..=10).contains(&rock.0), "hurt time {} counts down from 10", rock.0);
    assert!((38.0..=40.0).contains(&rock.2), "damage amplitude {} is 4 x 10", rock.2);
    settle().await;
    assert!(scene.is_tracked(boat), "exactly 40 does not break the hull");
    assert!(!scene.observed.was_removed(boat));
    assert_eq!(scene.observed.items_added("minecraft:oak_boat"), 0);

    scene.attack(boat).await;
    until("the removal", || scene.observed.was_removed(boat)).await;
    assert!(!scene.is_tracked(boat));
    until("the drop", || scene.observed.items_added("minecraft:oak_boat") == 1).await;
    let mobs = scene.server.mobs().expect("mobs");
    assert_eq!(mobs.with(|sim| sim.vehicle_of(LOCAL_PLAYER)), None, "the rider is dismounted");
    scene.server.shutdown().await;
}

/// A creative hit removes the hull outright and drops nothing; the survival
/// break of a second boat in the same server is the control proving the drop
/// detector sees a drop when there is one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_creative_hit_removes_a_boat_without_a_drop() {
    let mut scene = Scene::join().await;
    let creative_boat = scene.spawn_boat(12.5, "bamboo_raft");
    scene.streamed(creative_boat).await;
    scene.attack(creative_boat).await;
    until("the removal", || scene.observed.was_removed(creative_boat)).await;
    settle().await;
    assert_eq!(scene.observed.items_added("minecraft:bamboo_raft"), 0, "a creative break drops nothing");

    scene.hold(DIAMOND_SWORD).await;
    scene.survival().await;
    let survival_boat = scene.spawn_boat(13.5, "bamboo_raft");
    scene.streamed(survival_boat).await;
    scene.attack(survival_boat).await;
    until("the control drop", || scene.observed.items_added("minecraft:bamboo_raft") == 1).await;
    scene.server.shutdown().await;
}

/// Minecarts take the same model: they rock on the wire, and a break drops the
/// item the reference names: a command-block cart drops a plain minecart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn minecarts_rock_and_break_into_their_reference_drops() {
    let mut scene = Scene::join().await;
    scene.hold(WOODEN_SWORD).await;
    scene.survival().await;
    let rocked = scene.spawn_cart(12.5, MinecartKind::Plain);
    scene.streamed(rocked).await;
    scene.attack(rocked).await;
    until("the minecart rock", || scene.observed.hurts_of(rocked).iter().any(|h| h.1 == -1 && h.2 >= 38.0)).await;
    assert!(scene.is_tracked(rocked), "40 rocks a minecart without breaking it");

    scene.creative().await;
    scene.hold(DIAMOND_SWORD).await;
    scene.survival().await;
    let command = scene.spawn_cart(13.5, MinecartKind::CommandBlock);
    let chest = scene.spawn_cart(14.5, MinecartKind::Chest);
    scene.streamed(command).await;
    scene.streamed(chest).await;
    scene.attack(command).await;
    scene.attack(chest).await;
    until("both removals", || scene.observed.was_removed(command) && scene.observed.was_removed(chest)).await;
    until("the drops", || {
        scene.observed.items_added("minecraft:minecart") == 1
            && scene.observed.items_added("minecraft:chest_minecart") == 1
    })
    .await;
    assert_eq!(scene.observed.items_added("minecraft:command_block_minecart"), 0);
    scene.server.shutdown().await;
}

/// An arrow goes through the same entry: speed 2 deals 4 and rocks, speed 3 deals
/// 6 and breaks. A snowball deals nothing yet still rocks the hull.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn projectiles_hurt_boats_through_the_vehicle_model() {
    let scene = Scene::join().await;
    let mobs = scene.server.mobs().expect("mobs");
    let shoot = |kind: &str, speed: f64, x: f64| {
        mobs.with(|sim| {
            sim.spawn_projectile(
                ResourceKey::new("minecraft", kind).expect("valid key"),
                Projectile::arrow(Vec3::new(x, 71.2, 10.5), Vec3::new(0.0, 0.0, -speed)),
            )
        })
    };
    let slow = scene.spawn_boat(11.5, "oak_boat");
    let fast = scene.spawn_boat(13.5, "oak_boat");
    scene.streamed(slow).await;
    scene.streamed(fast).await;
    shoot("arrow", 2.0, 11.5);
    until("the slow arrow's rock", || scene.observed.hurts_of(slow).iter().any(|h| h.1 == -1)).await;
    assert!(scene.observed.hurts_of(slow).iter().any(|h| h.2 >= 38.0), "4 damage is a 40 amplitude");
    settle().await;
    assert!(scene.is_tracked(slow), "40 does not break the hull");
    shoot("arrow", 3.0, 13.5);
    until("the fast arrow's break", || scene.observed.was_removed(fast)).await;
    until("its drop", || scene.observed.items_added("minecraft:oak_boat") == 1).await;
    assert!(scene.is_tracked(slow), "the slow boat is untouched by the other arrow");
    scene.server.shutdown().await;
}

/// A blast breaks a boat in reach and leaves one beyond twice its radius alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_explosion_breaks_a_boat_in_reach_and_spares_one_out_of_it() {
    let scene = Scene::join().await;
    let near = scene.spawn_boat(12.5, "birch_boat");
    let far = scene.spawn_boat(15.5, "birch_boat");
    scene.streamed(near).await;
    scene.streamed(far).await;
    let mobs = scene.server.mobs().expect("mobs");
    // Power 1 reaches 2 blocks: the boat 1 block away takes it, the one 4 away cannot.
    mobs.with(|sim| sim.explode(Vec3::new(11.5, 71.0, 8.5), 1.0, lodestone_entity::DamageFlags::default()));
    until("the near boat breaks", || scene.observed.was_removed(near)).await;
    until("its drop", || scene.observed.items_added("minecraft:birch_boat") == 1).await;
    settle().await;
    assert!(scene.is_tracked(far));
    assert!(scene.observed.hurts_of(far).iter().all(|h| h.1 == 1), "the far boat never rocked");
    scene.server.shutdown().await;
}
