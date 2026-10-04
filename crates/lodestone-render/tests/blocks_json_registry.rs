#![cfg(not(target_arch = "wasm32"))]

use lodestone_data::block_states::{STATE_COUNT, StateId};
use lodestone_model::BlockStateRegistry;
use lodestone_render::BlocksJsonRegistry;

/// The canonical state space is the union of 26.2 and 26.3 (26.2's ids first), so
/// 26.3's report fills it completely and 26.2's leaves exactly the states 26.3
/// added as holes.
///
/// Expected figures come from counting the two official `blocks.json` files
/// directly, not from this crate: 26.2 has 32366 states, 26.3 has 35723, and
/// the report ids of the water source, the y-axis oak log and one acacia button
/// are read off each report.
#[test]
#[ignore = "requires official 26.2 and 26.3 generated blocks reports"]
fn official_reports_fill_the_union_canonical_census() {
    for (version, report_count, water, oak, button) in
        [("26.2", 32366u32, 86, 137, 10771), ("26.3", 35723, 89, 140, 12514)]
    {
        let path = lodestone_mc_cache::version_root(version).join("generated/reports/blocks.json");
        let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let raw = BlocksJsonRegistry::from_slice(&bytes).expect("official report parses");
        assert_eq!(raw.state_count(), report_count);
        for (id, name, key, value) in [
            (water, "minecraft:water", "level", "0"),
            (oak, "minecraft:oak_log", "axis", "y"),
            (button, "minecraft:acacia_button", "face", "floor"),
        ] {
            let state = raw.resolve(id).expect("captured report state exists");
            assert_eq!(state.block.to_string(), name, "{version} report slot {id}");
            assert_eq!(state.properties.get(key).map(String::as_str), Some(value));
        }

        let (canonical, report) = raw.into_canonical();
        assert_eq!(canonical.state_count(), STATE_COUNT);
        assert_eq!(report.input_states, report_count as usize);
        assert_eq!(report.matched_states, report_count as usize);
        assert_eq!(report.unsupported_states, 0, "{version}: every report state is in the union");
        assert!(report.unsupported_examples.is_empty());
        let mut holes = 0u32;
        for id in 0..STATE_COUNT {
            let expected = StateId::new(id).unwrap();
            let Some(actual) = canonical.resolve(id) else {
                holes += 1;
                continue;
            };
            assert_eq!(actual.block.to_string(), expected.name(), "{version} canonical slot {id}");
            assert_eq!(actual.properties.len(), expected.properties().len());
            for &(key, value) in expected.properties() {
                assert_eq!(actual.properties.get(key).map(String::as_str), Some(value), "{version} slot {id}, {key}");
            }
        }
        assert_eq!(holes, STATE_COUNT - report_count, "{version}: holes are the other release's states");
        assert_eq!(canonical.resolve(27).unwrap().block.to_string(), "minecraft:bamboo_planks");
        assert!(canonical.resolve(STATE_COUNT).is_none());
        let poplar_present = (0..STATE_COUNT)
            .filter_map(|id| canonical.resolve(id))
            .any(|state| state.block.to_string() == "minecraft:poplar_planks");
        assert_eq!(poplar_present, version == "26.3", "poplar planks exist only in 26.3");
    }
}

#[test]
#[ignore = "requires the staged resource archive and official 26.3 blocks report"]
fn canonical_report_drives_fluid_and_axis_model_consumers() {
    use lodestone_assets::{Direction, ResourceManager, ZipSource};
    use lodestone_render::{BlockModels, FluidKind};

    let latest = lodestone_mc_cache::version_root("26.3");
    let bytes = std::fs::read(latest.join("generated/reports/blocks.json")).unwrap();
    let (registry, _) = BlocksJsonRegistry::from_slice(&bytes).unwrap().into_canonical();
    let source = ZipSource::open(latest.join("lodestone-resources.zip")).unwrap();
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let models = BlockModels::build_with_mip_levels(&manager, &registry, 0).unwrap();

    assert_eq!(models.fluid(StateId::new(86).unwrap()).unwrap().kind, FluidKind::Water);
    let log = models.state(StateId::new(137).unwrap());
    assert_eq!(log.quads.len(), 6);
    for quad in &log.quads {
        let expected = match quad.direction {
            Direction::Up | Direction::Down => "minecraft:block/oak_log_top",
            _ => "minecraft:block/oak_log",
        };
        assert_eq!(models.atlas().sprites()[quad.sprite as usize].location.to_string(), expected);
    }
}
