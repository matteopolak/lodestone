//! Feature implementations and their configuration documents.

use std::sync::Arc;

use serde_json::Value;

use crate::env::Env;
use crate::json::{Res, get, type_of};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::registry::{Loader, PlacedFeature};

pub mod disk;
pub mod freeze;
pub mod lake;
pub mod ore;
pub mod plant;
pub mod select;
pub mod tree;

/// A configured feature.
#[derive(Clone, Debug)]
pub enum Feature {
    Ore(ore::OreConfig),
    ScatteredOre(ore::OreConfig),
    Lake(lake::LakeConfig),
    Spring(lake::SpringConfig),
    Disk(disk::DiskConfig),
    FreezeTopLayer,
    SimpleBlock(plant::SimpleBlockConfig),
    BlockColumn(plant::BlockColumnConfig),
    RandomSelector(select::RandomSelector),
    SimpleRandomSelector(Vec<Arc<PlacedFeature>>),
    WeightedRandomSelector(select::WeightedSelector),
    RandomBooleanSelector(Arc<PlacedFeature>, Arc<PlacedFeature>),
    Sequence(Vec<Arc<PlacedFeature>>),
    Overlay(Vec<Arc<PlacedFeature>>),
    Tree(Box<tree::TreeConfig>),
    FallenTree(Box<tree::fallen::FallenConfig>),
    NoOp,
    /// A feature type that is not ported yet; it places nothing.
    Unported(String),
}

impl Feature {
    /// Parses a `feature` document.
    pub fn parse(env: &Env, loader: &mut Loader<'_>, v: &Value, ctx: &str) -> Res<Self> {
        let t = type_of(v, ctx)?;
        Ok(match t {
            "ore" => Self::Ore(ore::OreConfig::parse(env, v, ctx)?),
            "scattered_ore" => Self::ScatteredOre(ore::OreConfig::parse(env, v, ctx)?),
            "lake" => Self::Lake(lake::LakeConfig::parse(env, v, ctx)?),
            "spring_feature" => Self::Spring(lake::SpringConfig::parse(env, v, ctx)?),
            "disk" => Self::Disk(disk::DiskConfig::parse(env, v, ctx)?),
            "freeze_top_layer" => Self::FreezeTopLayer,
            "simple_block" => Self::SimpleBlock(plant::SimpleBlockConfig::parse(env, v, ctx)?),
            "block_column" => Self::BlockColumn(plant::BlockColumnConfig::parse(env, v, ctx)?),
            "random_selector" => Self::RandomSelector(select::RandomSelector::parse(env, loader, v, ctx)?),
            "simple_random_selector" => Self::SimpleRandomSelector(loader.placed_list(env, get(v, "features", ctx)?, ctx)?),
            "weighted_random_selector" => Self::WeightedRandomSelector(select::WeightedSelector::parse(env, loader, v, ctx)?),
            "random_boolean_selector" => Self::RandomBooleanSelector(
                loader.placed_ref(env, get(v, "feature_true", ctx)?, ctx)?,
                loader.placed_ref(env, get(v, "feature_false", ctx)?, ctx)?,
            ),
            "sequence" => Self::Sequence(loader.placed_list(env, get(v, "features", ctx)?, ctx)?),
            "overlay" => Self::Overlay(loader.placed_list(env, get(v, "features", ctx)?, ctx)?),
            "tree" => Self::Tree(Box::new(tree::TreeConfig::parse(env, v, ctx)?)),
            "fallen_tree" => Self::FallenTree(Box::new(tree::fallen::FallenConfig::parse(env, v, ctx)?)),
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
            Self::Lake(_) => "lake",
            Self::Spring(_) => "spring_feature",
            Self::Disk(_) => "disk",
            Self::FreezeTopLayer => "freeze_top_layer",
            Self::SimpleBlock(_) => "simple_block",
            Self::BlockColumn(_) => "block_column",
            Self::RandomSelector(_) => "random_selector",
            Self::SimpleRandomSelector(_) => "simple_random_selector",
            Self::WeightedRandomSelector(_) => "weighted_random_selector",
            Self::RandomBooleanSelector(..) => "random_boolean_selector",
            Self::Sequence(_) => "sequence",
            Self::Overlay(_) => "overlay",
            Self::Tree(_) => "tree",
            Self::FallenTree(_) => "fallen_tree",
            Self::NoOp => "no_op",
            Self::Unported(t) => t,
        }
    }

    /// Records what keeps this feature from matching the reference yet: unported feature types
    /// and placed blocks whose survival rule is not ported, looking through nested features.
    pub fn gaps(&self, env: &Env, out: &mut std::collections::BTreeSet<String>) {
        let mut states = Vec::new();
        match self {
            Self::Unported(t) => {
                out.insert(format!("type {t}"));
            }
            Self::Tree(c) => c.unsupported.iter().for_each(|u| {
                out.insert(format!("tree {u}"));
            }),
            Self::FallenTree(c) => c.shell.unsupported.iter().for_each(|u| {
                out.insert(format!("fallen_tree {u}"));
            }),
            Self::SimpleBlock(c) => c.to_place.states(&mut states),
            Self::BlockColumn(c) => c.layers.iter().for_each(|(_, p)| p.states(&mut states)),
            Self::RandomSelector(c) => {
                c.features.iter().for_each(|(_, f)| f.gaps(env, out));
                c.default.gaps(env, out);
            }
            Self::SimpleRandomSelector(l) | Self::Sequence(l) | Self::Overlay(l) => l.iter().for_each(|f| f.gaps(env, out)),
            Self::WeightedRandomSelector(c) => c.features.iter().for_each(|(f, _)| f.gaps(env, out)),
            Self::RandomBooleanSelector(t, f) => {
                t.gaps(env, out);
                f.gaps(env, out);
            }
            _ => {}
        }
        for s in states {
            let b = env.blocks.block_of(s);
            if matches!(self, Self::SimpleBlock(_)) && !env.survive.supported(b) {
                out.insert(format!("survive {}", env.blocks.block_name(b)));
            }
        }
    }

    /// Places the feature at `origin`; returns whether anything was placed.
    pub fn place(&self, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
        match self {
            Self::Ore(c) => ore::place_ore(c, level, rng, origin),
            Self::ScatteredOre(c) => ore::place_scattered(c, level, rng, origin),
            Self::Lake(c) => lake::place_lake(c, level, rng, origin),
            Self::Spring(c) => lake::place_spring(c, level, origin),
            Self::Disk(c) => disk::place_disk(c, level, rng, origin),
            Self::FreezeTopLayer => freeze::place_freeze_top_layer(level, origin),
            Self::SimpleBlock(c) => plant::place_simple_block(c, level, rng, origin),
            Self::BlockColumn(c) => plant::place_block_column(c, level, rng, origin),
            Self::RandomSelector(c) => c.place(level, rng, origin),
            Self::SimpleRandomSelector(l) => select::place_simple_random(l, level, rng, origin),
            Self::WeightedRandomSelector(c) => c.place(level, rng, origin),
            Self::RandomBooleanSelector(t, f) => select::place_random_boolean(t, f, level, rng, origin),
            Self::Sequence(l) => select::place_sequence(l, level, rng, origin),
            Self::Overlay(l) => select::place_overlay(l, level, rng, origin),
            Self::Tree(c) => tree::place_tree(c, level, rng, origin),
            Self::FallenTree(c) => tree::fallen::place(c, level, rng, origin),
            Self::NoOp | Self::Unported(_) => false,
        }
    }
}
