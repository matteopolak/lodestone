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

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::density::{Builder, Context as DfContext, Density, NoiseChunkSampler};
use crate::engine::{Bounds, Program};
use crate::math::{clamp, clamped_map, floor, map};
use crate::rng::{PositionalRandomFactory, RandomSource};
pub use crate::rng::AnyPositionalFactory;

const CELL_WIDTH: i32 = 4;
const CELL_HEIGHT: i32 = 8;

/// Vanilla's own noise-settings derived cell size — its own quart-to-block
/// conversion applied to `size_horizontal` and `size_vertical`, i.e. `size * 4`.
///
/// **Not the same for every dimension.** The Overworld and the Nether are
/// `1, 2` → 4 wide / 8 tall (the [`CELL_WIDTH`]/[`CELL_HEIGHT`] this file's
/// Overworld path uses as constants); **the End is `2, 1` → 8 wide / 4 tall**.
/// Interpolation happens on cell corners, so a wrong cell size does not fail —
/// it produces smoothly wrong terrain.
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
    /// The default fluid (overworld: water).
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

// Grid constant (Aquifer.NoiseBasedAquifer): Y_SPACING.
const Y_SPACING: i32 = 12;

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
    /// deep-copying five `Density` trees (diagnostic D3). `Density::compute`
    /// reaches through the `Deref`, so every use site is unchanged.
    barrier: Arc<Density>,
    floodedness: Arc<Density>,
    spread: Arc<Density>,
    lava: Arc<Density>,
    prelim: Arc<Density>,

    positional: AnyPositionalFactory,
    sea_level: i32,
    /// Vanilla's own default-fluid query — the fluid the *global* picker's sea status
    /// carries.
    ///
    /// Was hardcoded to water, which is right for the Overworld and wrong for
    /// the Nether: `noise_settings/nether.json` says `minecraft:lava` with
    /// `sea_level: 32`, so the whole "lava sea" comes from here rather than from
    /// any aquifer behaviour. The `-54` deep-lava status below is a *separate*
    /// Overworld status and is unreachable in the Nether (`min(-54, 32) = -54`
    /// against a `min_y 0` dimension).
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
    prelim_cache: RefCell<HashMap<(i32, i32), i32>>,
}

impl AquiferSystem {
    /// Builds the aquifer + fill for chunk `(chunk_x, chunk_z)` from a
    /// `noise_settings` JSON value, using `builder` (seeded with the same seed as
    /// `RandomState`) to instantiate the router functions and the aquifer RNG.
    #[must_use]
    pub fn new(settings: &Value, builder: &Builder, chunk_x: i32, chunk_z: i32) -> Self {
        let router = &settings["noise_router"];
        let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(-64) as i32;
        let height = settings["noise"]["height"].as_i64().unwrap_or(384) as i32;
        let sea_level = settings["sea_level"].as_i64().unwrap_or(63) as i32;

        // Vanilla's own per-chunk field constructor. A dimension with `aquifers_enabled: false`
        // gets a *different implementation*, not a tuned one, and none of the
        // four aquifer noise fields is instantiated for it — so this branch is
        // taken before any of the `builder.build` calls below.
        if !settings["aquifers_enabled"].as_bool().unwrap_or(true) {
            let (cell_width, cell_height) = cell_geometry(settings);
            let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(-64) as i32;
            let height = settings["noise"]["height"].as_i64().unwrap_or(384) as i32;
            return Self::disabled(
                Program::compile(&builder.build(&router["final_density"])),
                builder.slot_count(),
                sea_level,
                fluid_from_settings(settings),
                min_y,
                height,
                chunk_x,
                chunk_z,
                cell_width,
                cell_height,
            );
        }

        let final_density_node = Program::compile(&builder.build(&router["final_density"]));
        let erosion_node = Program::compile(&builder.build(&router["erosion"]));
        let depth_node = Program::compile(&builder.build(&router["depth"]));
        let barrier = Arc::new(builder.build(&router["barrier"]));
        let floodedness = Arc::new(builder.build(&router["fluid_level_floodedness"]));
        let spread = Arc::new(builder.build(&router["fluid_level_spread"]));
        let lava = Arc::new(builder.build(&router["lava"]));
        let prelim = Arc::new(builder.build(&router["preliminary_surface_level"]));

        // vanilla's own aquifer-random query = random.fromHashOf("minecraft:aquifer").forkPositional().
        let mut aquifer_src = builder
            .positional_factory()
            .from_hash_of("minecraft:aquifer");
        let positional = aquifer_src.fork_positional();

        let slots = builder.slot_count();

        Self::from_parts(
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
        let final_density = NoiseChunkSampler::from_program(
            final_density_node,
            slots,
            CELL_WIDTH,
            CELL_HEIGHT,
            Some(Bounds {
                x: (min_block_x, max_block_x),
                y: (min_y, min_y + height - 1),
                z: (min_block_z, max_block_z),
            }),
        );
        let erosion =
            NoiseChunkSampler::from_program(erosion_node, slots, CELL_WIDTH, CELL_HEIGHT, None);
        let depth =
            NoiseChunkSampler::from_program(depth_node, slots, CELL_WIDTH, CELL_HEIGHT, None);

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

        let mut system = Self {
            final_density,
            erosion,
            depth,
            barrier,
            floodedness,
            spread,
            lava,
            prelim,
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
            aquifer_cache: RefCell::new(vec![None; total]),
            location_cache: RefCell::new(vec![None; total]),
            prelim_cache: RefCell::new(HashMap::new()),
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
        let final_density = NoiseChunkSampler::from_program(
            final_density_node,
            slots,
            cell_width,
            cell_height,
            Some(Bounds {
                x: (min_block_x, min_block_x + 15),
                y: (min_y, min_y + height - 1),
                z: (min_block_z, min_block_z + 15),
            }),
        );
        let stub = || Arc::new(Density::Const(0.0));
        let stub_sampler =
            || NoiseChunkSampler::new(Density::Const(0.0), 0, cell_width, cell_height);
        Self {
            final_density,
            erosion: stub_sampler(),
            depth: stub_sampler(),
            barrier: stub(),
            floodedness: stub(),
            spread: stub(),
            lava: stub(),
            prelim: stub(),
            // Vanilla's own disabled-aquifer constructor takes no
            // `PositionalRandomFactory` at all; this
            // is the cheapest inert stand-in and is never sampled.
            positional: crate::rng::Algorithm::Legacy.root_positional(0),
            sea_level,
            default_fluid: match default_fluid {
                BlockKind::Lava => Fluid::Lava,
                BlockKind::Water => Fluid::Water,
                // The End's `default_fluid` really is air, so this arm is not a
                // degenerate case — see `fluid_from_settings`.
                BlockKind::Air => Fluid::Air,
                // A silent fall-through to water would produce a plausible world.
                BlockKind::Stone => panic!("default_fluid is not a fluid: Stone"),
            },
            enabled: false,
            min_grid_x: 0,
            min_grid_y: 0,
            min_grid_z: 0,
            grid_size_x: 0,
            grid_size_z: 0,
            skip_sampling_above_y: i32::MAX,
            aquifer_cache: RefCell::new(Vec::new()),
            location_cache: RefCell::new(Vec::new()),
            prelim_cache: RefCell::new(HashMap::new()),
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
        crate::counters::bump_block_at();
        let density = self.final_density.final_density(x, y, z) + beard;
        match self.compute_substance(x, y, z, density) {
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
        if let Some(v) = self.prelim_cache.borrow().get(&(qx, qz)) {
            return *v;
        }
        let v = floor(self.prelim.compute(DfContext::new(qx, 0, qz)));
        self.prelim_cache.borrow_mut().insert((qx, qz), v);
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
        let mut block_z = min_block_z;
        while block_z <= max_block_z {
            let mut block_x = min_block_x;
            while block_x <= max_block_x {
                let surface_level = self.preliminary_surface_level(block_x, block_z);
                if surface_level > max_y {
                    max_y = surface_level;
                }
                block_x += 4;
            }
            block_z += 4;
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

    fn aquifer_status(&self, index: usize) -> FluidStatus {
        if let Some(status) = self.aquifer_cache.borrow()[index] {
            return status;
        }
        let (x, y, z) = self.location_cache.borrow()[index].expect("location computed first");
        let status = self.compute_aquifer_fluid(x, y, z);
        self.aquifer_cache.borrow_mut()[index] = Some(status);
        status
    }

    #[allow(clippy::too_many_lines)]
    fn compute_substance(&self, pos_x: i32, pos_y: i32, pos_z: i32, density: f64) -> Option<Fluid> {
        if density > 0.0 {
            return None;
        }

        let global_fluid = self.global_fluid(pos_y);
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
                    let (lx, ly, lz) =
                        self.location(spaced_grid_x, spaced_grid_y, spaced_grid_z, index);
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

        let closest_status1 = self.aquifer_status(closest_index1);
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
        let closest_status2 = self.aquifer_status(closest_index2);
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

        let closest_status3 = self.aquifer_status(closest_index3);
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
