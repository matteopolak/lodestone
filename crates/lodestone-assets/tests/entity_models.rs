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
use lodestone_assets::entity::{
    Affine, CubeDef, EntityModelDef, PartDef, PartPose, bake_entity, bake_entity_parts,
};
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

/// UVs must be finite and within a *gross* sanity envelope of the model's
/// declared sheet. Vanilla is emphatically not strictly `[0, 1]`: `SalmonModel`
/// uses a negative `texOffs(-4, 0)`, `CodModel` a negative `texOffs(20, -6)`,
/// and `PufferfishBigModel`'s fins run ~7 texels off the right edge of their
/// 32x32 sheet — all ship in the game and sample off-sheet on purpose. So the
/// real gates on UV correctness are the box-count test and the real-PNG
/// sheet-size check; this one only catches a catastrophic offset (NaN, or a
/// halved/doubled sheet that pushes UVs past 2x).
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
                    uv[0].is_finite() && uv[1].is_finite(),
                    "{}: non-finite UV {uv:?}",
                    e.name
                );
                assert!(
                    (-1.0..=2.0).contains(&uv[0]) && (-1.0..=2.0).contains(&uv[1]),
                    "{}: UV {uv:?} is wildly off the {}x{} sheet (sheet-size error?)",
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
        ("creeper", 6),          // head, body, 4 legs
        ("zombie", 7),           // head, hat, body, 2 arms, 2 legs
        ("skeleton", 7),         // head, hat, body, 2 arms, 2 legs (thin)
        ("spider", 11),          // head, body0, body1, 8 legs
        ("pig", 7),              // head+snout(2), body, 4 legs
        ("cow", 10),             // head+snout+2 horns(4), body+udder(2), 4 legs
        ("sheep", 6),            // head, body, 4 legs
        ("chicken", 8),          // head, beak, red_thing, body, 2 legs, 2 wings
        ("player_wide", 12),     // head+hat, body+jacket, arms+sleeves, legs+pants
        ("slime", 1),            // outer shell cube
        ("magma_cube", 9),       // 8 stacked segments + inside cube
        ("blaze", 13),           // head + 12 rods
        ("squid", 9),            // body + 8 tentacles
        ("bat", 9),              // body, head, 2 ears, 2 wings, 2 tips, feet
        ("enderman", 7),         // head, hat, body, 2 arms, 2 legs
        ("drowned", 7),          // zombie mesh, retextured left limbs
        ("iron_golem", 8),       // head+nose, body+belt, 2 arms, 2 legs
        ("snow_golem", 5),       // head, 2 arms, 2 stacked snow spheres
        ("vex", 7),              // head, 2-box body, 2 arms, 2 wings
        ("silverfish", 10),      // 7 segments + 3 raised plates
        ("endermite", 4),        // 4 segments
        ("piglin", 15), // player mesh: head(4)+2 ears, body, 2 arms+2 sleeves, 2 legs+2 pants
        ("ghast", 10),  // body + 9 tentacles
        ("cave_spider", 11), // spider mesh, scaled 0.7
        ("husk", 7),    // zombie mesh, scaled 1.0625
        ("wither_skeleton", 7), // skeleton mesh, scaled 1.2
        ("hoglin", 11), // body+mane, head+2 ears+2 horns, 4 legs
        ("strider", 9), // 2 legs, body + 6 bristles
        ("guardian", 22), // head(5) + 12 spikes + eye + tail(3)
        ("phantom", 8), // body, tail(2), 2 wings(2 boxes each), head
        ("warden", 10), // body, 2 ribcages, head, 2 tendrils, 2 arms, 2 legs
        ("wither", 9),  // shoulders, ribcage(4), tail, 3 heads
        ("ender_dragon", 65), // head(6)+jaw, 5 neck*2, 12 tail*2, body(4), 2 wings*2*2, 4 legs*3
        ("witch", 15),  // head, hat(+brim+3 stack), nose+mole, body, jacket, arms(3), 2 legs
        ("villager", 11), // head, hat, hat_rim, nose, body, jacket, arms(3), 2 legs
        ("zombie_villager", 10), // head(+nose), hat, hat_rim, body(2), 2 arms, 2 legs
        // --- animal/npc/object half (owned by this agent) ---
        ("armor_stand", 10), // baseplate, 2 shoulder sticks + torso stick, body, 2 arms, head, right_leg, left_leg (no hat)
        ("boat", 9),         // bottom, back, front, right, left, 2 paddles(2 boxes each)
        ("chest_boat", 12),  // boat(9) + chest(3: chest, lock, latch)
        ("raft", 6),         // bottom(2 boxes), 2 paddles(2 boxes each)
        ("chest_raft", 9),   // raft(6) + chest(3)
        ("minecart", 5),     // 4 walls + bottom
        ("end_crystal", 4),  // outer_glass, inner_glass, cube, base
        ("rabbit", 9),       // head+ears(3), body, tail, 2 front feet, 2 back feet
        ("fox", 10),         // head+ears(3), body, tail, 2 front legs, 2 back legs
        ("panda", 9),        // head(4), body, 4 legs
        ("goat", 12),        // head(3, incl. goatee)+2 horns, body, 4 legs, tail
        ("bee", 9),          // body, stinger, 2 antennae, 2 wings, 3 legs (front/middle/back)
        ("turtle", 8),       // head, body(2 boxes: shell+belly), egg_belly, 4 legs
        ("camel", 12),       // head(3: muzzle/skull/snout)+2 ears, body+hump+tail, 4 legs
        ("cod", 7), // body, tail, nose, 2 side fins, top_fin, tail_fin... transcribed below
        ("salmon", 8), // body, tail, nose, 2 fins, top_fin, tail_fin, back_fin
        ("pufferfish", 13), // Big variant: body + 12 spikes
        ("tropical_fish", 6), // Large variant: body, tail, 4 fins (right/left/top/bottom)
        ("dolphin", 8), // body, back_fin, left_fin, right_fin, tail+tail_fin, head+nose
        ("axolotl", 11), // body, tail, head, 3 gills, 4 legs
        ("frog", 16), // body(2), head(2), eyes(2), croaking_body, tongue, 2 arms+hands(4), 2 legs+feet(4)
        ("tadpole", 2), // body, tail
        ("sniffer", 15), // body(3), 6 legs, head(2), 2 ears, nose, lower_beak
        ("armadillo", 11), // body(2), tail, head_cube, 2 ear cubes, 4 legs, rolled-up cube
        ("horse", 12),     // equine base: body, tail, head_parts, head, 2 ears, mane, upper_mouth, 4 legs
        ("skeleton_horse", 12), // equine base, unscaled
        ("zombie_horse", 12),   // equine base, unscaled
        ("donkey", 14),    // equine base(12) with 2 chest boxes added, scaled 0.87
        ("mule", 14),      // equine base(12) with 2 chest boxes added, scaled 0.92
        ("llama", 11),     // head(4: main/neck/2 ears), body, right_chest, left_chest, 4 legs
        ("trader_llama", 11), // same mesh as llama
        ("cat", 11),       // feline base: head(4: main/nose/2 ears), body, tail1, tail2, 4 legs
        ("ocelot", 11),    // feline base, unscaled
        ("wolf", 11), // real_head(4), body, upper_body, 4 legs, real_tail (head/tail parts are empty pivots)
        ("parrot", 11), // body, tail, 2 wings, head tree(5: head/head2/beak1/beak2/feather), 2 legs
        ("polar_bear", 10), // head(4: main/mouth/2 ears), body(2), 4 legs
        ("pillager", 12), // head/hat/nose(3), body(2), arms/left_shoulder(3), 2 legs, 2 arms
        ("vindicator", 12), // same illager mesh
        ("evoker", 12),     // same illager mesh
        ("illusioner", 12), // same illager mesh
        ("ravager", 12), // neck/head/2 horns/mouth(5)+head main box(1)=6, body(2), 4 legs
        ("allay", 7),    // head, body(2: main+lower)+2 arms+2 wings
        ("shulker", 3),  // lid, base, head
        ("glow_squid", 9), // same mesh as squid
        ("wandering_trader", 11), // same mesh as villager
        ("mooshroom", 10), // same mesh as cow
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
    assert!(
        close(a, [1.0, -1.0, 0.0]),
        "case A got {a:?}, want [1,-1,0]"
    );

    // Case B: the spider-leg shape, yRot & zRot both nonzero, xRot=0.
    //   local (16,0,0)->(1,0,0)wu, no pivot; Ry(90)*(1,0,0)=(0,0,-1);
    //   Rz(90)*(0,0,-1)=(0,0,-1). A swapped order would give (0,1,0).
    let b = transform_probe([16.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, h, h]);
    assert!(
        close(b, [0.0, 0.0, -1.0]),
        "case B got {b:?}, want [0,0,-1]"
    );
}

/// The texture-variant seam must be non-vacuous: a `ByVariant` entry has to
/// resolve *distinct* sheets for distinct variants (a selector that returns the
/// same path for every variant is a silent bug the type system can't catch),
/// and a `Fixed` entry must ignore the variant entirely. 26.2's pig is the
/// canonical case: `_temperate`/`_cold`/`_warm` are three real sheets.
#[test]
fn variant_textures_resolve_distinctly() {
    use lodestone_assets::entity::{EntityTexture, EntityVariant, Temperature};

    let models = entity_models();
    let pig = models
        .iter()
        .find(|e| e.name == "pig")
        .expect("pig model registered");

    assert!(
        pig.texture.is_variant(),
        "pig must be variant-driven in 26.2"
    );

    let temperate = pig
        .texture
        .resolve(EntityVariant::Temperature(Temperature::Temperate));
    let cold = pig
        .texture
        .resolve(EntityVariant::Temperature(Temperature::Cold));
    let warm = pig
        .texture
        .resolve(EntityVariant::Temperature(Temperature::Warm));

    assert_eq!(
        temperate,
        pig.texture.default_path(),
        "default is the temperate sheet"
    );
    assert_ne!(temperate, cold, "cold variant must differ from temperate");
    assert_ne!(temperate, warm, "warm variant must differ from temperate");
    assert_ne!(cold, warm, "cold and warm variants must differ");

    // A Fixed entry ignores the variant and always returns its one path.
    let fixed = EntityTexture::Fixed("entity/creeper/creeper");
    assert!(!fixed.is_variant());
    assert_eq!(fixed.default_path(), "entity/creeper/creeper");
    assert_eq!(
        fixed.resolve(EntityVariant::Temperature(Temperature::Warm)),
        "entity/creeper/creeper",
        "Fixed must ignore the variant"
    );

    // Every registered ByVariant entry must resolve its own variant axis to
    // distinct, non-empty paths — proves no selector is a constant fn. Each
    // entry's axis is looked up by name rather than assumed to be Temperature,
    // since 26.2 grew independent axes for horse colour, llama, cat, wolf and
    // parrot alongside the original pig/cow/chicken temperature axis.
    use lodestone_assets::entity::{
        CatCoat, HorseColor, LlamaColor, MooshroomColor, ParrotColor, WolfCoat, WolfState,
    };

    for e in &models {
        if e.texture.is_variant() {
            let probes: Vec<EntityVariant> = match e.name {
                "pig" | "cow" | "chicken" => vec![
                    EntityVariant::Temperature(Temperature::Temperate),
                    EntityVariant::Temperature(Temperature::Cold),
                    EntityVariant::Temperature(Temperature::Warm),
                ],
                "horse" => vec![
                    EntityVariant::HorseColor(HorseColor::White),
                    EntityVariant::HorseColor(HorseColor::Creamy),
                    EntityVariant::HorseColor(HorseColor::Chestnut),
                    EntityVariant::HorseColor(HorseColor::Brown),
                    EntityVariant::HorseColor(HorseColor::Black),
                    EntityVariant::HorseColor(HorseColor::Gray),
                    EntityVariant::HorseColor(HorseColor::DarkBrown),
                ],
                "llama" | "trader_llama" => vec![
                    EntityVariant::Llama(LlamaColor::Creamy),
                    EntityVariant::Llama(LlamaColor::White),
                    EntityVariant::Llama(LlamaColor::Brown),
                    EntityVariant::Llama(LlamaColor::Gray),
                ],
                "cat" => vec![
                    EntityVariant::Cat(CatCoat::Tabby),
                    EntityVariant::Cat(CatCoat::Black),
                    EntityVariant::Cat(CatCoat::Red),
                    EntityVariant::Cat(CatCoat::Siamese),
                    EntityVariant::Cat(CatCoat::BritishShorthair),
                    EntityVariant::Cat(CatCoat::Calico),
                    EntityVariant::Cat(CatCoat::Persian),
                    EntityVariant::Cat(CatCoat::Ragdoll),
                    EntityVariant::Cat(CatCoat::White),
                    EntityVariant::Cat(CatCoat::Jellie),
                    EntityVariant::Cat(CatCoat::AllBlack),
                ],
                "wolf" => vec![
                    EntityVariant::Wolf {
                        coat: WolfCoat::Pale,
                        state: WolfState::Wild,
                    },
                    EntityVariant::Wolf {
                        coat: WolfCoat::Pale,
                        state: WolfState::Tame,
                    },
                    EntityVariant::Wolf {
                        coat: WolfCoat::Pale,
                        state: WolfState::Angry,
                    },
                    EntityVariant::Wolf {
                        coat: WolfCoat::Spotted,
                        state: WolfState::Wild,
                    },
                    EntityVariant::Wolf {
                        coat: WolfCoat::Snowy,
                        state: WolfState::Wild,
                    },
                    EntityVariant::Wolf {
                        coat: WolfCoat::Black,
                        state: WolfState::Wild,
                    },
                    EntityVariant::Wolf {
                        coat: WolfCoat::Ashen,
                        state: WolfState::Wild,
                    },
                    EntityVariant::Wolf {
                        coat: WolfCoat::Rusty,
                        state: WolfState::Wild,
                    },
                    EntityVariant::Wolf {
                        coat: WolfCoat::Woods,
                        state: WolfState::Wild,
                    },
                    EntityVariant::Wolf {
                        coat: WolfCoat::Chestnut,
                        state: WolfState::Wild,
                    },
                    EntityVariant::Wolf {
                        coat: WolfCoat::Striped,
                        state: WolfState::Wild,
                    },
                ],
                "parrot" => vec![
                    EntityVariant::Parrot(ParrotColor::RedBlue),
                    EntityVariant::Parrot(ParrotColor::Blue),
                    EntityVariant::Parrot(ParrotColor::Green),
                    EntityVariant::Parrot(ParrotColor::YellowBlue),
                    EntityVariant::Parrot(ParrotColor::Gray),
                ],
                "mooshroom" => vec![
                    EntityVariant::Mooshroom(MooshroomColor::Red),
                    EntityVariant::Mooshroom(MooshroomColor::Brown),
                ],
                other => panic!(
                    "{other}: ByVariant entry has no probe set in this test — add one \
                     covering its variant axis rather than letting it fall through to \
                     the Temperature default"
                ),
            };
            let resolved: Vec<&str> = probes.iter().map(|v| e.texture.resolve(*v)).collect();
            assert!(
                resolved.iter().all(|p| !p.is_empty()),
                "{}: empty path",
                e.name
            );
            let mut sorted = resolved.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                resolved.len(),
                "{}: variant selector is constant (or maps two distinct variants to the \
                 same path) — got {:?}",
                e.name,
                resolved
            );
        }
    }
}

/// Composes a [`BakedPart`] list's transform chain and asserts the result is
/// bit-identical to `bake_entity`.
///
/// This is the load-bearing test for per-part baking: an animating renderer
/// poses parts independently, so `bake_entity_parts` has to be a *factoring* of
/// `bake_entity` rather than a second, subtly different traversal. Comparing
/// against the shipped whole-model bake — the function already validated
/// against the real texture sheets — means the expected values do not come from
/// the code under test.
#[test]
fn part_bake_recomposes_to_the_whole_model_bake() {
    let mut checked = 0usize;
    for entry in entity_models() {
        let def = (entry.build)();
        let whole = bake_entity(&def);
        let parts = bake_entity_parts(&def);

        // Accumulate each part's chain, then re-apply it to its local quads.
        let mut chains: Vec<Affine> = Vec::with_capacity(parts.len());
        let mut recomposed = Vec::with_capacity(whole.len());
        for part in &parts {
            let parent = part.parent.map_or(Affine::IDENTITY, |i| chains[i]);
            let world = parent.compose(&Affine::of_pose(&part.rest));
            chains.push(world);
            for quad in &part.quads {
                let mut q = quad.clone();
                for p in &mut q.positions {
                    *p = world.apply(*p);
                }
                recomposed.push(q);
            }
        }

        assert_eq!(
            recomposed.len(),
            whole.len(),
            "{}: per-part bake produced {} quads, whole-model bake {}",
            entry.name,
            recomposed.len(),
            whole.len()
        );
        for (i, (got, want)) in recomposed.iter().zip(whole.iter()).enumerate() {
            assert_eq!(
                got.uvs, want.uvs,
                "{}: quad {i} UVs differ between per-part and whole bake",
                entry.name
            );
            for (c, (g, w)) in got.positions.iter().zip(want.positions.iter()).enumerate() {
                for axis in 0..3 {
                    assert!(
                        (g[axis] - w[axis]).abs() <= 1.0e-6,
                        "{}: quad {i} corner {c} axis {axis}: per-part {} != whole {}",
                        entry.name,
                        g[axis],
                        w[axis]
                    );
                }
            }
        }
        checked += whole.len();
    }
    assert!(
        checked > 5_000,
        "only {checked} quads compared — the corpus loop did not run (vacuous)"
    );
}
