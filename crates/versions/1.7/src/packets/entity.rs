//! Entity packets for protocol 5.
//!
//! # Entity ids are `i32`, not varints
//!
//! Only the four spawn packets carry a varint entity id in this era. Every
//! other entity packet — movement, look, teleport, velocity, metadata,
//! effects, equipment, head rotation, status, attach, collect — carries a
//! full `i32`. Protocol 47 converted most of them to varints, so a decoder
//! from that era reads a one-byte varint where four bytes sit and mis-frames
//! everything after it. For an entity id under 128 the first byte is the
//! whole id and the remaining three are zero, which then parse as a valid
//! (wrong) next field rather than erroring.
//!
//! # Two id spaces, and the packet tells you which
//!
//! `spawn_entity` carries an `i8` **object** type; `spawn_entity_living`
//! carries a `u8` **mob** type; `named_entity_spawn` carries no type at all
//! and is always a player. The spaces overlap — 50 is primed TNT as an object
//! and a creeper as a mob — so [`crate::entity_types`] resolves them through
//! two independent tables. Both were read off a real server's wire; see
//! `tests/entity_types.rs`.

use lodestone_macros::{Decode, Encode, Packet};

use super::metadata::EntityMetadata;
use super::position::PositionIii;
use super::slot::Slot;

/// Clientbound `spawn_entity`: a non-living entity.
///
/// # The conditional velocity tail
///
/// `object_data` is an `i32` whose meaning depends on the object type — for a
/// falling block it is the block id, for a thrown item it is the thrower's
/// entity id. When it is **non-zero** the packet carries three trailing
/// `i16` velocity components; when it is zero it stops. That makes the
/// packet's length depend on a field's *value*, which no declarative schema
/// here can express, so the tail is decoded by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnObject {
    /// Entity id.
    pub entity_id: i32,
    /// Object type id, resolved through the object table.
    pub kind: i8,
    /// Fixed-point x, 32 units per block.
    pub x: i32,
    /// Fixed-point y.
    pub y: i32,
    /// Fixed-point z.
    pub z: i32,
    /// Packed pitch.
    pub pitch: i8,
    /// Packed yaw.
    pub yaw: i8,
    /// Type-dependent payload; non-zero means velocity follows.
    pub object_data: i32,
    /// Velocity, present only when `object_data` is non-zero.
    pub velocity: Option<(i16, i16, i16)>,
}

impl lodestone_core::Decode for SpawnObject {
    fn decode(
        reader: &mut lodestone_core::Reader<'_>,
        _ctx: lodestone_core::Ctx,
    ) -> lodestone_core::Result<Self> {
        let entity_id = reader.var_i32()?;
        let kind = reader.i8()?;
        let x = reader.i32()?;
        let y = reader.i32()?;
        let z = reader.i32()?;
        let pitch = reader.i8()?;
        let yaw = reader.i8()?;
        let object_data = reader.i32()?;
        let velocity = if object_data == 0 {
            None
        } else {
            Some((reader.i16()?, reader.i16()?, reader.i16()?))
        };
        Ok(Self {
            entity_id,
            kind,
            x,
            y,
            z,
            pitch,
            yaw,
            object_data,
            velocity,
        })
    }
}

impl lodestone_core::Encode for SpawnObject {
    fn encode(
        &self,
        writer: &mut lodestone_core::Writer,
        _ctx: lodestone_core::Ctx,
    ) -> lodestone_core::Result<()> {
        writer.var_i32(self.entity_id);
        writer.i8(self.kind);
        writer.i32(self.x);
        writer.i32(self.y);
        writer.i32(self.z);
        writer.i8(self.pitch);
        writer.i8(self.yaw);
        writer.i32(self.object_data);
        if self.object_data != 0 {
            let (vx, vy, vz) = self.velocity.unwrap_or((0, 0, 0));
            writer.i16(vx);
            writer.i16(vy);
            writer.i16(vz);
        }
        Ok(())
    }
}

impl lodestone_core::Packet for SpawnObject {
    const NAME: &'static str = "minecraft:spawn_entity";
    const STATE: lodestone_core::State = lodestone_core::State::Play;
    const BOUND: lodestone_core::Bound = lodestone_core::Bound::Client;
    const PROTOCOLS: lodestone_core::ProtocolRange = lodestone_core::ProtocolRange::new(5, 5);
}

/// Clientbound `spawn_entity_living`: a mob.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_living", state = Play, bound = Client)]
pub struct SpawnEntityLiving {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Mob type id, resolved through the mob table.
    pub kind: u8,
    /// Fixed-point x, 32 units per block.
    pub x: i32,
    /// Fixed-point y.
    pub y: i32,
    /// Fixed-point z.
    pub z: i32,
    /// Packed yaw.
    pub yaw: i8,
    /// Packed pitch.
    pub pitch: i8,
    /// Packed head yaw.
    pub head_pitch: i8,
    /// Velocity x, in 1/8000 blocks per tick.
    pub velocity_x: i16,
    /// Velocity y.
    pub velocity_y: i16,
    /// Velocity z.
    pub velocity_z: i16,
    /// Initial data-watcher values.
    pub metadata: EntityMetadata,
}

/// Clientbound `named_entity_spawn`: another player.
///
/// # The UUID is a string here
///
/// The profile UUID arrives as a dashed 36-character string, not the 16-byte
/// binary form later protocols use. This is the *only* packet in the era that
/// carries a remote player's UUID at all — `player_info` does not — so it is
/// also the only place a name can be tied to an identity (see
/// [`crate::packets::player_info`]).
///
/// The profile properties array is present but empty on an offline-mode
/// server, and is where a skin would arrive on an online-mode one.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:named_entity_spawn", state = Play, bound = Client)]
pub struct NamedEntitySpawn {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Dashed profile UUID string.
    #[mc(max = 36)]
    pub player_uuid: String,
    /// Player name.
    #[mc(max = 16)]
    pub player_name: String,
    /// Signed profile properties; empty in offline mode.
    #[mc(len = "varint")]
    pub properties: Vec<ProfilePropertyEntry>,
    /// Fixed-point x.
    pub x: i32,
    /// Fixed-point y.
    pub y: i32,
    /// Fixed-point z.
    pub z: i32,
    /// Packed yaw.
    pub yaw: i8,
    /// Packed pitch.
    pub pitch: i8,
    /// Held item id, `0` for empty.
    pub current_item: i16,
    /// Initial data-watcher values.
    pub metadata: EntityMetadata,
}

/// One signed profile property in a [`NamedEntitySpawn`].
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct ProfilePropertyEntry {
    /// Property name, such as `textures`.
    #[mc(max = 32767)]
    pub name: String,
    /// Base64 property value.
    #[mc(max = 32767)]
    pub value: String,
    /// Mojang's signature over the value.
    #[mc(max = 32767)]
    pub signature: String,
}

/// Clientbound `spawn_entity_painting`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_painting", state = Play, bound = Client)]
pub struct SpawnEntityPainting {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Motive name, such as `Kebab`.
    #[mc(max = 13)]
    pub title: String,
    /// Block the painting hangs on.
    pub location: PositionIii,
    /// Facing ordinal.
    pub direction: i32,
}

/// Clientbound `spawn_entity_experience_orb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_experience_orb", state = Play, bound = Client)]
pub struct SpawnEntityExperienceOrb {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Fixed-point x.
    pub x: i32,
    /// Fixed-point y.
    pub y: i32,
    /// Fixed-point z.
    pub z: i32,
    /// Experience carried.
    pub count: i16,
}

/// Clientbound `spawn_entity_weather`: a lightning bolt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_entity_weather", state = Play, bound = Client)]
pub struct SpawnEntityWeather {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Weather kind; `1` is a lightning bolt.
    pub kind: i8,
    /// Fixed-point x.
    pub x: i32,
    /// Fixed-point y.
    pub y: i32,
    /// Fixed-point z.
    pub z: i32,
}

/// Clientbound `entity`: a no-op keep-tracking packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity", state = Play, bound = Client)]
pub struct EntityTick {
    /// Entity id.
    pub entity_id: i32,
}

/// Clientbound `rel_entity_move`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:rel_entity_move", state = Play, bound = Client)]
pub struct RelEntityMove {
    /// Entity id.
    pub entity_id: i32,
    /// Delta x in 1/32 blocks.
    pub d_x: i8,
    /// Delta y in 1/32 blocks.
    pub d_y: i8,
    /// Delta z in 1/32 blocks.
    pub d_z: i8,
}

/// Clientbound `entity_look`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_look", state = Play, bound = Client)]
pub struct EntityLook {
    /// Entity id.
    pub entity_id: i32,
    /// Packed yaw.
    pub yaw: i8,
    /// Packed pitch.
    pub pitch: i8,
}

/// Clientbound `entity_move_look`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_move_look", state = Play, bound = Client)]
pub struct EntityMoveLook {
    /// Entity id.
    pub entity_id: i32,
    /// Delta x in 1/32 blocks.
    pub d_x: i8,
    /// Delta y in 1/32 blocks.
    pub d_y: i8,
    /// Delta z in 1/32 blocks.
    pub d_z: i8,
    /// Packed yaw.
    pub yaw: i8,
    /// Packed pitch.
    pub pitch: i8,
}

/// Clientbound `entity_teleport`.
///
/// The coordinates are fixed-point, 32 units per block. Protocol 110 makes
/// them doubles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_teleport", state = Play, bound = Client)]
pub struct EntityTeleport {
    /// Entity id.
    pub entity_id: i32,
    /// Fixed-point x.
    pub x: i32,
    /// Fixed-point y.
    pub y: i32,
    /// Fixed-point z.
    pub z: i32,
    /// Packed yaw.
    pub yaw: i8,
    /// Packed pitch.
    pub pitch: i8,
}

/// Clientbound `entity_head_rotation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_head_rotation", state = Play, bound = Client)]
pub struct EntityHeadRotation {
    /// Entity id.
    pub entity_id: i32,
    /// Packed head yaw.
    pub head_yaw: i8,
}

/// Clientbound `entity_velocity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_velocity", state = Play, bound = Client)]
pub struct EntityVelocityPacket {
    /// Entity id.
    pub entity_id: i32,
    /// Velocity x in 1/8000 blocks per tick.
    pub velocity_x: i16,
    /// Velocity y.
    pub velocity_y: i16,
    /// Velocity z.
    pub velocity_z: i16,
}

/// Clientbound `entity_destroy`.
///
/// The array is prefixed with a single byte count, not a varint.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_destroy", state = Play, bound = Client)]
pub struct EntityDestroy {
    /// Entity ids leaving the client's view.
    #[mc(len = "u8")]
    pub entity_ids: Vec<i32>,
}

/// Clientbound `entity_metadata`.
///
/// Carries no entity type, so the receiver must remember what each id is
/// from its spawn packet. Without that, a data-watcher index cannot be
/// interpreted: index 12 is a baby flag on one mob and a variant on another.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_metadata", state = Play, bound = Client)]
pub struct EntityMetadataPacket {
    /// Entity id.
    pub entity_id: i32,
    /// Changed data-watcher values only.
    pub metadata: EntityMetadata,
}

/// Clientbound `entity_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_status", state = Play, bound = Client)]
pub struct EntityStatus {
    /// Entity id.
    pub entity_id: i32,
    /// Status ordinal.
    pub entity_status: i8,
}

/// Clientbound `attach_entity`.
///
/// `leash` distinguishes a lead from a mount. Protocol 47 keeps the same
/// shape; protocol 110 replaces it with a passengers list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:attach_entity", state = Play, bound = Client)]
pub struct AttachEntity {
    /// Entity being attached.
    pub entity_id: i32,
    /// Vehicle or holder, `-1` to detach.
    pub vehicle_id: i32,
    /// True for a lead, false for a mount.
    pub leash: bool,
}

/// Clientbound `entity_effect`.
///
/// `duration` is an `i16` here; protocol 47 sends a varint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_effect", state = Play, bound = Client)]
pub struct EntityEffect {
    /// Entity id.
    pub entity_id: i32,
    /// Numeric effect id.
    pub effect_id: i8,
    /// Amplifier; `0` is level I.
    pub amplifier: i8,
    /// Remaining duration in ticks.
    pub duration: i16,
}

/// Clientbound `remove_entity_effect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:remove_entity_effect", state = Play, bound = Client)]
pub struct RemoveEntityEffect {
    /// Entity id.
    pub entity_id: i32,
    /// Numeric effect id.
    pub effect_id: i8,
}

/// Clientbound `entity_equipment`.
///
/// The slot ordinal is an `i16`: `0` held, `1` boots through `4` helmet.
/// There is no off-hand in this era.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:entity_equipment", state = Play, bound = Client)]
pub struct ClientboundEntityEquipment {
    /// Entity id.
    pub entity_id: i32,
    /// Equipment slot ordinal.
    pub slot: i16,
    /// The stack now in that slot.
    pub item: Slot,
}

/// Clientbound `collect`: an item flying towards a collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:collect", state = Play, bound = Client)]
pub struct Collect {
    /// The item entity being collected.
    pub collected_entity_id: i32,
    /// The entity collecting it.
    pub collector_entity_id: i32,
}

/// Clientbound `animation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:animation", state = Play, bound = Client)]
pub struct Animation {
    /// Entity id.
    #[mc(varint)]
    pub entity_id: i32,
    /// Animation ordinal.
    pub animation: u8,
}

/// Clientbound `update_attributes`.
///
/// # Why the codec is hand-written
///
/// The outer array's count is an **`i32`**, and the derive's `len` attribute
/// accepts only `"varint"`, `"u8"` and `"i16"` — there is no spelling for a
/// four-byte count, and every one of the three it does accept consumes the
/// wrong number of bytes here. The inner modifier list's `i16` count is
/// expressible, so only the outer loop is by hand.
///
/// A cap on the entry count matters more than usual: the count is read before
/// any entry, so an `i32` read from a hostile or desynchronised stream would
/// otherwise be a request to preallocate up to two billion entries. The cap
/// is far above anything a real server sends (this era has seven attributes
/// in total) and far below anything that could exhaust memory.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateAttributes {
    /// Entity id.
    pub entity_id: i32,
    /// One entry per changed attribute.
    pub properties: Vec<AttributeProperty>,
}

/// Largest attribute-entry count this decoder will accept.
const MAX_ATTRIBUTE_ENTRIES: i32 = 256;

impl lodestone_core::Decode for UpdateAttributes {
    fn decode(
        reader: &mut lodestone_core::Reader<'_>,
        ctx: lodestone_core::Ctx,
    ) -> lodestone_core::Result<Self> {
        let entity_id = reader.i32()?;
        let count = reader.i32()?;
        if count < 0 {
            return Err(lodestone_core::Error::NegativeLength(count));
        }
        if count > MAX_ATTRIBUTE_ENTRIES {
            return Err(lodestone_core::Error::LimitExceeded {
                limit: MAX_ATTRIBUTE_ENTRIES as usize,
                actual: count as usize,
            });
        }
        let mut properties = Vec::with_capacity(count as usize);
        for _ in 0..count {
            properties.push(AttributeProperty::decode(reader, ctx)?);
        }
        Ok(Self {
            entity_id,
            properties,
        })
    }
}

impl lodestone_core::Encode for UpdateAttributes {
    fn encode(
        &self,
        writer: &mut lodestone_core::Writer,
        ctx: lodestone_core::Ctx,
    ) -> lodestone_core::Result<()> {
        writer.i32(self.entity_id);
        let count = i32::try_from(self.properties.len()).map_err(|_| {
            lodestone_core::Error::Custom(format!(
                "update_attributes carries {} entries, which overflows the i32 count",
                self.properties.len()
            ))
        })?;
        writer.i32(count);
        for property in &self.properties {
            property.encode(writer, ctx)?;
        }
        Ok(())
    }
}

impl lodestone_core::Packet for UpdateAttributes {
    const NAME: &'static str = "minecraft:update_attributes";
    const STATE: lodestone_core::State = lodestone_core::State::Play;
    const BOUND: lodestone_core::Bound = lodestone_core::Bound::Client;
    const PROTOCOLS: lodestone_core::ProtocolRange = lodestone_core::ProtocolRange::new(5, 5);
}

/// One attribute in an [`UpdateAttributes`] packet.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub struct AttributeProperty {
    /// Attribute key, such as `generic.maxHealth`.
    #[mc(max = 32767)]
    pub key: String,
    /// Base value before modifiers.
    pub value: f64,
    /// Modifiers applied to the base value.
    #[mc(len = "i16")]
    pub modifiers: Vec<AttributeModifier>,
}

/// One modifier on an [`AttributeProperty`].
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode)]
pub struct AttributeModifier {
    /// Modifier identity.
    pub uuid: uuid::Uuid,
    /// Modifier amount.
    pub amount: f64,
    /// Operation ordinal: `0` add, `1` multiply base, `2` multiply total.
    pub operation: i8,
}
