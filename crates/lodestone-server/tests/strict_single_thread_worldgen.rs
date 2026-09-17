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
use std::sync::Arc;
use std::time::{Duration, Instant};

use lodestone_server::worldgen_session::{
    GenerationRequest, GenerationRequestResult, GenerationSession,
};
use lodestone_server::{ChunkColumn, ChunkSource, ServerProtocol, overworld_chunk_source};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_server::dimension::Dimension as ServerDimension;
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
fn retired() -> Option<(u64, u64)> {
    let mut usage = RusageInfoV4::default();
    let result = unsafe {
        proc_pid_rusage(
            i32::try_from(std::process::id()).expect("pid fits in i32"),
            4,
            (&raw mut usage).cast::<core::ffi::c_void>(),
        )
    };
    assert_eq!(result, 0, "proc_pid_rusage failed with {result}");
    Some((usage.ri_instructions, usage.ri_cycles))
}

#[cfg(not(target_os = "macos"))]
fn retired() -> Option<(u64, u64)> {
    None
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

fn request_for(coordinate: (i32, i32)) -> GenerationRequest {
    GenerationRequest::new(
        WorldgenDimension::Overworld,
        coordinate,
        GenerationTarget::Full,
        1,
    )
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
    let layout = if std::env::var("LODESTONE_WORLDGEN_BENCH_LAYOUT").as_deref() == Ok("square") {
        "square"
    } else {
        "line"
    };

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

    let retained = measure_request(retired(), || request_one(&source, request_for(cold_coordinate)));
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

    let coordinates: Vec<(i32, i32)> = if std::env::var("LODESTONE_WORLDGEN_BENCH_LAYOUT").as_deref()
        == Ok("square")
    {
        let side = (count as f64).sqrt() as usize;
        assert_eq!(side * side, count, "square layout needs a perfect-square column count");
        (0..side)
            .flat_map(|z| (0..side).map(move |x| (20_000 + x as i32, -20_000 + z as i32)))
            .collect()
    } else {
        (0..count)
            .map(|index| (20_000 + index as i32, -20_000))
            .collect()
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
    base_source.generator().reset_store_lease_stats();
    let sustained = measure_request(retired(), || {
        let mut results = Vec::with_capacity(count);
        for sessions in &mut session_batches {
            results.extend(source.request_generation_batch(sessions));
        }
        results
    });
    let lease_stats = base_source.generator().store_lease_stats();
    println!(
        "STRICT_WORLDGEN metric=generator_leases phase=sustained_batch layout={layout} batch_size={batch_size} columns={count} opens={} batch_opens={} pins={} pin_shards={} unpins={} unpin_shards={}",
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
    report(
        "production_request",
        "sustained_batch",
        &sustained,
        count,
        batch_size,
        layout,
    );
    let mut columns = Vec::<((i32, i32), ChunkColumn)>::with_capacity(count);
    for (coordinate, result) in coordinates.iter().copied().zip(sustained.value) {
        let result = expect_generated(result, coordinate);
        let column = match result {
            GenerationRequestResult::Generated(snapshot) => snapshot.column().clone(),
            GenerationRequestResult::Existing(_) => unreachable!(),
        };
        assert_eq!(
            column.generation_stage(),
            lodestone_server::ChunkGenerationStage::Full
        );
        columns.push((coordinate, column));
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
