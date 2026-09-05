//! [`VersionAdapter`] implementation driving the protocol 340 join flow.

use std::collections::HashMap;
use std::sync::{Arc, LockResult, Mutex, MutexGuard, PoisonError};

use lodestone_core::{Ctx, Decode, Encode, ProtocolRange, Reader, Writer};
use lodestone_data::block_entity_types::block_entity_type;
use lodestone_data::block_states;
use lodestone_data::mob_effects::{mob_effect_name_for, MobEffectId};
use lodestone_model::{
    AdapterError, AnimationAction, BlockActionKind, BlockFace, BossAction, BossColor, BossOverlay,
    ChatKind, ChatMode, ChunkPos, ClientAction, ClientEvent, ClientSettings, CollisionRule,
    ConnectionState, Difficulty, Directive, DisplaySlot, DisplayedSkinParts, EntityEquipment,
    EntityInteraction, EntityMovement, EquipmentSlot, GameMode, Hand, Identifier, ItemStack,
    LoginProfile, MainHand, ObjectiveMode, ObjectiveRenderType, ParticleOptions, PlayerCommand,
    PlayerListEntry,
    ProfileProperty, RecipeBookType, ResourceKey, ResourcePackResponseKind, Rotation, SectionPos,
    ServerAddress, SoundCategory, TeamAction, TeamColor, TeamParameters, TeleportFlags, Text,
    Vec3, Vec3f, VersionAdapter, Visibility, WorldSink,
};
use lodestone_world::{ChunkPos as WorldChunkPos, Heightmaps, LoadedChunk};

use crate::canonical::{self, FallbackTally};
use crate::entity_types;
use crate::item_types;
use crate::particle_ids;
use crate::packets::chunk::{ChunkShape, MapChunk, UnloadChunk};
use crate::packets::common::{
    KeepAliveRequest, KeepAliveRequestVarInt, KeepAliveResponse, KeepAliveResponseVarInt,
};
use crate::packets::entity::{
    EntityDestroy, EntityLook, EntityMoveLook, EntityTeleport, EntityVelocityPacket,
    NamedEntitySpawn, RelEntityMove, SpawnEntityExperienceOrb, SpawnEntityLiving,
    SpawnEntityLivingByteType, SpawnEntityPainting, SpawnEntityWeather, SpawnObject,
};
use crate::packets::game::{
    Animation, AttachEntity, BlockAction, BlockDig, BlockPlace, BlockPlaceByteCursor,
    ClientCommand, ClientboundChat, ClientboundEntityEquipment, ClientboundPositionLook, Collect,
    DifficultyPacket, EntityAction, EntityEffect, JoinGame, KickDisconnect, NamedSoundEffect,
    NamedSoundEffectBytePitch, OpenSignEntity, PlayerlistHeader, RemoveEntityEffect, Respawn,
    ScoreboardDisplayObjective, ServerboundArmAnimation, ServerboundChat, ServerboundFlying,
    ServerboundLook, ServerboundPosition, ServerboundPositionLook, SetPassengers, SoundEffect,
    SoundEffectBytePitch, Spectate, SpawnPosition, TeleportConfirm, UpdateHealth, UpdateTime,
    UseEntity, UseEntityAt, UseEntityInteract, legacy_pitch, quantise_cursor,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{EncryptionRequest, LoginDisconnect, LoginSuccess, SetCompression};
use crate::packets::player_info::{PlayerInfo, PlayerInfoAction};
use crate::packets::position::Position;
use crate::packets::settings::{BrandPayload, PlayerAbilities, ResourcePackReceive, Settings};
use crate::packets::slot::Slot;
use crate::packets::window::{
    CloseWindow, EnchantItem, HeldItemSlot, OpenWindow, ServerboundCloseWindow,
    ServerboundHeldItemSlot, SetCreativeSlot, SetSlot, WindowItems,
};

/// Protocol version of the newest release this family speaks (Minecraft
/// 1.12.2), and the one a zero-argument [`adapter`] constructs.
pub const PROTOCOL: i32 = PROTOCOL_1_12_2;

/// Protocol version of Minecraft 1.9.4 — the era's opening release.
pub const PROTOCOL_1_9_4: i32 = 110;
/// Protocol version of Minecraft 1.10.2.
pub const PROTOCOL_1_10_2: i32 = 210;
/// Protocol version of Minecraft 1.11.2.
pub const PROTOCOL_1_11_2: i32 = 316;
/// Protocol version of Minecraft 1.12.2 — the era's closing release.
pub const PROTOCOL_1_12_2: i32 = 340;

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
/// This family is an *era* crate: one wire generation, four releases. The
/// four protocols agree on every packet id and on all but nine packet shapes
/// (`resource_pack_receive`, `named_sound_effect`, `sound_effect`, `collect`,
/// `spawn_entity_living`, `title`, `block_place`, `keep_alive` and the
/// entity-metadata type table), each of which is carried by a `since`/`until`
/// predicate or an explicitly protocol-selected struct rather than by a
/// second copy of the family. [`adapter_for`] selects that protocol's
/// generated id table at construction.
pub const PROTOCOLS: &[i32] = &[
    PROTOCOL_1_9_4,
    PROTOCOL_1_10_2,
    PROTOCOL_1_11_2,
    PROTOCOL_1_12_2,
];


/// The packet ids one protocol in this era assigns to the packets this
/// adapter names.
///
/// The generated `packet_ids_*` tables are one module per protocol, so a
/// `self.ids().block_dig` path can only ever mean *one* protocol's
/// id. This struct is the indirection that lets a single adapter body serve
/// four: it is resolved once, at construction, from the negotiated protocol,
/// and every id an arm sends reads through it. Nothing in this file may name
/// a generated module directly outside `packet_ids_from!` -- doing so is how
/// a 1.9.4 client ends up sending 1.12.2's ids.
///
/// Handshake, status and login ids are identical across all four protocols
/// (measured: the four generated tables differ only in the `play` section),
/// but they are selected the same way rather than shared, so that a future
/// era member which does move one cannot do so silently.
#[derive(Debug)]
struct PacketIds {
    /// This protocol's whole clientbound play table, the denominator the
    /// dispatch table is built against.
    play_clientbound_entries: &'static [(&'static str, i32)],
    /// `minecraft:set_protocol`, serverbound handshaking.
    handshake_set_protocol: i32,
    /// `minecraft:login_start`, serverbound login.
    login_start: i32,
    /// `minecraft:disconnect`, clientbound login.
    login_disconnect: i32,
    /// `minecraft:encryption_begin`, clientbound login.
    login_encryption_begin: i32,
    /// `minecraft:success`, clientbound login.
    login_success: i32,
    /// `minecraft:compress`, clientbound login.
    login_compress: i32,
    /// `minecraft:abilities`, serverbound play.
    abilities: i32,
    /// `minecraft:arm_animation`, serverbound play.
    arm_animation: i32,
    /// `minecraft:block_dig`, serverbound play.
    block_dig: i32,
    /// `minecraft:block_place`, serverbound play.
    block_place: i32,
    /// `minecraft:chat`, serverbound play.
    chat: i32,
    /// `minecraft:client_command`, serverbound play.
    client_command: i32,
    /// `minecraft:close_window`, serverbound play.
    close_window: i32,
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
    /// `minecraft:crafting_book_data`, serverbound play. Added in 1.12, so
    /// `None` for the three earlier protocols in this era -- an `Option`
    /// rather than a sentinel because "this protocol has no such packet" is
    /// a real answer an arm has to handle, not an error value.
    crafting_book_data: Option<i32>,
}

/// Builds a [`PacketIds`] from one generated table module.
macro_rules! packet_ids_from {
    ($table:ident, crafting_book_data = $crafting_book_data:expr) => {
        PacketIds {
            play_clientbound_entries: crate::$table::play::clientbound::ENTRIES,
            handshake_set_protocol: crate::$table::handshaking::serverbound::SET_PROTOCOL,
            login_start: crate::$table::login::serverbound::LOGIN_START,
            login_disconnect: crate::$table::login::clientbound::DISCONNECT,
            login_encryption_begin: crate::$table::login::clientbound::ENCRYPTION_BEGIN,
            login_success: crate::$table::login::clientbound::SUCCESS,
            login_compress: crate::$table::login::clientbound::COMPRESS,
            abilities: crate::$table::play::serverbound::ABILITIES,
            arm_animation: crate::$table::play::serverbound::ARM_ANIMATION,
            block_dig: crate::$table::play::serverbound::BLOCK_DIG,
            block_place: crate::$table::play::serverbound::BLOCK_PLACE,
            chat: crate::$table::play::serverbound::CHAT,
            client_command: crate::$table::play::serverbound::CLIENT_COMMAND,
            close_window: crate::$table::play::serverbound::CLOSE_WINDOW,
            custom_payload: crate::$table::play::serverbound::CUSTOM_PAYLOAD,
            enchant_item: crate::$table::play::serverbound::ENCHANT_ITEM,
            entity_action: crate::$table::play::serverbound::ENTITY_ACTION,
            flying: crate::$table::play::serverbound::FLYING,
            held_item_slot: crate::$table::play::serverbound::HELD_ITEM_SLOT,
            keep_alive: crate::$table::play::serverbound::KEEP_ALIVE,
            look: crate::$table::play::serverbound::LOOK,
            position: crate::$table::play::serverbound::POSITION,
            position_look: crate::$table::play::serverbound::POSITION_LOOK,
            resource_pack_receive: crate::$table::play::serverbound::RESOURCE_PACK_RECEIVE,
            set_creative_slot: crate::$table::play::serverbound::SET_CREATIVE_SLOT,
            settings: crate::$table::play::serverbound::SETTINGS,
            spectate: crate::$table::play::serverbound::SPECTATE,
            tab_complete: crate::$table::play::serverbound::TAB_COMPLETE,
            teleport_confirm: crate::$table::play::serverbound::TELEPORT_CONFIRM,
            use_entity: crate::$table::play::serverbound::USE_ENTITY,
            crafting_book_data: $crafting_book_data,
        }
    };
}

/// Minecraft 1.9.4's ids.
static IDS_1_9_4: PacketIds = packet_ids_from!(packet_ids_110, crafting_book_data = None);
/// Minecraft 1.10.2's ids.
static IDS_1_10_2: PacketIds = packet_ids_from!(packet_ids_210, crafting_book_data = None);
/// Minecraft 1.11.2's ids.
static IDS_1_11_2: PacketIds = packet_ids_from!(packet_ids_316, crafting_book_data = None);
/// Minecraft 1.12.2's ids.
static IDS_1_12_2: PacketIds = packet_ids_from!(
    packet_ids,
    crafting_book_data = Some(crate::packet_ids::play::serverbound::CRAFTING_BOOK_DATA)
);

/// Resolves a negotiated protocol to its id table.
///
/// # Panics
///
/// Panics for a protocol outside [`PROTOCOLS`]. This is a construction-time
/// check on a value the registry has already tested for membership, not a
/// wire value: reaching it means a caller bypassed
/// `VersionAdapter::supports`, and answering with some other protocol's ids
/// would be the silent-wrong-wire failure this whole indirection exists to
/// prevent.
fn ids_for(protocol: i32) -> &'static PacketIds {
    match protocol {
        PROTOCOL_1_9_4 => &IDS_1_9_4,
        PROTOCOL_1_10_2 => &IDS_1_10_2,
        PROTOCOL_1_11_2 => &IDS_1_11_2,
        PROTOCOL_1_12_2 => &IDS_1_12_2,
        other => panic!(
            "protocol {other} is outside this family's PROTOCOLS ({PROTOCOLS:?}); \
             callers must test membership before constructing an adapter"
        ),
    }
}

/// Requested next-state value in the handshake for a login connection.
const NEXT_STATE_LOGIN: i32 = 2;

/// Relative-teleport flag bits used by the clientbound 1.8 position packet.
const REL_X: i8 = 0x01;
const REL_Y: i8 = 0x02;
const REL_Z: i8 = 0x04;
const REL_YAW: i8 = 0x08;
const REL_PITCH: i8 = 0x10;

/// Per-connection state used by 1.12.2's client-side player-movement-send tick.
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

/// Version adapter implementing protocol 340 (Minecraft 1.12.2).
///
/// Holds the current dimension's [`ChunkShape`] because a `map_chunk` cannot
/// tell from its own bytes whether sky light is present — that depends on the
/// dimension announced at join. The shape is guarded by a [`Mutex`] purely to
/// satisfy `Sync`; packets are processed serially so there is no contention.
#[derive(Debug, Clone)]
pub struct V340Adapter {
    /// The negotiated protocol this adapter speaks: one of [`PROTOCOLS`].
    /// Every codec call in this file reads it through [`V340Adapter::ctx`],
    /// and every packet id through [`V340Adapter::ids`].
    protocol: i32,
    /// This protocol's generated id table, resolved once at construction.
    ids: &'static PacketIds,
    shape: Arc<Mutex<ChunkShape>>,
    /// Raw 1.12.2 dimension id from the most recent `login`/`respawn`, so a
    /// packet that identifies its dimension only implicitly (e.g.
    /// `spawn_position`, which has no dimension field of its own) can still
    /// build a [`lodestone_model::DimensionId`]. Defaults to `0` (overworld),
    /// matching `shape`'s own default.
    current_dimension: Arc<Mutex<i32>>,
    /// The most recently sent `ClientAction::CommandSuggestion`, remembered
    /// because 1.12.2's `tab_complete` reply carries neither a transaction id
    /// nor a replacement range (both added in 1.13) — see the `TAB_COMPLETE`
    /// arm in `handle_play` for how this is used to reconstruct them.
    pending_tab_complete: Arc<Mutex<Option<PendingTabComplete>>>,
    movement: Arc<Mutex<MovementSendState>>,
}

/// The half of an outgoing `command_suggestion` request 1.12.2's reply does
/// not echo back, kept just long enough to answer the one reply it produced.
/// Overwritten rather than queued: only one tab-complete request is ever in
/// flight, mirroring `lodestone_shell::chat::SuggestionRequests`'s own
/// single-`pending` design on the other end of this same round trip.
#[derive(Debug, Clone)]
struct PendingTabComplete {
    id: i32,
    command: String,
}

/// Byte offset of the last whitespace-delimited word in `text` — the start of
/// the range 1.12.2's `tab_complete` matches replace, since this version
/// sends full replacement strings for that word rather than a
/// server-declared range. Mirrors the vanilla client's own last-word-index
/// lookup: the offset just past the final run of whitespace, or `0` when
/// there is none.
fn last_word_index(text: &str) -> usize {
    let mut result = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            result = j;
            i = j;
        } else {
            i += 1;
        }
    }
    result
}

impl Default for V340Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl V340Adapter {
    /// Creates a new adapter for the era's newest protocol ([`PROTOCOL`],
    /// Minecraft 1.12.2), defaulting to the overworld chunk shape until a
    /// join packet announces the real dimension.
    ///
    /// Use [`adapter_for`] for any other member of the era.
    #[must_use]
    pub fn new() -> Self {
        Self::for_protocol(PROTOCOL)
    }

    /// Creates a new adapter speaking `protocol`.
    ///
    /// # Panics
    ///
    /// Panics for a protocol outside [`PROTOCOLS`] — see [`ids_for`].
    #[must_use]
    pub fn for_protocol(protocol: i32) -> Self {
        Self {
            protocol,
            ids: ids_for(protocol),
            shape: Arc::new(Mutex::new(ChunkShape::overworld())),
            current_dimension: Arc::new(Mutex::new(0)),
            pending_tab_complete: Arc::new(Mutex::new(None)),
            movement: Arc::new(Mutex::new(MovementSendState::default())),
        }
    }

    /// Selects the 1.12.2 movement shape, retaining the pose last sent on
    /// each axis and only sending the base `flying` packet for an on-ground
    /// transition.
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

    /// Records the request `TAB_COMPLETE`'s reply cannot echo back itself.
    fn remember_tab_complete(&self, id: i32, command: String) {
        if let Ok(mut pending) = self.pending_tab_complete.lock() {
            *pending = Some(PendingTabComplete { id, command });
        }
    }

    /// Takes (and clears) the pending tab-complete request, if any.
    fn take_tab_complete(&self) -> Option<PendingTabComplete> {
        self.pending_tab_complete.lock().ok().and_then(|mut p| p.take())
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
        if let Ok(mut current) = self.current_dimension.lock() {
            *current = dimension;
        }
    }

    /// Returns the most recently announced raw dimension id.
    fn current_dimension(&self) -> i32 {
        self.current_dimension.lock().map_or(0, |value| *value)
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
pub fn adapter_for(protocol: i32) -> V340Adapter {
    debug_assert!(
        PROTOCOLS.contains(&protocol),
        "adapter_for({protocol}) is outside this family's PROTOCOLS ({PROTOCOLS:?}); \
         callers must test membership before constructing"
    );
    V340Adapter::for_protocol(protocol)
}

impl V340Adapter {
    /// The codec context for the protocol this adapter was constructed for.
    ///
    /// Every `#[mc(since = N)]`/`#[mc(until = N)]` predicate and every
    /// `#[mc(protocols = "a..=b")]` precondition in this family reads
    /// `ctx.version`, so this is the single point at which "which protocol
    /// am I speaking" reaches the codecs. It is a per-instance value, not a
    /// constant, because one adapter type now serves four protocols.
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

    /// Encodes a packet body into a fresh byte buffer.
    ///
    /// Thin wrapper over the version-free [`lodestone_core::encode_body`],
    /// which returns a stringified error because `AdapterError` lives in
    /// `lodestone-model` and `lodestone-core` cannot depend on it.
    fn encode_body<T: Encode>(&self, packet: &T) -> Result<Vec<u8>, AdapterError> {
        lodestone_core::encode_body(packet, self.ctx()).map_err(AdapterError::Encode)
    }

    /// Decodes a packet body from raw bytes.
    fn decode_body<T: Decode>(&self, payload: &[u8]) -> Result<T, AdapterError> {
        lodestone_core::decode_body(payload, self.ctx()).map_err(AdapterError::Decode)
    }

    /// Like [`Self::decode_body`] but additionally requires the payload to be
    /// fully consumed. Used for packets whose whole body we decode (e.g. the
    /// entity destroy id list), where trailing bytes signal a wrong layout and
    /// must be rejected rather than silently ignored. Packets that
    /// deliberately leave a tail unread (metadata terminators, fields we don't
    /// model yet) keep using the lenient [`Self::decode_body`].
    fn decode_body_exact<T: Decode>(&self, payload: &[u8]) -> Result<T, AdapterError> {
        lodestone_core::decode_body_exact(payload, self.ctx()).map_err(AdapterError::Decode)
    }

    /// Builds a [`Directive::Send`] from a packet id and an encodable body.
    fn send<T: Encode>(&self, packet_id: i32, packet: &T) -> Result<Directive, AdapterError> {
        Ok(Directive::Send {
            packet_id,
            payload: self.encode_body(packet)?,
        })
    }
}

/// Encodes the serverbound `crafting_book_data` packet body for the
/// "settings changed" variant (`type` = `1`): a leading varint discriminant,
/// then the open flag and filter flag.
///
/// 1.12.2 folds two unrelated recipe-book actions into one packet via a
/// varint `type` switch (`0` = displayed-recipe id, `1` = settings changed;
/// minecraft-data's `packet_crafting_book_data`), so this can't be a plain
/// derived struct the way [`ClientCommand`]/[`Spectate`] are. `type` = `0`
/// (recipe seen) is not encoded here — see the adapter's
/// `RecipeBookSeenRecipe` arm for why.
fn encode_crafting_book_settings(open: bool, filtering: bool) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(1);
    w.bool(open);
    w.bool(filtering);
    w.into_vec()
}

/// Maps a decode error to the adapter's decode-error variant. Used by the
/// hand-decoded arms (`block_change`/`multi_block_change`/`entity_status`/
/// `entity_head_rotation`) that read a [`Reader`] directly rather than going
/// through a derived [`Decode`] body, mirroring `lodestone-v26-2`'s own
/// `dec_err` helper.
fn dec_err(err: impl std::fmt::Display) -> AdapterError {
    AdapterError::Decode(err.to_string())
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

/// Maps a `boss_bar` varint colour ordinal to the canonical [`BossColor`].
///
/// Vanilla's `BossEvent.BossBarColor` enum declaration order (`PINK`,
/// `BLUE`, `RED`, `GREEN`, `YELLOW`, `PURPLE`, `WHITE`) matches
/// [`BossColor`]'s own declaration order exactly, so this is a direct index
/// rather than a remapping.
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

/// Maps a `teams` signed colour byte to the canonical [`TeamColor`].
///
/// Vanilla packs this as an `EnumChatFormatting` ordinal (the same 16-colour
/// `§`-code order [`lodestone_model::TextColor`]'s own `NAMED` table walks),
/// so `-1` ("no colour"/reset) and any other value outside `0..=15` both
/// resolve to `None` rather than being rejected — a team legitimately has no
/// colour, and this is not a wire-shape error the way an unrecognised
/// `teams` mode or an unrecognised objective render type is.
fn team_color_from_byte(byte: i8) -> Option<TeamColor> {
    match byte {
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

/// Converts a decoded [`Slot`] into a canonical [`ItemStack`], resolving the
/// legacy numeric item id through [`item_types::item_name`].
///
/// Mirrors `lodestone-v1-8`'s identically-named helper: an empty slot or an id
/// this crate's item table has no entry for both resolve to `None`, and
/// `damage` is deliberately not folded into a variant — see
/// `crate::item_types`'s module doc.
fn slot_to_item_stack(slot: &Slot) -> Option<ItemStack> {
    match slot {
        Slot::Empty => None,
        Slot::Item { id, count, .. } => {
            let name = item_types::item_name(*id)?;
            let key: ResourceKey = name.parse().ok()?;
            Some(ItemStack::new(key, u32::try_from(*count).unwrap_or(0)))
        }
    }
}

/// Resolves a 1.12.2 `open_window` `inventory_type` string to a canonical
/// `minecraft:menu` key.
///
/// Identical mapping to `lodestone-v1-8`'s `resolve_menu_type` — 1.12.2's
/// `windows.json` names the same static container types 1.8 does, and a
/// chest/container/`EntityHorse` window still carries no fixed modern menu,
/// so its row count is derived from `slot_count` the same way vanilla's own
/// chest menu derives its row count (26.2 has no dedicated horse menu type
/// at all).
fn resolve_menu_type(inventory_type: &str, slot_count: u8) -> ResourceKey {
    let generic_rows = || {
        // Ceiling division: a floor division would under-count rows for any
        // `slot_count` not a multiple of 9 (a 17-slot horse inventory would
        // floor to 1 row of 9 and silently hide 8 slots).
        let rows = (u32::from(slot_count).div_ceil(9)).clamp(1, 6);
        format!("minecraft:generic_9x{rows}")
    };
    let key = match inventory_type {
        "minecraft:chest" | "minecraft:container" | "EntityHorse" => generic_rows(),
        "minecraft:dispenser" | "minecraft:dropper" => "minecraft:generic_3x3".to_string(),
        "minecraft:crafting_table" => "minecraft:crafting".to_string(),
        "minecraft:enchanting_table" => "minecraft:enchantment".to_string(),
        "minecraft:villager" => "minecraft:merchant".to_string(),
        // furnace, anvil, beacon, brewing_stand, hopper already match 26.2's
        // own key verbatim.
        other => other.to_string(),
    };
    key.parse().unwrap_or_else(|_| {
        // Unreachable for every branch above (all produce well-formed
        // `minecraft:*` keys); fall back to the generic shape rather than
        // panicking on a future `inventory_type` this table has not seen.
        generic_rows().parse().expect("generic_9xN is always valid")
    })
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

/// Largest block coordinate any vanilla world can legitimately contain, on
/// either horizontal axis: the world border implementation's absolute
/// maximum size constant is 29,999,984, and the border is what bounds every
/// world regardless of the `worldborder` command or the world's own
/// settings. Anything past this is not an awkward-but-real position, it is
/// invalid input.
const ABSOLUTE_MAX_BLOCK: i32 = 29_999_984;

/// Turns a wire-supplied chunk coordinate into the block coordinate of its
/// west/north edge, refusing anything the world border makes impossible.
///
/// This exists because of a remote panic found by
/// `lodestone-fuzz`'s `handle_packet_never_panics` target: `multi_block_change`
/// read `chunk_x`/`chunk_z` straight off the wire and did an unchecked
/// `chunk_x * 16`. For any `|chunk|` past `i32::MAX / 16` that **panics in
/// debug** and, worse, **silently wraps in release** — a wrapped coordinate
/// writes a block at a position the packet never named, which is a corrupted
/// world rather than a crash. The shrunk failing payload was twelve bytes:
/// `08 00 00 00 …`, i.e. `chunk_x = 134_217_728`.
///
/// Both halves of the guard are deliberate and neither is redundant:
///
/// - `checked_mul` is the **structural** half. It cannot overflow whatever the
///   bound below says, so a future edit that loosens or removes the range check
///   still cannot reintroduce a panic or a wrap.
/// - the range check is the **semantic** half. It refuses out-of-range input at
///   the decode seam instead of clamping it, because a clamp would silently
///   invent a position exactly the way the release-mode wrap did. Refusal is
///   the only outcome that cannot write a block somewhere it was never told to.
fn chunk_origin_block(chunk_coord: i32, axis: &str) -> Result<i32, AdapterError> {
    chunk_coord
        .checked_mul(16)
        .filter(|origin| origin.unsigned_abs() <= ABSOLUTE_MAX_BLOCK.unsigned_abs())
        .ok_or_else(|| {
            AdapterError::Decode(format!(
                "multi_block_change chunk {axis} {chunk_coord} is outside the world border \
                 (|chunk {axis} * 16| must be <= {ABSOLUTE_MAX_BLOCK})"
            ))
        })
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
/// the same formula and is used identically by v1-8 and v1-14.
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

/// The vanilla ability flag bit set when the client is invulnerable — only
/// meaningful on the **clientbound** `abilities` packet; the serverbound
/// direction the client sends carries only the flying bit.
const ABILITY_INVULNERABLE: i8 = 0x01;
/// The vanilla flying-ability flag bit set when the client is flying.
const ABILITY_FLYING: i8 = 0x02;
/// The vanilla ability flag bit set when the client is allowed to fly —
/// clientbound-only, same caveat as [`ABILITY_INVULNERABLE`].
const ABILITY_CAN_FLY: i8 = 0x04;
/// The vanilla ability flag bit set when the client may instantly break
/// blocks (creative mode) — clientbound-only, same caveat as
/// [`ABILITY_INVULNERABLE`].
const ABILITY_INSTABUILD: i8 = 0x08;
/// Vanilla default flying speed, sent in the server-ignored serverbound field.
const DEFAULT_FLYING_SPEED: f32 = 0.05;
/// Vanilla default walking speed, sent in the server-ignored serverbound field.
const DEFAULT_WALKING_SPEED: f32 = 0.1;

/// Signature every `play`-state clientbound handler shares: an inherent
/// associated function coerced to a plain fn pointer, so `Handler<T>`
/// stays `Copy` with no captured state. Kept uniform across all ~80
/// packets in this family's `play::clientbound::ENTRIES` table even
/// though most handlers use only a fraction of these parameters --
/// exactly the mechanical, one-shape-fits-all point of a dispatch table.
type PlayHandlerFn =
    fn(&V340Adapter, &mut dyn WorldSink, &[u8]) -> Result<Vec<Directive>, AdapterError>;

static PLAY_CLIENTBOUND_HANDLERS: &[(&str, lodestone_core::dispatch::Handler<PlayHandlerFn>)] = &[
    ("minecraft:login", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_login)),
    ("minecraft:map_chunk", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_map_chunk)),
    ("minecraft:unload_chunk", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_unload_chunk)),
    ("minecraft:keep_alive", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_keep_alive)),
    ("minecraft:chat", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_chat)),
    ("minecraft:position", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_position)),
    ("minecraft:spawn_entity_living", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_spawn_entity_living)),
    ("minecraft:spawn_entity", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_spawn_entity)),
    ("minecraft:named_entity_spawn", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_named_entity_spawn)),
    ("minecraft:rel_entity_move", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_rel_entity_move)),
    ("minecraft:entity_look", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_entity_look)),
    ("minecraft:entity_move_look", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_entity_move_look)),
    ("minecraft:entity_teleport", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_entity_teleport)),
    ("minecraft:entity_velocity", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_entity_velocity)),
    ("minecraft:entity_destroy", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_entity_destroy)),
    ("minecraft:kick_disconnect", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_kick_disconnect)),
    ("minecraft:update_health", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_update_health)),
    ("minecraft:respawn", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_respawn)),
    ("minecraft:entity_status", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_entity_status)),
    ("minecraft:entity_head_rotation", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_entity_head_rotation)),
    ("minecraft:block_change", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_block_change)),
    ("minecraft:multi_block_change", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_multi_block_change)),
    ("minecraft:open_window", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_open_window)),
    ("minecraft:close_window", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_close_window)),
    ("minecraft:window_items", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_window_items)),
    ("minecraft:set_slot", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_set_slot)),
    ("minecraft:craft_progress_bar", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_craft_progress_bar)),
    ("minecraft:title", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_title)),
    ("minecraft:tab_complete", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_tab_complete)),
    ("minecraft:player_info", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_player_info)),
    ("minecraft:held_item_slot", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_held_item_slot)),
    ("minecraft:abilities", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_abilities)),
    ("minecraft:block_action", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_block_action)),
    ("minecraft:entity_equipment", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_entity_equipment)),
    ("minecraft:animation", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_animation)),
    ("minecraft:named_sound_effect", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_named_sound_effect)),
    ("minecraft:sound_effect", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_sound_effect)),
    ("minecraft:scoreboard_display_objective", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_scoreboard_display_objective)),
    ("minecraft:scoreboard_objective", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_scoreboard_objective)),
    ("minecraft:scoreboard_score", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_scoreboard_score)),
    ("minecraft:teams", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_teams)),
    ("minecraft:boss_bar", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_boss_bar)),
    ("minecraft:spawn_position", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_spawn_position)),
    ("minecraft:update_time", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_update_time)),
    ("minecraft:difficulty", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_difficulty)),
    ("minecraft:playerlist_header", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_playerlist_header)),
    ("minecraft:attach_entity", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_attach_entity)),
    ("minecraft:set_passengers", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_set_passengers)),
    ("minecraft:collect", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_collect)),
    ("minecraft:entity_effect", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_entity_effect)),
    ("minecraft:remove_entity_effect", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_remove_entity_effect)),
    ("minecraft:spawn_entity_weather", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_spawn_entity_weather)),
    ("minecraft:spawn_entity_experience_orb", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_spawn_entity_experience_orb)),
    ("minecraft:world_particles", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_world_particles)),
    ("minecraft:experience", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_experience)),
    ("minecraft:vehicle_move", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_vehicle_move)),
    ("minecraft:set_cooldown", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_set_cooldown)),
    ("minecraft:combat_event", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_combat_event)),
    ("minecraft:world_border", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_world_border)),
    ("minecraft:open_sign_entity", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_open_sign_entity)),
    // 1.12 additions: absent from 110/210/316's tables entirely, so the
    // handler declares the range in which the packet exists. `Table::build`
    // skips it for the three earlier protocols and demands it for 340.
    ("minecraft:select_advancement_tab", lodestone_core::dispatch::Handler::new(ProtocolRange::new(PROTOCOL_1_12_2, PROTOCOL_1_12_2), V340Adapter::play_select_advancement_tab)),
    ("minecraft:spawn_entity_painting", lodestone_core::dispatch::Handler::new(ProtocolRange::ALL, V340Adapter::play_spawn_entity_painting)),
];

static PLAY_CLIENTBOUND_IGNORED: &[lodestone_core::dispatch::IGNORED] = &[
    lodestone_core::dispatch::IGNORED::new("minecraft:statistics", "v26-2 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:block_break_animation", "v26-2 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:tile_entity_data", "v26-2 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:transaction", "confirm-transaction handshake removed after 1.16; v26-2 has no clientbound equivalent"),
    lodestone_core::dispatch::IGNORED::new("minecraft:custom_payload", "v26-2 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:explosion", "v26-2 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:game_state_change", "v26-2 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:world_event", "v26-2 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:map", "v26-2 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:entity", "bare entity-id packet with no move/look payload (minecraft-data packet_entity is just {entityId}); nothing observable to translate"),
    lodestone_core::dispatch::IGNORED::ranged(
        "minecraft:craft_recipe_response",
        "v26-2 has this; backport",
        // Added in 1.12; the three earlier protocols in this era have no
        // such packet, so the exemption is ranged rather than blanket.
        ProtocolRange::new(PROTOCOL_1_12_2, PROTOCOL_1_12_2),
    ),
    lodestone_core::dispatch::IGNORED::new("minecraft:bed", "removed protocol packet; modern client conveys the sleeping pose via entity metadata, v26-2 has no clientbound equivalent"),
    lodestone_core::dispatch::IGNORED::ranged(
        "minecraft:unlock_recipes",
        "v26-2 has this; backport",
        // Added in 1.12; the three earlier protocols in this era have no
        // such packet, so the exemption is ranged rather than blanket.
        ProtocolRange::new(PROTOCOL_1_12_2, PROTOCOL_1_12_2),
    ),
    lodestone_core::dispatch::IGNORED::new("minecraft:resource_pack_send", "v26-2 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:camera", "v26-2 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:entity_metadata", "v26-2 has this; backport"),
    lodestone_core::dispatch::IGNORED::ranged(
        "minecraft:advancements",
        "v26-2 has this; backport",
        // Added in 1.12; the three earlier protocols in this era have no
        // such packet, so the exemption is ranged rather than blanket.
        ProtocolRange::new(PROTOCOL_1_12_2, PROTOCOL_1_12_2),
    ),
    lodestone_core::dispatch::IGNORED::new("minecraft:entity_update_attributes", "v26-2 has this; backport"),
];

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
        if packet_id == self.ids().login_compress {
            let body: SetCompression = self.decode_body(payload)?;
            return Ok(vec![Directive::SetCompression(body.threshold)]);
        }
        if packet_id == self.ids().login_success {
            // Validate the profile decodes (string UUID + name), then advance.
            let _profile: LoginSuccess = self.decode_body(payload)?;
            return Ok(vec![Directive::SetState(ConnectionState::Play)]);
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

    /// This protocol's clientbound play dispatch table, built on first use
    /// and then cached for the life of the process.
    ///
    /// One `OnceLock` per protocol in [`PROTOCOLS`], indexed the same way
    /// [`ids_for`] resolves an id table, so a table built for 1.9.4 can never
    /// be handed to a 1.12.2 adapter.
    fn play_dispatch_table(
        &self,
    ) -> &'static lodestone_core::dispatch::Table<'static, PlayHandlerFn> {
        static TABLES: [std::sync::OnceLock<
            lodestone_core::dispatch::Table<'static, PlayHandlerFn>,
        >; 4] = [
            std::sync::OnceLock::new(),
            std::sync::OnceLock::new(),
            std::sync::OnceLock::new(),
            std::sync::OnceLock::new(),
        ];
        let slot = match self.protocol {
            PROTOCOL_1_9_4 => 0,
            PROTOCOL_1_10_2 => 1,
            PROTOCOL_1_11_2 => 2,
            _ => 3,
        };
        TABLES[slot].get_or_init(|| {
            lodestone_core::dispatch::Table::build(
                self.protocol,
                self.ids().play_clientbound_entries,
                PLAY_CLIENTBOUND_HANDLERS,
                PLAY_CLIENTBOUND_IGNORED,
            )
            .unwrap_or_else(|err| {
                panic!(
                    "v1-9 play dispatch table for protocol {} must build: every clientbound \
                     ENTRIES id needs either a bound handler or a PLAY_CLIENTBOUND_IGNORED \
                     reason covering this protocol -- {err}",
                    self.protocol
                )
            })
        })
    }

    /// Handles a clientbound packet while in the play state.
    ///
    /// Dispatch is a `lodestone_core::dispatch::Table` built once **per
    /// protocol** from that protocol's own clientbound `ENTRIES`,
    /// `PLAY_CLIENTBOUND_HANDLERS` and `PLAY_CLIENTBOUND_IGNORED`, replacing
    /// the former if-chain and its silent trailing `_ =>` island: an id in
    /// `ENTRIES` with neither a handler nor an `IGNORED` reason fails table
    /// construction by name instead of being dropped forever with nothing red
    /// anywhere.
    ///
    /// Four protocols means four tables, cached independently, because the
    /// id→handler mapping genuinely differs: 1.12 inserted `craft_recipe_
    /// response`, `unlock_recipes`, `select_advancement_tab` and
    /// `advancements` into the middle of the clientbound table, shifting
    /// every id above `entity` by up to four.
    fn handle_play(
        &self,
        world: &mut dyn WorldSink,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let table = self.play_dispatch_table();
        match table.get(packet_id) {
            Some(handler) => handler(self, world, payload),
            // A packet id absent from this protocol's own `ENTRIES` table -- a
            // value outside 1.12.2's real wire range -- reaches here directly
            // from the raw VarInt `handle_packet` decoded off the wire, with
            // nothing upstream validating it against `ENTRIES` first (see
            // `handle_packet`). `Table::build` guarantees every *listed* id
            // resolves to a handler or an `IGNORED` reason; it says nothing
            // about an id it was never told about, so this is deliberately not
            // `unreachable!()` -- a malformed or non-vanilla server sending an
            // out-of-table id must not be able to panic this client. Falling
            // through silently is the same drop-recognises-nothing behaviour
            // the old if-chain already had for anything it did not name, not a
            // narrower one.
            None => Ok(Vec::new()),
        }
    }

    fn play_login(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: JoinGame = self.decode_body(payload)?;
        // Record whether this dimension carries sky light before any chunk
        // arrives, so single `map_chunk` packets decode the right geometry.
        self.set_dimension(body.dimension);
        return Ok(vec![Directive::Emit(ClientEvent::Login {
            entity_id: body.entity_id,
            game_mode: game_mode(body.game_mode)?,
            dimension: dimension_id(body.dimension)?,
        })]);
    }

    fn play_map_chunk(&self, world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
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
            LoadedChunk::new(data.column, data.light, Heightmaps::new(), data.block_entities),
        );
        return Ok(vec![Directive::Emit(ClientEvent::ChunkLoaded { pos })]);
    }

    fn play_unload_chunk(&self, world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // 1.12.2 has a dedicated forget packet (two ints), unlike 1.8's
        // empty-bitmask trick.
        let body: UnloadChunk = self.decode_body(payload)?;
        let pos = ChunkPos::new(body.chunk_x, body.chunk_z);
        world.unload(WorldChunkPos::new(body.chunk_x, body.chunk_z));
        return Ok(vec![Directive::Emit(ClientEvent::ChunkUnloaded { pos })]);
    }

    fn play_keep_alive(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // 1.12 widened the id from a VarInt to a fixed 64-bit integer. Both
        // forms exist in this era, so the struct is chosen by protocol; see
        // `packets::common`.
        let id = if self.protocol >= PROTOCOL_1_12_2 {
            self.decode_body::<KeepAliveRequest>(payload)?.id
        } else {
            i64::from(self.decode_body::<KeepAliveRequestVarInt>(payload)?.id)
        };
        return Ok(vec![Directive::Emit(ClientEvent::KeepAlive { id })]);
    }

    fn play_chat(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: ClientboundChat = self.decode_body(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::Chat {
            text: Text::from_json(&body.message),
            kind: chat_kind(body.position),
            // 1.12's chat packet carries no sender field — nothing to filter on.
            sender: None,
            ack: None,
        })]);
    }

    fn play_position(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: ClientboundPositionLook = self.decode_body(payload)?;
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
            self.send(self.ids().teleport_confirm, &confirm)?,
            Directive::Emit(ClientEvent::TeleportPlayer {
                pos: Vec3::new(body.x, body.y, body.z),
                rotation: Rotation::new(body.yaw, body.pitch),
                flags,
            }),
        ]);
    }

    fn play_spawn_entity_living(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // 1.11 widened the mob type from a byte to a VarInt; below that the
        // byte form is a different struct (see `packets::entity`).
        let body: SpawnEntityLiving = if self.protocol >= PROTOCOL_1_11_2 {
            self.decode_body(payload)?
        } else {
            let legacy: SpawnEntityLivingByteType = self.decode_body(payload)?;
            SpawnEntityLiving {
                entity_id: legacy.entity_id,
                entity_uuid: legacy.entity_uuid,
                kind: i32::from(legacy.kind),
                x: legacy.x,
                y: legacy.y,
                z: legacy.z,
                yaw: legacy.yaw,
                pitch: legacy.pitch,
                head_pitch: legacy.head_pitch,
                velocity_x: legacy.velocity_x,
                velocity_y: legacy.velocity_y,
                velocity_z: legacy.velocity_z,
                metadata: legacy.metadata,
            }
        };
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

    fn play_spawn_entity(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnObject = self.decode_body(payload)?;
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

    fn play_named_entity_spawn(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: NamedEntitySpawn = self.decode_body(payload)?;
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

    fn play_rel_entity_move(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: RelEntityMove = self.decode_body(payload)?;
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

    fn play_entity_look(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityLook = self.decode_body(payload)?;
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

    fn play_entity_move_look(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityMoveLook = self.decode_body(payload)?;
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

    fn play_entity_teleport(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityTeleport = self.decode_body(payload)?;
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

    fn play_entity_velocity(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityVelocityPacket = self.decode_body(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::EntityVelocity {
            entity_id: body.entity_id,
            velocity: Vec3::new(
                f64::from(body.velocity_x) / VELOCITY_SCALE,
                f64::from(body.velocity_y) / VELOCITY_SCALE,
                f64::from(body.velocity_z) / VELOCITY_SCALE,
            ),
        })]);
    }

    fn play_entity_destroy(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // A varint-counted list of varint ids. Now a derived struct: the
        // `#[mc(varint)]`-on-`Vec<i32>` macro attribute (reported as a gap
        // and since landed) encodes both the length and each element as a
        // varint, replacing the former hand-decoded loop.
        let body: EntityDestroy = self.decode_body_exact(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::EntityRemoved {
            entity_ids: body.entity_ids,
        })]);
    }

    fn play_kick_disconnect(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: KickDisconnect = self.decode_body(payload)?;
        return Ok(vec![Directive::Disconnect(json_reason_text(&body.reason))]);
    }

    fn play_update_health(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // f32 health, varint food, f32 saturation — verified against
        // minecraft-data's 1.12.2 `packet_update_health` (identical to 1.8's
        // own shape). `UpdateHealth` already existed in this crate but was
        // only ever round-tripped in `tests/join_flow.rs`, never wired into
        // `handle_play` — an island per CLAUDE.md's own definition (decoded
        // nowhere in production, tested only against our own encoder).
        let body: UpdateHealth = self.decode_body(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::HealthChanged {
            health: body.health,
            food: body.food,
            saturation: body.food_saturation,
        })]);
    }

    fn play_respawn(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Signed int dimension, u8 difficulty, u8 game mode, string level
        // type — verified against minecraft-data's 1.12.2
        // `packet_respawn`. Like `join`'s `dimension`, `respawn`'s
        // `game_mode` packs the hardcore flag in bit `0x8`; reusing the
        // same `game_mode` helper masks it off identically. The dimension
        // shape re-recorded here matters for the *next* `map_chunk`: a
        // portal into the nether/end must flip `ChunkShape` before that
        // column's light arrays are decoded, exactly as `LOGIN` does on
        // first join.
        let body: Respawn = self.decode_body(payload)?;
        self.set_dimension(body.dimension);
        return Ok(vec![Directive::Emit(ClientEvent::Respawned {
            dimension: dimension_id(body.dimension)?,
            game_mode: game_mode(body.game_mode)?,
            previous_game_mode: None,
            last_death_location: None,
        })]);
    }

    fn play_entity_status(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // A raw (non-VarInt) `i32` entity id, then a raw status byte —
        // verified against minecraft-data's 1.12.2 `packet_entity_status`
        // (identical to 1.8's shape) and matching `lodestone-v26-2`'s own
        // `ENTITY_EVENT` decode (`dec_err`, hand-`Reader` rather than a
        // derived struct, since there is nothing else to model). Drives
        // hurt/death animation, totem-of-undying particles, etc. — the
        // consumer interprets `status` per the entity's own type, exactly
        // as the modern decode already documents.
        let mut reader = Reader::new(payload);
        let entity_id = reader.i32().map_err(dec_err)?;
        let status = reader.u8().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        return Ok(vec![Directive::Emit(ClientEvent::EntityStatus {
            entity_id,
            status,
        })]);
    }

    fn play_entity_head_rotation(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // VarInt entity id, then a packed signed-byte yaw (256 steps per
        // circle, same packing `unpack_degrees` already handles for body
        // rotation) — verified against minecraft-data's 1.12.2
        // `packet_entity_head_rotation`.
        let mut reader = Reader::new(payload);
        let entity_id = reader.var_i32().map_err(dec_err)?;
        let packed = reader.i8().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        return Ok(vec![Directive::Emit(ClientEvent::EntityHeadRotation {
            entity_id,
            head_yaw: unpack_degrees(packed),
        })]);
    }

    fn play_block_change(&self, world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // A packed pre-1.14 `position` (see `crate::packets::position`,
        // x/y/z big-endian, y in the middle) plus the changed block's
        // legacy composite id as a VarInt — verified against
        // minecraft-data's 1.12.2 `packet_block_change`. Unlike 26.2's
        // `block_update` (a real 32,366-state registry id straight off the
        // wire), 1.12.2's value is pre-Flattening: bits `4..` are the
        // numeric block id, the low 4 bits are metadata
        // (`(old_block_id << 4) | meta`, the same composite `chunk.rs`
        // already extracts per paletted section entry). `canonical::
        // resolve_or_air` bridges it to a real 26.2 block-state id via the
        // table built against the real 1.13.2 server jar's own
        // `DataFixerUpper` flattening fix (see `crate::canonical`'s module
        // docs) — not this crate's own encoder, and not a formula.
        let mut reader = Reader::new(payload);
        let pos: Position = Position::decode(&mut reader, self.ctx()).map_err(dec_err)?;
        let raw = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        let raw = u16::try_from(raw)
            .ok()
            .filter(|&raw| raw <= 0x0FFF)
            .ok_or_else(|| {
                AdapterError::Decode(format!(
                    "block_change composite id {raw} outside the 4095-slot legacy table"
                ))
            })?;
        let old_block_id = (raw >> 4) as u8;
        let meta = (raw & 0xF) as u8;
        let mut tally = FallbackTally::default();
        let state = canonical::resolve_or_air(old_block_id, meta, &mut tally);
        let pos = pos.0;
        world.set_block(pos.x, pos.y, pos.z, state);
        // Writing a state is what creates/removes a block entity in
        // vanilla (done inside the chunk's own block-state setter, no
        // packet involved) — the same reasoning `lodestone-v26-2`'s
        // `BLOCK_UPDATE` arm documents.
        world.sync_block_entity(
            pos.x,
            pos.y,
            pos.z,
            lodestone_data::block_states::StateId::new(state)
                .and_then(block_entity_type)
                .map(|kind| kind.raw()),
        );
        return Ok(vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
            section: SectionPos::new(pos.x >> 4, pos.y >> 4, pos.z >> 4),
            blocks: vec![[
                pos.x.rem_euclid(16) as u8,
                pos.y.rem_euclid(16) as u8,
                pos.z.rem_euclid(16) as u8,
            ]],
        })]);
    }

    fn play_multi_block_change(&self, world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Chunk X/Z (i32 each), then a VarInt-counted array of records —
        // verified against minecraft-data's 1.12.2
        // `packet_multi_block_change` (identical to 1.8's shape). Each
        // record is `horizontalPos: u8` (high nibble relative X, low
        // nibble relative Z — minecraft-data's `protocol.json` gives the
        // field width but not this bit order; sourced from the
        // long-stable external wire documentation for this exact packet,
        // not from our own encoder, and flagged here as the one field in
        // this pass not cross-checked against either the jar or a live
        // capture), `y: u8` (full column height, unlike 26.2's
        // section-relative nibble), then the same legacy composite VarInt
        // `block_change` carries. 1.12.2 has no sections on the wire —
        // ordinary full-height columns — so one packet's records can span
        // several of `lodestone-world`'s 16-tall sections; each is
        // resolved and written individually, then grouped by section so
        // the emitted `SectionBlocksChanged` events match what a single
        // `block_change` would have produced for the same cell.
        let mut reader = Reader::new(payload);
        let chunk_x = reader.i32().map_err(dec_err)?;
        let chunk_z = reader.i32().map_err(dec_err)?;
        // Resolved *before* the record loop, not inside it: the whole point
        // of refusing rather than clamping (see `chunk_origin_block`) is
        // that nothing is written for an out-of-range packet, and a check
        // inside the loop would already have written earlier records.
        let origin_x = chunk_origin_block(chunk_x, "x")?;
        let origin_z = chunk_origin_block(chunk_z, "z")?;
        let count = reader.var_i32().map_err(dec_err)?;
        let count = usize::try_from(count).map_err(|_| {
            AdapterError::Decode(format!("negative multi_block_change record count {count}"))
        })?;
        // A full-height 1.12.2 column holds at most 16*16*256 = 65536
        // cells; cap the pre-allocation so a hostile count cannot force a
        // large speculative allocation before the truncated body is
        // rejected by the per-record reads below.
        let mut by_section: HashMap<i32, Vec<[u8; 3]>> =
            HashMap::with_capacity(count.min(16));
        let mut tally = FallbackTally::default();
        for _ in 0..count {
            let horizontal = reader.u8().map_err(dec_err)?;
            let y = i32::from(reader.u8().map_err(dec_err)?);
            let raw = reader.var_i32().map_err(dec_err)?;
            let raw = u16::try_from(raw)
                .ok()
                .filter(|&raw| raw <= 0x0FFF)
                .ok_or_else(|| {
                    AdapterError::Decode(format!(
                        "multi_block_change composite id {raw} outside the 4095-slot \
                         legacy table"
                    ))
                })?;
            let old_block_id = (raw >> 4) as u8;
            let meta = (raw & 0xF) as u8;
            let state = canonical::resolve_or_air(old_block_id, meta, &mut tally);
            let rel_x = i32::from(horizontal >> 4);
            let rel_z = i32::from(horizontal & 0xF);
            // `rel_x`/`rel_z` are 4-bit nibbles (0..=15) and `origin_*` is
            // already bounded by the world border, so these adds cannot
            // overflow — the guard is at `chunk_origin_block`, above.
            let x = origin_x + rel_x;
            let z = origin_z + rel_z;
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
                .push([rel_x as u8, y.rem_euclid(16) as u8, rel_z as u8]);
        }
        reader.ensure_empty().map_err(dec_err)?;
        if by_section.is_empty() {
            return Ok(Vec::new());
        }
        return Ok(by_section
            .into_iter()
            .map(|(section_y, blocks)| {
                Directive::Emit(ClientEvent::SectionBlocksChanged {
                    section: SectionPos::new(chunk_x, section_y, chunk_z),
                    blocks,
                })
            })
            .collect());
    }

    fn play_open_window(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // `OpenWindow`'s codec already existed and was already tested
        // (`tests/inventory.rs`, wire round trips only); nothing here
        // ever called it, so no 1.12.2 container screen — a chest, a
        // furnace, a crafting table — could ever open.
        let body: OpenWindow = self.decode_body(payload)?;
        let menu_type = resolve_menu_type(&body.inventory_type, body.slot_count);
        return Ok(vec![Directive::Emit(ClientEvent::ScreenOpened {
            window_id: i32::from(body.window_id),
            menu_type,
            title: Text::from_json(&body.window_title),
        })]);
    }

    fn play_close_window(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: CloseWindow = self.decode_body_exact(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::ScreenClosed {
            window_id: i32::from(body.window_id),
        })]);
    }

    fn play_window_items(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // 1.12.2 has no container-synchronization state id (added in a
        // much later version) and does not bundle the cursor item into
        // this packet the way it might elsewhere, so `state_id` is a
        // fixed 0 and `carried_item` stays `None` — this packet
        // genuinely does not say.
        let body: WindowItems = self.decode_body(payload)?;
        let items = body.items.iter().map(slot_to_item_stack).collect();
        return Ok(vec![Directive::Emit(ClientEvent::ContainerContent {
            window_id: i32::from(body.window_id),
            state_id: 0,
            items,
            carried_item: None,
        })]);
    }

    fn play_set_slot(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // 1.12.2 unifies what 26.2 splits into three packets
        // (`SET_CURSOR_ITEM`/`SET_PLAYER_INVENTORY`/`CONTAINER_SET_SLOT`)
        // behind one `window_id` sentinel: `-1` is the cursor (dragged
        // item), `0` is the player's own inventory with no container
        // screen open, anything else is a slot inside that open
        // container — matching exactly the three-way split the canonical
        // model already draws for the modern versions.
        let body: SetSlot = self.decode_body(payload)?;
        let item = slot_to_item_stack(&body.item);
        if body.window_id == -1 {
            return Ok(vec![Directive::Emit(ClientEvent::CursorItemChanged { item })]);
        }
        if body.window_id == 0 {
            return Ok(vec![Directive::Emit(ClientEvent::InventorySlotChanged {
                slot: i32::from(body.slot),
                item,
            })]);
        }
        return Ok(vec![Directive::Emit(ClientEvent::ContainerSlot {
            window_id: i32::from(body.window_id),
            state_id: 0,
            slot: i32::from(body.slot),
            item,
        })]);
    }

    fn play_craft_progress_bar(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // `packet_craft_progress_bar` (minecraft-data 1.12.2, identical
        // to 1.8's shape): `windowId: u8, property: i16, value: i16` — no
        // synchronization state id, so it maps directly onto the same
        // `ContainerData` 26.2's `minecraft:container_set_data` produces.
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

    fn play_title(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Action-multiplexed, verified field-by-field against
        // minecraft-data's 1.12.2 `packet_title`: unlike 1.8, the `text`
        // switch has three cases (`0`/`1`/`2` — title/subtitle/action-bar,
        // the action-bar case 1.11 adds), which pushes the
        // fade-in/stay/fade-out case (times) to `3` and the two
        // argument-less actions to `4`/`5`. Action-bar text always
        // renders as an overlay, so it maps to the same `Chat`
        // `GameInfo` event the dedicated `SET_ACTION_BAR_TEXT` packet
        // uses on 26.2 — there is no such dedicated packet before 1.17,
        // it rides this one instead. `4`/`5` are clear-then-reset, the
        // same pair 26.2's `CLEAR_TITLES` folds into one `resetTimes`
        // bool.
        let mut reader = Reader::new(payload);
        let raw_action = reader.var_i32().map_err(dec_err)?;
        // 1.11 inserted the action-bar case as `2`, shifting times/clear/reset
        // up by one. Normalise the earlier numbering onto the 1.11+ one so the
        // arm bodies below read a single vocabulary; the alternative -- two
        // parallel matches -- is where a mis-numbered case hides.
        let action = if self.protocol >= PROTOCOL_1_11_2 || raw_action < 2 {
            raw_action
        } else {
            raw_action + 1
        };
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
                return Err(AdapterError::Decode(format!(
                    "unknown title action {raw_action} (normalised to {other}) at protocol {}",
                    self.protocol
                )));
            }
        };
        reader.ensure_empty().map_err(dec_err)?;
        return Ok(vec![directive]);
    }

    fn play_tab_complete(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // `packet_tab_complete` (minecraft-data 1.12.2, identical to
        // 1.8's shape): a bare `matches: string[]`, no transaction id and
        // no replacement range (both added in 1.13). See v1-8's identical
        // arm and `pending_tab_complete`'s doc for why the id/range come
        // from the outgoing request instead of the wire.
        let mut reader = Reader::new(payload);
        let count = reader.var_i32().map_err(dec_err)?;
        let count = usize::try_from(count)
            .map_err(|_| AdapterError::Decode(format!("invalid tab_complete count {count}")))?;
        let mut matches = Vec::with_capacity(count.min(reader.remaining()));
        for _ in 0..count {
            matches.push(reader.string(32_767).map_err(dec_err)?);
        }
        reader.ensure_empty().map_err(dec_err)?;
        let pending = self.take_tab_complete();
        let (id, command) = pending
            .map(|p| (p.id, p.command))
            .unwrap_or_else(|| (0, String::new()));
        let start = last_word_index(&command) as i32;
        let length = command.len() as i32 - start;
        let suggestions = matches
            .into_iter()
            .map(|text| lodestone_model::CommandSuggestionEntry { text, tooltip: None })
            .collect();
        return Ok(vec![Directive::Emit(ClientEvent::CommandSuggestionsReceived {
            id,
            start,
            length,
            suggestions,
        })]);
    }

    fn play_player_info(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // A single `action` applies to every entry in the packet —
        // verified against minecraft-data's 1.12.2 `packet_player_info`
        // `switch`, byte-identical to 1.8's shape, unlike 26.2's
        // per-entry action bitmask. See `packets::player_info`'s module
        // doc.
        let body: PlayerInfo = self.decode_body_exact(payload)?;
        let mut updated = Vec::new();
        let mut removed = Vec::new();
        for entry in body.entries {
            let blank = || PlayerListEntry {
                uuid: Some(entry.uuid),
                name: None,
                game_mode: None,
                latency: None,
                display_name: None,
                // 1.12.2 has no separate "listed" bit — every entry the
                // server sends is, by construction, in the tab list.
                listed: None,
                properties: None,
                // 1.12.2 predates secure chat sessions entirely.
                chat_session: None,
                // 1.12.2 predates both `UPDATE_LIST_ORDER` and `UPDATE_HAT`
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

    fn play_held_item_slot(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // A single signed byte, the newly-selected hotbar index —
        // verified against minecraft-data's 1.12.2
        // `packet_held_item_slot` (identical shape at every later
        // version through 26.2). The already-defined [`HeldItemSlot`]
        // struct (`packets::window`) was never dispatched from here;
        // this is that decoder's first caller.
        let body: HeldItemSlot = self.decode_body(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::HeldSlotChanged {
            slot: i32::from(body.slot),
        })]);
    }

    fn play_abilities(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Signed-byte flags (bit 0x01 invulnerable, 0x02 flying, 0x04 can
        // fly, 0x08 instabuild), then f32 flying speed, f32 walking speed
        // — verified against minecraft-data's 1.12.2 `packet_abilities`,
        // byte-identical to 1.8's shape. 1.12.2 reuses one packet *name*
        // for both directions with different flag semantics (the
        // serverbound `abilities` this crate already encodes for
        // `SetFlying` carries only the flying bit); the clientbound
        // shape decoded here is byte-identical, so it is hand-decoded
        // rather than routed through the serverbound-tagged
        // [`PlayerAbilities`] struct to avoid conflating the two
        // directions' meaning.
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

    fn play_block_action(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Packed position, two opaque bytes, then a varint legacy block
        // *type* id — verified against minecraft-data's 1.12.2
        // `packet_block_action` (identical to 1.8's shape). Without this,
        // no note block ever plays, no piston ever animates, and no
        // chest lid ever opens for a 1.12.2 connection: those are all
        // this packet, not `block_change`.
        let body: BlockAction = self.decode_body_exact(payload)?;
        let block_id = u8::try_from(body.block_id).map_err(|_| {
            AdapterError::Decode(format!(
                "block_action block id {} is outside the legacy 0..=255 block-type space",
                body.block_id
            ))
        })?;
        // `block_action`'s wire shape carries no metadata component at
        // all (unlike `block_change`'s `id:meta` composite), and every
        // block that can trigger this event resolves to the same
        // canonical block *family* key regardless of metadata — only
        // within-family blockstate properties such as piston facing vary
        // with it, and this event's `block` field only ever needs the
        // family (see `ClientEvent::BlockEvent`'s doc). But `meta = 0` is
        // not always a populated slot in the legacy flattening table:
        // measured (`lodestone_v1_9::canonical::resolve`, a debug probe
        // over every meta) that a legacy chest/ender_chest/trapped_chest
        // id has **no** entry at meta `0` or `1` at all — those metas
        // were never a real chest orientation, only `2..=5` (facing)
        // were — so a fixed `meta = 0` would silently resolve every
        // chest-lid `block_action` to air. Scanning every meta and
        // taking the first `Resolved` slot is family-only-safe (any
        // meta the table does populate names the same block) and
        // correct for every id this packet has been observed to carry.
        let block = (0u8..16)
            .find_map(|meta| match canonical::resolve(block_id, meta) {
                canonical::CanonicalBlockState::Resolved(state) => Some(state),
                _ => None,
            })
            .unwrap_or_else(block_states::air_state)
            .block();
        let key: ResourceKey = block
            .name()
            .parse()
            .expect("built-in block names are valid resource keys");
        return Ok(vec![Directive::Emit(ClientEvent::BlockEvent {
            pos: body.location.0,
            b0: body.byte1,
            b1: body.byte2,
            block: key,
        })]);
    }

    fn play_entity_equipment(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Verified against minecraft-data's 1.12.2
        // `packet_entity_equipment`: a varint entity id, a varint
        // `EquipmentSlot` ordinal, then a `slot` item stack. Unlike the
        // modern packet this carries exactly one slot per message, so
        // the emitted `equipment` vec always has a single entry.
        let body: ClientboundEntityEquipment = self.decode_body_exact(payload)?;
        let ordinal = u8::try_from(body.slot).map_err(|_| {
            AdapterError::Decode(format!(
                "entity_equipment slot ordinal {} is outside u8 range",
                body.slot
            ))
        })?;
        let slot = EquipmentSlot::from_ordinal(ordinal).ok_or_else(|| {
            AdapterError::Decode(format!("unknown entity_equipment slot ordinal {ordinal}"))
        })?;
        let item = slot_to_item_stack(&body.item);
        return Ok(vec![Directive::Emit(ClientEvent::EntityEquipmentUpdated {
            entity_id: body.entity_id,
            equipment: vec![EntityEquipment { slot, item }],
        })]);
    }

    fn play_animation(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Verified against minecraft-data's 1.12.2 `packet_animation`: a
        // varint entity id, then a raw animation code byte. See
        // `Animation`'s own doc for the code table and why `1` maps to
        // `Other` rather than a named variant.
        let body: Animation = self.decode_body_exact(payload)?;
        let action = match body.animation {
            0 => AnimationAction::SwingMainHand,
            2 => AnimationAction::WakeUp,
            3 => AnimationAction::SwingOffHand,
            4 => AnimationAction::CriticalHit,
            5 => AnimationAction::MagicCriticalHit,
            other => AnimationAction::Other(other),
        };
        return Ok(vec![Directive::Emit(ClientEvent::EntityAnimation {
            entity_id: body.entity_id,
            action,
        })]);
    }

    fn play_named_sound_effect(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Verified against minecraft-data's 1.12.2
        // `packet_named_sound_effect`. `x`/`y`/`z` are vanilla's
        // fixed-point sound-position convention (real coordinate × 8);
        // this era carries no fixed audible range and no variant seed,
        // so both canonical fields are the "not present" default.
        // 1.10 widened the pitch from a quantised byte to a float; below that
        // the byte form is a different struct (see `packets::game`).
        let body: NamedSoundEffect = if self.protocol >= PROTOCOL_1_10_2 {
            self.decode_body_exact(payload)?
        } else {
            let legacy: NamedSoundEffectBytePitch = self.decode_body_exact(payload)?;
            NamedSoundEffect {
                sound_name: legacy.sound_name,
                sound_category: legacy.sound_category,
                x: legacy.x,
                y: legacy.y,
                z: legacy.z,
                volume: legacy.volume,
                pitch: legacy_pitch(legacy.pitch),
            }
        };
        let category_ordinal = u8::try_from(body.sound_category).map_err(|_| {
            AdapterError::Decode(format!(
                "named_sound_effect category {} is outside u8 range",
                body.sound_category
            ))
        })?;
        let category = SoundCategory::from_ordinal(category_ordinal).ok_or_else(|| {
            AdapterError::Decode(format!("unknown sound category {category_ordinal}"))
        })?;
        let sound: ResourceKey = body.sound_name.parse().map_err(|_| {
            AdapterError::Decode(format!(
                "named_sound_effect sound name {:?} is not a valid resource key",
                body.sound_name
            ))
        })?;
        return Ok(vec![Directive::Emit(ClientEvent::Sound {
            sound,
            category,
            pos: Vec3 {
                x: f64::from(body.x) / 8.0,
                y: f64::from(body.y) / 8.0,
                z: f64::from(body.z) / 8.0,
            },
            volume: body.volume,
            pitch: body.pitch,
            fixed_range: None,
            seed: 0,
        })]);
    }

    fn play_sound_effect(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Identical shape to `NAMED_SOUND_EFFECT` except the leading
        // field is a varint `SoundEvent` registry id rather than a
        // string name — resolved through the generated legacy
        // `sound_ids` table (`vendor/minecraft-data`'s
        // `pc/1.12.2/sounds.json`, wire-order network ids).
        let body: SoundEffect = if self.protocol >= PROTOCOL_1_10_2 {
            self.decode_body_exact(payload)?
        } else {
            let legacy: SoundEffectBytePitch = self.decode_body_exact(payload)?;
            SoundEffect {
                sound_id: legacy.sound_id,
                sound_category: legacy.sound_category,
                x: legacy.x,
                y: legacy.y,
                z: legacy.z,
                volume: legacy.volume,
                pitch: legacy_pitch(legacy.pitch),
            }
        };
        let category_ordinal = u8::try_from(body.sound_category).map_err(|_| {
            AdapterError::Decode(format!(
                "sound_effect category {} is outside u8 range",
                body.sound_category
            ))
        })?;
        let category = SoundCategory::from_ordinal(category_ordinal).ok_or_else(|| {
            AdapterError::Decode(format!("unknown sound category {category_ordinal}"))
        })?;
        let name = crate::sound_ids::sound_name(body.sound_id).ok_or_else(|| {
            AdapterError::Decode(format!("unknown legacy sound id {}", body.sound_id))
        })?;
        let sound: ResourceKey = name.parse().map_err(|_| {
            AdapterError::Decode(format!(
                "resolved sound name {name:?} is not a valid resource key"
            ))
        })?;
        return Ok(vec![Directive::Emit(ClientEvent::Sound {
            sound,
            category,
            pos: Vec3 {
                x: f64::from(body.x) / 8.0,
                y: f64::from(body.y) / 8.0,
                z: f64::from(body.z) / 8.0,
            },
            volume: body.volume,
            pitch: body.pitch,
            fixed_range: None,
            seed: 0,
        })]);
    }

    fn play_scoreboard_display_objective(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Verified against minecraft-data's 1.12.2
        // `packet_scoreboard_display_objective`: a raw `i8` slot
        // position, then a string objective name. This protocol
        // revision only ever sends 0/1/2 — the per-team-colour sidebar
        // slots are a later addition — and clears the slot with an
        // empty string rather than a dedicated marker.
        let body: ScoreboardDisplayObjective = self.decode_body_exact(payload)?;
        let slot = match body.position {
            0 => DisplaySlot::List,
            1 => DisplaySlot::Sidebar,
            2 => DisplaySlot::BelowName,
            other => {
                return Err(AdapterError::Decode(format!(
                    "unknown scoreboard display slot {other}"
                )));
            }
        };
        let objective = if body.name.is_empty() {
            None
        } else {
            Some(body.name)
        };
        return Ok(vec![Directive::Emit(ClientEvent::DisplayObjective {
            slot,
            objective,
        })]);
    }

    fn play_scoreboard_objective(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Mode-multiplexed (minecraft-data's 1.12.2
        // `packet_scoreboard_objective`), so this is a hand-decoded
        // `Reader` walk rather than a derived struct, mirroring
        // `block_change`/`entity_status`'s treatment of the same shape.
        // `displayText` is a **plain** legacy-formatted string at this
        // protocol revision (JSON scoreboard text is a 1.13+ addition),
        // so it goes through `Text::from_legacy`, not `from_json`.
        let mut reader = Reader::new(payload);
        let name = reader.string(16).map_err(dec_err)?;
        let action = reader.i8().map_err(dec_err)?;
        let event = match action {
            0 | 2 => {
                let display_text = reader.string(64).map_err(dec_err)?;
                let render_type_str = reader.string(16).map_err(dec_err)?;
                let render_type = match render_type_str.as_str() {
                    "integer" => ObjectiveRenderType::Integer,
                    "hearts" => ObjectiveRenderType::Hearts,
                    other => {
                        return Err(AdapterError::Decode(format!(
                            "unknown objective render type {other:?}"
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
                    display_name: Some(Text::from_legacy(&display_text)),
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

    fn play_scoreboard_score(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Verified against minecraft-data's 1.12.2
        // `packet_scoreboard_score`: `itemName` is the score *holder*
        // and `scoreName` is the *objective* — the mcdata field names
        // are misleading, not the wire order. `scoreName` is read
        // unconditionally (unlike `value`), so a `remove` action still
        // names exactly one objective, never "reset all".
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

    fn play_teams(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Mode-multiplexed (minecraft-data's 1.12.2 `packet_teams`), so
        // this is a hand-decoded `Reader` walk. Modes `0`
        // (create) and `2` (update) share the full parameter block;
        // `0` additionally carries the initial member list, and `3`/`4`
        // (add/remove members) carry only a member list. `friendlyFire`
        // packs two flags in one byte (`0x01` friendly fire, `0x02` see
        // friendly invisibles), a convention unchanged since 1.8. The
        // member-list count is capped against the
        // payload's own remaining length before `Vec::with_capacity`,
        // since `players` is exactly the attacker-influenced unbounded
        // wire count this crate's own trap list warns about.
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
                let display_name = reader.string(16).map_err(dec_err)?;
                let prefix = reader.string(16).map_err(dec_err)?;
                let suffix = reader.string(16).map_err(dec_err)?;
                let friendly_flags = reader.i8().map_err(dec_err)?;
                let visibility_str = reader.string(32).map_err(dec_err)?;
                let collision_str = reader.string(32).map_err(dec_err)?;
                let color_byte = reader.i8().map_err(dec_err)?;
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
                    display_name: Text::from_legacy(&display_name),
                    prefix: Text::from_legacy(&prefix),
                    suffix: Text::from_legacy(&suffix),
                    name_tag_visibility,
                    collision_rule,
                    color: team_color_from_byte(color_byte),
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

    fn play_boss_bar(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Action-multiplexed (minecraft-data's 1.12.2 `packet_boss_bar`),
        // so this is a hand-decoded `Reader` walk. Title is a JSON chat
        // component at this protocol revision (unlike the plain-string
        // scoreboard/team fields above — the boss bar packet has carried
        // `IChatComponent` since its 1.9 introduction, predating the
        // 1.13 scoreboard/team JSON migration), so it goes through
        // `Text::from_json`. `flags` packs three bits: `0x01` darken
        // sky, `0x02` boss music, `0x04` create fog.
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

    fn play_spawn_position(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnPosition = self.decode_body(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
            dimension: dimension_id(self.current_dimension())?,
            pos: body.location.0,
            angle: 0.0,
            pitch: 0.0,
        })]);
    }

    fn play_update_time(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: UpdateTime = self.decode_body(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::TimeChanged {
            world_age: body.age,
            time_of_day: body.time,
        })]);
    }

    fn play_difficulty(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: DifficultyPacket = self.decode_body(payload)?;
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
        // 1.12.2 has no "locked" bit — that is a later addition.
        return Ok(vec![Directive::Emit(ClientEvent::DifficultyChanged {
            difficulty,
            locked: false,
        })]);
    }

    fn play_playerlist_header(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: PlayerlistHeader = self.decode_body(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::TabListChanged {
            header: Text::from_json(&body.header),
            footer: Text::from_json(&body.footer),
        })]);
    }

    fn play_attach_entity(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: AttachEntity = self.decode_body(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::EntityLeashed {
            entity_id: body.entity_id,
            holder_id: (body.vehicle_id != 0).then_some(body.vehicle_id),
        })]);
    }

    fn play_set_passengers(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: SetPassengers = self.decode_body(payload)?;
        return Ok(vec![Directive::Emit(
            ClientEvent::EntityPassengersChanged {
                vehicle_id: body.entity_id,
                passenger_ids: body.passengers,
            },
        )]);
    }

    fn play_collect(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: Collect = self.decode_body(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::ItemPickup {
            item_entity_id: body.collected_entity_id,
            player_id: body.collector_entity_id,
            // 1.11 added the stack size to this packet; 110 and 210 decode it
            // as `0`, which is what the wire actually says -- the count is not
            // knowable from this packet alone before 1.11, and inventing one
            // would be worse than reporting the absence.
            amount: body.pickup_item_count,
        })]);
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

    /// Resolves a validated legacy effect id to its canonical event key.
    fn legacy_mob_effect_key(effect_id: MobEffectId) -> Result<ResourceKey, AdapterError> {
        let name = mob_effect_name_for(effect_id);
        name.parse()
            .map_err(|_| AdapterError::Decode(format!("effect id {name} is not a key")))
    }

    fn play_entity_effect(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityEffect = self.decode_body(payload)?;
        let effect_id = Self::legacy_mob_effect_id(i32::from(body.effect_id))?;
        let effect = Self::legacy_mob_effect_key(effect_id)?;
        return Ok(vec![Directive::Emit(ClientEvent::MobEffectApplied {
            entity_id: body.entity_id,
            effect,
            amplifier: i32::from(body.amplifier),
            duration_ticks: body.duration,
            ambient: body.flags & 0x01 != 0,
            visible: body.flags & 0x02 != 0,
            // Neither bit exists at this protocol revision: vanilla
            // 1.12.2 always shows the HUD icon and never blends.
            show_icon: true,
            blend: false,
        })]);
    }

    fn play_remove_entity_effect(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: RemoveEntityEffect = self.decode_body(payload)?;
        let effect_id = Self::legacy_mob_effect_id(i32::from(body.effect_id))?;
        let effect = Self::legacy_mob_effect_key(effect_id)?;
        return Ok(vec![Directive::Emit(ClientEvent::MobEffectRemoved {
            entity_id: body.entity_id,
            effect,
        })]);
    }

    fn play_spawn_entity_weather(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnEntityWeather = self.decode_body(payload)?;
        let entity_type: ResourceKey = "minecraft:lightning_bolt"
            .parse()
            .map_err(|_| AdapterError::Decode("lightning_bolt key invalid".to_owned()))?;
        return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
            entity_id: body.entity_id,
            uuid: None,
            entity_type,
            pos: Vec3::new(body.x, body.y, body.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        })]);
    }

    fn play_spawn_entity_experience_orb(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnEntityExperienceOrb = self.decode_body(payload)?;
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

    fn play_world_particles(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Fixed-width prefix (verified against minecraft-data's 1.12.2
        // `packet_world_particles`), then a legacy particle id whose
        // rename crosswalk into the modern `minecraft:particle_type`
        // registry lives in `crate::particle_ids` (see that module's
        // docs for how it was derived — a real decompile, not a guess).
        // Three legacy ids carry extra type-specific VarInts
        // (`particle_ids::extra_varint_count`); this crate cannot yet
        // model their payload as a typed `ParticleOptions` variant
        // (`lodestone-model` is off limits here — see the brokered-hunk
        // note this pass reports), so they are read and discarded, same
        // as `lodestone-v26-2` does for any particle name it does not
        // specifically parse a payload for.
        let mut reader = Reader::new(payload);
        let particle_id = reader.i32().map_err(dec_err)?;
        let long_distance = reader.bool().map_err(dec_err)?;
        let x = reader.f32().map_err(dec_err)?;
        let y = reader.f32().map_err(dec_err)?;
        let z = reader.f32().map_err(dec_err)?;
        let offset_x = reader.f32().map_err(dec_err)?;
        let offset_y = reader.f32().map_err(dec_err)?;
        let offset_z = reader.f32().map_err(dec_err)?;
        let max_speed = reader.f32().map_err(dec_err)?;
        let count = reader.i32().map_err(dec_err)?;
        for _ in 0..particle_ids::extra_varint_count(particle_id) {
            reader.var_i32().map_err(dec_err)?;
        }
        reader.ensure_empty().map_err(dec_err)?;
        let name = particle_ids::particle_key(particle_id).ok_or_else(|| {
            AdapterError::Decode(format!("unmapped legacy particle id {particle_id}"))
        })?;
        let particle: ResourceKey = name
            .parse()
            .map_err(|_| AdapterError::Decode(format!("particle id {name} is not a key")))?;
        return Ok(vec![Directive::Emit(ClientEvent::Particles {
            particle,
            long_distance,
            // 1.12's `WORLD_PARTICLES` has no always-show field: the
            // packet carries `longDistance` and nothing else, and the
            // flag was added with the 26.2-era packet. `false` is
            // therefore the honest value here, not an unported one --
            // there is nothing on this wire to port.
            always_show: false,
            pos: Vec3::new(f64::from(x), f64::from(y), f64::from(z)),
            offset: Vec3f::new(offset_x, offset_y, offset_z),
            max_speed,
            count,
            options: ParticleOptions::None,
        })]);
    }

    fn play_experience(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
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

    fn play_vehicle_move(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
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

    fn play_set_cooldown(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let item_id = reader.var_i32().map_err(dec_err)?;
        let cooldown_ticks = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        let item_id =
            i16::try_from(item_id).map_err(|_| AdapterError::Decode(format!(
                "cooldown item id {item_id} out of legacy item-id range"
            )))?;
        let name = item_types::item_name(item_id).ok_or_else(|| {
            AdapterError::Decode(format!("unknown legacy item id {item_id} in set_cooldown"))
        })?;
        let group: ResourceKey = name
            .parse()
            .map_err(|_| AdapterError::Decode(format!("item id {name} is not a key")))?;
        return Ok(vec![Directive::Emit(ClientEvent::ItemCooldown {
            group,
            duration_ticks: cooldown_ticks,
        })]);
    }

    fn play_combat_event(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Action-multiplexed, verified field-by-field against
        // minecraft-data's 1.12.2 `packet_combat_event`: event `0` (enter
        // combat) carries nothing further; event `1` (end combat) reads a
        // VarInt duration then a raw `i32` entity id (the model's
        // `PlayerCombatEnded` has no slot for the id, so it is read and
        // discarded — matching 26.2's own `ClientboundPlayerCombatEndPacket`,
        // which dropped it too); event `2` (entity died) reads a VarInt
        // player id, a raw `i32` entity id, then a JSON death-message
        // string, both discarded except the message, matching modern
        // `PLAYER_COMBAT_KILL`'s shape (`lodestone-v26-2`'s adapter).
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

    fn play_world_border(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // Action-multiplexed, verified field-by-field against
        // minecraft-data's 1.12.2 `packet_world_border`. Action `3`
        // ("initialize") is the only one that carries every field, in
        // this exact order: x, z, old_radius, new_radius, speed (VarLong
        // lerp-time ms), portal_boundary (VarInt absolute max size),
        // warning_time, warning_blocks — matching
        // `ClientEvent::WorldBorderInitialized`'s field order one-for-one.
        let mut reader = Reader::new(payload);
        // Unlike `title`, this packet's action vocabulary is unchanged across
        // every protocol in this era -- verified against a real 1.9.4
        // `world_border` capture, whose action-3 body decodes with zero
        // trailing bytes here and did not before this comment's own code was
        // corrected.
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

    fn play_open_sign_entity(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: OpenSignEntity = self.decode_body(payload)?;
        return Ok(vec![Directive::Emit(ClientEvent::SignEditorOpened {
            pos: body.location.0,
            // 1.12.2 has no front/back text distinction — that is a
            // later (1.20) addition, so this always edits the one text
            // a legacy sign has.
            is_front_text: true,
        })]);
    }

    fn play_select_advancement_tab(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        // An optional string, verified against minecraft-data's 1.12.2
        // `packet_select_advancement_tab`: a presence bool then the
        // string when present — hand-decoded because the derive macro's
        // `Option<T>` models a `#[mc(present_if = ...)]` condition on
        // another field, not a wire presence byte.
        let mut reader = Reader::new(payload);
        let present = reader.bool().map_err(dec_err)?;
        let tab = if present {
            let id = reader.string(32767).map_err(dec_err)?;
            let identifier: Identifier = id
                .parse()
                .map_err(|_| AdapterError::Decode(format!("invalid tab id {id}")))?;
            Some(identifier)
        } else {
            None
        };
        reader.ensure_empty().map_err(dec_err)?;
        return Ok(vec![Directive::Emit(ClientEvent::AdvancementsTabSelected {
            tab,
        })]);
    }

    fn play_spawn_entity_painting(&self, _world: &mut dyn WorldSink, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnEntityPainting = self.decode_body(payload)?;
        let entity_type: ResourceKey = "minecraft:painting"
            .parse()
            .map_err(|_| AdapterError::Decode("painting key invalid".to_owned()))?;
        let pos = body.location.0;
        return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
            entity_id: body.entity_id,
            uuid: Some(body.entity_uuid),
            entity_type,
            pos: Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)),
            // The legacy motive name and facing direction have no home
            // in this crate yet (no legacy motive -> modern
            // `minecraft:painting_variant` crosswalk, and no yaw
            // conversion for the facing byte) — dropped, same treatment
            // as `spawn_entity_painting`'s other unmodelled fields.
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        })]);
    }
}

impl VersionAdapter for V340Adapter {
    fn protocol_version(&self) -> i32 {
        self.protocol
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["1.9.4", "1.10.2", "1.11.2", "1.12.2"]
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
        // 1.8 login_start carries only the username: there is no client-provided
        // profile UUID, unlike the modern login hello packet.
        let login_start = crate::packets::login::LoginStart {
            username: profile.username.clone(),
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
                let payload = if self.protocol >= PROTOCOL_1_12_2 {
                    self.encode_body(&KeepAliveResponse { id: *id })?
                } else {
                    // The pre-1.12 id is a VarInt. The server only ever sends
                    // ids it can itself round-trip through one, so a value
                    // outside `i32` here means the id did not come from this
                    // connection's own challenge -- refuse rather than
                    // truncate into a response the server will not match.
                    let id = i32::try_from(*id).map_err(|_| {
                        AdapterError::Unsupported(format!(
                            "protocol {} carries the keep_alive id as a VarInt; {id} does not fit",
                            self.protocol
                        ))
                    })?;
                    self.encode_body(&KeepAliveResponseVarInt { id })?
                };
                Ok(Some((self.ids().keep_alive, payload)))
            }
            ClientAction::SendChat { text } => {
                let body = ServerboundChat {
                    message: text.clone(),
                };
                Ok(Some((self.ids().chat, self.encode_body(&body)?)))
            }
            // 1.8 has no dedicated command packet: a command is a chat message
            // beginning with a slash.
            ClientAction::SendCommand { command } => {
                let body = ServerboundChat {
                    message: format!("/{command}"),
                };
                Ok(Some((self.ids().chat, self.encode_body(&body)?)))
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
                    self.ids().arm_animation,
                    self.encode_body(&body)?,
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
                Ok(Some((self.ids().block_dig, self.encode_body(&body)?)))
            }
            // Item dropping also rides on `block_dig` (statuses 3/4).
            ClientAction::DropSelectedItemStack => {
                let body = BlockDig {
                    status: 3,
                    location: Position::new(0, 0, 0),
                    face: 0,
                };
                Ok(Some((self.ids().block_dig, self.encode_body(&body)?)))
            }
            ClientAction::DropSelectedItem => {
                let body = BlockDig {
                    status: 4,
                    location: Position::new(0, 0, 0),
                    face: 0,
                };
                Ok(Some((self.ids().block_dig, self.encode_body(&body)?)))
            }
            ClientAction::ReleaseUseItem => {
                let body = BlockDig {
                    status: 5,
                    location: Position::new(0, 0, 0),
                    face: 0,
                };
                Ok(Some((self.ids().block_dig, self.encode_body(&body)?)))
            }
            // 1.9+ off-hand swap is `block_dig` status 6 (unlike protocol 47,
            // which has no off-hand and rejects this action).
            ClientAction::SwapItemWithOffhand => {
                let body = BlockDig {
                    status: 6,
                    location: Position::new(0, 0, 0),
                    face: 0,
                };
                Ok(Some((self.ids().block_dig, self.encode_body(&body)?)))
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
                let payload = if self.protocol >= PROTOCOL_1_11_2 {
                    self.encode_body(&BlockPlace {
                        location: Position(*pos),
                        direction: face_ordinal(*face),
                        hand: hand_ordinal(*hand),
                        cursor_x: cursor.x,
                        cursor_y: cursor.y,
                        cursor_z: cursor.z,
                    })?
                } else {
                    // 110 and 210 carry the cursor as three
                    // sixteenth-of-a-face bytes, not three floats.
                    self.encode_body(&BlockPlaceByteCursor {
                        location: Position(*pos),
                        direction: face_ordinal(*face),
                        hand: hand_ordinal(*hand),
                        cursor_x: quantise_cursor(cursor.x),
                        cursor_y: quantise_cursor(cursor.y),
                        cursor_z: quantise_cursor(cursor.z),
                    })?
                };
                Ok(Some((self.ids().block_place, payload)))
            }
            // Using an item in the air: `block_place` with location (-1,-1,-1) and
            // direction -1.
            ClientAction::UseItem {
                hand,
                rotation: _,
                sequence: _,
            } => {
                let payload = if self.protocol >= PROTOCOL_1_11_2 {
                    self.encode_body(&BlockPlace {
                        location: Position::new(-1, -1, -1),
                        direction: -1,
                        hand: hand_ordinal(*hand),
                        cursor_x: 0.0,
                        cursor_y: 0.0,
                        cursor_z: 0.0,
                    })?
                } else {
                    // 110 and 210 carry the cursor as three
                    // sixteenth-of-a-face bytes, not three floats.
                    self.encode_body(&BlockPlaceByteCursor {
                        location: Position::new(-1, -1, -1),
                        direction: -1,
                        hand: hand_ordinal(*hand),
                        cursor_x: quantise_cursor(0.0),
                        cursor_y: quantise_cursor(0.0),
                        cursor_z: quantise_cursor(0.0),
                    })?
                };
                Ok(Some((self.ids().block_place, payload)))
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
                    Ok(Some((self.ids().use_entity, self.encode_body(&body)?)))
                }
                EntityInteraction::Interact { hand } => {
                    let body = UseEntityInteract {
                        target: *entity_id,
                        mouse: 0,
                        hand: hand_ordinal(*hand),
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
                    };
                    Ok(Some((self.ids().use_entity, self.encode_body(&body)?)))
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
                    self.ids().entity_action,
                    self.encode_body(&body)?,
                )))
            }

            // Inventory. Close/select ride on plain packets. Clearing a creative
            // slot sends an empty slot; a non-empty creative slot needs an item
            // registry (ResourceKey -> numeric id) that no crate has yet.
            ClientAction::ContainerClose { window_id } => {
                let body = ServerboundCloseWindow {
                    window_id: *window_id as u8,
                };
                Ok(Some((self.ids().close_window, self.encode_body(&body)?)))
            }
            ClientAction::SetCarriedItem { slot } => {
                let body = ServerboundHeldItemSlot { slot: *slot as i16 };
                Ok(Some((
                    self.ids().held_item_slot,
                    self.encode_body(&body)?,
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
                    self.ids().set_creative_slot,
                    self.encode_body(&body)?,
                )))
            }
            // Container clicks predate the modern `state_id` reconciliation.
            // Faithfully encoding 1.12's `window_click` needs a client-tracked
            // transaction id (the `action` counter, absent from the model which
            // carries only the 1.17+ `state_id`; this adapter tracks other
            // per-connection state, `pending_tab_complete`, but not this), an
            // item registry (`ResourceKey` -> numeric id) for the clicked stack,
            // and item metadata/damage that pre-1.13 slots carry but the model's
            // `ItemStack { item, count }` cannot express. Refused loudly rather
            // than encoded with wrong bytes that a live server rejects via a
            // failed transaction (silently dropping the click).
            //
            // This is also why clientbound `TRANSACTION` has no decode arm: it
            // exists solely to accept or reject a `window_click` this client
            // cannot yet send, so nothing here could ever receive one — wiring a
            // decode for it now would be an event with no producer that could
            // trigger it. It becomes real work once `ContainerClick` above is.
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
                Ok(Some((self.ids().settings, self.encode_body(&body)?)))
            }
            ClientAction::SendBrand { brand } => {
                let body = BrandPayload {
                    channel: "MC|Brand".to_owned(),
                    brand: brand.clone(),
                };
                Ok(Some((
                    self.ids().custom_payload,
                    self.encode_body(&body)?,
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
                Ok(Some((self.ids().enchant_item, self.encode_body(&body)?)))
            }
            ClientAction::SetFlying { flying } => {
                let body = PlayerAbilities {
                    flags: if *flying { ABILITY_FLYING } else { 0 },
                    flying_speed: DEFAULT_FLYING_SPEED,
                    walking_speed: DEFAULT_WALKING_SPEED,
                };
                Ok(Some((self.ids().abilities, self.encode_body(&body)?)))
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
                if self.protocol == PROTOCOL_1_9_4 {
                    // 1.9.4 alone expects the pushed pack's hash echoed back,
                    // and `resource_pack_send` is on this family's IGNORED
                    // list, so nothing here ever saw one. Sending an empty
                    // hash would be a value we invented rather than one the
                    // server pushed; refusing says so.
                    return Err(AdapterError::Unsupported(
                        "protocol 110 resource_pack_receive echoes the pushed pack's hash, \
                         which this family does not yet capture (resource_pack_send is not \
                         translated)"
                            .to_owned(),
                    ));
                }
                let body = ResourcePackReceive {
                    // Dropped in 1.10; `until = 110` keeps it off the wire for
                    // every protocol that reaches here.
                    hash: String::new(),
                    result,
                };
                Ok(Some((
                    self.ids().resource_pack_receive,
                    self.encode_body(&body)?,
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
            ClientAction::CommandSuggestion { id, command } => {
                // `packet_tab_complete` (minecraft-data 1.12.2): `text:
                // string, assumeCommand: bool, lookedAtBlock:
                // option<position>`. `command` already carries the leading
                // slash the way every chat-box request does, so
                // `assumeCommand` (for callers that omit it, e.g. a command
                // block) stays `false`; this client never tracks a
                // looked-at block either. `id` has nowhere to go on the
                // wire and is remembered instead (see `pending_tab_complete`).
                self.remember_tab_complete(*id, command.clone());
                let mut writer = Writer::default();
                writer.string(command);
                writer.bool(false);
                writer.bool(false);
                Ok(Some((self.ids().tab_complete, writer.into_vec())))
            }
            ClientAction::PaddleBoat { .. } => Err(AdapterError::Unsupported(
                "protocol 340 paddle boat encoding is not yet implemented".to_owned(),
            )),
            ClientAction::MoveVehicle { .. } => Err(AdapterError::Unsupported(
                "protocol 340 move vehicle encoding is not yet implemented".to_owned(),
            )),

            // Leaving the death screen. `client_command` action `0` =
            // perform respawn, a stable ordinal across every generation
            // checked (1.8, 1.12.2, 1.16.2/.4/.5 all encode it as a lone
            // varint action id per minecraft-data's protocol.json).
            ClientAction::Respawn => {
                let body = ClientCommand { action: 0 };
                Ok(Some((self.ids().client_command, self.encode_body(&body)?)))
            }
            // Clicking a name in the tab list while spectating. 1.12.2's
            // `spectate` packet carries the target's uuid directly, which the
            // model already supplies, so no entity registry is needed.
            ClientAction::TeleportToEntity { target } => {
                let body = Spectate { target: *target };
                Ok(Some((self.ids().spectate, self.encode_body(&body)?)))
            }
            // The continuous spectator-follow action carries only a network
            // entity id, but 1.12.2's wire packet is the same uuid-keyed
            // `spectate` packet as `TeleportToEntity` above. A stateless
            // adapter has no id->uuid registry to bridge the two.
            ClientAction::SpectatorAction { .. } => Err(AdapterError::Unsupported(
                "protocol 340's spectate packet needs a target uuid; SpectatorAction carries \
                 only a network entity id with no registry to resolve it into one (use \
                 TeleportToEntity instead, which already carries the uuid)"
                    .to_owned(),
            )),
            ClientAction::ChatAck { .. } => Err(AdapterError::Unsupported(
                "protocol 340 predates signed/acknowledged chat (added in 1.19)".to_owned(),
            )),
            ClientAction::SelectBundleItem { .. } => Err(AdapterError::Unsupported(
                "protocol 340 predates bundles (added in 1.21.2)".to_owned(),
            )),
            ClientAction::SetContainerSlotState { .. } => Err(AdapterError::Unsupported(
                "protocol 340 predates the crafter block (added in 1.21)".to_owned(),
            )),
            // 1.12.2 has only the crafting-table recipe book; the
            // furnace/blast-furnace/smoker books arrived in 1.13. A
            // non-crafting book type has no wire form to encode into.
            ClientAction::SetRecipeBookSettings {
                book_type,
                open,
                filtering,
            } => {
                if *book_type != RecipeBookType::Crafting {
                    return Err(AdapterError::Unsupported(
                        "protocol 340 predates furnace/blast furnace/smoker recipe books \
                         (added in 1.13)"
                            .to_owned(),
                    ));
                }
                let Some(packet_id) = self.ids().crafting_book_data else {
                    return Err(AdapterError::Unsupported(format!(
                        "protocol {} predates the recipe book entirely (added in 1.12); \
                         there is no crafting_book_data packet to encode into",
                        self.protocol
                    )));
                };
                Ok(Some((
                    packet_id,
                    encode_crafting_book_settings(*open, *filtering),
                )))
            }
            // Both packets identify a recipe by 1.12.2's legacy recipe
            // registry id (`craft_recipe_request`: varint; `crafting_book_data`
            // type 0: i32), which this stateless adapter has no registry to
            // resolve the model's display index into safely.
            ClientAction::RecipeBookSeenRecipe { .. } | ClientAction::PlaceRecipe { .. } => {
                Err(AdapterError::Unsupported(
                    "protocol 340's recipe-book recipe identity needs a legacy recipe registry \
                     this adapter does not have; the model's display index cannot be safely \
                     forwarded without one"
                        .to_owned(),
                ))
            }
            ClientAction::PingRequest { .. } => Err(AdapterError::Unsupported(
                "protocol 340 has no play-state ping request packet".to_owned(),
            )),
            ClientAction::ChangeGameMode { .. } => Err(AdapterError::Unsupported(
                "protocol 340 has no dedicated change_game_mode packet; a debug-menu game-mode \
                 switch in this era goes through the /gamemode chat command instead"
                    .to_owned(),
            )),

            _ => Ok(None),
        }
    }
}

#[cfg(test)]
use crate::packet_ids::play;

#[cfg(test)]
mod movement_tests {
    use super::*;

    #[test]
    fn poisoned_movement_state_is_recovered() {
        let adapter = V340Adapter::new();
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

#[cfg(test)]
mod mob_effect_tests {
    use super::*;
    use lodestone_world::World;

    fn encoded_update(adapter: &V340Adapter, wire_id: i8) -> Vec<u8> {
        adapter
            .encode_body(&EntityEffect {
                entity_id: 42,
                effect_id: wire_id,
                amplifier: 0,
                duration: 40,
                flags: 0,
            })
            .expect("entity effect encodes")
    }

    fn encoded_remove(adapter: &V340Adapter, wire_id: i8) -> Vec<u8> {
        adapter
            .encode_body(&RemoveEntityEffect {
                entity_id: 42,
                effect_id: wire_id,
            })
            .expect("remove entity effect encodes")
    }

    #[test]
    fn packet_ingress_resolves_one_based_speed_and_rejects_unknown_signed_ids() {
        for &protocol in PROTOCOLS {
            let adapter = V340Adapter::for_protocol(protocol);
            let mut world = World::new();
            let applied = adapter
                .play_entity_effect(&mut world, &encoded_update(&adapter, 1))
                .expect("known legacy effect decodes");
            let [Directive::Emit(ClientEvent::MobEffectApplied { effect, .. })] = applied.as_slice()
            else {
                panic!("known effect did not emit one application event: {applied:?}");
            };
            assert_eq!(effect.path(), "speed", "protocol {protocol}");

            let removed = adapter
                .play_remove_entity_effect(&mut world, &encoded_remove(&adapter, 1))
                .expect("known legacy effect removal decodes");
            let [Directive::Emit(ClientEvent::MobEffectRemoved { effect, .. })] = removed.as_slice()
            else {
                panic!("known effect did not emit one removal event: {removed:?}");
            };
            assert_eq!(effect.path(), "speed", "protocol {protocol}");

            for wire_id in [
                i8::MIN,
                0,
                (lodestone_data::mob_effects::MOB_EFFECT_COUNT + 1) as i8,
            ] {
                let mut world = World::new();
                let error = adapter
                    .play_entity_effect(&mut world, &encoded_update(&adapter, wire_id))
                    .expect_err("unknown update effect must fail closed");
                assert!(
                    error
                        .to_string()
                        .contains(&format!("unknown legacy effect id {wire_id}")),
                    "protocol {protocol}, update id {wire_id}: {error}"
                );

                let error = adapter
                    .play_remove_entity_effect(&mut world, &encoded_remove(&adapter, wire_id))
                    .expect_err("unknown removal effect must fail closed");
                assert!(
                    error
                        .to_string()
                        .contains(&format!("unknown legacy effect id {wire_id}")),
                    "protocol {protocol}, remove id {wire_id}: {error}"
                );
            }

            assert!(
                V340Adapter::legacy_mob_effect_id(i32::MIN).is_err(),
                "checked subtraction must keep an extreme wire value from overflowing"
            );
        }
    }
}

#[cfg(test)]
mod dispatch_coverage_tests {
    use super::*;
    use lodestone_core::dispatch::{DispatchError, Table};

    /// `Table::build` succeeding is a real, falsifiable claim here: it fails
    /// loudly the moment any `play::clientbound::ENTRIES` id has neither a
    /// bound handler nor a `PLAY_CLIENTBOUND_IGNORED` reason -- the former
    /// silent `_ =>` island, reborn as a construction-time error.
    #[test]
    fn play_dispatch_table_builds_and_covers_every_entry() {
        let table = Table::build(
            PROTOCOL,
            play::clientbound::ENTRIES,
            PLAY_CLIENTBOUND_HANDLERS,
            PLAY_CLIENTBOUND_IGNORED,
        )
        .expect("real v1-9 play tables must build");

        assert_eq!(table.len(), PLAY_CLIENTBOUND_HANDLERS.len());
        assert_eq!(
            play::clientbound::ENTRIES.len(),
            PLAY_CLIENTBOUND_HANDLERS.len() + PLAY_CLIENTBOUND_IGNORED.len(),
            "every id in ENTRIES must be either handled or explicitly ignored"
        );
    }

    /// Every protocol in this era must build its own table from its own
    /// `ENTRIES`. This is the check that fails if a 1.12-only packet is left
    /// on a blanket handler/ignore range, or if a packet the earlier
    /// protocols carry gains a 340-only range by accident.
    #[test]
    fn every_era_protocol_builds_its_own_dispatch_table() {
        for &protocol in PROTOCOLS {
            let ids = ids_for(protocol);
            let table = Table::build(
                protocol,
                ids.play_clientbound_entries,
                PLAY_CLIENTBOUND_HANDLERS,
                PLAY_CLIENTBOUND_IGNORED,
            )
            .unwrap_or_else(|err| panic!("protocol {protocol} table must build: {err}"));

            // 1.12 added four clientbound packets (craft_recipe_response,
            // unlock_recipes, select_advancement_tab, advancements); the
            // first three of those are ignored and the fourth handled, so the
            // handled count rises by exactly one at 340.
            let expected_handled = if protocol == PROTOCOL_1_12_2 {
                PLAY_CLIENTBOUND_HANDLERS.len()
            } else {
                PLAY_CLIENTBOUND_HANDLERS.len() - 1
            };
            assert_eq!(
                table.len(),
                expected_handled,
                "protocol {protocol} handled-packet count"
            );
        }
    }

    /// Negative control for the id seam, and the one this era crate exists to
    /// make falsifiable: `update_health` is id **62** at 110/210/316 and id
    /// **65** at 340 (the four 1.12 clientbound insertions shifted it). An
    /// adapter constructed for 110 must therefore dispatch 62 and *not* 65,
    /// and a 340 adapter the reverse. A single shared table -- the state
    /// before this era merge -- fails this in both directions.
    #[test]
    fn update_health_dispatches_on_each_protocols_own_id() {
        const UPDATE_HEALTH_110: i32 = 62;
        const UPDATE_HEALTH_340: i32 = 65;
        assert_eq!(
            crate::packet_ids_110::play::clientbound::UPDATE_HEALTH,
            UPDATE_HEALTH_110
        );
        assert_eq!(
            crate::packet_ids::play::clientbound::UPDATE_HEALTH,
            UPDATE_HEALTH_340
        );

        // Body of a real `update_health`: health 12.5 (f32), food 7 (varint),
        // saturation 3.5 (f32). Chosen so a wrong-arm decode cannot coincide
        // with a right one -- none of the three is a round number, and the
        // varint sits between two floats.
        let mut writer = Writer::default();
        writer.f32(12.5);
        writer.var_i32(7);
        writer.f32(3.5);
        let payload = writer.into_vec();

        for (protocol, own_id, other_id) in [
            (PROTOCOL_1_9_4, UPDATE_HEALTH_110, UPDATE_HEALTH_340),
            (PROTOCOL_1_10_2, UPDATE_HEALTH_110, UPDATE_HEALTH_340),
            (PROTOCOL_1_11_2, UPDATE_HEALTH_110, UPDATE_HEALTH_340),
            (PROTOCOL_1_12_2, UPDATE_HEALTH_340, UPDATE_HEALTH_110),
        ] {
            let adapter = adapter_for(protocol);
            let mut world = lodestone_world::World::default();

            let directives = adapter
                .handle_packet(&mut world, ConnectionState::Play, own_id, &payload)
                .expect("this protocol's own update_health id must decode");
            assert_eq!(
                directives,
                vec![Directive::Emit(ClientEvent::HealthChanged {
                    health: 12.5,
                    food: 7,
                    saturation: 3.5,
                })],
                "protocol {protocol} must translate id {own_id} as update_health"
            );

            // The *other* protocol's id must not produce a health event.
            // (At 110 id 65 is `held_item_slot`; at 340 id 62 is
            // `entity_velocity` -- both decode to something else entirely,
            // which is exactly the silent corruption a shared table causes.)
            let other = adapter
                .handle_packet(&mut world, ConnectionState::Play, other_id, &payload)
                .unwrap_or_default();
            assert!(
                !other.iter().any(|directive| matches!(
                    directive,
                    Directive::Emit(ClientEvent::HealthChanged { .. })
                )),
                "protocol {protocol} must not translate id {other_id} as update_health"
            );
        }
    }

    /// Negative control: drop one real `IGNORED` entry (`minecraft:statistics`,
    /// which has no bound handler either) and `Table::build` must now refuse
    /// to build, naming the newly-unaccounted-for id -- proving the detector
    /// this stage relies on actually fires rather than the happy path above
    /// passing by construction.
    #[test]
    fn dropping_an_ignored_entry_fails_construction() {
        let mut ignored: Vec<lodestone_core::dispatch::IGNORED> =
            PLAY_CLIENTBOUND_IGNORED.to_vec();
        let removed_index = ignored
            .iter()
            .position(|entry| entry.name == "minecraft:statistics")
            .expect("minecraft:statistics is a real IGNORED entry");
        ignored.remove(removed_index);

        let err = Table::build(
            PROTOCOL,
            play::clientbound::ENTRIES,
            PLAY_CLIENTBOUND_HANDLERS,
            &ignored,
        )
        .expect_err("removing statistics's IGNORED entry must reintroduce the `_ =>` island");

        assert_eq!(
            err,
            DispatchError::UnlistedId {
                name: "minecraft:statistics",
                id: play::clientbound::STATISTICS,
            }
        );
    }
}
