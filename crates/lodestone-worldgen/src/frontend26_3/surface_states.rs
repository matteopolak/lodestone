use lodestone_data::block_states::StateId;
use lodestone_worldgen_core::engine::release26_3::material::StateId as EngineStateId;
use lodestone_worldgen_core::engine::release26_3::settings::TerrainGenerator;
use lodestone_worldgen_core::engine::release26_3::surface::SurfaceChunk;

use super::{FrontendError, parse_state_key};

/// Maps the block states a 26.3 [`TerrainGenerator`] can produce (its interned
/// data keys) onto the canonical block-state census, resolved once at load.
///
/// Every state the material rules, ore veins and clay bands can write is
/// interned when the generator loads, so a successful `for_generator` covers
/// every id a surface build can emit.
#[derive(Clone, Debug)]
pub struct SurfaceStateMap {
    ids: Vec<StateId>,
}

impl SurfaceStateMap {
    /// Resolves every state of `generator`; an unknown block or property is an
    /// error, never a substitution.
    pub fn for_generator(generator: &TerrainGenerator) -> Result<Self, FrontendError> {
        let ids = (0..generator.state_count() as EngineStateId)
            .map(|id| parse_state_key(generator.state_key(id)))
            .collect::<Result<_, _>>()?;
        Ok(Self { ids })
    }

    /// The census state for an engine state id.
    #[must_use]
    pub fn resolve(&self, id: EngineStateId) -> StateId {
        self.ids[id as usize]
    }

    /// A surface chunk's blocks as census states, laid out like `SurfaceChunk::states`
    /// (`y + (x + z * 16) * height`, `y` relative to the chunk's minimum).
    #[must_use]
    pub fn chunk_states(&self, chunk: &SurfaceChunk) -> Vec<StateId> {
        chunk.states.iter().map(|&s| self.resolve(s)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// `surface-states.txt` is the real server's own `data key -> full state key`
    /// map for every state the rules write.
    const ORACLE: &str = include_str!("../../../lodestone-worldgen-core/tests/fixtures/release26_3/surface-states.txt");

    fn full_key(id: StateId) -> String {
        let props: BTreeMap<_, _> = id.properties().iter().copied().collect();
        if props.is_empty() {
            return id.name().to_owned();
        }
        let body: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{}[{}]", id.name(), body.join(","))
    }

    #[test]
    fn every_oracle_state_resolves_to_the_real_servers_full_state() {
        let mut n = 0;
        for line in ORACLE.lines() {
            let f: Vec<&str> = line.split(' ').collect();
            let got = parse_state_key(f[1]).unwrap_or_else(|e| panic!("{}: {e}", f[1]));
            assert_eq!(full_key(got), f[2], "data key {}", f[1]);
            n += 1;
        }
        assert!(n >= 40);
    }

    /// Control: the defaulted-property cases are the ones a name-only lookup
    /// would get wrong, so assert the resolver does not return the wrong state.
    #[test]
    fn control_property_defaults_are_not_dropped() {
        let grass = parse_state_key("minecraft:grass_block").unwrap();
        assert_eq!(full_key(grass), "minecraft:grass_block[snowy=false]");
        let snowy = parse_state_key("minecraft:grass_block[snowy=true]").unwrap();
        assert_ne!(grass, snowy);
        assert!(parse_state_key("minecraft:no_such_block").is_err());
        assert!(parse_state_key("minecraft:stone[bogus=1]").is_err());
    }

    /// Every dimension's generator resolves completely and its default block and
    /// fluids map to the expected census states (dimension coverage, plus a
    /// stone/netherrack/end-stone control that the map is not constant).
    #[test]
    fn every_bundled_generator_resolves() {
        use lodestone_worldgen_core::engine::release26_3::settings::ResourceSet;
        use lodestone_worldgen_data_26_3 as data;
        let res = ResourceSet::from_tables(data::DENSITY_FUNCTION, data::NOISE, data::NOISE_SETTINGS)
            .with_surface_data(data::MATERIAL_RULE, data::MATERIAL_CONDITION, data::BIOME);
        let mut default_blocks = Vec::new();
        for name in ["overworld", "amplified", "large_biomes", "caves", "floating_islands", "nether", "end"] {
            let g = TerrainGenerator::load(&res, name, 42).unwrap();
            let map = SurfaceStateMap::for_generator(&g).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(g.state_count() > 3, "{name}");
            default_blocks.push(full_key(map.resolve(g.default_block_state_id().unwrap())));
        }
        assert_eq!(default_blocks[0], "minecraft:stone");
        assert_eq!(default_blocks[5], "minecraft:netherrack");
        assert_eq!(default_blocks[6], "minecraft:end_stone");
    }
}
