//! [`VersionAdapter`] implementation driving the protocol 754 join flow.

use std::sync::{Arc, LockResult, Mutex, MutexGuard, PoisonError};

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_data::block_entity_types::block_entity_type;
use lodestone_data::mob_effects::mob_effect_name;
use lodestone_model::{
    AdapterError, AnimationAction, BlockActionKind, BlockFace, BossAction, BossColor, BossOverlay,
    ChatKind, ChatMode, ChunkPos, ClientAction, ClientEvent, ClientSettings, CollisionRule,
    ConnectionState, Difficulty, Directive, DisplaySlot, DisplayedSkinParts, EntityInteraction,
    EntityMovement, GameMode, Hand, LoginProfile, MainHand, ObjectiveMode, ObjectiveRenderType,
    PlayerCommand, PlayerListEntry, ProfileProperty, RecipeBookType, ResourceKey,
    ResourcePackResponseKind, Rotation, SectionPos, ServerAddress, TeamAction, TeamColor,
    TeamParameters, TeleportFlags, Text, Vec3, VersionAdapter, Visibility, WorldSink,
};
use lodestone_world::{ChunkPos as WorldChunkPos, Heightmaps, LoadedChunk};

use crate::canonical::{self, FallbackTally};
use crate::entity_types;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::chunk::{ChunkShape, MapChunk, UnloadChunk, UpdateLight};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse};
use crate::packets::entity::{
    EntityDestroy, EntityLook, EntityMoveLook, EntityTeleport, EntityVelocityPacket,
    NamedEntitySpawn, RelEntityMove, SpawnEntityExperienceOrb, SpawnEntityLiving, SpawnObject,
};
use crate::packets::game::{
    AttachEntity, BlockDig, BlockPlace, ClientCommand, ClientboundChat, ClientboundPositionLook,
    Collect, DifficultyPacket, EntityAction, EntityEffect, JoinGame, KickDisconnect,
    OpenSignEntity, PlayerlistHeader, RecipeBook, RemoveEntityEffect, Respawn,
    ServerboundArmAnimation, ServerboundChat, ServerboundFlying, ServerboundLook,
    ServerboundPosition, ServerboundPositionLook, SetPassengers, Spectate, SpawnPosition,
    TeleportConfirm, UpdateHealth, UpdateTime, UseEntity, UseEntityAt, UseEntityInteract,
    UseItem,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{EncryptionRequest, LoginDisconnect, LoginSuccess, SetCompression};
use crate::packets::player_info::{PlayerInfo, PlayerInfoAction};
use crate::packets::position::Position;
use crate::packets::settings::{BrandPayload, PlayerAbilities, ResourcePackReceive, Settings};
use crate::packets::slot::Slot;
use crate::packets::window::{
    CloseWindow, EnchantItem, HeldItemSlot, ServerboundCloseWindow, ServerboundHeldItemSlot,
    SetCreativeSlot,
};

/// Protocol version implemented by this adapter.
///
/// Note the folder name is `v735` and the protocol is **754** (Minecraft
/// 1.16.5). Never derive one from the other — ask [`PROTOCOLS`].
pub const PROTOCOL: i32 = 754;

/// Every protocol number this family speaks — the single source of truth for
/// its coverage.
///
/// [`VersionAdapter::supports`] tests membership here, and
/// `lodestone-registry`'s `FAMILIES` entry points at this same slice, so the
/// registry's view of a family cannot drift from the family's own. That
/// matters more than it looks: the registry needs to answer "does anything
/// handle protocol N?" *without* constructing an adapter, now that
/// construction takes the negotiated protocol (unit U2's multi-protocol seam).
///
/// This family is single-protocol, so the slice has one entry. A
/// multi-protocol family (the plan's v110/v498/v756 groupings) lists each
/// protocol in its wire era here and selects the matching generated
/// `packet_ids` table inside [`adapter_for`].
pub const PROTOCOLS: &[i32] = &[PROTOCOL];

/// Fixed decoding/encoding context for protocol 754.
const CTX: Ctx = Ctx { version: PROTOCOL };

/// Requested next-state value in the handshake for a login connection.
const NEXT_STATE_LOGIN: i32 = 2;

/// Relative-teleport flag bits used by the clientbound 1.8 position packet.
const REL_X: i8 = 0x01;
const REL_Y: i8 = 0x02;
const REL_Z: i8 = 0x04;
const REL_YAW: i8 = 0x08;
const REL_PITCH: i8 = 0x10;

/// Per-connection state used by 1.16.5's `LocalPlayer.sendPosition`.
#[derive(Debug, Clone, Copy)]
struct MovementSendState {
    last_pos: Vec3,
    last_yaw: f32,
    last_pitch: f32,
    last_on_ground: bool,
    position_reminder: u32,
}


impl Default for MovementSendState {
    fn default() -> Self {
        Self {
            last_pos: Vec3::new(0.0, 0.0, 0.0),
            last_yaw: 0.0,
            last_pitch: 0.0,
            last_on_ground: false,
            position_reminder: 0,
        }
    }
}

fn recover_movement_state<'a>(
    result: LockResult<MutexGuard<'a, MovementSendState>>,
) -> MutexGuard<'a, MovementSendState> {
    result.unwrap_or_else(PoisonError::into_inner)
}

/// Version adapter implementing protocol 754 (Minecraft 1.16.5).
///
/// Holds a [`ChunkShape`] for the paletted chunk decoder. In 1.16 the shape no
/// longer depends on the dimension (light left `map_chunk`), so it is constant;
/// the field is kept guarded by a [`Mutex`] purely to satisfy `Sync` and to
/// leave room for per-dimension configuration without an API change.
#[derive(Debug, Clone)]
pub struct V735Adapter {
    shape: Arc<Mutex<ChunkShape>>,
    /// Namespaced world name (e.g. `minecraft:overworld`) from the most
    /// recent `login`/`respawn`, so a packet that identifies its dimension
    /// only implicitly (`spawn_position` carries no dimension field at all)
    /// can still report one.
    current_dimension: Arc<Mutex<String>>,
    movement: Arc<Mutex<MovementSendState>>,
}

impl Default for V735Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl V735Adapter {
    /// Creates a new adapter with the 1.16.5 chunk shape.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shape: Arc::new(Mutex::new(ChunkShape::overworld())),
            current_dimension: Arc::new(Mutex::new("minecraft:overworld".to_owned())),
            movement: Arc::new(Mutex::new(MovementSendState::default())),
        }
    }

    /// Selects the 1.16.5 movement shape. This is deliberately local to the
    /// family: 1.16 shares the 1.12 rule, but not 1.8's idle cadence or the
    /// modern horizontal-collision status bit.
    fn select_move_packet(
        &self,
        pos: Vec3,
        rotation: Rotation,
        on_ground: bool,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        let mut state = recover_movement_state(self.movement.lock());
        let dx = pos.x - state.last_pos.x;
        let dy = pos.y - state.last_pos.y;
        let dz = pos.z - state.last_pos.z;
        state.position_reminder += 1;
        let moved = dx * dx + dy * dy + dz * dz > 9.0e-4 || state.position_reminder >= 20;
        let rotated = rotation.yaw != state.last_yaw || rotation.pitch != state.last_pitch;

        let packet = if moved && rotated {
            let body = ServerboundPositionLook {
                x: pos.x,
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
        } else if state.last_on_ground != on_ground {
            let body = ServerboundFlying { on_ground };
            Some((play::serverbound::FLYING, encode_body(&body)?))
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
        Ok(packet)
    }

    /// Returns the current dimension's chunk shape.
    fn current_shape(&self) -> ChunkShape {
        self.shape
            .lock()
            .map_or_else(|_| ChunkShape::overworld(), |shape| *shape)
    }

    /// Records the namespaced world name from a `login`/`respawn` packet for
    /// later packets (`spawn_position`) that identify their dimension only
    /// implicitly.
    fn set_dimension(&self, world_name: &str) {
        if let Ok(mut current) = self.current_dimension.lock() {
            *current = world_name.to_owned();
        }
    }

    /// Returns the namespaced world name recorded by the most recent
    /// `login`/`respawn`.
    fn current_dimension(&self) -> String {
        self.current_dimension
            .lock()
            .map_or_else(|_| "minecraft:overworld".to_owned(), |value| value.clone())
    }
}

/// Returns a protocol 754 version adapter.
///
/// This free function is the crate's canonical constructor entry point; the
/// client boxes the returned concrete type as a `dyn VersionAdapter`.
#[must_use]
pub fn adapter() -> V735Adapter {
    V735Adapter::new()
}

/// Returns an adapter configured for the **negotiated** protocol.
///
/// The multi-protocol construction seam (unit U2). Before it, every
/// family was built by a zero-argument `make: fn() -> Box<dyn VersionAdapter>`
/// and the negotiated number reached the adapter nowhere — which is precisely
/// what stopped one crate serving several protocol revisions, since it had
/// nothing to select a per-protocol `packet_ids` table by.
///
/// This family is single-protocol, so there is nothing to select and the
/// argument only states which protocol the caller negotiated. Keeping the
/// signature uniform is the point: a grouped family substitutes real table
/// selection here without the registry changing shape.
///
/// # Panics
///
/// Debug builds assert `protocol` is in [`PROTOCOLS`]. The registry always
/// checks membership before constructing, so reaching this with anything else
/// means a caller bypassed that check.
#[must_use]
pub fn adapter_for(protocol: i32) -> V735Adapter {
    debug_assert!(
        PROTOCOLS.contains(&protocol),
        "adapter_for({protocol}) is outside this family's PROTOCOLS ({PROTOCOLS:?}); \
         callers must test membership before constructing"
    );
    V735Adapter::new()
}

/// Encodes a packet body into a fresh byte buffer.
///
/// Thin wrapper over the version-free [`lodestone_core::encode_body`], which
/// returns a stringified error because `AdapterError` lives in
/// `lodestone-model` and `lodestone-core` cannot depend on it.
fn encode_body<T: Encode>(packet: &T) -> Result<Vec<u8>, AdapterError> {
    lodestone_core::encode_body(packet, CTX).map_err(AdapterError::Encode)
}

/// Maps the model's `RecipeBookType` onto the ordinal 1.16.5's `recipe_book`
/// packet expects. All four recipe books (crafting/furnace/blast
/// furnace/smoker) exist by 1.16, unlike 1.12.2 which has only the crafting
/// one; the ordinal itself has held stable from 1.13 through protocol 776.
fn recipe_book_type_to_ordinal(book_type: RecipeBookType) -> i32 {
    match book_type {
        RecipeBookType::Crafting => 0,
        RecipeBookType::Furnace => 1,
        RecipeBookType::BlastFurnace => 2,
        RecipeBookType::Smoker => 3,
    }
}

/// Decodes a packet body from raw bytes.
fn decode_body<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    lodestone_core::decode_body(payload, CTX).map_err(AdapterError::Decode)
}

/// Like [`decode_body`] but additionally requires the payload to be fully
/// consumed. Used for packets whose whole body we decode (e.g. the entity
/// destroy id list), where trailing bytes signal a wrong layout and must be
/// rejected rather than silently ignored. Packets that deliberately leave a
/// tail unread (metadata terminators, fields we don't model yet) keep using the
/// lenient [`decode_body`].
fn decode_body_exact<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    lodestone_core::decode_body_exact(payload, CTX).map_err(AdapterError::Decode)
}

/// Builds a [`Directive::Send`] from a packet id and an encodable body.
fn send<T: Encode>(packet_id: i32, packet: &T) -> Result<Directive, AdapterError> {
    Ok(Directive::Send {
        packet_id,
        payload: encode_body(packet)?,
    })
}

/// Decodes a 1.8 JSON disconnect reason into a full [`Text`] tree via the
/// shared [`Text::from_json`] front-end, falling back to a generic message when
/// the component carries no text.
fn json_reason_text(reason: &str) -> Text {
    let text = Text::from_json(reason);
    if text.to_plain_string().is_empty() {
        Text::literal("Disconnected")
    } else {
        text
    }
}

/// Maps the low bits of a 1.8 game-mode byte to the canonical [`GameMode`].
///
/// The `0x8` bit marks a hardcore world and is masked off here; the canonical
/// model has no hardcore flag.
fn game_mode(value: u8) -> Result<GameMode, AdapterError> {
    match value & 0x7 {
        0 => Ok(GameMode::Survival),
        1 => Ok(GameMode::Creative),
        2 => Ok(GameMode::Adventure),
        3 => Ok(GameMode::Spectator),
        other => Err(AdapterError::Decode(format!("unknown game mode {other}"))),
    }
}

/// Parses a 1.16 namespaced world name (e.g. `minecraft:overworld`) into a
/// canonical [`DimensionId`](lodestone_model::DimensionId).
///
/// # 1.16 divergence
///
/// Pre-1.16 join packets carried the dimension as a signed integer (`-1`
/// nether, `0` overworld, `1` end); 1.16 replaced that with a namespaced
/// **world name** string alongside an NBT dimension codec. The adapter maps the
/// string straight through — the model already speaks namespaced identifiers —
/// so no numeric table is involved.
fn dimension_id(name: &str) -> Result<lodestone_model::DimensionId, AdapterError> {
    name.parse()
        .map_err(|_| AdapterError::Decode(format!("invalid dimension identifier {name}")))
}

/// Maps the 1.8 clientbound chat `position` byte to a canonical [`ChatKind`].
const fn chat_kind(position: i8) -> ChatKind {
    match position {
        1 => ChatKind::System,
        2 => ChatKind::GameInfo,
        _ => ChatKind::Chat,
    }
}

/// Delta-position scale for 1.9+ `rel_entity_move` / `entity_move_look`: each
/// `i16` is `1/4096` of a block (1.8 used a signed byte in `1/32` units).
const MOVE_DELTA_SCALE: f64 = 4096.0;

/// Velocity scale shared by the velocity packets: each `i16` is `1/8000` of a
/// block per tick.
const VELOCITY_SCALE: f64 = 8000.0;

/// Converts a signed-byte angle to degrees (256 steps per full circle).
///
/// Delegates to the version-free [`lodestone_core::unpack_degrees`], which has
/// the same formula and is used identically by v47 and v340.
fn unpack_degrees(packed: i8) -> f32 {
    lodestone_core::unpack_degrees(packed)
}

/// Maps a canonical [`BlockFace`] to its numeric ordinal
/// (`Down=0, Up=1, North=2, South=3, West=4, East=5`), used by both `block_dig`
/// and `block_place`.
const fn face_ordinal(face: BlockFace) -> i32 {
    match face {
        BlockFace::Down => 0,
        BlockFace::Up => 1,
        BlockFace::North => 2,
        BlockFace::South => 3,
        BlockFace::West => 4,
        BlockFace::East => 5,
    }
}

/// Maps a canonical [`Hand`] to its numeric ordinal (`Main=0, Off=1`), for the
/// 1.9+ hand-carrying serverbound packets.
const fn hand_ordinal(hand: Hand) -> i32 {
    match hand {
        Hand::Main => 0,
        Hand::Off => 1,
    }
}

/// Packs a [`DisplayedSkinParts`] into the skin-parts bitmask.
const fn skin_parts_bits(parts: DisplayedSkinParts) -> u8 {
    (parts.cape as u8)
        | ((parts.jacket as u8) << 1)
        | ((parts.left_sleeve as u8) << 2)
        | ((parts.right_sleeve as u8) << 3)
        | ((parts.left_pants_leg as u8) << 4)
        | ((parts.right_pants_leg as u8) << 5)
        | ((parts.hat as u8) << 6)
}

/// Maps a canonical [`ChatMode`] to the wire chat-visibility value
/// (`0` full, `1` commands only, `2` hidden).
const fn chat_mode_value(mode: ChatMode) -> i32 {
    match mode {
        ChatMode::Full => 0,
        ChatMode::CommandsOnly => 1,
        ChatMode::Hidden => 2,
    }
}

/// Maps a canonical [`MainHand`] to the wire value (`0` left, `1` right).
const fn main_hand_value(hand: MainHand) -> i32 {
    match hand {
        MainHand::Left => 0,
        MainHand::Right => 1,
    }
}

/// The vanilla ability flag bit set when the player is invulnerable.
const ABILITY_INVULNERABLE: i8 = 0x01;
/// The vanilla flying-ability flag bit set when the client is flying.
const ABILITY_FLYING: i8 = 0x02;
/// The vanilla ability flag bit set when the player may fly.
const ABILITY_CAN_FLY: i8 = 0x04;
/// The vanilla ability flag bit set when the player may instantly build/break.
const ABILITY_INSTABUILD: i8 = 0x08;

/// Converts a low-level [`lodestone_core::Error`] into an [`AdapterError`].
///
/// Shared by every hand-`Reader`-decoded packet in `handle_play`, matching
/// `lodestone-v340`'s own convention for multiplexed/action-tagged packets
/// no derive attribute can express.
fn dec_err(err: impl std::fmt::Display) -> AdapterError {
    AdapterError::Decode(err.to_string())
}

/// Maps a `boss_bar` varint colour ordinal to the canonical [`BossColor`].
fn boss_color_from_ordinal(ordinal: i32) -> Result<BossColor, AdapterError> {
    match ordinal {
        0 => Ok(BossColor::Pink),
        1 => Ok(BossColor::Blue),
        2 => Ok(BossColor::Red),
        3 => Ok(BossColor::Green),
        4 => Ok(BossColor::Yellow),
        5 => Ok(BossColor::Purple),
        6 => Ok(BossColor::White),
        other => Err(AdapterError::Decode(format!("unknown boss bar color {other}"))),
    }
}

/// Maps a `boss_bar` varint overlay/division ordinal to the canonical
/// [`BossOverlay`]. Vanilla's `BossEvent.BossBarOverlay` declaration order
/// (`PROGRESS`, `NOTCHED_6`, `NOTCHED_10`, `NOTCHED_12`, `NOTCHED_20`)
/// matches [`BossOverlay`]'s own order exactly.
fn boss_overlay_from_ordinal(ordinal: i32) -> Result<BossOverlay, AdapterError> {
    match ordinal {
        0 => Ok(BossOverlay::Progress),
        1 => Ok(BossOverlay::Notched6),
        2 => Ok(BossOverlay::Notched10),
        3 => Ok(BossOverlay::Notched12),
        4 => Ok(BossOverlay::Notched20),
        other => Err(AdapterError::Decode(format!("unknown boss bar overlay {other}"))),
    }
}

/// Maps a `teams` varint colour/formatting ordinal to the canonical
/// [`TeamColor`].
///
/// Vanilla packs this as an `EnumChatFormatting`/`ChatFormatting` ordinal
/// (the same 16-colour `§`-code order [`lodestone_model::TextColor`]'s own
/// `NAMED` table walks), so `-1` ("no colour"/reset) and any other value
/// outside `0..=15` both resolve to `None` rather than being rejected — a
/// team legitimately has no colour, and this is not a wire-shape error the
/// way an unrecognised `teams` mode or an unrecognised objective render type
/// is.
fn team_color_from_ordinal(ordinal: i32) -> Option<TeamColor> {
    match ordinal {
        0 => Some(TeamColor::Black),
        1 => Some(TeamColor::DarkBlue),
        2 => Some(TeamColor::DarkGreen),
        3 => Some(TeamColor::DarkAqua),
        4 => Some(TeamColor::DarkRed),
        5 => Some(TeamColor::DarkPurple),
        6 => Some(TeamColor::Gold),
        7 => Some(TeamColor::Gray),
        8 => Some(TeamColor::DarkGray),
        9 => Some(TeamColor::Blue),
        10 => Some(TeamColor::Green),
        11 => Some(TeamColor::Aqua),
        12 => Some(TeamColor::Red),
        13 => Some(TeamColor::LightPurple),
        14 => Some(TeamColor::Yellow),
        15 => Some(TeamColor::White),
        _ => None,
    }
}

impl V735Adapter {
    /// Handles a clientbound packet while in the login state.
    ///
    /// # Architectural finding
    ///
    /// 1.8 has no Configuration state and no `login_acknowledged` packet. On
    /// login `success` the adapter simply transitions straight to Play with a
    /// single `SetState(Play)` directive — there is nothing to acknowledge and
    /// no client-information handshake to send first. The existing [`Directive`]
    /// vocabulary expresses this cleanly; the only unused concept is
    /// [`ConnectionState::Configuration`], which 1.8 never enters.
    fn handle_login(&self, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == login::clientbound::COMPRESS {
            let body: SetCompression = decode_body(payload)?;
            return Ok(vec![Directive::SetCompression(body.threshold)]);
        }
        if packet_id == login::clientbound::SUCCESS {
            // Validate the profile decodes (string UUID + name), then advance.
            let _profile: LoginSuccess = decode_body(payload)?;
            return Ok(vec![Directive::SetState(ConnectionState::Play)]);
        }
        if packet_id == login::clientbound::ENCRYPTION_BEGIN {
            let _request: EncryptionRequest = decode_body(payload)?;
            return Err(AdapterError::Unsupported(
                "encryption / online-mode authentication (login encryption_begin) is not yet \
                 implemented; connect to an offline-mode server"
                    .to_owned(),
            ));
        }
        if packet_id == login::clientbound::DISCONNECT {
            let body: LoginDisconnect = decode_body(payload)?;
            return Ok(vec![Directive::Disconnect(json_reason_text(&body.reason))]);
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
            let body: JoinGame = decode_body(payload)?;
            self.set_dimension(&body.world_name);
            return Ok(vec![Directive::Emit(ClientEvent::Login {
                entity_id: body.entity_id,
                game_mode: game_mode(body.game_mode)?,
                dimension: dimension_id(&body.world_name)?,
            })]);
        }
        if packet_id == play::clientbound::MAP_CHUNK {
            // Decode the paletted 1.16.5 column into version-free storage and
            // apply it to the world through the sink, emitting only a
            // lightweight notification. Light no longer travels here (1.14 split
            // it into update_light), so the loaded column carries empty light
            // until the matching update_light arrives.
            let shape = self.current_shape();
            let mut reader = Reader::new(payload);
            let data = MapChunk::decode(&mut reader, &shape)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            // Zero trailing bytes across the whole packet is the single best
            // detector of a subtly wrong layout: reject rather than apply a
            // silently misaligned chunk.
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let pos = ChunkPos::new(data.x, data.z);
            world.load(
                WorldChunkPos::new(data.x, data.z),
                LoadedChunk::new(data.column, data.light, Heightmaps::new(), Vec::new()),
            );
            return Ok(vec![Directive::Emit(ClientEvent::ChunkLoaded { pos })]);
        }
        if packet_id == play::clientbound::UPDATE_LIGHT {
            // 1.14+ delivers light separately from the chunk column. Decode the
            // per-section nibble arrays into a version-free LightPatch and merge
            // it onto the already-loaded column; a light update for an unloaded
            // column is a harmless no-op in the world store.
            let mut reader = Reader::new(payload);
            let update = UpdateLight::decode(&mut reader)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            world.merge_light(WorldChunkPos::new(update.x, update.z), update.patch);
            return Ok(Vec::new());
        }
        if packet_id == play::clientbound::UNLOAD_CHUNK {
            // 1.16.5 has a dedicated forget packet (two ints).
            let body: UnloadChunk = decode_body(payload)?;
            let pos = ChunkPos::new(body.chunk_x, body.chunk_z);
            world.unload(WorldChunkPos::new(body.chunk_x, body.chunk_z));
            return Ok(vec![Directive::Emit(ClientEvent::ChunkUnloaded { pos })]);
        }
        if packet_id == play::clientbound::KEEP_ALIVE {
            let keep_alive: KeepAliveRequest = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::KeepAlive {
                id: keep_alive.id,
            })]);
        }
        if packet_id == play::clientbound::CHAT {
            let body: ClientboundChat = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text: Text::from_json(&body.message),
                kind: chat_kind(body.position),
                // 1.16's chat packet carries no sender field — nothing to filter on.
                sender: None,
                ack: None,
            })]);
        }
        if packet_id == play::clientbound::POSITION {
            let body: ClientboundPositionLook = decode_body(payload)?;
            let flags = TeleportFlags {
                relative_x: body.flags & REL_X != 0,
                relative_y: body.flags & REL_Y != 0,
                relative_z: body.flags & REL_Z != 0,
                relative_yaw: body.flags & REL_YAW != 0,
                relative_pitch: body.flags & REL_PITCH != 0,
            };
            // 1.9+ requires echoing the teleport id back or the server
            // rubber-bands the player. This confirm choreography lives entirely
            // in the version crate; the driver just runs the directives in order.
            let confirm = TeleportConfirm {
                teleport_id: body.teleport_id,
            };
            return Ok(vec![
                send(play::serverbound::TELEPORT_CONFIRM, &confirm)?,
                Directive::Emit(ClientEvent::TeleportPlayer {
                    pos: Vec3::new(body.x, body.y, body.z),
                    rotation: Rotation::new(body.yaw, body.pitch),
                    flags,
                }),
            ]);
        }
        if packet_id == play::clientbound::SPAWN_ENTITY_LIVING {
            let body: SpawnEntityLiving = decode_body(payload)?;
            let entity_type = entity_types::mob_type_name(body.kind)
                .ok_or_else(|| {
                    AdapterError::Decode(format!("unknown mob type id {} in spawn", body.kind))
                })?
                .parse()
                .map_err(|_| {
                    AdapterError::Decode(format!("mob type id {} is not a key", body.kind))
                })?;
            return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: body.entity_id,
                uuid: Some(body.entity_uuid),
                entity_type,
                pos: Vec3::new(body.x, body.y, body.z),
                rotation: Rotation::new(unpack_degrees(body.yaw), unpack_degrees(body.pitch)),
                velocity: Some(Vec3::new(
                    f64::from(body.velocity_x) / VELOCITY_SCALE,
                    f64::from(body.velocity_y) / VELOCITY_SCALE,
                    f64::from(body.velocity_z) / VELOCITY_SCALE,
                )),
            })]);
        }
        if packet_id == play::clientbound::SPAWN_ENTITY {
            let body: SpawnObject = decode_body(payload)?;
            let type_id = body.kind;
            let entity_type = entity_types::object_type_name(type_id)
                .ok_or_else(|| {
                    AdapterError::Decode(format!("unknown object type id {type_id} in spawn"))
                })?
                .parse()
                .map_err(|_| {
                    AdapterError::Decode(format!("object type id {type_id} is not a key"))
                })?;
            // 1.16 always includes velocity, but a stationary object still
            // reports zero; forward `None` only when all components are zero to
            // match the semantic "no motion" rather than "explicit zero motion".
            let velocity = if body.velocity_x == 0 && body.velocity_y == 0 && body.velocity_z == 0 {
                None
            } else {
                Some(Vec3::new(
                    f64::from(body.velocity_x) / VELOCITY_SCALE,
                    f64::from(body.velocity_y) / VELOCITY_SCALE,
                    f64::from(body.velocity_z) / VELOCITY_SCALE,
                ))
            };
            return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: body.entity_id,
                uuid: Some(body.object_uuid),
                entity_type,
                pos: Vec3::new(body.x, body.y, body.z),
                rotation: Rotation::new(unpack_degrees(body.yaw), unpack_degrees(body.pitch)),
                velocity,
            })]);
        }
        if packet_id == play::clientbound::NAMED_ENTITY_SPAWN {
            let body: NamedEntitySpawn = decode_body(payload)?;
            let entity_type = entity_types::PLAYER
                .parse()
                .map_err(|_| AdapterError::Decode("player key invalid".to_owned()))?;
            return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: body.entity_id,
                uuid: Some(body.player_uuid),
                entity_type,
                pos: Vec3::new(body.x, body.y, body.z),
                rotation: Rotation::new(unpack_degrees(body.yaw), unpack_degrees(body.pitch)),
                velocity: None,
            })]);
        }
        if packet_id == play::clientbound::REL_ENTITY_MOVE {
            let body: RelEntityMove = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
                entity_id: body.entity_id,
                movement: EntityMovement::Relative(Vec3::new(
                    f64::from(body.dx) / MOVE_DELTA_SCALE,
                    f64::from(body.dy) / MOVE_DELTA_SCALE,
                    f64::from(body.dz) / MOVE_DELTA_SCALE,
                )),
                rotation: None,
                on_ground: body.on_ground,
            })]);
        }
        if packet_id == play::clientbound::ENTITY_LOOK {
            let body: EntityLook = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
                entity_id: body.entity_id,
                movement: EntityMovement::Relative(Vec3::new(0.0, 0.0, 0.0)),
                rotation: Some(Rotation::new(
                    unpack_degrees(body.yaw),
                    unpack_degrees(body.pitch),
                )),
                on_ground: body.on_ground,
            })]);
        }
        if packet_id == play::clientbound::ENTITY_MOVE_LOOK {
            let body: EntityMoveLook = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
                entity_id: body.entity_id,
                movement: EntityMovement::Relative(Vec3::new(
                    f64::from(body.dx) / MOVE_DELTA_SCALE,
                    f64::from(body.dy) / MOVE_DELTA_SCALE,
                    f64::from(body.dz) / MOVE_DELTA_SCALE,
                )),
                rotation: Some(Rotation::new(
                    unpack_degrees(body.yaw),
                    unpack_degrees(body.pitch),
                )),
                on_ground: body.on_ground,
            })]);
        }
        if packet_id == play::clientbound::ENTITY_TELEPORT {
            let body: EntityTeleport = decode_body(payload)?;
            // 1.9+ sends the absolute position directly as `f64`; no
            // fixed-point conversion, unlike 1.8.
            return Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
                entity_id: body.entity_id,
                movement: EntityMovement::Absolute(Vec3::new(body.x, body.y, body.z)),
                rotation: Some(Rotation::new(
                    unpack_degrees(body.yaw),
                    unpack_degrees(body.pitch),
                )),
                on_ground: body.on_ground,
            })]);
        }
        if packet_id == play::clientbound::ENTITY_VELOCITY {
            let body: EntityVelocityPacket = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityVelocity {
                entity_id: body.entity_id,
                velocity: Vec3::new(
                    f64::from(body.velocity_x) / VELOCITY_SCALE,
                    f64::from(body.velocity_y) / VELOCITY_SCALE,
                    f64::from(body.velocity_z) / VELOCITY_SCALE,
                ),
            })]);
        }
        if packet_id == play::clientbound::ENTITY_DESTROY {
            // A varint-counted list of varint ids. Now a derived struct: the
            // `#[mc(varint)]`-on-`Vec<i32>` macro attribute (reported as a gap
            // and since landed) encodes both the length and each element as a
            // varint, replacing the former hand-decoded loop.
            let body: EntityDestroy = decode_body_exact(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityRemoved {
                entity_ids: body.entity_ids,
            })]);
        }
        if packet_id == play::clientbound::KICK_DISCONNECT {
            let body: KickDisconnect = decode_body(payload)?;
            return Ok(vec![Directive::Disconnect(json_reason_text(&body.reason))]);
        }
        if packet_id == play::clientbound::UPDATE_HEALTH {
            // f32 health, varint food, f32 saturation — verified against
            // minecraft-data's 1.16.2 `packet_update_health` (byte-identical
            // to 1.12.2's shape). `UpdateHealth` already existed in this
            // crate but was only ever round-tripped in `tests/join_flow.rs`,
            // never wired into `handle_play` — an island per CLAUDE.md's own
            // definition.
            let body: UpdateHealth = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::HealthChanged {
                health: body.health,
                food: body.food,
                saturation: body.food_saturation,
            })]);
        }
        if packet_id == play::clientbound::RESPAWN {
            // Like `login`, 1.16 replaced the numeric dimension with a
            // namespaced `world_name` string plus an inline raw named-NBT
            // dimension type — see `Respawn`'s own doc. `Respawn` already
            // existed and was already round-trip tested, but nothing here
            // ever dispatched it: another island.
            let body: Respawn = decode_body(payload)?;
            self.set_dimension(&body.world_name);
            return Ok(vec![Directive::Emit(ClientEvent::Respawned {
                dimension: dimension_id(&body.world_name)?,
                game_mode: game_mode(body.game_mode)?,
                previous_game_mode: None,
                last_death_location: None,
            })]);
        }
        if packet_id == play::clientbound::SPAWN_POSITION {
            // A single packed `position`, verified against minecraft-data's
            // 1.16.2 `packet_spawn_position`. `SpawnPosition` already
            // existed and was already round-trip tested; nothing here ever
            // dispatched it. This protocol revision carries no angle or
            // dimension field (both are later additions), so `angle`/
            // `pitch` are `0.0` and `dimension` comes from the adapter's own
            // `current_dimension` (set by the most recent `login`/`respawn`).
            let body: SpawnPosition = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
                dimension: dimension_id(&self.current_dimension())?,
                pos: body.location.0,
                angle: 0.0,
                pitch: 0.0,
            })]);
        }
        if packet_id == play::clientbound::ENTITY_STATUS {
            // A raw (non-VarInt) `i32` entity id, then a raw status byte —
            // verified against minecraft-data's 1.16.2 `packet_entity_status`
            // (byte-identical to 1.12.2's/1.8's shape).
            let mut reader = Reader::new(payload);
            let entity_id = reader.i32().map_err(dec_err)?;
            let status = reader.u8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityStatus {
                entity_id,
                status,
            })]);
        }
        if packet_id == play::clientbound::ENTITY_HEAD_ROTATION {
            // VarInt entity id, then a packed signed-byte yaw — verified
            // against minecraft-data's 1.16.2 `packet_entity_head_rotation`.
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let packed = reader.i8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityHeadRotation {
                entity_id,
                head_yaw: unpack_degrees(packed),
            })]);
        }
        if packet_id == play::clientbound::ANIMATION {
            // VarInt entity id, then a raw `u8` animation id — verified
            // against minecraft-data's 1.16.2 `packet_animation`. Unlike
            // 1.12.2 (which has no dedicated hurt animation, so id `2` there
            // means "leave bed"), 1.9+ folds "leave bed" out and adds a
            // dedicated critical/magic-critical pair; `AnimationAction`'s
            // `Other` fallback carries anything this table does not name.
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let animation = reader.u8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let action = match animation {
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
        if packet_id == play::clientbound::ABILITIES {
            // Signed-byte flags (bit 0x01 invulnerable, 0x02 flying, 0x04
            // can fly, 0x08 instabuild), then f32 flying speed, f32 walking
            // speed — verified against minecraft-data's 1.16.2
            // `packet_abilities` (byte-identical to 1.12.2's/1.8's shape).
            // 1.16.5 reuses one packet *name* for both directions with
            // different flag semantics (the serverbound `abilities` this
            // crate already encodes for `SetFlying` carries only the flying
            // bit); the clientbound shape decoded here is byte-identical, so
            // it is hand-decoded rather than routed through the
            // serverbound-tagged [`PlayerAbilities`] struct to avoid
            // conflating the two directions' meaning.
            let mut reader = Reader::new(payload);
            let flags = reader.i8().map_err(dec_err)?;
            let flying_speed = reader.f32().map_err(dec_err)?;
            let walking_speed = reader.f32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::AbilitiesChanged {
                invulnerable: flags & ABILITY_INVULNERABLE != 0,
                flying: flags & ABILITY_FLYING != 0,
                can_fly: flags & ABILITY_CAN_FLY != 0,
                instabuild: flags & ABILITY_INSTABUILD != 0,
                flying_speed,
                walking_speed,
            })]);
        }
        if packet_id == play::clientbound::DIFFICULTY {
            let body: DifficultyPacket = decode_body_exact(payload)?;
            let difficulty = match body.difficulty {
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
                locked: body.difficulty_locked,
            })]);
        }
        if packet_id == play::clientbound::UPDATE_TIME {
            let body: UpdateTime = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::TimeChanged {
                world_age: body.age,
                time_of_day: body.time,
            })]);
        }
        if packet_id == play::clientbound::PLAYERLIST_HEADER {
            let body: PlayerlistHeader = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::TabListChanged {
                header: Text::from_json(&body.header),
                footer: Text::from_json(&body.footer),
            })]);
        }
        if packet_id == play::clientbound::ATTACH_ENTITY {
            let body: AttachEntity = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityLeashed {
                entity_id: body.entity_id,
                holder_id: (body.vehicle_id != 0).then_some(body.vehicle_id),
            })]);
        }
        if packet_id == play::clientbound::SET_PASSENGERS {
            let body: SetPassengers = decode_body(payload)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::EntityPassengersChanged {
                    vehicle_id: body.entity_id,
                    passenger_ids: body.passengers,
                },
            )]);
        }
        if packet_id == play::clientbound::COLLECT {
            let body: Collect = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::ItemPickup {
                item_entity_id: body.collected_entity_id,
                player_id: body.collector_entity_id,
                amount: body.pickup_item_count,
            })]);
        }
        if packet_id == play::clientbound::ENTITY_EFFECT {
            let body: EntityEffect = decode_body(payload)?;
            // Legacy (1-based) effect id; the shared `lodestone-data`
            // registry table is the 0-based modern `minecraft:mob_effect`
            // id, and the two id spaces have been stable in the same
            // relative order since Minecraft Beta 1.8.
            let name = mob_effect_name(i32::from(body.effect_id) - 1).ok_or_else(|| {
                AdapterError::Decode(format!("unknown legacy effect id {}", body.effect_id))
            })?;
            let effect: ResourceKey = name
                .parse()
                .map_err(|_| AdapterError::Decode(format!("effect id {name} is not a key")))?;
            return Ok(vec![Directive::Emit(ClientEvent::MobEffectApplied {
                entity_id: body.entity_id,
                effect,
                amplifier: i32::from(body.amplifier),
                duration_ticks: body.duration,
                ambient: body.flags & 0x01 != 0,
                visible: body.flags & 0x02 != 0,
                // 1.16.5 postdates 1.13, so unlike 1.12.2 the "show icon" bit
                // is real (cross-checked against `lodestone-v770`'s
                // `UPDATE_MOB_EFFECT` decode, same three low bits); "blend"
                // is a 1.19+ addition this protocol predates.
                show_icon: body.flags & 0x04 != 0,
                blend: false,
            })]);
        }
        if packet_id == play::clientbound::REMOVE_ENTITY_EFFECT {
            let body: RemoveEntityEffect = decode_body(payload)?;
            let name = mob_effect_name(i32::from(body.effect_id) - 1).ok_or_else(|| {
                AdapterError::Decode(format!("unknown legacy effect id {}", body.effect_id))
            })?;
            let effect: ResourceKey = name
                .parse()
                .map_err(|_| AdapterError::Decode(format!("effect id {name} is not a key")))?;
            return Ok(vec![Directive::Emit(ClientEvent::MobEffectRemoved {
                entity_id: body.entity_id,
                effect,
            })]);
        }
        if packet_id == play::clientbound::SPAWN_ENTITY_EXPERIENCE_ORB {
            let body: SpawnEntityExperienceOrb = decode_body(payload)?;
            let entity_type: ResourceKey = "minecraft:experience_orb"
                .parse()
                .map_err(|_| AdapterError::Decode("experience_orb key invalid".to_owned()))?;
            return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: body.entity_id,
                uuid: None,
                entity_type,
                pos: Vec3::new(body.x, body.y, body.z),
                rotation: Rotation::new(0.0, 0.0),
                velocity: None,
            })]);
        }
        if packet_id == play::clientbound::BLOCK_CHANGE {
            // A packed 1.14+ `position` (x/z/y bit order, unlike the pre-1.14
            // x/y/z order), then a varint **flat block-state id** — verified
            // against minecraft-data's 1.16.2 `packet_block_change`. 1.16.5
            // is post-Flattening, so unlike `lodestone-v340`'s legacy
            // `(id << 4) | meta` composite there is no metadata split: the
            // wire value is already a single state id in *this protocol's
            // own* id space, bridged to a real 26.2 state id via
            // `crate::canonical::resolve_or_air` — the same table
            // `packets/chunk.rs` uses for paletted chunk sections.
            let mut reader = Reader::new(payload);
            let pos: Position = Position::decode(&mut reader, CTX).map_err(dec_err)?;
            let raw = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let raw = u32::try_from(raw).map_err(|_| {
                AdapterError::Decode(format!("block_change state id {raw} is negative"))
            })?;
            let mut tally = FallbackTally::default();
            let state = canonical::resolve_or_air(raw, &mut tally);
            let pos = pos.0;
            world.set_block(pos.x, pos.y, pos.z, state);
            // Writing a state is what creates/removes a block entity in
            // vanilla (`LevelChunk.setBlockState`, no packet involved).
            world.sync_block_entity(pos.x, pos.y, pos.z, block_entity_type(state));
            return Ok(vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
                section: SectionPos::new(pos.x >> 4, pos.y >> 4, pos.z >> 4),
                blocks: vec![[
                    pos.x.rem_euclid(16) as u8,
                    pos.y.rem_euclid(16) as u8,
                    pos.z.rem_euclid(16) as u8,
                ]],
            })]);
        }
        if packet_id == play::clientbound::EXPERIENCE {
            // f32 progress bar, varint level, varint total — verified
            // against minecraft-data's 1.16.2 `packet_experience`.
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
        if packet_id == play::clientbound::VEHICLE_MOVE {
            // f64 x/y/z, f32 yaw/pitch — verified against minecraft-data's
            // 1.16.2 `packet_vehicle_move`.
            let mut reader = Reader::new(payload);
            let x = reader.f64().map_err(dec_err)?;
            let y = reader.f64().map_err(dec_err)?;
            let z = reader.f64().map_err(dec_err)?;
            let yaw = reader.f32().map_err(dec_err)?;
            let pitch = reader.f32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::VehicleMoved {
                pos: Vec3::new(x, y, z),
                yaw,
                pitch,
            })]);
        }
        if packet_id == play::clientbound::SELECT_ADVANCEMENT_TAB {
            // A single optional string tab id — verified against
            // minecraft-data's 1.16.2 `packet_select_advancement_tab`.
            let mut reader = Reader::new(payload);
            let present = reader.bool().map_err(dec_err)?;
            let tab = if present {
                let id = reader.string(256).map_err(dec_err)?;
                Some(id.parse().map_err(|_| {
                    AdapterError::Decode(format!("advancement tab id {id} is not an identifier"))
                })?)
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::AdvancementsTabSelected {
                tab,
            })]);
        }
        if packet_id == play::clientbound::OPEN_SIGN_ENTITY {
            let body: OpenSignEntity = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::SignEditorOpened {
                pos: body.location.0,
                // 1.16.5 predates the front/back sign text split (added
                // 1.20); every editable sign has only the one (front) text
                // at this protocol revision.
                is_front_text: true,
            })]);
        }
        if packet_id == play::clientbound::CAMERA {
            // A single varint entity id — verified against minecraft-data's
            // 1.16.2 `packet_camera`.
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::CameraSet { entity_id })]);
        }
        if packet_id == play::clientbound::UPDATE_VIEW_POSITION {
            // Two varints, chunk x/z — verified against minecraft-data's
            // 1.16.2 `packet_update_view_position`.
            let mut reader = Reader::new(payload);
            let x = reader.var_i32().map_err(dec_err)?;
            let z = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ChunkCacheCenterChanged {
                x,
                z,
            })]);
        }
        if packet_id == play::clientbound::UPDATE_VIEW_DISTANCE {
            // A single varint view distance — verified against
            // minecraft-data's 1.16.2 `packet_update_view_distance`.
            let mut reader = Reader::new(payload);
            let radius = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ChunkCacheRadiusChanged {
                radius,
            })]);
        }
        if packet_id == play::clientbound::HELD_ITEM_SLOT {
            // A single signed byte, the newly-selected hotbar index —
            // verified against minecraft-data's 1.16.2
            // `packet_held_item_slot`. `HeldItemSlot`'s codec already
            // existed (`packets/window.rs`) and was already round-trip
            // tested; nothing here ever dispatched it.
            let body: HeldItemSlot = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::HeldSlotChanged {
                slot: i32::from(body.slot),
            })]);
        }
        if packet_id == play::clientbound::CLOSE_WINDOW {
            let body: CloseWindow = decode_body_exact(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::ScreenClosed {
                window_id: i32::from(body.window_id),
            })]);
        }
        if packet_id == play::clientbound::CRAFT_PROGRESS_BAR {
            // `packet_craft_progress_bar` (minecraft-data 1.16.2, identical
            // to 1.8's/1.12.2's shape): `windowId: u8, property: i16, value:
            // i16` — no synchronization state id, so it maps directly onto
            // the same `ContainerData` 26.2's `minecraft:container_set_data`
            // produces.
            let mut reader = Reader::new(payload);
            let window_id = i32::from(reader.u8().map_err(dec_err)?);
            let property = i32::from(reader.i16().map_err(dec_err)?);
            let value = i32::from(reader.i16().map_err(dec_err)?);
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::ContainerData {
                window_id,
                property,
                value,
            })]);
        }
        if packet_id == play::clientbound::TITLE {
            // Action-multiplexed, verified field-by-field against
            // minecraft-data's 1.16.2 `packet_title` (identical to 1.12.2's
            // shape): the `text` switch has three cases (`0`/`1`/`2` —
            // title/subtitle/action-bar), the fade-in/stay/fade-out case
            // (times) is `3`, and the two argument-less actions are `4`/`5`.
            // Action-bar text always renders as an overlay, so it maps to
            // the same `Chat` `GameInfo` event the dedicated
            // `SET_ACTION_BAR_TEXT` packet uses on 26.2 — 1.16.5 predates
            // that split packet, it rides this one instead. `4`/`5` are
            // clear-then-reset, the same pair 26.2's `CLEAR_TITLES` folds
            // into one `resetTimes` bool.
            let mut reader = Reader::new(payload);
            let action = reader.var_i32().map_err(dec_err)?;
            let directive = match action {
                0 => {
                    let text = reader.string(32_767).map_err(dec_err)?;
                    Directive::Emit(ClientEvent::TitleText {
                        text: Text::from_json(&text),
                    })
                }
                1 => {
                    let text = reader.string(32_767).map_err(dec_err)?;
                    Directive::Emit(ClientEvent::SubtitleText {
                        text: Text::from_json(&text),
                    })
                }
                2 => {
                    let text = reader.string(32_767).map_err(dec_err)?;
                    Directive::Emit(ClientEvent::Chat {
                        text: Text::from_json(&text),
                        kind: ChatKind::GameInfo,
                        sender: None,
                        ack: None,
                    })
                }
                3 => {
                    let fade_in = reader.i32().map_err(dec_err)?;
                    let stay = reader.i32().map_err(dec_err)?;
                    let fade_out = reader.i32().map_err(dec_err)?;
                    Directive::Emit(ClientEvent::TitlesAnimation {
                        fade_in,
                        stay,
                        fade_out,
                    })
                }
                4 => Directive::Emit(ClientEvent::TitlesCleared { reset_times: false }),
                5 => Directive::Emit(ClientEvent::TitlesCleared { reset_times: true }),
                other => {
                    return Err(AdapterError::Decode(format!("unknown title action {other}")));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![directive]);
        }
        if packet_id == play::clientbound::TAB_COMPLETE {
            // `packet_tab_complete` (minecraft-data 1.16.2, its data source
            // for 1.16.5 per `dataPaths.json`): `transactionId: varint,
            // start: varint, length: varint, matches: [{match: string,
            // tooltip: option<string>}]` — full parity with 26.2's
            // `CommandSuggestionsResponse` shape (1.13 introduced this
            // range-based form), so no client-side bookkeeping is needed the
            // way v47/v340 need for their pre-1.13 bare-string-list shape.
            let mut reader = Reader::new(payload);
            let id = reader.var_i32().map_err(dec_err)?;
            let start = reader.var_i32().map_err(dec_err)?;
            let length = reader.var_i32().map_err(dec_err)?;
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count)
                .map_err(|_| AdapterError::Decode(format!("invalid tab_complete count {count}")))?;
            let mut suggestions = Vec::with_capacity(count.min(reader.remaining()));
            for _ in 0..count {
                let text = reader.string(32_767).map_err(dec_err)?;
                // Known, tracked lossy flatten (issue #656; pinned by
                // `tab_complete_reply_tooltip_hex_colour_is_lost_to_the_legacy_string_flatten`
                // in this crate's own test suite): the tooltip is a real JSON text
                // component and protocol 754 (1.16.5) postdates 1.16's hex-colour
                // introduction, so it can carry a `TextColor::Rgb` this flatten has
                // no legacy code for and silently drops. Not fixable here alone —
                // `CommandSuggestionEntry::tooltip`'s `String` type would need
                // widening in `lodestone-model`, plus every protocol crate's
                // construction site and shell consumer updated to match.
                let tooltip = if reader.bool().map_err(dec_err)? {
                    Some(Text::from_json(&reader.string(32_767).map_err(dec_err)?).to_legacy_string())
                } else {
                    None
                };
                suggestions.push(lodestone_model::CommandSuggestionEntry { text, tooltip });
            }
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::CommandSuggestionsReceived {
                id,
                start,
                length,
                suggestions,
            })]);
        }
        if packet_id == play::clientbound::PLAYER_INFO {
            // A single `action` applies to every entry in the packet —
            // verified against minecraft-data's 1.16.2 `packet_player_info`
            // `switch`, byte-identical to 1.12.2's/1.8's shape, unlike
            // 26.2's per-entry action bitmask. See `packets::player_info`'s
            // module doc.
            let body: PlayerInfo = decode_body_exact(payload)?;
            let mut updated = Vec::new();
            let mut removed = Vec::new();
            for entry in body.entries {
                let blank = || PlayerListEntry {
                    uuid: entry.uuid,
                    name: None,
                    game_mode: None,
                    latency: None,
                    display_name: None,
                    // 1.16.5 has no separate "listed" bit — every entry the
                    // server sends is, by construction, in the tab list.
                    listed: None,
                    properties: None,
                    // 1.16.5 predates secure chat sessions entirely.
                    chat_session: None,
                    // 1.16.5 predates both `UPDATE_LIST_ORDER` and `UPDATE_HAT`
                    // (added in 1.21.4's action-bitmask packet) entirely.
                    list_order: None,
                    hat_visible: None,
                };
                match entry.action {
                    PlayerInfoAction::AddPlayer {
                        name,
                        properties,
                        game_mode: raw_mode,
                        ping,
                        display_name,
                    } => {
                        updated.push(PlayerListEntry {
                            name: Some(name),
                            game_mode: Some(game_mode(
                                u8::try_from(raw_mode).map_err(|_| {
                                    AdapterError::Decode(format!(
                                        "player_info game mode {raw_mode} out of range"
                                    ))
                                })?,
                            )?),
                            latency: Some(ping),
                            display_name: display_name.map(|json| Text::from_json(&json)),
                            properties: Some(
                                properties
                                    .into_iter()
                                    .map(|property| ProfileProperty {
                                        name: property.name,
                                        value: property.value,
                                        signature: property.signature,
                                    })
                                    .collect(),
                            ),
                            ..blank()
                        });
                    }
                    PlayerInfoAction::UpdateGameMode { game_mode: raw_mode } => {
                        updated.push(PlayerListEntry {
                            game_mode: Some(game_mode(
                                u8::try_from(raw_mode).map_err(|_| {
                                    AdapterError::Decode(format!(
                                        "player_info game mode {raw_mode} out of range"
                                    ))
                                })?,
                            )?),
                            ..blank()
                        });
                    }
                    PlayerInfoAction::UpdateLatency { ping } => {
                        updated.push(PlayerListEntry {
                            latency: Some(ping),
                            ..blank()
                        });
                    }
                    PlayerInfoAction::UpdateDisplayName { display_name } => {
                        updated.push(PlayerListEntry {
                            display_name: display_name.map(|json| Text::from_json(&json)),
                            ..blank()
                        });
                    }
                    PlayerInfoAction::RemovePlayer => {
                        removed.push(entry.uuid);
                    }
                }
            }
            let mut directives = Vec::with_capacity(2);
            if !updated.is_empty() {
                directives.push(Directive::Emit(ClientEvent::PlayerListUpdate {
                    entries: updated,
                }));
            }
            if !removed.is_empty() {
                directives.push(Directive::Emit(ClientEvent::PlayerListRemove {
                    profile_ids: removed,
                }));
            }
            return Ok(directives);
        }
        if packet_id == play::clientbound::BOSS_BAR {
            // Action-multiplexed (minecraft-data's 1.16.2 `packet_boss_bar`,
            // byte-identical to 1.12.2's shape), so this is a hand-decoded
            // `Reader` walk. Title is a JSON chat component (the boss bar
            // packet has carried `IChatComponent`/JSON since its 1.9
            // introduction). `flags` packs three bits: `0x01` darken sky,
            // `0x02` boss music, `0x04` create fog.
            let mut reader = Reader::new(payload);
            let id = reader.uuid().map_err(dec_err)?;
            let action_ordinal = reader.var_i32().map_err(dec_err)?;
            let action = match action_ordinal {
                0 => {
                    let title = reader.string(32767).map_err(dec_err)?;
                    let progress = reader.f32().map_err(dec_err)?;
                    let color = boss_color_from_ordinal(reader.var_i32().map_err(dec_err)?)?;
                    let overlay = boss_overlay_from_ordinal(reader.var_i32().map_err(dec_err)?)?;
                    let flags = reader.u8().map_err(dec_err)?;
                    BossAction::Add {
                        title: Box::new(Text::from_json(&title)),
                        progress,
                        color,
                        overlay,
                        darken: flags & 0x01 != 0,
                        music: flags & 0x02 != 0,
                        fog: flags & 0x04 != 0,
                    }
                }
                1 => BossAction::Remove,
                2 => BossAction::UpdateProgress(reader.f32().map_err(dec_err)?),
                3 => {
                    let title = reader.string(32767).map_err(dec_err)?;
                    BossAction::UpdateName(Box::new(Text::from_json(&title)))
                }
                4 => {
                    let color = boss_color_from_ordinal(reader.var_i32().map_err(dec_err)?)?;
                    let overlay = boss_overlay_from_ordinal(reader.var_i32().map_err(dec_err)?)?;
                    BossAction::UpdateStyle { color, overlay }
                }
                5 => {
                    let flags = reader.u8().map_err(dec_err)?;
                    BossAction::UpdateFlags {
                        darken: flags & 0x01 != 0,
                        music: flags & 0x02 != 0,
                        fog: flags & 0x04 != 0,
                    }
                }
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown boss_bar action {other}"
                    )));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::BossBarUpdate { id, action })]);
        }
        if packet_id == play::clientbound::COMBAT_EVENT {
            // Action-multiplexed, verified field-by-field against
            // minecraft-data's 1.16.2 `packet_combat_event` (byte-identical
            // to 1.12.2's shape): event `0` (enter combat) carries nothing
            // further; event `1` (end combat) reads a VarInt duration then a
            // raw `i32` entity id (unused downstream, matching 26.2's own
            // `ClientboundPlayerCombatEndPacket`); event `2` (entity died)
            // reads a VarInt player id, a raw `i32` entity id, then a JSON
            // death-message string, both ids discarded except the message.
            let mut reader = Reader::new(payload);
            let event = reader.var_i32().map_err(dec_err)?;
            let directive = match event {
                0 => Directive::Emit(ClientEvent::PlayerCombatEntered),
                1 => {
                    let duration_ticks = reader.var_i32().map_err(dec_err)?;
                    reader.i32().map_err(dec_err)?; // entity id, unused downstream
                    Directive::Emit(ClientEvent::PlayerCombatEnded { duration_ticks })
                }
                2 => {
                    reader.var_i32().map_err(dec_err)?; // player id, unused downstream
                    reader.i32().map_err(dec_err)?; // killer entity id, unused downstream
                    let message = reader.string(32767).map_err(dec_err)?;
                    Directive::Emit(ClientEvent::Death {
                        message: Text::from_json(&message),
                    })
                }
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown combat_event action {other}"
                    )));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![directive]);
        }
        if packet_id == play::clientbound::WORLD_BORDER {
            // Action-multiplexed, verified field-by-field against
            // minecraft-data's 1.16.2 `packet_world_border` (byte-identical
            // to 1.12.2's shape). Action `3` ("initialize") is the only one
            // that carries every field, in this exact order: x, z,
            // old_radius, new_radius, speed (VarLong lerp-time ms),
            // portal_boundary (VarInt absolute max size), warning_time,
            // warning_blocks.
            let mut reader = Reader::new(payload);
            let action = reader.var_i32().map_err(dec_err)?;
            let directive = match action {
                0 => {
                    let radius = reader.f64().map_err(dec_err)?;
                    Directive::Emit(ClientEvent::WorldBorderSizeChanged { size: radius })
                }
                1 => {
                    let old_radius = reader.f64().map_err(dec_err)?;
                    let new_radius = reader.f64().map_err(dec_err)?;
                    let speed = reader.var_i64().map_err(dec_err)?;
                    Directive::Emit(ClientEvent::WorldBorderSizeLerping {
                        old_size: old_radius,
                        new_size: new_radius,
                        lerp_time_ms: speed,
                    })
                }
                2 => {
                    let x = reader.f64().map_err(dec_err)?;
                    let z = reader.f64().map_err(dec_err)?;
                    Directive::Emit(ClientEvent::WorldBorderCenterChanged { x, z })
                }
                3 => {
                    let x = reader.f64().map_err(dec_err)?;
                    let z = reader.f64().map_err(dec_err)?;
                    let old_radius = reader.f64().map_err(dec_err)?;
                    let new_radius = reader.f64().map_err(dec_err)?;
                    let speed = reader.var_i64().map_err(dec_err)?;
                    let portal_boundary = reader.var_i32().map_err(dec_err)?;
                    let warning_time = reader.var_i32().map_err(dec_err)?;
                    let warning_blocks = reader.var_i32().map_err(dec_err)?;
                    Directive::Emit(ClientEvent::WorldBorderInitialized {
                        x,
                        z,
                        old_size: old_radius,
                        new_size: new_radius,
                        lerp_time_ms: speed,
                        absolute_max_size: portal_boundary,
                        warning_blocks,
                        warning_time,
                    })
                }
                4 => {
                    let warning_time = reader.var_i32().map_err(dec_err)?;
                    Directive::Emit(ClientEvent::WorldBorderWarningDelayChanged { warning_time })
                }
                5 => {
                    let warning_blocks = reader.var_i32().map_err(dec_err)?;
                    Directive::Emit(ClientEvent::WorldBorderWarningDistanceChanged {
                        warning_blocks,
                    })
                }
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown world_border action {other}"
                    )));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![directive]);
        }
        if packet_id == play::clientbound::TEAMS {
            // Mode-multiplexed. **Field order differs from 1.12.2**: verified
            // field-by-field against minecraft-data's 1.16.2 `packet_teams`,
            // which reorders `prefix`/`suffix` to *after* `formatting`
            // (1.12.2 has them immediately after `name`, before
            // `friendlyFire`) and widens the colour field from a raw `i8`
            // ("color") to a VarInt ("formatting") — `lodestone-v340`'s own
            // decoder cannot be ported verbatim for this one packet. Order
            // for modes 0/2: name, friendlyFire, nameTagVisibility,
            // collisionRule, formatting, prefix, suffix. `displayName`/
            // `prefix`/`suffix` are JSON chat components at this protocol
            // revision (1.13+), unlike 1.12.2's plain legacy-formatted
            // strings.
            let mut reader = Reader::new(payload);
            let team = reader.string(16).map_err(dec_err)?;
            let mode = reader.i8().map_err(dec_err)?;
            let read_members = |reader: &mut Reader<'_>| -> Result<Vec<String>, AdapterError> {
                let count = reader.var_i32().map_err(dec_err)?;
                let count = usize::try_from(count)
                    .unwrap_or(0)
                    .min(reader.remaining());
                let mut members = Vec::with_capacity(count);
                for _ in 0..count {
                    members.push(reader.string(16).map_err(dec_err)?);
                }
                Ok(members)
            };
            let action = match mode {
                0 | 2 => {
                    let display_name = reader.string(32767).map_err(dec_err)?;
                    let friendly_flags = reader.i8().map_err(dec_err)?;
                    let visibility_str = reader.string(32).map_err(dec_err)?;
                    let collision_str = reader.string(32).map_err(dec_err)?;
                    let color_ordinal = reader.var_i32().map_err(dec_err)?;
                    let prefix = reader.string(32767).map_err(dec_err)?;
                    let suffix = reader.string(32767).map_err(dec_err)?;
                    let name_tag_visibility = match visibility_str.as_str() {
                        "always" => Visibility::Always,
                        "never" => Visibility::Never,
                        "hideForOtherTeams" => Visibility::HideForOtherTeams,
                        "hideForOwnTeam" => Visibility::HideForOwnTeam,
                        other => {
                            return Err(AdapterError::Decode(format!(
                                "unknown team name-tag visibility {other:?}"
                            )));
                        }
                    };
                    let collision_rule = match collision_str.as_str() {
                        "always" => CollisionRule::Always,
                        "never" => CollisionRule::Never,
                        "pushOtherTeams" => CollisionRule::PushOtherTeams,
                        "pushOwnTeam" => CollisionRule::PushOwnTeam,
                        other => {
                            return Err(AdapterError::Decode(format!(
                                "unknown team collision rule {other:?}"
                            )));
                        }
                    };
                    let params = Box::new(TeamParameters {
                        display_name: Text::from_json(&display_name),
                        prefix: Text::from_json(&prefix),
                        suffix: Text::from_json(&suffix),
                        name_tag_visibility,
                        collision_rule,
                        color: team_color_from_ordinal(color_ordinal),
                        friendly_fire: friendly_flags & 0x01 != 0,
                        see_friendly_invisibles: friendly_flags & 0x02 != 0,
                    });
                    if mode == 0 {
                        TeamAction::Create {
                            params,
                            members: read_members(&mut reader)?,
                        }
                    } else {
                        TeamAction::Update { params }
                    }
                }
                1 => TeamAction::Remove,
                3 => TeamAction::AddMembers(read_members(&mut reader)?),
                4 => TeamAction::RemoveMembers(read_members(&mut reader)?),
                other => {
                    return Err(AdapterError::Decode(format!("unknown teams mode {other}")));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TeamUpdate {
                name: team,
                action,
            })]);
        }
        if packet_id == play::clientbound::SCOREBOARD_DISPLAY_OBJECTIVE {
            // Verified against minecraft-data's 1.16.2
            // `packet_scoreboard_display_objective`: a raw `i8` slot
            // position, then a string objective name (byte-identical to
            // 1.12.2's shape). Clears the slot with an empty string rather
            // than a dedicated marker.
            let mut reader = Reader::new(payload);
            let position = reader.i8().map_err(dec_err)?;
            let name = reader.string(16).map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let slot = match position {
                0 => DisplaySlot::List,
                1 => DisplaySlot::Sidebar,
                2 => DisplaySlot::BelowName,
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown scoreboard display slot {other}"
                    )));
                }
            };
            let objective = if name.is_empty() { None } else { Some(name) };
            return Ok(vec![Directive::Emit(ClientEvent::DisplayObjective {
                slot,
                objective,
            })]);
        }
        if packet_id == play::clientbound::SCOREBOARD_OBJECTIVE {
            // Mode-multiplexed. **`type` is a VarInt render-type ordinal
            // here, unlike 1.12.2's plain string** — verified against
            // minecraft-data's 1.16.2 `packet_scoreboard_objective` (`0` =
            // integer, `1` = hearts; no other render type exists at this
            // protocol revision). `displayText` is a JSON chat component
            // (1.13+), unlike 1.12.2's plain legacy-formatted string.
            let mut reader = Reader::new(payload);
            let name = reader.string(16).map_err(dec_err)?;
            let action = reader.i8().map_err(dec_err)?;
            let event = match action {
                0 | 2 => {
                    let display_text = reader.string(32767).map_err(dec_err)?;
                    let render_type_ordinal = reader.var_i32().map_err(dec_err)?;
                    let render_type = match render_type_ordinal {
                        0 => ObjectiveRenderType::Integer,
                        1 => ObjectiveRenderType::Hearts,
                        other => {
                            return Err(AdapterError::Decode(format!(
                                "unknown objective render type {other}"
                            )));
                        }
                    };
                    ClientEvent::ObjectiveUpdate {
                        name,
                        mode: if action == 0 {
                            ObjectiveMode::Add
                        } else {
                            ObjectiveMode::Change
                        },
                        display_name: Some(Text::from_json(&display_text)),
                        render_type: Some(render_type),
                        number_format: None,
                    }
                }
                1 => ClientEvent::ObjectiveUpdate {
                    name,
                    mode: ObjectiveMode::Remove,
                    display_name: None,
                    render_type: None,
                    number_format: None,
                },
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown scoreboard_objective action {other}"
                    )));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(event)]);
        }
        if packet_id == play::clientbound::SCOREBOARD_SCORE {
            // Verified against minecraft-data's 1.16.2
            // `packet_scoreboard_score` (byte-identical to 1.12.2's shape):
            // `itemName` is the score *holder* and `scoreName` is the
            // *objective* — the mcdata field names are misleading, not the
            // wire order. `scoreName` is read unconditionally, so a
            // `remove` action still names exactly one objective, never
            // "reset all".
            let mut reader = Reader::new(payload);
            let holder = reader.string(64).map_err(dec_err)?;
            let action = reader.var_i32().map_err(dec_err)?;
            let objective = reader.string(16).map_err(dec_err)?;
            let event = match action {
                0 => {
                    let value = reader.var_i32().map_err(dec_err)?;
                    ClientEvent::ScoreUpdate {
                        holder,
                        objective,
                        value,
                        display: None,
                        number_format: None,
                    }
                }
                1 => ClientEvent::ScoreReset {
                    holder,
                    objective: Some(objective),
                },
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown scoreboard_score action {other}"
                    )));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(event)]);
        }
        // Everything else in play is intentionally ignored for now.
        Ok(Vec::new())
    }
}

impl VersionAdapter for V735Adapter {
    fn protocol_version(&self) -> i32 {
        PROTOCOL
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["1.16.5"]
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
        // 1.8 login_start carries only the username: there is no client-provided
        // profile UUID, unlike the modern login hello packet.
        let login_start = crate::packets::login::LoginStart {
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
            ConnectionState::Handshaking
            | ConnectionState::Status
            | ConnectionState::Configuration => Err(AdapterError::UnsupportedPacketState { state }),
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
                let body = KeepAliveResponse { id: *id };
                Ok(Some((play::serverbound::KEEP_ALIVE, encode_body(&body)?)))
            }
            ClientAction::SendChat { text } => {
                let body = ServerboundChat {
                    message: text.clone(),
                };
                Ok(Some((play::serverbound::CHAT, encode_body(&body)?)))
            }
            // 1.8 has no dedicated command packet: a command is a chat message
            // beginning with a slash.
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
                // This protocol's `PositionLook` packet has no
                // horizontal-collision bit — only `onGround` — so there is
                // nothing to forward it into.
                horizontal_collision: _,
            } => self.select_move_packet(*pos, *rotation, *on_ground),
            ClientAction::SwingArm { hand } => {
                let body = ServerboundArmAnimation {
                    hand: match hand {
                        Hand::Main => 0,
                        Hand::Off => 1,
                    },
                };
                Ok(Some((
                    play::serverbound::ARM_ANIMATION,
                    encode_body(&body)?,
                )))
            }

            // Block breaking rides on `block_dig` statuses 0/1/2. The model's
            // `sequence` (block-prediction, added 1.19) has no 1.16 equivalent
            // and is dropped deliberately.
            ClientAction::BlockAction {
                action,
                pos,
                face,
                sequence: _,
            } => {
                let status = match action {
                    BlockActionKind::StartDestroy => 0,
                    BlockActionKind::AbortDestroy => 1,
                    BlockActionKind::StopDestroy => 2,
                };
                let body = BlockDig {
                    status,
                    location: Position(*pos),
                    face: face_ordinal(*face) as i8,
                };
                Ok(Some((play::serverbound::BLOCK_DIG, encode_body(&body)?)))
            }
            // Item dropping also rides on `block_dig` (statuses 3/4).
            ClientAction::DropSelectedItemStack => {
                let body = BlockDig {
                    status: 3,
                    location: Position::new(0, 0, 0),
                    face: 0,
                };
                Ok(Some((play::serverbound::BLOCK_DIG, encode_body(&body)?)))
            }
            ClientAction::DropSelectedItem => {
                let body = BlockDig {
                    status: 4,
                    location: Position::new(0, 0, 0),
                    face: 0,
                };
                Ok(Some((play::serverbound::BLOCK_DIG, encode_body(&body)?)))
            }
            ClientAction::ReleaseUseItem => {
                let body = BlockDig {
                    status: 5,
                    location: Position::new(0, 0, 0),
                    face: 0,
                };
                Ok(Some((play::serverbound::BLOCK_DIG, encode_body(&body)?)))
            }
            // 1.9+ off-hand swap is `block_dig` status 6 (unlike protocol 47,
            // which has no off-hand and rejects this action).
            ClientAction::SwapItemWithOffhand => {
                let body = BlockDig {
                    status: 6,
                    location: Position::new(0, 0, 0),
                    face: 0,
                };
                Ok(Some((play::serverbound::BLOCK_DIG, encode_body(&body)?)))
            }

            // Placing a block / using an item on a block. 1.16 sends the hand
            // first, then the packed position, a varint face, a float cursor and
            // an `inside_block` flag; no inline item (the server resolves it) and
            // no block-prediction `sequence` (added 1.19), both dropped
            // deliberately.
            ClientAction::UseItemOn {
                hand,
                pos,
                face,
                cursor,
                inside_block,
                sequence: _,
            } => {
                let body = BlockPlace {
                    hand: hand_ordinal(*hand),
                    location: Position(*pos),
                    direction: face_ordinal(*face),
                    cursor_x: cursor.x,
                    cursor_y: cursor.y,
                    cursor_z: cursor.z,
                    inside_block: *inside_block,
                };
                Ok(Some((play::serverbound::BLOCK_PLACE, encode_body(&body)?)))
            }
            // Using an item in the air is the dedicated `use_item` packet in
            // 1.14+ (the legacy (-1,-1,-1) `block_place` sentinel no longer
            // works). The model's `rotation` and `sequence` have no 1.16
            // equivalent and are dropped.
            ClientAction::UseItem {
                hand,
                rotation: _,
                sequence: _,
            } => {
                let body = UseItem {
                    hand: hand_ordinal(*hand),
                };
                Ok(Some((play::serverbound::USE_ITEM, encode_body(&body)?)))
            }

            // Entity interaction. 1.9+ carries the hand for interact/interact-at
            // (attack has no hand), and 1.16 appends a `sneaking` flag to every
            // form. Each mouse value is a distinct wire shape.
            ClientAction::InteractEntity {
                entity_id,
                interaction,
                sneaking,
            } => match interaction {
                EntityInteraction::Attack => {
                    let body = UseEntity {
                        target: *entity_id,
                        mouse: 1,
                        sneaking: *sneaking,
                    };
                    Ok(Some((play::serverbound::USE_ENTITY, encode_body(&body)?)))
                }
                EntityInteraction::Interact { hand } => {
                    let body = UseEntityInteract {
                        target: *entity_id,
                        mouse: 0,
                        hand: hand_ordinal(*hand),
                        sneaking: *sneaking,
                    };
                    Ok(Some((play::serverbound::USE_ENTITY, encode_body(&body)?)))
                }
                EntityInteraction::InteractAt { hand, target } => {
                    let body = UseEntityAt {
                        target: *entity_id,
                        mouse: 2,
                        x: target.x as f32,
                        y: target.y as f32,
                        z: target.z as f32,
                        hand: hand_ordinal(*hand),
                        sneaking: *sneaking,
                    };
                    Ok(Some((play::serverbound::USE_ENTITY, encode_body(&body)?)))
                }
            },

            // Player commands ride on `entity_action`. 1.9+ (so 1.16) has the full
            // action set including stop-riding-jump (6), open-inventory (7), and
            // elytra fall-flying (8) — a divergence from 1.8, which lacks the last
            // two and numbers open-inventory as 6.
            ClientAction::PlayerCommand { entity_id, command } => {
                let action_id = match command {
                    PlayerCommand::StopSleeping => 2,
                    PlayerCommand::StartSprinting => 3,
                    PlayerCommand::StopSprinting => 4,
                    PlayerCommand::StartRidingJump { .. } => 5,
                    PlayerCommand::StopRidingJump => 6,
                    PlayerCommand::OpenInventory => 7,
                    PlayerCommand::StartFallFlying => 8,
                };
                let jump_boost = match command {
                    PlayerCommand::StartRidingJump { boost } => *boost,
                    _ => 0,
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

            // Inventory. Close/select ride on plain packets. Clearing a creative
            // slot sends an empty slot; a non-empty creative slot needs an item
            // registry (ResourceKey -> numeric id) that no crate has yet.
            ClientAction::ContainerClose { window_id } => {
                let body = ServerboundCloseWindow {
                    window_id: *window_id as u8,
                };
                Ok(Some((play::serverbound::CLOSE_WINDOW, encode_body(&body)?)))
            }
            ClientAction::SetCarriedItem { slot } => {
                let body = ServerboundHeldItemSlot { slot: *slot as i16 };
                Ok(Some((
                    play::serverbound::HELD_ITEM_SLOT,
                    encode_body(&body)?,
                )))
            }
            ClientAction::SetCreativeModeSlot { slot, item } => {
                if item.is_some() {
                    return Err(AdapterError::Unsupported(
                        "protocol 754 SetCreativeModeSlot with an item requires a ResourceKey -> \
                         numeric item-id registry that is not yet available"
                            .to_owned(),
                    ));
                }
                let body = SetCreativeSlot {
                    slot: *slot as i16,
                    item: Slot::Empty,
                };
                Ok(Some((
                    play::serverbound::SET_CREATIVE_SLOT,
                    encode_body(&body)?,
                )))
            }
            // Container clicks predate the modern `state_id` reconciliation.
            // Faithfully encoding 1.16's `window_click` needs a client-tracked
            // transaction id (the `action` counter, absent from the model which
            // carries only the 1.17+ `state_id`; this adapter is stateless) and
            // an item registry (`ResourceKey` -> numeric id) for the clicked
            // stack. 1.16 slots are flattened, so unlike v47/v340 there is no
            // item-metadata gap — but the transaction id and registry alone are
            // enough to make an encoded click be rejected by a live server (via a
            // failed transaction) rather than silently applied. Refused loudly.
            //
            // This is also why clientbound `TRANSACTION` has no decode arm: it
            // exists solely to accept or reject a `window_click` this client
            // cannot yet send, so nothing here could ever receive one — wiring a
            // decode for it now would be an event with no producer that could
            // trigger it. It becomes real work once `ContainerClick` above is.
            ClientAction::ContainerClick { .. } => Err(AdapterError::Unsupported(
                "protocol 754 ContainerClick needs a client-tracked transaction id (model carries \
                 only the 1.17+ state_id) and an item registry; refused rather than sending bytes \
                 a live server rejects via a failed transaction"
                    .to_owned(),
            )),

            // Genuinely absent in 1.16: there is no player-input packet (added
            // much later). `Stab` (off-hand attack) has no dedicated 1.16 packet
            // either.
            ClientAction::Stab => Err(AdapterError::Unsupported(
                "protocol 754 has no dedicated off-hand attack (Stab) packet".to_owned(),
            )),
            ClientAction::SetPlayerInput(_) => Err(AdapterError::Unsupported(
                "protocol 754 has no player-input packet".to_owned(),
            )),

            // Newly modelled actions that 1.16 genuinely carries. Encoded
            // faithfully against the minecraft-data wire shapes.
            ClientAction::SetClientSettings(settings) => {
                let ClientSettings {
                    locale,
                    view_distance,
                    chat_mode,
                    chat_colors,
                    skin_parts,
                    main_hand,
                    // 1.16 predates these fields; dropped deliberately.
                    text_filtering: _,
                    allow_server_listing: _,
                    particle_status: _,
                } = settings;
                let body = Settings {
                    locale: locale.clone(),
                    view_distance: *view_distance,
                    chat_flags: chat_mode_value(*chat_mode),
                    chat_colors: *chat_colors,
                    skin_parts: skin_parts_bits(*skin_parts),
                    main_hand: main_hand_value(*main_hand),
                };
                Ok(Some((play::serverbound::SETTINGS, encode_body(&body)?)))
            }
            ClientAction::SendBrand { brand } => {
                let body = BrandPayload {
                    channel: "minecraft:brand".to_owned(),
                    brand: brand.clone(),
                };
                Ok(Some((
                    play::serverbound::CUSTOM_PAYLOAD,
                    encode_body(&body)?,
                )))
            }
            ClientAction::ContainerButtonClick {
                window_id,
                button_id,
            } => {
                let window_id = i8::try_from(*window_id).map_err(|_| {
                    AdapterError::Encode(format!("window id {window_id} overflows i8"))
                })?;
                let button = i8::try_from(*button_id).map_err(|_| {
                    AdapterError::Encode(format!("button id {button_id} overflows i8"))
                })?;
                let body = EnchantItem { window_id, button };
                Ok(Some((play::serverbound::ENCHANT_ITEM, encode_body(&body)?)))
            }
            ClientAction::SetFlying { flying } => {
                // 1.16 reduced serverbound abilities to a single flags byte.
                let body = PlayerAbilities {
                    flags: if *flying { ABILITY_FLYING } else { 0 },
                };
                Ok(Some((play::serverbound::ABILITIES, encode_body(&body)?)))
            }
            ClientAction::ResourcePackResponse { response, .. } => {
                // 1.16 `resource_pack_receive` sends only the result varint (no
                // pack hash), so the four legacy outcomes map cleanly. The
                // 1.20.3+ outcomes have no 1.16 result code and are refused.
                let result = match response {
                    ResourcePackResponseKind::SuccessfullyLoaded => 0,
                    ResourcePackResponseKind::Declined => 1,
                    ResourcePackResponseKind::FailedDownload => 2,
                    ResourcePackResponseKind::Accepted => 3,
                    other => {
                        return Err(AdapterError::Unsupported(format!(
                            "protocol 754 resource_pack_receive has no result code for {other:?}"
                        )));
                    }
                };
                let body = ResourcePackReceive { result };
                Ok(Some((
                    play::serverbound::RESOURCE_PACK_RECEIVE,
                    encode_body(&body)?,
                )))
            }
            ClientAction::PongResponse { .. } => Err(AdapterError::Unsupported(
                "protocol 754 predates the play ping/pong packets (added in 1.17)".to_owned(),
            )),
            ClientAction::EndClientTick => Err(AdapterError::Unsupported(
                "protocol 754 has no client_tick_end packet".to_owned(),
            )),
            ClientAction::RenameItem { .. } => Err(AdapterError::Unsupported(
                "protocol 735 rename item encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SelectTrade { .. } => Err(AdapterError::Unsupported(
                "protocol 735 select trade encoding is not yet implemented".to_owned(),
            )),
            ClientAction::PickItemFromBlock { .. } => Err(AdapterError::Unsupported(
                "protocol 735 pick item from block encoding is not yet implemented".to_owned(),
            )),
            ClientAction::PickItemFromEntity { .. } => Err(AdapterError::Unsupported(
                "protocol 735 pick item from entity encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SetBeaconEffects { .. } => Err(AdapterError::Unsupported(
                "protocol 735 set beacon encoding requires a mob-effect registry that is not yet \
                 available"
                    .to_owned(),
            )),
            ClientAction::EditBook { .. } => Err(AdapterError::Unsupported(
                "protocol 735 edit book encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SignUpdate { .. } => Err(AdapterError::Unsupported(
                "protocol 735 sign update encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SetCommandBlock { .. } => Err(AdapterError::Unsupported(
                "protocol 735 set command block encoding is not yet implemented".to_owned(),
            )),
            ClientAction::PlayerLoaded => Err(AdapterError::Unsupported(
                "protocol 735 predates the player_loaded packet (added in 1.20.2)".to_owned(),
            )),
            ClientAction::SeenAdvancements { .. } => Err(AdapterError::Unsupported(
                "protocol 735 advancements encoding is not yet implemented".to_owned(),
            )),
            ClientAction::CommandSuggestion { id, command } => {
                // `packet_tab_complete` (minecraft-data 1.16.2): `transactionId:
                // varint, text: string` — full parity with 26.2's serverbound
                // shape, so `id` round-trips on the wire itself and needs no
                // client-side bookkeeping the way v47/v340 need.
                let mut writer = Writer::default();
                writer.var_i32(*id);
                writer.string(command);
                Ok(Some((play::serverbound::TAB_COMPLETE, writer.into_vec())))
            }
            ClientAction::PaddleBoat { .. } => Err(AdapterError::Unsupported(
                "protocol 735 paddle boat encoding is not yet implemented".to_owned(),
            )),
            ClientAction::MoveVehicle { .. } => Err(AdapterError::Unsupported(
                "protocol 735 move vehicle encoding is not yet implemented".to_owned(),
            )),

            // Leaving the death screen. `client_command` action `0` =
            // perform respawn, a stable ordinal across every generation
            // checked (1.8, 1.12.2, 1.16.2/.4/.5 all encode it as a lone
            // varint action id per minecraft-data's protocol.json).
            ClientAction::Respawn => {
                let body = ClientCommand { action: 0 };
                Ok(Some((play::serverbound::CLIENT_COMMAND, encode_body(&body)?)))
            }
            // Clicking a name in the tab list while spectating. 1.16.5's
            // `spectate` packet carries the target's uuid directly, which the
            // model already supplies, so no entity registry is needed.
            ClientAction::TeleportToEntity { target } => {
                let body = Spectate { target: *target };
                Ok(Some((play::serverbound::SPECTATE, encode_body(&body)?)))
            }
            // The continuous spectator-follow action carries only a network
            // entity id, but 1.16.5's wire packet is the same uuid-keyed
            // `spectate` packet as `TeleportToEntity` above. A stateless
            // adapter has no id->uuid registry to bridge the two.
            ClientAction::SpectatorAction { .. } => Err(AdapterError::Unsupported(
                "protocol 754's spectate packet needs a target uuid; SpectatorAction carries \
                 only a network entity id with no registry to resolve it into one (use \
                 TeleportToEntity instead, which already carries the uuid)"
                    .to_owned(),
            )),
            ClientAction::ChatAck { .. } => Err(AdapterError::Unsupported(
                "protocol 754 predates signed/acknowledged chat (added in 1.19)".to_owned(),
            )),
            ClientAction::SelectBundleItem { .. } => Err(AdapterError::Unsupported(
                "protocol 754 predates bundles (added in 1.21.2)".to_owned(),
            )),
            ClientAction::SetContainerSlotState { .. } => Err(AdapterError::Unsupported(
                "protocol 754 predates the crafter block (added in 1.21)".to_owned(),
            )),
            // All four recipe books exist by 1.16.5, so this needs no
            // version-specific fallback the way protocol 340's does.
            ClientAction::SetRecipeBookSettings {
                book_type,
                open,
                filtering,
            } => {
                let body = RecipeBook {
                    book_id: recipe_book_type_to_ordinal(*book_type),
                    book_open: *open,
                    filter_active: *filtering,
                };
                Ok(Some((play::serverbound::RECIPE_BOOK, encode_body(&body)?)))
            }
            // Both packets identify a recipe by a namespaced string id in
            // 1.16.5 (`craft_recipe_request.recipe` and
            // `displayed_recipe.recipeId`, both `string` per minecraft-data's
            // 1.16.2 protocol.json) rather than the numeric index the model
            // carries, and this stateless adapter has no recipe registry to
            // resolve one into the other.
            ClientAction::RecipeBookSeenRecipe { .. } | ClientAction::PlaceRecipe { .. } => {
                Err(AdapterError::Unsupported(
                    "protocol 754's recipe-book packets identify a recipe by a namespaced \
                     string id; the model's display index has no registry to resolve into one"
                        .to_owned(),
                ))
            }
            ClientAction::PingRequest { .. } => Err(AdapterError::Unsupported(
                "protocol 754 has no play-state ping request packet".to_owned(),
            )),
            ClientAction::ChangeGameMode { .. } => Err(AdapterError::Unsupported(
                "protocol 754 has no dedicated change_game_mode packet; a debug-menu game-mode \
                 switch in this era goes through the /gamemode chat command instead"
                    .to_owned(),
            )),

            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod movement_tests {
    use super::*;

    #[test]
    fn poisoned_movement_state_is_recovered() {
        let adapter = V735Adapter::new();
        let guard = adapter.movement.lock().expect("fresh movement state");
        let state = recover_movement_state(Err(PoisonError::new(guard)));
        drop(state);

        assert_eq!(
            adapter
                .select_move_packet(Vec3::new(1.0, 0.0, 0.0), Rotation::default(), false)
                .expect("poisoned state is recovered")
                .map(|(id, _)| id),
            Some(play::serverbound::POSITION)
        );
    }
}
