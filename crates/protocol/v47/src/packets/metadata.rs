//! The 1.8 (protocol 47) `entityMetadata` wire type — a keyed, self-terminating
//! list of typed entity data-watcher values.
//!
//! # Why this is hand-written and why it is *duplicated* rather than shared
//!
//! Entity metadata is one of the most version-divergent surfaces in the whole
//! protocol, and 1.8's encoding shares almost nothing with the modern one:
//!
//! * **Header.** 1.8 packs the type and key into a *single* byte,
//!   `(type << 5) | key` (3 bits of type, 5 bits of key). Modern versions send a
//!   `key: u8` then a separate `type: varint`.
//! * **Terminator.** 1.8 ends the list with the sentinel byte `0x7F`; modern
//!   versions use `0xFF`.
//! * **Type table.** 1.8 has eight types (`byte, short, int, float, string,
//!   slot, position, rotation`) where *position is an unpacked `(i32,i32,i32)`
//!   triple*. Modern versions renumber everything, drop `short`/`int`, and add
//!   `bool`, an `i64`-*packed* position, several `Option<_>` types, a UUID, a
//!   block-state id and NBT.
//!
//! There is no lossless common representation: a shared enum would either lose
//! 1.8's `Short`/`Int`/unpacked-position distinctions or invent modern variants
//! 1.8 can never produce. This is the case the project explicitly blessed
//! duplication for — so each version crate carries its own `MetadataValue`
//! enum and its own terminated-loop codec. See the v340 crate for the
//! contrasting modern table.
//!
//! Because these types implement `Encode`/`Decode`, packets that carry metadata
//! (entity spawn, the standalone metadata packet) still derive their own codecs
//! and simply hold an [`EntityMetadata`] field.

use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer};

use super::slot::Slot;

/// Sentinel byte marking the end of a 1.8 metadata list.
const END: u8 = 0x7F;

/// Upper bound on a metadata string, matching the vanilla 1.8 limit.
const MAX_STRING: usize = 32_767;

/// A single typed value in a 1.8 entity-metadata list.
///
/// The variant set is exactly 1.8's eight data-watcher types; note `Position`
/// is an **unpacked** integer triple, unlike the packed `i64` modern versions
/// use — a genuine wire divergence, not just a renumbering.
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
    /// Type 5: item slot.
    Slot(Slot),
    /// Type 6: block position as an unpacked `(x, y, z)` int triple.
    Position {
        /// Block X.
        x: i32,
        /// Block Y.
        y: i32,
        /// Block Z.
        z: i32,
    },
    /// Type 7: rotation as three floats `(pitch, yaw, roll)`.
    Rotation {
        /// Rotation about X.
        pitch: f32,
        /// Rotation about Y.
        yaw: f32,
        /// Rotation about Z.
        roll: f32,
    },
}

impl MetadataValue {
    /// The 1.8 type id (high 3 bits of the header byte).
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

    fn encode_value(&self, w: &mut Writer, ctx: Ctx) -> Result<()> {
        match self {
            MetadataValue::Byte(v) => w.i8(*v),
            MetadataValue::Short(v) => w.i16(*v),
            MetadataValue::Int(v) => w.i32(*v),
            MetadataValue::Float(v) => w.f32(*v),
            MetadataValue::String(v) => w.string(v),
            MetadataValue::Slot(v) => v.encode(w, ctx)?,
            MetadataValue::Position { x, y, z } => {
                w.i32(*x);
                w.i32(*y);
                w.i32(*z);
            }
            MetadataValue::Rotation { pitch, yaw, roll } => {
                w.f32(*pitch);
                w.f32(*yaw);
                w.f32(*roll);
            }
        }
        Ok(())
    }

    fn decode_value(r: &mut Reader<'_>, type_id: u8, ctx: Ctx) -> Result<Self> {
        Ok(match type_id {
            0 => MetadataValue::Byte(r.i8()?),
            1 => MetadataValue::Short(r.i16()?),
            2 => MetadataValue::Int(r.i32()?),
            3 => MetadataValue::Float(r.f32()?),
            4 => MetadataValue::String(r.string(MAX_STRING)?),
            5 => MetadataValue::Slot(Slot::decode(r, ctx)?),
            6 => MetadataValue::Position {
                x: r.i32()?,
                y: r.i32()?,
                z: r.i32()?,
            },
            7 => MetadataValue::Rotation {
                pitch: r.f32()?,
                yaw: r.f32()?,
                roll: r.f32()?,
            },
            other => {
                return Err(Error::InvalidEnumVariant {
                    name: "v47 metadata type",
                    value: i32::from(other),
                });
            }
        })
    }
}

/// One entry in a metadata list: a 5-bit key and its typed value.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataEntry {
    /// Data-watcher index (0..=31).
    pub key: u8,
    /// The typed value.
    pub value: MetadataValue,
}

/// A complete 1.8 entity-metadata list, terminated on the wire by `0x7F`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EntityMetadata(pub Vec<MetadataEntry>);

impl Encode for EntityMetadata {
    fn encode(&self, w: &mut Writer, ctx: Ctx) -> Result<()> {
        for entry in &self.0 {
            let header = (entry.value.type_id() << 5) | (entry.key & 0x1F);
            w.u8(header);
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
            let header = r.u8()?;
            if header == END {
                break;
            }
            let type_id = (header >> 5) & 0x07;
            let key = header & 0x1F;
            let value = MetadataValue::decode_value(r, type_id, ctx)?;
            entries.push(MetadataEntry { key, value });
        }
        Ok(EntityMetadata(entries))
    }
}
