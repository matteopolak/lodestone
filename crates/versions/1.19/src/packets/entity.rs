//! Entity packets for this era (protocol 762).
//!
//! # The era's defining change: one spawn packet
//!
//! Every era below this one carries a separate mob-spawn packet alongside the
//! object-spawn one. At 762 the mob packet is **gone**, and its head-rotation
//! byte was folded into the object packet — inserted *before* the object-data
//! field, which was itself widened from a fixed `i32` to a VarInt at the same
//! time. Two consequences worth stating because neither raises anything:
//!
//! * A decoder inherited from the era below reads the head-pitch byte as the
//!   first byte of the object data, then the object data's own bytes as the
//!   velocity, and produces a spawn at a plausible position with nonsense
//!   motion.
//! * Every mob on the wire now resolves through the generic spawn path and
//!   the unified entity registry. There is no longer a packet whose mere
//!   arrival means "this is a mob", so the entity kind is entirely a registry
//!   lookup — which is why [`crate::entity_types`] is per-era data.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use super::metadata::EntityMetadata;

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
// `lodestone-protocol-common` ranged 340..=758. EntityLook/
// EntityVelocityPacket carry no such divergence and are shared across all
// three (v1-8 included) with the derive's default ProtocolRange::ALL -- see
// `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::{
    EntityLook, EntityMoveLook, EntityTeleport, EntityVelocityPacket, RelEntityMove,
};

/// Clientbound `spawn_entity` — spawns **any** non-player entity, mob or
/// object.
///
/// Two fields separate this from the era below's, and the field order is the
/// half that matters: `head_pitch` is inserted *between* `yaw` and
/// `object_data`, and `object_data` is a VarInt rather than a fixed `i32`.
/// Neither is detectable by a round trip against our own encoder, so both are
/// pinned against a real join capture.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity", state = Play, bound = Client, protocols = "762..=762")]
pub struct SpawnObject {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Entity UUID.
    pub object_uuid: Uuid,
    /// Object type id (VarInt) into this era's unified entity registry.
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
    /// Head yaw as a signed-byte angle — folded in from the mob-spawn packet
    /// 1.19 removed. Meaningless for a true object; the server sends `0`.
    pub head_pitch: i8,
    /// Type-specific object data. A **VarInt** here, widened from the fixed
    /// `i32` the era below sends.
    #[mc(varint)]
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
/// The one spawn packet 1.19 did **not** fold into the generic path: a player
/// carries no type id, so it keeps its own packet. Unchanged from the era
/// below (measured).
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

// `EntityDestroy` is byte-identical across the 1.8, 1.9, 1.14 and 1.17 eras
// (measured), shared
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
