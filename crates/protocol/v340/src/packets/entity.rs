//! Entity packets for protocol 340 that carry [`EntityMetadata`].
//!
//! Both the mob-spawn packet and the standalone metadata packet end with a
//! metadata list; because [`EntityMetadata`](super::metadata::EntityMetadata)
//! implements `Encode`/`Decode`, these are ordinary derived structs.

use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use super::metadata::EntityMetadata;

/// Clientbound `spawn_entity_living` — spawns a mob with its initial metadata.
///
/// # 1.12 vs 1.8 divergence
///
/// 1.12 carries an **entity UUID**, a **VarInt type** and **`f64`
/// coordinates** — where 1.8 has no UUID, a `u8` type and fixed-point `i32`
/// coordinates. The trailing metadata uses the modern indexed format terminated
/// by `0xFF`.
///
/// Wire layout: varint entity id, UUID, varint type, three `f64` coordinates,
/// signed-byte yaw/pitch/head-pitch, three `i16` velocity components, then the
/// metadata list.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_living", state = Play, bound = Client)]
pub struct SpawnEntityLiving {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Entity UUID.
    pub entity_uuid: Uuid,
    /// Mob type id (VarInt, 1.12 numbering).
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
    /// Initial data-watcher metadata.
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

/// Clientbound `rel_entity_move` — a small relative movement (no rotation).
///
/// # 1.12.2 vs 1.8 divergence
///
/// 1.9+ encodes each delta as an `i16` in units of `1/4096` of a block (1.8
/// used a signed byte in `1/32` units), which is why this struct cannot be
/// shared with protocol 47.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:rel_entity_move", state = Play, bound = Client)]
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

/// Clientbound `entity_look` — a rotation-only update.
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

/// Clientbound `entity_move_look` — a combined relative move and rotation.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_move_look", state = Play, bound = Client)]
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

/// Clientbound `entity_teleport` — an absolute position + rotation update.
///
/// # 1.12.2 vs 1.8 divergence
///
/// 1.9+ sends the position as `f64` (1.8 used fixed-point `i32`, block × 32).
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_teleport", state = Play, bound = Client)]
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

/// Clientbound `entity_velocity` — a velocity update in `1/8000` block/tick.
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

/// Clientbound `spawn_entity` — spawns a non-living object entity.
///
/// # 1.12 vs 1.8 divergence
///
/// 1.12 adds a 128-bit `object_uuid`, uses `f64` coordinates, and — crucially —
/// sends the `velocity` **unconditionally**, whereas 1.8 gates it behind a
/// non-zero `object_data` switch. The unconditional shape means this is an
/// ordinary derived struct here (1.8's cannot be).
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity", state = Play, bound = Client)]
pub struct SpawnObject {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Entity UUID.
    pub object_uuid: Uuid,
    /// Object type id (1.12 object numbering).
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
/// 1.12 sends the player UUID as a 128-bit value (only Login Success uses the
/// string form) and drops 1.8's `current_item` field. The trailing metadata is
/// consumed by the derived [`EntityMetadata`] codec.
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
    /// Initial data-watcher metadata.
    pub metadata: EntityMetadata,
}

/// Clientbound `entity_destroy` — a varint-counted list of varint entity ids to
/// remove.
///
/// Previously hand-decoded because the derive macro could not express a `Vec`
/// whose *elements* are varints (only the length prefix was varint). That gap
/// was reported to `lodestone-macros` and closed: `#[mc(varint)]` on a
/// `Vec<i32>` now encodes the length **and** each element as a varint, so this
/// is an ordinary derived struct.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_destroy", state = Play, bound = Client)]
pub struct EntityDestroy {
    /// Entity ids to remove.
    #[mc(varint)]
    pub entity_ids: Vec<i32>,
}
