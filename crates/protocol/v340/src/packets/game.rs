//! Play-state packets for protocol 340.

use lodestone_macros::{Decode, Encode, Packet};

use crate::packets::position::Position;

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

/// Clientbound `chat` packet.
///
/// # Architectural note
///
/// The message is a **JSON string**, not the modern NBT text component. The
/// shared [`lodestone_model::Text::from_json`] front-end parses it into the same
/// format-agnostic tree that modern NBT chat decodes to.
///
/// Wire layout: string message (JSON), signed byte position (`0` chat, `1`
/// system, `2` action bar / game info).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat", state = Play, bound = Client)]
pub struct ClientboundChat {
    /// JSON-encoded chat component.
    pub message: String,
    /// Chat slot: `0` chat, `1` system, `2` action bar.
    pub position: i8,
}

/// Serverbound `chat` packet.
///
/// Wire layout: a single string (max 100 chars). A message beginning with `/`
/// is treated by the server as a command; 1.8 has no separate command packet.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat", state = Play, bound = Server)]
pub struct ServerboundChat {
    /// Message text (or `/command`), at most 256 characters (1.11+ raised this
    /// from the 100-character 1.8 limit).
    #[mc(max = 256)]
    pub message: String,
}

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
/// Wire layout: a single varint teleport id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:teleport_confirm", state = Play, bound = Server)]
pub struct TeleportConfirm {
    /// Teleport id echoed from the clientbound position packet.
    #[mc(varint)]
    pub teleport_id: i32,
}

/// Clientbound `spawn_position` packet setting the client's compass target.
///
/// Wire layout: a single packed 1.8 [`Position`]. This is the crate's real use
/// of the hand-written packed-position codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_position", state = Play, bound = Client)]
pub struct SpawnPosition {
    /// Compass target block position.
    pub location: Position,
}

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

/// Serverbound `position` packet.
///
/// Wire layout: f64 x/y/z, boolean on-ground.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:position", state = Play, bound = Server)]
pub struct ServerboundPosition {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate (feet position).
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Whether the player is on the ground.
    pub on_ground: bool,
}

/// Serverbound `look` packet.
///
/// Wire layout: f32 yaw/pitch, boolean on-ground.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:look", state = Play, bound = Server)]
pub struct ServerboundLook {
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the player is on the ground.
    pub on_ground: bool,
}

/// Serverbound `position_look` packet.
///
/// Wire layout: f64 x/y/z, f32 yaw/pitch, boolean on-ground.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:position_look", state = Play, bound = Server)]
pub struct ServerboundPositionLook {
    /// X coordinate.
    pub x: f64,
    /// Y coordinate (feet position).
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the player is on the ground.
    pub on_ground: bool,
}

/// Serverbound `arm_animation` (swing arm) packet.
///
/// Unlike 1.8 (protocol 47), where this packet is empty, 1.9+ carries which
/// hand swung as a VarInt (`0` = main, `1` = off). This per-version divergence
/// is why the swing encoding lives in each version crate rather than a shared
/// one.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:arm_animation", state = Play, bound = Server)]
pub struct ServerboundArmAnimation {
    /// Hand that swung: `0` = main hand, `1` = off hand.
    #[mc(varint)]
    pub hand: i32,
}

/// Serverbound `flying` (player-on-ground) packet.
///
/// Wire layout: a single boolean on-ground flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:flying", state = Play, bound = Server)]
pub struct ServerboundFlying {
    /// Whether the player is on the ground.
    pub on_ground: bool,
}

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
/// Wire layout: varint status, packed `position`, signed-byte face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_dig", state = Play, bound = Server)]
pub struct BlockDig {
    /// Digging status code.
    #[mc(varint)]
    pub status: i32,
    /// Target block position.
    pub location: Position,
    /// Face being mined (`0..=5`).
    pub face: i8,
}

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

/// Serverbound `entity_action` (player command) — sneak, sprint, leave bed, and
/// vehicle actions.
///
/// Wire layout: varint entity id, varint action id, varint jump boost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_action", state = Play, bound = Server)]
pub struct EntityAction {
    /// Player entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Action id (see the adapter's `PlayerCommand` mapping).
    #[mc(varint)]
    pub action_id: i32,
    /// Jump boost for the ride-jump action, otherwise `0`.
    #[mc(varint)]
    pub jump_boost: i32,
}
