//! A small, deterministic, **version-free** world generator.
//!
//! Why this exists: the shell's whole reason to render is the live server's
//! terrain, but that terrain cannot currently reach the shell through the public
//! client API (see `crate::net` and the report — `ClientEvent` carries no chunk
//! block data). So the shell renders a locally-generated [`World`] built from the
//! *same* version-free [`lodestone_world`] types a decoded server chunk would
//! produce. Every downstream link — classifier → mesher → GPU — is exercised
//! exactly as it would be for real chunks; only the wire delivery is stubbed.
//!
//! The generator is intentionally simple (a couple of sine waves plus a hash for
//! trees) and fully deterministic, so tests can assert block ids at known
//! coordinates.

use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

use crate::blocks::id;

/// World vertical layout: lowest block at y=0, `SECTION_COUNT` sections tall.
pub const MIN_Y: i32 = 0;
/// Number of 16-tall sections per column (80 blocks — plenty for a demo).
pub const SECTION_COUNT: usize = 5;
/// Sea level; anything generated below this and not solid becomes water.
pub const SEA_LEVEL: i32 = 38;

/// Deterministic surface height (world-y of the top solid block) at world
/// column `(wx, wz)`.
#[must_use]
pub fn surface_height(wx: i32, wz: i32) -> i32 {
    let x = wx as f64;
    let z = wz as f64;
    let h = 40.0 + 6.0 * (x * 0.08).sin() + 5.0 * (z * 0.07).cos() + 3.0 * ((x + z) * 0.15).sin();
    h.round() as i32
}

/// Cheap integer hash → `[0,1)`, for scattering trees deterministically.
fn hash01(x: i32, z: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374_761_393) ^ (z as u32).wrapping_mul(668_265_263);
    h ^= h >> 13;
    h = h.wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h & 0xffff) as f32 / 65536.0
}

/// Should a tree grow at this surface column?
fn tree_at(wx: i32, wz: i32, surface: i32) -> bool {
    surface > SEA_LEVEL + 1 && hash01(wx, wz) > 0.985
}

/// Generate a single chunk column at chunk position `(cx, cz)`.
#[must_use]
pub fn generate_column(cx: i32, cz: i32) -> LoadedChunk {
    let mut column = ChunkColumn::new(
        MIN_Y,
        SECTION_COUNT,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        id::AIR,
        0,
    );

    for lx in 0..16usize {
        for lz in 0..16usize {
            let wx = cx * 16 + lx as i32;
            let wz = cz * 16 + lz as i32;
            let surface =
                surface_height(wx, wz).clamp(MIN_Y + 1, MIN_Y + SECTION_COUNT as i32 * 16 - 1);

            for y in MIN_Y..=surface {
                let block = if y == MIN_Y {
                    id::BEDROCK
                } else if y >= surface - 3 && surface <= SEA_LEVEL + 1 {
                    id::SAND
                } else if y == surface {
                    id::GRASS
                } else if y >= surface - 3 {
                    id::DIRT
                } else {
                    id::STONE
                };
                column.set_block(lx, y, lz, block);
            }

            // Fill water up to sea level over low ground.
            if surface < SEA_LEVEL {
                for y in (surface + 1)..=SEA_LEVEL {
                    column.set_block(lx, y, lz, id::WATER);
                }
            }

            // A little tree: a 4-tall trunk capped by a 3x3x2 leaf blob.
            if tree_at(wx, wz, surface) {
                let base = surface + 1;
                for dy in 0..4 {
                    column.set_block(lx, base + dy, lz, id::LOG);
                }
                let crown = base + 3;
                for dy in 0..2 {
                    for ddx in -1..=1i32 {
                        for ddz in -1..=1i32 {
                            let nx = lx as i32 + ddx;
                            let nz = lz as i32 + ddz;
                            if (0..16).contains(&nx) && (0..16).contains(&nz) {
                                let y = crown + dy;
                                if column.get_block(nx as usize, y, nz as usize) == id::AIR {
                                    column.set_block(nx as usize, y, nz as usize, id::LEAVES);
                                }
                            }
                        }
                    }
                }
                column.set_block(lx, crown + 2, lz, id::LEAVES);
            }
        }
    }

    // The shell has no server light for a local world; full sky light is a fine
    // approximation and matches how meshing treats above-ground air.
    let light = ColumnLight::new(SECTION_COUNT);
    let heightmaps = Heightmaps::new();
    LoadedChunk::new(column, light, heightmaps, Vec::new())
}

/// Generate a square patch of chunks of radius `radius` (in chunks) centred on
/// chunk `(0,0)`, loaded into a fresh [`World`].
#[must_use]
pub fn generate(radius: i32) -> World {
    let mut world = World::new();
    for cz in -radius..=radius {
        for cx in -radius..=radius {
            world.load(ChunkPos { x: cx, z: cz }, generate_column(cx, cz));
        }
    }
    world
}

/// A comfortable spawn position (feet) above the surface at the world origin.
#[must_use]
pub fn spawn_feet() -> [f64; 3] {
    let surface = surface_height(0, 0);
    [0.5, (surface + 1) as f64, 0.5]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_deterministic() {
        assert_eq!(surface_height(3, 7), surface_height(3, 7));
    }

    #[test]
    fn bedrock_floor_and_grass_top() {
        let chunk = generate_column(0, 0);
        let col = &chunk.column;
        assert_eq!(
            col.get_block(0, MIN_Y, 0),
            id::BEDROCK,
            "bedrock at the floor"
        );
        let surface = surface_height(0, 0);
        // The top solid block is grass (dry land at origin) and above it is air.
        assert_eq!(col.get_block(0, surface, 0), id::GRASS);
        assert_eq!(col.get_block(0, surface + 5, 0), id::AIR);
    }

    #[test]
    fn columns_are_not_all_air() {
        let chunk = generate_column(2, -3);
        let col = &chunk.column;
        let mut solids = 0;
        for y in MIN_Y..MIN_Y + 60 {
            if col.get_block(8, y, 8) != id::AIR {
                solids += 1;
            }
        }
        assert!(solids > 20, "expected a solid column, got {solids} solids");
    }

    #[test]
    fn generate_loads_the_expected_chunk_count() {
        let world = generate(2);
        assert_eq!(world.len(), 25, "5x5 patch");
        assert!(crate::blocks::palette().len() >= 8);
    }
}
