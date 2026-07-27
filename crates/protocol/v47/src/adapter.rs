//! [`VersionAdapter`] implementation driving the protocol 47 join flow.

use std::sync::{Arc, Mutex};

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_model::{
    AdapterError, BlockActionKind, BlockFace, ChatKind, ChunkPos, ClientAction, ClientEvent,
    ConnectionState, Directive, EntityInteraction, EntityMovement, GameMode, Hand, LoginProfile,
    PlayerCommand, Rotation, ServerAddress, TeleportFlags, Text, Vec3, VersionAdapter,
};
use lodestone_world::{ChunkPos as WorldChunkPos, Heightmaps, LoadedChunk, WorldSink};

use crate::entity_types;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::chunk::{ChunkShape, MapChunk, MapChunkBulk};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse};
use crate::packets::entity::{
    EntityDestroy, EntityLook, EntityMoveLook, EntityTeleport, EntityVelocityPacket, RelEntityMove,
    SpawnEntityLiving, SpawnObject,
};
use crate::packets::game::{
    BlockDig, BlockPlace, ClientboundChat, ClientboundPositionLook, EntityAction, JoinGame,
    KickDisconnect, PlaySetCompression, ServerboundChat, ServerboundPositionLook, UseEntity,
    UseEntityAt,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{EncryptionRequest, LoginDisconnect, LoginSuccess, SetCompression};
use crate::packets::position::Position;
use crate::packets::slot::Slot;
use crate::packets::window::{ServerboundCloseWindow, ServerboundHeldItemSlot, SetCreativeSlot};

/// Protocol version implemented by this adapter.
pub const PROTOCOL: i32 = 47;

/// Fixed decoding/encoding context for protocol 47.
const CTX: Ctx = Ctx { version: PROTOCOL };

/// Requested next-state value in the handshake for a login connection.
const NEXT_STATE_LOGIN: i32 = 2;

/// Relative-teleport flag bits used by the clientbound 1.8 position packet.
const REL_X: i8 = 0x01;
const REL_Y: i8 = 0x02;
const REL_Z: i8 = 0x04;
const REL_YAW: i8 = 0x08;
const REL_PITCH: i8 = 0x10;

/// Version adapter implementing protocol 47 (Minecraft 1.8.8 / 1.8.9).
///
/// Holds the current dimension's [`ChunkShape`] because a `map_chunk` cannot
/// tell from its own bytes whether sky light is present — that depends on the
/// dimension announced at join. The shape is guarded by a [`Mutex`] purely to
/// satisfy `Sync`; there is no contention (packets are processed serially).
/// `map_chunk_bulk` carries its own `skyLightSent` flag and does not consult it.
#[derive(Debug, Clone)]
pub struct V47Adapter {
    shape: Arc<Mutex<ChunkShape>>,
}

impl Default for V47Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl V47Adapter {
    /// Creates a new adapter, defaulting to the overworld chunk shape until a
    /// join packet announces the real dimension.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shape: Arc::new(Mutex::new(ChunkShape::overworld())),
        }
    }

    /// Records whether the joined `dimension` carries sky light so subsequent
    /// `map_chunk` packets decode the right number of light arrays. 1.8
    /// dimension ids: `0` overworld (sky light), `-1` nether, `1` end.
    fn set_dimension(&self, dimension: i8) {
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

/// Returns a protocol 47 version adapter.
///
/// This free function is the crate's canonical constructor entry point; the
/// client boxes the returned concrete type as a `dyn VersionAdapter`.
#[must_use]
pub fn adapter() -> V47Adapter {
    V47Adapter::new()
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
fn dimension_id(value: i8) -> Result<lodestone_model::DimensionId, AdapterError> {
    let name = match value {
        -1 => "minecraft:the_nether",
        0 => "minecraft:overworld",
        1 => "minecraft:the_end",
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown 1.8 dimension {other}"
            )));
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

/// Fixed-point scale for 1.8 absolute entity coordinates: each unit is `1/32`
/// of a block (`ClientboundEntityTeleport`, `named_entity_spawn`, mob spawns).
const FIXED_POINT_SCALE: f64 = 32.0;

/// Delta-position scale for 1.8 `rel_entity_move` / `entity_move_look`: each
/// signed byte is `1/32` of a block (1.9+ widened these to `i16`/`1/4096`).
const MOVE_DELTA_SCALE: f64 = 32.0;

/// Velocity scale shared by 1.8 velocity packets: each `i16` is `1/8000` of a
/// block per tick (`ClientboundSetEntityMotion`).
const VELOCITY_SCALE: f64 = 8000.0;

/// Converts a signed-byte angle to degrees. 1.8 packs a full circle into 256
/// steps, so a byte of `64` is 90° (matches `Entity` rotation packing).
fn unpack_degrees(packed: i8) -> f32 {
    f32::from(packed) * 360.0 / 256.0
}

/// Maps a canonical [`BlockFace`] to its 1.8 numeric ordinal
/// (`Down=0, Up=1, North=2, South=3, West=4, East=5`), which matches the wire
/// order used by both `block_dig` and `block_place`.
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

/// Quantises a block-local cursor coordinate in `0.0..=1.0` to the 1.8
/// signed-byte cursor scale `0..=15`.
fn cursor_byte(v: f32) -> i8 {
    (v.clamp(0.0, 1.0) * 15.0).round() as i8
}

impl V47Adapter {
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
            // A `map_chunk` with an empty section bitmask is 1.8's chunk-unload
            // signal (there is no dedicated forget packet). Decoding yields an
            // empty column; treat that as an unload rather than storing air.
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
            if data.ground_up && data.column.allocated_sections() == 0 {
                world.unload(WorldChunkPos::new(data.x, data.z));
                return Ok(vec![Directive::Emit(ClientEvent::ChunkUnloaded { pos })]);
            }
            world.load(
                WorldChunkPos::new(data.x, data.z),
                LoadedChunk::new(data.column, data.light, Heightmaps::new(), Vec::new()),
            );
            return Ok(vec![Directive::Emit(ClientEvent::ChunkLoaded { pos })]);
        }
        if packet_id == play::clientbound::MAP_CHUNK_BULK {
            // One packet fans out to several full columns (a 1.8 construct with
            // no modern equivalent): load each and emit one notification each.
            let shape = self.current_shape();
            let mut reader = Reader::new(payload);
            let columns = MapChunkBulk::decode(&mut reader, &shape)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let mut directives = Vec::with_capacity(columns.len());
            for data in columns {
                let pos = ChunkPos::new(data.x, data.z);
                world.load(
                    WorldChunkPos::new(data.x, data.z),
                    LoadedChunk::new(data.column, data.light, Heightmaps::new(), Vec::new()),
                );
                directives.push(Directive::Emit(ClientEvent::ChunkLoaded { pos }));
            }
            return Ok(directives);
        }
        if packet_id == play::clientbound::KEEP_ALIVE {
            let keep_alive: KeepAliveRequest = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::KeepAlive {
                id: i64::from(keep_alive.id),
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
            // 1.8's teleport confirmation is genuinely different from modern:
            // there is no teleport id and no `teleport_confirm` packet (that
            // shape arrived in 1.9 / protocol 340). Instead the client echoes a
            // serverbound `position_look` back; until it does, the server holds
            // the player at the pending-teleport position and rubber-bands every
            // move — the same "unconfirmed teleport → physics looks broken"
            // failure the modern id-echo prevents. This per-version divergence
            // is exactly why the confirmation lives in the version crate.
            //
            // The join teleport vanilla sends is absolute (flags = 0), so
            // echoing the received coordinates confirms it exactly. Relative
            // components would need the player's current position, which a pure
            // adapter does not own — resolving those belongs to the physics
            // layer, which re-sends its resolved position via
            // `ClientAction::Move`.
            let confirm = ServerboundPositionLook {
                x: body.x,
                y: body.y,
                z: body.z,
                yaw: body.yaw,
                pitch: body.pitch,
                on_ground: false,
            };
            return Ok(vec![
                send(play::serverbound::POSITION_LOOK, &confirm)?,
                Directive::Emit(ClientEvent::TeleportPlayer {
                    pos: Vec3::new(body.x, body.y, body.z),
                    rotation: Rotation::new(body.yaw, body.pitch),
                    flags,
                }),
            ]);
        }
        if packet_id == play::clientbound::SPAWN_ENTITY_LIVING {
            // Reuse the existing derived mob-spawn decoder (varint id, u8 type,
            // fixed-point i32 coords, byte angles, i16 velocity, metadata). 1.8
            // mobs carry no UUID.
            let body: SpawnEntityLiving = decode_body(payload)?;
            let type_id = i32::from(body.kind);
            let entity_type = entity_types::mob_type_name(type_id)
                .ok_or_else(|| {
                    AdapterError::Decode(format!("unknown mob type id {type_id} in spawn"))
                })?
                .parse()
                .map_err(|_| AdapterError::Decode(format!("mob type id {type_id} is not a key")))?;
            return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: body.entity_id,
                uuid: None,
                entity_type,
                pos: Vec3::new(
                    f64::from(body.x) / FIXED_POINT_SCALE,
                    f64::from(body.y) / FIXED_POINT_SCALE,
                    f64::from(body.z) / FIXED_POINT_SCALE,
                ),
                rotation: Rotation::new(unpack_degrees(body.yaw), unpack_degrees(body.pitch)),
                velocity: Some(Vec3::new(
                    f64::from(body.velocity_x) / VELOCITY_SCALE,
                    f64::from(body.velocity_y) / VELOCITY_SCALE,
                    f64::from(body.velocity_z) / VELOCITY_SCALE,
                )),
            })]);
        }
        if packet_id == play::clientbound::SPAWN_ENTITY {
            // Object spawn. The trailing velocity is present only when
            // `object_data != 0`; that head-dependent tail is expressed by the
            // `#[mc(when = "object_data != 0")]` attribute on the derived
            // `SpawnObject` velocity fields, so this is now a plain decode.
            let body: SpawnObject = decode_body_exact(payload)?;
            let velocity = match (body.velocity_x, body.velocity_y, body.velocity_z) {
                (Some(vx), Some(vy), Some(vz)) => Some(Vec3::new(
                    f64::from(vx) / VELOCITY_SCALE,
                    f64::from(vy) / VELOCITY_SCALE,
                    f64::from(vz) / VELOCITY_SCALE,
                )),
                _ => None,
            };
            let type_id = i32::from(body.kind);
            let entity_type = entity_types::object_type_name(type_id)
                .ok_or_else(|| {
                    AdapterError::Decode(format!("unknown object type id {type_id} in spawn"))
                })?
                .parse()
                .map_err(|_| {
                    AdapterError::Decode(format!("object type id {type_id} is not a key"))
                })?;
            return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: body.entity_id,
                uuid: None,
                entity_type,
                pos: Vec3::new(
                    f64::from(body.x) / FIXED_POINT_SCALE,
                    f64::from(body.y) / FIXED_POINT_SCALE,
                    f64::from(body.z) / FIXED_POINT_SCALE,
                ),
                rotation: Rotation::new(unpack_degrees(body.yaw), unpack_degrees(body.pitch)),
                velocity,
            })]);
        }
        if packet_id == play::clientbound::NAMED_ENTITY_SPAWN {
            // Player spawn. 1.8 sends the player UUID as a 128-bit value here
            // (only Login Success uses the string form). Decoded inline: the
            // trailing data-watcher metadata is variable-length and not needed
            // for the spawn event, so the fixed prefix is read and the metadata
            // tail intentionally left unconsumed.
            let mut reader = Reader::new(payload);
            let dec = |e: lodestone_core::Error| AdapterError::Decode(e.to_string());
            let entity_id = reader.var_i32().map_err(dec)?;
            let uuid = reader.uuid().map_err(dec)?;
            let x = reader.i32().map_err(dec)?;
            let y = reader.i32().map_err(dec)?;
            let z = reader.i32().map_err(dec)?;
            let yaw = reader.i8().map_err(dec)?;
            let pitch = reader.i8().map_err(dec)?;
            let _current_item = reader.i16().map_err(dec)?;
            let entity_type = entity_types::PLAYER
                .parse()
                .map_err(|_| AdapterError::Decode("player key invalid".to_owned()))?;
            return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
                entity_id,
                uuid: Some(uuid),
                entity_type,
                pos: Vec3::new(
                    f64::from(x) / FIXED_POINT_SCALE,
                    f64::from(y) / FIXED_POINT_SCALE,
                    f64::from(z) / FIXED_POINT_SCALE,
                ),
                rotation: Rotation::new(unpack_degrees(yaw), unpack_degrees(pitch)),
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
            return Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
                entity_id: body.entity_id,
                movement: EntityMovement::Absolute(Vec3::new(
                    f64::from(body.x) / FIXED_POINT_SCALE,
                    f64::from(body.y) / FIXED_POINT_SCALE,
                    f64::from(body.z) / FIXED_POINT_SCALE,
                )),
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
        if packet_id == play::clientbound::SET_COMPRESSION {
            let body: PlaySetCompression = decode_body(payload)?;
            return Ok(vec![Directive::SetCompression(body.threshold)]);
        }
        // Everything else in play is intentionally ignored for now.
        Ok(Vec::new())
    }
}

impl VersionAdapter for V47Adapter {
    fn protocol_version(&self) -> i32 {
        PROTOCOL
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["1.8.8", "1.8.9"]
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
                let body = KeepAliveResponse { id: *id as i32 };
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
                // 1.8's `PositionLook` packet has no horizontal-collision
                // bit at all — only `onGround` — so there is nothing to
                // forward it into.
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
            // 1.8's serverbound `arm_animation` carries no fields: the offhand
            // did not exist until 1.9, so there is nothing to distinguish and
            // the empty packet is the whole message. The `hand` is dropped
            // deliberately (a divergence from 340/770, which encode it).
            ClientAction::SwingArm { hand: _ } => {
                Ok(Some((play::serverbound::ARM_ANIMATION, Vec::new())))
            }

            // Block breaking. 1.8 folds start/cancel/finish into `block_dig`
            // status codes 0/1/2. The model's `sequence` (block-prediction, added
            // in 1.19) has no 1.8 equivalent and is dropped deliberately.
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
                    face: face_ordinal(*face),
                };
                Ok(Some((play::serverbound::BLOCK_DIG, encode_body(&body)?)))
            }
            // Item dropping also rides on `block_dig` in 1.8 (statuses 3/4), with
            // an empty location and downward face by convention.
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
            // Releasing a use-item (finish eating, shoot bow) is `block_dig`
            // status 5 in 1.8.
            ClientAction::ReleaseUseItem => {
                let body = BlockDig {
                    status: 5,
                    location: Position::new(0, 0, 0),
                    face: 0,
                };
                Ok(Some((play::serverbound::BLOCK_DIG, encode_body(&body)?)))
            }

            // Placing a block / using an item on a block. 1.8 sends the held item
            // stack inline; because the adapter is stateless we send `Slot::Empty`
            // and let the vanilla server use its own authoritative held-item view
            // (verified live). The cursor floats are quantised to 0..=15 bytes.
            // The off-hand did not exist in 1.8, so a use targeting the off-hand
            // has nowhere to go and is rejected loudly rather than silently
            // encoded as a main-hand action.
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
                        "protocol 47 has no off-hand; UseItemOn{hand:Off} cannot be encoded"
                            .to_owned(),
                    ));
                }
                let body = BlockPlace {
                    location: Position(*pos),
                    direction: face_ordinal(*face),
                    held_item: Slot::Empty,
                    cursor_x: cursor_byte(cursor.x),
                    cursor_y: cursor_byte(cursor.y),
                    cursor_z: cursor_byte(cursor.z),
                };
                Ok(Some((play::serverbound::BLOCK_PLACE, encode_body(&body)?)))
            }
            // Using an item in the air. 1.8 signals this with a `block_place`
            // whose location is (-1,-1,-1) and direction -1.
            ClientAction::UseItem {
                hand,
                rotation: _,
                sequence: _,
            } => {
                if *hand == Hand::Off {
                    return Err(AdapterError::Unsupported(
                        "protocol 47 has no off-hand; UseItem{hand:Off} cannot be encoded"
                            .to_owned(),
                    ));
                }
                let body = BlockPlace {
                    location: Position::new(-1, -1, -1),
                    direction: -1,
                    held_item: Slot::Empty,
                    cursor_x: 0,
                    cursor_y: 0,
                    cursor_z: 0,
                };
                Ok(Some((play::serverbound::BLOCK_PLACE, encode_body(&body)?)))
            }

            // Entity interaction. 1.8's `use_entity` has no `hand` (added 1.9), so
            // the model's hand is dropped for interact/interact-at. Interact-at
            // carries a float hit location and is a distinct wire shape.
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
                EntityInteraction::Interact { hand: _ } => {
                    let body = UseEntity {
                        target: *entity_id,
                        mouse: 0,
                    };
                    Ok(Some((play::serverbound::USE_ENTITY, encode_body(&body)?)))
                }
                EntityInteraction::InteractAt { hand: _, target } => {
                    let body = UseEntityAt {
                        target: *entity_id,
                        mouse: 2,
                        x: target.x as f32,
                        y: target.y as f32,
                        z: target.z as f32,
                    };
                    Ok(Some((play::serverbound::USE_ENTITY, encode_body(&body)?)))
                }
            },

            // Player commands ride on `entity_action`. 1.8 has no elytra
            // (StartFallFlying) and no discrete stop-riding-jump action, so those
            // are rejected loudly rather than silently mapped to a wrong id.
            ClientAction::PlayerCommand { entity_id, command } => {
                let action_id = match command {
                    PlayerCommand::StopSleeping => 2,
                    PlayerCommand::StartSprinting => 3,
                    PlayerCommand::StopSprinting => 4,
                    PlayerCommand::StartRidingJump { .. } => 5,
                    PlayerCommand::OpenInventory => 6,
                    PlayerCommand::StopRidingJump => {
                        return Err(AdapterError::Unsupported(
                            "protocol 47 has no stop-riding-jump entity action".to_owned(),
                        ));
                    }
                    PlayerCommand::StartFallFlying => {
                        return Err(AdapterError::Unsupported(
                            "protocol 47 has no elytra (StartFallFlying) entity action".to_owned(),
                        ));
                    }
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
            // slot sends an empty slot; setting a non-empty creative slot needs an
            // item registry (ResourceKey -> numeric id) that no crate has yet, so
            // it is rejected loudly (same posture as v770).
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
                        "protocol 47 SetCreativeModeSlot with an item requires a ResourceKey -> \
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
            // Inventory clicks predate the modern `state_id` reconciliation and
            // require the item registry to encode the carried/changed stacks, so
            // they are rejected loudly rather than encoded with wrong bytes.
            ClientAction::ContainerClick { .. } => Err(AdapterError::Unsupported(
                "protocol 47 ContainerClick requires an item registry and transaction id that are \
                 not yet available"
                    .to_owned(),
            )),

            // Genuinely absent in 1.8: there is no off-hand and no player-input
            // packet. These fail loudly so a caller cannot mistake a silent no-op
            // for success.
            ClientAction::SwapItemWithOffhand => Err(AdapterError::Unsupported(
                "protocol 47 has no off-hand; SwapItemWithOffhand cannot be encoded".to_owned(),
            )),
            ClientAction::Stab => Err(AdapterError::Unsupported(
                "protocol 47 has no off-hand; Stab (off-hand attack) cannot be encoded".to_owned(),
            )),
            ClientAction::SetPlayerInput(_) => Err(AdapterError::Unsupported(
                "protocol 47 has no player-input packet".to_owned(),
            )),

            // Newly modelled actions (client settings/brand/pong/resource pack,
            // container button click, beacon, book/sign editing, command block,
            // player abilities, trade/pick-item) are not yet wired up for
            // protocol 47. Rejected loudly rather than silently dropped.
            ClientAction::SetClientSettings(_) => Err(AdapterError::Unsupported(
                "protocol 47 client settings encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SendBrand { .. } => Err(AdapterError::Unsupported(
                "protocol 47 brand payload encoding is not yet implemented".to_owned(),
            )),
            ClientAction::PongResponse { .. } => Err(AdapterError::Unsupported(
                "protocol 47 has no configuration/play ping-pong handshake".to_owned(),
            )),
            ClientAction::ResourcePackResponse { .. } => Err(AdapterError::Unsupported(
                "protocol 47 resource pack response encoding is not yet implemented".to_owned(),
            )),
            ClientAction::EndClientTick => Err(AdapterError::Unsupported(
                "protocol 47 has no client_tick_end packet".to_owned(),
            )),
            ClientAction::ContainerButtonClick { .. } => Err(AdapterError::Unsupported(
                "protocol 47 container button click encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SetFlying { .. } => Err(AdapterError::Unsupported(
                "protocol 47 player abilities encoding is not yet implemented".to_owned(),
            )),
            ClientAction::RenameItem { .. } => Err(AdapterError::Unsupported(
                "protocol 47 rename item encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SelectTrade { .. } => Err(AdapterError::Unsupported(
                "protocol 47 select trade encoding is not yet implemented".to_owned(),
            )),
            ClientAction::PickItemFromBlock { .. } => Err(AdapterError::Unsupported(
                "protocol 47 has no pick_item_from_block packet".to_owned(),
            )),
            ClientAction::PickItemFromEntity { .. } => Err(AdapterError::Unsupported(
                "protocol 47 has no pick_item_from_entity packet".to_owned(),
            )),
            ClientAction::SetBeaconEffects { .. } => Err(AdapterError::Unsupported(
                "protocol 47 set beacon encoding requires a mob-effect registry that is not yet \
                 available"
                    .to_owned(),
            )),
            ClientAction::EditBook { .. } => Err(AdapterError::Unsupported(
                "protocol 47 edit book encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SignUpdate { .. } => Err(AdapterError::Unsupported(
                "protocol 47 sign update encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SetCommandBlock { .. } => Err(AdapterError::Unsupported(
                "protocol 47 set command block encoding is not yet implemented".to_owned(),
            )),
            ClientAction::PlayerLoaded => Err(AdapterError::Unsupported(
                "protocol 47 predates the player_loaded packet (added in 1.20.2)".to_owned(),
            )),
            ClientAction::SeenAdvancements { .. } => Err(AdapterError::Unsupported(
                "protocol 47 predates the advancements screen (added in 1.12)".to_owned(),
            )),
            ClientAction::CommandSuggestion { .. } => Err(AdapterError::Unsupported(
                "protocol 47's tab-complete packet has a different wire shape and is not yet \
                 implemented"
                    .to_owned(),
            )),
            ClientAction::PaddleBoat { .. } => Err(AdapterError::Unsupported(
                "protocol 47 paddle boat encoding is not yet implemented".to_owned(),
            )),
            ClientAction::MoveVehicle { .. } => Err(AdapterError::Unsupported(
                "protocol 47 move vehicle encoding is not yet implemented".to_owned(),
            )),

            _ => Ok(None),
        }
    }
}
