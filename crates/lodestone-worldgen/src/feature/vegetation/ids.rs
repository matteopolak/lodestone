//! Tag membership as fixed bitsets indexed by [`StateId`] — vegetal decoration's
//! O(1), lock-free, allocation-free replacement for
//! `tags.some_set.contains(base_id(grid.get(x, y, z)))`.
//!
//! # What it is
//!
//! Unit 8 of [`docs/plans/worldgen-rewrite.md`](../../../../../docs/plans/worldgen-rewrite.md),
//! candidate 2 of its "Vegetation: cost per draw" section. Every ground check,
//! every trunk-position test, every air-or-leaves anchor and every heightmap cell
//! used to cost:
//!
//! 1. [`StateInterner::name_of`] — an `RwLock` **read guard**, on a cache line
//!    shared by every concurrent generator call (the shape `4307b59` was reverted
//!    for, at 289 concurrent columns);
//! 2. `split('[')` to recover the base name;
//! 3. a `HashSet<String>` probe — hashing ~20 bytes of UTF-8.
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
//! 13 tags × 65,536 bits = 106 KiB per [`super::VegTags`], one `alloc_zeroed` at
//! construction, and a [`super::VegTags`] is per-generator.
//!
//! Bits are only *meaningful* for ids the table has actually examined, so
//! [`IdTags::resolved`] is a watermark: ids below it answer from the bitset, ids
//! at or above it fall back to the string path. [`super::VegTags::bind`] walks the
//! interner's new ids and raises the watermark; the driver calls it **once per
//! decoration pass**, which is the only place `interner.len()`'s lock is taken.
//!
//! ## Why the fallback is required, not defensive
//!
//! Decoration mints ids *during* its own pass — a leaf rewritten to `distance=3`
//! is a state the interner may never have seen. Reading such an id from the
//! bitset would answer `false` for every tag, which is a **wrong answer, not a
//! slow one**: an unexamined `oak_log` would fail `#minecraft:logs` and change
//! where leaves decay. So the watermark test is a correctness gate, and the
//! string path behind it is the same code the pre-U8 engine ran.
//!
//! It also makes every existing unit test work untouched. A test that builds
//! `VegTags::default()`, inserts into `tags.leaves` and calls
//! [`super::place_tree`] directly never binds anything, so `resolved == 0`, so
//! every query takes the string path and answers exactly what it answered before.
//!
//! **That is also why [`fast_hits`]/[`slow_hits`] exist.** A silent fallback is
//! indistinguishable from a working fast path, and this repo's rule is that a
//! claim to have fixed a site needs a counter proving the site executed. The
//! acceptance gate asserts `fast_hits > 0` **and** `slow_hits == 0` on a warm
//! pass; without the first it would pass against a table that never bound, and
//! without the second it would pass against one that bound and then fell back for
//! everything.
//!
//! # How to change it, and the gotchas
//!
//! * **Never mutate a [`super::VegTags`]'s `HashSet`s after [`super::VegTags::bind`]
//!   has run.** The bitset is a cache of those sets, and nothing re-derives it:
//!   an insert after binding is visible to the string path and invisible to the
//!   bitset, so the same query answers two different things depending on one id's
//!   value. Production builds the sets once in
//!   [`super::build_veg_tags`] and never touches them again; the tests that do
//!   mutate them never bind. If you ever need both, add a `rebind` that clears
//!   the masks and resets the watermark to 0.
//! * **[`Clone`] deliberately returns an *unbound* table.** Cloning the atomics'
//!   values would be safe but the copy would then be bound to an interner the
//!   clone's owner may not be using. An unbound clone is always correct (string
//!   path) and rebinds on first use, so the failure mode is a slow pass, never a
//!   wrong block.
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
use std::sync::atomic::{AtomicI8, AtomicU64, AtomicUsize, Ordering};

use crate::interner::{StateId, StateInterner};

use super::VegGrid;
use super::base_id;
use super::config::VegTags;

/// The membership questions vegetal decoration asks of a block state.
///
/// The first seven are real registry tags resolved by [`super::build_veg_tags`];
/// the rest are base-name equalities the old engine spelled inline. See the
/// module doc on why they share one mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tag {
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
}

impl Tag {
    /// Every variant, in declaration order. `TAG_COUNT` and the mask layout are
    /// both derived from this, so it is the single place a new tag registers.
    pub(super) const ALL: [Tag; 15] = [
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
    ];

    const fn slot(self) -> usize {
        self as usize
    }
}

/// Number of bitsets in an [`IdTags`].
pub(super) const TAG_COUNT: usize = Tag::ALL.len();

/// The whole [`StateId`] space: `StateId` wraps a `u16`, so this is exact rather
/// than a guess, and the table can never need to grow.
const ID_SPACE: usize = u16::MAX as usize + 1;

/// 64-bit words per tag.
const WORDS_PER_TAG: usize = ID_SPACE / 64;

/// Decimal literals for the `distance=N` rewrite, so building the replacement
/// value needs no `n.to_string()`. Sized past `LeavesBlock.DECAY_DISTANCE` (7) on
/// purpose — the caller clamps, and an index panic here would be a worse failure
/// than a wrong-but-bounded literal.
const DISTANCE_LITERALS: [&str; 16] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15",
];

/// Per-[`super::VegTags`] bitsets answering [`Tag`] membership by [`StateId`].
///
/// See the module doc. Everything here is relaxed-atomic rather than locked: at
/// steady state the words are read-only, so the cache lines stay Shared across
/// however many threads are generating, which is the property `palette_names`
/// buys the same way in [`crate::dense_grid`].
pub(super) struct IdTags {
    /// The [`StateInterner::instance_id`] these masks describe. A different
    /// interner clears them — ids from two interners are not comparable.
    instance: AtomicU64,
    /// Ids `[0, resolved)` have meaningful bits. See the module doc: this is a
    /// correctness gate, not an optimisation.
    resolved: AtomicUsize,
    /// `TAG_COUNT` bitsets, concatenated, `WORDS_PER_TAG` words each.
    masks: Box<[AtomicU64]>,
    /// Each state's `distance=N` property value, or `-1` for a state that has no
    /// such property. Filled by [`VegTags::bind`] alongside the masks, and valid
    /// under the same [`Self::resolved`] watermark.
    ///
    /// The leaf-distance BFS asks this of all six neighbours of every
    /// cell it visits, so it is as hot as a tag test and gets the same treatment.
    /// 64 KiB, one byte per id — the property's range is `0..=7`.
    distance: Box<[AtomicI8]>,
    /// Memo for [`VegTags::rewrite`]: `(interner, state, what) -> rewritten
    /// state`, with `None` recorded for a state that does not carry the property at
    /// all (so a repeated miss is still one hash lookup, not a repeated string
    /// scan).
    ///
    /// **The interner's instance id is part of the key, and that is load-bearing.**
    /// It was not, once, and `tree_placement_is_deterministic_across_two_independent_generators`
    /// caught it immediately: that test hands one `VegTags` to two grids with two
    /// private interners, the memo returned the first interner's `StateId` to the
    /// second grid, and `name_of` panicked with "the len is 8 but the index is 10".
    /// A shorter tree would not have panicked — it would have stored a plausible
    /// wrong block. Clearing on [`Self::instance`] change is not sufficient cover,
    /// because nothing calls [`VegTags::bind`] on the direct-placement path this
    /// memo is still live on.
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
    /// only through `get`, `insert` and `clear` (grep the field name, not the file —
    /// that is the check `docs/worldgen-fast-hashing.md` prescribes). Nothing about
    /// a rewrite's *value* changes; only which bucket it lands in.
    rewrites: RwLock<FastMap<(u64, u16, Rewrite), Option<u16>>>,
}

/// A block-state property edit vegetal decoration performs on an
/// already-resolved state.
///
/// Both were string surgery before Unit 8 — `state.to_string()` then
/// `replace_range` then re-intern, once per leaf — and both are named in
/// `docs/worldgen-state-interning.md`'s account of what Unit 8 still had to
/// remove.
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
    Axis(&'static str),
}

impl Default for IdTags {
    fn default() -> Self {
        Self {
            instance: AtomicU64::new(u64::MAX),
            resolved: AtomicUsize::new(0),
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
    /// A summary, not 13,312 atomics. `VegTags` derives `Debug` and is printed in
    /// test failure messages; dumping the raw masks would bury the tag sets that
    /// are the actually useful part.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdTags")
            .field("instance", &self.instance.load(Ordering::Relaxed))
            .field("resolved", &self.resolved.load(Ordering::Relaxed))
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

    fn clear(&self) {
        for word in &self.masks {
            word.store(0, Ordering::Relaxed);
        }
        for slot in &self.distance {
            slot.store(-1, Ordering::Relaxed);
        }
        // Ids from the old interner are meaningless against the new one, so a
        // retained rewrite would map one arbitrary state onto another. This is the
        // silent-wrong-value class; it must be dropped with the bits.
        self.rewrites
            .write()
            .expect("veg id rewrite memo poisoned")
            .clear();
        self.resolved.store(0, Ordering::Relaxed);
    }
}

/// `LeavesBlock.DISTANCE`'s value in a canonical state string, if it has one.
///
/// The single definition of how the property is read; [`VegTags::bind`] fills the
/// [`IdTags::distance`] table from it and the fallback path calls it directly.
fn parse_distance(state: &str) -> Option<i32> {
    let idx = state.find("distance=")?;
    let start = idx + "distance=".len();
    let end = state[start..]
        .find([',', ']'])
        .map_or(state.len(), |o| start + o);
    state[start..end].parse().ok()
}

/// Replaces the value of `property` in a canonical state string, appending
/// nothing if the property is absent (`None`).
///
/// The `replace_range` idiom both string sites used, kept in one place. Only
/// reached on a [`IdTags::rewrites`] miss, i.e. during warmup.
fn rewrite_property(state: &str, property: &str, value: &str) -> Option<String> {
    let idx = state.find(property)?;
    let start = idx + property.len();
    let end = state[start..]
        .find([',', ']'])
        .map_or(state.len(), |o| start + o);
    let mut out = state.to_string();
    out.replace_range(start..end, value);
    Some(out)
}

impl VegTags {
    /// Whether `base` — a **base** state name, no properties — is in `tag`.
    ///
    /// The string path, and the definition the bitset caches. Keep the two in
    /// step: this function is the only thing that decides membership, and
    /// [`Self::bind`] calls it to fill the bits.
    fn member(&self, tag: Tag, base: &str) -> bool {
        match tag {
            Tag::CannotReplaceBelowTreeTrunk => self.cannot_replace_below_tree_trunk.contains(base),
            Tag::SupportsVegetation => self.supports_vegetation.contains(base),
            Tag::ReplaceableByTrees => self.replaceable_by_trees.contains(base),
            Tag::Logs => self.logs.contains(base),
            Tag::SupportsCactus => self.supports_cactus.contains(base),
            Tag::SupportsSugarCane => self.supports_sugar_cane.contains(base),
            Tag::Leaves => self.leaves.contains(base),
            // Delegated, not re-spelled: `config`'s two functions are the single
            // definition of what counts as air/fluid, so these bits cannot drift
            // from what the remaining string callers answer.
            Tag::Air => super::config::is_air(base),
            Tag::Fluid => super::config::is_fluid(base),
            Tag::Water => base == "minecraft:water",
            Tag::Lava => base == "minecraft:lava",
            Tag::Cactus => base == "minecraft:cactus",
            Tag::SugarCane => base == "minecraft:sugar_cane",
            Tag::MangroveLogsCanGrowThrough => self.mangrove_logs_can_grow_through.contains(base),
            Tag::MangroveRootsCanGrowThrough => self.mangrove_roots_can_grow_through.contains(base),
        }
    }

    /// Brings the bitsets up to date with `interner`, so subsequent membership
    /// queries take the O(1) path.
    ///
    /// Call **once per decoration pass**, from the driver — this is the only
    /// place [`StateInterner::len`]'s `RwLock` is taken, and calling it per query
    /// would put the lock back in the hot loop, defeating the whole point.
    ///
    /// Idempotent, and safe to race: two threads filling the same id range write
    /// the same bits, and the watermark only ever moves forward
    /// ([`AtomicUsize::fetch_max`]).
    pub fn bind(&self, interner: &StateInterner) {
        let instance = interner.instance_id();
        if self.id_tags.instance.load(Ordering::Acquire) != instance {
            self.id_tags.clear();
            self.id_tags.instance.store(instance, Ordering::Release);
        }
        // One lock acquisition per pass, here and nowhere else.
        let len = interner.len().min(ID_SPACE);
        let done = self.id_tags.resolved.load(Ordering::Acquire);
        if len <= done {
            return;
        }
        for raw in done..len {
            let id = StateId::from_raw(u16::try_from(raw).expect("raw < ID_SPACE fits u16"));
            let name = interner.name_of(id);
            let base = base_id(name);
            for tag in Tag::ALL {
                if self.member(tag, base) {
                    self.id_tags.set_bit(tag, raw);
                }
            }
            if let Some(d) = parse_distance(name) {
                self.id_tags.distance[raw].store(
                    i8::try_from(d).unwrap_or(-1),
                    Ordering::Relaxed,
                );
            }
        }
        self.id_tags.resolved.fetch_max(len, Ordering::Release);
    }

    /// The leaf-distance lookup's non-tag half, by id — the value of
    /// `id`'s `distance` property, or `None` if it has none.
    ///
    /// Hot: the leaf-distance BFS asks this of every neighbour of every cell
    /// its BFS visits.
    pub(super) fn distance_of(&self, interner: &StateInterner, id: StateId) -> Option<i32> {
        let index = id.index();
        if self.fast_ok(interner, index) {
            bump_fast();
            let d = self.id_tags.distance[index].load(Ordering::Relaxed);
            (d >= 0).then_some(i32::from(d))
        } else {
            bump_slow();
            parse_distance(interner.name_of(id))
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
        interner: &StateInterner,
        id: StateId,
        what: Rewrite,
    ) -> Option<StateId> {
        let key = (interner.instance_id(), id.raw(), what);
        if let Some(&hit) = self
            .id_tags
            .rewrites
            .read()
            .expect("veg id rewrite memo poisoned")
            .get(&key)
        {
            return hit.map(StateId::from_raw);
        }
        let (property, value): (&str, &str) = match what {
            Rewrite::Distance(n) => ("distance=", DISTANCE_LITERALS[usize::from(n.min(15))]),
            Rewrite::Waterlogged(true) => ("waterlogged=", "true"),
            Rewrite::Waterlogged(false) => ("waterlogged=", "false"),
            Rewrite::Axis(v) => ("axis=", v),
        };
        let out = rewrite_property(interner.name_of(id), property, value)
            .map(|name| interner.id_of(&name));
        self.id_tags
            .rewrites
            .write()
            .expect("veg id rewrite memo poisoned")
            .insert(key, out.map(StateId::raw));
        out
    }

    /// Whether `id`'s base state is in `tag`.
    ///
    /// One relaxed atomic load for any id [`Self::bind`] has seen; the pre-U8
    /// string path for anything newer (see the module doc — that fallback is a
    /// correctness gate).
    pub(super) fn has(&self, interner: &StateInterner, tag: Tag, id: StateId) -> bool {
        let index = id.index();
        if self.fast_ok(interner, index) {
            bump_fast();
            self.id_tags.bit(tag, index)
        } else {
            bump_slow();
            self.member(tag, base_id(interner.name_of(id)))
        }
    }

    /// Whether the bitsets can answer for `index`: they must describe **this**
    /// interner, and `index` must be below the watermark.
    ///
    /// The instance check is not redundant with [`Self::bind`]'s clear-on-change.
    /// A `VegTags` can be shared by two grids with two private interners and
    /// bound by neither — `place_tree` and friends are called directly by tests
    /// and by nothing else in production — so "whoever bound last" is not a
    /// reliable scoping. Without this, a `bind` against interner A would answer
    /// confidently wrong for interner B's ids, which is the same
    /// silent-wrong-value class the rewrite memo's key documents.
    ///
    /// Two relaxed-ish atomic loads and a `u64` field read: no lock, and nothing
    /// shared is written at steady state.
    fn fast_ok(&self, interner: &StateInterner, index: usize) -> bool {
        self.id_tags.instance.load(Ordering::Acquire) == interner.instance_id()
            && index < self.id_tags.resolved.load(Ordering::Acquire)
    }
}

/// `tags.has(..., grid.get_id(x, y, z))` — the shape almost every call site wants.
///
/// A free function rather than a `VegGrid` method because the tag sets live on
/// [`VegTags`] and the grid must not learn about them: `grid.rs` is the medium
/// Unit 7 owns, and the coordinate-space bug recorded in [`VegGrid`]'s own doc
/// comment is reason enough not to grow its responsibilities.
pub(super) fn tag_at(grid: &VegGrid, tags: &VegTags, tag: Tag, x: i32, y: i32, z: i32) -> bool {
    tags.has(grid.interner(), tag, grid.get_id(x, y, z))
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
    /// Queries that fell back to the string path.
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
        assert_eq!(TAG_COUNT, 15, "TAG_COUNT is derived from Tag::ALL");
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

    /// The bitset and the string path must answer identically for every state the
    /// table has bound — and the *control* is that they must also answer
    /// identically for an id past the watermark, which exercises the fallback.
    ///
    /// This is the differential gate for the whole file: a wrong `member` arm, a
    /// wrong `slot()`, or an off-by-one in the watermark all show up here as a
    /// disagreement between two implementations of one question.
    #[test]
    fn the_bitset_answers_what_the_string_path_answers() {
        let interner = StateInterner::new();
        let mut tags = VegTags::default();
        tags.logs.insert("minecraft:oak_log".to_string());
        tags.logs.insert("minecraft:acacia_log".to_string());
        tags.replaceable_by_trees.insert("minecraft:short_grass".to_string());
        tags.supports_vegetation.insert("minecraft:grass_block".to_string());
        tags.leaves.insert("minecraft:oak_leaves".to_string());

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
        let ids: Vec<StateId> = names.iter().map(|n| interner.id_of(n)).collect();

        tags.bind(&interner);
        assert!(
            tags.id_tags.resolved.load(Ordering::Relaxed) >= ids.len(),
            "bind must cover every id interned before it ran"
        );

        reset_counts();
        for (name, &id) in names.iter().zip(&ids) {
            let base = base_id(name);
            for tag in Tag::ALL {
                assert_eq!(
                    tags.has(&interner, tag, id),
                    tags.member(tag, base),
                    "bitset and string path disagree for {name:?} on {tag:?}"
                );
            }
        }
        assert_eq!(
            slow_hits(), 0,
            "every id above was interned before bind, so none may take the fallback"
        );
        assert!(fast_hits() > 0, "the bitset path must actually have been used");

        // Control for the fallback: an id minted AFTER bind is past the
        // watermark, so it must take the string path -- and still answer right.
        let late = interner.id_of("minecraft:oak_log[axis=z]");
        reset_counts();
        assert!(
            tags.has(&interner, Tag::Logs, late),
            "an id minted after bind must still resolve through the string path"
        );
        assert_eq!(slow_hits(), 1, "and it must be the fallback that answered it");
        assert_eq!(fast_hits(), 0);

        // ...and after a rebind the same id is on the fast path.
        tags.bind(&interner);
        reset_counts();
        assert!(tags.has(&interner, Tag::Logs, late));
        assert_eq!(fast_hits(), 1, "rebinding must promote the late id");
        assert_eq!(slow_hits(), 0);
    }

    /// Ids from a different interner are not comparable, so a rebind against one
    /// must throw the old bits away rather than answer from them. Without the
    /// clear, `oak_log`'s id in interner A could be `stone`'s in interner B and
    /// the tag test would answer confidently wrong — the silent-wrong-value class.
    #[test]
    fn binding_a_second_interner_discards_the_first_ones_bits() {
        let a = StateInterner::new();
        let mut tags = VegTags::default();
        tags.logs.insert("minecraft:oak_log".to_string());
        // In `a`, pad so that oak_log lands at some id > 0.
        for pad in 0..5 {
            let _ = a.id_of(&format!("minecraft:pad{pad}"));
        }
        let log_a = a.id_of("minecraft:oak_log");
        tags.bind(&a);
        assert!(tags.has(&a, Tag::Logs, log_a));

        // A fresh interner where the SAME numeric id is a non-log.
        let b = StateInterner::new();
        let stone_b = b.id_of("minecraft:stone");
        assert_eq!(
            stone_b.index(),
            1,
            "air is 0, so the first state interned after it is 1 -- the fixture \
             depends on this to make the ids collide"
        );
        let log_b = b.id_of("minecraft:oak_log");
        tags.bind(&b);
        assert!(tags.has(&b, Tag::Logs, log_b));
        assert!(
            !tags.has(&b, Tag::Logs, stone_b),
            "stone must not inherit a log bit set for the same id in another interner"
        );

        // ...and the reverse direction, which `bind`'s clear-on-change does NOT
        // cover: after binding to `b`, a query about `a`'s ids must fall back to
        // the string path rather than read `b`'s bits. This is what `fast_ok`'s
        // instance check buys.
        reset_counts();
        assert!(tags.has(&a, Tag::Logs, log_a));
        assert_eq!(
            slow_hits(), 1,
            "a query against the interner that is NOT bound must take the string path"
        );
    }

    /// The regression this file's worst bug left behind.
    ///
    /// One `VegTags` shared by two grids with two private interners — the exact
    /// shape of `tree_placement_is_deterministic_across_two_independent_generators`
    /// — must not let the first interner's rewritten id escape into the second.
    /// Before the interner's instance id became part of the memo key, this
    /// returned interner `a`'s `StateId` for interner `b` and `name_of` panicked
    /// with "the len is 8 but the index is 10". Held here rather than only in that
    /// distant test because the *mechanism* is local to this file.
    #[test]
    fn a_rewrite_never_escapes_the_interner_it_was_computed_in() {
        let tags = VegTags::default();
        let leaf = "minecraft:oak_leaves[distance=7,persistent=false,waterlogged=false]";

        let a = StateInterner::new();
        // Pad `a` so the same state has different ids in the two interners; without
        // this the bug would be invisible because the wrong id would be right.
        for pad in 0..6 {
            let _ = a.id_of(&format!("minecraft:pad{pad}"));
        }
        let leaf_a = a.id_of(leaf);
        let out_a = tags
            .rewrite(&a, leaf_a, Rewrite::Distance(3))
            .expect("a leaf state carries a distance property");

        let b = StateInterner::new();
        let leaf_b = b.id_of(leaf);
        assert_ne!(
            leaf_a.raw(),
            leaf_b.raw(),
            "the fixture needs the same state to have different ids in the two \
             interners, or it cannot detect the leak"
        );
        let out_b = tags
            .rewrite(&b, leaf_b, Rewrite::Distance(3))
            .expect("a leaf state carries a distance property");

        // The names must agree; the ids must not have been shared.
        assert_eq!(a.name_of(out_a), b.name_of(out_b));
        assert!(
            a.name_of(out_a).contains("distance=3"),
            "the rewrite must actually have changed the property, got {:?}",
            a.name_of(out_a)
        );
        // The real assertion: `out_b` must be a valid id IN `b`. Before the fix
        // this was `out_a`, an id past the end of `b`'s table.
        assert!(
            out_b.index() < b.len(),
            "rewrite returned id {} for an interner holding only {} states — the \
             first interner's id leaked",
            out_b.index(),
            b.len()
        );
    }
}
