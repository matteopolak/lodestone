//! Controls for the Nether's admission-dependent source ordering.

use lodestone_worldgen::stage_schedule::{ChunkRequest, NETHER_SOURCES};

#[test]
fn production_schedule_matches_request_plan() {
    let request = ChunkRequest::single(32, 48, 2);
    let planned = request.source_completion_order((32, 48)).offsets();

    assert_eq!(
        NETHER_SOURCES.order_for(request, (32, 48)),
        planned,
        "the production source schedule must delegate to the request plan",
    );
}

#[test]
fn request_boundary_is_a_negative_control_for_a_fixed_permutation() {
    let inside_tile = ChunkRequest::single(32, 48, 2);
    let across_tile_boundary = ChunkRequest::new(2, 16, 2, 16, 2);
    let inside = NETHER_SOURCES.order_for(inside_tile, (32, 48));
    let crossing = NETHER_SOURCES.order_for(across_tile_boundary, (16, 16));

    assert_ne!(
        inside, crossing,
        "an admission-dependent schedule must not collapse to one global source permutation",
    );
    assert_ne!(
        crossing,
        inside_tile.source_completion_order((32, 48)).offsets(),
        "the tile-boundary control must reject reusing the first request's order",
    );
}
