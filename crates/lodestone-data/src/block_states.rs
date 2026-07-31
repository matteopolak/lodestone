//! Block-state id resolution for protocol 776 (Minecraft 26.2).
//!
//! A chunk section palette yields numeric *block state ids* straight off the
//! wire; rendering needs to turn each into a block name plus its property values
//! (`facing=north`, `snowy=false`, …), because the blockstate/model JSON is keyed
//! by exactly those. That id → (block, properties) mapping is version-specific
//! generated data, so it lives here rather than in a version-free crate.
//!
//! # Memory design
//!
//! There are 32,366 states across 1,196 blocks in 26.2. The naive shape — an
//! owned `String` name and a `Vec<(String, String)>` per state — would be
//! megabytes of heap and pointer-chasing for data that is 100% static. Instead
//! the generated table (in [`crate::generated_block_states`]) is **pure rodata,
//! zero heap**:
//!
//! * block names are 1,196 interned `&'static str`;
//! * property sets are de-duplicated to the 6,454 that are actually distinct,
//!   each a `&'static [(&'static str, &'static str)]`;
//! * each state is a `(u16, u16)` pair — an index into the block-name table and
//!   an index into the property-set table.
//!
//! Lookup is O(1) indexing (ids are contiguous `0..STATE_COUNT`), not searching.
//! The zero-heap path is [`block_name`] and [`properties`], which hand back the
//! static slices directly.
//!
//! # The `BlockStateRegistry` trait, and why it costs heap
//!
//! [`lodestone_model::BlockStateRegistry`] — the version-free seam the asset
//! baker consumes — returns [`ResolvedBlockState`], which borrows an owned
//! [`Identifier`] and an owned `BTreeMap<String, String>`. Those owned types
//! cannot be produced from `&'static` data without materialising them, so
//! [`BlockStateTable`] builds a de-duplicated owned layer (1,196 identifiers +
//! 6,454 maps, **not** 32,366) on construction. It is a transient cost: build
//! one to bake, drop it to reclaim the heap, while the zero-heap static table
//! stays resident for the mesher. The `&BTreeMap` shape of `ResolvedBlockState`
//! is the reason this materialisation is unavoidable; see the crate report for a
//! proposed trait change that would let the static table satisfy the seam
//! directly.

use std::collections::BTreeMap;

use lodestone_model::{BlockStateRegistry, Identifier, ResolvedBlockState};

use crate::generated_block_states as table;

pub use crate::generated_block_registry::BLOCK_COUNT;
pub use table::STATE_COUNT;

/// The interned block identifier for `id` (for example `minecraft:oak_stairs`),
/// or `None` if `id` is not in `0..`[`STATE_COUNT`].
///
/// Zero-heap: returns a `&'static str` straight from rodata. O(1).
#[must_use]
pub fn block_name(id: u32) -> Option<&'static str> {
    let &(block, _) = table::STATES.get(id as usize)?;
    Some(table::BLOCK_NAMES[block as usize])
}

/// The interned identifier for the `minecraft:block` registry entry `id` (for
/// example `minecraft:note_block`), or `None` if `id` is out of range.
///
/// This is the *block-type* registry (one id per block, 1,196 entries in
/// 26.2), distinct from the block-*state* ids [`block_name`] indexes: packets
/// such as `block_event` carry a `Holder<Block>` (one id per block type) rather
/// than a palette state id, and so does a `minecraft:tool` rule's explicit block
/// set.
///
/// # The two id spaces are not the same order
///
/// This used to index [`BLOCK_NAMES`](crate::generated_block_states) directly,
/// on the assumption that a registry id and a block-name index are
/// interchangeable. They are not. `BLOCK_NAMES` comes from `blocks.json`, a
/// name-keyed JSON object, so it is **alphabetical**; the registry is in
/// **registration** order. `minecraft:air` is registry id 0 but alphabetical
/// index 19, and `minecraft:stone` is registry id 1 but alphabetical index 975 —
/// so every id resolved to an unrelated block, quietly. The reconciliation now
/// goes through the generated registry-order table.
///
/// [`block_name`] was never affected: its state→block index and `BLOCK_NAMES`
/// are built from the same alphabetical ordering by the same generator, so that
/// path is self-consistent.
///
/// Zero-heap: returns a `&'static str` straight from rodata. O(1).
#[must_use]
pub fn block_type_name(id: u32) -> Option<&'static str> {
    crate::generated_block_registry::BLOCK_REGISTRY_NAMES
        .get(id as usize)
        .copied()
}

/// The property values for `id` as a sorted slice of `(name, value)` pairs, or
/// `None` if `id` is not in `0..`[`STATE_COUNT`]. An empty slice means the block
/// has no properties.
///
/// Zero-heap: returns a `&'static [(&'static str, &'static str)]` straight from
/// rodata. O(1).
#[must_use]
pub fn properties(id: u32) -> Option<&'static [(&'static str, &'static str)]> {
    let &(_, set) = table::STATES.get(id as usize)?;
    Some(table::PROPERTY_SETS[set as usize])
}

/// A [`BlockStateRegistry`] implementation for protocol 776.
///
/// Holds the owned [`Identifier`]/`BTreeMap` layer that the trait's borrowing
/// shape requires (see the module docs). Construct it only when the trait is
/// needed — e.g. to drive the asset baker — and drop it afterwards; the
/// version-free [`block_name`]/[`properties`] accessors need no instance and
/// allocate nothing.
#[derive(Debug, Clone)]
pub struct BlockStateTable {
    /// One identifier per block, indexed as [`table::BLOCK_NAMES`].
    identifiers: Vec<Identifier>,
    /// One map per distinct property set, indexed as [`table::PROPERTY_SETS`].
    property_maps: Vec<BTreeMap<String, String>>,
}

impl BlockStateTable {
    /// Materialises the owned identifier and property-map layer from the static
    /// table.
    ///
    /// # Panics
    ///
    /// Panics only if the generated table contains a block name that is not a
    /// valid [`Identifier`], which is a generation-time invariant — real data
    /// never triggers it.
    #[must_use]
    pub fn new() -> Self {
        let identifiers = table::BLOCK_NAMES
            .iter()
            .map(|name| {
                name.parse::<Identifier>()
                    .expect("generated block name is a valid identifier")
            })
            .collect();
        let property_maps = table::PROPERTY_SETS
            .iter()
            .map(|set| {
                set.iter()
                    .map(|&(key, value)| (key.to_owned(), value.to_owned()))
                    .collect()
            })
            .collect();
        Self {
            identifiers,
            property_maps,
        }
    }

    /// Approximate heap bytes owned by the materialised layer, for measurement.
    ///
    /// Counts the two backing `Vec`s plus every owned string in the identifiers
    /// and property maps. Ignores `BTreeMap` node overhead, so it is a lower
    /// bound on true resident heap.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        let idents: usize = self
            .identifiers
            .iter()
            .map(|id| id.namespace().len() + id.path().len())
            .sum();
        let maps: usize = self
            .property_maps
            .iter()
            .flat_map(|map| map.iter())
            .map(|(key, value)| key.len() + value.len())
            .sum();
        self.identifiers.capacity() * size_of::<Identifier>()
            + self.property_maps.capacity() * size_of::<BTreeMap<String, String>>()
            + idents
            + maps
    }
}

impl Default for BlockStateTable {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockStateRegistry for BlockStateTable {
    fn resolve(&self, id: u32) -> Option<ResolvedBlockState<'_>> {
        let &(block, set) = table::STATES.get(id as usize)?;
        Some(ResolvedBlockState {
            block: &self.identifiers[block as usize],
            properties: &self.property_maps[set as usize],
        })
    }

    fn state_count(&self) -> u32 {
        table::STATE_COUNT
    }
}
