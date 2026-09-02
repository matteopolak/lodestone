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
//! **The search is now a literal port too**, with exactly one documented
//! omission: vanilla's own last-result field. Vanilla's own subtree-search
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
//! last-result field, a `ThreadLocal`
//! holding *the previous search's leaf*, then stores the new one.
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
//! **Implemented — the fresh-instance answer:** vanilla's traversal with
//! `candidate == null`, i.e. *the first leaf in vanilla's pruned DFS child order
//! achieving the minimum distance*. That is what a freshly constructed
//! `ParameterList` returns, what vanilla's very first sample after load returns,
//! and the only reading of "what vanilla does" that is a function of the target.
//!
//! **Deliberately not implemented — `lastResult` carry-over.** Reproducing it
//! would make a served chunk's biome depend on which chunk that worker thread
//! generated previously, so one seed would produce different worlds across runs
//! and `parallel_generation_is_deterministic_and_matches_serial` plus the
//! byte-identity gates would be measuring a coin flip. No seeding of any kind
//! happens here — not even a pruning-only hint, because under vanilla's
//! incumbent-wins-a-tie rule a hint that ties *is* the answer.
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

/// Vanilla's own R-tree children-per-node constant.
const CHILDREN_PER_NODE: usize = 6;

/// The climate parameter space's dimensionality — vanilla asserts this is 7
/// (vanilla's own R-tree `create`'s `if (dimensions != 7) throw`), six climate axes plus the
/// degenerate `offset` span. See [`BiomeParameterPoint`].
const DIMENSIONS: usize = 7;

/// "No leaf selected" — a node id that cannot exist.
const NONE: u32 = u32::MAX;

/// One flattened tree node. Leaves and subtrees share a representation so the
/// bound computation ([`BiomeTree::bound`]) is branch-free over kind.
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

/// Vanilla's `Climate.RTree`, with vanilla's own search minus `lastResult`. See
/// the module doc for exactly which behaviour that is and which it is not.
#[derive(Debug)]
pub(crate) struct BiomeTree {
    nodes: Vec<Node>,
    /// Child ids, referenced by `Node::{first_child, child_count}`.
    children: Vec<u32>,
    root: u32,
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
        let root = build_level(&mut arena, ids);
        Self::flatten(&arena, root)
    }

    /// Depth-first flatten of the arena into `nodes`/`children`, preserving child
    /// order — which under vanilla's tie-break is now **load-bearing for the
    /// answer**, not merely for pruning efficiency.
    fn flatten(arena: &Arena, root: u32) -> Self {
        let mut tree = Self {
            nodes: Vec::with_capacity(arena.space.len()),
            children: Vec::with_capacity(arena.space.len()),
            root: 0,
        };
        tree.root = tree.flatten_node(arena, root);
        tree
    }

    fn flatten_node(&mut self, arena: &Arena, id: u32) -> u32 {
        let kids = &arena.children[id as usize];
        let me = self.nodes.len() as u32;
        self.nodes.push(Node {
            space: arena.space[id as usize],
            first_child: 0,
            child_count: kids.len() as u32,
            row: arena.row[id as usize],
        });
        if kids.is_empty() {
            return me;
        }
        let first = self.children.len() as u32;
        self.children.extend(std::iter::repeat_n(0u32, kids.len()));
        self.nodes[me as usize].first_child = first;
        for (i, &kid) in kids.iter().enumerate() {
            let flat = self.flatten_node(arena, kid);
            self.children[first as usize + i] = flat;
        }
        me
    }

    /// Vanilla's own R-tree node distance routine `distance(target)`: the summed squared per-axis
    /// distance from `target` to this node's span. For a leaf this is exactly
    /// `BiomeParameterPoint::fitness`; for a subtree it is a lower bound on every
    /// leaf beneath it (module doc).
    #[inline]
    fn bound(&self, id: u32, target: &[i64; DIMENSIONS]) -> i64 {
        let space = &self.nodes[id as usize].space;
        let mut sum = 0i64;
        for i in 0..DIMENSIONS {
            let d = space[i].distance(target[i]);
            sum += d * d;
        }
        sum
    }

    /// The nearest biome's table row, by **vanilla's own search with no
    /// `lastResult`** — the fresh-instance answer (module doc).
    ///
    /// Bumps the biome-search counters: one search, plus the number of
    /// `Node::distance` evaluations it really performed. That second number is the
    /// D5 quantity — it used to be `searches × table_len` by construction, and the
    /// point of this module is that it no longer is, so it is counted.
    pub(crate) fn nearest_row(&self, target: &[i64; DIMENSIONS]) -> u32 {
        self.search(target, None).0
    }

    /// Vanilla's search with an explicit `candidate` in place of its `ThreadLocal`
    /// — the seeded form, kept **only** so a gate can demonstrate that seeding
    /// changes the returned row (and never the distance). Production always passes
    /// `None`; see the module doc for why.
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
        let mut evaluations = 0u64;
        let mut best_dist = i64::MAX;
        let mut best_node = NONE;
        if let Some(seed) = candidate {
            if (seed as usize) < self.nodes.len() && self.nodes[seed as usize].is_leaf() {
                best_dist = self.bound(seed, target);
                best_node = seed;
                evaluations += 1;
            }
        }
        // A single-row table's root is itself the leaf; vanilla's
        // `root.search(...)` returns it directly.
        if self.nodes[self.root as usize].is_leaf() {
            let d = self.bound(self.root, target);
            evaluations += 1;
            if best_dist > d {
                best_dist = d;
                best_node = self.root;
            }
        } else {
            self.visit(self.root, target, &mut best_dist, &mut best_node, &mut evaluations);
        }
        debug_assert_ne!(best_node, NONE, "the tree always has at least one leaf");
        crate::counters::bump_biome_search(evaluations);
        (self.nodes[best_node as usize].row, best_dist)
    }

    /// One subtree-search frame: iterate children in order, evaluate each
    /// child's bound, and descend on **strict** `best_dist > child_bound` —
    /// vanilla's `if (minDistance > childDistance)`. A tie is therefore *pruned*,
    /// which is precisely how the incumbent wins it.
    fn visit(
        &self,
        id: u32,
        target: &[i64; DIMENSIONS],
        best_dist: &mut i64,
        best_node: &mut u32,
        evaluations: &mut u64,
    ) {
        let node = &self.nodes[id as usize];
        let first = node.first_child as usize;
        let end = first + node.child_count as usize;
        for slot in first..end {
            let child = self.children[slot];
            let child_bound = self.bound(child, target);
            *evaluations += 1;
            if *best_dist > child_bound {
                if self.nodes[child as usize].is_leaf() {
                    // Java: `leaf == child`, so `leafDistance == childDistance`,
                    // and the outer `minDistance > leafDistance` is the same
                    // strict test that just passed.
                    *best_dist = child_bound;
                    *best_node = child;
                } else {
                    self.visit(child, target, best_dist, best_node, evaluations);
                }
            }
        }
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
        for (id, node) in self.nodes.iter().enumerate() {
            let first = node.first_child as usize;
            for slot in first..first + node.child_count as usize {
                let child = self.children[slot];
                let cs = &self.nodes[child as usize].space;
                for d in 0..DIMENSIONS {
                    if cs[d].min < node.space[d].min || cs[d].max > node.space[d].max {
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
            self.nodes.iter().filter(|n| n.is_leaf()).count(),
        )
    }

    /// The number of nodes, for a gate that wants to perturb one by index.
    pub(crate) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Whether node `id` is a leaf. Nodes are flattened in DFS **pre-order**, so
    /// node 0 is the root and node 1 is its first child.
    pub(crate) fn node_is_leaf(&self, id: usize) -> bool {
        self.nodes[id].is_leaf()
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
        let node = &self.nodes[self.root as usize];
        let first = node.first_child as usize;
        self.children[first..first + node.child_count as usize].to_vec()
    }

    /// Every leaf's node id, in table-row order — so a gate can seed a search with
    /// a specific row's leaf.
    pub(crate) fn leaf_node_for_row(&self, row: u32) -> Option<u32> {
        self.nodes
            .iter()
            .position(|n| n.is_leaf() && n.row == row)
            .map(|i| i as u32)
    }

    /// Shrinks one node's span to a degenerate point — the **control** for
    /// [`Self::hull_containment_violations`] and for the distance-identity gate.
    /// Breaking the lower-bound property is exactly the defect class those gates
    /// exist to detect, so they are only evidence if this makes them fail.
    pub(crate) fn perturb_node_span(&mut self, id: usize) {
        for d in 0..DIMENSIONS {
            let p = &mut self.nodes[id].space[d];
            let centre = (p.min + p.max) / 2;
            p.min = centre;
            p.max = centre;
        }
    }
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
}
