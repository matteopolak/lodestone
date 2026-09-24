use super::{CubeDef, EntityModelDef, PartDef, PartPose, SHIELD_SHEET};

/// A shield — vanilla's own shield-model layer construction:
///
/// ```text
/// plate   texOffs(0,  0)  addBox(-6, -11, -2,  12, 22, 1)  pose ZERO
/// handle  texOffs(26, 0)  addBox(-1,  -3, -1,   2,  6, 6)  pose ZERO
/// ```
///
/// Two parts, both `PartPose::ZERO` — unlike a banner's body+flag, vanilla
/// draws the whole shield (plate and handle together) as **one**
/// submit-model call, opaque, then re-submits this exact same mesh once per
/// pattern layer through the translucent pattern pipeline
/// (vanilla's own banner-renderer submit-patterns step, called from its own
/// shield-special-renderer submit step — the model argument is
/// the whole shield model, not a `flag`-only sub-part the way a banner's
/// masks are). So this crate's shield has no analogue of a banner's
/// `"banner_flag"`: a caller drawing a pattern layer reuses this same
/// `"shield"` mesh, at the
/// same placement, with a different sprite and tint — see
/// `lodestone_render::block_entity`'s shield section for the draw-order this
/// enables.
#[must_use]
pub fn shield_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "plate",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [-6.0, -11.0, -2.0],
                [12.0, 22.0, 1.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "handle",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [-1.0, -3.0, -1.0],
                [2.0, 6.0, 6.0],
                [26.0, 0.0],
            )),
        );
    EntityModelDef {
        texture_width: SHIELD_SHEET.0,
        texture_height: SHIELD_SHEET.1,
        root,
    }
}
