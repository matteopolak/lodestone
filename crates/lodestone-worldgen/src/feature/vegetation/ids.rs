//! Tag membership as fixed bitsets indexed by [`StateId`] — vegetal decoration's
//! O(1), lock-free, allocation-free replacement for
//! `tags.some_set.contains(block_at(grid, x, y, z))`.
//!
//! # What it is
//!
//! Unit 8 of [`docs/plans/worldgen-rewrite.md`](../../../../../docs/plans/worldgen-rewrite.md),
//! candidate 2 of its "Vegetation: cost per draw" section. Every ground check,
//! every trunk-position test, every air-or-leaves anchor and every heightmap cell
//! used to cost:
//!
//! 1. Reverse state-text lookup — an `RwLock` **read guard**, on a cache line
//!    shared by every concurrent generator call (the shape `4307b59` was reverted
//!    for, at 289 concurrent columns);
//! 2. `split('[')` to recover the base name;
//! 3. a string-set probe — hashing ~20 bytes of UTF-8.
//!
//! `docs/worldgen-vegetation-census.md` counts **74,745 ground rejections in one
//! 136-chunk sweep**, and that is only the rejections that reach a census bump —
//! the tree footprint scan, the leaf rows and the two heightmap scans do far more.
//! Here the same question is one relaxed atomic load and a bit test.
//!
//! # How it works
//!
//! [`IdTags`] is one bitset per [`Tag`], each covering the **entire** `StateId`
//! space. That is not a generous over-allocation, it is exact: [`StateId`] wraps a
//! `u16`, so 65,536 ids is the whole space and the table can never need to grow.
//! 26 tags × 65,536 bits = 208 KiB per [`super::VegTags`], one `alloc_zeroed` at
//! construction, and a [`super::VegTags`] is per-generator.
//!
//! The canonical state table is fixed at startup, so [`super::VegTags::bind`]
//! fills every mask once and the driver only needs to call it per decoration
//! pass to publish the process-wide table.
//!
//! # How to change it, and the gotchas
//!
//! * **Never mutate a [`super::VegTags`]'s sets after [`super::VegTags::bind`]
//!   has run.** The bitset is a cache of those sets, and nothing re-derives it.
//!   Production builds the sets once in
//!   [`super::build_veg_tags`] and never touches them again; the tests that do
//!   mutate them never bind. If you ever need both, add a `rebind` that clears
//!   the masks and resets the watermark to 0.
//! * **[`Clone`] deliberately returns an *unbound* table.** Cloning the atomics'
//!   values would make publication timing observable. An unbound clone binds
//!   from the canonical table on first use, so the failure mode is a slow pass,
//!   never a wrong block.
//! * **Add a [`Tag`] by adding a variant, a [`Tag::ALL`] entry and a
//!   [`super::VegTags::member`] arm.** `TAG_COUNT` is derived from `Tag::ALL`, and
//!   `tag_count_matches_the_all_table` fails if a variant is added without an
//!   entry — a missing `ALL` entry would leave that tag's bits permanently zero,
//!   which reads as "nothing is in this tag" and is exactly the silent-wrong-value
//!   class.
//! * The synthetic tags ([`Tag::Air`], [`Tag::Fluid`], [`Tag::Water`],
//!   [`Tag::Lava`], [`Tag::Cactus`], [`Tag::SugarCane`]) are not registry tags at
//!   all — they are the base-name equality tests the old code spelled inline
//!   (`is_air`, `is_fluid`, `base == "minecraft:cactus"`). They ride the same
//!   mechanism because they ask the same question of the same subject, and
//!   folding them in is what lets a hot loop test "air?" without touching a
//!   string. **`Fluid` must stay base-aware**: `carver/mod.rs` writes
//!   `minecraft:water[level=0]`, so a fluid is not a fixed handful of ids.

use lodestone_worldgen_core::hash::FastMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU64, Ordering};

use lodestone_data::block_properties::{BuiltinPropertyValue, Properties, PropertyKey};
use lodestone_data::block_states::StateId;
use lodestone_data::block::Block;

use super::VegGrid;
use super::config::VegTags;

/// The membership questions vegetal decoration asks of a block state.
///
/// Registry-backed variants are resolved by [`super::build_veg_tags`]; the
/// built-in state-name questions are kept in the same table so hot callers do
/// not need a second membership path. See the module doc on why they share one
/// mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    CannotReplaceBelowTreeTrunk,
    SupportsVegetation,
    ReplaceableByTrees,
    Logs,
    SupportsCactus,
    SupportsSugarCane,
    Leaves,
    /// `is_air`: `minecraft:{air,cave_air,void_air}`.
    Air,
    /// `is_fluid`: base `minecraft:water` or `minecraft:lava`.
    Fluid,
    /// Base `minecraft:water` — the waterlogged-leaf test and
    /// `MatchingFluid`'s water arm.
    Water,
    /// Base `minecraft:lava` — `MatchingFluid`'s lava arm.
    Lava,
    /// Base `minecraft:cactus` — the cactus survival rule's "below is cactus" arm.
    Cactus,
    /// Base `minecraft:sugar_cane` — the sugar-cane survival rule's own half.
    SugarCane,
    /// `#minecraft:mangrove_logs_can_grow_through` — the mangrove
    /// increment: the upwards-branching trunk placer's extra OR-arm on its
    /// valid-position test
    /// (a mangrove trunk can grow up through e.g. its own leaves/propagules,
    /// same shape as [`Tag::Leaves`]'s air-or-leaves anchor for dark oak).
    MangroveLogsCanGrowThrough,
    /// `#minecraft:mangrove_roots_can_grow_through` — `MangroveRootPlacer
    /// .canPlaceRoot`'s extra OR-arm.
    MangroveRootsCanGrowThrough,
    /// The ground tag used by the bundled huge-brown-mushroom feature.
    HugeBrownMushroomCanPlaceOn,
    /// The ground tag used by the bundled huge-red-mushroom feature.
    HugeRedMushroomCanPlaceOn,
    /// Blocks that the mushroom cap and stem writers may replace.
    ReplaceableByMushrooms,
    /// `#minecraft:supports_bamboo` — bamboo's own floor survival rule.
    SupportsBamboo,
    /// Dedicated floors for simple-block dry vegetation.
    SupportsDryVegetation,
    /// Dedicated floors for simple-block azaleas.
    SupportsAzalea,
    /// Dedicated floors for simple-block crimson roots.
    SupportsCrimsonRoots,
    /// Dedicated floors for simple-block small dripleaves.
    SupportsSmallDripleaf,
    /// Dedicated floors for simple-block soul fire.
    SoulFireBaseBlocks,
    /// Mushroom floors that bypass the world-generation brightness gate.
    OverridesMushroomLightRequirement,
    /// Solid floors that support lily pads when the floor is not water.
    SupportsLilyPad,
    /// Ground states the giant-conifer decorator may replace with podzol.
    BeneathTreePodzolReplaceable,
    /// Ground blocks accepted by azalea root-system candidates.
    AzaleaGrowsOn,
}

impl Tag {
    /// Every variant, in declaration order. `TAG_COUNT` and the mask layout are
    /// both derived from this, so it is the single place a new tag registers.
    pub(super) const ALL: &[Tag] = &[
        Tag::CannotReplaceBelowTreeTrunk,
        Tag::SupportsVegetation,
        Tag::ReplaceableByTrees,
        Tag::Logs,
        Tag::SupportsCactus,
        Tag::SupportsSugarCane,
        Tag::Leaves,
        Tag::Air,
        Tag::Fluid,
        Tag::Water,
        Tag::Lava,
        Tag::Cactus,
        Tag::SugarCane,
        Tag::MangroveLogsCanGrowThrough,
        Tag::MangroveRootsCanGrowThrough,
        Tag::HugeBrownMushroomCanPlaceOn,
        Tag::HugeRedMushroomCanPlaceOn,
        Tag::ReplaceableByMushrooms,
        Tag::SupportsBamboo,
        Tag::SupportsDryVegetation,
        Tag::SupportsAzalea,
        Tag::SupportsCrimsonRoots,
        Tag::SupportsSmallDripleaf,
        Tag::SoulFireBaseBlocks,
        Tag::OverridesMushroomLightRequirement,
        Tag::SupportsLilyPad,
        Tag::BeneathTreePodzolReplaceable,
        Tag::AzaleaGrowsOn,
    ];

    const fn slot(self) -> usize {
        self as usize
    }
}

/// Number of bitsets in an [`IdTags`].
pub(super) const TAG_COUNT: usize = Tag::ALL.len();

/// The whole [`StateId`] space: `StateId` wraps a `u16`, so this is exact rather
/// than a guess, and the table can never need to grow.
const ID_SPACE: usize = lodestone_data::block_states::STATE_COUNT as usize;

/// 64-bit words per tag.
const WORDS_PER_TAG: usize = ID_SPACE / 64;

fn distance_property(value: u8) -> BuiltinPropertyValue {
    match value {
        0 => BuiltinPropertyValue::Value0,
        1 => BuiltinPropertyValue::Value1,
        2 => BuiltinPropertyValue::Value2,
        3 => BuiltinPropertyValue::Value3,
        4 => BuiltinPropertyValue::Value4,
        5 => BuiltinPropertyValue::Value5,
        6 => BuiltinPropertyValue::Value6,
        7 => BuiltinPropertyValue::Value7,
        _ => BuiltinPropertyValue::Value7,
    }
}

/// Per-[`super::VegTags`] bitsets answering [`Tag`] membership by [`StateId`].
///
/// See the module doc. Everything here is relaxed-atomic rather than locked: at
/// steady state the words are read-only, so the cache lines stay Shared across
/// however many threads are generating, which is the property `palette_names`
/// buys the same way in [`crate::dense_grid`].
pub(super) struct IdTags {
    /// The canonical state table has one process-wide identity, so a completed
    /// bind is valid for every generator using these tags.
    bound: AtomicBool,
    /// `TAG_COUNT` bitsets, concatenated, `WORDS_PER_TAG` words each.
    masks: Box<[AtomicU64]>,
    /// Each state's `distance=N` property value, or `-1` for a state that has no
    /// such property. Filled by [`VegTags::bind`] alongside the masks.
    ///
    /// The leaf-distance BFS asks this of all six neighbours of every
    /// cell it visits, so it is as hot as a tag test and gets the same treatment.
    /// 64 KiB, one byte per id — the property's range is `0..=7`.
    distance: Box<[AtomicI8]>,
    /// Memo for [`VegTags::rewrite`]: `(state, what) -> rewritten
    /// state`, with `None` recorded for a state that does not carry the property at
    /// all (so a repeated miss is still one hash lookup, not a repeated string
    /// scan).
    ///
    /// Keys use canonical state ids, which are process-wide and therefore valid
    /// across every generator and grid.
    ///
    /// A lock rather than an atomic table because the key space is
    /// two-dimensional and sparse — only leaf-ish states carry `distance` or
    /// `waterlogged`. It is taken **per rewritten leaf**, tens of times per
    /// column, never per block; and at steady state every entry is present, so it
    /// is a read guard and a hash, with no allocation.
    /// [`FastMap`], not the default hasher — U17 measured this among the
    /// vegetation maps still paying SipHash (0.8% of all worldgen CPU, shared with
    /// `VegGrid`'s overlay and `tree.rs`'s BFS visited set) and left the row for
    /// whoever owned these files; U19 took it.
    ///
    /// Order-safe because this map is **never iterated**: it is a pure memo reached
    /// only through `get` and `insert` (grep the field name, not the file —
    /// that is the check `docs/worldgen-fast-hashing.md` prescribes). Nothing about
    /// a rewrite's *value* changes; only which bucket it lands in.
    rewrites: RwLock<FastMap<(u32, Rewrite), Option<u32>>>,
}

/// A block-state property edit vegetal decoration performs on an
/// already-resolved state.
///
/// Both edits are resolved through the generated property table and memoized by
/// canonical input id, so repeated leaf updates remain allocation-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Rewrite {
    /// `distance=N`, the leaf-distance BFS's output.
    Distance(u8),
    /// `waterlogged=true|false`, `try_place_leaf`'s fix-up.
    Waterlogged(bool),
    /// `axis=x|y|z`, the pillar-axis property — the fancy trunk placer's log-axis rule
    /// and `FallenTreeFeature`'s own `getSidewaysStateModifier`, both of which
    /// pick a log's axis from the direction it was placed in rather than the
    /// configured (vertical) default.
    Axis(Axis),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Axis {
    X,
    Y,
    Z,
}

impl Default for IdTags {
    fn default() -> Self {
        Self {
            bound: AtomicBool::new(false),
            masks: (0..TAG_COUNT * WORDS_PER_TAG)
                .map(|_| AtomicU64::new(0))
                .collect(),
            distance: (0..ID_SPACE).map(|_| AtomicI8::new(-1)).collect(),
            rewrites: RwLock::new(FastMap::default()),
        }
    }
}

impl Clone for IdTags {
    /// An **unbound** table — see the module doc's gotcha on why the atomics are
    /// not copied.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for IdTags {
    /// A summary, not 26,624 atomics. `VegTags` derives `Debug` and is printed in
    /// test failure messages; dumping the raw masks would bury the tag sets that
    /// are the actually useful part.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdTags")
            .field("bound", &self.bound.load(Ordering::Relaxed))
            .field("tags", &TAG_COUNT)
            .finish()
    }
}

impl IdTags {
    fn bit(&self, tag: Tag, index: usize) -> bool {
        let word = tag.slot() * WORDS_PER_TAG + index / 64;
        self.masks[word].load(Ordering::Relaxed) & (1u64 << (index % 64)) != 0
    }

    fn set_bit(&self, tag: Tag, index: usize) {
        let word = tag.slot() * WORDS_PER_TAG + index / 64;
        self.masks[word].fetch_or(1u64 << (index % 64), Ordering::Relaxed);
    }

}

/// `LeavesBlock.DISTANCE`'s value in a canonical state string, if it has one.
///
/// The single definition of how the property is read; [`VegTags::bind`] fills the
/// [`IdTags::distance`] table from it.
fn parse_distance(id: StateId) -> Option<i32> {
    match Properties::from_state_id(id).get(PropertyKey::Distance)?.builtin_value()? {
        BuiltinPropertyValue::Value0 => Some(0),
        BuiltinPropertyValue::Value1 => Some(1),
        BuiltinPropertyValue::Value2 => Some(2),
        BuiltinPropertyValue::Value3 => Some(3),
        BuiltinPropertyValue::Value4 => Some(4),
        BuiltinPropertyValue::Value5 => Some(5),
        BuiltinPropertyValue::Value6 => Some(6),
        BuiltinPropertyValue::Value7 => Some(7),
        _ => None,
    }
}

/// Replaces the value of `property` in a canonical state string, appending
/// nothing if the property is absent (`None`).
///
/// The `replace_range` idiom both string sites used, kept in one place. Only
/// reached on a [`IdTags::rewrites`] miss, i.e. during warmup.
impl VegTags {
    /// Whether the block represented by `base` is in `tag`.
    ///
    /// This is the only definition of membership, and [`Self::bind`] calls it
    /// to fill the bits.
    fn member(&self, tag: Tag, block: Block) -> bool {
        match tag {
            Tag::CannotReplaceBelowTreeTrunk => self.cannot_replace_below_tree_trunk.contains(block),
            Tag::SupportsVegetation => self.supports_vegetation.contains(block),
            Tag::ReplaceableByTrees => self.replaceable_by_trees.contains(block),
            Tag::Logs => self.logs.contains(block),
            Tag::SupportsCactus => self.supports_cactus.contains(block),
            Tag::SupportsSugarCane => self.supports_sugar_cane.contains(block),
            Tag::Leaves => self.leaves.contains(block),
            // Delegated, not re-spelled: `config`'s two functions are the single
            // definition of what counts as air/fluid, so these bits cannot drift
            // from what the remaining string callers answer.
            Tag::Air => matches!(block, Block::Air | Block::CaveAir | Block::VoidAir),
            Tag::Fluid => matches!(block, Block::Water | Block::Lava),
            Tag::Water => block == Block::Water,
            Tag::Lava => block == Block::Lava,
            Tag::Cactus => block == Block::Cactus,
            Tag::SugarCane => block == Block::SugarCane,
            Tag::MangroveLogsCanGrowThrough => self.mangrove_logs_can_grow_through.contains(block),
            Tag::MangroveRootsCanGrowThrough => self.mangrove_roots_can_grow_through.contains(block),
            Tag::HugeBrownMushroomCanPlaceOn => self.huge_brown_mushroom_can_place_on.contains(block),
            Tag::HugeRedMushroomCanPlaceOn => self.huge_red_mushroom_can_place_on.contains(block),
            Tag::ReplaceableByMushrooms => self.replaceable_by_mushrooms.contains(block),
            Tag::SupportsBamboo => self.supports_bamboo.contains(block),
            Tag::SupportsDryVegetation => self.supports_dry_vegetation.contains(block),
            Tag::SupportsAzalea => self.supports_azalea.contains(block),
            Tag::SupportsCrimsonRoots => self.supports_crimson_roots.contains(block),
            Tag::SupportsSmallDripleaf => self.supports_small_dripleaf.contains(block),
            Tag::SoulFireBaseBlocks => self.soul_fire_base_blocks.contains(block),
            Tag::OverridesMushroomLightRequirement => self.overrides_mushroom_light_requirement.contains(block),
            Tag::SupportsLilyPad => self.supports_lily_pad.contains(block),
            Tag::BeneathTreePodzolReplaceable => {
                self.beneath_tree_podzol_replaceable.contains(block)
            }
            Tag::AzaleaGrowsOn => self.azalea_grows_on.contains(block),
        }
    }

    /// Builds the canonical-state masks once. State ids are process-global, so
    /// no generator-local identity or late binding is needed.
    pub fn bind(&self) {
        if self.id_tags.bound.swap(true, Ordering::AcqRel) {
            return;
        }
        for raw in 0..ID_SPACE {
            let id = StateId::new(raw as u32).expect("generated state id is valid");
            let block = id.block();
            for tag in Tag::ALL.iter().copied() {
                if self.member(tag, block) {
                    self.id_tags.set_bit(tag, raw);
                }
            }
            if let Some(d) = parse_distance(id) {
                self.id_tags.distance[raw].store(
                    i8::try_from(d).unwrap_or(-1),
                    Ordering::Relaxed,
                );
            }
        }
    }

    /// The leaf-distance lookup's non-tag half, by id — the value of
    /// `id`'s `distance` property, or `None` if it has none.
    ///
    /// Hot: the leaf-distance BFS asks this of every neighbour of every cell
    /// its BFS visits.
    pub(super) fn distance_of(&self, id: StateId) -> Option<i32> {
        let index = id.index();
        if self.id_tags.bound.load(Ordering::Acquire) {
            bump_fast();
            let d = self.id_tags.distance[index].load(Ordering::Relaxed);
            (d >= 0).then_some(i32::from(d))
        } else {
            bump_slow();
            None
        }
    }

    /// The id of `id`'s state with one property changed, or `None` if `id` does not
    /// carry that property at all.
    ///
    /// Memoised (see [`IdTags::rewrites`]), so at steady state this is a read
    /// guard and a hash lookup with no string work and no allocation. The `None`
    /// answer is memoised too — `try_place_leaf` asks for a `waterlogged` rewrite
    /// on every leaf it places, and a species whose leaves have no such property
    /// would otherwise re-scan the name every time.
    pub(super) fn rewrite(
        &self,
        id: StateId,
        what: Rewrite,
    ) -> Option<StateId> {
        let key = (id.raw(), what);
        if let Some(&hit) = self
            .id_tags
            .rewrites
            .read()
            .expect("veg id rewrite memo poisoned")
            .get(&key)
        {
            return hit.and_then(StateId::new);
        }
        let (property, value) = match what {
            Rewrite::Distance(n) => (PropertyKey::Distance, distance_property(n.min(15))),
            Rewrite::Waterlogged(true) => (PropertyKey::Waterlogged, BuiltinPropertyValue::True),
            Rewrite::Waterlogged(false) => (PropertyKey::Waterlogged, BuiltinPropertyValue::False),
            Rewrite::Axis(Axis::X) => (PropertyKey::Axis, BuiltinPropertyValue::X),
            Rewrite::Axis(Axis::Y) => (PropertyKey::Axis, BuiltinPropertyValue::Y),
            Rewrite::Axis(Axis::Z) => (PropertyKey::Axis, BuiltinPropertyValue::Z),
        };
        let out = Properties::from_state_id(id)
            .with_builtin(property, value)
            .ok()
            .and_then(|properties| Properties::state_for_block(id.block(), &properties));
        self.id_tags
            .rewrites
            .write()
            .expect("veg id rewrite memo poisoned")
            .insert(key, out.map(StateId::raw));
        out
    }

    /// Whether `id`'s base state is in `tag`.
    ///
    /// One relaxed atomic load for any id [`Self::bind`] has seen. Unbound ids
    /// are rejected until the next bind pass.
    pub(super) fn has(&self, tag: Tag, id: StateId) -> bool {
        let index = id.index();
        if self.id_tags.bound.load(Ordering::Acquire) {
            bump_fast();
            self.id_tags.bit(tag, index)
        } else {
            bump_slow();
            false
        }
    }

}

/// `tags.has(..., grid.get_id(x, y, z))` — the shape almost every call site wants.
///
/// A free function rather than a `VegGrid` method because the tag sets live on
/// [`VegTags`] and the grid must not learn about them: `grid.rs` is the medium
/// Unit 7 owns, and the coordinate-space bug recorded in [`VegGrid`]'s own doc
/// comment is reason enough not to grow its responsibilities.
pub(super) fn tag_at(grid: &VegGrid, tags: &VegTags, tag: Tag, x: i32, y: i32, z: i32) -> bool {
    tags.has(tag, grid.get_id(x, y, z))
}

thread_local! {
    /// Queries answered from the bitset. See the module doc: without this the
    /// acceptance gate could not tell a working fast path from a table that never
    /// bound.
    ///
    /// Thread-local and `const`-initialised, for the two reasons
    /// `grid::census` already documents: a process-global counter would fold
    /// other tests' work into a gate whose expected value is exact (the
    /// *duration* species of vacuous test), and a lazily-initialised
    /// `thread_local!` allocates on first touch, which the allocation gate would
    /// then count.
    static FAST: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Queries that arrived before the id was bound.
    static SLOW: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn bump_fast() {
    FAST.with(|c| c.set(c.get().wrapping_add(1)));
}

fn bump_slow() {
    SLOW.with(|c| c.set(c.get().wrapping_add(1)));
}

/// Bitset-answered membership queries on this thread since [`reset_counts`].
#[must_use]
pub fn fast_hits() -> u64 {
    FAST.with(std::cell::Cell::get)
}

/// String-path membership queries on this thread since [`reset_counts`] — the
/// number a warm pass must drive to zero.
#[must_use]
pub fn slow_hits() -> u64 {
    SLOW.with(std::cell::Cell::get)
}

/// Zeroes both counters for this thread.
pub fn reset_counts() {
    FAST.with(|c| c.set(0));
    SLOW.with(|c| c.set(0));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A variant added without a [`Tag::ALL`] entry would leave its bits
    /// permanently zero — "nothing is in this tag", silently. `slot()` indexes
    /// the mask array by discriminant, so `ALL` must also be in declaration
    /// order and complete.
    #[test]
    fn tag_all_is_complete_and_in_discriminant_order() {
        let last = Tag::ALL.last().copied().expect("Tag::ALL cannot be empty");
        assert_eq!(TAG_COUNT, last.slot() + 1, "Tag::ALL must include every variant");
        for (i, tag) in Tag::ALL.iter().enumerate() {
            assert_eq!(
                tag.slot(),
                i,
                "Tag::ALL must list variants in discriminant order, since slot() \
                 indexes the mask array by discriminant: {tag:?} is at ALL[{i}] but \
                 slots to {}",
                tag.slot()
            );
        }
    }

    /// The bitset must answer the same membership questions as the canonical
    /// state metadata for every generated state it covers.
    #[test]
    fn the_bitset_answers_canonical_state_membership() {
        let mut tags = VegTags::default();
        tags.logs.insert(Block::OakLog);
        tags.logs.insert(Block::AcaciaLog);
        tags.replaceable_by_trees.insert(Block::ShortGrass);
        tags.supports_vegetation.insert(Block::GrassBlock);
        tags.leaves.insert(Block::OakLeaves);

        let names = [
            "minecraft:oak_log[axis=y]",
            "minecraft:acacia_log[axis=x]",
            "minecraft:short_grass",
            "minecraft:grass_block[snowy=false]",
            "minecraft:oak_leaves[distance=7,persistent=false,waterlogged=false]",
            "minecraft:stone",
            "minecraft:water[level=0]",
            "minecraft:water",
            "minecraft:lava[level=0]",
            "minecraft:cave_air",
            "minecraft:void_air",
            "minecraft:cactus[age=0]",
            "minecraft:sugar_cane",
        ];
        let ids: Vec<StateId> = names
            .iter()
            .map(|name| StateId::from_state_str(name).expect("test state is generated"))
            .collect();

        tags.bind();
        assert!(
            tags.id_tags.bound.load(Ordering::Relaxed),
            "bind must publish the canonical state masks"
        );

        reset_counts();
        for (name, &id) in names.iter().zip(&ids) {
            let block = id.block();
            for tag in Tag::ALL.iter().copied() {
                assert_eq!(
                    tags.has(tag, id),
                    tags.member(tag, block),
                    "bitset membership disagrees for {name:?} on {tag:?}"
                );
            }
        }
        assert_eq!(
            slow_hits(), 0,
            "every id above was queried after the canonical masks were bound"
        );
        assert!(fast_hits() > 0, "the bitset path must actually have been used");

    }

    #[test]
    fn a_rewrite_preserves_the_canonical_state_identity() {
        let tags = VegTags::default();
        let leaf = StateId::from_state_str(
            "minecraft:oak_leaves[distance=7,persistent=false,waterlogged=false]",
        )
        .expect("test state is generated");
        let out = tags
            .rewrite(leaf, Rewrite::Distance(3))
            .expect("a leaf state carries a distance property");
        assert_eq!(out.block(), leaf.block());
        assert_eq!(
            Properties::from_state_id(out)
                .get(PropertyKey::Distance)
                .and_then(|value| value.builtin_value()),
            Some(BuiltinPropertyValue::Value3),
        );
    }
}
