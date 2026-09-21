//! Stage 2 of [`OverworldGenerator::column`]: multi-noise biome resolution — the
//! per-quart surface-height sample plus the separate `y = 0` per-source-chunk answer
//! carver and ore selection use.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A. See [`crate::biome`] for the
//! sampler itself and its "y = 0 trap" section.

use crate::biome::{
    BiomeSearchCursor, BiomeTable, CachedBiomeAnswer, ClimateSampler, PreparedClimateGrid,
};
use lodestone_data::biomes::BiomeRef;
use sha2::{Digest as _, Sha256};
use std::sync::Arc;

use super::OverworldGenerator;
use super::biome_cells::BiomeCells;

/// Real multi-noise biome assignment, present on
/// [`OverworldGenerator`] whenever its [`Resolver`] supplies a non-empty
/// [`Resolver::biome_parameters`] table. See `crate::biome`'s module doc for
/// the resolution/height/excluded-biome decisions baked into this.
#[allow(missing_debug_implementations)]
pub(super) struct DynamicBiome {
    pub(super) climate: ClimateSampler,
    /// The climate rows **and** their search tree (Unit 9). Was a bare
    /// `Vec<BiomeParameterPoint>`; [`BiomeTable`] derefs to that slice, which is
    /// what keeps `super`'s own `d.table.iter()` and this struct's literal in
    /// `overworld/mod.rs` compiling untouched — see [`BiomeTable`]'s doc.
    pub(super) table: BiomeTable,
    pub(super) temperatures: std::collections::HashMap<String, f32>,
}

/// Lazy biome answers for the surface scan's expanded block footprint.
///
/// The wire grid is indexed directly by quart coordinates. Surface rules use a
/// nearby-corner selection instead, so this context keeps the extra border
/// cells and climate targets in typed caches while resolving each new quart
/// through the stateful tree. Consecutive references to the same quart reuse
/// its selected row without changing the cursor. The lookup sequence therefore
/// follows the surface walk's `x`, `z`, descending-`y` order, rather than a
/// separate qy-major prepass.
pub(super) struct SurfaceBiomeContext<'a> {
    min_qx: i32,
    min_qy: i32,
    min_qz: i32,
    width: usize,
    height: usize,
    depth: usize,
    /// Climate targets are pure for a quart coordinate, so retaining them
    /// avoids repeating the sampler arithmetic while still running the tree
    /// search for every reference biome lookup.
    targets: Vec<Option<[i64; 7]>>,
    answers: Vec<Option<CachedBiomeAnswer>>,
    climate: Option<&'a ClimateSampler>,
    prepared: Option<Arc<PreparedClimateGrid>>,
    table: Option<&'a BiomeTable>,
    fallback: &'a str,
    fiddle_lattice: ZoomFiddleLattice,
    cursor: Option<BiomeSearchCursor>,
    last_quart: Option<(usize, u32)>,
}

impl<'a> SurfaceBiomeContext<'a> {
    fn index(&self, qx: i32, qy: i32, qz: i32) -> usize {
        let x = usize::try_from(qx - self.min_qx).expect("surface biome x is precomputed");
        let y = usize::try_from(qy - self.min_qy).expect("surface biome y is precomputed");
        let z = usize::try_from(qz - self.min_qz).expect("surface biome z is precomputed");
        assert!(
            x < self.width && y < self.height && z < self.depth,
            "surface biome lookup escaped its precomputed footprint"
        );
        (y * self.depth + z) * self.width + x
    }

    fn quart(&mut self, qx: i32, qy: i32, qz: i32) -> &'a str {
        let index = self.index(qx, qy, qz);
        let row = match (&self.climate, &self.table, &mut self.cursor) {
            (Some(climate), Some(table), Some(cursor)) => {
                let target = self.targets[index].unwrap_or_else(|| {
                    let target = self.prepared.as_deref().map_or_else(
                        || climate.target(qx * 4, qy * 4, qz * 4),
                        |prepared| climate.target_prepared(prepared, qx * 4, qy * 4, qz * 4),
                    );
                    self.targets[index] = Some(target);
                    target
                });
                if let Some((last_index, row)) = self.last_quart
                    && last_index == index
                {
                    row
                } else {
                    let answer = *self.answers[index]
                        .get_or_insert_with(|| table.cached_answer(&target));
                    let row = table.apply_cached_answer(&target, cursor, answer);
                    self.last_quart = Some((index, row));
                    row
                }
            }
            (None, None, None) => 0,
            _ => panic!("surface biome context has incomplete dynamic state"),
        };
        self.table.map_or(self.fallback, |table| table.biome_at(row))
    }

    pub(super) fn at_block(&mut self, x: i32, y: i32, z: i32) -> &'a str {
        let (qx, qy, qz) = self.fiddle_lattice.selected_quart_at(x, y, z);
        self.quart(qx, qy, qz)
    }

    /// Performs the reference surface system's initial per-column lookup. The
    /// selected answer is not needed by the caller, but the query advances the
    /// stateful search cursor before the descending block scan begins.
    pub(super) fn touch_block(&mut self, x: i32, y: i32, z: i32) {
        let _ = self.at_block(x, y, z);
    }

    pub(super) fn into_cursor(self) -> Option<BiomeSearchCursor> {
        self.cursor
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ZoomFiddleStats {
    pub(super) vertex_hits: u64,
    pub(super) vertex_computes: u64,
}

/// Dense, request-scoped cache of the three pure offsets attached to each
/// quart vertex. It never stores climate targets or biome rows.
#[derive(Debug)]
pub(super) struct ZoomFiddleLattice {
    min_qx: i32,
    min_qy: i32,
    min_qz: i32,
    width: usize,
    height: usize,
    depth: usize,
    seed: i64,
    vertices: Vec<Option<[f64; 3]>>,
    #[cfg(test)]
    vertex_hits: u64,
    #[cfg(test)]
    vertex_computes: u64,
}

impl ZoomFiddleLattice {
    pub(super) fn for_block_bounds(
        seed: i64,
        min_x: i32,
        max_x: i32,
        min_y: i32,
        max_y: i32,
        min_z: i32,
        max_z: i32,
    ) -> Self {
        assert!(min_x <= max_x && min_y <= max_y && min_z <= max_z);
        let min_qx = (min_x - 2).div_euclid(4);
        let min_qy = (min_y - 2).div_euclid(4);
        let min_qz = (min_z - 2).div_euclid(4);
        let max_qx = (max_x - 2).div_euclid(4) + 1;
        let max_qy = (max_y - 2).div_euclid(4) + 1;
        let max_qz = (max_z - 2).div_euclid(4) + 1;
        let width = usize::try_from(max_qx - min_qx + 1).expect("zoom fiddle x footprint");
        let height = usize::try_from(max_qy - min_qy + 1).expect("zoom fiddle y footprint");
        let depth = usize::try_from(max_qz - min_qz + 1).expect("zoom fiddle z footprint");
        let len = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(depth))
            .expect("zoom fiddle footprint is too large");
        Self {
            min_qx,
            min_qy,
            min_qz,
            width,
            height,
            depth,
            seed,
            vertices: vec![None; len],
            #[cfg(test)]
            vertex_hits: 0,
            #[cfg(test)]
            vertex_computes: 0,
        }
    }

    pub(super) fn for_request_region(
        seed: i64,
        min_cx: i32,
        max_cx: i32,
        min_cz: i32,
        max_cz: i32,
        min_y: i32,
        height: i32,
    ) -> Self {
        assert!(height > 0);
        Self::for_block_bounds(
            seed,
            min_cx * 16,
            max_cx * 16 + 15,
            min_y,
            min_y + height - 1,
            min_cz * 16,
            max_cz * 16 + 15,
        )
    }

    fn index(&self, qx: i32, qy: i32, qz: i32) -> Option<usize> {
        let x = usize::try_from(qx - self.min_qx).ok()?;
        let y = usize::try_from(qy - self.min_qy).ok()?;
        let z = usize::try_from(qz - self.min_qz).ok()?;
        (x < self.width && y < self.height && z < self.depth)
            .then_some((y * self.depth + z) * self.width + x)
    }

    fn vertex(&mut self, qx: i32, qy: i32, qz: i32) -> [f64; 3] {
        let Some(index) = self.index(qx, qy, qz) else {
            return zoom_fiddle_vertex(self.seed, qx, qy, qz);
        };
        if let Some(value) = self.vertices[index] {
            #[cfg(test)]
            {
                self.vertex_hits += 1;
            }
            return value;
        }
        let value = zoom_fiddle_vertex(self.seed, qx, qy, qz);
        self.vertices[index] = Some(value);
        #[cfg(test)]
        {
            self.vertex_computes += 1;
        }
        value
    }

    pub(super) fn selected_corner(&mut self, x: i32, y: i32, z: i32) -> u8 {
        select_zoom_corner(x, y, z, |qx, qy, qz| self.vertex(qx, qy, qz))
    }

    pub(super) fn selected_quart_at(&mut self, x: i32, y: i32, z: i32) -> (i32, i32, i32) {
        let corner = self.selected_corner(x, y, z);
        (
            selected_quart(x, corner, 4),
            selected_quart(y, corner, 2),
            selected_quart(z, corner, 1),
        )
    }

    #[cfg(test)]
    pub(super) fn stats(&self) -> ZoomFiddleStats {
        ZoomFiddleStats {
            vertex_hits: self.vertex_hits,
            vertex_computes: self.vertex_computes,
        }
    }
}

fn next_zoom_random(value: i64, addend: i64) -> i64 {
    value
        .wrapping_mul(value.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407))
        .wrapping_add(addend)
}

fn zoom_fiddle(value: i64) -> f64 {
    let uniform = (value >> 24).rem_euclid(1024) as f64 / 1024.0;
    (uniform - 0.5) * 0.9
}

fn zoom_fiddle_vertex(seed: i64, x: i32, y: i32, z: i32) -> [f64; 3] {
    let mut value = seed;
    for coordinate in [x, y, z, x, y, z] {
        value = next_zoom_random(value, i64::from(coordinate));
    }
    let fx = zoom_fiddle(value);
    value = next_zoom_random(value, seed);
    let fy = zoom_fiddle(value);
    value = next_zoom_random(value, seed);
    let fz = zoom_fiddle(value);
    [fx, fy, fz]
}

fn selected_quart(block: i32, corner: u8, mask: u8) -> i32 {
    let parent = (block - 2).div_euclid(4);
    if corner & mask == 0 {
        parent
    } else {
        parent + 1
    }
}

fn select_zoom_corner<F>(x: i32, y: i32, z: i32, mut vertex: F) -> u8
where
    F: FnMut(i32, i32, i32) -> [f64; 3],
{
    let shifted_x = x - 2;
    let shifted_y = y - 2;
    let shifted_z = z - 2;
    let parent_x = shifted_x.div_euclid(4);
    let parent_y = shifted_y.div_euclid(4);
    let parent_z = shifted_z.div_euclid(4);
    let fract_x = f64::from(shifted_x.rem_euclid(4)) / 4.0;
    let fract_y = f64::from(shifted_y.rem_euclid(4)) / 4.0;
    let fract_z = f64::from(shifted_z.rem_euclid(4)) / 4.0;

    let mut selected = 0;
    let mut best = f64::INFINITY;
    for corner in 0..8 {
        let x_low = corner & 4 == 0;
        let y_low = corner & 2 == 0;
        let z_low = corner & 1 == 0;
        let qx = if x_low { parent_x } else { parent_x + 1 };
        let qy = if y_low { parent_y } else { parent_y + 1 };
        let qz = if z_low { parent_z } else { parent_z + 1 };
        let dx = if x_low { fract_x } else { fract_x - 1.0 };
        let dy = if y_low { fract_y } else { fract_y - 1.0 };
        let dz = if z_low { fract_z } else { fract_z - 1.0 };
        let [fx, fy, fz] = vertex(qx, qy, qz);
        let x = dx + fx;
        let y = dy + fy;
        let z = dz + fz;
        let distance = x * x + y * y + z * z;
        if best > distance {
            selected = corner;
            best = distance;
        }
    }
    selected
}

fn uncached_selected_corner(x: i32, y: i32, z: i32, seed: i64) -> u8 {
    select_zoom_corner(x, y, z, |qx, qy, qz| {
        zoom_fiddle_vertex(seed, qx, qy, qz)
    })
}

/// Resolves a block position through the same three-dimensional quart zoom
/// used by the world biome accessor. The wire biome grid stores the climate
/// answer at each quart corner, but a block lookup chooses one of the eight
/// surrounding corners after applying the seed-derived positional fiddle.
/// Keeping this here makes candidate biome predicates use the same category
/// lookup as surface rules instead of silently reading only the containing
/// quart cell.
pub(crate) fn zoomed_biome_ref<'a, F>(
    zoom_seed: i64,
    x: i32,
    y: i32,
    z: i32,
    source_at: F,
) -> Option<BiomeRef>
where
    F: Fn(i32, i32) -> Option<&'a BiomeCells>,
{
    let selected = uncached_selected_corner(x, y, z, zoom_seed);
    zoomed_biome_ref_from_corner(selected, x, y, z, source_at)
}

pub(super) fn zoomed_biome_ref_with_lattice<'a, F>(
    lattice: &mut ZoomFiddleLattice,
    x: i32,
    y: i32,
    z: i32,
    source_at: F,
) -> Option<BiomeRef>
where
    F: Fn(i32, i32) -> Option<&'a BiomeCells>,
{
    let selected = lattice.selected_corner(x, y, z);
    zoomed_biome_ref_from_corner(selected, x, y, z, source_at)
}

pub(super) fn zoomed_biome_ref_from_corner<'a, F>(
    selected: u8,
    x: i32,
    y: i32,
    z: i32,
    source_at: F,
) -> Option<BiomeRef>
where
    F: Fn(i32, i32) -> Option<&'a BiomeCells>,
{
    let qx = selected_quart(x, selected, 4);
    let qy = selected_quart(y, selected, 2);
    let qz = selected_quart(z, selected, 1);
    let block_x = qx * 4;
    let block_z = qz * 4;
    let cells = source_at(block_x.div_euclid(16), block_z.div_euclid(16))?;
    let local_qx = block_x.rem_euclid(16).div_euclid(4) as usize;
    let local_qz = block_z.rem_euclid(16).div_euclid(4) as usize;
    let local_qy = (qy * 4 - cells.min_y()).div_euclid(4);
    // Height ranges may choose candidates outside the generated window. Keep
    // the body in the stream and use the same edge-layer clamp as `at_block`;
    // a missing horizontal source remains the `None` case above.
    Some(cells.at_quart_ref(local_qx, local_qy.max(0) as usize, local_qz))
}

/// Resolves the same seed-fiddled quart corner for a vertically invariant
/// biome source. The callback receives the selected chunk and local quart.
pub(crate) fn zoomed_biome_flat<T, F>(
    zoom_seed: i64,
    x: i32,
    y: i32,
    z: i32,
    source_at: F,
) -> Option<T>
where
    F: Fn(i32, i32, usize, usize) -> Option<T>,
{
    let selected = uncached_selected_corner(x, y, z, zoom_seed);
    let qx = selected_quart(x, selected, 4);
    let qz = selected_quart(z, selected, 1);
    let block_x = qx * 4;
    let block_z = qz * 4;
    source_at(
        block_x.div_euclid(16),
        block_z.div_euclid(16),
        block_x.rem_euclid(16).div_euclid(4) as usize,
        block_z.rem_euclid(16).div_euclid(4) as usize,
    )
}

fn zoom_seed(seed: i64) -> i64 {
    let digest = Sha256::digest(seed.to_le_bytes());
    i64::from_le_bytes(digest[..8].try_into().expect("SHA-256 digest prefix"))
}

pub(crate) fn biome_zoom_seed(seed: i64) -> i64 {
    zoom_seed(seed)
}

impl OverworldGenerator {
    /// Prepares the surface biome footprint once for this request. The returned
    /// grid includes the one-quart border selected by the block-position zoom;
    /// the centre biome-cell stage takes a bounded view over the same storage.
    pub(super) fn prepare_climate_grid(
        &self,
        base_x: i32,
        base_z: i32,
    ) -> Option<Arc<PreparedClimateGrid>> {
        let dynamic = self.dynamic_biome.as_ref()?;
        crate::counters::bump_climate_grid_preparation();
        let min_qx = (base_x - 2).div_euclid(4);
        let max_qx = (base_x + 15 - 2).div_euclid(4) + 1;
        let min_qz = (base_z - 2).div_euclid(4);
        let max_qz = (base_z + 15 - 2).div_euclid(4) + 1;
        let width = usize::try_from(max_qx - min_qx + 1).expect("surface biome x footprint");
        let depth = usize::try_from(max_qz - min_qz + 1).expect("surface biome z footprint");
        Some(Arc::new(
            dynamic
                .climate
                .prepare_xz_rect(min_qx, min_qz, width, depth),
        ))
    }

    /// Precomputes the raw quart cells that the surface scan's zoomed biome
    /// lookup can select. The extra one-cell border comes from shifting the
    /// block position before choosing between adjacent corners. A caller may
    /// supply the request's bordered climate grid to avoid preparing it again.
    pub(super) fn surface_biome_context(
        &self,
        base_x: i32,
        base_z: i32,
        cursor: Option<BiomeSearchCursor>,
        prepared: Option<Arc<PreparedClimateGrid>>,
    ) -> SurfaceBiomeContext<'_> {
        let min_qx = (base_x - 2) >> 2;
        let max_qx = ((base_x + 15 - 2) >> 2) + 1;
        let min_qz = (base_z - 2) >> 2;
        let max_qz = ((base_z + 15 - 2) >> 2) + 1;
        let min_qy = (self.min_y - 2) >> 2;
        let max_qy = ((self.min_y + self.height - 2) >> 2) + 1;
        let width = usize::try_from(max_qx - min_qx + 1).expect("surface biome x footprint");
        let height = usize::try_from(max_qy - min_qy + 1).expect("surface biome y footprint");
        let depth = usize::try_from(max_qz - min_qz + 1).expect("surface biome z footprint");
        let prepared = prepared.or_else(|| self.prepare_climate_grid(base_x, base_z));
        SurfaceBiomeContext {
            min_qx,
            min_qy,
            min_qz,
            width,
            height,
            depth,
            targets: vec![None; width * height * depth],
            answers: vec![None; width * height * depth],
            climate: self.dynamic_biome.as_ref().map(|dynamic| &dynamic.climate),
            prepared,
            table: self.dynamic_biome.as_ref().map(|dynamic| &dynamic.table),
            fallback: &self.fallback_biome,
            fiddle_lattice: ZoomFiddleLattice::for_block_bounds(
                zoom_seed(self.seed),
                base_x,
                base_x + 15,
                self.min_y,
                self.min_y + self.height - 1,
                base_z,
                base_z + 15,
            ),
            cursor,
            last_quart: None,
        }
    }

    /// Stage 2: the 16 **surface** quarts —
    /// `(qx, qz)` in `0..4`, row-major `qz * 4 + qx`, matching `ChunkSection`'s own
    /// `BIOME_EDGE` of 4 — each read at that quart's own already-generated surface
    /// height.
    ///
    /// **This no longer samples anything.** The full biome grid made
    /// [`Self::biome_cells_stage`]'s 4×4×4 grid the primary product, and this reads
    /// the layer each quart's surface falls in. That is exact, not an
    /// approximation: the surface sample height is `(height >> 2) << 2`, already
    /// quart-aligned, so it is by construction one of that grid's own layers. If
    /// you change either sample convention, change both — a divergence would show
    /// up as surface vegetation being chosen from a biome the served chunk does not
    /// report.
    ///
    /// Why a *surface* array still exists alongside the grid: the module doc's
    /// "y = 0 trap". Surface material, carve and decorate each ask a specific,
    /// different question about Y, and having the grid does not license collapsing
    /// them onto one answer — see [`super::biome_cells`].
    pub(super) fn biome_stage(
        &self,
        cells: &BiomeCells,
        heights: &[i32; 256],
    ) -> [(String, bool); 16] {
        std::array::from_fn(|i| {
            let qx = i % 4;
            let qz = i / 4;
            let (lx, lz) = (qx as i32 * 4, qz as i32 * 4);
            let y = (heights[(lz * 16 + lx) as usize] >> 2) << 2;
            let qy = ((y - self.min_y) >> 2).max(0) as usize;
            let name = cells.at_quart(qx, qy, qz);
            let cold = match &self.dynamic_biome {
                Some(d) => crate::biome::cold_enough_to_snow(&d.temperatures, name),
                None => self.fallback_cold_enough_to_snow,
            };
            (name.to_string(), cold)
        })
    }

    /// Stage 2b: the **full** 4×4×4 biome grid for this column —
    /// `16 × height/4` cells, one vanilla multi-noise biome lookup per
    /// quart-position cell, which is what vanilla's own level-chunk-section's biome container holds.
    ///
    /// [`Self::biome_stage`]'s 16-entry surface array is the *same data* read at
    /// one Y per column — that function takes this grid as a parameter rather than
    /// sampling again; see [`super::biome_cells`]'s module doc for why that is
    /// exact and not an approximation.
    ///
    /// Falls back to a single-biome column when the resolver supplied no climate
    /// table, matching [`Self::biome_stage`]'s own per-quart degradation.
    pub(super) fn biome_cells_stage(
        &self,
        base_x: i32,
        base_z: i32,
        cursor: Option<&mut BiomeSearchCursor>,
    ) -> BiomeCells {
        self.biome_cells_stage_with_prepared(base_x, base_z, cursor, None)
    }

    /// Resolves biome cells from the centre view of a request's bordered grid.
    pub(super) fn biome_cells_stage_with_prepared(
        &self,
        base_x: i32,
        base_z: i32,
        mut cursor: Option<&mut BiomeSearchCursor>,
        prepared: Option<&PreparedClimateGrid>,
    ) -> BiomeCells {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Biome);
        let Some(dynamic) = &self.dynamic_biome else {
            return BiomeCells::uniform_strict(&self.fallback_biome, self.min_y, self.height);
        };
        let owned_prepared;
        let prepared = match prepared {
            Some(prepared) => prepared,
            None => {
                owned_prepared = self
                    .prepare_climate_grid(base_x, base_z)
                    .expect("dynamic biome stage requires a climate grid");
                owned_prepared.as_ref()
            }
        };
        let prepared = prepared.center_view();
        BiomeCells::from_fn_section_query_order_typed(self.min_y, self.height, |qx, qy, qz| {
            // Quart *corner*, not centre — the convention `biome_stage`'s own
            // comment records as having matched a real dark_forest/river boundary
            // where the centre convention did not. Applied to Y as well, which is
            // why `min_y` has to be quart-aligned for this to be exact (it is:
            // -64 >> 2 << 2 == -64).
            let y = self.min_y + (qy as i32) * 4;
            let target = dynamic.climate.target_prepared_view(
                &prepared,
                base_x + qx as i32 * 4,
                y,
                base_z + qz as i32 * 4,
            );
            let row = dynamic.table.nearest_row_with_cursor(
                &target,
                cursor
                    .as_deref_mut()
                    .expect("dynamic biome stage requires a search cursor"),
            );
            dynamic
                .table
                .biome_ref_at(row)
                .expect("strict worldgen table contains only generated built-in biomes")
        })
    }

    /// **Diagnostic for the R-tree ruling, not used by generation.** At one source chunk's own
    /// `y = 0` climate target, does vanilla's indexed search (what
    /// [`Self::biome_for_carver_source`] now uses) resolve to a different biome id
    /// than the brute-force scan it used before?
    ///
    /// This exists because "no gate failed" and "nothing changed" are different
    /// claims, and only the second one is worth reporting. The 0.98% divergence
    /// figure is measured over *arbitrary* climate targets; whether real generated
    /// climate at real coordinates ever lands on an exact tie is a separate
    /// question that no existing gate answers, so this lets one count it directly.
    /// Returns `(vanilla's indexed answer, the brute-force answer)`, equal when the
    /// tie-break makes no difference here, or `None` for a fixed-biome generator
    /// (which has no table to search). Returning both *names* rather than a bool is
    /// what lets a gate print coordinates and expected values a JVM oracle run can
    /// be pointed straight at.
    #[must_use]
    pub fn source_biome_tiebreak(&self, source_cx: i32, source_cz: i32) -> Option<(&str, &str)> {
        let d = self.dynamic_biome.as_ref()?;
        let target = d.climate.target(source_cx * 16, 0, source_cz * 16);
        let tree = d.table.nearest_row_stateless(&target);
        let brute = crate::biome::nearest_row_brute_force(&d.table, &target);
        Some((d.table.biome_at(tree), d.table.biome_at(brute)))
    }

    /// Replays the production 4×4 horizontal query order through one requested
    /// quart layer and returns `(stateful, stateless)` for that cell. This is a
    /// parity fixture: the first component includes the lifecycle cursor's
    /// previous-leaf history, while the second is the fresh indexed negative control.
    #[must_use]
    pub fn biome_cell_search_fixture(
        &self,
        cx: i32,
        cz: i32,
        qx: usize,
        qy: usize,
        qz: usize,
    ) -> Option<(&str, &str)> {
        if qx >= 4 || qz >= 4 {
            return None;
        }
        let d = self.dynamic_biome.as_ref()?;
        let mut cursor = d.table.search_cursor();
        let mut stateful = None;
        for section_layer in (0..=qy).step_by(4) {
            for x in 0..4usize {
                for local_y in 0..4usize {
                    let layer = section_layer + local_y;
                    if layer > qy {
                        break;
                    }
                    for z in 0..4usize {
                        let y = self.min_y + layer as i32 * 4;
                        let target = d.climate.target(cx * 16 + x as i32 * 4, y, cz * 16 + z as i32 * 4);
                    let row = d.table.nearest_row_with_cursor(&target, &mut cursor);
                        if layer == qy && x == qx && z == qz {
                            stateful = Some(d.table.biome_at(row));
                        }
                    }
                }
            }
        }
        let y = self.min_y + qy as i32 * 4;
        let target = d.climate.target(cx * 16 + qx as i32 * 4, y, cz * 16 + qz as i32 * 4);
        let stateless = d.table.biome_at(d.table.nearest_row_stateless(&target));
        Some((stateful.expect("requested quart was visited"), stateless))
    }

    /// The same question for the 16 **surface** quarts of one chunk — the biome a
    /// player standing there sees. Returns `(quarts, differing)`.
    ///
    /// Needs the chunk's own generated heights, so it runs the pre-ore pipeline via
    /// the store exactly as [`Self::biome_stage`]'s caller does; the sample height
    /// convention is therefore identical to production's rather than a restatement
    /// of it.
    #[must_use]
    pub fn surface_biome_tiebreak_differences(&self, cx: i32, cz: i32) -> (usize, usize) {
        let Some(d) = self.dynamic_biome.as_ref() else {
            return (0, 0);
        };
        let cached = self.pre_ore_stage(cx, cz);
        let heights = &cached.1;
        let base_x = cx * 16;
        let base_z = cz * 16;
        let mut differing = 0usize;
        for i in 0..16usize {
            let qx = (i % 4) as i32;
            let qz = (i / 4) as i32;
            let lx = qx * 4;
            let lz = qz * 4;
            let y = (heights[(lz * 16 + lx) as usize] >> 2) << 2;
            let target = d.climate.target(base_x + lx, y, base_z + lz);
            let tree = d.table.nearest_row_stateless(&target);
            let brute = crate::biome::nearest_row_brute_force(&d.table, &target);
            if d.table.biome_at(tree) != d.table.biome_at(brute) {
                differing += 1;
            }
        }
        (16, differing)
    }

    /// Biome for one *source chunk* in the carve neighbourhood — vanilla's
    /// real per-source-chunk carver-biome resolution (its own carve-application step):
    /// sampled at the source chunk's own quart corner (vanilla's own
    /// quart-from-block conversion
    /// of its min block X/Z, which is `source_cx * 16` / `source_cz * 16` —
    /// already quart-aligned since 16 is a multiple of 4, so no extra
    /// rounding is needed) and **`y = 0`**, not the source chunk's surface
    /// height. This is deliberately not [`Self::biome_stage`]'s question:
    /// carver *selection* and surface *material* sample the same climate
    /// fields at different heights and get different (correct) answers —
    /// see `docs/worldgen-parity.md`'s description of `ComposedChunkOracle
    /// .java`'s own `sourceBiome` resolution, which this reproduces exactly.
    ///
    /// # Unit 9: this is the D5 call site
    ///
    /// Called **289 times per pre-ore chunk** by `carve_stage` (`carver`'s
    /// `NEIGHBOURHOOD_RANGE = 8` ⇒ a 17×17 source window) and 9 more times per
    /// post-ore chunk by `ore_stage`, and the answer depends only on
    /// `(this generator, source_cx, source_cz)` — the requesting centre never
    /// enters it. So adjacent chunks' windows, which overlap in 272 of 289
    /// positions, were recomputing the same climate search. That is
    /// `docs/plans/worldgen-rewrite.md`'s D5: ~2.2M squared-distance comparisons
    /// per pre-ore chunk.
    ///
    /// Both halves of the fix are visible in the four lines below:
    /// [`crate::biome::memo`] answers a repeated `(cx, cz)` without searching at
    /// all, and a real search goes through the tree rather than the full-table
    /// scan. The signature is unchanged — still `-> &str` borrowed from `self` —
    /// which is why `carve_stage` and `ore_stage` needed no edit: the memo stores
    /// a **table row**, and the row indexes back into this generator's own table.
    pub(super) fn biome_for_carver_source(
        &self,
        source_cx: i32,
        source_cz: i32,
        _cursor: &mut BiomeSearchCursor,
    ) -> &str {
        match &self.dynamic_biome {
            None => self.fallback_biome.as_str(),
            Some(d) => {
                let row = crate::biome::memo::source_row(d.table.id(), source_cx, source_cz, || {
                    let target = d.climate.target(source_cx * 16, 0, source_cz * 16);
                    d.table.nearest_row_stateless(&target)
                });
                d.table.biome_at(row)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BiomeCells, BiomeTable, SurfaceBiomeContext, ZoomFiddleLattice, ZoomFiddleStats,
        uncached_selected_corner, zoom_seed, zoomed_biome_ref,
    };
    use crate::biome::{BiomeParameterPoint, ClimateSampler, Parameter};
    use crate::density::{Builder, NoiseParams, Resolver};
    use lodestone_data::biomes::{BiomeRef, BuiltinBiome};
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct SupportResolver {
        root: PathBuf,
    }

    impl SupportResolver {
        fn read(&self, kind: &str, id: &str) -> Value {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let path = self.root.join(kind).join(format!("{name}.json"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
        }
    }

    impl Resolver for SupportResolver {
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
    }

    #[test]
    fn repeated_surface_quarts_preserve_prepared_and_direct_sequence() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
        let resolver = SupportResolver { root: root.clone() };
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("noise_settings/overworld.json"))
                .expect("read overworld settings"),
        )
        .expect("parse overworld settings");
        let builder = Builder::new(42, &resolver);
        let climate = ClimateSampler::new(&settings, &builder);
        let table_json: Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../lodestone-server/assets/worldgen/biome_parameters/overworld.json"),
            )
            .expect("read overworld biome parameters"),
        )
        .expect("parse overworld biome parameters");
        let table = BiomeTable::new(crate::biome::parse_table(&table_json));

        let make_context = |prepared| SurfaceBiomeContext {
            min_qx: -1,
            min_qy: -17,
            min_qz: -1,
            width: 6,
            height: 98,
            depth: 6,
            targets: vec![None; 6 * 98 * 6],
            answers: vec![None; 6 * 98 * 6],
            climate: Some(&climate),
            prepared,
            table: Some(&table),
            fallback: "minecraft:plains",
            fiddle_lattice: ZoomFiddleLattice::for_block_bounds(
                zoom_seed(42), 0, 15, -64, 319, 0, 15,
            ),
            cursor: Some(table.search_cursor()),
            last_quart: None,
        };
        let mut direct = make_context(None);
        let mut prepared = make_context(Some(Arc::new(climate.prepare_xz_rect(-1, -1, 6, 6))));
        let sequence = [
            (0, -64, 0),
            (0, -64, 0),
            (1, -64, 1),
            (1, -64, 1),
            (0, -64, 0),
            (15, 0, 15),
            (15, 0, 15),
            (0, 0, 0),
        ];
        for &(x, y, z) in &sequence {
            assert_eq!(
                prepared.at_block(x, y, z),
                direct.at_block(x, y, z),
                "prepared surface answer changed at ({x}, {y}, {z})"
            );
        }
    }

    #[test]
    fn cached_answer_preserves_seeded_ties_across_a_query_sequence() {
        let wide = Parameter { min: -10_000, max: 10_000 };
        let tied = BiomeParameterPoint {
            params: [wide, wide, wide, wide, wide, wide, Parameter { min: 0, max: 0 }],
            biome: "minecraft:cold".to_owned(),
        };
        let table = BiomeTable::new(vec![
            tied.clone(),
            BiomeParameterPoint {
                biome: "minecraft:hot".to_owned(),
                ..tied
            },
        ]);
        let targets = [
            [0; 7],
            [5_000, 0, 0, 0, 0, 0, 0],
            [0; 7],
            [-5_000, 0, 0, 0, 0, 0, 0],
        ];
        let answers = targets.map(|target| table.cached_answer(&target));
        let mut expected = table.search_cursor();
        let mut actual = table.search_cursor();
        table.cursor_from_row(&mut expected, 1);
        table.cursor_from_row(&mut actual, 1);
        for (target, answer) in targets.iter().zip(answers) {
            assert_eq!(
                table.apply_cached_answer(target, &mut actual, answer),
                table.nearest_row_with_cursor(target, &mut expected),
            );
        }
    }

    #[test]
    fn zoom_seed_matches_the_independently_captured_seed_42_value() {
        assert_eq!(zoom_seed(42), -4_111_196_313_959_201_555);
    }

    #[test]
    fn block_zoom_selects_the_external_seed_42_candidate_biome() {
        // Independent JVM evidence for (-1907, 24, -1914) reports badlands.
        // The direct climate cell at (-477, 6, -479) reports dripstone_caves;
        // only the nearby-cell zoom can distinguish these two answers. Make
        // the externally selected corner the sole badlands cell so this test
        // exercises the selector rather than the table search.
        let cells = BiomeCells::from_fn(-64, 384, |qx, qy, qz| {
            if (qx, qy, qz) == (3, 21, 2) {
                "minecraft:badlands".to_string()
            } else {
                "minecraft:dripstone_caves".to_string()
            }
        });
        assert_eq!(cells.at_quart(3, 22, 1), "minecraft:dripstone_caves");
        let got = zoomed_biome_ref(zoom_seed(42), -1907, 24, -1914, |_, _| Some(&cells));
        assert_eq!(got, Some(BiomeRef::builtin(BuiltinBiome::Badlands)));
    }

    #[test]
    fn block_zoom_clamps_vertical_cells_but_not_missing_horizontal_sources() {
        let cells = BiomeCells::from_fn(-64, 8, |_, qy, _| {
            if qy == 0 {
                "minecraft:plains".to_string()
            } else {
                "minecraft:badlands".to_string()
            }
        });
        let below = zoomed_biome_ref(zoom_seed(42), 0, cells.min_y() - 64, 0, |_, _| {
            Some(&cells)
        });
        assert_eq!(below, Some(BiomeRef::builtin(BuiltinBiome::Plains)));

        let above = zoomed_biome_ref(
            zoom_seed(42),
            0,
            cells.min_y() + cells.y_quarts() as i32 * 4 + 64,
            0,
            |_, _| Some(&cells),
        );
        assert_eq!(above, Some(BiomeRef::builtin(BuiltinBiome::Badlands)));

        assert_eq!(
            zoomed_biome_ref(zoom_seed(42), 0, cells.min_y(), 0, |_, _| None),
            None
        );
    }

    #[test]
    fn zoom_corner_bits_hold_across_negative_quart_seams() {
        let seed = zoom_seed(42);
        for (position, expected) in [
            ((-1, -64, -1), 0b010),
            ((-4, -64, -4), 0b100),
            ((-16, 0, -16), 0b001),
            ((0, -64, 0), 0b100),
            ((15, -64, 15), 0b001),
        ] {
            assert_eq!(
                uncached_selected_corner(position.0, position.1, position.2, seed),
                expected,
                "selected corner changed at {position:?}",
            );
        }
    }

    #[test]
    fn zoom_fiddle_lattice_is_an_ab_control_for_selection_and_vertex_work() {
        let seed = zoom_seed(42);
        let mut lattice = ZoomFiddleLattice::for_block_bounds(seed, -16, 15, -64, 63, -16, 15);
        let first = (-1, -64, -1);
        let second = (3, -64, -1);
        assert_eq!(
            lattice.selected_corner(first.0, first.1, first.2),
            uncached_selected_corner(first.0, first.1, first.2, seed),
        );
        assert_eq!(
            lattice.selected_corner(first.0, first.1, first.2),
            uncached_selected_corner(first.0, first.1, first.2, seed),
        );
        assert_eq!(
            lattice.selected_corner(second.0, second.1, second.2),
            uncached_selected_corner(second.0, second.1, second.2, seed),
        );
        assert_eq!(
            lattice.stats(),
            ZoomFiddleStats {
                vertex_hits: 12,
                vertex_computes: 12,
            },
        );
    }
}
