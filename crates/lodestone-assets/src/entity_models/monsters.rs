use super::*;

/// vanilla's own slime model's outer-body-layer construction: the translucent outer shell, a single
/// 8×8×8 cube at `(-4,16,-4)` texOffs `(0,0)`, sheet 64×32. The inner core with
/// eyes/mouth is a second render layer (translucent-over-opaque) and is left to
/// the render pipeline; the outer cube is the recognisable silhouette.
pub fn slime_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO).with_child(
            "cube",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-4.0, 16.0, -4.0],
                [8.0, 8.0, 8.0],
                [0.0, 0.0],
            )),
        ),
    }
}

/// vanilla's own magma cube model's body-layer construction: eight stacked 8×1×8 segments (`y = 16+i`)
/// whose texel offset steps per the vanilla `u,v` schedule, plus a 4×4×4 inside
/// cube. Sheet 64×64. The loop is mirrored structurally, not unrolled.
pub fn magma_cube_model() -> EntityModelDef {
    let mut root = PartDef::new(PartPose::ZERO);
    for i in 0..8i32 {
        let (mut u, mut v) = (0i32, 0i32);
        if i > 0 && i < 4 {
            v += 9 * i;
        } else if i > 3 {
            u = 32;
            v += 9 * i - 36;
        }
        root = root.with_child(
            &format!("cube{i}"),
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-4.0, (16 + i) as f32, -4.0],
                [8.0, 1.0, 8.0],
                [u as f32, v as f32],
            )),
        );
    }
    root = root.with_child(
        "inside_cube",
        PartDef::new(PartPose::ZERO).with_cube(cube(
            [-2.0, 18.0, -2.0],
            [4.0, 4.0, 4.0],
            [24.0, 40.0],
        )),
    );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// Vanilla's own blaze-model body-layer construction: a head plus twelve rods placed on three rings
/// (radii 9/7/5, offset angles `0`/`π/4`/`0.47123894`), sheet 64×32. Positions
/// come from the exact vanilla trig loop; vanilla's own quantized cos/sin are approximated by
/// `f32` trig (sub-pixel identical for placement).
pub fn blaze_model() -> EntityModelDef {
    let mut root = PartDef::new(PartPose::ZERO).with_child(
        "head",
        PartDef::new(PartPose::ZERO).with_cube(cube(
            [-4.0, -4.0, -4.0],
            [8.0, 8.0, 8.0],
            [0.0, 0.0],
        )),
    );
    let rod = |x: f32, y: f32, z: f32| {
        PartDef::new(PartPose::offset(x, y, z)).with_cube(cube(
            [0.0, 0.0, 0.0],
            [2.0, 8.0, 2.0],
            [0.0, 16.0],
        ))
    };
    let mut idx = 0usize;
    // Ring 0: i in 0..4, y = -2 + cos(i*2*0.25); Ring 1: i in 4..8, y = 2 + cos(i*2*0.25);
    // Ring 2: i in 8..12, y = 11 + cos(i*1.5*0.5). The cos-arg uses vanilla's
    // absolute `i`, so the three loops keep distinct `i` ranges.
    {
        let mut angle = 0.0f32;
        for i in 0..4 {
            let x = angle.cos() * 9.0;
            let y = -2.0 + ((i as f32) * 2.0 * 0.25).cos();
            let z = angle.sin() * 9.0;
            root.children.push((format!("part{idx}"), rod(x, y, z)));
            idx += 1;
            angle += PI / 2.0;
        }
        angle = PI / 4.0;
        for i in 4..8 {
            let x = angle.cos() * 7.0;
            let y = 2.0 + ((i as f32) * 2.0 * 0.25).cos();
            let z = angle.sin() * 7.0;
            root.children.push((format!("part{idx}"), rod(x, y, z)));
            idx += 1;
            angle += PI / 2.0;
        }
        angle = 0.47123894;
        for i in 8..12 {
            let x = angle.cos() * 5.0;
            let y = 11.0 + ((i as f32) * 1.5 * 0.5).cos();
            let z = angle.sin() * 5.0;
            root.children.push((format!("part{idx}"), rod(x, y, z)));
            idx += 1;
            angle += PI / 2.0;
        }
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// vanilla's own squid model's body-layer construction: a 12×16×12 body plus eight 2×18×2 tentacles
/// evenly placed on a radius-5 ring, each yaw-rotated to face outward. Sheet
/// 64×32. Positions/rotations come from the exact vanilla loop.
pub fn squid_model() -> EntityModelDef {
    let mut root = PartDef::new(PartPose::ZERO).with_child(
        "body",
        PartDef::new(PartPose::offset(0.0, 8.0, 0.0)).with_cube(cube(
            [-6.0, -8.0, -6.0],
            [12.0, 16.0, 12.0],
            [0.0, 0.0],
        )),
    );
    for i in 0..8i32 {
        let a = (i as f32) * std::f32::consts::TAU / 8.0;
        let x = a.cos() * 5.0;
        let z = a.sin() * 5.0;
        let y_rot = (i as f32) * -std::f32::consts::TAU / 8.0 + PI / 2.0;
        root.children.push((
            format!("tentacle{i}"),
            PartDef::new(PartPose::offset_and_rotation(x, 15.0, z, 0.0, y_rot, 0.0))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 18.0, 2.0], [48.0, 0.0])),
        ));
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// vanilla's own bat model's body-layer construction: body, head with two flat ears, folded wings
/// (each a wing + wing-tip child on the body) and flat feet. Sheet **32×32** —
/// the only small-sheet entry in the batch, which the real-PNG coverage test
/// pins. Zero-thickness parts are intentional (vanilla flat quads).
pub fn bat_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::offset(0.0, 17.0, 0.0))
        .with_cube(cube([-1.5, 0.0, -1.0], [3.0, 5.0, 2.0], [0.0, 0.0]))
        .with_child(
            "right_wing",
            PartDef::new(PartPose::offset(-1.5, 0.0, 0.0))
                .with_cube(cube([-2.0, -2.0, 0.0], [2.0, 7.0, 0.0], [12.0, 0.0]))
                .with_child(
                    "right_wing_tip",
                    PartDef::new(PartPose::offset(-2.0, 0.0, 0.0)).with_cube(cube(
                        [-6.0, -2.0, 0.0],
                        [6.0, 8.0, 0.0],
                        [16.0, 0.0],
                    )),
                ),
        )
        .with_child(
            "left_wing",
            PartDef::new(PartPose::offset(1.5, 0.0, 0.0))
                .with_cube(cube([0.0, -2.0, 0.0], [2.0, 7.0, 0.0], [12.0, 7.0]))
                .with_child(
                    "left_wing_tip",
                    PartDef::new(PartPose::offset(2.0, 0.0, 0.0)).with_cube(cube(
                        [0.0, -2.0, 0.0],
                        [6.0, 8.0, 0.0],
                        [16.0, 8.0],
                    )),
                ),
        )
        .with_child(
            "feet",
            PartDef::new(PartPose::offset(0.0, 5.0, 0.0)).with_cube(cube(
                [-1.5, 0.0, 0.0],
                [3.0, 2.0, 0.0],
                [16.0, 16.0],
            )),
        );
    let head = PartDef::new(PartPose::offset(0.0, 17.0, 0.0))
        .with_cube(cube([-2.0, -3.0, -1.0], [4.0, 3.0, 2.0], [0.0, 7.0]))
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-1.5, -2.0, 0.0)).with_cube(cube(
                [-2.5, -4.0, 0.0],
                [3.0, 5.0, 0.0],
                [1.0, 15.0],
            )),
        )
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(1.1, -3.0, 0.0)).with_cube(cube(
                [-0.1, -3.0, 0.0],
                [3.0, 5.0, 0.0],
                [8.0, 15.0],
            )),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO)
            .with_child("body", body)
            .with_child("head", head),
    }
}

/// vanilla's own enderman model's body-layer construction: starts from the humanoid mesh but replaces
/// every part, so it is authored directly — small head with a `-0.5` hat, a
/// slim body, and the characteristic 2×30×2 arms and legs. Sheet 64×32.
pub fn enderman_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -13.0, 0.0))
        .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]))
        .with_child(
            "hat",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 16.0]).grown(-0.5)),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, -14.0, 0.0)).with_cube(cube(
                [-4.0, 0.0, -2.0],
                [8.0, 12.0, 4.0],
                [32.0, 16.0],
            )),
        )
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-5.0, -12.0, 0.0)).with_cube(cube(
                [-1.0, -2.0, -1.0],
                [2.0, 30.0, 2.0],
                [56.0, 0.0],
            )),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(5.0, -12.0, 0.0))
                .with_cube(cube([-1.0, -2.0, -1.0], [2.0, 30.0, 2.0], [56.0, 0.0]).mirrored()),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-2.0, -5.0, 0.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 30.0, 2.0],
                [56.0, 0.0],
            )),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(2.0, -5.0, 0.0))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 30.0, 2.0], [56.0, 0.0]).mirrored()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

// ===========================================================================
// monster/* remainder (impl-assets lane). Everything below is transcribed from
// vanilla's own per-mob model classes for the 26.2 family. The sibling `animal/*`,
// `npc/*` and `object/*` models are appended to `entity_models()` in their own
// delimited block.
// ===========================================================================

/// Vanilla's own drowned model (extends its own zombie model): the zombie humanoid mesh with the
/// left arm and left leg re-textured (`texOffs (32,48)` / `(16,48)`) and no
/// longer mirrored. Sheet 64×64.
pub fn drowned_model() -> EntityModelDef {
    let mut root = humanoid_root(0.0);
    if let Some(arm) = root.child_mut("left_arm") {
        arm.pose = PartPose::offset(5.0, 2.0, 0.0);
        arm.cubes = vec![cube([-1.0, -2.0, -2.0], [4.0, 12.0, 4.0], [32.0, 48.0])];
    }
    if let Some(leg) = root.child_mut("left_leg") {
        leg.pose = PartPose::offset(1.9, 12.0, 0.0);
        leg.cubes = vec![cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [16.0, 48.0])];
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// Vanilla's own iron-golem model: head (+nose), a broad body (+belt), long arms and stocky
/// legs. Sheet **128×128** — the largest in the corpus, pinned by the real-PNG
/// coverage test.
pub fn iron_golem_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -7.0, -2.0))
        .with_cube(cube([-4.0, -12.0, -5.5], [8.0, 10.0, 8.0], [0.0, 0.0]))
        .with_cube(cube([-1.0, -5.0, -7.5], [2.0, 4.0, 2.0], [24.0, 0.0]));
    let body = PartDef::new(PartPose::offset(0.0, -7.0, 0.0))
        .with_cube(cube([-9.0, -2.0, -6.0], [18.0, 12.0, 11.0], [0.0, 40.0]))
        .with_cube(cube([-4.5, 10.0, -3.0], [9.0, 5.0, 6.0], [0.0, 70.0]).grown(0.5));
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body)
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(0.0, -7.0, 0.0)).with_cube(cube(
                [-13.0, -2.5, -3.0],
                [4.0, 30.0, 6.0],
                [60.0, 21.0],
            )),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(0.0, -7.0, 0.0)).with_cube(cube(
                [9.0, -2.5, -3.0],
                [4.0, 30.0, 6.0],
                [60.0, 58.0],
            )),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-4.0, 11.0, 0.0)).with_cube(cube(
                [-3.5, -3.0, -3.0],
                [6.0, 16.0, 5.0],
                [37.0, 0.0],
            )),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(5.0, 11.0, 0.0))
                .with_cube(cube([-3.5, -3.0, -3.0], [6.0, 16.0, 5.0], [60.0, 0.0]).mirrored()),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// Vanilla's own snow-golem model: two stacked snow spheres, a head and two stick arms posed
/// with a `zRot` of ±1 rad (the right arm additionally yawed by π). All cubes
/// carry a `-0.5` deformation. Sheet 64×64.
pub fn snow_golem_model() -> EntityModelDef {
    let g = -0.5;
    let arm = || cube([-1.0, 0.0, -1.0], [12.0, 2.0, 2.0], [32.0, 0.0]).grown(g);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 4.0, 0.0))
                .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]).grown(g)),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset_and_rotation(5.0, 6.0, 1.0, 0.0, 0.0, 1.0))
                .with_cube(arm()),
        )
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset_and_rotation(
                -5.0, 6.0, -1.0, 0.0, PI, -1.0,
            ))
            .with_cube(arm()),
        )
        .with_child(
            "upper_body",
            PartDef::new(PartPose::offset(0.0, 13.0, 0.0))
                .with_cube(cube([-5.0, -10.0, -5.0], [10.0, 10.0, 10.0], [0.0, 16.0]).grown(g)),
        )
        .with_child(
            "lower_body",
            PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
                .with_cube(cube([-6.0, -12.0, -6.0], [12.0, 12.0, 12.0], [0.0, 36.0]).grown(g)),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// Vanilla's own vex model: a small floating humanoid — head, two-box body, thin arms and two
/// flat wings, all under a `root` pivot offset by `-2.5`. Sheet 32×32.
pub fn vex_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::offset(0.0, 20.0, 0.0))
        .with_cube(cube([-1.5, 0.0, -1.0], [3.0, 4.0, 2.0], [0.0, 10.0]))
        .with_cube(cube([-1.5, 1.0, -1.0], [3.0, 5.0, 2.0], [0.0, 16.0]).grown(-0.2))
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-1.75, 0.25, 0.0))
                .with_cube(cube([-1.25, -0.5, -1.0], [2.0, 4.0, 2.0], [23.0, 0.0]).grown(-0.1)),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(1.75, 0.25, 0.0))
                .with_cube(cube([-0.75, -0.5, -1.0], [2.0, 4.0, 2.0], [23.0, 6.0]).grown(-0.1)),
        )
        .with_child(
            "left_wing",
            PartDef::new(PartPose::offset(0.5, 1.0, 1.0))
                .with_cube(cube([0.0, 0.0, 0.0], [0.0, 5.0, 8.0], [16.0, 14.0]).mirrored()),
        )
        .with_child(
            "right_wing",
            PartDef::new(PartPose::offset(-0.5, 1.0, 1.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [0.0, 5.0, 8.0],
                [16.0, 14.0],
            )),
        );
    let root = PartDef::new(PartPose::offset(0.0, -2.5, 0.0))
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 20.0, 0.0)).with_cube(cube(
                [-2.5, -5.0, -2.5],
                [5.0, 5.0, 5.0],
                [0.0, 0.0],
            )),
        )
        .with_child("body", body);
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO).with_child("root", root),
    }
}

/// The shared segment-worm builder for vanilla's own silverfish/endermite models: each
/// segment is a box sized from `sizes[i]` at `texOffs texs[i]`, dropped to the
/// floor (`y = 24 - height`) and chained along `z` by half the sum of adjacent
/// depths starting at `-3.5`. Returns the root plus the per-segment `z` offsets
/// (the silverfish reuses them to place its raised plates).
fn segment_worm(sizes: &[[i32; 3]], texs: &[[i32; 2]]) -> (PartDef, Vec<f32>) {
    let mut root = PartDef::new(PartPose::ZERO);
    let mut z_place = Vec::with_capacity(sizes.len());
    let mut placement = -3.5f32;
    for i in 0..sizes.len() {
        let (w, h, d) = (sizes[i][0], sizes[i][1], sizes[i][2]);
        root = root.with_child(
            &format!("segment{i}"),
            PartDef::new(PartPose::offset(0.0, (24 - h) as f32, placement)).with_cube(cube(
                [w as f32 * -0.5, 0.0, d as f32 * -0.5],
                [w as f32, h as f32, d as f32],
                [texs[i][0] as f32, texs[i][1] as f32],
            )),
        );
        z_place.push(placement);
        if i + 1 < sizes.len() {
            placement += (d + sizes[i + 1][2]) as f32 * 0.5;
        }
    }
    (root, z_place)
}

/// Vanilla's own silverfish model: a seven-segment worm plus three raised texture plates
/// keyed off the segment `z` offsets. Sheet 64×32.
pub fn silverfish_model() -> EntityModelDef {
    let sizes = [
        [3, 2, 2],
        [4, 3, 2],
        [6, 4, 3],
        [3, 3, 3],
        [2, 2, 3],
        [2, 1, 2],
        [1, 1, 2],
    ];
    let texs = [[0, 0], [0, 4], [0, 9], [0, 16], [0, 22], [11, 0], [13, 4]];
    let (mut root, zp) = segment_worm(&sizes, &texs);
    root = root
        .with_child(
            "plate0",
            PartDef::new(PartPose::offset(0.0, 16.0, zp[2])).with_cube(cube(
                [-5.0, 0.0, sizes[2][2] as f32 * -0.5],
                [10.0, 8.0, sizes[2][2] as f32],
                [20.0, 0.0],
            )),
        )
        .with_child(
            "plate1",
            PartDef::new(PartPose::offset(0.0, 20.0, zp[4])).with_cube(cube(
                [-3.0, 0.0, sizes[4][2] as f32 * -0.5],
                [6.0, 4.0, sizes[4][2] as f32],
                [20.0, 11.0],
            )),
        )
        .with_child(
            "plate2",
            PartDef::new(PartPose::offset(0.0, 19.0, zp[1])).with_cube(cube(
                [-3.0, 0.0, sizes[4][2] as f32 * -0.5],
                [6.0, 5.0, sizes[1][2] as f32],
                [20.0, 18.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// Vanilla's own endermite model: a four-segment worm on the same chaining rule. Sheet 64×32.
pub fn endermite_model() -> EntityModelDef {
    let sizes = [[4, 3, 2], [6, 4, 5], [3, 3, 1], [1, 2, 1]];
    let texs = [[0, 0], [0, 5], [0, 14], [0, 18]];
    let (root, _) = segment_worm(&sizes, &texs);
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// Applies a vanilla mesh-transformer scaling of `factor` to a whole model, exactly
/// as vanilla's own mesh-definition apply step bakes it into the mesh: the root pose is
/// `scaled(factor).translated(0, 24.016*(1-factor), 0)`, i.e. scale about the
/// origin then shift so the model's feet stay planted. Used for the size-variant
/// mobs whose geometry is otherwise identical to a base model.
pub(super) fn scaled(mut model: EntityModelDef, factor: f32) -> EntityModelDef {
    let y_offset = 24.016 * (1.0 - factor);
    let p = &mut model.root.pose;
    // PartPose::scaled multiplies both the offset and the scale by the factor;
    // PartPose::translated then adds the y offset.
    p.x *= factor;
    p.y = p.y * factor + y_offset;
    p.z *= factor;
    p.scale = [
        p.scale[0] * factor,
        p.scale[1] * factor,
        p.scale[2] * factor,
    ];
    model
}

/// Husk: the zombie/humanoid mesh baked at `scaling(1.0625)`
/// (vanilla's own layer-definitions table).
pub fn husk_model() -> EntityModelDef {
    scaled(zombie_model(), 1.0625)
}

/// Wither skeleton: the skeleton mesh baked at `scaling(1.2)`.
pub fn wither_skeleton_model() -> EntityModelDef {
    scaled(skeleton_model(), 1.2)
}

/// Cave spider: the spider mesh baked at `scaling(0.7)`.
pub fn cave_spider_model() -> EntityModelDef {
    scaled(spider_model(), 0.7)
}

/// Ghast (vanilla's own ghast model's body-layer construction): a 16³ body plus nine hanging tentacles
/// whose lengths come from a seeded single-threaded random source (1660) (vanilla's
/// own Java-Random-compatible LCG). The whole mesh is baked at `scaling(4.5)`. The model's
/// UV sheet is 64x32 even though the shipped texture is 128x64 (a 2x texture).
pub fn ghast_model() -> EntityModelDef {
    // Vanilla java.util.Random, so the tentacle lengths are byte-identical to
    // the game rather than eyeballed. This used to be a local reimplementation
    // — a sixth copy of the same LCG the workspace carried, missed by the
    // original five-crate consolidation and found by a grep for the
    // multiplier constant afterwards. It is now `lodestone_javarandom`, the
    // one shared implementation; see `docs/java-random.md`.
    let mut root = PartDef::new(PartPose::ZERO).with_child(
        "body",
        PartDef::new(PartPose::offset(0.0, 17.6, 0.0)).with_cube(cube(
            [-8.0, -8.0, -8.0],
            [16.0, 16.0, 16.0],
            [0.0, 0.0],
        )),
    );

    let mut rng = lodestone_javarandom::JavaRandom::new(1660);
    for i in 0..9i32 {
        // Transcribed verbatim from vanilla's own ghast model: xo uses (i % 3) and (i / 3 % 2)
        // with integer division/modulo; yo uses (i / 3).
        let xo = (((i % 3) as f32 - (i / 3 % 2) as f32 * 0.5 + 0.25) / 2.0 * 2.0 - 1.0) * 5.0;
        let yo = ((i / 3) as f32 / 2.0 * 2.0 - 1.0) * 5.0;
        let len = (rng.next_i32_bound(7) + 8) as f32;
        root = root.with_child(
            &format!("tentacle{i}"),
            PartDef::new(PartPose::offset(xo, 24.6, yo)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, len, 2.0],
                [0.0, 0.0],
            )),
        );
    }

    scaled(
        EntityModelDef {
            texture_width: 64,
            texture_height: 32,
            root,
        },
        4.5,
    )
}

/// Hoglin (vanilla's own hoglin model's body-layer construction, also used for zoglin): a boxy body
/// with a flat mane plane, a tilted head with ears and horns, and four legs.
/// Sheet 128x64.
pub fn hoglin_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset_and_rotation(
        0.0, 2.0, -12.0, 0.87266463, 0.0, 0.0,
    ))
    .with_cube(cube([-7.0, -3.0, -19.0], [14.0, 6.0, 19.0], [61.0, 1.0]))
    .with_child(
        "right_ear",
        PartDef::new(PartPose::offset_and_rotation(
            -6.0,
            -2.0,
            -3.0,
            0.0,
            0.0,
            -PI * 2.0 / 9.0,
        ))
        .with_cube(cube([-6.0, -1.0, -2.0], [6.0, 1.0, 4.0], [1.0, 1.0])),
    )
    .with_child(
        "left_ear",
        PartDef::new(PartPose::offset_and_rotation(
            6.0,
            -2.0,
            -3.0,
            0.0,
            0.0,
            PI * 2.0 / 9.0,
        ))
        .with_cube(cube([0.0, -1.0, -2.0], [6.0, 1.0, 4.0], [1.0, 6.0])),
    )
    .with_child(
        "right_horn",
        PartDef::new(PartPose::offset(-7.0, 2.0, -12.0)).with_cube(cube(
            [-1.0, -11.0, -1.0],
            [2.0, 11.0, 2.0],
            [10.0, 13.0],
        )),
    )
    .with_child(
        "left_horn",
        PartDef::new(PartPose::offset(7.0, 2.0, -12.0)).with_cube(cube(
            [-1.0, -11.0, -1.0],
            [2.0, 11.0, 2.0],
            [1.0, 13.0],
        )),
    );

    let body = PartDef::new(PartPose::offset(0.0, 7.0, 0.0))
        .with_cube(cube([-8.0, -7.0, -13.0], [16.0, 14.0, 26.0], [1.0, 1.0]))
        .with_child(
            "mane",
            PartDef::new(PartPose::offset(0.0, -14.0, -7.0))
                .with_cube(cube([0.0, 0.0, -9.0], [0.0, 10.0, 19.0], [90.0, 33.0]).grown(0.001)),
        );

    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("head", head)
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-4.0, 10.0, -8.5)).with_cube(cube(
                [-3.0, 0.0, -3.0],
                [6.0, 14.0, 6.0],
                [66.0, 42.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(4.0, 10.0, -8.5)).with_cube(cube(
                [-3.0, 0.0, -3.0],
                [6.0, 14.0, 6.0],
                [41.0, 42.0],
            )),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-5.0, 13.0, 10.0)).with_cube(cube(
                [-2.5, 0.0, -2.5],
                [5.0, 11.0, 5.0],
                [21.0, 45.0],
            )),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(5.0, 13.0, 10.0)).with_cube(cube(
                [-2.5, 0.0, -2.5],
                [5.0, 11.0, 5.0],
                [0.0, 45.0],
            )),
        );

    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root,
    }
}

/// Adult strider (vanilla's own adult strider model's body-layer construction): two tall legs, a cubic
/// body, and six flat mirrored bristle planes. Sheet 64x128.
pub fn strider_model() -> EntityModelDef {
    // Each bristle is a flat (zero-height) plane; the three on the right are
    // mirrored. (origin, tex, mirrored, pose offset, zRot)
    type Bristle = ([f32; 3], [f32; 2], bool, [f32; 3], f32);
    let bristles: [Bristle; 6] = [
        (
            [-12.0, 0.0, 0.0],
            [16.0, 65.0],
            true,
            [-8.0, 4.0, -8.0],
            -1.2217305,
        ),
        (
            [-12.0, 0.0, 0.0],
            [16.0, 49.0],
            true,
            [-8.0, -1.0, -8.0],
            -1.134464,
        ),
        (
            [-12.0, 0.0, 0.0],
            [16.0, 33.0],
            true,
            [-8.0, -5.0, -8.0],
            -0.87266463,
        ),
        (
            [0.0, 0.0, 0.0],
            [16.0, 33.0],
            false,
            [8.0, -6.0, -8.0],
            0.87266463,
        ),
        (
            [0.0, 0.0, 0.0],
            [16.0, 49.0],
            false,
            [8.0, -2.0, -8.0],
            1.134464,
        ),
        (
            [0.0, 0.0, 0.0],
            [16.0, 65.0],
            false,
            [8.0, 3.0, -8.0],
            1.2217305,
        ),
    ];
    let names = [
        "right_bottom_bristle",
        "right_middle_bristle",
        "right_top_bristle",
        "left_top_bristle",
        "left_middle_bristle",
        "left_bottom_bristle",
    ];
    let mut body = PartDef::new(PartPose::offset(0.0, 1.0, 0.0)).with_cube(cube(
        [-8.0, -6.0, -8.0],
        [16.0, 14.0, 16.0],
        [0.0, 0.0],
    ));
    for (i, (origin, tex, mirror, off, z_rot)) in bristles.iter().enumerate() {
        let mut c = cube(*origin, [12.0, 0.0, 16.0], *tex);
        if *mirror {
            c = c.mirrored();
        }
        body = body.with_child(
            names[i],
            PartDef::new(PartPose::offset_and_rotation(
                off[0], off[1], off[2], 0.0, 0.0, *z_rot,
            ))
            .with_cube(c),
        );
    }
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-4.0, 8.0, 0.0)).with_cube(cube(
                [-2.0, 0.0, -2.0],
                [4.0, 16.0, 4.0],
                [0.0, 32.0],
            )),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(4.0, 8.0, 0.0)).with_cube(cube(
                [-2.0, 0.0, -2.0],
                [4.0, 16.0, 4.0],
                [0.0, 55.0],
            )),
        )
        .with_child("body", body);
    EntityModelDef {
        texture_width: 64,
        texture_height: 128,
        root,
    }
}

/// Guardian (vanilla's own guardian model's body-layer construction): a five-box head, twelve spikes
/// placed on a cube's faces at their resting offsets, an eye, and a three-part
/// tail. Sheet 64x64.
pub fn guardian_model() -> EntityModelDef {
    // Per-spike face position (SPIKE_X/Y/Z) and rotation multiples of PI
    // (SPIKE_*_ROT), transcribed from vanilla's own guardian model.
    const SPIKE_X: [f32; 12] = [
        0.0, 0.0, 8.0, -8.0, -8.0, 8.0, 8.0, -8.0, 0.0, 0.0, 8.0, -8.0,
    ];
    const SPIKE_Y: [f32; 12] = [
        -8.0, -8.0, -8.0, -8.0, 0.0, 0.0, 0.0, 0.0, 8.0, 8.0, 8.0, 8.0,
    ];
    const SPIKE_Z: [f32; 12] = [
        8.0, -8.0, 0.0, 0.0, -8.0, -8.0, 8.0, 8.0, 8.0, -8.0, 0.0, 0.0,
    ];
    const SPIKE_X_ROT: [f32; 12] = [
        1.75, 0.25, 0.0, 0.0, 0.5, 0.5, 0.5, 0.5, 1.25, 0.75, 0.0, 0.0,
    ];
    const SPIKE_Y_ROT: [f32; 12] = [
        0.0, 0.0, 0.0, 0.0, 0.25, 1.75, 1.25, 0.75, 0.0, 0.0, 0.0, 0.0,
    ];
    const SPIKE_Z_ROT: [f32; 12] = [
        0.0, 0.0, 0.25, 1.75, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.75, 1.25,
    ];

    let mut head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-6.0, 10.0, -8.0], [12.0, 12.0, 16.0], [0.0, 0.0]))
        .with_cube(cube([-8.0, 10.0, -6.0], [2.0, 12.0, 12.0], [0.0, 28.0]))
        .with_cube(cube([6.0, 10.0, -6.0], [2.0, 12.0, 12.0], [0.0, 28.0]).mirrored())
        .with_cube(cube([-6.0, 8.0, -6.0], [12.0, 2.0, 12.0], [16.0, 40.0]))
        .with_cube(cube([-6.0, 22.0, -6.0], [12.0, 2.0, 12.0], [16.0, 40.0]));

    // The resting spike offset baked by vanilla at ageInTicks=0, withdrawal=0:
    // offset = 1 + cos(i) * 0.01.
    for i in 0..12usize {
        let off = 1.0 + (i as f32).cos() * 0.01;
        let x = SPIKE_X[i] * off;
        let y = 16.0 + SPIKE_Y[i] * off;
        let z = SPIKE_Z[i] * off;
        head = head.with_child(
            &format!("spike{i}"),
            PartDef::new(PartPose::offset_and_rotation(
                x,
                y,
                z,
                PI * SPIKE_X_ROT[i],
                PI * SPIKE_Y_ROT[i],
                PI * SPIKE_Z_ROT[i],
            ))
            .with_cube(cube([-1.0, -4.5, -1.0], [2.0, 9.0, 2.0], [0.0, 0.0])),
        );
    }

    head = head.with_child(
        "eye",
        PartDef::new(PartPose::offset(0.0, 0.0, -8.25)).with_cube(cube(
            [-1.0, 15.0, 0.0],
            [2.0, 2.0, 1.0],
            [8.0, 0.0],
        )),
    );

    let tail2 = PartDef::new(PartPose::offset(0.5, 0.5, 6.0))
        .with_cube(cube([0.0, 14.0, 0.0], [2.0, 2.0, 6.0], [41.0, 32.0]))
        .with_cube(cube([1.0, 10.5, 3.0], [1.0, 9.0, 9.0], [25.0, 19.0]));
    let tail1 = PartDef::new(PartPose::offset(-1.5, 0.5, 14.0))
        .with_cube(cube([0.0, 14.0, 0.0], [3.0, 3.0, 7.0], [0.0, 54.0]))
        .with_child("tail2", tail2);
    let tail0 = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-2.0, 14.0, 7.0], [4.0, 4.0, 8.0], [40.0, 0.0]))
        .with_child("tail1", tail1);
    head = head.with_child("tail0", tail0);

    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO).with_child("head", head),
    }
}

/// Adult piglin mesh (vanilla's own adult piglin model's body-layer construction): the vanilla player
/// mesh with the body reduced to its base layer and the head replaced by the
/// piglin head (snout, tusks, floppy ears). Shared by piglin, zombified_piglin
/// and piglin_brute — only the texture differs.
pub fn piglin_model() -> EntityModelDef {
    let mut model = player_model(false);
    let root = &mut model.root;

    // body: base cube only, jacket overlay removed.
    if let Some(body) = root.child_mut("body") {
        body.pose = PartPose::ZERO;
        body.cubes = vec![cube([-4.0, 0.0, -2.0], [8.0, 12.0, 4.0], [16.0, 16.0])];
        body.children.clear();
    }

    // head: piglin snout/tusks, hat overlay removed, floppy ears added.
    if let Some(head) = root.child_mut("head") {
        head.pose = PartPose::ZERO;
        head.cubes = vec![
            cube([-5.0, -8.0, -4.0], [10.0, 8.0, 8.0], [0.0, 0.0]),
            cube([-2.0, -4.0, -5.0], [4.0, 4.0, 1.0], [31.0, 1.0]),
            cube([2.0, -2.0, -5.0], [1.0, 2.0, 1.0], [2.0, 4.0]),
            cube([-3.0, -2.0, -5.0], [1.0, 2.0, 1.0], [2.0, 0.0]),
        ];
        head.children = vec![
            (
                "left_ear".to_string(),
                PartDef::new(PartPose::offset_and_rotation(
                    4.5,
                    -6.0,
                    0.0,
                    0.0,
                    0.0,
                    -PI / 6.0,
                ))
                .with_cube(cube([0.0, 0.0, -2.0], [1.0, 5.0, 4.0], [51.0, 6.0])),
            ),
            (
                "right_ear".to_string(),
                PartDef::new(PartPose::offset_and_rotation(
                    -4.5,
                    -6.0,
                    0.0,
                    0.0,
                    0.0,
                    PI / 6.0,
                ))
                .with_cube(cube([-1.0, 0.0, -2.0], [1.0, 5.0, 4.0], [39.0, 6.0])),
            ),
        ];
    }

    model
}

/// vanilla's own phantom model's body-layer construction: a body carrying a two-segment tail, two
/// two-segment wings (right mirrored) and a head. Eight boxes, sheet 64x64.
pub fn phantom_model() -> EntityModelDef {
    let tail_tip = PartDef::new(PartPose::offset(0.0, 0.5, 6.0)).with_cube(cube(
        [-1.0, 0.0, 0.0],
        [1.0, 1.0, 6.0],
        [4.0, 29.0],
    ));
    let tail_base = PartDef::new(PartPose::offset(0.0, -2.0, 1.0))
        .with_cube(cube([-2.0, 0.0, 0.0], [3.0, 2.0, 6.0], [3.0, 20.0]))
        .with_child("tail_tip", tail_tip);

    let left_wing_tip = PartDef::new(PartPose::offset_and_rotation(6.0, 0.0, 0.0, 0.0, 0.0, 0.1))
        .with_cube(cube([0.0, 0.0, 0.0], [13.0, 1.0, 9.0], [16.0, 24.0]));
    let left_wing_base = PartDef::new(PartPose::offset_and_rotation(
        2.0, -2.0, -8.0, 0.0, 0.0, 0.1,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [6.0, 2.0, 9.0], [23.0, 12.0]))
    .with_child("left_wing_tip", left_wing_tip);

    let right_wing_tip = PartDef::new(PartPose::offset_and_rotation(
        -6.0, 0.0, 0.0, 0.0, 0.0, -0.1,
    ))
    .with_cube(cube([-13.0, 0.0, 0.0], [13.0, 1.0, 9.0], [16.0, 24.0]).mirrored());
    let right_wing_base = PartDef::new(PartPose::offset_and_rotation(
        -3.0, -2.0, -8.0, 0.0, 0.0, -0.1,
    ))
    .with_cube(cube([-6.0, 0.0, 0.0], [6.0, 2.0, 9.0], [23.0, 12.0]).mirrored())
    .with_child("right_wing_tip", right_wing_tip);

    let head = PartDef::new(PartPose::offset_and_rotation(0.0, 1.0, -7.0, 0.2, 0.0, 0.0))
        .with_cube(cube([-4.0, -2.0, -5.0], [7.0, 3.0, 5.0], [0.0, 0.0]));

    let body = PartDef::new(PartPose::rotation(-0.1, 0.0, 0.0))
        .with_cube(cube([-3.0, -2.0, -8.0], [5.0, 3.0, 9.0], [0.0, 8.0]))
        .with_child("tail_base", tail_base)
        .with_child("left_wing_base", left_wing_base)
        .with_child("right_wing_base", right_wing_base)
        .with_child("head", head);

    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO).with_child("body", body),
    }
}

/// vanilla's own warden model's body-layer construction: a rooted `bone` carrying the body (with two
/// flat ribcage planes, a head bearing two flat tendrils, and two long arms)
/// plus two legs. Ten boxes, sheet 128x128.
pub fn warden_model() -> EntityModelDef {
    let right_tendril = PartDef::new(PartPose::offset(-8.0, -12.0, 0.0)).with_cube(cube(
        [-16.0, -13.0, 0.0],
        [16.0, 16.0, 0.0],
        [52.0, 32.0],
    ));
    let left_tendril = PartDef::new(PartPose::offset(8.0, -12.0, 0.0)).with_cube(cube(
        [0.0, -13.0, 0.0],
        [16.0, 16.0, 0.0],
        [58.0, 0.0],
    ));
    let head = PartDef::new(PartPose::offset(0.0, -13.0, 0.0))
        .with_cube(cube([-8.0, -16.0, -5.0], [16.0, 16.0, 10.0], [0.0, 32.0]))
        .with_child("right_tendril", right_tendril)
        .with_child("left_tendril", left_tendril);

    let right_ribcage = PartDef::new(PartPose::offset(-7.0, -2.0, -4.0)).with_cube(cube(
        [-2.0, -11.0, -0.1],
        [9.0, 21.0, 0.0],
        [90.0, 11.0],
    ));
    let left_ribcage = PartDef::new(PartPose::offset(7.0, -2.0, -4.0))
        .with_cube(cube([-7.0, -11.0, -0.1], [9.0, 21.0, 0.0], [90.0, 11.0]).mirrored());
    let right_arm = PartDef::new(PartPose::offset(-13.0, -13.0, 1.0)).with_cube(cube(
        [-4.0, 0.0, -4.0],
        [8.0, 28.0, 8.0],
        [44.0, 50.0],
    ));
    let left_arm = PartDef::new(PartPose::offset(13.0, -13.0, 1.0)).with_cube(cube(
        [-4.0, 0.0, -4.0],
        [8.0, 28.0, 8.0],
        [0.0, 58.0],
    ));

    let body = PartDef::new(PartPose::offset(0.0, -21.0, 0.0))
        .with_cube(cube([-9.0, -13.0, -4.0], [18.0, 21.0, 11.0], [0.0, 0.0]))
        .with_child("right_ribcage", right_ribcage)
        .with_child("left_ribcage", left_ribcage)
        .with_child("head", head)
        .with_child("right_arm", right_arm)
        .with_child("left_arm", left_arm);

    let right_leg = PartDef::new(PartPose::offset(-5.9, -13.0, 0.0)).with_cube(cube(
        [-3.1, 0.0, -3.0],
        [6.0, 13.0, 6.0],
        [76.0, 48.0],
    ));
    let left_leg = PartDef::new(PartPose::offset(5.9, -13.0, 0.0)).with_cube(cube(
        [-2.9, 0.0, -3.0],
        [6.0, 13.0, 6.0],
        [76.0, 76.0],
    ));

    let bone = PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
        .with_child("body", body)
        .with_child("right_leg", right_leg)
        .with_child("left_leg", left_leg);

    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root: PartDef::new(PartPose::ZERO).with_child("bone", bone),
    }
}

/// vanilla's own wither boss model's body-layer construction at the base deformation: shoulders, a
/// four-cube ribcage, a tail, and three heads. Nine boxes, sheet 64x64. The
/// tail's rest pose is placed from `cos/sin(0.20420352)*10`, transcribed rather
/// than pre-computed so it matches vanilla to the last bit.
pub fn wither_model() -> EntityModelDef {
    const RIBCAGE_X_ROT: f32 = 0.20420352;

    let shoulders = PartDef::new(PartPose::ZERO).with_cube(cube(
        [-10.0, 3.9, -0.5],
        [20.0, 3.0, 3.0],
        [0.0, 16.0],
    ));

    let ribcage = PartDef::new(PartPose::offset_and_rotation(
        -2.0,
        6.9,
        -0.5,
        RIBCAGE_X_ROT,
        0.0,
        0.0,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [3.0, 10.0, 3.0], [0.0, 22.0]))
    .with_cube(cube([-4.0, 1.5, 0.5], [11.0, 2.0, 2.0], [24.0, 22.0]))
    .with_cube(cube([-4.0, 4.0, 0.5], [11.0, 2.0, 2.0], [24.0, 22.0]))
    .with_cube(cube([-4.0, 6.5, 0.5], [11.0, 2.0, 2.0], [24.0, 22.0]));

    let tail = PartDef::new(PartPose::offset_and_rotation(
        -2.0,
        6.9 + RIBCAGE_X_ROT.cos() * 10.0,
        -0.5 + RIBCAGE_X_ROT.sin() * 10.0,
        0.83252203,
        0.0,
        0.0,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [3.0, 6.0, 3.0], [12.0, 22.0]));

    let center_head = PartDef::new(PartPose::ZERO).with_cube(cube(
        [-4.0, -4.0, -4.0],
        [8.0, 8.0, 8.0],
        [0.0, 0.0],
    ));
    let side_head = |x: f32| {
        PartDef::new(PartPose::offset(x, 4.0, 0.0)).with_cube(cube(
            [-4.0, -4.0, -4.0],
            [6.0, 6.0, 6.0],
            [32.0, 0.0],
        ))
    };

    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO)
            .with_child("shoulders", shoulders)
            .with_child("ribcage", ribcage)
            .with_child("tail", tail)
            .with_child("center_head", center_head)
            .with_child("right_head", side_head(-8.0))
            .with_child("left_head", side_head(10.0)),
    }
}

/// vanilla's own ender dragon model's body-layer construction: head+jaw, five neck segments and twelve
/// tail segments (all sharing one two-cube "spine" mesh), a body, two wings
/// (each a bone + a flat skin plane) and four three-segment legs. 65 boxes on a
/// 256x256 sheet. The wings' `skin` planes use vanilla's negative `texOffs(-56,
/// ..)` — legitimately off the left edge, which is why the UV gate is a gross
/// sanity envelope, not a strict `[0, 1]` bound.
pub fn ender_dragon_model() -> EntityModelDef {
    // The shared neck/tail segment: a 10-cube plus a small dorsal scale.
    let spine = |x: f32, y: f32, z: f32| {
        PartDef::new(PartPose::offset(x, y, z))
            .with_cube(cube([-5.0, -5.0, -5.0], [10.0, 10.0, 10.0], [192.0, 104.0]))
            .with_cube(cube([-1.0, -9.0, -3.0], [2.0, 4.0, 6.0], [48.0, 0.0]))
    };

    let jaw = PartDef::new(PartPose::offset(0.0, 4.0, -8.0)).with_cube(cube(
        [-6.0, 0.0, -16.0],
        [12.0, 4.0, 16.0],
        [176.0, 65.0],
    ));
    let head = PartDef::new(PartPose::offset(0.0, 20.0, -62.0))
        .with_cube(cube([-6.0, -1.0, -24.0], [12.0, 5.0, 16.0], [176.0, 44.0]))
        .with_cube(cube([-8.0, -8.0, -10.0], [16.0, 16.0, 16.0], [112.0, 30.0]))
        .with_cube(cube([-5.0, -12.0, -4.0], [2.0, 4.0, 6.0], [0.0, 0.0]).mirrored())
        .with_cube(cube([-5.0, -3.0, -22.0], [2.0, 2.0, 4.0], [112.0, 0.0]).mirrored())
        .with_cube(cube([3.0, -12.0, -4.0], [2.0, 4.0, 6.0], [0.0, 0.0]).mirrored())
        .with_cube(cube([3.0, -3.0, -22.0], [2.0, 2.0, 4.0], [112.0, 0.0]).mirrored())
        .with_child("jaw", jaw);

    // One side's leg chain (hip -> tip -> foot). `s` is the x-sign: +1 left,
    // -1 right. Vanilla mirrors by sign of the hip offset, not by tex mirror.
    let leg = |s: f32| {
        let front_foot = PartDef::new(PartPose::offset_and_rotation(
            0.0, 23.0, 0.0, 0.75, 0.0, 0.0,
        ))
        .with_cube(cube([-4.0, 0.0, -12.0], [8.0, 4.0, 16.0], [144.0, 104.0]));
        let front_tip = PartDef::new(PartPose::offset_and_rotation(
            0.0, 20.0, -1.0, -0.5, 0.0, 0.0,
        ))
        .with_cube(cube([-3.0, -1.0, -3.0], [6.0, 24.0, 6.0], [226.0, 138.0]))
        .with_child("front_foot", front_foot);
        let front_leg = PartDef::new(PartPose::offset_and_rotation(
            12.0 * s,
            17.0,
            -6.0,
            1.3,
            0.0,
            0.0,
        ))
        .with_cube(cube([-4.0, -4.0, -4.0], [8.0, 24.0, 8.0], [112.0, 104.0]))
        .with_child("front_tip", front_tip);

        let hind_foot = PartDef::new(PartPose::offset_and_rotation(
            0.0, 31.0, 4.0, 0.75, 0.0, 0.0,
        ))
        .with_cube(cube([-9.0, 0.0, -20.0], [18.0, 6.0, 24.0], [112.0, 0.0]));
        let hind_tip = PartDef::new(PartPose::offset_and_rotation(
            0.0, 32.0, -4.0, 0.5, 0.0, 0.0,
        ))
        .with_cube(cube([-6.0, -2.0, 0.0], [12.0, 32.0, 12.0], [196.0, 0.0]))
        .with_child("hind_foot", hind_foot);
        let hind_leg = PartDef::new(PartPose::offset_and_rotation(
            16.0 * s,
            13.0,
            34.0,
            1.0,
            0.0,
            0.0,
        ))
        .with_cube(cube([-8.0, -4.0, -8.0], [16.0, 32.0, 16.0], [0.0, 0.0]))
        .with_child("hind_tip", hind_tip);
        (front_leg, hind_leg)
    };

    // Left wing bones are tex-mirrored and grow toward +x; right wing is not
    // mirrored and grows toward -x (box origins already at -56).
    let left_wing_tip = PartDef::new(PartPose::offset(56.0, 0.0, 0.0))
        .with_cube(cube([0.0, -2.0, -2.0], [56.0, 4.0, 4.0], [112.0, 136.0]).mirrored())
        .with_cube(cube([0.0, 0.0, 2.0], [56.0, 0.0, 56.0], [-56.0, 144.0]).mirrored());
    let left_wing = PartDef::new(PartPose::offset(12.0, 2.0, -6.0))
        .with_cube(cube([0.0, -4.0, -4.0], [56.0, 8.0, 8.0], [112.0, 88.0]).mirrored())
        .with_cube(cube([0.0, 0.0, 2.0], [56.0, 0.0, 56.0], [-56.0, 88.0]).mirrored())
        .with_child("left_wing_tip", left_wing_tip);

    let right_wing_tip = PartDef::new(PartPose::offset(-56.0, 0.0, 0.0))
        .with_cube(cube([-56.0, -2.0, -2.0], [56.0, 4.0, 4.0], [112.0, 136.0]))
        .with_cube(cube([-56.0, 0.0, 2.0], [56.0, 0.0, 56.0], [-56.0, 144.0]));
    let right_wing = PartDef::new(PartPose::offset(-12.0, 2.0, -6.0))
        .with_cube(cube([-56.0, -4.0, -4.0], [56.0, 8.0, 8.0], [112.0, 88.0]))
        .with_cube(cube([-56.0, 0.0, 2.0], [56.0, 0.0, 56.0], [-56.0, 88.0]))
        .with_child("right_wing_tip", right_wing_tip);

    let (left_front, left_hind) = leg(1.0);
    let (right_front, right_hind) = leg(-1.0);
    let body = PartDef::new(PartPose::offset(0.0, 3.0, 8.0))
        .with_cube(cube([-12.0, 1.0, -16.0], [24.0, 24.0, 64.0], [0.0, 0.0]))
        .with_cube(cube([-1.0, -5.0, -10.0], [2.0, 6.0, 12.0], [220.0, 53.0]))
        .with_cube(cube([-1.0, -5.0, 10.0], [2.0, 6.0, 12.0], [220.0, 53.0]))
        .with_cube(cube([-1.0, -5.0, 30.0], [2.0, 6.0, 12.0], [220.0, 53.0]))
        .with_child("left_wing", left_wing)
        .with_child("left_front_leg", left_front)
        .with_child("left_hind_leg", left_hind)
        .with_child("right_wing", right_wing)
        .with_child("right_front_leg", right_front)
        .with_child("right_hind_leg", right_hind);

    let mut root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body);
    for i in 0..5 {
        root = root.with_child(
            &format!("neck{i}"),
            spine(0.0, 20.0, -12.0 - i as f32 * 10.0),
        );
    }
    for i in 0..12 {
        root = root.with_child(
            &format!("tail{i}"),
            spine(0.0, 10.0, 60.0 + i as f32 * 10.0),
        );
    }

    EntityModelDef {
        texture_width: 256,
        texture_height: 256,
        root,
    }
}

/// vanilla's own witch model's body-layer construction: the villager body (body+jacket, three-cube
/// arms, two legs) and a head bearing the pointy witch hat (four stacked,
/// progressively-rotated segments over the inherited villager hat brim) plus a
/// nose with its mole. Fifteen boxes on a 64x128 sheet, baked at the villager
/// `scaling(0.9375)`. Witch has a single texture (`witch.png`), so unlike the
/// villager itself it needs no profession/type variant seam.
pub fn witch_model() -> EntityModelDef {
    // Villager body/arms/legs (vanilla's own villager-model body-model construction), unchanged.
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 12.0, 6.0], [16.0, 20.0]))
        .with_child(
            "jacket",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 20.0, 6.0], [0.0, 38.0]).grown(0.5)),
        );
    let arms = PartDef::new(PartPose::offset_and_rotation(
        0.0, 3.0, -1.0, -0.75, 0.0, 0.0,
    ))
    .with_cube(cube([-8.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0]))
    .with_cube(cube([4.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0]).mirrored())
    .with_cube(cube([-4.0, 2.0, -2.0], [8.0, 4.0, 4.0], [40.0, 38.0]));
    let right_leg = PartDef::new(PartPose::offset(-2.0, 12.0, 0.0)).with_cube(cube(
        [-2.0, 0.0, -2.0],
        [4.0, 12.0, 4.0],
        [0.0, 22.0],
    ));
    let left_leg = PartDef::new(PartPose::offset(2.0, 12.0, 0.0))
        .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 22.0]).mirrored());

    // Witch head: the villager head cube, the witch hat (with the villager brim
    // inherited beneath it via addOrReplaceChild's child merge), and the nose+mole.
    let hat4 = PartDef::new(PartPose::offset_and_rotation(
        1.75,
        -2.0,
        2.0,
        -PI / 15.0,
        0.0,
        0.10471976,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [1.0, 2.0, 1.0], [0.0, 95.0]).grown(0.25));
    let hat3 = PartDef::new(PartPose::offset_and_rotation(
        1.75,
        -4.0,
        2.0,
        -0.10471976,
        0.0,
        0.05235988,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [4.0, 4.0, 4.0], [0.0, 87.0]))
    .with_child("hat4", hat4);
    let hat2 = PartDef::new(PartPose::offset_and_rotation(
        1.75,
        -4.0,
        2.0,
        -0.05235988,
        0.0,
        0.02617994,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [7.0, 4.0, 7.0], [0.0, 76.0]))
    .with_child("hat3", hat3);
    let hat_rim = PartDef::new(PartPose::rotation(-PI / 2.0, 0.0, 0.0)).with_cube(cube(
        [-8.0, -8.0, -6.0],
        [16.0, 16.0, 1.0],
        [30.0, 47.0],
    ));
    let hat = PartDef::new(PartPose::offset(-5.0, -10.03125, -5.0))
        .with_cube(cube([0.0, 0.0, 0.0], [10.0, 2.0, 10.0], [0.0, 64.0]))
        .with_child("hat_rim", hat_rim)
        .with_child("hat2", hat2);
    let nose = PartDef::new(PartPose::offset(0.0, -2.0, 0.0))
        .with_cube(cube([-1.0, -1.0, -6.0], [2.0, 4.0, 2.0], [24.0, 0.0]))
        .with_child(
            "mole",
            PartDef::new(PartPose::offset(0.0, -2.0, 0.0))
                .with_cube(cube([0.0, 3.0, -6.75], [1.0, 1.0, 1.0], [0.0, 0.0]).grown(-0.25)),
        );
    let head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [0.0, 0.0]))
        .with_child("hat", hat)
        .with_child("nose", nose);

    let model = EntityModelDef {
        texture_width: 64,
        texture_height: 128,
        root: PartDef::new(PartPose::ZERO)
            .with_child("head", head)
            .with_child("body", body)
            .with_child("arms", arms)
            .with_child("right_leg", right_leg)
            .with_child("left_leg", left_leg),
    };
    scaled(model, 0.9375)
}

/// Vanilla's own villager-model body-model construction, sheet 64×64, wrapped
/// in `villagerLikeScale` = a mesh-transformer scaling of `0.9375`. Head carries a hat
/// (deform 0.51) with a flat brim rotated `-π/2`, plus the trademark nose; body
/// carries a jacket overlay (deform 0.5); the arms are one part posed forward
/// (`offsetAndRotation(0,3,-1, -0.75,0,0)`) holding both limb cubes and a
/// connecting cube. 11 boxes.
///
/// Profession/type/biome skins are *overlay layers* composited over this base
/// sheet in vanilla, not a single-sheet swap, so this ships `Fixed` on the plain
/// base texture; overlay compositing is a separate seam from the temperature
/// `ByVariant` swap and is deferred until the shell needs it.
pub fn villager_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [0.0, 0.0]))
        .with_child(
            "hat",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [32.0, 0.0]).grown(0.51))
                .with_child(
                    "hat_rim",
                    PartDef::new(PartPose::rotation(-PI / 2.0, 0.0, 0.0)).with_cube(cube(
                        [-8.0, -8.0, -6.0],
                        [16.0, 16.0, 1.0],
                        [30.0, 47.0],
                    )),
                ),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::offset(0.0, -2.0, 0.0)).with_cube(cube(
                [-1.0, -1.0, -6.0],
                [2.0, 4.0, 2.0],
                [24.0, 0.0],
            )),
        );
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 12.0, 6.0], [16.0, 20.0]))
        .with_child(
            "jacket",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 20.0, 6.0], [0.0, 38.0]).grown(0.5)),
        );
    let arms = PartDef::new(PartPose::offset_and_rotation(
        0.0, 3.0, -1.0, -0.75, 0.0, 0.0,
    ))
    .with_cube(cube([-8.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0]))
    .with_cube(cube([4.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0]).mirrored())
    .with_cube(cube([-4.0, 2.0, -2.0], [8.0, 4.0, 4.0], [40.0, 38.0]));
    let right_leg = PartDef::new(PartPose::offset(-2.0, 12.0, 0.0)).with_cube(cube(
        [-2.0, 0.0, -2.0],
        [4.0, 12.0, 4.0],
        [0.0, 22.0],
    ));
    let left_leg = PartDef::new(PartPose::offset(2.0, 12.0, 0.0))
        .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 22.0]).mirrored());
    let model = EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO)
            .with_child("head", head)
            .with_child("body", body)
            .with_child("arms", arms)
            .with_child("right_leg", right_leg)
            .with_child("left_leg", left_leg),
    };
    scaled(model, 0.9375)
}

/// Vanilla's own zombie-villager-model body-layer construction,
/// sheet 64×64, **no** villager scale (unlike the live villager). Built on the
/// humanoid layout but every part is re-specified: the head holds its own nose
/// cube (tex 24,0) as a second box, a hat (deform 0.5) with a flat brim, a body
/// with a jacket overlay (deform 0.05), straight zombie arms and standard legs.
/// 10 boxes.
///
/// Profession/type overlays composite over this base like the live villager, so
/// it also ships `Fixed` on the plain base sheet.
pub fn zombie_villager_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [0.0, 0.0]))
        .with_cube(cube([-1.0, -3.0, -6.0], [2.0, 4.0, 2.0], [24.0, 0.0]))
        .with_child(
            "hat",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [32.0, 0.0]).grown(0.5))
                .with_child(
                    "hat_rim",
                    PartDef::new(PartPose::rotation(-PI / 2.0, 0.0, 0.0)).with_cube(cube(
                        [-8.0, -8.0, -6.0],
                        [16.0, 16.0, 1.0],
                        [30.0, 47.0],
                    )),
                ),
        );
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 12.0, 6.0], [16.0, 20.0]))
        .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 20.0, 6.0], [0.0, 38.0]).grown(0.05));
    let right_arm = PartDef::new(PartPose::offset(-5.0, 2.0, 0.0)).with_cube(cube(
        [-3.0, -2.0, -2.0],
        [4.0, 12.0, 4.0],
        [44.0, 22.0],
    ));
    let left_arm = PartDef::new(PartPose::offset(5.0, 2.0, 0.0))
        .with_cube(cube([-1.0, -2.0, -2.0], [4.0, 12.0, 4.0], [44.0, 22.0]).mirrored());
    let right_leg = PartDef::new(PartPose::offset(-2.0, 12.0, 0.0)).with_cube(cube(
        [-2.0, 0.0, -2.0],
        [4.0, 12.0, 4.0],
        [0.0, 22.0],
    ));
    let left_leg = PartDef::new(PartPose::offset(2.0, 12.0, 0.0))
        .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 22.0]).mirrored());
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO)
            .with_child("head", head)
            .with_child("body", body)
            .with_child("right_arm", right_arm)
            .with_child("left_arm", left_arm)
            .with_child("right_leg", right_leg)
            .with_child("left_leg", left_leg),
    }
}

// ============================================================================
// animal/*, npc/*, object/* half (owned by this agent; impl-assets owns the
// piglin/guardian/witch/villager/golem/vex/phantom/ghast/silverfish/endermite/
// drowned/strider/warden/wither/ender_dragon lane above). The non-variant
// models (armor_stand, boats, minecart, end_crystal, rabbit..armadillo below)
// landed first; horse family, cat/wolf/ocelot and parrot followed once the
// texture-variant seam (`EntityTexture::ByVariant`, `EntityVariant`) was
// settled — see the second banner further down, right before
// `equine_base_root`.
// ============================================================================
