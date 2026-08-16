//! Play-state packets for protocol 47.

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
    /// Numeric dimension (`-1` nether, `0` overworld, `1` end).
    pub dimension: i8,
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
    /// Message text (or `/command`), at most 100 characters.
    #[mc(max = 100)]
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

/// Clientbound `set_compression` packet (rare in play; compression is normally
/// negotiated during login).
///
/// Wire layout: a single varint threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_compression", state = Play, bound = Client)]
pub struct PlaySetCompression {
    /// Compression threshold in bytes.
    #[mc(varint)]
    pub threshold: i32,
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
/// a block, plus the drop/shoot/eat status codes that share this packet.
///
/// # 1.8 divergence
///
/// 1.8 folds block breaking **and** item dropping / bow release / eating into a
/// single packet distinguished by `status` (modern 26.2 splits several of these
/// into `player_action` ordinals). Status codes: `0` start, `1` cancel, `2`
/// finish, `3` drop stack, `4` drop item, `5` shoot arrow / finish eating. There
/// is no block-prediction `sequence` (added in 1.19), so the model's `sequence`
/// is dropped deliberately by the adapter.
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
/// # 1.8 divergence
///
/// 1.8 carries the **held item stack inline** (`held_item`) and the cursor hit
/// position as three signed bytes in `0..=15`, where modern versions send a hand
/// index, float cursor, and a block-prediction `sequence` that the server
/// resolves against its own inventory view. Because the adapter is a pure
/// function with no inventory state, it sends an **empty** `held_item`; the
/// vanilla 1.8 server ignores this field and uses its own authoritative view of
/// the player's held item, so placement still resolves (verified live).
///
/// Using an item in the air (right-click with nothing targeted) is expressed by
/// this same packet with `location = (-1, -1, -1)`, `direction = -1`, and a zero
/// cursor.
///
/// Wire layout: packed `position`, signed-byte direction, `slot` held item,
/// three signed-byte cursor coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_place", state = Play, bound = Server)]
pub struct BlockPlace {
    /// Target block position (or `(-1,-1,-1)` for use-in-air).
    pub location: Position,
    /// Face being placed against (`0..=5`, or `-1` for use-in-air).
    pub direction: i8,
    /// The held item stack. Sent empty; the server uses its own view.
    pub held_item: Slot,
    /// Cursor X within the face (`0..=15`).
    pub cursor_x: i8,
    /// Cursor Y within the face (`0..=15`).
    pub cursor_y: i8,
    /// Cursor Z within the face (`0..=15`).
    pub cursor_z: i8,
}

/// Serverbound `use_entity` — attack or interact with an entity, without the
/// precise hit location (mouse `0` interact, `1` attack).
///
/// # 1.8 divergence
///
/// 1.8 has no off-hand, so there is no `hand` field (added in 1.9). The
/// interact-at variant carries a hit location and is a separate struct
/// ([`UseEntityAt`]) so both remain plain derived structs rather than needing a
/// `switch`-on-`mouse` conditional the derive macro cannot express.
///
/// Wire layout: varint target, varint mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_entity", state = Play, bound = Server)]
pub struct UseEntity {
    /// Target entity id.
    #[mc(varint)]
    pub target: i32,
    /// Interaction kind (`0` interact, `1` attack).
    #[mc(varint)]
    pub mouse: i32,
}

/// Serverbound `use_entity` with a precise hit location (mouse `2` interact-at).
///
/// Wire layout: varint target, varint mouse (always `2`), f32 x/y/z.
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

/// Serverbound `client_command` packet.
///
/// Wire layout: a single varint action id. `0` (`PERFORM_RESPAWN`) is the
/// only ordinal a canonical [`crate::adapter`] emits today; verified against
/// minecraft-data's 1.8 `protocol.json` (`packet_client_command`: a lone
/// `varint` field, same shape at 1.12.2 and 1.16.2/.4/.5).
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

/// Clientbound `difficulty` packet.
///
/// Wire layout: a single unsigned byte (`0` peaceful .. `3` hard). Verified
/// against minecraft-data's 1.8 `packet_difficulty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:difficulty", state = Play, bound = Client)]
pub struct DifficultyPacket {
    /// World difficulty (`0` peaceful .. `3` hard).
    pub difficulty: u8,
}

/// Clientbound `camera` packet — attaches the client's camera to an entity.
///
/// Wire layout: a single varint entity id. Verified against minecraft-data's
/// 1.8 `packet_camera`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:camera", state = Play, bound = Client)]
pub struct CameraPacket {
    /// Entity id the camera should follow.
    #[mc(varint)]
    pub camera_id: i32,
}

/// Clientbound `playerlist_header` packet — tab-list header/footer text.
///
/// Wire layout: two strings, header then footer. Both are **JSON** chat
/// components: this packet was introduced alongside 1.8's own JSON text
/// component format, unlike the scoreboard/team packets' plain legacy text
/// (which predate 1.8's JSON migration and were never revisited).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:playerlist_header", state = Play, bound = Client)]
pub struct PlayerlistHeader {
    /// JSON-encoded header component.
    #[mc(max = 32767)]
    pub header: String,
    /// JSON-encoded footer component.
    #[mc(max = 32767)]
    pub footer: String,
}

/// Clientbound `experience` packet — the local player's XP bar/level.
///
/// Wire layout: f32 progress bar, varint level, varint total experience.
/// Verified against minecraft-data's 1.8 `packet_experience`.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:experience", state = Play, bound = Client)]
pub struct Experience {
    /// Progress toward the next level, in `0.0..1.0`.
    pub bar: f32,
    /// Current experience level.
    #[mc(varint)]
    pub level: i32,
    /// Total accumulated experience points.
    #[mc(varint)]
    pub total: i32,
}
