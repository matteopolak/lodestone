//! **When a hoe or shovel is in the main hand and a shield in the off hand, which
//! one gets the click, and does it depend on the release being played?**
//!
//! Drives a real [`IntegratedServer`] over an in-memory connection: the player is
//! handed a tool in hotbar slot 0 and a shield in the off hand with creative slot
//! writes, switched to survival, and sends use-item-on packets at a dirt, grass or
//! rooted-dirt floor. The observations are the encode side of the protocol double:
//! block updates, the tool's slot, sounds, and the item entity a tilled rooted
//! dirt pops out.
//!
//! The expected rows come from reading the two releases' tool code: 26.2 deferred
//! to an off-hand shield for axes only, so a hoe or shovel transformed the block
//! regardless; 26.3 defers for every tool with a block transformer unless the
//! player is sneaking. A tilled block becomes farmland, a flattened one a path,
//! and each use wears the tool by one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::State;
use lodestone_data::{GameDataVersion, block_states::StateId};
use lodestone_model::{BlockFace, BlockPos, ItemStack, PredictionSequence, ResourceKey, SoundCategory, Vec3, Vec3f};
use lodestone_net::Connection;
use lodestone_server::{
    ChunkColumn, ChunkSource, EntitySnapshot, IntegratedServer, MetadataField, ServerBound, ServerDirective,
    ServerProtocol,
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
const MOVE_PLAYER: i32 = 40;
const GIVE_TOOL: i32 = 41;
const GIVE_SHIELD: i32 = 42;
const SURVIVAL: i32 = 43;
const CREATIVE: i32 = 44;
const CLICK: i32 = 45;
const SNEAK: i32 = 46;

const HOTBAR_ZERO: i32 = 36;
const OFFHAND_MENU_SLOT: i16 = 45;
const DEADLINE: Duration = Duration::from_secs(30);
const PLAYER_X: f64 = 8.5;
const PLAYER_Z: f64 = 8.5;
/// The clicked cell, away from the player's own.
const TARGET: BlockPos = BlockPos { x: 12, y: FLOOR, z: 8 };

fn state(name: &str) -> StateId {
    StateId::from_state_str(name).expect("fixture block state exists")
}

#[derive(Debug, Default)]
struct Observed {
    in_play: AtomicBool,
    updates: Mutex<Vec<(BlockPos, StateId)>>,
    tool_slot: Mutex<Vec<Option<u32>>>,
    sounds: Mutex<Vec<String>>,
    items: Mutex<Vec<String>>,
    acked: Mutex<Vec<i32>>,
}

#[derive(Debug)]
struct WatchingProtocol {
    seen: Arc<Observed>,
    release: GameDataVersion,
}

impl ServerProtocol for WatchingProtocol {
    fn decode(&self, st: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match st {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake { next_state: State::Login },
            State::Login if packet_id == LOGIN_START => {
                ServerBound::LoginStart { username: "Tiller".to_string(), uuid: Uuid::nil() }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => ServerBound::ConfigurationFinished,
            State::Play if packet_id == MOVE_PLAYER => ServerBound::PlayerMoved {
                x: PLAYER_X,
                y: f64::from(FLOOR) + 1.0,
                z: PLAYER_Z,
                rotation: None,
                on_ground: true,
            },
            State::Play if packet_id == GIVE_TOOL => {
                let name = String::from_utf8(payload.to_vec()).expect("tool name");
                let tool = ItemStack::new(ResourceKey::new("minecraft", &name).expect("valid key"), 1);
                ServerBound::CreativeModeSlotSet { slot: HOTBAR_ZERO as i16, item: Some(tool) }
            }
            State::Play if packet_id == GIVE_SHIELD => {
                let shield = ItemStack::new(ResourceKey::new("minecraft", "shield").expect("valid key"), 1);
                ServerBound::CreativeModeSlotSet { slot: OFFHAND_MENU_SLOT, item: Some(shield) }
            }
            State::Play if packet_id == SURVIVAL => ServerBound::ChatCommand { command: "gamemode survival".to_string() },
            State::Play if packet_id == CREATIVE => ServerBound::ChatCommand { command: "gamemode creative".to_string() },
            State::Play if packet_id == SNEAK => {
                ServerBound::PlayerInput { sprint: false, shift: payload[0] == 1, jump: false }
            }
            State::Play if packet_id == CLICK => ServerBound::UseItemOn {
                pos: TARGET,
                face: BlockFace::Up,
                cursor: Vec3f { x: 0.5, y: 1.0, z: 0.5 },
                sequence: i32::from(payload[0]),
                hand: 0,
            },
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
        self.seen.in_play.store(true, Ordering::SeqCst);
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
    fn game_data_version(&self) -> GameDataVersion {
        self.release
    }
    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: StateId) -> ServerDirective {
        self.seen.updates.lock().expect("updates lock").push((BlockPos::new(x, y, z), state));
        ServerDirective::None
    }
    fn encode_block_changed_ack(&self, sequence: PredictionSequence) -> ServerDirective {
        self.seen.acked.lock().expect("ack lock").push(sequence.as_wire());
        ServerDirective::None
    }
    fn encode_container_slot(&self, window_id: i32, _state_id: i32, slot: i32, item: Option<&ItemStack>) -> ServerDirective {
        if window_id == 0 && slot == HOTBAR_ZERO {
            self.seen.tool_slot.lock().expect("slot lock").push(item.map(|s| s.components.damage.unwrap_or(0)));
        }
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
        self.seen.sounds.lock().expect("sound lock").push(sound.to_owned());
        ServerDirective::None
    }
    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        for field in &entity.metadata {
            if let MetadataField::Item { item, .. } = field {
                self.seen.items.lock().expect("items lock").push(item.to_string());
            }
        }
        ServerDirective::None
    }
}

/// A floor of `ground` at [`FLOOR`] over stone, air above.
#[derive(Debug)]
struct Ground(&'static str);

impl Ground {
    fn build(&self) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                for y in FLOOR - 4..FLOOR {
                    column.set_block_id(x, y, z, state("minecraft:stone"));
                }
                column.set_block_id(x, FLOOR, z, state(self.0));
            }
        }
        column
    }
}

impl ChunkSource for Ground {
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

/// One scenario: what the player holds, what is under the click, the release, and
/// whether the player sneaks.
struct Scenario {
    release: GameDataVersion,
    tool: &'static str,
    ground: &'static str,
    shield: bool,
    sneaking: bool,
    /// Whether the click should pop an item, which streams a tick or two later.
    pops: bool,
}

/// Plays `scenario` and returns what the connection saw once the click's
/// prediction acknowledgement has arrived, so an absent change means the server
/// finished the click and chose to leave the block alone.
async fn click(scenario: Scenario) -> Arc<Observed> {
    let seen = Arc::new(Observed::default());
    let (server, client) = IntegratedServer::open_in_memory_with_mobs(
        WatchingProtocol { seen: Arc::clone(&seen), release: scenario.release },
        Ground(scenario.ground),
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
    until("the join", || seen.in_play.load(Ordering::SeqCst) && server.world_state().initial_seed_landed()).await;
    client.write_packet(MOVE_PLAYER, &[]).await.expect("move");
    client.write_packet(CREATIVE, &[]).await.expect("creative");
    tokio::time::sleep(Duration::from_millis(300)).await;
    client.write_packet(GIVE_TOOL, scenario.tool.as_bytes()).await.expect("give tool");
    if scenario.shield {
        client.write_packet(GIVE_SHIELD, &[]).await.expect("give shield");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    client.write_packet(SURVIVAL, &[]).await.expect("survival");
    tokio::time::sleep(Duration::from_millis(300)).await;
    if scenario.sneaking {
        client.write_packet(SNEAK, &[1]).await.expect("sneak");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    client.write_packet(CLICK, &[7]).await.expect("click");
    until("the click's acknowledgement", || seen.acked.lock().expect("ack lock").contains(&7)).await;
    if scenario.pops {
        until("the popped item", || !seen.items.lock().expect("items lock").is_empty()).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    server.shutdown().await;
    seen
}

fn updates_at_target(seen: &Observed) -> Vec<StateId> {
    seen.updates.lock().expect("updates lock").iter().filter(|(pos, _)| *pos == TARGET).map(|(_, s)| *s).collect()
}

fn worn(seen: &Observed) -> bool {
    seen.tool_slot.lock().expect("slot lock").iter().any(|d| *d == Some(1))
}

fn has_sound(seen: &Observed, sound: &str) -> bool {
    seen.sounds.lock().expect("sound lock").iter().any(|s| s == sound)
}

const FARMLAND: &str = "minecraft:farmland[moisture=0]";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hoe_tills_dirt_and_wears_by_one_with_no_shield_in_hand() {
    let seen = click(Scenario { release: GameDataVersion::V26_3, tool: "iron_hoe", ground: "minecraft:dirt", shield: false, sneaking: false, pops: false }).await;
    assert_eq!(updates_at_target(&seen), vec![state(FARMLAND)]);
    assert!(worn(&seen), "a survival till wears the hoe by one: {:?}", seen.tool_slot.lock().unwrap());
    assert!(has_sound(&seen, "minecraft:item.hoe.till"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shield_takes_the_hoe_click_in_26_3_and_leaves_the_dirt_alone() {
    let seen = click(Scenario { release: GameDataVersion::V26_3, tool: "iron_hoe", ground: "minecraft:dirt", shield: true, sneaking: false, pops: false }).await;
    assert_eq!(updates_at_target(&seen), Vec::<StateId>::new(), "no tilled block under an off-hand shield");
    assert!(!worn(&seen) && !has_sound(&seen, "minecraft:item.hoe.till"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hoe_still_tills_under_an_offhand_shield_in_26_2() {
    let seen = click(Scenario { release: GameDataVersion::V26_2, tool: "iron_hoe", ground: "minecraft:dirt", shield: true, sneaking: false, pops: false }).await;
    assert_eq!(updates_at_target(&seen), vec![state(FARMLAND)]);
    assert!(worn(&seen));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sneaking_keeps_the_hoe_click_in_26_3() {
    let seen = click(Scenario { release: GameDataVersion::V26_3, tool: "iron_hoe", ground: "minecraft:dirt", shield: true, sneaking: true, pops: false }).await;
    assert_eq!(updates_at_target(&seen), vec![state(FARMLAND)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shovel_flattens_grass_unless_a_26_3_shield_takes_the_click() {
    let path = state("minecraft:dirt_path");
    let plain = click(Scenario { release: GameDataVersion::V26_3, tool: "iron_shovel", ground: "minecraft:grass_block[snowy=false]", shield: false, sneaking: false, pops: false }).await;
    assert_eq!(updates_at_target(&plain), vec![path]);
    assert!(has_sound(&plain, "minecraft:item.shovel.flatten"));
    let shielded = click(Scenario { release: GameDataVersion::V26_3, tool: "iron_shovel", ground: "minecraft:grass_block[snowy=false]", shield: true, sneaking: false, pops: false }).await;
    assert_eq!(updates_at_target(&shielded), Vec::<StateId>::new());
    let old = click(Scenario { release: GameDataVersion::V26_2, tool: "iron_shovel", ground: "minecraft:grass_block[snowy=false]", shield: true, sneaking: false, pops: false }).await;
    assert_eq!(updates_at_target(&old), vec![path]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tilling_rooted_dirt_pops_one_hanging_roots() {
    let seen = click(Scenario { release: GameDataVersion::V26_3, tool: "iron_hoe", ground: "minecraft:rooted_dirt", shield: false, sneaking: false, pops: true }).await;
    assert_eq!(updates_at_target(&seen), vec![state("minecraft:dirt")]);
    until("the popped roots", || seen.items.lock().expect("items lock").iter().any(|i| i == "minecraft:hanging_roots")).await;
}
