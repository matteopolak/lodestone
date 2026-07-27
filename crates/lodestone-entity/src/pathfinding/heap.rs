//! The open-set binary heap, a faithful port of vanilla's `BinaryHeap`.
//!
//! Vanilla stores `heapIdx` on each `Node` so a node's priority can be changed
//! in place (`changeCost`) without a linear scan. We keep the nodes in an arena
//! (`&mut [Node]`) and store arena indices in the heap; every heap operation
//! updates the affected nodes' `heap_idx`, exactly as the original mutates the
//! node objects. Comparisons are on `Node::f`, using the same strict `<` that
//! vanilla uses (which decides ties deterministically).

use super::node::Node;

/// A min-heap of node arena indices ordered by `Node::f`.
#[derive(Debug, Default)]
pub struct BinaryHeap {
    heap: Vec<usize>,
}

impl BinaryHeap {
    /// Creates an empty heap.
    #[must_use]
    pub fn new() -> Self {
        Self { heap: Vec::new() }
    }

    /// Removes all nodes without touching their `heap_idx` (matching vanilla's
    /// `clear`, which just resets `size`; callers reset nodes separately).
    pub fn clear(&mut self) {
        self.heap.clear();
    }

    /// Whether the heap is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// The number of queued nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Inserts arena node `idx`, panicking if it is already queued (mirrors
    /// vanilla's `IllegalStateException`).
    pub fn insert(&mut self, nodes: &mut [Node], idx: usize) {
        assert!(nodes[idx].heap_idx < 0, "node already in open set");
        let pos = self.heap.len();
        self.heap.push(idx);
        nodes[idx].heap_idx = pos as i32;
        self.up_heap(nodes, pos);
    }

    /// Pops the lowest-`f` node, returning its arena index.
    pub fn pop(&mut self, nodes: &mut [Node]) -> usize {
        let popped = self.heap[0];
        let last = self.heap.pop().expect("pop on non-empty heap");
        if !self.heap.is_empty() {
            self.heap[0] = last;
            nodes[last].heap_idx = 0;
            self.down_heap(nodes, 0);
        }
        nodes[popped].heap_idx = -1;
        popped
    }

    /// Changes a queued node's `f` and restores the heap invariant.
    pub fn change_cost(&mut self, nodes: &mut [Node], idx: usize, new_cost: f32) {
        let old_cost = nodes[idx].f;
        nodes[idx].f = new_cost;
        let pos = nodes[idx].heap_idx as usize;
        if new_cost < old_cost {
            self.up_heap(nodes, pos);
        } else {
            self.down_heap(nodes, pos);
        }
    }

    fn up_heap(&mut self, nodes: &mut [Node], mut idx: usize) {
        let node = self.heap[idx];
        let cost = nodes[node].f;
        while idx > 0 {
            let parent_idx = (idx - 1) >> 1;
            let parent = self.heap[parent_idx];
            if cost >= nodes[parent].f {
                break;
            }
            self.heap[idx] = parent;
            nodes[parent].heap_idx = idx as i32;
            idx = parent_idx;
        }
        self.heap[idx] = node;
        nodes[node].heap_idx = idx as i32;
    }

    fn down_heap(&mut self, nodes: &mut [Node], mut idx: usize) {
        let node = self.heap[idx];
        let cost = nodes[node].f;
        let size = self.heap.len();
        loop {
            let left_idx = 1 + (idx << 1);
            let right_idx = left_idx + 1;
            if left_idx >= size {
                break;
            }
            let left_node = self.heap[left_idx];
            let left_cost = nodes[left_node].f;
            let (chosen_idx, chosen_node, chosen_cost) = if right_idx >= size {
                (left_idx, left_node, left_cost)
            } else {
                let right_node = self.heap[right_idx];
                let right_cost = nodes[right_node].f;
                if left_cost < right_cost {
                    (left_idx, left_node, left_cost)
                } else {
                    (right_idx, right_node, right_cost)
                }
            };
            if chosen_cost >= cost {
                break;
            }
            self.heap[idx] = chosen_node;
            nodes[chosen_node].heap_idx = idx as i32;
            idx = chosen_idx;
        }
        self.heap[idx] = node;
        nodes[node].heap_idx = idx as i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arena(fs: &[f32]) -> Vec<Node> {
        fs.iter()
            .enumerate()
            .map(|(i, &f)| {
                let mut n = Node::new(i as i32, 0, 0);
                n.f = f;
                n
            })
            .collect()
    }

    #[test]
    fn pops_in_ascending_f_order() {
        let mut nodes = arena(&[5.0, 1.0, 3.0, 2.0, 4.0]);
        let mut heap = BinaryHeap::new();
        for i in 0..nodes.len() {
            heap.insert(&mut nodes, i);
        }
        let mut popped = Vec::new();
        while !heap.is_empty() {
            let idx = heap.pop(&mut nodes);
            popped.push(nodes[idx].f);
        }
        assert_eq!(popped, vec![1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn change_cost_reorders() {
        let mut nodes = arena(&[5.0, 6.0, 7.0]);
        let mut heap = BinaryHeap::new();
        for i in 0..3 {
            heap.insert(&mut nodes, i);
        }
        heap.change_cost(&mut nodes, 2, 0.5); // node 2 becomes cheapest
        assert_eq!(heap.pop(&mut nodes), 2);
    }

    #[test]
    fn heap_idx_cleared_on_pop() {
        let mut nodes = arena(&[1.0]);
        let mut heap = BinaryHeap::new();
        heap.insert(&mut nodes, 0);
        assert!(nodes[0].in_open_set());
        heap.pop(&mut nodes);
        assert!(!nodes[0].in_open_set());
    }
}
