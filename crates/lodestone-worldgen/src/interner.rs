//! Numeric interning of block-state strings — [`StateId`], a `u16` handle for a
//! canonical block-state string, and [`StateInterner`], the per-generator table
//! that maps between the two.
//!
//! # Why this exists
//!
//! `docs/plans/worldgen-rewrite.md`'s D2: a steady-state warm column performed
//! **905,459 heap allocations**, of which **884,736** were a single
//! `state.to_string()` in `OverworldGenerator::stitch_veg_region`'s
//! `48 × 384 × 48` triple loop — 97.7% of all heap traffic on the serve path
//! from one line. Every grid in this engine already converges on a dense
//! palette-indexed representation; the strings existed only because the *edges*
//! of each grid (`get`/`set`) still spoke `&str`, so each hop between two dense
//! grids round-tripped through the heap.
//!
//! A [`StateId`] is that hop made free. Unit 3's acceptance criterion is the
//! bench's counting allocator (`benches/generation.rs`, `measure_allocs`)
//! reading **0** for a steady-state column — a real allocator count, not this
//! crate's hand-bumped `crate::counters::string_allocs`, which could be
//! satisfied by deleting bump calls.
//!
//! # Ids are deliberately *not* world-visible
//!
//! `crate::overworld`'s module doc (see its `RandomState` post-mortem) records
//! an iteration-order bug that already shipped here once, so the obvious worry
//! about interning is that id-assignment order leaks into the served palette
//! and becomes world-visible. **It cannot, by construction:**
//!
//! * A [`crate::dense_grid::DenseBlockGrid`] keeps its own **local palette** of
//!   [`StateId`]s in *first-write order*, exactly as it previously kept a local
//!   palette of `String`s in first-write order. Interning changed what the
//!   palette entries *are*, not the order they are appended in.
//! * `DenseBlockGrid::into_palette_and_blocks` therefore emits a `Vec<String>`
//!   in byte-identical order to before, and `blocks` is untouched.
//!
//! So a [`StateInterner`] may assign ids in any order at all — including an
//! order that varies between two runs — without changing one byte of output.
//! `column_is_byte_identical_across_two_independently_constructed_generators`
//! is the gate that holds that claim, and it would fail if any id reached the
//! wire. This is the property that makes the growable design below safe;
//! do not "optimise" a grid into storing interner ids directly in `blocks`
//! without re-deriving it, because that *would* put id order on the wire.
//!
//! # Growable, not frozen — and why that still reaches zero allocations
//!
//! Interning a state the table has not seen allocates (once, to own the
//! string). A frozen table built exhaustively at construction would avoid even
//! that, but it would need every state the engine can *synthesise* at
//! generation time (`[snowy=true]`, leaf `distance=N`, `waterlogged=`), and a
//! missed one has no correct fallback.
//!
//! Growable is both simpler and sufficient, because **the interner is owned by
//! the `OverworldGenerator` and outlives every column it serves.** The
//! allocation budget is written against a *steady-state* column — the C_ss
//! bench generates chunk `(5, 5)` after a warmup sweep — by which point every
//! state the data can produce is already interned and `id_of` is a pure lookup.
//! Cold-start interning is O(distinct states), a few hundred, once per
//! generator; steady state is 0.
//!
//! # Do not call [`StateInterner::name_of`] from a hot loop
//!
//! Reads take an `RwLock` read guard. That is far cheaper than the `String`
//! allocation it replaces, but it is a *shared* cache line, and this repo has a
//! measured scar for exactly that shape: `4307b59`'s revert message records
//! cache contention across **289 concurrent generator calls**. The ported hot
//! loops (fill, carve, ore, stitch, top-layer) traffic purely in [`StateId`]
//! and call neither `name_of` nor `id_of`; the remaining callers are the
//! not-yet-ported vegetation engine's shims (Unit 8) and the O(palette) exit at
//! `into_palette_and_blocks`. `crate::counters::bump_state_name_lookup` counts
//! them so a future regression into a per-block `name_of` is visible as a
//! counter delta rather than an unexplained slowdown.
//!
//! Leaking is what buys the lock-free *lifetime*: `name_of` copies a
//! `&'static str` out of the guard and drops it, which a `Vec<Box<str>>` could
//! not do without `unsafe` (denied workspace-wide). The leak is bounded by the
//! number of distinct block states per generator (a few hundred), not by
//! columns served.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

/// A numeric handle for a canonical block-state string, valid only against the
/// [`StateInterner`] that issued it.
///
/// Mixing ids from two different interners is a silent wrong-value bug of
/// exactly the class `CLAUDE.md` warns about (a fully-connected wire carrying
/// the wrong value), so the types that store ids carry the issuing interner's
/// [`StateInterner::instance_id`] and `debug_assert!` on it at the seams where
/// two id-carrying containers meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StateId(u16);

impl StateId {
    /// `"minecraft:air"`. Guaranteed to be id `0` in every [`StateInterner`],
    /// because [`StateInterner::new`] interns it first — the same guarantee
    /// `crate::overworld::GeneratedColumn` already documents for palette index
    /// 0, and which its `blocks[idx] != 0` non-air tests depend on.
    pub const AIR: Self = Self(0);

    /// The raw index, for use as a table subscript.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The raw `u16`, for packing into a grid cell.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// Rebuilds an id from a raw `u16` previously obtained from
    /// [`StateId::raw`] against the same interner.
    #[must_use]
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }
}

/// Interner state behind the lock. Split out so the recursive base-name intern
/// can run under a single write guard (see [`intern_locked`]).
#[derive(Debug, Default)]
struct Table {
    /// `id -> canonical state string`. Leaked so [`StateInterner::name_of`] can
    /// return `&'static str` out of a dropped read guard (see module doc).
    names: Vec<&'static str>,
    /// `canonical state string -> id`.
    ids: HashMap<&'static str, u16>,
    /// `id -> id of that state's base name` (the part before `[`). Self-
    /// referential for a state that has no properties, so this is always a
    /// valid index and never an `Option`.
    base_of: Vec<u16>,
}

/// Interns block-state strings to [`StateId`]s for one generator.
///
/// Read the module doc before changing this: the two load-bearing properties
/// are that ids never reach the wire (so assignment order is free) and that the
/// table outlives every column (so steady-state interning is zero-allocation).
#[derive(Debug)]
pub struct StateInterner {
    table: RwLock<Table>,
    instance_id: u64,
}

impl Default for StateInterner {
    fn default() -> Self {
        Self::new()
    }
}

/// Interns `s`, and its base name first, into an already-write-locked table.
///
/// Recursion depth is at most 2 (a base name contains no `[`, so its own base
/// is itself), and it must happen under the *same* guard as the caller's —
/// taking the write lock again here would deadlock.
fn intern_locked(table: &mut Table, s: &str) -> u16 {
    if let Some(&id) = table.ids.get(s) {
        return id;
    }
    // Resolve the base name first, so `base_of` is populated for every id at
    // the moment that id becomes visible.
    let base = s.split('[').next().unwrap_or(s);
    let base_id = if base == s {
        None
    } else {
        Some(intern_locked(table, base))
    };

    // The only allocation in this module, and the reason cold-start interning
    // shows in the allocation counter while steady state does not.
    crate::counters::bump_state_intern_new();
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    let id = u16::try_from(table.names.len()).expect("more than 65,536 distinct block states in one generator");
    table.names.push(leaked);
    table.ids.insert(leaked, id);
    // A property-less state is its own base.
    table.base_of.push(base_id.unwrap_or(id));
    id
}

impl StateInterner {
    /// A table pre-seeded so that [`StateId::AIR`] is id `0`.
    #[must_use]
    pub fn new() -> Self {
        static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(0);
        let this = Self {
            table: RwLock::new(Table::default()),
            instance_id: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
        };
        let air = this.id_of("minecraft:air");
        debug_assert_eq!(air, StateId::AIR, "air must intern to id 0");
        this
    }

    /// Distinguishes two interners, so a container of [`StateId`]s can
    /// `debug_assert!` that ids handed to it came from the interner it was
    /// built against. Cheap enough to check once per bulk operation; never
    /// check it per cell.
    #[must_use]
    pub fn instance_id(&self) -> u64 {
        self.instance_id
    }

    /// The id for `state`, interning it if this is the first time this
    /// interner has seen it.
    ///
    /// Allocates only on a miss. See the module doc on why that reaches zero in
    /// steady state, and why this must not be called per block.
    #[must_use]
    pub fn id_of(&self, state: &str) -> StateId {
        // Fast path: a read guard and a hash lookup, no allocation.
        if let Some(&id) = self
            .table
            .read()
            .expect("state interner lock poisoned")
            .ids
            .get(state)
        {
            return StateId(id);
        }
        let mut table = self.table.write().expect("state interner lock poisoned");
        StateId(intern_locked(&mut table, state))
    }

    /// The canonical state string for `id`.
    ///
    /// # Panics
    ///
    /// If `id` was not issued by this interner — which is a programming error,
    /// not an input error (see [`StateId`]).
    #[must_use]
    pub fn name_of(&self, id: StateId) -> &'static str {
        crate::counters::bump_state_name_lookup();
        self.table.read().expect("state interner lock poisoned").names[id.index()]
    }

    /// The id of `id`'s base name — `"minecraft:oak_log[axis=y]"` maps to the
    /// id of `"minecraft:oak_log"`, and a property-less state maps to itself.
    ///
    /// This is the table that replaces the crate's five separate `split('[')`
    /// helpers (`carver::base_name`, `feature::top_layer::base_id`,
    /// `feature::vegetation::base_id`, `surface::is_fluid`'s inline strip, and
    /// `feature::try_place_ore`'s inline strip-and-allocate). A base name
    /// cannot be recovered from a `u16` without it.
    #[must_use]
    pub fn base_of(&self, id: StateId) -> StateId {
        StateId(self.table.read().expect("state interner lock poisoned").base_of[id.index()])
    }

    /// The number of distinct states interned so far — for a test asserting
    /// that a steady-state column interns nothing new.
    #[must_use]
    pub fn len(&self) -> usize {
        self.table.read().expect("state interner lock poisoned").names.len()
    }

    /// Whether nothing has been interned. Never true after [`Self::new`],
    /// which seeds air; present because `clippy::len_without_is_empty` asks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_is_always_id_zero() {
        let interner = StateInterner::new();
        assert_eq!(interner.id_of("minecraft:air"), StateId::AIR);
        assert_eq!(interner.name_of(StateId::AIR), "minecraft:air");
    }

    #[test]
    fn interning_is_idempotent_and_round_trips() {
        let interner = StateInterner::new();
        let a = interner.id_of("minecraft:stone");
        let b = interner.id_of("minecraft:stone");
        assert_eq!(a, b, "the same string must intern to the same id");
        assert_eq!(interner.name_of(a), "minecraft:stone");
    }

    #[test]
    fn distinct_states_get_distinct_ids() {
        let interner = StateInterner::new();
        let stone = interner.id_of("minecraft:stone");
        let dirt = interner.id_of("minecraft:dirt");
        assert_ne!(stone, dirt);
    }

    #[test]
    fn base_of_strips_properties_and_is_self_for_a_bare_name() {
        let interner = StateInterner::new();
        let with_props = interner.id_of("minecraft:oak_log[axis=y]");
        let bare = interner.id_of("minecraft:oak_log");
        assert_eq!(interner.base_of(with_props), bare);
        assert_eq!(interner.base_of(bare), bare, "a bare name is its own base");
        assert_eq!(interner.name_of(interner.base_of(with_props)), "minecraft:oak_log");
    }

    #[test]
    fn base_name_is_interned_even_when_only_a_propertied_state_is_asked_for() {
        // `intern_locked` resolves the base first specifically so `base_of` is
        // never a dangling index; this is the case that would catch it.
        let interner = StateInterner::new();
        let leaves = interner.id_of("minecraft:oak_leaves[distance=3,waterlogged=false]");
        assert_eq!(interner.name_of(interner.base_of(leaves)), "minecraft:oak_leaves");
    }

    #[test]
    fn a_steady_state_lookup_interns_nothing_new() {
        // The property the allocation budget rests on: once warm, `id_of` is a
        // pure lookup. `len()` is the observable that would move if it were not.
        let interner = StateInterner::new();
        for state in ["minecraft:stone", "minecraft:dirt", "minecraft:oak_log[axis=y]"] {
            let _ = interner.id_of(state);
        }
        let warm = interner.len();
        for _ in 0..1000 {
            for state in ["minecraft:stone", "minecraft:dirt", "minecraft:oak_log[axis=y]"] {
                let _ = interner.id_of(state);
            }
        }
        assert_eq!(interner.len(), warm, "a warm lookup must not intern");
    }

    #[test]
    fn two_interners_have_different_instance_ids() {
        let a = StateInterner::new();
        let b = StateInterner::new();
        assert_ne!(a.instance_id(), b.instance_id());
    }
}
