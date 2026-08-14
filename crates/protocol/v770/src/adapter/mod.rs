//! [`VersionAdapter`] implementation driving the protocol 776 join flow.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lodestone_core::{
    Ctx, Decode, Encode, Reader, Writer, plain_text_from_nbt_component, read_network_nbt,
};
// The wire-shaped, decode-target command tree (issue #470). Deliberately *not*
// `lodestone-command`'s arena/`dyn ArgumentType` construction API — see #435 and
// `lodestone_model::command_tree`'s module doc for why the two stay separate.
use lodestone_model::command_tree::{
    ArgumentParser, CommandSuggestionEntry, CommandSuggestionsResponse, CommandTree, NodeKind,
    RawCommandNode, StringKind,
};
use lodestone_model::{
    AdapterError, AdvancementDisplay, AdvancementEntry, AdvancementFrame, AnimationAction,
    ArmorTrim, BlockAabb, BlockActionKind, BlockFace, BlockHardness,
    BlockPos,
    BossAction,
    BossColor,
    BossOverlay, ChatAckInfo, ChatCompletionsAction, ChatKind, ChatMode, ChunkPos, ClientAction,
    ClientEvent,
    ClientSettings, CollisionRule, CommandBlockMode, ConnectionState, ContainerClickType,
    ContainerSlotChange, DeathLocation, DebugSampleKind, Difficulty, DimensionTypeInfo, Directive,
    DisplaySlot,
    DisplayedSkinParts,
    EntityBaseDimensions,
    EntityEquipment,
    EntityFacts,
    EntityInteraction, EntityMetadataUpdate, EntityMovement, EntityVariant, EquipmentSlot,
    GameMode, Hand, ItemComponents,
    ItemEnchantment, ItemPrototype, ItemStack, ItemTool, JigsawJoint, LoginProfile,
    LookAnchor, MainHand, MapDecoration, MapPatch, MerchantOffer as ModelMerchantOffer,
    NumberFormat, ObjectiveMode, ObjectiveRenderType, PackedMessageSignature,
    ParticleStatus, PlayerCommand, PlayerInput, PlayerListEntry, PlayerLookAtEntity,
    PotDecorations, ProfileProperty as ModelProfileProperty,
    RecipeBookEntry, RecipeBookType,
    RecipeBookTypeSettings,
    ResourceKey, ResourcePackResponseKind, Rotation, SectionPos, ServerAddress, ServerLink,
    ServerLinkKind, SoundCategory, StatAward,
    StructureBlockMode, StructureBlockUpdateType, StructureMirror, StructureRotation, TeamAction,
    TeamColor, TeamParameters, TeleportFlags, TestBlockMode as ModelTestBlockMode,
    TestInstanceAction, TestInstanceData, TestInstanceStatus, Text, TextColor, ToolBlocks,
    ToolMining, ToolPatch, ToolRule, TrackedWaypoint, Vec3, Vec3f, VersionAdapter, Visibility,
    WaypointId, WaypointOperation, WaypointPosition, WorldSink,
};
use lodestone_world::{
    BiomePatch, ChunkPos as WorldChunkPos, LightPatch, LoadedChunk, NibbleArray, PalettedContainer,
};
use lodestone_game::chat_ack::{MessageSignature, MessageSignatureCache};

use lodestone_data::block_entity_types::block_entity_type;
use lodestone_data::block_states::block_type_name;
use crate::chunk_batch::ChunkBatchSizeCalculator;
use lodestone_data::data_component_types::component_type_name;
use lodestone_data::entity_types::entity_type_name;
use lodestone_data::items::{item_id, item_name};
use lodestone_data::menus::menu_name;
use lodestone_data::mob_effects::{mob_effect_id, mob_effect_name};
use crate::packet_ids::{configuration, handshaking, login, play};
use crate::packets::chunk::{ChunkShape, LevelChunkWithLight};
use crate::packets::common::{
    BrandPayload, ClientInformation, CookieResponse, KeepAlive, PingRequest, Pong,
    ResourcePackResponse, TeleportToEntity,
};
use crate::packets::configuration::{
    AcceptCodeOfConduct, FinishConfiguration, ServerboundKnownPacks,
};
use crate::packets::entity::{read_lp_vec3, unpack_degrees};
use crate::packets::game::{
    ABILITY_FLAG_CAN_FLY, ABILITY_FLAG_FLYING, ABILITY_FLAG_INSTABUILD, ABILITY_FLAG_INVULNERABLE,
    AcceptTeleportation, Attack, BlockEntityTagQuery, COMMAND_BLOCK_FLAG_AUTOMATIC,
    COMMAND_BLOCK_FLAG_CONDITIONAL,
    COMMAND_BLOCK_FLAG_TRACK_OUTPUT, ChangeGameMode, ChatAck, ChatCommand, ChatMessage,
    ChunkBatchFinished, ChunkBatchReceived, ClientCommand, ClientTickEnd, CommandSuggestion,
    ConfigurationAcknowledged, ContainerButtonClick, ContainerClose, ContainerSlotStateChanged,
    EditBook, EntityTagQuery, GameEvent, GameLogin, LevelEvent, LevelParticles,
    MOVE_FLAG_HORIZONTAL_COLLISION,
    MOVE_FLAG_ON_GROUND, MovePlayerPos, MovePlayerPosRot, MovePlayerRot, MovePlayerStatusOnly,
    MoveVehicle, PaddleBoat, PickItemFromBlock, PickItemFromEntity, PlaceRecipe, PlayerAbilities,
    PlayerAction, PlayerCommand as PlayerCommandPacket, PlayerInput as PlayerInputPacket,
    PlayerLoaded, RecipeBookChangeSettings, RecipeBookSeenRecipe, RenameItem, Respawn,
    SERVERBOUND_ABILITY_FLAG_FLYING, SelectBundleItem, SelectTrade, ServerboundPlayerAbilities,
    SetCarriedItem, SetCommandBlock, SetDefaultSpawnPosition, SetHealth, SignUpdate, Swing,
    UseItem, UseItemOn,
};
use crate::packets::handshake::Intention;
use crate::packets::login::{
    CustomQueryAnswer, EncryptionRequest, EncryptionResponse, LoginAcknowledged, LoginCompression,
    LoginDisconnect, LoginFinished,
};
use crate::packets::metadata::{
    MetadataClass, TrackedEntity, metadata_class, read_entity_metadata, read_update_attributes,
};
use crate::packets::player_info::{PlayerInfoRemove, PlayerInfoUpdate};
use crate::packets::registry::{ClientRegistries, DimensionType, RegistryData};
use crate::packets::scoreboard::{
    self as sb, BossEvent, ResetScore, SetDisplayObjective, SetObjective, SetPlayerTeam, SetScore,
};
use crate::packets::time::SetTime;
use lodestone_data::particle_types::particle_type_name;
use lodestone_data::sound_events::sound_event;

mod chat;
mod connection;
mod scoreboard;

use scoreboard::decode_play;

/// Protocol version implemented by this adapter.
pub const PROTOCOL: i32 = 776;

/// Fixed decoding/encoding context for protocol 776.
const CTX: Ctx = Ctx { version: PROTOCOL };

/// Requested next-state value in the handshake for a login connection.
const NEXT_STATE_LOGIN: i32 = 2;

/// Version adapter for the 770-family protocols, implementing protocol 776
/// (Minecraft 26.2).
///
/// The adapter carries a small amount of per-connection state: the current
/// dimension's [`ChunkShape`], needed to decode `level_chunk_with_light`
/// (chunk framing depends on the dimension's build-height window, which is not
/// carried in the chunk packet itself). It is set from the `login` and
/// `respawn` packets and defaults to the overworld. One adapter instance is
/// created per connection and driven sequentially by the client, so the state
/// is guarded by a [`Mutex`] purely to satisfy `Sync`; there is no contention.
#[derive(Debug, Clone)]
pub struct V770Adapter {
    shape: Arc<Mutex<ChunkShape>>,
    batch: Arc<Mutex<ChunkBatchState>>,
    movement: Arc<Mutex<MovementSendState>>,
    /// Tracks the concrete type of spawned entities whose cosmetic variant lives
    /// at a metadata index that other mobs reuse (sheep wool @ 17, horse variant
    /// @ 18). Only these ambiguous classes are stored, bounding the map to the
    /// mobs actually present; self-identifying registry-holder variants need no
    /// entry. Populated on `add_entity`, cleared on `remove_entities`.
    variants: Arc<Mutex<HashMap<i32, TrackedEntity>>>,
    /// The overworld day clock, held across packets because `set_time` mostly
    /// does **not** carry it. See [`DayClock`].
    clock: Arc<Mutex<DayClock>>,
    /// Registries folded out of the Configuration `registry_data` stream (issue
    /// #288). Empty until Configuration runs; every reader falls back
    /// explicitly, because a server that sends none must still play.
    registries: Arc<Mutex<ClientRegistries>>,
    /// Holder id of the `minecraft:world_clock` entry the **current dimension**
    /// follows, resolved at `login`/`respawn` from the dimension type's
    /// `default_clock`.
    ///
    /// `None` means "not resolved" — either no `registry_data` arrived, or the
    /// dimension type has no clock of its own (the Nether, which has fixed
    /// time). Both fall back to `SetTime::day_clock`'s lowest-holder-id pick;
    /// see the `set_time` arm.
    clock_holder: Arc<Mutex<Option<i32>>>,
    /// The client's 128-entry signed-chat signature cache (issue #286). Packed
    /// ids in `PLAYER_CHAT`'s last-seen list and in `delete_chat` index into it;
    /// every received signed body is pushed back so future ids resolve. Guarded
    /// by a [`Mutex`] only to satisfy `Sync`, like the other per-connection
    /// state above.
    chat_cache: Arc<Mutex<MessageSignatureCache>>,
}

/// The client's copy of the server's overworld day clock.
///
/// # Why any state is needed here at all
///
/// 26.2's `set_time` is `(gameTime, Map<Holder<WorldClock>, ClockNetworkState>)`,
/// and the map is **empty in almost every packet**: the once-a-second
/// `MinecraftServer::forceGameTimeSynchronization` sends `Map.of()`, while
/// `ServerClockManager::modifyClock` sends a one-entry map only when a clock
/// changes and `createFullSyncPacket` sends the full map once, at join. So a
/// stateless adapter has no day time to report for 19 packets out of 20, and the
/// previous code filled that hole with the monotonic world age — which pinned
/// `sky_darken_for_time_of_day` to one value for the whole session. See
/// [`SetTime::day_clock`] for the measurement.
///
/// Vanilla's client has the same problem and solves it the same way: it holds the
/// clock and advances it locally, the server only correcting it. Here the
/// correction *and* the elapsed-tick reference both ride on `set_time`'s own
/// `gameTime`, so no local tick loop is needed — `time_of_day` simply advances in
/// ~20-tick steps, one per sync. That granularity is invisible in the only
/// consumer (`sky_darken_for_time_of_day`, whose curve moves over thousands of
/// ticks).
///
/// # How to change it
///
/// If a second clock ever needs to be surfaced (the End clock, id `1`), widen
/// this to a map keyed by `holder_id` and pick per dimension; `ClientEvent`'s
/// single `time_of_day` field is the constraint, not this struct.
#[derive(Debug, Clone, Copy)]
struct DayClock {
    /// The clock's tick count at [`Self::at_game_time`].
    total_ticks: i64,
    /// Ticks of clock per tick of world age. **`0.0` means paused** — that is
    /// how `/gamerule advanceTime false` and a paused clock arrive on the wire.
    rate: f32,
    /// The `set_time.game_time` this anchor was taken at.
    at_game_time: i64,
    /// Whether a real clock update has ever been seen. Until it has, the anchor
    /// is seeded from the world age, reproducing the old behaviour exactly
    /// rather than reporting a confidently wrong `0`. In practice this window is
    /// closed by the join-time full sync before any gameplay packet arrives.
    synced: bool,
}

impl DayClock {
    /// The clock's tick count at world age `game_time`, extrapolated from the
    /// anchor at the server's own rate. Never runs backwards: a `game_time`
    /// behind the anchor (a re-anchor from a *later* packet, or a wrapped clock)
    /// contributes zero rather than a negative offset.
    fn time_of_day(&self, game_time: i64) -> i64 {
        let elapsed = game_time.saturating_sub(self.at_game_time).max(0);
        #[allow(clippy::cast_possible_truncation)]
        let advanced = (elapsed as f64 * f64::from(self.rate)) as i64;
        self.total_ticks.saturating_add(advanced)
    }
}

impl Default for DayClock {
    fn default() -> Self {
        Self {
            total_ticks: 0,
            rate: 1.0,
            at_game_time: 0,
            synced: false,
        }
    }
}

/// Per-connection chunk-batch flow-control state: the running rate estimator and
/// the start instant of the batch currently in flight. Guarded by a [`Mutex`]
/// only to satisfy `Sync`; a connection drives it sequentially.
#[derive(Debug)]
struct ChunkBatchState {
    calculator: ChunkBatchSizeCalculator,
    /// `None` on `wasm32`, which has no monotonic clock. Without a timing sample
    /// the calculator keeps its default desired rate, which is a throughput
    /// *hint* to the server rather than a correctness input — so declining to
    /// measure costs an adaptive batch size and nothing else.
    batch_start: Option<Instant>,
}

/// A monotonic reading where one exists, and `None` on `wasm32`.
///
/// `std::time::Instant::now()` **compiles** for `wasm32-unknown-unknown` and
/// then **panics at runtime** — "time not implemented on this platform". This
/// one call sat in `V770Adapter::new()`, so a browser session died at adapter
/// construction, before the first byte reached the wire, and the browser profile
/// is `panic = "abort"`: no unwind, no error path, just a dead session with
/// nothing in the log to name it.
///
/// Third instance of this defect in one day — the others were three join timers
/// in `lodestone-server`'s `server.rs` and `hold_read`/`hold_write` in
/// `lodestone-ecs` (`02d77f85`), the latter on the driver's ingest path, so the
/// join died on the first event after `Login`. **`tokio::time::Instant` is not
/// the fix**: rustc's own `help:` offers it, and it bottoms out in
/// `std::time::Instant::now()` and panics identically.
fn batch_now() -> Option<Instant> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Some(Instant::now())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// Per-connection movement-send tracking, mirroring the fields vanilla's
/// `LocalPlayer` uses to decide which `move_player_*` packet (if any) a tick
/// sends: `xLast`/`yLast`/`zLast`/`yRotLast`/`xRotLast`,
/// `lastOnGround`/`lastHorizontalCollision`, and `positionReminder`.
///
/// These track the last **sent** pose, not the last pose the client held —
/// position and rotation only advance when that axis actually sends (the
/// same hysteresis vanilla uses to avoid re-sending on float jitter), while
/// on-ground/horizontal-collision advance every tick regardless of whether
/// anything was sent. Zero-initialized to match Java's field defaults, so a
/// fresh connection's first `Move` reads as maximally "dirty" against the
/// all-zero baseline — exactly like a freshly constructed `LocalPlayer`.
#[derive(Debug, Clone, Copy)]
struct MovementSendState {
    last_pos: Vec3,
    last_yaw: f32,
    last_pitch: f32,
    last_on_ground: bool,
    last_horizontal_collision: bool,
    /// Ticks since the last full position update; forces one every 20 ticks
    /// even with zero movement, matching `positionReminder >= 20`.
    position_reminder: u32,
}

impl Default for MovementSendState {
    fn default() -> Self {
        Self {
            last_pos: Vec3::new(0.0, 0.0, 0.0),
            last_yaw: 0.0,
            last_pitch: 0.0,
            last_on_ground: false,
            last_horizontal_collision: false,
            position_reminder: 0,
        }
    }
}
impl V770Adapter {
    /// Creates a new adapter with the overworld chunk shape as its default.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shape: Arc::new(Mutex::new(ChunkShape::overworld_1_21())),
            batch: Arc::new(Mutex::new(ChunkBatchState {
                calculator: ChunkBatchSizeCalculator::new(),
                batch_start: batch_now(),
            })),
            movement: Arc::new(Mutex::new(MovementSendState::default())),
            variants: Arc::new(Mutex::new(HashMap::new())),
            clock: Arc::new(Mutex::new(DayClock::default())),
            registries: Arc::new(Mutex::new(ClientRegistries::default())),
            clock_holder: Arc::new(Mutex::new(None)),
            chat_cache: Arc::new(Mutex::new(MessageSignatureCache::vanilla())),
        }
    }

    /// Records the start of a chunk batch so its duration can be measured when
    /// the matching `chunk_batch_finished` arrives.
    fn begin_chunk_batch(&self) {
        if let Ok(mut state) = self.batch.lock() {
            state.batch_start = batch_now();
        }
    }

    /// Folds the finished batch into the rate estimator and returns the desired
    /// chunks-per-tick rate to acknowledge with.
    fn finish_chunk_batch(&self, batch_size: i32) -> f32 {
        match self.batch.lock() {
            Ok(mut state) => {
                if let Some(start) = state.batch_start {
                    let duration_nanos = start.elapsed().as_nanos() as f64;
                    state
                        .calculator
                        .on_batch_finished(batch_size, duration_nanos);
                }
                state.calculator.desired_chunks_per_tick()
            }
            Err(_) => ChunkBatchSizeCalculator::new().desired_chunks_per_tick(),
        }
    }

    /// Records the chunk shape for `dimension` so subsequent chunk packets in
    /// that dimension decode against the correct build-height window.
    ///
    /// The name-matched fallback, used only when `registry_data` did not resolve
    /// the dimension type — see [`Self::enter_dimension`].
    fn set_dimension(&self, dimension: &str) {
        if let Ok(mut shape) = self.shape.lock() {
            *shape = ChunkShape::for_dimension(dimension);
        }
    }

    /// Folds one `registry_data` packet into this connection's registry store.
    fn apply_registry_data(&self, data: RegistryData) {
        if let Ok(mut registries) = self.registries.lock() {
            registries.apply(data);
        }
    }

    /// Resolves a `login`/`respawn` dimension-type holder id against the
    /// registries received during Configuration, and installs everything that
    /// depends on it: the chunk [`ChunkShape`] and the day-clock holder.
    ///
    /// Returns the [`DimensionTypeInfo`] to publish, or `None` when the id did
    /// not resolve — in which case the shape falls back to
    /// [`ChunkShape::for_dimension`]'s level-name match, exactly the pre-#288
    /// behaviour. That fallback is *not* dead code: a protocol family or server
    /// that sends no `registry_data` still has to join, and the client must not
    /// disconnect over a registry it merely wanted.
    ///
    /// # Why the id and not the level name
    ///
    /// `login`/`respawn` carry both a level [`DimensionId`](lodestone_model::DimensionId)
    /// (`minecraft:the_nether`) and a bare `dimension_type` **holder id**. Only
    /// the latter is authoritative: a data pack can point a level called
    /// `mypack:mine` at the vanilla overworld type, or give
    /// `minecraft:overworld` a 1024-tall custom type, and a name match gets both
    /// wrong. `ChunkShape::for_dimension`'s own doc comment already admitted
    /// this; it is the height half of the same bug #34 filed for sky light.
    fn enter_dimension(&self, holder_id: i32, level_name: &str) -> Option<DimensionTypeInfo> {
        let resolved = self
            .registries
            .lock()
            .ok()
            .and_then(|registries| {
                let (name, dimension_type) = registries.dimension_type(holder_id)?;
                // The clock holder must be resolved *while the lock is held*,
                // because `default_clock` names an entry in a second registry
                // living in the same store.
                let clock_holder = dimension_type
                    .default_clock
                    .as_deref()
                    .and_then(|clock| registries.world_clock_id(clock));
                Some((name.to_owned(), dimension_type.clone(), clock_holder))
            });

        let Some((name, dimension_type, clock_holder)) = resolved else {
            // Unresolved: keep the name match, and forget any clock holder the
            // previous dimension resolved — a stale holder is worse than none,
            // since `day_clock`'s fallback at least tracks a real clock.
            self.set_dimension(level_name);
            if let Ok(mut holder) = self.clock_holder.lock() {
                *holder = None;
            }
            return None;
        };

        if let Ok(mut shape) = self.shape.lock() {
            // Only the vertical window comes from the registry; the palette
            // framing and the air/biome ids are properties of the *protocol
            // family*, not of the dimension, so they keep the family's values.
            *shape = ChunkShape {
                min_y: dimension_type.min_y,
                section_count: dimension_type.section_count(),
                world_height: u32::try_from(dimension_type.height.max(0)).unwrap_or(0),
                ..ChunkShape::overworld_1_21()
            };
        }
        if let Ok(mut holder) = self.clock_holder.lock() {
            *holder = clock_holder;
        }
        dimension_type_info(&name, &dimension_type)
    }

    /// The day-clock holder id the current dimension follows, if resolved.
    fn current_clock_holder(&self) -> Option<i32> {
        self.clock_holder.lock().ok().and_then(|holder| *holder)
    }

    /// Returns the current dimension's chunk shape.
    fn current_shape(&self) -> ChunkShape {
        self.shape
            .lock()
            .map(|shape| shape.clone())
            .unwrap_or_else(|_| ChunkShape::overworld_1_21())
    }

    /// Chooses which `move_player_*` packet, if any, this tick's movement
    /// produces, and updates the send-tracking state accordingly.
    ///
    /// Mirrors vanilla's `LocalPlayer.sendPosition()` exactly (see
    /// `ServerboundMovePlayerPacket` for the wire shapes it selects between):
    /// position is "dirty" when the squared distance from the last **sent**
    /// position exceeds `(2e-4)²`, or every 20 ticks regardless of movement
    /// (the periodic forced update); rotation is dirty on *any* nonzero yaw
    /// or pitch delta from the last sent rotation. Both-dirty sends
    /// `PosRot`; position-only sends `Pos`; rotation-only sends `Rot`;
    /// neither, but on-ground or horizontal-collision changed since last
    /// tick, sends `StatusOnly`; otherwise nothing is sent this tick — a
    /// deliberate, vanilla-faithful `None`, not a bug.
    ///
    /// `on_ground` and `horizontal_collision` are simulation outputs
    /// supplied by the caller (see [`ClientAction::Move`]); this method only
    /// ever compares and forwards them, never derives them.
    fn select_move_packet(
        &self,
        pos: Vec3,
        rotation: Rotation,
        on_ground: bool,
        horizontal_collision: bool,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        let mut state = self
            .movement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let delta_x = pos.x - state.last_pos.x;
        let delta_y = pos.y - state.last_pos.y;
        let delta_z = pos.z - state.last_pos.z;
        let delta_yaw = f64::from(rotation.yaw) - f64::from(state.last_yaw);
        let delta_pitch = f64::from(rotation.pitch) - f64::from(state.last_pitch);

        state.position_reminder += 1;
        let moved = delta_x * delta_x + delta_y * delta_y + delta_z * delta_z > 4.0e-8
            || state.position_reminder >= 20;
        let rotated = delta_yaw != 0.0 || delta_pitch != 0.0;

        let flags = (if on_ground { MOVE_FLAG_ON_GROUND } else { 0 })
            | (if horizontal_collision {
                MOVE_FLAG_HORIZONTAL_COLLISION
            } else {
                0
            });

        let packet = if moved && rotated {
            let body = MovePlayerPosRot {
                x: pos.x,
                y: pos.y,
                z: pos.z,
                yaw: rotation.yaw,
                pitch: rotation.pitch,
                flags,
            };
            Some((play::serverbound::MOVE_PLAYER_POS_ROT, encode_body(&body)?))
        } else if moved {
            let body = MovePlayerPos {
                x: pos.x,
                y: pos.y,
                z: pos.z,
                flags,
            };
            Some((play::serverbound::MOVE_PLAYER_POS, encode_body(&body)?))
        } else if rotated {
            let body = MovePlayerRot {
                yaw: rotation.yaw,
                pitch: rotation.pitch,
                flags,
            };
            Some((play::serverbound::MOVE_PLAYER_ROT, encode_body(&body)?))
        } else if state.last_on_ground != on_ground
            || state.last_horizontal_collision != horizontal_collision
        {
            let body = MovePlayerStatusOnly { flags };
            Some((
                play::serverbound::MOVE_PLAYER_STATUS_ONLY,
                encode_body(&body)?,
            ))
        } else {
            None
        };

        if moved {
            state.last_pos = pos;
            state.position_reminder = 0;
        }
        if rotated {
            state.last_yaw = rotation.yaw;
            state.last_pitch = rotation.pitch;
        }
        state.last_on_ground = on_ground;
        state.last_horizontal_collision = horizontal_collision;

        Ok(packet)
    }
}
impl Default for V770Adapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns a protocol 776 version adapter.
///
/// This free function is the crate's canonical constructor entry point; the
/// client boxes the returned concrete type as a `dyn VersionAdapter`.
#[must_use]
pub fn adapter() -> V770Adapter {
    V770Adapter::new()
}

/// Encodes a packet body into a fresh byte buffer.
///
/// Thin wrapper over the version-free [`lodestone_core::encode_body`], which
/// returns a stringified error because `AdapterError` lives in
/// `lodestone-model` and `lodestone-core` cannot depend on it.
fn encode_body<T: Encode>(packet: &T) -> Result<Vec<u8>, AdapterError> {
    lodestone_core::encode_body(packet, CTX).map_err(AdapterError::Encode)
}

/// Decodes a packet body from raw bytes.
fn decode_body<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    lodestone_core::decode_body(payload, CTX).map_err(AdapterError::Decode)
}

/// Decodes a packet body and asserts the payload was consumed to the last byte.
///
/// The trailing zero-length check is the cheapest misparse detector available:
/// a wrong field width or a skipped conditional almost always leaves the reader
/// misaligned, which surfaces here as a decode error instead of silently
/// corrupting downstream state.
fn decode_full<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX).map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(value)
}

/// Unpacks a vanilla `BlockPos.asLong` value into canonical block coordinates.
///
/// The packing places `x` in the high 26 bits, `z` in the middle 26 bits, and
/// `y` in the low 12 bits, each stored as a two's-complement signed field.
fn unpack_block_pos(packed: i64) -> BlockPos {
    let x = (packed >> 38) as i32;
    let y = ((packed << 52) >> 52) as i32;
    let z = ((packed << 26) >> 38) as i32;
    BlockPos { x, y, z }
}

/// Unpacks a vanilla `SectionPos.asLong` value into section-grid coordinates.
///
/// The packing places `x` in bits 42–63 (22 bits), `z` in bits 20–41 (22 bits),
/// and `y` in bits 0–19 (20 bits), each a two's-complement signed field.
fn unpack_section_pos(packed: i64) -> (i32, i32, i32) {
    let x = (packed >> 42) as i32;
    let y = ((packed << 44) >> 44) as i32;
    let z = ((packed << 22) >> 42) as i32;
    (x, y, z)
}

/// Packs block coordinates into a vanilla `BlockPos.asLong` value: `x` in bits
/// 38–63, `z` in bits 12–37, `y` in bits 0–11, each a signed field.
fn pack_block_pos(pos: BlockPos) -> i64 {
    ((i64::from(pos.x) & 0x3FF_FFFF) << 38)
        | ((i64::from(pos.z) & 0x3FF_FFFF) << 12)
        | (i64::from(pos.y) & 0xFFF)
}

/// Maps an interaction hand to its vanilla ordinal (`0` main, `1` off).
fn hand_ordinal(hand: Hand) -> i32 {
    match hand {
        Hand::Main => 0,
        Hand::Off => 1,
    }
}

/// Maps a block face to `Direction.get3DDataValue` (`0` down … `5` east).
fn face_ordinal(face: BlockFace) -> i32 {
    match face {
        BlockFace::Down => 0,
        BlockFace::Up => 1,
        BlockFace::North => 2,
        BlockFace::South => 3,
        BlockFace::West => 4,
        BlockFace::East => 5,
    }
}

/// Writes a `Vec3` using vanilla's `LpVec3` low-precision quantised codec: a
/// single `0` byte for the (near-)zero vector, otherwise a packed 48-bit buffer
/// (two bytes plus a big-endian int) carrying three 15-bit components and a
/// 2-bit scale, with an optional trailing scale varint when the scale overflows.
fn write_lp_vec3(w: &mut Writer, x: f64, y: f64, z: f64) {
    fn sanitize(v: f64) -> f64 {
        if v.is_nan() {
            0.0
        } else {
            v.clamp(-1.717_986_918_3E10, 1.717_986_918_3E10)
        }
    }
    // Vanilla `Math.round`, i.e. floor(a + 0.5); the argument is always >= 0.
    fn pack(v: f64) -> i64 {
        ((v * 0.5 + 0.5) * 32766.0 + 0.5).floor() as i64
    }
    let x = sanitize(x);
    let y = sanitize(y);
    let z = sanitize(z);
    let chess = x.abs().max(y.abs()).max(z.abs());
    if chess < 3.051_944_088_384_301E-5 {
        w.u8(0);
        return;
    }
    let scale = chess.ceil() as i64;
    let is_partial = (scale & 3) != scale;
    let markers = if is_partial { (scale & 3) | 4 } else { scale };
    let buffer = markers
        | (pack(x / scale as f64) << 3)
        | (pack(y / scale as f64) << 18)
        | (pack(z / scale as f64) << 33);
    w.u8(buffer as u8);
    w.u8((buffer >> 8) as u8);
    w.i32((buffer >> 16) as i32);
    if is_partial {
        w.var_i32((scale >> 2) as i32);
    }
}

/// Encodes a serverbound `interact` payload: VarInt entity id, VarInt hand,
/// `LpVec3` location, then the secondary-action bool. `location` is `None` for a
/// plain interact, which vanilla encodes as the zero vector (a single `0` byte).
fn encode_interact(entity_id: i32, hand: Hand, location: Option<Vec3>, sneaking: bool) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(entity_id);
    w.var_i32(hand_ordinal(hand));
    let loc = location.unwrap_or(Vec3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    });
    write_lp_vec3(&mut w, loc.x, loc.y, loc.z);
    w.bool(sneaking);
    w.into_vec()
}

/// Maps a container click mode to `ContainerInput`'s ordinal
/// (`ByteBufCodecs.idMapper`, a direct VarInt id: `0` pickup … `6` pickup_all).
fn container_input_ordinal(click_type: ContainerClickType) -> i32 {
    match click_type {
        ContainerClickType::Pickup => 0,
        ContainerClickType::QuickMove => 1,
        ContainerClickType::Swap => 2,
        ContainerClickType::Clone => 3,
        ContainerClickType::Throw => 4,
        ContainerClickType::QuickCraft => 5,
        ContainerClickType::PickupAll => 6,
    }
}

/// Encodes the serverbound `container_click` packet body.
///
/// Wire layout (`ServerboundContainerClickPacket`): VarInt container id, VarInt
/// state id, big-endian `short` slot, big-endian `byte` button,
/// `ContainerInput` ordinal (VarInt), a changed-slots map (VarInt entry count,
/// then per entry a big-endian `short` slot key and a `HashedStack` value),
/// then the carried cursor stack, also a `HashedStack`. Map iteration order is
/// not semantically significant (vanilla holds it in a hash map), so the
/// model's `Vec` order is used as-is.
fn encode_container_click(
    window_id: i32,
    state_id: i32,
    slot: i32,
    button: i32,
    click_type: ContainerClickType,
    changed_slots: &[ContainerSlotChange],
    carried_item: Option<&ItemStack>,
) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.var_i32(window_id);
    w.var_i32(state_id);
    let slot_i16 = i16::try_from(slot)
        .map_err(|_| AdapterError::Encode(format!("container click slot {slot} overflows i16")))?;
    w.i16(slot_i16);
    let button_i8 = i8::try_from(button).map_err(|_| {
        AdapterError::Encode(format!("container click button {button} overflows i8"))
    })?;
    w.i8(button_i8);
    w.var_i32(container_input_ordinal(click_type));
    let count = i32::try_from(changed_slots.len()).map_err(|_| {
        AdapterError::Encode("too many changed slots in container click".to_owned())
    })?;
    w.var_i32(count);
    for change in changed_slots {
        let change_slot = i16::try_from(change.slot).map_err(|_| {
            AdapterError::Encode(format!("changed slot {} overflows i16", change.slot))
        })?;
        w.i16(change_slot);
        write_hashed_stack(&mut w, change.item.as_ref())?;
    }
    write_hashed_stack(&mut w, carried_item)?;
    Ok(w.into_vec())
}

/// Maps a vanilla game-mode ordinal to the canonical [`GameMode`], if valid.
///
/// `pub(crate)` because `server_protocol` decodes the *serverbound*
/// `change_game_mode` with it — the same id table, the other direction.
pub(crate) fn game_mode_from_ordinal(ordinal: i32) -> Option<GameMode> {
    match ordinal {
        0 => Some(GameMode::Survival),
        1 => Some(GameMode::Creative),
        2 => Some(GameMode::Adventure),
        3 => Some(GameMode::Spectator),
        _ => None,
    }
}

/// Maps the canonical [`GameMode`] to vanilla's `GameType` id, the inverse of
/// [`game_mode_from_ordinal`].
pub(crate) fn game_mode_to_ordinal(mode: GameMode) -> i32 {
    match mode {
        GameMode::Survival => 0,
        GameMode::Creative => 1,
        GameMode::Adventure => 2,
        GameMode::Spectator => 3,
    }
}

/// Maps the canonical [`RecipeBookType`] to vanilla's `RecipeBookType` ordinal,
/// as written by `FriendlyByteBuf.writeEnum` in
/// `ServerboundRecipeBookChangeSettingsPacket`.
fn recipe_book_type_to_ordinal(book_type: RecipeBookType) -> i32 {
    match book_type {
        RecipeBookType::Crafting => 0,
        RecipeBookType::Furnace => 1,
        RecipeBookType::BlastFurnace => 2,
        RecipeBookType::Smoker => 3,
    }
}

/// The fixed-point scale for `sound` packet positions: coordinates are sent as
/// `(int)(block * 8)`, so each unit is `1/8` of a block (`LOCATION_ACCURACY`).
const SOUND_POSITION_SCALE: f64 = 8.0;

/// Decodes a `Holder<SoundEvent>`, returning the sound's identifier and its
/// optional fixed audible range.
///
/// The holder is a VarInt: `0` introduces an inline definition (an identifier
/// then an optional `f32` range), and any positive value references the
/// `minecraft:sound_event` registry at index `value - 1`, whose range is a
/// property of the registry entry rather than the wire.
fn read_sound_holder(reader: &mut Reader<'_>) -> Result<(String, Option<f32>), AdapterError> {
    let holder_id = reader.var_i32().map_err(dec_err)?;
    if holder_id == 0 {
        let name = reader.string(32767).map_err(dec_err)?;
        let range = if reader.bool().map_err(dec_err)? {
            Some(reader.f32().map_err(dec_err)?)
        } else {
            None
        };
        Ok((name, range))
    } else {
        let index = holder_id - 1;
        sound_event(index)
            .map(|(name, range)| (name.to_owned(), range))
            .ok_or_else(|| AdapterError::Decode(format!("unknown sound event id {index}")))
    }
}

/// Reads a `SoundSource` enum ordinal (a VarInt) as the canonical
/// [`SoundCategory`].
fn read_sound_category(reader: &mut Reader<'_>) -> Result<SoundCategory, AdapterError> {
    let ordinal = reader.var_i32().map_err(dec_err)?;
    u8::try_from(ordinal)
        .ok()
        .and_then(SoundCategory::from_ordinal)
        .ok_or_else(|| AdapterError::Decode(format!("invalid sound source ordinal {ordinal}")))
}

/// Reads an `EntityAnchorArgument.Anchor` ordinal (a VarInt): `0` = feet,
/// `1` = eyes. Used by `ClientboundPlayerLookAtPacket`.
fn read_look_anchor(reader: &mut Reader<'_>) -> Result<LookAnchor, AdapterError> {
    match reader.var_i32().map_err(dec_err)? {
        0 => Ok(LookAnchor::Feet),
        1 => Ok(LookAnchor::Eyes),
        other => Err(AdapterError::Decode(format!(
            "invalid entity anchor ordinal {other}"
        ))),
    }
}

/// Parses a `minecraft:*` identifier into a canonical [`ResourceKey`],
/// attributing a decode error to `what` on failure.
fn parse_key(name: &str, what: &str) -> Result<ResourceKey, AdapterError> {
    name.parse()
        .map_err(|_| AdapterError::Decode(format!("invalid {what} key {name}")))
}

/// Outcome of decoding one clientbound item stack.
///
/// # Why this is an enum and not a `{ stack, complete }` struct
///
/// It used to be a struct with a `complete: bool`, and a caller
/// (`decode_merchant_offers`) wrote `read_item_stack(reader)?.stack` — dropping
/// the flag and reading the *next* offer out of a reader parked mid-payload.
/// Every field after that decoded as a plausible-but-wrong value. A `bool`
/// beside the thing you actually want is an affordance to ignore it; an enum
/// has none, because there is no way to reach the stack without naming which
/// case you are in. **Do not reintroduce an accessor that returns the stack
/// without the verdict** (no `fn stack(self) -> Option<ItemStack>`), or the
/// affordance comes straight back.
///
/// The patch codec length-prefixes neither the patch nor its individual
/// components (26.2 `DataComponentPatch.STREAM_CODEC`, the undelimited variant
/// clientbound stacks use), so an unrecognised component cannot be skipped in
/// place — hence a partial outcome at all. See [`read_item_stack`].
#[must_use]
pub(crate) enum DecodedStack {
    /// The stack was consumed exactly; the reader sits immediately after it and
    /// reading on is safe. Inner `None` is the empty stack.
    Complete(Option<ItemStack>),
    /// An unmodeled component halted decoding partway through the stack's
    /// `DataComponentPatch`. The modeled fields that were decoded are valid, but
    /// **the rest of this packet is gone**: emit what is here and stop.
    ///
    /// The reader has been drained to its end by [`read_component_patch`], so a
    /// caller that ignores this and reads on gets a clean `UnexpectedEof` — a
    /// dropped packet, which the client driver survives — instead of silently
    /// consuming payload bytes as ids and lengths.
    Partial(Option<ItemStack>),
}

/// Decodes a clientbound optional item stack.
///
/// Wire shape (26.2 `ItemStack.OPTIONAL_STREAM_CODEC`): a VarInt count — `<= 0`
/// means the empty stack — then the item registry id as a VarInt, then a
/// `DataComponentPatch` (a VarInt count of added components and a VarInt count of
/// removed components; both zero means an empty patch). Each added component is a
/// `(type id VarInt, payload)` pair and each removed component a bare type id.
///
/// Added component payloads are **not** length-prefixed in the clientbound
/// (trusted) codec, so a component this build does not model cannot be skipped.
/// Rather than tear down the session on the next unrecognised component — every
/// future component addition would then be an outage — decoding degrades: the
/// modeled components (custom name, damage, enchantments) are decoded, and the
/// first unmodeled component stops the patch, flags the stack as partial
/// ([`ItemComponents::has_unmodeled`]), and yields it with `complete == false`.
///
/// `pub(crate)` because entity metadata carries the *same* codec under its
/// `ITEM_STACK` serializer (a dropped item's whole identity is one such field).
/// That path must reuse this decoder rather than grow a second one — two
/// independent readings of the component-patch wire is exactly how the two ends
/// drift apart.
pub(crate) fn read_item_stack(reader: &mut Reader<'_>) -> Result<DecodedStack, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    if count <= 0 {
        return Ok(DecodedStack::Complete(None));
    }
    let item_id = reader.var_i32().map_err(dec_err)?;
    let name = item_name(item_id)
        .ok_or_else(|| AdapterError::Decode(format!("unknown item registry id {item_id}")))?;
    let count = u32::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid item count {count}")))?;
    let (components, complete) = read_component_patch(reader, name)?;
    let stack = Some(ItemStack {
        item: parse_key(name, "item")?,
        count,
        components,
    });
    Ok(if complete {
        DecodedStack::Complete(stack)
    } else {
        DecodedStack::Partial(stack)
    })
}

/// `minecraft:trim_material` registry paths in **vanilla bootstrap order**
/// (`TrimMaterials.bootstrap`, `TrimMaterials.java:25-35`), which is the order a
/// vanilla server's Configuration-phase registry sync assigns ids in.
///
/// # Why this table and not a synced registry
///
/// `Registries.TRIM_MATERIAL` is a **dynamic** registry: its ids come from the
/// `registry_data` packets sent during Configuration, and this client keeps no
/// dynamic-registry store, so a `Holder::REFERENCE` id has nothing to resolve
/// against. Bootstrap order is what a vanilla server without a trim datapack
/// sends, so this is exact for vanilla and **provisional** for a modded server —
/// the same posture, and the same caveat, as `server_protocol.rs`'s `BIOME_NAMES`.
/// An id outside the table decodes as the empty string rather than failing: the
/// bytes are consumed either way, which is the property that keeps the rest of the
/// packet readable.
///
/// Deliberately *not* read from `lodestone_assets::trim::TRIM_MATERIALS`, which
/// happens to be in this same order today — `TRIM_PATTERNS` beside it is
/// alphabetical, so "the asset table is in registry order" is a coincidence for
/// one of the two and cannot be relied on for either.
const TRIM_MATERIAL_IDS: &[&str] = &[
    "quartz",
    "iron",
    "netherite",
    "redstone",
    "copper",
    "gold",
    "emerald",
    "diamond",
    "lapis",
    "amethyst",
    "resin",
];

/// `minecraft:trim_pattern` registry paths in vanilla bootstrap order
/// (`TrimPatterns.bootstrap`, `TrimPatterns.java:31-48`). See
/// [`TRIM_MATERIAL_IDS`] for the id-space caveat — and note this is **not** the
/// alphabetical order `lodestone_assets::trim::TRIM_PATTERNS` uses.
const TRIM_PATTERN_IDS: &[&str] = &[
    "sentry",
    "dune",
    "coast",
    "wild",
    "ward",
    "eye",
    "vex",
    "tide",
    "snout",
    "rib",
    "spire",
    "wayfinder",
    "shaper",
    "silence",
    "raiser",
    "host",
    "flow",
    "bolt",
];

/// Decodes `minecraft:trim`'s payload — `ArmorTrim.STREAM_CODEC`
/// (`ArmorTrim.java:26-28`), a `Holder<TrimMaterial>` then a
/// `Holder<TrimPattern>`.
///
/// Each holder is `ByteBufCodecs.holder(registry, DIRECT_STREAM_CODEC)`: a VarInt
/// where `0` introduces an **inline** definition and any positive value references
/// the registry at `value - 1`. Both forms are handled, because both must be — the
/// inline form is what a datapack-defined trim arrives as, and consuming the wrong
/// number of bytes for it would desync the rest of the packet exactly as the
/// unmodeled-component cliff this arm exists to remove does.
///
/// The inline bodies, from the two `DIRECT_STREAM_CODEC`s:
///
/// * `TrimMaterial` (`TrimMaterial.java:22-24`) — a `MaterialAssetGroup` (an
///   `AssetInfo` = one UTF-8 string, then a map of `ResourceKey -> AssetInfo`,
///   i.e. a VarInt count of `(string, string)` pairs) then a description
///   `Component` (network NBT).
/// * `TrimPattern` (`TrimPattern.java:25-33`) — an `Identifier` (string), a
///   description `Component`, then a `bool` `decal`.
///
/// **The inline material carries no registry name**, only its asset suffix, so
/// that is what is reported: for every vanilla material the suffix *is* the
/// registry path (`MaterialAssetGroup::create(base)`, `MaterialAssetGroup.java:36-46`),
/// and it is also the half `lodestone_assets::trim::trim_sprite_id` actually needs.
fn read_armor_trim(reader: &mut Reader<'_>) -> Result<ArmorTrim, AdapterError> {
    let material = match reader.var_i32().map_err(dec_err)? {
        0 => {
            let base = reader.string(32767).map_err(dec_err)?;
            let overrides = reader.var_i32().map_err(dec_err)?;
            for _ in 0..overrides {
                let _key = reader.string(32767).map_err(dec_err)?;
                let _suffix = reader.string(32767).map_err(dec_err)?;
            }
            let _description = read_network_nbt(reader).map_err(dec_err)?;
            base
        }
        holder => TRIM_MATERIAL_IDS
            .get((holder - 1) as usize)
            .copied()
            .unwrap_or_default()
            .to_owned(),
    };
    let pattern = match reader.var_i32().map_err(dec_err)? {
        0 => {
            let asset_id = reader.string(32767).map_err(dec_err)?;
            let _description = read_network_nbt(reader).map_err(dec_err)?;
            let _decal = reader.bool().map_err(dec_err)?;
            // The asset id is a full identifier; the registry path is what the
            // asset layer keys by.
            asset_id
                .rsplit_once(':')
                .map_or(asset_id.clone(), |(_, path)| path.to_owned())
        }
        holder => TRIM_PATTERN_IDS
            .get((holder - 1) as usize)
            .copied()
            .unwrap_or_default()
            .to_owned(),
    };
    Ok(ArmorTrim { material, pattern })
}

/// Decodes `minecraft:pot_decorations`' payload — `PotDecorations.STREAM_CODEC`,
/// which is `ByteBufCodecs.registry(Registries.ITEM).apply(ByteBufCodecs.list(4))`.
///
/// So the wire is a VarInt element count (`ByteBufCodecs.readCount`, capped at 4)
/// followed by that many **bare** item registry ids as VarInts. Two shapes it is
/// easy to get wrong, both re-read from the jar rather than inferred:
///
/// * `ByteBufCodecs.registry` is `idMapper`, which writes `VarInt.write(id)` with
///   **no `+1` and no `0` sentinel** — unlike `ByteBufCodecs.holder`, which
///   `minecraft:trim` uses two arms above. Adding an offset here would consume the
///   right number of bytes and report the wrong four sherds.
/// * The list is `list(4)`, a *maximum*, not a fixed width. A vanilla server
///   always writes four (`PotDecorations::ordered` builds a four-element list
///   unconditionally), but a shorter list is legal on the wire and its missing
///   tail is `Optional.empty()` — `PotDecorations::getItem`'s `i >= sherds.size()`
///   arm.
///
/// `minecraft:brick` decodes to `None`, mirroring `getItem`'s
/// `item == Items.BRICK ? Optional.empty() : Optional.of(item)`. An id outside the
/// item registry decodes as `None` rather than failing, for the same reason
/// [`TRIM_MATERIAL_IDS`] tolerates an unknown holder: the bytes are consumed
/// either way, and that is the property keeping the rest of the packet readable.
fn read_pot_decorations(reader: &mut Reader<'_>) -> Result<PotDecorations, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    if !(0..=4).contains(&count) {
        return Err(AdapterError::Decode(format!(
            "pot_decorations declares {count} sherds; ByteBufCodecs.list(4) permits 0..=4"
        )));
    }
    let mut sides: [Option<ResourceKey>; 4] = [None, None, None, None];
    for side in sides.iter_mut().take(count as usize) {
        let id = reader.var_i32().map_err(dec_err)?;
        // A brick face and an absent face are the same state in vanilla, so both
        // land on `None`.
        *side = match item_name(id) {
            Some("minecraft:brick") | None => None,
            Some(name) => Some(parse_key(name, "pot decoration")?),
        };
    }
    let [back, left, right, front] = sides;
    Ok(PotDecorations {
        back,
        left,
        right,
        front,
    })
}

/// Decodes an item stack's `DataComponentPatch` into the modeled component set,
/// returning whether the patch was fully consumed.
///
/// Modeled added components are read into their fields; the first unmodeled
/// added component stops decoding (its payload is not length-prefixed and so
/// cannot be skipped), flags the set, and returns `complete == false`. Removed
/// components are bare type ids and are always skippable, so a patch that
/// reaches them is fully consumed.
///
/// # The three *effective* fields start from the item's prototype
///
/// `max_stack_size`, `max_damage` and `equippable` are **not** patch fields —
/// they are the item's built-in prototype values, folded with whatever the patch
/// says. They are seeded here from [`lodestone_data::item_prototypes`] *before* the patch
/// is read, because a clientbound patch almost never mentions any of them
/// (vanilla keeps all three in the prototype component map) and a stack that
/// reported "unknown" for them would leave armour unequippable and every stack
/// cap at 64. See [`ItemComponents`] for why they are effective rather than
/// patch-shaped, and `docs/item-prototypes.md` for the census.
fn read_component_patch(
    reader: &mut Reader<'_>,
    item: &str,
) -> Result<(ItemComponents, bool), AdapterError> {
    let added = reader.var_i32().map_err(dec_err)?;
    let removed = reader.var_i32().map_err(dec_err)?;
    let mut components = ItemComponents::default();
    if let Some(prototype) = lodestone_data::item_prototypes::prototype(item) {
        components.max_stack_size = Some(u32::from(prototype.max_stack_size));
        components.max_damage = prototype.max_damage.map(u32::from);
        components.equippable = prototype.equip_slot;
    }

    for _ in 0..added {
        let type_id = reader.var_i32().map_err(dec_err)?;
        match component_type_name(type_id) {
            Some("minecraft:custom_name") => {
                let nbt = read_network_nbt(reader).map_err(dec_err)?;
                components.custom_name = Some(Text::from_nbt(&nbt));
            }
            Some("minecraft:damage") => {
                let damage = reader.var_i32().map_err(dec_err)?;
                components.damage = Some(u32::try_from(damage).map_err(|_| {
                    AdapterError::Decode(format!("negative item damage {damage}"))
                })?);
            }
            Some("minecraft:enchantments") => {
                components.enchantments = read_enchantments(reader)?;
            }
            Some("minecraft:tool") => {
                components.tool = ToolPatch::Set(read_tool(reader)?);
            }
            // `DyedItemColor.STREAM_CODEC` is a bare `ByteBufCodecs.INT`
            // (`DyedItemColor.java:24`) — fixed-width, not a `VarInt` like
            // every other scalar component here, so this is the one `i32()`
            // read in this match rather than `var_i32()`.
            Some("minecraft:dyed_color") => {
                components.dyed_color = Some(reader.i32().map_err(dec_err)? as u32);
            }
            // Decoded rather than left unmodeled *because* the `other` arm below
            // cannot skip: a trimmed armour stack used to truncate the whole
            // remaining packet, not merely lose its trim. See [`read_armor_trim`].
            Some("minecraft:trim") => {
                components.trim = Some(read_armor_trim(reader)?);
            }
            // `MapId.STREAM_CODEC` is `ByteBufCodecs.VAR_INT.map(MapId::new, …)`
            // (`MapId.java:19`), registered `networkSynchronized` at
            // `DataComponents.java:229`. Decoded for the same reason as the trim
            // above — a filled map in any inventory was truncating the packet from
            // here on, not merely losing which map it showed.
            Some("minecraft:map_id") => {
                components.map_id = Some(reader.var_i32().map_err(dec_err)?);
            }
            // Decoded for the same reason as the trim and the map id above, and
            // this one was found the hard way: the vanilla advancement
            // `adventure/craft_decorated_pot_using_only_sherds` has a
            // `minecraft:decorated_pot` icon, so a server that has sent any
            // advancement tree at all truncates `update_advancements` here — a
            // **join-blocking** failure, since that packet lands during the
            // initial world load. See [`read_pot_decorations`].
            Some("minecraft:pot_decorations") => {
                components.pot_decorations = Some(read_pot_decorations(reader)?);
            }
            // Both of these are `ByteBufCodecs.VAR_INT` (`DataComponents.java:110-115`)
            // and both *override* the prototype value seeded above. They are
            // decoded rather than treated as unmodeled not because servers send
            // them often — they essentially never do — but because a patch that
            // did carry one would otherwise stop decoding here and leave the
            // seeded prototype value silently stale.
            Some("minecraft:max_stack_size") => {
                let size = reader.var_i32().map_err(dec_err)?;
                components.max_stack_size = Some(u32::try_from(size).map_err(|_| {
                    AdapterError::Decode(format!("negative item max_stack_size {size}"))
                })?);
            }
            Some("minecraft:max_damage") => {
                let max = reader.var_i32().map_err(dec_err)?;
                components.max_damage = Some(u32::try_from(max).map_err(|_| {
                    AdapterError::Decode(format!("negative item max_damage {max}"))
                })?);
            }

            // ---------------------------------------------------------------
            // Consumed-for-alignment components.
            //
            // Everything from here to the `other` arm is decoded for exactly one
            // reason: an unmodeled component ends the packet. Nothing below is
            // *used* by this client (only `custom_data` is even kept), and that
            // is the point — the value is worthless and consuming the right
            // number of bytes is worth a whole packet. Each arm cites the vanilla
            // stream codec it mirrors; get a width wrong here and the failure is
            // silent misalignment rather than an honest bail-out, so no arm is
            // added without reading its codec in the jar.
            // ---------------------------------------------------------------

            // **The derived-NBT family.** These components are registered with
            // `persistent(codec)` and **no** `networkSynchronized(...)`, so
            // `DataComponentType.Builder.build` falls back to
            // `ByteBufCodecs.fromCodecWithRegistries(codec)` — which writes the
            // value as a single `FriendlyByteBuf.writeNbt` tag (root tag id then
            // payload, no name, no length prefix). One rule covers all seven, and
            // it is *not* the same codec as `CustomData.STREAM_CODEC`, which is
            // `@Deprecated` and used by `bucket_entity_data` rather than by
            // `custom_data`. Reading either as a bare compound would be wrong for
            // `recipes` (a list tag) and for the `Unit`-valued one (an empty
            // compound from `MapCodec.unitCodec`).
            //
            // `custom_data` is the one worth singling out: it is component id 0,
            // it is what every Bukkit/Paper plugin stamps on a GUI item, and while
            // it was unmodeled a lobby hotbar truncated whatever packet carried
            // it. Its bytes are kept verbatim rather than interpreted — see
            // [`ItemComponents::custom_data`].
            Some("minecraft:custom_data") => {
                components.custom_data = Some(read_network_nbt_bytes(reader)?);
            }
            Some(
                "minecraft:intangible_projectile"
                | "minecraft:map_decorations"
                | "minecraft:debug_stick_state"
                | "minecraft:recipes"
                | "minecraft:lock"
                | "minecraft:container_loot",
            ) => {
                read_network_nbt(reader).map_err(dec_err)?;
            }

            // `Unit.STREAM_CODEC` is `StreamCodec.unit(INSTANCE)`: **zero bytes**.
            // The component's presence in the patch is the whole value.
            Some(
                "minecraft:unbreakable" | "minecraft:creative_slot_lock" | "minecraft:glider",
            ) => {}

            // A bare VarInt. `rarity`, `dye`, `base_color` and `map_post_processing`
            // are `ByteBufCodecs.idMapper`, which is `VarInt.read` with no `+1` and
            // no `0` sentinel; the rest are `ByteBufCodecs.VAR_INT` directly, or a
            // one-field `StreamCodec.composite` over it (`enchantable`,
            // `ominous_bottle_amplifier`).
            Some(
                "minecraft:rarity"
                | "minecraft:repair_cost"
                | "minecraft:additional_trade_cost"
                | "minecraft:ominous_bottle_amplifier"
                | "minecraft:enchantable"
                | "minecraft:dye"
                | "minecraft:base_color"
                | "minecraft:map_post_processing",
            ) => {
                reader.var_i32().map_err(dec_err)?;
            }

            // Fixed-width scalars, **not** VarInts. `MapItemColor.STREAM_CODEC` is
            // `ByteBufCodecs.INT` (the same trap `minecraft:dyed_color` documents
            // above), and the two floats are `ByteBufCodecs.FLOAT`.
            Some("minecraft:map_color") => {
                reader.i32().map_err(dec_err)?;
            }
            Some("minecraft:minimum_attack_charge" | "minecraft:potion_duration_scale") => {
                reader.f32().map_err(dec_err)?;
            }
            Some("minecraft:enchantment_glint_override") => {
                reader.bool().map_err(dec_err)?;
            }

            // `Identifier.STREAM_CODEC` is `ByteBufCodecs.STRING_UTF8.map(...)`:
            // one length-prefixed string, capped at 32767.
            Some(
                "minecraft:item_model" | "minecraft:tooltip_style" | "minecraft:note_block_sound",
            ) => {
                reader.string(32767).map_err(dec_err)?;
            }

            // `ComponentSerialization.STREAM_CODEC` — the same network-NBT chat
            // component `minecraft:custom_name` uses. `item_name` is the *item's*
            // name rather than a rename, so it is consumed and not surfaced;
            // nothing here prefers it over `custom_name`.
            Some("minecraft:item_name") => {
                read_network_nbt(reader).map_err(dec_err)?;
            }

            // `ItemLore.STREAM_CODEC` is `ComponentSerialization.STREAM_CODEC
            // .apply(ByteBufCodecs.list(256))`: a VarInt count then that many
            // network-NBT components. 256 is the codec's own cap.
            Some("minecraft:lore") => {
                let lines = read_count(reader, "lore line")?;
                if lines > 256 {
                    return Err(AdapterError::Decode(format!(
                        "lore declares {lines} lines; ByteBufCodecs.list(256) permits at most 256"
                    )));
                }
                for _ in 0..lines {
                    read_network_nbt(reader).map_err(dec_err)?;
                }
            }

            // `stored_enchantments` shares `ItemEnchantments.STREAM_CODEC` with
            // `minecraft:enchantments`, so it reuses that reader — but it is an
            // enchanted *book*'s payload, not the stack's own effects, so it is
            // deliberately not merged into `components.enchantments`.
            Some("minecraft:stored_enchantments") => {
                read_enchantments(reader)?;
            }

            Some("minecraft:custom_model_data") => read_custom_model_data(reader)?,
            Some("minecraft:tooltip_display") => read_tooltip_display(reader)?,
            Some("minecraft:attribute_modifiers") => read_attribute_modifiers(reader)?,

            other => {
                // An unmodeled component: its payload is not length-prefixed, so
                // it and everything after it in this packet are unreadable. Keep
                // the modeled fields decoded so far, flag the stack, and stop —
                // the packet is dropped past this point, not fatal.
                //
                // **Skipping is genuinely impossible here, re-verified against the
                // jar rather than inherited from this comment.** 26.2 has two patch
                // codecs: `DataComponentPatch.STREAM_CODEC` writes each payload raw
                // and `DELIMITED_STREAM_CODEC` length-prefixes it
                // (`DataComponentPatch.java:62-76`). Clientbound stacks use
                // `ItemStack.OPTIONAL_STREAM_CODEC`, built on the **undelimited**
                // one; the delimited variant is `OPTIONAL_UNTRUSTED_STREAM_CODEC`,
                // i.e. serverbound only (`ItemStack.java:124-126`). So there is no
                // length to skip and no self-describing framing to walk. The only
                // way to stop a given component being a decode cliff is to model
                // it, which is what the `minecraft:trim` arm above does.
                //
                // One special case: if the component we cannot decode is
                // `minecraft:equippable` itself, the prototype slot seeded above
                // is *known* to be overridden, so report "unknown" rather than a
                // value we can see is wrong. (`Equippable`'s stream codec is an
                // eleven-field record with a `HolderSet<EntityType>`; decoding it
                // for the sake of a component no vanilla server patches is not
                // worth the surface.)
                if other == Some("minecraft:equippable") {
                    components.equippable = None;
                }
                components.has_unmodeled = true;
                tracing::warn!(
                    item,
                    component = other.unwrap_or("unknown"),
                    component_id = type_id,
                    "unmodeled item data component; delivering a partial stack and \
                     skipping the rest of the packet",
                );
                // Park the reader at the end of the payload. Every caller is
                // *supposed* to stop on the `false` below, but one did not, and
                // the bytes it then read as item ids and list lengths were the
                // interior of this component's payload — plausible-but-wrong
                // values, or an over-read blamed on framing. Draining makes the
                // contract self-enforcing: the worst a caller that reads on can
                // now do is raise `UnexpectedEof`, i.e. drop the packet, which
                // is the outcome the design already promises. It also makes a
                // trailing-bytes assertion pass instead of firing spuriously.
                let _ = reader.bytes(reader.remaining());
                return Ok((components, false));
            }
        }
    }

    for _ in 0..removed {
        // Removed components carry only their type id (no payload) and clear a
        // component back to *nothing* — not to the item's prototype value. That
        // distinction only matters for a component whose prototype value we
        // actually use, which today is `minecraft:tool`: `/give …[!minecraft:tool]`
        // makes a pickaxe mine like a fist, and treating the removal as "no
        // opinion" would leave it at 8x. Every other modeled field defaults to
        // "absent" anyway, so consuming the id is enough for those.
        let type_id = reader.var_i32().map_err(dec_err)?;
        match component_type_name(type_id) {
            Some("minecraft:tool") => components.tool = ToolPatch::Removed,
            // A removal clears the component back to *nothing*, and vanilla's
            // own fallback with no `minecraft:max_stack_size` at all is **1**,
            // not 64 (`ItemInstance.java:14-16`) — so this is a real, if exotic,
            // way to make an item unstackable.
            Some("minecraft:max_stack_size") => components.max_stack_size = Some(1),
            // No `minecraft:max_damage` means not damageable, which is exactly
            // what `None` means here.
            Some("minecraft:max_damage") => components.max_damage = None,
            Some("minecraft:equippable") => components.equippable = None,
            _ => {}
        }
    }

    Ok((components, true))
}

/// Reads one network-NBT tag and returns the exact bytes it occupied.
///
/// Used for `minecraft:custom_data`, whose value this client deliberately does
/// not interpret: the bytes are re-emittable and float-free as far as `Eq` is
/// concerned, where a parsed `Nbt` would not be. The span is derived from the
/// reader's own cursor movement rather than re-serialised, so it is byte-exact
/// even for shapes our writer would normalise.
fn read_network_nbt_bytes(reader: &mut Reader<'_>) -> Result<Vec<u8>, AdapterError> {
    let before = reader.remaining_bytes();
    read_network_nbt(reader).map_err(dec_err)?;
    let consumed = before.len() - reader.remaining();
    Ok(before[..consumed].to_vec())
}

/// Consumes a `minecraft:custom_model_data` payload
/// (`CustomModelData.STREAM_CODEC`).
///
/// Four independent VarInt-counted lists, in order: floats, flags (bools),
/// strings, colours. **The colours are `ByteBufCodecs.INT`** — fixed-width
/// big-endian, not VarInts — which is the one width in this component that a
/// VarInt-by-default reader gets wrong, and getting it wrong misaligns the whole
/// rest of the packet instead of merely losing a colour.
fn read_custom_model_data(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    let floats = read_count(reader, "custom_model_data float")?;
    for _ in 0..floats {
        reader.f32().map_err(dec_err)?;
    }
    let flags = read_count(reader, "custom_model_data flag")?;
    for _ in 0..flags {
        reader.bool().map_err(dec_err)?;
    }
    let strings = read_count(reader, "custom_model_data string")?;
    for _ in 0..strings {
        reader.string(32767).map_err(dec_err)?;
    }
    let colors = read_count(reader, "custom_model_data color")?;
    for _ in 0..colors {
        reader.i32().map_err(dec_err)?;
    }
    Ok(())
}

/// Consumes a `minecraft:tooltip_display` payload (`TooltipDisplay.STREAM_CODEC`).
///
/// A bool `hideTooltip`, then a VarInt-counted collection of
/// `DataComponentType.STREAM_CODEC` — which is `ByteBufCodecs.registry`, i.e. a
/// bare data-component-type registry id per entry with no offset.
///
/// This component replaced 1.21.4's `minecraft:hide_tooltip` and
/// `hide_additional_tooltip`, and it is what a plugin sets to hide an item's
/// attribute lines — so it turns up on essentially every custom GUI item.
fn read_tooltip_display(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    reader.bool().map_err(dec_err)?;
    let hidden = read_count(reader, "tooltip_display hidden component")?;
    for _ in 0..hidden {
        reader.var_i32().map_err(dec_err)?;
    }
    Ok(())
}

/// Consumes a `minecraft:attribute_modifiers` payload
/// (`ItemAttributeModifiers.STREAM_CODEC`).
///
/// A VarInt-counted list of `Entry`, each of which is, in wire order:
///
/// * the attribute as `Attribute.STREAM_CODEC` = `ByteBufCodecs.holderRegistry`,
///   a **bare** VarInt registry id — `holderRegistry` is `registry(…,
///   Registry::asHolderIdMap)`, so unlike `ByteBufCodecs.holder` there is no `+1`
///   and no inline-holder `0` sentinel;
/// * the modifier as `AttributeModifier.STREAM_CODEC` — an `Identifier` string, a
///   **`ByteBufCodecs.DOUBLE`** (fixed-width f64, not a float), then the operation
///   as an idMapper VarInt;
/// * the slot group as `EquipmentSlotGroup.STREAM_CODEC`, an idMapper VarInt;
/// * the display as `Display.STREAM_CODEC`, a VarInt `Display.Type` id dispatching
///   to a payload: `default` (0) and `hidden` (1) are `StreamCodec.unit`, i.e.
///   **zero bytes**, and `override` (2) carries one network-NBT chat component.
///
/// The `display` field is the trap: it is new enough that a transcription from an
/// older `ItemAttributeModifiers` (which ended after the slot group, with a
/// trailing `showInTooltip` bool in 1.21.4 and earlier) reads one byte where two
/// of the three variants read one and the third reads a whole NBT blob.
fn read_attribute_modifiers(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    let entries = read_count(reader, "attribute modifier")?;
    for _ in 0..entries {
        reader.var_i32().map_err(dec_err)?; // Holder<Attribute>, bare id
        reader.string(32767).map_err(dec_err)?; // AttributeModifier::id
        reader.f64().map_err(dec_err)?; // amount
        reader.var_i32().map_err(dec_err)?; // Operation
        reader.var_i32().map_err(dec_err)?; // EquipmentSlotGroup
        let display = reader.var_i32().map_err(dec_err)?;
        match display {
            // `default` and `hidden` are `StreamCodec.unit`: no payload.
            0 | 1 => {}
            // `override` carries the replacement text.
            2 => {
                read_network_nbt(reader).map_err(dec_err)?;
            }
            other => {
                return Err(AdapterError::Decode(format!(
                    "attribute modifier display type {other} is outside \
                     ItemAttributeModifiers.Display.Type's 0..=2"
                )));
            }
        }
    }
    Ok(())
}

/// Decodes a `minecraft:tool` component (26.2 `Tool.STREAM_CODEC`).
///
/// Wire shape, in order: a VarInt-counted list of rules, then the default mining
/// speed as an f32, the damage-per-block as a VarInt, and the
/// can-destroy-in-creative flag as a bool. Each rule is a `HolderSet<Block>`,
/// then an optional f32 speed and an optional bool correct-for-drops (both
/// `ByteBufCodecs::optional`, so a present-flag byte then the value).
///
/// Note this component is *rarely* on the wire: a stack carries only the delta
/// from its item's prototype component map, and vanilla puts a pickaxe's
/// `minecraft:tool` in that prototype. It appears for `/give …[minecraft:tool={…}]`
/// and datapack-authored items. The prototype half lives in [`lodestone_data::tool`];
/// both feed the same evaluator.
fn read_tool(reader: &mut Reader<'_>) -> Result<ItemTool, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid tool rule count {count}")))?;
    let mut rules = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let blocks = read_block_holder_set(reader)?;
        let speed = if reader.bool().map_err(dec_err)? {
            Some(reader.f32().map_err(dec_err)?)
        } else {
            None
        };
        let correct_for_drops = if reader.bool().map_err(dec_err)? {
            Some(reader.bool().map_err(dec_err)?)
        } else {
            None
        };
        rules.push(ToolRule::new(blocks, speed, correct_for_drops));
    }
    let default_mining_speed = reader.f32().map_err(dec_err)?;
    let damage_per_block = reader.var_i32().map_err(dec_err)?;
    let damage_per_block = u32::try_from(damage_per_block).map_err(|_| {
        AdapterError::Decode(format!("negative tool damage_per_block {damage_per_block}"))
    })?;
    let can_destroy_blocks_in_creative = reader.bool().map_err(dec_err)?;
    Ok(ItemTool::new(
        rules,
        default_mining_speed,
        damage_per_block,
        can_destroy_blocks_in_creative,
    ))
}

/// Decodes a `HolderSet<Block>` (26.2 `ByteBufCodecs.holderSet(Registries.BLOCK)`).
///
/// A single VarInt discriminates: `0` means a named tag follows as an
/// identifier string; any `n > 0` means `n - 1` direct holders follow, each a
/// **bare** `minecraft:block` registry id.
///
/// # The direct holders are *not* `id + 1`
///
/// There are two holder codecs in 26.2 and they differ by exactly one:
/// `ByteBufCodecs.holder(key, directCodec)` reserves `0` for an inline element
/// definition and so writes `id + 1`, while `ByteBufCodecs.holderRegistry(key)`
/// — which is what `holderSet` uses internally — delegates to the private
/// `registry(key, Registry::asHolderIdMap)` and writes the id **as-is**. Only
/// the outer set-size discriminator is offset by one.
///
/// This was originally implemented as `id + 1` by reading the *first* codec and
/// assuming the second matched. The hermetic test agreed, because it encoded the
/// same way; the live capture in `tests/live_tool_component.rs` did not — the
/// real server wrote `minecraft:stone` (registry id 1) as `01` and
/// `minecraft:obsidian` (193) as `c1 01`, and we decoded them as 0 and 192.
fn read_block_holder_set(reader: &mut Reader<'_>) -> Result<ToolBlocks, AdapterError> {
    let discriminator = reader.var_i32().map_err(dec_err)?;
    if discriminator == 0 {
        // Vanilla's `Identifier.STREAM_CODEC` is an unbounded UTF-8 string, so
        // the limit here is the shared 32,767-char ceiling the rest of this
        // adapter uses, not a tighter guess that could reject a valid tag.
        let tag = reader.string(32767).map_err(dec_err)?;
        return Ok(ToolBlocks::Tag(parse_key(&tag, "block tag")?));
    }
    let count = usize::try_from(discriminator - 1)
        .map_err(|_| AdapterError::Decode(format!("invalid block set size {discriminator}")))?;
    let mut blocks = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let raw = reader.var_i32().map_err(dec_err)?;
        if raw < 0 {
            return Err(AdapterError::Decode(format!(
                "negative block registry id {raw} in a tool rule"
            )));
        }
        blocks.push(raw);
    }
    Ok(ToolBlocks::Blocks(blocks))
}

/// Decodes an `ItemEnchantments` component: a VarInt-counted map of
/// `Holder<Enchantment>` (registry id, holder-encoded as `id + 1`) to a VarInt
/// level.
fn read_enchantments(reader: &mut Reader<'_>) -> Result<Vec<ItemEnchantment>, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid enchantment count {count}")))?;
    let mut enchantments = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let raw = reader.var_i32().map_err(dec_err)?;
        if raw <= 0 {
            // 0 is an inline holder (a full Enchantment definition); vanilla
            // sends registry references for item enchantments, never inline.
            return Err(AdapterError::Decode(
                "inline enchantment holder is not supported".to_owned(),
            ));
        }
        let level = reader.var_i32().map_err(dec_err)?;
        let level = u32::try_from(level)
            .map_err(|_| AdapterError::Decode(format!("negative enchantment level {level}")))?;
        enchantments.push(ItemEnchantment {
            id: raw - 1,
            level,
        });
    }
    Ok(enchantments)
}

/// Reads an item stack that is the final field of a packet, asserting no
/// trailing bytes remain — unless an unmodeled component left the stack partial,
/// in which case the unread remainder is deliberately dropped rather than raised
/// as a fatal decode error.
fn read_trailing_item_stack(
    reader: &mut Reader<'_>,
) -> Result<Option<ItemStack>, AdapterError> {
    match read_item_stack(reader)? {
        DecodedStack::Complete(stack) => {
            reader.ensure_empty().map_err(dec_err)?;
            Ok(stack)
        }
        // The misparse detector is skipped deliberately: there are unread bytes
        // by construction. (They are also already drained, so `ensure_empty`
        // would pass — running it anyway would make this arm look load-bearing
        // when it is not, and would silently start failing if the drain ever
        // went away.)
        DecodedStack::Partial(stack) => Ok(stack),
    }
}

/// Resolves an [`ItemStack`]'s canonical item key to protocol 776's numeric
/// item-registry id, attributing an unknown item loudly rather than silently
/// substituting a placeholder.
fn item_registry_id(stack: &ItemStack) -> Result<i32, AdapterError> {
    item_id(&stack.item.to_string())
        .ok_or_else(|| AdapterError::Encode(format!("unknown item key {}", stack.item)))
}

/// Writes a serverbound `set_creative_mode_slot` item
/// (`ItemStack.OPTIONAL_UNTRUSTED_STREAM_CODEC`): a VarInt count (`<= 0` is the
/// empty stack), then, only if non-empty, the item registry id as a VarInt and
/// an empty `DataComponentPatch` (VarInt `0` added, VarInt `0` removed).
///
/// Note: an [`ItemStack`] can now carry decoded components, but this serverbound
/// encoder deliberately writes the **empty** patch and does not re-serialise
/// them. Creative slot-set with custom components is out of Phase 1's scope; the
/// server accepts the empty patch and applies its own defaults. If creative
/// component round-tripping is ever needed, this is the single site to extend.
fn write_optional_item_stack(w: &mut Writer, item: Option<&ItemStack>) -> Result<(), AdapterError> {
    match item {
        None => w.var_i32(0),
        Some(stack) => {
            let count = i32::try_from(stack.count).map_err(|_| {
                AdapterError::Encode(format!("item count {} overflows i32", stack.count))
            })?;
            w.var_i32(count);
            w.var_i32(item_registry_id(stack)?);
            w.var_i32(0); // added components
            w.var_i32(0); // removed components
        }
    }
    Ok(())
}

/// Writes a serverbound container-click item as a `HashedStack`
/// (`ByteBufCodecs.optional(HashedStack.ActualItem.STREAM_CODEC)`): a bool
/// presence flag, then, only if present, the item registry id as a VarInt, the
/// count as a VarInt, and an empty `HashedPatchMap` (VarInt `0` added, VarInt
/// `0` removed).
///
/// The canonical [`ItemStack`] carries no components, so the patch is always
/// empty — the only shape this model can produce, and the common case for a
/// plain vanilla stack.
fn write_hashed_stack(w: &mut Writer, item: Option<&ItemStack>) -> Result<(), AdapterError> {
    match item {
        None => w.bool(false),
        Some(stack) => {
            w.bool(true);
            w.var_i32(item_registry_id(stack)?);
            let count = i32::try_from(stack.count).map_err(|_| {
                AdapterError::Encode(format!("item count {} overflows i32", stack.count))
            })?;
            w.var_i32(count);
            w.var_i32(0); // added components
            w.var_i32(0); // removed components
        }
    }
    Ok(())
}

/// Writes an `Optional<Holder<MobEffect>>` for the serverbound `set_beacon`
/// packet (`ByteBufCodecs.optional(MobEffect.STREAM_CODEC)`): a bool presence
/// flag, then, only if present, the effect's `minecraft:mob_effect` registry
/// id as a direct VarInt (`holderRegistry`, unlike the sound-holder codec, has
/// no inline-definition escape id).
fn write_optional_mob_effect(
    w: &mut Writer,
    effect: Option<&ResourceKey>,
) -> Result<(), AdapterError> {
    match effect {
        None => w.bool(false),
        Some(key) => {
            w.bool(true);
            let id = mob_effect_id(&key.to_string())
                .ok_or_else(|| AdapterError::Encode(format!("unknown mob effect {key}")))?;
            w.var_i32(id);
        }
    }
    Ok(())
}

/// Encodes the serverbound `set_beacon` packet body: two `Optional<Holder<MobEffect>>`
/// values (primary then secondary power), each written by
/// [`write_optional_mob_effect`].
fn encode_set_beacon(
    primary: Option<&ResourceKey>,
    secondary: Option<&ResourceKey>,
) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    write_optional_mob_effect(&mut w, primary)?;
    write_optional_mob_effect(&mut w, secondary)?;
    Ok(w.into_vec())
}

/// Encodes the serverbound `spectator_action` packet body
/// (`ServerboundSpectatorActionPacket`): a single VarInt using
/// `ByteBufCodecs.OPTIONAL_VAR_INT`'s offset encoding, **not** the common
/// bool-then-value optional shape — `0` means "not spectating an entity"
/// and a present id `i` is written as `i + 1`. This must be hand-written
/// rather than a derived `Option<i32>` field, since a naive bool-prefixed
/// encoder would silently produce a wire-incompatible packet that still
/// parses.
fn encode_spectator_action(target_entity_id: Option<i32>) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.var_i32(target_entity_id.map_or(0, |id| id + 1));
    Ok(w.into_vec())
}

/// Encodes the serverbound `seen_advancements` packet body
/// (`ServerboundSeenAdvancementsPacket`): a VarInt `Action` ordinal
/// (`OPENED_TAB` = 0, `CLOSED_SCREEN` = 1, via `FriendlyByteBuf.writeEnum`),
/// followed *only when opening a tab* by that tab's `minecraft:*` identifier
/// string (`writeIdentifier` = `writeUtf(id.toString())`). Closing writes
/// nothing further — the identifier's presence depends on the ordinal, so
/// this can't be a plain derived struct.
fn encode_seen_advancements(tab: Option<&ResourceKey>) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    match tab {
        Some(key) => {
            w.var_i32(0); // OPENED_TAB
            w.string(&key.to_string());
        }
        None => w.var_i32(1), // CLOSED_SCREEN
    }
    Ok(w.into_vec())
}

// ---- issue #304: the operator/debug serverbound encoders --------------------
//
// Thirteen packets a vanilla client can send that this adapter could not encode
// at all. Every layout below was read off the record definition in
// `.cache/mc/26.2/src` — the `write` method or the `StreamCodec` composition, not
// a summary — because there is no encoder of ours to round-trip against and
// `decode(encode(x)) == x` would be satisfied by two symmetric misunderstandings.
//
// Three of these have a shape a transliterating encoder gets wrong, and each is
// called out at its own function:
//
// * `set_structure_block`'s offset/size are **signed bytes**, not `Vec3i`
//   VarInts, and its flags byte is **last**;
// * `set_jigsaw_block`'s `joint` is a **string**, not an enum ordinal;
// * `custom_click_action`'s payload is **double-framed** — a VarInt byte length
//   wrapping an optional-NBT body.

/// Maps a [`Difficulty`] to `Difficulty.getId()`, which is what
/// `ByteBufCodecs.idMapper(Difficulty::byId, Difficulty::getId)` writes — the
/// declared enum order, `PEACEFUL` first.
fn difficulty_id(difficulty: Difficulty) -> i32 {
    match difficulty {
        Difficulty::Peaceful => 0,
        Difficulty::Easy => 1,
        Difficulty::Normal => 2,
        Difficulty::Hard => 3,
    }
}

/// `Rotation`'s wire id, from its own declared order
/// (`net/minecraft/world/level/block/Rotation.java`).
fn structure_rotation_id(rotation: StructureRotation) -> i32 {
    match rotation {
        StructureRotation::None => 0,
        StructureRotation::Clockwise90 => 1,
        StructureRotation::Clockwise180 => 2,
        StructureRotation::CounterClockwise90 => 3,
    }
}

/// Encodes the serverbound `set_game_rule` body: a VarInt-counted list of
/// `(rule identifier, value string)` pairs.
///
/// The value is a `STRING_UTF8` whatever the rule's real type is — the server
/// parses it against its own typed registry — so nothing here validates it.
fn encode_set_game_rules(entries: &[(ResourceKey, String)]) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.var_i32(i32::try_from(entries.len()).map_err(|_| {
        AdapterError::Encode("set_game_rule entry count exceeds i32".to_owned())
    })?);
    for (key, value) in entries {
        w.string(&key.to_string());
        w.string(value);
    }
    Ok(w.into_vec())
}

/// Encodes the serverbound `set_structure_block` body
/// (`ServerboundSetStructureBlockPacket.write`).
///
/// **Two traps, both invisible to a round trip against ourselves.** `offset` and
/// `size` are six `writeByte`s, not a `Vec3i`'s three VarInts each — vanilla
/// clamps them to `-48..=48` and `0..=48` on read, so an out-of-range value is
/// narrowed rather than refused, and this encoder narrows the same way rather
/// than emitting a byte that would wrap. And the flags byte is written **last**,
/// after `seed`, not next to the booleans it packs.
#[allow(clippy::too_many_arguments)]
fn encode_set_structure_block(
    pos: BlockPos,
    update_type: StructureBlockUpdateType,
    mode: StructureBlockMode,
    name: &str,
    offset: (i8, i8, i8),
    size: (i8, i8, i8),
    mirror: StructureMirror,
    rotation: StructureRotation,
    data: &str,
    integrity: f32,
    seed: i64,
    flags: u8,
) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.i64(pack_block_pos(pos));
    w.var_i32(match update_type {
        StructureBlockUpdateType::UpdateData => 0,
        StructureBlockUpdateType::SaveArea => 1,
        StructureBlockUpdateType::LoadArea => 2,
        StructureBlockUpdateType::ScanArea => 3,
    });
    w.var_i32(match mode {
        StructureBlockMode::Save => 0,
        StructureBlockMode::Load => 1,
        StructureBlockMode::Corner => 2,
        StructureBlockMode::Data => 3,
    });
    w.string(name);
    for axis in [offset.0, offset.1, offset.2] {
        w.i8(axis.clamp(-48, 48));
    }
    for axis in [size.0, size.1, size.2] {
        w.i8(axis.clamp(0, 48));
    }
    w.var_i32(match mirror {
        StructureMirror::None => 0,
        StructureMirror::LeftRight => 1,
        StructureMirror::FrontBack => 2,
    });
    w.var_i32(structure_rotation_id(rotation));
    w.string(data);
    w.f32(integrity.clamp(0.0, 1.0));
    w.var_i64(seed);
    w.u8(flags);
    Ok(w.into_vec())
}

/// Encodes the serverbound `set_jigsaw_block` body
/// (`ServerboundSetJigsawBlockPacket.write`).
///
/// The trap is `joint`: vanilla writes `joint.getSerializedName()`, a UTF string,
/// and falls back to `ALIGNED` for anything it cannot parse. An encoder that
/// wrote a VarInt ordinal here — the shape every other enum field in this packet
/// family uses — would produce a packet the server silently reads as a
/// zero-length name and defaults, i.e. a wrong value on a fully connected wire.
#[allow(clippy::too_many_arguments)]
fn encode_set_jigsaw_block(
    pos: BlockPos,
    name: &ResourceKey,
    target: &ResourceKey,
    pool: &ResourceKey,
    final_state: &str,
    joint: JigsawJoint,
    selection_priority: i32,
    placement_priority: i32,
) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.i64(pack_block_pos(pos));
    w.string(&name.to_string());
    w.string(&target.to_string());
    w.string(&pool.to_string());
    w.string(final_state);
    w.string(joint.serialized_name());
    w.var_i32(selection_priority);
    w.var_i32(placement_priority);
    Ok(w.into_vec())
}

/// Encodes the serverbound `test_instance_block_action` body
/// (`ServerboundTestInstanceBlockActionPacket` + `TestInstanceBlockEntity.Data`).
fn encode_test_instance_block_action(
    pos: BlockPos,
    action: TestInstanceAction,
    data: &TestInstanceData,
) -> Result<Vec<u8>, AdapterError> {
    let mut w = Writer::default();
    w.i64(pack_block_pos(pos));
    w.var_i32(match action {
        TestInstanceAction::Init => 0,
        TestInstanceAction::Query => 1,
        TestInstanceAction::Set => 2,
        TestInstanceAction::Reset => 3,
        TestInstanceAction::Save => 4,
        TestInstanceAction::Export => 5,
        TestInstanceAction::Run => 6,
    });
    match &data.test {
        Some(key) => {
            w.bool(true);
            w.string(&key.to_string());
        }
        None => w.bool(false),
    }
    w.var_i32(data.size.0);
    w.var_i32(data.size.1);
    w.var_i32(data.size.2);
    w.var_i32(structure_rotation_id(data.rotation));
    w.bool(data.ignore_entities);
    w.var_i32(match data.status {
        TestInstanceStatus::Cleared => 0,
        TestInstanceStatus::Running => 1,
        TestInstanceStatus::Finished => 2,
    });
    match &data.error_message {
        Some(component) => {
            w.bool(true);
            w.bytes(component);
        }
        None => w.bool(false),
    }
    Ok(w.into_vec())
}

/// Encodes the serverbound `debug_subscription_request` body: a VarInt-counted
/// list of `minecraft:debug_subscription` network ids, capped at 32 by the wire.
///
/// Unknown keys are **dropped** rather than failing the whole subscription — a
/// client asking for a feed this protocol does not have should get the rest,
/// which is also what makes an empty list (vanilla's "unsubscribe from
/// everything") indistinguishable from "all keys unknown". The caller sees the
/// difference through the returned count.
fn encode_debug_subscription_request(
    subscriptions: &[ResourceKey],
) -> Result<Vec<u8>, AdapterError> {
    let mut ids: Vec<i32> = subscriptions
        .iter()
        .filter_map(|key| {
            crate::stat_debug_registries::debug_subscription_id(&key.to_string())
        })
        .collect();
    ids.truncate(32);
    let mut w = Writer::default();
    w.var_i32(i32::try_from(ids.len()).map_err(|_| {
        AdapterError::Encode("debug subscription count exceeds i32".to_owned())
    })?);
    for id in ids {
        w.var_i32(id);
    }
    Ok(w.into_vec())
}

/// Encodes the serverbound `custom_click_action` body
/// (`ServerboundCustomClickActionPacket`).
///
/// **Double-framed.** The codec is
/// `optionalTagCodec(...).apply(lengthPrefixed(65536))`: an outer VarInt *byte
/// length*, and inside it the optional-NBT body. `payload` is already that inner
/// body (a leading present/absent byte and, if present, the NBT), so this only
/// adds the length prefix — writing the NBT with no prefix, or prefixing an
/// element count instead of a byte count, both produce something the server
/// cannot read.
fn encode_custom_click_action(id: &ResourceKey, payload: &[u8]) -> Result<Vec<u8>, AdapterError> {
    if payload.len() > 65536 {
        return Err(AdapterError::Encode(format!(
            "custom_click_action payload is {} bytes, over the wire's 65536 limit",
            payload.len()
        )));
    }
    let mut w = Writer::default();
    w.string(&id.to_string());
    w.var_bytes(payload)
        .map_err(|err| AdapterError::Encode(err.to_string()))?;
    Ok(w.into_vec())
}

/// Maps a [`ResourcePackResponseKind`] to `ServerboundResourcePackPacket.Action`'s
/// ordinal, matching its declared enum order.
fn resource_pack_response_ordinal(kind: ResourcePackResponseKind) -> i32 {
    match kind {
        ResourcePackResponseKind::SuccessfullyLoaded => 0,
        ResourcePackResponseKind::Declined => 1,
        ResourcePackResponseKind::FailedDownload => 2,
        ResourcePackResponseKind::Accepted => 3,
        ResourcePackResponseKind::Downloaded => 4,
        ResourcePackResponseKind::InvalidUrl => 5,
        ResourcePackResponseKind::FailedReload => 6,
        ResourcePackResponseKind::Discarded => 7,
    }
}

/// Maps a [`CommandBlockMode`] to `CommandBlockEntity.Mode`'s ordinal
/// (`0` sequence, `1` auto, `2` redstone).
fn command_block_mode_ordinal(mode: CommandBlockMode) -> i32 {
    match mode {
        CommandBlockMode::Sequence => 0,
        CommandBlockMode::Auto => 1,
        CommandBlockMode::Redstone => 2,
    }
}

/// Packs [`DisplayedSkinParts`] into vanilla's `client_information`
/// model-customisation bitmask (`PlayerModelPart`'s bit order): cape `0x01`,
/// jacket `0x02`, left sleeve `0x04`, right sleeve `0x08`, left pants leg
/// `0x10`, right pants leg `0x20`, hat `0x40`.
fn skin_parts_bitmask(parts: DisplayedSkinParts) -> u8 {
    u8::from(parts.cape)
        | (u8::from(parts.jacket) << 1)
        | (u8::from(parts.left_sleeve) << 2)
        | (u8::from(parts.right_sleeve) << 3)
        | (u8::from(parts.left_pants_leg) << 4)
        | (u8::from(parts.right_pants_leg) << 5)
        | (u8::from(parts.hat) << 6)
}

/// Decodes `sound`: a sound holder, a source category, a fixed-point position,
/// volume, pitch, and the server-rolled variant seed (forwarded untouched — the
/// variant is resolved client-side from the same seed so all clients agree).
fn decode_sound(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let (name, fixed_range) = read_sound_holder(&mut reader)?;
    let category = read_sound_category(&mut reader)?;
    let x = f64::from(reader.i32().map_err(dec_err)?) / SOUND_POSITION_SCALE;
    let y = f64::from(reader.i32().map_err(dec_err)?) / SOUND_POSITION_SCALE;
    let z = f64::from(reader.i32().map_err(dec_err)?) / SOUND_POSITION_SCALE;
    let volume = reader.f32().map_err(dec_err)?;
    let pitch = reader.f32().map_err(dec_err)?;
    let seed = reader.i64().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::Sound {
        sound: parse_key(&name, "sound")?,
        category,
        pos: Vec3 { x, y, z },
        volume,
        pitch,
        fixed_range,
        seed,
    })])
}

/// Decodes `sound_entity`: a sound holder, a source category, the entity id the
/// sound follows, volume, pitch, and the server-rolled variant seed.
fn decode_sound_entity(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let (name, fixed_range) = read_sound_holder(&mut reader)?;
    let category = read_sound_category(&mut reader)?;
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let volume = reader.f32().map_err(dec_err)?;
    let pitch = reader.f32().map_err(dec_err)?;
    let seed = reader.i64().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::EntitySound {
        sound: parse_key(&name, "sound")?,
        category,
        entity_id,
        volume,
        pitch,
        fixed_range,
        seed,
    })])
}

/// The `explosion_emitter`/`explosion` particle registry ids — the two
/// "simple" (argument-less) particle types every `Level.explode` call site
/// passes as `explosionParticle` (`Level.java:593,619,645`, all
/// `ParticleTypes.EXPLOSION_EMITTER`; `ServerExplosion`'s small/large split
/// can also choose `ParticleTypes.EXPLOSION`).
/// `ParticleTypes.STREAM_CODEC` dispatches on a registry id
/// (`ByteBufCodecs.registry(Registries.PARTICLE_TYPE)`, a plain 0-based
/// VarInt — **not** the `id + 1` "holder" scheme [`read_sound_holder`] and the
/// villager-data field use), and a `SimpleParticleType`'s own stream codec
/// reads no further bytes. Recognising just these two ids and rejecting
/// everything else is therefore sufficient to stay byte-aligned through this
/// field without modelling the full particle-options codec (dust colour,
/// block state, item stack, …) that `metadata.rs`'s `SER_PARTICLE`/
/// `SER_PARTICLES` already reject for the identical reason.
const PARTICLE_ID_EXPLOSION_EMITTER: i32 = 29;
const PARTICLE_ID_EXPLOSION: i32 = 30;

/// Decodes `explode` (protocol id 36): a creeper/TNT/bed/respawn-anchor
/// detonation, `ClientboundExplodePacket`.
///
/// # Server-sent, not client-predicted
///
/// Unlike a player's own block break (`e2544b9`: no level event is ever sent
/// at all, and the sound is predicted), an explosion's sound rides explicitly
/// on this packet's `explosionSound` field, and
/// `ClientPacketListener.handleExplosion` (`ClientPacketListener.java:1357`)
/// does nothing but play exactly what the server sent, at a
/// **client-rolled** pitch:
///
/// ```text
/// playLocalSound(center, packet.explosionSound(), SoundSource.BLOCKS, 4.0F,
///     (1.0F + (random.nextFloat() - random.nextFloat()) * 0.2F) * 0.7F, false)
/// ```
///
/// `volume` (`4.0`) is a client-side constant, never on the wire. `pitch` is
/// rolled by vanilla's own client from local randomness and is not on the
/// wire either — so this decoder rolls the identical die rather than
/// inventing a fixed pitch. A real client's explosion pitch already varies
/// run to run; a constant here would be *less* faithful, not more.
///
/// # What this does not decode
///
/// `radius`, `blockCount` and `playerKnockback` are consumed for wire
/// alignment only — no consumer today. `explosionParticle` is consumed via
/// the narrow allowlist above. `blockParticles` (the flying-debris
/// `WeightedList<ExplosionParticleInfo>`) is **not** decoded at all:
/// `explosionSound` is the second-to-last field the packet carries, so once
/// it is read there is nothing left this seam needs, and modelling
/// `ExplosionParticleInfo`'s own nested particle-options codec would cost
/// real complexity for zero consumers. This is therefore one of the packets
/// that does not run the trailing-bytes misparse check — like `metadata.rs`'s
/// partial item-stack decode, deliberately, not an oversight.
///
/// The flying block-debris particles (`blockParticles`) remain unimplemented
/// for the reason above. The shockwave/smoke visual itself is issue #416:
/// this decoder now also emits a `ClientEvent::Particles` directive for
/// `explosion_emitter` (`ParticleTypes.EXPLOSION_EMITTER`, the id this
/// packet actually carries — `HugeExplosionSeedParticle` is what schedules
/// the follow-up `HugeExplosionParticle`s vanilla-side, per
/// `docs/particle-catalogue.md`'s "Built, issue #416" entry), alongside the
/// existing `Sound` directive. `net.rs`/`sim.rs` need no new arm: this
/// crate's `ClientEvent::Particles` already forwards generically into
/// `Particles::spawn_particles`.
fn decode_explode(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let x = reader.f64().map_err(dec_err)?;
    let y = reader.f64().map_err(dec_err)?;
    let z = reader.f64().map_err(dec_err)?;
    let _radius = reader.f32().map_err(dec_err)?;
    let _block_count = reader.i32().map_err(dec_err)?;
    if reader.bool().map_err(dec_err)? {
        // `playerKnockback: Optional<Vec3>` — consumed, not applied yet.
        reader.f64().map_err(dec_err)?;
        reader.f64().map_err(dec_err)?;
        reader.f64().map_err(dec_err)?;
    }
    let particle_id = reader.var_i32().map_err(dec_err)?;
    if particle_id != PARTICLE_ID_EXPLOSION_EMITTER && particle_id != PARTICLE_ID_EXPLOSION {
        return Err(AdapterError::Decode(format!(
            "explode: unmodeled explosionParticle registry id {particle_id} (only \
             explosion_emitter/explosion are simple enough to skip byte-accurately)"
        )));
    }
    let (name, fixed_range) = read_sound_holder(&mut reader)?;
    // `blockParticles` follows and is deliberately not decoded — see the
    // function doc above. No `reader.ensure_empty()` call here on purpose.
    //
    // Issue #416: the shockwave/smoke visual, alongside the sound below.
    // Always `explosion_emitter` regardless of which of the two ids this
    // packet carried — `HugeExplosionSeedParticle` is what schedules the
    // follow-up `HugeExplosionParticle`s client-side (see
    // `Particle::tick_huge_explosion_seed`), so the seed is the one real
    // vanilla explosions actually spawn from this packet.
    Ok(vec![
        Directive::Emit(ClientEvent::Particles {
            particle: parse_key("explosion_emitter", "particle")?,
            long_distance: false,
            pos: Vec3::new(x, y, z),
            offset: Vec3f::new(0.0, 0.0, 0.0),
            max_speed: 0.0,
            count: 1,
        }),
        Directive::Emit(ClientEvent::Sound {
            sound: parse_key(&name, "sound")?,
            category: SoundCategory::Block,
            pos: Vec3::new(x, y, z),
            volume: 4.0,
            pitch: (1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.2) * 0.7,
            fixed_range,
            seed: rand::random(),
        }),
    ])
}

/// Decodes `open_screen`: a container id, a `minecraft:menu` registry id, and an
/// NBT text-component title.
fn decode_open_screen(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let window_id = reader.var_i32().map_err(dec_err)?;
    let menu_id = reader.var_i32().map_err(dec_err)?;
    let menu = menu_name(menu_id)
        .ok_or_else(|| AdapterError::Decode(format!("unknown menu id {menu_id}")))?;
    let menu_type = parse_key(menu, "menu")?;
    let title = read_network_nbt(&mut reader).map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::ScreenOpened {
        window_id,
        menu_type,
        title: Text::from_nbt(&title),
    })])
}

/// Decodes `damage_event`: entity id, a `minecraft:damage_type` registry id
/// (`ByteBufCodecs.holderRegistry`, a plain VarInt — carried raw, see
/// [`ClientEvent::EntityDamaged`] for why), then the cause/direct entity ids
/// each wire-encoded as `id + 1` (so `0` means "none", decoded here back to
/// `-1` via `varint - 1` to match vanilla's own `readOptionalEntityId`), and
/// finally a self-contained `Optional<Vec3>` (a bool presence flag then, only
/// if set, three plain `f64`s) — the one shape in this packet the `Decode`
/// derive's `present_if` (which only reads a *prior named field*, not an
/// inline bool) cannot express, so it is read by hand like the rest of this
/// packet.
fn decode_damage_event(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let damage_type_id = reader.var_i32().map_err(dec_err)?;
    let cause_id = reader.var_i32().map_err(dec_err)? - 1;
    let direct_id = reader.var_i32().map_err(dec_err)? - 1;
    let has_pos = reader.bool().map_err(dec_err)?;
    let source_pos = if has_pos {
        let x = reader.f64().map_err(dec_err)?;
        let y = reader.f64().map_err(dec_err)?;
        let z = reader.f64().map_err(dec_err)?;
        Some(Vec3 { x, y, z })
    } else {
        None
    };
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::EntityDamaged {
        entity_id,
        damage_type_id,
        cause_id: (cause_id != -1).then_some(cause_id),
        direct_id: (direct_id != -1).then_some(direct_id),
        source_pos,
    })])
}

/// Builds a [`Directive::Send`] from a packet id and an encodable body.
fn send<T: Encode>(packet_id: i32, packet: &T) -> Result<Directive, AdapterError> {
    Ok(Directive::Send {
        packet_id,
        payload: encode_body(packet)?,
    })
}

/// Maps a decode error to the adapter's decode-error variant.
fn dec_err(err: impl std::fmt::Display) -> AdapterError {
    AdapterError::Decode(err.to_string())
}

/// Reads a wire `BitSet` — a varint `long`-count followed by that many
/// big-endian 64-bit words (`BitSet.toLongArray()`, LSB-first bit order) —
/// returning the words for [`LightPatch::from_light_masks`] to index. The count
/// is bounded by the readable words so a garbled length cannot pre-allocate an
/// enormous vector.
fn read_wire_bitset(r: &mut Reader<'_>) -> Result<Vec<u64>, AdapterError> {
    let count = r.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("negative bitset long-count {count}")))?;
    if count > r.remaining() / 8 {
        return Err(AdapterError::Decode(format!(
            "bitset long-count {count} exceeds {} readable words",
            r.remaining() / 8
        )));
    }
    let mut words = Vec::with_capacity(count);
    for _ in 0..count {
        words.push(r.u64().map_err(dec_err)?);
    }
    Ok(words)
}

/// Reads a `light_update` nibble-array list: a varint element count, then each
/// element as a varint byte-length plus that many bytes, validated to be
/// exactly 2048 by [`NibbleArray::from_bytes`]. The count is bounded by the
/// readable bytes (each element is at least one byte) to cap pre-allocation.
fn read_light_arrays(r: &mut Reader<'_>) -> Result<Vec<NibbleArray>, AdapterError> {
    let count = r.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("negative light-array count {count}")))?;
    if count > r.remaining() {
        return Err(AdapterError::Decode(format!(
            "light-array count {count} exceeds {} readable bytes",
            r.remaining()
        )));
    }
    let mut arrays = Vec::with_capacity(count);
    for _ in 0..count {
        let len = r.var_i32().map_err(dec_err)?;
        let len = usize::try_from(len)
            .map_err(|_| AdapterError::Decode(format!("negative light-array length {len}")))?;
        let bytes = r.bytes(len).map_err(dec_err)?;
        arrays.push(NibbleArray::from_bytes(bytes).map_err(dec_err)?);
    }
    Ok(arrays)
}

/// The delta-position scale for `move_entity_*` packets: each short is
/// `1/4096` of a block (`ClientboundMoveEntityPacket`).
const MOVE_DELTA_SCALE: f64 = 4096.0;

/// Lowers a `Relative` bit set (see `net.minecraft.world.entity.Relative`) to
/// the canonical [`TeleportFlags`]. Bits: X=0, Y=1, Z=2, Y_ROT=3, X_ROT=4.
fn teleport_flags(value: i32) -> TeleportFlags {
    TeleportFlags {
        relative_x: value & (1 << 0) != 0,
        relative_y: value & (1 << 1) != 0,
        relative_z: value & (1 << 2) != 0,
        relative_yaw: value & (1 << 3) != 0,
        relative_pitch: value & (1 << 4) != 0,
    }
}

/// Decodes `player_position` and returns the teleport-accept confirmation plus
/// the canonical teleport event.
///
/// Wire layout (`ClientboundPlayerPositionPacket`): VarInt teleport id, a
/// `PositionMoveRotation` (position `f64`×3, delta-movement `f64`×3, yaw `f32`,
/// pitch `f32`), then a big-endian `i32` `Relative` bit set. The delta-movement
/// is consumed for alignment but not surfaced here — player velocity is owned by
/// the physics layer, which applies it from the same packet. Zero trailing
/// bytes is the misparse detector.
fn handle_player_position(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let id = reader.var_i32().map_err(dec_err)?;
    let x = reader.f64().map_err(dec_err)?;
    let y = reader.f64().map_err(dec_err)?;
    let z = reader.f64().map_err(dec_err)?;
    let _dx = reader.f64().map_err(dec_err)?;
    let _dy = reader.f64().map_err(dec_err)?;
    let _dz = reader.f64().map_err(dec_err)?;
    let yaw = reader.f32().map_err(dec_err)?;
    let pitch = reader.f32().map_err(dec_err)?;
    let relatives = reader.i32().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;

    Ok(vec![
        send(
            play::serverbound::ACCEPT_TELEPORTATION,
            &AcceptTeleportation { id },
        )?,
        Directive::Emit(ClientEvent::TeleportPlayer {
            pos: Vec3::new(x, y, z),
            rotation: Rotation::new(yaw, pitch),
            flags: teleport_flags(relatives),
        }),
    ])
}

/// Decodes `add_entity` into a canonical spawn event, plus an initial
/// head-rotation event.
///
/// Wire layout (`ClientboundAddEntityPacket`): VarInt entity id, UUID, VarInt
/// entity-type registry id, position `f64`×3, low-precision velocity, three
/// signed-byte angles (pitch, yaw, head yaw), and a VarInt data field. The type
/// id is resolved to its canonical identifier through the version-specific
/// [`entity_type_name`] table.
///
/// Head yaw is carried separately from body yaw on the wire (they diverge
/// constantly once a mob starts looking around) and vanilla sends it
/// unconditionally at spawn, so it is surfaced through the same
/// [`ClientEvent::EntityHeadRotation`] outlet [`ROTATE_HEAD`](play::clientbound::ROTATE_HEAD)
/// uses for later updates, rather than widening [`ClientEvent::EntitySpawned`]
/// itself — that struct is shared across every protocol version's adapter, and
/// adding a field to it would force edits into v47/v340/v735 outside this
/// crate's scope.
fn handle_add_entity(
    payload: &[u8],
    variants: &Mutex<HashMap<i32, TrackedEntity>>,
) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let uuid = reader.uuid().map_err(dec_err)?;
    let type_id = reader.var_i32().map_err(dec_err)?;
    let x = reader.f64().map_err(dec_err)?;
    let y = reader.f64().map_err(dec_err)?;
    let z = reader.f64().map_err(dec_err)?;
    let (vx, vy, vz) = read_lp_vec3(&mut reader).map_err(dec_err)?;
    let pitch = reader.i8().map_err(dec_err)?;
    let yaw = reader.i8().map_err(dec_err)?;
    let head_yaw = reader.i8().map_err(dec_err)?;
    // The **Object Data** field: one trailing VarInt whose meaning is decided
    // entirely by the entity type, read in that type's own `recreateFromPacket`.
    // Most types ignore it; `FallingBlockEntity`'s is
    // `this.blockState = Block.stateById(packet.getData())`, resolved below once
    // the type is known.
    let data = reader.var_i32().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;

    let name = entity_type_name(type_id).ok_or_else(|| {
        AdapterError::Decode(format!("unknown entity-type id {type_id} in add_entity"))
    })?;
    let entity_type = name.parse().map_err(|_| {
        AdapterError::Decode(format!(
            "entity-type id {type_id} is not a valid key: {name}"
        ))
    })?;

    // Remember the facts a later `set_entity_data` cannot recover from the wire:
    // the concrete class for mobs whose variant index is ambiguous, whether the
    // type is a `LivingEntity` (which decides whether index 8's byte is a
    // using-item bitfield or an arrow's crit flag — see `IDX_LIVING_FLAGS`), and
    // whether it is a `Mob` (index 15: mob flags, or an armour stand's client
    // flags — see `IDX_MOB_FLAGS`). Types with none of those stay out of the map,
    // so it is still bounded to the mobs actually present rather than every
    // entity in render distance.
    //
    // `is_living`/`is_mob` returning `None` for an id outside the census means "we
    // cannot establish it", which fails closed to `false`: a missing pose is a
    // visible gap, a wrongly-decoded flags byte is a silent lie.
    let tracked = TrackedEntity {
        class: metadata_class(name),
        living: lodestone_data::entity_census::is_living(type_id).unwrap_or(false),
        mob: lodestone_data::entity_census::is_mob(type_id).unwrap_or(false),
    };
    if tracked.is_tracked()
        && let Ok(mut map) = variants.lock()
    {
        map.insert(entity_id, tracked);
    }

    let mut directives = vec![
        Directive::Emit(ClientEvent::EntitySpawned {
            entity_id,
            uuid: Some(uuid),
            entity_type,
            pos: Vec3::new(x, y, z),
            rotation: Rotation::new(unpack_degrees(yaw), unpack_degrees(pitch)),
            velocity: Some(Vec3::new(vx, vy, vz)),
        }),
        Directive::Emit(ClientEvent::EntityHeadRotation {
            entity_id,
            head_yaw: unpack_degrees(head_yaw),
        }),
    ];

    // Vanilla's `SynchedEntityData` only ever puts a field on the wire when it
    // differs from the accessor's own default (`DataItem.isSetToDefault`,
    // `SynchedEntityData.getNonDefaultValues` — the only source `ServerEntity`
    // ever feeds a spawn's initial `set_entity_data`; see
    // `Sheep.defineSynchedData`: `entityData.define(DATA_WOOL_ID, (byte)0)`).
    // A naturally white, unsheared sheep (colour ordinal 0, sheared bit unset —
    // exactly byte `0`) therefore never puts index 18 on the wire at all, not
    // just at spawn: `read_entity_metadata` never sees the byte, `variant` stays
    // `None`, and every consumer keyed on `Some(EntityVariant::Dyed { .. })`
    // (`entities::sheep_wool`) draws no wool. A dyed or sheared sheep works
    // today because *that* state is non-default and is always on the wire.
    //
    // The fix is synthesizing the vanilla default here, once, as an ordinary
    // `EntityMetadataUpdated` event through the exact same channel a real
    // `set_entity_data` uses — so every downstream consumer (the ECS fold, the
    // shell snapshot) needs no special case for "unreported": a real
    // `set_entity_data` naming index 18 (dye, shear) is decoded afterward in
    // packet order and overwrites this default exactly as it would overwrite
    // any other synthesized-then-corrected value. Only sheep gets this: horse's
    // default variant is deferred (see `docs/entity-rendering.md`'s variant
    // census) rather than guessed at without the same wire confirmation.
    if tracked.class == Some(MetadataClass::Sheep) {
        directives.push(Directive::Emit(ClientEvent::EntityMetadataUpdated {
            entity_id,
            metadata: EntityMetadataUpdate {
                variant: Some(EntityVariant::Dyed {
                    color: 0,
                    sheared: false,
                }),
                ..EntityMetadataUpdate::default()
            },
        }));
    }

    // Same synthesis, same reason, for a creeper's three fields
    // (`Creeper.java:100-102`: `entityData.define(DATA_SWELL_DIR, -1)` /
    // `DATA_IS_POWERED, false` / `DATA_IS_IGNITED, false`). An ordinary,
    // uncharged, unlit creeper is *entirely* at its accessors' defaults, so a
    // real spawn's initial `set_entity_data` never mentions any of the three —
    // without this, a fresh creeper's `creeper_swell_dir` stays `None` forever
    // rather than the vanilla-true `Some(-1)`, until the moment it primes
    // changes it to a non-default value the wire actually carries.
    if tracked.class == Some(MetadataClass::Creeper) {
        directives.push(Directive::Emit(ClientEvent::EntityMetadataUpdated {
            entity_id,
            metadata: EntityMetadataUpdate {
                creeper_swell_dir: Some(-1),
                creeper_powered: Some(false),
                creeper_ignited: Some(false),
                ..EntityMetadataUpdate::default()
            },
        }));
    }

    // `FallingBlockEntity.recreateFromPacket`: the Object Data field read above is
    // `Block.getId(blockState)` and is the **only** place the imitated state
    // appears on the wire — `defineSynchedData` registers `DATA_START_POS` alone,
    // so no `set_entity_data` ever carries it. A consumer that never learns it
    // draws whatever state id `0` happens to be, with nothing logged.
    //
    // Emitted after `EntitySpawned` so a consumer keyed on the entity id always
    // sees the entity first. Guarded on the type rather than emitted for every
    // spawn: the field means something different for every type that reads it
    // (a display block, an item-frame rotation), and one event that claimed to
    // carry "a block state" for all of them would be wrong for most.
    if name == FALLING_BLOCK_TYPE {
        directives.push(Directive::Emit(ClientEvent::FallingBlockState {
            entity_id,
            // `max(0)` then a cast: the wire field is a signed VarInt and a
            // negative value is not a state id. Clamping to `0` (air, which bakes
            // no quads and therefore draws nothing) is the one reading that cannot
            // panic or wrap into a plausible-looking wrong block.
            block_state_id: data.max(0) as u32,
        }));
    }

    Ok(directives)
}

/// `EntityTypes.FALLING_BLOCK`'s registry key — the one entity type whose
/// `ADD_ENTITY` Object Data field this adapter interprets.
const FALLING_BLOCK_TYPE: &str = "minecraft:falling_block";

/// Decodes `remove_entities` (a VarInt-length list of VarInt ids) into a removal
/// event.
fn handle_remove_entities(
    payload: &[u8],
    variants: &Mutex<HashMap<i32, TrackedEntity>>,
) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("negative remove_entities count {count}")))?;
    let mut entity_ids = Vec::with_capacity(count);
    for _ in 0..count {
        entity_ids.push(reader.var_i32().map_err(dec_err)?);
    }
    reader.ensure_empty().map_err(dec_err)?;
    if let Ok(mut map) = variants.lock() {
        for id in &entity_ids {
            map.remove(id);
        }
    }
    Ok(vec![Directive::Emit(ClientEvent::EntityRemoved {
        entity_ids,
    })])
}

/// Decodes a `move_entity_*` packet into a relative-movement event. `has_pos`
/// and `has_rot` select which of the three variants (`pos`, `pos_rot`, `rot`)
/// is present: each short position delta is `1/4096` of a block and each angle
/// is a signed byte.
fn handle_move_entity(
    payload: &[u8],
    has_pos: bool,
    has_rot: bool,
) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let delta = if has_pos {
        let dx = f64::from(reader.i16().map_err(dec_err)?) / MOVE_DELTA_SCALE;
        let dy = f64::from(reader.i16().map_err(dec_err)?) / MOVE_DELTA_SCALE;
        let dz = f64::from(reader.i16().map_err(dec_err)?) / MOVE_DELTA_SCALE;
        Vec3::new(dx, dy, dz)
    } else {
        Vec3::new(0.0, 0.0, 0.0)
    };
    let rotation = if has_rot {
        let yaw = reader.i8().map_err(dec_err)?;
        let pitch = reader.i8().map_err(dec_err)?;
        Some(Rotation::new(unpack_degrees(yaw), unpack_degrees(pitch)))
    } else {
        None
    };
    let on_ground = reader.bool().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;

    Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
        entity_id,
        movement: EntityMovement::Relative(delta),
        rotation,
        on_ground,
    })])
}

/// Decodes an absolute entity position update. `has_relatives` selects between
/// `teleport_entity` (which carries a trailing `Relative` bit set) and
/// `entity_position_sync` (which does not); both share a leading VarInt id and
/// `PositionMoveRotation`, then a trailing on-ground boolean. The delta-movement
/// is consumed for alignment; velocity is surfaced separately via
/// `set_entity_motion`.
fn handle_entity_position(
    payload: &[u8],
    has_relatives: bool,
) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let x = reader.f64().map_err(dec_err)?;
    let y = reader.f64().map_err(dec_err)?;
    let z = reader.f64().map_err(dec_err)?;
    let _dx = reader.f64().map_err(dec_err)?;
    let _dy = reader.f64().map_err(dec_err)?;
    let _dz = reader.f64().map_err(dec_err)?;
    let yaw = reader.f32().map_err(dec_err)?;
    let pitch = reader.f32().map_err(dec_err)?;
    if has_relatives {
        let _relatives = reader.i32().map_err(dec_err)?;
    }
    let on_ground = reader.bool().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;

    Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
        entity_id,
        movement: EntityMovement::Absolute(Vec3::new(x, y, z)),
        rotation: Some(Rotation::new(yaw, pitch)),
        on_ground,
    })])
}

/// Decodes `set_entity_motion` (VarInt id, low-precision velocity) into a
/// velocity event.
fn handle_set_entity_motion(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let (vx, vy, vz) = read_lp_vec3(&mut reader).map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::EntityVelocity {
        entity_id,
        velocity: Vec3::new(vx, vy, vz),
    })])
}

/// Decodes `move_minecart_along_track`: a VarInt entity id followed by a
/// VarInt-counted list of `NewMinecartBehavior.MinecartStep` lerp steps, each
/// `(Vec3 position, Vec3 movement, ROTATION_BYTE yRot, ROTATION_BYTE xRot,
/// f32 weight)` in that order — verified against
/// `NewMinecartBehavior.MinecartStep.STREAM_CODEC` in 26.2 decompiled source.
/// `Vec3.STREAM_CODEC` is three big-endian f64s (matching every other
/// absolute-position decode in this adapter); `ROTATION_BYTE` is the same
/// signed-byte-angle encoding [`unpack_degrees`] already inverts for
/// `rotate_head`/`move_entity_*`.
///
/// Vanilla spends the whole list smoothly interpolating the cart across the
/// tick window the steps span (a curved rail sends more than one step per
/// packet); this adapter has no multi-waypoint movement event, so every
/// step's bytes are read and validated — a wire-format drift is still
/// caught — but only the **terminal** step's position/velocity/rotation is
/// applied, as an absolute jump rather than a spline. That is a documented
/// fidelity loss (movement will look stepped on curved track), not a
/// misdecode: minecarts stopped receiving ordinary `move_entity_*` packets
/// once this one exists, so without it a minecart snaps to reachable but
/// visibly discrete positions.
fn handle_move_minecart_along_track(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let count = reader.var_i32().map_err(dec_err)?;
    if count < 0 {
        return Err(AdapterError::Decode(format!(
            "negative minecart lerp step count {count}"
        )));
    }
    let mut terminal = None;
    for _ in 0..count {
        let x = reader.f64().map_err(dec_err)?;
        let y = reader.f64().map_err(dec_err)?;
        let z = reader.f64().map_err(dec_err)?;
        let vx = reader.f64().map_err(dec_err)?;
        let vy = reader.f64().map_err(dec_err)?;
        let vz = reader.f64().map_err(dec_err)?;
        let yaw = reader.i8().map_err(dec_err)?;
        let pitch = reader.i8().map_err(dec_err)?;
        let _weight = reader.f32().map_err(dec_err)?;
        terminal = Some((
            Vec3::new(x, y, z),
            Vec3::new(vx, vy, vz),
            unpack_degrees(yaw),
            unpack_degrees(pitch),
        ));
    }
    reader.ensure_empty().map_err(dec_err)?;

    let Some((pos, velocity, yaw, pitch)) = terminal else {
        // An empty step list carries no new pose; nothing to apply.
        return Ok(Vec::new());
    };
    Ok(vec![
        Directive::Emit(ClientEvent::EntityMoved {
            entity_id,
            movement: EntityMovement::Absolute(pos),
            rotation: Some(Rotation::new(yaw, pitch)),
            // MinecartStep carries no on-rail/on-ground bit.
            on_ground: false,
        }),
        Directive::Emit(ClientEvent::EntityVelocity { entity_id, velocity }),
    ])
}

/// Decodes `set_entity_data` into a metadata update event.
///
/// A metadata payload is length-framed, so a misparse is contained to this one
/// packet and cannot corrupt the stream. Rather than fail the whole connection
/// when a rare, unmodelled serializer appears on some exotic entity, a decode
/// error (or any trailing bytes, the misparse detector) is swallowed and no
/// event is emitted — the entity simply keeps its prior metadata. A genuinely
/// missing seam therefore surfaces as *absent fields* in a live test, loudly,
/// rather than as a dropped connection.
///
/// The one case where trailing bytes are *expected* is a stack carrying an
/// unmodeled data component: the item codec cannot skip it, so the metadata
/// decoder abandons the rest of the list and reports `complete == false`.
/// Running the misparse detector there would discard the item identity that was
/// already decoded exactly — fail-closed on the very packet this seam exists to
/// deliver — so the check is skipped and the partial update is emitted. Metadata
/// is applied incrementally, so a partial update is ordinary, not lossy.
fn handle_set_entity_data(
    payload: &[u8],
    variants: &Mutex<HashMap<i32, TrackedEntity>>,
) -> Vec<Directive> {
    let mut reader = Reader::new(payload);
    let Ok(entity_id) = reader.var_i32() else {
        return Vec::new();
    };
    // An id with no entry is an entity we chose not to track, which means it is
    // neither an ambiguous-variant mob nor a `LivingEntity` — so the default's
    // `living: false` is the right answer for it, not a lost fact.
    let tracked = variants
        .lock()
        .ok()
        .and_then(|map| map.get(&entity_id).copied())
        .unwrap_or_default();
    match read_entity_metadata(&mut reader, tracked) {
        // `complete == false` short-circuits the trailing-bytes check: the
        // reader is deliberately parked mid-payload there.
        Ok(decoded)
            if (!decoded.complete || reader.ensure_empty().is_ok())
                && !decoded.metadata.is_empty() =>
        {
            vec![Directive::Emit(ClientEvent::EntityMetadataUpdated {
                entity_id,
                metadata: decoded.metadata,
            })]
        }
        _ => Vec::new(),
    }
}

/// Decodes `update_attributes` into an attributes event, swallowing per-packet
/// decode errors for the same framing reason as [`handle_set_entity_data`].
fn handle_update_attributes(payload: &[u8]) -> Vec<Directive> {
    let mut reader = Reader::new(payload);
    match read_update_attributes(&mut reader) {
        Ok((entity_id, attributes)) if reader.ensure_empty().is_ok() && !attributes.is_empty() => {
            vec![Directive::Emit(ClientEvent::EntityAttributesUpdated {
                entity_id,
                attributes,
            })]
        }
        _ => Vec::new(),
    }
}

/// Lifts a wire [`DimensionType`] into the version-free [`DimensionTypeInfo`].
///
/// Returns `None` only when the registry entry's own id is not a valid
/// identifier — a server sending one has bigger problems, and reporting "not
/// resolved" is safer than inventing a key. The vertical window has already been
/// installed by the caller at that point, so a malformed *name* does not cost us
/// the correct chunk shape.
fn dimension_type_info(name: &str, value: &DimensionType) -> Option<DimensionTypeInfo> {
    Some(DimensionTypeInfo {
        name: name.parse::<ResourceKey>().ok()?,
        has_skylight: value.has_skylight,
        has_ceiling: value.has_ceiling,
        has_fixed_time: value.has_fixed_time,
        coordinate_scale: value.coordinate_scale,
        min_y: value.min_y,
        height: value.height,
        logical_height: value.logical_height,
        ambient_light: value.ambient_light,
    })
}

/// Maps a numeric game-type byte to the canonical [`GameMode`].
fn game_mode(value: u8) -> Result<GameMode, AdapterError> {
    match value {
        0 => Ok(GameMode::Survival),
        1 => Ok(GameMode::Creative),
        2 => Ok(GameMode::Adventure),
        3 => Ok(GameMode::Spectator),
        other => Err(AdapterError::Decode(format!("unknown game type {other}"))),
    }
}

/// Maps a tab-list game-mode id to the canonical [`GameMode`], returning `None`
/// for anything outside the four known modes (including the `-1` "no game mode"
/// sentinel a tab-list refresh may carry) rather than failing the whole packet.
fn tab_game_mode(id: i32) -> Option<GameMode> {
    match id {
        0 => Some(GameMode::Survival),
        1 => Some(GameMode::Creative),
        2 => Some(GameMode::Adventure),
        3 => Some(GameMode::Spectator),
        _ => None,
    }
}

/// Registry ids of `minecraft:slot_display`, from `SlotDisplays.java`'s
/// registration order (and cross-checked against `registries.json`).
///
/// A **built-in** registry, so these ids are fixed by the jar rather than synced
/// during Configuration — the same reason `MAP_DECORATION_TYPE_IDS` is a table.
mod slot_display {
    pub const EMPTY: i32 = 0;
    pub const ANY_FUEL: i32 = 1;
    pub const WITH_ANY_POTION: i32 = 2;
    pub const ONLY_WITH_COMPONENT: i32 = 3;
    pub const ITEM: i32 = 4;
    pub const ITEM_STACK: i32 = 5;
    pub const TAG: i32 = 6;
    pub const DYED: i32 = 7;
    pub const SMITHING_TRIM: i32 = 8;
    pub const WITH_REMAINDER: i32 = 9;
    pub const COMPOSITE: i32 = 10;
}

/// What walking a `SlotDisplay` yielded.
///
/// `complete == false` means the walk hit something this adapter does not model
/// and **the reader's position is no longer trustworthy** — the caller must
/// abandon the whole packet rather than continue. Same convention as
/// [`read_component_patch`]'s second return value, and for the same reason: a
/// nested union with per-entry codecs cannot be skipped generically, so partial
/// progress is the honest outcome and silently continuing would misread every
/// following field.
#[derive(Debug, Default)]
struct SlotDisplayItems {
    /// Item registry ids this display can show, in encounter order.
    items: Vec<i32>,
    /// Whether the walk consumed the display exactly.
    complete: bool,
}

impl SlotDisplayItems {
    fn incomplete() -> Self {
        Self {
            items: Vec::new(),
            complete: false,
        }
    }
}

/// Walks one `SlotDisplay` (`SlotDisplay.STREAM_CODEC`), collecting the item ids
/// it can display.
///
/// # This is a byte-exact walk, not a skip
///
/// `SlotDisplay` is a **recursive** registry-dispatched union of eleven variants,
/// four of which contain further `SlotDisplay`s and one of which
/// (`composite`) contains a list of them. There is no length prefix anywhere, so
/// there is no way to skip one without decoding it — which is why every consumer
/// of `RecipeDisplay` in this crate had to wait for this function, and why the
/// five recipe packets landed together.
///
/// `depth` bounds the recursion: a malicious or corrupt payload could otherwise
/// nest `composite` indefinitely and blow the stack. Vanilla's own nesting is two
/// or three deep in practice.
fn read_slot_display(reader: &mut Reader<'_>, depth: u32) -> Result<SlotDisplayItems, AdapterError> {
    // 16 is far above vanilla's own two-or-three and well below anything that
    // threatens the stack. Returning `incomplete` rather than erroring keeps a
    // hostile payload a dropped packet instead of a disconnect.
    if depth > 16 {
        return Ok(SlotDisplayItems::incomplete());
    }
    let kind = reader.var_i32().map_err(dec_err)?;
    let mut items = Vec::new();
    match kind {
        slot_display::EMPTY | slot_display::ANY_FUEL => {}
        slot_display::ITEM => {
            items.push(reader.var_i32().map_err(dec_err)?);
        }
        slot_display::ITEM_STACK => {
            // `ItemStackTemplate.STREAM_CODEC`: item id, count, then a
            // `DataComponentPatch` — which is exactly what `read_component_patch`
            // walks, including its bail-out on an unmodeled component type.
            let item_id = reader.var_i32().map_err(dec_err)?;
            let _count = reader.var_i32().map_err(dec_err)?;
            let name = item_name(item_id).unwrap_or("minecraft:air");
            let (_components, complete) = read_component_patch(reader, name)?;
            if !complete {
                return Ok(SlotDisplayItems::incomplete());
            }
            items.push(item_id);
        }
        slot_display::TAG => {
            // `TagKey.streamCodec` is one `Identifier` string. The tag's *members*
            // are not on the wire, so there is no item id to collect — a consumer
            // that needs one resolves the tag itself.
            let _tag = reader.string(32767).map_err(dec_err)?;
        }
        slot_display::WITH_ANY_POTION => {
            let inner = read_slot_display(reader, depth + 1)?;
            if !inner.complete {
                return Ok(SlotDisplayItems::incomplete());
            }
            items.extend(inner.items);
        }
        slot_display::ONLY_WITH_COMPONENT => {
            let inner = read_slot_display(reader, depth + 1)?;
            if !inner.complete {
                return Ok(SlotDisplayItems::incomplete());
            }
            // `DataComponentType.STREAM_CODEC` is a bare VarInt registry id.
            let _component_type = reader.var_i32().map_err(dec_err)?;
            items.extend(inner.items);
        }
        slot_display::DYED | slot_display::WITH_REMAINDER => {
            // Two `SlotDisplay`s. For `dyed` they are (dye, target); for
            // `with_remainder` (input, remainder). Both halves are walked because
            // both must be consumed — only the first carries the item a recipe
            // panel wants, but skipping the second is not an option (no length
            // prefix).
            let first = read_slot_display(reader, depth + 1)?;
            if !first.complete {
                return Ok(SlotDisplayItems::incomplete());
            }
            let second = read_slot_display(reader, depth + 1)?;
            if !second.complete {
                return Ok(SlotDisplayItems::incomplete());
            }
            items.extend(first.items);
        }
        slot_display::SMITHING_TRIM => {
            for _ in 0..3 {
                let inner = read_slot_display(reader, depth + 1)?;
                if !inner.complete {
                    return Ok(SlotDisplayItems::incomplete());
                }
                items.extend(inner.items);
            }
            // `TrimPattern.STREAM_CODEC` is `ByteBufCodecs.holder`: `0` means an
            // inline `TrimPattern` follows, which this adapter does not model, so
            // that case abandons the packet rather than guessing at its length.
            let holder = reader.var_i32().map_err(dec_err)?;
            if holder == 0 {
                return Ok(SlotDisplayItems::incomplete());
            }
        }
        slot_display::COMPOSITE => {
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).map_err(|_| {
                AdapterError::Decode(format!("invalid composite slot display count {count}"))
            })?;
            for _ in 0..count {
                let inner = read_slot_display(reader, depth + 1)?;
                if !inner.complete {
                    return Ok(SlotDisplayItems::incomplete());
                }
                items.extend(inner.items);
            }
        }
        // An id outside the built-in table means a modded registry entry whose
        // payload shape is unknown. The reader cannot go on.
        _ => return Ok(SlotDisplayItems::incomplete()),
    }
    Ok(SlotDisplayItems {
        items,
        complete: true,
    })
}

/// Walks one `RecipeDisplay` and returns the item ids of its **result** slot.
///
/// The result is what a recipe panel and a toast both key on; the ingredient
/// slots are walked only because they must be consumed. Returns `None` when the
/// walk hit something unmodeled, with the same "abandon the packet" contract as
/// [`read_slot_display`].
///
/// Variant ids are `RecipeDisplays.java`'s registration order: shapeless, shaped,
/// furnace, stonecutter, smithing.
fn read_recipe_display(reader: &mut Reader<'_>) -> Result<Option<Vec<i32>>, AdapterError> {
    let kind = reader.var_i32().map_err(dec_err)?;
    // Each variant is a fixed sequence of `SlotDisplay`s plus, for two of them,
    // some scalars. `result_index` is which of the walked displays is the result,
    // and `station_last` is true for every variant because `craftingStation` is
    // always the final `SlotDisplay`.
    let mut walked: Vec<Vec<i32>> = Vec::new();
    let walk = |reader: &mut Reader<'_>, walked: &mut Vec<Vec<i32>>| -> Result<bool, AdapterError> {
        let display = read_slot_display(reader, 0)?;
        if !display.complete {
            return Ok(false);
        }
        walked.push(display.items);
        Ok(true)
    };
    let result_index = match kind {
        // crafting_shapeless: list<SlotDisplay> ingredients, result, station.
        0 => {
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).map_err(|_| {
                AdapterError::Decode(format!("invalid shapeless ingredient count {count}"))
            })?;
            for _ in 0..count {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            let ingredients = walked.len();
            for _ in 0..2 {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            ingredients
        }
        // crafting_shaped: width, height, list<SlotDisplay>, result, station.
        1 => {
            let _width = reader.var_i32().map_err(dec_err)?;
            let _height = reader.var_i32().map_err(dec_err)?;
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).map_err(|_| {
                AdapterError::Decode(format!("invalid shaped ingredient count {count}"))
            })?;
            for _ in 0..count {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            let ingredients = walked.len();
            for _ in 0..2 {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            ingredients
        }
        // furnace: ingredient, fuel, result, station, duration, experience.
        2 => {
            for _ in 0..4 {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            let _duration = reader.var_i32().map_err(dec_err)?;
            let _experience = reader.f32().map_err(dec_err)?;
            2
        }
        // stonecutter: input, result, station.
        3 => {
            for _ in 0..3 {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            1
        }
        // smithing: template, base, addition, result, station.
        4 => {
            for _ in 0..5 {
                if !walk(reader, &mut walked)? {
                    return Ok(None);
                }
            }
            3
        }
        _ => return Ok(None),
    };
    Ok(walked.get(result_index).cloned().or(Some(Vec::new())))
}

/// Reads a `DebugSubscription.Update`'s dispatch head: the subscription's
/// registry id resolved to its identifier, then `ByteBufCodecs.optional`'s
/// present-flag, then the rest of the payload as opaque bytes.
///
/// The payload is opaque because the value codec is chosen per registry entry and
/// the seventeen registered ones share no shape — one (`dedicated_server_tick_time`)
/// has a `null` value codec and throws if it is ever sent this way. See
/// `lodestone_game::debug_feeds`' module doc.
fn read_debug_update(
    reader: &mut Reader<'_>,
) -> Result<(ResourceKey, Option<Vec<u8>>), AdapterError> {
    let subscription = read_debug_subscription_key(reader)?;
    let present = reader.bool().map_err(dec_err)?;
    let value = if present {
        Some(reader.remaining_bytes().to_vec())
    } else {
        None
    };
    Ok((subscription, value))
}

/// Reads a `minecraft:debug_subscription` registry id and resolves it.
///
/// An unknown id is a decode **error** rather than a synthetic key: the id is the
/// dispatch discriminant, so not knowing it means the bytes after it cannot be
/// attributed, and inventing `lodestone:unknown_7` would let two different feeds
/// collide in the store.
fn read_debug_subscription_key(reader: &mut Reader<'_>) -> Result<ResourceKey, AdapterError> {
    let id = reader.var_i32().map_err(dec_err)?;
    let name = crate::stat_debug_registries::debug_subscription_name(id).ok_or_else(|| {
        AdapterError::Decode(format!("unknown debug_subscription registry id {id}"))
    })?;
    parse_key(name, "debug subscription")
}

/// Decodes `ClientboundAwardStatsPacket`: a VarInt-counted map of
/// `(stat_type id, value id) -> count`.
///
/// `Stat.STREAM_CODEC` is `registry(STAT_TYPE).dispatch(Stat::getType,
/// StatType::streamCodec)`, so the **second** id's registry depends on the first:
/// a value under `minecraft:mined` is a block, under `minecraft:killed` an entity
/// type, and under `minecraft:custom` one of the 77 custom stats. Resolving it
/// with one fixed table would silently mislabel every category but one.
///
/// An id this build cannot resolve yields `value: None` rather than an error — the
/// count is still correct and vanilla's own General tab is entirely
/// `minecraft:custom`, which we always resolve.
fn decode_award_stats(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    use crate::stat_debug_registries::{
        StatValueRegistry, custom_stat_name, stat_type_name, stat_value_registry,
    };

    let mut reader = Reader::new(payload);
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid award_stats count {count}")))?;
    let mut stats = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let type_id = reader.var_i32().map_err(dec_err)?;
        let value_id = reader.var_i32().map_err(dec_err)?;
        let stat_count = reader.var_i32().map_err(dec_err)?;
        let type_name = stat_type_name(type_id).ok_or_else(|| {
            AdapterError::Decode(format!("unknown stat_type registry id {type_id}"))
        })?;
        let value_name = match stat_value_registry(type_id) {
            Some(StatValueRegistry::CustomStat) => custom_stat_name(value_id),
            Some(StatValueRegistry::Item) => item_name(value_id),
            Some(StatValueRegistry::EntityType) => entity_type_name(value_id),
            // `block_type_name` indexes the `minecraft:block` *registry* (one id
            // per block type, registration order), which is what a `minecraft:mined`
            // stat value is — not a palette state id. `block_name` would be the
            // wrong table here and would resolve every id to an unrelated block.
            Some(StatValueRegistry::Block) => {
                u32::try_from(value_id).ok().and_then(block_type_name)
            }
            None => None,
        };
        stats.push(StatAward {
            stat_type: parse_key(type_name, "stat type")?,
            value: match value_name {
                Some(name) => Some(parse_key(name, "stat value")?),
                None => None,
            },
            count: stat_count,
        });
    }
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::StatisticsAwarded {
        stats,
    })])
}

/// Consumes a `HolderSet<Item>` (`Ingredient.CONTENTS_STREAM_CODEC`) and returns
/// the explicit item ids, or an empty list for the tag form.
///
/// Same wire shape as [`read_block_holder_set`], one registry over: a VarInt where
/// `0` means a tag identifier follows and `n` means `n - 1` explicit ids.
fn read_item_holder_set(reader: &mut Reader<'_>) -> Result<Vec<i32>, AdapterError> {
    let discriminator = reader.var_i32().map_err(dec_err)?;
    if discriminator == 0 {
        let _tag = reader.string(32767).map_err(dec_err)?;
        return Ok(Vec::new());
    }
    let count = usize::try_from(discriminator - 1)
        .map_err(|_| AdapterError::Decode(format!("invalid item set size {discriminator}")))?;
    let mut items = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        items.push(reader.var_i32().map_err(dec_err)?);
    }
    Ok(items)
}

/// Decodes `ClientboundRecipeBookAddPacket`.
///
/// **The trailing `replace: bool` sits after the entry list**, so the list cannot
/// be taken as opaque trailing bytes — the whole reason this packet waited for
/// [`read_slot_display`]. Each entry is a `RecipeDisplayEntry` then an `i8` flags
/// byte (bit 0 notification, bit 1 highlight).
///
/// `RecipeDisplayEntry`'s `group` field is `ByteBufCodecs.OPTIONAL_VAR_INT`: a
/// single VarInt where `0` is absent and a present value `v` is written `v + 1` —
/// **not** the usual bool-then-value optional. A bool-prefixed reader would
/// mis-frame every entry after the first.
fn decode_recipe_book_add(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid recipe_book_add count {count}")))?;
    let mut entries = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let display_id = reader.var_i32().map_err(dec_err)?;
        let Some(result_items) = read_recipe_display(&mut reader)? else {
            return Ok(Vec::new());
        };
        // `OPTIONAL_VAR_INT`, not a bool-prefixed optional.
        let _group = reader.var_i32().map_err(dec_err)?;
        let _category = reader.var_i32().map_err(dec_err)?;
        if reader.bool().map_err(dec_err)? {
            let requirement_count = reader.var_i32().map_err(dec_err)?;
            let requirement_count = usize::try_from(requirement_count).map_err(|_| {
                AdapterError::Decode(format!(
                    "invalid crafting requirement count {requirement_count}"
                ))
            })?;
            for _ in 0..requirement_count {
                let _ingredient = read_item_holder_set(&mut reader)?;
            }
        }
        let flags = reader.i8().map_err(dec_err)?;
        entries.push(RecipeBookEntry {
            display_id,
            result_items,
            notification: flags & 0x01 != 0,
            highlight: flags & 0x02 != 0,
        });
    }
    let replace = reader.bool().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::RecipeBookAdded {
        entries,
        replace,
    })])
}

/// Decodes `ClientboundUpdateRecipesPacket`: the property sets, then the
/// stonecutter list.
///
/// Despite the name this is **not** the recipe corpus — it is the per-slot "which
/// items are valid here" sets vanilla's screens grey out against, plus the
/// stonecutter's own input→result pairs. A `RecipePropertySet` is a VarInt-counted
/// list of item registry ids and needs no display walk; the stonecutter half does.
fn decode_update_recipes(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let set_count = reader.var_i32().map_err(dec_err)?;
    let set_count = usize::try_from(set_count)
        .map_err(|_| AdapterError::Decode(format!("invalid property set count {set_count}")))?;
    let mut item_sets = Vec::with_capacity(set_count.min(256));
    for _ in 0..set_count {
        let key = reader.string(32767).map_err(dec_err)?;
        let item_count = reader.var_i32().map_err(dec_err)?;
        let item_count = usize::try_from(item_count)
            .map_err(|_| AdapterError::Decode(format!("invalid property item count {item_count}")))?;
        let mut items = Vec::with_capacity(item_count.min(4096));
        for _ in 0..item_count {
            items.push(reader.var_i32().map_err(dec_err)?);
        }
        item_sets.push((parse_key(&key, "recipe property set")?, items));
    }
    let stonecutter_count = reader.var_i32().map_err(dec_err)?;
    let stonecutter_count = usize::try_from(stonecutter_count).map_err(|_| {
        AdapterError::Decode(format!("invalid stonecutter count {stonecutter_count}"))
    })?;
    let mut stonecutter_results = Vec::with_capacity(stonecutter_count.min(4096));
    for _ in 0..stonecutter_count {
        // `SingleInputEntry`: an `Ingredient` (HolderSet<Item>) then a
        // `SlotDisplay` — a bare display, not a whole `RecipeDisplay`.
        let _input = read_item_holder_set(&mut reader)?;
        let display = read_slot_display(&mut reader, 0)?;
        if !display.complete {
            // Emit what was decoded before the unmodeled entry rather than the
            // whole packet: the property sets above are complete and independently
            // useful, and they are the half a screen actually reads.
            return Ok(vec![Directive::Emit(
                ClientEvent::RecipePropertySetsUpdated {
                    item_sets,
                    stonecutter_results,
                },
            )]);
        }
        stonecutter_results.push(display.items);
    }
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(
        ClientEvent::RecipePropertySetsUpdated {
            item_sets,
            stonecutter_results,
        },
    )])
}

/// Decodes `ClientboundMerchantOffersPacket`.
///
/// # The two traps
///
/// **Five of `MerchantOffer`'s fields are big-endian `i32`s, not VarInts** —
/// `uses`, `maxUses`, `xp`, `specialPriceDiff` and `demand` are all `writeInt`.
/// Almost every other integer in this protocol is a VarInt, so a
/// VarInt-by-default reader gets all five wrong *and* desynchronises everything
/// after them.
///
/// **The trailing scalars come after the offer list.** `villagerLevel`,
/// `villagerXp`, `showProgress` and `canRestock` are all past the offers, so they
/// are unreachable without parsing every `MerchantOffer` — including each
/// `ItemCost`'s `DataComponentExactPredicate`, which is a VarInt-counted list of
/// typed components. That list is `EMPTY` for every vanilla trade; a non-empty one
/// is unmodeled here and abandons the packet rather than guessing at its length.
fn decode_merchant_offers(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let window_id = reader.var_i32().map_err(dec_err)?;
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid merchant offer count {count}")))?;
    let mut offers = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let Some(cost_a) = read_item_cost(&mut reader)? else {
            return Ok(Vec::new());
        };
        // **This was the bug.** It read `.stack` off the old struct and dropped
        // the completeness flag, so an offer whose result carried an unmodeled
        // component left the reader parked mid-payload and the loop went on to
        // read this offer's remaining eight fields — and then the *next* offer —
        // out of that component's interior. On a plugin server stamping
        // `minecraft:custom_data` on every trade result, that is one warning per
        // offer followed by an over-read blamed on framing. An offer list has no
        // per-entry length prefix and the trailing `villagerLevel`/`villagerXp`
        // scalars sit past it, so there is nothing to resynchronise to: the only
        // correct move is to abandon the packet, exactly as a non-empty
        // `DataComponentExactPredicate` does two lines up.
        let result = match read_item_stack(&mut reader)? {
            DecodedStack::Complete(stack) => stack,
            DecodedStack::Partial(_) => return Ok(Vec::new()),
        };
        let cost_b = if reader.bool().map_err(dec_err)? {
            match read_item_cost(&mut reader)? {
                Some(cost) => Some(cost),
                None => return Ok(Vec::new()),
            }
        } else {
            None
        };
        let out_of_stock = reader.bool().map_err(dec_err)?;
        // The five `writeInt` fields. Not VarInts.
        let uses = reader.i32().map_err(dec_err)?;
        let max_uses = reader.i32().map_err(dec_err)?;
        let xp = reader.i32().map_err(dec_err)?;
        let special_price_diff = reader.i32().map_err(dec_err)?;
        let price_multiplier = reader.f32().map_err(dec_err)?;
        let demand = reader.i32().map_err(dec_err)?;
        offers.push(ModelMerchantOffer {
            cost_a,
            cost_b,
            result,
            out_of_stock,
            uses,
            max_uses,
            xp,
            special_price_diff,
            price_multiplier,
            demand,
        });
    }
    let villager_level = reader.var_i32().map_err(dec_err)?;
    let villager_xp = reader.var_i32().map_err(dec_err)?;
    let show_progress = reader.bool().map_err(dec_err)?;
    let can_restock = reader.bool().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::MerchantOffersReceived {
        window_id,
        offers,
        villager_level,
        villager_xp,
        show_progress,
        can_restock,
    })])
}

/// Reads one `ItemCost`: item registry id, count, then a
/// `DataComponentExactPredicate`.
///
/// Returns `None` when the predicate is non-empty, which this adapter does not
/// model — see [`decode_merchant_offers`]'s doc. `EMPTY` (a zero count) is what
/// every vanilla trade sends.
fn read_item_cost(reader: &mut Reader<'_>) -> Result<Option<(i32, i32)>, AdapterError> {
    let item_id = reader.var_i32().map_err(dec_err)?;
    let count = reader.var_i32().map_err(dec_err)?;
    let predicate_count = reader.var_i32().map_err(dec_err)?;
    if predicate_count != 0 {
        return Ok(None);
    }
    Ok(Some((item_id, count)))
}

/// Decodes `ClientboundTrackedWaypointPacket` and its hand-written
/// `TrackedWaypoint.write`.
///
/// The position is a four-way tagged union, not an optional: `EMPTY` carries
/// nothing, `VEC3I` three VarInts, `CHUNK` two, and `AZIMUTH` one f32 bearing.
/// Vanilla degrades to the coarser forms with distance, so a decoder that treated
/// anything but `VEC3I` as "no position" would blank the locator bar at range.
fn decode_waypoint(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let operation = match reader.var_i32().map_err(dec_err)? {
        0 => WaypointOperation::Track,
        1 => WaypointOperation::Untrack,
        2 => WaypointOperation::Update,
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown waypoint operation {other}"
            )));
        }
    };
    let id = if reader.bool().map_err(dec_err)? {
        WaypointId::Entity(reader.uuid().map_err(dec_err)?)
    } else {
        WaypointId::Named(reader.string(32767).map_err(dec_err)?)
    };
    let style = parse_key(&reader.string(32767).map_err(dec_err)?, "waypoint style")?;
    let color = if reader.bool().map_err(dec_err)? {
        // `ByteBufCodecs.RGB_COLOR` is a plain big-endian int.
        #[allow(clippy::cast_sign_loss)]
        Some(reader.i32().map_err(dec_err)? as u32)
    } else {
        None
    };
    let position = match reader.var_i32().map_err(dec_err)? {
        0 => WaypointPosition::Empty,
        1 => WaypointPosition::Exact(BlockPos {
            x: reader.var_i32().map_err(dec_err)?,
            y: reader.var_i32().map_err(dec_err)?,
            z: reader.var_i32().map_err(dec_err)?,
        }),
        2 => WaypointPosition::Chunk(ChunkPos {
            x: reader.var_i32().map_err(dec_err)?,
            z: reader.var_i32().map_err(dec_err)?,
        }),
        3 => WaypointPosition::Azimuth(reader.f32().map_err(dec_err)?),
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown waypoint position type {other}"
            )));
        }
    };
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::WaypointUpdated {
        operation,
        waypoint: TrackedWaypoint {
            id,
            style,
            color,
            position,
        },
    })])
}

/// Decodes `ClientboundShowDialogPacket`'s Play-state form.
///
/// The field is `ByteBufCodecs.holder(Registries.DIALOG, …)`: a VarInt where `0`
/// means "an inline value follows" and `n > 0` means registry id `n - 1` with no
/// further bytes. **The off-by-one is the trap** — reading the raw VarInt as the
/// id would reference the wrong dialog for every entry.
///
/// The inline form is a `Dialog`, which is an NBT `Codec` union of six types with
/// nested body/input/action trees — a schema, not a `StreamCodec` — so it is
/// carried as raw network-NBT bytes for a renderer to parse.
fn decode_show_dialog(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let holder = reader.var_i32().map_err(dec_err)?;
    let (registry_id, inline) = if holder == 0 {
        (None, Some(reader.remaining_bytes().to_vec()))
    } else {
        (Some(holder - 1), None)
    };
    Ok(vec![Directive::Emit(ClientEvent::DialogShown {
        registry_id,
        inline,
    })])
}

/// `minecraft:map_decoration_type` registry paths by numeric id, from
/// `.cache/mc/26.2/generated/reports/registries.json`.
///
/// A **built-in** registry, so the ids are fixed by the jar rather than synced
/// during Configuration (`MapDecorationType.STREAM_CODEC` is
/// `ByteBufCodecs.holderRegistry`, a bare VarInt registry id). That is why a
/// table is correct here where it would be a guess for a dynamic registry — see
/// [`TRIM_MATERIAL_IDS`] for the contrast.
const MAP_DECORATION_TYPE_IDS: &[&str] = &[
    "player",
    "frame",
    "red_marker",
    "blue_marker",
    "target_x",
    "target_point",
    "player_off_map",
    "player_off_limits",
    "mansion",
    "monument",
    "banner_white",
    "banner_orange",
    "banner_magenta",
    "banner_light_blue",
    "banner_yellow",
    "banner_lime",
    "banner_pink",
    "banner_gray",
    "banner_light_gray",
    "banner_cyan",
    "banner_purple",
    "banner_blue",
    "banner_brown",
    "banner_green",
    "banner_red",
    "banner_black",
    "red_x",
    "village_desert",
    "village_plains",
    "village_savanna",
    "village_snowy",
    "village_taiga",
    "jungle_temple",
    "swamp_hut",
    "trial_chambers",
];

/// Decodes `ClientboundMapItemDataPacket` (id 51).
///
/// Wire shape, from the record's own `STREAM_CODEC`: a VarInt `MapId`, a `byte`
/// scale, a `bool` locked, `Optional<List<MapDecoration>>`, then
/// `MapPatch.STREAM_CODEC`'s optional.
///
/// Two traps in the patch codec, both from `MapItemSavedData.MapPatch.read`:
///
/// * the field order on the wire is **width, height, startX, startY** — *not*
///   the record's declaration order (`startX, startY, width, height`); and
/// * the optional has **no boolean tag**. A `width` of zero *is* the absent
///   case, so the four position bytes and the colour array are only present when
///   the first byte is non-zero. Reading a leading `bool` here consumes the width.
fn decode_map_item_data(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let map_id = reader.var_i32().map_err(dec_err)?;
    let scale = reader.i8().map_err(dec_err)?;
    let locked = reader.bool().map_err(dec_err)?;
    let decorations = if reader.bool().map_err(dec_err)? {
        let count = reader.var_i32().map_err(dec_err)?;
        let count = usize::try_from(count)
            .map_err(|_| AdapterError::Decode(format!("invalid map decoration count {count}")))?;
        let mut list = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let type_id = reader.var_i32().map_err(dec_err)?;
            let path = usize::try_from(type_id)
                .ok()
                .and_then(|index| MAP_DECORATION_TYPE_IDS.get(index))
                .ok_or_else(|| {
                    AdapterError::Decode(format!("unknown map decoration type id {type_id}"))
                })?;
            let x = reader.i8().map_err(dec_err)?;
            let y = reader.i8().map_err(dec_err)?;
            let rot = reader.i8().map_err(dec_err)?;
            let name = if reader.bool().map_err(dec_err)? {
                Some(Text::from_nbt(&read_network_nbt(&mut reader).map_err(dec_err)?))
            } else {
                None
            };
            list.push(MapDecoration {
                kind: parse_key(path, "map decoration type")?,
                x,
                y,
                // Vanilla's own record constructor masks this, so the client
                // never sees a rotation outside 0..=15.
                #[allow(clippy::cast_sign_loss)]
                rotation: (rot as u8) & 15,
                name,
            });
        }
        Some(list)
    } else {
        None
    };
    let width = reader.u8().map_err(dec_err)?;
    let color_patch = if width == 0 {
        None
    } else {
        let height = reader.u8().map_err(dec_err)?;
        let start_x = reader.u8().map_err(dec_err)?;
        let start_y = reader.u8().map_err(dec_err)?;
        let colors = reader.var_bytes(1 << 16).map_err(dec_err)?.to_vec();
        let expected = usize::from(width) * usize::from(height);
        if colors.len() != expected {
            return Err(AdapterError::Decode(format!(
                "map patch {width}x{height} carries {} colour bytes, expected {expected}",
                colors.len()
            )));
        }
        Some(MapPatch {
            start_x,
            start_y,
            width,
            height,
            colors,
        })
    };
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::MapItemData {
        map_id,
        scale,
        locked,
        decorations,
        color_patch,
    })])
}

/// Reads one `ItemStackTemplate` (`ItemStackTemplate.STREAM_CODEC`).
///
/// **Not** the same shape as an `ItemStack`: the template writes the item holder
/// *first* and the count second, where `ItemStack.OPTIONAL_STREAM_CODEC` leads
/// with the count and uses `<= 0` as the empty sentinel. A template is never
/// empty (its constructor rejects air and count 0), so there is no sentinel and
/// no `Option`.
fn read_item_stack_template(reader: &mut Reader<'_>) -> Result<ItemStack, AdapterError> {
    let item_id = reader.var_i32().map_err(dec_err)?;
    let name = item_name(item_id)
        .ok_or_else(|| AdapterError::Decode(format!("unknown item registry id {item_id}")))?;
    let count = reader.var_i32().map_err(dec_err)?;
    let count = u32::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid item count {count}")))?;
    let (components, complete) = read_component_patch(reader, name)?;
    if !complete {
        return Err(AdapterError::Decode(format!(
            "advancement icon {name} carries an unmodeled item component, so the rest of the packet is unreadable"
        )));
    }
    Ok(ItemStack {
        item: parse_key(name, "item")?,
        count,
        components,
    })
}

/// Decodes `ClientboundUpdateAdvancementsPacket` (id 130).
///
/// Wire shape, from the packet's own reader: a `bool` reset, a list of
/// `AdvancementHolder`, a collection of removed identifiers, a map of
/// identifier → `AdvancementProgress`, then a `bool` showAdvancements.
///
/// `DisplayInfo`'s field order is **the wire's, not the datapack schema's**, and
/// the two differ (a vendored `minecraft-data` 1.21.9 schema disagrees with 26.2
/// here): `serializeToNetwork` writes title, description, icon, frame ordinal,
/// then a **raw big-endian `int`** flag word (`writeInt`, not a byte), then the
/// background identifier only when bit 0 is set, then x and y as floats.
/// `announceChat` is not on the wire at all — vanilla's reader hardcodes
/// `false` — so bit 1 is `showToast` and bit 2 is `hidden` with nothing between.
fn decode_update_advancements(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let reset = reader.bool().map_err(dec_err)?;

    let added_count = read_count(&mut reader, "advancement")?;
    let mut added = Vec::with_capacity(added_count.min(4096));
    for _ in 0..added_count {
        let id = reader.string(32767).map_err(dec_err)?;
        let id = parse_key(&id, "advancement")?;
        let parent = if reader.bool().map_err(dec_err)? {
            let parent = reader.string(32767).map_err(dec_err)?;
            Some(parse_key(&parent, "advancement parent")?)
        } else {
            None
        };
        let display = if reader.bool().map_err(dec_err)? {
            let title = Text::from_nbt(&read_network_nbt(&mut reader).map_err(dec_err)?);
            let description = Text::from_nbt(&read_network_nbt(&mut reader).map_err(dec_err)?);
            let icon = read_item_stack_template(&mut reader)?;
            let ordinal = reader.var_i32().map_err(dec_err)?;
            let frame = AdvancementFrame::from_ordinal(ordinal).ok_or_else(|| {
                AdapterError::Decode(format!("unknown advancement frame ordinal {ordinal}"))
            })?;
            let flags = reader.i32().map_err(dec_err)?;
            let background = if flags & 1 != 0 {
                let texture = reader.string(32767).map_err(dec_err)?;
                Some(parse_key(&texture, "advancement background")?)
            } else {
                None
            };
            let x = reader.f32().map_err(dec_err)?;
            let y = reader.f32().map_err(dec_err)?;
            Some(AdvancementDisplay {
                title,
                description,
                icon,
                frame,
                background,
                show_toast: flags & 2 != 0,
                hidden: flags & 4 != 0,
                x,
                y,
            })
        } else {
            None
        };
        let group_count = read_count(&mut reader, "requirement group")?;
        let mut requirements = Vec::with_capacity(group_count.min(4096));
        for _ in 0..group_count {
            let names = read_count(&mut reader, "requirement")?;
            let mut group = Vec::with_capacity(names.min(4096));
            for _ in 0..names {
                group.push(reader.string(32767).map_err(dec_err)?);
            }
            requirements.push(group);
        }
        let sends_telemetry_event = reader.bool().map_err(dec_err)?;
        added.push(AdvancementEntry {
            id,
            parent,
            display,
            requirements,
            sends_telemetry_event,
        });
    }

    let removed_count = read_count(&mut reader, "removed advancement")?;
    let mut removed = Vec::with_capacity(removed_count.min(4096));
    for _ in 0..removed_count {
        let id = reader.string(32767).map_err(dec_err)?;
        removed.push(parse_key(&id, "removed advancement")?);
    }

    let progress_count = read_count(&mut reader, "advancement progress")?;
    let mut progress = Vec::with_capacity(progress_count.min(4096));
    for _ in 0..progress_count {
        let id = reader.string(32767).map_err(dec_err)?;
        let id = parse_key(&id, "advancement progress")?;
        let criteria_count = read_count(&mut reader, "criterion")?;
        let mut criteria = Vec::with_capacity(criteria_count.min(4096));
        for _ in 0..criteria_count {
            let name = reader.string(32767).map_err(dec_err)?;
            // `CriterionProgress` is a nullable `Instant`: a presence bool then,
            // if set, epoch millis as a big-endian long (`writeInstant`).
            let obtained = if reader.bool().map_err(dec_err)? {
                Some(reader.i64().map_err(dec_err)?)
            } else {
                None
            };
            criteria.push((name, obtained));
        }
        progress.push((id, criteria));
    }

    let show_advancements = reader.bool().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::AdvancementsUpdated {
        reset,
        added,
        removed,
        progress,
        show_advancements,
    })])
}

/// A VarInt collection length, rejected rather than truncated when negative.
fn read_count(reader: &mut Reader<'_>, what: &str) -> Result<usize, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    usize::try_from(count).map_err(|_| AdapterError::Decode(format!("invalid {what} count {count}")))
}

/// Dispatches a clientbound Play-state packet by trying each already-split
/// domain first, then falling back to the packets not yet split out of this
/// file. Exactly one branch ever recognises a given `packet_id` (the wire ids
/// are disjoint by construction).
impl V770Adapter {
    fn handle_play(
        &self,
        world: &mut dyn WorldSink,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut directives = self.handle_play_connection(packet_id, payload)?;
        directives.extend(self.handle_play_chat(packet_id, payload)?);
        directives.extend(self.handle_play_scoreboard(packet_id, payload)?);
        if !directives.is_empty() {
            return Ok(directives);
        }
        if packet_id == play::clientbound::LOGIN {
            let body: GameLogin = decode_body(payload)?;
            // `dimension_type` is the registry holder id; `dimension` is the
            // level name. The id wins where the registry resolved it, and
            // `enter_dimension` falls back to the name match where it did not.
            let dimension_type = self.enter_dimension(body.dimension_type, &body.dimension);
            let dimension = body.dimension.parse().map_err(|_| {
                AdapterError::Decode(format!("invalid dimension {}", body.dimension))
            })?;
            // The biome registry's sky colours, indexed by holder id — the
            // integer a chunk section's biome palette stores (issue #96).
            // Emitted here rather than off `registry_data` itself for the same
            // reason `DimensionTypeChanged` is: `Login` is the point at which
            // the Configuration set is known complete, and re-entering
            // Configuration resends the registries and is followed by a fresh
            // `Login`, so this can never be stale.
            let biome_sky_colors = self
                .registries
                .lock()
                .ok()
                .map(|registries| registries.biome_sky_colors().to_vec())
                .unwrap_or_default();
            // The same registry generation's climate table (issue #25/#26's
            // shared biome lane), emitted at the same point and for the same
            // reason as `biome_sky_colors` just above — see `BiomeClimates`'s
            // own doc for why this is a second variant rather than two more
            // fields on `BiomeVisuals`.
            let (biome_temperatures, biome_downfall, biome_has_precipitation) = self
                .registries
                .lock()
                .ok()
                .map(|registries| {
                    let climates = registries.biome_climates();
                    (
                        climates.iter().map(|c| c.map(|c| c.temperature)).collect(),
                        climates.iter().map(|c| c.map(|c| c.downfall)).collect(),
                        climates
                            .iter()
                            .map(|c| c.map(|c| c.has_precipitation))
                            .collect(),
                    )
                })
                .unwrap_or_default();
            // The same registry generation's entry *names*, indexed by holder
            // id exactly like the two tables above (follow-up to issue #96 /
            // `eb423ac`) — see `ClientEvent::BiomeRegistryNames`'s own doc for
            // why the mesher's `FALLBACK_BIOME_NAMES` fallback is otherwise
            // wrong against a third-party server. `entry_names` already
            // decodes this correctly (it has since #288); nothing before this
            // change carried it past this crate.
            let biome_names = self
                .registries
                .lock()
                .ok()
                .and_then(|registries| {
                    registries
                        .entry_names(ClientRegistries::BIOME)
                        .map(<[String]>::to_vec)
                })
                .unwrap_or_default();
            // The same story one registry over, and the same fix. The
            // `minecraft:enchantment` order was **already decoded** by
            // `entry_names` and never handed past this crate, so
            // `Sim::riptide_level` resolved `minecraft:riptide` through a
            // hardcoded holder id of 32 — `riptide` being the 33rd of 26.2's 43
            // built-in enchantments in resource-location-sorted order. Right
            // against vanilla, silently wrong against any data pack that reorders,
            // because the id stays valid and still names *an* enchantment.
            let enchantment_names = self
                .registries
                .lock()
                .ok()
                .and_then(|registries| {
                    registries
                        .entry_names("minecraft:enchantment")
                        .map(<[String]>::to_vec)
                })
                .unwrap_or_default();
            return Ok(vec![
                // Before `Login`, deliberately: a consumer folding both sees the
                // dimension's geometry before the level name that depends on it.
                Directive::Emit(ClientEvent::DimensionTypeChanged {
                    holder_id: body.dimension_type,
                    dimension_type,
                }),
                Directive::Emit(ClientEvent::BiomeVisuals {
                    sky_colors: biome_sky_colors,
                }),
                Directive::Emit(ClientEvent::BiomeClimates {
                    temperatures: biome_temperatures,
                    downfall: biome_downfall,
                    has_precipitation: biome_has_precipitation,
                }),
                Directive::Emit(ClientEvent::BiomeRegistryNames { names: biome_names }),
                Directive::Emit(ClientEvent::EnchantmentRegistryNames {
                    names: enchantment_names,
                }),
                Directive::Emit(ClientEvent::Login {
                    entity_id: body.entity_id,
                    game_mode: game_mode(body.game_type)?,
                    dimension,
                }),
            ]);
        }
        if packet_id == play::clientbound::CHUNK_BATCH_START {
            // Empty packet; it only marks the start of a batch for rate timing.
            Reader::new(payload).ensure_empty().map_err(dec_err)?;
            self.begin_chunk_batch();
            return Ok(vec![]);
        }
        if packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
            // Acknowledge the batch — the server halts chunk delivery after ten
            // unacknowledged batches — reporting the estimated desired rate.
            let body: ChunkBatchFinished = decode_body(payload)?;
            let desired_chunks_per_tick = self.finish_chunk_batch(body.batch_size);
            return Ok(vec![send(
                play::serverbound::CHUNK_BATCH_RECEIVED,
                &ChunkBatchReceived {
                    desired_chunks_per_tick,
                },
            )?]);
        }
        if packet_id == play::clientbound::CONTAINER_SET_CONTENT {
            let mut reader = Reader::new(payload);
            let window_id = reader.var_i32().map_err(dec_err)?;
            let state_id = reader.var_i32().map_err(dec_err)?;
            let len = reader.var_i32().map_err(dec_err)?;
            let len = usize::try_from(len)
                .map_err(|_| AdapterError::Decode(format!("invalid item count {len}")))?;
            let mut items = Vec::with_capacity(len);
            let mut complete = true;
            for _ in 0..len {
                match read_item_stack(&mut reader)? {
                    DecodedStack::Complete(stack) => items.push(stack),
                    // An unmodeled component ended the patch; the remaining list
                    // entries and the carried item are unreadable. Deliver what
                    // decoded and drop the rest of the packet.
                    DecodedStack::Partial(stack) => {
                        items.push(stack);
                        complete = false;
                        break;
                    }
                }
            }
            let carried_item = if complete {
                match read_item_stack(&mut reader)? {
                    DecodedStack::Complete(stack) => stack,
                    DecodedStack::Partial(stack) => {
                        complete = false;
                        stack
                    }
                }
            } else {
                None
            };
            if complete {
                reader.ensure_empty().map_err(dec_err)?;
            }
            return Ok(vec![Directive::Emit(ClientEvent::ContainerContent {
                window_id,
                state_id,
                items,
                carried_item,
            })]);
        }
        if packet_id == play::clientbound::CONTAINER_SET_SLOT {
            let mut reader = Reader::new(payload);
            let window_id = reader.var_i32().map_err(dec_err)?;
            let state_id = reader.var_i32().map_err(dec_err)?;
            let slot = i32::from(reader.i16().map_err(dec_err)?);
            let item = read_trailing_item_stack(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::ContainerSlot {
                window_id,
                state_id,
                slot,
                item,
            })]);
        }
        if packet_id == play::clientbound::CONTAINER_SET_DATA {
            let mut reader = Reader::new(payload);
            let window_id = reader.var_i32().map_err(dec_err)?;
            let property = i32::from(reader.i16().map_err(dec_err)?);
            let value = i32::from(reader.i16().map_err(dec_err)?);
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ContainerData {
                window_id,
                property,
                value,
            })]);
        }
        if packet_id == play::clientbound::CONTAINER_CLOSE {
            let mut reader = Reader::new(payload);
            let window_id = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ScreenClosed {
                window_id,
            })]);
        }
        if packet_id == play::clientbound::SET_EQUIPMENT {
            // An entity id, then a continuation-flagged list: each entry is a
            // slot byte whose low 7 bits are the `EquipmentSlot` ordinal and
            // whose high bit signals another entry follows, then an item stack.
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let mut equipment = Vec::new();
            let mut complete = true;
            loop {
                let slot_byte = reader.u8().map_err(dec_err)?;
                let ordinal = slot_byte & 0x7F;
                let slot = EquipmentSlot::from_ordinal(ordinal).ok_or_else(|| {
                    AdapterError::Decode(format!("unknown equipment slot ordinal {ordinal}"))
                })?;
                let decoded = read_item_stack(&mut reader)?;
                let (item, partial) = match decoded {
                    DecodedStack::Complete(stack) => (stack, false),
                    DecodedStack::Partial(stack) => (stack, true),
                };
                equipment.push(EntityEquipment { slot, item });
                if partial {
                    // An unmodeled component ended the patch; further list
                    // entries are unreadable. Deliver what decoded and stop.
                    complete = false;
                    break;
                }
                if slot_byte & 0x80 == 0 {
                    break;
                }
            }
            if complete {
                reader.ensure_empty().map_err(dec_err)?;
            }
            return Ok(vec![Directive::Emit(ClientEvent::EntityEquipmentUpdated {
                entity_id,
                equipment,
            })]);
        }
        if packet_id == play::clientbound::SET_HEALTH {
            let body: SetHealth = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::HealthChanged {
                health: body.health,
                food: body.food,
                saturation: body.saturation,
            })]);
        }
        if packet_id == play::clientbound::RECIPE_BOOK_SETTINGS {
            // `RecipeBookSettings.STREAM_CODEC` composes four `TypeSettings`, each
            // two booleans, in the fixed order crafting, furnace, blast furnace,
            // smoker. Eight bytes, no length prefix and no discriminator — the
            // codec is `StreamCodec<FriendlyByteBuf, _>`, i.e. not registry-aware,
            // which is the structural proof that nothing else is on the wire.
            //
            // Field order within a pair is `open` then `filtering`. Getting that
            // pair backwards is the available mistake here and it is invisible to a
            // round-trip test, so `recipe_book_settings_wire_order_is_open_then_filtering`
            // pins it against a hand-built asymmetric byte pattern.
            let mut reader = Reader::new(payload);
            let mut settings = [RecipeBookTypeSettings::default(); 4];
            for slot in &mut settings {
                slot.open = reader.bool().map_err(dec_err)?;
                slot.filtering = reader.bool().map_err(dec_err)?;
            }
            reader.ensure_empty().map_err(dec_err)?;
            let [crafting, furnace, blast_furnace, smoker] = settings;
            return Ok(vec![Directive::Emit(
                ClientEvent::RecipeBookSettingsChanged {
                    crafting,
                    furnace,
                    blast_furnace,
                    smoker,
                },
            )]);
        }
        if packet_id == play::clientbound::SET_HELD_SLOT {
            let mut reader = Reader::new(payload);
            let slot = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::HeldSlotChanged { slot })]);
        }
        if packet_id == play::clientbound::SET_EXPERIENCE {
            // Field order on the wire is progress (float), level, then total —
            // not alphabetical/declaration order.
            let mut reader = Reader::new(payload);
            let progress = reader.f32().map_err(dec_err)?;
            let level = reader.var_i32().map_err(dec_err)?;
            let total = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ExperienceChanged {
                progress,
                level,
                total,
            })]);
        }
        if packet_id == play::clientbound::SET_CURSOR_ITEM {
            let mut reader = Reader::new(payload);
            let item = read_trailing_item_stack(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::CursorItemChanged {
                item,
            })]);
        }
        if packet_id == play::clientbound::SET_PLAYER_INVENTORY {
            let mut reader = Reader::new(payload);
            let slot = reader.var_i32().map_err(dec_err)?;
            let item = read_trailing_item_stack(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::InventorySlotChanged {
                slot,
                item,
            })]);
        }
        if packet_id == play::clientbound::COOLDOWN {
            // `Identifier.STREAM_CODEC` is `STRING_UTF8.map(Identifier::parse, ...)`
            // — a single length-prefixed "namespace:path" string, the same shape
            // `parse_key` already expects, not a separate namespace/path pair.
            let mut reader = Reader::new(payload);
            let group = reader.string(32767).map_err(dec_err)?;
            let duration_ticks = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ItemCooldown {
                group: parse_key(&group, "cooldown group")?,
                duration_ticks,
            })]);
        }
        if packet_id == play::clientbound::CHANGE_DIFFICULTY {
            // `Difficulty.STREAM_CODEC` wraps out-of-range ids in vanilla
            // (`ByIdMap.OutOfBoundsStrategy.WRAP`); this adapter instead treats an
            // id outside `0..=3` as an explicit decode error rather than silently
            // aliasing it to a different difficulty.
            let mut reader = Reader::new(payload);
            let difficulty_id = reader.var_i32().map_err(dec_err)?;
            let locked = reader.bool().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let difficulty = match difficulty_id {
                0 => Difficulty::Peaceful,
                1 => Difficulty::Easy,
                2 => Difficulty::Normal,
                3 => Difficulty::Hard,
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown difficulty id {other}"
                    )));
                }
            };
            return Ok(vec![Directive::Emit(ClientEvent::DifficultyChanged {
                difficulty,
                locked,
            })]);
        }
        if packet_id == play::clientbound::PLAYER_COMBAT_KILL {
            let mut reader = Reader::new(payload);
            // VarInt player id, then a network-NBT text component death message.
            reader
                .var_i32()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let component = read_network_nbt(&mut reader)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            return Ok(vec![Directive::Emit(ClientEvent::Death {
                message: Text::from_nbt(&component),
            })]);
        }
        if packet_id == play::clientbound::PLAYER_INFO_UPDATE {
            // Action-bitmask packet: decode the selected per-entry fields and
            // lift them into canonical player-list entries. Zero trailing bytes
            // is the misparse detector, since the field layout is conditional.
            let mut reader = Reader::new(payload);
            let update = PlayerInfoUpdate::decode(&mut reader, CTX)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let entries = update
                .entries
                .into_iter()
                .map(|entry| PlayerListEntry {
                    uuid: entry.uuid,
                    name: entry.name,
                    game_mode: entry.game_mode.and_then(tab_game_mode),
                    latency: entry.latency,
                    display_name: entry.display_name.map(Text::literal),
                    listed: entry.listed,
                    // Issue #62: carried through rather than dropped. The v770
                    // `ProfileProperty` and the model's are separate types by the
                    // usual version-seam rule, so this is a lower, not a move.
                    properties: entry.properties.map(|properties| {
                        properties
                            .into_iter()
                            .map(|property| ModelProfileProperty {
                                name: property.name,
                                value: property.value,
                                signature: property.signature,
                            })
                            .collect()
                    }),
                })
                .collect();
            return Ok(vec![Directive::Emit(ClientEvent::PlayerListUpdate {
                entries,
            })]);
        }
        if packet_id == play::clientbound::PLAYER_INFO_REMOVE {
            // The zero-trailing check still guards the wire: a misparse of the
            // UUID list would leave bytes that ensure_empty rejects.
            let remove: PlayerInfoRemove = decode_play(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::PlayerListRemove {
                profile_ids: remove.uuids,
            })]);
        }
        if packet_id == play::clientbound::CHUNKS_BIOMES {
            // `ClientboundChunksBiomesPacket` (id 13): a VarInt-prefixed list of
            // `(ChunkPos, byte[])` entries. Vanilla sends this to *resend* biomes
            // for chunks a player already has loaded — `ChunkMap.
            // resendBiomesForChunks`, whose only caller is `/fillbiome`
            // (`FillBiomeCommand.java`) — never at initial load, which is why the
            // per-section biome container already rides `level_chunk_with_light`
            // and this packet only ever *updates* it.
            //
            // Each entry's byte array is, per `ChunkBiomeData.extractChunkData`,
            // every section's `PalettedContainer<Holder<Biome>>.write` back to
            // back with **no other framing at all** — no non-air/fluid counts (it
            // has no blocks to count), no block-state container, just
            // `section_count` biome containers in ascending section order. That
            // makes this the one chunk-shaped packet whose per-section loop is
            // *shorter* than `level_chunk_with_light`'s, not a variant of it.
            let shape = self.current_shape();
            let mut reader = Reader::new(payload);
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count)
                .map_err(|_| AdapterError::Decode(format!("negative chunk-biomes count {count}")))?;
            let mut directives = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                // `ChunkPos.pack`/`unpack`: x in the low 32 bits, z in the high 32
                // — the same layout `forget_level_chunk` already unpacks.
                let packed = reader.i64().map_err(dec_err)?;
                let (x, z) = (packed as i32, (packed >> 32) as i32);
                let bytes = reader.var_bytes(2_097_152).map_err(dec_err)?;
                let mut blob = Reader::new(bytes);
                let mut patch = BiomePatch::new();
                for section_index in 0..shape.section_count {
                    let biomes = PalettedContainer::decode(shape.biome_kind, &mut blob)
                        .map_err(|err| AdapterError::Decode(err.to_string()))?;
                    patch.set_section(section_index, biomes);
                }
                // Zero trailing bytes in this chunk's own sub-blob is the
                // strongest per-chunk alignment check, exactly as
                // `level_chunk_with_light`'s section blob uses `ensure_empty` on
                // its own bounded sub-reader.
                blob.ensure_empty().map_err(dec_err)?;
                world.merge_biomes(WorldChunkPos::new(x, z), patch);
                // Reused rather than a new event: `ChunkLoaded` already means "the
                // column at pos is dirty, re-read or re-mesh it" (see
                // `light_update`'s arm above), which is exactly what a live biome
                // change needs — surface material and (once wired) tint both
                // read the world directly, not the event payload.
                directives.push(Directive::Emit(ClientEvent::ChunkLoaded {
                    pos: ChunkPos::new(x, z),
                }));
            }
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(directives);
        }
        if packet_id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT {
            // The chunk framing depends on the current dimension's build-height
            // window (set at login), which is not carried in the packet itself.
            let shape = self.current_shape();
            let mut reader = Reader::new(payload);
            let chunk = LevelChunkWithLight::decode(&mut reader, &shape)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            // Zero trailing bytes across the whole packet is the single best
            // detector of a subtly wrong layout: a misparse almost always
            // leaves the buffer misaligned, so reject rather than apply a
            // silently truncated chunk.
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            // Apply the fully decoded chunk (blocks, biomes, light, heightmaps,
            // block entities) straight into the client-owned world, moving each
            // part with no clone. The event then carries only the position.
            let pos = ChunkPos::new(chunk.x, chunk.z);
            world.load(
                WorldChunkPos::new(chunk.x, chunk.z),
                LoadedChunk::new(
                    chunk.column,
                    chunk.light,
                    chunk.heightmaps,
                    chunk.block_entities,
                ),
            );
            return Ok(vec![Directive::Emit(ClientEvent::ChunkLoaded { pos })]);
        }
        if packet_id == play::clientbound::LIGHT_UPDATE {
            // A standalone, light-only update carrying the same six-field light
            // payload embedded in `level_chunk_with_light`, but applied as a
            // *merge*: a section named by a full mask is replaced, one named by
            // an empty mask becomes explicit zero, and one named by neither is
            // left unchanged. All three-state semantics live in
            // `LightPatch::from_light_masks`; this arm only reads wire
            // primitives, in wire order. Note the wire order is NOT the
            // constructor's argument order — the four bitsets arrive
            // sky/block/empty-sky/empty-block, then the two array lists.
            let mut reader = Reader::new(payload);
            let x = reader.var_i32().map_err(dec_err)?;
            let z = reader.var_i32().map_err(dec_err)?;
            let sky_mask = read_wire_bitset(&mut reader)?;
            let block_mask = read_wire_bitset(&mut reader)?;
            let empty_sky_mask = read_wire_bitset(&mut reader)?;
            let empty_block_mask = read_wire_bitset(&mut reader)?;
            let sky_arrays = read_light_arrays(&mut reader)?;
            let block_arrays = read_light_arrays(&mut reader)?;
            // Zero trailing bytes is the highest-value detector here: a wrong
            // 2048 array length or an off-by-one bitset word-count leaves the
            // buffer misaligned, which shows up only as leftover bytes.
            reader.ensure_empty().map_err(dec_err)?;
            let patch = LightPatch::from_light_masks(
                &sky_mask,
                &empty_sky_mask,
                sky_arrays,
                &block_mask,
                &empty_block_mask,
                block_arrays,
            );
            world.merge_light(WorldChunkPos::new(x, z), patch);
            // `ChunkLoaded` doubles as "the region at pos is dirty; re-read or
            // re-mesh it" (its own docs) — exactly what a light change needs.
            return Ok(vec![Directive::Emit(ClientEvent::ChunkLoaded {
                pos: ChunkPos::new(x, z),
            })]);
        }
        if packet_id == play::clientbound::FORGET_LEVEL_CHUNK {
            // A single packed long: x in the low 32 bits, z in the high 32
            // (`ChunkPos.pack`, verified against 26.2 source).
            let mut reader = Reader::new(payload);
            let packed = reader
                .i64()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let (x, z) = (packed as i32, (packed >> 32) as i32);
            world.unload(WorldChunkPos::new(x, z));
            return Ok(vec![Directive::Emit(ClientEvent::ChunkUnloaded {
                pos: ChunkPos::new(x, z),
            })]);
        }
        if packet_id == play::clientbound::BLOCK_UPDATE {
            // A single block change: a packed `BlockPos` long and the new block
            // state's registry id. It mutates exactly the one loaded section that
            // owns the position — a no-op if that chunk is not held — so the
            // world stays live after break/place rather than frozen at load.
            let mut reader = Reader::new(payload);
            let packed = reader.i64().map_err(dec_err)?;
            let state = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let pos = unpack_block_pos(packed);
            let state = u32::try_from(state)
                .map_err(|_| AdapterError::Decode(format!("negative block state id {state}")))?;
            world.set_block(pos.x, pos.y, pos.z, state);
            // Writing a block state is what creates (or destroys) a block
            // entity: vanilla does it inside `LevelChunk.setBlockState`, with no
            // packet involved (`LevelChunk.java:341`). Skipping this is issue
            // #374 — a placed chest with a state, no record, and zero pixels,
            // which still *opened* because interaction reads the state.
            // `World::sync_block_entity` documents the create/keep/replace/remove
            // rule; the `Option` is the version-specific half.
            world.sync_block_entity(pos.x, pos.y, pos.z, block_entity_type(state));
            // Dirty exactly the section that owns the block. Without this a
            // break/place the *server* sends is applied to the world but never
            // drawn until some other event happens to dirty the column — the
            // silent desync behind "the chunk only renders properly when I
            // break something". A section-scoped signal (rather than reusing
            // `ChunkLoaded`) lets the consumer re-derive one section, and only
            // the neighbours a boundary cell actually touches.
            return Ok(vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
                section: SectionPos::new(pos.x >> 4, pos.y >> 4, pos.z >> 4),
                blocks: vec![[
                    pos.x.rem_euclid(16) as u8,
                    pos.y.rem_euclid(16) as u8,
                    pos.z.rem_euclid(16) as u8,
                ]],
            })]);
        }
        if packet_id == play::clientbound::SECTION_BLOCKS_UPDATE {
            // Many block changes within one section: a packed `SectionPos` long,
            // a count, then that many VarLongs each carrying `state << 12 | local`
            // where `local` packs the section-relative `x<<8 | z<<4 | y`. All
            // writes land in the one section, forking its storage at most once.
            let mut reader = Reader::new(payload);
            let node = reader.i64().map_err(dec_err)?;
            let (section_x, section_y, section_z) = unpack_section_pos(node);
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).map_err(|_| {
                AdapterError::Decode(format!("negative section update count {count}"))
            })?;
            // A section holds at most 4096 blocks; cap the pre-allocation so a
            // hostile count cannot force a large speculative allocation before
            // the truncated body is rejected by the per-entry reads.
            let mut blocks = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                let entry = reader.var_i64().map_err(dec_err)?;
                let local = (entry & 0xFFF) as u16;
                let state = u32::try_from((entry as u64) >> 12).map_err(|_| {
                    AdapterError::Decode("section block state id out of range".to_owned())
                })?;
                let rel_x = ((local >> 8) & 0xF) as u8;
                let rel_z = ((local >> 4) & 0xF) as u8;
                let rel_y = (local & 0xF) as u8;
                blocks.push((rel_x, rel_y, rel_z, state));
            }
            reader.ensure_empty().map_err(dec_err)?;
            world.set_blocks(section_x, section_y, section_z, &blocks);
            // Every state write goes through `sync_block_entity`, one call per
            // changed cell, for the same reason `BLOCK_UPDATE` does: in vanilla
            // `LevelChunk.setBlockState` is what creates and removes block
            // entities, no packet involved (`LevelChunk.java:308-348`). A piston
            // or a `/fill` arrives here rather than as N `BLOCK_UPDATE`s, so
            // skipping it would leave exactly the #374 bug for bulk edits.
            // Section-relative coordinates back to absolute — `set_blocks` does
            // the same conversion internally, but this seam takes absolute
            // coordinates because a block entity is keyed by world position.
            for &(rel_x, rel_y, rel_z, state) in &blocks {
                world.sync_block_entity(
                    (section_x << 4) | i32::from(rel_x),
                    (section_y << 4) | i32::from(rel_y),
                    (section_z << 4) | i32::from(rel_z),
                    block_entity_type(state),
                );
            }
            // Dirty the owning column so a server-authoritative multi-block
            // change (e.g. a falling tree, a piston, another player's edits) is
            // re-meshed rather than silently applied-but-invisible. An empty
            // change set touched nothing, so it needs no re-mesh. The relative
            // coordinates ride along so the consumer can distinguish an
            // interior edit from one on the section boundary.
            if blocks.is_empty() {
                return Ok(Vec::new());
            }
            return Ok(vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
                section: SectionPos::new(section_x, section_y, section_z),
                blocks: blocks.iter().map(|&(x, y, z, _)| [x, y, z]).collect(),
            })]);
        }
        if packet_id == play::clientbound::BLOCK_ENTITY_DATA {
            // A packed BlockPos long, a `registry(BLOCK_ENTITY_TYPE)` VarInt, then
            // the block entity's nameless network NBT compound (its "update tag",
            // not necessarily the full save tag). Mutates the world directly,
            // mirroring BLOCK_UPDATE/SECTION_BLOCKS_UPDATE: a no-op if the owning
            // chunk is not currently loaded.
            //
            // Since #374 this is what it is in vanilla — *data for an entity that
            // already exists*, created by the chunk packet's block-entity list or
            // by a state write through `sync_block_entity`. It nonetheless still
            // **creates** on a miss (`set_block_entity` is an upsert), which is a
            // deliberate divergence: vanilla's `handleBlockEntityData` drops the
            // payload when `getBlockEntity(pos, type)` is empty
            // (`ClientPacketListener.java:1476`, `BlockGetter.java:27-30`) because
            // it has `pendingBlockEntities` to promote from later, and we do not.
            // The two failure modes are not symmetric: an orphan record whose
            // state is not a chest resolves to no material and draws nothing (see
            // `lodestone-shell`'s `block_entities`), so creating is inert, whereas
            // dropping would lose server data we cannot ask for again.
            let mut reader = Reader::new(payload);
            let packed = reader.i64().map_err(dec_err)?;
            let type_id = reader.var_i32().map_err(dec_err)?;
            let type_id = u32::try_from(type_id).map_err(|_| {
                AdapterError::Decode(format!("negative block entity type id {type_id}"))
            })?;
            let nbt = read_network_nbt(&mut reader).map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let pos = unpack_block_pos(packed);
            world.set_block_entity(pos.x, pos.y, pos.z, type_id, nbt);
            return Ok(Vec::new());
        }
        if packet_id == play::clientbound::BLOCK_EVENT {
            // A packed BlockPos long, two opaque parameter bytes, then a
            // `registry(BLOCK)` VarInt naming the block type the parameters apply
            // to (needed by the consumer to interpret b0/b1 — e.g. a note pitch
            // vs. a piston direction — which the adapter itself does not).
            let mut reader = Reader::new(payload);
            let packed = reader.i64().map_err(dec_err)?;
            let b0 = reader.u8().map_err(dec_err)?;
            let b1 = reader.u8().map_err(dec_err)?;
            let block_id = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let block_id = u32::try_from(block_id)
                .map_err(|_| AdapterError::Decode(format!("negative block id {block_id}")))?;
            let name = block_type_name(block_id)
                .ok_or_else(|| AdapterError::Decode(format!("unknown block id {block_id}")))?;
            return Ok(vec![Directive::Emit(ClientEvent::BlockEvent {
                pos: unpack_block_pos(packed),
                b0,
                b1,
                block: parse_key(name, "block")?,
            })]);
        }
        if packet_id == play::clientbound::BLOCK_DESTRUCTION {
            // A VarInt breaker entity id, a packed BlockPos long, then the raw
            // break-stage byte. The stage's exact visual meaning beyond the wire
            // (which values clear the overlay) is a rendering concern, not
            // decoded here.
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let packed = reader.i64().map_err(dec_err)?;
            let progress = reader.u8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::BlockDestruction {
                entity_id,
                pos: unpack_block_pos(packed),
                progress,
            })]);
        }
        if packet_id == play::clientbound::BLOCK_CHANGED_ACK {
            let mut reader = Reader::new(payload);
            let sequence = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::BlockChangedAck {
                sequence,
            })]);
        }
        if packet_id == play::clientbound::SET_CHUNK_CACHE_CENTER {
            let mut reader = Reader::new(payload);
            let x = reader.var_i32().map_err(dec_err)?;
            let z = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::ChunkCacheCenterChanged { x, z },
            )]);
        }
        if packet_id == play::clientbound::SET_CHUNK_CACHE_RADIUS {
            let mut reader = Reader::new(payload);
            let radius = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::ChunkCacheRadiusChanged { radius },
            )]);
        }
        if packet_id == play::clientbound::SET_SIMULATION_DISTANCE {
            let mut reader = Reader::new(payload);
            let distance = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::SimulationDistanceChanged { distance },
            )]);
        }
        if packet_id == play::clientbound::PLAYER_POSITION {
            return handle_player_position(payload);
        }
        if packet_id == play::clientbound::ADD_ENTITY {
            return handle_add_entity(payload, &self.variants);
        }
        if packet_id == play::clientbound::REMOVE_ENTITIES {
            return handle_remove_entities(payload, &self.variants);
        }
        if packet_id == play::clientbound::MOVE_ENTITY_POS {
            return handle_move_entity(payload, true, false);
        }
        if packet_id == play::clientbound::MOVE_ENTITY_POS_ROT {
            return handle_move_entity(payload, true, true);
        }
        if packet_id == play::clientbound::MOVE_ENTITY_ROT {
            return handle_move_entity(payload, false, true);
        }
        if packet_id == play::clientbound::TELEPORT_ENTITY {
            return handle_entity_position(payload, true);
        }
        if packet_id == play::clientbound::ENTITY_POSITION_SYNC {
            return handle_entity_position(payload, false);
        }
        if packet_id == play::clientbound::SET_ENTITY_MOTION {
            return handle_set_entity_motion(payload);
        }
        if packet_id == play::clientbound::MOVE_MINECART_ALONG_TRACK {
            return handle_move_minecart_along_track(payload);
        }
        if packet_id == play::clientbound::SET_ENTITY_DATA {
            return Ok(handle_set_entity_data(payload, &self.variants));
        }
        if packet_id == play::clientbound::UPDATE_ATTRIBUTES {
            return Ok(handle_update_attributes(payload));
        }
        if packet_id == play::clientbound::ENTITY_EVENT {
            // Raw `int` entity id (NOT a VarInt — one of the few remaining
            // fixed-width ids in play) then a raw status byte.
            let mut reader = Reader::new(payload);
            let entity_id = reader.i32().map_err(dec_err)?;
            let status = reader.u8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityStatus {
                entity_id,
                status,
            })]);
        }
        if packet_id == play::clientbound::ROTATE_HEAD {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let packed = reader.i8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityHeadRotation {
                entity_id,
                head_yaw: unpack_degrees(packed),
            })]);
        }
        if packet_id == play::clientbound::SET_PASSENGERS {
            // A VarInt vehicle id then a VarInt-length-prefixed VarInt array —
            // `readVarIntArray`, not the general `Vec<T>` derive shape, so read
            // by hand.
            let mut reader = Reader::new(payload);
            let vehicle_id = reader.var_i32().map_err(dec_err)?;
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count)
                .map_err(|_| AdapterError::Decode(format!("negative passenger count {count}")))?;
            let mut passenger_ids = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                passenger_ids.push(reader.var_i32().map_err(dec_err)?);
            }
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::EntityPassengersChanged {
                    vehicle_id,
                    passenger_ids,
                },
            )]);
        }
        if packet_id == play::clientbound::SET_ENTITY_LINK {
            // Two raw `int`s (source, dest); dest `0` means "no holder", matching
            // vanilla's own sentinel (entity id 0 is never a valid entity).
            let mut reader = Reader::new(payload);
            let entity_id = reader.i32().map_err(dec_err)?;
            let holder_id = reader.i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityLeashed {
                entity_id,
                holder_id: (holder_id != 0).then_some(holder_id),
            })]);
        }
        if packet_id == play::clientbound::TAKE_ITEM_ENTITY {
            let mut reader = Reader::new(payload);
            let item_entity_id = reader.var_i32().map_err(dec_err)?;
            let player_id = reader.var_i32().map_err(dec_err)?;
            let amount = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ItemPickup {
                item_entity_id,
                player_id,
                amount,
            })]);
        }
        if packet_id == play::clientbound::DAMAGE_EVENT {
            return decode_damage_event(payload);
        }
        if packet_id == play::clientbound::HURT_ANIMATION {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let yaw = reader.f32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityHurtAnimation {
                entity_id,
                yaw,
            })]);
        }
        if packet_id == play::clientbound::ANIMATE {
            // A fixed, sparse set of named action constants (`1` is reserved and
            // never sent); anything else travels through `Other` rather than
            // being rejected, since a future action byte is still meaningful to
            // a consumer even if this table does not name it.
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let action = reader.u8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let action = match action {
                0 => AnimationAction::SwingMainHand,
                2 => AnimationAction::WakeUp,
                3 => AnimationAction::SwingOffHand,
                4 => AnimationAction::CriticalHit,
                5 => AnimationAction::MagicCriticalHit,
                other => AnimationAction::Other(other),
            };
            return Ok(vec![Directive::Emit(ClientEvent::EntityAnimation {
                entity_id,
                action,
            })]);
        }
        if packet_id == play::clientbound::UPDATE_MOB_EFFECT {
            // entity id, a `minecraft:mob_effect` registry VarInt id (a fixed,
            // built-in registry — unlike damage_type — so resolved to a name via
            // the generated table), amplifier, duration, then a bitset byte.
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let effect_id = reader.var_i32().map_err(dec_err)?;
            let amplifier = reader.var_i32().map_err(dec_err)?;
            let duration_ticks = reader.var_i32().map_err(dec_err)?;
            let flags = reader.u8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let name = mob_effect_name(effect_id).ok_or_else(|| {
                AdapterError::Decode(format!("unknown mob effect id {effect_id}"))
            })?;
            return Ok(vec![Directive::Emit(ClientEvent::MobEffectApplied {
                entity_id,
                effect: parse_key(name, "mob effect")?,
                amplifier,
                duration_ticks,
                ambient: flags & 0x1 != 0,
                visible: flags & 0x2 != 0,
                show_icon: flags & 0x4 != 0,
                blend: flags & 0x8 != 0,
            })]);
        }
        if packet_id == play::clientbound::REMOVE_MOB_EFFECT {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let effect_id = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let name = mob_effect_name(effect_id).ok_or_else(|| {
                AdapterError::Decode(format!("unknown mob effect id {effect_id}"))
            })?;
            return Ok(vec![Directive::Emit(ClientEvent::MobEffectRemoved {
                entity_id,
                effect: parse_key(name, "mob effect")?,
            })]);
        }
        if packet_id == play::clientbound::MOVE_VEHICLE {
            let mut reader = Reader::new(payload);
            let x = reader.f64().map_err(dec_err)?;
            let y = reader.f64().map_err(dec_err)?;
            let z = reader.f64().map_err(dec_err)?;
            let yaw = reader.f32().map_err(dec_err)?;
            let pitch = reader.f32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::VehicleMoved {
                pos: Vec3 { x, y, z },
                yaw,
                pitch,
            })]);
        }
        if packet_id == play::clientbound::RESPAWN {
            // A dimension change (or post-death respawn) resets the build-height
            // window that frames every subsequent chunk. Decode the spawn info
            // in full — the trailing zero-length check is the misparse detector
            // for the conditional last-death-location field — and record the new
            // dimension so `level_chunk_with_light` stays aligned across the
            // nether/end boundary.
            let mut reader = Reader::new(payload);
            let respawn = Respawn::decode(&mut reader, CTX)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            // Respawn is also how the server reports portal travel, so the
            // dimension type moves here too — and it is the *only* place a
            // Nether trip's `min_y`/`height` change can be picked up.
            let dimension_type = self.enter_dimension(respawn.dimension_type, &respawn.dimension);
            let dimension = respawn.dimension.parse().map_err(|_| {
                AdapterError::Decode(format!("invalid dimension {}", respawn.dimension))
            })?;
            let mode = game_mode(respawn.game_type)?;
            let previous_game_mode = if respawn.previous_game_type < 0 {
                None
            } else {
                Some(game_mode(respawn.previous_game_type as u8)?)
            };
            let last_death_location = respawn
                .last_death_location
                .map(|loc| -> Result<DeathLocation, AdapterError> {
                    let dimension = loc.dimension.parse().map_err(|_| {
                        AdapterError::Decode(format!(
                            "invalid death location dimension {}",
                            loc.dimension
                        ))
                    })?;
                    Ok(DeathLocation {
                        dimension,
                        pos: unpack_block_pos(loc.position),
                    })
                })
                .transpose()?;
            return Ok(vec![
                Directive::Emit(ClientEvent::DimensionTypeChanged {
                    holder_id: respawn.dimension_type,
                    dimension_type,
                }),
                Directive::Emit(ClientEvent::Respawned {
                    dimension,
                    game_mode: mode,
                    previous_game_mode,
                    last_death_location,
                }),
            ]);
        }
        if packet_id == play::clientbound::SET_TIME {
            // 26.2 reshaped set_time: a monotonic world age followed by a map of
            // per-world-clock updates (see `packets::time`). Decode it fully so
            // the trailing zero-length check guards the variable-length map.
            let mut reader = Reader::new(payload);
            let time = SetTime::decode(&mut reader, CTX)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            // The day time is *held*, not read off the packet: 19 of every 20
            // `set_time`s carry an empty clock map (the once-a-second game-time
            // sync), and treating that as "the day time is the world age" pinned
            // `sky_darken` to a session constant. Re-anchor only on a real clock
            // update; otherwise extrapolate the held anchor at the server's own
            // rate. See `DayClock` and `SetTime::day_clock`.
            // Which clock is "the" day clock is a *registry* question, and until
            // #288 it was answered by "the lowest holder id present", which is
            // the overworld clock in every dimension because vanilla registers
            // it first. In the End the right clock is `minecraft:the_end`
            // (holder 1) — see `ClientRegistries::world_clock_id`.
            //
            // `None` here (no `registry_data`, or a dimension with no clock of
            // its own — the Nether has fixed time and no `default_clock`) keeps
            // the lowest-id fallback. That is deliberate rather than reporting
            // "no time": `time_of_day`'s only consumer is a sky curve that does
            // not yet gate on `has_fixed_time`, so a Nether trip reporting the
            // overworld's clock is exactly as good as before and no worse.
            let time_of_day = {
                let clock_holder = self.current_clock_holder();
                let mut clock = self.clock.lock().expect("day clock poisoned");
                if let Some(update) = time.clock_for(clock_holder) {
                    *clock = DayClock {
                        total_ticks: update.total_ticks,
                        rate: update.rate,
                        at_game_time: time.game_time,
                        synced: true,
                    };
                } else if !clock.synced {
                    // No clock update has ever arrived (we are ahead of the
                    // join-time full sync). Seed from the world age, which is
                    // exactly what this arm used to report unconditionally, so
                    // this window is no worse than before and closes on the
                    // first real update.
                    *clock = DayClock {
                        total_ticks: time.game_time,
                        rate: 1.0,
                        at_game_time: time.game_time,
                        synced: false,
                    };
                }
                clock.time_of_day(time.game_time)
            };
            return Ok(vec![Directive::Emit(ClientEvent::TimeChanged {
                world_age: time.game_time,
                time_of_day,
            })]);
        }
        if packet_id == play::clientbound::GAME_EVENT {
            // A small keyed world-state change. Only the aspects the model can
            // represent are surfaced; the rest (demo, arrow-hit, etc.) decode
            // fully — so the trailing check still guards alignment — but
            // produce no directive.
            let event: GameEvent = decode_full(payload)?;
            let directives = match event.event {
                1 => vec![Directive::Emit(ClientEvent::WeatherChanged {
                    raining: Some(true),
                    rain_level: None,
                    thunder_level: None,
                })],
                2 => vec![Directive::Emit(ClientEvent::WeatherChanged {
                    raining: Some(false),
                    rain_level: None,
                    thunder_level: None,
                })],
                3 => game_mode_from_ordinal(event.param as i32)
                    .map(|game_mode| {
                        vec![Directive::Emit(ClientEvent::GameModeChanged { game_mode })]
                    })
                    .unwrap_or_default(),
                // WIN_GAME (issue #192): exiting the End through the exit
                // portal after the dragon fight. Vanilla's own handler
                // ignores `param` for this event and always opens the
                // credits screen with `showCredits = true`
                // (`ClientPacketListener.java:1548-1552`), so nothing from
                // the wire needs to ride along — see `ClientEvent::WinGame`'s
                // own doc.
                4 => vec![Directive::Emit(ClientEvent::WinGame)],
                7 => vec![Directive::Emit(ClientEvent::WeatherChanged {
                    raining: None,
                    rain_level: Some(event.param),
                    thunder_level: None,
                })],
                8 => vec![Directive::Emit(ClientEvent::WeatherChanged {
                    raining: None,
                    rain_level: None,
                    thunder_level: Some(event.param),
                })],
                _ => Vec::new(),
            };
            return Ok(directives);
        }
        if packet_id == play::clientbound::SET_DEFAULT_SPAWN_POSITION {
            // Reshaped in 26.2 to carry a full RespawnData: a dimension-qualified
            // position plus yaw and pitch. The model now models all of these.
            let spawn: SetDefaultSpawnPosition = decode_full(payload)?;
            let dimension = spawn.location.dimension.parse().map_err(|_| {
                AdapterError::Decode(format!("invalid dimension {}", spawn.location.dimension))
            })?;
            return Ok(vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
                dimension,
                pos: unpack_block_pos(spawn.location.position),
                angle: spawn.yaw,
                pitch: spawn.pitch,
            })]);
        }
        if packet_id == play::clientbound::PLAYER_ABILITIES {
            let abilities: PlayerAbilities = decode_full(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::AbilitiesChanged {
                invulnerable: abilities.flags & ABILITY_FLAG_INVULNERABLE != 0,
                flying: abilities.flags & ABILITY_FLAG_FLYING != 0,
                can_fly: abilities.flags & ABILITY_FLAG_CAN_FLY != 0,
                instabuild: abilities.flags & ABILITY_FLAG_INSTABUILD != 0,
                flying_speed: abilities.flying_speed,
                walking_speed: abilities.walking_speed,
            })]);
        }
        if packet_id == play::clientbound::LEVEL_EVENT {
            let level_event: LevelEvent = decode_full(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::LevelEvent {
                event: level_event.event,
                pos: unpack_block_pos(level_event.position),
                data: level_event.data,
                global: level_event.global,
            })]);
        }
        if packet_id == play::clientbound::LEVEL_PARTICLES {
            // The particle type is the final field: a registry id followed by
            // per-type option bytes the model does not carry. The prefix decodes
            // to fixed widths (so a misparse is caught before the id) and the
            // options are swallowed by `remaining`.
            let particles: LevelParticles = decode_full(payload)?;
            let name = particle_type_name(particles.particle_id).ok_or_else(|| {
                AdapterError::Decode(format!("unknown particle id {}", particles.particle_id))
            })?;
            return Ok(vec![Directive::Emit(ClientEvent::Particles {
                particle: parse_key(name, "particle")?,
                long_distance: particles.override_limiter,
                pos: Vec3 {
                    x: particles.x,
                    y: particles.y,
                    z: particles.z,
                },
                offset: Vec3f {
                    x: particles.x_dist,
                    y: particles.y_dist,
                    z: particles.z_dist,
                },
                max_speed: particles.max_speed,
                count: particles.count,
            })]);
        }
        if packet_id == play::clientbound::EXPLODE {
            return decode_explode(payload);
        }
        if packet_id == play::clientbound::SOUND {
            return decode_sound(payload);
        }
        if packet_id == play::clientbound::SOUND_ENTITY {
            return decode_sound_entity(payload);
        }
        if packet_id == play::clientbound::OPEN_SCREEN {
            return decode_open_screen(payload);
        }
        if packet_id == play::clientbound::PLAYER_ROTATION {
            let mut reader = Reader::new(payload);
            let y_rot = reader.f32().map_err(dec_err)?;
            let relative_y = reader.bool().map_err(dec_err)?;
            let x_rot = reader.f32().map_err(dec_err)?;
            let relative_x = reader.bool().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::PlayerRotationSet {
                y_rot,
                relative_y,
                x_rot,
                relative_x,
            })]);
        }
        if packet_id == play::clientbound::SET_CAMERA {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::CameraSet { entity_id })]);
        }
        if packet_id == play::clientbound::OPEN_BOOK {
            // `InteractionHand` ordinal: 0 = main hand, 1 = off hand.
            let mut reader = Reader::new(payload);
            let ordinal = reader.var_i32().map_err(dec_err)?;
            let main_hand = match ordinal {
                0 => true,
                1 => false,
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown interaction hand ordinal {other}"
                    )));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::BookOpened { main_hand })]);
        }
        if packet_id == play::clientbound::STOP_SOUND {
            // A flags byte: bit 0 = a source category follows, bit 1 = a sound
            // identifier follows. Either, both, or neither may be present.
            let mut reader = Reader::new(payload);
            let flags = reader.u8().map_err(dec_err)?;
            let category = if flags & 0x1 != 0 {
                Some(read_sound_category(&mut reader)?)
            } else {
                None
            };
            let sound = if flags & 0x2 != 0 {
                let name = reader.string(32767).map_err(dec_err)?;
                Some(parse_key(&name, "sound")?)
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::SoundStopped {
                sound,
                category,
            })]);
        }
        if packet_id == play::clientbound::TAB_LIST {
            let mut reader = Reader::new(payload);
            let header = read_network_nbt(&mut reader).map_err(dec_err)?;
            let footer = read_network_nbt(&mut reader).map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TabListChanged {
                header: Text::from_nbt(&header),
                footer: Text::from_nbt(&footer),
            })]);
        }
        if packet_id == play::clientbound::SET_BORDER_CENTER {
            let mut reader = Reader::new(payload);
            let x = reader.f64().map_err(dec_err)?;
            let z = reader.f64().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::WorldBorderCenterChanged { x, z },
            )]);
        }
        if packet_id == play::clientbound::SET_BORDER_LERP_SIZE {
            let mut reader = Reader::new(payload);
            let old_size = reader.f64().map_err(dec_err)?;
            let new_size = reader.f64().map_err(dec_err)?;
            let lerp_time_ms = reader.var_i64().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::WorldBorderSizeLerping {
                old_size,
                new_size,
                lerp_time_ms,
            })]);
        }
        if packet_id == play::clientbound::SET_BORDER_SIZE {
            let mut reader = Reader::new(payload);
            let size = reader.f64().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::WorldBorderSizeChanged {
                size,
            })]);
        }
        if packet_id == play::clientbound::SET_BORDER_WARNING_DELAY {
            let mut reader = Reader::new(payload);
            let warning_time = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::WorldBorderWarningDelayChanged { warning_time },
            )]);
        }
        if packet_id == play::clientbound::SET_BORDER_WARNING_DISTANCE {
            let mut reader = Reader::new(payload);
            let warning_blocks = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::WorldBorderWarningDistanceChanged { warning_blocks },
            )]);
        }
        if packet_id == play::clientbound::INITIALIZE_BORDER {
            let mut reader = Reader::new(payload);
            let x = reader.f64().map_err(dec_err)?;
            let z = reader.f64().map_err(dec_err)?;
            let old_size = reader.f64().map_err(dec_err)?;
            let new_size = reader.f64().map_err(dec_err)?;
            let lerp_time_ms = reader.var_i64().map_err(dec_err)?;
            let absolute_max_size = reader.var_i32().map_err(dec_err)?;
            let warning_blocks = reader.var_i32().map_err(dec_err)?;
            let warning_time = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::WorldBorderInitialized {
                x,
                z,
                old_size,
                new_size,
                lerp_time_ms,
                absolute_max_size,
                warning_blocks,
                warning_time,
            })]);
        }
        if packet_id == play::clientbound::PLAYER_COMBAT_ENTER {
            // `ClientboundPlayerCombatEnterPacket` is a singleton with no
            // fields (`StreamCodec.unit`).
            let reader = Reader::new(payload);
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::PlayerCombatEntered)]);
        }
        if packet_id == play::clientbound::PLAYER_COMBAT_END {
            let mut reader = Reader::new(payload);
            let duration_ticks = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::PlayerCombatEnded {
                duration_ticks,
            })]);
        }
        if packet_id == play::clientbound::OPEN_SIGN_EDITOR {
            let mut reader = Reader::new(payload);
            let packed = reader.i64().map_err(dec_err)?;
            let is_front_text = reader.bool().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::SignEditorOpened {
                pos: unpack_block_pos(packed),
                is_front_text,
            })]);
        }
        if packet_id == play::clientbound::SELECT_ADVANCEMENTS_TAB {
            let mut reader = Reader::new(payload);
            let has_tab = reader.bool().map_err(dec_err)?;
            let tab = if has_tab {
                let name = reader.string(32767).map_err(dec_err)?;
                Some(parse_key(&name, "advancement tab")?)
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::AdvancementsTabSelected {
                tab,
            })]);
        }
        if packet_id == play::clientbound::PROJECTILE_POWER {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let acceleration_power = reader.f64().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ProjectilePowerChanged {
                entity_id,
                acceleration_power,
            })]);
        }
        if packet_id == play::clientbound::MOUNT_SCREEN_OPEN {
            // Unlike most entity ids on the wire, `entityId` here is a raw
            // 4-byte `int` (`FriendlyByteBuf::readInt`), not a VarInt.
            let mut reader = Reader::new(payload);
            let container_id = reader.var_i32().map_err(dec_err)?;
            let inventory_columns = reader.var_i32().map_err(dec_err)?;
            let entity_id = reader.i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::MountScreenOpened {
                container_id,
                inventory_columns,
                entity_id,
            })]);
        }
        if packet_id == play::clientbound::GAME_RULE_VALUES {
            let mut reader = Reader::new(payload);
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count)
                .map_err(|_| AdapterError::Decode(format!("invalid game rule count {count}")))?;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                let key = reader.string(32767).map_err(dec_err)?;
                let key = parse_key(&key, "game rule")?;
                let value = reader.string(32767).map_err(dec_err)?;
                values.push((key, value));
            }
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::GameRulesChanged {
                values,
            })]);
        }
        if packet_id == play::clientbound::PLAYER_LOOK_AT {
            let mut reader = Reader::new(payload);
            let from_anchor = read_look_anchor(&mut reader)?;
            let x = reader.f64().map_err(dec_err)?;
            let y = reader.f64().map_err(dec_err)?;
            let z = reader.f64().map_err(dec_err)?;
            let at_entity_flag = reader.bool().map_err(dec_err)?;
            let at_entity = if at_entity_flag {
                let entity_id = reader.var_i32().map_err(dec_err)?;
                let to_anchor = read_look_anchor(&mut reader)?;
                Some(PlayerLookAtEntity {
                    entity_id,
                    to_anchor,
                })
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::PlayerLookAt {
                from_anchor,
                target: Vec3 { x, y, z },
                at_entity,
            })]);
        }
        if packet_id == play::clientbound::MAP_ITEM_DATA {
            return decode_map_item_data(payload);
        }
        if packet_id == play::clientbound::UPDATE_ADVANCEMENTS {
            return decode_update_advancements(payload);
        }
        // ---- issue #26: the remaining clientbound set ----------------------
        //
        // Every layout below was read off the record definition in
        // `.cache/mc/26.2/src`. Where a payload is carried as opaque bytes the
        // reason is stated at the decoder, and it is always the same reason: the
        // value is a *schema* (an NBT `Codec` union, or a per-registry-entry
        // codec table) rather than a `StreamCodec`, so decoding it is a
        // renderer's problem and not the wire's.
        if packet_id == play::clientbound::AWARD_STATS {
            return decode_award_stats(payload);
        }
        if packet_id == play::clientbound::DEBUG_BLOCK_VALUE {
            let mut reader = Reader::new(payload);
            let pos = unpack_block_pos(reader.i64().map_err(dec_err)?);
            let (subscription, value) = read_debug_update(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::DebugBlockValue {
                pos,
                subscription,
                value,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_CHUNK_VALUE {
            let mut reader = Reader::new(payload);
            // `ChunkPos.STREAM_CODEC` is one packed long: low 32 bits x, high 32
            // bits z (`ChunkPos.unpack`). Not two VarInts.
            let packed = reader.i64().map_err(dec_err)?;
            #[allow(clippy::cast_possible_truncation)]
            let chunk = ChunkPos {
                x: packed as i32,
                z: (packed >> 32) as i32,
            };
            let (subscription, value) = read_debug_update(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::DebugChunkValue {
                chunk,
                subscription,
                value,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_ENTITY_VALUE {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let (subscription, value) = read_debug_update(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::DebugEntityValue {
                entity_id,
                subscription,
                value,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_EVENT {
            // `DebugSubscription.Event` dispatches the same way `Update` does but
            // **without** the `ByteBufCodecs.optional` wrapper — an event always
            // has a value. Reusing `read_debug_update` here would eat the first
            // payload byte as a present-flag.
            let mut reader = Reader::new(payload);
            let subscription = read_debug_subscription_key(&mut reader)?;
            let value = reader.remaining_bytes().to_vec();
            return Ok(vec![Directive::Emit(ClientEvent::DebugEvent {
                subscription,
                value,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_SAMPLE {
            let mut reader = Reader::new(payload);
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count)
                .map_err(|_| AdapterError::Decode(format!("invalid sample count {count}")))?;
            let mut sample = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                sample.push(reader.i64().map_err(dec_err)?);
            }
            let kind = match reader.var_i32().map_err(dec_err)? {
                0 => DebugSampleKind::TickTime,
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown debug sample type {other}"
                    )));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::DebugSample {
                sample,
                kind,
            })]);
        }
        if packet_id == play::clientbound::GAME_TEST_HIGHLIGHT_POS {
            let mut reader = Reader::new(payload);
            let absolute = unpack_block_pos(reader.i64().map_err(dec_err)?);
            let relative = unpack_block_pos(reader.i64().map_err(dec_err)?);
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::GameTestHighlightPos {
                absolute,
                relative,
            })]);
        }
        if packet_id == play::clientbound::WAYPOINT {
            return decode_waypoint(payload);
        }
        if packet_id == play::clientbound::TAG_QUERY {
            let mut reader = Reader::new(payload);
            let transaction_id = reader.var_i32().map_err(dec_err)?;
            // `writeNbt` writes a bare `TAG_End` byte (0) for null, so the tail
            // is either that one byte or a whole compound. Carried as raw bytes
            // rather than a parsed `Nbt` because a queried block entity's tag is
            // arbitrary server/datapack data with no schema this crate models.
            let tail = reader.remaining_bytes();
            let tag = if tail == [0u8] {
                None
            } else {
                Some(tail.to_vec())
            };
            return Ok(vec![Directive::Emit(ClientEvent::TagQueryResponse {
                transaction_id,
                tag,
            })]);
        }
        if packet_id == play::clientbound::TICKING_STATE {
            let mut reader = Reader::new(payload);
            let tick_rate = reader.f32().map_err(dec_err)?;
            let frozen = reader.bool().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TickingStateChanged {
                tick_rate,
                frozen,
            })]);
        }
        if packet_id == play::clientbound::TICKING_STEP {
            let mut reader = Reader::new(payload);
            let tick_steps = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TickingStepped {
                tick_steps,
            })]);
        }
        if packet_id == play::clientbound::TEST_INSTANCE_BLOCK_STATUS {
            let mut reader = Reader::new(payload);
            let status = read_network_nbt(&mut reader).map_err(dec_err)?;
            let size = if reader.bool().map_err(dec_err)? {
                Some((
                    reader.var_i32().map_err(dec_err)?,
                    reader.var_i32().map_err(dec_err)?,
                    reader.var_i32().map_err(dec_err)?,
                ))
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::TestInstanceBlockStatus {
                    status: Text::from_nbt(&status),
                    size,
                },
            )]);
        }
        if packet_id == play::clientbound::SHOW_DIALOG {
            return decode_show_dialog(payload);
        }
        if packet_id == play::clientbound::CLEAR_DIALOG {
            Reader::new(payload).ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::DialogCleared)]);
        }
        if packet_id == play::clientbound::RECIPE_BOOK_REMOVE {
            let mut reader = Reader::new(payload);
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).map_err(|_| {
                AdapterError::Decode(format!("invalid recipe_book_remove count {count}"))
            })?;
            let mut display_ids = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                display_ids.push(reader.var_i32().map_err(dec_err)?);
            }
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::RecipeBookRemoved {
                display_ids,
            })]);
        }
        if packet_id == play::clientbound::RECIPE_BOOK_ADD {
            return decode_recipe_book_add(payload);
        }
        if packet_id == play::clientbound::PLACE_GHOST_RECIPE {
            let mut reader = Reader::new(payload);
            let window_id = reader.var_i32().map_err(dec_err)?;
            let Some(result_items) = read_recipe_display(&mut reader)? else {
                // An unmodeled nested display: the reader's position is no longer
                // trustworthy, so drop the packet rather than emit a half-read
                // event. Same contract as `read_component_patch`'s bail-out.
                return Ok(Vec::new());
            };
            return Ok(vec![Directive::Emit(ClientEvent::GhostRecipeShown {
                window_id,
                result_items,
            })]);
        }
        if packet_id == play::clientbound::UPDATE_RECIPES {
            return decode_update_recipes(payload);
        }
        if packet_id == play::clientbound::MERCHANT_OFFERS {
            return decode_merchant_offers(payload);
        }
        Ok(Vec::new())
    }
}

impl VersionAdapter for V770Adapter {
    fn protocol_version(&self) -> i32 {
        PROTOCOL
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["26.2"]
    }

    fn supports(&self, protocol: i32) -> bool {
        protocol == PROTOCOL
    }

    fn begin_login(
        &self,
        profile: &LoginProfile,
        server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        let intention = Intention {
            protocol_version: PROTOCOL,
            host: server.host.clone(),
            port: server.port,
            next_state: NEXT_STATE_LOGIN,
        };
        let hello = crate::packets::login::LoginHello {
            name: profile.username.clone(),
            profile_id: profile.uuid,
        };
        Ok(vec![
            send(handshaking::serverbound::INTENTION, &intention)?,
            Directive::SetState(ConnectionState::Login),
            send(login::serverbound::HELLO, &hello)?,
        ])
    }

    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        match state {
            ConnectionState::Login => self.handle_login(packet_id, payload),
            ConnectionState::Configuration => self.handle_configuration(packet_id, payload),
            ConnectionState::Play => self.handle_play(world, packet_id, payload),
            ConnectionState::Handshaking | ConnectionState::Status => {
                Err(AdapterError::UnsupportedPacketState { state })
            }
        }
    }
    fn encode_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        match action {
            ClientAction::KeepAliveResponse { id } => {
                let body = KeepAlive { id: *id };
                match state {
                    ConnectionState::Play => {
                        Ok(Some((play::serverbound::KEEP_ALIVE, encode_body(&body)?)))
                    }
                    ConnectionState::Configuration => Ok(Some((
                        configuration::serverbound::KEEP_ALIVE,
                        encode_body(&body)?,
                    ))),
                    _ => Ok(None),
                }
            }
            ClientAction::SendCommand { command } if state == ConnectionState::Play => {
                let body = ChatCommand {
                    command: command.clone(),
                };
                Ok(Some((play::serverbound::CHAT_COMMAND, encode_body(&body)?)))
            }
            ClientAction::ChatAck { offset } if state == ConnectionState::Play => {
                let body = ChatAck { offset: *offset };
                Ok(Some((play::serverbound::CHAT_ACK, encode_body(&body)?)))
            }
            ClientAction::SendChat { text } if state == ConnectionState::Play => Ok(Some((
                play::serverbound::CHAT,
                encode_body(&ChatMessage::unsigned(text.clone()))?,
            ))),
            ClientAction::Respawn if state == ConnectionState::Play => {
                // `client_command` action 0 = perform_respawn; leaves the death screen.
                let body = ClientCommand { action: 0 };
                Ok(Some((
                    play::serverbound::CLIENT_COMMAND,
                    encode_body(&body)?,
                )))
            }
            ClientAction::Move {
                pos,
                rotation,
                on_ground,
                horizontal_collision,
            } if state == ConnectionState::Play => {
                self.select_move_packet(*pos, *rotation, *on_ground, *horizontal_collision)
            }
            ClientAction::SwingArm { hand } if state == ConnectionState::Play => {
                let body = Swing {
                    hand: match hand {
                        Hand::Main => 0,
                        Hand::Off => 1,
                    },
                };
                Ok(Some((play::serverbound::SWING, encode_body(&body)?)))
            }
            ClientAction::BlockAction {
                action,
                pos,
                face,
                sequence,
            } if state == ConnectionState::Play => {
                let body = PlayerAction {
                    action: match action {
                        BlockActionKind::StartDestroy => 0,
                        BlockActionKind::AbortDestroy => 1,
                        BlockActionKind::StopDestroy => 2,
                    },
                    pos: pack_block_pos(*pos),
                    direction: face_ordinal(*face) as u8,
                    sequence: *sequence,
                };
                Ok(Some((
                    play::serverbound::PLAYER_ACTION,
                    encode_body(&body)?,
                )))
            }
            ClientAction::DropSelectedItem
            | ClientAction::DropSelectedItemStack
            | ClientAction::SwapItemWithOffhand
            | ClientAction::ReleaseUseItem
            | ClientAction::Stab
                if state == ConnectionState::Play =>
            {
                // Item actions share the `player_action` packet with a zeroed
                // position and a `down` face; only the action ordinal varies.
                let ordinal = match action {
                    ClientAction::DropSelectedItemStack => 3, // DROP_ALL_ITEMS
                    ClientAction::DropSelectedItem => 4,      // DROP_ITEM
                    ClientAction::ReleaseUseItem => 5,        // RELEASE_USE_ITEM
                    ClientAction::SwapItemWithOffhand => 6,   // SWAP_ITEM_WITH_OFFHAND
                    ClientAction::Stab => 7,                  // STAB
                    _ => unreachable!("guarded by the arm's pattern"),
                };
                let body = PlayerAction {
                    action: ordinal,
                    pos: 0,
                    direction: 0,
                    sequence: 0,
                };
                Ok(Some((
                    play::serverbound::PLAYER_ACTION,
                    encode_body(&body)?,
                )))
            }
            ClientAction::UseItemOn {
                hand,
                pos,
                face,
                cursor,
                inside_block,
                sequence,
            } if state == ConnectionState::Play => {
                let body = UseItemOn {
                    hand: hand_ordinal(*hand),
                    pos: pack_block_pos(*pos),
                    face: face_ordinal(*face),
                    cursor_x: cursor.x,
                    cursor_y: cursor.y,
                    cursor_z: cursor.z,
                    inside_block: *inside_block,
                    world_border_hit: false,
                    sequence: *sequence,
                };
                Ok(Some((play::serverbound::USE_ITEM_ON, encode_body(&body)?)))
            }
            ClientAction::UseItem {
                hand,
                rotation,
                sequence,
            } if state == ConnectionState::Play => {
                let body = UseItem {
                    hand: hand_ordinal(*hand),
                    sequence: *sequence,
                    yaw: rotation.yaw,
                    pitch: rotation.pitch,
                };
                Ok(Some((play::serverbound::USE_ITEM, encode_body(&body)?)))
            }
            ClientAction::InteractEntity {
                entity_id,
                interaction,
                sneaking,
            } if state == ConnectionState::Play => match interaction {
                // 26.2 splits attack into its own packet, which carries only the
                // entity id (no hand, location, or secondary-action flag).
                EntityInteraction::Attack => {
                    let body = Attack {
                        entity_id: *entity_id,
                    };
                    Ok(Some((play::serverbound::ATTACK, encode_body(&body)?)))
                }
                EntityInteraction::Interact { hand } => Ok(Some((
                    play::serverbound::INTERACT,
                    encode_interact(*entity_id, *hand, None, *sneaking),
                ))),
                EntityInteraction::InteractAt { hand, target } => Ok(Some((
                    play::serverbound::INTERACT,
                    encode_interact(*entity_id, *hand, Some(*target), *sneaking),
                ))),
            },
            ClientAction::SetPlayerInput(input) if state == ConnectionState::Play => {
                let PlayerInput {
                    forward,
                    backward,
                    left,
                    right,
                    jump,
                    shift,
                    sprint,
                } = input;
                let flags = u8::from(*forward)
                    | (u8::from(*backward) << 1)
                    | (u8::from(*left) << 2)
                    | (u8::from(*right) << 3)
                    | (u8::from(*jump) << 4)
                    | (u8::from(*shift) << 5)
                    | (u8::from(*sprint) << 6);
                let body = PlayerInputPacket { flags };
                Ok(Some((play::serverbound::PLAYER_INPUT, encode_body(&body)?)))
            }
            ClientAction::PlayerCommand { entity_id, command }
                if state == ConnectionState::Play =>
            {
                let (ordinal, data) = match command {
                    PlayerCommand::StopSleeping => (0, 0),
                    PlayerCommand::StartSprinting => (1, 0),
                    PlayerCommand::StopSprinting => (2, 0),
                    PlayerCommand::StartRidingJump { boost } => (3, *boost),
                    PlayerCommand::StopRidingJump => (4, 0),
                    PlayerCommand::OpenInventory => (5, 0),
                    PlayerCommand::StartFallFlying => (6, 0),
                };
                let body = PlayerCommandPacket {
                    entity_id: *entity_id,
                    action: ordinal,
                    data,
                };
                Ok(Some((
                    play::serverbound::PLAYER_COMMAND,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetCarriedItem { slot } if state == ConnectionState::Play => {
                let body = SetCarriedItem { slot: *slot as i16 };
                Ok(Some((
                    play::serverbound::SET_CARRIED_ITEM,
                    encode_body(&body)?,
                )))
            }
            ClientAction::ContainerClose { window_id } if state == ConnectionState::Play => {
                let body = ContainerClose {
                    window_id: *window_id,
                };
                Ok(Some((
                    play::serverbound::CONTAINER_CLOSE,
                    encode_body(&body)?,
                )))
            }
            ClientAction::ContainerClick {
                window_id,
                state_id,
                slot,
                button,
                click_type,
                changed_slots,
                carried_item,
            } if state == ConnectionState::Play => {
                let payload = encode_container_click(
                    *window_id,
                    *state_id,
                    *slot,
                    *button,
                    *click_type,
                    changed_slots,
                    carried_item.as_ref(),
                )?;
                Ok(Some((play::serverbound::CONTAINER_CLICK, payload)))
            }
            ClientAction::SetCreativeModeSlot { slot, item } if state == ConnectionState::Play => {
                let mut w = Writer::default();
                let slot_i16 = i16::try_from(*slot).map_err(|_| {
                    AdapterError::Encode(format!("creative slot {slot} overflows i16"))
                })?;
                w.i16(slot_i16);
                write_optional_item_stack(&mut w, item.as_ref())?;
                Ok(Some((
                    play::serverbound::SET_CREATIVE_MODE_SLOT,
                    w.into_vec(),
                )))
            }
            ClientAction::SetClientSettings(settings)
                if matches!(
                    state,
                    ConnectionState::Configuration | ConnectionState::Play
                ) =>
            {
                let ClientSettings {
                    locale,
                    view_distance,
                    chat_mode,
                    chat_colors,
                    skin_parts,
                    main_hand,
                    text_filtering,
                    allow_server_listing,
                    particle_status,
                } = settings;
                let body = ClientInformation {
                    language: locale.clone(),
                    view_distance: *view_distance,
                    chat_visibility: match chat_mode {
                        ChatMode::Full => 0,
                        ChatMode::CommandsOnly => 1,
                        ChatMode::Hidden => 2,
                    },
                    chat_colors: *chat_colors,
                    model_customization: skin_parts_bitmask(*skin_parts),
                    main_hand: match main_hand {
                        MainHand::Left => 0,
                        MainHand::Right => 1,
                    },
                    text_filtering: *text_filtering,
                    allows_listing: *allow_server_listing,
                    particle_status: match particle_status {
                        ParticleStatus::All => 0,
                        ParticleStatus::Decreased => 1,
                        ParticleStatus::Minimal => 2,
                    },
                };
                let packet_id = match state {
                    ConnectionState::Configuration => {
                        configuration::serverbound::CLIENT_INFORMATION
                    }
                    _ => play::serverbound::CLIENT_INFORMATION,
                };
                Ok(Some((packet_id, encode_body(&body)?)))
            }
            ClientAction::SendBrand { brand }
                if matches!(
                    state,
                    ConnectionState::Configuration | ConnectionState::Play
                ) =>
            {
                let body = BrandPayload {
                    channel: "minecraft:brand".to_owned(),
                    brand: brand.clone(),
                };
                let packet_id = match state {
                    ConnectionState::Configuration => configuration::serverbound::CUSTOM_PAYLOAD,
                    _ => play::serverbound::CUSTOM_PAYLOAD,
                };
                Ok(Some((packet_id, encode_body(&body)?)))
            }
            // Issue #301: the general case `SendBrand` above is vanilla's one
            // built-in instance of. `custom_payload`'s wire body is just
            // channel + raw bytes (`ClientboundCustomPayloadPacket`'s
            // `DiscardedPayload`, mirrored on the serverbound side), so this
            // needs no dedicated packet struct — `BrandPayload`'s two-string
            // shape doesn't fit arbitrary bytes, but `send` only needs an
            // `Encode` body, and a `(String, Vec<u8>)`-shaped write is exactly
            // what `custom_payload` is on every channel that isn't `brand`.
            ClientAction::SendCustomPayload { channel, data }
                if matches!(
                    state,
                    ConnectionState::Configuration | ConnectionState::Play
                ) =>
            {
                let mut writer = Writer::default();
                writer.string(&channel.to_string());
                writer.bytes(data);
                let packet_id = match state {
                    ConnectionState::Configuration => configuration::serverbound::CUSTOM_PAYLOAD,
                    _ => play::serverbound::CUSTOM_PAYLOAD,
                };
                Ok(Some((packet_id, writer.into_vec())))
            }
            ClientAction::PongResponse { id }
                if matches!(
                    state,
                    ConnectionState::Configuration | ConnectionState::Play
                ) =>
            {
                let body = Pong { id: *id };
                let packet_id = match state {
                    ConnectionState::Configuration => configuration::serverbound::PONG,
                    _ => play::serverbound::PONG,
                };
                Ok(Some((packet_id, encode_body(&body)?)))
            }
            ClientAction::ResourcePackResponse { id, response }
                if matches!(
                    state,
                    ConnectionState::Configuration | ConnectionState::Play
                ) =>
            {
                let body = ResourcePackResponse {
                    id: *id,
                    action: resource_pack_response_ordinal(*response),
                };
                let packet_id = match state {
                    ConnectionState::Configuration => configuration::serverbound::RESOURCE_PACK,
                    _ => play::serverbound::RESOURCE_PACK,
                };
                Ok(Some((packet_id, encode_body(&body)?)))
            }
            ClientAction::EndClientTick if state == ConnectionState::Play => Ok(Some((
                play::serverbound::CLIENT_TICK_END,
                encode_body(&ClientTickEnd)?,
            ))),
            ClientAction::ContainerButtonClick {
                window_id,
                button_id,
            } if state == ConnectionState::Play => {
                let body = ContainerButtonClick {
                    window_id: *window_id,
                    button_id: *button_id,
                };
                Ok(Some((
                    play::serverbound::CONTAINER_BUTTON_CLICK,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetFlying { flying } if state == ConnectionState::Play => {
                let flags = if *flying {
                    SERVERBOUND_ABILITY_FLAG_FLYING
                } else {
                    0
                };
                let body = ServerboundPlayerAbilities { flags };
                Ok(Some((
                    play::serverbound::PLAYER_ABILITIES,
                    encode_body(&body)?,
                )))
            }
            ClientAction::RenameItem { name } if state == ConnectionState::Play => {
                let body = RenameItem { name: name.clone() };
                Ok(Some((play::serverbound::RENAME_ITEM, encode_body(&body)?)))
            }
            ClientAction::SelectTrade { index } if state == ConnectionState::Play => {
                let body = SelectTrade { index: *index };
                Ok(Some((play::serverbound::SELECT_TRADE, encode_body(&body)?)))
            }
            ClientAction::PickItemFromBlock { pos, include_data }
                if state == ConnectionState::Play =>
            {
                let body = PickItemFromBlock {
                    pos: pack_block_pos(*pos),
                    include_data: *include_data,
                };
                Ok(Some((
                    play::serverbound::PICK_ITEM_FROM_BLOCK,
                    encode_body(&body)?,
                )))
            }
            ClientAction::PickItemFromEntity {
                entity_id,
                include_data,
            } if state == ConnectionState::Play => {
                let body = PickItemFromEntity {
                    entity_id: *entity_id,
                    include_data: *include_data,
                };
                Ok(Some((
                    play::serverbound::PICK_ITEM_FROM_ENTITY,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetBeaconEffects { primary, secondary }
                if state == ConnectionState::Play =>
            {
                let payload = encode_set_beacon(primary.as_ref(), secondary.as_ref())?;
                Ok(Some((play::serverbound::SET_BEACON, payload)))
            }
            ClientAction::EditBook { slot, pages, title } if state == ConnectionState::Play => {
                let body = EditBook {
                    slot: *slot,
                    pages: pages.clone(),
                    title: title.clone(),
                };
                Ok(Some((play::serverbound::EDIT_BOOK, encode_body(&body)?)))
            }
            ClientAction::SignUpdate {
                pos,
                is_front_text,
                lines,
            } if state == ConnectionState::Play => {
                let [line0, line1, line2, line3] = lines.clone();
                let body = SignUpdate {
                    pos: pack_block_pos(*pos),
                    is_front_text: *is_front_text,
                    line0,
                    line1,
                    line2,
                    line3,
                };
                Ok(Some((play::serverbound::SIGN_UPDATE, encode_body(&body)?)))
            }
            ClientAction::SetCommandBlock {
                pos,
                command,
                mode,
                track_output,
                conditional,
                automatic,
            } if state == ConnectionState::Play => {
                let flags = (if *track_output {
                    COMMAND_BLOCK_FLAG_TRACK_OUTPUT
                } else {
                    0
                }) | (if *conditional {
                    COMMAND_BLOCK_FLAG_CONDITIONAL
                } else {
                    0
                }) | (if *automatic {
                    COMMAND_BLOCK_FLAG_AUTOMATIC
                } else {
                    0
                });
                let body = SetCommandBlock {
                    pos: pack_block_pos(*pos),
                    command: command.clone(),
                    mode: command_block_mode_ordinal(*mode),
                    flags,
                };
                Ok(Some((
                    play::serverbound::SET_COMMAND_BLOCK,
                    encode_body(&body)?,
                )))
            }
            ClientAction::PlayerLoaded if state == ConnectionState::Play => Ok(Some((
                play::serverbound::PLAYER_LOADED,
                encode_body(&PlayerLoaded)?,
            ))),
            ClientAction::SeenAdvancements { tab } if state == ConnectionState::Play => Ok(Some((
                play::serverbound::SEEN_ADVANCEMENTS,
                encode_seen_advancements(tab.as_ref())?,
            ))),
            ClientAction::CommandSuggestion { id, command } if state == ConnectionState::Play => {
                let body = CommandSuggestion {
                    id: *id,
                    command: command.clone(),
                };
                Ok(Some((
                    play::serverbound::COMMAND_SUGGESTION,
                    encode_body(&body)?,
                )))
            }
            ClientAction::PaddleBoat { left, right } if state == ConnectionState::Play => {
                let body = PaddleBoat {
                    left: *left,
                    right: *right,
                };
                Ok(Some((play::serverbound::PADDLE_BOAT, encode_body(&body)?)))
            }
            ClientAction::MoveVehicle {
                pos,
                rotation,
                on_ground,
            } if state == ConnectionState::Play => {
                let body = MoveVehicle {
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                    yaw: rotation.yaw,
                    pitch: rotation.pitch,
                    on_ground: *on_ground,
                };
                Ok(Some((play::serverbound::MOVE_VEHICLE, encode_body(&body)?)))
            }
            ClientAction::SelectBundleItem {
                slot_id,
                selected_item_index,
            } if state == ConnectionState::Play => {
                let body = SelectBundleItem {
                    slot_id: *slot_id,
                    selected_item_index: *selected_item_index,
                };
                Ok(Some((
                    play::serverbound::BUNDLE_ITEM_SELECTED,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetContainerSlotState {
                slot_id,
                container_id,
                new_state,
            } if state == ConnectionState::Play => {
                let body = ContainerSlotStateChanged {
                    slot_id: *slot_id,
                    container_id: *container_id,
                    new_state: *new_state,
                };
                Ok(Some((
                    play::serverbound::CONTAINER_SLOT_STATE_CHANGED,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetRecipeBookSettings {
                book_type,
                open,
                filtering,
            } if state == ConnectionState::Play => {
                let body = RecipeBookChangeSettings {
                    book_type: recipe_book_type_to_ordinal(*book_type),
                    is_open: *open,
                    is_filtering: *filtering,
                };
                Ok(Some((
                    play::serverbound::RECIPE_BOOK_CHANGE_SETTINGS,
                    encode_body(&body)?,
                )))
            }
            ClientAction::RecipeBookSeenRecipe { recipe } if state == ConnectionState::Play => {
                let body = RecipeBookSeenRecipe { recipe: *recipe };
                Ok(Some((
                    play::serverbound::RECIPE_BOOK_SEEN_RECIPE,
                    encode_body(&body)?,
                )))
            }
            ClientAction::PlaceRecipe {
                container_id,
                recipe,
                use_max_items,
            } if state == ConnectionState::Play => {
                let body = PlaceRecipe {
                    container_id: *container_id,
                    recipe: *recipe,
                    use_max_items: *use_max_items,
                };
                Ok(Some((play::serverbound::PLACE_RECIPE, encode_body(&body)?)))
            }
            // Play-state only: the identically-shaped status-state ping is
            // driven by the ping flow, not by a canonical client action.
            ClientAction::PingRequest { time } if state == ConnectionState::Play => {
                let body = PingRequest { time: *time };
                Ok(Some((
                    play::serverbound::PING_REQUEST,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SpectatorAction { target_entity_id } if state == ConnectionState::Play => {
                Ok(Some((
                    play::serverbound::SPECTATOR_ACTION,
                    encode_spectator_action(*target_entity_id)?,
                )))
            }
            ClientAction::TeleportToEntity { target } if state == ConnectionState::Play => {
                let body = TeleportToEntity { uuid: *target };
                Ok(Some((
                    play::serverbound::TELEPORT_TO_ENTITY,
                    encode_body(&body)?,
                )))
            }
            ClientAction::ChangeGameMode { mode } if state == ConnectionState::Play => {
                let body = ChangeGameMode {
                    mode: game_mode_to_ordinal(*mode),
                };
                Ok(Some((
                    play::serverbound::CHANGE_GAME_MODE,
                    encode_body(&body)?,
                )))
            }
            // Issue #291: `cookie_response` exists in Login, Configuration and
            // Play alike (`ServerCookiePacketListener` is common to all
            // three), so this is one arm with a per-state packet id rather
            // than three separate ones.
            ClientAction::CookieResponse { key, payload } => {
                let packet_id = match state {
                    ConnectionState::Login => login::serverbound::COOKIE_RESPONSE,
                    ConnectionState::Configuration => configuration::serverbound::COOKIE_RESPONSE,
                    ConnectionState::Play => play::serverbound::COOKIE_RESPONSE,
                    ConnectionState::Handshaking | ConnectionState::Status => return Ok(None),
                };
                let body = CookieResponse {
                    key: key.to_string(),
                    payload: payload.clone(),
                };
                Ok(Some((packet_id, encode_body(&body)?)))
            }

            // ---- issue #304: the operator/debug set -------------------------
            ClientAction::QueryBlockEntityTag {
                transaction_id,
                pos,
            } if state == ConnectionState::Play => {
                let body = BlockEntityTagQuery {
                    transaction_id: *transaction_id,
                    pos: pack_block_pos(*pos),
                };
                Ok(Some((
                    play::serverbound::BLOCK_ENTITY_TAG_QUERY,
                    encode_body(&body)?,
                )))
            }
            ClientAction::QueryEntityTag {
                transaction_id,
                entity_id,
            } if state == ConnectionState::Play => {
                let body = EntityTagQuery {
                    transaction_id: *transaction_id,
                    entity_id: *entity_id,
                };
                Ok(Some((
                    play::serverbound::ENTITY_TAG_QUERY,
                    encode_body(&body)?,
                )))
            }
            ClientAction::ChangeDifficulty { difficulty } if state == ConnectionState::Play => {
                let mut w = Writer::default();
                w.var_i32(difficulty_id(*difficulty));
                Ok(Some((
                    play::serverbound::CHANGE_DIFFICULTY,
                    w.into_vec(),
                )))
            }
            ClientAction::LockDifficulty { locked } if state == ConnectionState::Play => {
                let mut w = Writer::default();
                w.bool(*locked);
                Ok(Some((play::serverbound::LOCK_DIFFICULTY, w.into_vec())))
            }
            ClientAction::SetGameRules { entries } if state == ConnectionState::Play => Ok(Some((
                play::serverbound::SET_GAME_RULE,
                encode_set_game_rules(entries)?,
            ))),
            ClientAction::SetCommandMinecart {
                entity_id,
                command,
                track_output,
            } if state == ConnectionState::Play => {
                let mut w = Writer::default();
                w.var_i32(*entity_id);
                w.string(command);
                w.bool(*track_output);
                Ok(Some((
                    play::serverbound::SET_COMMAND_MINECART,
                    w.into_vec(),
                )))
            }
            ClientAction::SetStructureBlock {
                pos,
                update_type,
                mode,
                name,
                offset,
                size,
                mirror,
                rotation,
                data,
                integrity,
                seed,
                ignore_entities,
                show_air,
                show_bounding_box,
                strict,
            } if state == ConnectionState::Play => {
                // `ServerboundSetStructureBlockPacket.write`'s flag bits, in the
                // order the read side unpacks them.
                let flags = u8::from(*ignore_entities)
                    | (u8::from(*show_air) << 1)
                    | (u8::from(*show_bounding_box) << 2)
                    | (u8::from(*strict) << 3);
                Ok(Some((
                    play::serverbound::SET_STRUCTURE_BLOCK,
                    encode_set_structure_block(
                        *pos,
                        *update_type,
                        *mode,
                        name,
                        *offset,
                        *size,
                        *mirror,
                        *rotation,
                        data,
                        *integrity,
                        *seed,
                        flags,
                    )?,
                )))
            }
            ClientAction::SetJigsawBlock {
                pos,
                name,
                target,
                pool,
                final_state,
                joint,
                selection_priority,
                placement_priority,
            } if state == ConnectionState::Play => Ok(Some((
                play::serverbound::SET_JIGSAW_BLOCK,
                encode_set_jigsaw_block(
                    *pos,
                    name,
                    target,
                    pool,
                    final_state,
                    *joint,
                    *selection_priority,
                    *placement_priority,
                )?,
            ))),
            ClientAction::GenerateJigsawStructure {
                pos,
                levels,
                keep_jigsaws,
            } if state == ConnectionState::Play => {
                let mut w = Writer::default();
                w.i64(pack_block_pos(*pos));
                w.var_i32(*levels);
                w.bool(*keep_jigsaws);
                Ok(Some((play::serverbound::JIGSAW_GENERATE, w.into_vec())))
            }
            ClientAction::SetTestBlock { pos, mode, message }
                if state == ConnectionState::Play =>
            {
                let mut w = Writer::default();
                w.i64(pack_block_pos(*pos));
                w.var_i32(match mode {
                    ModelTestBlockMode::Start => 0,
                    ModelTestBlockMode::Log => 1,
                    ModelTestBlockMode::Fail => 2,
                    ModelTestBlockMode::Accept => 3,
                });
                w.string(message);
                Ok(Some((play::serverbound::SET_TEST_BLOCK, w.into_vec())))
            }
            ClientAction::TestInstanceBlockAction { pos, action, data }
                if state == ConnectionState::Play =>
            {
                Ok(Some((
                    play::serverbound::TEST_INSTANCE_BLOCK_ACTION,
                    encode_test_instance_block_action(*pos, *action, data)?,
                )))
            }
            ClientAction::SubscribeDebug { subscriptions } if state == ConnectionState::Play => {
                Ok(Some((
                    play::serverbound::DEBUG_SUBSCRIPTION_REQUEST,
                    encode_debug_subscription_request(subscriptions)?,
                )))
            }
            // Present in Configuration and Play alike: `custom_click_action` is a
            // `ServerCommonPacketListener` packet, like `custom_payload` itself,
            // because `show_dialog` can be sent in either state.
            ClientAction::CustomClickAction { id, payload } => {
                let packet_id = match state {
                    ConnectionState::Configuration => {
                        configuration::serverbound::CUSTOM_CLICK_ACTION
                    }
                    ConnectionState::Play => play::serverbound::CUSTOM_CLICK_ACTION,
                    ConnectionState::Handshaking
                    | ConnectionState::Status
                    | ConnectionState::Login => return Ok(None),
                };
                Ok(Some((packet_id, encode_custom_click_action(id, payload)?)))
            }
            _ => Ok(None),
        }
    }
    fn build_encryption_response(
        &self,
        encrypted_secret: &[u8],
        encrypted_token: &[u8],
    ) -> Result<Directive, AdapterError> {
        // Both inputs are already RSA ciphertext from the driver; we only frame
        // them as the version's two-byte-array `key` packet.
        send(
            login::serverbound::KEY,
            &EncryptionResponse {
                shared_secret: encrypted_secret.to_vec(),
                verify_token: encrypted_token.to_vec(),
            },
        )
    }

    fn entity_dimensions(&self, entity_type_id: i32) -> Option<EntityBaseDimensions> {
        // The base hitbox census is 26.2 game data homed in `lodestone-data`
        // (issue #361); the registry seam reaches it through here so a
        // version-free consumer never names v770 or the data crate directly.
        // Base dims only — the caller folds SCALE/STEP_HEIGHT from the
        // entity's attribute map.
        lodestone_data::entity_dimensions::base_dimensions(entity_type_id)
    }

    fn entity_facts(&self, entity_type: &ResourceKey) -> Option<EntityFacts> {
        // The same two censuses `entity_dimensions` and `entity_census` expose,
        // read by resource key instead of wire id — which is the only identity a
        // consumer downstream of ingest still holds. Both lookups are indexed by
        // the same id, so resolving the key once serves both, and a type outside
        // either census misses whole rather than half-answering.
        let id = lodestone_data::entity_types::entity_type_id_parts(
            entity_type.namespace(),
            entity_type.path(),
        )?;
        Some(EntityFacts {
            dimensions: lodestone_data::entity_dimensions::base_dimensions(id)?,
            pushes_players: lodestone_data::entity_census::pushes_players(id)?,
        })
    }

    fn block_hardness(&self, state_id: u32) -> Option<BlockHardness> {
        // The per-block-state hardness census is 26.2 game data homed in
        // `lodestone-data` (issue #361); the registry seam reaches it through
        // here so a version-free consumer never names v770 or the data crate
        // directly. `requires_correct_tool` is the *block's* requirement, not
        // the player's tool match — see `BlockHardness`.
        lodestone_data::hardness::hardness(state_id).map(|entry| BlockHardness {
            hardness: entry.hardness,
            requires_correct_tool: entry.requires_correct_tool,
        })
    }

    fn tool_mining(&self, held: Option<&ItemStack>, state_id: u32) -> Option<ToolMining> {
        // The `minecraft:tool` census — item prototypes, block tag membership,
        // and the block-state→block-registry map — is 26.2 game data homed in
        // `lodestone-data` (issue #361); the registry seam reaches it through
        // here so a version-free consumer never names v770 or the data crate
        // directly. The returned `correct_tool` is already
        // `Player.hasCorrectToolForDrops`, block requirement folded in, so the
        // caller has nothing left to invert.
        lodestone_data::tool::mining(held, state_id)
    }

    fn block_collision(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        // The per-block-state collision census is 26.2 game data homed in
        // `lodestone-data` (issue #361; dumped from the real 26.2 server's
        // `Block.BLOCK_STATE_REGISTRY`); the registry seam reaches it through
        // here so a version-free consumer never names v770 or the data crate
        // directly. Zero-copy: `collision_shapes::Aabb` *is* `BlockAabb`, so
        // this hands back the rodata slice itself.
        lodestone_data::collision_shapes::collision_boxes(state_id)
    }

    fn block_name(&self, state_id: u32) -> Option<&'static str> {
        // Block *name* for a block-*state* id, from the same generated table the
        // asset baker resolves properties through. `&'static str` out of rodata,
        // O(1), no instance and no allocation — the physics seam calls this for
        // the block under the player every tick.
        lodestone_data::block_states::block_name(state_id)
    }

    fn block_outline(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        // `BlockStateBase.getShape` — the shape `Entity.pick` clips against, and
        // a third thing beside collision and fluid presence. 26.2 game data
        // homed in `lodestone-data` (issue #361); zero-copy out of rodata. See
        // `lodestone_data::outline_shapes` for why half of all states disagree
        // with `block_collision`.
        lodestone_data::outline_shapes::outline_boxes(state_id)
    }

    fn block_interaction(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        // `BlockStateBase.getInteractionShape` — empty for all but four block
        // families, and a *face* refinement on top of the outline hit rather than
        // a clip target of its own.
        lodestone_data::outline_shapes::interaction_boxes(state_id)
    }

    fn item_prototype(&self, item: &str) -> Option<ItemPrototype> {
        // The item-prototype census (`minecraft:max_stack_size`,
        // `minecraft:max_damage`, `minecraft:equippable`) is 26.2 game data
        // homed in `lodestone-data` (issue #361), because a clientbound stack
        // carries only the *patch* against it and so none of the three is
        // ever on the wire. Stacks decoded
        // by this adapter already have these folded into
        // `ItemComponents`' effective fields; this seam is for callers with no
        // stack in hand.
        lodestone_data::item_prototypes::model_prototype(item)
    }

    fn block_blocks_motion(&self, state_id: u32) -> Option<bool> {
        // `BlockState.blocksMotion()`, dumped per state rather than derived from
        // `block_collision`: `calculateSolid`'s first three branches
        // (`forceSolidOn` on 237 blocks, `forceSolidOff` on 8, and a null shape
        // cache on the 23 `dynamicShape()` blocks) are invisible to any shape
        // table, and skipping them is wrong for 2,618 of 32,366 states. One bit
        // out of rodata. See `lodestone_data::block_solidity`.
        lodestone_data::block_solidity::blocks_motion(state_id)
    }

    fn block_bubble_column_drag(&self, state_id: u32) -> Option<bool> {
        // Read straight off the generated state table rather than through a bespoke
        // census: `drag` is a real blockstate property that Mojang's own
        // `blocks.json` carries, so `properties()` already has it and there is
        // nothing to dump. The two states are 15294 (`drag=true`, the block's
        // default) and 15295 (`drag=false`) in the 26.2 palette.
        //
        // The name is checked first because `drag` is not guaranteed unique to this
        // block across versions, and matching on the property alone would silently
        // widen if another block ever gained one.
        if lodestone_data::block_states::block_name(state_id)? != "minecraft:bubble_column" {
            return None;
        }
        lodestone_data::block_states::properties(state_id)?
            .iter()
            .find(|(name, _)| *name == "drag")
            .map(|(_, value)| *value == "true")
    }
}
