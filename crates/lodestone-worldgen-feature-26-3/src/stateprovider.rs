//! Block-state providers: how a feature chooses the state it places at a position.

use std::sync::Arc;

use lodestone_worldgen_core::engine::release26_3::noise::{NoiseStack, NormalNoise};
use lodestone_worldgen_core::engine::release26_3::tree::parse_noise_params;
use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource};
use serde_json::Value;

use crate::blocks::{Dir, State};
use crate::env::Env;
use crate::json::{Res, array, float, get, int, strip, string, type_of};
use crate::level::Level;
use crate::pos::Rng;
use crate::predicate::BlockPred;
use crate::provider::IntProvider;

/// A noise field seeded the way the providers seed it: the parameter document built over a
/// fresh legacy random source.
#[derive(Clone, Debug)]
pub struct Field {
    noise: Arc<NoiseStack>,
}

impl Field {
    fn parse(v: &Value, seed: i64, ctx: &str) -> Res<Self> {
        let params = parse_noise_params(v).map_err(|e| format!("{ctx}: noise: {e:?}"))?;
        let noise = NormalNoise::new(params).create(&mut LegacyRandomSource::new(seed));
        Ok(Self { noise: Arc::new(noise) })
    }

    fn at(&self, x: i32, y: i32, z: i32, scale: f32) -> f32 {
        let s = f64::from(scale);
        self.noise.get(f64::from(x) * s, f64::from(y) * s, f64::from(z) * s)
    }
}

/// The state a noise value in `[-1, 1]` selects from `states`.
fn pick_by_noise(states: &[State], noise: f32) -> State {
    let placement = ((1.0f32 + noise) / 2.0f32).clamp(0.0, 0.9999f32);
    states[(placement * states.len() as f32) as usize]
}

fn states_of(env: &Env, v: &Value, key: &str, ctx: &str) -> Res<Vec<State>> {
    array(v, key, ctx)?.iter().map(|s| env.blocks.parse_state(s).map_err(|e| format!("{ctx}: {e}"))).collect()
}

#[derive(Clone, Debug)]
pub enum StateProvider {
    Simple(State),
    /// The first rule whose condition holds and whose provider yields a state wins; then the
    /// fallback. No state at all means the caller leaves the position alone.
    RuleBased { fallback: Option<Box<StateProvider>>, rules: Vec<(BlockPred, StateProvider)> },
    /// One bounded draw over the total weight, then the cumulative-weight pick.
    Weighted(Vec<(State, i32)>),
    /// A noise value picks one state, with no random draw.
    Noise { field: Field, scale: f32, states: Vec<State> },
    /// A slow noise sets how many of the states are in play at a spot (`variety`), each chosen
    /// from the slow noise at a shifted position, then the fast noise picks among them.
    DualNoise { variety: (i32, i32), slow: Field, slow_scale: f32, field: Field, scale: f32, states: Vec<State> },
    NoiseThreshold { field: Field, scale: f32, threshold: f32, high_chance: f32, default: State, low: Vec<State>, high: Vec<State> },
    /// Overrides an integer property of the source's state with a drawn value; a state without
    /// the property passes through with no draw.
    RandomizedInt { source: Box<StateProvider>, property: String, values: IntProvider },
    /// Orients the source's state: the `axis` property follows the direction's axis, `facing`
    /// follows the direction when its domain admits it.
    Rotated { state: Box<StateProvider>, direction: Option<Dir> },
    /// A uniformly chosen block's default state (the list is in tag listing order).
    RandomBlock(Vec<State>),
}

impl StateProvider {
    /// The state of a provider that always answers the same one.
    #[must_use]
    pub fn constant(&self) -> Option<State> {
        match self {
            Self::Simple(s) => Some(*s),
            _ => None,
        }
    }

    /// Parses a provider: a bare state, a named provider document, or a typed provider.
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        match v {
            Value::String(name) => match lodestone_worldgen_data_26_3::find(lodestone_worldgen_data_26_3::BLOCK_STATE_PROVIDER, name) {
                Some(doc) => {
                    let doc: Value = serde_json::from_str(doc).map_err(|e| format!("{ctx}: provider {name}: {e}"))?;
                    Self::parse(env, &doc, &format!("block_state_provider/{}", strip(name)))
                }
                None => Ok(Self::Simple(env.blocks.parse_state(v).map_err(|e| format!("{ctx}: {e}"))?)),
            },
            Value::Object(o) if !o.contains_key("type") => Ok(Self::Simple(env.blocks.parse_state(v).map_err(|e| format!("{ctx}: {e}"))?)),
            Value::Object(_) => Ok(match type_of(v, ctx)? {
                "simple" => Self::Simple(env.blocks.parse_state(get(v, "state", ctx)?).map_err(|e| format!("{ctx}: {e}"))?),
                "rule_based" => {
                    let fallback = v.get("fallback").map(|f| Self::parse(env, f, ctx).map(Box::new)).transpose()?;
                    let mut rules = Vec::new();
                    for r in array(v, "rules", ctx)? {
                        rules.push((BlockPred::parse(env, get(r, "if_true", ctx)?, ctx)?, Self::parse(env, get(r, "then", ctx)?, ctx)?));
                    }
                    Self::RuleBased { fallback, rules }
                }
                "weighted" => {
                    let mut entries = Vec::new();
                    for e in array(v, "entries", ctx)? {
                        entries.push((env.blocks.parse_state(get(e, "data", ctx)?).map_err(|x| format!("{ctx}: {x}"))?, int(e, "weight", ctx)?));
                    }
                    Self::Weighted(entries)
                }
                "noise" => {
                    let seed = get(v, "seed", ctx)?.as_i64().ok_or_else(|| format!("{ctx}: seed"))?;
                    Self::Noise { field: Field::parse(get(v, "noise", ctx)?, seed, ctx)?, scale: float(v, "scale", ctx)?, states: states_of(env, v, "states", ctx)? }
                }
                "dual_noise" => {
                    let seed = get(v, "seed", ctx)?.as_i64().ok_or_else(|| format!("{ctx}: seed"))?;
                    let var = array(v, "variety", ctx)?;
                    let bound = |i: usize| var.get(i).and_then(Value::as_i64).map(|n| n as i32).ok_or_else(|| format!("{ctx}: variety"));
                    Self::DualNoise {
                        variety: (bound(0)?, bound(1)?),
                        slow: Field::parse(get(v, "slow_noise", ctx)?, seed, ctx)?,
                        slow_scale: float(v, "slow_scale", ctx)?,
                        field: Field::parse(get(v, "noise", ctx)?, seed, ctx)?,
                        scale: float(v, "scale", ctx)?,
                        states: states_of(env, v, "states", ctx)?,
                    }
                }
                "noise_threshold" => {
                    let seed = get(v, "seed", ctx)?.as_i64().ok_or_else(|| format!("{ctx}: seed"))?;
                    Self::NoiseThreshold {
                        field: Field::parse(get(v, "noise", ctx)?, seed, ctx)?,
                        scale: float(v, "scale", ctx)?,
                        threshold: float(v, "threshold", ctx)?,
                        high_chance: float(v, "high_chance", ctx)?,
                        default: env.blocks.parse_state(get(v, "default_state", ctx)?).map_err(|e| format!("{ctx}: {e}"))?,
                        low: states_of(env, v, "low_states", ctx)?,
                        high: states_of(env, v, "high_states", ctx)?,
                    }
                }
                "randomized_int" => Self::RandomizedInt {
                    source: Box::new(Self::parse(env, get(v, "source", ctx)?, ctx)?),
                    property: string(v, "property", ctx)?.to_owned(),
                    values: IntProvider::parse(get(v, "values", ctx)?, ctx)?,
                },
                "rotated" => Self::Rotated {
                    state: Box::new(Self::parse(env, get(v, "state", ctx)?, ctx)?),
                    direction: v.get("direction").and_then(Value::as_str).map(|d| Dir::from_name(d).ok_or_else(|| format!("{ctx}: bad direction"))).transpose()?,
                },
                "random_block" => {
                    let blocks: Vec<crate::blocks::BlockId> = match get(v, "blocks", ctx)? {
                        Value::String(s) => match s.strip_prefix('#') {
                            Some(tag) => env.tags.ordered(tag).ok_or_else(|| format!("{ctx}: unknown tag {tag}"))?.to_vec(),
                            None => vec![env.blocks.block_by_name(s).ok_or_else(|| format!("{ctx}: unknown block {s}"))?],
                        },
                        Value::Array(a) => a
                            .iter()
                            .map(|b| b.as_str().and_then(|n| env.blocks.block_by_name(n)).ok_or_else(|| format!("{ctx}: unknown block")))
                            .collect::<Res<_>>()?,
                        _ => return Err(format!("{ctx}: blocks expected")),
                    };
                    Self::RandomBlock(blocks.into_iter().map(|b| env.blocks.default_state(b)).collect())
                }
                other => return Err(format!("{ctx}: unknown state provider `{other}`")),
            }),
            _ => Err(format!("{ctx}: state provider expected")),
        }
    }

    /// Every state this provider can yield.
    pub fn states(&self, out: &mut Vec<State>) {
        match self {
            Self::Simple(s) => out.push(*s),
            Self::RuleBased { fallback, rules } => {
                for (_, p) in rules {
                    p.states(out);
                }
                if let Some(f) = fallback {
                    f.states(out);
                }
            }
            Self::Weighted(e) => out.extend(e.iter().map(|(s, _)| *s)),
            Self::Noise { states, .. } | Self::DualNoise { states, .. } | Self::RandomBlock(states) => out.extend(states),
            Self::NoiseThreshold { default, low, high, .. } => {
                out.push(*default);
                out.extend(low);
                out.extend(high);
            }
            Self::RandomizedInt { source, .. } => source.states(out),
            Self::Rotated { state, .. } => state.states(out),
        }
    }

    /// The state to place, or `None` when no rule applies and there is no fallback.
    pub fn get_optional(&self, level: &Level<'_>, rng: &mut Rng, x: i32, y: i32, z: i32) -> Option<State> {
        match self {
            Self::Simple(s) => Some(*s),
            Self::RuleBased { fallback, rules } => {
                for (cond, then) in rules {
                    if cond.test(level, x, y, z) {
                        if let Some(s) = then.get_optional(level, rng, x, y, z) {
                            return Some(s);
                        }
                    }
                }
                fallback.as_ref().and_then(|f| f.get_optional(level, rng, x, y, z))
            }
            Self::Weighted(entries) => {
                let total: i32 = entries.iter().map(|(_, w)| *w).sum();
                let mut sel = rng.next_int_bounded(total);
                for (s, w) in entries {
                    sel -= *w;
                    if sel < 0 {
                        return Some(*s);
                    }
                }
                unreachable!("weighted pick lands inside the total")
            }
            Self::Noise { field, scale, states } => Some(pick_by_noise(states, field.at(x, y, z, *scale))),
            Self::DualNoise { variety, slow, slow_scale, field, scale, states } => {
                let v = f64::from(slow.at(x, y, z, *slow_scale));
                let t = ((v + 1.0) / 2.0).clamp(0.0, 1.0);
                let (lo, hi) = (f64::from(variety.0), f64::from(variety.1 + 1));
                let count = (lo + t * (hi - lo)) as i32;
                let chosen: Vec<State> = (0..count).map(|i| pick_by_noise(states, slow.at(x + i * 54545, y, z + i * 34234, *slow_scale))).collect();
                Some(pick_by_noise(&chosen, field.at(x, y, z, *scale)))
            }
            Self::NoiseThreshold { field, scale, threshold, high_chance, default, low, high } => {
                if field.at(x, y, z, *scale) < *threshold {
                    Some(low[rng.next_int_bounded(low.len() as i32) as usize])
                } else if rng.next_float() < *high_chance {
                    Some(high[rng.next_int_bounded(high.len() as i32) as usize])
                } else {
                    Some(*default)
                }
            }
            Self::RandomizedInt { source, property, values } => {
                let state = source.get_optional(level, rng, x, y, z)?;
                if !level.env.blocks.has_property(state, property) {
                    return Some(state);
                }
                let n = values.sample(rng);
                level.env.blocks.with(state, property, &n.to_string()).or(Some(state))
            }
            Self::Rotated { state, direction } => {
                let dir = direction.unwrap_or_else(|| Dir::ALL[rng.next_int_bounded(6) as usize]);
                let mut s = state.get_optional(level, rng, x, y, z)?;
                let blocks = &level.env.blocks;
                let axis = match dir {
                    Dir::Down | Dir::Up => "y",
                    Dir::North | Dir::South => "z",
                    Dir::West | Dir::East => "x",
                };
                if let Some(n) = blocks.with(s, "axis", axis) {
                    s = n;
                }
                if blocks.has_property(s, "facing") {
                    let admits = blocks.property_values(s, "facing").is_some_and(|v| v.len() == 6 || (v.len() == 4 && !matches!(dir, Dir::Down | Dir::Up)));
                    if admits {
                        s = blocks.with(s, "facing", dir.name()).unwrap_or(s);
                    }
                }
                Some(s)
            }
            Self::RandomBlock(states) => {
                if states.is_empty() {
                    None
                } else {
                    Some(states[rng.next_int_bounded(states.len() as i32) as usize])
                }
            }
        }
    }

    /// The state to place; a rule-based provider with no answer keeps the current block.
    pub fn get(&self, level: &Level<'_>, rng: &mut Rng, x: i32, y: i32, z: i32) -> State {
        self.get_optional(level, rng, x, y, z).unwrap_or_else(|| level.get(x, y, z))
    }
}
