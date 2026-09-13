//! Shared chunk/column terrain fixtures for benchmark harnesses. The module
//! keeps the definition of "realistic terrain" in one place instead of
//! duplicating hand-rolled shapes across `lodestone-world`
//! (`tests/memory.rs`, `tests/pool_footprint.rs`,
//! `benches/light_propagation.rs`) and `lodestone-render`
//! (`tests/world_mesher_bench.rs`, `tests/scene_bench.rs`).
//!
//! Two realism tiers are available:
//!
//! - [`synthetic_column`] / [`synthetic_overworld_column`] -- **Tier 2
//!   (fast, synthetic)**. No worldgen dependency, no filesystem I/O: a stone
//!   floor, a varied surface band, open sky above -- the same *public shape*
//!   real terrain has. At `seed = 0`, it matches the established
//!   `realistic_terrain_column` shape used by the sites above. Use this for
//!   benchmarks that need many columns cheaply and do not care about worldgen
//!   fidelity (meshing, light propagation, memory/footprint).
//! - [`RealTerrain`] (behind this crate's `worldgen` feature, default off)
//!   -- **Tier 1 (real, exact, slow)**. Drives
//!   `lodestone_worldgen::overworld::OverworldGenerator` and converts its
//!   output into an ordinary [`lodestone_world::ChunkColumn`] with the
//!   block-state-name-to-id conversion used by
//!   `lodestone-world/tests/pool_footprint.rs`. Use this for throughput and
//!   stage-split benchmarks where realism is the point.
//!
//! Neither tier performs network I/O, needs a live server, or needs
//! `--features live`; both run wherever `cargo test`/`cargo bench` runs.
//!
//! # Why a feature gate on the real tier
//!
//! `lodestone-testsupport` is a *normal* (non-dev) dependency of
//! `lodestone-shell` (used outside `#[cfg(test)]` there), so an unconditional
//! dependency on `lodestone-worldgen` here would compile into the shipped
//! shell binary and would need to survive
//! `cargo check -p lodestone-shell --no-default-features`. The `worldgen`
//! feature defaults off, and no workspace crate enables it via a plain
//! `{ workspace = true }` dependency (the form every current consumer uses),
//! so [`RealTerrain`] only exists in a build that opts in explicitly (a
//! bench's or test's own `Cargo.toml` `[dev-dependencies]` entry).

use lodestone_world::{ChunkColumn, PaletteKind};

/// Overworld shape constant shared by the benchmark fixtures
/// (`lodestone-world/tests/memory.rs`,
/// `lodestone-world/benches/light_propagation.rs`,
/// `lodestone-world/tests/pool_footprint.rs`): 1.18+'s `y = -64..320`.
pub const MODERN_MIN_Y: i32 = -64;
pub const MODERN_SECTIONS: usize = 24;

/// Tier 2 (fast, synthetic): a [`ChunkColumn`] at the same public shape real
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

#[cfg(feature = "worldgen")]
mod real {
    use std::collections::HashMap;
    use std::path::Path;

    use lodestone_world::{ChunkColumn, PaletteKind};
    use lodestone_worldgen::density::{NoiseParams, Resolver};
    use lodestone_worldgen::overworld::OverworldGenerator;
    use serde_json::Value;

    /// Reads density functions and noises from the checked-in fixture tree
    /// used by `lodestone-worldgen`'s `tests/overworld_gen.rs`,
    /// `benches/generation.rs`, and
    /// `lodestone-world/tests/pool_footprint.rs`. Keeping this resolver here
    /// gives all real-terrain benchmarks the same input format.
    struct FsResolver {
        root: std::path::PathBuf,
    }

    impl FsResolver {
        fn read(&self, kind: &str, id: &str) -> Value {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let path = self.root.join(kind).join(format!("{name}.json"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
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
    }

    /// Tier 1 (real, exact, slow): drives [`OverworldGenerator`] and converts
    /// its output into an ordinary [`ChunkColumn`] via
    /// [`ChunkColumn::set_block`]. Block-state names are interned into a
    /// stable `u32` space and air is elided; the lower-level
    /// `PalettedContainer` plumbing used by
    /// `lodestone-world/tests/pool_footprint.rs` is unnecessary for a bench
    /// fixture.
    ///
    /// Construction reads and parses the fixture JSON tree, so build one
    /// `RealTerrain` and reuse it for all columns in a benchmark.
    #[allow(missing_debug_implementations)] // `OverworldGenerator` has none either.
    pub struct RealTerrain {
        generator: OverworldGenerator,
        ids: HashMap<String, u32>,
        next_id: u32,
    }

    impl RealTerrain {
        /// Reads the checked-in fixture tree used by
        /// `lodestone-worldgen`'s tests and benches
        /// (`crates/lodestone-worldgen/tests/support/worldgen_data`), with no
        /// network I/O or live server. `seed` is the world seed passed to
        /// [`OverworldGenerator::new`], not the `synthetic_column` seed; the
        /// two tiers interpret their seeds independently.
        #[must_use]
        pub fn new(seed: i64) -> Self {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../lodestone-worldgen/tests/support/worldgen_data");
            let resolver = FsResolver { root: root.clone() };
            let settings: Value = serde_json::from_str(
                &std::fs::read_to_string(root.join("noise_settings/overworld.json"))
                    .expect("reading noise_settings/overworld.json"),
            )
            .expect("parsing noise_settings/overworld.json");
            let generator =
                OverworldGenerator::new(seed, &settings, &resolver, "minecraft:plains", false);
            let mut ids = HashMap::new();
            ids.insert("minecraft:air".to_string(), 0u32);
            Self {
                generator,
                ids,
                next_id: 1,
            }
        }

        fn intern(&mut self, name: &str) -> u32 {
            if let Some(&id) = self.ids.get(name) {
                return id;
            }
            let id = self.next_id;
            self.next_id += 1;
            self.ids.insert(name.to_string(), id);
            id
        }

        /// Generates real terrain for chunk `(cx, cz)` and converts it into a
        /// [`ChunkColumn`] at the generator's own min-Y/height (so the
        /// column always matches the noise settings this `RealTerrain` was
        /// built from, rather than assuming the modern overworld shape).
        #[must_use]
        pub fn column(&mut self, cx: i32, cz: i32) -> ChunkColumn {
            let min_y = self.generator.min_y();
            let height = self.generator.height();
            let sections = (height / 16) as usize;
            let gen_col = self.generator.column(cx, cz);
            let mut col = ChunkColumn::new(
                min_y,
                sections,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                0,
                0,
            );
            for z in 0..16usize {
                for x in 0..16usize {
                    for y in 0..height {
                        let world_y = min_y + y;
                        let name = gen_col.block_state(x, world_y, z);
                        if name == "minecraft:air" {
                            continue; // already air by construction; skip the intern+write
                        }
                        let id = self.intern(name);
                        col.set_block(x, world_y, z, id);
                    }
                }
            }
            col
        }
    }
}

#[cfg(feature = "worldgen")]
pub use real::RealTerrain;

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

    /// Tier 1 smoke test: `RealTerrain` drives the generator and produces
    /// varied, non-vacuous terrain. One column is cheap enough for the
    /// default `cargo test` run whenever the `worldgen` feature is enabled;
    /// the larger `lodestone-world/tests/pool_footprint.rs` sweep remains a
    /// separate benchmark.
    #[cfg(feature = "worldgen")]
    #[test]
    fn real_terrain_produces_varied_non_air_blocks() {
        let mut terrain = RealTerrain::new(42);
        let col = terrain.column(0, 0);
        assert_eq!(col.min_y(), -64);

        let mut saw_air = false;
        let mut saw_non_air = false;
        let mut ids = std::collections::HashSet::new();
        for y in -64..320 {
            for z in 0..16usize {
                for x in 0..16usize {
                    let id = col.get_block(x, y, z);
                    ids.insert(id);
                    if id == 0 {
                        saw_air = true;
                    } else {
                        saw_non_air = true;
                    }
                }
            }
        }
        assert!(saw_air, "real column (0,0) has no air at all -- fixture is suspect");
        assert!(saw_non_air, "real column (0,0) is all air -- generator produced nothing");
        assert!(
            ids.len() > 1,
            "real column (0,0) has only one distinct block id ({ids:?}) -- vacuous terrain"
        );
    }
}
