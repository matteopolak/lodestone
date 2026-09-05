//! [`VersionAdapter`] implementation driving the protocol 5 join flow.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use lodestone_canonical::canonical::{self, CanonicalBlockState};
use lodestone_core::{Ctx, Decode, Encode, Reader};
use lodestone_data::block_entity_types::block_entity_type;
use lodestone_data::block_states;
use lodestone_data::mob_effects::{mob_effect_name_for, MobEffectId};
use lodestone_model::{
    AdapterError, AnimationAction, BlockActionKind, BlockFace, BlockPos, BlockStateRef, ChatKind,
    ChatMode, ChunkPos, ClientAction, ClientActionKind, ClientEvent, ClientSettings, ConnectionState,
    Directive, EntityAttributeModifier, EntityAttributeSnapshot, EntityEquipment,
    EntityInteraction, EntityMovement, EquipmentSlot, GameMode, Hand, ItemStack, LoginProfile,
    LevelEventData, PlayerCommand, PlayerListEntry, ResourceKey, Rotation, SectionPos,
    ServerAddress, SoundCategory, TeleportFlags, Text, Vec3, VersionAdapter,
};
use lodestone_world::{ChunkPos as WorldChunkPos, Heightmaps, LoadedChunk, WorldSink};

use crate::entity_metadata;
use crate::entity_types;
use crate::item_types;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::chunk::{ChunkData, ChunkShape, MapChunk, MapChunkBulk};
use crate::packets::entity::{
    Animation, AttachEntity, ClientboundEntityEquipment, Collect, EntityDestroy, EntityEffect,
    EntityHeadRotation, EntityLook, EntityMetadataPacket, EntityMoveLook, EntityStatus,
    EntityTeleport, EntityVelocityPacket, NamedEntitySpawn, RelEntityMove, RemoveEntityEffect,
    SpawnEntityExperienceOrb, SpawnEntityLiving, SpawnEntityPainting, SpawnEntityWeather,
    SpawnObject, UpdateAttributes,
};
use crate::packets::game::{
    ClientCommand, ClientboundChat, ClientboundPositionLook, EntityAction, Experience,
    GameStateChange, JoinGame, KeepAliveRequest, KeepAliveResponse, KickDisconnect, Respawn,
    ServerboundArmAnimation, ServerboundChat, ServerboundCustomPayload, ServerboundFlying,
    ServerboundLook, ServerboundPosition, ServerboundPositionLook, SpawnPosition, UpdateHealth,
    UpdateTime, UseEntity,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{EncryptionRequest, LoginDisconnect, LoginStart, LoginSuccess};
use crate::packets::player_info::PlayerInfo;
use crate::packets::settings::{PlayerAbilities, Settings};
use crate::packets::slot::Slot;
use crate::packets::window::{
    CloseWindow, CraftProgressBar, EnchantItem, HeldItemSlot, OpenWindow, ServerboundCloseWindow,
    ServerboundHeldItemSlot, SetCreativeSlot, SetSlot, WindowItems,
};
use crate::packets::world::{
    BlockAction, BlockBreakAnimation, BlockChange, BlockDig, BlockPlace, MultiBlockChange,
    NamedSoundEffect, OpenSignEntity, WorldEvent,
};

/// Protocol version implemented by this adapter.
pub const PROTOCOL: i32 = 5;

/// Tags the block-break event's payload without pretending protocol 5's state
/// numbering is this build's generated numbering. All other event payloads
/// retain their signed event-specific shape.
fn level_event_data(event: i32, data: i32) -> LevelEventData {
    if event == 2001 {
        LevelEventData::BlockState(BlockStateRef::protocol_local(data as u32))
    } else {
        LevelEventData::Raw(data)
    }
}

/// Every protocol number this family speaks.
///
/// One entry, because the era is a singleton: measured against its upper
/// neighbour, protocol 5 and protocol 47 agree on 37 of 112 packet shapes, far
/// below the 85% the era-grouping threshold asks for.
///
/// One protocol number is not one Minecraft version. Protocol 5 is what 1.7.6,
/// 1.7.7, 1.7.8, 1.7.9 and 1.7.10 all negotiate, and nothing on the wire
/// distinguishes them, so this family serves all five by construction;
/// [`VersionAdapter::minecraft_versions`] lists them.
///
/// [`VersionAdapter::supports`] tests membership here and `lodestone-registry`
/// points at this same slice, so the registry's view of the family cannot
/// drift from the family's own.
pub const PROTOCOLS: &[i32] = &[PROTOCOL];

/// Fixed decoding/encoding context for protocol 5.
const CTX: Ctx = Ctx { version: PROTOCOL };

/// Requested next-state value in the handshake for a login connection.
const NEXT_STATE_LOGIN: i32 = 2;

/// Fixed-point scale for entity positions and movement deltas: 32 per block.
const FIXED_POINT_SCALE: f64 = 32.0;

/// Fixed-point scale for `named_sound_effect` positions.
///
/// **Eight** units per block, not thirty-two. The same protocol uses two
/// different fixed-point scales, so a sound placed with the entity scale lands
/// four times too close to the origin.
const SOUND_POSITION_SCALE: f64 = 8.0;

/// Divisor turning the sound packet's packed pitch byte into a multiplier,
/// where 63 is normal playback speed.
const SOUND_PITCH_SCALE: f32 = 63.0;

/// Velocity scale: 1/8000 of a block per tick per unit.
const VELOCITY_SCALE: f64 = 8000.0;

/// Squared distance below which a move is treated as no movement at all.
const MOVE_EPSILON_SQUARED: f64 = 9.0e-4;

/// Ticks after which a position packet is resent even without movement, so a
/// stationary client keeps its position claim alive.
const POSITION_REMINDER_TICKS: u32 = 20;

/// Standing eye height, added to the feet `y` to produce the `stance` field
/// this era's serverbound movement packets carry and protocol 47 removed.
///
/// The server range-checks the difference and closes the connection over one
/// outside its tolerance, so this cannot be left at zero.
const STANDING_EYE_HEIGHT: f64 = 1.62;

/// Ability bit flags, shared by the clientbound and serverbound packets.
const ABILITY_INVULNERABLE: i8 = 0x01;
const ABILITY_FLYING: i8 = 0x02;
const ABILITY_CAN_FLY: i8 = 0x04;
const ABILITY_INSTABUILD: i8 = 0x08;

/// Default flying speed echoed back on a flight toggle, matching the value the
/// server sends in its own abilities packet for a creative-mode player.
const DEFAULT_FLYING_SPEED: f32 = 0.05;

/// Default walking speed, from the same packet.
const DEFAULT_WALKING_SPEED: f32 = 0.1;

/// Game-state reason codes this adapter translates.
const GAME_STATE_RAIN_STOPS: u8 = 1;
const GAME_STATE_RAIN_STARTS: u8 = 2;
const GAME_STATE_GAME_MODE: u8 = 3;

/// `block_dig` status codes.
const DIG_START: i8 = 0;
const DIG_ABORT: i8 = 1;
const DIG_STOP: i8 = 2;
const DIG_DROP_STACK: i8 = 3;
const DIG_DROP_ONE: i8 = 4;
const DIG_RELEASE_USE: i8 = 5;

/// Version adapter implementing protocol 5 (Minecraft 1.7.6 - 1.7.10).
///
/// # Why this adapter is stateful
///
/// Three pieces of per-connection state, each because a packet in this era
/// carries strictly less than the canonical event it has to produce:
///
/// - **`shape`** — a chunk packet cannot tell from its own bytes whether sky
///   light is present; that depends on the dimension the join announced.
/// - **`dimension`** — the spawn-position packet names no dimension, and the
///   canonical event requires one.
/// - **`movement`** — the canonical move action is one message, but this era
///   has four movement packets and expects the client to pick the narrowest
///   one that carries the change.
#[derive(Debug, Clone)]
pub struct V5Adapter {
    shape: Arc<Mutex<ChunkShape>>,
    /// The raw dimension byte from the most recent join or respawn.
    dimension: Arc<Mutex<i8>>,
    movement: Arc<Mutex<MovementState>>,
}

/// What the last movement packet claimed, so the next one can carry only the
/// part that changed.
#[derive(Debug, Clone, Copy)]
struct MovementState {
    last_pos: Vec3,
    last_yaw: f32,
    last_pitch: f32,
    position_reminder: u32,
}

impl Default for MovementState {
    fn default() -> Self {
        Self {
            last_pos: Vec3::new(0.0, 0.0, 0.0),
            last_yaw: 0.0,
            last_pitch: 0.0,
            // Starts at the reminder threshold so the very first move sends a
            // full position rather than a bare on-ground flag, whatever the
            // caller's starting coordinates happen to be.
            position_reminder: POSITION_REMINDER_TICKS,
        }
    }
}

impl Default for V5Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl V5Adapter {
    /// Creates an adapter for a fresh connection.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shape: Arc::new(Mutex::new(ChunkShape::overworld())),
            dimension: Arc::new(Mutex::new(0)),
            movement: Arc::new(Mutex::new(MovementState::default())),
        }
    }

    fn set_dimension(&self, dimension: i8) {
        if let Ok(mut guard) = self.dimension.lock() {
            *guard = dimension;
        }
        if let Ok(mut guard) = self.shape.lock() {
            // Only the overworld sends sky light. Getting this wrong does not
            // fail a bounds check: it silently reads the next array's block
            // ids as light nibbles, so it must be set before the first chunk.
            *guard = if dimension == 0 {
                ChunkShape::overworld()
            } else {
                ChunkShape::no_skylight()
            };
        }
    }

    fn current_shape(&self) -> ChunkShape {
        match self.shape.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    fn current_dimension(&self) -> i8 {
        match self.dimension.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

/// Creates an adapter for protocol 5.
#[must_use]
pub fn adapter() -> V5Adapter {
    V5Adapter::new()
}

/// Creates an adapter for a negotiated protocol number.
///
/// This family is single-protocol, so the argument is checked rather than
/// selected on. The registry only calls this after `PROTOCOLS` confirmed
/// membership, so a mismatch is a registry bug rather than a wire condition,
/// and the debug assertion is where it should surface.
#[must_use]
pub fn adapter_for(protocol: i32) -> V5Adapter {
    debug_assert!(
        PROTOCOLS.contains(&protocol),
        "lodestone-v1-7 was asked for protocol {protocol}, which it does not serve"
    );
    V5Adapter::new()
}

fn encode_body<T: Encode>(packet: &T) -> Result<Vec<u8>, AdapterError> {
    lodestone_core::encode_body(packet, CTX).map_err(AdapterError::Encode)
}

fn decode_body<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    lodestone_core::decode_body(payload, CTX).map_err(AdapterError::Decode)
}

fn decode_body_exact<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    lodestone_core::decode_body_exact(payload, CTX).map_err(AdapterError::Decode)
}

fn dec_err(err: impl std::fmt::Display) -> AdapterError {
    AdapterError::Decode(err.to_string())
}

fn send<T: Encode>(packet_id: i32, packet: &T) -> Result<Directive, AdapterError> {
    Ok(Directive::Send {
        packet_id,
        payload: encode_body(packet)?,
    })
}

fn game_mode(value: u8) -> Result<GameMode, AdapterError> {
    // The `0x8` bit marks a hardcore world rather than selecting a mode.
    match value & 0x07 {
        0 => Ok(GameMode::Survival),
        1 => Ok(GameMode::Creative),
        2 => Ok(GameMode::Adventure),
        3 => Ok(GameMode::Spectator),
        other => Err(AdapterError::Decode(format!(
            "protocol 5 game mode {other} is not survival, creative, adventure or spectator"
        ))),
    }
}

fn dimension_id(value: i8) -> Result<lodestone_model::DimensionId, AdapterError> {
    let name = match value {
        -1 => "minecraft:the_nether",
        0 => "minecraft:overworld",
        1 => "minecraft:the_end",
        other => {
            return Err(AdapterError::Decode(format!(
                "protocol 5 dimension {other} is not the nether, the overworld or the end"
            )));
        }
    };
    name.parse()
        .map_err(|_| AdapterError::Decode(format!("dimension identifier {name} is not a key")))
}

/// Converts a stack from this era's wire into the canonical model's.
///
/// Resolves the item's family through [`item_types`] and nothing from
/// `damage`; see that module for why the variant is deliberately not guessed
/// at. An id absent from the table decodes to an empty slot rather than
/// failing the whole packet, so one unknown item cannot make an entire chest
/// undisplayable.
fn slot_to_item_stack(slot: &Slot) -> Option<ItemStack> {
    let id = slot.id?;
    let name = item_types::item_name(id)?;
    let key: ResourceKey = name.parse().ok()?;
    Some(ItemStack::new(key, u32::try_from(slot.count).unwrap_or(0)))
}

/// Maps this era's equipment-slot ordinal to the canonical slot.
///
/// **Not** the canonical enum's own declaration order, which puts the off-hand
/// at ordinal 1 and shifts every armour slot down by one. There is no off-hand
/// in this era: the five ordinals are held item, boots, leggings, chestplate,
/// helmet, matching the five-slot equipment array a living entity has here.
/// Using the modern ordinals would render every boots-equip as an off-hand
/// item and put the rest of the armour one slot out.
fn equipment_slot(ordinal: i16) -> Result<EquipmentSlot, AdapterError> {
    match ordinal {
        0 => Ok(EquipmentSlot::MainHand),
        1 => Ok(EquipmentSlot::Feet),
        2 => Ok(EquipmentSlot::Legs),
        3 => Ok(EquipmentSlot::Chest),
        4 => Ok(EquipmentSlot::Head),
        other => Err(AdapterError::Decode(format!(
            "protocol 5 equipment slot ordinal {other} is outside 0..=4"
        ))),
    }
}

/// Converts this era's signed, one-based wire id to the shared zero-based
/// built-in registry id. The conversion is kept at packet ingress so an
/// unknown or extension value cannot index the canonical table.
fn legacy_mob_effect_id(wire_id: i32) -> Result<MobEffectId, AdapterError> {
    let Some(id) = wire_id
        .checked_sub(1)
        .and_then(MobEffectId::from_registry_id)
    else {
        return Err(AdapterError::Decode(format!(
            "unknown legacy effect id {wire_id}"
        )));
    };
    Ok(id)
}

/// Resolves a validated legacy effect id to its canonical key.
fn effect_key(effect_id: MobEffectId) -> Result<ResourceKey, AdapterError> {
    let name = mob_effect_name_for(effect_id);
    name.parse()
        .map_err(|_| AdapterError::Decode(format!("mob effect name {name} is not a key")))
}

/// Resolves an attribute key as this era spells it to its canonical key.
///
/// This era's keys are dotted camel-case strings, which are not valid
/// canonical identifiers at all — an uppercase letter is a parse error, not
/// merely an unknown name — so a translation is unavoidable rather than
/// optional. The era has exactly seven attributes and all seven still exist in
/// the canonical registry under snake-case names, so this is a complete table
/// rather than a prefix of one.
fn attribute_key(key: &str) -> Option<ResourceKey> {
    let name = match key {
        "generic.maxHealth" => "minecraft:max_health",
        "generic.followRange" => "minecraft:follow_range",
        "generic.knockbackResistance" => "minecraft:knockback_resistance",
        "generic.movementSpeed" => "minecraft:movement_speed",
        "generic.attackDamage" => "minecraft:attack_damage",
        "horse.jumpStrength" => "minecraft:jump_strength",
        "zombie.spawnReinforcements" => "minecraft:spawn_reinforcements",
        _ => return None,
    };
    name.parse().ok()
}

/// Resolves a bare numeric block-**type** id, as the block-event packet
/// carries it, to a canonical block-family key.
///
/// That packet carries no metadata at all, unlike the id-and-metadata
/// composite the chunk and block-change paths build, and every block that can
/// trigger a block event resolves to the same family whatever its metadata —
/// only within-family properties such as facing vary with it. But metadata `0`
/// is not always a populated slot: a chest has entries only for its four
/// facing values, so a fixed `0` would resolve every chest-lid event to air.
/// Scanning for the first populated metadata is family-safe, since any
/// populated value names the same block.
fn block_family_key(block_id: u8) -> ResourceKey {
    let block = (0u8..16)
        .find_map(|meta| match canonical::resolve(block_id, meta) {
            CanonicalBlockState::Resolved(state) => Some(state),
            _ => None,
        })
        .unwrap_or_else(block_states::air_state)
        .block();
    block
        .name()
        .parse()
        .expect("built-in block names are valid resource keys")
}

fn unpack_degrees(packed: i8) -> f32 {
    f32::from(packed) * 360.0 / 256.0
}

/// Converts a fixed-point coordinate triple (32 units per block) to blocks.
fn fixed_point(x: i32, y: i32, z: i32) -> Vec3 {
    Vec3::new(
        f64::from(x) / FIXED_POINT_SCALE,
        f64::from(y) / FIXED_POINT_SCALE,
        f64::from(z) / FIXED_POINT_SCALE,
    )
}

fn velocity_vec(x: i16, y: i16, z: i16) -> Vec3 {
    Vec3::new(
        f64::from(x) / VELOCITY_SCALE,
        f64::from(y) / VELOCITY_SCALE,
        f64::from(z) / VELOCITY_SCALE,
    )
}

/// Applies one decoded column to the world and returns its notification.
fn apply_column(world: &mut dyn WorldSink, data: ChunkData) -> Directive {
    world.load(
        WorldChunkPos::new(data.x, data.z),
        LoadedChunk::new(
            data.column,
            data.light,
            // There are no heightmaps on the wire in this era; a client
            // derives them from the block data it just received.
            Heightmaps::new(),
            // Block-entity payloads arrive on their own packet here rather
            // than inside the column, and that packet has no canonical
            // carrier yet.
            Vec::new(),
        ),
    );
    Directive::Emit(ClientEvent::ChunkLoaded {
        pos: ChunkPos::new(data.x, data.z),
    })
}

/// Maps this era's container-kind string plus slot count to a canonical menu
/// key.
///
/// Two of these are judgement calls rather than lookups. A chest and a horse
/// inventory have no fixed modern menu — the modern registry picks a
/// `generic_9xN` from the row count — so both are derived from the slot count,
/// with a ceiling division, since flooring would hide the remainder of a
/// 17-slot horse inventory. Every other string here already spells its
/// canonical key.
fn resolve_menu_type(inventory_type: &str, slot_count: u8) -> ResourceKey {
    let generic_rows = || {
        let rows = (u32::from(slot_count).div_ceil(9)).clamp(1, 6);
        format!("minecraft:generic_9x{rows}")
    };
    let key = match inventory_type {
        "minecraft:chest" | "minecraft:container" | "EntityHorse" => generic_rows(),
        "minecraft:dispenser" | "minecraft:dropper" => "minecraft:generic_3x3".to_owned(),
        "minecraft:crafting_table" => "minecraft:crafting".to_owned(),
        "minecraft:enchanting_table" => "minecraft:enchantment".to_owned(),
        "minecraft:villager" => "minecraft:merchant".to_owned(),
        // furnace, anvil, beacon, brewing_stand and hopper already spell their
        // canonical key verbatim.
        other => other.to_owned(),
    };
    key.parse().unwrap_or_else(|_| {
        generic_rows()
            .parse()
            .expect("generic_9xN is always a valid key")
    })
}

const fn face_ordinal(face: BlockFace) -> i8 {
    match face {
        BlockFace::Down => 0,
        BlockFace::Up => 1,
        BlockFace::North => 2,
        BlockFace::South => 3,
        BlockFace::West => 4,
        BlockFace::East => 5,
    }
}

/// The cursor offset within a block face, in sixteenths.
fn cursor_byte(value: f32) -> i8 {
    (value * 16.0).clamp(0.0, 15.0) as i8
}

const fn chat_mode_value(mode: ChatMode) -> i8 {
    match mode {
        ChatMode::Full => 0,
        ChatMode::CommandsOnly => 1,
        ChatMode::Hidden => 2,
    }
}

/// The block `y` this era's dig and place packets carry: an unsigned byte over
/// the fixed 0..256 world.
fn block_y(pos: BlockPos) -> Result<u8, AdapterError> {
    u8::try_from(pos.y).map_err(|_| {
        AdapterError::Encode(format!(
            "protocol 5 block y {} is outside the 0..256 world this era has",
            pos.y
        ))
    })
}

impl V5Adapter {
    fn handle_login(&self, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == login::clientbound::SUCCESS {
            // Decoded rather than skipped: this packet's dashed-string UUID is
            // the shape separating this era from every later one, so a decode
            // failure here is worth surfacing where it happens.
            let _profile: LoginSuccess = decode_body(payload)?;
            // There is no configuration state and no login acknowledgement in
            // this era, so login success goes straight to play.
            return Ok(vec![Directive::SetState(ConnectionState::Play)]);
        }
        if packet_id == login::clientbound::ENCRYPTION_BEGIN {
            // Decoded with this era's own `i16`-prefixed blob shape before the
            // refusal, so a wrong framing assumption fails visibly here rather
            // than hiding behind an error that was coming anyway.
            let _request: EncryptionRequest = decode_body(payload)?;
            return Err(AdapterError::Unsupported(
                "online-mode authentication is not implemented for protocol 5; connect to an \
                 offline-mode server"
                    .to_owned(),
            ));
        }
        if packet_id == login::clientbound::DISCONNECT {
            let body: LoginDisconnect = decode_body(payload)?;
            return Ok(vec![Directive::Disconnect(Text::from_json(&body.reason))]);
        }
        // There is no compression packet in this state at all, so unlike every
        // later era there is no compression directive to emit here.
        Ok(Vec::new())
    }

    fn handle_play_login(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: JoinGame = decode_body(payload)?;
        // Recorded before any chunk arrives, so the first column decodes with
        // the right geometry.
        self.set_dimension(body.dimension);
        Ok(vec![Directive::Emit(ClientEvent::Login {
            entity_id: body.entity_id,
            game_mode: game_mode(body.game_mode)?,
            dimension: dimension_id(body.dimension)?,
        })])
    }

    fn handle_play_keep_alive(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: KeepAliveRequest = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::KeepAlive {
            id: i64::from(body.keep_alive_id),
        })])
    }

    fn handle_play_chat(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ClientboundChat = decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::Chat {
            text: Text::from_json(&body.message),
            // There is no chat-position byte in this era: every chat packet is
            // a chat-box message. The system and action-bar distinction
            // arrives with protocol 47.
            kind: ChatKind::Chat,
            // No sender field either, so there is nothing to filter on.
            sender: None,
            ack: None,
        })])
    }

    fn handle_play_update_time(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: UpdateTime = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TimeChanged {
            world_age: body.age,
            time_of_day: body.time,
        })])
    }

    fn handle_play_update_health(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: UpdateHealth = decode_body_exact(payload)?;
        let mut directives = vec![Directive::Emit(ClientEvent::HealthChanged {
            health: body.health,
            food: i32::from(body.food),
            saturation: body.food_saturation,
        })];
        if body.health <= 0.0 {
            // There is no combat-event packet in this era, so zero health is
            // the entire death signal and no death message accompanies it. The
            // empty text is the honest reading, not a stand-in for one the
            // packet withheld. Without this the client never sends a respawn
            // request, and the server holds a dead player on the death screen
            // and stops streaming chunks until it receives one.
            directives.push(Directive::Emit(ClientEvent::Death {
                message: Text::literal(String::new()),
            }));
        }
        Ok(directives)
    }

    fn handle_play_respawn(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: Respawn = decode_body(payload)?;
        // The join packet's dimension is a signed byte and this one widens it
        // to a full `i32` — an inconsistency inside this one protocol, not a
        // transcription slip, so the value is narrowed rather than the field.
        let dimension = i8::try_from(body.dimension).map_err(|_| {
            AdapterError::Decode(format!(
                "protocol 5 respawn dimension {} is outside the byte range the join packet uses",
                body.dimension
            ))
        })?;
        // Re-recorded because a portal changes the chunk geometry: the next
        // column's light arrays depend on it.
        self.set_dimension(dimension);
        Ok(vec![Directive::Emit(ClientEvent::Respawned {
            dimension: dimension_id(dimension)?,
            game_mode: game_mode(body.gamemode)?,
            // Neither field exists on the wire here.
            previous_game_mode: None,
            last_death_location: None,
        })])
    }

    fn handle_play_position(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ClientboundPositionLook = decode_body_exact(payload)?;
        // The wire's middle coordinate is the eye position, so the feet are
        // one standing eye height below it. See `ClientboundPositionLook`,
        // which records how the two readings were told apart.
        let feet_y = body.stance - STANDING_EYE_HEIGHT;
        let pos = Vec3::new(body.x, feet_y, body.z);
        let rotation = Rotation {
            yaw: body.yaw,
            pitch: body.pitch,
        };

        // The confirmation echo, and why nothing works without it.
        //
        // There is no teleport id and no confirmation packet in this era —
        // both arrive with protocol 340. Until the client sends a serverbound
        // `position_look` back, the server holds the player at the pending
        // teleport and silently ignores every movement packet: measured
        // against a real server by walking 320 blocks east one tick at a time
        // and receiving not one further chunk packet, which is what an ignored
        // move looks like from the client side. With the echo the same walk
        // streams new columns.
        //
        // Unlike protocol 47's version of this packet there is no
        // relative-coordinate flags byte, so every component is absolute and
        // the echo is unconditional: this era cannot express the relative
        // teleport that forces the later era to defer its confirmation to the
        // next movement tick.
        // The stance echoed back is the server's own value verbatim rather
        // than one re-derived from the feet, so a rounding difference cannot
        // put it outside the range the server range-checks.
        let confirm = ServerboundPositionLook {
            x: body.x,
            stance: body.stance,
            y: feet_y,
            z: body.z,
            yaw: body.yaw,
            pitch: body.pitch,
            on_ground: body.on_ground,
        };

        // Seed the movement state from where the server actually put the
        // player. Without this the next `Move` is diffed against a stale
        // origin, so a genuine teleport followed by standing still would send
        // a position packet claiming the old coordinates and re-trigger the
        // hold this echo just cleared.
        {
            let mut state = match self.movement.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.last_pos = pos;
            state.last_yaw = body.yaw;
            state.last_pitch = body.pitch;
            state.position_reminder = 0;
        }

        Ok(vec![
            send(play::serverbound::POSITION_LOOK, &confirm)?,
            Directive::Emit(ClientEvent::TeleportPlayer {
                pos,
                rotation,
                // Every coordinate in this era's teleport is absolute: the
                // relative-flags byte arrives with protocol 47, and the
                // trailing byte here is an on-ground boolean instead.
                flags: TeleportFlags::default(),
            }),
        ])
    }

    fn handle_play_spawn_position(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnPosition = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
            dimension: dimension_id(self.current_dimension())?,
            pos: body.location.to_model(),
            // No compass angle or pitch on the wire here; both are later
            // additions.
            angle: 0.0,
            pitch: 0.0,
        })])
    }

    fn handle_play_held_item_slot(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: HeldItemSlot = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::HeldSlotChanged {
            slot: i32::from(body.slot),
        })])
    }

    fn handle_play_experience(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: Experience = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ExperienceChanged {
            progress: body.experience_bar,
            level: i32::from(body.level),
            total: i32::from(body.total_experience),
        })])
    }

    fn handle_play_abilities(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        // Decoded through the type both directions share, since the two have
        // the same three fields here and the flags mean the same thing in
        // each.
        let body: PlayerAbilities = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::AbilitiesChanged {
            invulnerable: body.flags & ABILITY_INVULNERABLE != 0,
            flying: body.flags & ABILITY_FLYING != 0,
            can_fly: body.flags & ABILITY_CAN_FLY != 0,
            instabuild: body.flags & ABILITY_INSTABUILD != 0,
            flying_speed: body.flying_speed,
            walking_speed: body.walking_speed,
        })])
    }

    fn handle_play_game_state_change(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: GameStateChange = decode_body_exact(payload)?;
        match body.reason {
            GAME_STATE_RAIN_STARTS => Ok(vec![Directive::Emit(ClientEvent::WeatherChanged {
                raining: Some(true),
                rain_level: None,
                thunder_level: None,
            })]),
            GAME_STATE_RAIN_STOPS => Ok(vec![Directive::Emit(ClientEvent::WeatherChanged {
                raining: Some(false),
                rain_level: None,
                thunder_level: None,
            })]),
            GAME_STATE_GAME_MODE => {
                // The value slot is a float because one field serves every
                // reason code; a game mode is always integral in it.
                let game_mode = game_mode(body.game_mode as u8)?;
                Ok(vec![Directive::Emit(ClientEvent::GameModeChanged {
                    game_mode,
                })])
            }
            // Every other reason code — the bed-use refusal, the thunder
            // level, the demo and credits triggers, the arrow-hit effect —
            // has no canonical carrier that would not be invented here, so
            // they are decoded and dropped rather than guessed at.
            _ => Ok(Vec::new()),
        }
    }

    fn handle_play_kick_disconnect(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: KickDisconnect = decode_body(payload)?;
        Ok(vec![Directive::Disconnect(Text::from_json(&body.reason))])
    }

    fn handle_play_player_info(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: PlayerInfo = decode_body_exact(payload)?;
        if body.online {
            Ok(vec![Directive::Emit(ClientEvent::PlayerListUpdate {
                entries: vec![PlayerListEntry {
                    uuid: None,
                    name: Some(body.player_name),
                    // The wire carries neither a game mode nor a listed flag
                    // per entry; presence in the packet is the whole message.
                    game_mode: None,
                    latency: Some(i32::from(body.ping)),
                    display_name: None,
                    listed: Some(true),
                    // Every field below is absent rather than empty: this era
                    // has no mechanism that could carry any of them, so "the
                    // update did not include it" is the truthful reading and
                    // not "the profile has none". A fold treats `None` as
                    // keep-existing, which is exactly right for an era whose
                    // player list is one name at a time.
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                }],
            })])
        } else {
            Ok(vec![Directive::Emit(ClientEvent::PlayerListRemoveByName {
                profile_names: vec![body.player_name],
            })])
        }
    }

    // --- chunks and blocks ------------------------------------------------

    fn handle_play_map_chunk(
        &self,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let shape = self.current_shape();
        let mut reader = Reader::new(payload);
        let data = MapChunk::decode(&mut reader, &shape).map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        if data.is_unload() {
            // There is no separate unload packet in this era: a full column
            // with an empty section mask is the signal, so the world has to
            // be told here or the column stays loaded forever.
            world.unload(WorldChunkPos::new(data.x, data.z));
            return Ok(vec![Directive::Emit(ClientEvent::ChunkUnloaded {
                pos: ChunkPos::new(data.x, data.z),
            })]);
        }
        Ok(vec![apply_column(world, data)])
    }

    fn handle_play_map_chunk_bulk(
        &self,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let shape = self.current_shape();
        let mut reader = Reader::new(payload);
        let columns = MapChunkBulk::decode(&mut reader, &shape).map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(columns
            .into_iter()
            .map(|data| apply_column(world, data))
            .collect())
    }

    fn handle_play_block_change(
        &self,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: BlockChange = decode_body_exact(payload)?;
        let pos = body.location.to_model();
        let block_id = u32::try_from(body.block_type).map_err(|_| {
            AdapterError::Decode(format!(
                "protocol 5 block_change carried a negative block id {}",
                body.block_type
            ))
        })?;
        // The id and the metadata arrive as two fields and are recombined into
        // the same composite the chunk decoder builds, so one canonicalisation
        // path serves both.
        let composite = (block_id << 4) | u32::from(body.metadata & 0x0F);
        let state = canonical::resolve_composite_or_air(composite, &mut Default::default());
        world.set_block(pos.x, pos.y, pos.z, state);
        // Writing a state is what creates or removes a block entity in this
        // era; no packet announces it.
        world.sync_block_entity(
            pos.x,
            pos.y,
            pos.z,
            lodestone_data::block_states::StateId::new(state)
                .and_then(block_entity_type)
                .map(|kind| kind.raw()),
        );
        Ok(vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
            section: SectionPos::new(pos.x >> 4, pos.y >> 4, pos.z >> 4),
            blocks: vec![[
                pos.x.rem_euclid(16) as u8,
                pos.y.rem_euclid(16) as u8,
                pos.z.rem_euclid(16) as u8,
            ]],
        })])
    }

    fn handle_play_multi_block_change(
        &self,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: MultiBlockChange = decode_body_exact(payload)?;
        // Both origins are resolved before the record loop, so a packet naming
        // an out-of-range chunk writes nothing rather than writing half of
        // itself and then failing.
        let origin_x = body.chunk_x.checked_mul(16).ok_or_else(|| {
            AdapterError::Decode(format!(
                "protocol 5 multi_block_change chunk x {} overflows a block coordinate",
                body.chunk_x
            ))
        })?;
        let origin_z = body.chunk_z.checked_mul(16).ok_or_else(|| {
            AdapterError::Decode(format!(
                "protocol 5 multi_block_change chunk z {} overflows a block coordinate",
                body.chunk_z
            ))
        })?;
        let mut by_section: BTreeMap<i32, Vec<[u8; 3]>> = BTreeMap::new();
        let mut tally = Default::default();
        for record in &body.records {
            let x = origin_x + i32::from(record.x);
            let y = i32::from(record.y);
            let z = origin_z + i32::from(record.z);
            let state = canonical::resolve_composite_or_air(record.composite(), &mut tally);
            world.set_block(x, y, z, state);
            world.sync_block_entity(
                x,
                y,
                z,
                lodestone_data::block_states::StateId::new(state)
                    .and_then(block_entity_type)
                    .map(|kind| kind.raw()),
            );
            by_section
                .entry(y >> 4)
                .or_default()
                .push([record.x, y.rem_euclid(16) as u8, record.z]);
        }
        Ok(by_section
            .into_iter()
            .map(|(section_y, blocks)| {
                Directive::Emit(ClientEvent::SectionBlocksChanged {
                    section: SectionPos::new(body.chunk_x, section_y, body.chunk_z),
                    blocks,
                })
            })
            .collect())
    }

    fn handle_play_block_action(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: BlockAction = decode_body_exact(payload)?;
        let block_id = u8::try_from(body.block_id).map_err(|_| {
            AdapterError::Decode(format!(
                "protocol 5 block_action block id {} is outside the 0..=255 block-type space",
                body.block_id
            ))
        })?;
        Ok(vec![Directive::Emit(ClientEvent::BlockEvent {
            pos: body.location.to_model(),
            b0: body.byte1,
            b1: body.byte2,
            block: block_family_key(block_id),
        })])
    }

    fn handle_play_block_break_animation(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: BlockBreakAnimation = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::BlockDestruction {
            entity_id: body.entity_id,
            pos: body.location.to_model(),
            progress: u8::try_from(body.destroy_stage).unwrap_or(0),
        })])
    }

    fn handle_play_world_event(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: WorldEvent = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::LevelEvent {
            event: body.effect_id,
            pos: body.location.to_model(),
            data: level_event_data(body.effect_id, body.data),
            global: body.global,
        })])
    }

    fn handle_play_named_sound_effect(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: NamedSoundEffect = decode_body_exact(payload)?;
        let sound: ResourceKey = body.sound_name.parse().map_err(|_| {
            AdapterError::Decode(format!(
                "protocol 5 named_sound_effect sound name {:?} is not a valid resource key",
                body.sound_name
            ))
        })?;
        Ok(vec![Directive::Emit(ClientEvent::Sound {
            sound,
            // There is no sound category on the wire in this era, and vanilla
            // predates the per-category volume sliders entirely.
            category: SoundCategory::Master,
            pos: Vec3::new(
                f64::from(body.x) / SOUND_POSITION_SCALE,
                f64::from(body.y) / SOUND_POSITION_SCALE,
                f64::from(body.z) / SOUND_POSITION_SCALE,
            ),
            volume: body.volume,
            pitch: f32::from(body.pitch) / SOUND_PITCH_SCALE,
            // Neither an audible-range override nor a variant seed exists
            // here; both are much later additions.
            fixed_range: None,
            seed: 0,
        })])
    }

    fn handle_play_open_sign_entity(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: OpenSignEntity = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::SignEditorOpened {
            pos: body.location.to_model(),
            // No front-and-back distinction here: a sign in this era has one
            // text.
            is_front_text: true,
        })])
    }

    // --- entities ---------------------------------------------------------

    fn handle_play_spawn_entity(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnObject = decode_body_exact(payload)?;
        // Resolved through the *object* table. Reading this id through the mob
        // table would name a real, wrong entity: 50 is primed TNT in one space
        // and a creeper in the other.
        let Some(kind) = entity_types::object_type_name(i32::from(body.kind)) else {
            tracing::debug!(
                target: "v1-7::entity",
                object_type = body.kind,
                "no canonical name for this object type; dropping the spawn"
            );
            return Ok(Vec::new());
        };
        let entity_type: ResourceKey = kind
            .parse()
            .map_err(|_| AdapterError::Decode(format!("object type name {kind} is not a key")))?;
        Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
            entity_id: body.entity_id,
            // Object spawns carry no UUID at all in this era.
            uuid: None,
            entity_type,
            pos: fixed_point(body.x, body.y, body.z),
            rotation: Rotation {
                yaw: unpack_degrees(body.yaw),
                pitch: unpack_degrees(body.pitch),
            },
            velocity: body.velocity.map(|(vx, vy, vz)| velocity_vec(vx, vy, vz)),
        })])
    }

    fn handle_play_spawn_entity_living(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnEntityLiving = decode_body_exact(payload)?;
        let Some(kind) = entity_types::mob_type_name(i32::from(body.kind)) else {
            tracing::debug!(
                target: "v1-7::entity",
                mob_type = body.kind,
                "no canonical name for this mob type; dropping the spawn"
            );
            return Ok(Vec::new());
        };
        let entity_type: ResourceKey = kind
            .parse()
            .map_err(|_| AdapterError::Decode(format!("mob type name {kind} is not a key")))?;
        Ok(vec![
            Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: body.entity_id,
                uuid: None,
                entity_type,
                pos: fixed_point(body.x, body.y, body.z),
                rotation: Rotation {
                    yaw: unpack_degrees(body.yaw),
                    pitch: unpack_degrees(body.pitch),
                },
                velocity: Some(velocity_vec(
                    body.velocity_x,
                    body.velocity_y,
                    body.velocity_z,
                )),
            }),
            Directive::Emit(ClientEvent::EntityMetadataUpdated {
                entity_id: body.entity_id,
                metadata: entity_metadata::fold(&body.metadata),
            }),
            // The third rotation byte is the head yaw, despite sitting between
            // the two body angles on the wire.
            Directive::Emit(ClientEvent::EntityHeadRotation {
                entity_id: body.entity_id,
                head_yaw: unpack_degrees(body.head_pitch),
            }),
        ])
    }

    fn handle_play_named_entity_spawn(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: NamedEntitySpawn = decode_body_exact(payload)?;
        // This is the only packet in the era carrying a remote player's UUID,
        // and it carries it as a dashed 36-character string rather than the
        // binary form every later era uses.
        let uuid = uuid::Uuid::parse_str(&body.player_uuid).ok();
        let entity_type: ResourceKey = entity_types::PLAYER
            .parse()
            .map_err(|_| AdapterError::Decode("player type name is not a key".to_owned()))?;
        Ok(vec![
            Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: body.entity_id,
                uuid,
                entity_type,
                pos: fixed_point(body.x, body.y, body.z),
                rotation: Rotation {
                    yaw: unpack_degrees(body.yaw),
                    pitch: unpack_degrees(body.pitch),
                },
                velocity: None,
            }),
            Directive::Emit(ClientEvent::PlayerProfileNamed {
                entity_id: body.entity_id,
                profile_name: body.player_name,
            }),
            Directive::Emit(ClientEvent::EntityMetadataUpdated {
                entity_id: body.entity_id,
                metadata: entity_metadata::fold(&body.metadata),
            }),
        ])
    }

    fn handle_play_spawn_entity_painting(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnEntityPainting = decode_body_exact(payload)?;
        let pos = body.location.to_model();
        let entity_type: ResourceKey = entity_types::PAINTING
            .parse()
            .map_err(|_| AdapterError::Decode("painting type name is not a key".to_owned()))?;
        Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
            entity_id: body.entity_id,
            uuid: None,
            entity_type,
            // A hanging entity's spawn carries a *block* position, not a
            // fixed-point one, so this is not divided by 32.
            pos: Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)),
            rotation: Rotation {
                yaw: body.direction as f32 * 90.0,
                pitch: 0.0,
            },
            velocity: None,
        })])
    }

    fn handle_play_spawn_entity_experience_orb(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnEntityExperienceOrb = decode_body_exact(payload)?;
        let entity_type: ResourceKey = entity_types::EXPERIENCE_ORB.parse().map_err(|_| {
            AdapterError::Decode("experience orb type name is not a key".to_owned())
        })?;
        Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
            entity_id: body.entity_id,
            uuid: None,
            entity_type,
            pos: fixed_point(body.x, body.y, body.z),
            rotation: Rotation::default(),
            velocity: None,
        })])
    }

    fn handle_play_spawn_entity_weather(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnEntityWeather = decode_body_exact(payload)?;
        let entity_type: ResourceKey = "minecraft:lightning_bolt"
            .parse()
            .map_err(|_| AdapterError::Decode("lightning bolt name is not a key".to_owned()))?;
        Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
            entity_id: body.entity_id,
            uuid: None,
            entity_type,
            pos: fixed_point(body.x, body.y, body.z),
            rotation: Rotation::default(),
            velocity: None,
        })])
    }

    fn handle_play_rel_entity_move(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: RelEntityMove = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
            entity_id: body.entity_id,
            movement: EntityMovement::Relative(fixed_point(
                i32::from(body.d_x),
                i32::from(body.d_y),
                i32::from(body.d_z),
            )),
            rotation: None,
            // No entity movement packet in this era carries an on-ground bit;
            // it arrives with protocol 47.
            on_ground: false,
        })])
    }

    fn handle_play_entity_look(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityLook = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
            entity_id: body.entity_id,
            movement: EntityMovement::Relative(Vec3::new(0.0, 0.0, 0.0)),
            rotation: Some(Rotation {
                yaw: unpack_degrees(body.yaw),
                pitch: unpack_degrees(body.pitch),
            }),
            on_ground: false,
        })])
    }

    fn handle_play_entity_move_look(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityMoveLook = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
            entity_id: body.entity_id,
            movement: EntityMovement::Relative(fixed_point(
                i32::from(body.d_x),
                i32::from(body.d_y),
                i32::from(body.d_z),
            )),
            rotation: Some(Rotation {
                yaw: unpack_degrees(body.yaw),
                pitch: unpack_degrees(body.pitch),
            }),
            on_ground: false,
        })])
    }

    fn handle_play_entity_teleport(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityTeleport = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
            entity_id: body.entity_id,
            movement: EntityMovement::Absolute(fixed_point(body.x, body.y, body.z)),
            rotation: Some(Rotation {
                yaw: unpack_degrees(body.yaw),
                pitch: unpack_degrees(body.pitch),
            }),
            on_ground: false,
        })])
    }

    fn handle_play_entity_head_rotation(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityHeadRotation = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityHeadRotation {
            entity_id: body.entity_id,
            head_yaw: unpack_degrees(body.head_yaw),
        })])
    }

    fn handle_play_entity_velocity(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityVelocityPacket = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityVelocity {
            entity_id: body.entity_id,
            velocity: velocity_vec(body.velocity_x, body.velocity_y, body.velocity_z),
        })])
    }

    fn handle_play_entity_destroy(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityDestroy = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityRemoved {
            entity_ids: body.entity_ids,
        })])
    }

    fn handle_play_entity_metadata(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityMetadataPacket = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityMetadataUpdated {
            entity_id: body.entity_id,
            metadata: entity_metadata::fold(&body.metadata),
        })])
    }

    fn handle_play_entity_status(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityStatus = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityStatus {
            entity_id: body.entity_id,
            status: body.entity_status as u8,
        })])
    }

    fn handle_play_attach_entity(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: AttachEntity = decode_body_exact(payload)?;
        if body.leash {
            return Ok(vec![Directive::Emit(ClientEvent::EntityLeashed {
                entity_id: body.entity_id,
                holder_id: (body.vehicle_id >= 0).then_some(body.vehicle_id),
            })]);
        }
        // A mount relation arrives one pair at a time and names the *rider*
        // and its vehicle, where the canonical event names a vehicle and all
        // of its passengers. A dismount carries a vehicle id of -1 and so does
        // not say which vehicle was left, which is why no rider-to-vehicle map
        // would help: the single-passenger fold below is the most this packet
        // can support.
        Ok(vec![Directive::Emit(
            ClientEvent::EntityPassengersChanged {
                vehicle_id: body.vehicle_id,
                passenger_ids: if body.vehicle_id < 0 {
                    Vec::new()
                } else {
                    vec![body.entity_id]
                },
            },
        )])
    }

    fn handle_play_entity_effect(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityEffect = decode_body_exact(payload)?;
        let effect_id = legacy_mob_effect_id(i32::from(body.effect_id))?;
        Ok(vec![Directive::Emit(ClientEvent::MobEffectApplied {
            entity_id: body.entity_id,
            effect: effect_key(effect_id)?,
            amplifier: i32::from(body.amplifier),
            duration_ticks: i32::from(body.duration),
            // None of these four flags is on the wire in this era: even the
            // hide-particles bit protocol 47 carries is a later addition, so
            // an effect here is always ambient-off and always visible.
            ambient: false,
            visible: true,
            show_icon: true,
            blend: false,
        })])
    }

    fn handle_play_remove_entity_effect(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: RemoveEntityEffect = decode_body_exact(payload)?;
        let effect_id = legacy_mob_effect_id(i32::from(body.effect_id))?;
        Ok(vec![Directive::Emit(ClientEvent::MobEffectRemoved {
            entity_id: body.entity_id,
            effect: effect_key(effect_id)?,
        })])
    }

    fn handle_play_entity_equipment(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ClientboundEntityEquipment = decode_body_exact(payload)?;
        // One slot per message here, so the emitted list always has one entry.
        Ok(vec![Directive::Emit(
            ClientEvent::EntityEquipmentUpdated {
                entity_id: body.entity_id,
                equipment: vec![EntityEquipment {
                    slot: equipment_slot(body.slot)?,
                    item: slot_to_item_stack(&body.item),
                }],
            },
        )])
    }

    fn handle_play_collect(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: Collect = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ItemPickup {
            item_entity_id: body.collected_entity_id,
            player_id: body.collector_entity_id,
            // The stack size is not on the wire here; protocol 47 adds it. One
            // is a documented placeholder rather than a count this packet
            // could make honest.
            amount: 1,
        })])
    }

    fn handle_play_animation(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: Animation = decode_body_exact(payload)?;
        // Five ordinals here, and the canonical enum's spelling does not line
        // up with them one for one: there is no off-hand swing to map, ordinal
        // 1 is a hurt animation the canonical set has no name for, and ordinal
        // 2 leaves a bed. The two that have no canonical name are carried
        // through as raw codes rather than bent onto a neighbouring variant.
        const HURT: u8 = 1;
        let action = match body.animation {
            0 => AnimationAction::SwingMainHand,
            2 => AnimationAction::WakeUp,
            3 => AnimationAction::CriticalHit,
            4 => AnimationAction::MagicCriticalHit,
            HURT => AnimationAction::Other(HURT),
            other => AnimationAction::Other(other),
        };
        Ok(vec![Directive::Emit(ClientEvent::EntityAnimation {
            entity_id: body.entity_id,
            action,
        })])
    }

    fn handle_play_update_attributes(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: UpdateAttributes = decode_body_exact(payload)?;
        let mut attributes = Vec::with_capacity(body.properties.len());
        for property in &body.properties {
            // An unrecognised key is skipped rather than failing the packet:
            // the canonical snapshot replaces only the attributes it names, so
            // dropping one leaves the rest correct, while refusing the packet
            // would drop all of them.
            let Some(attribute) = attribute_key(&property.key) else {
                tracing::debug!(
                    target: "v1-7::attributes",
                    key = %property.key,
                    "no canonical attribute for this key; skipping the entry"
                );
                continue;
            };
            let mut modifiers = Vec::with_capacity(property.modifiers.len());
            for modifier in &property.modifiers {
                // The wire identifies a modifier by UUID alone, with no key at
                // all, where the canonical model wants an identifier. The UUID
                // is rendered into the path so two modifiers stay distinct and
                // the same modifier keeps one identity across updates; the
                // dashes go because a canonical path may not contain them.
                let id = format!("lodestone:legacy_modifier_{}", modifier.uuid.simple());
                let Ok(id) = id.parse() else {
                    continue;
                };
                modifiers.push(EntityAttributeModifier {
                    id,
                    amount: modifier.amount,
                    operation: modifier.operation as u8,
                });
            }
            attributes.push(EntityAttributeSnapshot {
                attribute,
                base: property.value,
                modifiers,
            });
        }
        Ok(vec![Directive::Emit(
            ClientEvent::EntityAttributesUpdated {
                entity_id: body.entity_id,
                attributes,
            },
        )])
    }

    // --- containers -------------------------------------------------------

    fn handle_play_open_window(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: OpenWindow = decode_body_exact(payload)?;
        // The title is a plain string here, not a JSON component, and the
        // trailing flag says whether it is that literal or a translation key.
        // There is no language table at this layer, so an untranslated key
        // stands in for itself rather than being dropped.
        let title = Text::literal(body.window_title.clone());
        Ok(vec![Directive::Emit(ClientEvent::ScreenOpened {
            window_id: i32::from(body.window_id),
            menu_type: resolve_menu_type(&body.inventory_type, body.slot_count),
            title,
        })])
    }

    fn handle_play_close_window(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: CloseWindow = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ScreenClosed {
            window_id: i32::from(body.window_id),
        })])
    }

    fn handle_play_set_slot(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SetSlot = decode_body_exact(payload)?;
        let item = slot_to_item_stack(&body.item);
        // One packet with a window-id sentinel does what later protocols split
        // into three: -1 is the cursor, 0 is the player's own inventory with no
        // container open, anything else is a slot inside an open container.
        if body.window_id == -1 {
            return Ok(vec![Directive::Emit(ClientEvent::CursorItemChanged {
                item,
            })]);
        }
        if body.window_id == 0 {
            return Ok(vec![Directive::Emit(ClientEvent::InventorySlotChanged {
                slot: i32::from(body.slot),
                item,
            })]);
        }
        Ok(vec![Directive::Emit(ClientEvent::ContainerSlot {
            window_id: i32::from(body.window_id),
            // There is no container-synchronisation state id in this era; zero
            // is a fixed placeholder, not a sequence number.
            state_id: 0,
            slot: i32::from(body.slot),
            item,
        })])
    }

    fn handle_play_window_items(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: WindowItems = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ContainerContent {
            window_id: i32::from(body.window_id),
            state_id: 0,
            items: body.items.iter().map(slot_to_item_stack).collect(),
            // The carried (cursor) stack is not part of this packet here; it
            // arrives through a set-slot with window id -1.
            carried_item: None,
        })])
    }

    fn handle_play_craft_progress_bar(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: CraftProgressBar = decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ContainerData {
            window_id: i32::from(body.window_id),
            property: i32::from(body.property),
            value: i32::from(body.value),
        })])
    }

    /// Handles a clientbound packet while in the play state.
    fn handle_play(
        &self,
        world: &mut dyn WorldSink,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let table = Self::play_dispatch_table();
        match table.get(packet_id) {
            Some(handler) => handler(self, world, payload),
            // `None` covers two cases this arm cannot tell apart: a declared
            // [`IGNORED`] id, which is the normal path for a packet with a
            // documented reason for no handler, and an id outside this
            // protocol's table entirely, which reaches here straight off the
            // wire with nothing having validated it. `Table::build` guarantees
            // every *listed* id resolves to one or the other and says nothing
            // about an id it was never told about, so this is deliberately not
            // a panic: it keeps a non-vanilla server sending an unknown id
            // from being able to bring this client down.
            None => Ok(Vec::new()),
        }
    }

    /// Builds this family's `play::clientbound` dispatch table from the
    /// generated `(name, id)` table plus [`CLIENTBOUND`] and [`IGNORED`].
    ///
    /// `Table::build` refuses construction if a declared id has neither a
    /// handler nor a documented ignore reason, which is what turns a silent
    /// catch-all arm into an error a test can see;
    /// `tests/dispatch_coverage.rs` carries the standing proof that it
    /// succeeds and a negative control proving it can fail. Rebuilt per call
    /// rather than cached: one adapter is constructed per connection and this
    /// is not the per-tick hot path.
    fn play_dispatch_table() -> lodestone_core::dispatch::Table<'static, PlayHandlerFn> {
        lodestone_core::dispatch::Table::build(
            PROTOCOL,
            play::clientbound::ENTRIES,
            CLIENTBOUND,
            IGNORED,
        )
        .expect("v1-7 play::clientbound dispatch table must be internally consistent")
    }

    /// Picks the narrowest serverbound movement packet carrying the change.
    ///
    /// This era has four, and unlike later protocols it does not accept one
    /// combined packet for every case, so the choice is part of speaking the
    /// protocol rather than an optimisation. The `stance` field the two
    /// position-carrying packets have is the player's eye height, which the
    /// server range-checks and disconnects over.
    fn select_move_packet(
        &self,
        pos: Vec3,
        rotation: Rotation,
        on_ground: bool,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        let mut state = match self.movement.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let dx = pos.x - state.last_pos.x;
        let dy = pos.y - state.last_pos.y;
        let dz = pos.z - state.last_pos.z;
        let moved = dx * dx + dy * dy + dz * dz > MOVE_EPSILON_SQUARED
            || state.position_reminder >= POSITION_REMINDER_TICKS;
        let rotated = rotation.yaw != state.last_yaw || rotation.pitch != state.last_pitch;

        let packet = if moved && rotated {
            let body = ServerboundPositionLook {
                x: pos.x,
                stance: pos.y + STANDING_EYE_HEIGHT,
                y: pos.y,
                z: pos.z,
                yaw: rotation.yaw,
                pitch: rotation.pitch,
                on_ground,
            };
            Some((play::serverbound::POSITION_LOOK, encode_body(&body)?))
        } else if moved {
            let body = ServerboundPosition {
                x: pos.x,
                stance: pos.y + STANDING_EYE_HEIGHT,
                y: pos.y,
                z: pos.z,
                on_ground,
            };
            Some((play::serverbound::POSITION, encode_body(&body)?))
        } else if rotated {
            let body = ServerboundLook {
                yaw: rotation.yaw,
                pitch: rotation.pitch,
                on_ground,
            };
            Some((play::serverbound::LOOK, encode_body(&body)?))
        } else {
            let body = ServerboundFlying { on_ground };
            Some((play::serverbound::FLYING, encode_body(&body)?))
        };

        state.position_reminder += 1;
        if moved {
            state.last_pos = pos;
            state.position_reminder = 0;
        }
        if rotated {
            state.last_yaw = rotation.yaw;
            state.last_pitch = rotation.pitch;
        }
        Ok(packet)
    }

    /// Encodes a dig packet for one of the statuses that names no block.
    ///
    /// Dropping an item and finishing a use ride on the dig packet in this
    /// era, with a position the server ignores.
    fn dig_without_block(status: i8) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        let body = BlockDig {
            status,
            x: 0,
            y: 0,
            z: 0,
            face: 0,
        };
        Ok(Some((play::serverbound::BLOCK_DIG, encode_body(&body)?)))
    }
}

/// Payload every `play::clientbound` handler runs: a plain fn pointer, since
/// every handler closes only over `&self`, `world` and `payload`.
pub type PlayHandlerFn =
    fn(&V5Adapter, &mut dyn WorldSink, &[u8]) -> Result<Vec<Directive>, AdapterError>;

/// Every `play::clientbound` packet this family translates, keyed by the same
/// canonical name `crate::packet_ids::play::clientbound::ENTRIES` uses.
///
/// `pub` so `tests/dispatch_coverage.rs` can rebuild — and deliberately
/// corrupt — this same table from outside the crate.
pub static CLIENTBOUND: &[(&str, lodestone_core::dispatch::Handler<PlayHandlerFn>)] = &[
    (
        "minecraft:login",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_login as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:keep_alive",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_keep_alive as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:chat",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_chat as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:update_time",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_update_time as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:update_health",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_update_health as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:respawn",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_respawn as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_position as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_spawn_position as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:held_item_slot",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_held_item_slot as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:experience",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_experience as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:abilities",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_abilities as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:game_state_change",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_game_state_change as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:kick_disconnect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_kick_disconnect as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:player_info",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_player_info as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:map_chunk",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_map_chunk as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:map_chunk_bulk",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_map_chunk_bulk as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:block_change",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_block_change as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:multi_block_change",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_multi_block_change as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:block_action",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_block_action as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:block_break_animation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_block_break_animation as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:world_event",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_world_event as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:named_sound_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_named_sound_effect as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:open_sign_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_open_sign_entity as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_spawn_entity as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_entity_living",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_spawn_entity_living as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:named_entity_spawn",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_named_entity_spawn as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_entity_painting",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_spawn_entity_painting as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_entity_experience_orb",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_spawn_entity_experience_orb as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_entity_weather",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_spawn_entity_weather as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:rel_entity_move",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_rel_entity_move as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_look",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_entity_look as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_move_look",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_entity_move_look as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_teleport",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_entity_teleport as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_head_rotation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_entity_head_rotation as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_velocity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_entity_velocity as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_destroy",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_entity_destroy as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_metadata",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_entity_metadata as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_status",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_entity_status as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:attach_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_attach_entity as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_entity_effect as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:remove_entity_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_remove_entity_effect as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_equipment",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_entity_equipment as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:collect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_collect as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:animation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_animation as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:update_attributes",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_update_attributes as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:open_window",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_open_window as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:close_window",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_close_window as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:set_slot",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_set_slot as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:window_items",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_window_items as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:craft_progress_bar",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V5Adapter::handle_play_craft_progress_bar as PlayHandlerFn,
        ),
    ),
];

/// Every `play::clientbound` packet this family deliberately does not
/// translate, each with the reason.
///
/// `Table::build` requires an entry here for every declared id with no
/// handler, so this list is the enumerated alternative to a silent catch-all.
pub static IGNORED: &[lodestone_core::dispatch::IGNORED] = &[
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:entity",
        "an entity-tracker heartbeat carrying an entity id and nothing else: it exists to stop a \
         tracker timing out, and no protocol family in this workspace translates it",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:bed",
        "the canonical event set has no sleeping-entity carrier; from protocol 498 onward vanilla \
         folds sleeping into entity metadata as a pose, so there is no modern packet to take a \
         shape from, and deriving a pose from a bed position would be a guess",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:transaction",
        "accepts or rejects a container click this family cannot send: encoding one needs a \
         client-tracked action counter, an item registry and the damage value the canonical \
         ItemStack cannot express, so nothing here could ever receive one",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:update_sign",
        "carries four lines of sign text with no canonical carrier; the block-entity path that \
         would consume it is the same one this era's tile_entity_data has no home in",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:tile_entity_data",
        "carries a gzip-compressed NBT block-entity payload, and the canonical world sink takes a \
         block-entity *type* rather than its data, so the payload has nowhere to go",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:map",
        "map item data in this era is a byte-packed command stream -- a colour column, a scale \
         change or an icon list, selected by the payload's first byte -- rather than the flat \
         colour array a canonical map event would carry",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:explosion",
        "the canonical explosion event carries a particle and sound selection this era's packet \
         does not have, and its block-offset list is redundant: the server sends an ordinary \
         block change for each removed block alongside it",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:world_particles",
        "names a particle by a string from this era's own naming, which the canonical particle \
         registry does not share, and carries no per-particle payload; resolving one would be a \
         guess rather than a lookup",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:custom_payload",
        "this era's channel names contain an uppercase letter and a pipe, neither of which a \
         canonical identifier permits, so the channel cannot be represented at all; the \
         serverbound direction is unaffected, since it writes the raw string",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:statistics",
        "carries one flat dotted name per statistic, where the canonical award splits a statistic \
         into a type key and a value key; that split is a mapping table with no outside source in \
         this era's data, not a parse",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:tab_complete",
        "returns completions for a request this family does not send, so nothing here could ever \
         receive one",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:scoreboard_objective",
        "scoreboard translation is not implemented for this era; the four scoreboard packets are \
         left undecoded rather than half-translated into a display that would be wrong in ways a \
         reader could not see",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:scoreboard_score",
        "see minecraft:scoreboard_objective",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:scoreboard_display_objective",
        "see minecraft:scoreboard_objective",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:scoreboard_team",
        "see minecraft:scoreboard_objective",
    ),
];

impl VersionAdapter for V5Adapter {
    fn protocol_version(&self) -> i32 {
        PROTOCOL
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        // Every release that negotiates protocol 5. Nothing on the wire
        // distinguishes them, so the family serves all five or none.
        &["1.7.6", "1.7.7", "1.7.8", "1.7.9", "1.7.10"]
    }

    fn supports(&self, protocol: i32) -> bool {
        PROTOCOLS.contains(&protocol)
    }

    fn begin_login(
        &self,
        profile: &LoginProfile,
        server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        let handshake = SetProtocol {
            protocol_version: PROTOCOL,
            server_host: server.host.clone(),
            server_port: server.port,
            next_state: NEXT_STATE_LOGIN,
        };
        let login_start = LoginStart {
            username: profile.username.clone(),
        };
        Ok(vec![
            send(handshaking::serverbound::SET_PROTOCOL, &handshake)?,
            Directive::SetState(ConnectionState::Login),
            send(login::serverbound::LOGIN_START, &login_start)?,
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
            ConnectionState::Play => self.handle_play(world, packet_id, payload),
            // There is no configuration state at protocol 5 at all, and the
            // status flow is driven by the ping path rather than an adapter.
            ConnectionState::Handshaking
            | ConnectionState::Status
            | ConnectionState::Configuration => {
                Err(AdapterError::UnsupportedPacketState { state })
            }
        }
    }

    fn encode_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        if state != ConnectionState::Play {
            return Ok(None);
        }
        match action {
            ClientAction::KeepAliveResponse { id } => {
                let keep_alive_id = i32::try_from(*id).map_err(|_| {
                    AdapterError::Encode(format!(
                        "keep-alive id {id} does not fit the i32 this era's packet carries"
                    ))
                })?;
                let body = KeepAliveResponse { keep_alive_id };
                Ok(Some((play::serverbound::KEEP_ALIVE, encode_body(&body)?)))
            }
            ClientAction::SendChat { text } => {
                let body = ServerboundChat {
                    message: text.clone(),
                };
                Ok(Some((play::serverbound::CHAT, encode_body(&body)?)))
            }
            // There is no dedicated command packet in this era: a command is a
            // chat message beginning with a slash.
            ClientAction::SendCommand { command } => {
                let body = ServerboundChat {
                    message: format!("/{command}"),
                };
                Ok(Some((play::serverbound::CHAT, encode_body(&body)?)))
            }
            ClientAction::Move {
                pos,
                rotation,
                on_ground,
                // No movement packet here carries a horizontal-collision bit,
                // so there is nothing to forward it into.
                horizontal_collision: _,
            } => self.select_move_packet(*pos, *rotation, *on_ground),
            // The serverbound swing packet in this era carries an entity id
            // and an animation ordinal, but the server derives the sender from
            // the connection and ignores both. Zero and the swing ordinal are
            // therefore honest values rather than a tracked id this adapter
            // does not hold. There is no off-hand, so the hand is dropped.
            ClientAction::SwingArm { hand: _ } => {
                let body = ServerboundArmAnimation {
                    entity_id: 0,
                    animation: 1,
                };
                Ok(Some((
                    play::serverbound::ARM_ANIMATION,
                    encode_body(&body)?,
                )))
            }
            // Block breaking folds start, cancel and finish into three status
            // codes on one packet. The canonical sequence number is a much
            // later block-prediction field with no equivalent here.
            ClientAction::BlockAction {
                action,
                pos,
                face,
                sequence: _,
            } => {
                let status = match action {
                    BlockActionKind::StartDestroy => DIG_START,
                    BlockActionKind::AbortDestroy => DIG_ABORT,
                    BlockActionKind::StopDestroy => DIG_STOP,
                };
                let body = BlockDig {
                    status,
                    x: pos.x,
                    y: block_y(*pos)?,
                    z: pos.z,
                    face: face_ordinal(*face),
                };
                Ok(Some((play::serverbound::BLOCK_DIG, encode_body(&body)?)))
            }
            ClientAction::DropSelectedItemStack => Self::dig_without_block(DIG_DROP_STACK),
            ClientAction::DropSelectedItem => Self::dig_without_block(DIG_DROP_ONE),
            ClientAction::ReleaseUseItem => Self::dig_without_block(DIG_RELEASE_USE),
            ClientAction::UseItemOn {
                hand,
                pos,
                face,
                cursor,
                inside_block: _,
                sequence: _,
            } => {
                if *hand == Hand::Off {
                    return Err(AdapterError::Unsupported(
                        "protocol 5 has no off-hand; a use targeting it cannot be encoded"
                            .to_owned(),
                    ));
                }
                let body = BlockPlace {
                    x: pos.x,
                    y: block_y(*pos)?,
                    z: pos.z,
                    direction: face_ordinal(*face),
                    // The inline stack is redundant -- the server uses its own
                    // view of the player's inventory -- but it is not
                    // optional, and the empty stack is accepted.
                    held_item: Slot::default(),
                    cursor_x: cursor_byte(cursor.x),
                    cursor_y: cursor_byte(cursor.y),
                    cursor_z: cursor_byte(cursor.z),
                };
                Ok(Some((play::serverbound::BLOCK_PLACE, encode_body(&body)?)))
            }
            // Using an item in the air is signalled by a place packet whose
            // position is all -1 and whose direction is -1. The `y` field is
            // unsigned on this wire, so its sentinel is 255 rather than -1.
            ClientAction::UseItem {
                hand,
                rotation: _,
                sequence: _,
            } => {
                if *hand == Hand::Off {
                    return Err(AdapterError::Unsupported(
                        "protocol 5 has no off-hand; a use targeting it cannot be encoded"
                            .to_owned(),
                    ));
                }
                let body = BlockPlace {
                    x: -1,
                    y: u8::MAX,
                    z: -1,
                    direction: -1,
                    held_item: Slot::default(),
                    cursor_x: 0,
                    cursor_y: 0,
                    cursor_z: 0,
                };
                Ok(Some((play::serverbound::BLOCK_PLACE, encode_body(&body)?)))
            }
            // The interaction packet has no hand field (a protocol 110
            // addition) and no interact-at variant at all, so an interaction
            // carrying a hit location is encoded as a plain interact: dropping
            // the location is the only lossy option this era offers, and
            // refusing the action outright would make every right-click on an
            // entity fail rather than merely lose its precision.
            ClientAction::InteractEntity {
                entity_id,
                interaction,
                sneaking: _,
            } => {
                let mouse = match interaction {
                    EntityInteraction::Attack => 1,
                    EntityInteraction::Interact { .. } | EntityInteraction::InteractAt { .. } => 0,
                };
                let body = UseEntity {
                    target: *entity_id,
                    mouse,
                };
                Ok(Some((play::serverbound::USE_ENTITY, encode_body(&body)?)))
            }
            ClientAction::PlayerCommand { entity_id, command } => {
                let (action_id, jump_boost) = match command {
                    // Protocol 5 starts these ordinals at one for sneaking,
                    // unlike the later zero-based frames.
                    PlayerCommand::StopSleeping => (3, 0),
                    PlayerCommand::StartSprinting => (4, 0),
                    PlayerCommand::StopSprinting => (5, 0),
                    PlayerCommand::StartRidingJump { boost } => (6, *boost),
                    PlayerCommand::OpenInventory => (7, 0),
                    PlayerCommand::StopRidingJump => {
                        return Err(AdapterError::Unsupported(
                            "protocol 5 has no stop-riding-jump entity action".to_owned(),
                        ));
                    }
                    PlayerCommand::StartFallFlying => {
                        return Err(AdapterError::Unsupported(
                            "protocol 5 predates elytra, so there is no fall-flying action"
                                .to_owned(),
                        ));
                    }
                };
                let body = EntityAction {
                    entity_id: *entity_id,
                    action_id,
                    jump_boost,
                };
                Ok(Some((
                    play::serverbound::ENTITY_ACTION,
                    encode_body(&body)?,
                )))
            }
            ClientAction::ContainerClose { window_id } => {
                let window_id = u8::try_from(*window_id).map_err(|_| {
                    AdapterError::Encode(format!("window id {window_id} does not fit a byte"))
                })?;
                let body = ServerboundCloseWindow { window_id };
                Ok(Some((play::serverbound::CLOSE_WINDOW, encode_body(&body)?)))
            }
            ClientAction::SetCarriedItem { slot } => {
                let slot_id = i16::try_from(*slot).map_err(|_| {
                    AdapterError::Encode(format!("hotbar slot {slot} does not fit an i16"))
                })?;
                let body = ServerboundHeldItemSlot { slot_id };
                Ok(Some((
                    play::serverbound::HELD_ITEM_SLOT,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetCreativeModeSlot { slot, item } => {
                if item.is_some() {
                    return Err(AdapterError::Unsupported(
                        "protocol 5 cannot encode a non-empty creative slot: it needs a canonical \
                         key to numeric item-id direction that no crate provides, plus the damage \
                         value the canonical ItemStack cannot express"
                            .to_owned(),
                    ));
                }
                let slot = i16::try_from(*slot).map_err(|_| {
                    AdapterError::Encode(format!("creative slot {slot} does not fit an i16"))
                })?;
                let body = SetCreativeSlot {
                    slot,
                    item: Slot::default(),
                };
                Ok(Some((
                    play::serverbound::SET_CREATIVE_SLOT,
                    encode_body(&body)?,
                )))
            }
            ClientAction::ContainerButtonClick {
                window_id,
                button_id,
            } => {
                let window_id = i8::try_from(*window_id).map_err(|_| {
                    AdapterError::Encode(format!("window id {window_id} does not fit an i8"))
                })?;
                let enchantment = i8::try_from(*button_id).map_err(|_| {
                    AdapterError::Encode(format!("button id {button_id} does not fit an i8"))
                })?;
                let body = EnchantItem {
                    window_id,
                    enchantment,
                };
                Ok(Some((play::serverbound::ENCHANT_ITEM, encode_body(&body)?)))
            }
            ClientAction::SetClientSettings(settings) => {
                let ClientSettings {
                    locale,
                    view_distance,
                    chat_mode,
                    chat_colors,
                    skin_parts,
                    // Every one of these postdates this era.
                    main_hand: _,
                    text_filtering: _,
                    allow_server_listing: _,
                    particle_status: _,
                } = settings;
                let body = Settings {
                    locale: locale.clone(),
                    view_distance: *view_distance,
                    chat_flags: chat_mode_value(*chat_mode),
                    chat_colors: *chat_colors,
                    // The trailing fields are a client-selected difficulty and
                    // a single cape boolean, not protocol 47's seven-bit skin
                    // mask. Normal is the value a vanilla client of this era
                    // sends by default; the cape bit is the only one of the
                    // seven canonical skin parts this era can honour.
                    difficulty: 2,
                    show_cape: skin_parts.cape,
                };
                Ok(Some((play::serverbound::SETTINGS, encode_body(&body)?)))
            }
            ClientAction::SendBrand { brand } => {
                // The channel payload here is a length-prefixed byte array
                // rather than a string, so the brand is the whole payload with
                // no inner length of its own.
                let body = ServerboundCustomPayload {
                    channel: "MC|Brand".to_owned(),
                    data: brand.clone().into_bytes(),
                };
                Ok(Some((
                    play::serverbound::CUSTOM_PAYLOAD,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetFlying { flying } => {
                let body = PlayerAbilities {
                    flags: if *flying { ABILITY_FLYING } else { 0 },
                    flying_speed: DEFAULT_FLYING_SPEED,
                    walking_speed: DEFAULT_WALKING_SPEED,
                };
                Ok(Some((play::serverbound::ABILITIES, encode_body(&body)?)))
            }
            // Leaving the death screen. Payload 0 is a respawn request, and
            // this era's field is a byte rather than the varint later
            // protocols use.
            ClientAction::Respawn => {
                let body = ClientCommand { payload: 0 };
                Ok(Some((
                    play::serverbound::CLIENT_COMMAND,
                    encode_body(&body)?,
                )))
            }
            // Everything else is either genuinely absent from this era's wire
            // or needs a capability no crate provides. Refused by name rather
            // than dropped, so a caller cannot mistake a silent no-op for a
            // sent packet.
            other => Err(AdapterError::UnsupportedAction {
                state,
                action: ClientActionKind::from(other),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_world::World;

    /// This era's own effect list starts speed at 1 while the canonical table
    /// starts it at 0. Both endpoints are named here rather than one being
    /// derived from the other, so a table shift on either side fails.
    #[test]
    fn effect_ids_are_one_based_against_the_zero_based_canonical_table() {
        let speed = legacy_mob_effect_id(1).expect("speed's legacy id validates");
        assert_eq!(effect_key(speed).unwrap().to_string(), "minecraft:speed");
        let saturation = legacy_mob_effect_id(23).expect("saturation's legacy id validates");
        assert_eq!(effect_key(saturation).unwrap().to_string(), "minecraft:saturation");
        // Zero is below this era's own first id, so it must not silently
        // resolve to the canonical table's first entry.
        assert!(legacy_mob_effect_id(0).is_err());
    }

    fn encoded_update(wire_id: i8) -> Vec<u8> {
        encode_body(&EntityEffect {
            entity_id: 42,
            effect_id: wire_id,
            amplifier: 0,
            duration: 40,
        })
        .expect("entity effect encodes")
    }

    fn encoded_remove(wire_id: i8) -> Vec<u8> {
        encode_body(&RemoveEntityEffect {
            entity_id: 42,
            effect_id: wire_id,
        })
        .expect("remove entity effect encodes")
    }

    #[test]
    fn packet_ingress_resolves_one_based_speed_and_rejects_unknown_signed_ids() {
        let adapter = V5Adapter::new();
        let mut world = World::new();
        let applied = adapter
            .handle_play_entity_effect(&mut world, &encoded_update(1))
            .expect("known legacy effect decodes");
        let [Directive::Emit(ClientEvent::MobEffectApplied { effect, .. })] = applied.as_slice()
        else {
            panic!("known effect did not emit one application event: {applied:?}");
        };
        assert_eq!(effect.path(), "speed");

        let removed = adapter
            .handle_play_remove_entity_effect(&mut world, &encoded_remove(1))
            .expect("known legacy effect removal decodes");
        let [Directive::Emit(ClientEvent::MobEffectRemoved { effect, .. })] = removed.as_slice()
        else {
            panic!("known effect did not emit one removal event: {removed:?}");
        };
        assert_eq!(effect.path(), "speed");

        for wire_id in [i8::MIN, 0, (lodestone_data::mob_effects::MOB_EFFECT_COUNT + 1) as i8] {
            let mut world = World::new();
            let error = adapter
                .handle_play_entity_effect(&mut world, &encoded_update(wire_id))
                .expect_err("unknown update effect must fail closed");
            assert!(
                error
                    .to_string()
                    .contains(&format!("unknown legacy effect id {wire_id}")),
                "update id {wire_id}: {error}"
            );

            let error = adapter
                .handle_play_remove_entity_effect(&mut world, &encoded_remove(wire_id))
                .expect_err("unknown removal effect must fail closed");
            assert!(
                error
                    .to_string()
                    .contains(&format!("unknown legacy effect id {wire_id}")),
                "remove id {wire_id}: {error}"
            );
        }

        assert!(
            legacy_mob_effect_id(i32::MIN).is_err(),
            "checked subtraction must keep an extreme wire value from overflowing"
        );
    }

    /// Reading an object id through the mob table would name a real, wrong
    /// entity rather than failing, which is why the two spaces are separate.
    #[test]
    fn the_object_and_mob_id_spaces_disagree_where_they_overlap() {
        assert_eq!(
            entity_types::object_type_name(50),
            Some("minecraft:primed_tnt")
        );
        assert_eq!(entity_types::mob_type_name(50), Some("minecraft:creeper"));
    }

    /// The attribute keys on this wire are not identifiers at all, so the
    /// mapping is load-bearing rather than cosmetic.
    #[test]
    fn legacy_attribute_keys_do_not_parse_as_canonical_identifiers() {
        assert!("generic.maxHealth".parse::<ResourceKey>().is_err());
        assert_eq!(
            attribute_key("generic.maxHealth").unwrap().to_string(),
            "minecraft:max_health"
        );
        assert!(attribute_key("generic.notAnAttribute").is_none());
    }

    /// A chest is the case a fixed metadata of zero would resolve to air, so
    /// the family scan is what makes a chest-lid block event land on a chest.
    #[test]
    fn a_block_family_resolves_past_an_unpopulated_metadata_zero() {
        const CHEST_ID: u8 = 54;
        assert!(!matches!(
            canonical::resolve(CHEST_ID, 0),
            CanonicalBlockState::Resolved(_)
        ));
        assert_eq!(block_family_key(CHEST_ID).to_string(), "minecraft:chest");
    }

    /// The opposite control: an entirely unassigned legacy block type must
    /// reach the typed air fallback rather than a plausible neighbouring block.
    #[test]
    fn an_unassigned_block_family_uses_typed_air() {
        const UNASSIGNED_ID: u8 = 253;
        assert!((0u8..16).all(|meta| !matches!(
            canonical::resolve(UNASSIGNED_ID, meta),
            CanonicalBlockState::Resolved(_)
        )));
        assert_eq!(block_family_key(UNASSIGNED_ID).to_string(), "minecraft:air");
    }

    /// The first move must carry a full position even when the caller starts
    /// at the origin, which the reminder counter's initial value is what
    /// guarantees.
    #[test]
    fn the_first_move_sends_a_position_not_a_bare_on_ground_flag() {
        let adapter = V5Adapter::new();
        let (packet_id, _) = adapter
            .select_move_packet(Vec3::new(0.0, 0.0, 0.0), Rotation::default(), true)
            .unwrap()
            .unwrap();
        assert_eq!(packet_id, play::serverbound::POSITION);
    }

    /// The stance is the eye height above the feet, and a server disconnects
    /// over one outside its tolerance, so it must not be the feet position.
    #[test]
    fn a_position_packet_carries_a_stance_above_its_feet() {
        let adapter = V5Adapter::new();
        let (_, payload) = adapter
            .select_move_packet(Vec3::new(1.0, 64.0, 2.0), Rotation::default(), true)
            .unwrap()
            .unwrap();
        let body: ServerboundPosition = decode_body_exact(&payload).unwrap();
        assert!((body.stance - body.y - STANDING_EYE_HEIGHT).abs() < 1.0e-9);
        assert!(body.stance > body.y);
    }

    /// Construction-time proof that every declared clientbound id resolves to
    /// a handler or a documented ignore reason.
    #[test]
    fn every_declared_clientbound_id_is_accounted_for() {
        let table = V5Adapter::play_dispatch_table();
        assert_eq!(table.len(), CLIENTBOUND.len());
        assert_eq!(
            CLIENTBOUND.len() + IGNORED.len(),
            play::clientbound::ENTRIES.len()
        );
    }
}
