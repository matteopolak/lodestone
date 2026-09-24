use super::{CubeDef, EntityModelDef, FACE_NORTH, PartDef, PartPose, only_face};

/// The decorated pot's base sheet, 32×32 —
/// vanilla's own decorated-pot-renderer base-layer construction declares a
/// 32×32 mesh definition.
const DECORATED_POT_BASE_SHEET: (u32, u32) = (32, 32);

/// The decorated pot's side sheet, 16×16 —
/// vanilla's own decorated-pot-renderer sides-layer construction declares a
/// 16×16 mesh definition.
/// One quad per model, not a full box — see [`decorated_pot_side_part`].
const DECORATED_POT_SIDE_SHEET: (u32, u32) = (16, 16);

/// The decorated pot's base — vanilla's own decorated-pot-renderer base-layer construction: the
/// neck (two nested boxes, deflated then inflated) plus flat top/bottom
/// planes sharing one cube. All three parts draw with the single
/// `decorated_pot_base` sheet regardless of which sherds (if any) are
/// stored, which is why this is one model rather than four.
///
/// `top`/`bottom` share vanilla's own top-bottom-plane cube unchanged (`texOffs(-14, 13)`,
/// `addBox(0, 0, 0, 14, 0, 14)`, no face restriction — a real, if
/// degenerate, six-face box, exactly as the jar authors it) and differ only
/// in the pivot's `y`.
#[must_use]
pub fn decorated_pot_base_model() -> EntityModelDef {
    let top_bottom_cube = || CubeDef::new([0.0, 0.0, 0.0], [14.0, 0.0, 14.0], [-14.0, 13.0]);
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
            PartDef::new(PartPose::offset(1.0, 16.0, 1.0)).with_cube(top_bottom_cube()),
        )
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset(1.0, 0.0, 1.0)).with_cube(top_bottom_cube()),
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

/// The pot's front side — `offsetAndRotation(1, 16, 15, PI, 0, 0)`.
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

/// The pot's back side — `offsetAndRotation(15, 16, 1, 0, 0, PI)`.
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

/// The pot's left side — `offsetAndRotation(1, 16, 1, 0, -PI/2, PI)`.
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

/// The pot's right side — `offsetAndRotation(15, 16, 15, 0, PI/2, PI)`.
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
