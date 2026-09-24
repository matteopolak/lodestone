//! The four per-block-state facts vanilla's own "freeze top layer" worldgen
//! feature (a snow-and-freeze feature) needs and that no other
//! census in this crate carries, plus the default-state key its consumer needs to
//! look them up, for protocol 776 (Minecraft 26.2).
//!
//! # Why this table has to exist
//!
//! Vanilla's own snow-and-freeze feature's place step asks four
//! questions of the block field, none of them answerable from `blocks.json`:
//!
//! | fn | vanilla expression | used for |
//! |---|---|---|
//! | [`face_full_up`] | is the collision shape's up-face full | the snow-layer block's own "can survive" check |
//! | [`has_fluid_state`] | `!state.getFluidState().isEmpty()` | the second half of the motion-blocking heightmap predicate |
//! | [`is_water_source_liquid_block`] | the fluid state is water **and** the block is a liquid block | which blocks turn to ice (vanilla's own biome "should freeze" check) |
//! | [`has_snowy_property`] | the state carries the "snowy" property | the `snowy` flip under a placed snow layer, also in the place step above |
//!
//! [`crate::block_solidity::blocks_motion`] already carries the *first* half of
//! the `MOTION_BLOCKING` predicate, and [`crate::collision_shapes`] carries the
//! collision *geometry* — but neither answers the questions above, for reasons
//! the dump makes measurable rather than assertable:
//!
//! * **`face_full_up` is not "is it a full cube".** Only **6,359** of 32,366
//!   states have a full UP collision face — a minority, which is itself
//!   surprising, and it does not coincide with "the collision shape is one
//!   unit box" either. `tests/snow_support.rs` measures both counts and their
//!   disagreement rather than restating them here. The predicate is a
//!   shape-join emptiness test between a full cube and the shape's own
//!   up-face slice, over the *discretised* voxel-shape grid, after
//!   vanilla's own face-calculation step's three-way branch on whether the
//!   shape is cube-like along Y / an empty slice / a cube-like slice.
//!   Re-deriving that
//!   from the AABB list [`crate::collision_shapes`] holds means re-implementing
//!   vanilla's own slice-shape and cube-point-range helpers and their `1.0E-7`
//!   fuzzy comparisons. That
//!   is a hand-rolled geometry pass, and this repo has already shipped a
//!   hand-rolled lexer that was silently wrong about lifetimes; asking the jar
//!   costs one column.
//!
//! * **`is_water_source_liquid_block` is true for exactly ONE state.** Not
//!   "every water block" and not "every waterlogged block": still water is
//!   the *source* fluid, so `water[level=1..15]` is the flowing-water fluid and
//!   fails the source-fluid identity test, while a waterlogged stair passes the fluid test
//!   and fails the liquid-block instance check. The single state is
//!   `minecraft:water[level=0]`. Ice therefore forms only on still source
//!   water, and any hand-written approximation of this predicate ("is it
//!   water?") freezes flowing water that vanilla leaves alone.
//!
//! * **`has_snowy_property` is true for exactly SIX states** — `grass_block`,
//!   `podzol` and `mycelium`, two states each. This one *is* derivable from
//!   [`crate::block_states::properties`], and `tests/snow_support.rs` asserts
//!   exactly that agreement. The column exists so "derivable" is a measurement
//!   rather than a claim.
//!
//! # Known scope
//!
//! The shape behind [`face_full_up`] is read at an empty block-getter
//! singleton and the origin position, so neighbour-dependent geometry (fences, walls, panes)
//! reports its no-neighbour shape — the same convention
//! [`crate::collision_shapes`] uses, and the dump carries the dynamic-shape
//! candidate set so that scope is checkable rather than asserted.
//!
//! # Data source
//!
//! Dumped from the real 26.2 server (`oracle-java/SnowSupportOracle.java`,
//! walking vanilla's own block-state registry after booting the server
//! headlessly), same as [`crate::block_solidity`] and
//! [`crate::shade_brightness`]. See `tests/snow_support.rs` for the generator,
//! the drift guard and `LODESTONE_REGEN=1`.
//!
//! # Memory design
//!
//! Five bitsets, 4,046 bytes each — pure rodata, no heap, O(1) by id. The fifth,
//! [`is_default_state`], is not a "freeze top layer" predicate; see its own doc.

use crate::block_states::StateId;
use crate::generated_snow_support as table;

pub use table::STATE_COUNT;

/// Reads `id` from a complete packed little-endian-within-byte bitset.
fn bit(bits: &[u8], id: StateId) -> bool {
    let raw = id.raw();
    let byte = bits[(raw / 8) as usize];
    byte & (1u8 << (raw % 8)) != 0
}

/// Vanilla's own "is face full" check against the state's collision shape and
/// the up direction, for validated block-state `id`.
///
/// This is the geometric half of the snow-layer block's own "can survive" check: a snow layer
/// survives on a block whose collision shape presents a full 1×1 square at its
/// top face. **Not** the same question as "is the collision shape a full block"
/// — see the module doc.
#[must_use]
pub fn face_full_up(id: StateId) -> bool {
    bit(&table::FACE_FULL_UP, id)
}

/// Vanilla `!state.getFluidState().isEmpty()` for validated block-state `id`.
///
/// The second half of the motion-blocking heightmap predicate
/// (`input.blocksMotion() || !input.getFluidState()
/// .isEmpty()`); combine with [`crate::block_solidity::blocks_motion`] for the
/// whole thing. True for still and flowing water and lava **and** every
/// waterlogged state, which is why it is broader than
/// [`is_water_source_liquid_block`].
#[must_use]
pub fn has_fluid_state(id: StateId) -> bool {
    bit(&table::HAS_FLUID_STATE, id)
}

/// Vanilla's own "is source water and a liquid block" check for validated
/// block-state `id`.
///
/// Exactly the condition vanilla's own biome "should freeze" check puts on a
/// block before it becomes ice. True for **one** state in 26.2,
/// `minecraft:water[level=0]` — see the module doc for why that is not a bug in
/// the dump.
#[must_use]
pub fn is_water_source_liquid_block(id: StateId) -> bool {
    bit(&table::IS_WATER_SOURCE_LIQUID_BLOCK, id)
}

/// Vanilla's own "has snowy property" check for validated block-state `id`.
///
/// Vanilla's own snow-and-freeze feature flips this property to `true` on the block it puts a
/// snow layer on (in its own place step); a port that skips the flip
/// leaves visibly wrong terrain (a green grass top under snow) even when the
/// snow itself is placed correctly.
#[must_use]
pub fn has_snowy_property(id: StateId) -> bool {
    bit(&table::HAS_SNOWY_PROPERTY, id)
}

/// Vanilla `state == state.getBlock().defaultBlockState()` for validated
/// block-state `id`. Exactly one state per block is set, so a single walk of
/// `0..STATE_COUNT` recovers every block's default with no name lookup.
///
/// This is not a `freeze_top_layer` predicate — it is the key its consumer needs.
/// `lodestone-worldgen` emits fluids without their `level` property
/// (`docs/worldgen-parity.md`'s "Known representation gap"), so a generated
/// column's water reads as `minecraft:water`; since
/// [`is_water_source_liquid_block`] is true for exactly one water state, a
/// property-less lookup must resolve to the block's **default** state or no ocean
/// ever freezes. `blocks.json` does carry a `"default": true` flag per block, but
/// [`crate::block_states`]' extraction did not retain it.
#[must_use]
pub fn is_default_state(id: StateId) -> bool {
    bit(&table::IS_DEFAULT_STATE, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validated_state_lookups_are_total_and_invalid_raw_ids_stop_at_boundary() {
        let state = StateId::new(0).expect("state zero is valid");
        let lookups: [fn(StateId) -> bool; 5] = [
            face_full_up,
            has_fluid_state,
            is_water_source_liquid_block,
            has_snowy_property,
            is_default_state,
        ];
        for lookup in lookups {
            let _ = lookup(state);
        }
        assert!(StateId::new(STATE_COUNT).is_none());
        assert!(StateId::new(u32::MAX).is_none());
    }

    /// Every column must be non-degenerate: neither all-zero nor all-one. A
    /// bitset makes shipping an all-zero table easy (a decode bug in the
    /// generator produces one silently), and an all-zero `face_full_up` would
    /// mean snow never survives anywhere — a plausible-looking empty world
    /// rather than a crash.
    #[test]
    fn every_column_has_both_values() {
        for (name, f) in [
            ("face_full_up", face_full_up as fn(StateId) -> bool),
            ("has_fluid_state", has_fluid_state),
            (
                "is_water_source_liquid_block",
                is_water_source_liquid_block,
            ),
            ("has_snowy_property", has_snowy_property),
            ("is_default_state", is_default_state),
        ] {
            let set = (0..STATE_COUNT)
                .map(|raw| StateId::new(raw).expect("census range is valid"))
                .filter(|&id| f(id))
                .count();
            assert!(set > 0, "{name} is all-zero across {STATE_COUNT} states");
            assert!(
                set < STATE_COUNT as usize,
                "{name} is all-one across {STATE_COUNT} states"
            );
        }
    }
}
