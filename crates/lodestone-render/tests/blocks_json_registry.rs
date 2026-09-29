#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use lodestone_data::block_states::{STATE_COUNT, StateId};
use lodestone_model::BlockStateRegistry;
use lodestone_render::BlocksJsonRegistry;

#[test]
#[ignore = "requires official 26.2 and 26.3 generated blocks reports"]
fn official_reports_fill_the_same_canonical_census() {
    for (version, report_count, unsupported, water, oak, button) in [
        ("26.2", 32366, 0, 86, 137, 10771),
        ("26.3", 35723, 3357, 89, 140, 12514),
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../.cache/mc/{version}/generated/reports/blocks.json"));
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
        assert_eq!(canonical.state_count(), 32366);
        assert_eq!(report.input_states, report_count as usize);
        assert_eq!(report.matched_states, 32366);
        assert_eq!(report.unsupported_states, unsupported);
        assert!(report.unsupported_examples.len() <= 4);
        for id in 0..STATE_COUNT {
            let expected = StateId::new(id).unwrap();
            let actual = canonical.resolve(id).unwrap_or_else(|| panic!("{version} canonical hole {id}"));
            assert_eq!(actual.block.to_string(), expected.name(), "{version} canonical slot {id}");
            assert_eq!(actual.properties.len(), expected.properties().len());
            for &(key, value) in expected.properties() {
                assert_eq!(actual.properties.get(key).map(String::as_str), Some(value), "{version} slot {id}, {key}");
            }
        }
        assert_eq!(canonical.resolve(27).unwrap().block.to_string(), "minecraft:bamboo_planks");
        assert!(canonical.resolve(STATE_COUNT).is_none());
        if version == "26.3" {
            assert!(report.unsupported_examples.iter().any(|name| name == "minecraft:poplar_planks"));
        }
    }
}

#[test]
#[ignore = "requires the staged resource archive and official 26.3 blocks report"]
fn canonical_report_drives_fluid_and_axis_model_consumers() {
    use lodestone_assets::{Direction, ResourceManager, ZipSource};
    use lodestone_render::{BlockModels, FluidKind};

    let cache = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/mc");
    let bytes = std::fs::read(cache.join("26.3/generated/reports/blocks.json")).unwrap();
    let (registry, _) = BlocksJsonRegistry::from_slice(&bytes).unwrap().into_canonical();
    let source = ZipSource::open(cache.join("26.2/lodestone-resources.zip")).unwrap();
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
