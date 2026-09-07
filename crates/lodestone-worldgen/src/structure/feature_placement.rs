//! Dense-grid adapter for jigsaw feature-pool elements.
//!
//! A feature-pool element needs the vegetation placement interpreter because
//! its placed-feature document can include modifiers and a configured feature;
//! a structure stage, however, owns a [`crate::dense_grid::DenseBlockGrid`].
//! This module gives one structure-placement pass a shared vegetation overlay,
//! so every element observes prior element writes and the final writes reach
//! the dense grid in declaration order.

use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;

use crate::dense_grid::DenseBlockGrid;
use crate::feature::vegetation::{VegGrid, VegTags};

use super::pool::PoolFeaturePlacement;

/// One feature-pool element retained by a structure piece until placement.
///
/// The pool parser owns the resolved document; the structure layer only carries
/// it to the dense-grid adapter with the assembled origin and projection intact.
pub type FeaturePlacement = PoolFeaturePlacement;

/// Applies all feature-pool placements reached by one structure placement pass.
///
/// `random` is deliberately supplied by the caller: it must be the already
/// seeded structure-placement stream for this chunk and structure. The function
/// neither derives nor resets a seed, because the elements' placement modifiers
/// consume one shared stream in the same document order retained by jigsaw
/// conversion.
pub fn place_feature_pool_elements<R: RandomSource>(
    random: &mut R,
    placements: &[FeaturePlacement],
    world: &mut DenseBlockGrid,
    tags: &VegTags,
) {
    if placements.is_empty() {
        return;
    }

    let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
    debug_assert_eq!(size_x, size_z, "structure feature grids are square chunks");
    let source = Arc::new(world.clone());
    let mut grid = VegGrid::with_sources(
        Arc::clone(world.interner()),
        min_y,
        size_y,
        min_x,
        min_z,
        0,
        size_x,
        |dx, dz| (dx == 0 && dz == 0).then(|| Arc::clone(&source)),
    );
    for placement in placements {
        placement.place(random, &mut grid, tags);
    }

    debug_assert_eq!(
        grid.interner().instance_id(),
        world.interner().instance_id(),
        "feature overlay and structure grid must share a state interner",
    );
    for (x, y, z, state) in grid.dirty_cell_ids() {
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

    #[test]
    fn resolved_feature_reaches_the_dense_grid() {
        let placement = PoolFeaturePlacement {
            feature: "test:block".to_string(),
            placed: Arc::new(PlacedRef {
                registry_id: None,
                placements: Vec::new(),
                feature: Box::new(ConfiguredFeature::SimpleBlock(
                    crate::feature::vegetation::BlockStateProvider::Simple(
                        "minecraft:gold_block".to_string(),
                    ),
                )),
            }),
            origin: crate::feature::BlockPos { x: 3, y: 1, z: 5 },
            projection: Projection::Rigid,
        };
        let mut world = DenseBlockGrid::new(0, 0, 0, 16, 16, 16, "minecraft:air");
        world.set(3, 0, 5, "minecraft:dirt");
        let mut tags = VegTags::default();
        tags.supports_vegetation.insert("minecraft:dirt".to_string());
        let mut random = LegacyRandomSource::new(0);
        place_feature_pool_elements(&mut random, &[placement], &mut world, &tags);
        assert_eq!(world.get(3, 1, 5), "minecraft:gold_block");
    }
}
