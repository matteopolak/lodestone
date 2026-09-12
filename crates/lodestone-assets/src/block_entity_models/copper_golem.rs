use super::{CubeDef, EntityModelDef, PartDef, PartPose};

/// The copper golem statue's sheet is 64×64 (every one of vanilla's own
/// copper-golem-model's
/// four per-pose layer constructions — standing, running,
/// sitting, star — shares this canvas size).
const COPPER_GOLEM_SHEET: (u32, u32) = (64, 64);

/// The statue's head — identical geometry in all four poses
/// (vanilla's own copper-golem-model per-pose layer constructions (standing/
/// running/sitting/star), `"head"` child):
/// four boxes (a wide skull-ish base, a snout nub, an "antenna" stalk, and
/// its tip), all sharing one deform-inflate pattern.
fn copper_golem_head_cubes() -> Vec<CubeDef> {
    vec![
        CubeDef::new([-4.0, -5.0, -5.0], [8.0, 5.0, 10.0], [0.0, 0.0]).grown(0.015),
        CubeDef::new([-1.0, -2.0, -6.0], [2.0, 3.0, 2.0], [56.0, 0.0]),
        CubeDef::new([-1.0, -9.0, -1.0], [2.0, 4.0, 2.0], [37.0, 8.0]).grown(-0.015),
        CubeDef::new([-2.0, -13.0, -2.0], [4.0, 4.0, 4.0], [37.0, 0.0]).grown(-0.015),
    ]
}

/// vanilla's own copper-golem-model body-layer construction — the statue's `STANDING` pose
/// (`CopperGolemStatueBlock.Pose.STANDING`), and the plainest of the four:
/// every part is a bare `PartPose.offset` with no rotation, the ordinary
/// humanoid-ish rig every other block in this corpus that reuses a mob rig
/// (skull) already assumes.
#[must_use]
pub fn copper_golem_statue_standing_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -6.0, 0.0));
    let head = copper_golem_head_cubes()
        .into_iter()
        .fold(head, PartDef::with_cube);
    let body = PartDef::new(PartPose::offset(0.0, -5.0, 0.0))
        .with_cube(CubeDef::new(
            [-4.0, -6.0, -3.0],
            [8.0, 6.0, 6.0],
            [0.0, 15.0],
        ))
        .with_child("head", head)
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-4.0, -6.0, 0.0)).with_cube(CubeDef::new(
                [-3.0, -1.0, -2.0],
                [3.0, 10.0, 4.0],
                [36.0, 16.0],
            )),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(4.0, -6.0, 0.0)).with_cube(CubeDef::new(
                [0.0, -1.0, -2.0],
                [3.0, 10.0, 4.0],
                [50.0, 16.0],
            )),
        );
    let root = PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
        .with_child("body", body)
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(0.0, -5.0, 0.0)).with_cube(CubeDef::new(
                [-4.0, 0.0, -2.0],
                [4.0, 5.0, 4.0],
                [0.0, 27.0],
            )),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(0.0, -5.0, 0.0)).with_cube(CubeDef::new(
                [0.0, 0.0, -2.0],
                [4.0, 5.0, 4.0],
                [16.0, 27.0],
            )),
        );
    EntityModelDef {
        texture_width: COPPER_GOLEM_SHEET.0,
        texture_height: COPPER_GOLEM_SHEET.1,
        root,
    }
}

/// vanilla's own copper-golem-model running-pose body-layer construction — the `RUNNING` pose.
/// Every limb is a *nested* `PartDefinition`: a bare pivot part (no cube of
/// its own) holding one `_r1` child that carries the real box at a further
/// `offsetAndRotation` — a 3D-pose-tool export shape distinct from every
/// other rig in this corpus, transcribed exactly rather than collapsed into
/// one part, since collapsing would double-apply neither pivot but *would*
/// silently drop the two-stage translate.
#[must_use]
pub fn copper_golem_statue_running_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.7, -5.6, -1.8));
    let head = copper_golem_head_cubes()
        .into_iter()
        .fold(head, PartDef::with_cube);
    let body_r1 = PartDef::new(PartPose::offset_and_rotation(
        1.1, 0.1, 0.7, 0.1204, -0.0064, -0.0779,
    ))
    .with_cube(CubeDef::new(
        [-4.02, -6.116, -3.5],
        [8.0, 6.0, 6.0],
        [0.0, 15.0],
    ));
    let body = PartDef::new(PartPose::offset(-1.064, -5.0, 0.0))
        .with_child("body_r1", body_r1)
        .with_child("head", head)
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-4.0, -6.0, 0.0)).with_child(
                "right_arm_r1",
                PartDef::new(PartPose::offset_and_rotation(
                    0.7, -0.248, -1.62, 1.0036, 0.0, 0.0,
                ))
                .with_cube(CubeDef::new(
                    [-3.052, -1.11, -2.036],
                    [3.0, 10.0, 4.0],
                    [36.0, 16.0],
                )),
            ),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(4.0, -6.0, 0.0)).with_child(
                "left_arm_r1",
                PartDef::new(PartPose::offset_and_rotation(
                    0.732, 0.0, 0.0, -0.8715, -0.0535, -0.0449,
                ))
                .with_cube(CubeDef::new(
                    [0.032, -1.1, -2.0],
                    [3.0, 10.0, 4.0],
                    [50.0, 16.0],
                )),
            ),
        );
    let right_leg = PartDef::new(PartPose::offset(-3.064, -5.0, 0.0)).with_child(
        "right_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            1.048, 0.0, -0.9, -0.8727, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [-1.856, -0.1, -1.09],
            [4.0, 5.0, 4.0],
            [0.0, 27.0],
        )),
    );
    let left_leg = PartDef::new(PartPose::offset(0.936, -5.0, 0.0)).with_child(
        "left_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            1.0, 0.0, 0.0, 0.7854, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [-2.088, -0.1, -2.0],
            [4.0, 5.0, 4.0],
            [16.0, 27.0],
        )),
    );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("right_leg", right_leg)
        .with_child("left_leg", left_leg);
    EntityModelDef {
        texture_width: COPPER_GOLEM_SHEET.0,
        texture_height: COPPER_GOLEM_SHEET.1,
        root,
    }
}

/// vanilla's own copper-golem-model sitting-pose body-layer construction — the `SITTING` pose.
/// The body itself carries **two** cubes plus a nested `body_r1` (the seat
/// cushion, rotated a full `180°` about `Z`) — the only pose whose top-level
/// part is not a bare pivot.
#[must_use]
pub fn copper_golem_statue_sitting_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -6.0, -0.2));
    let head = copper_golem_head_cubes()
        .into_iter()
        .fold(head, PartDef::with_cube);
    let body_r1 = PartDef::new(PartPose::offset_and_rotation(
        0.0, -1.0, -4.325, 0.0, 0.0, -3.1416,
    ))
    .with_cube(CubeDef::new(
        [-4.0, -3.0, -2.2],
        [8.0, 6.0, 3.0],
        [3.0, 18.0],
    ));
    let right_arm = PartDef::new(PartPose::offset_and_rotation(
        -4.0, -5.6, -1.8, 0.4363, 0.0, 0.0,
    ))
    .with_child(
        "right_arm_r1",
        PartDef::new(PartPose::offset_and_rotation(
            0.0, 0.0893, 0.1198, -1.0472, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [-3.075, -0.9733, -1.9966],
            [3.0, 10.0, 4.0],
            [36.0, 16.0],
        )),
    );
    let left_arm = PartDef::new(PartPose::offset_and_rotation(
        4.0, -5.6, -1.7, 0.4363, 0.0, 0.0,
    ))
    .with_child(
        "left_arm_r1",
        PartDef::new(PartPose::offset_and_rotation(
            0.0, -0.0015, -0.0808, -1.0472, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [0.075, -1.0443, -1.8997],
            [3.0, 10.0, 4.0],
            [50.0, 16.0],
        )),
    );
    let body = PartDef::new(PartPose::offset(0.0, -3.0, 2.325))
        .with_cube(CubeDef::new(
            [-3.0, -4.0, -4.525],
            [6.0, 1.0, 6.0],
            [3.0, 19.0],
        ))
        .with_cube(CubeDef::new(
            [-4.0, -3.0, -3.525],
            [8.0, 6.0, 6.0],
            [0.0, 15.0],
        ))
        .with_child("body_r1", body_r1)
        .with_child("head", head)
        .with_child("right_arm", right_arm)
        .with_child("left_arm", left_arm);
    let right_leg = PartDef::new(PartPose::offset(-2.1, -2.1, -2.075)).with_child(
        "right_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            0.05, -1.9, 1.075, -1.5708, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [-2.0, 0.975, 0.0],
            [4.0, 5.0, 4.0],
            [0.0, 27.0],
        )),
    );
    let left_leg = PartDef::new(PartPose::offset(2.0, -2.0, -2.075)).with_child(
        "left_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            0.05, -2.0, 1.075, -1.5708, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [-2.0, 0.975, 0.0],
            [4.0, 5.0, 4.0],
            [16.0, 27.0],
        )),
    );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("right_leg", right_leg)
        .with_child("left_leg", left_leg);
    EntityModelDef {
        texture_width: COPPER_GOLEM_SHEET.0,
        texture_height: COPPER_GOLEM_SHEET.1,
        root,
    }
}

/// vanilla's own copper-golem-model star-pose body-layer construction — the `STAR` pose (arms and
/// legs both flung outward). The arms additionally carry an empty
/// `"rightItem"` pivot child (vanilla's own copper-golem model's held-item anchor for a
/// *live* golem) with no cube — not baked here, since a placed statue's
/// renderer submits only the model, never a held item.
#[must_use]
pub fn copper_golem_statue_star_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -6.0, 0.0));
    let head = copper_golem_head_cubes()
        .into_iter()
        .fold(head, PartDef::with_cube);
    let right_arm = PartDef::new(PartPose::offset(-4.0, -6.0, 0.0)).with_child(
        "right_arm_r1",
        PartDef::new(PartPose::offset_and_rotation(
            1.0, 1.0, 0.0, 0.0, 0.0, 1.9199,
        ))
        .with_cube(CubeDef::new(
            [-1.5, -5.0, -2.0],
            [3.0, 10.0, 4.0],
            [36.0, 16.0],
        )),
    );
    let left_arm = PartDef::new(PartPose::offset(4.0, -6.0, 0.0)).with_child(
        "left_arm_r1",
        PartDef::new(PartPose::offset_and_rotation(
            -1.0, 1.0, 0.0, 0.0, 0.0, -1.9199,
        ))
        .with_cube(CubeDef::new(
            [-1.5, -5.0, -2.0],
            [3.0, 10.0, 4.0],
            [50.0, 16.0],
        )),
    );
    let body = PartDef::new(PartPose::offset(0.0, -5.0, 0.0))
        .with_cube(CubeDef::new(
            [-4.0, -6.0, -3.0],
            [8.0, 6.0, 6.0],
            [0.0, 15.0],
        ))
        .with_child("head", head)
        .with_child("right_arm", right_arm)
        .with_child("left_arm", left_arm);
    let right_leg = PartDef::new(PartPose::offset(-3.0, -5.0, 0.0)).with_child(
        "right_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            0.35, 2.0, 0.01, 0.0, 0.0, 0.2618,
        ))
        .with_cube(CubeDef::new(
            [-2.0, -2.5, -2.0],
            [4.0, 5.0, 4.0],
            [0.0, 27.0],
        )),
    );
    let left_leg = PartDef::new(PartPose::offset(1.0, -5.0, 0.0)).with_child(
        "left_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            1.65, 2.0, 0.0, 0.0, 0.0, -0.2618,
        ))
        .with_cube(CubeDef::new(
            [-2.0, -2.5, -2.0],
            [4.0, 5.0, 4.0],
            [16.0, 27.0],
        )),
    );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("right_leg", right_leg)
        .with_child("left_leg", left_leg);
    EntityModelDef {
        texture_width: COPPER_GOLEM_SHEET.0,
        texture_height: COPPER_GOLEM_SHEET.1,
        root,
    }
}
