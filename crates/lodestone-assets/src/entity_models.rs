//! The hand-ported entity-model corpus for the 1.21.5–26.2 family.
//!
//! Entity geometry is **code, not data** in vanilla (see [`crate::entity`]), so
//! each mesh here is transcribed by hand from the decompiled client under
//! `net/minecraft/client/model/...`. This module holds only the *data* (the
//! per-mob [`EntityModelDef`]s and their default texture paths); the version-free
//! baking primitive lives in [`crate::entity`]. In the project's ideal shape this
//! data would live in a version crate the way `AssetProfile` is supplied per
//! version; it lives here for now alongside `player_model`, clearly scoped to the
//! modern family, and the loader/baker never branches on version.
//!
//! Meshes are largely stable across versions, so this is authored once and
//! tweaked per version rather than reimplemented. Texel offsets, box extents,
//! poses and sheet sizes are the exact vanilla values; the sheet size of every
//! entry is checked against the real texture PNG in `client.jar` by the
//! `real_jar` coverage test, so a mistranscribed sheet size cannot pass silently.

use crate::entity::{CubeDef, EntityModelDef, PartDef, PartPose, player_model};
use std::f32::consts::PI;

/// One ported entity model: a stable `name`, the default in-jar texture path
/// (relative to `assets/<ns>/textures/`, no extension), and a builder that
/// produces the bake-ready [`EntityModelDef`].
///
/// The texture path is the *default* skin for biome-varying mobs — 26.2 split
/// pig/cow/chicken into `_temperate`/`_cold`/`_warm` variants and removed the
/// bare `pig.png`, so the temperate variant is the canonical default.
#[derive(Clone, Debug)]
pub struct EntityModelEntry {
    /// Stable identifier for the model (not necessarily the registry id).
    pub name: &'static str,
    /// Default texture path, relative to `assets/<ns>/textures/`, without `.png`.
    pub texture: &'static str,
    /// Builds the bake-ready model.
    pub build: fn() -> EntityModelDef,
}

fn cube(origin: [f32; 3], size: [f32; 3], tex: [f32; 2]) -> CubeDef {
    CubeDef::new(origin, size, tex)
}

/// The shared humanoid mesh (`HumanoidModel.createMesh(g, yOffset=0)`): head with
/// a hat overlay, body, two arms and two legs, on the standard box layout. `g` is
/// the uniform cube deformation (`0.0` for the base layer).
fn humanoid_root(g: f32) -> PartDef {
    let head = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]).grown(g))
        .with_child(
            "hat",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [32.0, 0.0]).grown(g + 0.5)),
        );
    PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
                .with_cube(cube([-4.0, 0.0, -2.0], [8.0, 12.0, 4.0], [16.0, 16.0]).grown(g)),
        )
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-5.0, 2.0, 0.0))
                .with_cube(cube([-3.0, -2.0, -2.0], [4.0, 12.0, 4.0], [40.0, 16.0]).grown(g)),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(5.0, 2.0, 0.0)).with_cube(
                cube([-1.0, -2.0, -2.0], [4.0, 12.0, 4.0], [40.0, 16.0])
                    .grown(g)
                    .mirrored(),
            ),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-1.9, 12.0, 0.0))
                .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0]).grown(g)),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(1.9, 12.0, 0.0)).with_cube(
                cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0])
                    .grown(g)
                    .mirrored(),
            ),
        )
}

/// `ZombieModel` / standard humanoid body layer, sheet 64×64.
pub fn zombie_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: humanoid_root(0.0),
    }
}

/// `SkeletonModel`: the humanoid mesh with thin (2×12×2) arms and legs, sheet
/// 64×32. Arms keep the humanoid pose; legs move to `±2.0`.
pub fn skeleton_model() -> EntityModelDef {
    let mut root = humanoid_root(0.0);
    if let Some(arm) = root.child_mut("right_arm") {
        arm.pose = PartPose::offset(-5.0, 2.0, 0.0);
        arm.cubes = vec![cube([-1.0, -2.0, -1.0], [2.0, 12.0, 2.0], [40.0, 16.0])];
    }
    if let Some(arm) = root.child_mut("left_arm") {
        arm.pose = PartPose::offset(5.0, 2.0, 0.0);
        arm.cubes = vec![cube([-1.0, -2.0, -1.0], [2.0, 12.0, 2.0], [40.0, 16.0]).mirrored()];
    }
    if let Some(leg) = root.child_mut("right_leg") {
        leg.pose = PartPose::offset(-2.0, 12.0, 0.0);
        leg.cubes = vec![cube([-1.0, 0.0, -1.0], [2.0, 12.0, 2.0], [0.0, 16.0])];
    }
    if let Some(leg) = root.child_mut("left_leg") {
        leg.pose = PartPose::offset(2.0, 12.0, 0.0);
        leg.cubes = vec![cube([-1.0, 0.0, -1.0], [2.0, 12.0, 2.0], [0.0, 16.0]).mirrored()];
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `CreeperModel`, sheet 64×32. Head and body at `y=6`; four short legs.
pub fn creeper_model() -> EntityModelDef {
    let leg = || cube([-2.0, 0.0, -2.0], [4.0, 6.0, 4.0], [0.0, 16.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 6.0, 0.0)).with_cube(cube(
                [-4.0, -8.0, -4.0],
                [8.0, 8.0, 8.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 6.0, 0.0)).with_cube(cube(
                [-4.0, 0.0, -2.0],
                [8.0, 12.0, 4.0],
                [16.0, 16.0],
            )),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-2.0, 18.0, 4.0)).with_cube(leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(2.0, 18.0, 4.0)).with_cube(leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-2.0, 18.0, -4.0)).with_cube(leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(2.0, 18.0, -4.0)).with_cube(leg()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `SpiderModel`, sheet 64×32. Head, two body segments, eight 16-long legs posed
/// by `offsetAndRotation` (the rotations are the vanilla rest pose).
pub fn spider_model() -> EntityModelDef {
    let right_leg = || cube([-15.0, -1.0, -1.0], [16.0, 2.0, 2.0], [18.0, 0.0]);
    let left_leg = || cube([-1.0, -1.0, -1.0], [16.0, 2.0, 2.0], [18.0, 0.0]).mirrored();
    let leg = |name: &str, x: f32, z: f32, y_rot: f32, z_rot: f32, mirror: bool| {
        let c = if mirror { left_leg() } else { right_leg() };
        (
            name.to_string(),
            PartDef::new(PartPose::offset_and_rotation(x, 15.0, z, 0.0, y_rot, z_rot)).with_cube(c),
        )
    };
    let mut root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 15.0, -3.0)).with_cube(cube(
                [-4.0, -4.0, -8.0],
                [8.0, 8.0, 8.0],
                [32.0, 4.0],
            )),
        )
        .with_child(
            "body0",
            PartDef::new(PartPose::offset(0.0, 15.0, 0.0)).with_cube(cube(
                [-3.0, -3.0, -3.0],
                [6.0, 6.0, 6.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "body1",
            PartDef::new(PartPose::offset(0.0, 15.0, 9.0)).with_cube(cube(
                [-5.0, -4.0, -6.0],
                [10.0, 8.0, 12.0],
                [0.0, 12.0],
            )),
        );
    let s = 0.58119464_f32;
    let legs = [
        leg("right_hind_leg", -4.0, 2.0, PI / 4.0, -PI / 4.0, false),
        leg("left_hind_leg", 4.0, 2.0, -PI / 4.0, PI / 4.0, true),
        leg("right_middle_hind_leg", -4.0, 1.0, PI / 8.0, -s, false),
        leg("left_middle_hind_leg", 4.0, 1.0, -PI / 8.0, s, true),
        leg("right_middle_front_leg", -4.0, 0.0, -PI / 8.0, -s, false),
        leg("left_middle_front_leg", 4.0, 0.0, PI / 8.0, s, true),
        leg("right_front_leg", -4.0, -1.0, -PI / 4.0, -PI / 4.0, false),
        leg("left_front_leg", 4.0, -1.0, PI / 4.0, PI / 4.0, true),
    ];
    for (name, part) in legs {
        root.children.push((name, part));
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// The shared quadruped mesh (`QuadrupedModel.createBodyMesh`): head, rotated
/// body, and four legs. `mirror_left`/`mirror_right` mirror the respective legs.
fn quadruped_root(leg_size: f32, mirror_left: bool, mirror_right: bool) -> PartDef {
    let right = || {
        let c = cube([-2.0, 0.0, -2.0], [4.0, leg_size, 4.0], [0.0, 16.0]);
        if mirror_right { c.mirrored() } else { c }
    };
    let left = || {
        let c = cube([-2.0, 0.0, -2.0], [4.0, leg_size, 4.0], [0.0, 16.0]);
        if mirror_left { c.mirrored() } else { c }
    };
    PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 18.0 - leg_size, -6.0)).with_cube(cube(
                [-4.0, -4.0, -8.0],
                [8.0, 8.0, 8.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                17.0 - leg_size,
                2.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-5.0, -10.0, -7.0], [10.0, 16.0, 8.0], [28.0, 8.0])),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.0, 24.0 - leg_size, 7.0)).with_cube(right()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.0, 24.0 - leg_size, 7.0)).with_cube(left()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-3.0, 24.0 - leg_size, -5.0)).with_cube(right()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(3.0, 24.0 - leg_size, -5.0)).with_cube(left()),
        )
}

/// `PigModel`: the quadruped base (leg 6, left legs mirrored) with a two-box head
/// (head + snout), sheet 64×64.
pub fn pig_model() -> EntityModelDef {
    let mut root = quadruped_root(6.0, true, false);
    if let Some(head) = root.child_mut("head") {
        head.pose = PartPose::offset(0.0, 12.0, -6.0);
        head.cubes = vec![
            cube([-4.0, -4.0, -8.0], [8.0, 8.0, 8.0], [0.0, 0.0]),
            cube([-2.0, 0.0, -9.0], [4.0, 3.0, 1.0], [16.0, 16.0]),
        ];
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `CowModel` (`createBaseCowModel`): four-box head (head, snout, two horns),
/// two-box body (body + udder), four full-length legs, sheet 64×64.
pub fn cow_model() -> EntityModelDef {
    let right = || cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0]);
    let left = || cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0]).mirrored();
    let head = PartDef::new(PartPose::offset(0.0, 4.0, -8.0))
        .with_cube(cube([-4.0, -4.0, -6.0], [8.0, 8.0, 6.0], [0.0, 0.0]))
        .with_cube(cube([-3.0, 1.0, -7.0], [6.0, 3.0, 1.0], [1.0, 33.0]))
        .with_cube(cube([-5.0, -5.0, -5.0], [1.0, 3.0, 1.0], [22.0, 0.0]))
        .with_cube(cube([4.0, -5.0, -5.0], [1.0, 3.0, 1.0], [22.0, 0.0]));
    let body = PartDef::new(PartPose::offset_and_rotation(
        0.0,
        5.0,
        2.0,
        PI / 2.0,
        0.0,
        0.0,
    ))
    .with_cube(cube([-6.0, -10.0, -7.0], [12.0, 18.0, 10.0], [18.0, 4.0]))
    .with_cube(cube([-2.0, 2.0, -8.0], [4.0, 6.0, 1.0], [52.0, 0.0]));
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-4.0, 12.0, 7.0)).with_cube(right()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(4.0, 12.0, 7.0)).with_cube(left()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-4.0, 12.0, -5.0)).with_cube(right()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(4.0, 12.0, -5.0)).with_cube(left()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `SheepModel` base body (no wool layer): quadruped base (leg 12, right legs
/// mirrored) with an overridden head and body, sheet 64×32.
pub fn sheep_model() -> EntityModelDef {
    let mut root = quadruped_root(12.0, false, true);
    if let Some(head) = root.child_mut("head") {
        head.pose = PartPose::offset(0.0, 6.0, -8.0);
        head.cubes = vec![cube([-3.0, -4.0, -6.0], [6.0, 6.0, 8.0], [0.0, 0.0])];
    }
    if let Some(body) = root.child_mut("body") {
        body.pose = PartPose::offset_and_rotation(0.0, 5.0, 2.0, PI / 2.0, 0.0, 0.0);
        body.cubes = vec![cube([-4.0, -10.0, -7.0], [8.0, 16.0, 6.0], [28.0, 8.0])];
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `AdultChickenModel` (`createBaseChickenModel`): head with beak and wattle
/// children, rotated body, two legs, two wings, sheet 64×32.
pub fn chicken_model() -> EntityModelDef {
    let leg = || cube([-1.0, 0.0, -3.0], [3.0, 5.0, 3.0], [26.0, 0.0]);
    let head = PartDef::new(PartPose::offset(0.0, 15.0, -4.0))
        .with_cube(cube([-2.0, -6.0, -2.0], [4.0, 6.0, 3.0], [0.0, 0.0]))
        .with_child(
            "beak",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-2.0, -4.0, -4.0],
                [4.0, 2.0, 2.0],
                [14.0, 0.0],
            )),
        )
        .with_child(
            "red_thing",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-1.0, -2.0, -3.0],
                [2.0, 2.0, 2.0],
                [14.0, 4.0],
            )),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child(
            "body",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                16.0,
                0.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-3.0, -4.0, -3.0], [6.0, 8.0, 6.0], [0.0, 9.0])),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-2.0, 19.0, 1.0)).with_cube(leg()),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(1.0, 19.0, 1.0)).with_cube(leg()),
        )
        .with_child(
            "right_wing",
            PartDef::new(PartPose::offset(-4.0, 13.0, 0.0)).with_cube(cube(
                [0.0, 0.0, -3.0],
                [1.0, 4.0, 6.0],
                [24.0, 13.0],
            )),
        )
        .with_child(
            "left_wing",
            PartDef::new(PartPose::offset(4.0, 13.0, 0.0)).with_cube(cube(
                [-1.0, 0.0, -3.0],
                [1.0, 4.0, 6.0],
                [24.0, 13.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `SlimeModel.createOuterBodyLayer`: the translucent outer shell, a single
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

/// `MagmaCubeModel.createBodyLayer`: eight stacked 8×1×8 segments (`y = 16+i`)
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
        PartDef::new(PartPose::ZERO).with_cube(cube([-2.0, 18.0, -2.0], [4.0, 4.0, 4.0], [24.0, 40.0])),
    );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `BlazeModel.createBodyLayer`: a head plus twelve rods placed on three rings
/// (radii 9/7/5, offset angles `0`/`π/4`/`0.47123894`), sheet 64×32. Positions
/// come from the exact vanilla trig loop; `Mth.cos`/`sin` are approximated by
/// `f32` trig (sub-pixel identical for placement).
pub fn blaze_model() -> EntityModelDef {
    let mut root = PartDef::new(PartPose::ZERO).with_child(
        "head",
        PartDef::new(PartPose::ZERO).with_cube(cube([-4.0, -4.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0])),
    );
    let rod = |x: f32, y: f32, z: f32| {
        PartDef::new(PartPose::offset(x, y, z))
            .with_cube(cube([0.0, 0.0, 0.0], [2.0, 8.0, 2.0], [0.0, 16.0]))
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

/// `SquidModel.createBodyLayer`: a 12×16×12 body plus eight 2×18×2 tentacles
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
            PartDef::new(PartPose::offset_and_rotation(x, 15.0, z, 0.0, y_rot, 0.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 18.0, 2.0],
                [48.0, 0.0],
            )),
        ));
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `BatModel.createBodyLayer`: body, head with two flat ears, folded wings
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

/// `EndermanModel.createBodyLayer`: starts from the humanoid mesh but replaces
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
            PartDef::new(PartPose::offset(5.0, -12.0, 0.0)).with_cube(
                cube([-1.0, -2.0, -1.0], [2.0, 30.0, 2.0], [56.0, 0.0]).mirrored(),
            ),
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
            PartDef::new(PartPose::offset(2.0, -5.0, 0.0)).with_cube(
                cube([-1.0, 0.0, -1.0], [2.0, 30.0, 2.0], [56.0, 0.0]).mirrored(),
            ),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

// ===========================================================================
// monster/* remainder (impl-assets lane). Everything below is transcribed from
// `net/minecraft/client/model/...` for the 26.2 family. The sibling `animal/*`,
// `npc/*` and `object/*` models are appended to `entity_models()` in their own
// delimited block.
// ===========================================================================

/// `DrownedModel` (extends `ZombieModel`): the zombie humanoid mesh with the
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

/// `IronGolemModel`: head (+nose), a broad body (+belt), long arms and stocky
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
            PartDef::new(PartPose::offset(5.0, 11.0, 0.0)).with_cube(
                cube([-3.5, -3.0, -3.0], [6.0, 16.0, 5.0], [60.0, 0.0]).mirrored(),
            ),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// `SnowGolemModel`: two stacked snow spheres, a head and two stick arms posed
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
            PartDef::new(PartPose::offset_and_rotation(-5.0, 6.0, -1.0, 0.0, PI, -1.0))
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

/// `VexModel`: a small floating humanoid — head, two-box body, thin arms and two
/// flat wings, all under a `root` pivot offset by `-2.5`. Sheet 32×32.
pub fn vex_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::offset(0.0, 20.0, 0.0))
        .with_cube(cube([-1.5, 0.0, -1.0], [3.0, 4.0, 2.0], [0.0, 10.0]))
        .with_cube(cube([-1.5, 1.0, -1.0], [3.0, 5.0, 2.0], [0.0, 16.0]).grown(-0.2))
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-1.75, 0.25, 0.0)).with_cube(
                cube([-1.25, -0.5, -1.0], [2.0, 4.0, 2.0], [23.0, 0.0]).grown(-0.1),
            ),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(1.75, 0.25, 0.0)).with_cube(
                cube([-0.75, -0.5, -1.0], [2.0, 4.0, 2.0], [23.0, 6.0]).grown(-0.1),
            ),
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

/// The shared segment-worm builder for `SilverfishModel`/`EndermiteModel`: each
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

/// `SilverfishModel`: a seven-segment worm plus three raised texture plates
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
    let texs = [
        [0, 0],
        [0, 4],
        [0, 9],
        [0, 16],
        [0, 22],
        [11, 0],
        [13, 4],
    ];
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

/// `EndermiteModel`: a four-segment worm on the same chaining rule. Sheet 64×32.
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

/// Applies a vanilla `MeshTransformer.scaling(factor)` to a whole model, exactly
/// as `LayerDefinition.apply` bakes it into the mesh: the root pose is
/// `scaled(factor).translated(0, 24.016*(1-factor), 0)`, i.e. scale about the
/// origin then shift so the model's feet stay planted. Used for the size-variant
/// mobs whose geometry is otherwise identical to a base model.
fn scaled(mut model: EntityModelDef, factor: f32) -> EntityModelDef {
    let y_offset = 24.016 * (1.0 - factor);
    let p = &mut model.root.pose;
    // PartPose::scaled multiplies both the offset and the scale by the factor;
    // PartPose::translated then adds the y offset.
    p.x *= factor;
    p.y = p.y * factor + y_offset;
    p.z *= factor;
    p.scale = [p.scale[0] * factor, p.scale[1] * factor, p.scale[2] * factor];
    model
}

/// Husk: the zombie/humanoid mesh baked at `scaling(1.0625)`
/// (`LayerDefinitions.java`).
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

/// Ghast (`GhastModel.createBodyLayer`): a 16³ body plus nine hanging tentacles
/// whose lengths come from a seeded `SingleThreadedRandomSource(1660)` (vanilla's
/// java.util.Random LCG). The whole mesh is baked at `scaling(4.5)`. The model's
/// UV sheet is 64x32 even though the shipped texture is 128x64 (a 2x texture).
pub fn ghast_model() -> EntityModelDef {
    // Vanilla java.util.Random, so the tentacle lengths are byte-identical to the
    // game rather than eyeballed.
    struct JavaRng(i64);
    impl JavaRng {
        fn new(seed: i64) -> Self {
            JavaRng((seed ^ 0x5DEECE66D) & ((1i64 << 48) - 1))
        }
        fn next(&mut self, bits: u32) -> i32 {
            self.0 = self.0.wrapping_mul(0x5DEECE66D).wrapping_add(0xB) & ((1i64 << 48) - 1);
            (self.0 >> (48 - bits)) as i32
        }
        fn next_int(&mut self, bound: i32) -> i32 {
            if bound & -bound == bound {
                return ((bound as i64).wrapping_mul(self.next(31) as i64) >> 31) as i32;
            }
            loop {
                let bits = self.next(31);
                let val = bits % bound;
                if bits - val + (bound - 1) >= 0 {
                    return val;
                }
            }
        }
    }

    let mut root = PartDef::new(PartPose::ZERO).with_child(
        "body",
        PartDef::new(PartPose::offset(0.0, 17.6, 0.0))
            .with_cube(cube([-8.0, -8.0, -8.0], [16.0, 16.0, 16.0], [0.0, 0.0])),
    );

    let mut rng = JavaRng::new(1660);
    for i in 0..9i32 {
        // Transcribed verbatim from GhastModel: xo uses (i % 3) and (i / 3 % 2)
        // with integer division/modulo; yo uses (i / 3).
        let xo = (((i % 3) as f32 - (i / 3 % 2) as f32 * 0.5 + 0.25) / 2.0 * 2.0 - 1.0) * 5.0;
        let yo = ((i / 3) as f32 / 2.0 * 2.0 - 1.0) * 5.0;
        let len = (rng.next_int(7) + 8) as f32;
        root = root.with_child(
            &format!("tentacle{i}"),
            PartDef::new(PartPose::offset(xo, 24.6, yo))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, len, 2.0], [0.0, 0.0])),
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

/// Hoglin (`HoglinModel.createBodyLayer`, also used for zoglin): a boxy body
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
        PartDef::new(PartPose::offset(-7.0, 2.0, -12.0))
            .with_cube(cube([-1.0, -11.0, -1.0], [2.0, 11.0, 2.0], [10.0, 13.0])),
    )
    .with_child(
        "left_horn",
        PartDef::new(PartPose::offset(7.0, 2.0, -12.0))
            .with_cube(cube([-1.0, -11.0, -1.0], [2.0, 11.0, 2.0], [1.0, 13.0])),
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
            PartDef::new(PartPose::offset(-4.0, 10.0, -8.5))
                .with_cube(cube([-3.0, 0.0, -3.0], [6.0, 14.0, 6.0], [66.0, 42.0])),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(4.0, 10.0, -8.5))
                .with_cube(cube([-3.0, 0.0, -3.0], [6.0, 14.0, 6.0], [41.0, 42.0])),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-5.0, 13.0, 10.0))
                .with_cube(cube([-2.5, 0.0, -2.5], [5.0, 11.0, 5.0], [21.0, 45.0])),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(5.0, 13.0, 10.0))
                .with_cube(cube([-2.5, 0.0, -2.5], [5.0, 11.0, 5.0], [0.0, 45.0])),
        );

    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root,
    }
}

/// Adult strider (`AdultStriderModel.createBodyLayer`): two tall legs, a cubic
/// body, and six flat mirrored bristle planes. Sheet 64x128.
pub fn strider_model() -> EntityModelDef {
    // Each bristle is a flat (zero-height) plane; the three on the right are
    // mirrored. (origin, tex, mirrored, pose offset, zRot)
    let bristles: [([f32; 3], [f32; 2], bool, [f32; 3], f32); 6] = [
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
    let mut body = PartDef::new(PartPose::offset(0.0, 1.0, 0.0))
        .with_cube(cube([-8.0, -6.0, -8.0], [16.0, 14.0, 16.0], [0.0, 0.0]));
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
            PartDef::new(PartPose::offset(-4.0, 8.0, 0.0))
                .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 16.0, 4.0], [0.0, 32.0])),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(4.0, 8.0, 0.0))
                .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 16.0, 4.0], [0.0, 55.0])),
        )
        .with_child("body", body);
    EntityModelDef {
        texture_width: 64,
        texture_height: 128,
        root,
    }
}

/// Guardian (`GuardianModel.createBodyLayer`): a five-box head, twelve spikes
/// placed on a cube's faces at their resting offsets, an eye, and a three-part
/// tail. Sheet 64x64.
pub fn guardian_model() -> EntityModelDef {
    // Per-spike face position (SPIKE_X/Y/Z) and rotation multiples of PI
    // (SPIKE_*_ROT), transcribed from GuardianModel.
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
        PartDef::new(PartPose::offset(0.0, 0.0, -8.25))
            .with_cube(cube([-1.0, 15.0, 0.0], [2.0, 2.0, 1.0], [8.0, 0.0])),
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

/// Adult piglin mesh (`AdultPiglinModel.createBodyLayer`): the vanilla player
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

// ============================================================================
// animal/*, npc/*, object/* half (owned by this agent; impl-assets owns the
// piglin/guardian/witch/villager/golem/vex/phantom/ghast/silverfish/endermite/
// drowned/strider/warden/wither/ender_dragon lane above). Horse family,
// cat/wolf/ocelot and parrot are deliberately NOT here yet: they select their
// texture sheet from entity metadata (colour/marking variant), and
// `EntityModelEntry.texture` is a single `&'static str` today. That seam is
// still being renegotiated as this is written, so those models are deferred
// rather than forcing a second variant mechanism into the registry.
// ============================================================================

/// `EndCrystalModel.createBodyLayer`: `outer_glass` (8³) with a nested
/// `inner_glass` (same box, `withScale(0.875)`) and a further-nested `cube`
/// (`withScale(0.765625)` = `0.875²`, a literal in vanilla, not computed) plus
/// a separate `base` box. Sheet 64×32.
pub fn end_crystal_model() -> EntityModelDef {
    let glass_cube = || cube([-4.0, -4.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]);
    let mut inner_glass = PartDef::new(PartPose {
        scale: [0.875, 0.875, 0.875],
        ..PartPose::ZERO
    })
    .with_cube(glass_cube());
    inner_glass = inner_glass.with_child(
        "cube",
        PartDef::new(PartPose {
            scale: [0.765625, 0.765625, 0.765625],
            ..PartPose::ZERO
        })
        .with_cube(cube([-4.0, -4.0, -4.0], [8.0, 8.0, 8.0], [32.0, 0.0])),
    );
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "outer_glass",
            PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
                .with_cube(glass_cube())
                .with_child("inner_glass", inner_glass),
        )
        .with_child(
            "base",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-6.0, 0.0, -6.0], [12.0, 4.0, 12.0], [0.0, 16.0])),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `ArmorStandModel.createBodyLayer`: starts from `HumanoidModel.createMesh`
/// but overrides head/body/arms/legs entirely with armor-stand-specific boxes
/// and adds `right_body_stick`/`left_body_stick`/`shoulder_stick`/`base_plate`.
/// The inherited `hat` child from the base humanoid mesh survives the
/// `addOrReplaceChild("head", ...)` merge (vanilla's `PartDefinition` keeps a
/// replaced node's *children*), but the constructor unconditionally sets
/// `this.hat.visible = false` and nothing ever re-enables it — so `hat` is
/// excluded here rather than baked as a permanently-invisible box. Sheet
/// 64×64.
pub fn armor_stand_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 1.0, 0.0))
                .with_cube(cube([-1.0, -7.0, -1.0], [2.0, 7.0, 2.0], [0.0, 0.0])),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-6.0, 0.0, -1.5], [12.0, 3.0, 3.0], [0.0, 26.0])),
        )
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-5.0, 2.0, 0.0))
                .with_cube(cube([-2.0, -2.0, -1.0], [2.0, 12.0, 2.0], [24.0, 0.0])),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(5.0, 2.0, 0.0)).with_cube(
                cube([0.0, -2.0, -1.0], [2.0, 12.0, 2.0], [32.0, 16.0]).mirrored(),
            ),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-1.9, 12.0, 0.0))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 11.0, 2.0], [8.0, 0.0])),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(1.9, 12.0, 0.0)).with_cube(
                cube([-1.0, 0.0, -1.0], [2.0, 11.0, 2.0], [40.0, 16.0]).mirrored(),
            ),
        )
        .with_child(
            "right_body_stick",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-3.0, 3.0, -1.0], [2.0, 7.0, 2.0], [16.0, 0.0])),
        )
        .with_child(
            "left_body_stick",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([1.0, 3.0, -1.0], [2.0, 7.0, 2.0], [48.0, 16.0])),
        )
        .with_child(
            "shoulder_stick",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, 10.0, -1.0], [8.0, 2.0, 2.0], [0.0, 48.0])),
        )
        .with_child(
            "base_plate",
            PartDef::new(PartPose::offset(0.0, 12.0, 0.0))
                .with_cube(cube([-6.0, 11.0, -6.0], [12.0, 1.0, 12.0], [0.0, 32.0])),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The hull + paddles shared by `BoatModel.addCommonParts` (also reused,
/// structurally, by the chest-boat variant which just appends chest boxes).
/// Sheet 128×64 for the plain boat.
fn boat_hull() -> PartDef {
    PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset_and_rotation(0.0, 3.0, 1.0, PI / 2.0, 0.0, 0.0))
                .with_cube(cube([-14.0, -9.0, -3.0], [28.0, 16.0, 3.0], [0.0, 0.0])),
        )
        .with_child(
            "back",
            PartDef::new(PartPose::offset_and_rotation(
                -15.0,
                4.0,
                4.0,
                0.0,
                PI * 3.0 / 2.0,
                0.0,
            ))
            .with_cube(cube([-13.0, -7.0, -1.0], [18.0, 6.0, 2.0], [0.0, 19.0])),
        )
        .with_child(
            "front",
            PartDef::new(PartPose::offset_and_rotation(15.0, 4.0, 0.0, 0.0, PI / 2.0, 0.0))
                .with_cube(cube([-8.0, -7.0, -1.0], [16.0, 6.0, 2.0], [0.0, 27.0])),
        )
        .with_child(
            "right",
            PartDef::new(PartPose::offset_and_rotation(0.0, 4.0, -9.0, 0.0, PI, 0.0))
                .with_cube(cube([-14.0, -7.0, -1.0], [28.0, 6.0, 2.0], [0.0, 35.0])),
        )
        .with_child(
            "left",
            PartDef::new(PartPose::offset(0.0, 4.0, 9.0))
                .with_cube(cube([-14.0, -7.0, -1.0], [28.0, 6.0, 2.0], [0.0, 43.0])),
        )
        .with_child(
            "left_paddle",
            PartDef::new(PartPose::offset_and_rotation(3.0, -5.0, 9.0, 0.0, 0.0, PI / 16.0))
                .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [62.0, 0.0]))
                .with_cube(cube([-1.001, -3.0, 8.0], [1.0, 6.0, 7.0], [62.0, 0.0])),
        )
        .with_child(
            "right_paddle",
            PartDef::new(PartPose::offset_and_rotation(3.0, -5.0, -9.0, 0.0, PI, PI / 16.0))
                .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [62.0, 20.0]))
                .with_cube(cube([0.001, -3.0, 8.0], [1.0, 6.0, 7.0], [62.0, 20.0])),
        )
}

/// `BoatModel.createBoatModel`: hull + both paddles, no chest. Sheet 128×64.
/// (`createWaterPatch`'s 0×0-sheet overlay quad is vanilla's water-clip patch,
/// not a textured model part, and is intentionally not ported.)
pub fn boat_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root: boat_hull(),
    }
}

/// `BoatModel.createChestBoatModel`: the same hull plus `chest_bottom`/
/// `chest_lid`/`chest_lock`. Sheet promotes to 128×128 to fit the chest.
pub fn chest_boat_model() -> EntityModelDef {
    let root = boat_hull()
        .with_child(
            "chest_bottom",
            PartDef::new(PartPose::offset_and_rotation(
                -2.0,
                -5.0,
                -6.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [12.0, 8.0, 12.0], [0.0, 76.0])),
        )
        .with_child(
            "chest_lid",
            PartDef::new(PartPose::offset_and_rotation(
                -2.0,
                -9.0,
                -6.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [12.0, 4.0, 12.0], [0.0, 59.0])),
        )
        .with_child(
            "chest_lock",
            PartDef::new(PartPose::offset_and_rotation(
                -1.0,
                -6.0,
                -1.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [2.0, 4.0, 1.0], [0.0, 59.0])),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// The hull + paddles shared by `RaftModel.addCommonParts`. Same part names as
/// `BoatModel` but a different hull shape (2 boxes, bamboo raft's flatter
/// profile) and paddle texOffs. The `1.5708F` bottom x-rotation is transcribed
/// verbatim (vanilla writes the literal, not `Math.PI / 2`).
fn raft_hull() -> PartDef {
    PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset_and_rotation(0.0, -2.1, 1.0, 1.5708, 0.0, 0.0))
                .with_cube(cube([-14.0, -11.0, -4.0], [28.0, 20.0, 4.0], [0.0, 0.0]))
                .with_cube(cube([-14.0, -9.0, -8.0], [28.0, 16.0, 4.0], [0.0, 0.0])),
        )
        .with_child(
            "left_paddle",
            PartDef::new(PartPose::offset_and_rotation(3.0, -4.0, 9.0, 0.0, 0.0, PI / 16.0))
                .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [0.0, 24.0]))
                .with_cube(cube([-1.001, -3.0, 8.0], [1.0, 6.0, 7.0], [0.0, 24.0])),
        )
        .with_child(
            "right_paddle",
            PartDef::new(PartPose::offset_and_rotation(3.0, -4.0, -9.0, 0.0, PI, PI / 16.0))
                .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [40.0, 24.0]))
                .with_cube(cube([0.001, -3.0, 8.0], [1.0, 6.0, 7.0], [40.0, 24.0])),
        )
}

/// `RaftModel.createRaftModel`: sheet 128×64.
pub fn raft_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root: raft_hull(),
    }
}

/// `RaftModel.createChestRaftModel`: same hull plus chest boxes at raft-specific
/// heights. Sheet 128×128.
pub fn chest_raft_model() -> EntityModelDef {
    let root = raft_hull()
        .with_child(
            "chest_bottom",
            PartDef::new(PartPose::offset_and_rotation(
                -2.0,
                -10.1,
                -6.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [12.0, 8.0, 12.0], [0.0, 76.0])),
        )
        .with_child(
            "chest_lid",
            PartDef::new(PartPose::offset_and_rotation(
                -2.0,
                -14.1,
                -6.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [12.0, 4.0, 12.0], [0.0, 59.0])),
        )
        .with_child(
            "chest_lock",
            PartDef::new(PartPose::offset_and_rotation(
                -1.0,
                -11.1,
                -1.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [2.0, 4.0, 1.0], [0.0, 59.0])),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// `MinecartModel.createBodyLayer`: bottom + 4 walls, flat root. Vanilla's
/// chest/hopper/tnt/command-block/spawner minecarts all reuse this exact
/// geometry class (they differ only by a separate block-overlay render layer,
/// out of scope here), so a single `"minecart"` entry covers them. Sheet
/// 64×32.
pub fn minecart_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset_and_rotation(0.0, 4.0, 0.0, PI / 2.0, 0.0, 0.0))
                .with_cube(cube([-10.0, -8.0, -1.0], [20.0, 16.0, 2.0], [0.0, 10.0])),
        )
        .with_child(
            "front",
            PartDef::new(PartPose::offset_and_rotation(
                -9.0,
                4.0,
                0.0,
                0.0,
                PI * 3.0 / 2.0,
                0.0,
            ))
            .with_cube(cube([-8.0, -9.0, -1.0], [16.0, 8.0, 2.0], [0.0, 0.0])),
        )
        .with_child(
            "back",
            PartDef::new(PartPose::offset_and_rotation(9.0, 4.0, 0.0, 0.0, PI / 2.0, 0.0))
                .with_cube(cube([-8.0, -9.0, -1.0], [16.0, 8.0, 2.0], [0.0, 0.0])),
        )
        .with_child(
            "left",
            PartDef::new(PartPose::offset_and_rotation(0.0, 4.0, -7.0, 0.0, PI, 0.0))
                .with_cube(cube([-8.0, -9.0, -1.0], [16.0, 8.0, 2.0], [0.0, 0.0])),
        )
        .with_child(
            "right",
            PartDef::new(PartPose::offset(0.0, 4.0, 7.0))
                .with_cube(cube([-8.0, -9.0, -1.0], [16.0, 8.0, 2.0], [0.0, 0.0])),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `AdultRabbitModel.createBodyLayer`: body(+tail), head(+ears), and two
/// pivot-only leg-group nodes (`frontlegs`/`backlegs`/`right_hind_leg`/
/// `left_hind_leg`) whose only cubes live on the leaf `*_leg`/`*_haunch`
/// parts. Sheet 64×64.
pub fn rabbit_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::offset_and_rotation(
        0.0,
        23.0,
        4.0,
        -0.3927,
        0.0,
        0.0,
    ))
    .with_cube(cube([-4.0, -6.0, -9.0], [8.0, 6.0, 10.0], [0.0, 0.0]))
    .with_child(
        "tail",
        PartDef::new(PartPose::offset(0.0, -4.9916, 0.0125))
            .with_cube(cube([-2.0, -3.0084, -1.0125], [4.0, 4.0, 4.0], [20.0, 16.0])),
    )
    .with_child(
        "head",
        PartDef::new(PartPose::offset_and_rotation(
            0.0, -5.2929, -8.1213, 0.3927, 0.0, 0.0,
        ))
        .with_cube(cube([-2.5, -3.0, -4.0], [5.0, 5.0, 5.0], [0.0, 16.0]))
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(1.5, -3.7071, -0.8787))
                .with_cube(cube([-1.0, -4.2929, -0.1213], [2.0, 5.0, 1.0], [32.0, 0.0])),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-1.5, -3.7071, -0.8787))
                .with_cube(cube([-1.0, -4.2929, -0.1213], [2.0, 5.0, 1.0], [26.0, 0.0])),
        ),
    )
    .with_child(
        "frontlegs",
        PartDef::new(PartPose::offset(0.0, -1.5349, -6.3108))
            .with_child(
                "right_front_leg",
                PartDef::new(PartPose::offset_and_rotation(
                    -2.0, 1.9239, 0.3827, 0.3927, 0.0, 0.0,
                ))
                .with_cube(cube([-0.9, -1.0, -0.9], [2.0, 4.0, 2.0], [36.0, 18.0])),
            )
            .with_child(
                "left_front_leg",
                PartDef::new(PartPose::offset_and_rotation(
                    2.0, 1.9239, 0.4827, 0.3927, 0.0, 0.0,
                ))
                .with_cube(cube([-1.0, -1.0, -1.0], [2.0, 4.0, 2.0], [44.0, 18.0])),
            ),
    );
    let backlegs = PartDef::new(PartPose::offset(0.0, 23.0, 4.0))
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.0, 0.5, 0.0)).with_child(
                "right_haunch",
                PartDef::new(PartPose::offset_and_rotation(0.0, -0.5, 0.0, 0.0, 0.3927, 0.0))
                    .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 1.0, 6.0], [20.0, 24.0])),
            ),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.0, 0.5, 0.0)).with_child(
                "left_haunch",
                PartDef::new(PartPose::offset_and_rotation(
                    0.0, -0.5, 0.0, 0.0, -0.3927, 0.0,
                ))
                .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 1.0, 6.0], [36.0, 24.0])),
            ),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("backlegs", backlegs);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `AdultFoxModel.createBodyLayer`: head(+ears+nose), body(+tail), and 4 legs.
/// `leftLeg`/`rightLeg` are each a single vanilla `CubeListBuilder` reused
/// across the hind and front leg *on the same side* (identical box, distinct
/// per-side texOffs); the sides are not related by `mirror()` at all. Sheet
/// 48×32.
pub fn fox_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(-1.0, 16.5, -3.0))
        .with_cube(cube([-3.0, -2.0, -5.0], [8.0, 6.0, 6.0], [1.0, 5.0]))
        .with_child(
            "right_ear",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-3.0, -4.0, -4.0], [2.0, 2.0, 1.0], [8.0, 1.0])),
        )
        .with_child(
            "left_ear",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([3.0, -4.0, -4.0], [2.0, 2.0, 1.0], [15.0, 1.0])),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-1.0, 2.01, -8.0], [4.0, 2.0, 3.0], [6.0, 18.0])),
        );
    let body = PartDef::new(PartPose::offset_and_rotation(
        0.0,
        16.0,
        -6.0,
        PI / 2.0,
        0.0,
        0.0,
    ))
    .with_cube(cube([-3.0, 3.999, -3.5], [6.0, 11.0, 6.0], [24.0, 15.0]))
    .with_child(
        "tail",
        PartDef::new(PartPose::offset_and_rotation(
            -4.0,
            15.0,
            -1.0,
            -0.05235988,
            0.0,
            0.0,
        ))
        .with_cube(cube([2.0, 0.0, -1.0], [4.0, 9.0, 5.0], [30.0, 0.0])),
    );
    let leg_fudge = 0.001;
    let left_leg = || cube([2.0, 0.5, -1.0], [2.0, 6.0, 2.0], [4.0, 24.0]).grown(leg_fudge);
    let right_leg = || cube([2.0, 0.5, -1.0], [2.0, 6.0, 2.0], [13.0, 24.0]).grown(leg_fudge);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-5.0, 17.5, 7.0)).with_cube(right_leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(-1.0, 17.5, 7.0)).with_cube(left_leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-5.0, 17.5, 0.0)).with_cube(right_leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(-1.0, 17.5, 0.0)).with_cube(left_leg()),
        );
    EntityModelDef {
        texture_width: 48,
        texture_height: 32,
        root,
    }
}

/// `PandaModel.createBodyLayer` (`QuadrupedModel`): head(+nose+2 ears), body,
/// and 4 legs sharing one vanilla `CubeListBuilder` (no mirroring, identical
/// box on all four). Sheet 64×64.
pub fn panda_model() -> EntityModelDef {
    let leg = || cube([-3.0, 0.0, -3.0], [6.0, 9.0, 6.0], [40.0, 0.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 11.5, -17.0))
                .with_cube(cube([-6.5, -5.0, -4.0], [13.0, 10.0, 9.0], [0.0, 6.0]))
                .with_cube(cube([-3.5, 0.0, -6.0], [7.0, 5.0, 2.0], [45.0, 16.0]))
                .with_cube(cube([3.5, -8.0, -1.0], [5.0, 4.0, 1.0], [52.0, 25.0]))
                .with_cube(cube([-8.5, -8.0, -1.0], [5.0, 4.0, 1.0], [52.0, 25.0])),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                10.0,
                0.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-9.5, -13.0, -6.5], [19.0, 26.0, 13.0], [0.0, 25.0])),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-5.5, 15.0, 9.0)).with_cube(leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(5.5, 15.0, 9.0)).with_cube(leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-5.5, 15.0, -9.0)).with_cube(leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(5.5, 15.0, -9.0)).with_cube(leg()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `GoatModel.createBodyLayer` (`QuadrupedModel`): head builds 3 boxes on one
/// `CubeListBuilder` — `right_ear` (no mirror), `left_ear` (`.mirror()`), then
/// `goatee` — and vanilla's `mirror()` flag is sticky per-builder with no
/// reset, so `goatee` inherits `mirror=true` too. It's a zero-width box so
/// this is visually inert, but transcribed faithfully rather than "corrected"
/// away. Head also carries left_horn/right_horn/nose children (each
/// independently toggleable via `hasLeftHorn`/`hasRightHorn` at runtime, both
/// baked here since this registry has no per-part visibility toggle). Sheet
/// 64×64.
pub fn goat_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(1.0, 14.0, 0.0))
        .with_cube(cube([-6.0, -11.0, -10.0], [3.0, 2.0, 1.0], [2.0, 61.0]))
        .with_cube(cube([2.0, -11.0, -10.0], [3.0, 2.0, 1.0], [2.0, 61.0]).mirrored())
        .with_cube(cube([-0.5, -3.0, -14.0], [0.0, 7.0, 5.0], [23.0, 52.0]).mirrored())
        .with_child(
            "left_horn",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-0.01, -16.0, -10.0], [2.0, 7.0, 2.0], [12.0, 55.0])),
        )
        .with_child(
            "right_horn",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-2.99, -16.0, -10.0], [2.0, 7.0, 2.0], [12.0, 55.0])),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::offset_and_rotation(
                0.0, -8.0, -8.0, 0.9599, 0.0, 0.0,
            ))
            .with_cube(cube([-3.0, -4.0, -8.0], [5.0, 7.0, 10.0], [34.0, 46.0])),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
                .with_cube(cube([-4.0, -17.0, -7.0], [9.0, 11.0, 16.0], [1.0, 1.0]))
                .with_cube(cube([-5.0, -18.0, -8.0], [11.0, 14.0, 11.0], [0.0, 28.0])),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(1.0, 14.0, 4.0))
                .with_cube(cube([0.0, 4.0, 0.0], [3.0, 6.0, 3.0], [36.0, 29.0])),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.0, 14.0, 4.0))
                .with_cube(cube([0.0, 4.0, 0.0], [3.0, 6.0, 3.0], [49.0, 29.0])),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(1.0, 14.0, -6.0))
                .with_cube(cube([0.0, 0.0, 0.0], [3.0, 10.0, 3.0], [49.0, 2.0])),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-3.0, 14.0, -6.0))
                .with_cube(cube([0.0, 0.0, 0.0], [3.0, 10.0, 3.0], [35.0, 2.0])),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `AdultBeeModel.createBodyLayer`: `bone` > `body`(stinger, left/right
/// antenna) plus `bone` > wings/legs. Sheet 64×64.
pub fn bee_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-3.5, -4.0, -5.0], [7.0, 7.0, 10.0], [0.0, 0.0]))
        .with_child(
            "stinger",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([0.0, -1.0, 5.0], [0.0, 1.0, 2.0], [26.0, 7.0])),
        )
        .with_child(
            "left_antenna",
            PartDef::new(PartPose::offset(0.0, -2.0, -5.0))
                .with_cube(cube([1.5, -2.0, -3.0], [1.0, 2.0, 3.0], [2.0, 0.0])),
        )
        .with_child(
            "right_antenna",
            PartDef::new(PartPose::offset(0.0, -2.0, -5.0))
                .with_cube(cube([-2.5, -2.0, -3.0], [1.0, 2.0, 3.0], [2.0, 3.0])),
        );
    let wing_fudge = 0.001;
    let bone = PartDef::new(PartPose::offset(0.0, 19.0, 0.0))
        .with_child("body", body)
        .with_child(
            "right_wing",
            PartDef::new(PartPose::offset_and_rotation(
                -1.5, -4.0, -3.0, 0.0, -0.2618, 0.0,
            ))
            .with_cube(cube([-9.0, 0.0, 0.0], [9.0, 0.0, 6.0], [0.0, 18.0]).grown(wing_fudge)),
        )
        .with_child(
            "left_wing",
            PartDef::new(PartPose::offset_and_rotation(
                1.5, -4.0, -3.0, 0.0, 0.2618, 0.0,
            ))
            .with_cube(
                cube([0.0, 0.0, 0.0], [9.0, 0.0, 6.0], [0.0, 18.0])
                    .grown(wing_fudge)
                    .mirrored(),
            ),
        )
        .with_child(
            "front_legs",
            PartDef::new(PartPose::offset(1.5, 3.0, -2.0))
                .with_cube(cube([-5.0, 0.0, 0.0], [7.0, 2.0, 0.0], [26.0, 1.0])),
        )
        .with_child(
            "middle_legs",
            PartDef::new(PartPose::offset(1.5, 3.0, 0.0))
                .with_cube(cube([-5.0, 0.0, 0.0], [7.0, 2.0, 0.0], [26.0, 3.0])),
        )
        .with_child(
            "back_legs",
            PartDef::new(PartPose::offset(1.5, 3.0, 2.0))
                .with_cube(cube([-5.0, 0.0, 0.0], [7.0, 2.0, 0.0], [26.0, 5.0])),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("bone", bone);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `AdultTurtleModel.createBodyLayer`: head, body(shell+belly), egg_belly
/// (visibility-toggled at runtime by `hasEgg`, baked unconditionally here) and
/// 4 legs. Sheet 128×64.
pub fn turtle_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 19.0, -10.0))
                .with_cube(cube([-3.0, -1.0, -3.0], [6.0, 5.0, 6.0], [3.0, 0.0])),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                11.0,
                -10.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-9.5, 3.0, -10.0], [19.0, 20.0, 6.0], [7.0, 37.0]))
            .with_cube(cube([-5.5, 3.0, -13.0], [11.0, 18.0, 3.0], [31.0, 1.0])),
        )
        .with_child(
            "egg_belly",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                11.0,
                -10.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-4.5, 3.0, -14.0], [9.0, 18.0, 1.0], [70.0, 33.0])),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.5, 22.0, 11.0))
                .with_cube(cube([-2.0, 0.0, 0.0], [4.0, 1.0, 10.0], [1.0, 23.0])),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.5, 22.0, 11.0))
                .with_cube(cube([-2.0, 0.0, 0.0], [4.0, 1.0, 10.0], [1.0, 12.0])),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-5.0, 21.0, -4.0))
                .with_cube(cube([-13.0, 0.0, -2.0], [13.0, 1.0, 5.0], [27.0, 30.0])),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(5.0, 21.0, -4.0))
                .with_cube(cube([0.0, 0.0, -2.0], [13.0, 1.0, 5.0], [27.0, 24.0])),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root,
    }
}

/// `AdultCamelModel.createBodyMesh`: body(+hump+tail), head (3 stacked boxes:
/// muzzle/skull/snout + ears), 4 legs. Sheet 128×128.
pub fn camel_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -3.0, -19.5))
        .with_cube(cube([-3.5, -7.0, -15.0], [7.0, 8.0, 19.0], [60.0, 24.0]))
        .with_cube(cube([-3.5, -21.0, -15.0], [7.0, 14.0, 7.0], [21.0, 0.0]))
        .with_cube(cube([-2.5, -21.0, -21.0], [5.0, 5.0, 6.0], [50.0, 0.0]))
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(2.5, -21.0, -9.5))
                .with_cube(cube([-0.5, 0.5, -1.0], [3.0, 1.0, 2.0], [45.0, 0.0])),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-2.5, -21.0, -9.5))
                .with_cube(cube([-2.5, 0.5, -1.0], [3.0, 1.0, 2.0], [67.0, 0.0])),
        );
    let body = PartDef::new(PartPose::offset(0.0, 4.0, 9.5))
        .with_cube(cube([-7.5, -12.0, -23.5], [15.0, 12.0, 27.0], [0.0, 25.0]))
        .with_child(
            "hump",
            PartDef::new(PartPose::offset(0.0, -12.0, -10.0))
                .with_cube(cube([-4.5, -5.0, -5.5], [9.0, 5.0, 11.0], [74.0, 0.0])),
        )
        .with_child(
            "tail",
            PartDef::new(PartPose::offset(0.0, -9.0, 3.5))
                .with_cube(cube([-1.5, 0.0, 0.0], [3.0, 14.0, 0.0], [122.0, 0.0])),
        )
        .with_child("head", head);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(4.9, 1.0, 9.5))
                .with_cube(cube([-2.5, 2.0, -2.5], [5.0, 21.0, 5.0], [58.0, 16.0])),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-4.9, 1.0, 9.5))
                .with_cube(cube([-2.5, 2.0, -2.5], [5.0, 21.0, 5.0], [94.0, 16.0])),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(4.9, 1.0, -10.5))
                .with_cube(cube([-2.5, 2.0, -2.5], [5.0, 21.0, 5.0], [0.0, 0.0])),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-4.9, 1.0, -10.5))
                .with_cube(cube([-2.5, 2.0, -2.5], [5.0, 21.0, 5.0], [0.0, 26.0])),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// `CodModel.createBodyLayer`: body, head, nose, 2 side fins, tail_fin, and a
/// `top_fin` at `texOffs(20, -6)` on a `0×1×6` box. The **negative Y offset is
/// vanilla's own value**, not a transcription slip: `top_fin`'s real
/// (non-degenerate) faces are the ones spanning the depth×height texel rect
/// that this offset addresses, and there is no non-wrapping `texOffs` vanilla
/// could have chosen instead that keeps that rect on-sheet — see the same
/// wraparound note on `salmon`'s `right_fin`. `every_uv_is_within_the_sheet`
/// carries a named exception for `"cod"` for exactly this box. Sheet 32×32.
pub fn cod_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0))
                .with_cube(cube([-1.0, -2.0, 0.0], [2.0, 4.0, 7.0], [0.0, 0.0])),
        )
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0))
                .with_cube(cube([-1.0, -2.0, -3.0], [2.0, 4.0, 3.0], [11.0, 0.0])),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::offset(0.0, 22.0, -3.0))
                .with_cube(cube([-1.0, -2.0, -1.0], [2.0, 3.0, 1.0], [0.0, 0.0])),
        )
        .with_child(
            "right_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -1.0,
                23.0,
                0.0,
                0.0,
                0.0,
                -PI / 4.0,
            ))
            .with_cube(cube([-2.0, 0.0, -1.0], [2.0, 0.0, 2.0], [22.0, 1.0])),
        )
        .with_child(
            "left_fin",
            PartDef::new(PartPose::offset_and_rotation(1.0, 23.0, 0.0, 0.0, 0.0, PI / 4.0))
                .with_cube(cube([0.0, 0.0, -1.0], [2.0, 0.0, 2.0], [22.0, 4.0])),
        )
        .with_child(
            "tail_fin",
            PartDef::new(PartPose::offset(0.0, 22.0, 7.0))
                .with_cube(cube([0.0, -2.0, 0.0], [0.0, 4.0, 4.0], [22.0, 3.0])),
        )
        .with_child(
            "top_fin",
            PartDef::new(PartPose::offset(0.0, 20.0, 0.0))
                .with_cube(cube([0.0, -1.0, -1.0], [0.0, 1.0, 6.0], [20.0, -6.0])),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// `SalmonModel.createBodyLayer`: body_front/body_back (each with a fin
/// child), head, and 2 side fins. `right_fin` is `texOffs(-4, 0)` on a
/// `2×0×2` box — vanilla relying on `GL_REPEAT` texture wrap for this one
/// real (non-degenerate, since height is the zero dimension here, not width)
/// face rect; verified there is no equivalent non-wrapping offset that fits
/// the required texel span before the sheet boundary. Named exception for
/// `"salmon"` in `every_uv_is_within_the_sheet`. Sheet 32×32.
pub fn salmon_model() -> EntityModelDef {
    let body_front = PartDef::new(PartPose::offset(0.0, 20.0, -7.2))
        .with_cube(cube([-1.5, -2.5, 0.0], [3.0, 5.0, 8.0], [0.0, 0.0]))
        .with_child(
            "top_front_fin",
            PartDef::new(PartPose::offset(0.0, -4.5, 5.0))
                .with_cube(cube([0.0, 0.0, 0.0], [0.0, 2.0, 3.0], [2.0, 1.0])),
        );
    let body_back = PartDef::new(PartPose::offset(0.0, 20.0, 0.8000002))
        .with_cube(cube([-1.5, -2.5, 0.0], [3.0, 5.0, 8.0], [0.0, 13.0]))
        .with_child(
            "back_fin",
            PartDef::new(PartPose::offset(0.0, 0.0, 8.0))
                .with_cube(cube([0.0, -2.5, 0.0], [0.0, 5.0, 6.0], [20.0, 10.0])),
        )
        .with_child(
            "top_back_fin",
            PartDef::new(PartPose::offset(0.0, -4.5, -1.0))
                .with_cube(cube([0.0, 0.0, 0.0], [0.0, 2.0, 4.0], [0.0, 2.0])),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body_front", body_front)
        .with_child("body_back", body_back)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 20.0, -7.2))
                .with_cube(cube([-1.0, -2.0, -3.0], [2.0, 4.0, 3.0], [22.0, 0.0])),
        )
        .with_child(
            "right_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -1.5,
                21.5,
                -7.2,
                0.0,
                0.0,
                -PI / 4.0,
            ))
            .with_cube(cube([-2.0, 0.0, 0.0], [2.0, 0.0, 2.0], [-4.0, 0.0])),
        )
        .with_child(
            "left_fin",
            PartDef::new(PartPose::offset_and_rotation(
                1.5,
                21.5,
                -7.2,
                0.0,
                0.0,
                PI / 4.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [2.0, 0.0, 2.0], [0.0, 0.0])),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// `PufferfishBigModel.createBodyLayer`: body plus 3 mirrored fin pairs (blue,
/// front/back, top/bottom×3). Chosen as the registered pufferfish variant
/// specifically because — unlike `PufferfishSmallModel` (`back_fin` at
/// `texOffs(-3, 0)`) — none of its offsets are negative, so it needs no UV
/// exception. Sheet 32×32.
pub fn pufferfish_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0))
                .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0])),
        )
        .with_child(
            "right_blue_fin",
            PartDef::new(PartPose::offset(-4.0, 15.0, -2.0))
                .with_cube(cube([-2.0, 0.0, -1.0], [2.0, 1.0, 2.0], [24.0, 0.0])),
        )
        .with_child(
            "left_blue_fin",
            PartDef::new(PartPose::offset(4.0, 15.0, -2.0))
                .with_cube(cube([0.0, 0.0, -1.0], [2.0, 1.0, 2.0], [24.0, 3.0])),
        )
        .with_child(
            "top_front_fin",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                14.0,
                -4.0,
                PI / 4.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-4.0, -1.0, 0.0], [8.0, 1.0, 0.0], [15.0, 17.0])),
        )
        .with_child(
            "top_middle_fin",
            PartDef::new(PartPose::offset(0.0, 14.0, 0.0))
                .with_cube(cube([-4.0, -1.0, 0.0], [8.0, 1.0, 1.0], [14.0, 16.0])),
        )
        .with_child(
            "top_back_fin",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                14.0,
                4.0,
                -PI / 4.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-4.0, -1.0, 0.0], [8.0, 1.0, 0.0], [23.0, 18.0])),
        )
        .with_child(
            "right_front_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -4.0,
                22.0,
                -4.0,
                0.0,
                -PI / 4.0,
                0.0,
            ))
            .with_cube(cube([-1.0, -8.0, 0.0], [1.0, 8.0, 0.0], [5.0, 17.0])),
        )
        .with_child(
            "left_front_fin",
            PartDef::new(PartPose::offset_and_rotation(
                4.0,
                22.0,
                -4.0,
                0.0,
                PI / 4.0,
                0.0,
            ))
            .with_cube(cube([0.0, -8.0, 0.0], [1.0, 8.0, 0.0], [1.0, 17.0])),
        )
        .with_child(
            "bottom_front_fin",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                22.0,
                -4.0,
                -PI / 4.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-4.0, 0.0, 0.0], [8.0, 1.0, 0.0], [15.0, 20.0])),
        )
        .with_child(
            "bottom_middle_fin",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0))
                .with_cube(cube([-4.0, 0.0, 0.0], [8.0, 1.0, 0.0], [15.0, 20.0])),
        )
        .with_child(
            "bottom_back_fin",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                22.0,
                4.0,
                PI / 4.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-4.0, 0.0, 0.0], [8.0, 1.0, 0.0], [15.0, 20.0])),
        )
        .with_child(
            "right_back_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -4.0,
                22.0,
                4.0,
                0.0,
                PI / 4.0,
                0.0,
            ))
            .with_cube(cube([-1.0, -8.0, 0.0], [1.0, 8.0, 0.0], [9.0, 17.0])),
        )
        .with_child(
            "left_back_fin",
            PartDef::new(PartPose::offset_and_rotation(
                4.0,
                22.0,
                4.0,
                0.0,
                -PI / 4.0,
                0.0,
            ))
            .with_cube(cube([0.0, -8.0, 0.0], [1.0, 8.0, 0.0], [9.0, 17.0])),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// `TropicalFishLargeModel.createBodyLayer(CubeDeformation.NONE)` — the plain
/// (unpatterned) large tropical fish, chosen over `TropicalFishSmallModel`
/// (which has negative-Y `texOffs` on `tail`/`top_fin`) so this entry needs no
/// UV exception. Sheet 32×32.
pub fn tropical_fish_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 19.0, 0.0))
                .with_cube(cube([-1.0, -3.0, -3.0], [2.0, 6.0, 6.0], [0.0, 20.0])),
        )
        .with_child(
            "tail",
            PartDef::new(PartPose::offset(0.0, 19.0, 3.0))
                .with_cube(cube([0.0, -3.0, 0.0], [0.0, 6.0, 5.0], [21.0, 16.0])),
        )
        .with_child(
            "right_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -1.0,
                20.0,
                0.0,
                0.0,
                PI / 4.0,
                0.0,
            ))
            .with_cube(cube([-2.0, 0.0, 0.0], [2.0, 2.0, 0.0], [2.0, 16.0])),
        )
        .with_child(
            "left_fin",
            PartDef::new(PartPose::offset_and_rotation(
                1.0,
                20.0,
                0.0,
                0.0,
                -PI / 4.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [2.0, 2.0, 0.0], [2.0, 12.0])),
        )
        .with_child(
            "top_fin",
            PartDef::new(PartPose::offset(0.0, 16.0, -3.0))
                .with_cube(cube([0.0, -4.0, 0.0], [0.0, 4.0, 6.0], [20.0, 11.0])),
        )
        .with_child(
            "bottom_fin",
            PartDef::new(PartPose::offset(0.0, 22.0, -3.0))
                .with_cube(cube([0.0, 0.0, 0.0], [0.0, 4.0, 6.0], [20.0, 21.0])),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// `DolphinModel.createBodyLayer`: body(back_fin, mirrored left_fin,
/// right_fin, tail[tail_fin], head[nose]). Sheet 64×64.
pub fn dolphin_model() -> EntityModelDef {
    let tail = PartDef::new(PartPose::offset_and_rotation(
        0.0,
        -2.5,
        11.0,
        -0.10471976,
        0.0,
        0.0,
    ))
    .with_cube(cube([-2.0, -2.5, 0.0], [4.0, 5.0, 11.0], [0.0, 19.0]))
    .with_child(
        "tail_fin",
        PartDef::new(PartPose::offset(0.0, 0.0, 9.0))
            .with_cube(cube([-5.0, -0.5, 0.0], [10.0, 1.0, 6.0], [19.0, 20.0])),
    );
    let head = PartDef::new(PartPose::offset(0.0, -4.0, -3.0))
        .with_cube(cube([-4.0, -3.0, -3.0], [8.0, 7.0, 6.0], [0.0, 0.0]))
        .with_child(
            "nose",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-1.0, 2.0, -7.0], [2.0, 2.0, 4.0], [0.0, 13.0])),
        );
    let body = PartDef::new(PartPose::offset(0.0, 22.0, -5.0))
        .with_cube(cube([-4.0, -7.0, 0.0], [8.0, 7.0, 13.0], [22.0, 0.0]))
        .with_child(
            "back_fin",
            PartDef::new(PartPose::rotation(PI / 3.0, 0.0, 0.0))
                .with_cube(cube([-0.5, 0.0, 8.0], [1.0, 4.0, 5.0], [51.0, 0.0])),
        )
        .with_child(
            "left_fin",
            PartDef::new(PartPose::offset_and_rotation(
                2.0,
                -2.0,
                4.0,
                PI / 3.0,
                0.0,
                PI * 2.0 / 3.0,
            ))
            .with_cube(cube([-0.5, -4.0, 0.0], [1.0, 4.0, 7.0], [48.0, 20.0]).mirrored()),
        )
        .with_child(
            "right_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -2.0,
                -2.0,
                4.0,
                PI / 3.0,
                0.0,
                -PI * 2.0 / 3.0,
            ))
            .with_cube(cube([-0.5, -4.0, 0.0], [1.0, 4.0, 7.0], [48.0, 20.0])),
        )
        .with_child("tail", tail)
        .with_child("head", head);
    let root = PartDef::new(PartPose::ZERO).with_child("body", body);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `AdultAxolotlModel.createBodyLayer`: body (main + a zero-width ridge box),
/// head (`grow=0.001` fudge, matching vanilla's flat-cube-z-fighting fix) with
/// 3 gill children, 4 legs (2 vanilla builders reused across front/hind on
/// each side, distinct origins — not `mirror()`-flagged), tail. Sheet 64×64.
pub fn axolotl_model() -> EntityModelDef {
    let fudge = 0.001;
    let head = PartDef::new(PartPose::offset(0.0, 0.0, -9.0))
        .with_cube(cube([-4.0, -3.0, -5.0], [8.0, 5.0, 5.0], [0.0, 1.0]).grown(fudge))
        .with_child(
            "top_gills",
            PartDef::new(PartPose::offset(0.0, -3.0, -1.0))
                .with_cube(cube([-4.0, -3.0, 0.0], [8.0, 3.0, 0.0], [3.0, 37.0]).grown(fudge)),
        )
        .with_child(
            "left_gills",
            PartDef::new(PartPose::offset(-4.0, 0.0, -1.0))
                .with_cube(cube([-3.0, -5.0, 0.0], [3.0, 7.0, 0.0], [0.0, 40.0]).grown(fudge)),
        )
        .with_child(
            "right_gills",
            PartDef::new(PartPose::offset(4.0, 0.0, -1.0))
                .with_cube(cube([0.0, -5.0, 0.0], [3.0, 7.0, 0.0], [11.0, 40.0]).grown(fudge)),
        );
    let left_leg = || cube([-1.0, 0.0, 0.0], [3.0, 5.0, 0.0], [2.0, 13.0]).grown(fudge);
    let right_leg = || cube([-2.0, 0.0, 0.0], [3.0, 5.0, 0.0], [2.0, 13.0]).grown(fudge);
    let body = PartDef::new(PartPose::offset(0.0, 19.5, 5.0))
        .with_cube(cube([-4.0, -2.0, -9.0], [8.0, 4.0, 10.0], [0.0, 11.0]))
        .with_cube(cube([0.0, -3.0, -8.0], [0.0, 5.0, 9.0], [2.0, 17.0]))
        .with_child("head", head)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.5, 1.0, -1.0)).with_cube(right_leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.5, 1.0, -1.0)).with_cube(left_leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-3.5, 1.0, -8.0)).with_cube(right_leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(3.5, 1.0, -8.0)).with_cube(left_leg()),
        )
        .with_child(
            "tail",
            PartDef::new(PartPose::offset(0.0, 0.0, 1.0))
                .with_cube(cube([0.0, -3.0, 0.0], [0.0, 5.0, 12.0], [2.0, 19.0])),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("body", body);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `FrogModel.createBodyLayer`: `root` (invisible pivot at y=24) > `body`(+
/// `head`[+`eyes`> left/right_eye], `croaking_body`, `tongue`, `left_arm`
/// [+`left_hand`], `right_arm`[+`right_hand`]), and `left_leg`/`right_leg`
/// (each +foot) as siblings of `body` under the same pivot node. `eyes` and
/// the `*_leg`/`*_arm` nodes are themselves pivot-only where noted. Sheet
/// 48×48 (**not** 64×64 — this is a small, easy-to-miss exception).
pub fn frog_model() -> EntityModelDef {
    let eyes = PartDef::new(PartPose::offset(-0.5, 0.0, 2.0))
        .with_child(
            "right_eye",
            PartDef::new(PartPose::offset(-1.5, -3.0, -6.5))
                .with_cube(cube([-1.5, -1.0, -1.5], [3.0, 2.0, 3.0], [0.0, 0.0])),
        )
        .with_child(
            "left_eye",
            PartDef::new(PartPose::offset(2.5, -3.0, -6.5))
                .with_cube(cube([-1.5, -1.0, -1.5], [3.0, 2.0, 3.0], [0.0, 5.0])),
        );
    let head = PartDef::new(PartPose::offset(0.0, -2.0, -1.0))
        .with_cube(cube([-3.5, -1.0, -7.0], [7.0, 0.0, 9.0], [23.0, 13.0]))
        .with_cube(cube([-3.5, -2.0, -7.0], [7.0, 3.0, 9.0], [0.0, 13.0]))
        .with_child("eyes", eyes);
    let left_arm = PartDef::new(PartPose::offset(4.0, -1.0, -6.5))
        .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 3.0], [0.0, 32.0]))
        .with_child(
            "left_hand",
            PartDef::new(PartPose::offset(0.0, 3.0, -1.0))
                .with_cube(cube([-4.0, 0.01, -4.0], [8.0, 0.0, 8.0], [18.0, 40.0])),
        );
    let right_arm = PartDef::new(PartPose::offset(-4.0, -1.0, -6.5))
        .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 3.0], [0.0, 38.0]))
        .with_child(
            "right_hand",
            PartDef::new(PartPose::offset(0.0, 3.0, 0.0))
                .with_cube(cube([-4.0, 0.01, -5.0], [8.0, 0.0, 8.0], [2.0, 40.0])),
        );
    let body = PartDef::new(PartPose::offset(0.0, -2.0, 4.0))
        .with_cube(cube([-3.5, -2.0, -8.0], [7.0, 3.0, 9.0], [3.0, 1.0]))
        .with_cube(cube([-3.5, -1.0, -8.0], [7.0, 0.0, 9.0], [23.0, 22.0]))
        .with_child("head", head)
        .with_child(
            "croaking_body",
            PartDef::new(PartPose::offset(0.0, -1.0, -5.0)).with_cube(
                cube([-3.5, -0.1, -2.9], [7.0, 2.0, 3.0], [26.0, 5.0]).grown(-0.1),
            ),
        )
        .with_child(
            "tongue",
            PartDef::new(PartPose::offset(0.0, -1.01, 1.0))
                .with_cube(cube([-2.0, 0.0, -7.1], [4.0, 0.0, 7.0], [17.0, 13.0])),
        )
        .with_child("left_arm", left_arm)
        .with_child("right_arm", right_arm);
    let model_root = PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
        .with_child("body", body)
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(3.5, -3.0, 4.0))
                .with_cube(cube([-1.0, 0.0, -2.0], [3.0, 3.0, 4.0], [14.0, 25.0]))
                .with_child(
                    "left_foot",
                    PartDef::new(PartPose::offset(2.0, 3.0, 0.0))
                        .with_cube(cube([-4.0, 0.01, -4.0], [8.0, 0.0, 8.0], [2.0, 32.0])),
                ),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-3.5, -3.0, 4.0))
                .with_cube(cube([-2.0, 0.0, -2.0], [3.0, 3.0, 4.0], [0.0, 25.0]))
                .with_child(
                    "right_foot",
                    PartDef::new(PartPose::offset(-2.0, 3.0, 0.0))
                        .with_cube(cube([-4.0, 0.01, -4.0], [8.0, 0.0, 8.0], [18.0, 32.0])),
                ),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("root", model_root);
    EntityModelDef {
        texture_width: 48,
        texture_height: 48,
        root,
    }
}

/// `TadpoleModel.createBodyLayer`: 2 flat boxes, no hierarchy. Sheet is
/// **16×16** — the smallest sheet in the whole corpus, easy to fat-finger as
/// 32×32.
pub fn tadpole_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 22.0, -3.0))
                .with_cube(cube([-1.5, -1.0, 0.0], [3.0, 2.0, 3.0], [0.0, 0.0])),
        )
        .with_child(
            "tail",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0))
                .with_cube(cube([0.0, -1.0, 0.0], [0.0, 2.0, 7.0], [0.0, 0.0])),
        );
    EntityModelDef {
        texture_width: 16,
        texture_height: 16,
        root,
    }
}

/// `SnifferModel.createBodyLayer`: `bone` > `body`(3 boxes) + 6 legs, `body` >
/// `head`(2 boxes) > ears/nose/lower_beak. Sheet is **192×192** — by far the
/// largest sheet in the corpus.
pub fn sniffer_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, 6.5, -19.48))
        .with_cube(cube([-6.5, -7.5, -11.5], [13.0, 18.0, 11.0], [8.0, 15.0]))
        .with_cube(cube([-6.5, 7.5, -11.5], [13.0, 0.0, 11.0], [8.0, 4.0]))
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(6.51, -7.5, -4.51))
                .with_cube(cube([0.0, 0.0, -3.0], [1.0, 19.0, 7.0], [2.0, 0.0])),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-6.51, -7.5, -4.51))
                .with_cube(cube([-1.0, 0.0, -3.0], [1.0, 19.0, 7.0], [48.0, 0.0])),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::offset(0.0, -4.5, -11.5))
                .with_cube(cube([-6.5, -2.0, -9.0], [13.0, 2.0, 9.0], [10.0, 45.0])),
        )
        .with_child(
            "lower_beak",
            PartDef::new(PartPose::offset(0.0, 2.5, -12.5))
                .with_cube(cube([-6.5, -7.0, -8.0], [13.0, 12.0, 9.0], [10.0, 57.0])),
        );
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-12.5, -14.0, -20.0], [25.0, 29.0, 40.0], [62.0, 68.0]))
        .with_cube(cube([-12.5, -14.0, -20.0], [25.0, 24.0, 40.0], [62.0, 0.0]).grown(0.5))
        .with_cube(cube([-12.5, 12.0, -20.0], [25.0, 0.0, 40.0], [87.0, 68.0]))
        .with_child("head", head);
    let bone = PartDef::new(PartPose::offset(0.0, 5.0, 0.0))
        .with_child("body", body)
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-7.5, 10.0, -15.0))
                .with_cube(cube([-3.5, -1.0, -4.0], [7.0, 10.0, 8.0], [32.0, 87.0])),
        )
        .with_child(
            "right_mid_leg",
            PartDef::new(PartPose::offset(-7.5, 10.0, 0.0))
                .with_cube(cube([-3.5, -1.0, -4.0], [7.0, 10.0, 8.0], [32.0, 105.0])),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-7.5, 10.0, 15.0))
                .with_cube(cube([-3.5, -1.0, -4.0], [7.0, 10.0, 8.0], [32.0, 123.0])),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(7.5, 10.0, -15.0))
                .with_cube(cube([-3.5, -1.0, -4.0], [7.0, 10.0, 8.0], [0.0, 87.0])),
        )
        .with_child(
            "left_mid_leg",
            PartDef::new(PartPose::offset(7.5, 10.0, 0.0))
                .with_cube(cube([-3.5, -1.0, -4.0], [7.0, 10.0, 8.0], [0.0, 105.0])),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(7.5, 10.0, 15.0))
                .with_cube(cube([-3.5, -1.0, -4.0], [7.0, 10.0, 8.0], [0.0, 123.0])),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("bone", bone);
    EntityModelDef {
        texture_width: 192,
        texture_height: 192,
        root,
    }
}

/// `AdultArmadilloModel.createBodyLayer`: body(+tail+head[+ears]), 4 legs, and
/// a separate root-level `cube` (the rolled-up ball form; vanilla toggles its
/// visibility with the roll animation state, baked unconditionally here since
/// this registry has no per-part runtime visibility). Sheet 64×64.
pub fn armadillo_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -2.0, -11.0))
        .with_child(
            "head_cube",
            PartDef::new(PartPose::offset_and_rotation(
                0.0, 0.0, 0.0, -0.3927, 0.0, 0.0,
            ))
            .with_cube(cube([-1.5, -1.0, -1.0], [3.0, 5.0, 2.0], [43.0, 15.0])),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-1.0, -1.0, 0.0)).with_child(
                "right_ear_cube",
                PartDef::new(PartPose::offset_and_rotation(
                    -0.5, 0.0, -0.6, 0.1886, -0.3864, -0.0718,
                ))
                .with_cube(cube([-2.0, -3.0, 0.0], [2.0, 5.0, 0.0], [43.0, 10.0])),
            ),
        )
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(1.0, -2.0, 0.0)).with_child(
                "left_ear_cube",
                PartDef::new(PartPose::offset_and_rotation(
                    0.5, 1.0, -0.6, 0.1886, 0.3864, 0.0718,
                ))
                .with_cube(cube([0.0, -3.0, 0.0], [2.0, 5.0, 0.0], [47.0, 10.0])),
            ),
        );
    let body = PartDef::new(PartPose::offset(0.0, 21.0, 4.0))
        .with_cube(cube([-4.0, -7.0, -10.0], [8.0, 8.0, 12.0], [0.0, 20.0]).grown(0.3))
        .with_cube(cube([-4.0, -7.0, -10.0], [8.0, 8.0, 12.0], [0.0, 40.0]))
        .with_child(
            "tail",
            PartDef::new(PartPose::offset_and_rotation(
                0.0, -3.0, 1.0, 0.5061, 0.0, 0.0,
            ))
            .with_cube(cube([-0.5, -0.0865, 0.0933], [1.0, 6.0, 1.0], [44.0, 53.0])),
        )
        .with_child("head", head);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-2.0, 21.0, 4.0))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 2.0], [51.0, 31.0])),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(2.0, 21.0, 4.0))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 2.0], [42.0, 31.0])),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-2.0, 21.0, -4.0))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 2.0], [51.0, 43.0])),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(2.0, 21.0, -4.0))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 2.0], [42.0, 43.0])),
        )
        .with_child(
            "cube",
            PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
                .with_cube(cube([-5.0, -10.0, -6.0], [10.0, 10.0, 10.0], [0.0, 0.0])),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

fn player_wide() -> EntityModelDef {
    player_model(false)
}

fn player_slim() -> EntityModelDef {
    player_model(true)
}

/// The ported entity-model corpus, in a fixed order (priority: player first,
/// then the common overworld set). Growing this list is how coverage climbs.
pub fn entity_models() -> Vec<EntityModelEntry> {
    vec![
        EntityModelEntry {
            name: "player_wide",
            texture: "entity/player/wide/steve",
            build: player_wide,
        },
        EntityModelEntry {
            name: "player_slim",
            texture: "entity/player/slim/alex",
            build: player_slim,
        },
        EntityModelEntry {
            name: "zombie",
            texture: "entity/zombie/zombie",
            build: zombie_model,
        },
        EntityModelEntry {
            name: "skeleton",
            texture: "entity/skeleton/skeleton",
            build: skeleton_model,
        },
        EntityModelEntry {
            name: "creeper",
            texture: "entity/creeper/creeper",
            build: creeper_model,
        },
        EntityModelEntry {
            name: "spider",
            texture: "entity/spider/spider",
            build: spider_model,
        },
        EntityModelEntry {
            name: "pig",
            texture: "entity/pig/pig_temperate",
            build: pig_model,
        },
        EntityModelEntry {
            name: "cow",
            texture: "entity/cow/cow_temperate",
            build: cow_model,
        },
        EntityModelEntry {
            name: "sheep",
            texture: "entity/sheep/sheep",
            build: sheep_model,
        },
        EntityModelEntry {
            name: "chicken",
            texture: "entity/chicken/chicken_temperate",
            build: chicken_model,
        },
        // Tier 2: common overworld/hostile expansion. Texture-only variants reuse
        // an existing builder because vanilla renders them with the same model
        // class. Where vanilla applies a `MeshTransformer.scaling(..)` to the
        // layer (baked into the mesh, not the renderer), we wrap the base builder
        // in `scaled(..)` so the geometry carries the same size.
        EntityModelEntry {
            name: "husk",
            texture: "entity/zombie/husk",
            build: husk_model,
        },
        EntityModelEntry {
            name: "stray",
            texture: "entity/skeleton/stray",
            build: skeleton_model,
        },
        EntityModelEntry {
            name: "wither_skeleton",
            texture: "entity/skeleton/wither_skeleton",
            build: wither_skeleton_model,
        },
        EntityModelEntry {
            name: "cave_spider",
            texture: "entity/spider/cave_spider",
            build: cave_spider_model,
        },
        EntityModelEntry {
            name: "slime",
            texture: "entity/slime/slime",
            build: slime_model,
        },
        EntityModelEntry {
            name: "magma_cube",
            texture: "entity/slime/magmacube",
            build: magma_cube_model,
        },
        EntityModelEntry {
            name: "blaze",
            texture: "entity/blaze/blaze",
            build: blaze_model,
        },
        EntityModelEntry {
            name: "squid",
            texture: "entity/squid/squid",
            build: squid_model,
        },
        EntityModelEntry {
            name: "bat",
            texture: "entity/bat/bat",
            build: bat_model,
        },
        EntityModelEntry {
            name: "enderman",
            texture: "entity/enderman/enderman",
            build: enderman_model,
        },
        // Tier 3: monster/* remainder (impl-assets lane).
        EntityModelEntry {
            name: "drowned",
            texture: "entity/zombie/drowned",
            build: drowned_model,
        },
        EntityModelEntry {
            name: "iron_golem",
            texture: "entity/iron_golem/iron_golem",
            build: iron_golem_model,
        },
        EntityModelEntry {
            name: "snow_golem",
            texture: "entity/snow_golem/snow_golem",
            build: snow_golem_model,
        },
        EntityModelEntry {
            name: "vex",
            texture: "entity/illager/vex",
            build: vex_model,
        },
        EntityModelEntry {
            name: "silverfish",
            texture: "entity/silverfish/silverfish",
            build: silverfish_model,
        },
        EntityModelEntry {
            name: "endermite",
            texture: "entity/endermite/endermite",
            build: endermite_model,
        },
        EntityModelEntry {
            name: "piglin",
            texture: "entity/piglin/piglin",
            build: piglin_model,
        },
        EntityModelEntry {
            name: "zombified_piglin",
            texture: "entity/piglin/zombified_piglin",
            build: piglin_model,
        },
        EntityModelEntry {
            name: "piglin_brute",
            texture: "entity/piglin/piglin_brute",
            build: piglin_model,
        },
        EntityModelEntry {
            name: "ghast",
            texture: "entity/ghast/ghast",
            build: ghast_model,
        },
        EntityModelEntry {
            name: "hoglin",
            texture: "entity/hoglin/hoglin",
            build: hoglin_model,
        },
        EntityModelEntry {
            name: "zoglin",
            texture: "entity/hoglin/zoglin",
            build: hoglin_model,
        },
        EntityModelEntry {
            name: "strider",
            texture: "entity/strider/strider",
            build: strider_model,
        },
        EntityModelEntry {
            name: "guardian",
            texture: "entity/guardian/guardian",
            build: guardian_model,
        },
        // --- animal/npc/object half (owned by this agent). Horse family,
        // cat/wolf/ocelot and parrot are deferred pending the texture-variant
        // seam (see the module banner above `end_crystal_model`). item_frame
        // is intentionally absent: vanilla resolves it via a block-model JSON
        // (`ItemFrameRenderer`/`BlockModelResolver`), not a `ModelPart`
        // `LayerDefinition`, so it does not fit `CubeDef`'s single-tex_offset
        // box-unwrap primitive without extending it or routing through the
        // block-model pipeline instead. ---
        EntityModelEntry {
            name: "armor_stand",
            texture: "entity/armorstand/armorstand",
            build: armor_stand_model,
        },
        EntityModelEntry {
            name: "boat",
            texture: "entity/boat/oak",
            build: boat_model,
        },
        EntityModelEntry {
            name: "chest_boat",
            texture: "entity/chest_boat/oak",
            build: chest_boat_model,
        },
        EntityModelEntry {
            name: "raft",
            texture: "entity/boat/bamboo",
            build: raft_model,
        },
        EntityModelEntry {
            name: "chest_raft",
            texture: "entity/chest_boat/bamboo",
            build: chest_raft_model,
        },
        EntityModelEntry {
            name: "minecart",
            texture: "entity/minecart/minecart",
            build: minecart_model,
        },
        EntityModelEntry {
            name: "end_crystal",
            texture: "entity/end_crystal/end_crystal",
            build: end_crystal_model,
        },
        EntityModelEntry {
            name: "rabbit",
            texture: "entity/rabbit/rabbit_brown",
            build: rabbit_model,
        },
        EntityModelEntry {
            name: "fox",
            texture: "entity/fox/fox",
            build: fox_model,
        },
        EntityModelEntry {
            name: "panda",
            texture: "entity/panda/panda",
            build: panda_model,
        },
        EntityModelEntry {
            name: "goat",
            texture: "entity/goat/goat",
            build: goat_model,
        },
        EntityModelEntry {
            name: "bee",
            texture: "entity/bee/bee",
            build: bee_model,
        },
        EntityModelEntry {
            name: "turtle",
            texture: "entity/turtle/turtle",
            build: turtle_model,
        },
        EntityModelEntry {
            name: "camel",
            texture: "entity/camel/camel",
            build: camel_model,
        },
        EntityModelEntry {
            name: "cod",
            texture: "entity/fish/cod",
            build: cod_model,
        },
        EntityModelEntry {
            name: "salmon",
            texture: "entity/fish/salmon",
            build: salmon_model,
        },
        EntityModelEntry {
            name: "pufferfish",
            texture: "entity/fish/pufferfish",
            build: pufferfish_model,
        },
        EntityModelEntry {
            name: "tropical_fish",
            // `TropicalFishRenderer` pairs `TropicalFishLargeModel` (this
            // entry's geometry, chosen to avoid `TropicalFishSmallModel`'s
            // negative texOffs) with `tropical_b.png`, not `tropical_a.png`
            // (that's the small model's texture) — confirmed directly against
            // `TropicalFishRenderer.getTextureLocation`.
            texture: "entity/fish/tropical_b",
            build: tropical_fish_model,
        },
        EntityModelEntry {
            name: "dolphin",
            texture: "entity/dolphin/dolphin",
            build: dolphin_model,
        },
        EntityModelEntry {
            name: "axolotl",
            texture: "entity/axolotl/axolotl_lucy",
            build: axolotl_model,
        },
        EntityModelEntry {
            name: "frog",
            texture: "entity/frog/frog_temperate",
            build: frog_model,
        },
        EntityModelEntry {
            name: "tadpole",
            texture: "entity/tadpole/tadpole",
            build: tadpole_model,
        },
        EntityModelEntry {
            name: "sniffer",
            texture: "entity/sniffer/sniffer",
            build: sniffer_model,
        },
        EntityModelEntry {
            name: "armadillo",
            texture: "entity/armadillo/armadillo",
            build: armadillo_model,
        },
    ]
}
