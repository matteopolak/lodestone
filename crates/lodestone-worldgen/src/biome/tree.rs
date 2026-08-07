//! Vanilla's `Climate.RTree`, ported — the search structure that replaces the
//! O(table_len) brute-force climate scan (`docs/plans/worldgen-rewrite.md` D5).
//!
//! # What is ported verbatim, and what deliberately is not
//!
//! **The tree structure is a literal port** of `Climate.RTree.create`/`build`/
//! `bucketize`/`sort`/`cost`/`buildParameterSpace` (`net/minecraft/world/level/
//! biome/Climate.java`, 26.2 de-obfuscated). Node ordering therefore comes from
//! the game data's own row order run through vanilla's own splitting heuristic
//! — nothing here re-derives a bucketing scheme of its own, which is the
//! property `docs/plans/worldgen-rewrite.md` Q2 asks for ("tree structure taken
//! from the game data's own node ordering").
//!
//! **The search is not a literal port, and that is the whole point of this
//! module.** Read `SubTree.search` in the Java:
//!
//! ```java
//! long minDistance = candidate == null ? Long.MAX_VALUE : distanceMetric.distance(candidate, target);
//! for (Node<T> child : this.children) {
//!    long childDistance = distanceMetric.distance(child, target);
//!    if (minDistance > childDistance) { ... if (minDistance > leafDistance) { ... } }
//! }
//! ```
//!
//! Both comparisons are **strict**, and `candidate` is seeded from
//! `RTree.lastResult`, a `ThreadLocal` holding the previous search's leaf. Three
//! consequences, all of them measured off the source rather than assumed:
//!
//! 1. On an exact distance tie the **incumbent wins**, so the answer depends on
//!    which leaf the tree happened to reach first — i.e. on child order.
//! 2. Because the incumbent can be `lastResult`, the answer depends on **what
//!    this thread searched previously**. Vanilla's tree is therefore *not a pure
//!    function of the target* when ties exist.
//! 3. Vanilla's own `findValueBruteForce` breaks ties by **earliest table row**
//!    (`if (fitness < bestFitness)` over `values()` in order). So vanilla ships
//!    two searches with two different tie-breaks and uses the history-dependent
//!    one.
//!
//! Porting (1)+(2) into this engine would put thread-local search history into
//! chunk generation: two identically-seeded generators could disagree, and
//! `column_is_byte_identical_across_two_independently_constructed_generators`
//! would become a coin flip. Per the plan's Q4.5 rollback rule the old engine is
//! the bridge and its behaviour is JVM-proven, and the old engine is brute
//! force. So this port keeps **brute force's tie-break** and makes the tree
//! *provably* result-identical to it:
//!
//! | vanilla | here |
//! |---|---|
//! | prune when `best > child_bound` (strict) | descend when `child_bound <= best_dist` — ties are **never** pruned |
//! | incumbent wins a tie | **lowest table row wins a tie** |
//! | `ThreadLocal` last-result seed decides ties | last-result seed is a pure pruning *hint*, provably unable to change the answer |
//!
//! # Why result-identity is a theorem here, not a hope
//!
//! Define `key(L) = (dist(L, t), row(L))` for a leaf `L`, ordered
//! lexicographically. Brute force returns the leaf minimising `key` (its strict
//! `<` on distance keeps the earliest row on a tie, which is exactly the
//! lexicographic minimum). [`BiomeTree::nearest_row`] also returns the
//! lexicographic minimum, because:
//!
//! * **The node bound is a lower bound.** A subtree's `space[i]` is the
//!   *interval hull* of its children's (`buildParameterSpace` unions with
//!   `Parameter.span`), and for `[lo, hi] ⊇ [lo', hi']` we have
//!   `distance([lo, hi], t) <= distance([lo', hi'], t)` for every `t` — widening
//!   a span can only move `t` closer to it or leave it inside. By induction
//!   `bound(N, t) <= dist(L, t)` for every leaf `L` under `N`.
//!   [`BiomeTree::hull_containment_violations`] checks the premise
//!   (`space(child) ⊆ space(parent)` on all 7 axes) exhaustively over **every**
//!   node/child pair in the real tree, so this is a verified premise rather
//!   than a claim about code that was written correctly.
//! * **Nothing that could tie is pruned.** A subtree is skipped only when
//!   `bound > best_dist` *or* when `bound == best_dist` and the subtree's
//!   minimum row (`min_row`, precomputed) cannot beat the incumbent's row. In
//!   both cases every leaf inside is `key`-worse than the incumbent, and the
//!   incumbent only improves, so a pruned subtree can never hold the answer.
//! * **The candidate set therefore always contains the lexicographic minimum**,
//!   and `Best::offer` is a lexicographic min-reduction — an associative,
//!   commutative, idempotent fold. Visit order and the initial seed cannot
//!   change its result.
//!
//! That last line is what licenses the `AtomicU32` seed hint: any leaf row is a
//! legal seed, a torn or foreign read is still a legal seed, and the fold's
//! result is seed-independent. So the locality trick vanilla gets from a
//! `ThreadLocal` is available here **without** making output history-dependent.
//! `biome_tree_identity.rs`'s `the_result_is_invariant_under_every_search_seed`
//! is the gate on that, not this comment.

use std::sync::atomic::{AtomicU32, Ordering};

use super::{BiomeParameterPoint, Parameter};

/// Vanilla's `Climate.RTree.CHILDREN_PER_NODE`.
const CHILDREN_PER_NODE: usize = 6;

/// The climate parameter space's dimensionality — vanilla asserts this is 7
/// (`RTree.create`'s `if (dimensions != 7) throw`), six climate axes plus the
/// degenerate `offset` span. See [`BiomeParameterPoint`].
const DIMENSIONS: usize = 7;

/// Sentinel for "no seed hint yet" in [`BiomeTree::seed_hint`].
const NO_HINT: u32 = u32::MAX;

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
    /// The table row this leaf carries, or — for a subtree — the **minimum**
    /// table row anywhere beneath it.
    ///
    /// For a leaf this is the tie-break key. For a subtree it is a second prune:
    /// a subtree whose bound merely *ties* the incumbent distance is worth
    /// entering only if it could also beat the incumbent's row, and `min_row`
    /// answers that in one comparison. Not in vanilla (which has no row
    /// tie-break to prune for).
    min_row: u32,
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
    min_row: Vec<u32>,
}

impl Arena {
    fn push(&mut self, space: [Parameter; DIMENSIONS], children: Vec<u32>, min_row: u32) -> u32 {
        let id = self.space.len() as u32;
        self.space.push(space);
        self.children.push(children);
        self.min_row.push(min_row);
        id
    }

    /// `Climate.RTree.comparator(dimension, absolute)`'s sort key: the span's
    /// centre, `(min + max) / 2` in **truncating** integer division (Java's
    /// `/2L` on a `long`, which Rust's `i64` division matches exactly, including
    /// for negatives — both truncate toward zero).
    #[inline]
    fn centre(&self, id: u32, axis: usize, absolute: bool) -> i64 {
        let p = self.space[id as usize][axis];
        let centre = (p.min + p.max) / 2;
        if absolute { centre.abs() } else { centre }
    }

    /// `Climate.RTree.sort(children, dimensions, dimension, absolute)` — a
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

    /// `Climate.RTree.buildParameterSpace` — the per-axis interval hull
    /// (`Parameter.span`: `min` of mins, `max` of maxes) over `ids`.
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
            .map(|&id| self.min_row[id as usize])
            .min()
            .expect("a subtree needs at least one child")
    }
}

/// `Climate.RTree.cost(parameterSpace)` — the summed axis extents of a candidate
/// bucket's hull. Vanilla picks the split axis minimising the total over buckets.
fn cost(space: &[Parameter; DIMENSIONS]) -> i64 {
    space.iter().map(|p| (p.max - p.min).abs()).sum()
}

/// `Climate.RTree.bucketize`'s `expectedChildrenCount`, computed in integers.
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

/// `Climate.RTree.bucketize(nodes)` — a straight sequential chunking of the
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

/// The lexicographic `(distance, row)` minimum found so far, and the node it
/// came from (kept only to update [`BiomeTree::seed_hint`]).
#[derive(Debug, Clone, Copy)]
struct Best {
    dist: i64,
    row: u32,
    node: u32,
}

impl Best {
    /// The lexicographic min-reduction. Associative, commutative and idempotent,
    /// which is why visit order and the seed cannot change the search's result.
    #[inline]
    fn offer(&mut self, dist: i64, row: u32, node: u32) {
        if dist < self.dist || (dist == self.dist && row < self.row) {
            *self = Best { dist, row, node };
        }
    }
}

/// Vanilla's `Climate.RTree`, with a deterministic brute-force-identical search.
/// See the module doc for exactly which parts are a literal port and which are
/// not.
#[derive(Debug)]
pub(crate) struct BiomeTree {
    nodes: Vec<Node>,
    /// Child ids, referenced by `Node::{first_child, child_count}`.
    children: Vec<u32>,
    root: u32,
    /// Vanilla's `RTree.lastResult` locality trick, made safe: a node id whose
    /// distance seeds `best` so the first level can prune immediately.
    ///
    /// `Relaxed` and shared across threads on purpose. Any node id is a legal
    /// seed and the search's result is provably seed-independent (module doc), so
    /// this needs no synchronisation and cannot make output depend on which
    /// thread searched what first. A `ThreadLocal` would cost a TLS lookup per
    /// search to buy nothing.
    seed_hint: AtomicU32,
}

impl BiomeTree {
    /// `Climate.RTree.create(values)` over the parsed table, in table order.
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
            min_row: Vec::with_capacity(points.len() * 2),
        };
        // Leaves in table order, so `min_row` is the table row and brute force's
        // tie-break key is carried by construction.
        let ids: Vec<u32> = points
            .iter()
            .enumerate()
            .map(|(row, p)| arena.push(p.params, Vec::new(), row as u32))
            .collect();
        let root = build_level(&mut arena, ids);
        Self::flatten(&arena, root)
    }

    /// Depth-first flatten of the arena into `nodes`/`children`, preserving child
    /// order (which is vanilla's traversal order, and what makes the pruning
    /// efficient even though the *result* does not depend on it).
    fn flatten(arena: &Arena, root: u32) -> Self {
        let mut tree = Self {
            nodes: Vec::with_capacity(arena.space.len()),
            children: Vec::with_capacity(arena.space.len()),
            root: 0,
            seed_hint: AtomicU32::new(NO_HINT),
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
            min_row: arena.min_row[id as usize],
        });
        if kids.is_empty() {
            return me;
        }
        // Reserve a contiguous slot range first: children are flattened after,
        // and each may append its own subtree's slots.
        let first = self.children.len() as u32;
        self.children.extend(std::iter::repeat_n(0u32, kids.len()));
        self.nodes[me as usize].first_child = first;
        for (i, &kid) in kids.iter().enumerate() {
            let flat = self.flatten_node(arena, kid);
            self.children[first as usize + i] = flat;
        }
        me
    }

    /// `Climate.RTree.Node.distance(target)`: the summed squared per-axis
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

    /// The table row of the nearest biome — the lexicographic `(distance, row)`
    /// minimum, identical to [`super::nearest_biome`]'s answer at every target
    /// (module doc for the proof, `biome_tree_identity.rs` for the gates).
    ///
    /// Returns the row and bumps the biome-search counters: one search, plus the
    /// number of `Node::distance` evaluations it really performed. That second
    /// number is the D5 quantity — `biome_rows_compared` used to be
    /// `searches × table_len` by construction, and the whole point of this module
    /// is that it no longer is, so it has to be counted rather than derived.
    pub(crate) fn nearest_row(&self, target: &[i64; DIMENSIONS]) -> u32 {
        let mut evaluations = 0u64;
        let mut best = {
            let hint = self.seed_hint.load(Ordering::Relaxed);
            // A hint is a *node* id; only a leaf is a legal candidate, and a
            // stale hint from a differently-shaped tree cannot occur (the hint
            // lives in the tree). Both checks are cheap insurance, not required
            // for correctness of the answer.
            if hint != NO_HINT
                && (hint as usize) < self.nodes.len()
                && self.nodes[hint as usize].is_leaf()
            {
                evaluations += 1;
                Best {
                    dist: self.bound(hint, target),
                    row: self.nodes[hint as usize].min_row,
                    node: hint,
                }
            } else {
                Best {
                    dist: i64::MAX,
                    row: u32::MAX,
                    node: NO_HINT,
                }
            }
        };
        let root_bound = self.bound(self.root, target);
        evaluations += 1;
        self.descend(self.root, root_bound, target, &mut best, &mut evaluations);
        debug_assert_ne!(best.node, NO_HINT, "the tree always has at least one leaf");
        self.seed_hint.store(best.node, Ordering::Relaxed);
        crate::counters::bump_biome_search(evaluations);
        best.row
    }

    /// Vanilla's `SubTree.search`, with the two comparison changes the module doc
    /// tabulates. `bound` is the caller's already-computed bound for `id`, so no
    /// node's distance is evaluated twice.
    fn descend(
        &self,
        id: u32,
        bound: i64,
        target: &[i64; DIMENSIONS],
        best: &mut Best,
        evaluations: &mut u64,
    ) {
        let node = &self.nodes[id as usize];
        if node.is_leaf() {
            best.offer(bound, node.min_row, id);
            return;
        }
        let first = node.first_child as usize;
        let end = first + node.child_count as usize;
        for slot in first..end {
            let child = self.children[slot];
            let child_bound = self.bound(child, target);
            *evaluations += 1;
            if self.can_hold_a_better_leaf(child, child_bound, best) {
                self.descend(child, child_bound, target, best, evaluations);
            }
        }
    }

    /// The prune test. `child_bound` lower-bounds every leaf under `child`, so:
    ///
    /// * `child_bound < best.dist` — a leaf inside could win on distance alone.
    /// * `child_bound == best.dist` — a leaf inside could *tie* on distance, and
    ///   then wins only with a lower row, which is possible only if the
    ///   subtree's `min_row` is lower. **Ties are never pruned on distance
    ///   alone**; that is what keeps the answer equal to brute force's.
    /// * `child_bound > best.dist` — every leaf inside is strictly worse.
    #[inline]
    fn can_hold_a_better_leaf(&self, child: u32, child_bound: i64, best: &Best) -> bool {
        if child_bound < best.dist {
            return true;
        }
        child_bound == best.dist && self.nodes[child as usize].min_row < best.row
    }

    /// Every `(parent, child)` pair whose spans violate hull containment on some
    /// axis, as `(parent_id, child_id, axis)`.
    ///
    /// The premise the result-identity proof rests on (module doc): a subtree's
    /// span must contain each child's span, or its `bound` stops lower-bounding
    /// the leaves beneath it and a prune could discard the true nearest biome.
    /// Exposed so a gate can check it **exhaustively over every node/child pair
    /// in the real tree** rather than trusting `Arena::hull`'s three lines. An
    /// empty return is only meaningful next to a control that perturbs a node and
    /// sees this fire — see `biome_tree_identity.rs`.
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

    /// Forces the seed hint, so a gate can prove the answer is seed-invariant
    /// (module doc's last paragraph). Gate support: production never needs to
    /// choose a seed, and a wrong one costs speed, never correctness. Not
    /// `#[cfg(test)]`, because `tests/biome_tree_identity.rs` is a separate crate
    /// and cannot see the lib's test-only items.
    pub(crate) fn force_seed_hint(&self, node: u32) {
        self.seed_hint.store(node, Ordering::Relaxed);
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

    /// Shrinks one node's span to a degenerate point — the **control** for
    /// [`Self::hull_containment_violations`] and for the brute-force identity
    /// gate. Breaking the lower-bound property is exactly the defect class the
    /// identity gate exists to detect, so the gate is only evidence if this makes
    /// it fail.
    pub(crate) fn perturb_node_span(&mut self, id: usize) {
        for d in 0..DIMENSIONS {
            let p = &mut self.nodes[id].space[d];
            let centre = (p.min + p.max) / 2;
            p.min = centre;
            p.max = centre;
        }
    }

    /// Vanilla's *exact* search — strict pruning, incumbent-wins ties, an
    /// explicit `candidate` seed in place of the `ThreadLocal`. Not used in
    /// production (see the module doc); it exists so a gate can answer the
    /// question the module doc raises: does vanilla's own tree ever disagree with
    /// vanilla's own brute force on the real table?
    pub(crate) fn nearest_row_vanilla_exact(
        &self,
        target: &[i64; DIMENSIONS],
        candidate: Option<u32>,
    ) -> u32 {
        fn go(
            tree: &BiomeTree,
            id: u32,
            target: &[i64; DIMENSIONS],
            mut min_distance: i64,
            mut closest: Option<u32>,
        ) -> (i64, Option<u32>) {
            let node = &tree.nodes[id as usize];
            if node.is_leaf() {
                return (min_distance, closest);
            }
            let first = node.first_child as usize;
            for slot in first..first + node.child_count as usize {
                let child = tree.children[slot];
                let child_distance = tree.bound(child, target);
                if min_distance > child_distance {
                    let (_, leaf) = go(tree, child, target, min_distance, closest);
                    let leaf = if tree.nodes[child as usize].is_leaf() {
                        Some(child)
                    } else {
                        leaf
                    };
                    let leaf_distance = match leaf {
                        Some(l) if l == child => child_distance,
                        Some(l) => tree.bound(l, target),
                        None => i64::MAX,
                    };
                    if min_distance > leaf_distance {
                        min_distance = leaf_distance;
                        closest = leaf;
                    }
                }
            }
            (min_distance, closest)
        }
        let seed_distance = candidate.map_or(i64::MAX, |c| self.bound(c, target));
        let (_, leaf) = go(self, self.root, target, seed_distance, candidate);
        let leaf = leaf.expect("vanilla's search always returns a leaf");
        self.nodes[leaf as usize].min_row
    }
}

/// `Climate.RTree.build(dimensions, children)` — one level of the recursion,
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
            let vanilla = 6.0f64
                .powf(((n as f64 - 0.01).ln() / 6.0f64.ln()).floor())
                as usize;
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

    /// The `Arena::hull` premise, and the control that it can fail. A gate that
    /// only ever observes "no violations" cannot distinguish a correct hull from
    /// a detector that never fires.
    #[test]
    fn hull_containment_control_fires_when_a_node_is_shrunk() {
        let table = spread_table(300);
        let mut tree = BiomeTree::build(&table);
        assert!(tree.hull_containment_violations().is_empty());
        // Node 0 is a leaf (leaves are pushed first), so shrink a subtree: the
        // root, which by construction has children.
        let root = tree.root as usize;
        tree.perturb_node_span(root);
        assert!(
            !tree.hull_containment_violations().is_empty(),
            "shrinking the root's span must be reported as a containment violation"
        );
    }

    #[test]
    fn tree_and_brute_force_agree_on_a_synthetic_table() {
        let table = spread_table(400);
        let tree = BiomeTree::build(&table);
        let mut checked = 0u32;
        for t in (-11000..=11000).step_by(311) {
            for h in (-11000..=11000).step_by(701) {
                for c in (-11000..=11000).step_by(1301) {
                    let target = [t, h, c, 0, 0, 0, 0];
                    let brute = super::super::nearest_row_brute_force(&table, &target);
                    assert_eq!(
                        tree.nearest_row(&target),
                        brute,
                        "tree disagreed with brute force at {target:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 5_000, "the sweep must actually run: {checked}");
    }

    /// The answer is a lexicographic min-reduction, so it cannot depend on the
    /// seed hint. Adversarial seeds included: the last leaf, and (per target) a
    /// rotating one.
    #[test]
    fn the_answer_is_invariant_under_every_seed_hint() {
        let table = spread_table(400);
        let tree = BiomeTree::build(&table);
        let nodes = tree.node_count() as u32;
        for t in (-11000..=11000).step_by(997) {
            let target = [t, t / 2, -t, 0, 0, 0, 0];
            let expected = super::super::nearest_row_brute_force(&table, &target);
            for hint in [NO_HINT, 0, 1, nodes - 1, (t.unsigned_abs() as u32) % nodes] {
                tree.force_seed_hint(hint);
                assert_eq!(
                    tree.nearest_row(&target),
                    expected,
                    "seed hint {hint} changed the answer at {target:?}"
                );
            }
        }
    }
}
