//! The point interpreter's per-`(node, x, z)` memo for provably xz-pure
//! subtrees — vanilla's `NoiseChunk.FlatCache`, in the one evaluator that had no
//! equivalent.
//!
//! ## What it is
//!
//! [`super::Density::compute`] is a recursive descent with no memo of any kind, so
//! a subtree reached twice is evaluated twice. DESIGN.md §12.134 measured the
//! consequence at **326,514** `noise_scaled` calls per interior column against
//! **68,286** distinct `(octave, coordinate)` tuples — a 4.87× ratio that the
//! `Op`-table node-sharing pass structurally could not collect, because sharing a
//! node does not stop it being *evaluated* twice.
//!
//! This is a fixed-size, direct-mapped, thread-local cache keyed by
//! `(node id, x, z)`. `flat_cache` and `cache_2d` nodes whose subtree is
//! **statically proved not to read `ctx.y`** (see
//! [`super::Density::is_xz_pure`]) carry a [`XzMemoId`] and consult it; every
//! other node is untouched.
//!
//! ## Why a map and not vanilla's one slot — measured, not assumed
//!
//! §12.132 deleted a one-slot last-`(x, z)` memo from `cache_2d` at a **0.12%**
//! hit rate, which is a true measurement of the wrong structure. Instrumenting
//! *three* hypothetical memos simultaneously over 100 interior columns
//! (`crates/lodestone-worldgen/tests/density_redundancy_probe.rs`) separates them:
//!
//! | memo | point-interpreter hit rate |
//! |---|---|
//! | one slot, last `(x, z)` per node (vanilla's `Cache2D`) | **2.1%** |
//! | full `(node, x, z)` map | **78.2%** |
//! | full `(node, x, y, z)` map | 0.9% |
//!
//! The reason the one-slot form cannot work here is the *fetch order*: the field
//! evaluator fills a cell by fetching eight corners as
//! `(x0,z0) (x1,z0) (x0,z0) (x1,z0) (x0,z1) (x1,z1) (x0,z1) (x1,z1)`, so
//! consecutive visits to one node alternate between four `(x, z)` pairs and a
//! single slot is evicted before it is ever read. Vanilla does not have this
//! problem because vanilla's `FlatCache` is a **chunk-wide `double[]` over the
//! quart grid**, not a slot — the one-slot structure is `Cache2D`'s, and vanilla
//! reaches these subtrees through `FlatCache`. So the map is the faithful shape,
//! not an invention.
//!
//! ## Why it cannot change a value
//!
//! Two independent reasons, and the first is structural rather than a property of
//! 26.2's data:
//!
//! 1. A node only carries an id if [`super::Density::is_xz_pure`] proves its whole
//!    subtree never reads `ctx.y` — no `y_clamped_gradient`, no `shift`, no
//!    `old_blended_noise`, no `find_top_surface`, no `noise`, and
//!    `shifted_noise` only with `y_scale == 0.0` **and** a `shift_y` that is a
//!    non-negative-zero constant (so `f64::from(y) * 0.0 + s` is bit-identical for
//!    every `y`, `-0.0` included). Anything unproved is simply not memoised. A
//!    datapack cannot defeat this, because the analysis runs on the built tree.
//! 2. Ids come from a process-wide monotonic counter, never reused, so an entry
//!    can only ever be returned for the node that wrote it. That is why the cache
//!    needs no clearing, no epoch and no lifetime coupling to a `Graph` — a
//!    pointer-keyed version would have had to reason about address reuse after a
//!    tree is dropped.
//!
//! ## How to change it
//!
//! * **`LOG2_LEN` is a locality trade, so re-measure rather than reason.** The
//!   cache is direct-mapped: a bigger table conflict-misses less and evicts more
//!   of everything else. §12.132's whole finding was that per-worker cache
//!   footprint, not locking, is what caps generation parallelism at 2.6×, and this
//!   table is paid for by every worker.
//! * **Do not add a `y` to the key.** The measured `(node, x, y, z)` hit rate in
//!   this evaluator is 0.9%; it would pay the lookup and return nothing.
//! * A hit skips the whole subtree, which is safe *here* only because the point
//!   interpreter has no side effects — unlike `engine::field`, where a skipped
//!   subtree can contain a cache-slot write that a later query depends on.
//!
//! ## Configuration
//!
//! None at runtime. The hit/miss counters are behind `gen-counters`, so a clean
//! release build carries the cache and not the instrument.
//!
//! ## Dependencies
//!
//! None outside this module.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};

/// A memoisable node's identity: a process-wide unique, never-reused id, or
/// [`XzMemoId::NONE`] for a node the purity analysis declined.
///
/// `Copy`, so cloning a [`super::Density`] tree keeps the ids — which is correct,
/// not a leak: a clone denotes the same function, so it may read the original's
/// memo entries.
///
/// Deliberately **excluded from [`super::Density::write_signature`]**, exactly as
/// `slot` is: an id is an index into an evaluator's memo, not part of the function
/// the node denotes, so two structurally identical nodes with different ids must
/// still share one compiled node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct XzMemoId(u32);

/// Hands out ids. Starts at 1 so `0` can mean "not memoised" and the cache's
/// empty entries need no separate presence bit.
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

impl XzMemoId {
    /// The id of a node that must not be memoised.
    pub const NONE: Self = Self(0);

    /// Allocates a fresh id.
    ///
    /// Saturates rather than wrapping: after `u32::MAX` allocations every further
    /// node is simply un-memoised, which costs speed and cannot cost correctness.
    /// Wrapping would reuse an id and return another node's value.
    #[must_use]
    pub fn allocate() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        if id == u32::MAX {
            NEXT_ID.store(u32::MAX, Ordering::Relaxed);
            Self::NONE
        } else {
            Self(id)
        }
    }

    /// Whether this node participates in the memo.
    #[must_use]
    pub fn is_some(self) -> bool {
        self.0 != 0
    }
}

/// `log2` of the table length.
///
/// Chosen by measurement, alternated against a clean arm on the 12×12 sweep
/// (I_ss, median of the 100 interior columns; the before arm is 485.08 M):
///
/// | `LOG2_LEN` | entries | bytes/thread | I_ss | vs before |
/// |---|---|---|---|---|
/// | 10 | 1,024 | 24 KB | 434.77 M | −10.38% |
/// | **12** | **4,096** | **98 KB** | **430.65 M** | **−11.22%** |
/// | 14 | 16,384 | 393 KB | 426.46 M | −11.88% |
///
/// 12 rather than 14 because the last 0.66% costs **4× the per-worker footprint**,
/// and §12.132 measured per-worker cache footprint — not locking — as the thing
/// that caps generation parallelism at 2.6× on this machine. The serial gain is
/// known and small; the parallel cost is unmeasured, and the 289-column join burst
/// is where it would show up. Raise it only with that burst measured, not on the
/// serial number alone.
const LOG2_LEN: u32 = 12;
const LEN: usize = 1 << LOG2_LEN;
const MASK: usize = LEN - 1;

#[derive(Clone, Copy)]
struct Entry {
    id: u32,
    x: i32,
    z: i32,
    value: f64,
}

const EMPTY: Entry = Entry {
    id: 0,
    x: 0,
    z: 0,
    value: 0.0,
};

struct Table {
    entries: Box<[Entry]>,
    #[cfg(feature = "gen-counters")]
    hits: u64,
    #[cfg(feature = "gen-counters")]
    misses: u64,
}

thread_local! {
    static TABLE: RefCell<Table> = RefCell::new(Table {
        entries: vec![EMPTY; LEN].into_boxed_slice(),
        #[cfg(feature = "gen-counters")]
        hits: 0,
        #[cfg(feature = "gen-counters")]
        misses: 0,
    });
}

#[inline]
fn index(id: u32, x: i32, z: i32) -> usize {
    // A 64-bit multiply-xor fold. The three fields are packed before mixing so a
    // change in any of them moves the high bits; the index is taken from the top,
    // where the multiply has spread the entropy.
    let packed = (u64::from(x as u32) << 32) | u64::from(z as u32);
    let mut h = packed ^ u64::from(id).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    ((h >> 33) as usize) & MASK
}

/// Reads `(id, x, z)`, or `None` on a miss or a conflicting entry.
#[inline]
pub(crate) fn get(id: XzMemoId, x: i32, z: i32) -> Option<f64> {
    TABLE.with(|t| {
        let t = &mut *t.borrow_mut();
        let e = t.entries[index(id.0, x, z)];
        if e.id == id.0 && e.x == x && e.z == z {
            #[cfg(feature = "gen-counters")]
            {
                t.hits += 1;
            }
            Some(e.value)
        } else {
            #[cfg(feature = "gen-counters")]
            {
                t.misses += 1;
            }
            None
        }
    })
}

/// Stores `(id, x, z) -> value`, evicting whatever shared the slot.
#[inline]
pub(crate) fn put(id: XzMemoId, x: i32, z: i32, value: f64) {
    TABLE.with(|t| {
        let t = &mut *t.borrow_mut();
        let i = index(id.0, x, z);
        t.entries[i] = Entry { id: id.0, x, z, value };
    });
}

/// `(hits, misses)` on this thread since the last [`reset_stats`]. Always
/// `(0, 0)` without `gen-counters`.
#[must_use]
pub fn stats() -> (u64, u64) {
    #[cfg(feature = "gen-counters")]
    {
        TABLE.with(|t| {
            let t = t.borrow();
            (t.hits, t.misses)
        })
    }
    #[cfg(not(feature = "gen-counters"))]
    {
        (0, 0)
    }
}

/// Zeroes this thread's hit/miss counters. Does **not** clear the table — the
/// table never needs clearing (module doc, reason 2), and clearing it here would
/// make a per-column measurement report a cold cache every column.
pub fn reset_stats() {
    #[cfg(feature = "gen-counters")]
    TABLE.with(|t| {
        let t = &mut *t.borrow_mut();
        t.hits = 0;
        t.misses = 0;
    });
}

/// Empties the table. Only for tests that need a cold cache; production never
/// calls it.
pub fn clear() {
    TABLE.with(|t| {
        let t = &mut *t.borrow_mut();
        t.entries.fill(EMPTY);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids are never reused, which is the whole reason the table needs no
    /// clearing. A wrapping counter would return another node's value.
    #[test]
    fn ids_are_distinct_and_none_is_falsy() {
        let a = XzMemoId::allocate();
        let b = XzMemoId::allocate();
        assert_ne!(a, b);
        assert!(a.is_some() && b.is_some());
        assert!(!XzMemoId::NONE.is_some());
    }

    /// A stored value is returned for its own key and for no other — the
    /// direct-mapped table must compare the full key, not only the index.
    #[test]
    fn a_hit_needs_the_whole_key() {
        clear();
        let id = XzMemoId::allocate();
        let other = XzMemoId::allocate();
        put(id, 4, -8, 1.25);
        assert_eq!(get(id, 4, -8), Some(1.25));
        assert_eq!(get(id, 4, -7), None, "a different z must miss");
        assert_eq!(get(id, 5, -8), None, "a different x must miss");
        assert_eq!(get(other, 4, -8), None, "a different node must miss");
    }
}
