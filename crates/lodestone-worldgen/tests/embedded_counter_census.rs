//! One-column census of the embedded production generator.
//!
//! This is intentionally an ignored diagnostic rather than a benchmark: it
//! keeps all counter families visible in one report without Criterion, LTO, or
//! a second harness changing the workload. The seed control also makes sure a
//! change to the generator input reaches both the output and the counters.

#![cfg(feature = "gen-counters")]

use lodestone_worldgen::counters::{self, Snapshot, Stage};
use lodestone_worldgen::overworld::GeneratedColumn;
use sha2::{Digest, Sha256};

const SEED: i64 = 42;
const CONTROL_SEED: i64 = 43;

fn column_digest(column: &GeneratedColumn) -> [u8; 32] {
    let mut digest = Sha256::new();
    for y in column.min_y()..column.min_y() + column.height() {
        for lz in 0..16 {
            for lx in 0..16 {
                digest.update(column.block_state_id(lx, y, lz).raw().to_le_bytes());
            }
        }
    }
    digest.finalize().into()
}

fn run(seed: i64) -> (GeneratedColumn, Snapshot) {
    counters::reset();
    lodestone_worldgen::overworld::structures::reset_structure_cache_stats();
    let generator = lodestone_server::overworld_generator(seed);
    let column = generator.column(0, 0);
    let snapshot = counters::snapshot();
    println!(
        "COUNTER_CENSUS structure_cache={:?}",
        lodestone_worldgen::overworld::structures::structure_cache_stats()
    );
    println!("COUNTER_CENSUS lease={:?}", generator.store_lease_stats());
    (column, snapshot)
}

fn print_snapshot(snapshot: &Snapshot) {
    println!("COUNTER_CENSUS cache_hits={:?}", snapshot.cache_hits);
    println!("COUNTER_CENSUS cache_misses={:?}", snapshot.cache_misses);
    println!("COUNTER_CENSUS cache_computes={:?}", snapshot.cache_computes);
    println!("COUNTER_CENSUS cache_evictions={:?}", snapshot.cache_evictions);
    println!("COUNTER_CENSUS logical_reads={:?}", snapshot.logical_reads);
    println!("COUNTER_CENSUS logical_writes={:?}", snapshot.logical_writes);
    println!("COUNTER_CENSUS logical_read_bytes={:?}", snapshot.logical_read_bytes);
    println!("COUNTER_CENSUS logical_write_bytes={:?}", snapshot.logical_write_bytes);
    println!(
        "COUNTER_CENSUS scratch reuse={} allocation={} eviction={} allocated_bytes={} retained={} high_water={}",
        snapshot.scratch_pool_reuses,
        snapshot.scratch_pool_allocations,
        snapshot.scratch_pool_evictions,
        snapshot.scratch_buffer_allocated_bytes,
        snapshot.scratch_retained_bytes,
        snapshot.scratch_retained_bytes_high_water
    );
    println!(
        "COUNTER_CENSUS scans={} scan_cells={} conversions={} conversion_cells={}",
        snapshot.full_column_scans,
        snapshot.full_column_scan_cells,
        snapshot.full_column_conversions,
        snapshot.full_column_conversion_cells
    );
    println!(
        "COUNTER_CENSUS recomputation pre_ore_computed={} pre_ore_hits={} preliminary_requests={} preliminary_unique={} preliminary_computations={} structure_starts={} structure_aquifers={} structure_height_probes={}",
        snapshot.pre_ore_computed,
        snapshot.pre_ore_hits,
        snapshot.preliminary_surface_requests,
        snapshot.preliminary_surface_unique,
        snapshot.preliminary_surface_computations,
        snapshot.structure_starts_computed,
        snapshot.structure_aquifers_built,
        snapshot.structure_height_probes
    );
    println!(
        "COUNTER_CENSUS blocks={} density_evals={} density_point_computes={} biome_searches={} biome_rows_compared={} rng_draws={} stage_entered={:?}",
        snapshot.block_at,
        snapshot.density_evals_total(),
        snapshot.density_point_computes_total(),
        snapshot.biome_searches,
        snapshot.biome_rows_compared,
        snapshot.rng_draws_total(),
        snapshot.stage_entered
    );
    println!(
        "COUNTER_CENSUS density_evals_ranked={:?}",
        snapshot.density_evals_ranked()
    );
    println!(
        "COUNTER_CENSUS corners lookups={} fills={} evals={} noise_batches={} slot_hits={} slot_misses={} palette_new={} palette_hits={} stitch_cells={} state_intern_new={} state_name_lookups={}",
        snapshot.corner_lookups,
        snapshot.cell_fills,
        snapshot.corner_evals,
        snapshot.noise_corner_batches,
        snapshot.slot_hits,
        snapshot.slot_misses,
        snapshot.palette_intern_new,
        snapshot.palette_intern_hit,
        snapshot.stitch_cells,
        snapshot.state_intern_new,
        snapshot.state_name_lookups
    );
    println!(
        "COUNTER_CENSUS structure_context block_at={} replaceable={} kind={}",
        snapshot.structure_context_block_at,
        snapshot.structure_context_replaceable_block_at,
        snapshot.structure_context_kind_block_at
    );
}

#[test]
#[ignore = "embedded production generation; run explicitly with --ignored"]
fn embedded_overworld_column_reports_live_counter_census() {
    let (column, snapshot) = run(SEED);
    let (control, control_snapshot) = run(CONTROL_SEED);
    let digest = column_digest(&column);
    let control_digest = column_digest(&control);

    assert_ne!(digest, control_digest, "seed control did not affect generated blocks");
    assert!(snapshot.block_at > 0, "embedded column did not reach terrain queries");
    assert!(snapshot.density_evals_total() > 0, "embedded column did not evaluate density");
    assert!(snapshot.full_column_scans > 0, "embedded column did not report materialization scans");
    assert!(snapshot.full_column_conversions > 0, "embedded column did not report conversion");
    assert!(snapshot.stage_entered[Stage::Intern as usize] == 1, "column must intern once");
    assert!(control_snapshot.block_at > 0, "input control did not reach terrain queries");

    println!("COUNTER_CENSUS seed={SEED} digest={digest:02x?}");
    print_snapshot(&snapshot);
    println!("COUNTER_CENSUS control_seed={CONTROL_SEED} digest={control_digest:02x?}");
    print_snapshot(&control_snapshot);
}
