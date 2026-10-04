//! Respawn points across dimensions, through the live connection loop with the
//! 26.2 wire.
//!
//! Each test logs a real client in, sets respawn points by right-clicking a bed
//! or a respawn anchor over the wire, dies, sends the perform-respawn command and
//! reads back which dimension the respawn packet names, where the position sync
//! puts the player, and whether the no-respawn-block event and the anchor's
//! depletion sound arrive. Expected positions are worked from the stand-up
//! search order (a north-facing bed's first cell is one block east; an anchor's
//! first cell is one block north) and expected dimensions from where the bed or
//! anchor stands, not from anything this server computes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, Vec3};
use lodestone_net::Connection;
use lodestone_server::access::AccessHandle;
use lodestone_server::dimension::Dimension;
use lodestone_server::player_data::PlayerDataStore;
use lodestone_server::portal::PortalIndex;
use lodestone_server::world_state::WorldStateHandle;
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, NoEntities, ScheduledTickHandle, TicketStoreHandle,
    WorldRegistries,
};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::{configuration, handshaking, login, play};
use lodestone_v26_2::packets::game::{Respawn, UseItemOn};
use lodestone_v26_2::packets::handshake::Intention;
use lodestone_v26_2::packets::login::LoginHello;
use tokio::io::DuplexStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 776 };
const DEADLINE: Duration = Duration::from_secs(20);
const PLAYER: u128 = 91;

fn state(name: &str) -> StateId {
    StateId::from_state_str(name).expect("fixture state")
}

/// Three flat dimensions sharing one edit map, a portal pair at the origin, an
/// optional player-file store and no spawn surprises: stone at y = 63.
#[derive(Clone)]
struct World {
    dimension: Dimension,
    edits: Arc<Mutex<HashMap<(Dimension, BlockPos), StateId>>>,
    portals: PortalIndex,
    tickets: Arc<[TicketStoreHandle; 3]>,
    registries: Option<WorldRegistries>,
}

impl World {
    fn new(player_dir: Option<&std::path::Path>) -> Self {
        let portals = PortalIndex::default();
        for dimension in [Dimension::Overworld, Dimension::Nether] {
            portals.insert(dimension, BlockPos::new(0, 64, 0));
        }
        let registries = player_dir.map(|dir| WorldRegistries {
            block_entities: BlockEntityHandle::default(),
            scheduled: ScheduledTickHandle::default(),
            player_data: Some(PlayerDataStore::new(dir).expect("player store")),
            native_storage: None,
        });
        Self {
            dimension: Dimension::Overworld,
            edits: Arc::new(Mutex::new(HashMap::new())),
            portals,
            tickets: Arc::new(std::array::from_fn(|_| TicketStoreHandle::new())),
            registries,
        }
    }

    fn in_dimension(&self, dimension: Dimension) -> Self {
        Self { dimension, ..self.clone() }
    }

    fn base_state(&self, x: i32, y: i32, z: i32) -> StateId {
        if y == 63 {
            return state("minecraft:stone");
        }
        if (x, y, z) == (0, 64, 0) {
            return state("minecraft:nether_portal[axis=x]");
        }
        StateId::AIR
    }
}

impl ChunkSource for World {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(self.dimension.min_y(), self.dimension.height());
        for z in 0..16 {
            for x in 0..16 {
                column.set_block_id(x, 63, z, state("minecraft:stone"));
                column.set_block_id(x, 64, z, self.base_state(cx * 16 + x, 64, cz * 16 + z));
            }
        }
        for (&(dimension, pos), &edit) in self.edits.lock().unwrap().iter() {
            if dimension == self.dimension && pos.x.div_euclid(16) == cx && pos.z.div_euclid(16) == cz {
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
            .get(&(self.dimension, BlockPos::new(x, y, z)))
            .copied()
            .unwrap_or_else(|| self.base_state(x, y, z))
    }

    fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        Some(self.block_state_id(x, y, z))
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".into()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, edit: StateId) {
        self.edits.lock().unwrap().insert((self.dimension, BlockPos::new(x, y, z)), edit);
    }

    fn dimension(&self) -> Option<Dimension> {
        Some(self.dimension)
    }

    fn ticket_store(&self) -> Option<TicketStoreHandle> {
        Some(self.tickets[match self.dimension {
            Dimension::Overworld => 0,
            Dimension::Nether => 1,
            Dimension::End => 2,
        }]
        .clone())
    }

    fn reconcile_ticket_residency(&self) {
        self.ticket_store().unwrap().tick();
    }

    fn sibling(&self, dimension: Dimension) -> Option<Arc<dyn ChunkSource>> {
        Some(Arc::new(self.in_dimension(dimension)))
    }

    fn portal_index(&self) -> Option<&PortalIndex> {
        Some(&self.portals)
    }

    fn world_registries(&self) -> Option<WorldRegistries> {
        self.registries.clone()
    }
}

async fn packet(client: &mut Connection<DuplexStream>) -> (i32, Vec<u8>) {
    tokio::time::timeout(DEADLINE, client.read_packet())
        .await
        .expect("bounded packet deadline")
        .expect("packet read")
        .expect("connection remains open")
}

async fn login(client: &mut Connection<DuplexStream>) {
    let mut writer = Writer::default();
    Intention { protocol_version: 776, host: "memory".into(), port: 25565, next_state: 2 }
        .encode(&mut writer, CTX)
        .unwrap();
    client.write_packet(handshaking::serverbound::INTENTION, writer.as_slice()).await.unwrap();
    let mut writer = Writer::default();
    LoginHello { name: "RespawnViewer".into(), profile_id: Uuid::from_u128(PLAYER) }
        .encode(&mut writer, CTX)
        .unwrap();
    client.write_packet(login::serverbound::HELLO, writer.as_slice()).await.unwrap();
    loop {
        let (id, payload) = packet(client).await;
        if id == login::clientbound::LOGIN_COMPRESSION {
            client.set_compression(Reader::new(&payload).var_i32().unwrap());
        } else {
            assert_eq!(id, login::clientbound::LOGIN_FINISHED);
            break;
        }
    }
    client.write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[]).await.unwrap();
    while packet(client).await.0 != configuration::clientbound::FINISH_CONFIGURATION {}
    client.write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[]).await.unwrap();
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

async fn movement(client: &mut Connection<DuplexStream>, x: f64, z: f64) {
    let mut writer = Writer::default();
    writer.f64(x);
    writer.f64(64.0);
    writer.f64(z);
    writer.u8(1);
    client.write_packet(play::serverbound::MOVE_PLAYER_POS, writer.as_slice()).await.unwrap();
}

/// Walks from `from` to the portal cell in short steps, as a client would; one
/// long jump is rejected as impossible movement.
async fn walk_to_portal(client: &mut Connection<DuplexStream>, from: (f64, f64)) {
    let steps = 40;
    for step in 1..=steps {
        let t = f64::from(step) / f64::from(steps);
        movement(client, from.0 + (0.5 - from.0) * t, from.1 + (0.5 - from.1) * t).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Reads until the first chunk batch has finished, which is the join.
async fn join_settled(client: &mut Connection<DuplexStream>) {
    loop {
        let (id, payload) = packet(client).await;
        acknowledge(client, id, &payload).await;
        if id == play::clientbound::CHUNK_BATCH_FINISHED {
            break;
        }
    }
}

/// Packs a block position the way the wire does: x, z then y.
fn packed(pos: BlockPos) -> i64 {
    ((i64::from(pos.x) & 0x3FF_FFFF) << 38) | ((i64::from(pos.z) & 0x3FF_FFFF) << 12) | (i64::from(pos.y) & 0xFFF)
}

/// Right-clicks the block at `pos` with an empty main hand and returns once the
/// server has answered the click (a ping round trip would also do; the chat line
/// is the answer a respawn point being set gives).
async fn use_block(client: &mut Connection<DuplexStream>, pos: BlockPos) {
    let mut writer = Writer::default();
    UseItemOn {
        hand: 0,
        pos: packed(pos),
        face: 1,
        cursor_x: 0.5,
        cursor_y: 1.0,
        cursor_z: 0.5,
        inside_block: false,
        world_border_hit: false,
        sequence: 1,
    }
    .encode(&mut writer, CTX)
    .unwrap();
    client.write_packet(play::serverbound::USE_ITEM_ON, writer.as_slice()).await.unwrap();
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle.as_bytes())
}

/// Reads until a system chat line containing `text` arrives.
async fn await_chat(client: &mut Connection<DuplexStream>, text: &str) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let (id, payload) = packet(client).await;
            acknowledge(client, id, &payload).await;
            if id == play::clientbound::SYSTEM_CHAT && contains(&payload, text) {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the chat line {text:?} never arrived"));
}

/// Reads until the dimension change to `dimension` has been fully applied (the
/// respawn packet naming it, then the position sync).
async fn await_dimension(client: &mut Connection<DuplexStream>, dimension: Dimension) {
    tokio::time::timeout(DEADLINE, async {
        let mut respawned = false;
        loop {
            let (id, payload) = packet(client).await;
            acknowledge(client, id, &payload).await;
            if id == play::clientbound::RESPAWN {
                assert_eq!(Respawn::decode(&mut Reader::new(&payload), CTX).unwrap().dimension, dimension.key());
                respawned = true;
            } else if respawned && id == play::clientbound::PLAYER_POSITION {
                return;
            }
        }
    })
    .await
    .expect("dimension respawn must reach the wire");
}

/// What the client saw between sending perform-respawn and the position sync
/// that follows the respawn packet.
#[derive(Debug)]
struct Respawned {
    /// Packet ids in arrival order.
    ids: Vec<i32>,
    /// The dimension the respawn packet named.
    dimension: String,
    /// The position sync's coordinates.
    position: Vec3,
    /// Parameter of every game event 0 (no respawn block) received.
    no_respawn_events: usize,
    /// Whether a sound packet arrived after the respawn packet.
    sound_after_respawn: bool,
}

/// Kills the player, waits for zero health, sends perform-respawn and reads the
/// answer.
async fn die_and_respawn(client: &mut Connection<DuplexStream>, world: &WorldStateHandle) -> Respawned {
    assert!(world.player_registry().push_effect(Uuid::from_u128(PLAYER), lodestone_server::commands::Effect::Kill));
    tokio::time::timeout(DEADLINE, async {
        loop {
            let (id, payload) = packet(client).await;
            acknowledge(client, id, &payload).await;
            if id == play::clientbound::SET_HEALTH && Reader::new(&payload).f32().unwrap() == 0.0 {
                break;
            }
        }
    })
    .await
    .expect("queued kill must reach the health wire");
    client.write_packet(play::serverbound::CLIENT_COMMAND, &[0]).await.unwrap();
    tokio::time::timeout(DEADLINE, async {
        let mut seen = Respawned {
            ids: Vec::new(),
            dimension: String::new(),
            position: Vec3::new(0.0, 0.0, 0.0),
            no_respawn_events: 0,
            sound_after_respawn: false,
        };
        let mut respawned = false;
        loop {
            let (id, payload) = packet(client).await;
            acknowledge(client, id, &payload).await;
            seen.ids.push(id);
            if id == play::clientbound::GAME_EVENT && payload[0] == 0 {
                seen.no_respawn_events += 1;
            } else if id == play::clientbound::RESPAWN {
                seen.dimension = Respawn::decode(&mut Reader::new(&payload), CTX).unwrap().dimension;
                respawned = true;
            } else if respawned && id == play::clientbound::SOUND {
                seen.sound_after_respawn = true;
            } else if respawned && id == play::clientbound::PLAYER_POSITION {
                let mut reader = Reader::new(&payload);
                reader.var_i32().unwrap();
                seen.position = Vec3::new(reader.f64().unwrap(), reader.f64().unwrap(), reader.f64().unwrap());
                // The depletion sound follows the position sync; give the rest
                // of the burst a moment to arrive.
                let quiet = tokio::time::Instant::now() + Duration::from_millis(300);
                while let Ok(Ok(Some((id, payload)))) = tokio::time::timeout_at(quiet, client.read_packet()).await {
                    acknowledge(client, id, &payload).await;
                    seen.ids.push(id);
                    if id == play::clientbound::SOUND {
                        seen.sound_after_respawn = true;
                    }
                }
                return seen;
            }
        }
    })
    .await
    .expect("the respawn must reach the wire")
}

/// Walks out of a portal and waits out the arrival cooldown.
async fn leave_portal(client: &mut Connection<DuplexStream>) {
    for step in 1..=10 {
        movement(client, 0.5 + 0.2 * f64::from(step), 0.5).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let cooldown = Duration::from_millis(50) * (lodestone_server::portal::PLAYER_PORTAL_COOLDOWN as u32 + 4);
    let deadline = tokio::time::Instant::now() + cooldown;
    while let Ok(read) = tokio::time::timeout_at(deadline, client.read_packet()).await {
        let (id, payload) = read.unwrap().unwrap();
        acknowledge(client, id, &payload).await;
    }
}

/// Walks into the portal from `from` until the trip to `dimension` happens,
/// stepping out and back in when the server has not taken it: entry is gated by
/// an arrival cooldown that only drains outside the portal, so one attempt can
/// land inside it.
async fn enter_portal(client: &mut Connection<DuplexStream>, from: (f64, f64), dimension: Dimension) {
    for _ in 0..6 {
        walk_to_portal(client, from).await;
        if tokio::time::timeout(Duration::from_millis(2500), await_dimension(client, dimension)).await.is_ok() {
            return;
        }
        leave_portal(client).await;
    }
    panic!("the portal never took the player to {dimension:?}");
}

async fn start(
    world: &World,
    state: &WorldStateHandle,
) -> (Connection<DuplexStream>, tokio::task::JoinHandle<Result<lodestone_server::ServeSummary, lodestone_server::ServerError>>) {
    let (client_io, server_io) = tokio::io::duplex(4 * 1024 * 1024);
    let server_world = world.clone();
    let server_state = state.clone();
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
    login(&mut client).await;
    join_settled(&mut client).await;
    (client, server)
}

fn charges(world: &World, dimension: Dimension, pos: BlockPos) -> u8 {
    let state = world.in_dimension(dimension).block_state_id(pos.x, pos.y, pos.z);
    state
        .properties()
        .iter()
        .find_map(|(key, value)| (*key == "charges").then(|| value.parse().unwrap()))
        .unwrap_or(0)
}

const BED: BlockPos = BlockPos::new(10, 64, 10);
const ANCHOR: BlockPos = BlockPos::new(6, 64, 6);

fn setup_blocks(world: &World, anchor_charges: u8) {
    world.set_block(BED.x, BED.y, BED.z, state("minecraft:red_bed[facing=north,part=foot]"));
    world
        .in_dimension(Dimension::Nether)
        .set_block(ANCHOR.x, ANCHOR.y, ANCHOR.z, state(&format!("minecraft:respawn_anchor[charges={anchor_charges}]")));
}

/// A bed in the overworld, an anchor in the Nether, and deaths in each: the
/// respawn goes to the dimension of the block, never the dimension of the death.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn respawn_points_follow_their_own_dimension() {
    let world = World::new(None);
    setup_blocks(&world, 1);
    let state_handle = WorldStateHandle::new();
    let _ = state_handle.ensure_dimension_runtime(Dimension::Overworld);
    let _ = state_handle.ensure_dimension_runtime(Dimension::Nether);
    state_handle.set_rule("players_nether_portal_default_delay", "0").unwrap();
    let (mut client, server) = start(&world, &state_handle).await;

    // Set the bed as the respawn point, then die in the Nether: home to the bed.
    use_block(&mut client, BED).await;
    await_chat(&mut client, "Respawn point set").await;
    movement(&mut client, 0.5, 0.5).await;
    await_dimension(&mut client, Dimension::Nether).await;
    let home = die_and_respawn(&mut client, &state_handle).await;
    assert_eq!(home.dimension, Dimension::Overworld.key(), "a bed is in the overworld: {home:?}");
    assert_eq!(home.position, Vec3::new(11.5, 64.0, 10.5), "one block east of a north-facing bed");
    assert_eq!(home.no_respawn_events, 0);
    assert!(!home.sound_after_respawn, "a bed makes no depletion sound");

    // Now set the Nether anchor (from the Nether), come home and die in the
    // overworld: the respawn travels to the Nether, spends the charge, and the
    // player hears the depletion.
    enter_portal(&mut client, (11.5, 10.5), Dimension::Nether).await;
    use_block(&mut client, ANCHOR).await;
    await_chat(&mut client, "Respawn point set").await;
    leave_portal(&mut client).await;
    enter_portal(&mut client, (2.5, 0.5), Dimension::Overworld).await;
    leave_portal(&mut client).await;
    let nether = die_and_respawn(&mut client, &state_handle).await;
    assert_eq!(nether.dimension, Dimension::Nether.key(), "an overworld death respawns in the Nether: {nether:?}");
    assert_eq!(nether.position, Vec3::new(6.5, 64.0, 5.5), "one block north of the anchor");
    assert_eq!(charges(&world, Dimension::Nether, ANCHOR), 0, "one charge spent");
    assert!(nether.sound_after_respawn, "the depletion sound follows: {nether:?}");
    assert_eq!(nether.no_respawn_events, 0);

    // The anchor is empty now, so dying in the Nether finds nothing: the event
    // arrives before the respawn packet and the player goes home to world spawn.
    let empty = die_and_respawn(&mut client, &state_handle).await;
    assert_eq!(empty.no_respawn_events, 1, "{empty:?}");
    assert_eq!(empty.dimension, Dimension::Overworld.key());
    let event_at = empty.ids.iter().position(|id| *id == play::clientbound::GAME_EVENT).unwrap();
    let respawn_at = empty.ids.iter().position(|id| *id == play::clientbound::RESPAWN).unwrap();
    assert!(event_at < respawn_at, "the event precedes the respawn packet: {empty:?}");
    assert!(!empty.sound_after_respawn);
    assert_ne!(empty.position, Vec3::new(6.5, 64.0, 5.5));

    // And the cleared point stays cleared: the next death is quiet.
    let quiet = die_and_respawn(&mut client, &state_handle).await;
    assert_eq!(quiet.no_respawn_events, 0, "{quiet:?}");
    drop(client);
    let _ = tokio::time::timeout(DEADLINE, server).await;
}

/// The respawn point is saved with the player and applies after a reconnect:
/// a bed set in one session is where the player respawns in the next.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_respawn_point_survives_a_reconnect() {
    let dir = tempfile::tempdir().expect("scratch world directory");
    let world = World::new(Some(dir.path()));
    setup_blocks(&world, 1);

    let first_state = WorldStateHandle::new();
    let _ = first_state.ensure_dimension_runtime(Dimension::Overworld);
    let (mut client, server) = start(&world, &first_state).await;
    use_block(&mut client, BED).await;
    await_chat(&mut client, "Respawn point set").await;
    drop(client);
    let _ = tokio::time::timeout(DEADLINE, server).await.expect("the first session ends");

    // A second session over the same player files, with fresh world state: no
    // bed was clicked this time.
    let second_state = WorldStateHandle::new();
    let _ = second_state.ensure_dimension_runtime(Dimension::Overworld);
    let (mut client, server) = start(&world, &second_state).await;
    let respawned = die_and_respawn(&mut client, &second_state).await;
    assert_eq!(
        respawned.position,
        Vec3::new(11.5, 64.0, 10.5),
        "the bed from the earlier session applies: {respawned:?}"
    );
    assert_eq!(respawned.no_respawn_events, 0);
    drop(client);
    let _ = tokio::time::timeout(DEADLINE, server).await;
}
