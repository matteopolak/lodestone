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
    ]
}
