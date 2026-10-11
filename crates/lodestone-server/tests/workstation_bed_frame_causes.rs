//! What a menu, a bed and an item frame do about the world and the people
//! around them, driven by the real causes (a block removal, a walk away, a
//! click, a thrown snowball) through live protocol-776 connections to one
//! hosted world.
//!
//! Expected values come from outside the code under test: a positional menu
//! stays valid only while its block is still one of its menu's blocks and the
//! eye is within 4.5 + 4.0 blocks of the block's cube; closing hands the grid
//! contents back to the inventory; an occupied bed answers "This bed is
//! occupied"; a monster keeps a player from sleeping within 8 x 5 x 8 of the
//! bed unless it is a neutral one not angry at that player; a fixed item frame
//! yields only to a creative player's projectile (or a creative swing); frames
//! are never ticked as burning entities. Wire layouts are the container-close
//! packet (one VarInt window id) and the container-click packet.
//!
//! Controls, run by hand: dropping the positional arm of `menu_still_valid`,
//! the range test in `within_menu_range`, the `close_menu` call in
//! `publish_open_container`, the occupied flag written at lay-down and cleared
//! in `tick`'s vacated-bed drain, the `RestRule` table, and the
//! `creative_shooter` plumbing each fail the matching test here.


use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::{Ctx, Encode, Reader, Writer};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ItemStack, ResourceKey};
use lodestone_net::Connection;
use lodestone_server::commands::Effect;
use lodestone_server::{ChunkColumn, ChunkSource, EntitySnapshot, IntegratedServer};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::{configuration, handshaking, login, play};
use lodestone_v26_2::packets::game::{Attack, ChatCommand, PlayerCommand, UseItem, UseItemOn};
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

    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.server.mobs().expect("mob sim").with(|sim| sim.snapshots())
    }

    async fn shutdown(self) {
        drop(self.sessions);
        self.server.shutdown().await;
    }
}

fn stacked(name: &str, count: u32) -> ItemStack {
    ItemStack::new(ResourceKey::from_str(name).unwrap(), count)
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

/// Every block change in `log`, whether it arrived alone or in a section batch.
fn block_updates(log: &[(i32, Vec<u8>)]) -> Vec<(BlockPos, StateId)> {
    let mut changes = Vec::new();
    for (id, payload) in log {
        let mut reader = Reader::new(payload);
        if *id == play::clientbound::BLOCK_UPDATE {
            let pos = unpacked(reader.i64().unwrap());
            changes.push((pos, StateId::new(u32::try_from(reader.var_i32().unwrap()).unwrap()).expect("wire state")));
        } else if *id == play::clientbound::SECTION_BLOCKS_UPDATE {
            let section = reader.i64().unwrap();
            let (sx, sy, sz) = (section >> 42, section << 44 >> 44, section << 22 >> 42);
            for _ in 0..reader.var_i32().unwrap() {
                let entry = reader.var_i64().unwrap();
                let (lx, lz, ly) = ((entry >> 8) & 15, (entry >> 4) & 15, entry & 15);
                let pos = BlockPos::new((sx * 16 + lx) as i32, (sy * 16 + ly) as i32, (sz * 16 + lz) as i32);
                changes.push((pos, StateId::new(u32::try_from(entry >> 12).unwrap()).expect("wire state")));
            }
        }
    }
    changes
}

// ---- shared observation helpers -----------------------------------------------

impl Session {
    /// A container click on `window` (state id unchecked by the server).
    async fn container_click(&mut self, window: i32, slot: i16, click_type: i32) {
        let mut writer = Writer::default();
        writer.var_i32(window);
        writer.var_i32(0);
        writer.i16(slot);
        writer.u8(0);
        writer.var_i32(click_type);
        writer.var_i32(0);
        writer.u8(0);
        self.send(play::serverbound::CONTAINER_CLICK, writer.as_slice()).await;
    }

    async fn attack(&mut self, entity_id: i32) {
        let mut writer = Writer::default();
        Attack { entity_id }.encode(&mut writer, CTX).unwrap();
        self.send(play::serverbound::ATTACK, writer.as_slice()).await;
    }

    async fn get_out_of_bed(&mut self) {
        let mut writer = Writer::default();
        PlayerCommand { entity_id: 0, action: 0, data: 0 }.encode(&mut writer, CTX).unwrap();
        self.send(play::serverbound::PLAYER_COMMAND, writer.as_slice()).await;
    }

    async fn use_item(&mut self, yaw: f32, pitch: f32) {
        self.sequence += 1;
        let mut writer = Writer::default();
        UseItem { hand: 0, sequence: self.sequence, yaw, pitch }.encode(&mut writer, CTX).unwrap();
        self.send(play::serverbound::USE_ITEM, writer.as_slice()).await;
    }
}

/// The text of every action-bar message in `log`: a system chat whose trailing
/// overlay flag is set.
fn action_bars(log: &[(i32, Vec<u8>)]) -> Vec<String> {
    log.iter()
        .filter(|(id, payload)| *id == play::clientbound::SYSTEM_CHAT && payload.last() == Some(&1))
        .map(|(_, payload)| String::from_utf8_lossy(payload).into_owned())
        .collect()
}

fn action_bar_says(log: &[(i32, Vec<u8>)], text: &str) -> bool {
    action_bars(log).iter().any(|bar| bar.contains(text))
}

/// `(window, slot, count)` of every slot update in `log`.
fn slot_updates(log: &[(i32, Vec<u8>)]) -> Vec<(i32, i16, i32)> {
    log.iter()
        .filter(|(id, _)| *id == play::clientbound::CONTAINER_SET_SLOT)
        .map(|(_, payload)| {
            let mut reader = Reader::new(payload);
            let window = reader.var_i32().unwrap();
            let _state = reader.var_i32().unwrap();
            let slot = reader.i16().unwrap();
            (window, slot, reader.var_i32().unwrap())
        })
        .collect()
}

fn occupied(state: StateId) -> Option<bool> {
    state.properties().iter().find_map(|(k, v)| (*k == "occupied").then(|| *v == "true"))
}

/// Whether `log` carries a block update that set `pos` to an occupied (or, with
/// `flag` false, a free) bed half.
fn bed_flag_update(log: &[(i32, Vec<u8>)], pos: BlockPos, flag: bool) -> bool {
    block_updates(log).into_iter().any(|(at, state)| at == pos && occupied(state) == Some(flag))
}

// ---- positional menus -----------------------------------------------------------

const MENU_BLOCKS: [&str; 7] = [
    "minecraft:crafting_table",
    "minecraft:anvil",
    "minecraft:grindstone",
    "minecraft:stonecutter",
    "minecraft:smithing_table",
    "minecraft:loom",
    "minecraft:enchanting_table",
];

fn menu_cell(index: usize) -> BlockPos {
    // Two rows within reach of a player standing at (3.5, 3.0).
    BlockPos::new(1 + 2 * (index as i32 % 4), FLOOR + 1, if index < 4 { 1 } else { 5 })
}

async fn menu_world() -> Harness {
    let mut h = Harness::start(&["Breaker", "Viewer"], |world| {
        for (index, block) in MENU_BLOCKS.iter().enumerate() {
            let at = menu_cell(index);
            world.set_block(at.x, at.y, at.z, state(block));
        }
    })
    .await;
    h.sessions[1].walk_to(3.5, 3.0).await;
    h.pump(Duration::from_millis(200)).await;
    h
}

/// Opens the menu at `at` for the viewer and returns its window id.
async fn open_menu(h: &mut Harness, at: BlockPos) -> i32 {
    let from = h.sessions[1].log.len();
    h.sessions[1].click(at).await;
    let windows = opened_windows(&h.sessions[1].log[from..]);
    let [window] = windows[..] else { panic!("expected one window at {at:?}, got {windows:?}") };
    window
}

async fn remove_block(h: &mut Harness, at: BlockPos) {
    h.sessions[0].command(&format!("setblock {} {} {} minecraft:air destroy", at.x, at.y, at.z)).await;
}

/// Every positional menu's window closes on the viewer's connection when
/// another player removes its block, and not before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_the_block_closes_every_positional_menu() {
    let mut h = menu_world().await;
    for (index, block) in MENU_BLOCKS.iter().enumerate() {
        let at = menu_cell(index);
        let from = h.sessions[1].log.len();
        let window = open_menu(&mut h, at).await;
        h.pump(Duration::from_millis(300)).await;
        assert!(closed_windows(&h.sessions[1].log[from..]).is_empty(), "{block}: the window closed with nothing happening");
        remove_block(&mut h, at).await;
        h.sessions[1].until(&format!("{block}: the close packet"), from, |log| has(log, play::clientbound::CONTAINER_CLOSE)).await;
        assert_eq!(closed_windows(&h.sessions[1].log[from..]), [window], "{block}: the close names the viewer's window");
    }
    h.shutdown().await;
}

/// Replacing the block with a different one closes the menu; turning an anvil
/// into a damaged anvil does not, because the menu's block is the anvil family.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacing_the_block_closes_the_menu_unless_it_is_the_same_family() {
    let mut h = menu_world().await;

    let table = menu_cell(0);
    let from = h.sessions[1].log.len();
    let window = open_menu(&mut h, table).await;
    h.sessions[0].command(&format!("setblock {} {} {} minecraft:stone", table.x, table.y, table.z)).await;
    h.sessions[1].until("the close after a replacement", from, |log| has(log, play::clientbound::CONTAINER_CLOSE)).await;
    assert_eq!(closed_windows(&h.sessions[1].log[from..]), [window]);

    let anvil = menu_cell(1);
    let from = h.sessions[1].log.len();
    let window = open_menu(&mut h, anvil).await;
    h.sessions[0].command(&format!("setblock {} {} {} minecraft:damaged_anvil", anvil.x, anvil.y, anvil.z)).await;
    h.wait("the anvil to be damaged", |h| h.world.at(anvil) != state("minecraft:anvil")).await;
    h.pump(Duration::from_millis(400)).await;
    assert!(closed_windows(&h.sessions[1].log[from..]).is_empty(), "a damaged anvil is still the anvil family");
    remove_block(&mut h, anvil).await;
    h.sessions[1].until("the close after the anvil goes", from, |log| has(log, play::clientbound::CONTAINER_CLOSE)).await;
    assert_eq!(closed_windows(&h.sessions[1].log[from..]), [window]);
    h.shutdown().await;
}

/// The menu closes once the player's eye is more than 4.5 + 4.0 blocks from
/// the block's cube, and stays open short of that.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn walking_out_of_reach_closes_a_positional_menu() {
    let mut h = menu_world().await;
    for index in [0, 1] {
        let at = menu_cell(index);
        let from = h.sessions[1].log.len();
        let window = open_menu(&mut h, at).await;
        // Eye 6.2 blocks from the cube's nearest point: well inside 8.5.
        h.sessions[0].command("tp Viewer 3.5 64 8.0").await;
        h.pump(Duration::from_millis(500)).await;
        assert!(closed_windows(&h.sessions[1].log[from..]).is_empty(), "{}: closed within reach", MENU_BLOCKS[index]);
        h.sessions[0].command("tp Viewer 3.5 64 40.0").await;
        h.sessions[1].until("the close after walking away", from, |log| has(log, play::clientbound::CONTAINER_CLOSE)).await;
        assert_eq!(closed_windows(&h.sessions[1].log[from..]), [window], "{}", MENU_BLOCKS[index]);
        h.sessions[0].command("tp Viewer 3.5 64 3.0").await;
        h.pump(Duration::from_millis(400)).await;
    }
    h.shutdown().await;
}

/// What sits in a crafting or anvil grid comes back to the inventory when the
/// server closes the window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_crafting_or_anvil_grid_returns_its_items() {
    let mut h = menu_world().await;
    // (block index, item, count, menu slot of window-0 slot 9, first grid slot)
    for (index, item, count, offset, grid) in [(0usize, "minecraft:oak_planks", 5u32, 1i16, 1i16), (1, "minecraft:cobblestone", 7, -6, 0)] {
        let slot = h.give(1, stacked(item, count)).await;
        let at = menu_cell(index);
        let from = h.sessions[1].log.len();
        let window = open_menu(&mut h, at).await;
        h.sessions[1].container_click(window, slot + offset, 0).await;
        h.sessions[1].container_click(window, grid, 0).await;
        h.sessions[1].settle(Duration::from_millis(300)).await;
        let before_close = h.sessions[1].log.len();
        remove_block(&mut h, at).await;
        h.sessions[1].until("the close packet", from, |log| has(log, play::clientbound::CONTAINER_CLOSE)).await;
        h.sessions[1].until("the returned stack", before_close, |log| slot_updates(log).iter().any(|&(w, _, n)| w == 0 && n == count as i32)).await;
        // Until the close, the stack sits in the grid: no window-0 slot holds them.
        assert!(
            !slot_updates(&h.sessions[1].log[from..before_close]).iter().any(|&(w, _, n)| w == 0 && n == count as i32),
            "{}: the stack came back before the window closed",
            MENU_BLOCKS[index]
        );
        // Put the block back for nothing else to depend on this one.
        h.sessions[0].command(&format!("setblock {} {} {} {}", at.x, at.y, at.z, MENU_BLOCKS[index])).await;
        h.pump(Duration::from_millis(300)).await;
    }
    h.shutdown().await;
}

// ---- beds ------------------------------------------------------------------------

const BED_FOOT: BlockPos = BlockPos { x: 3, y: FLOOR + 1, z: 2 };
const BED_HEAD: BlockPos = BlockPos { x: 4, y: FLOOR + 1, z: 2 };

fn bed_state(part: &str, occupied: bool) -> StateId {
    state(&format!("minecraft:red_bed[facing=east,occupied={occupied},part={part}]"))
}

async fn bed_world(occupied: bool) -> Harness {
    let mut h = Harness::start(&["Sleeper", "Bystander"], |world| {
        world.set_block(BED_FOOT.x, BED_FOOT.y, BED_FOOT.z, bed_state("foot", occupied));
        world.set_block(BED_HEAD.x, BED_HEAD.y, BED_HEAD.z, bed_state("head", occupied));
    })
    .await;
    for session in &mut h.sessions {
        session.walk_to(3.5, 3.0).await;
    }
    h.sessions[0].command("time set 18000").await;
    h.pump(Duration::from_millis(800)).await;
    h
}

const OCCUPIED_MESSAGE: &str = "This bed is occupied";
const MONSTERS_MESSAGE: &str = "there are monsters nearby";

/// Right-clicks the bed's head as `who` and reports whether they lay down.
async fn try_sleep(h: &mut Harness, who: usize) -> bool {
    // A lone sleeper can carry the night to morning, so make it night again.
    h.sessions[0].command("time set 18000").await;
    h.pump(Duration::from_millis(250)).await;
    let from = h.sessions[who].log.len();
    h.sessions[who].click(BED_HEAD).await;
    h.sessions[who].settle(Duration::from_millis(200)).await;
    bed_flag_update(&h.sessions[who].log[from..], BED_HEAD, true)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_occupied_bed_is_refused_with_the_action_bar_message() {
    let mut h = bed_world(true).await;
    // Either half names the bed.
    for half in [BED_HEAD, BED_FOOT] {
        let from = h.sessions[0].log.len();
        h.sessions[0].click(half).await;
        assert!(action_bar_says(&h.sessions[0].log[from..], OCCUPIED_MESSAGE), "{half:?}: {:?}", action_bars(&h.sessions[0].log[from..]));
    }
    h.shutdown().await;

    // Control: the same bed free, at the same hour, takes the sleeper and says nothing of occupancy.
    let mut h = bed_world(false).await;
    let from = h.sessions[0].log.len();
    assert!(try_sleep(&mut h, 0).await, "a free bed at night takes the sleeper");
    assert!(!action_bar_says(&h.sessions[0].log[from..], OCCUPIED_MESSAGE));
    h.shutdown().await;
}

/// Lying down marks both halves occupied for everyone, a second player is
/// refused, and getting up frees the bed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bed_is_occupied_while_someone_lies_in_it() {
    let mut h = bed_world(false).await;
    assert!(try_sleep(&mut h, 0).await);
    h.wait("both halves to read occupied on the bystander", |h| {
        bed_flag_update(&h.sessions[1].log, BED_HEAD, true) && bed_flag_update(&h.sessions[1].log, BED_FOOT, true)
    })
    .await;
    assert_eq!(h.world.at(BED_HEAD), bed_state("head", true));

    let from = h.sessions[1].log.len();
    h.sessions[1].click(BED_HEAD).await;
    assert!(action_bar_says(&h.sessions[1].log[from..], OCCUPIED_MESSAGE), "{:?}", action_bars(&h.sessions[1].log[from..]));

    let from = h.sessions[1].log.len();
    h.sessions[0].get_out_of_bed().await;
    h.sessions[1].until("the bed to read free again", from, |log| bed_flag_update(log, BED_HEAD, false) && bed_flag_update(log, BED_FOOT, false)).await;
    assert_eq!(h.world.at(BED_HEAD), bed_state("head", false));
    assert!(try_sleep(&mut h, 1).await, "the freed bed takes the next sleeper");
    h.shutdown().await;
}

async fn summon(h: &mut Harness, entity: &str, x: f64, z: f64) {
    h.sessions[0].command(&format!("summon {entity} {x} 64 {z}")).await;
    let kind = entity.to_owned();
    h.wait(&format!("the {kind}"), |h| h.snapshots().iter().any(|s| s.entity_type.to_string() == kind)).await;
}

fn entity_id(h: &Harness, entity: &str) -> i32 {
    h.snapshots().iter().find(|s| s.entity_type.to_string() == entity).unwrap_or_else(|| panic!("no {entity}")).id
}

async fn get_up(h: &mut Harness, who: usize) {
    let from = h.sessions[who].log.len();
    h.sessions[who].get_out_of_bed().await;
    h.sessions[who].until("the bed to read free", from, |log| bed_flag_update(log, BED_HEAD, false)).await;
}

/// A monster within 8 x 5 x 8 of the bed keeps the player awake; a distant one
/// does not; a mob in the monster spawn category that is not a monster by
/// lineage (a slime) never does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nearby_monster_keeps_the_player_awake() {
    let mut h = bed_world(false).await;
    summon(&mut h, "minecraft:zombie", 40.5, 3.5).await;
    assert!(try_sleep(&mut h, 0).await, "a zombie 36 blocks away does not matter");
    get_up(&mut h, 0).await;

    summon(&mut h, "minecraft:slime", 6.5, 4.5).await;
    assert!(try_sleep(&mut h, 0).await, "a slime is not a monster");
    get_up(&mut h, 0).await;

    summon(&mut h, "minecraft:skeleton", 6.5, 4.5).await;
    let from = h.sessions[0].log.len();
    assert!(!try_sleep(&mut h, 0).await);
    assert!(action_bar_says(&h.sessions[0].log[from..], MONSTERS_MESSAGE), "{:?}", action_bars(&h.sessions[0].log[from..]));
    h.shutdown().await;
}

/// A zombified piglin counts only for the player it is angry at.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_zombified_piglin_keeps_only_the_player_it_is_angry_at_awake() {
    let mut h = bed_world(false).await;
    summon(&mut h, "minecraft:zombified_piglin", 4.5, 4.5).await;
    assert!(try_sleep(&mut h, 0).await, "a calm piglin does not keep anyone awake");
    get_up(&mut h, 0).await;

    let piglin = entity_id(&h, "minecraft:zombified_piglin");
    h.sessions[0].attack(piglin).await;
    h.pump(Duration::from_millis(300)).await;
    let from = h.sessions[0].log.len();
    assert!(!try_sleep(&mut h, 0).await, "an angry piglin keeps its target awake");
    assert!(action_bar_says(&h.sessions[0].log[from..], MONSTERS_MESSAGE), "{:?}", action_bars(&h.sessions[0].log[from..]));
    assert!(try_sleep(&mut h, 1).await, "the piglin is not angry at the bystander");
    h.shutdown().await;
}

// ---- item frames ---------------------------------------------------------------------

const FRAME_ITEM: &str = "{Facing:3b,Item:{id:\"minecraft:diamond\",count:1}}";

fn frames(h: &Harness) -> Vec<i32> {
    h.server.mobs().expect("mob sim").with(|sim| {
        let mut ids: Vec<i32> = sim.snapshots().into_iter().filter(|s| s.entity_type.to_string().ends_with("item_frame")).map(|s| s.id).collect();
        ids.sort_unstable();
        ids
    })
}

fn framed_item(h: &Harness, id: i32) -> Option<ItemStack> {
    h.server.mobs().expect("mob sim").with(|sim| sim.frame_state(id).and_then(|state| state.2))
}

async fn frame_world(nbt: &str) -> (Harness, i32) {
    let mut h = Harness::start(&["Archer"], |world| {
        for x in 2..=7 {
            world.set_block(x, FLOOR + 1, 0, state("minecraft:stone"));
        }
    })
    .await;
    h.sessions[0].walk_to(3.5, 3.5).await;
    h.sessions[0].command(&format!("summon minecraft:item_frame 3 64 1 {nbt}")).await;
    h.wait("the frame", |h| frames(h).len() == 1).await;
    let id = frames(&h)[0];
    (h, id)
}

/// Throws a snowball from the archer's hand at the frame in cell (3, 64, 1).
async fn throw_snowball_at_frame(h: &mut Harness) {
    let slot = h.give(0, stacked("minecraft:snowball", 4)).await;
    h.sessions[0].select(slot - 36).await;
    h.sessions[0].use_item(180.0, 24.0).await;
    h.pump(Duration::from_millis(900)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_creative_players_projectile_breaks_a_fixed_frame() {
    let (mut h, id) = frame_world(&format!("{{Fixed:1b,{}", &FRAME_ITEM[1..])).await;

    // Control: a survival player's snowball strikes the frame and changes nothing.
    throw_snowball_at_frame(&mut h).await;
    assert_eq!(frames(&h), [id], "a survival projectile left the fixed frame");
    assert!(framed_item(&h, id).is_some());

    h.sessions[0].command("gamemode creative").await;
    h.pump(Duration::from_millis(400)).await;
    throw_snowball_at_frame(&mut h).await;
    assert!(frames(&h).is_empty(), "a creative player's snowball breaks a fixed frame");
    h.shutdown().await;
}

/// The same throw at a frame that is not fixed shows the throw reaches it: the
/// item pops for any shooter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_survival_projectile_pops_an_ordinary_frames_item() {
    let (mut h, id) = frame_world(FRAME_ITEM).await;
    throw_snowball_at_frame(&mut h).await;
    assert_eq!(frames(&h), [id]);
    assert!(framed_item(&h, id).is_none(), "the item popped out");
    h.shutdown().await;
}

/// A frame is not ticked as a burning entity, so fire in its own cell leaves
/// it and its item alone, while a hit on a second frame shows the observation
/// would have seen a change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fire_in_a_frames_cell_does_not_hurt_it() {
    let (mut h, burning) = frame_world(FRAME_ITEM).await;
    h.sessions[0].command("summon minecraft:item_frame 6 64 1 {Facing:3b,Item:{id:\"minecraft:diamond\",count:1}}").await;
    h.wait("the second frame", |h| frames(h).len() == 2).await;
    let control = *frames(&h).iter().find(|&&id| id != burning).unwrap();
    h.sessions[0].command("setblock 3 64 1 minecraft:fire").await;
    h.wait("the fire", |h| h.world.at(BlockPos::new(3, FLOOR + 1, 1)) == state("minecraft:fire")).await;
    h.sessions[0].attack(control).await;
    h.pump(Duration::from_millis(2500)).await;
    assert!(framed_item(&h, control).is_none(), "the control hit popped its item");
    assert!(framed_item(&h, burning).is_some(), "the frame in the fire kept its item");
    assert_eq!(frames(&h).len(), 2);
    h.shutdown().await;
}
