//! Play-state packets for protocol 404 (Minecraft 1.13.2).

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use crate::packets::position::Position;
use crate::packets::slot::Slot;

/// Clientbound `block_action`: a block event with two block-defined bytes.
///
/// The trailing VarInt is a protocol-404 block *type* registry id rather than
/// a block-state id.  It selects the event family; the adapter leaves the two
/// parameter bytes untouched for the shell's animation consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_action", state = Play, bound = Client)]
pub struct BlockAction {
    /// Block at which the event occurred.
    pub location: Position,
    /// Block-defined event parameter.
    pub byte1: u8,
    /// Block-defined event parameter.
    pub byte2: u8,
    /// Protocol-404 block type registry id.
    #[mc(varint)]
    pub block_id: i32,
}

/// Clientbound `entity_equipment`: exactly one slot update in protocol 404.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_equipment", state = Play, bound = Client)]
pub struct ClientboundEntityEquipment {
    /// Entity whose equipment changed.
    #[mc(varint)]
    pub entity_id: i32,
    /// Equipment-slot ordinal.
    #[mc(varint)]
    pub slot: i32,
    /// Replacement stack, or empty for an explicit clear.
    pub item: Slot,
}

/// Clientbound `explosion` (protocol 404).
///
/// Coordinates and the player impulse are single-precision values on this
/// wire.  The offset records are signed bytes relative to the floored centre;
/// the count is a signed `i32`, so decoding validates it before allocating.
#[derive(Debug, Clone, PartialEq)]
pub struct Explosion {
    /// Explosion centre.
    pub x: f32,
    /// Explosion centre.
    pub y: f32,
    /// Explosion centre.
    pub z: f32,
    /// Blast radius.
    pub radius: f32,
    /// Removed-block offsets from the floored centre.
    pub affected_block_offsets: Vec<[i8; 3]>,
    /// Player knockback impulse, always present (zero means no impulse).
    pub player_motion_x: f32,
    /// Player knockback impulse, always present.
    pub player_motion_y: f32,
    /// Player knockback impulse, always present.
    pub player_motion_z: f32,
}

impl lodestone_core::Decode for Explosion {
    fn decode(r: &mut lodestone_core::Reader<'_>, _ctx: lodestone_core::Ctx) -> lodestone_core::Result<Self> {
        let x = r.f32()?;
        let y = r.f32()?;
        let z = r.f32()?;
        let radius = r.f32()?;
        let count = r.i32()?;
        if count < 0 {
            return Err(lodestone_core::Error::NegativeLength(count));
        }
        let count = usize::try_from(count).map_err(|_| lodestone_core::Error::UnexpectedEof)?;
        const MOTION_BYTES: usize = 12;
        let offset_bytes = r.remaining().checked_sub(MOTION_BYTES).ok_or(lodestone_core::Error::UnexpectedEof)?;
        if count > offset_bytes / 3 {
            return Err(lodestone_core::Error::LimitExceeded { limit: offset_bytes / 3, actual: count });
        }
        let mut affected_block_offsets = Vec::with_capacity(count);
        for _ in 0..count {
            affected_block_offsets.push([r.i8()?, r.i8()?, r.i8()?]);
        }
        Ok(Self {
            x,
            y,
            z,
            radius,
            affected_block_offsets,
            player_motion_x: r.f32()?,
            player_motion_y: r.f32()?,
            player_motion_z: r.f32()?,
        })
    }
}

impl lodestone_core::Encode for Explosion {
    fn encode(&self, w: &mut lodestone_core::Writer, _ctx: lodestone_core::Ctx) -> lodestone_core::Result<()> {
        w.f32(self.x);
        w.f32(self.y);
        w.f32(self.z);
        w.f32(self.radius);
        let count = i32::try_from(self.affected_block_offsets.len())
            .map_err(|_| lodestone_core::Error::Custom("too many explosion offsets".to_owned()))?;
        w.i32(count);
        for [x, y, z] in &self.affected_block_offsets {
            w.i8(*x);
            w.i8(*y);
            w.i8(*z);
        }
        w.f32(self.player_motion_x);
        w.f32(self.player_motion_y);
        w.f32(self.player_motion_z);
        Ok(())
    }
}

/// Clientbound `game_state_change`.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:game_state_change", state = Play, bound = Client)]
pub struct GameStateChange {
    /// Reason code (1/2 rain, 3 game mode, 7 rain strength, 8 thunder strength).
    pub reason: u8,
    /// Reason-specific value.
    pub value: f32,
}

/// Clientbound `block_break_animation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_break_animation", state = Play, bound = Client)]
pub struct BlockBreakAnimation {
    /// Entity performing the break animation.
    #[mc(varint)]
    pub entity_id: i32,
    /// Animated block position.
    pub location: Position,
    /// Raw destruction-stage byte.
    pub destroy_stage: i8,
}

/// Clientbound `login` (game-join) packet.
///
/// 1.13.2 sits between two rewrites of this packet and shares neither
/// neighbour's shape. 1.14 **removed** the `difficulty` byte (difficulty
/// moved to its own serverbound packet) and **inserted** a `view_distance`
/// VarInt before `reduced_debug_info`; 1.16 replaced the numeric dimension
/// with a world-name string plus two inline NBT blobs. So this is the 1.9-era
/// shape with nothing added: hardcore is still folded into the `0x8` bit of
/// the game-mode byte, and the world generator is still a bare `level_type`
/// string.
///
/// Getting the 1.14 shape's absence wrong does not error. Reading a
/// `view_distance` VarInt that was never sent consumes the
/// `reduced_debug_info` boolean, and the packet then ends one byte short --
/// which is why the committed capture, not a round trip, is what pins this.
///
/// Wire layout: i32 entity id, u8 game mode, i32 dimension, u8 difficulty,
/// u8 max players, string level type, bool reduced debug info.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:login", state = Play, bound = Client, protocols = "404..=404")]
pub struct JoinGame {
    /// Local player entity id.
    pub entity_id: i32,
    /// Game mode; the `0x8` bit flags hardcore.
    pub game_mode: u8,
    /// Numeric dimension: `-1` nether, `0` overworld, `1` end.
    pub dimension: i32,
    /// Server difficulty (`0` peaceful .. `3` hard). Removed from this packet
    /// in 1.14.
    pub difficulty: u8,
    /// Legacy max-players hint.
    pub max_players: u8,
    /// World generator name, such as `default` or `flat`.
    #[mc(max = 16)]
    pub level_type: String,
    /// Whether reduced debug info is in effect.
    pub reduced_debug_info: bool,
}

// `ClientboundChat` is byte-identical to v1-8's and v1-9's (measured: the
// packet is absent from the 1.12.2 -> 1.13.2 shape diff), shared via
// `lodestone-protocol-common` ranged 47..=404 -- 1.16 appended a `sender`
// UUID, which is where that range stops. The widening from 47..=340 landed
// with this era, checked by `tests/captures/join_1_13_2.txt`.
pub use lodestone_protocol_common::packets::chat::ClientboundChat;

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
    /// Added in 1.9; absent in 1.8.
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

// `SpawnPosition` is byte-identical to v1-8's and v1-9's (measured), shared
// via `lodestone-protocol-common` ranged 47..=404 -- it embeds the pre-1.14
// packed `Position`, which is where that range stops.
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
/// The pre-1.14 shape: a numeric dimension, a **difficulty byte** (dropped in
/// 1.14), the game mode, and a generator-name string. Not on the
/// join-and-stay critical path -- respawn fires only on death or a dimension
/// change -- but derived for correctness, and it is one of only two packets
/// where 1.14's removal of the difficulty byte is observable.
///
/// Wire layout: i32 dimension, u8 difficulty, u8 game mode, string level type.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:respawn", state = Play, bound = Client, protocols = "404..=404")]
pub struct Respawn {
    /// Numeric dimension: `-1` nether, `0` overworld, `1` end.
    pub dimension: i32,
    /// Server difficulty (`0` peaceful .. `3` hard). Removed from this packet
    /// in 1.14.
    pub difficulty: u8,
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

// `BlockDig` is byte-identical to v1-8's and v1-9's (measured), shared via
// `lodestone-protocol-common` ranged 47..=404 -- it embeds the pre-1.14
// packed `Position`. 1.9+ added status `6` (swap item in hands), which the
// adapter maps rather than rejecting as protocol 47 must.
pub use lodestone_protocol_common::packets::position::BlockDig;

/// Serverbound `block_place` (player block placement / item use on a block).
///
/// # 1.13 field order
///
/// 1.13.2 sends the packed `position` **first**, then a varint `direction`, a
/// varint `hand`, and three `f32` cursor coordinates. 1.14 moved `hand` to
/// the front and appended an `inside_block` boolean. A struct with 1.14's
/// order fed 1.13.2's bytes reads the low VarInt of the packed position as
/// the hand and desynchronises from there, so this is a field order that has
/// to be got right from the wire rather than inherited.
///
/// It does **not** carry the held item stack inline (contrast pre-1.13,
/// which sent a full `slot`): 1.13 is the release that dropped it, so the
/// server resolves the item from its own inventory view and placement needs
/// no item registry. There is no block-prediction `sequence` (added 1.19).
///
/// Using an item **in the air** is a separate [`UseItem`] packet from 1.9 on,
/// not a sentinel `block_place` as in 1.8.
///
/// Wire layout: packed `position`, varint direction, varint hand, three f32
/// cursor coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_place", state = Play, bound = Server)]
pub struct BlockPlace {
    /// Target block position.
    pub location: Position,
    /// Face being placed against (`0..=5`).
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

/// Serverbound `use_item` — use the held item in the air.
///
/// In 1.8 this was expressed as a sentinel `block_place`; from 1.9 it is a
/// dedicated packet carrying only the hand.
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
    /// Whether the player was sneaking. Added in 1.16 and appended, so 404
    /// sends nothing here; kept as a predicate rather than deleted because it
    /// is the field the model's interaction path already carries.
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
    /// Whether the player was sneaking. Added in 1.16 and appended, so 404
    /// sends nothing here; kept as a predicate rather than deleted because it
    /// is the field the model's interaction path already carries.
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
    /// Whether the player was sneaking. Added in 1.16 and appended, so 404
    /// sends nothing here; kept as a predicate rather than deleted because it
    /// is the field the model's interaction path already carries.
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
/// Wire layout: a single u8 difficulty. 1.14 appended a `difficultyLocked`
/// boolean when it made difficulty client-settable; 404 predates that, so a
/// 1.14-era decoder applied here reads one byte too many.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:difficulty", state = Play, bound = Client)]
pub struct DifficultyPacket {
    /// Raw difficulty id (`0` peaceful .. `3` hard).
    pub difficulty: u8,
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
/// Wire layout: verified against minecraft-data's 1.13.2
/// `packet_entity_effect`, which the shape diff reports unchanged from
/// 1.12.2 through 1.16.5: VarInt entity id, raw `i8` legacy effect id, raw
/// `i8` amplifier, VarInt duration, raw `i8` flags byte. The adapter reads
/// only `0x01` ambient and `0x02` show particles out of the flags byte; the
/// `0x04` show-icon bit later versions define is not one this crate claims
/// for 404, and the byte is carried whole either way.
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

// `RemoveEntityEffect` is byte-identical to v1-9's own definition (measured),
// shared via `lodestone-protocol-common` ranged 340..=754 -- see
// `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::RemoveEntityEffect;

/// Serverbound `crafting_book_data` packet — the pane state of every recipe
/// book at once.
///
/// The packet leads with an action selector, and this crate only ever sends
/// action `1` (the pane state); action `0` announces a displayed recipe and
/// became its own packet in 1.16, which this crate does not send.
///
/// **1.13.2 carries two books, not four.** The blast-furnace and smoker
/// books arrived with 1.14, so the 1.14-era version of this packet has eight
/// booleans where this has four. The four extra bytes a 1.14-shaped encoder
/// would append are not rejected by the server -- they are simply read as the
/// next packet -- so the count comes from `minecraft-data`'s 1.13.2
/// `crafting_book_data` and is checked live rather than assumed.
///
/// Sending both pairs is not a limitation of this port, it is the shape: the
/// client owns the whole recipe-book state and re-states it on every change,
/// so the adapter keeps that state and fills in the pane the caller did not
/// name rather than defaulting it shut.
///
/// Wire layout: varint action (`1`), then four bools — crafting open/filter,
/// smelting open/filter, in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:crafting_book_data", state = Play, bound = Server)]
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
}

#[cfg(test)]
mod tests {
    use super::{BlockBreakAnimation, Explosion, GameStateChange};
    use crate::packets::position::Position;
    use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};

    const CTX: Ctx = Ctx { version: 404 };

    #[test]
    fn explosion_wire_is_byte_exact_and_round_trips() {
        let value = Explosion {
            x: 1.5,
            y: -2.25,
            z: 3.25,
            radius: 2.0,
            affected_block_offsets: vec![[-1, 2, -3]],
            player_motion_x: 0.0,
            player_motion_y: 0.5,
            player_motion_z: -1.0,
        };
        let mut writer = Writer::default();
        value.encode(&mut writer, CTX).expect("encode");
        assert_eq!(
            writer.as_slice(),
            &[
                0x3f, 0xc0, 0x00, 0x00, 0xc0, 0x10, 0x00, 0x00, 0x40, 0x50, 0x00, 0x00,
                0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xff, 0x02, 0xfd, 0x00, 0x00,
                0x00, 0x00, 0x3f, 0x00, 0x00, 0x00, 0xbf, 0x80, 0x00, 0x00,
            ]
        );
        let mut reader = Reader::new(writer.as_slice());
        assert_eq!(Explosion::decode(&mut reader, CTX).expect("decode"), value);
        reader.ensure_empty().expect("no trailing bytes");
    }

    #[test]
    fn game_state_change_wire_is_reason_then_float() {
        let mut writer = Writer::default();
        GameStateChange { reason: 7, value: 0.25 }
            .encode(&mut writer, CTX)
            .expect("encode");
        assert_eq!(writer.as_slice(), &[7, 0x3e, 0x80, 0x00, 0x00]);
    }

    #[test]
    fn block_break_animation_uses_varint_position_stage() {
        let value = BlockBreakAnimation {
            entity_id: 300,
            location: Position::new(-1, 64, 2),
            destroy_stage: 9,
        };
        let mut writer = Writer::default();
        value.encode(&mut writer, CTX).expect("encode");
        assert_eq!(
            writer.as_slice(),
            &[0xac, 0x02, 0xff, 0xff, 0xff, 0xc1, 0x00, 0x00, 0x00, 0x02, 0x09]
        );
    }
}
