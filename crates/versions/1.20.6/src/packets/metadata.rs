//! This era's `entityMetadata` wire type — an indexed, self-terminating list
//! of typed entity data-watcher values.
//!
//! # Why this is hand-written and *duplicated* rather than shared
//!
//! Entity metadata is one of the most version-divergent surfaces in the whole
//! protocol, and the serializer **type table is renumbered between eras**.
//! Against the 1.19 era's nineteen types, this one has **thirty-one**: a
//! 64-bit `long` is inserted at index 2 (moving every later type up by one),
//! the two chat types became anonymous NBT rather than JSON strings, a
//! *list*-of-particles type was added beside the single-particle one, and six
//! variant types (cat, wolf, frog, painting, sniffer state, armadillo state)
//! plus a vector and a quaternion were appended. A decoder inherited from the
//! era below therefore reads a `float` where a `long` is written, from index
//! 2 onward, for every entity on the wire.
//!
//! * **Header.** `key: u8` then a separate `type: varint`; the list ends with
//!   a `0xFF` key.
//! * **Type table.** `0 byte, 1 int, 2 long, 3 float, 4 string, 5 component,
//!   6 optional_component, 7 item_stack, 8 boolean, 9 rotations, 10
//!   block_pos, 11 optional_block_pos, 12 direction, 13 optional_uuid, 14
//!   block_state, 15 optional_block_state, 16 compound_tag, 17 particle, 18
//!   particles, 19 villager_data, 20 optional_unsigned_int, 21 pose, 22
//!   cat_variant, 23 wolf_variant, 24 frog_variant, 25 optional_global_pos,
//!   26 painting_variant, 27 sniffer_state, 28 armadillo_state, 29 vector3,
//!   30 quaternion`.
//!
//! # The four this table refuses, and why refusing is right
//!
//! A metadata value's length is implied by its type, exactly as an item
//! component's is, so an unmodelled type cannot be skipped — it has to fail.
//!
//! * `particle` and `particles` need a particle-id registry with per-particle
//!   payloads that no crate here carries.
//! * `wolf_variant` and `painting_variant` are registry-entry *holders*: a
//!   varint id, or a zero followed by the whole variant inline. The id form
//!   is decoded; the inline form is refused, since its payload is a nested
//!   record this crate has no model for.
//! * `optional_global_pos` is a disagreement rather than a gap.
//!   `minecraft-data` models it as an optional bare **string**, while the
//!   value it names is a dimension *and* a block position everywhere else it
//!   appears. The two readings differ by eight bytes and no dump in this tree
//!   settles it, so it errors by name. No vanilla entity in this era's
//!   registry is known to send one, which is why that refusal has not been
//!   reachable in practice.

use lodestone_core::{Ctx, Decode, Encode, Error, Nbt, Reader, Result, Writer, read_network_nbt,
    write_network_nbt};
use uuid::Uuid;

use super::position::Position;
use super::slot::Slot;

/// Sentinel byte marking the end of a metadata list.
const END: u8 = 0xFF;

/// Upper bound on a metadata string, matching the vanilla limit.
const MAX_STRING: usize = 32_767;

/// A single typed value in this era's entity-metadata list.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    /// Type 0: signed byte.
    Byte(i8),
    /// Type 1: VarInt-encoded int.
    VarInt(i32),
    /// Type 2: VarLong-encoded long — inserted at this era, which is what
    /// renumbers everything below it.
    VarLong(i64),
    /// Type 3: float.
    Float(f32),
    /// Type 4: UTF-8 string.
    String(String),
    /// Type 5: chat component, as anonymous NBT rather than a JSON string.
    Component(Nbt),
    /// Type 6: optional chat component.
    OptComponent(Option<Nbt>),
    /// Type 7: item stack.
    Slot(Slot),
    /// Type 8: boolean.
    Bool(bool),
    /// Type 9: rotation as three floats `(pitch, yaw, roll)`.
    Rotations {
        /// Rotation about X.
        pitch: f32,
        /// Rotation about Y.
        yaw: f32,
        /// Rotation about Z.
        roll: f32,
    },
    /// Type 10: packed `i64` block position.
    BlockPos(Position),
    /// Type 11: optional packed block position.
    OptBlockPos(Option<Position>),
    /// Type 12: VarInt facing direction.
    Direction(i32),
    /// Type 13: optional UUID.
    OptUuid(Option<Uuid>),
    /// Type 14: VarInt block-state id.
    BlockState(i32),
    /// Type 15: optional block-state id (`0` = absent, else `value + 1`).
    OptBlockState(Option<i32>),
    /// Type 16: NBT compound.
    CompoundTag(Nbt),
    /// Type 19: villager data — three VarInts `(type, profession, level)`.
    VillagerData {
        /// Villager type id.
        kind: i32,
        /// Villager profession id.
        profession: i32,
        /// Villager level.
        level: i32,
    },
    /// Type 20: optional VarInt (`0` = absent, otherwise `value + 1` on the
    /// wire); modeled as the already-decoded logical value.
    OptUnsignedInt(Option<i32>),
    /// Type 21: VarInt pose id.
    Pose(i32),
    /// Type 22: cat variant id.
    CatVariant(i32),
    /// Type 23: wolf variant, by registry id — see the module docs for the
    /// inline form this refuses.
    WolfVariant(i32),
    /// Type 24: frog variant id.
    FrogVariant(i32),
    /// Type 26: painting variant, by registry id.
    PaintingVariant(i32),
    /// Type 27: sniffer state id.
    SnifferState(i32),
    /// Type 28: armadillo state id.
    ArmadilloState(i32),
    /// Type 29: three floats.
    Vector3 {
        /// X component.
        x: f32,
        /// Y component.
        y: f32,
        /// Z component.
        z: f32,
    },
    /// Type 30: four floats.
    Quaternion {
        /// X component.
        x: f32,
        /// Y component.
        y: f32,
        /// Z component.
        z: f32,
        /// W component.
        w: f32,
    },
}

impl MetadataValue {
    /// The serializer type id.
    const fn type_id(&self) -> i32 {
        match self {
            MetadataValue::Byte(_) => 0,
            MetadataValue::VarInt(_) => 1,
            MetadataValue::VarLong(_) => 2,
            MetadataValue::Float(_) => 3,
            MetadataValue::String(_) => 4,
            MetadataValue::Component(_) => 5,
            MetadataValue::OptComponent(_) => 6,
            MetadataValue::Slot(_) => 7,
            MetadataValue::Bool(_) => 8,
            MetadataValue::Rotations { .. } => 9,
            MetadataValue::BlockPos(_) => 10,
            MetadataValue::OptBlockPos(_) => 11,
            MetadataValue::Direction(_) => 12,
            MetadataValue::OptUuid(_) => 13,
            MetadataValue::BlockState(_) => 14,
            MetadataValue::OptBlockState(_) => 15,
            MetadataValue::CompoundTag(_) => 16,
            MetadataValue::VillagerData { .. } => 19,
            MetadataValue::OptUnsignedInt(_) => 20,
            MetadataValue::Pose(_) => 21,
            MetadataValue::CatVariant(_) => 22,
            MetadataValue::WolfVariant(_) => 23,
            MetadataValue::FrogVariant(_) => 24,
            MetadataValue::PaintingVariant(_) => 26,
            MetadataValue::SnifferState(_) => 27,
            MetadataValue::ArmadilloState(_) => 28,
            MetadataValue::Vector3 { .. } => 29,
            MetadataValue::Quaternion { .. } => 30,
        }
    }

    fn encode_value(&self, w: &mut Writer, ctx: Ctx) -> Result<()> {
        match self {
            MetadataValue::Byte(v) => w.i8(*v),
            MetadataValue::VarInt(v)
            | MetadataValue::Direction(v)
            | MetadataValue::BlockState(v)
            | MetadataValue::Pose(v)
            | MetadataValue::CatVariant(v)
            | MetadataValue::FrogVariant(v)
            | MetadataValue::SnifferState(v)
            | MetadataValue::ArmadilloState(v) => w.var_i32(*v),
            MetadataValue::VarLong(v) => w.var_i64(*v),
            MetadataValue::Float(v) => w.f32(*v),
            MetadataValue::String(v) => w.string(v),
            MetadataValue::Component(value) => write_network_nbt(w, value)?,
            MetadataValue::OptComponent(opt) => match opt {
                Some(value) => {
                    w.bool(true);
                    write_network_nbt(w, value)?;
                }
                None => w.bool(false),
            },
            MetadataValue::Slot(v) => v.encode(w, ctx)?,
            MetadataValue::Bool(v) => w.bool(*v),
            MetadataValue::Rotations { pitch, yaw, roll } => {
                w.f32(*pitch);
                w.f32(*yaw);
                w.f32(*roll);
            }
            MetadataValue::BlockPos(p) => p.encode(w, ctx)?,
            MetadataValue::OptBlockPos(opt) => match opt {
                Some(p) => {
                    w.bool(true);
                    p.encode(w, ctx)?;
                }
                None => w.bool(false),
            },
            MetadataValue::OptUuid(opt) => match opt {
                Some(id) => {
                    w.bool(true);
                    w.uuid(*id);
                }
                None => w.bool(false),
            },
            // Both optional-varint forms share the `0 = absent` encoding.
            MetadataValue::OptBlockState(opt) | MetadataValue::OptUnsignedInt(opt) => match opt {
                Some(v) => w.var_i32(v.wrapping_add(1)),
                None => w.var_i32(0),
            },
            MetadataValue::CompoundTag(value) => write_network_nbt(w, value)?,
            MetadataValue::VillagerData {
                kind,
                profession,
                level,
            } => {
                w.var_i32(*kind);
                w.var_i32(*profession);
                w.var_i32(*level);
            }
            // A registry-entry holder writes `id + 1`; a zero would introduce
            // the inline form this crate refuses to decode, so it is never
            // written either.
            MetadataValue::WolfVariant(id) | MetadataValue::PaintingVariant(id) => {
                w.var_i32(id.wrapping_add(1));
            }
            MetadataValue::Vector3 { x, y, z } => {
                w.f32(*x);
                w.f32(*y);
                w.f32(*z);
            }
            MetadataValue::Quaternion { x, y, z, w: rot_w } => {
                w.f32(*x);
                w.f32(*y);
                w.f32(*z);
                w.f32(*rot_w);
            }
        }
        Ok(())
    }

    fn decode_value(r: &mut Reader<'_>, type_id: i32, ctx: Ctx) -> Result<Self> {
        Ok(match type_id {
            0 => MetadataValue::Byte(r.i8()?),
            1 => MetadataValue::VarInt(r.var_i32()?),
            2 => MetadataValue::VarLong(r.var_i64()?),
            3 => MetadataValue::Float(r.f32()?),
            4 => MetadataValue::String(r.string(MAX_STRING)?),
            5 => MetadataValue::Component(read_network_nbt(r)?),
            6 => MetadataValue::OptComponent(if r.bool()? {
                Some(read_network_nbt(r)?)
            } else {
                None
            }),
            7 => MetadataValue::Slot(Slot::decode(r, ctx)?),
            8 => MetadataValue::Bool(r.bool()?),
            9 => MetadataValue::Rotations {
                pitch: r.f32()?,
                yaw: r.f32()?,
                roll: r.f32()?,
            },
            10 => MetadataValue::BlockPos(Position::decode(r, ctx)?),
            11 => MetadataValue::OptBlockPos(if r.bool()? {
                Some(Position::decode(r, ctx)?)
            } else {
                None
            }),
            12 => MetadataValue::Direction(r.var_i32()?),
            13 => MetadataValue::OptUuid(if r.bool()? { Some(r.uuid()?) } else { None }),
            14 => MetadataValue::BlockState(r.var_i32()?),
            15 => MetadataValue::OptBlockState(decode_optional_varint(r)?),
            16 => MetadataValue::CompoundTag(read_network_nbt(r)?),
            19 => MetadataValue::VillagerData {
                kind: r.var_i32()?,
                profession: r.var_i32()?,
                level: r.var_i32()?,
            },
            20 => MetadataValue::OptUnsignedInt(decode_optional_varint(r)?),
            21 => MetadataValue::Pose(r.var_i32()?),
            22 => MetadataValue::CatVariant(r.var_i32()?),
            23 => MetadataValue::WolfVariant(decode_registry_holder(r, "wolf variant")?),
            24 => MetadataValue::FrogVariant(r.var_i32()?),
            26 => MetadataValue::PaintingVariant(decode_registry_holder(r, "painting variant")?),
            27 => MetadataValue::SnifferState(r.var_i32()?),
            28 => MetadataValue::ArmadilloState(r.var_i32()?),
            29 => MetadataValue::Vector3 {
                x: r.f32()?,
                y: r.f32()?,
                z: r.f32()?,
            },
            30 => MetadataValue::Quaternion {
                x: r.f32()?,
                y: r.f32()?,
                z: r.f32()?,
                w: r.f32()?,
            },
            other => {
                // Types 17, 18 and 25, and any id this era does not define,
                // fall here: no model for the payload means no way to skip
                // it, so this fails loudly rather than misparsing the rest of
                // the list. See the module docs.
                return Err(Error::InvalidEnumVariant {
                    name: "1.20.6 metadata type",
                    value: other,
                });
            }
        })
    }
}

/// Reads the `0 = absent, else value + 1` optional-varint form both
/// `optional_block_state` and `optional_unsigned_int` use.
fn decode_optional_varint(r: &mut Reader<'_>) -> Result<Option<i32>> {
    let raw = r.var_i32()?;
    Ok(if raw == 0 { None } else { Some(raw - 1) })
}

/// Reads a registry-entry holder's **id** form, refusing the inline form.
///
/// The wire writes `id + 1` for a registry reference and `0` to mean "the
/// whole entry follows". This crate models no variant record, so a zero is an
/// error naming the field rather than a guess at the inline layout.
fn decode_registry_holder(r: &mut Reader<'_>, what: &'static str) -> Result<i32> {
    let raw = r.var_i32()?;
    if raw == 0 {
        return Err(Error::Custom(format!(
            "{what} arrived as an inline registry entry, which this protocol \
             family does not model; only the registry-id form is supported"
        )));
    }
    Ok(raw - 1)
}

/// One entry in a metadata list: an index key and its typed value.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataEntry {
    /// Data-watcher index.
    pub key: u8,
    /// The typed value.
    pub value: MetadataValue,
}

/// A complete entity-metadata list, terminated on the wire by `0xFF`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EntityMetadata(pub Vec<MetadataEntry>);

impl Encode for EntityMetadata {
    fn encode(&self, w: &mut Writer, ctx: Ctx) -> Result<()> {
        for entry in &self.0 {
            w.u8(entry.key);
            w.var_i32(entry.value.type_id());
            entry.value.encode_value(w, ctx)?;
        }
        w.u8(END);
        Ok(())
    }
}

impl Decode for EntityMetadata {
    fn decode(r: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        let mut entries = Vec::new();
        loop {
            let key = r.u8()?;
            if key == END {
                break;
            }
            let type_id = r.var_i32()?;
            let value = MetadataValue::decode_value(r, type_id, ctx)?;
            entries.push(MetadataEntry { key, value });
        }
        Ok(EntityMetadata(entries))
    }
}
