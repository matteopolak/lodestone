//! Block-state id resolution for protocol 776 (Minecraft 26.2).
//!
//! A chunk section palette yields numeric *block state ids* straight off the
//! wire; rendering needs to turn each into a block name plus its property values
//! (`facing=north`, `snowy=false`, …), because the blockstate/model JSON is keyed
//! by exactly those. That id → (block, properties) mapping is generated data
//! specific to 26.2, the one canonical internal version (#343). It is a
//! game-data census, not wire-format code, so it lives here in this data
//! crate (issue #361) rather than in `lodestone-v770` or in the fully
//! version-free `lodestone-model`.
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

/// One block's contiguous span of state ids plus its jar-marked default state —
/// the whole of the reverse map's index, 1,196 entries of 12 bytes.
///
/// `first..=last` is a *contiguous* range because vanilla builds
/// `Block.BLOCK_STATE_REGISTRY` block by block; `block_state_index` asserts that
/// when it builds this, so a table that ever stopped being block-major fails
/// loudly at first use rather than silently resolving into a neighbouring
/// block's states.
#[derive(Debug, Clone, Copy)]
struct BlockSpan {
    first: u32,
    last: u32,
    /// The id `is_default_state` marks, i.e. vanilla's
    /// `defaultBlockState()`. `first` only when the default column has somehow
    /// lost this block — see [`state_id`]'s tier 3.
    default: u32,
}

/// The reverse map's index: one [`BlockSpan`] per block, in
/// [`table::BLOCK_NAMES`] order, plus that order's names sorted for binary
/// search.
struct BlockStateIndex {
    spans: Box<[BlockSpan]>,
    /// Block indices sorted by name. Built rather than assumed: `BLOCK_NAMES`
    /// happens to be alphabetical today (it comes from a JSON object's keys),
    /// but nothing in the generator promises it stays that way, and a silently
    /// unsorted array would make `binary_search` return wrong answers rather
    /// than fail.
    by_name: Box<[u16]>,
}

/// Builds [`BlockStateIndex`] once per process by walking the 32,366-row static
/// table. ~32k iterations and two small allocations (14 KB + 2.4 KB), amortised
/// over the whole process.
fn block_state_index() -> &'static BlockStateIndex {
    static INDEX: std::sync::OnceLock<BlockStateIndex> = std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let block_count = table::BLOCK_NAMES.len();
        let mut spans: Vec<Option<BlockSpan>> = vec![None; block_count];
        let mut counts: Vec<u32> = vec![0; block_count];
        for id in 0..table::STATE_COUNT {
            let (block, _) = table::STATES[id as usize];
            let block = block as usize;
            counts[block] += 1;
            let is_default = crate::snow_support::is_default_state(id) == Some(true);
            match &mut spans[block] {
                Some(span) => {
                    span.last = id;
                    if is_default {
                        span.default = id;
                    }
                }
                slot @ None => {
                    *slot = Some(BlockSpan {
                        first: id,
                        last: id,
                        default: id,
                    });
                }
            }
        }
        let spans: Box<[BlockSpan]> = spans
            .into_iter()
            .enumerate()
            .map(|(block, span)| {
                let span = span.unwrap_or_else(|| {
                    panic!(
                        "generated block-state table has no state for block `{}` — regenerate or \
                         fix the table",
                        table::BLOCK_NAMES[block]
                    )
                });
                assert_eq!(
                    span.last - span.first + 1,
                    counts[block],
                    "block `{}`'s states are not contiguous in the generated table \
                     ({}..={} spans {} ids but the block owns {}); `state_id` scans the span, so \
                     a non-block-major table would resolve into a neighbour's states",
                    table::BLOCK_NAMES[block],
                    span.first,
                    span.last,
                    span.last - span.first + 1,
                    counts[block]
                );
                span
            })
            .collect();
        let mut by_name: Vec<u16> = (0..block_count as u16).collect();
        by_name.sort_unstable_by_key(|&b| table::BLOCK_NAMES[b as usize]);
        BlockStateIndex {
            spans,
            by_name: by_name.into_boxed_slice(),
        }
    })
}

/// The block index in [`table::BLOCK_NAMES`] whose identifier is `name`, or
/// `None`. `O(log 1196)`.
fn block_index(name: &str) -> Option<u16> {
    let index = block_state_index();
    index
        .by_name
        .binary_search_by_key(&name, |&b| table::BLOCK_NAMES[b as usize])
        .ok()
        .map(|slot| index.by_name[slot])
}

/// The block-state id for `minecraft:air`, resolved by name rather than
/// hardcoded as registry id `0`, and cached.
///
/// Every caller that needs a "nothing here" / unresolvable-state fallback wants
/// this. It used to be a 32,366-row scan per call in
/// `lodestone-v770`'s `server_protocol.rs`.
///
/// # Panics
/// Panics if the generated table has no `minecraft:air` state (a corrupt table,
/// not a runtime condition).
#[must_use]
pub fn air_state_id() -> u32 {
    static AIR: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *AIR.get_or_init(|| {
        state_id("minecraft:air").expect(
            "generated block-state table has no `minecraft:air` entry — regenerate or fix the table",
        )
    })
}

/// The **reverse** of [`block_name`]/[`properties`]: resolves a canonical
/// block-state string — `"minecraft:stone"`, `"minecraft:water[level=0]"`,
/// `"minecraft:oak_stairs[facing=east,half=bottom,shape=straight,waterlogged=false]"`
/// — to its protocol-776 global state id, or `None` if the block name is not in
/// the table at all.
///
/// `O(log 1196)` for the name plus at most one scan of *that block's* states
/// (27 on average, 1,296 at the worst) — never the 32,366-row scan with a string
/// compare per row this replaced. See `docs/lodestone-data-crate.md` for the
/// index and `docs/chunk-column-encoding.md` for the measurement that motivated
/// it.
///
/// # Three-tier fallback: exact, default-plus-overrides, then the default state
///
/// 1. **Exact match** — name and every property value agree. The common case for
///    anything decoded off a real edit or a fully-qualified generator state.
/// 2. **The block's default state with the named properties written over it** —
///    vanilla's own `defaultBlockState().setValue(k, v)…`. Every property the
///    caller did *not* name keeps its vanilla default, and any property no real
///    state of this block carries (a *synthetic* one, e.g. this server's
///    `minecraft:comparator[…,output=N]`, which vanilla keeps in a block entity)
///    is dropped rather than sinking the whole lookup. Since a block's state set
///    is the full cross product of its properties' domains, the merged set always
///    names a real state unless a value is outside its domain.
/// 3. **The default state alone** — a bare name, or a named value outside its
///    property's domain.
///
/// # The default state is *not* the lowest id, and assuming it was caused three bugs
///
/// This logic used to live in `lodestone-v770`'s `server_protocol.rs` and, before
/// `43a6e030`, fell back to the **lowest** id sharing the name — right for water
/// (`86`, `level=0`) and lava (`102`), wrong for 661 of the 797 multi-state
/// blocks. Three shipped consequences: bare `minecraft:grass_block` resolved to
/// id `8`, `snowy=true`, so every blade of spread grass rendered snowy (#546);
/// bare directional blocks came out at whatever the lowest id's `facing` happened
/// to be (#475); and redstone dust's four connection properties came out `up`
/// rather than `none`, so wire rendered climbing rather than flat. The default is
/// read from [`crate::snow_support::is_default_state`] — `state ==
/// state.getBlock().defaultBlockState()` dumped from the real 26.2 server,
/// exactly one id per block.
///
/// # Panics
/// Panics if the generated table is not block-major (see [`BlockSpan`]) or has a
/// block with no states — both generation-time invariants.
#[must_use]
pub fn state_id(state: &str) -> Option<u32> {
    let (name, raw_props) = match state.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
        None => (state, ""),
    };
    let mut wanted: Vec<(&str, &str)> = if raw_props.is_empty() {
        Vec::new()
    } else {
        raw_props
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .collect()
    };
    wanted.sort_unstable();

    let span = block_state_index().spans[block_index(name)? as usize];

    // Tier 1.
    for id in span.first..=span.last {
        let mut have: Vec<(&str, &str)> = properties(id).unwrap_or(&[]).to_vec();
        have.sort_unstable();
        if have == wanted {
            return Some(id);
        }
    }

    // Tier 3's value, and tier 2's base.
    let base = span.default;
    if wanted.is_empty() {
        return Some(base);
    }

    // Tier 2: `defaultBlockState().setValue(k, v)…` for every property the block
    // really has, dropping any synthetic one.
    let mut merged: Vec<(&str, &str)> = properties(base).unwrap_or(&[]).to_vec();
    let mut overridden = false;
    for &(key, value) in &wanted {
        if let Some(slot) = merged.iter_mut().find(|(have_key, _)| *have_key == key) {
            if slot.1 != value {
                slot.1 = value;
                overridden = true;
            }
        }
    }
    if !overridden {
        return Some(base);
    }
    merged.sort_unstable();
    for id in span.first..=span.last {
        let mut have: Vec<(&str, &str)> = properties(id).unwrap_or(&[]).to_vec();
        have.sort_unstable();
        if have == merged {
            return Some(id);
        }
    }
    Some(base)
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
