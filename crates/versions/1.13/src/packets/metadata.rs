//! The 1.13.2 (protocol 404) `entityMetadata` wire type — an indexed,
//! self-terminating list of typed entity data-watcher values.
//!
//! # Why this is hand-written and why it is *duplicated* rather than shared
//!
//! Entity metadata is one of the most version-divergent surfaces in the whole
//! protocol, and the serializer **type table is renumbered between families**.
//! 1.13 is one of the two releases that renumbered it: it inserted `OptChat`
//! at index 5, pushing `Slot`, `Boolean`, `Rotation` and every later type up
//! by one relative to 1.12, and appended `Particle` at 15. 1.14 then appended
//! `VillagerData`, `OptVarInt` and `Pose`. So the same wire byte names a
//! different serializer in each of the three neighbouring eras, and a shared
//! enum would have to carry a per-version discriminant map; the project
//! blesses duplicating the whole codec per version instead, and this table is
//! exactly 1.13.2's.
//!
//! * **Header.** `key: u8` then a separate `type: varint`; the list ends with a
//!   `0xFF` key.
//! * **Type table (1.13.2).** `0 Byte, 1 VarInt, 2 Float, 3 String, 4 Chat,
//!   5 OptChat, 6 Slot, 7 Boolean, 8 Rotation, 9 Position, 10 OptPosition,
//!   11 Direction, 12 OptUUID, 13 OptBlockID, 14 NBT, 15 Particle`. It stops
//!   there: type 16 is not a serializer at 404, and decoding one is a desync,
//!   not a value to guess at.
//!
//! `Particle` (type 15) needs a particle-id registry with per-particle payloads
//! that no crate carries yet, so it is **not** modeled: decoding a particle
//! entry fails loudly with [`Error::InvalidEnumVariant`] rather than silently
//! misparsing. It is documented rather than papered over.
//!
//! The 1.13.2 list is reachable from more packets than the 1.14 one: at 404
//! `spawn_entity_living` and `named_entity_spawn` still append a metadata
//! list, which 1.15 removed.
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

/// A single typed value in a 1.13.2 entity-metadata list.
///
/// The variant set matches 1.13.2's serializer table. `Position` is a packed
/// pre-1.14 `i64` block position; `OptChat` and `Particle` are the two 1.13
/// additions.
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
    /// Type 5: optional chat component (added 1.13).
    OptChat(Option<String>),
    /// Type 6: item slot.
    Slot(Slot),
    /// Type 7: boolean.
    Bool(bool),
    /// Type 8: rotation as three floats `(pitch, yaw, roll)`.
    Rotation {
        /// Rotation about X.
        pitch: f32,
        /// Rotation about Y.
        yaw: f32,
        /// Rotation about Z.
        roll: f32,
    },
    /// Type 9: packed `i64` block position.
    Position(Position),
    /// Type 10: optional packed block position.
    OptPosition(Option<Position>),
    /// Type 11: VarInt facing direction.
    Direction(i32),
    /// Type 12: optional UUID.
    OptUuid(Option<Uuid>),
    /// Type 13: VarInt block-state id (`0` = absent/air).
    BlockId(i32),
    /// Type 14: NBT tag, stored as raw bytes (`None` = the `TAG_End` marker).
    Nbt(Option<Vec<u8>>),
}

impl MetadataValue {
    /// The 1.13.2 serializer type id.
    const fn type_id(&self) -> i32 {
        match self {
            MetadataValue::Byte(_) => 0,
            MetadataValue::VarInt(_) => 1,
            MetadataValue::Float(_) => 2,
            MetadataValue::String(_) => 3,
            MetadataValue::Chat(_) => 4,
            MetadataValue::OptChat(_) => 5,
            MetadataValue::Slot(_) => 6,
            MetadataValue::Bool(_) => 7,
            MetadataValue::Rotation { .. } => 8,
            MetadataValue::Position(_) => 9,
            MetadataValue::OptPosition(_) => 10,
            MetadataValue::Direction(_) => 11,
            MetadataValue::OptUuid(_) => 12,
            MetadataValue::BlockId(_) => 13,
            MetadataValue::Nbt(_) => 14,
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
            5 => MetadataValue::OptChat(if r.bool()? {
                Some(r.string(MAX_STRING)?)
            } else {
                None
            }),
            6 => MetadataValue::Slot(Slot::decode(r, ctx)?),
            7 => MetadataValue::Bool(r.bool()?),
            8 => MetadataValue::Rotation {
                pitch: r.f32()?,
                yaw: r.f32()?,
                roll: r.f32()?,
            },
            9 => MetadataValue::Position(Position::decode(r, ctx)?),
            10 => MetadataValue::OptPosition(if r.bool()? {
                Some(Position::decode(r, ctx)?)
            } else {
                None
            }),
            11 => MetadataValue::Direction(r.var_i32()?),
            12 => MetadataValue::OptUuid(if r.bool()? { Some(r.uuid()?) } else { None }),
            13 => MetadataValue::BlockId(r.var_i32()?),
            14 => MetadataValue::Nbt(decode_optional_nbt(r)?),
            other => {
                // Type 15 (Particle) has no registry to model it, and 16, 17
                // and 18 are 1.14 serializers that do not exist at 404 at
                // all: a body carrying one is a desync, so fail loudly rather
                // than consume bytes on a guess.
                return Err(Error::InvalidEnumVariant {
                    name: "v1-13 metadata type",
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

/// A complete 1.13.2 entity-metadata list, terminated on the wire by `0xFF`.
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
