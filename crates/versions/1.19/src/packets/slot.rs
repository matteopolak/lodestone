//! The 1.13.1+ `slot` wire type — a single inventory item stack.
//!
//! # Why this is hand-written and not derived
//!
//! A slot is a *tagged* structure whose tail depends on its head: a leading
//! `present` boolean says whether an item follows; when it does, a VarInt item
//! id, a byte count and an **optional NBT tag** follow. That "field present iff
//! a previous field has a particular value" shape has no derive attribute (it is
//! the same `switch` the modern component list would need), so it is an explicit
//! `Encode`/`Decode` pair. Because it implements those traits, packets can hold
//! a `Slot` (or `Vec<Slot>`) and still derive their own codec.
//!
//! # 1.13 flattening: this is *not* the pre-1.13 slot
//!
//! Before 1.13 a slot led with a signed `i16` block id (`-1` = empty) and
//! carried a separate `damage` (metadata) `i16`. The 1.13 flattening removed the
//! `(id, meta)` split entirely: an item is now a single flat registry id, so
//! this format is `present: bool`, `id: VarInt`, `count: i8`, `nbt`. There is no
//! `damage` field. This crate therefore cannot share `Slot` with the pre-1.13
//! v1-8/v1-9 crates — the wire shapes genuinely disagree.
//!
//! # The NBT-as-raw-bytes decision (a `lodestone-core` seam)
//!
//! `lodestone-core` exposes NBT *reading* ([`read_named_nbt`]) but no NBT
//! *writing*. A slot must round-trip losslessly (the client echoes items back to
//! the server on a window click), so this type stores the item's NBT as the
//! **raw wire bytes** rather than a parsed [`Nbt`](lodestone_core::Nbt) tree:
//! decode captures the exact byte span the reader consumed, and encode writes it
//! back verbatim. This is lossless and correct for echoing server-sent items;
//! constructing an item with *fresh* NBT from scratch would need an NBT writer,
//! which is the seam to report if a higher layer ever needs it. `None` is the
//! single `0x00` (`TAG_End`) "no NBT" marker.

use lodestone_core::{Ctx, Decode, Encode, Error, Reader, Result, Writer, read_named_nbt};

/// A single inventory slot: either empty or an item stack with optional NBT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    /// An empty slot (`present == false`).
    Empty,
    /// An occupied slot.
    Item {
        /// Flat item registry id (1.13+ numbering, VarInt on the wire).
        id: i32,
        /// Stack size.
        count: i8,
        /// Raw NBT tag bytes (`None` for the `0x00` no-NBT marker). Stored as
        /// bytes because `lodestone-core` has no NBT writer; see the module docs.
        nbt: Option<Vec<u8>>,
    },
}

impl Slot {
    /// Whether this slot is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Slot::Empty)
    }
}

impl Encode for Slot {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        match self {
            Slot::Empty => w.bool(false),
            Slot::Item { id, count, nbt } => {
                w.bool(true);
                w.var_i32(*id);
                w.i8(*count);
                match nbt {
                    None => w.u8(0), // TAG_End: no NBT
                    Some(raw) => w.bytes(raw),
                }
            }
        }
        Ok(())
    }
}

impl Decode for Slot {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let present = r.bool()?;
        if !present {
            return Ok(Slot::Empty);
        }
        let id = r.var_i32()?;
        let count = r.i8()?;
        let nbt = decode_optional_nbt(r)?;
        Ok(Slot::Item { id, count, nbt })
    }
}

/// Reads a slot's optional NBT: a lone `0x00` (`TAG_End`) means "no NBT";
/// anything else is a full named compound tag, captured as raw bytes.
///
/// `remaining_bytes` returns a slice tied to the underlying buffer lifetime (not
/// the reader borrow), so the pre-read span stays valid across the parse and the
/// exact consumed length can be sliced out afterwards.
fn decode_optional_nbt(r: &mut Reader<'_>) -> Result<Option<Vec<u8>>> {
    let &first = r.remaining_bytes().first().ok_or(Error::UnexpectedEof)?;
    if first == 0 {
        r.u8()?; // consume the TAG_End marker
        return Ok(None);
    }
    let before = r.remaining_bytes();
    let start_len = before.len();
    read_named_nbt(r)?; // advances the cursor past the whole tag
    let consumed = start_len - r.remaining_bytes().len();
    Ok(Some(before[..consumed].to_vec()))
}
