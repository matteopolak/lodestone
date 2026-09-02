//! Chunk-generation benchmarks for the real, JVM-verified overworld pipeline
//! (this crate's column- and per-stage-cost benchmark work). Builds the *real*
//! [`OverworldGenerator`] the same way `tests/overworld_gen.rs` and
//! `tests/*_parity.rs` do — an `FsResolver` reading the checked-in
//! `tests/support/worldgen_data` JSON tree — rather than a synthetic stand-in,
//! per the epic's evidence standard: chunk generation is "stable enough to
//! benchmark meaningfully" specifically because it is the one subsystem
//! verified bit-exact against JVM oracles (`HANDOFF.md` §4).
//!
//! # Two resolvers, and which benches may use which
//!
//! **This changed in the worldgen-rewrite Unit 1 pass.** This file's header used
//! to say "no `lodestone-server` dependency is needed to benchmark
//! `lodestone-worldgen` itself", and for shape-only throughput that is still
//! true. It is **not** true for anything claiming to measure the composed
//! pipeline:
//!
//! | resolver | source | may be used by |
//! |---|---|---|
//! | [`make_shape_only_generator`] | in-crate fixture tree, 2 of 9 methods | raw noise-router throughput only |
//! | [`make_full_generator`] | in-crate fixture tree, all 9 methods | single-biome composed benches; 2 of 10 stages inert |
//! | [`make_embedded_generator`] | **`lodestone-server`'s embedded production data** | [`C_ss`/`C_cold`](bench_steady_state_and_cold) and the [calibration](bench_counter_calibration) |
//!
//! The fixture tree is single-biome plains and carries no `block_freeze_facts`
//! document, so against it the biome nearest-neighbour search never runs and
//! `freeze_top_layer` early-returns — **two of the ten stages are structurally
//! absent**, and the percentage table stays perfectly plausible while describing
//! a pipeline with stages missing. That is `CLAUDE.md`'s "world" species of
//! vacuous test, and it is this file's own documented history (see
//! [`make_shape_only_generator`]). `docs/plans/worldgen-rewrite.md` therefore
//! pins C_ss/C_cold to the embedded data specifically.
//!
//! **The defence is a counter, not a comment.** Every bench below that claims to
//! measure a stage asserts, via [`lodestone_worldgen::counters`], that the stage
//! actually *ran* — `stage_entered[top_layer] == 0` is precisely how a bench
//! discovers it has been silently pointed at the fixture tree. A prose warning
//! is what this file already had, and it did not help.
//!
//! Run with: `cargo bench -p lodestone-worldgen --bench generation`. The counter
//! benches need the feature:
//! `cargo bench -p lodestone-worldgen --features gen-counters --bench generation`.

// The counting allocator below needs `unsafe impl GlobalAlloc`, and the
// workspace sets `unsafe_code = "deny"` (root `Cargo.toml`'s
// `[workspace.lints.rust]`). This is the second opt-out in the workspace, after
// `lodestone-fuzz/tests/length_prefix_allocation.rs`, and it is scoped as
// narrowly as the lint allows: `#![allow]` is a crate-root attribute, and cargo
// compiles each `[[bench]]` target as its own separate binary crate, so it
// cannot leak into the library or any other target.
//
// The cost is paid for the same reason that file paid it: the rewrite plan's
// allocation budget ("0 heap allocations from the hot path, plus O(1) for the
// returned column's own buffers") is an *acceptance criterion* for Units 3, 4, 7
// and 10, and the ~885k-allocations-per-column figure it ratchets down from is
// the single most damning number in the diagnosis. Deriving that from source
// reading is exactly the kind of static claim `CLAUDE.md`'s record is built out
// of being wrong about. `alloc`/`dealloc` here are pass-throughs to `System`
// plus a counter — no allocation logic of their own to get wrong.
#![allow(unsafe_code)]

mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_worldgen::counters::{self, Snapshot, Stage};
use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Counting allocator — **this bench binary only**
// ---------------------------------------------------------------------------

thread_local! {
    /// Allocations made **on the calling thread** while [`ALLOC_COUNTING`] is on.
    ///
    /// ## Per-thread, not a global atomic — and that is not a style choice
    ///
    /// `lodestone-fuzz/tests/length_prefix_allocation.rs` records
    /// what happens with the obvious design: a bare process-wide
    /// `AtomicU64` let one measurement absorb another thread's allocations, and
    /// the follow-up fix — the same atomic plus a mutex held across each
    /// measurement — *flaked*, because a lock only excludes code that takes it
    /// and unrelated parallel work allocated straight into the shared counter.
    /// A thread-local needs no cooperation from anything: other threads'
    /// allocations land in their own cell and are structurally invisible here.
    ///
    /// That matters even though C_ss is single-threaded by definition: criterion
    /// is free to allocate from its own threads, and `Vec::sort` and JSONL
    /// recording run in the same process.
    ///
    /// `const`-initialised `Cell` deliberately, copying that file's reasoning
    /// verbatim: it compiles to a plain per-thread slot with no lazy
    /// initialisation and no destructor, so reading it from inside `alloc`
    /// cannot itself allocate and recurse.
    static ALLOC_COUNT: Cell<u64> = const { Cell::new(0) };
    /// Gate on [`ALLOC_COUNT`]. Off by default so criterion's own sampling,
    /// generator construction and JSON parsing are not attributed to generation.
    static ALLOC_COUNTING: Cell<bool> = const { Cell::new(false) };
    /// [`ALLOC_COUNT`] split by the stage that was executing, so a residual
    /// allocation count can be *attributed* instead of guessed at.
    ///
    /// Added for Unit 3, which took the steady-state figure from 905,459 to
    /// 20,684 and then needed to answer "and where is the rest?" — a question
    /// the single total structurally cannot answer. Reasoning about it from the
    /// source instead would be the kind of hand-derived number
    /// `CLAUDE.md` records as having been wrong four times in four ways.
    ///
    /// An array of `Cell`s rather than a `Cell<[u64; N]>`: the latter would
    /// copy the whole array in and out on **every allocation in the process**.
    ///
    /// **Only meaningful with `--features gen-counters`**, because
    /// [`counters::current_stage`] is compiled down to a constant
    /// [`Stage::Other`] without it — which is exactly why
    /// `steady_state_heap_allocs_per_column` must read the same with the feature
    /// on and off (the bench asserts this; see
    /// [`bench_steady_state_and_cold`]). If it did not, the attribution would
    /// describe a different program from the one the ratchet measures.
    static ALLOC_BY_STAGE: [Cell<u64>; counters::STAGE_COUNT] =
        const { [const { Cell::new(0) }; counters::STAGE_COUNT] };
}

/// Counts allocations, forwarding everything to [`System`].
///
/// # Why this lives in the bench binary and nowhere else
///
/// `docs/plans/worldgen-rewrite.md`'s allocation budget is an *acceptance
/// criterion* (0 heap allocations from the steady-state hot path, with an
/// explicit O(1) allowance for the returned column's own buffers), so it needs a
/// real count, not an estimate. But a `#[global_allocator]` is process-wide and
/// exactly one may exist — `lodestone-allocbench` carries a deliberate
/// `compile_error!` for precisely this reason. A bench binary is its own crate
/// and its own process, so installing one here affects nothing else in the
/// workspace and adds no allocator to any shipped artifact.
///
/// # Gotchas
///
/// * **It counts only the calling thread's allocations** (see [`ALLOC_COUNT`]).
///   C_ss and C_cold are single-thread by definition, so that is what is wanted;
///   but a *parallel* sweep wrapped in [`measure_allocs`] would report only the
///   coordinating thread's share, which is not the per-column figure. Do not use
///   this to gate `generate_columns_parallel`.
/// * **It counts allocations, not bytes, and not peak.** `bench_region_rss`
///   remains the footprint instrument; this one answers "did the hot path
///   allocate at all", which is the question the ratchet is written in.
/// * `realloc`/`alloc_zeroed` are left to `GlobalAlloc`'s defaults, which route
///   through `alloc`, so a `Vec` growth counts as one allocation. That is the
///   intended reading: it is a heap acquisition.
struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with`, not `with`: an allocation during thread teardown (after
        // TLS destruction) must be recorded nowhere rather than panic inside the
        // global allocator. No measurement can be in flight then anyway.
        let _ = ALLOC_COUNTING.try_with(|on| {
            if on.get() {
                let _ = ALLOC_COUNT.try_with(|n| n.set(n.get().wrapping_add(1)));
                // `current_stage` reads a thread-local `Cell` and cannot
                // allocate, so this cannot recurse into `alloc`.
                let stage = counters::current_stage() as usize;
                let _ = ALLOC_BY_STAGE.try_with(|bins| {
                    let bin = &bins[stage.min(counters::STAGE_COUNT - 1)];
                    bin.set(bin.get().wrapping_add(1));
                });
            }
        });
        // SAFETY: `layout` is forwarded unchanged to the system allocator, which
        // upholds `GlobalAlloc`'s contract for it.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` came from `Self::alloc`, i.e. from `System`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL_ALLOC: CountingAllocator = CountingAllocator;

// ---------------------------------------------------------------------------
// Instructions retired — the standing before/after comparator
// ---------------------------------------------------------------------------
//
// **Why this exists beside the wall clock rather than instead of it.**
// `DESIGN.md` §12.112 states this bench's own reproducibility: wall clock here
// moves **10.8%** run to run on a machine with two or three other agents
// compiling, and §12.98 records a 20% swing on an identical binary. That is wider
// than most of the wins a later optimisation unit will claim, so a wall-clock
// acceptance criterion on this file is unusable — it was the reason 21 units of
// `docs/plans/worldgen-rewrite.md` landed against allocation counters and the
// headline figure went unmeasured for the whole drive.
//
// `proc_pid_rusage(getpid(), RUSAGE_INFO_V4, …)`'s `ri_instructions` reproduces to
// **0.1–0.6%** under the same load: thermal state, DVFS and P-vs-E-core placement
// change how *fast* instructions retire, never *which* instructions a
// deterministic program executes. `docs/plans/worldgen-cycle-accounting.md` is the
// characterisation; `crates/lodestone-shell/tests/client_chunk_cycles.rs` is the
// first customer and the pattern this follows, including reaching
// `ri_instructions` **by name** out of a field-by-field transcription rather than
// at a hand-computed offset.
//
// **Instructions are blind to locality, so both are kept.** §12 records a case
// measuring 490k instructions against ~7× more time; a cache or locality story
// needs the pair, and reporting the ratio (instructions per µs) is how a change
// that moved only locality announces itself.

// Darwin-only from here to `instructions_retired`. `proc_pid_rusage` lives in
// `libSystem` and has no equivalent symbol on Linux or Windows, so an ungated
// `extern "C"` declaration of it links fine on macOS and fails the *link* — not
// the compile — everywhere else. A bench target is built by `--all-targets` and
// linked by `cargo test`, so this is a real cross-platform break and not only a
// concern for `cargo bench`.

/// `RUSAGE_INFO_V4` from `<sys/resource.h>`.
#[cfg(target_os = "macos")]
const RUSAGE_INFO_V4: i32 = 4;

/// `struct rusage_info_v4` from macOS `<sys/resource.h>`, transcribed
/// **field-by-field in declaration order** so `ri_instructions` is reached by name.
/// Same transcription as `client_chunk_cycles.rs`; [`RUSAGE_INFO_V4_SIZE`] is the
/// check that a dropped or mis-typed line fails loudly instead of silently
/// shifting which field is read.
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

/// The size the transcription above must have if every field is present and
/// correctly typed: a 16-byte UUID followed by 36 `u64`s. Derived from the field
/// list, not measured.
#[cfg(target_os = "macos")]
const RUSAGE_INFO_V4_SIZE: usize = 16 + 36 * 8;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// `libproc`'s task-level resource accounting, in `libSystem` — no link flag
    /// and no privileges needed for the calling process.
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
}

/// Non-Darwin arm. Panics rather than returning a plausible zero: every figure
/// this bench reports is a before/after difference, and a counter stuck at 0
/// would print an instructions-per-µs ratio of zero as though it were a result.
/// `cargo bench` on a non-Darwin host should fail here and say why, not publish a
/// fabricated number.
#[cfg(not(target_os = "macos"))]
fn instructions_retired() -> u64 {
    unimplemented!(
        "instructions retired is read through proc_pid_rusage(RUSAGE_INFO_V4), which exists \
         only on Darwin; this bench's instruction-count arm cannot run on this target (its \
         wall-clock arm is unaffected)"
    )
}

/// Instructions retired by this **process** so far.
///
/// Process-wide, not per-thread: everything measured through it below is
/// single-threaded and criterion's own sampling is not running concurrently with
/// it, which is the same constraint `client_chunk_cycles.rs` documents.
#[cfg(target_os = "macos")]
fn instructions_retired() -> u64 {
    let mut info = RusageInfoV4::default();
    let rc = unsafe {
        proc_pid_rusage(
            i32::try_from(std::process::id()).expect("pid fits in i32"),
            RUSAGE_INFO_V4,
            (&raw mut info).cast::<core::ffi::c_void>(),
        )
    };
    assert_eq!(
        rc, 0,
        "proc_pid_rusage(RUSAGE_INFO_V4) failed with {rc}; the standing before/after \
         comparator has nothing to report without it"
    );
    info.ri_instructions
}

/// A fixed-iteration SplitMix64 chain: the instrument's own scaling control.
/// `#[inline(never)]` so the optimiser cannot fold two calls with different
/// `iters` into one codegen, which would break the ratio the control asserts.
#[inline(never)]
fn reference_kernel(iters: u64) -> u64 {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..iters {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x = x.wrapping_add(z ^ (z >> 31));
    }
    x
}

/// Fails unless `ri_instructions` is being read at all and is really an
/// instruction count.
///
/// Two checks, both with expected values from outside the field being read:
///
/// * **Struct size** — `16 + 36 × 8`, from the field list. Catches a dropped,
///   added or mis-typed line, which would shift the read to a neighbouring field
///   whose value would still look like a plausible large number.
/// * **4× scaling** — four times the loop bound must retire ~four times the
///   instructions. The expected value is a ratio of two constants chosen in this
///   file, so it is arithmetic, not an observation. A footprint, flags or
///   wakeup-count field reads ≈1.00 here; a zero (Rosetta, or a kernel that does
///   not populate the field) fails the non-zero check inside it.
///
/// The locality control that separates `ri_instructions` from `ri_cycles` — the
/// third arm — lives in `client_chunk_cycles.rs`, which is the same struct read
/// the same way. It is not duplicated here.
/// Non-Darwin arm. Both controls above are statements *about* Darwin's
/// `rusage_info_v4` and the counter it carries, so there is no instrument here to
/// validate. It panics rather than returning `()`: a control that quietly does
/// nothing is worse than no control, since its entire purpose is to stand between
/// this bench and a plausible-looking wrong number.
#[cfg(not(target_os = "macos"))]
fn assert_instruction_counter_is_real() {
    unimplemented!(
        "the struct-size and 4x-scaling controls validate a Darwin rusage_info_v4 \
         transcription and the instruction counter it feeds; neither exists on this target"
    )
}

#[cfg(target_os = "macos")]
fn assert_instruction_counter_is_real() {
    assert_eq!(
        std::mem::size_of::<RusageInfoV4>(),
        RUSAGE_INFO_V4_SIZE,
        "the rusage_info_v4 transcription is {} bytes, not the {RUSAGE_INFO_V4_SIZE} its \
         own field list implies; `ri_instructions` is therefore not the field being read",
        std::mem::size_of::<RusageInfoV4>()
    );

    // **Large, because the instrument's own cost sits inside both arms.** Each
    // `measure` call brackets the kernel with two `proc_pid_rusage` reads, and a
    // read costs ~80,000 instructions (it walks the task's threads) — far more than
    // the ~600 ns wall figure suggests. That fixed term inflates the smaller arm
    // proportionally more, so the ratio is `(4nk + C) / (nk + C)`, not 4. At
    // `BASE = 200_000` it measured **3.663** and failed this control on a correct
    // reading; at 5,000,000 the term is 0.4% of the smaller arm and the ratio lands
    // at 3.98. The band below is set to that residual, not widened to hide it.
    const BASE: u64 = 5_000_000;
    // `black_box` on the **argument** as well as the result, and it is load-bearing.
    // `reference_kernel` reads no memory, so LLVM infers `readnone` and is free to
    // common-subexpression two calls it can prove share an argument — `#[inline(never)]`
    // stops inlining, not CSE. Observed: the warm-up call and the first measured
    // call, both at `BASE`, collapsed into one and this control read
    // **1304.7×** instead of 4.00. It fired on a broken control rather than on
    // broken code, which is the direction a control is supposed to fail in.
    let measure = |iters: u64| -> u64 {
        let before = instructions_retired();
        black_box(reference_kernel(black_box(iters)));
        instructions_retired().saturating_sub(before)
    };
    // Warm: the first call pays for lazily-resolved symbols on the syscall path.
    // A different bound from either measured arm, so nothing can be folded into it.
    black_box(measure(BASE / 2 + 1));
    let one = measure(BASE);
    let four = measure(4 * BASE);
    assert!(
        one > 0 && four > 0,
        "the instruction counter did not move over {BASE} kernel iterations \
         ({one} then {four}); it reads zero on Rosetta and on kernels that do not \
         populate the field, and a zero delta would satisfy every ratio below"
    );
    let ratio = four as f64 / one as f64;
    assert!(
        (3.90..=4.10).contains(&ratio),
        "4x the loop bound retired {ratio:.3}x the instructions ({one} -> {four}); the \
         expected 4.00 is the ratio of two constants in this file, so a value near 1.00 \
         means a non-instruction field is being read"
    );
}

/// Runs `f` with allocation counting on for this thread, returning its value and
/// the count.
fn measure_allocs<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let (out, total, _by_stage) = measure_allocs_by_stage(f);
    (out, total)
}

/// [`measure_allocs`] plus the per-stage split (see [`ALLOC_BY_STAGE`]).
///
/// The per-stage figures are all-zero-but-`other` without
/// `--features gen-counters`; the total is exact either way.
fn measure_allocs_by_stage<T>(f: impl FnOnce() -> T) -> (T, u64, [u64; counters::STAGE_COUNT]) {
    ALLOC_COUNT.set(0);
    ALLOC_BY_STAGE.with(|bins| {
        for bin in bins {
            bin.set(0);
        }
    });
    ALLOC_COUNTING.set(true);
    let out = f();
    ALLOC_COUNTING.set(false);
    let mut by_stage = [0u64; counters::STAGE_COUNT];
    ALLOC_BY_STAGE.with(|bins| {
        for (slot, bin) in by_stage.iter_mut().zip(bins) {
            *slot = bin.get();
        }
    });
    (out, ALLOC_COUNT.get(), by_stage)
}

/// Same shape as `tests/overworld_gen.rs`'s `FsResolver` / `chunk_parity.rs`'s
/// resolver: reads density functions and noises straight off disk under
/// `tests/support/worldgen_data`, the fixture tree the parity suite already
/// keeps current against the JVM oracle.
struct FsResolver {
    root: std::path::PathBuf,
    /// When false, only `density_function` and `noise` are answered and every
    /// other [`Resolver`] method keeps its `Value::Null` default — see
    /// [`make_shape_only_generator`].
    full: bool,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }

    /// Like [`Self::read`] but `Value::Null` for a missing file, matching
    /// `tests/vegetation_parity.rs`'s `try_json`. The fixture tree does not
    /// carry every id the real registries do, and a missing one must degrade to
    /// "no data" rather than panicking mid-benchmark.
    fn try_read(&self, kind: &str, id: &str) -> Value {
        if !self.full {
            return Value::Null;
        }
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or(Value::Null),
            Err(_) => Value::Null,
        }
    }
}

impl Resolver for FsResolver {
    fn density_function(&self, id: &str) -> Value {
        self.read("density_function", id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        let v = self.read("noise", id);
        NoiseParams {
            first_octave: v["firstOctave"].as_i64().expect("firstOctave") as i32,
            amplitudes: v["amplitudes"]
                .as_array()
                .expect("amplitudes")
                .iter()
                .map(|a| a.as_f64().expect("amplitude"))
                .collect(),
        }
    }
    fn biome_document(&self, id: &str) -> Value {
        self.try_read("biome", id)
    }
    fn configured_carver(&self, id: &str) -> Value {
        self.try_read("configured_carver", id)
    }
    fn configured_feature(&self, id: &str) -> Value {
        self.try_read("configured_feature", id)
    }
    fn placed_feature(&self, id: &str) -> Value {
        self.try_read("placed_feature", id)
    }
    fn block_tag(&self, id: &str) -> Value {
        self.try_read("tags/block", id)
    }
}

fn generator_with(seed: i64, full: bool) -> OverworldGenerator {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone(), full };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    OverworldGenerator::new(seed, &settings, &resolver, "minecraft:plains", false)
}

/// The **shape-only** generator: density functions and noises resolved, and
/// nothing else.
///
/// # This was the whole file's generator, and it made two benches measure a
/// pipeline they claimed not to be measuring
///
/// `Resolver` has nine methods. This bench's `FsResolver` implemented the two
/// required ones and inherited `Value::Null` defaults for `biome_document`,
/// `configured_carver`, `configured_feature`, `placed_feature`,
/// `block_freeze_facts` and `block_tag` (`src/density/mod.rs:220-278`). Those
/// nulls resolve to empty carver lists, empty feature steps and an empty
/// replaceable-block tag, so **carvers, ore features, vegetal decoration and the
/// freeze stage were all inert** — every one of them an early return.
///
/// Measured, before the fix: over 49 chunks the carve stage totalled 185µs
/// (0.02%), ore 10µs, vegetation 9µs, top-layer 7µs. Meanwhile
/// `bench_ore_composition_sweep`'s doc comment asserted it "actually exercises
/// `OverworldGenerator::ore_stage`" and warned in terms that "a resolver with no
/// ore data at all would make `ore_stage` an early-return no-op and this bench
/// would measure nothing relevant" — while citing a `biome/plains.json` its own
/// resolver never opened. The file names the trap and then contains it, which is
/// `CLAUDE.md`'s "world" species of vacuous test exactly: the flaw is in the
/// input data, and nothing about reading the test reveals it.
///
/// This constructor is kept, because a shape-only generator is genuinely the
/// right subject for the raw noise-router throughput numbers (it samples fast
/// enough for criterion to take many samples). It is just no longer allowed to
/// masquerade as the composed pipeline: every bench using it says so, and
/// anything making a claim about carvers/ores/vegetation uses
/// [`make_full_generator`] instead.
fn make_shape_only_generator(seed: i64) -> OverworldGenerator {
    generator_with(seed, false)
}

/// The **composed** generator: every fixture the tree carries is resolved, so
/// carvers, the 3×3 ore driver, vegetal decoration and the freeze stage all
/// really run. Much slower per column, which is why benches using it work over
/// small patches.
fn make_full_generator(seed: i64) -> OverworldGenerator {
    generator_with(seed, true)
}

/// The **embedded production** generator: `lodestone-server`'s bundled 26.2
/// worldgen data, i.e. the exact generator the integrated server serves to a
/// real client.
///
/// This is what `docs/plans/worldgen-rewrite.md`'s C_ss/C_cold definitions
/// require, and the difference from [`make_full_generator`] is not cosmetic — it
/// is the difference between eight live stages and ten:
///
/// * **real multi-noise biome variety** (`biome_parameters/overworld`, 7,594
///   rows), so `biome_stage`'s nearest-neighbour search actually runs. Against
///   the fixture tree `dynamic_biome` is `None` and the search count is zero.
/// * **`block_freeze_facts`**, which is built from `lodestone-data`'s jar dumps
///   rather than any JSON asset, so `freeze_top_layer` has predicates and the
///   `top_layer` stage stops early-returning.
///
/// Both are asserted by counter, not assumed — see
/// [`assert_all_ten_stages_ran`].
fn make_embedded_generator(seed: i64) -> OverworldGenerator {
    lodestone_server::overworld_generator(seed)
}

/// Seed 42, chunk (0,0): the exact fixture `overworld_gen.rs`'s
/// `composed_shape_matches_verified_density_field` anchors to a checked-in JVM
/// density dump, so this bench exercises the same terrain those correctness
/// tests already pinned down (an oceanic column: shape + fluid + surface all
/// exercised, not a vacuous stone slab).
const SEED: i64 = 42;

/// Headline throughput: real `column()` generation over a modest patch, timed
/// both by criterion (statistically robust, many samples) and — once, outside
/// criterion's loop — recorded to `bench-results/generation.jsonl` with scene
/// metadata so it is comparable across runs on this machine.
fn bench_column_throughput(c: &mut Criterion) {
    // SHAPE-ONLY: this is the noise-router throughput number, deliberately not
    // the composed pipeline's. See `make_shape_only_generator`.
    let generator = make_shape_only_generator(SEED);
    // A small patch so criterion's sampling stays fast; the region-level
    // generation benches below are the place for a full render-distance sweep.
    let coords: Vec<(i32, i32)> = (-2..=2).flat_map(|cz| (-2..=2).map(move |cx| (cx, cz))).collect();

    // Warm up (touch caches / branch predictor) before either measurement.
    for &(cx, cz) in coords.iter().take(4) {
        black_box(generator.column(cx, cz));
    }

    // One-shot diagnostic measurement, recorded with metadata — independent of
    // criterion's own iteration count/loop, matching `bench_worldgen.rs`'s
    // percentile shape.
    let scene = format!("seed={SEED} patch=5x5(25 chunks)");
    let n = coords.len();
    let mut per_chunk_us: Vec<f64> = Vec::with_capacity(n);
    for &(cx, cz) in &coords {
        let t = Instant::now();
        black_box(generator.column(cx, cz));
        per_chunk_us.push(t.elapsed().as_secs_f64() * 1e6);
    }
    per_chunk_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = per_chunk_us[n / 2];
    support::record(support::Record {
        bench: "generation",
        metric: "column_median_us",
        scene: &scene,
        value: median,
        unit: "us",
    });

    // Criterion's own headline number, for local `--baseline`/`--save-baseline`
    // before/after comparisons.
    let mut idx = 0usize;
    c.bench_function("worldgen/column_real_generator", |b| {
        b.iter(|| {
            let (cx, cz) = coords[idx % coords.len()];
            idx += 1;
            black_box(generator.column(black_box(cx), black_box(cz)))
        })
    });
}

/// **The per-stage cost split** — now with **one bucket per stage
/// the pipeline actually has**: aquifer, noise router (shape), biome, surface,
/// materialize, carve, ore, vegetation, top-layer, intern.
///
/// # What this pass changed, and why the previous split was not the answer
///
/// A four-bucket split already existed here and was recorded to
/// `bench-results/generation.jsonl`. Two of its four buckets did not measure
/// what they were named, so the persisted numbers were misattributed:
///
/// * `stage_fluid_heightmap_pct` was **the biome stage**. The heightmap is
///   computed inside the shape window.
/// * `stage_intern_pct` was **materialize + carve + ore + vegetation +
///   intern**. So carvers, ore features and vegetal decoration — the three
///   things the per-stage split explicitly says are "missing from that split entirely" — were
///   present in the total but invisible, filed under "interning".
///
/// Both are fixed by [`StageTimes`]'s new fields. **Do not compare a
/// `stage_intern_pct` from before this change with one from after**; the scene
/// string differs, which is what keeps `cargo xtask bench-compare` from pairing
/// them.
///
/// # The anti-drift control the per-stage split asks for
///
/// The per-stage split's method note is that the previously-deleted instrumented twin was a
/// *correctness* risk because it duplicated the verified pipeline. `column_timed`
/// is not a duplicate — it calls the same private stage functions in the same
/// order — but it does bypass the two memo caches `column()` goes through, which
/// is a real difference and exactly the sort of thing that drifts silently. So
/// this bench **asserts the two produce identical output**, block for block,
/// over a fresh generator per arm.
///
/// The fresh generator per arm is load-bearing rather than tidiness: the
/// generator holds a 512-entry memo cache, so running both arms on one generator
/// would have the second arm read the first's cached result and agree with
/// itself no matter what — the trap that neutered two determinism gates once
/// already (see `chunk.rs`'s determinism test).
fn bench_stage_split(c: &mut Criterion) {
    // --- Anti-drift control: column_timed must equal column, block for block.
    {
        let g_plain = make_full_generator(SEED);
        let g_timed = make_full_generator(SEED);
        let (cx, cz) = (1, -1);
        let plain = g_plain.column(cx, cz);
        let (timed, _) = g_timed.column_timed(cx, cz);
        assert_eq!(
            (plain.min_y(), plain.height(), plain.non_air_count()),
            (timed.min_y(), timed.height(), timed.non_air_count()),
            "column_timed's column shape differs from column()'s — the timed path has drifted \
             from the verified one, which is the risk the per-stage split's method note is about"
        );
        let mut first_mismatch = None;
        let mut mismatches = 0usize;
        for lz in 0..16 {
            for lx in 0..16 {
                for y in plain.min_y()..plain.min_y() + plain.height() {
                    if plain.block_state(lx, y, lz) != timed.block_state(lx, y, lz) {
                        mismatches += 1;
                        first_mismatch.get_or_insert((lx, y, lz));
                    }
                }
            }
        }
        assert_eq!(
            mismatches, 0,
            "column_timed disagrees with column() at {mismatches} of {} cells, first at {:?} \
             (chunk {cx},{cz}) — the timed path is no longer measuring the real pipeline",
            16 * 16 * plain.height(),
            first_mismatch
        );
        // Control on the control: the comparison above must be capable of
        // seeing a difference. A neighbouring chunk must NOT match, or the
        // block_state comparison is reading something constant.
        let other = g_plain.column(cx + 1, cz);
        let differs = (plain.min_y()..plain.min_y() + plain.height())
            .any(|y| plain.block_state(3, y, 5) != other.block_state(3, y, 5));
        assert!(
            differs,
            "the block-for-block comparison found no difference between two DIFFERENT chunks, so \
             it could not have detected drift between column and column_timed either"
        );
    }

    let generator = make_full_generator(SEED);
    // 3x3 rather than 7x7: the composed pipeline is ~1000x slower per column
    // than the shape-only one this bench used to run, so the patch shrinks to
    // keep the bench runnable. Nine columns is still enough for the ore
    // driver's 3x3 neighbourhood sharing to be exercised.
    let coords: Vec<(i32, i32)> = (-1..=1).flat_map(|cz| (-1..=1).map(move |cx| (cx, cz))).collect();

    for &(cx, cz) in coords.iter().take(4) {
        black_box(generator.column_timed(cx, cz));
    }

    // One accumulator per stage, in pipeline order.
    let mut stages: [(&str, Vec<f64>); 10] = [
        ("aquifer", Vec::new()),
        ("shape", Vec::new()),
        ("biome", Vec::new()),
        ("surface", Vec::new()),
        ("materialize", Vec::new()),
        ("carve", Vec::new()),
        ("ore", Vec::new()),
        ("vegetation", Vec::new()),
        ("top_layer", Vec::new()),
        ("intern", Vec::new()),
    ];
    for &(cx, cz) in &coords {
        let (col, t) = generator.column_timed(cx, cz);
        black_box(col.non_air_count());
        let us = |d: std::time::Duration| d.as_secs_f64() * 1e6;
        for (slot, value) in stages.iter_mut().zip([
            us(t.aquifer),
            us(t.shape),
            us(t.biome),
            us(t.surface),
            us(t.materialize),
            us(t.carve),
            us(t.ore),
            us(t.vegetation),
            us(t.top_layer),
            us(t.intern),
        ]) {
            slot.1.push(value);
        }
    }
    let sum = |v: &[f64]| v.iter().sum::<f64>();
    let total: f64 = stages.iter().map(|(_, v)| sum(v)).sum();
    assert!(total > 0.0, "every stage measured zero — nothing was timed");

    // The non-vacuity gate that the previous four-bucket split had no way to
    // express, and whose absence let this file measure an ore-free, carver-free
    // pipeline for as long as it existed. Each of these stages must have cost
    // *something*: a stage measuring ~0 here means the resolver stopped
    // supplying its data (a `Value::Null` default silently re-inherited, a
    // fixture file renamed) and the split has gone back to describing a
    // pipeline that is missing stages, while still looking like a clean
    // percentage table.
    //
    // The threshold is per-stage total over the whole patch, in microseconds —
    // a count-like floor, not a performance ceiling, so machine load cannot
    // move it. Carvers and ores were 185us and 10us across 49 chunks when
    // inert; a real run of either is orders of magnitude above that.
    let stage_total = |name: &str| -> f64 {
        stages.iter().find(|(n, _)| *n == name).map_or(0.0, |(_, v)| sum(v))
    };
    for name in ["aquifer", "shape", "surface", "materialize", "carve", "ore", "vegetation"] {
        let got = stage_total(name);
        assert!(
            got > 1_000.0,
            "stage {name:?} measured only {got:.1}us across {} composed columns, which is the \
             signature of an early return rather than a stage. Check that this bench's FsResolver \
             still answers the method that stage's data comes from (biome_document / \
             configured_carver / configured_feature / placed_feature / block_tag) — a \
             `Value::Null` default makes the stage a no-op and the percentage table below stays \
             perfectly plausible while describing a pipeline with stages missing.",
            coords.len()
        );
    }

    println!("worldgen stage split over {} chunks (seed={SEED}), release-or-debug per the recorded profile:", coords.len());
    for (name, v) in &stages {
        println!(
            "  {name:<12} {:>9.1}us total  {:>6.2}%",
            sum(v),
            100.0 * sum(v) / total
        );
    }
    println!(
        "  (HANDOFF.md \u{a7}4's original, deleted-instrumentation split was noise 40% / surface 51% / intern 7% at RD8/seed 3 \u{2014} different scene AND different bucket boundaries, so this is a fresh measurement, not a reproduction of that figure)"
    );
    println!(
        "  two stages are expected to read ~0 here and are excluded from the non-vacuity gate \
         above, for stated reasons rather than convenience: `biome` because this generator is \
         constructed with a single fixed biome (\"minecraft:plains\"), so there is no multi-noise \
         search to do; and `top_layer` because the fixture tree carries no \
         `block_freeze_facts` document, so `freeze_top_layer` has no predicates and early-returns. \
         `lodestone-server`'s own top_layer share assertion covers that stage against the \
         EMBEDDED data, which does carry those facts."
    );

    // Scene string names the ten-bucket split explicitly, so a reader (and
    // `bench-compare`) can never pair these with the old four-bucket numbers.
    //
    // **The patch size is derived from `coords`, not restated.** It used to be
    // the literal `patch=7x7(49 chunks)` while the sweep above was 3×3 — stale
    // metadata found while planning the worldgen rewrite
    // (`docs/plans/worldgen-rewrite.md`, "Stale claims found while planning").
    // A scene string is what `cargo xtask bench-compare` pairs history on, so a
    // wrong one silently compares 9-chunk runs against 49-chunk runs and reports
    // the difference as a regression. Deriving it from the same expression the
    // sweep uses is the only form that cannot drift again — a hand-written
    // constant next to a loop is a second source of truth.
    let side = (coords.len() as f64).sqrt().round() as usize;
    let scene = format!(
        "seed={SEED} patch={side}x{side}({} chunks) split=10stage resolver=fixture_tree",
        coords.len()
    );
    for (name, v) in &stages {
        support::record(support::Record {
            bench: "generation",
            metric: &format!("stage_{name}_pct"),
            scene: &scene,
            value: 100.0 * sum(v) / total,
            unit: "%",
        });
        support::record(support::Record {
            bench: "generation",
            metric: &format!("stage_{name}_us_per_chunk"),
            scene: &scene,
            value: sum(v) / coords.len() as f64,
            unit: "us",
        });
    }

    // Also give criterion a crack at the whole `column_timed` call so its
    // overhead vs. plain `column()` is visible (should be ~identical; a
    // divergence would mean the timing wrapper itself is not free).
    let mut idx = 0usize;
    c.bench_function("worldgen/column_timed_overhead", |b| {
        b.iter(|| {
            let (cx, cz) = coords[idx % coords.len()];
            idx += 1;
            black_box(generator.column_timed(black_box(cx), black_box(cz)))
        })
    });
}

/// Sanity: does generation scale roughly linearly with chunk count? A ratio
/// against a paired same-run measurement, per the epic's method rules — not a
/// wall-clock ceiling. Superlinear growth here would mean a per-call cost is
/// leaking global state (e.g. an unbounded cache) across chunks.
fn bench_linearity_check(c: &mut Criterion) {
    // SHAPE-ONLY: linearity of the noise router in column count. The composed
    // pipeline's linearity is a different question (its memo caches make
    // adjacent columns cheaper) and is not what this ratio measures.
    let generator = make_shape_only_generator(SEED);
    let small: Vec<(i32, i32)> = (0..3).map(|i| (i, 0)).collect(); // 3 chunks
    let large: Vec<(i32, i32)> = (0..12).map(|i| (i, 0)).collect(); // 12 chunks (4x)

    for &(cx, cz) in &small {
        black_box(generator.column(cx, cz));
    }

    let time_batch = |coords: &[(i32, i32)]| -> f64 {
        let mut best = f64::INFINITY;
        for _ in 0..5 {
            let t0 = Instant::now();
            for &(cx, cz) in coords {
                black_box(generator.column(cx, cz));
            }
            best = best.min(t0.elapsed().as_secs_f64());
        }
        best
    };

    let t_small = time_batch(&small);
    let t_large = time_batch(&large);
    let ratio = t_large / t_small;
    let chunk_ratio = large.len() as f64 / small.len() as f64;
    println!(
        "worldgen linearity: {}x chunks -> {:.2}x time (best-of-5, seed={SEED}) \u{2014} expect close to {:.1}x for linear cost",
        chunk_ratio, ratio, chunk_ratio
    );

    let scene = format!("seed={SEED} small={} large={}", small.len(), large.len());
    support::record(support::Record {
        bench: "generation",
        metric: "linearity_ratio_vs_expected",
        scene: &scene,
        value: ratio / chunk_ratio,
        unit: "x",
    });

    let mut group = c.benchmark_group("worldgen/linearity");
    group.bench_function("small_3_chunks", |b| {
        b.iter(|| {
            for &(cx, cz) in &small {
                black_box(generator.column(black_box(cx), black_box(cz)));
            }
        })
    });
    group.bench_function("large_12_chunks", |b| {
        b.iter(|| {
            for &(cx, cz) in &large {
                black_box(generator.column(black_box(cx), black_box(cz)));
            }
        })
    });
    group.finish();
}

/// The ore-composition work's before/after: the ore-composition 3×3 driver's per-chunk cost
/// over a sweep large enough to exercise `pre_ore_cache` the same way a real
/// server view does (adjacent chunks share 8 of their 9 driven neighbours).
/// `tests/support/worldgen_data/biome/plains.json` carries plains' real
/// `UNDERGROUND_ORES` step (the same fixture `tests/feature_parity.rs` proves
/// matches the real JVM, per that test's own header), so this sweep actually
/// exercises `OverworldGenerator::ore_stage`/`stitch_region` — a resolver with
/// no ore data at all would make `ore_stage` an early-return no-op and this
/// bench would measure nothing relevant.
///
/// A 12×12 (144-chunk) patch specifically because `crate::overworld`'s own
/// module doc "Performance" section already has a historical number at this
/// exact size (`lodestone_server::worldgen_data::tests
/// ::served_columns_never_carry_an_unported_badlands_variant`, debug profile,
/// ~700.57s) to be comparable *in shape* against, even though that number
/// used the embedded server data (real biome variety) rather than this
/// crate's single-biome fixture and a different (debug, `cargo test`)
/// profile — see this bench's own `support::record` scene string, which
/// names both, so a reader never conflates the two numbers.
fn bench_ore_composition_sweep(c: &mut Criterion) {
    // FULL resolver, which is what makes this bench's name true. It previously
    // ran the shape-only generator, so `ore_stage` early-returned and the
    // recorded `ore_composition_column_median_us` described a pipeline with no
    // ores in it at all. Patch shrunk from 12x12 to 4x4 to stay runnable now
    // that the ore driver actually runs; 16 columns still gives interior chunks
    // whose 9 driven neighbours are mostly `pre_ore_cache` hits, which is the
    // sharing this bench exists to exercise.
    let generator = make_full_generator(SEED);
    let coords: Vec<(i32, i32)> = (0..4).flat_map(|cz| (0..4).map(move |cx| (cx, cz))).collect();

    // Warm up (first pass through the patch primes `pre_ore_cache` for the
    // interior chunks the timed pass below will find as cache hits).
    for &(cx, cz) in coords.iter().take(4) {
        black_box(generator.column(cx, cz));
    }

    // Scene names the patch size AND the resolver, because both changed: the
    // pre-fix runs of this metric were 12x12 with a resolver that supplied no
    // ore data. A different scene string is what stops `bench-compare` pairing
    // the two, per `docs/benchmark-harness.md`.
    let scene = format!("seed={SEED} patch=4x4(16 chunks) resolver=full ores=real_plains_fixture");
    let n = coords.len();
    let mut per_chunk_us: Vec<f64> = Vec::with_capacity(n);
    let t_total = Instant::now();
    for &(cx, cz) in &coords {
        let t = Instant::now();
        black_box(generator.column(cx, cz));
        per_chunk_us.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let total_s = t_total.elapsed().as_secs_f64();
    per_chunk_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = per_chunk_us[n / 2];
    println!(
        "worldgen ore-composition sweep: {n} chunks in {total_s:.3}s, median {median:.1}us/chunk (scene={scene:?})"
    );
    support::record(support::Record {
        bench: "generation",
        metric: "ore_composition_column_median_us",
        scene: &scene,
        value: median,
        unit: "us",
    });
    support::record(support::Record {
        bench: "generation",
        metric: "ore_composition_sweep_total_s",
        scene: &scene,
        value: total_s,
        unit: "s",
    });

    let mut idx = 0usize;
    c.bench_function("worldgen/ore_composition_column", |b| {
        b.iter(|| {
            let (cx, cz) = coords[idx % coords.len()];
            idx += 1;
            black_box(generator.column(black_box(cx), black_box(cz)))
        })
    });
}

/// **Region-level generation** with **peak resident-set growth**,
/// the half of that work that was missing (its render-distance and linearity halves
/// are `bench_ore_composition_sweep` and `bench_linearity_check` above).
///
/// # Deltas, never absolutes
///
/// RSS is a process-wide counter that long outlives any one measurement, so an
/// absolute reading here would be the "duration species" of vacuous test — it
/// would mostly report how much of the binary and the fixture JSON tree happened
/// to be resident, which has nothing to do with generation. Everything recorded
/// below is therefore a **delta** from a baseline taken after the generator is
/// built and warmed, plus a per-chunk normalisation. The one number worth
/// tracking across runs is bytes/chunk.
///
/// # Why the sweep is small by default
///
/// The region-level plan asks for RD 8/16/32. At the composed pipeline's real release cost those
/// are 289, 1089 and 4225 columns — roughly 4 minutes, 15 minutes and an hour on
/// this machine, which is not a benchmark anybody will run. So the default sweep
/// is small and the large radii are opt-in via `LODESTONE_BENCH_BIG_RD=1`. The
/// bytes/chunk and µs/chunk figures are what let a reader *project* RD 8/16/32,
/// and a projection is labelled as one rather than recorded as a measurement.
///
/// # On "how long before a joining player sees terrain"
///
/// The region-level plan asks to cross-reference the integrated server's spawn chunk-send radius.
/// Checked: there is no constant to cite — `view_radius` is a caller-supplied
/// parameter (`lodestone_server::integrated::open_in_memory(.., view_radius)`,
/// clamped per-client in `server.rs:2080`), so the region a joining player needs
/// is configuration, not a fixed number. µs/chunk × (2r+1)² is the answer for
/// whatever radius a caller passes, which is why this bench records the
/// per-chunk figure rather than one region total.
fn bench_region_rss(_c: &mut Criterion) {
    let big = std::env::var("LODESTONE_BENCH_BIG_RD").is_ok();
    let radii: &[i32] = if big { &[2, 4, 8, 16] } else { &[2, 4] };
    if !big {
        println!(
            "worldgen region RSS: running small radii {radii:?} only; set LODESTONE_BENCH_BIG_RD=1 \
             for the RD 8/16 sweep the region-level plan describes (RD 16 is ~1089 columns and takes minutes)"
        );
    }

    for &rd in radii {
        // FULL resolver: "how long before a joining player sees terrain" is a
        // question about the composed pipeline, not the noise router alone.
        let generator = make_full_generator(SEED);
        // Warm: build the density trees, touch the fixture JSON, let the
        // allocator reach a steady state — all of which would otherwise land in
        // the delta and be attributed to generation.
        for cx in 0..2 {
            black_box(generator.column(cx, 0));
        }

        let baseline = rss_bytes();
        let coords: Vec<(i32, i32)> =
            (-rd..=rd).flat_map(|cz| (-rd..=rd).map(move |cx| (cx, cz))).collect();
        let n = coords.len();
        let t0 = Instant::now();
        let mut peak = baseline;
        let mut non_air = 0usize;
        for &(cx, cz) in &coords {
            let col = generator.column(cx, cz);
            non_air += col.non_air_count();
            peak = peak.max(rss_bytes());
        }
        let wall_s = t0.elapsed().as_secs_f64();
        assert!(non_air > 0, "rd={rd}: generated only air — nothing measured");

        let growth = peak.saturating_sub(baseline);
        let per_chunk_us = wall_s * 1e6 / n as f64;
        println!(
            "worldgen region: rd={rd} ({n} columns) wall {wall_s:.3}s = {per_chunk_us:.0}us/chunk; \
             RSS baseline {:.1}MiB -> peak {:.1}MiB, growth {:.1}MiB = {:.1}KiB/chunk. \
             PROVISIONAL (wall-clock on a shared machine; the RSS delta is not load-sensitive). \
             Projection only: (2r+1)^2 x {per_chunk_us:.0}us => RD8 ~{:.0}s, RD16 ~{:.0}s, RD32 ~{:.0}s.",
            baseline as f64 / (1 << 20) as f64,
            peak as f64 / (1 << 20) as f64,
            growth as f64 / (1 << 20) as f64,
            growth as f64 / n as f64 / 1024.0,
            289.0 * per_chunk_us / 1e6,
            1089.0 * per_chunk_us / 1e6,
            4225.0 * per_chunk_us / 1e6,
        );

        let scene = format!("seed={SEED} rd={rd} columns={n}");
        for (metric, value, unit) in [
            ("region_rss_growth_bytes", growth as f64, "bytes"),
            ("region_rss_bytes_per_chunk", growth as f64 / n as f64, "bytes"),
            ("region_column_us", per_chunk_us, "us"),
            ("region_wall_s", wall_s, "s"),
        ] {
            support::record(support::Record {
                bench: "generation",
                metric,
                scene: &scene,
                value,
                unit,
            });
        }
    }
}

/// Current process resident set in bytes. In-process sampling rather than
/// `lodestone-allocbench`'s `/usr/bin/time -l` subprocess wrapper, because that
/// pattern can only report one figure for a whole process and the region-level plan wants a
/// per-radius sweep inside one run. `memory-stats` reads the same OS counter
/// (`task_info` on macOS) with no subprocess and no output parsing — notably
/// *not* a shell pipeline, per `CLAUDE.md`'s rule that a number a conclusion
/// rests on must not come through one.
fn rss_bytes() -> u64 {
    memory_stats::memory_stats().map_or(0, |s| s.physical_mem as u64)
}

// ===========================================================================
// Worldgen-rewrite Unit 1: counters, calibration, C_ss / C_cold
// ===========================================================================

/// Chunk-fill geometry, restated once so every prediction below can cite it.
///
/// `fill_stage` (`src/overworld/fill.rs`) is `lz in 0..16` × `lx in 0..16` ×
/// `ly in 0..height`, with exactly one `AquiferSystem::block_at` per iteration
/// and no `continue`. So one chunk fill is `256 * height` calls — and for the
/// overworld's `height = 384` (`noise_settings/overworld.json`) that is
/// **98,304**, the figure `docs/plans/worldgen-rewrite.md`'s D1 states.
///
/// This is derived from the loop bounds and asserted against the *generator's
/// own* reported height rather than hardcoded, so a settings change moves the
/// prediction instead of breaking it.
const BLOCKS_PER_CHUNK_LAYER: u64 = 16 * 16;

/// Chunks whose stages 1–4 one cold `column()` must compute: the 5×5 pre-ore
/// closure.
///
/// `vegetation_stage` reads `post_ore_world` over the 3×3 around the centre;
/// each `ore_stage` reads `pre_ore_stage` over *its own* 3×3. Composing the two
/// gives 5×5 = 25 — the number D4 states, here as an assertion rather than a
/// claim.
const COLD_PRE_ORE_CHUNKS: u64 = 25;

/// Chebyshev radius of the pre-ore closure above, in chunks: 5×5 is radius 2.
const COLD_PRE_ORE_RADIUS: i32 = 2;

/// Chebyshev radius of the **structure-start** closure, in chunks.
///
/// `pre_ore` reads `structure_refs`, which walks `structure_starts` over
/// [`lodestone_worldgen::overworld::structures::REFS_RADIUS`]. Composed with the
/// pre-ore closure that is `2 + 8 = 10` — the widest closure in the pipeline, and
/// the one `overworld::STORE_RETENTION` has to be sized against.
const SWEEP_STRUCTURE_RADIUS: i32 =
    COLD_PRE_ORE_RADIUS + lodestone_worldgen::overworld::structures::REFS_RADIUS;

/// Chunks whose **structure starts** one cold `column()` must compute.
///
/// This is this change's new closure and it is much wider than the terrain one.
/// `pre_ore_stage` reads `structure_refs_stage` for its own chunk (S1's ordering
/// edge), and `structure_refs_stage` walks `structure_starts_stage` over
/// `overworld::structures::REFS_RADIUS` = 8 chunks in every direction —
/// vanilla's own `createReferences` range. Composing that with the 5×5 pre-ore
/// closure gives a Chebyshev radius of `2 + 8 = 10`, i.e. **21×21 = 441**
/// chunks.
///
/// Derived from `REFS_RADIUS` rather than written down, so a change to vanilla's
/// range moves the prediction instead of breaking it.
const COLD_STRUCTURE_START_CHUNKS: u64 = {
    let side = 1 + 2 * SWEEP_STRUCTURE_RADIUS;
    (side * side) as u64
};

/// Ore RNG walks one cold `column()` must run: the 3×3 post-ore closure. D4's
/// "9 ore RNG walks".
const COLD_POST_ORE_CHUNKS: u64 = 9;

/// Source chunks each of `ore_stage` and `vegetation_stage` stitches into its
/// region view (the 3×3 driver, all 9 sources including the centre).
const STITCH_SOURCES_PER_STAGE: u64 = 9;

/// Fails unless all ten stages really ran, by counter.
///
/// # Why this exists
///
/// This is the guard the plan makes Unit 1 responsible for. A bench measuring
/// the "composed pipeline" against data that makes stages no-ops is the **world**
/// species of vacuous test: the flaw is in the input, the source reads as
/// rigorous, and the resulting percentage table is plausible. This file already
/// shipped that defect once — carvers, ores, vegetation and freeze were all
/// early-returning while a doc comment asserted the opposite.
///
/// A per-stage *timing* floor (which `bench_stage_split` has) catches it only
/// probabilistically and only for stages expensive enough to notice. A
/// `stage_entered` counter catches it exactly, which is why the stage guards sit
/// *below* each stage's no-data early return rather than above it.
fn assert_all_ten_stages_ran(s: &Snapshot, chunks: u64, what: &str) {
    assert!(
        counters::enabled(),
        "assert_all_ten_stages_ran called in a build without `gen-counters`; \
         every counter reads 0 and the assertions below would be vacuous"
    );
    for stage in [
        Stage::Aquifer,
        Stage::Shape,
        Stage::Biome,
        Stage::Surface,
        Stage::Materialize,
        Stage::Carve,
        Stage::Ore,
        Stage::Vegetation,
        Stage::TopLayer,
        Stage::Intern,
    ] {
        let n = s.stage_entered[stage as usize];
        assert!(
            n > 0,
            "{what}: stage {:?} never ran ({n} entries over {chunks} chunks). This is \
             the 'world' vacuity signature, not a slow stage: the resolver supplied no \
             data for it and the stage early-returned. `top_layer` reading 0 means the \
             generator is the fixture tree (no `block_freeze_facts`) rather than the \
             embedded server data; `ore`/`vegetation` reading 0 means no ore/feature \
             documents resolved.",
            stage
        );
    }
    // The biome stage is entered per call whether or not it has a parameter
    // table (it degrades per-quart, it has no single early return), so its
    // `stage_entered` count is NOT its reality check — the search count is.
    // Without this line a fixture-tree run would satisfy every assertion above.
    assert!(
        s.biome_searches > 0,
        "{what}: `biome_stage` ran but performed zero nearest-neighbour searches, so \
         `dynamic_biome` is `None` and every column got the single fallback biome. \
         Stage entry alone cannot see this — that is why it is checked separately."
    );
}

/// **Calibration.** On a known chunk, every counter must equal a hand-derived
/// expectation.
///
/// # The point
///
/// A counter that cannot predict is a counter that cannot gate. Acceptance
/// criteria for Units 3–14 are written in these counters, so if a counter is
/// merely *plausible* rather than *predicted*, every later unit's gate inherits
/// the ambiguity. Each assertion below carries its arithmetic.
///
/// # Why one cold `column()` rather than one isolated chunk fill
///
/// `pre_ore_stage` is private, so a single chunk's stages 1–4 cannot be driven
/// directly from a bench. Using a cold `column()` is strictly better anyway: it
/// pins the per-fill count *and* the dependency-closure size in one measurement,
/// and the two are independent (a wrong closure with a right per-fill count, or
/// the reverse, both fail). `block_at / stage_entered[shape]` is the per-fill
/// figure; `stage_entered[shape]` is the closure.
fn bench_counter_calibration(_c: &mut Criterion) {
    if !counters::enabled() {
        println!(
            "worldgen calibration: SKIPPED — build without `--features gen-counters`. \
             Every counter reads 0, so asserting on them here would pass vacuously. \
             Run: cargo bench -p lodestone-worldgen --features gen-counters --bench generation"
        );
        return;
    }

    // Fresh generator: the memo caches are per-generator, so this is the only
    // way to get a genuinely cold region. Reusing a warmed generator is the trap
    // that neutered two determinism gates in this repo already.
    let generator = make_embedded_generator(SEED);
    counters::reset();
    let (column, allocs) = measure_allocs(|| generator.column(0, 0));
    let s = counters::snapshot();

    let height = u64::from(column.height().unsigned_abs());
    assert_eq!(
        height, 384,
        "expected the 26.2 overworld's height=384 from `noise_settings/overworld.json`; \
         got {height}. Every prediction below is a function of this number — if the \
         settings really changed, the predictions move with them, but the 98,304 figure \
         in `docs/plans/worldgen-rewrite.md` is specific to 384 and should be re-stated."
    );

    // --- 1. `block_at` per chunk fill = 256 * height = 98,304 ------------
    let per_fill = BLOCKS_PER_CHUNK_LAYER * height;
    assert_eq!(
        per_fill, 98_304,
        "arithmetic check on the derivation itself: 16*16*{height} should be 98,304"
    );
    assert_eq!(
        s.stage_entered[Stage::Shape as usize], COLD_PRE_ORE_CHUNKS,
        "cold `column()` should compute the fill for exactly {COLD_PRE_ORE_CHUNKS} chunks \
         (the 5×5 pre-ore closure of the 3×3 ore closure of the 3×3 vegetation \
         neighbourhood); got {}",
        s.stage_entered[Stage::Shape as usize]
    );
    // `block_at` has **two** consumers as of this change, and this assertion is an
    // exhaustive decomposition rather than a literal.
    //
    // 1. `fill_stage` — one call per cell, `256 × height` per chunk fill, over the
    //    5×5 pre-ore closure. This is the term the rewrite plan's D1 states as
    //    98,304 and it is still exact.
    // 2. `StartSampler::first_occupied_height` — structure placement asking for a
    //    heightmap on terrain that does not exist yet, so it scans a column
    //    top-down through the aquifer. Data-dependent: how many probes, and how
    //    deep each one goes, is a property of which structure sets try to place
    //    near (0, 0) and where the surface is. Counted, not predicted.
    //
    // Writing this as `fill + probes` keeps consumer 1 pinned. The alternative —
    // raising the literal to the observed 99,752/fill — would have made the gate
    // green while destroying the only thing it measures: a *third* consumer, or a
    // per-fill regression to two calls per cell, would then be indistinguishable
    // from a structure-placement data change.
    //
    // **This is self-controlling.** If `bump_structure_height_probe` were dead
    // (compiled out, or never reached), `structure_probe_block_at` would read 0
    // and the identity would fail by exactly the amount the probes cost — which
    // is how this assertion was first diagnosed. No hand-bumped control is needed
    // for a term whose absence makes the equation false.
    let fill_block_at = per_fill * COLD_PRE_ORE_CHUNKS;
    assert_eq!(
        s.block_at,
        fill_block_at + s.structure_probe_block_at,
        "`block_at` must decompose exactly into its two consumers: \
         {per_fill} per fill × {COLD_PRE_ORE_CHUNKS} fills = {fill_block_at}, plus \
         {} calls from {} structure height probes = {}; got {}. A surplus means a THIRD \
         consumer of `AquiferSystem::block_at` appeared (grep for `.block_at(` — there \
         should be exactly two call sites, `overworld/fill.rs` and \
         `overworld/structures.rs`) or the fill loop stopped being one call per cell. \
         A deficit means the probe counter is over-reporting.",
        s.structure_probe_block_at,
        s.structure_height_probes,
        fill_block_at + s.structure_probe_block_at,
        s.block_at
    );
    // The probe term is not predicted, but it *is* bounded: every probe issues at
    // least one query and at most one per Y level. A ratio outside that range
    // means the counter and the loop have drifted apart, which would let the
    // identity above absorb an arbitrary amount of new work.
    assert!(
        s.structure_height_probes > 0,
        "no structure height probe ran on a cold column against embedded data, so the \
         `block_at` decomposition above degenerates to the pre-structure-placement form and would not \
         notice the probes coming back. Structure sets failed to resolve."
    );
    assert!(
        s.structure_probe_block_at >= s.structure_height_probes
            && s.structure_probe_block_at <= s.structure_height_probes * height,
        "each of the {} structure height probes must issue between 1 and {height} \
         `block_at` calls; {} total is a mean of {:.1}, outside [1, {height}]",
        s.structure_height_probes,
        s.structure_probe_block_at,
        s.structure_probe_block_at as f64 / s.structure_height_probes as f64
    );
    // Same bound one level up: a probe reuses its chunk's aquifer from
    // `StartSampler`'s own map, so builds can only ever be *fewer* than probes.
    assert!(
        s.structure_aquifers_built >= 1
            && s.structure_aquifers_built <= s.structure_height_probes,
        "structure probes built {} aquifers for {} probes — must be in \
         [1, probes], since `StartSampler` memoises one per chunk",
        s.structure_aquifers_built,
        s.structure_height_probes
    );
    // The closure size for stage 0a, which is *why* the surplus exists at all.
    // Arithmetic from `REFS_RADIUS`, not an observation.
    assert_eq!(
        s.structure_starts_computed, COLD_STRUCTURE_START_CHUNKS,
        "a cold column must compute structure starts for exactly \
         {COLD_STRUCTURE_START_CHUNKS} chunks (the 17×17 `createReferences` walk of the \
         5×5 pre-ore closure); got {}. This is the widest closure in the pipeline — \
         nearly 18× the terrain one — so a change here moves C_cold a lot.",
        s.structure_starts_computed
    );

    // --- 2. The D4 dependency closure -----------------------------------
    assert_eq!(
        s.pre_ore_computed, COLD_PRE_ORE_CHUNKS,
        "D4 predicts a cold column touches a 5×5 = {COLD_PRE_ORE_CHUNKS}-chunk pre-ore \
         region; got {}",
        s.pre_ore_computed
    );
    assert_eq!(
        s.post_ore_computed, COLD_POST_ORE_CHUNKS,
        "D4 predicts {COLD_POST_ORE_CHUNKS} ore RNG walks on a cold column; got {}",
        s.post_ore_computed
    );
    // `Stage::Aquifer` is the one stage with the same two-consumer problem as
    // `block_at`: `StartSampler` builds a real aquifer per probed chunk, and those
    // chunks are not in the pre-ore closure. Decomposed, for the same reason.
    for (stage, expected) in [
        (Stage::Aquifer, COLD_PRE_ORE_CHUNKS + s.structure_aquifers_built),
        (Stage::Surface, COLD_PRE_ORE_CHUNKS),
        (Stage::Materialize, COLD_PRE_ORE_CHUNKS),
        (Stage::Carve, COLD_PRE_ORE_CHUNKS),
        (Stage::Ore, COLD_POST_ORE_CHUNKS),
        (Stage::Vegetation, 1),
        (Stage::TopLayer, 1),
        (Stage::Intern, 1),
    ] {
        assert_eq!(
            s.stage_entered[stage as usize], expected,
            "stage {:?}: expected {expected} runs for one cold column, got {}",
            stage, s.stage_entered[stage as usize]
        );
    }

    // --- 3. D2's stitch copies: ZERO, which is U7's acceptance criterion ---
    //
    // Unit 7 of `docs/plans/worldgen-rewrite.md` replaced both stitches with views
    // that borrow the nine source grids rather than copying them —
    // `crate::feature::region_view::RegionView` for the ore driver and
    // `VegGrid::with_sources` for vegetation. This assertion is the criterion.
    //
    // Written as a two-hypothesis magnitude check rather than a bare `== 0`, per
    // `CLAUDE.md`'s "predict the value, do not merely assert the sign of the
    // change". Both hypotheses come from constants outside the code under test:
    //
    // * **Pre-U7** — `ore_stage` stitched 9 sources once per ore walk and
    //   `vegetation_stage` stitched 9 once, each copying `256 * height` cells:
    //   `(9 walks × 9 + 9) × 98,304`.
    // * **Post-U7** — nothing is copied to make the neighbourhood addressable, so
    //   exactly 0. Not "small": there is no residual term, because the counter is
    //   bumped from the stitch loops and both are deleted.
    //
    // The two are 8.8 million apart, so no measurement can be ambiguous between
    // them, and a *partial* revert (one stitch back, one gone) lands on neither
    // and fails.
    let pre_u7_stitch =
        (COLD_POST_ORE_CHUNKS * STITCH_SOURCES_PER_STAGE + STITCH_SOURCES_PER_STAGE) * per_fill;
    assert_eq!(
        pre_u7_stitch, 8_847_360,
        "arithmetic check on the pre-U7 hypothesis itself: \
         ({COLD_POST_ORE_CHUNKS} × {STITCH_SOURCES_PER_STAGE} + {STITCH_SOURCES_PER_STAGE}) \
         × {per_fill} should be 8,847,360"
    );
    assert_eq!(
        s.stitch_cells, 0,
        "U7's acceptance criterion: a cold column must copy ZERO cells to make its 3×3 \
         neighbourhood addressable. The pre-U7 hypothesis for this same column is \
         {pre_u7_stitch}; got {}. Anything non-zero means a region stitch is back — \
         grep `bump_stitch_cells` for the caller.",
        s.stitch_cells
    );
    // **Control for the assertion above.** An absence claim is only as good as the
    // evidence the detector would have fired, and `stitch_cells == 0` is exactly
    // the shape that reads as a pass when the instrument is dead. Bumping by hand
    // must move the snapshot. Safe to do here: `s` was already taken, so nothing
    // above or below re-reads the live counter.
    counters::bump_stitch_cells(7);
    assert_eq!(
        counters::snapshot().stitch_cells, 7,
        "control: the stitch_cells counter must be observed moving in this very build, \
         or the `== 0` above proves only that the hook is compiled out"
    );
    // The vegetation stitch used to allocate a `String` per cell. **Unit 3
    // deleted that term**, so this assertion is now the other way round — and it
    // is written as a two-hypothesis magnitude check rather than a bound in one
    // direction, per `CLAUDE.md`'s "predict the value, do not merely assert the
    // sign of the change".
    //
    // The two hypotheses, both derived from constants outside the code under
    // test:
    //
    // * **Pre-U3** (`stitch_veg_region` calls `to_string()` per cell):
    //   `string_allocs >= 884,736`.
    // * **Post-U3** (both grids carry `StateId`): the only remaining
    //   contributor is `StateInterner`'s one-allocation-per-distinct-state
    //   warmup, which is bounded by the number of distinct block states this
    //   data can produce — two orders of magnitude below the above. Measured at
    //   **65** for the cold column on the embedded data.
    //
    // A ceiling rather than `== 65` deliberately: the exact count is a property
    // of the worldgen *data*, so pinning it would make this assertion fail on a
    // data update for no good reason. The ceiling is far enough below the
    // pre-U3 hypothesis that no confusion between the two is possible, and low
    // enough that a regression to per-cell or per-block interning (the real
    // failure mode, which would put it in the tens of thousands) still trips it.
    let veg_stitch_cells = STITCH_SOURCES_PER_STAGE * per_fill;
    assert_eq!(
        veg_stitch_cells, 884_736,
        "the ~885k figure D2 calls the single most damning number in the diagnosis: \
         {STITCH_SOURCES_PER_STAGE} × {per_fill}"
    );
    /// Ceiling on interner warmup allocations for one cold column. See above for
    /// why this is a ceiling and not an equality.
    const U3_INTERN_CEILING: u64 = 1_000;
    assert!(
        s.string_allocs < U3_INTERN_CEILING,
        "expected fewer than {U3_INTERN_CEILING} String allocations on the block path \
         after Unit 3 (interner warmup only, measured at 65 on embedded data); got {}. \
         If this is >= {veg_stitch_cells}, something is allocating a String per \
         neighbourhood cell again — the loop that used to do it (`stitch_veg_region`) \
         was deleted by Unit 7, so a regression here means a new per-cell copy, not \
         that one coming back.",
        s.string_allocs
    );

    // --- 4. Corner lookups: 8 per *cell fill* ----------------------------
    // This assertion used to read `corner_lookups >= 8 * block_at`, on the
    // premise that `interpolate` unrolls 8 `corner()` calls per block. **Unit 4's
    // corner hoist deleted that premise**: the eight corners of a cell are now
    // fetched once for the whole cell, so the true figure is 8 per *cell fill* —
    // 128× below the old floor, since a 4×8×4 cell holds 128 blocks. The old
    // floor is now not merely wrong but inverted, and its diagnostic ("a lower
    // number means the root is no longer an `Interpolated` node") would send a
    // reader hunting for a missing node that is present and working.
    //
    // What survives as a bench-level check is the *identity*, which holds
    // regardless of geometry, plus a liveness floor. The magnitudes have a
    // dedicated gate with derived predictions and named failure diagnoses:
    // `tests/engine_counters.rs`, which is its own binary because these counters
    // are process-global.
    assert_eq!(
        s.corner_lookups,
        8 * s.cell_fills,
        "every `interpolated` cell fill assembles exactly 8 corners: {} lookups \
         over {} fills. This is an identity, so a mismatch is a broken hoist, not \
         a geometry change.",
        s.corner_lookups,
        s.cell_fills
    );
    assert!(
        s.cell_fills > 0 && s.corner_evals > 0,
        "no cell was filled ({}) or no corner evaluated ({}) during a cold \
         column, so the interpolation path did not run at all",
        s.cell_fills,
        s.corner_evals
    );
    assert!(
        s.corner_evals < s.corner_lookups,
        "corner evaluations ({}) must be strictly fewer than corner lookups \
         ({}) — adjacent cells share corners, so equality means the per-slot memo \
         beneath the cell cache is dead",
        s.corner_evals,
        s.corner_lookups
    );

    // --- 5. Every stage real -------------------------------------------
    assert_all_ten_stages_ran(&s, 1, "calibration (cold column, embedded data)");

    println!("\n=== worldgen counter calibration: 1 cold column, seed {SEED}, EMBEDDED data ===");
    print_counters(&s, 1);
    println!("  heap allocations in the column path: {allocs}");
    println!(
        "  DERIVED: block_at/fill = {} (plan states 98,304); pre_ore closure = {} (plan: 25); \
         ore walks = {} (plan: 9)",
        (s.block_at - s.structure_probe_block_at) / s.stage_entered[Stage::Shape as usize].max(1),
        s.pre_ore_computed,
        s.post_ore_computed
    );
    println!(
        "  DERIVED: structure starts closure = {} (21x21 = {COLD_STRUCTURE_START_CHUNKS}); \
         height probes = {} over {} block_at calls ({:.1} deep each, {:.1}% of all block_at)",
        s.structure_starts_computed,
        s.structure_height_probes,
        s.structure_probe_block_at,
        s.structure_probe_block_at as f64 / s.structure_height_probes.max(1) as f64,
        100.0 * s.structure_probe_block_at as f64 / s.block_at.max(1) as f64,
    );

    // The **fill-only** figure, i.e. with the structure-probe term removed. Before
    // this subtraction the recorded metric drifted from 98,304 to 99,752 and the
    // JSONL history read as a 1.5% per-fill regression that had not happened.
    support::record(support::Record {
        bench: "generation",
        metric: "calibration_block_at_per_chunk_fill",
        scene: "seed=42 chunk=(0,0) resolver=embedded cold=true",
        value: ((s.block_at - s.structure_probe_block_at)
            / s.stage_entered[Stage::Shape as usize].max(1)) as f64,
        unit: "calls",
    });
}

/// Prints every counter, normalised per chunk where that is meaningful.
fn print_counters(s: &Snapshot, chunks: u64) {
    let n = chunks.max(1) as f64;
    let row = |name: &str, v: u64| {
        println!("  {name:<34} {v:>14}   {:>12.1}/chunk", v as f64 / n);
    };
    row("block_at", s.block_at);
    row("density_evals (chunk sampler)", s.density_evals_total());
    row("density_computes (point eval)", s.density_point_computes_total());
    row("corner_lookups", s.corner_lookups);
    row("slot_cache_hits", s.slot_hits);
    row("slot_cache_misses (real evals)", s.slot_misses);
    row("palette_intern_new", s.palette_intern_new);
    row("palette_intern_hit", s.palette_intern_hit);
    row("pre_ore_computed", s.pre_ore_computed);
    row("pre_ore_cache_hits", s.pre_ore_hits);
    row("post_ore_computed", s.post_ore_computed);
    row("post_ore_cache_hits", s.post_ore_hits);
    row("biome_nn_searches", s.biome_searches);
    row("biome_rows_compared", s.biome_rows_compared);
    row("stitch_cells_copied", s.stitch_cells);
    row("string_allocs", s.string_allocs);
    row("structure_starts_computed", s.structure_starts_computed);
    row("structure_height_probes", s.structure_height_probes);
    row("structure_probe_block_at", s.structure_probe_block_at);
    row("structure_aquifers_built", s.structure_aquifers_built);
    row("rng_draws (all stages)", s.rng_draws_total());
    println!("  rng draws by stage:");
    for (i, name) in lodestone_worldgen::counters::STAGE_NAMES.iter().enumerate() {
        if s.rng_draws[i] > 0 {
            println!(
                "    {name:<14} {:>14}   {:>12.1}/chunk",
                s.rng_draws[i],
                s.rng_draws[i] as f64 / n
            );
        }
    }
    println!("  stage entries (work actually done):");
    for (i, name) in lodestone_worldgen::counters::STAGE_NAMES.iter().enumerate() {
        if s.stage_entered[i] > 0 {
            println!("    {name:<14} {:>14}", s.stage_entered[i]);
        }
    }
    println!("  top density component kinds by evaluation count:");
    for (name, count) in s.density_evals_ranked().iter().take(8) {
        println!("    {name:<20} {count:>14}");
    }
    // Per-slot corner/flat-cache evaluations. This is the counter U4's prediction
    // is written in ("1,225 corner evaluations per interpolated slot per chunk",
    // the 5x49x5 lattice), and it has to be per-slot rather than a total: the
    // aquifer builds eight samplers sharing one slot address space, so a total
    // cannot be attributed to the `final_density` interpolator that the lattice
    // prediction is about. Printed normalised per chunk so the 1,225 is directly
    // readable rather than needing division by the closure size.
    println!("  slot-cache misses (real evaluations) by density slot, per chunk:");
    let mut slots: Vec<(usize, u64)> = s
        .slot_misses_by_slot
        .iter()
        .enumerate()
        .filter(|&(_, &n)| n > 0)
        .map(|(i, &n)| (i, n))
        .collect();
    slots.sort_by(|a, b| b.1.cmp(&a.1));
    for (slot, count) in slots.iter().take(12) {
        println!(
            "    slot {slot:<3} {count:>14}   {:>12.1}/chunk",
            *count as f64 / n
        );
    }
}

/// **C_ss and C_cold**, exactly as `docs/plans/worldgen-rewrite.md` §Q3 defines
/// them. This is the baseline every later unit's acceptance criteria are stated
/// against.
///
/// * **C_ss** — median wall time of `column(cx, cz)` over the **100 interior
///   chunks of a 12×12 sweep**, single thread, release profile, embedded server
///   data, all stages real, seed 42, every stage counter-asserted to have run
///   exactly once per chunk across the sweep.
/// * **C_cold** — wall time of the first `column()` in a fresh region.
///
/// # The "interior" 100, and why the border is excluded
///
/// A 12×12 sweep with a one-chunk border removed is 10×10 = 100. The border
/// chunks are excluded because their neighbours' stages were computed *by them*
/// rather than earlier in the sweep, so they carry cold-neighbour cost and would
/// drag the median toward C_cold. Interior chunks find their dependencies already
/// computed — "warm in the sense a sweep makes natural", which is the phrase the
/// plan uses and the thing a server actually experiences.
///
/// # Exactly-once, per stage, with the right radius for each
///
/// The plan asks for "every stage ran exactly once per chunk". That is one
/// statement with a different chunk count per stage, because each stage has its
/// own dependency radius, and stating it as a single number would be wrong for
/// nine of the ten. For a 12×12 sweep:
///
/// | stage | chunks | why |
/// |---|---|---|
/// | aquifer…carve | 16×16 = 256 | 5×5 pre-ore closure of the sweep |
/// | ore | 14×14 = 196 | 3×3 post-ore closure of the sweep |
/// | vegetation, top_layer, intern | 12×12 = 144 | the sweep itself |
///
/// This is a much stronger statement than "144 of each": it fails if a single
/// chunk's stage is recomputed *or* if the closure is the wrong size. It holds
/// only because 256 < the memo caches' 512-entry capacity — if a future sweep
/// grows past that, FIFO eviction will cause recomputation and **this assertion
/// is what will report it**, rather than a mysteriously slower median.
fn bench_steady_state_and_cold(_c: &mut Criterion) {
    const SIDE: i32 = 12;

    // ---- C_cold: first column in a fresh region -----------------------
    // Fresh generator, so nothing is warm. Timed before anything else runs.
    let cold_generator = make_embedded_generator(SEED);
    counters::reset();
    let t0 = Instant::now();
    let cold_column = cold_generator.column(0, 0);
    let c_cold_us = t0.elapsed().as_secs_f64() * 1e6;
    let cold_snapshot = counters::snapshot();
    black_box(cold_column.non_air_count());

    // ---- C_ss: 12×12 sweep, median of the 100 interior ----------------
    let generator = make_embedded_generator(SEED);
    let coords: Vec<(i32, i32)> =
        (0..SIDE).flat_map(|cz| (0..SIDE).map(move |cx| (cx, cz))).collect();

    // Instructions are read per column **inside the sweep**, so `I_ss` below is the
    // median of the same 100 interior columns C_ss is the median of — the same
    // scene, the same definition, two instruments. Measuring it on a *repeat* of
    // one already-swept column instead would have been much easier and wrong: a
    // repeat re-runs only vegetation/top_layer/intern (the store answers the rest),
    // which measured 3.6 ms against C_ss's 21.4 ms. A comparator blind to 83% of
    // the cost is worse than no comparator, because it looks like one.
    //
    // The two reads cost ~1.2 µs against a ~20 ms column, i.e. 0.006%.
    assert_instruction_counter_is_real();
    counters::reset();
    let mut per_chunk: Vec<(i32, i32, f64, u64)> = Vec::with_capacity(coords.len());
    let sweep_start = Instant::now();
    let mut non_air_total = 0usize;
    for &(cx, cz) in &coords {
        let insns_before = instructions_retired();
        let t = Instant::now();
        let col = generator.column(cx, cz);
        let us = t.elapsed().as_secs_f64() * 1e6;
        let insns = instructions_retired().saturating_sub(insns_before);
        non_air_total += col.non_air_count();
        per_chunk.push((cx, cz, us, insns));
    }
    let sweep_s = sweep_start.elapsed().as_secs_f64();
    let s = counters::snapshot();
    assert!(non_air_total > 0, "the whole sweep generated only air — nothing was measured");

    // Interior = the sweep minus a one-chunk border.
    let is_interior = |cx: i32, cz: i32| cx > 0 && cz > 0 && cx < SIDE - 1 && cz < SIDE - 1;
    let mut interior: Vec<f64> = per_chunk
        .iter()
        .filter(|&&(cx, cz, _, _)| is_interior(cx, cz))
        .map(|&(_, _, us, _)| us)
        .collect();
    let mut interior_insns: Vec<u64> = per_chunk
        .iter()
        .filter(|&&(cx, cz, _, _)| is_interior(cx, cz))
        .map(|&(_, _, _, insns)| insns)
        .collect();
    assert_eq!(
        interior.len(),
        100,
        "C_ss is defined over the 100 interior chunks of a 12×12 sweep; this filter \
         selected {}. The definition and the filter have drifted apart.",
        interior.len()
    );
    interior.sort_by(|a, b| a.partial_cmp(b).unwrap());
    interior_insns.sort_unstable();
    let c_ss_us = (interior[49] + interior[50]) / 2.0;
    let p95 = interior[94];
    let i_ss = (interior_insns[49] + interior_insns[50]) / 2;
    let i_ss_p95 = interior_insns[94];

    // ---- The eviction control, which every counter below depends on ----
    //
    // This runs with the counters feature **off** as well, deliberately: it is the
    // one check that separates "each stage ran once" from "each stage ran once,
    // plus however many times the retention ceiling made us redo it", and it needs
    // no counters to do it. The detector itself is proven live in
    // `overworld::store`'s own `a_view_walked_across_the_world_...` gate, which
    // asserts `evicted() > 0` over a 240-step walk — so a zero here is a
    // measurement, not a dead instrument.
    assert_eq!(
        generator.store_evictions(),
        0,
        "the {SIDE}×{SIDE} sweep evicted {} store entries, so every stage-computation \
         count below is inflated by recomputation and the median is measuring the \
         eviction rather than the pipeline. The sweep's working set is \
         {} entries (its {}-radius structure closure) and `STORE_RETENTION` must exceed \
         it — see DESIGN.md §12.130, where this read 740 pre-ore computations against a \
         prediction of 256.",
        generator.store_evictions(),
        (SIDE + 2 * SWEEP_STRUCTURE_RADIUS) * (SIDE + 2 * SWEEP_STRUCTURE_RADIUS),
        SWEEP_STRUCTURE_RADIUS
    );

    // ---- Counter assertions on the sweep ------------------------------
    if counters::enabled() {
        // Printed *before* the assertions, deliberately: an assertion failure here
        // used to abort with a single stage's number and no attribution, which is
        // the least useful moment to lose the table.
        println!("\n  -- counters over the {}-chunk sweep (pre-assertion) --", coords.len());
        print_counters(&s, u64::try_from(coords.len()).unwrap());
        let sweep = u64::try_from(coords.len()).unwrap();
        let closure = |extra: i32| -> u64 { u64::try_from((SIDE + 2 * extra) * (SIDE + 2 * extra)).unwrap() };
        for (stage, expected, radius) in [
            // See the calibration bench: structure height probes build aquifers
            // for chunks outside the pre-ore closure, so this one stage carries a
            // second term. Counted, not predicted — the probe set is data.
            (
                Stage::Aquifer,
                closure(2) + s.structure_aquifers_built,
                "5×5 pre-ore closure + structure probes",
            ),
            (Stage::Shape, closure(2), "5×5 pre-ore closure"),
            (Stage::Surface, closure(2), "5×5 pre-ore closure"),
            (Stage::Materialize, closure(2), "5×5 pre-ore closure"),
            (Stage::Carve, closure(2), "5×5 pre-ore closure"),
            (Stage::Ore, closure(1), "3×3 post-ore closure"),
            (Stage::Vegetation, sweep, "the sweep itself"),
            (Stage::TopLayer, sweep, "the sweep itself"),
            (Stage::Intern, sweep, "the sweep itself"),
        ] {
            assert_eq!(
                s.stage_entered[stage as usize], expected,
                "exactly-once violated for stage {:?}: expected {expected} runs over a \
                 {SIDE}×{SIDE} sweep ({radius}), got {}. A HIGHER number means a chunk's \
                 stage was recomputed — check `store_evictions()` below first: the \
                 retention ceiling being smaller than the sweep's real working set is \
                 what produced 740 pre-ore computations against this same 256 \
                 (DESIGN.md §12.130). A LOWER number means a stage stopped running.",
                stage, s.stage_entered[stage as usize]
            );
        }
        assert_eq!(
            s.pre_ore_computed,
            closure(2),
            "pre-ore computations over the sweep should equal the 5×5 closure exactly"
        );
        assert_eq!(
            s.post_ore_computed,
            closure(1),
            "post-ore computations over the sweep should equal the 3×3 closure exactly"
        );
        // Structure starts are the **widest** closure in the pipeline, and the
        // reason `STORE_RETENTION` is what it is. Predicted from `REFS_RADIUS`.
        assert_eq!(
            s.structure_starts_computed,
            closure(SWEEP_STRUCTURE_RADIUS),
            "structure-start computations over the sweep should equal the \
             {SWEEP_STRUCTURE_RADIUS}-radius closure ({}×{}) exactly; got {}. This counter \
             read 7,569 against this same prediction of 1,024 at `ac5391d1` — a 7.4× \
             recompute caused entirely by the store's retention ceiling.",
            SIDE + 2 * SWEEP_STRUCTURE_RADIUS,
            SIDE + 2 * SWEEP_STRUCTURE_RADIUS,
            s.structure_starts_computed
        );
        assert_all_ten_stages_ran(&s, sweep, "C_ss sweep (embedded data)");
        assert_all_ten_stages_ran(&cold_snapshot, 1, "C_cold (embedded data)");
    } else {
        println!(
            "worldgen C_ss/C_cold: counters NOT compiled in, so the exactly-once \
             invariant was NOT checked. The timings below are still real, but they \
             describe a pipeline whose stage participation is unverified — which is \
             precisely the condition that let this file measure an ore-free pipeline \
             for as long as it did. Re-run with --features gen-counters to gate them."
        );
    }

    // ---- Steady-state allocation count -------------------------------
    // One more interior column on the warm generator. Its neighbours' stages are
    // all computed, so this is the steady-state serve path — the number the
    // plan's allocation budget ratchets down to "0 from the hot path, plus O(1)
    // for the returned column's own buffers".
    let (warm_col, steady_allocs, steady_allocs_by_stage) =
        measure_allocs_by_stage(|| generator.column(5, 5));
    black_box(warm_col.non_air_count());

    // ---- Repeatability probe: why I_ss is the comparator ---------------
    //
    // The instrument's justification, measured here rather than quoted. Both
    // numbers come from the **same repetitions of identical work** — that is the
    // only shape in which a spread means reproducibility rather than scene
    // variation, which is why this cannot be read off the interior-100
    // distribution above.
    //
    // The repeated work is `column(5, 5)` on the already-swept generator: not the
    // C_ss path (the store answers pre_ore/post_ore, so only
    // vegetation/top_layer/intern re-run), and it does not need to be. It only has
    // to be *the same work each time*.
    //
    // Deliberately outside `measure_allocs`: the counting allocator adds
    // instructions of its own at every allocation in the window.
    const PROBE_REPS: usize = 7;
    let mut probe: Vec<(u64, f64)> = Vec::with_capacity(PROBE_REPS);
    for _ in 0..PROBE_REPS {
        let t = Instant::now();
        let before = instructions_retired();
        let col = generator.column(5, 5);
        let insns = instructions_retired().saturating_sub(before);
        let us = t.elapsed().as_secs_f64() * 1e6;
        black_box(col.non_air_count());
        probe.push((insns, us));
    }
    let spread = |lo: f64, hi: f64, mid: f64| 100.0 * (hi - lo) / mid.max(1.0);
    let mut probe_insns: Vec<u64> = probe.iter().map(|&(i, _)| i).collect();
    let mut probe_us: Vec<f64> = probe.iter().map(|&(_, u)| u).collect();
    probe_insns.sort_unstable();
    probe_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let insn_spread = spread(
        probe_insns[0] as f64,
        probe_insns[PROBE_REPS - 1] as f64,
        probe_insns[PROBE_REPS / 2] as f64,
    );
    let us_spread = spread(probe_us[0], probe_us[PROBE_REPS - 1], probe_us[PROBE_REPS / 2]);

    // ---- Report ------------------------------------------------------
    println!("\n=== worldgen C_ss / C_cold — release baseline, EMBEDDED server data ===");
    println!("  scene: seed={SEED}, {SIDE}x{SIDE} sweep ({} chunks), single thread", coords.len());
    println!("  C_ss   (median of 100 interior) : {c_ss_us:>12.1} us   target <= 1000 us (GOAL, not gate)");
    println!("  C_ss   p95 interior             : {p95:>12.1} us");
    println!("  C_cold (first column, fresh)    : {c_cold_us:>12.1} us   target <= 8000 us");
    println!("  whole {SIDE}x{SIDE} sweep            : {sweep_s:>12.3} s");
    println!("  steady-state heap allocs/column : {steady_allocs:>12}   target 0 from hot path + O(1) output");
    println!(
        "  I_ss   (median insns of same 100): {i_ss:>12}   <-- THE before/after comparator"
    );
    println!("  I_ss   p95 interior             : {i_ss_p95:>12}");
    println!(
        "  I_ss   insn/us                  : {:>12.1}   (moves with I_ss flat => a locality story, not a work story)",
        i_ss as f64 / c_ss_us.max(1.0)
    );
    println!(
        "  repeatability, {PROBE_REPS} reps of one : insns {insn_spread:>6.2}%   wall {us_spread:>6.2}%   \
         <-- why the comparator is the left one"
    );
    if counters::enabled() {
        // Attribution for whatever the total still is. Unit 3 took it from
        // 905,459 to 20,684 by interning; the split says which stage owns the
        // residue, so the next unit aims at a measured target rather than a
        // plausible one.
        println!("  -- steady-state allocs by stage (needs gen-counters) --");
        let mut ranked: Vec<(usize, u64)> = steady_allocs_by_stage.iter().copied().enumerate().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        for (stage, n) in ranked {
            if n == 0 {
                continue;
            }
            let pct = 100.0 * n as f64 / steady_allocs.max(1) as f64;
            println!(
                "     {:<14} {n:>10}  ({pct:>5.1}%)",
                counters::STAGE_NAMES[stage]
            );
        }
    }
    if counters::enabled() {
        println!("\n  -- counters for the single COLD column (C_cold) --");
        print_counters(&cold_snapshot, 1);
    }

    let scene = format!(
        "seed={SEED} patch={SIDE}x{SIDE}({} chunks) interior=100 resolver=embedded thread=1",
        coords.len()
    );
    for (metric, value, unit) in [
        ("c_ss_median_interior_us", c_ss_us, "us"),
        ("c_ss_p95_interior_us", p95, "us"),
        ("c_cold_first_column_us", c_cold_us, "us"),
        ("c_ss_sweep_total_s", sweep_s, "s"),
        ("steady_state_heap_allocs_per_column", steady_allocs as f64, "allocs"),
        // The comparator. Recorded last so a `bench-compare` reader sees it beside
        // the wall-clock figures it is meant to replace as the acceptance criterion.
        ("i_ss_median_instructions_per_column", i_ss as f64, "instructions"),
    ] {
        support::record(support::Record {
            bench: "generation",
            metric,
            scene: &scene,
            value,
            unit,
        });
    }
}

/// **The number that decides the project**: the vegetation walk's own cost, in
/// release, on embedded data — per `docs/plans/worldgen-rewrite.md` §Q3, whether
/// it is under or over ~1 ms.
///
/// # Why this is measured with `column_timed` and what that changes
///
/// `column_timed` runs the centre chunk's stages 1–4 itself rather than through
/// `pre_ore_stage`'s memo cache, so its `aquifer`…`carve` figures are
/// **cache-cold** for the centre while its `ore`/`vegetation` figures include
/// only whatever neighbour work the caches did not already have. The vegetation
/// number is therefore the one to read here, and it is read *warm* (the sweep
/// below primes the neighbourhood first) because that is the condition C_ss
/// describes.
///
/// # The counter that makes the µs figure trustworthy
///
/// A per-stage duration on a shared machine is a sample. What is *not* a sample
/// is `rng_draws[vegetation]` — the spec-bound draw count the plan says SIMD and
/// parallelism cannot touch. Reported alongside, it converts the timing into a
/// **cost per draw**, which is the quantity §Q3's five cost-per-draw candidates
/// are about and the only form in which a later unit can claim an improvement
/// without re-running this exact machine state.
fn bench_vegetation_walk_cost(_c: &mut Criterion) {
    let generator = make_embedded_generator(SEED);
    // Warm the neighbourhood the way a sweep does, so the vegetation stage is
    // measured against already-computed neighbours rather than paying for 25
    // pre-ore chunks inside the timed window.
    for cz in -2..=2 {
        for cx in -2..=2 {
            black_box(generator.column(cx, cz));
        }
    }

    counters::reset();
    let (col, times) = generator.column_timed(0, 0);
    let s = counters::snapshot();
    black_box(col.non_air_count());

    let veg_us = times.vegetation.as_secs_f64() * 1e6;
    let veg_draws = s.rng_draws[Stage::Vegetation as usize];
    let ns_per_draw = if veg_draws > 0 {
        times.vegetation.as_secs_f64() * 1e9 / veg_draws as f64
    } else {
        f64::NAN
    };

    // The per-stage µs table on EMBEDDED data — the U2 deliverable.
    // `bench_stage_split` produces the same shape against the fixture tree,
    // where `biome` and `top_layer` are structurally inert; this is the version
    // with all ten stages live, and the two are deliberately recorded under
    // different scene strings so `bench-compare` can never pair them.
    let stage_us: [(&str, f64); 10] = [
        ("aquifer", times.aquifer.as_secs_f64() * 1e6),
        ("shape", times.shape.as_secs_f64() * 1e6),
        ("biome", times.biome.as_secs_f64() * 1e6),
        ("surface", times.surface.as_secs_f64() * 1e6),
        ("materialize", times.materialize.as_secs_f64() * 1e6),
        ("carve", times.carve.as_secs_f64() * 1e6),
        ("ore", times.ore.as_secs_f64() * 1e6),
        ("vegetation", veg_us),
        ("top_layer", times.top_layer.as_secs_f64() * 1e6),
        ("intern", times.intern.as_secs_f64() * 1e6),
    ];
    let total_us: f64 = stage_us.iter().map(|&(_, v)| v).sum();
    println!("\n=== per-stage split, release, EMBEDDED server data (all ten stages live) ===");
    println!("  (centre chunk's stages 1-4 are cache-cold by `column_timed`'s design; ore and");
    println!("   vegetation read the warm 5x5 neighbourhood, matching C_ss's condition)");
    for &(name, us) in &stage_us {
        println!("  {name:<12} {us:>12.1} us  {:>6.2}%", 100.0 * us / total_us);
    }
    println!("  {:<12} {total_us:>12.1} us", "TOTAL");
    if counters::enabled() {
        // The same guard the other embedded benches carry: a stage reading ~0
        // here must be genuinely cheap, not absent. `top_layer` is the one this
        // catches — against the fixture tree it reads 0.000% because it never
        // ran at all, and no timing threshold can tell those apart.
        assert_all_ten_stages_ran(&s, 1, "per-stage split (embedded data)");
    }
    for &(name, us) in &stage_us {
        support::record(support::Record {
            bench: "generation",
            metric: &format!("embedded_stage_{name}_us"),
            scene: "seed=42 chunk=(0,0) warm=5x5 resolver=embedded split=10stage",
            value: us,
            unit: "us",
        });
    }

    println!("\n=== the sub-ms question: vegetation walk cost, release, embedded data ===");
    println!("  vegetation stage        : {veg_us:>12.1} us   (~1 ms is the decision threshold)");
    println!("  RNG draws in vegetation : {veg_draws:>12}   (spec-bound: cannot be reduced at parity)");
    println!("  cost per draw           : {ns_per_draw:>12.1} ns   (ours to reduce — plan Q3's five candidates)");
    println!(
        "  verdict: vegetation alone is {} the ~1ms threshold",
        if veg_us <= 1000.0 { "UNDER" } else { "OVER" }
    );
    if counters::enabled() {
        assert!(
            veg_draws > 0,
            "the vegetation stage drew zero RNG values, so it did no placement work at \
             all — the cost-per-draw figure would be meaningless and the µs number would \
             be measuring an early return"
        );
    }

    let scene = format!("seed={SEED} chunk=(0,0) warm=5x5 resolver=embedded");
    for (metric, value, unit) in [
        ("vegetation_stage_us", veg_us, "us"),
        ("vegetation_rng_draws", veg_draws as f64, "draws"),
        ("vegetation_ns_per_rng_draw", ns_per_draw, "ns"),
    ] {
        support::record(support::Record {
            bench: "generation",
            metric,
            scene: &scene,
            value,
            unit,
        });
    }
}

criterion_group!(
    benches,
    bench_counter_calibration,
    bench_steady_state_and_cold,
    bench_vegetation_walk_cost,
    bench_column_throughput,
    bench_stage_split,
    bench_linearity_check,
    bench_ore_composition_sweep,
    bench_region_rss
);
criterion_main!(benches);
