//! Authoritative block placement through the live connection loop with the
//! 26.2 wire: what the server writes into the world and which block updates it
//! sends back, for the branches a single-cell placement never exercises.
//!
//! Each case pairs an accepted arm with a rejected or contrasting arm over the
//! same click, so a rule that always accepts (or always writes the same state)
//! fails one of them:
//!
//! - a door owns two cells: placed whole when the upper cell is free, not at
//!   all when it is occupied, and the refusal re-sends both cells so a client
//!   that predicted the pair is corrected;
//! - a waterloggable block placed into a water source keeps the water, placed
//!   into flowing water or air does not;
//! - right-clicking a chest while holding a chest opens it, and the same click
//!   with sneak held places a new, single chest beside it instead.
//!
//! Expected states are written out as block-state strings, not derived from the
//! placement code.
//!
//! Controls, run by hand: skipping the waterlogging step, ignoring the sneak
//! bit at the menu check, or skipping the multi-cell legality gate each fails
//! exactly the matching test here.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::{Ctx, Encode, Reader, Writer};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ItemStack, ResourceKey};
use lodestone_net::Connection;
use lodestone_server::access::AccessHandle;
use lodestone_server::commands::Effect;
use lodestone_server::world_state::WorldStateHandle;
use lodestone_server::{BlockEntityHandle, ChunkColumn, ChunkSource, NoEntities};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::{configuration, handshaking, login, play};
use lodestone_v26_2::packets::game::UseItemOn;
use lodestone_v26_2::packets::handshake::Intention;
use lodestone_v26_2::packets::login::LoginHello;
use tokio::io::DuplexStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 776 };
const DEADLINE: Duration = Duration::from_secs(20);
const PLAYER: u128 = 0x769;
const FACE_UP: i32 = 1;
const FACE_EAST: i32 = 5;

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
        if y == 63 { state("minecraft:stone") } else { StateId::AIR }
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
                column.set_block_id(x, 63, z, Self::base(63));
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
        self.edits
            .lock()
            .unwrap()
            .get(&BlockPos::new(x, y, z))
            .copied()
            .unwrap_or_else(|| Self::base(y))
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

async fn packet(client: &mut Connection<DuplexStream>) -> (i32, Vec<u8>) {
    tokio::time::timeout(DEADLINE, client.read_packet())
        .await
        .expect("bounded packet deadline")
        .expect("packet read")
        .expect("connection remains open")
}

async fn acknowledge(client: &mut Connection<DuplexStream>, id: i32, payload: &[u8]) {
    if id == play::clientbound::PLAYER_POSITION {
        let mut ack = Writer::default();
        ack.var_i32(Reader::new(payload).var_i32().unwrap());
        client.write_packet(play::serverbound::ACCEPT_TELEPORTATION, ack.as_slice()).await.unwrap();
    } else if id == play::clientbound::CHUNK_BATCH_FINISHED {
        let mut ack = Writer::default();
        ack.f32(64.0);
        client.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, ack.as_slice()).await.unwrap();
        client.write_packet(play::serverbound::PLAYER_LOADED, &[]).await.unwrap();
    } else if id == play::clientbound::KEEP_ALIVE {
        client.write_packet(play::serverbound::KEEP_ALIVE, payload).await.unwrap();
    }
}

/// Logs in and reads until the first chunk batch has finished.
async fn start(
    world: &World,
    state_handle: &WorldStateHandle,
) -> (Connection<DuplexStream>, tokio::task::JoinHandle<Result<lodestone_server::ServeSummary, lodestone_server::ServerError>>) {
    let (client_io, server_io) = lodestone_net::memory_pair();
    let server_world = world.clone();
    let server_state = state_handle.clone();
    let server = tokio::spawn(async move {
        lodestone_server::serve_connection_with_access_and_state(
            &mut Connection::new(server_io),
            &V770ServerProtocol,
            &server_world,
            &NoEntities,
            0,
            &AccessHandle::default(),
            &server_state,
            &BlockEntityHandle::default(),
            None,
        )
        .await
    });
    let mut client = Connection::new(client_io);
    let mut writer = Writer::default();
    Intention { protocol_version: 776, host: "memory".into(), port: 25565, next_state: 2 }
        .encode(&mut writer, CTX)
        .unwrap();
    client.write_packet(handshaking::serverbound::INTENTION, writer.as_slice()).await.unwrap();
    let mut writer = Writer::default();
    LoginHello { name: "Placer".into(), profile_id: Uuid::from_u128(PLAYER) }
        .encode(&mut writer, CTX)
        .unwrap();
    client.write_packet(login::serverbound::HELLO, writer.as_slice()).await.unwrap();
    loop {
        let (id, payload) = packet(&mut client).await;
        if id == login::clientbound::LOGIN_COMPRESSION {
            client.set_compression(Reader::new(&payload).var_i32().unwrap());
        } else {
            assert_eq!(id, login::clientbound::LOGIN_FINISHED);
            break;
        }
    }
    client.write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[]).await.unwrap();
    while packet(&mut client).await.0 != configuration::clientbound::FINISH_CONFIGURATION {}
    client.write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[]).await.unwrap();
    loop {
        let (id, payload) = packet(&mut client).await;
        acknowledge(&mut client, id, &payload).await;
        if id == play::clientbound::CHUNK_BATCH_FINISHED {
            break;
        }
    }
    (client, server)
}

/// Packs a block position the way the wire does: x, z then y.
fn packed(pos: BlockPos) -> i64 {
    ((i64::from(pos.x) & 0x3FF_FFFF) << 38) | ((i64::from(pos.z) & 0x3FF_FFFF) << 12) | (i64::from(pos.y) & 0xFFF)
}

fn unpacked(value: i64) -> BlockPos {
    BlockPos::new((value >> 38) as i32, (value << 52 >> 52) as i32, (value << 26 >> 38) as i32)
}

/// What the server answered one click with.
#[derive(Debug, Default)]
struct Answer {
    /// Every block update, in arrival order.
    updates: Vec<(BlockPos, StateId)>,
    /// Whether a container screen opened.
    opened_screen: bool,
}

impl Answer {
    fn update_at(&self, pos: BlockPos) -> Option<StateId> {
        self.updates.iter().rev().find(|(at, _)| *at == pos).map(|&(_, state)| state)
    }
}

/// Gives the player `count` of `item` and waits until the stack has landed.
async fn give(client: &mut Connection<DuplexStream>, world: &WorldStateHandle, item: &str, count: u32) {
    let stack = ItemStack::new(ResourceKey::from_str(item).unwrap(), count);
    tokio::time::timeout(DEADLINE, async {
        // The player registers once the join has been processed, which can trail
        // the first chunk batch.
        while !world.player_registry().push_effect(Uuid::from_u128(PLAYER), Effect::GiveItems(vec![stack.clone()])) {
            if let Ok(Ok(Some((id, payload)))) = tokio::time::timeout(Duration::from_millis(20), client.read_packet()).await {
                acknowledge(client, id, &payload).await;
            }
        }
        loop {
            let (id, payload) = packet(client).await;
            acknowledge(client, id, &payload).await;
            if id == play::clientbound::CONTAINER_SET_SLOT {
                return;
            }
        }
    })
    .await
    .expect("the given stack must reach the inventory wire");
}

/// Turns the player to `yaw` degrees, level-ish, as a client does before it
/// clicks; directional placements read this.
async fn look(client: &mut Connection<DuplexStream>, yaw: f32) {
    let mut writer = Writer::default();
    writer.f32(yaw);
    writer.f32(30.0);
    writer.u8(1);
    client.write_packet(play::serverbound::MOVE_PLAYER_ROT, writer.as_slice()).await.unwrap();
}

async fn set_sneaking(client: &mut Connection<DuplexStream>, sneaking: bool) {
    client
        .write_packet(play::serverbound::PLAYER_INPUT, &[if sneaking { 0x20 } else { 0 }])
        .await
        .unwrap();
}

/// Right-clicks `face` of `pos` with the main hand and collects the answer up
/// to the matching block-changed acknowledgement, plus a short quiet window for
/// anything sent after it.
async fn click(client: &mut Connection<DuplexStream>, pos: BlockPos, face: i32, sequence: i32) -> Answer {
    let mut writer = Writer::default();
    UseItemOn {
        hand: 0,
        pos: packed(pos),
        face,
        cursor_x: 0.5,
        cursor_y: if face == FACE_UP { 1.0 } else { 0.5 },
        cursor_z: 0.5,
        inside_block: false,
        world_border_hit: false,
        sequence,
    }
    .encode(&mut writer, CTX)
    .unwrap();
    client.write_packet(play::serverbound::USE_ITEM_ON, writer.as_slice()).await.unwrap();
    let mut answer = Answer::default();
    let record = |id: i32, payload: &[u8], answer: &mut Answer| {
        if id == play::clientbound::BLOCK_UPDATE {
            let mut reader = Reader::new(payload);
            let pos = unpacked(reader.i64().unwrap());
            let raw = reader.var_i32().unwrap();
            answer.updates.push((pos, StateId::new(u32::try_from(raw).unwrap()).expect("wire state id")));
        } else if id == play::clientbound::OPEN_SCREEN {
            answer.opened_screen = true;
        }
    };
    tokio::time::timeout(DEADLINE, async {
        loop {
            let (id, payload) = packet(client).await;
            acknowledge(client, id, &payload).await;
            record(id, &payload, &mut answer);
            if id == play::clientbound::BLOCK_CHANGED_ACK && Reader::new(&payload).var_i32().unwrap() == sequence {
                break;
            }
        }
    })
    .await
    .expect("every use-item-on is acknowledged");
    let quiet = tokio::time::Instant::now() + Duration::from_millis(200);
    while let Ok(Ok(Some((id, payload)))) = tokio::time::timeout_at(quiet, client.read_packet()).await {
        acknowledge(client, id, &payload).await;
        record(id, &payload, &mut answer);
    }
    answer
}

fn up(pos: BlockPos, dy: i32) -> BlockPos {
    BlockPos::new(pos.x, pos.y + dy, pos.z)
}

fn property<'a>(state: StateId, key: &str) -> Option<&'a str> {
    state.properties().iter().find_map(|(k, v)| (*k == key).then_some(*v))
}

/// A door placed on free ground writes both halves; the same door under an
/// occupied upper cell writes nothing and re-sends both cells.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_door_is_placed_whole_or_not_at_all() {
    let world = World::default();
    let blocked = BlockPos::new(2, 64, -2);
    world.set_block(blocked.x, blocked.y + 1, blocked.z, state("minecraft:stone"));
    let state_handle = WorldStateHandle::new();
    let _ = state_handle.ensure_dimension_runtime(lodestone_server::dimension::Dimension::Overworld);
    let (mut client, server) = start(&world, &state_handle).await;
    give(&mut client, &state_handle, "minecraft:oak_door", 2).await;
    // Yaw 0 looks south (+z), and a door faces the way its placer looks.
    look(&mut client, 0.0).await;

    let free = BlockPos::new(2, 64, 2);
    let accepted = click(&mut client, up(free, -1), FACE_UP, 1).await;
    let lower = world.at(free);
    let upper = world.at(up(free, 1));
    assert_eq!(lower.block().name(), "minecraft:oak_door", "{accepted:?}");
    assert_eq!(property(lower, "half"), Some("lower"));
    assert_eq!(upper.block().name(), "minecraft:oak_door");
    assert_eq!(property(upper, "half"), Some("upper"));
    assert_eq!(property(lower, "facing"), Some("south"));
    assert_eq!(property(upper, "facing"), Some("south"));
    assert_eq!(accepted.update_at(free), Some(lower), "{accepted:?}");
    assert_eq!(accepted.update_at(up(free, 1)), Some(upper), "{accepted:?}");

    let refused = click(&mut client, up(blocked, -1), FACE_UP, 2).await;
    assert_eq!(world.at(blocked), StateId::AIR, "no lower half under an occupied upper cell");
    assert_eq!(world.at(up(blocked, 1)), state("minecraft:stone"), "the occupant is untouched");
    assert_eq!(refused.update_at(blocked), Some(StateId::AIR), "the refused lower cell is re-sent: {refused:?}");
    assert_eq!(
        refused.update_at(up(blocked, 1)),
        Some(state("minecraft:stone")),
        "the refused upper cell is re-sent: {refused:?}"
    );

    drop(client);
    let _ = server.await;
}

/// The same slab click into a water source, into flowing water and into air:
/// only the source makes it waterlogged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slab_keeps_only_source_water() {
    let world = World::default();
    let source = BlockPos::new(2, 64, 2);
    let flowing = BlockPos::new(2, 64, -2);
    let dry = BlockPos::new(-2, 64, 2);
    world.set_block(source.x, source.y, source.z, state("minecraft:water[level=0]"));
    world.set_block(flowing.x, flowing.y, flowing.z, state("minecraft:water[level=1]"));
    let state_handle = WorldStateHandle::new();
    let _ = state_handle.ensure_dimension_runtime(lodestone_server::dimension::Dimension::Overworld);
    let (mut client, server) = start(&world, &state_handle).await;
    give(&mut client, &state_handle, "minecraft:oak_slab", 3).await;

    let mut placed = Vec::new();
    for (sequence, target) in [(1, source), (2, flowing), (3, dry)] {
        let answer = click(&mut client, up(target, -1), FACE_UP, sequence).await;
        let state = world.at(target);
        assert_eq!(state.block().name(), "minecraft:oak_slab", "{target:?}: {answer:?}");
        assert_eq!(answer.update_at(target), Some(state), "the placed state is what the client is told");
        placed.push(property(state, "waterlogged"));
    }
    assert_eq!(placed, [Some("true"), Some("false"), Some("false")]);

    drop(client);
    let _ = server.await;
}

/// Holding a chest, a plain click on a chest opens it and places nothing; the
/// same click while sneaking places a single chest on the clicked side.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sneaking_places_beside_a_chest_instead_of_opening_it() {
    let world = World::default();
    let chest = BlockPos::new(2, 64, 2);
    let beside = BlockPos::new(3, 64, 2);
    world.set_block(chest.x, chest.y, chest.z, state("minecraft:chest[facing=north,type=single,waterlogged=false]"));
    let state_handle = WorldStateHandle::new();
    let _ = state_handle.ensure_dimension_runtime(lodestone_server::dimension::Dimension::Overworld);
    let (mut client, server) = start(&world, &state_handle).await;
    give(&mut client, &state_handle, "minecraft:chest", 2).await;

    let plain = click(&mut client, chest, FACE_EAST, 1).await;
    assert!(plain.opened_screen, "a plain click opens the chest: {plain:?}");
    assert_eq!(world.at(beside), StateId::AIR, "and places nothing");

    set_sneaking(&mut client, true).await;
    let sneaking = click(&mut client, chest, FACE_EAST, 2).await;
    assert!(!sneaking.opened_screen, "{sneaking:?}");
    let placed = world.at(beside);
    assert_eq!(placed.block().name(), "minecraft:chest", "{sneaking:?}");
    assert_eq!(property(placed, "type"), Some("single"), "a sneak placement does not pair");
    assert_eq!(property(world.at(chest), "type"), Some("single"), "nor retype the clicked chest");
    assert_eq!(sneaking.update_at(beside), Some(placed));

    drop(client);
    let _ = server.await;
}
