//! Block predicates (placement filters and feature conditions) and rule tests (ore targets).

use serde_json::Value;

use crate::blocks::{BlockId, Dir, FluidKind, State, Support};
use crate::env::Env;
use crate::json::{Res, array, get, int, obj, strip, string, type_of};
use crate::level::Level;
use crate::provider::Anchor;
use crate::tags::BlockSet;

/// A set of blocks named by id list or by tag.
#[derive(Clone, Debug)]
pub enum BlockMatch {
    Blocks(Vec<BlockId>),
    Tag(BlockSet),
}

impl BlockMatch {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let one = |s: &str| -> Res<BlockId> { env.blocks.block_by_name(s).ok_or_else(|| format!("{ctx}: unknown block {s}")) };
        match v {
            Value::String(s) => {
                if let Some(t) = s.strip_prefix('#') {
                    Ok(Self::Tag(env.tags.get(t).ok_or_else(|| format!("{ctx}: unknown tag {t}"))?.clone()))
                } else {
                    Ok(Self::Blocks(vec![one(s)?]))
                }
            }
            Value::Array(a) => {
                let mut out = Vec::new();
                for e in a {
                    out.push(one(e.as_str().ok_or_else(|| format!("{ctx}: block id expected"))?)?);
                }
                Ok(Self::Blocks(out))
            }
            _ => Err(format!("{ctx}: expected a block list")),
        }
    }

    #[must_use]
    pub fn contains(&self, b: BlockId) -> bool {
        match self {
            Self::Blocks(v) => v.contains(&b),
            Self::Tag(t) => t.contains(b),
        }
    }
}

/// A fluid a predicate names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FluidMatch {
    kind: FluidKind,
    /// `Some(true)` the source fluid, `Some(false)` the flowing one, `None` (empty) either.
    source: Option<bool>,
}

impl FluidMatch {
    fn parse(name: &str, ctx: &str) -> Res<Self> {
        Ok(match strip(name) {
            "empty" => Self { kind: FluidKind::Empty, source: None },
            "water" => Self { kind: FluidKind::Water, source: Some(true) },
            "flowing_water" => Self { kind: FluidKind::Water, source: Some(false) },
            "lava" => Self { kind: FluidKind::Lava, source: Some(true) },
            "flowing_lava" => Self { kind: FluidKind::Lava, source: Some(false) },
            other => return Err(format!("{ctx}: unknown fluid {other}")),
        })
    }

    fn test(self, env: &Env, s: State) -> bool {
        let kind = env.blocks.fluid(s);
        if kind != self.kind {
            return false;
        }
        self.source.is_none_or(|src| env.blocks.fluid_is_source(s) == src)
    }
}

#[derive(Clone, Debug)]
pub enum BlockPred {
    AlwaysTrue,
    AllOf(Vec<BlockPred>),
    AnyOf(Vec<BlockPred>),
    Not(Box<BlockPred>),
    MatchingBlocks { offset: [i32; 3], blocks: BlockMatch },
    MatchingBlockTag { offset: [i32; 3], tag: BlockSet },
    MatchingFluids { offset: [i32; 3], fluids: Vec<FluidMatch> },
    HasSturdyFace { offset: [i32; 3], dir: Dir },
    Solid { offset: [i32; 3] },
    Replaceable { offset: [i32; 3] },
    WouldSurvive { offset: [i32; 3], state: State },
    InsideWorldBounds { offset: [i32; 3] },
    HeightRange { min: Anchor, max: Anchor },
    Unobstructed { offset: [i32; 3] },
    VolumeMatch { min: [i32; 3], max: [i32; 3], matcher: Box<BlockPred> },
}

pub fn offset3(v: &Value, key: &str, ctx: &str) -> Res<[i32; 3]> {
    match v.get(key) {
        None => Ok([0; 3]),
        Some(Value::Array(a)) if a.len() == 3 => {
            let n = |i: usize| a[i].as_i64().map(|x| x as i32).ok_or_else(|| format!("{ctx}.{key}: integer expected"));
            Ok([n(0)?, n(1)?, n(2)?])
        }
        Some(_) => Err(format!("{ctx}.{key}: expected [x, y, z]")),
    }
}

impl BlockPred {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let list = |key: &str| -> Res<Vec<BlockPred>> {
            array(v, key, ctx)?.iter().map(|e| Self::parse(env, e, ctx)).collect()
        };
        Ok(match type_of(v, ctx)? {
            "true" => Self::AlwaysTrue,
            "all_of" => Self::AllOf(list("predicates")?),
            "any_of" => Self::AnyOf(list("predicates")?),
            "not" => Self::Not(Box::new(Self::parse(env, get(v, "predicate", ctx)?, ctx)?)),
            "matching_blocks" => {
                Self::MatchingBlocks { offset: offset3(v, "offset", ctx)?, blocks: BlockMatch::parse(env, get(v, "blocks", ctx)?, ctx)? }
            }
            "matching_block_tag" => {
                let tag = strip(string(v, "tag", ctx)?);
                Self::MatchingBlockTag {
                    offset: offset3(v, "offset", ctx)?,
                    tag: env.tags.get(tag).ok_or_else(|| format!("{ctx}: unknown tag {tag}"))?.clone(),
                }
            }
            "matching_fluids" => {
                let names: Vec<&str> = match get(v, "fluids", ctx)? {
                    Value::String(s) => vec![s.as_str()],
                    Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                    _ => return Err(format!("{ctx}: fluids expected")),
                };
                let fluids = names.into_iter().map(|n| FluidMatch::parse(n, ctx)).collect::<Res<Vec<_>>>()?;
                Self::MatchingFluids { offset: offset3(v, "offset", ctx)?, fluids }
            }
            "has_sturdy_face" => Self::HasSturdyFace {
                offset: offset3(v, "offset", ctx)?,
                dir: Dir::from_name(string(v, "direction", ctx)?).ok_or_else(|| format!("{ctx}: bad direction"))?,
            },
            "solid" => Self::Solid { offset: offset3(v, "offset", ctx)? },
            "replaceable" => Self::Replaceable { offset: offset3(v, "offset", ctx)? },
            "would_survive" => Self::WouldSurvive {
                offset: offset3(v, "offset", ctx)?,
                state: env.blocks.parse_state(get(v, "state", ctx)?).map_err(|e| format!("{ctx}: {e}"))?,
            },
            "inside_world_bounds" => Self::InsideWorldBounds { offset: offset3(v, "offset", ctx)? },
            "height_range" => Self::HeightRange {
                min: Anchor::parse(get(v, "min_inclusive", ctx)?, ctx)?,
                max: Anchor::parse(get(v, "max_inclusive", ctx)?, ctx)?,
            },
            "unobstructed" => Self::Unobstructed { offset: offset3(v, "offset", ctx)? },
            "volume_match" => Self::VolumeMatch {
                min: offset3(v, "min", ctx)?,
                max: offset3(v, "max", ctx)?,
                matcher: Box::new(Self::parse(env, get(v, "match", ctx)?, ctx)?),
            },
            other => return Err(format!("{ctx}: unknown block predicate `{other}`")),
        })
    }

    /// Collects the states whose placement rule this predicate consults.
    pub fn survive_states(&self, out: &mut Vec<State>) {
        match self {
            Self::AllOf(p) | Self::AnyOf(p) => p.iter().for_each(|p| p.survive_states(out)),
            Self::Not(p) => p.survive_states(out),
            Self::VolumeMatch { matcher, .. } => matcher.survive_states(out),
            Self::WouldSurvive { state, .. } => out.push(*state),
            _ => {}
        }
    }

    #[must_use]
    pub fn test(&self, level: &Level<'_>, x: i32, y: i32, z: i32) -> bool {
        let env = level.env;
        let at = |o: &[i32; 3]| level.get(x + o[0], y + o[1], z + o[2]);
        match self {
            Self::AlwaysTrue => true,
            Self::AllOf(p) => p.iter().all(|p| p.test(level, x, y, z)),
            Self::AnyOf(p) => p.iter().any(|p| p.test(level, x, y, z)),
            Self::Not(p) => !p.test(level, x, y, z),
            Self::MatchingBlocks { offset, blocks } => blocks.contains(env.blocks.block_of(at(offset))),
            Self::MatchingBlockTag { offset, tag } => tag.contains(env.blocks.block_of(at(offset))),
            Self::MatchingFluids { offset, fluids } => {
                let s = at(offset);
                fluids.iter().any(|f| f.test(env, s))
            }
            Self::HasSturdyFace { offset, dir } => env.blocks.face_sturdy(at(offset), *dir, Support::Full),
            Self::Solid { offset } => env.blocks.solid(at(offset)),
            Self::Replaceable { offset } => env.blocks.replaceable(at(offset)),
            Self::WouldSurvive { offset, state } => crate::survive::can_survive(level, *state, x + offset[0], y + offset[1], z + offset[2]),
            Self::InsideWorldBounds { offset } => !level.is_outside_build_height(y + offset[1]),
            Self::HeightRange { min, max } => y >= min.resolve(level) && y <= max.resolve(level),
            Self::Unobstructed { .. } => true,
            Self::VolumeMatch { min, max, matcher } => {
                for ox in min[0]..=max[0] {
                    for oz in min[2]..=max[2] {
                        for oy in min[1]..=max[1] {
                            if !matcher.test(level, x + ox, y + oy, z + oz) {
                                return false;
                            }
                        }
                    }
                }
                true
            }
        }
    }
}

/// A test over a state and position (ore targets).
#[derive(Clone, Debug)]
pub enum RuleTest {
    AlwaysTrue,
    AllOf(Vec<RuleTest>),
    AnyOf(Vec<RuleTest>),
    Not(Box<RuleTest>),
    BlockMatch(BlockId),
    TagMatch(BlockSet),
    HeightMatch { min: i32, max: i32 },
}

impl RuleTest {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let kind = string(v, "predicate_type", ctx)?;
        let list = |key: &str| -> Res<Vec<RuleTest>> { array(v, key, ctx)?.iter().map(|e| Self::parse(env, e, ctx)).collect() };
        Ok(match strip(kind) {
            "always_true" => Self::AlwaysTrue,
            "all_of" => Self::AllOf(list("rules")?),
            "any_of" => Self::AnyOf(list("rules")?),
            "not" => Self::Not(Box::new(Self::parse(env, get(v, "rule", ctx)?, ctx)?)),
            "block_match" => {
                let b = string(v, "block", ctx)?;
                Self::BlockMatch(env.blocks.block_by_name(b).ok_or_else(|| format!("{ctx}: unknown block {b}"))?)
            }
            "tag_match" => {
                let t = strip(string(v, "tag", ctx)?);
                Self::TagMatch(env.tags.get(t).ok_or_else(|| format!("{ctx}: unknown tag {t}"))?.clone())
            }
            "height_match" => Self::HeightMatch { min: int(v, "min_inclusive", ctx)?, max: int(v, "max_inclusive", ctx)? },
            other => return Err(format!("{ctx}: unknown rule test `{other}`")),
        })
    }

    #[must_use]
    pub fn test(&self, env: &Env, state: State, y: i32) -> bool {
        match self {
            Self::AlwaysTrue => true,
            Self::AllOf(r) => r.iter().all(|r| r.test(env, state, y)),
            Self::AnyOf(r) => r.iter().any(|r| r.test(env, state, y)),
            Self::Not(r) => !r.test(env, state, y),
            Self::BlockMatch(b) => env.blocks.block_of(state) == *b,
            Self::TagMatch(t) => t.contains(env.blocks.block_of(state)),
            Self::HeightMatch { min, max } => *min <= y && y <= *max,
        }
    }
}

/// Resolves a block-state document, naming the context on failure.
pub fn state_of(env: &Env, v: &Value, ctx: &str) -> Res<State> {
    let _ = obj; // keep the helper import used across modules
    env.blocks.parse_state(v).map_err(|e| format!("{ctx}: {e}"))
}
