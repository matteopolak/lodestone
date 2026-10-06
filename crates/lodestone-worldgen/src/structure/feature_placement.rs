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
use super::{StructureWorld};

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
    placements: &[FeaturePlacement],
    world: &mut W,
    tags: &VegTags,
) {
    if placements.is_empty() {
        return;
    }

    let mut grid = VegGrid::with_borrowed_structure_source(world);
    for placement in placements {
        placement.place(random, &mut grid, tags);
    }

    let writes: Vec<_> = grid.dirty_cells().collect();
    drop(grid);
    for (x, y, z, state) in writes {
        world.set_id(x, y, z, state);
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
        place_feature_pool_elements(&mut random, &[placement], &mut world, &tags);
        assert_eq!(world.get(3, 1, 5), "minecraft:gold_block");
    }
}
