//! The per-source-chunk biome memo — the other half of
//! `docs/plans/worldgen-rewrite.md`'s D5.
//!
//! # The defect this exists for
//!
//! `carve_stage` resolves a carver biome for every chunk in a **17×17 = 289**
//! source neighbourhood (`carver::NEIGHBOURHOOD_RANGE = 8`), once per pre-ore
//! chunk, and `ore_stage` does the same for its own 3×3. Two adjacent chunks'
//! 289-chunk windows overlap in 272 of 289 positions, and the answer for a source
//! chunk is a **pure function of `(generator, cx, cz)`** — the sample is taken at
//! that chunk's own quart corner and `y = 0`, nothing about the requesting centre
//! enters it. So all but the newly-entered strip of every window is a repeat of
//! work already done, and before this module none of it was reused: the plan
//! measured ~2.2M squared-distance comparisons per pre-ore chunk.
//!
//! # Why thread-local and direct-mapped, rather than in the staged store
//!
//! The plan's U9 row says "memoised per-source biome in the store", and
//! `docs/worldgen-staged-store.md`'s "How to change it" invites a fourth
//! `StageSlot`. **Measured against the store's own derivation, that is the wrong
//! home**, and the store's doc is what says why: `COLUMN_CLOSURE_RADIUS` and
//! `STORE_RETENTION` are *derived* from the drivers, and a carver-source lookup
//! reaches radius 8 beyond a pre-ore chunk. One `column()` closes over pre-ore
//! radius 2, so its carver-source closure is radius **10** — a 21×21 = 441-chunk
//! pin becomes 41×41 = 1,681, and the 289-column burst's 441-chunk working set
//! becomes 37×37 = 1,369. `STORE_RETENTION = 512` is *derived from 441*; a stage
//! with a 1,369-chunk closure either forces retention past 1,369 — quadrupling the
//! worst-case live grid memory, since an entry retains its pre-ore and post-ore
//! grids too — or it evicts inside a live request, which is the exact property
//! that doc calls "structurally ineligible" and gates at zero. Neither is
//! acceptable for a memo whose value is four bytes and whose recomputation is one
//! tree search.
//!
//! So the memo is its own structure, and being separate it can be shaped for its
//! own access pattern:
//!
//! * **Thread-local, so there is no lock at all.** The value is a pure function
//!   of the key, so per-thread copies cannot disagree, and CLAUDE.md/the store
//!   doc are both explicit that buffer reuse here belongs in a `thread_local`
//!   rather than behind a shared lock — a shared map would re-create, one layer
//!   down, exactly the contention U6 deleted.
//! * **Direct-mapped on the low 5 bits of each chunk coordinate**, so a slot is
//!   `((cz & 31) << 5) | (cx & 31)`. That is not a hash: any 17×17 window fits
//!   inside a 32×32 residue block, so **a single carve stage's 289 lookups are
//!   collision-free by construction**, not by probability. Sweeping past 32
//!   chunks wraps and displaces the oldest residues, which is the eviction policy
//!   this wants anyway.
//! * **Tagged with the full key.** A slot stores `(table_id, cx, cz)` and is only
//!   a hit on an exact match, so a displaced residue is a miss, never a wrong
//!   biome. This is the store doc's "exact keys only" rule, and the reason it has
//!   that rule is on the record in this crate: a *clamped*-key cache once aliased
//!   two chunk coordinates and hung a JVM oracle.
//! * **Keyed by table identity, not just position.** Tests build several
//!   generators on one thread; without `table_id` in the tag, generator B would
//!   read generator A's biomes. See [`super::BiomeTable::id`].
//!
//! Because a miss only ever costs a recomputation of a pure function, a memo hit
//! rate cannot change generated terrain — which is what makes this the one half
//! of U9 with no parity risk at all. The half that *does* carry parity risk is
//! [`super::tree`].

use std::cell::RefCell;

/// Chunk-coordinate bits used to index a slot. 5 bits ⇒ a 32×32 residue block,
/// which strictly contains any 17×17 carver-source window
/// (`carver::NEIGHBOURHOOD_RANGE = 8` ⇒ 17 wide), so one carve stage never
/// self-collides. Raising this cannot improve that property; lowering it to 4
/// (16×16) would break it, since 17 > 16.
const COORD_BITS: i32 = 5;

/// 1,024 slots — `1 << (2 * COORD_BITS)`.
const SLOTS: usize = 1 << (2 * COORD_BITS as usize);

/// The residue period the slot map folds chunk coordinates into: 32.
const PERIOD: i32 = 1 << COORD_BITS;

/// One carve stage's source window width, **derived from the carver** rather than
/// written down here: `carver::apply_carvers` walks `dx, dz ∈
/// [-NEIGHBOURHOOD_RANGE, NEIGHBOURHOOD_RANGE]`.
const SOURCE_WINDOW: i32 = 2 * crate::carver::NEIGHBOURHOOD_RANGE + 1;

/// **The collision-freedom property, as a build failure rather than a test.**
///
/// The whole reason a direct-mapped table is acceptable here is that one carve
/// stage's window fits inside a single residue block, so its 289 lookups cannot
/// evict each other. That holds only while `SOURCE_WINDOW <= PERIOD`, and it is a
/// relationship between a constant in *this* module and one in `carver`. A unit
/// test asserting it would have to name a width, and naming 17 is precisely how a
/// widened carver neighbourhood would slip through with every test green — the
/// geometry, not the assertion, is where that class of vacuity lives. So it is
/// checked here, against the real constant, at compile time.
const _: () = assert!(
    SOURCE_WINDOW <= PERIOD,
    "biome::memo's slot map folds chunk coordinates modulo 2^COORD_BITS, so a carve \
     stage's source window must fit inside one residue block or its lookups evict each \
     other. Raise COORD_BITS."
);

/// `table_id` value that can never be a real id, so a zeroed slot is a miss.
/// [`super::next_table_id`] starts at 1.
const EMPTY: u64 = 0;

/// One memoised answer. 24 bytes, so the whole table is 24 KiB per thread.
#[derive(Debug, Clone, Copy)]
struct Slot {
    table_id: u64,
    cx: i32,
    cz: i32,
    row: u32,
}

thread_local! {
    /// Boxed so the 24 KiB lives on the heap rather than in the thread's TLS
    /// block, and allocated once per thread on first use.
    static SLOTS_TLS: RefCell<Box<[Slot; SLOTS]>> = RefCell::new(Box::new(
        [Slot { table_id: EMPTY, cx: 0, cz: 0, row: 0 }; SLOTS],
    ));
}

#[inline]
fn slot_of(cx: i32, cz: i32) -> usize {
    let mask = (1i32 << COORD_BITS) - 1;
    (((cz & mask) << COORD_BITS) | (cx & mask)) as usize
}

/// The memoised table row for one source chunk, computing it with `compute` on a
/// miss.
///
/// `compute` must be a pure function of `(table_id, cx, cz)`. It is: the only
/// caller samples climate at `(cx * 16, 0, cz * 16)` through the generator's own
/// fixed `ClimateSampler` and searches the generator's own fixed table.
#[inline]
pub(crate) fn source_row(table_id: u64, cx: i32, cz: i32, compute: impl FnOnce() -> u32) -> u32 {
    debug_assert_ne!(table_id, EMPTY, "table ids start at 1");
    let slot = slot_of(cx, cz);
    // The borrow is released before `compute` runs: `compute` re-enters this
    // module for no other key, but a `RefCell` held across a closure call is a
    // panic waiting for the first caller who does.
    let hit = SLOTS_TLS.with(|cache| {
        let cache = cache.borrow();
        let entry = cache[slot];
        (entry.table_id == table_id && entry.cx == cx && entry.cz == cz).then_some(entry.row)
    });
    if let Some(row) = hit {
        return row;
    }
    let row = compute();
    SLOTS_TLS.with(|cache| {
        cache.borrow_mut()[slot] = Slot {
            table_id,
            cx,
            cz,
            row,
        };
    });
    row
}

/// Drops every memoised entry on the calling thread. Test-only: production never
/// invalidates, because an entry is a pure function of its tagged key and a
/// generator's table id is unique for its lifetime.
#[cfg(test)]
pub(crate) fn clear_for_tests() {
    SLOTS_TLS.with(|cache| {
        for slot in cache.borrow_mut().iter_mut() {
            slot.table_id = EMPTY;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole design rests on: within one carver-source window, no
    /// two positions share a slot. Checked over every window origin in more than a
    /// full residue period rather than at one origin, since the claim is about all
    /// windows.
    ///
    /// The window size is read from [`crate::carver::NEIGHBOURHOOD_RANGE`], not
    /// written as 8 — a test that names its own geometry cannot notice the
    /// production geometry changing under it.
    #[test]
    fn a_carver_source_window_never_self_collides() {
        let range = crate::carver::NEIGHBOURHOOD_RANGE;
        let expected = (SOURCE_WINDOW * SOURCE_WINDOW) as usize;
        assert_eq!(expected, 289, "the shipped carver window is 17x17");
        for origin_x in -(PERIOD + 8)..(PERIOD + 8) {
            for origin_z in -(PERIOD + 8)..(PERIOD + 8) {
                let mut seen = vec![false; SLOTS];
                for dx in -range..=range {
                    for dz in -range..=range {
                        let slot = slot_of(origin_x + dx, origin_z + dz);
                        assert!(
                            !seen[slot],
                            "slot {slot} collides inside the window at ({origin_x}, {origin_z})"
                        );
                        seen[slot] = true;
                    }
                }
                assert_eq!(
                    seen.iter().filter(|s| **s).count(),
                    expected,
                    "a {SOURCE_WINDOW}x{SOURCE_WINDOW} window must occupy {expected} distinct slots"
                );
            }
        }
    }

    #[test]
    fn a_hit_returns_the_memoised_row_and_a_miss_recomputes() {
        clear_for_tests();
        let computes = std::cell::Cell::new(0u32);
        let ask = |id: u64, cx: i32, cz: i32| {
            source_row(id, cx, cz, || {
                computes.set(computes.get() + 1);
                (cx * 31 + cz) as u32
            })
        };
        assert_eq!(ask(1, 5, 7), (5 * 31 + 7) as u32);
        assert_eq!(ask(1, 5, 7), (5 * 31 + 7) as u32);
        assert_eq!(computes.get(), 1, "the second ask must be a hit");
        // Same slot, different coordinate (32 apart) — a displacement, so a miss
        // and a *correct* answer, not the neighbour's.
        assert_eq!(ask(1, 37, 7), (37 * 31 + 7) as u32);
        assert_eq!(computes.get(), 2);
        // ...and the displaced entry is now a miss rather than a stale hit.
        assert_eq!(ask(1, 5, 7), (5 * 31 + 7) as u32);
        assert_eq!(computes.get(), 3);
    }

    /// Two generators on one thread must not read each other's biomes. Without
    /// `table_id` in the tag this returns generator 1's row for generator 2.
    #[test]
    fn a_second_table_id_does_not_read_the_firsts_entries() {
        clear_for_tests();
        let first = source_row(1, 3, 4, || 11);
        let second = source_row(2, 3, 4, || 22);
        assert_eq!(first, 11);
        assert_eq!(second, 22, "table id must be part of the tag");
        // And the second write displaced the first, so re-asking recomputes
        // rather than returning 22.
        assert_eq!(source_row(1, 3, 4, || 33), 33);
    }

    /// Negative coordinates: `&` on a negative `i32` is a two's-complement bit
    /// mask, so `-1 & 31 == 31`. A `%` here would produce `-1` and index out of
    /// bounds.
    #[test]
    fn negative_chunk_coordinates_map_into_range() {
        for cx in -100..100i32 {
            for cz in -100..100i32 {
                assert!(slot_of(cx, cz) < SLOTS);
            }
        }
        assert_eq!(slot_of(-1, -1), SLOTS - 1);
    }
}
