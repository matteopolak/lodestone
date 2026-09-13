//! Where an explosion's instructions actually go — the measured profile behind
//! `docs/explosion-performance.md`.
//!
//! # What it is
//!
//! Three instruction-retired measurements over the real `explosion_blocks` code
//! (a blast in stone, a blast in open air, and an eight-blast cannon in one
//! tick), plus three isolating micro-measurements of the ray march's innermost
//! operations, so the follow-up optimisation work is dispatchable against numbers
//! rather than intuition.
//!
//! Nothing here asserts a *cost*: the assertions are structural (the instrument
//! moved, and the arms are ordered as the arithmetic requires), and the numbers
//! are printed. A cost threshold on a shared machine would be a flake.
//!
//! `#[ignore]`d, because it is a measurement rather than a gate and only means
//! anything in a release build on an otherwise idle machine:
//!
//! ```text
//! cargo test --release -p lodestone-server --test explosion_cost_profile \
//!     -- --ignored --nocapture
//! ```
//!
//! # Why instructions retired rather than wall clock
//!
//! This host reproduces wall clock to 11–19% at best with sibling agents
//! compiling, and instructions retired to 0.16–0.21%. Every conclusion in the
//! performance doc is a *ratio between stages*, which a noisy denominator
//! destroys. `proc_pid_rusage(RUSAGE_INFO_V4)` is the same instrument
//! `join_parallel_efficiency.rs` and the worldgen generation bench already use.
//!
//! # The two predictions this was written to test
//!
//! Both stated before the first run, per the repo's own rule that a unit cost
//! derived from someone else's aggregate has been wrong by 2× and 40×:
//!
//! 1. The flat `StateId`-indexed resistance lookup is **a small minority** of the
//!    per-step cost — it is one bounds-checked index into 65 KB of rodata.
//! 2. `ChunkSource::block_state` (which allocates a `String` per cell) plus
//!    `block_states::state_id` (which parses that string back to an id) together
//!    **dominate** it. If that holds, the section-level dense cache described in
//!    the performance doc is the whole optimisation and everything else is noise.
//!
//! See `docs/explosion-performance.md` for what the run actually said.

// `proc_pid_rusage` is an `extern "C"` call and the workspace denies unsafe code.
// Scoped as narrowly as the lint allows: cargo compiles each integration test as
// its own crate, so this cannot leak into the library. Same opt-out, for the same
// reason and against the same function, as `join_parallel_efficiency.rs`.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::sync::Mutex;

use lodestone_data::{block_blast, block_states};
use lodestone_model::Vec3;
use lodestone_server::explosion_blocks::{self, BlastEnv, RAY_COUNT};
use lodestone_server::{ChunkColumn, ChunkSource, SpawnRng};

// Darwin-only from here to `instructions_now`. `proc_pid_rusage` lives in
// `libSystem` and has no equivalent symbol on Linux or Windows, so an ungated
// `extern "C"` declaration of it links fine on macOS and fails the *link* — not
// the compile — everywhere else. `cargo check` never links, which is why this
// was invisible to every `check` job and only ever surfaced as
// `rust-lld: error: undefined symbol: proc_pid_rusage` in `cargo test`.
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

/// Non-Darwin arm. Panics rather than returning a plausible zero: a counter that
/// silently reads 0 would satisfy every comparison below while measuring nothing,
/// which is the shape of vacuous green this repo's evidence rules exist to
/// forbid. The only test that calls this is `#[ignore]`d, so nothing on Linux or
/// Windows reaches it — running it explicitly with `--ignored` is what should say
/// so, loudly, instead of reporting a fabricated measurement.
#[cfg(not(target_os = "macos"))]
fn instructions_now() -> u64 {
    unimplemented!(
        "instructions retired is read through proc_pid_rusage(RUSAGE_INFO_V4), which exists \
         only on Darwin; this profile is a macOS-only measurement and has no counter to \
         report on this target"
    )
}

#[cfg(target_os = "macos")]
fn instructions_now() -> u64 {
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

/// Runs `body` and returns instructions retired.
fn measure(body: impl FnOnce()) -> u64 {
    let before = instructions_now();
    body();
    instructions_now().saturating_sub(before)
}

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// The production column behind a `Mutex`, so `block_state` pays the real
/// `String` allocation and the real bit-packed section read rather than a test
/// shortcut. Anything cheaper here would understate the world-read share, which is
/// the number the whole profile turns on.
struct Rig {
    columns: Mutex<HashMap<(i32, i32), ChunkColumn>>,
    fill: &'static str,
    floor_y: Option<i32>,
}

impl Rig {
    fn new(fill: &'static str, floor_y: Option<i32>) -> Self {
        Self {
            columns: Mutex::new(HashMap::new()),
            fill,
            floor_y,
        }
    }

    fn fresh_column(&self) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        if let Some(floor_y) = self.floor_y {
            for y in MIN_Y..=floor_y {
                for lz in 0..16 {
                    for lx in 0..16 {
                        column.set_block(lx, y, lz, self.fill);
                    }
                }
            }
        }
        column
    }
}

impl ChunkSource for Rig {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut columns = self.columns.lock().expect("rig lock");
        columns
            .entry((cx, cz))
            .or_insert_with(|| self.fresh_column())
            .clone()
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let mut columns = self.columns.lock().expect("rig lock");
        let column = columns.entry((cx, cz)).or_insert_with(|| self.fresh_column());
        column.block_state(x - cx * 16, y, z - cz * 16).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let mut columns = self.columns.lock().expect("rig lock");
        let column = columns.entry((cx, cz)).or_insert_with(|| self.fresh_column());
        column.biome_state_at(x - cx * 16, y, z - cz * 16).to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let mut columns = self.columns.lock().expect("rig lock");
        let column = columns.entry((cx, cz)).or_insert_with(|| self.fresh_column());
        column.set_block(x - cx * 16, y, z - cz * 16, name);
    }
}

const SEED: u64 = 0x5150_3131_4213_9977;

#[test]
#[ignore = "a measurement, not a gate; run in release on an idle machine"]
fn explosion_instruction_profile() {
    let env = BlastEnv {
        min_y: MIN_Y,
        height: HEIGHT,
    };

    // --- stage 1: one creeper blast in solid stone (short rays) ---------------
    let stone = Rig::new("minecraft:stone", Some(64));
    stone.set_block(8, 8, 8, "minecraft:air");
    // Warm the column cache so the measurement is the blast, not the rig's
    // one-off 16x16x129 fill.
    let _ = stone.block_state(8, 8, 8);
    let mut rng = SpawnRng::new(SEED);
    let mut cells = 0usize;
    let stone_instructions = measure(|| {
        cells = explosion_blocks::exploded_positions(
            &stone,
            env,
            Vec3::new(8.5, 8.5, 8.5),
            3.0,
            &mut rng,
        )
        .len();
    });

    // --- stage 2: one creeper blast in open air (longest rays) ----------------
    let air = Rig::new("minecraft:stone", None);
    let _ = air.block_state(0, 100, 0);
    let mut rng = SpawnRng::new(SEED);
    let mut air_cells = 0usize;
    let air_instructions = measure(|| {
        air_cells = explosion_blocks::exploded_positions(
            &air,
            env,
            Vec3::new(0.5, 100.5, 0.5),
            3.0,
            &mut rng,
        )
        .len();
    });

    // --- stage 3: a cannon — eight overlapping blasts in one tick -------------
    // Overlapping deliberately: this is the case a section-level cache shared
    // across one tick's explosions is meant to collapse, so the per-blast cost
    // here is the baseline that change would be judged against.
    let cannon = Rig::new("minecraft:stone", Some(64));
    let _ = cannon.block_state(8, 40, 8);
    let mut rng = SpawnRng::new(SEED);
    let mut cannon_cells = 0usize;
    let cannon_instructions = measure(|| {
        for i in 0..8 {
            cannon_cells += explosion_blocks::destroy_blocks(
                &cannon,
                env,
                Vec3::new(8.5 + f64::from(i), 40.5, 8.5),
                3.0,
                &mut rng,
            )
            .len();
        }
    });

    // --- the three innermost operations, in isolation -------------------------
    const PROBES: usize = 200_000;
    // Every probe varies its input across a small set, so no arm can be hoisted
    // out of its loop as loop-invariant. The first attempt held `probe_id`
    // constant and the flat-table arm measured **0.0 instructions per call** —
    // LLVM had lifted the whole lookup out. That reading was an artefact, not a
    // result, and it is exactly the kind of too-good number this repo's own rule
    // about predicting both halves exists to catch.
    let probe_states: Vec<String> = [
        "minecraft:stone",
        "minecraft:dirt",
        "minecraft:oak_planks",
        "minecraft:obsidian",
        "minecraft:water",
        "minecraft:short_grass",
        "minecraft:bedrock",
        "minecraft:oak_leaves[distance=1,persistent=false]",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let probe_ids: Vec<u32> = probe_states
        .iter()
        .map(|s| block_states::state_id(s).expect("the probe states resolve"))
        .collect();

    let mut sink = 0u64;
    let read_instructions = measure(|| {
        for i in 0..PROBES {
            let state = stone.block_state(i as i32 % 16, 20, 0);
            sink = sink.wrapping_add(state.len() as u64);
        }
    });
    let resolve_instructions = measure(|| {
        for i in 0..PROBES {
            sink = sink.wrapping_add(u64::from(
                block_states::state_id(&probe_states[i % probe_states.len()]).unwrap_or(0),
            ));
        }
    });
    let lookup_instructions = measure(|| {
        for i in 0..PROBES {
            let state = block_states::StateId::new(probe_ids[i % probe_ids.len()])
                .expect("profile state id is valid");
            sink = sink.wrapping_add(u64::from(
                block_blast::explosion_resistance_for_state_id(state)
                    .unwrap_or(0.0)
                    .to_bits(),
            ));
        }
    });
    assert!(sink > 0, "the probe loops must not be optimised away");

    let per_read = read_instructions as f64 / PROBES as f64;
    let per_resolve = resolve_instructions as f64 / PROBES as f64;
    let per_lookup = lookup_instructions as f64 / PROBES as f64;
    let per_step_total = per_read + per_resolve + per_lookup;

    println!("--- explosion instruction profile (radius 3.0, {RAY_COUNT} rays) ---");
    println!("stone blast      : {stone_instructions:>12} instructions, {cells} cells claimed");
    println!("open-air blast   : {air_instructions:>12} instructions, {air_cells} cells claimed");
    println!("8-blast cannon   : {cannon_instructions:>12} instructions, {cannon_cells} cells changed");
    println!(
        "per ray (air)    : {:>12.0}",
        air_instructions as f64 / RAY_COUNT as f64
    );
    println!("--- the innermost three, per call, over {PROBES} probes ---");
    println!(
        "block_state read : {per_read:>12.1}  ({:.1}% of the three)",
        100.0 * per_read / per_step_total
    );
    println!(
        "state_id resolve : {per_resolve:>12.1}  ({:.1}% of the three)",
        100.0 * per_resolve / per_step_total
    );
    println!(
        "flat table index : {per_lookup:>12.1}  ({:.1}% of the three)",
        100.0 * per_lookup / per_step_total
    );
    println!(
        "world read+resolve share: {:.1}%",
        100.0 * (per_read + per_resolve) / per_step_total
    );

    // Structural assertions only — the numbers above are the deliverable.
    assert!(
        stone_instructions > 0 && air_instructions > 0 && cannon_instructions > 0,
        "the instrument must have moved on every arm"
    );
    assert!(
        air_instructions > stone_instructions,
        "an open-air blast marches further than one in stone, so it must cost more: \
         air {air_instructions} vs stone {stone_instructions}"
    );
    assert!(
        per_lookup < per_read,
        "the flat table index must be cheaper than the world read it sits behind \
         ({per_lookup} vs {per_read})"
    );
}
