//! Feature implementations and their configuration documents.

use serde_json::Value;

use crate::env::Env;
use crate::json::{Res, type_of};
use crate::level::Level;
use crate::pos::{Pos, Rng};

pub mod ore;

/// A configured feature.
#[derive(Clone, Debug)]
pub enum Feature {
    Ore(ore::OreConfig),
    ScatteredOre(ore::OreConfig),
    NoOp,
    /// A feature type that is not ported yet; it places nothing.
    Unported(String),
}

impl Feature {
    /// Parses a `feature` document.
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let t = type_of(v, ctx)?;
        Ok(match t {
            "ore" => Self::Ore(ore::OreConfig::parse(env, v, ctx)?),
            "scattered_ore" => Self::ScatteredOre(ore::OreConfig::parse(env, v, ctx)?),
            "no_op" => Self::NoOp,
            other => Self::Unported(other.to_owned()),
        })
    }

    /// The registry name of the feature's type.
    #[must_use]
    pub fn type_name(&self) -> &str {
        match self {
            Self::Ore(_) => "ore",
            Self::ScatteredOre(_) => "scattered_ore",
            Self::NoOp => "no_op",
            Self::Unported(t) => t,
        }
    }

    /// Places the feature at `origin`; returns whether anything was placed.
    pub fn place(&self, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
        match self {
            Self::Ore(c) => ore::place_ore(c, level, rng, origin),
            Self::ScatteredOre(c) => ore::place_scattered(c, level, rng, origin),
            Self::NoOp | Self::Unported(_) => false,
        }
    }
}
