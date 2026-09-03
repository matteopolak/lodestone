//! Hand-written codec for the 1.14+ packed block `position` type.
//!
//! # Architectural finding (macro gap)
//!
//! Every other field in this crate is expressed with ordinary struct fields and
//! the derive macro. The `position` type cannot be, because it is a *bit-packed*
//! value: three signed integers (x: 26 bits, z: 26 bits, y: 12 bits) packed into
//! a single big-endian `i64`. The `#[mc(...)]` attribute set (`varint`,
//! `varlong`, `len`, `max`, `fixed`, `remaining`, ...) has no way to describe a
//! sub-byte bitfield, so this is the one place a hand-written
//! [`lodestone_core::Encode`]/[`lodestone_core::Decode`] pair is unavoidable.
//!
//! This is reported to the architecture owners as a missing derive attribute
//! (for example `#[mc(position = "1.8")]` / `#[mc(position = "1.14")]`, or a
//! general `#[mc(bits(x = 26, z = 26, y = 12))]`), because the *only* thing
//! that differs between old and modern positions is the field order of the
//! packing: 1.8 through 1.13 pack `x, y, z` (y in the middle) whereas 1.14+
//! (which is every protocol in this era) packs `x, z, y` (y in the low bits). A subtly wrong
//! bit layout is invisible to a round-trip test, which is why this module is
//! covered by byte-level golden tests.

use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};
use lodestone_model::BlockPos;

const X_BITS: u32 = 26;
const Y_BITS: u32 = 12;
const Z_BITS: u32 = 26;

const X_SHIFT: u32 = Z_BITS + Y_BITS; // 38: x occupies the top 26 bits
const Z_SHIFT: u32 = Y_BITS; // 12: z occupies the middle 26 bits

const X_MASK: i64 = (1 << X_BITS) - 1;
const Y_MASK: i64 = (1 << Y_BITS) - 1;
const Z_MASK: i64 = (1 << Z_BITS) - 1;

/// Packs a [`BlockPos`] into the 1.14+ `position` `i64` representation.
///
/// The 1.14+ layout is `x(26) | z(26) | y(12)` from the most significant bit
/// down, i.e. y occupies the low bits. This differs from the pre-1.14 layout,
/// which packed `x(26) | y(12) | z(26)` (y in the middle).
#[must_use]
pub fn pack_position(pos: BlockPos) -> i64 {
    ((i64::from(pos.x) & X_MASK) << X_SHIFT)
        | ((i64::from(pos.z) & Z_MASK) << Z_SHIFT)
        | (i64::from(pos.y) & Y_MASK)
}

/// Unpacks a 1.14+ `position` `i64` into a [`BlockPos`], sign-extending each of
/// the three packed signed integers.
#[must_use]
pub fn unpack_position(value: i64) -> BlockPos {
    // For a field of `bits` width at shift `s`, `(value << (64 - s - bits)) >>
    // (64 - bits)` sign-extends the packed signed integer via arithmetic shifts.
    let x = (value >> X_SHIFT) as i32;
    let z = ((value << (i64::BITS - Z_SHIFT - Z_BITS)) >> (i64::BITS - Z_BITS)) as i32;
    let y = ((value << (i64::BITS - Y_BITS)) >> (i64::BITS - Y_BITS)) as i32;
    BlockPos::new(x, y, z)
}

/// A block position encoded using the 1.14+ packed `position` wire type.
///
/// This newtype exists purely so the packed codec participates in the derive
/// machinery like any other field type: structs can hold a `Position` and still
/// derive `Encode`/`Decode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Position(pub BlockPos);

impl Position {
    /// Creates a packed position from raw block coordinates.
    #[must_use]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self(BlockPos::new(x, y, z))
    }
}

impl From<BlockPos> for Position {
    fn from(pos: BlockPos) -> Self {
        Self(pos)
    }
}

impl From<Position> for BlockPos {
    fn from(position: Position) -> Self {
        position.0
    }
}

impl Encode for Position {
    fn encode(&self, w: &mut Writer, _ctx: Ctx) -> Result<()> {
        w.i64(pack_position(self.0));
        Ok(())
    }
}

impl Decode for Position {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        Ok(Self(unpack_position(r.i64()?)))
    }
}
