//! `root_system`: a tree grown from a column of rooted dirt, with hanging roots under the
//! ceiling around it (azalea trees in lush caves, sulfur springs).

use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use crate::blocks::{BlockId, Dir, FluidKind, Support};
use crate::env::{Env, Heightmap};
use crate::json::{Res, get, int};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::predicate::BlockPred;
use crate::registry::{Loader, PlacedFeature};
use crate::stateprovider::StateProvider;
use crate::survive::can_survive;

#[derive(Clone, Debug)]
pub struct RootSystemConfig {
    pub tree: Arc<PlacedFeature>,
    pub vertical_space: i32,
    pub level_test_distance: i32,
    pub max_level_deviation: i32,
    pub root_radius: i32,
    pub root_replaceable: Vec<BlockId>,
    pub root_state: StateProvider,
    pub root_attempts: i32,
    pub root_column_max_height: i32,
    pub hanging_radius: i32,
    pub hanging_span: i32,
    pub hanging_state: StateProvider,
    pub hanging_attempts: i32,
    pub allowed_water: i32,
    pub allowed_position: BlockPred,
}

impl RootSystemConfig {
    pub fn parse(env: &Env, loader: &mut Loader<'_>, v: &Value, ctx: &str) -> Res<Self> {
        let tag = crate::json::string(v, "root_replaceable", ctx)?;
        let tag = tag.strip_prefix('#').ok_or_else(|| format!("{ctx}: root_replaceable must be a tag"))?;
        let tag = tag.strip_prefix("minecraft:").unwrap_or(tag);
        Ok(Self {
            tree: loader.placed_ref(env, get(v, "feature", ctx)?, ctx)?,
            vertical_space: int(v, "required_vertical_space_for_tree", ctx)?,
            level_test_distance: int(v, "level_test_distance", ctx)?,
            max_level_deviation: int(v, "max_level_deviation", ctx)?,
            root_radius: int(v, "root_radius", ctx)?,
            root_replaceable: env.tags.ordered(tag).ok_or_else(|| format!("{ctx}: unknown tag {tag}"))?.to_vec(),
            root_state: StateProvider::parse(env, get(v, "root_state_provider", ctx)?, ctx)?,
            root_attempts: int(v, "root_placement_attempts", ctx)?,
            root_column_max_height: int(v, "root_column_max_height", ctx)?,
            hanging_radius: int(v, "hanging_root_radius", ctx)?,
            hanging_span: int(v, "hanging_roots_vertical_span", ctx)?,
            hanging_state: StateProvider::parse(env, get(v, "hanging_root_state_provider", ctx)?, ctx)?,
            hanging_attempts: int(v, "hanging_root_placement_attempts", ctx)?,
            allowed_water: int(v, "allowed_vertical_water_for_tree", ctx)?,
            allowed_position: BlockPred::parse(env, get(v, "allowed_tree_position", ctx)?, ctx)?,
        })
    }

    fn space_for_tree(&self, level: &Level<'_>, pos: Pos) -> bool {
        let blocks = &level.env.blocks;
        let mut p = pos;
        for i in 1..=self.vertical_space {
            p = p.above();
            let s = level.get(p.x, p.y, p.z);
            let allowed = blocks.is_air(s) || (i + 1 <= self.allowed_water && blocks.fluid(s) == FluidKind::Water);
            if !allowed {
                return false;
            }
        }
        if self.level_test_distance > 0 {
            // The 2D data values run south, west, north, east.
            for d in [Dir::South, Dir::West, Dir::North, Dir::East] {
                let corner = pos.relative_n(d, self.level_test_distance);
                let below = level.get(corner.x, corner.y - self.max_level_deviation, corner.z);
                let above = level.get(corner.x, corner.y + self.max_level_deviation, corner.z);
                if blocks.is_air(below) || !blocks.is_air(above) {
                    return false;
                }
            }
        }
        true
    }

    fn place_dirt_and_tree(&self, level: &mut Level<'_>, rng: &mut Rng, working: &mut Pos, pos: Pos) -> bool {
        let blocks = &level.env.blocks;
        for y in 0..self.root_column_max_height {
            *working = working.above();
            if level.height(Heightmap::WorldSurface, working.x, working.z) < working.y {
                return false;
            }
            if self.allowed_position.test(level, working.x, working.y, working.z) && self.space_for_tree(level, *working) {
                let below = working.below();
                let s = level.get(below.x, below.y, below.z);
                if blocks.fluid(s) == FluidKind::Lava || !blocks.solid(s) {
                    return false;
                }
                if self.tree.place_nested(level, rng, *working) {
                    self.place_dirt(pos, pos.y + y, level, rng);
                    return true;
                }
            }
        }
        false
    }

    fn place_dirt(&self, origin: Pos, target: i32, level: &mut Level<'_>, rng: &mut Rng) {
        for y in origin.y..target {
            let mut working = Pos::new(origin.x, y, origin.z);
            for _ in 0..self.root_attempts {
                let dx = rng.next_int_bounded(self.root_radius) - rng.next_int_bounded(self.root_radius);
                let dz = rng.next_int_bounded(self.root_radius) - rng.next_int_bounded(self.root_radius);
                working = working.offset(dx, 0, dz);
                let s = level.get(working.x, working.y, working.z);
                if self.root_replaceable.contains(&level.env.blocks.block_of(s)) {
                    let st = self.root_state.get(level, rng, working.x, working.y, working.z);
                    level.set(working.x, working.y, working.z, st);
                }
                working = Pos::new(origin.x, working.y, origin.z);
            }
        }
    }

    fn place_roots(&self, level: &mut Level<'_>, rng: &mut Rng, pos: Pos) {
        for _ in 0..self.hanging_attempts {
            let dx = rng.next_int_bounded(self.hanging_radius) - rng.next_int_bounded(self.hanging_radius);
            let dy = rng.next_int_bounded(self.hanging_span) - rng.next_int_bounded(self.hanging_span);
            let dz = rng.next_int_bounded(self.hanging_radius) - rng.next_int_bounded(self.hanging_radius);
            let p = pos.offset(dx, dy, dz);
            if level.env.blocks.is_air(level.get(p.x, p.y, p.z)) {
                let target = self.hanging_state.get(level, rng, p.x, p.y, p.z);
                let above = level.get(p.x, p.y + 1, p.z);
                if can_survive(level, target, p.x, p.y, p.z) && level.env.blocks.face_sturdy(above, Dir::Down, Support::Full) {
                    level.set(p.x, p.y, p.z, target);
                }
            }
        }
    }
}

pub fn place(cfg: &RootSystemConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    if !level.env.blocks.is_air(level.get(origin.x, origin.y, origin.z)) {
        return false;
    }
    let mut working = origin;
    if cfg.place_dirt_and_tree(level, rng, &mut working, origin) {
        cfg.place_roots(level, rng, origin);
    }
    true
}

