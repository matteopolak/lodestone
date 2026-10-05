//! Features that pick or chain other placed features: the selectors, `sequence` and `overlay`.

use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use crate::env::Env;
use crate::json::{Res, array, float, get, int};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::registry::{Loader, PlacedFeature};

/// The first entry whose chance roll succeeds, else the default.
#[derive(Clone, Debug)]
pub struct RandomSelector {
    pub features: Vec<(f32, Arc<PlacedFeature>)>,
    pub default: Arc<PlacedFeature>,
}

#[derive(Clone, Debug)]
pub struct WeightedSelector {
    pub features: Vec<(Arc<PlacedFeature>, i32)>,
}

impl RandomSelector {
    pub fn parse(env: &Env, loader: &mut Loader<'_>, v: &Value, ctx: &str) -> Res<Self> {
        let mut features = Vec::new();
        for e in array(v, "features", ctx)? {
            features.push((float(e, "chance", ctx)?, loader.placed_ref(env, get(e, "feature", ctx)?, ctx)?));
        }
        Ok(Self { features, default: loader.placed_ref(env, get(v, "default", ctx)?, ctx)? })
    }

    pub fn place(&self, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
        for (chance, f) in &self.features {
            if rng.next_float() < *chance {
                return f.place_nested(level, rng, origin);
            }
        }
        self.default.place_nested(level, rng, origin)
    }
}

impl WeightedSelector {
    pub fn parse(env: &Env, loader: &mut Loader<'_>, v: &Value, ctx: &str) -> Res<Self> {
        let mut features = Vec::new();
        for e in array(v, "features", ctx)? {
            features.push((loader.placed_ref(env, get(e, "data", ctx)?, ctx)?, int(e, "weight", ctx)?));
        }
        Ok(Self { features })
    }

    pub fn place(&self, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
        let total: i32 = self.features.iter().map(|(_, w)| *w).sum();
        if total == 0 {
            return false;
        }
        let mut sel = rng.next_int_bounded(total);
        for (f, w) in &self.features {
            sel -= *w;
            if sel < 0 {
                return f.place_nested(level, rng, origin);
            }
        }
        unreachable!("weighted pick lands inside the total")
    }
}

/// One uniformly chosen feature.
pub fn place_simple_random(list: &[Arc<PlacedFeature>], level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let i = rng.next_int_bounded(list.len() as i32) as usize;
    list[i].place_nested(level, rng, origin)
}

pub fn place_random_boolean(t: &PlacedFeature, f: &PlacedFeature, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    if rng.next_bool() { t.place_nested(level, rng, origin) } else { f.place_nested(level, rng, origin) }
}

/// Places each in order and stops at the first that places nothing.
pub fn place_sequence(list: &[Arc<PlacedFeature>], level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    list.iter().all(|f| f.place_nested(level, rng, origin))
}

/// Places every one; true when any placed.
pub fn place_overlay(list: &[Arc<PlacedFeature>], level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let mut any = false;
    for f in list {
        any |= f.place_nested(level, rng, origin);
    }
    any
}
