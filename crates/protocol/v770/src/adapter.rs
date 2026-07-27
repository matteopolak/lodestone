//! [`VersionAdapter`] implementation driving the protocol 776 join flow.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use lodestone_core::{
    Ctx, Decode, Encode, Reader, Writer, plain_text_from_nbt_component, read_network_nbt,
};
use lodestone_model::{
    AdapterError, BlockActionKind, BlockFace, BlockPos, BossAction, BossColor, BossOverlay,
    ChatKind, ChunkPos, ClientAction, ClientEvent, CollisionRule, ConnectionState, Directive,
    DisplaySlot, EntityInteraction, EntityMovement, EquipmentSlot, EntityEquipment, GameMode, Hand,
    ItemStack, LoginProfile, NumberFormat, ObjectiveMode, ObjectiveRenderType, PlayerCommand,
    PlayerInput, PlayerListEntry, ResourceKey, Rotation, ServerAddress, SoundCategory, TeamAction,
    TeamColor, TeamParameters, TeleportFlags, Text, TextColor, Vec3, Vec3f, VersionAdapter,
    Visibility, WorldSink,
};
use lodestone_world::{ChunkPos as WorldChunkPos, LightPatch, LoadedChunk, NibbleArray};

use crate::chunk_batch::ChunkBatchSizeCalculator;
use crate::entity_types::entity_type_name;
use crate::items::item_name;
use crate::menus::menu_name;
use crate::packet_ids::{configuration, handshaking, login, play};
use crate::packets::chunk::{ChunkShape, LevelChunkWithLight};
use crate::packets::common::{ClientInformation, KeepAlive};
use crate::packets::configuration::{
    AcceptCodeOfConduct, FinishConfiguration, ServerboundKnownPacks,
};
use crate::packets::entity::{read_lp_vec3, unpack_degrees};
use crate::packets::game::{
    ABILITY_FLAG_CAN_FLY, ABILITY_FLAG_FLYING, ABILITY_FLAG_INSTABUILD, ABILITY_FLAG_INVULNERABLE,
    AcceptTeleportation, Attack, ChatCommand, ChatMessage, ChunkBatchFinished, ChunkBatchReceived,
    ClientCommand, ConfigurationAcknowledged, ContainerClose, GameEvent, GameLogin, LevelEvent,
    LevelParticles, MOVE_FLAG_ON_GROUND, MovePlayerPosRot, PlayerAbilities, PlayerAction,
    PlayerCommand as PlayerCommandPacket, PlayerInput as PlayerInputPacket, Respawn, SetCarriedItem,
    SetDefaultSpawnPosition, SetHealth, Swing, UseItem, UseItemOn,
};
use crate::packets::handshake::Intention;
use crate::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginAcknowledged, LoginCompression, LoginDisconnect,
    LoginFinished,
};
use crate::packets::metadata::{read_entity_metadata, read_update_attributes};
use crate::packets::player_info::{PlayerInfoRemove, PlayerInfoUpdate};
use crate::packets::scoreboard::{
    self as sb, BossEvent, ResetScore, SetDisplayObjective, SetObjective, SetPlayerTeam, SetScore,
};
use crate::packets::time::SetTime;
use crate::particle_types::particle_type_name;
use crate::sound_events::sound_event;

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
}

/// Per-connection chunk-batch flow-control state: the running rate estimator and
/// the start instant of the batch currently in flight. Guarded by a [`Mutex`]
/// only to satisfy `Sync`; a connection drives it sequentially.
#[derive(Debug)]
struct ChunkBatchState {
    calculator: ChunkBatchSizeCalculator,
    batch_start: Instant,
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
                state.calculator.on_batch_finished(batch_size, duration_nanos);
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

/// Parses a `minecraft:*` identifier into a canonical [`ResourceKey`],
/// attributing a decode error to `what` on failure.
fn parse_key(name: &str, what: &str) -> Result<ResourceKey, AdapterError> {
    name.parse()
        .map_err(|_| AdapterError::Decode(format!("invalid {what} key {name}")))
}

/// Decodes a clientbound optional item stack.
///
/// Wire shape (26.2 `ItemStack.OPTIONAL_STREAM_CODEC`): a VarInt count — `<= 0`
/// means the empty stack — then the item registry id as a VarInt, then a
/// `DataComponentPatch` (a VarInt count of added components and a VarInt count of
/// removed components; both zero means an empty patch).
///
/// The canonical [`ItemStack`] models item + count only. A non-empty component
/// patch cannot be skipped, because the clientbound patch codec length-prefixes
/// neither the patch nor its individual components — consuming it requires a
/// bespoke codec for each of the 111 component types. Until those land, a stack
/// carrying components is refused loudly rather than misparsed, which keeps the
/// zero-trailing-bytes guarantee honest: plain stacks (empty patch) decode, and
/// anything else is an explicit, attributed decode error.
fn read_item_stack(reader: &mut Reader<'_>) -> Result<Option<ItemStack>, AdapterError> {
    let count = reader.var_i32().map_err(dec_err)?;
    if count <= 0 {
        return Ok(None);
    }
    let item_id = reader.var_i32().map_err(dec_err)?;
    let added = reader.var_i32().map_err(dec_err)?;
    let removed = reader.var_i32().map_err(dec_err)?;
    if added != 0 || removed != 0 {
        return Err(AdapterError::Decode(format!(
            "item id {item_id} carries {added} added and {removed} removed data \
             components; component patches are not yet supported"
        )));
    }
    let name = item_name(item_id)
        .ok_or_else(|| AdapterError::Decode(format!("unknown item registry id {item_id}")))?;
    let count = u32::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("invalid item count {count}")))?;
    Ok(Some(ItemStack {
        item: parse_key(name, "item")?,
        count,
    }))
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
        title: Text::literal(plain_text_from_nbt_component(&title)),
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
    let text = plain_text_from_nbt_component(&component);
    if text.is_empty() {
        Ok(Text::literal("Disconnected"))
    } else {
        Ok(Text::literal(text))
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

/// Decodes `add_entity` into a canonical spawn event.
///
/// Wire layout (`ClientboundAddEntityPacket`): VarInt entity id, UUID, VarInt
/// entity-type registry id, position `f64`×3, low-precision velocity, three
/// signed-byte angles (pitch, yaw, head yaw), and a VarInt data field. The type
/// id is resolved to its canonical identifier through the version-specific
/// [`entity_type_name`] table.
fn handle_add_entity(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
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
    let _head_yaw = reader.i8().map_err(dec_err)?;
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

    Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
        entity_id,
        uuid: Some(uuid),
        entity_type,
        pos: Vec3::new(x, y, z),
        rotation: Rotation::new(unpack_degrees(yaw), unpack_degrees(pitch)),
        velocity: Some(Vec3::new(vx, vy, vz)),
    })])
}

/// Decodes `remove_entities` (a VarInt-length list of VarInt ids) into a removal
/// event.
fn handle_remove_entities(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("negative remove_entities count {count}")))?;
    let mut entity_ids = Vec::with_capacity(count);
    for _ in 0..count {
        entity_ids.push(reader.var_i32().map_err(dec_err)?);
    }
    reader.ensure_empty().map_err(dec_err)?;
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
fn handle_set_entity_data(payload: &[u8]) -> Vec<Directive> {
    let mut reader = Reader::new(payload);
    let Ok(entity_id) = reader.var_i32() else {
        return Vec::new();
    };
    match read_entity_metadata(&mut reader) {
        Ok(metadata) if reader.ensure_empty().is_ok() && !metadata.is_empty() => {
            vec![Directive::Emit(ClientEvent::EntityMetadataUpdated {
                entity_id,
                metadata,
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
        if packet_id == play::clientbound::SYSTEM_CHAT {
            let mut reader = Reader::new(payload);
            let component = read_network_nbt(&mut reader)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let overlay = reader
                .bool()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let text = plain_text_from_nbt_component(&component);
            let kind = if overlay {
                ChatKind::GameInfo
            } else {
                ChatKind::System
            };
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text: Text::literal(text),
                kind,
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
            let text = plain_text_from_nbt_component(&component);
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text: Text::literal(text),
                kind: ChatKind::GameInfo,
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
            for _ in 0..len {
                items.push(read_item_stack(&mut reader)?);
            }
            let carried_item = read_item_stack(&mut reader)?;
            reader.ensure_empty().map_err(dec_err)?;
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
            let item = read_item_stack(&mut reader)?;
            reader.ensure_empty().map_err(dec_err)?;
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
            loop {
                let slot_byte = reader.u8().map_err(dec_err)?;
                let ordinal = slot_byte & 0x7F;
                let slot = EquipmentSlot::from_ordinal(ordinal).ok_or_else(|| {
                    AdapterError::Decode(format!("unknown equipment slot ordinal {ordinal}"))
                })?;
                let item = read_item_stack(&mut reader)?;
                equipment.push(EntityEquipment { slot, item });
                if slot_byte & 0x80 == 0 {
                    break;
                }
            }
            reader.ensure_empty().map_err(dec_err)?;
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
        if packet_id == play::clientbound::PLAYER_COMBAT_KILL {
            let mut reader = Reader::new(payload);
            // VarInt player id, then a network-NBT text component death message.
            reader
                .var_i32()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let component = read_network_nbt(&mut reader)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let message = plain_text_from_nbt_component(&component);
            return Ok(vec![Directive::Emit(ClientEvent::Death {
                message: Text::literal(message),
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
            // Decoded to validate the wire (a misparse would leave trailing
            // bytes) even though the canonical model carries no removal event
            // yet — that seam is reported to the model/client owners.
            let mut reader = Reader::new(payload);
            PlayerInfoRemove::decode(&mut reader, CTX)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            return Ok(Vec::new());
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
            return Ok(Vec::new());
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
            return Ok(Vec::new());
        }
        if packet_id == play::clientbound::PLAYER_POSITION {
            return handle_player_position(payload);
        }
        if packet_id == play::clientbound::ADD_ENTITY {
            return handle_add_entity(payload);
        }
        if packet_id == play::clientbound::REMOVE_ENTITIES {
            return handle_remove_entities(payload);
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
            return Ok(handle_set_entity_data(payload));
        }
        if packet_id == play::clientbound::UPDATE_ATTRIBUTES {
            return Ok(handle_update_attributes(payload));
        }
        if packet_id == play::clientbound::RESPAWN {
            // A dimension change (or post-death respawn) resets the build-height
            // window that frames every subsequent chunk. Decode the spawn info
            // in full — the trailing zero-length check is the misparse detector
            // for the conditional last-death-location field — and record the new
            // dimension so `level_chunk_with_light` stays aligned across the
            // nether/end boundary. No canonical respawn event exists yet, so no
            // directive is emitted; the seam is reported to the model owner.
            let mut reader = Reader::new(payload);
            let respawn = Respawn::decode(&mut reader, CTX)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            self.set_dimension(&respawn.dimension);
            return Ok(Vec::new());
        }
        if packet_id == play::clientbound::SET_TIME {
            // 26.2 reshaped set_time: a monotonic world age followed by a map of
            // per-world-clock updates (see `packets::time`). Decode it fully so
            // the trailing zero-length check guards the variable-length map, and
            // surface the world age plus a best-effort day time.
            let mut reader = Reader::new(payload);
            let time = SetTime::decode(&mut reader, CTX)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            return Ok(vec![Directive::Emit(ClientEvent::TimeChanged {
                world_age: time.game_time,
                time_of_day: time.day_time(),
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
            } if state == ConnectionState::Play => {
                // Both position and rotation are always supplied, so this maps
                // to `move_player_pos_rot`. The model carries no
                // horizontal-collision signal, so only the on-ground bit is set
                // (a controller with collision info would extend this).
                let flags = if *on_ground { MOVE_FLAG_ON_GROUND } else { 0 };
                let body = MovePlayerPosRot {
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                    yaw: rotation.yaw,
                    pitch: rotation.pitch,
                    flags,
                };
                Ok(Some((
                    play::serverbound::MOVE_PLAYER_POS_ROT,
                    encode_body(&body)?,
                )))
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
            ClientAction::ContainerClick { .. } if state == ConnectionState::Play => {
                // 26.2's `container_click` encodes slot contents as `HashedStack`
                // (item id, count, and a CRC32 hash of the component patch), not
                // a full `ItemStack`. The canonical model's `ItemStack` is
                // item+count only and cannot reproduce those hashes, so encoding
                // this now would send wrong bytes rather than none.
                Err(AdapterError::Unsupported(
                    "container_click (26.2 HashedStack component hashing is not yet modelled)"
                        .to_owned(),
                ))
            }
            ClientAction::SetCreativeModeSlot { .. } if state == ConnectionState::Play => {
                // Needs the full `ItemStack.OPTIONAL_UNTRUSTED_STREAM_CODEC`: a
                // numeric item-registry id plus a data-component patch. There is
                // no generated item registry in this crate and the model omits
                // components, so a faithful encoding is not yet possible.
                Err(AdapterError::Unsupported(
                    "set_creative_mode_slot (no item registry table and ItemStack components are not yet modelled)"
                        .to_owned(),
                ))
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
}
