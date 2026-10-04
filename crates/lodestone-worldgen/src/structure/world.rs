//! Clipped mutable state views consumed by structure placement.

use lodestone_data::block_states::StateId;

use crate::dense_grid::BaseStateFacts;
use crate::feature::region_view::Overlay;
use crate::feature::vegetation::VegGrid;

use super::StructureMutationSink;
use super::processor::WorldRead;

/// The state and mutation surface shared by dense and sparse structure worlds.
pub trait StructureWorld: WorldRead + std::fmt::Debug + Send {
    /// The origin and dimensions of the accepted write box.
    fn bounds(&self) -> (i32, i32, i32, i32, i32, i32);
    fn get_id(&self, x: i32, y: i32, z: i32) -> StateId;
    fn set_id(&mut self, x: i32, y: i32, z: i32, state: StateId);
    fn base_facts(&self, x: i32, y: i32, z: i32) -> BaseStateFacts;
    fn set_id_observed(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        state: StateId,
        source: (i32, i32),
        step: i32,
        sink: &mut dyn StructureMutationSink,
    );
}

/// One source-step write set over an unchanged outer decoration baseline.
#[derive(Debug)]
pub(crate) struct ClippedStructureWorld<'outer, 'source> {
    outer: &'outer mut VegGrid<'source>,
    writes: Overlay,
    min_x: i32,
    min_y: i32,
    min_z: i32,
    height: i32,
}

impl<'outer, 'source> ClippedStructureWorld<'outer, 'source> {
    pub(crate) fn new(
        outer: &'outer mut VegGrid<'source>,
        source_x: i32,
        source_z: i32,
        min_y: i32,
        height: i32,
    ) -> Self {
        Self {
            outer,
            writes: Overlay::with_bounds(0, 16, min_y, height),
            min_x: source_x * 16,
            min_y,
            min_z: source_z * 16,
            height,
        }
    }

    fn local_key(&self, x: i32, y: i32, z: i32) -> Option<(i32, i32, i32)> {
        let key = (x - self.min_x, y, z - self.min_z);
        ((0..16).contains(&key.0)
            && (self.min_y..self.min_y + self.height).contains(&y)
            && (0..16).contains(&key.2))
            .then_some(key)
    }

    pub(crate) fn finish(mut self) {
        let outer = &mut self.outer;
        let min_x = self.min_x;
        let min_z = self.min_z;
        self.writes.visit_in_yzx_order(|(lx, y, lz), state| {
            let x = min_x + lx;
            let z = min_z + lz;
            if state != outer.get_id(x, y, z) {
                let landed = outer.set_id_if_in_bounds(x, y, z, state);
                debug_assert!(landed, "structure write fell outside the mixed grid");
            }
        });
    }

    #[cfg(test)]
    pub(crate) fn scratch_usage(&self) -> (usize, usize) {
        (self.writes.len(), self.writes.retained_bytes())
    }
}

impl WorldRead for ClippedStructureWorld<'_, '_> {
    fn state_at(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        Some(self.get_id(x, y, z))
    }
}

impl StructureWorld for ClippedStructureWorld<'_, '_> {
    fn bounds(&self) -> (i32, i32, i32, i32, i32, i32) {
        (self.min_x, self.min_y, self.min_z, 16, self.height, 16)
    }

    fn get_id(&self, x: i32, y: i32, z: i32) -> StateId {
        let Some(key) = self.local_key(x, y, z) else { return StateId::AIR };
        self.writes.get_in_bounds(&key).unwrap_or_else(|| self.outer.get_id(x, y, z))
    }

    fn set_id(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        if let Some(key) = self.local_key(x, y, z) {
            self.writes.insert_in_bounds(key, state);
        }
    }

    fn base_facts(&self, x: i32, y: i32, z: i32) -> BaseStateFacts {
        let state = self.get_id(x, y, z);
        BaseStateFacts::Builtin {
            is_air: matches!(state.block(), lodestone_data::block::Block::Air
                | lodestone_data::block::Block::CaveAir | lodestone_data::block::Block::VoidAir),
            is_fluid: lodestone_data::snow_support::has_fluid_state(state),
            blocks_motion: lodestone_data::block_solidity::blocks_motion(state),
        }
    }

    fn set_id_observed(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        state: StateId,
        source: (i32, i32),
        step: i32,
        sink: &mut dyn StructureMutationSink,
    ) {
        if let Some(key) = self.local_key(x, y, z) {
            sink.record_structure_mutation(source, step, [x, y, z], state);
            self.writes.insert_in_bounds(key, state);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lodestone_data::block::Block;

    use super::*;
    use crate::dense_grid::DenseBlockGrid;
    use crate::feature::BlockPos;
    use crate::feature::vegetation::{
        BlockStateProvider, ConfiguredFeature, HeightmapKind, PlacedRef, VegPlacement, VegTags,
    };
    use crate::rng::{LegacyRandomSource, RandomSource};
    use crate::structure::{CodedBlock, StructureMutationContext, StructureMutationRecorder};
    use crate::structure::feature_placement::place_feature_pool_elements_with_sink;
    use crate::structure::pool::{PoolFeaturePlacement, Projection};
    use crate::structure::stronghold::{PostSurfaceWrite, place_post_surface_blocks_with_sink};
    use crate::structure::template::{BlockState, PlaceOrigin, PlaceSettings, StructureTemplate};

    #[test]
    fn nested_structure_groups_snapshot_at_entry_and_read_prior_writes() {
        let stone = Block::Stone.default_state();
        let mut baseline = DenseBlockGrid::with_default(-32, 0, -48, 16, 128, 16, StateId::AIR);
        baseline.set_id(-29, 1, -43, stone);
        let baseline = Arc::new(baseline);
        let mut outer = VegGrid::with_sources(0, 256, -32, -48, 0, 16, |dx, dz| {
            (dx == 0 && dz == 0).then(|| Arc::clone(&baseline))
        });
        assert_eq!(outer.height_world_surface_wg(-29, -43), 2);
        assert_eq!(outer.height_world_surface(-29, -43), 2);
        let placement = |heightmap, block: Block| PoolFeaturePlacement {
            feature: block.name().to_string(),
            placed: Arc::new(PlacedRef {
                registry_id: None,
                placements: vec![VegPlacement::RarityFilter(1), VegPlacement::Heightmap(heightmap)],
                feature: Box::new(ConfiguredFeature::SimpleBlock(BlockStateProvider::Simple(block.default_state()))),
            }),
            origin: BlockPos { x: -29, y: 0, z: -43 },
            projection: Projection::Rigid,
        };
        let mut view = ClippedStructureWorld::new(&mut outer, -2, -3, 0, 128);
        let mut recorder = StructureMutationRecorder::default();
        let mut mutation = StructureMutationContext::new(&mut recorder, (-2, -3), 4);
        mutation.write(&mut view, -29, 4, -43, stone);
        let mut random = LegacyRandomSource::new(91);
        let mut tags = VegTags::default();
        for floor in [Block::Stone, Block::Pumpkin, Block::DiamondBlock, Block::IronBlock] {
            tags.supports_vegetation.insert(floor);
        }
        place_feature_pool_elements_with_sink(&mut random, 91, &[
            placement(HeightmapKind::WorldSurfaceWg, Block::GoldBlock),
            placement(HeightmapKind::WorldSurfaceWg, Block::Pumpkin),
            placement(HeightmapKind::WorldSurface, Block::DiamondBlock),
        ], &mut view, &tags, Some(&mut mutation));
        assert_eq!(view.get_id(-29, 5, -43), Block::Pumpkin.default_state());
        assert_eq!(view.get_id(-29, 6, -43), Block::DiamondBlock.default_state());
        place_feature_pool_elements_with_sink(&mut random, 91, &[
            placement(HeightmapKind::WorldSurfaceWg, Block::IronBlock),
            placement(HeightmapKind::WorldSurface, Block::EmeraldBlock),
        ], &mut view, &tags, Some(&mut mutation));
        assert_eq!(view.get_id(-29, 7, -43), Block::IronBlock.default_state());
        assert_eq!(view.get_id(-29, 8, -43), Block::EmeraldBlock.default_state());
        assert_eq!(random.next_int(), 1_306_595_175, "five 24-bit LCG draws followed by one 32-bit draw");
        assert_eq!(view.outer.height_world_surface_wg(-29, -43), 2);
        assert_eq!(view.outer.height_world_surface(-29, -43), 2);
        let mut stale = VegGrid::with_sources(0, 128, -32, -48, 0, 16, |dx, dz| {
            (dx == 0 && dz == 0).then(|| Arc::clone(&baseline))
        });
        stale.set_id_if_in_bounds(-29, 4, -43, stone);
        assert_ne!(stale.height_world_surface_wg(-29, -43), 5, "outer WG fails the first entry snapshot");
        assert_ne!(stale.height_world_surface(-29, -43), 7, "detached prior-piece state fails the second entry snapshot");
        place_post_surface_blocks_with_sink(&mut view, &[
            PostSurfaceWrite { block: CodedBlock { pos: [-29, 8, -43], state: stone }, only_if_non_air: true },
            PostSurfaceWrite { block: CodedBlock { pos: [-29, 9, -43], state: stone }, only_if_non_air: true },
        ], Some(&mut mutation));
        assert_eq!(view.get_id(-29, 8, -43), stone);
        assert_eq!(view.get_id(-29, 9, -43), StateId::AIR);
        let ladder = StateId::from_state_str("minecraft:ladder[facing=east,waterlogged=false]").unwrap();
        let template = StructureTemplate::from_blocks([1, 1, 1], vec![BlockState { id: ladder }], vec![([0, 0, 0], 0)]);
        let origin = PlaceOrigin { position: [-28, 8, -43], reference: [-28, 8, -43], seed: 91 };
        template.place_with_mutations(origin, &PlaceSettings::default(), &mut view, &mut mutation);
        assert_eq!(view.get_id(-28, 8, -43), ladder);
        mutation.write(&mut view, -29, 8, -43, StateId::AIR);
        template.place_with_mutations(origin, &PlaceSettings::default(), &mut view, &mut mutation);
        assert_eq!(view.get_id(-28, 8, -43), StateId::AIR);
        view.finish();
        assert_eq!(outer.height_world_surface_wg(-29, -43), 2);
        assert_eq!(outer.height_world_surface(-29, -43), 8);
        assert_eq!(outer.dirty_len(), 4);
        assert_eq!(recorder.finish().mutations().iter().map(|write| (write.ordinal, write.position, write.state)).collect::<Vec<_>>(), vec![
            (0, [-29, 4, -43], stone), (1, [-29, 5, -43], Block::GoldBlock.default_state()),
            (2, [-29, 5, -43], Block::Pumpkin.default_state()), (3, [-29, 6, -43], Block::DiamondBlock.default_state()),
            (4, [-29, 7, -43], Block::IronBlock.default_state()), (5, [-29, 8, -43], Block::EmeraldBlock.default_state()),
            (6, [-29, 8, -43], stone), (7, [-28, 8, -43], ladder),
            (8, [-29, 8, -43], StateId::AIR), (9, [-28, 8, -43], ladder),
            (10, [-28, 8, -43], StateId::AIR),
        ]);
    }
}
