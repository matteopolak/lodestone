//! A lock-free single-producer / single-consumer ring buffer of `f32` samples.
//!
//! # The constraint it satisfies
//!
//! [`Mixer::render`](crate::Mixer::render) runs on a realtime audio callback —
//! a native `cpal` thread or a browser `AudioWorklet` — and **may not block or
//! allocate**. Streaming a long track therefore cannot decode inside `render`;
//! instead a producer (a native thread, or a browser worker) decodes ahead with
//! [`VorbisStream`](crate::stream::VorbisStream) and pushes samples into this
//! ring, while `render` pulls whatever is ready with [`read`](SampleRing::read).
//! Both `read` and `write` are wait-free and never allocate: they operate on a
//! buffer allocated once at construction.
//!
//! # Correctness model — safe Rust, no `unsafe`
//!
//! This crate denies `unsafe_code`, so the ring is built from safe atomics
//! rather than a hand-rolled `UnsafeCell` structure. Each sample slot is an
//! [`AtomicU32`] holding the sample's `f32::to_bits` pattern (a lossless, exact
//! round-trip). Two free-running counters — `head` for the consumer, `tail` for
//! the producer — give occupancy `tail - head`, with a power-of-two capacity so
//! the index is `counter & mask`.
//!
//! The producer publishes with a `Release` store to `tail`; the consumer
//! observes with an `Acquire` load, and symmetrically for `head`. Because every
//! slot is itself atomic, there is no data race even independently of that
//! ordering; the `Release`/`Acquire` pairing is what makes the *FIFO* semantics
//! hold (a consumer that has seen `tail` advance has also seen the slot writes
//! that preceded it).
//!
//! The single-threaded invariants (wraparound, partial read on underrun, partial
//! write when full, exact sample values) are unit-tested by asserting on the
//! actual samples that come out. A native producer/consumer stress test
//! additionally exercises the concurrent path. On `wasm32` without threads the
//! ring is simply used single-threaded; the atomics degrade to plain loads and
//! stores and the logic is unchanged.
//!
//! **Invariant the caller must uphold:** at most one producer calls `write` and
//! at most one consumer calls `read`. The structure is memory-safe under any
//! usage (every slot is atomic), but FIFO ordering only holds for one producer
//! and one consumer.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// A wait-free SPSC ring buffer of `f32` samples with a fixed, power-of-two
/// capacity.
#[derive(Debug)]
pub struct SampleRing {
    /// Sample storage, one `AtomicU32` (an `f32` bit pattern) per slot.
    buf: Box<[AtomicU32]>,
    /// `capacity - 1`; capacity is a power of two so `index & mask` wraps.
    mask: usize,
    /// Free-running count of samples the consumer has taken. Written only by the
    /// consumer.
    head: AtomicUsize,
    /// Free-running count of samples the producer has published. Written only by
    /// the producer.
    tail: AtomicUsize,
}

impl SampleRing {
    /// Creates a ring whose capacity is the smallest power of two `>= min_len`
    /// (and at least 2). All samples start at `0.0`.
    pub fn with_min_capacity(min_len: usize) -> Self {
        let capacity = min_len.max(2).next_power_of_two();
        let mut v = Vec::with_capacity(capacity);
        v.resize_with(capacity, || AtomicU32::new(0));
        Self {
            buf: v.into_boxed_slice(),
            mask: capacity - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// The total number of samples the ring can hold.
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// The number of samples currently available to read. Safe to call from
    /// either side; it is a snapshot and may be stale the instant it returns.
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    /// Whether the ring currently holds no readable samples.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The number of samples that can be written before the ring is full.
    pub fn free(&self) -> usize {
        self.capacity() - self.len()
    }

    /// **Producer side.** Copies as many samples from `src` as fit, returning the
    /// count actually written (`< src.len()` when the ring fills). Wait-free and
    /// non-allocating.
    ///
    /// Must be called by at most one thread for FIFO ordering to hold.
    pub fn write(&self, src: &[f32]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let free = self.capacity() - tail.wrapping_sub(head);
        let n = free.min(src.len());
        for (i, &s) in src.iter().take(n).enumerate() {
            let idx = tail.wrapping_add(i) & self.mask;
            self.buf[idx].store(s.to_bits(), Ordering::Relaxed);
        }
        self.tail.store(tail.wrapping_add(n), Ordering::Release);
        n
    }

    /// **Consumer side.** Fills as much of `dst` as there are samples available,
    /// returning the count actually read (`< dst.len()` on underrun). The caller
    /// is responsible for zero-filling any remainder — this is the realtime
    /// `render` path, so it never blocks waiting for the producer. Wait-free and
    /// non-allocating.
    ///
    /// Must be called by at most one thread for FIFO ordering to hold.
    pub fn read(&self, dst: &mut [f32]) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let avail = tail.wrapping_sub(head);
        let n = avail.min(dst.len());
        for (i, out) in dst.iter_mut().take(n).enumerate() {
            let idx = head.wrapping_add(i) & self.mask;
            *out = f32::from_bits(self.buf[idx].load(Ordering::Relaxed));
        }
        self.head.store(head.wrapping_add(n), Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_rounds_up_to_power_of_two() {
        assert_eq!(SampleRing::with_min_capacity(1).capacity(), 2);
        assert_eq!(SampleRing::with_min_capacity(3).capacity(), 4);
        assert_eq!(SampleRing::with_min_capacity(1000).capacity(), 1024);
        assert_eq!(SampleRing::with_min_capacity(1024).capacity(), 1024);
    }

    #[test]
    fn write_then_read_returns_exact_samples() {
        let ring = SampleRing::with_min_capacity(8);
        let src = [0.1, 0.2, 0.3, 0.4];
        assert_eq!(ring.write(&src), 4);
        assert_eq!(ring.len(), 4);

        let mut dst = [0.0f32; 4];
        assert_eq!(ring.read(&mut dst), 4);
        assert_eq!(dst, src);
        assert!(ring.is_empty());
    }

    #[test]
    fn write_saturates_when_full_and_reports_short_count() {
        let ring = SampleRing::with_min_capacity(4); // capacity 4
        let src = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(ring.write(&src), 4); // only 4 fit
        assert_eq!(ring.free(), 0);
        assert_eq!(ring.write(&[9.0]), 0); // nothing more fits

        let mut dst = [0.0f32; 4];
        assert_eq!(ring.read(&mut dst), 4);
        assert_eq!(dst, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn read_underruns_to_partial_and_leaves_dst_remainder_untouched() {
        let ring = SampleRing::with_min_capacity(8);
        assert_eq!(ring.write(&[7.0, 8.0]), 2);

        let mut dst = [-1.0f32; 5];
        assert_eq!(ring.read(&mut dst), 2);
        // Only the first two are written; the caller zero-fills the rest.
        assert_eq!(&dst[..2], &[7.0, 8.0]);
        assert_eq!(&dst[2..], &[-1.0, -1.0, -1.0]);
    }

    #[test]
    fn indices_wrap_correctly_across_the_capacity_boundary() {
        // Capacity 4. Advance head/tail to the wrap point, then push a run that
        // straddles index 3 → 0 and assert the values survive the wrap in order.
        let ring = SampleRing::with_min_capacity(4);
        assert_eq!(ring.write(&[0.0, 0.0, 0.0]), 3);
        let mut sink = [0.0f32; 3];
        assert_eq!(ring.read(&mut sink), 3);
        assert_eq!(ring.len(), 0);

        // Now tail == head == 3. Write 4 samples: indices 3,0,1,2.
        let src = [10.0, 20.0, 30.0, 40.0];
        assert_eq!(ring.write(&src), 4);
        let mut dst = [0.0f32; 4];
        assert_eq!(ring.read(&mut dst), 4);
        assert_eq!(dst, src);
    }

    #[test]
    fn interleaved_partial_writes_and_reads_preserve_fifo_order() {
        let ring = SampleRing::with_min_capacity(4);
        let mut expected_next = 0.0f32;
        let mut produced = 0.0f32;
        let mut consumed = Vec::new();

        // Repeatedly try to write small runs and read small runs; assert every
        // sample comes out exactly once, in order.
        for _ in 0..50 {
            let batch: Vec<f32> = (0..3).map(|k| produced + k as f32).collect();
            let w = ring.write(&batch);
            produced += w as f32;

            let mut dst = [0.0f32; 2];
            let r = ring.read(&mut dst);
            for &x in &dst[..r] {
                consumed.push(x);
            }
        }
        // Drain the rest.
        loop {
            let mut dst = [0.0f32; 8];
            let r = ring.read(&mut dst);
            if r == 0 {
                break;
            }
            consumed.extend_from_slice(&dst[..r]);
        }

        for x in consumed {
            assert_eq!(x, expected_next);
            expected_next += 1.0;
        }
        assert_eq!(expected_next, produced);
    }

    // Cross-thread evidence: not a proof, but it exercises the Acquire/Release
    // pairing under real concurrency. wasm32 has no threads, so this is native
    // only — the single-threaded tests above cover the wasm usage.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn threaded_producer_consumer_transfers_a_ramp_losslessly() {
        use std::sync::Arc;

        const N: usize = 200_000;
        let ring = Arc::new(SampleRing::with_min_capacity(1024));

        let prod_ring = Arc::clone(&ring);
        let producer = std::thread::spawn(move || {
            let mut i = 0usize;
            while i < N {
                let end = (i + 97).min(N);
                let batch: Vec<f32> = (i..end).map(|k| k as f32).collect();
                let mut off = 0;
                while off < batch.len() {
                    off += prod_ring.write(&batch[off..]);
                }
                i = end;
            }
        });

        let mut received = 0usize;
        let mut expected = 0.0f32;
        let mut buf = [0.0f32; 128];
        while received < N {
            let r = ring.read(&mut buf);
            for &x in &buf[..r] {
                assert_eq!(x, expected, "sample {received} out of order");
                expected += 1.0;
                received += 1;
            }
        }
        producer.join().unwrap();
        assert_eq!(received, N);
    }
}
