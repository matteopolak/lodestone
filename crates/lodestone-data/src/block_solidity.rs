//! Per-block-state "legacy solid" / "blocks motion" flags for protocol 776
//! (Minecraft 26.2) — the one block fact that decides whether an entity is
//! stopped by a cell, and the one that **cannot** be derived from the collision
//! census next door.
//!
//! # Why this table has to exist
//!
//! Vanilla's own "blocks motion" accessor is
//! `block != COBWEB && block != BAMBOO_SAPLING && isSolid()`, and its own
//! "is solid" accessor is a plain read of a cached "legacy solid" field
//! computed once per state by a "calculate solid" step:
//!
//! ```text
//! if (properties.forceSolidOn)  return true;
//! if (properties.forceSolidOff) return false;
//! if (cache == null)            return false;          // dynamic-shape blocks
//! if (collisionShape.isEmpty()) return false;
//! return bounds.getSize() >= 0.7291666666666666 || bounds.getYsize() >= 1.0;
//! ```
//!
//! Only the last line is geometry. The first two branches read vanilla's own
//! block-properties builder's "force solid on"/"force solid off" flags, which have **no
//! getter**, are absent from `blocks.json`, and are set on **237** and **8**
//! blocks respectively in 26.2. The third fires for the 23 blocks vanilla marks
//! as having a dynamic shape, whose cache is never built (vanilla's own
//! cache-init step skips it for those) — so
//! their "legacy solid" flag ignores the shape they nonetheless report.
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
//! and the ladder block's own "force solid off" flag exists because landing on
//! it gives the wrong answer.
//!
//! # Data source
//!
//! Dumped from the real 26.2 server (`BlockPhysicsOracle.java`, walking
//! vanilla's own block-state registry after booting the server headlessly),
//! same as [`crate::hardness`] and
//! [`crate::outline_shapes`]. See `tests/block_physics.rs` for the generator, the
//! drift guard and `LODESTONE_REGEN=1`.
//!
//! # Memory design
//!
//! Two bitsets, 4,046 bytes each — pure rodata, no heap, O(1) by id. A `[bool;
//! 32366]` would cost 8× that for the same information.

use crate::generated_block_solidity as table;
use crate::block_states::StateId;

pub use table::STATE_COUNT;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_id_boundary_rejects_invalid_raw_ids_and_lookup_is_total() {
        assert!(StateId::new(STATE_COUNT).is_none());
        assert!(StateId::new(u32::MAX).is_none());

        let valid = StateId::new(0).expect("state zero is in the generated table");
        assert!(!legacy_solid(valid));
    }
}

/// Reads a validated state bit out of a packed little-endian-within-byte bitset.
fn bit(bits: &[u8], id: StateId) -> bool {
    let raw = id.raw();
    let byte = bits[(raw / 8) as usize];
    byte & (1u8 << (raw % 8)) != 0
}

/// The complete-table "is solid" flag for a validated block-state id.
///
/// Prefer [`blocks_motion`] for physics. This is exposed because "is solid" has
/// several vanilla consumers that are *not* motion blocking (replaceability
/// checks, a can-be-replaced-by-fluid check), and because it is the value the drift
/// guard compares a shape-derived answer against.
#[must_use]
pub fn legacy_solid(id: StateId) -> bool {
    bit(&table::LEGACY_SOLID, id)
}

/// Raw-id compatibility boundary for callers that have not validated a
/// block-state id. New code should construct [`StateId`] and call
/// [`legacy_solid`] so this complete table is accessed with a total lookup.
#[must_use]
pub fn legacy_solid_raw(id: u32) -> Option<bool> {
    StateId::new(id).map(legacy_solid)
}

/// Vanilla's own "blocks motion" accessor for block-state `id`, or `None` if `id`
/// is not in `0..`[`STATE_COUNT`].
///
/// This is [`legacy_solid`] with vanilla's two hard-coded exclusions already
/// folded in — in 26.2 they differ on exactly **two** states,
/// `minecraft:cobweb` and `minecraft:bamboo_sapling`, both of which are
/// single-state blocks. Do not re-apply the exclusions on top.
#[must_use]
pub fn blocks_motion(id: u32) -> Option<bool> {
    StateId::new(id).map(|id| bit(&table::BLOCKS_MOTION, id))
}
