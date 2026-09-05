//! Per-block-state block-entity type for protocol 776 (Minecraft 26.2): which
//! `BLOCK_ENTITY_TYPE` registry entry, if any, a block state owns.
//!
//! # Why this table exists at all
//!
//! In vanilla a block entity is **not** created by a packet. Vanilla's own
//! chunk class's own "set block state" step creates one from the state alone:
//! when the new state declares a block entity, it looks up any existing
//! block entity at that position in "check" mode; if one exists but no
//! longer matches the new state, it is removed; and if none exists (or was
//! just removed), the new block's own block-entity factory constructs a
//! fresh one for that position and state.
//!
//! So *setting a chest block state is what creates the chest block entity*, and
//! `block_entity_data` is only ever **data for an entity that already exists**.
//! Lodestone had no equivalent: a `block_update` wrote the state and nothing
//! else, so a freshly placed chest had a state, no block-entity record, and drew
//! zero pixels while still opening (interaction resolves from the state).
//! This census is the missing input: it is what lets a block-state
//! write decide whether to create, keep, replace or remove the record, which is
//! [`lodestone_world::World::sync_block_entity`]'s job.
//!
//! # Data source: the jar, because neither report carries the pairing
//!
//! `blocks.json` is block *properties* only — it has no has-block-entity flag
//! and no block-entity type. `registries.json` **does** carry the
//! `minecraft:block_entity_type` registry (all 49 entries with their protocol
//! ids), but says nothing about which blocks each type covers, and it does not
//! carry data-pack registries at all. The pairing exists only inside the jar, so
//! the table is generated from a headless 26.2 server dump — see
//! `tests/block_entity_types.rs` for the generator and drift guard, and
//! `oracle-java/BlockEntityTypeOracle.java` for why
//! vanilla's own block-entity-type "is valid" check is the faithful way to
//! recover it (it *is*
//! `validBlocks.contains(state.getBlock())`, the very set the block's
//! own "new block entity" owner was registered with) rather than constructing
//! 32,366 live block-entity objects.
//!
//! # Memory design
//!
//! Pure rodata, zero heap, O(1) by id: one `u16` per state with
//! [`NO_BLOCK_ENTITY`] as the "none" sentinel, plus the 49 type names by id.
//! 4,567 of the 32,366 states own a block entity, and there are only 49 distinct
//! types, so a `u16` per state is both dense and future-proof (a `u8` would
//! break silently at 254 types).
//!
//! # Gotcha: the type does not identify the block
//!
//! `minecraft:copper_chest` and its three weathered variants all map to
//! `minecraft:chest` (type id 1) — verified in the dump. `minecraft:chest` and
//! `minecraft:trapped_chest` are distinct types (1 and 2). So a renderer must
//! still read the **block state** to decide what to draw; the type only answers
//! "does a record belong here, and is the existing one still the right kind".
//! That is exactly how `lodestone-shell`'s `block_entities` module uses it.

use crate::{block_states::StateId, generated_block_entity_types as table};

pub use table::{STATE_COUNT, TYPE_COUNT};

/// The `STATE_TYPE` sentinel for a state that owns no block entity.
///
/// `u16::MAX`, not `0` — type id `0` is `minecraft:furnace`, a perfectly real
/// entry, so a zero sentinel would give every non-block-entity block a furnace.
pub const NO_BLOCK_ENTITY: u16 = u16::MAX;

/// A validated entry in the block-entity-type registry, distinct from a block state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockEntityType(u32);

impl BlockEntityType {
    /// Validates a raw registry id at a wire or import boundary.
    #[must_use]
    pub const fn new(raw: u32) -> Option<Self> {
        if raw < TYPE_COUNT {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// The registry id for version-free world records and wire encoding.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// The `minecraft:block_entity_type` registry id owned by block-state `id`, or
/// `None` when the state owns no block entity — vanilla's own
/// "has block entity" check plus *which* type.
///
/// The input is validated at the caller's state boundary. `None` means only
/// that a valid state owns no block entity.
///
/// Zero-heap: reads straight from rodata. O(1) indexing, no search.
#[must_use]
pub fn block_entity_type(id: StateId) -> Option<BlockEntityType> {
    let entry = table::STATE_TYPE[id.raw() as usize];
    if entry == NO_BLOCK_ENTITY {
        return None;
    }
    Some(BlockEntityType(u32::from(entry)))
}

/// The registry key of a validated block-entity type (e.g. `"minecraft:chest"`).
///
/// Server record construction and diagnostics use names; world records and
/// rendering retain numeric registry ids.
#[must_use]
pub fn block_entity_type_name(id: BlockEntityType) -> &'static str {
    table::TYPE_NAMES[id.raw() as usize]
}

/// Resolves a canonical `minecraft:block_entity_type` registry key to its
/// validated protocol-776 id.
///
/// This is the only reverse boundary for built-in block-entity records. A
/// custom key remains `None`: callers must keep such a key in the dynamic
/// registry that supplied it instead of borrowing an unrelated built-in id.
/// The fixed table has only 49 entries and block-entity records are sparse, so
/// a linear scan avoids allocating a second copy of the names or a hash map.
#[must_use]
pub fn block_entity_type_id(name: &str) -> Option<BlockEntityType> {
    table::TYPE_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .and_then(|index| u32::try_from(index).ok())
        .and_then(BlockEntityType::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_lookup_round_trips_each_validated_type() {
        for raw in 0..TYPE_COUNT {
            let id = BlockEntityType::new(raw).expect("table id validates");
            assert_eq!(block_entity_type_id(block_entity_type_name(id)), Some(id));
        }
    }

    /// Literal controls from the pinned 26.2 registry order. These do not
    /// derive their expected names from `TYPE_NAMES`, so an accidental
    /// permutation cannot pass merely by making both lookup directions agree.
    #[test]
    fn reverse_lookup_keeps_known_registry_slots() {
        assert_eq!(
            block_entity_type_id("minecraft:furnace").map(BlockEntityType::raw),
            Some(0)
        );
        assert_eq!(
            block_entity_type_id("minecraft:chest").map(BlockEntityType::raw),
            Some(1)
        );
        assert_eq!(
            block_entity_type_id("minecraft:piston").map(BlockEntityType::raw),
            Some(11)
        );
        assert_eq!(
            block_entity_type_id("minecraft:not_a_real_block_entity"),
            None
        );
    }
}
