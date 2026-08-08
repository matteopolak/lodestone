//! **Where do the client's CPU cycles go, from a chunk packet arriving to its
//! pixels being drawn?** — instruction-denominated, per-stage, over real
//! generated terrain.
//!
//! # Why instructions and not time
//!
//! Wall clock on this machine reproduces to **10.8%** peak-to-peak, and one
//! worldgen stage swung **22% across three runs of an identical binary** while
//! an allocation counter read 905,459 to the digit 3 of 3 (`DESIGN.md` §12.98,
//! §12.103). A duration measured here has already been attributed to the wrong
//! cause outright — a "debug versus release" story that turned out to be pure
//! machine load.
//!
//! `proc_pid_rusage(getpid(), RUSAGE_INFO_V4, …)` returns `ri_instructions` and
//! `ri_cycles` for the calling process: populated on Apple Silicon,
//! unprivileged, ~600 ns per read, and reproducible to **0.1–0.6%** under
//! concurrent-agent load. Thermal state, DVFS and P-vs-E-core placement change
//! how *fast* instructions retire, never *which* instructions a deterministic
//! program executes — exactly the confound §12.103 measured. See
//! `docs/plans/worldgen-cycle-accounting.md` for the characterisation this
//! harness is the client-side first customer of, and `docs/client-chunk-cycles.md`
//! for how to read and extend the output.
//!
//! # The stages, and why these boundaries
//!
//! The boundaries are the ones production already crosses, read from
//! `crates/protocol/v770/src/adapter.rs:3689-3716` (the real
//! `LEVEL_CHUNK_WITH_LIGHT` arm) rather than invented here:
//!
//! | stage | the exact production call |
//! |---|---|
//! | S1 decode | `VersionAdapter::handle_packet` into a sink that discards |
//! | S2 insert | `World::load(pos, LoadedChunk)` over pre-cloned chunks |
//! | S1+S2 | `handle_packet` into the real `World` — the consistency control |
//! | S3a snapshot | `snapshot_section` (the 27-section gather) |
//! | S3b mesh | `mesh_snapshot_models` + `mesh_snapshot_fluids` |
//! | S4 submit | the *marginal* cost of one more section inside `RenderState::render` |
//!
//! S4 is a **difference**: render the same frame with no terrain resident and
//! then with every fixture section, and divide by the section count. The fixed
//! per-frame cost (sky, clears, encoder setup, queue submit, driver bring-up)
//! cancels, leaving the term that actually scales with render distance — which
//! is the whole question, since `gpu/frame.rs:459/480/720` iterate *every*
//! resident section with no frustum and no distance cull.
//!
//! Nothing here imports a version crate. `handle_packet` is reached through
//! `lodestone_registry::adapter_for_protocol`, the same seam `net.rs` uses, so
//! this file cannot become the hardcoded-`v770` dependency in shell code that
//! `just check-seam` exists to prevent — and the packet id comes out of
//! `ServerDirective::Send` rather than being restated.
//!
//! **S1 + S2 ≈ S1+S2 is an internal control, and it is not a tautology**: S1's
//! arm drops each decoded column immediately (paying `free`) while S2's arm is
//! handed pre-built chunks (paying no decode), so the two decompositions share
//! no measurement. If they disagree by more than a few percent, the split is
//! wrong and the per-stage numbers should not be quoted. The harness asserts it.
//!
//! # Why one test function in one binary
//!
//! `ri_instructions` is **process-wide**, not per-thread. Two `#[test]`s in one
//! binary run concurrently by default, so a second test's work lands inside the
//! first's measurement window: a counter gate that shared a binary here once
//! read 502 against a true 256. Everything below is therefore a single test
//! function, and every measured stage is single-threaded — no `MeshScheduler`
//! and no worker pool, `mesh_snapshot_models` is called directly, which is the
//! same function the pool's workers call.
//!
//! # The instrument's own controls, and what each would catch
//!
//! `ri_instructions` sits 30 `u64`s deep in a 36-field struct behind a 16-byte
//! UUID. A hand-counted offset is exactly the class of error `CLAUDE.md` bans
//! for entity metadata indices, so the struct is declared field-by-field and the
//! field is reached **by name**. Three controls back that up, each failing on a
//! different wrong reading:
//!
//! | control | expected | what a wrong reading gives |
//! |---|---|---|
//! | `size_of::<RusageInfoV4>() == 304` | 16 + 36×8, from the field list | any added, dropped or mis-typed field |
//! | 4× kernel scaling ∈ [3.80, 4.20] | 4.00, the ratio of two loop bounds | ≈1.00 for a footprint or flags field |
//! | locality separation > 4× | ≈ the measured cycle ratio (~12 here) | its reciprocal (~0.08) if the two fields are read swapped |
//!
//! Both expected values are arithmetic. The scaling control's is a ratio of two
//! loop bounds chosen in this file. The locality control's is the observation
//! that the same compiled loop taking the same number of steps must retire the
//! same instructions whether its table is 4 KiB or 16 MiB, while its *cycles*
//! blow out — which is the separation no scaling test can make, because cycles
//! scale 4× too. An earlier `IPC > 1.0` control was **premise-false and fired on
//! correct code**; see [`assert_counters_are_real`] for the measurement and why
//! it was the dangerous direction of wrong.
//!
//! # What the fixture contains, asserted rather than hoped
//!
//! The *world* species of vacuous test cannot be found by reading the test — the
//! flaw lives in the input data. Seed 1234 chunk (0,0) is **ocean**, and a light
//! gate passed vacuously on it in this repo with 3,584 counted cells and none of
//! the intended structure. So this harness generates real columns through
//! `lodestone_server::overworld_chunk_source` (the generator singleplayer uses)
//! and **asserts the fixture's structure before measuring**: non-air cells,
//! sections with a genuinely indirect palette, and a distinct block-state count
//! above a floor. A single-value or all-air column short-circuits the palette
//! code, the mesher and the draw loop simultaneously, and every number below
//! would be small for a reason unrelated to cost.
//!
//! # Evidence caveat on S1
//!
//! The packet bytes come from our own `ServerProtocol::encode_chunk` — the
//! production singleplayer encoder, but still ours. Per `CLAUDE.md` that is weak
//! evidence for *correctness* (a symmetric misunderstanding round-trips fine),
//! and it is called out for that reason. It does not weaken the *cost*
//! measurement: decode cost is driven by section count, bits-per-entry, palette
//! size and light-array presence, all of which come from the real generator, not
//! from the framing. There is no captured vanilla chunk payload in this repo, and
//! no JVM or Docker available this session to make one.
//!
//! ```text
//! cargo test -p lodestone-shell --release --test client_chunk_cycles -- --ignored --nocapture
//! ```
//!
//! Release is not optional: a debug-profile instruction count measures `rustc`'s
//! unoptimised codegen, not the shipped client.
#![allow(unsafe_code)]

use std::hint::black_box;

use lodestone::mesher::{SectionKey, mesh_snapshot_fluids, mesh_snapshot_models, snapshot_section};
use lodestone_client::ConnectionState;
use lodestone_core::Nbt;
use lodestone_render::BlockModels;
use lodestone_world::{
    BiomePatch, BlockEntitySync, ChunkPos, ColumnPatch, LightPatch, LoadedChunk, World, WorldSink,
};

// ---------------------------------------------------------------------------
// The instrument
// ---------------------------------------------------------------------------

/// `RUSAGE_INFO_V4` from `<sys/resource.h>`.
const RUSAGE_INFO_V4: i32 = 4;

/// `struct rusage_info_v4` from macOS `<sys/resource.h>`, transcribed
/// **field-by-field in declaration order** so `ri_instructions` is reached by
/// name and no offset is hand-computed. `size_of` is asserted against
/// `16 + 36 * 8` at run start; see the module docs for why that check plus the
/// 4×-scaling and IPC controls are the three things standing between this file
/// and a plausible-looking wrong number.
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
/// list, not measured, so a dropped or duplicated line fails loudly.
const RUSAGE_INFO_V4_SIZE: usize = 16 + 36 * 8;

unsafe extern "C" {
    /// `libproc`'s task-level resource accounting, in `libSystem` — no link flag
    /// and no privileges needed for the calling process.
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
}

/// One reading of the process's retired-instruction and cycle counters.
#[derive(Clone, Copy, Debug, Default)]
struct Counters {
    instructions: u64,
    cycles: u64,
}

impl Counters {
    /// Reads the counters now. Costs ~600 ns, which is why nothing below reads
    /// them per block or per quad.
    ///
    /// Never returns zeros silently: a zero delta would satisfy every check
    /// below while measuring nothing (the Intel / Rosetta case), so
    /// [`assert_counters_are_real`] runs before any stage.
    fn read() -> Self {
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
            "proc_pid_rusage(RUSAGE_INFO_V4) failed with {rc}; this harness measures \
             instructions retired and has nothing to report without it"
        );
        Self {
            instructions: info.ri_instructions,
            cycles: info.ri_cycles,
        }
    }
}

/// Instructions and cycles retired between two readings.
#[derive(Clone, Copy, Debug)]
struct Delta {
    instructions: u64,
    cycles: u64,
}

impl Delta {
    fn ipc(self) -> f64 {
        if self.cycles == 0 {
            0.0
        } else {
            self.instructions as f64 / self.cycles as f64
        }
    }
}

/// Runs `f` once and returns what it retired.
fn measure<T>(f: impl FnOnce() -> T) -> (Delta, T) {
    let before = Counters::read();
    let out = f();
    let after = Counters::read();
    (
        Delta {
            instructions: after.instructions.saturating_sub(before.instructions),
            cycles: after.cycles.saturating_sub(before.cycles),
        },
        out,
    )
}

/// Runs `f` `reps` times, returning the **median** delta by instruction count.
///
/// Median rather than mean: an interrupt or a page fault landing in one
/// repetition adds instructions to that repetition only, and the median is
/// insensitive to it where a mean is not.
fn measure_median(reps: usize, mut f: impl FnMut()) -> Delta {
    assert!(reps > 0, "need at least one repetition");
    let mut samples: Vec<Delta> = (0..reps).map(|_| measure(&mut f).0).collect();
    samples.sort_by_key(|d| d.instructions);
    samples[reps / 2]
}

/// The reference workload: a fixed-iteration SplitMix64 chain. Deterministic,
/// integer-only, no allocation, `#[inline(never)]` so the optimiser cannot hoist
/// it out of its caller or fold two calls with different `iters` into one.
///
/// Its job is not to be representative of anything — it is the instrument's own
/// scaling control.
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

/// A pointer-chase over `table`, whose entries form a single permutation cycle.
/// `iters` steps of the cycle, so the **instruction count depends only on
/// `iters`** while the **cycle count depends on where `table` lives** — L1 or
/// DRAM. That separation is the whole thesis of measuring instructions, and it
/// is what the locality control below uses to tell `ri_instructions` from
/// `ri_cycles`.
///
/// `#[inline(never)]` so the two arms share one codegen of the loop; a
/// specialised copy per call site would break the "same instruction count"
/// premise the control rests on.
#[inline(never)]
fn chase(table: &[u32], iters: u64) -> u32 {
    let mut i: u32 = 0;
    for _ in 0..iters {
        i = table[i as usize % table.len()];
    }
    i
}

/// Builds a `len`-entry single-cycle permutation (Sattolo's algorithm) so a walk
/// visits every slot before repeating and the hardware prefetcher cannot predict
/// the next address. Deterministic in `len`.
///
/// # Panics
/// Panics if `len < 2`.
fn permutation_cycle(len: usize) -> Vec<u32> {
    assert!(len >= 2, "a cycle needs at least two slots");
    let mut order: Vec<u32> = (0..len as u32).collect();
    // Sattolo: swap each i with a strictly-lower index, producing one n-cycle.
    let mut rng: u64 = 0x243F_6A88_85A3_08D3;
    for i in (1..len).rev() {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let j = (rng % i as u64) as usize;
        order.swap(i, j);
    }
    // `table[a] = b` means "after a, go to b" along the cycle.
    let mut table = vec![0u32; len];
    for w in 0..len {
        table[order[w] as usize] = order[(w + 1) % len];
    }
    table
}

/// L1-resident arm of the locality control: 1024 × `u32` = 4 KiB.
const CHASE_SMALL: usize = 1 << 10;

/// DRAM-resident arm: 4 Mi × `u32` = 16 MiB, far beyond this machine's caches.
const CHASE_LARGE: usize = 1 << 22;

/// Steps taken by each arm of the locality control. Identical in both arms —
/// that identity *is* the outside expectation.
const CHASE_ITERS: u64 = 1_500_000;

/// Iteration count for the 1× arm of the scaling control.
const KERNEL_ITERS: u64 = 2_000_000;

/// The multiplier the scaling control expects to recover. The outside expected
/// value: a ratio of two loop bounds chosen here, which nothing the counter
/// reports can influence.
const KERNEL_SCALE: u64 = 4;

/// Proves the counter is a real instruction counter before anything is measured
/// with it, and prints the evidence.
///
/// Three failures this catches that a `> 0` check would not:
/// * **wrong field** — a footprint or flags field is roughly constant, so its
///   1×-versus-4× ratio is ≈1.0, not 4.0 (the *scaling* control);
/// * **swapped fields** — `ri_cycles` scales 4× too, so no scaling test can
///   separate them; the *locality* control does, because the same program over a
///   4 KiB table and over a 16 MiB table executes the same instructions and
///   burns wildly different numbers of cycles;
/// * **zeros** — Intel or Rosetta populates neither field, and a zero delta
///   would pass every equality check below while measuring nothing.
///
/// # An earlier control here was premise-false, and this is the record
///
/// The first version of the swapped-field control asserted `IPC > 1.0` on the
/// SplitMix64 reference kernel, reasoning that "a wide out-of-order core retires
/// more than one integer op per cycle". It **failed on correct code**, measuring
/// IPC 0.643 (18,196,013 instructions / 28,285,041 cycles) — because
/// [`reference_kernel`] is a *serially dependent* chain: two 64-bit multiplies
/// per iteration, each feeding the next, so it is latency-bound at ~14 cycles
/// for ~9.1 instructions. Low IPC is the *correct* reading for that kernel. The
/// premise was wrong, not the counter, and the direction of the error was the
/// dangerous one: a `< 1.0` assertion would have "passed" on a swapped read.
/// The locality control replaces it precisely because its expectation comes from
/// arithmetic — two arms take the same number of steps through the same compiled
/// loop — and not from any belief about this core's width. (DESIGN.md §12.118.)
fn assert_counters_are_real() {
    assert_eq!(
        size_of::<RusageInfoV4>(),
        RUSAGE_INFO_V4_SIZE,
        "rusage_info_v4 transcription is {} bytes, not the {RUSAGE_INFO_V4_SIZE} its 16-byte \
         UUID plus 36 u64 fields require — a field was dropped, duplicated or mis-typed, and \
         every offset after it (including ri_instructions) is wrong",
        size_of::<RusageInfoV4>()
    );

    // Warm the code path so the 1x arm is not paying a first-touch cost the 4x
    // arm does not.
    black_box(reference_kernel(KERNEL_ITERS / 10));

    let one = measure_median(5, || {
        black_box(reference_kernel(black_box(KERNEL_ITERS)));
    });
    let four = measure_median(5, || {
        black_box(reference_kernel(black_box(KERNEL_ITERS * KERNEL_SCALE)));
    });

    assert!(
        one.instructions > 0 && four.instructions > 0,
        "ri_instructions read zero ({} / {}). This platform does not populate the field \
         (Intel, or Rosetta): every stage below would report a zero delta and the harness \
         would pass while measuring nothing. macOS on Apple Silicon is required.",
        one.instructions,
        four.instructions
    );

    let scale = four.instructions as f64 / one.instructions as f64;
    println!("--- instrument controls ---");
    println!(
        "rusage_info_v4 size          {} bytes (required {RUSAGE_INFO_V4_SIZE})",
        size_of::<RusageInfoV4>()
    );
    println!(
        "kernel {KERNEL_ITERS} iters      {:>14} instructions {:>14} cycles  IPC {:.2}",
        one.instructions,
        one.cycles,
        one.ipc()
    );
    println!(
        "kernel x{KERNEL_SCALE}                   {:>14} instructions {:>14} cycles  IPC {:.2}",
        four.instructions,
        four.cycles,
        four.ipc()
    );
    println!(
        "scaling                      {scale:.4}x   correct hypothesis {KERNEL_SCALE}.0, \
         wrong-field hypothesis 1.0"
    );
    println!(
        "instructions per iteration   {:.2}",
        one.instructions as f64 / KERNEL_ITERS as f64
    );

    assert!(
        (3.80..=4.20).contains(&scale),
        "the reference kernel run {KERNEL_SCALE}x longer retired {scale:.4}x the instructions, \
         not ~{KERNEL_SCALE}.0. The correct hypothesis is {KERNEL_SCALE}.0 (the ratio of the two \
         loop bounds, chosen in this file); the wrong-field hypothesis is 1.0 (a footprint or \
         flags field, roughly constant). The measurement lands on neither, so the field being \
         read is not instructions retired."
    );
    // -- the locality control: same program, two memory footprints ----------
    // Instructions are a property of the program; cycles are a property of the
    // machine running it. `chase` takes CHASE_ITERS steps through one compiled
    // loop in both arms, so the instruction counts must match; the DRAM arm
    // stalls on every load, so the cycle counts must not. Reading the two fields
    // swapped inverts exactly this, and no scaling test can see it.
    let small = permutation_cycle(CHASE_SMALL);
    let large = permutation_cycle(CHASE_LARGE);
    // Fault every page of both tables in before measuring. Without this the cold
    // arm pays soft page faults, whose *kernel* instructions count toward this
    // process and inflate its instruction reading — measured at 1.17x before
    // this touch was added, which is why the assertion below is a ratio of
    // ratios rather than a tight band on the instruction ratio alone.
    black_box(small.iter().fold(0u64, |a, &v| a + u64::from(v)));
    black_box(large.iter().fold(0u64, |a, &v| a + u64::from(v)));
    black_box(chase(&small, CHASE_ITERS / 10));
    black_box(chase(&large, CHASE_ITERS / 10));
    let hot = measure_median(5, || {
        black_box(chase(black_box(&small), black_box(CHASE_ITERS)));
    });
    let cold = measure_median(5, || {
        black_box(chase(black_box(&large), black_box(CHASE_ITERS)));
    });
    let insn_ratio = cold.instructions as f64 / hot.instructions as f64;
    let cycle_ratio = cold.cycles as f64 / hot.cycles as f64;
    println!(
        "chase 4 KiB   {CHASE_ITERS} steps  {:>14} instructions {:>14} cycles  IPC {:.2}",
        hot.instructions,
        hot.cycles,
        hot.ipc()
    );
    println!(
        "chase 16 MiB  {CHASE_ITERS} steps  {:>14} instructions {:>14} cycles  IPC {:.2}",
        cold.instructions,
        cold.cycles,
        cold.ipc()
    );
    let separation = cycle_ratio / insn_ratio;
    println!(
        "locality      instruction ratio {insn_ratio:.4}, cycle ratio {cycle_ratio:.2}, \
         separation {separation:.2}x"
    );

    // The premise check comes first: if the large table is not actually missing
    // cache, the control fires on nothing and proves nothing about which field is
    // which. This is the step that catches a premise-false control, and it is
    // separate from the discriminating assertion below.
    assert!(
        cycle_ratio > 2.0,
        "PREMISE FALSE: the 16 MiB arm burned only {cycle_ratio:.2}x the cycles of the 4 KiB \
         arm, so the walk is not memory-bound (it became predictable, or the buffer was \
         elided) and the locality control below is measuring nothing. hot {} cycles, cold {} \
         cycles.",
        hot.cycles,
        cold.cycles
    );

    // The discriminator, with both hypotheses computed from the two measured
    // ratios rather than restated. Correct reading: the same compiled loop takes
    // the same number of steps in both arms, so instructions barely move while
    // cycles blow out — separation equals roughly the cycle ratio itself
    // (~13 on this machine). Swapped reading: the two ratios trade places, so
    // separation becomes its own reciprocal (~0.08). A threshold of 4.0 sits two
    // orders of magnitude from the wrong hypothesis and well below the right one.
    //
    // The instruction ratio is *not* asserted to be 1.00: page-fault and TLB
    // handling in the kernel retires instructions that count toward this process,
    // measured at 1.17x before the pre-fault touch above. That residual is small
    // next to 13 and cannot flip this verdict, which is exactly why the
    // assertion is on the separation and not on the tight band.
    assert!(
        separation > 4.0,
        "locality separation is {separation:.3}x (cycle ratio {cycle_ratio:.2} / instruction \
         ratio {insn_ratio:.4}). The correct hypothesis is ~{cycle_ratio:.1} — the same \
         compiled loop taking the same {CHASE_ITERS} steps retires the same instructions while \
         the DRAM arm stalls. The swapped-field hypothesis is its reciprocal, ~{:.3}. The \
         measurement lands on neither, so the two fields are not instructions and cycles.",
        1.0 / separation
    );
    println!("all three instrument controls pass.\n");
}

// ---------------------------------------------------------------------------
// A sink that discards, so decode can be measured without the insert
// ---------------------------------------------------------------------------

/// A [`WorldSink`] that counts what it is handed and drops it.
///
/// This is the S1 arm: `handle_packet` does the full production decode and
/// builds a real [`LoadedChunk`], and this sink drops it instead of inserting
/// it. So S1 measures decode plus construction plus `free`, and S2 (measured
/// separately over pre-cloned chunks) measures the insert with no decode. The
/// two share no measurement, which is what makes their sum a real control
/// against `handle_packet` into a live `World`.
#[derive(Default)]
struct DiscardSink {
    loads: usize,
}

impl WorldSink for DiscardSink {
    fn load(&mut self, _pos: ChunkPos, chunk: LoadedChunk) {
        self.loads += 1;
        drop(chunk);
    }
    fn merge(&mut self, _pos: ChunkPos, _patch: ColumnPatch) {}
    fn set_block(&mut self, _x: i32, _y: i32, _z: i32, _state: u32) {}
    fn set_blocks(
        &mut self,
        _section_x: i32,
        _section_y: i32,
        _section_z: i32,
        _blocks: &[(u8, u8, u8, u32)],
    ) {
    }
    fn set_block_entity(&mut self, _x: i32, _y: i32, _z: i32, _type_id: u32, _nbt: Nbt) {}
    fn sync_block_entity(
        &mut self,
        _x: i32,
        _y: i32,
        _z: i32,
        _block_entity_type: Option<u32>,
    ) -> BlockEntitySync {
        BlockEntitySync::ChunkAbsent
    }
    fn merge_light(&mut self, _pos: ChunkPos, _patch: LightPatch) {}
    fn merge_biomes(&mut self, _pos: ChunkPos, _patch: BiomePatch) {}
    fn unload(&mut self, _pos: ChunkPos) {}
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// Seed for the generated terrain. Chosen for what the columns *contain* — the
/// fixture guards below reject a degenerate run, which is the protection against
/// the ocean-column trap (`CLAUDE.md`: seed 1234 chunk (0,0) is ocean and a
/// light gate passed vacuously on it with 3,584 counted cells).
const SEED: i64 = 0x4C4F_4445;

/// How many real columns to generate, as a radius. Real worldgen costs ~100 ms
/// per column, so this is the smallest radius that still gives the mesher a
/// *complete* neighbourhood: `snapshot_section` reads all 26 neighbours, so a
/// 3×3 block of columns is the minimum producing one centre column with no
/// unloaded neighbour, and radius 2 leaves a 3×3 interior.
const FIXTURE_RADIUS: i32 = 2;

/// Distinct block-state ids a real overworld column must contain before this
/// harness believes its fixture. Well below what real terrain produces (stone,
/// deepslate, dirt, grass, water, gravel, sand, ores, air, …) and well above
/// what an all-air or single-biome ocean column gives, so it separates the two
/// without encoding today's exact generator output.
const MIN_DISTINCT_STATES: usize = 8;

/// What [`measure_draw_submission`] found.
struct SubmitCost {
    /// Instructions for a frame with no terrain resident: sky, clears, depth,
    /// encoder setup, queue submit. The term culling cannot remove.
    fixed: Delta,
    /// Instructions for the same frame with every fixture section resident.
    loaded: Delta,
    /// Sections the loaded frame actually submitted, from `RenderStats`.
    sections_drawn: usize,
    /// Draw calls the loaded frame issued, from `RenderStats`.
    draw_calls: usize,
}

/// Renders one frame with no terrain, then the same frame with every section of
/// every fixture column, through the **real** `RenderState::render` — the call
/// the live frame loop makes.
///
/// The marginal per-section cost is `(loaded - fixed) / sections_drawn`. Taking a
/// difference is what makes this honest on a process-wide counter: the Metal
/// driver's own threads retire instructions inside both windows, and the fixed
/// share of that cancels, leaving the per-draw share — which is the cost being
/// asked about, so including it is correct rather than a confound.
///
/// Both arms use the same `RenderState`, the same `HeadlessTarget` and the same
/// camera, in that order, so nothing differs but section residency. Warm-up
/// frames run before the fixed arm; without them the fixed arm would absorb
/// first-frame pipeline compilation and the marginal cost would come out
/// negative.
fn measure_draw_submission(
    world: &World,
    atlas: &lodestone_render::BlockAtlas,
    models: &BlockModels,
    min_y: i32,
    column_sections: usize,
) -> SubmitCost {
    use lodestone::gpu::RenderState;
    use lodestone::mesher::SectionGeometry;
    use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};

    let ctx = GpuContext::new_headless_blocking().expect(
        "this measurement opted in via --ignored but no wgpu adapter is available; run on a \
         host with a GPU — a silent skip here would drop the draw-submission stage, which is \
         the one the render plan is sequenced around",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let (w, h) = (854u32, 480u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let mut state = RenderState::new(device, queue, format, w, h, Some(atlas));

    let camera = Camera {
        position: glam::Vec3::new(8.0, 96.0, 8.0),
        yaw: 45.0,
        pitch: 15.0,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };
    let mut one_frame = |state: &RenderState| {
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[])
    };

    // Frames per measurement window, and windows per arm. A *single* frame is not
    // measurable here: wgpu submits to Metal and the driver's own threads retire
    // instructions asynchronously, so a one-frame window catches a random slice
    // of the previous frame's driver work. Batching frames per window averages
    // that; the median over windows removes an interrupt landing in one.
    //
    // This is not a cosmetic change. The first version of this measurement took
    // one frame per arm after 4 warm-up frames and reported the loaded frame as
    // *cheaper* than the empty one (4,288,471 against 7,958,869 instructions) —
    // a negative marginal cost, silently clamped to 0 by a `saturating_sub`.
    // Lazy Metal pipeline compilation had not settled by frame 4, so the "fixed"
    // arm absorbed one-off work. Hence 40 warm-up frames, and hence the marginal
    // cost is asserted **positive** rather than saturated. (DESIGN.md §12.118.)
    const FRAMES_PER_WINDOW: usize = 10;
    const WINDOWS: usize = 5;
    const WARMUP_FRAMES: usize = 40;

    let mut arm = |state: &RenderState, one_frame: &mut dyn FnMut(&RenderState) -> _| {
        for _ in 0..WARMUP_FRAMES {
            black_box(one_frame(state));
        }
        let mut windows: Vec<Delta> = Vec::with_capacity(WINDOWS);
        let mut last = None;
        for _ in 0..WINDOWS {
            let (d, stats) = measure(|| {
                let mut s = None;
                for _ in 0..FRAMES_PER_WINDOW {
                    s = Some(one_frame(state));
                }
                s.expect("at least one frame per window")
            });
            windows.push(Delta {
                instructions: d.instructions / FRAMES_PER_WINDOW as u64,
                cycles: d.cycles / FRAMES_PER_WINDOW as u64,
            });
            last = Some(stats);
        }
        windows.sort_by_key(|d| d.instructions);
        (windows[WINDOWS / 2], last.expect("one window"))
    };

    let (fixed, empty_stats) = arm(&state, &mut one_frame);
    assert_eq!(
        empty_stats.sections_drawn, 0,
        "the no-terrain arm drew {} sections, so it is not the fixed-cost baseline this \
         difference assumes",
        empty_stats.sections_drawn
    );

    // Upload every section of every fixture column, outside both windows.
    let mut uploaded = 0usize;
    for (pos, _) in world.iter() {
        for si in 0..column_sections {
            let key = SectionKey {
                cx: pos.x,
                cz: pos.z,
                si,
                min_y,
            };
            let Some(snap) = snapshot_section(world, key) else {
                continue;
            };
            let opaque = mesh_snapshot_models(&snap, models);
            let water = mesh_snapshot_fluids(&snap, models).water;
            if opaque.quad_count() == 0 && water.quad_count() == 0 {
                continue;
            }
            state.upload_section(
                device,
                queue,
                key,
                &SectionGeometry::Model { opaque, water },
            );
            uploaded += 1;
        }
    }
    assert!(
        uploaded > 0,
        "no section uploaded, so the loaded arm equals the fixed arm and the marginal cost \
         would read zero for a reason unrelated to submission cost"
    );

    let (loaded, stats) = arm(&state, &mut one_frame);
    // `sections_drawn` is incremented only by the **opaque** loop
    // (`frame.rs:480`), and a water-only section — an ocean surface with no solid
    // blocks — carries `mesh: None` there while still issuing a water draw at
    // `frame.rs:720`. So `sections_drawn` legitimately undercounts uploads by the
    // water-only share; measured at 189 of 195 on this fixture. The floor is
    // therefore a fraction, and `draw_calls` (which counts both passes) is
    // reported beside it — that gap *is* the render plan's "water sections pay a
    // second set of encoder calls".
    assert!(
        stats.sections_drawn * 10 >= uploaded * 9,
        "uploaded {uploaded} sections but the opaque pass drew only {}, below the 90% floor. \
         Water-only sections explain a few percent; a larger gap means the draw loop is not \
         iterating what this measurement thinks it is.",
        stats.sections_drawn
    );
    assert!(
        stats.draw_calls > stats.sections_drawn,
        "draw_calls ({}) did not exceed sections_drawn ({}), so this fixture has no water \
         sections and the second-draw cost the render plan's U4 targets is unexercised here",
        stats.draw_calls,
        stats.sections_drawn
    );
    // A negative marginal cost is a broken measurement, not a fast renderer. The
    // first version of this function produced exactly that and clamped it to zero.
    assert!(
        loaded.instructions > fixed.instructions,
        "the frame with {} sections retired FEWER instructions ({}) than the frame with none \
         ({}). A negative marginal cost is impossible: something one-off is still landing in \
         the no-terrain arm despite {WARMUP_FRAMES} warm-up frames, or the driver's async work \
         is not settling within {FRAMES_PER_WINDOW}-frame windows. Do not clamp this to zero — \
         raise WARMUP_FRAMES or FRAMES_PER_WINDOW until it is positive.",
        stats.sections_drawn,
        loaded.instructions,
        fixed.instructions
    );

    SubmitCost {
        fixed,
        loaded,
        sections_drawn: stats.sections_drawn,
        draw_calls: stats.draw_calls,
    }
}

#[test]
#[ignore = "measurement: real worldgen plus the real client.jar model bake"]
fn client_chunk_path_cycle_attribution() {
    assert_counters_are_real();

    // -- fixture: real terrain through the real generator -------------------
    let protocol = lodestone::Config::default().protocol;
    let server_protocol = lodestone_registry::server_protocol_for_protocol(protocol)
        .expect("the default build hosts singleplayer, so a ServerProtocol exists");
    let adapter = lodestone_registry::adapter_for_protocol(protocol)
        .expect("the default `live` feature compiles a client family for this protocol");
    let source = lodestone_server::overworld_chunk_source(SEED);

    println!("--- fixture: generating real columns (seed {SEED:#x}) ---");
    let mut payloads: Vec<(i32, i32, i32, Vec<u8>)> = Vec::new();
    for cz in -FIXTURE_RADIUS..=FIXTURE_RADIUS {
        for cx in -FIXTURE_RADIUS..=FIXTURE_RADIUS {
            let column = lodestone_server::ChunkSource::column(&source, cx, cz);
            // The packet id comes out of the encoder rather than being restated
            // here: a hardcoded id would be both a version leak and a silent
            // wrong-arm hazard if the id ever moved.
            match server_protocol.encode_chunk(cx, cz, &column) {
                lodestone_server::ServerDirective::Send { packet_id, payload } => {
                    payloads.push((cx, cz, packet_id, payload));
                }
                other => panic!("encode_chunk must produce a Send directive, got {other:?}"),
            }
        }
    }
    let expected_columns = ((2 * FIXTURE_RADIUS + 1) as usize).pow(2);
    assert_eq!(
        payloads.len(),
        expected_columns,
        "generated {} columns, expected {expected_columns}",
        payloads.len()
    );

    let (_, _, chunk_packet_id, centre) = payloads
        .iter()
        .find(|(cx, cz, _, _)| *cx == 0 && *cz == 0)
        .cloned()
        .expect("centre column present");
    let total_bytes: usize = payloads.iter().map(|(_, _, _, p)| p.len()).sum();

    // -- build the real resident world (production path) --------------------
    let mut world = World::new();
    for (_, _, id, payload) in &payloads {
        adapter
            .handle_packet(&mut world, ConnectionState::Play, *id, payload)
            .expect("the real adapter decodes our own encoder's chunk packet");
    }
    let resident_columns = world.len();
    assert_eq!(
        resident_columns, expected_columns,
        "only {resident_columns} of {expected_columns} columns reached the World — the adapter \
         arm did not run, and every stage below is measuring the wrong thing"
    );

    // -- census the fixture, and assert it is not degenerate ----------------
    let mut distinct_states = std::collections::HashSet::new();
    let mut non_air_cells = 0usize;
    let mut indirect_sections = 0usize;
    let mut single_value_sections = 0usize;
    let mut sections_with_geometry = 0usize;
    let centre_pos = ChunkPos::new(0, 0);
    let mut section_index = 0usize;
    while let Some(section) = world.section(centre_pos, section_index) {
        let blocks = section.block_states();
        if blocks.is_single() {
            single_value_sections += 1;
        } else {
            indirect_sections += 1;
        }
        let mut non_air = 0usize;
        for i in 0..blocks.entry_count() {
            let v = blocks.get(i);
            distinct_states.insert(v);
            if v != 0 {
                non_air += 1;
            }
        }
        if non_air > 0 {
            sections_with_geometry += 1;
        }
        non_air_cells += non_air;
        section_index += 1;
    }
    let column_sections = section_index;

    println!("columns generated            {expected_columns}");
    println!("packet bytes (all columns)   {total_bytes}");
    println!("centre packet bytes          {}", centre.len());
    println!("centre column sections       {column_sections}");
    println!("centre non-air cells         {non_air_cells}");
    println!("centre sections w/ geometry  {sections_with_geometry}");
    println!(
        "centre palette shape         {indirect_sections} indirect, {single_value_sections} \
         single-value"
    );
    println!("centre distinct states       {}", distinct_states.len());

    // The world-species guards. Each names what a failure means, because each
    // failure makes *every* stage below small for a reason unrelated to cost.
    assert!(
        column_sections > 0,
        "the centre column reports zero sections — the world census read nothing"
    );
    assert!(
        non_air_cells > 4096,
        "the centre column has only {non_air_cells} non-air cells, fewer than one full section. \
         This is the ocean-column trap: a near-empty column short-circuits the palette code, \
         the mesher and the draw loop at once, and every stage below would report a small \
         number for a reason that has nothing to do with cost. Change SEED."
    );
    assert!(
        distinct_states.len() >= MIN_DISTINCT_STATES,
        "the centre column contains only {} distinct block states (floor {MIN_DISTINCT_STATES}). \
         A low-variety column makes every PalettedContainer single-value or tiny-palette, so \
         the linear palette scan is never exercised.",
        distinct_states.len()
    );
    assert!(
        indirect_sections > 0,
        "no section in the centre column has an indirect palette — every one is single-value, \
         so PalettedContainer's palette scan is unreachable on this fixture"
    );
    assert!(
        sections_with_geometry > 0,
        "no section in the centre column has geometry, so the mesh stage has nothing to do"
    );
    println!("all five fixture guards pass.\n");

    // -- S1: decode, into a sink that discards ------------------------------
    let s1 = measure_median(9, || {
        let mut sink = DiscardSink::default();
        adapter
            .handle_packet(
                &mut sink,
                ConnectionState::Play,
                chunk_packet_id,
                black_box(&centre),
            )
            .expect("decode");
        assert_eq!(sink.loads, 1, "the chunk arm must have reached the sink");
        black_box(sink.loads);
    });

    // -- S2: World::load, over pre-cloned chunks ----------------------------
    // The real resident world already holds the decoded centre column, and
    // `LoadedChunk` is `Clone`, so the measured loop pays no decode at all —
    // this is the insert, isolated, with no subtraction.
    const S2_REPS: usize = 15;
    let owned_chunk = world
        .get(centre_pos)
        .expect("the centre column is resident")
        .clone();
    let clones: Vec<LoadedChunk> = (0..S2_REPS).map(|_| owned_chunk.clone()).collect();
    let s2 = {
        // One fresh `World` per insert, built *outside* the measured window, so
        // no sample pays for the previous occupant's drop or for a rehash the
        // others do not.
        let mut targets: Vec<World> = (0..S2_REPS).map(|_| World::new()).collect();
        let mut per_call: Vec<Delta> = Vec::with_capacity(S2_REPS);
        for (target, chunk) in targets.iter_mut().zip(clones) {
            let (d, _) = measure(|| target.load(centre_pos, black_box(chunk)));
            per_call.push(d);
        }
        black_box(&targets);
        per_call.sort_by_key(|d| d.instructions);
        per_call[per_call.len() / 2]
    };

    // -- the consistency control: S1 + S2 against the production whole ------
    let whole = measure_median(9, || {
        let mut target = World::new();
        adapter
            .handle_packet(
                &mut target,
                ConnectionState::Play,
                chunk_packet_id,
                black_box(&centre),
            )
            .expect("decode");
        black_box(&target);
    });
    let parts = s1.instructions + s2.instructions;
    let split_ratio = parts as f64 / whole.instructions as f64;

    // -- S3: snapshot + mesh ------------------------------------------------
    // One source for both the mesher and the GPU atlas: the same loader the real
    // client uses (`resources.rs`'s `try_vanilla`), so the mesh stage and the
    // draw stage cannot silently disagree about which pack they measured.
    let resources = lodestone::resources::BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "the vanilla pack did not load, so both the mesh and draw stages would measure a \
             different renderer than production runs. Set LODESTONE_ASSETS to a pack root with \
             client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    });
    let models: &BlockModels = atlas
        .models()
        .expect("the vanilla load must attach baked block models");
    // `SectionKey::min_y` must be the dimension's own `min_y` (mesher.rs:526);
    // read it off the resident column rather than restating -64, so a fixture at
    // a different build-height window keys its sections correctly instead of
    // silently snapshotting nothing.
    let min_y = world
        .get(centre_pos)
        .expect("centre column resident")
        .column
        .min_y();
    let centre_keys: Vec<SectionKey> = (0..column_sections)
        .map(|si| SectionKey {
            cx: 0,
            cz: 0,
            si,
            min_y,
        })
        .filter(|k| snapshot_section(&world, *k).is_some())
        .collect();
    assert!(
        !centre_keys.is_empty(),
        "no section of the centre column snapshots, so the mesh stage would measure nothing. \
         The {}x{} fixture should give the centre column a complete 26-neighbour set.",
        2 * FIXTURE_RADIUS + 1,
        2 * FIXTURE_RADIUS + 1
    );

    let s3_snapshot = measure_median(5, || {
        for key in &centre_keys {
            black_box(snapshot_section(&world, *key));
        }
    });
    let snapshots: Vec<_> = centre_keys
        .iter()
        .map(|k| snapshot_section(&world, *k).expect("already checked"))
        .collect();
    // Split, because they are separately fixable and each is a different loop
    // over the same 4096 cells: `mesh_models` emits block geometry,
    // `mesh_fluids` emits water/lava surfaces. Whichever dominates is where a
    // per-cell optimisation pays.
    let s3_models = measure_median(5, || {
        for snap in &snapshots {
            black_box(mesh_snapshot_models(snap, models));
        }
    });
    let s3_fluids = measure_median(5, || {
        for snap in &snapshots {
            black_box(mesh_snapshot_fluids(snap, models));
        }
    });
    let s3_mesh = Delta {
        instructions: s3_models.instructions + s3_fluids.instructions,
        cycles: s3_models.cycles + s3_fluids.cycles,
    };

    // Decomposing the fluid stage: `mesh_fluids` (`models.rs:1158`) scans all
    // 4096 cells and `continue`s on `fluid_at(..) == None`, so a section with no
    // fluid still pays the full scan. Splitting the snapshots by whether they
    // contain any fluid cell at all answers whether the cost is the fluid
    // geometry or the empty scan — and therefore whether a palette-level
    // "contains no fluid" precheck (O(palette_len) ≈ 10) can skip it entirely.
    let mut dry: Vec<&_> = Vec::new();
    let mut wet: Vec<&_> = Vec::new();
    let mut fluid_cells = 0usize;
    for (snap, key) in snapshots.iter().zip(&centre_keys) {
        let section = world
            .section(centre_pos, key.si)
            .expect("the key came from this column");
        let blocks = section.block_states();
        let n = (0..blocks.entry_count())
            .filter(|&i| models.fluid(blocks.get(i)).is_some())
            .count();
        fluid_cells += n;
        if n == 0 { dry.push(snap) } else { wet.push(snap) }
    }
    let s3_fluids_dry = measure_median(5, || {
        for snap in &dry {
            black_box(mesh_snapshot_fluids(snap, models));
        }
    });
    let s3_fluids_wet = measure_median(5, || {
        for snap in &wet {
            black_box(mesh_snapshot_fluids(snap, models));
        }
    });
    let meshed_quads: usize = snapshots
        .iter()
        .map(|s| mesh_snapshot_models(s, models).quad_count())
        .sum();

    // -- S4: draw submission, as a marginal cost per section ----------------
    // A difference, not an absolute: render the same frame with 0 sections
    // resident and then with every section of every fixture column, and divide
    // by the section count. The fixed per-frame cost (sky, clears, depth,
    // encoder setup, queue submit, driver bring-up) cancels, leaving the term
    // that scales with render distance — which is the whole question, since
    // `gpu/frame.rs:459/480/720` iterate every resident section with no frustum
    // and no distance cull.
    //
    // The same `RenderState` and the same target are used for both arms, in
    // that order, so the two readings differ in nothing but section residency.
    let s4 = measure_draw_submission(&world, atlas.as_ref(), models, min_y, column_sections);

    // -- the per-frame costs the owner's question is really about -----------
    // Not stages of the chunk path: what the client spends *every frame* on the
    // data the chunk path produced. Measured with the same instrument because
    // the comparison against the one-off per-column cost is the whole point.
    let heap_bytes_call = measure_median(9, || {
        black_box(world.heap_bytes());
    });
    let positions_vec = measure_median(9, || {
        black_box(world.iter().map(|(pos, _)| *pos).collect::<Vec<_>>());
    });

    // -- report -------------------------------------------------------------
    let sections = centre_keys.len();
    println!("--- per-stage attribution, ONE column, {sections} sections with geometry ---");
    println!(
        "{:<30} {:>14} {:>14} {:>6} {:>14}",
        "stage", "instructions", "cycles", "IPC", "per section"
    );
    let row = |name: &str, d: Delta, denom: f64| {
        println!(
            "{name:<30} {:>14} {:>14} {:>6.2} {:>14.0}",
            d.instructions,
            d.cycles,
            d.ipc(),
            d.instructions as f64 / denom
        );
    };
    row("S1 decode (discarding sink)", s1, sections as f64);
    row("S2 World::load", s2, sections as f64);
    row("S1+S2 production whole", whole, sections as f64);
    row("S3a snapshot_section", s3_snapshot, sections as f64);
    row("S3b1 mesh models", s3_models, sections as f64);
    row("S3b2 mesh fluids", s3_fluids, sections as f64);
    row("S3b  mesh total", s3_mesh, sections as f64);
    println!(
        "\n--- fluid-stage decomposition ({} fluid cells in {} sections of the centre column) ---",
        fluid_cells, sections
    );
    println!(
        "{:<30} {:>14} {:>14} {:>6} {:>14}",
        "arm", "instructions", "cycles", "IPC", "per section"
    );
    if !dry.is_empty() {
        row("sections with NO fluid", s3_fluids_dry, dry.len() as f64);
    }
    if !wet.is_empty() {
        row("sections WITH fluid", s3_fluids_wet, wet.len() as f64);
    }
    if fluid_cells > 0 {
        // The terrain-independent figure, printed rather than divided by hand:
        // the 58.8% share belongs to *this* water-bearing column, but the
        // per-fluid-cell cost is the number a fix is judged on. Cycles too,
        // because a locality change can move them without moving instructions
        // — see "Where instructions understate" in `docs/client-chunk-cycles.md`.
        let cells = fluid_cells as f64;
        println!(
            "per FLUID cell (wet arm)       {:>14.0} {:>14.0} {:>6.2}",
            s3_fluids_wet.instructions as f64 / cells,
            s3_fluids_wet.cycles as f64 / cells,
            s3_fluids_wet.instructions as f64 / s3_fluids_wet.cycles as f64
        );
    }
    println!(
        "dry sections {} of {sections}. The dry arm is the 4096-cell scan with every cell \n\
         empty. It is NOT free and it is NOT the target: issue #542 measured a palette-level \n\
         `contains no fluid` precheck at ~2% of the term. `FluidGrid::any_fluid` now gives \n\
         that precheck away as a by-product of the fill — but the fill itself is what makes \n\
         this arm move, so watch it when changing `cell_at` (DESIGN.md §12.124).",
        dry.len()
    );
    println!(
        "\nsplit control  S1+S2 = {parts}, whole = {}, ratio {split_ratio:.4} \
         (must be within 0.85..1.15)",
        whole.instructions
    );
    println!(
        "meshed quads (centre column) {meshed_quads}, {:.0} instructions per quad",
        s3_mesh.instructions as f64 / meshed_quads.max(1) as f64
    );
    let chunk_path_total = whole.instructions + s3_snapshot.instructions + s3_mesh.instructions;
    println!("\nONE-OFF cost of one column reaching meshed geometry: {chunk_path_total} instructions");
    println!(
        "  decode+insert {:>5.1}%   snapshot {:>5.1}%   mesh {:>5.1}%",
        100.0 * whole.instructions as f64 / chunk_path_total as f64,
        100.0 * s3_snapshot.instructions as f64 / chunk_path_total as f64,
        100.0 * s3_mesh.instructions as f64 / chunk_path_total as f64
    );

    // -- S4 report ----------------------------------------------------------
    // No `saturating_sub`: `measure_draw_submission` has already asserted the
    // difference is positive, so a clamp here would only hide a regression in
    // that assertion.
    let marginal = s4.loaded.instructions - s4.fixed.instructions;
    let per_section_draw = marginal as f64 / s4.sections_drawn as f64;
    println!("\n--- S4 draw submission, real RenderState::render, 854x480 ---");
    println!("frame with no terrain         {:>14} instructions (median of 5 x 10-frame windows)", s4.fixed.instructions);
    println!(
        "frame with {:>4} sections      {:>14} instructions ({} draw calls)",
        s4.sections_drawn, s4.loaded.instructions, s4.draw_calls
    );
    println!("marginal for those sections   {marginal:>14} instructions");
    println!("per section drawn             {per_section_draw:>14.0} instructions");
    // The extrapolation to the one recorded live figure. 931 sections / 441k
    // quads at default render distance is `45a93e4`'s commit message — the only
    // measured live section count in the record. Stated as an extrapolation from
    // the per-section rate above, not as a measurement.
    const RECORDED_LIVE_SECTIONS: u64 = 931;
    println!(
        "\nextrapolated to the recorded live frame ({RECORDED_LIVE_SECTIONS} sections, \
         45a93e4):"
    );
    println!(
        "  terrain submission  {:>12} instructions per frame",
        per_section_draw as u64 * RECORDED_LIVE_SECTIONS
    );
    println!(
        "  at 60 fps           {:>12} instructions per second",
        per_section_draw as u64 * RECORDED_LIVE_SECTIONS * 60
    );

    println!("\n--- PER-FRAME costs over {resident_columns} resident columns ---");
    row("World::heap_bytes (F3 field)", heap_bytes_call, resident_columns as f64);
    row("loaded-positions Vec (F3)", positions_vec, resident_columns as f64);
    // The headline comparison, scaled to a real session. Render distance 8
    // streams (2*(8+1)+1)^2 = 361 columns (`app/session.rs`'s view_radius =
    // render_distance + 1), so the per-frame terms below are what the shipped
    // default actually pays, extrapolated linearly from the measured per-column
    // rate — stated as an extrapolation, not a measurement.
    const RD8_COLUMNS: u64 = 361;
    let per_col_heap = heap_bytes_call.instructions / resident_columns as u64;
    let per_col_vec = positions_vec.instructions / resident_columns as u64;
    println!(
        "\nextrapolated to {RD8_COLUMNS} resident columns (render distance 8, view_radius 9):"
    );
    println!(
        "  heap_bytes          {:>12} instructions per frame",
        per_col_heap * RD8_COLUMNS
    );
    println!(
        "  loaded-positions Vec{:>12} instructions per frame",
        per_col_vec * RD8_COLUMNS
    );
    println!(
        "  at 60 fps that is   {:>12} instructions per second on an F3 field",
        (per_col_heap + per_col_vec) * RD8_COLUMNS * 60
    );

    // -- non-vacuity floors -------------------------------------------------
    // An early-returned or short-circuited stage reads near-zero instructions.
    // Each floor is derived from the fixture's structure, not from a previous
    // run: decoding or meshing N sections of 4096 cells cannot cost fewer
    // instructions than there are cells.
    let cells = (sections * 4096) as u64;
    assert!(
        s1.instructions > cells,
        "S1 decode retired {} instructions for {cells} block cells (< 1 per cell) — the decode \
         short-circuited rather than parsing the sections",
        s1.instructions
    );
    assert!(
        s3_mesh.instructions > cells,
        "S3b mesh retired {} instructions for {cells} block cells (< 1 per cell) — the mesher \
         returned early rather than emitting geometry",
        s3_mesh.instructions
    );
    assert!(
        s2.instructions > 0 && s3_snapshot.instructions > 0,
        "S2 ({}) or S3a ({}) measured no work at all",
        s2.instructions,
        s3_snapshot.instructions
    );
    assert!(
        heap_bytes_call.instructions > resident_columns as u64,
        "World::heap_bytes retired {} instructions over {resident_columns} columns — it cannot \
         have walked them, so the per-frame cost reported above is not the real one",
        heap_bytes_call.instructions
    );
    // The split control. Not a tautology: S1's arm pays `free` and no insert,
    // S2's arm pays an insert and no decode, and neither shares a measurement
    // with `whole`. A ratio outside the band means the decomposition is wrong
    // and the per-stage numbers must not be quoted.
    assert!(
        (0.85..=1.15).contains(&split_ratio),
        "S1 + S2 = {parts} instructions but the production whole measured {} — ratio \
         {split_ratio:.4}, outside 0.85..1.15. The per-stage split does not account for the \
         real path, so no number above should be quoted.",
        whole.instructions
    );
}
