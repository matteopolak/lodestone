//! **Does a cushion item used on a block through a joined connection spawn a
//! cushion that streams, seats the player, and breaks to its coloured item?**
//!
//! `cushion::tests` proves the placement rules and `mobs::cushion` the sim; this
//! drives the packets. It starts a real [`IntegratedServer`], gives the player
//! three red cushions with a creative slot write, switches to survival, and sends
//! use-item-on, interact-entity and attack packets. The observations are on the
//! encode side of the protocol double: the add-entity snapshot, the colour
//! metadata field, the window-0 count, and the passenger lists.
//!
//! Expected values are the reference's: the floor top is `y = 71.0` so the
//! cushion stands there, centred on its cell; red is dye ordinal 14.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::State;
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockFace, BlockPos, ItemStack, ResourceKey, Vec3, Vec3f};
use lodestone_net::Connection;
use lodestone_server::{
    ChunkColumn, ChunkSource, EntitySnapshot, IntegratedServer, MetadataField, ServerBound,
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
const SET_SLOT: i32 = 41;
const INTERACT: i32 = 42;
const SURVIVAL: i32 = 43;
const CREATIVE: i32 = 44;
const USE_ON: i32 = 45;
const ATTACK: i32 = 46;
const SNEAK_INTERACT: i32 = 47;

const HOTBAR_ZERO: i16 = 36;
const DEADLINE: Duration = Duration::from_secs(30);
const PLAYER_X: f64 = 8.5;
const PLAYER_Z: f64 = 8.5;
/// The protocol double's own local player id, as the real encoders use.
const LOCAL_PLAYER: i32 = 1;

fn fixture_state(name: &str) -> StateId {
    StateId::from_state_str(name).expect("fixture block state exists")
}

#[derive(Debug, Default)]
struct Observed {
    in_play: AtomicBool,
    added: Mutex<Vec<EntitySnapshot>>,
    colours: Mutex<Vec<(i32, u8)>>,
    slots: Mutex<Vec<(i32, Option<u32>)>>,
    passengers: Mutex<Vec<(i32, Vec<i32>)>>,
    removed: Mutex<Vec<i32>>,
}

#[derive(Debug)]
struct WatchingProtocol(Arc<Observed>);

impl ServerProtocol for WatchingProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        let entity = || i32::from_be_bytes(payload[..4].try_into().expect("entity id"));
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake { next_state: State::Login },
            State::Login if packet_id == LOGIN_START => {
                ServerBound::LoginStart { username: "Sitter".to_string(), uuid: Uuid::nil() }
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
            State::Play if packet_id == SET_SLOT => {
                let cushions = ItemStack::new(ResourceKey::new("minecraft", "red_cushion").expect("valid key"), 3);
                ServerBound::CreativeModeSlotSet { slot: HOTBAR_ZERO, item: Some(cushions) }
            }
            State::Play if packet_id == USE_ON => ServerBound::UseItemOn {
                pos: BlockPos::new(i32::from(payload[0]), FLOOR, 8),
                face: BlockFace::Up,
                cursor: Vec3f { x: 0.5, y: 1.0, z: 0.5 },
                sequence: 1,
                hand: 0,
            },
            State::Play if packet_id == INTERACT => {
                ServerBound::InteractEntity { entity_id: entity(), hand: 0, using_secondary_action: false }
            }
            State::Play if packet_id == SNEAK_INTERACT => {
                ServerBound::InteractEntity { entity_id: entity(), hand: 0, using_secondary_action: true }
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
        self.0.added.lock().expect("added lock").push(entity.clone());
        ServerDirective::None
    }
    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        self.0.removed.lock().expect("removed lock").extend_from_slice(ids);
        ServerDirective::None
    }
    fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
        for field in fields {
            if let MetadataField::CushionColor(color) = field {
                self.0.colours.lock().expect("colours lock").push((entity_id, *color));
            }
        }
        ServerDirective::None
    }
    fn encode_set_passengers(&self, vehicle_id: i32, passenger_ids: &[i32]) -> ServerDirective {
        self.0.passengers.lock().expect("passengers lock").push((vehicle_id, passenger_ids.to_vec()));
        ServerDirective::None
    }
    fn encode_container_slot(
        &self,
        window_id: i32,
        _state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        if window_id == 0 {
            self.0.slots.lock().expect("slots lock").push((slot, item.map(|stack| stack.count)));
        }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cushion_places_streams_seats_the_player_and_breaks_to_its_item() {
    let observed = Arc::new(Observed::default());
    let (server, client) = IntegratedServer::open_in_memory_with_mobs(
        WatchingProtocol(Arc::clone(&observed)),
        Floor,
        (-2..=2, -2..=2),
        (8, 8),
        3,
    );
    server.world_state().set_rule("spawn_mobs", "false").expect("spawn_mobs is a known rule");
    let mobs = server.mobs().expect("the server runs a mob sim");

    let mut client = Connection::new(client);
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    client.write_packet(LOGIN_START, &[0]).await.expect("login start");
    client.read_packet().await.unwrap().unwrap();
    client.write_packet(LOGIN_ACKNOWLEDGED, &[]).await.expect("login ack");
    client.write_packet(FINISH_CONFIGURATION, &[]).await.expect("finish configuration");

    until("the join", || observed.in_play.load(Ordering::SeqCst) && server.world_state().initial_seed_landed()).await;
    // Registers the player with the sim; the first write may precede the seed.
    client.write_packet(MOVE_PLAYER, &[]).await.expect("move");
    client.write_packet(CREATIVE, &[]).await.expect("creative");
    tokio::time::sleep(Duration::from_millis(300)).await;
    client.write_packet(SET_SLOT, &[]).await.expect("give cushions");
    tokio::time::sleep(Duration::from_millis(300)).await;
    client.write_packet(SURVIVAL, &[]).await.expect("survival");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Use the floor at x = 12: a placement the player is not standing in.
    client.write_packet(USE_ON, &[12]).await.expect("use on");
    until("the cushion", || {
        mobs.with(|sim| sim.cushion_count()) == 1
            && observed.added.lock().expect("added lock").iter().any(|e| e.entity_type.to_string() == "minecraft:cushion")
    })
    .await;
    until("its colour", || !observed.colours.lock().expect("colours lock").is_empty()).await;
    let (id, position, color) = {
        let added = observed.added.lock().expect("added lock");
        let cushion = added
            .iter()
            .find(|e| e.entity_type.to_string() == "minecraft:cushion")
            .expect("the cushion streamed");
        (cushion.id, cushion.position, cushion.metadata.clone())
    };
    assert_eq!(position, Vec3::new(12.5, f64::from(FLOOR) + 1.0, 8.5));
    assert_eq!(color, vec![MetadataField::CushionColor(14)], "red is ordinal 14");
    until("the count drop", || {
        observed.slots.lock().expect("slots lock").iter().any(|(slot, count)| *slot == i32::from(HOTBAR_ZERO) && *count == Some(2))
    })
    .await;

    // The same spot again is refused: no second cushion, no further consumption.
    client.write_packet(USE_ON, &[12]).await.expect("use on again");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(mobs.with(|sim| sim.cushion_count()), 1);
    assert!(
        !observed.slots.lock().expect("slots lock").iter().any(|(_, count)| *count == Some(1)),
        "a refused placement must not consume"
    );

    // Sneaking does not sit; a plain click does.
    client.write_packet(SNEAK_INTERACT, &id.to_be_bytes()).await.expect("sneak interact");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mobs.with(|sim| sim.cushion_rider(id)), None);
    client.write_packet(INTERACT, &id.to_be_bytes()).await.expect("interact");
    until("the passenger list", || {
        observed.passengers.lock().expect("passengers lock").contains(&(id, vec![LOCAL_PLAYER]))
    })
    .await;
    assert!(mobs.with(|sim| sim.cushion_rider(id)).is_some());

    // A hit breaks it, frees the seat on the wire and drops the red cushion.
    client.write_packet(ATTACK, &id.to_be_bytes()).await.expect("attack");
    until("the removal", || observed.removed.lock().expect("removed lock").contains(&id)).await;
    assert_eq!(mobs.with(|sim| sim.cushion_count()), 0);
    assert!(observed.passengers.lock().expect("passengers lock").contains(&(id, Vec::new())));
    until("the dropped item", || {
        observed.added.lock().expect("added lock").iter().any(|e| {
            e.entity_type.to_string() == "minecraft:item"
                && e.metadata.iter().any(|m| matches!(m, MetadataField::Item { item, .. } if item.to_string() == "minecraft:red_cushion"))
        })
    })
    .await;
    server.shutdown().await;
}
