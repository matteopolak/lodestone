//! Entity movement/lifecycle packets that carry no embedded
//! [`Position`](super::position::Position),
//! [`Slot`](super::slot::Slot) or `EntityMetadata`.
//!
//! [`EntityAction`], [`EntityDestroy`], [`EntityLook`] and
//! [`EntityVelocityPacket`] are byte-identical across every protocol these
//! three crates cover (47, 340, 754) -- measured against v1-14's own
//! definitions field for field -- and keep the derive's default
//! `ProtocolRange::ALL`.
//!
//! [`AttachEntity`], [`Collect`], [`EntityMoveLook`], [`EntityTeleport`],
//! [`RelEntityMove`], [`RemoveEntityEffect`], [`SetPassengers`] and
//! [`TeleportConfirm`] are shared only between v1-9 and v1-14 (declared
//! `#[mc(protocols = "340..=754")]`): each is a 1.9+ packet (offhand,
//! `f64` positions, wider relative-move deltas, or a packet 1.8 lacks
//! entirely), so v1-8 either has no equivalent or a genuinely different
//! shape and keeps its own definition.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `entity_action` (player command) -- sneak, sprint, leave bed, and
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

/// Clientbound `entity_destroy` -- a varint-counted list of varint entity ids to
/// remove.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_destroy", state = Play, bound = Client)]
pub struct EntityDestroy {
    /// Entity ids to remove.
    #[mc(varint)]
    pub entity_ids: Vec<i32>,
}

/// Clientbound `entity_look` -- a rotation-only update.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_look", state = Play, bound = Client)]
pub struct EntityLook {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Yaw as a signed-byte angle (`256` = 360°).
    pub yaw: i8,
    /// Pitch as a signed-byte angle.
    pub pitch: i8,
    /// Whether the entity is on the ground.
    pub on_ground: bool,
}

/// Clientbound `entity_velocity` -- a velocity update in `1/8000` block/tick.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_velocity", state = Play, bound = Client)]
pub struct EntityVelocityPacket {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Velocity X in `1/8000` block/tick.
    pub velocity_x: i16,
    /// Velocity Y in `1/8000` block/tick.
    pub velocity_y: i16,
    /// Velocity Z in `1/8000` block/tick.
    pub velocity_z: i16,
}

/// Clientbound `attach_entity` -- sets or clears an entity's leash holder.
/// Shared only 340..=754 -- see the module docs.
///
/// Wire layout: two raw (non-VarInt) `i32`s. A `vehicle_id` of `0` means "no
/// holder" -- the same sentinel the modern `SET_ENTITY_LINK` packet uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:attach_entity", state = Play, bound = Client, protocols = "340..=754")]
pub struct AttachEntity {
    /// Leashed entity id.
    pub entity_id: i32,
    /// Holder entity id, or `0` for "no holder".
    pub vehicle_id: i32,
}

/// Clientbound `set_passengers` -- the full passenger list of a vehicle.
/// Shared only 340..=754 -- see the module docs.
///
/// Wire layout: a VarInt vehicle id then a VarInt-counted VarInt array.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_passengers", state = Play, bound = Client, protocols = "340..=754")]
pub struct SetPassengers {
    /// Vehicle entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Passenger entity ids, in mounting order.
    #[mc(varint)]
    pub passengers: Vec<i32>,
}

/// Clientbound `collect` -- an item entity (or experience orb) was picked up.
/// Shared only 340..=754 -- see the module docs (1.8 carries no pickup
/// count).
///
/// Wire layout: three VarInts -- collected entity, collector, then amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:collect", state = Play, bound = Client, protocols = "340..=754")]
pub struct Collect {
    /// The entity that was picked up.
    #[mc(varint)]
    pub collected_entity_id: i32,
    /// The entity that did the picking up (usually a player).
    #[mc(varint)]
    pub collector_entity_id: i32,
    /// Stack size collected.
    #[mc(varint)]
    pub pickup_item_count: i32,
}

/// Clientbound `rel_entity_move` -- a small relative movement (no rotation).
/// Shared only 340..=754 -- see the module docs (1.8 used a narrower
/// signed-byte delta).
///
/// Wire layout: varint entity id, three `i16` deltas in `1/4096` block
/// units, boolean on-ground.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:rel_entity_move", state = Play, bound = Client, protocols = "340..=754")]
pub struct RelEntityMove {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// X delta in `1/4096` block units.
    pub dx: i16,
    /// Y delta in `1/4096` block units.
    pub dy: i16,
    /// Z delta in `1/4096` block units.
    pub dz: i16,
    /// Whether the entity is on the ground.
    pub on_ground: bool,
}

/// Clientbound `entity_move_look` -- a combined relative move and rotation.
/// Shared only 340..=754 -- see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_move_look", state = Play, bound = Client, protocols = "340..=754")]
pub struct EntityMoveLook {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// X delta in `1/4096` block units.
    pub dx: i16,
    /// Y delta in `1/4096` block units.
    pub dy: i16,
    /// Z delta in `1/4096` block units.
    pub dz: i16,
    /// Yaw as a signed-byte angle.
    pub yaw: i8,
    /// Pitch as a signed-byte angle.
    pub pitch: i8,
    /// Whether the entity is on the ground.
    pub on_ground: bool,
}

/// Clientbound `entity_teleport` -- an absolute position + rotation update.
/// Shared only 340..=754 -- see the module docs (1.8 sent fixed-point `i32`
/// coordinates, not `f64`).
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_teleport", state = Play, bound = Client, protocols = "340..=754")]
pub struct EntityTeleport {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Absolute X.
    pub x: f64,
    /// Absolute Y.
    pub y: f64,
    /// Absolute Z.
    pub z: f64,
    /// Yaw as a signed-byte angle.
    pub yaw: i8,
    /// Pitch as a signed-byte angle.
    pub pitch: i8,
    /// Whether the entity is on the ground.
    pub on_ground: bool,
}

/// Clientbound `remove_entity_effect`. Shared only 340..=754 -- see the
/// module docs.
///
/// Wire layout: varint entity id, signed byte legacy effect id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:remove_entity_effect",
    state = Play,
    bound = Client,
    protocols = "340..=754"
)]
pub struct RemoveEntityEffect {
    /// Target entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Legacy (1-based) potion-effect id.
    pub effect_id: i8,
}

/// Serverbound `teleport_confirm` -- echoes a clientbound position packet's
/// teleport id. Shared only 340..=754 -- see the module docs (1.8 has no
/// teleport confirmation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:teleport_confirm", state = Play, bound = Server, protocols = "340..=754")]
pub struct TeleportConfirm {
    /// Teleport id echoed from the clientbound position packet.
    #[mc(varint)]
    pub teleport_id: i32,
}
