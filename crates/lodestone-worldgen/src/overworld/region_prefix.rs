//! Region-native execution of the immutable part of the Overworld terrain prefix.
//!
//! A bounded prefix request shares final-density and surface-biome products
//! while retaining chunk-local aquifer, surface, and carving state.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;

use lodestone_data::biomes::{BiomeRef, BuiltinBiome};

use crate::aquifer::{AquiferRegionCache, AquiferSystem, PreliminarySurfaceCache};
use crate::biome::PreparedClimateGrid;
use crate::density::{Context, NoiseChunkRegionSampler};
use crate::engine::{Bounds, PointScratch, XzProductLattice, XzRect};
use crate::overworld::biome_cells::BiomeCells;
use crate::surface::SurfaceBiomeAnswer;
use super::fill::PackedStateCarrier;
use super::structures::REFS_RADIUS;
use super::{OverworldGenerator, PreOreResult};

/// Biome cells for the sampled rectangle plus the one-chunk surface border.
#[derive(Debug)]
struct RegionBiomeSidecar {
    min_x: i32,
    min_z: i32,
    width: usize,
    depth: usize,
    cells: Vec<Option<Arc<BiomeCells>>>,
    no_sulfur: Vec<bool>,
    cold_biomes: [bool; BuiltinBiome::COUNT as usize],
    fiddle_lattice: RefCell<super::biome::ZoomFiddleLattice>,
    surface_search: RefCell<Option<crate::biome::BiomeSearchCursor>>,
}

impl RegionBiomeSidecar {
    fn new(
        generator: &OverworldGenerator,
        min_x: i32,
        max_x: i32,
        min_z: i32,
        max_z: i32,
        admitted_bounds: (i32, i32, i32, i32),
        climate: Option<&PreparedClimateGrid>,
    ) -> Self {
        let (product_min_x, product_max_x, product_min_z, product_max_z) = admitted_bounds;
        let side_min_x = min_x - 1;
        let side_min_z = min_z - 1;
        let width = (max_x - side_min_x + 2) as usize;
        let depth = (max_z - side_min_z + 2) as usize;
        let mut cells = Vec::with_capacity(width * depth);
        let mut no_sulfur = Vec::with_capacity(width * depth);
        let mut cold_biomes = [false; BuiltinBiome::COUNT as usize];
        if let Some(dynamic) = &generator.dynamic_biome {
            for biome in BuiltinBiome::all() {
                cold_biomes[biome as usize] = crate::biome::cold_enough_to_snow(
                    &dynamic.temperatures,
                    biome.name(),
                );
            }
        } else {
            cold_biomes.fill(generator.fallback_cold_enough_to_snow);
        }
        for cz in side_min_z..=max_z + 1 {
            for cx in side_min_x..=max_x + 1 {
                let admitted = cx >= product_min_x
                    && cx <= product_max_x
                    && cz >= product_min_z
                    && cz <= product_max_z;
                let biome_cells = (generator.dynamic_biome.is_none() || admitted)
                    .then(|| {
                        admitted
                            .then(|| generator.store.get((cx, cz)))
                            .flatten()
                            .and_then(|entry| {
                                entry.pre_ore.peek().map(|product| Arc::clone(&product.3))
                            })
                            .unwrap_or_else(|| {
                                Arc::new(region_biome_cells(generator, cx, cz, climate))
                            })
                    });
                no_sulfur.push(biome_cells.as_ref().is_some_and(|biome_cells| {
                    biome_cells
                        .palette_entries()
                        .iter()
                        .all(|entry| *entry != BiomeRef::builtin(BuiltinBiome::SulfurCaves))
                }));
                cells.push(biome_cells);
            }
        }
        Self {
            min_x: side_min_x,
            min_z: side_min_z,
            width,
            depth,
            cells,
            no_sulfur,
            cold_biomes,
            fiddle_lattice: RefCell::new(super::biome::ZoomFiddleLattice::for_request_region(
                super::biome::biome_zoom_seed(generator.seed),
                side_min_x + 1,
                max_x,
                side_min_z + 1,
                max_z,
                generator.min_y,
                generator.height,
            )),
            surface_search: RefCell::new(generator.biome_search_cursor()),
        }
    }

    #[inline]
    fn cells_at(&self, cx: i32, cz: i32) -> Option<&Arc<BiomeCells>> {
        let x = usize::try_from(cx - self.min_x).ok()?;
        let z = usize::try_from(cz - self.min_z).ok()?;
        (x < self.width && z < self.depth)
            .then(|| self.cells[z * self.width + x].as_ref())
            .flatten()
    }

    #[inline]
    fn no_sulfur_at(&self, cx: i32, cz: i32) -> bool {
        let x = usize::try_from(cx - self.min_x).expect("surface biome X escaped sidecar");
        let z = usize::try_from(cz - self.min_z).expect("surface biome Z escaped sidecar");
        self.no_sulfur[z * self.width + x]
    }

    /// Returns a conservative no-sulfur mask for the chunk's 256 X/Z lanes.
    /// Each lane considers both possible raw quart candidates on each
    /// horizontal axis and the complete generated vertical biome palette.
    fn deep_biome_absent_mask(&self, base_x: i32, base_z: i32) -> [bool; 256] {
        let mut mask = [false; 256];
        for lx in 0..16i32 {
            for lz in 0..16i32 {
                let parent_qx = (base_x + lx - 2).div_euclid(4);
                let parent_qz = (base_z + lz - 2).div_euclid(4);
                let mut absent = true;
                for qx in [parent_qx, parent_qx + 1] {
                    for qz in [parent_qz, parent_qz + 1] {
                        let chunk_x = (qx * 4).div_euclid(16);
                        let chunk_z = (qz * 4).div_euclid(16);
                        if !self.no_sulfur_at(chunk_x, chunk_z) {
                            absent = false;
                        }
                    }
                }
                mask[(lz * 16 + lx) as usize] = absent;
            }
        }
        mask
    }

    #[inline]
    fn uniform_resident_biome_at(&self, x: i32, y: i32, z: i32) -> Option<SurfaceBiomeAnswer> {
        let parent_qx = (x - 2).div_euclid(4);
        let parent_qy = (y - 2).div_euclid(4);
        let parent_qz = (z - 2).div_euclid(4);
        let mut uniform = None;
        for qx in [parent_qx, parent_qx + 1] {
            let block_x = qx * 4;
            let local_qx = block_x.rem_euclid(16).div_euclid(4) as usize;
            for qz in [parent_qz, parent_qz + 1] {
                let block_z = qz * 4;
                let cells = self.cells_at(block_x.div_euclid(16), block_z.div_euclid(16))?;
                let local_qz = block_z.rem_euclid(16).div_euclid(4) as usize;
                for qy in [parent_qy, parent_qy + 1] {
                    let local_qy = (qy * 4 - cells.min_y()).div_euclid(4).max(0) as usize;
                    let biome = cells.at_quart_ref(local_qx, local_qy, local_qz);
                    if uniform.is_some_and(|previous| previous != biome) {
                        return None;
                    }
                    uniform = Some(biome);
                }
            }
        }
        uniform.and_then(BiomeRef::builtin_or_none).map(|biome| {
            SurfaceBiomeAnswer::uniform_shifted_quart(
                parent_qy, biome, self.cold_biomes[biome as usize],
            )
        })
    }

    #[inline]
    fn biome_at_typed(
        &self,
        generator: &OverworldGenerator,
        prepared: Option<&PreparedClimateGrid>,
        x: i32,
        y: i32,
        z: i32,
    ) -> SurfaceBiomeAnswer {
        // Equal resident corner identities make the pure zoom choice irrelevant.
        // Missing cells must retain the cursor-based search in the zoomed path.
        if let Some(answer) = self.uniform_resident_biome_at(x, y, z) {
            return answer;
        }
        let (biome, cold) = self.biome_at_typed_zoomed(generator, prepared, x, y, z);
        SurfaceBiomeAnswer::exact(y, biome, cold)
    }

    #[inline]
    fn biome_at_typed_zoomed(
        &self,
        generator: &OverworldGenerator,
        prepared: Option<&PreparedClimateGrid>,
        x: i32,
        y: i32,
        z: i32,
    ) -> (BuiltinBiome, bool) {
        let (qx, qy, qz) = self
            .fiddle_lattice
            .borrow_mut()
            .selected_quart_at(x, y, z);
        let block_x = qx * 4;
        let block_z = qz * 4;
        if let Some(cells) = self.cells_at(block_x.div_euclid(16), block_z.div_euclid(16)) {
            let local_qx = block_x.rem_euclid(16).div_euclid(4) as usize;
            let local_qz = block_z.rem_euclid(16).div_euclid(4) as usize;
            let local_qy = (qy * 4 - cells.min_y()).div_euclid(4).max(0) as usize;
            let biome = cells
                .at_quart_ref(local_qx, local_qy, local_qz)
                .builtin_or_none()
                .expect("strict worldgen biome cells contain only built-in biomes");
            return (biome, self.cold_biomes[biome as usize]);
        }
        let dynamic = generator
            .dynamic_biome
            .as_ref()
            .expect("fixed-biome generation materializes its complete surface border");
        let target = prepared.map_or_else(
            || dynamic.climate.target(block_x, qy * 4, block_z),
            |prepared| dynamic.climate.target_prepared(prepared, block_x, qy * 4, block_z),
        );
        let row = dynamic
            .table
            .nearest_row_with_cursor(&target, self.surface_search.borrow_mut().as_mut().expect("dynamic biome search cursor"));
        let biome = dynamic
            .table
            .biome_ref_at(row)
            .expect("strict worldgen table contains only generated built-in biomes")
            .builtin_or_none()
            .expect("strict worldgen table contains only built-in biomes");
        (biome, self.cold_biomes[biome as usize])
    }
}

/// An immutable set of region products. `products` is sorted by exact chunk
/// coordinate so callers can retrieve a result without aliasing a neighbour.
#[derive(Debug)]
pub(super) struct RegionPrefixBatch {
    products: Vec<((i32, i32), Arc<PreOreResult>)>,
}

impl RegionPrefixBatch {
    /// Samples only missing outputs and their halos. Admitted biome bounds
    /// retain the distinction between materialized cells and cursor searches.
    pub(super) fn execute(
        generator: &OverworldGenerator,
        missing_positions: &[(i32, i32)],
        admitted_bounds: (i32, i32, i32, i32),
        preliminary: &Arc<PreliminarySurfaceCache>,
    ) -> Self {
        Self::execute_with_geometry(
            generator,
            missing_positions,
            missing_positions,
            admitted_bounds,
            preliminary,
        )
    }

    fn execute_with_geometry(
        generator: &OverworldGenerator,
        positions: &[(i32, i32)],
        missing_positions: &[(i32, i32)],
        admitted_bounds: (i32, i32, i32, i32),
        preliminary: &Arc<PreliminarySurfaceCache>,
    ) -> Self {
        assert!(!positions.is_empty(), "region prefix requires at least one position");
        let (min_x, max_x, min_z, max_z) = position_bounds(positions);
        let structure_sampler = generator
            .has_structure_registry()
            .then(|| super::structures::StartSampler::new(generator));
        let origin_index = structure_sampler.as_ref().and_then(|sampler| {
            generator.structure_origin_index_for_bounds_with_sampler(
                min_x - REFS_RADIUS,
                max_x + REFS_RADIUS,
                min_z - REFS_RADIUS,
                max_z + REFS_RADIUS,
                sampler,
            )
        });
        let xz_products = build_xz_products(generator, positions);
        let sampler = NoiseChunkRegionSampler::from_program_with_xz_products(
            generator.aquifer_trees.final_density.clone(),
            generator.slot_count,
            generator.aquifer_trees.cell_width,
            generator.aquifer_trees.cell_height,
            Bounds {
                x: (min_x * 16, max_x * 16 + 15),
                y: (generator.min_y, generator.min_y + generator.height - 1),
                z: (min_z * 16, max_z * 16 + 15),
            },
            xz_products.clone(),
        );

        // X/Z-only climate channels are prepared once for the sidecar.
        let climate = generator.dynamic_biome.as_ref().map(|dynamic| {
            crate::counters::bump_climate_grid_preparation();
            let side_min_x = (min_x - 1) * 4;
            let side_min_z = (min_z - 1) * 4;
            let side_width = ((max_x - min_x + 3) * 4) as usize;
            let side_depth = ((max_z - min_z + 3) * 4) as usize;
            Arc::new(dynamic.climate.prepare_xz_rect(
                side_min_x,
                side_min_z,
                side_width,
                side_depth,
            ))
        });
        let sidecar = RegionBiomeSidecar::new(
            generator,
            min_x,
            max_x,
            min_z,
            max_z,
            admitted_bounds,
            climate.as_deref(),
        );
        let mut aquifer_cache = AquiferRegionCache::new();
        let mut products = Vec::with_capacity(missing_positions.len());
        for &(cx, cz) in missing_positions {
            let aquifer = build_region_aquifer(
                generator,
                cx,
                cz,
                preliminary,
                xz_products.clone(),
            );
            let beard = structure_sampler.as_ref().map_or_else(
                crate::structure::beardifier::Beardifier::empty,
                |sampler| {
                    generator.beardifier_for_with_index_and_sampler(
                        cx,
                        cz,
                        origin_index.as_ref(),
                        sampler,
                    )
                },
            );
            let field = generator.fill_stage_packed_with_region(
                &aquifer,
                cx * 16,
                cz * 16,
                &beard,
                Some(&sampler),
                Some(&mut aquifer_cache),
            );
            let biome_cells = sidecar
                .cells_at(cx, cz)
                .expect("region biome sidecar omitted an admitted chunk")
                .clone();
            let biome_quarts = generator.biome_stage(biome_cells.as_ref(), &field.1);
            let base_x = cx * 16;
            let base_z = cz * 16;
            let mut carrier = PackedStateCarrier::from_field(generator, field.0, base_x, base_z);
            let deep_biome_absent = sidecar.deep_biome_absent_mask(base_x, base_z);
            generator.surface.build_surface_reusing_packed_in_place(
                &mut carrier,
                &field.1,
                &deep_biome_absent,
                &|lx, y, lz| {
                    sidecar.biome_at_typed(
                        generator,
                        climate.as_deref(),
                        base_x + lx,
                        y,
                        base_z + lz,
                    )
                },
                &|_, _, _| {},
                base_x,
                base_z,
                preliminary,
            );
            let result = generator.finish_pre_ore_stage_from_state_carrier(
                cx,
                cz,
                &aquifer,
                field.1,
                biome_quarts,
                biome_cells,
                carrier,
            );
            products.push(((cx, cz), Arc::new(result)));
        }
        products.sort_unstable_by_key(|(coordinate, _)| *coordinate);
        Self { products }
    }

    /// Returns the exact result for `coordinate`.
    #[must_use]
    pub(super) fn result(&self, coordinate: (i32, i32)) -> Arc<PreOreResult> {
        self.products
            .binary_search_by_key(&coordinate, |(position, _)| *position)
            .ok()
            .map(|index| Arc::clone(&self.products[index].1))
            .unwrap_or_else(|| panic!("region prefix result missing for {coordinate:?}"))
    }
}

fn build_region_aquifer(
    generator: &OverworldGenerator,
    cx: i32,
    cz: i32,
    preliminary: &Arc<PreliminarySurfaceCache>,
    xz_products: Option<Arc<XzProductLattice>>,
) -> AquiferSystem {
    let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Aquifer);
    let t = &generator.aquifer_trees;
    AquiferSystem::from_parts_with_preliminary_cache_and_point_programs(
        t.final_density.clone(),
        t.erosion.clone(),
        t.depth.clone(),
        t.barrier.clone(),
        t.floodedness.clone(),
        t.spread.clone(),
        t.lava.clone(),
        t.prelim.clone(),
        t.prelim_program.clone(),
        t.point_programs.clone(),
        t.positional,
        generator.sea_level,
        generator.min_y,
        generator.height,
        cx,
        cz,
        generator.slot_count,
        t.cell_width,
        t.cell_height,
        Arc::clone(preliminary),
        xz_products,
    )
}

fn build_xz_products(
    generator: &OverworldGenerator,
    positions: &[(i32, i32)],
) -> Option<Arc<XzProductLattice>> {
    let identity = generator
        .aquifer_trees
        .prelim_program
        .xz_product_identity()?;
    if generator.aquifer_trees.final_density.xz_product_identity() != Some(identity.clone()) {
        return None;
    }

    let mut keys = BTreeSet::new();
    for &(cx, cz) in positions {
        for qz in (cz * 4 - 4)..=(cz * 4 + 6) {
            for qx in (cx * 4 - 4)..=(cx * 4 + 6) {
                keys.insert((qx, qz));
            }
        }
    }
    let (min_qx, max_qx, min_qz, max_qz) = keys.iter().fold(
        (i32::MAX, i32::MIN, i32::MAX, i32::MIN),
        |(min_x, max_x, min_z, max_z), &(qx, qz)| {
            (min_x.min(qx), max_x.max(qx), min_z.min(qz), max_z.max(qz))
        },
    );
    let rect = XzRect::new(
        min_qx,
        min_qz,
        usize::try_from(max_qx - min_qx + 1).expect("X/Z lattice width overflow"),
        usize::try_from(max_qz - min_qz + 1).expect("X/Z lattice depth overflow"),
    );
    let contexts: Vec<_> = keys
        .iter()
        .map(|&(qx, qz)| Context::new(qx * 4, 0, qz * 4))
        .collect();
    let mut values = vec![(0.0, 0.0); contexts.len()];
    let mut scratch = PointScratch::new();
    if !generator
        .aquifer_trees
        .prelim_program
        .compute_xz_products(&contexts, &mut values, &mut scratch)
    {
        return None;
    }
    let mut lattice = XzProductLattice::new_with_identity(rect, identity);
    for (context, &(factor, offset)) in contexts.iter().zip(&values) {
        lattice.insert_pair(context.x, context.z, factor, offset);
    }
    Some(Arc::new(lattice))
}

fn position_bounds(positions: &[(i32, i32)]) -> (i32, i32, i32, i32) {
    let &(first_x, first_z) = positions
        .first()
        .expect("region prefix position list must not be empty");
    positions.iter().skip(1).fold(
        (first_x, first_x, first_z, first_z),
        |(min_x, max_x, min_z, max_z), &(x, z)| {
            (min_x.min(x), max_x.max(x), min_z.min(z), max_z.max(z))
        },
    )
}

fn region_biome_cells(
    generator: &OverworldGenerator,
    cx: i32,
    cz: i32,
    prepared: Option<&PreparedClimateGrid>,
) -> BiomeCells {
    let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Biome);
    let Some(dynamic) = &generator.dynamic_biome else {
        return BiomeCells::uniform_strict(
            &generator.fallback_biome,
            generator.min_y,
            generator.height,
        );
    };
    let mut cursor = dynamic.table.search_cursor();
    let base_x = cx * 16;
    let base_z = cz * 16;
    BiomeCells::from_fn_section_query_order_typed(
        generator.min_y,
        generator.height,
        |qx, qy, qz| {
            let y = generator.min_y + qy as i32 * 4;
            let target = prepared.map_or_else(
                || dynamic.climate.target(base_x + qx as i32 * 4, y, base_z + qz as i32 * 4),
                |prepared| {
                    dynamic.climate.target_prepared(
                        prepared,
                        base_x + qx as i32 * 4,
                        y,
                        base_z + qz as i32 * 4,
                    )
                },
            );
            let row = dynamic.table.nearest_row_with_cursor(&target, &mut cursor);
            dynamic
                .table
                .biome_ref_at(row)
                .expect("strict worldgen table contains only generated built-in biomes")
        },
    )
}

#[cfg(test)]
mod tests {
    use super::position_bounds;

    #[test]
    fn region_bounds_are_coordinate_exact() {
        assert_eq!(position_bounds(&[(-2, 4), (3, -1), (0, 2)]), (-2, 3, -1, 4));
    }

    #[test]
    fn region_bounds_negative_coordinates_do_not_round() {
        assert_eq!(position_bounds(&[(-8, -8), (-1, -2)]), (-8, -1, -8, -2));
    }
}

#[cfg(all(test, feature = "gen-counters"))]
mod prefix_comparison_tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use serde_json::Value;

    use super::{
        OverworldGenerator, RegionBiomeSidecar, RegionPrefixBatch, build_xz_products,
        position_bounds,
    };
    use crate::counters::Stage;
    use crate::density::{NoiseParams, Resolver};
    use crate::overworld::structures::StartSampler;

    struct FsResolver {
        root: PathBuf,
        dynamic_biomes: bool,
    }

    impl FsResolver {
        fn read(&self, kind: &str, id: &str) -> Value {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let path = self.root.join(kind).join(format!("{name}.json"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
        }
    }

    impl Resolver for FsResolver {
        fn density_function(&self, id: &str) -> Value {
            self.read("density_function", id)
        }

        fn noise(&self, id: &str) -> NoiseParams {
            let value = self.read("noise", id);
            NoiseParams {
                first_octave: value["firstOctave"].as_i64().expect("firstOctave") as i32,
                amplitudes: value["amplitudes"]
                    .as_array()
                    .expect("amplitudes")
                    .iter()
                    .map(|amplitude| amplitude.as_f64().expect("amplitude"))
                .collect(),
            }
        }

        fn structure_set_ids(&self) -> Vec<String> {
            vec!["test:region_probe_set".to_owned()]
        }

        fn biome_parameters(&self) -> Value {
            if self.dynamic_biomes {
                self.read_production_biome_data("overworld")
            } else {
                Value::Array(Vec::new())
            }
        }

        fn biome_temperatures(&self) -> Value {
            if self.dynamic_biomes {
                self.read_production_biome_data("overworld_temperature")
            } else {
                Value::Object(serde_json::Map::new())
            }
        }

        fn structure_set(&self, id: &str) -> Value {
            if id != "test:region_probe_set" {
                return Value::Null;
            }
            serde_json::json!({
                "placement": {
                    "type": "minecraft:random_spread",
                    "spacing": 24,
                    "separation": 4,
                    "salt": 165745295
                },
                "structures": [{ "structure": "test:region_probe", "weight": 1 }]
            })
        }

        fn structure(&self, id: &str) -> Value {
            if id != "test:region_probe" {
                return Value::Null;
            }
            serde_json::json!({
                "type": "test:placement_probe",
                "biomes": ["minecraft:plains"],
                "step": "surface_structures"
            })
        }
    }

    impl FsResolver {
        fn read_production_biome_data(&self, name: &str) -> Value {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../lodestone-server/assets/worldgen/biome_parameters")
                .join(format!("{name}.json"));
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read biome data"))
                .expect("parse biome data")
        }
    }

    fn generator() -> OverworldGenerator {
        generator_with_biomes(false)
    }

    fn generator_with_biomes(dynamic_biomes: bool) -> OverworldGenerator {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
        let resolver = FsResolver {
            root: root.clone(),
            dynamic_biomes,
        };
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("noise_settings/overworld.json"))
                .expect("read overworld settings"),
        )
        .expect("parse overworld settings");
        OverworldGenerator::new(42, &settings, &resolver, "minecraft:plains", false)
    }

    fn surface_biome_sidecar(
        generator: &OverworldGenerator,
        seed: i64,
        cells: super::BiomeCells,
    ) -> RegionBiomeSidecar {
        let mut cold_biomes = [false; lodestone_data::biomes::BuiltinBiome::COUNT as usize];
        cold_biomes[lodestone_data::biomes::BuiltinBiome::SnowyPlains as usize] = true;
        RegionBiomeSidecar {
            min_x: -2,
            min_z: -2,
            width: 4,
            depth: 4,
            cells: vec![Some(Arc::new(cells)); 16],
            no_sulfur: vec![true; 16],
            cold_biomes,
            fiddle_lattice: std::cell::RefCell::new(
                super::super::biome::ZoomFiddleLattice::for_block_bounds(
                    seed, -16, 16, -129, 388, -16, 16,
                ),
            ),
            surface_search: std::cell::RefCell::new(generator.biome_search_cursor()),
        }
    }

    fn assert_surface_biome_band_queries(
        fast: &RegionBiomeSidecar,
        control: &RegionBiomeSidecar,
        generator: &OverworldGenerator,
        x: i32,
        z: i32,
        ys: std::ops::RangeInclusive<i32>,
        expected_queries: usize,
    ) {
        let mut cached: Option<super::SurfaceBiomeAnswer> = None;
        let mut queries = 0;
        for y in ys.rev() {
            if cached.and_then(|answer| answer.at_y(y)).is_none() {
                cached = Some(fast.biome_at_typed(generator, None, x, y, z));
                queries += 1;
            }
            assert_eq!(
                cached.and_then(|answer| answer.at_y(y)),
                Some(control.biome_at_typed_zoomed(generator, None, x, y, z)),
                "cached biome at ({x}, {y}, {z})",
            );
        }
        assert_eq!(queries, expected_queries, "surface callback at ({x}, {z})");
    }

    #[test]
    fn uniform_resident_surface_biome_omits_zoom_and_preserves_clamping() {
        use lodestone_data::biomes::BuiltinBiome;

        let generator = generator();
        for seed in [0, 42, -7_492_335_011] {
            let cells = super::BiomeCells::uniform("minecraft:snowy_plains", -64, 384);
            let fast = surface_biome_sidecar(&generator, seed, cells.clone());
            let control = surface_biome_sidecar(&generator, seed, cells);
            assert_surface_biome_band_queries(&fast, &control, &generator, -1, -1, -6..=9, 4);
            for x in [-16, -15, -2, -1, 0, 1, 2, 3, 14, 15, 16] {
                for z in [-16, -15, -2, -1, 0, 1, 2, 3, 14, 15, 16] {
                    for y in [-129, -68, -65, -64, -63, -1, 0, 1, 2, 63, 127, 319, 320, 388] {
                        let actual = fast.biome_at_typed(&generator, None, x, y, z);
                        assert_eq!(actual.at_y(y), Some((BuiltinBiome::SnowyPlains, true)));
                        assert_eq!(
                            actual.at_y(y),
                            Some(control.biome_at_typed_zoomed(&generator, None, x, y, z)),
                            "uniform biome at ({x}, {y}, {z}), seed {seed}",
                        );
                    }
                }
            }
            let fast_stats = fast.fiddle_lattice.borrow().stats();
            assert_eq!((fast_stats.vertex_hits, fast_stats.vertex_computes), (0, 0));
            assert!(control.fiddle_lattice.borrow().stats().vertex_computes > 0);
        }
    }

    #[test]
    fn uniform_resident_surface_biome_retains_mixed_and_vertical_zoom() {
        use lodestone_data::biomes::BuiltinBiome;

        let generator = generator();
        for horizontal_mix in [false, true] {
            let cells = super::BiomeCells::from_fn(-64, 384, |qx, qy, _| {
                let biome = if qy >= 48 {
                    "minecraft:snowy_plains"
                } else if horizontal_mix && qx % 2 == 1 {
                    "minecraft:forest"
                } else {
                    "minecraft:plains"
                };
                biome.to_owned()
            });
            let fast = surface_biome_sidecar(&generator, 42, cells.clone());
            let control = surface_biome_sidecar(&generator, 42, cells);
            assert!(fast.uniform_resident_biome_at(0, 128, 0).is_none());
            assert_surface_biome_band_queries(&fast, &control, &generator, 0, 0, 126..=129, 4);
            if horizontal_mix {
                assert!(fast.uniform_resident_biome_at(4, 64, 4).is_none());
            }
            for x in -16..=16 {
                for z in [-16, -1, 0, 1, 15, 16] {
                    for y in [-129, -64, -1, 125, 126, 127, 128, 129, 130, 319, 388] {
                        let actual = fast.biome_at_typed(&generator, None, x, y, z);
                        assert_eq!(
                            actual.at_y(y),
                            Some(control.biome_at_typed_zoomed(&generator, None, x, y, z)),
                            "mixed biome at ({x}, {y}, {z}), horizontal mix {horizontal_mix}",
                        );
                        if y >= 130 {
                            assert_eq!(actual.at_y(y), Some((BuiltinBiome::SnowyPlains, true)));
                        } else if y <= 125 && !horizontal_mix {
                            assert_eq!(actual.at_y(y), Some((BuiltinBiome::Plains, false)));
                        }
                    }
                }
            }
            assert!(fast.fiddle_lattice.borrow().stats().vertex_computes > 0);
        }
    }

    #[test]
    fn uniform_resident_surface_biome_retains_missing_cell_search_sequence() {
        let generator = generator_with_biomes(true);
        let cells = super::BiomeCells::uniform("minecraft:plains", -64, 384);
        let mut fast = surface_biome_sidecar(&generator, 42, cells.clone());
        let mut control = surface_biome_sidecar(&generator, 42, cells);
        fast.cells[10] = None;
        control.cells[10] = None;
        assert!(fast.uniform_resident_biome_at(1, 64, 1).is_none());
        assert_surface_biome_band_queries(&fast, &control, &generator, 1, 1, 62..=65, 4);
        let mut exterior_queries = 0;
        let mut choice = super::super::biome::ZoomFiddleLattice::for_block_bounds(
            42, -16, 16, -129, 388, -16, 16,
        );
        for y in [-64, -1, 63, 128, 319, -1, 63] {
            for (x, z) in [(-8, -8), (0, 0), (1, 1), (2, 2), (3, 3)] {
                let (qx, _, qz) = choice.selected_quart_at(x, y, z);
                exterior_queries += usize::from(qx.div_euclid(4) == 0 && qz.div_euclid(4) == 0);
                assert_eq!(
                    fast.biome_at_typed(&generator, None, x, y, z).at_y(y),
                    Some(control.biome_at_typed_zoomed(&generator, None, x, y, z)),
                    "missing-cell sequence at ({x}, {y}, {z})",
                );
            }
        }
        assert!(exterior_queries > 0, "control never selected the missing cell");
        assert!(fast.fiddle_lattice.borrow().stats().vertex_computes > 0);
    }

    fn digest(result: &crate::overworld::PreOreResult) -> u64 {
        let mut digest = 0xcbf2_9ce4_8422_2325u64;
        let mut add = |byte: u8| {
            digest = (digest ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3);
        };
        let world = &result.0;
        let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
        for y in min_y..min_y + size_y {
            for z in min_z..min_z + size_z {
                for x in min_x..min_x + size_x {
                    for byte in world.get(x, y, z).as_bytes() {
                        add(*byte);
                    }
                    add(0xff);
                }
            }
        }
        for height in result.1 {
            for byte in height.to_le_bytes() {
                add(byte);
            }
        }
        for (biome, cold) in &result.2 {
            for byte in biome
                .builtin_or_none()
                .expect("strict worldgen biome prefix contains built-ins")
                .name()
                .as_bytes()
            {
                add(*byte);
            }
            add(u8::from(*cold));
        }
        let cells = &result.3;
        for qy in 0..cells.y_quarts() {
            for qz in 0..4 {
                for qx in 0..4 {
                    for byte in cells.at_quart(qx, qy, qz).as_bytes() {
                        add(*byte);
                    }
                    add(0xfe);
                }
            }
        }
        digest
    }

    #[test]
    fn four_by_four_region_matches_scalar_prefixes_and_rejects_shifted_control() {
        let positions = (0..4)
            .flat_map(|cz| (0..4).map(move |cx| (cx, cz)))
            .collect::<Vec<_>>();
        let scalar = generator();
        let expected = positions
            .iter()
            .map(|&position| digest(&scalar.pre_ore_stage(position.0, position.1)))
            .collect::<Vec<_>>();

        let region = generator();
        let preliminary = region.preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
        let batch = RegionPrefixBatch::execute(
            &region, &positions, position_bounds(&positions), &preliminary,
        );
        let actual = positions
            .iter()
            .map(|&position| digest(&batch.result(position)))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "region prefix changed an admitted column");

        // Deliberately compare the first product with the adjacent scalar
        // product. The position-sensitive digest must make this detector fail
        // if a future lookup aliases a neighbour.
        assert_ne!(actual[0], expected[1], "negative coordinate-alias control passed");
    }

    #[test]
    fn mostly_warm_dynamic_region_matches_full_geometry_and_reuses_admitted_biomes() {
        let admitted = (-4..0)
            .flat_map(|z| (-4..0).map(move |x| (x, z)))
            .collect::<Vec<_>>();
        let missing = (-4..0).map(|z| (-1, z)).collect::<Vec<_>>();
        let admitted_bounds = position_bounds(&admitted);

        let control = generator_with_biomes(true);
        assert!(
            control.dynamic_biome.as_ref().expect("dynamic biome fixture").table.len() > 100,
        );
        let preliminary = control.preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
        crate::counters::reset();
        let full = RegionPrefixBatch::execute_with_geometry(
            &control, &admitted, &missing, admitted_bounds, &preliminary,
        );
        let full_counts = crate::counters::snapshot();

        let narrowed = generator_with_biomes(true);
        let preliminary = narrowed.preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
        crate::counters::reset();
        let cold = RegionPrefixBatch::execute(
            &narrowed, &missing, admitted_bounds, &preliminary,
        );
        let cold_counts = crate::counters::snapshot();

        let warmed = generator_with_biomes(true);
        for z in -4..0 {
            let _ = warmed.pre_ore_stage(-2, z);
        }
        let preliminary = warmed.preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
        crate::counters::reset();
        let warm = RegionPrefixBatch::execute(
            &warmed, &missing, admitted_bounds, &preliminary,
        );
        let warm_counts = crate::counters::snapshot();
        for &position in &missing {
            let full_digest = digest(&full.result(position));
            let cold_digest = digest(&cold.result(position));
            let warm_digest = digest(&warm.result(position));
            assert_eq!(
                cold_digest,
                full_digest,
                "{position:?}",
            );
            assert_eq!(warm_digest, full_digest, "warm neighbour changed {position:?}");
            eprintln!(
                "dynamic prefix {position:?}: full={full_digest:016x} cold={cold_digest:016x} warm={warm_digest:016x}",
            );
        }
        assert_ne!(
            digest(&warm.result(missing[0])),
            digest(&full.result(missing[1])),
            "negative coordinate-alias control passed",
        );
        assert_eq!(full_counts.stage_entered[Stage::Biome as usize], 16);
        assert_eq!(cold_counts.stage_entered[Stage::Biome as usize], 8);
        assert_eq!(warm_counts.stage_entered[Stage::Biome as usize], 4);
        assert_eq!(full_counts.stage_entered[Stage::Shape as usize], 4);
        assert_eq!(cold_counts.stage_entered[Stage::Shape as usize], 4);
        assert_eq!(warm_counts.stage_entered[Stage::Shape as usize], 4);

        let full_xz = build_xz_products(&control, &admitted).expect("fixture X/Z products");
        let cold_xz = build_xz_products(&narrowed, &missing).expect("fixture X/Z products");
        assert_eq!(full_xz.computes(), 529);
        assert_eq!(cold_xz.computes(), 253);
        let rect = cold_xz.rect();
        for z in 0..rect.depth {
            for x in 0..rect.width {
                let block_x = (rect.min_qx + x as i32) * 4;
                let block_z = (rect.min_qz + z as i32) * 4;
                let expected = full_xz.get_pair(block_x, block_z).expect("full X/Z pair");
                let actual = cold_xz.get_pair(block_x, block_z).expect("cold X/Z pair");
                assert_eq!(
                    (actual.0.to_bits(), actual.1.to_bits()),
                    (expected.0.to_bits(), expected.1.to_bits()),
                    "({block_x}, {block_z})",
                );
            }
        }
    }

    #[test]
    fn cold_negative_region_across_zero_matches_scalar_prefixes() {
        let positions = [(-2, -1), (-1, -1), (-2, 0), (-1, 0)];
        let scalar = generator();
        let region = generator();
        let preliminary = region.preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
        let batch = RegionPrefixBatch::execute(
            &region, &positions, position_bounds(&positions), &preliminary,
        );
        for &(x, z) in &positions {
            assert_eq!(
                digest(&batch.result((x, z))),
                digest(&scalar.pre_ore_stage(x, z)),
                "({x}, {z})",
            );
        }
    }

    #[test]
    fn cold_sidecar_retains_warm_neighbour_cells_and_true_exterior_searches() {
        let generator = generator_with_biomes(true);
        assert_eq!(generator.store.len(), 0);
        let full = RegionBiomeSidecar::new(
            &generator, -4, -1, -4, -1, (-4, -1, -4, -1), None,
        );
        assert_eq!(generator.store.len(), 0, "cold sidecar inserted stage metadata");
        let warm = generator.pre_ore_stage(-2, -3);
        generator.store.entry((0, -3)).pre_ore.get_or_compute(
            |_| {}, || warm.as_ref().clone(),
        );
        let warm_store_len = generator.store.len();
        let cold = RegionBiomeSidecar::new(
            &generator, -1, -1, -4, -1, (-4, -1, -4, -1), None,
        );
        let misclassified = RegionBiomeSidecar::new(
            &generator, -1, -1, -4, -1, (-1, -1, -4, -1), None,
        );
        assert_eq!(generator.store.len(), warm_store_len, "sidecar inserted missing neighbours");
        assert!(Arc::ptr_eq(cold.cells_at(-2, -3).expect("warm neighbour cells"), &warm.3));
        assert!(misclassified.cells_at(-2, -3).is_none());
        assert!(cold.cells_at(0, -3).is_none());
        assert_eq!(full.cells.len(), 36);
        assert_eq!(cold.cells.len(), 18);
        for z in -4..0 {
            let base_x = -16;
            let base_z = z * 16;
            assert_eq!(
                cold.deep_biome_absent_mask(base_x, base_z),
                full.deep_biome_absent_mask(base_x, base_z),
            );
            for (x, z) in [(0, 0), (15, 0), (0, 15), (15, 15), (1, 14)] {
                for y in [-64, -1, 63, 318] {
                    assert_eq!(
                        cold.biome_at_typed(&generator, None, base_x + x, y, base_z + z).at_y(y),
                        full.biome_at_typed(&generator, None, base_x + x, y, base_z + z).at_y(y),
                    );
                }
            }
        }
    }

    #[test]
    fn region_origin_index_shares_candidate_probes_across_eight_by_eight_targets() {
        let positions = (0..8)
            .flat_map(|cz| (0..8).map(move |cx| (cx, cz)))
            .collect::<Vec<_>>();

        let scalar = generator();
        crate::counters::reset();
        for &(cx, cz) in &positions {
            let _ = scalar.structure_refs_stage(cx, cz);
        }
        let scalar_probes = crate::counters::snapshot().structure_candidate_cell_probes;

        let batched = generator();
        crate::counters::reset();
        let structure_sampler = StartSampler::new(&batched);
        let index = batched
            .structure_origin_index_for_bounds_with_sampler(
                -8,
                15,
                -8,
                15,
                &structure_sampler,
            )
            .expect("the synthetic structure set must build an origin index");
        for &(cx, cz) in &positions {
            let _ = batched.structure_refs_stage_with_index(cx, cz, Some(&index));
        }
        let batched_probes = crate::counters::snapshot().structure_candidate_cell_probes;

        assert_eq!(scalar_probes, 1_024, "scalar probe control changed");
        assert_eq!(batched_probes, 16, "the union index must probe each cell once");
        assert!(batched_probes < scalar_probes);
    }

    #[test]
    fn region_prefix_reuses_one_structure_sampler_for_all_targets() {
        let positions = [(0, 0), (1, 0)];

        let scalar = generator();
        crate::overworld::structures::reset_structure_cache_stats();
        let expected = positions.map(|(cx, cz)| digest(&scalar.pre_ore_stage(cx, cz)));
        let scalar_samplers =
            crate::overworld::structures::structure_cache_stats().sampler_constructions;

        let batched = generator();
        let preliminary = batched.preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
        crate::overworld::structures::reset_structure_cache_stats();
        let batch = RegionPrefixBatch::execute(
            &batched, &positions, position_bounds(&positions), &preliminary,
        );
        let batched_samplers =
            crate::overworld::structures::structure_cache_stats().sampler_constructions;

        assert_eq!(positions.map(|position| digest(&batch.result(position))), expected);
        assert_eq!(scalar_samplers, 6);
        assert_eq!(batched_samplers, 5);
        assert!(batched_samplers < scalar_samplers);
    }

    #[test]
    fn translated_dynamic_eight_by_eight_prefix_matches_four_admitted_tiles() {
        let positions = (-3..5)
            .flat_map(|z| (-5..3).map(move |x| (x, z)))
            .collect::<Vec<_>>();
        let expected = {
            let full = generator_with_biomes(true);
            assert!(full.dynamic_biome.as_ref().expect("dynamic biome fixture").table.len() > 100);
            let preliminary = full.preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
            let batch = RegionPrefixBatch::execute(
                &full, &positions, position_bounds(&positions), &preliminary,
            );
            positions.iter()
                .map(|&position| (position, digest(&batch.result(position))))
                .collect::<Vec<_>>()
        };

        let tiled = generator_with_biomes(true);
        let preliminary = tiled.preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
        let mut actual = Vec::with_capacity(positions.len());
        for (min_x, min_z) in [(-5, -3), (-1, -3), (-5, 1), (-1, 1)] {
            let tile = (min_z..min_z + 4)
                .flat_map(|z| (min_x..min_x + 4).map(move |x| (x, z)))
                .collect::<Vec<_>>();
            let batch = RegionPrefixBatch::execute(
                &tiled, &tile, position_bounds(&tile), &preliminary,
            );
            actual.extend(tile.iter().map(|&position| (position, digest(&batch.result(position)))));
        }
        actual.sort_unstable_by_key(|(position, _)| *position);
        assert_eq!(actual.len(), 64);
        assert_ne!(expected[0].1, expected[1].1);
        let mut mismatches = Vec::new();
        for &(position, full_digest) in &expected {
            let index = actual.binary_search_by_key(&position, |(position, _)| *position)
                .expect("tile omitted an admitted coordinate");
            let tiled_digest = actual[index].1;
            eprintln!("dynamic prefix {position:?}: full={full_digest:016x} tiled={tiled_digest:016x}");
            if full_digest != tiled_digest {
                mismatches.push((position, full_digest, tiled_digest));
            }
        }
        assert!(
            mismatches.is_empty(),
            "admitted tile borders changed prefixes within chunk bounds (-5, -3)..=(2, 4): {mismatches:?}",
        );
    }
}
