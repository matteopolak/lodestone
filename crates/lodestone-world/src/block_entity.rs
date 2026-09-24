//! Block entities carried alongside a chunk: position, type, and NBT.
//!
//! In the chunk packet each block entity is a compact record: a byte packing
//! the section-relative X and Z (`x << 4 | z`), a signed short Y, a VarInt
//! registry id for the block-entity type, and a network-form NBT tag (which may
//! be a bare `TAG_End` when the server sends no extra data). The packing and
//! field order are structural and version-free; only the meaning of the type id
//! and the NBT schema are version-specific, and those belong to a version crate.
//!
//! These records are inbound-only for a client, so this type decodes but does
//! not re-encode: `lodestone-core` exposes an NBT *reader* but no writer, and a
//! client never needs to serialise a block entity back onto the wire. Capturing
//! the raw NBT byte span at decode time would allow byte-exact re-emission if
//! that ever changes; that seam is noted in the crate docs.

use lodestone_core::{Nbt, Reader, read_network_nbt};

use crate::Result;

/// A single block entity attached to a chunk column.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockEntity {
    /// Section-relative X coordinate (`0..16`).
    pub rel_x: u8,
    /// Section-relative Z coordinate (`0..16`).
    pub rel_z: u8,
    /// Absolute world Y coordinate.
    pub y: i16,
    /// Version-specific block-entity-type registry id.
    pub type_id: u32,
    /// Decoded NBT payload; [`Nbt::End`] when the server sent no data.
    pub nbt: Nbt,
}

impl BlockEntity {
    /// Reads one block entity from the chunk-packet wire form.
    ///
    /// # Errors
    /// Returns [`WorldError`](crate::WorldError) if the header or NBT payload is
    /// truncated or malformed.
    pub fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let packed_xz = r.u8()?;
        let y = r.i16()?;
        let type_id = r.var_i32()? as u32;
        let nbt = read_network_nbt(r)?;
        Ok(Self {
            rel_x: packed_xz >> 4,
            rel_z: packed_xz & 0x0F,
            y,
            type_id,
            nbt,
        })
    }

    /// Reads the VarInt-counted block-entity list that follows the chunk data.
    ///
    /// # Errors
    /// Returns [`WorldError`](crate::WorldError) on a negative count or any
    /// malformed record.
    pub fn decode_list(r: &mut Reader<'_>) -> Result<Vec<Self>> {
        let count = r.var_i32()?;
        if count < 0 {
            return Err(lodestone_core::Error::NegativeLength(count).into());
        }
        let mut list = Vec::with_capacity((count as usize).min(4096));
        for _ in 0..count {
            list.push(Self::decode(r)?);
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::Writer;

    /// Network NBT for a compound `{ Id: 5i }` (tag id, name, payload, End).
    fn sample_nbt_bytes(w: &mut Writer) {
        w.u8(10); // TAG_Compound root
        w.u8(3); // TAG_Int
        w.u16(2); // name length
        w.bytes(b"Id");
        w.i32(5); // value
        w.u8(0); // TAG_End
    }

    #[test]
    fn decodes_position_type_and_nbt() {
        let mut w = Writer::default();
        w.u8(0x3A); // packedXZ: x=3, z=10
        w.i16(72); // y
        w.var_i32(9); // type id
        sample_nbt_bytes(&mut w);

        let mut r = Reader::new(w.as_slice());
        let be = BlockEntity::decode(&mut r).expect("decode");
        assert!(r.is_empty());
        assert_eq!(be.rel_x, 3);
        assert_eq!(be.rel_z, 10);
        assert_eq!(be.y, 72);
        assert_eq!(be.type_id, 9);
        assert_eq!(be.nbt, Nbt::Compound(vec![("Id".to_string(), Nbt::Int(5))]));
    }

    #[test]
    fn empty_nbt_decodes_as_end() {
        let mut w = Writer::default();
        w.u8(0x00);
        w.i16(0);
        w.var_i32(1);
        w.u8(0); // bare TAG_End => no data
        let mut r = Reader::new(w.as_slice());
        let be = BlockEntity::decode(&mut r).expect("decode");
        assert_eq!(be.nbt, Nbt::End);
    }

    #[test]
    fn decodes_a_list() {
        let mut w = Writer::default();
        w.var_i32(2);
        for i in 0..2u8 {
            w.u8(i);
            w.i16(i16::from(i));
            w.var_i32(u32::from(i) as i32);
            w.u8(0);
        }
        let mut r = Reader::new(w.as_slice());
        let list = BlockEntity::decode_list(&mut r).expect("decode");
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].type_id, 1);
    }
}
