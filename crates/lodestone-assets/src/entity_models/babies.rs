//! Dedicated baby rigs for the 26.3 family, transcribed part for part from the
//! decompiled client's baby model definitions. Each keeps the adult rig's part names so the
//! name-keyed skeleton animates it unchanged. Generated data; see
//! `docs/entity-appearance-parity.md`.

use super::*;

/// The dedicated baby rig `zombie_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn zombie_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 17.5, 0.0))
            .with_cube(cube([-2.0, -2.5, -1.0], [4.0, 5.0, 2.0], [16.0, 16.0])))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 15.25, 0.0))
            .with_cube(cube([-3.0, -6.25, -3.0], [6.0, 6.0, 6.0], [3.0, 3.0]))
            .with_cube(cube([-3.0, -6.15, -3.0], [6.0, 6.0, 6.0], [35.0, 3.0]).grown(0.25))
            .with_child("hat", PartDef::new(PartPose::offset(0.0, 0.0, 0.0))))
        .with_child("right_arm", PartDef::new(PartPose::offset(-3.0, 15.5, 0.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 5.0, 2.0], [36.0, 16.0])))
        .with_child("left_arm", PartDef::new(PartPose::offset(3.0, 15.5, 0.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 5.0, 2.0], [28.0, 16.0])))
        .with_child("right_leg", PartDef::new(PartPose::offset(-1.0, 20.0, 0.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 4.0, 2.0], [8.0, 16.0])))
        .with_child("left_leg", PartDef::new(PartPose::offset(1.0, 20.0, 0.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 4.0, 2.0], [0.0, 16.0])));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `zombie_villager_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn zombie_villager_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 18.75, 0.0))
            .with_cube(cube([-2.0, -2.75, -1.5], [4.0, 5.0, 3.0], [0.0, 15.0]))
            .with_cube(cube([-2.0, -2.75, -1.5], [4.0, 6.0, 3.0], [16.0, 22.0]).grown(0.1)))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 16.0, 0.0))
            .with_cube(cube([-4.0, -8.0, -3.5], [8.0, 8.0, 7.0], [0.0, 0.0]))
            .with_child("hat", PartDef::new(PartPose::offset(0.0, -4.0, 0.0))
                .with_cube(cube([-4.0, -4.0, -3.5], [8.0, 8.0, 7.0], [0.0, 31.0]).grown(0.3)))
            .with_child("hat_rim", PartDef::new(PartPose::offset(0.0, -4.5, 0.0))
                .with_cube(cube([-7.0, -0.5, -6.0], [14.0, 1.0, 12.0], [0.0, 46.0])))
            .with_child("nose", PartDef::new(PartPose::offset(0.0, -1.0, -4.0))
                .with_cube(cube([-1.0, -1.0, -0.5], [2.0, 2.0, 1.0], [23.0, 0.0]))))
        .with_child("right_arm", PartDef::new(PartPose::offset(-3.0, 15.5, 0.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 5.0, 2.0], [24.0, 15.0])))
        .with_child("left_arm", PartDef::new(PartPose::offset(3.0, 15.5, 0.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 5.0, 2.0], [16.0, 15.0])))
        .with_child("right_leg", PartDef::new(PartPose::offset(-1.0, 21.5, 0.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 3.0, 2.0], [8.0, 23.0])))
        .with_child("left_leg", PartDef::new(PartPose::offset(1.0, 21.5, 0.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 3.0, 2.0], [0.0, 23.0])));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `piglin_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn piglin_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 18.0, -0.5))
            .with_cube(cube([-3.0, -3.0, -1.0], [6.0, 5.0, 3.0], [0.0, 13.0])))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 15.0, 0.0))
            .with_cube(cube([-1.5, -3.0, -4.5], [3.0, 3.0, 1.0], [21.0, 30.0]))
            .with_cube(cube([-4.5, -6.0, -3.5], [9.0, 6.0, 7.0], [0.0, 0.0]))
            .with_child("hat", PartDef::new(PartPose::offset(0.0, 0.0, 0.0)))
            .with_child("left_ear", PartDef::new(PartPose::offset(4.2, -4.0, 0.0))
                .with_child("left_ear_r1", PartDef::new(PartPose::offset_and_rotation(1.0, 1.75, 0.0, 0.0, 0.0, -0.6109))
                    .with_cube(cube([-0.5, -3.0, -2.0], [1.0, 6.0, 4.0], [0.0, 21.0]))))
            .with_child("right_ear", PartDef::new(PartPose::offset(-4.2, -4.0, 0.0))
                .with_child("right_ear_r1", PartDef::new(PartPose::offset_and_rotation(-1.0, 1.75, 0.0, 0.0, 0.0, 0.6109))
                    .with_cube(cube([-0.5, -3.0, -2.0], [1.0, 6.0, 4.0], [18.0, 13.0])))))
        .with_child("left_arm", PartDef::new(PartPose::offset(4.0, 15.0, 0.0))
            .with_cube(cube([-1.0, 0.0, -1.5], [2.0, 5.0, 3.0], [28.0, 13.0])))
        .with_child("right_arm", PartDef::new(PartPose::offset(-4.0, 15.0, 0.0))
            .with_cube(cube([-1.0, 0.0, -1.5], [2.0, 5.0, 3.0], [10.0, 30.0])))
        .with_child("right_leg", PartDef::new(PartPose::offset(-1.5, 20.0, 0.0))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 4.0, 3.0], [22.0, 23.0])))
        .with_child("left_leg", PartDef::new(PartPose::offset(1.5, 20.0, 0.0))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 4.0, 3.0], [10.0, 23.0])));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `villager_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn villager_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("arms", PartDef::new(PartPose::offset(0.0, 17.5, 0.0))
            .with_child("right_hand", PartDef::new(PartPose::offset_and_rotation(-3.0, 1.4025, -0.9599, -1.0472, 0.0, 0.0))
                .with_cube(cube([-1.0, -2.4925, -1.8401], [2.0, 4.0, 2.0], [36.0, 15.0]))
                .with_cube(cube([5.0, -2.4925, -1.8401], [2.0, 4.0, 2.0], [16.0, 15.0])))
            .with_child("middlearm_r1", PartDef::new(PartPose::offset_and_rotation(0.0, 0.9024, -1.8175, -1.0472, 0.0, 0.0))
                .with_cube(cube([-2.0, -0.9924, -0.9825], [4.0, 2.0, 2.0], [24.0, 17.0]))))
        .with_child("right_leg", PartDef::new(PartPose::offset(-1.0, 21.5, 0.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 3.0, 2.0], [8.0, 23.0])))
        .with_child("left_leg", PartDef::new(PartPose::offset(1.0, 21.5, 0.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 3.0, 2.0], [0.0, 23.0])))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 16.0, 0.0))
            .with_cube(cube([-4.0, -8.0, -3.5], [8.0, 8.0, 7.0], [0.0, 0.0]))
            .with_child("hat", PartDef::new(PartPose::offset(0.0, -4.0, 0.0))
                .with_cube(cube([-4.0, -4.0, -3.5], [8.0, 8.0, 7.0], [0.0, 30.0]).grown(0.3)))
            .with_child("hat_rim", PartDef::new(PartPose::offset(0.0, -4.5, 0.0))
                .with_cube(cube([-7.0, -0.5, -6.0], [14.0, 1.0, 12.0], [0.0, 45.0])))
            .with_child("nose", PartDef::new(PartPose::offset(0.0, -2.0, -4.0))
                .with_cube(cube([-1.0, 0.0, -0.5], [2.0, 2.0, 1.0], [23.0, 0.0]))))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 18.75, 0.0))
            .with_cube(cube([-2.0, -2.75, -1.5], [4.0, 5.0, 3.0], [0.0, 15.0])))
        .with_child("bb_main", PartDef::new(PartPose::offset(0.5, 24.0, 0.0))
            .with_cube(cube([-2.5, -8.0, -1.5], [4.0, 6.0, 3.0], [16.0, 21.0]).grown(0.2)));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `pig_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn pig_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 19.0, 0.5))
            .with_cube(cube([-3.5, -3.0, -4.5], [7.0, 6.0, 9.0], [0.0, 0.0])))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 19.0, -2.0))
            .with_cube(cube([-3.5, -5.0, -5.0], [7.0, 6.0, 6.0], [0.0, 15.0]).grown(0.025))
            .with_cube(cube([-1.5, -1.975, -6.0], [3.0, 2.0, 1.0], [6.0, 27.0]).grown(0.015)))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(2.5, 22.0, -3.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [0.0, 0.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-2.5, 22.0, -3.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [23.0, 0.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(2.5, 22.0, 4.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [0.0, 4.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-2.5, 22.0, 4.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [23.0, 4.0])));
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// The dedicated baby rig `cow_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn cow_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 13.569, -5.1667))
            .with_cube(cube([-3.0, -4.569, -4.8333], [6.0, 6.0, 5.0], [0.0, 18.0]))
            .with_cube(cube([3.0, -5.569, -3.8333], [1.0, 2.0, 1.0], [8.0, 29.0]))
            .with_cube(cube([-4.0, -5.569, -3.8333], [1.0, 2.0, 1.0], [4.0, 29.0]).mirrored())
            .with_cube(cube([-2.0, -1.569, -5.8333], [4.0, 3.0, 1.0], [12.0, 29.0])))
        .with_child("body", PartDef::new(PartPose::offset(3.0, 19.0, -5.0))
            .with_cube(cube([-7.0, -7.0, -1.0], [8.0, 6.0, 12.0], [0.0, 0.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-2.5, 18.0, -3.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 6.0, 3.0], [22.0, 18.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(2.5, 18.0, -3.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 6.0, 3.0], [34.0, 18.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-2.5, 18.0, 3.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 6.0, 3.0], [22.0, 27.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(2.5, 18.0, 3.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 6.0, 3.0], [34.0, 27.0])));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `chicken_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn chicken_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 20.25, -1.25))
            .with_cube(cube([-2.0, -2.25, -0.75], [4.0, 4.0, 4.0], [0.0, 0.0]))
            .with_cube(cube([-1.0, -0.25, -1.75], [2.0, 1.0, 1.0], [10.0, 8.0])))
        .with_child("left_leg", PartDef::new(PartPose::offset(1.0, 22.0, 0.5))
            .with_cube(cube([-0.5, 0.0, 0.0], [1.0, 2.0, 0.0], [2.0, 2.0]))
            .with_cube(cube([-0.5, 2.0, -1.0], [1.0, 0.0, 1.0], [0.0, 1.0])))
        .with_child("right_leg", PartDef::new(PartPose::offset(-1.0, 22.0, 0.5))
            .with_cube(cube([-0.5, 0.0, 0.0], [1.0, 2.0, 0.0], [0.0, 2.0]))
            .with_cube(cube([-0.5, 2.0, -1.0], [1.0, 0.0, 1.0], [0.0, 0.0])))
        .with_child("right_wing", PartDef::new(PartPose::offset(2.0, 20.0, 0.0))
            .with_cube(cube([0.0, 0.0, -1.0], [1.0, 0.0, 2.0], [6.0, 8.0])))
        .with_child("left_wing", PartDef::new(PartPose::offset(-2.0, 20.0, 0.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [1.0, 0.0, 2.0], [4.0, 8.0])));
    EntityModelDef {
        texture_width: 16,
        texture_height: 16,
        root,
    }
}

/// The dedicated baby rig `sheep_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn sheep_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 17.0, 0.5))
            .with_cube(cube([-3.0, -2.0, -4.5], [6.0, 4.0, 9.0], [0.0, 10.0])))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 15.5, -2.5))
            .with_cube(cube([-2.5, -4.5, -3.5], [5.0, 5.0, 5.0], [0.0, 0.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-2.0, 19.0, 3.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 5.0, 2.0], [0.0, 23.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(2.0, 19.0, 3.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 5.0, 2.0], [24.0, 12.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-2.0, 19.0, -2.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 5.0, 2.0], [8.0, 23.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(2.0, 19.0, -2.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 5.0, 2.0], [24.0, 5.0])));
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// The dedicated baby rig `wolf_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn wolf_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 18.25, -4.0))
            .with_cube(cube([-2.99, -3.25, -3.0], [6.0, 5.0, 5.0], [0.0, 12.0]).grown(0.025))
            .with_cube(cube([-1.5, -0.24, -5.0], [3.0, 2.0, 2.0], [17.0, 12.0]))
            .with_child("right_ear", PartDef::new(PartPose::offset(-2.0, -4.25, -0.5))
                .with_cube(cube([-1.0, -1.0, -0.5], [2.0, 2.0, 1.0], [0.0, 5.0])))
            .with_child("left_ear", PartDef::new(PartPose::offset(2.0, -4.25, -0.5))
                .with_cube(cube([-1.0, -1.0, -0.5], [2.0, 2.0, 1.0], [20.0, 5.0]))))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 19.0, 0.0))
            .with_cube(cube([-3.0, -2.0, -4.0], [6.0, 4.0, 8.0], [0.0, 0.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-1.5, 21.0, 3.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 2.0], [0.0, 22.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(1.5, 21.0, 3.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 2.0], [8.0, 22.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-1.5, 21.0, -3.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 2.0], [0.0, 0.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(1.5, 21.0, -3.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 2.0], [20.0, 0.0])))
        .with_child("tail", PartDef::new(PartPose::offset_and_rotation(0.0, 19.0, 3.0, -0.5236, 0.0, 0.0))
            .with_child("tail_r1", PartDef::new(PartPose::offset_and_rotation(0.0, -0.6, 0.2, -3.1, 0.0, 0.0))
                .with_cube(cube([-1.0, -5.7, -1.0], [2.0, 6.0, 2.0], [22.0, 16.0]))));
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// The dedicated baby rig `feline_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn feline_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 20.0, -3.125))
            .with_cube(cube([-2.5, -3.0, -2.875], [5.0, 4.0, 4.0], [0.0, 0.0]))
            .with_cube(cube([-2.0, -4.0, -0.875], [1.0, 1.0, 2.0], [18.0, 0.0]))
            .with_cube(cube([1.0, -4.0, -0.875], [1.0, 1.0, 2.0], [24.0, 0.0]))
            .with_cube(cube([-1.5, -1.0, -3.875], [3.0, 2.0, 1.0], [18.0, 3.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(1.0, 22.0, -1.5))
            .with_cube(cube([-0.5, 0.0, -1.0], [1.0, 2.0, 2.0], [18.0, 18.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-1.0, 22.0, -1.5))
            .with_cube(cube([-0.5, 0.0, -1.0], [1.0, 2.0, 2.0], [12.0, 18.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(1.0, 22.0, 2.5))
            .with_cube(cube([-0.5, 0.0, -1.0], [1.0, 2.0, 2.0], [18.0, 22.0])))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 20.5, 0.5))
            .with_cube(cube([-2.0, -1.5, -3.5], [4.0, 3.0, 7.0], [0.0, 8.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-1.0, 22.0, 2.5))
            .with_cube(cube([-0.5, 0.0, -1.0], [1.0, 2.0, 2.0], [12.0, 22.0])))
        .with_child("tail1", PartDef::new(PartPose::offset_and_rotation(0.0, 19.107, 3.9151, -0.567232, 0.0, 0.0))
            .with_cube(cube([-0.5, -0.107, 0.0849], [1.0, 1.0, 5.0], [0.0, 18.0])))
        .with_child("tail2", PartDef::new(PartPose::offset(0.0, 0.0, 0.0)));
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// The dedicated baby rig `horse_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn horse_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 12.5, 0.0))
            .with_cube(cube([-4.0, -3.5, -7.0], [8.0, 7.0, 14.0], [0.0, 13.0]))
            .with_child("tail", PartDef::new(PartPose::offset_and_rotation(0.0, -1.0, 7.0, -0.7418, 0.0, 0.0))
                .with_cube(cube([-1.5, -1.5, -1.0], [3.0, 3.0, 8.0], [24.0, 34.0]))))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(2.4, 16.0, 5.4))
            .with_cube(cube([-1.5, -1.0, -1.5], [3.0, 9.0, 3.0], [12.0, 46.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-2.4, 16.0, 5.4))
            .with_cube(cube([-1.5, -1.0, -1.5], [3.0, 9.0, 3.0], [0.0, 46.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(2.4, 16.0, -5.4))
            .with_cube(cube([-1.5, -1.0, -1.5], [3.0, 9.0, 3.0], [12.0, 34.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-2.4, 16.0, -5.4))
            .with_cube(cube([-1.5, -1.0, -1.5], [3.0, 9.0, 3.0], [0.0, 34.0])))
        .with_child("head_parts", PartDef::new(PartPose::offset_and_rotation(0.0, 10.0, -6.0, 0.6109, 0.0, 0.0))
            .with_cube(cube([-2.0, -6.0, -2.0], [4.0, 8.0, 4.0], [30.0, 0.0]))
            .with_child("head", PartDef::new(PartPose::offset(0.0, -6.0516, -0.2951))
                .with_cube(cube([-3.0, -3.9484, -6.705], [6.0, 4.0, 9.0], [0.0, 0.0]))
                .with_child("left_ear", PartDef::new(PartPose::offset_and_rotation(2.0, -4.2484, 1.9451, 0.0, 0.0, 0.2618))
                    .with_cube(cube([-1.0, -2.5, -0.8], [2.0, 3.0, 1.0], [0.0, 4.0])))
                .with_child("right_ear", PartDef::new(PartPose::offset_and_rotation(-2.0, -4.2484, 1.645, 0.0, 0.0, -0.2618))
                    .with_cube(cube([-1.0, -2.5, -0.5], [2.0, 3.0, 1.0], [0.0, 0.0])))));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `donkey_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn donkey_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(1.0, 14.0, 0.0))
            .with_cube(cube([-5.0, -3.0, -7.0], [8.0, 6.0, 14.0], [0.0, 13.0]))
            .with_child("tail", PartDef::new(PartPose::offset(0.0, -1.5, 6.5))
                .with_child("tail_r1", PartDef::new(PartPose::offset_and_rotation(0.0, 0.0, 0.0, -0.7418, 0.0, 0.0))
                    .with_cube(cube([-2.5, -1.0, -0.5], [3.0, 3.0, 8.0], [24.0, 33.0]))))
            .with_child("left_hind_leg", PartDef::new(PartPose::offset(2.25, 3.5, 5.25))
                .with_cube(cube([-2.5, -1.5, -1.5], [3.0, 8.0, 3.0], [12.0, 44.0])))
            .with_child("right_hind_leg", PartDef::new(PartPose::offset(-2.4, 3.5, 5.4))
                .with_cube(cube([-2.5, -1.5, -1.5], [3.0, 8.0, 3.0], [0.0, 44.0])))
            .with_child("left_front_leg", PartDef::new(PartPose::offset(2.4, 3.5, -5.3))
                .with_cube(cube([-2.5, -1.5, -1.5], [3.0, 8.0, 3.0], [12.0, 33.0])))
            .with_child("right_front_leg", PartDef::new(PartPose::offset(-2.4, 3.5, -5.4))
                .with_cube(cube([-2.5, -1.5, -1.5], [3.0, 8.0, 3.0], [0.0, 33.0])))
            .with_child("head_parts", PartDef::new(PartPose::offset(0.0, -3.0, -5.0))
                .with_child("neck_r1", PartDef::new(PartPose::offset_and_rotation(0.0, 0.0, 0.0, 0.3927, 0.0, 0.0))
                    .with_cube(cube([-3.0, -6.0, -3.0], [4.0, 8.0, 4.0], [30.0, 9.0])))
                .with_child("head", PartDef::new(PartPose::offset(0.0, -5.0, -3.0))
                    .with_child("head_r1", PartDef::new(PartPose::offset_and_rotation(0.0, -1.0, 1.0, 0.3927, 0.0, 0.0))
                        .with_cube(cube([-4.0, -3.6, -8.4], [6.0, 4.0, 9.0], [0.0, 0.0])))
                    .with_child("left_ear", PartDef::new(PartPose::offset_and_rotation(2.0, -3.5, -1.0, 0.48, 0.0, 0.48))
                        .with_cube(cube([-2.0, -6.5, -0.3], [2.0, 7.0, 1.0], [0.0, 0.0])))
                    .with_child("right_ear", PartDef::new(PartPose::offset_and_rotation(-2.0, -3.5, -1.0, 0.48, 0.0, -0.48))
                        .with_cube(cube([-2.0, -6.5, -0.3], [2.0, 7.0, 1.0], [22.0, 0.0]).mirrored()))))
            .with_child("right_chest", PartDef::new(PartPose::offset(-1.0, 10.0, 0.0)))
            .with_child("left_chest", PartDef::new(PartPose::offset(-1.0, 10.0, 0.0))));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `llama_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn llama_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 12.0, -4.0))
            .with_cube(cube([-3.0, -9.0, -4.0], [6.0, 11.0, 4.0], [0.0, 0.0]))
            .with_cube(cube([-1.5, -7.0, -7.0], [3.0, 3.0, 3.0], [0.0, 15.0]))
            .with_cube(cube([0.5, -11.0, -3.0], [2.0, 2.0, 2.0], [20.0, 4.0]))
            .with_cube(cube([-2.5, -11.0, -3.0], [2.0, 2.0, 2.0], [20.0, 0.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-2.5, 16.5, 4.5))
            .with_cube(cube([-1.4, -0.5, -1.5], [3.0, 8.0, 3.0], [0.0, 45.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(2.5, 16.5, 4.5))
            .with_cube(cube([-1.6, -0.5, -1.5], [3.0, 8.0, 3.0], [12.0, 45.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-2.5, 16.5, -3.5))
            .with_cube(cube([-1.4, -0.5, -1.5], [3.0, 8.0, 3.0], [0.0, 34.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(2.5, 16.5, -3.5))
            .with_cube(cube([-1.6, -0.5, -1.5], [3.0, 8.0, 3.0], [12.0, 34.0])))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 14.0, 2.5))
            .with_cube(cube([-4.0, -3.0, -8.5], [8.0, 6.0, 13.0], [0.0, 15.0])));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `rabbit_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn rabbit_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 23.0, 1.6))
            .with_child("body_r1", PartDef::new(PartPose::offset_and_rotation(0.0, -2.0, -1.6, -0.5236, 0.0, 0.0))
                .with_cube(cube([-2.0, -2.0, -3.0], [4.0, 3.0, 6.0], [0.0, 8.0])))
            .with_child("tail", PartDef::new(PartPose::offset(0.0, -2.2, 2.0))
                .with_child("tail_r1", PartDef::new(PartPose::offset_and_rotation(-0.1, 0.0, 0.0, -0.5236, 0.0, 0.0))
                    .with_cube(cube([-1.4, -2.0268, -1.0177], [3.0, 3.0, 3.0], [0.0, 21.0]))))
            .with_child("head", PartDef::new(PartPose::offset(0.0, -5.0, -2.6))
                .with_cube(cube([-2.5, -3.0, -3.0], [5.0, 4.0, 4.0], [0.0, 0.0]))
                .with_child("right_ear", PartDef::new(PartPose::offset(-1.5, -3.5, -0.5))
                    .with_cube(cube([-1.0, -3.5, -0.5], [2.0, 4.0, 1.0], [18.0, 0.0])))
                .with_child("left_ear", PartDef::new(PartPose::offset(1.5, -3.5, -0.5))
                    .with_cube(cube([-1.0, -3.5, -0.5], [2.0, 4.0, 1.0], [24.0, 0.0]))))
            .with_child("frontlegs", PartDef::new(PartPose::offset(0.0, -2.5, -2.6))
                .with_child("left_front_leg", PartDef::new(PartPose::offset_and_rotation(1.0, 1.0, -0.5, 0.3927, 0.0, 0.0))
                    .with_child("left_front_leg_r1", PartDef::new(PartPose::offset_and_rotation(0.0, 1.0, 0.0, -0.3927, 0.0, 0.0))
                        .with_cube(cube([-0.5, -1.5, -0.5], [1.0, 3.0, 1.0], [18.0, 8.0]))))
                .with_child("right_front_leg", PartDef::new(PartPose::offset_and_rotation(-1.0, 1.0, -0.5, 0.3927, 0.0, 0.0))
                    .with_child("right_front_leg_r1", PartDef::new(PartPose::offset_and_rotation(0.0, 1.0, 0.0, -0.3927, 0.0, 0.0))
                        .with_cube(cube([-0.5, -1.5, -0.5], [1.0, 3.0, 1.0], [14.0, 8.0]))))))
        .with_child("backlegs", PartDef::new(PartPose::offset(0.0, 23.0, 2.0))
            .with_child("left_hind_leg", PartDef::new(PartPose::offset_and_rotation(1.5, 0.5, 0.5, 0.0, 3.1416, 0.0))
                .with_child("left_haunch", PartDef::new(PartPose::offset_and_rotation(1.0, 0.0, 0.5, 0.0, -0.7854, 0.0))
                    .with_cube(cube([-2.0, -0.5, 0.0], [2.0, 1.0, 3.0], [10.0, 17.0]))))
            .with_child("right_hind_leg", PartDef::new(PartPose::offset_and_rotation(-1.5, 0.5, 0.5, 0.0, 3.1416, 0.0))
                .with_child("right_haunch", PartDef::new(PartPose::offset_and_rotation(0.5, 0.0, -0.9, 0.0, 0.7854, 0.0))
                    .with_cube(cube([-2.0, -0.5, 0.0], [2.0, 1.0, 3.0], [0.0, 17.0])))));
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// The dedicated baby rig `fox_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn fox_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 18.125, 0.125))
            .with_cube(cube([-3.0, -2.125, -5.125], [6.0, 5.0, 5.0], [0.0, 0.0]))
            .with_cube(cube([-1.0, 0.875, -7.125], [2.0, 2.0, 2.0], [18.0, 20.0]))
            .with_cube(cube([-3.0, -4.125, -4.125], [2.0, 2.0, 1.0], [22.0, 8.0]))
            .with_cube(cube([1.0, -4.125, -4.125], [2.0, 2.0, 1.0], [22.0, 11.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-1.5, 22.0, 4.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [22.0, 4.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(1.5, 22.0, 4.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [22.0, 0.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-1.5, 22.0, 0.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [22.0, 4.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(1.5, 22.0, 0.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [22.0, 0.0])))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 20.0, 2.0))
            .with_cube(cube([-2.5, -2.0, -3.0], [5.0, 4.0, 6.0], [0.0, 10.0]))
            .with_child("tail", PartDef::new(PartPose::offset(0.0, -0.5, 3.0))
                .with_cube(cube([-1.5, -1.48, -1.0], [3.0, 3.0, 6.0], [0.0, 20.0]))));
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// The dedicated baby rig `goat_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn goat_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(1.5, 19.5, 3.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 5.0, 2.0], [29.0, 12.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-1.5, 19.5, 3.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 5.0, 2.0], [21.0, 12.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-1.5, 19.5, -2.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 5.0, 2.0], [21.0, 5.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(1.5, 19.5, -2.0))
            .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 5.0, 2.0], [29.0, 5.0])))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 17.8, 0.0))
            .with_cube(cube([-3.0, -2.3, -4.5], [6.0, 5.0, 9.0], [0.0, 10.0]))
            .with_cube(cube([-2.5, -2.2, -4.0], [5.0, 4.0, 8.0], [0.0, 24.0])))
        .with_child("head", PartDef::new(PartPose::offset_and_rotation(0.0, 15.5, -3.0, 0.4363, 0.0, 0.0))
            .with_cube(cube([-2.0, -3.8126, -5.1548], [4.0, 4.0, 6.0], [0.0, 0.0]))
            .with_child("right_horn", PartDef::new(PartPose::offset_and_rotation(-1.5, -1.5, -1.0, -0.392699, 0.0, 0.0))
                .with_cube(cube([0.0, -4.5, 0.0], [1.0, 2.0, 1.0], [24.0, 0.0]).mirrored()))
            .with_child("left_horn", PartDef::new(PartPose::offset_and_rotation(-1.5, -1.5, -1.0, -0.392699, 0.0, 0.0))
                .with_cube(cube([2.0, -4.5, 0.0], [1.0, 2.0, 1.0], [24.0, 0.0]).mirrored()))
            .with_child("right_ear", PartDef::new(PartPose::offset_and_rotation(-1.7, -2.3126, 0.1452, 0.0, -0.5236, 0.0))
                .with_cube(cube([-2.0, -0.5, -0.5], [2.0, 1.0, 1.0], [0.0, 12.0]).mirrored()))
            .with_child("left_ear", PartDef::new(PartPose::offset_and_rotation(1.7, -2.3126, 0.1452, 0.0, 0.5236, 0.0))
                .with_cube(cube([0.0, -0.5, -0.5], [2.0, 1.0, 1.0], [0.0, 12.0])))
            .with_child("HeadMain", PartDef::new(PartPose::offset(0.0, -1.3126, -1.1548))
                .with_cube(cube([-2.0, -2.5, -4.0], [4.0, 4.0, 6.0], [0.0, 0.0]))));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `bee_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn bee_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("bone", PartDef::new(PartPose::offset(0.0, 19.6667, -1.8567))
            .with_cube(cube([1.0, -1.6667, -2.1633], [1.0, 2.0, 2.0], [6.0, 12.0]))
            .with_cube(cube([-2.0, -1.6667, -2.1933], [1.0, 2.0, 2.0], [0.0, 12.0]))
            .with_child("body", PartDef::new(PartPose::offset(0.0, 1.3333, 2.3567))
                .with_cube(cube([-2.0, -2.0, -2.5], [4.0, 4.0, 5.0], [0.0, 0.0]))
                .with_child("stinger", PartDef::new(PartPose::offset(0.0, 0.5, 2.5))
                    .with_cube(cube([0.0, -0.5, 0.0], [0.0, 1.0, 1.0], [13.0, 2.0]))))
            .with_child("right_wing", PartDef::new(PartPose::offset_and_rotation(-1.0, -0.6667, 0.8567, 0.2182, 0.3491, 0.0))
                .with_cube(cube([-3.0, 0.0, 0.0], [3.0, 0.0, 3.0], [3.0, 9.0])))
            .with_child("left_wing", PartDef::new(PartPose::offset_and_rotation(1.0, -0.6667, 0.8567, 0.2182, -0.3491, 0.0))
                .with_cube(cube([0.0, 0.0, 0.0], [3.0, 0.0, 3.0], [-3.0, 9.0]).mirrored()))
            .with_child("front_legs", PartDef::new(PartPose::offset(0.0, 3.3333, 1.8567))
                .with_cube(cube([-1.5, 0.0, 0.0], [3.0, 1.0, 0.0], [13.0, 0.0])))
            .with_child("middle_legs", PartDef::new(PartPose::offset(0.0, 3.3333, 2.8567))
                .with_cube(cube([-1.5, 0.0, 0.0], [3.0, 1.0, 0.0], [13.0, 1.0])))
            .with_child("back_legs", PartDef::new(PartPose::offset(0.0, 3.3333, 3.8567))
                .with_cube(cube([-1.5, 0.0, 0.0], [3.0, 1.0, 0.0], [13.0, 2.0]))));
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// The dedicated baby rig `polar_bear_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn polar_bear_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 17.5, 0.0))
            .with_cube(cube([-4.0, -3.5, -6.0], [8.0, 7.0, 12.0], [0.0, 9.0])))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 18.625, -5.75))
            .with_cube(cube([-3.0, -2.625, -4.25], [6.0, 5.0, 4.0], [0.0, 0.0]))
            .with_cube(cube([-2.0, 0.375, -6.25], [4.0, 2.0, 2.0], [20.0, 3.0]))
            .with_cube(cube([-4.0, -3.625, -2.75], [2.0, 2.0, 1.0], [20.0, 0.0]))
            .with_cube(cube([2.0, -3.625, -2.75], [2.0, 2.0, 1.0], [26.0, 0.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-2.5, 21.5, 4.5))
            .with_cube(cube([-1.5, -0.5, -1.5], [3.0, 3.0, 3.0], [0.0, 34.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(2.5, 21.5, 4.5))
            .with_cube(cube([-1.5, -0.5, -1.5], [3.0, 3.0, 3.0], [12.0, 34.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-2.5, 21.5, -4.5))
            .with_cube(cube([-1.5, -0.5, -1.5], [3.0, 3.0, 3.0], [0.0, 28.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(2.5, 21.5, -4.5))
            .with_cube(cube([-1.5, -0.5, -1.5], [3.0, 3.0, 3.0], [12.0, 28.0])));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `panda_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn panda_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 18.5, 2.5))
            .with_cube(cube([-4.5, -3.5, -5.5], [9.0, 7.0, 11.0], [0.0, 11.0])))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 19.0, -3.0))
            .with_cube(cube([-3.5, -3.0, -5.0], [7.0, 6.0, 5.0], [0.0, 0.0]))
            .with_cube(cube([-2.0, 1.0, -6.0], [4.0, 2.0, 1.0], [24.0, 6.0]))
            .with_cube(cube([-4.5, -4.0, -3.5], [3.0, 3.0, 1.0], [24.0, 0.0]))
            .with_cube(cube([1.5, -4.0, -3.5], [3.0, 3.0, 1.0], [33.0, 0.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-3.0, 22.0, 6.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 2.0, 3.0], [0.0, 34.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(3.0, 22.0, 6.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 2.0, 3.0], [12.0, 34.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-3.0, 22.0, -1.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 2.0, 3.0], [0.0, 29.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(3.0, 22.0, -1.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 2.0, 3.0], [12.0, 29.0])));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `turtle_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn turtle_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 22.9, 1.0))
            .with_cube(cube([-2.0, -1.0, -2.0], [4.0, 2.0, 4.0], [0.0, 0.0])))
        .with_child("head", PartDef::new(PartPose::offset(0.0, 22.9, -1.0))
            .with_cube(cube([-1.5, -2.0, -3.0], [3.0, 3.0, 3.0], [0.0, 6.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-2.0, 23.9, 2.5))
            .with_cube(cube([-2.0, 0.0, -0.5], [2.0, 0.0, 1.0], [-1.0, 0.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(2.0, 23.9, 2.5))
            .with_cube(cube([0.0, 0.0, -0.5], [2.0, 0.0, 1.0], [-1.0, 1.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-2.0, 23.9, -0.5))
            .with_cube(cube([-2.0, 0.0, -0.5], [2.0, 0.0, 1.0], [8.0, 6.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(2.0, 23.9, -0.5))
            .with_cube(cube([0.0, 0.0, -0.5], [2.0, 0.0, 1.0], [8.0, 7.0])));
    EntityModelDef {
        texture_width: 16,
        texture_height: 16,
        root,
    }
}

/// The dedicated baby rig `dolphin_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn dolphin_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 21.5, 0.0))
            .with_cube(cube([-3.0, -2.5, -4.0], [6.0, 5.0, 8.0], [20.0, 0.0]))
            .with_child("head", PartDef::new(PartPose::offset(0.0, 1.0, -4.0))
                .with_cube(cube([-3.0, -3.5, -4.0], [6.0, 5.0, 4.0], [0.0, 0.0]))
                .with_child("nose", PartDef::new(PartPose::offset(0.0, 0.5, -4.0))
                    .with_cube(cube([-1.0, -1.0, -2.0], [2.0, 2.0, 2.0], [0.0, 9.0]))))
            .with_child("left_fin", PartDef::new(PartPose::offset_and_rotation(1.8, 0.85, -2.6, 0.8727, 0.0, 1.7017))
                .with_cube(cube([-0.5, -1.5, -0.5], [1.0, 3.0, 6.0], [34.0, 18.0])))
            .with_child("right_fin", PartDef::new(PartPose::offset_and_rotation(-1.8, 0.85, -2.6, 0.8727, 0.0, -1.7017))
                .with_cube(cube([-0.5, -1.5, -0.5], [1.0, 3.0, 6.0], [48.0, 18.0]).mirrored()))
            .with_child("tail", PartDef::new(PartPose::offset(0.0, 1.0, 4.0))
                .with_cube(cube([-2.0, -1.5, 0.0], [4.0, 3.0, 7.0], [0.0, 13.0]))
                .with_child("tail_fin", PartDef::new(PartPose::offset(0.0, 0.0, 6.0))
                    .with_cube(cube([-4.0, -0.5, -1.0], [8.0, 1.0, 4.0], [22.0, 13.0]))))
            .with_child("back_fin", PartDef::new(PartPose::offset_and_rotation(0.0, -1.0, -2.7, 0.8727, 0.0, 0.0))
                .with_cube(cube([-0.5, -1.0, 1.0], [1.0, 3.0, 4.0], [42.0, 0.0]))));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `armadillo_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn armadillo_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 20.0, 0.5))
            .with_cube(cube([-2.5, -2.0, -3.5], [5.0, 4.0, 7.0], [0.0, 0.0]).grown(0.3))
            .with_cube(cube([-2.5, -2.0, -3.0], [5.0, 4.0, 6.0], [0.0, 11.0]))
            .with_child("tail", PartDef::new(PartPose::offset(0.0, 0.0, 3.4))
                .with_child("right_ear_cube", PartDef::new(PartPose::offset_and_rotation(0.0, 1.5, 1.0, -1.0472, 0.0, 0.0))
                    .with_cube(cube([-0.5, -0.5, -2.0], [1.0, 1.0, 4.0], [22.0, 11.0]))))
            .with_child("head", PartDef::new(PartPose::offset(0.0, 0.0, -3.2))
                .with_child("head_cube", PartDef::new(PartPose::offset_and_rotation(0.0, 0.0, 0.0, 0.741765, 0.0, 0.0))
                    .with_cube(cube([-1.0, -2.0, -4.0], [2.0, 2.0, 4.0], [20.0, 17.0]))
                    .with_child("right_ear", PartDef::new(PartPose::offset_and_rotation(-1.0, -2.0, -0.3, -0.4363, -0.1134, 0.0524))
                        .with_cube(cube([-1.8, -2.0, 0.0], [2.0, 3.0, 0.0], [28.0, 8.0]).mirrored()))
                    .with_child("left_ear", PartDef::new(PartPose::offset_and_rotation(1.0, -2.0, -0.3, -0.4363, 0.1134, -0.0524))
                        .with_cube(cube([-0.2, -2.0, 0.0], [2.0, 3.0, 0.0], [28.0, 8.0]))))))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-1.5, 22.0, 2.5))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [20.0, 27.0]).mirrored()))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(1.5, 22.0, 2.5))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [20.0, 27.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(1.5, 22.0, -1.5))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [20.0, 23.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(-1.5, 22.0, -1.5))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 2.0, 2.0], [24.0, 0.0]).mirrored()))
        .with_child("cube", PartDef::new(PartPose::offset(0.0, 20.7, 0.5))
            .with_cube(cube([-3.0, -3.0, -3.0], [6.0, 6.0, 6.0], [0.0, 25.0]).grown(0.3)));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `axolotl_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn axolotl_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("root", PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
            .with_child("body", PartDef::new(PartPose::offset(0.0, -1.25, 1.75))
                .with_cube(cube([-2.0, -0.75, -2.75], [4.0, 2.0, 6.0], [0.0, 0.0]))
                .with_cube(cube([0.0, -1.75, -2.75], [0.0, 3.0, 5.0], [0.0, 12.0]))
                .with_child("right_front_leg", PartDef::new(PartPose::offset(-2.0, 0.25, -1.25))
                    .with_cube(cube([-3.0, 0.0, -0.5], [3.0, 0.0, 1.0], [20.0, 16.0])))
                .with_child("right_hind_leg", PartDef::new(PartPose::offset_and_rotation(-2.0, 0.25, 1.75, 0.0, 1.5708, 1.5708))
                    .with_child("right_leg_r1", PartDef::new(PartPose::offset_and_rotation(0.0, 0.0, 0.0, -1.5708, 0.0, 1.5708))
                        .with_cube(cube([0.0, 0.0, -0.5], [3.0, 0.0, 1.0], [20.0, 14.0]))))
                .with_child("left_front_leg", PartDef::new(PartPose::offset(2.0, 0.25, -1.25))
                    .with_cube(cube([0.0, 0.0, -0.5], [3.0, 0.0, 1.0], [20.0, 13.0])))
                .with_child("left_hind_leg", PartDef::new(PartPose::offset(2.0, 0.25, 1.75))
                    .with_cube(cube([0.0, 0.0, -0.5], [3.0, 0.0, 1.0], [20.0, 14.0])))
                .with_child("tail", PartDef::new(PartPose::offset(0.0, -0.25, 3.25))
                    .with_cube(cube([0.0, -1.5, -1.0], [0.0, 3.0, 8.0], [10.0, 9.0])))
                .with_child("head", PartDef::new(PartPose::offset(0.0, 0.25, -2.75))
                    .with_cube(cube([-3.0, -2.0, -4.0], [6.0, 3.0, 4.0], [0.0, 8.0]))
                    .with_child("left_gills", PartDef::new(PartPose::offset(3.0, -0.5, -2.0))
                        .with_cube(cube([0.0, -3.5, 0.0], [3.0, 5.0, 0.0], [20.0, 8.0])))
                    .with_child("right_gills", PartDef::new(PartPose::offset(-3.0, -0.5, -2.0))
                        .with_cube(cube([-3.0, -3.5, 0.0], [3.0, 5.0, 0.0], [20.0, 3.0])))
                    .with_child("top_gills", PartDef::new(PartPose::offset(0.0, -2.0, -2.0))
                        .with_cube(cube([-3.0, -3.0, 0.0], [6.0, 3.0, 0.0], [20.0, 0.0]))))));
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// The dedicated baby rig `camel_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn camel_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 7.0, 0.0))
            .with_cube(cube([-4.5, -4.0, -8.0], [9.0, 8.0, 16.0], [0.0, 14.0]))
            .with_child("tail", PartDef::new(PartPose::offset(0.0, -1.5, 8.05))
                .with_cube(cube([-1.5, -0.5, 0.0], [3.0, 9.0, 0.0], [50.0, 38.0])))
            .with_child("head", PartDef::new(PartPose::offset(0.0, 1.0, -7.5))
                .with_cube(cube([-2.5, -3.0, -7.5], [5.0, 5.0, 7.0], [20.0, 0.0]))
                .with_cube(cube([-2.5, -12.0, -7.5], [5.0, 9.0, 5.0], [0.0, 0.0]))
                .with_cube(cube([-2.5, -12.0, -10.5], [5.0, 4.0, 3.0], [0.0, 14.0]))
                .with_child("right_ear", PartDef::new(PartPose::offset(-2.5, -11.0, -4.0))
                    .with_cube(cube([-3.0, -0.5, -1.0], [3.0, 1.0, 2.0], [37.0, 0.0])))
                .with_child("left_ear", PartDef::new(PartPose::offset(2.5, -11.0, -4.0))
                    .with_cube(cube([0.0, -0.5, -1.0], [3.0, 1.0, 2.0], [47.0, 0.0])))))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-3.0, 11.5, -5.5))
            .with_cube(cube([-1.5, -0.5, -1.5], [3.0, 13.0, 3.0], [36.0, 14.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(3.0, 11.5, -5.5))
            .with_cube(cube([-1.5, -0.5, -1.5], [3.0, 13.0, 3.0], [48.0, 14.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(3.0, 11.5, 5.5))
            .with_cube(cube([-1.5, -0.5, -1.5], [3.0, 13.0, 3.0], [12.0, 38.0])))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-3.0, 11.5, 5.5))
            .with_cube(cube([-1.5, -0.5, -1.5], [3.0, 13.0, 3.0], [0.0, 38.0])));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `strider_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn strider_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 16.75, 0.0))
            .with_cube(cube([-3.5, -3.75, -4.0], [7.0, 7.0, 8.0], [0.0, 0.0]))
            .with_child("bristle0", PartDef::new(PartPose::offset(0.0, -4.25, 2.0))
                .with_cube(cube([-3.5, -2.5, 0.0], [7.0, 3.0, 0.0], [0.0, 21.0])))
            .with_child("bristle1", PartDef::new(PartPose::offset(0.0, -4.25, 0.0))
                .with_cube(cube([-3.5, -2.5, 0.0], [7.0, 3.0, 0.0], [0.0, 18.0])))
            .with_child("bristle2", PartDef::new(PartPose::offset(0.0, -4.25, -2.0))
                .with_cube(cube([-3.5, -2.5, 0.0], [7.0, 3.0, 0.0], [0.0, 15.0]))))
        .with_child("right_leg", PartDef::new(PartPose::offset(-1.5, 20.0, 0.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 4.0, 2.0], [0.0, 24.0])))
        .with_child("left_leg", PartDef::new(PartPose::offset(1.5, 20.0, 0.0))
            .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 4.0, 2.0], [8.0, 24.0])));
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// The dedicated baby rig `hoglin_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn hoglin_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("head", PartDef::new(PartPose::offset_and_rotation(0.0, 13.0, -7.0, 0.8727, 0.0, 0.0))
            .with_cube(cube([-5.0, -2.2605, -10.547], [10.0, 4.0, 12.0], [0.0, 0.0]))
            .with_cube(cube([-7.0, -4.0981, -8.4879], [2.0, 5.0, 2.0], [44.0, 29.0]))
            .with_cube(cube([5.0, -4.0981, -8.4879], [2.0, 5.0, 2.0], [52.0, 29.0]))
            .with_child("right_ear", PartDef::new(PartPose::offset_and_rotation(-5.0, -1.0, -1.5, 0.0, 0.0, -0.8727))
                .with_cube(cube([-5.1, -0.5, -2.0], [6.0, 1.0, 4.0], [32.0, 5.0])))
            .with_child("left_ear", PartDef::new(PartPose::offset_and_rotation(5.0, -1.0, -1.5, 0.0, 0.0, 0.8727))
                .with_cube(cube([-0.9, -0.5, -2.0], [6.0, 1.0, 4.0], [32.0, 0.0]).mirrored())))
        .with_child("body", PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
            .with_cube(cube([-4.0, -14.0, -7.0], [8.0, 8.0, 14.0], [0.0, 16.0]).grown(0.02))
            .with_cube(cube([0.0, -18.0, -8.0], [0.0, 6.0, 11.0], [24.0, 39.0]).grown(0.02)))
        .with_child("right_hind_leg", PartDef::new(PartPose::offset(-2.5, 18.0, 4.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 6.0, 3.0], [0.0, 47.0])))
        .with_child("left_hind_leg", PartDef::new(PartPose::offset(2.5, 18.0, 4.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 6.0, 3.0], [12.0, 47.0])))
        .with_child("right_front_leg", PartDef::new(PartPose::offset(-2.5, 18.0, -4.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 6.0, 3.0], [0.0, 38.0])))
        .with_child("left_front_leg", PartDef::new(PartPose::offset(2.5, 18.0, -4.5))
            .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 6.0, 3.0], [12.0, 38.0])));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `nautilus_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn nautilus_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("root", PartDef::new(PartPose::offset(-0.5, 28.0, -0.5))
            .with_child("shell", PartDef::new(PartPose::offset(3.0, -8.0, -2.0))
                .with_cube(cube([-6.0, -4.0, -1.0], [7.0, 4.0, 7.0], [0.0, 0.0]))
                .with_cube(cube([-6.0, 0.0, -1.0], [7.0, 4.0, 9.0], [0.0, 11.0]))
                .with_cube(cube([-6.0, 0.0, 5.0], [7.0, 4.0, 0.0], [23.0, 11.0])))
            .with_child("body", PartDef::new(PartPose::offset(0.5, -5.0, 3.0))
                .with_cube(cube([-2.5, -3.01, -1.0], [5.0, 4.0, 7.0], [0.0, 24.0]))
                .with_cube(cube([-2.5, -3.01, 4.1], [5.0, 4.0, 0.0], [0.0, 35.0]))
                .with_child("upper_mouth", PartDef::new(PartPose::offset(0.0, -2.01, 3.9))
                    .with_cube(cube([-2.5, -1.0, 0.0], [5.0, 2.0, 2.0], [24.0, 24.0]).grown(-0.001)))
                .with_child("inner_mouth", PartDef::new(PartPose::offset(0.0, -1.01, 4.9))
                    .with_cube(cube([-1.5, -1.0, -1.0], [3.0, 2.0, 2.0], [24.0, 32.0])))
                .with_child("lower_mouth", PartDef::new(PartPose::offset(0.0, -0.01, 3.9))
                    .with_cube(cube([-2.5, -1.0, 0.0], [5.0, 2.0, 2.0], [24.0, 28.0]).grown(-0.001)))));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The dedicated baby rig `sniffer_baby_model`: parts keep the adult rig's names so the shared skeleton animates it.
pub fn sniffer_baby_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_child("bone", PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
            .with_child("body", PartDef::new(PartPose::offset(6.0, -3.0, -9.5))
                .with_cube(cube([-13.0, -14.0, -0.5], [14.0, 14.0, 20.0], [0.0, 35.0]).grown(0.25))
                .with_cube(cube([-13.0, -14.0, -0.5], [14.0, 15.0, 20.0], [0.0, 0.0]))
                .with_cube(cube([-13.0, 0.0, -0.5], [14.0, 0.0, 20.0], [68.0, 0.0]))
                .with_child("head", PartDef::new(PartPose::offset(-6.0, -4.75, 0.0))
                    .with_cube(cube([-5.0, -4.25, -7.5], [10.0, 9.0, 9.0], [68.0, 20.0]))
                    .with_cube(cube([-5.0, 3.75, -7.5], [10.0, 0.0, 9.0], [88.0, 20.0]))
                    .with_child("left_ear", PartDef::new(PartPose::offset(5.0, -4.25, -1.5))
                        .with_cube(cube([0.0, 0.0, -2.0], [1.0, 11.0, 3.0], [104.0, 38.0])))
                    .with_child("right_ear", PartDef::new(PartPose::offset(-5.0, -4.25, -1.5))
                        .with_cube(cube([-1.0, 0.0, -2.0], [1.0, 11.0, 3.0], [96.0, 38.0])))
                    .with_child("nose", PartDef::new(PartPose::offset(0.0, -1.25, -9.5))
                        .with_cube(cube([-5.0, -3.0, -2.0], [10.0, 3.0, 4.0], [68.0, 47.0])))
                    .with_child("lower_beak", PartDef::new(PartPose::offset(0.0, 1.25, -9.5))
                        .with_cube(cube([-5.0, -2.5, -2.0], [10.0, 5.0, 4.0], [68.0, 38.0])))))
            .with_child("right_front_leg", PartDef::new(PartPose::offset(-4.0, -4.0, -7.0))
                .with_cube(cube([-2.0, -1.0, -2.0], [4.0, 5.0, 4.0], [0.0, 69.0])))
            .with_child("right_mid_leg", PartDef::new(PartPose::offset(-4.0, -4.0, 0.0))
                .with_cube(cube([-2.0, -1.0, -2.0], [4.0, 5.0, 4.0], [0.0, 78.0])))
            .with_child("right_hind_leg", PartDef::new(PartPose::offset(-4.0, -4.0, 7.0))
                .with_cube(cube([-2.0, -1.0, -2.0], [4.0, 5.0, 4.0], [0.0, 87.0])))
            .with_child("left_front_leg", PartDef::new(PartPose::offset(4.0, -4.0, -7.0))
                .with_cube(cube([-2.0, -1.0, -2.0], [4.0, 5.0, 4.0], [16.0, 69.0])))
            .with_child("left_mid_leg", PartDef::new(PartPose::offset(4.0, -4.0, 0.0))
                .with_cube(cube([-2.0, -1.0, -2.0], [4.0, 5.0, 4.0], [16.0, 78.0])))
            .with_child("left_hind_leg", PartDef::new(PartPose::offset(4.0, -4.0, 7.0))
                .with_cube(cube([-2.0, -1.0, -2.0], [4.0, 5.0, 4.0], [16.0, 87.0]))));
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// The dedicated baby rig `squid_baby_model`: a cube body and eight short tentacles on a ring, sheet 32x32.
pub fn squid_baby_model() -> EntityModelDef {
    let mut root = PartDef::new(PartPose::ZERO).with_child(
        "body",
        PartDef::new(PartPose::offset(0.0, 13.0, 0.0))
            .with_cube(cube([-4.0, -5.0, -4.0], [8.0, 10.0, 8.0], [0.0, 0.0])),
    );
    for i in 0..8i32 {
        let a = f64::from(i) * std::f64::consts::TAU / 8.0;
        let x = a.cos() as f32 * 3.0;
        let z = a.sin() as f32 * 3.0;
        let y_rot = (f64::from(i) * -std::f64::consts::TAU / 8.0 + std::f64::consts::FRAC_PI_2) as f32;
        root.children.push((
            format!("tentacle{i}"),
            PartDef::new(PartPose::offset_and_rotation(x, 18.5, z, 0.0, y_rot, 0.0))
                .with_cube(cube([-1.0, -0.5, -1.0], [2.0, 6.0, 2.0], [0.0, 18.0])),
        ));
    }
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// Every baby rig as a corpus entry: `(name, default sheet, builder)`. A baby shares
/// its adult's part names; the default sheet is the adult's with the baby suffix.
pub fn baby_entries() -> Vec<EntityModelEntry> {
    let rows: [(&'static str, &'static str, fn() -> EntityModelDef); 40] = [
        ("zombie_baby", "entity/zombie/zombie_baby", zombie_baby_model),
        ("husk_baby", "entity/zombie/husk_baby", zombie_baby_model),
        ("drowned_baby", "entity/zombie/drowned_baby", zombie_baby_model),
        ("zombie_villager_baby", "entity/zombie_villager/zombie_villager_baby", zombie_villager_baby_model),
        ("piglin_baby", "entity/piglin/piglin_baby", piglin_baby_model),
        ("zombified_piglin_baby", "entity/piglin/zombified_piglin_baby", piglin_baby_model),
        ("villager_baby", "entity/villager/villager_baby", villager_baby_model),
        ("pig_baby", "entity/pig/pig_temperate_baby", pig_baby_model),
        ("cow_baby", "entity/cow/cow_temperate_baby", cow_baby_model),
        ("mooshroom_baby", "entity/cow/mooshroom_red_baby", cow_baby_model),
        ("chicken_baby", "entity/chicken/chicken_temperate_baby", chicken_baby_model),
        ("sheep_baby", "entity/sheep/sheep_baby", sheep_baby_model),
        ("wolf_baby", "entity/wolf/wolf_baby", wolf_baby_model),
        ("cat_baby", "entity/cat/cat_tabby_baby", feline_baby_model),
        ("ocelot_baby", "entity/cat/ocelot_baby", feline_baby_model),
        ("horse_baby", "entity/horse/horse_white_baby", horse_baby_model),
        ("donkey_baby", "entity/horse/donkey_baby", donkey_baby_model),
        ("mule_baby", "entity/horse/mule_baby", donkey_baby_model),
        ("skeleton_horse_baby", "entity/horse/horse_skeleton_baby", horse_baby_model),
        ("zombie_horse_baby", "entity/horse/horse_zombie_baby", horse_baby_model),
        ("llama_baby", "entity/llama/llama_creamy_baby", llama_baby_model),
        ("trader_llama_baby", "entity/llama/llama_creamy_baby", llama_baby_model),
        ("rabbit_baby", "entity/rabbit/rabbit_brown_baby", rabbit_baby_model),
        ("fox_baby", "entity/fox/fox_baby", fox_baby_model),
        ("goat_baby", "entity/goat/goat_baby", goat_baby_model),
        ("bee_baby", "entity/bee/bee_baby", bee_baby_model),
        ("polar_bear_baby", "entity/bear/polarbear_baby", polar_bear_baby_model),
        ("panda_baby", "entity/panda/panda_baby", panda_baby_model),
        ("turtle_baby", "entity/turtle/turtle_baby", turtle_baby_model),
        ("squid_baby", "entity/squid/squid_baby", squid_baby_model),
        ("glow_squid_baby", "entity/squid/glow_squid_baby", squid_baby_model),
        ("dolphin_baby", "entity/dolphin/dolphin_baby", dolphin_baby_model),
        ("armadillo_baby", "entity/armadillo/armadillo_baby", armadillo_baby_model),
        ("axolotl_baby", "entity/axolotl/axolotl_lucy_baby", axolotl_baby_model),
        ("camel_baby", "entity/camel/camel_baby", camel_baby_model),
        ("strider_baby", "entity/strider/strider_baby", strider_baby_model),
        ("hoglin_baby", "entity/hoglin/hoglin_baby", hoglin_baby_model),
        ("zoglin_baby", "entity/hoglin/zoglin_baby", hoglin_baby_model),
        ("nautilus_baby", "entity/nautilus/nautilus_baby", nautilus_baby_model),
        ("sniffer_baby", "entity/sniffer/snifflet", sniffer_baby_model),
    ];
    rows.into_iter()
        .map(|(name, sheet, build)| EntityModelEntry {
            name,
            texture: EntityTexture::Fixed(sheet),
            build,
        })
        .collect()
}
