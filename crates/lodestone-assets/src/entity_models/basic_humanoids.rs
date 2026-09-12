use super::*;

/// Vanilla's own zombie model / standard humanoid body layer, sheet 64×64.
pub fn zombie_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: humanoid_root(0.0),
    }
}

/// Vanilla's own skeleton model: the humanoid mesh with thin (2×12×2) arms and legs, sheet
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

/// Vanilla's own creeper model, sheet 64×32. Head and body at `y=6`; four short legs.
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

/// Vanilla's own spider model, sheet 64×32. Head, two body segments, eight 16-long legs posed
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

