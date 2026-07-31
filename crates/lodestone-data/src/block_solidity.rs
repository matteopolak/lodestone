//! Per-block-state `legacySolid` / `blocksMotion` flags for protocol 776
//! (Minecraft 26.2) — the one block fact that decides whether an entity is
//! stopped by a cell, and the one that **cannot** be derived from the collision
//! census next door.
//!
//! # Why this table has to exist
//!
//! `BlockState.blocksMotion()` is
//! `block != COBWEB && block != BAMBOO_SAPLING && isSolid()`
//! (`BlockBehaviour.java:541-545`), and `isSolid()` is a plain read of the cached
//! `legacySolid` field (`BlockBehaviour.java:547-550`) computed once per state by
//! `calculateSolid()` (`BlockBehaviour.java:484-504`):
//!
//! ```text
//! if (properties.forceSolidOn)  return true;
//! if (properties.forceSolidOff) return false;
//! if (cache == null)            return false;          // dynamicShape blocks
//! if (collisionShape.isEmpty()) return false;
//! return bounds.getSize() >= 0.7291666666666666 || bounds.getYsize() >= 1.0;
//! ```
//!
//! Only the last line is geometry. The first two branches read
//! `BlockBehaviour.Properties.forceSolidOn` / `forceSolidOff`, which have **no
//! getter**, are absent from `blocks.json`, and are set on **237** and **8**
//! blocks respectively in 26.2. The third fires for the 23 `dynamicShape()`
//! blocks, whose `Cache` is never built (`BlockBehaviour.java:509-511`) — so
//! their `legacySolid` ignores the shape they nonetheless report.
//!
//! Deriving solidity from the shape instead gets **2,742 of 32,366 states across
//! 222 blocks** wrong: every sign, hanging sign, banner, pressure plate, chain,
//! *open* fence gate, lantern, shulker box, cobweb, bamboo and pointed dripstone
//! reads as non-blocking when vanilla blocks motion, and ladder, snow, azalea,
//! big dripleaf, chorus plant/flower and end rod read as blocking when vanilla
//! does not. `tests/block_physics.rs` measures that number against the committed
//! census rather than restating it.
//!
//! `0.7291666666666666` is exactly `(1 + 1 + 3/16) / 3` — the mean extent of a
//! ladder's collision box. The constant exists *because* a ladder lands on it,
//! and `forceSolidOff()` on `Blocks.LADDER` exists because landing on it gives
//! the wrong answer.
//!
//! # Data source
//!
//! Dumped from the real 26.2 server (`BlockPhysicsOracle.java`, walking
//! `Block.BLOCK_STATE_REGISTRY` after `SharedConstants::tryDetectVersion` +
//! `Bootstrap::bootStrap`), same as [`crate::hardness`] and
//! [`crate::outline_shapes`]. See `tests/block_physics.rs` for the generator, the
//! drift guard and `LODESTONE_REGEN=1`.
//!
//! # Memory design
//!
//! Two bitsets, 4,046 bytes each — pure rodata, no heap, O(1) by id. A `[bool;
//! 32366]` would cost 8× that for the same information.

use crate::generated_block_solidity as table;

pub use table::STATE_COUNT;

/// Reads bit `id` out of a packed little-endian-within-byte bitset.
fn bit(bits: &[u8], id: u32) -> Option<bool> {
    if id >= STATE_COUNT {
        return None;
    }
    let byte = *bits.get((id / 8) as usize)?;
    Some(byte & (1u8 << (id % 8)) != 0)
}

/// Vanilla `BlockState.isSolid()` — the raw cached `legacySolid` flag — for
/// block-state `id`, or `None` if `id` is not in `0..`[`STATE_COUNT`].
///
/// Prefer [`blocks_motion`] for physics. This is exposed because `isSolid()` has
/// several vanilla consumers that are *not* motion blocking (replaceability
/// checks, `BlockBehaviour.java:269`), and because it is the value the drift
/// guard compares a shape-derived answer against.
#[must_use]
pub fn legacy_solid(id: u32) -> Option<bool> {
    bit(&table::LEGACY_SOLID, id)
}

/// Vanilla `BlockState.blocksMotion()` for block-state `id`, or `None` if `id`
/// is not in `0..`[`STATE_COUNT`].
///
/// This is [`legacy_solid`] with vanilla's two hard-coded exclusions already
/// folded in — in 26.2 they differ on exactly **two** states,
/// `minecraft:cobweb` and `minecraft:bamboo_sapling`, both of which are
/// single-state blocks. Do not re-apply the exclusions on top.
#[must_use]
pub fn blocks_motion(id: u32) -> Option<bool> {
    bit(&table::BLOCKS_MOTION, id)
}
