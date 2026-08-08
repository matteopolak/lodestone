//! Where the join burst's parallelism actually goes: **instructions retired** for
//! the same 289 columns, serial against a window sweep through the production
//! scheduler, plus the two contention counters that name the loser.
//!
//! # The question
//!
//! §12.112 measured 361 columns in 6.3–6.9 s while a serial column cost 15–21 ms.
//! That is an effective speedup of about **1×** on a 10-core machine with a
//! generation window of 20 (`join_scheduler::generation_window`). §12.130 then cut
//! the serial column from 79.9 ms to 21.0 ms, so the remaining gap to Matthew's
//! "2,000 chunks in a couple of seconds" is entirely parallelism.
//!
//! Three contention hazards were on the record, none re-measured since the staged
//! store landed: the 708 shared `Cache2D` try-lock slots (§12.102), the state
//! interner's `RwLock`, and the store's per-entry `OnceLock` waits. This binary
//! decides between them, and the decisive instrument is not a timing.
//!
//! # Why instructions retired, and what it can and cannot say
//!
//! A wall-clock speedup conflates two entirely different failures:
//!
//! | failure | instructions retired | wall |
//! |---|---|---|
//! | redundant recomputation (a failed `try_lock` **is** a recompute) | rises | rises |
//! | workers parked on a lock or a `OnceLock` | flat | rises |
//!
//! So `I_parallel / I_serial` over the *same coordinates on a fresh generator each
//! time* separates them in one number, and it is load-balancing-independent: it
//! does not care which core ran what or how fast. Flat instructions with a 1× wall
//! is a **blocking** story and convicts nothing that recomputes; inflated
//! instructions is a **recompute** story and the two counters below say which
//! cache did it.
//!
//! Both are kept, because instructions are blind to locality (§12.120 measured
//! 490k instructions against 7× the time).
//!
//! # The two counters, and their calibration
//!
//! * `lodestone_worldgen_core::density::cache_2d_stats` — hits / misses /
//!   **contended**, where contended is a failed `try_lock` and therefore a
//!   recompute. Exactly **0** single-threaded, which is the calibration: a
//!   `try_lock` cannot fail with no other thread in the slot.
//! * `lodestone_worldgen::overworld::store::wait_stats` — how many `StageSlot`
//!   lookups were **parked** on another thread's `get_or_init`, and for how long
//!   in total. Also exactly **0** single-threaded, for the same structural reason.
//!
//! Both are process-global, so — as in `join_scheduler_counters.rs` — **nothing in
//! this binary may generate terrain except [`measure`]**, which runs every arm
//! sequentially inside one `OnceLock`.
//!
//! # Running it
//!
//! ```text
//! cargo test --release -p lodestone-server \
//!   --test join_parallel_efficiency -- --ignored --nocapture
//! ```
//!
//! **Without `--release` the numbers are meaningless** and the ratios are not even
//! comparable (a debug build changes which costs dominate). Do **not** add
//! `gen-counters`: those hooks inflate a burst ~3×, and a `try_lock` outcome is a
//! timing-dependent observable, so a counters build would report the contention of
//! a different system. Everything read here is live in a clean release build by
//! construction.

// `proc_pid_rusage` is an `extern "C"` call, and the workspace sets
// `unsafe_code = "deny"`. Scoped as narrowly as the lint allows — `#![allow]` is a
// crate-root attribute and cargo compiles each integration test as its own binary
// crate, so it cannot leak into the library or any other target. Same opt-out, for
// the same reason and against the same function, as
// `crates/lodestone-worldgen/benches/generation.rs` and
// `crates/lodestone-shell/tests/client_chunk_cycles.rs`.
#![allow(unsafe_code)]

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use lodestone_server::join_scheduler::{ColumnPipeline, generation_window};
use lodestone_server::overworld_chunk_source;
use lodestone_worldgen::density::{self, Cache2DStats};
use lodestone_worldgen::overworld::store::{self, WaitStats};

/// 17×17 at `view_radius = 8` — the burst `4307b59` named, and the scene §12.112
/// and `join_scheduler_counters.rs` both use.
const BURST_RADIUS: i32 = 8;
const BURST_COLUMNS: usize = 289;

/// The window sweep. `1` is the serial-through-the-scheduler arm, so the
/// scheduler's own overhead is held constant against every other arm rather than
/// being folded into the serial baseline; `generation_window()` (20 here) is
/// production. `40` is past `2 × available_parallelism` deliberately — if the
/// answer were "not enough work in flight", 40 would beat 20.
const WINDOWS: &[usize] = &[1, 5, 10, 20, 40];

/// The store closure of this burst, from `join_scheduler_counters.rs`'s derivation:
/// post-ore is reached over a 19×19, pre-ore over a 21×21. Used only to sanity-check
/// that an arm did the work it was supposed to.
const BURST_PRE_ORE_CLOSURE: usize = 21 * 21;

// --- instructions retired ---------------------------------------------------
//
// Transcribed from `crates/lodestone-worldgen/benches/generation.rs`, which took it
// from `crates/lodestone-shell/tests/client_chunk_cycles.rs`. A shared crate for
// this would be the right home once there is a third customer; for now the
// duplication is deliberate and the size assertion below is what makes a
// mis-transcription fail loudly instead of reading a neighbouring field.

/// `RUSAGE_INFO_V4` from `<sys/resource.h>`.
const RUSAGE_INFO_V4: i32 = 4;

/// `struct rusage_info_v4` from macOS `<sys/resource.h>`, field-by-field in
/// declaration order so `ri_instructions` is reached **by name**.
#[repr(C)]
#[derive(Default, Clone, Copy)]
#[allow(non_snake_case, dead_code)]
struct RusageInfoV4 {
    ri_uuid: [u8; 16],
    ri_user_time: u64,
    ri_system_time: u64,
    ri_pkg_idle_wkups: u64,
    ri_interrupt_wkups: u64,
    ri_pageins: u64,
    ri_wired_size: u64,
    ri_resident_size: u64,
    ri_phys_footprint: u64,
    ri_proc_start_abstime: u64,
    ri_proc_exit_abstime: u64,
    ri_child_user_time: u64,
    ri_child_system_time: u64,
    ri_child_pkg_idle_wkups: u64,
    ri_child_interrupt_wkups: u64,
    ri_child_pageins: u64,
    ri_child_elapsed_abstime: u64,
    ri_diskio_bytesread: u64,
    ri_diskio_byteswritten: u64,
    ri_cpu_time_qos_default: u64,
    ri_cpu_time_qos_maintenance: u64,
    ri_cpu_time_qos_background: u64,
    ri_cpu_time_qos_utility: u64,
    ri_cpu_time_qos_legacy: u64,
    ri_cpu_time_qos_user_initiated: u64,
    ri_cpu_time_qos_user_interactive: u64,
    ri_billed_system_time: u64,
    ri_serviced_system_time: u64,
    ri_logical_writes: u64,
    ri_lifetime_max_phys_footprint: u64,
    ri_instructions: u64,
    ri_cycles: u64,
    ri_billed_energy: u64,
    ri_serviced_energy: u64,
    ri_interval_max_phys_footprint: u64,
    ri_runnable_time: u64,
    ri_flags: u64,
}

/// What the transcription must weigh if every field is present and correctly
/// typed: a 16-byte UUID and 36 `u64`s. Derived from the field list, not measured.
const RUSAGE_INFO_V4_SIZE: usize = 16 + 36 * 8;

unsafe extern "C" {
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
}

/// Instructions retired by this **process** — every thread, which is exactly the
/// point here: a burst's total work is the sum over the pool.
fn instructions_retired() -> u64 {
    assert_eq!(
        size_of::<RusageInfoV4>(),
        RUSAGE_INFO_V4_SIZE,
        "the rusage_info_v4 transcription is the wrong size, so `ri_instructions` is \
         not the field being read"
    );
    let mut info = RusageInfoV4::default();
    let rc = unsafe {
        proc_pid_rusage(
            i32::try_from(std::process::id()).expect("pid fits in i32"),
            RUSAGE_INFO_V4,
            (&raw mut info).cast::<core::ffi::c_void>(),
        )
    };
    assert_eq!(rc, 0, "proc_pid_rusage(RUSAGE_INFO_V4) failed with {rc}");
    info.ri_instructions
}

// --- the arms ---------------------------------------------------------------

/// The wire order for a view of `radius` centred on `(ox, oz)` — rings outward,
/// `dz`-outer/`dx`-inner within a ring, mirroring `server.rs`'s private
/// `join_view_rings`. Same helper as `join_scheduler_counters.rs`.
fn wire_order(radius: i32, ox: i32, oz: i32) -> Vec<(i32, i32)> {
    (0..=radius)
        .flat_map(|r| {
            let mut ring = Vec::new();
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dz.abs()) == r {
                        ring.push((dx + ox, dz + oz));
                    }
                }
            }
            ring
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct Arm {
    /// `None` for the direct serial arm, `Some(w)` for a pipeline at window `w`.
    window: Option<usize>,
    wall: Duration,
    instructions: u64,
    cache_2d: Cache2DStats,
    waits: WaitStats,
    store_len: usize,
    evictions: usize,
    columns: usize,
    /// Whether the pipeline emitted in the order it was handed. Always `true` for
    /// the serial arm.
    in_order: bool,
}

impl Arm {
    fn insns_per_column(&self) -> f64 {
        self.instructions as f64 / self.columns as f64
    }
    fn us_per_column(&self) -> f64 {
        self.wall.as_nanos() as f64 / 1000.0 / self.columns as f64
    }
}

/// Runs one arm on a **fresh generator** over the same coordinates, and brackets it
/// with every instrument.
///
/// Fresh is the whole method: a warm store answers ~83% of a column (§12.130), so
/// an arm run after another arm on the same generator measures a different, much
/// cheaper program. `bench_worldgen.rs`'s sweep has exactly that defect, which is
/// why its 2.4–2.9× is not comparable to anything here.
fn run_arm(
    runtime: &tokio::runtime::Runtime,
    coords: &[(i32, i32)],
    window: Option<usize>,
) -> Arm {
    let source = Arc::new(overworld_chunk_source(42));

    density::reset_cache_2d_stats();
    store::reset_wait_stats();
    let insns_before = instructions_retired();
    let started = Instant::now();

    let (columns, in_order) = match window {
        None => {
            for &(cx, cz) in coords {
                let column = lodestone_server::ChunkSource::column(&*source, cx, cz);
                std::hint::black_box(column.solid_count());
            }
            (coords.len(), true)
        }
        Some(window) => runtime.block_on(async {
            let mut pipeline =
                ColumnPipeline::with_window(Arc::clone(&source), coords.to_vec(), window);
            let mut emitted = Vec::with_capacity(coords.len());
            while let Some((pos, column)) = pipeline.next().await {
                std::hint::black_box(column.solid_count());
                emitted.push(pos);
            }
            let n = emitted.len();
            (n, emitted == coords)
        }),
    };

    let wall = started.elapsed();
    let instructions = instructions_retired().saturating_sub(insns_before);

    Arm {
        window,
        wall,
        instructions,
        cache_2d: density::cache_2d_stats(),
        waits: store::wait_stats(),
        store_len: source.generator().store_len(),
        evictions: source.generator().store_evictions(),
        columns,
        in_order,
    }
}

struct Measurement {
    window: usize,
    serial: Arm,
    sweep: Vec<Arm>,
}

impl Measurement {
    /// The arm at production's window.
    fn production(&self) -> &Arm {
        self.sweep
            .iter()
            .find(|a| a.window == Some(self.window))
            .unwrap_or_else(|| {
                panic!(
                    "the sweep {WINDOWS:?} does not contain this machine's \
                     generation_window() = {}",
                    self.window
                )
            })
    }
}

/// The **only** thing in this binary that may generate terrain — the counters and
/// the instruction reading are process-global.
fn measure() -> &'static Measurement {
    static ONCE: OnceLock<Measurement> = OnceLock::new();
    ONCE.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            // Two worker threads: generation happens on the blocking pool, so the
            // async side only drives the window. Sibling agents share this machine.
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a multi-thread runtime, the flavour production's blocking pool needs");

        // One coordinate set for every arm, so the arms differ *only* in how the
        // work was scheduled. Each arm gets its own generator, so reusing the
        // origin cannot leak a warm store between them.
        let coords = wire_order(BURST_RADIUS, 700, 700);
        assert_eq!(coords.len(), BURST_COLUMNS);

        let serial = run_arm(&runtime, &coords, None);
        let sweep: Vec<Arm> = WINDOWS
            .iter()
            .map(|&w| run_arm(&runtime, &coords, Some(w)))
            .collect();

        let m = Measurement {
            window: generation_window(),
            serial,
            sweep,
        };
        m.report();
        m
    })
}

impl Measurement {
    fn report(&self) {
        let s = &self.serial;
        eprintln!(
            "\n[U-par] {BURST_COLUMNS} columns, seed 42, fresh generator per arm. \
             generation_window() = {}\n",
            self.window
        );
        eprintln!(
            "  {:<10} {:>10} {:>11} {:>10} {:>8} {:>10} {:>12} {:>9} {:>10}",
            "arm", "wall(s)", "us/col", "Ginsn", "I/I_ser", "speedup", "c2d cont%", "waits", "park%"
        );
        for arm in std::iter::once(s).chain(self.sweep.iter()) {
            let c2d = arm.cache_2d;
            let total = (c2d.hits + c2d.misses + c2d.contended).max(1);
            let workers = arm.window.unwrap_or(1) as f64;
            eprintln!(
                "  {:<10} {:>10.3} {:>11.0} {:>10.1} {:>8.3} {:>10.2} {:>12.4} {:>9} {:>10.1}",
                arm.window
                    .map_or_else(|| "serial".to_string(), |w| format!("window={w}")),
                arm.wall.as_secs_f64(),
                arm.us_per_column(),
                arm.instructions as f64 / 1e9,
                arm.instructions as f64 / s.instructions as f64,
                s.wall.as_secs_f64() / arm.wall.as_secs_f64(),
                100.0 * c2d.contended as f64 / total as f64,
                arm.waits.waits,
                100.0 * (arm.waits.wait_nanos as f64 / 1e9) / (arm.wall.as_secs_f64() * workers),
            );
        }
        eprintln!(
            "\n  serial:  {} c2d lookups ({} hits, {} misses, {} contended), \
             {} store computes in {:.2} s of compute time",
            s.cache_2d.hits + s.cache_2d.misses + s.cache_2d.contended,
            s.cache_2d.hits,
            s.cache_2d.misses,
            s.cache_2d.contended,
            s.waits.computes,
            s.waits.compute_nanos as f64 / 1e9,
        );
        for arm in &self.sweep {
            eprintln!(
                "  window={:<3} store_len={} evictions={} in_order={} | c2d {} contended of {} \
                 | {} waits totalling {:.2} s, {} computes totalling {:.2} s",
                arm.window.unwrap_or(0),
                arm.store_len,
                arm.evictions,
                arm.in_order,
                arm.cache_2d.contended,
                arm.cache_2d.hits + arm.cache_2d.misses + arm.cache_2d.contended,
                arm.waits.waits,
                arm.waits.wait_nanos as f64 / 1e9,
                arm.waits.computes,
                arm.waits.compute_nanos as f64 / 1e9,
            );
        }
        eprintln!(
            "\n  2,000 columns at production window: {:.1} s (serial would be {:.1} s)",
            2000.0 * self.production().us_per_column() / 1e6,
            2000.0 * s.us_per_column() / 1e6,
        );
    }
}

// --- gates ------------------------------------------------------------------

/// **The calibration for both contention counters.** A single-threaded arm cannot
/// fail a `try_lock` and cannot park on a `OnceLock`, so both must read exactly
/// zero — and both must be *non*-zero on some concurrent arm, or a zero above would
/// be an uncalled hook rather than an uncontended one.
///
/// This is the control that licenses reading either number: without the second half
/// a dead instrument and a contention-free system are the same observation.
#[test]
#[ignore = "289 columns of real embedded-data generation, six times; minutes in release"]
fn the_contention_counters_read_zero_serially_and_fire_concurrently() {
    let m = measure();
    let s = &m.serial;

    assert_eq!(
        s.cache_2d.contended, 0,
        "a single-threaded sweep failed {} try_locks; either another thread is in \
         this process's density tree or the counter is measuring something else",
        s.cache_2d.contended
    );
    assert_eq!(
        s.waits.waits, 0,
        "a single-threaded sweep parked on {} OnceLocks, which is structurally \
         impossible — the wait/compute fork is inverted",
        s.waits.waits
    );
    assert!(
        s.cache_2d.hits + s.cache_2d.misses > 0,
        "the Cache2D counter recorded no lookups at all over {BURST_COLUMNS} columns, \
         so its zero above says nothing"
    );
    assert!(
        s.waits.computes > 0,
        "the store recorded no computes at all over {BURST_COLUMNS} columns, so its \
         zero waits above says nothing"
    );

    let concurrent = m.production();
    assert!(
        concurrent.waits.waits > 0,
        "window={} recorded zero OnceLock parks; the serial zero is then a dead \
         instrument, not evidence",
        m.window
    );
}

/// **The convicting measurement.** Instructions retired for the same 289 columns
/// must not grow with the window: every extra instruction under concurrency is work
/// the serial arm did not have to do, i.e. a cache that lost a race and recomputed.
///
/// The bound is not a tolerance chosen to pass. Serial is the floor by construction
/// (nothing can contend), and the two named recompute sources are the `Cache2D`
/// try-lock and a racing store miss. `Cache2D` sits under the spline leaves at
/// ~10^4–10^5 lookups per chunk against a column's ~4.9×10^8 instructions, so if a
/// meaningful fraction of those lookups became recomputes the ratio would land far
/// above 1.10; the store's own `pre_ore_computed` gate in
/// `join_scheduler_counters.rs` already pins the other source at exactly once.
#[test]
#[ignore = "289 columns of real embedded-data generation, six times; minutes in release"]
fn concurrency_does_not_buy_redundant_work() {
    let m = measure();
    let serial = m.serial.insns_per_column();

    for arm in &m.sweep {
        let ratio = arm.insns_per_column() / serial;
        assert!(
            ratio < 1.10,
            "window={:?} retired {:.3}x the serial arm's instructions per column \
             ({:.0} vs {:.0}); that is redundant recomputation, and c2d reported \
             {} contended lookups of {}",
            arm.window,
            ratio,
            arm.insns_per_column(),
            serial,
            arm.cache_2d.contended,
            arm.cache_2d.hits + arm.cache_2d.misses + arm.cache_2d.contended,
        );
        assert!(
            arm.in_order,
            "window={:?} emitted out of the order it was handed",
            arm.window
        );
        assert_eq!(
            arm.columns, BURST_COLUMNS,
            "window={:?} emitted {} columns of {BURST_COLUMNS}",
            arm.window, arm.columns
        );
    }

    // The store did the same work in every arm, which is what makes the
    // instruction comparison a comparison of *scheduling* rather than of scenes.
    assert!(
        m.serial.store_len >= BURST_PRE_ORE_CLOSURE,
        "the serial arm's store holds {} entries, under the {BURST_PRE_ORE_CLOSURE} \
         pre-ore closure this burst must reach — the scene is not the one derived",
        m.serial.store_len
    );
}
