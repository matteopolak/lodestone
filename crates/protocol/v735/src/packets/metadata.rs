//! The 1.12.2 (protocol 340) `entityMetadata` wire type — an indexed,
//! self-terminating list of typed entity data-watcher values.
//!
//! # Why this is hand-written and why it is *duplicated* rather than shared
//!
//! Entity metadata is one of the most version-divergent surfaces in the whole
//! protocol. This modern encoding shares almost nothing with 1.8's:
//!
//! * **Header.** 1.12 sends a `key: u8` then a separate `type: varint`; 1.8
//!   packs both into one byte as `(type << 5) | key`.
//! * **Terminator.** 1.12 ends the list with `0xFF`; 1.8 uses `0x7F`.
//! * **Type table.** 1.12 renumbers everything, drops 1.8's `short`/`int`, and
//!   adds `bool`, an `i64`-**packed** position, several `Option<_>` types, a
//!   UUID, a block-state id and NBT — none of which 1.8 can express. 1.8's
//!   *unpacked* `(i32,i32,i32)` position has no 1.12 equivalent either.
//!
//! There is no lossless common representation across the two, so — as the
//! project explicitly blessed for exactly this case — each version crate carries
//! its own `MetadataValue` enum and codec. See the v47 crate for the legacy
//! table.
//!
//! Because these types implement `Encode`/`Decode`, packets that carry metadata
//! still derive their own codecs and simply hold an [`EntityMetadata`] field.

use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer, read_named_nbt};
use uuid::Uuid;

use super::position::Position;
use super::slot::Slot;

/// Sentinel byte marking the end of a 1.12 metadata list.
const END: u8 = 0xFF;

/// Upper bound on a metadata string, matching the vanilla limit.
const MAX_STRING: usize = 32_767;

/// A single typed value in a 1.12.2 entity-metadata list.
///
/// The variant set is exactly 1.12.2's serializer table. Note `Position` is a
/// **packed** `i64` block position (unlike 1.8's unpacked triple), and several
/// variants (`Bool`, the `Opt*` options, `Uuid`, `Nbt`) have no 1.8 counterpart
/// at all.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    /// Type 0: signed byte.
    Byte(i8),
    /// Type 1: VarInt-encoded int.
    VarInt(i32),
    /// Type 2: float.
    Float(f32),
    /// Type 3: UTF-8 string.
    String(String),
    /// Type 4: chat component (JSON string).
    Chat(String),
    /// Type 5: item slot.
    Slot(Slot),
    /// Type 6: boolean.
    Bool(bool),
    /// Type 7: rotation as three floats `(pitch, yaw, roll)`.
    Rotation {
        /// Rotation about X.
        pitch: f32,
        /// Rotation about Y.
        yaw: f32,
        /// Rotation about Z.
        roll: f32,
    },
    /// Type 8: packed `i64` block position.
    Position(Position),
    /// Type 9: optional packed block position.
    OptPosition(Option<Position>),
    /// Type 10: VarInt facing direction.
    Direction(i32),
    /// Type 11: optional UUID.
    OptUuid(Option<Uuid>),
    /// Type 12: VarInt block-state id (`0` = absent/air).
    BlockId(i32),
    /// Type 13: NBT tag, stored as raw bytes (`None` = the `TAG_End` marker).
    Nbt(Option<Vec<u8>>),
}

impl MetadataValue {
    /// The 1.12 serializer type id.
    const fn type_id(&self) -> i32 {
        match self {
            MetadataValue::Byte(_) => 0,
            MetadataValue::VarInt(_) => 1,
            MetadataValue::Float(_) => 2,
            MetadataValue::String(_) => 3,
            MetadataValue::Chat(_) => 4,
            MetadataValue::Slot(_) => 5,
            MetadataValue::Bool(_) => 6,
            MetadataValue::Rotation { .. } => 7,
            MetadataValue::Position(_) => 8,
            MetadataValue::OptPosition(_) => 9,
            MetadataValue::Direction(_) => 10,
            MetadataValue::OptUuid(_) => 11,
            MetadataValue::BlockId(_) => 12,
            MetadataValue::Nbt(_) => 13,
        }
    }

    fn encode_value(&self, w: &mut Writer, ctx: Ctx) -> Result<()> {
        match self {
            MetadataValue::Byte(v) => w.i8(*v),
            MetadataValue::VarInt(v) | MetadataValue::Direction(v) | MetadataValue::BlockId(v) => {
                w.var_i32(*v);
            }
            MetadataValue::Float(v) => w.f32(*v),
            MetadataValue::String(v) | MetadataValue::Chat(v) => w.string(v),
            MetadataValue::Slot(v) => v.encode(w, ctx)?,
            MetadataValue::Bool(v) => w.bool(*v),
            MetadataValue::Rotation { pitch, yaw, roll } => {
                w.f32(*pitch);
                w.f32(*yaw);
                w.f32(*roll);
            }
            MetadataValue::Position(p) => p.encode(w, ctx)?,
            MetadataValue::OptPosition(opt) => match opt {
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
            MetadataValue::Nbt(nbt) => match nbt {
                None => w.u8(0), // TAG_End
                Some(raw) => w.bytes(raw),
            },
        }
        Ok(())
    }

    fn decode_value(r: &mut Reader<'_>, type_id: i32, ctx: Ctx) -> Result<Self> {
        Ok(match type_id {
            0 => MetadataValue::Byte(r.i8()?),
            1 => MetadataValue::VarInt(r.var_i32()?),
            2 => MetadataValue::Float(r.f32()?),
            3 => MetadataValue::String(r.string(MAX_STRING)?),
            4 => MetadataValue::Chat(r.string(MAX_STRING)?),
            5 => MetadataValue::Slot(Slot::decode(r, ctx)?),
            6 => MetadataValue::Bool(r.bool()?),
            7 => MetadataValue::Rotation {
                pitch: r.f32()?,
                yaw: r.f32()?,
                roll: r.f32()?,
            },
            8 => MetadataValue::Position(Position::decode(r, ctx)?),
            9 => MetadataValue::OptPosition(if r.bool()? {
                Some(Position::decode(r, ctx)?)
            } else {
                None
            }),
            10 => MetadataValue::Direction(r.var_i32()?),
            11 => MetadataValue::OptUuid(if r.bool()? { Some(r.uuid()?) } else { None }),
            12 => MetadataValue::BlockId(r.var_i32()?),
            13 => MetadataValue::Nbt(decode_optional_nbt(r)?),
            other => {
                return Err(Error::InvalidEnumVariant {
                    name: "v735 metadata type",
                    value: other,
                });
            }
        })
    }
}

/// Reads a metadata NBT value: a lone `0x00` (`TAG_End`) means "no NBT";
/// anything else is a full named compound captured as raw bytes (there is no
/// NBT *writer* in `lodestone-core`, so round-tripping stores raw wire bytes).
fn decode_optional_nbt(r: &mut Reader<'_>) -> Result<Option<Vec<u8>>> {
    let &first = r.remaining_bytes().first().ok_or(Error::UnexpectedEof)?;
    if first == 0 {
        r.u8()?;
        return Ok(None);
    }
    let before = r.remaining_bytes();
    let start_len = before.len();
    read_named_nbt(r)?;
    let consumed = start_len - r.remaining_bytes().len();
    Ok(Some(before[..consumed].to_vec()))
}

/// One entry in a metadata list: an index key and its typed value.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataEntry {
    /// Data-watcher index.
    pub key: u8,
    /// The typed value.
    pub value: MetadataValue,
}

/// A complete 1.12.2 entity-metadata list, terminated on the wire by `0xFF`.
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
