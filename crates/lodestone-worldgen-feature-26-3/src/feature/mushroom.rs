//! `huge_red_mushroom` and `huge_brown_mushroom`: a stem column with a cap, drawn from the
//! cap provider with the faces set from the position inside the cap.

use serde_json::Value;

use crate::blocks::State;
use crate::env::Env;
use crate::json::{Res, get, int_or};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::predicate::BlockPred;
use crate::stateprovider::StateProvider;
use lodestone_worldgen_core::rng::RandomSource;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Red,
    Brown,
}

#[derive(Clone, Debug)]
pub struct MushroomConfig {
    pub shape: Shape,
    pub cap: StateProvider,
    pub stem: StateProvider,
    pub radius: i32,
    pub can_place_on: BlockPred,
}

impl MushroomConfig {
    pub fn parse(env: &Env, v: &Value, shape: Shape, ctx: &str) -> Res<Self> {
        Ok(Self {
            shape,
            cap: StateProvider::parse(env, get(v, "cap_provider", ctx)?, ctx)?,
            stem: StateProvider::parse(env, get(v, "stem_provider", ctx)?, ctx)?,
            radius: int_or(v, "foliage_radius", 2, ctx)?,
            can_place_on: BlockPred::parse(env, get(v, "can_place_on", ctx)?, ctx)?,
        })
    }

    /// The radius of the footprint that must be clear at height `yo`.
    fn radius_at(&self, tree_height: i32, yo: i32) -> i32 {
        match self.shape {
            Shape::Brown => {
                if yo <= 3 {
                    0
                } else {
                    self.radius
                }
            }
            Shape::Red => {
                if (yo < tree_height && yo >= tree_height - 3) || yo == tree_height {
                    self.radius
                } else {
                    0
                }
            }
        }
    }
}

fn place_block(level: &mut Level<'_>, p: Pos, s: State) {
    let env = level.env;
    let here = level.get(p.x, p.y, p.z);
    let replaceable = env.tags.get("replaceable_by_mushrooms").expect("replaceable_by_mushrooms tag");
    if env.blocks.is_air(here) || replaceable.contains(env.blocks.block_of(here)) {
        level.set(p.x, p.y, p.z, s);
    }
}

fn flag(level: &Level<'_>, s: State, face: &str, on: bool) -> State {
    level.env.blocks.with(s, face, if on { "true" } else { "false" }).expect("face property")
}

pub fn place(cfg: &MushroomConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let mut tree_height = rng.next_int_bounded(3) + 4;
    if rng.next_int_bounded(12) == 0 {
        tree_height *= 2;
    }
    let y = origin.y;
    if y < level.min_y + 1 || y + tree_height + 1 > level.max_y() {
        return false;
    }
    let below = origin.below();
    if !cfg.can_place_on.test(level, below.x, below.y, below.z) {
        return false;
    }
    let leaves = env.tags.get("leaves").expect("leaves tag");
    for dy in 0..=tree_height {
        let r = cfg.radius_at(tree_height, dy);
        for dx in -r..=r {
            for dz in -r..=r {
                let s = level.get(origin.x + dx, y + dy, origin.z + dz);
                if !env.blocks.is_air(s) && !leaves.contains(env.blocks.block_of(s)) {
                    return false;
                }
            }
        }
    }
    let r = cfg.radius;
    let has_faces = |s: State, faces: &[&str]| faces.iter().all(|f| env.blocks.has_property(s, f));
    match cfg.shape {
        Shape::Brown => {
            for dx in -r..=r {
                for dz in -r..=r {
                    let (min_x, max_x, min_z, max_z) = (dx == -r, dx == r, dz == -r, dz == r);
                    let (x_edge, z_edge) = (min_x || max_x, min_z || max_z);
                    if x_edge && z_edge {
                        continue;
                    }
                    let west = min_x || (z_edge && dx == 1 - r);
                    let east = max_x || (z_edge && dx == r - 1);
                    let north = min_z || (x_edge && dz == 1 - r);
                    let south = max_z || (x_edge && dz == r - 1);
                    let mut s = cfg.cap.get(level, rng, origin.x, origin.y, origin.z);
                    if has_faces(s, &["west", "east", "north", "south"]) {
                        s = flag(level, s, "west", west);
                        s = flag(level, s, "east", east);
                        s = flag(level, s, "north", north);
                        s = flag(level, s, "south", south);
                    }
                    place_block(level, origin.offset(dx, tree_height, dz), s);
                }
            }
        }
        Shape::Red => {
            for dy in (tree_height - 3)..=tree_height {
                let radius = if dy < tree_height { r } else { r - 1 };
                let center = r - 2;
                for dx in -radius..=radius {
                    for dz in -radius..=radius {
                        let x_edge = dx == -radius || dx == radius;
                        let z_edge = dz == -radius || dz == radius;
                        if dy >= tree_height || x_edge != z_edge {
                            let mut s = cfg.cap.get(level, rng, origin.x, origin.y, origin.z);
                            if has_faces(s, &["west", "east", "north", "south", "up"]) {
                                s = flag(level, s, "up", dy >= tree_height - 1);
                                s = flag(level, s, "west", dx < -center);
                                s = flag(level, s, "east", dx > center);
                                s = flag(level, s, "north", dz < -center);
                                s = flag(level, s, "south", dz > center);
                            }
                            place_block(level, origin.offset(dx, dy, dz), s);
                        }
                    }
                }
            }
        }
    }
    for dy in 0..tree_height {
        let s = cfg.stem.get(level, rng, origin.x, origin.y, origin.z);
        place_block(level, origin.offset(0, dy, 0), s);
    }
    true
}
