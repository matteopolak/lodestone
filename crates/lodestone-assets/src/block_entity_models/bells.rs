use super::{BELL_SHEET, CubeDef, EntityModelDef, PartDef, PartPose};

/// A bell's swinging body and its flared bottom rim —
/// vanilla's own bell-model body-layer construction:
///
/// ```text
/// bell_body  texOffs(0,  0)  box(-3, -6, -3,  6, 7, 6)  pose offset(8, 12, 8)
///   bell_base  texOffs(0, 13)  box(4, 4, 4,  8, 2, 8)  pose offset(-8, -12, -8)  (child of bell_body)
/// ```
///
/// `bell_base` is **nested inside** `bell_body` in the real jar
/// (`bellBody.addOrReplaceChild("bell_base", …)`), not a sibling under root.
/// Its own local pose `(-8, -12, -8)` exactly cancels `bell_body`'s pivot
/// `(8, 12, 8)`, so the flared rim's *world* pivot lands at the block's own
/// corner `(0, 0, 0)` — the rim (`4..12, 4..6, 4..12` texels there) then sits
/// directly below the tapered body (`5..11, 6..13, 5..11` texels once
/// `bell_body`'s own pivot is folded in), which is exactly what a bell's
/// flared bottom skirt should do. The nesting also matters for the
/// animation: `BellModel.setupAnim` only ever poses `bellBody.xRot`/`zRot`
/// (see [`crate::block_entity_models`]'s sibling doc in `lodestone-render`'s
/// `bell_shake_angle`) and the rim swings with it *because* it is a child,
/// the same "shared handle" reasoning [`chest_single_model`]'s doc gives for
/// why `lid`/`lock` share a pivot rather than nesting one inside the other —
/// here the correct shape is the opposite: nested, not siblings, because
/// vanilla itself nests them.
///
/// Authored **block-space-up**, the same convention [`chest_single_model`]
/// uses and unlike [`skull_head_part`]: vanilla's own bell-renderer submit step applies no
/// `scale(-1, -1, 1)` flip (unlike its own skull-block renderer), so `CubeDef::origin`
/// and `PartPose` add directly with no sign flip.
#[must_use]
pub fn bell_model() -> EntityModelDef {
    let bell_base = PartDef::new(PartPose::offset(-8.0, -12.0, -8.0)).with_cube(CubeDef::new(
        [4.0, 4.0, 4.0],
        [8.0, 2.0, 8.0],
        [0.0, 13.0],
    ));
    let bell_body = PartDef::new(PartPose::offset(8.0, 12.0, 8.0))
        .with_cube(CubeDef::new(
            [-3.0, -6.0, -3.0],
            [6.0, 7.0, 6.0],
            [0.0, 0.0],
        ))
        .with_child("bell_base", bell_base);
    let root = PartDef::new(PartPose::ZERO).with_child("bell_body", bell_body);
    EntityModelDef {
        texture_width: BELL_SHEET.0,
        texture_height: BELL_SHEET.1,
        root,
    }
}
