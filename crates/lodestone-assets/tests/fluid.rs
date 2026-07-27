//! Hermetic tests for the fluid module.
//!
//! Fluid rendering is *not* a per-state model bake — corner heights and flow
//! direction depend on the neighbouring fluid cells, which only the mesher can
//! supply. These tests exercise the pure, hermetic core: own/render heights,
//! corner-height averaging from explicit neighbour heights, the verified
//! `getFlow` horizontal vector, still-vs-flowing texture selection, and the
//! flow-angle used to rotate the flowing texture.

use lodestone_assets::fluid::{
    FlowNeighbor, FluidState, FluidTexture, corner_height, flow_angle, flow_horizontal,
    neighbor_height, select_texture,
};

const SOURCE: f32 = 8.0 / 9.0;

#[test]
fn own_height_is_amount_over_nine() {
    // Verified from FlowingFluid.getOwnHeight = amount / 9.
    assert_eq!(FluidState::source().own_height(), SOURCE);
    assert_eq!(FluidState::new(8, false).own_height(), 8.0 / 9.0);
    assert_eq!(FluidState::new(1, false).own_height(), 1.0 / 9.0);
}

#[test]
fn render_height_is_full_when_same_fluid_above() {
    // Verified from FlowingFluid.getHeight = hasSameAbove ? 1.0 : ownHeight.
    let s = FluidState::source();
    assert_eq!(s.render_height(false), SOURCE);
    assert_eq!(s.render_height(true), 1.0);
}

#[test]
fn neighbor_height_distinguishes_air_from_solid() {
    // Verified FluidRenderer.getHeight: same fluid → 1 (above) / own; different
    // fluid → 0 if non-solid (air, averaged in), -1 if solid (excluded).
    assert_eq!(neighbor_height(true, true, SOURCE, false), 1.0);
    assert_eq!(neighbor_height(true, false, SOURCE, false), SOURCE);
    assert_eq!(neighbor_height(false, false, 0.0, false), 0.0);
    assert_eq!(neighbor_height(false, false, 0.0, true), -1.0);
}

#[test]
fn corner_height_of_uniform_source_is_source_height() {
    // Four source cells (self, two edges, diagonal), none full → weighted average
    // is just the source height.
    let h = corner_height(SOURCE, SOURCE, SOURCE, SOURCE);
    assert!((h - SOURCE).abs() < 1e-6, "got {h}");
}

#[test]
fn corner_height_snaps_to_one_when_edge_is_full_column() {
    // An edge at render-height 1.0 (full column) pulls the corner to the top.
    assert_eq!(corner_height(SOURCE, 1.0, -1.0, -1.0), 1.0);
}

#[test]
fn corner_height_excludes_solid_cells() {
    // Solid neighbours (-1.0) are skipped; only the fluid itself contributes.
    let h = corner_height(SOURCE, -1.0, -1.0, -1.0);
    assert!((h - SOURCE).abs() < 1e-6, "got {h}");
}

#[test]
fn corner_height_averages_in_air_pulling_corner_down() {
    // Air (0.0) is included with weight 1 (unlike solid), lowering the corner.
    // self=SOURCE(w10), edge_a=SOURCE(w10, triggers diagonal), edge_b=air(w1),
    // diagonal=air(w1): (SOURCE*20 + 0 + 0) / 22.
    let h = corner_height(SOURCE, SOURCE, 0.0, 0.0);
    let expect = SOURCE * 20.0 / 22.0;
    assert!((h - expect).abs() < 1e-6, "got {h} want {expect}");
    assert!(
        h < SOURCE,
        "air should pull the corner below the source height"
    );
}

#[test]
fn corner_height_weights_tall_cells_heavily() {
    // A shallow neighbour (< 0.8) barely moves a corner surrounded by tall fluid,
    // matching vanilla's 10:1 weighting. Diagonal solid (-1) is excluded.
    let shallow = 3.0 / 9.0;
    let weighted = corner_height(SOURCE, SOURCE, shallow, -1.0);
    // (SOURCE*10 + SOURCE*10 + shallow*1) / 21.
    let expect = (SOURCE * 10.0 + SOURCE * 10.0 + shallow) / 21.0;
    assert!(
        (weighted - expect).abs() < 1e-6,
        "got {weighted} want {expect}"
    );
}

#[test]
fn still_fluid_selects_still_texture() {
    // No neighbours → no flow → still texture, angle irrelevant.
    let flow = flow_horizontal(SOURCE, none(), none(), none(), none());
    assert_eq!(flow, [0.0, 0.0]);
    assert_eq!(select_texture(flow), FluidTexture::Still);
}

#[test]
fn flow_points_from_high_to_low() {
    // Verified getFlow: distance = ownHeight - neighborHeight, summed over
    // dir.step. Only the east neighbour is lower, so flow runs +X (east).
    let low_east = FlowNeighbor {
        own_height: 3.0 / 9.0,
        blocks_motion: false,
        below_own_height: 0.0,
    };
    let flow = flow_horizontal(SOURCE, none(), none(), low_east, none());
    assert_eq!(select_texture(flow), FluidTexture::Flowing);
    assert!(flow[0] > 0.0, "should flow +x (east), got {flow:?}");
    assert!(flow[1].abs() < 1e-9, "no z component, got {flow:?}");
    // Normalised.
    let mag = (flow[0] * flow[0] + flow[1] * flow[1]).sqrt();
    assert!(
        (mag - 1.0).abs() < 1e-9,
        "flow should be unit length, got {mag}"
    );
}

#[test]
fn flow_falls_off_ledge_via_below_neighbour() {
    // Neighbour cell is empty (own_height 0) and passable, but the cell below it
    // holds fluid: verified getFlow reaches down, distance = own - (below-0.888).
    let ledge = FlowNeighbor {
        own_height: 0.0,
        blocks_motion: false,
        below_own_height: SOURCE,
    };
    let flow = flow_horizontal(SOURCE, none(), none(), ledge, none());
    // distance = 0.888 - (0.888 - 0.888) = 0.888 > 0 → flow east toward the drop.
    assert!(flow[0] > 0.0, "should flow toward the ledge, got {flow:?}");
}

#[test]
fn flow_angle_of_east_flow_is_zero_reference() {
    // atan2(z, x) with pure +x flow → 0 radians before the vanilla -pi/2 shift.
    let a = flow_angle([1.0, 0.0]);
    assert!((a - (-std::f32::consts::FRAC_PI_2)).abs() < 1e-6, "got {a}");
}

fn none() -> FlowNeighbor {
    FlowNeighbor {
        own_height: 0.0,
        blocks_motion: true,
        below_own_height: 0.0,
    }
}

use lodestone_assets::Direction;
use lodestone_assets::fluid::{FaceSet, FluidGeometry, SpriteUv, bake_fluid};

fn uv(a: f32, b: f32, c: f32, d: f32) -> SpriteUv {
    SpriteUv {
        min: [a, b],
        max: [c, d],
    }
}

#[test]
fn baked_top_face_carries_corner_heights_and_tint() {
    let geom = FluidGeometry {
        corners: [SOURCE, SOURCE, 0.5, 0.5], // sloped toward +Z
        flow: [0.0, 0.0],
        faces: FaceSet {
            up: true,
            down: false,
            north: false,
            south: false,
            east: false,
            west: false,
        },
        tint_index: Some(0),
    };
    let quads = bake_fluid(&geom, uv(0.0, 0.0, 0.5, 0.5), uv(0.5, 0.0, 1.0, 0.5));
    assert_eq!(quads.len(), 1);
    let top = &quads[0];
    assert_eq!(top.direction, Direction::Up);
    assert_eq!(top.tint_index, Some(0));
    // Vanilla winding NW, SW, SE, NE. corners = [nw, ne, se, sw].
    assert_eq!(top.positions[0], [0.0, SOURCE, 0.0]); // NW
    assert_eq!(top.positions[1], [0.0, 0.5, 1.0]); // SW
    assert_eq!(top.positions[2], [1.0, 0.5, 1.0]); // SE
    assert_eq!(top.positions[3], [1.0, SOURCE, 0.0]); // NE
    // Still surface → UVs from the still sprite rect (SE corner is 0.5,0.5).
    assert_eq!(top.uvs[2], [0.5, 0.5]);
}

#[test]
fn flowing_top_face_uses_flow_sprite() {
    let still = uv(0.0, 0.0, 0.5, 0.5);
    let flow = uv(0.5, 0.0, 1.0, 0.5);
    let geom = FluidGeometry {
        corners: [SOURCE; 4],
        flow: [1.0, 0.0], // flowing east
        faces: FaceSet {
            up: true,
            down: false,
            north: false,
            south: false,
            east: false,
            west: false,
        },
        tint_index: None,
    };
    let quads = bake_fluid(&geom, still, flow);
    // Every top UV should land inside the flow sprite's U range [0.5, 1.0].
    for [u, _] in quads[0].uvs {
        assert!((0.5..=1.0).contains(&u), "flow UV u={u} out of flow sprite");
    }
}

#[test]
fn full_cell_emits_six_faces_no_cullface() {
    let geom = FluidGeometry {
        corners: [1.0; 4],
        flow: [0.0, 0.0],
        faces: FaceSet::default(),
        tint_index: Some(0),
    };
    let quads = bake_fluid(&geom, uv(0.0, 0.0, 1.0, 1.0), uv(0.0, 0.0, 1.0, 1.0));
    assert_eq!(quads.len(), 6);
    // Fluids are culled by the mesher via FaceSet, so no quad carries a cullface.
    assert!(quads.iter().all(|q| q.cullface.is_none()));
    // All six geometric directions are present exactly once.
    for dir in [
        Direction::Up,
        Direction::Down,
        Direction::North,
        Direction::South,
        Direction::East,
        Direction::West,
    ] {
        assert_eq!(quads.iter().filter(|q| q.direction == dir).count(), 1);
    }
}

#[test]
fn side_face_uses_left_half_of_flow_texture() {
    // Vanilla side faces sample u in [0, 0.5] and v scaled by height.
    let geom = FluidGeometry {
        corners: [SOURCE; 4],
        flow: [0.0, 0.0],
        faces: FaceSet {
            up: false,
            down: false,
            north: true,
            south: false,
            east: false,
            west: false,
        },
        tint_index: Some(0),
    };
    // Flow sprite occupies full [0,1] rect so at(u,v) == (u,v).
    let quads = bake_fluid(&geom, uv(0.0, 0.0, 1.0, 1.0), uv(0.0, 0.0, 1.0, 1.0));
    assert_eq!(quads.len(), 1);
    let side = &quads[0];
    // u only reaches 0.5 (left half); v spans (1-h)*0.5 .. 0.5.
    for [u, _] in side.uvs {
        assert!(u <= 0.5 + 1e-6, "side u={u} should stay in the left half");
    }
    // Top verts sit at v=(1-SOURCE)*0.5; bottom verts at v=0.5.
    let top_v = (1.0 - SOURCE) * 0.5;
    assert!((side.uvs[0][1] - top_v).abs() < 1e-6);
    assert!((side.uvs[2][1] - 0.5).abs() < 1e-6);
}
