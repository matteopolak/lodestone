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
/// (`ModelPart.translateAndRotate`, client 26.2), applied *after* the
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

// ---------------------------------------------------------------------------
// Sheep wool layer
// ---------------------------------------------------------------------------
//
// `sheep_wool_model` is deliberately *not* registered in `entity_models()`: like
// the humanoid armour meshes in `equipment.rs`, it is a layer drawn over another
// rig's own part matrices, not a standalone drawable entity type. These tests
// therefore call it directly rather than looping the corpus.

/// The wool mesh's parts must share `sheep_model`'s part *names* and *pivots*
/// exactly, in the same pre-order — that equivalence is the whole precondition
/// for posing wool off the sheep body's own `part_transforms` by name, the way
/// `ArmourMesh::attach` poses armour off a wearer's. If a future edit to either
/// model renames or re-pivots a part without updating the other, a wool layer
/// would either fail to attach (name mismatch) or attach at the wrong joint
/// (pivot mismatch) — both silent, both exactly the "resolves perfectly and is
/// completely wrong" shape `CLAUDE.md` warns about for the armour/pig trap.
#[test]
fn sheep_wool_model_shares_sheep_body_part_names_and_pivots() {
    use lodestone_assets::entity_models::{sheep_model, sheep_wool_model};

    let body_parts = bake_entity_parts(&sheep_model());
    let wool_parts = bake_entity_parts(&sheep_wool_model());

    assert_eq!(
        body_parts.len(),
        wool_parts.len(),
        "sheep body has {} parts, wool has {} — they must match 1:1 for attach-by-name",
        body_parts.len(),
        wool_parts.len()
    );
    for (b, w) in body_parts.iter().zip(&wool_parts) {
        assert_eq!(
            b.name, w.name,
            "part order diverged between sheep_model and sheep_wool_model"
        );
        assert_eq!(
            b.parent, w.parent,
            "{}: parent index diverged between sheep_model and sheep_wool_model",
            b.name
        );
        assert_eq!(
            b.rest, w.rest,
            "{}: pivot/pose diverged between sheep_model and sheep_wool_model — a wool layer \
             posed off the sheep body's part_transforms would be at the wrong joint",
            b.name
        );
    }
    // Non-vacuity: the real part set, not an accidental empty match.
    let names: Vec<&str> = body_parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "",
            "head",
            "body",
            "right_hind_leg",
            "left_hind_leg",
            "right_front_leg",
            "left_front_leg"
        ]
    );
}

/// Per-part inflation, measured on the **baked local geometry** rather than
/// asserted on the constructor's `.grown(_)` arguments — the armour doc's
/// "pin these on the baked geometry, not the constants" rule, because a
/// deformation dropped anywhere between the table and the bake would still
/// pass a test that only re-reads the argument. Every value here is computed
/// by hand from `SheepFurModel.java`'s literal box constants
/// (`origin ± grow`, independent of this crate's `CubeDef::grown`), not
/// re-derived from the code under test.
#[test]
fn sheep_wool_inflations_match_vanilla_sheepfurmodel() {
    use lodestone_assets::entity_models::sheep_wool_model;

    let parts = bake_entity_parts(&sheep_wool_model());
    let x_half_extent = |name: &str| -> f32 {
        let part = parts.iter().find(|p| p.name == name).unwrap();
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for q in &part.quads {
            for p in &q.positions {
                lo = lo.min(p[0]);
                hi = hi.max(p[0]);
            }
        }
        (hi - lo) / 2.0
    };

    // head: origin.x=-3, size.x=6 (symmetric), grow=0.6 -> half-extent (3+0.6)/16.
    let head = x_half_extent("head");
    assert!(
        (head - 3.6 / 16.0).abs() < 1e-5,
        "head X half-extent {head}, want {}",
        3.6 / 16.0
    );
    // body: origin.x=-4, size.x=8 (symmetric), grow=1.75 -> half-extent (4+1.75)/16.
    let body = x_half_extent("body");
    assert!(
        (body - 5.75 / 16.0).abs() < 1e-5,
        "body X half-extent {body}, want {}",
        5.75 / 16.0
    );
    // legs: origin.x=-2, size.x=4 (symmetric), grow=0.5 -> half-extent (2+0.5)/16.
    for leg in [
        "right_hind_leg",
        "left_hind_leg",
        "right_front_leg",
        "left_front_leg",
    ] {
        let got = x_half_extent(leg);
        assert!(
            (got - 2.5 / 16.0).abs() < 1e-5,
            "{leg} X half-extent {got}, want {}",
            2.5 / 16.0
        );
    }

    // The legs are a *shorter* box, not a scaled copy: fur covers only the top
    // half of a leg (vanilla's "socks"), so its part-local Y span (6+0.5*2=7
    // texels) is well under the body model's full 12-texel leg.
    let leg_y_span = {
        let part = parts.iter().find(|p| p.name == "right_hind_leg").unwrap();
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for q in &part.quads {
            for p in &q.positions {
                lo = lo.min(p[1]);
                hi = hi.max(p[1]);
            }
        }
        hi - lo
    };
    assert!(
        (leg_y_span - 7.0 / 16.0).abs() < 1e-5,
        "wool leg Y span {leg_y_span}, want {} (6 texels + 2*0.5 grow)",
        7.0 / 16.0
    );
}

/// `sheep_wool_model` declares the same 64×32 sheet as `sheep_model`
/// (`LayerDefinition.create(mesh, 64, 32)` in both `SheepModel` and
/// `SheepFurModel`) — the real-PNG check against `entity/sheep/sheep_wool.png`
/// lives in `real_jar.rs`, since that needs the client jar.
#[test]
fn sheep_wool_model_sheet_is_64x32() {
    use lodestone_assets::entity_models::sheep_wool_model;

    let model = sheep_wool_model();
    assert_eq!((model.texture_width, model.texture_height), (64, 32));
    let quads = bake_entity(&model);
    assert_eq!(quads.len(), 6 * 6, "6 boxes (head, body, 4 legs) = 36 quads");
}

/// `sheep_wool_tint`'s 16-entry table, hand-computed from `DyeColor.java`'s
/// literal `textureDiffuseColor` constants and `ColorLerper`'s fixed
/// `brightness = 0.75` for `Type.SHEEP` (`floor(channel * 0.75)`), with
/// `DyeColor.WHITE` special-cased to vanilla's own literal `-1644826`
/// (`0xE6E6E6`). Computed independently of `sheep_wool_tint`'s source, not by
/// calling the same formula the function under test uses.
#[test]
fn sheep_wool_tint_matches_vanilla_color_lerper() {
    use lodestone_assets::entity_models::sheep_wool_tint;

    let expected: [[u8; 3]; 16] = [
        [230, 230, 230], // white (special-cased)
        [186, 96, 21],   // orange
        [149, 58, 141],  // magenta
        [43, 134, 163],  // light_blue
        [190, 162, 45],  // yellow
        [96, 149, 23],   // lime
        [182, 104, 127], // pink
        [53, 59, 61],    // gray
        [117, 117, 113], // light_gray
        [16, 117, 117],  // cyan
        [102, 37, 138],  // purple
        [45, 51, 127],   // blue
        [98, 63, 37],    // brown
        [70, 93, 16],    // green
        [132, 34, 28],   // red
        [21, 21, 24],    // black
    ];
    for (ordinal, want) in expected.iter().enumerate() {
        let got = sheep_wool_tint(ordinal as u8);
        assert_eq!(got, *want, "dye ordinal {ordinal}");
    }
    // Out-of-range ordinals fail open to undyed white, matching
    // `armour_layer_tint`'s rule for an unrecognised colour.
    assert_eq!(sheep_wool_tint(16), [230, 230, 230]);
    assert_eq!(sheep_wool_tint(255), [230, 230, 230]);

    // Non-vacuity: the table is not sixteen copies of the same entry.
    let unique: std::collections::HashSet<_> = expected.iter().collect();
    assert_eq!(unique.len(), 16, "dye table has duplicate entries");
}

// ============================================================================
// Projectile rigs
// ============================================================================

/// The arrow's box unwrap, computed independently from vanilla's
/// `ModelPart.Cube` texel formula rather than from our own baker.
///
/// The interesting part is the `cross` box's **non-unit `texScale`**:
/// `addBox(-12, -2, 0, 16, 4, 0, CubeDeformation.NONE, 1.0F, 0.8F)` divides `v`
/// by `32 * 0.8 = 25.6` instead of `32`, which is why the shaft strip in
/// `arrow.png` is 5 pixels tall for a 4-texel-tall box. Nothing else in the
/// corpus exercises `texScale`, so if this is wrong nothing else catches it.
///
/// Hand-derived NORTH face of `cross` (`d = 0`, `w = 16`, `h = 4`, `texOffs
/// (0, 0)`): `u1 = 0 + 0 = 0`, `u2 = 0 + 16 = 16`, `v1 = 0 + 0 = 0`,
/// `v2 = 0 + 4 = 4`. Normalised: `u ∈ [0, 16/32] = [0, 0.5]`,
/// `v ∈ [0, 4/25.6] = [0, 0.15625]`.
#[test]
fn arrow_cross_north_uv_matches_hand_derived_vanilla_unwrap() {
    use lodestone_assets::entity_models::arrow_model;

    let model = arrow_model();
    assert_eq!((model.texture_width, model.texture_height), (32, 32));
    let quads = bake_entity(&model);
    // 3 boxes × 6 faces. Six of the eighteen are degenerate (the flat boxes'
    // collapsed sides) — that is vanilla's own behaviour, not a bug here.
    assert_eq!(quads.len(), 18, "arrow should bake 3 boxes = 18 quads");
    let degenerate = quads
        .iter()
        .filter(|q| quad_is_degenerate(&q.positions))
        .count();
    // The zero-width fletching plane keeps WEST and EAST and collapses the other
    // four; each zero-depth shaft plane keeps NORTH and SOUTH and collapses four.
    // 4 + 4 + 4 = 12, leaving 6 real quads for the whole arrow.
    assert_eq!(
        degenerate, 12,
        "expected exactly 12 degenerate faces (4 from the zero-width fletching \
         plane, 4 from each of the two zero-depth shaft planes)"
    );

    let north: Vec<_> = quads
        .iter()
        .filter(|q| q.direction == Direction::North && !quad_is_degenerate(&q.positions))
        .collect();
    assert_eq!(
        north.len(),
        2,
        "both shaft planes contribute one real NORTH face"
    );
    for q in &north {
        let us: Vec<f32> = q.uvs.iter().map(|uv| uv[0]).collect();
        let vs: Vec<f32> = q.uvs.iter().map(|uv| uv[1]).collect();
        let (umin, umax) = (
            us.iter().copied().fold(f32::MAX, f32::min),
            us.iter().copied().fold(f32::MIN, f32::max),
        );
        let (vmin, vmax) = (
            vs.iter().copied().fold(f32::MAX, f32::min),
            vs.iter().copied().fold(f32::MIN, f32::max),
        );
        assert!(
            (umin - 0.0).abs() < 1e-6 && (umax - 0.5).abs() < 1e-6,
            "cross NORTH u span {umin}..{umax}, want 0.0..0.5"
        );
        assert!(
            (vmin - 0.0).abs() < 1e-6 && (vmax - 0.15625).abs() < 1e-6,
            "cross NORTH v span {vmin}..{vmax}, want 0.0..0.15625 — a `texScale` of \
             1.0 instead of 0.8 gives 0.125 here, which looks plausible and crops \
             the shaft"
        );
    }
}

/// The mesh-wide `0.9×` and the fletching's extra `0.8×` both reach the geometry.
///
/// `LayerDefinition.create(mesh.transformed(pose -> pose.scaled(0.9F)), 32, 32)`
/// reads as "scale every part", but `PartDefinition.transformed` applies the
/// function to *its own* pose and copies children untouched,
/// so it is the **root** pose that carries the
/// 0.9. Modelling it as a per-part 0.9 on each of the three children instead would
/// look identical for `cross_1`/`cross_2` and put the fletching in the wrong place,
/// because `back`'s pivot at `x = -11` would then not be scaled by it.
#[test]
fn arrow_carries_both_vanilla_scale_factors() {
    use lodestone_assets::entity_models::arrow_model;

    let parts = bake_entity_parts(&arrow_model());
    let by_name = |n: &str| {
        parts
            .iter()
            .find(|p| p.name == n)
            .unwrap_or_else(|| panic!("arrow has no part {n}"))
    };
    assert_eq!(by_name("").rest.scale, [0.9, 0.9, 0.9], "root mesh scale");
    assert_eq!(by_name("back").rest.scale, [0.8, 0.8, 0.8], "fletching scale");
    assert_eq!(by_name("back").rest.x, -11.0);
    assert_eq!(by_name("cross_1").rest.scale, [1.0, 1.0, 1.0]);

    // The composed effect, which is what actually reaches a vertex: the shaft runs
    // from x = -12 to +4 texels, so 16 texels = 1.0 block unscaled and 0.9 blocks
    // at the mesh scale. `MODEL_FEET_OFFSET`-style constants aside, this is the one
    // number a player would notice being wrong.
    let quads = bake_entity(&arrow_model());
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for q in &quads {
        for p in q.positions {
            lo = lo.min(p[0]);
            hi = hi.max(p[0]);
        }
    }
    let length = hi - lo;
    assert!(
        (length - 0.9).abs() < 1e-4,
        "arrow spans {length} blocks along its shaft, want 0.9 (16 texels × 0.9)"
    );
}

/// The `0.9×` really is a **root** scale, not three per-part ones: it must move
/// the fletching's pivot too.
///
/// `back`'s pivot is `x = -11` texels. Under a root scale the baked fletching sits
/// at `-11/16 × 0.9 = -0.61875` blocks; under a per-part scale it would sit at
/// `-11/16 = -0.6875` — 1.1 texels further back, sticking out past the end of the
/// shaft. Small, plausible, and wrong; and no UV or quad-count test can see it.
#[test]
fn the_arrow_mesh_scale_moves_the_fletching_pivot_too() {
    use lodestone_assets::entity_models::arrow_model;

    let parts = bake_entity_parts(&arrow_model());
    let root = parts.iter().find(|p| p.name.is_empty()).expect("root part");
    let back = parts.iter().find(|p| p.name == "back").expect("back part");
    let chain = Affine::of_pose(&root.rest).compose(&Affine::of_pose(&back.rest));
    let pivot = chain.apply([0.0, 0.0, 0.0]);
    assert!(
        (pivot[0] - (-11.0 / 16.0 * 0.9)).abs() < 1e-6,
        "fletching pivot at x={}, want {} (a per-part 0.9 would give {})",
        pivot[0],
        -11.0 / 16.0 * 0.9,
        -11.0 / 16.0
    );
}

/// One posed, **non-degenerate** vertex: `(position, uv)` as fixed-point integers
/// so two floating-point paths that agree mathematically compare equal.
type PosedVertex = [i64; 5];

/// Applies each part's transform chain and returns the posed vertices **per part**,
/// with degenerate (zero-area) quads dropped.
///
/// Degenerate faces are dropped because they never rasterise, and their UVs come
/// from a collapsed unwrap that carries no meaning — including them made the first
/// draft of `a_y_flip_of_the_arrow_rig_moves_no_geometry` compare noise.
///
/// `flip_y` inserts a `scale(1, -1, 1)` between the placement and the model, which
/// is what reusing `LivingEntityRenderer`'s flip would do to a rig authored `+Y`
/// up.
fn posed_parts(model: &EntityModelDef, flip_y: bool) -> Vec<(String, Vec<PosedVertex>)> {
    let sign = if flip_y { -1.0 } else { 1.0 };
    let flip = Affine {
        m: [[1.0, 0.0, 0.0], [0.0, sign, 0.0], [0.0, 0.0, 1.0]],
        t: [0.0, 0.0, 0.0],
    };
    let parts = bake_entity_parts(model);
    // Pre-order with parent index < own index, so one forward pass composes.
    let mut chains: Vec<Affine> = Vec::with_capacity(parts.len());
    let mut out = Vec::new();
    for part in &parts {
        let local = Affine::of_pose(&part.rest);
        let chain = match part.parent {
            Some(p) => chains[p].compose(&local),
            None => flip.compose(&local),
        };
        chains.push(chain);
        let mut verts = Vec::new();
        for q in &part.quads {
            if quad_is_degenerate(&q.positions) {
                continue;
            }
            for (p, uv) in q.positions.iter().zip(q.uvs) {
                let w = chain.apply(*p);
                verts.push([
                    (w[0] * 4096.0).round() as i64,
                    (w[1] * 4096.0).round() as i64,
                    (w[2] * 4096.0).round() as i64,
                    (uv[0] * 65536.0).round() as i64,
                    (uv[1] * 65536.0).round() as i64,
                ]);
            }
        }
        verts.sort_unstable();
        out.push((part.name.clone(), verts));
    }
    out
}

/// Every posed vertex of a rig, whole-rig, sorted.
fn posed_all(model: &EntityModelDef, flip_y: bool) -> Vec<PosedVertex> {
    let mut v: Vec<PosedVertex> = posed_parts(model, flip_y)
        .into_iter()
        .flat_map(|(_, verts)| verts)
        .collect();
    v.sort_unstable();
    v
}

/// **A Y flip of the arrow rig moves no geometry**, and moves no UV except on the
/// fletching plane, where it permutes the four corners of a patch that is
/// (separately, against the real jar) a 4-fold-symmetric plus sign.
///
/// # Why this test exists rather than a Y-flip pixel gate
///
/// A two-direction long-axis pixel test cannot catch a wrong `scale(1, -1, 1)`,
/// since `ArrowModel` is symmetric under `y → −y`. The conclusion drawn from
/// that — "so resolving the
/// flip needs a texel comparison against a captured vanilla frame, or a live
/// oracle" — does not follow, and this test is why. The flip is **not observable at
/// all** on this rig, so no frame and no oracle could settle it: a vanilla frame
/// and a Y-flipped frame are the same frame.
///
/// Three separate facts, and they are separate:
///
/// 1. **No position moves, anywhere in the rig.** `cross_1` (`xRot = π/4`) and
///    `cross_2` (`3π/4`) each span a plane through the shaft axis, and `y → −y`
///    maps each onto the other's plane; the cube's `y ∈ [-2, +2]` extent is
///    symmetric about the pivot, so the swap is exact. `back` is a 45°-rotated
///    square, which is symmetric under `y → −y` outright. So the **silhouette** is
///    identical from every angle — this is the fact the long-axis gate could not
///    have caught, now proved rather than assumed.
///
/// 2. **The two shaft planes' `(position, uv)` pairs are identical too**, because
///    they are built from the *same* `CubeListBuilder` and therefore carry
///    identical UVs: the parts exchange places vertex-for-vertex *and*
///    texel-for-texel. This is the important half, because the shaft box is the one
///    that samples the arrowhead — the only genuinely Y-asymmetric region of
///    `arrow.png` (rows 1 and 3 differ: `D`/`C` at 193/226 grey against `E`/`E` at
///    158).
///
/// 3. **The fletching plane keeps its four UV corners but reassigns them**, by a
///    reflection across a diagonal of its 5×5 patch. That residual is closed by
///    `real_jar.rs`'s `arrow_fletching_patch_is_fully_symmetric`, which checks the
///    patch in Mojang's own PNG: it is a plus sign, invariant under both diagonal
///    reflections, so the reassignment samples the same texel at every corner. That
///    part *is* texture-dependent, which is exactly why it is asserted against the
///    jar rather than argued here.
///
/// The one thing the flip does change is triangle **winding**. That is invisible
/// only because `EntityPipeline` sets `cull_mode: None` and its shader takes
/// `abs(dot(n, light_dir))`. **Turning on back-face culling would make the flip
/// observable again**, and would need a real oracle at that point.
///
/// The trident is the **control** throughout: its rig is genuinely asymmetric in Y
/// (tip at negative `Y`, pole at positive), so the same comparisons must find
/// differences. Without it, a `posed_parts` that returned nothing would make every
/// assertion above pass vacuously.
#[test]
fn a_y_flip_of_the_arrow_rig_moves_no_geometry() {
    use lodestone_assets::entity_models::{arrow_model, trident_model};

    // --- the control first, so a broken detector cannot reach the real assertion
    let t_plain = posed_all(&trident_model(), false);
    let t_flipped = posed_all(&trident_model(), true);
    assert_eq!(
        t_plain.len(),
        5 * 6 * 4,
        "control: the trident's five solid boxes should contribute 30 real quads"
    );
    assert_ne!(
        t_plain, t_flipped,
        "control failed: the trident rig is asymmetric in Y, so a Y flip must change \
         its posed vertices. If it does not, `posed_parts` is not measuring what the \
         assertions below claim and they are vacuous."
    );
    // And specifically its *positions*, not merely its UVs — otherwise the
    // position-only comparison below has no proven detector either.
    let t_pos = |v: &[PosedVertex]| {
        let mut p: Vec<[i64; 3]> = v.iter().map(|q| [q[0], q[1], q[2]]).collect();
        p.sort_unstable();
        p.dedup();
        p
    };
    assert_ne!(
        t_pos(&t_plain),
        t_pos(&t_flipped),
        "control failed: a Y flip must move the trident's positions"
    );

    // --- (1) no position moves
    let plain = posed_all(&arrow_model(), false);
    let flipped = posed_all(&arrow_model(), true);
    assert_eq!(
        plain.len(),
        6 * 4,
        "the arrow has 6 real quads (2 fletching faces + 2 per shaft plane)"
    );
    assert_eq!(
        t_pos(&plain),
        t_pos(&flipped),
        "a Y flip moved the arrow's geometry, so the silhouette is NOT flip-invariant \
         and #380's long-axis gate would after all catch a flip. If this fires the \
         doc comment above is wrong."
    );

    // --- (2) the shaft planes are identical, texels included
    let by_name = |flip: bool| -> std::collections::HashMap<String, Vec<PosedVertex>> {
        posed_parts(&arrow_model(), flip).into_iter().collect()
    };
    let (a, b) = (by_name(false), by_name(true));
    let mut shaft_plain: Vec<PosedVertex> = Vec::new();
    let mut shaft_flipped: Vec<PosedVertex> = Vec::new();
    for part in ["cross_1", "cross_2"] {
        shaft_plain.extend(a[part].iter().copied());
        shaft_flipped.extend(b[part].iter().copied());
    }
    shaft_plain.sort_unstable();
    shaft_flipped.sort_unstable();
    assert_eq!(
        shaft_plain.len(),
        2 * 2 * 4,
        "two shaft planes, two real faces each"
    );
    assert_eq!(
        shaft_plain, shaft_flipped,
        "a Y flip changed which texel lands where on the arrow's shaft/head. That is \
         the one region of `arrow.png` with real Y asymmetry (the arrowhead), so if \
         this fires the flip IS observable and needs a pixel gate or a live oracle."
    );

    // --- (3) the fletching keeps its UV corner set, only reassigning them
    let uv_set = |v: &[PosedVertex]| {
        let mut u: Vec<[i64; 2]> = v.iter().map(|q| [q[3], q[4]]).collect();
        u.sort_unstable();
        u.dedup();
        u
    };
    assert_eq!(
        uv_set(&a["back"]),
        uv_set(&b["back"]),
        "the fletching plane's UV corners changed as a set, not just in assignment — \
         the residual is then not a corner permutation and `real_jar.rs`'s patch-\
         symmetry check does not close it"
    );
    assert_ne!(
        a["back"], b["back"],
        "the fletching's (position, uv) pairing is expected to CHANGE under the flip \
         (a diagonal reflection of its patch). If it does not, fact (3) in the doc \
         comment above is describing something that is not happening, and the \
         real-jar patch-symmetry check it points at is dead weight."
    );
}

/// The trident rig's own structure, since it does **not** fall out of the arrow
/// work: five solid boxes, a mirrored right spike, and a tip at negative `Y`.
///
/// That last fact is what `projectile_pitch_offset_deg("trident") == 90.0` exists
/// for, so it is pinned here rather than left implicit in the render crate.
#[test]
fn the_trident_rig_points_along_negative_y() {
    use lodestone_assets::entity_models::trident_model;

    let model = trident_model();
    assert_eq!((model.texture_width, model.texture_height), (32, 32));
    let quads = bake_entity(&model);
    assert_eq!(quads.len(), 5 * 6, "pole, base, and three spikes");

    let (mut min_y, mut max_y) = (f32::MAX, f32::MIN);
    for q in &quads {
        for p in q.positions {
            min_y = min_y.min(p[1]);
            max_y = max_y.max(p[1]);
        }
    }
    // Spikes at -4 texels, pole top at +27 texels, in blocks.
    assert!((min_y - (-4.0 / 16.0)).abs() < 1e-6, "min y {min_y}");
    assert!((max_y - (27.0 / 16.0)).abs() < 1e-6, "max y {max_y}");
    assert!(
        max_y > -min_y,
        "the long end must be the +Y one: the tip is the short end at {min_y}"
    );
}

/// `ghast_model`'s nine tentacle lengths are `SingleThreadedRandomSource(1660)`
/// draws (`nextInt(7) + 8` each), and used to come from a local reimplementation
/// of `java.util.Random` — since consolidated into `lodestone-javarandom`. The
/// expected lengths below come from an **independent** Python re-implementation
/// of the same LCG (not a call into this workspace's Rust), so this is a real
/// differential check on the rewrite, not `decode(encode(x)) == x` against our
/// own code.
#[test]
fn ghast_tentacle_lengths_match_an_independent_lcg_reimplementation() {
    use lodestone_assets::entity_models::ghast_model;

    let model = ghast_model();
    let mut lengths = Vec::new();
    for i in 0..9 {
        let (name, part) = model
            .root
            .children
            .iter()
            .find(|(n, _)| n == &format!("tentacle{i}"))
            .expect("every tentacle child must be present");
        assert_eq!(name, &format!("tentacle{i}"));
        lengths.push(part.cubes[0].size[1]);
    }
    // NB: `ghast_model` bakes the whole rig at `scaling(4.5)`, but the cube
    // sizes on `PartDef` are pre-bake model-texel values, so these are the raw
    // `nextInt(7) + 8` draws with no scale factor to undo.
    assert_eq!(lengths, vec![8.0, 13.0, 9.0, 11.0, 11.0, 10.0, 12.0, 9.0, 12.0]);
}
