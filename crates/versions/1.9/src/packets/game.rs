//! Play-state packets for protocol 340.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use crate::packets::position::Position;
use crate::packets::slot::Slot;

/// Clientbound `login` (game-join) packet.
///
/// # Architectural notes
///
/// * `dimension` is a **numeric signed byte** (`-1` nether, `0` overworld, `1`
///   end), not the modern namespaced dimension identifier string. The adapter
///   maps this byte onto the canonical `DimensionId` identifier.
/// * `game_mode` is a `u8` whose `0x8` bit flags a hardcore world; the low two
///   bits carry the mode. There is no separate hardcore boolean as in modern
///   join.
///
/// Wire layout: signed int entity id, unsigned byte game mode (bit `0x8` =
/// hardcore), signed byte dimension, unsigned byte difficulty, unsigned byte
/// max players, string level type, boolean reduced debug info.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login", state = Play, bound = Client)]
pub struct JoinGame {
    /// Local player entity id.
    pub entity_id: i32,
    /// Packed game mode: low two bits are the mode, `0x8` marks hardcore.
    pub game_mode: u8,
    /// Numeric dimension (`-1` nether, `0` overworld, `1` end). Widened from a
    /// signed byte in 1.8 to a full `i32` in 1.9+ (protocol 340 is 1.12.2).
    pub dimension: i32,
    /// World difficulty (`0` peaceful .. `3` hard).
    pub difficulty: u8,
    /// Maximum player count (legacy hint, unused by the client).
    pub max_players: u8,
    /// Level type string, such as `default` or `flat`.
    #[mc(max = 16)]
    pub level_type: String,
    /// Whether reduced debug info is in effect.
    pub reduced_debug_info: bool,
}

// `ClientboundChat` is byte-identical to v1-8's (measured), shared via
// `lodestone-protocol-common` ranged 47..=340 -- v1-14 (1.16) added a
// `sender: Uuid` field, so it is not in this range.
pub use lodestone_protocol_common::packets::chat::ClientboundChat;

// `ServerboundChat` is byte-identical to v1-14's (measured), shared via
// `lodestone-protocol-common` ranged 340..=754 -- v1-8/1.8 capped the message
// at 100 characters, not 256. See `packets::chat`'s module docs.
pub use lodestone_protocol_common::packets::chat::ServerboundChat;

/// Clientbound `position` (player position and look) packet.
///
/// # Architectural note
///
/// 1.8 introduced the relative-teleport `flags` byte, so every coordinate can
/// be absolute or relative. Bit `0x01` x, `0x02` y, `0x04` z, `0x08` yaw,
/// `0x10` pitch are relative when set.
///
/// Wire layout: f64 x/y/z, f32 yaw/pitch, signed byte flags.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:position", state = Play, bound = Client)]
pub struct ClientboundPositionLook {
    /// X coordinate (absolute or relative per `flags`).
    pub x: f64,
    /// Y coordinate (absolute or relative per `flags`).
    pub y: f64,
    /// Z coordinate (absolute or relative per `flags`).
    pub z: f64,
    /// Yaw in degrees (absolute or relative per `flags`).
    pub yaw: f32,
    /// Pitch in degrees (absolute or relative per `flags`).
    pub pitch: f32,
    /// Relative-coordinate bitmask.
    pub flags: i8,
    /// Teleport id the client must echo back in a `teleport_confirm` packet.
    /// Added in 1.9 (protocol 340 is 1.12.2); absent in 1.8.
    #[mc(varint)]
    pub teleport_id: i32,
}

/// Serverbound `teleport_confirm` packet.
///
/// # Architectural note
///
/// This packet does not exist in 1.8. From 1.9 onward the server assigns each
/// clientbound position/look a `teleport_id`, and the client must echo it back
/// here or the server rubber-bands the player back to the teleport origin. The
/// per-version adapter appends this `Send` directive when it processes a
/// clientbound position packet, so the confirm choreography stays entirely
/// inside the version crate.
///
// `TeleportConfirm` is byte-identical to v1-14's (measured; this packet does
// not exist in v1-8/1.8 at all), shared via `lodestone-protocol-common`
// ranged 340..=754 -- see `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::TeleportConfirm;

// `SpawnPosition` is byte-identical to v1-8's (measured), shared via
// `lodestone-protocol-common` -- see `packets::position`'s module docs. Not
// shared with v1-14: its `Position` field type has an incompatible 1.14+ bit
// layout.
pub use lodestone_protocol_common::packets::position::SpawnPosition;

/// Clientbound `update_health` packet.
///
/// Wire layout: f32 health, varint food, f32 saturation.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:update_health", state = Play, bound = Client)]
pub struct UpdateHealth {
    /// Current health (`0.0`..=`20.0`).
    pub health: f32,
    /// Current food level.
    #[mc(varint)]
    pub food: i32,
    /// Current food saturation.
    pub food_saturation: f32,
}

/// Clientbound `respawn` packet.
///
/// Wire layout: signed int dimension, unsigned byte difficulty, unsigned byte
/// game mode, string level type.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:respawn", state = Play, bound = Client)]
pub struct Respawn {
    /// Numeric dimension the player respawns into.
    pub dimension: i32,
    /// World difficulty.
    pub difficulty: u8,
    /// Packed game mode.
    pub game_mode: u8,
    /// Level type string.
    #[mc(max = 16)]
    pub level_type: String,
}

/// Clientbound `kick_disconnect` packet sent during play.
///
/// Wire layout: a single JSON string reason (as with login disconnect, not
/// NBT).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:kick_disconnect", state = Play, bound = Client)]
pub struct KickDisconnect {
    /// JSON-encoded disconnect reason component.
    pub reason: String,
}

// ServerboundPosition/ServerboundLook/ServerboundPositionLook are
// byte-identical across v1-8/v1-9/v1-14 (measured; raw f64/f32 fields, no
// embedded Position), shared via `lodestone-protocol-common` -- see
// `packets::movement`'s module docs.
pub use lodestone_protocol_common::packets::movement::{
    ServerboundLook, ServerboundPosition, ServerboundPositionLook,
};

// `ServerboundArmAnimation` is byte-identical to v1-14's (measured), shared
// via `lodestone-protocol-common` ranged 340..=754 -- 1.8 has no hand field
// at all. See `packets::chat`'s module docs.
pub use lodestone_protocol_common::packets::chat::ServerboundArmAnimation;

pub use lodestone_protocol_common::packets::movement::ServerboundFlying;

/// Serverbound `settings` (client settings) packet.
///
/// Wire layout: string locale, signed byte view distance, signed byte chat
/// flags, boolean chat colors, unsigned byte skin-part bitmask.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:settings", state = Play, bound = Server)]
pub struct ClientSettings {
    /// Client locale, such as `en_US`.
    #[mc(max = 16)]
    pub locale: String,
    /// Requested render distance in chunks.
    pub view_distance: i8,
    /// Chat visibility flags (`0` full, `1` commands only, `2` hidden).
    pub chat_flags: i8,
    /// Whether chat colors are enabled.
    pub chat_colors: bool,
    /// Displayed skin part bitmask.
    pub skin_parts: u8,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            locale: "en_US".to_owned(),
            view_distance: 8,
            chat_flags: 0,
            chat_colors: true,
            skin_parts: 0x7f,
        }
    }
}

/// Serverbound `block_dig` (player digging) — start, cancel, or finish breaking
/// a block, plus drop / release / swap-hands status codes.
///
/// # 1.12 divergence
///
/// The wire shape matches 1.8, but 1.9+ added **status 6 = swap item in hands**
/// (off-hand exists from 1.9), so `SwapItemWithOffhand` maps here rather than
/// being rejected as it is on protocol 47. There is no block-prediction
/// `sequence` (added 1.19); the model's `sequence` is dropped deliberately.
///
// `BlockDig` is byte-identical to v1-8's (measured), shared via
// `lodestone-protocol-common` -- see `packets::position`'s module docs. Not
// shared with v1-14: its `Position` field type has an incompatible 1.14+ bit
// layout.
pub use lodestone_protocol_common::packets::position::BlockDig;

/// Serverbound `block_place` (player block placement / item use on a block).
///
/// # 1.12 divergence
///
/// Unlike 1.8, 1.12 does **not** carry the held item stack inline: it sends a
/// `hand` index (0 main, 1 off), a varint `direction`, and a **float** cursor.
/// The server resolves the actual item from its own inventory view. Because
/// there is no inline stack, placement needs no item registry (contrast
/// protocol 47's inline `slot`). There is no block-prediction `sequence`.
///
/// Using an item in the air is expressed with `location = (-1,-1,-1)` and
/// `direction = -1`.
///
/// Wire layout: packed `position`, varint direction, varint hand, three f32
/// cursor coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_place", state = Play, bound = Server)]
pub struct BlockPlace {
    /// Target block position (or `(-1,-1,-1)` for use-in-air).
    pub location: Position,
    /// Face being placed against (`0..=5`, or `-1` for use-in-air).
    #[mc(varint)]
    pub direction: i32,
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
    /// Cursor X within the face (`0.0..=1.0`).
    pub cursor_x: f32,
    /// Cursor Y within the face (`0.0..=1.0`).
    pub cursor_y: f32,
    /// Cursor Z within the face (`0.0..=1.0`).
    pub cursor_z: f32,
}

/// Serverbound `block_place` as protocols 110 and 210 accept it: the cursor
/// is three **signed bytes**, not three floats.
///
/// The unit is the same in both forms — a sixteenth of a block face, so a
/// cursor `y` above the halfway point is what makes a slab place as a top
/// slab — but before 1.11 it was carried as the pixel index rather than the
/// fraction. That is a retype of three fields in the middle of the packet, so
/// it is a separate struct; encoding twelve bytes where the server expects
/// three would desynchronise the whole stream, not just this packet.
/// [`quantise_cursor`] converts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:block_place",
    state = Play,
    bound = Server,
    protocols = "110..=210"
)]
pub struct BlockPlaceByteCursor {
    /// Target block position (or `(-1,-1,-1)` for use-in-air).
    pub location: Position,
    /// Face being placed against (`0..=5`, or `-1` for use-in-air).
    #[mc(varint)]
    pub direction: i32,
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
    /// Cursor X within the face, in sixteenths (`0..=15`).
    pub cursor_x: i8,
    /// Cursor Y within the face, in sixteenths (`0..=15`).
    pub cursor_y: i8,
    /// Cursor Z within the face, in sixteenths (`0..=15`).
    pub cursor_z: i8,
}

/// Quantises a `0.0..=1.0` face coordinate into the sixteenth-of-a-face index
/// protocols 110 and 210 carry.
///
/// Clamped rather than wrapped: a raytraced cursor of exactly `1.0` must land
/// on the last pixel of the face, not roll over into the next block.
#[must_use]
pub fn quantise_cursor(value: f32) -> i8 {
    #[allow(clippy::cast_possible_truncation)]
    let scaled = (value * 16.0).floor() as i32;
    scaled.clamp(0, 15) as i8
}

/// Serverbound `use_entity` for an **attack** (mouse `1`): no hand, no hit
/// location.
///
/// Wire layout: varint target, varint mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_entity", state = Play, bound = Server)]
pub struct UseEntity {
    /// Target entity id.
    #[mc(varint)]
    pub target: i32,
    /// Interaction kind (always `1`).
    #[mc(varint)]
    pub mouse: i32,
}

/// Serverbound `use_entity` for a plain **interact** (mouse `0`).
///
/// # 1.12 divergence
///
/// 1.9+ added an off-hand, so unlike protocol 47 the interact form carries a
/// `hand` field. Kept as a distinct struct so it remains a plain derived struct
/// rather than needing a `switch`-on-`mouse` conditional.
///
/// Wire layout: varint target, varint mouse (`0`), varint hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_entity", state = Play, bound = Server)]
pub struct UseEntityInteract {
    /// Target entity id.
    #[mc(varint)]
    pub target: i32,
    /// Interaction kind (always `0`).
    #[mc(varint)]
    pub mouse: i32,
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
}

/// Serverbound `use_entity` with a precise hit location (mouse `2`,
/// interact-at), carrying the hand (1.9+).
///
/// Wire layout: varint target, varint mouse (`2`), f32 x/y/z, varint hand.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_entity", state = Play, bound = Server)]
pub struct UseEntityAt {
    /// Target entity id.
    #[mc(varint)]
    pub target: i32,
    /// Interaction kind (always `2`).
    #[mc(varint)]
    pub mouse: i32,
    /// Entity-local hit X.
    pub x: f32,
    /// Entity-local hit Y.
    pub y: f32,
    /// Entity-local hit Z.
    pub z: f32,
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
}

// `EntityAction` is byte-identical across v1-8/v1-9/v1-14 (measured), shared
// via `lodestone-protocol-common` -- see `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::EntityAction;

/// Serverbound `client_command` packet.
///
/// Wire layout: a single varint action id (`0` = perform respawn); same
/// shape at 1.8 and 1.16.2/.4/.5 per minecraft-data's `protocol.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:client_command", state = Play, bound = Server)]
pub struct ClientCommand {
    /// Action id (`0` = perform respawn).
    #[mc(varint)]
    pub action: i32,
}

/// Serverbound `spectate` packet — teleport to (or, while already
/// spectating, follow) another entity by uuid. Sent when a spectator clicks a
/// name in the tab list or player/team overlay.
///
/// Wire layout: a single 128-bit uuid; unchanged from 1.8 through 1.16.2 per
/// minecraft-data's `protocol.json` (`packet_spectate`: `{ target: UUID }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spectate", state = Play, bound = Server)]
pub struct Spectate {
    /// Uuid of the entity to spectate/teleport to.
    pub target: Uuid,
}

/// Clientbound `block_action` — a block-triggering "block event": a note
/// block playing, a piston starting to move, a chest lid opening or closing,
/// a beacon beam changing. Verified against minecraft-data's 1.12.2
/// `packet_block_action` (identical to 1.8's shape): packed `position`,
/// then `byte1`/`byte2` (opaque per-block-type parameters — meaning depends
/// on `block_id` and is a rendering/audio concern for the consumer, not
/// something the adapter interprets), then a varint legacy block *type* id
/// (not an `id:meta` composite — this space has no metadata component at
/// all, unlike `block_change`'s).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_action", state = Play, bound = Client)]
pub struct BlockAction {
    /// Block position the event occurred at.
    pub location: Position,
    /// First event parameter, meaning depends on `block_id`.
    pub byte1: u8,
    /// Second event parameter, meaning depends on `block_id`.
    pub byte2: u8,
    /// Legacy numeric block *type* id (no metadata), e.g. `25` = note block.
    #[mc(varint)]
    pub block_id: i32,
}

/// Clientbound `entity_equipment` — one entity equipment slot changed.
///
/// Verified against minecraft-data's 1.12.2 `packet_entity_equipment`: a
/// varint entity id, a varint `EquipmentSlot` ordinal, then a `slot` item
/// stack. Unlike the modern packet this carries exactly **one** slot per
/// message (the array-of-slots batching is a later addition), so the
/// adapter always emits a single-element `equipment` vec.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_equipment", state = Play, bound = Client)]
pub struct ClientboundEntityEquipment {
    /// Entity whose equipment changed.
    #[mc(varint)]
    pub entity_id: i32,
    /// Vanilla `EquipmentSlot` ordinal (0 main hand, 1 off hand, 2 boots, 3
    /// leggings, 4 chestplate, 5 helmet in this era).
    #[mc(varint)]
    pub slot: i32,
    /// New item in the slot, or `Slot::Empty` when cleared.
    pub item: Slot,
}

/// Clientbound `animation` — a hand-swing or hit-effect animation.
///
/// Verified against minecraft-data's 1.12.2 `packet_animation`: a varint
/// entity id, then a raw `u8` animation code. Wiki.vg's historical
/// documentation for this era lists `0` swing main arm, `2` leave bed, `3`
/// swing offhand, `4` critical effect, `5` magic critical effect; `1` is not
/// assigned a name at this protocol revision either, matching
/// `AnimationAction`'s own note that Mojang left it reserved. Any code this
/// crate doesn't recognise still travels intact via `AnimationAction::Other`
/// rather than being dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:animation", state = Play, bound = Client)]
pub struct Animation {
    /// Entity performing the animation.
    #[mc(varint)]
    pub entity_id: i32,
    /// Raw animation code.
    pub animation: u8,
}

/// Clientbound `named_sound_effect` — plays a sound by its namespaced name
/// (used for record discs, resource-pack-only sounds, etc.).
///
/// Verified against minecraft-data's 1.12.2 `packet_named_sound_effect`: a
/// string sound name, a varint sound category, then `x`/`y`/`z` as
/// fixed-point `i32`s (real coordinate × 8, vanilla's
/// `ClientboundSoundPacket` fixed-point convention), then `f32` volume and
/// pitch.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:named_sound_effect", state = Play, bound = Client)]
pub struct NamedSoundEffect {
    /// Sound event name; may or may not carry a `namespace:` prefix.
    #[mc(max = 256)]
    pub sound_name: String,
    /// Vanilla `SoundCategory` ordinal.
    #[mc(varint)]
    pub sound_category: i32,
    /// Fixed-point X (real coordinate × 8).
    pub x: i32,
    /// Fixed-point Y (real coordinate × 8).
    pub y: i32,
    /// Fixed-point Z (real coordinate × 8).
    pub z: i32,
    /// Volume multiplier.
    pub volume: f32,
    /// Pitch multiplier.
    pub pitch: f32,
}

/// Clientbound `named_sound_effect` as protocol 110 (Minecraft 1.9.4) sends
/// it: a single **unsigned byte** pitch rather than a float.
///
/// 1.10 widened the field; every earlier release quantised the pitch into one
/// byte on the way out. This is a retype, not a field appearing, so it is a
/// separate struct — see [`super::common`]'s module docs for why an
/// `#[mc(since)]` predicate cannot express one. [`legacy_pitch`] converts.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:named_sound_effect",
    state = Play,
    bound = Client,
    protocols = "110..=110"
)]
pub struct NamedSoundEffectBytePitch {
    /// Sound event name; may or may not carry a `namespace:` prefix.
    #[mc(max = 256)]
    pub sound_name: String,
    /// Vanilla `SoundCategory` ordinal.
    #[mc(varint)]
    pub sound_category: i32,
    /// Fixed-point X (real coordinate × 8).
    pub x: i32,
    /// Fixed-point Y (real coordinate × 8).
    pub y: i32,
    /// Fixed-point Z (real coordinate × 8).
    pub z: i32,
    /// Volume multiplier.
    pub volume: f32,
    /// Quantised pitch — see [`legacy_pitch`].
    pub pitch: u8,
}

/// Scale the pre-1.10 sound packets quantise a pitch multiplier by before
/// putting it in one byte.
///
/// Measured, not recalled: `/playsound … 1 1.5` against a real 1.9.4 server
/// puts **94** on the wire, and `… 1 0.5` puts **31**. 94/1.5 = 62.67 and
/// 31/0.5 = 62 are both consistent with a truncating `pitch * 63` and with
/// nothing else in the plausible range — a scale of 62 would give 93/31 and a
/// scale of 64 would give 96/32, so the pair of observations separates all
/// three. See `tests/captures/README.md` for the capture that recorded them.
pub const LEGACY_SOUND_PITCH_SCALE: f32 = 63.0;

/// Converts a pre-1.10 quantised pitch byte back to a multiplier.
#[must_use]
pub fn legacy_pitch(pitch: u8) -> f32 {
    f32::from(pitch) / LEGACY_SOUND_PITCH_SCALE
}

/// Clientbound `sound_effect` — plays a sound by its registry id.
///
/// Verified against minecraft-data's 1.12.2 `packet_sound_effect`: identical
/// shape to [`NamedSoundEffect`] except the leading field is a varint
/// `SoundEvent` registry id rather than a string name.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:sound_effect", state = Play, bound = Client)]
pub struct SoundEffect {
    /// Legacy `SoundEvent` registry id.
    #[mc(varint)]
    pub sound_id: i32,
    /// Vanilla `SoundCategory` ordinal.
    #[mc(varint)]
    pub sound_category: i32,
    /// Fixed-point X (real coordinate × 8).
    pub x: i32,
    /// Fixed-point Y (real coordinate × 8).
    pub y: i32,
    /// Fixed-point Z (real coordinate × 8).
    pub z: i32,
    /// Volume multiplier.
    pub volume: f32,
    /// Pitch multiplier.
    pub pitch: f32,
}

/// Clientbound `sound_effect` as protocol 110 sends it: a single **unsigned
/// byte** pitch rather than a float, matching
/// [`NamedSoundEffectBytePitch`]. Convert with [`legacy_pitch`].
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:sound_effect",
    state = Play,
    bound = Client,
    protocols = "110..=110"
)]
pub struct SoundEffectBytePitch {
    /// Legacy `SoundEvent` registry id.
    #[mc(varint)]
    pub sound_id: i32,
    /// Vanilla `SoundCategory` ordinal.
    #[mc(varint)]
    pub sound_category: i32,
    /// Fixed-point X (real coordinate × 8).
    pub x: i32,
    /// Fixed-point Y (real coordinate × 8).
    pub y: i32,
    /// Fixed-point Z (real coordinate × 8).
    pub z: i32,
    /// Volume multiplier.
    pub volume: f32,
    /// Quantised pitch — see [`legacy_pitch`].
    pub pitch: u8,
}

/// Clientbound `scoreboard_display_objective` — assigns an objective to a
/// display slot.
///
/// Verified against minecraft-data's 1.12.2
/// `packet_scoreboard_display_objective`: a raw `i8` slot position, then a
/// string objective name (empty string clears the slot — vanilla's
/// `ClientboundSetDisplayObjectivePacket` never sends a dedicated "clear"
/// marker at this protocol revision).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:scoreboard_display_objective", state = Play, bound = Client)]
pub struct ScoreboardDisplayObjective {
    /// Raw display-slot position (`0` list, `1` sidebar, `2` below-name at
    /// this protocol revision — the per-team-colour sidebar slots are a
    /// later addition).
    pub position: i8,
    /// Objective name, or an empty string to clear the slot.
    #[mc(max = 16)]
    pub name: String,
}

/// Clientbound `update_time` — the world's age and the current time of day.
///
/// Wire layout: two raw `i64`s, verified against minecraft-data's 1.12.2
/// `packet_update_time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:update_time", state = Play, bound = Client)]
pub struct UpdateTime {
    /// Total world age, in ticks.
    pub age: i64,
    /// Current time of day, in ticks.
    pub time: i64,
}

// AttachEntity/SetPassengers/Collect are byte-identical to v1-14's own
// definitions (measured), shared via `lodestone-protocol-common` ranged
// 340..=754 -- v1-8's `attach_entity` carries an extra `leash: bool` field and
// its `collect` has no pickup count, so neither is in this range. See
// `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::{AttachEntity, Collect, SetPassengers};

/// Clientbound `entity_effect` — a status effect was applied to an entity.
///
/// Wire layout: verified against minecraft-data's 1.12.2 `packet_entity_effect`:
/// VarInt entity id, raw `i8` legacy effect id, raw `i8` amplifier, VarInt
/// duration, raw `i8` flags byte. 1.12.2's flags byte packs two bits — `0x01`
/// ambient, `0x02` show particles — where later protocols add a third
/// (`0x04` show icon); this protocol revision has neither the "show icon" nor
/// the "blend" bit, so those two always decode as `true`/`false` respectively
/// in the adapter, matching vanilla 1.12.2's own always-on HUD icon and
/// always-off blend behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_effect", state = Play, bound = Client)]
pub struct EntityEffect {
    /// Target entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Legacy (1-based) potion-effect id.
    pub effect_id: i8,
    /// Effect amplifier (`0` = level I).
    pub amplifier: i8,
    /// Remaining duration, in ticks.
    #[mc(varint)]
    pub duration: i32,
    /// Flags byte: bit `0x01` ambient, bit `0x02` show particles.
    pub flags: i8,
}

// `RemoveEntityEffect` is byte-identical to v1-14's own definition (measured),
// shared via `lodestone-protocol-common` ranged 340..=754 -- see
// `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::RemoveEntityEffect;

/// Clientbound `difficulty` — the server's configured difficulty changed.
///
/// Wire layout: a single raw `u8`, verified against minecraft-data's 1.12.2
/// `packet_difficulty`. 1.12.2 has no "locked" bit — that is a later
/// addition — so the adapter always reports `locked: false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:difficulty", state = Play, bound = Client)]
pub struct DifficultyPacket {
    /// Raw difficulty id (`0` peaceful .. `3` hard).
    pub difficulty: u8,
}

/// Clientbound `playerlist_header` — the tab list's header/footer text.
///
/// Wire layout: two length-prefixed strings, verified against
/// minecraft-data's 1.12.2 `packet_playerlist_header`. Both are JSON text
/// components, the same as `chat` and `open_window`'s title at this protocol
/// revision — not plain legacy text.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:playerlist_header", state = Play, bound = Client)]
pub struct PlayerlistHeader {
    /// Header JSON text.
    #[mc(max = 32767)]
    pub header: String,
    /// Footer JSON text.
    #[mc(max = 32767)]
    pub footer: String,
}

/// Clientbound `open_sign_entity` — the server opened a sign-editing UI.
///
/// Wire layout: a single packed 1.8 [`Position`](super::position::Position),
/// verified against minecraft-data's 1.12.2 `packet_open_sign_entity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:open_sign_entity", state = Play, bound = Client)]
pub struct OpenSignEntity {
    /// Block position of the sign.
    pub location: super::position::Position,
}

