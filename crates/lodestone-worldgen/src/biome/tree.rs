//! Vanilla's `Climate.RTree`, ported — the search structure that replaces the
//! O(table_len) brute-force climate scan (`docs/plans/worldgen-rewrite.md` D5),
//! and per the owner's own ruling also **the definition of the answer**.
//!
//! # What is ported, and the one thing that cannot be
//!
//! **The tree structure is a literal port** of vanilla's own R-tree
//! construction routines — `create`/`build`/
//! `bucketize`/`sort`/`cost`/`buildParameterSpace`, 26.2 de-obfuscated. Node
//! ordering therefore comes from
//! the game data's own row order run through vanilla's own splitting heuristic —
//! nothing here re-derives a bucketing scheme of its own.
//!
//! **The search is now a literal port too**, including vanilla's own last-result
//! field. Vanilla's own subtree-search
//! routine, restated without its Java syntax, is:
//!
//! ```text
//! minDistance = candidate is absent ? MAX : distance(candidate, target)
//! closestLeaf = candidate
//! for each child of this node:
//!    childDistance = distance(child, target)
//!    if minDistance > childDistance:
//!       leaf = child.search(target, closestLeaf)
//!       leafDistance = if child is itself a leaf: childDistance
//!                      else: distance(leaf, target)
//!       if minDistance > leafDistance:
//!          minDistance = leafDistance
//!          closestLeaf = leaf
//! ```
//!
//! and vanilla's own top-level search routine seeds `candidate` from its own
//! last-result field, a `ThreadLocal` holding *the previous search's leaf*, then
//! stores the new one. The port keeps that history in an explicit cursor owned by
//! one generation lifecycle, so worker assignment cannot become semantic state.
//!
//! ## What `lastResult` can and cannot do — traced, not guessed
//!
//! This matters because if the seed reached the answer, "what vanilla does" would
//! be thread-schedule-dependent and could not be implemented as a function at all.
//! Two claims, both resting on span containment (below):
//!
//! * **It cannot change the returned leaf's distance.** A node's bound
//!   lower-bounds every leaf beneath it. A subtree is skipped only when
//!   `minDistance <= bound`, and at that moment every leaf inside is at distance
//!   `>= bound >= minDistance`; `minDistance` is always the distance of some real
//!   leaf and only decreases. So no skipped subtree ever held anything better, and
//!   the returned leaf is always at the global minimum distance `d_min` — for
//!   **every** seed, including an adversarial one.
//! * **It can change the returned *row*.** If `dist(candidate) == d_min`, then
//!   `minDistance == d_min` from the start; every subtree has `bound <= d_min` so
//!   is skipped unless `bound < d_min`, and any subtree that *is* descended yields
//!   a leaf at distance `>= d_min`, so the strict `minDistance > leafDistance`
//!   never fires. **The candidate itself is returned**, beating whichever leaf
//!   tree order would otherwise have selected.
//!
//! So `lastResult` is not a pure optimisation: at a target where two rows tie on
//! squared distance, vanilla's answer depends on what that thread searched before.
//! `a_tying_seed_changes_the_returned_row` in `biome_tree_identity.rs` exhibits a
//! concrete case rather than leaving this as an argument.
//!
//! ## Therefore: which vanilla behaviour this implements
//!
//! Production uses the same lifecycle history: each query seeds the traversal
//! from the previous query's selected leaf, and the selected leaf becomes the
//! next seed. Query order is therefore part of a generation lifecycle and must
//! remain fixed inside the staged pipeline. The public stateless helper is kept
//! as a negative control for fixtures and diagnostics.
//!
//! # The theorem, restated for vanilla's tie-break
//!
//! Span containment gives the part that still holds unconditionally:
//!
//! * **The node bound is a lower bound.** A subtree's `space[i]` is the interval
//!   hull of its children's (`buildParameterSpace` unions with vanilla's own per-axis span),
//!   and for `[lo, hi] ⊇ [lo', hi']`, `distance([lo, hi], t) <= distance([lo',
//!   hi'], t)` for every `t`. By induction `bound(N, t) <= dist(L, t)` for every
//!   leaf `L` under `N`. [`BiomeTree::hull_containment_violations`] checks that
//!   premise by complete enumeration over **every** node/child pair on all 7 axes
//!   of the real tree.
//! * **Hence `nearest_row` returns a row at the same minimum distance as
//!   [`super::nearest_row_brute_force`], at every target.** That is the strongest
//!   true statement, and it is what the exhaustive lattice gates now assert.
//!
//! What is *no longer* claimed is
//! row-identity with brute force. Brute force breaks a tie by **earliest table
//! row**; vanilla's tree breaks it by **traversal order**. Measured on the real
//! table, those disagree on the resolved biome id at 0.98% of arbitrary targets.
//! Vanilla calls the tree, so the tree is the answer and brute force is now the
//! documented divergence — retained as the independent implementation that proves
//! the distance claim, not as the target.

use super::{BiomeParameterPoint, Parameter};

#[cfg(test)]
use std::cell::Cell;

/// Vanilla's own R-tree children-per-node constant.
const CHILDREN_PER_NODE: usize = 6;

/// The climate parameter space's dimensionality — vanilla asserts this is 7
/// (vanilla's own R-tree `create`'s `if (dimensions != 7) throw`), six climate axes plus the
/// degenerate `offset` span. See [`BiomeParameterPoint`].
const DIMENSIONS: usize = 7;

/// "No leaf selected" — a node id that cannot exist.
const NONE: u32 = u32::MAX;

#[cfg(test)]
thread_local! {
    static BOUND_NARROW_AXIS_HISTOGRAM: Cell<[u64; DIMENSIONS + 1]> =
        const { Cell::new([0; DIMENSIONS + 1]) };
    static BOUND_NARROW_AXIS_HISTOGRAM_ENABLED: Cell<bool> = const { Cell::new(false) };
}

#[inline(always)]
fn record_narrow_cutoff(axis: usize) {
    #[cfg(test)]
    BOUND_NARROW_AXIS_HISTOGRAM_ENABLED.with(|enabled| {
        if enabled.get() {
            BOUND_NARROW_AXIS_HISTOGRAM.with(|histogram| {
                let mut counts = histogram.get();
                counts[axis] += 1;
                histogram.set(counts);
            });
        }
    });
    #[cfg(not(test))]
    let _ = axis;
}

/// One flattened tree node in the wide representation. Leaves and subtrees share
/// a representation; bounded tables are converted to the compact equivalent.
#[derive(Debug, Clone)]
struct Node {
    /// The node's own 7-axis span: for a leaf, the biome's own parameter point;
    /// for a subtree, the interval hull of its children (vanilla's
    /// `buildParameterSpace`).
    space: [Parameter; DIMENSIONS],
    /// `[first_child, first_child + child_count)` into [`BiomeTree::children`],
    /// or an empty range for a leaf.
    first_child: u32,
    child_count: u32,
    /// The table row this leaf carries. Subtrees store the minimum row beneath
    /// them, which is *no longer read*: it existed to prune for brute force's
    /// lowest-row tie-break, and vanilla breaks ties by traversal order instead.
    /// Kept because the leaf case is how a search result becomes a row.
    row: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct NarrowParameter {
    min: i32,
    max: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct NarrowNode {
    space: [NarrowParameter; DIMENSIONS],
    first_child: u32,
    child_count: u32,
}

#[derive(Debug, Clone)]
enum NodeStorage {
    Wide(Vec<Node>),
    Narrow(Vec<NarrowNode>),
}

impl NodeStorage {
    fn from_wide(nodes: Vec<Node>) -> Self {
        let Some(narrow) = nodes
            .iter()
            .map(|node| {
                let space = node
                    .space
                    .iter()
                    .map(|parameter| {
                        Some(NarrowParameter {
                            min: i32::try_from(parameter.min).ok()?,
                            max: i32::try_from(parameter.max).ok()?,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?
                    .try_into()
                    .ok()?;
                Some(NarrowNode {
                    space,
                    first_child: if node.child_count == 0 {
                        node.row
                    } else {
                        node.first_child
                    },
                    child_count: node.child_count,
                })
            })
            .collect::<Option<Vec<_>>>()
        else {
            return Self::Wide(nodes);
        };
        Self::Narrow(narrow)
    }

    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Wide(nodes) => nodes.len(),
            Self::Narrow(nodes) => nodes.len(),
        }
    }

    #[inline]
    fn is_leaf(&self, id: u32) -> bool {
        match self {
            Self::Wide(nodes) => nodes[id as usize].is_leaf(),
            Self::Narrow(nodes) => nodes[id as usize].child_count == 0,
        }
    }

    #[inline]
    fn metadata(&self, id: u32) -> (u32, u32, u32) {
        match self {
            Self::Wide(nodes) => {
                let node = &nodes[id as usize];
                (node.first_child, node.child_count, node.row)
            }
            Self::Narrow(nodes) => {
                let node = &nodes[id as usize];
                (
                    node.first_child,
                    node.child_count,
                    if node.child_count == 0 {
                        node.first_child
                    } else {
                        0
                    },
                )
            }
        }
    }

    #[inline]
    fn space(&self, id: u32) -> [Parameter; DIMENSIONS] {
        match self {
            Self::Wide(nodes) => nodes[id as usize].space,
            Self::Narrow(nodes) => nodes[id as usize].space.map(|parameter| Parameter {
                min: i64::from(parameter.min),
                max: i64::from(parameter.max),
            }),
        }
    }

    #[cfg(test)]
    fn into_wide(self) -> Self {
        match self {
            Self::Wide(nodes) => Self::Wide(nodes),
            Self::Narrow(nodes) => Self::Wide(
                nodes
                    .into_iter()
                    .map(|node| Node {
                        space: node.space.map(|parameter| Parameter {
                            min: i64::from(parameter.min),
                            max: i64::from(parameter.max),
                        }),
                        first_child: node.first_child,
                        child_count: node.child_count,
                        row: if node.child_count == 0 {
                            node.first_child
                        } else {
                            0
                        },
                    })
                    .collect(),
            ),
        }
    }

    #[cfg(test)]
    #[inline]
    fn is_narrow(&self) -> bool {
        matches!(self, Self::Narrow(_))
    }
}

impl Node {
    #[inline]
    fn is_leaf(&self) -> bool {
        self.child_count == 0
    }
}

/// A node under construction, before flattening. Vanilla's `build` works on a
/// `List<Node>` it re-sorts in place seven times per level; doing that on owned
/// subtrees would clone the whole leaf set once per axis, so construction shuffles
/// `u32` arena indices instead and the arena holds the spans.
#[derive(Debug)]
struct Arena {
    space: Vec<[Parameter; DIMENSIONS]>,
    children: Vec<Vec<u32>>,
    row: Vec<u32>,
}

impl Arena {
    fn push(&mut self, space: [Parameter; DIMENSIONS], children: Vec<u32>, row: u32) -> u32 {
        let id = self.space.len() as u32;
        self.space.push(space);
        self.children.push(children);
        self.row.push(row);
        id
    }

    /// vanilla's own R-tree `comparator(dimension, absolute)`'s sort key: the span's
    /// centre, `(min + max) / 2` in **truncating** integer division (Java's
    /// `/2L` on a `long`, which Rust's `i64` division matches exactly, including
    /// for negatives — both truncate toward zero).
    #[inline]
    fn centre(&self, id: u32, axis: usize, absolute: bool) -> i64 {
        let p = self.space[id as usize][axis];
        let centre = (p.min + p.max) / 2;
        if absolute { centre.abs() } else { centre }
    }

    /// vanilla's own R-tree `sort(children, dimensions, dimension, absolute)` — a
    /// comparator chain starting at `axis` and wrapping through all 7 axes.
    /// **Stable**, matching Java's `List.sort` (TimSort): ties keep the
    /// incoming order, which is part of the resulting structure.
    fn sort_nodes(&self, ids: &mut [u32], axis: usize, absolute: bool) {
        ids.sort_by(|&a, &b| {
            for step in 0..DIMENSIONS {
                let d = (axis + step) % DIMENSIONS;
                let ka = self.centre(a, d, absolute);
                let kb = self.centre(b, d, absolute);
                if ka != kb {
                    return ka.cmp(&kb);
                }
            }
            std::cmp::Ordering::Equal
        });
    }

    /// vanilla's own R-tree `buildParameterSpace` — the per-axis interval hull
    /// (vanilla's own per-axis span: `min` of mins, `max` of maxes) over `ids`.
    ///
    /// # Panics
    /// Panics on an empty `ids`, matching vanilla's `SubTree needs at least one
    /// child`.
    fn hull(&self, ids: &[u32]) -> [Parameter; DIMENSIONS] {
        assert!(!ids.is_empty(), "a subtree needs at least one child");
        let mut bounds = self.space[ids[0] as usize];
        for &id in &ids[1..] {
            let child = &self.space[id as usize];
            for d in 0..DIMENSIONS {
                bounds[d].min = bounds[d].min.min(child[d].min);
                bounds[d].max = bounds[d].max.max(child[d].max);
            }
        }
        bounds
    }

    fn min_row_of(&self, ids: &[u32]) -> u32 {
        ids.iter()
            .map(|&id| self.row[id as usize])
            .min()
            .expect("a subtree needs at least one child")
    }
}

/// vanilla's own R-tree `cost(parameterSpace)` — the summed axis extents of a candidate
/// bucket's hull. Vanilla picks the split axis minimising the total over buckets.
fn cost(space: &[Parameter; DIMENSIONS]) -> i64 {
    space.iter().map(|p| (p.max - p.min).abs()).sum()
}

/// vanilla's own R-tree `bucketize`'s `expectedChildrenCount`, computed in integers.
///
/// Vanilla writes it as
/// `(int) Math.pow(6.0, Math.floor(Math.log(nodes.size() - 0.01) / Math.log(6.0)))`.
/// That is `6^k` for the largest `k` with `6^k <= n - 0.01`, and since `n` is an
/// integer and `6^k` is an integer that condition is exactly `6^k < n`. Computing
/// it that way removes `ln`/`powf` from the *structure* of the tree: `Math.log`
/// and `Math.pow` are only specified to 1 ulp, so a float port would make node
/// layout depend on the host libm at any `n` near a power of six. The equivalence
/// is not asserted by this comment —
/// `bucket_size_matches_vanillas_float_formula_for_every_plausible_n` in
/// `biome_tree_identity.rs` checks both forms agree for every `n` the real table
/// can produce.
fn expected_bucket_size(n: usize) -> usize {
    let mut size = 1usize;
    while size * CHILDREN_PER_NODE < n {
        size *= CHILDREN_PER_NODE;
    }
    size
}

/// vanilla's own R-tree `bucketize(nodes)` — a straight sequential chunking of the
/// already-sorted list into runs of [`expected_bucket_size`], with a short final
/// bucket if one is left over.
fn bucketize(ids: &[u32]) -> Vec<Vec<u32>> {
    let expected = expected_bucket_size(ids.len());
    let mut buckets: Vec<Vec<u32>> = Vec::new();
    let mut current: Vec<u32> = Vec::new();
    for &id in ids {
        current.push(id);
        if current.len() >= expected {
            buckets.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        buckets.push(current);
    }
    buckets
}

/// Vanilla's `Climate.RTree`, including its lifecycle cached last result. See
/// the module doc for the exact lifecycle and tie behavior.
#[derive(Debug, Clone)]
pub(crate) struct BiomeTree {
    nodes: NodeStorage,
    /// Child ids, referenced by `Node::{first_child, child_count}`.
    children: Vec<u32>,
    root: u32,
    offset_zero: bool,
}

impl BiomeTree {
    /// vanilla's own R-tree `create(values)` over the parsed table, in table order.
    ///
    /// # Panics
    /// Panics on an empty table, matching vanilla's `Need at least one value to
    /// build the search tree.`
    pub(crate) fn build(points: &[BiomeParameterPoint]) -> Self {
        assert!(
            !points.is_empty(),
            "need at least one value to build the biome search tree"
        );
        let mut arena = Arena {
            space: Vec::with_capacity(points.len() * 2),
            children: Vec::with_capacity(points.len() * 2),
            row: Vec::with_capacity(points.len() * 2),
        };
        let ids: Vec<u32> = points
            .iter()
            .enumerate()
            .map(|(row, p)| arena.push(p.params, Vec::new(), row as u32))
            .collect();
        let offset_zero = points
            .iter()
            .all(|point| point.params[DIMENSIONS - 1].min == 0
                && point.params[DIMENSIONS - 1].max == 0);
        let root = build_level(&mut arena, ids);
        let mut tree = Self::flatten(&arena, root);
        tree.offset_zero = offset_zero;
        tree
    }

    /// Depth-first flatten of the arena into `nodes`/`children`, preserving child
    /// order — which under vanilla's tie-break is now **load-bearing for the
    /// answer**, not merely for pruning efficiency.
    fn flatten(arena: &Arena, root: u32) -> Self {
        let mut nodes = Vec::with_capacity(arena.space.len());
        let mut children = Vec::with_capacity(arena.space.len());
        let root = flatten_node(&mut nodes, &mut children, arena, root);
        Self {
            nodes: NodeStorage::from_wide(nodes),
            children,
            root,
            offset_zero: false,
        }
    }

    #[inline]
    fn is_leaf(&self, id: u32) -> bool {
        self.nodes.is_leaf(id)
    }

    /// The exact node distance: all seven i64 per-axis squared terms.
    ///
    /// Search uses this uncapped form only where it must retain the selected
    /// distance (the incumbent seed and the single-leaf root). Child visits use
    /// [`Self::bound`] instead, because a child whose partial sum reaches the
    /// incumbent can be pruned without evaluating the remaining axes.
    #[cfg(test)]
    #[inline]
    fn distance(&self, id: u32, target: &[i64; DIMENSIONS]) -> i64 {
        match &self.nodes {
            NodeStorage::Wide(nodes) => distance_wide(&nodes[id as usize].space, target),
            NodeStorage::Narrow(nodes) => {
                if self.offset_zero && target[DIMENSIONS - 1] == 0 {
                    distance_narrow_offset_zero(&nodes[id as usize].space, target)
                } else {
                    distance_narrow(&nodes[id as usize].space, target)
                }
            }
        }
    }

    /// A node distance capped at `cutoff`, preserving the strict pruning rule.
    ///
    /// The return value is exact when it is below `cutoff`; once a partial sum
    /// reaches the cutoff, returning the cutoff is sufficient because callers
    /// descend only for `best_dist > child_bound`. In particular, equality
    /// returns the cutoff and therefore still prunes a tied child, preserving
    /// the incumbent-wins-ties traversal semantics. The terms remain seven
    /// independent i64 squares; only the suffix after a proven prune is skipped.
    #[cfg(test)]
    #[inline]
    fn bound(&self, id: u32, target: &[i64; DIMENSIONS], cutoff: i64) -> i64 {
        match &self.nodes {
            NodeStorage::Wide(nodes) => bound_wide(&nodes[id as usize].space, target, cutoff),
            NodeStorage::Narrow(nodes) => bound_narrow(&nodes[id as usize].space, target, cutoff),
        }
    }

    /// The exact selected node span, used only by invariant controls.
    #[inline]
    fn space(&self, id: u32) -> [Parameter; DIMENSIONS] {
        self.nodes.space(id)
    }

    #[cfg(test)]
    fn uses_narrow_storage(&self) -> bool {
        self.nodes.is_narrow()
    }

    #[cfg(test)]
    fn into_wide_storage(mut self) -> Self {
        self.nodes = self.nodes.into_wide();
        self
    }

    #[inline]
    fn node_metadata(&self, id: u32) -> (u32, u32, u32) {
        self.nodes.metadata(id)
    }

    /// The nearest biome's table row, by the indexed search with no cached
    /// incumbent — the stateless negative-control answer (module doc).
    ///
    /// Bumps the biome-search counters: one search, plus the number of
    /// `Node::distance` evaluations it really performed. That second number is the
    /// D5 quantity — it used to be `searches × table_len` by construction, and the
    /// point of this module is that it no longer is, so it is counted.
    pub(crate) fn nearest_row(&self, target: &[i64; DIMENSIONS]) -> u32 {
        self.search(target, None).0
    }

    /// Search with the cached incumbent supplied by the caller, returning both
    /// the table row and the selected leaf node so the caller can retain the
    /// exact history for the next query.
    pub(crate) fn nearest_row_with_candidate(
        &self,
        target: &[i64; DIMENSIONS],
        candidate: Option<u32>,
    ) -> (u32, u32) {
        let (node, _) = self.search_node(target, candidate);
        (self.nodes.metadata(node).2, node)
    }

    pub(crate) fn nearest_leaf_and_distance(
        &self,
        target: &[i64; DIMENSIONS],
    ) -> (u32, u32, i64) {
        let (node, distance) = self.search_node(target, None);
        (self.nodes.metadata(node).2, node, distance)
    }

    #[inline]
    pub(crate) fn leaf_row_and_distance(
        &self,
        node: u32,
        target: &[i64; DIMENSIONS],
    ) -> (u32, i64) {
        let row = self.nodes.metadata(node).2;
        let distance = match &self.nodes {
            NodeStorage::Wide(nodes) => distance_wide(&nodes[node as usize].space, target),
            NodeStorage::Narrow(nodes) => {
                if self.offset_zero && target[DIMENSIONS - 1] == 0 {
                    distance_narrow_offset_zero(&nodes[node as usize].space, target)
                } else {
                    distance_narrow(&nodes[node as usize].space, target)
                }
            }
        };
        (row, distance)
    }

    /// Vanilla's search with an explicit candidate in place of its lifecycle
    /// cached incumbent — the seeded form used by tie controls.
    pub(crate) fn nearest_row_seeded(
        &self,
        target: &[i64; DIMENSIONS],
        candidate: Option<u32>,
    ) -> u32 {
        self.search(target, candidate).0
    }

    /// `(row, distance)` of the selected leaf. `distance` is exposed so gates can
    /// assert the distance claim separately from the row claim — the two have
    /// different strengths now (module doc).
    pub(crate) fn nearest_row_and_distance(&self, target: &[i64; DIMENSIONS]) -> (u32, i64) {
        self.search(target, None)
    }

    /// Vanilla's own top-level and subtree search routines, transcribed.
    ///
    /// Vanilla threads `closestLeaf` down as each recursive call's `candidate`, and
    /// a recursive call's own `minDistance` is therefore `dist(closestLeaf)` — the
    /// same value the caller holds. So a single running `(best_dist, best_node)`
    /// is exactly equivalent to Java's parameter passing, and it also lets a
    /// subtree's returned leaf skip the redundant `distance(leaf, target)` Java
    /// recomputes. Neither is a semantic change; the counter counts the
    /// evaluations this form actually performs.
    fn search(&self, target: &[i64; DIMENSIONS], candidate: Option<u32>) -> (u32, i64) {
        let (node, distance) = self.search_node(target, candidate);
        (self.nodes.metadata(node).2, distance)
    }

    fn search_node(&self, target: &[i64; DIMENSIONS], candidate: Option<u32>) -> (u32, i64) {
        let (best_node, best_dist, evaluations) = match &self.nodes {
            NodeStorage::Wide(nodes) => search_wide(
                nodes,
                &self.children,
                self.root,
                target,
                candidate,
            ),
            NodeStorage::Narrow(nodes) => {
                if self.offset_zero && target[DIMENSIONS - 1] == 0 {
                    search_narrow::<false, true>(
                        nodes,
                        &self.children,
                        self.root,
                        target,
                        candidate,
                    )
                } else {
                    search_narrow::<false, false>(
                        nodes,
                        &self.children,
                        self.root,
                        target,
                        candidate,
                    )
                }
            }
        };
        debug_assert_ne!(best_node, NONE, "the tree always has at least one leaf");
        crate::counters::bump_biome_search(evaluations);
        (best_node, best_dist)
    }

    /// Every `(parent, child)` pair whose spans violate hull containment on some
    /// axis, as `(parent_id, child_id, axis)`.
    ///
    /// The premise both distance claims rest on (module doc): a subtree's span must
    /// contain each child's, or its `bound` stops lower-bounding the leaves beneath
    /// it and a prune could discard the true nearest biome. Exposed so a gate can
    /// check it **exhaustively over every node/child pair in the real tree**. An
    /// empty return is only meaningful next to a control that perturbs a node and
    /// sees this fire.
    pub(crate) fn hull_containment_violations(&self) -> Vec<(u32, u32, usize)> {
        let mut bad = Vec::new();
        for id in 0..self.nodes.len() as u32 {
            let (first_child, child_count, _) = self.node_metadata(id);
            let parent = self.space(id);
            let first = first_child as usize;
            for slot in first..first + child_count as usize {
                let child = self.children[slot];
                let cs = self.space(child);
                for d in 0..DIMENSIONS {
                    if cs[d].min < parent[d].min || cs[d].max > parent[d].max {
                        bad.push((id as u32, child, d));
                    }
                }
            }
        }
        bad
    }

    /// Total node count, and the leaf count — a shape assertion for the gates
    /// (leaves must equal the table length: no row dropped, none duplicated).
    pub(crate) fn shape(&self) -> (usize, usize) {
        (
            self.nodes.len(),
            (0..self.nodes.len() as u32)
                .filter(|&id| self.is_leaf(id))
                .count(),
        )
    }

    /// The number of nodes, for a gate that wants to perturb one by index.
    pub(crate) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Whether node `id` is a leaf. Nodes are flattened in DFS **pre-order**, so
    /// node 0 is the root and node 1 is its first child.
    pub(crate) fn node_is_leaf(&self, id: usize) -> bool {
        self.is_leaf(id as u32)
    }

    /// The root's child node ids, in vanilla's traversal order.
    ///
    /// Gate support with a specific purpose: a control that breaks a node's bound
    /// only observes anything if that node can actually be **pruned**, and the
    /// root's *first* child never can — `search` starts with `best_dist = i64::MAX`,
    /// so `best_dist > child_bound` is unconditionally true for it. A control must
    /// therefore target a later child. (Measured: collapsing node 1, the first
    /// child, produced zero wrong distances.)
    pub(crate) fn root_child_nodes(&self) -> Vec<u32> {
        let (first_child, child_count, _) = self.node_metadata(self.root);
        let first = first_child as usize;
        self.children[first..first + child_count as usize].to_vec()
    }

    /// Every leaf's node id, in table-row order — so a gate can seed a search with
    /// a specific row's leaf.
    pub(crate) fn leaf_node_for_row(&self, row: u32) -> Option<u32> {
        (0..self.nodes.len() as u32)
            .find(|&id| self.is_leaf(id) && self.node_metadata(id).2 == row)
    }

    /// Shrinks one node's span to a degenerate point — the **control** for
    /// [`Self::hull_containment_violations`] and for the distance-identity gate.
    /// Breaking the lower-bound property is exactly the defect class those gates
    /// exist to detect, so they are only evidence if this makes them fail.
    pub(crate) fn perturb_node_span(&mut self, id: usize) {
        match &mut self.nodes {
            NodeStorage::Wide(nodes) => {
                for d in 0..DIMENSIONS {
                    let p = &mut nodes[id].space[d];
                    let centre = (p.min + p.max) / 2;
                    p.min = centre;
                    p.max = centre;
                }
            }
            NodeStorage::Narrow(nodes) => {
                for d in 0..DIMENSIONS {
                    let p = &mut nodes[id].space[d];
                    let centre = (i64::from(p.min) + i64::from(p.max)) / 2;
                    p.min = centre as i32;
                    p.max = centre as i32;
                }
            }
        }
    }
}

fn search_wide(
    nodes: &[Node],
    children: &[u32],
    root: u32,
    target: &[i64; DIMENSIONS],
    candidate: Option<u32>,
) -> (u32, i64, u64) {
    let mut evaluations = 0u64;
    let mut best_dist = i64::MAX;
    let mut best_node = NONE;
    if let Some(seed) = candidate {
        if let Some(node) = nodes.get(seed as usize) {
            if node.is_leaf() {
                best_dist = distance_wide(&node.space, target);
                best_node = seed;
                evaluations += 1;
            }
        }
    }
    let root_node = &nodes[root as usize];
    if root_node.is_leaf() {
        let distance = distance_wide(&root_node.space, target);
        evaluations += 1;
        if best_dist > distance {
            best_dist = distance;
            best_node = root;
        }
    } else {
        visit_wide(
            nodes,
            children,
            root,
            target,
            &mut best_dist,
            &mut best_node,
            &mut evaluations,
        );
    }
    (best_node, best_dist, evaluations)
}

fn visit_wide(
    nodes: &[Node],
    children: &[u32],
    id: u32,
    target: &[i64; DIMENSIONS],
    best_dist: &mut i64,
    best_node: &mut u32,
    evaluations: &mut u64,
) {
    let node = &nodes[id as usize];
    let first = node.first_child as usize;
    let end = first + node.child_count as usize;
    for &child in &children[first..end] {
        let child_node = &nodes[child as usize];
        let child_bound = bound_wide(&child_node.space, target, *best_dist);
        *evaluations += 1;
        if *best_dist > child_bound {
            if child_node.is_leaf() {
                *best_dist = child_bound;
                *best_node = child;
            } else {
                visit_wide(
                    nodes,
                    children,
                    child,
                    target,
                    best_dist,
                    best_node,
                    evaluations,
                );
            }
        }
    }
}

fn search_narrow<const RECORD: bool, const OFFSET_ZERO: bool>(
    nodes: &[NarrowNode],
    children: &[u32],
    root: u32,
    target: &[i64; DIMENSIONS],
    candidate: Option<u32>,
) -> (u32, i64, u64) {
    let mut evaluations = 0u64;
    let mut best_dist = i64::MAX;
    let mut best_node = NONE;
    if let Some(seed) = candidate {
        if let Some(node) = nodes.get(seed as usize) {
            if node.child_count == 0 {
                best_dist = if OFFSET_ZERO {
                    distance_narrow_offset_zero(&node.space, target)
                } else {
                    distance_narrow(&node.space, target)
                };
                best_node = seed;
                evaluations += 1;
            }
        }
    }
    let root_node = &nodes[root as usize];
    if root_node.child_count == 0 {
        let distance = if OFFSET_ZERO {
            distance_narrow_offset_zero(&root_node.space, target)
        } else {
            distance_narrow(&root_node.space, target)
        };
        evaluations += 1;
        if best_dist > distance {
            best_dist = distance;
            best_node = root;
        }
    } else {
        visit_narrow::<RECORD, OFFSET_ZERO>(
            nodes,
            children,
            root,
            target,
            &mut best_dist,
            &mut best_node,
            &mut evaluations,
        );
    }
    (best_node, best_dist, evaluations)
}

fn visit_narrow<const RECORD: bool, const OFFSET_ZERO: bool>(
    nodes: &[NarrowNode],
    children: &[u32],
    id: u32,
    target: &[i64; DIMENSIONS],
    best_dist: &mut i64,
    best_node: &mut u32,
    evaluations: &mut u64,
) {
    let node = &nodes[id as usize];
    let first = node.first_child as usize;
    let end = first + node.child_count as usize;
    for &child in &children[first..end] {
        let child_node = &nodes[child as usize];
        let child_bound = if OFFSET_ZERO {
            bound_narrow_offset_zero(&child_node.space, target, *best_dist)
        } else if RECORD {
            bound_narrow_recording(&child_node.space, target, *best_dist)
        } else {
            bound_narrow(&child_node.space, target, *best_dist)
        };
        *evaluations += 1;
        if *best_dist > child_bound {
            if child_node.child_count == 0 {
                *best_dist = child_bound;
                *best_node = child;
            } else {
                visit_narrow::<RECORD, OFFSET_ZERO>(
                    nodes,
                    children,
                    child,
                    target,
                    best_dist,
                    best_node,
                    evaluations,
                );
            }
        }
    }
}

fn flatten_node(
    nodes: &mut Vec<Node>,
    children: &mut Vec<u32>,
    arena: &Arena,
    id: u32,
) -> u32 {
    let kids = &arena.children[id as usize];
    let me = nodes.len() as u32;
    nodes.push(Node {
        space: arena.space[id as usize],
        first_child: 0,
        child_count: kids.len() as u32,
        row: arena.row[id as usize],
    });
    if kids.is_empty() {
        return me;
    }
    let first = children.len() as u32;
    children.extend(std::iter::repeat_n(0u32, kids.len()));
    nodes[me as usize].first_child = first;
    for (i, &kid) in kids.iter().enumerate() {
        let flat = flatten_node(nodes, children, arena, kid);
        children[first as usize + i] = flat;
    }
    me
}

#[inline]
fn axis_distance(min: i64, max: i64, target: i64) -> i64 {
    let above = target - max;
    let below = min - target;
    if above > 0 { above } else { below.max(0) }
}

#[cfg(all(test, feature = "gen-counters"))]
#[inline]
fn axis_distance_branchless(min: i64, max: i64, target: i64) -> i64 {
    (target - max).max(min - target).max(0)
}

#[inline]
fn distance_wide(space: &[Parameter; DIMENSIONS], target: &[i64; DIMENSIONS]) -> i64 {
    let d0 = axis_distance(space[0].min, space[0].max, target[0]);
    let d1 = axis_distance(space[1].min, space[1].max, target[1]);
    let d2 = axis_distance(space[2].min, space[2].max, target[2]);
    let d3 = axis_distance(space[3].min, space[3].max, target[3]);
    let d4 = axis_distance(space[4].min, space[4].max, target[4]);
    let d5 = axis_distance(space[5].min, space[5].max, target[5]);
    let d6 = axis_distance(space[6].min, space[6].max, target[6]);
    d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3 + d4 * d4 + d5 * d5 + d6 * d6
}

#[inline]
fn distance_narrow(
    space: &[NarrowParameter; DIMENSIONS],
    target: &[i64; DIMENSIONS],
) -> i64 {
    let d0 = axis_distance(i64::from(space[0].min), i64::from(space[0].max), target[0]);
    let d1 = axis_distance(i64::from(space[1].min), i64::from(space[1].max), target[1]);
    let d2 = axis_distance(i64::from(space[2].min), i64::from(space[2].max), target[2]);
    let d3 = axis_distance(i64::from(space[3].min), i64::from(space[3].max), target[3]);
    let d4 = axis_distance(i64::from(space[4].min), i64::from(space[4].max), target[4]);
    let d5 = axis_distance(i64::from(space[5].min), i64::from(space[5].max), target[5]);
    let d6 = axis_distance(i64::from(space[6].min), i64::from(space[6].max), target[6]);
    d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3 + d4 * d4 + d5 * d5 + d6 * d6
}

#[inline]
fn distance_narrow_offset_zero(
    space: &[NarrowParameter; DIMENSIONS],
    target: &[i64; DIMENSIONS],
) -> i64 {
    let d0 = axis_distance(i64::from(space[0].min), i64::from(space[0].max), target[0]);
    let d1 = axis_distance(i64::from(space[1].min), i64::from(space[1].max), target[1]);
    let d2 = axis_distance(i64::from(space[2].min), i64::from(space[2].max), target[2]);
    let d3 = axis_distance(i64::from(space[3].min), i64::from(space[3].max), target[3]);
    let d4 = axis_distance(i64::from(space[4].min), i64::from(space[4].max), target[4]);
    let d5 = axis_distance(i64::from(space[5].min), i64::from(space[5].max), target[5]);
    d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3 + d4 * d4 + d5 * d5
}

macro_rules! bound_distance {
    ($cutoff:expr, $d0:expr, $d1:expr, $d2:expr, $d3:expr, $d4:expr, $d5:expr, $d6:expr $(,)?) => {{
        let cutoff = $cutoff;
        let d0 = $d0;
        let mut sum = d0 * d0;
        if sum >= cutoff {
            return cutoff;
        }
        let d1 = $d1;
        sum += d1 * d1;
        if sum >= cutoff {
            return cutoff;
        }
        let d2 = $d2;
        sum += d2 * d2;
        if sum >= cutoff {
            return cutoff;
        }
        let d3 = $d3;
        sum += d3 * d3;
        if sum >= cutoff {
            return cutoff;
        }
        let d4 = $d4;
        sum += d4 * d4;
        if sum >= cutoff {
            return cutoff;
        }
        let d5 = $d5;
        sum += d5 * d5;
        if sum >= cutoff {
            return cutoff;
        }
        let d6 = $d6;
        sum += d6 * d6;
        sum
    }};
}

#[inline]
fn bound_wide(
    space: &[Parameter; DIMENSIONS],
    target: &[i64; DIMENSIONS],
    cutoff: i64,
) -> i64 {
    bound_distance!(
        cutoff,
        axis_distance(space[0].min, space[0].max, target[0]),
        axis_distance(space[1].min, space[1].max, target[1]),
        axis_distance(space[2].min, space[2].max, target[2]),
        axis_distance(space[3].min, space[3].max, target[3]),
        axis_distance(space[4].min, space[4].max, target[4]),
        axis_distance(space[5].min, space[5].max, target[5]),
        axis_distance(space[6].min, space[6].max, target[6]),
    )
}

macro_rules! bound_narrow_distance {
    ($record:expr, $cutoff:expr, $d0:expr, $d1:expr, $d2:expr, $d3:expr, $d4:expr, $d5:expr, $d6:expr $(,)?) => {{
        let cutoff = $cutoff;
        let d0 = $d0;
        let mut sum = d0 * d0;
        if sum >= cutoff {
            if $record { record_narrow_cutoff(0); }
            return cutoff;
        }
        let d1 = $d1;
        sum += d1 * d1;
        if sum >= cutoff {
            if $record { record_narrow_cutoff(1); }
            return cutoff;
        }
        let d2 = $d2;
        sum += d2 * d2;
        if sum >= cutoff {
            if $record { record_narrow_cutoff(2); }
            return cutoff;
        }
        let d3 = $d3;
        sum += d3 * d3;
        if sum >= cutoff {
            if $record { record_narrow_cutoff(3); }
            return cutoff;
        }
        let d4 = $d4;
        sum += d4 * d4;
        if sum >= cutoff {
            if $record { record_narrow_cutoff(4); }
            return cutoff;
        }
        let d5 = $d5;
        sum += d5 * d5;
        if sum >= cutoff {
            if $record { record_narrow_cutoff(5); }
            return cutoff;
        }
        let d6 = $d6;
        sum += d6 * d6;
        if $record { record_narrow_cutoff(DIMENSIONS); }
        sum
    }};
}

macro_rules! bound_narrow_distance_without_offset {
    ($cutoff:expr, $d0:expr, $d1:expr, $d2:expr, $d3:expr, $d4:expr, $d5:expr $(,)?) => {{
        let cutoff = $cutoff;
        let d0 = $d0;
        let mut sum = d0 * d0;
        if sum >= cutoff {
            return cutoff;
        }
        let d1 = $d1;
        sum += d1 * d1;
        if sum >= cutoff {
            return cutoff;
        }
        let d2 = $d2;
        sum += d2 * d2;
        if sum >= cutoff {
            return cutoff;
        }
        let d3 = $d3;
        sum += d3 * d3;
        if sum >= cutoff {
            return cutoff;
        }
        let d4 = $d4;
        sum += d4 * d4;
        if sum >= cutoff {
            return cutoff;
        }
        let d5 = $d5;
        sum += d5 * d5;
        sum
    }};
}

#[inline]
fn bound_narrow(
    space: &[NarrowParameter; DIMENSIONS],
    target: &[i64; DIMENSIONS],
    cutoff: i64,
) -> i64 {
    bound_narrow_distance!(
        false,
        cutoff,
        axis_distance(i64::from(space[4].min), i64::from(space[4].max), target[4]),
        axis_distance(i64::from(space[5].min), i64::from(space[5].max), target[5]),
        axis_distance(i64::from(space[2].min), i64::from(space[2].max), target[2]),
        axis_distance(i64::from(space[0].min), i64::from(space[0].max), target[0]),
        axis_distance(i64::from(space[1].min), i64::from(space[1].max), target[1]),
        axis_distance(i64::from(space[3].min), i64::from(space[3].max), target[3]),
        axis_distance(i64::from(space[6].min), i64::from(space[6].max), target[6]),
    )
}

#[inline]
fn bound_narrow_offset_zero(
    space: &[NarrowParameter; DIMENSIONS],
    target: &[i64; DIMENSIONS],
    cutoff: i64,
) -> i64 {
    bound_narrow_distance_without_offset!(
        cutoff,
        axis_distance(i64::from(space[4].min), i64::from(space[4].max), target[4]),
        axis_distance(i64::from(space[5].min), i64::from(space[5].max), target[5]),
        axis_distance(i64::from(space[2].min), i64::from(space[2].max), target[2]),
        axis_distance(i64::from(space[0].min), i64::from(space[0].max), target[0]),
        axis_distance(i64::from(space[1].min), i64::from(space[1].max), target[1]),
        axis_distance(i64::from(space[3].min), i64::from(space[3].max), target[3]),
    )
}

#[inline]
fn bound_narrow_recording(
    space: &[NarrowParameter; DIMENSIONS],
    target: &[i64; DIMENSIONS],
    cutoff: i64,
) -> i64 {
    bound_narrow_distance!(
        true,
        cutoff,
        axis_distance(i64::from(space[4].min), i64::from(space[4].max), target[4]),
        axis_distance(i64::from(space[5].min), i64::from(space[5].max), target[5]),
        axis_distance(i64::from(space[2].min), i64::from(space[2].max), target[2]),
        axis_distance(i64::from(space[0].min), i64::from(space[0].max), target[0]),
        axis_distance(i64::from(space[1].min), i64::from(space[1].max), target[1]),
        axis_distance(i64::from(space[3].min), i64::from(space[3].max), target[3]),
        axis_distance(i64::from(space[6].min), i64::from(space[6].max), target[6]),
    )
}

#[cfg(test)]
#[inline]
fn bound_narrow_canonical(
    space: &[NarrowParameter; DIMENSIONS],
    target: &[i64; DIMENSIONS],
    cutoff: i64,
) -> i64 {
    bound_narrow_distance!(
        false,
        cutoff,
        axis_distance(i64::from(space[0].min), i64::from(space[0].max), target[0]),
        axis_distance(i64::from(space[1].min), i64::from(space[1].max), target[1]),
        axis_distance(i64::from(space[2].min), i64::from(space[2].max), target[2]),
        axis_distance(i64::from(space[3].min), i64::from(space[3].max), target[3]),
        axis_distance(i64::from(space[4].min), i64::from(space[4].max), target[4]),
        axis_distance(i64::from(space[5].min), i64::from(space[5].max), target[5]),
        axis_distance(i64::from(space[6].min), i64::from(space[6].max), target[6]),
    )
}

#[cfg(all(test, feature = "gen-counters"))]
#[inline]
fn bound_narrow_branchful(
    space: &[NarrowParameter; DIMENSIONS],
    target: &[i64; DIMENSIONS],
    cutoff: i64,
) -> i64 {
    bound_narrow_distance!(
        false,
        cutoff,
        axis_distance(i64::from(space[0].min), i64::from(space[0].max), target[0]),
        axis_distance(i64::from(space[1].min), i64::from(space[1].max), target[1]),
        axis_distance(i64::from(space[2].min), i64::from(space[2].max), target[2]),
        axis_distance(i64::from(space[3].min), i64::from(space[3].max), target[3]),
        axis_distance(i64::from(space[4].min), i64::from(space[4].max), target[4]),
        axis_distance(i64::from(space[5].min), i64::from(space[5].max), target[5]),
        axis_distance(i64::from(space[6].min), i64::from(space[6].max), target[6]),
    )
}

#[cfg(all(test, feature = "gen-counters"))]
#[inline]
fn bound_narrow_branchless(
    space: &[NarrowParameter; DIMENSIONS],
    target: &[i64; DIMENSIONS],
    cutoff: i64,
) -> i64 {
    bound_narrow_distance!(
        false,
        cutoff,
        axis_distance_branchless(i64::from(space[0].min), i64::from(space[0].max), target[0]),
        axis_distance_branchless(i64::from(space[1].min), i64::from(space[1].max), target[1]),
        axis_distance_branchless(i64::from(space[2].min), i64::from(space[2].max), target[2]),
        axis_distance_branchless(i64::from(space[3].min), i64::from(space[3].max), target[3]),
        axis_distance_branchless(i64::from(space[4].min), i64::from(space[4].max), target[4]),
        axis_distance_branchless(i64::from(space[5].min), i64::from(space[5].max), target[5]),
        axis_distance_branchless(i64::from(space[6].min), i64::from(space[6].max), target[6]),
    )
}

/// vanilla's own R-tree `build(dimensions, children)` — one level of the recursion,
/// ported statement for statement. `ids` is consumed and re-sorted in place, the
/// way vanilla re-sorts its `List` seven times.
fn build_level(arena: &mut Arena, mut ids: Vec<u32>) -> u32 {
    assert!(!ids.is_empty(), "need at least one child to build a node");
    if ids.len() == 1 {
        return ids[0];
    }
    if ids.len() <= CHILDREN_PER_NODE {
        // Vanilla's small-node sort: a single key, the summed absolute span
        // centres over all 7 axes. Stable, so equal keys keep table order.
        let mut keyed: Vec<(i64, u32)> = ids
            .iter()
            .map(|&id| {
                let total: i64 = (0..DIMENSIONS)
                    .map(|d| {
                        let p = arena.space[id as usize][d];
                        ((p.min + p.max) / 2).abs()
                    })
                    .sum();
                (total, id)
            })
            .collect();
        keyed.sort_by_key(|&(k, _)| k);
        let sorted: Vec<u32> = keyed.into_iter().map(|(_, id)| id).collect();
        let space = arena.hull(&sorted);
        let min_row = arena.min_row_of(&sorted);
        return arena.push(space, sorted, min_row);
    }

    let mut min_cost = i64::MAX;
    let mut min_axis = usize::MAX;
    let mut min_buckets: Vec<Vec<u32>> = Vec::new();
    for axis in 0..DIMENSIONS {
        arena.sort_nodes(&mut ids, axis, false);
        let buckets = bucketize(&ids);
        let total: i64 = buckets.iter().map(|b| cost(&arena.hull(b))).sum();
        // Strict `>`, so the lowest axis wins a cost tie — vanilla's
        // `if (minCost > totalCost)`.
        if min_cost > total {
            min_cost = total;
            min_axis = axis;
            min_buckets = buckets;
        }
    }

    // Vanilla sorts the chosen buckets *as nodes* (by their hulls' centres, this
    // time absolute) before recursing, so bucket order is part of the structure.
    let mut wrappers: Vec<u32> = min_buckets
        .iter()
        .map(|b| {
            let space = arena.hull(b);
            let min_row = arena.min_row_of(b);
            arena.push(space, b.clone(), min_row)
        })
        .collect();
    arena.sort_nodes(&mut wrappers, min_axis, true);

    let children: Vec<u32> = wrappers
        .into_iter()
        .map(|w| {
            let kids = arena.children[w as usize].clone();
            build_level(arena, kids)
        })
        .collect();
    let space = arena.hull(&children);
    let min_row = arena.min_row_of(&children);
    arena.push(space, children, min_row)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "gen-counters")]
    use std::hint::black_box;
    #[cfg(feature = "gen-counters")]
    use lodestone_time::Instant;

    /// The integer rewrite of `bucketize`'s `expectedChildrenCount` must agree
    /// with vanilla's float expression at every size the recursion can present.
    /// Checked over the whole range rather than at a few points, because the only
    /// way the two forms can differ is a floor landing on the wrong side of an
    /// integer, and that is a property of individual `n`.
    #[test]
    fn bucket_size_matches_vanillas_float_formula_for_every_plausible_n() {
        for n in 2..100_000usize {
            let vanilla = 6.0f64.powf(((n as f64 - 0.01).ln() / 6.0f64.ln()).floor()) as usize;
            assert_eq!(
                expected_bucket_size(n),
                vanilla,
                "expected_bucket_size disagrees with vanilla's float formula at n = {n}"
            );
        }
    }

    fn point(values: [(i64, i64); DIMENSIONS]) -> BiomeParameterPoint {
        BiomeParameterPoint {
            params: values.map(|(min, max)| Parameter { min, max }),
            biome: String::new(),
        }
    }

    /// A synthetic table wide enough to force several levels of bucketing
    /// (`> 6` children at the root, so the axis-choosing branch runs).
    fn spread_table(n: usize) -> Vec<BiomeParameterPoint> {
        (0..n)
            .map(|i| {
                let t = (i as i64 * 7919) % 20001 - 10000;
                let h = (i as i64 * 6301) % 20001 - 10000;
                let c = (i as i64 * 4909) % 20001 - 10000;
                point([
                    (t, t + 100),
                    (h, h + 100),
                    (c, c + 100),
                    (-10000, 10000),
                    (-10000, 10000),
                    (-10000, 10000),
                    (0, 0),
                ])
            })
            .collect()
    }

    #[test]
    fn every_table_row_becomes_exactly_one_leaf() {
        for n in [1usize, 2, 6, 7, 36, 37, 216, 500] {
            let table = spread_table(n);
            let tree = BiomeTree::build(&table);
            let (_, leaves) = tree.shape();
            assert_eq!(leaves, n, "leaf count must equal the table length at n = {n}");
            assert!(
                tree.hull_containment_violations().is_empty(),
                "hull containment must hold at n = {n}"
            );
        }
    }

    #[test]
    fn cutoff_bound_is_exact_below_cutoff_and_clamped_after_partial_sum() {
        let tree = BiomeTree::build(&spread_table(37));
        let target = [1234, -2345, 3456, 0, 0, 0, 0];
        let mut saw_partial_cutoff = false;
        for id in 0..tree.node_count() as u32 {
            let exact = tree.distance(id, &target);
            assert_eq!(
                tree.bound(id, &target, exact.saturating_add(1)),
                exact,
                "a cutoff above the exact seven-term distance must not change it for node {id}"
            );
            assert_eq!(
                tree.bound(id, &target, exact),
                exact,
                "an equal cutoff must remain equal so strict pruning keeps its semantics"
            );
            if exact > 1 {
                let cutoff = exact - 1;
                assert_eq!(
                    tree.bound(id, &target, cutoff),
                    cutoff,
                    "a partial sum above the cutoff must return the cutoff for node {id}"
                );
                saw_partial_cutoff = true;
            }
        }
        assert!(
            saw_partial_cutoff,
            "the control must exercise a real cutoff rather than only exact distances"
        );
    }

    #[test]
    fn permuted_narrow_bound_matches_canonical_order_for_safe_cutoffs() {
        let space = [
            NarrowParameter { min: -17, max: 23 },
            NarrowParameter { min: 41, max: 53 },
            NarrowParameter { min: -71, max: -61 },
            NarrowParameter { min: 89, max: 97 },
            NarrowParameter { min: -113, max: -101 },
            NarrowParameter { min: 127, max: 131 },
            NarrowParameter { min: -149, max: -139 },
        ];
        let target = [1_000_000_000, -1_000_000_000, 700_000_000, -500_000_000,
            300_000_000, -200_000_000, 100_000_000];
        let full = distance_narrow(&space, &target);
        for (label, cutoff) in [
            ("equality", full),
            ("mid-prefix", 17_i64),
            ("full", i64::MAX),
        ] {
            assert_eq!(
                bound_narrow(&space, &target, cutoff),
                bound_narrow_canonical(&space, &target, cutoff),
                "permuted bound changed {label} result"
            );
        }

        let extreme_target = [1_000_000_000; DIMENSIONS];
        let extreme_cutoff = distance_narrow(&space, &extreme_target);
        assert_eq!(
            bound_narrow(&space, &extreme_target, extreme_cutoff),
            bound_narrow_canonical(&space, &extreme_target, extreme_cutoff),
            "permuted bound changed the extreme equality result"
        );
    }

    #[test]
    fn omitted_offset_axis_is_a_negative_control_outside_its_precondition() {
        let space = [NarrowParameter { min: 0, max: 0 }; DIMENSIONS];
        let target = [0, 0, 0, 0, 0, 0, 7];
        let cutoff = 100;
        let complete = bound_narrow(&space, &target, cutoff);
        let omitted = bound_narrow_offset_zero(&space, &target, cutoff);
        assert_eq!(complete, 49);
        assert_ne!(complete, omitted, "omitting axis 6 must be observable");
    }

    #[test]
    fn zero_offset_distance_matches_the_complete_distance() {
        let space = [
            NarrowParameter { min: -17, max: 23 },
            NarrowParameter { min: 41, max: 53 },
            NarrowParameter { min: -71, max: -61 },
            NarrowParameter { min: 89, max: 97 },
            NarrowParameter { min: -113, max: -101 },
            NarrowParameter { min: 127, max: 131 },
            NarrowParameter { min: 0, max: 0 },
        ];
        for target in [
            [0, 0, 0, 0, 0, 0, 0],
            [1_000_000, -2_000_000, 3_000_000, -4_000_000, 5_000_000, -6_000_000, 0],
        ] {
            assert_eq!(
                distance_narrow(&space, &target),
                distance_narrow_offset_zero(&space, &target)
            );
        }
    }

    #[test]
    fn visit_prunes_equal_child_bound_and_keeps_first_tied_row() {
        let first = point([(0, 0); DIMENSIONS]);
        let second = point([(0, 0); DIMENSIONS]);
        let tree = BiomeTree::build(&[first, second]);
        let target = [0; DIMENSIONS];
        assert_eq!(tree.nearest_row_and_distance(&target), (0, 0));
    }

    #[test]
    fn hull_containment_control_fires_when_a_node_is_shrunk() {
        let table = spread_table(300);
        let mut tree = BiomeTree::build(&table);
        assert!(tree.hull_containment_violations().is_empty());
        let root = tree.root as usize;
        tree.perturb_node_span(root);
        assert!(
            !tree.hull_containment_violations().is_empty(),
            "shrinking the root's span must be reported as a containment violation"
        );
    }

    /// The distance claim: vanilla's search always lands on the same *minimum
    /// squared distance* brute force finds, even where the chosen row differs.
    #[test]
    fn the_minimum_distance_always_matches_brute_force_on_a_synthetic_table() {
        let table = spread_table(400);
        let tree = BiomeTree::build(&table);
        let mut checked = 0u32;
        for t in (-11000..=11000).step_by(311) {
            for h in (-11000..=11000).step_by(701) {
                for c in (-11000..=11000).step_by(1301) {
                    let target = [t, h, c, 0, 0, 0, 0];
                    let brute = super::super::nearest_row_brute_force(&table, &target);
                    let (_, dist) = tree.nearest_row_and_distance(&target);
                    assert_eq!(
                        dist,
                        table[brute as usize].fitness(&target),
                        "tree found a different minimum distance at {target:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 5_000, "the sweep must actually run: {checked}");
    }

    /// A seed can never change the *distance* — the first of the module doc's two
    /// `lastResult` claims, on a synthetic table.
    #[test]
    fn no_seed_can_change_the_minimum_distance() {
        let table = spread_table(400);
        let tree = BiomeTree::build(&table);
        let nodes = tree.node_count() as u32;
        for t in (-11000..=11000).step_by(997) {
            let target = [t, t / 2, -t, 0, 0, 0, 0];
            let brute = super::super::nearest_row_brute_force(&table, &target);
            let expected = table[brute as usize].fitness(&target);
            for seed in [None, Some(0), Some(1), Some(nodes - 1)] {
                let row = tree.nearest_row_seeded(&target, seed);
                assert_eq!(
                    table[row as usize].fitness(&target),
                    expected,
                    "seed {seed:?} changed the minimum distance at {target:?}"
                );
            }
        }
    }

    #[test]
    fn bounded_tables_use_narrow_storage_without_changing_large_target_results() {
        let table = spread_table(400);
        let narrow = BiomeTree::build(&table);
        assert!(narrow.uses_narrow_storage());
        let wide = BiomeTree::build(&table).into_wide_storage();
        let targets = [
            [100_000_000, -100_000_001, 99_999_997, 0, 0, 0, 0],
            [i32::MAX as i64 - 10_000, 0, 0, 0, 0, 0, 0],
            [-i32::MAX as i64 + 10_000, 0, 0, 0, 0, 0, 0],
        ];
        for target in targets {
            assert_eq!(
                narrow.nearest_row_and_distance(&target),
                wide.nearest_row_and_distance(&target),
                "narrow storage changed indexed output at {target:?}"
            );
        }
        let tie = [0; DIMENSIONS];
        assert_eq!(
            narrow.nearest_row_and_distance(&tie),
            wide.nearest_row_and_distance(&tie),
            "narrow storage changed the unseeded tied row or distance"
        );
        let seed = narrow.leaf_node_for_row(1);
        assert_eq!(
            narrow.nearest_row_seeded(&tie, seed),
            wide.nearest_row_seeded(&tie, seed),
            "narrow storage changed seeded tie identity"
        );
    }

    #[test]
    fn narrow_nodes_fit_one_cache_line_and_preserve_tree_shape() {
        assert_eq!(std::mem::size_of::<NarrowNode>(), 64);
        let table = spread_table(37);
        let narrow = BiomeTree::build(&table);
        let wide = BiomeTree::build(&table).into_wide_storage();
        let NodeStorage::Narrow(nodes) = &narrow.nodes else {
            panic!("bounded table must use narrow storage");
        };

        assert_eq!(narrow.shape(), wide.shape());
        assert_eq!(narrow.children, wide.children);
        for id in 0..narrow.node_count() as u32 {
            let (first, count, row) = narrow.node_metadata(id);
            let (wide_first, wide_count, wide_row) = wide.node_metadata(id);
            assert_eq!(count, wide_count, "node {id} changed its child count");
            if count == 0 {
                assert_eq!(first, row, "leaf {id} must carry its row in first_child");
                assert_eq!(row, wide_row, "leaf {id} changed its row");
            } else {
                assert_eq!(first, wide_first, "internal node {id} changed its offset");
                assert!(
                    (first as usize).saturating_add(count as usize) <= narrow.children.len(),
                    "internal node {id} points outside the child array"
                );
            }
            assert_eq!(nodes[id as usize].child_count, count);
        }
    }

    #[test]
    fn narrow_and_wide_searches_match_across_a_seeded_query_sequence() {
        let table = spread_table(400);
        let narrow = BiomeTree::build(&table);
        let wide = BiomeTree::build(&table).into_wide_storage();
        let targets = [
            [0, 0, 0, 0, 0, 0, 0],
            [7919, -6301, 4909, 0, 0, 0, 0],
            [-10_000, 10_000, -10_000, 0, 0, 0, 0],
            [100_000_000, -100_000_001, 99_999_997, 0, 0, 0, 0],
        ];
        let mut narrow_candidate = None;
        let mut wide_candidate = None;
        for target in targets {
            let (narrow_row, narrow_node) =
                narrow.nearest_row_with_candidate(&target, narrow_candidate);
            let (wide_row, wide_node) = wide.nearest_row_with_candidate(&target, wide_candidate);
            assert_eq!(narrow_row, wide_row, "row changed at {target:?}");
            assert_eq!(
                narrow.distance(narrow_node, &target),
                wide.distance(wide_node, &target),
                "distance changed at {target:?}"
            );
            narrow_candidate = Some(narrow_node);
            wide_candidate = Some(wide_node);
        }
    }

    #[test]
    fn corrupted_narrow_leaf_row_is_detected_by_wide_negative_control() {
        let table = [point([(0, 0); DIMENSIONS]), point([(0, 0); DIMENSIONS])];
        let mut narrow = BiomeTree::build(&table);
        let wide = BiomeTree::build(&table).into_wide_storage();
        let leaf = narrow
            .leaf_node_for_row(0)
            .expect("row zero must have a leaf") as usize;
        assert_eq!(narrow.nearest_row_and_distance(&[0; DIMENSIONS]), (0, 0));

        let NodeStorage::Narrow(nodes) = &mut narrow.nodes else {
            panic!("bounded table must use narrow storage");
        };
        nodes[leaf].first_child = 1;

        let expected = wide.nearest_row_and_distance(&[0; DIMENSIONS]);
        let corrupted = narrow.nearest_row_and_distance(&[0; DIMENSIONS]);
        assert_eq!(expected, (0, 0));
        assert_ne!(
            corrupted, expected,
            "the deliberate row corruption must be observable"
        );
    }

    #[test]
    fn narrow_dispatch_preserves_tied_row_and_distance_for_large_targets() {
        let table = [point([(0, 0); DIMENSIONS]), point([(0, 0); DIMENSIONS])];
        let narrow = BiomeTree::build(&table);
        let wide = BiomeTree::build(&table).into_wide_storage();
        let target = [100_000_000; DIMENSIONS];
        let expected = (0, 7 * 100_000_000_i64 * 100_000_000_i64);
        assert_eq!(narrow.nearest_row_and_distance(&target), expected);
        assert_eq!(wide.nearest_row_and_distance(&target), expected);

        let seed = narrow.leaf_node_for_row(1);
        assert_eq!(narrow.nearest_row_seeded(&target, seed), 1);
        assert_eq!(wide.nearest_row_seeded(&target, seed), 1);
    }

    #[test]
    fn a_table_bound_outside_i32_keeps_the_wide_fallback() {
        let mut table = spread_table(37);
        table[0].params[0] = Parameter {
            min: i64::from(i32::MAX) + 1,
            max: i64::from(i32::MAX) + 1,
        };
        let tree = BiomeTree::build(&table);
        assert!(!tree.uses_narrow_storage());
        for target in [[0, 0, 0, 0, 0, 0, 0], [100_000_000, 0, 0, 0, 0, 0, 0]] {
            let (row, distance) = tree.nearest_row_and_distance(&target);
            let brute = super::super::nearest_row_brute_force(&table, &target);
            assert_eq!(row, brute);
            assert_eq!(distance, table[brute as usize].fitness(&target));
        }
    }

    #[cfg(feature = "gen-counters")]
    #[test]
    fn branchless_axis_distance_is_identical_at_boundary_and_extreme_inputs() {
        let values = [
            -1_000_000_000,
            -1,
            0,
            1,
            1_000_000_000,
        ];
        for min in values {
            for max in values {
                if min > max {
                    continue;
                }
                for target in values {
                    assert_eq!(
                        axis_distance(min, max, target),
                        axis_distance_branchless(min, max, target),
                        "axis distance differs for [{min}, {max}] at {target}"
                    );
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "gen-counters")]
    #[ignore = "explicit production-data performance probe"]
    fn production_tree_cutoff_axis_histogram_and_branchless_control() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../lodestone-server/assets/worldgen/biome_parameters/overworld.json"
        ))
        .expect("embedded production biome table must be valid JSON");
        let table = super::super::parse_table(&value);
        let tree = BiomeTree::build(&table);
        let NodeStorage::Narrow(nodes) = &tree.nodes else {
            panic!("production biome table must fit narrow tree storage");
        };

        let mut state = 0x9e37_79b9_u64;
        let mut samples = Vec::with_capacity(100_000);
        for _ in 0..100_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let node = &nodes[(state as usize) % nodes.len()];
            state = state.rotate_left(17);
            let target = std::array::from_fn(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                ((state >> 16) as i64 % 24_001) - 12_000
            });
            state = state.rotate_left(23);
            let cutoff = ((state >> 8) as i64 % 200_000_001).max(1);
            samples.push((node.space, target, cutoff));
        }

        let mut branchful_sum = 0i64;
        let started = Instant::now();
        for (space, target, cutoff) in &samples {
            branchful_sum ^= black_box(bound_narrow_branchful(space, target, *cutoff));
        }
        let branchful_elapsed = started.elapsed();

        let mut branchless_sum = 0i64;
        let started = Instant::now();
        for (space, target, cutoff) in &samples {
            branchless_sum ^= black_box(bound_narrow_branchless(space, target, *cutoff));
        }
        let branchless_elapsed = started.elapsed();
        assert_eq!(branchful_sum, branchless_sum, "branchless control changed bounds");

        BOUND_NARROW_AXIS_HISTOGRAM.with(|histogram| histogram.set([0; DIMENSIONS + 1]));
        BOUND_NARROW_AXIS_HISTOGRAM_ENABLED.with(|enabled| enabled.set(true));
        let mut rows = 0u32;
        for (_, target, _) in samples.iter().take(20_000) {
            let (node, _, _) = search_narrow::<true, false>(
                nodes,
                &tree.children,
                tree.root,
                target,
                None,
            );
            rows ^= black_box(node);
        }
        let histogram = BOUND_NARROW_AXIS_HISTOGRAM.with(Cell::get);
        BOUND_NARROW_AXIS_HISTOGRAM_ENABLED.with(|enabled| enabled.set(false));

        println!(
            "biome tree bound probe: samples={} branchful={:?} branchless={:?} sums={branchful_sum}/{branchless_sum} search_rows={rows} histogram={histogram:?}",
            samples.len(), branchful_elapsed, branchless_elapsed
        );
    }

    #[cfg(feature = "gen-counters")]
    #[test]
    #[ignore = "explicit instruction counter control"]
    fn production_tree_bound_instruction_control() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../lodestone-server/assets/worldgen/biome_parameters/overworld.json"
        ))
        .expect("embedded production biome table must be valid JSON");
        let table = super::super::parse_table(&value);
        let tree = BiomeTree::build(&table);
        let NodeStorage::Narrow(nodes) = &tree.nodes else {
            panic!("production biome table must fit narrow tree storage");
        };

        let mut state = 0x9e37_79b9_u64;
        let mut samples = Vec::with_capacity(100_000);
        for _ in 0..100_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let node = &nodes[(state as usize) % nodes.len()];
            state = state.rotate_left(17);
            let target = std::array::from_fn(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                ((state >> 16) as i64 % 24_001) - 12_000
            });
            state = state.rotate_left(23);
            let cutoff = ((state >> 8) as i64 % 200_000_001).max(1);
            samples.push((node.space, target, cutoff));
        }

        let mode = std::env::var("LODESTONE_BIOME_TREE_BOUND_MODE")
            .expect("set LODESTONE_BIOME_TREE_BOUND_MODE to branchful or branchless");
        let mut checksum = 0i64;
        if mode == "branchful" {
            for _ in 0..100 {
                for (space, target, cutoff) in &samples {
                    checksum ^= black_box(bound_narrow_branchful(space, target, *cutoff));
                }
            }
        } else if mode == "branchless" {
            for _ in 0..100 {
                for (space, target, cutoff) in &samples {
                    checksum ^= black_box(bound_narrow_branchless(space, target, *cutoff));
                }
            }
        } else {
            panic!("unknown bound mode: {mode}");
        }
        println!("biome tree bound instruction control: mode={mode} checksum={checksum}");
    }

}
