//! Vanilla's `TreeNodePosition`: where each advancement widget sits
//! in its tab's tree.
//!
//! # Why this has to exist
//!
//! 26.2's advancement JSON carries **no** `x`/`y`. `DisplayInfo`'s position
//! fields are computed server-side by `net/minecraft/advancements/TreeNodePosition.java`
//! and only ever appear on the wire, so a client that builds the screen from the
//! data pack — which is what [`super::advancement_data`] does — has to run the
//! same algorithm. There is nothing on disk to verify a position against, which
//! is why this is a term-for-term port rather than a plausible auto-layout: the
//! only available correctness argument is that it *is* vanilla's own procedure.
//!
//! It is the Reingold–Tilford / Buchheim tidy-tree algorithm: `firstWalk` assigns
//! provisional `y`s bottom-up and `apportion` pushes conflicting subtrees apart,
//! `secondWalk` accumulates the modifiers into final `y`s and records the
//! minimum, and `thirdWalk` shifts the whole tree down so nothing is negative.
//! `x` is simply the depth.
//!
//! # How to change it
//!
//! Index-based, not pointer-based: every `TreeNodePosition` field that is a
//! reference in Java is a `usize` into [`TreeLayout::nodes`] here, because the
//! algorithm mutates siblings and ancestors while walking and `&mut` cannot
//! express that. `NO_NODE` stands in for Java's `null` — using `Option<usize>`
//! everywhere made the four `previousOrThread`/`nextOrThread` chains unreadable
//! without changing what they do.
//!
//! The one place to be careful is `getAncestor`: it consults
//! `other.parent.children.contains(this.ancestor)`, i.e. a *sibling-set*
//! membership test, not an ancestry test. Reading it as the latter produces a
//! tree that looks tidy and overlaps under deep fan-out.

use super::advancement_data::{ADVANCEMENTS, Advancement};

/// Java's `null` for the index-based node references. `usize::MAX` rather than
/// `Option<usize>`; see the module doc.
const NO_NODE: usize = usize::MAX;

/// One positioned advancement — the output of [`layout_tree`].
#[derive(Debug, Clone, Copy)]
pub struct PositionedAdvancement {
    /// Its entry in [`ADVANCEMENTS`].
    pub advancement: &'static Advancement,
    /// Index into [`ADVANCEMENTS`] of its parent, or `None` for the root.
    pub parent: Option<usize>,
    /// Vanilla's `DisplayInfo.x` — the depth from the root.
    pub x: f32,
    /// Vanilla's `DisplayInfo.y`.
    pub y: f32,
}

/// One tab's laid-out tree.
#[derive(Debug, Clone)]
pub struct TreeLayout {
    /// Every visible node, root first. Indices in
    /// [`PositionedAdvancement::parent`] address this vector, not
    /// [`ADVANCEMENTS`].
    pub nodes: Vec<PositionedAdvancement>,
}

/// Working state for one node during the walk — Java's `TreeNodePosition`
/// fields, one for one.
#[derive(Debug, Clone)]
struct Node {
    /// Index into [`ADVANCEMENTS`].
    entry: usize,
    parent: usize,
    previous_sibling: usize,
    /// Java's `childIndex`, **one-based** — `this.children.size() + 1` at
    /// construction, and the root is constructed with `1`. `moveSubtree` divides
    /// by the difference of two of these, so a zero-based version would divide by
    /// the wrong number for the first sibling.
    child_index: i32,
    children: Vec<usize>,
    ancestor: usize,
    thread: usize,
    x: i32,
    y: f32,
    mod_: f32,
    change: f32,
    shift: f32,
}

/// Lay out the tree rooted at `root_id`, which must be one of
/// [`ADVANCEMENTS`]' five roots.
///
/// Returns `None` if `root_id` is not a known advancement. Every node in the
/// result is visible: a hidden advancement is still positioned (vanilla positions
/// it too, and only the *draw* skips it), because dropping it would shift its
/// siblings.
#[must_use]
pub fn layout_tree(root_id: &str) -> Option<TreeLayout> {
    let root_entry = ADVANCEMENTS.iter().position(|a| a.id == root_id)?;

    // Build the forest of *entry* indices first: one pass over a table that is
    // already ordered parents-before-children, so a child always finds its
    // parent's slot.
    let mut children_of: Vec<Vec<usize>> = vec![Vec::new(); ADVANCEMENTS.len()];
    for (i, a) in ADVANCEMENTS.iter().enumerate() {
        if let Some(parent) = a.parent
            && let Some(p) = ADVANCEMENTS.iter().position(|c| c.id == parent)
        {
            children_of[p].push(i);
        }
    }

    let mut nodes: Vec<Node> = Vec::new();
    build(&mut nodes, &children_of, root_entry, NO_NODE, 0);

    first_walk(&mut nodes, 0);
    let root_y = nodes[0].y;
    let min = second_walk(&mut nodes, 0, 0.0, 0, root_y);
    if min < 0.0 {
        third_walk(&mut nodes, 0, -min);
    }

    // Remap Java's node graph onto the flat output. `nodes` is in construction
    // order, which is depth-first from the root, so index 0 is the root.
    let mut out = Vec::with_capacity(nodes.len());
    for node in &nodes {
        out.push(PositionedAdvancement {
            advancement: &ADVANCEMENTS[node.entry],
            parent: (node.parent != NO_NODE).then_some(node.parent),
            x: node.x as f32,
            y: node.y,
        });
    }
    Some(TreeLayout { nodes: out })
}

/// The constructor plus `addChild`, flattened into an arena push.
///
/// Vanilla's `addChild` skips an invisible advancement and adopts its
/// grandchildren instead. Every entry in [`ADVANCEMENTS`] has a `display` block
/// by construction (that is the extraction's own filter), so the skip branch
/// cannot fire here — noted rather than ported, since a future table that
/// included display-less recipe advancements would need it back.
fn build(
    nodes: &mut Vec<Node>,
    children_of: &[Vec<usize>],
    entry: usize,
    parent: usize,
    depth: i32,
) -> usize {
    let me = nodes.len();
    nodes.push(Node {
        entry,
        parent,
        previous_sibling: NO_NODE,
        child_index: 1,
        children: Vec::new(),
        ancestor: me,
        thread: NO_NODE,
        x: depth,
        y: -1.0,
        mod_: 0.0,
        change: 0.0,
        shift: 0.0,
    });
    let mut previous = NO_NODE;
    for &child_entry in &children_of[entry] {
        let child = build(nodes, children_of, child_entry, me, depth + 1);
        nodes[child].previous_sibling = previous;
        nodes[child].child_index = nodes[me].children.len() as i32 + 1;
        nodes[me].children.push(child);
        previous = child;
    }
    me
}

fn first_walk(nodes: &mut Vec<Node>, i: usize) {
    if nodes[i].children.is_empty() {
        nodes[i].y = match nodes[i].previous_sibling {
            NO_NODE => 0.0,
            prev => nodes[prev].y + 1.0,
        };
        return;
    }
    let children = nodes[i].children.clone();
    let mut default_ancestor = NO_NODE;
    for &child in &children {
        first_walk(nodes, child);
        let seed = if default_ancestor == NO_NODE { child } else { default_ancestor };
        default_ancestor = apportion(nodes, child, seed);
    }
    execute_shifts(nodes, i);
    let midpoint = (nodes[children[0]].y + nodes[children[children.len() - 1]].y) / 2.0;
    match nodes[i].previous_sibling {
        NO_NODE => nodes[i].y = midpoint,
        prev => {
            nodes[i].y = nodes[prev].y + 1.0;
            nodes[i].mod_ = nodes[i].y - midpoint;
        }
    }
}

fn second_walk(nodes: &mut Vec<Node>, i: usize, mod_sum: f32, depth: i32, mut min: f32) -> f32 {
    nodes[i].y += mod_sum;
    nodes[i].x = depth;
    if nodes[i].y < min {
        min = nodes[i].y;
    }
    let children = nodes[i].children.clone();
    let own_mod = nodes[i].mod_;
    for child in children {
        min = second_walk(nodes, child, mod_sum + own_mod, depth + 1, min);
    }
    min
}

fn third_walk(nodes: &mut Vec<Node>, i: usize, offset: f32) {
    nodes[i].y += offset;
    let children = nodes[i].children.clone();
    for child in children {
        third_walk(nodes, child, offset);
    }
}

fn execute_shifts(nodes: &mut Vec<Node>, i: usize) {
    let mut shift = 0.0;
    let mut change = 0.0;
    let children = nodes[i].children.clone();
    for &child in children.iter().rev() {
        nodes[child].y += shift;
        nodes[child].mod_ += shift;
        change += nodes[child].change;
        shift += nodes[child].shift + change;
    }
}

fn previous_or_thread(nodes: &[Node], i: usize) -> usize {
    if nodes[i].thread != NO_NODE {
        return nodes[i].thread;
    }
    nodes[i].children.first().copied().unwrap_or(NO_NODE)
}

fn next_or_thread(nodes: &[Node], i: usize) -> usize {
    if nodes[i].thread != NO_NODE {
        return nodes[i].thread;
    }
    nodes[i].children.last().copied().unwrap_or(NO_NODE)
}

fn apportion(nodes: &mut Vec<Node>, me: usize, mut default_ancestor: usize) -> usize {
    if nodes[me].previous_sibling == NO_NODE {
        return default_ancestor;
    }
    let mut vir = me;
    let mut vor = me;
    let mut vil = nodes[me].previous_sibling;
    let mut vol = nodes[nodes[me].parent].children[0];
    let mut sir = nodes[me].mod_;
    let mut sor = nodes[me].mod_;
    let mut sil = nodes[vil].mod_;
    let mut sol = nodes[vol].mod_;

    while next_or_thread(nodes, vil) != NO_NODE && previous_or_thread(nodes, vir) != NO_NODE {
        vil = next_or_thread(nodes, vil);
        vir = previous_or_thread(nodes, vir);
        vol = previous_or_thread(nodes, vol);
        vor = next_or_thread(nodes, vor);
        // Java assigns `vor.ancestor = this` unconditionally here, *before* the
        // shift test — and `vor` may be `null` on the way out of the loop only
        // after the condition fails, so at this point it is always live.
        nodes[vor].ancestor = me;
        let shift = nodes[vil].y + sil - (nodes[vir].y + sir) + 1.0;
        if shift > 0.0 {
            let ancestor = get_ancestor(nodes, vil, me, default_ancestor);
            move_subtree(nodes, ancestor, me, shift);
            sir += shift;
            sor += shift;
        }
        sil += nodes[vil].mod_;
        sir += nodes[vir].mod_;
        sol += nodes[vol].mod_;
        sor += nodes[vor].mod_;
    }

    if next_or_thread(nodes, vil) != NO_NODE && next_or_thread(nodes, vor) == NO_NODE {
        nodes[vor].thread = next_or_thread(nodes, vil);
        nodes[vor].mod_ += sil - sor;
    } else {
        if previous_or_thread(nodes, vir) != NO_NODE && previous_or_thread(nodes, vol) == NO_NODE {
            nodes[vol].thread = previous_or_thread(nodes, vir);
            nodes[vol].mod_ += sir - sol;
        }
        default_ancestor = me;
    }
    default_ancestor
}

fn move_subtree(nodes: &mut [Node], left: usize, right: usize, shift: f32) {
    let subtrees = (nodes[right].child_index - nodes[left].child_index) as f32;
    if subtrees != 0.0 {
        nodes[right].change -= shift / subtrees;
        nodes[left].change += shift / subtrees;
    }
    nodes[right].shift += shift;
    nodes[right].y += shift;
    nodes[right].mod_ += shift;
}

/// `getAncestor` — and note what it tests: whether `self.ancestor` is a
/// **sibling** of `other`, i.e. a member of `other.parent.children`. Reading it
/// as "is an ancestor of" gives a tidy-looking tree that overlaps under deep
/// fan-out.
fn get_ancestor(nodes: &[Node], me: usize, other: usize, default_ancestor: usize) -> usize {
    let ancestor = nodes[me].ancestor;
    if ancestor != NO_NODE {
        let other_parent = nodes[other].parent;
        if other_parent != NO_NODE && nodes[other_parent].children.contains(&ancestor) {
            return ancestor;
        }
    }
    default_ancestor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Vec<&'static str> {
        ADVANCEMENTS.iter().filter(|a| a.parent.is_none()).map(|a| a.id).collect()
    }

    #[test]
    fn every_root_lays_out_its_whole_subtree() {
        let total: usize = roots()
            .iter()
            .map(|id| layout_tree(id).expect("a root lays out").nodes.len())
            .sum();
        // Every advancement belongs to exactly one of the five trees, so the five
        // layouts must partition the table with nothing left over.
        assert_eq!(total, ADVANCEMENTS.len());
    }

    #[test]
    fn x_is_the_depth_and_y_is_non_negative() {
        for id in roots() {
            let tree = layout_tree(id).expect("a root lays out");
            assert_eq!(tree.nodes[0].x, 0.0, "{id}'s root is not at depth 0");
            for node in &tree.nodes {
                assert!(node.y >= 0.0, "{} has a negative y", node.advancement.id);
                if let Some(parent) = node.parent {
                    assert_eq!(
                        node.x,
                        tree.nodes[parent].x + 1.0,
                        "{} is not one level below its parent",
                        node.advancement.id
                    );
                }
            }
        }
    }

    #[test]
    fn no_two_nodes_at_the_same_depth_overlap() {
        // The whole point of the algorithm: siblings-by-depth are at least one
        // row apart. This is the property vanilla's `apportion` exists to
        // guarantee, and it is independent of our implementation — a naive
        // "y = index within depth" layout would satisfy it, but a *broken*
        // Reingold-Tilford would not, which is exactly the failure to catch.
        for id in roots() {
            let tree = layout_tree(id).expect("a root lays out");
            let max_depth = tree.nodes.iter().map(|n| n.x as i32).max().unwrap_or(0);
            for depth in 0..=max_depth {
                let mut ys: Vec<f32> =
                    tree.nodes.iter().filter(|n| n.x as i32 == depth).map(|n| n.y).collect();
                ys.sort_by(|a, b| a.partial_cmp(b).expect("no NaN positions"));
                for pair in ys.windows(2) {
                    assert!(
                        pair[1] - pair[0] >= 1.0 - f32::EPSILON,
                        "{id} depth {depth}: {} and {} are closer than one row",
                        pair[0],
                        pair[1]
                    );
                }
            }
        }
    }

    #[test]
    fn a_parent_sits_between_its_own_children() {
        // `firstWalk`'s midpoint rule, checked on the real tree rather than
        // asserted about the code: a node with children is placed at the mean of
        // its first and last child, so it can never be outside their span.
        for id in roots() {
            let tree = layout_tree(id).expect("a root lays out");
            for (i, node) in tree.nodes.iter().enumerate() {
                let children: Vec<f32> = tree
                    .nodes
                    .iter()
                    .filter(|c| c.parent == Some(i))
                    .map(|c| c.y)
                    .collect();
                if children.is_empty() {
                    continue;
                }
                let lo = children.iter().copied().fold(f32::INFINITY, f32::min);
                let hi = children.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                assert!(
                    node.y >= lo - 0.001 && node.y <= hi + 0.001,
                    "{} at y {} is outside its children's span {lo}..{hi}",
                    node.advancement.id,
                    node.y
                );
            }
        }
    }

    #[test]
    fn an_unknown_root_lays_out_nothing() {
        assert!(layout_tree("minecraft:not/a/real/advancement").is_none());
    }
}
