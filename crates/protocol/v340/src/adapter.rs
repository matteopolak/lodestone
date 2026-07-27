//! [`VersionAdapter`] implementation driving the protocol 340 join flow.

use std::sync::{Arc, Mutex};

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_model::{
    AdapterError, BlockActionKind, BlockFace, ChatKind, ChatMode, ChunkPos, ClientAction,
    ClientEvent, ClientSettings, ConnectionState, Directive, DisplayedSkinParts, EntityInteraction,
    EntityMovement, GameMode, Hand, LoginProfile, MainHand, PlayerCommand,
    ResourcePackResponseKind, Rotation, ServerAddress, TeleportFlags, Text, Vec3, VersionAdapter,
    WorldSink,
};
use lodestone_world::{ChunkPos as WorldChunkPos, Heightmaps, LoadedChunk};

use crate::entity_types;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::chunk::{ChunkShape, MapChunk, UnloadChunk};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse};
use crate::packets::entity::{
    EntityDestroy, EntityLook, EntityMoveLook, EntityTeleport, EntityVelocityPacket,
    NamedEntitySpawn, RelEntityMove, SpawnEntityLiving, SpawnObject,
};
use crate::packets::game::{
    BlockDig, BlockPlace, ClientboundChat, ClientboundPositionLook, EntityAction, JoinGame,
    KickDisconnect, ServerboundArmAnimation, ServerboundChat, ServerboundPositionLook,
    TeleportConfirm, UseEntity, UseEntityAt, UseEntityInteract,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{EncryptionRequest, LoginDisconnect, LoginSuccess, SetCompression};
use crate::packets::position::Position;
use crate::packets::settings::{BrandPayload, PlayerAbilities, ResourcePackReceive, Settings};
use crate::packets::slot::Slot;
use crate::packets::window::{
    EnchantItem, ServerboundCloseWindow, ServerboundHeldItemSlot, SetCreativeSlot,
};

/// Protocol version implemented by this adapter.
pub const PROTOCOL: i32 = 340;

/// Fixed decoding/encoding context for protocol 340.
const CTX: Ctx = Ctx { version: PROTOCOL };

/// Requested next-state value in the handshake for a login connection.
const NEXT_STATE_LOGIN: i32 = 2;

/// Relative-teleport flag bits used by the clientbound 1.8 position packet.
const REL_X: i8 = 0x01;
const REL_Y: i8 = 0x02;
const REL_Z: i8 = 0x04;
const REL_YAW: i8 = 0x08;
const REL_PITCH: i8 = 0x10;

/// Version adapter implementing protocol 340 (Minecraft 1.12.2).
///
/// Holds the current dimension's [`ChunkShape`] because a `map_chunk` cannot
/// tell from its own bytes whether sky light is present — that depends on the
/// dimension announced at join. The shape is guarded by a [`Mutex`] purely to
/// satisfy `Sync`; packets are processed serially so there is no contention.
#[derive(Debug, Clone)]
pub struct V340Adapter {
    shape: Arc<Mutex<ChunkShape>>,
}

impl Default for V340Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl V340Adapter {
    /// Creates a new adapter, defaulting to the overworld chunk shape until a
    /// join packet announces the real dimension.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shape: Arc::new(Mutex::new(ChunkShape::overworld())),
        }
    }

    /// Records whether the joined `dimension` carries sky light so subsequent
    /// `map_chunk` packets decode the right number of light arrays. 1.12.2
    /// dimension ids: `0` overworld (sky light), `-1` nether, `1` end.
    fn set_dimension(&self, dimension: i32) {
        if let Ok(mut shape) = self.shape.lock() {
            *shape = if dimension == 0 {
                ChunkShape::overworld()
            } else {
                ChunkShape::no_skylight()
            };
        }
    }

    /// Returns the current dimension's chunk shape.
    fn current_shape(&self) -> ChunkShape {
        self.shape
            .lock()
            .map_or_else(|_| ChunkShape::overworld(), |shape| *shape)
    }
}

/// Returns a protocol 340 version adapter.
///
/// This free function is the crate's canonical constructor entry point; the
/// client boxes the returned concrete type as a `dyn VersionAdapter`.
#[must_use]
pub fn adapter() -> V340Adapter {
    V340Adapter::new()
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

/// Like [`decode_body`] but additionally requires the payload to be fully
/// consumed. Used for packets whose whole body we decode (e.g. the entity
/// destroy id list), where trailing bytes signal a wrong layout and must be
/// rejected rather than silently ignored. Packets that deliberately leave a
/// tail unread (metadata terminators, fields we don't model yet) keep using the
/// lenient [`decode_body`].
fn decode_body_exact<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    let mut reader = Reader::new(payload);
    let body = T::decode(&mut reader, CTX).map_err(|err| AdapterError::Decode(err.to_string()))?;
    reader
        .ensure_empty()
        .map_err(|err| AdapterError::Decode(err.to_string()))?;
    Ok(body)
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

/// Maps a 1.8 numeric dimension to a canonical namespaced dimension identifier.
///
/// # Architectural note
///
/// 1.8's join packet carries the dimension as a signed byte, not the modern
/// namespaced identifier the model expects. This mapping is the adapter's job:
/// the model stays version-free and the numeric encoding stays in the version
/// crate.
fn dimension_id(value: i32) -> Result<lodestone_model::DimensionId, AdapterError> {
    let name = match value {
        -1 => "minecraft:the_nether",
        0 => "minecraft:overworld",
        1 => "minecraft:the_end",
        other => {
            return Err(AdapterError::Decode(format!("unknown dimension {other}")));
        }
    };
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
fn unpack_degrees(packed: i8) -> f32 {
    f32::from(packed) * 360.0 / 256.0
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

/// The vanilla flying-ability flag bit set when the client is flying.
const ABILITY_FLYING: i8 = 0x02;
/// Vanilla default flying speed, sent in the server-ignored serverbound field.
const DEFAULT_FLYING_SPEED: f32 = 0.05;
/// Vanilla default walking speed, sent in the server-ignored serverbound field.
const DEFAULT_WALKING_SPEED: f32 = 0.1;

impl V340Adapter {
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
            // Record whether this dimension carries sky light before any chunk
            // arrives, so single `map_chunk` packets decode the right geometry.
            self.set_dimension(body.dimension);
            return Ok(vec![Directive::Emit(ClientEvent::Login {
                entity_id: body.entity_id,
                game_mode: game_mode(body.game_mode)?,
                dimension: dimension_id(body.dimension)?,
            })]);
        }
        if packet_id == play::clientbound::MAP_CHUNK {
            // Decode the paletted 1.12.2 column into version-free storage and
            // apply it to the world through the sink, emitting only a
            // lightweight notification. 1.12.2 always sends a real column here
            // (unloads use the dedicated unload_chunk packet), so this loads.
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
        if packet_id == play::clientbound::UNLOAD_CHUNK {
            // 1.12.2 has a dedicated forget packet (two ints), unlike 1.8's
            // empty-bitmask trick.
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
            let type_id = i32::from(body.kind);
            let entity_type = entity_types::object_type_name(type_id)
                .ok_or_else(|| {
                    AdapterError::Decode(format!("unknown object type id {type_id} in spawn"))
                })?
                .parse()
                .map_err(|_| {
                    AdapterError::Decode(format!("object type id {type_id} is not a key"))
                })?;
            // 1.12 always includes velocity, but a stationary object still
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
        // Everything else in play is intentionally ignored for now.
        Ok(Vec::new())
    }
}

impl VersionAdapter for V340Adapter {
    fn protocol_version(&self) -> i32 {
        PROTOCOL
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["1.12.2"]
    }

    fn supports(&self, protocol: i32) -> bool {
        protocol == PROTOCOL
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
            } => {
                let body = ServerboundPositionLook {
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                    yaw: rotation.yaw,
                    pitch: rotation.pitch,
                    on_ground: *on_ground,
                };
                Ok(Some((
                    play::serverbound::POSITION_LOOK,
                    encode_body(&body)?,
                )))
            }
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
            // `sequence` (block-prediction, added 1.19) has no 1.12 equivalent
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

            // Placing a block / using an item on a block. 1.12 sends a hand index
            // and float cursor with no inline item (the server resolves the item
            // from its own inventory view), so no item registry is needed.
            ClientAction::UseItemOn {
                hand,
                pos,
                face,
                cursor,
                inside_block: _,
                sequence: _,
            } => {
                let body = BlockPlace {
                    location: Position(*pos),
                    direction: face_ordinal(*face),
                    hand: hand_ordinal(*hand),
                    cursor_x: cursor.x,
                    cursor_y: cursor.y,
                    cursor_z: cursor.z,
                };
                Ok(Some((play::serverbound::BLOCK_PLACE, encode_body(&body)?)))
            }
            // Using an item in the air: `block_place` with location (-1,-1,-1) and
            // direction -1.
            ClientAction::UseItem {
                hand,
                rotation: _,
                sequence: _,
            } => {
                let body = BlockPlace {
                    location: Position::new(-1, -1, -1),
                    direction: -1,
                    hand: hand_ordinal(*hand),
                    cursor_x: 0.0,
                    cursor_y: 0.0,
                    cursor_z: 0.0,
                };
                Ok(Some((play::serverbound::BLOCK_PLACE, encode_body(&body)?)))
            }

            // Entity interaction. 1.9+ carries the hand for interact/interact-at
            // (attack has no hand). Each mouse value is a distinct wire shape.
            ClientAction::InteractEntity {
                entity_id,
                interaction,
                sneaking: _,
            } => match interaction {
                EntityInteraction::Attack => {
                    let body = UseEntity {
                        target: *entity_id,
                        mouse: 1,
                    };
                    Ok(Some((play::serverbound::USE_ENTITY, encode_body(&body)?)))
                }
                EntityInteraction::Interact { hand } => {
                    let body = UseEntityInteract {
                        target: *entity_id,
                        mouse: 0,
                        hand: hand_ordinal(*hand),
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
                    };
                    Ok(Some((play::serverbound::USE_ENTITY, encode_body(&body)?)))
                }
            },

            // Player commands ride on `entity_action`. 1.9+ (so 1.12) has the full
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
                        "protocol 340 SetCreativeModeSlot with an item requires a ResourceKey -> \
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
            // Faithfully encoding 1.12's `window_click` needs a client-tracked
            // transaction id (the `action` counter, absent from the model which
            // carries only the 1.17+ `state_id`; this adapter is stateless), an
            // item registry (`ResourceKey` -> numeric id) for the clicked stack,
            // and item metadata/damage that pre-1.13 slots carry but the model's
            // `ItemStack { item, count }` cannot express. Refused loudly rather
            // than encoded with wrong bytes that a live server rejects via a
            // failed transaction (silently dropping the click).
            ClientAction::ContainerClick { .. } => Err(AdapterError::Unsupported(
                "protocol 340 ContainerClick needs a client-tracked transaction id (model carries \
                 only the 1.17+ state_id), an item registry, and item metadata the model's \
                 ItemStack cannot express"
                    .to_owned(),
            )),

            // Genuinely absent in 1.12: there is no player-input packet (added
            // much later). `Stab` (off-hand attack) has no dedicated 1.12 packet
            // either.
            ClientAction::Stab => Err(AdapterError::Unsupported(
                "protocol 340 has no dedicated off-hand attack (Stab) packet".to_owned(),
            )),
            ClientAction::SetPlayerInput(_) => Err(AdapterError::Unsupported(
                "protocol 340 has no player-input packet".to_owned(),
            )),

            // Newly modelled actions that 1.12 genuinely carries. Encoded
            // faithfully against the minecraft-data wire shapes.
            ClientAction::SetClientSettings(settings) => {
                let ClientSettings {
                    locale,
                    view_distance,
                    chat_mode,
                    chat_colors,
                    skin_parts,
                    main_hand,
                    // 1.12 predates these fields; dropped deliberately.
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
                    channel: "MC|Brand".to_owned(),
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
                let body = PlayerAbilities {
                    flags: if *flying { ABILITY_FLYING } else { 0 },
                    flying_speed: DEFAULT_FLYING_SPEED,
                    walking_speed: DEFAULT_WALKING_SPEED,
                };
                Ok(Some((play::serverbound::ABILITIES, encode_body(&body)?)))
            }
            ClientAction::ResourcePackResponse { response, .. } => {
                // 1.12 `resource_pack_receive` sends only the result varint (no
                // pack hash), so the Uuid-keyed model maps cleanly for the four
                // outcomes 1.12 defines. The 1.20.3+ outcomes have no 1.12 wire
                // value and are refused rather than mapped to a wrong code.
                let result = match response {
                    ResourcePackResponseKind::SuccessfullyLoaded => 0,
                    ResourcePackResponseKind::Declined => 1,
                    ResourcePackResponseKind::FailedDownload => 2,
                    ResourcePackResponseKind::Accepted => 3,
                    other => {
                        return Err(AdapterError::Unsupported(format!(
                            "protocol 340 resource_pack_receive has no result code for {other:?}"
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
                "protocol 340 has no configuration/play ping-pong handshake".to_owned(),
            )),
            ClientAction::EndClientTick => Err(AdapterError::Unsupported(
                "protocol 340 has no client_tick_end packet".to_owned(),
            )),
            ClientAction::RenameItem { .. } => Err(AdapterError::Unsupported(
                "protocol 340 rename item encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SelectTrade { .. } => Err(AdapterError::Unsupported(
                "protocol 340 select trade encoding is not yet implemented".to_owned(),
            )),
            ClientAction::PickItemFromBlock { .. } => Err(AdapterError::Unsupported(
                "protocol 340 has no pick_item_from_block packet".to_owned(),
            )),
            ClientAction::PickItemFromEntity { .. } => Err(AdapterError::Unsupported(
                "protocol 340 has no pick_item_from_entity packet".to_owned(),
            )),
            ClientAction::SetBeaconEffects { .. } => Err(AdapterError::Unsupported(
                "protocol 340 set beacon encoding requires a mob-effect registry that is not yet \
                 available"
                    .to_owned(),
            )),
            ClientAction::EditBook { .. } => Err(AdapterError::Unsupported(
                "protocol 340 edit book encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SignUpdate { .. } => Err(AdapterError::Unsupported(
                "protocol 340 sign update encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SetCommandBlock { .. } => Err(AdapterError::Unsupported(
                "protocol 340 set command block encoding is not yet implemented".to_owned(),
            )),
            ClientAction::PlayerLoaded => Err(AdapterError::Unsupported(
                "protocol 340 predates the player_loaded packet (added in 1.20.2)".to_owned(),
            )),
            ClientAction::SeenAdvancements { .. } => Err(AdapterError::Unsupported(
                "protocol 340 advancements encoding is not yet implemented".to_owned(),
            )),
            ClientAction::CommandSuggestion { .. } => Err(AdapterError::Unsupported(
                "protocol 340's tab-complete packet has a different wire shape and is not yet \
                 implemented"
                    .to_owned(),
            )),
            ClientAction::PaddleBoat { .. } => Err(AdapterError::Unsupported(
                "protocol 340 paddle boat encoding is not yet implemented".to_owned(),
            )),
            ClientAction::MoveVehicle { .. } => Err(AdapterError::Unsupported(
                "protocol 340 move vehicle encoding is not yet implemented".to_owned(),
            )),

            _ => Ok(None),
        }
    }
}
