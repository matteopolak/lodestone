use super::{CubeDef, EntityModelDef, PartDef, PartPose, SHULKER_SHEET};

/// A shulker box's shell — vanilla's own shulker-model box-layer construction,
/// via the shared
/// shell-mesh construction step:
///
/// ```text
/// lid   texOffs(0,  0)  addBox(-8, -16, -8,  16, 12, 16)  pose offset(0, 24, 0)
/// base  texOffs(0, 28)  addBox(-8,  -8, -8,  16,  8, 16)  pose offset(0, 24, 0)
/// ```
///
/// **The box-layer construction, not the body-layer one** — the two share the
/// shell-mesh construction
/// and the body layer adds a third `head` part for the *mob*. A block-entity
/// shulker box has no head, and baking the body layer here would draw a shulker's
/// face floating inside every box in the world.
///
/// `lid` and `base` are siblings with the **same** pivot `(0, 24, 0)`, which is
/// the sole reason this type "fits the existing `(model, texture)` batch key as
/// is": vanilla's own shulker-box-renderer per-frame pose step only ever moves `lid`, and
/// a closed box (`progress == 0`) needs no pose override at all — so a shulker
/// box is one static mesh per dye colour and nothing per instance. The open
/// animation is `lid.y = 24 - progress * 8` and `lid.yRot = 270° * progress`, and
/// it needs a container-open signal this client does not have yet; see
/// `docs/block-entity-renderers.md`.
///
/// Authored **block-space-up** like [`chest_single_model`] and [`bell_model`]:
/// vanilla's own shulker-box-renderer model-transform construction folds its own
/// `scale(1, -1, -1) · translate(0, -1, 0)` flip into the *placement* matrix, so
/// the box origins here add to `PartPose` with no sign change.
#[must_use]
pub fn shulker_box_model() -> EntityModelDef {
    let pivot = PartPose::offset(0.0, 24.0, 0.0);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "lid",
            PartDef::new(pivot).with_cube(CubeDef::new(
                [-8.0, -16.0, -8.0],
                [16.0, 12.0, 16.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "base",
            PartDef::new(pivot).with_cube(CubeDef::new(
                [-8.0, -8.0, -8.0],
                [16.0, 8.0, 16.0],
                [0.0, 28.0],
            )),
        );
    EntityModelDef {
        texture_width: SHULKER_SHEET.0,
        texture_height: SHULKER_SHEET.1,
        root,
    }
}
