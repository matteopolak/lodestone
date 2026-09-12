use super::{
    CONDUIT_CAGE_SHEET, CONDUIT_EYE_SHEET, CONDUIT_SHELL_SHEET, CONDUIT_WIND_SHEET, CubeDef,
    EntityModelDef, PartDef, PartPose,
};

/// The conduit's eye — vanilla's own conduit-renderer eye-layer construction:
///
/// ```text
/// eye  texOffs(0, 0)  addBox(-4, -4, 0,  8, 8, 0, CubeDeformation(0.01F))  pose ZERO
/// ```
///
/// A near-planar box: zero depth grown by `0.01` texels on every axis by the
/// deformation, matching vanilla's own `CubeDeformation` rather than a hand
/// wave at "basically a quad". This is the part vanilla's own conduit-renderer
/// submit step
/// billboards toward the camera and re-skins between `open_eye`/`closed_eye`
/// per frame — see `lodestone_render::block_entity::resolve_conduit`'s doc for
/// the billboard and the hunting-state sprite swap, neither of which belongs
/// in the geometry.
#[must_use]
pub fn conduit_eye_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "eye",
        PartDef::new(PartPose::ZERO)
            .with_cube(CubeDef::new([-4.0, -4.0, 0.0], [8.0, 8.0, 0.0], [0.0, 0.0]).grown(0.01)),
    );
    EntityModelDef {
        texture_width: CONDUIT_EYE_SHEET.0,
        texture_height: CONDUIT_EYE_SHEET.1,
        root,
    }
}

/// The conduit's "wind" plume — vanilla's own conduit-renderer wind-layer construction:
///
/// ```text
/// wind  texOffs(0, 0)  addBox(-8, -8, -8,  16, 16, 16)  pose ZERO
/// ```
///
/// One full 16-texel cube, drawn **twice** per active frame at two different
/// poses sharing this one mesh (`resolve_conduit`'s two `CONDUIT_WIND`
/// instances) — vanilla reuses the same model part for both
/// submit-model-part calls in its own conduit-renderer submit step's active branch.
#[must_use]
pub fn conduit_wind_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "wind",
        PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
            [-8.0, -8.0, -8.0],
            [16.0, 16.0, 16.0],
            [0.0, 0.0],
        )),
    );
    EntityModelDef {
        texture_width: CONDUIT_WIND_SHEET.0,
        texture_height: CONDUIT_WIND_SHEET.1,
        root,
    }
}

/// The conduit's **inactive** shell — vanilla's own conduit-renderer shell-layer construction:
///
/// ```text
/// shell  texOffs(0, 0)  addBox(-3, -3, -3,  6, 6, 6)  pose ZERO
/// ```
///
/// Drawn slowly spinning (`entity/conduit/base`) whenever the frame is not
/// complete — the `!state.isActive` branch of vanilla's own conduit-renderer
/// submit step. A
/// smaller cube than [`conduit_cage_model`]'s (6×6×6 against 8×8×8): the
/// conduit visibly grows when it activates, not just changes texture.
#[must_use]
pub fn conduit_shell_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "shell",
        PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
            [-3.0, -3.0, -3.0],
            [6.0, 6.0, 6.0],
            [0.0, 0.0],
        )),
    );
    EntityModelDef {
        texture_width: CONDUIT_SHELL_SHEET.0,
        texture_height: CONDUIT_SHELL_SHEET.1,
        root,
    }
}

/// The conduit's **active** shell ("cage") — vanilla's own conduit-renderer cage-layer construction:
///
/// ```text
/// shell  texOffs(0, 0)  addBox(-4, -4, -4,  8, 8, 8)  pose ZERO
/// ```
///
/// Named `"shell"` in the jar too (vanilla's own cage-layer construction also calls
/// `addOrReplaceChild("shell", …)`) — the two are still separate *models*
/// here (`conduit_shell` vs `conduit_cage`), each its own
/// [`BlockEntityModelEntry`] with its own sheet, since a real client never
/// draws both in the same frame (vanilla's own conduit-renderer submit step
/// branches on
/// `state.isActive` and picks exactly one).
#[must_use]
pub fn conduit_cage_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "shell",
        PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
            [-4.0, -4.0, -4.0],
            [8.0, 8.0, 8.0],
            [0.0, 0.0],
        )),
    );
    EntityModelDef {
        texture_width: CONDUIT_CAGE_SHEET.0,
        texture_height: CONDUIT_CAGE_SHEET.1,
        root,
    }
}
