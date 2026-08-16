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
/// Clientbound `entity_equipment` — one equipment slot changed on an entity.
///
/// Wire layout: varint entity id, signed `i16` equipment-slot ordinal
/// (`0` held item, `1` boots, `2` leggings, `3` chestplate, `4` helmet — 1.8
/// predates the off-hand slot modern versions insert at ordinal `1`, so this
/// ordinal table is *not* the same as [`lodestone_model::EquipmentSlot`]'s own
/// `from_ordinal`; the adapter maps it by hand), then a [`Slot`](super::slot::Slot).
/// Verified against minecraft-data's 1.8 `packet_entity_equipment`.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_equipment", state = Play, bound = Client)]
pub struct ClientboundEntityEquipment {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// 1.8 equipment-slot ordinal (see struct doc for the table).
    pub slot: i16,
    /// New item in the slot, or an empty [`Slot`](super::slot::Slot).
    pub item: super::slot::Slot,
}

/// Clientbound `animation` — a per-entity animation trigger (arm swing, hurt
/// flash, wake up, critical-hit particles).
///
/// Wire layout: varint entity id, raw animation-code byte. Verified against
/// minecraft-data's 1.8 `packet_animation` (identical shape at 1.12.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:animation", state = Play, bound = Client)]
pub struct Animation {
    /// Entity id performing the animation.
    #[mc(varint)]
    pub entity_id: i32,
    /// Raw animation code.
    pub animation: u8,
}

/// Clientbound `attach_entity` — 1.8 overloads one packet for both leashing
/// **and** mounting, distinguished by `leash`.
///
/// Wire layout: raw (non-varint) `i32` entity id, raw `i32` vehicle id, bool
/// leash — verified against minecraft-data's 1.8 `packet_attach_entity`.
/// **This is not the same shape as later versions**: 1.9 split mounting into
/// its own `set_passengers` packet and dropped `leash` from this one (two
/// `i32` fields only), so this struct must not be reused verbatim from a
/// sibling family. When `leash` is `true`, `entity_id` is the leashed entity
/// and `vehicle_id` is its holder (`-1` clears the leash). When `leash` is
/// `false`, `entity_id` is a passenger and `vehicle_id` is the vehicle it now
/// rides (`-1` dismounts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:attach_entity", state = Play, bound = Client)]
pub struct AttachEntity {
    /// Leashed/passenger entity id.
    pub entity_id: i32,
    /// Holder/vehicle entity id, or `-1`.
    pub vehicle_id: i32,
    /// `true` for a leash relation, `false` for a mount/ride relation.
    pub leash: bool,
}

/// Clientbound `collect` — an item (or arrow, or XP orb) entity was picked up
/// and should fly toward its collector before despawning.
///
/// Wire layout: varint collected-entity id, varint collector-entity id.
/// **1.8 carries no pickup count** — verified against minecraft-data's 1.8
/// `packet_collect`, which is exactly these two fields; 1.12.2 inserts a
/// third `pickupItemCount` varint that does not exist here. The adapter
/// supplies a documented placeholder for the canonical event's `amount`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:collect", state = Play, bound = Client)]
pub struct Collect {
    /// Collected (despawning) entity id.
    #[mc(varint)]
    pub collected_entity_id: i32,
    /// Collector entity id.
    #[mc(varint)]
    pub collector_entity_id: i32,
}

/// Clientbound `spawn_entity_weather` — spawns a lightning bolt.
///
/// Wire layout: varint entity id, signed byte type (always `1`, thunderbolt),
/// three fixed-point `i32` coordinates. Verified against minecraft-data's 1.8
/// `packet_spawn_entity_weather`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_weather", state = Play, bound = Client)]
pub struct SpawnEntityWeather {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Weather-entity type (always `1` for a thunderbolt).
    pub kind: i8,
    /// Fixed-point X (block units × 32).
    pub x: i32,
    /// Fixed-point Y (block units × 32).
    pub y: i32,
    /// Fixed-point Z (block units × 32).
    pub z: i32,
}

/// Clientbound `spawn_entity_experience_orb`.
///
/// Wire layout: varint entity id, three fixed-point `i32` coordinates, `i16`
/// orb value. Verified against minecraft-data's 1.8
/// `packet_spawn_entity_experience_orb`; 1.12.2 widens the coordinates to raw
/// `f64` (no fixed-point scale), another divergence this struct must not
/// inherit from a sibling family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_experience_orb", state = Play, bound = Client)]
pub struct SpawnEntityExperienceOrb {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Fixed-point X (block units × 32).
    pub x: i32,
    /// Fixed-point Y (block units × 32).
    pub y: i32,
    /// Fixed-point Z (block units × 32).
    pub z: i32,
    /// XP value carried by the orb.
    pub count: i16,
}

/// Clientbound `spawn_entity_painting`.
///
/// Wire layout: varint entity id, string title (motive), packed [`Position`]
/// (unlike most spawns' fixed-point coordinates — a painting sits on a block
/// grid), unsigned byte direction. Verified against minecraft-data's 1.8
/// `packet_spawn_entity_painting`. **1.8 carries no entity UUID** — 1.12.2
/// inserts one between `entityId` and `title` that this struct must not
/// carry.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_painting", state = Play, bound = Client)]
pub struct SpawnEntityPainting {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Motive/title name (unmapped to a canonical painting variant here).
    #[mc(max = 13)]
    pub title: String,
    /// Packed block position.
    pub location: super::position::Position,
    /// Facing direction (`0` south, `1` west, `2` north, `3` east).
    pub direction: u8,
}

/// Clientbound `entity_effect` — a potion/mob effect was applied or
/// refreshed.
///
/// Wire layout: varint entity id, signed byte legacy effect id, signed byte
/// amplifier, varint duration (ticks), bool `hideParticles`. Verified against
/// minecraft-data's 1.8 `packet_entity_effect`. **The trailing flag is a
/// single `bool`** — 1.12.2's same-named field is a bitmask byte carrying a
/// second (ambient) bit that does not exist on this wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_effect", state = Play, bound = Client)]
pub struct EntityEffect {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// 1-based legacy `minecraft:mob_effect` id.
    pub effect_id: i8,
    /// Effect amplifier (0 = level I).
    pub amplifier: i8,
    /// Remaining duration, in ticks.
    #[mc(varint)]
    pub duration: i32,
    /// Whether particles are hidden.
    pub hide_particles: bool,
}

/// Clientbound `remove_entity_effect`.
///
/// Wire layout: varint entity id, signed byte legacy effect id. Verified
/// against minecraft-data's 1.8 `packet_remove_entity_effect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:remove_entity_effect", state = Play, bound = Client)]
pub struct RemoveEntityEffect {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// 1-based legacy `minecraft:mob_effect` id.
    pub effect_id: i8,
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
