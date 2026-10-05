//! The `fallen_tree` feature: a one-block stump and a horizontal log lying a few cells away.
//!
//! Decorators run on a [`Run`] whose log set holds just the stump, then just the fallen log;
//! the shell config only supplies the provider and decorator list, no trunk or foliage placer.

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use super::decorator::Decorator;
use super::foliage::{FoliageKind, FoliagePlacer};
use super::trunk::{TrunkKind, TrunkPlacer};
use super::{FeatureSize, Run, TreeConfig, int_provider};
use crate::blocks::{Dir, Support};
use crate::env::Env;
use crate::javaset::JavaSet;
use crate::json::{Res, array, get};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::provider::IntProvider;
use crate::registry::Loader;
use crate::stateprovider::StateProvider;

#[derive(Clone, Debug)]
pub struct FallenConfig {
    pub log_length: IntProvider,
    pub stump_decorators: Vec<Decorator>,
    pub log_decorators: Vec<Decorator>,
    /// Carries the trunk provider and any unported decorator types.
    pub shell: TreeConfig,
}

impl FallenConfig {
    pub fn parse(env: &Env, loader: &mut Loader<'_>, v: &Value, ctx: &str) -> Res<Self> {
        let mut unsupported = Vec::new();
        let mut list = |key: &str| -> Res<Vec<Decorator>> {
            array(v, key, ctx)?.iter().map(|d| Decorator::parse(env, loader, d, ctx, &mut unsupported)).collect()
        };
        let stump_decorators = list("stump_decorators")?;
        let log_decorators = list("log_decorators")?;
        let trunk = StateProvider::parse(env, get(v, "trunk_provider", ctx)?, ctx)?;
        let shell = TreeConfig {
            trunk: trunk.clone(),
            foliage: trunk.clone(),
            below_trunk: trunk,
            trunk_placer: TrunkPlacer { base: 0, rand_a: 0, rand_b: 0, kind: TrunkKind::Straight },
            foliage_placer: FoliagePlacer { radius: IntProvider::Constant(0), offset: IntProvider::Constant(0), kind: FoliageKind::Blob { height: 0 } },
            size: FeatureSize::TwoLayers { limit: 1, lower: 0, upper: 1, min_clipped: None },
            decorators: Vec::new(),
            root_placer: None,
            ignore_vines: false,
            unsupported,
        };
        Ok(Self { log_length: int_provider(v, "log_length", ctx)?, stump_decorators, log_decorators, shell })
    }
}

fn over_solid_ground(run: &Run<'_, '_>, p: Pos) -> bool {
    run.env.blocks.face_sturdy(run.get(p.below()), Dir::Up, Support::Full)
}

fn place_log(run: &mut Run<'_, '_>, p: Pos, axis: Option<&str>) {
    let mut s = run.cfg.trunk.get(run.level, run.rng, p.x, p.y, p.z);
    if let Some(a) = axis {
        if let Some(t) = run.env.blocks.with(s, "axis", a) {
            s = t;
        }
    }
    run.level.set(p.x, p.y, p.z, s);
}

fn decorate(run: &mut Run<'_, '_>, logs: &[Pos], decorators: &[Decorator]) {
    run.trunks = JavaSet::new();
    for p in logs {
        run.trunks.insert(*p);
    }
    for d in decorators {
        d.place(run);
    }
}

pub fn place(cfg: &FallenConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    if !cfg.shell.unsupported.is_empty() {
        return false;
    }
    let mut run = Run::new(level, rng, &cfg.shell);
    place_log(&mut run, origin, None);
    decorate(&mut run, &[origin], &cfg.stump_decorators);

    let dir = Dir::HORIZONTAL[run.rng.next_int_bounded(4) as usize];
    let length = cfg.log_length.sample(run.rng) - 2;
    let along = 2 + run.rng.next_int_bounded(2);
    let (dx, _, dz) = dir.step();
    let mut start = origin.offset(dx * along, 0, dz * along).above();
    for _ in 0..6 {
        if run.valid_tree_pos(start) && over_solid_ground(&run, start) {
            break;
        }
        start = start.below();
    }
    let mut gap = 0;
    let mut cursor = start;
    for _ in 0..length {
        if !run.valid_tree_pos(cursor) {
            return true;
        }
        if over_solid_ground(&run, cursor) {
            gap = 0;
        } else {
            gap += 1;
            if gap > 2 {
                return true;
            }
        }
        cursor = cursor.offset(dx, 0, dz);
    }
    let mut log = Vec::new();
    let mut cursor = start;
    for _ in 0..length {
        place_log(&mut run, cursor, Some(dir.axis_name()));
        log.push(cursor);
        cursor = cursor.offset(dx, 0, dz);
    }
    decorate(&mut run, &log, &cfg.log_decorators);
    true
}
