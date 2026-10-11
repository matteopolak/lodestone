//! What a removed block does to the world around it, driven by the real causes
//! (an explosion, `/setblock`, a player's dig) through live protocol-776
//! connections to one hosted world.
//!
//! The four behaviours are a brewing stand's bottles spilling, a destroyed
//! hive's bees, the window every other viewer of a vanished container has open,
//! and a jukebox's power while a song plays.
//!
//! Expected values come from outside the code under test: the bottle, ingredient
//! and fuel stacks are the five slots written in and the item ids the reference
//! gives each bottle kind; the silk-touch rule is the reference's enchantment
//! tag (`prevents_bee_spawns_when_mining` holds only silk touch); the bees'
//! 17 x 13 x 17 anger box and the four-block breaker rule are the reference's;
//! the wire layout of the close packet is a single VarInt window id; a playing
//! jukebox powers 15 and a stopped one 0.
//!
//! Controls, run by hand: dropping the bottle slots from the brewing stand's
//! spill, the hive release in `block_removal::dispose`, the `menu_still_valid`
//! check in `publish_open_container`, and the jukebox arm in
//! `redstone::weak_signal_with_entity_output` each fail the matching test here.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::{Ctx, Encode, Nbt, NbtTag, Reader, Writer};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ItemStack, ResourceKey, Vec3};
use lodestone_net::Connection;
use lodestone_server::commands::Effect;
use lodestone_server::{
    Beehive, BlockEntity, Bottle, BottleKind, BrewingStand, ChunkColumn, ChunkSource, EntitySnapshot, IntegratedServer, MetadataField, Occupant,
};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::{configuration, handshaking, login, play};
use lodestone_v26_2::packets::game::{ChatCommand, PlayerAction, UseItemOn};
use lodestone_v26_2::packets::handshake::Intention;
use lodestone_v26_2::packets::login::LoginHello;
use tokio::io::DuplexStream;
use tokio::net::TcpStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 776 };
const DEADLINE: Duration = Duration::from_secs(25);
const FLOOR: i32 = 63;
const FACE_UP: i32 = 1;

fn state(name: &str) -> StateId {
    StateId::from_state_str(name).expect("fixture state")
}

/// Stone at y = 63, air above, plus whatever a case writes in.
#[derive(Clone, Default)]
struct World {
    edits: Arc<Mutex<HashMap<BlockPos, StateId>>>,
}

impl World {
    fn base(y: i32) -> StateId {
        if y == FLOOR { state("minecraft:stone") } else { StateId::AIR }
    }

    fn at(&self, pos: BlockPos) -> StateId {
        self.block_state_id(pos.x, pos.y, pos.z)
    }
}

impl ChunkSource for World {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block_id(x, FLOOR, z, Self::base(FLOOR));
            }
        }
        for (&pos, &edit) in self.edits.lock().unwrap().iter() {
            if pos.x.div_euclid(16) == cx && pos.z.div_euclid(16) == cz {
                column.set_block_id(pos.x.rem_euclid(16), pos.y, pos.z.rem_euclid(16), edit);
            }
        }
        column
    }

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        Some(self.column(cx, cz))
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        self.edits.lock().unwrap().get(&BlockPos::new(x, y, z)).copied().unwrap_or_else(|| Self::base(y))
    }

    fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        Some(self.block_state_id(x, y, z))
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".into()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, edit: StateId) {
        self.edits.lock().unwrap().insert(BlockPos::new(x, y, z), edit);
    }
}

fn packed(pos: BlockPos) -> i64 {
    ((i64::from(pos.x) & 0x3FF_FFFF) << 38) | ((i64::from(pos.z) & 0x3FF_FFFF) << 12) | (i64::from(pos.y) & 0xFFF)
}

fn unpacked(value: i64) -> BlockPos {
    BlockPos::new((value >> 38) as i32, (value << 52 >> 52) as i32, (value << 26 >> 38) as i32)
}

type Log = Vec<(i32, Vec<u8>)>;

/// The host's in-memory connection or a guest's socket.
enum Link {
    Memory(Connection<DuplexStream>),
    Tcp(Connection<TcpStream>),
}

impl Link {
    async fn read_packet(&mut self) -> std::io::Result<Option<(i32, Vec<u8>)>> {
        match self {
            Link::Memory(c) => c.read_packet().await.map_err(std::io::Error::other),
            Link::Tcp(c) => c.read_packet().await.map_err(std::io::Error::other),
        }
    }

    async fn write_packet(&mut self, id: i32, payload: &[u8]) -> std::io::Result<()> {
        match self {
            Link::Memory(c) => c.write_packet(id, payload).await.map_err(std::io::Error::other),
            Link::Tcp(c) => c.write_packet(id, payload).await.map_err(std::io::Error::other),
        }
    }

    fn set_compression(&mut self, threshold: i32) {
        match self {
            Link::Memory(c) => c.set_compression(threshold),
            Link::Tcp(c) => c.set_compression(threshold),
        }
    }
}

/// A joined client: every packet read is acknowledged where the protocol needs
/// it and kept for inspection.
struct Session {
    client: Link,
    log: Log,
    sequence: i32,
    uuid: Uuid,
}

impl Session {
    async fn read(&mut self, wait: Duration) -> bool {
        match tokio::time::timeout(wait, self.client.read_packet()).await {
            Ok(Ok(Some((id, payload)))) => {
                self.acknowledge(id, &payload).await;
                self.log.push((id, payload));
                true
            }
            _ => false,
        }
    }

    async fn acknowledge(&mut self, id: i32, payload: &[u8]) {
        if id == play::clientbound::PLAYER_POSITION {
            let mut ack = Writer::default();
            ack.var_i32(Reader::new(payload).var_i32().unwrap());
            self.client.write_packet(play::serverbound::ACCEPT_TELEPORTATION, ack.as_slice()).await.unwrap();
        } else if id == play::clientbound::CHUNK_BATCH_FINISHED {
            let mut ack = Writer::default();
            ack.f32(64.0);
            self.client.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, ack.as_slice()).await.unwrap();
            self.client.write_packet(play::serverbound::PLAYER_LOADED, &[]).await.unwrap();
        } else if id == play::clientbound::KEEP_ALIVE {
            self.client.write_packet(play::serverbound::KEEP_ALIVE, payload).await.unwrap();
        }
    }

    async fn settle(&mut self, quiet: Duration) {
        let cap = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < cap && self.read(quiet).await {}
    }

    /// Reads until `done` holds over the packets received after `from`.
    async fn until(&mut self, what: &str, from: usize, done: impl Fn(&[(i32, Vec<u8>)]) -> bool) {
        let started = tokio::time::Instant::now();
        while !done(&self.log[from..]) {
            assert!(started.elapsed() < DEADLINE, "timed out waiting for {what}; saw {:?}", self.log[from..].iter().map(|p| p.0).collect::<Vec<_>>());
            self.read(Duration::from_millis(50)).await;
        }
    }

    async fn send(&mut self, id: i32, payload: &[u8]) {
        self.client.write_packet(id, payload).await.unwrap();
    }

    /// Right-clicks `pos` with the main hand and waits for the acknowledgement.
    async fn click(&mut self, pos: BlockPos) -> usize {
        self.sequence += 1;
        let sequence = self.sequence;
        let from = self.log.len();
        let mut writer = Writer::default();
        UseItemOn { hand: 0, pos: packed(pos), face: FACE_UP, cursor_x: 0.5, cursor_y: 1.0, cursor_z: 0.5, inside_block: false, world_border_hit: false, sequence }
            .encode(&mut writer, CTX)
            .unwrap();
        self.send(play::serverbound::USE_ITEM_ON, writer.as_slice()).await;
        self.until("the click acknowledgement", from, |log| {
            log.iter().any(|(id, p)| *id == play::clientbound::BLOCK_CHANGED_ACK && Reader::new(p).var_i32().unwrap() == sequence)
        })
        .await;
        self.settle(Duration::from_millis(120)).await;
        from
    }

    async fn select(&mut self, hotbar: i16) {
        self.send(play::serverbound::SET_CARRIED_ITEM, &hotbar.to_be_bytes()).await;
    }

    async fn walk_to(&mut self, x: f64, z: f64) {
        let mut writer = Writer::default();
        writer.f64(x);
        writer.f64(f64::from(FLOOR) + 1.0);
        writer.f64(z);
        writer.u8(1);
        self.send(play::serverbound::MOVE_PLAYER_POS, writer.as_slice()).await;
    }

    async fn dig(&mut self, pos: BlockPos) {
        for action in [0, 2] {
            self.sequence += 1;
            let mut writer = Writer::default();
            PlayerAction { action, pos: packed(pos), direction: 1, sequence: self.sequence }.encode(&mut writer, CTX).unwrap();
            self.send(play::serverbound::PLAYER_ACTION, writer.as_slice()).await;
        }
    }

    async fn command(&mut self, command: &str) {
        let mut writer = Writer::default();
        ChatCommand { command: command.to_owned() }.encode(&mut writer, CTX).unwrap();
        self.send(play::serverbound::CHAT_COMMAND, writer.as_slice()).await;
    }
}

/// Logs `client` in as `name` and returns once its first chunk batch landed.
async fn join(mut client: Link, name: &str, index: usize) -> Session {
    let uuid = Uuid::from_u128(0x5800 + index as u128);
    let mut writer = Writer::default();
    Intention { protocol_version: 776, host: "memory".into(), port: 25565, next_state: 2 }.encode(&mut writer, CTX).unwrap();
    client.write_packet(handshaking::serverbound::INTENTION, writer.as_slice()).await.unwrap();
    let mut writer = Writer::default();
    LoginHello { name: name.into(), profile_id: uuid }.encode(&mut writer, CTX).unwrap();
    client.write_packet(login::serverbound::HELLO, writer.as_slice()).await.unwrap();
    let mut session = Session { client, log: Vec::new(), sequence: 0, uuid };
    loop {
        let (id, payload) = tokio::time::timeout(DEADLINE, session.client.read_packet()).await.unwrap().unwrap().unwrap();
        if id == login::clientbound::LOGIN_COMPRESSION {
            session.client.set_compression(Reader::new(&payload).var_i32().unwrap());
        } else {
            assert_eq!(id, login::clientbound::LOGIN_FINISHED);
            break;
        }
    }
    session.send(login::serverbound::LOGIN_ACKNOWLEDGED, &[]).await;
    loop {
        let (id, _) = tokio::time::timeout(DEADLINE, session.client.read_packet()).await.unwrap().unwrap().unwrap();
        if id == configuration::clientbound::FINISH_CONFIGURATION {
            break;
        }
    }
    session.send(configuration::serverbound::FINISH_CONFIGURATION, &[]).await;
    session.until("the first chunk batch", 0, |log| log.iter().any(|(id, _)| *id == play::clientbound::CHUNK_BATCH_FINISHED)).await;
    session.settle(Duration::from_millis(300)).await;
    session
}

struct Harness {
    server: IntegratedServer,
    world: World,
    sessions: Vec<Session>,
}

impl Harness {
    /// Hosts a world that `prepare` has edited first. The first name is the
    /// host, on the in-memory connection; every other joins over a socket the
    /// host published. The server keeps its own resident copy of terrain, so
    /// later edits through the source would not be seen.
    async fn start(names: &[&str], prepare: impl FnOnce(&World)) -> Self {
        let world = World::default();
        prepare(&world);
        let (mut server, io) = IntegratedServer::open_in_memory_with_mobs(V770ServerProtocol, world.clone(), (-2..=2, -2..=2), (0, 0), 3);
        for (rule, value) in [("spawn_mobs", "false"), ("random_tick_speed", "0")] {
            server.world_state().set_rule(rule, value).expect("known rule");
        }
        let mut sessions = vec![join(Link::Memory(Connection::new(io)), names[0], 0).await];
        if names.len() > 1 {
            let address = server.publish(("127.0.0.1", 0), None).await.expect("publish the world");
            for (index, name) in names.iter().enumerate().skip(1) {
                let socket = TcpStream::connect(address).await.expect("connect");
                sessions.push(join(Link::Tcp(Connection::new(socket)), name, index).await);
            }
        }
        Self { server, world, sessions }
    }

    /// Reads every session for `wait`, so packets sent to the others arrive.
    async fn pump(&mut self, wait: Duration) {
        for session in &mut self.sessions {
            session.settle(wait).await;
        }
    }

    /// Reads every session until `done` holds.
    async fn wait(&mut self, what: &str, mut done: impl FnMut(&Harness) -> bool) {
        let started = tokio::time::Instant::now();
        while !done(self) {
            assert!(started.elapsed() < DEADLINE, "timed out waiting for {what}");
            for session in &mut self.sessions {
                session.read(Duration::from_millis(20)).await;
            }
        }
    }

    /// Gives player `who` a stack and returns the window-0 slot it landed in.
    async fn give(&mut self, who: usize, stack: ItemStack) -> i16 {
        let from = self.sessions[who].log.len();
        let uuid = self.sessions[who].uuid;
        let started = tokio::time::Instant::now();
        while !self.server.world_state().player_registry().push_effect(uuid, Effect::GiveItems(vec![stack.clone()])) {
            assert!(started.elapsed() < DEADLINE, "the player never registered");
            self.sessions[who].read(Duration::from_millis(20)).await;
        }
        self.sessions[who].until("the given stack", from, |log| log.iter().any(|(id, _)| *id == play::clientbound::CONTAINER_SET_SLOT)).await;
        let (_, payload) = self.sessions[who].log[from..].iter().find(|(id, _)| *id == play::clientbound::CONTAINER_SET_SLOT).unwrap();
        let mut reader = Reader::new(payload);
        let _window = reader.var_i32().unwrap();
        let _state = reader.var_i32().unwrap();
        reader.i16().unwrap()
    }

    fn insert(&self, pos: BlockPos, entity: BlockEntity) {
        self.server.block_entities().expect("block entities").with(|reg| reg.insert(pos, entity));
    }

    fn entity_exists(&self, pos: BlockPos) -> bool {
        self.server.block_entities().expect("block entities").with(|reg| reg.get(pos).is_some())
    }

    /// Primed TNT with a one-tick fuse centred on `at`.
    fn detonate(&self, at: Vec3) {
        self.server.mobs().expect("mob sim").with(|sim| sim.spawn_tnt(at, 1));
    }

    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.server.mobs().expect("mob sim").with(|sim| sim.snapshots())
    }

    /// Item entities on the ground: item id and stack size, sorted.
    fn ground_items(&self) -> Vec<(String, u8)> {
        let mut items: Vec<(String, u8)> = self
            .snapshots()
            .iter()
            .flat_map(|s| s.metadata.iter())
            .filter_map(|field| match field {
                MetadataField::Item { item, count, .. } => Some((item.to_string(), *count)),
                _ => None,
            })
            .collect();
        items.sort();
        items
    }

    fn bees(&self) -> Vec<EntitySnapshot> {
        self.snapshots().into_iter().filter(|s| s.entity_type.to_string() == "minecraft:bee").collect()
    }

    async fn shutdown(self) {
        drop(self.sessions);
        self.server.shutdown().await;
    }
}

fn stacked(name: &str, count: u32) -> ItemStack {
    ItemStack::new(ResourceKey::from_str(name).unwrap(), count)
}

fn clone_entity(entity: &BlockEntity) -> Option<BlockEntity> {
    match entity {
        BlockEntity::Opaque { id, nbt } => Some(BlockEntity::Opaque { id: id.clone(), nbt: nbt.clone() }),
        _ => None,
    }
}

fn has(log: &[(i32, Vec<u8>)], packet: i32) -> bool {
    log.iter().any(|(id, _)| *id == packet)
}

fn opened_windows(log: &[(i32, Vec<u8>)]) -> Vec<i32> {
    log.iter().filter(|(id, _)| *id == play::clientbound::OPEN_SCREEN).map(|(_, p)| Reader::new(p).var_i32().unwrap()).collect()
}

fn closed_windows(log: &[(i32, Vec<u8>)]) -> Vec<i32> {
    log.iter().filter(|(id, p)| *id == play::clientbound::CONTAINER_CLOSE && !p.is_empty()).map(|(_, p)| Reader::new(p).var_i32().unwrap()).collect()
}

fn block_updates(log: &[(i32, Vec<u8>)]) -> Vec<(BlockPos, StateId)> {
    log.iter()
        .filter(|(id, _)| *id == play::clientbound::BLOCK_UPDATE)
        .map(|(_, payload)| {
            let mut reader = Reader::new(payload);
            let pos = unpacked(reader.i64().unwrap());
            (pos, StateId::new(u32::try_from(reader.var_i32().unwrap()).unwrap()).expect("wire state"))
        })
        .collect()
}

fn level_events(log: &[(i32, Vec<u8>)]) -> Vec<(i32, BlockPos)> {
    log.iter()
        .filter(|(id, _)| *id == play::clientbound::LEVEL_EVENT)
        .map(|(_, payload)| {
            let mut reader = Reader::new(payload);
            (reader.i32().unwrap(), unpacked(reader.i64().unwrap()))
        })
        .collect()
}

// ---- brewing stand ---------------------------------------------------------

/// A stand holding a plain potion, a splash potion, a lingering potion, two
/// nether wart and three blaze powder.
fn loaded_brewing_stand() -> BlockEntity {
    let bottle = |kind| Bottle::from_potion_name(kind, "minecraft:water");
    // The fuel counter starts full so the first tick does not spend a powder.
    BlockEntity::BrewingStand(BrewingStand::restore(
        [bottle(BottleKind::Potion), bottle(BottleKind::Splash), bottle(BottleKind::Lingering)],
        Some(("minecraft:nether_wart".to_owned(), 2)),
        Some(("minecraft:blaze_powder".to_owned(), 3)),
        20,
        0,
        None,
    ))
}

/// The five stacks of a loaded stand, by the reference's item ids for the three
/// bottle kinds.
fn loaded_brewing_stand_stacks() -> Vec<(String, u8)> {
    let mut expected = vec![
        ("minecraft:potion".to_owned(), 1),
        ("minecraft:splash_potion".to_owned(), 1),
        ("minecraft:lingering_potion".to_owned(), 1),
        ("minecraft:nether_wart".to_owned(), 2),
        ("minecraft:blaze_powder".to_owned(), 3),
    ];
    expected.sort();
    expected
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_destroyed_brewing_stand_spills_its_bottles_ingredient_and_fuel() {
    let stand = BlockPos::new(2, FLOOR + 1, 2);
    let mut h = Harness::start(&["Keeper"], |world| world.set_block(stand.x, stand.y, stand.z, state("minecraft:brewing_stand"))).await;
    h.insert(stand, loaded_brewing_stand());
    assert!(h.ground_items().is_empty());

    h.sessions[0].command("setblock 2 64 2 minecraft:air destroy").await;
    h.wait("the stand to go", |h| h.world.at(stand) == StateId::AIR).await;
    h.wait("the spill", |h| h.ground_items().len() >= 5).await;
    let spilled = h.ground_items();
    // The stand's own loot (the stand item) is rolled besides the contents.
    let contents: Vec<(String, u8)> = spilled.iter().filter(|(item, _)| item != "minecraft:brewing_stand").cloned().collect();
    assert_eq!(contents, loaded_brewing_stand_stacks());
    assert!(!h.entity_exists(stand), "the entity goes with its block");
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blast_spills_a_brewing_stands_bottles_too() {
    let stand = BlockPos::new(2, FLOOR + 1, 2);
    let mut h = Harness::start(&["Keeper"], |world| world.set_block(stand.x, stand.y, stand.z, state("minecraft:brewing_stand"))).await;
    h.insert(stand, loaded_brewing_stand());
    h.detonate(Vec3::new(2.5, f64::from(FLOOR) + 1.0, 3.5));
    h.wait("the stand to go", |h| h.world.at(stand) == StateId::AIR).await;
    h.wait("the spill", |h| h.ground_items().iter().filter(|(item, _)| item.ends_with("potion")).count() >= 3).await;
    let potions: Vec<String> = h.ground_items().into_iter().map(|(item, _)| item).filter(|item| item.ends_with("potion")).collect();
    assert_eq!(potions, ["minecraft:lingering_potion", "minecraft:potion", "minecraft:splash_potion"]);
    h.shutdown().await;
}

// ---- beehives ---------------------------------------------------------------

fn bee_record(uuid_word: i32) -> Nbt {
    Nbt::Compound(vec![
        ("id".into(), Nbt::String("minecraft:bee".into())),
        ("UUID".into(), Nbt::IntArray(vec![0, 0, 0, uuid_word])),
        (
            "Pos".into(),
            Nbt::List { element_type: NbtTag::Double, elements: vec![Nbt::Double(0.0), Nbt::Double(0.0), Nbt::Double(0.0)] },
        ),
    ])
}

fn hive_with_one_bee() -> BlockEntity {
    BlockEntity::Beehive(Beehive::restore(vec![Occupant::entering(bee_record(7))], None))
}

async fn hive_world(names: &[&str]) -> (Harness, BlockPos) {
    let nest = BlockPos::new(4, FLOOR + 1, 4);
    let mut h = Harness::start(names, |world| world.set_block(nest.x, nest.y, nest.z, state("minecraft:bee_nest"))).await;
    h.insert(nest, hive_with_one_bee());
    // Stand close, inside the anger box and the breaker's four blocks.
    h.sessions[0].walk_to(3.5, 3.5).await;
    h.pump(Duration::from_millis(200)).await;
    (h, nest)
}

/// Runs the world until a bee has been released and has had time to take a
/// grudge, then returns its target.
async fn released_bee_target(h: &mut Harness) -> Option<Vec3> {
    h.wait("a bee to come out", |h| !h.bees().is_empty()).await;
    h.pump(Duration::from_millis(400)).await;
    let id = h.bees()[0].id;
    h.server.mobs().expect("mob sim").with(|sim| sim.get(id).and_then(|bee| bee.angry_target()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blast_empties_the_hive_and_angers_the_bee_at_the_player() {
    let (mut h, nest) = hive_world(&["Keeper"]).await;
    h.sessions[0].command("gamemode creative").await;
    h.pump(Duration::from_millis(300)).await;
    h.detonate(Vec3::new(4.5, f64::from(FLOOR) + 1.0, 5.5));
    h.wait("the nest to go", |h| h.world.at(nest) == StateId::AIR).await;
    let target = released_bee_target(&mut h).await;
    assert_eq!(h.bees().len(), 1, "the one bee in the hive comes out");
    let target = target.expect("a bee in the box takes the only player in it as its target");
    assert!((target.x - 3.5).abs() < 2.0 && (target.z - 3.5).abs() < 2.0, "the grudge chases the player, not the hive: {target:?}");
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_setblock_destroy_loses_the_bees() {
    let (mut h, nest) = hive_world(&["Keeper"]).await;
    h.sessions[0].command("setblock 4 64 4 minecraft:air destroy").await;
    h.wait("the nest to go", |h| h.world.at(nest) == StateId::AIR).await;
    h.pump(Duration::from_millis(600)).await;
    assert!(h.bees().is_empty(), "only a dig or a blast of TNT, a creeper or a TNT minecart lets bees out");
    assert!(!h.entity_exists(nest));
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dig_lets_the_bees_out_angry_unless_the_tool_has_silk_touch_or_the_player_is_in_creative() {
    // A plain dig: the bee comes out and takes the breaker, four blocks away at most.
    let (mut h, nest) = hive_world(&["Keeper"]).await;
    let slot = h.give(0, stacked("minecraft:diamond_pickaxe", 1)).await;
    h.sessions[0].select(slot - 36).await;
    h.sessions[0].dig(nest).await;
    h.wait("the nest to break", |h| h.world.at(nest) == StateId::AIR).await;
    let target = released_bee_target(&mut h).await;
    assert_eq!(h.bees().len(), 1);
    assert!(target.is_some(), "the released bee goes for the player who dug");
    h.shutdown().await;

    // Silk touch keeps the bees in.
    let (mut h, nest) = hive_world(&["Keeper"]).await;
    let mut silk = stacked("minecraft:diamond_pickaxe", 1);
    silk.components.enchantments.push(lodestone_model::ItemEnchantment {
        id: lodestone_server::enchantment_data::id_of("minecraft:silk_touch").expect("silk touch is registered"),
        level: 1,
    });
    let slot = h.give(0, silk).await;
    h.sessions[0].select(slot - 36).await;
    h.sessions[0].dig(nest).await;
    h.wait("the nest to break", |h| h.world.at(nest) == StateId::AIR).await;
    h.pump(Duration::from_millis(800)).await;
    assert!(h.bees().is_empty(), "silk touch releases nothing");
    h.shutdown().await;

    // Creative breaks the hive and keeps the bees in.
    let (mut h, nest) = hive_world(&["Keeper"]).await;
    h.sessions[0].command("gamemode creative").await;
    h.pump(Duration::from_millis(300)).await;
    h.sessions[0].dig(nest).await;
    h.wait("the nest to break", |h| h.world.at(nest) == StateId::AIR).await;
    h.pump(Duration::from_millis(800)).await;
    assert!(h.bees().is_empty(), "a creative dig releases nothing");
    h.shutdown().await;
}

// ---- open windows -----------------------------------------------------------

/// One container per block kind that has an open-menu viewer.
fn container_kinds() -> Vec<(&'static str, BlockEntity)> {
    vec![
        ("minecraft:chest", BlockEntity::container("minecraft:chest")),
        ("minecraft:barrel", BlockEntity::container("minecraft:barrel")),
        ("minecraft:dispenser", BlockEntity::container("minecraft:dispenser")),
        ("minecraft:shulker_box", BlockEntity::container("minecraft:shulker_box")),
        ("minecraft:furnace", BlockEntity::Furnace(lodestone_server::Furnace::new(lodestone_server::FurnaceKind::Furnace))),
        ("minecraft:hopper", BlockEntity::Hopper(lodestone_server::Hopper::new())),
    ]
}

fn container_cell(index: usize) -> BlockPos {
    // A 3 x 2 grid within reach of a player standing at (3.5, 3.5).
    BlockPos::new(1 + 2 * (index as i32 % 3), FLOOR + 1, if index < 3 { 1 } else { 5 })
}

async fn window_world() -> Harness {
    let mut h = Harness::start(&["Breaker", "Viewer"], |world| {
        for (index, (block, _)) in container_kinds().iter().enumerate() {
            let at = container_cell(index);
            world.set_block(at.x, at.y, at.z, state(block));
        }
        world.set_block(9, FLOOR + 1, 9, state("minecraft:stone"));
    })
    .await;
    for (index, (_, entity)) in container_kinds().into_iter().enumerate() {
        h.insert(container_cell(index), entity);
    }
    h.sessions[1].walk_to(3.5, 3.0).await;
    h.pump(Duration::from_millis(200)).await;
    h
}

/// Every container kind's window closes on the viewer's connection when
/// another player removes the block, and only then: removing a different
/// block leaves it open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_players_removal_closes_the_window_of_every_container_kind() {
    let mut h = window_world().await;
    for (index, (block, _)) in container_kinds().iter().enumerate() {
        let at = container_cell(index);
        let from = h.sessions[1].log.len();
        h.sessions[1].click(at).await;
        let windows = opened_windows(&h.sessions[1].log[from..]);
        let [window] = windows[..] else { panic!("{block}: expected one window, got {windows:?}") };

        // A removal elsewhere is a control: the window stays.
        h.sessions[0].command("setblock 9 64 9 minecraft:air").await;
        h.wait("the control removal", |h| h.world.at(BlockPos::new(9, FLOOR + 1, 9)) == StateId::AIR).await;
        h.pump(Duration::from_millis(300)).await;
        assert!(closed_windows(&h.sessions[1].log[from..]).is_empty(), "{block}: an unrelated removal closed the window");
        h.sessions[0].command("setblock 9 64 9 minecraft:stone").await;
        h.wait("the control block back", |h| h.world.at(BlockPos::new(9, FLOOR + 1, 9)) != StateId::AIR).await;

        // The viewer's block goes by a command the viewer did not issue.
        h.sessions[0].command(&format!("setblock {} {} {} minecraft:air destroy", at.x, at.y, at.z)).await;
        h.sessions[1].until(&format!("{block}: the close packet"), from, |log| has(log, play::clientbound::CONTAINER_CLOSE)).await;
        assert_eq!(closed_windows(&h.sessions[1].log[from..]), [window], "{block}: the close names the window the viewer opened");
        assert_eq!(h.world.at(at), StateId::AIR, "{block}");
        assert!(!h.entity_exists(at), "{block}: the entity goes with its block");
        // The breaker never had it open and gets no close.
        h.pump(Duration::from_millis(100)).await;
        assert!(closed_windows(&h.sessions[0].log).is_empty(), "{block}: a bystander got a close packet");
    }
    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blast_closes_the_window_of_a_chest_it_destroys() {
    let mut h = window_world().await;
    // Creative, so the blast cannot kill the viewer and end the window another way.
    h.sessions[1].command("gamemode creative").await;
    h.pump(Duration::from_millis(300)).await;
    let at = container_cell(0);
    let from = h.sessions[1].log.len();
    h.sessions[1].click(at).await;
    let windows = opened_windows(&h.sessions[1].log[from..]);
    let [window] = windows[..] else { panic!("expected one window, got {windows:?}") };
    h.detonate(Vec3::new(f64::from(at.x) + 0.5, f64::from(at.y), f64::from(at.z) + 1.5));
    h.wait("the chest to go", |h| h.world.at(at) == StateId::AIR).await;
    h.sessions[1].until("the close packet", from, |log| has(log, play::clientbound::CONTAINER_CLOSE)).await;
    assert_eq!(closed_windows(&h.sessions[1].log[from..]), [window]);
    h.shutdown().await;
}

// ---- jukebox power -------------------------------------------------------------

fn jukebox_entity(disc: &str, playing_for: Option<i64>) -> BlockEntity {
    let record = Nbt::Compound(vec![("id".into(), Nbt::String(disc.into())), ("count".into(), Nbt::Int(1))]);
    let mut fields = vec![("id".into(), Nbt::String("minecraft:jukebox".into())), ("RecordItem".into(), record)];
    if let Some(ticks) = playing_for {
        fields.push(("ticks_since_song_started".into(), Nbt::Long(ticks)));
    }
    BlockEntity::Opaque { id: "minecraft:jukebox".to_owned().into(), nbt: Nbt::Compound(fields) }
}

/// The `power` values the log's block updates gave `wire`, in order, with
/// repeats folded.
fn power_history(log: &[(i32, Vec<u8>)], wire: BlockPos) -> Vec<String> {
    let mut history: Vec<String> = Vec::new();
    for (at, state) in block_updates(log) {
        let Some(power) = state.properties().iter().find_map(|(k, v)| (*k == "power").then(|| (*v).to_owned())) else { continue };
        if at == wire && history.last() != Some(&power) {
            history.push(power);
        }
    }
    history
}

async fn wait_for_powers(h: &mut Harness, wire: BlockPos, expected: &[&str]) {
    let started = tokio::time::Instant::now();
    loop {
        let history = power_history(&h.sessions[0].log, wire);
        if history == expected {
            return;
        }
        assert!(started.elapsed() < DEADLINE, "the dust's power history should be {expected:?}, saw {history:?}");
        h.sessions[0].read(Duration::from_millis(50)).await;
    }
}

/// Dust beside a jukebox follows the song: 15 while it plays, 0 once it runs
/// out, 15 again for a fresh disc and 0 once the disc is ejected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_jukebox_powers_dust_beside_it_while_a_song_plays() {
    let jukebox = BlockPos::new(2, FLOOR + 1, 2);
    let idle = BlockPos::new(2, FLOOR + 1, 6);
    let wire = BlockPos::new(3, FLOOR + 1, 2);
    let idle_wire = BlockPos::new(3, FLOOR + 1, 6);
    let mut h = Harness::start(&["Keeper"], |world| {
        for pos in [jukebox, idle] {
            world.set_block(pos.x, pos.y, pos.z, state("minecraft:jukebox[has_record=true]"));
        }
        for pos in [wire, idle_wire] {
            world.set_block(pos.x, pos.y, pos.z, state("minecraft:redstone_wire"));
        }
    })
    .await;
    // Ten ticks short of the end, with a second jukebox holding a disc but not playing.
    h.insert(jukebox, jukebox_entity("minecraft:music_disc_11", Some(1430)));
    h.insert(idle, jukebox_entity("minecraft:music_disc_11", None));

    wait_for_powers(&mut h, wire, &["15", "0"]).await;
    h.sessions[0].until("the song's end", 0, |log| level_events(log).iter().any(|e| e.0 == 1011)).await;
    h.pump(Duration::from_millis(300)).await;
    assert!(power_history(&h.sessions[0].log, idle_wire).is_empty(), "a jukebox that is not playing powers nothing");

    // A fresh disc plays again, and taking it out ends the power.
    h.server.block_entities().expect("block entities").with(|reg| reg.insert(jukebox, jukebox_entity("minecraft:music_disc_11", Some(0))));
    wait_for_powers(&mut h, wire, &["15", "0", "15"]).await;
    h.sessions[0].click(jukebox).await;
    wait_for_powers(&mut h, wire, &["15", "0", "15", "0"]).await;
    let Some(BlockEntity::Opaque { nbt: Nbt::Compound(fields), .. }) = h.server.block_entities().expect("block entities").with(|reg| reg.get(jukebox).and_then(clone_entity)) else {
        panic!("the jukebox keeps its entity")
    };
    assert!(!fields.iter().any(|(name, _)| name == "RecordItem"), "the disc came out");
    h.shutdown().await;
}
