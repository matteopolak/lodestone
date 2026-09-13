//! Play-state packets for this era (protocols 498, 578, 754).

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use crate::packets::position::Position;

/// Clientbound `block_action` / block-event packet.
///
/// Its final numeric field is a block-type registry id, not a block-state id;
/// the adapter resolves it through the negotiated protocol's historical
/// registry table before emitting the shared block-event model event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_action", state = Play, bound = Client)]
pub struct BlockAction {
    /// Block position.
    pub location: Position,
    /// First per-block event parameter.
    pub action: u8,
    /// Second per-block event parameter.
    pub parameter: u8,
    /// Historical block-type registry id.
    #[mc(varint)]
    pub block_id: i32,
}

/// Clientbound `login` (game-join) packet for 1.16.5 (protocol 754) only.
///
/// [`JoinGameLegacy`] is the 498/578 form. The two share the leading entity
/// id and nothing else: at 498 the second field is a game-mode byte and at
/// 754 it is a hardcore boolean, and the dimension is a plain `i32` there
/// against two inline NBT blobs here. Nothing about that is expressible as
/// a `since`/`until` predicate, and getting it wrong would not error --
/// `is_hardcore` would read the game-mode byte's low bit and every later
/// field would be shifted.
///
/// # Architectural notes
///
/// 1.16 rewrote join substantially, and it is the clearest place the version
/// isolation earns its keep:
///
/// * The current dimension is no longer a numeric byte (`-1`/`0`/`1`); it is a
///   **namespaced world name string** (`world_name`, e.g. `minecraft:overworld`)
///   plus two inline **NBT** blobs — a `dimension_codec` registry and the
///   per-world `dimension` type. This crate consumes both NBT blobs (there is no
///   registry consumer yet) and maps `world_name` onto the canonical
///   `DimensionId`.
/// * `game_mode` is a plain `u8` (0..=3); hardcore is a **separate** boolean
///   (`is_hardcore`), unlike pre-1.16 where the `0x8` bit of the mode byte
///   flagged it.
///
/// The inline NBT blobs are captured as raw named-NBT byte spans; the client
/// currently only needs to carry/validate them, not interpret their contents.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login", state = Play, bound = Client, protocols = "754..=754")]
pub struct JoinGame {
    /// Local player entity id.
    pub entity_id: i32,
    /// Whether the world is hardcore (a separate field since 1.16).
    pub is_hardcore: bool,
    /// Game mode (`0` survival, `1` creative, `2` adventure, `3` spectator).
    pub game_mode: u8,
    /// Previous game mode (`255`/`-1` when there is none).
    pub previous_game_mode: u8,
    /// Names of every world on the server (namespaced identifiers).
    pub world_names: Vec<String>,
    /// Raw named-NBT dimension codec registry.
    #[mc(nbt)]
    pub dimension_codec: Vec<u8>,
    /// Raw named-NBT current dimension type.
    #[mc(nbt)]
    pub dimension: Vec<u8>,
    /// Namespaced identifier of the world the player is joining.
    pub world_name: String,
    /// Hashed world seed (for client-side biome noise).
    pub hashed_seed: i64,
    /// Legacy max-players hint.
    #[mc(varint)]
    pub max_players: i32,
    /// Server view distance in chunks.
    #[mc(varint)]
    pub view_distance: i32,
    /// Whether reduced debug info is in effect.
    pub reduced_debug_info: bool,
    /// Whether the respawn screen is shown on death.
    pub enable_respawn_screen: bool,
    /// Whether this is a debug world.
    pub is_debug: bool,
    /// Whether this is a superflat world.
    pub is_flat: bool,
}

/// Clientbound `login` (game-join) packet for 1.14.4 and 1.15.2 (protocols
/// 498 and 578).
///
/// Pre-1.16 join is the shape 1.13 left behind: a signed numeric dimension
/// (`-1` nether, `0` overworld, `1` end) rather than a namespaced world name,
/// a `level_type` string rather than a dimension codec, and hardcore folded
/// into the `0x8` bit of the game-mode byte.
///
/// 1.15 added exactly two fields, both **appended to an existing prefix
/// rather than retyping anything**, which is what makes one struct with two
/// predicates correct here where [`JoinGame`] needed its own: a seed hash
/// after the dimension, and a respawn-screen flag at the end.
///
/// Wire layout: i32 entity id, u8 game mode, i32 dimension, [i64 hashed seed
/// — 578 only], u8 max players, string level type, varint view distance, bool
/// reduced debug info, [bool enable respawn screen — 578 only].
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login", state = Play, bound = Client, protocols = "498..=578")]
pub struct JoinGameLegacy {
    /// Local player entity id.
    pub entity_id: i32,
    /// Game mode; the `0x8` bit flags hardcore, unlike 1.16's separate field.
    pub game_mode: u8,
    /// Numeric dimension: `-1` nether, `0` overworld, `1` end.
    pub dimension: i32,
    /// Hashed world seed (for client-side biome noise). Added in 1.15.
    #[mc(since = 578)]
    pub hashed_seed: i64,
    /// Legacy max-players hint.
    pub max_players: u8,
    /// World generator name, such as `default` or `flat`.
    #[mc(max = 16)]
    pub level_type: String,
    /// Server view distance in chunks.
    #[mc(varint)]
    pub view_distance: i32,
    /// Whether reduced debug info is in effect.
    pub reduced_debug_info: bool,
    /// Whether the respawn screen is shown on death. Added in 1.15.
    #[mc(since = 578)]
    pub enable_respawn_screen: bool,
}

/// Clientbound `chat` packet.
///
/// # Architectural note
///
/// The message is a **JSON string**, not the modern NBT text component. The
/// shared [`lodestone_model::Text::from_json`] front-end parses it into the same
/// format-agnostic tree that modern NBT chat decodes to. 1.16 appended a
/// `sender` UUID (the source player, or the nil UUID for system messages).
///
/// Wire layout: string message (JSON), signed byte position (`0` chat, `1`
/// system, `2` action bar / game info), then — from 1.16 only — a 128-bit
/// sender uuid. 498 and 578 end after the position byte; the field is
/// appended, never retyped, so a `since` predicate carries it.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat", state = Play, bound = Client)]
pub struct ClientboundChat {
    /// JSON-encoded chat component.
    pub message: String,
    /// Chat slot: `0` chat, `1` system, `2` action bar.
    pub position: i8,
    /// UUID of the sending player (nil for system/server messages). Added in
    /// 1.16; the nil UUID below 754, where the wire carries no such field.
    #[mc(since = 754)]
    pub sender: Uuid,
}

// `ServerboundChat` is byte-identical to v1-9's (measured), shared via
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
    /// Added in 1.9 (protocol 754 is 1.16.5); absent in 1.8.
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
// `TeleportConfirm` is byte-identical to v1-9's (measured; this packet does
// not exist in v1-8/1.8 at all), shared via `lodestone-protocol-common`
// ranged 340..=754 -- see `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::TeleportConfirm;

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

/// Clientbound `respawn` packet for 1.16.5 (protocol 754) only.
///
/// Like [`JoinGame`], 1.16 replaced the numeric dimension with a namespaced
/// `world_name` string plus an inline raw named-NBT dimension type. It is not on
/// the join-and-stay critical path (respawn fires only on death / dimension
/// change), but the shape is derived for correctness. [`RespawnLegacy`] is
/// the 498/578 form; the very first field is `i32` there and NBT here, so
/// the split is a second struct, not a predicate.
///
/// Wire layout: nbt dimension, string world name, i64 hashed seed, u8 game mode,
/// u8 previous game mode, bool is-debug, bool is-flat, bool copy-metadata.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:respawn", state = Play, bound = Client, protocols = "754..=754")]
pub struct Respawn {
    /// Raw named-NBT dimension type.
    #[mc(nbt)]
    pub dimension: Vec<u8>,
    /// Namespaced identifier of the world respawned into.
    pub world_name: String,
    /// Hashed world seed (retained so raw packet bytes can be replayed exactly).
    pub hashed_seed: i64,
    /// Game mode after respawn.
    pub game_mode: u8,
    /// Previous game mode.
    pub previous_game_mode: u8,
    /// Whether this is a debug world.
    pub is_debug: bool,
    /// Whether this is a superflat world.
    pub is_flat: bool,
    /// Whether to keep entity metadata / attributes across the respawn.
    pub copy_metadata: bool,
}

/// Clientbound `respawn` packet for 1.14.4 and 1.15.2 (protocols 498, 578).
///
/// The pre-1.16 shape: a numeric dimension and a generator-name string, with
/// 1.15's seed hash **inserted between** the dimension and the game mode —
/// an appearing field, so one `since` predicate carries it.
///
/// Wire layout: i32 dimension, [i64 hashed seed — 578 only], u8 game mode,
/// string level type.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:respawn", state = Play, bound = Client, protocols = "498..=578")]
pub struct RespawnLegacy {
    /// Numeric dimension: `-1` nether, `0` overworld, `1` end.
    pub dimension: i32,
    /// Hashed world seed. Added in 1.15.
    #[mc(since = 578)]
    pub hashed_seed: i64,
    /// Game mode after respawn; the `0x8` bit flags hardcore.
    pub game_mode: u8,
    /// World generator name, such as `default` or `flat`.
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

/// Clientbound `game_state_change` — one reason byte and a reason-dependent
/// floating-point value. The adapter recognizes the weather and game-mode
/// reasons whose semantics have a shared-model event.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:game_state_change", state = Play, bound = Client)]
pub struct GameStateChange {
    /// Numeric event reason.
    pub reason: u8,
    /// Reason-dependent value (game-mode ordinal or weather intensity).
    pub value: f32,
}

// ServerboundPosition/ServerboundLook/ServerboundPositionLook are
// byte-identical across v1-8/v1-9/v1-14 (measured; raw f64/f32 fields, no
// embedded Position), shared via `lodestone-protocol-common` -- see
// `packets::movement`'s module docs.
pub use lodestone_protocol_common::packets::movement::{
    ServerboundLook, ServerboundPosition, ServerboundPositionLook,
};

// `ServerboundArmAnimation` is byte-identical to v1-9's (measured), shared
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
/// # 1.16 divergence
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
/// # 1.14+ shape
///
/// 1.16.5 sends `hand` **first**, then the packed `position`, a varint
/// `direction`, three `f32` cursor coordinates, and an `inside_block` boolean
/// (added 1.14, true when the player's head is inside the targeted block). It
/// does **not** carry the held item stack inline (contrast pre-1.13, which sent
/// a full `slot`): the server resolves the item from its own inventory view,
/// so placement needs no item registry. There is no block-prediction `sequence`
/// (added 1.19).
///
/// Using an item **in the air** is a separate [`UseItem`] packet in 1.14+, not a
/// sentinel `block_place` as in the legacy protocols.
///
/// Wire layout: varint hand, packed `position`, varint direction, three f32
/// cursor coordinates, bool inside-block.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_place", state = Play, bound = Server)]
pub struct BlockPlace {
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
    /// Target block position.
    pub location: Position,
    /// Face being placed against (`0..=5`).
    #[mc(varint)]
    pub direction: i32,
    /// Cursor X within the face (`0.0..=1.0`).
    pub cursor_x: f32,
    /// Cursor Y within the face (`0.0..=1.0`).
    pub cursor_y: f32,
    /// Cursor Z within the face (`0.0..=1.0`).
    pub cursor_z: f32,
    /// Whether the player's head is inside the targeted block.
    pub inside_block: bool,
}

/// Serverbound `use_item` — use the held item in the air (1.14+).
///
/// In the legacy protocols this was expressed as a sentinel `block_place`; from
/// 1.14 it is a dedicated packet carrying only the hand.
///
/// Wire layout: a single varint hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_item", state = Play, bound = Server)]
pub struct UseItem {
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
}

/// Serverbound `use_entity` for an **attack** (mouse `1`): no hand, no hit
/// location.
///
/// Wire layout: varint target, varint mouse, bool sneaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_entity", state = Play, bound = Server)]
pub struct UseEntity {
    /// Target entity id.
    #[mc(varint)]
    pub target: i32,
    /// Interaction kind (always `1`).
    #[mc(varint)]
    pub mouse: i32,
    /// Whether the player was sneaking. Added in 1.16 and appended, so 498
    /// and 578 send nothing here.
    #[mc(since = 754)]
    pub sneaking: bool,
}

/// Serverbound `use_entity` for a plain **interact** (mouse `0`).
///
/// # 1.9+ shape
///
/// 1.9+ added an off-hand, so the interact form carries a `hand` field; 1.16
/// appended a trailing `sneaking` boolean. Kept as a distinct struct so it
/// remains a plain derived struct rather than needing a `switch`-on-`mouse`.
///
/// Wire layout: varint target, varint mouse (`0`), varint hand, bool sneaking.
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
    /// Whether the player was sneaking. Added in 1.16 and appended, so 498
    /// and 578 send nothing here.
    #[mc(since = 754)]
    pub sneaking: bool,
}

/// Serverbound `use_entity` with a precise hit location (mouse `2`,
/// interact-at), carrying the hand (1.9+) and the 1.16 `sneaking` flag.
///
/// Wire layout: varint target, varint mouse (`2`), f32 x/y/z, varint hand, bool
/// sneaking.
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
    /// Whether the player was sneaking. Added in 1.16 and appended, so 498
    /// and 578 send nothing here.
    #[mc(since = 754)]
    pub sneaking: bool,
}

// `EntityAction` is byte-identical across v1-8/v1-9/v1-14 (measured), shared
// via `lodestone-protocol-common` -- see `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::EntityAction;

/// Serverbound `client_command` packet.
///
/// Wire layout: a single varint action id (`0` = perform respawn); same
/// shape at 1.8 and 1.12.2 per minecraft-data's `protocol.json`.
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

/// Clientbound `update_time` packet.
///
/// Wire layout: i64 world age, i64 time of day, verified against
/// minecraft-data's 1.16.2 `packet_update_time` (byte-identical to 1.12.2's
/// shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:update_time", state = Play, bound = Client)]
pub struct UpdateTime {
    /// Total world age, in ticks.
    pub age: i64,
    /// Current time of day, in ticks.
    pub time: i64,
}

/// Clientbound `difficulty` packet.
///
/// Wire layout: u8 difficulty, bool difficulty-locked — verified against
/// minecraft-data's 1.16.2 `packet_difficulty`. Unlike 1.12.2 (which has no
/// lock bit at all), 1.14+ appends `difficultyLocked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:difficulty", state = Play, bound = Client)]
pub struct DifficultyPacket {
    /// Raw difficulty id (`0` peaceful .. `3` hard).
    pub difficulty: u8,
    /// Whether the difficulty is locked from further changes in the UI.
    pub difficulty_locked: bool,
}

/// Clientbound `playerlist_header` packet — the tab list's header/footer.
///
/// Wire layout: two length-prefixed JSON strings, verified against
/// minecraft-data's 1.16.2 `packet_playerlist_header`.
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

/// Clientbound `open_sign_entity` packet — the server opened a sign-editing
/// UI for the local player.
///
/// Wire layout: a single packed [`Position`], verified against
/// minecraft-data's 1.16.2 `packet_open_sign_entity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:open_sign_entity", state = Play, bound = Client)]
pub struct OpenSignEntity {
    /// Block position of the sign.
    pub location: Position,
}

// AttachEntity/SetPassengers/Collect are byte-identical to v1-9's own
// definitions (measured), shared via `lodestone-protocol-common` ranged
// 340..=754 -- v1-8's `attach_entity` carries an extra `leash: bool` field and
// its `collect` has no pickup count, so neither is in this range. See
// `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::{AttachEntity, Collect, SetPassengers};

/// Clientbound `entity_effect` packet — a status effect was applied to an
/// entity.
///
/// Wire layout: verified against minecraft-data's 1.16.2
/// `packet_entity_effect`: VarInt entity id, raw `i8` legacy effect id, raw
/// `i8` amplifier, VarInt duration, raw `i8` flags byte. 1.16.5 postdates
/// 1.13, so unlike 1.12.2 the flags byte carries a third bit (`0x04` show
/// icon) alongside `0x01` ambient / `0x02` show particles — cross-checked
/// against `lodestone-v26-2`'s `UPDATE_MOB_EFFECT` decode, which uses the same
/// three low bits (plus a 1.19+ `0x08` blend bit this protocol predates).
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
    /// Flags byte: bit `0x01` ambient, bit `0x02` show particles, bit `0x04`
    /// show icon.
    pub flags: i8,
}

// `RemoveEntityEffect` is byte-identical to v1-9's own definition (measured),
// shared via `lodestone-protocol-common` ranged 340..=754 -- see
// `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::RemoveEntityEffect;

/// Serverbound `recipe_book` packet — toggle a recipe book's open/filtering
/// state, in its 1.16 (protocol 754) form.
///
/// [`CraftingBookData`] is the 498/578 equivalent. 1.16 split the older
/// packet's two actions into two packets, so the two are not the same wire
/// message with a field added: the old one leads with an action selector and
/// carries **all four** books at once.
///
/// Wire layout: varint book id (`RecipeBookType` ordinal: `0` crafting, `1`
/// furnace, `2` blast furnace, `3` smoker — all four exist by 1.16, unlike
/// 1.12.2 which has only the crafting book), then the open flag and filter
/// flag. Per minecraft-data's 1.16.2 `protocol.json` (`packet_recipe_book`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:recipe_book", state = Play, bound = Server, protocols = "754..=754")]
pub struct RecipeBook {
    /// `RecipeBookType` ordinal.
    #[mc(varint)]
    pub book_id: i32,
    /// Whether the book is open.
    pub book_open: bool,
    /// Whether the "only craftable" filter is active.
    pub filter_active: bool,
}

/// Serverbound `crafting_book_data` packet for 1.14.4 and 1.15.2 (protocols
/// 498, 578) — the pane state of **every** recipe book at once.
///
/// The pre-1.16 packet leads with an action selector, and this crate only
/// ever sends action `1` (the pane state); action `0` announces a displayed
/// recipe and became its own packet in 1.16, which this crate does not send
/// in either era.
///
/// Sending all four pairs is not a limitation of this port, it is the shape:
/// the client owns the whole recipe-book state and re-states it on every
/// change, so the adapter keeps that state and fills in the three panes the
/// caller did not name rather than defaulting them shut.
///
/// Wire layout: varint action (`1`), then eight bools — crafting open/filter,
/// smelting open/filter, blasting open/filter, smoking open/filter, in that
/// order, which is the `RecipeBookType` ordinal order [`RecipeBook`]'s
/// `book_id` also uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:crafting_book_data", state = Play, bound = Server, protocols = "498..=578")]
pub struct CraftingBookData {
    /// Action selector; `1` for the pane state this crate sends.
    #[mc(varint)]
    pub action: i32,
    /// Whether the crafting book is open.
    pub crafting_open: bool,
    /// Whether the crafting book's "only craftable" filter is active.
    pub crafting_filter: bool,
    /// Whether the furnace book is open.
    pub smelting_open: bool,
    /// Whether the furnace book's filter is active.
    pub smelting_filter: bool,
    /// Whether the blast-furnace book is open.
    pub blasting_open: bool,
    /// Whether the blast-furnace book's filter is active.
    pub blasting_filter: bool,
    /// Whether the smoker book is open.
    pub smoking_open: bool,
    /// Whether the smoker book's filter is active.
    pub smoking_filter: bool,
}
