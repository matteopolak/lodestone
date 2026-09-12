use super::*;

/// vanilla's own adult rabbit model's body-layer construction: body(+tail), head(+ears), and two
/// pivot-only leg-group nodes (`frontlegs`/`backlegs`/`right_hind_leg`/
/// `left_hind_leg`) whose only cubes live on the leaf `*_leg`/`*_haunch`
/// parts. Sheet 64×64.
#[allow(
    clippy::approx_constant,
    reason = "the 0.3927/-0.3927 rotations are vanilla's own literals in its own adult-rabbit-model source, not the true value of PI/8 — transcribed verbatim"
)]
pub fn rabbit_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::offset_and_rotation(
        0.0, 23.0, 4.0, -0.3927, 0.0, 0.0,
    ))
    .with_cube(cube([-4.0, -6.0, -9.0], [8.0, 6.0, 10.0], [0.0, 0.0]))
    .with_child(
        "tail",
        PartDef::new(PartPose::offset(0.0, -4.9916, 0.0125)).with_cube(cube(
            [-2.0, -3.0084, -1.0125],
            [4.0, 4.0, 4.0],
            [20.0, 16.0],
        )),
    )
    .with_child(
        "head",
        PartDef::new(PartPose::offset_and_rotation(
            0.0, -5.2929, -8.1213, 0.3927, 0.0, 0.0,
        ))
        .with_cube(cube([-2.5, -3.0, -4.0], [5.0, 5.0, 5.0], [0.0, 16.0]))
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(1.5, -3.7071, -0.8787)).with_cube(cube(
                [-1.0, -4.2929, -0.1213],
                [2.0, 5.0, 1.0],
                [32.0, 0.0],
            )),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-1.5, -3.7071, -0.8787)).with_cube(cube(
                [-1.0, -4.2929, -0.1213],
                [2.0, 5.0, 1.0],
                [26.0, 0.0],
            )),
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
                .with_cube(cube(
                    [-0.9, -1.0, -0.9],
                    [2.0, 4.0, 2.0],
                    [36.0, 18.0],
                )),
            )
            .with_child(
                "left_front_leg",
                PartDef::new(PartPose::offset_and_rotation(
                    2.0, 1.9239, 0.4827, 0.3927, 0.0, 0.0,
                ))
                .with_cube(cube(
                    [-1.0, -1.0, -1.0],
                    [2.0, 4.0, 2.0],
                    [44.0, 18.0],
                )),
            ),
    );
    let backlegs = PartDef::new(PartPose::offset(0.0, 23.0, 4.0))
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.0, 0.5, 0.0)).with_child(
                "right_haunch",
                PartDef::new(PartPose::offset_and_rotation(
                    0.0, -0.5, 0.0, 0.0, 0.3927, 0.0,
                ))
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

/// vanilla's own adult fox model's body-layer construction: head(+ears+nose), body(+tail), and 4 legs.
/// `leftLeg`/`rightLeg` are each a single vanilla `CubeListBuilder` reused
/// across the hind and front leg *on the same side* (identical box, distinct
/// per-side texOffs); the sides are not related by `mirror()` at all. Sheet
/// 48×32.
pub fn fox_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(-1.0, 16.5, -3.0))
        .with_cube(cube([-3.0, -2.0, -5.0], [8.0, 6.0, 6.0], [1.0, 5.0]))
        .with_child(
            "right_ear",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-3.0, -4.0, -4.0],
                [2.0, 2.0, 1.0],
                [8.0, 1.0],
            )),
        )
        .with_child(
            "left_ear",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [3.0, -4.0, -4.0],
                [2.0, 2.0, 1.0],
                [15.0, 1.0],
            )),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-1.0, 2.01, -8.0],
                [4.0, 2.0, 3.0],
                [6.0, 18.0],
            )),
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

/// Vanilla's own panda-model body-layer construction (a quadruped model): head(+nose+2 ears), body,
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

/// Vanilla's own goat-model body-layer construction (a quadruped model): head builds 3 boxes on one
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
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-0.01, -16.0, -10.0],
                [2.0, 7.0, 2.0],
                [12.0, 55.0],
            )),
        )
        .with_child(
            "right_horn",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-2.99, -16.0, -10.0],
                [2.0, 7.0, 2.0],
                [12.0, 55.0],
            )),
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
            PartDef::new(PartPose::offset(1.0, 14.0, 4.0)).with_cube(cube(
                [0.0, 4.0, 0.0],
                [3.0, 6.0, 3.0],
                [36.0, 29.0],
            )),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.0, 14.0, 4.0)).with_cube(cube(
                [0.0, 4.0, 0.0],
                [3.0, 6.0, 3.0],
                [49.0, 29.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(1.0, 14.0, -6.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [3.0, 10.0, 3.0],
                [49.0, 2.0],
            )),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-3.0, 14.0, -6.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [3.0, 10.0, 3.0],
                [35.0, 2.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// vanilla's own adult bee model's body-layer construction: `bone` > `body`(stinger, left/right
/// antenna) plus `bone` > wings/legs. Sheet 64×64.
pub fn bee_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-3.5, -4.0, -5.0], [7.0, 7.0, 10.0], [0.0, 0.0]))
        .with_child(
            "stinger",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [0.0, -1.0, 5.0],
                [0.0, 1.0, 2.0],
                [26.0, 7.0],
            )),
        )
        .with_child(
            "left_antenna",
            PartDef::new(PartPose::offset(0.0, -2.0, -5.0)).with_cube(cube(
                [1.5, -2.0, -3.0],
                [1.0, 2.0, 3.0],
                [2.0, 0.0],
            )),
        )
        .with_child(
            "right_antenna",
            PartDef::new(PartPose::offset(0.0, -2.0, -5.0)).with_cube(cube(
                [-2.5, -2.0, -3.0],
                [1.0, 2.0, 3.0],
                [2.0, 3.0],
            )),
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
            PartDef::new(PartPose::offset(1.5, 3.0, -2.0)).with_cube(cube(
                [-5.0, 0.0, 0.0],
                [7.0, 2.0, 0.0],
                [26.0, 1.0],
            )),
        )
        .with_child(
            "middle_legs",
            PartDef::new(PartPose::offset(1.5, 3.0, 0.0)).with_cube(cube(
                [-5.0, 0.0, 0.0],
                [7.0, 2.0, 0.0],
                [26.0, 3.0],
            )),
        )
        .with_child(
            "back_legs",
            PartDef::new(PartPose::offset(1.5, 3.0, 2.0)).with_cube(cube(
                [-5.0, 0.0, 0.0],
                [7.0, 2.0, 0.0],
                [26.0, 5.0],
            )),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("bone", bone);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// vanilla's own adult turtle model's body-layer construction: head, body(shell+belly), egg_belly
/// (visibility-toggled at runtime by `hasEgg`, baked unconditionally here) and
/// 4 legs. Sheet 128×64.
pub fn turtle_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 19.0, -10.0)).with_cube(cube(
                [-3.0, -1.0, -3.0],
                [6.0, 5.0, 6.0],
                [3.0, 0.0],
            )),
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
            PartDef::new(PartPose::offset(-3.5, 22.0, 11.0)).with_cube(cube(
                [-2.0, 0.0, 0.0],
                [4.0, 1.0, 10.0],
                [1.0, 23.0],
            )),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.5, 22.0, 11.0)).with_cube(cube(
                [-2.0, 0.0, 0.0],
                [4.0, 1.0, 10.0],
                [1.0, 12.0],
            )),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-5.0, 21.0, -4.0)).with_cube(cube(
                [-13.0, 0.0, -2.0],
                [13.0, 1.0, 5.0],
                [27.0, 30.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(5.0, 21.0, -4.0)).with_cube(cube(
                [0.0, 0.0, -2.0],
                [13.0, 1.0, 5.0],
                [27.0, 24.0],
            )),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root,
    }
}

/// vanilla's own adult camel model's body-mesh construction: body(+hump+tail), head (3 stacked boxes:
/// muzzle/skull/snout + ears), 4 legs. Sheet 128×128.
pub fn camel_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -3.0, -19.5))
        .with_cube(cube([-3.5, -7.0, -15.0], [7.0, 8.0, 19.0], [60.0, 24.0]))
        .with_cube(cube([-3.5, -21.0, -15.0], [7.0, 14.0, 7.0], [21.0, 0.0]))
        .with_cube(cube([-2.5, -21.0, -21.0], [5.0, 5.0, 6.0], [50.0, 0.0]))
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(2.5, -21.0, -9.5)).with_cube(cube(
                [-0.5, 0.5, -1.0],
                [3.0, 1.0, 2.0],
                [45.0, 0.0],
            )),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-2.5, -21.0, -9.5)).with_cube(cube(
                [-2.5, 0.5, -1.0],
                [3.0, 1.0, 2.0],
                [67.0, 0.0],
            )),
        );
    let body = PartDef::new(PartPose::offset(0.0, 4.0, 9.5))
        .with_cube(cube([-7.5, -12.0, -23.5], [15.0, 12.0, 27.0], [0.0, 25.0]))
        .with_child(
            "hump",
            PartDef::new(PartPose::offset(0.0, -12.0, -10.0)).with_cube(cube(
                [-4.5, -5.0, -5.5],
                [9.0, 5.0, 11.0],
                [74.0, 0.0],
            )),
        )
        .with_child(
            "tail",
            PartDef::new(PartPose::offset(0.0, -9.0, 3.5)).with_cube(cube(
                [-1.5, 0.0, 0.0],
                [3.0, 14.0, 0.0],
                [122.0, 0.0],
            )),
        )
        .with_child("head", head);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(4.9, 1.0, 9.5)).with_cube(cube(
                [-2.5, 2.0, -2.5],
                [5.0, 21.0, 5.0],
                [58.0, 16.0],
            )),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-4.9, 1.0, 9.5)).with_cube(cube(
                [-2.5, 2.0, -2.5],
                [5.0, 21.0, 5.0],
                [94.0, 16.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(4.9, 1.0, -10.5)).with_cube(cube(
                [-2.5, 2.0, -2.5],
                [5.0, 21.0, 5.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-4.9, 1.0, -10.5)).with_cube(cube(
                [-2.5, 2.0, -2.5],
                [5.0, 21.0, 5.0],
                [0.0, 26.0],
            )),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// vanilla's own cod model's body-layer construction: body, head, nose, 2 side fins, tail_fin, and a
/// `top_fin` at `texOffs(20, -6)` on a `0×1×6` box. The **negative Y offset is
/// vanilla's own value**, not a transcription slip: `top_fin`'s real
/// (non-degenerate) faces are the ones spanning the depth×height texel rect
/// that this offset addresses, and there is no non-wrapping `texOffs` vanilla
/// could have chosen instead that keeps that rect on-sheet — see the same
/// wraparound note on `salmon`'s `right_fin`. `every_uv_is_within_the_sheet`
/// allows a small texel margin that covers exactly this. Sheet 32×32.
pub fn cod_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0)).with_cube(cube(
                [-1.0, -2.0, 0.0],
                [2.0, 4.0, 7.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0)).with_cube(cube(
                [-1.0, -2.0, -3.0],
                [2.0, 4.0, 3.0],
                [11.0, 0.0],
            )),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::offset(0.0, 22.0, -3.0)).with_cube(cube(
                [-1.0, -2.0, -1.0],
                [2.0, 3.0, 1.0],
                [0.0, 0.0],
            )),
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
            PartDef::new(PartPose::offset_and_rotation(
                1.0,
                23.0,
                0.0,
                0.0,
                0.0,
                PI / 4.0,
            ))
            .with_cube(cube([0.0, 0.0, -1.0], [2.0, 0.0, 2.0], [22.0, 4.0])),
        )
        .with_child(
            "tail_fin",
            PartDef::new(PartPose::offset(0.0, 22.0, 7.0)).with_cube(cube(
                [0.0, -2.0, 0.0],
                [0.0, 4.0, 4.0],
                [22.0, 3.0],
            )),
        )
        .with_child(
            "top_fin",
            PartDef::new(PartPose::offset(0.0, 20.0, 0.0)).with_cube(cube(
                [0.0, -1.0, -1.0],
                [0.0, 1.0, 6.0],
                [20.0, -6.0],
            )),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// vanilla's own salmon model's body-layer construction: body_front/body_back (each with a fin
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
            PartDef::new(PartPose::offset(0.0, -4.5, 5.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [0.0, 2.0, 3.0],
                [2.0, 1.0],
            )),
        );
    let body_back = PartDef::new(PartPose::offset(0.0, 20.0, 0.8000002))
        .with_cube(cube([-1.5, -2.5, 0.0], [3.0, 5.0, 8.0], [0.0, 13.0]))
        .with_child(
            "back_fin",
            PartDef::new(PartPose::offset(0.0, 0.0, 8.0)).with_cube(cube(
                [0.0, -2.5, 0.0],
                [0.0, 5.0, 6.0],
                [20.0, 10.0],
            )),
        )
        .with_child(
            "top_back_fin",
            PartDef::new(PartPose::offset(0.0, -4.5, -1.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [0.0, 2.0, 4.0],
                [0.0, 2.0],
            )),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body_front", body_front)
        .with_child("body_back", body_back)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 20.0, -7.2)).with_cube(cube(
                [-1.0, -2.0, -3.0],
                [2.0, 4.0, 3.0],
                [22.0, 0.0],
            )),
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

/// Vanilla's own big-pufferfish-model body-layer construction: body plus 3 mirrored fin pairs (blue,
/// front/back, top/bottom×3). Chosen as the registered pufferfish variant
/// specifically because — unlike vanilla's own small-pufferfish model (`back_fin` at
/// `texOffs(-3, 0)`) — none of its offsets are negative, so it needs no UV
/// exception. Sheet 32×32.
pub fn pufferfish_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0)).with_cube(cube(
                [-4.0, -8.0, -4.0],
                [8.0, 8.0, 8.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "right_blue_fin",
            PartDef::new(PartPose::offset(-4.0, 15.0, -2.0)).with_cube(cube(
                [-2.0, 0.0, -1.0],
                [2.0, 1.0, 2.0],
                [24.0, 0.0],
            )),
        )
        .with_child(
            "left_blue_fin",
            PartDef::new(PartPose::offset(4.0, 15.0, -2.0)).with_cube(cube(
                [0.0, 0.0, -1.0],
                [2.0, 1.0, 2.0],
                [24.0, 3.0],
            )),
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
            PartDef::new(PartPose::offset(0.0, 14.0, 0.0)).with_cube(cube(
                [-4.0, -1.0, 0.0],
                [8.0, 1.0, 1.0],
                [14.0, 16.0],
            )),
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
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0)).with_cube(cube(
                [-4.0, 0.0, 0.0],
                [8.0, 1.0, 0.0],
                [15.0, 20.0],
            )),
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

/// Vanilla's own large-tropical-fish-model body-layer construction, no deformation — the plain
/// (unpatterned) large tropical fish, chosen over vanilla's own small-tropical-fish model
/// (which has negative-Y `texOffs` on `tail`/`top_fin`) so this entry needs no
/// UV exception. Sheet 32×32.
pub fn tropical_fish_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 19.0, 0.0)).with_cube(cube(
                [-1.0, -3.0, -3.0],
                [2.0, 6.0, 6.0],
                [0.0, 20.0],
            )),
        )
        .with_child(
            "tail",
            PartDef::new(PartPose::offset(0.0, 19.0, 3.0)).with_cube(cube(
                [0.0, -3.0, 0.0],
                [0.0, 6.0, 5.0],
                [21.0, 16.0],
            )),
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
            PartDef::new(PartPose::offset(0.0, 16.0, -3.0)).with_cube(cube(
                [0.0, -4.0, 0.0],
                [0.0, 4.0, 6.0],
                [20.0, 11.0],
            )),
        )
        .with_child(
            "bottom_fin",
            PartDef::new(PartPose::offset(0.0, 22.0, -3.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [0.0, 4.0, 6.0],
                [20.0, 21.0],
            )),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// vanilla's own dolphin model's body-layer construction: body(back_fin, mirrored left_fin,
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
        PartDef::new(PartPose::offset(0.0, 0.0, 9.0)).with_cube(cube(
            [-5.0, -0.5, 0.0],
            [10.0, 1.0, 6.0],
            [19.0, 20.0],
        )),
    );
    let head = PartDef::new(PartPose::offset(0.0, -4.0, -3.0))
        .with_cube(cube([-4.0, -3.0, -3.0], [8.0, 7.0, 6.0], [0.0, 0.0]))
        .with_child(
            "nose",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-1.0, 2.0, -7.0],
                [2.0, 2.0, 4.0],
                [0.0, 13.0],
            )),
        );
    let body = PartDef::new(PartPose::offset(0.0, 22.0, -5.0))
        .with_cube(cube([-4.0, -7.0, 0.0], [8.0, 7.0, 13.0], [22.0, 0.0]))
        .with_child(
            "back_fin",
            PartDef::new(PartPose::rotation(PI / 3.0, 0.0, 0.0)).with_cube(cube(
                [-0.5, 0.0, 8.0],
                [1.0, 4.0, 5.0],
                [51.0, 0.0],
            )),
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

/// vanilla's own adult axolotl model's body-layer construction: body (main + a zero-width ridge box),
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
            PartDef::new(PartPose::offset(0.0, 0.0, 1.0)).with_cube(cube(
                [0.0, -3.0, 0.0],
                [0.0, 5.0, 12.0],
                [2.0, 19.0],
            )),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("body", body);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// vanilla's own frog model's body-layer construction: `root` (invisible pivot at y=24) > `body`(+
/// `head`[+`eyes`> left/right_eye], `croaking_body`, `tongue`, `left_arm`
/// [+`left_hand`], `right_arm`[+`right_hand`]), and `left_leg`/`right_leg`
/// (each +foot) as siblings of `body` under the same pivot node. `eyes` and
/// the `*_leg`/`*_arm` nodes are themselves pivot-only where noted. Sheet
/// 48×48 (**not** 64×64 — this is a small, easy-to-miss exception).
pub fn frog_model() -> EntityModelDef {
    let eyes = PartDef::new(PartPose::offset(-0.5, 0.0, 2.0))
        .with_child(
            "right_eye",
            PartDef::new(PartPose::offset(-1.5, -3.0, -6.5)).with_cube(cube(
                [-1.5, -1.0, -1.5],
                [3.0, 2.0, 3.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "left_eye",
            PartDef::new(PartPose::offset(2.5, -3.0, -6.5)).with_cube(cube(
                [-1.5, -1.0, -1.5],
                [3.0, 2.0, 3.0],
                [0.0, 5.0],
            )),
        );
    let head = PartDef::new(PartPose::offset(0.0, -2.0, -1.0))
        .with_cube(cube([-3.5, -1.0, -7.0], [7.0, 0.0, 9.0], [23.0, 13.0]))
        .with_cube(cube([-3.5, -2.0, -7.0], [7.0, 3.0, 9.0], [0.0, 13.0]))
        .with_child("eyes", eyes);
    let left_arm = PartDef::new(PartPose::offset(4.0, -1.0, -6.5))
        .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 3.0], [0.0, 32.0]))
        .with_child(
            "left_hand",
            PartDef::new(PartPose::offset(0.0, 3.0, -1.0)).with_cube(cube(
                [-4.0, 0.01, -4.0],
                [8.0, 0.0, 8.0],
                [18.0, 40.0],
            )),
        );
    let right_arm = PartDef::new(PartPose::offset(-4.0, -1.0, -6.5))
        .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 3.0], [0.0, 38.0]))
        .with_child(
            "right_hand",
            PartDef::new(PartPose::offset(0.0, 3.0, 0.0)).with_cube(cube(
                [-4.0, 0.01, -5.0],
                [8.0, 0.0, 8.0],
                [2.0, 40.0],
            )),
        );
    let body = PartDef::new(PartPose::offset(0.0, -2.0, 4.0))
        .with_cube(cube([-3.5, -2.0, -8.0], [7.0, 3.0, 9.0], [3.0, 1.0]))
        .with_cube(cube([-3.5, -1.0, -8.0], [7.0, 0.0, 9.0], [23.0, 22.0]))
        .with_child("head", head)
        .with_child(
            "croaking_body",
            PartDef::new(PartPose::offset(0.0, -1.0, -5.0))
                .with_cube(cube([-3.5, -0.1, -2.9], [7.0, 2.0, 3.0], [26.0, 5.0]).grown(-0.1)),
        )
        .with_child(
            "tongue",
            PartDef::new(PartPose::offset(0.0, -1.01, 1.0)).with_cube(cube(
                [-2.0, 0.0, -7.1],
                [4.0, 0.0, 7.0],
                [17.0, 13.0],
            )),
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
                    PartDef::new(PartPose::offset(2.0, 3.0, 0.0)).with_cube(cube(
                        [-4.0, 0.01, -4.0],
                        [8.0, 0.0, 8.0],
                        [2.0, 32.0],
                    )),
                ),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-3.5, -3.0, 4.0))
                .with_cube(cube([-2.0, 0.0, -2.0], [3.0, 3.0, 4.0], [0.0, 25.0]))
                .with_child(
                    "right_foot",
                    PartDef::new(PartPose::offset(-2.0, 3.0, 0.0)).with_cube(cube(
                        [-4.0, 0.01, -4.0],
                        [8.0, 0.0, 8.0],
                        [18.0, 32.0],
                    )),
                ),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("root", model_root);
    EntityModelDef {
        texture_width: 48,
        texture_height: 48,
        root,
    }
}

/// vanilla's own tadpole model's body-layer construction: 2 flat boxes, no hierarchy. Sheet is
/// **16×16** — the smallest sheet in the whole corpus, easy to fat-finger as
/// 32×32.
pub fn tadpole_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 22.0, -3.0)).with_cube(cube(
                [-1.5, -1.0, 0.0],
                [3.0, 2.0, 3.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "tail",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0)).with_cube(cube(
                [0.0, -1.0, 0.0],
                [0.0, 2.0, 7.0],
                [0.0, 0.0],
            )),
        );
    EntityModelDef {
        texture_width: 16,
        texture_height: 16,
        root,
    }
}

/// vanilla's own sniffer model's body-layer construction: `bone` > `body`(3 boxes) + 6 legs, `body` >
/// `head`(2 boxes) > ears/nose/lower_beak. Sheet is **192×192** — by far the
/// largest sheet in the corpus.
pub fn sniffer_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, 6.5, -19.48))
        .with_cube(cube([-6.5, -7.5, -11.5], [13.0, 18.0, 11.0], [8.0, 15.0]))
        .with_cube(cube([-6.5, 7.5, -11.5], [13.0, 0.0, 11.0], [8.0, 4.0]))
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(6.51, -7.5, -4.51)).with_cube(cube(
                [0.0, 0.0, -3.0],
                [1.0, 19.0, 7.0],
                [2.0, 0.0],
            )),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-6.51, -7.5, -4.51)).with_cube(cube(
                [-1.0, 0.0, -3.0],
                [1.0, 19.0, 7.0],
                [48.0, 0.0],
            )),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::offset(0.0, -4.5, -11.5)).with_cube(cube(
                [-6.5, -2.0, -9.0],
                [13.0, 2.0, 9.0],
                [10.0, 45.0],
            )),
        )
        .with_child(
            "lower_beak",
            PartDef::new(PartPose::offset(0.0, 2.5, -12.5)).with_cube(cube(
                [-6.5, -7.0, -8.0],
                [13.0, 12.0, 9.0],
                [10.0, 57.0],
            )),
        );
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube(
            [-12.5, -14.0, -20.0],
            [25.0, 29.0, 40.0],
            [62.0, 68.0],
        ))
        .with_cube(cube([-12.5, -14.0, -20.0], [25.0, 24.0, 40.0], [62.0, 0.0]).grown(0.5))
        .with_cube(cube([-12.5, 12.0, -20.0], [25.0, 0.0, 40.0], [87.0, 68.0]))
        .with_child("head", head);
    let bone = PartDef::new(PartPose::offset(0.0, 5.0, 0.0))
        .with_child("body", body)
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-7.5, 10.0, -15.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [32.0, 87.0],
            )),
        )
        .with_child(
            "right_mid_leg",
            PartDef::new(PartPose::offset(-7.5, 10.0, 0.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [32.0, 105.0],
            )),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-7.5, 10.0, 15.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [32.0, 123.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(7.5, 10.0, -15.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [0.0, 87.0],
            )),
        )
        .with_child(
            "left_mid_leg",
            PartDef::new(PartPose::offset(7.5, 10.0, 0.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [0.0, 105.0],
            )),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(7.5, 10.0, 15.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [0.0, 123.0],
            )),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("bone", bone);
    EntityModelDef {
        texture_width: 192,
        texture_height: 192,
        root,
    }
}

/// vanilla's own adult armadillo model's body-layer construction: body(+tail+head[+ears]), 4 legs, and
/// a separate root-level `cube` (the rolled-up ball form; vanilla toggles its
/// visibility with the roll animation state, baked unconditionally here since
/// this registry has no per-part runtime visibility). Sheet 64×64.
#[allow(
    clippy::approx_constant,
    reason = "-0.3927 is vanilla's own literal in its own adult-armadillo-model source, not the true value of PI/8 — transcribed verbatim"
)]
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
            PartDef::new(PartPose::offset(-2.0, 21.0, 4.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 3.0, 2.0],
                [51.0, 31.0],
            )),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(2.0, 21.0, 4.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 3.0, 2.0],
                [42.0, 31.0],
            )),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-2.0, 21.0, -4.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 3.0, 2.0],
                [51.0, 43.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(2.0, 21.0, -4.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 3.0, 2.0],
                [42.0, 43.0],
            )),
        )
        .with_child(
            "cube",
            PartDef::new(PartPose::offset(0.0, 24.0, 0.0)).with_cube(cube(
                [-5.0, -10.0, -6.0],
                [10.0, 10.0, 10.0],
                [0.0, 0.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

// ============================================================================
// The variant-heavy remainder of this agent's roster: horse family (horse,
// donkey, mule, skeleton_horse, zombie_horse, llama, trader_llama), cat, wolf,
// ocelot, parrot. Deferred until the `EntityTexture::ByVariant`/`EntityVariant`
// seam (in `entity.rs`) was settled; it has been, so these are ported here.
//
// Coordination note: `impl-assets` (who proposed the seam) was unreachable
// when this landed (agent no longer present in this session's roster), so the
// `HorseColor`/`LlamaColor`/`CatCoat`/`WolfCoat`/`WolfState`/`ParrotColor` enum
// shapes below were decided unilaterally from the decompiled source rather
// than confirmed with them first, as the task asked. Flagging this explicitly
// for review rather than presenting it as pre-agreed. One open design point:
// horse markings (vanilla's own markings enum) genuinely need a *second*, independently
// selected texture layer composited over the base colour (vanilla's own
// horse-marking layer
// submits a second translucent pass using the same model) — `ByVariant`
// resolves exactly one path per call, so it cannot express this. Rather than
// invent a new `EntityTexture` case unilaterally (a real seam decision that
// affects the shell/render consumer), `horse_markings_texture` below is a
// plain standalone function + `HorseMarkings` enum, deliberately *not* wired
// into `EntityTexture`/`EntityVariant`, ready for whoever implements the
// second render pass to call directly.
// ============================================================================
