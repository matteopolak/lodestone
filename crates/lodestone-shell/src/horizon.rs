//! Local, query-only input for the bounded distant-terrain renderer.
//!
//! A running [`lodestone_server::IntegratedServer`] deliberately does not
//! expose its erased [`lodestone_server::ChunkSource`]: it lives on the net
//! thread and asking it for a column would make a visual option grow the normal
//! stream and cache. This module builds a separate immutable estimate only for
//! local Overworld-family presets. Remote servers, custom sources, and other
//! dimensions have no query and therefore draw no coarse horizon.

use lodestone_render::HorizonCell;

use crate::menu::create_world::WorldTypePreset;

/// A local Overworld surface estimate that never materializes a chunk.
///
/// The generator is built once when an eligible integrated world opens. Its
/// [`Self::sample`] path calls only `preliminary_surface_level`, which does not
/// access the staged generated-column store and does not allocate. It is kept
/// separate from the server source so no render-thread action can request,
/// retain, or mutate an ordinary chunk.
pub(crate) struct HorizonSurfaceQuery {
    generator: lodestone_server::OverworldGenerator,
}

impl std::fmt::Debug for HorizonSurfaceQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HorizonSurfaceQuery").finish_non_exhaustive()
    }
}

impl HorizonSurfaceQuery {
    /// Builds an estimate only for the three Overworld generator presets.
    ///
    /// The remaining presets have distinct source semantics (flat, debug, or
    /// fixed-biome) and a persisted custom source can supersede all of them;
    /// declining is safer than showing a plausible but wrong terrain horizon.
    #[must_use]
    pub(crate) fn for_preset(seed: i64, preset: WorldTypePreset) -> Option<Self> {
        let world_type = match preset {
            WorldTypePreset::Normal => lodestone_server::WorldType::Overworld,
            WorldTypePreset::LargeBiomes => lodestone_server::WorldType::LargeBiomes,
            WorldTypePreset::Amplified => lodestone_server::WorldType::Amplified,
            WorldTypePreset::SingleBiomeSurface
            | WorldTypePreset::Flat
            | WorldTypePreset::FlatAllDimensions
            | WorldTypePreset::DebugAllBlockStates => return None,
        };
        Some(Self {
            generator: lodestone_server::overworld_generator_of_type(seed, world_type),
        })
    }

    /// Samples one 16-by-16-block coarse cell at its world-space origin.
    ///
    /// The fixed colours are intentionally conservative until a no-allocation
    /// biome-colour query is available. Height and sea level come from the
    /// same local generator configuration as the eligible server source;
    /// colour never triggers a biome-name allocation on this hot path.
    #[must_use]
    pub(crate) fn sample(&self, block_x: i32, block_z: i32) -> HorizonCell {
        const LAND_RGB565: u16 = 0x5A85;
        const WATER_RGB565: u16 = 0x2D9B;
        let terrain = self.generator.preliminary_surface_level(block_x, block_z);
        let water = (terrain < self.generator.sea_level()).then_some(self.generator.sea_level());
        HorizonCell {
            terrain_y: terrain.saturating_add(64).clamp(0, i32::from(u16::MAX)) as u16,
            water_y: water
                .map(|y| y.saturating_add(64).clamp(0, i32::from(u16::MAX)) as u16)
                .unwrap_or(HorizonCell::DRY),
            surface_rgb565: if water.is_some() {
                WATER_RGB565
            } else {
                LAND_RGB565
            },
            flags: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn generated_column_count(&self) -> usize {
        self.generator.store_len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eligible_overworld_query_never_enters_the_generated_column_store() {
        let query = HorizonSurfaceQuery::for_preset(42, WorldTypePreset::Normal)
            .expect("normal worlds have a local Overworld query");
        let before = query.generated_column_count();
        let sample = query.sample(-16_384, 16_384);
        assert_ne!(sample.terrain_y, HorizonCell::EMPTY.terrain_y);
        assert_eq!(
            query.generated_column_count(),
            before,
            "the horizon estimate must not stream or cache a full chunk"
        );
    }

    #[test]
    fn custom_source_presets_decline_the_overworld_estimate() {
        for preset in [
            WorldTypePreset::SingleBiomeSurface,
            WorldTypePreset::Flat,
            WorldTypePreset::FlatAllDimensions,
            WorldTypePreset::DebugAllBlockStates,
        ] {
            assert!(HorizonSurfaceQuery::for_preset(42, preset).is_none());
        }
    }
}
