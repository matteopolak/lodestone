//! [`VersionAdapter`] implementation driving the protocol 340 join flow.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_data::block_entity_types::block_entity_type;
use lodestone_data::block_states;
use lodestone_data::mob_effects::mob_effect_name;
use lodestone_model::{
    AdapterError, AnimationAction, BlockActionKind, BlockFace, BossAction, BossColor, BossOverlay,
    ChatKind, ChatMode, ChunkPos, ClientAction, ClientEvent, ClientSettings, CollisionRule,
    ConnectionState, Difficulty, Directive, DisplaySlot, DisplayedSkinParts, EntityEquipment,
    EntityInteraction, EntityMovement, EquipmentSlot, GameMode, Hand, ItemStack, LoginProfile,
    MainHand, ObjectiveMode, ObjectiveRenderType, ParticleOptions, PlayerCommand, PlayerListEntry,
    ProfileProperty, RecipeBookType, ResourceKey, ResourcePackResponseKind, Rotation, SectionPos,
    ServerAddress, SoundCategory, TeamAction, TeamColor, TeamParameters, TeleportFlags, Text,
    Vec3, Vec3f, VersionAdapter, Visibility, WorldSink,
};
use lodestone_world::{ChunkPos as WorldChunkPos, Heightmaps, LoadedChunk};

use crate::canonical::{self, FallbackTally};
use crate::entity_types;
use crate::item_types;
use crate::particle_ids;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::chunk::{ChunkShape, MapChunk, UnloadChunk};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse};
use crate::packets::entity::{
    EntityDestroy, EntityLook, EntityMoveLook, EntityTeleport, EntityVelocityPacket,
    NamedEntitySpawn, RelEntityMove, SpawnEntityExperienceOrb, SpawnEntityLiving,
    SpawnEntityWeather, SpawnObject,
};
use crate::packets::game::{
    Animation, AttachEntity, BlockAction, BlockDig, BlockPlace, ClientCommand, ClientboundChat,
    ClientboundEntityEquipment, ClientboundPositionLook, Collect, DifficultyPacket, EntityAction,
    EntityEffect, JoinGame, KickDisconnect, NamedSoundEffect, PlayerlistHeader,
    RemoveEntityEffect, Respawn, ScoreboardDisplayObjective, ServerboundArmAnimation,
    ServerboundChat, ServerboundPositionLook, SetPassengers, SoundEffect, Spectate,
    SpawnPosition, TeleportConfirm, UpdateHealth, UpdateTime, UseEntity, UseEntityAt,
    UseEntityInteract,
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

/// Protocol version implemented by this adapter.
pub const PROTOCOL: i32 = 340;

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
    /// Raw 1.12.2 dimension id from the most recent `login`/`respawn`, so a
    /// packet that identifies its dimension only implicitly (e.g.
    /// `spawn_position`, which has no dimension field of its own) can still
    /// build a [`lodestone_model::DimensionId`]. Defaults to `0` (overworld),
    /// matching `shape`'s own default.
    current_dimension: Arc<Mutex<i32>>,
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
            current_dimension: Arc::new(Mutex::new(0)),
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
    V340Adapter::new()
}

/// Encodes a packet body into a fresh byte buffer.
///
/// Thin wrapper over the version-free [`lodestone_core::encode_body`], which
/// returns a stringified error because `AdapterError` lives in
/// `lodestone-model` and `lodestone-core` cannot depend on it.
fn encode_body<T: Encode>(packet: &T) -> Result<Vec<u8>, AdapterError> {
    lodestone_core::encode_body(packet, CTX).map_err(AdapterError::Encode)
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

/// Decodes a packet body from raw bytes.
fn decode_body<T: Decode>(payload: &[u8]) -> Result<T, AdapterError> {
    lodestone_core::decode_body(payload, CTX).map_err(AdapterError::Decode)
}

/// Maps a decode error to the adapter's decode-error variant. Used by the
/// hand-decoded arms (`block_change`/`multi_block_change`/`entity_status`/
/// `entity_head_rotation`) that read a [`Reader`] directly rather than going
/// through a derived [`Decode`] body, mirroring `lodestone-v770`'s own
/// `dec_err` helper.
fn dec_err(err: impl std::fmt::Display) -> AdapterError {
    AdapterError::Decode(err.to_string())
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
/// Mirrors `lodestone-v47`'s identically-named helper: an empty slot or an id
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
/// Identical mapping to `lodestone-v47`'s `resolve_menu_type` — 1.12.2's
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
/// either horizontal axis: `WorldBorder.absoluteMaxSize` (`WorldBorder.java`)
/// is 29,999,984, and the border is what bounds every world regardless of the
/// `worldborder` command or the world's own settings. Anything past this is not
/// an awkward-but-real position, it is invalid input.
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
/// the same formula and is used identically by v47 and v735.
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
                // 1.12's chat packet carries no sender field — nothing to filter on.
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
        if packet_id == play::clientbound::UPDATE_HEALTH {
            // f32 health, varint food, f32 saturation — verified against
            // minecraft-data's 1.12.2 `packet_update_health` (identical to 1.8's
            // own shape). `UpdateHealth` already existed in this crate but was
            // only ever round-tripped in `tests/join_flow.rs`, never wired into
            // `handle_play` — an island per CLAUDE.md's own definition (decoded
            // nowhere in production, tested only against our own encoder).
            let body: UpdateHealth = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::HealthChanged {
                health: body.health,
                food: body.food,
                saturation: body.food_saturation,
            })]);
        }
        if packet_id == play::clientbound::RESPAWN {
            // Signed int dimension, u8 difficulty, u8 game mode, string level
            // type — verified against minecraft-data's 1.12.2
            // `packet_respawn`. Like `join`'s `dimension`, `respawn`'s
            // `game_mode` packs the hardcore flag in bit `0x8`; reusing the
            // same `game_mode` helper masks it off identically. The dimension
            // shape re-recorded here matters for the *next* `map_chunk`: a
            // portal into the nether/end must flip `ChunkShape` before that
            // column's light arrays are decoded, exactly as `LOGIN` does on
            // first join.
            let body: Respawn = decode_body(payload)?;
            self.set_dimension(body.dimension);
            return Ok(vec![Directive::Emit(ClientEvent::Respawned {
                dimension: dimension_id(body.dimension)?,
                game_mode: game_mode(body.game_mode)?,
                previous_game_mode: None,
                last_death_location: None,
            })]);
        }
        if packet_id == play::clientbound::ENTITY_STATUS {
            // A raw (non-VarInt) `i32` entity id, then a raw status byte —
            // verified against minecraft-data's 1.12.2 `packet_entity_status`
            // (identical to 1.8's shape) and matching `lodestone-v770`'s own
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
        if packet_id == play::clientbound::ENTITY_HEAD_ROTATION {
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
        if packet_id == play::clientbound::BLOCK_CHANGE {
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
            // same reasoning `lodestone-v770`'s `BLOCK_UPDATE` arm
            // documents.
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
        if packet_id == play::clientbound::MULTI_BLOCK_CHANGE {
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
        if packet_id == play::clientbound::OPEN_WINDOW {
            // `OpenWindow`'s codec already existed and was already tested
            // (`tests/inventory.rs`, wire round trips only); nothing here
            // ever called it, so no 1.12.2 container screen — a chest, a
            // furnace, a crafting table — could ever open.
            let body: OpenWindow = decode_body(payload)?;
            let menu_type = resolve_menu_type(&body.inventory_type, body.slot_count);
            return Ok(vec![Directive::Emit(ClientEvent::ScreenOpened {
                window_id: i32::from(body.window_id),
                menu_type,
                title: Text::from_json(&body.window_title),
            })]);
        }
        if packet_id == play::clientbound::CLOSE_WINDOW {
            let body: CloseWindow = decode_body_exact(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::ScreenClosed {
                window_id: i32::from(body.window_id),
            })]);
        }
        if packet_id == play::clientbound::WINDOW_ITEMS {
            // 1.12.2 has no container-synchronization state id (added in a
            // much later version) and does not bundle the cursor item into
            // this packet the way it might elsewhere, so `state_id` is a
            // fixed 0 and `carried_item` stays `None` — this packet
            // genuinely does not say.
            let body: WindowItems = decode_body(payload)?;
            let items = body.items.iter().map(slot_to_item_stack).collect();
            return Ok(vec![Directive::Emit(ClientEvent::ContainerContent {
                window_id: i32::from(body.window_id),
                state_id: 0,
                items,
                carried_item: None,
            })]);
        }
        if packet_id == play::clientbound::SET_SLOT {
            // 1.12.2 unifies what 26.2 splits into three packets
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
        if packet_id == play::clientbound::PLAYER_INFO {
            // A single `action` applies to every entry in the packet —
            // verified against minecraft-data's 1.12.2 `packet_player_info`
            // `switch`, byte-identical to 1.8's shape, unlike 26.2's
            // per-entry action bitmask. See `packets::player_info`'s module
            // doc.
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
                    // 1.12.2 has no separate "listed" bit — every entry the
                    // server sends is, by construction, in the tab list.
                    listed: None,
                    properties: None,
                    // 1.12.2 predates secure chat sessions entirely.
                    chat_session: None,
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
        if packet_id == play::clientbound::HELD_ITEM_SLOT {
            // A single signed byte, the newly-selected hotbar index —
            // verified against minecraft-data's 1.12.2
            // `packet_held_item_slot` (identical shape at every later
            // version through 26.2). The already-defined [`HeldItemSlot`]
            // struct (`packets::window`) was never dispatched from here;
            // this is that decoder's first caller.
            let body: HeldItemSlot = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::HeldSlotChanged {
                slot: i32::from(body.slot),
            })]);
        }
        if packet_id == play::clientbound::ABILITIES {
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
        if packet_id == play::clientbound::BLOCK_ACTION {
            // Packed position, two opaque bytes, then a varint legacy block
            // *type* id — verified against minecraft-data's 1.12.2
            // `packet_block_action` (identical to 1.8's shape). Without this,
            // no note block ever plays, no piston ever animates, and no
            // chest lid ever opens for a 1.12.2 connection: those are all
            // this packet, not `block_change`.
            let body: BlockAction = decode_body_exact(payload)?;
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
            // measured (`lodestone_v340::canonical::resolve`, a debug probe
            // over every meta) that a legacy chest/ender_chest/trapped_chest
            // id has **no** entry at meta `0` or `1` at all — those metas
            // were never a real chest orientation, only `2..=5` (facing)
            // were — so a fixed `meta = 0` would silently resolve every
            // chest-lid `block_action` to air. Scanning every meta and
            // taking the first `Resolved` slot is family-only-safe (any
            // meta the table does populate names the same block) and
            // correct for every id this packet has been observed to carry.
            let state = (0u8..16)
                .find_map(|meta| match canonical::resolve(block_id, meta) {
                    canonical::CanonicalBlockState::Resolved(state) => Some(state),
                    _ => None,
                })
                .unwrap_or_else(canonical::air_state_id);
            let key: ResourceKey = block_states::block_name(state)
                .unwrap_or("minecraft:air")
                .parse()
                .map_err(|_| {
                    AdapterError::Decode(format!(
                        "resolved block name for legacy block_action id {block_id} is not a \
                         valid resource key"
                    ))
                })?;
            return Ok(vec![Directive::Emit(ClientEvent::BlockEvent {
                pos: body.location.0,
                b0: body.byte1,
                b1: body.byte2,
                block: key,
            })]);
        }
        if packet_id == play::clientbound::ENTITY_EQUIPMENT {
            // Verified against minecraft-data's 1.12.2
            // `packet_entity_equipment`: a varint entity id, a varint
            // `EquipmentSlot` ordinal, then a `slot` item stack. Unlike the
            // modern packet this carries exactly one slot per message, so
            // the emitted `equipment` vec always has a single entry.
            let body: ClientboundEntityEquipment = decode_body_exact(payload)?;
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
        if packet_id == play::clientbound::ANIMATION {
            // Verified against minecraft-data's 1.12.2 `packet_animation`: a
            // varint entity id, then a raw animation code byte. See
            // `Animation`'s own doc for the code table and why `1` maps to
            // `Other` rather than a named variant.
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
        if packet_id == play::clientbound::NAMED_SOUND_EFFECT {
            // Verified against minecraft-data's 1.12.2
            // `packet_named_sound_effect`. `x`/`y`/`z` are vanilla's
            // fixed-point sound-position convention (real coordinate × 8);
            // this era carries no fixed audible range and no variant seed,
            // so both canonical fields are the "not present" default.
            let body: NamedSoundEffect = decode_body_exact(payload)?;
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
        if packet_id == play::clientbound::SOUND_EFFECT {
            // Identical shape to `NAMED_SOUND_EFFECT` except the leading
            // field is a varint `SoundEvent` registry id rather than a
            // string name — resolved through the generated legacy
            // `sound_ids` table (`vendor/minecraft-data`'s
            // `pc/1.12.2/sounds.json`, wire-order network ids).
            let body: SoundEffect = decode_body_exact(payload)?;
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
        if packet_id == play::clientbound::SCOREBOARD_DISPLAY_OBJECTIVE {
            // Verified against minecraft-data's 1.12.2
            // `packet_scoreboard_display_objective`: a raw `i8` slot
            // position, then a string objective name. This protocol
            // revision only ever sends 0/1/2 — the per-team-colour sidebar
            // slots are a later addition — and clears the slot with an
            // empty string rather than a dedicated marker.
            let body: ScoreboardDisplayObjective = decode_body_exact(payload)?;
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
        if packet_id == play::clientbound::SCOREBOARD_OBJECTIVE {
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
        if packet_id == play::clientbound::SCOREBOARD_SCORE {
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
        if packet_id == play::clientbound::TEAMS {
            // Mode-multiplexed (minecraft-data's 1.12.2 `packet_teams`), so
            // this is a hand-decoded `Reader` walk. Modes `0`
            // (create) and `2` (update) share the full parameter block;
            // `0` additionally carries the initial member list, and `3`/`4`
            // (add/remove members) carry only a member list. `friendlyFire`
            // packs two flags in one byte (`0x01` friendly fire, `0x02` see
            // friendly invisibles) — vanilla's `PacketPlayOutScoreboardTeam`
            // convention. The member-list count is capped against the
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
        if packet_id == play::clientbound::BOSS_BAR {
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
        if packet_id == play::clientbound::SPAWN_POSITION {
            let body: SpawnPosition = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
                dimension: dimension_id(self.current_dimension())?,
                pos: body.location.0,
                angle: 0.0,
                pitch: 0.0,
            })]);
        }
        if packet_id == play::clientbound::UPDATE_TIME {
            let body: UpdateTime = decode_body(payload)?;
            return Ok(vec![Directive::Emit(ClientEvent::TimeChanged {
                world_age: body.age,
                time_of_day: body.time,
            })]);
        }
        if packet_id == play::clientbound::DIFFICULTY {
            let body: DifficultyPacket = decode_body(payload)?;
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
            // 1.12.2's legacy effect id is 1-based; the shared
            // `lodestone-data` registry table is the 0-based modern
            // `minecraft:mob_effect` network id, and the two id spaces have
            // been stable in the same relative order since Minecraft
            // Beta 1.8 — verified entry-for-entry against
            // `vendor/minecraft-data`'s `data/pc/1.12/effects.json` (ids
            // `1..=27`) against `generated/mob_effects.rs`'s `MOB_EFFECT_NAMES`
            // (indices `0..=26`).
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
                // Neither bit exists at this protocol revision: vanilla
                // 1.12.2 always shows the HUD icon and never blends.
                show_icon: true,
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
        if packet_id == play::clientbound::SPAWN_ENTITY_WEATHER {
            let body: SpawnEntityWeather = decode_body(payload)?;
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
        if packet_id == play::clientbound::WORLD_PARTICLES {
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
            // as `lodestone-v770` does for any particle name it does not
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
                pos: Vec3::new(f64::from(x), f64::from(y), f64::from(z)),
                offset: Vec3f::new(offset_x, offset_y, offset_z),
                max_speed,
                count,
                options: ParticleOptions::None,
            })]);
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

            // Leaving the death screen. `client_command` action `0` =
            // perform respawn, a stable ordinal across every generation
            // checked (1.8, 1.12.2, 1.16.2/.4/.5 all encode it as a lone
            // varint action id per minecraft-data's protocol.json).
            ClientAction::Respawn => {
                let body = ClientCommand { action: 0 };
                Ok(Some((play::serverbound::CLIENT_COMMAND, encode_body(&body)?)))
            }
            // Clicking a name in the tab list while spectating. 1.12.2's
            // `spectate` packet carries the target's uuid directly, which the
            // model already supplies, so no entity registry is needed.
            ClientAction::TeleportToEntity { target } => {
                let body = Spectate { target: *target };
                Ok(Some((play::serverbound::SPECTATE, encode_body(&body)?)))
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
                Ok(Some((
                    play::serverbound::CRAFTING_BOOK_DATA,
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
