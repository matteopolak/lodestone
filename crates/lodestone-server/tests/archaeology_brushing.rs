//! Brushing suspicious gravel that structure generation placed, through a live 26.2 connection.
//!
//! The columns are the production 26.3 Overworld generator's (seed 42): a trail ruins site whose
//! processors append an archaeology loot table to the gravel they turn suspicious. A real player
//! teleports onto a block, holds a brush and right-clicks; the observations are the packets the
//! client gets (block updates for the `dusted` steps, block-entity data, the completion level event
//! and the item entity) and the server's own block-entity registry.
//!
//! Expected values come from outside the code under test: the brush takes ten strokes starting
//! five ticks in and ten apart, the dust steps at the first, third and sixth stroke and the tenth
//! finishes, a settling block loses two strokes every four ticks after forty idle ones, the item
//! comes from one of the two vanilla trail-ruins tables (their item lists are copied below from the
//! vanilla data files), and a finished block turns into plain gravel.
//!
//! Controls, run by hand: dropping the `brushing::begin` call in the use-item-on dispatch ends the
//! dusted updates; dropping the brushing tick in `server/player_tick.rs` (the pass both play loops share) does the same; removing the
//! `mark_client_dirty` call leaves no block-entity data on the wire; and dropping the gravity-tick
//! arm's settle leaves the dust where it was.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::{Ctx, Encode, Nbt, Reader, Writer};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ItemStack, ResourceKey};
use lodestone_net::Connection;
use lodestone_server::commands::Effect;
use lodestone_server::{
    BlockEntity, ChunkColumn, ChunkSource, IntegratedServer, MetadataField, WorldType, overworld_chunk_source_of_type,
};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::{configuration, handshaking, login, play};
use lodestone_v26_2::packets::game::{PlayerAction, UseItemOn};
use lodestone_v26_2::packets::handshake::Intention;
use lodestone_v26_2::packets::login::LoginHello;
use tokio::io::DuplexStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 776 };
const DEADLINE: Duration = Duration::from_secs(120);
const PLAYER: u128 = 0xB805;
const FACE_UP: i32 = 1;
const RELEASE_USE_ITEM: i32 = 5;
const SEED: i64 = 42;

/// Trail ruins sit around chunk (2, -34) in seed 42; these chunks hold suspicious gravel.
const RUIN_CHUNKS: [(i32, i32); 4] = [(2, -34), (1, -34), (1, -35), (2, -35)];

/// Every item of the vanilla `trail_ruins_common` loot table.
const COMMON: [&str; 31] = [
    "emerald", "wheat", "wooden_hoe", "clay", "brick", "yellow_dye", "blue_dye", "light_blue_dye", "white_dye",
    "orange_dye", "red_candle", "green_candle", "purple_candle", "brown_candle", "magenta_stained_glass_pane",
    "pink_stained_glass_pane", "blue_stained_glass_pane", "light_blue_stained_glass_pane", "red_stained_glass_pane",
    "yellow_stained_glass_pane", "purple_stained_glass_pane", "spruce_hanging_sign", "oak_hanging_sign", "gold_nugget",
    "coal", "wheat_seeds", "beetroot_seeds", "dead_bush", "flower_pot", "string", "lead",
];
/// Every item of the vanilla `trail_ruins_rare` loot table.
const RARE: [&str; 12] = [
    "burn_pottery_sherd", "danger_pottery_sherd", "friend_pottery_sherd", "heart_pottery_sherd",
    "heartbreak_pottery_sherd", "howl_pottery_sherd", "sheaf_pottery_sherd", "wayfinder_armor_trim_smithing_template",
    "raiser_armor_trim_smithing_template", "shaper_armor_trim_smithing_template", "host_armor_trim_smithing_template",
    "music_disc_relic",
];

fn table_items(table: &str) -> Vec<String> {
    let names: &[&str] = match table {
        "minecraft:archaeology/trail_ruins_common" => &COMMON,
        "minecraft:archaeology/trail_ruins_rare" => &RARE,
        other => panic!("a trail ruins gravel names an unexpected table {other}"),
    };
    names.iter().map(|name| format!("minecraft:{name}")).collect()
}

fn packed(pos: BlockPos) -> i64 {
    ((i64::from(pos.x) & 0x3FF_FFFF) << 38) | ((i64::from(pos.z) & 0x3FF_FFFF) << 12) | (i64::from(pos.y) & 0xFFF)
}

fn unpacked(value: i64) -> BlockPos {
    BlockPos::new((value >> 38) as i32, (value << 52 >> 52) as i32, (value << 26 >> 38) as i32)
}

fn property<'a>(state: StateId, key: &str) -> Option<&'a str> {
    state.properties().iter().find_map(|(k, v)| (*k == key).then_some(*v))
}

fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields.iter().find(|(name, _)| name == key).map(|(_, value)| value),
        _ => None,
    }
}

/// A suspicious block of the generated ruin the player can stand over and look straight down at.
struct Target {
    pos: BlockPos,
    table: String,
}

fn find_target() -> Target {
    let source = overworld_chunk_source_of_type(SEED, WorldType::Overworld);
    for (cx, cz) in RUIN_CHUNKS {
        let column = source.column(cx, cz);
        for (pos, entity) in column.block_entities() {
            let BlockEntity::Brushable(brushable) = entity else { continue };
            let at = |dy: i32| column.block_state_id(pos.x.rem_euclid(16), pos.y + dy, pos.z.rem_euclid(16));
            let loot = brushable.loot().expect("a generated brushable block still holds its table");
            if at(0).name() == "minecraft:suspicious_gravel"
                && at(1).name() == "minecraft:air"
                && at(2).name() == "minecraft:air"
                && at(3).name() == "minecraft:air"
                && !matches!(at(-1).name(), "minecraft:air" | "minecraft:cave_air" | "minecraft:water" | "minecraft:lava")
            {
                return Target { pos: *pos, table: loot.table.clone() };
            }
        }
    }
    panic!("no suspicious gravel with open air above it in {RUIN_CHUNKS:?}");
}

type Log = Vec<(i32, Vec<u8>)>;

struct Session {
    client: Connection<DuplexStream>,
    log: Log,
    sequence: i32,
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

    async fn until(&mut self, what: &str, from: usize, done: impl Fn(&[(i32, Vec<u8>)]) -> bool) {
        let started = tokio::time::Instant::now();
        while !done(&self.log[from..]) {
            assert!(
                started.elapsed() < DEADLINE,
                "timed out waiting for {what}; saw {:?}",
                self.log[from..].iter().map(|p| p.0).collect::<Vec<_>>()
            );
            self.read(Duration::from_millis(50)).await;
        }
    }

    async fn send(&mut self, id: i32, payload: &[u8]) {
        self.client.write_packet(id, payload).await.unwrap();
    }

    async fn right_click(&mut self, pos: BlockPos) {
        self.sequence += 1;
        let sequence = self.sequence;
        let from = self.log.len();
        let mut writer = Writer::default();
        UseItemOn {
            hand: 0,
            pos: packed(pos),
            face: FACE_UP,
            cursor_x: 0.5,
            cursor_y: 1.0,
            cursor_z: 0.5,
            inside_block: false,
            world_border_hit: false,
            sequence,
        }
        .encode(&mut writer, CTX)
        .unwrap();
        self.send(play::serverbound::USE_ITEM_ON, writer.as_slice()).await;
        self.until("the click acknowledgement", from, |log| {
            log.iter().any(|(id, p)| *id == play::clientbound::BLOCK_CHANGED_ACK && Reader::new(p).var_i32().unwrap() == sequence)
        })
        .await;
    }

    async fn release(&mut self) {
        self.sequence += 1;
        let mut writer = Writer::default();
        PlayerAction { action: RELEASE_USE_ITEM, pos: 0, direction: 0, sequence: self.sequence }.encode(&mut writer, CTX).unwrap();
        self.send(play::serverbound::PLAYER_ACTION, writer.as_slice()).await;
    }
}

struct Harness {
    server: IntegratedServer,
    session: Session,
}

impl Harness {
    async fn start<S: ChunkSource + 'static>(source: S, centre: (i32, i32)) -> Self {
        let (server, io) = IntegratedServer::open_in_memory_with_mobs(V770ServerProtocol, source, (-2..=2, -2..=2), centre, 2);
        for (rule, value) in [("spawn_mobs", "false"), ("random_tick_speed", "0")] {
            server.world_state().set_rule(rule, value).expect("known rule");
        }
        let mut client = Connection::new(io);
        let mut writer = Writer::default();
        Intention { protocol_version: 776, host: "memory".into(), port: 25565, next_state: 2 }.encode(&mut writer, CTX).unwrap();
        client.write_packet(handshaking::serverbound::INTENTION, writer.as_slice()).await.unwrap();
        let mut writer = Writer::default();
        LoginHello { name: "Digger".into(), profile_id: Uuid::from_u128(PLAYER) }.encode(&mut writer, CTX).unwrap();
        client.write_packet(login::serverbound::HELLO, writer.as_slice()).await.unwrap();
        let mut session = Session { client, log: Vec::new(), sequence: 0 };
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
        let mut harness = Self { server, session };
        harness.session.settle(Duration::from_millis(300)).await;
        harness
    }

    async fn push(&mut self, effect: Effect) {
        let started = tokio::time::Instant::now();
        while !self.server.world_state().player_registry().push_effect(Uuid::from_u128(PLAYER), effect.clone()) {
            assert!(started.elapsed() < DEADLINE, "the player never registered");
            self.session.read(Duration::from_millis(20)).await;
        }
    }

    /// Gives one brush and returns the window-0 slot it landed in.
    async fn give_brush(&mut self) -> i16 {
        let stack = ItemStack::new(ResourceKey::from_str("minecraft:brush").unwrap(), 1);
        let from = self.session.log.len();
        self.push(Effect::GiveItems(vec![stack])).await;
        self.session.until("the brush", from, |log| log.iter().any(|(id, _)| *id == play::clientbound::CONTAINER_SET_SLOT)).await;
        let (_, payload) = self.session.log[from..].iter().find(|(id, _)| *id == play::clientbound::CONTAINER_SET_SLOT).unwrap();
        let mut reader = Reader::new(payload);
        let _window = reader.var_i32().unwrap();
        let _state = reader.var_i32().unwrap();
        reader.i16().unwrap()
    }

    /// Moves the player to stand on `pos`'s top face looking straight down, and waits for that chunk.
    async fn stand_over(&mut self, pos: BlockPos) {
        let from = self.session.log.len();
        self.push(Effect::Teleport {
            x: f64::from(pos.x) + 0.5,
            y: f64::from(pos.y) + 1.0,
            z: f64::from(pos.z) + 0.5,
            yaw: Some(0.0),
            pitch: Some(90.0),
        })
        .await;
        let (cx, cz) = (pos.x.div_euclid(16), pos.z.div_euclid(16));
        let has_chunk = |log: &[(i32, Vec<u8>)]| {
            log.iter().any(|(id, p)| {
                *id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT && {
                    let mut reader = Reader::new(p);
                    (reader.i32().unwrap(), reader.i32().unwrap()) == (cx, cz)
                }
            })
        };
        self.session.until("the teleport", from, |log| log.iter().any(|(id, _)| *id == play::clientbound::PLAYER_POSITION)).await;
        // A client reports where it now stands, which is what recentres the chunk view.
        let mut writer = Writer::default();
        writer.f64(f64::from(pos.x) + 0.5);
        writer.f64(f64::from(pos.y) + 1.0);
        writer.f64(f64::from(pos.z) + 0.5);
        writer.u8(1);
        self.session.send(play::serverbound::MOVE_PLAYER_POS, writer.as_slice()).await;
        self.session.until("the target chunk", 0, |log| has_chunk(log)).await;
        self.session.settle(Duration::from_millis(400)).await;
    }

    fn game_time(&self) -> i64 {
        self.server.world_state().time().game_time
    }

    fn item_entities(&self) -> Vec<(ResourceKey, u8)> {
        self.server
            .mobs()
            .expect("the server runs a mob sim")
            .with(|sim| sim.snapshots())
            .iter()
            .flat_map(|snapshot| {
                snapshot.metadata.iter().filter_map(|field| match field {
                    MetadataField::Item { item, count, .. } => Some((item.clone(), *count)),
                    _ => None,
                })
            })
            .collect()
    }

    async fn shutdown(self) {
        drop(self.session);
        self.server.shutdown().await;
    }
}

/// What the block at `pos` was on the wire, in order: its `dusted` value, or its name once it stopped
/// being suspicious gravel.
fn block_history(log: &[(i32, Vec<u8>)], pos: BlockPos) -> Vec<String> {
    log.iter()
        .filter(|(id, _)| *id == play::clientbound::BLOCK_UPDATE)
        .filter_map(|(_, payload)| {
            let mut reader = Reader::new(payload);
            let at = unpacked(reader.i64().unwrap());
            let state = StateId::new(u32::try_from(reader.var_i32().unwrap()).unwrap()).expect("wire state");
            (at == pos).then(|| match property(state, "dusted") {
                Some(dusted) => dusted.to_owned(),
                None => state.name().trim_start_matches("minecraft:").to_owned(),
            })
        })
        .collect()
}

/// Block-entity data for `pos`, in order.
fn entity_data(log: &[(i32, Vec<u8>)], pos: BlockPos) -> Vec<Nbt> {
    log.iter()
        .filter(|(id, _)| *id == play::clientbound::BLOCK_ENTITY_DATA)
        .filter_map(|(_, payload)| {
            let mut reader = Reader::new(payload);
            let at = unpacked(reader.i64().unwrap());
            let _kind = reader.var_i32().unwrap();
            let nbt = lodestone_core::read_network_nbt(&mut reader).unwrap();
            (at == pos).then_some(nbt)
        })
        .collect()
}

fn level_events(log: &[(i32, Vec<u8>)]) -> Vec<(i32, BlockPos, i32)> {
    log.iter()
        .filter(|(id, _)| *id == play::clientbound::LEVEL_EVENT)
        .map(|(_, payload)| {
            let mut reader = Reader::new(payload);
            let event = reader.i32().unwrap();
            (event, unpacked(reader.i64().unwrap()), reader.i32().unwrap())
        })
        .collect()
}

fn revealed_item(nbt: &Nbt) -> Option<String> {
    match field(field(nbt, "item")?, "id")? {
        Nbt::String(id) => Some(id.clone()),
        _ => None,
    }
}

/// The whole dig: ten strokes, the three dust steps, the item revealed, then the item entity, the
/// completion event and plain gravel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_brushed_structure_gravel_dusts_reveals_and_gives_up_its_item() {
    let target = find_target();
    let pos = target.pos;
    let allowed = table_items(&target.table);
    let mut h = Harness::start(overworld_chunk_source_of_type(SEED, WorldType::Overworld), (pos.x.div_euclid(16), pos.z.div_euclid(16))).await;
    let slot = h.give_brush().await;
    assert_eq!(slot, 36, "the first hotbar slot");
    h.stand_over(pos).await;

    let started_at = h.game_time();
    h.session.right_click(pos).await;
    // The click's own correction of the clicked cell is acknowledged before the first stroke lands.
    let before = h.session.log.len();
    h.session
        .until("the block turning into gravel", before, |log| block_history(log, pos).last().is_some_and(|name| name == "gravel"))
        .await;
    let finished_at = h.game_time();
    h.session.settle(Duration::from_millis(300)).await;
    let log = &h.session.log[before..];

    assert_eq!(block_history(log, pos), ["1", "2", "3", "gravel"], "the dust steps at strokes one, three and six, then the block turns into gravel");

    // Strokes land at ticks 5, 15, ..., 95 of the use.
    let elapsed = finished_at - started_at;
    assert!((90..=130).contains(&elapsed), "ten strokes ten ticks apart finish about 95 ticks in, took {elapsed}");

    let data = entity_data(log, pos);
    let revealed: Vec<_> = data.iter().filter_map(revealed_item).collect();
    assert!(!revealed.is_empty(), "the first stroke streams the revealed item to the client: {data:?}");
    let item = revealed[0].clone();
    assert!(allowed.contains(&item), "{item} is not an item of {}", target.table);
    assert!(revealed.iter().all(|r| *r == item), "the rolled item never changes");
    assert!(
        data.iter().all(|nbt| matches!(field(nbt, "hit_direction"), Some(Nbt::Byte(1)))),
        "the stroke hit the top face, which is direction id 1: {data:?}"
    );

    let events = level_events(log);
    let (event, at, state_on_wire) = *events.iter().find(|(event, ..)| *event == 3008).expect("completion level event");
    assert_eq!((event, at), (3008, pos));
    let last_suspicious = log
        .iter()
        .filter(|(id, _)| *id == play::clientbound::BLOCK_UPDATE)
        .filter_map(|(_, p)| {
            let mut reader = Reader::new(p);
            let at = unpacked(reader.i64().unwrap());
            let state = i32::try_from(reader.var_i32().unwrap()).unwrap();
            (at == pos).then_some(state)
        })
        .rev()
        .nth(1)
        .expect("the update before the gravel");
    assert_eq!(state_on_wire, last_suspicious, "the event carries the block state the gravel had when it finished");

    // The item entity.
    let items = h.item_entities();
    assert_eq!(items.len(), 1, "one item entity appeared: {items:?}");
    assert_eq!(items[0].0.to_string(), item, "the entity carries the revealed item");
    assert!((1..=30).contains(&items[0].1), "the dropped count is at most the 10..=30 roll and at most the stack: {items:?}");
    let spawn = log.iter().find(|(id, _)| *id == play::clientbound::ADD_ENTITY).expect("the item is announced");
    let mut reader = Reader::new(&spawn.1);
    let _id = reader.var_i32().unwrap();
    let _uuid = reader.bytes(16).unwrap();
    let _kind = reader.var_i32().unwrap();
    let (x, y, z) = (reader.f64().unwrap(), reader.f64().unwrap(), reader.f64().unwrap());
    assert!((x - (f64::from(pos.x) + 0.5)).abs() < 1e-6 && (z - (f64::from(pos.z) + 0.5)).abs() < 1e-6, "dropped at the cell's centre: {x} {z}");
    assert!((y - (f64::from(pos.y) + 1.625)).abs() < 0.1, "dropped from the cell above: {y}");

    // The brush wore by one when the block finished.
    assert!(log.iter().any(|(id, p)| *id == play::clientbound::CONTAINER_SET_SLOT && Reader::new(p).var_i32().is_ok()), "the brush slot was resent");
    h.shutdown().await;
}

/// A brush put down after three strokes lets the dust settle: it steps back through the same
/// states, and the client stops being told about an item.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dust_settles_when_the_brushing_stops() {
    let target = find_target();
    let pos = target.pos;
    let mut h = Harness::start(overworld_chunk_source_of_type(SEED, WorldType::Overworld), (pos.x.div_euclid(16), pos.z.div_euclid(16))).await;
    h.give_brush().await;
    h.stand_over(pos).await;
    h.session.right_click(pos).await;
    let before = h.session.log.len();
    // Strokes at ticks 5, 15 and 25 reach state 2; release after the third.
    h.session.until("the second dust step", before, |log| block_history(log, pos).last().is_some_and(|d| d == "2")).await;
    h.session.release().await;
    // 40 idle ticks later two strokes drop (3 -> 1: state 1), four ticks on the rest (state 0).
    h.session.until("the dust settling to nothing", before, |log| block_history(log, pos).last().is_some_and(|d| d == "0")).await;
    assert_eq!(block_history(&h.session.log[before..], pos), ["1", "2", "1", "0"]);
    let data = entity_data(&h.session.log[before..], pos);
    let last = data.last().expect("block-entity data");
    assert!(field(last, "hit_direction").is_none(), "a settled block forgets the face it was brushed from: {last:?}");
    h.shutdown().await;
}

/// Stone floor at y = 63, a slime block at [`SUPPORT`] and suspicious gravel on top of it whose
/// generated entity holds an unrolled loot table.
#[derive(Clone, Default)]
struct FallWorld {
    edits: Arc<Mutex<HashMap<BlockPos, StateId>>>,
}

const FLOOR: i32 = 63;

fn support() -> BlockPos {
    BlockPos::new(2, FLOOR + 4, 2)
}

fn resting() -> BlockPos {
    BlockPos::new(2, FLOOR + 5, 2)
}

impl FallWorld {
    fn base(y: i32) -> StateId {
        if y == FLOOR { StateId::from_state_str("minecraft:stone").unwrap() } else { StateId::AIR }
    }
}

impl ChunkSource for FallWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block_id(x, FLOOR, z, Self::base(FLOOR));
            }
        }
        if (cx, cz) == (0, 0) {
            let (support, resting) = (support(), resting());
            column.set_block_id(support.x, support.y, support.z, StateId::from_state_str("minecraft:slime_block").unwrap());
            column.set_block_id(resting.x, resting.y, resting.z, StateId::from_state_str("minecraft:suspicious_gravel").unwrap());
            column.set_block_entities(vec![(
                resting,
                BlockEntity::Brushable(Box::new(lodestone_server::Brushable::with_loot("minecraft:archaeology/trail_ruins_rare", 77))),
            )]);
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
        self.column(x.div_euclid(16), z.div_euclid(16)).block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
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

/// Breaking the block under suspicious gravel makes it fall; the fall carries the unrolled loot
/// table, and the cell it left holds no entity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_suspicious_block_falls_and_keeps_its_loot() {
    let mut h = Harness::start(FallWorld::default(), (0, 0)).await;
    h.stand_over(BlockPos::new(2, FLOOR, 4)).await;
    for action in [0, 2] {
        h.session.sequence += 1;
        let mut writer = Writer::default();
        PlayerAction { action, pos: packed(support()), direction: 1, sequence: h.session.sequence }.encode(&mut writer, CTX).unwrap();
        h.session.send(play::serverbound::PLAYER_ACTION, writer.as_slice()).await;
    }
    let landing = BlockPos::new(2, FLOOR + 1, 2);
    let started = tokio::time::Instant::now();
    loop {
        let landed = h.server.block_entities().expect("registry").with(|registry| match registry.get(landing) {
            Some(BlockEntity::Brushable(entity)) => entity.loot().map(|loot| (loot.table.clone(), loot.seed)),
            _ => None,
        });
        if let Some((table, seed)) = landed {
            assert_eq!((table.as_str(), seed), ("minecraft:archaeology/trail_ruins_rare", 77));
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "the block never landed with its loot");
        h.session.read(Duration::from_millis(50)).await;
    }
    assert!(h.server.block_entities().expect("registry").with(|registry| registry.get(resting()).is_none()), "the origin holds no entity");
    h.shutdown().await;
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-archaeology-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch world dir");
    dir
}

fn open_world(dir: &std::path::Path) -> lodestone_server::region_source::RegionChunkSource<lodestone_server::Terrain263ChunkSource> {
    lodestone_server::region_source::RegionChunkSource::new(
        overworld_chunk_source_of_type(SEED, WorldType::Overworld),
        dir,
        lodestone_server::dimension::Dimension::Overworld,
        -64,
        384,
    )
    .expect("open world")
}

/// A generated block's unrolled loot table, and a block whose item was already rolled, each come
/// back from a second, independent world over the same directory exactly as they were saved.
#[test]
fn brushable_blocks_survive_a_close_and_reopen() {
    let target = find_target();
    let pos = target.pos;
    let (cx, cz) = (pos.x.div_euclid(16), pos.z.div_euclid(16));
    let dir = scratch("round-trip");
    // A second brushable block of the same chunk, rolled before the save.
    let source = overworld_chunk_source_of_type(SEED, WorldType::Overworld);
    let other = source
        .column(cx, cz)
        .block_entities()
        .iter()
        .map(|(at, _)| *at)
        .find(|at| *at != pos)
        .expect("the ruin holds more than one brushable block");
    let rolled = ItemStack::new(ResourceKey::from_str("minecraft:music_disc_relic").unwrap(), 1);
    let loot_seed;
    {
        let world = open_world(&dir);
        let column = world.column(cx, cz);
        loot_seed = match column.block_entities().iter().find(|(at, _)| *at == pos) {
            Some((_, BlockEntity::Brushable(entity))) => entity.loot().expect("generated loot").seed,
            other => panic!("expected a generated brushable block at {pos:?}, found {other:?}"),
        };
        assert_ne!(loot_seed, 0, "structure placement drew a loot seed");
        // Dirty the chunk the way a player's edit would, and roll one block's item.
        world.set_block(pos.x, pos.y + 10, pos.z, StateId::from_state_str("minecraft:stone").unwrap());
        world.block_entities().with(|registry| {
            registry.insert(other, BlockEntity::Brushable(Box::new(lodestone_server::Brushable::restore(None, Some(rolled.clone()), None))));
        });
        world.save_handle().save().expect("save");
    }
    let world = open_world(&dir);
    let _ = world.column(cx, cz);
    world.block_entities().with(|registry| {
        match registry.get(pos) {
            Some(BlockEntity::Brushable(entity)) => {
                let loot = entity.loot().expect("the unrolled table survives");
                assert_eq!((loot.table.as_str(), loot.seed), (target.table.as_str(), loot_seed));
                assert!(entity.item().is_none());
            }
            other => panic!("expected the saved brushable block at {pos:?}, found {other:?}"),
        }
        match registry.get(other) {
            Some(BlockEntity::Brushable(entity)) => {
                assert!(entity.loot().is_none(), "a rolled block has no table left");
                assert_eq!(entity.item(), Some(&rolled));
            }
            other_entity => panic!("expected the rolled brushable block at {other:?}, found {other_entity:?}"),
        }
    });
}
