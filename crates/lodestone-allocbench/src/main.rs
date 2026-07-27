//! Allocator evaluation harness for Lodestone.
//!
//! Builds one binary per global allocator (selected by a cargo feature). The
//! workload models Lodestone's dominant allocation pattern: chunk section
//! storage and mesh vertex/index buffers produced on worker threads and freed
//! either locally or — the interesting case — on a *different* thread, which is
//! what actually happens when meshes are handed to the render thread for upload.
//!
//! Throughput is printed to stdout; peak RSS is measured externally by running
//! this binary under `/usr/bin/time -l` (see `bench.sh`), so nothing here needs
//! `libc`/`getrusage` and the crate stays free of `unsafe`.

// --- Global allocator selection -------------------------------------------
// Exactly one allocator is compiled in. `alloc-system` is the implicit default
// and pulls in no crate (the platform allocator is used). Enabling more than
// one non-system allocator is a hard error.

#[cfg(any(
    all(feature = "alloc-mimalloc", feature = "alloc-snmalloc"),
    all(feature = "alloc-mimalloc", feature = "alloc-jemalloc"),
    all(feature = "alloc-snmalloc", feature = "alloc-jemalloc"),
))]
compile_error!(
    "select at most one of `alloc-mimalloc`, `alloc-snmalloc`, `alloc-jemalloc`; \
     omit them all to use the system allocator"
);

#[cfg(feature = "alloc-mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(all(feature = "alloc-snmalloc", not(feature = "alloc-mimalloc")))]
#[global_allocator]
static GLOBAL: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

#[cfg(all(
    feature = "alloc-jemalloc",
    not(any(feature = "alloc-mimalloc", feature = "alloc-snmalloc"))
))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(feature = "alloc-mimalloc")]
const ALLOCATOR: &str = "mimalloc";
#[cfg(all(feature = "alloc-snmalloc", not(feature = "alloc-mimalloc")))]
const ALLOCATOR: &str = "snmalloc";
#[cfg(all(
    feature = "alloc-jemalloc",
    not(any(feature = "alloc-mimalloc", feature = "alloc-snmalloc"))
))]
const ALLOCATOR: &str = "jemalloc";
#[cfg(not(any(
    feature = "alloc-mimalloc",
    feature = "alloc-snmalloc",
    feature = "alloc-jemalloc"
)))]
const ALLOCATOR: &str = "system";

use std::collections::VecDeque;
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Barrier};
use std::time::Instant;

/// Bits-per-entry values a paletted chunk section actually uses. Each maps to a
/// section storage allocation of `4096 * bpe / 8` bytes (512 B .. 7.5 KiB).
const BPE_CLASSES: [usize; 9] = [1, 2, 3, 4, 5, 6, 7, 8, 15];

/// How many buffers each worker keeps live before disposing the oldest. Models
/// "hold a while" so the allocator reaches steady state instead of alloc/free
/// ping-ponging a single block.
const HOLD_WINDOW: usize = 768;

/// Fraction (out of 256) of disposed buffers freed on the consumer thread in
/// `cross` mode. ~60% — a meaningful majority, matching mesh upload handoff.
const CROSS_FREE_NUM: u32 = 154;

/// Bound on the cross-thread free queue: the render/upload thread keeps pace,
/// so an unbounded backlog would be unrealistic (and would inflate RSS).
const FREE_QUEUE_BOUND: usize = 2048;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Every allocation is freed on the thread that made it.
    Local,
    /// A majority of allocations are freed on a separate consumer thread.
    Cross,
}

/// Tiny deterministic PRNG (SplitMix64). Deterministic so every allocator does
/// byte-for-byte identical work; only wall time and RSS differ.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next() as usize) % (hi - lo)
    }
}

/// Allocate `size` bytes and fully populate them, mirroring how Lodestone
/// actually uses these buffers: paletted section arrays and mesh vertex/index
/// data are written in full, not left zeroed. We deliberately allocate via
/// `with_capacity` + `resize` (a plain `alloc`, then a fill) rather than
/// `vec![0u8; size]` (`alloc_zeroed`) so that no allocator can "win" merely by
/// handing back fresh OS-zeroed pages and skipping the memset — the fill cost is
/// then identical across allocators and only the alloc/free path differs.
/// Returns a checksum so the work can't be optimised away.
#[inline]
fn make_buffer(size: usize, tag: u8, out: &mut Vec<Vec<u8>>) -> u64 {
    let mut v: Vec<u8> = Vec::with_capacity(size);
    v.resize(size, tag);
    let sum = if size > 0 {
        (v[0] as u64) ^ (v[size / 2] as u64) ^ (v[size - 1] as u64)
    } else {
        0
    };
    out.push(v);
    sum
}

/// Process one chunk column worth of allocations into `window`.
#[inline]
fn produce_column(rng: &mut Rng, window: &mut Vec<Vec<u8>>) -> u64 {
    let mut sum = 0u64;
    // 8..24 paletted sections.
    let sections = rng.range(8, 25);
    for _ in 0..sections {
        let bpe = BPE_CLASSES[rng.range(0, BPE_CLASSES.len())];
        let size = 4096 * bpe / 8;
        sum ^= make_buffer(size, 0xA5, window);
    }
    // 2..5 mesh buffers, a few KiB .. a few hundred KiB.
    let meshes = rng.range(2, 6);
    for _ in 0..meshes {
        let size = rng.range(2 * 1024, 300 * 1024);
        sum ^= make_buffer(size, 0x5A, window);
    }
    sum
}

struct WorkerResult {
    ops: u64,
    checksum: u64,
}

fn run(threads: usize, mode: Mode, columns_per_thread: u64, warmup_cols: u64) -> (f64, u64) {
    // Consumer (render/upload) thread for cross-thread frees.
    let (tx, rx) = sync_channel::<Vec<u8>>(FREE_QUEUE_BOUND);
    let consumer = if mode == Mode::Cross {
        Some(std::thread::spawn(move || {
            let mut freed = 0u64;
            let mut sum = 0u64;
            for buf in rx {
                // Touch then drop => the free happens on this thread.
                if !buf.is_empty() {
                    sum = sum.wrapping_add(buf[buf.len() - 1] as u64);
                }
                freed += 1;
                drop(buf);
            }
            (freed, sum)
        }))
    } else {
        drop(rx);
        None
    };

    let barrier = Arc::new(Barrier::new(threads + 1));
    let mut handles = Vec::with_capacity(threads);

    for t in 0..threads {
        let barrier = Arc::clone(&barrier);
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            let mut rng = Rng(0xD1B5_4A32 ^ ((t as u64) << 33 | (t as u64 + 1)));
            let mut window: Vec<Vec<u8>> = Vec::with_capacity(HOLD_WINDOW + 64);
            let mut disp: VecDeque<Vec<u8>> = VecDeque::new();
            let mut checksum = 0u64;
            let mut ops = 0u64;
            let mut cross_ctr: u32 = 0;

            let dispose = |buf: Vec<u8>, disp: &mut VecDeque<Vec<u8>>, cross: &mut u32| {
                if mode == Mode::Cross {
                    *cross = cross.wrapping_add(97);
                    if (*cross & 0xFF) < CROSS_FREE_NUM {
                        // Cross-thread free. If the consumer is saturated this
                        // blocks (backpressure), which is realistic.
                        let _ = tx.send(buf);
                        return;
                    }
                }
                disp.push_back(buf); // freed here when it falls out of scope below
                if disp.len() > 8 {
                    disp.pop_front();
                }
            };

            // Warmup: reach steady state, untimed.
            for c in 0..warmup_cols {
                checksum ^= produce_column(&mut rng, &mut window);
                while window.len() > HOLD_WINDOW {
                    let b = window.swap_remove(0);
                    dispose(b, &mut disp, &mut cross_ctr);
                }
                let _ = c;
            }

            barrier.wait();
            let start = Instant::now();
            for _ in 0..columns_per_thread {
                let before = window.len();
                checksum ^= produce_column(&mut rng, &mut window);
                ops += (window.len() - before) as u64;
                while window.len() > HOLD_WINDOW {
                    let b = window.swap_remove(0);
                    dispose(b, &mut disp, &mut cross_ctr);
                }
            }
            let elapsed = start.elapsed().as_secs_f64();

            // Drain remaining live buffers (freed here, after timing).
            for b in window.drain(..) {
                dispose(b, &mut disp, &mut cross_ctr);
            }
            drop(disp);
            drop(tx);
            (elapsed, WorkerResult { ops, checksum })
        }));
    }

    // Release the extra sender so the consumer can terminate once workers finish.
    drop(tx);

    barrier.wait();
    let mut max_elapsed = 0f64;
    let mut total_ops = 0u64;
    let mut checksum = 0u64;
    for h in handles {
        let (elapsed, res) = h.join().expect("worker panicked");
        max_elapsed = max_elapsed.max(elapsed);
        total_ops += res.ops;
        checksum ^= res.checksum;
    }
    if let Some(c) = consumer {
        let (_freed, sum) = c.join().expect("consumer panicked");
        checksum ^= sum;
    }

    // Prevent the optimiser from eliding the whole workload.
    if checksum == 0xDEAD_BEEF_DEAD_BEEF {
        eprintln!("checksum sentinel");
    }
    (max_elapsed, total_ops)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let threads: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);
    let mode = match args.get(2).map(String::as_str) {
        Some("cross") => Mode::Cross,
        Some("local") | None => Mode::Local,
        Some(other) => {
            eprintln!("unknown mode `{other}` (use `local` or `cross`)");
            std::process::exit(2);
        }
    };
    // Total chunk columns of work, split across threads. Fixed work => fair.
    let total_columns: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(60_000);
    let columns_per_thread = (total_columns / threads as u64).max(1);
    let warmup_cols = (columns_per_thread / 5).max(1);

    let (elapsed, ops) = run(threads, mode, columns_per_thread, warmup_cols);
    let mode_s = if mode == Mode::Cross {
        "cross"
    } else {
        "local"
    };
    let throughput = ops as f64 / elapsed;
    // Machine-parseable line for the driver, plus a human summary.
    println!(
        "RESULT allocator={ALLOCATOR} threads={threads} mode={mode_s} ops={ops} \
         elapsed_s={elapsed:.4} throughput_ops_per_s={throughput:.0}"
    );
}
