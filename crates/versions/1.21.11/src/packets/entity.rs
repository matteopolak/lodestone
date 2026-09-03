//! Entity packets for this era (protocol 774).
//!
//! # One spawn packet, and the velocity moved inside it
//!
//! Every entity a client learns about — object, mob and player alike — arrives
//! through [`AddEntity`] carrying a varint type id into this era's unified
//! registry ([`crate::entity_types`]).
//!
//! Its field order is **not** the 1.20.6 era's. There, the three velocity
//! shorts are the packet's tail, after the angle bytes and the type-specific
//! data varint; here they sit immediately after the position, before the
//! angles. The two orders consume the same number of bytes for the same
//! values, so a stale decoder raises no error at all: it reads the yaw byte as
//! part of a velocity short and spawns entities facing the wrong way, moving
//! in a direction nothing sent.
//!
//! # And one packet that no longer exists
//!
//! The dedicated experience-orb spawn packet is gone; orbs spawn through
//! [`AddEntity`] like everything else. Its old id now names a different
//! packet, so an adapter that keeps the arm decodes an unrelated body.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use super::metadata::EntityMetadata;

/// Clientbound `minecraft:set_entity_data` — an incremental metadata update.
///
/// The header is a varint entity id; the whole version divergence is in
/// [`EntityMetadata`](super::metadata::EntityMetadata)'s serializer table,
/// which this era renumbers.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_entity_data", state = Play, bound = Client, protocols = "774..=774")]
pub struct EntityMetadataPacket {
    /// Target entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// The changed metadata entries.
    pub metadata: EntityMetadata,
}

/// Clientbound `minecraft:add_entity` — spawns **any** entity, including
/// players.
///
/// See the module docs for the velocity-before-angles ordering, which is what
/// separates this from the era below.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:add_entity", state = Play, bound = Client, protocols = "774..=774")]
pub struct AddEntity {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Entity UUID.
    pub object_uuid: Uuid,
    /// Type id into this era's unified entity registry.
    #[mc(varint)]
    pub kind: i32,
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Velocity X in `1/8000` block/tick.
    pub velocity_x: i16,
    /// Velocity Y in `1/8000` block/tick.
    pub velocity_y: i16,
    /// Velocity Z in `1/8000` block/tick.
    pub velocity_z: i16,
    /// Pitch as a signed-byte angle.
    pub pitch: i8,
    /// Yaw as a signed-byte angle.
    pub yaw: i8,
    /// Head yaw as a signed-byte angle. Meaningless for a true object; the
    /// server sends `0`.
    pub head_pitch: i8,
    /// Type-specific object data.
    #[mc(varint)]
    pub object_data: i32,
}

/// Clientbound `minecraft:move_entity_pos` — a short relative move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_entity_pos", state = Play, bound = Client, protocols = "774..=774")]
pub struct MoveEntityPos {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// X delta in `1/4096` block.
    pub delta_x: i16,
    /// Y delta in `1/4096` block.
    pub delta_y: i16,
    /// Z delta in `1/4096` block.
    pub delta_z: i16,
    /// Whether the entity is on the ground.
    pub on_ground: bool,
}

/// Clientbound `minecraft:move_entity_pos_rot` — a relative move with a new
/// orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_entity_pos_rot", state = Play, bound = Client, protocols = "774..=774")]
pub struct MoveEntityPosRot {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// X delta in `1/4096` block.
    pub delta_x: i16,
    /// Y delta in `1/4096` block.
    pub delta_y: i16,
    /// Z delta in `1/4096` block.
    pub delta_z: i16,
    /// Yaw as a signed-byte angle.
    pub yaw: i8,
    /// Pitch as a signed-byte angle.
    pub pitch: i8,
    /// Whether the entity is on the ground.
    pub on_ground: bool,
}

/// Clientbound `minecraft:move_entity_rot` — orientation only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:move_entity_rot", state = Play, bound = Client, protocols = "774..=774")]
pub struct MoveEntityRot {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Yaw as a signed-byte angle.
    pub yaw: i8,
    /// Pitch as a signed-byte angle.
    pub pitch: i8,
    /// Whether the entity is on the ground.
    pub on_ground: bool,
}

/// Clientbound `minecraft:teleport_entity` — an absolute reposition with
/// byte-angle orientation.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:teleport_entity", state = Play, bound = Client, protocols = "774..=774")]
pub struct TeleportEntity {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Yaw as a signed-byte angle.
    pub yaw: i8,
    /// Pitch as a signed-byte angle.
    pub pitch: i8,
    /// Whether the entity is on the ground.
    pub on_ground: bool,
}

/// Clientbound `minecraft:entity_position_sync` — an absolute reposition that
/// also carries the entity's velocity and full-precision angles.
///
/// It has no counterpart in the 1.20.6 era. Servers send it for entities whose
/// interpolation the client must not smooth over, so an adapter that drops it
/// leaves those entities where the last relative move put them.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_position_sync", state = Play, bound = Client, protocols = "774..=774")]
pub struct EntityPositionSync {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// X velocity, blocks per tick.
    pub dx: f64,
    /// Y velocity, blocks per tick.
    pub dy: f64,
    /// Z velocity, blocks per tick.
    pub dz: f64,
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Whether the entity is on the ground.
    pub on_ground: bool,
}

/// Clientbound `minecraft:set_entity_motion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_entity_motion", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetEntityMotion {
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

/// Clientbound `minecraft:rotate_head` — the head yaw, which tracks
/// independently of the body yaw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:rotate_head", state = Play, bound = Client, protocols = "774..=774")]
pub struct RotateHead {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Head yaw as a signed-byte angle.
    pub head_yaw: i8,
}

/// Clientbound `minecraft:remove_entities`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:remove_entities", state = Play, bound = Client, protocols = "774..=774")]
pub struct RemoveEntities {
    /// Entity ids to forget.
    #[mc(varint)]
    pub entity_ids: Vec<i32>,
}

/// Clientbound `minecraft:entity_event` — a one-byte status code whose meaning
/// depends on the entity's own type (hurt, death, taming, and so on).
///
/// The entity id is a plain `i32`, not a varint: this is one of the few
/// entity packets that never adopted the varint form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_event", state = Play, bound = Client, protocols = "774..=774")]
pub struct EntityEvent {
    /// Entity id, fixed-width.
    pub entity_id: i32,
    /// Type-dependent status code.
    pub entity_status: i8,
}

/// Clientbound `minecraft:animate` — a one-shot entity animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:animate", state = Play, bound = Client, protocols = "774..=774")]
pub struct Animate {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Animation id (`0` swing main hand, `1` take damage, `2` leave bed,
    /// `3` swing off hand, `4` critical effect, `5` magic critical effect).
    pub animation: u8,
}

/// Clientbound `minecraft:set_entity_link` — leashing, and the only packet
/// that reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_entity_link", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetEntityLink {
    /// The leashed entity, fixed-width.
    pub entity_id: i32,
    /// The holder, or `-1` to unleash.
    pub vehicle_id: i32,
}

/// Clientbound `minecraft:set_passengers` — the complete passenger list of a
/// vehicle, replacing whatever the client had.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_passengers", state = Play, bound = Client, protocols = "774..=774")]
pub struct SetPassengers {
    /// The vehicle entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Every passenger, in seat order.
    #[mc(varint)]
    pub passengers: Vec<i32>,
}

/// Clientbound `minecraft:take_item_entity` — the pickup animation, and the
/// only packet that says *how many* items a pickup was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:take_item_entity", state = Play, bound = Client, protocols = "774..=774")]
pub struct TakeItemEntity {
    /// The item or experience-orb entity being collected.
    #[mc(varint)]
    pub collected_entity_id: i32,
    /// The entity collecting it.
    #[mc(varint)]
    pub collector_entity_id: i32,
    /// Stack size picked up.
    #[mc(varint)]
    pub pickup_item_count: i32,
}
