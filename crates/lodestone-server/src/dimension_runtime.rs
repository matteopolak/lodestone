//! World-owned entity simulation and publications, memoized per dimension.

use std::sync::{Arc, OnceLock};

use lodestone_data::GameDataVersion;

use crate::dimension::Dimension;
use crate::mobs::{ChunkWorld, LiveMobSource, MobHandle};
use crate::players::PlayerRegistry;
use crate::tick_area::TickAnchors;

/// One dimension's mutable population and its connection-facing publication.
#[derive(Debug)]
pub struct DimensionRuntime {
    dimension: Dimension,
    mobs: MobHandle,
    entities: LiveMobSource,
}

impl DimensionRuntime {
    #[must_use]
    pub fn dimension(&self) -> Dimension { self.dimension }

    #[must_use]
    pub fn mobs(&self) -> &MobHandle { &self.mobs }

    #[must_use]
    pub fn entities(&self) -> &LiveMobSource { &self.entities }

    /// Publishes the current population after a host-owned simulation mutation.
    pub fn publish_entities(&self) {
        let (entities, bars) = self.mobs.with(|sim| (sim.snapshots(), sim.boss_bars()));
        self.entities.publish(entities);
        self.entities.publish_boss_bars(bars);
    }
}

#[derive(Debug)]
pub(crate) struct WorldRuntime {
    /// The release every dimension of this world plays under.
    release: GameDataVersion,
    dimensions: [OnceLock<Arc<DimensionRuntime>>; 3],
    pub(crate) players: PlayerRegistry,
    pub(crate) anchors: TickAnchors,
}

impl Default for WorldRuntime {
    /// A 26.2 world, for embeds that host no protocol.
    fn default() -> Self {
        Self::new(GameDataVersion::V26_2)
    }
}

impl WorldRuntime {
    pub(crate) fn new(release: GameDataVersion) -> Self {
        let players = PlayerRegistry::new();
        Self {
            release,
            dimensions: std::array::from_fn(|_| OnceLock::new()),
            anchors: TickAnchors::from_players(players.clone()),
            players,
        }
    }

    pub(crate) fn release(&self) -> GameDataVersion {
        self.release
    }

    fn slot(&self, dimension: Dimension) -> &OnceLock<Arc<DimensionRuntime>> {
        &self.dimensions[match dimension {
            Dimension::Overworld => 0,
            Dimension::Nether => 1,
            Dimension::End => 2,
        }]
    }

    pub(crate) fn get(&self, dimension: Dimension) -> Option<Arc<DimensionRuntime>> {
        self.slot(dimension).get().cloned()
    }

    pub(crate) fn ensure(&self, dimension: Dimension) -> Arc<DimensionRuntime> {
        Arc::clone(self.slot(dimension).get_or_init(|| {
            Arc::new(DimensionRuntime {
                dimension,
                mobs: MobHandle::for_release(ChunkWorld::new(dimension.min_y(), dimension.height()), self.release),
                entities: LiveMobSource::default(),
            })
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_state::WorldStateHandle;

    /// Every dimension of a world, lazily created siblings included, plays under
    /// the release the world was built for, and a replaced terrain snapshot keeps it.
    #[test]
    fn every_dimension_runtime_follows_the_worlds_release() {
        for release in [GameDataVersion::V26_2, GameDataVersion::V26_3] {
            let world = WorldStateHandle::for_release(release);
            assert_eq!(world.release(), release);
            for dimension in [Dimension::Overworld, Dimension::Nether, Dimension::End] {
                let runtime = world.ensure_dimension_runtime(dimension);
                assert_eq!(runtime.mobs().release(), release, "{dimension:?} {release:?}");
                runtime.mobs().replace_world(ChunkWorld::new(dimension.min_y(), dimension.height()));
                assert_eq!(runtime.mobs().release(), release, "{dimension:?} after replace_world");
            }
        }
    }
}
