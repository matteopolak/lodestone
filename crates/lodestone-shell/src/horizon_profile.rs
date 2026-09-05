//! Bounded headless work for profiling the distant-terrain horizon.
//!
//! This runs the fixed-tile updates used by the renderer and asks the bundled
//! chunk source for a finite square of reduced far columns. It deliberately
//! does not create a GPU device, interactive window, or network connection. It
//! is a Samply input, not a timing regression test.

use lodestone_render::{
    DistantTerrain, HORIZON_CELL_BLOCKS, HORIZON_CELLS_PER_TILE, HORIZON_TILE_BLOCKS,
    HORIZON_TILE_CELLS, MAX_HORIZON_BYTES, MAX_HORIZON_TILES, horizon_tile_intersects_radius,
};

use lodestone_server::{
    ChunkGenerationStage, ChunkSource, overworld_chunk_source,
};

/// The largest supported visual radius exercised by this profiler input.
pub const PROFILE_HORIZON_DISTANCE_CHUNKS: u32 = 256;
/// A small, realistic streamed radius used only to label the excluded near field.
pub const PROFILE_NEAR_DISTANCE_CHUNKS: u32 = 12;
/// Tile updates per recenter step. This keeps a profiling capture short.
pub const PROFILE_TILE_UPDATE_BUDGET: usize = 6;
/// Number of reduced-generation far columns in the bounded workload.
pub const PROFILE_FAR_CHUNKS: usize = 256;
/// Side length of the square far-column sample (`16 × 16 = 256`).
pub const PROFILE_FAR_CHUNK_SIDE: usize = 16;

// This square sits near the supported 256-chunk horizon while staying inside
// the fixed 9 × 9 tile allocation around the origin camera. It is deliberately
// not a random walk: a finite spatially coherent input makes the staged-store
// closure visible in a profile without pretending it is a benchmark baseline.
const PROFILE_FAR_CHUNK_ORIGIN: (i32, i32) = (240, -8);

const PROFILE_STEPS: [([i32; 2], u32); 3] = [
    ([0, 0], 128),
    ([HORIZON_TILE_BLOCKS, -HORIZON_TILE_BLOCKS], 192),
    ([HORIZON_TILE_BLOCKS * 2, HORIZON_TILE_BLOCKS], PROFILE_HORIZON_DISTANCE_CHUNKS),
];

/// Counters emitted by one bounded distant-horizon profiling run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HorizonProfileReport {
    /// Reduced-generation columns requested from the far field.
    pub far_columns: usize,
    /// Columns returned by the staged `Shaped` path.
    pub shaped_columns: usize,
    /// Full columns observed (the profile rejects any non-zero value).
    pub full_columns: usize,
    /// Solid blocks observed in the reduced columns, proving the input is populated.
    pub far_solid_blocks: usize,
    /// Distinct staged-store entries retained after the far-column pass.
    pub staged_store_entries: usize,
    /// Staged-store evictions during the bounded pass.
    pub staged_store_evictions: usize,
    /// All fixed-grid tiles intersecting the three configured large-radius passes.
    pub horizon_candidates: usize,
    /// Tiles whose 64 by 64 cells were sampled and updated within the budget.
    pub horizon_tiles_updated: usize,
    /// Eligible tiles deferred after the per-step update budget was exhausted.
    pub horizon_tiles_skipped: usize,
    /// Horizon cells written by the bounded tile updates.
    pub horizon_cells_written: usize,
    /// Fixed CPU bytes reserved for the coarse horizon grid.
    pub atlas_cpu_bytes: usize,
    /// Fixed GPU texture bytes the corresponding renderer would reserve.
    pub atlas_gpu_bytes: usize,
}

/// Why the horizon profiling input could not run.
#[derive(Debug)]
pub enum HorizonProfileError {
    /// The fixed terrain grid could not be allocated.
    Allocation(lodestone_render::HorizonAllocationError),
    /// The staged workload returned a full column for a far request.
    UnexpectedFullColumns(usize),
}

impl std::fmt::Display for HorizonProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Allocation(error) => error.fmt(f),
            Self::UnexpectedFullColumns(count) => write!(
                f,
                "horizon profiling returned {count} full columns for reduced far requests"
            ),
        }
    }
}

impl std::error::Error for HorizonProfileError {}

impl From<lodestone_render::HorizonAllocationError> for HorizonProfileError {
    fn from(value: lodestone_render::HorizonAllocationError) -> Self {
        Self::Allocation(value)
    }
}

/// Runs the bounded 128-, 192-, and 256-chunk horizon workload.
///
/// The report is a witness of path coverage, not a wall-clock comparison. The
/// far pass requests exactly [`PROFILE_FAR_CHUNKS`] columns through the staged
/// `ChunkGenerationStage::Shaped` path, while each horizon step recentres the
/// same fixed 9 by 9 allocation, selects renderer-identical candidates, and
/// updates at most [`PROFILE_TILE_UPDATE_BUDGET`] tiles.
///
/// # Errors
/// Returns an error if the fixed grid cannot allocate or a far request returns
/// a full column instead of the reduced `Shaped` result.
pub fn run_horizon_profile(seed: i64) -> Result<HorizonProfileReport, HorizonProfileError> {
    let source = overworld_chunk_source(seed);
    let mut terrain = DistantTerrain::new(PROFILE_STEPS[0].0[0], PROFILE_STEPS[0].0[1])?;
    let mut report = HorizonProfileReport {
        far_columns: 0,
        shaped_columns: 0,
        full_columns: 0,
        far_solid_blocks: 0,
        staged_store_entries: 0,
        staged_store_evictions: 0,
        horizon_candidates: 0,
        horizon_tiles_updated: 0,
        horizon_tiles_skipped: 0,
        horizon_cells_written: 0,
        atlas_cpu_bytes: MAX_HORIZON_BYTES,
        atlas_gpu_bytes: MAX_HORIZON_BYTES,
    };

    for (camera_block, radius_chunks) in PROFILE_STEPS {
        terrain.recenter(camera_block[0], camera_block[1]);
        let radius_blocks = (radius_chunks * HORIZON_CELL_BLOCKS as u32) as f32;
        let candidates: Vec<_> = terrain
            .tiles()
            .enumerate()
            .filter_map(|(slot, tile)| {
                horizon_tile_intersects_radius(tile, camera_block, radius_blocks).then_some(slot)
            })
            .collect();
        report.horizon_candidates += candidates.len();
        report.horizon_tiles_skipped += candidates.len().saturating_sub(PROFILE_TILE_UPDATE_BUDGET);

        for slot in candidates.into_iter().take(PROFILE_TILE_UPDATE_BUDGET) {
            let tile = terrain
                .tiles_mut()
                .nth(slot)
                .expect("candidate slot indexes the fixed terrain grid");
            let (origin_x, origin_z) = tile.coord().block_origin();
            for z in 0..HORIZON_TILE_CELLS {
                for x in 0..HORIZON_TILE_CELLS {
                    let block_x = origin_x.saturating_add(x as i32 * HORIZON_CELL_BLOCKS);
                    let block_z = origin_z.saturating_add(z as i32 * HORIZON_CELL_BLOCKS);
                    let terrain_y = source.generator().preliminary_surface_level(block_x, block_z);
                    let water_y = (terrain_y < source.generator().sea_level())
                        .then_some(source.generator().sea_level());
                    assert!(tile.set_cell(
                        x,
                        z,
                        lodestone_render::HorizonCell {
                            terrain_y: terrain_y.saturating_add(64).clamp(0, i32::from(u16::MAX)) as u16,
                            water_y: water_y
                                .map(|y| y.saturating_add(64).clamp(0, i32::from(u16::MAX)) as u16)
                                .unwrap_or(lodestone_render::HorizonCell::DRY),
                            surface_rgb565: if water_y.is_some() { 0x2D9B } else { 0x5A85 },
                            flags: 0,
                        },
                    ));
                }
            }
            report.horizon_tiles_updated += 1;
            report.horizon_cells_written += HORIZON_TILE_CELLS * HORIZON_TILE_CELLS;
        }
    }

    for index in 0..PROFILE_FAR_CHUNKS {
        let (cx, cz) = profile_far_chunk(index).expect("the fixed far workload has 256 coordinates");
        let column = source.column_at(cx, cz, ChunkGenerationStage::Shaped);
        report.far_columns += 1;
        match column.generation_stage() {
            ChunkGenerationStage::Shaped => report.shaped_columns += 1,
            ChunkGenerationStage::Full => report.full_columns += 1,
        }
        report.far_solid_blocks += column.solid_count();
    }
    report.staged_store_entries = source.generator().store_len();
    report.staged_store_evictions = source.generator().store_evictions();
    if report.full_columns != 0 {
        return Err(HorizonProfileError::UnexpectedFullColumns(report.full_columns));
    }
    debug_assert_eq!(terrain.tile_count(), MAX_HORIZON_TILES);
    Ok(report)
}

/// Returns one coordinate from the fixed far-column square.
#[must_use]
pub const fn profile_far_chunk(index: usize) -> Option<(i32, i32)> {
    if index >= PROFILE_FAR_CHUNKS {
        return None;
    }
    let x = PROFILE_FAR_CHUNK_ORIGIN.0 + (index % PROFILE_FAR_CHUNK_SIDE) as i32;
    let z = PROFILE_FAR_CHUNK_ORIGIN.1 + (index / PROFILE_FAR_CHUNK_SIDE) as i32;
    Some((x, z))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn far_workload_is_exactly_256_populated_coordinates_outside_near_field() {
        let coordinates: Vec<_> = (0..PROFILE_FAR_CHUNKS)
            .map(|index| profile_far_chunk(index).expect("fixed coordinate"))
            .collect();
        assert_eq!(coordinates.len(), 256);
        assert_eq!(profile_far_chunk(PROFILE_FAR_CHUNKS), None);
        assert_eq!(coordinates.windows(2).filter(|pair| pair[0] == pair[1]).count(), 0);
        assert!(coordinates.iter().all(|&(cx, cz)| {
            cx.abs().max(cz.abs()) > PROFILE_NEAR_DISTANCE_CHUNKS as i32
        }));
    }

    #[test]
    fn horizon_profile_budget_and_atlas_are_fixed() {
        assert_eq!(PROFILE_FAR_CHUNKS, PROFILE_FAR_CHUNK_SIDE * PROFILE_FAR_CHUNK_SIDE);
        assert_eq!(PROFILE_STEPS.len() * PROFILE_TILE_UPDATE_BUDGET, 18);
        assert_eq!(MAX_HORIZON_BYTES, 2_654_208);
        assert_eq!(PROFILE_HORIZON_DISTANCE_CHUNKS, 256);
    }

    /// Execute the real bounded workload and verify that its far-column phase
    /// stayed on the reduced generation seam. This is deliberately ignored:
    /// it generates 256 real embedded-data columns and belongs to an explicit
    /// release profiling run, not the default shell unit-test pass.
    #[test]
    #[ignore = "real 256-column staged-generation profile; run the profile recipe explicitly"]
    fn profile_workload_proves_shaped_far_columns_and_real_horizon_work() {
        let report = run_horizon_profile(42).expect("bounded horizon profile should complete");
        assert_eq!(report.far_columns, PROFILE_FAR_CHUNKS);
        assert_eq!(report.shaped_columns, PROFILE_FAR_CHUNKS);
        assert_eq!(report.full_columns, 0, "far requests must not fall back to full columns");
        assert!(report.far_solid_blocks > 0, "the far-column fixture must contain terrain");
        assert!(report.horizon_tiles_updated > 0, "the horizon phase must update real tiles");
        assert!(report.horizon_tiles_skipped > 0, "the bounded tile budget must be observable");
        assert_eq!(
            report.horizon_cells_written,
            report.horizon_tiles_updated * HORIZON_CELLS_PER_TILE
        );
        assert_eq!(report.atlas_cpu_bytes, MAX_HORIZON_BYTES);
        assert_eq!(report.atlas_gpu_bytes, MAX_HORIZON_BYTES);
    }
}
