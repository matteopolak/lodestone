//! A measurement-only probe that sizes the **evaluation redundancy** in both
//! density evaluators, without changing a single value.
//!
//! ## What it is
//!
//! DESIGN.md §12.134 measured `ImprovedNoise::noise_scaled` at **326,514** calls
//! per interior column against **68,286** distinct `(octave, coordinate)` tuples
//! — a 4.87× redundancy ratio — and established that the node-sharing pass over
//! the `Op` table could not collect it, because neither evaluator has a per-node
//! memo. This probe answers the question that has to be answered *before* a memo
//! is designed: **which memo, keyed how, in which evaluator, would actually hit,
//! and how often.**
//!
//! It counts, per node kind, for every node visit in a measurement window:
//!
//! | quantity | the memo it predicts |
//! |---|---|
//! | `visits` | nothing — the denominator |
//! | `xz_single_hits` | vanilla's `NoiseChunk.Cache2D`: **one slot per node**, last `(x, z)` |
//! | `xz_map_hits` | a full `(node, x, z)` map, window-scoped |
//! | `xyz_map_hits` | a full `(node, x, y, z)` map — the whole CSE prize |
//!
//! The three are strictly ordered (`xz_single ≤ xz_map ≤`… not quite: an
//! `xyz` hit implies an `xz` hit, so `xz_map_hits ≥ xyz_map_hits`), and the gap
//! between them is the whole design question: if `xz_single_hits` is already
//! large, vanilla's one-slot structure is enough and no map is needed.
//!
//! ## How it works
//!
//! Both evaluators call [`visit_point`] / [`visit_field`] next to their existing
//! `crate::counters` hook. A node is identified by its address (the point
//! interpreter walks `Density` nodes that live in an immutable, never-reallocated
//! `Graph::leaves`) or by its `NodeId` (the field evaluator). Nothing is recorded
//! unless [`enable`] has been called on this thread, and the whole module is
//! behind `gen-counters` — so a production build has neither the maps nor the
//! branch.
//!
//! ## How to change it
//!
//! The window is whatever the caller brackets with [`reset`] and [`take`]. A
//! per-column window is the honest scope for a chunk-scoped memo; a wider window
//! reports a hit rate no per-chunk cache could deliver, which is the one way to
//! read this instrument wrong.
//!
//! ## Configuration
//!
//! `gen-counters`. Off by default even then: [`enable`] is per-thread.
//!
//! ## Dependencies
//!
//! `crate::density::Density` for `KIND_COUNT`. Nothing else.

use crate::density::Density;

const KINDS: usize = Density::KIND_COUNT;

/// One window's worth of redundancy counts.
#[derive(Clone, Debug)]
pub struct Redundancy {
    /// Point-interpreter (`Density::compute`) visits by kind index.
    pub point_visits: [u64; KINDS],
    /// Point visits whose node's *own previous* `(x, z)` matched.
    pub point_xz_single_hits: [u64; KINDS],
    /// Point visits whose `(node, x, z)` was seen earlier in the window.
    pub point_xz_map_hits: [u64; KINDS],
    /// Point visits whose `(node, x, y, z)` was seen earlier in the window.
    pub point_xyz_map_hits: [u64; KINDS],
    /// Field-evaluator (`Field::eval`) visits by kind index.
    pub field_visits: [u64; KINDS],
    /// Field visits whose node's own previous `(x, z)` matched.
    pub field_xz_single_hits: [u64; KINDS],
    /// Field visits whose `(node, x, z)` was seen earlier in the window.
    pub field_xz_map_hits: [u64; KINDS],
    /// Field visits whose `(node, x, y, z)` was seen earlier in the window.
    pub field_xyz_map_hits: [u64; KINDS],
}

impl Default for Redundancy {
    fn default() -> Self {
        Self {
            point_visits: [0; KINDS],
            point_xz_single_hits: [0; KINDS],
            point_xz_map_hits: [0; KINDS],
            point_xyz_map_hits: [0; KINDS],
            field_visits: [0; KINDS],
            field_xz_single_hits: [0; KINDS],
            field_xz_map_hits: [0; KINDS],
            field_xyz_map_hits: [0; KINDS],
        }
    }
}

impl Redundancy {
    /// Sums another window into this one.
    pub fn accumulate(&mut self, other: &Redundancy) {
        for i in 0..KINDS {
            self.point_visits[i] += other.point_visits[i];
            self.point_xz_single_hits[i] += other.point_xz_single_hits[i];
            self.point_xz_map_hits[i] += other.point_xz_map_hits[i];
            self.point_xyz_map_hits[i] += other.point_xyz_map_hits[i];
            self.field_visits[i] += other.field_visits[i];
            self.field_xz_single_hits[i] += other.field_xz_single_hits[i];
            self.field_xz_map_hits[i] += other.field_xz_map_hits[i];
            self.field_xyz_map_hits[i] += other.field_xyz_map_hits[i];
        }
    }

    /// Total point-interpreter visits.
    #[must_use]
    pub fn point_total(&self) -> u64 {
        self.point_visits.iter().sum()
    }

    /// Total field-evaluator visits.
    #[must_use]
    pub fn field_total(&self) -> u64 {
        self.field_visits.iter().sum()
    }
}

#[cfg(feature = "gen-counters")]
mod live {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};

    use super::Redundancy;

    struct State {
        on: bool,
        counts: Redundancy,
        last_xz: HashMap<u64, (i32, i32)>,
        seen_xz: HashSet<(u64, i32, i32)>,
        seen_xyz: HashSet<(u64, i32, i32, i32)>,
    }

    impl State {
        fn new() -> Self {
            Self {
                on: false,
                counts: Redundancy::default(),
                last_xz: HashMap::new(),
                seen_xz: HashSet::new(),
                seen_xyz: HashSet::new(),
            }
        }
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::new());
    }

    /// Which evaluator a visit came from — the high bit of the node key, so the
    /// two never share a map entry.
    const FIELD_TAG: u64 = 1 << 63;

    fn record(node: u64, kind: usize, x: i32, y: i32, z: i32, field: bool) {
        STATE.with(|s| {
            let s = &mut *s.borrow_mut();
            if !s.on {
                return;
            }
            let c = &mut s.counts;
            let (visits, single, xz, xyz) = if field {
                (
                    &mut c.field_visits,
                    &mut c.field_xz_single_hits,
                    &mut c.field_xz_map_hits,
                    &mut c.field_xyz_map_hits,
                )
            } else {
                (
                    &mut c.point_visits,
                    &mut c.point_xz_single_hits,
                    &mut c.point_xz_map_hits,
                    &mut c.point_xyz_map_hits,
                )
            };
            if kind >= visits.len() {
                return;
            }
            visits[kind] += 1;
            match s.last_xz.insert(node, (x, z)) {
                Some(prev) if prev == (x, z) => single[kind] += 1,
                _ => {}
            }
            if !s.seen_xz.insert((node, x, z)) {
                xz[kind] += 1;
            }
            if !s.seen_xyz.insert((node, x, y, z)) {
                xyz[kind] += 1;
            }
        });
    }

    /// Records one point-interpreter node visit.
    pub fn visit_point(node: *const (), kind: usize, x: i32, y: i32, z: i32) {
        record(node as u64 & !FIELD_TAG, kind, x, y, z, false);
    }

    /// Records one field-evaluator node visit.
    ///
    /// A `NodeId` is only unique **within one `Graph`**, and a column evaluates
    /// several (`final_density`, `depth`, `erosion`, the climate channels), so the
    /// graph's own address is folded in. Keying on the id alone would merge
    /// unrelated nodes and over-report every hit rate.
    pub fn visit_field(graph: *const (), node: u32, kind: usize, x: i32, y: i32, z: i32) {
        let key = (graph as u64).rotate_left(17) ^ u64::from(node);
        record(key | FIELD_TAG, kind, x, y, z, true);
    }

    /// Starts recording on this thread.
    pub fn enable() {
        STATE.with(|s| s.borrow_mut().on = true);
    }

    /// Stops recording on this thread.
    pub fn disable() {
        STATE.with(|s| s.borrow_mut().on = false);
    }

    /// Clears the window (counts *and* the seen-sets).
    pub fn reset() {
        STATE.with(|s| {
            let s = &mut *s.borrow_mut();
            s.counts = Redundancy::default();
            s.last_xz.clear();
            s.seen_xz.clear();
            s.seen_xyz.clear();
        });
    }

    /// Reads the window without clearing it.
    pub fn snapshot() -> Redundancy {
        STATE.with(|s| s.borrow().counts.clone())
    }
}

#[cfg(not(feature = "gen-counters"))]
mod live {
    use super::Redundancy;

    #[inline(always)]
    pub fn visit_point(_node: *const (), _kind: usize, _x: i32, _y: i32, _z: i32) {}
    #[inline(always)]
    pub fn visit_field(_graph: *const (), _node: u32, _kind: usize, _x: i32, _y: i32, _z: i32) {}
    pub fn enable() {}
    pub fn disable() {}
    pub fn reset() {}
    #[must_use]
    pub fn snapshot() -> Redundancy {
        Redundancy::default()
    }
}

pub use live::{disable, enable, reset, snapshot, visit_field, visit_point};
