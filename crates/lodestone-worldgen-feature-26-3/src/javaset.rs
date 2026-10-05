//! A hash set of positions that iterates in the order the reference's own hash set does.
//!
//! Tree decorators read the trunk and foliage sets in iteration order (stably sorted by height
//! afterwards), and the leaf-distance pass drains its work lists in iteration order, so the
//! order is observable in placed blocks. The reference table starts at 16 buckets, doubles when
//! the element count passes three quarters of the capacity, indexes by the spread hash, and
//! keeps insertion order inside a bucket (a resize split preserves relative order).

use crate::pos::Pos;

/// The position hash: `(y + z * 31) * 31 + x` with wrapping 32-bit arithmetic.
fn hash(p: Pos) -> i32 {
    p.y.wrapping_add(p.z.wrapping_mul(31)).wrapping_mul(31).wrapping_add(p.x)
}

fn spread(h: i32) -> u32 {
    let h = h as u32;
    h ^ (h >> 16)
}

#[derive(Clone, Debug, Default)]
pub struct JavaSet {
    buckets: Vec<Vec<Pos>>,
    size: usize,
}

impl JavaSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn index(&self, p: Pos) -> usize {
        (spread(hash(p)) as usize) & (self.buckets.len() - 1)
    }

    #[must_use]
    pub fn contains(&self, p: Pos) -> bool {
        !self.buckets.is_empty() && self.buckets[self.index(p)].contains(&p)
    }

    /// Adds a position; returns whether it was new.
    pub fn insert(&mut self, p: Pos) -> bool {
        if self.buckets.is_empty() {
            self.buckets = vec![Vec::new(); 16];
        }
        let i = self.index(p);
        if self.buckets[i].contains(&p) {
            return false;
        }
        self.buckets[i].push(p);
        self.size += 1;
        if self.size > self.buckets.len() * 3 / 4 {
            self.resize();
        }
        true
    }

    fn resize(&mut self) {
        let old = std::mem::take(&mut self.buckets);
        self.buckets = vec![Vec::new(); old.len() * 2];
        for p in old.into_iter().flatten() {
            let i = self.index(p);
            self.buckets[i].push(p);
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// The elements in the reference's iteration order.
    pub fn iter(&self) -> impl Iterator<Item = Pos> + '_ {
        self.buckets.iter().flatten().copied()
    }

    /// Removes and returns the first element in iteration order (a fresh iterator's `next`
    /// followed by `remove`).
    pub fn pop_first(&mut self) -> Option<Pos> {
        let b = self.buckets.iter_mut().find(|b| !b.is_empty())?;
        self.size -= 1;
        Some(b.remove(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-computed with the reference hash: (16,0,0) and (0,0,0) share bucket 0 (hashes 16 and
    /// 0), (1,0,0) and (0,0,1) share bucket 1 (hashes 1 and 961); insertion order decides inside.
    #[test]
    fn iterates_by_bucket_then_insertion() {
        let mut s = JavaSet::new();
        for p in [Pos::new(0, 0, 1), Pos::new(1, 0, 0), Pos::new(16, 0, 0), Pos::new(0, 0, 0)] {
            s.insert(p);
        }
        let order: Vec<_> = s.iter().collect();
        assert_eq!(order, [Pos::new(16, 0, 0), Pos::new(0, 0, 0), Pos::new(0, 0, 1), Pos::new(1, 0, 0)]);
    }

    #[test]
    fn resize_keeps_relative_order() {
        let mut s = JavaSet::new();
        for i in 0..13 {
            s.insert(Pos::new(i * 16, 0, 0));
        }
        // 13 elements pass the 12-element threshold: 32 buckets, indexes 0 and 16 alternate.
        let order: Vec<_> = s.iter().map(|p| p.x / 16).collect();
        assert_eq!(order, [0, 2, 4, 6, 8, 10, 12, 1, 3, 5, 7, 9, 11]);
    }
}
