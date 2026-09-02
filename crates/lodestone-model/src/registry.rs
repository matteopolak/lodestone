//! The block-state registry seam ([`BlockStateRegistry`]).
//!
//! Rendering a chunk starts from a numeric *block state id* — a `u32` taken
//! straight from the chunk section palette in the wire protocol. Turning that
//! number into geometry requires knowing which block it is and what property
//! values it carries (`facing=north`, `half=top`, …), because the blockstate
//! JSON is keyed by exactly those properties.
//!
//! That id → (block, properties) mapping is *version-specific data*: Mojang's
//! data generator emits it per game version, and the numbers are reshuffled
//! whenever blocks are added. Per this crate's central rule, such data lives in
//! a version crate, never in the version-free layers. So this module defines
//! only the **trait** that every version crate must satisfy; the table that
//! implements it is generated elsewhere (e.g. a `v26-2` crate from
//! `reports/blocks.json`). Version-free consumers — the asset baker in
//! particular — depend on this trait and stay ignorant of any one version's
//! numbering.

use std::collections::BTreeMap;

use crate::ids::Identifier;

/// A block and its property values, resolved from a numeric block state id.
///
/// Borrows from the backing [`BlockStateRegistry`] so a hot meshing loop can
/// resolve millions of states without allocating. The property map is ordered
/// (a [`BTreeMap`]) so that a variant key like `facing=north,half=top` can be
/// matched deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedBlockState<'a> {
    /// The block identifier, for example `minecraft:oak_stairs`.
    pub block: &'a Identifier,
    /// The block's property values (name → value), in sorted order.
    pub properties: &'a BTreeMap<String, String>,
}

/// Maps numeric block state ids to their block and properties.
///
/// This is the seam between a version crate (which owns the generated id table)
/// and the version-free asset layer (which bakes geometry). Implementations are
/// expected to be cheap, read-only lookups over a table built once at load.
///
/// # Examples
///
/// A minimal in-memory implementation backed by a `Vec`:
///
/// ```
/// use std::collections::BTreeMap;
/// use lodestone_model::{BlockStateRegistry, ResolvedBlockState, Identifier};
///
/// #[derive(Debug)]
/// struct Table {
///     block: Identifier,
///     states: Vec<BTreeMap<String, String>>,
/// }
///
/// impl BlockStateRegistry for Table {
///     fn resolve(&self, id: u32) -> Option<ResolvedBlockState<'_>> {
///         let properties = self.states.get(id as usize)?;
///         Some(ResolvedBlockState { block: &self.block, properties })
///     }
///
///     fn state_count(&self) -> u32 {
///         self.states.len() as u32
///     }
/// }
///
/// let table = Table {
///     block: "minecraft:stone".parse().unwrap(),
///     states: vec![BTreeMap::new()],
/// };
/// let resolved = table.resolve(0).unwrap();
/// assert_eq!(resolved.block.path(), "stone");
/// assert!(resolved.properties.is_empty());
/// assert!(table.resolve(1).is_none());
/// ```
pub trait BlockStateRegistry {
    /// Resolves a block state id to its block and property values.
    ///
    /// Returns `None` if the id is not part of this registry.
    fn resolve(&self, id: u32) -> Option<ResolvedBlockState<'_>>;

    /// Returns the number of registered block states.
    ///
    /// Block state ids form a contiguous range `0..state_count()` in the
    /// vanilla global palette, so this doubles as an iteration bound. A
    /// registry with sparse ids may return an upper bound and rely on
    /// [`resolve`](BlockStateRegistry::resolve) returning `None` for gaps.
    fn state_count(&self) -> u32;
}
