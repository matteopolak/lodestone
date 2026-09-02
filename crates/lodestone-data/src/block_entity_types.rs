//! Per-block-state block-entity type for protocol 776 (Minecraft 26.2): which
//! `BLOCK_ENTITY_TYPE` registry entry, if any, a block state owns.
//!
//! # Why this table exists at all
//!
//! In vanilla a block entity is **not** created by a packet. Vanilla's own
//! chunk class's own "set block state" step creates one from the state alone:
//!
//! ```text
//! if (state.hasBlockEntity() && …) {
//!     BlockEntity blockEntity = this.getBlockEntity(pos, EntityCreationType.CHECK);
//!     if (blockEntity != null && !blockEntity.isValidBlockState(state)) { removeBlockEntity(pos); blockEntity = null; }
//!     if (blockEntity == null) { blockEntity = ((EntityBlock)newBlock).newBlockEntity(pos, state); … }
//! }
//! ```
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

use crate::generated_block_entity_types as table;

pub use table::{STATE_COUNT, TYPE_COUNT};

/// The `STATE_TYPE` sentinel for a state that owns no block entity.
///
/// `u16::MAX`, not `0` — type id `0` is `minecraft:furnace`, a perfectly real
/// entry, so a zero sentinel would give every non-block-entity block a furnace.
pub const NO_BLOCK_ENTITY: u16 = u16::MAX;

/// The `minecraft:block_entity_type` registry id owned by block-state `id`, or
/// `None` when the state owns no block entity — vanilla's own
/// "has block entity" check plus *which* type.
///
/// Also `None` for an `id` outside `0..`[`STATE_COUNT`], so an unknown state id
/// behaves like plain terrain rather than panicking: a hostile or
/// newer-than-expected state id must not be able to crash the client.
///
/// Zero-heap: reads straight from rodata. O(1) indexing, no search.
#[must_use]
pub fn block_entity_type(id: u32) -> Option<u32> {
    let &entry = table::STATE_TYPE.get(id as usize)?;
    if entry == NO_BLOCK_ENTITY {
        return None;
    }
    Some(u32::from(entry))
}

/// The registry key of block-entity type `id` (e.g. `"minecraft:chest"`), or
/// `None` if `id` is not in `0..`[`TYPE_COUNT`].
///
/// Diagnostics and tests only — nothing in the render or world path needs the
/// name, and matching on it would be a slower spelling of comparing ids.
#[must_use]
pub fn block_entity_type_name(id: u32) -> Option<&'static str> {
    table::TYPE_NAMES.get(id as usize).copied()
}
