//! Dripstone-style features: `speleothem` (one spike), `speleothem_cluster` and
//! `large_dripstone`, parameterised by the base and pointed blocks (dripstone or sulfur).

use lodestone_worldgen_core::math;
use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use crate::blocks::{BlockId, Dir, FluidKind, State};
use crate::env::{Env, Heightmap};
use crate::json::{Res, float, float_or, get, int, int_or};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::provider::{FloatProvider, IntProvider, clamped_normal};
use crate::stateprovider::StateProvider;

/// A block set written as `#tag`, a list, or one name.
fn block_set(env: &Env, v: &Value, ctx: &str) -> Res<Vec<BlockId>> {
    if let Some(s) = v.as_str() {
        if let Some(tag) = s.strip_prefix('#') {
            let tag = tag.strip_prefix("minecraft:").unwrap_or(tag);
            return Ok(env.tags.ordered(tag).ok_or_else(|| format!("{ctx}: unknown tag {tag}"))?.to_vec());
        }
        let name = s.strip_prefix("minecraft:").unwrap_or(s);
        return Ok(vec![env.blocks.block_by_name(name).ok_or_else(|| format!("{ctx}: unknown block {s}"))?]);
    }
    let mut out = Vec::new();
    for n in v.as_array().ok_or_else(|| format!("{ctx}: block set"))? {
        out.extend(block_set(env, n, ctx)?);
    }
    Ok(out)
}

fn fixed_state(env: &Env, v: &Value, key: &str, ctx: &str) -> Res<State> {
    StateProvider::parse(env, get(v, key, ctx)?, ctx)?.constant().ok_or_else(|| format!("{ctx}: {key} must be fixed"))
}

fn is_block(env: &Env, s: State, name: &str) -> bool {
    env.blocks.block_of(s) == env.blocks.block_by_name(name).unwrap_or_else(|| panic!("block {name}"))
}

fn empty_or_water(env: &Env, s: State) -> bool {
    env.blocks.is_air(s) || is_block(env, s, "water")
}

fn empty_or_water_or_lava(env: &Env, s: State) -> bool {
    empty_or_water(env, s) || is_block(env, s, "lava")
}

fn neither_empty_nor_water(env: &Env, s: State) -> bool {
    !empty_or_water(env, s)
}

/// The shared base/pointed/replaceable triple.
#[derive(Clone, Debug)]
pub struct Material {
    pub base: State,
    pub pointed: State,
    pub replaceable: Vec<BlockId>,
}

impl Material {
    fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            base: fixed_state(env, v, "base_block", ctx)?,
            pointed: if v.get("pointed_block").is_some() { fixed_state(env, v, "pointed_block", ctx)? } else { 0 },
            replaceable: block_set(env, get(v, "replaceable_blocks", ctx)?, ctx)?,
        })
    }

    fn is_base(&self, env: &Env, s: State) -> bool {
        let b = env.blocks.block_of(s);
        b == env.blocks.block_of(self.base) || self.replaceable.contains(&b)
    }

    fn place_base_if_possible(&self, level: &mut Level<'_>, p: Pos) -> bool {
        let s = level.get(p.x, p.y, p.z);
        if self.replaceable.contains(&level.env.blocks.block_of(s)) {
            level.set(p.x, p.y, p.z, self.base);
            true
        } else {
            false
        }
    }

    fn pointed_state(&self, level: &Level<'_>, dir: Dir, thickness: &str) -> State {
        let b = &level.env.blocks;
        let s = b.with(self.pointed, "vertical_direction", dir.name()).expect("vertical_direction");
        b.with(s, "thickness", thickness).expect("thickness")
    }

    /// Writes the pointed blocks of one spike from its root outward.
    fn grow(&self, level: &mut Level<'_>, start: Pos, tip: Dir, height: i32, merged: bool) {
        let env = level.env;
        if !self.is_base(env, level.get_pos(start.relative(tip.opposite()))) {
            return;
        }
        let mut states: Vec<&str> = Vec::new();
        if height >= 3 {
            states.push("base");
            for _ in 0..height - 3 {
                states.push("middle");
            }
        }
        if height >= 2 {
            states.push("frustum");
        }
        if height >= 1 {
            states.push(if merged { "tip_merge" } else { "tip" });
        }
        let mut pos = start;
        for t in states {
            let mut s = self.pointed_state(level, tip, t);
            let wet = env.blocks.fluid(level.get_pos(pos)) == FluidKind::Water;
            s = env.blocks.with(s, "waterlogged", if wet { "true" } else { "false" }).expect("waterlogged");
            level.set(pos.x, pos.y, pos.z, s);
            pos = pos.relative(tip);
        }
    }
}

impl Level<'_> {
    /// The block at a position.
    fn get_pos(&self, p: Pos) -> State {
        self.get(p.x, p.y, p.z)
    }
}

/// A vertical run of empty cells (or water) and the solid edges that close it.
#[derive(Clone, Copy, Debug)]
struct Column {
    floor: Option<i32>,
    ceiling: Option<i32>,
}

impl Column {
    fn height(&self) -> Option<i32> {
        Some(self.ceiling? - self.floor? - 1)
    }

    fn scan(level: &Level<'_>, pos: Pos, range: i32, inside: &dyn Fn(State) -> bool, edge: &dyn Fn(State) -> bool) -> Option<Self> {
        if !inside(level.get_pos(pos)) {
            return None;
        }
        let dir = |d: i32| {
            let mut p = pos;
            let mut i = 1;
            while i < range && inside(level.get_pos(p)) {
                p = p.offset(0, d, 0);
                i += 1;
            }
            edge(level.get_pos(p)).then_some(p.y)
        };
        let ceiling = dir(1);
        let floor = dir(-1);
        Some(Self { floor, ceiling })
    }
}

#[derive(Clone, Debug)]
pub struct SpeleothemConfig {
    pub material: Material,
    pub taller: f32,
    pub directional: f32,
    pub radius2: f32,
    pub radius3: f32,
}

impl SpeleothemConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            material: Material::parse(env, v, ctx)?,
            taller: float_or(v, "chance_of_taller_generation", 0.2, ctx)?,
            directional: float_or(v, "chance_of_directional_spread", 0.7, ctx)?,
            radius2: float_or(v, "chance_of_spread_radius2", 0.5, ctx)?,
            radius3: float_or(v, "chance_of_spread_radius3", 0.5, ctx)?,
        })
    }
}

pub fn place_speleothem(cfg: &SpeleothemConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let m = &cfg.material;
    let above = m.is_base(env, level.get_pos(origin.above()));
    let below = m.is_base(env, level.get_pos(origin.below()));
    let tip = if above && below {
        if rng.next_bool() { Dir::Down } else { Dir::Up }
    } else if above {
        Dir::Down
    } else if below {
        Dir::Up
    } else {
        return false;
    };
    let root = origin.relative(tip.opposite());
    m.place_base_if_possible(level, root);
    for d in Dir::HORIZONTAL {
        if !(rng.next_float() > cfg.directional) {
            let p1 = root.relative(d);
            m.place_base_if_possible(level, p1);
            if !(rng.next_float() > cfg.radius2) {
                let p2 = p1.relative(Dir::ALL[rng.next_int_bounded(6) as usize]);
                m.place_base_if_possible(level, p2);
                if !(rng.next_float() > cfg.radius3) {
                    let p3 = p2.relative(Dir::ALL[rng.next_int_bounded(6) as usize]);
                    m.place_base_if_possible(level, p3);
                }
            }
        }
    }
    let height = if rng.next_float() < cfg.taller && empty_or_water(env, level.get_pos(origin.relative(tip))) { 2 } else { 1 };
    m.grow(level, origin, tip, height, false);
    true
}

#[derive(Clone, Debug)]
pub struct ClusterConfig {
    pub material: Material,
    pub search_range: i32,
    pub height: IntProvider,
    pub radius: IntProvider,
    pub max_diff: i32,
    pub deviation: i32,
    pub layer_thickness: IntProvider,
    pub density: FloatProvider,
    pub wetness: FloatProvider,
    pub chance_at_max: f32,
    pub max_edge: i32,
    pub max_center: i32,
}

impl ClusterConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            material: Material::parse(env, v, ctx)?,
            search_range: int(v, "floor_to_ceiling_search_range", ctx)?,
            height: IntProvider::parse(get(v, "height", ctx)?, ctx)?,
            radius: IntProvider::parse(get(v, "radius", ctx)?, ctx)?,
            max_diff: int(v, "max_stalagmite_stalactite_height_diff", ctx)?,
            deviation: int(v, "height_deviation", ctx)?,
            layer_thickness: IntProvider::parse(get(v, "speleothem_block_layer_thickness", ctx)?, ctx)?,
            density: FloatProvider::parse(get(v, "density", ctx)?, ctx)?,
            wetness: FloatProvider::parse(get(v, "wetness", ctx)?, ctx)?,
            chance_at_max: float(v, "chance_of_speleothem_at_max_distance_from_center", ctx)?,
            max_edge: int(v, "max_distance_from_edge_affecting_chance_of_speleothem", ctx)?,
            max_center: int(v, "max_distance_from_center_affecting_height_bias", ctx)?,
        })
    }

    fn chance(&self, xr: i32, zr: i32, dx: i32, dz: i32) -> f64 {
        let edge = (xr - dx.abs()).min(zr - dz.abs());
        math::clamped_map(f64::from(edge), 0.0, f64::from(self.max_edge), f64::from(self.chance_at_max), 1.0)
    }

    fn spike_height(&self, rng: &mut Rng, dx: i32, dz: i32, density: f32, max_height: i32) -> i32 {
        if rng.next_float() > density {
            return 0;
        }
        let dist = dx.abs() + dz.abs();
        let mean = math::clamped_map(f64::from(dist), 0.0, f64::from(self.max_center), f64::from(max_height) / 2.0, 0.0) as f32;
        clamped_normal(rng, mean, self.deviation as f32, 0.0, max_height as f32) as i32
    }

    fn can_adjacent_water(env: &Env, level: &Level<'_>, p: Pos) -> bool {
        let s = level.get_pos(p);
        env.tags.get("base_stone_overworld").expect("tag").contains(env.blocks.block_of(s)) || env.blocks.fluid(s) == FluidKind::Water
    }

    fn can_place_pool(&self, env: &Env, level: &Level<'_>, p: Pos) -> bool {
        let s = level.get_pos(p);
        let b = env.blocks.block_of(s);
        if is_block(env, s, "water") || b == env.blocks.block_of(self.material.base) || b == env.blocks.block_of(self.material.pointed) {
            return false;
        }
        if env.blocks.fluid(level.get_pos(p.above())) == FluidKind::Water {
            return false;
        }
        for d in Dir::HORIZONTAL {
            if !Self::can_adjacent_water(env, level, p.relative(d)) {
                return false;
            }
        }
        Self::can_adjacent_water(env, level, p.below())
    }

    fn replace_with_base(&self, level: &mut Level<'_>, first: Pos, max: i32, dir: Dir) {
        let mut p = first;
        for _ in 0..max {
            if !self.material.place_base_if_possible(level, p) {
                return;
            }
            p = p.relative(dir);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn place_column(&self, level: &mut Level<'_>, rng: &mut Rng, pos: Pos, dx: i32, dz: i32, chance_water: f32, chance_spike: f64, cluster_height: i32, density: f32) {
        let env = level.env;
        let inside = |s: State| empty_or_water(env, s);
        let edge = |s: State| neither_empty_nor_water(env, s);
        let Some(base) = Column::scan(level, pos, self.search_range, &inside, &edge) else { return };
        let (ceiling, base_floor) = (base.ceiling, base.floor);
        if ceiling.is_none() && base_floor.is_none() {
            return;
        }
        let want_pool = rng.next_float() < chance_water;
        let mut column = base;
        if want_pool {
            if let Some(f) = base_floor {
                if self.can_place_pool(env, level, pos.at_y(f)) {
                    column = Column { floor: Some(f - 1), ceiling: base.ceiling };
                    let water = env.blocks.default_state(env.blocks.block_by_name("water").expect("water"));
                    level.set(pos.x, f, pos.z, water);
                }
            }
        }
        let floor = column.floor;
        let is_lava = |level: &Level<'_>, p: Pos| is_block(env, level.get_pos(p), "lava");
        let want_stalactite = rng.next_double() < chance_spike;
        let stalactite_height = match ceiling {
            Some(c) if want_stalactite && !is_lava(level, pos.at_y(c)) => {
                let thickness = self.layer_thickness.sample(rng);
                self.replace_with_base(level, pos.at_y(c), thickness, Dir::Up);
                let max = floor.map_or(cluster_height, |f| cluster_height.min(c - f));
                self.spike_height(rng, dx, dz, density, max)
            }
            _ => 0,
        };
        let want_stalagmite = rng.next_double() < chance_spike;
        let stalagmite_height = match floor {
            Some(f) if want_stalagmite && !is_lava(level, pos.at_y(f)) => {
                let thickness = self.layer_thickness.sample(rng);
                self.replace_with_base(level, pos.at_y(f), thickness, Dir::Down);
                if ceiling.is_some() {
                    (stalactite_height + math::random_between_inclusive(rng, -self.max_diff, self.max_diff)).max(0)
                } else {
                    self.spike_height(rng, dx, dz, density, cluster_height)
                }
            }
            _ => 0,
        };
        let (actual_stalactite, actual_stalagmite) = match (ceiling, floor) {
            (Some(c), Some(f)) if c - stalactite_height <= f + stalagmite_height => {
                let lowest_bottom = (c - stalactite_height).max(f + 1);
                let highest_top = (f + stalagmite_height).min(c - 1);
                let bottom = math::random_between_inclusive(rng, lowest_bottom, highest_top + 1);
                (c - bottom, bottom - 1 - f)
            }
            _ => (stalactite_height, stalagmite_height),
        };
        let merge = rng.next_bool() && actual_stalactite > 0 && actual_stalagmite > 0 && column.height() == Some(actual_stalactite + actual_stalagmite);
        if let Some(c) = ceiling {
            self.material.grow(level, pos.at_y(c - 1), Dir::Down, actual_stalactite, merge);
        }
        if let Some(f) = floor {
            self.material.grow(level, pos.at_y(f + 1), Dir::Up, actual_stalagmite, merge);
        }
    }
}

pub fn place_cluster(cfg: &ClusterConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    if !empty_or_water(env, level.get_pos(origin)) {
        return false;
    }
    let height = cfg.height.sample(rng);
    let wetness = cfg.wetness.sample(rng);
    let density = cfg.density.sample(rng);
    let xr = cfg.radius.sample(rng);
    let zr = cfg.radius.sample(rng);
    for dx in -xr..=xr {
        for dz in -zr..=zr {
            let chance = cfg.chance(xr, zr, dx, dz);
            cfg.place_column(level, rng, origin.offset(dx, 0, dz), dx, dz, wetness, chance, height, density);
        }
    }
    true
}

#[derive(Clone, Debug)]
pub struct LargeConfig {
    pub replaceable: Vec<BlockId>,
    pub search_range: i32,
    pub column_radius: IntProvider,
    pub height_scale: FloatProvider,
    pub max_ratio: f32,
    pub stalactite_bluntness: FloatProvider,
    pub stalagmite_bluntness: FloatProvider,
    pub wind_speed: FloatProvider,
    pub min_radius_for_wind: i32,
    pub min_bluntness_for_wind: f32,
}

impl LargeConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            replaceable: block_set(env, get(v, "replaceable_blocks", ctx)?, ctx)?,
            search_range: int_or(v, "floor_to_ceiling_search_range", 30, ctx)?,
            column_radius: IntProvider::parse(get(v, "column_radius", ctx)?, ctx)?,
            height_scale: FloatProvider::parse(get(v, "height_scale", ctx)?, ctx)?,
            max_ratio: float(v, "max_column_radius_to_cave_height_ratio", ctx)?,
            stalactite_bluntness: FloatProvider::parse(get(v, "stalactite_bluntness", ctx)?, ctx)?,
            stalagmite_bluntness: FloatProvider::parse(get(v, "stalagmite_bluntness", ctx)?, ctx)?,
            wind_speed: FloatProvider::parse(get(v, "wind_speed", ctx)?, ctx)?,
            min_radius_for_wind: int(v, "min_radius_for_wind", ctx)?,
            min_bluntness_for_wind: float(v, "min_bluntness_for_wind", ctx)?,
        })
    }
}

fn spike_height(xz_distance: f64, radius: f64, scale: f64, bluntness: f64) -> f64 {
    let d = if xz_distance < bluntness { bluntness } else { xz_distance };
    let r = d / radius * 0.384;
    let part1 = 0.75 * r.powf(1.333_333_333_333_333_3);
    let part2 = r.powf(0.666_666_666_666_666_6);
    let part3 = 0.333_333_333_333_333_3 * r.ln();
    let h = (scale * (part1 - part2 - part3)).max(0.0);
    h / 0.384 * radius
}

fn mostly_embedded(level: &Level<'_>, center: Pos, radius: i32) -> bool {
    let env = level.env;
    if empty_or_water_or_lava(env, level.get_pos(center)) {
        return false;
    }
    let increment = 6.0f32 / radius as f32;
    let end = (std::f64::consts::PI * 2.0) as f32;
    let mut angle = 0.0f32;
    while angle < end {
        let dx = (math::cos(f64::from(angle)) * radius as f32) as i32;
        let dz = (math::sin(f64::from(angle)) * radius as f32) as i32;
        if empty_or_water_or_lava(env, level.get_pos(center.offset(dx, 0, dz))) {
            return false;
        }
        angle += increment;
    }
    true
}

struct Wind {
    origin_y: i32,
    speed: Option<(f64, f64)>,
    max_offset: i32,
}

impl Wind {
    fn offset(&self, p: Pos) -> Pos {
        let Some((sx, sz)) = self.speed else { return p };
        let dy = f64::from(self.origin_y - p.y);
        let dx = math::floor(sx * dy).clamp(-self.max_offset, self.max_offset);
        let dz = math::floor(sz * dy).clamp(-self.max_offset, self.max_offset);
        p.offset(dx, 0, dz)
    }
}

struct Large {
    root: Pos,
    up: bool,
    radius: i32,
    bluntness: f64,
    scale: f64,
}

impl Large {
    fn height_at(&self, r: f32) -> i32 {
        spike_height(f64::from(r), f64::from(self.radius), self.scale, self.bluntness) as i32
    }

    fn suitable_for_wind(&self, min_radius: i32, min_bluntness: f32) -> bool {
        self.radius >= min_radius && self.bluntness >= f64::from(min_bluntness)
    }

    fn move_back(&mut self, level: &Level<'_>, wind: &Wind) -> bool {
        let env = level.env;
        while self.radius > 1 {
            let mut new_root = self.root;
            let tries = 10.min(self.height_at(0.0));
            for _ in 0..tries {
                if is_block(env, level.get_pos(new_root), "lava") {
                    return false;
                }
                if mostly_embedded(level, wind.offset(new_root), self.radius) {
                    self.root = new_root;
                    return true;
                }
                new_root = new_root.offset(0, if self.up { -1 } else { 1 }, 0);
            }
            self.radius /= 2;
        }
        false
    }

    fn place(&self, level: &mut Level<'_>, rng: &mut Rng, wind: &Wind) {
        let env = level.env;
        let dripstone = env.blocks.default_state(env.blocks.block_by_name("dripstone_block").expect("dripstone_block"));
        let base_stone = env.tags.get("base_stone_overworld").expect("tag");
        for dx in -self.radius..=self.radius {
            for dz in -self.radius..=self.radius {
                let current = ((dx * dx + dz * dz) as f32).sqrt();
                if current > self.radius as f32 {
                    continue;
                }
                let mut height = self.height_at(current);
                if height > 0 {
                    if f64::from(rng.next_float()) < 0.2 {
                        height = (height as f32 * math::random_between(rng, 0.8, 1.0)) as i32;
                    }
                    let mut pos = self.root.offset(dx, 0, dz);
                    let mut out_of_stone = false;
                    let max_y = if self.up { level.height(Heightmap::WorldSurfaceWg, pos.x, pos.z) } else { i32::MAX };
                    let mut i = 0;
                    while i < height && pos.y < max_y {
                        let adj = wind.offset(pos);
                        let s = level.get_pos(adj);
                        if empty_or_water_or_lava(env, s) {
                            out_of_stone = true;
                            level.set(adj.x, adj.y, adj.z, dripstone);
                        } else if out_of_stone && base_stone.contains(env.blocks.block_of(s)) {
                            break;
                        }
                        pos = pos.offset(0, if self.up { 1 } else { -1 }, 0);
                        i += 1;
                    }
                }
            }
        }
    }
}

pub fn place_large(cfg: &LargeConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    if !empty_or_water(env, level.get_pos(origin)) {
        return false;
    }
    let dripstone = env.blocks.block_by_name("dripstone_block").expect("dripstone_block");
    let inside = |s: State| empty_or_water(env, s);
    let edge = |s: State| {
        let b = env.blocks.block_of(s);
        b == dripstone || cfg.replaceable.contains(&b) || is_block(env, s, "lava")
    };
    let Some(column) = Column::scan(level, origin, cfg.search_range, &inside, &edge) else { return false };
    let (Some(floor), Some(ceiling)) = (column.floor, column.ceiling) else { return false };
    let height = ceiling - floor - 1;
    if height < 4 {
        return false;
    }
    let max_by_height = (height as f32 * cfg.max_ratio) as i32;
    let (rmin, rmax) = (cfg.column_radius.min_inclusive(), cfg.column_radius.max_inclusive());
    let max_radius = max_by_height.clamp(rmin, rmax);
    let radius = math::random_between_inclusive(rng, rmin, max_radius);
    let make = |rng: &mut Rng, root: Pos, up: bool, blunt: &FloatProvider| {
        let bluntness = f64::from(blunt.sample(rng));
        let scale = f64::from(cfg.height_scale.sample(rng));
        Large { root, up, radius, bluntness, scale }
    };
    let mut stalactite = make(rng, origin.at_y(ceiling - 1), false, &cfg.stalactite_bluntness);
    let mut stalagmite = make(rng, origin.at_y(floor + 1), true, &cfg.stalagmite_bluntness);
    let wind = if stalactite.suitable_for_wind(cfg.min_radius_for_wind, cfg.min_bluntness_for_wind)
        && stalagmite.suitable_for_wind(cfg.min_radius_for_wind, cfg.min_bluntness_for_wind)
    {
        let speed = cfg.wind_speed.sample(rng);
        let direction = math::random_between(rng, 0.0, std::f32::consts::PI);
        let (c, s) = (math::cos(f64::from(direction)), math::sin(f64::from(direction)));
        Wind { origin_y: origin.y, speed: Some((f64::from(c * speed), f64::from(s * speed))), max_offset: 16 - radius }
    } else {
        Wind { origin_y: 0, speed: None, max_offset: 0 }
    };
    let a = stalactite.move_back(level, &wind);
    let b = stalagmite.move_back(level, &wind);
    if a {
        stalactite.place(level, rng, &wind);
    }
    if b {
        stalagmite.place(level, rng, &wind);
    }
    true
}
