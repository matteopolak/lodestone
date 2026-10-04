//! Value providers: integer, float and height distributions and vertical anchors.

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use crate::json::{Res, array, double, float, get, int, int_or, type_of};
use crate::level::Level;

/// A uniform draw in `[min, max_inclusive]`: one bounded draw plus the minimum.
fn between<R: RandomSource>(r: &mut R, min: i32, max_inclusive: i32) -> i32 {
    r.next_int_bounded(max_inclusive - min + 1) + min
}

fn normal<R: RandomSource>(r: &mut R, mean: f32, deviation: f32) -> f32 {
    mean + r.next_gaussian() as f32 * deviation
}

#[derive(Clone, Debug)]
pub enum IntProvider {
    Constant(i32),
    Uniform(i32, i32),
    BiasedToBottom(i32, i32),
    VeryBiasedToBottom(i32, i32),
    Clamped { source: Box<IntProvider>, min: i32, max: i32 },
    ClampedNormal { mean: f32, deviation: f32, min: i32, max: i32 },
    Trapezoid { min: i32, max: i32, plateau: i32 },
    WeightedList(Vec<(IntProvider, i32)>),
}

impl IntProvider {
    pub fn parse(v: &Value, ctx: &str) -> Res<Self> {
        if let Some(n) = v.as_i64() {
            return Ok(Self::Constant(n as i32));
        }
        Ok(match type_of(v, ctx)? {
            "constant" => Self::Constant(int(v, "value", ctx)?),
            "uniform" => Self::Uniform(int(v, "min_inclusive", ctx)?, int(v, "max_inclusive", ctx)?),
            "biased_to_bottom" => Self::BiasedToBottom(int(v, "min_inclusive", ctx)?, int(v, "max_inclusive", ctx)?),
            "very_biased_to_bottom" => Self::VeryBiasedToBottom(int(v, "min_inclusive", ctx)?, int(v, "max_inclusive", ctx)?),
            "clamped" => Self::Clamped {
                source: Box::new(Self::parse(get(v, "source", ctx)?, ctx)?),
                min: int(v, "min_inclusive", ctx)?,
                max: int(v, "max_inclusive", ctx)?,
            },
            "clamped_normal" => Self::ClampedNormal {
                mean: float(v, "mean", ctx)?,
                deviation: float(v, "deviation", ctx)?,
                min: int(v, "min_inclusive", ctx)?,
                max: int(v, "max_inclusive", ctx)?,
            },
            "trapezoid" => Self::Trapezoid { min: int(v, "min", ctx)?, max: int(v, "max", ctx)?, plateau: int(v, "plateau", ctx)? },
            "weighted_list" => {
                let mut list = Vec::new();
                for e in array(v, "distribution", ctx)? {
                    list.push((Self::parse(get(e, "data", ctx)?, ctx)?, int(e, "weight", ctx)?));
                }
                Self::WeightedList(list)
            }
            other => return Err(format!("{ctx}: unknown int provider `{other}`")),
        })
    }

    pub fn sample<R: RandomSource>(&self, r: &mut R) -> i32 {
        match self {
            Self::Constant(v) => *v,
            Self::Uniform(a, b) => between(r, *a, *b),
            Self::BiasedToBottom(a, b) => {
                let inner = r.next_int_bounded(*b - *a + 1) + 1;
                *a + r.next_int_bounded(inner)
            }
            Self::VeryBiasedToBottom(a, b) => {
                let x = r.next_int_bounded(*b - *a + 1) + 1;
                let y = r.next_int_bounded(x) + 1;
                *a + r.next_int_bounded(y)
            }
            Self::Clamped { source, min, max } => source.sample(r).clamp(*min, *max),
            Self::ClampedNormal { mean, deviation, min, max } => {
                normal(r, *mean, *deviation).clamp(*min as f32, *max as f32) as i32
            }
            Self::Trapezoid { min, max, plateau } => {
                if *plateau == 0 && *max == -*min {
                    return r.next_int_bounded(*max + 1) - r.next_int_bounded(*max + 1);
                }
                let range = *max - *min;
                if *plateau == range {
                    return between(r, *min, *max);
                }
                let plateau_start = (range - *plateau) / 2;
                let plateau_end = range - plateau_start;
                *min + between(r, 0, plateau_end) + between(r, 0, plateau_start)
            }
            Self::WeightedList(list) => {
                let total: i32 = list.iter().map(|(_, w)| *w).sum();
                let mut sel = r.next_int_bounded(total);
                for (p, w) in list {
                    sel -= *w;
                    if sel < 0 {
                        return p.sample(r);
                    }
                }
                unreachable!("weighted selection within total")
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum FloatProvider {
    Constant(f32),
    Uniform(f32, f32),
    ClampedNormal { mean: f32, deviation: f32, min: f32, max: f32 },
    Trapezoid { min: f32, max: f32, plateau: f32 },
}

impl FloatProvider {
    pub fn parse(v: &Value, ctx: &str) -> Res<Self> {
        if let Some(n) = v.as_f64() {
            return Ok(Self::Constant(n as f32));
        }
        Ok(match type_of(v, ctx)? {
            "constant" => Self::Constant(float(v, "value", ctx)?),
            "uniform" => Self::Uniform(float(v, "min_inclusive", ctx)?, float(v, "max_exclusive", ctx)?),
            "clamped_normal" => Self::ClampedNormal {
                mean: float(v, "mean", ctx)?,
                deviation: float(v, "deviation", ctx)?,
                min: float(v, "min", ctx)?,
                max: float(v, "max", ctx)?,
            },
            "trapezoid" => Self::Trapezoid { min: float(v, "min", ctx)?, max: float(v, "max", ctx)?, plateau: float(v, "plateau", ctx)? },
            other => return Err(format!("{ctx}: unknown float provider `{other}`")),
        })
    }

    pub fn sample<R: RandomSource>(&self, r: &mut R) -> f32 {
        match self {
            Self::Constant(v) => *v,
            Self::Uniform(a, b) => r.next_float() * (*b - *a) + *a,
            Self::ClampedNormal { mean, deviation, min, max } => normal(r, *mean, *deviation).clamp(*min, *max),
            Self::Trapezoid { min, max, plateau } => {
                let range = *max - *min;
                let plateau_start = (range - *plateau) / 2.0;
                let plateau_end = range - plateau_start;
                *min + r.next_float() * plateau_end + r.next_float() * plateau_start
            }
        }
    }
}

/// A vertical position resolved against the generator's range.
#[derive(Clone, Copy, Debug)]
pub enum Anchor {
    Absolute(i32),
    AboveBottom(i32),
    BelowTop(i32),
    RelativeToSeaLevel(i32),
}

impl Anchor {
    pub fn parse(v: &Value, ctx: &str) -> Res<Self> {
        let o = crate::json::obj(v, ctx)?;
        if o.contains_key("absolute") {
            Ok(Self::Absolute(int(v, "absolute", ctx)?))
        } else if o.contains_key("above_bottom") {
            Ok(Self::AboveBottom(int(v, "above_bottom", ctx)?))
        } else if o.contains_key("below_top") {
            Ok(Self::BelowTop(int(v, "below_top", ctx)?))
        } else if o.contains_key("relative_to_sea_level") {
            Ok(Self::RelativeToSeaLevel(int(v, "relative_to_sea_level", ctx)?))
        } else {
            Err(format!("{ctx}: unknown vertical anchor"))
        }
    }

    #[must_use]
    pub fn resolve(self, level: &Level<'_>) -> i32 {
        match self {
            Self::Absolute(y) => y,
            Self::AboveBottom(o) => level.gen_min_y + o,
            Self::BelowTop(o) => level.gen_depth - 1 + level.gen_min_y - o,
            Self::RelativeToSeaLevel(o) => level.sea_level + o,
        }
    }
}

#[derive(Clone, Debug)]
pub enum HeightProvider {
    Constant(Anchor),
    Uniform(Anchor, Anchor),
    BiasedToBottom { min: Anchor, max: Anchor, inner: i32 },
    VeryBiasedToBottom { min: Anchor, max: Anchor, inner: i32 },
    Trapezoid { min: Anchor, max: Anchor, plateau: i32 },
    WeightedList(Vec<(HeightProvider, i32)>),
}

impl HeightProvider {
    pub fn parse(v: &Value, ctx: &str) -> Res<Self> {
        if v.get("type").is_none() {
            return Ok(Self::Constant(Anchor::parse(v, ctx)?));
        }
        let a = |k: &str| Anchor::parse(get(v, k, ctx)?, ctx);
        Ok(match type_of(v, ctx)? {
            "constant" => Self::Constant(Anchor::parse(get(v, "value", ctx)?, ctx)?),
            "uniform" => Self::Uniform(a("min_inclusive")?, a("max_inclusive")?),
            "biased_to_bottom" => Self::BiasedToBottom { min: a("min_inclusive")?, max: a("max_inclusive")?, inner: int_or(v, "inner", 1, ctx)? },
            "very_biased_to_bottom" => {
                Self::VeryBiasedToBottom { min: a("min_inclusive")?, max: a("max_inclusive")?, inner: int_or(v, "inner", 1, ctx)? }
            }
            "trapezoid" => Self::Trapezoid { min: a("min_inclusive")?, max: a("max_inclusive")?, plateau: int_or(v, "plateau", 0, ctx)? },
            "weighted_list" => {
                let mut list = Vec::new();
                for e in array(v, "distribution", ctx)? {
                    list.push((Self::parse(get(e, "data", ctx)?, ctx)?, int(e, "weight", ctx)?));
                }
                Self::WeightedList(list)
            }
            other => return Err(format!("{ctx}: unknown height provider `{other}`")),
        })
    }

    pub fn sample<R: RandomSource>(&self, r: &mut R, level: &Level<'_>) -> i32 {
        match self {
            Self::Constant(a) => a.resolve(level),
            Self::Uniform(a, b) => {
                let (min, max) = (a.resolve(level), b.resolve(level));
                if min > max { min } else { between(r, min, max) }
            }
            Self::BiasedToBottom { min, max, inner } => {
                let (min, max) = (min.resolve(level), max.resolve(level));
                if max - min - inner + 1 <= 0 {
                    return min;
                }
                let limit = r.next_int_bounded(max - min - inner + 1);
                r.next_int_bounded(limit + inner) + min
            }
            Self::VeryBiasedToBottom { min, max, inner } => {
                let (min, max) = (min.resolve(level), max.resolve(level));
                if max - min - inner + 1 <= 0 {
                    return min;
                }
                let next = |r: &mut R, lo: i32, hi: i32| if lo >= hi { lo } else { r.next_int_bounded(hi - lo + 1) + lo };
                let upper = next(r, min + inner, max);
                let biased = next(r, min, upper - 1);
                next(r, min, biased - 1 + inner)
            }
            Self::Trapezoid { min, max, plateau } => {
                let (min, max) = (min.resolve(level), max.resolve(level));
                if min > max {
                    return min;
                }
                let range = max - min;
                if *plateau >= range {
                    return between(r, min, max);
                }
                let plateau_start = (range - *plateau) / 2;
                let plateau_end = range - plateau_start;
                min + between(r, 0, plateau_end) + between(r, 0, plateau_start)
            }
            Self::WeightedList(list) => {
                let total: i32 = list.iter().map(|(_, w)| *w).sum();
                let mut sel = r.next_int_bounded(total);
                for (p, w) in list {
                    sel -= *w;
                    if sel < 0 {
                        return p.sample(r, level);
                    }
                }
                unreachable!("weighted selection within total")
            }
        }
    }
}

/// Reads a double with a default.
pub fn double_or(v: &Value, key: &str, default: f64, ctx: &str) -> Res<f64> {
    if v.get(key).is_some() { double(v, key, ctx) } else { Ok(default) }
}
