//! Play-state packets for protocol 5 that are not entity-, world-, window- or
//! chunk-shaped.

use lodestone_macros::{Decode, Encode, Packet};

use super::position::PositionIii;

/// Clientbound `login` (game-join) packet.
///
/// Wire layout: `i32` entity id, `u8` game mode, `i8` dimension, `u8`
/// difficulty, `u8` max players, string level type. Measured from a real
/// join: a 13-byte body for entity id 22 in a flat overworld.
///
/// Two differences from protocol 47's otherwise identically-named packet:
/// there is no trailing `reducedDebugInfo` boolean, and `dimension` is a
/// signed byte rather than an int (protocol 110 widens it). A decoder that
/// expects the boolean reads one byte past the end of this packet.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login", state = Play, bound = Client)]
pub struct JoinGame {
    /// Local player entity id.
    pub entity_id: i32,
    /// Packed game mode: low two bits are the mode, `0x8` marks hardcore.
    pub game_mode: u8,
    /// Numeric dimension: `-1` nether, `0` overworld, `1` end.
    pub dimension: i8,
    /// World difficulty, `0` peaceful through `3` hard.
    pub difficulty: u8,
    /// Maximum player count. A legacy hint the client does not act on.
    pub max_players: u8,
    /// Level type string, such as `default` or `flat`.
    #[mc(max = 16)]
    pub level_type: String,
}

/// Clientbound `chat` packet carrying a JSON component string.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat", state = Play, bound = Client)]
pub struct ClientboundChat {
    /// JSON-encoded chat component.
    #[mc(max = 32767)]
    pub message: String,
}

/// Serverbound `chat` packet.
///
/// A message beginning with `/` is treated by the server as a command; this
/// era has no separate command packet.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat", state = Play, bound = Server)]
pub struct ServerboundChat {
    /// Message text, or `/command`.
    #[mc(max = 100)]
    pub message: String,
}

/// Clientbound `keep_alive`.
///
/// The payload is an `i32`, not the varint protocol 47 uses. Measured: a
/// four-byte body on every one of the nine keep-alives in the recorded join.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Client)]
pub struct KeepAliveRequest {
    /// Token the client must echo back unchanged.
    pub keep_alive_id: i32,
}

/// Serverbound `keep_alive`, echoing the server's token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:keep_alive", state = Play, bound = Server)]
pub struct KeepAliveResponse {
    /// Token from the matching request.
    pub keep_alive_id: i32,
}

/// Clientbound `update_time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:update_time", state = Play, bound = Client)]
pub struct UpdateTime {
    /// Total world age in ticks.
    pub age: i64,
    /// Time of day in ticks; negative means the day-night cycle is frozen.
    pub time: i64,
}

/// Clientbound `update_health`.
///
/// `food` is an `i16` here, where protocol 47 sends a varint. The two agree
/// for every food level a server can actually send (`0..=20`) in *length* but
/// not in bytes: 20 is `0x00 0x14` here and `0x14` there.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:update_health", state = Play, bound = Client)]
pub struct UpdateHealth {
    /// Current health, `0.0` through `20.0`.
    pub health: f32,
    /// Food level, `0` through `20`.
    pub food: i16,
    /// Food saturation.
    pub food_saturation: f32,
}

/// Clientbound `respawn`.
///
/// `dimension` is an `i32` here even though the join packet's is an `i8` — a
/// genuine inconsistency inside this one protocol, not a transcription slip.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:respawn", state = Play, bound = Client)]
pub struct Respawn {
    /// Numeric dimension being respawned into.
    pub dimension: i32,
    /// World difficulty.
    pub difficulty: u8,
    /// Game mode after the respawn.
    pub gamemode: u8,
    /// Level type string.
    #[mc(max = 16)]
    pub level_type: String,
}

/// Clientbound `position` (player position and look).
///
/// Wire layout: `f64` x/[`stance`](Self::stance)/z, `f32` yaw/pitch, `bool`
/// on-ground. Protocol 47 replaces the trailing boolean with a
/// relative-coordinate flags byte, so a decoder for that era reads this
/// packet's `on_ground` as a flags mask — `true` becomes `0x01`, a relative
/// x — and teleports the player to the wrong place rather than erroring.
///
/// # The middle coordinate is the eye position, not the feet
///
/// Unlike every later protocol, the second `f64` here is the **stance**: the
/// same eye-height value the serverbound packets carry in their own `stance`
/// slot. Measured, because the two readings differ by a constant and so look
/// equally plausible in isolation: teleporting a player to an exact `y` of
/// 80.0 over RCON produced 81.62 in this field, and a fresh login the server's
/// own log placed at `y` 2.0 produced 3.62. Reading it as feet puts the player
/// 1.62 blocks in the air on every teleport, and — because the confirmation
/// echo derives its own stance from the value — makes that echo carry a stance
/// the server refuses, so it rubber-bands every subsequent move instead of
/// accepting it.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:position", state = Play, bound = Client)]
pub struct ClientboundPositionLook {
    /// Absolute x.
    pub x: f64,
    /// Absolute eye height, **not** the feet: see the type's own docs.
    pub stance: f64,
    /// Absolute z.
    pub z: f64,
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the server considers the player on the ground.
    pub on_ground: bool,
}

/// Serverbound `position`.
///
/// # The stance field, and the order it sits in
///
/// This era's movement packets carry a `stance` alongside `y`: the eye
/// height, `y + 1.62` for a standing player. Protocol 47 removed it.
///
/// The field order — `x`, `y`, `stance`, `z`, with the stance **after** the
/// feet — was measured, because getting it wrong is silent. Until the client
/// echoes a position matching the one the server teleported it to, the server
/// holds the player and discards movement without logging anything or closing
/// the connection; with `stance` and `y` transposed the echo never matches, so
/// the hold never lifts. Measured both ways against a real server by walking
/// 320 blocks and reading the outcome three independent ways: with the
/// transposition the server re-sent its own position 65-70 times, streamed no
/// further chunk columns, and saved the player at the spawn point on logout;
/// with this order it re-sent its position once, streamed new columns, and
/// began sending chunk unloads for the columns left behind. The stance check
/// the server would otherwise fail on never runs, because the held-player
/// branch returns before reaching it — which is why the wrong order produces
/// no error at all.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:position", state = Play, bound = Server)]
pub struct ServerboundPosition {
    /// Absolute x.
    pub x: f64,
    /// Absolute y, at the player's feet.
    pub y: f64,
    /// Eye height: feet `y` plus the standing eye offset.
    pub stance: f64,
    /// Absolute z.
    pub z: f64,
    /// Whether the client believes it is on the ground.
    pub on_ground: bool,
}

/// Serverbound `look`, rotation only.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:look", state = Play, bound = Server)]
pub struct ServerboundLook {
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the client believes it is on the ground.
    pub on_ground: bool,
}

/// Serverbound `position_look`, position and rotation together.
///
/// Same `x`, `y`, `stance`, `z` order as [`ServerboundPosition`], and the same
/// measurement stands behind it. This is also the packet the clientbound
/// teleport is confirmed with.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:position_look", state = Play, bound = Server)]
pub struct ServerboundPositionLook {
    /// Absolute x.
    pub x: f64,
    /// Absolute y, at the player's feet.
    pub y: f64,
    /// Eye height: feet `y` plus the standing eye offset.
    pub stance: f64,
    /// Absolute z.
    pub z: f64,
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the client believes it is on the ground.
    pub on_ground: bool,
}

/// Serverbound `flying`: the on-ground-only movement tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:flying", state = Play, bound = Server)]
pub struct ServerboundFlying {
    /// Whether the client believes it is on the ground.
    pub on_ground: bool,
}

/// Clientbound `spawn_position`, the compass target.
///
/// Three separate ints, not a packed long.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_position", state = Play, bound = Client)]
pub struct SpawnPosition {
    /// World spawn.
    pub location: PositionIii,
}

/// Clientbound `experience`.
///
/// Both integer fields are `i16` here; protocol 47 sends varints.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:experience", state = Play, bound = Client)]
pub struct Experience {
    /// Progress through the current level, `0.0` through `1.0`.
    pub experience_bar: f32,
    /// Current level.
    pub level: i16,
    /// Total accumulated experience.
    pub total_experience: i16,
}

/// Clientbound `kick_disconnect`, carrying a JSON reason.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:kick_disconnect", state = Play, bound = Client)]
pub struct KickDisconnect {
    /// JSON-encoded disconnect reason.
    #[mc(max = 32767)]
    pub reason: String,
}

/// Clientbound `game_state_change`.
///
/// `game_mode` is an `f32` because the field is a generic value slot shared
/// by every reason code, and some reasons carry a fractional value.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:game_state_change", state = Play, bound = Client)]
pub struct GameStateChange {
    /// Reason code.
    pub reason: u8,
    /// Reason-dependent value.
    pub game_mode: f32,
}

/// Clientbound `statistics`.
///
/// Decoded to keep the framing honest; nothing downstream consumes a
/// statistic yet, and the entry count is the only thing retained.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:statistics", state = Play, bound = Client)]
pub struct Statistics {
    /// One `(name, value)` pair per statistic.
    #[mc(len = "varint")]
    pub entries: Vec<StatisticEntry>,
}

/// One statistic in a [`Statistics`] packet.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct StatisticEntry {
    /// Statistic name, such as `stat.playOneMinute`.
    #[mc(max = 32767)]
    pub name: String,
    /// Accumulated value.
    #[mc(varint)]
    pub value: i32,
}

/// Serverbound `client_command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:client_command", state = Play, bound = Server)]
pub struct ClientCommand {
    /// `0` respawn, `1` request statistics, `2` open inventory achievement.
    pub payload: i8,
}

/// Serverbound `arm_animation`.
///
/// Carries an entity id and an animation ordinal, both of which the server
/// ignores in favour of the sender's own identity. Protocol 47 reduced it to
/// an empty body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:arm_animation", state = Play, bound = Server)]
pub struct ServerboundArmAnimation {
    /// Sender's entity id. The server does not trust it.
    pub entity_id: i32,
    /// Animation ordinal; `1` is the swing.
    pub animation: i8,
}

/// Serverbound `entity_action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_action", state = Play, bound = Server)]
pub struct EntityAction {
    /// Sender's entity id.
    pub entity_id: i32,
    /// Action ordinal: `1` crouch, `2` uncrouch, `3` leave bed, `4` start
    /// sprinting, `5` stop sprinting.
    pub action_id: i8,
    /// Jump boost for a horse, `0` otherwise.
    pub jump_boost: i32,
}

/// Serverbound `use_entity`.
///
/// A `mouse` of `0` is an attack and `1` is an interaction. Protocol 47
/// replaced this with a varint-typed packet that also carries an interaction
/// point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_entity", state = Play, bound = Server)]
pub struct UseEntity {
    /// Target entity id.
    pub target: i32,
    /// `0` attack, `1` interact.
    pub mouse: i8,
}

/// Serverbound `custom_payload`.
///
/// # The length prefix
///
/// The payload here is prefixed with a big-endian `i16` byte count. Protocol
/// 47 made it the rest of the packet with no count at all. Measured on the
/// brand message from a real server: `MC|Brand` followed by `00 07` and seven
/// bytes of `vanilla`, an 18-byte body.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:custom_payload", state = Play, bound = Server)]
pub struct ServerboundCustomPayload {
    /// Channel name, such as `MC|Brand`.
    #[mc(max = 20)]
    pub channel: String,
    /// Channel-specific bytes.
    #[mc(len = "i16")]
    pub data: Vec<u8>,
}

/// Clientbound `custom_payload`, same shape as the serverbound one.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:custom_payload", state = Play, bound = Client)]
pub struct ClientboundCustomPayload {
    /// Channel name.
    #[mc(max = 20)]
    pub channel: String,
    /// Channel-specific bytes.
    #[mc(len = "i16")]
    pub data: Vec<u8>,
}
