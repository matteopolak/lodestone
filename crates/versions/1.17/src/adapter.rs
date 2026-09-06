//! [`VersionAdapter`] implementation driving this era's join flow, for
//! protocols 756 and 758.

use std::sync::{Arc, LockResult, Mutex, MutexGuard, PoisonError};

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_data::block_entity_types::block_entity_type;
use lodestone_data::mob_effects::{mob_effect_name_for, MobEffectId};
use lodestone_model::{
    AdapterError, AnimationAction, BlockActionKind, BlockFace, BlockStateRef, BossAction, BossColor, BossOverlay,
    ChatKind, ChatMode, ChunkPos, ClientAction, ClientEvent, ClientSettings, CollisionRule,
    ConnectionState, Difficulty, Directive, DisplaySlot, DisplayedSkinParts, EntityInteraction,
    EntityAttributeModifier, EntityAttributeSnapshot, EntityEquipment, EntityMovement, EquipmentSlot,
    GameMode, Hand, ItemComponents, ItemStack, LevelEventData, LoginProfile, MainHand, ObjectiveMode, ObjectiveRenderType,
    PlayerCommand, PlayerListEntry, ProfileProperty, RecipeBookType, ResourceKey,
    ResourcePackResponseKind, Rotation, SectionPos, ServerAddress, TeamAction, TeamColor,
    TeamParameters, TeleportFlags, Text, Vec3, VersionAdapter, Visibility, WorldSink,
};
use lodestone_world::{ChunkPos as WorldChunkPos, Heightmaps, LoadedChunk};

use crate::canonical::FallbackTally;
use crate::entity_types;
use crate::registry;
use crate::packets::chunk::{ChunkShape, MapChunk, UnloadChunk, UpdateLight};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse};
use crate::packets::entity::{
    EntityDestroy, EntityLook, EntityMoveLook, EntityTeleport, EntityVelocityPacket,
    NamedEntitySpawn, RelEntityMove, SpawnEntityExperienceOrb, SpawnEntityLiving, SpawnObject,
};
use crate::packets::game::{
    AttachEntity, BlockDig, BlockPlace, ClientCommand, ClientboundChat, ClientboundPositionLook,
    Collect, DifficultyPacket, EntityAction, EntityEffect, JoinGame, KickDisconnect,
    EntityEffectVarint, OpenSignEntity, PlayerlistHeader, RecipeBook, RemoveEntityEffect,
    RemoveEntityEffectVarint, Respawn,
    ServerboundArmAnimation, ServerboundChat, ServerboundFlying, ServerboundLook,
    ServerboundPosition, ServerboundPositionLook, SetPassengers,
    Spectate, SpawnPosition, TeleportConfirm, UpdateHealth, UpdateTime, UseEntity, UseEntityAt,
    UseEntityInteract, UseItem,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{
    EncryptionRequest, LoginDisconnect, LoginSuccess, SetCompression,
};
use crate::packets::player_info::{PlayerInfo, PlayerInfoAction};
use crate::packets::position::Position;
use crate::packets::settings::{BrandPayload, PlayerAbilities, ResourcePackReceive, Settings};
use crate::packets::slot::Slot;
use crate::packets::window::{
    CloseWindow, EnchantItem, HeldItemSlot, ServerboundCloseWindow, ServerboundHeldItemSlot,
    SetCreativeSlot,
};

/// Protocol version of the newest release this family speaks (Minecraft
/// 1.18.2), and the one a zero-argument [`adapter`] constructs.
///
/// Note the folder name is `1.17` and this protocol is **758**. Never derive
/// one from the other — ask [`PROTOCOLS`].
pub const PROTOCOL: i32 = PROTOCOL_1_18_2;

/// Protocol version of Minecraft 1.17.1 — the era's opening release.
///
/// Read off the jar's own `version.json`, which reports
/// `"protocol_version": 756`; 755 is 1.17, which is not fetched here.
pub const PROTOCOL_1_17_1: i32 = 756;
/// Protocol version of Minecraft 1.18.2 — the era's closing release.
///
/// Read off the jar's own `version.json`, which reports
/// `"protocol_version": 758`.
pub const PROTOCOL_1_18_2: i32 = 758;

/// Every protocol number this family speaks — the single source of truth for
/// its coverage.
///
/// [`VersionAdapter::supports`] tests membership here, and
/// `lodestone-registry`'s `FAMILIES` entry points at this same slice, so the
/// registry's view of a family cannot drift from the family's own.
///
/// This family is an *era* crate: one wire generation, two releases. The two
/// protocols agree on 156 of 166 packet shapes (measured from
/// `minecraft-data` with named types inlined and primitive aliases kept) and
/// disagree on fifteen clientbound play ids — 1.18 inserted
/// `simulation_distance` at id 87 and everything above it moved by one.
/// [`adapter_for`] selects that protocol's generated id table at
/// construction; nothing here may name a generated module directly.
pub const PROTOCOLS: &[i32] = &[PROTOCOL_1_17_1, PROTOCOL_1_18_2];

/// The packet ids one protocol in this era assigns to the packets this
/// adapter names.
///
/// The generated `packet_ids*` tables are one module per protocol, so a
/// `self.ids().block_dig` path can only ever mean *one* protocol's id. This
/// struct is the indirection that lets a single adapter body serve both: it
/// is resolved once, at construction, from the negotiated protocol, and every
/// id an arm sends reads through it. Nothing in this file may name a
/// generated module directly outside `packet_ids_from!`.
///
/// Measured against the two generated tables, the era's id drift is
/// **clientbound only**: 15 of the 97 shared clientbound play ids differ (the
/// fifteen at or above the id 1.18 gave `simulation_distance`), while all 47
/// shared serverbound play ids and the whole handshaking, status and login
/// sections agree. Every id is still selected through this struct rather than
/// named directly, so a future era member that does move a serverbound id
/// cannot do so silently.
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
    /// `minecraft:use_item`, serverbound play.
    use_item: i32,
    /// `minecraft:recipe_book`, serverbound play — the pane-state half of
    /// the pair 1.16 split out of the older single recipe-book packet. Both
    /// protocols here carry the split shape, so unlike the era below there
    /// is no second wire form to select between.
    recipe_book: i32,
}

/// Builds a [`PacketIds`] from one generated table module.
macro_rules! packet_ids_from {
    ($table:ident) => {
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
            use_item: crate::$table::play::serverbound::USE_ITEM,
            recipe_book: crate::$table::play::serverbound::RECIPE_BOOK,
        }
    };
}

/// Minecraft 1.17.1's ids.
static IDS_1_17_1: PacketIds = packet_ids_from!(packet_ids);
/// Minecraft 1.18.2's ids.
static IDS_1_18_2: PacketIds = packet_ids_from!(packet_ids_758);

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
        PROTOCOL_1_17_1 => &IDS_1_17_1,
        PROTOCOL_1_18_2 => &IDS_1_18_2,
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

/// Per-connection state used by this era's client-side player-position-send tick.
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

/// Version adapter implementing this era's three protocols.
///
/// Holds a [`ChunkShape`] for the paletted chunk decoder. Unlike every era
/// below, that shape is **not** constant per protocol: 1.17 moved the
/// column's vertical range into the dimension entry the `login`/`respawn`
/// packet carries, so the shape starts at that protocol's vanilla overworld
/// window and is replaced by [`V756Adapter::adopt_dimension_shape`] the
/// moment a server states its own. The [`Mutex`] is therefore load-bearing,
/// not merely a `Sync` formality.
#[derive(Debug, Clone)]
pub struct V756Adapter {
    /// The negotiated protocol this adapter speaks: one of [`PROTOCOLS`].
    protocol: i32,
    /// This protocol's id table, resolved once at construction by
    /// [`ids_for`].
    ids: &'static PacketIds,
    shape: Arc<Mutex<ChunkShape>>,
    /// Namespaced world name (e.g. `minecraft:overworld`) from the most
    /// recent `login`/`respawn`, so a packet that identifies its dimension
    /// only implicitly (`spawn_position` carries no dimension field at all)
    /// can still report one.
    current_dimension: Arc<Mutex<String>>,
    movement: Arc<Mutex<MovementSendState>>,
}

impl Default for V756Adapter {
    fn default() -> Self {
        Self::new()
    }
}

impl V756Adapter {
    /// Creates a new adapter speaking [`PROTOCOL`] (the era's newest release).
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
            current_dimension: Arc::new(Mutex::new("minecraft:overworld".to_owned())),
            movement: Arc::new(Mutex::new(MovementSendState::default())),
        }
    }

    /// The codec context for the protocol this adapter was constructed for.
    ///
    /// Every `#[mc(since = N)]`/`#[mc(until = N)]` predicate and every
    /// `#[mc(protocols = "a..=b")]` precondition in this family reads
    /// `ctx.version`, so this is the single point at which "which protocol am
    /// I speaking" reaches the codecs. It is a per-instance value, not a
    /// constant, because one adapter type now serves three protocols.
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

    /// Selects this era's movement shape. This is deliberately local to the
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

    /// Adopts the vertical window the server's own dimension entry declares.
    ///
    /// `dimension` is the raw named-NBT blob `login` and `respawn` both
    /// carry. A blob that does not state a usable `min_y`/`height` pair
    /// leaves the shape alone — see
    /// [`ChunkShape::from_dimension_nbt`](crate::packets::chunk::ChunkShape::from_dimension_nbt)
    /// for why an unreadable height must never be replaced with a guess.
    fn adopt_dimension_shape(&self, dimension: &[u8]) {
        if let Ok(mut shape) = self.shape.lock()
            && let Some(next) = shape.from_dimension_nbt(dimension)
        {
            *shape = next;
        }
    }

    /// Returns the current dimension's chunk shape.
    fn current_shape(&self) -> ChunkShape {
        self.shape
            .lock()
            .map_or_else(|_| ChunkShape::overworld(self.protocol), |shape| *shape)
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

/// Returns a version adapter speaking [`PROTOCOL`] (the era's newest release).
///
/// This free function is the crate's canonical constructor entry point; the
/// client boxes the returned concrete type as a `dyn VersionAdapter`.
#[must_use]
pub fn adapter() -> V756Adapter {
    V756Adapter::new()
}

/// Returns an adapter configured for the **negotiated** protocol.
///
/// The multi-protocol construction seam (unit U2). Before it, every
/// family was built by a zero-argument `make: fn() -> Box<dyn VersionAdapter>`
/// and the negotiated number reached the adapter nowhere — which is precisely
/// what stopped one crate serving several protocol revisions, since it had
/// nothing to select a per-protocol `packet_ids` table by.
/// This family is an era crate, so the argument selects that protocol's
/// generated id table, block-state table, entity-type registry and chunk
/// shape, all resolved once here.
///
/// # Panics
///
/// Debug builds assert `protocol` is in [`PROTOCOLS`]. The registry always
/// checks membership before constructing, so reaching this with anything else
/// means a caller bypassed that check.
#[must_use]
pub fn adapter_for(protocol: i32) -> V756Adapter {
    debug_assert!(
        PROTOCOLS.contains(&protocol),
        "adapter_for({protocol}) is outside this family's PROTOCOLS ({PROTOCOLS:?}); \
         callers must test membership before constructing"
    );
    V756Adapter::for_protocol(protocol)
}

/// Maps the model's `RecipeBookType` onto the ordinal this era's `recipe_book`
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

/// Resolves the textual attribute spelling used by protocols 756 and 758 to
/// the shared attribute registry. These wire names retain the `generic.` path
/// segment; the model's canonical keys do not.
fn attribute_key(wire_key: &str) -> Option<ResourceKey> {
    let canonical = match wire_key {
        "minecraft:generic.armor" => "minecraft:armor",
        "minecraft:generic.armor_toughness" => "minecraft:armor_toughness",
        "minecraft:generic.attack_damage" => "minecraft:attack_damage",
        "minecraft:generic.attack_knockback" => "minecraft:attack_knockback",
        "minecraft:generic.attack_speed" => "minecraft:attack_speed",
        "minecraft:generic.flying_speed" => "minecraft:flying_speed",
        "minecraft:generic.follow_range" => "minecraft:follow_range",
        "minecraft:generic.knockback_resistance" => "minecraft:knockback_resistance",
        "minecraft:generic.luck" => "minecraft:luck",
        "minecraft:generic.max_health" => "minecraft:max_health",
        "minecraft:generic.movement_speed" => "minecraft:movement_speed",
        "minecraft:horse.jump_strength" => "minecraft:jump_strength",
        "minecraft:zombie.spawn_reinforcements" => "minecraft:spawn_reinforcements",
        _ => return None,
    };
    canonical.parse().ok()
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
/// so no numeric table is involved. Every protocol in this era is on the
/// string side of that seam.
fn dimension_id(name: &str) -> Result<lodestone_model::DimensionId, AdapterError> {
    name.parse()
        .map_err(|_| AdapterError::Decode(format!("invalid dimension identifier {name}")))
}

/// Decodes a packet body that is exactly one JSON text component and nothing
/// else — the shape all five of 1.17's split title packets and their action-bar
/// sibling share.
fn decode_single_json_text(payload: &[u8]) -> Result<Text, AdapterError> {
    let mut reader = Reader::new(payload);
    let json = reader.string(32_767).map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(Text::from_json(&json))
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
/// the same formula and is used identically by v1-8 and v1-9.
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
/// `lodestone-v1-9`'s own convention for multiplexed/action-tagged packets
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

impl V756Adapter {
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
            // Validate the profile decodes, then advance. Both protocols
            // here send the UUID as sixteen raw bytes, the form 1.16
            // introduced; the dashed-string form the eras below carry is not
            // reachable from any protocol in this crate.
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
}

/// Fn-pointer payload every `play` clientbound handler below shares: given
/// the adapter, a mutable world sink, and the raw packet payload, produce the
/// directives to run. A plain `fn` pointer (rather than a closure or boxed
/// trait object) because every extracted handler closes only over its three
/// parameters -- nothing needs to capture additional state -- so
/// `lodestone_core::dispatch::Handler<T>`'s `Copy` bound is satisfied by
/// construction, and building the dispatch table costs nothing per call.
type PlayHandler =
    fn(&V756Adapter, &mut dyn WorldSink, &[u8]) -> Result<Vec<Directive>, AdapterError>;

impl V756Adapter {
    /// `minecraft:login`. Carries the dimension entry this era reads its
    /// vertical range out of — see [`V756Adapter::adopt_dimension_shape`].
    fn handle_play_login(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: JoinGame = adapter.decode_body(payload)?;
        adapter.adopt_dimension_shape(&body.dimension);
        adapter.set_dimension(&body.world_name);
        Ok(vec![Directive::Emit(ClientEvent::Login {
            entity_id: body.entity_id,
            game_mode: game_mode(body.game_mode)?,
            dimension: dimension_id(&body.world_name)?,
        })])
    }

    /// `minecraft:map_chunk`. Decodes the paletted column into version-free
    /// storage and applies it to the world through the sink, emitting only a
    /// lightweight notification.
    ///
    /// The column shape comes from the adapter's own state, which the most
    /// recent `login`/`respawn` set from the server's dimension entry — the
    /// era's one genuinely stateful decode. At 756 the column arrives with no
    /// light and waits for `update_light`; at 758 the light is in this packet.
    fn handle_play_map_chunk(
        adapter: &V756Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let shape = adapter.current_shape();
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
        Ok(vec![Directive::Emit(ClientEvent::ChunkLoaded { pos })])
    }

    /// `minecraft:update_light`. The light-only update, which exists in both
    /// protocols — at 758 a full column's light additionally rides along in
    /// `map_chunk`, but this packet is still what a relight sends.
    ///
    /// Decodes the per-section nibble arrays into a version-free `LightPatch`
    /// and merges it onto the already-loaded column; a light update for an
    /// unloaded column is a harmless no-op in the world store.
    fn handle_play_update_light(
        adapter: &V756Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let update = UpdateLight::decode(&mut reader, &adapter.current_shape())
            .map_err(|err| AdapterError::Decode(err.to_string()))?;
        reader
            .ensure_empty()
            .map_err(|err| AdapterError::Decode(err.to_string()))?;
        world.merge_light(WorldChunkPos::new(update.x, update.z), update.patch);
        Ok(Vec::new())
    }

    /// `minecraft:unload_chunk`. This era has a dedicated forget packet (two
    /// ints).
    fn handle_play_unload_chunk(
        adapter: &V756Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: UnloadChunk = adapter.decode_body(payload)?;
        let pos = ChunkPos::new(body.chunk_x, body.chunk_z);
        world.unload(WorldChunkPos::new(body.chunk_x, body.chunk_z));
        Ok(vec![Directive::Emit(ClientEvent::ChunkUnloaded { pos })])
    }

    /// `minecraft:keep_alive`.
    fn handle_play_keep_alive(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let keep_alive: KeepAliveRequest = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::KeepAlive {
            id: keep_alive.id,
        })])
    }

    /// `minecraft:chat`.
    fn handle_play_chat(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: ClientboundChat = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::Chat {
            text: Text::from_json(&body.message),
            kind: chat_kind(body.position),
            // 1.16's chat packet carries no sender field — nothing to filter on.
            sender: None,
            ack: None,
        })])
    }

    /// `minecraft:position`.
    fn handle_play_position(
        adapter: &V756Adapter,
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
        // 1.9+ requires echoing the teleport id back or the server
        // rubber-bands the player. This confirm choreography lives entirely
        // in the version crate; the driver just runs the directives in order.
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

    /// `minecraft:spawn_entity_living`.
    fn handle_play_spawn_entity_living(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnEntityLiving = adapter.decode_body(payload)?;
        let entity_type = entity_types::table_for(adapter.protocol)
            .mob_type_name(body.kind)
            .ok_or_else(|| {
                AdapterError::Decode(format!("unknown mob type id {} in spawn", body.kind))
            })?
            .parse()
            .map_err(|_| AdapterError::Decode(format!("mob type id {} is not a key", body.kind)))?;
        Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
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
        })])
    }

    /// `minecraft:spawn_entity`.
    fn handle_play_spawn_entity(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnObject = adapter.decode_body(payload)?;
        let type_id = body.kind;
        let entity_type = entity_types::table_for(adapter.protocol)
            .object_type_name(type_id)
            .ok_or_else(|| AdapterError::Decode(format!("unknown object type id {type_id} in spawn")))?
            .parse()
            .map_err(|_| AdapterError::Decode(format!("object type id {type_id} is not a key")))?;
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
        Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
            entity_id: body.entity_id,
            uuid: Some(body.object_uuid),
            entity_type,
            pos: Vec3::new(body.x, body.y, body.z),
            rotation: Rotation::new(unpack_degrees(body.yaw), unpack_degrees(body.pitch)),
            velocity,
        })])
    }

    /// `minecraft:named_entity_spawn`.
    fn handle_play_named_entity_spawn(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: NamedEntitySpawn = adapter.decode_body(payload)?;
        let entity_type = entity_types::PLAYER
            .parse()
            .map_err(|_| AdapterError::Decode("player key invalid".to_owned()))?;
        Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
            entity_id: body.entity_id,
            uuid: Some(body.player_uuid),
            entity_type,
            pos: Vec3::new(body.x, body.y, body.z),
            rotation: Rotation::new(unpack_degrees(body.yaw), unpack_degrees(body.pitch)),
            velocity: None,
        })])
    }

    /// `minecraft:rel_entity_move`.
    fn handle_play_rel_entity_move(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: RelEntityMove = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
            entity_id: body.entity_id,
            movement: EntityMovement::Relative(Vec3::new(
                f64::from(body.dx) / MOVE_DELTA_SCALE,
                f64::from(body.dy) / MOVE_DELTA_SCALE,
                f64::from(body.dz) / MOVE_DELTA_SCALE,
            )),
            rotation: None,
            on_ground: body.on_ground,
        })])
    }

    /// `minecraft:entity_look`.
    fn handle_play_entity_look(
        adapter: &V756Adapter,
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

    /// `minecraft:entity_move_look`.
    fn handle_play_entity_move_look(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityMoveLook = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
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
        })])
    }

    /// `minecraft:entity_teleport`. 1.9+ sends the absolute position
    /// directly as `f64`; no fixed-point conversion, unlike 1.8.
    fn handle_play_entity_teleport(
        adapter: &V756Adapter,
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
        adapter: &V756Adapter,
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

    /// `minecraft:entity_destroy`. A varint-counted list of varint ids,
    /// via the derived `#[mc(varint)]`-on-`Vec<i32>` struct.
    fn handle_play_entity_destroy(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: EntityDestroy = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityRemoved {
            entity_ids: body.entity_ids,
        })])
    }

    /// `minecraft:kick_disconnect`.
    fn handle_play_kick_disconnect(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: KickDisconnect = adapter.decode_body(payload)?;
        Ok(vec![Directive::Disconnect(json_reason_text(&body.reason))])
    }

    /// `minecraft:update_health`.
    fn handle_play_update_health(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: UpdateHealth = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::HealthChanged {
            health: body.health,
            food: body.food,
            saturation: body.food_saturation,
        })])
    }

    /// `minecraft:respawn`. Like `login`, 1.16 replaced the numeric
    /// dimension with a namespaced `world_name` string plus an inline raw
    /// named-NBT dimension type.
    /// Carries the same dimension entry `login` does, so a respawn into a
    /// dimension of a different height re-resolves the column shape.
    fn handle_play_respawn(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: Respawn = adapter.decode_body(payload)?;
        adapter.adopt_dimension_shape(&body.dimension);
        adapter.set_dimension(&body.world_name);
        Ok(vec![Directive::Emit(ClientEvent::Respawned {
            dimension: dimension_id(&body.world_name)?,
            game_mode: game_mode(body.game_mode)?,
            previous_game_mode: None,
            last_death_location: None,
        })])
    }

    /// `minecraft:spawn_position`. This protocol revision carries no angle
    /// or dimension field (both are later additions), so `angle`/`pitch`
    /// are `0.0` and `dimension` comes from the adapter's own
    /// `current_dimension` (set by the most recent `login`/`respawn`).
    fn handle_play_spawn_position(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SpawnPosition = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
            dimension: dimension_id(&adapter.current_dimension())?,
            pos: body.location.0,
            angle: 0.0,
            pitch: 0.0,
        })])
    }

    /// `minecraft:entity_status`. A raw (non-VarInt) `i32` entity id, then a
    /// raw status byte.
    fn handle_play_entity_status(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let entity_id = reader.i32().map_err(dec_err)?;
        let status = reader.u8().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityStatus {
            entity_id,
            status,
        })])
    }

    /// `minecraft:entity_head_rotation`. VarInt entity id, then a packed
    /// signed-byte yaw.
    fn handle_play_entity_head_rotation(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let entity_id = reader.var_i32().map_err(dec_err)?;
        let packed = reader.i8().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityHeadRotation {
            entity_id,
            head_yaw: unpack_degrees(packed),
        })])
    }

    /// `minecraft:animation`. Unlike 1.12.2 (which has no dedicated hurt
    /// animation, so id `2` there means "leave bed"), 1.9+ folds "leave bed"
    /// out and adds a dedicated critical/magic-critical pair;
    /// `AnimationAction`'s `Other` fallback carries anything this table
    /// does not name.
    fn handle_play_animation(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
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
        Ok(vec![Directive::Emit(ClientEvent::EntityAnimation {
            entity_id,
            action,
        })])
    }

    /// `minecraft:abilities` (clientbound direction). This era reuses one
    /// packet *name* for both directions with different flag semantics; the
    /// clientbound shape decoded here is byte-identical to 1.12.2's/1.8's,
    /// so it is hand-decoded rather than routed through the
    /// serverbound-tagged `PlayerAbilities` struct to avoid conflating the
    /// two directions' meaning.
    fn handle_play_abilities(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let flags = reader.i8().map_err(dec_err)?;
        let flying_speed = reader.f32().map_err(dec_err)?;
        let walking_speed = reader.f32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::AbilitiesChanged {
            invulnerable: flags & ABILITY_INVULNERABLE != 0,
            flying: flags & ABILITY_FLYING != 0,
            can_fly: flags & ABILITY_CAN_FLY != 0,
            instabuild: flags & ABILITY_INSTABUILD != 0,
            flying_speed,
            walking_speed,
        })])
    }

    /// `minecraft:difficulty`.
    fn handle_play_difficulty(
        adapter: &V756Adapter,
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
                return Err(AdapterError::Decode(format!("unknown difficulty id {other}")));
            }
        };
        Ok(vec![Directive::Emit(ClientEvent::DifficultyChanged {
            difficulty,
            locked: body.difficulty_locked,
        })])
    }

    /// `minecraft:update_time`.
    fn handle_play_update_time(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: UpdateTime = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TimeChanged {
            world_age: body.age,
            time_of_day: body.time,
        })])
    }

    /// `minecraft:playerlist_header`.
    fn handle_play_playerlist_header(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: PlayerlistHeader = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TabListChanged {
            header: Text::from_json(&body.header),
            footer: Text::from_json(&body.footer),
        })])
    }

    /// `minecraft:attach_entity`.
    fn handle_play_attach_entity(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: AttachEntity = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityLeashed {
            entity_id: body.entity_id,
            holder_id: (body.vehicle_id != 0).then_some(body.vehicle_id),
        })])
    }

    /// `minecraft:set_passengers`.
    fn handle_play_set_passengers(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: SetPassengers = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(
            ClientEvent::EntityPassengersChanged {
                vehicle_id: body.entity_id,
                passenger_ids: body.passengers,
            },
        )])
    }

    /// `minecraft:collect`.
    fn handle_play_collect(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: Collect = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ItemPickup {
            item_entity_id: body.collected_entity_id,
            player_id: body.collector_entity_id,
            amount: body.pickup_item_count,
        })])
    }

    /// Converts this era's legacy (1-based) wire effect id into the shared
    /// `lodestone-data` 0-based `minecraft:mob_effect` id. The conversion is
    /// the boundary between packet data and the validated registry table.
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

    /// Resolves a validated legacy effect id into the canonical event key.
    fn legacy_mob_effect_key(effect_id: MobEffectId) -> Result<ResourceKey, AdapterError> {
        let name = mob_effect_name_for(effect_id);
        name.parse()
            .map_err(|_| AdapterError::Decode(format!("effect id {name} is not a key")))
    }

    /// `minecraft:entity_effect`.
    fn handle_play_entity_effect(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        // 1.18 retyped the effect id from a signed byte to a VarInt, so the
        // two protocols decode through different structs -- see
        // [`EntityEffectVarint`]'s own docs for what reading the narrower
        // form off the wider one would do instead of erroring.
        let (entity_id, wire_effect_id, amplifier, duration, flags) =
            if adapter.protocol >= PROTOCOL_1_18_2 {
                let body: EntityEffectVarint = adapter.decode_body(payload)?;
                (
                    body.entity_id,
                    body.effect_id,
                    body.amplifier,
                    body.duration,
                    body.flags,
                )
            } else {
                let body: EntityEffect = adapter.decode_body(payload)?;
                (
                    body.entity_id,
                    i32::from(body.effect_id),
                    body.amplifier,
                    body.duration,
                    body.flags,
                )
            };
        let effect_id = Self::legacy_mob_effect_id(wire_effect_id)?;
        let effect = Self::legacy_mob_effect_key(effect_id)?;
        Ok(vec![Directive::Emit(ClientEvent::MobEffectApplied {
            entity_id,
            effect,
            amplifier: i32::from(amplifier),
            duration_ticks: duration,
            ambient: flags & 0x01 != 0,
            visible: flags & 0x02 != 0,
            // The "show icon" bit is real from 1.13 on; "blend" is a 1.19+
            // addition this era predates.
            show_icon: flags & 0x04 != 0,
            blend: false,
        })])
    }

    /// `minecraft:remove_entity_effect`.
    fn handle_play_remove_entity_effect(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        // Same 1.18 retype as `entity_effect`, on the same field.
        let (entity_id, wire_effect_id) = if adapter.protocol >= PROTOCOL_1_18_2 {
            let body: RemoveEntityEffectVarint = adapter.decode_body(payload)?;
            (body.entity_id, body.effect_id)
        } else {
            let body: RemoveEntityEffect = adapter.decode_body(payload)?;
            (body.entity_id, i32::from(body.effect_id))
        };
        let effect_id = Self::legacy_mob_effect_id(wire_effect_id)?;
        let effect = Self::legacy_mob_effect_key(effect_id)?;
        Ok(vec![Directive::Emit(ClientEvent::MobEffectRemoved {
            entity_id,
            effect,
        })])
    }

    /// `minecraft:spawn_entity_experience_orb`.
    fn handle_play_spawn_entity_experience_orb(
        adapter: &V756Adapter,
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

    /// `minecraft:block_change`. A packed 1.14+ `position` (x/z/y bit
    /// order, unlike the pre-1.14 x/y/z order), then a varint **flat
    /// block-state id**. This era is post-Flattening, so unlike
    /// `lodestone-v1-9`'s legacy `(id << 4) | meta` composite there is no
    /// metadata split: the wire value is already a single state id in
    /// *this protocol's own* id space, bridged to a real 26.2 state id via
    /// this protocol's own `CanonicalTable` — the same table
    /// `packets/chunk.rs` uses for paletted chunk sections.
    fn handle_play_block_change(
        adapter: &V756Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let pos: Position = Position::decode(&mut reader, adapter.ctx()).map_err(dec_err)?;
        let raw = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        let raw = u32::try_from(raw)
            .map_err(|_| AdapterError::Decode(format!("block_change state id {raw} is negative")))?;
        let mut tally = FallbackTally::default();
        let state = adapter.current_shape().canonical.resolve_or_air(raw, &mut tally);
        let pos = pos.0;
        world.set_block(pos.x, pos.y, pos.z, state.raw());
        // Writing a state is what creates/removes a block entity in vanilla
        // (done inside the chunk's own block-state setter, no packet
        // involved).
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

    /// `minecraft:block_action`: a position, two block-specific bytes, and a
    /// protocol-local block registry id. The event consumer interprets the
    /// bytes from the canonical block key (chest lid, note pitch, piston move,
    /// and so on), so this handler preserves both bytes unchanged.
    fn handle_play_block_action(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let pos: Position = Position::decode(&mut reader, adapter.ctx()).map_err(dec_err)?;
        let b0 = reader.u8().map_err(dec_err)?;
        let b1 = reader.u8().map_err(dec_err)?;
        let id = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        let block = registry::block(adapter.protocol, id).ok_or_else(|| {
            AdapterError::Decode(format!("unknown protocol-{} block registry id {id}", adapter.protocol))
        })?;
        Ok(vec![Directive::Emit(ClientEvent::BlockEvent { pos: pos.0, b0, b1, block })])
    }

    /// `minecraft:entity_equipment`: an entity id followed by a
    /// continuation-flagged sequence of `(slot, slot)` records. The old slot
    /// payload has optional NBT rather than item components; a non-empty tag
    /// cannot be represented by `ItemComponents`, so reject it explicitly.
    fn handle_play_entity_equipment(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let entity_id = reader.var_i32().map_err(dec_err)?;
        let mut equipment = Vec::new();
        loop {
            if equipment.len() == EquipmentSlot::ALL.len() {
                return Err(AdapterError::Decode("too many entity equipment entries".to_owned()));
            }
            let encoded_slot = reader.u8().map_err(dec_err)?;
            let slot = EquipmentSlot::from_ordinal(encoded_slot & 0x7f).ok_or_else(|| {
                AdapterError::Decode(format!("unknown equipment slot ordinal {}", encoded_slot & 0x7f))
            })?;
            let item = match Slot::decode(&mut reader, adapter.ctx()).map_err(dec_err)? {
                Slot::Empty => None,
                Slot::Item { id, nbt: Some(_), .. } => {
                    return Err(AdapterError::Unsupported(format!(
                        "protocol-{} entity equipment item {id} carries legacy NBT, which the canonical item model cannot represent",
                        adapter.protocol
                    )));
                }
                Slot::Item { id, count, nbt: None } => {
                    let count = u32::try_from(count).map_err(|_| {
                        AdapterError::Decode(format!("negative entity equipment item count {count}"))
                    })?;
                    if count == 0 {
                        return Err(AdapterError::Decode("zero entity equipment item count".to_owned()));
                    }
                    let item = registry::item(adapter.protocol, id).ok_or_else(|| {
                        AdapterError::Decode(format!("unknown protocol-{} item registry id {id}", adapter.protocol))
                    })?;
                    Some(ItemStack { item, count, components: ItemComponents::default() })
                }
            };
            equipment.push(EntityEquipment { slot, item });
            if encoded_slot & 0x80 == 0 {
                break;
            }
        }
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityEquipmentUpdated { entity_id, equipment })])
    }

    /// `minecraft:block_break_animation`.
    fn handle_play_block_break_animation(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let entity_id = reader.var_i32().map_err(dec_err)?;
        let location = Position::decode(&mut reader, Ctx { version: PROTOCOL }).map_err(dec_err)?;
        let progress = reader.i8().map_err(dec_err)? as u8;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::BlockDestruction {
            entity_id,
            pos: location.0,
            progress,
        })])
    }

    /// `minecraft:explosion`, including the VarInt-counted block-offset list.
    fn handle_play_explosion(
        adapter: &V756Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let pos = Vec3::new(
            f64::from(reader.f32().map_err(dec_err)?),
            f64::from(reader.f32().map_err(dec_err)?),
            f64::from(reader.f32().map_err(dec_err)?),
        );
        let radius = reader.f32().map_err(dec_err)?;
        let raw_count = reader.var_i32().map_err(dec_err)?;
        let count = usize::try_from(raw_count).map_err(|_| AdapterError::Decode(format!("negative explosion block count {raw_count}")))?;
        const MAX_EXPLOSION_BLOCKS: usize = 65_536;
        if count > MAX_EXPLOSION_BLOCKS || count > reader.remaining() / 3 {
            return Err(AdapterError::Decode(format!("explosion block count {count} exceeds bounded payload")));
        }
        let mut affected_blocks = Vec::with_capacity(count);
        for _ in 0..count {
            affected_blocks.push([reader.i8().map_err(dec_err)?, reader.i8().map_err(dec_err)?, reader.i8().map_err(dec_err)?]);
        }
        let knockback = Vec3::new(
            f64::from(reader.f32().map_err(dec_err)?),
            f64::from(reader.f32().map_err(dec_err)?),
            f64::from(reader.f32().map_err(dec_err)?),
        );
        reader.ensure_empty().map_err(dec_err)?;
        let base_x = pos.x.floor() as i32;
        let base_y = pos.y.floor() as i32;
        let base_z = pos.z.floor() as i32;
        let air = adapter.current_shape().canonical.air_state_id().raw();
        for [dx, dy, dz] in &affected_blocks {
            let x = base_x
                .checked_add(i32::from(*dx))
                .ok_or_else(|| AdapterError::Decode("explosion x offset overflow".to_owned()))?;
            let y = base_y
                .checked_add(i32::from(*dy))
                .ok_or_else(|| AdapterError::Decode("explosion y offset overflow".to_owned()))?;
            let z = base_z
                .checked_add(i32::from(*dz))
                .ok_or_else(|| AdapterError::Decode("explosion z offset overflow".to_owned()))?;
            world.set_block(x, y, z, air);
            world.sync_block_entity(x, y, z, None);
        }
        Ok(vec![Directive::Emit(ClientEvent::Explosion { pos, radius, affected_blocks, knockback: Some(knockback) })])
    }

    /// `minecraft:game_state_change` world-state codes used by this adapter.
    fn handle_play_game_state_change(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let reason = reader.u8().map_err(dec_err)?;
        let value = reader.f32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        let event = match reason {
            1 => Some(ClientEvent::WeatherChanged { raining: Some(true), rain_level: None, thunder_level: None }),
            2 => Some(ClientEvent::WeatherChanged { raining: Some(false), rain_level: None, thunder_level: None }),
            3 if value.is_finite() && value.fract() == 0.0 && (0.0..=3.0).contains(&value) => {
                Some(ClientEvent::GameModeChanged { game_mode: game_mode(value as u8)? })
            }
            3 => return Err(AdapterError::Decode(format!("invalid game mode parameter {value}"))),
            7 => Some(ClientEvent::WeatherChanged { raining: None, rain_level: Some(value), thunder_level: None }),
            8 => Some(ClientEvent::WeatherChanged { raining: None, rain_level: None, thunder_level: Some(value) }),
            _ => None,
        };
        Ok(event.map_or_else(Vec::new, |event| vec![Directive::Emit(event)]))
    }

    /// `minecraft:world_event`, preserving event 2001's protocol-local state id.
    fn handle_play_world_event(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let event = reader.i32().map_err(dec_err)?;
        let location = Position::decode(&mut reader, Ctx { version: PROTOCOL }).map_err(dec_err)?;
        let raw_data = reader.i32().map_err(dec_err)?;
        let global = reader.bool().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        let data = if event == 2001 { LevelEventData::BlockState(BlockStateRef::protocol_local(raw_data as u32)) } else { LevelEventData::Raw(raw_data) };
        Ok(vec![Directive::Emit(ClientEvent::LevelEvent { event, pos: location.0, data, global })])
    }

    /// `minecraft:multi_block_change`. The packed header's section Y is
    /// unsigned at 756 and signed at 758; records use x<<8 | z<<4 | y.
    fn handle_play_multi_block_change(
        adapter: &V756Adapter,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let packed = reader.i64().map_err(dec_err)? as u64;
        let sign_extend = |value: u64, bits: u32| -> i32 {
            let shift = 64 - bits;
            ((value << shift) as i64 >> shift) as i32
        };
        let chunk_x = sign_extend(packed >> 42, 22);
        let chunk_z = sign_extend((packed >> 20) & ((1 << 22) - 1), 22);
        let raw_section_y = packed & ((1 << 20) - 1);
        let section_y = if adapter.protocol == PROTOCOL_1_18_2 { sign_extend(raw_section_y, 20) } else { raw_section_y as i32 };
        let _not_trust_edges = reader.bool().map_err(dec_err)?;
        let raw_count = reader.var_i32().map_err(dec_err)?;
        let count = usize::try_from(raw_count).map_err(|_| AdapterError::Decode(format!("negative multi-block count {raw_count}")))?;
        const MAX_MULTI_BLOCKS: usize = 4096;
        if count > MAX_MULTI_BLOCKS {
            return Err(AdapterError::Decode(format!("multi-block count {count} exceeds {MAX_MULTI_BLOCKS}")));
        }
        let mut blocks = Vec::with_capacity(count);
        let mut changed = Vec::with_capacity(count);
        let mut tally = FallbackTally::default();
        for _ in 0..count {
            let record = reader.var_i64().map_err(dec_err)? as u64;
            let local = (record & 0xFFF) as u16;
            let wire_state = (record >> 12) as u32;
            let x = ((local >> 8) & 0xF) as u8;
            let z = ((local >> 4) & 0xF) as u8;
            let y = (local & 0xF) as u8;
            let state = adapter.current_shape().canonical.resolve_or_air(wire_state, &mut tally);
            blocks.push((x, y, z, state.raw()));
            changed.push([x, y, z]);
        }
        reader.ensure_empty().map_err(dec_err)?;
        world.set_blocks(chunk_x, section_y, chunk_z, &blocks);
        for &(x, y, z, state) in &blocks {
            world.sync_block_entity(
                (chunk_x << 4) | i32::from(x),
                (section_y << 4) | i32::from(y),
                (chunk_z << 4) | i32::from(z),
                lodestone_data::block_states::StateId::new(state)
                    .and_then(block_entity_type)
                    .map(|kind| kind.raw()),
            );
        }
        if changed.is_empty() { return Ok(Vec::new()); }
        Ok(vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
            section: SectionPos::new(chunk_x, section_y, chunk_z),
            blocks: changed,
        })])
    }

    /// `minecraft:entity_update_attributes` uses textual attribute names.
    /// Modifier UUIDs are retained as stable namespaced identifiers.
    fn handle_play_entity_update_attributes(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let entity_id = reader.var_i32().map_err(dec_err)?;
        let raw_count = reader.var_i32().map_err(dec_err)?;
        let count = usize::try_from(raw_count).map_err(|_| AdapterError::Decode(format!("negative attribute count {raw_count}")))?;
        const MAX_ATTRIBUTES: usize = 128;
        const MAX_MODIFIERS: usize = 1024;
        if count > MAX_ATTRIBUTES { return Err(AdapterError::Decode(format!("attribute count {count} exceeds {MAX_ATTRIBUTES}"))); }
        let mut attributes = Vec::with_capacity(count);
        for _ in 0..count {
            let name = reader.string(32_767).map_err(dec_err)?;
            let base = reader.f64().map_err(dec_err)?;
            let raw_modifier_count = reader.var_i32().map_err(dec_err)?;
            let modifier_count = usize::try_from(raw_modifier_count).map_err(|_| AdapterError::Decode(format!("negative attribute modifier count {raw_modifier_count}")))?;
            if modifier_count > MAX_MODIFIERS { return Err(AdapterError::Decode(format!("attribute modifier count {modifier_count} exceeds {MAX_MODIFIERS}"))); }
            let mut modifiers = Vec::with_capacity(modifier_count);
            for _ in 0..modifier_count {
                let uuid = reader.uuid().map_err(dec_err)?;
                let amount = reader.f64().map_err(dec_err)?;
                let operation = reader.i8().map_err(dec_err)?;
                if !(0..=2).contains(&operation) { return Err(AdapterError::Decode(format!("invalid attribute operation {operation}"))); }
                let id = format!("minecraft:uuid/{uuid}").parse().map_err(|_| AdapterError::Decode("invalid modifier UUID".to_owned()))?;
                modifiers.push(EntityAttributeModifier { id, amount, operation: operation as u8 });
            }
            if let Some(attribute) = attribute_key(&name) {
                attributes.push(EntityAttributeSnapshot { attribute, base, modifiers });
            }
        }
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::EntityAttributesUpdated { entity_id, attributes })])
    }

    /// `minecraft:experience`.
    fn handle_play_experience(
        _adapter: &V756Adapter,
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
        _adapter: &V756Adapter,
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

    /// `minecraft:select_advancement_tab`.
    fn handle_play_select_advancement_tab(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
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
        Ok(vec![Directive::Emit(ClientEvent::AdvancementsTabSelected {
            tab,
        })])
    }

    /// `minecraft:open_sign_entity`. This era predates the front/back sign
    /// text split (added 1.20); every editable sign has only the one
    /// (front) text at this protocol revision.
    fn handle_play_open_sign_entity(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: OpenSignEntity = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::SignEditorOpened {
            pos: body.location.0,
            is_front_text: true,
        })])
    }

    /// `minecraft:camera`.
    fn handle_play_camera(
        _adapter: &V756Adapter,
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
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let x = reader.var_i32().map_err(dec_err)?;
        let z = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::ChunkCacheCenterChanged {
            x,
            z,
        })])
    }

    /// `minecraft:update_view_distance`.
    fn handle_play_update_view_distance(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let radius = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::ChunkCacheRadiusChanged {
            radius,
        })])
    }

    /// `minecraft:held_item_slot`.
    fn handle_play_held_item_slot(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: HeldItemSlot = adapter.decode_body(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::HeldSlotChanged {
            slot: i32::from(body.slot),
        })])
    }

    /// `minecraft:close_window`.
    fn handle_play_close_window(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: CloseWindow = adapter.decode_body_exact(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::ScreenClosed {
            window_id: i32::from(body.window_id),
        })])
    }

    /// `minecraft:craft_progress_bar`. No synchronization state id, so it
    /// maps directly onto the same `ContainerData` 26.2's
    /// `minecraft:container_set_data` produces.
    fn handle_play_craft_progress_bar(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let window_id = i32::from(reader.u8().map_err(dec_err)?);
        let property = i32::from(reader.i16().map_err(dec_err)?);
        let value = i32::from(reader.i16().map_err(dec_err)?);
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::ContainerData {
            window_id,
            property,
            value,
        })])
    }

    /// `minecraft:title`. Action-multiplexed: the `text` switch has three
    /// cases (`0`/`1`/`2` — title/subtitle/action-bar), the fade-in/stay/
    /// fade-out case (times) is `3`, and the two argument-less actions are
    /// `4`/`5`. Action-bar text always renders as an overlay, so it maps to
    /// the same `Chat` `GameInfo` event the dedicated `SET_ACTION_BAR_TEXT`
    /// packet uses on 26.2 — this era predates that split packet, it rides
    /// this one instead. `4`/`5` are clear-then-reset, the same pair 26.2's
    /// `CLEAR_TITLES` folds into one `resetTimes` bool.
    /// `minecraft:set_title_text`. 1.17 split the older single title packet —
    /// a VarInt action selector followed by that action's body — into six
    /// packets with no selector. This is action `0`.
    fn handle_play_set_title_text(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let text = decode_single_json_text(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::TitleText { text })])
    }

    /// `minecraft:set_title_subtitle`, the split packet for the older
    /// action `1`.
    fn handle_play_set_title_subtitle(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let text = decode_single_json_text(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::SubtitleText { text })])
    }

    /// `minecraft:action_bar`, the split packet for the older action `2`.
    /// Reported as a game-info chat line, exactly as the merged form was.
    fn handle_play_action_bar(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let text = decode_single_json_text(payload)?;
        Ok(vec![Directive::Emit(ClientEvent::Chat {
            text,
            kind: ChatKind::GameInfo,
            sender: None,
            ack: None,
        })])
    }

    /// `minecraft:set_title_time`, the split packet for the older action `3`.
    fn handle_play_set_title_time(
        _adapter: &V756Adapter,
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

    /// `minecraft:clear_titles`. The older packet's actions `4` (clear) and
    /// `5` (reset) collapsed into one packet with a boolean, so the boolean
    /// is exactly the `reset_times` the model already carries.
    fn handle_play_clear_titles(
        _adapter: &V756Adapter,
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


    /// `minecraft:tab_complete`. Full parity with 26.2's
    /// `CommandSuggestionsResponse` shape (1.13 introduced this range-based
    /// form), so no client-side bookkeeping is needed the way v1-8/v1-9 need
    /// for their pre-1.13 bare-string-list shape.
    fn handle_play_tab_complete(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
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
            // The tooltip is a real JSON text component, and this era
            // postdates 1.16's hex-colour introduction, so it can
            // carry a `TextColor::Rgb` — keep it as a real `Text` rather
            // than flattening through `Text::to_legacy_string`.
            let tooltip = if reader.bool().map_err(dec_err)? {
                Some(Text::from_json(&reader.string(32_767).map_err(dec_err)?))
            } else {
                None
            };
            suggestions.push(lodestone_model::CommandSuggestionEntry { text, tooltip });
        }
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::CommandSuggestionsReceived {
            id,
            start,
            length,
            suggestions,
        })])
    }

    /// `minecraft:player_info`. A single `action` applies to every entry in
    /// the packet, byte-identical to 1.12.2's/1.8's shape, unlike 26.2's
    /// per-entry action bitmask.
    fn handle_play_player_info(
        adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let body: PlayerInfo = adapter.decode_body_exact(payload)?;
        let mut updated = Vec::new();
        let mut removed = Vec::new();
        for entry in body.entries {
            let blank = || PlayerListEntry {
                uuid: Some(entry.uuid),
                name: None,
                game_mode: None,
                latency: None,
                display_name: None,
                // This era has no separate "listed" bit — every entry the
                // server sends is, by construction, in the tab list.
                listed: None,
                properties: None,
                // This era predates secure chat sessions entirely.
                chat_session: None,
                // This era predates both `UPDATE_LIST_ORDER` and `UPDATE_HAT`
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
                        game_mode: Some(game_mode(u8::try_from(raw_mode).map_err(|_| {
                            AdapterError::Decode(format!(
                                "player_info game mode {raw_mode} out of range"
                            ))
                        })?)?),
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
                        game_mode: Some(game_mode(u8::try_from(raw_mode).map_err(|_| {
                            AdapterError::Decode(format!(
                                "player_info game mode {raw_mode} out of range"
                            ))
                        })?)?),
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
        Ok(directives)
    }

    /// `minecraft:boss_bar`. Action-multiplexed: title is a JSON chat
    /// component; `flags` packs three bits: `0x01` darken sky, `0x02` boss
    /// music, `0x04` create fog.
    fn handle_play_boss_bar(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
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
                return Err(AdapterError::Decode(format!("unknown boss_bar action {other}")));
            }
        };
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::BossBarUpdate { id, action })])
    }

    /// `minecraft:combat_event`. Event `0` (enter combat) carries nothing
    /// further; event `1` (end combat) reads a VarInt duration then a raw
    /// `i32` entity id (unused downstream, matching 26.2's own
    /// `ClientboundPlayerCombatEndPacket`); event `2` (entity died) reads a
    /// VarInt player id, a raw `i32` entity id, then a JSON death-message
    /// string, both ids discarded except the message.
    /// `minecraft:enter_combat_event`, the split packet for the older
    /// combat packet's event `0`. It carries no body at all.
    fn handle_play_enter_combat_event(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        Reader::new(payload).ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::PlayerCombatEntered)])
    }

    /// `minecraft:end_combat_event`, the split packet for event `1`.
    fn handle_play_end_combat_event(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let duration_ticks = reader.var_i32().map_err(dec_err)?;
        reader.i32().map_err(dec_err)?; // entity id, unused downstream
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::PlayerCombatEnded {
            duration_ticks,
        })])
    }

    /// `minecraft:death_combat_event`, the split packet for event `2`.
    fn handle_play_death_combat_event(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        reader.var_i32().map_err(dec_err)?; // player id, unused downstream
        reader.i32().map_err(dec_err)?; // killer entity id, unused downstream
        let message = reader.string(32_767).map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::Death {
            message: Text::from_json(&message),
        })])
    }


    /// `minecraft:world_border`. Action `3` ("initialize") is the only one
    /// that carries every field, in this exact order: x, z, old_radius,
    /// new_radius, speed (VarLong lerp-time ms), portal_boundary (VarInt
    /// absolute max size), warning_time, warning_blocks.
    /// `minecraft:initialize_world_border`, the split packet for the older
    /// world-border packet's action `3`. 1.17 replaced that one
    /// action-selected packet with six, one per action, with the bodies
    /// unchanged.
    fn handle_play_initialize_world_border(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
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
        Ok(vec![Directive::Emit(ClientEvent::WorldBorderInitialized {
            x,
            z,
            old_size,
            new_size,
            lerp_time_ms,
            absolute_max_size,
            warning_blocks,
            warning_time,
        })])
    }

    /// `minecraft:world_border_center`, the split packet for action `2`.
    fn handle_play_world_border_center(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let x = reader.f64().map_err(dec_err)?;
        let z = reader.f64().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(
            ClientEvent::WorldBorderCenterChanged { x, z },
        )])
    }

    /// `minecraft:world_border_size`, the split packet for action `0`.
    fn handle_play_world_border_size(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let size = reader.f64().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(
            ClientEvent::WorldBorderSizeChanged { size },
        )])
    }

    /// `minecraft:world_border_lerp_size`, the split packet for action `1`.
    fn handle_play_world_border_lerp_size(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let old_size = reader.f64().map_err(dec_err)?;
        let new_size = reader.f64().map_err(dec_err)?;
        let lerp_time_ms = reader.var_i64().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(ClientEvent::WorldBorderSizeLerping {
            old_size,
            new_size,
            lerp_time_ms,
        })])
    }

    /// `minecraft:world_border_warning_delay`, the split packet for action `4`.
    fn handle_play_world_border_warning_delay(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let warning_time = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(
            ClientEvent::WorldBorderWarningDelayChanged { warning_time },
        )])
    }

    /// `minecraft:world_border_warning_reach`, the split packet for action `5`.
    fn handle_play_world_border_warning_reach(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let warning_blocks = reader.var_i32().map_err(dec_err)?;
        reader.ensure_empty().map_err(dec_err)?;
        Ok(vec![Directive::Emit(
            ClientEvent::WorldBorderWarningDistanceChanged { warning_blocks },
        )])
    }


    /// `minecraft:teams`. **Field order differs from 1.12.2**: reorders
    /// `prefix`/`suffix` to *after* `formatting` (1.12.2 has them
    /// immediately after `name`, before `friendlyFire`) and widens the
    /// colour field from a raw `i8` ("color") to a VarInt ("formatting").
    /// `displayName`/`prefix`/`suffix` are JSON chat components at this
    /// protocol revision (1.13+), unlike 1.12.2's plain legacy-formatted
    /// strings.
    fn handle_play_teams(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut reader = Reader::new(payload);
        let team = reader.string(16).map_err(dec_err)?;
        let mode = reader.i8().map_err(dec_err)?;
        let read_members = |reader: &mut Reader<'_>| -> Result<Vec<String>, AdapterError> {
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).unwrap_or(0).min(reader.remaining());
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
        Ok(vec![Directive::Emit(ClientEvent::TeamUpdate {
            name: team,
            action,
        })])
    }

    /// `minecraft:scoreboard_display_objective`. Clears the slot with an
    /// empty string rather than a dedicated marker.
    fn handle_play_scoreboard_display_objective(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
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
        Ok(vec![Directive::Emit(ClientEvent::DisplayObjective {
            slot,
            objective,
        })])
    }

    /// `minecraft:scoreboard_objective`. **`type` is a VarInt render-type
    /// ordinal here, unlike 1.12.2's plain string** (`0` = integer, `1` =
    /// hearts; no other render type exists at this protocol revision).
    /// `displayText` is a JSON chat component (1.13+), unlike 1.12.2's
    /// plain legacy-formatted string.
    fn handle_play_scoreboard_objective(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
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
        Ok(vec![Directive::Emit(event)])
    }

    /// `minecraft:scoreboard_score`. `itemName` is the score *holder* and
    /// `scoreName` is the *objective* — the mcdata field names are
    /// misleading, not the wire order. `scoreName` is read unconditionally,
    /// so a `remove` action still names exactly one objective, never
    /// "reset all".
    fn handle_play_scoreboard_score(
        _adapter: &V756Adapter,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
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
        Ok(vec![Directive::Emit(event)])
    }
}

/// `play` clientbound handlers, keyed by the canonical packet name this
/// protocol's own `play::clientbound::ENTRIES` uses for it (minecraft-data's
/// naming; see `docs/protocol-dispatch.md`). `Table::build` checks every one
/// of these names against `ENTRIES` at construction, so a typo here fails
/// loudly at first use rather than silently never dispatching.
static CLIENTBOUND: &[(&str, lodestone_core::dispatch::Handler<PlayHandler>)] = &[
    (
        "minecraft:login",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_login,
        ),
    ),
    (
        "minecraft:map_chunk",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_map_chunk,
        ),
    ),
    (
        "minecraft:update_light",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_update_light,
        ),
    ),
    (
        "minecraft:unload_chunk",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_unload_chunk,
        ),
    ),
    (
        "minecraft:block_action",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_block_action,
        ),
    ),
    (
        "minecraft:entity_equipment",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_entity_equipment,
        ),
    ),
    (
        "minecraft:keep_alive",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_keep_alive,
        ),
    ),
    (
        "minecraft:chat",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_chat,
        ),
    ),
    (
        "minecraft:position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_position,
        ),
    ),
    (
        "minecraft:spawn_entity_living",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_spawn_entity_living,
        ),
    ),
    (
        "minecraft:spawn_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_spawn_entity,
        ),
    ),
    (
        "minecraft:named_entity_spawn",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_named_entity_spawn,
        ),
    ),
    (
        "minecraft:rel_entity_move",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_rel_entity_move,
        ),
    ),
    (
        "minecraft:entity_look",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_entity_look,
        ),
    ),
    (
        "minecraft:entity_move_look",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_entity_move_look,
        ),
    ),
    (
        "minecraft:entity_teleport",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_entity_teleport,
        ),
    ),
    (
        "minecraft:entity_velocity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_entity_velocity,
        ),
    ),
    (
        "minecraft:entity_destroy",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_entity_destroy,
        ),
    ),
    (
        "minecraft:kick_disconnect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_kick_disconnect,
        ),
    ),
    (
        "minecraft:update_health",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_update_health,
        ),
    ),
    (
        "minecraft:respawn",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_respawn,
        ),
    ),
    (
        "minecraft:spawn_position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_spawn_position,
        ),
    ),
    (
        "minecraft:entity_status",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_entity_status,
        ),
    ),
    (
        "minecraft:entity_head_rotation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_entity_head_rotation,
        ),
    ),
    (
        "minecraft:animation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_animation,
        ),
    ),
    (
        "minecraft:abilities",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_abilities,
        ),
    ),
    (
        "minecraft:difficulty",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_difficulty,
        ),
    ),
    (
        "minecraft:update_time",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_update_time,
        ),
    ),
    (
        "minecraft:playerlist_header",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_playerlist_header,
        ),
    ),
    (
        "minecraft:attach_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_attach_entity,
        ),
    ),
    (
        "minecraft:set_passengers",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_set_passengers,
        ),
    ),
    (
        "minecraft:collect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_collect,
        ),
    ),
    (
        "minecraft:entity_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_entity_effect,
        ),
    ),
    (
        "minecraft:remove_entity_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_remove_entity_effect,
        ),
    ),
    (
        "minecraft:spawn_entity_experience_orb",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_spawn_entity_experience_orb,
        ),
    ),
    (
        "minecraft:block_change",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_block_change,
        ),
    ),
    (
        "minecraft:block_break_animation",
        lodestone_core::dispatch::Handler::new(lodestone_core::ProtocolRange::ALL, V756Adapter::handle_play_block_break_animation),
    ),
    (
        "minecraft:explosion",
        lodestone_core::dispatch::Handler::new(lodestone_core::ProtocolRange::ALL, V756Adapter::handle_play_explosion),
    ),
    (
        "minecraft:game_state_change",
        lodestone_core::dispatch::Handler::new(lodestone_core::ProtocolRange::ALL, V756Adapter::handle_play_game_state_change),
    ),
    (
        "minecraft:world_event",
        lodestone_core::dispatch::Handler::new(lodestone_core::ProtocolRange::ALL, V756Adapter::handle_play_world_event),
    ),
    (
        "minecraft:multi_block_change",
        lodestone_core::dispatch::Handler::new(lodestone_core::ProtocolRange::ALL, V756Adapter::handle_play_multi_block_change),
    ),
    (
        "minecraft:entity_update_attributes",
        lodestone_core::dispatch::Handler::new(lodestone_core::ProtocolRange::ALL, V756Adapter::handle_play_entity_update_attributes),
    ),
    (
        "minecraft:experience",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_experience,
        ),
    ),
    (
        "minecraft:vehicle_move",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_vehicle_move,
        ),
    ),
    (
        "minecraft:select_advancement_tab",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_select_advancement_tab,
        ),
    ),
    (
        "minecraft:open_sign_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_open_sign_entity,
        ),
    ),
    (
        "minecraft:camera",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_camera,
        ),
    ),
    (
        "minecraft:update_view_position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_update_view_position,
        ),
    ),
    (
        "minecraft:update_view_distance",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_update_view_distance,
        ),
    ),
    (
        "minecraft:held_item_slot",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_held_item_slot,
        ),
    ),
    (
        "minecraft:close_window",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_close_window,
        ),
    ),
    (
        "minecraft:craft_progress_bar",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_craft_progress_bar,
        ),
    ),
    (
        "minecraft:set_title_text",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_set_title_text,
        ),
    ),
    (
        "minecraft:set_title_subtitle",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_set_title_subtitle,
        ),
    ),
    (
        "minecraft:set_title_time",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_set_title_time,
        ),
    ),
    (
        "minecraft:action_bar",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_action_bar,
        ),
    ),
    (
        "minecraft:clear_titles",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_clear_titles,
        ),
    ),
    (
        "minecraft:tab_complete",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_tab_complete,
        ),
    ),
    (
        "minecraft:player_info",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_player_info,
        ),
    ),
    (
        "minecraft:boss_bar",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_boss_bar,
        ),
    ),
    (
        "minecraft:enter_combat_event",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_enter_combat_event,
        ),
    ),
    (
        "minecraft:end_combat_event",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_end_combat_event,
        ),
    ),
    (
        "minecraft:death_combat_event",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_death_combat_event,
        ),
    ),
    (
        "minecraft:initialize_world_border",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_initialize_world_border,
        ),
    ),
    (
        "minecraft:world_border_center",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_world_border_center,
        ),
    ),
    (
        "minecraft:world_border_size",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_world_border_size,
        ),
    ),
    (
        "minecraft:world_border_lerp_size",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_world_border_lerp_size,
        ),
    ),
    (
        "minecraft:world_border_warning_delay",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_world_border_warning_delay,
        ),
    ),
    (
        "minecraft:world_border_warning_reach",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_world_border_warning_reach,
        ),
    ),
    (
        "minecraft:teams",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_teams,
        ),
    ),
    (
        "minecraft:scoreboard_display_objective",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_scoreboard_display_objective,
        ),
    ),
    (
        "minecraft:scoreboard_objective",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_scoreboard_objective,
        ),
    ),
    (
        "minecraft:scoreboard_score",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V756Adapter::handle_play_scoreboard_score,
        ),
    ),
];

/// `play` clientbound packets deliberately left without a handler, and why.
/// Every id in `play::clientbound::ENTRIES` must appear either here or in
/// [`CLIENTBOUND`] above — `Table::build` rejects construction otherwise,
/// which is the anti-island guard replacing the old if-chain's trailing
/// `Ok(Vec::new())`. Re-derived by grepping the pre-conversion if-chain for
/// every `play::clientbound::X` it tested (54 handled) against the full
/// 92-entry `ENTRIES` table, then checking `crates/versions/26.2/src/adapter/`
/// for each of the 38 gaps by canonical name or an obvious Mojang-naming
/// synonym (v26-2 uses Mojang's own names, which differ from minecraft-data's
/// for the same packet).
static IGNORED: &[lodestone_core::dispatch::IGNORED] = &[
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:spawn_entity_painting",
        "v26-2 has this; backport (painting spawns fold into the generic add_entity path there, ADD_ENTITY)",
    ),
    lodestone_core::dispatch::IGNORED::new("minecraft:statistics", "v26-2 has this; backport (AWARD_STATS)"),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:acknowledge_player_digging",
        "v26-2 has this; backport (BLOCK_CHANGED_ACK)",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:tile_entity_data",
        "v26-2 has this; backport (BLOCK_ENTITY_DATA)",
    ),
    lodestone_core::dispatch::IGNORED::new("minecraft:declare_commands", "v26-2 has this; backport (COMMANDS)"),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:ping",
        "v26-2 has this; backport (PING -- 1.17 renamed the inventory-transaction ack and \
         dropped its window/action fields for a bare i32 id)",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:window_items",
        "v26-2 has this; backport (CONTAINER_SET_CONTENT)",
    ),
    lodestone_core::dispatch::IGNORED::new("minecraft:set_slot", "v26-2 has this; backport (CONTAINER_SET_SLOT)"),
    lodestone_core::dispatch::IGNORED::new("minecraft:set_cooldown", "v26-2 has this; backport (COOLDOWN)"),
    lodestone_core::dispatch::IGNORED::new("minecraft:custom_payload", "v26-2 has this; backport (CUSTOM_PAYLOAD)"),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:named_sound_effect",
        "v26-2 has this; backport (SOUND)",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:open_horse_window",
        "v26-2 has this; backport (MOUNT_SCREEN_OPEN)",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:world_particles",
        "v26-2 has this; backport (LEVEL_PARTICLES)",
    ),
    lodestone_core::dispatch::IGNORED::new("minecraft:map", "v26-2 has this; backport (MAP_ITEM_DATA)"),
    lodestone_core::dispatch::IGNORED::new("minecraft:trade_list", "v26-2 has this; backport (MERCHANT_OFFERS)"),
    lodestone_core::dispatch::IGNORED::new("minecraft:open_book", "v26-2 has this; backport (OPEN_BOOK)"),
    lodestone_core::dispatch::IGNORED::new("minecraft:open_window", "v26-2 has this; backport (OPEN_SCREEN)"),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:craft_recipe_response",
        "v26-2 has this; backport (PLACE_GHOST_RECIPE)",
    ),
    lodestone_core::dispatch::IGNORED::new("minecraft:face_player", "v26-2 has this; backport (PLAYER_LOOK_AT)"),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:unlock_recipes",
        "v26-2 has this; backport (RECIPE_BOOK_ADD/RECIPE_BOOK_REMOVE/RECIPE_BOOK_SETTINGS)",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:resource_pack_send",
        "v26-2 has this; backport (RESOURCE_PACK_PUSH)",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:entity_metadata",
        "v26-2 has this; backport (SET_ENTITY_DATA)",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:entity_sound_effect",
        "v26-2 has this; backport (SOUND_ENTITY)",
    ),
    lodestone_core::dispatch::IGNORED::new("minecraft:sound_effect", "v26-2 has this; backport (SOUND)"),
    lodestone_core::dispatch::IGNORED::new("minecraft:stop_sound", "v26-2 has this; backport (STOP_SOUND)"),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:nbt_query_response",
        "v26-2 has this; backport (TAG_QUERY)",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:advancements",
        "v26-2 has this; backport (UPDATE_ADVANCEMENTS)",
    ),
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:declare_recipes",
        "v26-2 has this; backport (UPDATE_RECIPES)",
    ),
    lodestone_core::dispatch::IGNORED::new("minecraft:tags", "v26-2 has this; backport (UPDATE_TAGS)"),
    // The 1.17 additions this crate does not translate. Each is a real
    // packet on both wires unless a range says otherwise.
    lodestone_core::dispatch::IGNORED::new(
        "minecraft:sculk_vibration_signal",
        "v26-2 has this; backport (the 1.17 sculk-sensor vibration cue)",
    ),
    // 1.18 inserted this at clientbound id 87, which is what shifts the
    // fifteen ids above it. Ranged, so 756's table does not fail
    // construction on a stale entry and 758's does not fail on an unlisted
    // one -- the two failure modes the dispatch builder is there to catch.
    lodestone_core::dispatch::IGNORED::ranged(
        "minecraft:simulation_distance",
        "v26-2 has this; backport (the 1.18 per-player simulation distance)",
        lodestone_core::ProtocolRange::new(PROTOCOL_1_18_2, PROTOCOL_1_18_2),
    ),
];

impl V756Adapter {
    /// Builds this adapter's protocol's `play` clientbound dispatch table
    /// once, from [`CLIENTBOUND`], [`IGNORED`] and that protocol's own
    /// clientbound `ENTRIES`.
    ///
    /// One `OnceLock` per protocol in [`PROTOCOLS`], indexed the same way
    /// [`ids_for`] resolves an id table, so a table built for 1.17.1 can
    /// never be handed to a 1.18.2 adapter. Two tables and not one because
    /// the id→handler mapping genuinely differs: fifteen of the 97 shared
    /// clientbound names move by one across the era.
    ///
    /// # Panics
    ///
    /// Panics if construction fails: a name in [`CLIENTBOUND`] or [`IGNORED`]
    /// that does not match `ENTRIES` for a protocol its declared range
    /// covers, a duplicate handler, or an `ENTRIES` id with neither a handler
    /// nor an ignore entry. Every one of those is a static-table defect
    /// introduced at edit time, not a runtime condition that depends on what
    /// the wire sends, so failing loudly the first time this protocol is used
    /// (rather than silently misdispatching forever) is the correct
    /// behaviour.
    fn play_dispatch_table(&self) -> &'static lodestone_core::dispatch::Table<'static, PlayHandler> {
        static TABLES: [std::sync::OnceLock<
            lodestone_core::dispatch::Table<'static, PlayHandler>,
        >; 2] = [std::sync::OnceLock::new(), std::sync::OnceLock::new()];
        let slot = usize::from(self.protocol != PROTOCOL_1_17_1);
        TABLES[slot].get_or_init(|| {
            lodestone_core::dispatch::Table::build(
                self.protocol,
                self.ids().play_clientbound_entries,
                CLIENTBOUND,
                IGNORED,
            )
            .unwrap_or_else(|err| {
                panic!(
                    "v1-17 play dispatch table for protocol {} must build: every clientbound \
                     ENTRIES id needs either a bound handler or an IGNORED reason covering \
                     this protocol -- {err}",
                    self.protocol
                )
            })
        })
    }

    /// Handles a clientbound packet while in the play state.
    ///
    /// Looks `packet_id` up in the table [`Self::play_dispatch_table`] builds
    /// once per protocol from [`CLIENTBOUND`] and [`IGNORED`].
    /// `Table::build`'s own construction-time check guarantees every id this
    /// protocol's own `ENTRIES` declares has either a handler or a named
    /// ignore reason -- the
    /// anti-island guard this replaces the old if-chain's trailing
    /// `Ok(Vec::new())` with -- but `packet_id` itself reaches this function
    /// straight off the wire (`ClientProtocolDriver` hands the decoded id
    /// through unfiltered), so an id this protocol's own table has never
    /// heard of is a different case from an *unlisted known* id: it is
    /// handled the same defensive way the old `_ =>` arm handled everything,
    /// ignored rather than panicked on, because a malformed or unexpected
    /// byte from the network must never crash the client.
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

impl VersionAdapter for V756Adapter {
    fn protocol_version(&self) -> i32 {
        self.protocol
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["1.17.1", "1.18.2"]
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
                let body = KeepAliveResponse { id: *id };
                Ok(Some((self.ids().keep_alive, self.encode_body(&body)?)))
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
                Ok(Some((self.ids().block_place, self.encode_body(&body)?)))
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
                Ok(Some((self.ids().use_item, self.encode_body(&body)?)))
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
                        "this era's SetCreativeModeSlot with an item requires a ResourceKey -> \
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
            // Faithfully encoding 1.16's `window_click` needs a client-tracked
            // transaction id (the `action` counter, absent from the model which
            // carries only the 1.17+ `state_id`; this adapter is stateless) and
            // an item registry (`ResourceKey` -> numeric id) for the clicked
            // stack. 1.16 slots are flattened, so unlike v1-8/v1-9 there is no
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
                "this era's ContainerClick needs a client-tracked transaction id (model carries \
                 only the 1.17+ state_id) and an item registry; refused rather than sending bytes \
                 a live server rejects via a failed transaction"
                    .to_owned(),
            )),

            // Genuinely absent in 1.16: there is no player-input packet (added
            // much later). `Stab` (off-hand attack) has no dedicated 1.16 packet
            // either.
            ClientAction::Stab => Err(AdapterError::Unsupported(
                "this era has no dedicated off-hand attack (Stab) packet".to_owned(),
            )),
            ClientAction::SetPlayerInput(_) => Err(AdapterError::Unsupported(
                "this era has no player-input packet".to_owned(),
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
                    text_filtering,
                    allow_server_listing,
                    // 1.20.5 introduced the particle-status field; this era
                    // predates it.
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
                    // Written unconditionally; the derive drops it at 756.
                    allow_server_listing: *allow_server_listing,
                };
                Ok(Some((self.ids().settings, self.encode_body(&body)?)))
            }
            ClientAction::SendBrand { brand } => {
                let body = BrandPayload {
                    channel: "minecraft:brand".to_owned(),
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
                // 1.16 reduced serverbound abilities to a single flags byte.
                let body = PlayerAbilities {
                    flags: if *flying { ABILITY_FLYING } else { 0 },
                };
                Ok(Some((self.ids().abilities, self.encode_body(&body)?)))
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
                            "this era's resource_pack_receive has no result code for {other:?}"
                        )));
                    }
                };
                let body = ResourcePackReceive {
                    // Protocol 110 only; `until = 110` drops it here.
                    hash: String::new(),
                    result,
                };
                Ok(Some((
                    self.ids().resource_pack_receive,
                    self.encode_body(&body)?,
                )))
            }
            ClientAction::PongResponse { .. } => Err(AdapterError::Unsupported(
                "this era predates the play ping/pong packets (added in 1.17)".to_owned(),
            )),
            ClientAction::EndClientTick => Err(AdapterError::Unsupported(
                "this era has no client_tick_end packet".to_owned(),
            )),
            ClientAction::RenameItem { .. } => Err(AdapterError::Unsupported(
                "this era's rename item encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SelectTrade { .. } => Err(AdapterError::Unsupported(
                "this era's select trade encoding is not yet implemented".to_owned(),
            )),
            ClientAction::PickItemFromBlock { .. } => Err(AdapterError::Unsupported(
                "this era's pick item from block encoding is not yet implemented".to_owned(),
            )),
            ClientAction::PickItemFromEntity { .. } => Err(AdapterError::Unsupported(
                "this era's pick item from entity encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SetBeaconEffects { .. } => Err(AdapterError::Unsupported(
                "this era's set beacon encoding requires a mob-effect registry that is not yet \
                 available"
                    .to_owned(),
            )),
            ClientAction::EditBook { .. } => Err(AdapterError::Unsupported(
                "this era's edit book encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SignUpdate { .. } => Err(AdapterError::Unsupported(
                "this era's sign update encoding is not yet implemented".to_owned(),
            )),
            ClientAction::SetCommandBlock { .. } => Err(AdapterError::Unsupported(
                "this era's set command block encoding is not yet implemented".to_owned(),
            )),
            ClientAction::PlayerLoaded => Err(AdapterError::Unsupported(
                "this era's predates the player_loaded packet (added in 1.20.2)".to_owned(),
            )),
            ClientAction::SeenAdvancements { .. } => Err(AdapterError::Unsupported(
                "this era's advancements encoding is not yet implemented".to_owned(),
            )),
            ClientAction::CommandSuggestion { id, command } => {
                // `packet_tab_complete` (minecraft-data 1.16.2): `transactionId:
                // varint, text: string` — full parity with 26.2's serverbound
                // shape, so `id` round-trips on the wire itself and needs no
                // client-side bookkeeping the way v1-8/v1-9 need.
                let mut writer = Writer::default();
                writer.var_i32(*id);
                writer.string(command);
                Ok(Some((self.ids().tab_complete, writer.into_vec())))
            }
            ClientAction::PaddleBoat { .. } => Err(AdapterError::Unsupported(
                "this era's paddle boat encoding is not yet implemented".to_owned(),
            )),
            ClientAction::MoveVehicle { .. } => Err(AdapterError::Unsupported(
                "this era's move vehicle encoding is not yet implemented".to_owned(),
            )),

            // Leaving the death screen. `client_command` action `0` =
            // perform respawn, a stable ordinal across every generation
            // checked (1.8, 1.12.2, 1.16.2/.4/.5 all encode it as a lone
            // varint action id per minecraft-data's protocol.json).
            ClientAction::Respawn => {
                let body = ClientCommand { action: 0 };
                Ok(Some((self.ids().client_command, self.encode_body(&body)?)))
            }
            // Clicking a name in the tab list while spectating. This era's
            // `spectate` packet carries the target's uuid directly, which the
            // model already supplies, so no entity registry is needed.
            ClientAction::TeleportToEntity { target } => {
                let body = Spectate { target: *target };
                Ok(Some((self.ids().spectate, self.encode_body(&body)?)))
            }
            // The continuous spectator-follow action carries only a network
            // entity id, but this era's wire packet is the same uuid-keyed
            // `spectate` packet as `TeleportToEntity` above. A stateless
            // adapter has no id->uuid registry to bridge the two.
            ClientAction::SpectatorAction { .. } => Err(AdapterError::Unsupported(
                "this era's spectate packet needs a target uuid; SpectatorAction carries \
                 only a network entity id with no registry to resolve it into one (use \
                 TeleportToEntity instead, which already carries the uuid)"
                    .to_owned(),
            )),
            ClientAction::ChatAck { .. } => Err(AdapterError::Unsupported(
                "this era predates signed/acknowledged chat (added in 1.19)".to_owned(),
            )),
            ClientAction::SelectBundleItem { .. } => Err(AdapterError::Unsupported(
                "this era predates bundles (added in 1.21.2)".to_owned(),
            )),
            ClientAction::SetContainerSlotState { .. } => Err(AdapterError::Unsupported(
                "this era predates the crafter block (added in 1.21)".to_owned(),
            )),
            // All four recipe books exist by this era, so this needs no
            // version-specific fallback the way protocol 340's does.
            ClientAction::SetRecipeBookSettings {
                book_type,
                open,
                filtering,
            } => {
                let ordinal = recipe_book_type_to_ordinal(*book_type);
                let payload = self.encode_body(&RecipeBook {
                    book_id: ordinal,
                    book_open: *open,
                    filter_active: *filtering,
                })?;
                Ok(Some((self.ids().recipe_book, payload)))
            }
            // Both packets identify a recipe by a namespaced string id in
            // this era (`craft_recipe_request.recipe` and
            // `displayed_recipe.recipeId`, both `string` per minecraft-data's
            // 1.16.2 protocol.json) rather than the numeric index the model
            // carries, and this stateless adapter has no recipe registry to
            // resolve one into the other.
            ClientAction::RecipeBookSeenRecipe { .. } | ClientAction::PlaceRecipe { .. } => {
                Err(AdapterError::Unsupported(
                    "this era's recipe-book packets identify a recipe by a namespaced \
                     string id; the model's display index has no registry to resolve into one"
                        .to_owned(),
                ))
            }
            ClientAction::PingRequest { .. } => Err(AdapterError::Unsupported(
                "this era has no play-state ping request packet".to_owned(),
            )),
            ClientAction::ChangeGameMode { .. } => Err(AdapterError::Unsupported(
                "this era has no dedicated change_game_mode packet; a debug-menu game-mode \
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
        let adapter = V756Adapter::new();
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

    fn encoded_update(adapter: &V756Adapter, wire_id: i32) -> Vec<u8> {
        if adapter.protocol_version() == PROTOCOL_1_17_1 {
            adapter
                .encode_body(&EntityEffect {
                    entity_id: 42,
                    effect_id: i8::try_from(wire_id).expect("test id fits the legacy byte"),
                    amplifier: 0,
                    duration: 40,
                    flags: 0,
                })
                .expect("entity effect encodes")
        } else {
            adapter
                .encode_body(&EntityEffectVarint {
                    entity_id: 42,
                    effect_id: wire_id,
                    amplifier: 0,
                    duration: 40,
                    flags: 0,
                })
                .expect("entity effect encodes")
        }
    }

    fn encoded_remove(adapter: &V756Adapter, wire_id: i32) -> Vec<u8> {
        if adapter.protocol_version() == PROTOCOL_1_17_1 {
            adapter
                .encode_body(&RemoveEntityEffect {
                    entity_id: 42,
                    effect_id: i8::try_from(wire_id).expect("test id fits the legacy byte"),
                })
                .expect("remove entity effect encodes")
        } else {
            adapter
                .encode_body(&RemoveEntityEffectVarint {
                    entity_id: 42,
                    effect_id: wire_id,
                })
                .expect("remove entity effect encodes")
        }
    }

    #[test]
    fn both_legacy_packet_forms_resolve_one_based_speed_id() {
        for &protocol in PROTOCOLS {
            let adapter = V756Adapter::for_protocol(protocol);
            let mut world = World::new();
            let applied = V756Adapter::handle_play_entity_effect(
                &adapter,
                &mut world,
                &encoded_update(&adapter, 1),
            )
            .expect("known legacy effect decodes");
            let [Directive::Emit(ClientEvent::MobEffectApplied { effect, .. })] = applied.as_slice()
            else {
                panic!("known effect did not emit one application event: {applied:?}");
            };
            assert_eq!(effect.path(), "speed", "protocol {protocol}");

            let removed = V756Adapter::handle_play_remove_entity_effect(
                &adapter,
                &mut world,
                &encoded_remove(&adapter, 1),
            )
            .expect("known legacy effect removal decodes");
            let [Directive::Emit(ClientEvent::MobEffectRemoved { effect, .. })] = removed.as_slice()
            else {
                panic!("known effect did not emit one removal event: {removed:?}");
            };
            assert_eq!(effect.path(), "speed", "protocol {protocol}");
        }
    }

    #[test]
    fn unknown_legacy_effect_ids_are_rejected_at_packet_ingress() {
        let unknown_ids = [0, lodestone_data::mob_effects::MOB_EFFECT_COUNT as i32 + 1];
        for &protocol in PROTOCOLS {
            let adapter = V756Adapter::for_protocol(protocol);
            for wire_id in unknown_ids {
                let mut world = World::new();
                let error = V756Adapter::handle_play_entity_effect(
                    &adapter,
                    &mut world,
                    &encoded_update(&adapter, wire_id),
                )
                .expect_err("unknown update effect must fail closed");
                assert!(
                    error.to_string().contains(&format!("unknown legacy effect id {wire_id}")),
                    "protocol {protocol}, update id {wire_id}: {error}"
                );

                let error = V756Adapter::handle_play_remove_entity_effect(
                    &adapter,
                    &mut world,
                    &encoded_remove(&adapter, wire_id),
                )
                .expect_err("unknown removal effect must fail closed");
                assert!(
                    error.to_string().contains(&format!("unknown legacy effect id {wire_id}")),
                    "protocol {protocol}, remove id {wire_id}: {error}"
                );
            }
        }

        assert!(
            V756Adapter::legacy_mob_effect_id(i32::MIN).is_err(),
            "checked subtraction must keep an extreme wire value from overflowing"
        );
    }
}

// `CLIENTBOUND`, `IGNORED` and `PlayHandler` are crate-private -- they are
// this module's own dispatch-table plumbing, not part of the crate's public
// API -- so an integration test under `tests/` cannot name them (an
// integration test binary only sees the crate's `pub` surface). Exposing
// them (or a `#[doc(hidden)] pub` accessor) solely so an external test file
// could reach them would leak internal representation for no benefit over
// a unit-test module here, which already has direct access. See
// `docs/testing-policy.md`: this is exactly the "would this realistically
// break, and would this test be how we find out" case for `Table::build`'s
// construction-time check.
#[cfg(test)]
mod dispatch_coverage_tests {
    use super::*;

    /// The real table, built from the real `ENTRIES`/`CLIENTBOUND`/`IGNORED`
    /// for **each** protocol in this era, must construct successfully --
    /// meaningful specifically because `Table::build` fails loudly the moment
    /// any clientbound id is neither handled nor declared `IGNORED` for that
    /// protocol. Two protocols and one handler list is the whole claim of an
    /// era crate, so both are built here rather than just the newest.
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

    /// The two tables really are two different id->handler mappings.
    ///
    /// Not a restatement of the build above: it would pass just as happily if
    /// both tables were identical. The era's whole id drift is the fifteen
    /// clientbound names at or above the id 1.18 gave `simulation_distance`,
    /// so the probes are one that moved (`entity_teleport`, 97 -> 98) and one
    /// that did not (`keep_alive`, 33 in both) -- the pair separates the two
    /// tables *and* pins that the drift is a shift rather than a wholesale
    /// renumbering, which neither packet does alone.
    #[test]
    fn each_protocols_table_uses_its_own_ids() {
        let ids = |protocol: i32, name: &str| {
            ids_for(protocol)
                .play_clientbound_entries
                .iter()
                .find(|(entry, _)| *entry == name)
                .map(|(_, id)| *id)
                .expect("every protocol in this era carries both probes")
        };
        assert_eq!(
            [
                (
                    ids(PROTOCOL_1_17_1, "minecraft:entity_teleport"),
                    ids(PROTOCOL_1_17_1, "minecraft:keep_alive"),
                ),
                (
                    ids(PROTOCOL_1_18_2, "minecraft:entity_teleport"),
                    ids(PROTOCOL_1_18_2, "minecraft:keep_alive"),
                ),
            ],
            [(97, 33), (98, 33)]
        );
    }

    /// Negative control: drop the last `IGNORED` entry
    /// (`minecraft:tags`) from a local copy of the list. Its packet id then
    /// has neither a bound handler nor a matching ignore entry -- exactly
    /// the `_ =>` island this table exists to catch -- so construction must
    /// fail, and by name, proving the detector actually works rather than
    /// just trusting the happy-path test above.
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
            "dropping the minecraft:tags IGNORED entry must fail construction on that exact packet"
        );
    }
}
