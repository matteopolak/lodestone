//! Instructions retired at the **protocol chunk-encode boundary**, string path
//! against integer path, both arms in one process — `DESIGN.md` §12.131's
//! measurement.
//!
//! # What this measures and why it was never measured before
//!
//! `V770ServerProtocol::encode_chunk` turns a `lodestone-server` `ChunkColumn`
//! into a `level_chunk_with_light` body. Until this unit its inner loop read a
//! block-state **`&str`** for each of the 98,304 cells in a column, probed each
//! through a per-column `HashMap<&str, u32>` (std's SipHash), and resolved each
//! *distinct* string through a 32,366-row scan of the generated state table doing
//! a property-vector compare per row whose name matched — order 10⁶ string
//! comparisons per served column, paid on every join and every view-tracker
//! resend.
//!
//! It sat outside every worldgen instrument, because the generation cost metric
//! `docs/plans/worldgen-rewrite.md`'s 21 units moved excludes protocol encode by
//! definition. That is the whole reason a 2–8 ms per-column cost survived a
//! dedicated optimisation drive.
//!
//! # Why instructions retired rather than wall clock
//!
//! `DESIGN.md` §12.130 measured this bench family's own reproducibility on this
//! machine: instructions **0.16–0.21%** against wall clock's **11.6–19.1%**, with
//! other agents always compiling. A wall-clock acceptance criterion here is
//! unusable. Same `proc_pid_rusage(getpid(), RUSAGE_INFO_V4, …)` pattern as
//! `lodestone-worldgen/benches/generation.rs` and
//! `lodestone-shell/tests/client_chunk_cycles.rs`, including reaching
//! `ri_instructions` **by name** out of a field-by-field transcription rather
//! than at a hand-computed offset. Wall clock is printed beside it because
//! instructions are blind to locality (§12.120).
//!
//! This lives here rather than beside the byte-identity gate in
//! `src/server_protocol/chunk_encode_identity.rs` for one reason:
//! `lodestone-v770` is `#![forbid(unsafe_code)]`, which `#[allow]` cannot
//! override, so the `proc_pid_rusage` FFI cannot live in the lib at all.
//!
//! # Scope: the cell loop, not the whole encoder
//!
//! Both arms reproduce `build_world_column`'s **block** loop only — the biome
//! half is byte-identically the same code in both, is resolved once per biome
//! *palette* entry (single digits per column), and needs a private table to
//! reproduce. Omitting it from both arms removes an equal constant from each and
//! leaves the difference the unit is about. The whole-`encode_chunk` figure is
//! printed too, as the denominator that says what share of a real send the
//! removed work was.
//!
//! # Running it
//!
//! ```text
//! cargo test --release -p lodestone-v770 --test chunk_encode_cycles -- --ignored --nocapture
//! ```
//!
//! Release matters: the integer arm is a range check and two array indexes, and a
//! debug build's overhead swamps the ratio.

// `proc_pid_rusage` is a `libSystem` FFI declaration, and the workspace sets
// `unsafe_code = "deny"` (root `Cargo.toml`'s `[workspace.lints.rust]`), on top of
// `lodestone-v770`'s own `#![forbid(unsafe_code)]`. `forbid` cannot be overridden
// at all, which is exactly why this measurement is an integration test rather
// than a module in the lib: cargo compiles each `tests/*.rs` as its own binary
// crate, so the crate-root `#![allow]` here cannot reach the library or any other
// target, and the lib keeps its `forbid`.
//
// Precedent and the same trade: `lodestone-worldgen/benches/generation.rs` and
// `lodestone-shell/tests/client_chunk_cycles.rs` both pay it for the same
// instrument. The one `unsafe` call is a read-only accounting syscall into a
// stack buffer whose size is asserted against a derived constant.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::time::Instant;

use lodestone_data::block_states::{block_name, properties};
use lodestone_server::{ChunkColumn, ChunkSource, ServerDirective, ServerProtocol, overworld_chunk_source};
use lodestone_v770::V770ServerProtocol;
use lodestone_world::{ChunkColumn as WorldChunkColumn, ChunkSection};
use lodestone_v770::packets::chunk::ChunkShape;

/// Seed and origin: the fixture `tests/block_edit.rs` and
/// `encode_chunk_carries_real_block_states_including_a_fluid` already pin, so the
/// columns measured here are ones other gates describe.
const SEED: i64 = 1234;
const COLUMNS: usize = 8;
const REPEATS: usize = 5;

// ---------------------------------------------------------------------------
// The two arms
// ---------------------------------------------------------------------------

/// `resolve_state_id` as it was: a linear scan over **all** 32,366 block states
/// with a property-vector compare per matching row, then the two fallback tiers.
///
/// A verbatim copy of the pre-change function, as the measurement's *before* arm.
/// It is not a silent second implementation: `chunk_encode_identity.rs` asserts
/// the integer path and this path encode byte-identical payloads, so a change to
/// `lodestone_data::block_states::state_id`'s semantics fails there loudly rather
/// than drifting here quietly.
fn resolve_state_id_legacy(state: &str) -> u32 {
    let (name, raw_props) = match state.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
        None => (state, ""),
    };
    let mut wanted: Vec<(&str, &str)> = if raw_props.is_empty() {
        Vec::new()
    } else {
        raw_props
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .collect()
    };
    wanted.sort_unstable();

    let mut first_id: Option<u32> = None;
    let mut last_id: Option<u32> = None;
    let mut default_id: Option<u32> = None;
    for id in 0..lodestone_data::block_states::STATE_COUNT {
        if block_name(id) != Some(name) {
            continue;
        }
        first_id.get_or_insert(id);
        last_id = Some(id);
        if lodestone_data::snow_support::is_default_state(id) == Some(true) {
            default_id = Some(id);
        }
        let mut have: Vec<(&str, &str)> = properties(id).unwrap_or(&[]).to_vec();
        have.sort_unstable();
        if have == wanted {
            return id;
        }
    }

    let Some(base) = default_id.or(first_id) else {
        return lodestone_data::block_states::air_state_id();
    };
    if wanted.is_empty() {
        return base;
    }

    let mut merged: Vec<(&str, &str)> = properties(base).unwrap_or(&[]).to_vec();
    let mut overridden = false;
    for &(key, value) in &wanted {
        if let Some(slot) = merged.iter_mut().find(|(have_key, _)| *have_key == key) {
            if slot.1 != value {
                slot.1 = value;
                overridden = true;
            }
        }
    }
    if !overridden {
        return base;
    }
    merged.sort_unstable();
    let (Some(start), Some(end)) = (first_id, last_id) else {
        return base;
    };
    for id in start..=end {
        let mut have: Vec<(&str, &str)> = properties(id).unwrap_or(&[]).to_vec();
        have.sort_unstable();
        if have == merged {
            return id;
        }
    }
    base
}

/// The **before** arm: `&str` per cell, SipHash memo, 32k-row scan per distinct
/// entry.
fn cell_loop_string_path(shape: &ChunkShape, source: &ChunkColumn) -> WorldChunkColumn {
    let mut column = WorldChunkColumn::new(
        shape.min_y,
        shape.section_count,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );
    let mut seen: HashMap<&str, u32> = HashMap::new();
    for section_index in 0..shape.section_count {
        let base_y = shape.min_y + (section_index * ChunkSection::EDGE) as i32;
        let mut section = ChunkSection::new(
            shape.block_kind,
            shape.biome_kind,
            shape.air_id,
            shape.biome_id,
        );
        for ly in 0..ChunkSection::EDGE {
            let wy = base_y + ly as i32;
            for lz in 0..ChunkSection::EDGE {
                for lx in 0..ChunkSection::EDGE {
                    let state = source.block_state(lx as i32, wy, lz as i32);
                    let id = *seen
                        .entry(state)
                        .or_insert_with(|| resolve_state_id_legacy(state));
                    if id != shape.air_id {
                        section.set_block(lx, ly, lz, id);
                    }
                }
            }
        }
        if !section.is_empty(shape.biome_id) {
            column.set_section(section_index, Some(section));
        }
    }
    column
}

/// The **after** arm: one range check and two array indexes per cell, because the
/// column resolved its own palette once at adoption time
/// (`ChunkColumn::palette_state_ids`).
fn cell_loop_integer_path(shape: &ChunkShape, source: &ChunkColumn) -> WorldChunkColumn {
    let mut column = WorldChunkColumn::new(
        shape.min_y,
        shape.section_count,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );
    for section_index in 0..shape.section_count {
        let base_y = shape.min_y + (section_index * ChunkSection::EDGE) as i32;
        let mut section = ChunkSection::new(
            shape.block_kind,
            shape.biome_kind,
            shape.air_id,
            shape.biome_id,
        );
        for ly in 0..ChunkSection::EDGE {
            let wy = base_y + ly as i32;
            for lz in 0..ChunkSection::EDGE {
                for lx in 0..ChunkSection::EDGE {
                    let id = source.block_state_id(lx as i32, wy, lz as i32);
                    if id != shape.air_id {
                        section.set_block(lx, ly, lz, id);
                    }
                }
            }
        }
        if !section.is_empty(shape.biome_id) {
            column.set_section(section_index, Some(section));
        }
    }
    column
}

// ---------------------------------------------------------------------------
// Instructions retired
// ---------------------------------------------------------------------------

// Darwin-only from here to `instructions_retired`. `proc_pid_rusage` lives in
// `libSystem` and has no equivalent symbol on Linux or Windows, so an ungated
// `extern "C"` declaration of it links fine on macOS and fails the *link* — not
// the compile — everywhere else. `cargo check` never links, which is why this was
// invisible to every `check` job and only surfaced as
// `rust-lld: error: undefined symbol: proc_pid_rusage` in `cargo test`.
//
// Gated per-item rather than over the whole file on purpose:
// `encode_chunk_still_returns_a_send` below is NOT `#[ignore]`d and is not a
// measurement, so it must keep compiling and running on every platform. A
// file-level `#![cfg(target_os = "macos")]` would have silently dropped it from
// the Linux and Windows suites.

/// `RUSAGE_INFO_V4` from `<sys/resource.h>`.
#[cfg(target_os = "macos")]
const RUSAGE_INFO_V4: i32 = 4;

/// `struct rusage_info_v4` from macOS `<sys/resource.h>`, transcribed
/// **field-by-field in declaration order** so `ri_instructions` is reached by
/// name. [`RUSAGE_INFO_V4_SIZE`] is the check that a dropped or mis-typed line
/// fails loudly instead of silently shifting which field is read.
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

/// Non-Darwin arm. Panics rather than returning a plausible zero: the only caller
/// compares a before/after difference, and a counter stuck at 0 would report a
/// cost of zero instructions for both encode paths — a measurement that looks
/// like a result and is not one. Its caller is `#[ignore]`d, so nothing on Linux
/// or Windows reaches this; `--ignored` on those targets should say so loudly.
#[cfg(not(target_os = "macos"))]
fn instructions_retired() -> u64 {
    unimplemented!(
        "instructions retired is read through proc_pid_rusage(RUSAGE_INFO_V4), which exists \
         only on Darwin; this comparator is a macOS-only measurement and has no counter to \
         report on this target"
    )
}

/// Instructions retired by this **process** so far. Process-wide, not
/// per-thread: everything measured through it here is single-threaded.
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
        "proc_pid_rusage(RUSAGE_INFO_V4) failed with {rc}; the standing before/after comparator \
         has nothing to report without it"
    );
    info.ri_instructions
}

/// Median, and the min→max spread as a percentage of it — the reproducibility
/// figure that licenses quoting the median at all.
fn summarise(samples: &mut [u64]) -> (u64, f64) {
    samples.sort_unstable();
    let median = samples[samples.len() / 2];
    let spread = (samples[samples.len() - 1] - samples[0]) as f64 / median as f64 * 100.0;
    (median, spread)
}

#[test]
#[ignore = "measurement: real worldgen, then instructions retired for both encode paths"]
fn encode_cost_per_column_instructions_retired() {
    // The transcription-size control is a statement about Darwin's
    // `rusage_info_v4`, so there is nothing to assert where that struct's constants
    // are configured out. No vacuous green is created by its absence: the very
    // next thing this test does is read the counter, and `instructions_retired`'s
    // non-Darwin arm panics, so the test cannot pass on a non-Darwin host either
    // way. It is `#[ignore]`d, so it does not run there at all.
    #[cfg(target_os = "macos")]
    {
        assert_eq!(
            size_of::<RusageInfoV4>(),
            RUSAGE_INFO_V4_SIZE,
            "rusage_info_v4 transcription is the wrong size, so ri_instructions is the wrong field"
        );
    }

    let shape = ChunkShape::overworld_1_21();
    // Generation happens before any counter is read, so none of it lands in a
    // measured window.
    let source = overworld_chunk_source(SEED);
    let columns: Vec<ChunkColumn> = (0..COLUMNS)
        .map(|i| source.column((i % 4) as i32, (i / 4) as i32))
        .collect();
    let cells_per_column = 16 * 16 * shape.world_height as u64;
    let distinct: usize = columns.iter().map(|c| c.raw_palette().len()).sum();
    assert!(
        columns.iter().any(|c| c.raw_palette().len() >= 3),
        "fixture is degenerate: no column has three distinct palette entries, so the resolver \
         arm is not being exercised"
    );

    // Warm both arms so neither pays the one-time `lodestone_data` index build or
    // first-touch faults inside a measured window.
    std::hint::black_box(cell_loop_integer_path(&shape, &columns[0]));
    std::hint::black_box(cell_loop_string_path(&shape, &columns[0]));
    let proto = V770ServerProtocol;
    std::hint::black_box(proto.encode_chunk(0, 0, &columns[0]));

    let mut integer_insn = Vec::with_capacity(REPEATS);
    let mut string_insn = Vec::with_capacity(REPEATS);
    let mut encode_insn = Vec::with_capacity(REPEATS);
    let mut integer_ns = Vec::with_capacity(REPEATS);
    let mut string_ns = Vec::with_capacity(REPEATS);

    for _ in 0..REPEATS {
        // One counter read per sweep of all COLUMNS columns, not per column:
        // `proc_pid_rusage` itself costs ~80,000 instructions per read (§12.130 —
        // it walks the task's threads), a fixed term the smaller arm would
        // otherwise absorb proportionally more of. Amortised over 8 columns it is
        // ~10k, well under 1% of either arm.
        //
        // The arms alternate within each repeat so a drift in machine state lands
        // on both.
        let t0 = Instant::now();
        let a = instructions_retired();
        for column in &columns {
            std::hint::black_box(cell_loop_integer_path(&shape, std::hint::black_box(column)));
        }
        let b = instructions_retired();
        let t1 = Instant::now();
        for column in &columns {
            std::hint::black_box(cell_loop_string_path(&shape, std::hint::black_box(column)));
        }
        let c = instructions_retired();
        let t2 = Instant::now();
        for column in &columns {
            std::hint::black_box(proto.encode_chunk(0, 0, std::hint::black_box(column)));
        }
        let d = instructions_retired();

        integer_insn.push((b - a) / COLUMNS as u64);
        string_insn.push((c - b) / COLUMNS as u64);
        encode_insn.push((d - c) / COLUMNS as u64);
        integer_ns.push((t1 - t0).as_nanos() as u64 / COLUMNS as u64);
        string_ns.push((t2 - t1).as_nanos() as u64 / COLUMNS as u64);
    }

    let (integer, integer_spread) = summarise(&mut integer_insn);
    let (string, string_spread) = summarise(&mut string_insn);
    let (encode, encode_spread) = summarise(&mut encode_insn);
    let (integer_time, _) = summarise(&mut integer_ns);
    let (string_time, string_time_spread) = summarise(&mut string_ns);

    println!(
        "chunk encode boundary: {COLUMNS} real columns (seed {SEED}) x {REPEATS} repeats, \
         {cells_per_column} cells/column, {distinct} palette entries across the set"
    );
    println!(
        "  cell loop, string path  (before): {string:>11} insn/column  spread {string_spread:.2}%  \
         {string_time:>9} ns/column"
    );
    println!(
        "  cell loop, integer path (after):  {integer:>11} insn/column  spread {integer_spread:.2}%  \
         {integer_time:>9} ns/column"
    );
    println!(
        "  removed: {} insn/column ({:.2}x), {} insn/cell",
        string - integer,
        string as f64 / integer as f64,
        (string - integer) / cells_per_column
    );
    println!(
        "  whole encode_chunk, integer path (cells + biomes + light + framing): \
         {encode:>11} insn/column  spread {encode_spread:.2}%"
    );
    println!(
        "  the removed work was {:.1}% of what a served column would cost today",
        (string - integer) as f64 / (encode + string - integer) as f64 * 100.0
    );
    println!(
        "  wall-clock spread on the string arm was {string_time_spread:.2}% against its \
         {string_spread:.2}% instruction spread — the reason instructions are the comparator"
    );

    // A floor on the instrument rather than an acceptance criterion: the integer
    // path cannot legitimately cost more, and two arms reading within noise of
    // each other would mean the counter is not measuring this work.
    assert!(
        string > integer,
        "the string path ({string}) did not cost more than the integer path ({integer}) — either \
         the change did nothing or the counter is not measuring it"
    );
    assert!(
        integer_spread < 5.0 && string_spread < 5.0,
        "instruction counts moved more than 5% across identical repeats \
         (integer {integer_spread:.2}%, string {string_spread:.2}%), so neither median is \
         quotable — re-run alone before believing either"
    );
}

/// The `ServerDirective` shape the measurement's whole-encode arm exercises, so a
/// silent change to `encode_chunk`'s return does not turn that arm into a
/// no-op measured against nothing.
#[test]
fn encode_chunk_still_returns_a_send() {
    let source = overworld_chunk_source(SEED);
    let directive = V770ServerProtocol.encode_chunk(0, 0, &source.column(0, 0));
    match directive {
        ServerDirective::Send { payload, .. } => assert!(!payload.is_empty()),
        other => panic!("expected Send, got {other:?}"),
    }
}
