use super::{BANNER_SHEET, CubeDef, EntityModelDef, PartDef, PartPose};

/// A standing banner's pole and cross-bar — vanilla's own banner-model
/// body-layer construction, standing variant:
///
/// ```text
/// pole  texOffs(44, 0)  addBox(-1, -42, -1,  2, 42, 2)  pose ZERO
/// bar   texOffs(0, 42)  addBox(-10, -44, -1,  20, 2, 2)  pose ZERO
/// ```
///
/// Only `standing = true` is ported (this issue's own scope — a wall banner
/// is `createBodyLayer(false)`, a second entry later with different box
/// origins for `bar` and no `pole` at all). Draws through the ordinary
/// opaque block-entity batcher with [`BlockEntityModelEntry::texture`]'s
/// sheet (`entity/banner/banner_base` — vanilla's `Sheets.BANNER_BASE`),
/// the *wood/cloth* texture, not a pattern mask; the
/// coloured pattern masks are a wholly separate translucent draw list (see
/// `lodestone_render::block_entity`'s banner doc) reusing the *flag* mesh,
/// never this one.
#[must_use]
pub fn banner_body_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "pole",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [-1.0, -42.0, -1.0],
                [2.0, 42.0, 2.0],
                [44.0, 0.0],
            )),
        )
        .with_child(
            "bar",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [-10.0, -44.0, -1.0],
                [20.0, 2.0, 2.0],
                [0.0, 42.0],
            )),
        );
    EntityModelDef {
        texture_width: BANNER_SHEET.0,
        texture_height: BANNER_SHEET.1,
        root,
    }
}

/// A standing banner's cloth — vanilla's own banner-flag-model flag-layer
/// construction, standing variant:
///
/// ```text
/// flag  texOffs(0, 0)  addBox(-10, 0, -2,  20, 40, 1)  pose offset(0, -44, 0)
/// ```
///
/// One part, one box — deliberately a single rigid box rather than the
/// per-vertex cloth-wave geometry an earlier pass through this doc
/// (mistakenly) assumed vanilla has. `BannerFlagModel.setupAnim` poses only
/// `flag.xRot`, a single per-part rotation
/// (`lodestone_render::block_entity::banner_flag_x_rot`) —
/// [`BlockEntityMesh::part_transforms`](crate::block_entity)'s override
/// mechanism (already used by the chest lid and the bell body) is the right
/// shape for it, not a new animation family.
///
/// Drawn **twice** by a real consumer: once opaque through the same
/// `entity/banner/banner_base` sheet [`banner_body_model`] uses (vanilla's
/// `submitBanner` passes `Sheets.BANNER_BASE` to both the body and the flag
/// model), and then again, translucent, once per pattern layer — see
/// `lodestone_render::block_entity`'s module doc for the draw-order and
/// pipeline split.
#[must_use]
pub fn banner_flag_model() -> EntityModelDef {
    let flag = PartPose::offset(0.0, -44.0, 0.0);
    let root = PartDef::new(PartPose::ZERO).with_child(
        "flag",
        PartDef::new(flag).with_cube(CubeDef::new(
            [-10.0, 0.0, -2.0],
            [20.0, 40.0, 1.0],
            [0.0, 0.0],
        )),
    );
    EntityModelDef {
        texture_width: BANNER_SHEET.0,
        texture_height: BANNER_SHEET.1,
        root,
    }
}

/// A **wall** banner's cross-bar — vanilla's own banner-model body-layer
/// construction, wall variant:
///
/// ```text
/// bar  texOffs(0, 42)  addBox(-10, -20.5, 9.5,  20, 2, 2)  pose ZERO
/// ```
///
/// **No `pole`.** Vanilla's own body-layer construction adds the pole only under `if (standing)`, and
/// this is the branch that skips it — a wall banner hangs off a block face, so a
/// standing banner's 42-texel post would be a pole floating in mid-air. That is
/// exactly what happens if the two are conflated, and it is why the gather
/// declined wall banners outright until this mesh existed.
///
/// The `bar` box is not the standing one moved: **both of its `y` and `z` origins
/// differ** (`-20.5, 9.5` against `-44, -1`), from the same ternary pair in
/// vanilla's own body-layer construction. Only the texel offsets and extents are shared, so this is a
/// second entry rather than a placement variant.
#[must_use]
pub fn banner_wall_body_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "bar",
        PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
            [-10.0, -20.5, 9.5],
            [20.0, 2.0, 2.0],
            [0.0, 42.0],
        )),
    );
    EntityModelDef {
        texture_width: BANNER_SHEET.0,
        texture_height: BANNER_SHEET.1,
        root,
    }
}

/// A **wall** banner's cloth — vanilla's own banner-flag-model flag-layer
/// construction, wall variant:
///
/// ```text
/// flag  texOffs(0, 0)  addBox(-10, 0, -2,  20, 40, 1)  pose offset(0, -20.5, 10.5)
/// ```
///
/// The **cube is byte-identical** to [`banner_flag_model`]'s; only the part's rest
/// pose differs (`(0, -20.5, 10.5)` against `(0, -44, 0)`). So this is one mesh
/// that could in principle have been one mesh with a pose override — and it is not,
/// for the reason [`banner_flag_model`]'s doc already gives: the flag's `x_rot`
/// sway is *itself* a pose override, and stacking a second, static override on the
/// same part is how the two silently start fighting over one field.
///
/// `BannerFlagModel.setupAnim` poses `flag.xRot` identically for both kinds — the
/// sway is not attachment-dependent.
#[must_use]
pub fn banner_wall_flag_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "flag",
        PartDef::new(PartPose::offset(0.0, -20.5, 10.5)).with_cube(CubeDef::new(
            [-10.0, 0.0, -2.0],
            [20.0, 40.0, 1.0],
            [0.0, 0.0],
        )),
    );
    EntityModelDef {
        texture_width: BANNER_SHEET.0,
        texture_height: BANNER_SHEET.1,
        root,
    }
}
