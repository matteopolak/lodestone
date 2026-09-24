//! Entity packets for protocol 340 that carry [`EntityMetadata`].
//!
//! Both the mob-spawn packet and the standalone metadata packet end with a
//! metadata list; because [`EntityMetadata`](super::metadata::EntityMetadata)
//! implements `Encode`/`Decode`, these are ordinary derived structs.

use lodestone_core::{Ctx, Error, Reader, Result, Writer};
use lodestone_macros::{Decode, Encode, Packet};
use uuid::Uuid;

use super::metadata::EntityMetadata;
use super::position::Position;

/// Clientbound `entity_update_attributes`.
///
/// The outer property count is a fixed `i32`, while each property's modifier
/// count is a VarInt. Both are decoded manually so a malformed packet cannot
/// choose either allocation size.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateAttributes {
    /// Entity whose attributes changed.
    pub entity_id: i32,
    /// Complete snapshots of the named properties included in this update.
    pub properties: Vec<AttributeProperty>,
}

/// One textual legacy attribute snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeProperty {
    /// Legacy dotted-camel-case key, such as `generic.maxHealth`.
    pub key: String,
    /// Base value before modifiers.
    pub value: f64,
    /// Modifiers applied to the base value.
    pub modifiers: Vec<AttributeModifier>,
}

/// One UUID-identified legacy attribute modifier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttributeModifier {
    /// Stable modifier identity on the wire.
    pub uuid: Uuid,
    /// Modifier amount.
    pub amount: f64,
    /// Operation ordinal: `0` add, `1` multiply base, `2` multiply total.
    pub operation: u8,
}

const MAX_ATTRIBUTE_ENTRIES: i32 = 128;
const MAX_ATTRIBUTE_MODIFIERS: i32 = 1_024;
const MAX_ATTRIBUTE_KEY: usize = 32_767;

fn checked_count(count: i32, cap: i32) -> Result<usize> {
    if count < 0 {
        return Err(Error::NegativeLength(count));
    }
    if count > cap {
        return Err(Error::LimitExceeded {
            limit: cap as usize,
            actual: count as usize,
        });
    }
    Ok(count as usize)
}

impl lodestone_core::Decode for UpdateAttributes {
    fn decode(reader: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let entity_id = reader.var_i32()?;
        let count = checked_count(reader.i32()?, MAX_ATTRIBUTE_ENTRIES)?;
        let mut properties = Vec::with_capacity(count);
        for _ in 0..count {
            let key = reader.string(MAX_ATTRIBUTE_KEY)?;
            let value = reader.f64()?;
            let modifier_count = checked_count(reader.var_i32()?, MAX_ATTRIBUTE_MODIFIERS)?;
            let mut modifiers = Vec::with_capacity(modifier_count);
            for _ in 0..modifier_count {
                let uuid = reader.uuid()?;
                let amount = reader.f64()?;
                let operation = reader.u8()?;
                if operation > 2 {
                    return Err(Error::InvalidEnumVariant {
                        name: "legacy attribute modifier operation",
                        value: i32::from(operation),
                    });
                }
                modifiers.push(AttributeModifier {
                    uuid,
                    amount,
                    operation,
                });
            }
            properties.push(AttributeProperty {
                key,
                value,
                modifiers,
            });
        }
        Ok(Self {
            entity_id,
            properties,
        })
    }
}

impl lodestone_core::Encode for UpdateAttributes {
    fn encode(&self, writer: &mut Writer, _ctx: Ctx) -> Result<()> {
        writer.var_i32(self.entity_id);
        let count = i32::try_from(self.properties.len()).map_err(|_| {
            Error::Custom(format!(
                "entity_update_attributes carries {} entries, which overflows the i32 count",
                self.properties.len()
            ))
        })?;
        checked_count(count, MAX_ATTRIBUTE_ENTRIES)?;
        writer.i32(count);
        for property in &self.properties {
            writer.string(&property.key);
            writer.f64(property.value);
            let modifier_count = i32::try_from(property.modifiers.len()).map_err(|_| {
                Error::Custom(format!(
                    "attribute {} carries {} modifiers, which overflows the VarInt count",
                    property.key,
                    property.modifiers.len()
                ))
            })?;
            checked_count(modifier_count, MAX_ATTRIBUTE_MODIFIERS)?;
            writer.var_i32(modifier_count);
            for modifier in &property.modifiers {
                writer.uuid(modifier.uuid);
                writer.f64(modifier.amount);
                writer.u8(modifier.operation);
            }
        }
        Ok(())
    }
}

impl lodestone_core::Packet for UpdateAttributes {
    const NAME: &'static str = "minecraft:entity_update_attributes";
    const STATE: lodestone_core::State = lodestone_core::State::Play;
    const BOUND: lodestone_core::Bound = lodestone_core::Bound::Client;
    const PROTOCOLS: lodestone_core::ProtocolRange = lodestone_core::ProtocolRange::new(110, 340);
}

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

/// Clientbound `spawn_entity_living` as protocols 110 and 210 send it: the
/// mob type is a single **unsigned byte**, not a VarInt.
///
/// 1.11 widened it when the mob id space outgrew 255. A VarInt read of a byte
/// field is the worst kind of wrong here — for every id below 128 the two
/// encodings coincide, so a shared struct would decode most of the vanilla
/// mob set correctly and silently misread the rest along with every field
/// after it. That is why this is a separate struct rather than a predicate.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(
    name = "minecraft:spawn_entity_living",
    state = Play,
    bound = Client,
    protocols = "110..=210"
)]
pub struct SpawnEntityLivingByteType {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Entity UUID.
    pub entity_uuid: Uuid,
    /// Mob type id (single byte in this era's first two releases).
    pub kind: u8,
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
// RelEntityMove/EntityMoveLook/EntityTeleport are byte-identical to v1-14's
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

/// Clientbound `spawn_entity_weather` — spawns a lightning bolt.
///
/// Wire layout: verified against minecraft-data's 1.12.2
/// `packet_spawn_entity_weather`: VarInt entity id, raw `i8` type (always `1`,
/// the only weather-entity type vanilla ever sends), three `f64` coordinates.
/// No UUID — 1.12.2 assigns weather entities none.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_weather", state = Play, bound = Client)]
pub struct SpawnEntityWeather {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Weather-entity type (always `1`, lightning bolt, at this protocol
    /// revision).
    pub kind: i8,
    /// X coordinate.
    pub x: f64,
    /// Y coordinate.
    pub y: f64,
    /// Z coordinate.
    pub z: f64,
}

/// Clientbound `spawn_entity_experience_orb` — spawns an experience orb.
///
/// Wire layout: verified against minecraft-data's 1.12.2
/// `packet_spawn_entity_experience_orb`: VarInt entity id, three `f64`
/// coordinates, raw `i16` XP count. No UUID.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
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
    /// XP value carried by this orb.
    pub count: i16,
}

/// Clientbound `spawn_entity_painting` — spawns a painting.
///
/// Wire layout: verified against minecraft-data's 1.12.2
/// `packet_spawn_entity_painting`: VarInt entity id, UUID, string title (the
/// legacy painting-motive name — this crate has no legacy motive→modern
/// `minecraft:painting_variant` crosswalk yet, so the adapter drops it and
/// spawns a plain `minecraft:painting`, same as any other spawn field this
/// crate does not yet fully model), a packed [`Position`], a raw `u8`
/// direction.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_painting", state = Play, bound = Client)]
pub struct SpawnEntityPainting {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Entity UUID.
    pub entity_uuid: Uuid,
    /// Legacy painting-motive name (dropped — see struct docs).
    #[mc(max = 32767)]
    pub title: String,
    /// Anchor block position.
    pub location: Position,
    /// Facing direction (`0` south, `1` west, `2` north, `3` east).
    pub direction: u8,
}

// `EntityDestroy` is byte-identical across v1-8/v1-9/v1-14 (measured), shared
// via `lodestone-protocol-common` -- see `packets::entity`'s module docs.
pub use lodestone_protocol_common::packets::entity::EntityDestroy;
