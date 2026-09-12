use super::*;

/// The hull + paddles shared by vanilla's own boat-model add-common-parts step (also reused,
/// structurally, by the chest-boat variant which just appends chest boxes).
/// Sheet 128×64 for the plain boat.
pub(super) fn boat_hull() -> PartDef {
    PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                3.0,
                1.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
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
            PartDef::new(PartPose::offset_and_rotation(
                15.0,
                4.0,
                0.0,
                0.0,
                PI / 2.0,
                0.0,
            ))
            .with_cube(cube([-8.0, -7.0, -1.0], [16.0, 6.0, 2.0], [0.0, 27.0])),
        )
        .with_child(
            "right",
            PartDef::new(PartPose::offset_and_rotation(0.0, 4.0, -9.0, 0.0, PI, 0.0))
                .with_cube(cube([-14.0, -7.0, -1.0], [28.0, 6.0, 2.0], [0.0, 35.0])),
        )
        .with_child(
            "left",
            PartDef::new(PartPose::offset(0.0, 4.0, 9.0)).with_cube(cube(
                [-14.0, -7.0, -1.0],
                [28.0, 6.0, 2.0],
                [0.0, 43.0],
            )),
        )
        .with_child(
            "left_paddle",
            PartDef::new(PartPose::offset_and_rotation(
                3.0,
                -5.0,
                9.0,
                0.0,
                0.0,
                PI / 16.0,
            ))
            .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [62.0, 0.0]))
            .with_cube(cube([-1.001, -3.0, 8.0], [1.0, 6.0, 7.0], [62.0, 0.0])),
        )
        .with_child(
            "right_paddle",
            PartDef::new(PartPose::offset_and_rotation(
                3.0,
                -5.0,
                -9.0,
                0.0,
                PI,
                PI / 16.0,
            ))
            .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [62.0, 20.0]))
            .with_cube(cube([0.001, -3.0, 8.0], [1.0, 6.0, 7.0], [62.0, 20.0])),
        )
}

/// Vanilla's own boat-model boat-model construction: hull + both paddles, no chest. Sheet 128×64.
/// (vanilla's own create-water-patch step's own invisible clip quad is [`boat_water_patch_model`],
/// a separate corpus entry rather than a child of this part tree — see its
/// own doc for why.)
pub fn boat_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root: boat_hull(),
    }
}

/// vanilla's own boat model's water-patch construction: an invisible depth-only mask shaped like a
/// **mirror** of the hull's own `bottom` plank — same box, offset the other
/// way (`y = -3.0` here against `bottom`'s `y = 3.0`) — that fills the boat's
/// hollow interior. Owner report: "placing down a boat still shows water
/// through the bottom". The hull is five thin planks around an open top, so
/// looking down (or across, at a grazing angle) into an occupied or empty
/// boat has a real gap between them; without this water-patch mask, the
/// translucent water surface underneath draws straight through it. Vanilla closes the
/// gap not with a visible floor but with **depth only**: this model is
/// submitted through `EntityPipeline::water_mask_pipeline` (colour writes
/// disabled, depth writes on, texture bound but never sampled into the
/// framebuffer) rather than the normal textured entity pipeline, so it
/// occludes the water pass's depth test while remaining itself invisible —
/// whatever would have been visible with no boat there at all (sky, terrain,
/// nothing) still shows through the hollow, exactly as vanilla's is.
///
/// A **separate corpus entry**, not a child part of [`boat_model`]/
/// [`chest_boat_model`]'s own tree: every part of one `PartDef` draws through
/// the *same* pipeline in one batch (`prepare_entities`), and this needs a
/// different one. Vanilla's own boat-renderer constructor bakes one shared
/// boat-water-patch model layer regardless of chest-or-not
/// (in the decompiled 26.2 client source), so one entry serves
/// both here too.
///
/// **Rafts get none of this.** Vanilla's own raft-renderer does not override
/// the base boat-renderer's submit-type-additions step, whose default body is empty —
/// so `"raft"`/`"chest_raft"` never resolve to this model, matching vanilla's
/// real (if inconsistent) behaviour: a raft's water is not masked either.
pub fn boat_water_patch_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO).with_child(
            "water_patch",
            PartDef::new(PartPose::offset_and_rotation(0.0, -3.0, 1.0, PI / 2.0, 0.0, 0.0))
                .with_cube(cube([-14.0, -9.0, -3.0], [28.0, 16.0, 3.0], [0.0, 0.0])),
        ),
    }
}

/// Vanilla's own boat-model chest-boat-model construction: the same hull plus `chest_bottom`/
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

/// The hull + paddles shared by vanilla's own raft-model add-common-parts step. Same part names as
/// vanilla's own boat model but a different hull shape (2 boxes, bamboo raft's flatter
/// profile) and paddle texOffs. The `1.5708F` bottom x-rotation is transcribed
/// verbatim (vanilla writes the literal, not the true value of PI / 2).
#[allow(
    clippy::approx_constant,
    reason = "1.5708 is vanilla's own literal in its own raft-model source, not the true value of PI/2 — transcribed verbatim, not the nearby true constant"
)]
fn raft_hull() -> PartDef {
    PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset_and_rotation(
                0.0, -2.1, 1.0, 1.5708, 0.0, 0.0,
            ))
            .with_cube(cube([-14.0, -11.0, -4.0], [28.0, 20.0, 4.0], [0.0, 0.0]))
            .with_cube(cube([-14.0, -9.0, -8.0], [28.0, 16.0, 4.0], [0.0, 0.0])),
        )
        .with_child(
            "left_paddle",
            PartDef::new(PartPose::offset_and_rotation(
                3.0,
                -4.0,
                9.0,
                0.0,
                0.0,
                PI / 16.0,
            ))
            .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [0.0, 24.0]))
            .with_cube(cube([-1.001, -3.0, 8.0], [1.0, 6.0, 7.0], [0.0, 24.0])),
        )
        .with_child(
            "right_paddle",
            PartDef::new(PartPose::offset_and_rotation(
                3.0,
                -4.0,
                -9.0,
                0.0,
                PI,
                PI / 16.0,
            ))
            .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [40.0, 24.0]))
            .with_cube(cube([0.001, -3.0, 8.0], [1.0, 6.0, 7.0], [40.0, 24.0])),
        )
}

/// Vanilla's own raft-model raft-model construction: sheet 128×64.
pub fn raft_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root: raft_hull(),
    }
}

/// Vanilla's own raft-model chest-raft-model construction: same hull plus chest boxes at raft-specific
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

/// vanilla's own minecart model's body-layer construction: bottom + 4 walls, flat root. Vanilla's
/// chest/hopper/tnt/furnace/command-block/spawner minecarts all reuse this
/// exact geometry class (they differ only by a separate block-overlay render
/// layer), so a single `"minecart"` entry covers them —
/// `lodestone_render::entity::canonical_model_name` aliases every
/// server-simulated subtype onto this rig. The overlay itself is not part of
/// this corpus entry: it is a block model, not a second cuboid rig, and draws
/// through `crates/lodestone-shell/src/gpu/moving_blocks.rs`'s
/// `merge_minecart_contents` instead. Sheet 64×32.
pub fn minecart_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                4.0,
                0.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
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
            PartDef::new(PartPose::offset_and_rotation(
                9.0,
                4.0,
                0.0,
                0.0,
                PI / 2.0,
                0.0,
            ))
            .with_cube(cube([-8.0, -9.0, -1.0], [16.0, 8.0, 2.0], [0.0, 0.0])),
        )
        .with_child(
            "left",
            PartDef::new(PartPose::offset_and_rotation(0.0, 4.0, -7.0, 0.0, PI, 0.0))
                .with_cube(cube([-8.0, -9.0, -1.0], [16.0, 8.0, 2.0], [0.0, 0.0])),
        )
        .with_child(
            "right",
            PartDef::new(PartPose::offset(0.0, 4.0, 7.0)).with_cube(cube(
                [-8.0, -9.0, -1.0],
                [16.0, 8.0, 2.0],
                [0.0, 0.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}
