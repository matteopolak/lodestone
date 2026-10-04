//! Borrowed structure-world adapter for jigsaw feature-pool elements.
//!
//! A feature-pool element needs the vegetation placement interpreter because
//! its placed-feature document can include modifiers and a configured feature;
//! each helper entry freezes its source while a shared vegetation overlay runs.
//! WG heightmaps observe that entry snapshot, live reads observe earlier elements,
//! and captured writes are replayed into the structure world in declaration order.

use lodestone_worldgen_core::rng::RandomSource;

#[cfg(test)]
use crate::dense_grid::DenseBlockGrid;
use crate::feature::vegetation::{VegGrid, VegTags};

use super::pool::PoolFeaturePlacement;
use super::{StructureMutationContext, StructureWorld};

/// One feature-pool element retained by a structure piece until placement.
///
/// The pool parser owns the resolved document; the structure layer only carries
/// it to the structure-world adapter with the assembled origin and projection intact.
pub type FeaturePlacement = PoolFeaturePlacement;

/// Applies all feature-pool placements reached by one structure placement pass.
///
/// `random` is deliberately supplied by the caller: it must be the already
/// seeded structure-placement stream for this chunk and structure. The function
/// neither derives nor resets a seed, because the elements' placement modifiers
/// consume one shared stream in the same document order retained by jigsaw
/// conversion.
pub fn place_feature_pool_elements<R: RandomSource, W: StructureWorld>(
    random: &mut R,
    world_seed: i64,
    placements: &[FeaturePlacement],
    world: &mut W,
    tags: &VegTags,
) {
    place_feature_pool_elements_with_sink(random, world_seed, placements, world, tags, None);
}

pub fn place_feature_pool_elements_with_sink<R: RandomSource, W: StructureWorld>(
    random: &mut R,
    world_seed: i64,
    placements: &[FeaturePlacement],
    world: &mut W,
    tags: &VegTags,
    mutation: Option<&mut StructureMutationContext<'_>>,
) {
    if placements.is_empty() {
        return;
    }

    let mut grid = VegGrid::with_borrowed_structure_source(world);
    if mutation.is_some() {
        grid.begin_structure_mutation_capture();
    }
    for placement in placements {
        placement.place(random, world_seed, &mut grid, tags);
    }

    let writes = if mutation.is_some() {
        grid
            .take_structure_mutation_capture()
            .expect("structure mutation capture enabled")
    } else {
        grid.dirty_cells().collect()
    };
    drop(grid);
    if let Some(mutation) = mutation {
        for (x, y, z, state) in writes {
            mutation.write_id(world, x, y, z, state);
        }
    } else {
        for (x, y, z, state) in writes {
            world.set_id(x, y, z, state);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lodestone_worldgen_core::rng::LegacyRandomSource;

    use super::*;
    use crate::feature::vegetation::{ConfiguredFeature, PlacedRef};
    use crate::structure::pool::Projection;
    use lodestone_data::block::Block;

    #[test]
    fn resolved_feature_reaches_the_dense_grid() {
        let placement = PoolFeaturePlacement {
            feature: "test:block".to_string(),
            placed: Arc::new(PlacedRef {
                registry_id: None,
                placements: Vec::new(),
                feature: Box::new(ConfiguredFeature::SimpleBlock(
                    crate::feature::vegetation::BlockStateProvider::Simple(
                        Block::GoldBlock.default_state(),
                    ),
                )),
            }),
            origin: crate::feature::BlockPos { x: 3, y: 1, z: 5 },
            projection: Projection::Rigid,
        };
        let mut world = DenseBlockGrid::new(0, 0, 0, 16, 16, 16, "minecraft:air");
        world.set(3, 0, 5, "minecraft:dirt");
        let mut tags = VegTags::default();
        tags.supports_vegetation.insert(Block::Dirt);
        let mut random = LegacyRandomSource::new(0);
        place_feature_pool_elements(&mut random, 0, &[placement], &mut world, &tags);
        assert_eq!(world.get(3, 1, 5), "minecraft:gold_block");
    }

    #[test]
    fn feature_pool_mutations_retain_repeated_and_same_state_writes() {
        let placement = |state: &str| PoolFeaturePlacement {
            feature: format!("test:{state}"),
            placed: Arc::new(PlacedRef {
                registry_id: None,
                placements: Vec::new(),
                feature: Box::new(ConfiguredFeature::SimpleBlock(
                    crate::feature::vegetation::BlockStateProvider::Simple(
                        lodestone_data::block_states::StateId::from_state_str(state)
                            .expect("fixture state is generated"),
                    ),
                )),
            }),
            origin: crate::feature::BlockPos { x: 3, y: 1, z: 5 },
            projection: Projection::Rigid,
        };
        let mut world = DenseBlockGrid::new(0, 0, 0, 16, 16, 16, "minecraft:air");
        let tags = VegTags::default();
        let mut random = LegacyRandomSource::new(0);
        let mut recorder = crate::structure::StructureMutationRecorder::default();
        let mut mutation = crate::structure::StructureMutationContext::new(
            &mut recorder,
            (7, -3),
            4,
        );
        place_feature_pool_elements_with_sink(
            &mut random,
            0,
            &[placement("minecraft:tuff"), placement("minecraft:pumpkin"), placement("minecraft:pumpkin")],
            &mut world,
            &tags,
            Some(&mut mutation),
        );
        let blocks = recorder.finish();
        assert_eq!(
            blocks
                .mutations()
                .iter()
                .map(|write| (write.ordinal, write.position, write.state))
                .collect::<Vec<_>>(),
            vec![
                (0, [3, 1, 5], lodestone_data::block::Block::Tuff.default_state()),
                (1, [3, 1, 5], lodestone_data::block::Block::Pumpkin.default_state()),
                (2, [3, 1, 5], lodestone_data::block::Block::Pumpkin.default_state()),
            ]
        );
        assert_eq!(world.get(3, 1, 5), "minecraft:pumpkin");
    }
}
