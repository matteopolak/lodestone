//! Play-state packets for protocol 754 (Minecraft 1.16.5).

use lodestone_core::{Ctx, Reader, Result, Writer, read_named_nbt};
use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use crate::packets::position::Position;

/// Clientbound `login` (game-join) packet for 1.16.5.
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
/// Because the wire carries NBT (which the derive macro cannot express) and a
/// `worldNames` string array, this is a hand-written [`lodestone_core::Decode`]
/// (and a matching [`lodestone_core::Encode`] used only by hermetic tests, which
/// writes each NBT blob as a lone `TAG_End` — a valid empty named tag that the
/// decoder consumes symmetrically).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[mc(name = "minecraft:login", state = Play, bound = Client)]
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
    /// Namespaced identifier of the world the player is joining.
    pub world_name: String,
    /// Hashed world seed (for client-side biome noise).
    pub hashed_seed: i64,
    /// Legacy max-players hint.
    pub max_players: i32,
    /// Server view distance in chunks.
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

impl lodestone_core::Decode for JoinGame {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let entity_id = r.i32()?;
        let is_hardcore = r.bool()?;
        let game_mode = r.u8()?;
        let previous_game_mode = r.u8()?;
        let count = r.var_i32()?;
        if count < 0 {
            return Err(lodestone_core::Error::NegativeLength(count));
        }
        let mut world_names = Vec::with_capacity(count.min(1024) as usize);
        for _ in 0..count {
            world_names.push(r.string(32767)?);
        }
        read_named_nbt(r)?; // dimension_codec registry — consumed, not retained
        read_named_nbt(r)?; // current dimension type — consumed, not retained
        let world_name = r.string(32767)?;
        let hashed_seed = r.i64()?;
        let max_players = r.var_i32()?;
        let view_distance = r.var_i32()?;
        let reduced_debug_info = r.bool()?;
        let enable_respawn_screen = r.bool()?;
        let is_debug = r.bool()?;
        let is_flat = r.bool()?;
        Ok(Self {
            entity_id,
            is_hardcore,
            game_mode,
            previous_game_mode,
            world_names,
            world_name,
            hashed_seed,
            max_players,
            view_distance,
            reduced_debug_info,
            enable_respawn_screen,
            is_debug,
            is_flat,
        })
    }
}

impl lodestone_core::Encode for JoinGame {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        w.i32(self.entity_id);
        w.bool(self.is_hardcore);
        w.u8(self.game_mode);
        w.u8(self.previous_game_mode);
        w.var_i32(self.world_names.len() as i32);
        for name in &self.world_names {
            w.string(name);
        }
        w.u8(0); // dimension_codec: empty named NBT (TAG_End)
        w.u8(0); // dimension: empty named NBT (TAG_End)
        w.string(&self.world_name);
        w.i64(self.hashed_seed);
        w.var_i32(self.max_players);
        w.var_i32(self.view_distance);
        w.bool(self.reduced_debug_info);
        w.bool(self.enable_respawn_screen);
        w.bool(self.is_debug);
        w.bool(self.is_flat);
        Ok(())
    }
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
/// system, `2` action bar / game info), 128-bit sender uuid.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:chat", state = Play, bound = Client)]
pub struct ClientboundChat {
    /// JSON-encoded chat component.
    pub message: String,
    /// Chat slot: `0` chat, `1` system, `2` action bar.
    pub position: i8,
    /// UUID of the sending player (nil for system/server messages).
    pub sender: Uuid,
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

/// Clientbound `respawn` packet for 1.16.5.
///
/// Like [`JoinGame`], 1.16 replaced the numeric dimension with a namespaced
/// `world_name` string plus an inline **NBT** dimension type, so this is a
/// hand-written codec that consumes the NBT blob. It is not on the join-and-stay
/// critical path (respawn fires only on death / dimension change), but the shape
/// is migrated for correctness.
///
/// Wire layout: nbt dimension, string world name, i64 hashed seed, u8 game mode,
/// u8 previous game mode, bool is-debug, bool is-flat, bool copy-metadata.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[mc(name = "minecraft:respawn", state = Play, bound = Client)]
pub struct Respawn {
    /// Namespaced identifier of the world respawned into.
    pub world_name: String,
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

impl lodestone_core::Decode for Respawn {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        read_named_nbt(r)?; // dimension type — consumed, not retained
        let world_name = r.string(32767)?;
        let _hashed_seed = r.i64()?;
        let game_mode = r.u8()?;
        let previous_game_mode = r.u8()?;
        let is_debug = r.bool()?;
        let is_flat = r.bool()?;
        let copy_metadata = r.bool()?;
        Ok(Self {
            world_name,
            game_mode,
            previous_game_mode,
            is_debug,
            is_flat,
            copy_metadata,
        })
    }
}

impl lodestone_core::Encode for Respawn {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        w.u8(0); // dimension: empty named NBT (TAG_End)
        w.string(&self.world_name);
        w.i64(0); // hashed seed (not retained)
        w.u8(self.game_mode);
        w.u8(self.previous_game_mode);
        w.bool(self.is_debug);
        w.bool(self.is_flat);
        w.bool(self.copy_metadata);
        Ok(())
    }
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
    /// Whether the player was sneaking (added 1.16).
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
    /// Whether the player was sneaking (added 1.16).
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
    /// Whether the player was sneaking (added 1.16).
    pub sneaking: bool,
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
