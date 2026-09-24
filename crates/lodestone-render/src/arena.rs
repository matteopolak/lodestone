//! GPU-backed arena buffer that suballocates via the pure [`Suballocator`].
//!
//! One `wgpu::Buffer` per chunk section would be disastrous at the thousands of
//! sections we expect, and `wgpu` does not suballocate. [`ArenaBuffer`] owns a
//! single large buffer and hands out [`ArenaAllocation`] handles whose offsets
//! come straight from [`Suballocator`] — all the allocation policy and its
//! tests live in [`crate::suballoc`], with no GPU required.

use crate::suballoc::{AllocStats, Region, SuballocError, Suballocator};

/// A handle to a suballocated span within an [`ArenaBuffer`]. Return it to
/// [`ArenaBuffer::free`] when the mesh it backs is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArenaAllocation {
    region: Region,
}

impl ArenaAllocation {
    /// Byte offset of this allocation within the arena buffer.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.region.offset
    }

    /// Aligned byte length of this allocation.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.region.size
    }
}

/// Errors from arena operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArenaError {
    /// Underlying allocation failure.
    #[error(transparent)]
    Alloc(#[from] SuballocError),
    /// A write was larger than the allocation it targeted.
    #[error("write of {data} bytes exceeds allocation of {capacity} bytes")]
    WriteTooLarge {
        /// Bytes the caller tried to write.
        data: u64,
        /// Capacity of the target allocation.
        capacity: u64,
    },
}

/// A large `wgpu::Buffer` that is suballocated for the chunk-mesh workload.
///
/// A mesh producer allocates a span, writes packed vertex/index bytes into it,
/// and later frees it; the draw strategies consume the resulting offsets via
/// [`DrawRegion`](crate::strategy::DrawRegion).
#[derive(Debug)]
pub struct ArenaBuffer {
    buffer: wgpu::Buffer,
    alloc: Suballocator,
}

impl ArenaBuffer {
    /// Minimum alignment `wgpu` requires for buffer copies/writes.
    pub const MIN_ALIGN: u64 = wgpu::COPY_BUFFER_ALIGNMENT;

    /// Create an arena of `capacity` bytes with the given `usage`. `align` is
    /// raised to at least [`ArenaBuffer::MIN_ALIGN`] so every handed-out offset
    /// is a legal copy destination.
    ///
    /// # Panics
    /// Panics if `align` is not a power of two.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        label: &str,
        capacity: u64,
        align: u64,
        usage: wgpu::BufferUsages,
    ) -> Self {
        let align = align.max(Self::MIN_ALIGN);
        let alloc = Suballocator::new(capacity, align);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: alloc.capacity(),
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { buffer, alloc }
    }

    /// Reserve `size` bytes (rounded up to the arena alignment).
    ///
    /// # Errors
    /// Propagates [`SuballocError`] on exhaustion or a zero request.
    pub fn allocate(&mut self, size: u64) -> Result<ArenaAllocation, ArenaError> {
        let region = self.alloc.allocate(size)?;
        Ok(ArenaAllocation { region })
    }

    /// Upload `data` into a previously reserved allocation via the queue.
    ///
    /// # Errors
    /// Returns [`ArenaError::WriteTooLarge`] if `data` is bigger than the
    /// allocation.
    pub fn write(
        &self,
        queue: &wgpu::Queue,
        allocation: &ArenaAllocation,
        data: &[u8],
    ) -> Result<(), ArenaError> {
        let len = data.len() as u64;
        if len > allocation.size() {
            return Err(ArenaError::WriteTooLarge {
                data: len,
                capacity: allocation.size(),
            });
        }
        queue.write_buffer(&self.buffer, allocation.offset(), data);
        Ok(())
    }

    /// Release an allocation back to the free pool (coalescing neighbours).
    ///
    /// # Errors
    /// Returns [`ArenaError::Alloc`] wrapping [`SuballocError::InvalidFree`] on a
    /// double-free or fabricated handle.
    pub fn free(&mut self, allocation: ArenaAllocation) -> Result<(), ArenaError> {
        self.alloc.free(allocation.region)?;
        Ok(())
    }

    /// The underlying GPU buffer (e.g. to bind as vertex/index/storage buffer).
    #[must_use]
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// Current occupancy snapshot.
    #[must_use]
    pub fn stats(&self) -> AllocStats {
        self.alloc.stats()
    }
}
