//! The pre-1.14 packed block `position` wire type, and the two packets that
//! embed it.
//!
//! # Why this is shared only 47..=340, not with v1-14
//!
//! `cargo xtask protocol-dup`'s struct-identity scan reports `Position` (and
//! `BlockDig`/`SpawnPosition`, which embed it) as identical across all three
//! legacy families -- but that scan compares only a struct's own field list,
//! never a hand-written `Encode`/`Decode` impl beside it. Diffed by hand:
//! v1-14's packed-position codec genuinely differs. 1.8 through 1.13
//! (protocols 47 and 340) pack `x(26) | y(12) | z(26)` from the most
//! significant bit down (y in the middle); 1.14+ (protocol 754, this crate's
//! v1-14) repacks to `x(26) | z(26) | y(12)` (y in the low bits). Same
//! newtype shape, same field name in every crate, **incompatible bytes on
//! the wire**. This is a measured instance of the class CLAUDE.md calls out
//! generally: a naive detector sees a matching field list and misses a
//! divergent implementation behind it.
//!
//! So `Position`, `BlockDig` and `SpawnPosition` are shared only between v1-8
//! and v1-9. `Position` has no `Packet` derive (it is a field type embedded
//! in other packets, not a packet itself) so it has no
//! `#[mc(protocols = ...)]` to enforce this -- this doc comment is the only
//! place the range is recorded, and v1-14 must keep (and does keep) its own
//! incompatible `Position` type rather than importing this one.

use lodestone_core::{Ctx, Decode, Encode, Reader, Result, Writer};
use lodestone_macros::{Decode, Encode, Packet};
use lodestone_model::BlockPos;

const X_BITS: u32 = 26;
const Y_BITS: u32 = 12;
const Z_BITS: u32 = 26;

const X_SHIFT: u32 = Y_BITS + Z_BITS; // 38
const Y_SHIFT: u32 = Z_BITS; // 26

const X_MASK: i64 = (1 << X_BITS) - 1;
const Y_MASK: i64 = (1 << Y_BITS) - 1;
const Z_MASK: i64 = (1 << Z_BITS) - 1;

/// Packs a [`BlockPos`] into the pre-1.14 `position` `i64` representation.
///
/// The pre-1.14 layout is `x(26) | y(12) | z(26)` from the most significant
/// bit down, i.e. y occupies the middle bits. This differs from the 1.14+
/// layout, which packs `x(26) | z(26) | y(12)` -- v1-14 (protocol 754) keeps
/// its own copy of this function with that later layout; see the module
/// docs for why the two cannot share one definition.
#[must_use]
pub fn pack_position(pos: BlockPos) -> i64 {
    ((i64::from(pos.x) & X_MASK) << X_SHIFT)
        | ((i64::from(pos.y) & Y_MASK) << Y_SHIFT)
        | (i64::from(pos.z) & Z_MASK)
}

/// Unpacks a pre-1.14 `position` `i64` into a [`BlockPos`], sign-extending
/// each of the three packed signed integers.
#[must_use]
pub fn unpack_position(value: i64) -> BlockPos {
    // Arithmetic left/right shifts sign-extend the value that lands in the top
    // bit, recovering the original signed integer for each packed field.
    let x = (value >> X_SHIFT) as i32;
    let y = ((value << (i64::BITS - X_SHIFT)) >> (i64::BITS - Y_BITS)) as i32;
    let z = ((value << (i64::BITS - Z_BITS)) >> (i64::BITS - Z_BITS)) as i32;
    BlockPos::new(x, y, z)
}

/// A block position encoded using the pre-1.14 packed `position` wire type.
///
/// This newtype exists purely so the packed codec participates in the derive
/// machinery like any other field type: structs can hold a `Position` and still
/// derive `Encode`/`Decode`. Shared only 47..=340 -- see the module docs.
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

/// Serverbound `block_dig` (player digging) -- start, cancel, or finish
/// breaking a block, plus the drop/shoot/eat status codes that share this
/// packet. Shared only 47..=340 -- see the module docs (it embeds
/// [`Position`]).
///
/// # Divergence
///
/// This era folds block breaking **and** item dropping / bow release /
/// eating into a single packet distinguished by `status` (modern 26.2
/// splits several of these into `player_action` ordinals). Status codes:
/// `0` start, `1` cancel, `2` finish, `3` drop stack, `4` drop item, `5`
/// shoot arrow / finish eating. There is no block-prediction `sequence`
/// (added in 1.19), so the model's `sequence` is dropped deliberately by the
/// adapter.
///
/// Wire layout: varint status, packed `position`, signed-byte face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:block_dig", state = Play, bound = Server, protocols = "47..=340")]
pub struct BlockDig {
    /// Digging status code.
    #[mc(varint)]
    pub status: i32,
    /// Target block position.
    pub location: Position,
    /// Face being mined (`0..=5`).
    pub face: i8,
}

/// Clientbound `spawn_position` packet setting the client's compass target.
/// Shared only 47..=340 -- see the module docs (it embeds [`Position`]).
///
/// Wire layout: a single packed pre-1.14 [`Position`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:spawn_position", state = Play, bound = Client, protocols = "47..=340")]
pub struct SpawnPosition {
    /// Compass target block position.
    pub location: Position,
}
