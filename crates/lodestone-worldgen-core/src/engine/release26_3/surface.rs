//! Surface building: turns a terrain-shape fill into final blocks by running a
//! chunk's columns through the material rules, then the biome-specific
//! extensions (eroded-badlands pillars, frozen-ocean icebergs).
//!
//! Order matters throughout: the preliminary-surface and ore-vein density
//! functions share the sampling context with the fill, so the sequence of
//! sampling calls here is part of the result.

use super::aquifer::map;
use super::biome::{self, BiomeId, BiomeTable};
use super::material::{Anchor, Cond, GenContext, MaterialSystem, OreVein, Rule, StateId};
use super::sampler::{Ctx, Program, SId};
use super::settings::{ChunkFill, Substance, TerrainGenerator};
use super::tree::java_round;
use super::volume::Volume;
use crate::rng::{PositionalRandomFactory, RandomSource};

const WAY_BELOW_MIN_Y: i32 = -32512;

/// A chunk's blocks and bookkeeping after surface building.
#[derive(Clone, Debug)]
pub struct SurfaceChunk {
    pub min_y: i32,
    pub height: i32,
    /// `y + (x + z * 16) * height`, `y` relative to `min_y`.
    pub states: Vec<StateId>,
    /// Per column (`x + z * 16`): the lowest air Y above the highest non-air block.
    first_available: [i32; 256],
    /// Per section (bottom up): packed in-section offsets, in marking order.
    pub post_processing: Vec<Vec<u16>>,
}

impl SurfaceChunk {
    fn new(min_y: i32, height: i32, air: StateId) -> Self {
        Self {
            min_y,
            height,
            states: vec![air; (16 * 16 * height) as usize],
            first_available: [min_y; 256],
            post_processing: vec![Vec::new(); ((height + 15) / 16) as usize],
        }
    }

    fn idx(&self, x: i32, y: i32, z: i32) -> usize {
        ((y - self.min_y) + (x + z * 16) * self.height) as usize
    }

    pub fn max_y(&self) -> i32 {
        self.min_y + self.height - 1
    }

    /// The block at a world Y of column `(x, z)`; outside the chunk's height range
    /// this is `None` (void air).
    pub fn get(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        (y >= self.min_y && y <= self.max_y()).then(|| self.states[self.idx(x, y, z)])
    }

    /// The highest non-air Y of a column, or `min_y - 1` when it holds none.
    pub fn surface_height(&self, x: i32, z: i32) -> i32 {
        self.first_available[(x + z * 16) as usize] - 1
    }

    fn mark_post_process(&mut self, x: i32, y: i32, z: i32) {
        let section = ((y - self.min_y) >> 4) as usize;
        self.post_processing[section].push(((x & 15) | ((y & 15) << 4) | ((z & 15) << 8)) as u16);
    }

    fn update_heightmap(&mut self, sys: &MaterialSystem, x: i32, y: i32, z: i32, state: StateId) {
        let col = (x + z * 16) as usize;
        let first = self.first_available[col];
        if y <= first - 2 {
            return;
        }
        if !sys.states.is_air(state) {
            if y >= first {
                self.first_available[col] = y + 1;
            }
        } else if first - 1 == y {
            for yy in (self.min_y..y).rev() {
                if !sys.states.is_air(self.states[self.idx(x, yy, z)]) {
                    self.first_available[col] = yy + 1;
                    return;
                }
            }
            self.first_available[col] = self.min_y;
        }
    }

    /// A plain block write: the heightmap follows, nothing is queued.
    pub(crate) fn set_block_unmarked(&mut self, sys: &MaterialSystem, x: i32, y: i32, z: i32, state: StateId) {
        if y < self.min_y || y > self.max_y() {
            return;
        }
        let i = self.idx(x, y, z);
        self.states[i] = state;
        self.update_heightmap(sys, x, y, z, state);
    }

    pub(crate) fn mark(&mut self, x: i32, y: i32, z: i32) {
        self.mark_post_process(x, y, z);
    }

    /// A surface-rule write: fluid states are also queued for post-processing.
    fn set_block(&mut self, sys: &MaterialSystem, x: i32, y: i32, z: i32, state: StateId) {
        if y < self.min_y || y > self.max_y() {
            return;
        }
        self.set_block_unmarked(sys, x, y, z, state);
        if sys.states.has_fluid(state) {
            self.mark_post_process(x, y, z);
        }
    }
}

/// The dynamic state a rule evaluation reads.
struct Eval<'a> {
    sys: &'a MaterialSystem,
    program: &'a Program,
    ctx: &'a mut Ctx,
    biomes: &'a BiomeTable,
    biome_at: &'a mut dyn FnMut(i32, i32, i32) -> BiomeId,
    range: GenContext,
    narrowed: Volume,
    prelim_volume: Volume,
    prelim_buffer: Option<Vec<f32>>,
    ore: Vec<OreBuffers>,
    xz_stamp: u64,
    y_stamp: u64,
    x: i32,
    y: i32,
    z: i32,
    gradient_x: i32,
    gradient_z: i32,
    surface_depth: i32,
    secondary: (u64, f64),
    min_surface: (u64, i32),
    biome: Option<BiomeId>,
    water_height: i32,
    stone_below: i32,
    stone_above: i32,
    noise_memo: Vec<(u64, f64)>,
}

struct OreBuffers {
    density: Vec<f32>,
    richness: Vec<f32>,
}

impl Eval<'_> {
    fn update_xz(&mut self, x: i32, z: i32, gx: i32, gz: i32) {
        self.xz_stamp += 1;
        self.y_stamp += 1;
        self.x = x;
        self.z = z;
        self.gradient_x = gx;
        self.gradient_z = gz;
        self.surface_depth = self.surface_depth_at(x, z);
    }

    fn update_y(&mut self, above: i32, below: i32, water: i32, y: i32) {
        self.y_stamp += 1;
        self.biome = None;
        self.y = y;
        self.water_height = water;
        self.stone_below = below;
        self.stone_above = above;
    }

    fn surface_depth_at(&self, x: i32, z: i32) -> i32 {
        let n = f64::from(self.program.noises[self.sys.noises.surface].get(f64::from(x), 0.0, f64::from(z)));
        (n * 2.75 + 3.0 + self.sys.noise_random.at(x, 0, z).next_double() * 0.25) as i32
    }

    fn surface_secondary(&mut self) -> f64 {
        if self.secondary.0 != self.xz_stamp {
            let v = f64::from(self.program.noises[self.sys.noises.surface_secondary].get(f64::from(self.x), 0.0, f64::from(self.z)));
            self.secondary = (self.xz_stamp, v);
        }
        self.secondary.1
    }

    fn min_surface_level(&mut self) -> i32 {
        if self.min_surface.0 != self.xz_stamp {
            let level = match self.prelim_volume.index_of_block(self.x, 0, self.z) {
                Some(i) => {
                    if self.prelim_buffer.is_none() {
                        let mut buf = vec![0.0f32; self.prelim_volume.len()];
                        self.program.volume(self.ctx, self.sys.preliminary_surface, &mut buf, &self.prelim_volume);
                        self.prelim_buffer = Some(buf);
                    }
                    self.prelim_buffer.as_ref().expect("filled above")[i]
                }
                None => self.program.value(self.ctx, self.sys.preliminary_surface, self.x, 0, self.z),
            };
            self.min_surface = (self.xz_stamp, level.floor() as i32 + self.surface_depth - 8);
        }
        self.min_surface.1
    }

    fn biome(&mut self) -> BiomeId {
        if let Some(b) = self.biome {
            return b;
        }
        let b = (self.biome_at)(self.x, self.y, self.z);
        self.biome = Some(b);
        b
    }

    fn noise_value(&mut self, slot: usize, noise: usize, is_3d: bool) -> f64 {
        let stamp = if is_3d { self.y_stamp } else { self.xz_stamp };
        if self.noise_memo[slot].0 != stamp {
            let n = &self.program.noises[self.sys.noise_map[noise]];
            let v = if is_3d {
                n.get(f64::from(self.x), f64::from(self.y), f64::from(self.z))
            } else {
                n.get(f64::from(self.x), 0.0, f64::from(self.z))
            };
            self.noise_memo[slot] = (stamp, f64::from(v));
        }
        self.noise_memo[slot].1
    }

    fn band(&self, x: i32, y: i32, z: i32) -> StateId {
        let offset = java_round(self.program.noises[self.sys.noises.clay_bands_offset].get(f64::from(x), 0.0, f64::from(z)) * 4.0f32);
        let len = self.sys.clay_bands.len() as i32;
        self.sys.clay_bands[((y + offset + len) % len) as usize]
    }

    fn anchor(&self, a: Anchor) -> i32 {
        a.resolve(self.range)
    }

    fn test(&mut self, c: &Cond) -> bool {
        match c {
            Cond::Biome(ids) => {
                let b = self.biome();
                ids.contains(&b)
            }
            Cond::NoiseThreshold { noise, slot, min, max, is_3d } => {
                let v = self.noise_value(*slot, *noise, *is_3d);
                v >= *min && v <= *max
            }
            Cond::VerticalGradient { random, true_at_and_below, false_at_and_above } => {
                let (t, f) = (self.anchor(*true_at_and_below), self.anchor(*false_at_and_above));
                if self.y <= t {
                    return true;
                }
                if self.y >= f {
                    return false;
                }
                let probability = map(f64::from(self.y), f64::from(t), f64::from(f), 1.0, 0.0);
                let mut r = self.sys.randoms[*random].at(self.x, self.y, self.z);
                f64::from(r.next_float()) < probability
            }
            Cond::YAbove { anchor, surface_depth_multiplier, add_stone_depth } => {
                self.y + if *add_stone_depth { self.stone_above } else { 0 }
                    >= self.anchor(*anchor) + self.surface_depth * surface_depth_multiplier
            }
            Cond::Water { offset, surface_depth_multiplier, add_stone_depth } => {
                self.water_height == i32::MIN
                    || self.y + if *add_stone_depth { self.stone_above } else { 0 }
                        >= self.water_height + offset + self.surface_depth * surface_depth_multiplier
            }
            Cond::Temperature => {
                let b = self.biome();
                biome::cold_enough_to_snow(self.biomes.info(b), self.x, self.y, self.z, self.sys.sea_level)
            }
            Cond::Steep => self.gradient_x <= -4 || self.gradient_z >= 4,
            Cond::Not(inner) => !self.test(inner),
            Cond::Hole => self.surface_depth <= 0,
            Cond::AbovePreliminarySurface => self.y >= self.min_surface_level(),
            Cond::StoneDepth { offset, add_surface_depth, secondary_depth_range, ceiling } => {
                let stone_depth = if *ceiling { self.stone_below } else { self.stone_above };
                let surface_depth = if *add_surface_depth { self.surface_depth } else { 0 };
                let secondary = if *secondary_depth_range == 0 {
                    0
                } else {
                    map(self.surface_secondary(), -1.0, 1.0, 0.0, f64::from(*secondary_depth_range)) as i32
                };
                stone_depth <= 1 + offset + surface_depth + secondary
            }
        }
    }

    fn density_at(&mut self, id: SId, buffer: Option<usize>, which: Which) -> f32 {
        let (x, y, z) = (self.x, self.y, self.z);
        if let Some(ore) = buffer {
            if let Some(i) = self.narrowed.index_of_block(x, y, z) {
                return match which {
                    Which::Density => self.ore[ore].density[i],
                    Which::Richness => self.ore[ore].richness[i],
                };
            }
        }
        self.program.value(self.ctx, id, x, y, z)
    }

    fn apply(&mut self, rule: &Rule) -> Option<StateId> {
        match rule {
            Rule::Block(s) => Some(*s),
            Rule::Bandlands => Some(self.band(self.x, self.y, self.z)),
            Rule::Sequence(rules) => rules.iter().find_map(|r| self.apply(r)),
            Rule::Condition(c, then) => {
                if self.test(c) {
                    self.apply(then)
                } else {
                    None
                }
            }
            Rule::OreVein(v) => self.ore_vein(v),
        }
    }

    fn ore_vein(&mut self, v: &OreVein) -> Option<StateId> {
        let [density, richness, filler_gap] = self.sys.ore_functions[v.functions];
        let d = self.density_at(density, Some(v.functions), Which::Density);
        if d <= 0.0 {
            return None;
        }
        let mut random = self.sys.ore_random.at(self.x, self.y, self.z);
        if random.next_float() > d {
            return None;
        }
        let r = self.density_at(richness, Some(v.functions), Which::Richness);
        if random.next_float() < r && self.density_at(filler_gap, None, Which::Density) < 0.0 {
            Some(if random.next_float() < v.raw_ore_chance { v.raw_ore } else { v.ore })
        } else {
            Some(v.filler)
        }
    }

    /// Prefills ore density and richness volumes in rule order, as compiling the rule does.
    fn prepare(&mut self, rule: &Rule) {
        match rule {
            Rule::Sequence(rules) => rules.iter().for_each(|r| self.prepare(r)),
            Rule::Condition(_, then) => self.prepare(then),
            Rule::OreVein(v) => {
                let [density, richness, _] = self.sys.ore_functions[v.functions];
                let mut d = vec![0.0f32; self.narrowed.len()];
                self.program.volume(self.ctx, density, &mut d, &self.narrowed);
                let mut r = vec![0.0f32; self.narrowed.len()];
                self.program.volume(self.ctx, richness, &mut r, &self.narrowed);
                if self.ore.len() <= v.functions {
                    self.ore.resize_with(v.functions + 1, || OreBuffers { density: Vec::new(), richness: Vec::new() });
                }
                self.ore[v.functions] = OreBuffers { density: d, richness: r };
            }
            Rule::Block(_) | Rule::Bandlands => {}
        }
    }
}

#[derive(Clone, Copy)]
enum Which {
    Density,
    Richness,
}

impl TerrainGenerator {
    fn new_eval<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        biome_at: &'a mut dyn FnMut(i32, i32, i32) -> BiomeId,
        range: GenContext,
        narrowed: Volume,
        prelim_volume: Volume,
    ) -> Eval<'a> {
        let sys = self.material.as_ref().expect("material rules are loaded");
        Eval {
            sys,
            program: &self.program,
            ctx,
            biomes: &self.biomes,
            biome_at,
            range,
            narrowed,
            prelim_volume,
            prelim_buffer: None,
            ore: Vec::new(),
            xz_stamp: 0,
            y_stamp: 0,
            x: 0,
            y: 0,
            z: 0,
            gradient_x: 0,
            gradient_z: 0,
            surface_depth: 0,
            secondary: (0, 0.0),
            min_surface: (0, 0),
            biome: None,
            water_height: 0,
            stone_below: 0,
            stone_above: 0,
            noise_memo: vec![(0, 0.0); sys.noise_slots.len()],
        }
    }

    /// The block the surface rules put at one position, with one stone block
    /// above and below it, used to re-dress dirt left bare under a carved-out
    /// grass block. `chunk` supplies the surface gradient; `x`/`z` are in-chunk.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn top_material(
        &self,
        chunk: &SurfaceChunk,
        ctx: &mut Ctx,
        biome_at: &mut dyn FnMut(i32, i32, i32) -> BiomeId,
        range: GenContext,
        world: [i32; 3],
        local: [i32; 2],
        under_fluid: bool,
    ) -> Option<StateId> {
        let sys = self.material.as_ref().expect("material rules are loaded");
        let narrowed = Volume::new([1, 1, 1], world, [1, 1, 1]);
        let prelim = Volume::new([1, 1, 1], [world[0], 0, world[2]], [1, 1, 1]);
        let mut e = self.new_eval(ctx, biome_at, range, narrowed, prelim);
        e.prepare(&sys.rule);
        let (x, z) = (local[0], local[1]);
        let gx = chunk.surface_height((x + 1).min(15), z) - chunk.surface_height((x - 1).max(0), z);
        let gz = chunk.surface_height(x, (z + 1).min(15)) - chunk.surface_height(x, (z - 1).max(0));
        e.update_xz(world[0], world[2], gx, gz);
        e.update_y(1, 1, if under_fluid { world[1] + 1 } else { i32::MIN }, world[1]);
        e.apply(&sys.rule)
    }

    /// Writes the fill into a block grid the way the generator's fill step does:
    /// non-air cells only, with the surface heightmap and fluid post-processing.
    pub fn place_fill(&self, fill: &ChunkFill, chunk_min_y: i32, chunk_height: i32) -> SurfaceChunk {
        let sys = self.material.as_ref().expect("material rules are loaded");
        let mut chunk = SurfaceChunk::new(chunk_min_y, chunk_height, sys.air);
        let v = &fill.volume;
        let mut cursor = 0usize;
        for z in 0..v.size[2] {
            for x in 0..v.size[0] {
                for y in (0..v.size[1]).rev() {
                    let idx = v.index(x, y, z);
                    let scheduled = fill.fluid_updates.get(cursor) == Some(&(idx as u32));
                    if scheduled {
                        cursor += 1;
                    }
                    let state = match fill.substance[idx] {
                        Substance::Default => sys.default_block,
                        Substance::Fluid(super::aquifer::Fluid::Air) => sys.air,
                        Substance::Fluid(super::aquifer::Fluid::Water) => sys.water,
                        Substance::Fluid(super::aquifer::Fluid::Lava) => sys.lava,
                    };
                    if sys.states.is_air(state) {
                        continue;
                    }
                    let by = v.block_y(y);
                    let i = chunk.idx(x, by, z);
                    chunk.states[i] = state;
                    chunk.update_heightmap(sys, x, by, z, state);
                    if scheduled && sys.states.has_fluid(state) {
                        chunk.mark_post_process(x, by, z);
                    }
                }
            }
        }
        chunk
    }

    /// Runs the surface rules over a filled chunk.
    ///
    /// `chunk_min_y`/`chunk_height` are the dimension's range (which can exceed the
    /// noise range) and `biome_source` the quart-resolution biome lookup, which is
    /// zoomed with this generator's own world seed. `ctx` must be the context
    /// the fill used.
    #[allow(clippy::too_many_arguments)]
    pub fn build_surface(
        &self,
        fill: &ChunkFill,
        chunk_x: i32,
        chunk_z: i32,
        chunk_min_y: i32,
        chunk_height: i32,
        biome_source: &mut dyn FnMut(i32, i32, i32) -> BiomeId,
        ctx: &mut Ctx,
    ) -> SurfaceChunk {
        let sys = self.material.as_ref().expect("material rules are loaded");
        let mut chunk = self.place_fill(fill, chunk_min_y, chunk_height);
        let (min_x, min_z) = (chunk_x * 16, chunk_z * 16);
        let range = GenContext {
            min_y: chunk_min_y.max(self.min_y),
            height: chunk_height.min(self.height),
            sea_level: sys.sea_level,
        };
        // The narrowed volume covers up to the top of the highest section holding a block.
        let highest_section = (0..chunk.post_processing.len() as i32)
            .rev()
            .find(|&s| {
                let base = chunk_min_y + s * 16;
                (0..16).any(|dy| (0..256).any(|c| !sys.states.is_air(chunk.states[((base + dy - chunk_min_y) + c * chunk_height) as usize])))
            });
        let max_block_y = match highest_section {
            Some(s) => chunk_min_y + s * 16 + 15,
            // The reference's index -1 maps to the section below the lowest.
            None => chunk_min_y - 16 + 15,
        };
        let narrowed = Volume::new(
            [16, (max_block_y - fill.volume.min[1] + 1).max(1), 16],
            [min_x, fill.volume.min[1], min_z],
            [1, 1, 1],
        );
        let mut zoomed = |x: i32, y: i32, z: i32| biome::zoomed_biome(self.zoom_seed, x, y, z, &mut *biome_source);
        let prelim = Volume::new([16, 1, 16], [min_x, 0, min_z], [1, 1, 1]);
        let mut e = self.new_eval(ctx, &mut zoomed, range, narrowed, prelim);
        e.prepare(&sys.rule);

        let min_y = chunk_min_y;
        let max_y = chunk.max_y();
        for x in 0..16 {
            for z in 0..16 {
                let (bx, bz) = (min_x + x, min_z + z);
                let starting_height = chunk.surface_height(x, z) + 1;
                let surface_biome = (e.biome_at)(bx, starting_height, bz);
                if Some(surface_biome) == sys.eroded_badlands {
                    self.eroded_badlands_extension(&mut e, &mut chunk, x, z, bx, bz, starting_height);
                }
                let height = chunk.surface_height(x, z) + 1;
                let gx = chunk.surface_height((x + 1).min(15), z) - chunk.surface_height((x - 1).max(0), z);
                let gz = chunk.surface_height(x, (z + 1).min(15)) - chunk.surface_height(x, (z - 1).max(0));
                e.update_xz(bx, bz, gx, gz);
                let mut stone_above_depth = 0;
                let mut water_height = i32::MIN;
                let mut next_ceiling_stone_y = i32::MAX;
                let end_y = min_y;
                let mut y = height;
                while y >= end_y {
                    let old = chunk.get(x, y, z);
                    let is_air = old.is_none_or(|s| sys.states.is_air(s));
                    if is_air {
                        stone_above_depth = 0;
                        water_height = i32::MIN;
                    } else if old.is_some_and(|s| sys.states.has_fluid(s)) {
                        if water_height == i32::MIN {
                            water_height = y + 1;
                        }
                    } else {
                        if next_ceiling_stone_y >= y {
                            next_ceiling_stone_y = WAY_BELOW_MIN_Y;
                            let mut look = y - 1;
                            while look >= end_y - 1 {
                                let next = chunk.get(x, look, z);
                                let stone = next.is_some_and(|s| !sys.states.is_air(s) && !sys.states.has_fluid(s));
                                if !stone {
                                    next_ceiling_stone_y = look + 1;
                                    break;
                                }
                                look -= 1;
                            }
                        }
                        stone_above_depth += 1;
                        let stone_below_depth = y - next_ceiling_stone_y + 1;
                        e.update_y(stone_above_depth, stone_below_depth, water_height, y);
                        if y >= min_y && y <= max_y {
                            if let Some(state) = e.apply(&sys.rule) {
                                chunk.set_block(sys, x, y, z, state);
                            }
                        }
                    }
                    y -= 1;
                }
                if Some(surface_biome) == sys.frozen_ocean || Some(surface_biome) == sys.deep_frozen_ocean {
                    let min_surface = e.min_surface_level();
                    self.frozen_ocean_extension(&mut e, &mut chunk, min_surface, surface_biome, x, z, bx, bz, starting_height);
                }
            }
        }
        chunk
    }

    #[allow(clippy::too_many_arguments)]
    fn eroded_badlands_extension(&self, e: &mut Eval<'_>, chunk: &mut SurfaceChunk, x: i32, z: i32, bx: i32, bz: i32, height: i32) {
        let sys = self.material.as_ref().expect("material rules are loaded");
        let n = &e.program.noises;
        let (fx, fz) = (f64::from(bx), f64::from(bz));
        let pillar_buffer = (f64::from(n[sys.noises.badlands_surface].get(fx, 0.0, fz)) * 8.25)
            .abs()
            .min(f64::from(n[sys.noises.badlands_pillar].get(fx * 0.2, 0.0, fz * 0.2) * 15.0f32));
        if pillar_buffer <= 0.0 {
            return;
        }
        let pillar_floor = (f64::from(n[sys.noises.badlands_pillar_roof].get(fx * 0.75, 0.0, fz * 0.75)) * 1.5).abs();
        let extension_top = 64.0 + (pillar_buffer * pillar_buffer * 2.5).min((pillar_floor * 50.0).ceil() + 24.0);
        let start_y = extension_top.floor() as i32;
        if height <= start_y {
            let mut y = start_y;
            while y >= chunk.min_y {
                let old = chunk.get(x, y, z);
                if old.is_some_and(|s| sys.states.same_block(s, sys.default_block)) {
                    break;
                }
                if old.is_some_and(|s| sys.states.is_block(s, "minecraft:water")) {
                    return;
                }
                y -= 1;
            }
            let mut y = start_y;
            while y >= chunk.min_y && chunk.get(x, y, z).is_none_or(|s| sys.states.is_air(s)) {
                chunk.set_block(sys, x, y, z, sys.default_block);
                y -= 1;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn frozen_ocean_extension(
        &self,
        e: &mut Eval<'_>,
        chunk: &mut SurfaceChunk,
        min_surface_level: i32,
        surface_biome: BiomeId,
        x: i32,
        z: i32,
        bx: i32,
        bz: i32,
        height: i32,
    ) {
        let sys = self.material.as_ref().expect("material rules are loaded");
        let n = &e.program.noises;
        let (fx, fz) = (f64::from(bx), f64::from(bz));
        let iceberg = (f64::from(n[sys.noises.iceberg_surface].get(fx, 0.0, fz)) * 8.25)
            .abs()
            .min(f64::from(n[sys.noises.iceberg_pillar].get(fx * 1.28, 0.0, fz * 1.28) * 15.0f32));
        if iceberg <= 1.8 {
            return;
        }
        let roof = (f64::from(n[sys.noises.iceberg_pillar_roof].get(fx * 1.17, 0.0, fz * 1.17)) * 1.5).abs();
        let mut top = (iceberg * iceberg * 1.2).min((roof * 40.0).ceil() + 14.0);
        if biome::melts_frozen_ocean_iceberg_slightly(self.biomes.info(surface_biome), bx, sys.sea_level, bz, sys.sea_level) {
            top -= 2.0;
        }
        if top <= 2.0 {
            return;
        }
        let sea = f64::from(sys.sea_level);
        let extension_bottom = sea - top - 7.0;
        top += sea;
        let extension_top = top;
        let mut random = sys.noise_random.at(bx, 0, bz);
        let max_snow_depth = 2 + random.next_int_bounded(4);
        let min_snow_height = sys.sea_level + 18 + random.next_int_bounded(10);
        let mut snow_depth = 0;
        let mut y = height.max(extension_top as i32 + 1);
        while y >= min_surface_level {
            let here = chunk.get(x, y, z);
            let air = here.is_none_or(|s| sys.states.is_air(s));
            let water = here.is_some_and(|s| sys.states.is_block(s, "minecraft:water"));
            if (air && y < extension_top as i32 && random.next_double() > 0.01)
                || (water && y > extension_bottom as i32 && y < sys.sea_level && random.next_double() > 0.15)
            {
                if snow_depth <= max_snow_depth && y > min_snow_height {
                    chunk.set_block(sys, x, y, z, sys.snow_block);
                    snow_depth += 1;
                } else {
                    chunk.set_block(sys, x, y, z, sys.packed_ice);
                }
            }
            y -= 1;
        }
    }
}
