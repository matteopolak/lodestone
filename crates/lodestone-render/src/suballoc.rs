//! GPU-independent buffer suballocation.
//!
//! `wgpu` does not suballocate buffers, and allocating one buffer per chunk
//! section would be catastrophic (we expect thousands of live sections). This
//! module implements the *pure* allocation policy over a single flat arena so
//! it can be exhaustively unit-tested with no GPU present. The GPU-backed
//! wrapper lives in [`crate::arena`] and delegates every allocation decision to
//! [`Suballocator`].
//!
//! ## Fragmentation policy
//!
//! The allocator uses **address-ordered first-fit with immediate boundary-tag
//! coalescing**:
//!
//! * The free list is a [`BTreeMap`] keyed by block offset, so it is always
//!   sorted by address. Allocation walks it in address order and takes the
//!   first block large enough (first-fit). Address-ordered first-fit is a
//!   well-studied policy with fragmentation behaviour close to best-fit but
//!   without the cost of scanning the whole list for the tightest hole.
//! * On [`Suballocator::free`] the returned block is immediately merged with an
//!   adjacent predecessor and/or successor free block if they are physically
//!   contiguous. This keeps external fragmentation from accumulating across
//!   alloc/free churn: any run of freed neighbours collapses back into one hole.
//! * Every request is rounded up to the arena alignment, so all offsets and
//!   sizes stay aligned and the GPU wrapper can hand offsets straight to
//!   `wgpu` without re-aligning.
//!
//! This is deliberately simple. A production tuned allocator would add
//! segregated free lists per size-class; the policy here is chosen so the
//! behaviour is obvious and testable, and the public API would not change if we
//! swap the internals later.

use std::collections::{BTreeMap, HashMap};

/// A contiguous span within an arena, measured in bytes from the arena base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Region {
    /// Byte offset of the span from the start of the arena.
    pub offset: u64,
    /// Length of the span in bytes (already rounded to the arena alignment).
    pub size: u64,
}

impl Region {
    /// One-past-the-end offset of the region.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.offset + self.size
    }
}

/// Errors produced by [`Suballocator`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SuballocError {
    /// The request could not be satisfied from the current free space.
    #[error("out of space: requested {requested} bytes, largest free block is {largest_free}")]
    OutOfSpace {
        /// Aligned size that was requested.
        requested: u64,
        /// Largest contiguous free block currently available.
        largest_free: u64,
    },
    /// A zero-sized allocation was requested.
    #[error("cannot allocate zero bytes")]
    ZeroSized,
    /// The region passed to [`Suballocator::free`] was never handed out (or was
    /// already freed): a double-free or a fabricated region.
    #[error("invalid free of region at offset {offset} (size {size}): not a live allocation")]
    InvalidFree {
        /// Offset of the offending region.
        offset: u64,
        /// Size of the offending region.
        size: u64,
    },
}

/// A point-in-time summary of arena occupancy, used for tests and telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocStats {
    /// Total arena capacity in bytes.
    pub capacity: u64,
    /// Bytes currently handed out to live allocations.
    pub used: u64,
    /// Bytes currently free (`capacity - used`).
    pub free: u64,
    /// Size of the largest single contiguous free block.
    pub largest_free_block: u64,
    /// Number of distinct free blocks (holes).
    pub free_block_count: usize,
    /// Number of live allocations.
    pub live_allocations: usize,
}

impl AllocStats {
    /// External fragmentation in `[0.0, 1.0]`: the fraction of free space that
    /// is *not* in the largest hole. `0.0` means all free space is contiguous.
    #[must_use]
    pub fn fragmentation(&self) -> f64 {
        if self.free == 0 {
            0.0
        } else {
            1.0 - (self.largest_free_block as f64 / self.free as f64)
        }
    }
}

/// A pure, GPU-independent free-list allocator over one flat arena.
#[derive(Debug, Clone)]
pub struct Suballocator {
    capacity: u64,
    align: u64,
    /// Free blocks keyed by offset (address-ordered).
    free: BTreeMap<u64, u64>,
    /// Live allocations keyed by offset -> size, for free validation.
    live: HashMap<u64, u64>,
}

impl Suballocator {
    /// Create an allocator managing `capacity` bytes with the given power-of-two
    /// `align`. `align` is clamped to at least 1 and every allocation size is
    /// rounded up to a multiple of it.
    ///
    /// # Panics
    /// Panics if `align` is not a power of two.
    #[must_use]
    pub fn new(capacity: u64, align: u64) -> Self {
        let align = align.max(1);
        assert!(align.is_power_of_two(), "align must be a power of two");
        let capacity = align_up(capacity, align);
        let mut free = BTreeMap::new();
        if capacity > 0 {
            free.insert(0, capacity);
        }
        Self {
            capacity,
            align,
            free,
            live: HashMap::new(),
        }
    }

    /// Total arena capacity in bytes.
    #[must_use]
    pub const fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Alignment applied to every allocation.
    #[must_use]
    pub const fn align(&self) -> u64 {
        self.align
    }

    /// Allocate `size` bytes (rounded up to the alignment) using address-ordered
    /// first-fit.
    ///
    /// # Errors
    /// Returns [`SuballocError::ZeroSized`] for a zero request and
    /// [`SuballocError::OutOfSpace`] when no single free block is large enough.
    pub fn allocate(&mut self, size: u64) -> Result<Region, SuballocError> {
        if size == 0 {
            return Err(SuballocError::ZeroSized);
        }
        let need = align_up(size, self.align);

        // First-fit in address order.
        let mut chosen: Option<(u64, u64)> = None;
        for (&off, &blk) in &self.free {
            if blk >= need {
                chosen = Some((off, blk));
                break;
            }
        }

        let (off, blk) = chosen.ok_or_else(|| SuballocError::OutOfSpace {
            requested: need,
            largest_free: self.largest_free_block(),
        })?;

        self.free.remove(&off);
        let remainder = blk - need;
        if remainder > 0 {
            self.free.insert(off + need, remainder);
        }
        self.live.insert(off, need);
        Ok(Region {
            offset: off,
            size: need,
        })
    }

    /// Return a previously allocated region to the free pool, coalescing with
    /// physically adjacent free blocks.
    ///
    /// # Errors
    /// Returns [`SuballocError::InvalidFree`] if `region` does not exactly match
    /// a currently live allocation (guards against double-free and fabricated
    /// regions).
    pub fn free(&mut self, region: Region) -> Result<(), SuballocError> {
        match self.live.get(&region.offset) {
            Some(&size) if size == region.size => {
                self.live.remove(&region.offset);
            }
            _ => {
                return Err(SuballocError::InvalidFree {
                    offset: region.offset,
                    size: region.size,
                });
            }
        }

        let mut off = region.offset;
        let mut size = region.size;

        // Coalesce with successor.
        if let Some(&next_size) = self.free.get(&(off + size)) {
            self.free.remove(&(off + size));
            size += next_size;
        }

        // Coalesce with predecessor.
        if let Some((&prev_off, &prev_size)) = self.free.range(..off).next_back()
            && prev_off + prev_size == off
        {
            self.free.remove(&prev_off);
            off = prev_off;
            size += prev_size;
        }

        self.free.insert(off, size);
        Ok(())
    }

    /// Largest contiguous free block, or `0` if the arena is full.
    #[must_use]
    pub fn largest_free_block(&self) -> u64 {
        self.free.values().copied().max().unwrap_or(0)
    }

    /// Snapshot of current occupancy.
    #[must_use]
    pub fn stats(&self) -> AllocStats {
        let free: u64 = self.free.values().copied().sum();
        AllocStats {
            capacity: self.capacity,
            used: self.capacity - free,
            free,
            largest_free_block: self.largest_free_block(),
            free_block_count: self.free.len(),
            live_allocations: self.live.len(),
        }
    }
}

/// Round `value` up to the next multiple of `align` (a power of two).
const fn align_up(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + (align - 1)) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_arena_is_one_free_block() {
        let a = Suballocator::new(1024, 256);
        let s = a.stats();
        assert_eq!(s.capacity, 1024);
        assert_eq!(s.used, 0);
        assert_eq!(s.free, 1024);
        assert_eq!(s.largest_free_block, 1024);
        assert_eq!(s.free_block_count, 1);
        assert_eq!(s.fragmentation(), 0.0);
    }

    #[test]
    fn allocation_rounds_up_to_alignment() {
        let mut a = Suballocator::new(1024, 256);
        let r = a.allocate(1).unwrap();
        assert_eq!(r.offset, 0);
        assert_eq!(r.size, 256);
        assert_eq!(a.stats().used, 256);
    }

    #[test]
    fn allocations_are_contiguous_and_aligned() {
        let mut a = Suballocator::new(1024, 256);
        let r0 = a.allocate(200).unwrap();
        let r1 = a.allocate(200).unwrap();
        assert_eq!(r0.offset, 0);
        assert_eq!(r1.offset, 256);
        assert_eq!(r1.offset % 256, 0);
    }

    #[test]
    fn zero_sized_is_rejected() {
        let mut a = Suballocator::new(1024, 256);
        assert_eq!(a.allocate(0), Err(SuballocError::ZeroSized));
    }

    #[test]
    fn exhaustion_reports_largest_free() {
        let mut a = Suballocator::new(512, 256);
        let _ = a.allocate(256).unwrap();
        let _ = a.allocate(256).unwrap();
        let err = a.allocate(256).unwrap_err();
        assert_eq!(
            err,
            SuballocError::OutOfSpace {
                requested: 256,
                largest_free: 0
            }
        );
    }

    #[test]
    fn free_returns_space_and_coalesces_neighbours() {
        let mut a = Suballocator::new(768, 256);
        let r0 = a.allocate(256).unwrap();
        let r1 = a.allocate(256).unwrap();
        let r2 = a.allocate(256).unwrap();
        assert_eq!(a.stats().free_block_count, 0);

        // Free the middle then the two neighbours; all should coalesce to one.
        a.free(r1).unwrap();
        assert_eq!(a.stats().free_block_count, 1);
        a.free(r0).unwrap();
        a.free(r2).unwrap();
        let s = a.stats();
        assert_eq!(s.free, 768);
        assert_eq!(s.free_block_count, 1, "all holes should coalesce into one");
        assert_eq!(s.largest_free_block, 768);
    }

    #[test]
    fn coalesce_with_predecessor_only() {
        let mut a = Suballocator::new(768, 256);
        let r0 = a.allocate(256).unwrap();
        let r1 = a.allocate(256).unwrap();
        let _r2 = a.allocate(256).unwrap();
        a.free(r0).unwrap();
        a.free(r1).unwrap();
        // r0+r1 merge into a 512 hole; r2 still live.
        let s = a.stats();
        assert_eq!(s.free_block_count, 1);
        assert_eq!(s.largest_free_block, 512);
    }

    #[test]
    fn fragmentation_measures_scattered_holes() {
        let mut a = Suballocator::new(1024, 256);
        let r0 = a.allocate(256).unwrap();
        let _r1 = a.allocate(256).unwrap();
        let r2 = a.allocate(256).unwrap();
        let _r3 = a.allocate(256).unwrap();
        // Free two non-adjacent blocks -> two separate 256 holes.
        a.free(r0).unwrap();
        a.free(r2).unwrap();
        let s = a.stats();
        assert_eq!(s.free, 512);
        assert_eq!(s.free_block_count, 2);
        assert_eq!(s.largest_free_block, 256);
        assert!((s.fragmentation() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn reuse_after_free_fills_the_hole() {
        let mut a = Suballocator::new(1024, 256);
        let r0 = a.allocate(256).unwrap();
        let _r1 = a.allocate(256).unwrap();
        a.free(r0).unwrap();
        // first-fit should reuse the freed hole at offset 0.
        let r = a.allocate(256).unwrap();
        assert_eq!(r.offset, 0);
    }

    #[test]
    fn double_free_is_rejected() {
        let mut a = Suballocator::new(512, 256);
        let r = a.allocate(256).unwrap();
        a.free(r).unwrap();
        assert_eq!(
            a.free(r),
            Err(SuballocError::InvalidFree {
                offset: r.offset,
                size: r.size
            })
        );
    }

    #[test]
    fn fabricated_region_free_is_rejected() {
        let mut a = Suballocator::new(512, 256);
        let bogus = Region {
            offset: 128,
            size: 64,
        };
        assert!(matches!(
            a.free(bogus),
            Err(SuballocError::InvalidFree { .. })
        ));
    }

    #[test]
    fn churn_keeps_space_accounted() {
        let mut a = Suballocator::new(4096, 64);
        let mut live = Vec::new();
        for i in 0..32 {
            live.push(a.allocate(64 + (i % 3) * 64).unwrap());
        }
        // Free every other allocation, then refill.
        for r in live.iter().step_by(2).copied().collect::<Vec<_>>() {
            a.free(r).unwrap();
            live.retain(|x| *x != r);
        }
        let mut total_after: u64 = a.stats().used;
        while let Ok(r) = a.allocate(64) {
            total_after += 64;
            live.push(r);
            if total_after >= a.capacity() {
                break;
            }
        }
        let s = a.stats();
        assert_eq!(s.used + s.free, s.capacity);
        assert!(s.used <= s.capacity);
    }
}
