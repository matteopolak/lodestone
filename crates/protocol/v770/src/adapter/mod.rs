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
mod chunk;
mod connection;
mod entity;
mod inventory;
mod player;
mod scoreboard;

// Re-exported so `crate::adapter::{game_mode_from_ordinal, game_mode_to_ordinal,
// DecodedStack, read_item_stack}` keep resolving after the split — `server_protocol.rs`
// and `packets/metadata.rs` depend on those exact paths.
pub(crate) use chunk::game_mode_from_ordinal;
pub(crate) use inventory::{DecodedStack, read_item_stack};

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

/// Parses a `minecraft:*` identifier into a canonical [`ResourceKey`],
/// attributing a decode error to `what` on failure.
fn parse_key(name: &str, what: &str) -> Result<ResourceKey, AdapterError> {
    name.parse()
        .map_err(|_| AdapterError::Decode(format!("invalid {what} key {name}")))
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
        directives.extend(self.handle_play_player(packet_id, payload)?);
        directives.extend(self.handle_play_inventory(packet_id, payload)?);
        directives.extend(self.handle_play_entity(packet_id, payload)?);
        directives.extend(self.handle_play_chunk(world, packet_id, payload)?);
        return Ok(directives);
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
