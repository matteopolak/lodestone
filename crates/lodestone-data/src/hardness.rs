//! Per-block-state hardness (vanilla's own "destroy speed" field) and correct-tool
//! requirement for protocol 776 (Minecraft 26.2).
//!
//! `lodestone-game`'s `mining` module (`BreakInputs`) already replays
//! vanilla's break-time math bit-exactly (the 30/100 correct-tool divider, the
//! `destroy_stage()` 0-9 mapping, `hardness == -1.0` meaning unbreakable) —
//! what it does not own is the *data*: which block state has which hardness,
//! and which requires the correct tool for drops. That is exactly the seam
//! this module fills, mirroring [`crate::collision_shapes`] and
//! [`crate::entity_dimensions`].
//!
//! # Data source: interrogate the real jar, not `minecraft-data`
//!
//! `blocks.json` (Mojang's data-generator report) has **no destroy-speed
//! field** — it is block *properties* only, so there is no property-derived
//! shortcut. `vendor/minecraft-data` was measured stale/incomplete for 26.2 on
//! the neighbouring collision-shape table (see [`crate::collision_shapes`]
//! module docs: ~92.3% state coverage, 30 blocks missing by name), and there is
//! no reason to assume its per-block hardness numbers are any fresher. So, as
//! with collision shapes and entity dimensions, the only authoritative source
//! is the running game: the table is generated from a dump produced by booting
//! the real 26.2 server headlessly and asking every one of the 32,366 block
//! states in
//! vanilla's own block-state registry for its own "get destroy speed" accessor
//! (called with a null getter and the origin position) and its own
//! "requires correct tool for drops" check — see `tests/hardness.rs` for the generator
//! and drift guard, and `oracle-java/HardnessOracle.java` for why a `null`
//! block getter is a faithful stand-in (no block subclass overrides either
//! method with world/neighbour dependence).
//!
//! # Memory design
//!
//! Pure rodata, zero heap, O(1) by id, identical in shape to
//! [`crate::collision_shapes`]: the 32,366 states collapse to a few dozen
//! distinct `(hardness, requires_correct_tool)` pairs (most blocks share a
//! handful of tiers — dirt-like, stone-like, wood-like, and so on), so a state
//! maps to a `u16` index into that small table.

use crate::block_states::StateId;
use crate::generated_hardness as table;

pub use table::STATE_COUNT;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_id_boundary_rejects_invalid_raw_ids_and_lookup_is_total() {
        assert!(StateId::new(STATE_COUNT).is_none());
        assert!(StateId::new(u32::MAX).is_none());

        let valid = StateId::new(0).expect("state zero is in the generated table");
        assert!(hardness(valid).hardness.is_finite());
    }
}

/// A block state's break-time inputs: vanilla's own "destroy speed" field (hardness) and
/// whether the correct tool is required for drops.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hardness {
    /// Vanilla's own "get destroy speed" accessor (vanilla's field name for hardness).
    /// `-1.0` marks an unbreakable block (bedrock, barrier, ...).
    pub hardness: f32,
    /// Vanilla's own "requires correct tool for drops" check. Selects vanilla's `30` vs
    /// `100` break-speed divider (see `lodestone-game`'s `mining` module).
    pub requires_correct_tool: bool,
}

/// The hardness/correct-tool data for block-state `id`, or `None` if `id` is
/// not in `0..`[`STATE_COUNT`].
///
/// Zero-heap: reads straight from rodata. O(1) indexing, no search.
#[must_use]
pub fn hardness(id: StateId) -> Hardness {
    let entry = table::STATE_ENTRY[id.raw() as usize];
    let (hardness, requires_correct_tool) = table::ENTRIES[entry as usize];
    Hardness {
        hardness,
        requires_correct_tool,
    }
}

/// Raw-id compatibility boundary for callers that have not validated a
/// block-state id. New code should construct [`StateId`] and call [`hardness`]
/// so this complete table is accessed with a total lookup.
#[must_use]
pub fn hardness_raw(id: u32) -> Option<Hardness> {
    StateId::new(id).map(hardness)
}
