//! Version-free port of vanilla's own noise-based aquifer and the
//! chunk-generator fill step.
//!
//! This is the stage *below* surface rules. Vanilla's own per-chunk noise
//! field produces an
//! interpolated `final_density` field; the fill step then asks the aquifer, for every
//! block, its own compute-substance call, which decides whether the block
//! is solid (the default block, stone), a fluid (water/lava), or air. The result
//! is the **pre-surface column** that [`crate::surface::SurfaceSystem`] consumes.
//!
//! The aquifer is not "water below sea level": it builds *local* water tables
//! and air pockets from four noise fields (barrier, floodedness, spread, lava)
//! and a positional aquifer-centre RNG, and it can override the density decision
//! by pushing a barrier pressure back into `density + barrier`. Approximating it
//! looks right on a surface screenshot and is wrong everywhere underground, so
//! this is a faithful port checked block-for-block against the JVM.
//!
//! # Version split (plan §3)
//!
//! The engine is data-driven and version-free: it interprets the `noise_router`
//! routes (`barrier`, `fluid_level_floodedness`, `fluid_level_spread`, `lava`,
//! `erosion`, `depth`, `preliminary_surface_level`, `final_density`) and the
//! `sea_level` from the supplied `noise_settings`. It returns a [`BlockKind`]
//! enum, never a block string — the caller maps that to canonical block states
//! from the version's block data, exactly like the surface system.
//!
//! # Parity discipline (plan §11)
//!
//! No Mojang source is transliterated: this is written from the documented
//! algorithm and checked against the running server's own fill-step output
//! (`scripts/worldgen-oracle/SurfaceOracle.java` dumps the `pre.*` column). The
//! test compares block-for-block over a whole chunk column and names the
//! divergent `x,y,z`.

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Condvar, Mutex};

use serde_json::Value;

use crate::density::{Builder, Context as DfContext, Density, NoiseChunkSampler};
use crate::engine::{Bounds, PointProgram, PointScratch, Program, XzProductLattice};
use crate::math::{clamp, clamped_map, floor, map};
use crate::rng::{PositionalRandomFactory, RandomSource};
pub use crate::rng::AnyPositionalFactory;

/// Vanilla's own noise-settings derived cell size — its own quart-to-block
/// conversion applied to `size_horizontal` and `size_vertical`, i.e. `size * 4`.
///
/// **Not the same for every dimension.** The Overworld and the Nether are
/// `1, 2` → 4 wide / 8 tall; **the End is `2, 1` → 8 wide / 4 tall**.
/// Interpolation happens on cell corners, so a wrong cell size does not fail —
/// it produces smoothly wrong terrain. Callers must carry this pair through
/// every sampler they construct for a settings document.
#[must_use]
pub fn cell_geometry(settings: &Value) -> (i32, i32) {
    let noise = &settings["noise"];
    let horizontal = noise["size_horizontal"].as_i64().unwrap_or(1) as i32;
    let vertical = noise["size_vertical"].as_i64().unwrap_or(2) as i32;
    (horizontal * 4, vertical * 4)
}

/// Vanilla's own "way below min Y" sentinel for the standard 1.18+ height (`MIN_Y << 4`,
/// `MIN_Y = -2032`). Used as the "no fluid here" sentinel level.
const WAY_BELOW_MIN_Y: i32 = -2032 << 4;

/// The block a filled position resolves to. Version-free: the caller maps each
/// variant to a canonical block string (the default block for [`Stone`], the
/// dimension's fluids for the rest).
///
/// [`Stone`]: BlockKind::Stone
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// A solid block — vanilla writes the settings' `default_block` (stone).
    Stone,
    /// Air (vanilla's own compute-substance call returned an air fluid state).
    Air,
    /// A settings-selected fluid (water, lava, or air).
    Water,
    /// Lava.
    Lava,
}

/// `settings.defaultFluid()` as a [`BlockKind`] — water for the Overworld, lava
/// for the Nether. Reads only the `Name`, since the fluid's `level` property is
/// not part of the identity the fill decision turns on.
///
/// `minecraft:air` is a real answer, not a missing one: `noise_settings/end.json`
/// says exactly that (with `sea_level: 0`), so the End's global picker returns air
/// at every height and the dimension has no fluid at all.
///
/// # Panics
/// Panics on anything else: this engine's [`BlockKind`] has no fourth fluid, and
/// defaulting would silently fill a dimension with the wrong liquid.
#[must_use]
pub fn fluid_from_settings(settings: &Value) -> BlockKind {
    match settings["default_fluid"]["Name"].as_str() {
        Some("minecraft:lava") => BlockKind::Lava,
        Some("minecraft:water") | None => BlockKind::Water,
        Some("minecraft:air") => BlockKind::Air,
        Some(other) => panic!("unsupported default_fluid: {other}"),
    }
}

/// The fluid identity carried inside vanilla's `Aquifer.FluidStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fluid {
    Air,
    Water,
    Lava,
}

impl Fluid {
    fn from_block(block: BlockKind) -> Self {
        match block {
            BlockKind::Air => Self::Air,
            BlockKind::Water => Self::Water,
            BlockKind::Lava => Self::Lava,
            BlockKind::Stone => panic!("default_fluid is not a fluid: Stone"),
        }
    }

    fn to_block(self) -> BlockKind {
        match self {
            Fluid::Air => BlockKind::Air,
            Fluid::Water => BlockKind::Water,
            Fluid::Lava => BlockKind::Lava,
        }
    }
}

/// Vanilla `Aquifer.FluidStatus`: a fluid type and the y below which it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FluidStatus {
    fluid_level: i32,
    fluid_type: Fluid,
}

impl FluidStatus {
    fn at(self, block_y: i32) -> Fluid {
        if block_y < self.fluid_level {
            self.fluid_type
        } else {
            Fluid::Air
        }
    }
}

enum PointDensity {
    SimpleNoise { node: Arc<Density>, kind: usize },
    Tree(Arc<Density>),
    Compiled {
        program: Arc<PointProgram>,
        scratch: RefCell<PointScratch>,
    },
}

/// Generator-owned point evaluators shared by every chunk-bound aquifer.
/// Unsupported opaque leaves stay on the source tree path instead of being
/// compiled into a program that would immediately recurse back into it.
#[derive(Clone)]
pub(crate) struct CompiledAquiferPointRoutes {
    pub(crate) barrier: Option<Arc<PointProgram>>,
    pub(crate) floodedness: Option<Arc<PointProgram>>,
    pub(crate) spread: Option<Arc<PointProgram>>,
    pub(crate) lava: Option<Arc<PointProgram>>,
}

impl CompiledAquiferPointRoutes {
    pub(crate) fn from_trees(
        barrier: &Arc<Density>,
        floodedness: &Arc<Density>,
        spread: &Arc<Density>,
        lava: &Arc<Density>,
    ) -> Self {
        Self {
            barrier: compile_point_route(barrier),
            floodedness: compile_point_route(floodedness),
            spread: compile_point_route(spread),
            lava: compile_point_route(lava),
        }
    }
}

fn compile_point_route(node: &Arc<Density>) -> Option<Arc<PointProgram>> {
    (!matches!(node.as_ref(), Density::Noise { .. }) && point_route_is_compilable(node))
        .then(|| Arc::new(PointProgram::compile(node)))
}

fn point_route_is_compilable(node: &Density) -> bool {
    match node {
        Density::Blended(_) | Density::EndIslands(_) => false,
        Density::Add(left, right)
        | Density::Mul(left, right)
        | Density::Min(left, right)
        | Density::Max(left, right) => {
            point_route_is_compilable(left) && point_route_is_compilable(right)
        }
        Density::Abs(inner)
        | Density::Square(inner)
        | Density::Cube(inner)
        | Density::HalfNegative(inner)
        | Density::QuarterNegative(inner)
        | Density::Squeeze(inner)
        | Density::Invert(inner)
        | Density::Interpolated { inner, .. }
        | Density::FlatCache { inner, .. }
        | Density::Cache2D { inner, .. }
        | Density::Marker(inner) => point_route_is_compilable(inner),
        Density::Clamp { input, .. } => point_route_is_compilable(input),
        Density::ShiftedNoise {
            shift_x,
            shift_y,
            shift_z,
            ..
        } => {
            point_route_is_compilable(shift_x)
                && point_route_is_compilable(shift_y)
                && point_route_is_compilable(shift_z)
        }
        Density::RangeChoice {
            input,
            when_in_range,
            when_out_of_range,
            ..
        } => {
            point_route_is_compilable(input)
                && point_route_is_compilable(when_in_range)
                && point_route_is_compilable(when_out_of_range)
        }
        Density::IntervalSelect {
            input, functions, ..
        } => {
            point_route_is_compilable(input)
                && functions.iter().all(point_route_is_compilable)
        }
        Density::Spline(spline) => spline_is_compilable(spline),
        Density::FindTopSurface {
            density,
            upper_bound,
            ..
        } => point_route_is_compilable(density) && point_route_is_compilable(upper_bound),
        Density::Const(_)
        | Density::BlendAlpha
        | Density::BlendOffset
        | Density::Beardifier
        | Density::YClampedGradient { .. }
        | Density::Noise { .. }
        | Density::ShiftA(_)
        | Density::ShiftB(_)
        | Density::Shift(_) => true,
    }
}

fn spline_is_compilable(spline: &crate::density::Spline) -> bool {
    match spline {
        crate::density::Spline::Constant(_) => true,
        crate::density::Spline::Multipoint { coordinate, points } => {
            point_route_is_compilable(coordinate)
                && points.iter().all(|point| spline_is_compilable(&point.value))
        }
    }
}

impl PointDensity {
    fn from_arc(node: Arc<Density>) -> Self {
        if matches!(node.as_ref(), Density::Noise { .. }) {
            let kind = node.kind_index();
            Self::SimpleNoise { node, kind }
        } else {
            Self::Tree(node)
        }
    }

    fn from_arc_and_program(node: Arc<Density>, program: Option<Arc<PointProgram>>) -> Self {
        if matches!(node.as_ref(), Density::Noise { .. }) {
            Self::from_arc(node)
        } else if let Some(program) = program {
            Self::from_program(program)
        } else {
            Self::from_arc(node)
        }
    }

    fn from_program(program: Arc<PointProgram>) -> Self {
        Self::Compiled {
            program,
            scratch: RefCell::new(PointScratch::new()),
        }
    }

    #[cfg(test)]
    fn is_compiled(&self) -> bool {
        matches!(self, Self::Compiled { .. })
    }

    #[inline]
    fn compute(&self, ctx: DfContext) -> f64 {
        match self {
            Self::SimpleNoise { node, kind } => {
                crate::counters::bump_density_point_compute(*kind);
                crate::engine::redundancy_probe::visit_point(
                    Arc::as_ptr(node).cast::<()>(),
                    *kind,
                    ctx.x,
                    ctx.y,
                    ctx.z,
                );
                match node.as_ref() {
                    Density::Noise {
                        noise,
                        xz_scale,
                        y_scale,
                    } => noise.get_value(
                        f64::from(ctx.x) * xz_scale,
                        f64::from(ctx.y) * y_scale,
                        f64::from(ctx.z) * xz_scale,
                    ),
                    _ => unreachable!("simple-noise route changed after specialization"),
                }
            }
            Self::Tree(node) => node.compute(ctx),
            Self::Compiled { program, scratch } => {
                program.compute(ctx, &mut scratch.borrow_mut())
            }
        }
    }

    #[inline]
    fn compute_with_xz_products(
        &self,
        ctx: DfContext,
        products: Option<&XzProductLattice>,
    ) -> f64 {
        match (self, products) {
            (Self::Compiled { program, scratch }, Some(products)) => {
                program.compute_with_xz_products(ctx, &mut scratch.borrow_mut(), products)
            }
            _ => self.compute(ctx),
        }
    }
}

/// The preliminary route is normally served by `PreliminarySurfaceCache`.
/// Keep its compiled scratch optional so constructing an aquifer does not
/// reserve a 4096-entry memo before a fallback cache miss needs to evaluate.
enum PreliminaryPointDensity {
    Fallback(PointDensity),
    Compiled {
        program: Arc<PointProgram>,
        scratch: RefCell<Option<PointScratch>>,
        #[cfg(feature = "gen-counters")]
        scratch_allocations: Cell<u64>,
    },
}

impl PreliminaryPointDensity {
    fn from_arc_and_program(node: Arc<Density>, program: Option<Arc<PointProgram>>) -> Self {
        program.map_or_else(
            || Self::Fallback(PointDensity::from_arc(node)),
            |program| Self::Compiled {
                program,
                scratch: RefCell::new(None),
                #[cfg(feature = "gen-counters")]
                scratch_allocations: Cell::new(0),
            },
        )
    }

    #[cfg(test)]
    fn is_compiled(&self) -> bool {
        matches!(self, Self::Compiled { .. })
    }

    #[cfg(test)]
    fn scratch_allocated(&self) -> bool {
        matches!(self, Self::Compiled { scratch, .. } if scratch.borrow().is_some())
    }

    #[cfg(feature = "gen-counters")]
    fn scratch_allocations(&self) -> u64 {
        match self {
            Self::Fallback(_) => 0,
            Self::Compiled {
                scratch_allocations,
                ..
            } => scratch_allocations.get(),
        }
    }

    #[cfg(test)]
    #[inline]
    fn compute(&self, ctx: DfContext) -> f64 {
        match self {
            Self::Fallback(density) => density.compute(ctx),
            Self::Compiled {
                program,
                scratch,
                #[cfg(feature = "gen-counters")]
                scratch_allocations,
            } => {
                let mut scratch = scratch.borrow_mut();
                program.compute(
                    ctx,
                    scratch.get_or_insert_with(|| {
                        #[cfg(feature = "gen-counters")]
                        scratch_allocations.set(scratch_allocations.get() + 1);
                        PointScratch::new()
                    }),
                )
            }
        }
    }

    #[inline]
    fn compute_with_xz_products(
        &self,
        ctx: DfContext,
        products: Option<&XzProductLattice>,
    ) -> f64 {
        match self {
            Self::Fallback(density) => density.compute_with_xz_products(ctx, products),
            Self::Compiled {
                program,
                scratch,
                #[cfg(feature = "gen-counters")]
                scratch_allocations,
            } => {
                let mut scratch = scratch.borrow_mut();
                match products {
                    Some(products) => program.compute_with_xz_products(
                        ctx,
                        scratch.get_or_insert_with(|| {
                            #[cfg(feature = "gen-counters")]
                            scratch_allocations.set(scratch_allocations.get() + 1);
                            PointScratch::new()
                        }),
                        products,
                    ),
                    None => program.compute(
                        ctx,
                        scratch.get_or_insert_with(|| {
                            #[cfg(feature = "gen-counters")]
                            scratch_allocations.set(scratch_allocations.get() + 1);
                            PointScratch::new()
                        }),
                    ),
                }
            }
        }
    }
}

// Grid constant (Aquifer.NoiseBasedAquifer): Y_SPACING.
const Y_SPACING: i32 = 12;
const VERTICAL_CANDIDATE_COUNT: usize = 12;

const SURFACE_SAMPLING_OFFSETS_IN_CHUNKS: [[i32; 2]; 13] = [
    [0, 0],
    [-2, -1],
    [-1, -1],
    [0, -1],
    [1, -1],
    [-3, 0],
    [-2, 0],
    [-1, 0],
    [1, 0],
    [-2, 1],
    [-1, 1],
    [0, 1],
    [1, 1],
];

#[inline]
fn grid_x(block: i32) -> i32 {
    block >> 4
}
#[inline]
fn grid_z(block: i32) -> i32 {
    block >> 4
}
#[inline]
fn grid_y(block: i32) -> i32 {
    block.div_euclid(Y_SPACING)
}
#[inline]
fn from_grid_x(grid: i32, offset: i32) -> i32 {
    (grid << 4) + offset
}
#[inline]
fn from_grid_z(grid: i32, offset: i32) -> i32 {
    (grid << 4) + offset
}
#[inline]
fn from_grid_y(grid: i32, offset: i32) -> i32 {
    grid * Y_SPACING + offset
}
#[inline]
fn section_to_block(section: i32) -> i32 {
    section << 4
}
#[inline]
fn similarity(distance_sqr1: i32, distance_sqr2: i32) -> f64 {
    1.0 - f64::from(distance_sqr2 - distance_sqr1) / 25.0
}

#[inline]
fn quantize(value: f64, resolution: i32) -> i32 {
    floor(value / f64::from(resolution)) * resolution
}

#[derive(Clone, Copy)]
struct VerticalRunScratch {
    anchor_x: i32,
    anchor_y: i32,
    anchor_z: i32,
    current_y: i32,
    indices: [usize; VERTICAL_CANDIDATE_COUNT],
    distances: [i32; VERTICAL_CANDIDATE_COUNT],
    deltas: [i32; VERTICAL_CANDIDATE_COUNT],
}

impl VerticalRunScratch {
    fn new(
        aquifer: &AquiferSystem,
        x: i32,
        y: i32,
        z: i32,
        mut region_cache: Option<&mut AquiferRegionCache>,
    ) -> Self {
        VERTICAL_RUN_SCRATCH_INITIALIZATIONS.with(|count| count.set(count.get() + 1));
        let anchor_x = grid_x(x - 5);
        let anchor_y = grid_y(y + 1);
        let anchor_z = grid_z(z - 5);
        let mut indices = [0; VERTICAL_CANDIDATE_COUNT];
        let mut distances = [0; VERTICAL_CANDIDATE_COUNT];
        let mut deltas = [0; VERTICAL_CANDIDATE_COUNT];
        let mut candidate = 0;
        for x1 in 0..=1 {
            for y1 in -1..=1 {
                for z1 in 0..=1 {
                    let spaced_grid_x = anchor_x + x1;
                    let spaced_grid_y = anchor_y + y1;
                    let spaced_grid_z = anchor_z + z1;
                    let index = aquifer.get_index(spaced_grid_x, spaced_grid_y, spaced_grid_z);
                    let (lx, ly, lz) = match region_cache.as_deref_mut() {
                        Some(cache) => aquifer.location_with_region_cache(
                            spaced_grid_x,
                            spaced_grid_y,
                            spaced_grid_z,
                            index,
                            cache,
                        ),
                        None => aquifer.location(
                            spaced_grid_x,
                            spaced_grid_y,
                            spaced_grid_z,
                            index,
                        ),
                    };
                    let dx = lx - x;
                    let dy = ly - y;
                    let dz = lz - z;
                    distances[candidate] = dx * dx + dy * dy + dz * dz;
                    deltas[candidate] = 1 - 2 * dy;
                    indices[candidate] = index;
                    candidate += 1;
                }
            }
        }
        Self {
            anchor_x,
            anchor_y,
            anchor_z,
            current_y: y,
            indices,
            distances,
            deltas,
        }
    }

    #[inline]
    fn can_advance_to(&self, anchor_x: i32, anchor_y: i32, anchor_z: i32, y: i32) -> bool {
        self.anchor_x == anchor_x
            && self.anchor_y == anchor_y
            && self.anchor_z == anchor_z
            && self.current_y.checked_add(1) == Some(y)
    }

    #[inline]
    fn advance(&mut self) {
        for candidate in 0..VERTICAL_CANDIDATE_COUNT {
            self.distances[candidate] += self.deltas[candidate];
            self.deltas[candidate] += 2;
        }
        self.current_y += 1;
    }

    #[inline]
    fn closest_three(&self) -> (usize, usize, usize, i32, i32, i32) {
        let mut distance_sqr1 = i32::MAX;
        let mut distance_sqr2 = i32::MAX;
        let mut distance_sqr3 = i32::MAX;
        let mut closest_index1 = 0;
        let mut closest_index2 = 0;
        let mut closest_index3 = 0;
        for candidate in 0..VERTICAL_CANDIDATE_COUNT {
            let new_distance = self.distances[candidate];
            if distance_sqr1 >= new_distance {
                closest_index3 = closest_index2;
                closest_index2 = closest_index1;
                closest_index1 = candidate;
                distance_sqr3 = distance_sqr2;
                distance_sqr2 = distance_sqr1;
                distance_sqr1 = new_distance;
            } else if distance_sqr2 >= new_distance {
                closest_index3 = closest_index2;
                closest_index2 = candidate;
                distance_sqr3 = distance_sqr2;
                distance_sqr2 = new_distance;
            } else if distance_sqr3 >= new_distance {
                closest_index3 = candidate;
                distance_sqr3 = new_distance;
            }
        }
        (
            self.indices[closest_index1],
            self.indices[closest_index2],
            self.indices[closest_index3],
            distance_sqr1,
            distance_sqr2,
            distance_sqr3,
        )
    }
}

/// Request-local state for adjacent vertical density slices.
///
/// The production cell path supplies eight blocks at a time. Keeping this
/// small value across adjacent slices lets the next slice advance the same
/// candidate-distance recurrence when its grid anchor is unchanged. It is
/// deliberately not stored on [`AquiferSystem`]: each XZ column owns one
/// state, and concurrent requests must not share mutable fill state.
#[derive(Default)]
pub(crate) struct VerticalRunState {
    scratch: Option<VerticalRunScratch>,
}

thread_local! {
    static VERTICAL_RUN_SCRATCH_INITIALIZATIONS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
fn reset_vertical_run_scratch_initializations() {
    VERTICAL_RUN_SCRATCH_INITIALIZATIONS.with(|count| count.set(0));
}

#[cfg(test)]
fn vertical_run_scratch_initializations() -> u64 {
    VERTICAL_RUN_SCRATCH_INITIALIZATIONS.with(Cell::get)
}

/// Per-thread cache storage retained between adjacent aquifer instances.
///
/// The aquifer is deliberately rebuilt for each chunk because its sampler
/// bounds and cell coordinates belong to that chunk. Its three mutable caches
/// are not part of the returned world, though, so retaining their backing
/// storage avoids repeating large vector/map allocations on every fill.
#[derive(Default)]
struct AquiferScratch {
    aquifer: Vec<Option<FluidStatus>>,
    locations: Vec<Option<(i32, i32, i32)>>,
}

/// Request-local candidate locations and fluid statuses keyed by absolute
/// aquifer-grid coordinates.
#[derive(Debug)]
pub(crate) struct AquiferRegionCache {
    entries: Vec<AquiferRegionEntry>,
    used: usize,
    #[cfg(test)]
    location_lookups: u64,
    #[cfg(test)]
    location_hits: u64,
    #[cfg(test)]
    location_computes: u64,
    #[cfg(test)]
    status_lookups: u64,
    #[cfg(test)]
    status_hits: u64,
    #[cfg(test)]
    status_computes: u64,
}

#[derive(Clone, Copy, Debug)]
struct AquiferRegionEntry {
    grid_x: i32,
    grid_y: i32,
    grid_z: i32,
    location: (i32, i32, i32),
    has_location: bool,
    status: FluidStatus,
    has_status: bool,
    occupied: bool,
}

impl AquiferRegionEntry {
    const EMPTY: Self = Self {
        grid_x: 0,
        grid_y: 0,
        grid_z: 0,
        location: (0, 0, 0),
        has_location: false,
        status: FluidStatus {
            fluid_level: 0,
            fluid_type: Fluid::Air,
        },
        has_status: false,
        occupied: false,
    };
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AquiferRegionCacheStats {
    pub location_lookups: u64,
    pub location_hits: u64,
    pub location_computes: u64,
    pub status_lookups: u64,
    pub status_hits: u64,
    pub status_computes: u64,
    pub retained_entries: usize,
    pub capacity: usize,
}

const AQUIFER_REGION_CACHE_CAPACITY: usize = 8_192;

impl AquiferRegionCache {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::with_capacity(AQUIFER_REGION_CACHE_CAPACITY)
    }

    #[must_use]
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1).next_power_of_two();
        Self {
            entries: vec![AquiferRegionEntry::EMPTY; capacity],
            used: 0,
            #[cfg(test)]
            location_lookups: 0,
            #[cfg(test)]
            location_hits: 0,
            #[cfg(test)]
            location_computes: 0,
            #[cfg(test)]
            status_lookups: 0,
            #[cfg(test)]
            status_hits: 0,
            #[cfg(test)]
            status_computes: 0,
        }
    }

    #[inline]
    fn hash(grid_x: i32, grid_y: i32, grid_z: i32) -> usize {
        let x = (grid_x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let y = (grid_y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        let z = (grid_z as i64 as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
        let mut hash = x ^ y.rotate_left(21) ^ z.rotate_left(42);
        hash ^= hash >> 32;
        hash = (hash ^ (hash >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        hash = (hash ^ (hash >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (hash ^ (hash >> 31)) as usize
    }

    #[inline]
    fn find_slot(&self, grid_x: i32, grid_y: i32, grid_z: i32) -> Option<usize> {
        let mut index = Self::hash(grid_x, grid_y, grid_z) & (self.entries.len() - 1);
        for _ in 0..self.entries.len() {
            let entry = self.entries[index];
            if !entry.occupied {
                return None;
            }
            if entry.grid_x == grid_x && entry.grid_y == grid_y && entry.grid_z == grid_z {
                return Some(index);
            }
            index = (index + 1) & (self.entries.len() - 1);
        }
        None
    }

    #[inline]
    fn insert_slot(&mut self, grid_x: i32, grid_y: i32, grid_z: i32) -> usize {
        assert!(
            self.used < self.entries.len(),
            "aquifer region cache capacity exceeded"
        );
        let mut index = Self::hash(grid_x, grid_y, grid_z) & (self.entries.len() - 1);
        loop {
            if !self.entries[index].occupied {
                self.entries[index] = AquiferRegionEntry {
                    grid_x,
                    grid_y,
                    grid_z,
                    occupied: true,
                    ..AquiferRegionEntry::EMPTY
                };
                self.used += 1;
                return index;
            }
            index = (index + 1) & (self.entries.len() - 1);
        }
    }

    fn get_or_compute_location(
        &mut self,
        grid_x: i32,
        grid_y: i32,
        grid_z: i32,
        positional: AnyPositionalFactory,
    ) -> (i32, i32, i32) {
        #[cfg(test)]
        {
            self.location_lookups += 1;
        }
        if let Some(index) = self.find_slot(grid_x, grid_y, grid_z) {
            if self.entries[index].has_location {
                #[cfg(test)]
                {
                    self.location_hits += 1;
                }
                return self.entries[index].location;
            }
        }
        #[cfg(test)]
        {
            self.location_computes += 1;
        }
        let mut random = positional.at(grid_x, grid_y, grid_z);
        let location = (
            from_grid_x(grid_x, random.next_int_bounded(10)),
            from_grid_y(grid_y, random.next_int_bounded(9)),
            from_grid_z(grid_z, random.next_int_bounded(10)),
        );
        let index = self
            .find_slot(grid_x, grid_y, grid_z)
            .unwrap_or_else(|| self.insert_slot(grid_x, grid_y, grid_z));
        self.entries[index].location = location;
        self.entries[index].has_location = true;
        location
    }

    fn get_or_compute_status(
        &mut self,
        grid_x: i32,
        grid_y: i32,
        grid_z: i32,
        compute: impl FnOnce() -> FluidStatus,
    ) -> FluidStatus {
        #[cfg(test)]
        {
            self.status_lookups += 1;
        }
        if let Some(index) = self.find_slot(grid_x, grid_y, grid_z) {
            if self.entries[index].has_status {
                #[cfg(test)]
                {
                    self.status_hits += 1;
                }
                return self.entries[index].status;
            }
        }
        #[cfg(test)]
        {
            self.status_computes += 1;
        }
        let status = compute();
        let index = self
            .find_slot(grid_x, grid_y, grid_z)
            .unwrap_or_else(|| self.insert_slot(grid_x, grid_y, grid_z));
        self.entries[index].status = status;
        self.entries[index].has_status = true;
        status
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn stats(&self) -> AquiferRegionCacheStats {
        AquiferRegionCacheStats {
            location_lookups: self.location_lookups,
            location_hits: self.location_hits,
            location_computes: self.location_computes,
            status_lookups: self.status_lookups,
            status_hits: self.status_hits,
            status_computes: self.status_computes,
            retained_entries: self.used,
            capacity: self.entries.len(),
        }
    }
}

impl Default for AquiferRegionCache {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug)]
struct PreliminarySurfaceEntry {
    qx: i32,
    qz: i32,
    value: i32,
    occupied: bool,
}

#[derive(Debug)]
struct PreliminarySurfaceState {
    values: Vec<PreliminarySurfaceEntry>,
    scratch: PointScratch,
    pending: Vec<PreliminarySurfacePending>,
    program: Option<Arc<PointProgram>>,
    lookups: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
    scratch_slots: usize,
    replacement: usize,
}

#[derive(Clone, Copy, Debug)]
struct PreliminarySurfacePending {
    qx: i32,
    qz: i32,
}

const PRELIMINARY_LOCAL_CAPACITY: usize = 512;

#[derive(Clone, Copy, Debug)]
struct PreliminaryLocalEntry {
    qx: i32,
    qz: i32,
    value: i32,
    occupied: bool,
}

impl PreliminaryLocalEntry {
    const EMPTY: Self = Self {
        qx: 0,
        qz: 0,
        value: 0,
        occupied: false,
    };
}

#[derive(Debug)]
struct PreliminaryLocalCache {
    values: [PreliminaryLocalEntry; PRELIMINARY_LOCAL_CAPACITY],
}

impl PreliminaryLocalCache {
    fn new() -> Self {
        Self {
            values: [PreliminaryLocalEntry::EMPTY; PRELIMINARY_LOCAL_CAPACITY],
        }
    }

    #[inline]
    fn index(qx: i32, qz: i32) -> usize {
        (PreliminarySurfaceCache::hash(qx, qz) >> 5) as usize
            & (PRELIMINARY_LOCAL_CAPACITY - 1)
    }

    #[inline]
    fn get(&self, qx: i32, qz: i32) -> Option<i32> {
        let entry = self.values[Self::index(qx, qz)];
        (entry.occupied && entry.qx == qx && entry.qz == qz).then_some(entry.value)
    }

    #[inline]
    fn insert(&mut self, qx: i32, qz: i32, value: i32) {
        self.values[Self::index(qx, qz)] = PreliminaryLocalEntry {
            qx,
            qz,
            value,
            occupied: true,
        };
    }
}

impl PreliminarySurfaceEntry {
    const EMPTY: Self = Self {
        qx: 0,
        qz: 0,
        value: 0,
        occupied: false,
    };
}

/// Bounded preliminary-surface values with scalar and cross-shard batch admission.
#[derive(Debug)]
pub(crate) struct PreliminarySurfaceCache {
    shards: Vec<PreliminarySurfaceShard>,
    shard_capacity: usize,
    batch_scratch: Mutex<PointScratch>,
}

#[derive(Debug)]
struct PreliminarySurfaceShard {
    state: Mutex<PreliminarySurfaceState>,
    ready: Condvar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreliminarySurfaceCacheStats {
    pub lookups: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub retained_entries: usize,
    pub capacity: usize,
    pub entry_bytes: usize,
    pub scratch_slots: usize,
    pub retained_bytes: usize,
}

const PRELIMINARY_CACHE_SCALAR_CAPACITY: usize = PRELIMINARY_LOCAL_CAPACITY;
pub(crate) const PRELIMINARY_CACHE_BATCH_CAPACITY: usize = 8_192;
pub(crate) const PRELIMINARY_CACHE_REGION_CAPACITY: usize = 8_192;
const PRELIMINARY_CACHE_SHARDS: usize = 32;
const PRELIMINARY_POINT_SCRATCH_CAPACITY: usize = 128;
const POINT_SCRATCH_ENTRY_BYTES: usize = 24;
const PRELIMINARY_BATCH_WIDTH: usize = 8;

impl PreliminarySurfaceCache {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::with_capacity(PRELIMINARY_CACHE_SCALAR_CAPACITY)
    }

    #[must_use]
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity
            .max(PRELIMINARY_CACHE_SHARDS)
            .next_power_of_two();
        let shard_capacity = capacity / PRELIMINARY_CACHE_SHARDS;
        let scratch_slots = PRELIMINARY_POINT_SCRATCH_CAPACITY;
        Self {
            shards: (0..PRELIMINARY_CACHE_SHARDS)
                .map(|_| PreliminarySurfaceShard {
                    state: Mutex::new(PreliminarySurfaceState {
                        values: vec![PreliminarySurfaceEntry::EMPTY; shard_capacity],
                        scratch: PointScratch::with_capacity(scratch_slots),
                        pending: Vec::with_capacity(shard_capacity),
                        program: None,
                        lookups: 0,
                        hits: 0,
                        misses: 0,
                        evictions: 0,
                        scratch_slots,
                        replacement: 0,
                    }),
                    ready: Condvar::new(),
                })
                .collect(),
            shard_capacity,
            batch_scratch: Mutex::new(PointScratch::with_capacity(scratch_slots)),
        }
    }

    #[must_use]
    pub(crate) fn with_program(program: Arc<PointProgram>, capacity: usize) -> Self {
        let cache = Self::with_capacity(capacity);
        for shard in &cache.shards {
            shard
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .program = Some(Arc::clone(&program));
        }
        cache
    }

    #[inline]
    fn hash(qx: i32, qz: i32) -> u64 {
        let x = (qx as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let z = (qz as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        let mut hash = x ^ z.rotate_left(32);
        hash ^= hash >> 32;
        hash = (hash ^ (hash >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        hash = (hash ^ (hash >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        hash ^ (hash >> 31)
    }

    #[inline]
    fn locate(qx: i32, qz: i32, capacity: usize) -> (usize, usize) {
        let hash = Self::hash(qx, qz);
        (
            (hash as usize) & (PRELIMINARY_CACHE_SHARDS - 1),
            ((hash >> PRELIMINARY_CACHE_SHARDS.trailing_zeros()) as usize) & (capacity - 1),
        )
    }

    #[cfg(test)]
    pub(crate) fn get_or_compute(&self, qx: i32, qz: i32, compute: impl FnOnce() -> i32) -> i32 {
        let shard_capacity = self.shard_capacity;
        let (shard_index, mut index) = Self::locate(qx, qz, shard_capacity);
        let mut state = self.shards[shard_index]
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.lookups += 1;
        for _ in 0..shard_capacity {
            let entry = state.values[index];
            if !entry.occupied {
                state.misses += 1;
                let value = compute();
                state.values[index] = PreliminarySurfaceEntry {
                    qx,
                    qz,
                    value,
                    occupied: true,
                };
                return value;
            }
            if entry.qx == qx && entry.qz == qz {
                state.hits += 1;
                return entry.value;
            }
            index = (index + 1) & (shard_capacity - 1);
        }
        state.misses += 1;
        state.evictions += 1;
        let value = compute();
        let replacement = state.replacement;
        state.replacement = (replacement + 1) & (shard_capacity - 1);
        state.values[replacement] = PreliminarySurfaceEntry {
            qx,
            qz,
            value,
            occupied: true,
        };
        value
    }

    pub(crate) fn get_or_compute_preliminary(
        &self,
        qx: i32,
        qz: i32,
        fallback: impl FnOnce() -> i32,
    ) -> i32 {
        self.get_or_compute_preliminary_with_products(qx, qz, None, fallback)
    }

    pub(crate) fn get_or_compute_preliminary_with_products(
        &self,
        qx: i32,
        qz: i32,
        products: Option<&XzProductLattice>,
        fallback: impl FnOnce() -> i32,
    ) -> i32 {
        let mut fallback = Some(fallback);
        let (shard_index, _) = Self::locate(qx, qz, self.shard_capacity);
        let mut state = self.shards[shard_index]
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.lookups += 1;
        let mut waited = false;
        loop {
            if let Some(index) = Self::find_slot(&state, qx, qz) {
                if !waited {
                    state.hits += 1;
                }
                return state.values[index].value;
            }
            if state
                .pending
                .iter()
                .any(|pending| pending.qx == qx && pending.qz == qz)
            {
                if !waited {
                    state.hits += 1;
                    waited = true;
                }
                state = self.shards[shard_index]
                    .ready
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
                continue;
            }
            state.misses += 1;
            let program = state.program.clone();
            let value = match program {
                Some(program) => {
                    crate::counters::bump_preliminary_surface_compute();
                    let context = DfContext::new(qx, 0, qz);
                    floor(match products {
                        Some(products) => program.compute_with_xz_products(
                            context,
                            &mut state.scratch,
                            products,
                        ),
                        None => program.compute(context, &mut state.scratch),
                    })
                }
                None => fallback.take().expect("preliminary fallback consumed")(),
            };
            Self::store(&mut state, qx, qz, value, self.shard_capacity);
            return value;
        }
    }

    /// Computes up to eight keys with one compiled graph walk when possible.
    pub(crate) fn get_or_compute_preliminary_batch(
        &self,
        keys: &[(i32, i32)],
        output: &mut [i32],
        fallback: impl FnMut(i32, i32) -> i32,
    ) {
        self.get_or_compute_preliminary_batch_with_products(keys, output, None, fallback);
    }

    pub(crate) fn get_or_compute_preliminary_batch_with_products(
        &self,
        keys: &[(i32, i32)],
        output: &mut [i32],
        products: Option<&XzProductLattice>,
        mut fallback: impl FnMut(i32, i32) -> i32,
    ) {
        assert_eq!(keys.len(), output.len());
        assert!(keys.len() <= PRELIMINARY_BATCH_WIDTH);
        if keys.is_empty() {
            return;
        }

        let mut shard_indices = [usize::MAX; PRELIMINARY_BATCH_WIDTH];
        let mut shard_len = 0;
        for &(qx, qz) in keys {
            let shard_index = Self::locate(qx, qz, self.shard_capacity).0;
            if !shard_indices[..shard_len].contains(&shard_index) {
                shard_indices[shard_len] = shard_index;
                shard_len += 1;
            }
        }
        shard_indices[..shard_len].sort_unstable();
        let mut guards = Vec::with_capacity(shard_len);
        for &shard_index in &shard_indices[..shard_len] {
            guards.push((
                shard_index,
                self.shards[shard_index]
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            ));
        }
        let mut missing = [(0, 0); PRELIMINARY_BATCH_WIDTH];
        let mut missing_outputs = [usize::MAX; PRELIMINARY_BATCH_WIDTH];
        let mut missing_len = 0;
        let mut waiting = [usize::MAX; PRELIMINARY_BATCH_WIDTH];
        let mut waiting_len = 0;
        let mut deferred = [usize::MAX; PRELIMINARY_BATCH_WIDTH];
        let mut deferred_len = 0;
        let mut program = None;
        for (key_index, &(qx, qz)) in keys.iter().enumerate() {
            let shard_index = Self::locate(qx, qz, self.shard_capacity).0;
            let guard_index = shard_indices[..shard_len]
                .binary_search(&shard_index)
                .expect("batch shard was not admitted");
            let state = &mut guards[guard_index].1;
            state.lookups += 1;
            if let Some(index) = Self::find_slot(state, qx, qz) {
                state.hits += 1;
                output[key_index] = state.values[index].value;
            } else if state
                .pending
                .iter()
                .any(|pending| pending.qx == qx && pending.qz == qz)
            {
                state.hits += 1;
                waiting[waiting_len] = key_index;
                waiting_len += 1;
            } else {
                if state.pending.len() == state.pending.capacity() {
                    state.lookups -= 1;
                    deferred[deferred_len] = key_index;
                    deferred_len += 1;
                    continue;
                }
                state.misses += 1;
                state.pending.push(PreliminarySurfacePending { qx, qz });
                missing[missing_len] = (qx, qz);
                missing_outputs[missing_len] = key_index;
                missing_len += 1;
                if program.is_none() {
                    program = state.program.clone();
                }
            }
        }
        drop(guards);

        let mut values = [0i32; PRELIMINARY_BATCH_WIDTH];
        if missing_len != 0 {
            if let Some(program) = program {
                let mut contexts = [DfContext::new(0, 0, 0); PRELIMINARY_BATCH_WIDTH];
                for (lane, &(qx, qz)) in missing[..missing_len].iter().enumerate() {
                    contexts[lane] = DfContext::new(qx, 0, qz);
                }
                let mut computed = [0.0; PRELIMINARY_BATCH_WIDTH];
                let mut scratch = self
                    .batch_scratch
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if let Some(products) = products {
                    program.compute_batch_with_xz_products(
                        &contexts[..missing_len],
                        &mut computed[..missing_len],
                        &mut scratch,
                        products,
                    );
                } else {
                    program.compute_batch(
                        &contexts[..missing_len],
                        &mut computed[..missing_len],
                        &mut scratch,
                    );
                }
                for lane in 0..missing_len {
                    crate::counters::bump_preliminary_surface_compute();
                    values[lane] = floor(computed[lane]);
                }
            } else {
                for lane in 0..missing_len {
                    let (qx, qz) = missing[lane];
                    values[lane] = fallback(qx, qz);
                }
            }
        }

        let mut guards = Vec::with_capacity(shard_len);
        for &shard_index in &shard_indices[..shard_len] {
            guards.push((
                shard_index,
                self.shards[shard_index]
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            ));
        }
        for lane in 0..missing_len {
            let (qx, qz) = missing[lane];
            let shard_index = Self::locate(qx, qz, self.shard_capacity).0;
            let guard_index = shard_indices[..shard_len]
                .binary_search(&shard_index)
                .expect("batch shard was not committed");
            let state = &mut guards[guard_index].1;
            let pending = state
                .pending
                .iter()
                .position(|pending| pending.qx == qx && pending.qz == qz)
                .expect("preliminary batch reservation disappeared");
            state.pending.swap_remove(pending);
            Self::store(state, qx, qz, values[lane], self.shard_capacity);
            output[missing_outputs[lane]] = values[lane];
        }
        drop(guards);
        for &shard_index in &shard_indices[..shard_len] {
            self.shards[shard_index].ready.notify_all();
        }
        for &key_index in &waiting[..waiting_len] {
            let (qx, qz) = keys[key_index];
            output[key_index] = self.wait_for_preliminary(qx, qz, products, &mut fallback);
        }
        for &key_index in &deferred[..deferred_len] {
            let (qx, qz) = keys[key_index];
            output[key_index] = self.get_or_compute_preliminary_with_products(
                qx,
                qz,
                products,
                || fallback(qx, qz),
            );
        }
    }

    fn wait_for_preliminary(
        &self,
        qx: i32,
        qz: i32,
        products: Option<&XzProductLattice>,
        fallback: &mut impl FnMut(i32, i32) -> i32,
    ) -> i32 {
        let (shard_index, _) = Self::locate(qx, qz, self.shard_capacity);
        let mut state = self.shards[shard_index]
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            if let Some(index) = Self::find_slot(&state, qx, qz) {
                return state.values[index].value;
            }
            if state
                .pending
                .iter()
                .any(|pending| pending.qx == qx && pending.qz == qz)
            {
                state = self.shards[shard_index]
                    .ready
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            } else {
                drop(state);
                return self.get_or_compute_preliminary_with_products(
                    qx,
                    qz,
                    products,
                    || fallback(qx, qz),
                );
            }
        }
    }

    #[inline]
    fn find_slot(state: &PreliminarySurfaceState, qx: i32, qz: i32) -> Option<usize> {
        let mut index = Self::locate(qx, qz, state.values.len()).1;
        for _ in 0..state.values.len() {
            let entry = state.values[index];
            if !entry.occupied {
                return None;
            }
            if entry.qx == qx && entry.qz == qz {
                return Some(index);
            }
            index = (index + 1) & (state.values.len() - 1);
        }
        None
    }

    fn store(
        state: &mut PreliminarySurfaceState,
        qx: i32,
        qz: i32,
        value: i32,
        shard_capacity: usize,
    ) {
        let mut index = Self::locate(qx, qz, shard_capacity).1;
        for _ in 0..shard_capacity {
            let entry = state.values[index];
            if !entry.occupied || (entry.qx == qx && entry.qz == qz) {
                state.values[index] = PreliminarySurfaceEntry {
                    qx,
                    qz,
                    value,
                    occupied: true,
                };
                return;
            }
            index = (index + 1) & (shard_capacity - 1);
        }
        state.evictions += 1;
        let replacement = state.replacement;
        state.replacement = (replacement + 1) & (shard_capacity - 1);
        state.values[replacement] = PreliminarySurfaceEntry {
            qx,
            qz,
            value,
            occupied: true,
        };
    }

    pub fn stats(&self) -> PreliminarySurfaceCacheStats {
        let mut stats = PreliminarySurfaceCacheStats {
            lookups: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            retained_entries: 0,
            capacity: 0,
            entry_bytes: std::mem::size_of::<PreliminarySurfaceEntry>(),
            scratch_slots: 0,
            retained_bytes: 0,
        };
        for shard in &self.shards {
            let state = shard.state.lock().unwrap_or_else(|error| error.into_inner());
            stats.lookups += state.lookups;
            stats.hits += state.hits;
            stats.misses += state.misses;
            stats.evictions += state.evictions;
            stats.retained_entries += state.values.iter().filter(|entry| entry.occupied).count();
            stats.capacity += state.values.len();
            stats.scratch_slots += state.scratch_slots;
            stats.retained_bytes +=
                state.pending.capacity() * std::mem::size_of::<PreliminarySurfacePending>();
            stats.retained_bytes += state.scratch.batch_buffer_bytes();
        }
        let batch_scratch = self
            .batch_scratch
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        stats.retained_bytes += batch_scratch.batch_buffer_bytes();
        stats.retained_bytes += stats.capacity * stats.entry_bytes
            + (stats.scratch_slots + PRELIMINARY_POINT_SCRATCH_CAPACITY)
                * POINT_SCRATCH_ENTRY_BYTES;
        stats
    }

}

impl Default for PreliminarySurfaceCache {
    fn default() -> Self {
        Self::new()
    }
}


thread_local! {
    static AQUIFER_SCRATCH: RefCell<Vec<AquiferScratch>> =
        const { RefCell::new(Vec::new()) };
}

fn take_aquifer_scratch() -> AquiferScratch {
    AQUIFER_SCRATCH.with(|slot| slot.borrow_mut().pop().unwrap_or_default())
}

fn return_aquifer_scratch(mut scratch: AquiferScratch) {
    scratch.aquifer.clear();
    scratch.locations.clear();
    AQUIFER_SCRATCH.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.len() < 2 {
            slot.push(scratch);
        }
    });
}

/// The overworld density-noise fill with aquifers.
///
/// Construct once per chunk with [`AquiferSystem::new`], then read blocks with
/// [`AquiferSystem::block_at`]. Grid caches make repeated `block_at` calls over a
/// column cheap; the structure is single-chunk (its grid bounds are fixed at
/// construction from the chunk position), matching vanilla's per-chunk
/// `NoiseChunk`.
#[allow(missing_debug_implementations)]
pub struct AquiferSystem {
    final_density: NoiseChunkSampler,
    erosion: NoiseChunkSampler,
    depth: NoiseChunkSampler,
    /// The four point-evaluated router outputs and the preliminary-surface
    /// tree, behind `Arc` so a per-chunk `AquiferSystem` shares them instead of
    /// deep-copying five `Density` trees. Direct `noise` roots use the compact
    /// point path; compound roots use an indexed `PointProgram` walk.
    barrier: PointDensity,
    floodedness: PointDensity,
    spread: PointDensity,
    lava: PointDensity,
    prelim: PreliminaryPointDensity,

    positional: AnyPositionalFactory,
    sea_level: i32,
    /// Vanilla's own default-fluid query — the fluid the *global* picker's sea status
    /// carries.
    ///
    /// This comes from the settings document for every enabled aquifer. The
    /// Overworld uses water, while a custom aquifer-enabled settings document
    /// may select another supported fluid. The `-54` deep-lava status below is
    /// a separate global status and is selected by the height rule rather than
    /// by this field.
    default_fluid: Fluid,
    /// `false` when the settings say `aquifers_enabled: false` — the Nether and
    /// the End. Vanilla swaps the whole implementation
    /// (its own per-chunk field constructor picks a disabled aquifer), so this is a
    /// bypass in [`Self::compute_substance`] rather than a tuning parameter, and
    /// every noise field and grid cache below is left empty in that mode.
    enabled: bool,

    min_grid_x: i32,
    min_grid_y: i32,
    min_grid_z: i32,
    grid_size_x: i32,
    grid_size_z: i32,
    skip_sampling_above_y: i32,

    aquifer_cache: RefCell<Vec<Option<FluidStatus>>>,
    location_cache: RefCell<Vec<Option<(i32, i32, i32)>>>,
    prelim_cache: RefCell<PreliminaryLocalCache>,
    preliminary_shared: Arc<PreliminarySurfaceCache>,
    xz_products: Option<Arc<XzProductLattice>>,
}

impl AquiferSystem {
    /// Number of operations in the immutable preliminary-surface program.
    ///
    /// This is a diagnostic witness that the production aquifer handoff carries
    /// the compiled point route; it does not affect generation decisions.
    #[must_use]
    pub fn preliminary_surface_program_nodes(&self) -> usize {
        match &self.prelim {
            PreliminaryPointDensity::Compiled { program, .. } => program.node_count(),
            PreliminaryPointDensity::Fallback(_) => 0,
        }
    }

    /// Number of fallback `PointScratch` instances allocated by this aquifer's
    /// preliminary route. A shared compiled preliminary cache should leave this
    /// at zero; a direct fallback evaluation increments it once.
    #[cfg(feature = "gen-counters")]
    #[must_use]
    pub fn preliminary_surface_scratch_allocations(&self) -> u64 {
        self.prelim.scratch_allocations()
    }

    /// Builds the aquifer + fill for chunk `(chunk_x, chunk_z)` from a
    /// `noise_settings` JSON value, using `builder` (seeded with the same seed as
    /// `RandomState`) to instantiate the router functions and the aquifer RNG.
    #[must_use]
    pub fn new(settings: &Value, builder: &Builder, chunk_x: i32, chunk_z: i32) -> Self {
        let router = &settings["noise_router"];
        let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(-64) as i32;
        let height = settings["noise"]["height"].as_i64().unwrap_or(384) as i32;
        let sea_level = settings["sea_level"].as_i64().unwrap_or(63) as i32;
        let default_fluid = fluid_from_settings(settings);
        let (cell_width, cell_height) = cell_geometry(settings);

        // Vanilla's own per-chunk field constructor. A dimension with `aquifers_enabled: false`
        // gets a *different implementation*, not a tuned one, and none of the
        // four aquifer noise fields is instantiated for it — so this branch is
        // taken before any of the `builder.build` calls below.
        if !settings["aquifers_enabled"].as_bool().unwrap_or(true) {
            return Self::disabled(
                Program::compile(
                    &builder
                        .build(&router["final_density"])
                        .expect("bundled final_density density-function document"),
                ),
                builder.slot_count(),
                sea_level,
                default_fluid,
                min_y,
                height,
                chunk_x,
                chunk_z,
                cell_width,
                cell_height,
            );
        }

        let final_density_node = Program::compile(
            &builder
                .build(&router["final_density"])
                .expect("bundled final_density density-function document"),
        );
        let erosion_node = Program::compile(
            &builder.build(&router["erosion"]).expect("bundled erosion density-function document"),
        );
        let depth_node = Program::compile(
            &builder.build(&router["depth"]).expect("bundled depth density-function document"),
        );
        let barrier =
            Arc::new(builder.build(&router["barrier"]).expect("bundled barrier density-function document"));
        let floodedness = Arc::new(
            builder
                .build(&router["fluid_level_floodedness"])
                .expect("bundled fluid_level_floodedness density-function document"),
        );
        let spread = Arc::new(
            builder
                .build(&router["fluid_level_spread"])
                .expect("bundled fluid_level_spread density-function document"),
        );
        let lava =
            Arc::new(builder.build(&router["lava"]).expect("bundled lava density-function document"));
        let prelim = Arc::new(
            builder
                .build(&router["preliminary_surface_level"])
                .expect("bundled preliminary_surface_level density-function document"),
        );

        // vanilla's own aquifer-random query = random.fromHashOf("minecraft:aquifer").forkPositional().
        let mut aquifer_src = builder
            .positional_factory()
            .from_hash_of("minecraft:aquifer");
        let positional = aquifer_src.fork_positional();

        let slots = builder.slot_count();

        let mut system = Self::from_parts(
            final_density_node,
            erosion_node,
            depth_node,
            barrier,
            floodedness,
            spread,
            lava,
            prelim,
            positional,
            sea_level,
            min_y,
            height,
            chunk_x,
            chunk_z,
            slots,
            cell_width,
            cell_height,
        );
        // `from_parts` is also used by the cached Overworld route builder,
        // whose historical call contract is water. The settings-backed path
        // must overwrite that compatibility default before any block query.
        system.default_fluid = Fluid::from_block(default_fluid);
        system
    }

    /// Same construction as [`Self::new`], but from already-built density
    /// trees and positional factory instead of a resolver-backed builder.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn from_parts(
        final_density_node: Program,
        erosion_node: Program,
        depth_node: Program,
        barrier: Arc<Density>,
        floodedness: Arc<Density>,
        spread: Arc<Density>,
        lava: Arc<Density>,
        prelim: Arc<Density>,
        positional: AnyPositionalFactory,
        sea_level: i32,
        min_y: i32,
        height: i32,
        chunk_x: i32,
        chunk_z: i32,
        slots: usize,
        cell_width: i32,
        cell_height: i32,
    ) -> Self {
        let point_programs = CompiledAquiferPointRoutes::from_trees(
            &barrier,
            &floodedness,
            &spread,
            &lava,
        );
        let prelim_program = Arc::new(PointProgram::compile(&prelim));
        let preliminary_shared = Arc::new(PreliminarySurfaceCache::with_program(
            Arc::clone(&prelim_program),
            PRELIMINARY_CACHE_SCALAR_CAPACITY,
        ));
        Self::from_parts_internal(
            final_density_node,
            erosion_node,
            depth_node,
            barrier,
            floodedness,
            spread,
            lava,
            prelim,
            Some(prelim_program),
            Some(point_programs),
            positional,
            sea_level,
            min_y,
            height,
            chunk_x,
            chunk_z,
            slots,
            cell_width,
            cell_height,
            preliminary_shared,
            None,
        )
    }

    /// Builds a chunk-bound aquifer using generator-owned compiled point routes.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub(crate) fn from_parts_with_preliminary_cache_and_point_programs(
        final_density_node: Program,
        erosion_node: Program,
        depth_node: Program,
        barrier: Arc<Density>,
        floodedness: Arc<Density>,
        spread: Arc<Density>,
        lava: Arc<Density>,
        prelim: Arc<Density>,
        prelim_program: Arc<PointProgram>,
        point_programs: CompiledAquiferPointRoutes,
        positional: AnyPositionalFactory,
        sea_level: i32,
        min_y: i32,
        height: i32,
        chunk_x: i32,
        chunk_z: i32,
        slots: usize,
        cell_width: i32,
        cell_height: i32,
        preliminary_shared: Arc<PreliminarySurfaceCache>,
        xz_products: Option<Arc<XzProductLattice>>,
    ) -> Self {
        Self::from_parts_internal(
            final_density_node,
            erosion_node,
            depth_node,
            barrier,
            floodedness,
            spread,
            lava,
            prelim,
            Some(prelim_program),
            Some(point_programs),
            positional,
            sea_level,
            min_y,
            height,
            chunk_x,
            chunk_z,
            slots,
            cell_width,
            cell_height,
            preliminary_shared,
            xz_products,
        )
    }

    /// Same construction as [`Self::new`], but from already-built density
    /// trees and positional factory instead of a `Resolver`-backed
    /// [`Builder`]. Exists so a caller that must keep the trees around across
    /// many chunks (e.g. [`crate::overworld::OverworldGenerator`], which is
    /// built once per world seed and cannot hold a borrowed `Builder`/
    /// `Resolver` for its own lifetime) can build the eight router outputs
    /// once and construct a fresh per-chunk [`AquiferSystem`] — matching
    /// vanilla's own per-chunk `NoiseChunk` — by cloning the trees rather than
    /// re-resolving JSON every chunk.
    ///
    /// Since U4 those clones are **refcount bumps**: the three interpolated
    /// routes arrive as a [`Program`] (`Arc<Graph>` plus a root index) and the
    /// five point-evaluated ones as `Arc<Density>`. Before that they were eight
    /// recursive deep copies of a `Box`-linked tree whose every node was 232
    /// bytes wide, performed once per chunk — diagnostic D3.
    /// `cell_width` and `cell_height` are the settings-derived interpolation
    /// geometry and must be shared by all three field samplers.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    fn from_parts_internal(
        final_density_node: Program,
        erosion_node: Program,
        depth_node: Program,
        barrier: Arc<Density>,
        floodedness: Arc<Density>,
        spread: Arc<Density>,
        lava: Arc<Density>,
        prelim: Arc<Density>,
        prelim_program: Option<Arc<PointProgram>>,
        point_programs: Option<CompiledAquiferPointRoutes>,
        positional: AnyPositionalFactory,
        sea_level: i32,
        min_y: i32,
        height: i32,
        chunk_x: i32,
        chunk_z: i32,
        slots: usize,
        cell_width: i32,
        cell_height: i32,
        preliminary_shared: Arc<PreliminarySurfaceCache>,
        xz_products: Option<Arc<XzProductLattice>>,
    ) -> Self {
        // Grid bounds, verbatim from NoiseBasedAquifer's constructor.
        let min_block_x = chunk_x * 16;
        let max_block_x = min_block_x + 15;
        let min_block_z = chunk_z * 16;
        let max_block_z = min_block_z + 15;

        // `final_density` is only ever queried by `Self::block_at`, which
        // every caller in this crate (this module's own doc-tested contract,
        // `aquifer_parity`'s whole-chunk sweep, and
        // `crate::overworld::OverworldGenerator`'s fill/carve stages) calls
        // exclusively at exact `(min_block_x..=max_block_x, min_y..min_y+height-1,
        // min_block_z..=max_block_z)` positions — the same "known, small query
        // region" contract `NoiseChunkSampler::new_bounded` documents for the
        // shape stage's own `DenseShape`. Swapping this one sampler to the
        // dense/bounded cache avoids a `HashMap`-backed `CornerCache` on the
        // single hottest per-block call in the composed pipeline (found by an
        // architecture review). `erosion`/`depth` stay on the hashed
        // `new` — they're queried from `is_deep_dark_region` at aquifer-grid
        // locations that legitimately range outside this chunk's own bounds
        // (the padded grid-cell search `Self::compute_aquifer_fluid` walks),
        // so bounding them would violate `new_bounded`'s contract.
        let final_density = NoiseChunkSampler::from_program_with_xz_products(
            final_density_node,
            slots,
            cell_width,
            cell_height,
            Some(Bounds {
                x: (min_block_x, max_block_x),
                y: (min_y, min_y + height - 1),
                z: (min_block_z, max_block_z),
            }),
            xz_products.clone(),
        );
        let erosion =
            NoiseChunkSampler::from_program(erosion_node, slots, cell_width, cell_height, None);
        let depth =
            NoiseChunkSampler::from_program(depth_node, slots, cell_width, cell_height, None);

        let min_grid_x = grid_x(min_block_x + -5);
        let max_grid_x = grid_x(max_block_x + -5) + 1;
        let grid_size_x = max_grid_x - min_grid_x + 1;
        let min_grid_y = grid_y(min_y + 1) + -1;
        let max_grid_y = grid_y(min_y + height + 1) + 1;
        let grid_size_y = max_grid_y - min_grid_y + 1;
        let min_grid_z = grid_z(min_block_z + -5);
        let max_grid_z = grid_z(max_block_z + -5) + 1;
        let grid_size_z = max_grid_z - min_grid_z + 1;
        let total = (grid_size_x * grid_size_y * grid_size_z) as usize;

        let mut scratch = take_aquifer_scratch();
        scratch.aquifer.resize(total, None);
        scratch.locations.resize(total, None);

        let point_programs = point_programs.as_ref();
        let mut system = Self {
            final_density,
            erosion,
            depth,
            barrier: PointDensity::from_arc_and_program(
                barrier,
                point_programs.and_then(|programs| programs.barrier.clone()),
            ),
            floodedness: PointDensity::from_arc_and_program(
                floodedness,
                point_programs.and_then(|programs| programs.floodedness.clone()),
            ),
            spread: PointDensity::from_arc_and_program(
                spread,
                point_programs.and_then(|programs| programs.spread.clone()),
            ),
            lava: PointDensity::from_arc_and_program(
                lava,
                point_programs.and_then(|programs| programs.lava.clone()),
            ),
            prelim: PreliminaryPointDensity::from_arc_and_program(prelim, prelim_program),
            positional,
            sea_level,
            default_fluid: Fluid::Water,
            enabled: true,
            min_grid_x,
            min_grid_y,
            min_grid_z,
            grid_size_x,
            grid_size_z,
            skip_sampling_above_y: 0,
            aquifer_cache: RefCell::new(scratch.aquifer),
            location_cache: RefCell::new(scratch.locations),
            prelim_cache: RefCell::new(PreliminaryLocalCache::new()),
            preliminary_shared,
            xz_products,
        };

        let max_prelim = system.max_preliminary_surface_level(
            from_grid_x(min_grid_x, 0),
            from_grid_z(min_grid_z, 0),
            from_grid_x(max_grid_x, 9),
            from_grid_z(max_grid_z, 9),
        );
        let max_adjusted = system.adjust_surface_level(max_prelim);
        let skip_grid_y = grid_y(max_adjusted + 12) - -1;
        system.skip_sampling_above_y = from_grid_y(skip_grid_y, 11) - 1;

        system
    }

    /// Vanilla's own disabled-aquifer constructor — the whole aquifer for a
    /// dimension whose settings say `aquifers_enabled: false`, which is the
    /// Nether and the End.
    ///
    /// ```text
    /// density > 0.0 ? none : fluid_rule.compute_fluid(x, y, z).at(y)
    /// ```
    ///
    /// So: solid where the interpolated density is positive, otherwise the
    /// *global* picker's answer — the dimension's `default_fluid` below
    /// `sea_level`, air above. Nothing positional, nothing cached, no barrier
    /// pressure pushed back into the density. That is why every noise field and
    /// grid cache below is a stub: reading one in this mode would be a bug, and
    /// `enabled: false` makes [`Self::compute_substance`] return before it can.
    ///
    /// **The Nether's lava sea comes out of here, not out of aquifer logic.**
    /// With `sea_level 32` and `default_fluid` lava, every position below y=32
    /// whose density is `<= 0` is lava and everything above it is air. Modelling
    /// it as "an aquifer whose second fluid is lava" would be a different, wrong
    /// mechanism — the `-54` deep-lava status is an Overworld feature and is
    /// unreachable against `min_y 0`.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn disabled(
        final_density_node: Program,
        slots: usize,
        sea_level: i32,
        default_fluid: BlockKind,
        min_y: i32,
        height: i32,
        chunk_x: i32,
        chunk_z: i32,
        cell_width: i32,
        cell_height: i32,
    ) -> Self {
        let min_block_x = chunk_x * 16;
        let min_block_z = chunk_z * 16;
        Self::disabled_bounded(
            final_density_node,
            slots,
            sea_level,
            default_fluid,
            min_y,
            height,
            (min_block_x, min_block_x + 15),
            (min_block_z, min_block_z + 15),
            cell_width,
            cell_height,
        )
    }

    /// Disabled aquifer over an explicit horizontal block rectangle. This is
    /// the batch counterpart of [`Self::disabled`]: it uses one density scratch
    /// for adjacent chunks so interpolation corners on their boundaries are
    /// evaluated once. Point evaluation and floating-point operation order are
    /// unchanged.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn disabled_bounded(
        final_density_node: Program,
        slots: usize,
        sea_level: i32,
        default_fluid: BlockKind,
        min_y: i32,
        height: i32,
        x_bounds: (i32, i32),
        z_bounds: (i32, i32),
        cell_width: i32,
        cell_height: i32,
    ) -> Self {
        let final_density = NoiseChunkSampler::from_program(
            final_density_node,
            slots,
            cell_width,
            cell_height,
            Some(Bounds {
                x: x_bounds,
                y: (min_y, min_y + height - 1),
                z: z_bounds,
            }),
        );
        let stub = || Arc::new(Density::Const(0.0));
        let stub_sampler =
            || NoiseChunkSampler::new(Density::Const(0.0), 0, cell_width, cell_height);
        Self {
            final_density,
            erosion: stub_sampler(),
            depth: stub_sampler(),
            barrier: PointDensity::from_arc(stub()),
            floodedness: PointDensity::from_arc(stub()),
            spread: PointDensity::from_arc(stub()),
            lava: PointDensity::from_arc(stub()),
            prelim: PreliminaryPointDensity::Fallback(PointDensity::from_arc(stub())),
            // Vanilla's own disabled-aquifer constructor takes no
            // `PositionalRandomFactory` at all; this
            // is the cheapest inert stand-in and is never sampled.
            positional: crate::rng::Algorithm::Legacy.root_positional(0),
            sea_level,
            default_fluid: Fluid::from_block(default_fluid),
            enabled: false,
            min_grid_x: 0,
            min_grid_y: 0,
            min_grid_z: 0,
            grid_size_x: 0,
            grid_size_z: 0,
            skip_sampling_above_y: i32::MAX,
            aquifer_cache: RefCell::new(Vec::new()),
            location_cache: RefCell::new(Vec::new()),
            prelim_cache: RefCell::new(PreliminaryLocalCache::new()),
            preliminary_shared: Arc::new(PreliminarySurfaceCache::new()),
            xz_products: None,
        }
    }

    /// The pre-surface block at world coordinates — vanilla's own fill
    /// step's decision:
    /// its own compute-substance call over `final_density(x,y,z)`, mapped to a
    /// [`BlockKind`] (`None` → the default block, stone).
    ///
    /// **No beard.** This is the spelling every caller outside the fill loop
    /// wants, and it is vanilla's too: vanilla's own chunk generator passes
    /// a constant `0.0` beardifier marker at both of its non-fill call
    /// sites (its own base-column and base-height queries), which is exactly
    /// why a structure's *own* height probe
    /// does not see the terrain its own beard is about to create.
    #[must_use]
    pub fn block_at(&self, x: i32, y: i32, z: i32) -> BlockKind {
        self.block_at_beard(x, y, z, 0.0)
    }

    /// Evaluates a vertical final-density run using the aquifer's chunk-bound
    /// sampler. The caller may then pass each value to [`Self::block_at_density`]
    /// while retaining this aquifer's independent fluid caches.
    pub fn final_density_column(
        &self,
        x: i32,
        z: i32,
        y_start: i32,
        output: &mut [f64],
    ) {
        self.final_density
            .final_density_column(x, z, y_start, output);
    }

    /// Starts request-local state for adjacent vertical density slices.
    pub(crate) fn vertical_run_state(&self) -> VerticalRunState {
        VerticalRunState::default()
    }

    /// Resolves a consecutive vertical density run without repeating the
    /// candidate-location and squared-distance work for every block.
    pub fn block_at_density_vertical_run(
        &self,
        x: i32,
        z: i32,
        y_start: i32,
        densities: &[f64],
        output: &mut [BlockKind],
    ) {
        let mut state = VerticalRunState::default();
        self.block_at_density_vertical_slice_impl(
            x,
            z,
            y_start,
            densities,
            output,
            &mut state,
            None,
        );
    }

    /// Resolves one consecutive density slice using caller-owned request state.
    ///
    /// Adjacent slices can pass the same [`VerticalRunState`] to preserve the
    /// candidate-distance recurrence across an eight-block cell boundary. A
    /// positive-density block, disabled aquifer, upper shortcut, lava shortcut,
    /// or changed grid anchor clears the state at the exact boundary where the
    /// scalar path would no longer be equivalent.
    pub(crate) fn block_at_density_vertical_slice(
        &self,
        x: i32,
        z: i32,
        y_start: i32,
        densities: &[f64],
        output: &mut [BlockKind],
        state: &mut VerticalRunState,
    ) {
        self.block_at_density_vertical_slice_impl(
            x,
            z,
            y_start,
            densities,
            output,
            state,
            None,
        );
    }

    /// Region-only counterpart of [`Self::block_at_density_vertical_slice`].
    pub(crate) fn block_at_density_vertical_slice_with_region_cache(
        &self,
        x: i32,
        z: i32,
        y_start: i32,
        densities: &[f64],
        output: &mut [BlockKind],
        state: &mut VerticalRunState,
        region_cache: &mut AquiferRegionCache,
    ) {
        self.block_at_density_vertical_slice_impl(
            x,
            z,
            y_start,
            densities,
            output,
            state,
            Some(region_cache),
        );
    }

    fn block_at_density_vertical_slice_impl(
        &self,
        x: i32,
        z: i32,
        y_start: i32,
        densities: &[f64],
        output: &mut [BlockKind],
        state: &mut VerticalRunState,
        mut region_cache: Option<&mut AquiferRegionCache>,
    ) {
        assert_eq!(densities.len(), output.len());
        for (offset, (&density, block)) in densities.iter().zip(output.iter_mut()).enumerate() {
            let y = y_start + offset as i32;
            if density > 0.0 {
                *block = self.block_at_density(x, y, z, density);
                state.scratch = None;
                continue;
            }
            let global_fluid = self.global_fluid(y);
            if !self.enabled
                || y > self.skip_sampling_above_y
                || global_fluid.at(y) == Fluid::Lava
            {
                *block = match region_cache.as_deref_mut() {
                    Some(cache) => self.block_at_density_with_global_region_cache(
                        x,
                        y,
                        z,
                        density,
                        global_fluid,
                        cache,
                    ),
                    None => self.block_at_density_with_global(x, y, z, density, global_fluid),
                };
                state.scratch = None;
                continue;
            }

            let anchor_x = grid_x(x - 5);
            let anchor_y = grid_y(y + 1);
            let anchor_z = grid_z(z - 5);
            let run = match state.scratch.as_mut() {
                Some(run) if run.can_advance_to(anchor_x, anchor_y, anchor_z, y) => {
                    run.advance();
                    run
                }
                Some(_) => {
                    *block = match region_cache.as_deref_mut() {
                        Some(cache) => self.block_at_density_with_global_region_cache(
                            x,
                            y,
                            z,
                            density,
                            global_fluid,
                            cache,
                        ),
                        None => self.block_at_density_with_global(x, y, z, density, global_fluid),
                    };
                    state.scratch = None;
                    continue;
                }
                None => state.scratch.insert(VerticalRunScratch::new(
                    self,
                    x,
                    y,
                    z,
                    region_cache.as_deref_mut(),
                )),
            };
            let (closest1, closest2, closest3, distance1, distance2, distance3) =
                run.closest_three();
            crate::counters::bump_block_at();
            let fluid = self.compute_substance_with_closest_optional(
                x, y, z, density, closest1, closest2, closest3, distance1, distance2, distance3,
                region_cache.as_deref_mut(),
            );
            *block = match fluid {
                None => BlockKind::Stone,
                Some(fluid) => fluid.to_block(),
            };
        }
    }

    /// [`block_at`](Self::block_at) with a beardifier term added to the density —
    /// vanilla's own structure-adaptation density addition.
    ///
    /// The `+ beard` is the whole of structure placement's S3 at this layer, and the
    /// **operand order is the specification**: vanilla's own binary-add node
    /// evaluates
    /// `argument1.compute(ctx) + argument2.compute(ctx)`, so the interpolated
    /// density comes first. See
    /// [`crate::structure::beardifier`] for why the term is added here rather
    /// than inside the density graph.
    #[must_use]
    pub fn block_at_beard(&self, x: i32, y: i32, z: i32, beard: f64) -> BlockKind {
        let density = self.final_density.final_density(x, y, z) + beard;
        self.block_at_density(x, y, z, density)
    }

    /// Resolves a block from a final-density value that was already sampled
    /// for the same position. This keeps the aquifer status and location
    /// caches on their normal per-target path without re-entering the density
    /// field evaluator.
    #[must_use]
    pub fn block_at_density(&self, x: i32, y: i32, z: i32, density: f64) -> BlockKind {
        crate::counters::bump_block_at();
        let block = match self.compute_substance(x, y, z, density) {
            None => BlockKind::Stone,
            Some(fluid) => fluid.to_block(),
        };
        block
    }

    /// Resolves a non-positive density when the caller already evaluated the
    /// global fluid picker for this Y. The vertical fill path needs that value
    /// to select its fast branches, so threading it through avoids evaluating
    /// the same tiny picker a second time without changing any aquifer draws.
    #[inline]
    fn block_at_density_with_global(
        &self,
        x: i32,
        y: i32,
        z: i32,
        density: f64,
        global_fluid: FluidStatus,
    ) -> BlockKind {
        crate::counters::bump_block_at();
        match self.compute_substance_with_global(x, y, z, density, global_fluid) {
            None => BlockKind::Stone,
            Some(fluid) => fluid.to_block(),
        }
    }

    #[inline]
    fn block_at_density_with_global_region_cache(
        &self,
        x: i32,
        y: i32,
        z: i32,
        density: f64,
        global_fluid: FluidStatus,
        region_cache: &mut AquiferRegionCache,
    ) -> BlockKind {
        crate::counters::bump_block_at();
        match self.compute_substance_with_global_optional(
            x,
            y,
            z,
            density,
            global_fluid,
            Some(region_cache),
        ) {
            None => BlockKind::Stone,
            Some(fluid) => fluid.to_block(),
        }
    }

    /// Vanilla's own world-carver carve-state lookup's aquifer branch:
    /// its aquifer's combined substance computation at this single point, threshold 0.0. `None` means
    /// "do not carve — keep the existing block" (only reachable if the local
    /// density were positive, which the carver never passes); `Some` is the
    /// carve substance (air below the surface, or the local water/lava table).
    #[must_use]
    pub fn carve_substance(&self, x: i32, y: i32, z: i32) -> Option<BlockKind> {
        self.compute_substance(x, y, z, 0.0).map(Fluid::to_block)
    }

    fn adjust_surface_level(&self, preliminary_surface_level: i32) -> i32 {
        preliminary_surface_level + 8
    }

    fn preliminary_surface_level(&self, sample_x: i32, sample_z: i32) -> i32 {
        let qx = (sample_x >> 2) << 2;
        let qz = (sample_z >> 2) << 2;
        crate::counters::bump_preliminary_surface_request(qx, qz);
        if let Some(v) = self.prelim_cache.borrow().get(qx, qz) {
            return v;
        }
        let prelim = &self.prelim;
        let products = self.xz_products.as_deref();
        let v = self
            .preliminary_shared
            .get_or_compute_preliminary_with_products(qx, qz, products, || {
            crate::counters::bump_preliminary_surface_compute();
            floor(prelim.compute_with_xz_products(DfContext::new(qx, 0, qz), products))
        });
        self.prelim_cache.borrow_mut().insert(qx, qz, v);
        v
    }

    fn max_preliminary_surface_level(
        &self,
        min_block_x: i32,
        min_block_z: i32,
        max_block_x: i32,
        max_block_z: i32,
    ) -> i32 {
        let mut max_y = i32::MIN;
        let mut local_cache = self.prelim_cache.borrow_mut();
        let mut keys = [(0, 0); PRELIMINARY_BATCH_WIDTH];
        let mut values = [0i32; PRELIMINARY_BATCH_WIDTH];
        let mut batch_len = 0;
        let mut block_z = min_block_z;
        while block_z <= max_block_z {
            let mut block_x = min_block_x;
            while block_x <= max_block_x {
                crate::counters::bump_preliminary_surface_request(block_x, block_z);
                keys[batch_len] = (block_x, block_z);
                batch_len += 1;
                if batch_len == PRELIMINARY_BATCH_WIDTH {
                    let prelim = &self.prelim;
                    self.preliminary_shared
                        .get_or_compute_preliminary_batch_with_products(
                        &keys,
                        &mut values,
                        self.xz_products.as_deref(),
                        |qx, qz| {
                            crate::counters::bump_preliminary_surface_compute();
                            floor(prelim.compute_with_xz_products(
                                DfContext::new(qx, 0, qz),
                                self.xz_products.as_deref(),
                            ))
                        },
                    );
                    for (key, &surface_level) in keys.iter().zip(values.iter()) {
                        local_cache.insert(key.0, key.1, surface_level);
                        max_y = max_y.max(surface_level);
                    }
                    batch_len = 0;
                }
                block_x += 4;
            }
            block_z += 4;
        }
        if batch_len != 0 {
            let prelim = &self.prelim;
            self.preliminary_shared
                .get_or_compute_preliminary_batch_with_products(
                &keys[..batch_len],
                &mut values[..batch_len],
                self.xz_products.as_deref(),
                |qx, qz| {
                    crate::counters::bump_preliminary_surface_compute();
                    floor(prelim.compute_with_xz_products(
                        DfContext::new(qx, 0, qz),
                        self.xz_products.as_deref(),
                    ))
                },
            );
            for (key, &surface_level) in keys[..batch_len].iter().zip(values[..batch_len].iter()) {
                local_cache.insert(key.0, key.1, surface_level);
                max_y = max_y.max(surface_level);
            }
        }
        max_y
    }

    fn global_fluid(&self, y: i32) -> FluidStatus {
        // createFluidPicker: lava below min(-54, seaLevel), else the sea fluid,
        // whose type is the dimension's own `default_fluid`.
        if y < (-54).min(self.sea_level) {
            FluidStatus {
                fluid_level: -54,
                fluid_type: Fluid::Lava,
            }
        } else {
            FluidStatus {
                fluid_level: self.sea_level,
                fluid_type: self.default_fluid,
            }
        }
    }

    fn get_index(&self, grid_x: i32, grid_y: i32, grid_z: i32) -> usize {
        let x = grid_x - self.min_grid_x;
        let y = grid_y - self.min_grid_y;
        let z = grid_z - self.min_grid_z;
        ((y * self.grid_size_z + z) * self.grid_size_x + x) as usize
    }

    fn location(&self, grid_x: i32, grid_y: i32, grid_z: i32, index: usize) -> (i32, i32, i32) {
        if let Some(loc) = self.location_cache.borrow()[index] {
            return loc;
        }
        let mut random = self.positional.at(grid_x, grid_y, grid_z);
        let loc = (
            from_grid_x(grid_x, random.next_int_bounded(10)),
            from_grid_y(grid_y, random.next_int_bounded(9)),
            from_grid_z(grid_z, random.next_int_bounded(10)),
        );
        self.location_cache.borrow_mut()[index] = Some(loc);
        loc
    }

    fn location_with_region_cache(
        &self,
        grid_x: i32,
        grid_y: i32,
        grid_z: i32,
        index: usize,
        region_cache: &mut AquiferRegionCache,
    ) -> (i32, i32, i32) {
        if let Some(loc) = self.location_cache.borrow()[index] {
            return loc;
        }
        let loc = region_cache.get_or_compute_location(
            grid_x,
            grid_y,
            grid_z,
            self.positional,
        );
        self.location_cache.borrow_mut()[index] = Some(loc);
        loc
    }

    #[inline]
    fn grid_coordinate(&self, index: usize) -> (i32, i32, i32) {
        let grid_size_x = self.grid_size_x as usize;
        let grid_size_z = self.grid_size_z as usize;
        let grid_x = index % grid_size_x;
        let index = index / grid_size_x;
        let grid_z = index % grid_size_z;
        let grid_y = index / grid_size_z;
        (
            self.min_grid_x + grid_x as i32,
            self.min_grid_y + grid_y as i32,
            self.min_grid_z + grid_z as i32,
        )
    }

    fn aquifer_status(&self, index: usize) -> FluidStatus {
        if let Some(status) = self.aquifer_cache.borrow()[index] {
            return status;
        }
        let (x, y, z) = self.location_cache.borrow()[index].expect("location computed first");
        let status = self.compute_aquifer_fluid(x, y, z);
        self.aquifer_cache.borrow_mut()[index] = Some(status);
        status
    }

    fn aquifer_status_with_region_cache(
        &self,
        index: usize,
        region_cache: &mut AquiferRegionCache,
    ) -> FluidStatus {
        if let Some(status) = self.aquifer_cache.borrow()[index] {
            return status;
        }
        let (x, y, z) = self.location_cache.borrow()[index].expect("location computed first");
        let (grid_x, grid_y, grid_z) = self.grid_coordinate(index);
        let status = region_cache.get_or_compute_status(grid_x, grid_y, grid_z, || {
            self.compute_aquifer_fluid(x, y, z)
        });
        self.aquifer_cache.borrow_mut()[index] = Some(status);
        status
    }

    #[allow(clippy::too_many_lines)]
    fn compute_substance(&self, pos_x: i32, pos_y: i32, pos_z: i32, density: f64) -> Option<Fluid> {
        if density > 0.0 {
            return None;
        }

        self.compute_substance_with_global(
            pos_x,
            pos_y,
            pos_z,
            density,
            self.global_fluid(pos_y),
        )
    }

    #[allow(clippy::too_many_lines)]
    fn compute_substance_with_global(
        &self,
        pos_x: i32,
        pos_y: i32,
        pos_z: i32,
        density: f64,
        global_fluid: FluidStatus,
    ) -> Option<Fluid> {
        self.compute_substance_with_global_optional(
            pos_x,
            pos_y,
            pos_z,
            density,
            global_fluid,
            None,
        )
    }

    #[allow(clippy::too_many_lines)]
    fn compute_substance_with_global_optional(
        &self,
        pos_x: i32,
        pos_y: i32,
        pos_z: i32,
        density: f64,
        global_fluid: FluidStatus,
        mut region_cache: Option<&mut AquiferRegionCache>,
    ) -> Option<Fluid> {
        if !self.enabled {
            // Vanilla's own disabled-aquifer constructor's entire body. Deliberately before the
            // `skip_sampling_above_y` shortcut rather than folded into it: that
            // shortcut is an optimisation inside the *noise* aquifer with its own
            // derivation, and reusing it here would make the disabled path's
            // correctness depend on it.
            return Some(global_fluid.at(pos_y));
        }
        if pos_y > self.skip_sampling_above_y {
            return Some(global_fluid.at(pos_y));
        }
        if global_fluid.at(pos_y) == Fluid::Lava {
            return Some(Fluid::Lava);
        }

        let x_anchor = grid_x(pos_x + -5);
        let y_anchor = grid_y(pos_y + 1);
        let z_anchor = grid_z(pos_z + -5);
        let mut distance_sqr1 = i32::MAX;
        let mut distance_sqr2 = i32::MAX;
        let mut distance_sqr3 = i32::MAX;
        let mut closest_index1 = 0usize;
        let mut closest_index2 = 0usize;
        let mut closest_index3 = 0usize;

        for x1 in 0..=1 {
            for y1 in -1..=1 {
                for z1 in 0..=1 {
                    let spaced_grid_x = x_anchor + x1;
                    let spaced_grid_y = y_anchor + y1;
                    let spaced_grid_z = z_anchor + z1;
                    let index = self.get_index(spaced_grid_x, spaced_grid_y, spaced_grid_z);
                    let (lx, ly, lz) = match region_cache.as_deref_mut() {
                        Some(cache) => self.location_with_region_cache(
                            spaced_grid_x,
                            spaced_grid_y,
                            spaced_grid_z,
                            index,
                            cache,
                        ),
                        None => self.location(spaced_grid_x, spaced_grid_y, spaced_grid_z, index),
                    };
                    let dx = lx - pos_x;
                    let dy = ly - pos_y;
                    let dz = lz - pos_z;
                    let new_distance = dx * dx + dy * dy + dz * dz;
                    if distance_sqr1 >= new_distance {
                        closest_index3 = closest_index2;
                        closest_index2 = closest_index1;
                        closest_index1 = index;
                        distance_sqr3 = distance_sqr2;
                        distance_sqr2 = distance_sqr1;
                        distance_sqr1 = new_distance;
                    } else if distance_sqr2 >= new_distance {
                        closest_index3 = closest_index2;
                        closest_index2 = index;
                        distance_sqr3 = distance_sqr2;
                        distance_sqr2 = new_distance;
                    } else if distance_sqr3 >= new_distance {
                        closest_index3 = index;
                        distance_sqr3 = new_distance;
                    }
                }
            }
        }

        self.compute_substance_with_closest_optional(
            pos_x,
            pos_y,
            pos_z,
            density,
            closest_index1,
            closest_index2,
            closest_index3,
            distance_sqr1,
            distance_sqr2,
            distance_sqr3,
            region_cache,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_substance_with_closest_optional(
        &self,
        pos_x: i32,
        pos_y: i32,
        pos_z: i32,
        density: f64,
        closest_index1: usize,
        closest_index2: usize,
        closest_index3: usize,
        distance_sqr1: i32,
        distance_sqr2: i32,
        distance_sqr3: i32,
        mut region_cache: Option<&mut AquiferRegionCache>,
    ) -> Option<Fluid> {
        let closest_status1 = match region_cache.as_deref_mut() {
            Some(cache) => self.aquifer_status_with_region_cache(closest_index1, cache),
            None => self.aquifer_status(closest_index1),
        };
        let similarity12 = similarity(distance_sqr1, distance_sqr2);
        let fluid_state = closest_status1.at(pos_y);
        if similarity12 <= 0.0 {
            return Some(fluid_state);
        }

        if fluid_state == Fluid::Water && self.global_fluid(pos_y - 1).at(pos_y - 1) == Fluid::Lava
        {
            return Some(fluid_state);
        }

        let mut barrier_noise_value = f64::NAN;
        let closest_status2 = match region_cache.as_deref_mut() {
            Some(cache) => self.aquifer_status_with_region_cache(closest_index2, cache),
            None => self.aquifer_status(closest_index2),
        };
        let barrier12 = similarity12
            * self.calculate_pressure(
                pos_x,
                pos_y,
                pos_z,
                &mut barrier_noise_value,
                closest_status1,
                closest_status2,
            );
        if density + barrier12 > 0.0 {
            return None;
        }

        let closest_status3 = match region_cache.as_deref_mut() {
            Some(cache) => self.aquifer_status_with_region_cache(closest_index3, cache),
            None => self.aquifer_status(closest_index3),
        };
        let similarity13 = similarity(distance_sqr1, distance_sqr3);
        if similarity13 > 0.0 {
            let barrier13 = similarity12
                * similarity13
                * self.calculate_pressure(
                    pos_x,
                    pos_y,
                    pos_z,
                    &mut barrier_noise_value,
                    closest_status1,
                    closest_status3,
                );
            if density + barrier13 > 0.0 {
                return None;
            }
        }

        let similarity23 = similarity(distance_sqr2, distance_sqr3);
        if similarity23 > 0.0 {
            let barrier23 = similarity12
                * similarity23
                * self.calculate_pressure(
                    pos_x,
                    pos_y,
                    pos_z,
                    &mut barrier_noise_value,
                    closest_status2,
                    closest_status3,
                );
            if density + barrier23 > 0.0 {
                return None;
            }
        }

        // Vanilla's own "should schedule fluid update" is a fluid-tick side effect and does not
        // change the block placed, so the flow branches below it are omitted.
        Some(fluid_state)
    }

    fn calculate_pressure(
        &self,
        pos_x: i32,
        pos_y: i32,
        pos_z: i32,
        barrier_noise_value: &mut f64,
        status1: FluidStatus,
        status2: FluidStatus,
    ) -> f64 {
        let type1 = status1.at(pos_y);
        let type2 = status2.at(pos_y);
        let lava_water = type1 == Fluid::Lava && type2 == Fluid::Water;
        let water_lava = type1 == Fluid::Water && type2 == Fluid::Lava;
        if lava_water || water_lava {
            return 2.0;
        }

        let fluid_y_diff = (status1.fluid_level - status2.fluid_level).abs();
        if fluid_y_diff == 0 {
            return 0.0;
        }

        let average_fluid_y = 0.5 * f64::from(status1.fluid_level + status2.fluid_level);
        let how_far_above = f64::from(pos_y) + 0.5 - average_fluid_y;
        let base_value = f64::from(fluid_y_diff) / 2.0;
        let distance_from_edge = base_value - how_far_above.abs();
        let gradient = if how_far_above > 0.0 {
            let center_point = 0.0 + distance_from_edge;
            if center_point > 0.0 {
                center_point / 1.5
            } else {
                center_point / 2.5
            }
        } else {
            let center_point = 3.0 + distance_from_edge;
            if center_point > 0.0 {
                center_point / 3.0
            } else {
                center_point / 10.0
            }
        };

        // Preserves vanilla's exact `!(d < -2.0) && !(d > 2.0)` bounds check
        // rather than `(-2.0..=2.0).contains(&d)`, so NaN handling matches.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        let noise_value = if !(gradient < -2.0) && !(gradient > 2.0) {
            if barrier_noise_value.is_nan() {
                let b = self.barrier.compute(DfContext::new(pos_x, pos_y, pos_z));
                *barrier_noise_value = b;
                b
            } else {
                *barrier_noise_value
            }
        } else {
            0.0
        };

        2.0 * (noise_value + gradient)
    }

    fn compute_aquifer_fluid(&self, x: i32, y: i32, z: i32) -> FluidStatus {
        let global_fluid = self.global_fluid(y);
        let mut lowest_preliminary_surface = i32::MAX;
        let top_of_cell = y + 12;
        let bottom_of_cell = y - 12;
        let mut surface_at_center_under_global = false;

        for offset in SURFACE_SAMPLING_OFFSETS_IN_CHUNKS {
            let sample_x = x + section_to_block(offset[0]);
            let sample_z = z + section_to_block(offset[1]);
            let preliminary_surface_level = self.preliminary_surface_level(sample_x, sample_z);
            let adjusted_surface_level = self.adjust_surface_level(preliminary_surface_level);
            let start = offset[0] == 0 && offset[1] == 0;
            if start && bottom_of_cell > adjusted_surface_level {
                return global_fluid;
            }

            let top_pokes_above = top_of_cell > adjusted_surface_level;
            if top_pokes_above || start {
                let global_at_surface = self.global_fluid(adjusted_surface_level);
                if global_at_surface.at(adjusted_surface_level) != Fluid::Air {
                    if start {
                        surface_at_center_under_global = true;
                    }
                    if top_pokes_above {
                        return global_at_surface;
                    }
                }
            }

            lowest_preliminary_surface = lowest_preliminary_surface.min(preliminary_surface_level);
        }

        let fluid_surface_level = self.compute_surface_level(
            x,
            y,
            z,
            global_fluid,
            lowest_preliminary_surface,
            surface_at_center_under_global,
        );
        FluidStatus {
            fluid_level: fluid_surface_level,
            fluid_type: self.compute_fluid_type(x, y, z, global_fluid, fluid_surface_level),
        }
    }

    fn is_deep_dark_region(&self, x: i32, y: i32, z: i32) -> bool {
        // Vanilla compares against float literals (`-0.225F`, `0.9F`); the double
        // promotion of those floats is not the same as the double literals, so
        // the thresholds must be built from `f32` to match bit-for-bit.
        self.erosion.sample(x, y, z) < f64::from(-0.225_f32)
            && self.depth.sample(x, y, z) > f64::from(0.9_f32)
    }

    fn compute_surface_level(
        &self,
        x: i32,
        y: i32,
        z: i32,
        global_fluid: FluidStatus,
        lowest_preliminary_surface: i32,
        surface_at_center_under_global: bool,
    ) -> i32 {
        let (partially_floodedness, fully_floodedness) = if self.is_deep_dark_region(x, y, z) {
            (-1.0, -1.0)
        } else {
            let distance_below_surface = lowest_preliminary_surface + 8 - y;
            let floodedness_factor = if surface_at_center_under_global {
                clamped_map(f64::from(distance_below_surface), 0.0, 64.0, 1.0, 0.0)
            } else {
                0.0
            };
            let floodedness_noise =
                clamp(self.floodedness.compute(DfContext::new(x, y, z)), -1.0, 1.0);
            let fully_threshold = map(floodedness_factor, 1.0, 0.0, -0.3, 0.8);
            let partially_threshold = map(floodedness_factor, 1.0, 0.0, -0.8, 0.4);
            (
                floodedness_noise - partially_threshold,
                floodedness_noise - fully_threshold,
            )
        };

        if fully_floodedness > 0.0 {
            global_fluid.fluid_level
        } else if partially_floodedness > 0.0 {
            self.compute_randomized_fluid_surface_level(x, y, z, lowest_preliminary_surface)
        } else {
            WAY_BELOW_MIN_Y
        }
    }

    fn compute_randomized_fluid_surface_level(
        &self,
        x: i32,
        y: i32,
        z: i32,
        lowest_preliminary_surface: i32,
    ) -> i32 {
        let fluid_level_cell_x = x.div_euclid(16);
        let fluid_level_cell_y = y.div_euclid(40);
        let fluid_level_cell_z = z.div_euclid(16);
        let fluid_cell_middle_y = fluid_level_cell_y * 40 + 20;
        let fluid_level_spread = self.spread.compute(DfContext::new(
            fluid_level_cell_x,
            fluid_level_cell_y,
            fluid_level_cell_z,
        )) * 10.0;
        let quantized = quantize(fluid_level_spread, 3);
        let target = fluid_cell_middle_y + quantized;
        lowest_preliminary_surface.min(target)
    }

    fn compute_fluid_type(
        &self,
        x: i32,
        y: i32,
        z: i32,
        global_fluid: FluidStatus,
        fluid_surface_level: i32,
    ) -> Fluid {
        let mut fluid_type = global_fluid.fluid_type;
        if fluid_surface_level <= -10
            && fluid_surface_level != WAY_BELOW_MIN_Y
            && global_fluid.fluid_type != Fluid::Lava
        {
            let cell_x = x.div_euclid(64);
            let cell_y = y.div_euclid(40);
            let cell_z = z.div_euclid(64);
            let lava_noise = self.lava.compute(DfContext::new(cell_x, cell_y, cell_z));
            if lava_noise.abs() > 0.3 {
                fluid_type = Fluid::Lava;
            }
        }
        fluid_type
    }
}

impl Drop for AquiferSystem {
    fn drop(&mut self) {
        let scratch = AquiferScratch {
            aquifer: std::mem::take(self.aquifer_cache.get_mut()),
            locations: std::mem::take(self.location_cache.get_mut()),
        };
        return_aquifer_scratch(scratch);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        reset_vertical_run_scratch_initializations, vertical_run_scratch_initializations,
        AquiferRegionCache, AquiferSystem, BlockKind, Fluid, FluidStatus, VerticalRunScratch,
        VERTICAL_CANDIDATE_COUNT,
    };
    use crate::density::{Builder, Context, Context as DfContext, Density, NoiseParams, Resolver};
    use crate::engine::{PointProgram, PointScratch};
    use crate::math::floor;
    use crate::noise::{BlendedNoise, NormalNoise};
    use crate::rng::LegacyRandomSource;
    use serde_json::Value;
    use std::sync::Arc;

    struct NoReferences;

    impl Resolver for NoReferences {
        fn density_function(&self, id: &str) -> Value {
            panic!("unexpected density-function reference: {id}");
        }

        fn noise(&self, id: &str) -> NoiseParams {
            panic!("unexpected noise reference: {id}");
        }
    }

    fn constant_density() -> Value {
        serde_json::json!({"type": "minecraft:constant", "argument": 0.0})
    }

    fn nonlinear_density() -> Value {
        serde_json::json!({
            "type": "minecraft:interpolated",
            "argument": {
                "type": "minecraft:square",
                "argument": {
                    "type": "minecraft:y_clamped_gradient",
                    "from_y": 0,
                    "to_y": 8,
                    "from_value": 0.0,
                    "to_value": 1.0
                }
            }
        })
    }

    fn nonconstant_negative_density() -> Value {
        serde_json::json!({
            "type": "minecraft:y_clamped_gradient",
            "from_y": 0,
            "to_y": 16,
            "from_value": -0.25,
            "to_value": -0.75
        })
    }

    fn preliminary_find_top_density() -> Value {
        serde_json::json!({
            "type": "minecraft:find_top_surface",
            "cell_height": 8,
            "lower_bound": -16,
            "upper_bound": 32.0,
            "density": {
                "type": "minecraft:add",
                "argument1": {
                    "type": "minecraft:y_clamped_gradient",
                    "from_y": -16,
                    "to_y": 16,
                    "from_value": -1.0,
                    "to_value": 1.0
                },
                "argument2": {
                    "type": "minecraft:cache_2d",
                    "argument": 0.25
                }
            }
        })
    }

    #[test]
    fn direct_noise_point_route_preserves_bits() {
        let mut random = LegacyRandomSource::new(17);
        let density = std::sync::Arc::new(Density::Noise {
            noise: NormalNoise::create(&mut random, -2, &[1.0, 0.5]),
            xz_scale: 0.75,
            y_scale: 0.125,
        });
        let route = super::PointDensity::from_arc(std::sync::Arc::clone(&density));
        assert!(matches!(route, super::PointDensity::SimpleNoise { .. }));
        for (x, y, z) in [(0, 0, 0), (17, -23, 41), (-101, 64, 7)] {
            let ctx = Context::new(x, y, z);
            assert_eq!(route.compute(ctx).to_bits(), density.compute(ctx).to_bits());
        }
    }

    #[test]
    fn preliminary_compiled_route_allocates_scratch_only_on_first_compute() {
        let tree = Arc::new(Density::YClampedGradient {
            from_y: -16.0,
            to_y: 16.0,
            from_value: -1.0,
            to_value: 1.0,
        });
        let route = super::PreliminaryPointDensity::from_arc_and_program(
            Arc::clone(&tree),
            Some(Arc::new(PointProgram::compile(&tree))),
        );
        assert!(route.is_compiled());
        assert!(!route.scratch_allocated());
        #[cfg(feature = "gen-counters")]
        assert_eq!(route.scratch_allocations(), 0);
        let context = Context::new(7, 3, -11);
        assert_eq!(route.compute(context).to_bits(), tree.compute(context).to_bits());
        assert!(route.scratch_allocated());
        #[cfg(feature = "gen-counters")]
        assert_eq!(route.scratch_allocations(), 1);
    }

    #[test]
    fn production_preliminary_route_is_compiled_and_bit_exact() {
        let mut settings = test_settings(constant_density(), 8);
        settings["noise_router"]["preliminary_surface_level"] = preliminary_find_top_density();
        let resolver = NoReferences;
        let builder = Builder::new(0, &resolver);
        let tree = builder
            .build(&settings["noise_router"]["preliminary_surface_level"])
            .expect("preliminary fixture density");
        let system = AquiferSystem::new(&settings, &builder, 0, 0);

        assert!(system.prelim.is_compiled());
        assert!(system.preliminary_surface_program_nodes() > 0);
        assert!(
            !system.prelim.scratch_allocated(),
            "shared preliminary cache should own compiled scratch"
        );

        // This is the negative control for the production-consumption witness:
        // constructing a point route directly from the tree remains recursive,
        // so the assertion cannot pass merely because every point route was
        // changed to report itself as compiled.
        let recursive = super::PointDensity::from_arc(std::sync::Arc::new(tree.clone()));
        assert!(!recursive.is_compiled());

        let sampled = system.preliminary_surface_level(1000, 1000);
        assert_eq!(
            sampled,
            floor(tree.compute(Context::new(1000, 0, 1000))),
            "production preliminary request did not use the compiled route"
        );

        for (x, y, z) in [(-9, -17, 4), (0, 0, 0), (7, 15, -6), (23, 64, 11)] {
            let context = Context::new(x, y, z);
            assert_eq!(
                system.prelim.compute(context).to_bits(),
                tree.compute(context).to_bits(),
                "compiled preliminary mismatch at ({x}, {y}, {z})"
            );
        }
    }

    #[test]
    fn production_compound_aquifer_routes_are_compiled_and_bit_exact() {
        let route = nonlinear_density();
        let mut settings = test_settings(constant_density(), 8);
        for name in [
            "barrier",
            "fluid_level_floodedness",
            "fluid_level_spread",
            "lava",
        ] {
            settings["noise_router"][name] = route.clone();
        }
        let resolver = NoReferences;
        let builder = Builder::new(0, &resolver);
        let expected = [
            builder
                .build(&settings["noise_router"]["barrier"])
                .expect("barrier fixture density"),
            builder
                .build(&settings["noise_router"]["fluid_level_floodedness"])
                .expect("floodedness fixture density"),
            builder
                .build(&settings["noise_router"]["fluid_level_spread"])
                .expect("spread fixture density"),
            builder
                .build(&settings["noise_router"]["lava"])
                .expect("lava fixture density"),
        ];
        let system = AquiferSystem::new(&settings, &builder, 0, 0);
        assert!(system.barrier.is_compiled());
        assert!(system.floodedness.is_compiled());
        assert!(system.spread.is_compiled());
        assert!(system.lava.is_compiled());

        let routes = [&system.barrier, &system.floodedness, &system.spread, &system.lava];
        for (route, expected) in routes.into_iter().zip(expected) {
            for context in [
                Context::new(-9, -17, 4),
                Context::new(0, 0, 0),
                Context::new(7, 15, -6),
                Context::new(23, 64, 11),
            ] {
                assert_eq!(
                    route.compute(context).to_bits(),
                    expected.compute(context).to_bits(),
                    "compiled aquifer route mismatch at ({},{},{})",
                    context.x,
                    context.y,
                    context.z,
                );
            }
        }
    }

    #[test]
    fn compiled_aquifer_route_bundle_is_reusable_and_has_exact_fallback() {
        let resolver = NoReferences;
        let builder = Builder::new(0, &resolver);
        let route = Arc::new(
            builder
                .build(&nonlinear_density())
                .expect("compound route fixture density"),
        );
        let routes = super::CompiledAquiferPointRoutes::from_trees(
            &route,
            &route,
            &route,
            &route,
        );
        assert!(routes.barrier.is_some());
        assert!(routes.floodedness.is_some());
        assert!(routes.spread.is_some());
        assert!(routes.lava.is_some());

        let first = super::PointDensity::from_arc_and_program(
            Arc::clone(&route),
            routes.barrier.clone(),
        );
        let second = super::PointDensity::from_arc_and_program(
            Arc::clone(&route),
            routes.barrier.clone(),
        );
        let (first_program, second_program) = match (&first, &second) {
            (
                super::PointDensity::Compiled { program: first, .. },
                super::PointDensity::Compiled { program: second, .. },
            ) => (first, second),
            _ => panic!("compound route did not select the compiled path"),
        };
        assert!(Arc::ptr_eq(first_program, second_program));

        let contexts = [
            Context::new(-9, -17, 4),
            Context::new(0, 0, 0),
            Context::new(7, 15, -6),
            Context::new(23, 64, 11),
        ];
        let mut recursive_digest = 0xcbf2_9ce4_8422_2325u64;
        let mut compiled_digest = recursive_digest;
        for context in contexts {
            let expected = route.compute(context);
            let actual = first.compute(context);
            assert_eq!(actual.to_bits(), expected.to_bits());
            recursive_digest = recursive_digest
                .wrapping_mul(0x1000_0000_01b3)
                .wrapping_add(expected.to_bits());
            compiled_digest = compiled_digest
                .wrapping_mul(0x1000_0000_01b3)
                .wrapping_add(actual.to_bits());
        }
        assert_eq!(compiled_digest, recursive_digest);

        let mut random = LegacyRandomSource::new(17);
        let unsupported = Arc::new(Density::Blended(BlendedNoise::new(
            &mut random,
            1.0,
            1.0,
            1.0,
            1.0,
            1.0,
        )));
        let fallback_routes = super::CompiledAquiferPointRoutes::from_trees(
            &unsupported,
            &unsupported,
            &unsupported,
            &unsupported,
        );
        assert!(fallback_routes.barrier.is_none());
        let fallback = super::PointDensity::from_arc_and_program(
            Arc::clone(&unsupported),
            fallback_routes.barrier,
        );
        assert!(!fallback.is_compiled());
        for context in contexts {
            assert_eq!(fallback.compute(context).to_bits(), unsupported.compute(context).to_bits());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "local instruction/cycle control for compiled aquifer routes"]
    #[allow(unsafe_code)]
    fn compiled_aquifer_route_instruction_cycle_control() {
        use std::hint::black_box;
        use std::mem::size_of;

        #[repr(C)]
        #[derive(Default, Clone, Copy)]
        struct RusageInfoV4 {
            uuid: [u8; 16],
            prefix: [u64; 29],
            instructions: u64,
            cycles: u64,
            suffix: [u64; 5],
        }

        unsafe extern "C" {
            fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
        }

        fn usage() -> (u64, u64) {
            assert_eq!(size_of::<RusageInfoV4>(), 304);
            let mut info = RusageInfoV4::default();
            let result = unsafe {
                proc_pid_rusage(
                    i32::try_from(std::process::id()).expect("pid fits in i32"),
                    4,
                    (&raw mut info).cast::<core::ffi::c_void>(),
                )
            };
            assert_eq!(result, 0, "proc_pid_rusage failed with {result}");
            (info.instructions, info.cycles)
        }

        let mut tree = Density::YClampedGradient {
            from_y: -64.0,
            to_y: 320.0,
            from_value: -1.0,
            to_value: 1.0,
        };
        for index in 0..16 {
            tree = Density::Add(
                Box::new(tree),
                Box::new(Density::Const(f64::from(index) * 0.03125)),
            );
        }
        let tree = Arc::new(tree);
        let program = Arc::new(PointProgram::compile(&tree));
        let recursive = super::PointDensity::from_arc(Arc::clone(&tree));
        let compiled = super::PointDensity::from_arc_and_program(
            Arc::clone(&tree),
            Some(program),
        );
        let contexts: Vec<_> = (0..16_384)
            .map(|index| {
                Context::new(
                    index % 257 - 128,
                    index % 385 - 64,
                    (index * 3) % 257 - 128,
                )
            })
            .collect();

        let mut recursive_digest = 0_u64;
        let before_recursive = usage();
        for context in &contexts {
            recursive_digest ^= black_box(recursive.compute(*context).to_bits());
        }
        let after_recursive = usage();

        let mut compiled_digest = 0_u64;
        let before_compiled = usage();
        for context in &contexts {
            compiled_digest ^= black_box(compiled.compute(*context).to_bits());
        }
        let after_compiled = usage();
        assert_eq!(compiled_digest, recursive_digest);
        println!(
            "AQUIFER_POINT_CONTROL samples={} recursive_instructions={} compiled_instructions={} recursive_cycles={} compiled_cycles={} digest={compiled_digest:016x}",
            contexts.len(),
            after_recursive.0.saturating_sub(before_recursive.0),
            after_compiled.0.saturating_sub(before_compiled.0),
            after_recursive.1.saturating_sub(before_recursive.1),
            after_compiled.1.saturating_sub(before_compiled.1),
        );
    }

    fn test_settings(final_density: Value, sea_level: i32) -> Value {
        serde_json::json!({
            "aquifers_enabled": true,
            "sea_level": sea_level,
            "default_fluid": {"Name": "minecraft:water"},
            "noise": {
                "min_y": 0,
                "height": 16,
                "size_horizontal": 1,
                "size_vertical": 2
            },
            "noise_router": {
                "final_density": final_density,
                "erosion": constant_density(),
                "depth": constant_density(),
                "barrier": constant_density(),
                "fluid_level_floodedness": constant_density(),
                "fluid_level_spread": constant_density(),
                "lava": constant_density(),
                "preliminary_surface_level": constant_density()
            }
        })
    }

    #[test]
    fn enabled_aquifer_uses_settings_cell_geometry() {
        let settings = serde_json::json!({
            "aquifers_enabled": true,
            "sea_level": 0,
            "default_fluid": {"Name": "minecraft:water"},
            "noise": {
                "min_y": 0,
                "height": 16,
                "size_horizontal": 2,
                "size_vertical": 1
            },
            "noise_router": {
                "final_density": nonlinear_density(),
                "erosion": constant_density(),
                "depth": constant_density(),
                "barrier": constant_density(),
                "fluid_level_floodedness": constant_density(),
                "fluid_level_spread": constant_density(),
                "lava": constant_density(),
                "preliminary_surface_level": constant_density()
            }
        });
        let resolver = NoReferences;
        let builder = Builder::new(0, &resolver);
        let system = AquiferSystem::new(&settings, &builder, 0, 0);

        // The square of the gradient is 0 at y=0 and 0.25 at y=4. With a
        // four-block vertical cell, y=2 is halfway between those corners.
        assert_eq!(system.final_density.final_density(0, 2, 0).to_bits(), 0.125_f64.to_bits());
    }

    #[test]
    fn density_column_resolves_same_blocks_as_scalar_path() {
        let settings = test_settings(nonlinear_density(), 8);
        let resolver = NoReferences;
        let builder = Builder::new(0, &resolver);
        let column_path = AquiferSystem::new(&settings, &builder, 0, 0);
        let scalar_path = AquiferSystem::new(&settings, &builder, 0, 0);
        let mut density = [0.0; 16];
        column_path.final_density_column(7, 9, 0, &mut density);

        for (offset, value) in density.iter().copied().enumerate() {
            let y = offset as i32;
            assert_eq!(
                column_path.block_at_density(7, y, 9, value),
                scalar_path.block_at(7, y, 9),
                "y={y}"
            );
        }
    }

    #[test]
    fn vertical_density_run_matches_scalar_across_anchor_seams() {
        let settings = test_settings(constant_density(), 8);
        let resolver = NoReferences;
        let builder = Builder::new(0, &resolver);
        let run_path = AquiferSystem::new(&settings, &builder, 0, 0);
        let scalar_path = AquiferSystem::new(&settings, &builder, 0, 0);
        let y_start = 0;
        let mut densities = [0.0; 16];
        run_path.final_density_column(7, 9, y_start, &mut densities);
        let mut blocks = [BlockKind::Stone; 16];
        run_path.block_at_density_vertical_run(7, 9, y_start, &densities, &mut blocks);

        for (offset, (&density, &block)) in densities.iter().zip(&blocks).enumerate() {
            let y = y_start + offset as i32;
            assert_eq!(block, scalar_path.block_at_density(7, y, 9, density), "y={y}");
        }
    }

    #[test]
    fn vertical_slice_reuse_is_bit_exact_at_nonconstant_anchor_seam() {
        let settings = test_settings(nonconstant_negative_density(), 8);
        let resolver = NoReferences;
        let builder = Builder::new(0, &resolver);
        let reused_path = AquiferSystem::new(&settings, &builder, 0, 0);
        let scalar_path = AquiferSystem::new(&settings, &builder, 0, 0);
        let mut densities = [0.0; 16];
        reused_path.final_density_column(7, 9, 0, &mut densities);
        assert!(densities
            .windows(2)
            .any(|window| window[0].to_bits() != window[1].to_bits()));
        assert!(densities.iter().all(|density| *density <= 0.0));

        reset_vertical_run_scratch_initializations();
        let mut reused_blocks = [BlockKind::Stone; 16];
        let mut reused_state = reused_path.vertical_run_state();
        for y_start in [0, 8] {
            let start = y_start as usize;
            reused_path.block_at_density_vertical_slice(
                7,
                9,
                y_start,
                &densities[start..start + 8],
                &mut reused_blocks[start..start + 8],
                &mut reused_state,
            );
        }
        let reused_initializations = vertical_run_scratch_initializations();

        reset_vertical_run_scratch_initializations();
        let mut baseline_blocks = [BlockKind::Stone; 16];
        for y_start in [0, 8] {
            let start = y_start as usize;
            let mut state = reused_path.vertical_run_state();
            reused_path.block_at_density_vertical_slice(
                7,
                9,
                y_start,
                &densities[start..start + 8],
                &mut baseline_blocks[start..start + 8],
                &mut state,
            );
        }
        let baseline_initializations = vertical_run_scratch_initializations();

        assert_eq!(reused_blocks, baseline_blocks);
        for (y, (&density, &block)) in densities.iter().zip(&reused_blocks).enumerate() {
            assert_eq!(
                block,
                scalar_path.block_at(7, y as i32, 9),
                "scalar final-density path diverged at y={y}"
            );
            assert_eq!(
                block,
                scalar_path.block_at_density(7, y as i32, 9, density),
                "scalar supplied-density path diverged at y={y}"
            );
        }
        assert_eq!(baseline_initializations, 3);
        assert_eq!(reused_initializations, 2);
        assert_eq!(baseline_initializations - reused_initializations, 1);
    }

    #[test]
    fn vertical_run_scratch_preserves_later_ties_and_invalidates_gaps() {
        let mut scratch = VerticalRunScratch {
            anchor_x: 1,
            anchor_y: 2,
            anchor_z: 3,
            current_y: 4,
            indices: std::array::from_fn(|index| index),
            distances: [7; VERTICAL_CANDIDATE_COUNT],
            deltas: [0; VERTICAL_CANDIDATE_COUNT],
        };
        assert_eq!(scratch.closest_three().0, 11);
        assert!(scratch.can_advance_to(1, 2, 3, 5));
        assert!(!scratch.can_advance_to(1, 2, 3, 6));
        assert!(!scratch.can_advance_to(1, 4, 3, 5));
        scratch.advance();
        assert_eq!(scratch.current_y, 5);
    }

    #[test]
    fn vertical_run_scratch_fits_request_local_budget() {
        assert!(std::mem::size_of::<VerticalRunScratch>() <= 256);
    }

    #[test]
    fn aquifer_region_cache_keys_locations_and_statuses_by_absolute_grid_coordinate() {
        use std::cell::Cell;

        let mut cache = AquiferRegionCache::with_capacity(32);
        let positional = crate::rng::Algorithm::Legacy.root_positional(17);
        let first = cache.get_or_compute_location(-1, -2, 3, positional);
        assert_eq!(
            cache.get_or_compute_location(-1, -2, 3, positional),
            first
        );
        let _translated = cache.get_or_compute_location(0, -2, 3, positional);

        let status_calls = Cell::new(0);
        let first_status = cache.get_or_compute_status(-1, -2, 3, || {
            status_calls.set(status_calls.get() + 1);
            FluidStatus {
                fluid_level: -54,
                fluid_type: Fluid::Lava,
            }
        });
        let second_status = cache.get_or_compute_status(-1, -2, 3, || {
            status_calls.set(status_calls.get() + 1);
            panic!("cached status should not invoke its fallback")
        });
        assert_eq!(first_status, second_status);
        assert_eq!(status_calls.get(), 1);

        let stats = cache.stats();
        assert_eq!(stats.location_computes, 2);
        assert_eq!(stats.location_hits, 1);
        assert_eq!(stats.status_computes, 1);
        assert_eq!(stats.status_hits, 1);
        assert!(stats.retained_entries <= stats.capacity);
    }

    #[test]
    fn aquifer_region_cache_reduces_4x4_candidate_work_without_changing_blocks() {
        fn run(shared: bool) -> (Vec<BlockKind>, super::AquiferRegionCacheStats) {
            let settings = test_settings(constant_density(), 8);
            let resolver = NoReferences;
            let builder = Builder::new(0, &resolver);
            let mut shared_cache = AquiferRegionCache::new();
            let mut blocks = Vec::with_capacity(4 * 4 * 16 * 16 * 16);
            let mut scalar_stats = super::AquiferRegionCacheStats::default();
            for cz in 0..4 {
                for cx in 0..4 {
                    let mut local_cache = AquiferRegionCache::new();
                    let cache = if shared {
                        &mut shared_cache
                    } else {
                        &mut local_cache
                    };
                    let system = AquiferSystem::new(&settings, &builder, cx, cz);
                    let mut densities = [0.0; 16];
                    let mut column_blocks = [BlockKind::Air; 16];
                    for z in 0..16 {
                        for x in 0..16 {
                            let mut state = system.vertical_run_state();
                            system.final_density_column(
                                cx * 16 + x,
                                cz * 16 + z,
                                0,
                                &mut densities,
                            );
                            system.block_at_density_vertical_slice_with_region_cache(
                                cx * 16 + x,
                                cz * 16 + z,
                                0,
                                &densities,
                                &mut column_blocks,
                                &mut state,
                                cache,
                            );
                            blocks.extend(column_blocks);
                        }
                    }
                    if !shared {
                        let stats = cache.stats();
                        scalar_stats.location_computes += stats.location_computes;
                        scalar_stats.status_computes += stats.status_computes;
                    }
                }
            }
            if shared {
                (blocks, shared_cache.stats())
            } else {
                (blocks, scalar_stats)
            }
        }

        let (scalar_blocks, scalar_stats) = run(false);
        let (shared_blocks, shared_stats) = run(true);
        assert_eq!(shared_blocks, scalar_blocks);
        assert_eq!(scalar_stats.location_computes, 576);
        assert_eq!(shared_stats.location_computes, 144);
        assert_eq!(scalar_stats.status_computes, 286);
        assert_eq!(shared_stats.status_computes, 98);
        assert!(shared_stats.retained_entries <= shared_stats.capacity);
    }

    #[test]
    fn supplied_density_path_does_not_resample_final_density() {
        let settings = test_settings(constant_density(), 4);
        let resolver = NoReferences;
        let builder = Builder::new(0, &resolver);
        let system = AquiferSystem::new(&settings, &builder, 0, 0);

        let scalar = system.block_at(0, 0, 0);
        assert_ne!(scalar, BlockKind::Stone);
        assert_eq!(system.block_at_density(0, 0, 0, 1.0), BlockKind::Stone);
    }

    #[test]
    fn enabled_aquifer_uses_settings_default_fluid() {
        let settings = serde_json::json!({
            "aquifers_enabled": true,
            "sea_level": 4,
            "default_fluid": {"Name": "minecraft:lava"},
            "noise": {
                "min_y": 0,
                "height": 16,
                "size_horizontal": 1,
                "size_vertical": 2
            },
            "noise_router": {
                "final_density": constant_density(),
                "erosion": constant_density(),
                "depth": constant_density(),
                "barrier": constant_density(),
                "fluid_level_floodedness": constant_density(),
                "fluid_level_spread": constant_density(),
                "lava": constant_density(),
                "preliminary_surface_level": constant_density()
            }
        });
        let resolver = NoReferences;
        let builder = Builder::new(0, &resolver);
        let system = AquiferSystem::new(&settings, &builder, 0, 0);

        // At y=0 the global status is below its level 4, so an enabled
        // aquifer must preserve the settings-selected lava identity.
        assert_eq!(system.block_at(0, 0, 0), BlockKind::Lava);
    }

    #[test]
    fn preliminary_cache_preserves_integer_skip_bound_across_translation() {
        let settings = serde_json::json!({
            "aquifers_enabled": true,
            "sea_level": 0,
            "default_fluid": {"Name": "minecraft:water"},
            "noise": {
                "min_y": 0,
                "height": 16,
                "size_horizontal": 1,
                "size_vertical": 2
            },
            "noise_router": {
                "final_density": constant_density(),
                "erosion": constant_density(),
                "depth": constant_density(),
                "barrier": constant_density(),
                "fluid_level_floodedness": constant_density(),
                "fluid_level_spread": constant_density(),
                "lava": constant_density(),
                "preliminary_surface_level": constant_density()
            }
        });
        let resolver = NoReferences;
        let builder = Builder::new(0, &resolver);
        let origin = AquiferSystem::new(&settings, &builder, 0, 0);
        let translated = AquiferSystem::new(&settings, &builder, -1, -1);

        // max preliminary = 0, adjustment = 8, then the exact integer grid
        // conversion yields grid 2 and the last block y = 34.
        assert_eq!(origin.skip_sampling_above_y, 34);
        assert_eq!(translated.skip_sampling_above_y, origin.skip_sampling_above_y);
    }

    #[test]
    fn preliminary_cache_is_once_filled_for_negative_and_translated_keys() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = std::sync::Arc::new(super::PreliminarySurfaceCache::new());
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let cache = std::sync::Arc::clone(&cache);
                let calls = std::sync::Arc::clone(&calls);
                scope.spawn(move || {
                    assert_eq!(
                        cache.get_or_compute(-4, -8, || {
                            calls.fetch_add(1, Ordering::Relaxed);
                            17
                        }),
                        17
                    );
                });
            }
        });
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            cache.get_or_compute(4, -8, || {
                calls.fetch_add(1, Ordering::Relaxed);
                23
            }),
            23
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn preliminary_cache_batch_deduplicates_keys_and_preserves_order() {
        use std::cell::Cell;

        let cache = super::PreliminarySurfaceCache::new();
        let calls = Cell::new(0);
        let keys = [(-4, -8), (4, -8), (-4, -8), (12, 16), (4, -8)];
        let mut output = [0; 5];
        cache.get_or_compute_preliminary_batch(&keys, &mut output, |qx, qz| {
            calls.set(calls.get() + 1);
            qx + qz
        });
        assert_eq!(output, [-12, -4, -12, 28, -4]);
        assert_eq!(calls.get(), 3, "each unique key should be evaluated once");
        let stats = cache.stats();
        assert_eq!(stats.lookups, keys.len() as u64);
        assert_eq!(stats.misses, 3);
        assert_eq!(stats.hits, 2);
    }

    #[test]
    fn preliminary_cache_batch_defers_when_a_shard_is_full() {
        use std::cell::Cell;

        let cache = super::PreliminarySurfaceCache::with_capacity(32);
        let first = (0, 0);
        let shard = super::PreliminarySurfaceCache::locate(
            first.0,
            first.1,
            cache.shard_capacity,
        )
        .0;
        let mut second = (1, 0);
        while super::PreliminarySurfaceCache::locate(
            second.0,
            second.1,
            cache.shard_capacity,
        )
        .0 != shard
        {
            second.0 += 1;
        }
        let calls = Cell::new(0);
        let keys = [first, second];
        let mut output = [0; 2];
        cache.get_or_compute_preliminary_batch(&keys, &mut output, |qx, qz| {
            calls.set(calls.get() + 1);
            qx + qz
        });
        assert_eq!(output, [0, second.0 + second.1]);
        assert_eq!(calls.get(), 2);
        assert_eq!(cache.stats().misses, 2);
    }

    #[test]
    fn preliminary_cache_batch_uses_compiled_program_scratch() {
        let density = Density::FindTopSurface {
            density: Box::new(Density::Const(1.0)),
            upper_bound: Box::new(Density::Const(8.0)),
            lower_bound: 0,
            cell_height: 8,
        };
        let program = Arc::new(PointProgram::compile(&density));
        let cache = super::PreliminarySurfaceCache::with_program(Arc::clone(&program), 32);
        let keys = [(-4, -8), (4, -8), (12, 16)];
        let mut output = [0; 3];
        cache.get_or_compute_preliminary_batch(&keys, &mut output, |_, _| unreachable!());
        assert_eq!(output, [8, 8, 8]);
        let mut expected_scratch = PointScratch::new();
        for (key, value) in keys.iter().zip(output) {
            assert_eq!(
                value,
                floor(program.compute(DfContext::new(key.0, 0, key.1), &mut expected_scratch))
            );
        }
        let stats = cache.stats();
        let fixed_bytes = stats.capacity * stats.entry_bytes
            + stats.scratch_slots * super::POINT_SCRATCH_ENTRY_BYTES;
        assert!(stats.retained_bytes > fixed_bytes);
    }

    #[test]
    fn preliminary_batch_reservation_makes_scalar_racer_wait() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Barrier};

        let cache = Arc::new(super::PreliminarySurfaceCache::new());
        let entered = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(Barrier::new(2));
        let key = (20, -28);
        let (batch_value, scalar_value) = std::thread::scope(|scope| {
            let batch_cache = Arc::clone(&cache);
            let batch_entered = Arc::clone(&entered);
            let batch_gate = Arc::clone(&gate);
            let batch = scope.spawn(move || {
                let mut output = [0];
                batch_cache.get_or_compute_preliminary_batch(
                    &[key],
                    &mut output,
                    |_, _| {
                        batch_entered.store(true, Ordering::Release);
                        batch_gate.wait();
                        17
                    },
                );
                output[0]
            });
            while !entered.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            let scalar_cache = Arc::clone(&cache);
            let scalar = scope.spawn(move || {
                scalar_cache.get_or_compute_preliminary(key.0, key.1, || 99)
            });
            while cache.stats().hits == 0 {
                std::thread::yield_now();
            }
            gate.wait();
            (batch.join().unwrap(), scalar.join().unwrap())
        });
        assert_eq!((batch_value, scalar_value), (17, 17));
        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 1);
    }

    #[test]
    fn preliminary_cache_retains_bounded_compiled_route_scratch() {
        let cache = super::PreliminarySurfaceCache::new();
        let stats = cache.stats();
        assert_eq!(stats.capacity, super::PRELIMINARY_CACHE_SCALAR_CAPACITY);
        assert_eq!(
            stats.scratch_slots,
            super::PRELIMINARY_CACHE_SHARDS * super::PRELIMINARY_POINT_SCRATCH_CAPACITY
        );
        assert_eq!(
            stats.retained_bytes,
            stats.capacity * stats.entry_bytes
                + (stats.scratch_slots + super::PRELIMINARY_POINT_SCRATCH_CAPACITY)
                    * super::POINT_SCRATCH_ENTRY_BYTES
                + super::PRELIMINARY_CACHE_SHARDS
                    * cache.shard_capacity
                    * std::mem::size_of::<super::PreliminarySurfacePending>()
        );
    }

    #[test]
    fn preliminary_cache_allows_distinct_shards_to_compute_concurrently() {
        use std::sync::{Arc, Barrier};

        let cache = Arc::new(super::PreliminarySurfaceCache::new());
        let first = (0, 0);
        let mut second = (1, 0);
        while super::PreliminarySurfaceCache::locate(
            first.0,
            first.1,
            cache.shard_capacity,
        )
        .0
            == super::PreliminarySurfaceCache::locate(
                second.0,
                second.1,
                cache.shard_capacity,
            )
            .0
        {
            second.0 += 1;
        }
        let gate = Arc::new(Barrier::new(2));
        let (first_value, second_value) = std::thread::scope(|scope| {
            let first_cache = Arc::clone(&cache);
            let first_gate = Arc::clone(&gate);
            let first_thread = scope.spawn(move || {
                first_cache.get_or_compute(first.0, first.1, || {
                    first_gate.wait();
                    17
                })
            });
            let second_cache = Arc::clone(&cache);
            let second_gate = Arc::clone(&gate);
            let second_thread = scope.spawn(move || {
                second_cache.get_or_compute(second.0, second.1, || {
                    second_gate.wait();
                    23
                })
            });
            (first_thread.join().unwrap(), second_thread.join().unwrap())
        });
        assert_eq!((first_value, second_value), (17, 23));
        let stats = cache.stats();
        assert_eq!(stats.lookups, 2);
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 2);
        assert_eq!(stats.evictions, 0);
    }
}
