//! The protocol 5 `entityMetadata` wire type: a keyed, self-terminating list
//! of typed data-watcher values.
//!
//! # Why this is duplicated rather than shared with the 1.8 era
//!
//! The *loop* is identical to protocol 47's, down to the bit packing: one
//! header byte holding `(type << 5) | key`, the sentinel `0x7F` ending the
//! list, and the same eight types in the same order. A field-list comparison
//! of the two modules would call them the same code.
//!
//! They are not the same code, because type 5 is an item stack and an item
//! stack is not the same shape in the two eras. Protocol 5's carries
//! gzip-compressed NBT behind an `i16` length; protocol 47's carries a bare
//! optional tag (see [`crate::packets::slot`]). A metadata list containing one
//! stack therefore frames differently, and every entry after that stack is
//! read at the wrong offset.
//!
//! This is precisely the trap `lodestone-protocol-common`'s module docs
//! describe for hand-decoded types — a field whose *type* is per-era despite
//! sharing a name — so the sharing stops here rather than at the loop.

use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer};

use super::slot::Slot;

/// Sentinel byte ending a metadata list.
const END: u8 = 0x7F;

/// Upper bound on a metadata string.
const MAX_STRING: usize = 32_767;

/// A single typed value in a protocol 5 metadata list.
///
/// `Position` is an **unpacked** integer triple here; the packed 64-bit form
/// arrives with protocol 47, alongside the packed block position.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    /// Type 0: signed byte.
    Byte(i8),
    /// Type 1: signed short.
    Short(i16),
    /// Type 2: signed int.
    Int(i32),
    /// Type 3: float.
    Float(f32),
    /// Type 4: UTF-8 string.
    String(String),
    /// Type 5: item stack, in this era's own shape.
    Slot(Slot),
    /// Type 6: block position as an unpacked `(x, y, z)` triple.
    Position {
        /// Block x.
        x: i32,
        /// Block y.
        y: i32,
        /// Block z.
        z: i32,
    },
    /// Type 7: rotation as three floats.
    Rotation {
        /// Rotation about x.
        pitch: f32,
        /// Rotation about y.
        yaw: f32,
        /// Rotation about z.
        roll: f32,
    },
}

impl MetadataValue {
    /// The type id carried in the high 3 bits of the header byte.
    const fn type_id(&self) -> u8 {
        match self {
            MetadataValue::Byte(_) => 0,
            MetadataValue::Short(_) => 1,
            MetadataValue::Int(_) => 2,
            MetadataValue::Float(_) => 3,
            MetadataValue::String(_) => 4,
            MetadataValue::Slot(_) => 5,
            MetadataValue::Position { .. } => 6,
            MetadataValue::Rotation { .. } => 7,
        }
    }

    fn encode_value(&self, writer: &mut Writer, ctx: Ctx) -> Result<()> {
        match self {
            MetadataValue::Byte(value) => writer.i8(*value),
            MetadataValue::Short(value) => writer.i16(*value),
            MetadataValue::Int(value) => writer.i32(*value),
            MetadataValue::Float(value) => writer.f32(*value),
            MetadataValue::String(value) => writer.string(value),
            MetadataValue::Slot(value) => value.encode(writer, ctx)?,
            MetadataValue::Position { x, y, z } => {
                writer.i32(*x);
                writer.i32(*y);
                writer.i32(*z);
            }
            MetadataValue::Rotation { pitch, yaw, roll } => {
                writer.f32(*pitch);
                writer.f32(*yaw);
                writer.f32(*roll);
            }
        }
        Ok(())
    }

    fn decode_value(reader: &mut Reader<'_>, type_id: u8, ctx: Ctx) -> Result<Self> {
        Ok(match type_id {
            0 => MetadataValue::Byte(reader.i8()?),
            1 => MetadataValue::Short(reader.i16()?),
            2 => MetadataValue::Int(reader.i32()?),
            3 => MetadataValue::Float(reader.f32()?),
            4 => MetadataValue::String(reader.string(MAX_STRING)?),
            5 => MetadataValue::Slot(Slot::decode(reader, ctx)?),
            6 => MetadataValue::Position {
                x: reader.i32()?,
                y: reader.i32()?,
                z: reader.i32()?,
            },
            7 => MetadataValue::Rotation {
                pitch: reader.f32()?,
                yaw: reader.f32()?,
                roll: reader.f32()?,
            },
            other => {
                return Err(Error::InvalidEnumVariant {
                    name: "protocol 5 metadata type",
                    value: i32::from(other),
                });
            }
        })
    }
}

/// One entry: a 5-bit data-watcher index and its typed value.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataEntry {
    /// Data-watcher index, `0..=31`.
    pub key: u8,
    /// The typed value.
    pub value: MetadataValue,
}

/// A complete metadata list, terminated on the wire by `0x7F`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EntityMetadata(pub Vec<MetadataEntry>);

impl EntityMetadata {
    /// The value at a data-watcher index, if the list carried one.
    #[must_use]
    pub fn get(&self, key: u8) -> Option<&MetadataValue> {
        self.0
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| &entry.value)
    }
}

impl Encode for EntityMetadata {
    fn encode(&self, writer: &mut Writer, ctx: Ctx) -> Result<()> {
        for entry in &self.0 {
            writer.u8((entry.value.type_id() << 5) | (entry.key & 0x1F));
            entry.value.encode_value(writer, ctx)?;
        }
        writer.u8(END);
        Ok(())
    }
}

impl Decode for EntityMetadata {
    fn decode(reader: &mut Reader<'_>, ctx: Ctx) -> Result<Self> {
        let mut entries = Vec::new();
        loop {
            let header = reader.u8()?;
            if header == END {
                break;
            }
            let type_id = (header >> 5) & 0x07;
            let key = header & 0x1F;
            entries.push(MetadataEntry {
                key,
                value: MetadataValue::decode_value(reader, type_id, ctx)?,
            });
        }
        Ok(EntityMetadata(entries))
    }
}
