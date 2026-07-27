//! Entity packets for protocol 47 that carry [`EntityMetadata`].
//!
//! Both the mob-spawn packet and the standalone metadata packet end with a
//! metadata list; because [`EntityMetadata`](super::metadata::EntityMetadata)
//! implements `Encode`/`Decode`, these are ordinary derived structs.

use lodestone_macros::{Decode, Encode, Packet};

use super::metadata::EntityMetadata;

/// Clientbound `spawn_entity_living` — spawns a mob with its initial metadata.
///
/// # 1.8 vs modern divergence
///
/// 1.8 sends the entity **type as a `u8`**, coordinates as **fixed-point
/// `i32`** (block units × 32), and **no entity UUID** — modern versions send a
/// UUID, a varint type and `f64` coordinates. The trailing metadata uses the
/// legacy `(type << 5) | key` byte-keyed format terminated by `0x7F`.
///
/// Wire layout: varint entity id, unsigned byte type, three fixed-point `i32`
/// coordinates, signed-byte yaw/pitch/head-pitch, three `i16` velocity
/// components, then the metadata list.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_living", state = Play, bound = Client)]
pub struct SpawnEntityLiving {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Mob type id (1.8 numbering).
    pub kind: u8,
    /// Fixed-point X (block units × 32).
    pub x: i32,
    /// Fixed-point Y (block units × 32).
    pub y: i32,
    /// Fixed-point Z (block units × 32).
    pub z: i32,
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

/// Clientbound `spawn_entity` — spawns a non-living "object" entity (item,
/// projectile, minecart, …).
///
/// # 1.8 wire layout and the conditional velocity tail
///
/// varint entity id, signed-byte type, three fixed-point `i32` coordinates,
/// signed-byte pitch then yaw, a signed `i32` `object_data`, and finally three
/// `i16` velocity components **that are present only when `object_data != 0`**.
/// That head-dependent tail is expressed with `#[mc(when = ...)]` on
/// `Option` velocity fields: decode yields `Some` only when the predicate holds
/// and `None` otherwise, and encode requires the values to be present when the
/// predicate is true (so a contradictory `object_data != 0` with a `None`
/// velocity is a hard error rather than a silent zero).
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity", state = Play, bound = Client)]
pub struct SpawnObject {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Object type id (1.8 object numbering, distinct from the mob table).
    pub kind: i8,
    /// Fixed-point X (block units × 32).
    pub x: i32,
    /// Fixed-point Y (block units × 32).
    pub y: i32,
    /// Fixed-point Z (block units × 32).
    pub z: i32,
    /// Pitch as a signed-byte angle.
    pub pitch: i8,
    /// Yaw as a signed-byte angle.
    pub yaw: i8,
    /// Object data (meaning depends on the object type); `0` means "no velocity
    /// follows".
    pub object_data: i32,
    /// Velocity X (fixed-point) — present only when `object_data != 0`.
    #[mc(when = "object_data != 0")]
    pub velocity_x: Option<i16>,
    /// Velocity Y (fixed-point) — present only when `object_data != 0`.
    #[mc(when = "object_data != 0")]
    pub velocity_y: Option<i16>,
    /// Velocity Z (fixed-point) — present only when `object_data != 0`.
    #[mc(when = "object_data != 0")]
    pub velocity_z: Option<i16>,
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
/// # 1.8 vs modern divergence
///
/// 1.8 encodes each delta as a **signed byte** in units of `1/32` of a block,
/// so the maximum single step is 4 blocks. 1.9+ widened these to `i16` in units
/// of `1/4096`, which is why this struct cannot be shared with 340/770.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:rel_entity_move", state = Play, bound = Client)]
pub struct RelEntityMove {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// X delta in `1/32` block units.
    pub dx: i8,
    /// Y delta in `1/32` block units.
    pub dy: i8,
    /// Z delta in `1/32` block units.
    pub dz: i8,
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
    /// X delta in `1/32` block units.
    pub dx: i8,
    /// Y delta in `1/32` block units.
    pub dy: i8,
    /// Z delta in `1/32` block units.
    pub dz: i8,
    /// Yaw as a signed-byte angle.
    pub yaw: i8,
    /// Pitch as a signed-byte angle.
    pub pitch: i8,
    /// Whether the entity is on the ground.
    pub on_ground: bool,
}

/// Clientbound `entity_teleport` — an absolute position + rotation update.
///
/// # 1.8 vs modern divergence
///
/// 1.8 sends the position as **fixed-point `i32`** (block units × 32); 1.9+
/// switched to `f64`. The angle bytes are unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_teleport", state = Play, bound = Client)]
pub struct EntityTeleport {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Fixed-point X (block units × 32).
    pub x: i32,
    /// Fixed-point Y (block units × 32).
    pub y: i32,
    /// Fixed-point Z (block units × 32).
    pub z: i32,
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
