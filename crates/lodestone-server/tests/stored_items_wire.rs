//! Shulker boxes, jukeboxes, chiseled bookshelves and decorated pots through a
//! live 26.2 connection: real use-item-on, container-click, container-close and
//! player-action packets in, block updates, block events, level events and slot
//! updates out, with the block-entity registry and a placed comparator as the
//! server-side witnesses.
//!
//! Expected values come from outside the code under test: slot arithmetic worked
//! out from the 3 x 2 shelf grid, comparator outputs from the reference song
//! files and from `floor(14 * fill) + 1`, the song id from the song's position in
//! the alphabetically ordered registry, and block ids from the reference
//! registry report for the wire's block ids.
//!
//! Controls, run by hand: removing the `apply_stored_item_use` call in
//! `use_item_on`, the shulker fill at placement, or the break drops in
//! `destroy_block` each fails the matching test here.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::{Ctx, Encode, Nbt, Reader, Writer};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ItemStack, ResourceKey, Vec3};
use lodestone_net::Connection;
use lodestone_server::commands::Effect;
use lodestone_server::{BlockEntity, ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::{configuration, handshaking, login, play};
use lodestone_v26_2::packets::game::{PlayerAction, UseItemOn};
use lodestone_v26_2::packets::handshake::Intention;
use lodestone_v26_2::packets::login::LoginHello;
use tokio::io::DuplexStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 776 };
const DEADLINE: Duration = Duration::from_secs(25);
const PLAYER: u128 = 0x5701;
const FACE_UP: i32 = 1;
const FACE_NORTH: i32 = 2;
const FACE_SOUTH: i32 = 3;
const FLOOR: i32 = 63;

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

/// A joined client: every packet read is acknowledged where the protocol needs
/// it and kept for inspection.
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

    /// Reads until the connection has been quiet for `quiet`.
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
    async fn click(&mut self, pos: BlockPos, face: i32, cursor: (f32, f32, f32)) -> usize {
        self.sequence += 1;
        let sequence = self.sequence;
        let from = self.log.len();
        let mut writer = Writer::default();
        UseItemOn {
            hand: 0,
            pos: packed(pos),
            face,
            cursor_x: cursor.0,
            cursor_y: cursor.1,
            cursor_z: cursor.2,
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
        self.settle(Duration::from_millis(120)).await;
        from
    }

    async fn look(&mut self, yaw: f32) {
        let mut writer = Writer::default();
        writer.f32(yaw);
        writer.f32(30.0);
        writer.u8(1);
        self.send(play::serverbound::MOVE_PLAYER_ROT, writer.as_slice()).await;
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

    async fn shift_click(&mut self, window: i32, slot: i16) {
        let mut writer = Writer::default();
        writer.var_i32(window);
        writer.var_i32(0);
        writer.i16(slot);
        writer.i8(0);
        writer.var_i32(1);
        writer.var_i32(0);
        writer.bool(false);
        self.send(play::serverbound::CONTAINER_CLICK, writer.as_slice()).await;
    }

    async fn close_window(&mut self, window: i32) {
        let mut writer = Writer::default();
        writer.var_i32(window);
        self.send(play::serverbound::CONTAINER_CLOSE, writer.as_slice()).await;
    }

    async fn dig(&mut self, pos: BlockPos) {
        for action in [0, 2] {
            self.sequence += 1;
            let mut writer = Writer::default();
            PlayerAction { action, pos: packed(pos), direction: 1, sequence: self.sequence }.encode(&mut writer, CTX).unwrap();
            self.send(play::serverbound::PLAYER_ACTION, writer.as_slice()).await;
        }
    }
}

struct Harness {
    server: IntegratedServer,
    world: World,
    session: Session,
}

impl Harness {
    async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// Starts a joined server over a world that `prepare` has edited first: the
    /// server keeps its own resident copy of terrain, so later edits through
    /// the source would not be seen.
    async fn start_with(prepare: impl FnOnce(&World)) -> Self {
        let world = World::default();
        prepare(&world);
        let (server, io) = IntegratedServer::open_in_memory_with_mobs(V770ServerProtocol, world.clone(), (-2..=2, -2..=2), (0, 0), 3);
        for (rule, value) in [("spawn_mobs", "false"), ("random_tick_speed", "0")] {
            server.world_state().set_rule(rule, value).expect("known rule");
        }
        let mut client = Connection::new(io);
        let mut writer = Writer::default();
        Intention { protocol_version: 776, host: "memory".into(), port: 25565, next_state: 2 }.encode(&mut writer, CTX).unwrap();
        client.write_packet(handshaking::serverbound::INTENTION, writer.as_slice()).await.unwrap();
        let mut writer = Writer::default();
        LoginHello { name: "Keeper".into(), profile_id: Uuid::from_u128(PLAYER) }.encode(&mut writer, CTX).unwrap();
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
        let mut harness = Self { server, world, session };
        harness.session.settle(Duration::from_millis(300)).await;
        harness
    }

    /// Gives the player a stack and returns the window-0 slot it landed in.
    async fn give(&mut self, item: &str, count: u32) -> i16 {
        let stack = ItemStack::new(ResourceKey::from_str(item).unwrap(), count);
        let from = self.session.log.len();
        let started = tokio::time::Instant::now();
        while !self.server.world_state().player_registry().push_effect(Uuid::from_u128(PLAYER), Effect::GiveItems(vec![stack.clone()])) {
            assert!(started.elapsed() < DEADLINE, "the player never registered");
            self.session.read(Duration::from_millis(20)).await;
        }
        self.session.until("the given stack", from, |log| log.iter().any(|(id, _)| *id == play::clientbound::CONTAINER_SET_SLOT)).await;
        let (_, payload) = self.session.log[from..].iter().find(|(id, _)| *id == play::clientbound::CONTAINER_SET_SLOT).unwrap();
        let mut reader = Reader::new(payload);
        let _window = reader.var_i32().unwrap();
        let _state = reader.var_i32().unwrap();
        reader.i16().unwrap()
    }

    fn entity(&self, pos: BlockPos) -> Option<BlockEntity> {
        self.server.block_entities().expect("block entities").with(|reg| reg.get(pos).map(clone_entity))
    }

    fn comparator_output(&self, pos: BlockPos) -> Option<u8> {
        match self.entity(pos)? {
            BlockEntity::Comparator { output } => Some(output),
            _ => None,
        }
    }

    async fn shutdown(self) {
        drop(self.session);
        self.server.shutdown().await;
    }
}

/// `BlockEntity` has no `Clone`; the cases here read three shapes.
fn clone_entity(entity: &BlockEntity) -> BlockEntity {
    match entity {
        BlockEntity::Container { id, slots } => BlockEntity::Container { id: id.clone(), slots: slots.clone() },
        BlockEntity::Comparator { output } => BlockEntity::Comparator { output: *output },
        BlockEntity::Opaque { id, nbt } => BlockEntity::Opaque { id: id.clone(), nbt: nbt.clone() },
        other => panic!("unexpected entity {other:?}"),
    }
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

fn stack_summary(stack: &Option<ItemStack>) -> Option<(String, u32)> {
    stack.as_ref().map(|s| (s.item.to_string(), s.count))
}

/// Block events in the log: position, kind, value, block registry id.
fn block_events(log: &[(i32, Vec<u8>)]) -> Vec<(BlockPos, u8, u8, i32)> {
    log.iter()
        .filter(|(id, _)| *id == play::clientbound::BLOCK_EVENT)
        .map(|(_, payload)| {
            let mut reader = Reader::new(payload);
            let pos = unpacked(reader.i64().unwrap());
            (pos, reader.u8().unwrap(), reader.u8().unwrap(), reader.var_i32().unwrap())
        })
        .collect()
}

/// Level events in the log: event, position, data.
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

/// Block updates in the log, latest per position last.
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

/// The latest value of `key` the log's block updates gave `pos`.
fn updated_flag(log: &[(i32, Vec<u8>)], pos: BlockPos, key: &str) -> Option<String> {
    block_updates(log).iter().rev().find(|(at, _)| *at == pos).and_then(|&(_, s)| property(s, key).map(str::to_owned))
}

fn opened_windows(log: &[(i32, Vec<u8>)]) -> Vec<i32> {
    log.iter().filter(|(id, _)| *id == play::clientbound::OPEN_SCREEN).map(|(_, p)| Reader::new(p).var_i32().unwrap()).collect()
}

fn has(log: &[(i32, Vec<u8>)], packet: i32) -> bool {
    log.iter().any(|(id, _)| *id == packet)
}

/// The block registry id this wire (protocol 776) uses for a block.
fn wire_block_id(name: &str) -> i32 {
    i32::from(lodestone_data::block::Block::from_name(name).expect("a block").registry_id())
}

async fn wait_for_output(h: &mut Harness, comparator: BlockPos, expected: u8) {
    let started = tokio::time::Instant::now();
    while h.comparator_output(comparator) != Some(expected) {
        assert!(started.elapsed() < DEADLINE, "the comparator should read {expected}, saw {:?}", h.comparator_output(comparator));
        h.session.read(Duration::from_millis(50)).await;
    }
}

/// A shulker box carries its contents through place, break, pick up and place:
/// three stacks go in through the open window, the lid events bracket the
/// window, and the same three stacks come back out of the second placement.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shulker_box_keeps_three_stacks_across_place_break_place() {
    let mut h = Harness::start().await;
    let first = BlockPos::new(2, FLOOR + 1, 2);
    let second = BlockPos::new(4, FLOOR + 1, 2);

    // Place: the entity exists with 27 empty slots.
    let slot = h.give("minecraft:shulker_box", 1).await;
    assert_eq!(slot, 36, "the first hotbar slot");
    h.session.click(BlockPos::new(2, FLOOR, 2), FACE_UP, (0.5, 1.0, 0.5)).await;
    assert_eq!(h.world.at(first).block().name(), "minecraft:shulker_box");
    let Some(BlockEntity::Container { slots, .. }) = h.entity(first) else { panic!("a placed shulker box has a container entity") };
    assert_eq!(slots.len(), 27);
    assert!(slots.iter().all(Option::is_none));

    // Fill: three stacks of three kinds, shift-clicked in through the open window.
    let given = [
        (h.give("minecraft:diamond", 5).await, "minecraft:diamond", 5),
        (h.give("minecraft:stick", 7).await, "minecraft:stick", 7),
        (h.give("minecraft:apple", 3).await, "minecraft:apple", 3),
    ];
    let from = h.session.click(first, FACE_UP, (0.5, 1.0, 0.5)).await;
    let window = *opened_windows(&h.session.log[from..]).first().expect("the box opens a window");
    let lid = block_events(&h.session.log[from..]);
    assert_eq!(lid.iter().map(|&(pos, kind, value, _)| (pos, kind, value)).collect::<Vec<_>>(), [(first, 1, 1)], "one viewer opens the lid");
    assert_eq!(lid[0].3, wire_block_id("minecraft:shulker_box"), "the event names the shulker box block");
    assert!(has(&h.session.log[from..], play::clientbound::SOUND), "the lid opening is audible");
    for (window0_slot, _, _) in given {
        // Window 0 hotbar slot 36 + n is slot 54 + n of a 27-slot container window.
        h.session.shift_click(window, 54 + (window0_slot - 36)).await;
    }
    h.session.settle(Duration::from_millis(400)).await;
    let Some(BlockEntity::Container { slots, .. }) = h.entity(first) else { panic!("entity") };
    assert_eq!(
        slots.iter().take(4).map(stack_summary).collect::<Vec<_>>(),
        [
            Some(("minecraft:diamond".to_owned(), 5)),
            Some(("minecraft:stick".to_owned(), 7)),
            Some(("minecraft:apple".to_owned(), 3)),
            None,
        ]
    );
    let from = h.session.log.len();
    h.session.close_window(window).await;
    h.session.until("the lid closing", from, |log| !block_events(log).is_empty()).await;
    assert_eq!(block_events(&h.session.log[from..]).iter().map(|e| (e.1, e.2)).collect::<Vec<_>>(), [(1, 0)], "the last viewer leaves");

    // Break: the box drops as one item that carries the stacks.
    let pick = h.give("minecraft:diamond_pickaxe", 1).await;
    h.session.select(pick - 36).await;
    h.session.dig(first).await;
    let started = tokio::time::Instant::now();
    while h.world.at(first) != StateId::AIR {
        assert!(started.elapsed() < DEADLINE, "the box never broke");
        h.session.read(Duration::from_millis(50)).await;
    }
    let started = tokio::time::Instant::now();
    while h.entity(first).is_some() {
        assert!(started.elapsed() < DEADLINE, "the entity must go with the block");
        h.session.read(Duration::from_millis(50)).await;
    }
    let mobs = h.server.mobs().expect("mob sim").clone();
    let started = tokio::time::Instant::now();
    let dropped = loop {
        let found = mobs.with(|sim| {
            let probe = Vec3::new(2.5, f64::from(FLOOR) + 1.0, 2.5);
            sim.items_within_pickup_range(probe).into_iter().map(|(_, stack)| stack).collect::<Vec<_>>()
        });
        if let Some(stack) = found.into_iter().find(|s| s.item.to_string() == "minecraft:shulker_box") {
            break stack;
        }
        assert!(started.elapsed() < DEADLINE, "no shulker item dropped");
        h.session.read(Duration::from_millis(50)).await;
    };
    assert_eq!(dropped.components.container.iter().flatten().count(), 3, "the dropped item holds the three stacks");

    // Pick up by walking onto it, then place again.
    let from = h.session.log.len();
    h.session.walk_to(2.5, 2.5).await;
    let started = tokio::time::Instant::now();
    while mobs.with(|sim| sim.item_count()) != 0 {
        assert!(started.elapsed() < DEADLINE, "the item was never picked up");
        h.session.read(Duration::from_millis(50)).await;
    }
    h.session.settle(Duration::from_millis(200)).await;
    let landed = h.session.log[from..]
        .iter()
        .filter(|(id, _)| *id == play::clientbound::CONTAINER_SET_SLOT)
        .map(|(_, p)| {
            let mut r = Reader::new(p);
            let _ = (r.var_i32(), r.var_i32());
            r.i16().unwrap()
        })
        .last()
        .expect("the pickup reaches a slot");
    h.session.select(landed - 36).await;
    h.session.click(BlockPos::new(4, FLOOR, 2), FACE_UP, (0.5, 1.0, 0.5)).await;
    assert_eq!(h.world.at(second).block().name(), "minecraft:shulker_box");
    let Some(BlockEntity::Container { slots, .. }) = h.entity(second) else { panic!("second placement has an entity") };
    assert_eq!(
        slots.iter().take(4).map(stack_summary).collect::<Vec<_>>(),
        [
            Some(("minecraft:diamond".to_owned(), 5)),
            Some(("minecraft:stick".to_owned(), 7)),
            Some(("minecraft:apple".to_owned(), 3)),
            None,
        ],
        "the same three stacks come back"
    );
    h.shutdown().await;
}

/// A disc goes in with a play event carrying its registry id, the comparator
/// beside it reads the song's output, and an empty-handed click ejects the disc.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_jukebox_plays_a_disc_and_ejects_it() {
    let jukebox = BlockPos::new(2, FLOOR + 1, 2);
    let mut h = Harness::start_with(|world| world.set_block(jukebox.x, jukebox.y, jukebox.z, state("minecraft:jukebox[has_record=false]"))).await;

    // A comparator west of the jukebox. It reads the block it faces, and a
    // placed diode faces the player, so the player looks west.
    h.give("minecraft:comparator", 1).await;
    h.session.look(90.0).await;
    h.session.click(BlockPos::new(1, FLOOR, 2), FACE_UP, (0.5, 1.0, 0.5)).await;
    let comparator = BlockPos::new(1, FLOOR + 1, 2);
    assert_eq!(property(h.world.at(comparator), "facing"), Some("east"));
    assert_eq!(h.comparator_output(comparator), Some(0), "an empty jukebox reads nothing");

    let slot = h.give("minecraft:music_disc_cat", 1).await;
    h.session.select(slot - 36).await;
    let from = h.session.click(jukebox, FACE_UP, (0.5, 1.0, 0.5)).await;
    let log = &h.session.log[from..];
    // "cat" is the sixth song of the alphabetically ordered registry:
    // 11, 13, 5, blocks, bounce, cat.
    assert_eq!(level_events(log), [(1010, jukebox, 5)]);
    assert_eq!(updated_flag(log, jukebox, "has_record"), Some("true".to_owned()));
    let Some(BlockEntity::Opaque { nbt, .. }) = h.entity(jukebox) else { panic!("jukebox entity") };
    let held = field(&nbt, "RecordItem").expect("the disc is stored");
    assert_eq!(field(held, "id"), Some(&Nbt::String("minecraft:music_disc_cat".into())));
    let started = tokio::time::Instant::now();
    while h.comparator_output(comparator) != Some(2) {
        assert!(started.elapsed() < DEADLINE, "the comparator never read the disc: {:?}", h.comparator_output(comparator));
        h.session.read(Duration::from_millis(50)).await;
    }

    // Empty-handed: the disc comes out and the song stops.
    h.session.select(8).await;
    let from = h.session.click(jukebox, FACE_UP, (0.5, 1.0, 0.5)).await;
    let log = &h.session.log[from..];
    assert_eq!(level_events(log), [(1011, jukebox, 0)]);
    assert_eq!(updated_flag(log, jukebox, "has_record"), Some("false".to_owned()));
    let mobs = h.server.mobs().expect("mob sim").clone();
    let started = tokio::time::Instant::now();
    loop {
        let found = mobs.with(|sim| sim.items_within_pickup_range(Vec3::new(2.5, f64::from(FLOOR) + 1.5, 2.5)));
        if found.iter().any(|(_, s)| s.item.to_string() == "minecraft:music_disc_cat") {
            break;
        }
        assert!(started.elapsed() < DEADLINE, "no disc was ejected");
        h.session.read(Duration::from_millis(50)).await;
    }
    let started = tokio::time::Instant::now();
    while h.comparator_output(comparator) != Some(0) {
        assert!(started.elapsed() < DEADLINE, "the comparator kept reading the ejected disc");
        h.session.read(Duration::from_millis(50)).await;
    }
    h.shutdown().await;
}

/// The slot comes from the hit position on the front face; the book lands there,
/// the occupied flag and the comparator follow, and the same click empty-handed
/// takes it back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chiseled_bookshelf_takes_a_book_at_the_slot_the_hit_selects() {
    let shelf = BlockPos::new(2, FLOOR + 1, 2);
    let mut h = Harness::start_with(|world| {
        world.set_block(
            shelf.x,
            shelf.y,
            shelf.z,
            state("minecraft:chiseled_bookshelf[facing=south,slot_0_occupied=false,slot_1_occupied=false,slot_2_occupied=false,slot_3_occupied=false,slot_4_occupied=false,slot_5_occupied=false]"),
        );
    })
    .await;
    h.give("minecraft:comparator", 1).await;
    h.session.look(90.0).await;
    h.session.click(BlockPos::new(1, FLOOR, 2), FACE_UP, (0.5, 1.0, 0.5)).await;
    let comparator = BlockPos::new(1, FLOOR + 1, 2);
    assert_eq!(property(h.world.at(comparator), "facing"), Some("east"), "it reads the shelf to its east");
    let slot = h.give("minecraft:book", 3).await;
    h.session.select(slot - 36).await;
    let flags = |h: &Harness| -> String {
        (0..6).map(|i| if property(h.world.at(shelf), &format!("slot_{i}_occupied")) == Some("true") { '1' } else { '0' }).collect()
    };

    // The north face is the back: a click there selects nothing.
    h.session.click(shelf, FACE_NORTH, (0.5, 0.5, 0.0)).await;
    assert_eq!(flags(&h), "000000", "only the front face takes books");

    // Facing south, the viewer's left is -x, so the column is x in thirds:
    // x = 0.9, y = 0.2 is the right column of the bottom row: slot 5.
    h.session.click(shelf, FACE_SOUTH, (0.9, 0.2, 1.0)).await;
    assert_eq!(flags(&h), "000001");
    wait_for_output(&mut h, comparator, 6).await;
    let Some(BlockEntity::Opaque { nbt, .. }) = h.entity(shelf) else { panic!("shelf entity") };
    assert_eq!(field(&nbt, "last_interacted_slot"), Some(&Nbt::Int(5)));
    // x = 0.1, y = 0.8 is the left column of the top row: slot 0.
    h.session.click(shelf, FACE_SOUTH, (0.1, 0.8, 1.0)).await;
    assert_eq!(flags(&h), "100001");
    // The comparator reads the slot touched last, plus one.
    wait_for_output(&mut h, comparator, 1).await;
    // An occupied slot with a book in hand takes the book out instead.
    let from = h.session.click(shelf, FACE_SOUTH, (0.9, 0.2, 1.0)).await;
    assert_eq!(flags(&h), "100000");
    wait_for_output(&mut h, comparator, 6).await;
    assert!(has(&h.session.log[from..], play::clientbound::CONTAINER_SET_SLOT), "the book returns to the inventory");
    let Some(BlockEntity::Opaque { nbt, .. }) = h.entity(shelf) else { panic!("shelf entity") };
    assert_eq!(field(&nbt, "last_interacted_slot"), Some(&Nbt::Int(5)), "taking updates the last slot too");
    h.shutdown().await;
}

/// One kind of item stacks in a pot up to its stack size, wobbling each time;
/// the comparator beside it reads the fill; another kind is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decorated_pot_stacks_one_item_and_the_comparator_reads_it() {
    let pot = BlockPos::new(2, FLOOR + 1, 2);
    let mut h = Harness::start_with(|world| {
        world.set_block(pot.x, pot.y, pot.z, state("minecraft:decorated_pot[cracked=false,facing=north,waterlogged=false]"));
    })
    .await;
    h.give("minecraft:comparator", 1).await;
    h.session.look(90.0).await;
    h.session.click(BlockPos::new(1, FLOOR, 2), FACE_UP, (0.5, 1.0, 0.5)).await;
    let comparator = BlockPos::new(1, FLOOR + 1, 2);
    assert_eq!(property(h.world.at(comparator), "facing"), Some("east"), "it reads the pot to its east");
    assert_eq!(h.comparator_output(comparator), Some(0));

    let pearls = h.give("minecraft:ender_pearl", 16).await;
    let spare = h.give("minecraft:ender_pearl", 1).await;
    let sticks = h.give("minecraft:stick", 3).await;
    h.session.select(pearls - 36).await;
    let pot_count = |h: &Harness| -> Option<(String, i64)> {
        let Some(BlockEntity::Opaque { nbt, .. }) = h.entity(pot) else { return None };
        let item = field(&nbt, "item")?;
        let Some(Nbt::Int(count)) = field(item, "count") else { return None };
        let Some(Nbt::String(id)) = field(item, "id") else { return None };
        Some((id.clone(), i64::from(*count)))
    };

    let from = h.session.click(pot, FACE_UP, (0.5, 1.0, 0.5)).await;
    assert_eq!(pot_count(&h), Some(("minecraft:ender_pearl".to_owned(), 1)));
    assert_eq!(block_events(&h.session.log[from..]).iter().map(|e| (e.1, e.2)).collect::<Vec<_>>(), [(1, 0)], "a positive wobble");
    assert_eq!(block_events(&h.session.log[from..])[0].3, wire_block_id("minecraft:decorated_pot"));
    let started = tokio::time::Instant::now();
    while h.comparator_output(comparator) != Some(1) {
        assert!(started.elapsed() < DEADLINE, "one pearl should read 1, saw {:?}", h.comparator_output(comparator));
        h.session.read(Duration::from_millis(50)).await;
    }

    for _ in 0..15 {
        h.session.click(pot, FACE_UP, (0.5, 1.0, 0.5)).await;
    }
    assert_eq!(pot_count(&h), Some(("minecraft:ender_pearl".to_owned(), 16)), "pearls stack to sixteen");
    // floor(14 * 16 / 16) + 1 = 15.
    let started = tokio::time::Instant::now();
    while h.comparator_output(comparator) != Some(15) {
        assert!(started.elapsed() < DEADLINE, "a full pot reads 15, saw {:?}", h.comparator_output(comparator));
        h.session.read(Duration::from_millis(50)).await;
    }

    // The seventeenth pearl, from a stack that still has one, is refused with
    // the negative wobble.
    h.session.select(spare - 36).await;
    let from = h.session.click(pot, FACE_UP, (0.5, 1.0, 0.5)).await;
    assert_eq!(pot_count(&h), Some(("minecraft:ender_pearl".to_owned(), 16)));
    assert_eq!(block_events(&h.session.log[from..]).iter().map(|e| (e.1, e.2)).collect::<Vec<_>>(), [(1, 1)]);
    // And so is another kind.
    h.session.select(sticks - 36).await;
    h.session.click(pot, FACE_UP, (0.5, 1.0, 0.5)).await;
    assert_eq!(pot_count(&h), Some(("minecraft:ender_pearl".to_owned(), 16)));
    h.shutdown().await;
}
