//! Play-state packets for this era (protocol 762).

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use crate::packets::position::Position;

/// Clientbound `login` (game-join) packet for this era.
///
/// # What this packet decides
///
/// The vertical range of every column decoded afterwards is settled here, and
/// **not the way the era below settles it**. Through 1.18 this packet carried
/// the already-resolved dimension entry inline as a second NBT blob, so
/// `min_y` and `height` could be read straight out of it. At 762 that blob is
/// gone: what arrives is the whole registry (`dimension_codec`) plus the
/// *name* of the dimension type in use (`world_type`), and the vertical
/// window has to be looked up by that name inside the registry. An adapter
/// that keeps reading the old field reads a string where an NBT compound was
/// and desynchronises immediately; one that keeps the era below's default
/// window silently consumes the wrong number of bytes per column. See
/// [`ChunkShape::from_dimension_registry`](crate::packets::chunk::ChunkShape::from_dimension_registry).
///
/// * The current dimension is named twice: `world_type` is the **dimension
///   type** (`minecraft:overworld`, the key into `dimension_codec`) and
///   `world_name` is the **level** (also `minecraft:overworld` in vanilla,
///   but a data pack may make them differ).
/// * `game_mode` is a plain `u8` (0..=3); hardcore is a **separate** boolean
///   (`is_hardcore`).
/// * The trailing death location is 1.19's own addition: where the player
///   last died, so the client can point a recovery compass at it.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login", state = Play, bound = Client, protocols = "762..=762")]
pub struct JoinGame {
    /// Local player entity id.
    pub entity_id: i32,
    /// Whether the world is hardcore.
    pub is_hardcore: bool,
    /// Game mode (`0` survival, `1` creative, `2` adventure, `3` spectator).
    pub game_mode: u8,
    /// Previous game mode, or `-1` when there is none.
    pub previous_game_mode: i8,
    /// Names of every world on the server (namespaced identifiers).
    pub world_names: Vec<String>,
    /// Raw named-NBT registry of every dimension type, chat type and biome
    /// the server knows. This era reads the column's vertical window out of
    /// it, keyed by `world_type` — see the type docs.
    #[mc(nbt)]
    pub dimension_codec: Vec<u8>,
    /// Namespaced identifier of the **dimension type** in use, the key into
    /// `dimension_codec`. Replaces the inline dimension blob the era below
    /// carries.
    pub world_type: String,
    /// Namespaced identifier of the **level** the player is joining.
    pub world_name: String,
    /// Hashed world seed (for client-side biome noise).
    pub hashed_seed: i64,
    /// Legacy max-players hint.
    #[mc(varint)]
    pub max_players: i32,
    /// Server view distance in chunks.
    #[mc(varint)]
    pub view_distance: i32,
    /// Server simulation distance in chunks — how far entities tick.
    #[mc(varint)]
    pub simulation_distance: i32,
    /// Whether reduced debug info is in effect.
    pub reduced_debug_info: bool,
    /// Whether the respawn screen is shown on death.
    pub enable_respawn_screen: bool,
    /// Whether this is a debug world.
    pub is_debug: bool,
    /// Whether this is a superflat world.
    pub is_flat: bool,
    /// Whether a death location follows. 1.19's recovery-compass support.
    pub has_death_location: bool,
    /// Namespaced level the player last died in.
    #[mc(present_if = "has_death_location == true")]
    pub death_dimension: Option<String>,
    /// Block position the player last died at.
    #[mc(present_if = "has_death_location == true")]
    pub death_location: Option<Position>,
}

// The clientbound `chat` packet the four eras below carry does not exist at
// 762. 1.19 replaced it with three packets that differ in what they can be
// trusted to say -- signed player chat, unsigned server-decorated chat, and
// system text -- and they live in `packets::chat` with the serverbound
// signing machinery they belong with.

// The serverbound `chat` packet the eras below share is not reachable here
// either: at 762 sending a message means sending a timestamp, a salt, an
// optional signature and an acknowledgement window with it. See
// `packets::chat`.

/// Clientbound `position` (player position and look) packet.
///
/// # Architectural note
///
/// 1.8 introduced the relative-teleport `flags` byte, so every coordinate can
/// be absolute or relative. Bit `0x01` x, `0x02` y, `0x04` z, `0x08` yaw,
/// `0x10` pitch are relative when set.
///
/// Wire layout: f64 x/y/z, f32 yaw/pitch, signed byte flags, varint teleport
/// id. The trailing dismount-vehicle boolean the era below carries was
/// **removed** at 1.19 — a field disappearing at the end of a packet is
/// invisible to a decoder that keeps reading it only when something follows,
/// which is why this crate's own definition drops it rather than predicating
/// it.
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
    #[mc(varint)]
    pub teleport_id: i32,
}

/// Clientbound `game_state_change` — a compact, reason-keyed world update.
///
/// The reason is an unsigned byte and the argument is always an `f32`, even
/// when the selected reason interprets it as an ordinal.  Keeping the shared
/// frame here makes the adapter's reason-specific conversion subject to an
/// exact trailing-byte check.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:game_state_change", state = Play, bound = Client)]
pub struct GameStateChange {
    /// Reason selecting the argument's meaning.
    pub reason: u8,
    /// Reason-dependent argument.
    pub value: f32,
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
// ranged 110..=758 -- see `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::TeleportConfirm;

/// Clientbound `spawn_position` packet setting the client's compass target.
///
/// Wire layout: a packed [`Position`] then, from 1.17, an `f32` angle. This
/// is the crate's real use of the hand-written packed-position codec.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_position", state = Play, bound = Client)]
pub struct SpawnPosition {
    /// Compass target block position.
    pub location: Position,
    /// Angle the compass points from, in degrees. Added in 1.17, so present
    /// in every protocol this crate speaks.
    pub angle: f32,
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
/// Carries the same dimension-by-name shape [`JoinGame`] does: the inline NBT
/// dimension blob the era below reads is gone, replaced by the namespaced
/// **dimension type**. Respawning into a dimension of a different height
/// therefore re-resolves the column shape through the registry the join
/// packet delivered, not out of this packet.
///
/// Wire layout: string dimension type, string level name, i64 hashed seed,
/// i8 game mode, u8 previous game mode, bool is-debug, bool is-flat, bool
/// copy-metadata, then the optional death location 1.19 added.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:respawn", state = Play, bound = Client, protocols = "762..=762")]
pub struct Respawn {
    /// Namespaced identifier of the dimension type respawned into.
    pub world_type: String,
    /// Namespaced identifier of the level respawned into.
    pub world_name: String,
    /// Hashed world seed (retained so raw packet bytes can be replayed exactly).
    pub hashed_seed: i64,
    /// Game mode after respawn.
    pub game_mode: i8,
    /// Previous game mode.
    pub previous_game_mode: u8,
    /// Whether this is a debug world.
    pub is_debug: bool,
    /// Whether this is a superflat world.
    pub is_flat: bool,
    /// Whether to keep entity metadata / attributes across the respawn.
    pub copy_metadata: bool,
    /// Whether a death location follows.
    pub has_death_location: bool,
    /// Namespaced level the player last died in.
    #[mc(present_if = "has_death_location == true")]
    pub death_dimension: Option<String>,
    /// Block position the player last died at.
    #[mc(present_if = "has_death_location == true")]
    pub death_location: Option<Position>,
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
// byte-identical across the 1.8, 1.9, 1.14 and 1.17 eras (measured; raw f64/f32 fields, no
// embedded Position), shared via `lodestone-protocol-common` -- see
// `packets::movement`'s module docs.
pub use lodestone_protocol_common::packets::movement::{
    ServerboundLook, ServerboundPosition, ServerboundPositionLook,
};

// `ServerboundArmAnimation` is byte-identical to v1-9's (measured), shared
// via `lodestone-protocol-common` ranged 110..=758 -- 1.8 has no hand field
// at all. See `packets::chat`'s module docs.
pub use lodestone_protocol_common::packets::chat::ServerboundArmAnimation;

pub use lodestone_protocol_common::packets::movement::ServerboundFlying;

/// Serverbound `block_dig` (player digging) — start, cancel, or finish breaking
/// a block, plus drop / release / swap-hands status codes.
///
/// # The era's own addition
///
/// The wire shape matches 1.8 through the face byte, but **1.19 appends a
/// block-prediction `sequence`**: a monotonically increasing id the client
/// stamps on every world-changing action, which the server echoes back so the
/// client knows which of its optimistic predictions to keep. Every era below
/// this one drops the model's `sequence`; this is the first that can carry
/// it. Omitting it is not a silent shortening — the server reads a VarInt off
/// the next packet's length prefix — so this field is load-bearing.
///
/// Wire layout: varint status, packed `position`, signed-byte face, varint
/// sequence.
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
    /// Block-prediction sequence id — see the type docs.
    #[mc(varint)]
    pub sequence: i32,
}

/// Serverbound `block_place` (player block placement / item use on a block).
///
/// # 1.14+ shape
///
/// This era sends `hand` **first**, then the packed `position`, a varint
/// `direction`, three `f32` cursor coordinates, and an `inside_block` boolean
/// (added 1.14, true when the player's head is inside the targeted block). It
/// does **not** carry the held item stack inline (contrast pre-1.13, which sent
/// a full `slot`): the server resolves the item from its own inventory view,
/// so placement needs no item registry. 1.19 appends a block-prediction
/// `sequence` — see [`BlockDig`].
///
/// Using an item **in the air** is a separate [`UseItem`] packet in 1.14+, not a
/// sentinel `block_place` as in the legacy protocols.
///
/// Wire layout: varint hand, packed `position`, varint direction, three f32
/// cursor coordinates, bool inside-block, varint sequence.
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
    /// Block-prediction sequence id — see [`BlockDig`].
    #[mc(varint)]
    pub sequence: i32,
}

/// Serverbound `use_item` — use the held item in the air (1.14+).
///
/// In the legacy protocols this was expressed as a sentinel `block_place`; from
/// 1.14 it is a dedicated packet carrying only the hand.
///
/// Wire layout: varint hand, varint sequence (1.19's block-prediction id —
/// see [`BlockDig`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:use_item", state = Play, bound = Server)]
pub struct UseItem {
    /// Hand used (`0` main, `1` off).
    #[mc(varint)]
    pub hand: i32,
    /// Block-prediction sequence id — see [`BlockDig`].
    #[mc(varint)]
    pub sequence: i32,
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
    /// Whether the player was sneaking. Added in 1.16, so present in every
    /// protocol this crate speaks.
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
    /// Whether the player was sneaking. Added in 1.16, so present in every
    /// protocol this crate speaks.
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
    /// Whether the player was sneaking. Added in 1.16, so present in every
    /// protocol this crate speaks.
    pub sneaking: bool,
}

// `EntityAction` is byte-identical across the 1.8, 1.9, 1.14 and 1.17 eras
// (measured), shared
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
// 110..=758 -- the 1.8 era's `attach_entity` carries an extra `leash: bool` field and
// its `collect` has no pickup count, so neither is in this range. See
// `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::{AttachEntity, Collect, SetPassengers};

/// Clientbound `entity_effect` packet — a status effect was applied to an
/// entity.
///
/// Wire layout: VarInt entity id, VarInt effect id, raw `i8` amplifier,
/// VarInt duration, raw `i8` flags byte, then an optional named-NBT "factor
/// data" blob 1.19 added for the darkness effect's pulsing. The flags byte
/// carries `0x01` ambient, `0x02` show particles, `0x04` show icon and — new
/// at 1.19 — `0x08` blend.
///
/// The effect id is a **VarInt** here. The 1.17-era crate carries two structs
/// for exactly this field because 1.18 retyped it from a signed byte; by 762
/// only the VarInt form is reachable, so this era needs one.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_effect", state = Play, bound = Client, protocols = "762..=762")]
pub struct EntityEffect {
    /// Target entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Potion-effect id.
    #[mc(varint)]
    pub effect_id: i32,
    /// Effect amplifier (`0` = level I).
    pub amplifier: i8,
    /// Remaining duration, in ticks.
    #[mc(varint)]
    pub duration: i32,
    /// Flags byte: bit `0x01` ambient, `0x02` show particles, `0x04` show
    /// icon, `0x08` blend.
    pub flags: i8,
    /// Whether the trailing factor-data blob is present.
    pub has_factor_data: bool,
    /// Raw named-NBT factor data, empty when the flag above is unset.
    /// Carried verbatim rather than modelled: nothing above the adapter reads
    /// it, and skipping it by a guessed length would desynchronise the
    /// stream.
    #[mc(nbt, present_if = "has_factor_data == true")]
    pub factor_data: Vec<u8>,
}

/// Clientbound `remove_entity_effect`: VarInt entity id, VarInt effect id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:remove_entity_effect",
    state = Play,
    bound = Client,
    protocols = "762..=762"
)]
pub struct RemoveEntityEffect {
    /// Target entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Potion-effect id.
    #[mc(varint)]
    pub effect_id: i32,
}

/// Serverbound `recipe_book` packet — toggle a recipe book's open/filtering
/// state.
///
/// The era below carries two wire forms of this, because 1.16 split an
/// older action-selected packet in two; both protocols here are on the split
/// side, so there is one shape and no selector to branch on.
///
/// Wire layout: varint book id (`RecipeBookType` ordinal: `0` crafting, `1`
/// furnace, `2` blast furnace, `3` smoker), then the open flag and the
/// filter flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:recipe_book", state = Play, bound = Server, protocols = "762..=762")]
pub struct RecipeBook {
    /// `RecipeBookType` ordinal.
    #[mc(varint)]
    pub book_id: i32,
    /// Whether the book is open.
    pub book_open: bool,
    /// Whether the "only craftable" filter is active.
    pub filter_active: bool,
}
