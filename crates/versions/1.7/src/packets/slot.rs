//! Item stacks on the protocol 5 wire.
//!
//! # Why this cannot be the shared `Slot`
//!
//! The field list is the same as protocol 47's -- `i16` id, then count,
//! damage and NBT when the id is not `-1` -- and `cargo xtask protocol-dup`'s
//! struct scan would call the two identical. The NBT field is not the same
//! thing:
//!
//! - **protocol 5** carries an `i16` byte count followed by that many bytes of
//!   **gzip**-compressed NBT, with `-1` meaning absent.
//! - **protocol 47** carries a bare optional tag: one `0` byte for absent, or
//!   an uncompressed named compound in place.
//!
//! Both encodings start with a byte that is frequently zero, so a decoder for
//! one reads the other's empty case without error and then mis-frames every
//! stack after it in a `window_items` array. This is the trap the shared
//! crate's module docs warn about for hand-decoded types, in its sharpest
//! form, which is why this module is local and hand-written.
//!
//! # What reaches canonical state
//!
//! Nothing NBT-shaped, yet. The canonical `ItemStack` carries an item key and
//! a count; it has no carrier for the damage value that identifies a variant
//! before the Flattening, and none for stack NBT. The compressed blob is
//! therefore consumed and its length recorded, so that the framing is right
//! and the loss is visible, rather than skipped by seeking to the end of the
//! packet.

use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer};

/// Sentinel id for an empty slot.
pub const EMPTY_ID: i16 = -1;

/// Sentinel NBT length for a stack with no tag.
const NO_NBT: i16 = -1;

/// Largest stack NBT blob accepted, in compressed bytes.
const MAX_NBT_BYTES: usize = 32 * 1024;

/// One item stack, or the empty slot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Slot {
    /// Numeric item id, or `None` for an empty slot.
    pub id: Option<i16>,
    /// Stack size. Meaningless when `id` is `None`.
    pub count: i8,
    /// Damage/variant value. Pre-Flattening this selects the variant for
    /// items such as dyes and logs, and the canonical model has nowhere to
    /// put it.
    pub damage: i16,
    /// Length in bytes of the gzip-compressed NBT blob that followed, or
    /// `None` when the stack carried no tag.
    ///
    /// The blob's *contents* are not retained: nothing downstream can consume
    /// them. The length is, because it is the only evidence at this layer
    /// that a tag was present and how much was dropped.
    pub nbt_bytes: Option<usize>,
}

impl Slot {
    /// Whether this slot holds nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.id.is_none()
    }
}

impl Decode for Slot {
    fn decode(reader: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let id = reader.i16()?;
        if id == EMPTY_ID {
            return Ok(Self::default());
        }
        let count = reader.i8()?;
        let damage = reader.i16()?;
        let declared = reader.i16()?;
        let nbt_bytes = if declared == NO_NBT {
            None
        } else {
            let length =
                usize::try_from(declared).map_err(|_| Error::NegativeLength(i32::from(declared)))?;
            if length > MAX_NBT_BYTES {
                return Err(Error::LimitExceeded {
                    limit: MAX_NBT_BYTES,
                    actual: length,
                });
            }
            // Consumed, not skipped: the reader must end up exactly past the
            // blob so the next stack in an array starts in the right place.
            let _compressed = reader.bytes(length)?;
            Some(length)
        };
        Ok(Self {
            id: Some(id),
            count,
            damage,
            nbt_bytes,
        })
    }
}

impl Encode for Slot {
    fn encode(&self, writer: &mut Writer, _ctx: Ctx) -> Result<()> {
        match self.id {
            None => {
                writer.i16(EMPTY_ID);
                Ok(())
            }
            Some(id) => {
                writer.i16(id);
                writer.i8(self.count);
                writer.i16(self.damage);
                // A stack this crate constructs never carries a tag: the
                // decoder keeps only the blob's length, so there is nothing
                // to re-encode. Writing the absent sentinel is the only
                // honest option, and it is what an item this client places
                // or moves actually needs.
                writer.i16(NO_NBT);
                Ok(())
            }
        }
    }
}
