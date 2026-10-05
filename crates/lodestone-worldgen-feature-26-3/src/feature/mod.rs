//! Feature implementations and their configuration documents.

use std::sync::Arc;

use serde_json::Value;

use crate::env::Env;
use crate::json::{Res, get, type_of};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::registry::{Loader, PlacedFeature};

pub mod coral;
pub mod disk;
pub mod dripstone;
pub mod end;
pub mod freeze;
pub mod geode;
pub mod ice;
pub mod lake;
pub mod misc;
pub mod multiface;
pub mod sculk;
pub mod mushroom;
pub mod nether;
pub mod ore;
pub mod patch;
pub mod plant;
pub mod room;
pub mod root_system;
pub mod select;
pub mod structure;
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
    HugeMushroom(Box<mushroom::MushroomConfig>),
    Multiface(multiface::MultifaceConfig),
    SculkPatch(sculk::SculkPatchConfig),
    Spike(ice::SpikeConfig),
    Iceberg(ice::IcebergConfig),
    BlueIce,
    Speleothem(dripstone::SpeleothemConfig),
    SpeleothemCluster(Box<dripstone::ClusterConfig>),
    LargeDripstone(Box<dripstone::LargeConfig>),
    VegetationPatch(Box<patch::PatchConfig>),
    RootSystem(Box<root_system::RootSystemConfig>),
    MonsterRoom,
    Template(Box<structure::TemplateConfig>),
    Fossil(Box<structure::FossilConfig>),
    Geode(Box<geode::GeodeConfig>),
    CoralTree(Arc<PlacedFeature>),
    CoralClaw(Arc<PlacedFeature>),
    Vines,
    Bamboo(f32),
    UnderwaterMagma(misc::MagmaConfig),
    BlockBlob(misc::BlobConfig),
    ReplaceBlobs(nether::ReplaceBlobsConfig),
    Delta(nether::DeltaConfig),
    HugeFungus(Box<nether::FungusConfig>),
    NeighborSpread(Box<nether::NeighborSpreadConfig>),
    Pillar(Box<nether::PillarConfig>),
    ColumnCluster(Box<nether::ColumnClusterConfig>),
    PatchySquare(Box<nether::PatchySquareConfig>),
    EndSpike(end::SpikeConfig),
    EndPlatform,
    EndGateway(end::GatewayConfig),
    EndIsland,
    ChorusPlant,
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
            "tree" => Self::Tree(Box::new(tree::TreeConfig::parse(env, loader, v, ctx)?)),
            "fallen_tree" => Self::FallenTree(Box::new(tree::fallen::FallenConfig::parse(env, loader, v, ctx)?)),
            "huge_red_mushroom" => Self::HugeMushroom(Box::new(mushroom::MushroomConfig::parse(env, v, mushroom::Shape::Red, ctx)?)),
            "huge_brown_mushroom" => Self::HugeMushroom(Box::new(mushroom::MushroomConfig::parse(env, v, mushroom::Shape::Brown, ctx)?)),
            "multiface_growth" => Self::Multiface(multiface::MultifaceConfig::parse(env, v, ctx)?),
            "sculk_patch" => Self::SculkPatch(sculk::SculkPatchConfig::parse(v, ctx)?),
            "spike" => Self::Spike(ice::SpikeConfig::parse(env, v, ctx)?),
            "iceberg" => Self::Iceberg(ice::IcebergConfig::parse(env, v, ctx)?),
            "blue_ice" => Self::BlueIce,
            "speleothem" => Self::Speleothem(dripstone::SpeleothemConfig::parse(env, v, ctx)?),
            "speleothem_cluster" => Self::SpeleothemCluster(Box::new(dripstone::ClusterConfig::parse(env, v, ctx)?)),
            "large_dripstone" => Self::LargeDripstone(Box::new(dripstone::LargeConfig::parse(env, v, ctx)?)),
            "vegetation_patch" => Self::VegetationPatch(Box::new(patch::PatchConfig::parse(env, loader, v, false, ctx)?)),
            "waterlogged_vegetation_patch" => Self::VegetationPatch(Box::new(patch::PatchConfig::parse(env, loader, v, true, ctx)?)),
            "root_system" => Self::RootSystem(Box::new(root_system::RootSystemConfig::parse(env, loader, v, ctx)?)),
            "coral_tree" => Self::CoralTree(loader.placed_ref(env, get(v, "feature", ctx)?, ctx)?),
            "coral_claw" => Self::CoralClaw(loader.placed_ref(env, get(v, "feature", ctx)?, ctx)?),
            "monster_room" => Self::MonsterRoom,
            "template" => Self::Template(Box::new(structure::TemplateConfig::parse(env, v, ctx)?)),
            "fossil" => Self::Fossil(Box::new(structure::FossilConfig::parse(env, v, ctx)?)),
            "geode" => Self::Geode(Box::new(geode::GeodeConfig::parse(env, v, ctx)?)),
            "vines" => Self::Vines,
            "bamboo" => Self::Bamboo(crate::json::float(v, "probability", ctx)?),
            "underwater_magma" => Self::UnderwaterMagma(misc::MagmaConfig::parse(v, ctx)?),
            "block_blob" => Self::BlockBlob(misc::BlobConfig::parse(env, v, ctx)?),
            "netherrack_replace_blobs" => Self::ReplaceBlobs(nether::ReplaceBlobsConfig::parse(env, v, ctx)?),
            "delta_feature" => Self::Delta(nether::DeltaConfig::parse(env, v, ctx)?),
            "huge_fungus" => Self::HugeFungus(Box::new(nether::FungusConfig::parse(env, v, ctx)?)),
            "random_neighbor_spread" => Self::NeighborSpread(Box::new(nether::NeighborSpreadConfig::parse(env, v, ctx)?)),
            "single_block_pillar" => Self::Pillar(Box::new(nether::PillarConfig::parse(env, loader, v, ctx)?)),
            "stepped_column_cluster" => Self::ColumnCluster(Box::new(nether::ColumnClusterConfig::parse(env, v, ctx)?)),
            "projected_random_patchy_square" => Self::PatchySquare(Box::new(nether::PatchySquareConfig::parse(env, v, ctx)?)),
            "end_spike" => Self::EndSpike(end::SpikeConfig::parse(v, ctx)?),
            "end_platform" => Self::EndPlatform,
            "end_gateway" => Self::EndGateway(end::GatewayConfig::parse(v, ctx)?),
            "end_island" => Self::EndIsland,
            "chorus_plant" => Self::ChorusPlant,
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
            Self::Multiface(_) => "multiface_growth",
            Self::SculkPatch(_) => "sculk_patch",
            Self::Spike(_) => "spike",
            Self::Iceberg(_) => "iceberg",
            Self::BlueIce => "blue_ice",
            Self::Speleothem(_) => "speleothem",
            Self::SpeleothemCluster(_) => "speleothem_cluster",
            Self::LargeDripstone(_) => "large_dripstone",
            Self::VegetationPatch(c) => if c.waterlogged { "waterlogged_vegetation_patch" } else { "vegetation_patch" },
            Self::RootSystem(_) => "root_system",
            Self::CoralTree(_) => "coral_tree",
            Self::CoralClaw(_) => "coral_claw",
            Self::MonsterRoom => "monster_room",
            Self::Template(_) => "template",
            Self::Fossil(_) => "fossil",
            Self::Geode(_) => "geode",
            Self::Vines => "vines",
            Self::Bamboo(_) => "bamboo",
            Self::UnderwaterMagma(_) => "underwater_magma",
            Self::BlockBlob(_) => "block_blob",
            Self::HugeMushroom(c) => if c.shape == mushroom::Shape::Red { "huge_red_mushroom" } else { "huge_brown_mushroom" },
            Self::ReplaceBlobs(_) => "netherrack_replace_blobs",
            Self::Delta(_) => "delta_feature",
            Self::HugeFungus(_) => "huge_fungus",
            Self::NeighborSpread(_) => "random_neighbor_spread",
            Self::Pillar(_) => "single_block_pillar",
            Self::ColumnCluster(_) => "stepped_column_cluster",
            Self::PatchySquare(_) => "projected_random_patchy_square",
            Self::EndSpike(_) => "end_spike",
            Self::EndPlatform => "end_platform",
            Self::EndGateway(_) => "end_gateway",
            Self::EndIsland => "end_island",
            Self::ChorusPlant => "chorus_plant",
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
            Self::Template(c) => c.unsupported().iter().for_each(|u| {
                out.insert(format!("template {u}"));
            }),
            Self::Fossil(c) => c.unsupported().iter().for_each(|u| {
                out.insert(format!("fossil {u}"));
            }),
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
            Self::VegetationPatch(c) => c.vegetation.gaps(env, out),
            Self::RootSystem(c) => c.tree.gaps(env, out),
            Self::CoralTree(f) | Self::CoralClaw(f) => f.gaps(env, out),
            Self::Pillar(c) => c.cap.iter().for_each(|f| f.gaps(env, out)),
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
            Self::HugeMushroom(c) => mushroom::place(c, level, rng, origin),
            Self::Multiface(c) => multiface::place(c, level, rng, origin),
            Self::SculkPatch(c) => sculk::place(c, level, rng, origin),
            Self::Spike(c) => ice::place_spike(c, level, rng, origin),
            Self::Iceberg(c) => ice::place_iceberg(c, level, rng, origin),
            Self::BlueIce => ice::place_blue_ice(level, rng, origin),
            Self::Speleothem(c) => dripstone::place_speleothem(c, level, rng, origin),
            Self::SpeleothemCluster(c) => dripstone::place_cluster(c, level, rng, origin),
            Self::LargeDripstone(c) => dripstone::place_large(c, level, rng, origin),
            Self::VegetationPatch(c) => patch::place(c, level, rng, origin),
            Self::RootSystem(c) => root_system::place(c, level, rng, origin),
            Self::CoralTree(f) => coral::place_tree(f, level, rng, origin),
            Self::CoralClaw(f) => coral::place_claw(f, level, rng, origin),
            Self::MonsterRoom => room::place(level, rng, origin),
            Self::Template(c) => structure::place_template(c, level, rng, origin),
            Self::Fossil(c) => structure::place_fossil(c, level, rng, origin),
            Self::Geode(c) => geode::place(c, level, rng, origin),
            Self::Vines => misc::place_vines(level, origin),
            Self::Bamboo(p) => misc::place_bamboo(*p, level, rng, origin),
            Self::UnderwaterMagma(c) => misc::place_underwater_magma(c, level, rng, origin),
            Self::BlockBlob(c) => misc::place_block_blob(c, level, rng, origin),
            Self::ReplaceBlobs(c) => nether::place_replace_blobs(c, level, rng, origin),
            Self::Delta(c) => nether::place_delta(c, level, rng, origin),
            Self::HugeFungus(c) => nether::place_huge_fungus(c, level, rng, origin),
            Self::NeighborSpread(c) => nether::place_neighbor_spread(c, level, rng, origin),
            Self::Pillar(c) => nether::place_pillar(c, level, rng, origin),
            Self::ColumnCluster(c) => nether::place_column_cluster(c, level, rng, origin),
            Self::PatchySquare(c) => nether::place_patchy_square(c, level, rng, origin),
            Self::EndSpike(c) => end::place_spikes(c, level, rng, origin),
            Self::EndPlatform => end::place_platform(level, origin),
            Self::EndGateway(c) => end::place_gateway(c, level, origin),
            Self::EndIsland => end::place_island(level, rng, origin),
            Self::ChorusPlant => end::place_chorus(level, rng, origin),
            Self::NoOp | Self::Unported(_) => false,
        }
    }
}
