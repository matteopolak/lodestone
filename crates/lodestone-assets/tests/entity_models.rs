//! Whole-corpus tests for the hand-ported entity model library.
//!
//! Entity models are vanilla Java code, not data, so each mesh here is
//! transcribed by hand from the decompiled 26.2 client
//! (`net/minecraft/client/model/...`). These tests assert the structural
//! invariants that a transposed axis, a wrong texel offset, or a wrong sheet
//! size would break, plus determinism. The *external authority* check (each
//! model's sheet size against the real texture PNG in `client.jar`) lives in
//! `real_jar.rs` so a wrong sheet size cannot pass.

use lodestone_assets::Direction;
use lodestone_assets::entity::{CubeDef, EntityModelDef, PartDef, PartPose, bake_entity};
use lodestone_assets::entity_models::entity_models;

/// Bakes a single zero-size box whose corner sits at model texel `local`,
/// attached to one part posed at `pivot` (model texels) with the given
/// rotations (radians), and returns the world-space position of a baked
/// vertex. A zero-size box collapses all eight corners onto `local`, so every
/// baked vertex is the full part transform applied to that one point — an
/// isolated probe of `translate -> rotationZYX -> (corner)` with no unwrap
/// bookkeeping in the way.
fn transform_probe(local: [f32; 3], pivot: [f32; 3], rot: [f32; 3]) -> [f32; 3] {
    let model = EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::offset(0.0, 0.0, 0.0)).with_child(
            "p",
            PartDef::new(PartPose::offset_and_rotation(
                pivot[0], pivot[1], pivot[2], rot[0], rot[1], rot[2],
            ))
            .with_cube(CubeDef::new(local, [0.0, 0.0, 0.0], [0.0, 0.0])),
        ),
    };
    bake_entity(&model)[0].positions[0]
}

/// Every ported model must bake to at least one quad and never panic.
#[test]
fn every_model_bakes_nonempty() {
    let models = entity_models();
    assert!(
        models.len() >= 9,
        "priority corpus present: {}",
        models.len()
    );
    for e in &models {
        let quads = bake_entity(&(e.build)());
        assert!(!quads.is_empty(), "{} baked no quads", e.name);
    }
}

/// Every baked UV must lie within the model's declared sheet (`[0, 1]`). A UV
/// outside means the texel offset or sheet size is wrong. Degenerate (zero-area)
/// faces are skipped: vanilla flat planes (e.g. strider bristles, hoglin mane)
/// are boxes with a zero dimension whose collapsed side faces are laid out
/// off-sheet by vanilla itself and never sampled.
#[test]
fn every_uv_is_within_the_sheet() {
    for e in &entity_models() {
        let model = (e.build)();
        for q in bake_entity(&model) {
            if quad_is_degenerate(&q.positions) {
                continue;
            }
            for uv in q.uvs {
                assert!(
                    (-1e-6..=1.0 + 1e-6).contains(&uv[0]) && (-1e-6..=1.0 + 1e-6).contains(&uv[1]),
                    "{}: UV {uv:?} escapes the {}x{} sheet",
                    e.name,
                    model.texture_width,
                    model.texture_height
                );
            }
        }
    }
}

/// A quad is degenerate (zero area) when two of its edges collapse — true for the
/// side faces of a flat, zero-thickness box.
fn quad_is_degenerate(p: &[[f32; 3]; 4]) -> bool {
    let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
    let e2 = [p[3][0] - p[0][0], p[3][1] - p[0][1], p[3][2] - p[0][2]];
    let cross = [
        e1[1] * e2[2] - e1[2] * e2[1],
        e1[2] * e2[0] - e1[0] * e2[2],
        e1[0] * e2[1] - e1[1] * e2[0],
    ];
    (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]) < 1e-9
}

/// Baking is deterministic: identical input yields byte-identical output, with
/// no `HashMap` iteration order anywhere in the path.
#[test]
fn baking_is_deterministic() {
    for e in &entity_models() {
        let a = bake_entity(&(e.build)());
        let b = bake_entity(&(e.build)());
        assert_eq!(a.len(), b.len(), "{} quad count unstable", e.name);
        for (qa, qb) in a.iter().zip(&b) {
            assert_eq!(qa.positions, qb.positions, "{} pos unstable", e.name);
            assert_eq!(qa.uvs, qb.uvs, "{} uv unstable", e.name);
        }
    }
}

/// The size-variant mobs must actually carry vanilla's baked-in
/// `MeshTransformer.scaling`, not just reuse the base mesh. Cave spider is
/// spider baked at 0.7; every baked vertex must equal `0.7 * spider_vertex +
/// (0, y_offset/16, 0)` where `y_offset = 24.016 * (1 - 0.7)`. If the scale were
/// forgotten the two would be identical and this fails (0.7 != 1).
#[test]
fn size_variants_carry_the_vanilla_mesh_scale() {
    use lodestone_assets::entity_models::{cave_spider_model, spider_model};

    let factor = 0.7f32;
    let ty = 24.016 * (1.0 - factor) / 16.0;
    let base = bake_entity(&spider_model());
    let scaled = bake_entity(&cave_spider_model());
    assert_eq!(base.len(), scaled.len(), "cave_spider changed box count");
    // Prove non-vacuity: the scaled model is genuinely different from the base.
    assert_ne!(
        base[0].positions[0], scaled[0].positions[0],
        "cave_spider is identical to spider — scale not applied"
    );
    for (b, s) in base.iter().zip(&scaled) {
        for k in 0..4 {
            let bp = b.positions[k];
            let sp = s.positions[k];
            let want = [factor * bp[0], factor * bp[1] + ty, factor * bp[2]];
            for axis in 0..3 {
                assert!(
                    (sp[axis] - want[axis]).abs() < 1e-4,
                    "cave_spider vertex {sp:?} != scaled spider {want:?}"
                );
            }
        }
    }
}


/// Exact per-box counts, transcribed from the vanilla meshes: each solid box
/// emits six faces, so a dropped or duplicated box shows up here.
#[test]
fn quad_counts_match_vanilla_box_counts() {
    let expected: &[(&str, usize)] = &[
        ("creeper", 6),      // head, body, 4 legs
        ("zombie", 7),       // head, hat, body, 2 arms, 2 legs
        ("skeleton", 7),     // head, hat, body, 2 arms, 2 legs (thin)
        ("spider", 11),      // head, body0, body1, 8 legs
        ("pig", 7),          // head+snout(2), body, 4 legs
        ("cow", 10),         // head+snout+2 horns(4), body+udder(2), 4 legs
        ("sheep", 6),        // head, body, 4 legs
        ("chicken", 8),      // head, beak, red_thing, body, 2 legs, 2 wings
        ("player_wide", 12), // head+hat, body+jacket, arms+sleeves, legs+pants
        ("slime", 1),        // outer shell cube
        ("magma_cube", 9),   // 8 stacked segments + inside cube
        ("blaze", 13),       // head + 12 rods
        ("squid", 9),        // body + 8 tentacles
        ("bat", 9),          // body, head, 2 ears, 2 wings, 2 tips, feet
        ("enderman", 7),     // head, hat, body, 2 arms, 2 legs
        ("drowned", 7),      // zombie mesh, retextured left limbs
        ("iron_golem", 8),   // head+nose, body+belt, 2 arms, 2 legs
        ("snow_golem", 5),   // head, 2 arms, 2 stacked snow spheres
        ("vex", 7),          // head, 2-box body, 2 arms, 2 wings
        ("silverfish", 10),  // 7 segments + 3 raised plates
        ("endermite", 4),    // 4 segments
        ("piglin", 15),      // player mesh: head(4)+2 ears, body, 2 arms+2 sleeves, 2 legs+2 pants
        ("ghast", 10),       // body + 9 tentacles
        ("cave_spider", 11), // spider mesh, scaled 0.7
        ("husk", 7),         // zombie mesh, scaled 1.0625
        ("wither_skeleton", 7), // skeleton mesh, scaled 1.2
        ("hoglin", 11),      // body+mane, head+2 ears+2 horns, 4 legs
        ("strider", 9),      // 2 legs, body + 6 bristles
        ("guardian", 22),    // head(5) + 12 spikes + eye + tail(3)
    ];
    let models = entity_models();
    for (name, boxes) in expected {
        let e = models
            .iter()
            .find(|e| e.name == *name)
            .unwrap_or_else(|| panic!("model {name} missing from corpus"));
        let quads = bake_entity(&(e.build)());
        assert_eq!(
            quads.len(),
            boxes * 6,
            "{name} should have {boxes} boxes = {} quads",
            boxes * 6
        );
    }
}

/// The creeper body box unwrap, computed independently from the vanilla
/// `ModelPart.Cube` texel formula: box `(-4,0,-2)` size `(8,12,4)`, texOffs
/// `(16,16)`, on a 64x32 sheet. The NORTH face texel rect is
/// `(u1=16+4=20, v1=16+4=20)`..`(u2=20+8=28, v2=20+12=32)`. This is the same
/// class of check as impl-entity's arrow test: a wrong axis or flipped V must
/// diverge from the hand-derived value, not merely from a value we also
/// computed in the implementation.
#[test]
fn creeper_body_north_uv_matches_hand_derived_vanilla_unwrap() {
    let models = entity_models();
    let creeper = models.iter().find(|e| e.name == "creeper").unwrap();
    let model = (creeper.build)();
    assert_eq!((model.texture_width, model.texture_height), (64, 32));
    let quads = bake_entity(&model);
    // The body's NORTH face. The body part is posed at offset (0,6,0) model
    // texels, so its box corner t0 = (-4,0,-2) lands at world (-4, 6, -2)/16.
    // Match on that corner (z=-2/16 distinguishes body from the head at z=-4/16).
    let body_north = quads
        .iter()
        .filter(|q| q.direction == Direction::North)
        .find(|q| {
            let p = q.positions[1];
            (p[0] + 4.0 / 16.0).abs() < 1e-4
                && (p[1] - 6.0 / 16.0).abs() < 1e-4
                && (p[2] + 2.0 / 16.0).abs() < 1e-4
        })
        .expect("creeper body north face present");
    // Vanilla NORTH remap: [0]=(uMax,vMin) [1]=(uMin,vMin) [2]=(uMin,vMax) [3]=(uMax,vMax).
    let close = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5;
    assert!(
        close(body_north.uvs[0], [28.0 / 64.0, 20.0 / 32.0]),
        "uv0 {:?}",
        body_north.uvs[0]
    );
    assert!(
        close(body_north.uvs[1], [20.0 / 64.0, 20.0 / 32.0]),
        "uv1 {:?}",
        body_north.uvs[1]
    );
    assert!(
        close(body_north.uvs[2], [20.0 / 64.0, 32.0 / 32.0]),
        "uv2 {:?}",
        body_north.uvs[2]
    );
    assert!(
        close(body_north.uvs[3], [28.0 / 64.0, 32.0 / 32.0]),
        "uv3 {:?}",
        body_north.uvs[3]
    );
}

/// Vanilla composes part rotation as `rotationZYX(zRot, yRot, xRot)` = `Rz*Ry*Rx`
/// (`ModelPart.translateAndRotate`, client 26.2 line 166), applied *after* the
/// pivot translation. The order only matters when two or more axes rotate at
/// once — which is exactly the shape of a spider leg (both `yRot` and `zRot`
/// nonzero via `offsetAndRotation`). This test hand-derives two multi-axis
/// results and asserts the bake matches, so a transposed multiply (`Rx*Ry*Rz`)
/// cannot pass: it would land the probe at a *different, also-plausible* point.
#[test]
fn composition_order_matches_vanilla_zyx_hand_derived() {
    let close = |a: [f32; 3], b: [f32; 3]| {
        (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5 && (a[2] - b[2]).abs() < 1e-5
    };
    let h = std::f32::consts::FRAC_PI_2;

    // Case A: local (0,0,16)->(0,0,1)wu, pivot (16,0,0)->(1,0,0)wu, xRot=yRot=90.
    //   Rx(90)*(0,0,1)=(0,-1,0); Ry(90)*(0,-1,0)=(0,-1,0); +pivot=(1,-1,0).
    //   A swapped Rx*Ry*Rz order would give (2,0,0) instead.
    let a = transform_probe([0.0, 0.0, 16.0], [16.0, 0.0, 0.0], [h, h, 0.0]);
    assert!(close(a, [1.0, -1.0, 0.0]), "case A got {a:?}, want [1,-1,0]");

    // Case B: the spider-leg shape, yRot & zRot both nonzero, xRot=0.
    //   local (16,0,0)->(1,0,0)wu, no pivot; Ry(90)*(1,0,0)=(0,0,-1);
    //   Rz(90)*(0,0,-1)=(0,0,-1). A swapped order would give (0,1,0).
    let b = transform_probe([16.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, h, h]);
    assert!(close(b, [0.0, 0.0, -1.0]), "case B got {b:?}, want [0,0,-1]");
}
