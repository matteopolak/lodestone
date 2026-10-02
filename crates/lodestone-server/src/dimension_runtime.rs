//! World-owned entity simulation and publications, memoized per dimension.

use std::sync::{Arc, OnceLock};

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
    /// Adopts existing handles without replacing their population or publication.
    #[must_use]
    pub fn new(dimension: Dimension, mobs: MobHandle, entities: LiveMobSource) -> Self {
        Self { dimension, mobs, entities }
    }

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
    dimensions: [OnceLock<Arc<DimensionRuntime>>; 3],
    pub(crate) players: PlayerRegistry,
    pub(crate) anchors: TickAnchors,
}

impl Default for WorldRuntime {
    fn default() -> Self {
        let players = PlayerRegistry::new();
        Self {
            dimensions: std::array::from_fn(|_| OnceLock::new()),
            anchors: TickAnchors::from_players(players.clone()),
            players,
        }
    }
}

impl WorldRuntime {
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

    pub(crate) fn install(&self, runtime: DimensionRuntime) -> Arc<DimensionRuntime> {
        Arc::clone(self.slot(runtime.dimension()).get_or_init(|| Arc::new(runtime)))
    }

    pub(crate) fn ensure(&self, dimension: Dimension) -> Arc<DimensionRuntime> {
        Arc::clone(self.slot(dimension).get_or_init(|| {
            Arc::new(DimensionRuntime::new(
                dimension,
                MobHandle::new(ChunkWorld::new(dimension.min_y(), dimension.height())),
                LiveMobSource::default(),
            ))
        }))
    }
}
