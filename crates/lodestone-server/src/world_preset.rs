//! The world-type presets a new singleplayer world can be created with.

use std::sync::Arc;

use crate::{
    ChunkSource, WorldType, WorldgenScope, WorldgenScopeMismatch, bundled_worldgen_serves,
    debug_chunk_source, flat_chunk_source, overworld_chunk_source_of_type,
    single_biome_chunk_source, world_preset_flat_settings, world_preset_single_biome_default_biome,
};

/// One of the seven bundled world generators, selectable by a world-creation UI
/// or by the browser worker's launch message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorldPreset {
    #[default]
    Normal,
    LargeBiomes,
    Amplified,
    SingleBiomeSurface,
    Flat,
    FlatAllDimensions,
    DebugAllBlockStates,
}

impl WorldPreset {
    /// Every preset, in wire-id order.
    pub const ALL: [Self; 7] = [
        Self::Normal,
        Self::LargeBiomes,
        Self::Amplified,
        Self::SingleBiomeSurface,
        Self::Flat,
        Self::FlatAllDimensions,
        Self::DebugAllBlockStates,
    ];

    /// The id the page sends the browser worker. It is the index in [`Self::ALL`].
    #[must_use]
    pub fn wire_id(self) -> u8 {
        Self::ALL.iter().position(|p| *p == self).map_or(0, |i| i as u8)
    }

    #[must_use]
    pub fn from_wire_id(id: u8) -> Option<Self> {
        Self::ALL.get(usize::from(id)).copied()
    }

    /// Builds this preset's generator, returning it with its `(min_y, height)`.
    ///
    /// `scope` is the hosting protocol's declared worldgen scope. Every preset
    /// builds from the one embedded bundle, so every preset is refused when the
    /// scope is not the one the bundle serves.
    ///
    /// # Errors
    ///
    /// The requested scope, when the bundle does not serve it.
    pub fn chunk_source(
        self,
        scope: WorldgenScope,
        seed: i64,
    ) -> Result<(Arc<dyn ChunkSource>, i32, i32), WorldgenScopeMismatch> {
        if !bundled_worldgen_serves(scope) {
            return Err(WorldgenScopeMismatch { requested: scope });
        }
        let overworld = crate::dimension::Dimension::Overworld;
        Ok(match self {
            Self::Normal | Self::LargeBiomes | Self::Amplified => {
                let world_type = match self {
                    Self::LargeBiomes => WorldType::LargeBiomes,
                    Self::Amplified => WorldType::Amplified,
                    _ => WorldType::Overworld,
                };
                let s = overworld_chunk_source_of_type(seed, world_type);
                let (min_y, height) = (s.min_y(), s.height());
                (Arc::new(s), min_y, height)
            }
            Self::SingleBiomeSurface => {
                let biome = world_preset_single_biome_default_biome();
                let s = single_biome_chunk_source(seed, &biome);
                let (min_y, height) = (s.min_y(), s.height());
                (Arc::new(s), min_y, height)
            }
            Self::Flat | Self::FlatAllDimensions => {
                let all_dimensions = self == Self::FlatAllDimensions;
                let s = flat_chunk_source(world_preset_flat_settings(all_dimensions));
                (Arc::new(s), overworld.min_y(), overworld.height())
            }
            Self::DebugAllBlockStates => {
                (Arc::new(debug_chunk_source()), overworld.min_y(), overworld.height())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_ids_round_trip_and_reject_unknown() {
        for (id, preset) in WorldPreset::ALL.into_iter().enumerate() {
            assert_eq!(preset.wire_id() as usize, id);
            assert_eq!(WorldPreset::from_wire_id(id as u8), Some(preset));
        }
        assert_eq!(WorldPreset::from_wire_id(7), None);
    }

    /// Every preset refuses a scope the bundle does not serve.
    #[test]
    fn every_preset_refuses_a_scope_mismatch() {
        let mismatched = WorldgenScope::None;
        for preset in WorldPreset::ALL {
            let Err(err) = preset.chunk_source(mismatched, 1) else {
                panic!("{preset:?} built a chunk source despite a scope mismatch");
            };
            assert_eq!(err, WorldgenScopeMismatch { requested: mismatched });
        }
    }

    /// Control for the refusal test: a matching scope builds every preset.
    #[test]
    fn every_preset_builds_when_the_scope_matches() {
        for preset in WorldPreset::ALL {
            assert!(
                preset.chunk_source(crate::BUNDLED_WORLDGEN_SCOPE, 1).is_ok(),
                "{preset:?} must build when the scope matches the bundle"
            );
        }
    }
}
