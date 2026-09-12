use super::{CubeDef, EntityModelDef, PartDef, PartPose};

/// A skull/head's single box — vanilla's own skull-model head-mesh construction:
/// a box of `(-4, -8, -4)` origin, `(8, 8, 8)` size at the identity pose, texel offset `(0, 0)`.
///
/// Unlike the chest models, this is authored in the **same Y-down convention
/// as a mob's own head part** — vanilla never re-authored it block-space-up
/// the way its own chest model was. Vanilla's own skull-block-renderer own
/// placement transforms
/// (its own ground/wall transformation construction, ported as
/// `lodestone_render::block_entity::{skull_ground_placement_matrix,
/// skull_wall_placement_matrix}`) apply vanilla's `scale(-1, -1, 1)` flip to
/// compensate, exactly the sign [`crate::entity::entity_model_matrix`] uses.
/// Porting this box pre-flipped (to look "right" in isolation) would double
/// the flip once placement is applied.
pub(super) fn skull_head_part() -> PartDef {
    PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
        [-4.0, -8.0, -4.0],
        [8.0, 8.0, 8.0],
        [0.0, 0.0],
    ))
}

/// The 64×32-canvas skull head — vanilla's own skull-model mob-head-layer
/// construction. Used by
/// skeleton, wither skeleton and creeper, whose skin PNGs really are 64×32.
///
/// **Two models exist for one box, not one.** The head's `texOffs(0, 0)`
/// placement is identical on both canvases (the cube only occupies the
/// top-left 32×16 texels regardless of total sheet size — the extra height on
/// the 64×64 canvas is room for the "hat" overlay and body parts this
/// renderer does not draw), but UV normalisation divides by the *declared*
/// canvas size at bake time. Baking one model at 64×32 and sampling a 64×64
/// skin (or vice versa) would double or halve the head's `v` extent — a
/// texture-stretch bug invisible in a coverage-only gate, since the mesh
/// still draws a full box either way.
#[must_use]
pub fn skull_mob_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child("head", skull_head_part());
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// The 64×64-canvas skull head — vanilla's own skull-model humanoid-head-layer
/// construction.
/// Used by zombie (whose skin moved to 64×64) and player (always 64×64). See
/// [`skull_mob_model`] for why the canvas size is a second model rather than
/// a parameter.
///
/// # The `"hat"` overlay, and the comment that used to say it was pointless
///
/// Vanilla's own humanoid-head-layer construction adds a `"hat"` child *of* `head` — the same
/// `-4, -8, -4` box, at `texOffs(32, 0)`, inflated `0.25` — and it is what
/// draws a skin's second layer: the hair, the helmet, the pumpkin. It is the
/// **only** part of this model whose texels come from the right-hand half of
/// the sheet.
///
/// It was previously omitted, with a comment reasoning that "every ported
/// skull type here draws with a fixed skin the hat layer would just
/// double-draw against". That was true when written and is not any more: a
/// placed player head now resolves its owner's real skin
/// (`BlockEntityTexture::PlayerSkin`), so the layer that comment dismissed is
/// exactly the layer a player's head is missing — and it was never right for
/// the *zombie* head either, whose jar sheet carries a real overlay at
/// `(32, 0)` and which vanilla draws through this same layer.
///
/// The inflation is what keeps the two boxes from z-fighting: `0.25` texels
/// puts the overlay strictly outside the base head on every face. Dropping it
/// while keeping the part is worse than not having the part at all.
///
/// `skull_mob_model` correctly has no hat — vanilla's own mob-head-layer
/// construction adds none,
/// and a skeleton sheet has nothing at `(32, 0)` to draw.
#[must_use]
pub fn skull_humanoid_model() -> EntityModelDef {
    let head = skull_head_part().with_child(
        "hat",
        PartDef::new(PartPose::ZERO).with_cube(
            CubeDef::new([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [32.0, 0.0])
                .grown(SKULL_HAT_INFLATION),
        ),
    );
    let root = PartDef::new(PartPose::ZERO).with_child("head", head);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The ender dragon's own sheet, 256×256 — the canvas
/// vanilla's own dragon-head-model head-layer construction declares, and the same
/// `entity/enderdragon/dragon` PNG the mob renderer samples.
const DRAGON_HEAD_SHEET: (u32, u32) = (256, 256);

/// The piglin's own sheet, 64×64 — vanilla's own layer-definitions table
/// declares its piglin head layer at a 64×64 mesh definition.
const PIGLIN_HEAD_SHEET: (u32, u32) = (64, 64);

/// The dragon head — vanilla's own dragon-head-model head-layer construction:
///
/// ```text
/// head  offset(0, -7.986666, 0).scaled(0.75)
///   upper_lip           (-6, -1, -24)  12x5x16   texOffs(176, 44)
///   upper_head          (-8, -8, -10)  16x16x16  texOffs(112, 30)
///   scale    [mirror]   (-5, -12, -4)   2x4x6    texOffs(  0,  0)
///   nostril  [mirror]   (-5, -3, -22)   2x2x4    texOffs(112,  0)
///   scale               ( 3, -12, -4)   2x4x6    texOffs(  0,  0)
///   nostril             ( 3, -3, -22)   2x2x4    texOffs(112,  0)
///   jaw   offset(0, 4, -8)
///     jaw               (-6,  0, -16)  12x4x16   texOffs(176, 65)
/// ```
///
/// **Nothing about this is the shared 8×8×8 skull box.** It is a real
/// six-cube rig on the dragon's own 256×256 sheet, scaled to 0.75 at the part
/// pose — which is why porting it needed [`crate::entity::PartPose::scaled`]
/// rather than another entry beside [`skull_mob_model`]. The `mirror(true)`
/// pair matters: the left-hand scale and nostril share their right-hand
/// siblings' texels, read backwards, so dropping the flag gives two boxes
/// whose texture runs the wrong way and whose winding is inverted.
///
/// The jaw is a **child** of the head, so the head's `0.75` scale carries it;
/// its own resting angle is not the authored zero but
/// `lodestone_render::block_entity::dragon_head_jaw_x_rot`'s value, applied by
/// the renderer the way vanilla's own dragon-head-model per-frame pose step applies it.
#[must_use]
pub fn dragon_head_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -7.986_666, 0.0).scaled(0.75))
        .with_cube(CubeDef::new(
            [-6.0, -1.0, -24.0],
            [12.0, 5.0, 16.0],
            [176.0, 44.0],
        ))
        .with_cube(CubeDef::new(
            [-8.0, -8.0, -10.0],
            [16.0, 16.0, 16.0],
            [112.0, 30.0],
        ))
        .with_cube(CubeDef::new([-5.0, -12.0, -4.0], [2.0, 4.0, 6.0], [0.0, 0.0]).mirrored())
        .with_cube(CubeDef::new([-5.0, -3.0, -22.0], [2.0, 2.0, 4.0], [112.0, 0.0]).mirrored())
        .with_cube(CubeDef::new(
            [3.0, -12.0, -4.0],
            [2.0, 4.0, 6.0],
            [0.0, 0.0],
        ))
        .with_cube(CubeDef::new(
            [3.0, -3.0, -22.0],
            [2.0, 2.0, 4.0],
            [112.0, 0.0],
        ))
        .with_child(
            "jaw",
            PartDef::new(PartPose::offset(0.0, 4.0, -8.0)).with_cube(CubeDef::new(
                [-6.0, 0.0, -16.0],
                [12.0, 4.0, 16.0],
                [176.0, 65.0],
            )),
        );
    EntityModelDef {
        texture_width: DRAGON_HEAD_SHEET.0,
        texture_height: DRAGON_HEAD_SHEET.1,
        root: PartDef::new(PartPose::ZERO).with_child("head", head),
    }
}

/// The piglin head — vanilla's own piglin-head-model head-mesh construction,
/// which is
/// vanilla's own shared abstract-piglin add-head helper, with no deformation, verbatim:
///
/// ```text
/// head  PartPose.ZERO
///   (-5, -8, -4)  10x8x8  texOffs( 0, 0)     the wide snouted skull
///   (-2, -4, -5)   4x4x1  texOffs(31, 1)     the snout plate
///   ( 2, -2, -5)   1x2x1  texOffs( 2, 4)     left tusk
///   (-3, -2, -5)   1x2x1  texOffs( 2, 0)     right tusk
///   left_ear   offsetAndRotation( 4.5, -6, 0, 0, 0, -PI/6)  ( 0, 0, -2) 1x5x4 texOffs(51, 6)
///   right_ear  offsetAndRotation(-4.5, -6, 0, 0, 0,  PI/6)  (-1, 0, -2) 1x5x4 texOffs(39, 6)
/// ```
///
/// **Ten texels wide, not eight** — a piglin head is not a cube, so nothing
/// about it could have been recovered by pointing [`skull_mob_model`] at the
/// piglin sheet.
///
/// The two ears carry an authored `±PI/6` rest rotation that the renderer then
/// **overrides**: `PiglinHeadModel.setupAnim` assigns `zRot` unconditionally,
/// so a placed piglin head never shows `±PI/6` — it shows
/// `lodestone_render::block_entity::piglin_head_ear_z_rots`'s value. The
/// authored pose is kept faithful here anyway, because it is what the jar
/// declares and because the offset half of it *is* load-bearing.
#[must_use]
pub fn piglin_head_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::ZERO)
        .with_cube(CubeDef::new(
            [-5.0, -8.0, -4.0],
            [10.0, 8.0, 8.0],
            [0.0, 0.0],
        ))
        .with_cube(CubeDef::new(
            [-2.0, -4.0, -5.0],
            [4.0, 4.0, 1.0],
            [31.0, 1.0],
        ))
        .with_cube(CubeDef::new([2.0, -2.0, -5.0], [1.0, 2.0, 1.0], [2.0, 4.0]))
        .with_cube(CubeDef::new(
            [-3.0, -2.0, -5.0],
            [1.0, 2.0, 1.0],
            [2.0, 0.0],
        ))
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset_and_rotation(
                4.5,
                -6.0,
                0.0,
                0.0,
                0.0,
                -std::f32::consts::FRAC_PI_6,
            ))
            .with_cube(CubeDef::new([0.0, 0.0, -2.0], [1.0, 5.0, 4.0], [51.0, 6.0])),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset_and_rotation(
                -4.5,
                -6.0,
                0.0,
                0.0,
                0.0,
                std::f32::consts::FRAC_PI_6,
            ))
            .with_cube(CubeDef::new(
                [-1.0, 0.0, -2.0],
                [1.0, 5.0, 4.0],
                [39.0, 6.0],
            )),
        );
    EntityModelDef {
        texture_width: PIGLIN_HEAD_SHEET.0,
        texture_height: PIGLIN_HEAD_SHEET.1,
        root: PartDef::new(PartPose::ZERO).with_child("head", head),
    }
}

/// `SkullModel.createHumanoidHeadLayer`'s `new CubeDeformation(0.25F)` on the
/// `"hat"` overlay, in model texels.
///
/// Named because it is load-bearing twice over: it is what separates the two
/// coincident boxes, and it is what widens the model's baked AABB from 8 to
/// 8.5 texels on every axis — which any gate deriving a screen extent from
/// that AABB will see.
pub const SKULL_HAT_INFLATION: f32 = 0.25;
