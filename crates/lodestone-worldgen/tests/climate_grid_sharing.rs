//! Production control for request-scoped climate-grid sharing.

#![cfg(feature = "gen-counters")]

use lodestone_worldgen::counters;

#[test]
fn one_shaped_request_prepares_one_grid_for_biomes_and_surface() {
    let generator = lodestone_server::overworld_generator(42);
    counters::reset();

    std::hint::black_box(generator.column_shaped(0, 0).non_air_count());

    let snapshot = counters::snapshot();
    assert_eq!(
        snapshot.climate_grid_preparations, 1,
        "biome cells and surface must share one bordered request grid: {snapshot:?}"
    );
}
