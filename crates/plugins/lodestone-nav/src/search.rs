//! The resumable, budgeted, weighted A\* with an anytime partial
//! (`docs/baritone-port.md` §4.6).
//!
//! **A state machine, not a loop.** That is the most important structural decision
//! after the simulated cost model, because it makes the two drivers — a dedicated
//! thread on native, a per-frame budget on wasm — *one implementation*. It also
//! makes the search deterministically testable (run exactly N steps, assert
//! state) and makes cancellation free: stop calling [`Search::step`].
//!
//! # Determinism
//!
//! Every source of order-dependence is closed deliberately:
//!
//! * costs are fixed-point ([`crate::ticks::Ticks`]), so equal-cost ties do not
//!   depend on float accumulation order;
//! * the total order in the open set is `(f, then u32::MAX - g, then insertion
//!   sequence)` — ties break toward **larger g**, deeper into the graph and closer
//!   to something, which is what stops flat terrain degenerating into
//!   breadth-first;
//! * neighbour expansion is in `Dir4::ALL` order, which is vanilla's horizontal
//!   order;
//! * no iteration over a `HashMap` ever influences an ordering decision — the map
//!   is only ever probed by key.
//!
//! The consequence is pinned by a test: the same snapshot, start, goal and policy
//! produce an identical plan across runs.

use std::collections::HashMap;
use std::sync::Arc;

use crate::cost::{EntryRel, SpeedClass, SurfaceClass, TemplateKey, TemplateTable};
use crate::goal::{Goal, Rates};
use crate::graph::{Arrival, MoveKind, NavNode, successors};
use crate::plan::{Edge, Plan};
use crate::policy::NavPolicy;
use crate::ticks::Ticks;
use crate::view::NavView;

/// How much work one [`Search::step`] may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Maximum node expansions this step.
    pub nodes: u32,
}

impl Budget {
    /// A budget sized for a per-tick driver: small enough not to jitter the one
    /// thread that must not jitter, large enough that a 20,000-node search
    /// finishes in ~10 ticks.
    pub const PER_TICK: Self = Self { nodes: 2_000 };
}

/// What a finished search concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A popped node satisfied the goal.
    Reached,
    /// Ran out of node budget.
    BudgetExhausted,
    /// The heap emptied, or the frontier sat at the snapshot edge. **Normal, not
    /// an error** — the world is finite and the goal usually is not.
    WorldExhausted,
    /// Nothing worth committing was found: no partial cleared `min_progress`.
    ///
    /// Distinct from `Superseded` (which lives at the plugin layer): a behaviour
    /// treats "no path" as a serious signal — blacklist a target, give up, tell the
    /// user — and conflating ordinary abandonment with genuine failure makes them
    /// abort constantly.
    Failed,
}

/// Whether a step finished the search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// More work to do.
    Working,
    /// Finished, with this outcome.
    Done(Outcome),
}

/// Counters, for the overlay and for the honest reporting the repo's evidence
/// standards ask for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchStats {
    /// Nodes popped and expanded.
    pub expanded: u32,
    /// Nodes inserted into the arena.
    pub generated: u32,
    /// Times a cheaper route to an open node was found.
    pub improved: u32,
    /// Times an improvement was below `min_improvement` and discarded.
    pub improvements_ignored: u32,
    /// Expansions that produced no successors because a stencil cell was outside
    /// the snapshot.
    pub edge_of_world: u32,
}

/// One arena slot.
#[derive(Debug, Clone, Copy)]
struct Node {
    node: NavNode,
    g: Ticks,
    f: Ticks,
    h: Ticks,
    parent: u32,
    /// The movement that reached this node from `parent`.
    kind: Option<MoveKind>,
    /// Feet surface at this node, carried so a plan does not have to re-derive it.
    surface: f64,
    /// Position in `heap`, or `NOT_IN_HEAP` when closed.
    heap_idx: u32,
    /// Insertion order, the final tie-break.
    seq: u32,
}

const NOT_IN_HEAP: u32 = u32::MAX;
const NO_PARENT: u32 = u32::MAX;

/// A single-use resumable search.
///
/// Two contracts worth enforcing, both cheap asserts that prevent confusing
/// states: a `Search` is **single-use** (calling [`Self::step`] after `Done` is a
/// programmer error, not a silent no-op), and **exactly one** may be in flight per
/// navigator.
#[derive(Debug)]
pub struct Search {
    view: Arc<dyn NavViewSend>,
    goal: Box<dyn Goal>,
    policy: NavPolicy,
    templates: TemplateTable,
    rates: Rates,

    arena: Vec<Node>,
    heap: Vec<u32>,
    index: HashMap<u64, u32>,
    stats: SearchStats,

    start: NavNode,
    /// Arena index of the expanded node with the smallest `h` so far, ignoring the
    /// `min_progress` filter (applied at extraction).
    best: u32,
    /// Arena index of a node that satisfied the goal, once one is popped.
    reached: Option<u32>,
    finished: Option<Outcome>,
    consecutive_edge_of_world: u32,
    /// Scratch, reused across expansions so a 20,000-node search allocates once.
    scratch: Vec<crate::graph::Step>,
}

/// `NavView` that can cross a thread boundary — what a worker owns.
///
/// A supertrait alias rather than bounds spelled at every use site, so the
/// boundary is auditable in one place: if a view is not `Send + Sync + 'static`
/// the design has been violated.
pub trait NavViewSend: NavView + Send + Sync + std::fmt::Debug + 'static {}

impl<T: NavView + Send + Sync + std::fmt::Debug + 'static> NavViewSend for T {}

impl Search {
    /// Begin a search from `start` toward `goal` over `view`.
    ///
    /// `policy` is taken by value and is immutable for the search's lifetime
    /// (`docs/baritone-port.md` §4.11).
    #[must_use]
    pub fn new(
        view: Arc<dyn NavViewSend>,
        start: NavNode,
        goal: Box<dyn Goal>,
        policy: NavPolicy,
        profile: lodestone_physics::PhysicsProfile,
    ) -> Self {
        let mut templates = TemplateTable::new(profile);
        let per_block = templates.cheapest_ticks_per_block();
        // Vertical rate: the cheapest way up a block is a jump, whose airborne
        // phase is ~12 ticks — but M1 has no `StepUp`, so an admissible bound only
        // has to be *a* lower bound. Charging one block of horizontal travel is
        // both a lower bound and cheap to justify; `StepUp` (M2) replaces it with
        // its own simulated template.
        let rates = Rates {
            per_block,
            per_block_up: per_block,
        };

        let h = goal.heuristic(start.x, start.y, start.z, &rates);
        let root = Node {
            node: start,
            g: Ticks::ZERO,
            f: weighted(Ticks::ZERO, h, policy.heuristic_weight),
            h,
            parent: NO_PARENT,
            kind: None,
            surface: f64::from(start.y),
            heap_idx: 0,
            seq: 0,
        };

        let mut search = Self {
            view,
            goal,
            policy,
            templates,
            rates,
            arena: vec![root],
            heap: vec![0],
            index: HashMap::new(),
            stats: SearchStats {
                generated: 1,
                ..SearchStats::default()
            },
            start,
            best: 0,
            reached: None,
            finished: None,
            consecutive_edge_of_world: 0,
            scratch: Vec::with_capacity(8),
        };
        if let Some(key) = start.try_pack() {
            search.index.insert(key, 0);
        } else {
            // A start outside the packable world cannot be searched from at all.
            search.finished = Some(Outcome::Failed);
        }
        search
    }

    /// Expand at most `budget.nodes` nodes.
    ///
    /// # Panics
    ///
    /// In debug builds, if called after the search has finished.
    pub fn step(&mut self, budget: Budget) -> Progress {
        debug_assert!(
            self.finished.is_none(),
            "a Search is single-use; stepping a finished search is a programmer error"
        );
        if let Some(outcome) = self.finished {
            return Progress::Done(outcome);
        }

        for _ in 0..budget.nodes {
            if self.stats.expanded >= self.policy.search_budget_nodes {
                return self.finish(Outcome::BudgetExhausted);
            }
            let Some(current) = self.pop() else {
                return self.finish(Outcome::WorldExhausted);
            };

            let node = self.arena[current as usize];
            if self.goal.satisfied(node.node.x, node.node.y, node.node.z) {
                debug_assert_eq!(
                    self.goal
                        .heuristic(node.node.x, node.node.y, node.node.z, &self.rates),
                    Ticks::ZERO,
                    "Goal::satisfied must imply heuristic == 0, or the search steps past its own goal"
                );
                self.reached = Some(current);
                return self.finish(Outcome::Reached);
            }

            self.stats.expanded += 1;
            if node.h < self.arena[self.best as usize].h {
                self.best = current;
            }
            self.expand(current);

            if self.consecutive_edge_of_world >= self.policy.edge_of_world_strikes {
                return self.finish(Outcome::WorldExhausted);
            }
        }
        Progress::Working
    }

    /// Run to completion. For tests and for a worker thread that owns its budget.
    pub fn run(&mut self, budget: Budget) -> Outcome {
        loop {
            if let Progress::Done(outcome) = self.step(budget) {
                return outcome;
            }
        }
    }

    /// The node with the smallest `h` reached so far, for a frontier overlay.
    #[must_use]
    pub fn frontier(&self) -> NavNode {
        self.arena[self.best as usize].node
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> SearchStats {
        self.stats
    }

    /// The outcome, once finished.
    #[must_use]
    pub fn outcome(&self) -> Option<Outcome> {
        self.finished
    }

    /// The template table, so a caller can report how many simulations a search
    /// actually cost.
    #[must_use]
    pub fn templates(&self) -> &TemplateTable {
        &self.templates
    }

    /// The best committable plan, as an **immutable copy**.
    ///
    /// Never a view into live structures: handing internals to a reader on another
    /// thread is the obvious way to make this unsound.
    ///
    /// On `Reached`, the plan to the goal, untruncated. Otherwise the expanded node
    /// with minimum `h` subject to being at least `min_progress` blocks from the
    /// start, with the tail discarded — the far end of a weighted search is both
    /// the least-trustworthy part and the least-known world.
    #[must_use]
    pub fn best_plan(&self) -> Option<Plan> {
        if let Some(reached) = self.reached {
            return self.plan_to(reached, false);
        }
        let best = self.arena[self.best as usize].node;
        let dx = f64::from(best.x - self.start.x);
        let dz = f64::from(best.z - self.start.z);
        if (dx * dx + dz * dz).sqrt() < self.policy.min_progress {
            // Report failure honestly. Committing a two-block plan produces
            // visible dithering and no progress, and returning *something* makes
            // the user believe a route exists while the bot visibly does nothing.
            return None;
        }
        self.plan_to(self.best, true)
    }

    /// Reconstruct the plan ending at arena index `end`.
    fn plan_to(&self, end: u32, truncate: bool) -> Option<Plan> {
        let mut edges = Vec::new();
        let mut cursor = end;
        while let Some(kind) = self.arena[cursor as usize].kind {
            let node = self.arena[cursor as usize];
            let parent = self.arena[node.parent as usize];
            edges.push(Edge {
                kind,
                from: parent.node,
                to: node.node,
                cost: Ticks::from_raw(node.g.raw().saturating_sub(parent.g.raw())),
                to_surface: node.surface,
            });
            cursor = node.parent;
        }
        edges.reverse();
        if edges.is_empty() {
            return None;
        }
        let plan = Plan::new(self.arena[cursor as usize].node, edges).ok()?;
        if !truncate || plan.len() < self.policy.tail_min_len {
            return Some(plan);
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let drop = ((plan.len() as f64) * self.policy.tail_discard).round() as usize;
        plan.truncated(drop).or(Some(plan))
    }

    /// Generate and relax every successor of arena index `current`.
    fn expand(&mut self, current: u32) {
        let node = self.arena[current as usize];
        self.scratch.clear();
        // Borrow dance: `successors` needs `&dyn NavView` while `self.scratch` is
        // borrowed mutably, so take the buffer out and put it back.
        let mut scratch = std::mem::take(&mut self.scratch);
        successors(self.view.as_ref(), node.node, &mut scratch);

        if scratch.is_empty() {
            // No successors at all: either genuinely boxed in, or the stencil
            // reached outside the snapshot. Only the latter is edge-of-world, and
            // distinguishing them is what keeps `WorldExhausted` from firing in a
            // sealed room.
            if self.touches_unknown(node.node) {
                self.stats.edge_of_world += 1;
                self.consecutive_edge_of_world += 1;
            } else {
                self.consecutive_edge_of_world = 0;
            }
            self.scratch = scratch;
            return;
        }
        self.consecutive_edge_of_world = 0;

        for step in &scratch {
            let arrival_dir = match node.node.arrival {
                Arrival::Still => None,
                Arrival::Walking(dir) => Some(dir),
            };
            // `WalkDiagonal` needs its own entry classification —
            // `EntryRel::of` assumes `going` is a single `Dir4`, which a
            // diagonal is not. `EntryRel::of_diagonal`'s own doc comment
            // explains why it collapses to `Still`/`Straight`/`Reverse`
            // rather than needing new variants.
            let entry = match step.kind {
                MoveKind::WalkDiagonal(d1, d2) => EntryRel::of_diagonal(arrival_dir, d1, d2),
                _ => EntryRel::of(arrival_dir, step.kind.dir()),
            };
            let Some(cost) = self.edge_cost(step, entry) else {
                continue;
            };
            self.relax(current, step.to, step.kind, step.to_surface, cost);
        }
        self.scratch = scratch;
    }

    /// Whether any of a node's movement stencils, in any direction, reads
    /// outside the snapshot.
    ///
    /// `Drop`'s representative `n = 2` is just that — a representative, since
    /// [`MoveKind::stencil`] ignores `n` and every `Drop` in a direction shares
    /// one generous, `n`-independent stencil (`drop_stencil` in `graph.rs`).
    fn touches_unknown(&self, node: NavNode) -> bool {
        let cardinal_or_drop = crate::graph::Dir4::ALL.iter().any(|dir| {
            [
                MoveKind::Walk(*dir),
                MoveKind::StepUp(*dir),
                MoveKind::Descend(*dir),
                MoveKind::Drop(*dir, 2),
            ]
            .iter()
            .any(|kind| {
                kind.stencil().iter().any(|cell| {
                    self.view
                        .state_at(node.x + cell[0], node.y + cell[1], node.z + cell[2])
                        .is_none()
                })
            })
        });
        if cardinal_or_drop {
            return true;
        }
        // Same idea, one direction pair per diagonal: `MoveKind::stencil`
        // ignores which pair it is (all four translate the same four-column
        // shape), so any one representative per `d1` covers it.
        crate::graph::Dir4::ALL.iter().any(|d1| {
            MoveKind::WalkDiagonal(*d1, d1.clockwise())
                .stencil()
                .iter()
                .any(|cell| {
                    self.view
                        .state_at(node.x + cell[0], node.y + cell[1], node.z + cell[2])
                        .is_none()
                })
        })
    }

    /// The simulated cost of one step, plus the policy's additive penalties.
    ///
    /// `None` when the simulation says the physics cannot perform the movement —
    /// a legality answer the graph's static predicates cannot produce, and the one
    /// that prevents the plan-fail-replan loop.
    fn edge_cost(&mut self, step: &crate::graph::Step, entry: EntryRel) -> Option<Ticks> {
        // The surface being walked *onto* is the one whose friction and speed
        // factor govern the movement: vanilla reads them from the block the feet
        // rest on at the end of the move.
        //
        // **Which cell that is comes off `to_surface`, not off `to.y - 1`.** By
        // `graph::stand_surface`'s convention a partial block (soul sand `0.875`, a
        // slab `0.5`) is stood on from *inside* its own cell, and the surface then
        // sits above that cell's floor; a full-height support is stood on from the
        // cell above, and the surface is exactly that cell's floor. Reading `to.y - 1`
        // unconditionally therefore classified every partial-block destination by
        // whatever is buried underneath it — silently, because the block under a slab
        // is usually more stone with the same friction, and because until `walk_step`
        // learned to change cells a soul-sand floor was not reachable at all. It is
        // the whole reason `SpeedClass::Slow` exists.
        let surface_cell = if step.to_surface > f64::from(step.to.y) + crate::graph::SURFACE_EPS {
            step.to.y
        } else {
            step.to.y - 1
        };
        let facts = self
            .view
            .facts_at(step.to.x, surface_cell, step.to.z)
            .or_else(|| self.view.facts_at(step.to.x, step.to.y, step.to.z))?;
        let key = TemplateKey {
            kind: step.kind.id(),
            entry,
            surface: SurfaceClass::of(facts.friction),
            speed: SpeedClass::of(facts.speed_factor),
            sprint: self.policy.allow_sprint,
            drop_n: if let MoveKind::Drop(_, n) = step.kind { n } else { 0 },
        };
        let template = self.templates.get(key);
        if !template.ok {
            return None;
        }
        let turn = Ticks::from_f64(f64::from(entry.quarter_turns()) * self.policy.turn_penalty);
        let mut extra = turn;
        match step.kind {
            MoveKind::StepUp(_) => {
                extra = extra.saturating_add(Ticks::from_f64(self.policy.jump_penalty));
            }
            MoveKind::Drop(_, _) => {
                let delta = step.from_surface - step.to_surface;
                // The hard legality cap: `docs/baritone-port.md` §4.4's "legality
                // is separate" — refuse rather than route a plan through a fall it
                // is not willing to take, regardless of how the cost model would
                // otherwise price it.
                if delta > self.policy.max_fall_blocks {
                    return None;
                }
                // The real damage rule (`LivingEntity.java:1856`), priced rather
                // than merely gated: `max_fall_blocks` alone would make every
                // legal drop free, and a policy that raises the cap should still
                // prefer a shorter fall over a longer, dearer one.
                let half_hearts =
                    (delta + 1e-6 - crate::graph::SAFE_FALL_DISTANCE).floor().max(0.0);
                extra = extra.saturating_add(Ticks::from_f64(half_hearts * self.policy.damage_cost));
            }
            MoveKind::Walk(_) | MoveKind::Descend(_) | MoveKind::WalkDiagonal(_, _) => {}
        }
        Some(template.ticks.saturating_add(extra))
    }

    /// Insert or improve a successor.
    fn relax(&mut self, parent: u32, to: NavNode, kind: MoveKind, surface: f64, cost: Ticks) {
        let Some(key) = to.try_pack() else {
            return;
        };
        let g = self.arena[parent as usize].g.saturating_add(cost);
        if g == Ticks::IMPOSSIBLE {
            return;
        }

        if let Some(&existing) = self.index.get(&key) {
            let old = self.arena[existing as usize];
            if g >= old.g {
                return;
            }
            if old.g.raw() - g.raw() < self.policy.min_improvement {
                self.stats.improvements_ignored += 1;
                return;
            }
            self.stats.improved += 1;
            let f = weighted(g, old.h, self.policy.heuristic_weight);
            let slot = &mut self.arena[existing as usize];
            slot.g = g;
            slot.f = f;
            slot.parent = parent;
            slot.kind = Some(kind);
            slot.surface = surface;
            if slot.heap_idx == NOT_IN_HEAP {
                // Re-open a closed node. Rare under a consistent heuristic, and
                // cheaper to handle than to prove impossible.
                let index = self.heap.len();
                self.heap.push(existing);
                self.arena[existing as usize].heap_idx = index as u32;
                self.sift_up(index);
            } else {
                let index = slot.heap_idx as usize;
                self.sift_up(index);
            }
            return;
        }

        let h = self.goal.heuristic(to.x, to.y, to.z, &self.rates);
        let seq = self.stats.generated;
        let arena_index = self.arena.len() as u32;
        self.arena.push(Node {
            node: to,
            g,
            f: weighted(g, h, self.policy.heuristic_weight),
            h,
            parent,
            kind: Some(kind),
            surface,
            heap_idx: self.heap.len() as u32,
            seq,
        });
        self.stats.generated += 1;
        self.index.insert(key, arena_index);
        let index = self.heap.len();
        self.heap.push(arena_index);
        self.sift_up(index);
    }

    fn finish(&mut self, outcome: Outcome) -> Progress {
        let outcome = if outcome == Outcome::Reached {
            outcome
        } else if self.best_plan_would_exist() {
            outcome
        } else {
            Outcome::Failed
        };
        self.finished = Some(outcome);
        Progress::Done(outcome)
    }

    /// Whether a partial clears `min_progress`, without building the plan.
    fn best_plan_would_exist(&self) -> bool {
        let best = self.arena[self.best as usize].node;
        let dx = f64::from(best.x - self.start.x);
        let dz = f64::from(best.z - self.start.z);
        (dx * dx + dz * dz).sqrt() >= self.policy.min_progress
    }

    // --- the heap: a binary min-heap of arena indices with decrease-key ---
    //
    // `heap_idx` lives on the node so a cost improvement is a decrease-key rather
    // than a re-insert or a linear scan. The shape is the one already proven in
    // `crates/lodestone-entity/src/pathfinding/heap.rs`.

    fn pop(&mut self) -> Option<u32> {
        if self.heap.is_empty() {
            return None;
        }
        let top = self.heap[0];
        let last = self.heap.pop().expect("non-empty");
        self.arena[top as usize].heap_idx = NOT_IN_HEAP;
        if !self.heap.is_empty() {
            self.heap[0] = last;
            self.arena[last as usize].heap_idx = 0;
            self.sift_down(0);
        }
        Some(top)
    }

    /// The total order: `f`, then **larger g**, then insertion sequence.
    fn less(&self, a: u32, b: u32) -> bool {
        let (a, b) = (&self.arena[a as usize], &self.arena[b as usize]);
        match a.f.cmp(&b.f) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => match b.g.cmp(&a.g) {
                std::cmp::Ordering::Less => true,
                std::cmp::Ordering::Greater => false,
                std::cmp::Ordering::Equal => a.seq < b.seq,
            },
        }
    }

    fn sift_up(&mut self, mut index: usize) {
        while index > 0 {
            let parent = (index - 1) / 2;
            if self.less(self.heap[index], self.heap[parent]) {
                self.swap(index, parent);
                index = parent;
            } else {
                break;
            }
        }
    }

    fn sift_down(&mut self, mut index: usize) {
        loop {
            let left = index * 2 + 1;
            if left >= self.heap.len() {
                return;
            }
            let right = left + 1;
            let mut smallest = left;
            if right < self.heap.len() && self.less(self.heap[right], self.heap[left]) {
                smallest = right;
            }
            if self.less(self.heap[smallest], self.heap[index]) {
                self.swap(index, smallest);
                index = smallest;
            } else {
                return;
            }
        }
    }

    fn swap(&mut self, a: usize, b: usize) {
        self.heap.swap(a, b);
        self.arena[self.heap[a] as usize].heap_idx = a as u32;
        self.arena[self.heap[b] as usize].heap_idx = b as u32;
    }
}

/// `f = g + w·h`, saturating.
fn weighted(g: Ticks, h: Ticks, weight: f64) -> Ticks {
    g.saturating_add(h.scaled(weight))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FactsTable, FixtureCensus};
    use crate::goal::{AtBlock, AtColumn};
    use crate::view::GridView;
    use lodestone_physics::PhysicsProfile;

    const AIR: u32 = FixtureCensus::AIR;
    const STONE: u32 = FixtureCensus::STONE;
    const SLAB: u32 = FixtureCensus::SLAB;

    fn flat(radius: i32) -> Arc<GridView> {
        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let mut view = GridView::new(facts, AIR, -64, 320, Some((-radius, -radius, radius, radius)));
        view.fill(-radius, 0, -radius, radius, 0, radius, STONE);
        Arc::new(view)
    }

    fn search(view: Arc<GridView>, goal: Box<dyn Goal>, policy: NavPolicy) -> Search {
        Search::new(
            view,
            NavNode::still(0, 1, 0),
            goal,
            policy,
            PhysicsProfile::mc_1_21(),
        )
    }

    #[test]
    fn it_reaches_a_nearby_goal_over_flat_ground() {
        let mut s = search(
            flat(40),
            Box::new(AtBlock { x: 10, y: 1, z: 0 }),
            NavPolicy::default(),
        );
        assert_eq!(s.run(Budget { nodes: 1_000 }), Outcome::Reached);
        let plan = s.best_plan().expect("a plan");
        assert_eq!(plan.len(), 10, "ten one-block walks");
        assert_eq!(plan.terminal().x, 10);
        // Ten straight walks at ~4.6 ticks each, the first from rest.
        let cost = plan.total_cost().as_f64();
        assert!((40.0..70.0).contains(&cost), "{cost} ticks for 10 blocks");
    }

    /// The claim that makes the plugin's whole notion of time honest: the plan's
    /// cost is what actually executing it takes. Not a closed loop against a cost
    /// table — the executor's own drive is replayed through the integrator here,
    /// which is the same code the plugin runs.
    #[test]
    fn the_planned_cost_matches_what_executing_the_plan_costs() {
        use crate::drive::WalkDrive;
        use lodestone_physics::{PlayerState, Vec3d};

        let view = flat(40);
        let mut s = search(
            view.clone(),
            Box::new(AtBlock { x: 12, y: 1, z: 0 }),
            NavPolicy::default(),
        );
        assert_eq!(s.run(Budget { nodes: 1_000 }), Outcome::Reached);
        let plan = s.best_plan().expect("a plan");

        let profile = PhysicsProfile::mc_1_21();
        let mut state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        state.on_ground = true;
        let mut actual = 0u32;
        for (i, edge) in plan.edges().iter().enumerate() {
            let drive = WalkDrive {
                cell: [edge.to.x, edge.to.y, edge.to.z],
                surface: edge.to_surface,
                brake: i + 1 == plan.len(),
                sprint: false,
                // `steer`: the loop below adopts `step.yaw` before ticking.
                steer: true,
                jump: matches!(edge.kind, MoveKind::StepUp(_)),
            };
            let mut edge_ticks = 0;
            while !drive.done(&state) && edge_ticks < 60 {
                let step = drive.tick(&state);
                state.yaw = step.yaw;
                state = state.with_movement_speed(f64::from(profile.base_movement_speed));
                lodestone_physics::tick(&mut state, step.input, view.as_ref(), &profile);
                edge_ticks += 1;
                actual += 1;
            }
            assert!(edge_ticks < 60, "edge {i} never completed");
        }

        // The final edge brakes, which the plan does not charge for, so allow the
        // executed total to exceed the planned one — but not by much. p95 ≤ 2
        // ticks per edge is §6's bar; over 12 edges that is 24 ticks of slack.
        let planned = plan.total_cost().as_f64();
        let error = f64::from(actual) - planned;
        assert!(
            error.abs() < 24.0,
            "planned {planned:.1} ticks, executed {actual} ({error:+.1})"
        );
        // And it actually arrived.
        assert_eq!(state.position.x.floor() as i32, 12);
    }

    /// The diagonal counterpart of `the_planned_cost_matches_what_executing_the_plan_costs`
    /// — the same real-physics replay, but over a plan that is `WalkDiagonal`
    /// edges end to end, which is the direct answer to whether
    /// `WalkDrive::arrived`/`done` (and therefore the executor, not merely the
    /// cost simulation) still hold for a genuinely non-cardinal approach.
    ///
    /// They do, and the reason is structural rather than luck:
    /// `WalkDrive::inside_cell` already ANDs `floor(x) == cell[0]` with
    /// `floor(z) == cell[2]` — it was never a single-axis test the way the cost
    /// model's own sub-tick fraction turned out to be (see `completion_fraction`'s
    /// doc comment for the bug that *was* real). `WalkDiagonal` also never
    /// changes surface height (same-cell-height family, like `Walk`), so the
    /// straddle trap `StepUp`/`Descend`/`Drop` needed `arrived`'s surface-height
    /// check for does not apply here either — a diagonal never straddles two
    /// *different* surfaces at once, only two different horizontal cells.
    #[test]
    fn the_planned_cost_matches_what_executing_a_diagonal_plan_costs() {
        use crate::drive::WalkDrive;
        use lodestone_physics::{PlayerState, Vec3d};

        let view = flat(40);
        let mut s = search(
            view.clone(),
            Box::new(AtBlock { x: 5, y: 1, z: 5 }),
            NavPolicy::default(),
        );
        assert_eq!(s.run(Budget { nodes: 1_000 }), Outcome::Reached);
        let plan = s.best_plan().expect("a plan");
        assert!(
            plan.edges()
                .iter()
                .all(|e| matches!(e.kind, MoveKind::WalkDiagonal(_, _))),
            "{:?}",
            plan.edges()
        );

        let profile = PhysicsProfile::mc_1_21();
        let mut state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        state.on_ground = true;
        let mut actual = 0u32;
        for (i, edge) in plan.edges().iter().enumerate() {
            let drive = WalkDrive {
                cell: [edge.to.x, edge.to.y, edge.to.z],
                surface: edge.to_surface,
                brake: i + 1 == plan.len(),
                sprint: false,
                steer: true,
                jump: false,
            };
            let mut edge_ticks = 0;
            while !drive.done(&state) && edge_ticks < 60 {
                let step = drive.tick(&state);
                state.yaw = step.yaw;
                state = state.with_movement_speed(f64::from(profile.base_movement_speed));
                lodestone_physics::tick(&mut state, step.input, view.as_ref(), &profile);
                edge_ticks += 1;
                actual += 1;
            }
            assert!(edge_ticks < 60, "edge {i} never completed");
        }

        let planned = plan.total_cost().as_f64();
        let error = f64::from(actual) - planned;
        assert!(
            error.abs() < 24.0,
            "planned {planned:.1} ticks, executed {actual} ({error:+.1})"
        );
        // And it actually arrived, on **both** axes — the exact thing a
        // single-axis `arrived`/`done` could get away with faking.
        assert_eq!(state.position.x.floor() as i32, 5);
        assert_eq!(state.position.z.floor() as i32, 5);
    }

    /// The surface a step is costed against is the block the **feet rest on**, which
    /// for a partial block is the destination cell itself and not the cell below it.
    ///
    /// `edge_cost` read `to.y - 1` unconditionally, so a soul-sand floor was costed as
    /// the stone buried under it: the `0.4` speed factor — the entire reason
    /// `SpeedClass::Slow` exists — never reached the template key from a real search.
    /// It could not be caught before `graph::walk_step` learned that a 0.125 step
    /// changes the feet *cell*, because until then no step landed on soul sand at all.
    ///
    /// The stone step in the opposite direction is the control: without it, "soul sand
    /// costs more" could be satisfied by any change that made every edge dearer.
    #[test]
    fn a_partial_block_destination_is_costed_by_the_block_the_feet_rest_on() {
        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let mut view = GridView::new(facts, AIR, -64, 320, Some((-4, -4, 4, 4)));
        view.fill(-4, 0, -4, 4, 0, 4, STONE);
        view.set(1, 0, 0, FixtureCensus::SOUL_SAND);
        let view = Arc::new(view);
        let mut s = search(
            view.clone(),
            Box::new(AtBlock { x: 1, y: 0, z: 0 }),
            NavPolicy::default(),
        );

        let onto = crate::graph::walk_step(
            view.as_ref(),
            NavNode::still(0, 1, 0),
            1.0,
            crate::graph::Dir4::East,
        )
        .expect("a 0.125 step down onto a soul-sand floor is a Walk");
        let off = crate::graph::walk_step(
            view.as_ref(),
            NavNode::still(0, 1, 0),
            1.0,
            crate::graph::Dir4::West,
        )
        .expect("and plain stone the other way");

        let slow = s
            .edge_cost(&onto, EntryRel::Straight)
            .expect("soul sand is walkable");
        let normal = s
            .edge_cost(&off, EntryRel::Straight)
            .expect("stone is walkable");
        assert!(
            slow > normal,
            "soul sand {slow} vs stone {normal}: the 0.4 speed factor must reach the \
             template key"
        );
    }

    /// Budget honesty: `step` expands exactly what it was given. Trivially true
    /// until someone adds an early return; then it is the test that notices.
    #[test]
    fn a_step_expands_exactly_its_budget() {
        let mut s = search(
            flat(60),
            Box::new(AtColumn { x: 500, z: 500 }),
            NavPolicy::default(),
        );
        assert_eq!(s.step(Budget { nodes: 37 }), Progress::Working);
        assert_eq!(s.stats().expanded, 37);
        assert_eq!(s.step(Budget { nodes: 13 }), Progress::Working);
        assert_eq!(s.stats().expanded, 50);
    }

    /// Determinism: same inputs, byte-identical plan. Cheap, and it catches
    /// accidental float or hash-order dependence.
    #[test]
    fn the_same_inputs_produce_an_identical_plan() {
        let plan_of = || {
            let mut s = search(
                flat(40),
                Box::new(AtBlock { x: 14, y: 1, z: 9 }),
                NavPolicy::default(),
            );
            s.run(Budget { nodes: 5_000 });
            s.best_plan().expect("a plan")
        };
        let a = plan_of();
        for _ in 0..5 {
            assert_eq!(plan_of(), a);
        }
    }

    /// Resumability is not a second implementation: stepping in slices must reach
    /// the identical plan as running in one go. This is what makes the wasm arm
    /// impossible to rot into an untested branch.
    #[test]
    fn stepping_in_slices_reaches_the_same_plan_as_one_pass() {
        let goal = || Box::new(AtBlock { x: 14, y: 1, z: -6 }) as Box<dyn Goal>;
        let mut one = search(flat(40), goal(), NavPolicy::default());
        one.run(Budget { nodes: 100_000 });

        let mut sliced = search(flat(40), goal(), NavPolicy::default());
        while sliced.step(Budget { nodes: 7 }) == Progress::Working {}

        assert_eq!(one.outcome(), sliced.outcome());
        assert_eq!(one.best_plan(), sliced.best_plan());
    }

    /// The mechanism, not a penalty: a goal outside the snapshot ends the search
    /// without inventing terrain, and still yields forward progress.
    #[test]
    fn a_goal_outside_the_snapshot_exhausts_the_world_and_still_makes_progress() {
        let mut s = search(
            flat(6),
            Box::new(AtColumn { x: 400, z: 0 }),
            NavPolicy::default(),
        );
        let outcome = s.run(Budget { nodes: 1_000 });
        assert_eq!(outcome, Outcome::WorldExhausted);
        let plan = s.best_plan().expect("a partial");
        assert!(plan.terminal().x >= 5, "{:?}", plan.terminal());
        assert!(
            plan.positions().all(|(x, _, z)| x.abs() <= 6 && z.abs() <= 6),
            "the plan left the known world"
        );
    }

    /// Honest failure: a goal two blocks away that cannot be reached produces
    /// `Failed`, not a dithering two-block plan.
    #[test]
    fn a_boxed_in_search_fails_rather_than_returning_a_useless_plan() {
        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let mut view = GridView::new(facts, AIR, -64, 320, Some((-4, -4, 4, 4)));
        view.fill(-4, 0, -4, 4, 0, 4, STONE);
        // Wall the player into a 1×1 cell.
        view.fill(1, 1, -1, 1, 2, 1, STONE);
        view.fill(-1, 1, -1, -1, 2, 1, STONE);
        view.fill(0, 1, 1, 0, 2, 1, STONE);
        view.fill(0, 1, -1, 0, 2, -1, STONE);

        let mut s = search(
            Arc::new(view),
            Box::new(AtBlock { x: 3, y: 1, z: 0 }),
            NavPolicy::default(),
        );
        assert_eq!(s.run(Budget { nodes: 1_000 }), Outcome::Failed);
        assert!(s.best_plan().is_none());
    }

    /// A route over a slab is chosen and reports the slab's real surface — the
    /// case no scene in the tree could exercise before per-state collision landed.
    #[test]
    fn a_plan_over_a_slab_carries_the_slab_surface() {
        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let mut view = GridView::new(facts, AIR, -64, 320, Some((-10, -2, 10, 2)));
        view.fill(-10, 0, -2, 10, 0, 2, STONE);
        view.set(3, 1, 0, SLAB);
        let mut s = search(
            Arc::new(view),
            Box::new(AtBlock { x: 6, y: 1, z: 0 }),
            NavPolicy::default(),
        );
        assert_eq!(s.run(Budget { nodes: 1_000 }), Outcome::Reached);
        let plan = s.best_plan().expect("a plan");
        let over_slab = plan
            .edges()
            .iter()
            .find(|e| (e.to.x, e.to.z) == (3, 0))
            .expect("the plan crosses the slab column");
        assert!(
            (over_slab.to_surface - 1.5).abs() < 1e-9,
            "{}",
            over_slab.to_surface
        );
    }

    /// Every plan the search emits is well-formed. The type enforces it, so this
    /// is the assertion that the *search* never has to be rescued by the type.
    #[test]
    fn every_emitted_plan_is_well_formed() {
        for (gx, gz) in [(9, 0), (0, 9), (7, 7), (-8, 4), (12, -11)] {
            let mut s = search(
                flat(30),
                Box::new(AtBlock { x: gx, y: 1, z: gz }),
                NavPolicy::default(),
            );
            s.run(Budget { nodes: 20_000 });
            let plan = s.best_plan().expect("a plan");
            let mut seen = std::collections::HashSet::new();
            assert!(plan.positions().all(|p| seen.insert(p)));
            assert_eq!(plan.positions().count(), plan.len() + 1);
        }
    }

    /// `docs/baritone-port.md` §4.1's whole point in having `WalkDiagonal` at
    /// all: reaching a 45°-offset goal via **five diagonal edges** must cost
    /// less than reaching an *equal Manhattan-distance* goal that can only be
    /// walked cardinally in **ten**. Before `WalkDiagonal` existed this
    /// comparison was the opposite claim ("the dog-leg costs more, because it
    /// needs a turn") — the graph was 4-connected then, so `(5, 5)` really
    /// was a ten-edge zigzag with an extra turn baked in. It is now an
    /// eight-connected graph, and `(5, 5)` is the cheap route, not the dear
    /// one: this test is the one this crate's own generalisation from M1 to
    /// M2's diagonal made obsolete in its old form, kept here in its new one
    /// so the search's actual advantage is never assumed, only measured.
    ///
    /// This is also the gate `docs/baritone-port.md`'s brief for this work
    /// calls the "exact resulting path" check: the plan to `(5, 5)` must
    /// actually **use** `WalkDiagonal` edges — cheaper-in-theory and
    /// cheaper-in-practice are different claims, and only the second is
    /// worth anything to a player watching the bot walk.
    #[test]
    fn a_diagonal_reachable_goal_beats_an_equal_manhattan_cardinal_only_goal() {
        let cost_to = |gx: i32, gz: i32| {
            let mut s = search(
                flat(30),
                Box::new(AtBlock { x: gx, y: 1, z: gz }),
                NavPolicy::default(),
            );
            s.run(Budget { nodes: 20_000 });
            s.best_plan().expect("a plan")
        };
        // Both goals are 10 blocks of Manhattan distance from the start.
        let straight = cost_to(10, 0);
        let diagonal = cost_to(5, 5);
        assert!(
            diagonal.total_cost() < straight.total_cost(),
            "diagonal-reachable {} should beat the cardinal-only equal-Manhattan-distance {}",
            diagonal.total_cost(),
            straight.total_cost()
        );
        assert_eq!(diagonal.len(), 5, "five diagonal edges, not ten cardinal ones");
        assert!(
            diagonal
                .edges()
                .iter()
                .all(|e| matches!(e.kind, MoveKind::WalkDiagonal(_, _))),
            "{:?}",
            diagonal.edges()
        );
        assert_eq!(diagonal.terminal().x, 5);
        assert_eq!(diagonal.terminal().z, 5);
    }

    /// The unreachable control for `WalkDiagonal`: a one-block-wide,
    /// L-shaped corridor — walled solid on both sides everywhere off the path
    /// itself — must force a cardinal-only route to the same `(5, 5)` goal
    /// the previous test reaches diagonally, because a diagonal's shoulder
    /// check (`diagonal_step`) can never find *both* shoulders open when
    /// there is no two-dimensional open space for them to be open *in*. This
    /// is the "did the detector actually fire" standard the M1/M2 cardinal
    /// controls (`a_boxed_in_search_fails_rather_than_returning_a_useless_plan`,
    /// `an_ascend_taller_than_the_jump_apex_is_refused`) already hold to —
    /// and it is a real before/after, not a test that could never have
    /// failed: an earlier version of this control walled only the direct
    /// diagonal line's own shoulders, and the search simply shifted the
    /// diagonal run over by one row to dodge it, still using four
    /// `WalkDiagonal` edges. Only removing all room for a shoulder to exist
    /// closes that escape.
    #[test]
    fn a_diagonal_walled_off_at_every_shoulder_forces_the_cardinal_only_route() {
        let facts = Arc::new(FactsTable::build(&FixtureCensus));
        let mut view = GridView::new(facts, AIR, -64, 320, Some((-2, -2, 8, 8)));
        // Solid everywhere, at foot height and head height, off the corridor.
        view.fill(-2, 0, -2, 8, 0, 8, STONE);
        view.fill(-2, 1, -2, 8, 2, 8, STONE);
        // Carve the one-block-wide L: east along z = 0 from x = 0 to 5, then
        // north along x = 5 from z = 0 to 5.
        for x in 0..=5 {
            view.set(x, 1, 0, AIR);
            view.set(x, 2, 0, AIR);
        }
        for z in 0..=5 {
            view.set(5, 1, z, AIR);
            view.set(5, 2, z, AIR);
        }
        let view = Arc::new(view);

        let mut s = search(
            view,
            Box::new(AtBlock { x: 5, y: 1, z: 5 }),
            NavPolicy::default(),
        );
        s.run(Budget { nodes: 20_000 });
        let plan = s.best_plan().expect("still reachable along the corridor");
        assert!(
            plan.edges()
                .iter()
                .all(|e| matches!(e.kind, MoveKind::Walk(_))),
            "a one-block-wide corridor leaves no room for any diagonal shoulder: {:?}",
            plan.edges()
        );
        assert_eq!((plan.terminal().x, plan.terminal().z), (5, 5));
    }
}
