//! Entity packets for this era (protocol 766).
//!
//! # One spawn packet, and no player spawn at all
//!
//! The 1.19 era folded the mob-spawn packet into the object-spawn one. This
//! era finishes the job: the **player**-spawn packet is gone too, so every
//! entity a client learns about — object, mob and player alike — arrives
//! through `spawn_entity` carrying a varint type id into this era's unified
//! registry ([`crate::entity_types`]). A decoder ported from the era below
//! keeps an arm for a packet id that now names something else entirely.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use super::metadata::EntityMetadata;

/// Clientbound `entity_metadata` — an incremental metadata update.
///
/// The header is a varint entity id; the whole version divergence is in
/// [`EntityMetadata`](super::metadata::EntityMetadata)'s serializer table,
/// which this era renumbers.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_metadata", state = Play, bound = Client, protocols = "766..=766")]
pub struct EntityMetadataPacket {
    /// Target entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// The changed metadata entries.
    pub metadata: EntityMetadata,
}

/// Clientbound `spawn_entity` — spawns **any** entity, including players.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity", state = Play, bound = Client, protocols = "766..=766")]
pub struct SpawnObject {
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
    /// Velocity X in `1/8000` block/tick.
    pub velocity_x: i16,
    /// Velocity Y in `1/8000` block/tick.
    pub velocity_y: i16,
    /// Velocity Z in `1/8000` block/tick.
    pub velocity_z: i16,
}

/// Clientbound `spawn_entity_experience_orb`.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_experience_orb", state = Play, bound = Client, protocols = "766..=766")]
pub struct SpawnEntityExperienceOrb {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Experience count carried by this orb.
    pub count: i16,
}

/// Clientbound `rel_entity_move` — a short relative move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:rel_entity_move", state = Play, bound = Client, protocols = "766..=766")]
pub struct RelEntityMove {
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

/// Clientbound `entity_move_look` — a relative move with a new orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_move_look", state = Play, bound = Client, protocols = "766..=766")]
pub struct EntityMoveLook {
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

/// Clientbound `entity_look` — an orientation-only update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_look", state = Play, bound = Client, protocols = "766..=766")]
pub struct EntityLook {
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

/// Clientbound `entity_teleport` — an absolute position and orientation.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_teleport", state = Play, bound = Client, protocols = "766..=766")]
pub struct EntityTeleport {
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

/// Clientbound `entity_velocity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_velocity", state = Play, bound = Client, protocols = "766..=766")]
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

/// Clientbound `entity_head_rotation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_head_rotation", state = Play, bound = Client, protocols = "766..=766")]
pub struct EntityHeadRotation {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Head yaw as a signed-byte angle.
    pub head_yaw: i8,
}

/// Clientbound `entity_destroy` — a varint-counted list of entity ids.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_destroy", state = Play, bound = Client, protocols = "766..=766")]
pub struct EntityDestroy {
    /// Ids of the entities to remove.
    #[mc(varint)]
    pub entity_ids: Vec<i32>,
}

/// Clientbound `entity_status` — a one-byte event on an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_status", state = Play, bound = Client, protocols = "766..=766")]
pub struct EntityStatus {
    /// Entity id — a fixed `i32`, unlike most entity-id fields here.
    pub entity_id: i32,
    /// The status code.
    pub status: i8,
}

/// Clientbound `animation` — a one-byte animation on an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:animation", state = Play, bound = Client, protocols = "766..=766")]
pub struct EntityAnimation {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Animation id.
    pub animation: u8,
}

/// Clientbound `attach_entity` — leashes one entity to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:attach_entity", state = Play, bound = Client, protocols = "766..=766")]
pub struct AttachEntity {
    /// The leashed entity.
    pub entity_id: i32,
    /// The holder, or `-1` to detach.
    pub vehicle_id: i32,
}

/// Clientbound `set_passengers`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:set_passengers", state = Play, bound = Client, protocols = "766..=766")]
pub struct SetPassengers {
    /// The vehicle.
    #[mc(varint)]
    pub entity_id: i32,
    /// Its passengers, in order.
    #[mc(varint)]
    pub passengers: Vec<i32>,
}

/// Clientbound `collect` — an item or orb being picked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:collect", state = Play, bound = Client, protocols = "766..=766")]
pub struct Collect {
    /// The item entity being collected.
    #[mc(varint)]
    pub collected_entity_id: i32,
    /// The entity collecting it.
    #[mc(varint)]
    pub collector_entity_id: i32,
    /// How many items moved.
    #[mc(varint)]
    pub pickup_item_count: i32,
}
