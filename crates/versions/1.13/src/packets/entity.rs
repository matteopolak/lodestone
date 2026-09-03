//! Entity packets for protocol 404 that carry [`EntityMetadata`].
//!
//! Three of them do at 404, not one: 1.13.2 still appends a metadata list to
//! `spawn_entity_living` and `named_entity_spawn` as well as sending the
//! standalone `entity_metadata`. 1.15 removed both trailers, which is why the
//! 1.14 era's equivalents of these two structs simply end earlier. Because
//! [`EntityMetadata`](super::metadata::EntityMetadata) implements
//! `Encode`/`Decode`, all three are ordinary derived structs.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use super::metadata::EntityMetadata;

/// Clientbound `spawn_entity_living` — spawns a mob.
///
/// # 1.13 shape
///
/// Carries an **entity UUID**, a **VarInt type** into the unified 1.13 entity
/// registry, **`f64` coordinates**, and a **trailing metadata list**. That
/// trailer is the difference from the era above: 1.15 removed it, so a 1.14-era
/// decoder applied here stops one list early and leaves the rest of the packet
/// unread.
///
/// Wire layout: varint entity id, UUID, varint type, three `f64` coordinates,
/// signed-byte yaw/pitch/head-pitch, three `i16` velocity components, metadata
/// list.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_living", state = Play, bound = Client)]
pub struct SpawnEntityLiving {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Entity UUID.
    pub entity_uuid: Uuid,
    /// Mob type id (VarInt, 1.13 unified-registry numbering).
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
    /// Initial data-watcher values. Removed from this packet in 1.15.
    pub metadata: EntityMetadata,
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
// and v1-14's own definitions (measured: none of the three appears in the
// 1.12.2 -> 1.13.2 or 1.13.2 -> 1.14.4 shape diff) but not to v1-8's (1.8
// used a narrower signed-byte delta and fixed-point coordinates), so they are
// shared via `lodestone-protocol-common` ranged 340..=754 -- a range that
// already covered 404. EntityLook/EntityVelocityPacket carry no such
// divergence and are shared across every family (v1-8 included) with the
// derive's default ProtocolRange::ALL -- see `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::{
    EntityLook, EntityMoveLook, EntityTeleport, EntityVelocityPacket, RelEntityMove,
};

/// Clientbound `spawn_entity` — spawns a non-living object entity.
///
/// # 1.13 shape
///
/// Carries a 128-bit `object_uuid`, a **signed-byte `type`** indexing the same
/// unified registry `spawn_entity_living` uses, `f64` coordinates, and sends
/// `velocity` **unconditionally**. The type field widened to a VarInt in 1.14;
/// below 128 the two encodings coincide byte for byte, which is exactly why
/// this one is worth stating rather than inheriting — 1.13.2's registry has 95
/// entries, so every id a real server sends fits in the overlap and no test
/// driven by real traffic can tell the two apart.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity", state = Play, bound = Client)]
pub struct SpawnObject {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Entity UUID.
    pub object_uuid: Uuid,
    /// Object type id (signed byte, 1.13 unified-registry numbering).
    pub kind: i8,
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
/// 1.13.2 sends the player UUID as a 128-bit value and **appends a metadata
/// list**; 1.15 removed that trailer.
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
    /// Initial data-watcher values. Removed from this packet in 1.15.
    pub metadata: EntityMetadata,
}

// `EntityDestroy` is byte-identical across v1-8/v1-9/v1-14 (measured), shared
// via `lodestone-protocol-common` -- see `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::EntityDestroy;

/// Clientbound `spawn_entity_experience_orb` — spawns an experience orb.
///
/// Wire layout: varint entity id, three `f64` coordinates, `i16` xp count —
/// verified against minecraft-data's 1.13.2
/// `packet_spawn_entity_experience_orb`, which the shape diff reports
/// unchanged from 1.12.2 through 1.16.5.
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
