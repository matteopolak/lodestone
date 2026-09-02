//! [`VersionAdapter`] implementation driving the protocol 47 join flow.

use std::collections::HashMap;
use std::sync::{Arc, LockResult, Mutex, MutexGuard, PoisonError};

use lodestone_canonical::canonical::{self, CanonicalBlockState, FallbackTally};
use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_data::block_entity_types::block_entity_type;
use lodestone_data::block_states;
use lodestone_data::mob_effects::mob_effect_name;
use lodestone_model::{
    AdapterError, AnimationAction, BlockActionKind, BlockFace, ChatKind, ChatMode, ChunkPos,
    ClientAction, ClientEvent, ClientSettings, CollisionRule, ConnectionState, Difficulty,
    Directive, DisplayedSkinParts, DisplaySlot, EntityEquipment, EntityInteraction,
    EntityMovement, EquipmentSlot, GameMode, Hand, ItemStack, LoginProfile, ObjectiveMode,
    ObjectiveRenderType, PlayerCommand, PlayerListEntry, ProfileProperty, ResourceKey, Rotation,
    SectionPos, ServerAddress, SoundCategory, TeamAction, TeamColor, TeamParameters,
    TeleportFlags, Text, Vec3, VersionAdapter, Visibility,
};
use lodestone_world::{ChunkPos as WorldChunkPos, Heightmaps, LoadedChunk, WorldSink};

use crate::entity_metadata;
use crate::entity_types;
use crate::item_types;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::chunk::{ChunkShape, MapChunk, MapChunkBulk};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse};
use crate::packets::entity::{
    Animation, AttachEntity, ClientboundEntityEquipment, Collect, EntityDestroy, EntityEffect,
    EntityLook, EntityMetadataPacket, EntityMoveLook, EntityTeleport, EntityVelocityPacket,
    RelEntityMove, RemoveEntityEffect, SpawnEntityExperienceOrb, SpawnEntityLiving,
    SpawnEntityPainting, SpawnEntityWeather, SpawnObject,
};
use crate::packets::game::{
    BlockDig, BlockPlace, CameraPacket, ClientCommand, ClientboundChat, ClientboundPositionLook,
    DifficultyPacket, EntityAction, Experience, JoinGame, KickDisconnect, PlayerlistHeader,
    PlaySetCompression, Respawn, ServerboundChat, ServerboundFlying, ServerboundLook,
    ServerboundPosition, ServerboundPositionLook, Spectate, SpawnPosition, UpdateHealth,
    UseEntity, UseEntityAt,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{EncryptionRequest, LoginDisconnect, LoginSuccess, SetCompression};
use crate::packets::player_info::{PlayerInfo, PlayerInfoAction};
use crate::packets::position::Position;
use crate::packets::settings::{BrandPayload, PlayerAbilities, Settings};
use crate::packets::slot::Slot;
use crate::packets::window::{
    CloseWindow, EnchantItem, HeldItemSlot, OpenWindow, ServerboundCloseWindow,
    ServerboundHeldItemSlot, SetCreativeSlot, SetSlot, WindowItems,
};
use crate::packets::world::{
    BlockAction, BlockBreakAnimation, NamedSoundEffect, OpenSignEntity, WorldEvent,
};

/// Protocol version implemented by this adapter.
pub const PROTOCOL: i32 = 47;

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

/// Per-connection state used by 1.8.9's `EntityPlayerSP.onUpdateWalkingPlayer`.
///
/// Unlike later clients, the 1.8 branch emits its base `flying` packet every
/// tick when neither position nor rotation is dirty. Its reminder counter is
/// checked before it is incremented, so a forced position update follows 20
/// idle `flying` packets.
#[derive(Debug, Clone, Copy)]
struct MovementSendState {
    last_pos: Vec3,
    last_yaw: f32,
    last_pitch: f32,
    position_reminder: u32,
}


impl Default for MovementSendState {
    fn default() -> Self {
        Self {
            last_pos: Vec3::new(0.0, 0.0, 0.0),
            last_yaw: 0.0,
            last_pitch: 0.0,
            position_reminder: 0,
        }
    }
}

fn recover_movement_state<'a>(
    result: LockResult<MutexGuard<'a, MovementSendState>>,
) -> MutexGuard<'a, MovementSendState> {
    result.unwrap_or_else(PoisonError::into_inner)
}

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
    /// Entity id -> canonical mob-type identifier (e.g. `"minecraft:sheep"`),
    /// recorded at `spawn_entity_living` and consulted by the standalone
    /// `entity_metadata` packet, which carries no type of its own. Without
    /// this, the incremental packet cannot know which
    /// [`crate::entity_metadata::MobProfile`] applies and would have to guess
    /// — exactly the index-collision trap the metadata module's docs warn
    /// about. Entries are removed on `entity_destroy` so this stays bounded
    /// to currently-tracked mobs rather than growing for the life of the
    /// connection.
    entity_kinds: Arc<Mutex<HashMap<i32, &'static str>>>,
    /// The raw 1.8 dimension byte from the most recent `login`/`respawn`,
    /// kept alongside `shape` (which only distinguishes "has skylight" from
    /// "does not") because `spawn_position` needs the real
    /// [`lodestone_model::DimensionId`], and nether (`-1`) vs end (`1`)
    /// collapse to the same `ChunkShape`.
    dimension: Arc<Mutex<i8>>,
    /// Vehicle entity id -> its current passenger ids, in mount order.
    /// `attach_entity` (mount/ride branch) sends one relation at a time; this
    /// is the adapter-side fold that turns that stream into the full list
    /// [`ClientEvent::EntityPassengersChanged`] expects, mirroring
    /// `entity_kinds`' role for `entity_metadata`.
    vehicle_passengers: Arc<Mutex<HashMap<i32, Vec<i32>>>>,
    /// Passenger entity id -> the vehicle it currently rides, so a dismount
    /// (`vehicle_id == -1`) knows which `vehicle_passengers` entry to prune
    /// without scanning every vehicle.
    passenger_vehicle: Arc<Mutex<HashMap<i32, i32>>>,
    /// The most recently sent `ClientAction::CommandSuggestion`, remembered
    /// because 1.8's `tab_complete` reply carries neither a transaction id
    /// nor a replacement range (both added in 1.13) — see the `TAB_COMPLETE`
    /// arm in `handle_play` for how this is used to reconstruct them.
    pending_tab_complete: Arc<Mutex<Option<PendingTabComplete>>>,
    movement: Arc<Mutex<MovementSendState>>,
}

/// The half of an outgoing `command_suggestion` request 1.8's reply does not
/// echo back, kept just long enough to answer the one reply it produced.
/// Overwritten rather than queued: only one tab-complete request is ever in
/// flight, mirroring `lodestone_shell::chat::SuggestionRequests`'s own
/// single-`pending` design on the other end of this same round trip.
#[derive(Debug, Clone)]
struct PendingTabComplete {
    id: i32,
    command: String,
}

/// Byte offset of the last whitespace-delimited word in `text` — the start of
/// the range 1.8/1.12's `tab_complete` matches replace, since those versions
/// send full replacement strings for that word rather than a server-declared
/// range. Mirrors `CommandSuggestions.getLastWordIndex`'s shape: the offset
/// just past the final run of whitespace, or `0` when there is none.
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
            entity_kinds: Arc::new(Mutex::new(HashMap::new())),
            dimension: Arc::new(Mutex::new(0)),
            vehicle_passengers: Arc::new(Mutex::new(HashMap::new())),
            passenger_vehicle: Arc::new(Mutex::new(HashMap::new())),
            pending_tab_complete: Arc::new(Mutex::new(None)),
            movement: Arc::new(Mutex::new(MovementSendState::default())),
        }
    }

    /// Selects 1.8.9's player-movement wire shape from state last sent by this
    /// adapter. The base `flying` packet is deliberate: pre-1.9 vanilla sends
    /// it on every otherwise-idle tick rather than returning no packet.
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

    /// Records the canonical mob-type name a `spawn_entity_living` announced
    /// for `entity_id`, so a later standalone `entity_metadata` packet for
    /// the same id can be folded with the right [`entity_metadata::fold`]
    /// gating.
    fn remember_kind(&self, entity_id: i32, kind: &'static str) {
        if let Ok(mut kinds) = self.entity_kinds.lock() {
            kinds.insert(entity_id, kind);
        }
    }

    /// Returns the canonical mob-type name recorded for `entity_id`, if this
    /// adapter saw it spawn as a mob.
    fn kind_for(&self, entity_id: i32) -> Option<&'static str> {
        self.entity_kinds
            .lock()
            .ok()
            .and_then(|kinds| kinds.get(&entity_id).copied())
    }

    /// Forgets every id in `entity_ids` — called on `entity_destroy` so the
    /// map does not grow for the life of the connection.
    fn forget_kinds(&self, entity_ids: &[i32]) {
        if let Ok(mut kinds) = self.entity_kinds.lock() {
            for id in entity_ids {
                kinds.remove(id);
            }
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
        if let Ok(mut dim) = self.dimension.lock() {
            *dim = dimension;
        }
    }

    /// Returns the current dimension's chunk shape.
    fn current_shape(&self) -> ChunkShape {
        self.shape
            .lock()
            .map_or_else(|_| ChunkShape::overworld(), |shape| *shape)
    }

    /// Returns the raw 1.8 dimension byte from the most recent
    /// `login`/`respawn`.
    fn current_dimension(&self) -> i8 {
        self.dimension.lock().map_or(0, |dim| *dim)
    }

    /// Records that `passenger` now rides `vehicle`, returning `vehicle`'s
    /// full updated passenger list. If `passenger` already rode a different
    /// vehicle, it is removed from that vehicle's list first (a mount always
    /// implies leaving any prior one, matching vanilla's single-vehicle
    /// invariant).
    fn mount(&self, vehicle: i32, passenger: i32) -> Vec<i32> {
        let (Ok(mut passengers), Ok(mut owner)) =
            (self.vehicle_passengers.lock(), self.passenger_vehicle.lock())
        else {
            return vec![passenger];
        };
        if let Some(&previous) = owner.get(&passenger) {
            if previous != vehicle {
                if let Some(list) = passengers.get_mut(&previous) {
                    list.retain(|&id| id != passenger);
                }
            }
        }
        let list = passengers.entry(vehicle).or_default();
        if !list.contains(&passenger) {
            list.push(passenger);
        }
        owner.insert(passenger, vehicle);
        list.clone()
    }

    /// Records that `passenger` dismounted, returning the former vehicle id
    /// and its remaining passenger list, if `passenger` was tracked as
    /// riding one.
    fn dismount(&self, passenger: i32) -> Option<(i32, Vec<i32>)> {
        let (Ok(mut passengers), Ok(mut owner)) =
            (self.vehicle_passengers.lock(), self.passenger_vehicle.lock())
        else {
            return None;
        };
        let vehicle = owner.remove(&passenger)?;
        let list = passengers.get_mut(&vehicle)?;
        list.retain(|&id| id != passenger);
        Some((vehicle, list.clone()))
    }

    /// Forgets any vehicle/passenger bookkeeping for destroyed entities —
    /// called alongside `forget_kinds` on `entity_destroy` so neither map
    /// grows for the life of the connection.
    fn forget_vehicles(&self, entity_ids: &[i32]) {
        if let (Ok(mut passengers), Ok(mut owner)) =
            (self.vehicle_passengers.lock(), self.passenger_vehicle.lock())
        {
            for id in entity_ids {
                passengers.remove(id);
                owner.remove(id);
            }
            for list in passengers.values_mut() {
                list.retain(|passenger| !entity_ids.contains(passenger));
            }
        }
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
pub fn adapter_for(protocol: i32) -> V47Adapter {
    debug_assert!(
        PROTOCOLS.contains(&protocol),
        "adapter_for({protocol}) is outside this family's PROTOCOLS ({PROTOCOLS:?}); \
         callers must test membership before constructing"
    );
    V47Adapter::new()
}

/// Encodes a packet body into a fresh byte buffer.
///
/// Thin wrapper over the version-free [`lodestone_core::encode_body`], which
/// returns a stringified error because `AdapterError` lives in
/// `lodestone-model` and `lodestone-core` cannot depend on it.
fn encode_body<T: Encode>(packet: &T) -> Result<Vec<u8>, AdapterError> {
    lodestone_core::encode_body(packet, CTX).map_err(AdapterError::Encode)
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

/// Maps a decode error to the adapter's decode-error variant. Used by the
/// hand-decoded arms (`entity_status`/`entity_head_rotation`/`block_change`/
/// `multi_block_change`) that read a [`Reader`] directly rather than going
/// through a derived [`Decode`] body, mirroring `lodestone-v770`'s own
/// `dec_err` helper.
fn dec_err(err: impl std::fmt::Display) -> AdapterError {
    AdapterError::Decode(err.to_string())
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

/// Largest block coordinate any vanilla world can legitimately contain, on
/// either horizontal axis: `WorldBorder.absoluteMaxSize` (`WorldBorder.java`)
/// is 29,999,984, and the border is what bounds every world regardless of the
/// `worldborder` command or the world's own settings. Anything past this is not
/// an awkward-but-real position, it is invalid input.
const ABSOLUTE_MAX_BLOCK: i32 = 29_999_984;

/// Turns a wire-supplied chunk coordinate into the block coordinate of its
/// west/north edge, refusing anything the world border makes impossible.
///
/// Mirrors `lodestone-v340`'s identically-named helper, added there after
/// `lodestone-fuzz`'s `handle_packet_never_panics` target found an unchecked
/// `chunk_x * 16` panics in debug and silently wraps in release for any
/// `|chunk|` past `i32::MAX / 16` — a wrapped coordinate writes a block at a
/// position the packet never named, which is a corrupted world rather than a
/// crash. `multi_block_change` shares 1.12.2's wire shape exactly, so it
/// shares the hazard.
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

/// Converts a decoded 1.8 [`Slot`] into the canonical
/// [`Option<ItemStack>`](ItemStack).
///
/// Resolves the item **family** from [`item_types`] but not `damage` (see
/// that module's doc for why turning damage into a variant would be a
/// guess); every stack carries no components. An id absent from the 1.8
/// item table decodes to `None` (an empty slot) rather than failing the
/// whole packet — a single unresolvable item must not make an entire chest
/// or player inventory unopenable.
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

/// Resolves a 1.8 `open_window` `inventory_type` string to a canonical
/// `minecraft:menu` key from 26.2's own registry
/// (`lodestone_data::generated::menus::MENU_NAMES` lists the 25 real
/// entries this is checked against).
///
/// Two cases are **judgement calls, not lookups**, both driven by real wire
/// data rather than guessed:
///
/// * `"minecraft:chest"`/`"minecraft:container"` and `"EntityHorse"` have no
///   fixed modern menu — vanilla's own `ChestMenu`/`ChestType` picks
///   `generic_9x{rows}` from the container's slot count, and 26.2 has no
///   dedicated horse menu type at all (`MENU_NAMES` has no horse entry; the
///   horse GUI became a normal generic-shaped container upstream). Both are
///   resolved from `slot_count` the same way vanilla's own chest menu
///   derives its row count, clamped to the `1..=6` rows 26.2's registry
///   actually has.
/// * every other 1.8 `inventory_type` string here is a real, unchanged
///   `minecraft:*` key already (confirmed against
///   `vendor/minecraft-data/data/pc/1.8/windows.json`), so it is mapped
///   directly rather than through a table that could drift.
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

/// Maps a 1.8 `entity_equipment`/`item_equipment` slot ordinal to the
/// canonical [`EquipmentSlot`].
///
/// **Not** [`EquipmentSlot::from_ordinal`]: that table is the *modern*
/// `EquipmentSlot` enum's declaration order, which inserts `OffHand` at
/// ordinal `1` (a 1.9 addition) and shifts every armor slot down by one.
/// 1.8 has no off-hand at all — its five ordinals are `0` held item, `1`
/// boots, `2` leggings, `3` chestplate, `4` helmet — verified against
/// minecraft-data's 1.8 `packet_entity_equipment` (which only ever carries
/// `0..=4`) and against `EntityLiving.getEquipmentInSlot`'s five-slot array
/// in the same era. Using the modern table here would silently render every
/// 1.8 boots-equip as an off-hand item and shift the rest of the armor one
/// slot off.
fn legacy_equipment_slot(ordinal: i16) -> Result<EquipmentSlot, AdapterError> {
    match ordinal {
        0 => Ok(EquipmentSlot::MainHand),
        1 => Ok(EquipmentSlot::Feet),
        2 => Ok(EquipmentSlot::Legs),
        3 => Ok(EquipmentSlot::Chest),
        4 => Ok(EquipmentSlot::Head),
        other => Err(AdapterError::Decode(format!(
            "entity_equipment slot ordinal {other} is outside 1.8's 0..=4 range"
        ))),
    }
}

/// Resolves a 1-based legacy `minecraft:mob_effect` id to its canonical
/// resource key.
///
/// The shared [`lodestone_data::mob_effects`] table is 0-based (the modern
/// registry network id); 1.8's `entity_effect`/`remove_entity_effect` ids
/// have been stable and 1-based in the same relative order since Beta 1.8,
/// matching `lodestone-v340`'s identical `- 1` adjustment for 1.12.2.
fn legacy_effect_key(effect_id: i8) -> Result<ResourceKey, AdapterError> {
    let name = mob_effect_name(i32::from(effect_id) - 1)
        .ok_or_else(|| AdapterError::Decode(format!("unknown legacy effect id {effect_id}")))?;
    name.parse()
        .map_err(|_| AdapterError::Decode(format!("effect id {name} is not a key")))
}

/// Resolves a legacy numeric block-**type** id (no metadata component, as
/// `block_action`/`block_break_animation`-adjacent packets carry) to a
/// canonical block-family resource key.
///
/// `block_action`'s wire shape carries no metadata at all, unlike
/// `block_change`'s `id:meta` composite, and every block that can trigger a
/// block-event resolves to the same canonical family regardless of
/// metadata — only within-family blockstate properties (piston facing,
/// chest orientation) vary with it, and [`ClientEvent::BlockEvent`] only
/// needs the family. But `meta = 0` is not always a populated slot in the
/// legacy flattening table: a chest/ender_chest/trapped_chest id has no
/// entry at meta `0` or `1` — only `2..=5` (facing) were ever real chest
/// orientations — so a fixed `meta = 0` would silently resolve every
/// chest-lid `block_action` to air. Scanning every meta and taking the
/// first `Resolved` slot is family-only-safe (any meta the table does
/// populate names the same block).
fn legacy_block_type_key(block_id: u8) -> ResourceKey {
    let state = (0u8..16)
        .find_map(|meta| match canonical::resolve(block_id, meta) {
            CanonicalBlockState::Resolved(state) => Some(state),
            _ => None,
        })
        .unwrap_or_else(canonical::air_state_id);
    block_states::block_name(state)
        .unwrap_or("minecraft:air")
        .parse()
        .unwrap_or_else(|_| "minecraft:air".parse().expect("minecraft:air is valid"))
}

/// Maps a 1.8 team-color byte (a legacy chat formatting colour code,
/// `0..=15`) to the canonical [`TeamColor`], or `None` for `-1` (no colour).
const fn team_color_from_byte(byte: i8) -> Option<TeamColor> {
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
///
/// Delegates to the version-free [`lodestone_core::unpack_degrees`], which has
/// the same formula and is used identically by v340 and v735.
fn unpack_degrees(packed: i8) -> f32 {
    lodestone_core::unpack_degrees(packed)
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

/// Packs a [`DisplayedSkinParts`] into the 1.8 skin-parts bitmask.
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
const fn chat_mode_value(mode: ChatMode) -> i8 {
    match mode {
        ChatMode::Full => 0,
        ChatMode::CommandsOnly => 1,
        ChatMode::Hidden => 2,
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::LOGIN`.
    fn handle_play_login(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::MAP_CHUNK`.
    fn handle_play_map_chunk(
        &self,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::MAP_CHUNK_BULK`.
    fn handle_play_map_chunk_bulk(
        &self,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::KEEP_ALIVE`.
    fn handle_play_keep_alive(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let keep_alive: KeepAliveRequest = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::KeepAlive {
                id: i64::from(keep_alive.id),
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::CHAT`.
    fn handle_play_chat(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: ClientboundChat = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::Chat {
                text: Text::from_json(&body.message),
                kind: chat_kind(body.position),
                // 1.8's chat packet carries no sender field — nothing to filter on.
                sender: None,
                ack: None,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::POSITION`.
    fn handle_play_position(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

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
            // echoing the received coordinates confirms it exactly — that case
            // sends the confirmation immediately, below.
            //
            // A **relative** component cannot be echoed this way: `body.x/y/z`
            // are deltas in that case, not absolute coordinates, and a pure
            // adapter does not own the player's current position needed to
            // resolve them. Echoing the raw delta back as if it were absolute
            // sends the server a bogus position, which it never recognises as
            // matching the pending teleport — the server keeps holding movement,
            // and every following packet re-triggers the same mismatch. That is
            // exactly the "couldn't move at all, kept getting rubber-banded"
            // failure shape. So a relative-flagged packet sends **no** immediate
            // echo here; the physics layer resolves `ClientEvent::TeleportPlayer`
            // (relative components against its own current position, same as
            // every other family) into an absolute pose, and the very next
            // `ClientAction::Move` this adapter encodes — which lowers to this
            // same `POSITION_LOOK` packet id — carries that resolved absolute
            // position. It arrives one tick later, but it is correct, and a
            // wrong-but-immediate echo is worse than a correct-but-one-tick-late
            // one.
            let mut directives = Vec::with_capacity(2);
            if body.flags == 0 {
                let confirm = ServerboundPositionLook {
                    x: body.x,
                    y: body.y,
                    z: body.z,
                    yaw: body.yaw,
                    pitch: body.pitch,
                    on_ground: false,
                };
                directives.push(send(play::serverbound::POSITION_LOOK, &confirm)?);
            }
            directives.push(Directive::Emit(ClientEvent::TeleportPlayer {
                pos: Vec3::new(body.x, body.y, body.z),
                rotation: Rotation::new(body.yaw, body.pitch),
                flags,
            }));
            return Ok(directives);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SPAWN_ENTITY_LIVING`.
    fn handle_play_spawn_entity_living(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Reuse the existing derived mob-spawn decoder (varint id, u8 type,
            // fixed-point i32 coords, byte angles, i16 velocity, metadata). 1.8
            // mobs carry no UUID.
            let body: SpawnEntityLiving = decode_body(payload)?;
            let type_id = i32::from(body.kind);
            let kind_name = entity_types::mob_type_name(type_id).ok_or_else(|| {
                AdapterError::Decode(format!("unknown mob type id {type_id} in spawn"))
            })?;
            let entity_type = kind_name
                .parse()
                .map_err(|_| AdapterError::Decode(format!("mob type id {type_id} is not a key")))?;
            self.remember_kind(body.entity_id, kind_name);
            let mut directives = vec![Directive::Emit(ClientEvent::EntitySpawned {
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
            })];
            // 1.8 embeds the mob's *entire* registered DataWatcher in the
            // spawn packet itself (confirmed against `DataWatcher.a`, which
            // iterates every entry unconditionally — unlike the incremental
            // `entity_metadata` packet's dirty-only `DataWatcher.b`), so this
            // is real initial state, not a synthesized default the way
            // 26.2's adapter has to for sheep/creeper.
            let metadata = entity_metadata::fold(Some(kind_name), &body.metadata);
            if !metadata.is_empty() {
                directives.push(Directive::Emit(ClientEvent::EntityMetadataUpdated {
                    entity_id: body.entity_id,
                    metadata,
                }));
            }
            return Ok(directives);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SPAWN_ENTITY`.
    fn handle_play_spawn_entity(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::NAMED_ENTITY_SPAWN`.
    fn handle_play_named_entity_spawn(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::REL_ENTITY_MOVE`.
    fn handle_play_rel_entity_move(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ENTITY_LOOK`.
    fn handle_play_entity_look(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ENTITY_MOVE_LOOK`.
    fn handle_play_entity_move_look(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ENTITY_TELEPORT`.
    fn handle_play_entity_teleport(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ENTITY_VELOCITY`.
    fn handle_play_entity_velocity(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ENTITY_DESTROY`.
    fn handle_play_entity_destroy(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // A varint-counted list of varint ids. Now a derived struct: the
            // `#[mc(varint)]`-on-`Vec<i32>` macro attribute (reported as a gap
            // and since landed) encodes both the length and each element as a
            // varint, replacing the former hand-decoded loop.
            let body: EntityDestroy = decode_body_exact(payload)?;
            self.forget_kinds(&body.entity_ids);
            self.forget_vehicles(&body.entity_ids);
            return Ok(vec![Directive::Emit(ClientEvent::EntityRemoved {
                entity_ids: body.entity_ids,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ENTITY_METADATA`.
    fn handle_play_entity_metadata(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // The incremental metadata packet carries no type of its own —
            // only `spawn_entity_living` names the mob, so a later delta for
            // the same id is gated by whatever `remember_kind` recorded at
            // spawn. An id this adapter never saw spawn (a player, an object,
            // or an entity that spawned before this connection existed) folds
            // with `None`, which only reads the universal Entity/EntityLiving
            // base fields — never a class-specific index it cannot safely
            // gate.
            let body: EntityMetadataPacket = decode_body_exact(payload)?;
            let kind = self.kind_for(body.entity_id);
            let metadata = entity_metadata::fold(kind, &body.metadata);
            if metadata.is_empty() {
                return Ok(vec![]);
            }
            return Ok(vec![Directive::Emit(ClientEvent::EntityMetadataUpdated {
                entity_id: body.entity_id,
                metadata,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::KICK_DISCONNECT`.
    fn handle_play_kick_disconnect(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: KickDisconnect = decode_body(payload)?;
            return Ok(vec![Directive::Disconnect(json_reason_text(&body.reason))]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SET_COMPRESSION`.
    fn handle_play_set_compression(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: PlaySetCompression = decode_body(payload)?;
            return Ok(vec![Directive::SetCompression(body.threshold)]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::UPDATE_HEALTH`.
    fn handle_play_update_health(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // f32 health, varint food, f32 saturation — verified against
            // minecraft-data's 1.8 `packet_update_health` (identical shape at
            // 1.12.2). `UpdateHealth` already existed in this crate but was
            // only ever round-tripped in `tests/join_flow.rs`, never wired
            // into `handle_play` — an island per CLAUDE.md's own definition
            // (decoded nowhere in production, tested only against our own
            // encoder).
            let body: UpdateHealth = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::HealthChanged {
                health: body.health,
                food: body.food,
                saturation: body.food_saturation,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::RESPAWN`.
    fn handle_play_respawn(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Signed int dimension, u8 difficulty, u8 game mode, string level
            // type — verified against minecraft-data's 1.8 `packet_respawn`.
            // Unlike `join`'s dimension (a signed *byte*), 1.8's `respawn`
            // widens dimension to a full `i32` on the wire — a genuine
            // historical inconsistency within 1.8 itself, confirmed against
            // minecraft-data rather than assumed from `join`'s shape; `Respawn`
            // (like `UpdateHealth` above) already existed here but was never
            // wired in. Like `game_mode`'s hardcore bit, the same `dimension_id`
            // helper is reused after narrowing to `i8` (every real dimension
            // value fits). Re-recording the dimension matters for the *next*
            // `map_chunk`: a portal into the nether/end must flip `ChunkShape`
            // before that column's light arrays are decoded, exactly as `LOGIN`
            // does on first join.
            let body: Respawn = decode_body(payload)?;
            let dimension = i8::try_from(body.dimension).map_err(|_| {
                AdapterError::Decode(format!(
                    "respawn dimension {} does not fit in 1.8's byte range",
                    body.dimension
                ))
            })?;
            self.set_dimension(dimension);
            return Ok(vec![Directive::Emit(ClientEvent::Respawned {
                dimension: dimension_id(dimension)?,
                game_mode: game_mode(body.game_mode)?,
                previous_game_mode: None,
                last_death_location: None,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ENTITY_STATUS`.
    fn handle_play_entity_status(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // A raw (non-VarInt) `i32` entity id, then a raw status byte —
            // verified against minecraft-data's 1.8 `packet_entity_status`
            // (identical shape at 1.12.2) and matching `lodestone-v770`'s own
            // `ENTITY_EVENT` decode. Drives hurt/death animation,
            // totem-of-undying particles, etc. — the consumer interprets
            // `status` per the entity's own type, exactly as the modern decode
            // already documents.
            let mut reader = Reader::new(payload);
            let entity_id = reader.i32().map_err(dec_err)?;
            let status = reader.u8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::EntityStatus {
                entity_id,
                status,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ENTITY_HEAD_ROTATION`.
    fn handle_play_entity_head_rotation(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // VarInt entity id, then a packed signed-byte yaw (256 steps per
            // circle, the same packing `unpack_degrees` already handles for
            // body rotation) — verified against minecraft-data's 1.8
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::BLOCK_CHANGE`.
    fn handle_play_block_change(
        &self,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // A packed 1.8 `position` (see `crate::packets::position`, x/y/z
            // big-endian, y in the middle) plus the changed block's legacy
            // composite id as a VarInt — verified both against minecraft-data's
            // 1.8 `packet_block_change` and against a real 1.8.9 server capture
            // in `tests/live_interaction.rs`'s `decode_block_change` (which
            // decodes this exact shape independently, for its own oracle
            // assertions). 1.8's value is pre-Flattening, identical in shape to
            // 1.12.2's: bits `4..` are the numeric block id, the low 4 bits are
            // metadata (`(old_block_id << 4) | meta`, the same composite
            // `chunk.rs` already extracts per paletted section entry).
            // `lodestone_canonical::canonical::resolve_or_air` bridges it to a
            // real 26.2 block-state id via the table built against the real
            // 1.13.2 server jar's own `DataFixerUpper` flattening fix — the
            // same shared table `lodestone-v340` uses, not this crate's own
            // encoder and not a formula (every pre-1.13 family speaks the same
            // `id:meta` space).
            let mut reader = Reader::new(payload);
            let pos: Position = Position::decode(&mut reader, CTX).map_err(dec_err)?;
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
            // vanilla (`LevelChunk.setBlockState`, no packet involved) — the
            // same reasoning `lodestone-v340`'s `BLOCK_CHANGE` arm documents.
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::MULTI_BLOCK_CHANGE`.
    fn handle_play_multi_block_change(
        &self,
        world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Chunk X/Z (i32 each), then a VarInt-counted array of records —
            // verified against minecraft-data's 1.8 `packet_multi_block_change`
            // (identical shape at 1.12.2, where `lodestone-v340`'s own arm
            // documents the same field order). Each record is
            // `horizontalPos: u8` (high nibble relative X, low nibble relative
            // Z — minecraft-data's `protocol.json` gives the field width but
            // not this bit order; sourced from the long-stable external wire
            // documentation for this exact packet, not from our own encoder,
            // and flagged here as the one field in this pass not cross-checked
            // against either the jar or a live capture), `y: u8` (full column
            // height, unlike 26.2's section-relative nibble), then the same
            // legacy composite VarInt `block_change` carries. 1.8 has no
            // sections on the wire — ordinary full-height columns — so one
            // packet's records can span several of `lodestone-world`'s 16-tall
            // sections; each is resolved and written individually, then
            // grouped by section so the emitted `SectionBlocksChanged` events
            // match what a single `block_change` would have produced for the
            // same cell.
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
            // A full-height 1.8 column holds at most 16*16*256 = 65536 cells;
            // cap the pre-allocation so a hostile count cannot force a large
            // speculative allocation before the truncated body is rejected by
            // the per-record reads below.
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
                world.sync_block_entity(x, y, z, block_entity_type(state));
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::OPEN_WINDOW`.
    fn handle_play_open_window(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // `OpenWindow`'s codec already existed and was already tested
            // (`tests/inventory.rs`); nothing here ever called it, so no 1.8
            // container screen — a chest, a furnace, a crafting table —
            // could ever open.
            let body: OpenWindow = decode_body(payload)?;
            let menu_type = resolve_menu_type(&body.inventory_type, body.slot_count);
            return Ok(vec![Directive::Emit(ClientEvent::ScreenOpened {
                window_id: i32::from(body.window_id),
                menu_type,
                title: Text::from_json(&body.window_title),
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::CLOSE_WINDOW`.
    fn handle_play_close_window(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: CloseWindow = decode_body_exact(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::ScreenClosed {
                window_id: i32::from(body.window_id),
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::WINDOW_ITEMS`.
    fn handle_play_window_items(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // 1.8 has no container-synchronization state id (added in a much
            // later version) and does not bundle the cursor item into this
            // packet the way it might elsewhere, so `state_id` is a fixed 0
            // and `carried_item` stays `None` — this packet genuinely does
            // not say.
            let body: WindowItems = decode_body(payload)?;
            let items = body.items.iter().map(slot_to_item_stack).collect();
            return Ok(vec![Directive::Emit(ClientEvent::ContainerContent {
                window_id: i32::from(body.window_id),
                state_id: 0,
                items,
                carried_item: None,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SET_SLOT`.
    fn handle_play_set_slot(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // 1.8 unifies what 26.2 splits into three packets
            // (`SET_CURSOR_ITEM`/`SET_PLAYER_INVENTORY`/`CONTAINER_SET_SLOT`)
            // behind one `window_id` sentinel: `-1` is the cursor (dragged
            // item), `0` is the player's own inventory with no container
            // screen open, anything else is a slot inside that open
            // container — matching exactly the three-way split the canonical
            // model already draws for the modern versions.
            let body: SetSlot = decode_body(payload)?;
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::CRAFT_PROGRESS_BAR`.
    fn handle_play_craft_progress_bar(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // `packet_craft_progress_bar` (minecraft-data 1.8/1.12.2, identical
            // shape): `windowId: u8, property: i16, value: i16` — no
            // synchronization state id (added much later, same absence as
            // `WINDOW_ITEMS` above), so it maps directly onto the same
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::TITLE`.
    fn handle_play_title(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Action-multiplexed, verified field-by-field against
            // minecraft-data's 1.8 `packet_title`: the `text` switch only
            // has cases for actions `0`/`1` (title/subtitle — 1.8 predates
            // the action-bar text case modern versions insert at `2`) and the
            // fade-in/stay/fade-out switch only has a case for action `2`
            // (times), leaving `3`/`4` as the two argument-less actions.
            // Every other version-implementing family in this workspace
            // treats the immediately-following pair as clear-then-reset, and
            // 26.2's own `CLEAR_TITLES` folds that same pair into one packet
            // with a `resetTimes` bool — `3` maps to `false`, `4` to `true`.
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
                    let fade_in = reader.i32().map_err(dec_err)?;
                    let stay = reader.i32().map_err(dec_err)?;
                    let fade_out = reader.i32().map_err(dec_err)?;
                    Directive::Emit(ClientEvent::TitlesAnimation {
                        fade_in,
                        stay,
                        fade_out,
                    })
                }
                3 => Directive::Emit(ClientEvent::TitlesCleared { reset_times: false }),
                4 => Directive::Emit(ClientEvent::TitlesCleared { reset_times: true }),
                other => {
                    return Err(AdapterError::Decode(format!("unknown title action {other}")));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![directive]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::TAB_COMPLETE`.
    fn handle_play_tab_complete(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // `packet_tab_complete` (minecraft-data 1.8/1.12.2, identical
            // shape): a bare `matches: string[]`, no transaction id and no
            // replacement range — 1.8 predates both (added in 1.13). Every
            // match is a complete replacement for the input's last
            // whitespace-delimited word, so the id and range this adapter's
            // own outgoing request remembered (`pending_tab_complete`) are
            // what let `CommandSuggestionsReceived` line up with
            // `SuggestionRequests::receive`'s id check and
            // `ChatInput::apply_suggestions`'s `original[..start] + text`
            // splice — the wire itself says neither.
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::PLAYER_INFO`.
    fn handle_play_player_info(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // A single `action` applies to every entry in the packet
            // (verified against minecraft-data's 1.8 `packet_player_info`
            // `switch`), unlike 26.2's per-entry action bitmask — see
            // `packets::player_info`'s module doc.
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
                    // 1.8 has no separate "listed" bit — every entry the
                    // server sends is, by construction, in the tab list.
                    listed: None,
                    properties: None,
                    // 1.8 predates secure chat sessions entirely.
                    chat_session: None,
                    // 1.8 predates both `UPDATE_LIST_ORDER` and `UPDATE_HAT`
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::HELD_ITEM_SLOT`.
    fn handle_play_held_item_slot(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // A single signed byte, the newly-selected hotbar index — verified
            // against minecraft-data's 1.8 `packet_held_item_slot` (identical
            // shape at every later version through 26.2). The
            // already-defined [`HeldItemSlot`] struct (`packets::window`) was
            // never dispatched from here; this is that decoder's first caller.
            let body: HeldItemSlot = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::HeldSlotChanged {
                slot: i32::from(body.slot),
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ABILITIES`.
    fn handle_play_abilities(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Signed-byte flags (bit 0x01 invulnerable, 0x02 flying, 0x04 can
            // fly, 0x08 instabuild), then f32 flying speed, f32 walking speed
            // — verified against minecraft-data's 1.8 `packet_abilities`.
            // 1.8 reuses one packet *name* for both directions with different
            // flag semantics (the serverbound `abilities` this crate already
            // encodes for `SetFlying` carries only the flying bit); the
            // clientbound shape decoded here is byte-identical, so it is
            // hand-decoded rather than routed through the serverbound-tagged
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SPAWN_POSITION`.
    fn handle_play_spawn_position(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // The already-defined `SpawnPosition` codec (`packets::game`) was
            // never dispatched from here — this is that decoder's first
            // caller, so the compass never pointed anywhere but world spawn
            // for a 1.8 connection.
            let body: SpawnPosition = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
                dimension: dimension_id(self.current_dimension())?,
                pos: body.location.0,
                // 1.8's spawn_position carries no compass angle/pitch — that
                // is a later (recompass) addition.
                angle: 0.0,
                pitch: 0.0,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::DIFFICULTY`.
    fn handle_play_difficulty(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: DifficultyPacket = decode_body(payload)?;
            let difficulty = match body.difficulty {
                0 => Difficulty::Peaceful,
                1 => Difficulty::Easy,
                2 => Difficulty::Normal,
                3 => Difficulty::Hard,
                other => {
                    return Err(AdapterError::Decode(format!("unknown difficulty id {other}")));
                }
            };
            // 1.8 has no "locked" bit — that is a later addition.
            return Ok(vec![Directive::Emit(ClientEvent::DifficultyChanged {
                difficulty,
                locked: false,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::CAMERA`.
    fn handle_play_camera(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: CameraPacket = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::CameraSet {
                entity_id: body.camera_id,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::PLAYERLIST_HEADER`.
    fn handle_play_playerlist_header(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Both fields are JSON chat components: this packet was
            // introduced alongside 1.8's own JSON text component format
            // (unlike the scoreboard/team packets below, which predate it).
            let body: PlayerlistHeader = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::TabListChanged {
                header: Text::from_json(&body.header),
                footer: Text::from_json(&body.footer),
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::EXPERIENCE`.
    fn handle_play_experience(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: Experience = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::ExperienceChanged {
                progress: body.bar,
                level: body.level,
                total: body.total,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ANIMATION`.
    fn handle_play_animation(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Verified against minecraft-data's 1.8 `packet_animation`
            // (identical shape at 1.12.2): varint entity id, raw animation
            // code. `1` has no assigned meaning in either era's client
            // handler and maps to `Other` rather than a named variant.
            let body: Animation = decode_body_exact(payload)?;
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ENTITY_EQUIPMENT`.
    fn handle_play_entity_equipment(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // 1.8 carries exactly one slot per message (unlike some later
            // revisions' batched form), so the emitted `equipment` vec
            // always has a single entry.
            let body: ClientboundEntityEquipment = decode_body_exact(payload)?;
            let slot = legacy_equipment_slot(body.slot)?;
            let item = slot_to_item_stack(&body.item);
            return Ok(vec![Directive::Emit(ClientEvent::EntityEquipmentUpdated {
                entity_id: body.entity_id,
                equipment: vec![EntityEquipment { slot, item }],
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::WORLD_BORDER`.
    fn handle_play_world_border(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Action-multiplexed, verified field-by-field against
            // minecraft-data's 1.8 `packet_world_border` (identical shape
            // at 1.12.2, action `3` "initialize" carrying every field in
            // the order matching `ClientEvent::WorldBorderInitialized`
            // one-for-one).
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::COMBAT_EVENT`.
    fn handle_play_combat_event(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Action-multiplexed, verified field-by-field against
            // minecraft-data's 1.8 `packet_combat_event` (identical shape
            // at 1.12.2). Event `2` (entity died) is only ever sent to the
            // dying player about their own death (vanilla's
            // `EntityPlayerMP`-scoped `CombatTracker`), so `playerId` is
            // always this connection's own entity id and is read-and-discarded
            // exactly as `lodestone-v340` documents for the same packet.
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::OPEN_SIGN_ENTITY`.
    fn handle_play_open_sign_entity(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: OpenSignEntity = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::SignEditorOpened {
                pos: body.location.0,
                // 1.8 has no front/back text distinction — a later (1.20)
                // addition — so this always edits the one text a legacy
                // sign has.
                is_front_text: true,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ATTACH_ENTITY`.
    fn handle_play_attach_entity(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // 1.8 overloads one packet for both leashing and mounting (see
            // `AttachEntity`'s own doc); this is not `lodestone-v340`'s
            // leash-only shape and must not be ported from it verbatim.
            let body: AttachEntity = decode_body_exact(payload)?;
            if body.leash {
                return Ok(vec![Directive::Emit(ClientEvent::EntityLeashed {
                    entity_id: body.entity_id,
                    holder_id: (body.vehicle_id != -1).then_some(body.vehicle_id),
                })]);
            }
            if body.vehicle_id == -1 {
                let Some((vehicle_id, passenger_ids)) = self.dismount(body.entity_id) else {
                    return Ok(Vec::new());
                };
                return Ok(vec![Directive::Emit(ClientEvent::EntityPassengersChanged {
                    vehicle_id,
                    passenger_ids,
                })]);
            }
            let passenger_ids = self.mount(body.vehicle_id, body.entity_id);
            return Ok(vec![Directive::Emit(ClientEvent::EntityPassengersChanged {
                vehicle_id: body.vehicle_id,
                passenger_ids,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::COLLECT`.
    fn handle_play_collect(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // 1.8's `collect` carries no pickup count (see `Collect`'s own
            // doc); `amount` is presently unread by every consumer of
            // `ClientEvent::ItemPickup` (the fly-to-collector overlay counts
            // from the separately-tracked item-stack resource instead), so
            // `1` is a safe, documented placeholder rather than a guess this
            // packet cannot make honest.
            let body: Collect = decode_body_exact(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::ItemPickup {
                item_entity_id: body.collected_entity_id,
                player_id: body.collector_entity_id,
                amount: 1,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::BLOCK_ACTION`.
    fn handle_play_block_action(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: BlockAction = decode_body_exact(payload)?;
            let block_id = u8::try_from(body.block_id).map_err(|_| {
                AdapterError::Decode(format!(
                    "block_action block id {} is outside the legacy 0..=255 block-type space",
                    body.block_id
                ))
            })?;
            return Ok(vec![Directive::Emit(ClientEvent::BlockEvent {
                pos: body.location.0,
                b0: body.byte1,
                b1: body.byte2,
                block: legacy_block_type_key(block_id),
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::BLOCK_BREAK_ANIMATION`.
    fn handle_play_block_break_animation(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: BlockBreakAnimation = decode_body(payload)?;
            let progress = u8::try_from(body.destroy_stage).unwrap_or(0);
            return Ok(vec![Directive::Emit(ClientEvent::BlockDestruction {
                entity_id: body.entity_id,
                pos: body.location.0,
                progress,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::WORLD_EVENT`.
    fn handle_play_world_event(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: WorldEvent = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::LevelEvent {
                event: body.effect_id,
                pos: body.location.0,
                data: body.data,
                global: body.global,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::NAMED_SOUND_EFFECT`.
    fn handle_play_named_sound_effect(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // 1.8 carries no sound category (defaults to `Master`, matching
            // vanilla's pre-category behaviour) and packs pitch as a single
            // byte — see `NamedSoundEffect`'s own doc for the `/63.0`
            // conversion source.
            let body: NamedSoundEffect = decode_body(payload)?;
            let sound: ResourceKey = body.sound_name.parse().map_err(|_| {
                AdapterError::Decode(format!(
                    "named_sound_effect sound name {:?} is not a valid resource key",
                    body.sound_name
                ))
            })?;
            return Ok(vec![Directive::Emit(ClientEvent::Sound {
                sound,
                category: SoundCategory::Master,
                pos: Vec3::new(
                    f64::from(body.x) / 8.0,
                    f64::from(body.y) / 8.0,
                    f64::from(body.z) / 8.0,
                ),
                volume: body.volume,
                pitch: f32::from(body.pitch) / 63.0,
                fixed_range: None,
                seed: 0,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::ENTITY_EFFECT`.
    fn handle_play_entity_effect(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: EntityEffect = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::MobEffectApplied {
                entity_id: body.entity_id,
                effect: legacy_effect_key(body.effect_id)?,
                amplifier: i32::from(body.amplifier),
                duration_ticks: body.duration,
                // 1.8's `hideParticles` is a single bit (see `EntityEffect`'s
                // own doc) — there is no separate ambient bit to read, and
                // vanilla 1.8 always shows the HUD icon.
                ambient: false,
                visible: !body.hide_particles,
                show_icon: true,
                blend: false,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::REMOVE_ENTITY_EFFECT`.
    fn handle_play_remove_entity_effect(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: RemoveEntityEffect = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::MobEffectRemoved {
                entity_id: body.entity_id,
                effect: legacy_effect_key(body.effect_id)?,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SPAWN_ENTITY_WEATHER`.
    fn handle_play_spawn_entity_weather(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: SpawnEntityWeather = decode_body(payload)?;
            let entity_type: ResourceKey = "minecraft:lightning_bolt"
                .parse()
                .map_err(|_| AdapterError::Decode("lightning_bolt key invalid".to_owned()))?;
            return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: body.entity_id,
                uuid: None,
                entity_type,
                pos: Vec3::new(
                    f64::from(body.x) / FIXED_POINT_SCALE,
                    f64::from(body.y) / FIXED_POINT_SCALE,
                    f64::from(body.z) / FIXED_POINT_SCALE,
                ),
                rotation: Rotation::new(0.0, 0.0),
                velocity: None,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SPAWN_ENTITY_EXPERIENCE_ORB`.
    fn handle_play_spawn_entity_experience_orb(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: SpawnEntityExperienceOrb = decode_body(payload)?;
            let entity_type: ResourceKey = "minecraft:experience_orb"
                .parse()
                .map_err(|_| AdapterError::Decode("experience_orb key invalid".to_owned()))?;
            return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: body.entity_id,
                uuid: None,
                entity_type,
                pos: Vec3::new(
                    f64::from(body.x) / FIXED_POINT_SCALE,
                    f64::from(body.y) / FIXED_POINT_SCALE,
                    f64::from(body.z) / FIXED_POINT_SCALE,
                ),
                rotation: Rotation::new(0.0, 0.0),
                velocity: None,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SPAWN_ENTITY_PAINTING`.
    fn handle_play_spawn_entity_painting(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            let body: SpawnEntityPainting = decode_body(payload)?;
            let entity_type: ResourceKey = "minecraft:painting"
                .parse()
                .map_err(|_| AdapterError::Decode("painting key invalid".to_owned()))?;
            let pos = body.location.0;
            return Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
                entity_id: body.entity_id,
                // 1.8's spawn_entity_painting carries no entity UUID (see
                // `SpawnEntityPainting`'s own doc).
                uuid: None,
                entity_type,
                pos: Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)),
                // The legacy motive name and facing direction have no home
                // in the canonical model yet (no legacy motive -> modern
                // `minecraft:painting_variant` crosswalk, and no yaw
                // conversion for the facing byte) — dropped, matching
                // `lodestone-v340`'s treatment of the same gap.
                rotation: Rotation::new(0.0, 0.0),
                velocity: None,
            })]);
            }

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SCOREBOARD_OBJECTIVE`.
    fn handle_play_scoreboard_objective(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Mode-multiplexed (minecraft-data's 1.8 `packet_scoreboard_objective`,
            // identical shape at 1.12.2), so this is a hand-decoded `Reader`
            // walk. `displayText` is a **plain** legacy-formatted string at
            // this protocol revision (JSON scoreboard text is a 1.13+
            // addition), so it goes through `Text::from_legacy`.
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SCOREBOARD_SCORE`.
    fn handle_play_scoreboard_score(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Verified against minecraft-data's 1.8 `packet_scoreboard_score`
            // (identical shape at 1.12.2): `itemName` is the score *holder*
            // and `scoreName` is the *objective* — the mcdata field names are
            // misleading, not the wire order. `scoreName` is read
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SCOREBOARD_DISPLAY_OBJECTIVE`.
    fn handle_play_scoreboard_display_objective(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Verified against minecraft-data's 1.8
            // `packet_scoreboard_display_objective` (identical shape at
            // 1.12.2): raw `i8` slot position, then a string objective name.
            // This protocol revision only ever sends 0/1/2 — the
            // per-team-colour sidebar slots are a later addition — and
            // clears the slot with an empty string rather than a dedicated
            // marker.
            let mut reader = Reader::new(payload);
            let position = reader.i8().map_err(dec_err)?;
            let name = reader.string(16).map_err(dec_err)?;
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

    /// Extracted from the former if-chain arm for
    /// `play::clientbound::SCOREBOARD_TEAM`.
    fn handle_play_scoreboard_team(
        &self,
        _world: &mut dyn WorldSink,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {

            // Mode-multiplexed (minecraft-data's 1.8 `packet_scoreboard_team`),
            // so this is a hand-decoded `Reader` walk. **1.8 carries no
            // collision-rule field** (a 1.9 addition `lodestone-v340`'s
            // sibling packet does have) — `CollisionRule::Always` documents
            // the pre-collision-rule vanilla behaviour (everyone always
            // pushes) rather than guessing a value this wire cannot supply.
            // `friendlyFire` packs two flags in one byte (`0x01` friendly
            // fire, `0x02` see friendly invisibles), a convention unchanged
            // since 1.8.
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
                    let display_name = reader.string(16).map_err(dec_err)?;
                    let prefix = reader.string(16).map_err(dec_err)?;
                    let suffix = reader.string(16).map_err(dec_err)?;
                    let friendly_flags = reader.i8().map_err(dec_err)?;
                    let visibility_str = reader.string(32).map_err(dec_err)?;
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
                    let params = Box::new(TeamParameters {
                        display_name: Text::from_legacy(&display_name),
                        prefix: Text::from_legacy(&prefix),
                        suffix: Text::from_legacy(&suffix),
                        name_tag_visibility,
                        collision_rule: CollisionRule::Always,
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
                    return Err(AdapterError::Decode(format!("unknown scoreboard_team mode {other}")));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TeamUpdate {
                name: team,
                action,
            })]);
            }

    /// Handles a clientbound packet while in the play state. Looks up
    /// `packet_id` in [`Self::play_dispatch_table`] and runs the bound
    /// handler -- every arm this used to be an if-chain over is now a plain
    /// fn pointer in [`CLIENTBOUND`], since no arm in this family needed to
    /// capture anything beyond `&self`, `world` and `payload`.
    fn handle_play(
        &self,
        world: &mut dyn WorldSink,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let table = Self::play_dispatch_table();
        match table.get(packet_id) {
            Some(handler) => handler(self, world, payload),
            None => unreachable!(
                "Table::build already rejected any play::clientbound id with neither \
                 a handler nor an IGNORED entry"
            ),
        }
    }

    /// Builds this family's `play::clientbound` dispatch table from the
    /// generated `(name, id)` table, [`CLIENTBOUND`]'s handler bindings and
    /// [`IGNORED`]'s declared-unhandled list. Rebuilt on every call (a
    /// `BTreeMap` over ~74 entries) rather than cached in a `OnceLock`: this
    /// family constructs one `V47Adapter` per connection and `handle_play` is
    /// not the hot per-tick path (chunk/entity streaming dominates), so the
    /// mechanical choice is the one with no interior-mutability bookkeeping to
    /// get wrong. `.expect(...)` here is the correct failure mode for a
    /// genuinely malformed static table -- exactly what `Table::build`'s
    /// construction-time checks exist to catch; `tests/dispatch_coverage.rs`
    /// carries the standing proof that it succeeds, plus a negative control
    /// proving it can fail.
    fn play_dispatch_table() -> lodestone_core::dispatch::Table<'static, PlayHandlerFn> {
        lodestone_core::dispatch::Table::build(
            PROTOCOL,
            play::clientbound::ENTRIES,
            CLIENTBOUND,
            IGNORED,
        )
        .expect("v47 play::clientbound dispatch table must be internally consistent")
    }

}

/// Payload every `play::clientbound` [`lodestone_core::dispatch::Handler`]
/// below runs: a plain fn pointer, since every extracted arm closes only over
/// `&self`, `world` and `payload` -- no arm in this family needed a captured
/// closure or an enum fallback. `pub` (rather than `pub(crate)`) only because
/// [`CLIENTBOUND`]'s element type names it and a public static may not use a
/// less-visible type in its signature.
pub type PlayHandlerFn =
    fn(&V47Adapter, &mut dyn WorldSink, &[u8]) -> Result<Vec<Directive>, AdapterError>;

/// Every `play::clientbound` packet this family translates, keyed by its
/// canonical (minecraft-data) name -- the same spelling
/// `crate::packet_ids::play::clientbound::ENTRIES` uses for this protocol.
/// [`V47Adapter::play_dispatch_table`] builds the runtime dispatch table from
/// this slice plus [`IGNORED`]; `Table::build` fails construction if a name
/// here has no matching id in `ENTRIES` -- see `dispatch.rs`. `pub` so
/// `tests/dispatch_coverage.rs` can rebuild (and deliberately corrupt) this
/// same table from outside the crate.
pub static CLIENTBOUND: &[(&str, lodestone_core::dispatch::Handler<PlayHandlerFn>)] = &[
    (
        "minecraft:login",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_login as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:map_chunk",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_map_chunk as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:map_chunk_bulk",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_map_chunk_bulk as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:keep_alive",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_keep_alive as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:chat",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_chat as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_position as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_entity_living",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_spawn_entity_living as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_spawn_entity as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:named_entity_spawn",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_named_entity_spawn as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:rel_entity_move",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_rel_entity_move as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_look",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_entity_look as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_move_look",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_entity_move_look as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_teleport",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_entity_teleport as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_velocity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_entity_velocity as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_destroy",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_entity_destroy as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_metadata",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_entity_metadata as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:kick_disconnect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_kick_disconnect as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:set_compression",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_set_compression as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:update_health",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_update_health as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:respawn",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_respawn as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_status",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_entity_status as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_head_rotation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_entity_head_rotation as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:block_change",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_block_change as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:multi_block_change",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_multi_block_change as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:open_window",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_open_window as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:close_window",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_close_window as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:window_items",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_window_items as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:set_slot",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_set_slot as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:craft_progress_bar",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_craft_progress_bar as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:title",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_title as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:tab_complete",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_tab_complete as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:player_info",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_player_info as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:held_item_slot",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_held_item_slot as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:abilities",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_abilities as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_position",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_spawn_position as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:difficulty",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_difficulty as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:camera",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_camera as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:playerlist_header",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_playerlist_header as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:experience",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_experience as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:animation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_animation as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_equipment",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_entity_equipment as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:world_border",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_world_border as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:combat_event",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_combat_event as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:open_sign_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_open_sign_entity as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:attach_entity",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_attach_entity as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:collect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_collect as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:block_action",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_block_action as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:block_break_animation",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_block_break_animation as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:world_event",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_world_event as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:named_sound_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_named_sound_effect as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:entity_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_entity_effect as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:remove_entity_effect",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_remove_entity_effect as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_entity_weather",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_spawn_entity_weather as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_entity_experience_orb",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_spawn_entity_experience_orb as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:spawn_entity_painting",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_spawn_entity_painting as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:scoreboard_objective",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_scoreboard_objective as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:scoreboard_score",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_scoreboard_score as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:scoreboard_display_objective",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_scoreboard_display_objective as PlayHandlerFn,
        ),
    ),
    (
        "minecraft:scoreboard_team",
        lodestone_core::dispatch::Handler::new(
            lodestone_core::ProtocolRange::ALL,
            V47Adapter::handle_play_scoreboard_team as PlayHandlerFn,
        ),
    ),
];

/// Every `play::clientbound` packet id with deliberately no handler above,
/// each with its own re-derived reason. `tests/dispatch_coverage.rs` proves
/// this list plus [`CLIENTBOUND`] together account for every id
/// `play::clientbound::ENTRIES` declares, and that removing an entry breaks
/// construction.
pub static IGNORED: &[lodestone_core::dispatch::IGNORED] = &[
    lodestone_core::dispatch::IGNORED::new("minecraft:update_time", "v770 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:bed", "removed from the wire after protocol 340 (1.12.2); vanilla folds sleeping state into entity metadata (a Pose value) from 1.14 onward, so there is no v770 clientbound packet to backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:entity", "entity-tracker no-op heartbeat: minecraft-data's own schema is a bare entityId with no other field, and no protocol family in this workspace (v340, v735, v770) translates it either"),
    lodestone_core::dispatch::IGNORED::new("minecraft:update_attributes", "v770 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:explosion", "v770 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:world_particles", "v770 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:game_state_change", "v770 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:transaction", "removed from the wire after protocol 754 (1.16.5, still present in v735); v770 has no clientbound-or-serverbound transaction-ack packet at all, so there is nothing to backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:update_sign", "the clientbound arm was removed after protocol 754 (v340 and v735 both carry it serverbound-only); modern sign text travels through block-entity NBT instead, so there is no v770 clientbound packet to backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:map", "v770 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:tile_entity_data", "v770 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:statistics", "v770 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:custom_payload", "v770 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:resource_pack_send", "v770 has this; backport"),
    lodestone_core::dispatch::IGNORED::new("minecraft:update_entity_nbt", "a debug-only packet (entity id plus its raw NBT tag, per minecraft-data's 1.8 schema); no consumer exists in the canonical model and no later protocol family in this workspace carries an equivalent"),
];

impl VersionAdapter for V47Adapter {
    fn protocol_version(&self) -> i32 {
        PROTOCOL
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["1.8.8", "1.8.9"]
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
            } => self.select_move_packet(*pos, *rotation, *on_ground),
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
            // Newly modelled actions that 1.8 genuinely carries. Encoded
            // faithfully against the minecraft-data wire shapes.
            ClientAction::SetClientSettings(settings) => {
                let ClientSettings {
                    locale,
                    view_distance,
                    chat_mode,
                    chat_colors,
                    skin_parts,
                    // 1.8 predates these fields; dropped deliberately.
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
                    skin_parts: skin_parts_bits(*skin_parts),
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

            // Container clicks predate the modern `state_id` reconciliation.
            // Faithfully encoding 1.8's `window_click` requires three things the
            // current model/architecture cannot supply, so it is refused loudly
            // rather than encoded with wrong bytes (which a live server rejects
            // via a failed transaction, silently dropping the click):
            //   1. a client-tracked transaction id (the `action` counter) plus
            //      the `confirm_transaction` ack loop — the model carries only
            //      the 1.17+ `state_id`, and this adapter now tracks other
            //      per-connection state (`pending_tab_complete`) but not this;
            //   2. an item registry (`ResourceKey` -> numeric id) to encode the
            //      clicked stack, which no version crate has yet;
            //   3. item metadata/damage, which pre-1.13 slots carry but the
            //      model's `ItemStack { item, count }` cannot express.
            //
            // This is also why the clientbound `TRANSACTION` packet (id 0x32)
            // has no decode arm: it exists solely to accept or reject a
            // `window_click` this client cannot yet send, so nothing here
            // could ever receive one. Wiring a decode for it now would be an
            // event with no producer that could trigger it — inventing a
            // consumer for a packet a real server never sends us is the wrong
            // side of that trade. It becomes real work once `ContainerClick`
            // above is, not before.
            ClientAction::ContainerClick { .. } => Err(AdapterError::Unsupported(
                "protocol 47 ContainerClick needs a client-tracked transaction id (model carries \
                 only the 1.17+ state_id), an item registry, and item metadata the model's \
                 ItemStack cannot express"
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

            // Actions that predate 1.8 wire support or need model carriers 1.8
            // lacks. Rejected loudly rather than silently dropped.
            ClientAction::PongResponse { .. } => Err(AdapterError::Unsupported(
                "protocol 47 has no configuration/play ping-pong handshake".to_owned(),
            )),
            ClientAction::ResourcePackResponse { .. } => Err(AdapterError::Unsupported(
                "protocol 47 resource_pack_receive carries a pack hash string that the model's \
                 Uuid-keyed ResourcePackResponse cannot supply"
                    .to_owned(),
            )),
            ClientAction::EndClientTick => Err(AdapterError::Unsupported(
                "protocol 47 has no client_tick_end packet".to_owned(),
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
            ClientAction::CommandSuggestion { id, command } => {
                // `packet_tab_complete` (minecraft-data 1.8): `text: string,
                // block: option<position>`. This client never tracks a
                // looked-at block, so the option is always absent; `id` has
                // nowhere to go on the wire and is remembered instead (see
                // `pending_tab_complete`).
                self.remember_tab_complete(*id, command.clone());
                let mut writer = Writer::default();
                writer.string(command);
                writer.bool(false);
                Ok(Some((play::serverbound::TAB_COMPLETE, writer.into_vec())))
            }
            ClientAction::PaddleBoat { .. } => Err(AdapterError::Unsupported(
                "protocol 47 paddle boat encoding is not yet implemented".to_owned(),
            )),
            ClientAction::MoveVehicle { .. } => Err(AdapterError::Unsupported(
                "protocol 47 move vehicle encoding is not yet implemented".to_owned(),
            )),

            // Leaving the death screen. `client_command` action `0` =
            // perform respawn, a stable ordinal across every generation
            // checked (1.8, 1.12.2, 1.16.2/.4/.5 all encode it as a lone
            // varint action id per minecraft-data's protocol.json).
            ClientAction::Respawn => {
                let body = ClientCommand { action: 0 };
                Ok(Some((play::serverbound::CLIENT_COMMAND, encode_body(&body)?)))
            }
            // Clicking a name in the tab list while spectating. 1.8's
            // `spectate` packet carries the target's uuid directly, which the
            // model already supplies, so no entity registry is needed.
            ClientAction::TeleportToEntity { target } => {
                let body = Spectate { target: *target };
                Ok(Some((play::serverbound::SPECTATE, encode_body(&body)?)))
            }
            // The continuous spectator-follow action carries only a network
            // entity id, but 1.8's wire packet is the same uuid-keyed
            // `spectate` packet as `TeleportToEntity` above. A stateless
            // adapter has no id->uuid registry to bridge the two.
            ClientAction::SpectatorAction { .. } => Err(AdapterError::Unsupported(
                "protocol 47's spectate packet needs a target uuid; SpectatorAction carries \
                 only a network entity id with no registry to resolve it into one (use \
                 TeleportToEntity instead, which already carries the uuid)"
                    .to_owned(),
            )),
            ClientAction::ChatAck { .. } => Err(AdapterError::Unsupported(
                "protocol 47 predates signed/acknowledged chat (added in 1.19)".to_owned(),
            )),
            ClientAction::SelectBundleItem { .. } => Err(AdapterError::Unsupported(
                "protocol 47 predates bundles (added in 1.21.2)".to_owned(),
            )),
            ClientAction::SetContainerSlotState { .. } => Err(AdapterError::Unsupported(
                "protocol 47 predates the crafter block (added in 1.21)".to_owned(),
            )),
            ClientAction::SetRecipeBookSettings { .. }
            | ClientAction::RecipeBookSeenRecipe { .. }
            | ClientAction::PlaceRecipe { .. } => Err(AdapterError::Unsupported(
                "protocol 47 predates the recipe book (added in 1.12)".to_owned(),
            )),
            ClientAction::PingRequest { .. } => Err(AdapterError::Unsupported(
                "protocol 47 has no play-state ping request packet".to_owned(),
            )),
            ClientAction::ChangeGameMode { .. } => Err(AdapterError::Unsupported(
                "protocol 47 has no dedicated change_game_mode packet; a debug-menu game-mode \
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
        let adapter = V47Adapter::new();
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
