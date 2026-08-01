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
use lodestone_assets::fluid::{FaceSet, FluidGeometry, SideOverlay, SpriteUv, bake_fluid};

fn uv(a: f32, b: f32, c: f32, d: f32) -> SpriteUv {
    SpriteUv {
        min: [a, b],
        max: [c, d],
        anim: 0,
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
        back_up_face: false,
        side_overlay: SideOverlay::default(),
    };
    let quads = bake_fluid(&geom, uv(0.0, 0.0, 0.5, 0.5), uv(0.5, 0.0, 1.0, 0.5), None);
    assert_eq!(quads.len(), 1);
    let top = &quads[0];
    assert_eq!(top.direction, Direction::Up);
    assert_eq!(top.tint_index, Some(0));
    // Vanilla winding NW, SW, SE, NE. corners = [nw, ne, se, sw]. The top face
    // draws, so vanilla's `~0.001` z-fight inset pulls every corner down.
    const EPS: f32 = 0.001;
    assert_eq!(top.positions[0], [0.0, SOURCE - EPS, 0.0]); // NW
    assert_eq!(top.positions[1], [0.0, 0.5 - EPS, 1.0]); // SW
    assert_eq!(top.positions[2], [1.0, 0.5 - EPS, 1.0]); // SE
    assert_eq!(top.positions[3], [1.0, SOURCE - EPS, 0.0]); // NE
    // Still surface → UVs from the still sprite rect (SE corner is 0.5,0.5).
    assert_eq!(top.uvs[2], [0.5, 0.5]);
}

#[test]
fn top_face_gets_no_inset_when_not_drawn_and_sides_read_the_uninset_height() {
    // The `~0.001` corner inset only happens inside vanilla's `if (renderUp &&
    // !occluded)` block — so a side face drawn *without* the top face (e.g. the
    // top is occluded by a solid block) must use the raw, un-inset corner
    // height, not the top face's adjusted one.
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
        back_up_face: false,
        side_overlay: SideOverlay::default(),
    };
    let quads = bake_fluid(&geom, uv(0.0, 0.0, 1.0, 1.0), uv(0.0, 0.0, 1.0, 1.0), None);
    // North without the top face still emits front + back (flow, not overlay).
    assert_eq!(quads.len(), 2);
    let front = &quads[0];
    assert_eq!(front.positions[0][1], SOURCE, "uninset height on the side face");
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
        back_up_face: false,
        side_overlay: SideOverlay::default(),
    };
    let quads = bake_fluid(&geom, still, flow, None);
    // Every top UV should land inside the flow sprite's U range [0.5, 1.0].
    for [u, _] in quads[0].uvs {
        assert!((0.5..=1.0).contains(&u), "flow UV u={u} out of flow sprite");
    }
}

#[test]
fn full_cell_emits_six_faces_no_cullface() {
    // No back-up-face and no overlay: the top and bottom stay single-sided, but
    // each of the four sides gets a reversed back copy (`addBackFace` is
    // unconditional for a non-overlay side face) — 1 + 1 + 4*2 = 10.
    let geom = FluidGeometry {
        corners: [1.0; 4],
        flow: [0.0, 0.0],
        faces: FaceSet::default(),
        tint_index: Some(0),
        back_up_face: false,
        side_overlay: SideOverlay::default(),
    };
    let quads = bake_fluid(&geom, uv(0.0, 0.0, 1.0, 1.0), uv(0.0, 0.0, 1.0, 1.0), None);
    assert_eq!(quads.len(), 10);
    // Fluids are culled by the mesher via FaceSet, so no quad carries a cullface.
    assert!(quads.iter().all(|q| q.cullface.is_none()));
    // Up and down are single-sided; each horizontal direction appears twice
    // (front + back).
    for (dir, want) in [
        (Direction::Up, 1),
        (Direction::Down, 1),
        (Direction::North, 2),
        (Direction::South, 2),
        (Direction::East, 2),
        (Direction::West, 2),
    ] {
        assert_eq!(
            quads.iter().filter(|q| q.direction == dir).count(),
            want,
            "direction {dir:?}"
        );
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
        back_up_face: false,
        side_overlay: SideOverlay::default(),
    };
    // Flow sprite occupies full [0,1] rect so at(u,v) == (u,v).
    let quads = bake_fluid(&geom, uv(0.0, 0.0, 1.0, 1.0), uv(0.0, 0.0, 1.0, 1.0), None);
    // Front + reversed back copy (no overlay material supplied).
    assert_eq!(quads.len(), 2);
    let side = &quads[0];
    // u only reaches 0.5 (left half); v spans (1-h)*0.5 .. 0.5.
    for [u, _] in side.uvs {
        assert!(u <= 0.5 + 1e-6, "side u={u} should stay in the left half");
    }
    // Top verts sit at v=(1-SOURCE)*0.5; bottom verts at v=0.5.
    let top_v = (1.0 - SOURCE) * 0.5;
    assert!((side.uvs[0][1] - top_v).abs() < 1e-6);
    assert!((side.uvs[2][1] - 0.5).abs() < 1e-6);

    // The back copy is the reversed winding [0,3,2,1] with matching UVs, and
    // both quads' bottom edge sits flush at y=0 (the bottom face is culled, so
    // `bottom_offs` stays 0 per vanilla's `renderDown ? 0.001 : 0.0`).
    let back = &quads[1];
    assert_eq!(back.positions[0], side.positions[0]);
    assert_eq!(back.positions[1], side.positions[3]);
    assert_eq!(back.positions[2], side.positions[2]);
    assert_eq!(back.positions[3], side.positions[1]);
    assert_eq!(side.positions[2][1], 0.0, "bottom face culled: flush at y=0");
}

#[test]
fn overlay_side_face_is_single_sided_and_samples_the_overlay_sprite() {
    // A side face against glass/ice/leaves uses `water_overlay` and omits its
    // back copy (`addBackFace = !isOverlay`), matching `FluidRenderer.tesselate`.
    let flow = uv(0.0, 0.0, 0.5, 0.5); // distinguishable ranges so a test bug
    let overlay = uv(0.5, 0.5, 1.0, 1.0); // would show up as a wrong-sprite UV
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
        back_up_face: false,
        side_overlay: SideOverlay {
            north: true,
            ..SideOverlay::default()
        },
    };
    let quads = bake_fluid(&geom, uv(0.0, 0.0, 1.0, 1.0), flow, Some(overlay));
    assert_eq!(quads.len(), 1, "overlay side face has no back copy");
    for [u, v] in quads[0].uvs {
        assert!(
            (0.5..=1.0).contains(&u) && (0.5..=1.0).contains(&v),
            "overlay UV ({u}, {v}) should land in the overlay sprite's rect, not flow's"
        );
    }
}

#[test]
fn overlay_is_ignored_without_an_overlay_sprite() {
    // Lava has no overlay material in vanilla: even if the mesher flagged a
    // side as an overlay neighbour, `bake_fluid` must fall back to `flow` and
    // keep the back face, because `overlay` is `None`.
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
        tint_index: None,
        back_up_face: false,
        side_overlay: SideOverlay {
            north: true,
            ..SideOverlay::default()
        },
    };
    let quads = bake_fluid(&geom, uv(0.0, 0.0, 1.0, 1.0), uv(0.0, 0.0, 1.0, 1.0), None);
    assert_eq!(quads.len(), 2, "no overlay sprite supplied: back face restored");
}

#[test]
fn bottom_face_and_side_base_lift_together_when_both_drawn() {
    // `bottomOffs = renderDown ? 0.001F : 0.0F` is shared: when the bottom face
    // draws, both it *and* every side face's bottom edge sit at y=0.001, so the
    // two don't z-fight against each other.
    let geom = FluidGeometry {
        corners: [SOURCE; 4],
        flow: [0.0, 0.0],
        faces: FaceSet {
            up: false,
            down: true,
            north: true,
            south: false,
            east: false,
            west: false,
        },
        tint_index: Some(0),
        back_up_face: false,
        side_overlay: SideOverlay::default(),
    };
    let quads = bake_fluid(&geom, uv(0.0, 0.0, 1.0, 1.0), uv(0.0, 0.0, 1.0, 1.0), None);
    // down (1) + north front/back (2).
    assert_eq!(quads.len(), 3);
    let down = quads.iter().find(|q| q.direction == Direction::Down).unwrap();
    assert!(
        down.positions.iter().all(|p| (p[1] - 0.001).abs() < 1e-7),
        "bottom face lifted by the z-fight inset"
    );
    let side = quads.iter().find(|q| q.direction == Direction::North).unwrap();
    assert!(
        (side.positions[2][1] - 0.001).abs() < 1e-7,
        "side face's bottom edge lifted to match the bottom face"
    );
}

#[test]
fn top_face_back_copy_is_reversed_winding() {
    let geom = FluidGeometry {
        corners: [SOURCE; 4],
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
        back_up_face: true,
        side_overlay: SideOverlay::default(),
    };
    let quads = bake_fluid(&geom, uv(0.0, 0.0, 1.0, 1.0), uv(0.0, 0.0, 1.0, 1.0), None);
    assert_eq!(quads.len(), 2, "shouldRenderBackwardUpFace true: front + back");
    let (front, back) = (&quads[0], &quads[1]);
    assert_eq!(back.positions, [
        front.positions[0],
        front.positions[3],
        front.positions[2],
        front.positions[1],
    ]);
}
