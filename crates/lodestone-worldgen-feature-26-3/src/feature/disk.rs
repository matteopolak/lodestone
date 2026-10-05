//! Disks of sand, clay, gravel and the like (`disk`).

use serde_json::Value;

use crate::env::Env;
use crate::json::{Res, get, int};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::predicate::BlockPred;
use crate::provider::IntProvider;
use crate::stateprovider::StateProvider;

#[derive(Clone, Debug)]
pub struct DiskConfig {
    pub state: StateProvider,
    pub target: BlockPred,
    pub radius: IntProvider,
    pub half_height: i32,
}

impl DiskConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            state: StateProvider::parse(env, get(v, "state_provider", ctx)?, ctx)?,
            target: BlockPred::parse(env, get(v, "target", ctx)?, ctx)?,
            radius: IntProvider::parse(get(v, "radius", ctx)?, ctx)?,
            half_height: int(v, "half_height", ctx)?,
        })
    }
}

pub fn place_disk(cfg: &DiskConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let top = origin.y + cfg.half_height;
    let bottom = origin.y - cfg.half_height - 1;
    let r = cfg.radius.sample(rng);
    let mut placed_any = false;
    // Columns are visited x-fastest then z, which fixes the order of any provider draws.
    for z in origin.z - r..=origin.z + r {
        for x in origin.x - r..=origin.x + r {
            let (xd, zd) = (x - origin.x, z - origin.z);
            if xd * xd + zd * zd <= r * r {
                placed_any |= place_column(cfg, level, rng, x, z, top, bottom);
            }
        }
    }
    placed_any
}

fn place_column(cfg: &DiskConfig, level: &mut Level<'_>, rng: &mut Rng, x: i32, z: i32, top: i32, bottom: i32) -> bool {
    let mut placed_any = false;
    for y in (bottom + 1..=top).rev() {
        if cfg.target.test(level, x, y, z) {
            if let Some(state) = cfg.state.get_optional(level, rng, x, y, z) {
                level.set(x, y, z, state);
                placed_any = true;
            }
        }
    }
    placed_any
}
