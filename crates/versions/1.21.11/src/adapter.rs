//! [`VersionAdapter`] implementation driving this era's join flow, for
//! protocol 774.
//!
//! # The join is three states, not two
//!
//! Handshake → login → **configuration** → play. The login state ends with an
//! acknowledgement, and the configuration state is where the server delivers
//! its registries, its feature flags and its tags, and where the client
//! announces its brand and its client information. Play begins only when both
//! sides exchange a finish-configuration packet.
//! [`V774Adapter::handle_configuration`] is the whole of that phase, and it is
//! not a login-time detour: `start_configuration` can pull a *playing*
//! connection back into it at any time.
//!
//! # What the configuration phase decides
//!
//! The vertical window of every column decoded afterwards. The join packet
//! names its dimension by a **registry index**, and the registry that index
//! points into arrives here as `registry_data` for
//! `minecraft:dimension_type`. An adapter that skips the phase has no way to
//! frame a column: it does not know how many sections one holds, and a wrong
//! section count desynchronises the stream rather than erroring.
//!
//! # What this era adds over the one below it
//!
//! Three things this adapter must do that a 1.20.6-era adapter does not, each
//! silent when missed rather than loud:
//!
//! * **Movement carries a flag byte, not a boolean.** Bit `0x02` reports a
//!   horizontal collision, and the two encodings agree byte-for-byte while the
//!   client is not colliding.
//! * **Chat types arrive as registry-entry holders.** The wire value is
//!   `id + 1`; reading it raw selects the neighbouring chat format.
//! * **The serverbound signed-chat packets end with a checksum byte.**

use std::sync::{Arc, LockResult, Mutex, MutexGuard, PoisonError};

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_data::block_entity_types::block_entity_type;
use lodestone_data::mob_effects::{mob_effect_name_for, MobEffectId};
use lodestone_model::{
    AdapterError, AnimationAction, BlockActionKind, BlockFace, ChatAckInfo, ChatKind, ChatMode,
    ChatSessionInfo, ChunkPos, ClientAction, ClientEvent, ClientSettings, ConnectionState,
    ContainerClickType, Difficulty, Directive, DisplayedSkinParts, EntityInteraction,
    EntityMetadataUpdate, EntityMovement, GameMode, Hand, LoginProfile, MainHand, ParticleStatus,
    PlayerCommand, PlayerInput, PlayerListEntry, ProfileProperty, RecipeBookType, ResourceKey,
    ResourcePackResponseKind, Rotation, SectionPos, ServerAddress, TeleportFlags, Text, Vec3,
    VersionAdapter, WorldSink,
};
use lodestone_world::{ChunkPos as WorldChunkPos, Heightmaps, LoadedChunk};

use crate::canonical::FallbackTally;
use crate::entity_types;
use crate::packets::chat::{
    ChatCommand, ChatMessage, MessageAcknowledgement, PlayerChat, ProfilelessChat, SystemChat,
};
use crate::packets::chunk::{
    ChunkShape, DimensionRegistry, ForgetLevelChunk, LevelChunk, LightUpdatePacket,
};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse, NetworkNbt};
use crate::packets::configuration::{
    AcknowledgeFinishConfiguration, ConfigurationBrandPayload, ConfigurationClientInformation,
    ConfigurationDisconnect, ConfigurationKeepAliveRequest, ConfigurationKeepAliveResponse,
    ConfigurationPing, ConfigurationPong, RegistryData, SelectKnownPacksResponse,
};
use crate::packets::entity::{
    AddEntity, Animate, EntityEvent, EntityMetadataPacket, EntityPositionSync, MoveEntityPos,
    MoveEntityPosRot, MoveEntityRot, RemoveEntities, RotateHead, SetEntityLink, SetEntityMotion,
    SetPassengers, TakeItemEntity, TeleportEntity,
};
use crate::packets::game::{
    AcceptTeleportation, ChangeDifficulty, ChunkBatchFinished, ChunkBatchReceived, ClientCommand,
    ClientTickEnd, ClientboundAbilities, ClientboundPlayerPosition, ClientboundPlayerRotation,
    ConfigurationAcknowledged, GameEvent, Interact, InteractAt, InteractHand,
    JoinGame, MovePlayerPos, MovePlayerPosRot, MovePlayerRot, MovePlayerStatusOnly, OpenSignEditor,
    PlayDisconnect, PlayerAction, PlayerCommandPacket, PlayerInputPacket, PlayerLoaded,
    RecipeBookChangeSettings, RemoveMobEffect, Respawn, SectionBlocksUpdate, SetDefaultSpawnPosition,
    SetExperience, SetHealth, SetTime, Swing, TabList, TeleportToEntity, TickingState, TickingStep,
    UpdateMobEffect, UseItem, UseItemOn, movement_flags, player_input_flags,
};
use crate::packets::handshake::Intention;
use crate::packets::login::{
    EncryptionRequest, LoginAcknowledged, LoginDisconnect, LoginFinished, LoginStart, SetCompression,
};
use crate::packets::metadata::MetadataValue;
use crate::packets::player_info::{PlayerInfoRemove, PlayerInfoUpdate};
use crate::packets::position::Position;
use crate::packets::settings::{
    BrandPayload, ClientInformation, PlayerAbilities, ResourcePackReceive,
};
use crate::packets::slot::Slot;
use crate::packets::window::{
    ChangedSlot, ContainerButtonClick, ContainerClick, ContainerClose, ContainerSetData,
    HashedStack, ServerboundContainerClose, SetCarriedItem, SetCreativeModeSlot, SetHeldSlot,
};

/// The protocol this family speaks, and the one a zero-argument [`adapter`]
/// constructs.
///
/// The folder is named `1.21.11` and this protocol is **774**. Never derive one
/// from the other — ask [`PROTOCOLS`].
pub const PROTOCOL: i32 = PROTOCOL_1_21_11;

/// Protocol version of Minecraft 1.21.11.
///
/// Read off the jar's own `version.json` in `.cache/mc/1.21.11/server.jar`,
/// which reports `"protocol_version": 774`, and independently off
/// `minecraft-data`'s `protocolVersions.json`, which also lists 774 for
/// 1.21.11 and nothing else. The neighbouring releases have their own numbers
/// (773 for 1.21.9/1.21.10, 775 for the release above), so this one number
/// covers exactly one Minecraft version.
pub const PROTOCOL_1_21_11: i32 = 774;

/// Every protocol number this family speaks — the single source of truth for
/// its coverage.
///
/// [`VersionAdapter::supports`] tests membership here, and
/// `lodestone-registry`'s `FAMILIES` entry points at this same slice, so the
/// registry's view of a family cannot drift from the family's own.
///
/// One entry, one Minecraft version. The wire *era* is measurably wider than
/// this list: with `minecraft-data`'s packet shapes and named types inlined
/// recursively, 774 agrees with 773 on 94% of shapes, with 772 on 87% and with
/// 771 on 89% — all above the 85% grouping threshold — while 770 below agrees
/// on only 80%. So protocols 771 through 774 form one era and the lower
/// boundary is real. `PROTOCOLS` lists what is implemented and checked against
/// real bytes, never what the shape measurement permits.
pub const PROTOCOLS: &[i32] = &[PROTOCOL_1_21_11];

/// The packet ids one protocol in this era assigns to the packets this adapter
/// names.
///
/// The generated `packet_ids` tables are one module per protocol, so a
/// `self.ids().player_action` path can only ever mean *one* protocol's id. This
/// struct is the indirection that lets a single adapter body serve several: it
/// is resolved once, at construction, from the negotiated protocol, and every
/// id an arm sends reads through it. Nothing in this file may name a generated
/// module directly outside `packet_ids_from!`.
#[derive(Debug)]
struct PacketIds {
    /// This protocol's whole clientbound play table, the denominator the
    /// dispatch table is built against.
    play_clientbound_entries: &'static [(&'static str, i32)],
    /// `minecraft:intention`, serverbound handshaking.
    handshake_intention: i32,
    /// `minecraft:hello`, serverbound login — the login-start packet.
    login_start: i32,
    /// `minecraft:login_acknowledged`, serverbound login — the packet that
    /// ends the login state.
    login_acknowledged: i32,
    /// `minecraft:login_disconnect`, clientbound login.
    login_disconnect: i32,
    /// `minecraft:hello`, clientbound login — the encryption request.
    login_encryption_request: i32,
    /// `minecraft:login_finished`, clientbound login.
    login_finished: i32,
    /// `minecraft:login_compression`, clientbound login.
    login_compression: i32,
    /// `minecraft:registry_data`, clientbound configuration.
    config_registry_data: i32,
    /// `minecraft:select_known_packs`, clientbound configuration.
    config_select_known_packs: i32,
    /// `minecraft:select_known_packs`, serverbound configuration.
    config_select_known_packs_reply: i32,
    /// `minecraft:finish_configuration`, clientbound configuration.
    config_finish: i32,
    /// `minecraft:finish_configuration`, serverbound configuration.
    config_finish_ack: i32,
    /// `minecraft:keep_alive`, clientbound configuration.
    config_keep_alive: i32,
    /// `minecraft:keep_alive`, serverbound configuration.
    config_keep_alive_reply: i32,
    /// `minecraft:ping`, clientbound configuration.
    config_ping: i32,
    /// `minecraft:pong`, serverbound configuration.
    config_pong: i32,
    /// `minecraft:disconnect`, clientbound configuration.
    config_disconnect: i32,
    /// `minecraft:custom_payload`, serverbound configuration.
    config_custom_payload: i32,
    /// `minecraft:client_information`, serverbound configuration.
    config_client_information: i32,
    /// `minecraft:player_abilities`, serverbound play.
    player_abilities: i32,
    /// `minecraft:swing`, serverbound play.
    swing: i32,
    /// `minecraft:player_action`, serverbound play.
    player_action: i32,
    /// `minecraft:use_item_on`, serverbound play.
    use_item_on: i32,
    /// `minecraft:chat`, serverbound play.
    chat: i32,
    /// `minecraft:chat_command`, serverbound play — the unsigned command
    /// packet, one string and nothing else.
    chat_command: i32,
    /// `minecraft:chat_ack`, serverbound play.
    chat_ack: i32,
    /// `minecraft:chunk_batch_received`, serverbound play — the chunk-pacing
    /// reply without which the server throttles world delivery.
    chunk_batch_received: i32,
    /// `minecraft:client_command`, serverbound play.
    client_command: i32,
    /// `minecraft:container_close`, serverbound play.
    container_close: i32,
    /// `minecraft:configuration_acknowledged`, serverbound play — the reply
    /// that re-enters the configuration phase.
    configuration_acknowledged: i32,
    /// `minecraft:custom_payload`, serverbound play.
    custom_payload: i32,
    /// `minecraft:container_button_click`, serverbound play.
    container_button_click: i32,
    /// `minecraft:player_command`, serverbound play.
    player_command: i32,
    /// `minecraft:player_input`, serverbound play.
    player_input: i32,
    /// `minecraft:move_player_status_only`, serverbound play.
    move_player_status_only: i32,
    /// `minecraft:set_carried_item`, serverbound play.
    set_carried_item: i32,
    /// `minecraft:keep_alive`, serverbound play.
    keep_alive: i32,
    /// `minecraft:move_player_rot`, serverbound play.
    move_player_rot: i32,
    /// `minecraft:pong`, serverbound play.
    pong: i32,
    /// `minecraft:move_player_pos`, serverbound play.
    move_player_pos: i32,
    /// `minecraft:move_player_pos_rot`, serverbound play.
    move_player_pos_rot: i32,
    /// `minecraft:resource_pack`, serverbound play.
    resource_pack: i32,
    /// `minecraft:set_creative_mode_slot`, serverbound play.
    set_creative_mode_slot: i32,
    /// `minecraft:client_information`, serverbound play.
    client_information: i32,
    /// `minecraft:teleport_to_entity`, serverbound play.
    teleport_to_entity: i32,
    /// `minecraft:command_suggestion`, serverbound play.
    command_suggestion: i32,
    /// `minecraft:accept_teleportation`, serverbound play.
    accept_teleportation: i32,
    /// `minecraft:interact`, serverbound play.
    interact: i32,
    /// `minecraft:use_item`, serverbound play.
    use_item: i32,
    /// `minecraft:container_click`, serverbound play.
    container_click: i32,
    /// `minecraft:recipe_book_change_settings`, serverbound play.
    recipe_book_change_settings: i32,
    /// `minecraft:player_loaded`, serverbound play.
    player_loaded: i32,
    /// `minecraft:client_tick_end`, serverbound play.
    client_tick_end: i32,
}

/// Builds a [`PacketIds`] from one generated table module.
macro_rules! packet_ids_from {
    ($table:ident) => {
        PacketIds {
            play_clientbound_entries: crate::$table::play::clientbound::ENTRIES,
            handshake_intention: crate::$table::handshaking::serverbound::INTENTION,
            login_start: crate::$table::login::serverbound::HELLO,
            login_acknowledged: crate::$table::login::serverbound::LOGIN_ACKNOWLEDGED,
            login_disconnect: crate::$table::login::clientbound::LOGIN_DISCONNECT,
            login_encryption_request: crate::$table::login::clientbound::HELLO,
            login_finished: crate::$table::login::clientbound::LOGIN_FINISHED,
            login_compression: crate::$table::login::clientbound::LOGIN_COMPRESSION,
            config_registry_data: crate::$table::configuration::clientbound::REGISTRY_DATA,
            config_select_known_packs:
                crate::$table::configuration::clientbound::SELECT_KNOWN_PACKS,
            config_select_known_packs_reply:
                crate::$table::configuration::serverbound::SELECT_KNOWN_PACKS,
            config_finish: crate::$table::configuration::clientbound::FINISH_CONFIGURATION,
            config_finish_ack: crate::$table::configuration::serverbound::FINISH_CONFIGURATION,
            config_keep_alive: crate::$table::configuration::clientbound::KEEP_ALIVE,
            config_keep_alive_reply: crate::$table::configuration::serverbound::KEEP_ALIVE,
            config_ping: crate::$table::configuration::clientbound::PING,
            config_pong: crate::$table::configuration::serverbound::PONG,
            config_disconnect: crate::$table::configuration::clientbound::DISCONNECT,
            config_custom_payload: crate::$table::configuration::serverbound::CUSTOM_PAYLOAD,
            config_client_information:
                crate::$table::configuration::serverbound::CLIENT_INFORMATION,
            player_abilities: crate::$table::play::serverbound::PLAYER_ABILITIES,
            swing: crate::$table::play::serverbound::SWING,
            player_action: crate::$table::play::serverbound::PLAYER_ACTION,
            use_item_on: crate::$table::play::serverbound::USE_ITEM_ON,
            chat: crate::$table::play::serverbound::CHAT,
            chat_command: crate::$table::play::serverbound::CHAT_COMMAND,
            chat_ack: crate::$table::play::serverbound::CHAT_ACK,
            chunk_batch_received: crate::$table::play::serverbound::CHUNK_BATCH_RECEIVED,
            client_command: crate::$table::play::serverbound::CLIENT_COMMAND,
            container_close: crate::$table::play::serverbound::CONTAINER_CLOSE,
            configuration_acknowledged:
                crate::$table::play::serverbound::CONFIGURATION_ACKNOWLEDGED,
            custom_payload: crate::$table::play::serverbound::CUSTOM_PAYLOAD,
            container_button_click: crate::$table::play::serverbound::CONTAINER_BUTTON_CLICK,
            player_command: crate::$table::play::serverbound::PLAYER_COMMAND,
            player_input: crate::$table::play::serverbound::PLAYER_INPUT,
            move_player_status_only: crate::$table::play::serverbound::MOVE_PLAYER_STATUS_ONLY,
            set_carried_item: crate::$table::play::serverbound::SET_CARRIED_ITEM,
            keep_alive: crate::$table::play::serverbound::KEEP_ALIVE,
            move_player_rot: crate::$table::play::serverbound::MOVE_PLAYER_ROT,
            pong: crate::$table::play::serverbound::PONG,
            move_player_pos: crate::$table::play::serverbound::MOVE_PLAYER_POS,
            move_player_pos_rot: crate::$table::play::serverbound::MOVE_PLAYER_POS_ROT,
            resource_pack: crate::$table::play::serverbound::RESOURCE_PACK,
            set_creative_mode_slot: crate::$table::play::serverbound::SET_CREATIVE_MODE_SLOT,
            client_information: crate::$table::play::serverbound::CLIENT_INFORMATION,
            teleport_to_entity: crate::$table::play::serverbound::TELEPORT_TO_ENTITY,
            command_suggestion: crate::$table::play::serverbound::COMMAND_SUGGESTION,
            accept_teleportation: crate::$table::play::serverbound::ACCEPT_TELEPORTATION,
            interact: crate::$table::play::serverbound::INTERACT,
            use_item: crate::$table::play::serverbound::USE_ITEM,
            container_click: crate::$table::play::serverbound::CONTAINER_CLICK,
            recipe_book_change_settings:
                crate::$table::play::serverbound::RECIPE_BOOK_CHANGE_SETTINGS,
            player_loaded: crate::$table::play::serverbound::PLAYER_LOADED,
            client_tick_end: crate::$table::play::serverbound::CLIENT_TICK_END,
        }
    };
}

/// Protocol 774's ids.
static IDS_774: PacketIds = packet_ids_from!(packet_ids);

/// Resolves a negotiated protocol to its id table.
///
/// # Panics
///
/// Panics for a protocol outside [`PROTOCOLS`]. This is a construction-time
/// check on a value the registry has already tested for membership, not a wire
/// value: reaching it means a caller bypassed `VersionAdapter::supports`, and
/// answering with some other protocol's ids would be the silent-wrong-wire
/// failure this indirection exists to prevent.
fn ids_for(protocol: i32) -> &'static PacketIds {
    match protocol {
        PROTOCOL_1_21_11 => &IDS_774,
        other => panic!(
            "protocol {other} is outside this family's PROTOCOLS ({PROTOCOLS:?}); \
             callers must test membership before constructing an adapter"
        ),
    }
}

/// Requested next-state value in the handshake for a login connection.
const NEXT_STATE_LOGIN: i32 = 2;

/// Relative-teleport flag bits on the clientbound position packet.
///
/// A 32-bit word here, where the 1.20.6 era packs the same idea into a single
/// byte with five assigned bits. Four more are assigned: the three velocity
/// components and a "rotate the velocity by the yaw delta" bit.
const REL_X: i32 = 0x001;
const REL_Y: i32 = 0x002;
const REL_Z: i32 = 0x004;
const REL_YAW: i32 = 0x008;
const REL_PITCH: i32 = 0x010;

/// The registry whose entry order the join packet's dimension index refers to.
/// Every other registry the configuration phase delivers is passed over.
const DIMENSION_TYPE_REGISTRY: &str = "minecraft:dimension_type";

/// Columns per tick this client asks for when it answers a chunk batch.
///
/// The value is a *request*, not a measurement, and a server that receives
/// none throttles chunk delivery to its floor. Chosen at the vanilla server's
/// own per-tick cap so a batch reply never asks for less than the server would
/// send unprompted.
const CHUNKS_PER_TICK: f32 = 64.0;

/// Per-connection state used by this era's client-side position-send tick.
#[derive(Debug, Clone, Copy)]
struct MovementSendState {
    last_pos: Vec3,
    last_yaw: f32,
    last_pitch: f32,
    last_flags: u8,
    position_reminder: u32,
}

impl Default for MovementSendState {
    fn default() -> Self {
        Self {
            last_pos: Vec3::new(0.0, 0.0, 0.0),
            last_yaw: 0.0,
            last_pitch: 0.0,
            last_flags: 0,
            position_reminder: 0,
        }
    }
}

fn recover_movement_state<'a>(
    result: LockResult<MutexGuard<'a, MovementSendState>>,
) -> MutexGuard<'a, MovementSendState> {
    result.unwrap_or_else(PoisonError::into_inner)
}

/// Version adapter implementing this era's protocol.
///
/// Four pieces of per-connection state, every one load-bearing:
///
/// * `dimension_registry`, the `minecraft:dimension_type` entries the
///   configuration phase delivered. The join packet names its dimension by an
///   **index into this**, so without it there is no vertical window and no way
///   to frame a column.
/// * `shape`, the resolved window itself, re-resolved on every join and
///   respawn.
/// * `pending_ack`, the count of signed player-chat messages received but not
///   yet acknowledged. A server whose pending list is never drained
///   disconnects the connection, so this counter is what keeps a chat-reading
///   session alive.
/// * `movement`, the client-side position-send state.
#[derive(Debug, Clone)]
pub struct V774Adapter {
    /// The negotiated protocol this adapter speaks: one of [`PROTOCOLS`].
    protocol: i32,
    /// This protocol's id table, resolved once at construction by [`ids_for`].
    ids: &'static PacketIds,
    shape: Arc<Mutex<ChunkShape>>,
    dimension_registry: Arc<Mutex<DimensionRegistry>>,
    pending_ack: Arc<Mutex<i32>>,
    movement: Arc<Mutex<MovementSendState>>,
}

impl Default for V774Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl V774Adapter {
    /// Creates a new adapter speaking [`PROTOCOL`].
    #[must_use]
    pub fn new() -> Self {
        Self::for_protocol(PROTOCOL)
    }

    /// Creates an adapter for one of [`PROTOCOLS`], resolving that protocol's
    /// id table, block-state table and chunk shape once, here.
    ///
    /// # Panics
    ///
    /// Panics for a protocol outside [`PROTOCOLS`] — see [`ids_for`].
    #[must_use]
    pub fn for_protocol(protocol: i32) -> Self {
        Self {
            protocol,
            ids: ids_for(protocol),
            shape: Arc::new(Mutex::new(ChunkShape::overworld(protocol))),
            dimension_registry: Arc::new(Mutex::new(DimensionRegistry::default())),
            pending_ack: Arc::new(Mutex::new(0)),
            movement: Arc::new(Mutex::new(MovementSendState::default())),
        }
    }

    /// The codec context for the protocol this adapter was constructed for.
    const fn ctx(&self) -> Ctx {
        Ctx {
            version: self.protocol,
        }
    }

    /// The generated packet-id table for the protocol this adapter was
    /// constructed for.
    const fn ids(&self) -> &'static PacketIds {
        self.ids
    }

    fn encode_body<T: Encode>(&self, packet: &T) -> Result<Vec<u8>, AdapterError> {
        lodestone_core::encode_body(packet, self.ctx()).map_err(AdapterError::Encode)
    }

    fn decode_body<T: Decode>(&self, payload: &[u8]) -> Result<T, AdapterError> {
        lodestone_core::decode_body(payload, self.ctx()).map_err(AdapterError::Decode)
    }

    /// Like [`Self::decode_body`] but additionally requires the payload to be
    /// fully consumed. Used where trailing bytes signal a wrong layout and
    /// must be rejected rather than silently ignored.
    fn decode_body_exact<T: Decode>(&self, payload: &[u8]) -> Result<T, AdapterError> {
        lodestone_core::decode_body_exact(payload, self.ctx()).map_err(AdapterError::Decode)
    }

    fn send<T: Encode>(&self, packet_id: i32, packet: &T) -> Result<Directive, AdapterError> {
        Ok(Directive::Send {
            packet_id,
            payload: self.encode_body(packet)?,
        })
    }

    /// The column shape in force right now.
    fn current_shape(&self) -> ChunkShape {
        self.shape
            .lock()
            .map(|shape| *shape)
            .unwrap_or_else(|err| *err.into_inner())
    }

    /// Records the dimension-type registry one `registry_data` packet carried.
    fn adopt_dimension_registry(&self, data: &RegistryData) {
        if let Ok(mut registry) = self.dimension_registry.lock() {
            registry.adopt(&data.entries);
        }
    }

    /// Re-resolves the column shape from the registry entry at `index`.
    ///
    /// Leaves the shape untouched when the registry cannot answer — see
    /// [`ChunkShape::from_dimension_index`] for why guessing a height is the
    /// one thing that must not happen.
    fn adopt_dimension_shape(&self, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Ok(registry) = self.dimension_registry.lock() else {
            return;
        };
        let Ok(mut shape) = self.shape.lock() else {
            return;
        };
        if let Some(resolved) = shape.from_dimension_index(&registry, index) {
            *shape = resolved;
        }
    }

    /// Counts one signed player-chat message as owed an acknowledgement.
    fn note_pending_ack(&self) {
        if let Ok(mut pending) = self.pending_ack.lock() {
            *pending = pending.saturating_add(1);
        }
    }

    /// Takes and clears the owed acknowledgement count.
    fn take_pending_ack(&self) -> i32 {
        self.pending_ack
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default()
    }

    /// Selects this era's movement shape.
    ///
    /// The four packets differ only in which of position and rotation they
    /// carry; all four end with the same [`movement_flags`] byte, which is
    /// what makes the collision bit reachable from every one of them.
    fn select_move_packet(
        &self,
        pos: Vec3,
        rotation: Rotation,
        on_ground: bool,
        horizontal_collision: bool,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        let mut state = recover_movement_state(self.movement.lock());
        let flags = movement_flags::pack(on_ground, horizontal_collision);
        let dx = pos.x - state.last_pos.x;
        let dy = pos.y - state.last_pos.y;
        let dz = pos.z - state.last_pos.z;
        state.position_reminder += 1;
        let moved = dx * dx + dy * dy + dz * dz > 9.0e-4 || state.position_reminder >= 20;
        let rotated = rotation.yaw != state.last_yaw || rotation.pitch != state.last_pitch;

        let packet = if moved && rotated {
            let body = MovePlayerPosRot {
                x: pos.x,
                y: pos.y,
                z: pos.z,
                yaw: rotation.yaw,
                pitch: rotation.pitch,
                flags,
            };
            Some((self.ids().move_player_pos_rot, self.encode_body(&body)?))
        } else if moved {
            let body = MovePlayerPos {
                x: pos.x,
                y: pos.y,
                z: pos.z,
                flags,
            };
            Some((self.ids().move_player_pos, self.encode_body(&body)?))
        } else if rotated {
            let body = MovePlayerRot {
                yaw: rotation.yaw,
                pitch: rotation.pitch,
                flags,
            };
            Some((self.ids().move_player_rot, self.encode_body(&body)?))
        } else if state.last_flags != flags {
            let body = MovePlayerStatusOnly { flags };
            Some((
                self.ids().move_player_status_only,
                self.encode_body(&body)?,
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
        state.last_flags = flags;
        Ok(packet)
    }

    /// Handles a clientbound packet while in the login state.
    ///
    /// Login `login_finished` does **not** enter play. It enters configuration,
    /// and the transition is explicit — the client sends `login_acknowledged`
    /// first, then the brand and client-information packets the phase expects,
    /// all after the state change so they are framed as configuration packets.
    fn handle_login(&self, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == self.ids().login_compression {
            let body: SetCompression = self.decode_body(payload)?;
            return Ok(vec![Directive::SetCompression(body.threshold)]);
        }
        if packet_id == self.ids().login_finished {
            let _profile: LoginFinished = self.decode_body(payload)?;
            return Ok(vec![
                self.send(self.ids().login_acknowledged, &LoginAcknowledged)?,
                Directive::SetState(ConnectionState::Configuration),
                self.send(
                    self.ids().config_custom_payload,
                    &ConfigurationBrandPayload {
                        channel: "minecraft:brand".to_owned(),
                        brand: "vanilla".to_owned(),
                    },
                )?,
                self.send(
                    self.ids().config_client_information,
                    &default_client_information(),
                )?,
            ]);
        }
        if packet_id == self.ids().login_encryption_request {
            let _request: EncryptionRequest = self.decode_body(payload)?;
            return Err(AdapterError::Unsupported(
                "encryption / online-mode authentication (login encryption request) is not yet \
                 implemented; connect to an offline-mode server"
                    .to_owned(),
            ));
        }
        if packet_id == self.ids().login_disconnect {
            let body: LoginDisconnect = self.decode_body(payload)?;
            return Ok(vec![Directive::Disconnect(json_reason_text(&body.reason))]);
        }
        Ok(Vec::new())
    }

    /// Handles a clientbound packet while in the configuration state.
    ///
    /// # Why the known-packs reply is empty
    ///
    /// `select_known_packs` offers to *elide* registry payloads for any data
    /// pack the client claims. Claiming none is what makes the dimension
    /// registry arrive with its `min_y` and `height` values in it — and those
    /// values are the only way to frame a column. Claiming the vanilla core
    /// pack would save a few kilobytes and leave every column unframeable.
    ///
    /// # Why an unrecognised packet is silently dropped here
    ///
    /// Unlike the play state, this phase has no dispatch table with an
    /// enumerated ignore list: the packets that matter are the four the join
    /// depends on, and the rest (tags, feature flags, resource-pack pushes,
    /// cookies, dialogs) are the server telling the client things this client
    /// does not act on during configuration. A `finish_configuration` cannot be
    /// missed because it is matched explicitly.
    fn handle_configuration(
        &self,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let ids = self.ids();
        if packet_id == ids.config_registry_data {
            let body: RegistryData = self.decode_body(payload)?;
            if body.registry == DIMENSION_TYPE_REGISTRY {
                self.adopt_dimension_registry(&body);
            }
            return Ok(Vec::new());
        }
        if packet_id == ids.config_select_known_packs {
            return Ok(vec![self.send(
                ids.config_select_known_packs_reply,
                &SelectKnownPacksResponse { packs: Vec::new() },
            )?]);
        }
        if packet_id == ids.config_finish {
            return Ok(vec![
                self.send(ids.config_finish_ack, &AcknowledgeFinishConfiguration)?,
                Directive::SetState(ConnectionState::Play),
            ]);
        }
        if packet_id == ids.config_keep_alive {
            let body: ConfigurationKeepAliveRequest = self.decode_body(payload)?;
            return Ok(vec![self.send(
                ids.config_keep_alive_reply,
                &ConfigurationKeepAliveResponse { id: body.id },
            )?]);
        }
        if packet_id == ids.config_ping {
            let body: ConfigurationPing = self.decode_body(payload)?;
            return Ok(vec![
                self.send(ids.config_pong, &ConfigurationPong { id: body.id })?,
            ]);
        }
        if packet_id == ids.config_disconnect {
            let body: ConfigurationDisconnect = self.decode_body(payload)?;
            return Ok(vec![Directive::Disconnect(nbt_reason_text(&body.reason))]);
        }
        Ok(Vec::new())
    }
}

/// The client information this adapter announces on entering configuration.
///
/// A server needs a locale and a view distance before play begins, and a
/// connection that sends none is disconnected by a server with strict error
/// handling on. The values are this client's defaults; a later
/// `SetClientSettings` action replaces them through the play-state packet.
fn default_client_information() -> ConfigurationClientInformation {
    ConfigurationClientInformation {
        locale: "en_us".to_owned(),
        view_distance: 8,
        chat_flags: 0,
        chat_colors: true,
        skin_parts: 0x7f,
        main_hand: 1,
        text_filtering: false,
        allow_server_listing: true,
        particle_status: 0,
    }
}

/// Maps the model's `RecipeBookType` onto this era's recipe-book ordinal.
fn recipe_book_type_to_ordinal(book_type: RecipeBookType) -> i32 {
    match book_type {
        RecipeBookType::Crafting => 0,
        RecipeBookType::Furnace => 1,
        RecipeBookType::BlastFurnace => 2,
        RecipeBookType::Smoker => 3,
    }
}

/// Decodes a JSON disconnect reason into a [`Text`] tree, falling back to a
/// generic message when the component carries no text.
fn json_reason_text(reason: &str) -> Text {
    let text = Text::from_json(reason);
    if text.to_plain_string().is_empty() {
        Text::literal("Disconnected")
    } else {
        text
    }
}

/// Decodes an anonymous-NBT disconnect reason into a [`Text`] tree.
///
/// The play- and configuration-state disconnects use this; the login-state one
/// is still a JSON string at this protocol and uses [`json_reason_text`].
/// Keeping two functions rather than one that sniffs the payload is
/// deliberate: the *state* decides the form, and a sniff would silently accept
/// the wrong one.
fn nbt_reason_text(reason: &NetworkNbt) -> Text {
    let text = Text::from_nbt(&reason.0);
    if text.to_plain_string().is_empty() {
        Text::literal("Disconnected")
    } else {
        text
    }
}

/// Maps a game-mode byte to the canonical [`GameMode`].
fn game_mode(value: u8) -> Result<GameMode, AdapterError> {
    match value & 0x7 {
        0 => Ok(GameMode::Survival),
        1 => Ok(GameMode::Creative),
        2 => Ok(GameMode::Adventure),
        3 => Ok(GameMode::Spectator),
        other => Err(AdapterError::Decode(format!("unknown game mode {other}"))),
    }
}

/// Parses a namespaced level name into a canonical
/// [`DimensionId`](lodestone_model::DimensionId).
fn dimension_id(name: &str) -> Result<lodestone_model::DimensionId, AdapterError> {
    name.parse()
        .map_err(|_| AdapterError::Decode(format!("invalid dimension identifier {name}")))
}

/// Decodes a packet body that is exactly one anonymous-NBT text component and
/// nothing else — the shape the split title packets and the action bar share.
fn decode_single_nbt_text(payload: &[u8]) -> Result<Text, AdapterError> {
    let mut reader = Reader::new(payload);
    let nbt = lodestone_core::read_network_nbt(&mut reader).map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(Text::from_nbt(&nbt))
}

/// Delta-position scale: each `i16` is `1/4096` of a block.
const MOVE_DELTA_SCALE: f64 = 4096.0;

/// Converts a signed-byte angle to degrees (256 steps per full circle).
fn unpack_degrees(packed: i8) -> f32 {
    lodestone_core::unpack_degrees(packed)
}

/// Maps a canonical [`BlockFace`] to its numeric ordinal.
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

/// Maps a canonical [`Hand`] to its numeric ordinal.
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

/// Maps a canonical [`ChatMode`] to the wire chat-visibility value.
const fn chat_mode_value(mode: ChatMode) -> i32 {
    match mode {
        ChatMode::Full => 0,
        ChatMode::CommandsOnly => 1,
        ChatMode::Hidden => 2,
    }
}

/// Maps a canonical [`MainHand`] to the wire value.
const fn main_hand_value(hand: MainHand) -> i32 {
    match hand {
        MainHand::Left => 0,
        MainHand::Right => 1,
    }
}

/// Maps a canonical [`ParticleStatus`] to this era's wire ordinal.
///
/// The field has no counterpart in the 1.20.6 era's client-information packet,
/// so this mapping exists only here.
const fn particle_status_value(status: ParticleStatus) -> i32 {
    match status {
        ParticleStatus::All => 0,
        ParticleStatus::Decreased => 1,
        ParticleStatus::Minimal => 2,
    }
}

/// Packs a [`PlayerInput`] into this era's single input byte.
const fn player_input_bits(input: PlayerInput) -> u8 {
    (if input.forward {
        player_input_flags::FORWARD
    } else {
        0
    }) | (if input.backward {
        player_input_flags::BACKWARD
    } else {
        0
    }) | (if input.left {
        player_input_flags::LEFT
    } else {
        0
    }) | (if input.right {
        player_input_flags::RIGHT
    } else {
        0
    }) | (if input.jump {
        player_input_flags::JUMP
    } else {
        0
    }) | (if input.shift {
        player_input_flags::SHIFT
    } else {
        0
    }) | (if input.sprint {
        player_input_flags::SPRINT
    } else {
        0
    })
}

/// Maps a canonical [`ContainerClickType`] to this era's click-mode ordinal.
const fn click_mode_value(click_type: ContainerClickType) -> i32 {
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

/// The ability flag bit set when the player is invulnerable.
const ABILITY_INVULNERABLE: i8 = 0x01;
/// The flying-ability flag bit set when the client is flying.
const ABILITY_FLYING: i8 = 0x02;
/// The ability flag bit set when the player may fly.
const ABILITY_CAN_FLY: i8 = 0x04;
/// The ability flag bit set when the player may instantly build/break.
const ABILITY_INSTABUILD: i8 = 0x08;

/// The metadata index carrying the shared entity flags byte.
///
/// Index `0` with a byte serializer is the base entity's own flags field —
/// on-fire, crouching, sprinting, swimming, invisible, glowing, fall-flying —
/// and it is the one index whose meaning needs no knowledge of the entity's
/// type, because every entity inherits it. Every *other* index collides
/// between entity categories at this protocol, which is why this arm reports
/// only this one.
const METADATA_INDEX_SHARED_FLAGS: u8 = 0;

/// Converts a low-level [`lodestone_core::Error`] into an [`AdapterError`].
fn dec_err(err: impl std::fmt::Display) -> AdapterError {
    AdapterError::Decode(err.to_string())
}

/// Fn-pointer payload every `play` clientbound handler below shares.
type PlayHandler =
    fn(&V774Adapter, &mut dyn WorldSink, &[u8]) -> Result<Vec<Directive>, AdapterError>;

impl V774Adapter {
    /// `minecraft:login`. Names its dimension by a registry index — see
    /// [`V774Adapter::adopt_dimension_shape`].
    fn handle_play_login(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: JoinGame = adapter.decode_body(payload)?;
        adapter.adopt_dimension_shape(body.world_state.dimension);
        Ok(vec![Directive::Emit(ClientEvent::Login {
            entity_id: body.entity_id,
            game_mode: game_mode(body.world_state.game_mode as u8)?,
            dimension: dimension_id(&body.world_state.world_name)?,
        })])
    }

    /// `minecraft:respawn`. Carries the same spawn description the join packet
    /// does, so a respawn into a dimension of a different height re-resolves
    /// the column shape here rather than inheriting a stale one.
    fn handle_play_respawn(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: Respawn = adapter.decode_body(payload)?;
        adapter.adopt_dimension_shape(body.world_state.dimension);
        Ok(vec![Directive::Emit(ClientEvent::Respawned {
            dimension: dimension_id(&body.world_state.world_name)?,
            game_mode: game_mode(body.world_state.game_mode as u8)?,
            previous_game_mode: None,
            last_death_location: None,
        })])
    }

    /// `minecraft:level_chunk_with_light`.
    fn handle_play_level_chunk_with_light(
        adapter: &V774Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let shape = adapter.current_shape();
        let mut reader = Reader::new(payload);
        let data = LevelChunk::decode(&mut reader, &shape).map_err(dec_err)?;
        // Zero trailing bytes across the whole packet is the best available
        // detector of a subtly wrong layout: reject rather than apply a
        // silently misaligned column.
        reader.ensure_empty().map_err(dec_err)?;
        let pos = ChunkPos::new(data.x, data.z);
        world.load(
            WorldChunkPos::new(data.x, data.z),
            LoadedChunk::new(
                data.column,
                data.light,
                Heightmaps::new(),
                data.block_entities,
            ),
        );
        Ok(vec![Directive::Emit(ClientEvent::ChunkLoaded { pos })])
    }

    /// `minecraft:light_update`.
    fn handle_play_light_update(
        adapter: &V774Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let update =
            LightUpdatePacket::decode(&mut reader, &adapter.current_shape()).map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        world.merge_light(WorldChunkPos::new(update.x, update.z), update.patch);
        Ok(Vec::new())
    }

    /// `minecraft:forget_level_chunk`. Two plain ints, **z first** — see
    /// [`ForgetLevelChunk`].
    fn handle_play_forget_level_chunk(
        adapter: &V774Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ForgetLevelChunk = adapter.decode_body_exact(payload)?;
        let pos = ChunkPos::new(body.chunk_x, body.chunk_z);
        world.unload(WorldChunkPos::new(body.chunk_x, body.chunk_z));
        Ok(vec![Directive::Emit(ClientEvent::ChunkUnloaded { pos })])
    }

    /// `minecraft:keep_alive`.
    fn handle_play_keep_alive(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let keep_alive: KeepAliveRequest = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::KeepAlive {
            id: keep_alive.id,
        })])
    }

    /// `minecraft:ping` (play state). Answered immediately with a pong; the
    /// event is emitted so a consumer can time the round trip.
    fn handle_play_ping(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let id = reader.i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        let mut writer = Writer::default();
        writer.i32(id);
        Ok(vec![
            Directive::Send {
                packet_id: adapter.ids().pong,
                payload: writer.into_vec(),
            },
            Directive::Emit(ClientEvent::Ping { id }),
        ])
    }

    /// `minecraft:system_chat` — server text, as an anonymous-NBT component.
    fn handle_play_system_chat(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SystemChat = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::Chat {
            text: Text::from_nbt(&body.content.0),
            kind: if body.is_action_bar {
                ChatKind::GameInfo
            } else {
                ChatKind::System
            },
            sender: None,
            ack: None,
        })])
    }

    /// `minecraft:disguised_chat`.
    fn handle_play_disguised_chat(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ProfilelessChat = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::Chat {
            text: Text::from_nbt(&body.message.0),
            kind: ChatKind::Chat,
            sender: None,
            ack: None,
        })])
    }

    /// `minecraft:player_chat` — a message a player wrote.
    ///
    /// Two things this arm does that a pre-1.19 era cannot. It reports the
    /// **sender's profile id**, which is the key a hide-in-chat filter needs;
    /// and it fills a [`ChatAckInfo`] and bumps the pending-acknowledgement
    /// counter, which is what keeps the connection alive — a server whose
    /// pending list is never drained disconnects the client.
    ///
    /// The `global_index` field is genuinely on this wire, unlike in the era
    /// below where the same [`ChatAckInfo`] field has to be filled with the
    /// per-sender chain index for want of anything better.
    ///
    /// The displayed text prefers the server's decorated form, but
    /// `raw_content` always keeps the *signed* string: a signature is taken
    /// over exactly that and never over the decoration.
    fn handle_play_player_chat(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: PlayerChat = adapter.decode_body_exact(payload)?;
        adapter.note_pending_ack();
        let signature = body
            .signature
            .as_ref()
            .map(|sig| sig.0.to_vec())
            .unwrap_or_default();
        let last_seen: Vec<Vec<u8>> = body
            .previous_messages
            .iter()
            .filter_map(|entry| entry.signature.as_ref().map(|sig| sig.0.to_vec()))
            .collect();
        let text = match &body.unsigned_content {
            Some(nbt) => Text::from_nbt(&nbt.0),
            None => Text::literal(body.plain_message.clone()),
        };
        Ok(vec![Directive::Emit(ClientEvent::Chat {
            text,
            kind: ChatKind::Chat,
            sender: Some(body.sender),
            ack: Some(ChatAckInfo {
                signature,
                global_index: body.global_index,
                // Filter type `1` is "fully filtered": the message still
                // burns an acknowledgement but is not shown.
                was_shown: body.filter_type != 1,
                message_index: body.index,
                timestamp_millis: body.timestamp,
                salt: body.salt,
                raw_content: body.plain_message,
                last_seen,
                // Fail-closed — only the client driver holds the sender's
                // public key and may raise this.
                verified: false,
            }),
        })])
    }

    /// `minecraft:delete_chat` — retract a delivered signed message.
    ///
    /// A cache-index reference (`id != 0`) cannot be resolved here: that needs
    /// a per-connection signature cache this adapter does not keep, so only
    /// the inline-signature form produces an event.
    fn handle_play_delete_chat(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let id = reader.var_i32().map_err(dec_err)?;
        if id != 0 {
            return Ok(Vec::new());
        }
        let signature = reader.bytes(256).map_err(dec_err)?.to_vec();
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::ChatMessageDeleted {
            signature: lodestone_model::PackedMessageSignature::Full(signature),
        })])
    }

    /// `minecraft:player_position`.
    ///
    /// The teleport id leads the packet here and trails it in the era below,
    /// and the flag set is a 32-bit word rather than a byte. Only the five
    /// position/rotation bits map onto the canonical
    /// [`TeleportFlags`](lodestone_model::TeleportFlags); the four velocity
    /// bits have no field there, and the velocity itself is reported through
    /// the separate velocity event so nothing is silently dropped.
    fn handle_play_player_position(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ClientboundPlayerPosition = adapter.decode_body_exact(payload)?;
        let flags = TeleportFlags {
            relative_x: body.flags & REL_X != 0,
            relative_y: body.flags & REL_Y != 0,
            relative_z: body.flags & REL_Z != 0,
            relative_yaw: body.flags & REL_YAW != 0,
            relative_pitch: body.flags & REL_PITCH != 0,
        };
        // The teleport id must be echoed back or the server rubber-bands the
        // player. The confirm choreography lives entirely in the version
        // crate; the driver just runs the directives in order.
        let confirm = AcceptTeleportation {
            teleport_id: body.teleport_id,
        };
        Ok(vec![
            adapter.send(adapter.ids().accept_teleportation, &confirm)?,
            Directive::Emit(ClientEvent::TeleportPlayer {
                pos: Vec3::new(body.x, body.y, body.z),
                rotation: Rotation::new(body.yaw, body.pitch),
                flags,
            }),
        ])
    }

    /// `minecraft:player_rotation` — a rotation-only correction with no
    /// counterpart in the era below.
    ///
    /// Reported as a teleport whose position is relative and zero, which is
    /// exactly what a rotation-only correction is: the three position bits are
    /// set so a consumer applies no displacement.
    fn handle_play_player_rotation(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ClientboundPlayerRotation = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TeleportPlayer {
            pos: Vec3::new(0.0, 0.0, 0.0),
            rotation: Rotation::new(body.yaw, body.pitch),
            flags: TeleportFlags {
                relative_x: true,
                relative_y: true,
                relative_z: true,
                relative_yaw: body.relative_yaw,
                relative_pitch: body.relative_pitch,
            },
        })])
    }

    /// `minecraft:chunk_batch_start`. Opens a paced batch; nothing to report
    /// until it closes.
    fn handle_play_chunk_batch_start(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(Vec::new())
    }

    /// `minecraft:chunk_batch_finished`. Answering this is not politeness: a
    /// server that gets no pacing reply throttles chunk delivery to its floor,
    /// so a client that ignores the packet loads the world at a trickle with
    /// nothing logged anywhere.
    fn handle_play_chunk_batch_finished(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let _body: ChunkBatchFinished = adapter.decode_body_exact(payload)?;
        Ok(vec![adapter.send(
            adapter.ids().chunk_batch_received,
            &ChunkBatchReceived {
                chunks_per_tick: CHUNKS_PER_TICK,
            },
        )?])
    }

    /// `minecraft:start_configuration`. The server pulls a live session back
    /// into the configuration phase; the client acknowledges and re-enters it.
    /// Without this the next `registry_data` is read as a play packet.
    fn handle_play_start_configuration(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(vec![
            adapter.send(
                adapter.ids().configuration_acknowledged,
                &ConfigurationAcknowledged,
            )?,
            Directive::SetState(ConnectionState::Configuration),
        ])
    }

    /// `minecraft:add_entity` — **every** entity at this protocol, including
    /// players, told apart only by the type id resolved through
    /// [`crate::entity_types`].
    fn handle_play_add_entity(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: AddEntity = adapter.decode_body(payload)?;
        let type_id = body.kind;
        let entity_type = entity_types::table_for(adapter.protocol)
            .entity_type_name(type_id)
            .ok_or_else(|| {
                AdapterError::Decode(format!("unknown entity type id {type_id} in spawn"))
            })?
            .parse()
            .map_err(|_| AdapterError::Decode(format!("entity type id {type_id} is not a key")))?;
        // Velocity is always on the wire, but this era spells a stationary
        // entity's as a single zero byte; forward `None` for it, to match "no
        // motion" rather than "explicit zero motion".
        let velocity = if body.velocity.is_zero() {
            None
        } else {
            Some(Vec3::new(
                body.velocity.x,
                body.velocity.y,
                body.velocity.z,
            ))
        };
        Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
            entity_id: body.entity_id,
            uuid: Some(body.object_uuid),
            entity_type,
            pos: Vec3::new(body.x, body.y, body.z),
            rotation: Rotation::new(unpack_degrees(body.yaw), unpack_degrees(body.pitch)),
            velocity,
        })])
    }

    /// `minecraft:move_entity_pos`.
    fn handle_play_move_entity_pos(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: MoveEntityPos = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
            entity_id: body.entity_id,
            movement: EntityMovement::Relative(Vec3::new(
                f64::from(body.delta_x) / MOVE_DELTA_SCALE,
                f64::from(body.delta_y) / MOVE_DELTA_SCALE,
                f64::from(body.delta_z) / MOVE_DELTA_SCALE,
            )),
            rotation: None,
            on_ground: body.on_ground,
        })])
    }

    /// `minecraft:move_entity_pos_rot`.
    fn handle_play_move_entity_pos_rot(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: MoveEntityPosRot = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
            entity_id: body.entity_id,
            movement: EntityMovement::Relative(Vec3::new(
                f64::from(body.delta_x) / MOVE_DELTA_SCALE,
                f64::from(body.delta_y) / MOVE_DELTA_SCALE,
                f64::from(body.delta_z) / MOVE_DELTA_SCALE,
            )),
            rotation: Some(Rotation::new(
                unpack_degrees(body.yaw),
                unpack_degrees(body.pitch),
            )),
            on_ground: body.on_ground,
        })])
    }

    /// `minecraft:move_entity_rot`.
    fn handle_play_move_entity_rot(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: MoveEntityRot = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
            entity_id: body.entity_id,
            movement: EntityMovement::Relative(Vec3::new(0.0, 0.0, 0.0)),
            rotation: Some(Rotation::new(
                unpack_degrees(body.yaw),
                unpack_degrees(body.pitch),
            )),
            on_ground: body.on_ground,
        })])
    }

    /// `minecraft:teleport_entity`.
    fn handle_play_teleport_entity(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: TeleportEntity = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
            entity_id: body.entity_id,
            movement: EntityMovement::Absolute(Vec3::new(body.x, body.y, body.z)),
            rotation: Some(Rotation::new(
                unpack_degrees(body.yaw),
                unpack_degrees(body.pitch),
            )),
            on_ground: body.on_ground,
        })])
    }

    /// `minecraft:entity_position_sync` — absolute position, velocity and
    /// full-precision angles in one packet, with no counterpart in the era
    /// below.
    ///
    /// Two events, because the packet carries two independent facts: a
    /// position the client must not interpolate towards, and a velocity. A
    /// consumer that got only the first would keep whatever velocity the last
    /// relative move implied.
    fn handle_play_entity_position_sync(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityPositionSync = adapter.decode_body_exact(payload)?;
        Ok(vec![
            Directive::Emit(ClientEvent::EntityMoved {
                entity_id: body.entity_id,
                movement: EntityMovement::Absolute(Vec3::new(body.x, body.y, body.z)),
                rotation: Some(Rotation::new(body.yaw, body.pitch)),
                on_ground: body.on_ground,
            }),
            Directive::Emit(ClientEvent::EntityVelocity {
                entity_id: body.entity_id,
                velocity: Vec3::new(body.dx, body.dy, body.dz),
            }),
        ])
    }

    /// `minecraft:set_entity_motion`.
    fn handle_play_set_entity_motion(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SetEntityMotion = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityVelocity {
            entity_id: body.entity_id,
            velocity: Vec3::new(body.velocity.x, body.velocity.y, body.velocity.z),
        })])
    }

    /// `minecraft:rotate_head`.
    fn handle_play_rotate_head(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: RotateHead = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityHeadRotation {
            entity_id: body.entity_id,
            head_yaw: unpack_degrees(body.head_yaw),
        })])
    }

    /// `minecraft:remove_entities`.
    fn handle_play_remove_entities(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: RemoveEntities = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityRemoved {
            entity_ids: body.entity_ids,
        })])
    }

    /// `minecraft:entity_event`.
    fn handle_play_entity_event(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityEvent = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityStatus {
            entity_id: body.entity_id,
            status: body.entity_status as u8,
        })])
    }

    /// `minecraft:animate`.
    fn handle_play_animate(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: Animate = adapter.decode_body_exact(payload)?;
        let action = match body.animation {
            0 => AnimationAction::SwingMainHand,
            2 => AnimationAction::WakeUp,
            3 => AnimationAction::SwingOffHand,
            4 => AnimationAction::CriticalHit,
            5 => AnimationAction::MagicCriticalHit,
            other => AnimationAction::Other(other),
        };
        Ok(vec![Directive::Emit(ClientEvent::EntityAnimation {
            entity_id: body.entity_id,
            action,
        })])
    }

    /// `minecraft:set_entity_data`.
    ///
    /// Only the shared entity flags byte at index
    /// [`METADATA_INDEX_SHARED_FLAGS`] is reported. Every other index at this
    /// protocol is claimed by more than one entity category with the same
    /// serializer, and this adapter has no id-to-category map to tell them
    /// apart — reporting one anyway would put an arrow's crit bit where a
    /// player's using-item bit belongs. The whole entry list is still decoded
    /// (and any unmodelled serializer still refused by name), so an
    /// unrecognised field fails loudly rather than desynchronising.
    fn handle_play_set_entity_data(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityMetadataPacket = adapter.decode_body(payload)?;
        let flags = body
            .metadata
            .0
            .iter()
            .find_map(|entry| match (entry.key, &entry.value) {
                (METADATA_INDEX_SHARED_FLAGS, MetadataValue::Byte(bits)) => Some(*bits as u8),
                _ => None,
            });
        if flags.is_none() {
            return Ok(Vec::new());
        }
        Ok(vec![Directive::Emit(ClientEvent::EntityMetadataUpdated {
            entity_id: body.entity_id,
            metadata: EntityMetadataUpdate {
                flags,
                ..EntityMetadataUpdate::default()
            },
        })])
    }

    /// `minecraft:set_entity_link`.
    fn handle_play_set_entity_link(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SetEntityLink = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityLeashed {
            entity_id: body.entity_id,
            holder_id: (body.vehicle_id != -1).then_some(body.vehicle_id),
        })])
    }

    /// `minecraft:set_passengers`.
    fn handle_play_set_passengers(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SetPassengers = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(
            ClientEvent::EntityPassengersChanged {
                vehicle_id: body.entity_id,
                passenger_ids: body.passengers,
            },
        )])
    }

    /// `minecraft:take_item_entity`.
    fn handle_play_take_item_entity(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: TakeItemEntity = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ItemPickup {
            item_entity_id: body.collected_entity_id,
            player_id: body.collector_entity_id,
            amount: body.pickup_item_count,
        })])
    }

    /// Resolves a validated effect id into the canonical event key.
    fn mob_effect_key(effect_id: MobEffectId) -> Result<ResourceKey, AdapterError> {
        let name = mob_effect_name_for(effect_id);
        name.parse()
            .map_err(|_| AdapterError::Decode(format!("effect id {name} is not a key")))
    }

    /// Validates this packet's zero-based built-in mob-effect id before any
    /// canonical registry lookup.
    fn modern_mob_effect_id(raw_id: i32) -> Result<MobEffectId, AdapterError> {
        MobEffectId::from_registry_id(raw_id)
            .ok_or_else(|| AdapterError::Decode(format!("unknown effect id {raw_id}")))
    }

    /// `minecraft:update_mob_effect`.
    ///
    /// The effect id is the **zero-based** mob-effect registry id, not the
    /// one-based legacy numbering the pre-1.20.5 eras send. It is validated
    /// before the shared registry table is indexed.
    fn handle_play_update_mob_effect(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: UpdateMobEffect = adapter.decode_body(payload)?;
        let effect_id = Self::modern_mob_effect_id(body.effect_id)?;
        let effect = Self::mob_effect_key(effect_id)?;
        Ok(vec![Directive::Emit(ClientEvent::MobEffectApplied {
            entity_id: body.entity_id,
            effect,
            amplifier: body.amplifier,
            duration_ticks: body.duration,
            ambient: body.flags & 0x01 != 0,
            visible: body.flags & 0x02 != 0,
            show_icon: body.flags & 0x04 != 0,
            blend: body.flags & 0x08 != 0,
        })])
    }

    /// `minecraft:remove_mob_effect`. Zero-based effect id, as
    /// [`Self::handle_play_update_mob_effect`] documents.
    fn handle_play_remove_mob_effect(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: RemoveMobEffect = adapter.decode_body_exact(payload)?;
        let effect_id = Self::modern_mob_effect_id(body.effect_id)?;
        let effect = Self::mob_effect_key(effect_id)?;
        Ok(vec![Directive::Emit(ClientEvent::MobEffectRemoved {
            entity_id: body.entity_id,
            effect,
        })])
    }

    /// `minecraft:block_update`. A packed position then a varint **flat
    /// block-state id** in this protocol's own id space, bridged to a
    /// canonical state through the same table the paletted chunk sections use.
    fn handle_play_block_update(
        adapter: &V774Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let pos: Position = Position::decode(&mut reader, adapter.ctx()).map_err(dec_err)?;
        let raw = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        let raw = u32::try_from(raw).map_err(|_| {
            AdapterError::Decode(format!("block_update state id {raw} is negative"))
        })?;
        let mut tally = FallbackTally::default();
        let state = adapter
            .current_shape()
            .canonical
            .resolve_or_air(raw, &mut tally);
        let pos = pos.0;
        world.set_block(pos.x, pos.y, pos.z, state);
        // Writing a state is what creates or removes a block entity; no packet
        // is involved.
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

    /// `minecraft:section_blocks_update` — many changes inside one section.
    ///
    /// Both the section coordinate and each record are bit-packed, so the
    /// whole packet is decoded arithmetically; see [`SectionBlocksUpdate`].
    /// Every record's state id goes through the same era-to-canonical table
    /// the single-block update uses, so a section update cannot end up with a
    /// different numbering from the column it lands in.
    fn handle_play_section_blocks_update(
        adapter: &V774Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SectionBlocksUpdate = adapter.decode_body_exact(payload)?;
        let shape = adapter.current_shape();
        let mut tally = FallbackTally::default();
        let mut changed = Vec::with_capacity(body.blocks.len());
        for (local, raw) in &body.blocks {
            let raw = u32::try_from(*raw).map_err(|_| {
                AdapterError::Decode(format!("section_blocks_update state id {raw} is negative"))
            })?;
            let state = shape.canonical.resolve_or_air(raw, &mut tally);
            let x = body.section_x * 16 + i32::from(local[0]);
            let y = body.section_y * 16 + i32::from(local[1]);
            let z = body.section_z * 16 + i32::from(local[2]);
            world.set_block(x, y, z, state);
            world.sync_block_entity(
                x,
                y,
                z,
                lodestone_data::block_states::StateId::new(state)
                    .and_then(block_entity_type)
                    .map(|kind| kind.raw()),
            );
            changed.push(*local);
        }
        if changed.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
            section: SectionPos::new(body.section_x, body.section_y, body.section_z),
            blocks: changed,
        })])
    }

    /// `minecraft:disconnect` (play state). Anonymous NBT here, where the
    /// login-state disconnect at this same protocol is still a JSON string.
    fn handle_play_disconnect(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: PlayDisconnect = adapter.decode_body(payload)?;
        Ok(vec![Directive::Disconnect(nbt_reason_text(&body.reason))])
    }

    /// `minecraft:set_health`.
    fn handle_play_set_health(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SetHealth = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::HealthChanged {
            health: body.health,
            food: body.food,
            saturation: body.food_saturation,
        })])
    }

    /// `minecraft:set_default_spawn_position`.
    ///
    /// The packet names its own level, where the era below's version carries a
    /// bare position and has to be told the dimension from the adapter's
    /// record of the last join. It also carries a pitch.
    fn handle_play_set_default_spawn_position(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SetDefaultSpawnPosition = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
            dimension: dimension_id(&body.location.dimension)?,
            pos: body.location.location.0,
            angle: body.yaw,
            pitch: body.pitch,
        })])
    }

    /// `minecraft:player_abilities` (clientbound).
    fn handle_play_player_abilities(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ClientboundAbilities = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::AbilitiesChanged {
            invulnerable: body.flags & ABILITY_INVULNERABLE != 0,
            flying: body.flags & ABILITY_FLYING != 0,
            can_fly: body.flags & ABILITY_CAN_FLY != 0,
            instabuild: body.flags & ABILITY_INSTABUILD != 0,
            flying_speed: body.flying_speed,
            walking_speed: body.walking_speed,
        })])
    }

    /// `minecraft:game_event`. Reason `3` is a game-mode change; the float
    /// carries the new mode's ordinal.
    fn handle_play_game_event(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: GameEvent = adapter.decode_body_exact(payload)?;
        if body.reason != 3 {
            return Ok(Vec::new());
        }
        let mode = game_mode(body.value as u8)?;
        Ok(vec![Directive::Emit(ClientEvent::GameModeChanged {
            game_mode: mode,
        })])
    }

    /// `minecraft:change_difficulty`.
    fn handle_play_change_difficulty(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ChangeDifficulty = adapter.decode_body_exact(payload)?;
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
        Ok(vec![Directive::Emit(ClientEvent::DifficultyChanged {
            difficulty,
            locked: body.difficulty_locked,
        })])
    }

    /// `minecraft:set_time`.
    ///
    /// The trailing day-time flag is this era's addition. It is reported
    /// through the time-of-day value rather than a separate field: a frozen
    /// day cycle keeps sending the same `time`, which is what a consumer
    /// observes anyway.
    fn handle_play_set_time(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SetTime = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TimeChanged {
            world_age: body.age,
            time_of_day: body.time,
        })])
    }

    /// `minecraft:tab_list`. Both components are anonymous NBT here, not the
    /// JSON strings the older eras send.
    fn handle_play_tab_list(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: TabList = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TabListChanged {
            header: Text::from_nbt(&body.header.0),
            footer: Text::from_nbt(&body.footer.0),
        })])
    }

    /// `minecraft:player_info_update` in its action-bitmask form.
    ///
    /// Two actions here that the era below does not have — the hat flag and
    /// the list-order priority — and both reach the canonical entry, so the
    /// tab list can be sorted the way the server asked.
    fn handle_play_player_info_update(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: PlayerInfoUpdate = adapter.decode_body_exact(payload)?;
        let mut updated = Vec::with_capacity(body.entries.len());
        for entry in body.entries {
            let game_mode = entry
                .game_mode
                .map(|raw| {
                    game_mode(u8::try_from(raw).map_err(|_| {
                        AdapterError::Decode(format!(
                            "player_info_update game mode {raw} out of range"
                        ))
                    })?)
                })
                .transpose()?;
            updated.push(PlayerListEntry {
                uuid: Some(entry.uuid),
                name: entry.name,
                game_mode,
                latency: entry.latency,
                display_name: entry.display_name,
                listed: entry.listed,
                properties: entry.properties.map(|properties| {
                    properties
                        .into_iter()
                        .map(|property| ProfileProperty {
                            name: property.name,
                            value: property.value,
                            signature: property.signature,
                        })
                        .collect()
                }),
                // The receiving half of secure chat: dropping this is what
                // would make every `player_chat` permanently unverifiable.
                chat_session: entry.chat_session.map(|session| ChatSessionInfo {
                    session_id: session.session_id,
                    public_key: session.public_key,
                    expires_at: session.expires_at,
                }),
                list_order: entry.list_order,
                hat_visible: entry.show_hat,
            });
        }
        if updated.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![Directive::Emit(ClientEvent::PlayerListUpdate {
            entries: updated,
        })])
    }

    /// `minecraft:player_info_remove`.
    fn handle_play_player_info_remove(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: PlayerInfoRemove = adapter.decode_body_exact(payload)?;
        if body.uuids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![Directive::Emit(ClientEvent::PlayerListRemove {
            profile_ids: body.uuids,
        })])
    }

    /// `minecraft:set_held_slot`. A varint here, where the era below sends a
    /// single byte.
    fn handle_play_set_held_slot(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SetHeldSlot = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::HeldSlotChanged {
            slot: body.slot,
        })])
    }

    /// `minecraft:container_close`. A varint window id here, where the era
    /// below sends an unsigned byte.
    fn handle_play_container_close(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ContainerClose = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ScreenClosed {
            window_id: body.window_id,
        })])
    }

    /// `minecraft:container_set_data`.
    fn handle_play_container_set_data(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ContainerSetData = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ContainerData {
            window_id: body.window_id,
            property: i32::from(body.property),
            value: i32::from(body.value),
        })])
    }

    /// `minecraft:set_experience`.
    fn handle_play_set_experience(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SetExperience = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ExperienceChanged {
            progress: body.experience_bar,
            level: body.level,
            total: body.total_experience,
        })])
    }

    /// `minecraft:move_vehicle`.
    fn handle_play_move_vehicle(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let x = reader.f64().map_err(dec_err)?;
        let y = reader.f64().map_err(dec_err)?;
        let z = reader.f64().map_err(dec_err)?;
        let yaw = reader.f32().map_err(dec_err)?;
        let pitch = reader.f32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::VehicleMoved {
            pos: Vec3::new(x, y, z),
            yaw,
            pitch,
        })])
    }

    /// `minecraft:set_camera`.
    fn handle_play_set_camera(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let entity_id = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::CameraSet { entity_id })])
    }

    /// `minecraft:set_chunk_cache_center`.
    fn handle_play_set_chunk_cache_center(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let x = reader.var_i32().map_err(dec_err)?;
        let z = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(
            ClientEvent::ChunkCacheCenterChanged { x, z },
        )])
    }

    /// `minecraft:set_chunk_cache_radius`.
    fn handle_play_set_chunk_cache_radius(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let radius = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(
            ClientEvent::ChunkCacheRadiusChanged { radius },
        )])
    }

    /// `minecraft:set_simulation_distance`.
    fn handle_play_set_simulation_distance(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let distance = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(
            ClientEvent::SimulationDistanceChanged { distance },
        )])
    }

    /// `minecraft:open_sign_editor`. Signs are two-sided throughout this era,
    /// so the face the server opened is on the wire rather than assumed.
    fn handle_play_open_sign_editor(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: OpenSignEditor = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::SignEditorOpened {
            pos: body.location.0,
            is_front_text: body.is_front_text,
        })])
    }

    /// `minecraft:set_title_text`.
    fn handle_play_set_title_text(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let text = decode_single_nbt_text(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TitleText { text })])
    }

    /// `minecraft:set_subtitle_text`.
    fn handle_play_set_subtitle_text(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let text = decode_single_nbt_text(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::SubtitleText { text })])
    }

    /// `minecraft:set_action_bar_text`. Reported as a game-info chat line, the
    /// same surface `system_chat`'s action-bar flag selects.
    fn handle_play_set_action_bar_text(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let text = decode_single_nbt_text(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::Chat {
            text,
            kind: ChatKind::GameInfo,
            sender: None,
            ack: None,
        })])
    }

    /// `minecraft:set_titles_animation`.
    fn handle_play_set_titles_animation(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let fade_in = reader.i32().map_err(dec_err)?;
        let stay = reader.i32().map_err(dec_err)?;
        let fade_out = reader.i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::TitlesAnimation {
            fade_in,
            stay,
            fade_out,
        })])
    }

    /// `minecraft:clear_titles`.
    fn handle_play_clear_titles(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let reset_times = reader.bool().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::TitlesCleared {
            reset_times,
        })])
    }

    /// `minecraft:ticking_state`.
    fn handle_play_ticking_state(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: TickingState = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TickingStateChanged {
            tick_rate: body.tick_rate,
            frozen: body.is_frozen,
        })])
    }

    /// `minecraft:ticking_step`.
    fn handle_play_ticking_step(
        adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: TickingStep = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TickingStepped {
            tick_steps: body.tick_steps,
        })])
    }

    /// `minecraft:bundle_delimiter`. Carries no body: it brackets a run of
    /// packets the client must apply in the same frame.
    fn handle_play_bundle_delimiter(
        _adapter: &V774Adapter,
        _world: &mut dyn WorldSink,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(vec![Directive::BundleDelimiter])
    }

    /// Builds this protocol's `play` clientbound dispatch table once, from
    /// [`CLIENTBOUND`], [`IGNORED`] and that protocol's own `ENTRIES`.
    ///
    /// # Panics
    ///
    /// Panics if construction fails: a name in [`CLIENTBOUND`] or [`IGNORED`]
    /// that does not match `ENTRIES`, a duplicate handler, or an `ENTRIES` id
    /// with neither a handler nor an ignore entry. Every one of those is a
    /// static-table defect introduced at edit time, not a runtime condition,
    /// so failing loudly the first time this protocol is used is correct.
    fn play_dispatch_table(&self) -> &'static lodestone_core::dispatch::Table<'static, PlayHandler>
    {
        static TABLES: [std::sync::OnceLock<
            lodestone_core::dispatch::Table<'static, PlayHandler>,
        >; 1] = [std::sync::OnceLock::new()];
        TABLES[0].get_or_init(|| {
            lodestone_core::dispatch::Table::build(
                self.protocol,
                self.ids().play_clientbound_entries,
                CLIENTBOUND,
                IGNORED,
            )
            .unwrap_or_else(|err| {
                panic!(
                    "v1-21-11 play dispatch table for protocol {} must build: every clientbound \
                     ENTRIES id needs either a bound handler or an IGNORED reason covering this \
                     protocol -- {err}",
                    self.protocol
                )
            })
        })
    }

    /// Handles a clientbound packet while in the play state.
    ///
    /// `Table::build`'s construction-time check guarantees every id this
    /// protocol's `ENTRIES` declares has a handler or a named ignore reason.
    /// `packet_id` itself, though, arrives straight off the wire, so an id the
    /// table has never heard of is a different case: ignored rather than
    /// panicked on, because a malformed byte from the network must never crash
    /// the client.
    fn handle_play(
        &self,
        world: &mut dyn WorldSink,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        match self.play_dispatch_table().get(packet_id) {
            Some(handler) => handler(self, world, payload),
            None => Ok(Vec::new()),
        }
    }
}

/// Every clientbound play packet this era decodes, by name.
static CLIENTBOUND: &[(&str, lodestone_core::dispatch::Handler<PlayHandler>)] = &[
    (
        "minecraft:login",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_login,
        ),
    ),
    (
        "minecraft:respawn",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_respawn,
        ),
    ),
    (
        "minecraft:level_chunk_with_light",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_level_chunk_with_light,
        ),
    ),
    (
        "minecraft:light_update",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_light_update,
        ),
    ),
    (
        "minecraft:forget_level_chunk",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_forget_level_chunk,
        ),
    ),
    (
        "minecraft:keep_alive",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_keep_alive,
        ),
    ),
    (
        "minecraft:ping",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_ping,
        ),
    ),
    (
        "minecraft:system_chat",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_system_chat,
        ),
    ),
    (
        "minecraft:disguised_chat",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_disguised_chat,
        ),
    ),
    (
        "minecraft:player_chat",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_player_chat,
        ),
    ),
    (
        "minecraft:delete_chat",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_delete_chat,
        ),
    ),
    (
        "minecraft:player_position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_player_position,
        ),
    ),
    (
        "minecraft:player_rotation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_player_rotation,
        ),
    ),
    (
        "minecraft:chunk_batch_start",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_chunk_batch_start,
        ),
    ),
    (
        "minecraft:chunk_batch_finished",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_chunk_batch_finished,
        ),
    ),
    (
        "minecraft:start_configuration",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_start_configuration,
        ),
    ),
    (
        "minecraft:add_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_add_entity,
        ),
    ),
    (
        "minecraft:move_entity_pos",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_move_entity_pos,
        ),
    ),
    (
        "minecraft:move_entity_pos_rot",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_move_entity_pos_rot,
        ),
    ),
    (
        "minecraft:move_entity_rot",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_move_entity_rot,
        ),
    ),
    (
        "minecraft:teleport_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_teleport_entity,
        ),
    ),
    (
        "minecraft:entity_position_sync",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_entity_position_sync,
        ),
    ),
    (
        "minecraft:set_entity_motion",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_entity_motion,
        ),
    ),
    (
        "minecraft:rotate_head",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_rotate_head,
        ),
    ),
    (
        "minecraft:remove_entities",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_remove_entities,
        ),
    ),
    (
        "minecraft:entity_event",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_entity_event,
        ),
    ),
    (
        "minecraft:animate",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_animate,
        ),
    ),
    (
        "minecraft:set_entity_data",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_entity_data,
        ),
    ),
    (
        "minecraft:set_entity_link",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_entity_link,
        ),
    ),
    (
        "minecraft:set_passengers",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_passengers,
        ),
    ),
    (
        "minecraft:take_item_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_take_item_entity,
        ),
    ),
    (
        "minecraft:update_mob_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_update_mob_effect,
        ),
    ),
    (
        "minecraft:remove_mob_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_remove_mob_effect,
        ),
    ),
    (
        "minecraft:block_update",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_block_update,
        ),
    ),
    (
        "minecraft:section_blocks_update",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_section_blocks_update,
        ),
    ),
    (
        "minecraft:disconnect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_disconnect,
        ),
    ),
    (
        "minecraft:set_health",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_health,
        ),
    ),
    (
        "minecraft:set_default_spawn_position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_default_spawn_position,
        ),
    ),
    (
        "minecraft:player_abilities",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_player_abilities,
        ),
    ),
    (
        "minecraft:game_event",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_game_event,
        ),
    ),
    (
        "minecraft:change_difficulty",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_change_difficulty,
        ),
    ),
    (
        "minecraft:set_time",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_time,
        ),
    ),
    (
        "minecraft:tab_list",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_tab_list,
        ),
    ),
    (
        "minecraft:player_info_update",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_player_info_update,
        ),
    ),
    (
        "minecraft:player_info_remove",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_player_info_remove,
        ),
    ),
    (
        "minecraft:set_held_slot",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_held_slot,
        ),
    ),
    (
        "minecraft:container_close",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_container_close,
        ),
    ),
    (
        "minecraft:container_set_data",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_container_set_data,
        ),
    ),
    (
        "minecraft:set_experience",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_experience,
        ),
    ),
    (
        "minecraft:move_vehicle",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_move_vehicle,
        ),
    ),
    (
        "minecraft:set_camera",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_camera,
        ),
    ),
    (
        "minecraft:set_chunk_cache_center",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_chunk_cache_center,
        ),
    ),
    (
        "minecraft:set_chunk_cache_radius",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_chunk_cache_radius,
        ),
    ),
    (
        "minecraft:set_simulation_distance",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_simulation_distance,
        ),
    ),
    (
        "minecraft:open_sign_editor",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_open_sign_editor,
        ),
    ),
    (
        "minecraft:set_title_text",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_title_text,
        ),
    ),
    (
        "minecraft:set_subtitle_text",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_subtitle_text,
        ),
    ),
    (
        "minecraft:set_action_bar_text",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_action_bar_text,
        ),
    ),
    (
        "minecraft:set_titles_animation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_set_titles_animation,
        ),
    ),
    (
        "minecraft:clear_titles",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_clear_titles,
        ),
    ),
    (
        "minecraft:ticking_state",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_ticking_state,
        ),
    ),
    (
        "minecraft:ticking_step",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_ticking_step,
        ),
    ),
    (
        "minecraft:bundle_delimiter",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V774Adapter::handle_play_bundle_delimiter,
        ),
    ),
];

/// Every clientbound play packet this era deliberately does not decode, with
/// the reason.
///
/// The list is not documentation: `Table::build` requires every id in this
/// protocol's `ENTRIES` to appear either here or in [`CLIENTBOUND`], so a
/// packet that is neither handled nor named here fails table construction the
/// first time this protocol is used.
static IGNORED: &[lodestone_core::dispatch::IGNORED] = &[
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:award_stats",
        "the statistics screen has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:block_changed_ack",
        "block-prediction acknowledgement is the server confirming a sequence this client \
         already applied optimistically",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:block_destruction",
        "the crack overlay on another player's mining progress is cosmetic and has no world \
         effect",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:block_entity_data",
        "block-entity payloads arrive with their column; an incremental update needs a \
         per-type NBT model this era does not carry",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:block_event",
        "note-block, piston and chest-lid animations are cosmetic",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:boss_event",
        "the boss bar has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:chunks_biomes",
        "a biome-only column update needs a paletted-biome patch path this era does not carry",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:command_suggestions",
        "tab-completion results have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:commands",
        "the command tree is only needed to drive client-side completion",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:container_set_content",
        "container contents need the item-component decoder to produce canonical stacks, \
         which is modelled but not yet bridged to a canonical item id",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:container_set_slot",
        "one slot of the same container model container_set_content needs",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:cookie_request",
        "server cookies are persisted only by a client that implements transfers",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:cooldown",
        "the item-cooldown overlay is cosmetic",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:custom_chat_completions",
        "chat-completion hints have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:custom_payload",
        "plugin-channel payloads are opaque to this client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:damage_event",
        "the damage-source detail is cosmetic; the health change arrives separately",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:debug/block_value",
        "the server-side debug subscription channels are only sent to a subscribed client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:debug/chunk_value",
        "the server-side debug subscription channels are only sent to a subscribed client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:debug/entity_value",
        "the server-side debug subscription channels are only sent to a subscribed client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:debug/event",
        "the server-side debug subscription channels are only sent to a subscribed client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:debug_sample",
        "tick-timing samples are only sent to a client that asked for them",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:explode",
        "the blast's block changes arrive as their own updates; the particle and knockback \
         detail is cosmetic",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:game_test_highlight_pos",
        "the game-test harness overlay has no surface outside a test world",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:mount_screen_open",
        "the horse-inventory screen has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:hurt_animation",
        "the damage tilt is cosmetic",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:initialize_border",
        "the world border has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:level_event",
        "world sound and particle events are cosmetic",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:level_particles",
        "particles are cosmetic",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:map_item_data",
        "filled-map rendering has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:merchant_offers",
        "the trading screen has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:move_minecart_along_track",
        "the interpolated minecart path is a rendering refinement over the plain entity moves \
         that also arrive",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:open_book",
        "the book screen has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:open_screen",
        "container screens need the item model container_set_content needs",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:pong_response",
        "the play-state pong answers a client ping this adapter does not send",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:place_ghost_recipe",
        "the recipe-book ghost overlay has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:player_combat_end",
        "the combat tracker only drives the death screen's damage summary",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:player_combat_enter",
        "the combat tracker only drives the death screen's damage summary",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:player_combat_kill",
        "the death screen has no surface for this era; the respawn is driven by the health \
         change and the client command",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:player_look_at",
        "forcing the local player's camera onto a target has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:recipe_book_add",
        "the recipe book has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:recipe_book_remove",
        "the recipe book has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:recipe_book_settings",
        "the recipe book has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:reset_score",
        "scoreboards have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:resource_pack_pop",
        "server resource packs are not applied by this client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:resource_pack_push",
        "server resource packs are not applied by this client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:select_advancements_tab",
        "the advancements screen has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:server_data",
        "the server MOTD and icon push has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_border_center",
        "the world border has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_border_lerp_size",
        "the world border has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_border_size",
        "the world border has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_border_warning_delay",
        "the world border has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_border_warning_distance",
        "the world border has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_cursor_item",
        "the held cursor stack needs the item model container_set_content needs",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_display_objective",
        "scoreboards have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_equipment",
        "another entity's worn and held items need the item model container_set_content needs",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_objective",
        "scoreboards have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_player_inventory",
        "the local player's inventory needs the item model container_set_content needs",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_player_team",
        "teams have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_score",
        "scoreboards have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:sound_entity",
        "sound playback is cosmetic",
    ),
    lodestone_core::dispatch::IGNORED::new("minecraft:sound", "sound playback is cosmetic"),
    lodestone_core::dispatch::IGNORED::new("minecraft:stop_sound", "sound playback is cosmetic"),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:store_cookie",
        "server cookies are persisted only by a client that implements transfers",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:tag_query",
        "the NBT query response answers a debug request this adapter does not send",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:test_instance_block_status",
        "the game-test harness has no surface outside a test world",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:transfer",
        "server-to-server transfer is a connection-level capability this client does not \
         implement",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:update_advancements",
        "the advancements screen has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:update_attributes",
        "entity attribute modifiers need an attribute model this era does not carry",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:update_recipes",
        "the recipe registry only drives the recipe book and client-side crafting preview",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:update_tags",
        "block and item tags only drive client-side prediction this client does not do",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:projectile_power",
        "the crossbow-projectile power hint is a rendering refinement",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:custom_report_details",
        "crash-report metadata is only used by a client that files reports",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:server_links",
        "the server-links menu has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:waypoint",
        "the locator-bar waypoint markers have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:clear_dialog",
        "server-driven dialog screens have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:show_dialog",
        "server-driven dialog screens have no surface for this era",
    ),
];

impl VersionAdapter for V774Adapter {
    fn protocol_version(&self) -> i32 {
        self.protocol
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["1.21.11"]
    }

    fn supports(&self, protocol: i32) -> bool {
        PROTOCOLS.contains(&protocol)
    }

    fn begin_login(
        &self,
        profile: &LoginProfile,
        server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        let handshake = Intention {
            protocol_version: self.protocol,
            server_host: server.host.clone(),
            server_port: server.port,
            next_state: NEXT_STATE_LOGIN,
        };
        // The profile UUID is **not** optional at this protocol. The eras
        // below write a presence boolean and then, usually, nothing; here the
        // sixteen bytes are read unconditionally, so an offline-mode client
        // still has to put a uuid on the wire. The server ignores its value in
        // offline mode but not its length.
        let login_start = LoginStart {
            username: profile.username.clone(),
            uuid: profile.uuid,
        };
        Ok(vec![
            self.send(self.ids().handshake_intention, &handshake)?,
            Directive::SetState(ConnectionState::Login),
            self.send(self.ids().login_start, &login_start)?,
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
        if state != ConnectionState::Play {
            return Ok(None);
        }
        match action {
            ClientAction::KeepAliveResponse { id } => {
                let body = KeepAliveResponse { id: *id };
                Ok(Some((self.ids().keep_alive, self.encode_body(&body)?)))
            }
            // Every serverbound chat *message* carries a last-seen
            // acknowledgement tail, so sending one is also what drains the
            // server's pending list. The trailing checksum is `0`: an unsigned
            // session has no key to compute one with, and `0` is the value a
            // server accepts as "not computed".
            ClientAction::SendChat { text } => {
                let mut body = ChatMessage::unsigned(text.clone());
                body.last_seen_offset = self.take_pending_ack();
                Ok(Some((self.ids().chat, self.encode_body(&body)?)))
            }
            // A command does **not** carry that tail: the unsigned command
            // packet is one string and nothing else, and the signed form is a
            // different packet. So the pending count is deliberately left
            // standing here rather than silently dropped — the next chat
            // message or acknowledgement drains it.
            ClientAction::SendCommand { command } => {
                let body = ChatCommand {
                    command: command.clone(),
                };
                Ok(Some((self.ids().chat_command, self.encode_body(&body)?)))
            }
            // The standalone drain. Without it, a client that reads chat and
            // never writes it grows the server's pending list until the server
            // disconnects it.
            ClientAction::ChatAck { offset } => {
                let combined = offset.saturating_add(self.take_pending_ack());
                let body = MessageAcknowledgement { count: combined };
                Ok(Some((self.ids().chat_ack, self.encode_body(&body)?)))
            }
            // The collision bit reaches the wire here, where the era below has
            // nowhere to put it.
            ClientAction::Move {
                pos,
                rotation,
                on_ground,
                horizontal_collision,
            } => self.select_move_packet(*pos, *rotation, *on_ground, *horizontal_collision),
            ClientAction::SwingArm { hand } => {
                let body = Swing {
                    hand: hand_ordinal(*hand),
                };
                Ok(Some((self.ids().swing, self.encode_body(&body)?)))
            }

            // Block breaking rides on player-action statuses 0/1/2, carrying
            // the block-prediction sequence the server echoes back.
            ClientAction::BlockAction {
                action,
                pos,
                face,
                sequence,
            } => {
                let status = match action {
                    BlockActionKind::StartDestroy => 0,
                    BlockActionKind::AbortDestroy => 1,
                    BlockActionKind::StopDestroy => 2,
                };
                let body = PlayerAction {
                    status,
                    location: Position(*pos),
                    face: face_ordinal(*face) as i8,
                    sequence: *sequence,
                };
                Ok(Some((self.ids().player_action, self.encode_body(&body)?)))
            }
            // Dropping, releasing and the off-hand swap ride on the same
            // packet's statuses 3 through 6. None of them predicts a block
            // change, so there is nothing for the server to acknowledge and
            // the sequence is zero.
            ClientAction::DropSelectedItemStack => Ok(Some((
                self.ids().player_action,
                self.encode_body(&PlayerAction {
                    status: 3,
                    location: Position::new(0, 0, 0),
                    face: 0,
                    sequence: 0,
                })?,
            ))),
            ClientAction::DropSelectedItem => Ok(Some((
                self.ids().player_action,
                self.encode_body(&PlayerAction {
                    status: 4,
                    location: Position::new(0, 0, 0),
                    face: 0,
                    sequence: 0,
                })?,
            ))),
            ClientAction::ReleaseUseItem => Ok(Some((
                self.ids().player_action,
                self.encode_body(&PlayerAction {
                    status: 5,
                    location: Position::new(0, 0, 0),
                    face: 0,
                    sequence: 0,
                })?,
            ))),
            ClientAction::SwapItemWithOffhand => Ok(Some((
                self.ids().player_action,
                self.encode_body(&PlayerAction {
                    status: 6,
                    location: Position::new(0, 0, 0),
                    face: 0,
                    sequence: 0,
                })?,
            ))),

            // The world-border flag between the inside-block flag and the
            // sequence is this era's addition. A client that is not aiming
            // through the border reports `false`, which is why omitting the
            // field passes every ordinary test and then shifts the sequence.
            ClientAction::UseItemOn {
                hand,
                pos,
                face,
                cursor,
                inside_block,
                sequence,
            } => {
                let body = UseItemOn {
                    hand: hand_ordinal(*hand),
                    location: Position(*pos),
                    direction: face_ordinal(*face),
                    cursor_x: cursor.x,
                    cursor_y: cursor.y,
                    cursor_z: cursor.z,
                    inside_block: *inside_block,
                    world_border_hit: false,
                    sequence: *sequence,
                };
                Ok(Some((self.ids().use_item_on, self.encode_body(&body)?)))
            }
            // The look direction is on this era's packet, so the model's
            // rotation reaches the wire instead of being dropped.
            ClientAction::UseItem {
                hand,
                rotation,
                sequence,
            } => {
                let body = UseItem {
                    hand: hand_ordinal(*hand),
                    sequence: *sequence,
                    yaw: rotation.yaw,
                    pitch: rotation.pitch,
                };
                Ok(Some((self.ids().use_item, self.encode_body(&body)?)))
            }

            // Each interaction kind is a distinct wire shape behind one packet
            // id, selected by the `mouse` value.
            ClientAction::InteractEntity {
                entity_id,
                interaction,
                sneaking,
            } => match interaction {
                EntityInteraction::Attack => {
                    let body = Interact {
                        target: *entity_id,
                        mouse: 1,
                        sneaking: *sneaking,
                    };
                    Ok(Some((self.ids().interact, self.encode_body(&body)?)))
                }
                EntityInteraction::Interact { hand } => {
                    let body = InteractHand {
                        target: *entity_id,
                        mouse: 0,
                        hand: hand_ordinal(*hand),
                        sneaking: *sneaking,
                    };
                    Ok(Some((self.ids().interact, self.encode_body(&body)?)))
                }
                EntityInteraction::InteractAt { hand, target } => {
                    let body = InteractAt {
                        target: *entity_id,
                        mouse: 2,
                        x: target.x as f32,
                        y: target.y as f32,
                        z: target.z as f32,
                        hand: hand_ordinal(*hand),
                        sneaking: *sneaking,
                    };
                    Ok(Some((self.ids().interact, self.encode_body(&body)?)))
                }
            },

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
                let body = PlayerCommandPacket {
                    entity_id: *entity_id,
                    action_id,
                    jump_boost,
                };
                Ok(Some((self.ids().player_command, self.encode_body(&body)?)))
            }
            // Continuous movement input is its own packet at this protocol —
            // the sneak and sprint *edges* still go through the player-command
            // channel above, and the server needs both.
            ClientAction::SetPlayerInput(input) => {
                let body = PlayerInputPacket {
                    inputs: player_input_bits(*input),
                };
                Ok(Some((self.ids().player_input, self.encode_body(&body)?)))
            }

            ClientAction::ContainerClose { window_id } => {
                let body = ServerboundContainerClose {
                    window_id: *window_id,
                };
                Ok(Some((self.ids().container_close, self.encode_body(&body)?)))
            }
            ClientAction::SetCarriedItem { slot } => {
                let body = SetCarriedItem { slot: *slot as i16 };
                Ok(Some((self.ids().set_carried_item, self.encode_body(&body)?)))
            }
            ClientAction::SetCreativeModeSlot { slot, item } => {
                if item.is_some() {
                    return Err(AdapterError::Unsupported(
                        "this era's SetCreativeModeSlot with an item requires a \
                         ResourceKey -> numeric item-id registry for protocol 774, which does \
                         not exist yet"
                            .to_owned(),
                    ));
                }
                let body = SetCreativeModeSlot {
                    slot: *slot as i16,
                    item: Slot::Empty,
                };
                Ok(Some((
                    self.ids().set_creative_mode_slot,
                    self.encode_body(&body)?,
                )))
            }
            // The click shape is this era's exactly — a state id, the client's
            // own view of every slot the click changed, and the resulting
            // cursor stack — so a click that moves nothing but empty slots
            // encodes faithfully. A click carrying a real stack needs both a
            // numeric item id and the *hashed* component form this era's
            // serverbound stacks use, and is refused rather than guessed: a
            // wrong hash is rejected by the server as a desync, and a wrong id
            // is accepted and applied.
            ClientAction::ContainerClick {
                window_id,
                state_id,
                slot,
                button,
                click_type,
                changed_slots,
                carried_item,
            } => {
                if carried_item.is_some() || changed_slots.iter().any(|entry| entry.item.is_some())
                {
                    return Err(AdapterError::Unsupported(
                        "this era's ContainerClick with a non-empty stack requires a \
                         ResourceKey -> numeric item-id registry for protocol 774 plus the \
                         hashed component form its serverbound stacks use, neither of which \
                         exists yet"
                            .to_owned(),
                    ));
                }
                let body = ContainerClick {
                    window_id: *window_id,
                    state_id: *state_id,
                    slot: i16::try_from(*slot).map_err(|_| {
                        AdapterError::Encode(format!("container slot {slot} overflows i16"))
                    })?,
                    button: i8::try_from(*button).map_err(|_| {
                        AdapterError::Encode(format!("click button {button} overflows i8"))
                    })?,
                    mode: click_mode_value(*click_type),
                    changed_slots: changed_slots
                        .iter()
                        .map(|entry| {
                            Ok(ChangedSlot {
                                location: i16::try_from(entry.slot).map_err(|_| {
                                    AdapterError::Encode(format!(
                                        "changed slot {} overflows i16",
                                        entry.slot
                                    ))
                                })?,
                                item: HashedStack,
                            })
                        })
                        .collect::<Result<Vec<_>, AdapterError>>()?,
                    cursor_item: HashedStack,
                };
                Ok(Some((self.ids().container_click, self.encode_body(&body)?)))
            }
            ClientAction::ContainerButtonClick {
                window_id,
                button_id,
            } => {
                let body = ContainerButtonClick {
                    window_id: *window_id,
                    button_id: *button_id,
                };
                Ok(Some((
                    self.ids().container_button_click,
                    self.encode_body(&body)?,
                )))
            }

            // The particle-status field reaches the wire here; the era below
            // has to drop it.
            ClientAction::SetClientSettings(settings) => {
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
                    locale: locale.clone(),
                    view_distance: *view_distance,
                    chat_flags: chat_mode_value(*chat_mode),
                    chat_colors: *chat_colors,
                    skin_parts: skin_parts_bits(*skin_parts),
                    main_hand: main_hand_value(*main_hand),
                    text_filtering: *text_filtering,
                    allow_server_listing: *allow_server_listing,
                    particle_status: particle_status_value(*particle_status),
                };
                Ok(Some((self.ids().client_information, self.encode_body(&body)?)))
            }
            ClientAction::SendBrand { brand } => {
                let body = BrandPayload {
                    channel: "minecraft:brand".to_owned(),
                    brand: brand.clone(),
                };
                Ok(Some((self.ids().custom_payload, self.encode_body(&body)?)))
            }
            ClientAction::SetFlying { flying } => {
                let body = PlayerAbilities {
                    flags: if *flying { ABILITY_FLYING } else { 0 },
                };
                Ok(Some((self.ids().player_abilities, self.encode_body(&body)?)))
            }
            // The reply names the pack by uuid, because a server may have
            // several applied at once and pushes or removes them individually.
            ClientAction::ResourcePackResponse { id, response } => {
                let result = match response {
                    ResourcePackResponseKind::SuccessfullyLoaded => 0,
                    ResourcePackResponseKind::Declined => 1,
                    ResourcePackResponseKind::FailedDownload => 2,
                    ResourcePackResponseKind::Accepted => 3,
                    ResourcePackResponseKind::Downloaded => 4,
                    ResourcePackResponseKind::InvalidUrl => 5,
                    ResourcePackResponseKind::FailedReload => 6,
                    ResourcePackResponseKind::Discarded => 7,
                };
                let body = ResourcePackReceive { uuid: *id, result };
                Ok(Some((self.ids().resource_pack, self.encode_body(&body)?)))
            }
            ClientAction::PongResponse { id } => {
                let mut writer = Writer::default();
                writer.i32(*id);
                Ok(Some((self.ids().pong, writer.into_vec())))
            }
            ClientAction::Respawn => {
                let body = ClientCommand { action: 0 };
                Ok(Some((self.ids().client_command, self.encode_body(&body)?)))
            }
            ClientAction::TeleportToEntity { target } => {
                let body = TeleportToEntity { target: *target };
                Ok(Some((
                    self.ids().teleport_to_entity,
                    self.encode_body(&body)?,
                )))
            }
            ClientAction::CommandSuggestion { id, command } => {
                let mut writer = Writer::default();
                writer.var_i32(*id);
                writer.string(command);
                Ok(Some((self.ids().command_suggestion, writer.into_vec())))
            }
            ClientAction::SetRecipeBookSettings {
                book_type,
                open,
                filtering,
            } => {
                let body = RecipeBookChangeSettings {
                    book_id: recipe_book_type_to_ordinal(*book_type),
                    book_open: *open,
                    filter_active: *filtering,
                };
                Ok(Some((
                    self.ids().recipe_book_change_settings,
                    self.encode_body(&body)?,
                )))
            }
            // Both of these are genuinely on this era's wire and genuinely
            // absent from the era below. A server that never receives
            // `player_loaded` holds the player in a loading state; one that
            // never receives a tick end batches the client's input a tick
            // late.
            ClientAction::PlayerLoaded => Ok(Some((
                self.ids().player_loaded,
                self.encode_body(&PlayerLoaded)?,
            ))),
            ClientAction::EndClientTick => Ok(Some((
                self.ids().client_tick_end,
                self.encode_body(&ClientTickEnd)?,
            ))),

            // Genuinely absent at this protocol.
            ClientAction::Stab => Err(AdapterError::Unsupported(
                "this era has no dedicated off-hand attack packet".to_owned(),
            )),
            ClientAction::SelectBundleItem { .. } => Err(AdapterError::Unsupported(
                "this era's bundle-selection packet is not modelled; bundles need the item \
                 component decoder bridged to a canonical item id first"
                    .to_owned(),
            )),
            // The teleport-to-entity packet needs the target's uuid, which
            // this action does not carry; the id-to-uuid map lives above this
            // adapter.
            ClientAction::SpectatorAction { .. } => Err(AdapterError::Unsupported(
                "this era's teleport-to-entity packet needs a target uuid; SpectatorAction \
                 carries only a network entity id (use TeleportToEntity, which already carries \
                 the uuid)"
                    .to_owned(),
            )),
            // Both recipe packets identify a recipe by a namespaced string,
            // where the model carries a display index and this adapter holds
            // no recipe registry to bridge the two.
            ClientAction::RecipeBookSeenRecipe { .. } | ClientAction::PlaceRecipe { .. } => {
                Err(AdapterError::Unsupported(
                    "this era's recipe-book packets identify a recipe by a namespaced string id; \
                     the model's display index has no registry to resolve into one"
                        .to_owned(),
                ))
            }
            ClientAction::SetBeaconEffects { .. } => Err(AdapterError::Unsupported(
                "this era's beacon packet names effects by registry id, and no protocol 774 \
                 mob-effect id table exists to resolve one"
                    .to_owned(),
            )),

            _ => Ok(None),
        }
    }
}

/// Constructs an adapter speaking [`PROTOCOL`].
#[must_use]
pub fn adapter() -> V774Adapter {
    V774Adapter::new()
}

/// Constructs an adapter for one of [`PROTOCOLS`].
///
/// # Panics
///
/// Panics for a protocol outside [`PROTOCOLS`] — see [`ids_for`].
#[must_use]
pub fn adapter_for(protocol: i32) -> V774Adapter {
    assert!(
        PROTOCOLS.contains(&protocol),
        "protocol {protocol} is outside this family's PROTOCOLS ({PROTOCOLS:?}); \
         callers must test membership before constructing"
    );
    V774Adapter::for_protocol(protocol)
}

#[cfg(test)]
mod movement_tests {
    use super::*;

    #[test]
    fn poisoned_movement_state_is_recovered() {
        let adapter = V774Adapter::new();
        let guard = adapter.movement.lock().expect("fresh movement state");
        let state = recover_movement_state(Err(PoisonError::new(guard)));
        drop(state);

        assert_eq!(
            adapter
                .select_move_packet(Vec3::new(1.0, 0.0, 0.0), Rotation::default(), false, false)
                .expect("poisoned state is recovered")
                .map(|(id, _)| id),
            Some(adapter.ids().move_player_pos)
        );
    }

    /// The collision bit is the one thing this era's movement byte carries
    /// that the era below cannot express, so it gets its own gate: two sends
    /// that differ *only* in the collision flag must differ on the wire.
    ///
    /// The expected bytes are the packet's own layout — three `f64` then the
    /// flag byte — not a re-read of the encoder, and the flag values `0x01`
    /// and `0x03` are read off the bit assignment rather than out of
    /// `movement_flags`.
    #[test]
    fn the_collision_bit_reaches_the_wire() {
        let adapter = V774Adapter::new();
        let pos = Vec3::new(8.0, 65.0, -3.0);
        let no_collision = adapter
            .select_move_packet(pos, Rotation::default(), true, false)
            .expect("encodes")
            .expect("a move from the origin sends a position packet");
        // Same position, so only the flag byte can differ; the reminder
        // counter is what makes the second send happen at all.
        let collision = adapter
            .select_move_packet(pos, Rotation::default(), true, true)
            .expect("encodes")
            .expect("a changed flag byte sends a status-only packet");
        assert_eq!(no_collision.1.len(), 25, "three f64 plus one flag byte");
        assert_eq!(no_collision.1[24], 0x01, "on ground, not colliding");
        assert_eq!(collision.0, adapter.ids().move_player_status_only);
        assert_eq!(collision.1, vec![0x03], "on ground and colliding");
    }
}

#[cfg(test)]
mod mob_effect_tests {
    use super::*;
    use lodestone_world::World;

    fn encoded_update(adapter: &V774Adapter, effect_id: i32) -> Vec<u8> {
        adapter
            .encode_body(&UpdateMobEffect {
                entity_id: 42,
                effect_id,
                amplifier: 0,
                duration: 40,
                flags: 0,
            })
            .expect("update mob effect encodes")
    }

    fn encoded_remove(adapter: &V774Adapter, effect_id: i32) -> Vec<u8> {
        adapter
            .encode_body(&RemoveMobEffect {
                entity_id: 42,
                effect_id,
            })
            .expect("remove mob effect encodes")
    }

    #[test]
    fn modern_zero_based_speed_id_resolves_for_update_and_remove() {
        let adapter = V774Adapter::new();
        let mut world = World::new();
        let applied = V774Adapter::handle_play_update_mob_effect(
            &adapter,
            &mut world,
            &encoded_update(&adapter, 0),
        )
        .expect("known modern effect decodes");
        let [Directive::Emit(ClientEvent::MobEffectApplied { effect, .. })] = applied.as_slice()
        else {
            panic!("known effect did not emit one application event: {applied:?}");
        };
        assert_eq!(effect.path(), "speed");

        let removed = V774Adapter::handle_play_remove_mob_effect(
            &adapter,
            &mut world,
            &encoded_remove(&adapter, 0),
        )
        .expect("known modern effect removal decodes");
        let [Directive::Emit(ClientEvent::MobEffectRemoved { effect })] = removed.as_slice()
        else {
            panic!("known effect did not emit one removal event: {removed:?}");
        };
        assert_eq!(effect.path(), "speed");
    }

    #[test]
    fn unknown_modern_effect_ids_are_rejected_at_packet_ingress() {
        let unknown_ids = [-1, lodestone_data::mob_effects::MOB_EFFECT_COUNT as i32];
        let adapter = V774Adapter::new();
        for effect_id in unknown_ids {
            let mut world = World::new();
            let error = V774Adapter::handle_play_update_mob_effect(
                &adapter,
                &mut world,
                &encoded_update(&adapter, effect_id),
            )
            .expect_err("unknown update effect must fail closed");
            assert!(
                error
                    .to_string()
                    .contains(&format!("unknown effect id {effect_id}")),
                "update id {effect_id}: {error}"
            );

            let error = V774Adapter::handle_play_remove_mob_effect(
                &adapter,
                &mut world,
                &encoded_remove(&adapter, effect_id),
            )
            .expect_err("unknown removal effect must fail closed");
            assert!(
                error
                    .to_string()
                    .contains(&format!("unknown effect id {effect_id}")),
                "remove id {effect_id}: {error}"
            );
        }
    }
}

// `CLIENTBOUND`, `IGNORED` and `PlayHandler` are crate-private plumbing, not
// part of the crate's public API, so an integration test under `tests/` cannot
// name them. Exposing them solely so an external file could reach them would
// leak internal representation for no benefit over a unit-test module here.
#[cfg(test)]
mod dispatch_coverage_tests {
    use super::*;

    /// The real table, built from the real `ENTRIES`/`CLIENTBOUND`/`IGNORED`
    /// for every protocol in this era, must construct — meaningful because
    /// `Table::build` fails the moment any clientbound id is neither handled
    /// nor declared `IGNORED`.
    #[test]
    fn play_dispatch_table_builds_for_every_protocol() {
        for &protocol in PROTOCOLS {
            let table = lodestone_core::dispatch::Table::build(
                protocol,
                ids_for(protocol).play_clientbound_entries,
                CLIENTBOUND,
                IGNORED,
            );
            assert!(
                table.is_ok(),
                "protocol {protocol}: every clientbound ENTRIES id must be handled or \
                 explicitly IGNORED: {:?}",
                table.err()
            );
        }
    }

    /// Negative control: drop one `IGNORED` entry from a local copy. Its
    /// packet id then has neither a handler nor an ignore entry, so
    /// construction must fail *by name* — which is what proves the check above
    /// is doing anything at all.
    #[test]
    fn negative_control_dropping_one_ignored_entry_fails_construction() {
        let position = IGNORED
            .iter()
            .position(|entry| entry.name == "minecraft:update_tags")
            .expect("minecraft:update_tags is an IGNORED entry");
        let mut ignored_missing_one: Vec<lodestone_core::dispatch::IGNORED> = IGNORED.to_vec();
        let removed = ignored_missing_one.remove(position);
        assert_eq!(removed.name, "minecraft:update_tags");
        let entries = ids_for(PROTOCOL).play_clientbound_entries;
        let tags_id = entries
            .iter()
            .find(|(name, _)| *name == "minecraft:update_tags")
            .map(|(_, id)| *id)
            .expect("this era carries minecraft:update_tags");
        let table = lodestone_core::dispatch::Table::build(
            PROTOCOL,
            entries,
            CLIENTBOUND,
            &ignored_missing_one,
        );
        assert_eq!(
            table.err(),
            Some(lodestone_core::dispatch::DispatchError::UnlistedId {
                name: "minecraft:update_tags",
                id: tags_id,
            }),
            "dropping the minecraft:update_tags IGNORED entry must fail construction on that \
             packet"
        );
    }

    /// The ids this crate speaks are its own, not a neighbouring era's.
    ///
    /// The expected numbers are written literally on one side and read out of
    /// the generated table on the other, so the test cannot pass by reading
    /// the same value twice. Both probes sit at different ids in the 1.20.6
    /// era, so a table silently inherited from there would fail here.
    #[test]
    fn the_clientbound_ids_are_this_protocols_own() {
        let id = |name: &str| {
            ids_for(PROTOCOL)
                .play_clientbound_entries
                .iter()
                .find(|(entry, _)| *entry == name)
                .map(|(_, id)| *id)
                .unwrap_or_else(|| panic!("{name} is in this protocol's clientbound ENTRIES"))
        };
        assert_eq!(
            (id("minecraft:login"), id("minecraft:keep_alive")),
            (48, 43),
            "these are 774's ids; 766's are (43, 38) for the same two packets"
        );
    }
}
