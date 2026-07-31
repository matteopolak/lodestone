//! [`VersionAdapter`] implementation driving the protocol 776 join flow.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer, read_network_nbt};
use lodestone_model::{
    AdapterError, AnimationAction, BlockAabb, BlockActionKind, BlockFace, BlockHardness, BlockPos,
    BossAction,
    BossColor,
    BossOverlay, ChatAckInfo, ChatKind, ChatMode, ChunkPos, ClientAction, ClientEvent,
    ClientSettings, CollisionRule, CommandBlockMode, ConnectionState, ContainerClickType,
    ContainerSlotChange, DeathLocation, Difficulty, Directive, DisplaySlot, DisplayedSkinParts,
    EntityBaseDimensions,
    EntityEquipment,
    EntityFacts,
    EntityInteraction, EntityMovement, EquipmentSlot, GameMode, Hand, ItemComponents,
    ItemEnchantment, ItemPrototype, ItemStack, ItemTool, LoginProfile,
    LookAnchor, MainHand, NumberFormat, ObjectiveMode, ObjectiveRenderType, PackedMessageSignature,
    ParticleStatus, PlayerCommand, PlayerInput, PlayerListEntry, PlayerLookAtEntity,
    RecipeBookType,
    ResourceKey, ResourcePackResponseKind, Rotation, SectionPos, ServerAddress, SoundCategory,
    TeamAction, TeamColor, TeamParameters, TeleportFlags, Text, TextColor, ToolBlocks, ToolMining,
    ToolPatch, ToolRule, Vec3, Vec3f, VersionAdapter, Visibility, WorldSink,
};
use lodestone_world::{ChunkPos as WorldChunkPos, LightPatch, LoadedChunk, NibbleArray};

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
    BrandPayload, ClientInformation, KeepAlive, PingRequest, Pong, ResourcePackResponse,
    TeleportToEntity,
};
use crate::packets::configuration::{
    AcceptCodeOfConduct, FinishConfiguration, ServerboundKnownPacks,
};
use crate::packets::entity::{read_lp_vec3, unpack_degrees};
use crate::packets::game::{
    ABILITY_FLAG_CAN_FLY, ABILITY_FLAG_FLYING, ABILITY_FLAG_INSTABUILD, ABILITY_FLAG_INVULNERABLE,
    AcceptTeleportation, Attack, COMMAND_BLOCK_FLAG_AUTOMATIC, COMMAND_BLOCK_FLAG_CONDITIONAL,
    COMMAND_BLOCK_FLAG_TRACK_OUTPUT, ChangeGameMode, ChatAck, ChatCommand, ChatMessage,
    ChunkBatchFinished, ChunkBatchReceived, ClientCommand, ClientTickEnd, CommandSuggestion,
    ConfigurationAcknowledged, ContainerButtonClick, ContainerClose, ContainerSlotStateChanged,
    EditBook, GameEvent, GameLogin, LevelEvent, LevelParticles, MOVE_FLAG_HORIZONTAL_COLLISION,
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
    EncryptionRequest, EncryptionResponse, LoginAcknowledged, LoginCompression, LoginDisconnect,
    LoginFinished,
};
use crate::packets::metadata::{
    MetadataClass, metadata_class, read_entity_metadata, read_update_attributes,
};
use crate::packets::player_info::{PlayerInfoRemove, PlayerInfoUpdate};
use crate::packets::scoreboard::{
    self as sb, BossEvent, ResetScore, SetDisplayObjective, SetObjective, SetPlayerTeam, SetScore,
};
use crate::packets::time::SetTime;
use lodestone_data::particle_types::particle_type_name;
use lodestone_data::sound_events::sound_event;

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
    variants: Arc<Mutex<HashMap<i32, MetadataClass>>>,
    /// The overworld day clock, held across packets because `set_time` mostly
    /// does **not** carry it. See [`DayClock`].
    clock: Arc<Mutex<DayClock>>,
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
    batch_start: Instant,
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
                batch_start: Instant::now(),
            })),
            movement: Arc::new(Mutex::new(MovementSendState::default())),
            variants: Arc::new(Mutex::new(HashMap::new())),
            clock: Arc::new(Mutex::new(DayClock::default())),
        }
    }

    /// Records the start of a chunk batch so its duration can be measured when
    /// the matching `chunk_batch_finished` arrives.
    fn begin_chunk_batch(&self) {
        if let Ok(mut state) = self.batch.lock() {
            state.batch_start = Instant::now();
        }
    }

    /// Folds the finished batch into the rate estimator and returns the desired
    /// chunks-per-tick rate to acknowledge with.
    fn finish_chunk_batch(&self, batch_size: i32) -> f32 {
        match self.batch.lock() {
            Ok(mut state) => {
                let duration_nanos = state.batch_start.elapsed().as_nanos() as f64;
                state
                    .calculator
                    .on_batch_finished(batch_size, duration_nanos);
                state.calculator.desired_chunks_per_tick()
            }
            Err(_) => ChunkBatchSizeCalculator::new().desired_chunks_per_tick(),
        }
    }

    /// Records the chunk shape for `dimension` so subsequent chunk packets in
    /// that dimension decode against the correct build-height window.
    fn set_dimension(&self, dimension: &str) {
        if let Ok(mut shape) = self.shape.lock() {
            *shape = ChunkShape::for_dimension(dimension);
        }
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
fn encode_body<T: Encode>(packet: &T) -> Result<Vec<u8>, AdapterError> {
    let mut writer = Writer::default();
    packet
        .encode(&mut writer, CTX)
        .map_err(|err| AdapterError::Encode(err.to_string()))?;
    Ok(writer.into_vec())
}

/// Decodes a packet body from raw bytes.
fn decode_body<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    let mut reader = Reader::new(payload);
    T::decode(&mut reader, CTX).map_err(|err| AdapterError::Decode(err.to_string()))
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
fn game_mode_from_ordinal(ordinal: i32) -> Option<GameMode> {
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
fn game_mode_to_ordinal(mode: GameMode) -> i32 {
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

/// Consumes a packed `LastSeenMessages` collection: a VarInt count (capped at
/// 20 by vanilla) then that many packed message signatures. Each packed
/// signature is a VarInt: `0` is followed by a full 256-byte signature (a
/// newly-seen message), and any positive value references a cached signature by
/// index and carries no further bytes.
fn read_last_seen_packed(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid last-seen count {count}")))?;
    for _ in 0..count {
        if reader.var_i32().map_err(dec_err)? == 0 {
            reader.bytes(256).map_err(dec_err)?;
        }
    }
    Ok(())
}

/// Reads a `FilterMask` and returns whether the message is shown to the player.
///
/// Ordinal: `0` = pass-through (shown), `1` = fully filtered (hidden), `2` =
/// partially filtered (shown) followed by a `BitSet` of filtered word indices
/// (a VarInt long-count then that many `i64` words).
fn read_filter_mask(reader: &mut Reader<'_>) -> Result<bool, AdapterError> {
    let ordinal = reader.var_i32().map_err(dec_err)?;
    match ordinal {
        0 => Ok(true),
        1 => Ok(false),
        2 => {
            let words = reader.var_i32().map_err(dec_err)?;
            let words = usize::try_from(words).map_err(|_| {
                AdapterError::Decode(format!("invalid filter mask bitset length {words}"))
            })?;
            for _ in 0..words {
                reader.i64().map_err(dec_err)?;
            }
            Ok(true)
        }
        other => Err(AdapterError::Decode(format!(
            "invalid filter mask ordinal {other}"
        ))),
    }
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

/// Consumes a `ChatType.Bound`: a `Holder<ChatType>`, a trusted NBT name
/// component, and an optional trusted NBT target-name component.
///
/// The holder is a VarInt: `0` would introduce an inline chat-type definition
/// (decoration plus style), which vanilla servers never send in chat packets
/// and which Phase 1 does not model; any positive value references the
/// `minecraft:chat_type` registry at index `value - 1` and carries no further
/// bytes. An inline holder fails loudly rather than misparsing the rest of the
/// stream.
fn read_chat_type_bound(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    if reader.var_i32().map_err(dec_err)? == 0 {
        return Err(AdapterError::Decode(
            "inline chat_type definitions are not supported".to_owned(),
        ));
    }
    read_network_nbt(reader).map_err(dec_err)?;
    if reader.bool().map_err(dec_err)? {
        read_network_nbt(reader).map_err(dec_err)?;
    }
    Ok(())
}

/// Parses a `minecraft:*` identifier into a canonical [`ResourceKey`],
/// attributing a decode error to `what` on failure.
fn parse_key(name: &str, what: &str) -> Result<ResourceKey, AdapterError> {
    name.parse()
        .map_err(|_| AdapterError::Decode(format!("invalid {what} key {name}")))
}

/// Outcome of decoding one clientbound item stack.
pub(crate) struct DecodedStack {
    /// The decoded stack, or `None` for the empty stack.
    pub(crate) stack: Option<ItemStack>,
    /// `false` when an unmodeled component halted decoding partway through the
    /// stack's `DataComponentPatch`, leaving the reader positioned mid-patch.
    ///
    /// The patch codec length-prefixes neither the patch nor its individual
    /// components (26.2 `DataComponentPatch.STREAM_CODEC`, the trusted variant
    /// clientbound stacks use), so an unrecognised component cannot be skipped
    /// in place. When this is `false`, the modeled fields that were decoded are
    /// still valid, but the remainder of the packet is unreadable and callers
    /// must stop reading further items and skip the trailing-bytes check rather
    /// than raising a fatal decode error.
    pub(crate) complete: bool,
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
        return Ok(DecodedStack {
            stack: None,
            complete: true,
        });
    }
    let item_id = reader.var_i32().map_err(dec_err)?;
    let name = item_name(item_id)
        .ok_or_else(|| AdapterError::Decode(format!("unknown item registry id {item_id}")))?;
    let count = u32::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid item count {count}")))?;
    let (components, complete) = read_component_patch(reader, name)?;
    Ok(DecodedStack {
        stack: Some(ItemStack {
            item: parse_key(name, "item")?,
            count,
            components,
        }),
        complete,
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
            other => {
                // An unmodeled component: its payload is not length-prefixed, so
                // it and everything after it in this packet are unreadable. Keep
                // the modeled fields decoded so far, flag the stack, and stop —
                // the packet is dropped past this point, not fatal.
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
    let decoded = read_item_stack(reader)?;
    if decoded.complete {
        reader.ensure_empty().map_err(dec_err)?;
    }
    Ok(decoded.stack)
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

/// Decodes an NBT text-component disconnect reason into plain text, falling back
/// to a generic message when the component carries no text.
fn nbt_reason_text(payload: &[u8]) -> Result<Text, AdapterError> {
    let mut reader = Reader::new(payload);
    let component =
        read_network_nbt(&mut reader).map_err(|err| AdapterError::Decode(err.to_string()))?;
    let reason = Text::from_nbt(&component);
    if reason.to_plain_string().is_empty() {
        Ok(Text::literal("Disconnected"))
    } else {
        Ok(reason)
    }
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

/// Decodes a play packet body and asserts zero trailing bytes, returning the
/// value. Zero trailing bytes is the misparse detector: a wrong conditional
/// branch consuming the wrong byte count is caught here rather than silently
/// corrupting the following packet.
fn decode_play<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX).map_err(|err| AdapterError::Decode(err.to_string()))?;
    reader
        .ensure_empty()
        .map_err(|err| AdapterError::Decode(err.to_string()))?;
    Ok(value)
}

fn map_objective_mode(method: u8) -> Result<ObjectiveMode, AdapterError> {
    Ok(match method {
        0 => ObjectiveMode::Add,
        1 => ObjectiveMode::Remove,
        2 => ObjectiveMode::Change,
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown objective mode {other}"
            )));
        }
    })
}

fn map_render_type(id: i32) -> Result<ObjectiveRenderType, AdapterError> {
    Ok(match id {
        0 => ObjectiveRenderType::Integer,
        1 => ObjectiveRenderType::Hearts,
        other => return Err(AdapterError::Decode(format!("unknown render type {other}"))),
    })
}

/// Lowers the wire number format into the canonical model form. The wire
/// `styled` variant carries a full `Style` (decoded into a `Text`); the model
/// keeps only the colour, so it is extracted, defaulting to white when absent.
fn map_number_format(nf: sb::NumberFormat) -> NumberFormat {
    match nf {
        sb::NumberFormat::Blank => NumberFormat::Blank,
        sb::NumberFormat::Styled(text) => {
            NumberFormat::Styled(text.style.color.unwrap_or(TextColor::White))
        }
        sb::NumberFormat::Fixed(text) => NumberFormat::Fixed(Box::new(text)),
    }
}

fn map_team_color(id: i32) -> Result<TeamColor, AdapterError> {
    Ok(match id {
        0 => TeamColor::Black,
        1 => TeamColor::DarkBlue,
        2 => TeamColor::DarkGreen,
        3 => TeamColor::DarkAqua,
        4 => TeamColor::DarkRed,
        5 => TeamColor::DarkPurple,
        6 => TeamColor::Gold,
        7 => TeamColor::Gray,
        8 => TeamColor::DarkGray,
        9 => TeamColor::Blue,
        10 => TeamColor::Green,
        11 => TeamColor::Aqua,
        12 => TeamColor::Red,
        13 => TeamColor::LightPurple,
        14 => TeamColor::Yellow,
        15 => TeamColor::White,
        other => return Err(AdapterError::Decode(format!("unknown team color {other}"))),
    })
}

fn map_display_slot(id: i32) -> Result<DisplaySlot, AdapterError> {
    Ok(match id {
        0 => DisplaySlot::List,
        1 => DisplaySlot::Sidebar,
        2 => DisplaySlot::BelowName,
        3..=18 => DisplaySlot::TeamSidebar(map_team_color(id - 3)?),
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown display slot {other}"
            )));
        }
    })
}

fn map_visibility(id: i32) -> Result<Visibility, AdapterError> {
    Ok(match id {
        0 => Visibility::Always,
        1 => Visibility::Never,
        2 => Visibility::HideForOtherTeams,
        3 => Visibility::HideForOwnTeam,
        other => return Err(AdapterError::Decode(format!("unknown visibility {other}"))),
    })
}

fn map_collision_rule(id: i32) -> Result<CollisionRule, AdapterError> {
    Ok(match id {
        0 => CollisionRule::Always,
        1 => CollisionRule::Never,
        2 => CollisionRule::PushOtherTeams,
        3 => CollisionRule::PushOwnTeam,
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown collision rule {other}"
            )));
        }
    })
}

fn map_team_parameters(params: sb::TeamParameters) -> Result<TeamParameters, AdapterError> {
    let color = match params.color {
        Some(id) => Some(map_team_color(id)?),
        None => None,
    };
    Ok(TeamParameters {
        display_name: params.display_name,
        prefix: params.prefix,
        suffix: params.suffix,
        name_tag_visibility: map_visibility(params.name_tag_visibility)?,
        collision_rule: map_collision_rule(params.collision_rule)?,
        color,
        friendly_fire: params.friendly_fire,
        see_friendly_invisibles: params.see_friendly_invisibles,
    })
}

fn map_team_action(team: SetPlayerTeam) -> Result<TeamAction, AdapterError> {
    Ok(match team.method {
        0 => TeamAction::Create {
            params: Box::new(map_team_parameters(team.parameters.ok_or_else(|| {
                AdapterError::Decode("team create without parameters".into())
            })?)?),
            members: team.players,
        },
        1 => TeamAction::Remove,
        2 => TeamAction::Update {
            params: Box::new(map_team_parameters(team.parameters.ok_or_else(|| {
                AdapterError::Decode("team update without parameters".into())
            })?)?),
        },
        3 => TeamAction::AddMembers(team.players),
        4 => TeamAction::RemoveMembers(team.players),
        other => return Err(AdapterError::Decode(format!("unknown team method {other}"))),
    })
}

fn map_boss_color(id: i32) -> Result<BossColor, AdapterError> {
    Ok(match id {
        0 => BossColor::Pink,
        1 => BossColor::Blue,
        2 => BossColor::Red,
        3 => BossColor::Green,
        4 => BossColor::Yellow,
        5 => BossColor::Purple,
        6 => BossColor::White,
        other => return Err(AdapterError::Decode(format!("unknown boss color {other}"))),
    })
}

fn map_boss_overlay(id: i32) -> Result<BossOverlay, AdapterError> {
    Ok(match id {
        0 => BossOverlay::Progress,
        1 => BossOverlay::Notched6,
        2 => BossOverlay::Notched10,
        3 => BossOverlay::Notched12,
        4 => BossOverlay::Notched20,
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown boss overlay {other}"
            )));
        }
    })
}

fn map_boss_action(op: sb::BossOp) -> Result<BossAction, AdapterError> {
    Ok(match op {
        sb::BossOp::Add {
            title,
            progress,
            color,
            overlay,
            darken,
            music,
            fog,
        } => BossAction::Add {
            title: Box::new(title),
            progress,
            color: map_boss_color(color)?,
            overlay: map_boss_overlay(overlay)?,
            darken,
            music,
            fog,
        },
        sb::BossOp::Remove => BossAction::Remove,
        sb::BossOp::UpdateProgress(p) => BossAction::UpdateProgress(p),
        sb::BossOp::UpdateName(name) => BossAction::UpdateName(Box::new(name)),
        sb::BossOp::UpdateStyle { color, overlay } => BossAction::UpdateStyle {
            color: map_boss_color(color)?,
            overlay: map_boss_overlay(overlay)?,
        },
        sb::BossOp::UpdateProperties { darken, music, fog } => {
            BossAction::UpdateFlags { darken, music, fog }
        }
    })
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
    variants: &Mutex<HashMap<i32, MetadataClass>>,
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
    let _data = reader.var_i32().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;

    let name = entity_type_name(type_id).ok_or_else(|| {
        AdapterError::Decode(format!("unknown entity-type id {type_id} in add_entity"))
    })?;
    let entity_type = name.parse().map_err(|_| {
        AdapterError::Decode(format!(
            "entity-type id {type_id} is not a valid key: {name}"
        ))
    })?;

    // Remember the concrete type only for mobs whose variant index is ambiguous,
    // so a later `set_entity_data` can disambiguate it. Everything else is left
    // untracked; its variant (if any) resolves by serializer alone.
    if let Some(class) = metadata_class(name)
        && let Ok(mut map) = variants.lock()
    {
        map.insert(entity_id, class);
    }

    Ok(vec![
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
    ])
}

/// Decodes `remove_entities` (a VarInt-length list of VarInt ids) into a removal
/// event.
fn handle_remove_entities(
    payload: &[u8],
    variants: &Mutex<HashMap<i32, MetadataClass>>,
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
    variants: &Mutex<HashMap<i32, MetadataClass>>,
) -> Vec<Directive> {
    let mut reader = Reader::new(payload);
    let Ok(entity_id) = reader.var_i32() else {
        return Vec::new();
    };
    let class = variants
        .lock()
        .ok()
        .and_then(|map| map.get(&entity_id).copied());
    match read_entity_metadata(&mut reader, class) {
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

impl V770Adapter {
    /// Handles a clientbound packet while in the login state.
    fn handle_login(&self, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == login::clientbound::LOGIN_COMPRESSION {
            let body: LoginCompression = decode_body(payload)?;
            return Ok(vec![Directive::SetCompression(body.threshold)]);
        }
        if packet_id == login::clientbound::LOGIN_FINISHED {
            // Validate the profile decodes, then acknowledge and advance.
            let _profile: LoginFinished = decode_body(payload)?;
            return Ok(vec![
                send(login::serverbound::LOGIN_ACKNOWLEDGED, &LoginAcknowledged)?,
                Directive::SetState(ConnectionState::Configuration),
                send(
                    configuration::serverbound::CLIENT_INFORMATION,
                    &ClientInformation::default(),
                )?,
            ]);
        }
        if packet_id == login::clientbound::HELLO {
            let request: EncryptionRequest = decode_body(payload)?;
            // Hand the driver the protocol-shaped crypto inputs; it performs the
            // key exchange and session auth and asks us back to frame the reply.
            return Ok(vec![Directive::BeginEncryption {
                server_id: request.server_id,
                public_key: request.public_key,
                verify_token: request.challenge,
                should_authenticate: request.should_authenticate,
            }]);
        }
        if packet_id == login::clientbound::LOGIN_DISCONNECT {
            let body: LoginDisconnect = decode_body(payload)?;
            return Ok(vec![Directive::Disconnect(Text::literal(body.reason))]);
        }
        Ok(Vec::new())
    }

    /// Handles a clientbound packet while in the configuration state.
    fn handle_configuration(
        &self,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == configuration::clientbound::SELECT_KNOWN_PACKS {
            return Ok(vec![send(
                configuration::serverbound::SELECT_KNOWN_PACKS,
                &ServerboundKnownPacks { packs: Vec::new() },
            )?]);
        }
        if packet_id == configuration::clientbound::FINISH_CONFIGURATION {
            return Ok(vec![
                send(
                    configuration::serverbound::FINISH_CONFIGURATION,
                    &FinishConfiguration,
                )?,
                Directive::SetState(ConnectionState::Play),
            ]);
        }
        if packet_id == configuration::clientbound::KEEP_ALIVE {
            let keep_alive: KeepAlive = decode_body(payload)?;
            return Ok(vec![send(
                configuration::serverbound::KEEP_ALIVE,
                &keep_alive,
            )?]);
        }
        if packet_id == configuration::clientbound::PING {
            let ping: Pong = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::Ping { id: ping.id })]);
        }
        if packet_id == configuration::clientbound::CODE_OF_CONDUCT {
            return Ok(vec![send(
                configuration::serverbound::ACCEPT_CODE_OF_CONDUCT,
                &AcceptCodeOfConduct,
            )?]);
        }
        if packet_id == configuration::clientbound::DISCONNECT {
            return Ok(vec![Directive::Disconnect(nbt_reason_text(payload)?)]);
        }
        Ok(Vec::new())
    }

    /// Handles a clientbound packet while in the play state.
    fn handle_play(
        &self,
        world: &mut dyn WorldSink,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == play::clientbound::LOGIN {
            let body: GameLogin = decode_body(payload)?;
            self.set_dimension(&body.dimension);
            let dimension = body.dimension.parse().map_err(|_| {
                AdapterError::Decode(format!("invalid dimension {}", body.dimension))
            })?;
            return Ok(vec![Directive::Emit(ClientEvent::Login {
                entity_id: body.entity_id,
                game_mode: game_mode(body.game_type)?,
                dimension,
            })]);
        }
        if packet_id == play::clientbound::KEEP_ALIVE {
            let keep_alive: KeepAlive = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::KeepAlive {
                id: keep_alive.id,
            })]);
        }
        if packet_id == play::clientbound::PING {
            let ping: Pong = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::Ping { id: ping.id })]);
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
        if packet_id == play::clientbound::START_CONFIGURATION {
            // The server is pulling us back into configuration mid-session
            // (resource-pack/datapack reload, `transfer`). Acknowledge on the
            // play protocol, then switch state so subsequent packets decode as
            // configuration. The packet body is empty.
            Reader::new(payload).ensure_empty().map_err(dec_err)?;
            return Ok(vec![
                send(
                    play::serverbound::CONFIGURATION_ACKNOWLEDGED,
                    &ConfigurationAcknowledged,
                )?,
                Directive::SetState(ConnectionState::Configuration),
            ]);
        }
        if packet_id == play::clientbound::DISCONNECT {
            return Ok(vec![Directive::Disconnect(nbt_reason_text(payload)?)]);
        }
        if packet_id == play::clientbound::PLAYER_CHAT {
            let mut reader = Reader::new(payload);
            let global_index = reader.var_i32().map_err(dec_err)?;
            let _sender = reader.uuid().map_err(dec_err)?;
            let _index = reader.var_i32().map_err(dec_err)?;
            let signature = if reader.bool().map_err(dec_err)? {
                reader.bytes(256).map_err(dec_err)?.to_vec()
            } else {
                Vec::new()
            };
            // SignedMessageBody.Packed: raw content, timestamp, salt, last-seen.
            let content = reader.string(256).map_err(dec_err)?;
            let _timestamp = reader.i64().map_err(dec_err)?;
            let _salt = reader.i64().map_err(dec_err)?;
            read_last_seen_packed(&mut reader)?;
            let unsigned = if reader.bool().map_err(dec_err)? {
                Some(read_network_nbt(&mut reader).map_err(dec_err)?)
            } else {
                None
            };
            let was_shown = read_filter_mask(&mut reader)?;
            read_chat_type_bound(&mut reader)?;
            reader.ensure_empty().map_err(dec_err)?;
            // The server-decorated form (if any) is preferred for display; a
            // plain signed message carries only its raw content. The decorated
            // component keeps its colour/style tree; the raw content is a bare
            // string.
            let text = match unsigned {
                Some(component) => Text::from_nbt(&component),
                None => Text::literal(content),
            };
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text,
                kind: ChatKind::Chat,
                ack: Some(ChatAckInfo {
                    signature,
                    global_index,
                    was_shown,
                }),
            })]);
        }
        if packet_id == play::clientbound::DISGUISED_CHAT {
            let mut reader = Reader::new(payload);
            let component = read_network_nbt(&mut reader).map_err(dec_err)?;
            read_chat_type_bound(&mut reader)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text: Text::from_nbt(&component),
                kind: ChatKind::Chat,
                ack: None,
            })]);
        }
        if packet_id == play::clientbound::SYSTEM_CHAT {
            let mut reader = Reader::new(payload);
            let component = read_network_nbt(&mut reader)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let overlay = reader
                .bool()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let kind = if overlay {
                ChatKind::GameInfo
            } else {
                ChatKind::System
            };
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text: Text::from_nbt(&component),
                kind,
                ack: None,
            })]);
        }
        if packet_id == play::clientbound::SET_ACTION_BAR_TEXT {
            // The action bar carries a single trusted text component and always
            // renders as an overlay, so it maps to a `GameInfo` chat event.
            let mut reader = Reader::new(payload);
            let component = read_network_nbt(&mut reader)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text: Text::from_nbt(&component),
                kind: ChatKind::GameInfo,
                ack: None,
            })]);
        }
        if packet_id == play::clientbound::SET_TITLE_TEXT {
            let mut reader = Reader::new(payload);
            let component = read_network_nbt(&mut reader).map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TitleText {
                text: Text::from_nbt(&component),
            })]);
        }
        if packet_id == play::clientbound::SET_SUBTITLE_TEXT {
            let mut reader = Reader::new(payload);
            let component = read_network_nbt(&mut reader).map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::SubtitleText {
                text: Text::from_nbt(&component),
            })]);
        }
        if packet_id == play::clientbound::CLEAR_TITLES {
            let mut reader = Reader::new(payload);
            let reset_times = reader.bool().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TitlesCleared {
                reset_times,
            })]);
        }
        if packet_id == play::clientbound::SET_TITLES_ANIMATION {
            // All three fields are raw `int`s (`readInt`), not VarInts.
            let mut reader = Reader::new(payload);
            let fade_in = reader.i32().map_err(dec_err)?;
            let stay = reader.i32().map_err(dec_err)?;
            let fade_out = reader.i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TitlesAnimation {
                fade_in,
                stay,
                fade_out,
            })]);
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
                let decoded = read_item_stack(&mut reader)?;
                items.push(decoded.stack);
                if !decoded.complete {
                    // An unmodeled component desynced the stream; the remaining
                    // list entries and carried item are unreadable. Deliver what
                    // decoded and drop the rest of the packet.
                    complete = false;
                    break;
                }
            }
            let carried_item = if complete {
                let decoded = read_item_stack(&mut reader)?;
                complete = decoded.complete;
                decoded.stack
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
                equipment.push(EntityEquipment {
                    slot,
                    item: decoded.stack,
                });
                if !decoded.complete {
                    // An unmodeled component desynced the stream; further list
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
        if packet_id == play::clientbound::SET_OBJECTIVE {
            // Conditional body: the display-name/render-type/number-format tail
            // is present only for add(0)/change(2), absent for remove(1). A wrong
            // branch leaves trailing bytes, which ensure_empty rejects.
            let obj: SetObjective = decode_play(payload)?;
            let render_type = match obj.render_type {
                Some(id) => Some(map_render_type(id)?),
                None => None,
            };
            return Ok(vec![Directive::Emit(ClientEvent::ObjectiveUpdate {
                name: obj.name,
                mode: map_objective_mode(obj.method)?,
                display_name: obj.display_name,
                render_type,
                number_format: obj.number_format.map(map_number_format),
            })]);
        }
        if packet_id == play::clientbound::SET_DISPLAY_OBJECTIVE {
            let display: SetDisplayObjective = decode_play(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::DisplayObjective {
                slot: map_display_slot(display.slot)?,
                objective: display.objective,
            })]);
        }
        if packet_id == play::clientbound::SET_SCORE {
            let score: SetScore = decode_play(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::ScoreUpdate {
                holder: score.owner,
                objective: score.objective,
                value: score.score,
                display: score.display,
                number_format: score.number_format.map(map_number_format),
            })]);
        }
        if packet_id == play::clientbound::RESET_SCORE {
            let reset: ResetScore = decode_play(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::ScoreReset {
                holder: reset.owner,
                objective: reset.objective,
            })]);
        }
        if packet_id == play::clientbound::SET_PLAYER_TEAM {
            // Multi-mode: parameters present for create(0)/update(2); member list
            // present for create(0)/add(3)/remove(4). Zero trailing bytes proves
            // the mode byte selected the right combination.
            let team: SetPlayerTeam = decode_play(payload)?;
            let name = team.name.clone();
            return Ok(vec![Directive::Emit(ClientEvent::TeamUpdate {
                name,
                action: map_team_action(team)?,
            })]);
        }
        if packet_id == play::clientbound::BOSS_EVENT {
            // Op-tagged union keyed by UUID; each op has a distinct body.
            let boss: BossEvent = decode_play(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::BossBarUpdate {
                id: boss.id,
                action: map_boss_action(boss.op)?,
            })]);
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
            self.set_dimension(&respawn.dimension);
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
            return Ok(vec![Directive::Emit(ClientEvent::Respawned {
                dimension,
                game_mode: mode,
                previous_game_mode,
                last_death_location,
            })]);
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
            let time_of_day = {
                let mut clock = self.clock.lock().expect("day clock poisoned");
                if let Some(update) = time.day_clock() {
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
            // represent are surfaced; the rest (win game, demo, arrow-hit, etc.)
            // decode fully — so the trailing check still guards alignment — but
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
        if packet_id == play::clientbound::TRANSFER {
            let mut reader = Reader::new(payload);
            let host = reader.string(32767).map_err(dec_err)?;
            let port = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TransferRequested {
                host,
                port,
            })]);
        }
        if packet_id == play::clientbound::COOKIE_REQUEST {
            let mut reader = Reader::new(payload);
            let key = reader.string(32767).map_err(dec_err)?;
            let key = parse_key(&key, "cookie")?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::CookieRequested { key })]);
        }
        if packet_id == play::clientbound::STORE_COOKIE {
            let mut reader = Reader::new(payload);
            let key = reader.string(32767).map_err(dec_err)?;
            let key = parse_key(&key, "cookie")?;
            let cookie_payload = reader.var_bytes(5120).map_err(dec_err)?.to_vec();
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::CookieStored {
                key,
                payload: cookie_payload,
            })]);
        }
        if packet_id == play::clientbound::RESOURCE_PACK_PUSH {
            let mut reader = Reader::new(payload);
            let id = reader.uuid().map_err(dec_err)?;
            let url = reader.string(32767).map_err(dec_err)?;
            let hash = reader.string(40).map_err(dec_err)?;
            let required = reader.bool().map_err(dec_err)?;
            let has_prompt = reader.bool().map_err(dec_err)?;
            let prompt = if has_prompt {
                let component = read_network_nbt(&mut reader).map_err(dec_err)?;
                Some(Text::from_nbt(&component))
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ResourcePackPushed {
                id,
                url,
                hash,
                required,
                prompt,
            })]);
        }
        if packet_id == play::clientbound::RESOURCE_PACK_POP {
            let mut reader = Reader::new(payload);
            let has_id = reader.bool().map_err(dec_err)?;
            let id = if has_id {
                Some(reader.uuid().map_err(dec_err)?)
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ResourcePackPopped { id })]);
        }
        if packet_id == play::clientbound::CUSTOM_PAYLOAD {
            // Only `minecraft:brand` gets a specially-typed codec in vanilla
            // (a single UTF-8 string); every other channel is
            // `DiscardedPayload`, which just consumes whatever bytes remain
            // in the packet. Carrying the raw bytes for every channel (rather
            // than special-casing brand) loses nothing and avoids guessing at
            // channel-specific shapes this adapter cannot verify.
            let mut reader = Reader::new(payload);
            let channel = reader.string(32767).map_err(dec_err)?;
            let channel = parse_key(&channel, "custom payload channel")?;
            let remaining = reader.remaining();
            let data = reader.bytes(remaining).map_err(dec_err)?.to_vec();
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::CustomPayload {
                channel,
                data,
            })]);
        }
        if packet_id == play::clientbound::SERVER_DATA {
            let mut reader = Reader::new(payload);
            let motd_nbt = read_network_nbt(&mut reader).map_err(dec_err)?;
            let motd = Text::from_nbt(&motd_nbt);
            let has_icon = reader.bool().map_err(dec_err)?;
            let icon = if has_icon {
                let remaining = reader.remaining();
                Some(reader.var_bytes(remaining).map_err(dec_err)?.to_vec())
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ServerDataReceived {
                motd,
                icon,
            })]);
        }
        if packet_id == play::clientbound::PONG_RESPONSE {
            // `ClientboundPongResponsePacket` (the `net.minecraft.network.
            // protocol.ping` one), distinct from the `PING`/`ClientEvent::Ping`
            // pair handled above.
            let mut reader = Reader::new(payload);
            let time = reader.i64().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::PongReceived { time })]);
        }
        if packet_id == play::clientbound::DELETE_CHAT {
            // `MessageSignature.Packed`: a VarInt `id + 1`; `0` is followed by
            // a full 256-byte signature, any other value is `id - 1` into the
            // last-seen cache (which this adapter does not track — see
            // `PackedMessageSignature`).
            let mut reader = Reader::new(payload);
            let id_plus_one = reader.var_i32().map_err(dec_err)?;
            let signature = if id_plus_one == 0 {
                PackedMessageSignature::Full(reader.bytes(256).map_err(dec_err)?.to_vec())
            } else {
                PackedMessageSignature::Cached(id_plus_one - 1)
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ChatMessageDeleted {
                signature,
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
        // Everything else in play is intentionally ignored for now.
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
}
