//! Production worldgen measurement with exactly one generation worker.
//!
//! This is intentionally an ignored benchmark rather than a CI timing test.
//! It drives `ChunkSource::request_generation`, so each target passes through
//! the production session, full stage schedule, ordered mutable commit, and
//! packet snapshot boundary. The light and packet encoder are measured after
//! generation instead of being folded into the generation number.

#![cfg(not(target_arch = "wasm32"))]
#![allow(unsafe_code)]

use std::hint::black_box;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
use std::cell::Cell;
#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
use std::sync::OnceLock;
#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use lodestone_server::worldgen_session::{
    GenerationRequest, GenerationRequestResult, GenerationSession,
};
use lodestone_server::{ChunkColumn, ChunkSource, ServerProtocol, overworld_chunk_source};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_server::dimension::Dimension as ServerDimension;
use lodestone_worldgen::counters;
#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
use lodestone_worldgen::counters::{RegionPhase, Stage, StageEvent, STAGE_COUNT};
use lodestone_worldgen::stage_schedule::{Dimension as WorldgenDimension, GenerationTarget};

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default, Clone, Copy)]
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

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
}

#[cfg(target_os = "macos")]
fn rusage() -> RusageInfoV4 {
    let mut usage = RusageInfoV4::default();
    let result = unsafe {
        proc_pid_rusage(
            i32::try_from(std::process::id()).expect("pid fits in i32"),
            4,
            (&raw mut usage).cast::<core::ffi::c_void>(),
        )
    };
    assert_eq!(result, 0, "proc_pid_rusage failed with {result}");
    usage
}

#[cfg(target_os = "macos")]
fn retired() -> Option<(u64, u64)> {
    let usage = rusage();
    Some((usage.ri_instructions, usage.ri_cycles))
}

#[cfg(not(target_os = "macos"))]
fn retired() -> Option<(u64, u64)> {
    None
}

#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
static STAGE_INSTRUCTIONS: [AtomicU64; STAGE_COUNT] =
    [const { AtomicU64::new(0) }; STAGE_COUNT];
#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
static STAGE_CYCLES: [AtomicU64; STAGE_COUNT] = [const { AtomicU64::new(0) }; STAGE_COUNT];
#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
static REGION_INSTRUCTIONS: [AtomicU64; 24] = [const { AtomicU64::new(0) }; 24];
#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
static REGION_CYCLES: [AtomicU64; 24] = [const { AtomicU64::new(0) }; 24];
#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
static PMU_READ_COST: OnceLock<(u64, u64)> = OnceLock::new();

#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
thread_local! {
    static STAGE_START: Cell<Option<(Stage, u64, u64)>> = const { Cell::new(None) };
    static REGION_START: Cell<([Option<(RegionPhase, u64, u64)>; 16], usize)> =
        const { Cell::new(([None; 16], 0)) };
}

#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
fn stage_pmu_observer(stage: Stage, event: StageEvent) {
    match event {
        StageEvent::Enter => {
            let (instructions, cycles) = retired().expect("stage PMU requires retired counters");
            STAGE_START.with(|start| {
                assert!(start.get().is_none(), "stage PMU scopes may not overlap");
                start.set(Some((stage, instructions, cycles)));
            });
        }
        StageEvent::Exit => {
            let (started_stage, before_instructions, before_cycles) = STAGE_START
                .with(|start| start.take())
                .expect("stage PMU exit without an enter");
            assert_eq!(started_stage, stage, "stage PMU scope changed stages");
            let (after_instructions, after_cycles) =
                retired().expect("stage PMU requires retired counters");
            let (read_instructions, read_cycles) = *PMU_READ_COST
                .get()
                .expect("stage PMU read cost must be initialized");
            STAGE_INSTRUCTIONS[stage as usize].fetch_add(
                after_instructions
                    .saturating_sub(before_instructions)
                    .saturating_sub(read_instructions),
                Relaxed,
            );
            STAGE_CYCLES[stage as usize].fetch_add(
                after_cycles
                    .saturating_sub(before_cycles)
                    .saturating_sub(read_cycles),
                Relaxed,
            );
        }
    }
}

#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
fn region_pmu_observer(phase: RegionPhase, event: StageEvent) {
    match event {
        StageEvent::Enter => {
            let (instructions, cycles) = retired().expect("region PMU requires retired counters");
            REGION_START.with(|state| {
                let (mut stack, depth) = state.get();
                assert!(depth < stack.len(), "region PMU nesting exceeded its bound");
                stack[depth] = Some((phase, instructions, cycles));
                state.set((stack, depth + 1));
            });
        }
        StageEvent::Exit => {
            let (started_phase, before_instructions, before_cycles) = REGION_START.with(|state| {
                let (mut stack, depth) = state.get();
                assert!(depth > 0, "region PMU exit without an enter");
                let frame = stack[depth - 1]
                    .take()
                    .expect("active region PMU frame is present");
                state.set((stack, depth - 1));
                frame
            });
            assert_eq!(started_phase, phase, "region PMU scope changed phases");
            let (after_instructions, after_cycles) =
                retired().expect("region PMU requires retired counters");
            let (read_instructions, read_cycles) = *PMU_READ_COST
                .get()
                .expect("region PMU read cost must be initialized");
            let index = phase as usize;
            REGION_INSTRUCTIONS[index].fetch_add(
                after_instructions
                    .saturating_sub(before_instructions)
                    .saturating_sub(read_instructions),
                Relaxed,
            );
            REGION_CYCLES[index].fetch_add(
                after_cycles
                    .saturating_sub(before_cycles)
                    .saturating_sub(read_cycles),
                Relaxed,
            );
        }
    }
}

#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
fn install_stage_pmu() {
    let before = retired().expect("stage PMU requires retired counters");
    let after = retired().expect("stage PMU requires retired counters");
    PMU_READ_COST
        .set((
            after.0.saturating_sub(before.0),
            after.1.saturating_sub(before.1),
        ))
        .expect("stage PMU read cost initialized once");
    assert!(
        PMU_READ_COST.get().is_some_and(|(instructions, cycles)| {
            *instructions > 0 && *cycles > 0
        }),
        "stage PMU read-cost control did not observe work"
    );
    assert!(
        counters::install_stage_observer(stage_pmu_observer),
        "stage PMU observer can only be installed once"
    );
    assert!(
        counters::install_region_observer(region_pmu_observer),
        "region PMU observer can only be installed once"
    );
}

#[cfg(all(feature = "worldgen-stage-pmu", not(target_os = "macos")))]
fn install_stage_pmu() {
    panic!("stage PMU diagnostics require macOS retired-instruction counters");
}

#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
fn report_stage_pmu(total: &Measurement<impl Sized>, columns: usize, phase: &str) {
    let per_column = columns.max(1) as f64;
    let instructions: [u64; STAGE_COUNT] =
        std::array::from_fn(|index| STAGE_INSTRUCTIONS[index].load(Relaxed));
    let cycles: [u64; STAGE_COUNT] =
        std::array::from_fn(|index| STAGE_CYCLES[index].load(Relaxed));
    let sum_instructions = instructions.iter().sum::<u64>();
    let sum_cycles = cycles.iter().sum::<u64>();
    let remainder_instructions = total
        .counters
        .map_or(0, |(value, _)| value.saturating_sub(sum_instructions));
    let remainder_cycles = total
        .counters
        .map_or(0, |(_, value)| value.saturating_sub(sum_cycles));
    let report = |stage: &str, instruction_count: u64, cycle_count: u64| {
        println!(
            "STRICT_WORLDGEN metric=stage_pmu phase={phase} stage={stage} columns={columns} instructions={instruction_count} instructions_per_column={:.0} cycles={cycle_count} cycles_per_column={:.0} ipc={:.3}",
            instruction_count as f64 / per_column,
            cycle_count as f64 / per_column,
            instruction_count as f64 / cycle_count.max(1) as f64,
        );
    };
    let (prefix_instructions, prefix_cycles) = grouped_stage_totals(
        &instructions,
        &cycles,
        &[Stage::Aquifer, Stage::Shape, Stage::Biome, Stage::Surface, Stage::Materialize, Stage::Carve, Stage::Structure],
    );
    let (feature_instructions, feature_cycles) = grouped_stage_totals(
        &instructions,
        &cycles,
        &[Stage::Ore, Stage::Vegetation],
    );
    let (final_instructions, final_cycles) = grouped_stage_totals(
        &instructions,
        &cycles,
        &[Stage::TopLayer, Stage::Intern],
    );
    report("terrain_prefix", prefix_instructions, prefix_cycles);
    report("features", feature_instructions, feature_cycles);
    report("finalization", final_instructions, final_cycles);
    report("session_remainder", remainder_instructions, remainder_cycles);
    for stage in [
        Stage::Aquifer,
        Stage::Shape,
        Stage::Biome,
        Stage::Surface,
        Stage::Materialize,
        Stage::Carve,
        Stage::Structure,
        Stage::Ore,
        Stage::Vegetation,
        Stage::TopLayer,
        Stage::Intern,
    ] {
        report(counters::STAGE_NAMES[stage as usize], instructions[stage as usize], cycles[stage as usize]);
    }
    for (name, index) in [
        ("admission", 0),
        ("replay_context", 1),
        ("mutable_target", 2),
        ("mutable_padding", 3),
        ("snapshot_finalization", 4),
        ("prefix_import", 5),
        ("ledger_checkpoint_capture", 6),
        ("session_hydration", 7),
        ("checkpoint_export", 8),
        ("ledger_publish_inner", 9),
        ("mutation_winner_scan", 10),
        ("direct_transition_mirror", 11),
        ("output_snapshot", 12),
        ("packet_neighbours", 13),
        ("packet_finalize", 14),
        ("machine_rebuild", 15),
        ("settlement_resume", 16),
        ("commit_features", 17),
        ("commit_top_layer", 18),
        ("resume_output", 19),
        ("feature_source_commit", 20),
        ("feature_snapshot", 21),
        ("feature_stage_publish", 22),
        ("feature_settlement", 23),
    ] {
        report(name, REGION_INSTRUCTIONS[index].load(Relaxed), REGION_CYCLES[index].load(Relaxed));
    }
}

#[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
fn grouped_stage_totals(
    instructions: &[u64; STAGE_COUNT],
    cycles: &[u64; STAGE_COUNT],
    stages: &[Stage],
) -> (u64, u64) {
    stages.iter().fold((0, 0), |(instruction_sum, cycle_sum), stage| {
        (
            instruction_sum + instructions[*stage as usize],
            cycle_sum + cycles[*stage as usize],
        )
    })
}

fn parse(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

struct Measurement<T> {
    value: T,
    elapsed: Duration,
    counters: Option<(u64, u64)>,
}

fn measure_request<T>(before: Option<(u64, u64)>, request: impl FnOnce() -> T) -> Measurement<T> {
    let started = Instant::now();
    let value = request();
    let elapsed = started.elapsed();
    let counters = before.zip(retired()).map(|(before, after)| {
        (
            after.0.saturating_sub(before.0),
            after.1.saturating_sub(before.1),
        )
    });
    Measurement {
        value,
        elapsed,
        counters,
    }
}

fn report<T>(
    metric: &str,
    phase: &str,
    measurement: &Measurement<T>,
    columns: usize,
    batch_size: usize,
    layout: &str,
) {
    let elapsed = measurement.elapsed;
    let per_column = columns.max(1) as f64;
    if let Some((instructions, cycles)) = measurement.counters {
        assert!(
            instructions > 0,
            "PMU instructions counter must be nonzero for {metric}/{phase}"
        );
        assert!(
            cycles > 0,
            "PMU cycles counter must be nonzero for {metric}/{phase}"
        );
        println!(
            "STRICT_WORLDGEN metric={metric} phase={phase} layout={layout} batch_size={batch_size} columns={columns} elapsed_ms={:.3} columns_per_sec={:.3} instructions={} instructions_per_column={:.0} cycles={} cycles_per_column={:.0}",
            elapsed.as_secs_f64() * 1000.0,
            per_column / elapsed.as_secs_f64(),
            instructions,
            instructions as f64 / per_column,
            cycles,
            cycles as f64 / per_column,
        );
    } else {
        println!(
            "STRICT_WORLDGEN metric={metric} phase={phase} layout={layout} batch_size={batch_size} columns={columns} elapsed_ms={:.3} columns_per_sec={:.3} instructions=unavailable cycles=unavailable",
            elapsed.as_secs_f64() * 1000.0,
            per_column / elapsed.as_secs_f64(),
        );
    }
}

fn report_generation_counters(
    before: Option<counters::Snapshot>,
    after: Option<counters::Snapshot>,
    columns: usize,
    phase: &str,
) {
    if std::env::var("LODESTONE_WORLDGEN_BENCH_COUNTERS").as_deref() != Ok("1") {
        return;
    }
    let Some((before, after)) = before.zip(after) else {
        println!("STRICT_WORLDGEN metric=gen_counters phase={phase} counters=unavailable");
        return;
    };
    let per_column = columns.max(1) as f64;
    let delta = |a: u64, b: u64| b.saturating_sub(a);
    let stage = |index| delta(before.stage_entered[index], after.stage_entered[index]);
    let prefix_computed = delta(before.pre_ore_computed, after.pre_ore_computed);
    let prefix_hits = delta(before.pre_ore_hits, after.pre_ore_hits);
    let ore = stage(counters::Stage::Ore as usize);
    let vegetation = stage(counters::Stage::Vegetation as usize);
    let top_layer = stage(counters::Stage::TopLayer as usize);
    let intern = stage(counters::Stage::Intern as usize);
    let conversions = delta(before.full_column_conversions, after.full_column_conversions);
    let conversion_cells = delta(
        before.full_column_conversion_cells,
        after.full_column_conversion_cells,
    );
    let full_raw = delta(
        before.epoch_dirty_full_raw_entries,
        after.epoch_dirty_full_raw_entries,
    );
    let full_unique = delta(
        before.epoch_dirty_full_unique_positions,
        after.epoch_dirty_full_unique_positions,
    );
    let sparse_raw = delta(
        before.epoch_dirty_sparse_raw_entries,
        after.epoch_dirty_sparse_raw_entries,
    );
    let sparse_unique = delta(
        before.epoch_dirty_sparse_unique_positions,
        after.epoch_dirty_sparse_unique_positions,
    );
    println!(
        "STRICT_WORLDGEN metric=gen_counters phase={phase} columns={columns} immutable_prefix_computed={prefix_computed} immutable_prefix_hits={prefix_hits} immutable_prefix_per_column={:.3} mutable_feature_execution_ore={ore} mutable_feature_execution_vegetation={vegetation} mutable_feature_execution_top_layer={top_layer} finalization_packing_intern={intern} finalization_packing_conversions={conversions} finalization_packing_cells={conversion_cells} epoch_dirty_full_raw={full_raw} epoch_dirty_full_unique={full_unique} epoch_dirty_sparse_raw={sparse_raw} epoch_dirty_sparse_unique={sparse_unique} instructions=unavailable cycles=unavailable replay_context_construction=unavailable context_product_count=unavailable",
        (prefix_computed + prefix_hits) as f64 / per_column,
    );
    println!(
        "STRICT_WORLDGEN metric=gen_work phase={phase} block_at={} full_scans={} full_scan_cells={} biome_searches={} biome_rows={} climate_grids={} preliminary_requests={} preliminary_unique={} preliminary_computations={} corner_lookups={} corner_evals={} cell_fills={} slot_hits={} slot_misses={} noise_batches={} structure_starts={} structure_height_probes={} structure_probe_blocks={} structure_context_blocks={} structure_references={} structure_candidate_cells={} structure_piece_checks={} structure_pieces_reached={} nonpositive_cell_skips={}",
        delta(before.block_at, after.block_at),
        delta(before.full_column_scans, after.full_column_scans),
        delta(before.full_column_scan_cells, after.full_column_scan_cells),
        delta(before.biome_searches, after.biome_searches),
        delta(before.biome_rows_compared, after.biome_rows_compared),
        delta(before.climate_grid_preparations, after.climate_grid_preparations),
        delta(before.preliminary_surface_requests, after.preliminary_surface_requests),
        delta(before.preliminary_surface_unique, after.preliminary_surface_unique),
        delta(before.preliminary_surface_computations, after.preliminary_surface_computations),
        delta(before.corner_lookups, after.corner_lookups),
        delta(before.corner_evals, after.corner_evals),
        delta(before.cell_fills, after.cell_fills),
        delta(before.slot_hits, after.slot_hits),
        delta(before.slot_misses, after.slot_misses),
        delta(before.noise_corner_batches, after.noise_corner_batches),
        delta(before.structure_starts_computed, after.structure_starts_computed),
        delta(before.structure_height_probes, after.structure_height_probes),
        delta(before.structure_probe_block_at, after.structure_probe_block_at),
        delta(before.structure_context_block_at, after.structure_context_block_at),
        delta(before.structure_reference_computations, after.structure_reference_computations),
        delta(before.structure_candidate_cell_probes, after.structure_candidate_cell_probes),
        delta(before.structure_place_piece_bbox_checks, after.structure_place_piece_bbox_checks),
        delta(before.structure_place_pieces_reached, after.structure_place_pieces_reached),
        delta(before.nonpositive_cell_skips, after.nonpositive_cell_skips),
    );
    let epoch_dirty_local = delta(before.epoch_dirty_local_writes, after.epoch_dirty_local_writes);
    let epoch_dirty_spill = delta(before.epoch_dirty_spill_writes, after.epoch_dirty_spill_writes);
    let override_attempts = delta(
        before.materializer_override_revision_attempts,
        after.materializer_override_revision_attempts,
    );
    let override_insertions = delta(
        before.materializer_override_revision_insertions,
        after.materializer_override_revision_insertions,
    );
    let carver_attempts = delta(
        before.materializer_carver_revision_attempts,
        after.materializer_carver_revision_attempts,
    );
    let carver_insertions = delta(
        before.materializer_carver_revision_insertions,
        after.materializer_carver_revision_insertions,
    );
    let canonical_winner_updates = delta(
        before.canonical_winner_updates,
        after.canonical_winner_updates,
    );
    let winner_local_vacant = delta(
        before.canonical_winner_local_vacant,
        after.canonical_winner_local_vacant,
    );
    let winner_local_replaced = delta(
        before.canonical_winner_local_replaced,
        after.canonical_winner_local_replaced,
    );
    let winner_local_lost = delta(
        before.canonical_winner_local_lost,
        after.canonical_winner_local_lost,
    );
    let winner_foreign_vacant = delta(
        before.canonical_winner_foreign_vacant,
        after.canonical_winner_foreign_vacant,
    );
    let winner_foreign_replaced = delta(
        before.canonical_winner_foreign_replaced,
        after.canonical_winner_foreign_replaced,
    );
    let winner_foreign_lost = delta(
        before.canonical_winner_foreign_lost,
        after.canonical_winner_foreign_lost,
    );
    let winner_attempts = winner_local_vacant
        + winner_local_replaced
        + winner_local_lost
        + winner_foreign_vacant
        + winner_foreign_replaced
        + winner_foreign_lost;
    let winner_vacant = winner_local_vacant + winner_foreign_vacant;
    let authenticated_write_calls = delta(
        before.authenticated_write_calls,
        after.authenticated_write_calls,
    );
    println!(
        "STRICT_WORLDGEN metric=target_write_bookkeeping phase={phase} outputs={columns} epoch_dirty_local={epoch_dirty_local} epoch_dirty_local_per_output={:.3} epoch_dirty_spill={epoch_dirty_spill} epoch_dirty_spill_per_output={:.3} override_revision_attempts={override_attempts} override_revision_attempts_per_output={:.3} override_revision_insertions={override_insertions} override_revision_insertions_per_output={:.3} carver_revision_attempts={carver_attempts} carver_revision_attempts_per_output={:.3} carver_revision_insertions={carver_insertions} carver_revision_insertions_per_output={:.3} canonical_winner_updates={canonical_winner_updates} canonical_winner_updates_per_output={:.3} winner_local_vacant={winner_local_vacant} winner_local_replaced={winner_local_replaced} winner_local_lost={winner_local_lost} winner_foreign_vacant={winner_foreign_vacant} winner_foreign_replaced={winner_foreign_replaced} winner_foreign_lost={winner_foreign_lost} winner_vacant_share={:.4} authenticated_write_calls={authenticated_write_calls} authenticated_write_calls_per_output={:.3}",
        epoch_dirty_local as f64 / per_column,
        epoch_dirty_spill as f64 / per_column,
        override_attempts as f64 / per_column,
        override_insertions as f64 / per_column,
        carver_attempts as f64 / per_column,
        carver_insertions as f64 / per_column,
        canonical_winner_updates as f64 / per_column,
        winner_vacant as f64 / winner_attempts.max(1) as f64,
        authenticated_write_calls as f64 / per_column,
    );
}

fn request_for(coordinate: (i32, i32)) -> GenerationRequest {
    GenerationRequest::new(
        WorldgenDimension::Overworld,
        coordinate,
        GenerationTarget::Full,
        1,
    )
}

fn output_checksums(columns: &[((i32, i32), ChunkColumn)]) -> [u64; 3] {
    let mut blocks = DefaultHasher::new();
    let mut biomes = DefaultHasher::new();
    let mut heightmaps = DefaultHasher::new();
    for (coordinate, column) in columns {
        for digest in [&mut blocks, &mut biomes, &mut heightmaps] {
            coordinate.hash(digest);
            column.min_y.hash(digest);
            column.height.hash(digest);
        }
        for y in column.min_y..column.min_y + column.height {
            for z in 0..16 {
                for x in 0..16 {
                    column.block_state_id(x, y, z).raw().hash(&mut blocks);
                }
            }
        }
        for qy in 0..column.biome_y_quarts() {
            for qz in 0..4 {
                for qx in 0..4 {
                    column.biome_cell(qx, qy, qz).hash(&mut biomes);
                }
            }
        }
        column.client_heightmaps_raw().hash(&mut heightmaps);
    }
    [blocks.finish(), biomes.finish(), heightmaps.finish()]
}

#[test]
fn output_checksum_detects_a_changed_block() {
    let mut columns = [((3, -7), ChunkColumn::new(-64, 16))];
    let before = output_checksums(&columns);
    columns[0].1.set_block_id(5, -61, 9, lodestone_data::block::Block::Stone.default_state());
    let after = output_checksums(&columns);
    assert_ne!(before[0], after[0]);
    assert_eq!(before[1..], after[1..]);
}

#[test]
fn singleton_padding_settlement_matches_one_batch_and_cache() {
    let coordinates = (20_000..20_006)
        .map(|x| (x, -20_000))
        .collect::<Vec<_>>();
    let single = lodestone_server::retained_chunk_source_for_view_radius(
        Arc::new(overworld_chunk_source(42)),
        2,
    );
    let mut single_columns = Vec::new();
    for &coordinate in &coordinates {
        let GenerationRequestResult::Generated(snapshot) =
            expect_generated(request_one(&single, request_for(coordinate)), coordinate)
        else {
            unreachable!()
        };
        let column = snapshot.column().clone();
        let cached = single.column(coordinate.0, coordinate.1);
        assert_eq!(
            output_checksums(&[(coordinate, column.clone())]),
            output_checksums(&[(coordinate, cached)]),
            "packet snapshot and cache differ at {coordinate:?}"
        );
        single_columns.push((coordinate, column));
    }

    let batched = lodestone_server::retained_chunk_source_for_view_radius(
        Arc::new(overworld_chunk_source(42)),
        2,
    );
    let mut sessions = coordinates
        .iter()
        .copied()
        .map(request_for)
        .map(GenerationSession::new)
        .collect::<Vec<_>>();
    let batch_columns = coordinates
        .iter()
        .copied()
        .zip(batched.request_generation_batch(&mut sessions))
        .map(|(coordinate, result)| {
            let GenerationRequestResult::Generated(snapshot) = expect_generated(result, coordinate)
            else {
                unreachable!()
            };
            (coordinate, snapshot.column().clone())
        })
        .collect::<Vec<_>>();
    assert_eq!(output_checksums(&single_columns), output_checksums(&batch_columns));
    assert_eq!(
        single_columns[5].1.block_state_id(0, 32, 0),
        lodestone_data::block_states::StateId::from_raw(4)
    );
}

fn request_one<S: ChunkSource>(
    source: &S,
    request: GenerationRequest,
) -> Result<Option<GenerationRequestResult>, lodestone_server::worldgen_session::GenerationRequestError> {
    source.request_generation(request, None)
}

const CALIBRATION_ROUNDS: u64 = 100_000;

#[inline(never)]
fn calibration_kernel(mut state: u64) -> u64 {
    for round in 0..CALIBRATION_ROUNDS {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(round ^ 1_442_695_040_888_963_407);
    }
    black_box(state)
}

fn expect_generated(
    result: Result<Option<GenerationRequestResult>, lodestone_server::worldgen_session::GenerationRequestError>,
    coordinate: (i32, i32),
) -> GenerationRequestResult {
    match result
        .unwrap_or_else(|error| panic!("production generation request {coordinate:?} failed: {error}"))
        .unwrap_or_else(|| panic!("production source returned no snapshot for {coordinate:?}"))
    {
        GenerationRequestResult::Generated(snapshot) => GenerationRequestResult::Generated(snapshot),
        GenerationRequestResult::Existing(_) => {
            panic!("fresh benchmark coordinate unexpectedly came from persistence")
        }
    }
}

fn request_cohort(
    source: &dyn ChunkSource,
    sessions: &mut [GenerationSession],
    on_output: &mut dyn FnMut(),
) -> Vec<
    Result<
        Option<GenerationRequestResult>,
        lodestone_server::worldgen_session::GenerationRequestError,
    >,
> {
    let mut emitted = (0..sessions.len()).map(|_| None).collect::<Vec<_>>();
    let statuses = source.request_generation_cohort(sessions, &mut |index, _, result| {
        on_output();
        let Some(slot) = emitted.get_mut(index) else {
            return Err(lodestone_server::worldgen_session::GenerationRequestError::Boundary(
                format!("cohort emitted out-of-range index {index}"),
            ));
        };
        if slot.replace(result).is_some() {
            return Err(lodestone_server::worldgen_session::GenerationRequestError::Boundary(
                format!("cohort emitted index {index} more than once"),
            ));
        }
        Ok(())
    });
    let statuses = match statuses {
        Ok(statuses) if statuses.len() == sessions.len() => statuses,
        Ok(statuses) => {
            let message = format!(
                "cohort returned {} statuses for {} sessions",
                statuses.len(),
                sessions.len()
            );
            return (0..sessions.len())
                .map(|_| {
                    Err(lodestone_server::worldgen_session::GenerationRequestError::Boundary(
                        message.clone(),
                    ))
                })
                .collect();
        }
        Err(error) => {
            let message = error.to_string();
            return (0..sessions.len())
                .map(|_| {
                    Err(lodestone_server::worldgen_session::GenerationRequestError::Boundary(
                        message.clone(),
                    ))
                })
                .collect();
        }
    };

    statuses
        .into_iter()
        .enumerate()
        .map(|(index, status)| match (status, emitted[index].take()) {
            (Ok(()), Some(result)) => Ok(Some(result)),
            (Ok(()), None) => Err(
                lodestone_server::worldgen_session::GenerationRequestError::Boundary(format!(
                    "cohort did not emit successful index {index}"
                )),
            ),
            (Err(error), None) => Err(error),
            (Err(_), Some(_)) => Err(
                lodestone_server::worldgen_session::GenerationRequestError::Boundary(format!(
                    "cohort emitted failed index {index}"
                )),
            ),
        })
        .collect()
}

#[test]
#[ignore = "production benchmark; run on a quiet machine with LODESTONE_WORLDGEN_WORKERS=1"]
fn strict_single_thread_production_worldgen() {
    assert_eq!(
        std::env::var("LODESTONE_WORLDGEN_WORKERS").as_deref(),
        Ok("1"),
        "set LODESTONE_WORLDGEN_WORKERS=1 so this benchmark cannot silently use multiple workers"
    );
    let seed = std::env::var("LODESTONE_WORLDGEN_BENCH_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(42_i64);
    let count = parse("LODESTONE_WORLDGEN_BENCH_COLUMNS", 8);
    assert!(count > 0, "benchmark must have at least one target");
    let batch_size = parse("LODESTONE_WORLDGEN_BENCH_BATCH", 2);
    assert!(batch_size > 0, "benchmark batch must contain at least one target");
    let cohort_mode = std::env::var("LODESTONE_WORLDGEN_BENCH_COHORT").as_deref() == Ok("1");
    let sustained_phase = if cohort_mode {
        "sustained_cohort"
    } else {
        "sustained_batch"
    };
    let requested_layout = std::env::var("LODESTONE_WORLDGEN_BENCH_LAYOUT")
        .unwrap_or_else(|_| "line".to_owned());
    let layout = match requested_layout.as_str() {
        "line" => "line",
        "square" => "square",
        "ring" => "ring",
        other => panic!("unsupported benchmark layout {other}"),
    };
    let production_only =
        std::env::var("LODESTONE_WORLDGEN_BENCH_PHASES").as_deref() == Ok("production");

    let calibration = measure_request(retired(), || calibration_kernel(0x1234_5678_9abc_def0));
    report("pmu_calibration", "fixed_inline_never", &calibration, 1, 1, "fixed");
    assert_ne!(calibration.value, 0x1234_5678_9abc_def0);

    let construction = Instant::now();
    let base_source = Arc::new(overworld_chunk_source(seed));
    let source = lodestone_server::retained_chunk_source_for_view_radius(
        Arc::clone(&base_source),
        2,
    );
    println!(
        "STRICT_WORLDGEN metric=construction phase=setup layout={layout} batch_size={batch_size} elapsed_ms={:.3} seed={seed} workers={} columns={count}",
        construction.elapsed().as_secs_f64() * 1000.0,
        std::env::var("LODESTONE_WORLDGEN_WORKERS").unwrap_or_default(),
    );

    if !production_only {
        let cold_coordinate = (10_000, -10_000);
        let cold_request = request_for(cold_coordinate);
        let cold = measure_request(retired(), || request_one(&source, cold_request));
        report("production_request", "cold", &cold, 1, 1, "single");
        let cold_result = expect_generated(cold.value, cold_coordinate);
        let cold_column = match cold_result {
            GenerationRequestResult::Generated(snapshot) => snapshot.column().clone(),
            GenerationRequestResult::Existing(_) => unreachable!(),
        };
        assert_eq!(
            cold_column.generation_stage(),
            lodestone_server::ChunkGenerationStage::Full
        );
        black_box(cold_column);

        let retained =
            measure_request(retired(), || request_one(&source, request_for(cold_coordinate)));
        report("retained_target_control", "immediate", &retained, 1, 1, "single");
        assert!(
            matches!(
                retained.value
                    .expect("retained request must succeed")
                    .expect("retained source must return a result"),
                GenerationRequestResult::Existing(_)
            ),
            "immediate repeat must use the retained target"
        );
    }

    if layout == "ring" {
        let center = (20_000, -20_000);
        let primed = measure_request(retired(), || request_one(&source, request_for(center)));
        report("production_request", "primed_center", &primed, 1, 1, layout);
        black_box(expect_generated(primed.value, center));
    }

    let coordinates: Vec<(i32, i32)> = match layout {
        "square" => {
            let side = (count as f64).sqrt() as usize;
            assert_eq!(side * side, count, "square layout needs a perfect-square column count");
            (0..side)
                .flat_map(|z| (0..side).map(move |x| (20_000 + x as i32, -20_000 + z as i32)))
                .collect()
        }
        "ring" => {
            let mut coordinates = Vec::with_capacity(count);
            for radius in 1i32.. {
                for dz in -radius..=radius {
                    for dx in -radius..=radius {
                        if dx.abs().max(dz.abs()) == radius {
                            coordinates.push((20_000 + dx, -20_000 + dz));
                            if coordinates.len() == count {
                                break;
                            }
                        }
                    }
                    if coordinates.len() == count {
                        break;
                    }
                }
                if coordinates.len() == count {
                    break;
                }
            }
            coordinates
        }
        _ => (0..count)
            .map(|index| (20_000 + index as i32, -20_000))
            .collect(),
    };
    let session_initialization = measure_request(retired(), || {
        coordinates
            .chunks(batch_size)
            .map(|batch| {
                batch
                    .iter()
                    .copied()
                    .map(request_for)
                    .map(GenerationSession::new)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    });
    report(
        "session_initialization",
        "sustained_batch",
        &session_initialization,
        count,
        batch_size,
        layout,
    );
    let mut session_batches = session_initialization.value;
    let counters_enabled = counters::enabled()
        && std::env::var("LODESTONE_WORLDGEN_BENCH_COUNTERS").as_deref() == Ok("1");
    if counters_enabled {
        counters::reset();
    }
    let generation_counter_before = counters_enabled.then(counters::snapshot);
    base_source.generator().reset_store_lease_stats();
    #[cfg(feature = "worldgen-stage-pmu")]
    install_stage_pmu();
    let sustained_started = Instant::now();
    let mut first_output_latency = None;
    let sustained = measure_request(retired(), || {
        let mut results = Vec::with_capacity(count);
        for sessions in &mut session_batches {
            if cohort_mode {
                results.extend(request_cohort(&source, sessions, &mut || {
                    if first_output_latency.is_none() {
                        first_output_latency = Some(sustained_started.elapsed());
                    }
                }));
            } else {
                results.extend(source.request_generation_batch(sessions));
                if first_output_latency.is_none() {
                    first_output_latency = Some(sustained_started.elapsed());
                }
            }
        }
        results
    });
    println!(
        "STRICT_WORLDGEN metric=first_output phase={sustained_phase} layout={layout} batch_size={batch_size} columns={count} elapsed_ms={:.3}",
        first_output_latency
            .expect("generation must produce a first output")
            .as_secs_f64()
            * 1000.0,
    );
    let lease_stats = base_source.generator().store_lease_stats();
    println!(
        "STRICT_WORLDGEN metric=generator_leases phase={sustained_phase} layout={layout} batch_size={batch_size} columns={count} opens={} batch_opens={} pins={} pin_shards={} unpins={} unpin_shards={}",
        lease_stats.opens,
        lease_stats.batch_opens,
        lease_stats.pins,
        lease_stats.pin_shards,
        lease_stats.unpins,
        lease_stats.unpin_shards,
    );
    assert!(lease_stats.opens > 0, "sustained batch must admit through the generator lease");
    let expected_batch_groups = count.div_ceil(batch_size);
    assert!(
        lease_stats.batch_opens <= expected_batch_groups as u64,
        "production admission opened more union leases than submitted batches",
    );
    assert_eq!(sustained.value.len(), count, "every request needs a result");
    for (coordinate, result) in coordinates.iter().copied().zip(&sustained.value) {
        match result {
            Ok(Some(_)) => {}
            Ok(None) => panic!("production source returned no snapshot for {coordinate:?}"),
            Err(error) => panic!("production generation request {coordinate:?} failed: {error}"),
        }
    }
    report(
        "production_request",
        sustained_phase,
        &sustained,
        count,
        batch_size,
        layout,
    );
    #[cfg(target_os = "macos")]
    {
        let usage = rusage();
        println!(
            "STRICT_WORLDGEN metric=memory phase={sustained_phase} layout={layout} batch_size={batch_size} columns={count} current_bytes={} peak_bytes={}",
            usage.ri_phys_footprint,
            usage.ri_lifetime_max_phys_footprint,
        );
    }
    report_generation_counters(
        generation_counter_before,
        counters_enabled.then(counters::snapshot),
        count,
        sustained_phase,
    );
    #[cfg(all(feature = "worldgen-stage-pmu", target_os = "macos"))]
    report_stage_pmu(&sustained, count, sustained_phase);
    let mut columns = Vec::<((i32, i32), ChunkColumn)>::with_capacity(count);
    let mut generated_count = 0usize;
    let mut promoted_count = 0usize;
    for (coordinate, result) in coordinates.iter().copied().zip(sustained.value) {
        let result = result
            .unwrap_or_else(|error| {
                panic!("production generation request {coordinate:?} failed: {error}")
            })
            .unwrap_or_else(|| panic!("production source returned no snapshot for {coordinate:?}"));
        let column = match result {
            GenerationRequestResult::Generated(snapshot) => {
                generated_count += 1;
                snapshot.column().clone()
            }
            GenerationRequestResult::Existing(column) => {
                promoted_count += 1;
                column
            }
        };
        assert_eq!(
            column.generation_stage(),
            lodestone_server::ChunkGenerationStage::Full
        );
        columns.push((coordinate, column));
    }
    println!(
        "STRICT_WORLDGEN metric=request_results phase={sustained_phase} layout={layout} batch_size={batch_size} columns={count} generated={generated_count} promoted={promoted_count}"
    );
    assert_eq!(generated_count + promoted_count, count);
    let [blocks, biomes, heightmaps] = output_checksums(&columns);
    println!(
        "STRICT_WORLDGEN metric=output_checksum phase={sustained_phase} layout={layout} batch_size={batch_size} columns={count} blocks={blocks:016x} biomes={biomes:016x} heightmaps={heightmaps:016x}"
    );
    if let Ok(compare_batch_size) = std::env::var("LODESTONE_WORLDGEN_BENCH_COMPARE_BATCH") {
        let compare_batch_size = compare_batch_size.parse::<usize>().expect("compare batch size");
        assert!(compare_batch_size > 0);
        let compare_base = Arc::new(overworld_chunk_source(seed));
        let compare_source = lodestone_server::retained_chunk_source_for_view_radius(
            compare_base,
            2,
        );
        if layout == "ring" {
            let center = (20_000, -20_000);
            black_box(expect_generated(
                request_one(&compare_source, request_for(center)),
                center,
            ));
        }
        let mut compared = Vec::with_capacity(count);
        for batch in coordinates.chunks(compare_batch_size) {
            let mut sessions = batch
                .iter()
                .copied()
                .map(request_for)
                .map(GenerationSession::new)
                .collect::<Vec<_>>();
            for (coordinate, result) in batch.iter().copied().zip(
                compare_source.request_generation_batch(&mut sessions),
            ) {
                let result = result
                    .unwrap_or_else(|error| panic!("comparison {coordinate:?}: {error}"))
                    .expect("comparison must return a column");
                let column = match result {
                    GenerationRequestResult::Generated(snapshot) => snapshot.column().clone(),
                    GenerationRequestResult::Existing(column) => column,
                };
                compared.push((coordinate, column));
            }
        }
        let [compare_blocks, compare_biomes, compare_heightmaps] = output_checksums(&compared);
        println!(
            "STRICT_WORLDGEN metric=comparison_checksum layout={layout} batch_size={batch_size} compare_batch_size={compare_batch_size} blocks={compare_blocks:016x} biomes={compare_biomes:016x} heightmaps={compare_heightmaps:016x}"
        );
        let mut differences = 0usize;
        for ((coordinate, column), (other_coordinate, other)) in columns.iter().zip(&compared) {
            assert_eq!(coordinate, other_coordinate);
            for y in column.min_y..column.min_y + column.height {
                for z in 0..16 {
                    for x in 0..16 {
                        let left = column.block_state_id(x, y, z);
                        let right = other.block_state_id(x, y, z);
                        if left != right {
                            if differences < 32 {
                                println!(
                                    "STRICT_WORLDGEN metric=comparison_cell coordinate={coordinate:?} local=({x},{y},{z}) batch_state={} batch_block={} compare_state={} compare_block={}",
                                    left.raw(),
                                    left.name(),
                                    right.raw(),
                                    right.name(),
                                );
                            }
                            differences += 1;
                        }
                    }
                }
            }
        }
        println!("STRICT_WORLDGEN metric=comparison_differences cells={differences}");
        if compare_batch_size == batch_size {
            assert_eq!(differences, 0, "identical batch widths must reproduce block output");
            assert_eq!(
                [blocks, biomes, heightmaps],
                [compare_blocks, compare_biomes, compare_heightmaps],
                "identical batch widths must reproduce every output checksum"
            );
        }
    }
    if production_only {
        black_box(columns);
        return;
    }

    let fresh_source = overworld_chunk_source(seed);
    let fresh = measure_request(retired(), || {
        coordinates
            .iter()
            .map(|&(cx, cz)| fresh_source.column(cx, cz))
            .collect::<Vec<_>>()
    });
    report("fresh_column", "pure", &fresh, count, 1, layout);
    for column in fresh.value {
        assert_eq!(column.generation_stage(), lodestone_server::ChunkGenerationStage::Full);
        black_box(column);
    }

    let source_once_source = overworld_chunk_source(seed);
    if counters_enabled {
        counters::reset();
    }
    let source_once_counter_before = counters_enabled.then(counters::snapshot);
    let source_once = measure_request(retired(), || {
        source_once_source
            .generator()
            .source_once_features(&coordinates)
    });
    report(
        "source_once_features",
        "experimental",
        &source_once,
        count,
        batch_size,
        layout,
    );
    report_generation_counters(
        source_once_counter_before,
        counters_enabled.then(counters::snapshot),
        count,
        "source_once",
    );
    let expected_sources = match layout {
        "square" => {
            let side = (count as f64).sqrt() as usize;
            (side + 2) * (side + 2)
        }
        "ring" => coordinates
            .iter()
            .flat_map(|&(x, z)| {
                (-1..=1).flat_map(move |dz| (-1..=1).map(move |dx| (x + dx, z + dz)))
            })
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        _ => 3 * (count + 2),
    };
    assert_eq!(source_once.value.source_execution_count(), expected_sources);
    println!(
        "STRICT_WORLDGEN metric=source_once_products phase=source_once layout={layout} requested_products={} mutable_products={} context_products=unavailable heavy_products={} mutable_writes={} retained_bytes={} padding_mutations={}",
        source_once.value.requested().len(),
        source_once.value.mutable_write_count(),
        source_once.value.source_execution_count(),
        source_once.value.mutable_write_count(),
        source_once.value.region_retained_bytes(),
        source_once.value.padding_mutations().len(),
    );
    black_box(source_once.value);

    let protocol = V770ServerProtocol;
    let before = retired();
    let started = Instant::now();
    for &((cx, cz), ref column) in &columns {
        let packet = protocol
            .try_encode_chunk_in_dimension(cx, cz, column, ServerDimension::Overworld)
            .expect("production chunk light and encoding must succeed");
        black_box(packet);
    }
    let measurement = Measurement {
        value: (),
        elapsed: started.elapsed(),
        counters: before.zip(retired()).map(|(before, after)| {
            (
                after.0.saturating_sub(before.0),
                after.1.saturating_sub(before.1),
            )
        }),
    };
    report("light_encode", "post_generation", &measurement, columns.len(), 1, layout);
}

#[test]
#[ignore = "source-once worldgen benchmark"]
fn strict_single_thread_source_once_worldgen() {
    assert_eq!(
        std::env::var("LODESTONE_WORLDGEN_WORKERS").as_deref(),
        Ok("1")
    );
    let seed = std::env::var("LODESTONE_WORLDGEN_BENCH_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(42_i64);
    let count = parse("LODESTONE_WORLDGEN_BENCH_COLUMNS", 64);
    let side = (count as f64).sqrt() as usize;
    assert_eq!(side * side, count, "source-once benchmark needs a square count");
    let coordinates = (0..side)
        .flat_map(|z| (0..side).map(move |x| (20_000 + x as i32, -20_000 + z as i32)))
        .collect::<Vec<_>>();
    let source = overworld_chunk_source(seed);
    let capture = std::env::var("LODESTONE_WORLDGEN_BENCH_CAPTURE").as_deref() == Ok("1");
    let counters_enabled = counters::enabled()
        && std::env::var("LODESTONE_WORLDGEN_BENCH_COUNTERS").as_deref() == Ok("1");
    if counters_enabled {
        counters::reset();
    }
    let counter_before = counters_enabled.then(counters::snapshot);
    let measurement = measure_request(retired(), || {
        source
            .generator()
            .source_once_features_with_capture(&coordinates, capture)
    });
    report(
        "source_once_features",
        if capture { "capture" } else { "isolated" },
        &measurement,
        count,
        count,
        "square",
    );
    report_generation_counters(
        counter_before,
        counters_enabled.then(counters::snapshot),
        count,
        "source_once",
    );
    println!(
        "STRICT_WORLDGEN metric=source_once_products phase=source_once capture={capture} requested_products={} mutable_products={} context_products=unavailable heavy_products={} sources={} final_mutations={} mutable_writes={} retained_bytes={} padding_mutations={}",
        measurement.value.requested().len(),
        measurement.value.mutable_write_count(),
        measurement.value.source_execution_count(),
        measurement.value.source_execution_count(),
        measurement.value.global_overrides().len(),
        measurement.value.mutable_write_count(),
        measurement.value.region_retained_bytes(),
        measurement.value.padding_mutations().len(),
    );
    black_box(measurement.value);
}
