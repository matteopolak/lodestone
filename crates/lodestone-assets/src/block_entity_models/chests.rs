use super::{
    CHEST_SHEET, CubeDef, EntityModelDef, FACE_EAST, FACE_WEST, PartDef, PartPose, hide_face,
};

/// A single chest — the real single-body chest cuboid rig.
///
/// ```text
/// bottom  texOffs(0, 19)  box(1, 0, 1,  14, 10, 14)  pose ZERO
/// lid     texOffs(0,  0)  box(1, 0, 0,  14,  5, 14)  pose offset(0, 9, 1)
/// lock    texOffs(0,  0)  box(7,-2, 14,  2,  4,  1)  pose offset(0, 9, 1)
/// ```
///
/// `lid` and `lock` share a pivot on purpose: the real chest's animation code
/// assigns the lock's own rotation from the lid's every frame, so the latch
/// swings with the lid rather than staying put. They are **siblings**, not
/// parent and child — a nested `lock` would compose the pivot twice.
#[must_use]
pub fn chest_single_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [1.0, 0.0, 1.0],
                [14.0, 10.0, 14.0],
                [0.0, 19.0],
            )),
        )
        .with_child(
            "lid",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef::new(
                [1.0, 0.0, 0.0],
                [14.0, 5.0, 14.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "lock",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef::new(
                [7.0, -2.0, 14.0],
                [2.0, 4.0, 1.0],
                [0.0, 0.0],
            )),
        );
    EntityModelDef {
        texture_width: CHEST_SHEET.0,
        texture_height: CHEST_SHEET.1,
        root,
    }
}

/// The left half of a double chest — the real double-chest left-body builder.
///
/// Boxes start at x `0` and are 15 wide (not 1/14 as in the single chest), and
/// every box omits its `West` face: that face is the seam against the right
/// half and is never visible. See the module doc for why the index is
/// [`FACE_WEST`] and not `Direction::West as usize`.
#[must_use]
pub fn chest_double_left_model() -> EntityModelDef {
    let hide = hide_face(FACE_WEST);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([0.0, 0.0, 1.0], [15.0, 10.0, 14.0], [0.0, 19.0])
            }),
        )
        .with_child(
            "lid",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([0.0, 0.0, 0.0], [15.0, 5.0, 14.0], [0.0, 0.0])
            }),
        )
        .with_child(
            "lock",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([0.0, -2.0, 14.0], [1.0, 4.0, 1.0], [0.0, 0.0])
            }),
        );
    EntityModelDef {
        texture_width: CHEST_SHEET.0,
        texture_height: CHEST_SHEET.1,
        root,
    }
}

/// The right half of a double chest — the real double-chest right-body builder.
///
/// Boxes start at x `1` and are 15 wide, and every box omits its `East` face
/// (the seam against the left half).
#[must_use]
pub fn chest_double_right_model() -> EntityModelDef {
    let hide = hide_face(FACE_EAST);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([1.0, 0.0, 1.0], [15.0, 10.0, 14.0], [0.0, 19.0])
            }),
        )
        .with_child(
            "lid",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([1.0, 0.0, 0.0], [15.0, 5.0, 14.0], [0.0, 0.0])
            }),
        )
        .with_child(
            "lock",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([15.0, -2.0, 14.0], [1.0, 4.0, 1.0], [0.0, 0.0])
            }),
        );
    EntityModelDef {
        texture_width: CHEST_SHEET.0,
        texture_height: CHEST_SHEET.1,
        root,
    }
}
