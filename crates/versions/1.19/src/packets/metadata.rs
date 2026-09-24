//! This era's `entityMetadata` wire type — an indexed,
//! self-terminating list of typed entity data-watcher values.
//!
//! # Why this is hand-written and why it is *duplicated* rather than shared
//!
//! Entity metadata is one of the most version-divergent surfaces in the whole
//! protocol, and the serializer **type table is renumbered between families**:
//! 1.13 inserted `OptChat` at index 5, pushing `Slot`, `Boolean`, `Rotation`
//! and every later type up by one relative to 1.12, and later versions appended
//! `Particle`, `VillagerData`, `OptVarInt` and `Pose`. A shared enum would have
//! to carry a per-version discriminant map; the project blesses duplicating the
//! whole codec per version instead, so this table is exactly this era's.
//!
//! The committed real 1.19.4 capture contains an index-9 serializer-3 value
//! followed by `0x41800000` (`16.0f`), proving that serializer 3 is `Float`.
//! Protocol 762 inserted `VarLong` at serializer 2, so every older serializer
//! at 2 and above shifts by one; copying the 1.17 table makes that float look
//! like a 65-byte string and runs out of input. The complete shifted table was
//! then cross-checked against `minecraft-data`; the capture remains the
//! authority for the framing that exposed the previous error.
//!
//! * **Header.** `key: u8` then a separate `type: varint`; the list ends with a
//!   `0xFF` key.
//! * **Type table.** `0 Byte, 1 VarInt, 2 VarLong, 3 Float, 4 String,
//!   5 Chat, 6 OptChat, 7 Slot, 8 Boolean, 9 Rotation, 10 Position,
//!   11 OptPosition, 12 Direction, 13 OptUUID, 14 BlockState,
//!   15 OptBlockState, 16 NBT, 17 Particle, 18 VillagerData,
//!   19 OptVarInt, 20 Pose, 21 CatVariant, 22 FrogVariant,
//!   23 OptGlobalPos, 24 PaintingVariant, 25 SnifferState, 26 Vector3,
//!   27 Quaternion`.
//!
//! `Particle` (type 17) needs a particle-id registry with per-particle payloads
//! that no crate carries yet. Type 23's local schema disagrees with the
//! dimension-qualified position shape used elsewhere, by eight bytes. Neither
//! is modeled: decoding either fails loudly with [`Error::InvalidEnumVariant`]
//! rather than silently misparsing. The adapter reports only the type-agnostic
//! shared-flags entry; an unmodelled entry still rejects the whole incremental
//! update rather than guessing its variable-sized payload.
//!
//! Because these types implement `Encode`/`Decode`, packets that carry metadata
//! still derive their own codecs and simply hold an [`EntityMetadata`] field.

use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer, read_named_nbt};
use uuid::Uuid;

use super::position::Position;
use super::slot::Slot;

/// Sentinel byte marking the end of a metadata list.
const END: u8 = 0xFF;

/// Upper bound on a metadata string, matching the vanilla limit.
const MAX_STRING: usize = 32_767;

/// A single typed value in this era's entity-metadata list.
///
/// The variant set matches this era's serializer table. `Position` is a packed
/// `i64` block position; several variants (`OptChat`, the `Opt*` options,
/// `VillagerData`, `Pose`) are 1.13+/1.14+ additions.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    /// Type 0: signed byte.
    Byte(i8),
    /// Type 1: VarInt-encoded int.
    VarInt(i32),
    /// Type 2: VarLong-encoded integer, inserted by this protocol era.
    VarLong(i64),
    /// Type 3: float.
    Float(f32),
    /// Type 4: UTF-8 string.
    String(String),
    /// Type 5: chat component (JSON string).
    Chat(String),
    /// Type 6: optional chat component (added 1.13).
    OptChat(Option<String>),
    /// Type 7: item slot.
    Slot(Slot),
    /// Type 8: boolean.
    Bool(bool),
    /// Type 9: rotation as three floats `(pitch, yaw, roll)`.
    Rotation {
        /// Rotation about X.
        pitch: f32,
        /// Rotation about Y.
        yaw: f32,
        /// Rotation about Z.
        roll: f32,
    },
    /// Type 10: packed `i64` block position.
    Position(Position),
    /// Type 11: optional packed block position.
    OptPosition(Option<Position>),
    /// Type 12: VarInt facing direction.
    Direction(i32),
    /// Type 13: optional UUID.
    OptUuid(Option<Uuid>),
    /// Type 14: VarInt block-state id.
    BlockId(i32),
    /// Type 15: optional VarInt block-state id (`0` = absent, otherwise the
    /// logical id plus one on the wire).
    OptBlockId(Option<i32>),
    /// Type 16: NBT tag, stored as raw bytes (`None` = the `TAG_End` marker).
    Nbt(Option<Vec<u8>>),
    /// Type 18: villager data — three VarInts `(type, profession, level)`.
    VillagerData {
        /// Villager type id.
        kind: i32,
        /// Villager profession id.
        profession: i32,
        /// Villager level.
        level: i32,
    },
    /// Type 19: optional VarInt (`0` = absent, otherwise `value + 1` on the
    /// wire); modeled as the already-decoded logical value.
    OptVarInt(Option<i32>),
    /// Type 20: VarInt pose id.
    Pose(i32),
    /// Type 21: VarInt cat-variant id.
    CatVariant(i32),
    /// Type 22: VarInt frog-variant id.
    FrogVariant(i32),
    /// Type 24: VarInt painting-variant id.
    PaintingVariant(i32),
    /// Type 25: VarInt sniffer-state id.
    SnifferState(i32),
    /// Type 26: three f32 components.
    Vector3 { x: f32, y: f32, z: f32 },
    /// Type 27: four f32 components.
    Quaternion { x: f32, y: f32, z: f32, w: f32 },
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
            MetadataValue::Chat(_) => 5,
            MetadataValue::OptChat(_) => 6,
            MetadataValue::Slot(_) => 7,
            MetadataValue::Bool(_) => 8,
            MetadataValue::Rotation { .. } => 9,
            MetadataValue::Position(_) => 10,
            MetadataValue::OptPosition(_) => 11,
            MetadataValue::Direction(_) => 12,
            MetadataValue::OptUuid(_) => 13,
            MetadataValue::BlockId(_) => 14,
            MetadataValue::OptBlockId(_) => 15,
            MetadataValue::Nbt(_) => 16,
            MetadataValue::VillagerData { .. } => 18,
            MetadataValue::OptVarInt(_) => 19,
            MetadataValue::Pose(_) => 20,
            MetadataValue::CatVariant(_) => 21,
            MetadataValue::FrogVariant(_) => 22,
            MetadataValue::PaintingVariant(_) => 24,
            MetadataValue::SnifferState(_) => 25,
            MetadataValue::Vector3 { .. } => 26,
            MetadataValue::Quaternion { .. } => 27,
        }
    }

    fn encode_value(&self, w: &mut Writer, ctx: Ctx) -> Result<()> {
        match self {
            MetadataValue::Byte(v) => w.i8(*v),
            MetadataValue::VarInt(v)
            | MetadataValue::Direction(v)
            | MetadataValue::BlockId(v)
            | MetadataValue::Pose(v)
            | MetadataValue::CatVariant(v)
            | MetadataValue::FrogVariant(v)
            | MetadataValue::PaintingVariant(v)
            | MetadataValue::SnifferState(v) => {
                w.var_i32(*v);
            }
            MetadataValue::VarLong(v) => w.var_i64(*v),
            MetadataValue::Float(v) => w.f32(*v),
            MetadataValue::String(v) | MetadataValue::Chat(v) => w.string(v),
            MetadataValue::OptChat(opt) => match opt {
                Some(text) => {
                    w.bool(true);
                    w.string(text);
                }
                None => w.bool(false),
            },
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
            MetadataValue::VillagerData {
                kind,
                profession,
                level,
            } => {
                w.var_i32(*kind);
                w.var_i32(*profession);
                w.var_i32(*level);
            }
            // Wire form: `0` = absent, else logical value + 1.
            MetadataValue::OptVarInt(opt) => match opt {
                Some(v) => w.var_i32(v.wrapping_add(1)),
                None => w.var_i32(0),
            },
            MetadataValue::OptBlockId(opt) => match opt {
                Some(v) => w.var_i32(v.wrapping_add(1)),
                None => w.var_i32(0),
            },
            MetadataValue::Vector3 { x, y, z } => {
                w.f32(*x);
                w.f32(*y);
                w.f32(*z);
            }
            MetadataValue::Quaternion { x, y, z, w: rotation_w } => {
                w.f32(*x);
                w.f32(*y);
                w.f32(*z);
                w.f32(*rotation_w);
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
            5 => MetadataValue::Chat(r.string(MAX_STRING)?),
            6 => MetadataValue::OptChat(if r.bool()? {
                Some(r.string(MAX_STRING)?)
            } else {
                None
            }),
            7 => MetadataValue::Slot(Slot::decode(r, ctx)?),
            8 => MetadataValue::Bool(r.bool()?),
            9 => MetadataValue::Rotation {
                pitch: r.f32()?,
                yaw: r.f32()?,
                roll: r.f32()?,
            },
            10 => MetadataValue::Position(Position::decode(r, ctx)?),
            11 => MetadataValue::OptPosition(if r.bool()? {
                Some(Position::decode(r, ctx)?)
            } else {
                None
            }),
            12 => MetadataValue::Direction(r.var_i32()?),
            13 => MetadataValue::OptUuid(if r.bool()? { Some(r.uuid()?) } else { None }),
            14 => MetadataValue::BlockId(r.var_i32()?),
            15 => {
                let raw = r.var_i32()?;
                MetadataValue::OptBlockId(if raw == 0 { None } else { Some(raw - 1) })
            }
            16 => MetadataValue::Nbt(decode_optional_nbt(r)?),
            18 => MetadataValue::VillagerData {
                kind: r.var_i32()?,
                profession: r.var_i32()?,
                level: r.var_i32()?,
            },
            19 => {
                let raw = r.var_i32()?;
                MetadataValue::OptVarInt(if raw == 0 { None } else { Some(raw - 1) })
            }
            20 => MetadataValue::Pose(r.var_i32()?),
            21 => MetadataValue::CatVariant(r.var_i32()?),
            22 => MetadataValue::FrogVariant(r.var_i32()?),
            24 => MetadataValue::PaintingVariant(r.var_i32()?),
            25 => MetadataValue::SnifferState(r.var_i32()?),
            26 => MetadataValue::Vector3 {
                x: r.f32()?,
                y: r.f32()?,
                z: r.f32()?,
            },
            27 => MetadataValue::Quaternion {
                x: r.f32()?,
                y: r.f32()?,
                z: r.f32()?,
                w: r.f32()?,
            },
            other => {
                // Type 17 (Particle), type 23 (optional global position), and
                // any unknown id fall here. Neither known gap has a settled
                // length, so fail loudly rather than misparse a later entry.
                return Err(Error::InvalidEnumVariant {
                    name: "v1-19 metadata type",
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
