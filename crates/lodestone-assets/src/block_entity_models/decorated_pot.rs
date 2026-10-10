use super::{CubeDef, EntityModelDef, FACE_DOWN, FACE_NORTH, FACE_UP, PartDef, PartPose, only_face};

const DECORATED_POT_BASE_SHEET: (u32, u32) = (32, 32);

const DECORATED_POT_SIDE_SHEET: (u32, u32) = (16, 16);

/// One neck mesh and two outward-facing planes sharing the base sheet.
/// The GPU pass is double-sided, so emitting both sides of a zero-height
/// plane would put two faces at the same depth.
#[must_use]
pub fn decorated_pot_base_model() -> EntityModelDef {
    let plane = |face| CubeDef {
        visible_faces: only_face(face),
        ..CubeDef::new([0.0, 0.0, 0.0], [14.0, 0.0, 14.0], [-14.0, 13.0])
    };
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "neck",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                37.0,
                16.0,
                std::f32::consts::PI,
                0.0,
                0.0,
            ))
            .with_cube(CubeDef::new([4.0, 17.0, 4.0], [8.0, 3.0, 8.0], [0.0, 0.0]).grown(-0.1))
            .with_cube(CubeDef::new([5.0, 20.0, 5.0], [6.0, 1.0, 6.0], [0.0, 5.0]).grown(0.2)),
        )
        .with_child(
            "top",
            PartDef::new(PartPose::offset(1.0, 16.0, 1.0)).with_cube(plane(FACE_UP)),
        )
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset(1.0, 0.0, 1.0)).with_cube(plane(FACE_DOWN)),
        );
    EntityModelDef {
        texture_width: DECORATED_POT_BASE_SHEET.0,
        texture_height: DECORATED_POT_BASE_SHEET.1,
        root,
    }
}

/// One decorated-pot side quad — vanilla's own decorated-pot-renderer
/// sides-layer construction's
/// shared side-plane: `texOffs(1, 0).addBox(0, 0, 0, 14, 16, 0,
/// restricted to just the north face)`. Only the `North` face is emitted (the box
/// has zero depth, so the other five are degenerate or coincident) — see
/// [`only_face`].
///
/// **Why four models and not one reused four times.** Each of vanilla's four
/// named children (`front`/`back`/`left`/`right`) has its own fixed
/// `PartPose` — not a yaw step around the block's own facing, which is
/// already applied once by [`crate::block_entity_models`]'s placement
/// matrix — so there is no single rest pose the other three could be
/// *derived* from at render time. And because
/// [`BlockEntityInstance`](../../lodestone_render/block_entity/struct.BlockEntityInstance.html)
/// carries exactly one texture for every part it draws, putting all four
/// quads in one model (as the jar's own `sidesRoot` does, for its four
/// separate `submitModelPart` calls) would force all four sherds to share
/// one sheet — the very limitation this type exists to route around. Four
/// single-quad models, one per side, each resolved as its own
/// [`BlockEntityInstance`](../../lodestone_render/block_entity/struct.BlockEntityInstance.html)
/// with its own texture, is the decomposition `docs/block-entity-renderers.md`
/// names.
fn decorated_pot_side_part(pose: PartPose) -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "side",
        PartDef::new(pose).with_cube(CubeDef {
            visible_faces: only_face(FACE_NORTH),
            ..CubeDef::new([0.0, 0.0, 0.0], [14.0, 16.0, 0.0], [1.0, 0.0])
        }),
    );
    EntityModelDef {
        texture_width: DECORATED_POT_SIDE_SHEET.0,
        texture_height: DECORATED_POT_SIDE_SHEET.1,
        root,
    }
}

/// The pot's front side — offset and rotation.
#[must_use]
pub fn decorated_pot_side_front_model() -> EntityModelDef {
    decorated_pot_side_part(PartPose::offset_and_rotation(
        1.0,
        16.0,
        15.0,
        std::f32::consts::PI,
        0.0,
        0.0,
    ))
}

/// The pot's back side — offset and rotation.
#[must_use]
pub fn decorated_pot_side_back_model() -> EntityModelDef {
    decorated_pot_side_part(PartPose::offset_and_rotation(
        15.0,
        16.0,
        1.0,
        0.0,
        0.0,
        std::f32::consts::PI,
    ))
}

/// The pot's left side — offset and rotation.
#[must_use]
pub fn decorated_pot_side_left_model() -> EntityModelDef {
    decorated_pot_side_part(PartPose::offset_and_rotation(
        1.0,
        16.0,
        1.0,
        0.0,
        -std::f32::consts::FRAC_PI_2,
        std::f32::consts::PI,
    ))
}

/// The pot's right side — offset and rotation.
#[must_use]
pub fn decorated_pot_side_right_model() -> EntityModelDef {
    decorated_pot_side_part(PartPose::offset_and_rotation(
        15.0,
        16.0,
        15.0,
        0.0,
        std::f32::consts::FRAC_PI_2,
        std::f32::consts::PI,
    ))
}
