//! [`VersionAdapter`] implementation driving this era's join flow, for
//! protocol 766.
//!
//! # The join is three states, not two
//!
//! Every era below this one goes handshake → login → play. Here the login
//! state ends with an *acknowledgement* and the connection enters a
//! configuration state, where the server delivers its registries, its
//! feature flags and its tags, and where the client announces its brand and
//! its client information. Play begins only when both sides exchange a
//! finish-configuration packet. [`V766Adapter::handle_configuration`] is the
//! whole of that phase, and it is not a login-time detour: `start_configuration`
//! can pull a *playing* connection back into it at any time.
//!
//! # What the configuration phase decides
//!
//! The vertical window of every column decoded afterwards. The join packet
//! names its dimension by a **registry index**, and the registry that index
//! points into arrives here as `registry_data` for
//! `minecraft:dimension_type`. An adapter that skips the phase has no way to
//! frame a column: it does not know how many sections one holds, and a wrong
//! section count desynchronises the stream rather than erroring.

use std::collections::BTreeMap;
use std::mem::size_of;
use std::sync::{Arc, LockResult, Mutex, MutexGuard, PoisonError};

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_data::block_entity_types::block_entity_type;
use lodestone_data::mob_effects::{mob_effect_name_for, MobEffectId};
use lodestone_model::{
    AdapterError, AnimationAction, BlockActionKind, BlockFace, ChatAckInfo, ChatKind, ChatMode,
    ChatSessionInfo, ChunkPos, ClientAction, ClientEvent, ClientSettings, ConnectionState,
    ContainerClickType, Difficulty, Directive, DisplayedSkinParts, EntityAttributeModifier,
    EntityAttributeSnapshot, EntityEquipment, EntityInteraction, EntityMetadataUpdate,
    EntityMovement, EquipmentSlot, GameMode, Hand, ItemStack, LoginProfile, MainHand,
    PlayerCommand, PlayerListEntry, ProfileProperty, RecipeBookType, ResourceKey,
    ResourcePackResponseKind, Rotation, SectionPos, ServerAddress, TeleportFlags, Text, Vec3,
    VersionAdapter, WorldSink,
};
use lodestone_world::{ChunkPos as WorldChunkPos, Heightmaps, LoadedChunk};

use crate::canonical::FallbackTally;
use crate::entity_types;
use crate::registry;
use crate::packets::chat::{
    ChatCommand, ChatMessage, MessageAcknowledgement, PlayerChat, ProfilelessChat, SystemChat,
};
use crate::packets::chunk::{ChunkShape, DimensionRegistry, MapChunk, UnloadChunk, UpdateLight};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse, NetworkNbt};
use crate::packets::configuration::{
    AcknowledgeFinishConfiguration, ConfigurationBrandPayload, ConfigurationDisconnect,
    ConfigurationKeepAliveRequest, ConfigurationKeepAliveResponse, ConfigurationPing,
    ConfigurationPong, ConfigurationSettings, RegistryData, SelectKnownPacksResponse,
};
use crate::packets::entity::{
    AttachEntity, Collect, EntityAnimation, EntityDestroy, EntityHeadRotation, EntityLook,
    EntityMetadataPacket, EntityMoveLook, EntityStatus, EntityTeleport, EntityVelocityPacket,
    RelEntityMove, SetPassengers, SpawnEntityExperienceOrb, SpawnObject,
};
use crate::packets::game::{
    BlockBreakAnimation, BlockDig, BlockPlace, ChunkBatchFinished, ChunkBatchReceived,
    ClientCommand, ClientboundAbilities, ClientboundPositionLook, ConfigurationAcknowledged,
    DifficultyPacket, EntityAction, EntityEffect, GameStateChange, JoinGame, KickDisconnect,
    MultiBlockChange, OpenSignEntity, PlayerlistHeader, RecipeBook, RemoveEntityEffect, Respawn,
    ServerboundArmAnimation,
    ServerboundFlying, ServerboundLook, ServerboundPosition, ServerboundPositionLook, SpawnPosition,
    Spectate, TeleportConfirm, UpdateHealth, UpdateTime, UseEntity, UseEntityAt, UseEntityInteract,
    UseItem,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{
    EncryptionRequest, LoginAcknowledged, LoginDisconnect, LoginStart, LoginSuccess, SetCompression,
};
use crate::packets::metadata::MetadataValue;
use crate::packets::player_info::{PlayerInfoRemove, PlayerInfoUpdate};
use crate::packets::position::Position;
use crate::packets::settings::{BrandPayload, PlayerAbilities, ResourcePackReceive, Settings};
use crate::packets::slot::Slot;
use crate::packets::window::{
    ChangedSlot, CloseWindow, CraftProgressBar, EnchantItem, HeldItemSlot, ServerboundCloseWindow,
    ServerboundHeldItemSlot, SetCreativeSlot, WindowClick,
};

/// The protocol this family speaks, and the one a zero-argument [`adapter`]
/// constructs.
///
/// The folder is named `1.20.6` and this protocol is **766**. Never derive one
/// from the other — ask [`PROTOCOLS`].
pub const PROTOCOL: i32 = PROTOCOL_1_20_6;

/// Protocol version of Minecraft 1.20.5 and 1.20.6.
///
/// Read off the jar's own `version.json` in `.cache/mc/1.20.6/server.jar`,
/// which reports `"protocol_version": 766`, and independently off
/// `minecraft-data`'s `protocolVersions.json`, which lists 766 for both
/// 1.20.5 and 1.20.6. The two releases are the same wire version — 1.20.6
/// changed no packet — which is why one number covers two Minecraft versions.
pub const PROTOCOL_1_20_6: i32 = 766;

/// Every protocol number this family speaks — the single source of truth for
/// its coverage.
///
/// [`VersionAdapter::supports`] tests membership here, and
/// `lodestone-registry`'s `FAMILIES` entry points at this same slice, so the
/// registry's view of a family cannot drift from the family's own.
///
/// One entry, two Minecraft versions. The wire era is measurably **wider**
/// than this list: using `minecraft-data` with named types inlined
/// recursively, 766 agrees with 767 on 204 of 226 packet shapes (90%), above
/// the 85% grouping threshold, while 765 below agrees on 177 of 220 (80%) and
/// 762 on 119 of 220 (54%). So the lower boundary is real and the upper one
/// is not: 767 belongs in this crate and is not yet implemented here.
/// `PROTOCOLS` lists what is implemented and checked against real bytes, never
/// what the shape measurement permits.
pub const PROTOCOLS: &[i32] = &[PROTOCOL_1_20_6];

/// The packet ids one protocol in this era assigns to the packets this
/// adapter names.
///
/// The generated `packet_ids` tables are one module per protocol, so a
/// `self.ids().block_dig` path can only ever mean *one* protocol's id. This
/// struct is the indirection that lets a single adapter body serve several:
/// it is resolved once, at construction, from the negotiated protocol, and
/// every id an arm sends reads through it. Nothing in this file may name a
/// generated module directly outside `packet_ids_from!`.
#[derive(Debug)]
struct PacketIds {
    /// This protocol's whole clientbound play table, the denominator the
    /// dispatch table is built against.
    play_clientbound_entries: &'static [(&'static str, i32)],
    /// `minecraft:set_protocol`, serverbound handshaking.
    handshake_set_protocol: i32,
    /// `minecraft:login_start`, serverbound login.
    login_start: i32,
    /// `minecraft:login_acknowledged`, serverbound login — the packet that
    /// ends the login state.
    login_acknowledged: i32,
    /// `minecraft:disconnect`, clientbound login.
    login_disconnect: i32,
    /// `minecraft:encryption_begin`, clientbound login.
    login_encryption_begin: i32,
    /// `minecraft:success`, clientbound login.
    login_success: i32,
    /// `minecraft:compress`, clientbound login.
    login_compress: i32,
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
    /// `minecraft:settings`, serverbound configuration.
    config_settings: i32,
    /// `minecraft:abilities`, serverbound play.
    abilities: i32,
    /// `minecraft:arm_animation`, serverbound play.
    arm_animation: i32,
    /// `minecraft:block_dig`, serverbound play.
    block_dig: i32,
    /// `minecraft:block_place`, serverbound play.
    block_place: i32,
    /// `minecraft:chat_message`, serverbound play.
    chat_message: i32,
    /// `minecraft:chat_command`, serverbound play — the unsigned command
    /// packet, one string and nothing else at this protocol.
    chat_command: i32,
    /// `minecraft:message_acknowledgement`, serverbound play.
    message_acknowledgement: i32,
    /// `minecraft:chunk_batch_received`, serverbound play — the chunk-pacing
    /// reply without which the server throttles world delivery.
    chunk_batch_received: i32,
    /// `minecraft:client_command`, serverbound play.
    client_command: i32,
    /// `minecraft:close_window`, serverbound play.
    close_window: i32,
    /// `minecraft:configuration_acknowledged`, serverbound play — the reply
    /// that re-enters the configuration phase.
    configuration_acknowledged: i32,
    /// `minecraft:custom_payload`, serverbound play.
    custom_payload: i32,
    /// `minecraft:enchant_item`, serverbound play.
    enchant_item: i32,
    /// `minecraft:entity_action`, serverbound play.
    entity_action: i32,
    /// `minecraft:flying`, serverbound play.
    flying: i32,
    /// `minecraft:held_item_slot`, serverbound play.
    held_item_slot: i32,
    /// `minecraft:keep_alive`, serverbound play.
    keep_alive: i32,
    /// `minecraft:look`, serverbound play.
    look: i32,
    /// `minecraft:pong`, serverbound play.
    pong: i32,
    /// `minecraft:position`, serverbound play.
    position: i32,
    /// `minecraft:position_look`, serverbound play.
    position_look: i32,
    /// `minecraft:resource_pack_receive`, serverbound play.
    resource_pack_receive: i32,
    /// `minecraft:set_creative_slot`, serverbound play.
    set_creative_slot: i32,
    /// `minecraft:settings`, serverbound play.
    settings: i32,
    /// `minecraft:spectate`, serverbound play.
    spectate: i32,
    /// `minecraft:tab_complete`, serverbound play.
    tab_complete: i32,
    /// `minecraft:teleport_confirm`, serverbound play.
    teleport_confirm: i32,
    /// `minecraft:use_entity`, serverbound play.
    use_entity: i32,
    /// `minecraft:use_item`, serverbound play.
    use_item: i32,
    /// `minecraft:window_click`, serverbound play.
    window_click: i32,
    /// `minecraft:recipe_book`, serverbound play.
    recipe_book: i32,
}

/// Builds a [`PacketIds`] from one generated table module.
macro_rules! packet_ids_from {
    ($table:ident) => {
        PacketIds {
            play_clientbound_entries: crate::$table::play::clientbound::ENTRIES,
            handshake_set_protocol: crate::$table::handshaking::serverbound::SET_PROTOCOL,
            login_start: crate::$table::login::serverbound::LOGIN_START,
            login_acknowledged: crate::$table::login::serverbound::LOGIN_ACKNOWLEDGED,
            login_disconnect: crate::$table::login::clientbound::DISCONNECT,
            login_encryption_begin: crate::$table::login::clientbound::ENCRYPTION_BEGIN,
            login_success: crate::$table::login::clientbound::SUCCESS,
            login_compress: crate::$table::login::clientbound::COMPRESS,
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
            config_settings: crate::$table::configuration::serverbound::SETTINGS,
            abilities: crate::$table::play::serverbound::ABILITIES,
            arm_animation: crate::$table::play::serverbound::ARM_ANIMATION,
            block_dig: crate::$table::play::serverbound::BLOCK_DIG,
            block_place: crate::$table::play::serverbound::BLOCK_PLACE,
            chat_message: crate::$table::play::serverbound::CHAT_MESSAGE,
            chat_command: crate::$table::play::serverbound::CHAT_COMMAND,
            message_acknowledgement: crate::$table::play::serverbound::MESSAGE_ACKNOWLEDGEMENT,
            chunk_batch_received: crate::$table::play::serverbound::CHUNK_BATCH_RECEIVED,
            client_command: crate::$table::play::serverbound::CLIENT_COMMAND,
            close_window: crate::$table::play::serverbound::CLOSE_WINDOW,
            configuration_acknowledged:
                crate::$table::play::serverbound::CONFIGURATION_ACKNOWLEDGED,
            custom_payload: crate::$table::play::serverbound::CUSTOM_PAYLOAD,
            enchant_item: crate::$table::play::serverbound::ENCHANT_ITEM,
            entity_action: crate::$table::play::serverbound::ENTITY_ACTION,
            flying: crate::$table::play::serverbound::FLYING,
            held_item_slot: crate::$table::play::serverbound::HELD_ITEM_SLOT,
            keep_alive: crate::$table::play::serverbound::KEEP_ALIVE,
            look: crate::$table::play::serverbound::LOOK,
            pong: crate::$table::play::serverbound::PONG,
            position: crate::$table::play::serverbound::POSITION,
            position_look: crate::$table::play::serverbound::POSITION_LOOK,
            resource_pack_receive: crate::$table::play::serverbound::RESOURCE_PACK_RECEIVE,
            set_creative_slot: crate::$table::play::serverbound::SET_CREATIVE_SLOT,
            settings: crate::$table::play::serverbound::SETTINGS,
            spectate: crate::$table::play::serverbound::SPECTATE,
            tab_complete: crate::$table::play::serverbound::TAB_COMPLETE,
            teleport_confirm: crate::$table::play::serverbound::TELEPORT_CONFIRM,
            use_entity: crate::$table::play::serverbound::USE_ENTITY,
            use_item: crate::$table::play::serverbound::USE_ITEM,
            window_click: crate::$table::play::serverbound::WINDOW_CLICK,
            recipe_book: crate::$table::play::serverbound::RECIPE_BOOK,
        }
    };
}

/// Protocol 766's ids.
static IDS_766: PacketIds = packet_ids_from!(packet_ids);

/// Resolves a negotiated protocol to its id table.
///
/// # Panics
///
/// Panics for a protocol outside [`PROTOCOLS`]. This is a construction-time
/// check on a value the registry has already tested for membership, not a
/// wire value: reaching it means a caller bypassed
/// `VersionAdapter::supports`, and answering with some other protocol's ids
/// would be the silent-wrong-wire failure this indirection exists to prevent.
fn ids_for(protocol: i32) -> &'static PacketIds {
    match protocol {
        PROTOCOL_1_20_6 => &IDS_766,
        other => panic!(
            "protocol {other} is outside this family's PROTOCOLS ({PROTOCOLS:?}); \
             callers must test membership before constructing an adapter"
        ),
    }
}

/// Requested next-state value in the handshake for a login connection.
const NEXT_STATE_LOGIN: i32 = 2;

/// Relative-teleport flag bits on the clientbound position packet.
const REL_X: i8 = 0x01;
const REL_Y: i8 = 0x02;
const REL_Z: i8 = 0x04;
const REL_YAW: i8 = 0x08;
const REL_PITCH: i8 = 0x10;

/// The registry whose entry order the join packet's dimension index refers
/// to. Every other registry the configuration phase delivers is passed over.
const DIMENSION_TYPE_REGISTRY: &str = "minecraft:dimension_type";

/// Columns per tick this client asks for when it answers a chunk batch.
///
/// The value is a *request*, not a measurement, and a server that receives
/// none throttles chunk delivery to its floor. Chosen at the vanilla
/// server's own per-tick cap so a batch reply never asks for less than the
/// server would send unprompted.
const CHUNKS_PER_TICK: f32 = 64.0;

/// Per-connection state used by this era's client-side position-send tick.
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

/// Version adapter implementing this era's protocol.
///
/// Four pieces of per-connection state, every one load-bearing:
///
/// * `dimension_registry`, the `minecraft:dimension_type` entries the
///   configuration phase delivered. The join packet names its dimension by an
///   **index into this**, so without it there is no vertical window and no
///   way to frame a column.
/// * `shape`, the resolved window itself, re-resolved on every join and
///   respawn.
/// * `pending_ack`, the count of signed player-chat messages received but not
///   yet acknowledged. A server whose pending list is never drained
///   disconnects the connection, so this counter is what keeps a
///   chat-reading session alive.
/// * `movement`, the client-side position-send state.
#[derive(Debug, Clone)]
pub struct V766Adapter {
    /// The negotiated protocol this adapter speaks: one of [`PROTOCOLS`].
    protocol: i32,
    /// This protocol's id table, resolved once at construction by
    /// [`ids_for`].
    ids: &'static PacketIds,
    shape: Arc<Mutex<ChunkShape>>,
    dimension_registry: Arc<Mutex<DimensionRegistry>>,
    /// Namespaced level name from the most recent join or respawn, so a
    /// packet that carries no dimension field of its own (`spawn_position`)
    /// can still report one.
    current_dimension: Arc<Mutex<String>>,
    pending_ack: Arc<Mutex<i32>>,
    movement: Arc<Mutex<MovementSendState>>,
}

impl Default for V766Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl V766Adapter {
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
            current_dimension: Arc::new(Mutex::new("minecraft:overworld".to_owned())),
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

    /// The level name the most recent join or respawn reported.
    fn current_dimension(&self) -> String {
        self.current_dimension
            .lock()
            .map(|name| name.clone())
            .unwrap_or_else(|err| err.into_inner().clone())
    }

    fn set_dimension(&self, name: &str) {
        if let Ok(mut current) = self.current_dimension.lock() {
            current.clear();
            current.push_str(name);
        }
    }

    /// Records the dimension-type registry one `registry_data` packet
    /// carried.
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
            Some((self.ids().position_look, self.encode_body(&body)?))
        } else if moved {
            let body = ServerboundPosition {
                x: pos.x,
                y: pos.y,
                z: pos.z,
                on_ground,
            };
            Some((self.ids().position, self.encode_body(&body)?))
        } else if rotated {
            let body = ServerboundLook {
                yaw: rotation.yaw,
                pitch: rotation.pitch,
                on_ground,
            };
            Some((self.ids().look, self.encode_body(&body)?))
        } else if state.last_on_ground != on_ground {
            let body = ServerboundFlying { on_ground };
            Some((self.ids().flying, self.encode_body(&body)?))
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

    /// Handles a clientbound packet while in the login state.
    ///
    /// The one structural difference from every era below: login `success`
    /// does **not** enter play. It enters configuration, and the transition
    /// is explicit — the client sends `login_acknowledged` first, then the
    /// brand and client-information packets the phase expects, all after the
    /// state change so they are framed as configuration packets.
    fn handle_login(&self, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == self.ids().login_compress {
            let body: SetCompression = self.decode_body(payload)?;
            return Ok(vec![Directive::SetCompression(body.threshold)]);
        }
        if packet_id == self.ids().login_success {
            let _profile: LoginSuccess = self.decode_body(payload)?;
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
                self.send(self.ids().config_settings, &default_client_information())?,
            ]);
        }
        if packet_id == self.ids().login_encryption_begin {
            let _request: EncryptionRequest = self.decode_body(payload)?;
            return Err(AdapterError::Unsupported(
                "encryption / online-mode authentication (login encryption_begin) is not yet \
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
    /// cookies) are the server telling the client things this client does not
    /// act on during configuration. A `finish_configuration` cannot be missed
    /// because it is matched explicitly.
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
fn default_client_information() -> ConfigurationSettings {
    ConfigurationSettings {
        locale: "en_us".to_owned(),
        view_distance: 8,
        chat_flags: 0,
        chat_colors: true,
        skin_parts: 0x7f,
        main_hand: 1,
        text_filtering: false,
        allow_server_listing: true,
    }
}

/// Maps the model's `RecipeBookType` onto this era's `recipe_book` ordinal.
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
/// The play- and configuration-state disconnects use this; the login-state
/// one is still a JSON string at this protocol and uses
/// [`json_reason_text`]. Keeping two functions rather than one that sniffs
/// the payload is deliberate: the *state* decides the form, and a sniff would
/// silently accept the wrong one.
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
/// nothing else — the shape the split title packets and the action bar share
/// at this protocol.
fn decode_single_nbt_text(payload: &[u8]) -> Result<Text, AdapterError> {
    let mut reader = Reader::new(payload);
    let nbt = lodestone_core::read_network_nbt(&mut reader).map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(Text::from_nbt(&nbt))
}

/// Delta-position scale: each `i16` is `1/4096` of a block.
const MOVE_DELTA_SCALE: f64 = 4096.0;

/// Velocity scale: each `i16` is `1/8000` of a block per tick.
const VELOCITY_SCALE: f64 = 8000.0;

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
/// Index `0` with a `BYTE` serializer is the base entity's own flags field —
/// on-fire, crouching, sprinting, swimming, invisible, glowing, fall-flying —
/// and it is the one index whose meaning needs no knowledge of the entity's
/// type, because every entity inherits it. Every *other* index collides
/// between entity categories at this protocol, which is why this arm reports
/// only this one.
const METADATA_INDEX_SHARED_FLAGS: u8 = 0;

/// A server cannot validly send more than 128 attribute snapshots in one
/// update; the cap bounds hostile allocation before the first entry is read.
const MAX_ATTRIBUTE_ENTRIES: usize = 128;

fn checked_count(raw: i32, cap: usize, available: usize, what: &str) -> Result<usize, AdapterError> {
    let count = usize::try_from(raw)
        .map_err(|_| AdapterError::Decode(format!("negative {what} {raw}")))?;
    let limit = cap.min(available);
    if count > limit {
        return Err(AdapterError::Decode(format!(
            "{what} {count} exceeds bounded limit {limit}"
        )));
    }
    Ok(count)
}

fn canonical_attribute_key(wire: &str) -> Result<ResourceKey, AdapterError> {
    // This wire registry has three namespaces (`generic`, `player`, and
    // `zombie`) that all collapse into the model's unqualified attribute
    // paths. Keep the accepted prefixes explicit: another dotted namespace
    // would need a deliberate model mapping rather than silently becoming a
    // canonical key.
    let path = wire
        .strip_prefix("minecraft:generic.")
        .or_else(|| wire.strip_prefix("minecraft:player."))
        .or_else(|| wire.strip_prefix("minecraft:zombie."))
        .ok_or_else(|| {
            AdapterError::Decode(format!("unsupported attribute key {wire}"))
        })?;
    let canonical = format!("minecraft:{path}");
    canonical
        .parse()
        .map_err(|_| AdapterError::Decode(format!("invalid attribute key {wire}")))
}

/// Converts a low-level [`lodestone_core::Error`] into an [`AdapterError`].
fn dec_err(err: impl std::fmt::Display) -> AdapterError {
    AdapterError::Decode(err.to_string())
}

/// Consumes one protocol-766 particle value without retaining its visual
/// parameters. The local protocol schema fixes both the registry's 109 ids and
/// the option shape selected by each id, so this is sufficient to reach the
/// following sound holder without guessing its byte boundary.
fn skip_explosion_particle(reader: &mut Reader<'_>, ctx: Ctx) -> Result<(), AdapterError> {
    let id = reader.var_i32().map_err(dec_err)?;
    if !(0..=108).contains(&id) {
        return Err(AdapterError::Decode(format!(
            "unknown 766 explosion particle id {id}"
        )));
    }
    match id {
        1 | 2 | 28 | 105 | 99 => {
            let _ = reader.var_i32().map_err(dec_err)?;
        }
        13 => {
            for _ in 0..4 {
                let _ = reader.f32().map_err(dec_err)?;
            }
        }
        14 => {
            for _ in 0..7 {
                let _ = reader.f32().map_err(dec_err)?;
            }
        }
        20 => {
            let _ = reader.i32().map_err(dec_err)?;
        }
        35 => {
            let _ = reader.f32().map_err(dec_err)?;
        }
        44 => {
            let _ = Slot::decode(reader, ctx).map_err(dec_err)?;
        }
        45 => {
            let position_kind = reader.var_i32().map_err(dec_err)?;
            match position_kind {
                0 => {
                    let _ = Position::decode(reader, ctx).map_err(dec_err)?;
                }
                1 => {
                    let _ = reader.var_i32().map_err(dec_err)?;
                    let _ = reader.f32().map_err(dec_err)?;
                }
                _ => {
                    return Err(AdapterError::Decode(format!(
                        "invalid 766 vibration position kind {position_kind}"
                    )));
                }
            }
            let _ = reader.var_i32().map_err(dec_err)?;
        }
        _ => {}
    }
    Ok(())
}

/// Consumes the mandatory sound holder following an explosion's particle
/// values. A positive holder is an indexed registry reference; zero instead
/// introduces the inline identifier and optional fixed range.
fn skip_explosion_sound_holder(reader: &mut Reader<'_>) -> Result<(), AdapterError> {
    let holder = reader.var_i32().map_err(dec_err)?;
    match holder {
        0 => {
            let _ = reader.string(32_767).map_err(dec_err)?;
            if reader.bool().map_err(dec_err)? {
                let _ = reader.f32().map_err(dec_err)?;
            }
        }
        1.. => {}
        _ => {
            return Err(AdapterError::Decode(format!(
                "negative 766 explosion sound holder {holder}"
            )));
        }
    }
    Ok(())
}

/// Fn-pointer payload every `play` clientbound handler below shares.
type PlayHandler =
    fn(&V766Adapter, &mut dyn WorldSink, &[u8]) -> Result<Vec<Directive>, AdapterError>;

impl V766Adapter {
    /// `minecraft:login`. Names its dimension by a registry index — see
    /// [`V766Adapter::adopt_dimension_shape`].
    fn handle_play_login(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: JoinGame = adapter.decode_body(payload)?;
        adapter.adopt_dimension_shape(body.world_state.dimension);
        adapter.set_dimension(&body.world_state.world_name);
        Ok(vec![Directive::Emit(ClientEvent::Login {
            entity_id: body.entity_id,
            game_mode: game_mode(body.world_state.game_mode as u8)?,
            dimension: dimension_id(&body.world_state.world_name)?,
        })])
    }

    /// `minecraft:respawn`. Carries the same spawn description the join
    /// packet does, so a respawn into a dimension of a different height
    /// re-resolves the column shape here rather than inheriting a stale one.
    fn handle_play_respawn(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: Respawn = adapter.decode_body(payload)?;
        adapter.adopt_dimension_shape(body.world_state.dimension);
        adapter.set_dimension(&body.world_state.world_name);
        Ok(vec![Directive::Emit(ClientEvent::Respawned {
            dimension: dimension_id(&body.world_state.world_name)?,
            game_mode: game_mode(body.world_state.game_mode as u8)?,
            previous_game_mode: None,
            last_death_location: None,
        })])
    }

    /// `minecraft:map_chunk`.
    fn handle_play_map_chunk(
        adapter: &V766Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let shape = adapter.current_shape();
        let mut reader = Reader::new(payload);
        let data = MapChunk::decode(&mut reader, &shape).map_err(dec_err)?;
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

    /// `minecraft:update_light`.
    fn handle_play_update_light(
        adapter: &V766Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let update =
            UpdateLight::decode(&mut reader, &adapter.current_shape()).map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        world.merge_light(WorldChunkPos::new(update.x, update.z), update.patch);
        Ok(Vec::new())
    }

    /// `minecraft:unload_chunk`. Two plain ints, **z first** — see
    /// [`UnloadChunk`].
    fn handle_play_unload_chunk(
        adapter: &V766Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: UnloadChunk = adapter.decode_body_exact(payload)?;
        let pos = ChunkPos::new(body.chunk_x, body.chunk_z);
        world.unload(WorldChunkPos::new(body.chunk_x, body.chunk_z));
        Ok(vec![Directive::Emit(ClientEvent::ChunkUnloaded { pos })])
    }

    /// `minecraft:keep_alive`.
    fn handle_play_keep_alive(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let keep_alive: KeepAliveRequest = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::KeepAlive {
            id: keep_alive.id,
        })])
    }

    /// `minecraft:ping` (play state). Answered immediately with `pong`; the
    /// event is emitted so a consumer can time the round trip.
    fn handle_play_ping(
        adapter: &V766Adapter,
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
        adapter: &V766Adapter,
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

    /// `minecraft:profileless_chat`.
    fn handle_play_profileless_chat(
        adapter: &V766Adapter,
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
    /// The displayed text prefers the server's decorated form, but
    /// `raw_content` always keeps the *signed* string: a signature is taken
    /// over exactly that and never over the decoration.
    fn handle_play_player_chat(
        adapter: &V766Adapter,
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
                // There is no server-global message index at this protocol —
                // the packet opens with the sender UUID. Reported as this
                // message's own chain index so the field is never silently
                // another number's value.
                global_index: body.index,
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

    /// `minecraft:hide_message` — retract a delivered signed message.
    ///
    /// A cache-index reference (`id != 0`) cannot be resolved here: that
    /// needs a per-connection signature cache this adapter does not keep, so
    /// only the inline-signature form produces an event.
    fn handle_play_hide_message(
        _adapter: &V766Adapter,
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

    /// `minecraft:position`.
    fn handle_play_position(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ClientboundPositionLook = adapter.decode_body(payload)?;
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
        let confirm = TeleportConfirm {
            teleport_id: body.teleport_id,
        };
        Ok(vec![
            adapter.send(adapter.ids().teleport_confirm, &confirm)?,
            Directive::Emit(ClientEvent::TeleportPlayer {
                pos: Vec3::new(body.x, body.y, body.z),
                rotation: Rotation::new(body.yaw, body.pitch),
                flags,
                velocity: None,
            }),
        ])
    }

    /// `minecraft:chunk_batch_start`. Opens a paced batch; nothing to report
    /// until it closes.
    fn handle_play_chunk_batch_start(
        _adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(Vec::new())
    }

    /// `minecraft:chunk_batch_finished`. Answering this is not politeness:
    /// a server that gets no pacing reply throttles chunk delivery to its
    /// floor, so a client that ignores the packet loads the world at a
    /// trickle with nothing logged anywhere.
    fn handle_play_chunk_batch_finished(
        adapter: &V766Adapter,
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
    /// into the configuration phase; the client acknowledges and re-enters
    /// it. Without this the next `registry_data` is read as a play packet.
    fn handle_play_start_configuration(
        adapter: &V766Adapter,
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

    /// `minecraft:spawn_entity` — **every** entity at this protocol,
    /// including players, told apart only by the type id resolved through
    /// [`crate::entity_types`].
    fn handle_play_spawn_entity(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnObject = adapter.decode_body(payload)?;
        let type_id = body.kind;
        let entity_type = entity_types::table_for(adapter.protocol)
            .entity_type_name(type_id)
            .ok_or_else(|| {
                AdapterError::Decode(format!("unknown entity type id {type_id} in spawn"))
            })?
            .parse()
            .map_err(|_| AdapterError::Decode(format!("entity type id {type_id} is not a key")))?;
        // Velocity is always on the wire, but a stationary entity still
        // reports zero; forward `None` only when every component is zero, to
        // match "no motion" rather than "explicit zero motion".
        let velocity = if body.velocity_x == 0 && body.velocity_y == 0 && body.velocity_z == 0 {
            None
        } else {
            Some(Vec3::new(
                f64::from(body.velocity_x) / VELOCITY_SCALE,
                f64::from(body.velocity_y) / VELOCITY_SCALE,
                f64::from(body.velocity_z) / VELOCITY_SCALE,
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

    /// `minecraft:spawn_entity_experience_orb`.
    fn handle_play_spawn_entity_experience_orb(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnEntityExperienceOrb = adapter.decode_body(payload)?;
        let entity_type: ResourceKey = "minecraft:experience_orb"
            .parse()
            .map_err(|_| AdapterError::Decode("experience_orb key invalid".to_owned()))?;
        Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
            entity_id: body.entity_id,
            uuid: None,
            entity_type,
            pos: Vec3::new(body.x, body.y, body.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        })])
    }

    /// `minecraft:rel_entity_move`.
    fn handle_play_rel_entity_move(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: RelEntityMove = adapter.decode_body(payload)?;
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

    /// `minecraft:entity_move_look`.
    fn handle_play_entity_move_look(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityMoveLook = adapter.decode_body(payload)?;
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

    /// `minecraft:entity_look`.
    fn handle_play_entity_look(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityLook = adapter.decode_body(payload)?;
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

    /// `minecraft:entity_teleport`.
    fn handle_play_entity_teleport(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityTeleport = adapter.decode_body(payload)?;
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

    /// `minecraft:entity_velocity`.
    fn handle_play_entity_velocity(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityVelocityPacket = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityVelocity {
            entity_id: body.entity_id,
            velocity: Vec3::new(
                f64::from(body.velocity_x) / VELOCITY_SCALE,
                f64::from(body.velocity_y) / VELOCITY_SCALE,
                f64::from(body.velocity_z) / VELOCITY_SCALE,
            ),
        })])
    }

    /// `minecraft:entity_head_rotation`.
    fn handle_play_entity_head_rotation(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityHeadRotation = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityHeadRotation {
            entity_id: body.entity_id,
            head_yaw: unpack_degrees(body.head_yaw),
        })])
    }

    /// `minecraft:entity_destroy`.
    fn handle_play_entity_destroy(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityDestroy = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityRemoved {
            entity_ids: body.entity_ids,
        })])
    }

    /// `minecraft:entity_status`.
    fn handle_play_entity_status(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityStatus = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityStatus {
            entity_id: body.entity_id,
            status: body.status as u8,
        })])
    }

    /// `minecraft:animation`.
    fn handle_play_animation(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityAnimation = adapter.decode_body_exact(payload)?;
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

    /// `minecraft:entity_metadata`.
    ///
    /// Only the shared entity flags byte at index
    /// [`METADATA_INDEX_SHARED_FLAGS`] is reported. Every other index at this
    /// protocol is claimed by more than one entity category with the same
    /// serializer, and this adapter has no id-to-category map to tell them
    /// apart — reporting one anyway would put an arrow's crit bit where a
    /// player's using-item bit belongs. The whole entry list is still decoded
    /// (and any unmodelled serializer still refused by name), so an
    /// unrecognised field fails loudly rather than desynchronising.
    fn handle_play_entity_metadata(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityMetadataPacket = adapter.decode_body(payload)?;
        let flags = body.metadata.0.iter().find_map(|entry| {
            match (entry.key, &entry.value) {
                (METADATA_INDEX_SHARED_FLAGS, MetadataValue::Byte(bits)) => Some(*bits as u8),
                _ => None,
            }
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

    /// `minecraft:entity_equipment`: the high bit on each slot byte means
    /// another `(slot, stack)` pair follows. The item bridge turns the
    /// protocol-local registry id into the canonical key the ECS and renderer
    /// consume; component payloads have already been framed by [`Slot`].
    fn handle_play_entity_equipment(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let entity_id = reader.var_i32().map_err(dec_err)?;
        let mut equipment = Vec::new();
        loop {
            if equipment.len() == EquipmentSlot::ALL.len() {
                return Err(AdapterError::Decode(format!(
                    "entity equipment carries more than {} entries",
                    EquipmentSlot::ALL.len()
                )));
            }
            let encoded_slot = reader.u8().map_err(dec_err)?;
            let slot = EquipmentSlot::from_ordinal(encoded_slot & 0x7f).ok_or_else(|| {
                AdapterError::Decode(format!("unknown equipment slot {}", encoded_slot & 0x7f))
            })?;
            let item = match Slot::decode(&mut reader, adapter.ctx()).map_err(dec_err)? {
                Slot::Empty => None,
                Slot::Item {
                    id,
                    count,
                    components,
                    removed,
                } => {
                    let name = registry::item_name(id).ok_or_else(|| {
                        AdapterError::Decode(format!("unknown item registry id {id}"))
                    })?;
                    let key: ResourceKey = name.parse().map_err(|_| {
                        AdapterError::Decode(format!("invalid item key {name}"))
                    })?;
                    let count = u32::try_from(count).map_err(|_| {
                        AdapterError::Decode(format!("invalid item count {count}"))
                    })?;
                    let mut item = ItemStack::new(key, count);
                    // The model has no semantic projection for this era's
                    // component patch. Retain its presence so consumers do
                    // not mistake the prototype-only stack for a complete
                    // effective stack.
                    item.components.has_unmodeled =
                        !components.is_empty() || !removed.is_empty();
                    Some(item)
                }
            };
            equipment.push(EntityEquipment { slot, item });
            if encoded_slot & 0x80 == 0 {
                break;
            }
        }
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityEquipmentUpdated {
            entity_id,
            equipment,
        })])
    }

    /// `minecraft:entity_update_attributes`: registry-holder attributes, a
    /// base double, then UUID-identified modifiers. The registry bridge is
    /// deliberately local to this protocol; borrowing a neighbour's dense
    /// order would produce plausible but wrong attributes.
    fn handle_play_entity_update_attributes(
        _adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let entity_id = reader.var_i32().map_err(dec_err)?;
        let count = checked_count(
            reader.var_i32().map_err(dec_err)?,
            MAX_ATTRIBUTE_ENTRIES,
            reader.remaining(),
            "attribute count",
        )?;
        let mut attributes = Vec::with_capacity(count);
        for _ in 0..count {
            let id = reader.var_i32().map_err(dec_err)?;
            let wire = registry::attribute_name(id).ok_or_else(|| {
                AdapterError::Decode(format!("unknown attribute registry id {id}"))
            })?;
            let base = reader.f64().map_err(dec_err)?;
            let modifier_count = checked_count(
                reader.var_i32().map_err(dec_err)?,
                reader.remaining() / (16 + size_of::<f64>() + 1),
                reader.remaining() / (16 + size_of::<f64>() + 1),
                "attribute modifier count",
            )?;
            let mut modifiers = Vec::with_capacity(modifier_count);
            for _ in 0..modifier_count {
                let uuid = reader.uuid().map_err(dec_err)?;
                let amount = reader.f64().map_err(dec_err)?;
                let operation = reader.u8().map_err(dec_err)?;
                if operation > 2 {
                    return Err(AdapterError::Decode(format!(
                        "attribute modifier operation {operation} is outside 0..=2"
                    )));
                }
                let modifier = format!("lodestone:legacy_modifier_{}", uuid.simple())
                    .parse()
                    .map_err(|_| AdapterError::Decode("invalid modifier identifier".to_owned()))?;
                modifiers.push(EntityAttributeModifier {
                    id: modifier,
                    amount,
                    operation,
                });
            }
            attributes.push(EntityAttributeSnapshot {
                attribute: canonical_attribute_key(wire)?,
                base,
                modifiers,
            });
        }
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityAttributesUpdated {
            entity_id,
            attributes,
        })])
    }

    /// `minecraft:block_action`: two opaque event bytes associated with a
    /// block type. The shell's block-event path owns their interpretation and
    /// turns them into the chest, bell, gateway, and spawner animation state.
    fn handle_play_block_action(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let pos: Position = Position::decode(&mut reader, adapter.ctx()).map_err(dec_err)?;
        let b0 = reader.u8().map_err(dec_err)?;
        let b1 = reader.u8().map_err(dec_err)?;
        let id = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        let block = registry::block_name(id)
            .ok_or_else(|| AdapterError::Decode(format!("unknown block registry id {id}")))?
            .parse()
            .map_err(|_| AdapterError::Decode(format!("invalid block registry id {id}")))?;
        Ok(vec![Directive::Emit(ClientEvent::BlockEvent {
            pos: pos.0,
            b0,
            b1,
            block,
        })])
    }

    /// `minecraft:attach_entity`.
    fn handle_play_attach_entity(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: AttachEntity = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityLeashed {
            entity_id: body.entity_id,
            holder_id: (body.vehicle_id != -1).then_some(body.vehicle_id),
        })])
    }

    /// `minecraft:set_passengers`.
    fn handle_play_set_passengers(
        adapter: &V766Adapter,
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

    /// `minecraft:collect`.
    fn handle_play_collect(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: Collect = adapter.decode_body_exact(payload)?;
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

    /// `minecraft:entity_effect`.
    ///
    /// The effect id is the **modern zero-based** `minecraft:mob_effect`
    /// registry id at this protocol, not the one-based legacy numbering the
    /// pre-1.20.5 eras send. It is validated before the shared registry table
    /// is indexed.
    fn handle_play_entity_effect(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityEffect = adapter.decode_body(payload)?;
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

    /// `minecraft:remove_entity_effect`. Zero-based effect id, as
    /// [`Self::handle_play_entity_effect`] documents.
    fn handle_play_remove_entity_effect(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: RemoveEntityEffect = adapter.decode_body_exact(payload)?;
        let effect_id = Self::modern_mob_effect_id(body.effect_id)?;
        let effect = Self::mob_effect_key(effect_id)?;
        Ok(vec![Directive::Emit(ClientEvent::MobEffectRemoved {
            entity_id: body.entity_id,
            effect,
        })])
    }

    /// `minecraft:block_change`. A packed position then a varint **flat
    /// block-state id** in this protocol's own id space, bridged to a
    /// canonical 26.2 state through the same table the paletted chunk
    /// sections use.
    fn handle_play_block_change(
        adapter: &V766Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let pos: Position = Position::decode(&mut reader, adapter.ctx()).map_err(dec_err)?;
        let raw = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        let raw = u32::try_from(raw).map_err(|_| {
            AdapterError::Decode(format!("block_change state id {raw} is negative"))
        })?;
        let mut tally = FallbackTally::default();
        let state = adapter
            .current_shape()
            .canonical
            .resolve_or_air(raw, &mut tally);
        let pos = pos.0;
        world.set_block(pos.x, pos.y, pos.z, state.raw());
        // Writing a state is what creates or removes a block entity; no
        // packet is involved.
        world.sync_block_entity(
            pos.x,
            pos.y,
            pos.z,
            block_entity_type(state)
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

    /// `minecraft:multi_block_change` — sparse block-state changes inside one
    /// section. The source state ids use this era's palette and must take the
    /// same canonical bridge as both a column and `block_change`.
    fn handle_play_multi_block_change(
        adapter: &V766Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: MultiBlockChange = adapter.decode_body_exact(payload)?;
        let shape = adapter.current_shape();
        let mut tally = FallbackTally::default();
        let mut changed = Vec::with_capacity(body.blocks.len());
        for (local, raw) in &body.blocks {
            let raw = u32::try_from(*raw).map_err(|_| {
                AdapterError::Decode(format!("multi_block_change state id {raw} is negative"))
            })?;
            let state = shape.canonical.resolve_or_air(raw, &mut tally);
            let x = body.section_x * 16 + i32::from(local[0]);
            let y = body.section_y * 16 + i32::from(local[1]);
            let z = body.section_z * 16 + i32::from(local[2]);
            world.set_block(x, y, z, state.raw());
            world.sync_block_entity(
                x,
                y,
                z,
                block_entity_type(state).map(|kind| kind.raw()),
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

    /// `minecraft:block_break_animation` — a remote break-overlay update.
    fn handle_play_block_break_animation(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: BlockBreakAnimation = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::BlockDestruction {
            entity_id: body.entity_id,
            pos: body.location.0,
            progress: body.destroy_stage as u8,
        })])
    }

    /// `minecraft:explosion`. The affected offsets are authoritative air
    /// writes, while the two trailing particles and sound holder are consumed
    /// to prove the complete body remains aligned.
    fn handle_play_explosion(
        adapter: &V766Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let pos = Vec3::new(
            reader.f64().map_err(dec_err)?,
            reader.f64().map_err(dec_err)?,
            reader.f64().map_err(dec_err)?,
        );
        let radius = reader.f32().map_err(dec_err)?;
        let count = reader.var_i32().map_err(dec_err)?;
        if count < 0 {
            return Err(AdapterError::Decode(format!(
                "explosion affected-block count {count} is negative"
            )));
        }
        // Motion, interaction, two particle ids, and a sound holder each have
        // a mandatory minimum representation after the offset array. Reserve
        // them before accepting the count so offset bytes cannot consume the
        // beginning of that tail.
        const MIN_EXPLOSION_TAIL_BYTES: usize = 3 * size_of::<f32>() + 1 + 2 + 1;
        let offset_bytes = reader.remaining().checked_sub(MIN_EXPLOSION_TAIL_BYTES).ok_or_else(|| {
            AdapterError::Decode("explosion lacks its mandatory particle and sound tail".into())
        })?;
        let count = usize::try_from(count).expect("negative count rejected above");
        if count > offset_bytes / 3 {
            return Err(AdapterError::Decode(format!(
                "explosion affected-block count {count} leaves no mandatory tail"
            )));
        }
        let mut affected_blocks = Vec::with_capacity(count);
        for _ in 0..count {
            affected_blocks.push([
                reader.i8().map_err(dec_err)?,
                reader.i8().map_err(dec_err)?,
                reader.i8().map_err(dec_err)?,
            ]);
        }
        let knockback = Some(Vec3::new(
            f64::from(reader.f32().map_err(dec_err)?),
            f64::from(reader.f32().map_err(dec_err)?),
            f64::from(reader.f32().map_err(dec_err)?),
        ));
        let _block_interaction = reader.var_i32().map_err(dec_err)?;
        skip_explosion_particle(&mut reader, adapter.ctx())?;
        skip_explosion_particle(&mut reader, adapter.ctx())?;
        skip_explosion_sound_holder(&mut reader)?;
        reader.ensure_empty().map_err(dec_err)?;

        let origin_x = pos.x.floor() as i32;
        let origin_y = pos.y.floor() as i32;
        let origin_z = pos.z.floor() as i32;
        let air = adapter.current_shape().air_id;
        let mut changed_sections: BTreeMap<(i32, i32, i32), Vec<[u8; 3]>> = BTreeMap::new();
        for offset in &affected_blocks {
            let x = origin_x.checked_add(i32::from(offset[0])).ok_or_else(|| {
                AdapterError::Decode("explosion x offset overflows world coordinates".into())
            })?;
            let y = origin_y.checked_add(i32::from(offset[1])).ok_or_else(|| {
                AdapterError::Decode("explosion y offset overflows world coordinates".into())
            })?;
            let z = origin_z.checked_add(i32::from(offset[2])).ok_or_else(|| {
                AdapterError::Decode("explosion z offset overflows world coordinates".into())
            })?;
            world.set_block(x, y, z, air);
            world.sync_block_entity(x, y, z, None);
            changed_sections
                .entry((x >> 4, y >> 4, z >> 4))
                .or_default()
                .push([(x & 15) as u8, (y & 15) as u8, (z & 15) as u8]);
        }

        let mut directives = changed_sections
            .into_iter()
            .map(|((x, y, z), blocks)| {
                Directive::Emit(ClientEvent::SectionBlocksChanged {
                    section: SectionPos::new(x, y, z),
                    blocks,
                })
            })
            .collect::<Vec<_>>();
        directives.push(Directive::Emit(ClientEvent::Explosion {
            pos,
            radius,
            affected_blocks,
            knockback,
        }));
        Ok(directives)
    }

    /// `minecraft:kick_disconnect`. Anonymous NBT here, where the
    /// login-state disconnect at this same protocol is still a JSON string.
    fn handle_play_kick_disconnect(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: KickDisconnect = adapter.decode_body(payload)?;
        Ok(vec![Directive::Disconnect(nbt_reason_text(&body.reason))])
    }

    /// `minecraft:update_health`.
    fn handle_play_update_health(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: UpdateHealth = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::HealthChanged {
            health: body.health,
            food: body.food,
            saturation: body.food_saturation,
        })])
    }

    /// `minecraft:spawn_position`. Carries no dimension field, so the level
    /// name comes from the adapter's own record of the most recent join or
    /// respawn.
    fn handle_play_spawn_position(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnPosition = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
            dimension: dimension_id(&adapter.current_dimension())?,
            pos: body.location.0,
            angle: body.angle,
            pitch: 0.0,
        })])
    }

    /// `minecraft:abilities` (clientbound).
    fn handle_play_abilities(
        adapter: &V766Adapter,
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

    /// `minecraft:game_state_change`. The server changes one aspect at a time:
    /// rain starts/stops at reasons `1`/`2`, its intensities are reasons `7`/`8`,
    /// and reason `3` carries a game-mode ordinal.
    fn handle_play_game_state_change(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: GameStateChange = adapter.decode_body_exact(payload)?;
        let directives = match body.reason {
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
            3 => {
                if !body.value.is_finite()
                    || body.value.fract() != 0.0
                    || !(0.0..=3.0).contains(&body.value)
                {
                    return Err(AdapterError::Decode(format!(
                        "game_state_change game-mode value {} is not an ordinal in 0..=3",
                        body.value
                    )));
                }
                vec![Directive::Emit(ClientEvent::GameModeChanged {
                    game_mode: game_mode(body.value as u8)?,
                })]
            }
            7 => vec![Directive::Emit(ClientEvent::WeatherChanged {
                raining: None,
                rain_level: Some(body.value),
                thunder_level: None,
            })],
            8 => vec![Directive::Emit(ClientEvent::WeatherChanged {
                raining: None,
                rain_level: None,
                thunder_level: Some(body.value),
            })],
            _ => Vec::new(),
        };
        Ok(directives)
    }

    /// `minecraft:difficulty`.
    fn handle_play_difficulty(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: DifficultyPacket = adapter.decode_body_exact(payload)?;
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

    /// `minecraft:update_time`.
    fn handle_play_update_time(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: UpdateTime = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TimeChanged {
            world_age: body.age,
            time_of_day: body.time,
        })])
    }

    /// `minecraft:playerlist_header`. Both components are anonymous NBT here,
    /// not the JSON strings the older eras send.
    fn handle_play_playerlist_header(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: PlayerlistHeader = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TabListChanged {
            header: Text::from_nbt(&body.header.0),
            footer: Text::from_nbt(&body.footer.0),
        })])
    }

    /// `minecraft:player_info` in its action-bitmask form.
    fn handle_play_player_info(
        adapter: &V766Adapter,
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
                        AdapterError::Decode(format!("player_info game mode {raw} out of range"))
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
                // Both are later additions than this protocol; the decoder
                // must not read them, so there is nothing to report.
                list_order: None,
                hat_visible: None,
            });
        }
        if updated.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![Directive::Emit(ClientEvent::PlayerListUpdate {
            entries: updated,
        })])
    }

    /// `minecraft:player_remove`.
    fn handle_play_player_remove(
        adapter: &V766Adapter,
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

    /// `minecraft:held_item_slot`.
    fn handle_play_held_item_slot(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: HeldItemSlot = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::HeldSlotChanged {
            slot: i32::from(body.slot),
        })])
    }

    /// `minecraft:close_window`.
    fn handle_play_close_window(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: CloseWindow = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ScreenClosed {
            window_id: i32::from(body.window_id),
        })])
    }

    /// `minecraft:craft_progress_bar`.
    fn handle_play_craft_progress_bar(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: CraftProgressBar = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ContainerData {
            window_id: i32::from(body.window_id),
            property: i32::from(body.property),
            value: i32::from(body.value),
        })])
    }

    /// `minecraft:experience`.
    fn handle_play_experience(
        _adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let progress = reader.f32().map_err(dec_err)?;
        let level = reader.var_i32().map_err(dec_err)?;
        let total = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::ExperienceChanged {
            progress,
            level,
            total,
        })])
    }

    /// `minecraft:vehicle_move`.
    fn handle_play_vehicle_move(
        _adapter: &V766Adapter,
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

    /// `minecraft:camera`.
    fn handle_play_camera(
        _adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let entity_id = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::CameraSet { entity_id })])
    }

    /// `minecraft:update_view_position`.
    fn handle_play_update_view_position(
        _adapter: &V766Adapter,
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

    /// `minecraft:update_view_distance`.
    fn handle_play_update_view_distance(
        _adapter: &V766Adapter,
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

    /// `minecraft:simulation_distance`.
    fn handle_play_simulation_distance(
        _adapter: &V766Adapter,
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

    /// `minecraft:open_sign_entity`. Signs are two-sided throughout this era,
    /// so the face the server opened is on the wire rather than assumed.
    fn handle_play_open_sign_entity(
        adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: OpenSignEntity = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::SignEditorOpened {
            pos: body.location.0,
            is_front_text: body.is_front_text,
        })])
    }

    /// `minecraft:set_title_text`.
    fn handle_play_set_title_text(
        _adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let text = decode_single_nbt_text(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TitleText { text })])
    }

    /// `minecraft:set_title_subtitle`.
    fn handle_play_set_title_subtitle(
        _adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let text = decode_single_nbt_text(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::SubtitleText { text })])
    }

    /// `minecraft:action_bar`. Reported as a game-info chat line, the same
    /// surface `system_chat`'s action-bar flag selects.
    fn handle_play_action_bar(
        _adapter: &V766Adapter,
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

    /// `minecraft:set_title_time`.
    fn handle_play_set_title_time(
        _adapter: &V766Adapter,
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
        _adapter: &V766Adapter,
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

    /// `minecraft:set_ticking_state`.
    fn handle_play_set_ticking_state(
        _adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let tick_rate = reader.f32().map_err(dec_err)?;
        let is_frozen = reader.bool().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::TickingStateChanged {
            tick_rate,
            frozen: is_frozen,
        })])
    }

    /// `minecraft:step_tick`.
    fn handle_play_step_tick(
        _adapter: &V766Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let steps = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::TickingStepped {
            tick_steps: steps,
        })])
    }

    /// `minecraft:bundle_delimiter`. Carries no body: it brackets a run of
    /// packets the client must apply in the same frame.
    fn handle_play_bundle_delimiter(
        _adapter: &V766Adapter,
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
                    "v1-20-6 play dispatch table for protocol {} must build: every clientbound \
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
        "minecraft:block_action",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_block_action,
        ),
    ),
    (
        "minecraft:login",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_login,
        ),
    ),
    (
        "minecraft:respawn",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_respawn,
        ),
    ),
    (
        "minecraft:map_chunk",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_map_chunk,
        ),
    ),
    (
        "minecraft:update_light",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_update_light,
        ),
    ),
    (
        "minecraft:unload_chunk",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_unload_chunk,
        ),
    ),
    (
        "minecraft:keep_alive",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_keep_alive,
        ),
    ),
    (
        "minecraft:ping",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_ping,
        ),
    ),
    (
        "minecraft:system_chat",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_system_chat,
        ),
    ),
    (
        "minecraft:player_chat",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_player_chat,
        ),
    ),
    (
        "minecraft:profileless_chat",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_profileless_chat,
        ),
    ),
    (
        "minecraft:hide_message",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_hide_message,
        ),
    ),
    (
        "minecraft:position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_position,
        ),
    ),
    (
        "minecraft:chunk_batch_start",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_chunk_batch_start,
        ),
    ),
    (
        "minecraft:chunk_batch_finished",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_chunk_batch_finished,
        ),
    ),
    (
        "minecraft:start_configuration",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_start_configuration,
        ),
    ),
    (
        "minecraft:spawn_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_spawn_entity,
        ),
    ),
    (
        "minecraft:spawn_entity_experience_orb",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_spawn_entity_experience_orb,
        ),
    ),
    (
        "minecraft:rel_entity_move",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_rel_entity_move,
        ),
    ),
    (
        "minecraft:entity_move_look",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_entity_move_look,
        ),
    ),
    (
        "minecraft:entity_look",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_entity_look,
        ),
    ),
    (
        "minecraft:entity_teleport",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_entity_teleport,
        ),
    ),
    (
        "minecraft:entity_velocity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_entity_velocity,
        ),
    ),
    (
        "minecraft:entity_head_rotation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_entity_head_rotation,
        ),
    ),
    (
        "minecraft:entity_destroy",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_entity_destroy,
        ),
    ),
    (
        "minecraft:entity_status",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_entity_status,
        ),
    ),
    (
        "minecraft:explosion",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_explosion,
        ),
    ),
    (
        "minecraft:animation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_animation,
        ),
    ),
    (
        "minecraft:entity_metadata",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_entity_metadata,
        ),
    ),
    (
        "minecraft:entity_equipment",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_entity_equipment,
        ),
    ),
    (
        "minecraft:entity_update_attributes",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_entity_update_attributes,
        ),
    ),
    (
        "minecraft:attach_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_attach_entity,
        ),
    ),
    (
        "minecraft:set_passengers",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_set_passengers,
        ),
    ),
    (
        "minecraft:collect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_collect,
        ),
    ),
    (
        "minecraft:entity_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_entity_effect,
        ),
    ),
    (
        "minecraft:remove_entity_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_remove_entity_effect,
        ),
    ),
    (
        "minecraft:block_change",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_block_change,
        ),
    ),
    (
        "minecraft:multi_block_change",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_multi_block_change,
        ),
    ),
    (
        "minecraft:block_break_animation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_block_break_animation,
        ),
    ),
    (
        "minecraft:kick_disconnect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_kick_disconnect,
        ),
    ),
    (
        "minecraft:update_health",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_update_health,
        ),
    ),
    (
        "minecraft:spawn_position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_spawn_position,
        ),
    ),
    (
        "minecraft:abilities",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_abilities,
        ),
    ),
    (
        "minecraft:game_state_change",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_game_state_change,
        ),
    ),
    (
        "minecraft:difficulty",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_difficulty,
        ),
    ),
    (
        "minecraft:update_time",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_update_time,
        ),
    ),
    (
        "minecraft:playerlist_header",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_playerlist_header,
        ),
    ),
    (
        "minecraft:player_info",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_player_info,
        ),
    ),
    (
        "minecraft:player_remove",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_player_remove,
        ),
    ),
    (
        "minecraft:held_item_slot",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_held_item_slot,
        ),
    ),
    (
        "minecraft:close_window",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_close_window,
        ),
    ),
    (
        "minecraft:craft_progress_bar",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_craft_progress_bar,
        ),
    ),
    (
        "minecraft:experience",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_experience,
        ),
    ),
    (
        "minecraft:vehicle_move",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_vehicle_move,
        ),
    ),
    (
        "minecraft:camera",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_camera,
        ),
    ),
    (
        "minecraft:update_view_position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_update_view_position,
        ),
    ),
    (
        "minecraft:update_view_distance",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_update_view_distance,
        ),
    ),
    (
        "minecraft:simulation_distance",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_simulation_distance,
        ),
    ),
    (
        "minecraft:open_sign_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_open_sign_entity,
        ),
    ),
    (
        "minecraft:set_title_text",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_set_title_text,
        ),
    ),
    (
        "minecraft:set_title_subtitle",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_set_title_subtitle,
        ),
    ),
    (
        "minecraft:action_bar",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_action_bar,
        ),
    ),
    (
        "minecraft:set_title_time",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_set_title_time,
        ),
    ),
    (
        "minecraft:clear_titles",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_clear_titles,
        ),
    ),
    (
        "minecraft:set_ticking_state",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_set_ticking_state,
        ),
    ),
    (
        "minecraft:step_tick",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_step_tick,
        ),
    ),
    (
        "minecraft:bundle_delimiter",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V766Adapter::handle_play_bundle_delimiter,
        ),
    ),
];

/// Every clientbound play packet this era decodes nothing for, with the
/// reason. The dispatch table refuses to build unless this list plus
/// [`CLIENTBOUND`] covers every id in the protocol's own `ENTRIES`, so a
/// packet cannot be silently dropped by omission.
static IGNORED: &[lodestone_core::dispatch::IGNORED] = &[
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:statistics",
        "the statistics screen has no surface in this client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:acknowledge_player_digging",
        "block-prediction acknowledgement needs a client-side prediction queue this adapter does not keep",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:tile_entity_data",
        "a block entity's payload is modelled only where a column delivers it; a standalone update needs a per-type NBT model",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:boss_bar",
        "the boss bar surface is not wired for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:chunk_biomes",
        "the biome-only column update needs a partial-column merge the world store does not offer",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:tab_complete",
        "command suggestions are sent but the response surface is not wired for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:declare_commands",
        "the command tree has no consumer for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:window_items",
        "an item stack at this protocol names its item by a numeric id, and no 766 item-id registry exists to resolve one into a canonical key",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_slot",
        "same missing 766 item-id registry as window_items",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:cookie_request",
        "server cookies have no store in this client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_cooldown",
        "the item-cooldown overlay is not wired for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:chat_suggestions",
        "server-supplied chat completions have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:custom_payload",
        "plugin channels are opaque to this client; the brand is announced during configuration and nothing reads a reply",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:damage_event",
        "the typed damage cue has no consumer for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:debug_sample",
        "the debug-sample channel has no consumer",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:open_horse_window",
        "the mount screen has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:hurt_animation",
        "the directional hurt tilt has no draw path for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:initialize_world_border",
        "the world border has no draw path for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:world_event",
        "level events (door sounds, break particles) have no consumer for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:world_particles",
        "particles have no emitter path wired for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:map",
        "map item rendering is not wired for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:trade_list",
        "the merchant screen has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:open_book",
        "the book screen has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:open_window",
        "the screen carries a menu type id, and resolving it needs the same 766 registry window_items lacks",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:ping_response",
        "the play-state ping is answered by the ping handler; a status-style response is never solicited here",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:craft_recipe_response",
        "recipe placement is not sent, so nothing can receive its answer",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:end_combat_event",
        "combat timers have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:enter_combat_event",
        "combat timers have no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:death_combat_event",
        "the death screen is driven by health rather than this packet in this client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:face_player",
        "server-driven look-at has no consumer for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:unlock_recipes",
        "the recipe book's unlocked set has no store for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:reset_score",
        "the scoreboard surface is not wired for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:remove_resource_pack",
        "server resource packs are not applied by this client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:add_resource_pack",
        "server resource packs are not applied by this client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:select_advancement_tab",
        "the advancements screen has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:server_data",
        "the server MOTD and icon push has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:world_border_center",
        "the world border has no draw path for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:world_border_lerp_size",
        "the world border has no draw path for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:world_border_size",
        "the world border has no draw path for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:world_border_warning_delay",
        "the world border has no draw path for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:world_border_warning_reach",
        "the world border has no draw path for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:scoreboard_display_objective",
        "the scoreboard surface is not wired for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:scoreboard_objective",
        "the scoreboard surface is not wired for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:teams",
        "the team surface is not wired for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:scoreboard_score",
        "the scoreboard surface is not wired for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:entity_sound_effect",
        "sound events name a registry id, and no 766 sound registry table exists to resolve one",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:sound_effect",
        "sound events name a registry id, and no 766 sound registry table exists to resolve one",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:stop_sound",
        "no sound is started for this era, so none can be stopped",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:store_cookie",
        "server cookies have no store in this client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:nbt_query_response",
        "no NBT query is sent, so nothing can receive its answer",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:transfer",
        "server-to-server transfer has no consumer in this client",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:advancements",
        "the advancements screen has no surface for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:declare_recipes",
        "the recipe set has no store for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:tags",
        "block and item tags have no consumer for this era",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:set_projectile_power",
        "the projectile-power cue has no consumer for this era",
    ),
];

impl VersionAdapter for V766Adapter {
    fn protocol_version(&self) -> i32 {
        self.protocol
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["1.20.5", "1.20.6"]
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
            protocol_version: self.protocol,
            server_host: server.host.clone(),
            server_port: server.port,
            next_state: NEXT_STATE_LOGIN,
        };
        // The profile UUID is **not** optional at this protocol. The eras
        // below write a presence boolean and then, usually, nothing; here the
        // sixteen bytes are read unconditionally, so an offline-mode client
        // still has to put a uuid on the wire. The server ignores its value
        // in offline mode but not its length.
        let login_start = LoginStart {
            username: profile.username.clone(),
            uuid: profile.uuid,
        };
        Ok(vec![
            self.send(self.ids().handshake_set_protocol, &handshake)?,
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
            // server's pending list.
            ClientAction::SendChat { text } => {
                let mut body = ChatMessage::unsigned(text.clone());
                body.last_seen_offset = self.take_pending_ack();
                Ok(Some((self.ids().chat_message, self.encode_body(&body)?)))
            }
            // A command does **not** carry that tail at this protocol: the
            // unsigned command packet is one string and nothing else, and
            // the signed form is a different packet. So the pending count is
            // deliberately left standing here rather than silently dropped —
            // the next chat message or acknowledgement drains it.
            ClientAction::SendCommand { command } => {
                let body = ChatCommand {
                    command: command.clone(),
                };
                Ok(Some((self.ids().chat_command, self.encode_body(&body)?)))
            }
            // The standalone drain. Without it, a client that reads chat and
            // never writes it grows the server's pending list until the
            // server disconnects it.
            ClientAction::ChatAck { offset } => {
                let combined = offset.saturating_add(self.take_pending_ack());
                let body = MessageAcknowledgement { count: combined };
                Ok(Some((
                    self.ids().message_acknowledgement,
                    self.encode_body(&body)?,
                )))
            }
            ClientAction::Move {
                pos,
                rotation,
                on_ground,
                // This protocol's movement packets carry only `onGround`;
                // the horizontal-collision bit is a later addition, so there
                // is nothing to forward it into.
                horizontal_collision: _,
            } => self.select_move_packet(*pos, *rotation, *on_ground),
            ClientAction::SwingArm { hand } => {
                let body = ServerboundArmAnimation {
                    hand: hand_ordinal(*hand),
                };
                Ok(Some((self.ids().arm_animation, self.encode_body(&body)?)))
            }

            // Block breaking rides on `block_dig` statuses 0/1/2, carrying
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
                let body = BlockDig {
                    status,
                    location: Position(*pos),
                    face: face_ordinal(*face) as i8,
                    sequence: *sequence,
                };
                Ok(Some((self.ids().block_dig, self.encode_body(&body)?)))
            }
            // Dropping, releasing and the off-hand swap ride on the same
            // packet's statuses 3 through 6. None of them predicts a block
            // change, so there is nothing for the server to acknowledge and
            // the sequence is zero.
            ClientAction::DropSelectedItemStack => Ok(Some((
                self.ids().block_dig,
                self.encode_body(&BlockDig {
                    status: 3,
                    location: Position::new(0, 0, 0),
                    face: 0,
                    sequence: 0,
                })?,
            ))),
            ClientAction::DropSelectedItem => Ok(Some((
                self.ids().block_dig,
                self.encode_body(&BlockDig {
                    status: 4,
                    location: Position::new(0, 0, 0),
                    face: 0,
                    sequence: 0,
                })?,
            ))),
            ClientAction::ReleaseUseItem => Ok(Some((
                self.ids().block_dig,
                self.encode_body(&BlockDig {
                    status: 5,
                    location: Position::new(0, 0, 0),
                    face: 0,
                    sequence: 0,
                })?,
            ))),
            ClientAction::SwapItemWithOffhand => Ok(Some((
                self.ids().block_dig,
                self.encode_body(&BlockDig {
                    status: 6,
                    location: Position::new(0, 0, 0),
                    face: 0,
                    sequence: 0,
                })?,
            ))),

            ClientAction::UseItemOn {
                hand,
                pos,
                face,
                cursor,
                inside_block,
                sequence,
            } => {
                let body = BlockPlace {
                    hand: hand_ordinal(*hand),
                    location: Position(*pos),
                    direction: face_ordinal(*face),
                    cursor_x: cursor.x,
                    cursor_y: cursor.y,
                    cursor_z: cursor.z,
                    inside_block: *inside_block,
                    sequence: *sequence,
                };
                Ok(Some((self.ids().block_place, self.encode_body(&body)?)))
            }
            ClientAction::UseItem {
                hand,
                // The model's rotation has no field on this era's packet.
                rotation: _,
                sequence,
            } => {
                let body = UseItem {
                    hand: hand_ordinal(*hand),
                    sequence: *sequence,
                };
                Ok(Some((self.ids().use_item, self.encode_body(&body)?)))
            }

            // Each interaction kind is a distinct wire shape behind one
            // packet id, selected by the `mouse` value.
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
                    Ok(Some((self.ids().use_entity, self.encode_body(&body)?)))
                }
                EntityInteraction::Interact { hand } => {
                    let body = UseEntityInteract {
                        target: *entity_id,
                        mouse: 0,
                        hand: hand_ordinal(*hand),
                        sneaking: *sneaking,
                    };
                    Ok(Some((self.ids().use_entity, self.encode_body(&body)?)))
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
                    Ok(Some((self.ids().use_entity, self.encode_body(&body)?)))
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
                let body = EntityAction {
                    entity_id: *entity_id,
                    action_id,
                    jump_boost,
                };
                Ok(Some((self.ids().entity_action, self.encode_body(&body)?)))
            }

            ClientAction::ContainerClose { window_id } => {
                let body = ServerboundCloseWindow {
                    window_id: *window_id as u8,
                };
                Ok(Some((self.ids().close_window, self.encode_body(&body)?)))
            }
            ClientAction::SetCarriedItem { slot } => {
                let body = ServerboundHeldItemSlot { slot: *slot as i16 };
                Ok(Some((self.ids().held_item_slot, self.encode_body(&body)?)))
            }
            ClientAction::SetCreativeModeSlot { slot, item } => {
                if item.is_some() {
                    return Err(AdapterError::Unsupported(
                        "this era's SetCreativeModeSlot with an item requires a \
                         ResourceKey -> numeric item-id registry for protocol 766, which does \
                         not exist yet"
                            .to_owned(),
                    ));
                }
                let body = SetCreativeSlot {
                    slot: *slot as i16,
                    item: Slot::Empty,
                };
                Ok(Some((
                    self.ids().set_creative_slot,
                    self.encode_body(&body)?,
                )))
            }
            // The model's click shape is this era's exactly — a state id, the
            // client's own view of every slot the click changed, and the
            // resulting cursor stack — so a click that moves nothing but
            // empty slots encodes faithfully. A click carrying a real stack
            // still needs the numeric item id, which is refused rather than
            // guessed: a wrong id is accepted by the server and applied.
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
                         ResourceKey -> numeric item-id registry for protocol 766, which does \
                         not exist yet"
                            .to_owned(),
                    ));
                }
                let body = WindowClick {
                    window_id: *window_id as u8,
                    state_id: state_id.as_wire(),
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
                                item: Slot::Empty,
                            })
                        })
                        .collect::<Result<Vec<_>, AdapterError>>()?,
                    cursor_item: Slot::Empty,
                };
                Ok(Some((self.ids().window_click, self.encode_body(&body)?)))
            }
            ClientAction::ContainerButtonClick {
                window_id,
                button_id,
            } => {
                let window_id = i8::try_from(*window_id).map_err(|_| {
                    AdapterError::Encode(format!("window id {window_id} overflows i8"))
                })?;
                let enchantment = i8::try_from(*button_id).map_err(|_| {
                    AdapterError::Encode(format!("button id {button_id} overflows i8"))
                })?;
                let body = EnchantItem {
                    window_id,
                    enchantment,
                };
                Ok(Some((self.ids().enchant_item, self.encode_body(&body)?)))
            }

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
                    // The particle-status field is a later addition than this
                    // protocol; writing it appends a byte the server reads as
                    // the next packet's length prefix.
                    particle_status: _,
                } = settings;
                let body = Settings {
                    locale: locale.clone(),
                    view_distance: *view_distance,
                    chat_flags: chat_mode_value(*chat_mode),
                    chat_colors: *chat_colors,
                    skin_parts: skin_parts_bits(*skin_parts),
                    main_hand: main_hand_value(*main_hand),
                    text_filtering: *text_filtering,
                    allow_server_listing: *allow_server_listing,
                };
                Ok(Some((self.ids().settings, self.encode_body(&body)?)))
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
                Ok(Some((self.ids().abilities, self.encode_body(&body)?)))
            }
            // The reply names the pack by uuid at this protocol, because a
            // server may have several applied at once and pushes or removes
            // them individually.
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
                Ok(Some((
                    self.ids().resource_pack_receive,
                    self.encode_body(&body)?,
                )))
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
                let body = Spectate { target: *target };
                Ok(Some((self.ids().spectate, self.encode_body(&body)?)))
            }
            ClientAction::CommandSuggestion { id, command } => {
                let mut writer = Writer::default();
                writer.var_i32(*id);
                writer.string(command);
                Ok(Some((self.ids().tab_complete, writer.into_vec())))
            }
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
                Ok(Some((self.ids().recipe_book, self.encode_body(&body)?)))
            }

            // Genuinely absent at this protocol.
            ClientAction::Stab => Err(AdapterError::Unsupported(
                "this era has no dedicated off-hand attack packet".to_owned(),
            )),
            ClientAction::SetPlayerInput(_) => Err(AdapterError::Unsupported(
                "this era has no player-input packet".to_owned(),
            )),
            ClientAction::EndClientTick => Err(AdapterError::Unsupported(
                "this era has no client_tick_end packet".to_owned(),
            )),
            ClientAction::PlayerLoaded => Err(AdapterError::Unsupported(
                "this era has no player_loaded packet".to_owned(),
            )),
            ClientAction::SelectBundleItem { .. } => Err(AdapterError::Unsupported(
                "this era predates bundles".to_owned(),
            )),
            // The spectate packet needs the target's uuid, which this action
            // does not carry; the id-to-uuid map lives above this adapter.
            ClientAction::SpectatorAction { .. } => Err(AdapterError::Unsupported(
                "this era's spectate packet needs a target uuid; SpectatorAction carries only a \
                 network entity id (use TeleportToEntity, which already carries the uuid)"
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
                "this era's beacon packet names effects by registry id, and no 766 mob-effect \
                 id table exists to resolve one"
                    .to_owned(),
            )),

            _ => Ok(None),
        }
    }
}

/// Constructs an adapter speaking [`PROTOCOL`].
#[must_use]
pub fn adapter() -> V766Adapter {
    V766Adapter::new()
}

/// Constructs an adapter for one of [`PROTOCOLS`].
///
/// # Panics
///
/// Panics for a protocol outside [`PROTOCOLS`] — see [`ids_for`].
#[must_use]
pub fn adapter_for(protocol: i32) -> V766Adapter {
    assert!(
        PROTOCOLS.contains(&protocol),
        "protocol {protocol} is outside this family's PROTOCOLS ({PROTOCOLS:?}); \
         callers must test membership before constructing"
    );
    V766Adapter::for_protocol(protocol)
}

#[cfg(test)]
mod movement_tests {
    use super::*;

    #[test]
    fn poisoned_movement_state_is_recovered() {
        let adapter = V766Adapter::new();
        let guard = adapter.movement.lock().expect("fresh movement state");
        let state = recover_movement_state(Err(PoisonError::new(guard)));
        drop(state);

        assert_eq!(
            adapter
                .select_move_packet(Vec3::new(1.0, 0.0, 0.0), Rotation::default(), false)
                .expect("poisoned state is recovered")
                .map(|(id, _)| id),
            Some(adapter.ids().position)
        );
    }
}

#[cfg(test)]
mod mob_effect_tests {
    use super::*;
    use lodestone_world::World;

    fn encoded_update(adapter: &V766Adapter, effect_id: i32) -> Vec<u8> {
        adapter
            .encode_body(&EntityEffect {
                entity_id: 42,
                effect_id,
                amplifier: 0,
                duration: 40,
                flags: 0,
            })
            .expect("entity effect encodes")
    }

    fn encoded_remove(adapter: &V766Adapter, effect_id: i32) -> Vec<u8> {
        adapter
            .encode_body(&RemoveEntityEffect {
                entity_id: 42,
                effect_id,
            })
            .expect("remove entity effect encodes")
    }

    #[test]
    fn modern_zero_based_speed_id_resolves_for_update_and_remove() {
        let adapter = V766Adapter::new();
        let mut world = World::new();
        let applied = V766Adapter::handle_play_entity_effect(
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

        let removed = V766Adapter::handle_play_remove_entity_effect(
            &adapter,
            &mut world,
            &encoded_remove(&adapter, 0),
        )
        .expect("known modern effect removal decodes");
        let [Directive::Emit(ClientEvent::MobEffectRemoved { effect, .. })] = removed.as_slice()
        else {
            panic!("known effect did not emit one removal event: {removed:?}");
        };
        assert_eq!(effect.path(), "speed");
    }

    #[test]
    fn unknown_modern_effect_ids_are_rejected_at_packet_ingress() {
        let unknown_ids = [-1, lodestone_data::mob_effects::MOB_EFFECT_COUNT as i32];
        let adapter = V766Adapter::new();
        for effect_id in unknown_ids {
            let mut world = World::new();
            let error = V766Adapter::handle_play_entity_effect(
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

            let error = V766Adapter::handle_play_remove_entity_effect(
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
// part of the crate's public API, so an integration test under `tests/`
// cannot name them. Exposing them solely so an external file could reach them
// would leak internal representation for no benefit over a unit-test module
// here.
#[cfg(test)]
mod dispatch_coverage_tests {
    use super::*;
    use lodestone_core::Nbt;
    use lodestone_world::{ChunkColumn, ColumnLight, World};

    fn decode_play_with_world(
        world: &mut World,
        packet_id: i32,
        body: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        V766Adapter::new()
            .handle_packet(world, ConnectionState::Play, packet_id, body)
    }

    fn decode_play(packet_id: i32, body: &[u8]) -> Vec<Directive> {
        decode_play_with_world(&mut World::new(), packet_id, body)
            .expect("the independent packet bytes decode")
    }

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
    /// construction must fail *by name* — which is what proves the check
    /// above is doing anything at all.
    #[test]
    fn negative_control_dropping_one_ignored_entry_fails_construction() {
        let position = IGNORED
            .iter()
            .position(|entry| entry.name == "minecraft:tags")
            .expect("minecraft:tags is an IGNORED entry");
        let mut ignored_missing_one: Vec<lodestone_core::dispatch::IGNORED> = IGNORED.to_vec();
        let removed = ignored_missing_one.remove(position);
        assert_eq!(removed.name, "minecraft:tags");
        let entries = ids_for(PROTOCOL).play_clientbound_entries;
        let tags_id = entries
            .iter()
            .find(|(name, _)| *name == "minecraft:tags")
            .map(|(_, id)| *id)
            .expect("this era carries minecraft:tags");
        let table = lodestone_core::dispatch::Table::build(
            PROTOCOL,
            entries,
            CLIENTBOUND,
            &ignored_missing_one,
        );
        assert_eq!(
            table.err(),
            Some(lodestone_core::dispatch::DispatchError::UnlistedId {
                name: "minecraft:tags",
                id: tags_id,
            }),
            "dropping the minecraft:tags IGNORED entry must fail construction on that packet"
        );
    }

    /// The ids this crate speaks are its own, not a neighbouring era's.
    ///
    /// The expected numbers are written literally on one side and read out of
    /// the generated table on the other, so the test cannot pass by reading
    /// the same value twice. Both probes sit at different ids in the 1.19 era,
    /// so a table silently inherited from there would fail here.
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
            (43, 38),
            "these are 766's ids; 762's are (40, 35)"
        );
    }

    /// The join capture's own `login` packet decodes through this era's
    /// struct and consumes every byte.
    ///
    /// The bytes are a real 766 server's, recorded off the wire, so nothing
    /// in this crate produced the expectation: entity id `1`, three worlds,
    /// dimension **index** `0`, a flat world, and no death location.
    #[test]
    fn the_captured_join_packet_decodes_exactly() {
        // play packet id 43, body verbatim.
        let body: Vec<u8> = [
            0x00, 0x00, 0x00, 0x01, 0x00, 0x03, 0x13,
        ]
        .into_iter()
        .chain(b"minecraft:overworld".iter().copied())
        .chain([0x14])
        .chain(b"minecraft:the_nether".iter().copied())
        .chain([0x11])
        .chain(b"minecraft:the_end".iter().copied())
        .chain([0x14, 0x0a, 0x0a, 0x00, 0x01, 0x00, 0x00, 0x13])
        .chain(b"minecraft:overworld".iter().copied())
        .chain([
            0x49, 0x02, 0x13, 0xf5, 0x22, 0x67, 0x12, 0x92, 0x00, 0xff, 0x00, 0x01, 0x00, 0x00,
            0x00,
        ])
        .collect();
        let join: JoinGame = lodestone_core::decode_body_exact(&body, Ctx { version: PROTOCOL })
            .expect("the captured join packet decodes with no bytes left over");
        assert_eq!(join.entity_id, 1);
        assert!(!join.is_hardcore);
        assert_eq!(join.world_names.len(), 3);
        assert_eq!(join.max_players, 20);
        assert_eq!(join.view_distance, 10);
        assert_eq!(join.simulation_distance, 10);
        assert_eq!(join.world_state.dimension, 0);
        assert_eq!(join.world_state.world_name, "minecraft:overworld");
        assert_eq!(join.world_state.game_mode, 0);
        assert_eq!(join.world_state.previous_game_mode, 0xff);
        assert!(join.world_state.is_flat);
        assert!(!join.world_state.has_death_location);
        assert_eq!(join.world_state.portal_cooldown, 0);
        assert!(!join.enforces_secure_chat);
    }

    /// The record values below are manually assembled from protocol 766's
    /// `state << 12 | x << 8 | z << 4 | y` layout. In particular, the first
    /// record is `0x1234`, encoded as the two-byte **VarInt** `b4 24`. The
    /// protocol definition names this record type `varint`; the test keeps the
    /// count and both record bytes literal rather than calling our encoder.
    #[test]
    fn multi_block_change_reads_signed_section_bits_and_varint_records() {
        // section (-2, -4, 3), two records: state 1 at (2, 4, 3), then state 2
        // at (15, 1, 0). The first eight bytes are the independently packed
        // 22/22/20-bit coordinate long, in x/z/y field order.
        let body = [
            0xff, 0xff, 0xf8, 0x00, 0x00, 0x3f, 0xff, 0xfc, 0x02, 0xb4, 0x24, 0x81, 0x5e,
        ];
        assert_eq!(
            decode_play(73, &body),
            vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
                section: SectionPos::new(-2, -4, 3),
                blocks: vec![[2, 4, 3], [15, 1, 0]],
            })]
        );
    }

    #[test]
    fn block_break_animation_preserves_the_raw_clear_stage() {
        // entity 300 (`ac 02`), then packed position (1, 64, -2), then -1.
        let body = [
            0xac, 0x02, 0x00, 0x00, 0x00, 0x7f, 0xff, 0xff, 0xe0, 0x40, 0xff,
        ];
        assert_eq!(
            decode_play(6, &body),
            vec![Directive::Emit(ClientEvent::BlockDestruction {
                entity_id: 300,
                pos: lodestone_model::BlockPos::new(1, 64, -2),
                progress: 255,
            })]
        );
    }

    #[test]
    fn game_state_weather_reasons_update_only_the_changed_aspect() {
        let cases: [(u8, f32, ClientEvent); 4] = [
            (
                1,
                0.0,
                ClientEvent::WeatherChanged {
                    raining: Some(true),
                    rain_level: None,
                    thunder_level: None,
                },
            ),
            (
                2,
                0.0,
                ClientEvent::WeatherChanged {
                    raining: Some(false),
                    rain_level: None,
                    thunder_level: None,
                },
            ),
            (
                7,
                0.625,
                ClientEvent::WeatherChanged {
                    raining: None,
                    rain_level: Some(0.625),
                    thunder_level: None,
                },
            ),
            (
                8,
                0.375,
                ClientEvent::WeatherChanged {
                    raining: None,
                    rain_level: None,
                    thunder_level: Some(0.375),
                },
            ),
        ];
        for (reason, value, expected) in cases {
            let mut body = vec![reason];
            body.extend(value.to_be_bytes());
            assert_eq!(decode_play(34, &body), vec![Directive::Emit(expected)]);
        }
    }

    #[test]
    fn game_state_game_mode_requires_a_finite_integral_ordinal() {
        assert_eq!(
            decode_play(34, &[0x03, 0x40, 0x40, 0x00, 0x00]),
            vec![Directive::Emit(ClientEvent::GameModeChanged {
                game_mode: GameMode::Spectator,
            })]
        );

        // -1, 1.5, 4, and a quiet NaN are all distinct invalid f32 shapes.
        for body in [
            [0x03, 0xbf, 0x80, 0x00, 0x00],
            [0x03, 0x3f, 0xc0, 0x00, 0x00],
            [0x03, 0x40, 0x80, 0x00, 0x00],
            [0x03, 0x7f, 0xc0, 0x00, 0x00],
        ] {
            assert!(
                decode_play_with_world(&mut World::new(), 34, &body).is_err(),
                "malformed game-mode body {body:02x?} must not coerce into a mode"
            );
        }
    }

    #[test]
    fn explosion_consumes_particle_and_sound_holder_tail() {
        // Centre (1.5, -2.25, 3.75), radius 4, two signed offsets, motion
        // (1, -0.5, 2), interaction 2. The last three bytes are two simple
        // particle ids and a registry sound holder reference.
        let body = [
            0x3f, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x02, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x40, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x80, 0x00, 0x00,
            0x02, 0xfe, 0x03, 0xf8, 0x04, 0xfb, 0x06, 0x3f, 0x80, 0x00, 0x00, 0xbf, 0x00, 0x00,
            0x00, 0x40, 0x00, 0x00, 0x00, 0x02, 0x15, 0x16, 0x01,
        ];
        assert_eq!(
            decode_play(32, &body),
            vec![
                Directive::Emit(ClientEvent::SectionBlocksChanged {
                    section: SectionPos::new(-1, 0, -1),
                    blocks: vec![[15, 0, 11]],
                }),
                Directive::Emit(ClientEvent::SectionBlocksChanged {
                    section: SectionPos::new(0, -1, 0),
                    blocks: vec![[5, 8, 9]],
                }),
                Directive::Emit(ClientEvent::Explosion {
                    pos: Vec3::new(1.5, -2.25, 3.75),
                    radius: 4.0,
                    affected_blocks: vec![[-2, 3, -8], [4, -5, 6]],
                    knockback: Some(Vec3::new(1.0, -0.5, 2.0)),
                }),
            ]
        );
    }

    #[test]
    fn explosion_rejects_a_missing_or_overrun_mandatory_tail() {
        // These bytes are the complete fixed prefix and two offsets from the
        // preceding test, without either particle head or sound-holder head.
        let missing_tail = [
            0x3f, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x02, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x40, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x80, 0x00, 0x00,
            0x02, 0xfe, 0x03, 0xf8, 0x04, 0xfb, 0x06, 0x3f, 0x80, 0x00, 0x00, 0xbf, 0x00, 0x00,
            0x00, 0x40, 0x00, 0x00, 0x00, 0x02,
        ];
        assert!(
            decode_play_with_world(&mut World::new(), 32, &missing_tail).is_err(),
            "an explosion must carry both particle heads and its sound holder"
        );

        // Identical complete packet except that count says there are three
        // offsets. A prefix-only decoder would consume the first three motion
        // bytes as the final offset and accept the false boundary.
        let mut count_eats_prefix = missing_tail.to_vec();
        count_eats_prefix.extend([0x15, 0x16, 0x01]);
        count_eats_prefix[28] = 0x03;
        assert!(
            decode_play_with_world(&mut World::new(), 32, &count_eats_prefix).is_err(),
            "the offset count must reserve the mandatory tail"
        );
    }

    #[test]
    fn explosion_skips_parameterised_particle_options_exactly() {
        // First particle is id 1 (block) with state id 0; the next is simple
        // id 22, followed by sound-holder reference 0. If the block option is
        // not consumed exactly, that option becomes the second particle head.
        let body = [
            &[0x00_u8; 24][..],
            &[0x3f, 0x80, 0x00, 0x00], // radius 1
            &[0x00],                   // no affected offsets
            &[0x00; 12],               // zero motion
            &[0x00],                   // block interaction
            &[0x01, 0x00, 0x16, 0x01], // block(state 0), explosion, sound 0
        ]
        .concat();
        assert_eq!(
            decode_play(32, &body),
            vec![Directive::Emit(ClientEvent::Explosion {
                pos: Vec3::new(0.0, 0.0, 0.0),
                radius: 1.0,
                affected_blocks: Vec::new(),
                knockback: Some(Vec3::new(0.0, 0.0, 0.0)),
            })]
        );
    }

    #[test]
    fn explosion_skips_effect_flash_and_instant_effect_without_options() {
        // The local 766 schema gives particle ids 15, 39, and 43 no option
        // payload. Each tail combines two of those ids with sound holder 1.
        for tail in [[0x0f, 0x27, 0x01], [0x2b, 0x0f, 0x01]] {
            let body = [
                &[0x00_u8; 24][..],
                &[0x3f, 0x80, 0x00, 0x00], // radius 1
                &[0x00],                   // no affected offsets
                &[0x00; 12],               // zero motion
                &[0x00],                   // block interaction
                &tail,
            ]
            .concat();
            assert_eq!(
                decode_play(32, &body),
                vec![Directive::Emit(ClientEvent::Explosion {
                    pos: Vec3::new(0.0, 0.0, 0.0),
                    radius: 1.0,
                    affected_blocks: Vec::new(),
                    knockback: Some(Vec3::new(0.0, 0.0, 0.0)),
                })]
            );
        }
    }

    #[test]
    fn explosion_removes_loaded_blocks_and_their_block_entities() {
        // Centre zero, one offset (1, 2, 3), zero motion and interaction,
        // followed by two simple particles and a registry sound holder.
        let body = [
            &[0x00_u8; 24][..],
            &[0x3f, 0x80, 0x00, 0x00], // radius 1
            &[0x01, 0x01, 0x02, 0x03], // one offset at (1, 2, 3)
            &[0x00; 12],               // zero motion
            &[0x00],                   // block interaction
            &[0x15, 0x16, 0x01],       // two simple particles, sound 0
        ]
        .concat();
        let adapter = V766Adapter::new();
        let shape = adapter.current_shape();
        let mut column = ChunkColumn::new(
            shape.min_y,
            shape.section_count,
            shape.block_kind,
            shape.biome_kind,
            shape.air_id,
            shape.biome_id,
        );
        column.set_block(1, 2, 3, 777);
        let mut world = World::new();
        world.load(
            WorldChunkPos::new(0, 0),
            LoadedChunk::new(
                column,
                ColumnLight::new(shape.section_count),
                Heightmaps::new(),
                Vec::new(),
            ),
        );
        world.set_block_entity(1, 2, 3, 42, Nbt::End);

        let directives = adapter
            .handle_packet(&mut world, ConnectionState::Play, 32, &body)
            .expect("complete explosion decodes and applies");
        assert_eq!(world.block_state_at(1, 2, 3), Some(shape.air_id));
        let loaded = world
            .unload(WorldChunkPos::new(0, 0))
            .expect("fixture chunk stays loaded");
        assert!(
            loaded.block_entities.is_empty(),
            "the air write must clear the removed block's block entity"
        );
        assert_eq!(
            directives,
            vec![
                Directive::Emit(ClientEvent::SectionBlocksChanged {
                    section: SectionPos::new(0, 0, 0),
                    blocks: vec![[1, 2, 3]],
                }),
                Directive::Emit(ClientEvent::Explosion {
                    pos: Vec3::new(0.0, 0.0, 0.0),
                    radius: 1.0,
                    affected_blocks: vec![[1, 2, 3]],
                    knockback: Some(Vec3::new(0.0, 0.0, 0.0)),
                }),
            ]
        );
    }
}
