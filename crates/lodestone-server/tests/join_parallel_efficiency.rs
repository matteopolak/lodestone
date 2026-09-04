//! Where the join burst's parallelism actually goes: **instructions retired** for
//! the same 289 columns, serial against a window sweep through the production
//! scheduler, with cycles and IPC alongside so a *blocking* loss and a *redundant
//! work* loss cannot be mistaken for each other.
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
//! interner's `RwLock`, and the store's per-entry `OnceLock` waits. This binary was
//! written to decide between them and **exonerated the first two** (§12.132): the
//! window itself was the defect, and it is the only one of the four instruments here
//! that had never been swept.
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
//! instructions is a **recompute** story, and the store's own once-only counters
//! (`join_scheduler_counters.rs`) then say which cache did it.
//!
//! Wall clock is kept because instructions are blind to locality (§12.120 measured
//! 490k instructions against 7× the time), and `ri_cycles` is kept because it is the
//! *third* reading that separates the two ways a flat instruction count can still be
//! slow: parked threads burn no cycles, stalled ones burn plenty.
//!
//! # How the two remaining losses are told apart
//!
//! Once redundant work is ruled out, inflated cycles for flat instructions has two
//! possible causes and they call for opposite fixes:
//!
//! | cause | fix | signature |
//! |---|---|---|
//! | shared mutable state on the generator being fought over | shard or privatise it | inflation at **every** window, including a tiny one |
//! | `workers` working sets exceeding the last-level cache | fewer workers | a threshold: nothing, then super-linear |
//!
//! So the sweep spans a small window as well as a wide one, and that shape is the
//! discriminator — `a_small_window_shows_no_lock_on_the_shared_generator`. A lock
//! contended by 20 workers is contended by 4.
//!
//! # The counter, and its calibration
//!
//! `lodestone_worldgen::overworld::store::wait_stats` — how many `StageSlot`
//! lookups were **parked** on another thread's `get_or_init`, and for how long in
//! total. Exactly **0** single-threaded, which is the calibration: a `OnceLock`
//! cannot park a thread with no other thread inside it.
//!
//! A second counter lived here through phase 1 and is deliberately gone: it counted
//! `Cache2DSlot`'s try-lock hits / misses / contention, measured a **0.12% hit
//! rate** and up to 9% contention, and the memo it measured was deleted as a result
//! (§12.132). Its replacement is the `IPC` / `Gcyc/col` columns below, which is a
//! stronger instrument for the same question: the loss was never *work*, so no
//! event count could have sized it.
//!
//! `wait_stats` is process-global, so — as in `join_scheduler_counters.rs` —
//! **nothing in this binary may generate terrain except [`measure`]**, which runs
//! every arm sequentially inside one `OnceLock`.
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
//! `gen-counters`: those hooks inflate a burst ~3×, so every cycle and IPC figure
//! here would describe a different system. Everything read here is live in a clean
//! release build by construction.

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
use lodestone_worldgen::overworld::store::{self, WaitStats};

/// 17×17 at `view_radius = 8` — the burst `4307b59` named, and the scene §12.112
/// and `join_scheduler_counters.rs` both use.
const BURST_RADIUS: i32 = 8;
const BURST_COLUMNS: usize = 289;

/// The window sweep, which must **bracket** `generation_window()` on both sides or
/// `the_production_window_sits_at_the_measured_optimum` degenerates into asserting
/// that an endpoint is an optimum.
///
/// `1` is the serial-through-the-scheduler arm, so the scheduler's own overhead is
/// held constant against every other arm rather than folded into the serial baseline.
/// `20` is the old `2 × available_parallelism` value, kept so the right-hand wall of
/// the U stays visible in the printed curve rather than becoming folklore.
const WINDOWS: &[usize] = &[1, SMALL_WINDOW, 6, 7, 8, 9, 10, 12, 20];

/// The window `a_small_window_shows_no_lock_on_the_shared_generator` reads. Small
/// enough to be below any plausible `available_parallelism()`, and above 1 so there
/// really are concurrent workers.
const SMALL_WINDOW: usize = 4;

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

// Darwin-only from here to `rusage_now`. `proc_pid_rusage` lives in `libSystem`
// and has no equivalent symbol on Linux or Windows, so an ungated `extern "C"`
// declaration of it links fine on macOS and fails the *link* — not the compile —
// everywhere else. `cargo check` never links, which is why this was invisible to
// every `check` job and only surfaced as
// `rust-lld: error: undefined symbol: proc_pid_rusage` in `cargo test`.

/// `RUSAGE_INFO_V4` from `<sys/resource.h>`.
#[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
const RUSAGE_INFO_V4_SIZE: usize = 16 + 36 * 8;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
}

/// Instructions retired, cycles and physical footprint for this **process** —
/// every thread, which is exactly the point here: a burst's total work is the sum
/// over the pool.
///
/// All three come out of the one syscall because they answer one question
/// together. Instructions alone cannot distinguish the two remaining losses once
/// redundant work is ruled out:
///
/// | reading | says |
/// |---|---|
/// | instructions flat, cycles flat, wall up | workers **parked** — a lock story |
/// | instructions flat, cycles **up** | IPC collapse — memory, coherence or E-core placement |
/// | footprint near RAM | the machine is swapping and every ratio above is about the machine |
///
/// The third row is not paranoia: this burst's store holds 1,369 live entries and
/// the host has 16 GB shared with sibling agents.
#[derive(Debug, Clone, Copy)]
struct Rusage {
    instructions: u64,
    cycles: u64,
    footprint: u64,
}

/// Non-Darwin arm. Panics rather than returning zeroed counters: all three
/// readings feed ratios, and a silent zero would make every ratio degenerate
/// while still reporting a number — the vacuous green this repo's evidence rules
/// exist to forbid. Every test that reaches this is `#[ignore]`d, so nothing on
/// Linux or Windows calls it; running one explicitly with `--ignored` is what
/// should say so, loudly.
#[cfg(not(target_os = "macos"))]
fn rusage_now() -> Rusage {
    unimplemented!(
        "instructions, cycles and physical footprint are read through \
         proc_pid_rusage(RUSAGE_INFO_V4), which exists only on Darwin; this efficiency \
         harness is a macOS-only measurement and has no counters to report on this target"
    )
}

#[cfg(target_os = "macos")]
fn rusage_now() -> Rusage {
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
    Rusage {
        instructions: info.ri_instructions,
        cycles: info.ri_cycles,
        footprint: info.ri_phys_footprint,
    }
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
    cycles: u64,
    /// Peak physical footprint observed at the end of the arm, in bytes.
    footprint: u64,
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
    /// Instructions per cycle. The one number that separates "parked" from
    /// "running slowly": parking removes both, a stall removes only instructions.
    fn ipc(&self) -> f64 {
        self.instructions as f64 / self.cycles.max(1) as f64
    }
    /// Cycles the whole pool burned per column — the quantity a scaling story has
    /// to keep flat, and the one that rises when work moves onto an E-core or
    /// starts waiting on memory.
    fn cycles_per_column(&self) -> f64 {
        self.cycles as f64 / self.columns as f64
    }
    /// Fraction of the pool's capacity (`window x wall`) spent parked on a
    /// `StageSlot`'s `OnceLock`.
    fn parked_fraction(&self) -> f64 {
        let workers = self.window.unwrap_or(1) as f64;
        (self.waits.wait_nanos as f64 / 1e9) / (self.wall.as_secs_f64() * workers)
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

    store::reset_wait_stats();
    let before = rusage_now();
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
            while let Some((pos, payload)) = pipeline
                .next()
                .await
                .expect("a source without an encoder cannot fail")
            {
                // This arm builds the pipeline with no `ChunkEncoder`, so the
                // payload is always the column — `expect` rather than a
                // `if let`, because silently skipping the `black_box` would let
                // the optimiser delete the generation this arm is timing.
                let column = payload
                    .column()
                    .expect("a pipeline with no encoder must yield the column itself");
                std::hint::black_box(column.solid_count());
                emitted.push(pos);
            }
            let n = emitted.len();
            (n, emitted == coords)
        }),
    };

    let wall = started.elapsed();
    let after = rusage_now();

    Arm {
        window,
        wall,
        instructions: after.instructions.saturating_sub(before.instructions),
        cycles: after.cycles.saturating_sub(before.cycles),
        footprint: after.footprint,
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
            "  {:<10} {:>9} {:>9} {:>8} {:>8} {:>8} {:>8} {:>6} {:>8} {:>7}",
            "arm",
            "wall(s)",
            "us/col",
            "speedup",
            "Ginsn",
            "I/I_ser",
            "Gcyc/col",
            "IPC",
            "C/C_ser",
            "park%",
        );
        for arm in std::iter::once(s).chain(self.sweep.iter()) {
            eprintln!(
                "  {:<10} {:>9.3} {:>9.0} {:>8.2} {:>8.1} {:>8.3} {:>8.3} {:>6.2} {:>8.3} \
                 {:>7.1}",
                arm.window
                    .map_or_else(|| "serial".to_string(), |w| format!("window={w}")),
                arm.wall.as_secs_f64(),
                arm.us_per_column(),
                s.wall.as_secs_f64() / arm.wall.as_secs_f64(),
                arm.instructions as f64 / 1e9,
                arm.instructions as f64 / s.instructions as f64,
                arm.cycles_per_column() / 1e9,
                arm.ipc(),
                arm.cycles as f64 / s.cycles.max(1) as f64,
                100.0 * arm.parked_fraction(),
            );
        }
        eprintln!(
            "\n  peak phys footprint: serial {:.2} GB, production window {:.2} GB",
            s.footprint as f64 / 1e9,
            self.production().footprint as f64 / 1e9,
        );
        eprintln!(
            "\n  serial:  {} store computes in {:.2} s of compute time",
            s.waits.computes,
            s.waits.compute_nanos as f64 / 1e9,
        );
        for arm in &self.sweep {
            eprintln!(
                "  window={:<3} store_len={} evictions={} in_order={} \
                 | {} waits totalling {:.2} s, {} computes totalling {:.2} s",
                arm.window.unwrap_or(0),
                arm.store_len,
                arm.evictions,
                arm.in_order,
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

/// **The calibration for the wait counter.** A single-threaded arm cannot park on a
/// `OnceLock`, so `waits` must read exactly zero — and it must be *non*-zero on the
/// concurrent arm, or the zero is an uncalled hook rather than an uncontended one.
///
/// Both halves are the licence to read the number at all: without the second, a dead
/// instrument and a contention-free system are the same observation.
#[test]
#[ignore = "289 columns of real embedded-data generation, seven times; minutes in release"]
fn the_wait_counter_reads_zero_serially_and_fires_concurrently() {
    let m = measure();
    let s = &m.serial;

    assert_eq!(
        s.waits.waits, 0,
        "a single-threaded sweep parked on {} OnceLocks, which is structurally \
         impossible — the wait/compute fork is inverted",
        s.waits.waits
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

/// **Redundant work.** Instructions retired for the same 289 columns must not grow
/// with the window: every extra instruction under concurrency is work the serial arm
/// did not have to do, i.e. a memo that lost a race and recomputed.
///
/// Serial is the floor by construction. The bound is 1.10 because the two recompute
/// sources are both individually pinned elsewhere — the store's `pre_ore_computed`
/// gate in `join_scheduler_counters.rs` holds stage computations at exactly once, and
/// the `Cache2D` try-lock that used to recompute on contention no longer exists
/// (§12.132) — so anything above a few percent here is a *third* source and wants
/// finding, not accommodating. Measured before the memo was deleted: 1.019 at the
/// widest window, with a measured 9% try-lock contention rate behind it.
#[test]
#[ignore = "289 columns of real embedded-data generation, seven times; minutes in release"]
fn concurrency_does_not_buy_redundant_work() {
    let m = measure();
    let serial = m.serial.insns_per_column();

    for arm in &m.sweep {
        let ratio = arm.insns_per_column() / serial;
        assert!(
            ratio < 1.10,
            "window={:?} retired {ratio:.3}x the serial arm's instructions per column \
             ({:.0} vs {serial:.0}); that is redundant recomputation",
            arm.window,
            arm.insns_per_column(),
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

/// **The gate the unit turns on: production's window sits at the sweep's optimum.**
///
/// Not a threshold on a duration — a comparison of arms measured in the same process
/// minutes apart, which is what makes it survive machine load. `generation_window()`
/// must be within 20% of the fastest window in [`WINDOWS`], and the sweep brackets it
/// on both sides so "the optimum" is a measured floor rather than an endpoint.
///
/// **Why 20% and not 10%**, since the number is the whole gate: the floor is broad and
/// its exact position moves with machine load. Four runs on the reference machine put
/// `window=8 / window=10` at 0.855, 0.855, 0.916 and 1.079 — so 8 is marginally the
/// better of the two and a 10% bound would have failed half of those runs on a
/// correctly-configured tree. 20% sits below the whole observed spread and still
/// leaves 2.3× of clearance against the failure this exists for: `2 × P` measured
/// **0.58** of the floor in the same run that measured 0.916. Tightening it wants a
/// portable way to ask for the *performance*-core count, which `available_parallelism`
/// is not — it counts this machine's 4 efficiency cores alongside its 6 P-cores, which
/// is most of why the floor sits below it.
///
/// Two failures it is built to catch, and neither is catchable by a fixed number:
///
/// * `2 × available_parallelism`, the value until §12.132, which measured **1.49×**
///   against window 8's **2.60×** — a third of the burst's throughput given away by
///   an unmeasured factor of 2.
/// * a machine, or a workload, whose floor is somewhere else. The bound is a *ratio
///   against the same run's best arm*, so a 4-core laptop and a 64-core server both
///   get a meaningful verdict without this file knowing anything about either.
#[test]
#[ignore = "289 columns of real embedded-data generation, ten times; minutes in release"]
fn the_production_window_sits_at_the_measured_optimum() {
    let m = measure();
    let best = m
        .sweep
        .iter()
        .min_by(|a, b| a.wall.cmp(&b.wall))
        .expect("the sweep is not empty");
    let arm = m.production();
    let ratio = best.wall.as_secs_f64() / arm.wall.as_secs_f64();
    assert!(
        ratio > 0.80,
        "generation_window() = {} took {:.3} s where window={:?} took {:.3} s \
         ({ratio:.2} of it). The window is past the sweep's floor, which costs \
         throughput for nothing — instructions across the sweep vary by {:.1}%, so \
         this is scheduling and not work. Full curve:\n{}",
        m.window,
        arm.wall.as_secs_f64(),
        best.window,
        best.wall.as_secs_f64(),
        100.0
            * (m.sweep
                .iter()
                .map(|a| a.insns_per_column())
                .fold(f64::MIN, f64::max)
                / m.sweep
                    .iter()
                    .map(|a| a.insns_per_column())
                    .fold(f64::MAX, f64::min)
                - 1.0),
        m.sweep
            .iter()
            .map(|a| format!(
                "    window={:<3} {:.3} s  {:.2}x  IPC {:.2}",
                a.window.unwrap_or(0),
                a.wall.as_secs_f64(),
                m.serial.wall.as_secs_f64() / a.wall.as_secs_f64(),
                a.ipc(),
            ))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// **No lock on the shared generator's hot path**, and the one gate here that can see
/// one.
///
/// Instructions retired cannot see a lock nobody recomputes behind, and cycles at
/// *production's* window cannot separate a contended lock from `workers` working sets
/// exceeding the last-level cache. A **small** window can, and that is the whole
/// trick: a lock contended by 20 workers is contended by 4, so it inflates cycles at
/// every window, whereas cache capacity is a threshold effect that costs nothing until
/// the working sets stop fitting. So the assertion is at `SMALL_WINDOW`, a window well
/// below any plausible core count.
///
/// The two hypotheses, computed from outside the run and 5× apart:
///
/// | hypothesis | cycles/column at window 4 vs serial |
/// |---|---|
/// | a lock on a path taken 10^4–10^5 times per column | ≫ 1.3 — §12.102's scar was 5,000 concurrent attempts on one `Arc<Mutex>` |
/// | cache capacity, which is what §12.132 measured | **1.01–1.15** over five runs |
///
/// The bound is 1.30 because the low hypothesis is *load-sensitive*: an externally
/// descheduled thread loses its cache and its cycles rise, and five runs with a
/// sibling agent compiling put this at 1.012, 1.035, 1.093, 1.107 and 1.153. 1.30 sits
/// above that whole spread and still 2× below the widest window's own reading in every
/// one of those runs, which is the control below.
///
/// **Its control is in the same table**, which is why this is not an assertion of an
/// absence with no detector: the identical measurement at a window of 20 reads
/// **3.8–4.4×**, so cycles-per-column demonstrably moves when something is wrong. An
/// earlier version of this gate compared a shared generator against a private one per
/// column and had to be withdrawn — building a generator per column dominates that
/// arm's instruction mix with JSON parsing and noise init, so its single-threaded IPC
/// (3.4–3.8) is not the workload's (5.3) and the ratio was measuring construction.
/// `CLAUDE.md`'s "ask what else already paints here", one level up.
#[test]
#[ignore = "289 columns of real embedded-data generation, ten times; minutes in release"]
fn a_small_window_shows_no_lock_on_the_shared_generator() {
    let m = measure();
    let s = &m.serial;
    let small = m
        .sweep
        .iter()
        .find(|a| a.window == Some(SMALL_WINDOW))
        .unwrap_or_else(|| panic!("the sweep {WINDOWS:?} must contain {SMALL_WINDOW}"));
    let widest = m
        .sweep
        .iter()
        .max_by_key(|a| a.window.unwrap_or(0))
        .expect("the sweep is not empty");

    let inflation = small.cycles_per_column() / s.cycles_per_column();
    assert!(
        inflation < 1.30,
        "at a window of only {SMALL_WINDOW} the pool already burned {inflation:.3}x the \
         serial arm's cycles per column ({:.3} vs {:.3} Gcyc) for {:.3}x its \
         instructions. Cache capacity measured 1.01-1.15x here across five runs; \
         this is a lock.",
        small.cycles_per_column() / 1e9,
        s.cycles_per_column() / 1e9,
        small.insns_per_column() / s.insns_per_column(),
    );

    // The control. Without it, "1.03x at window 4" is equally consistent with
    // cycles-per-column being a constant this test cannot move.
    let wide = widest.cycles_per_column() / s.cycles_per_column();
    assert!(
        wide > 2.0,
        "cycles per column only reached {wide:.2}x serial at the sweep's widest window \
         ({:?}), so the {inflation:.3}x above is not evidence of anything — this \
         instrument is not responding to concurrency at all",
        widest.window,
    );
    assert!(
        s.ipc() > 3.0,
        "the serial arm itself ran at IPC {:.2}, so both ratios above are comparisons \
         of two stalled arms",
        s.ipc()
    );
}
