//! Tests for the code-defined entity model geometry primitive.
//!
//! Entity models in vanilla are Java code (`LayerDefinition`/`MeshDefinition`/
//! `PartDefinition`/`CubeDefinition`), not JSON, so these fixtures are built by
//! hand and the expected vertices/UVs are computed directly from the vanilla
//! `ModelPart.Cube` unwrap so a transposed axis or a flipped V is caught.

use lodestone_assets::Direction;
use lodestone_assets::entity::{
    CubeDef, EntityModelDef, PartDef, PartPose, bake_entity, player_model,
};

fn approx(a: [f32; 3], b: [f32; 3]) {
    for i in 0..3 {
        assert!((a[i] - b[i]).abs() < 1e-5, "pos {a:?} != {b:?}");
    }
}
fn approx2(a: [f32; 2], b: [f32; 2]) {
    for i in 0..2 {
        assert!((a[i] - b[i]).abs() < 1e-5, "uv {a:?} != {b:?}");
    }
}

/// A single box (w=2,h=4,d=6) at origin, texOffs (0,0), on a 64x64 sheet.
fn single_box() -> EntityModelDef {
    let cube = CubeDef::new([0.0, 0.0, 0.0], [2.0, 4.0, 6.0], [0.0, 0.0]);
    let root = PartDef::new(PartPose::ZERO).with_cube(cube);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

#[test]
fn cube_emits_six_faces() {
    let quads = bake_entity(&single_box());
    assert_eq!(quads.len(), 6);
    let mut dirs: Vec<Direction> = quads.iter().map(|q| q.direction).collect();
    dirs.sort_by_key(|d| format!("{d:?}"));
    dirs.dedup();
    assert_eq!(dirs.len(), 6, "each of the 6 faces appears once");
}

#[test]
fn north_face_positions_and_uvs_match_vanilla_unwrap() {
    let quads = bake_entity(&single_box());
    let north = quads
        .iter()
        .find(|q| q.direction == Direction::North)
        .unwrap();
    // NORTH verts {t1, t0, t3, t2} in model space / 16.
    approx(north.positions[0], [2.0 / 16.0, 0.0, 0.0]); // t1
    approx(north.positions[1], [0.0, 0.0, 0.0]); // t0
    approx(north.positions[2], [0.0, 4.0 / 16.0, 0.0]); // t3
    approx(north.positions[3], [2.0 / 16.0, 4.0 / 16.0, 0.0]); // t2
    // NORTH texel rect (u1=6, v1=6, u2=8, v2=10) → normalised /64.
    approx2(north.uvs[0], [8.0 / 64.0, 6.0 / 64.0]);
    approx2(north.uvs[1], [6.0 / 64.0, 6.0 / 64.0]);
    approx2(north.uvs[2], [6.0 / 64.0, 10.0 / 64.0]);
    approx2(north.uvs[3], [8.0 / 64.0, 10.0 / 64.0]);
}

#[test]
fn up_face_v_is_inverted_like_vanilla() {
    let quads = bake_entity(&single_box());
    let up = quads.iter().find(|q| q.direction == Direction::Up).unwrap();
    // UP passes (u2=8, v1=6, u22=10, v0=0): vMin=6 at verts 0/1, vMax=0 at 2/3.
    approx2(up.uvs[0], [10.0 / 64.0, 6.0 / 64.0]);
    approx2(up.uvs[1], [8.0 / 64.0, 6.0 / 64.0]);
    approx2(up.uvs[2], [8.0 / 64.0, 0.0]);
    approx2(up.uvs[3], [10.0 / 64.0, 0.0]);
}

#[test]
fn part_pose_offset_translates_all_vertices() {
    let cube = CubeDef::new([0.0, 0.0, 0.0], [2.0, 2.0, 2.0], [0.0, 0.0]);
    // Pivot at (16,0,0) model texels → +1.0 world units in x.
    let root = PartDef::new(PartPose::offset(16.0, 0.0, 0.0)).with_cube(cube);
    let model = EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    };
    let quads = bake_entity(&model);
    let north = quads
        .iter()
        .find(|q| q.direction == Direction::North)
        .unwrap();
    // t0 = (0,0,0)/16 shifted by (+1,0,0).
    approx(north.positions[1], [1.0, 0.0, 0.0]);
}

#[test]
fn y_rotation_90_maps_x_axis_to_negative_z() {
    let cube = CubeDef::new([0.0, 0.0, 0.0], [16.0, 0.0, 0.0], [0.0, 0.0]);
    let root =
        PartDef::new(PartPose::rotation(0.0, std::f32::consts::FRAC_PI_2, 0.0)).with_cube(cube);
    let model = EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    };
    let quads = bake_entity(&model);
    // The point (16,0,0)/16 = (1,0,0) under Ry(+90°) → (0,0,-1).
    let has_expected = quads
        .iter()
        .flat_map(|q| q.positions)
        .any(|p| (p[0]).abs() < 1e-5 && (p[1]).abs() < 1e-5 && (p[2] + 1.0).abs() < 1e-5);
    assert!(has_expected, "Ry(90) should send +x to -z");
}

#[test]
fn child_part_inherits_parent_transform() {
    let child_cube = CubeDef::new([0.0, 0.0, 0.0], [2.0, 2.0, 2.0], [0.0, 0.0]);
    let child = PartDef::new(PartPose::offset(16.0, 0.0, 0.0)).with_cube(child_cube);
    let root = PartDef::new(PartPose::offset(0.0, 16.0, 0.0)).with_child("arm", child);
    let model = EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    };
    let quads = bake_entity(&model);
    // Child cube's t0 world = parent(+0,+1,0) ∘ child(+1,0,0) applied to (0,0,0).
    let north = quads
        .iter()
        .find(|q| q.direction == Direction::North)
        .unwrap();
    approx(north.positions[1], [1.0, 1.0, 0.0]);
}

#[test]
fn player_model_wide_and_slim_differ_by_arm_width() {
    let wide = player_model(false);
    let slim = player_model(true);
    let wq = bake_entity(&wide);
    let sq = bake_entity(&slim);
    // Both are 64x64 sheets with head/body/arms/legs plus overlay layers.
    assert_eq!(wide.texture_width, 64);
    assert_eq!(wide.texture_height, 64);
    // Slim arms are 3 wide vs 4, so the two models are not identical.
    assert_ne!(wq.len(), 0);
    assert_eq!(
        wq.len(),
        sq.len(),
        "same part count, different arm geometry"
    );
    // Prove the arm width difference shows up in vertex extents.
    let wide_span = x_span(&wq);
    let slim_span = x_span(&sq);
    assert!(
        slim_span < wide_span,
        "slim model is narrower: {slim_span} < {wide_span}"
    );
}

fn x_span(quads: &[lodestone_assets::entity::EntityQuad]) -> f32 {
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for q in quads {
        for p in q.positions {
            lo = lo.min(p[0]);
            hi = hi.max(p[0]);
        }
    }
    hi - lo
}

#[test]
fn mirror_reverses_winding_order() {
    let plain = CubeDef::new([0.0, 0.0, 0.0], [2.0, 2.0, 2.0], [0.0, 0.0]);
    let mirrored = CubeDef::new([0.0, 0.0, 0.0], [2.0, 2.0, 2.0], [0.0, 0.0]).mirrored();
    let a = bake_entity(&EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO).with_cube(plain),
    });
    let b = bake_entity(&EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO).with_cube(mirrored),
    });
    let na = a.iter().find(|q| q.direction == Direction::North).unwrap();
    let nb = b.iter().find(|q| q.direction == Direction::North).unwrap();
    // Mirroring reverses the vertex order, so first and last swap.
    assert_ne!(na.uvs, nb.uvs);
}
