//! Finite, headless Samply input for the distant-terrain horizon.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let report = lodestone::horizon_profile::run_horizon_profile(42)?;
    println!(
        "horizon-profile phase=far-columns requested={} shaped={} full={} solid_blocks={} store_entries={} store_evictions={}",
        report.far_columns,
        report.shaped_columns,
        report.full_columns,
        report.far_solid_blocks,
        report.staged_store_entries,
        report.staged_store_evictions,
    );
    println!(
        "horizon-profile phase=horizon candidates={} tiles_updated={} tiles_skipped={} cells_written={} atlas_cpu_bytes={} atlas_gpu_bytes={}",
        report.horizon_candidates,
        report.horizon_tiles_updated,
        report.horizon_tiles_skipped,
        report.horizon_cells_written,
        report.atlas_cpu_bytes,
        report.atlas_gpu_bytes,
    );
    Ok(())
}
