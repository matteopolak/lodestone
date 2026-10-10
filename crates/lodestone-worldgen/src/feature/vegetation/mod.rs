//! Placed-feature parsing and placement for structure feature-pool elements.
//!
//! A placed-feature document is parsed once into a [`PlacedRef`]: its
//! placement modifiers ([`VegPlacement`]) and its configured feature
//! ([`ConfiguredFeature`]). Placement walks the modifiers depth-first, as the
//! parent module describes, and dispatches each surviving position to the
//! feature body against a [`VegGrid`].
//!
//! # Degrade, never crash
//!
//! A configured-feature type this engine does not model parses to
//! [`ConfiguredFeature::Unsupported`] and places nothing, so a datapack naming
//! an unknown type still produces a world. Set `LODESTONE_VEG_STRICT=1` to turn
//! that no-op into a panic naming the reason.
//!
//! # File layout
//!
//! * this file: `place_placed_feature` and the configured-feature dispatch;
//! * [`config`]: the JSON, predicate and provider layer every feature is parsed into;
//! * [`tree`]: trunk placers, foliage placers and leaf-distance propagation;
//! * [`place`]: simple block, block column, tree and beehive bodies;
//! * [`features`] and the per-feature files: every other feature body;
//! * [`grid`]: [`VegGrid`];
//! * [`ids`]: [`VegTags`], the block-tag membership the predicates test.

mod config;
pub(crate) mod features;
mod grid;
pub(crate) mod ids;
mod place;
mod tree;

/// Whether an unmodelled feature should panic instead of placing nothing:
/// `LODESTONE_VEG_STRICT=1`, read once per process.
fn strict_unsupported() -> bool {
    static STRICT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *STRICT.get_or_init(|| {
        std::env::var("LODESTONE_VEG_STRICT").is_ok_and(|v| v != "0" && !v.is_empty())
    })
}

pub(crate) use self::config::*;
pub(crate) use self::grid::*;
use self::place::*;

use crate::feature::BlockPos;
use crate::rng::RandomSource;
#[cfg(test)]
use lodestone_data::block::Block;

/// Runs `placed`'s placement modifiers from `origin`, then its configured
/// feature at every position they emit. Returns whether any block was written.
pub(crate) fn place_placed_feature<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    placed: &PlacedRef,
    grid: &mut VegGrid,
    tags: &VegTags,
) -> bool {
    fn recurse<R: RandomSource>(
        random: &mut R,
        mods: &[VegPlacement],
        i: usize,
        pos: BlockPos,
        grid: &mut VegGrid,
        tags: &VegTags,
        feature: &ConfiguredFeature,
    ) -> bool {
        if i == mods.len() {
            return place_configured_feature(random, pos, feature, grid, tags);
        }
        // `Repeat(p, n)` recurses `n` times on the same position, depth-first,
        // which fixes the draw order.
        match mods[i].get_positions(random, pos, grid, tags) {
            Positions::None => false,
            Positions::One(next) => recurse(random, mods, i + 1, next, grid, tags, feature),
            Positions::Repeat(next, n) => {
                let mut placed = false;
                for _ in 0..n {
                    placed |= recurse(random, mods, i + 1, next, grid, tags, feature);
                }
                placed
            }
        }
    }
    recurse(random, &placed.placements, 0, origin, grid, tags, &placed.feature)
}

fn place_configured_feature<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    feature: &ConfiguredFeature,
    grid: &mut VegGrid,
    tags: &VegTags,
) -> bool {
    feature.bind_states();
    let writes_before = grid.dirty_len();
    match feature {
        ConfiguredFeature::SimpleBlock(provider) => {
            place_simple_block(random, pos, provider, grid, tags)
        }
        ConfiguredFeature::Tree(cfg) => {
            place_tree(random, pos, cfg, grid, tags)
        }
        ConfiguredFeature::BlockColumn(cfg) => {
            place_block_column(random, pos, cfg, grid, tags)
        }
        ConfiguredFeature::BlockPile(provider) => {
            features::place_block_pile(random, pos, provider, grid, tags)
        }
        ConfiguredFeature::SculkPatch(cfg) => {
            features::place_sculk_patch(random, pos, cfg, grid, tags)
        }
        ConfiguredFeature::NoOp => {}
        // A no-op: an unmodelled type must still produce a world.
        // `LODESTONE_VEG_STRICT=1` turns it into a panic naming the reason.
        ConfiguredFeature::Unsupported(reason) => {
            assert!(
                !strict_unsupported(),
                "LODESTONE_VEG_STRICT: unmodelled structure-pool feature reached a \
                 placement at {pos:?}: {reason}"
            );

        }
    }
    grid.dirty_len() != writes_before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::IntProvider;
    use crate::rng::LegacyRandomSource;

    fn state(spec: &str) -> lodestone_data::block_states::StateId {
        lodestone_data::block_states::StateId::from_state_str(spec)
            .expect("test state is in the generated table")
    }

    #[test]
    fn writes_outside_chunk_footprint_are_dropped_not_clamped() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        assert!(!grid.set_id_if_in_bounds(-1, 70, 5, state("minecraft:oak_log")));
        assert!(!grid.set_id_if_in_bounds(16, 70, 5, state("minecraft:oak_log")));
        assert!(grid.set_id_if_in_bounds(0, 70, 5, state("minecraft:oak_log")));
        assert!(grid.set_id_if_in_bounds(15, 70, 5, state("minecraft:oak_log")));
    }

    #[test]
    fn dirty_cells_only_reports_in_bounds_writes_in_write_order() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        assert!(!grid.set_id_if_in_bounds(-1, 70, 5, state("minecraft:oak_log")));
        assert!(grid.set_id_if_in_bounds(3, 70, 5, state("minecraft:oak_log")));
        assert!(grid.set_id_if_in_bounds(4, 71, 5, state("minecraft:oak_leaves")));
        let cells: Vec<(i32, i32, i32, String)> = grid
            .dirty_cells()
            .map(|(x, y, z, s)| (x, y, z, s.canonical_state()))
            .collect();
        assert_eq!(
            cells,
            vec![
                (3, 70, 5, "minecraft:oak_log[axis=y]".to_string()),
                (
                    4,
                    71,
                    5,
                    "minecraft:oak_leaves[distance=7,persistent=false,waterlogged=false]".to_string(),
                ),
            ],
            "the out-of-bounds attempt must not appear, and order must match write order"
        );
    }

    #[test]
    fn int_provider_weighted_list_parses_from_placed_feature_shape() {
        let v = serde_json::json!({
            "type": "minecraft:weighted_list",
            "distribution": [{"data": 0, "weight": 19}, {"data": 1, "weight": 1}]
        });
        let parsed = try_parse_int_provider(&v).expect("weighted_list must parse");
        match parsed {
            IntProvider::WeightedProviders(entries) => {
                let scalar_entries: Vec<_> = entries
                    .into_iter()
                    .map(|(provider, weight)| match *provider {
                        IntProvider::Constant(value) => (value, weight),
                        other => panic!("weighted_list scalar data parsed as {other:?}"),
                    })
                    .collect();
                assert_eq!(scalar_entries, vec![(0, 19), (1, 1)]);
            }
            other => panic!("expected WeightedProviders, got {other:?}"),
        }
    }

    #[test]
    fn clamped_normal_offsets_match_compiled_server_samples() {
        let horizontal = try_parse_int_provider(&serde_json::json!({
            "type": "minecraft:clamped_normal",
            "mean": 0.0,
            "deviation": 3.0,
            "min_inclusive": -10,
            "max_inclusive": 10
        })).expect("horizontal clamped-normal provider must parse");
        let vertical = try_parse_int_provider(&serde_json::json!({
            "type": "minecraft:clamped_normal",
            "mean": 0.0,
            "deviation": 0.6,
            "min_inclusive": -2,
            "max_inclusive": 2
        })).expect("vertical clamped-normal provider must parse");
        let samples = [
            (0, 2, 0, 6),
            (1, 4, 0, -3),
            (2, 0, 0, 0),
            (19, 3, 0, 0),
            (42, 3, 0, -2),
        ];
        for (seed, first_horizontal, vertical_offset, second_horizontal) in samples {
            let mut random = LegacyRandomSource::new(seed);
            assert_eq!(
                horizontal.sample(&mut random),
                first_horizontal,
                "first horizontal sample at seed {seed}"
            );
            assert_eq!(
                vertical.sample(&mut random),
                vertical_offset,
                "vertical sample at seed {seed}"
            );
            assert_eq!(
                horizontal.sample(&mut random),
                second_horizontal,
                "cached-Gaussian follow-up at seed {seed}"
            );
        }
    }

    #[test]
    fn would_survive_cactus_requires_supports_cactus_below_and_clear_sides() {
        let mut tags = VegTags::default();
        tags.supports_cactus.insert(Block::Sand);
        tags.bind();
        let pred = BlockPredicate::WouldSurviveCactus;

        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed_id(5, 69, 5, state("minecraft:sand"));
        grid.seed_id(5, 70, 5, state("minecraft:air"));
        grid.seed_id(6, 70, 5, state("minecraft:air"));
        grid.seed_id(4, 70, 5, state("minecraft:air"));
        grid.seed_id(5, 70, 6, state("minecraft:air"));
        grid.seed_id(5, 70, 4, state("minecraft:air"));
        grid.seed_id(5, 71, 5, state("minecraft:air"));
        assert!(
            pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }),
            "sand below, all 4 horizontal neighbours air: must survive"
        );

        // Control: a solid neighbour must fail the check that just passed.
        grid.seed_id(6, 70, 5, state("minecraft:stone"));
        assert!(
            !pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }),
            "a solid horizontal neighbour must block cactus survival"
        );

        // Control: a non-supports_cactus block below must also fail.
        grid.seed_id(6, 70, 5, state("minecraft:air"));
        grid.seed_id(5, 69, 5, state("minecraft:stone"));
        assert!(
            !pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }),
            "stone below (not in supports_cactus) must block cactus survival"
        );
    }

    #[test]
    fn would_survive_sugar_cane_ignores_adjacency_by_design() {
        // See BlockPredicate::WouldSurviveSugarCane's own doc: the
        // water-adjacency half of the cactus block's real-vanilla sibling
        // (the sugar-cane survival check) is deliberately NOT modelled here —
        // every patch_sugar_cane* placed feature re-checks it via an
        // explicit sibling `any_of(matching_fluids)`. This predicate alone
        // must therefore pass on bare sand with NO adjacent water.
        let mut tags = VegTags::default();
        tags.supports_sugar_cane.insert(Block::Sand);
        tags.bind();
        let pred = BlockPredicate::WouldSurviveSugarCane;
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed_id(5, 69, 5, state("minecraft:sand"));
        grid.seed_id(5, 70, 5, state("minecraft:air"));
        assert!(pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }));

        // Control: stone below (not in supports_sugar_cane) must fail.
        grid.seed_id(5, 69, 5, state("minecraft:stone"));
        assert!(!pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }));
    }

    #[test]
    fn matching_fluids_any_of_is_the_real_gate_sugar_cane_relies_on() {
        // The explicit sibling predicate patch_sugar_cane*'s own JSON uses
        // instead of adjacency-in-would_survive (see the test above). This
        // is the control that proves `AnyOf`/`MatchingFluid` actually gate
        // placement rather than defaulting to `True` the way every
        // unrecognised combinator used to (see BlockPredicate::AllOf's doc).
        let pred = BlockPredicate::AnyOf(vec![
            BlockPredicate::MatchingFluid {
                fluids: [Block::Water].into_iter().collect(),
                offset: (1, -1, 0),
            },
            BlockPredicate::MatchingFluid {
                fluids: [Block::Water].into_iter().collect(),
                offset: (-1, -1, 0),
            },
        ]);
        let tags = VegTags::default();
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed_id(5, 69, 5, state("minecraft:sand"));
        grid.seed_id(5, 70, 5, state("minecraft:air"));
        grid.seed_id(6, 69, 5, state("minecraft:sand"));
        assert!(
            !pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }),
            "no adjacent water: must fail"
        );

        grid.seed_id(6, 69, 5, state("minecraft:water"));
        assert!(
            pred.test(&grid, &tags, BlockPos { x: 5, y: 70, z: 5 }),
            "water at offset (1,-1,0): must pass"
        );
    }
}
