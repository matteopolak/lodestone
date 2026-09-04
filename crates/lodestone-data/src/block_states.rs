//! Block-state id resolution for protocol 776 (Minecraft 26.2).
//!
//! A chunk section palette yields numeric *block state ids* straight off the
//! wire; rendering needs to turn each into a block name plus its property values
//! (`facing=north`, `snowy=false`, …), because the blockstate/model JSON is keyed
//! by exactly those. That id → (block, properties) mapping is generated data
//! specific to 26.2, the one canonical internal version. It is a
//! game-data census, not wire-format code, so it lives here in this data
//! crate rather than in `lodestone-v26-2` or in the fully
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
//! * block names stay in the one registry-order canonical column, reached from
//!   the state table's name-sorted block index through a generated permutation;
//! * property sets are de-duplicated to the 6,454 that are actually distinct,
//!   each a `&'static [(&'static str, &'static str)]`;
//! * each state is a `(u16, u16)` pair — an alphabetical block-name index and
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

use crate::block::Block;

/// Resolves a block-state table's **alphabetical** block index through the
/// generated name-order permutation into the canonical registry-order names.
///
/// `STATES` deliberately keeps this index: it is the order of the name-keyed
/// report that supplies the state rows. The canonical name column deliberately
/// keeps registration order: it is the wire's block-type registry-id order. The
/// permutation is the only bridge; treating either index as the other silently
/// changes the block a state names.
fn block_name_at_alphabetical_index(index: u16) -> &'static str {
    let registry_id = crate::generated_block_enum::REGISTRY_IDS_BY_NAME[index as usize];
    crate::generated_block_registry::BLOCK_REGISTRY_NAMES[registry_id as usize]
}

/// A validated global block-state id — one of the 32,366 states of 26.2.
///
/// # Why a newtype and not an enum
///
/// [`Block`] is an enum because 1,196 hand-named block types is a set the
/// compiler can usefully check exhaustively. A block *state* is not that set: it
/// is the cross product of each block's property domains, 32,366 entries with no
/// individual names, and nothing ever wants to `match` on one. So the type's job
/// here is different — it is to make the *range* invariant true by construction
/// so that every downstream lookup can be total.
///
/// That is the payoff. `StateId::new` is the single fallible step; after it,
/// [`block`](Self::block), [`properties`](Self::properties) and
/// [`is_default`](Self::is_default) return values rather than `Option`s. The
/// free-function forms below ([`block_name`], [`properties`]) keep taking a raw
/// `u32` and keep returning `Option`, because they are the un-migrated wire-side
/// entry points; prefer the methods in new code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StateId(u32);

impl StateId {
    /// Validates a raw global block-state id, or `None` if it is not in
    /// `0..`[`STATE_COUNT`].
    #[must_use]
    pub fn new(raw: u32) -> Option<Self> {
        (raw < STATE_COUNT).then_some(Self(raw))
    }

    /// The raw global id, for the wire.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// The block this state belongs to. Total, O(1), two array indexes.
    ///
    /// Goes through the generated registry-order join rather than treating a
    /// state's block index as a registry id: the state table is name-sorted and
    /// the registry is registration-ordered, so the two are unrelated
    /// permutations.
    #[must_use]
    pub fn block(self) -> Block {
        let registry_id = crate::generated_block_registry::STATE_BLOCK[self.0 as usize];
        Block::from_registry_id(registry_id)
            .expect("generated STATE_BLOCK column holds a valid registry id")
    }

    /// This state's property values as a sorted `(name, value)` slice; empty for
    /// a block with no properties. Total, O(1), zero-heap.
    #[must_use]
    pub fn properties(self) -> &'static [(&'static str, &'static str)] {
        let (_, set) = table::STATES[self.0 as usize];
        table::PROPERTY_SETS[set as usize]
    }

    /// Whether this is its block's own default-block-state. Total, O(1).
    #[must_use]
    pub fn is_default(self) -> bool {
        crate::snow_support::is_default_state(self)
    }
}

/// The interned block identifier for `id` (for example `minecraft:oak_stairs`),
/// or `None` if `id` is not in `0..`[`STATE_COUNT`].
///
/// Zero-heap: returns a `&'static str` straight from rodata. O(1).
#[must_use]
pub fn block_name(id: u32) -> Option<&'static str> {
    let &(block, _) = table::STATES.get(id as usize)?;
    Some(block_name_at_alphabetical_index(block))
}

/// The interned identifier for the `minecraft:block` registry entry `id` (for
/// example `minecraft:note_block`), or `None` if `id` is out of range.
///
/// This is the *block-type* registry (one id per block, 1,196 entries in
/// 26.2), distinct from the block-*state* ids [`block_name`] indexes: packets
/// such as `block_event` carry one registry id per block type rather than a
/// palette state id, and so does a `minecraft:tool` rule's explicit block set.
///
/// # The two id spaces are not the same order
///
/// The block-state table does not carry a second copy of these names. Its first
/// column remains an **alphabetical** index from the name-keyed blocks report;
/// the lookup resolves it through
/// [`crate::generated_block_enum::REGISTRY_IDS_BY_NAME`] into this
/// registration-order canonical column. `minecraft:air` is registry id 0 but
/// alphabetical index 19, and `minecraft:stone` is registry id 1 but
/// alphabetical index 975, so the two orders cannot be used interchangeably.
///
/// [`block_name`] follows that same alphabetical index through the permutation,
/// so it retains its public behavior without retaining duplicate names.
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
/// its own block-state registry block by block; `block_state_index` asserts that
/// when it builds this, so a table that ever stopped being block-major fails
/// loudly at first use rather than silently resolving into a neighbouring
/// block's states.
#[derive(Debug, Clone, Copy)]
struct BlockSpan {
    first: u32,
    last: u32,
    /// The id `is_default_state` marks, i.e. vanilla's
    /// own default-block-state. `first` only when the default column has somehow
    /// lost this block — see [`state_id`]'s tier 3.
    default: u32,
}

/// The reverse map's index: one [`BlockSpan`] per state-table alphabetical
/// block index, plus those names sorted for binary search.
struct BlockStateIndex {
    spans: Box<[BlockSpan]>,
    /// State-table block indices sorted by canonical name. Built rather than
    /// assumed: the raw `STATES` column carries no names, and a silently
    /// unsorted permutation would make `binary_search` return wrong answers
    /// rather than fail.
    by_name: Box<[u16]>,
}

/// Builds [`BlockStateIndex`] once per process by walking the 32,366-row static
/// table. ~32k iterations and two small allocations (14 KB + 2.4 KB), amortised
/// over the whole process.
fn block_state_index() -> &'static BlockStateIndex {
    static INDEX: std::sync::OnceLock<BlockStateIndex> = std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let block_count = BLOCK_COUNT as usize;
        let mut spans: Vec<Option<BlockSpan>> = vec![None; block_count];
        let mut counts: Vec<u32> = vec![0; block_count];
        for id in 0..table::STATE_COUNT {
            let (block, _) = table::STATES[id as usize];
            let block = block as usize;
            counts[block] += 1;
            let state = StateId::new(id).expect("generated state-table index is valid");
            let is_default = crate::snow_support::is_default_state(state);
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
                        block_name_at_alphabetical_index(block as u16)
                    )
                });
                assert_eq!(
                    span.last - span.first + 1,
                    counts[block],
                    "block `{}`'s states are not contiguous in the generated table \
                     ({}..={} spans {} ids but the block owns {}); `state_id` scans the span, so \
                     a non-block-major table would resolve into a neighbour's states",
                    block_name_at_alphabetical_index(block as u16),
                    span.first,
                    span.last,
                    span.last - span.first + 1,
                    counts[block]
                );
                span
            })
            .collect();
        // Licenses `state_id`'s allocation-free property comparison: every
        // generated set is already sorted by key, so a candidate's static slice
        // can be compared directly against the caller's sorted `wanted` instead
        // of being copied into a `Vec` and sorted per candidate row. Keys are
        // unique within a set, so key order and `(key, value)` tuple order are
        // the same order. 6,454 sets checked once per process; without this the
        // comparison would silently compare unequal orderings the day the
        // generator's output order changed.
        for (set_index, set) in table::PROPERTY_SETS.iter().enumerate() {
            assert!(
                set.windows(2).all(|w| w[0].0 < w[1].0),
                "generated PROPERTY_SETS[{set_index}] is not strictly sorted by property name \
                 ({set:?}); `state_id` compares these slices directly and would stop matching"
            );
        }
        let mut by_name: Vec<u16> = (0..block_count as u16).collect();
        by_name.sort_unstable_by_key(|&b| block_name_at_alphabetical_index(b));
        BlockStateIndex {
            spans,
            by_name: by_name.into_boxed_slice(),
        }
    })
}

/// The state table's alphabetical block index whose identifier is `name`, or
/// `None`. `O(log 1196)`.
fn block_index(name: &str) -> Option<u16> {
    let index = block_state_index();
    index
        .by_name
        .binary_search_by_key(&name, |&b| block_name_at_alphabetical_index(b))
        .ok()
        .map(|slot| index.by_name[slot])
}

/// The block-state id for `minecraft:air`, resolved by name rather than
/// hardcoded as registry id `0`, and cached.
///
/// Every caller that needs a "nothing here" / unresolvable-state fallback wants
/// this. It used to be a 32,366-row scan per call in
/// `lodestone-v26-2`'s `server_protocol.rs`.
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
/// This logic used to live in `lodestone-v26-2`'s `server_protocol.rs` and, before
/// `43a6e030`, fell back to the **lowest** id sharing the name — right for water
/// (`86`, `level=0`) and lava (`102`), wrong for 661 of the 797 multi-state
/// blocks. Three shipped consequences: bare `minecraft:grass_block` resolved to
/// id `8`, `snowy=true`, so every blade of spread grass rendered snowy;
/// bare directional blocks came out at whatever the lowest id's `facing` happened
/// to be; and redstone dust's four connection properties came out `up`
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

    let index = block_state_index();
    let span = index.spans[block_index(name)? as usize];

    // Tier 1. Compares the candidate's *static* slice against `wanted` with no
    // copy and no per-row sort — licensed by the sortedness assertion in
    // `block_state_index`, which is why that assertion is not decoration. This
    // used to `to_vec()` and sort per candidate row: one small allocation per row
    // scanned, so ~750 per column at `ChunkColumn::from_generated` time.
    for id in span.first..=span.last {
        if properties(id).unwrap_or(&[]) == wanted.as_slice() {
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
    // `merged` started as a (sorted) static set and only had *values* written
    // over it, so it is still sorted by key; the sort is kept because it is
    // free at this point (one call per unresolved state, not per row) and
    // because it makes the direct slice comparison below true by construction
    // rather than by an argument about `overridden`.
    merged.sort_unstable();
    for id in span.first..=span.last {
        if properties(id).unwrap_or(&[]) == merged.as_slice() {
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
    /// One identifier per state-table alphabetical block index.
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
        let identifiers = (0..BLOCK_COUNT as u16)
            .map(|index| {
                let name = block_name_at_alphabetical_index(index);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The discriminating case for a partial-property lookup: on a
    /// block with many states it must land on the *named* state, not on the
    /// lowest id sharing the block's name.
    ///
    /// Redstone dust is the case that shipped wrong. Before `state_id`
    /// existed, `lodestone-v26-2`'s hand-rolled scan fell back to the lowest
    /// id sharing a block name whenever the caller's property set did not
    /// exactly match a real state — which, for a block this server only ever
    /// partially describes (`minecraft:redstone_wire[power=N]`, never the
    /// other four connection properties), was *every* update. That lowest id
    /// is state **4011**, confirmed as this block's first (lowest) id by the
    /// committed JVM dump `tests/support/snow_support_jvm.txt` ("`B 4011
    /// minecraft:redstone_wire`" — the dump's own header defines `B` as "the
    /// first state id of a block"). Its `power` is `0`. So every dust update
    /// used to resolve to the same id regardless of the power the server
    /// actually wanted to send, and rendered as unpowered wire.
    ///
    /// `power=7` is chosen because it discriminates the two hypotheses: the
    /// old code returns id 4011 (`power=0`) for *any* power value, so an
    /// input that also produced 4011 would not tell the implementations
    /// apart.
    #[test]
    fn state_id_resolves_redstone_dust_by_power_not_to_the_lowest_id() {
        const WRONG_OLD_ANSWER: u32 = 4011;

        assert_eq!(
            properties(WRONG_OLD_ANSWER)
                .and_then(|props| props.iter().find(|(k, _)| *k == "power"))
                .map(|(_, v)| *v),
            Some("0"),
            "fixture sanity: the old broken fallback's wrong answer must actually carry power=0"
        );

        let power_seven = state_id("minecraft:redstone_wire[power=7]")
            .expect("minecraft:redstone_wire[power=7] must resolve");

        // The assertion that matters: not the old wrong answer.
        assert_ne!(
            power_seven, WRONG_OLD_ANSWER,
            "state_id(\"minecraft:redstone_wire[power=7]\") returned the old lowest-id fallback \
             ({WRONG_OLD_ANSWER}, power=0) instead of resolving power=7 to its own state — this \
             is the exact defect issues #465/#511 describe"
        );
        assert_eq!(
            properties(power_seven)
                .and_then(|props| props.iter().find(|(k, _)| *k == "power"))
                .map(|(_, v)| *v),
            Some("7"),
            "state_id must resolve the requested power value exactly, not merely to a different id"
        );

        // Tier 2's contract: every property the caller did *not* name keeps
        // the block's jar-marked default value.
        let default_id = state_id("minecraft:redstone_wire")
            .expect("minecraft:redstone_wire must resolve to its jar-marked default");
        let default_props = properties(default_id).expect("default state has properties");
        let resolved_props = properties(power_seven).expect("resolved state has properties");
        for &(key, default_value) in default_props {
            if key == "power" {
                continue;
            }
            let resolved_value = resolved_props
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| *v);
            assert_eq!(
                resolved_value,
                Some(default_value),
                "property `{key}` should keep its default value when the caller only named `power`"
            );
        }
    }

    /// A bare name with no properties at all resolves to the block's
    /// jar-marked default (tier 3) — **not** the lowest id (4011). Cross-
    /// checked against the committed JVM dump's own `D` (`state ==
    /// defaultBlockState()`) bitstring for this block's id range in
    /// `tests/support/snow_support_jvm.txt`, which marks id 5171 as the one
    /// true bit — an id this test derives independently of `state_id` itself
    /// by walking the dump's `P D` line, not by trusting the function under
    /// test.
    #[test]
    fn state_id_resolves_a_bare_name_to_the_jar_marked_default() {
        assert_eq!(state_id("minecraft:redstone_wire"), Some(5171));
    }

    /// An unknown block name resolves to nothing — `state_id` does not paper
    /// over a name this version's census does not carry.
    #[test]
    fn state_id_returns_none_for_an_unknown_block() {
        assert_eq!(state_id("minecraft:not_a_real_block"), None);
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
