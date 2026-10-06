//! Shared chunk/column terrain fixtures for benchmark harnesses. The module
//! keeps the definition of "realistic terrain" in one place instead of
//! duplicating hand-rolled shapes across `lodestone-world`
//! (`tests/memory.rs`, `tests/pool_footprint.rs`,
//! `benches/light_propagation.rs`) and `lodestone-render`
//! (`tests/world_mesher_bench.rs`, `tests/scene_bench.rs`).
//!
//! [`synthetic_column`] / [`synthetic_overworld_column`] need no worldgen
//! dependency and no filesystem I/O: a stone floor, a varied surface band and
//! open sky above, the same public shape real terrain has. At `seed = 0` it
//! matches the established `realistic_terrain_column` shape used by the sites
//! above. Use it for benchmarks that need many columns cheaply and do not care
//! about worldgen fidelity (meshing, light propagation, memory/footprint).

use lodestone_world::{ChunkColumn, PaletteKind};

/// Overworld shape constant shared by the benchmark fixtures
/// (`lodestone-world/tests/memory.rs`,
/// `lodestone-world/benches/light_propagation.rs`,
/// `lodestone-world/tests/pool_footprint.rs`): 1.18+'s `y = -64..320`.
pub const MODERN_MIN_Y: i32 = -64;
pub const MODERN_SECTIONS: usize = 24;

/// A synthetic [`ChunkColumn`] at the same public shape real
/// terrain has -- a solid stone base, a *varied* surface band that forces
/// real per-cell differences (never a flat, uniform slab, because a
/// light/mesh benchmark over uniform terrain degenerates to near-O(1)
/// regardless of whether the algorithm under test is correct), and open sky
/// above.
///
/// The result is deterministic in `seed`: `seed = 0` reproduces the
/// established `realistic_terrain_column` shape used by
/// `lodestone-world/tests/memory.rs` and
/// `lodestone-world/benches/light_propagation.rs`. A different seed produces
/// a different, equally varied surface band, which is useful for a
/// non-uniform neighbourhood in a light or mesh benchmark spanning columns.
///
/// `min_y`/`sections` are explicit rather than defaulted to the modern
/// overworld shape ([`MODERN_MIN_Y`]/[`MODERN_SECTIONS`], see
/// [`synthetic_overworld_column`] for that convenience wrapper) so a
/// benchmark against a legacy world height is not forced through the modern
/// constant.
#[must_use]
pub fn synthetic_column(min_y: i32, sections: usize, seed: u64) -> ChunkColumn {
    let mut col = ChunkColumn::new(
        min_y,
        sections,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        0,
        0,
    );
    let stone = 1u32;
    // Stone floor: 104 levels, matching the reference fixture's `MIN_Y..40`
    // when `min_y == -64` (40 - (-64) == 104).
    let surface_start = min_y + 104;
    let surface_end = surface_start + 8;
    for y in min_y..surface_start {
        for z in 0..16 {
            for x in 0..16 {
                col.set_block(x, y, z, stone);
            }
        }
    }
    for y in surface_start..surface_end {
        for z in 0..16 {
            for x in 0..16 {
                // `rem_euclid`, not `%`, so this stays well-defined for a
                // `min_y` that puts the surface band at a negative `y`.
                // The reference shape uses positive `y` values (40..48),
                // where a `usize` cast would also be valid.
                let id = 1
                    + (i64::from(x as i32) + i64::from(z as i32) + i64::from(y) + seed as i64)
                        .rem_euclid(6) as u32;
                col.set_block(x, y, z, id);
            }
        }
    }
    col
}

/// [`synthetic_column`] at the modern overworld shape
/// ([`MODERN_MIN_Y`]/[`MODERN_SECTIONS`]) -- the convenience entry point most
/// callers want.
#[must_use]
pub fn synthetic_overworld_column(seed: u64) -> ChunkColumn {
    synthetic_column(MODERN_MIN_Y, MODERN_SECTIONS, seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_column_matches_the_original_hand_rolled_shape_at_seed_zero() {
        // The reference shape used by `lodestone-world/tests/memory.rs` and
        // `benches/light_propagation.rs` has stone through y=39, a varied
        // surface band 40..48, and air above. These assertions keep the
        // shared fixture compatible with that shape.
        let col = synthetic_overworld_column(0);
        assert_eq!(col.min_y(), MODERN_MIN_Y);

        // Below the surface band: solid stone (id 1).
        assert_eq!(col.get_block(0, 0, 0), 1);
        assert_eq!(col.get_block(15, 39, 15), 1);

        // Surface band: the reference formula is
        // `1 + ((x + z + y as usize) % 6)` for y in 40..48.
        for y in 40..48 {
            for z in [0usize, 7, 15] {
                for x in [0usize, 7, 15] {
                    let expected = 1 + ((x + z + y as usize) % 6) as u32;
                    assert_eq!(
                        col.get_block(x, y, z),
                        expected,
                        "surface band mismatch at ({x}, {y}, {z})"
                    );
                }
            }
        }

        // Above the surface band: air (elided, reads as the air id).
        assert_eq!(col.get_block(0, 48, 0), 0);
        assert_eq!(col.get_block(0, 319, 0), 0);
    }

    #[test]
    fn synthetic_column_varies_the_surface_band_by_seed() {
        let a = synthetic_overworld_column(0);
        let b = synthetic_overworld_column(1);
        let mut differs = false;
        for z in 0..16usize {
            for x in 0..16usize {
                if a.get_block(x, 42, z) != b.get_block(x, 42, z) {
                    differs = true;
                }
            }
        }
        assert!(
            differs,
            "seed=0 and seed=1 produced identical surface bands -- the seed parameter is dead"
        );
    }

    #[test]
    fn synthetic_column_exercises_more_than_one_block_id_in_the_surface_band() {
        // A secretly uniform "varied surface band" would let a light or mesh
        // benchmark measure a vacuous world: uniform terrain can propagate or
        // cull in near-O(1) regardless of algorithm correctness.
        let col = synthetic_overworld_column(0);
        let mut ids = std::collections::HashSet::new();
        for z in 0..16usize {
            for x in 0..16usize {
                ids.insert(col.get_block(x, 42, z));
            }
        }
        assert!(
            ids.len() > 1,
            "surface band at y=42 is uniform ({ids:?}) -- this fixture would be vacuous"
        );
    }
}
