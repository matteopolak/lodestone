//! PGO before/after comparator, built to answer a simple question: does
//! profile-guided optimization measurably help this workspace's release
//! build, on top of the `lto = "thin"`/`codegen-units = 1` it already uses?
//!
//! Deliberately self-contained within `lodestone-worldgen` +
//! `lodestone-worldgen-core` — no `lodestone-server` dependency, unlike
//! `benches/generation.rs`'s embedded-data arms. That crate had an unrelated,
//! uncommitted compile error from a live agent's in-progress edit
//! (`PlayerCandidate` missing `xp_level`/`xp_points`) partway through this
//! experiment, which is exactly the transient-error class `CLAUDE.md`
//! describes ("Expect transient errors from files you never touched; the
//! discriminator is `git status`/`git diff`") — not something to wait out
//! indefinitely when a dependency-free path exists. Uses the same
//! `tests/support/worldgen_data` fixture tree `tests/overworld_gen.rs`,
//! `benches/generation.rs` and `tests/embedded_vs_fixture_stage_cost.rs`
//! already share.
//!
//! # Method
//!
//! Generates the same fixed `PATCH_SIDE x PATCH_SIDE` cache-cold patch twice
//! (warm-up discarded, second pass timed) and reports **instructions
//! retired** (`proc_pid_rusage(RUSAGE_INFO_V4)`, macOS-only — the same
//! counter `benches/generation.rs`'s `instructions_retired` uses and
//! validates), per `CLAUDE.md`'s "prefer a counter over a duration" rule:
//! this machine has 5 other agents compiling concurrently, so a wall-clock
//! number here would be measuring machine load, not PGO. Run this binary
//! once compiled with `-Cprofile-generate=<dir>` and the resulting profile
//! merged and fed back via `-Cprofile-use=<merged.profdata>`, and compare the
//! printed instruction count against a plain release build's.
//!
//! `cargo run --release --example pgo_probe -p lodestone-worldgen`

// Same narrow exception `crates/lodestone-fuzz/tests/length_prefix_allocation.rs`
// and `container_set_content_unbounded_allocation.rs` already carry for this
// workspace's `unsafe_code = "deny"` lint: a single FFI call
// (`proc_pid_rusage`) with no allocation or aliasing logic of its own to get
// wrong, scoped to one example binary that nothing else in the crate depends
// on (`examples/*.rs` never links into the library or any other target).
#![allow(unsafe_code)]

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use serde_json::Value;
use std::hint::black_box;
use std::path::Path;

const SEED: i64 = 42;
const PATCH_SIDE: i32 = 6;

#[cfg(target_os = "macos")]
mod insn_counter {
    #[repr(C)]
    #[derive(Default)]
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

    const RUSAGE_INFO_V4: i32 = 4;

    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
    }

    pub fn instructions_retired() -> u64 {
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
}

#[cfg(not(target_os = "macos"))]
mod insn_counter {
    pub fn instructions_retired() -> u64 {
        unimplemented!("instructions-retired counter is Darwin-only (proc_pid_rusage)")
    }
}

struct FsResolver {
    root: std::path::PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }

    fn try_read(&self, kind: &str, id: &str) -> Value {
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

fn build_generator() -> OverworldGenerator {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    OverworldGenerator::new(SEED, &settings, &resolver, "minecraft:plains", false)
}

fn generate_patch(generator: &OverworldGenerator) -> u64 {
    let mut total = 0u64;
    for cx in 0..PATCH_SIDE {
        for cz in 0..PATCH_SIDE {
            let col = generator.column(cx, cz);
            total = total.wrapping_add(black_box(col.non_air_count()) as u64);
        }
    }
    total
}

fn main() {
    // Warm-up pass: page faults, allocator warm-up, and one-time lazy inits
    // (interner tables etc.) must not be attributed to the timed pass.
    let warm_up_generator = build_generator();
    black_box(generate_patch(&warm_up_generator));

    let generator = build_generator();
    let before = insn_counter::instructions_retired();
    let non_air = generate_patch(&generator);
    let after = insn_counter::instructions_retired();
    let insns = after.saturating_sub(before);

    println!("PGO_PROBE patch={PATCH_SIDE}x{PATCH_SIDE} seed={SEED} non_air_checksum={non_air}");
    println!("PGO_PROBE instructions_retired={insns}");
}
