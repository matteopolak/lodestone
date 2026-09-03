//! Block and world-event packets for protocol 5.

use lodestone_macros::{Decode, Encode, Packet};

use super::position::{PositionIbi, PositionIii, PositionIsi};

/// Clientbound `block_change`.
///
/// # Two fields where later protocols have one
///
/// The block id arrives as a varint and its metadata as a separate trailing
/// byte. Protocol 47 replaced both with a single varint block-state id. The
/// pair is recombined into the same `(id << 4) | meta` composite the chunk
/// decoder builds, so one canonicalisation path serves both.
///
/// Measured from a real join: 11-byte bodies for ids under 128 and 12-byte
/// for the rest, which is exactly `4 + 1 + 4 + varint + 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_change", state = Play, bound = Client)]
pub struct BlockChange {
    /// Block position, with an unsigned-byte y.
    pub location: PositionIbi,
    /// Numeric block id.
    #[mc(varint)]
    pub block_type: i32,
    /// Block metadata, `0..16`.
    pub metadata: u8,
}

/// Clientbound `multi_block_change`.
///
/// # Why the records are decoded by hand
///
/// The packet declares both a record count and a byte length, and each record
/// is four bytes packing metadata, block id and local coordinates across
/// bit-field boundaries: `u16` of `(metadata:4, blockId:12)`, then a `y`
/// byte, then a `u8` of `(z:4, x:4)`. A derived codec cannot express the
/// bit-fields, and the redundant length is worth checking against the count
/// rather than ignoring — they disagree only if the framing is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiBlockChange {
    /// Column x, in chunks.
    pub chunk_x: i32,
    /// Column z, in chunks.
    pub chunk_z: i32,
    /// One entry per changed block.
    pub records: Vec<BlockRecord>,
}

/// One changed block in a [`MultiBlockChange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRecord {
    /// Block x within the column, `0..16`.
    pub x: u8,
    /// Block y, `0..256`.
    pub y: u8,
    /// Block z within the column, `0..16`.
    pub z: u8,
    /// Numeric block id, `0..4096`.
    pub block_id: u16,
    /// Block metadata, `0..16`.
    pub metadata: u8,
}

impl BlockRecord {
    /// This record as the pre-Flattening `(id << 4) | meta` composite.
    #[must_use]
    pub const fn composite(self) -> u32 {
        ((self.block_id as u32) << 4) | (self.metadata as u32)
    }
}

/// Bytes each record occupies on the wire.
const RECORD_BYTES: usize = 4;

impl lodestone_core::Decode for MultiBlockChange {
    fn decode(
        reader: &mut lodestone_core::Reader<'_>,
        _ctx: lodestone_core::Ctx,
    ) -> lodestone_core::Result<Self> {
        let chunk_x = reader.i32()?;
        let chunk_z = reader.i32()?;
        let count = reader.i16()?;
        let count =
            usize::try_from(count).map_err(|_| lodestone_core::Error::NegativeLength(i32::from(count)))?;
        let declared = reader.i32()?;
        let declared = usize::try_from(declared)
            .map_err(|_| lodestone_core::Error::NegativeLength(declared))?;
        if declared != count * RECORD_BYTES {
            return Err(lodestone_core::Error::Custom(format!(
                "multi_block_change declares {count} records but {declared} bytes, and a record \
                 is {RECORD_BYTES} bytes"
            )));
        }
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let packed = reader.u16()?;
            let y = reader.u8()?;
            let horizontal = reader.u8()?;
            records.push(BlockRecord {
                x: horizontal & 0x0F,
                y,
                z: (horizontal >> 4) & 0x0F,
                block_id: packed >> 4,
                metadata: (packed & 0x0F) as u8,
            });
        }
        Ok(Self {
            chunk_x,
            chunk_z,
            records,
        })
    }
}

impl lodestone_core::Encode for MultiBlockChange {
    fn encode(
        &self,
        writer: &mut lodestone_core::Writer,
        _ctx: lodestone_core::Ctx,
    ) -> lodestone_core::Result<()> {
        writer.i32(self.chunk_x);
        writer.i32(self.chunk_z);
        let count = i16::try_from(self.records.len()).map_err(|_| {
            lodestone_core::Error::Custom(format!(
                "{} records do not fit multi_block_change's i16 count",
                self.records.len()
            ))
        })?;
        writer.i16(count);
        writer.i32((self.records.len() * RECORD_BYTES) as i32);
        for record in &self.records {
            writer.u16((record.block_id << 4) | u16::from(record.metadata & 0x0F));
            writer.u8(record.y);
            writer.u8(((record.z & 0x0F) << 4) | (record.x & 0x0F));
        }
        Ok(())
    }
}

impl lodestone_core::Packet for MultiBlockChange {
    const NAME: &'static str = "minecraft:multi_block_change";
    const STATE: lodestone_core::State = lodestone_core::State::Play;
    const BOUND: lodestone_core::Bound = lodestone_core::Bound::Client;
    const PROTOCOLS: lodestone_core::ProtocolRange = lodestone_core::ProtocolRange::new(5, 5);
}

/// Clientbound `block_action`: a block's animation state, such as a chest
/// lid or a note block pitch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_action", state = Play, bound = Client)]
pub struct BlockAction {
    /// Block position, with an `i16` y.
    pub location: PositionIsi,
    /// Action-specific first byte.
    pub byte1: u8,
    /// Action-specific second byte.
    pub byte2: u8,
    /// Numeric block id the action belongs to.
    #[mc(varint)]
    pub block_id: i32,
}

/// Clientbound `block_break_animation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_break_animation", state = Play, bound = Client)]
pub struct BlockBreakAnimation {
    /// Entity id of the digger.
    #[mc(varint)]
    pub entity_id: i32,
    /// Block being broken.
    pub location: PositionIii,
    /// Progress stage, `0..=9`.
    pub destroy_stage: i8,
}

/// Clientbound `world_event`: a sound or particle effect keyed by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:world_event", state = Play, bound = Client)]
pub struct WorldEvent {
    /// Effect id.
    pub effect_id: i32,
    /// Where it happened, with an unsigned-byte y.
    pub location: PositionIbi,
    /// Effect-specific data.
    pub data: i32,
    /// Whether the effect is audible beyond its normal range.
    pub global: bool,
}

/// Clientbound `named_sound_effect`.
///
/// The coordinates are fixed-point in *eighths* of a block here, not the
/// thirty-seconds used for entity positions — a different scale in the same
/// protocol, so it cannot share a conversion with them.
#[derive(Debug, Clone, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:named_sound_effect", state = Play, bound = Client)]
pub struct NamedSoundEffect {
    /// Sound name, such as `mob.creeper.say`.
    #[mc(max = 32767)]
    pub sound_name: String,
    /// Fixed-point x, 8 units per block.
    pub x: i32,
    /// Fixed-point y.
    pub y: i32,
    /// Fixed-point z.
    pub z: i32,
    /// Volume multiplier.
    pub volume: f32,
    /// Pitch, as `pitch / 63.0` of normal.
    pub pitch: u8,
}

/// Clientbound `open_sign_entity`: the sign-editing screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:open_sign_entity", state = Play, bound = Client)]
pub struct OpenSignEntity {
    /// The sign to edit.
    pub location: PositionIii,
}

/// Serverbound `block_dig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_dig", state = Play, bound = Server)]
pub struct BlockDig {
    /// `0` start, `1` cancel, `2` finish, `3` drop stack, `4` drop one,
    /// `5` release use.
    pub status: i8,
    /// Block x.
    pub x: i32,
    /// Block y.
    pub y: u8,
    /// Block z.
    pub z: i32,
    /// Face ordinal being dug.
    pub face: i8,
}

/// Serverbound `block_place`.
///
/// The inline held item is redundant — the server uses its own view of the
/// player's inventory — but it is not optional, and an empty stack is
/// accepted.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_place", state = Play, bound = Server)]
pub struct BlockPlace {
    /// Block x being placed against.
    pub x: i32,
    /// Block y.
    pub y: u8,
    /// Block z.
    pub z: i32,
    /// Face ordinal, or `-1` for a use-in-air.
    pub direction: i8,
    /// Held stack, as the client sees it.
    pub held_item: super::slot::Slot,
    /// Cursor x within the face, in sixteenths.
    pub cursor_x: i8,
    /// Cursor y within the face.
    pub cursor_y: i8,
    /// Cursor z within the face.
    pub cursor_z: i8,
}
