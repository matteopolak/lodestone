//! **Does a real name-tag use-entity packet, arriving on a joined connection,
//! reach `play_dispatch`, name the mob, consume the tag and stream the name?**
//!
//! `mobs::tests::cosmetics` proves `MobSim::apply_name_tag` and the metadata it
//! queues, and the dispatch branch calls it; neither says a packet decoded off a
//! connection gets there. This starts a real [`IntegratedServer`], joins a
//! connection, puts a named name tag in the first hotbar slot with a creative
//! slot write, spawns a cow beside the player through the live handle, and sends
//! an interact-entity packet for it. The observations are on the encode side of
//! the protocol double: a custom-name metadata field for that entity, and a
//! window-0 slot update showing one tag left of the two given.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_core::State;
use lodestone_data::block_states::StateId;
use lodestone_model::text::Text;
use lodestone_model::{ItemStack, ResourceKey, Vec3};
use lodestone_net::Connection;
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, MetadataField, ServerBound, ServerDirective,
    ServerProtocol,
};
use uuid::Uuid;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const FLOOR: i32 = 70;
const CEILING: i32 = FLOOR + 4;

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

/// Menu slot of the first hotbar slot.
const HOTBAR_ZERO: i16 = 36;
const DEADLINE: Duration = Duration::from_secs(30);

const PLAYER_X: f64 = 8.5;
const PLAYER_Z: f64 = 8.5;

fn fixture_state(name: &str) -> StateId {
    StateId::from_state_str(name).expect("fixture block state exists")
}

#[derive(Debug, Default)]
struct Observed {
    in_play: AtomicBool,
    /// Every `(entity id, plain custom name)` streamed in entity metadata.
    names: Mutex<Vec<(i32, String)>>,
    /// Every window-0 slot update as `(slot, remaining count)`.
    slots: Mutex<Vec<(i32, Option<u32>)>>,
}

#[derive(Debug)]
struct WatchingProtocol(Arc<Observed>);

impl ServerProtocol for WatchingProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => ServerBound::LoginStart {
                username: "NameTagger".to_string(),
                uuid: Uuid::nil(),
            },
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            State::Play if packet_id == MOVE_PLAYER => ServerBound::PlayerMoved {
                x: PLAYER_X,
                y: f64::from(FLOOR) + 1.0,
                z: PLAYER_Z,
                rotation: None,
                on_ground: true,
            },
            State::Play if packet_id == SET_SLOT => {
                let mut tag = ItemStack::new(
                    ResourceKey::new("minecraft", "name_tag").expect("valid key"),
                    2,
                );
                tag.components.custom_name = Some(Text::literal("Bessie"));
                ServerBound::CreativeModeSlotSet { slot: HOTBAR_ZERO, item: Some(tag) }
            }
            State::Play if packet_id == INTERACT => ServerBound::InteractEntity {
                entity_id: i32::from_be_bytes(payload[..4].try_into().expect("entity id")),
                hand: 0,
                using_secondary_action: false,
            },
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

    fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
        for field in fields {
            if let MetadataField::CustomName(Some(name)) = field {
                self.0.names.lock().expect("names lock").push((entity_id, name.to_plain_string()));
            }
        }
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

/// **The gate.** The interact packet names the cow, the name reaches the metadata
/// encoder, and the tag stack drops from two to one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_name_tag_interact_packet_names_the_mob_and_consumes_one_tag() {
    let observed = Arc::new(Observed::default());
    let (server, client) = IntegratedServer::open_in_memory_with_mobs(
        WatchingProtocol(Arc::clone(&observed)),
        RoofedRoom,
        (-2..=2, -2..=2),
        (8, 8),
        3,
    );
    server.world_state().set_rule("spawn_mobs", "false").expect("spawn_mobs is a known rule");
    // A creative slot write is the way a test hands the player an item with a
    // component; the player is then switched to survival so the tag is consumed.

    let mut client = Connection::new(client);
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    client.write_packet(LOGIN_START, &[0]).await.expect("login start");
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client.write_packet(LOGIN_ACKNOWLEDGED, &[]).await.expect("login ack");
    client.write_packet(FINISH_CONFIGURATION, &[]).await.expect("finish configuration");

    let start = tokio::time::Instant::now();
    let mut cow: Option<i32> = None;
    let mut interacted = false;
    while start.elapsed() < DEADLINE {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !observed.in_play.load(Ordering::SeqCst) {
            continue;
        }
        // Registers the player with the mob sim, and keeps it registered.
        let _ = client.write_packet(MOVE_PLAYER, &[]).await;

        if cow.is_none() {
            if let Some(mobs) = server.mobs() {
                // The join-centred seed replaces the whole simulation when it
                // lands, so a cow spawned earlier would be wiped.
                if server.world_state().initial_seed_landed() {
                    let id = mobs.with(|sim| {
                        sim.spawn_species(
                            ResourceKey::new("minecraft", "cow").expect("valid key"),
                            Vec3::new(PLAYER_X + 1.0, f64::from(FLOOR) + 1.0, PLAYER_Z),
                        )
                        .id()
                    });
                    cow = Some(id);
                    client.write_packet(CREATIVE, &[]).await.expect("switch to creative");
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    client.write_packet(SET_SLOT, &[]).await.expect("give the tag");
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    client.write_packet(SURVIVAL, &[]).await.expect("switch to survival");
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            }
        } else if !interacted {
            let id = cow.expect("spawned");
            client.write_packet(INTERACT, &id.to_be_bytes()).await.expect("interact");
            interacted = true;
        }

        let named = observed
            .names
            .lock()
            .expect("names lock")
            .iter()
            .any(|(id, name)| Some(*id) == cow && name == "Bessie");
        let consumed = observed
            .slots
            .lock()
            .expect("slots lock")
            .iter()
            .any(|(slot, count)| *slot == i32::from(HOTBAR_ZERO) && *count == Some(1));
        if named && consumed {
            break;
        }
    }
    let named = server.mobs().and_then(|mobs| {
        let id = cow?;
        mobs.with(|sim| sim.get(id).and_then(|mob| mob.custom_name()))
    });
    server.shutdown().await;

    let names = observed.names.lock().expect("names lock").clone();
    let slots = observed.slots.lock().expect("slots lock").clone();
    assert!(cow.is_some(), "the join-centred mob seed never landed in {DEADLINE:?}");
    assert_eq!(named.as_deref(), Some("Bessie"), "the sim state; streamed names {names:?}");
    assert!(
        names.iter().any(|(id, name)| Some(*id) == cow && name == "Bessie"),
        "the name never reached the metadata encoder: {names:?}"
    );
    assert!(
        slots.iter().any(|(slot, count)| *slot == i32::from(HOTBAR_ZERO) && *count == Some(1)),
        "the tag stack was not reduced from two to one: {slots:?}"
    );
}
