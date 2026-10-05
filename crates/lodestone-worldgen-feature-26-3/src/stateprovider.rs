//! Block-state providers: how a feature chooses the state it places at a position.

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use crate::blocks::State;
use crate::env::Env;
use crate::json::{Res, array, get, int, strip, type_of};
use crate::level::Level;
use crate::pos::Rng;
use crate::predicate::BlockPred;

#[derive(Clone, Debug)]
pub enum StateProvider {
    Simple(State),
    /// The first rule whose condition holds and whose provider yields a state wins; then the
    /// fallback. No state at all means the caller leaves the position alone.
    RuleBased { fallback: Option<Box<StateProvider>>, rules: Vec<(BlockPred, StateProvider)> },
    /// One bounded draw over the total weight, then the cumulative-weight pick.
    Weighted(Vec<(State, i32)>),
}

impl StateProvider {
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
                other => return Err(format!("{ctx}: unknown state provider `{other}`")),
            }),
            _ => Err(format!("{ctx}: state provider expected")),
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
        }
    }

    /// The state to place; a rule-based provider with no answer keeps the current block.
    pub fn get(&self, level: &Level<'_>, rng: &mut Rng, x: i32, y: i32, z: i32) -> State {
        self.get_optional(level, rng, x, y, z).unwrap_or_else(|| level.get(x, y, z))
    }
}
