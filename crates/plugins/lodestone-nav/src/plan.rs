//! A committed plan, and the well-formedness it must have.
//!
//! `docs/baritone-port.md` §2.3: the position→edge mapping is many-to-one and
//! **must stay a function**. Both of the executor's index-recovery scans depend on
//! it, and on the plan **never revisiting a position** — a plan that does makes the
//! mapping ambiguous and recovery non-deterministic. So well-formedness is validated
//! at construction, not assumed.

use std::collections::HashSet;

use crate::graph::{MoveKind, NavNode};
use crate::ticks::Ticks;

/// One movement of a plan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edge {
    /// What to do.
    pub kind: MoveKind,
    /// Where it starts.
    pub from: NavNode,
    /// Where it ends.
    pub to: NavNode,
    /// **Planning-time** cost.
    ///
    /// The stall budget is captured from this when the edge begins and never
    /// recomputed. `docs/baritone-port.md` §2.3 calls that the most non-obvious rule
    /// in the section: a break-then-move edge's *remaining* cost falls as you break
    /// the blocks, so a live budget always outruns the elapsed time and the counter
    /// never trips — the bot then digs forever.
    pub cost: Ticks,
    /// World-space feet `y` at [`Self::to`], from the legality check that admitted
    /// this edge.
    pub to_surface: f64,
}

/// Why a plan was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    /// An edge's `from` is not the previous edge's `to`.
    Discontinuous(usize),
    /// A cell appears twice, so the position→edge mapping is not a function.
    RevisitsAPosition(usize),
    /// The plan has no edges.
    Empty,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Discontinuous(i) => write!(f, "edge {i} does not start where the previous ended"),
            Self::RevisitsAPosition(i) => write!(f, "edge {i} revisits an earlier position"),
            Self::Empty => write!(f, "plan has no edges"),
        }
    }
}

impl std::error::Error for PlanError {}

/// A validated sequence of movements.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    start: NavNode,
    edges: Vec<Edge>,
    total: Ticks,
}

impl Plan {
    /// Build and validate.
    ///
    /// # Errors
    ///
    /// [`PlanError`] when the plan is empty, discontinuous, or revisits a cell.
    pub fn new(start: NavNode, edges: Vec<Edge>) -> Result<Self, PlanError> {
        if edges.is_empty() {
            return Err(PlanError::Empty);
        }
        let mut seen: HashSet<(i32, i32, i32)> = HashSet::with_capacity(edges.len() + 1);
        seen.insert((start.x, start.y, start.z));
        let mut cursor = start;
        let mut total = Ticks::ZERO;
        for (i, edge) in edges.iter().enumerate() {
            if (edge.from.x, edge.from.y, edge.from.z) != (cursor.x, cursor.y, cursor.z) {
                return Err(PlanError::Discontinuous(i));
            }
            if !seen.insert((edge.to.x, edge.to.y, edge.to.z)) {
                return Err(PlanError::RevisitsAPosition(i));
            }
            total = total.saturating_add(edge.cost);
            cursor = edge.to;
        }
        Ok(Self {
            start,
            edges,
            total,
        })
    }

    /// The node the plan starts at.
    #[must_use]
    pub fn start(&self) -> NavNode {
        self.start
    }

    /// The node the plan ends at — the hand-off point a continuation search starts
    /// from, which is what makes concatenation trivially valid and removes the need
    /// for a splice predicate at all.
    #[must_use]
    pub fn terminal(&self) -> NavNode {
        self.edges.last().map_or(self.start, |e| e.to)
    }

    /// The movements, in order.
    #[must_use]
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Number of movements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Whether the plan is empty. Never true for a constructed plan; present because
    /// clippy asks for it beside `len`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Sum of every edge's planning-time cost.
    #[must_use]
    pub fn total_cost(&self) -> Ticks {
        self.total
    }

    /// Estimated cost of the edges from `index` onward, **excluding** the edge at
    /// `index` itself.
    ///
    /// The exclusion is §2.3's non-obvious detail and it is not an off-by-one: the
    /// look-ahead trigger has to ignore the currently executing edge, because
    /// including it lets one long edge (a big descent) keep the estimate above the
    /// trigger until it completes — so the look-ahead never fires and the bot stalls
    /// at the segment boundary anyway.
    #[must_use]
    pub fn remaining_cost_after(&self, index: usize) -> Ticks {
        self.edges
            .iter()
            .skip(index.saturating_add(1))
            .fold(Ticks::ZERO, |acc, e| acc.saturating_add(e.cost))
    }

    /// Every cell the plan passes through, start first.
    pub fn positions(&self) -> impl Iterator<Item = (i32, i32, i32)> + '_ {
        std::iter::once((self.start.x, self.start.y, self.start.z))
            .chain(self.edges.iter().map(|e| (e.to.x, e.to.y, e.to.z)))
    }

    /// The union of every edge's translated stencil: the cells whose values this
    /// plan's legality depended on (`docs/baritone-port.md` §4.5).
    ///
    /// Computed **once, at commit**, not during the search. A block update is then
    /// tested for membership — `O(block updates)` per tick rather than `O(plan)` per
    /// tick forever, for an event that almost never happens.
    #[must_use]
    pub fn witnesses(&self) -> HashSet<u64> {
        let mut out = HashSet::with_capacity(self.edges.len() * 8);
        for edge in &self.edges {
            for cell in edge.kind.stencil() {
                let node = NavNode::still(
                    edge.from.x + cell[0],
                    edge.from.y + cell[1],
                    edge.from.z + cell[2],
                );
                if let Some(key) = node.try_pack() {
                    out.insert(key);
                }
            }
        }
        out
    }

    /// Whether a changed cell invalidates a plan with these witnesses.
    #[must_use]
    pub fn witnesses_contain(witnesses: &HashSet<u64>, x: i32, y: i32, z: i32) -> bool {
        NavNode::still(x, y, z)
            .try_pack()
            .is_some_and(|key| witnesses.contains(&key))
    }

    /// Squared horizontal distance from `position` to the nearest cell centre in the
    /// plan.
    ///
    /// **Nearest anywhere in the plan, not the current node** (§2.3), so a legitimate
    /// shortcut does not read as drift. Horizontal only, which is also §2.3's rule:
    /// mid-fall you are genuinely far from both the block you left and the one you
    /// will land on without being off-course at all.
    #[must_use]
    pub fn nearest_distance_sqr(&self, x: f64, z: f64) -> f64 {
        self.positions()
            .map(|(px, _, pz)| {
                let dx = f64::from(px) + 0.5 - x;
                let dz = f64::from(pz) + 0.5 - z;
                dx * dx + dz * dz
            })
            .fold(f64::INFINITY, f64::min)
    }

    /// Drop the last `count` edges, keeping the plan valid.
    ///
    /// Returns `None` when that would empty it, because an empty plan is not a
    /// shorter plan — it is a failure, and the caller has to say so.
    #[must_use]
    pub fn truncated(&self, count: usize) -> Option<Self> {
        if count >= self.edges.len() {
            return None;
        }
        let mut edges = self.edges.clone();
        edges.truncate(self.edges.len() - count);
        Self::new(self.start, edges).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Arrival, Dir4};

    fn walk_edge(from: NavNode, dir: Dir4) -> Edge {
        let (dx, dz) = dir.delta();
        Edge {
            kind: MoveKind::Walk(dir),
            from,
            to: NavNode {
                x: from.x + dx,
                y: from.y,
                z: from.z + dz,
                arrival: Arrival::Walking(dir),
            },
            cost: Ticks::from_f64(4.63),
            to_surface: f64::from(from.y),
        }
    }

    fn straight(n: usize) -> Plan {
        let mut cursor = NavNode::still(0, 1, 0);
        let mut edges = Vec::new();
        for _ in 0..n {
            let edge = walk_edge(cursor, Dir4::East);
            cursor = edge.to;
            edges.push(edge);
        }
        Plan::new(NavNode::still(0, 1, 0), edges).expect("well formed")
    }

    #[test]
    fn a_straight_plan_is_well_formed_and_costs_the_sum_of_its_edges() {
        let plan = straight(5);
        assert_eq!(plan.len(), 5);
        assert_eq!(plan.terminal().x, 5);
        assert_eq!(plan.positions().count(), 6, "one more position than edges");
        assert!((plan.total_cost().as_f64() - 5.0 * 4.63).abs() < 0.05);
    }

    /// The invariant §2.3 says recovery becomes non-deterministic without.
    #[test]
    fn a_plan_that_doubles_back_is_rejected() {
        let start = NavNode::still(0, 1, 0);
        let out = walk_edge(start, Dir4::East);
        let back = walk_edge(out.to, Dir4::West);
        assert_eq!(
            Plan::new(start, vec![out, back]),
            Err(PlanError::RevisitsAPosition(1))
        );
    }

    #[test]
    fn a_discontinuous_plan_is_rejected() {
        let start = NavNode::still(0, 1, 0);
        let a = walk_edge(start, Dir4::East);
        let b = walk_edge(NavNode::still(50, 1, 0), Dir4::East);
        assert_eq!(Plan::new(start, vec![a, b]), Err(PlanError::Discontinuous(1)));
    }

    #[test]
    fn an_empty_plan_is_rejected() {
        assert_eq!(
            Plan::new(NavNode::still(0, 1, 0), Vec::new()),
            Err(PlanError::Empty)
        );
    }

    /// The exclusion that stops one long edge suppressing the look-ahead trigger.
    #[test]
    fn remaining_cost_excludes_the_current_edge() {
        let plan = straight(4);
        let all = plan.total_cost().as_f64();
        let after_first = plan.remaining_cost_after(0).as_f64();
        assert!(
            (all - after_first - 4.63).abs() < 0.05,
            "all {all}, after first {after_first}"
        );
        assert_eq!(plan.remaining_cost_after(3), Ticks::ZERO);
    }

    #[test]
    fn witnesses_cover_every_cell_each_edge_read() {
        let plan = straight(3);
        let witnesses = plan.witnesses();
        for x in 0..=3 {
            assert!(
                Plan::witnesses_contain(&witnesses, x, 0, 0),
                "support at x={x} not witnessed"
            );
        }
        assert!(!Plan::witnesses_contain(&witnesses, 0, 0, 40));
    }

    /// Drift is measured to the nearest cell **anywhere** in the plan, so a shortcut
    /// does not look like drift.
    #[test]
    fn drift_distance_is_to_the_nearest_plan_cell_not_the_current_one() {
        let plan = straight(10);
        // Standing on the far end of the plan is not drift, even though it is 9
        // blocks from the start.
        assert!(plan.nearest_distance_sqr(9.5, 0.5) < 1e-9);
        // Standing well off the line is.
        assert!(plan.nearest_distance_sqr(5.5, 8.5) > 60.0);
    }

    #[test]
    fn truncation_keeps_the_plan_valid_and_refuses_to_empty_it() {
        let plan = straight(10);
        let short = plan.truncated(3).expect("still non-empty");
        assert_eq!(short.len(), 7);
        assert_eq!(short.terminal().x, 7);
        assert!(plan.truncated(10).is_none());
        assert!(plan.truncated(99).is_none());
    }
}
