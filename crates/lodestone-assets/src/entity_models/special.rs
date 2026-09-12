use super::*;

/// vanilla's own end crystal model's body-layer construction: `outer_glass` (8³) with a nested
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
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-6.0, 0.0, -6.0],
                [12.0, 4.0, 12.0],
                [0.0, 16.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// vanilla's own armor stand model's body-layer construction: starts from vanilla's own humanoid model's mesh construction
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
            PartDef::new(PartPose::offset(0.0, 1.0, 0.0)).with_cube(cube(
                [-1.0, -7.0, -1.0],
                [2.0, 7.0, 2.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-6.0, 0.0, -1.5],
                [12.0, 3.0, 3.0],
                [0.0, 26.0],
            )),
        )
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-5.0, 2.0, 0.0)).with_cube(cube(
                [-2.0, -2.0, -1.0],
                [2.0, 12.0, 2.0],
                [24.0, 0.0],
            )),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(5.0, 2.0, 0.0))
                .with_cube(cube([0.0, -2.0, -1.0], [2.0, 12.0, 2.0], [32.0, 16.0]).mirrored()),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-1.9, 12.0, 0.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 11.0, 2.0],
                [8.0, 0.0],
            )),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(1.9, 12.0, 0.0))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 11.0, 2.0], [40.0, 16.0]).mirrored()),
        )
        .with_child(
            "right_body_stick",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-3.0, 3.0, -1.0],
                [2.0, 7.0, 2.0],
                [16.0, 0.0],
            )),
        )
        .with_child(
            "left_body_stick",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [1.0, 3.0, -1.0],
                [2.0, 7.0, 2.0],
                [48.0, 16.0],
            )),
        )
        .with_child(
            "shoulder_stick",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-4.0, 10.0, -1.0],
                [8.0, 2.0, 2.0],
                [0.0, 48.0],
            )),
        )
        .with_child(
            "base_plate",
            PartDef::new(PartPose::offset(0.0, 12.0, 0.0)).with_cube(cube(
                [-6.0, 11.0, -6.0],
                [12.0, 1.0, 12.0],
                [0.0, 32.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

