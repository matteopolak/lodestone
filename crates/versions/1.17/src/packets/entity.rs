//! Entity packets for protocol 754 that carry [`EntityMetadata`].
//!
//! Both the mob-spawn packet and the standalone metadata packet end with a
//! metadata list; because [`EntityMetadata`](super::metadata::EntityMetadata)
//! implements `Encode`/`Decode`, these are ordinary derived structs.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use super::metadata::EntityMetadata;

/// Clientbound `spawn_entity_living` — spawns a mob.
///
/// # 1.16 shape
///
/// Carries an **entity UUID**, a **VarInt type** and **`f64` coordinates**. The
/// trailing metadata list that pre-1.15 packets appended was **removed** in 1.15
/// — metadata now arrives only via the separate `entity_metadata` packet — so
/// this struct ends at the velocity components.
///
/// Wire layout: varint entity id, UUID, varint type, three `f64` coordinates,
/// signed-byte yaw/pitch/head-pitch, three `i16` velocity components.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_living", state = Play, bound = Client)]
pub struct SpawnEntityLiving {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Entity UUID.
    pub entity_uuid: Uuid,
    /// Mob type id (VarInt, 1.16 numbering).
    #[mc(varint)]
    pub kind: i32,
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
    /// Yaw as a signed-byte angle (`256` = 360°).
    pub yaw: i8,
    /// Pitch as a signed-byte angle.
    pub pitch: i8,
    /// Head pitch as a signed-byte angle.
    pub head_pitch: i8,
    /// Velocity X (fixed-point).
    pub velocity_x: i16,
    /// Velocity Y (fixed-point).
    pub velocity_y: i16,
    /// Velocity Z (fixed-point).
    pub velocity_z: i16,
}

/// Clientbound `entity_metadata` — an incremental metadata update for an entity.
///
/// The header is identical across versions (`varint entity id`, then the list);
/// only the list encoding differs, and that difference lives entirely in
/// [`EntityMetadata`](super::metadata::EntityMetadata).
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_metadata", state = Play, bound = Client)]
pub struct EntityMetadataPacket {
    /// Target entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// The changed metadata entries.
    pub metadata: EntityMetadata,
}

// RelEntityMove/EntityMoveLook/EntityTeleport are byte-identical to v1-9's
// own definitions (measured) but not to v1-8's (1.8 used a narrower
// signed-byte delta and fixed-point coordinates), so they are shared via
// `lodestone-protocol-common` ranged 340..=754. EntityLook/
// EntityVelocityPacket carry no such divergence and are shared across all
// three (v1-8 included) with the derive's default ProtocolRange::ALL -- see
// `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::{
    EntityLook, EntityMoveLook, EntityTeleport, EntityVelocityPacket, RelEntityMove,
};

/// Clientbound `spawn_entity` — spawns a non-living object entity.
///
/// # 1.16 shape
///
/// Carries a 128-bit `object_uuid`, a **VarInt `type`** (widened from the legacy
/// byte), `f64` coordinates, and sends `velocity` **unconditionally**. The
/// unconditional shape means this is an ordinary derived struct here.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity", state = Play, bound = Client)]
pub struct SpawnObject {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Entity UUID.
    pub object_uuid: Uuid,
    /// Object type id (VarInt, 1.16 entity-type numbering).
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
    /// Type-specific object data.
    pub object_data: i32,
    /// Velocity X in `1/8000` block/tick.
    pub velocity_x: i16,
    /// Velocity Y in `1/8000` block/tick.
    pub velocity_y: i16,
    /// Velocity Z in `1/8000` block/tick.
    pub velocity_z: i16,
}

/// Clientbound `named_entity_spawn` — spawns a player entity.
///
/// 1.16 sends the player UUID as a 128-bit value and, since 1.15, no longer
/// appends a metadata list (it arrives via the separate `entity_metadata`
/// packet), so this struct ends at the pitch angle.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:named_entity_spawn", state = Play, bound = Client)]
pub struct NamedEntitySpawn {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Player UUID.
    pub player_uuid: Uuid,
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
}

// `EntityDestroy` is byte-identical across v1-8/v1-9/v1-14 (measured), shared
// via `lodestone-protocol-common` -- see `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::EntityDestroy;

/// Clientbound `spawn_entity_experience_orb` — spawns an experience orb.
///
/// Wire layout: varint entity id, three `f64` coordinates, `i16` xp count —
/// verified against minecraft-data's 1.16.2 `packet_spawn_entity_experience_orb`
/// (byte-identical to 1.12.2's shape).
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_experience_orb", state = Play, bound = Client)]
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
