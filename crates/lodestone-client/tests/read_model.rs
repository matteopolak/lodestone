//! Hermetic tests for the programmable bot API: the maintained read-model,
//! movement actions, and awaiting primitives.
//!
//! Like `driver.rs`, every test uses [`lodestone_net::memory_pair`] and a
//! hand-written fake [`VersionAdapter`]; none require a real server. The fake
//! carries no version knowledge — it maps `(state, packet_id)` to canned events
//! and encodes actions with a stable, inspectable layout so assertions are made
//! against bytes on the wire, not against the read-model the client authored.

use std::collections::HashMap;
use std::time::Duration;

use lodestone_client::{
    BlockPos, BossAction, BossColor, BossOverlay, ChunkPos, ClientBuilder, ClientEvent,
    CollisionRule, ConnectionState, Directive, DisplaySlot, LoginProfile, NumberFormat,
    ObjectiveMode, ObjectiveRenderType, Rotation, ServerAddress, TeamAction, TeamParameters, Vec3,
    Visibility, WaitError,
};
use lodestone_model::event::{
    ChatKind, EntityAttributeModifier, EntityAttributeSnapshot, EntityEquipment,
    EntityMetadataUpdate, EntityMovement, EntityPose, EquipmentSlot, TeleportFlags,
};
use lodestone_model::text::Text;
use lodestone_model::{AdapterError, ClientAction, GameMode, Identifier, ItemStack};
use lodestone_net::{Connection, memory_pair};
use lodestone_world::{
    ChunkColumn, ChunkPos as WorldChunkPos, ChunkSection, ColumnLight, Heightmaps, LoadedChunk,
    PaletteKind, WorldSink,
};
use std::sync::Arc;
use uuid::Uuid;

const MOVE_ID: i32 = 0x11;
const KEEPALIVE_RESP_ID: i32 = 0x30;

/// A world write the fake adapter applies to the [`WorldSink`] when a given
/// packet arrives, mirroring how a real adapter decodes chunk packets in place.
/// Keyed by [`lodestone_world::ChunkPos`], the world's own vocabulary.
#[derive(Debug, Clone)]
enum WorldWrite {
    Load(WorldChunkPos, LoadedChunk),
    #[allow(dead_code)]
    Unload(WorldChunkPos),
}

/// A scriptable fake adapter that emits canned events, applies canned world
/// writes through the [`WorldSink`], and encodes `Move`, `SendChat`, and
/// `KeepAliveResponse` to inspectable bytes.
#[derive(Debug, Default)]
struct FakeAdapter {
    begin: Vec<Directive>,
    script: HashMap<(ConnectionState, i32), Vec<Directive>>,
    world_writes: HashMap<(ConnectionState, i32), Vec<WorldWrite>>,
}

impl FakeAdapter {
    fn new() -> Self {
        Self::default()
    }

    fn begin(mut self, directives: Vec<Directive>) -> Self {
        self.begin = directives;
        self
    }

    fn on(mut self, state: ConnectionState, packet_id: i32, directives: Vec<Directive>) -> Self {
        self.script.insert((state, packet_id), directives);
        self
    }

    /// Registers a world write applied to the sink when `(state, packet_id)` is
    /// handled, before the scripted directives are returned.
    fn world_write(mut self, state: ConnectionState, packet_id: i32, write: WorldWrite) -> Self {
        self.world_writes
            .entry((state, packet_id))
            .or_default()
            .push(write);
        self
    }
}

impl lodestone_client::VersionAdapter for FakeAdapter {
    fn protocol_version(&self) -> i32 {
        0
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["fake"]
    }

    fn supports(&self, _protocol: i32) -> bool {
        true
    }

    fn begin_login(
        &self,
        _profile: &LoginProfile,
        _server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(self.begin.clone())
    }

    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        // Apply any canned world writes in place first — exactly as a real
        // adapter decodes a chunk packet straight into the sink — then return
        // the lightweight scripted directives (e.g. a `ChunkLoaded` notice).
        if let Some(writes) = self.world_writes.get(&(state, packet_id)) {
            for write in writes {
                match write {
                    WorldWrite::Load(pos, chunk) => world.load(*pos, chunk.clone()),
                    WorldWrite::Unload(pos) => world.unload(*pos),
                }
            }
        }
        Ok(self
            .script
            .get(&(state, packet_id))
            .cloned()
            .unwrap_or_default())
    }

    fn encode_action(
        &self,
        _state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        match action {
            ClientAction::Move {
                pos,
                rotation,
                on_ground,
                // This fake adapter mirrors the real ones' wire body byte for
                // byte; it doesn't need horizontal-collision for the
                // read-model behaviour under test here.
                horizontal_collision: _,
            } => {
                let mut payload = Vec::new();
                payload.extend_from_slice(&pos.x.to_be_bytes());
                payload.extend_from_slice(&pos.y.to_be_bytes());
                payload.extend_from_slice(&pos.z.to_be_bytes());
                payload.extend_from_slice(&rotation.yaw.to_be_bytes());
                payload.extend_from_slice(&rotation.pitch.to_be_bytes());
                payload.push(u8::from(*on_ground));
                Ok(Some((MOVE_ID, payload)))
            }
            ClientAction::KeepAliveResponse { id } => {
                Ok(Some((KEEPALIVE_RESP_ID, id.to_be_bytes().to_vec())))
            }
            _ => Ok(None),
        }
    }
}

/// Decodes a `Move` payload written by [`FakeAdapter::encode_action`].
fn decode_move(payload: &[u8]) -> (Vec3, Rotation, bool) {
    let f64_at = |i: usize| f64::from_be_bytes(payload[i..i + 8].try_into().unwrap());
    let f32_at = |i: usize| f32::from_be_bytes(payload[i..i + 4].try_into().unwrap());
    let pos = Vec3::new(f64_at(0), f64_at(8), f64_at(16));
    let rotation = Rotation::new(f32_at(24), f32_at(28));
    (pos, rotation, payload[32] != 0)
}

fn profile() -> LoginProfile {
    LoginProfile {
        username: "Tester".into(),
        uuid: Uuid::nil(),
    }
}

fn server() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

fn dim(path: &str) -> Identifier {
    Identifier::new("minecraft", path).unwrap()
}

fn start(
    adapter: FakeAdapter,
) -> (
    lodestone_client::ClientHandle,
    lodestone_client::EventStream,
    Connection<tokio::io::DuplexStream>,
) {
    let (client_io, server_io) = memory_pair();
    let (handle, events) =
        ClientBuilder::new(server(), profile(), Box::new(adapter)).connect_with(client_io);
    (handle, events, Connection::new(server_io))
}

/// Builds a single-chunk [`LoadedChunk`] with one non-air block placed at local
/// `(x, y, z)`.
fn loaded_chunk_with_block(x: usize, y: i32, z: usize, id: u32) -> LoadedChunk {
    let mut column = ChunkColumn::new(
        0,
        16,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        0,
        0,
    );
    column.set_block(x, y, z, id);
    LoadedChunk::new(column, ColumnLight::new(16), Heightmaps::new(), Vec::new())
}

/// The read-model folds login, health, and teleport into queryable player
/// state.
#[tokio::test]
async fn read_model_folds_player_state() {
    const TRIGGER: i32 = 1;
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            TRIGGER,
            vec![
                Directive::Emit(ClientEvent::Login {
                    entity_id: 7,
                    game_mode: GameMode::Creative,
                    dimension: dim("overworld"),
                }),
                Directive::Emit(ClientEvent::HealthChanged {
                    health: 18.0,
                    food: 15,
                    saturation: 2.5,
                }),
                Directive::Emit(ClientEvent::TeleportPlayer {
                    pos: Vec3::new(1.0, 64.0, 2.0),
                    rotation: Rotation::new(90.0, 0.0),
                    flags: TeleportFlags::default(),
                }),
            ],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(TRIGGER, &[]).await.unwrap();
    // Draining the three surfaced events guarantees the driver has folded them.
    for _ in 0..3 {
        events.recv().await.unwrap();
    }

    let player = handle.player();
    assert_eq!(player.entity_id, Some(7));
    assert_eq!(player.game_mode, Some(GameMode::Creative));
    assert_eq!(player.dimension, Some(dim("overworld")));
    assert_eq!(handle.position(), Some(Vec3::new(1.0, 64.0, 2.0)));
    assert_eq!(handle.rotation(), Rotation::new(90.0, 0.0));
    assert_eq!(handle.health(), Some(18.0));
    assert_eq!(handle.food(), Some(15));
    assert!(handle.is_alive());

    drop(handle);
}

/// A relative teleport adds to the previous position rather than replacing it.
#[tokio::test]
async fn relative_teleport_is_applied_as_delta() {
    const TRIGGER: i32 = 1;
    let flags = TeleportFlags {
        relative_x: true,
        relative_y: false,
        relative_z: true,
        relative_yaw: false,
        relative_pitch: false,
    };
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            TRIGGER,
            vec![
                Directive::Emit(ClientEvent::TeleportPlayer {
                    pos: Vec3::new(10.0, 64.0, 10.0),
                    rotation: Rotation::default(),
                    flags: TeleportFlags::default(),
                }),
                Directive::Emit(ClientEvent::TeleportPlayer {
                    pos: Vec3::new(5.0, 70.0, -3.0),
                    rotation: Rotation::default(),
                    flags,
                }),
            ],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(TRIGGER, &[]).await.unwrap();
    for _ in 0..2 {
        events.recv().await.unwrap();
    }

    // x,z relative (10+5, 10-3); y absolute (70).
    assert_eq!(handle.position(), Some(Vec3::new(15.0, 70.0, 7.0)));
    drop(handle);
}

/// A decoded chunk applied through the [`WorldSink`] populates the world store
/// and is queryable by block, while the `ChunkLoaded` notification that follows
/// carries only a position — the heavy payload never touches the event channel.
/// A trailing scalar event proves the channel only saw the lightweight notice.
#[tokio::test]
async fn chunk_applied_through_sink_and_only_notice_is_forwarded() {
    const TRIGGER: i32 = 1;
    let chunk = loaded_chunk_with_block(3, 65, 4, 42);
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .world_write(
            ConnectionState::Play,
            TRIGGER,
            WorldWrite::Load(WorldChunkPos::new(0, 0), chunk),
        )
        .on(
            ConnectionState::Play,
            TRIGGER,
            vec![
                Directive::Emit(ClientEvent::ChunkLoaded {
                    pos: ChunkPos::new(0, 0),
                }),
                Directive::Emit(ClientEvent::Chat {
                    text: Text::literal("after"),
                    kind: ChatKind::System,
                    ack: None,
                }),
            ],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(TRIGGER, &[]).await.unwrap();

    handle
        .wait_for_chunk(ChunkPos::new(0, 0), Duration::from_secs(2))
        .await
        .expect("chunk should load");

    assert_eq!(handle.loaded_chunk_count(), 1);
    assert!(handle.is_chunk_loaded(ChunkPos::new(0, 0)));
    assert_eq!(handle.block_at(BlockPos::new(3, 65, 4)), Some(42));
    // Air within the loaded chunk reads back as the air id.
    assert_eq!(handle.block_at(BlockPos::new(0, 65, 0)), Some(0));
    // A block in an unloaded chunk is unknown, not a silent zero.
    assert_eq!(handle.block_at(BlockPos::new(100, 65, 100)), None);

    // The channel carries the lightweight ChunkLoaded notice, then the trailing
    // Chat — but never the chunk payload, which reached the world via the sink.
    match events.recv().await.unwrap() {
        ClientEvent::ChunkLoaded { pos } => assert_eq!(pos, ChunkPos::new(0, 0)),
        other => panic!("expected the ChunkLoaded notice, got {other:?}"),
    }
    match events.recv().await.unwrap() {
        ClientEvent::Chat { text, .. } => assert_eq!(text.to_plain_string(), "after"),
        other => panic!("expected the trailing Chat, got {other:?}"),
    }

    drop(handle);
}

/// `section_at` / `sections_at` hand out owned `Arc<ChunkSection>` snapshots
/// whose validity is independent of the world lock. This is the property the
/// mesher depends on: it takes the 27-section neighbourhood's snapshots, releases
/// the lock immediately, and meshes off them while chunk loading continues — so a
/// section snapshot taken before an unload must keep reading valid data after the
/// chunk is gone from the world. Asserting that invariant (which comes from
/// `Arc`, not from my own read-model bookkeeping) is what proves the surface
/// can't re-introduce consumer<->world coupling.
#[tokio::test]
async fn section_snapshot_outlives_unload() {
    const LOAD: i32 = 1;
    const UNLOAD: i32 = 2;
    // Column min_y = 0, 16 sections. A block at column-y 65 lives in section
    // index 65 / 16 = 4, at section-local y 65 % 16 = 1.
    let chunk = loaded_chunk_with_block(3, 65, 4, 42);
    let pos = ChunkPos::new(0, 0);
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .world_write(
            ConnectionState::Play,
            LOAD,
            WorldWrite::Load(WorldChunkPos::new(0, 0), chunk),
        )
        .on(
            ConnectionState::Play,
            LOAD,
            vec![Directive::Emit(ClientEvent::ChunkLoaded { pos })],
        )
        .world_write(
            ConnectionState::Play,
            UNLOAD,
            WorldWrite::Unload(WorldChunkPos::new(0, 0)),
        )
        .on(
            ConnectionState::Play,
            UNLOAD,
            vec![Directive::Emit(ClientEvent::ChunkUnloaded { pos })],
        );
    let (handle, _events, mut peer) = start(adapter);

    peer.write_packet(LOAD, &[]).await.unwrap();
    handle
        .wait_for_chunk(pos, Duration::from_secs(2))
        .await
        .expect("chunk should load");

    // The section snapshot the mesher would hold for the whole mesh.
    let held: Arc<ChunkSection> = handle.section_at(pos, 4).expect("section 4 present");
    assert_eq!(held.get_block(3, 1, 4), 42);

    // A batch query returns one aligned slot per request: the populated section,
    // an all-air (elided) section as None, and an absent chunk as None — never a
    // silent empty section.
    let batch = handle.sections_at(&[(pos, 4), (pos, 0), (ChunkPos::new(9, 9), 4)]);
    assert_eq!(batch.len(), 3);
    assert!(
        batch[0].as_ref().is_some_and(|s| Arc::ptr_eq(s, &held)),
        "batch hands back the same shared section as section_at, no copy"
    );
    assert!(batch[1].is_none(), "all-air section is a None slot");
    assert!(batch[2].is_none(), "absent chunk is a None slot");

    // Unload the chunk out from under the holder.
    peer.write_packet(UNLOAD, &[]).await.unwrap();
    handle
        .wait_for(Duration::from_secs(2), |h| !h.is_chunk_loaded(pos))
        .await
        .expect("chunk should unload");
    assert_eq!(handle.loaded_chunk_count(), 0);
    assert!(handle.section_at(pos, 4).is_none());

    // The snapshot taken before the unload is unaffected: the mesher keeps
    // reading valid section data with no lock held and no coupling to world
    // liveness.
    assert_eq!(
        held.get_block(3, 1, 4),
        42,
        "held section snapshot outlives the unload"
    );

    drop(handle);
}
#[tokio::test]
async fn entities_are_tracked_moved_and_removed() {
    const TRIGGER: i32 = 1;
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            TRIGGER,
            vec![
                Directive::Emit(ClientEvent::EntitySpawned {
                    entity_id: 10,
                    uuid: Some(Uuid::from_u128(1)),
                    entity_type: dim("pig"),
                    pos: Vec3::new(0.0, 64.0, 0.0),
                    rotation: Rotation::default(),
                    velocity: None,
                }),
                Directive::Emit(ClientEvent::EntitySpawned {
                    entity_id: 11,
                    uuid: None,
                    entity_type: dim("cow"),
                    pos: Vec3::new(5.0, 64.0, 5.0),
                    rotation: Rotation::default(),
                    velocity: None,
                }),
                Directive::Emit(ClientEvent::EntityMoved {
                    entity_id: 10,
                    movement: EntityMovement::Relative(Vec3::new(1.0, 0.0, 0.0)),
                    rotation: None,
                    on_ground: true,
                }),
                Directive::Emit(ClientEvent::EntityRemoved {
                    entity_ids: vec![11],
                }),
            ],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(TRIGGER, &[]).await.unwrap();
    for _ in 0..4 {
        events.recv().await.unwrap();
    }

    let entities = handle.entities();
    assert_eq!(entities.len(), 1, "entity 11 was removed");
    let pig = handle.entity(10).expect("entity 10 present");
    assert_eq!(pig.position, Vec3::new(1.0, 64.0, 0.0));
    assert!(pig.on_ground);
    assert!(handle.entity(11).is_none());

    drop(handle);
}

/// Head yaw arrives separately from body rotation (`add_entity`'s initial
/// value, then independent `rotate_head` updates) and equipment slots replace
/// by slot key, mirroring how attribute snapshots already replace by id. A
/// slot that is never reported must stay *absent*, not collapse into a
/// `None`-item entry — those are different states (no override vs. an
/// explicit "this slot is empty").
#[tokio::test]
async fn entity_head_yaw_and_equipment_are_tracked() {
    const TRIGGER: i32 = 1;
    let diamond_sword = dim("diamond_sword");
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            TRIGGER,
            vec![
                Directive::Emit(ClientEvent::EntitySpawned {
                    entity_id: 20,
                    uuid: None,
                    entity_type: dim("zombie"),
                    pos: Vec3::new(0.0, 64.0, 0.0),
                    rotation: Rotation::new(0.0, 0.0),
                    velocity: None,
                }),
                // The initial head yaw a real add_entity would carry.
                Directive::Emit(ClientEvent::EntityHeadRotation {
                    entity_id: 20,
                    head_yaw: 45.0,
                }),
                // The mob turns its head further while its body keeps facing
                // forward — body rotation must stay untouched.
                Directive::Emit(ClientEvent::EntityHeadRotation {
                    entity_id: 20,
                    head_yaw: 90.0,
                }),
                Directive::Emit(ClientEvent::EntityEquipmentUpdated {
                    entity_id: 20,
                    equipment: vec![EntityEquipment {
                        slot: EquipmentSlot::MainHand,
                        item: Some(ItemStack {
                            item: diamond_sword.clone(),
                            count: 1,
                        }),
                    }],
                }),
                // A later update replaces MainHand and adds Head; OffHand is
                // never mentioned and must remain absent.
                Directive::Emit(ClientEvent::EntityEquipmentUpdated {
                    entity_id: 20,
                    equipment: vec![
                        EntityEquipment {
                            slot: EquipmentSlot::MainHand,
                            item: None, // explicitly emptied, not "unknown"
                        },
                        EntityEquipment {
                            slot: EquipmentSlot::Head,
                            item: Some(ItemStack {
                                item: dim("iron_helmet"),
                                count: 1,
                            }),
                        },
                    ],
                }),
            ],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(TRIGGER, &[]).await.unwrap();
    for _ in 0..6 {
        events.recv().await.unwrap();
    }

    let zombie = handle.entity(20).expect("entity 20 present");
    assert_eq!(zombie.head_yaw, 90.0, "the latest rotate_head wins");
    assert_eq!(
        zombie.rotation,
        Rotation::new(0.0, 0.0),
        "body rotation is untouched by head_yaw updates"
    );
    assert_eq!(
        zombie.equipment.len(),
        2,
        "MainHand replaced in place, Head appended; OffHand never reported"
    );
    let main_hand = zombie
        .equipment
        .iter()
        .find(|e| e.slot == EquipmentSlot::MainHand)
        .expect("MainHand entry present");
    assert_eq!(
        main_hand.item, None,
        "explicitly emptied slot, not defaulted"
    );
    let head = zombie
        .equipment
        .iter()
        .find(|e| e.slot == EquipmentSlot::Head)
        .expect("Head entry present");
    assert_eq!(
        head.item,
        Some(ItemStack {
            item: dim("iron_helmet"),
            count: 1,
        })
    );
    assert!(
        zombie
            .equipment
            .iter()
            .all(|e| e.slot != EquipmentSlot::OffHand),
        "a slot that was never reported must stay absent, not appear as None"
    );

    drop(handle);
}

/// Metadata folds *incrementally* — a later partial update must not clobber
/// fields an earlier one set — and attribute snapshots replace by id. The live
/// gate proves the real adapter emits these, but only over a single fully-formed
/// packet; this isolates the merge logic the happy path can't exercise.
#[tokio::test]
async fn entity_metadata_and_attributes_merge_incrementally() {
    const TRIGGER: i32 = 1;
    let speed = dim("movement_speed");
    let follow = dim("follow_range");
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            TRIGGER,
            vec![
                Directive::Emit(ClientEvent::EntitySpawned {
                    entity_id: 10,
                    uuid: None,
                    entity_type: dim("pig"),
                    pos: Vec3::new(0.0, 64.0, 0.0),
                    rotation: Rotation::default(),
                    velocity: None,
                }),
                // First metadata packet: name + health.
                Directive::Emit(ClientEvent::EntityMetadataUpdated {
                    entity_id: 10,
                    metadata: EntityMetadataUpdate {
                        custom_name: Some(Some("Babe".to_string())),
                        health: Some(6.0),
                        ..Default::default()
                    },
                }),
                // Second packet mentions only pose + baby: it must NOT wipe the
                // name/health the first packet set.
                Directive::Emit(ClientEvent::EntityMetadataUpdated {
                    entity_id: 10,
                    metadata: EntityMetadataUpdate {
                        pose: Some(EntityPose::Standing),
                        baby: Some(true),
                        ..Default::default()
                    },
                }),
                // Attributes arrive, then a later packet replaces movement_speed
                // and adds follow_range: replace-by-id, not append-duplicate.
                Directive::Emit(ClientEvent::EntityAttributesUpdated {
                    entity_id: 10,
                    attributes: vec![EntityAttributeSnapshot {
                        attribute: speed.clone(),
                        base: 0.25,
                        modifiers: vec![],
                    }],
                }),
                Directive::Emit(ClientEvent::EntityAttributesUpdated {
                    entity_id: 10,
                    attributes: vec![
                        EntityAttributeSnapshot {
                            attribute: speed.clone(),
                            base: 0.30,
                            modifiers: vec![EntityAttributeModifier {
                                id: Identifier::new("lodestone", "boost").unwrap(),
                                amount: 0.1,
                                operation: 0,
                            }],
                        },
                        EntityAttributeSnapshot {
                            attribute: follow.clone(),
                            base: 16.0,
                            modifiers: vec![],
                        },
                    ],
                }),
            ],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(TRIGGER, &[]).await.unwrap();
    for _ in 0..5 {
        events.recv().await.unwrap();
    }

    let pig = handle.entity(10).expect("entity 10 present");
    // Incremental merge: every field from both packets survives.
    assert_eq!(pig.custom_name, Some(Some("Babe".to_string())));
    assert_eq!(pig.health, Some(6.0));
    assert_eq!(pig.pose, Some(EntityPose::Standing));
    assert_eq!(pig.baby, Some(true));
    // Replace-by-id: two distinct attributes, movement_speed updated to the
    // later snapshot (base 0.30 with its modifier), no duplicate.
    assert_eq!(
        pig.attributes.len(),
        2,
        "replace-by-id, not append: {:?}",
        pig.attributes
    );
    let ms = pig
        .attributes
        .iter()
        .find(|a| a.attribute == speed)
        .expect("movement_speed present");
    assert_eq!(ms.base, 0.30);
    assert_eq!(ms.modifiers.len(), 1);
    assert!(pig.attributes.iter().any(|a| a.attribute == follow));

    drop(handle);
}
#[tokio::test]
async fn wait_for_times_out() {
    let adapter = FakeAdapter::new().begin(vec![Directive::SetState(ConnectionState::Play)]);
    let (handle, _events, _peer) = start(adapter);

    let result = handle
        .wait_for(Duration::from_millis(100), |h| h.health().is_some())
        .await;
    assert_eq!(result, Err(WaitError::Timeout));

    drop(handle);
}

/// `wait_for` wakes as soon as a driver state change satisfies its predicate.
#[tokio::test]
async fn wait_for_wakes_on_state_change() {
    const TRIGGER: i32 = 1;
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            TRIGGER,
            vec![Directive::Emit(ClientEvent::HealthChanged {
                health: 20.0,
                food: 20,
                saturation: 5.0,
            })],
        );
    let (handle, _events, mut peer) = start(adapter);

    let waiter = handle.wait_for(Duration::from_secs(2), |h| h.health() == Some(20.0));
    let poke = async {
        // Small delay so the waiter registers first, exercising the wake path.
        tokio::time::sleep(Duration::from_millis(20)).await;
        peer.write_packet(TRIGGER, &[]).await.unwrap();
    };
    let (result, ()) = tokio::join!(waiter, poke);
    result.expect("waiter should wake on the health change");
    assert_eq!(handle.health(), Some(20.0));

    drop(handle);
}

/// Movement helpers that need a position return a typed error before the server
/// has placed the player, rather than silently no-op.
#[tokio::test]
async fn movement_before_position_is_a_typed_error() {
    let adapter = FakeAdapter::new().begin(vec![Directive::SetState(ConnectionState::Play)]);
    let (handle, _events, _peer) = start(adapter);

    assert_eq!(
        handle.look_at(Vec3::new(0.0, 0.0, 0.0)),
        Err(lodestone_client::BotError::PositionUnknown)
    );
    assert_eq!(
        handle.step_toward(Vec3::new(1.0, 0.0, 0.0), 1.0),
        Err(lodestone_client::BotError::PositionUnknown)
    );

    drop(handle);
}

/// A move action reaches the wire encoded by the adapter, and the read-model
/// predicts the new position optimistically. The wire bytes — authored by the
/// adapter, not the read-model — are the source of truth.
#[tokio::test]
async fn set_position_hits_wire_and_predicts_locally() {
    const TRIGGER: i32 = 1;
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            TRIGGER,
            vec![Directive::Emit(ClientEvent::TeleportPlayer {
                pos: Vec3::new(0.0, 64.0, 0.0),
                rotation: Rotation::default(),
                flags: TeleportFlags::default(),
            })],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(TRIGGER, &[]).await.unwrap();
    events.recv().await.unwrap(); // teleport folded

    handle.set_position(Vec3::new(5.0, 64.0, 5.0)).unwrap();

    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, MOVE_ID);
    let (pos, _rot, _on_ground) = decode_move(&payload);
    assert_eq!(pos, Vec3::new(5.0, 64.0, 5.0));

    // Optimistic local prediction reflects the sent position.
    assert_eq!(handle.position(), Some(Vec3::new(5.0, 64.0, 5.0)));

    drop(handle);
}

/// The per-tick primitive `move_to` forwards the caller's *complete* movement
/// state — position, rotation and ground contact — verbatim to the wire, and
/// predicts locally. Unlike `set_position` (which reuses the read-model's
/// rotation), this proves rotation and `on_ground` are caller-supplied, which is
/// exactly what a tick-driven controller needs.
#[tokio::test]
async fn move_to_forwards_full_state_to_wire_and_predicts() {
    const TRIGGER: i32 = 1;
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            TRIGGER,
            vec![Directive::Emit(ClientEvent::TeleportPlayer {
                pos: Vec3::new(0.0, 64.0, 0.0),
                rotation: Rotation::default(),
                flags: TeleportFlags::default(),
            })],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(TRIGGER, &[]).await.unwrap();
    events.recv().await.unwrap(); // teleport folded

    let rotation = Rotation::new(90.0, -30.0);
    handle
        .move_to(Vec3::new(1.0, 65.0, -2.0), rotation, true, false)
        .unwrap();

    let (id, payload) = peer.read_packet().await.unwrap().unwrap();
    assert_eq!(id, MOVE_ID);
    let (pos, rot, on_ground) = decode_move(&payload);
    assert_eq!(pos, Vec3::new(1.0, 65.0, -2.0));
    assert_eq!(rot, rotation, "move_to must forward the caller's rotation");
    assert!(
        on_ground,
        "move_to must forward the caller's on_ground flag"
    );

    assert_eq!(handle.position(), Some(Vec3::new(1.0, 65.0, -2.0)));

    drop(handle);
}

/// Awaiting a bot condition must not stall packet processing or keep-alives: a
/// keep-alive is still answered on the wire while a `wait_for` is pending.
#[tokio::test]
async fn keep_alive_answered_while_a_wait_is_pending() {
    const KA_TRIGGER: i32 = 2;
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            KA_TRIGGER,
            vec![Directive::Emit(ClientEvent::KeepAlive { id: 99 })],
        );
    let (handle, _events, mut peer) = start(adapter);

    let poke = async {
        peer.write_packet(KA_TRIGGER, &[]).await.unwrap();
        let (id, payload) = peer.read_packet().await.unwrap().unwrap();
        assert_eq!(id, KEEPALIVE_RESP_ID);
        assert_eq!(payload, 99i64.to_be_bytes());
    };

    // The waiter's condition never becomes true; `select!` lets `poke` finish
    // first, proving the driver answered the keep-alive despite the pending wait.
    tokio::select! {
        _ = handle.wait_for(Duration::from_secs(5), |_| false) => {
            panic!("wait_for should not have resolved");
        }
        () = poke => {}
    }

    drop(handle);
}

/// Builds minimal team parameters; the fold only stores these verbatim, so the
/// exact values matter only for the round-trip identity assertion.
fn team_params(color: Option<lodestone_client::TeamColor>) -> TeamParameters {
    TeamParameters {
        display_name: Text::literal("team"),
        prefix: Text::literal(""),
        suffix: Text::literal(""),
        name_tag_visibility: Visibility::Always,
        collision_rule: CollisionRule::Always,
        color,
        friendly_fire: true,
        see_friendly_invisibles: false,
    }
}

/// Objectives, their scores and a display-slot assignment all fold into the
/// queryable scoreboard, and scores come back in sidebar render order (value
/// descending). None of these values echo the read-model's own defaults — they
/// come straight off the emitted events.
#[tokio::test]
async fn scoreboard_folds_objectives_scores_and_display() {
    const TRIGGER: i32 = 1;
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            TRIGGER,
            vec![
                Directive::Emit(ClientEvent::ObjectiveUpdate {
                    name: "kills".into(),
                    mode: ObjectiveMode::Add,
                    display_name: Some(Text::literal("Kills")),
                    render_type: Some(ObjectiveRenderType::Integer),
                    number_format: Some(NumberFormat::Default),
                }),
                Directive::Emit(ClientEvent::DisplayObjective {
                    slot: DisplaySlot::Sidebar,
                    objective: Some("kills".into()),
                }),
                Directive::Emit(ClientEvent::ScoreUpdate {
                    holder: "Alice".into(),
                    objective: "kills".into(),
                    value: 5,
                    display: None,
                    number_format: None,
                }),
                Directive::Emit(ClientEvent::ScoreUpdate {
                    holder: "Bob".into(),
                    objective: "kills".into(),
                    value: 12,
                    display: None,
                    number_format: None,
                }),
            ],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(TRIGGER, &[]).await.unwrap();
    for _ in 0..4 {
        events.recv().await.unwrap();
    }

    let sb = handle.scoreboard();
    let obj = sb.objective("kills").expect("objective folded");
    assert_eq!(obj.display_name, Some(Text::literal("Kills")));
    assert_eq!(obj.render_type, Some(ObjectiveRenderType::Integer));
    assert_eq!(sb.displayed(DisplaySlot::Sidebar), Some("kills"));

    // Sidebar order: Bob(12) before Alice(5), highest first.
    let scores = sb.scores("kills");
    assert_eq!(scores.len(), 2);
    assert_eq!(scores[0].holder, "Bob");
    assert_eq!(scores[0].value, 12);
    assert_eq!(scores[1].holder, "Alice");
    assert_eq!(scores[1].value, 5);
    // The slot resolver returns the same order for the objective on the slot.
    assert_eq!(sb.scores_in_slot(DisplaySlot::Sidebar), scores);
    assert_eq!(sb.score("kills", "Alice").map(|s| s.value), Some(5));

    drop(handle);
}

/// Removing an objective must purge its scores *and* any display slot pointing
/// at it — a self-echo store that only inserted would leave both behind. The
/// pre-populated state proves the purge did work, not that it was never set.
#[tokio::test]
async fn objective_remove_purges_scores_and_display() {
    const SETUP: i32 = 1;
    const REMOVE: i32 = 2;
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            SETUP,
            vec![
                Directive::Emit(ClientEvent::ObjectiveUpdate {
                    name: "kills".into(),
                    mode: ObjectiveMode::Add,
                    display_name: Some(Text::literal("Kills")),
                    render_type: None,
                    number_format: None,
                }),
                Directive::Emit(ClientEvent::DisplayObjective {
                    slot: DisplaySlot::Sidebar,
                    objective: Some("kills".into()),
                }),
                Directive::Emit(ClientEvent::ScoreUpdate {
                    holder: "Alice".into(),
                    objective: "kills".into(),
                    value: 5,
                    display: None,
                    number_format: None,
                }),
            ],
        )
        .on(
            ConnectionState::Play,
            REMOVE,
            vec![Directive::Emit(ClientEvent::ObjectiveUpdate {
                name: "kills".into(),
                mode: ObjectiveMode::Remove,
                display_name: None,
                render_type: None,
                number_format: None,
            })],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(SETUP, &[]).await.unwrap();
    for _ in 0..3 {
        events.recv().await.unwrap();
    }
    // Precondition: everything is present before the remove.
    let before = handle.scoreboard();
    assert!(before.objective("kills").is_some());
    assert_eq!(before.displayed(DisplaySlot::Sidebar), Some("kills"));
    assert_eq!(before.scores("kills").len(), 1);

    peer.write_packet(REMOVE, &[]).await.unwrap();
    events.recv().await.unwrap();

    let after = handle.scoreboard();
    assert!(after.objective("kills").is_none());
    assert!(after.scores("kills").is_empty());
    assert_eq!(after.displayed(DisplaySlot::Sidebar), None);

    drop(handle);
}

/// A `ScoreReset` with no objective clears the holder from *every* objective;
/// one naming an objective clears only that one. A second holder is the
/// negative control: it must survive both resets.
#[tokio::test]
async fn score_reset_clears_holder_selectively() {
    const SETUP: i32 = 1;
    const RESET_ONE: i32 = 2;
    const RESET_ALL: i32 = 3;
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            SETUP,
            vec![
                Directive::Emit(ClientEvent::ObjectiveUpdate {
                    name: "kills".into(),
                    mode: ObjectiveMode::Add,
                    display_name: None,
                    render_type: None,
                    number_format: None,
                }),
                Directive::Emit(ClientEvent::ObjectiveUpdate {
                    name: "deaths".into(),
                    mode: ObjectiveMode::Add,
                    display_name: None,
                    render_type: None,
                    number_format: None,
                }),
                Directive::Emit(ClientEvent::ScoreUpdate {
                    holder: "Alice".into(),
                    objective: "kills".into(),
                    value: 5,
                    display: None,
                    number_format: None,
                }),
                Directive::Emit(ClientEvent::ScoreUpdate {
                    holder: "Alice".into(),
                    objective: "deaths".into(),
                    value: 2,
                    display: None,
                    number_format: None,
                }),
                Directive::Emit(ClientEvent::ScoreUpdate {
                    holder: "Bob".into(),
                    objective: "kills".into(),
                    value: 9,
                    display: None,
                    number_format: None,
                }),
            ],
        )
        .on(
            ConnectionState::Play,
            RESET_ONE,
            vec![Directive::Emit(ClientEvent::ScoreReset {
                holder: "Alice".into(),
                objective: Some("kills".into()),
            })],
        )
        .on(
            ConnectionState::Play,
            RESET_ALL,
            vec![Directive::Emit(ClientEvent::ScoreReset {
                holder: "Alice".into(),
                objective: None,
            })],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(SETUP, &[]).await.unwrap();
    for _ in 0..5 {
        events.recv().await.unwrap();
    }

    peer.write_packet(RESET_ONE, &[]).await.unwrap();
    events.recv().await.unwrap();
    let sb = handle.scoreboard();
    assert!(sb.score("kills", "Alice").is_none(), "kills cleared");
    assert_eq!(
        sb.score("deaths", "Alice").map(|s| s.value),
        Some(2),
        "deaths kept"
    );
    assert_eq!(
        sb.score("kills", "Bob").map(|s| s.value),
        Some(9),
        "Bob untouched"
    );

    peer.write_packet(RESET_ALL, &[]).await.unwrap();
    events.recv().await.unwrap();
    let sb = handle.scoreboard();
    assert!(
        sb.score("deaths", "Alice").is_none(),
        "reset-all clears remaining"
    );
    assert_eq!(
        sb.score("kills", "Bob").map(|s| s.value),
        Some(9),
        "Bob still untouched"
    );

    drop(handle);
}

/// Teams fold membership, and adding a holder to a new team moves it off its
/// old one (the reverse index stays consistent). Removal clears the reverse
/// index too.
#[tokio::test]
async fn teams_fold_membership_and_moves() {
    const SETUP: i32 = 1;
    const MOVE: i32 = 2;
    const TEARDOWN: i32 = 3;
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            SETUP,
            vec![
                Directive::Emit(ClientEvent::TeamUpdate {
                    name: "red".into(),
                    action: TeamAction::Create {
                        params: Box::new(team_params(Some(lodestone_client::TeamColor::Red))),
                        members: vec!["Alice".into(), "Bob".into()],
                    },
                }),
                Directive::Emit(ClientEvent::TeamUpdate {
                    name: "blue".into(),
                    action: TeamAction::Create {
                        params: Box::new(team_params(Some(lodestone_client::TeamColor::Blue))),
                        members: vec![],
                    },
                }),
            ],
        )
        .on(
            ConnectionState::Play,
            MOVE,
            vec![Directive::Emit(ClientEvent::TeamUpdate {
                name: "blue".into(),
                action: TeamAction::AddMembers(vec!["Alice".into()]),
            })],
        )
        .on(
            ConnectionState::Play,
            TEARDOWN,
            vec![
                Directive::Emit(ClientEvent::TeamUpdate {
                    name: "blue".into(),
                    action: TeamAction::RemoveMembers(vec!["Alice".into()]),
                }),
                Directive::Emit(ClientEvent::TeamUpdate {
                    name: "red".into(),
                    action: TeamAction::Remove,
                }),
            ],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(SETUP, &[]).await.unwrap();
    for _ in 0..2 {
        events.recv().await.unwrap();
    }
    let sb = handle.scoreboard();
    assert_eq!(sb.team_of("Alice").map(|t| t.name.as_str()), Some("red"));
    assert_eq!(sb.team_of("Bob").map(|t| t.name.as_str()), Some("red"));

    peer.write_packet(MOVE, &[]).await.unwrap();
    events.recv().await.unwrap();
    let sb = handle.scoreboard();
    assert_eq!(
        sb.team_of("Alice").map(|t| t.name.as_str()),
        Some("blue"),
        "moved to blue"
    );
    assert_eq!(
        sb.team("red").unwrap().members,
        vec!["Bob".to_string()],
        "removed from red"
    );
    assert_eq!(sb.team("blue").unwrap().members, vec!["Alice".to_string()]);

    peer.write_packet(TEARDOWN, &[]).await.unwrap();
    for _ in 0..2 {
        events.recv().await.unwrap();
    }
    let sb = handle.scoreboard();
    assert!(sb.team_of("Alice").is_none(), "removed from blue");
    assert!(sb.team("red").is_none(), "red team gone");
    assert!(
        sb.team_of("Bob").is_none(),
        "red removal cleared reverse index"
    );

    drop(handle);
}

/// Boss bars keep server insertion order for rendering, updates mutate the
/// matching bar in place, and removal drops one without disturbing the order of
/// the rest.
#[tokio::test]
async fn boss_bars_insertion_ordered_and_mutated() {
    const ADD: i32 = 1;
    const UPDATE: i32 = 2;
    const REMOVE: i32 = 3;
    let id1 = Uuid::from_u128(1);
    let id2 = Uuid::from_u128(2);
    let id3 = Uuid::from_u128(3);
    let add = |id: Uuid, title: &str| {
        Directive::Emit(ClientEvent::BossBarUpdate {
            id,
            action: BossAction::Add {
                title: Box::new(Text::literal(title)),
                progress: 1.0,
                color: BossColor::Pink,
                overlay: BossOverlay::Progress,
                darken: false,
                music: false,
                fog: false,
            },
        })
    };
    let adapter = FakeAdapter::new()
        .begin(vec![Directive::SetState(ConnectionState::Play)])
        .on(
            ConnectionState::Play,
            ADD,
            vec![add(id1, "first"), add(id2, "second"), add(id3, "third")],
        )
        .on(
            ConnectionState::Play,
            UPDATE,
            vec![
                Directive::Emit(ClientEvent::BossBarUpdate {
                    id: id2,
                    action: BossAction::UpdateProgress(0.25),
                }),
                Directive::Emit(ClientEvent::BossBarUpdate {
                    id: id1,
                    action: BossAction::UpdateName(Box::new(Text::literal("renamed"))),
                }),
            ],
        )
        .on(
            ConnectionState::Play,
            REMOVE,
            vec![Directive::Emit(ClientEvent::BossBarUpdate {
                id: id2,
                action: BossAction::Remove,
            })],
        );
    let (handle, mut events, mut peer) = start(adapter);

    peer.write_packet(ADD, &[]).await.unwrap();
    for _ in 0..3 {
        events.recv().await.unwrap();
    }
    let bars = handle.boss_bars();
    assert_eq!(
        bars.iter().map(|b| b.id).collect::<Vec<_>>(),
        vec![id1, id2, id3],
        "insertion order preserved"
    );

    peer.write_packet(UPDATE, &[]).await.unwrap();
    for _ in 0..2 {
        events.recv().await.unwrap();
    }
    let bars = handle.boss_bars();
    assert_eq!(bars[1].progress, 0.25, "id2 progress updated");
    assert_eq!(bars[0].title, Text::literal("renamed"), "id1 title updated");

    peer.write_packet(REMOVE, &[]).await.unwrap();
    events.recv().await.unwrap();
    let bars = handle.boss_bars();
    assert_eq!(
        bars.iter().map(|b| b.id).collect::<Vec<_>>(),
        vec![id1, id3],
        "id2 removed, order of the rest intact"
    );

    drop(handle);
}
