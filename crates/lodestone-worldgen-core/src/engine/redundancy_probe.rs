//! A measurement-only probe that sizes the **evaluation redundancy** in both
//! density evaluators, without changing a single value.
//!
//! ## What it is
//!
//! It distinguishes source-tree visits, compiled field visits, and compiled point
//! visits. Compiled point repeats count only an exact graph node and full point
//! within one scalar evaluation or fixed-width batch: the scope a bounded
//! readiness table could actually observe.
//!
//! It counts, per node kind, for every node visit in a measurement window:
//!
//! | quantity | the memo it predicts |
//! |---|---|
//! | `visits` | nothing — the denominator |
//! | `xz_single_hits` | one slot per node, last `(x, z)` |
//! | `xz_map_hits` | a full `(node, x, z)` map, window-scoped |
//! | `xyz_map_hits` | a full `(node, x, y, z)` map — the whole CSE prize |
//! | `field_xz_own_hits` / `field_xyz_own_hits` | the same two maps **scoped to one sampler** |
//!
//! Field "own" counters separate repetition inside one sampler from repetition
//! across samplers. Compiled point counters deliberately avoid wider-window maps,
//! because only an evaluation-scoped exact-XYZ duplicate is actionable for a
//! readiness experiment.
//!
//! ## How it works
//!
//! The source tree calls [`visit_point`], while compiled point and field evaluators
//! carry their graph identity and `NodeId` separately. Compiled point visits also
//! carry a scalar or batch scope, so an exact-XYZ duplicate is only reported when
//! a per-evaluation table could observe it. Nothing is recorded unless [`enable`]
//! has been called on this thread, and the whole module is behind `gen-counters`
//! — so a production build has neither the maps nor the branch.
//!
//! ## How to change it
//!
//! The window is whatever the caller brackets with [`reset`] and [`snapshot`]. A
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
    /// Field visits whose `(sampler, node, x, z)` was seen earlier in the
    /// window — i.e. the part of [`Self::field_xz_map_hits`] one sampler could
    /// have answered on its own.
    pub field_xz_own_hits: [u64; KINDS],
    /// Field visits whose `(sampler, node, x, y, z)` was seen earlier in the
    /// window. The gap to [`Self::field_xyz_map_hits`] is the **cross-sampler**
    /// duplication, which is the only part a shared [`super::Scratch`] can
    /// collect.
    pub field_xyz_own_hits: [u64; KINDS],
    /// How many distinct samplers (scratches) the field evaluator was entered
    /// through in this window.
    pub field_scopes: u64,
    /// Field visits whose node's *own previous* `(x, y, z)` matched — a one-slot
    /// memo on the full position. Distinct from
    /// [`Self::field_xz_single_hits`] and the one that decides whether a
    /// duplicate pair is **adjacent** in the walk (one slot is enough) or far
    /// apart (a table is needed).
    pub field_xyz_single_hits: [u64; KINDS],
    /// The same, in the point interpreter.
    pub point_xyz_single_hits: [u64; KINDS],
    /// Compiled point-program scalar visits by kind index.
    pub compiled_point_scalar_visits: [u64; KINDS],
    /// Scalar visits that repeat one exact graph node and full position within
    /// one public point-program evaluation.
    pub compiled_point_scalar_xyz_scope_hits: [u64; KINDS],
    /// Number of scalar point-program evaluation scopes observed.
    pub compiled_point_scalar_scopes: u64,
    /// Compiled point-program batch visits by kind index.
    pub compiled_point_batch_visits: [u64; KINDS],
    /// Batch visits that repeat one exact graph node and full position within
    /// one fixed-width batch evaluation.
    pub compiled_point_batch_xyz_scope_hits: [u64; KINDS],
    /// Number of fixed-width point-program batch scopes observed.
    pub compiled_point_batch_scopes: u64,
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
            field_xz_own_hits: [0; KINDS],
            field_xyz_own_hits: [0; KINDS],
            field_scopes: 0,
            field_xyz_single_hits: [0; KINDS],
            point_xyz_single_hits: [0; KINDS],
            compiled_point_scalar_visits: [0; KINDS],
            compiled_point_scalar_xyz_scope_hits: [0; KINDS],
            compiled_point_scalar_scopes: 0,
            compiled_point_batch_visits: [0; KINDS],
            compiled_point_batch_xyz_scope_hits: [0; KINDS],
            compiled_point_batch_scopes: 0,
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
            self.field_xz_own_hits[i] += other.field_xz_own_hits[i];
            self.field_xyz_own_hits[i] += other.field_xyz_own_hits[i];
            self.field_xyz_single_hits[i] += other.field_xyz_single_hits[i];
            self.point_xyz_single_hits[i] += other.point_xyz_single_hits[i];
            self.compiled_point_scalar_visits[i] += other.compiled_point_scalar_visits[i];
            self.compiled_point_scalar_xyz_scope_hits[i] +=
                other.compiled_point_scalar_xyz_scope_hits[i];
            self.compiled_point_batch_visits[i] += other.compiled_point_batch_visits[i];
            self.compiled_point_batch_xyz_scope_hits[i] +=
                other.compiled_point_batch_xyz_scope_hits[i];
        }
        self.field_scopes += other.field_scopes;
        self.compiled_point_scalar_scopes += other.compiled_point_scalar_scopes;
        self.compiled_point_batch_scopes += other.compiled_point_batch_scopes;
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

    /// Total compiled scalar point-program visits.
    #[must_use]
    pub fn compiled_point_scalar_total(&self) -> u64 {
        self.compiled_point_scalar_visits.iter().sum()
    }

    /// Total compiled batch point-program visits.
    #[must_use]
    pub fn compiled_point_batch_total(&self) -> u64 {
        self.compiled_point_batch_visits.iter().sum()
    }
}

#[cfg(feature = "gen-counters")]
mod live {
    use std::cell::{Cell, RefCell};
    use std::collections::{HashMap, HashSet};

    use super::Redundancy;

    #[derive(Clone, Copy, Eq, Hash, PartialEq)]
    enum EvaluatorMode {
        Tree,
        FieldExact,
        FieldInterpolated,
        FieldTile,
        PointScalar,
        PointBatch,
    }

    #[derive(Clone, Copy, Eq, Hash, PartialEq)]
    struct NodeKey {
        graph: usize,
        node: u32,
        mode: EvaluatorMode,
    }

    struct State {
        counts: Redundancy,
        last_xz: HashMap<NodeKey, (i32, i32)>,
        last_xyz: HashMap<NodeKey, (i32, i32, i32)>,
        seen_xz: HashSet<(NodeKey, i32, i32)>,
        seen_xyz: HashSet<(NodeKey, i32, i32, i32)>,
        /// The same two sets with the *sampler* folded into the key. The gap
        /// between the pair is what a scratch shared across a column's samplers
        /// would collect and a per-sampler scratch structurally cannot.
        seen_own_xz: HashSet<(u64, NodeKey, i32, i32)>,
        seen_own_xyz: HashSet<(u64, NodeKey, i32, i32, i32)>,
        seen_compiled_point_scalar_xyz: HashSet<(NodeKey, i32, i32, i32)>,
        seen_compiled_point_batch_xyz: HashSet<(NodeKey, i32, i32, i32)>,
        scopes: HashSet<u64>,
    }

    impl State {
        fn new() -> Self {
            Self {
                counts: Redundancy::default(),
                last_xz: HashMap::new(),
                last_xyz: HashMap::new(),
                seen_xz: HashSet::new(),
                seen_xyz: HashSet::new(),
                seen_own_xz: HashSet::new(),
                seen_own_xyz: HashSet::new(),
                seen_compiled_point_scalar_xyz: HashSet::new(),
                seen_compiled_point_batch_xyz: HashSet::new(),
                scopes: HashSet::new(),
            }
        }
    }

    thread_local! {
        static ACTIVE: Cell<bool> = const { Cell::new(false) };
        static STATE: RefCell<State> = RefCell::new(State::new());
    }

    fn record(node: NodeKey, kind: usize, x: i32, y: i32, z: i32, field: bool, scope: u64) {
        STATE.with(|s| {
            let s = &mut *s.borrow_mut();
            if kind < super::KINDS {
                let one_slot_xyz = match s.last_xyz.insert(node, (x, y, z)) {
                    Some(prev) => prev == (x, y, z),
                    None => false,
                };
                if one_slot_xyz {
                    if field {
                        s.counts.field_xyz_single_hits[kind] += 1;
                    } else {
                        s.counts.point_xyz_single_hits[kind] += 1;
                    }
                }
            }
            if field && kind < super::KINDS {
                if s.scopes.insert(scope) {
                    s.counts.field_scopes += 1;
                }
                if !s.seen_own_xz.insert((scope, node, x, z)) {
                    s.counts.field_xz_own_hits[kind] += 1;
                }
                if !s.seen_own_xyz.insert((scope, node, x, y, z)) {
                    s.counts.field_xyz_own_hits[kind] += 1;
                }
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
    #[inline(always)]
    pub fn visit_point(node: *const (), kind: usize, x: i32, y: i32, z: i32) {
        ACTIVE.with(|active| {
            if active.get() {
                record(
                    NodeKey {
                        graph: node as usize,
                        node: 0,
                        mode: EvaluatorMode::Tree,
                    },
                    kind,
                    x,
                    y,
                    z,
                    false,
                    0,
                );
            }
        });
    }

    /// Records one field-evaluator node visit.
    ///
    /// A `NodeId` is only unique **within one `Graph`**, and a column evaluates
    /// several (`final_density`, `depth`, `erosion`, the climate channels), so the
    /// graph identity is kept alongside the node id. Keying on the id alone
    /// would merge unrelated nodes and over-report every hit rate.
    #[inline(always)]
    pub fn visit_field(
        graph: *const (),
        node: u32,
        kind: usize,
        x: i32,
        y: i32,
        z: i32,
        scope: u64,
        interpolated: bool,
    ) {
        ACTIVE.with(|active| {
            if active.get() {
                record(
                    NodeKey {
                        graph: graph as usize,
                        node,
                        mode: if interpolated {
                            EvaluatorMode::FieldInterpolated
                        } else {
                            EvaluatorMode::FieldExact
                        },
                    },
                    kind,
                    x,
                    y,
                    z,
                    true,
                    scope,
                );
            }
        });
    }

    /// Records one batched field-program node visit.
    #[inline(always)]
    pub fn visit_field_tile(
        graph: *const (),
        node: u32,
        kind: usize,
        x: i32,
        y: i32,
        z: i32,
        scope: u64,
    ) {
        ACTIVE.with(|active| {
            if active.get() {
                record(
                    NodeKey {
                        graph: graph as usize,
                        node,
                        mode: EvaluatorMode::FieldTile,
                    },
                    kind,
                    x,
                    y,
                    z,
                    true,
                    scope,
                );
            }
        });
    }

    fn begin_point_scope(mode: EvaluatorMode) {
        ACTIVE.with(|active| {
            if !active.get() {
                return;
            }
            STATE.with(|s| {
                let s = &mut *s.borrow_mut();
                match mode {
                    EvaluatorMode::PointScalar => {
                        s.counts.compiled_point_scalar_scopes += 1;
                        s.seen_compiled_point_scalar_xyz.clear();
                    }
                    EvaluatorMode::PointBatch => {
                        s.counts.compiled_point_batch_scopes += 1;
                        s.seen_compiled_point_batch_xyz.clear();
                    }
                    EvaluatorMode::Tree
                    | EvaluatorMode::FieldExact
                    | EvaluatorMode::FieldInterpolated
                    | EvaluatorMode::FieldTile => unreachable!(),
                }
            })
        })
    }

    /// Starts one scalar point-program evaluation scope.
    #[inline(always)]
    pub fn begin_point_scalar_scope() {
        begin_point_scope(EvaluatorMode::PointScalar);
    }

    /// Starts one fixed-width point-program batch evaluation scope.
    #[inline(always)]
    pub fn begin_point_batch_scope() {
        begin_point_scope(EvaluatorMode::PointBatch);
    }

    fn record_compiled_point(
        graph: *const (),
        node: u32,
        kind: usize,
        x: i32,
        y: i32,
        z: i32,
        mode: EvaluatorMode,
    ) {
        ACTIVE.with(|active| {
            if !active.get() || kind >= super::KINDS {
                return;
            }
            STATE.with(|s| {
                let s = &mut *s.borrow_mut();
                let key = NodeKey {
                    graph: graph as usize,
                    node,
                    mode,
                };
                let repeated = match mode {
                    EvaluatorMode::PointScalar => {
                        !s.seen_compiled_point_scalar_xyz.insert((key, x, y, z))
                    }
                    EvaluatorMode::PointBatch => {
                        !s.seen_compiled_point_batch_xyz.insert((key, x, y, z))
                    }
                    EvaluatorMode::Tree
                    | EvaluatorMode::FieldExact
                    | EvaluatorMode::FieldInterpolated
                    | EvaluatorMode::FieldTile => unreachable!(),
                };
                let (visits, scoped_hits) = match mode {
                    EvaluatorMode::PointScalar => (
                        &mut s.counts.compiled_point_scalar_visits,
                        &mut s.counts.compiled_point_scalar_xyz_scope_hits,
                    ),
                    EvaluatorMode::PointBatch => (
                        &mut s.counts.compiled_point_batch_visits,
                        &mut s.counts.compiled_point_batch_xyz_scope_hits,
                    ),
                    EvaluatorMode::Tree
                    | EvaluatorMode::FieldExact
                    | EvaluatorMode::FieldInterpolated
                    | EvaluatorMode::FieldTile => unreachable!(),
                };
                visits[kind] += 1;
                if repeated {
                    scoped_hits[kind] += 1;
                }
            });
        });
    }

    /// Records one scalar compiled point-program node visit.
    #[inline(always)]
    pub fn visit_compiled_point_scalar(
        graph: *const (),
        node: u32,
        kind: usize,
        x: i32,
        y: i32,
        z: i32,
    ) {
        record_compiled_point(
            graph,
            node,
            kind,
            x,
            y,
            z,
            EvaluatorMode::PointScalar,
        );
    }

    /// Records one batch compiled point-program node visit.
    #[inline(always)]
    pub fn visit_compiled_point_batch(
        graph: *const (),
        node: u32,
        kind: usize,
        x: i32,
        y: i32,
        z: i32,
    ) {
        record_compiled_point(
            graph,
            node,
            kind,
            x,
            y,
            z,
            EvaluatorMode::PointBatch,
        );
    }

    /// Starts recording on this thread.
    pub fn enable() {
        ACTIVE.with(|active| active.set(true));
    }

    /// Stops recording on this thread.
    pub fn disable() {
        ACTIVE.with(|active| active.set(false));
    }

    /// Clears the window (counts *and* the seen-sets).
    pub fn reset() {
        STATE.with(|s| {
            let s = &mut *s.borrow_mut();
            s.counts = Redundancy::default();
            s.last_xz.clear();
            s.last_xyz.clear();
            s.seen_xz.clear();
            s.seen_xyz.clear();
            s.seen_own_xz.clear();
            s.seen_own_xyz.clear();
            s.seen_compiled_point_scalar_xyz.clear();
            s.seen_compiled_point_batch_xyz.clear();
            s.scopes.clear();
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
    pub fn visit_field(
        _graph: *const (),
        _node: u32,
        _kind: usize,
        _x: i32,
        _y: i32,
        _z: i32,
        _scope: u64,
        _interpolated: bool,
    ) {
    }
    #[inline(always)]
    pub fn visit_field_tile(
        _graph: *const (),
        _node: u32,
        _kind: usize,
        _x: i32,
        _y: i32,
        _z: i32,
        _scope: u64,
    ) {
    }
    #[inline(always)]
    pub fn begin_point_scalar_scope() {}
    #[inline(always)]
    pub fn begin_point_batch_scope() {}
    #[inline(always)]
    pub fn visit_compiled_point_scalar(
        _graph: *const (),
        _node: u32,
        _kind: usize,
        _x: i32,
        _y: i32,
        _z: i32,
    ) {
    }
    #[inline(always)]
    pub fn visit_compiled_point_batch(
        _graph: *const (),
        _node: u32,
        _kind: usize,
        _x: i32,
        _y: i32,
        _z: i32,
    ) {
    }
    pub fn enable() {}
    pub fn disable() {}
    pub fn reset() {}
    #[must_use]
    pub fn snapshot() -> Redundancy {
        Redundancy::default()
    }
}

pub use live::{
    begin_point_batch_scope, begin_point_scalar_scope, disable, enable, reset, snapshot,
    visit_compiled_point_batch, visit_compiled_point_scalar, visit_field, visit_field_tile,
    visit_point,
};

#[cfg(all(test, feature = "gen-counters"))]
mod tests {
    use super::*;

    #[test]
    fn compiled_point_scope_keeps_nodes_and_y_coordinates_distinct() {
        let graph = 0_u8;
        let graph = std::ptr::from_ref(&graph).cast::<()>();
        let kind = Density::Const(0.0).kind_index();

        reset();
        enable();
        begin_point_scalar_scope();
        visit_compiled_point_scalar(graph, 1, kind, 4, 8, 12);
        visit_compiled_point_scalar(graph, 2, kind, 4, 8, 12);
        visit_compiled_point_scalar(graph, 1, kind, 4, 9, 12);
        visit_compiled_point_scalar(graph, 1, kind, 4, 8, 12);
        disable();

        let snapshot = snapshot();
        assert_eq!(snapshot.compiled_point_scalar_visits[kind], 4);
        assert_eq!(snapshot.compiled_point_scalar_xyz_scope_hits[kind], 1);
    }
}
