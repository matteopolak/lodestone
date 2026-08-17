//! Block-entity renderers: the cuboid rigs vanilla's `BlockEntityRenderer`s
//! draw for blocks whose block model does not describe them (issue #23).
//!
//! Chest today. The module is shaped so a second type is an entry in
//! [`lodestone_assets::block_entity_models::BLOCK_ENTITY_MODELS`] plus a
//! `*_spawns → BlockEntityInstance` resolver, not a new pipeline.
//!
//! # Why this is not `crate::entity`, when it shares every primitive
//!
//! The bake is identical — vanilla's `ModelPart` has no idea whether its owner
//! is a mob or a chest, and this module reuses `CubeDef`/`PartDef`/`bake_entity_parts`
//! and [`crate::entity`]'s winding rule verbatim. **Placement is the difference,
//! and it is total:**
//!
//! | | entity | block entity |
//! |---|---|---|
//! | model space | Y-**down** | Y-**up** |
//! | placement | `entity_model_matrix`: `translate(feet) · rotY(180°−yaw) · scale(−s,−s,s) · translate(0,−1.501,0)` | [`block_entity_placement_matrix`]: `translate(pos) · rotateAround(−yaw, ½,0,½)` |
//! | anchor | the entity's feet | the block's corner |
//!
//! `ChestRenderer.submit`'s *entire* prologue is
//! `Matrix4f().rotationAround(Axis.YP.rotationDegrees(-facing.toYRot()), 0.5F, 0.0F, 0.5F)`
//! — no flip and no lift, because the chest's own texels are already block-space:
//! `bottom` spans y `0..10` texels (`0..0.625` blocks off the floor) and the
//! `lid` pivot at y `9` puts the closed lid's top at `14/16`, the real chest
//! height. Feeding a chest through the entity matrix buries it 1.5 blocks down,
//! upside down. `placement_does_not_flip_or_lift` is the assertion that catches
//! that, and it compares against a real matrix rather than restating a constant.
//!
//! # Determinant, and why there is nothing to get backwards here
//!
//! `CLAUDE.md`'s winding rule says to derive the front-facing sign from a real
//! camera rather than asserting a polarity. That warning applies where a
//! *handedness flip* is in play — the GUI item pose, and the entity path's
//! `scale(−1,−1,1)`. This placement matrix is a translation composed with a
//! rotation: `det = +1` exactly, for every facing, so it cannot reverse winding
//! and the quads' baked outward normals reach the rasteriser unchanged.
//! `placement_preserves_orientation` measures the determinant rather than
//! asserting it is "positive because rotations are".
//!
//! # The lid animation, and the two easings that are not the same one
//!
//! Vanilla applies **two** transforms to the raw openness and they live in
//! different classes, which is exactly the kind of thing a summary loses:
//!
//! 1. `ChestRenderer.submit` eases the *progress*:
//!    `open = 1 - open; open = 1 - open*open*open` — a cubic ease-out
//!    ([`chest_lid_openness`]).
//! 2. `ChestModel.setupAnim` turns the eased value into an *angle*:
//!    `lid.xRot = -(open * PI/2)`, and then `lock.xRot = lid.xRot`
//!    ([`chest_lid_x_rot`]).
//!
//! Collapsing these into one function that takes raw openness and returns an
//! angle would still look right at the two endpoints (0 and 1 are fixed points
//! of the ease) and be wrong for every frame in between — an animation bug that
//! a screenshot at rest cannot see. They are separate, and both are unit-tested
//! against the endpoints *and* the midpoint.
//!
//! # How to change it
//!
//! * A new block-entity type needs: a model in `lodestone-assets`, a texture-stem
//!   resolver here (see [`chest_texture_stem`]), a `*Spawn` input struct, and an
//!   arm in the shell's prepare. It does **not** need a new pipeline — everything
//!   here draws through [`crate::entity_pipeline::EntityPipeline`], which spends
//!   exactly two bind groups (camera+fog / texture) and so leaves the model
//!   shader's 4-group floor alone.
//! * Part names are the animation's only handle. [`BlockEntityMesh::index_of`]
//!   resolves `"lid"`/`"lock"` by name; renaming either in the asset corpus
//!   silently freezes the lid shut (the mesh still draws, so a coverage-only gate
//!   stays green). `lodestone-assets`'
//!   `lid_and_lock_share_the_pivot_the_animation_rotates_about` is the guard.
//! * [`chest_texture_stems`] is what the shell preloads. A material added to
//!   [`ChestMaterial`] and *not* to that list resolves to a stem with no bind
//!   group, and the shell falls back — visible, but wrong. Both are derived from
//!   the same match, so add the arm and the list entry together.

use glam::{Mat4, Quat, Vec3};
use lodestone_assets::ResourceLocation;
use lodestone_assets::block_entity_models::{BLOCK_ENTITY_MODELS, BlockEntityModelEntry};
use lodestone_assets::entity::{EntityModelDef, PartPose, bake_entity_parts};

use crate::banner_pattern::{DyeColor, StoredPatternLayer, banner_pattern_layers};
use crate::camera::Frustum;
use crate::entity::{ENTITY_FULLBRIGHT, PartRange, push_part_quads};
use crate::entity_pipeline::InstanceTint;
use crate::models::ModelVertex;

/// Model name of a single chest, keying both the mesh set and the shell's
/// texture map.
pub const CHEST_SINGLE: &str = "chest";
/// Model name of a double chest's left half.
pub const CHEST_LEFT: &str = "chest_left";
/// Model name of a double chest's right half.
pub const CHEST_RIGHT: &str = "chest_right";

/// Which of vanilla's three chest *layers* an instance draws.
///
/// Not a pose of one layer: the halves are 15 texels wide against the single
/// chest's 14 and each omits the seam face, so this selects a different mesh.
/// Mirrors `ChestType` (`SINGLE`/`LEFT`/`RIGHT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChestHalf {
    /// A lone chest.
    Single,
    /// The left half of a double chest.
    Left,
    /// The right half of a double chest.
    Right,
}

impl ChestHalf {
    /// The model name this half draws.
    #[must_use]
    pub const fn model(self) -> &'static str {
        match self {
            ChestHalf::Single => CHEST_SINGLE,
            ChestHalf::Left => CHEST_LEFT,
            ChestHalf::Right => CHEST_RIGHT,
        }
    }

    /// Parses vanilla's `type` block-state property value.
    ///
    /// A chest state always has `type`; anything unrecognised (a future value,
    /// a datapack block reusing the block entity) degrades to
    /// [`ChestHalf::Single`], which draws a complete chest rather than a
    /// half-open shell with a hole in it.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "left" => ChestHalf::Left,
            "right" => ChestHalf::Right,
            _ => ChestHalf::Single,
        }
    }
}

/// Which chest sheet an instance draws with.
///
/// Mirrors `ChestRenderState.ChestMaterialType` (via `Sheets.chooseSprite`).
/// Copper's four weathering stages are separate arms rather than a nested enum
/// so [`chest_texture_stem`] stays one flat match with [`chest_texture_stems`]
/// derived from the same set of arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChestMaterial {
    /// `minecraft:chest`.
    Regular,
    /// `minecraft:trapped_chest`.
    Trapped,
    /// `minecraft:ender_chest` — one sheet, no left/right variants.
    Ender,
    /// The seasonal override (`SpecialDates.isExtendedChristmas()`).
    Christmas,
    /// `minecraft:copper_chest`, unaffected.
    CopperUnaffected,
    /// `minecraft:exposed_copper_chest`.
    CopperExposed,
    /// `minecraft:weathered_copper_chest`.
    CopperWeathered,
    /// `minecraft:oxidized_copper_chest`.
    CopperOxidized,
}

impl ChestMaterial {
    /// Resolves a block's registry path (namespace stripped) to its chest
    /// material, or `None` if the path is not a chest at all.
    ///
    /// This is the *block*-driven half of `ChestRenderer.getChestMaterial`; the
    /// seasonal [`ChestMaterial::Christmas`] override is date-driven and belongs
    /// to the caller (see [`chest_material_with_season`]).
    #[must_use]
    pub fn from_block_path(path: &str) -> Option<Self> {
        Some(match path {
            "chest" => ChestMaterial::Regular,
            "trapped_chest" => ChestMaterial::Trapped,
            "ender_chest" => ChestMaterial::Ender,
            "copper_chest" => ChestMaterial::CopperUnaffected,
            "exposed_copper_chest" => ChestMaterial::CopperExposed,
            "weathered_copper_chest" => ChestMaterial::CopperWeathered,
            "oxidized_copper_chest" => ChestMaterial::CopperOxidized,
            _ => return None,
        })
    }
}

/// Applies vanilla's christmas override to a block-derived material.
///
/// `ChestRenderer.getChestMaterial` checks copper **first**, then ender, and
/// only then the seasonal flag — so a copper or ender chest keeps its own sheet
/// in December while a plain or trapped chest does not. Ordering this the
/// obvious way (season first) would repaint every copper chest for two weeks a
/// year, which is precisely the kind of defect nobody sees until December.
#[must_use]
pub fn chest_material_with_season(material: ChestMaterial, christmas: bool) -> ChestMaterial {
    if !christmas {
        return material;
    }
    match material {
        ChestMaterial::Regular | ChestMaterial::Trapped => ChestMaterial::Christmas,
        other => other,
    }
}

/// The jar texture stem (no `assets/<ns>/textures/` prefix, no `.png`) for a
/// material/half pair — `Sheets.chooseSprite` plus
/// `ChestSpecialRenderer.createDefaultTextures`'s `<prefix>`/`<prefix>_left`/
/// `<prefix>_right` naming.
///
/// **Ender is deliberately half-independent.** `Sheets.chooseSprite` returns the
/// single `ENDER_CHEST_LOCATION` for every `ChestType`, and the jar ships only
/// `entity/chest/ender.png` — no `ender_left`/`ender_right` exist. Deriving the
/// suffix uniformly would name a file that is not there and the chest would fall
/// back to a placeholder sheet, which reads as "the renderer is broken" rather
/// than "one texture is missing".
#[must_use]
pub const fn chest_texture_stem(material: ChestMaterial, half: ChestHalf) -> &'static str {
    match (material, half) {
        (ChestMaterial::Ender, _) => "entity/chest/ender",
        (ChestMaterial::Regular, ChestHalf::Single) => "entity/chest/normal",
        (ChestMaterial::Regular, ChestHalf::Left) => "entity/chest/normal_left",
        (ChestMaterial::Regular, ChestHalf::Right) => "entity/chest/normal_right",
        (ChestMaterial::Trapped, ChestHalf::Single) => "entity/chest/trapped",
        (ChestMaterial::Trapped, ChestHalf::Left) => "entity/chest/trapped_left",
        (ChestMaterial::Trapped, ChestHalf::Right) => "entity/chest/trapped_right",
        (ChestMaterial::Christmas, ChestHalf::Single) => "entity/chest/christmas",
        (ChestMaterial::Christmas, ChestHalf::Left) => "entity/chest/christmas_left",
        (ChestMaterial::Christmas, ChestHalf::Right) => "entity/chest/christmas_right",
        (ChestMaterial::CopperUnaffected, ChestHalf::Single) => "entity/chest/copper",
        (ChestMaterial::CopperUnaffected, ChestHalf::Left) => "entity/chest/copper_left",
        (ChestMaterial::CopperUnaffected, ChestHalf::Right) => "entity/chest/copper_right",
        (ChestMaterial::CopperExposed, ChestHalf::Single) => "entity/chest/copper_exposed",
        (ChestMaterial::CopperExposed, ChestHalf::Left) => "entity/chest/copper_exposed_left",
        (ChestMaterial::CopperExposed, ChestHalf::Right) => "entity/chest/copper_exposed_right",
        (ChestMaterial::CopperWeathered, ChestHalf::Single) => "entity/chest/copper_weathered",
        (ChestMaterial::CopperWeathered, ChestHalf::Left) => "entity/chest/copper_weathered_left",
        (ChestMaterial::CopperWeathered, ChestHalf::Right) => "entity/chest/copper_weathered_right",
        (ChestMaterial::CopperOxidized, ChestHalf::Single) => "entity/chest/copper_oxidized",
        (ChestMaterial::CopperOxidized, ChestHalf::Left) => "entity/chest/copper_oxidized_left",
        (ChestMaterial::CopperOxidized, ChestHalf::Right) => "entity/chest/copper_oxidized_right",
    }
}

/// Every material, for enumerating stems and for exhaustiveness in tests.
pub const CHEST_MATERIALS: &[ChestMaterial] = &[
    ChestMaterial::Regular,
    ChestMaterial::Trapped,
    ChestMaterial::Ender,
    ChestMaterial::Christmas,
    ChestMaterial::CopperUnaffected,
    ChestMaterial::CopperExposed,
    ChestMaterial::CopperWeathered,
    ChestMaterial::CopperOxidized,
];

/// Every chest sheet stem the renderer can ask for, deduplicated — what the
/// shell preloads into bind groups.
///
/// **Derived from [`chest_texture_stem`], never hand-listed.** A hand list is
/// how a material silently ends up with no bind group: the match compiles, the
/// list looks complete, and one chest in the world draws a placeholder.
#[must_use]
pub fn chest_texture_stems() -> Vec<&'static str> {
    let mut out = Vec::new();
    for material in CHEST_MATERIALS {
        for half in [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right] {
            let stem = chest_texture_stem(*material, half);
            if !out.contains(&stem) {
                out.push(stem);
            }
        }
    }
    out
}

/// Vanilla's cubic ease-out on a chest's raw openness
/// (`ChestRenderer.submit`: `open = 1 - open; open = 1 - open³`).
///
/// `0 → 0`, `1 → 1`, and `0.5 → 0.875` — noticeably *ahead* of linear, which is
/// what makes a chest snap open and settle. See the module doc on why this is
/// separate from [`chest_lid_x_rot`].
#[must_use]
pub fn chest_lid_openness(raw: f32) -> f32 {
    let inverted = 1.0 - raw.clamp(0.0, 1.0);
    1.0 - inverted * inverted * inverted
}

/// The lid's (and lock's) X rotation in radians for an **already eased**
/// openness — `ChestModel.setupAnim`: `lid.xRot = -(open * PI/2)`.
///
/// Negative: the lid tips backwards, away from the chest's facing.
#[must_use]
pub fn chest_lid_x_rot(eased_openness: f32) -> f32 {
    -(eased_openness * std::f32::consts::FRAC_PI_2)
}

/// The world placement transform for a block entity at `pos` facing
/// `facing_yaw_deg`.
///
/// `Matrix4f().rotationAround(Axis.YP.rotationDegrees(-facing.toYRot()), 0.5F, 0.0F, 0.5F)`
/// composed with the block's own translation. `facing_yaw_deg` is Minecraft's
/// `Direction.toYRot()` (south `0`, west `90`, north `180`, east `270`) — the
/// **same convention** the entity path's `body_yaw_deg` uses, so a caller does
/// not have to remember two.
///
/// No Y flip and no feet lift; see the module doc's table for why that is the
/// whole difference from [`crate::entity::entity_model_matrix`].
#[must_use]
pub fn block_entity_placement_matrix(pos: [i32; 3], facing_yaw_deg: f32) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    let pivot = Vec3::new(0.5, 0.0, 0.5);
    Mat4::from_translation(origin + pivot)
        * Mat4::from_rotation_y(-facing_yaw_deg.to_radians())
        * Mat4::from_translation(-pivot)
}

/// `Direction.toYRot()` for vanilla's four horizontal facing names, or `None`
/// for a value that is not a horizontal direction.
///
/// South is `0` because that is what vanilla's `Direction` returns
/// (`Direction.SOUTH.toYRot() == 0`), not because it is the natural choice —
/// reading this off `Direction`'s declaration order instead gives
/// down/up/north/south/west/east and rotates every chest by a quarter turn.
#[must_use]
pub fn horizontal_facing_yaw(name: &str) -> Option<f32> {
    Some(match name {
        "south" => 0.0,
        "west" => 90.0,
        "north" => 180.0,
        "east" => 270.0,
        _ => return None,
    })
}

/// Which of vanilla's five simple skull/head types this renderer draws.
///
/// Vanilla ships seven (`SkullBlock.Types` plus the player-profile case). The
/// first five share one CPU model (`SkullModel`, a single 8×8×8 head box —
/// see `lodestone_assets::block_entity_models::skull_mob_model`'s doc) and
/// differ only by canvas size and sheet; `dragon`/`piglin` use their own
/// multi-part rigs (`DragonHeadModel`/`PiglinHeadModel`) unrelated to that
/// shared box and are not ported — [`SkullType::from_block_path`] declines
/// them rather than drawing a wrong shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkullType {
    /// `minecraft:skeleton_skull`/`skeleton_wall_skull`.
    Skeleton,
    /// `minecraft:wither_skeleton_skull`/`wither_skeleton_wall_skull`.
    WitherSkeleton,
    /// `minecraft:zombie_head`/`zombie_wall_head`.
    Zombie,
    /// `minecraft:creeper_head`/`creeper_wall_head`.
    Creeper,
    /// `minecraft:player_head`/`player_wall_head`, always drawn with the
    /// default Steve skin (`DefaultPlayerSkin.getDefaultTexture()`) — a real
    /// profile skin needs a network fetch, out of scope here.
    Player,
}

/// Model name of the 64×32-canvas skull head (skeleton/wither skeleton/creeper).
pub const SKULL_MOB: &str = "skull_mob";
/// Model name of the 64×64-canvas skull head (zombie/player).
pub const SKULL_HUMANOID: &str = "skull_humanoid";

impl SkullType {
    /// Resolves a block's registry path (namespace stripped, wall/floor
    /// suffix included) to its skull type, or `None` for a path this
    /// renderer does not cover — including the two real skull types it
    /// declines (`dragon_head`/`piglin_head` and their wall variants) and
    /// anything that is not a skull at all.
    #[must_use]
    pub fn from_block_path(path: &str) -> Option<Self> {
        Some(match path {
            "skeleton_skull" | "skeleton_wall_skull" => SkullType::Skeleton,
            "wither_skeleton_skull" | "wither_skeleton_wall_skull" => SkullType::WitherSkeleton,
            "zombie_head" | "zombie_wall_head" => SkullType::Zombie,
            "creeper_head" | "creeper_wall_head" => SkullType::Creeper,
            "player_head" | "player_wall_head" => SkullType::Player,
            _ => return None,
        })
    }

    /// The baked model this type draws with.
    #[must_use]
    pub const fn model(self) -> &'static str {
        match self {
            SkullType::Skeleton | SkullType::WitherSkeleton | SkullType::Creeper => SKULL_MOB,
            SkullType::Zombie | SkullType::Player => SKULL_HUMANOID,
        }
    }
}

/// The jar sheet a [`SkullType`] draws with — `SkullBlockRenderer.SKIN_BY_TYPE`,
/// minus the `.png`/`assets/<ns>/textures/` wrapping.
///
/// **These are the mob skins already on disk for entity rendering, not a new
/// asset family.** `resources::load_block_entity_textures` (the shell's
/// loader) has to load them a second time regardless — this pass keeps its
/// own texture bind groups, entirely separate from `EntityRenderer`'s — but
/// there is nothing to author or ship beyond this stem list.
#[must_use]
pub const fn skull_texture_stem(skull_type: SkullType) -> &'static str {
    match skull_type {
        SkullType::Skeleton => "entity/skeleton/skeleton",
        SkullType::WitherSkeleton => "entity/skeleton/wither_skeleton",
        SkullType::Zombie => "entity/zombie/zombie",
        SkullType::Creeper => "entity/creeper/creeper",
        SkullType::Player => "entity/player/wide/steve",
    }
}

/// Every skull type, for enumerating stems and exhaustiveness in tests.
pub const SKULL_TYPES: &[SkullType] = &[
    SkullType::Skeleton,
    SkullType::WitherSkeleton,
    SkullType::Zombie,
    SkullType::Creeper,
    SkullType::Player,
];

/// Every skull sheet stem the renderer can ask for — what the shell preloads,
/// mirroring [`chest_texture_stems`].
#[must_use]
pub fn skull_texture_stems() -> Vec<&'static str> {
    SKULL_TYPES.iter().map(|t| skull_texture_stem(*t)).collect()
}

/// Where a skull/head sits: on the floor, spun by a `rotation` segment, or on
/// a wall, offset outward from the block it faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SkullOrientation {
    /// A floor-placed skull's `rotation` property, `0..16` — vanilla's
    /// `RotationSegment` (16 steps of 22.5°, **not**
    /// [`horizontal_facing_yaw`]'s four-direction convention: segment `0` is
    /// north, not south).
    Floor {
        /// `0..16`; out-of-range values still compose a matrix rather than
        /// panicking.
        rotation_segment: u8,
    },
    /// A wall skull's `facing` property, already converted by
    /// [`horizontal_facing_yaw`] — the direction the skull points *away from*
    /// its wall.
    Wall {
        /// `Direction.toYRot()` of the `facing` property.
        facing_yaw_deg: f32,
    },
}

/// The world placement transform for a floor-standing skull —
/// `SkullBlockRenderer.createGroundTransformation`:
/// `Matrix4f().translation(0.5, 0, 0.5).rotate(Axis.YP.rotationDegrees(-deg)).scale(-1, -1, 1)`,
/// composed with the block's own translation.
///
/// **This is the one block-entity placement in this module that *does*
/// flip** (`scale(-1, -1, 1)`, matching
/// [`crate::entity::entity_model_matrix`]'s sign exactly) — unlike
/// [`block_entity_placement_matrix`]. `SkullModel`'s head box is authored in
/// the same Y-down convention as a mob's head part, and vanilla never
/// re-authors it block-space-up the way `ChestModel` was; see
/// `lodestone_assets::block_entity_models::skull_head_part`'s doc.
/// `rotation_segment` is vanilla's `RotationSegment` (16 steps of 22.5°), not
/// [`horizontal_facing_yaw`]'s four-value convention — segment `0` is
/// **north** (`Direction.NORTH.toYRot() == 180`), so do not reuse that helper
/// here.
#[must_use]
pub fn skull_ground_placement_matrix(pos: [i32; 3], rotation_segment: u8) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    let segment_deg = f32::from(rotation_segment) * (360.0 / 16.0);
    Mat4::from_translation(origin)
        * Mat4::from_translation(Vec3::new(0.5, 0.0, 0.5))
        * Mat4::from_rotation_y(-segment_deg.to_radians())
        * Mat4::from_scale(Vec3::new(-1.0, -1.0, 1.0))
}

/// The world placement transform for a wall-mounted skull —
/// `SkullBlockRenderer.createWallTransformation`:
/// `translate(0.5 − dir.stepX·0.25, 0.25, 0.5 − dir.stepZ·0.25) · rotY(−opposite(dir).toYRot()) · scale(−1,−1,1)`.
///
/// `dir.getStepX()/getStepZ()` are recovered from `facing_yaw_deg` by trig
/// rather than a second lookup table that could drift from
/// [`horizontal_facing_yaw`]'s: south `0° → (0, 1)`, west `90° → (−1, 0)`,
/// north `180° → (0, −1)`, east `270° → (1, 0)` — hand-verified against
/// vanilla's `Direction` enum, not derived from this function.
#[must_use]
pub fn skull_wall_placement_matrix(pos: [i32; 3], facing_yaw_deg: f32) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    let yaw_rad = facing_yaw_deg.to_radians();
    let step_x = -yaw_rad.sin();
    let step_z = yaw_rad.cos();
    let opposite_yaw_deg = (facing_yaw_deg + 180.0).rem_euclid(360.0);
    Mat4::from_translation(origin)
        * Mat4::from_translation(Vec3::new(0.5 - step_x * 0.25, 0.25, 0.5 - step_z * 0.25))
        * Mat4::from_rotation_y(-opposite_yaw_deg.to_radians())
        * Mat4::from_scale(Vec3::new(-1.0, -1.0, 1.0))
}

/// The world placement transform for a ground/standing banner —
/// `BannerRenderer.modelTransformation`/`createGroundTransformation`
/// (`BannerRenderer.java:243-249`):
///
/// ```text
/// MODEL_TRANSLATION = (0.5, 0.0, 0.5)
/// MODEL_SCALE       = (0.6666667, -0.6666667, -0.6666667)
/// Transformation(MODEL_TRANSLATION, Axis.YP.rotationDegrees(-angle), MODEL_SCALE, null)
/// angle = RotationSegment.convertToDegrees(segment)   // segment * 22.5
/// ```
///
/// # A third placement shape, not a variant of the other two
///
/// [`block_entity_placement_matrix`]/[`skull_ground_placement_matrix`] both
/// exist because chest/skull geometry is baked *corner*-anchored, so rotating
/// it in place needs a pivot: `translate(pivot) · rotate · translate(-pivot)`.
/// A banner's model space is **not** corner-anchored — `BannerFlagModel`'s own
/// `PartPose::offset` already positions the flag relative to an origin the
/// same way an entity's skeleton does — so vanilla itself uses a straight
/// `T · R · S` here instead, confirmed against `com.mojang.math.Transformation`'s
/// own `compose` (`translation(t)` then `.rotate(leftRotation)` then
/// `.scale(scale)`, with `rightRotation` unused since this call passes `null`
/// for it): `M = T * R * S`, scale applied to the model first, then rotated,
/// then translated to the block. [`banner_flag_placement_verifies_against_the_transformation_compose_formula`]
/// pins this against that literal formula rather than against this function's
/// own arithmetic restated.
///
/// # The `2/3` scale and the Y/Z flip are both real
///
/// `BannerModel`/`BannerFlagModel` are shared with the banner **item**'s
/// GUI/held-item render (`SIZE = 0.6666667` is the same constant vanilla's
/// item-in-hand code uses elsewhere), so this in-world path re-applies that
/// same correction on top of otherwise entity-style baked geometry. Skipping
/// the flip renders the flag upside down and mirrored on Z; skipping the
/// scale renders it 1.5× too large. Both signs are negative (`-2/3` on Y
/// *and* Z), so — like [`skull_ground_placement_matrix`]'s single-axis flip
/// being paired with a second one — the *product* of the flips is positive
/// and this placement does not reverse a quad's winding, even though it does
/// mirror geometry on two axes; see
/// [`banner_ground_placement_preserves_orientation`] for the measurement.
///
/// The wall form is [`banner_wall_placement_matrix`], which is this **same**
/// `T · R · S` with `direction.toYRot()` in place of the rotation-segment angle
/// and no extra offset — the geometry, not the placement, is what differs
/// between the two.
#[must_use]
pub fn banner_ground_placement_matrix(pos: [i32; 3], rotation_segment: u8) -> Mat4 {
    let segment_deg = f32::from(rotation_segment) * (360.0 / 16.0);
    banner_placement_matrix(pos, segment_deg)
}

/// The world placement transform for a **wall** banner —
/// `BannerRenderer.createWallTransformation`, which is
/// `modelTransformation(direction.toYRot())`.
///
/// Byte-for-byte [`banner_ground_placement_matrix`] with a different angle: both
/// go through `modelTransformation`, so the `MODEL_TRANSLATION` `(0.5, 0, 0.5)`
/// and the `(2/3, -2/3, -2/3)` `MODEL_SCALE` are shared and there is **no** extra
/// push away from the wall — unlike [`skull_wall_placement_matrix`], which has a
/// `0.25` offset. Adding one here on the assumption that "wall placements offset"
/// would float the banner a quarter block off the block face; the offset a wall
/// banner needs is already baked into its own mesh's `z` origins
/// (`lodestone_assets::block_entity_models::banner_wall_body_model`), which is
/// why the wall geometry is a second mesh rather than the standing one moved.
#[must_use]
pub fn banner_wall_placement_matrix(pos: [i32; 3], facing_yaw_deg: f32) -> Mat4 {
    banner_placement_matrix(pos, facing_yaw_deg)
}

/// `BannerRenderer.modelTransformation(angle)`, shared by both attachments:
/// `Transformation(MODEL_TRANSLATION, Axis.YP.rotationDegrees(-angle), MODEL_SCALE, null)`,
/// composed as `T · R · S` — see [`banner_ground_placement_matrix`]'s doc for why
/// this is `T · R · S` and not the pivot sandwich chest and skull use.
fn banner_placement_matrix(pos: [i32; 3], angle_deg: f32) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    Mat4::from_translation(origin)
        * Mat4::from_translation(Vec3::new(0.5, 0.0, 0.5))
        * Mat4::from_rotation_y(-angle_deg.to_radians())
        * Mat4::from_scale(Vec3::new(2.0 / 3.0, -2.0 / 3.0, -2.0 / 3.0))
}

/// Per-block-position phase offset for a banner's cloth sway —
/// `BannerRenderer.extractRenderState` (`BannerRenderer.java:93`):
///
/// ```text
/// phase = (floorMod(x*7 + y*9 + z*13 + gameTime, 100) + partialTicks) / 100
/// ```
///
/// So neighbouring banners do not sway in lockstep, and the phase advances
/// one step per game tick, wrapping every 100 ticks. `game_time` is the
/// world's raw tick counter (`Level.getGameTime()`); `partial_tick` is the
/// usual sub-tick interpolation fraction, `0.0..1.0`.
#[must_use]
pub fn banner_phase(pos: [i32; 3], game_time: i64, partial_tick: f32) -> f32 {
    let sum = i64::from(pos[0]) * 7 + i64::from(pos[1]) * 9 + i64::from(pos[2]) * 13 + game_time;
    // `floorMod`, not Rust's `%` (which truncates toward zero): a negative
    // block coordinate must still wrap into `0..100`, not go negative.
    let wrapped = sum.rem_euclid(100);
    (wrapped as f32 + partial_tick) / 100.0
}

/// The flag part's `x_rot` override for a given phase —
/// `BannerFlagModel.setupAnim` (`BannerFlagModel.java:32-35`):
///
/// ```text
/// flag.xRot = (-0.0125 + 0.01 * cos(2*PI*phase)) * PI
/// ```
///
/// A single per-part rotation, not per-vertex cloth animation — see
/// `docs/banner-shield-patterns.md`'s "Steps D–F" section for why an earlier
/// pass through that doc wrongly assumed the latter.
#[must_use]
pub fn banner_flag_x_rot(phase: f32) -> f32 {
    (-0.0125 + 0.01 * (2.0 * std::f32::consts::PI * phase).cos()) * std::f32::consts::PI
}

/// A CPU block-entity mesh: part-local vertices plus the part hierarchy needed
/// to rebuild transforms with per-part overrides each frame.
///
/// The hierarchy is kept here rather than in a [`crate::entity_anim::Skeleton`]
/// because `Skeleton` animates by *slot* — `head`, `right_arm`, the limb table —
/// and classifies anything without those names as `AnimFamily::Static`. A chest
/// is `Static` by that rule and would pose with a permanently shut lid. What a
/// block entity needs instead is a direct per-part pose override, which is a
/// different (and much smaller) mechanism, so it lives here.
#[derive(Debug, Clone)]
pub struct BlockEntityMesh {
    /// Four vertices per quad, part-local (no pose folded in).
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad, wound from each quad's baked outward normal.
    pub indices: Vec<u32>,
    /// One index sub-range per part, in bake (pre-order) order.
    pub parts: Vec<PartRange>,
    /// Part names, parallel to `parts`.
    pub part_names: Vec<String>,
    /// Parent index per part (`None` for the root); always less than the part's
    /// own index, so one forward pass composes the chain.
    pub part_parents: Vec<Option<usize>>,
    /// The authored pose per part; an override copies and adjusts it.
    pub part_rest: Vec<PartPose>,
    /// Local AABB minimum at rest, in block units.
    pub local_min: Vec3,
    /// Local AABB maximum at rest, in block units.
    pub local_max: Vec3,
}

impl BlockEntityMesh {
    /// Bakes a model definition into a renderable block-entity mesh.
    #[must_use]
    pub fn from_model(def: &EntityModelDef) -> Self {
        let baked = bake_entity_parts(def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::with_capacity(baked.len());
        let mut part_names = Vec::with_capacity(baked.len());
        let mut part_parents = Vec::with_capacity(baked.len());
        let mut part_rest = Vec::with_capacity(baked.len());

        for part in &baked {
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push(PartRange {
                index_start,
                index_count: indices.len() as u32 - index_start,
                vertex_start,
                vertex_count: vertices.len() as u32 - vertex_start,
            });
            part_names.push(part.name.clone());
            part_parents.push(part.parent);
            part_rest.push(part.rest);
        }

        let mut mesh = BlockEntityMesh {
            vertices,
            indices,
            parts,
            part_names,
            part_parents,
            part_rest,
            local_min: Vec3::ZERO,
            local_max: Vec3::ZERO,
        };
        // The rest AABB, measured through the same transform chain the draw uses
        // (`part_transforms` with no overrides) rather than from the texel
        // extents restated by hand.
        let rest = mesh.part_transforms(Mat4::IDENTITY, &[]);
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for (part_index, part) in baked.iter().enumerate() {
            for quad in &part.quads {
                for p in &quad.positions {
                    let posed = rest[part_index].transform_point3(Vec3::from(*p));
                    min = min.min(posed);
                    max = max.max(posed);
                }
            }
        }
        if mesh.indices.is_empty() {
            min = Vec3::ZERO;
            max = Vec3::ZERO;
        }
        mesh.local_min = min;
        mesh.local_max = max;
        mesh
    }

    /// The index of a part by name.
    #[must_use]
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.part_names.iter().position(|n| n == name)
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }

    /// Composes one world matrix per part: `placement · chain(parent) · pose`,
    /// with `overrides` replacing the authored pose of the parts it names.
    ///
    /// `overrides` is `(part index, pose)`; a part named twice takes the last
    /// entry. The chain uses [`lodestone_assets::entity::Affine::of_pose`] rather
    /// than a local `rotationZYX`, so the rotation order can never drift from
    /// the one the bake itself used — a second implementation of `rotZYX` is
    /// exactly how a lid ends up hinging about the wrong axis with every unit
    /// test still green.
    #[must_use]
    pub fn part_transforms(&self, placement: Mat4, overrides: &[(usize, PartPose)]) -> Vec<Mat4> {
        use lodestone_assets::entity::Affine;
        let mut poses = self.part_rest.clone();
        for (index, pose) in overrides {
            if let Some(slot) = poses.get_mut(*index) {
                *slot = *pose;
            }
        }
        let mut chain: Vec<Affine> = Vec::with_capacity(poses.len());
        for (index, pose) in poses.iter().enumerate() {
            let local = Affine::of_pose(pose);
            let world = match self.part_parents[index] {
                // `parent < index` is guaranteed by `bake_entity_parts`'
                // pre-order, so the parent's composed transform already exists.
                Some(parent) => chain[parent].compose(&local),
                None => local,
            };
            chain.push(world);
        }
        chain
            .into_iter()
            .map(|a| placement * affine_to_mat4(&a))
            .collect()
    }
}

/// Widens an [`Affine`](lodestone_assets::entity::Affine) (row-major 3×3 plus a
/// translation) into a column-major [`Mat4`].
///
/// `Affine::m[i][j]` is *row* `i`, *column* `j`; `Mat4::from_cols_array_2d`
/// takes **columns**. The transpose here is the whole point — feeding the rows
/// in as columns yields the inverse rotation, which for a chest lid looks like
/// the lid opening *into* the chest and is easy to mistake for a sign error in
/// `chest_lid_x_rot`.
fn affine_to_mat4(a: &lodestone_assets::entity::Affine) -> Mat4 {
    Mat4::from_cols_array_2d(&[
        [a.m[0][0], a.m[1][0], a.m[2][0], 0.0],
        [a.m[0][1], a.m[1][1], a.m[2][1], 0.0],
        [a.m[0][2], a.m[1][2], a.m[2][2], 0.0],
        [a.t[0], a.t[1], a.t[2], 1.0],
    ])
}

/// Model name of the bell body/rim rig.
pub const BELL: &str = "bell";

/// The jar sheet a bell draws with — `BellRenderer.BELL_TEXTURE`
/// (`Sheets.BLOCK_ENTITIES_MAPPER.defaultNamespaceApply("bell/bell_body")`).
/// Single stem, no material variants: unlike chest, a bell's sheet never
/// changes with block state or NBT.
pub const BELL_TEXTURE_STEM: &str = "entity/bell/bell_body";

/// The one bell sheet stem, for [`block_entity_texture_stems`] — mirrors
/// [`skull_texture_stems`]'s shape even though there is only one entry, so a
/// future material split (there is none today) has one function to widen
/// rather than a call site to find.
#[must_use]
pub fn bell_texture_stems() -> Vec<&'static str> {
    vec![BELL_TEXTURE_STEM]
}

/// A bell's shake direction — `BellModel.State.shakeDirection`, the four
/// horizontal directions a player (or projectile) can hit a bell from.
/// `Option<BellShakeDirection>` (not a fifth "none" variant) mirrors the
/// jar's own `@Nullable Direction`, and matches how [`BellSpawn::shake`]
/// spells "at rest".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BellShakeDirection {
    /// `Direction.NORTH`.
    North,
    /// `Direction.SOUTH`.
    South,
    /// `Direction.EAST`.
    East,
    /// `Direction.WEST`.
    West,
}

/// The bell body's `(x_rot, z_rot)` in radians for a shake in progress —
/// `BellModel.setupAnim`:
///
/// ```text
/// baseRot = sin(ticks / PI) / (4 + ticks / 3)
/// NORTH: xRot = -baseRot   SOUTH: xRot = +baseRot
/// EAST:  zRot = -baseRot   WEST:  zRot = +baseRot
/// ```
///
/// `direction = None` returns `(0.0, 0.0)` without evaluating `base_rot` at
/// all, mirroring `BellModel.setupAnim`'s own `if (state.shakeDirection !=
/// null)` guard rather than computing the ratio and multiplying by a zero
/// that never appears in the real formula — there is no direction to carry a
/// sign for a bell at rest, so a literal port has nothing to multiply.
///
/// `ticks` is vanilla's raw tick counter (`BellBlockEntity.ticks`, `0..50`,
/// **not** eased or clamped here) plus partial tick, exactly as
/// `BellRenderer.extractRenderState` passes it — unlike
/// [`chest_lid_openness`], there is only one transform here because vanilla
/// itself has only one; `setupAnim` computes the angle directly from ticks
/// with no separate easing pass.
#[must_use]
pub fn bell_shake_angle(direction: Option<BellShakeDirection>, ticks: f32) -> (f32, f32) {
    let Some(direction) = direction else {
        return (0.0, 0.0);
    };
    let base_rot = (ticks / std::f32::consts::PI).sin() / (4.0 + ticks / 3.0);
    match direction {
        BellShakeDirection::North => (-base_rot, 0.0),
        BellShakeDirection::South => (base_rot, 0.0),
        BellShakeDirection::East => (0.0, -base_rot),
        BellShakeDirection::West => (0.0, base_rot),
    }
}

/// Model name of a standing banner's pole+bar body.
pub const BANNER_BODY: &str = "banner_body";
/// Model name of a standing banner's flag.
pub const BANNER_FLAG: &str = "banner_flag";
/// Model name of a **wall** banner's bar — `createBodyLayer(false)`, which has no
/// pole at all.
pub const BANNER_WALL_BODY: &str = "banner_wall_body";
/// Model name of a **wall** banner's flag — the same cube as [`BANNER_FLAG`]'s at
/// a different rest pose.
pub const BANNER_WALL_FLAG: &str = "banner_wall_flag";

/// How a banner is attached to the world, and therefore which pair of meshes and
/// which placement angle it uses.
///
/// The same shape as [`SkullOrientation`], and for the same reason: the two forms
/// carry *different data* (a 16-way rotation segment against a four-way facing),
/// so a shared `angle: f32` field would let a caller hand a wall banner a segment
/// and get a plausible eighth-turn error. Unlike a skull, both forms here also
/// select a different **mesh**, since `createBodyLayer(false)` drops the pole.
///
/// No `Eq`/`Hash`, unlike [`SkullOrientation`]: the wall arm carries a yaw as an
/// `f32`, matching every other placement input in this module rather than
/// re-encoding four directions as an enum only this type would use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BannerAttachment {
    /// A standing banner: `BannerBlock`, with the `ROTATION` property.
    Ground {
        /// The `ROTATION` block-state property, `0..16` — vanilla's
        /// `RotationSegment`, segment `0` being north.
        rotation_segment: u8,
    },
    /// A wall banner: `WallBannerBlock`, with `FACING`.
    Wall {
        /// `Direction.toYRot()` of `WallBannerBlock.FACING` (south `0`, west `90`,
        /// north `180`, east `270`) — [`horizontal_facing_yaw`]'s convention.
        facing_yaw_deg: f32,
    },
}

/// The jar sheet a banner's *body* (pole/bar) and *flag* both draw with for
/// their opaque pass — `Sheets.BANNER_BASE` (`Sheets.java:52`), i.e.
/// `Sheets.BANNER_MAPPER.defaultNamespaceApply("banner_base")` ->
/// `entity/banner/banner_base`. This is the plain wood/cloth texture, never a
/// pattern mask: `BannerRenderer.submitBanner` passes this one `SpriteId` to
/// *both* `submitModel` calls (model and flagModel) before `submitPatterns`
/// draws anything coloured over the flag a second time.
pub const BANNER_BASE_TEXTURE_STEM: &str = "entity/banner/banner_base";

/// The one banner sheet stem, for [`block_entity_texture_stems`] — mirrors
/// [`bell_texture_stems`]'s shape: one stem shared by all four banner models
/// (standing and wall, body and flag), because `submitBanner` passes the same
/// `Sheets.BANNER_BASE` to every one of them.
#[must_use]
pub fn banner_texture_stems() -> Vec<&'static str> {
    vec![BANNER_BASE_TEXTURE_STEM]
}

/// Model name of a shield's `plate`+`handle` mesh (`lodestone_assets::
/// block_entity_models::shield_model`) — see that function's doc for why a
/// shield, unlike a banner, has only one mesh, reused for every draw.
pub const SHIELD: &str = "shield";

/// `Sheets.SHIELD_BASE_NO_PATTERN` — `entity/shield/shield_base_nopattern`,
/// the plain wood-and-iron sheet every shield with no `minecraft:base_color`
/// and no stored `minecraft:banner_patterns` layer draws (the common case,
/// straight off a crafting table).
pub const SHIELD_BASE_NO_PATTERN_TEXTURE_STEM: &str = "entity/shield/shield_base_nopattern";

/// `Sheets.SHIELD_BASE` — `entity/shield/shield_base`, the sheet a shield
/// with a base colour or at least one loom pattern draws its **opaque** pass
/// with (a plain grey canvas the translucent pattern layers tint and mask
/// over, mirroring [`BANNER_BASE_TEXTURE_STEM`]'s role for a banner).
pub const SHIELD_BASE_TEXTURE_STEM: &str = "entity/shield/shield_base";

/// Whether a shield stack's translucent pattern pass draws at all —
/// vanilla's own `hasPatterns` (`ShieldSpecialRenderer.submit`): `true` when
/// the stack carries a `minecraft:base_color` **or** at least one stored
/// `minecraft:banner_patterns` layer. `false` is the common case (a shield
/// straight off a crafting table), which draws only the opaque
/// [`SHIELD_BASE_NO_PATTERN_TEXTURE_STEM`] sheet and nothing translucent —
/// [`shield_pattern_layers`](crate::banner_pattern::shield_pattern_layers)
/// is never even called for it, matching vanilla's own early-out rather than
/// calling it and discarding an always-present base mask the way a banner's
/// own `layers` list (never empty) does.
#[must_use]
pub fn shield_has_patterns(base_color: Option<&str>, pattern_count: usize) -> bool {
    base_color.is_some() || pattern_count > 0
}

/// `(model, texture)` for a shield item's **opaque** base draw — the one
/// `plate`+`handle` mesh ([`SHIELD`]), textured by whether the stack has
/// anything translucent to draw over it. Shared by every surface that draws
/// a shield (the first-person hand, the GUI icon) the same way
/// [`banner_item_rig`] is shared by a banner's two surfaces — see this
/// module's shield section for why there is no second mesh the way a
/// banner's `flag` is.
#[must_use]
pub fn shield_item_rig(has_patterns: bool) -> (&'static str, &'static str) {
    let texture = if has_patterns {
        SHIELD_BASE_TEXTURE_STEM
    } else {
        SHIELD_BASE_NO_PATTERN_TEXTURE_STEM
    };
    (SHIELD, texture)
}

/// Both sheets a shield can draw its opaque pass with, for
/// [`block_entity_texture_stems`]'s preload list — unlike
/// [`banner_texture_stems`]'s single stem, a shield's *runtime* state (has a
/// base colour or a pattern, or not) picks between two, so both must be
/// preloaded rather than only the default [`BlockEntityModelEntry::texture`]
/// a gate with no item state draws.
#[must_use]
pub fn shield_texture_stems() -> Vec<&'static str> {
    vec![SHIELD_BASE_TEXTURE_STEM, SHIELD_BASE_NO_PATTERN_TEXTURE_STEM]
}

/// Model name of a shulker box's shell (lid + base).
pub const SHULKER_BOX: &str = "shulker_box";

/// Vanilla's sixteen dye colours, in `DyeColor` **ordinal** order — which is
/// what `Sheets.getShulkerBoxSprite(color)` indexes
/// (`SHULKER_TEXTURE_LOCATION.get(color.getId())`, `Sheets.java:48,89`).
///
/// The order is load-bearing and is *not* alphabetical: reading it off the
/// texture directory listing gives `black, blue, brown, …` and shifts every
/// coloured box one sprite along, which draws a plausible wrong colour rather
/// than nothing.
pub const SHULKER_COLOURS: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

/// `Sheets.DEFAULT_SHULKER_TEXTURE_LOCATION` — the undyed box's sheet
/// (`Sheets.SHULKER_MAPPER.defaultNamespaceApply("shulker")`, `Sheets.java:47`).
pub const SHULKER_DEFAULT_TEXTURE_STEM: &str = "entity/shulker/shulker";

/// The sheet stem for one shulker box, by dye colour name, or the undyed sheet
/// for `None` — `ShulkerBoxRenderer.submit`'s own `color == null` fork.
///
/// An **unrecognised** colour name also falls back to the undyed sheet rather
/// than being dropped: the caller derives it from a block id, and a plain
/// `shulker_box` (the uncoloured one) has no colour segment at all.
#[must_use]
pub fn shulker_texture_stem(colour: Option<&str>) -> &'static str {
    let Some(colour) = colour else {
        return SHULKER_DEFAULT_TEXTURE_STEM;
    };
    for name in SHULKER_COLOURS {
        if name == colour {
            return shulker_coloured_stem(name);
        }
    }
    SHULKER_DEFAULT_TEXTURE_STEM
}

/// `entity/shulker/shulker_<colour>` for one of [`SHULKER_COLOURS`].
///
/// A `match` rather than a `format!` because the return is `&'static str`: these
/// stems key the shell's preloaded bind-group map, so an owned `String` here
/// would mean an allocation per box per frame.
fn shulker_coloured_stem(colour: &str) -> &'static str {
    match colour {
        "white" => "entity/shulker/shulker_white",
        "orange" => "entity/shulker/shulker_orange",
        "magenta" => "entity/shulker/shulker_magenta",
        "light_blue" => "entity/shulker/shulker_light_blue",
        "yellow" => "entity/shulker/shulker_yellow",
        "lime" => "entity/shulker/shulker_lime",
        "pink" => "entity/shulker/shulker_pink",
        "gray" => "entity/shulker/shulker_gray",
        "light_gray" => "entity/shulker/shulker_light_gray",
        "cyan" => "entity/shulker/shulker_cyan",
        "purple" => "entity/shulker/shulker_purple",
        "blue" => "entity/shulker/shulker_blue",
        "brown" => "entity/shulker/shulker_brown",
        "green" => "entity/shulker/shulker_green",
        "red" => "entity/shulker/shulker_red",
        "black" => "entity/shulker/shulker_black",
        _ => SHULKER_DEFAULT_TEXTURE_STEM,
    }
}

/// All seventeen shulker sheet stems — the undyed one plus one per dye colour.
///
/// Unlike [`bell_texture_stems`] this really does have variants, and they are
/// picked by *block id* rather than by NBT (`minecraft:red_shulker_box` is its
/// own block), which is why [`shulker_texture_stem`] takes a colour name and not
/// a `DyeColor`-shaped enum.
#[must_use]
pub fn shulker_texture_stems() -> Vec<&'static str> {
    let mut stems = vec![SHULKER_DEFAULT_TEXTURE_STEM];
    stems.extend(SHULKER_COLOURS.map(shulker_coloured_stem));
    stems
}

/// The world placement transform for a shulker box facing `facing` —
/// `ShulkerBoxRenderer.createModelTransform`
/// (`ShulkerBoxRenderer.java:110-121`):
///
/// ```text
/// translation(0.5, 0.5, 0.5) · scale(0.9995) · rotate(facing.getRotation())
///   · scale(1, -1, -1) · translate(0, -1, 0)
/// ```
///
/// **This is not [`block_entity_placement_matrix`] with a yaw.** Three things
/// differ and each is visible: the pivot is the block's *centre* `(0.5, 0.5,
/// 0.5)` rather than its floor `(0.5, 0, 0.5)`; a shulker box can face **up or
/// down**, so the rotation is a full `Direction.getRotation()` quaternion and not
/// a Y yaw; and it carries the `scale(1, -1, -1)` entity flip and the `-1` lift
/// that `SkullBlockRenderer` also has and `ChestRenderer` does not. Reusing the
/// chest matrix draws an upside-down box on the floor for `facing=up`, which is
/// the common case.
///
/// The `0.9995` shrink is vanilla's own z-fighting guard against a neighbouring
/// full block, not a rounding artefact — keep it.
#[must_use]
pub fn shulker_placement_matrix(pos: [i32; 3], facing: ShulkerFacing) -> Mat4 {
    const SHRINK: f32 = 0.9995;
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    Mat4::from_translation(origin + Vec3::splat(0.5))
        * Mat4::from_scale(Vec3::splat(SHRINK))
        * facing.rotation()
        * Mat4::from_scale(Vec3::new(1.0, -1.0, -1.0))
        * Mat4::from_translation(Vec3::new(0.0, -1.0, 0.0))
}

/// Which face a shulker box's lid opens toward — `ShulkerBoxBlock.FACING`, one
/// of all six directions rather than the four horizontals a chest has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ShulkerFacing {
    /// `Direction.UP`, and `ShulkerBoxRenderer`'s own
    /// `getValueOrElse(FACING, Direction.UP)` default.
    #[default]
    Up,
    /// `Direction.DOWN`.
    Down,
    /// `Direction.NORTH`.
    North,
    /// `Direction.SOUTH`.
    South,
    /// `Direction.WEST`.
    West,
    /// `Direction.EAST`.
    East,
}

impl ShulkerFacing {
    /// `Direction.getRotation()` (`Direction.java:144-153`) as a rotation matrix.
    ///
    /// `rotationXYZ(x, y, z)` is JOML's **X then Y then Z** intrinsic order, which
    /// for the four horizontals here is `Mat4::from_rotation_z * from_rotation_x`
    /// — the Z term is applied last. Composing them the other way round rotates a
    /// wall-mounted box about the wrong axis, and the result still looks like a
    /// box, so this is the line to check first if a side-placed shulker is wrong.
    #[must_use]
    pub fn rotation(self) -> Mat4 {
        use std::f32::consts::{FRAC_PI_2, PI};
        match self {
            ShulkerFacing::Up => Mat4::IDENTITY,
            ShulkerFacing::Down => Mat4::from_rotation_x(PI),
            ShulkerFacing::North => Mat4::from_rotation_z(PI) * Mat4::from_rotation_x(FRAC_PI_2),
            ShulkerFacing::South => Mat4::from_rotation_x(FRAC_PI_2),
            ShulkerFacing::West => {
                Mat4::from_rotation_z(FRAC_PI_2) * Mat4::from_rotation_x(FRAC_PI_2)
            }
            ShulkerFacing::East => {
                Mat4::from_rotation_z(-FRAC_PI_2) * Mat4::from_rotation_x(FRAC_PI_2)
            }
        }
    }

    /// One of vanilla's six `Direction` names, or `None`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "up" => ShulkerFacing::Up,
            "down" => ShulkerFacing::Down,
            "north" => ShulkerFacing::North,
            "south" => ShulkerFacing::South,
            "west" => ShulkerFacing::West,
            "east" => ShulkerFacing::East,
            _ => return None,
        })
    }
}

/// The lid's `(y_offset, y_rot)` for an open fraction —
/// `ShulkerBoxRenderer.ShulkerBoxModel.setupAnim` (`:135-138`):
/// `lid.setPos(0, 24 - progress * 0.5 * 16, 0)` and
/// `lid.yRot = 270° * progress`.
///
/// `24.0` is the part's rest `y`, so the returned offset is absolute and not a
/// delta. `progress == 0` gives exactly the rest pose, which is why a closed box
/// needs no override at all.
#[must_use]
pub fn shulker_lid_pose(progress: f32) -> (f32, f32) {
    let progress = progress.clamp(0.0, 1.0);
    (
        24.0 - progress * 0.5 * 16.0,
        (270.0 * progress).to_radians(),
    )
}

/// Model name of the decorated pot's base (neck + top + bottom) — one
/// texture (`decorated_pot_base`) for every pot in the world, regardless of
/// its stored sherds.
pub const DECORATED_POT_BASE: &str = "decorated_pot_base";

/// Model name of the pot's front side quad — see
/// `lodestone_assets::block_entity_models::decorated_pot_side_part`'s doc for
/// why this is a distinct model from the other three sides rather than one
/// model reused with an override.
pub const DECORATED_POT_SIDE_FRONT: &str = "decorated_pot_side_front";
/// Model name of the pot's back side quad.
pub const DECORATED_POT_SIDE_BACK: &str = "decorated_pot_side_back";
/// Model name of the pot's left side quad.
pub const DECORATED_POT_SIDE_LEFT: &str = "decorated_pot_side_left";
/// Model name of the pot's right side quad.
pub const DECORATED_POT_SIDE_RIGHT: &str = "decorated_pot_side_right";

/// The jar sheet the pot's base always draws with —
/// `Sheets.DECORATED_POT_BASE` (`entity/decorated_pot/decorated_pot_base`).
pub const DECORATED_POT_BASE_TEXTURE_STEM: &str = "entity/decorated_pot/decorated_pot_base";

/// The jar sheet an undecorated side draws with —
/// `Sheets.DECORATED_POT_SIDE` (`entity/decorated_pot/decorated_pot_side`),
/// `DecoratedPotRenderer.getSideSprite`'s fallback for an absent or
/// unrecognised sherd.
pub const DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM: &str =
    "entity/decorated_pot/decorated_pot_side";

/// A stored sherd's item path (namespace stripped, e.g.
/// `"angler_pottery_sherd"`) to the sherd's own pattern texture stem —
/// `DecoratedPotPatterns.itemToPatternMappings` plus
/// `DecoratedPotPattern.assetId()`/`Sheets.DECORATED_POT_MAPPER`, transcribed
/// from `.cache/mc/26.2/client-src/net/minecraft/world/level/block/entity/
/// DecoratedPotPatterns.java`'s own `bootstrap` — the pattern's registered
/// asset id, not a guess from the sherd's own name (they happen to share a
/// `<name>_pottery_pattern`/`<name>_pottery_sherd` stem, but that is a jar
/// convention this reads off the real registration, not an assumption).
///
/// `None` (an absent side, or an item path this table does not recognise —
/// vanilla's own `getSideSprite` falls back identically for a `null` map
/// lookup) maps to [`DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM`] by the caller,
/// not here, so this function's contract stays "sherd path in, pattern stem
/// out" with no third case to remember at the call site.
#[must_use]
pub fn decorated_pot_pattern_texture_stem(sherd_item_path: &str) -> Option<&'static str> {
    Some(match sherd_item_path {
        "angler_pottery_sherd" => "entity/decorated_pot/angler_pottery_pattern",
        "archer_pottery_sherd" => "entity/decorated_pot/archer_pottery_pattern",
        "arms_up_pottery_sherd" => "entity/decorated_pot/arms_up_pottery_pattern",
        "blade_pottery_sherd" => "entity/decorated_pot/blade_pottery_pattern",
        "brewer_pottery_sherd" => "entity/decorated_pot/brewer_pottery_pattern",
        "burn_pottery_sherd" => "entity/decorated_pot/burn_pottery_pattern",
        "danger_pottery_sherd" => "entity/decorated_pot/danger_pottery_pattern",
        "explorer_pottery_sherd" => "entity/decorated_pot/explorer_pottery_pattern",
        "flow_pottery_sherd" => "entity/decorated_pot/flow_pottery_pattern",
        "friend_pottery_sherd" => "entity/decorated_pot/friend_pottery_pattern",
        "guster_pottery_sherd" => "entity/decorated_pot/guster_pottery_pattern",
        "heart_pottery_sherd" => "entity/decorated_pot/heart_pottery_pattern",
        "heartbreak_pottery_sherd" => "entity/decorated_pot/heartbreak_pottery_pattern",
        "howl_pottery_sherd" => "entity/decorated_pot/howl_pottery_pattern",
        "miner_pottery_sherd" => "entity/decorated_pot/miner_pottery_pattern",
        "mourner_pottery_sherd" => "entity/decorated_pot/mourner_pottery_pattern",
        "plenty_pottery_sherd" => "entity/decorated_pot/plenty_pottery_pattern",
        "prize_pottery_sherd" => "entity/decorated_pot/prize_pottery_pattern",
        "scrape_pottery_sherd" => "entity/decorated_pot/scrape_pottery_pattern",
        "sheaf_pottery_sherd" => "entity/decorated_pot/sheaf_pottery_pattern",
        "shelter_pottery_sherd" => "entity/decorated_pot/shelter_pottery_pattern",
        "skull_pottery_sherd" => "entity/decorated_pot/skull_pottery_pattern",
        "snort_pottery_sherd" => "entity/decorated_pot/snort_pottery_pattern",
        _ => return None,
    })
}

/// Every decorated-pot sheet: the base, the undecorated-side default, and
/// all twenty-three sherd patterns — the union [`block_entity_texture_stems`]
/// folds in, so a pot with any combination of sherds always finds a bind
/// group.
#[must_use]
pub fn decorated_pot_texture_stems() -> Vec<&'static str> {
    let mut stems = vec![
        DECORATED_POT_BASE_TEXTURE_STEM,
        DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM,
    ];
    for sherd in [
        "angler_pottery_sherd",
        "archer_pottery_sherd",
        "arms_up_pottery_sherd",
        "blade_pottery_sherd",
        "brewer_pottery_sherd",
        "burn_pottery_sherd",
        "danger_pottery_sherd",
        "explorer_pottery_sherd",
        "flow_pottery_sherd",
        "friend_pottery_sherd",
        "guster_pottery_sherd",
        "heart_pottery_sherd",
        "heartbreak_pottery_sherd",
        "howl_pottery_sherd",
        "miner_pottery_sherd",
        "mourner_pottery_sherd",
        "plenty_pottery_sherd",
        "prize_pottery_sherd",
        "scrape_pottery_sherd",
        "sheaf_pottery_sherd",
        "shelter_pottery_sherd",
        "skull_pottery_sherd",
        "snort_pottery_sherd",
    ] {
        if let Some(stem) = decorated_pot_pattern_texture_stem(sherd) {
            stems.push(stem);
        }
    }
    stems
}

/// The world placement transform for a decorated pot at `pos` facing
/// `facing_yaw_deg` — `DecoratedPotRenderer.createModelTransformation`:
/// `rotateAround(Axis.YP.rotationDegrees(180.0F - entityDirection.toYRot()),
/// 0.5F, 0.5F, 0.5F)`.
///
/// **Not [`block_entity_placement_matrix`] with a yaw**: the pivot is the
/// block's *centre* (`0.5, 0.5, 0.5`, like [`shulker_placement_matrix`]'s,
/// not chest's floor pivot) and the angle carries an extra `180°` term chest
/// does not. Reusing the chest matrix draws every pot rotated a half-turn
/// from its real facing, which still looks like a plausible pot.
#[must_use]
pub fn decorated_pot_placement_matrix(pos: [i32; 3], facing_yaw_deg: f32) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    let pivot = Vec3::splat(0.5);
    Mat4::from_translation(origin + pivot)
        * Mat4::from_rotation_y((180.0 - facing_yaw_deg).to_radians())
        * Mat4::from_translation(-pivot)
}

/// The version-free description of one decorated pot to draw this frame.
///
/// The caller owns every field: `HORIZONTAL_FACING` off the block state →
/// `facing_yaw_deg` (the same [`horizontal_facing_yaw`] convention chest
/// uses); the block entity's own NBT `"sherds"` list, namespace-stripped and
/// ordered `[back, left, right, front]` per `PotDecorations.CODEC` → the four
/// `Option<String>` fields (`None` for an absent side — vanilla's own
/// `PotDecorations` maps a stored `minecraft:brick` to absent at parse time,
/// so a caller reading raw NBT should do the same rather than passing
/// `Some("brick")` through); world light → `light`.
///
/// No wobble phase: `DecoratedPotBlockEntity`'s hit-wobble is a `BLOCK_EVENT`
/// animation with no producer in this workspace yet, the same
/// not-yet-triggered gap [`ShulkerSpawn::progress`] documents for the lid.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DecoratedPotSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// `Direction.toYRot()` for the block's `HORIZONTAL_FACING`.
    pub facing_yaw_deg: f32,
    /// The front side's stored sherd item path, or `None`.
    pub front: Option<String>,
    /// The back side's stored sherd item path, or `None`.
    pub back: Option<String>,
    /// The left side's stored sherd item path, or `None`.
    pub left: Option<String>,
    /// The right side's stored sherd item path, or `None`.
    pub right: Option<String>,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl DecoratedPotSpawn {
    /// A south-facing, undecorated, full-bright pot at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        DecoratedPotSpawn {
            pos,
            facing_yaw_deg: 0.0,
            light: ENTITY_FULLBRIGHT,
            ..Default::default()
        }
    }
}

/// Model name of the conduit's inactive shell — `ConduitRenderer.shell`
/// (`ModelLayers.CONDUIT_SHELL`), the 6×6×6 layer.
pub const CONDUIT_SHELL: &str = "conduit_shell";

/// Model name of the conduit's active shell ("cage") — `ConduitRenderer.cage`
/// (`ModelLayers.CONDUIT_CAGE`), the 8×8×8 layer. A distinct mesh from
/// [`CONDUIT_SHELL`], not a scaled reuse — see
/// [`lodestone_assets::block_entity_models::conduit_cage_model`]'s doc.
pub const CONDUIT_CAGE: &str = "conduit_cage";

/// Model name of the conduit's wind plane — `ConduitRenderer.wind`
/// (`ModelLayers.CONDUIT_WIND`), one 16×16×16 cube drawn twice per active
/// frame at two different poses sharing this one mesh.
pub const CONDUIT_WIND: &str = "conduit_wind";

/// Model name of the conduit's billboarded eye — `ConduitRenderer.eye`
/// (`ModelLayers.CONDUIT_EYE`), the near-planar 8×8 box.
pub const CONDUIT_EYE: &str = "conduit_eye";

/// `ConduitRenderer.SHELL_TEXTURE` — `entity/conduit/base`, the inactive shell's
/// sheet.
pub const CONDUIT_SHELL_TEXTURE_STEM: &str = "entity/conduit/base";

/// `ConduitRenderer.ACTIVE_SHELL_TEXTURE` — `entity/conduit/cage`, the active
/// cage's sheet.
pub const CONDUIT_CAGE_TEXTURE_STEM: &str = "entity/conduit/cage";

/// `ConduitRenderer.WIND_TEXTURE` — `entity/conduit/wind`, used for both wind
/// planes whenever [`ConduitSpawn::animation_phase`] is not `1`.
pub const CONDUIT_WIND_TEXTURE_STEM: &str = "entity/conduit/wind";

/// `ConduitRenderer.VERTICAL_WIND_TEXTURE` — `entity/conduit/wind_vertical`,
/// used for both wind planes when [`ConduitSpawn::animation_phase`] **is**
/// `1`. Same UV layout as [`CONDUIT_WIND_TEXTURE_STEM`], different sheet —
/// picking the texture without also picking the plane's own rotation
/// (`wind1_rot`'s `1 =>` arm) draws a plane that spins but never actually
/// turns to face the direction its "vertical" sheet implies.
pub const CONDUIT_WIND_VERTICAL_TEXTURE_STEM: &str = "entity/conduit/wind_vertical";

/// `ConduitRenderer.OPEN_EYE_TEXTURE` — `entity/conduit/open_eye`, drawn while
/// [`ConduitSpawn::hunting`].
pub const CONDUIT_OPEN_EYE_TEXTURE_STEM: &str = "entity/conduit/open_eye";

/// `ConduitRenderer.CLOSED_EYE_TEXTURE` — `entity/conduit/closed_eye`, drawn
/// otherwise (including the entire inactive branch, which never submits an
/// eye instance at all).
pub const CONDUIT_CLOSED_EYE_TEXTURE_STEM: &str = "entity/conduit/closed_eye";

/// Every conduit sheet, for [`block_entity_texture_stems`]. **Excludes**
/// [`CONDUIT_WIND_TEXTURE_STEM`]/[`CONDUIT_WIND_VERTICAL_TEXTURE_STEM`]'s
/// remaining 21 animation frames — the jar ships each as a `64×704` vertical
/// strip (22 frames at `64×32`, `frametime: 3`, no `interpolate`) and the
/// shell's block-entity loader crops each to its first frame only. See
/// `crates/lodestone-shell/src/gpu/block_entities.rs`'s conduit loading note
/// for why: this pass has no per-material animation uniform the way the block
/// atlas does, and building one is out of scope here. The wind planes are
/// therefore correct in shape, rotation and texture *choice*, and static
/// rather than flowing — a documented simplification, not a silent bug.
#[must_use]
pub fn conduit_texture_stems() -> Vec<&'static str> {
    vec![
        CONDUIT_SHELL_TEXTURE_STEM,
        CONDUIT_CAGE_TEXTURE_STEM,
        CONDUIT_WIND_TEXTURE_STEM,
        CONDUIT_WIND_VERTICAL_TEXTURE_STEM,
        CONDUIT_OPEN_EYE_TEXTURE_STEM,
        CONDUIT_CLOSED_EYE_TEXTURE_STEM,
    ]
}

/// One conduit's fully-resolved per-frame animation/activation state — the
/// output of [`conduit_frame_scan`] plus a per-instance tick tracker folded
/// through [`conduit_advance`], [`conduit_active_rotation_value`],
/// [`conduit_anim_time`] and [`conduit_animation_phase`] by the caller. This
/// struct carries already-resolved numbers, not a live clock or a block
/// store — the same split [`BellSpawn::shake`] makes from `BellShakes`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConduitSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// `ConduitBlockEntity.isActive()` — [`ConduitFrame::is_active`].
    pub active: bool,
    /// `ConduitBlockEntity.isHunting()` — [`ConduitFrame::is_hunting`]. Only
    /// meaningful (and only ever read) while `active`; vanilla's own
    /// `state.isHunting` can theoretically be true while `state.isActive` is
    /// false for one frame of hysteresis, but the inactive branch never reads
    /// it, so [`Self::resolve_conduit`]/[`BlockEntityModelSet::resolve_conduit`]
    /// does not either.
    pub hunting: bool,
    /// [`conduit_active_rotation_value`]'s output — see that function's doc
    /// for why the same number is read as degrees in one branch and radians
    /// in the other.
    pub active_rotation_value: f32,
    /// [`conduit_anim_time`] — `tickCount + partialTick`, feeds
    /// [`conduit_bob`].
    pub anim_time: f32,
    /// [`conduit_animation_phase`] — `0`, `1` or `2`.
    pub animation_phase: u8,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl Default for ConduitSpawn {
    fn default() -> Self {
        ConduitSpawn {
            pos: [0, 0, 0],
            active: false,
            hunting: false,
            active_rotation_value: 0.0,
            anim_time: 0.0,
            animation_phase: 0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

impl ConduitSpawn {
    /// An inactive, resting, full-bright conduit at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        ConduitSpawn {
            pos,
            ..Default::default()
        }
    }
}

/// Total cells [`conduit_frame_scan`]'s 5×5×5 pass ever tests, regardless of
/// which blocks are placed — `ConduitBlockEntity.MIN_KILL_SIZE`'s own value,
/// which is exactly this count: "hunting" is not a majority threshold, it is
/// *every* candidate cell filled. Checked by
/// `conduit_frame_candidate_count_is_42_and_matches_min_kill_size` as an
/// outside-arithmetic control on the geometry alone, independent of the block
/// predicate.
pub const CONDUIT_FRAME_CANDIDATE_COUNT: u32 = 42;

/// One conduit's activation frame — `ConduitBlockEntity.updateShape`'s
/// `effectBlocks.size()`, plus the two booleans it and `updateHunting` derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConduitFrame {
    /// `effectBlocks.size()`. `0` whenever the inner 3×3×3 was not entirely
    /// water — `updateShape` clears the list and returns before the 5×5×5
    /// pass ever runs in that case, so a zero here does not distinguish "no
    /// water" from "water but zero frame blocks"; nothing downstream needs
    /// to.
    pub effect_block_count: u32,
}

impl ConduitFrame {
    /// `ConduitBlockEntity.MIN_ACTIVE_SIZE` — `updateShape`'s own return
    /// value.
    #[must_use]
    pub fn is_active(self) -> bool {
        self.effect_block_count >= 16
    }

    /// `ConduitBlockEntity.MIN_KILL_SIZE` — `updateHunting`
    /// (`effectBlocks.size() >= 42`). Exactly
    /// [`CONDUIT_FRAME_CANDIDATE_COUNT`], so this requires *every* candidate
    /// cell filled, not a majority.
    #[must_use]
    pub fn is_hunting(self) -> bool {
        self.effect_block_count >= 42
    }
}

/// Scans a conduit's activation frame around `pos` —
/// `ConduitBlockEntity.updateShape`, clause by clause:
///
/// 1. The inner 3×3×3 (`ox,oy,oz` each `-1..=1`, all 27 cells, including the
///    conduit's own position) must be **entirely water** —
///    `Level.isWaterAt(pos)`, which is `getFluidState(pos).is(FluidTags.WATER)`:
///    true for a source, for flowing water, and for a waterlogged block. A
///    single non-water cell anywhere in the inner cube returns an *empty*
///    frame immediately, matching vanilla's early `return false` — so
///    `is_water` is checked over the **whole** inner cube before
///    `is_valid_frame_block` is consulted at all. Skipping this clause (or
///    checking it lazily, cell-by-cell mixed with the outer scan) is exactly
///    the "one implemented conjunct" trap: a room built entirely of
///    prismarine with one stray air pocket in the conduit's own inner cube
///    would activate, and vanilla's would not.
/// 2. Only once (1) holds: scan the 5×5×5 region (`-2..=2` each axis) and, at
///    each of the (up to) [`CONDUIT_FRAME_CANDIDATE_COUNT`] cells on the three
///    axis-aligned "plus" rings — `(ax>1||ay>1||az>1)` *and* one of
///    `ox==0 && (ay==2||az==2)` / `oy==0 && (ax==2||az==2)` /
///    `oz==0 && (ax==2||ay==2)` — count how many hold one of the four frame
///    blocks: `minecraft:prismarine`, `minecraft:prismarine_bricks`,
///    `minecraft:sea_lantern`, `minecraft:dark_prismarine`
///    (`ConduitBlockEntity.VALID_BLOCKS`).
///
/// Both closures receive **absolute** world positions (`pos` already added),
/// so a caller needs no coordinate math of its own — see
/// `crate::block_entities::conduit_spawn` in the shell for the real block-store
/// adapter this is built to be called from.
#[must_use]
pub fn conduit_frame_scan(
    pos: [i32; 3],
    mut is_water: impl FnMut([i32; 3]) -> bool,
    mut is_valid_frame_block: impl FnMut([i32; 3]) -> bool,
) -> ConduitFrame {
    for ox in -1..=1 {
        for oy in -1..=1 {
            for oz in -1..=1 {
                let test = [pos[0] + ox, pos[1] + oy, pos[2] + oz];
                if !is_water(test) {
                    return ConduitFrame::default();
                }
            }
        }
    }

    let mut count = 0u32;
    for ox in -2i32..=2 {
        for oy in -2i32..=2 {
            for oz in -2i32..=2 {
                let (ax, ay, az) = (ox.abs(), oy.abs(), oz.abs());
                let outside_inner = ax > 1 || ay > 1 || az > 1;
                let on_plus_ring = (ox == 0 && (ay == 2 || az == 2))
                    || (oy == 0 && (ax == 2 || az == 2))
                    || (oz == 0 && (ax == 2 || ay == 2));
                if outside_inner
                    && on_plus_ring
                    && is_valid_frame_block([pos[0] + ox, pos[1] + oy, pos[2] + oz])
                {
                    count += 1;
                }
            }
        }
    }
    ConduitFrame {
        effect_block_count: count,
    }
}

/// One client tick's counter bookkeeping for a placed conduit —
/// `ConduitBlockEntity.clientTick`'s non-scan half: `tickCount` always
/// advances by one, `activeRotation` only advances `if (entity.isActive())`.
/// The 40-tick-periodic shape rescan ([`conduit_frame_scan`]) is the caller's
/// job; this only advances the two counters the animation math below reads.
/// Returns `(tick_count, active_rotation_ticks)`.
#[must_use]
pub fn conduit_advance(
    tick_count: u32,
    active_rotation_ticks: u32,
    active: bool,
) -> (u32, u32) {
    let tick_count = tick_count.wrapping_add(1);
    let active_rotation_ticks = if active {
        active_rotation_ticks.wrapping_add(1)
    } else {
        active_rotation_ticks
    };
    (tick_count, active_rotation_ticks)
}

/// `ConduitBlockEntity.getActiveRotation`/`ROTATION_SPEED` (`-0.0375`), folded
/// with the partial tick exactly as `ConduitRenderer.extractRenderState` does:
/// `getActiveRotation(isActive ? partialTicks : 0.0F)` — the partial tick is
/// folded in only while active, so the inactive spin advances one **whole**
/// step per tick with no sub-tick smoothing.
///
/// # The returned number is not one unit — read both call sites before "fixing" this
///
/// This returns the raw `(counter + partial) * -0.0375` vanilla calls
/// `state.activeRotation`, and `ConduitRenderer.submit` reads it two
/// **different** ways depending on which branch runs:
///
/// * **Inactive**: `rotationY(state.activeRotation * (Math.PI / 180.0))` —
///   treated as **degrees**, converted once. See
///   [`conduit_inactive_y_rot_radians`].
/// * **Active**: `float rotation = state.activeRotation * (180.0F /
///   (float)Math.PI); … rotationAxis(rotation * (float)(Math.PI / 180.0),
///   axis)` — multiplied by `180/π` and then immediately back by `π/180`,
///   which is the identity; the two conversions cancel and the axis rotation
///   ends up using the raw value **as radians**, unconverted. See
///   [`conduit_active_axis_rotation_radians`].
///
/// So the same field is degrees in one branch and radians in the other in the
/// jar itself. This is transcribed literally rather than "simplified" to one
/// unit, because simplifying it is exactly how a port gets the active spin's
/// speed wrong by a factor of `180/π` (≈57×) while the inactive spin still
/// looks right — the inactive branch's own conversion masks the bug.
#[must_use]
pub fn conduit_active_rotation_value(
    active_rotation_ticks: u32,
    partial_tick: f32,
    active: bool,
) -> f32 {
    let counter = if active {
        active_rotation_ticks as f32 + partial_tick
    } else {
        active_rotation_ticks as f32
    };
    counter * -0.0375
}

/// Reads [`conduit_active_rotation_value`]'s output as **degrees** and
/// converts — the inactive branch's `rotationY(state.activeRotation * (PI/180))`.
#[must_use]
pub fn conduit_inactive_y_rot_radians(active_rotation_value: f32) -> f32 {
    active_rotation_value.to_radians()
}

/// Reads [`conduit_active_rotation_value`]'s output **directly as radians** —
/// the active branch's `rotation * (PI/180)` after its own `* (180/PI)`,
/// which cancel. See [`conduit_active_rotation_value`]'s doc for the full
/// derivation; this function exists so the cancellation is written down once
/// rather than re-derived (or "simplified away") at every call site.
#[must_use]
pub fn conduit_active_axis_rotation_radians(active_rotation_value: f32) -> f32 {
    active_rotation_value
}

/// `blockEntity.tickCount + partialTicks` — `ConduitRenderer.extractRenderState`'s
/// `state.animTime`. Feeds [`conduit_bob`].
#[must_use]
pub fn conduit_anim_time(tick_count: u32, partial_tick: f32) -> f32 {
    tick_count as f32 + partial_tick
}

/// `blockEntity.tickCount / 66 % 3` — **integer** division, so this steps once
/// every 66 ticks regardless of the fractional partial tick baked into
/// [`conduit_anim_time`] (which this does *not* take; it reads the raw tick
/// counter directly). Selects which of the two wind planes' extra rotation
/// applies in [`BlockEntityModelSet::resolve_conduit`], and which sprite
/// (`wind` vs `wind_vertical`) both planes draw with.
#[must_use]
pub fn conduit_animation_phase(tick_count: u32) -> u8 {
    ((tick_count / 66) % 3) as u8
}

/// The bob-height fold shared by the cage and the eye —
/// `ConduitRenderer.submit`'s own `hh`, used as `0.3 + hh * 0.2`.
///
/// **Not** `ConduitBlockEntity.animationTick`'s same-named local — that one
/// adds a `+35`-tick phase offset and a trailing `* 0.3F` this one does not
/// have (`hh = (hh*hh+hh) * 0.3`, for a *particle* spawn position, not
/// geometry). The two share a name and a shape, which is exactly how a port
/// conflates them; only the version transcribed here belongs in the renderer.
#[must_use]
pub fn conduit_bob(anim_time: f32) -> f32 {
    let hh = (anim_time * 0.1).sin() / 2.0 + 0.5;
    hh * hh + hh
}

/// Model name of the open-book rig, keying both the mesh set and the shell's
/// texture map.
///
/// Named for the *mesh* rather than for the lectern, because
/// `LecternRenderer` and `EnchantTableRenderer` bake the same
/// `ModelLayers.BOOK` layer. Only the lectern consumes it today; the enchanting
/// table needs its own animation state on top (see [`LECTERN_BOOK_OPENNESS`]).
pub const BOOK: &str = "book";

/// The jar sheet a book draws with — `EnchantTableRenderer.BOOK_TEXTURE`
/// (`Sheets.BLOCK_ENTITIES_MAPPER.defaultNamespaceApply("enchantment/enchanting_table_book")`).
///
/// **`LecternRenderer` has no texture of its own** — it passes
/// `EnchantTableRenderer.BOOK_TEXTURE` straight through, which is why this stem
/// says `enchantment` and not `lectern`. Grepping the jar for a lectern book
/// texture finds nothing.
pub const BOOK_TEXTURE_STEM: &str = "entity/enchantment/enchanting_table_book";

/// The one book sheet stem, for [`block_entity_texture_stems`] — same shape as
/// [`bell_texture_stems`].
#[must_use]
pub fn book_texture_stems() -> Vec<&'static str> {
    vec![BOOK_TEXTURE_STEM]
}

/// A lectern book's `openness`, which is a **compile-time constant**.
///
/// `LecternRenderer.BOOK_STATE` is
/// `BookModel.State.forAnimation(0.0, 0.1, 0.9, 1.2)`, and `forAnimation`
/// computes `openness = (sin(progress * 0.02) * 0.1 + 1.25) * openness`. With
/// `progress == 0` the `sin` term is exactly zero, so the whole expression
/// collapses to `1.25 * 1.2 == 1.5` for every lectern in the world, every frame.
///
/// That dead arithmetic is the trap: it *looks* like an animation, and porting a
/// live `progress` here would make every lectern book breathe, which vanilla's
/// does not. The page-flip animation belongs to `EnchantTableRenderer`, which
/// feeds `forAnimation` a real, client-simulated `progress`.
pub const LECTERN_BOOK_OPENNESS: f32 = 1.5;

/// A lectern book's `pageFlip1`/`pageFlip2` — `BOOK_STATE`'s second and third
/// arguments, also constant. Kept as named constants rather than inlined
/// because [`book_part_poses`] is the shared entry point for the enchanting
/// table too, where both of these *do* vary.
pub const LECTERN_BOOK_PAGE_FLIP: (f32, f32) = (0.1, 0.9);

/// `BookModel.setupAnim`'s six per-part poses, as `(part name, y_rot, x)`.
///
/// ```text
/// left_lid.yRot    = PI + openness
/// right_lid.yRot   = -openness
/// left_pages.yRot  = openness
/// right_pages.yRot = -openness
/// flip_page1.yRot  = openness - openness * 2 * page_flip1
/// flip_page2.yRot  = openness - openness * 2 * page_flip2
/// left_pages.x = right_pages.x = flip_page1.x = flip_page2.x = sin(openness)
/// ```
///
/// `x` is an **absolute** pivot in texels, not a delta: `setupAnim` assigns
/// `this.leftPages.x = Mth.sin(openness)`, overwriting the rest pose's `0`. The
/// two lids keep their rest `z` of ∓1 and are not moved in `x` at all, so they
/// carry `None`.
///
/// `seam` is deliberately absent — the jar never poses it, and its rest
/// `rotation(0, PI/2, 0)` is the spine's quarter turn. Adding it here with a
/// zero pose would flatten the spine into the covers.
#[must_use]
pub fn book_part_poses(
    openness: f32,
    page_flip: (f32, f32),
) -> [(&'static str, f32, Option<f32>); 6] {
    let slide = Some(openness.sin());
    [
        ("left_lid", std::f32::consts::PI + openness, None),
        ("right_lid", -openness, None),
        ("left_pages", openness, slide),
        ("right_pages", -openness, slide),
        (
            "flip_page1",
            openness - openness * 2.0 * page_flip.0,
            slide,
        ),
        (
            "flip_page2",
            openness - openness * 2.0 * page_flip.1,
            slide,
        ),
    ]
}

/// `Direction.getClockWise().toYRot()` for vanilla's four horizontal facing
/// names, or `None` for anything else.
///
/// `LecternRenderer.extractRenderState` stores
/// `getValue(FACING).getClockWise().toYRot()`, **not** `FACING.toYRot()`, and
/// then `submit` rotates by the *negation* of it. Both steps are easy to unwind
/// wrongly and each is a quarter turn: a book fed [`horizontal_facing_yaw`]
/// directly lies across the lectern's shelf at 90° to the reader.
///
/// A clockwise turn is `+90°` in `toYRot()` terms (north `180` → east `270`,
/// east `270` → south `0`), which is why this is one addition and not a second
/// four-arm match to keep in sync.
#[must_use]
pub fn horizontal_facing_clockwise_yaw(name: &str) -> Option<f32> {
    horizontal_facing_yaw(name).map(|yaw| (yaw + 90.0) % 360.0)
}

/// The world placement transform for a lectern's book — `LecternRenderer.submit`:
///
/// ```text
/// translate(0.5, 1.0625, 0.5) · rotateY(-yRot) · rotateZ(67.5°) · translate(0, -0.125, 0)
/// ```
///
/// `yRot` is [`horizontal_facing_clockwise_yaw`]'s value, in degrees.
///
/// **Not [`block_entity_placement_matrix`] with a yaw.** Three differences, all
/// visible: the translation is `1.0625` blocks up (the shelf's own height) and is
/// applied *before* the rotation, so the rotation pivots about the book rather
/// than about the block's floor corner; there is a `67.5°` tilt about **Z**,
/// which is the whole reason a lectern book faces a reader instead of lying
/// flat; and the final `-0.125` lift happens in the tilted frame, so it does
/// **not** commute with the translation at the front.
#[must_use]
pub fn lectern_book_placement_matrix(pos: [i32; 3], facing_yaw_deg: f32) -> Mat4 {
    const TILT_DEG: f32 = 67.5;
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    Mat4::from_translation(origin + Vec3::new(0.5, 1.0625, 0.5))
        * Mat4::from_rotation_y(-facing_yaw_deg.to_radians())
        * Mat4::from_rotation_z(TILT_DEG.to_radians())
        * Mat4::from_translation(Vec3::new(0.0, -0.125, 0.0))
}

/// `EnchantTableRenderer.submit`'s `Axis.ZP.rotationDegrees(80.0F)` — the tilt
/// that stands the floating book up.
///
/// **Not the lectern's `67.5`.** The two renderers share one mesh and one
/// `book_part_poses`, and nothing but this number and the placement below tells
/// them apart geometrically, so copying the lectern's transform is a change no
/// mesh assertion can see.
pub const ENCHANTING_TABLE_BOOK_TILT_DEG: f32 = 80.0;

/// The floating book's hover, in blocks: `0.1 + sin(time * 0.1) * 0.01` on top of
/// the base `0.75` lift.
///
/// A ±`0.01`-block bob — a sixth of a texel. It is easy to dismiss as noise and
/// drop, and then the book sits dead still, which is the one thing a player
/// actually notices about an enchanting table they are standing next to.
#[must_use]
pub fn enchanting_table_book_hover(time: f32) -> f32 {
    0.1 + (time * 0.1).sin() * 0.01
}

/// The world placement transform for the floating book over an enchanting table
/// — `EnchantTableRenderer.submit`:
///
/// ```text
/// T(pos) · T(0.5, 0.75, 0.5) · T(0, hover(time), 0) · Ry(-y_rot) · Rz(80°)
/// ```
///
/// # `y_rot` is **radians**, and it is not a block facing
///
/// `Axis.YP.rotation(-yRot)` takes radians where the lectern's
/// `Axis.YP.rotationDegrees` takes degrees, and the value is
/// `EnchantingTableBlockEntity.rot` — a client-simulated angle that chases the
/// nearest player, not a `Direction`. An enchanting table has no `facing`
/// property at all, so there is nothing on its block state this could have come
/// from; feeding it a facing yaw would pin every book to a compass direction and
/// look plausible until a player walks around one.
#[must_use]
pub fn enchanting_table_book_placement_matrix(pos: [i32; 3], y_rot: f32, time: f32) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    Mat4::from_translation(origin + Vec3::new(0.5, 0.75, 0.5))
        * Mat4::from_translation(Vec3::new(0.0, enchanting_table_book_hover(time), 0.0))
        * Mat4::from_rotation_y(-y_rot)
        * Mat4::from_rotation_z(ENCHANTING_TABLE_BOOK_TILT_DEG.to_radians())
}

/// `BookModel.State.forAnimation`'s first output — the live `openness` the
/// lectern's [`LECTERN_BOOK_OPENNESS`] is the frozen case of:
///
/// ```text
/// (sin(progress * 0.02) * 0.1 + 1.25) * open
/// ```
///
/// The lectern passes `progress == 0`, which kills the `sin` and collapses the
/// whole thing to `1.25 * 1.2 == 1.5`. Here `progress` is the block entity's real
/// `time` counter, so the term is alive and the book breathes between `1.15 * open`
/// and `1.35 * open`. **That is why the lectern's constant must not be reused
/// here, and why the dead arithmetic there must not be revived** — the same
/// expression is genuinely constant in one caller and genuinely animated in the
/// other.
#[must_use]
pub fn enchanting_table_book_openness(time: f32, open: f32) -> f32 {
    ((time * 0.02).sin() * 0.1 + 1.25) * open
}

/// `EnchantTableRenderer.submit`'s two page-flip phases, from the block entity's
/// `flip` accumulator:
///
/// ```text
/// clamp(frac(flip + 0.25) * 1.6 - 0.3, 0, 1)
/// clamp(frac(flip + 0.75) * 1.6 - 0.3, 0, 1)
/// ```
///
/// The two offsets are half a period apart, which is what makes the pages turn
/// alternately rather than together. **Both clamps are load-bearing and neither is
/// decorative:** `frac(..) * 1.6 - 0.3` ranges over `-0.3..1.3`, so an unclamped
/// value drives `openness - openness * 2 * page_flip` past the covers and turns a
/// page inside out through the spine. The `1.6`/`-0.3` pair is exactly what makes
/// each page spend part of its cycle pinned flat against a cover, which is what
/// vanilla's book looks like.
#[must_use]
pub fn enchanting_table_page_flips(flip: f32) -> (f32, f32) {
    let frac = |x: f32| x - x.floor();
    (
        (frac(flip + 0.25) * 1.6 - 0.3).clamp(0.0, 1.0),
        (frac(flip + 0.75) * 1.6 - 0.3).clamp(0.0, 1.0),
    )
}

/// How many cooking slots a campfire has — `CampfireBlockEntity`'s
/// `NonNullList.withSize(4, ItemStack.EMPTY)`.
pub const CAMPFIRE_SLOTS: usize = 4;

/// `CampfireRenderer.SIZE`: the uniform scale each cooking item is drawn at.
pub const CAMPFIRE_ITEM_SCALE: f32 = 0.375;

/// `CampfireRenderer.submit`'s lift: `0.44921875` blocks, i.e. `115/256`.
///
/// Not `0.4375` (`7/16`, the campfire block model's own top face) — the extra
/// `1/256` is what keeps a flat food sprite from z-fighting the log it lies on.
pub const CAMPFIRE_ITEM_LIFT: f32 = 0.449_218_75;

/// The world placement transform for the item cooking in a campfire's `slot`,
/// ported from `CampfireRenderer.submit` term for term:
///
/// ```text
/// T(pos) · T(0.5, 0.44921875, 0.5) · Ry(-slotYRot) · Rx(90°)
///        · T(-0.3125, -0.3125, 0) · S(0.375)
/// ```
///
/// Compose it with the item's own `display.fixed`
/// ([`display_matrix`](crate::display_matrix)) on the **right** — vanilla applies
/// the `ItemTransform` inside `ItemStackRenderState.LayerRenderState.submit`,
/// after everything above is on the pose stack. [`crate::entity::campfire_item_mesh`]
/// is that composition; prefer it to hand-multiplying here.
///
/// # A campfire is the only block entity here whose renderer draws no mesh of
/// its own
///
/// `CampfireRenderer` has no model, no layer and no sheet: the fire, the logs and
/// the smoke are all part of the **block** model, and the whole renderer is this
/// pose repeated over four item stacks. So there is no `campfire_model()` builder
/// and no texture stem to preload — reading "campfire needs a fire texture" off
/// the block's appearance is the wrong inference, and it is the one this port
/// nearly made.
///
/// # `slot` is an offset from the block's facing, not an absolute corner
///
/// Vanilla's `Direction.from2DDataValue((slot + facing.get2DDataValue()) % 4)`
/// means slot 0 always sits in the corner the campfire *faces away* toward, and
/// the four march clockwise from there. Ignoring the facing term puts every
/// campfire's first item in the same world corner, which looks right until two
/// campfires face different ways.
///
/// `facing_yaw_deg` is [`horizontal_facing_yaw`]'s convention (south `0`), and
/// `get2DDataValue()` is exactly that divided by `90` — `toYRot()` is
/// `(data2d & 3) * 90` in `Direction` itself, so the two are one expression and
/// there is no second table to keep in sync.
#[must_use]
pub fn campfire_item_matrix(pos: [i32; 3], facing_yaw_deg: f32, slot: usize) -> Mat4 {
    // `(slot + facing.get2DDataValue()) % 4`, then back through `toYRot()`.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the four horizontal facing yaws are exact non-negative multiples of 90"
    )]
    let facing_2d = (facing_yaw_deg / 90.0) as usize;
    let slot_yaw = ((slot + facing_2d) % 4) as f32 * 90.0;
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    Mat4::from_translation(origin + Vec3::new(0.5, CAMPFIRE_ITEM_LIFT, 0.5))
        * Mat4::from_rotation_y(-slot_yaw.to_radians())
        * Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2)
        * Mat4::from_translation(Vec3::new(-0.3125, -0.3125, 0.0))
        * Mat4::from_scale(Vec3::splat(CAMPFIRE_ITEM_SCALE))
}

/// `BrushableBlockRenderer.translations`'s base offset — `[0.5, 0.0, 0.5]`
/// before the hit-direction override below replaces one axis.
const BRUSHABLE_ITEM_BASE_OFFSET: Vec3 = Vec3::new(0.5, 0.0, 0.5);

/// `BrushableBlockRenderer.submit`'s outward lift along the hit direction:
/// `completionState / 10.0F * 0.75F`, where `completionState` is the block
/// state's own `dusted` property (`0..=3`, `BrushableBlockEntity.getCompletionState`'s
/// own range) fed straight in — **not** rescaled to `0..=10` first, matching
/// the real jar's own (slightly odd) division by a constant larger than the
/// value's own range.
#[must_use]
fn brushable_item_offset(hit_direction: lodestone_assets::Direction, dust_progress: u8) -> Vec3 {
    use lodestone_assets::Direction;
    let completion_offset = f32::from(dust_progress) / 10.0 * 0.75;
    let mut xyz = BRUSHABLE_ITEM_BASE_OFFSET;
    match hit_direction {
        Direction::East => xyz.x = 0.73 + completion_offset,
        Direction::West => xyz.x = 0.25 - completion_offset,
        Direction::Up => xyz.y = 0.25 + completion_offset,
        Direction::Down => xyz.y = -0.23 - completion_offset,
        Direction::North => xyz.z = 0.25 - completion_offset,
        Direction::South => xyz.z = 0.73 + completion_offset,
    }
    xyz
}

/// The world placement matrix for the item revealed by brushing a suspicious
/// sand/gravel block, ported from `BrushableBlockRenderer.submit`'s pose
/// stack term for term:
///
/// ```text
/// T(pos) · T(0, 0.5, 0) · T(translations(hitDirection, dustProgress))
///        · Ry(75°) · Ry((eastWest ? 90 : 0) + 11°) · S(0.5)
/// ```
///
/// Compose with the item's own `display.fixed` on the right —
/// [`crate::entity::brushable_item_mesh`] does this, the same composition
/// [`campfire_item_matrix`] uses for the same `ItemDisplayContext.FIXED`
/// reason (`extractRenderState` resolves `blockEntity.getItem()` in `FIXED`,
/// not `GROUND`).
#[must_use]
pub fn brushable_item_matrix(
    pos: [i32; 3],
    hit_direction: lodestone_assets::Direction,
    dust_progress: u8,
) -> Mat4 {
    use lodestone_assets::Direction;
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    let offset = brushable_item_offset(hit_direction, dust_progress);
    let east_west = matches!(hit_direction, Direction::East | Direction::West);
    let extra_deg: f32 = if east_west { 90.0 } else { 0.0 } + 11.0;
    Mat4::from_translation(origin)
        * Mat4::from_translation(Vec3::new(0.0, 0.5, 0.0))
        * Mat4::from_translation(offset)
        * Mat4::from_rotation_y(75f32.to_radians())
        * Mat4::from_rotation_y(extra_deg.to_radians())
        * Mat4::from_scale(Vec3::splat(0.5))
}

/// Every sheet stem across every block-entity family — what the shell's
/// texture loader preloads. Union of [`chest_texture_stems`],
/// [`skull_texture_stems`], [`bell_texture_stems`],
/// [`banner_texture_stems`], [`shield_texture_stems`] and
/// [`shulker_texture_stems`] rather than the shell iterating each list
/// itself, so a new family only has to update this one function to reach the
/// loader (see the module doc's "How to change it" — this is the "entry in
/// the preload list" step, generalised past chest).
///
/// **Does not include a banner's pattern-mask sprites.** Those are a wholly
/// separate resource (the banner-pattern atlas, `lodestone-assets` work not
/// yet done — see `docs/banner-shield-patterns.md`'s "jar ships individual
/// sprite PNGs" section) and a wholly separate draw list
/// ([`BannerLayerDraw`]), not a stem this preload list can name.
#[must_use]
pub fn block_entity_texture_stems() -> Vec<&'static str> {
    let mut stems = chest_texture_stems();
    stems.extend(skull_texture_stems());
    stems.extend(bell_texture_stems());
    stems.extend(banner_texture_stems());
    stems.extend(shield_texture_stems());
    stems.extend(shulker_texture_stems());
    stems.extend(book_texture_stems());
    stems.extend(decorated_pot_texture_stems());
    stems.extend(conduit_texture_stems());
    stems
}

/// The baked block-entity corpus: one [`BlockEntityMesh`] per entry in
/// [`BLOCK_ENTITY_MODELS`], baked on the CPU with no GPU involvement.
#[derive(Debug, Clone)]
pub struct BlockEntityModelSet {
    models: Vec<(&'static str, BlockEntityMesh)>,
}

impl BlockEntityModelSet {
    /// Bakes every ported block-entity model.
    #[must_use]
    pub fn load() -> Self {
        BlockEntityModelSet {
            models: BLOCK_ENTITY_MODELS
                .iter()
                .map(|entry: &BlockEntityModelEntry| {
                    (entry.name, BlockEntityMesh::from_model(&(entry.build)()))
                })
                .collect(),
        }
    }

    /// Iterates `(model name, mesh)`.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &BlockEntityMesh)> {
        self.models.iter().map(|(n, m)| (*n, m))
    }

    /// The mesh for a model name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&BlockEntityMesh> {
        self.models
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, m)| m)
    }

    /// Number of baked models.
    #[must_use]
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Whether the corpus is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Resolves a **`minecraft:special` item** into a drawable instance at an
    /// arbitrary placement — the world surfaces of the seam
    /// [`special_item_rig`] owns.
    ///
    /// `kind` is the special-renderer id from the item's own definition
    /// (`SpecialItemForm::kind`), `item_path` the item's registry path with the
    /// namespace stripped, and `placement` the surface's already-composed
    /// block→world matrix: a dropped stack's bob-and-spin chain, another entity's
    /// hand chain, or an item frame's wall pose. `transformation` is the item
    /// definition's whole root-to-`special` `"transformation"` chain, outermost
    /// first (`SpecialItemForm::transformation`) — folded *underneath*
    /// `placement`, same as every other consumer; see
    /// [`crate::compose_special_node_transform`]'s doc for the derivation.
    /// Returns `None` when the `kind` has no ported rig (six of the ten do not)
    /// or the rig is not in the corpus.
    ///
    /// # Why this exists rather than each surface building the instance itself
    ///
    /// Three callers, each with its own placement and nothing else different.
    /// Everything after the placement — the rig lookup, the sheet, the rest-pose
    /// part transforms, the cull AABB — is identical, and a second copy of it is
    /// how one surface ends up drawing a chest the batcher groups under the wrong
    /// sheet. It is the same shape [`Self::resolve_chest`] and its six siblings
    /// have, differing only in taking the placement instead of deriving one from a
    /// block position.
    ///
    /// # No pose overrides, deliberately
    ///
    /// `part_transforms(placement, &[])`, so a chest's lid is **shut** and a
    /// shulker box's is **closed**: `ChestSpecialRenderer`/`ShulkerBoxSpecialRenderer`
    /// carry a fixed `openness` and no animation at all. An item is not the block,
    /// and passing a lid angle here would open every chest lying on every floor.
    #[must_use]
    pub fn resolve_special_item(
        &self,
        kind: &str,
        item_path: &str,
        placement: Mat4,
        transformation: &[lodestone_assets::ItemNodeTransform],
        light: u8,
    ) -> Option<BlockEntityInstance> {
        let (model, texture) = special_item_rig(kind, item_path)?;
        let mesh = self.get(model)?;
        let placement = crate::compose_special_node_transform(placement, transformation);
        let part_transforms = mesh.part_transforms(placement, &[]);
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model,
            texture,
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one chest into a drawable instance, or `None` if its model is
    /// not in the corpus.
    #[must_use]
    pub fn resolve_chest(&self, spawn: &ChestSpawn) -> Option<BlockEntityInstance> {
        let model = spawn.half.model();
        let mesh = self.get(model)?;
        let placement = block_entity_placement_matrix(spawn.pos, spawn.facing_yaw_deg);

        // The lid and the lock rotate together about the *same* pivot
        // (`lock.xRot = lid.xRot`), which is why the asset corpus makes them
        // siblings sharing `offset(0, 9, 1)` rather than nesting the lock.
        let x_rot = chest_lid_x_rot(chest_lid_openness(spawn.openness));
        let mut overrides = Vec::with_capacity(2);
        for name in ["lid", "lock"] {
            if let Some(index) = mesh.index_of(name) {
                let mut pose = mesh.part_rest[index];
                pose.x_rot = x_rot;
                overrides.push((index, pose));
            }
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);

        // The cull AABB is taken from the *rest* bounds through the placement
        // matrix, deliberately ignoring the lid angle: an open lid only ever
        // grows the box backwards by a few texels, and recomputing per frame
        // would make a chest pop in and out of view as somebody opens it.
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model,
            texture: chest_texture_stem(spawn.material, spawn.half),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one skull/head into a drawable instance, or `None` if its
    /// model is not in the corpus.
    ///
    /// No overrides: unlike a chest lid, none of the five ported skull types
    /// pose their head part (`SkullBlockRenderState.yRot`/`xRot` are only
    /// ever set for the *item-frame*/GUI skull paths, never by
    /// `SkullBlockRenderer.extractRenderState` for a placed block), so
    /// `part_transforms` is built from the rest pose alone — same shape as
    /// [`Self::resolve_chest`] minus the animation.
    #[must_use]
    pub fn resolve_skull(&self, spawn: &SkullSpawn) -> Option<BlockEntityInstance> {
        let model = spawn.skull_type.model();
        let mesh = self.get(model)?;
        let placement = match spawn.orientation {
            SkullOrientation::Floor { rotation_segment } => {
                skull_ground_placement_matrix(spawn.pos, rotation_segment)
            }
            SkullOrientation::Wall { facing_yaw_deg } => {
                skull_wall_placement_matrix(spawn.pos, facing_yaw_deg)
            }
        };
        let part_transforms = mesh.part_transforms(placement, &[]);
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model,
            texture: skull_texture_stem(spawn.skull_type),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one bell into a drawable instance, or `None` if the model is
    /// not in the corpus.
    ///
    /// `bell_body` is the only part overridden — `bell_base` is its *child*
    /// in the baked mesh (see `lodestone_assets::block_entity_models::bell_model`'s
    /// doc), so rotating the body carries the rim with it through
    /// [`BlockEntityMesh::part_transforms`]'s own chain, exactly as
    /// `ModelPart`'s parent/child composition does in the jar. There is no
    /// second override for `bell_base`, unlike chest's `lid`/`lock` pair,
    /// because vanilla itself poses only `bellBody.xRot`/`zRot`.
    #[must_use]
    pub fn resolve_bell(&self, spawn: &BellSpawn) -> Option<BlockEntityInstance> {
        let mesh = self.get(BELL)?;
        let placement = block_entity_placement_matrix(spawn.pos, 0.0);

        let (x_rot, z_rot) = match spawn.shake {
            Some((direction, ticks)) => bell_shake_angle(Some(direction), ticks),
            None => (0.0, 0.0),
        };
        let mut overrides = Vec::with_capacity(1);
        if let Some(index) = mesh.index_of("bell_body") {
            let mut pose = mesh.part_rest[index];
            pose.x_rot = x_rot;
            pose.z_rot = z_rot;
            overrides.push((index, pose));
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);

        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model: BELL,
            texture: BELL_TEXTURE_STEM,
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one shulker box into a drawable instance, or `None` if the model
    /// is not in the corpus.
    ///
    /// Only `lid` is ever overridden, and only when the box is actually open:
    /// `progress == 0.0` leaves the rest pose alone, so the common case produces
    /// an instance whose `part_transforms` depend on nothing but `pos` and
    /// `facing`.
    #[must_use]
    pub fn resolve_shulker(&self, spawn: &ShulkerSpawn) -> Option<BlockEntityInstance> {
        let mesh = self.get(SHULKER_BOX)?;
        let placement = shulker_placement_matrix(spawn.pos, spawn.facing);

        let mut overrides = Vec::new();
        if spawn.progress > 0.0
            && let Some(index) = mesh.index_of("lid")
        {
            let (y, y_rot) = shulker_lid_pose(spawn.progress);
            let mut pose = mesh.part_rest[index];
            pose.y = y;
            pose.y_rot = y_rot;
            overrides.push((index, pose));
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model: SHULKER_BOX,
            texture: shulker_texture_stem(spawn.colour),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one decorated pot into a base instance plus up to four
    /// independently textured side instances — the base plus per-side
    /// decomposition [`docs/block-entity-renderers.md`] names as the way
    /// around the `(model, texture)` single-texture-per-instance batch key.
    /// `None` only if the base model itself is missing from the corpus (the
    /// four side models are checked the same way, per side, and a missing
    /// one simply omits that side rather than failing the whole pot).
    ///
    /// # Five instances, five ordinary batch slots — no new mechanism
    ///
    /// Unlike [`Self::resolve_banner`], nothing here needs a second,
    /// unbatched draw pass. A banner layers N *tinted masks* over **one**
    /// mesh, which the plain `(model, texture)` key cannot express and which
    /// is why that pass exists at all. A decorated pot instead needs four
    /// **distinct diffuse textures on four distinct quads** — a shape the
    /// ordinary batcher was already built for, the same way two chests with
    /// different materials batch separately today. Each returned instance
    /// carries its own `model/texture` pair and rejoins
    /// [`plan_block_entities`]'s normal instanced draw exactly like a chest
    /// or a skull; two pots sharing a sherd on the same side coalesce into
    /// one GPU instance of one batch, the same as two oak chests would.
    ///
    /// # Every side always draws
    ///
    /// Vanilla's `DecoratedPotRenderer.submit` calls `submitModelPart` for
    /// `front`/`back`/`left`/`right` unconditionally, falling back to
    /// `Sheets.DECORATED_POT_SIDE` per side rather than skipping an
    /// undecorated one — so this returns all four side instances always, not
    /// only the decorated ones. Skipping a blank side would draw a pot with
    /// invisible faces on three sides of the corpus's test cases and silently
    /// autocorrect on real ones the moment a player added their first sherd.
    #[must_use]
    pub fn resolve_decorated_pot(&self, spawn: &DecoratedPotSpawn) -> Option<[BlockEntityInstance; 5]> {
        let placement = decorated_pot_placement_matrix(spawn.pos, spawn.facing_yaw_deg);

        let instance = |model: &'static str, texture: &'static str| -> Option<BlockEntityInstance> {
            let mesh = self.get(model)?;
            let part_transforms = mesh.part_transforms(placement, &[]);
            let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
            Some(BlockEntityInstance {
                model,
                texture,
                transform: placement,
                part_transforms,
                aabb_min,
                aabb_max,
                light: spawn.light,
                tint: [255, 255, 255],
            })
        };

        let side_texture = |sherd: &Option<String>| -> &'static str {
            sherd
                .as_deref()
                .and_then(decorated_pot_pattern_texture_stem)
                .unwrap_or(DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM)
        };

        let base = instance(DECORATED_POT_BASE, DECORATED_POT_BASE_TEXTURE_STEM)?;
        let front = instance(DECORATED_POT_SIDE_FRONT, side_texture(&spawn.front))?;
        let back = instance(DECORATED_POT_SIDE_BACK, side_texture(&spawn.back))?;
        let left = instance(DECORATED_POT_SIDE_LEFT, side_texture(&spawn.left))?;
        let right = instance(DECORATED_POT_SIDE_RIGHT, side_texture(&spawn.right))?;
        Some([base, front, back, left, right])
    }

    /// Resolves one conduit into its drawable instances — `ConduitRenderer.submit`,
    /// branch by branch. Returns one instance (the slowly-spinning inactive
    /// shell) or four (the tumbling cage, both wind planes, and the
    /// camera-facing eye) — never a mix, because vanilla's own `submit` is a
    /// single `if (!state.isActive) { … } else { … }`, not two independently
    /// gated draws. A missing mesh drops that one instance rather than the
    /// whole conduit, the same fail-open [`Self::resolve_decorated_pot`]'s four
    /// sides use.
    ///
    /// `camera_orientation` is [`crate::entity::camera_orientation`] applied to
    /// the frame's view matrix — only the eye instance reads it
    /// (`poseStack.mulPose(camera.orientation)`, active branch only), but it is
    /// taken unconditionally so building it is the caller's problem once per
    /// frame, exactly as [`crate::entity::experience_orb_matrix`] does.
    ///
    /// See [`conduit_frame_scan`]/[`conduit_advance`] for how [`ConduitSpawn`]'s
    /// fields are meant to be produced — this method only consumes an
    /// already-resolved spawn, the same split [`Self::resolve_bell`] makes
    /// between [`crate::block_entity::BellSpawn`] and its `BellShakes` tracker.
    #[must_use]
    pub fn resolve_conduit(
        &self,
        spawn: &ConduitSpawn,
        camera_orientation: Mat4,
    ) -> Vec<BlockEntityInstance> {
        let origin = Vec3::new(
            spawn.pos[0] as f32,
            spawn.pos[1] as f32,
            spawn.pos[2] as f32,
        );
        let make = |model: &'static str, texture: &'static str, placement: Mat4| {
            let mesh = self.get(model)?;
            let part_transforms = mesh.part_transforms(placement, &[]);
            let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
            Some(BlockEntityInstance {
                model,
                texture,
                transform: placement,
                part_transforms,
                aabb_min,
                aabb_max,
                light: spawn.light,
                tint: [255, 255, 255],
            })
        };

        if !spawn.active {
            // `!state.isActive`: `translate(0.5,0.5,0.5)` then
            // `rotationY(state.activeRotation * (PI/180))` — the "degrees"
            // reading, see [`conduit_inactive_y_rot_radians`]'s doc — then the
            // 6×6×6 `shell` layer against `SHELL_TEXTURE` (`entity/conduit/base`).
            let placement = Mat4::from_translation(origin + Vec3::new(0.5, 0.5, 0.5))
                * Mat4::from_rotation_y(conduit_inactive_y_rot_radians(
                    spawn.active_rotation_value,
                ));
            return make(CONDUIT_SHELL, CONDUIT_SHELL_TEXTURE_STEM, placement)
                .into_iter()
                .collect();
        }

        let hh = conduit_bob(spawn.anim_time);
        // `translate(0.5F, 0.3F + hh * 0.2F, 0.5F)` — the cage's and the eye's
        // own bob; the two wind planes stay fixed at `translate(0.5,0.5,0.5)`
        // with **no** `hh` term. All four calls look alike at a glance; only two
        // of them carry this.
        let bob = 0.3 + hh * 0.2;
        let mut out = Vec::with_capacity(4);

        // Cage: tumbles about the fixed diagonal `Vector3f(0.5,1,0.5).normalize()`,
        // using the "radians" reading of `state.activeRotation` — see
        // [`conduit_active_axis_rotation_radians`]'s doc for why that is not a
        // second unit conversion.
        let axis = Vec3::new(0.5, 1.0, 0.5).normalize();
        let cage_placement = Mat4::from_translation(origin + Vec3::new(0.5, bob, 0.5))
            * Mat4::from_axis_angle(
                axis,
                conduit_active_axis_rotation_radians(spawn.active_rotation_value),
            );
        out.extend(make(CONDUIT_CAGE, CONDUIT_CAGE_TEXTURE_STEM, cage_placement));

        // The two wind planes share **one** texture choice — vanilla computes
        // `windSpriteId`/`windRenderType`/`windSprite` once and passes the same
        // three to both `submitModelPart` calls.
        let wind_texture = if spawn.animation_phase == 1 {
            CONDUIT_WIND_VERTICAL_TEXTURE_STEM
        } else {
            CONDUIT_WIND_TEXTURE_STEM
        };
        // First wind plane: `translate(0.5,0.5,0.5)` then, only by
        // `animationPhase`, a quarter turn about X (phase 1) or Z (phase 2) —
        // phase 0 submits with no extra rotation at all.
        let wind1_rot = match spawn.animation_phase {
            1 => Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2),
            2 => Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2),
            _ => Mat4::IDENTITY,
        };
        let wind1_placement =
            Mat4::from_translation(origin + Vec3::new(0.5, 0.5, 0.5)) * wind1_rot;
        out.extend(make(CONDUIT_WIND, wind_texture, wind1_placement));

        // Second wind plane: `translate(0.5,0.5,0.5)`, `scale(0.875)`, then
        // `rotationXYZ(PI, 0, PI)` — JOML's `rotationXYZ(x,y,z)` composes as
        // `rotateX(x).rotateY(y).rotateZ(z)` (each a post-multiply), so with
        // `y == 0` this is `qX(PI) * qZ(PI)`: rotate 180° about Z first, then
        // 180° about X. Written as two `glam::Quat`s multiplied in that same
        // left-to-right order, not collapsed to a single axis by hand.
        let wind2_rot =
            Quat::from_rotation_x(std::f32::consts::PI) * Quat::from_rotation_z(std::f32::consts::PI);
        let wind2_placement = Mat4::from_translation(origin + Vec3::new(0.5, 0.5, 0.5))
            * Mat4::from_scale(Vec3::splat(0.875))
            * Mat4::from_quat(wind2_rot);
        out.extend(make(CONDUIT_WIND, wind_texture, wind2_placement));

        // Eye: `translate(0.5, 0.3+hh*0.2, 0.5)`, `scale(0.5)`,
        // `mulPose(camera.orientation)`, `mulPose(rotationZ(PI).rotateY(PI))`,
        // `scale(1.3333334)` — net linear scale `0.5 * 1.3333334 ≈ 0.6667`, not
        // a single `scale(1.3333334)`; the two calls are not redundant, they
        // straddle the billboard rotation. `rotationZ(PI).rotateY(PI)` is
        // `qZ(PI) * qY(PI)` by the same JOML post-multiply reading as the
        // second wind plane above.
        let eye_rot =
            Quat::from_rotation_z(std::f32::consts::PI) * Quat::from_rotation_y(std::f32::consts::PI);
        let eye_placement = Mat4::from_translation(origin + Vec3::new(0.5, bob, 0.5))
            * Mat4::from_scale(Vec3::splat(0.5))
            * camera_orientation
            * Mat4::from_quat(eye_rot)
            * Mat4::from_scale(Vec3::splat(1.333_333_4));
        let eye_texture = if spawn.hunting {
            CONDUIT_OPEN_EYE_TEXTURE_STEM
        } else {
            CONDUIT_CLOSED_EYE_TEXTURE_STEM
        };
        out.extend(make(CONDUIT_EYE, eye_texture, eye_placement));

        out
    }

    /// Resolves one ground/standing banner into its opaque body+flag
    /// instances plus its ordered, translucent pattern-layer draw list, or
    /// `None` if either model is not in the corpus.
    ///
    /// # Two meshes, three draws — see the module's banner section
    ///
    /// Vanilla's `submitBanner` draws the pole+bar opaque, the flag opaque
    /// (both with `Sheets.BANNER_BASE`), then `submitPatterns`: the base
    /// mask tinted by `base_color` plus every stored pattern layer, all
    /// through `RenderTypes::bannerPattern`
    /// (`EntityPipeline::banner_layer_pipeline`). The first two ride the
    /// ordinary [`plan_block_entities`] batcher via
    /// [`BannerInstances::body`]/[`BannerInstances::flag`]; the third is
    /// [`BannerInstances::layers`], a flat ordered list a caller draws
    /// directly, one draw per entry, in order — never re-batched by texture,
    /// since these draws are translucent and depth-write-off and so must
    /// submit in the item's own stored order (two banners reusing the same
    /// two sprites in opposite orders could not both be right).
    ///
    /// # The flag's own transform, reused by every layer
    ///
    /// Every pattern mask paints over the *posed* flag (the same sway
    /// [`banner_flag_x_rot`] applies to the opaque flag draw), never the
    /// pole/bar — `submitPatterns` is called with the same `flagModel`
    /// [`submitBanner`] already posed. [`BannerLayerDraw::transform`] is
    /// therefore the flag part's own world matrix, computed once and shared
    /// by all `1 + patterns.len().min(MAX_PATTERN_LAYERS)` layers.
    #[must_use]
    pub fn resolve_banner(&self, spawn: &BannerSpawn) -> Option<BannerInstances> {
        // Both the mesh pair and the placement angle come from the attachment, in
        // one match, so a wall banner can never be drawn on the standing rig (a
        // 42-texel pole hanging in mid-air) or at the wrong angle.
        let (body_model, flag_model, placement) = match spawn.attachment {
            BannerAttachment::Ground { rotation_segment } => (
                BANNER_BODY,
                BANNER_FLAG,
                banner_ground_placement_matrix(spawn.pos, rotation_segment),
            ),
            BannerAttachment::Wall { facing_yaw_deg } => (
                BANNER_WALL_BODY,
                BANNER_WALL_FLAG,
                banner_wall_placement_matrix(spawn.pos, facing_yaw_deg),
            ),
        };
        let body_mesh = self.get(body_model)?;
        let flag_mesh = self.get(flag_model)?;

        let body_transforms = body_mesh.part_transforms(placement, &[]);
        let (body_min, body_max) =
            transformed_aabb(&placement, body_mesh.local_min, body_mesh.local_max);
        let body = BlockEntityInstance {
            model: body_model,
            texture: BANNER_BASE_TEXTURE_STEM,
            transform: placement,
            part_transforms: body_transforms,
            aabb_min: body_min,
            aabb_max: body_max,
            light: spawn.light,
            tint: [255, 255, 255],
        };

        // The one override: the flag's own sway, the same mechanism the
        // chest lid and the bell body already use.
        let x_rot = banner_flag_x_rot(spawn.phase);
        let flag_index = flag_mesh.index_of("flag")?;
        let mut pose = flag_mesh.part_rest[flag_index];
        pose.x_rot = x_rot;
        let flag_transforms = flag_mesh.part_transforms(placement, &[(flag_index, pose)]);
        let (flag_min, flag_max) =
            transformed_aabb(&placement, flag_mesh.local_min, flag_mesh.local_max);
        let flag_world = flag_transforms[flag_index];
        let flag = BlockEntityInstance {
            model: flag_model,
            texture: BANNER_BASE_TEXTURE_STEM,
            transform: placement,
            part_transforms: flag_transforms,
            aabb_min: flag_min,
            aabb_max: flag_max,
            light: spawn.light,
            tint: [255, 255, 255],
        };

        let layers = banner_pattern_layers(spawn.base_color, &spawn.patterns)
            .into_iter()
            .map(|layer| BannerLayerDraw {
                transform: flag_world,
                sprite: layer.sprite,
                color: layer.color,
                light: spawn.light,
            })
            .collect();

        Some(BannerInstances { body, flag, layers })
    }

    /// Resolves one lectern's open book into a drawable instance, or `None` if
    /// the model is not in the corpus.
    ///
    /// Six overrides, one per posed part, from [`book_part_poses`] — the widest
    /// override list in this module, and the reason
    /// [`BlockEntityMesh::part_transforms`]' `(index, pose)` mechanism was
    /// written to take a slice rather than one part.
    ///
    /// Every one of the six is a **flat child of the root**, so there is no
    /// parent/child composition to get right the way [`Self::resolve_bell`] has.
    /// What there *is* instead is `x`: four of the six move their pivot as well
    /// as rotating, which no other type here does.
    ///
    /// Nothing about the result varies per frame — [`LECTERN_BOOK_OPENNESS`] is
    /// constant — so a caller may cache it against `(pos, facing)` if it ever
    /// matters.
    #[must_use]
    pub fn resolve_lectern(&self, spawn: &LecternSpawn) -> Option<BlockEntityInstance> {
        let mesh = self.get(BOOK)?;
        let placement = lectern_book_placement_matrix(spawn.pos, spawn.facing_yaw_deg);

        let mut overrides = Vec::with_capacity(6);
        for (name, y_rot, x) in book_part_poses(LECTERN_BOOK_OPENNESS, LECTERN_BOOK_PAGE_FLIP) {
            let Some(index) = mesh.index_of(name) else {
                continue;
            };
            let mut pose = mesh.part_rest[index];
            pose.y_rot = y_rot;
            if let Some(x) = x {
                pose.x = x;
            }
            overrides.push((index, pose));
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);

        // The rest AABB through the placement matrix, like every other type
        // here. The posed book opens *wider* than its rest bounds (the lids
        // swing out past `openness` radians), so this is deliberately
        // generous-in-the-wrong-direction rather than exact — but a book is a
        // few texels across and the placement is a block off the floor, so the
        // block's own AABB dominates either way.
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model: BOOK,
            texture: BOOK_TEXTURE_STEM,
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }

    /// Resolves one enchanting table's floating book into a drawable instance, or
    /// `None` if the model is not in the corpus.
    ///
    /// The same mesh, the same six overrides and the same
    /// [`book_part_poses`] as [`Self::resolve_lectern`] — and **everything else
    /// differs**. The placement is
    /// [`enchanting_table_book_placement_matrix`] (an `80°` tilt, not `67.5`, and
    /// a live hover), the openness is
    /// [`enchanting_table_book_openness`] rather than the lectern's frozen
    /// constant, and the page flips come from
    /// [`enchanting_table_page_flips`] rather than a pair of literals.
    ///
    /// So this is emphatically **not** `resolve_lectern` with a different matrix,
    /// and folding the two into one function parameterised by a matrix would
    /// silently give an enchanting table the lectern's dead `1.5` openness — a
    /// book that never opens, on a rig that draws perfectly.
    #[must_use]
    pub fn resolve_enchanting_table(
        &self,
        spawn: &EnchantingTableSpawn,
    ) -> Option<BlockEntityInstance> {
        let mesh = self.get(BOOK)?;
        let placement =
            enchanting_table_book_placement_matrix(spawn.pos, spawn.y_rot, spawn.time);
        let openness = enchanting_table_book_openness(spawn.time, spawn.open);
        let page_flip = enchanting_table_page_flips(spawn.flip);

        let mut overrides = Vec::with_capacity(6);
        for (name, y_rot, x) in book_part_poses(openness, page_flip) {
            let Some(index) = mesh.index_of(name) else {
                continue;
            };
            let mut pose = mesh.part_rest[index];
            pose.y_rot = y_rot;
            if let Some(x) = x {
                pose.x = x;
            }
            overrides.push((index, pose));
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);

        // The rest AABB through the placement matrix, as `resolve_lectern` — and
        // generous in the same wrong direction, but by more: this book's lids
        // swing all the way open rather than sitting at a fixed `1.5`. Still
        // dominated by the block's own AABB, and the hover is ±0.01 blocks.
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model: BOOK,
            texture: BOOK_TEXTURE_STEM,
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
            tint: [255, 255, 255],
        })
    }
}

/// The version-free description of one enchanting table's floating book this
/// frame — every field already interpolated by the caller.
///
/// **The only spawn in this module whose every animated field is client-simulated
/// with nothing on the wire.** `EnchantingTableBlockEntity.bookAnimationTick` runs
/// on the client, driven by the nearest player's position, and the server sends
/// none of `time`/`open`/`flip`/`rot` — so a source that captured a stale copy of
/// this state freezes the book, and there is no packet whose absence would
/// explain it.
///
/// Interpolation belongs to the caller (`EnchantTableRenderer.extractRenderState`
/// does it, not `submit`): `open` and `flip` are `lerp(partialTicks, o*, *)`,
/// `time` is `time + partialTicks`, and `y_rot` is the **shortest-arc** lerp of
/// `oRot`→`rot`. That last one is not an ordinary lerp — see
/// [`Self::y_rot`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnchantingTableSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// The book's facing, in **radians**, already shortest-arc interpolated:
    /// `oRot + wrap(rot - oRot) * partialTicks`, where `wrap` brings the delta
    /// into `-PI..PI`.
    ///
    /// Skipping the wrap makes the book spin the long way round — a full
    /// backwards revolution in one tick — every time the angle crosses `±PI`,
    /// which happens whenever a player walks past the north-west corner. A plain
    /// `lerp` is wrong in exactly one place and looks right everywhere else.
    pub y_rot: f32,
    /// `EnchantingTableBlockEntity.time + partialTicks` — vanilla's raw tick
    /// counter, feeding both the hover and the openness breath.
    pub time: f32,
    /// `lerp(partialTicks, oOpen, open)`, `0..1`: how far the book has opened.
    /// `0` is fully shut and renders identically to no book at all, which is what
    /// makes garbage-collecting a rested entry safe.
    pub open: f32,
    /// `lerp(partialTicks, oFlip, flip)` — the page-flip accumulator, **not** a
    /// `0..1` phase. It is unbounded and drifts in either direction; the two
    /// phases come out of [`enchanting_table_page_flips`].
    pub flip: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl EnchantingTableSpawn {
    /// A fully-open, resting, full-bright book over the table at `pos` — the
    /// minimum a hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        EnchantingTableSpawn {
            pos,
            y_rot: 0.0,
            time: 0.0,
            open: 1.0,
            flip: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one lectern's book to draw this frame.
///
/// Two fields and no animation state at all, which makes this the cheapest type
/// in the module: `LecternBlock.HAS_BOOK` decides whether there is a spawn to
/// make in the first place (a bookless lectern draws nothing here — its shelf is
/// a real block model), and `FACING` gives the yaw. There is no NBT read and
/// nothing on the wire.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LecternSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// `Direction.getClockWise().toYRot()` of `LecternBlock.FACING`, in degrees
    /// — see [`horizontal_facing_clockwise_yaw`], which is the only correct way
    /// to produce this. Passing `FACING.toYRot()` puts the book sideways.
    pub facing_yaw_deg: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl LecternSpawn {
    /// A north-facing, full-bright lectern book at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        LecternSpawn {
            pos,
            facing_yaw_deg: horizontal_facing_clockwise_yaw("north").unwrap_or(270.0),
            light: ENTITY_FULLBRIGHT,
        }
    }
}

impl Default for BlockEntityModelSet {
    fn default() -> Self {
        Self::load()
    }
}

/// The version-free description of one chest to draw this frame.
///
/// The caller owns every field: block state → `facing_yaw_deg`/`half`, block
/// path → `material`, block event viewer count → `openness`, world light →
/// `light`. Keeping this a plain struct is what stops the render crate depending
/// on a protocol version or a client.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChestSpawn {
    /// Block position (the block's minimum corner, in world coordinates).
    pub pos: [i32; 3],
    /// `Direction.toYRot()` of the chest's `facing` property.
    pub facing_yaw_deg: f32,
    /// Which layer to draw.
    pub half: ChestHalf,
    /// Which sheet to draw with.
    pub material: ChestMaterial,
    /// **Raw** openness in `0..=1` — the eased value is computed here, so a
    /// caller that already eased would double-ease.
    pub openness: f32,
    /// Packed sky/block light (`sky << 4 | block`) at this block. Pass
    /// [`ENTITY_FULLBRIGHT`] only when there is genuinely no world to sample.
    pub light: u8,
}

impl ChestSpawn {
    /// A closed, full-bright, south-facing single chest at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        ChestSpawn {
            pos,
            facing_yaw_deg: 0.0,
            half: ChestHalf::Single,
            material: ChestMaterial::Regular,
            openness: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one skull/head to draw this frame.
///
/// The caller owns every field, the same contract as [`ChestSpawn`]: block
/// state → `orientation`/`skull_type`, world light → `light`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkullSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// Floor or wall placement.
    pub orientation: SkullOrientation,
    /// Which mob's model and sheet.
    pub skull_type: SkullType,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl SkullSpawn {
    /// A floor-placed, `rotation_segment = 0`, full-bright skeleton skull at
    /// `pos` — the minimum a hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        SkullSpawn {
            pos,
            orientation: SkullOrientation::Floor { rotation_segment: 0 },
            skull_type: SkullType::Skeleton,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one bell to draw this frame.
///
/// Unlike [`ChestSpawn`]/[`SkullSpawn`], placement needs no facing at all:
/// `BellRenderer.submit` applies no rotation of its own before
/// `submitModel` (contrast `ChestRenderer.submit`'s explicit
/// `rotationAround`), so every `FACING`/`ATTACHMENT` combination poses the
/// body identically — only the block's own attachment-frame *model* (drawn
/// by the ordinary block mesher, not this pass) differs per attachment.
/// [`BlockEntityModelSet::resolve_bell`] therefore calls
/// [`block_entity_placement_matrix`] with a fixed `facing_yaw_deg` of `0.0`,
/// reusing the chest's placement function unchanged rather than adding a
/// bell-specific one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BellSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// The in-progress shake (direction plus vanilla's raw tick counter,
    /// `0..50`), or `None` at rest.
    ///
    /// **`None` is the only value this pass can produce today.** The
    /// `BLOCK_EVENT` trigger that starts a shake (`b0 == 1`, direction packed
    /// in `b1` — `BellBlockEntity.triggerEvent`) is not wired from any
    /// gather in this crate; see `docs/block-entity-renderers.md`'s Bell
    /// section for exactly what is missing and why (the install call site is
    /// outside this crate's file ownership for the session that ported the
    /// geometry). A bell always draws — closing the "hole" the doc's chest
    /// section describes for a model-less block entity — it just never
    /// shakes yet.
    pub shake: Option<(BellShakeDirection, f32)>,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl BellSpawn {
    /// A resting, full-bright bell at `pos` — the minimum a hermetic gate
    /// needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        BellSpawn {
            pos,
            shake: None,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one shulker box to draw this frame.
///
/// Three fields and no animation state, which is why this type was the cheapest
/// one to add after bell: the box's facing and its dye colour both come straight
/// off the block state (`FACING`, and the block id for the colour), and a closed
/// box needs no part override — so a shulker box slots into
/// [`plan_block_entities`]' existing `(model, texture)` batch key untouched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShulkerSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// `ShulkerBoxBlock.FACING`, defaulting to [`ShulkerFacing::Up`] the way
    /// `ShulkerBoxRenderer.extractRenderState`'s `getValueOrElse` does.
    pub facing: ShulkerFacing,
    /// The dye colour name (`"red"`, …) or `None` for the undyed box.
    pub colour: Option<&'static str>,
    /// `ShulkerBoxBlockEntity.getProgress(partialTicks)` — `0.0` closed, `1.0`
    /// fully open.
    ///
    /// **`0.0` is the only value this pass can produce today.** Progress comes
    /// from the block entity's own open/close counter, which the server drives
    /// through the same `BLOCK_EVENT` path a chest lid uses — and unlike a chest,
    /// nothing in this workspace folds a shulker box's event yet. A closed box is
    /// what a shulker box looks like whenever nobody has it open, so this is the
    /// honest state rather than a placeholder; see
    /// `docs/block-entity-renderers.md`.
    pub progress: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl ShulkerSpawn {
    /// A closed, upward-facing, undyed, full-bright box at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        ShulkerSpawn {
            pos,
            facing: ShulkerFacing::Up,
            colour: None,
            progress: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one banner — standing **or** wall — to draw
/// this frame.
///
/// The caller owns every field, the same contract as [`ChestSpawn`]: the
/// `ROTATION` property or `FACING`, whichever the block has → `attachment`; the
/// banner **block's own** colour (`AbstractBannerBlock.getColor()` — one banner
/// block per dye colour, there is no `type`-style state property, so this is
/// not read off block state the way [`ChestSpawn::material`] is) →
/// `base_color`; the block entity's own NBT `"patterns"` key
/// (`docs/banner-shield-patterns.md`'s "Prerequisite 1 does not block the
/// block-entity consumer" section — this is *not* an item component) →
/// `patterns`; the world clock → `phase` (see [`banner_phase`]); world light
/// → `light`.
///
/// Everything past `attachment` is shared by both forms, including the sway and
/// the whole pattern-layer stack — `BannerRenderer` picks two meshes and an angle
/// off the attachment type and then runs one `submitBanner` for either.
#[derive(Debug, Clone, PartialEq)]
pub struct BannerSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// Standing or wall, carrying that form's own angle — see
    /// [`BannerAttachment`], and [`banner_ground_placement_matrix`] for why a
    /// rotation segment is not [`horizontal_facing_yaw`]'s convention.
    pub attachment: BannerAttachment,
    /// The banner block's own dye colour.
    pub base_color: DyeColor,
    /// The block entity's stored pattern layers, in stack order.
    pub patterns: Vec<StoredPatternLayer>,
    /// This frame's cloth-sway phase, `0.0..1.0` — see [`banner_phase`].
    pub phase: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl BannerSpawn {
    /// A resting (`phase = 0`), full-bright, segment-`0` **standing**,
    /// pattern-less white banner at `pos` — the minimum a hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        BannerSpawn {
            pos,
            attachment: BannerAttachment::Ground { rotation_segment: 0 },
            base_color: DyeColor::White,
            patterns: Vec::new(),
            phase: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }

    /// The wall sibling of [`Self::at`]: a resting, full-bright, pattern-less
    /// white banner on a wall facing `facing_yaw_deg`.
    #[must_use]
    pub fn on_wall(pos: [i32; 3], facing_yaw_deg: f32) -> Self {
        BannerSpawn {
            attachment: BannerAttachment::Wall { facing_yaw_deg },
            ..BannerSpawn::at(pos)
        }
    }
}

/// One item cooking in one campfire slot.
///
/// **The only `*Spawn` here that [`BlockEntityModelSet`] does not resolve**, and
/// deliberately so: a campfire's renderer draws item *models*, not a cuboid part
/// rig, so this feeds the model pipeline through
/// [`crate::entity::campfire_item_mesh`] the way a dropped item does — see
/// [`campfire_item_matrix`]'s doc for why there is no mesh and no sheet on this
/// path at all. Sending it through `resolve_*` would need a texture stem that
/// does not exist.
///
/// One per **occupied** slot, so a campfire holding two steaks yields two of
/// these and an empty campfire yields none — matching
/// `CampfireRenderer.submit`'s `if (!itemState.isEmpty())`.
#[derive(Debug, Clone, PartialEq)]
pub struct CampfireItemSpawn {
    /// Block position of the campfire.
    pub pos: [i32; 3],
    /// The campfire block's `facing`, in [`horizontal_facing_yaw`]'s convention.
    pub facing_yaw_deg: f32,
    /// Which of the four cooking slots (`0..CAMPFIRE_SLOTS`) this item is in.
    /// Vanilla offsets it by the facing, so this is *not* a world corner —
    /// see [`campfire_item_matrix`].
    pub slot: usize,
    /// The item id whose baked geometry to draw, from the block entity's NBT
    /// `Items` list.
    pub item: ResourceLocation,
    /// Packed sky/block light at the campfire.
    pub light: u8,
}

/// One suspicious sand/gravel block's revealed item, for this frame —
/// vanilla's `BrushableBlockRenderer`.
///
/// **A second `*Spawn` here [`BlockEntityModelSet`] does not resolve**, for the
/// same reason [`CampfireItemSpawn`] is the first: `BrushableBlockRenderer`
/// draws an *item model*, not a cuboid part rig — the sand/gravel a player
/// sees is the ordinary **block** model, real geometry the terrain mesher
/// already draws (`suspicious_sand`/`suspicious_gravel` are not a hole in the
/// world), so this feeds the model pipeline through
/// [`crate::entity::brushable_item_mesh`] the way a dropped item does.
///
/// Present only once **both** `BrushableBlockEntity.getHitDirection()` is
/// non-null (a player has brushed at least once) and `item` is non-empty (a
/// loot table has actually rolled a reward) and `dustProgress > 0` —
/// vanilla's own three-part guard in `BrushableBlockRenderer.submit`. A brand
/// new, never-brushed block therefore contributes no spawn at all.
#[derive(Debug, Clone, PartialEq)]
pub struct BrushableItemSpawn {
    /// Block position of the suspicious sand/gravel.
    pub pos: [i32; 3],
    /// The face a player last brushed, from `hit_direction` NBT
    /// (`Direction.LEGACY_ID_CODEC`) — feeds [`brushable_item_matrix`].
    pub hit_direction: lodestone_assets::Direction,
    /// The block state's own `dusted` property, `0..=3` —
    /// `BrushableBlockEntity.getCompletionState()`'s range, read off the
    /// state rather than re-derived from `brushCount` (which is not on the
    /// wire; only the property is).
    pub dust_progress: u8,
    /// The revealed item's id, from the block entity's `item` NBT
    /// (`ItemStack.CODEC`).
    pub item: ResourceLocation,
    /// Packed sky/block light at the block.
    pub light: u8,
}

/// One vault's floating display-item cluster, for this frame — vanilla's
/// `VaultRenderer`.
///
/// **A third `*Spawn` here [`BlockEntityModelSet`] does not resolve**, for the
/// same reason [`CampfireItemSpawn`] is the first: `VaultRenderer.submit` is
/// `ItemEntityRenderer.renderMultipleFromCount` at a fixed pose, not a cuboid
/// part rig — the vault's own cage, base and door are all real *block* model
/// geometry the ordinary terrain mesher already draws (`blockstates/vault.json`
/// is a plain `variants` map over `facing`/`ominous`/`vault_state`, the same
/// shape the mob-spawner cage and trial-spawner's per-state textures already
/// proved), so this feeds the model pipeline through
/// [`crate::entity::vault_display_item_mesh`] the way a dropped item does.
///
/// Present only when `VaultBlockEntity.Client.shouldDisplayActiveEffects`
/// (`sharedData.hasDisplayItem()`) is true — an empty `shared_data.display_item`
/// yields **no** spawn for that vault, matching vanilla's own
/// `!displayItem.isEmpty()` guard in `VaultRenderer.extractRenderState`. A
/// vault the server has not yet rolled a reward for (state `INACTIVE`) is
/// therefore silent, not a partially-drawn cluster.
#[derive(Debug, Clone, PartialEq)]
pub struct VaultSpawn {
    /// Block position of the vault.
    pub pos: [i32; 3],
    /// The display item's id, from `shared_data.display_item.id`.
    pub item: ResourceLocation,
    /// The display item's stack count, from `shared_data.display_item.count`
    /// (vanilla's codec defaults this to `1` when absent) — feeds
    /// [`crate::entity::rendered_amount`] the same way a dropped stack's count
    /// does.
    pub count: u32,
    /// This frame's spin, in degrees — [`crate::entity::vault_spin_degrees`]
    /// evaluated at the gather's `(game_time, partial_tick)`, already resolved
    /// so the draw site needs no clock of its own.
    pub spin_deg: f32,
    /// Packed sky/block light at the vault.
    pub light: u8,
}

/// One `moving_piston` block entity for this frame — vanilla's
/// `PistonHeadRenderer`.
///
/// **The second `*Spawn` here [`BlockEntityModelSet`] does not resolve**, and for
/// the same reason [`CampfireItemSpawn`] is the first: `PistonHeadRenderer`'s
/// constructor calls no `bakeLayer`, so it owns no cuboid rig. What it draws is
/// whole *block models* posed somewhere other than their own cell, which is the
/// moving-block seam (`gpu/moving_blocks.rs` in the shell) rather than either the
/// entity or the item pipeline.
///
/// # Everything here is semantic, not a matrix
///
/// The offset is deliberately **not** precomputed into a `Mat4` by the gather.
/// `getExtendedProgress` is the one piece of arithmetic in this renderer that a
/// plausible reading gets backwards — it is `progress - 1.0` while extending and
/// `1.0 - progress` while retracting, and the two agree at `progress == 0.5` — so
/// it lives next to its sibling `falling_block_pose` where its wrong hypothesis is
/// evaluated against it. Carrying `direction`/`progress`/`extending` keeps that
/// possible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovingPistonSpawn {
    /// The cell the `moving_piston` block entity itself occupies. Geometry draws
    /// at this cell **plus** the offset derived from the three fields below; the
    /// cell itself has no block model (`moving_piston` is
    /// `RenderShape.INVISIBLE`), which is why an unset source leaves a hole.
    pub pos: [i32; 3],
    /// The global block-state id to draw offset — already resolved by the gather,
    /// because two of `extractRenderState`'s three branches *synthesise* a state
    /// rather than using the stored one (a `piston_head` with `short` set from the
    /// progress, in particular).
    pub state_id: u32,
    /// The retracting **source** piston's own base, drawn at [`Self::pos`] with
    /// no offset at all — `submit` pops the translated pose before submitting it.
    /// `None` for every other case, which is the common one.
    pub base_state_id: Option<u32>,
    /// `PistonMovingBlockEntity.direction`'s unit step.
    ///
    /// **Not the movement direction.** `getMovementDirection()` is `extending ?
    /// direction : direction.getOpposite()`, but `getXOff`/`getYOff`/`getZOff`
    /// multiply the *raw* `direction` step by a signed progress that carries the
    /// retraction's sign itself. Using the movement direction here and a positive
    /// progress would double-negate the retracting case.
    pub direction: [i32; 3],
    /// `getProgress(partialTick)` — `lerp(a, progressO, progress)`, in `0..=1`.
    pub progress: f32,
    /// `PistonMovingBlockEntity.extending`.
    pub extending: bool,
    /// Packed sky/block light for the offset geometry. Vanilla samples it one cell
    /// **back** along the movement direction
    /// (`getBlockPos().relative(getMovementDirection().getOpposite())`), not at the
    /// block entity's own cell — the cell being moved *into* is the one full of
    /// `moving_piston`.
    pub light: u8,
    /// Packed sky/block light for [`Self::base_state_id`], sampled at
    /// [`Self::pos`] itself. Ignored when there is no base.
    pub base_light: u8,
}

/// Which block-entity rig and sheet draw one `minecraft:special` **item** form —
/// vanilla's `SpecialModelRenderer` family, the ex-`builtin/entity` items.
///
/// `kind` is the special-renderer id an item definition names
/// (`lodestone_assets::IconPart::Special::kind`); `item_path` is the item's own
/// registry path with the namespace stripped. Returns `(model name, texture stem)`
/// — the same two keys a placed block entity is batched on, because it is the same
/// rig and the same sheet.
///
/// # One resolver, every surface
///
/// This exists so a chest's rig is chosen in exactly **one** place for all five
/// surfaces vanilla draws these items on: the inventory slot, the first-person
/// hand, another entity's hand, a dropped stack, and an item frame. Two copies of
/// the mapping is how a chest ends up correct in the GUI and oak-coloured in the
/// hand — the copies differ in whichever `kind` was added later.
///
/// # Keyed by `kind` first and item path second, never by item path alone
///
/// The family is ten `kind`s over 91 item definitions. A `match` on the item id
/// alone would need 91 arms and would leave any datapack item invisible; the
/// `kind` says *what rig*, and the path only picks *which sheet* within it —
/// exactly vanilla's split, where `ChestSpecialRenderer.Unbaked` carries the
/// `texture` field the item definition names.
///
/// `ChestHalf::Single` is not a simplification: `ChestSpecialRenderer.Unbaked`'s
/// `chest_type` defaults to `ChestType.SINGLE` and no 26.2 item definition
/// overrides it, so an item chest is never one of the two double halves.
///
/// # Which kinds resolve today
///
/// `chest` (13 item definitions), `shulker_box` (17), `head` (6),
/// `player_head` (1) and `shield` (2, undyed/pattern-less only — see below) —
/// every `kind` whose rig `BLOCK_ENTITY_MODELS` already holds. The other five
/// return `None` and draw nothing, which is the behaviour before this
/// existed:
///
/// | kind | items | why not yet |
/// |---|---|---|
/// | `banner` | 16 | needs the ordered translucent pattern-mask pass, not one rig |
/// | `conduit` | 1 | rig unported (`ConduitRenderer` calls `bakeLayer` four times) |
/// | `copper_golem_statue` | 32 | rig unported |
/// | `decorated_pot` | 1 | needs up to four independently textured sprites per instance |
/// | `trident` | 2 | rig unported |
///
/// **`shield` also resolves here now, but only ever as the undyed,
/// pattern-less rig.** The first-person hand and the GUI icon both bypass
/// this function and call [`shield_item_rig`] directly, because *they* carry
/// real per-stack state (`minecraft:base_color`, `minecraft:banner_patterns`)
/// this function's `(kind, item_path)` signature has no room for — see that
/// pair's own call sites (`lodestone_shell::gpu::first_person`'s
/// `prepare_special_hand`, `lodestone_shell::hud::item_icon`'s GUI-icon
/// pass). But this resolver's three callers (a dropped stack, another
/// entity's hand, an item frame) had **no** shield arm at all until this one
/// landed — `_ => None` swallowed every one of them, so a dropped or framed
/// shield drew nothing, full stop, not merely undyed. Resolving it here to
/// the no-pattern sheet unconditionally is the same *bounded* shortfall this
/// module already accepts elsewhere on these three surfaces (a dropped
/// stack's own doc: "no stack multiplication"; a framed item's own doc: "the
/// in-frame rotation is undecoded") — a real shield reaching real pixels,
/// just not its dye or loom pattern, because neither surface threads that
/// state through `EntityDraw` today.
///
/// `None` is also the right answer for an item path a `kind` does not recognise (a
/// datapack item declaring `minecraft:chest` over something that is not a chest):
/// drawing nothing beats drawing a plain oak chest for it. And `dragon_head`/
/// `piglin_head` fall out here too, because [`SkullType::from_block_path`] declines
/// them — they use multi-part rigs unrelated to the two skull layers.
#[must_use]
pub fn special_item_rig(kind: &str, item_path: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "minecraft:chest" => {
            let material = ChestMaterial::from_block_path(item_path)?;
            Some((
                CHEST_SINGLE,
                chest_texture_stem(material, ChestHalf::Single),
            ))
        }
        "minecraft:shulker_box" => {
            // `shulker_box` (the undyed one) has no colour prefix and takes the
            // default sheet; every other path is `<colour>_shulker_box`. Passing
            // the whole path through would silently take the default arm for all
            // seventeen, which is the plausible wrong version: it draws, and it
            // draws purple.
            let colour = item_path.strip_suffix("_shulker_box").filter(|c| {
                // Only a real dye colour — a datapack `foo_shulker_box` should
                // not quietly become the default sheet.
                SHULKER_COLOURS.contains(c)
            });
            if colour.is_none() && item_path != "shulker_box" {
                return None;
            }
            Some((SHULKER_BOX, shulker_texture_stem(colour)))
        }
        // Two `kind`s, one rig family: vanilla splits `player_head` out because its
        // renderer resolves a profile texture, which we do not fetch here — a
        // player head therefore draws the default Steve sheet, exactly as a placed
        // one does.
        "minecraft:head" | "minecraft:player_head" => {
            let ty = SkullType::from_block_path(item_path)?;
            Some((ty.model(), skull_texture_stem(ty)))
        }
        // Always the no-pattern sheet — see this function's own doc for why
        // a dyed or patterned shield still only reaches pixels through the
        // hand/GUI call sites that bypass this resolver entirely.
        "minecraft:shield" => Some((SHIELD, SHIELD_BASE_NO_PATTERN_TEXTURE_STEM)),
        _ => None,
    }
}

/// The `minecraft:banner` item rig — [`special_item_rig`]'s own doc table lists
/// this `kind` as one of the six that resolve to `None` ("needs the ordered
/// translucent pattern-mask pass, not one rig"), which was the honest state the
/// day that table was written and is also the reason a banner drew nothing at
/// all in a hotbar slot, an inventory slot or the first-person hand — not a
/// missing draw call, a `kind` this dispatcher never recognised.
///
/// # Two landings, and this is the second
///
/// The first landing multiplied the base colour directly into the opaque flag
/// texture — an approximation, disclosed at the time: vanilla's own opaque
/// body+flag pass (`Sheets.BANNER_BASE`, the plain wood/cloth sheet
/// [`BANNER_BASE_TEXTURE_STEM`] names) is drawn **untinted**, and everything
/// the player perceives as colour is a *second*, translucent
/// `submitPatterns` draw layered over it — first the base mask tinted by the
/// base colour, then up to 16 loom pattern masks, each its own draw
/// (`BannerRenderer.submitPatterns`). A single flat tint cannot show a
/// pattern at all.
///
/// Now that `minecraft:banner_patterns` decodes to a real, typed value (see
/// [`lodestone_model::ItemComponents::banner_patterns`]) and a caller can
/// derive the same ordered mask list a placed banner uses
/// (`crate::banner_pattern::banner_pattern_layers`, fed by
/// [`banner_item_base_color`] plus the decoded patterns), this rig's own two
/// meshes draw **untinted**, exactly like
/// [`BlockEntityModelSet::resolve_banner`]'s own `body`/`flag` — matching
/// vanilla's opaque pass exactly, with every bit of colour riding the
/// caller's own translucent layer draws instead. Reusing the field name
/// `flag_tint` for "no tint" would have been the same defaulting trap
/// `DESIGN.md` §12 records for a trait method with a `true` default: the
/// safer shape is a rig with no colour field to forget to multiply in twice.
///
/// Returns `None` for an item path that is not `<dye>_banner` — a shield, or a
/// datapack item naming this `kind` over something else.
#[must_use]
pub fn banner_item_rig(item_path: &str) -> Option<BannerItemRig> {
    banner_item_base_color(item_path)?;
    Some(BannerItemRig {
        body: (BANNER_BODY, BANNER_BASE_TEXTURE_STEM),
        flag: (BANNER_FLAG, BANNER_BASE_TEXTURE_STEM),
    })
}

/// The base dye colour parsed from a banner item's own path (`<dye>_banner`,
/// via [`crate::banner_pattern::DyeColor::from_name`]) — factored out of
/// [`banner_item_rig`] so a caller building the *translucent* pattern-layer
/// draws (base mask plus every loom pattern, via
/// [`crate::banner_pattern::banner_pattern_layers`]) derives the same base
/// colour [`banner_item_rig`] validated, rather than re-parsing the item path
/// a second, potentially diverging way. `None` for the same inputs
/// [`banner_item_rig`] rejects.
#[must_use]
pub fn banner_item_base_color(item_path: &str) -> Option<crate::banner_pattern::DyeColor> {
    let name = item_path.strip_suffix("_banner")?;
    crate::banner_pattern::DyeColor::from_name(name)
}

/// [`banner_item_rig`]'s result: two opaque draws sharing one placement, both
/// **untinted** — see that function's doc for why colour no longer lives
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BannerItemRig {
    /// `(model, texture)` for the pole/bar.
    pub body: (&'static str, &'static str),
    /// `(model, texture)` for the flag — the same mesh a caller's translucent
    /// pattern-layer draws paint over.
    pub flag: (&'static str, &'static str),
}

// A skull item's own placement, deliberately, is **not** the world skull
// functions' `scale(-1, -1, 1)`. An earlier version of this file added exactly
// that flip here, reasoning from `skull_ground_placement_matrix`'s own doc
// (`SkullModel`'s head box is authored Y-down) without first checking the real
// *item*-context renderer. It was wrong, caught before landing by reading
// `SkullSpecialRenderer.submit`/`PlayerHeadSpecialRenderer.submit`
// (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/special/`): both
// call `SkullBlockRenderer.submitSkull` directly, with **no** placement
// transformation at all — the ground/wall flip is applied only by
// `SkullBlockRenderer.submit`, the *block-entity* path, via
// `poseStack.mulPose(state.transformation)` before it ever reaches
// `submitSkull`. So the item path really does draw the raw Y-down
// `SkullModel` cube (`addBox(-4, -8, -4, 8, 8, 8)`, matching this crate's own
// baked `SKULL_HUMANOID`/`SKULL_MOB` AABB exactly) through nothing but the
// ordinary `ItemTransform.apply` chain every other special rig uses.
//
// The GUI icon and held player head *do* still measure off-centre against a
// naive full-cell prediction, and that has a real cause, just not this one:
// `assets/minecraft/items/player_head.json` puts a **second** transformation —
// `translation: [0.5, 0.0, 0.5]` plus a 180°-about-X `left_rotation` — on the
// `minecraft:special` model *node itself*
// (`SpecialModelWrapper.Unbaked.bake`: `Transformation.compose(displayTransform,
// this.transformation)`). That gap is closed: `ItemModelNode::Special` carries
// the whole root-to-node `"transformation"` chain and every consumer folds it
// through `compose_special_node_transform`.

/// One resolved block entity, ready to batch.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockEntityInstance {
    /// Model name (the mesh key).
    pub model: &'static str,
    /// Texture stem (the bind-group key).
    pub texture: &'static str,
    /// The placement matrix (block → world).
    pub transform: Mat4,
    /// One world matrix per part, in mesh part order.
    pub part_transforms: Vec<Mat4>,
    /// World AABB minimum, for culling.
    pub aabb_min: Vec3,
    /// World AABB maximum, for culling.
    pub aabb_max: Vec3,
    /// Packed sky/block light.
    pub light: u8,
    /// Gamma-space `[r, g, b]` multiplied into the texel — `[255, 255, 255]`
    /// (`entity_pipeline::NO_TINT`'s rgb half) for "leave the texel alone".
    ///
    /// Per-instance tint already exists end to end for entities (sheep wool,
    /// dyed armour, the hurt overlay, the creeper flash all go through
    /// [`crate::entity_pipeline::EntityInstanceRaw::tint`]/[`InstanceTint`]) —
    /// this is that same plumbing reaching block entities. Every resolver in
    /// this module passes `[255, 255, 255]` today (no block-entity type here
    /// is tinted yet); a future banner base-colour or shulker-box dye reads
    /// this field instead of widening the pipeline.
    pub tint: [u8; 3],
}

/// One resolved translucent pattern-mask layer, ready for a caller to draw
/// directly through `EntityPipeline::banner_layer_pipeline` — the "small
/// separate ordered draw list" `docs/banner-shield-patterns.md` calls for.
///
/// **Deliberately not batched.** These draws are translucent and
/// depth-write-off, so they must submit in the item's own stored order —
/// [`plan_block_entities`]'s `(model, texture)` batching would let two
/// banners reusing the same two sprites in opposite orders interleave
/// incorrectly. Banners are rare, so a handful of unbatched draw calls per
/// banner costs nothing; a caller draws [`BannerInstances::layers`] in
/// order, one instance per draw.
#[derive(Debug, Clone, PartialEq)]
pub struct BannerLayerDraw {
    /// World transform for the flag part this layer paints over — the same
    /// value for every layer of one banner, since masks paint over the
    /// (posed, swaying) flag, never the pole/bar.
    pub transform: Mat4,
    /// The mask sprite to sample: `entity/banner/base` for the always-present
    /// base layer, then `entity/banner/<pattern-asset-id>` per stored
    /// pattern, in the item's own order. See
    /// [`crate::banner_pattern::PatternLayer::sprite`]'s doc — this is a full
    /// [`ResourceLocation`], not a bare asset id.
    pub sprite: ResourceLocation,
    /// Gamma-space `[r, g, b]` in `0.0..=1.0` to tint this layer's sampled
    /// texel by (see `crate::banner_pattern`'s gamma-space note — **do not**
    /// convert this to linear before multiplying the sampled texel).
    pub color: [f32; 3],
    /// Packed sky/block light — identical to the flag's own.
    pub light: u8,
}

/// Everything one ground/standing banner draws this frame: the opaque
/// body/flag (through the ordinary [`plan_block_entities`] batcher, same as
/// every other block-entity type in this module) plus the ordered,
/// translucent pattern-layer draw list (drawn separately, through
/// `EntityPipeline::banner_layer_pipeline`, in order) — see
/// [`BlockEntityModelSet::resolve_banner`]'s doc for the full draw-order
/// derivation.
#[derive(Debug, Clone, PartialEq)]
pub struct BannerInstances {
    /// The pole+bar, opaque, `entity/banner/banner_base`.
    pub body: BlockEntityInstance,
    /// The flag, opaque, same sheet as `body` — the plain cloth/wood pass,
    /// *not* a pattern. Already posed with this frame's sway.
    pub flag: BlockEntityInstance,
    /// The base-colour mask plus every stored pattern layer, in draw order —
    /// `1 + patterns.len().min(MAX_PATTERN_LAYERS)` entries, never empty
    /// (the base layer always draws, even with zero stored patterns).
    pub layers: Vec<BannerLayerDraw>,
}

/// One draw batch: every instance sharing a model **and** a texture.
///
/// Both keys matter. Batching on the model alone would draw a trapped chest with
/// a plain chest's sheet, because the mesh is identical and only the bind group
/// differs — a bug that looks like a texture-loading failure, not a batching
/// one.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockEntityBatch {
    /// Model name.
    pub model: &'static str,
    /// Texture stem.
    pub texture: &'static str,
    /// `parts[p][i]` is part `p` of instance `i` — the same per-part instance
    /// layout the entity pass uses, because vertices are part-local and a lid
    /// only moves if its own matrices are uploaded.
    pub parts: Vec<Vec<Mat4>>,
    /// Packed light per instance.
    pub lights: Vec<u32>,
    /// Per-instance tint, lockstep with [`lights`](Self::lights) — every part
    /// of one instance shares its tint, so this lives once per instance
    /// rather than once per `parts` slot. Fed to
    /// [`crate::entity_pipeline::upload_instances_tinted`] the same way
    /// entity draws already are; a short/missing entry falls back to
    /// [`InstanceTint::NONE`], matching that function's own lockstep-fallback
    /// contract for `lights`.
    pub tints: Vec<InstanceTint>,
}

impl BlockEntityBatch {
    /// Number of instances in this batch.
    #[must_use]
    pub fn count(&self) -> u32 {
        self.lights.len() as u32
    }
}

/// Culling counters for one frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockEntityCullStats {
    /// Instances offered.
    pub total: usize,
    /// Instances that survived the frustum.
    pub drawn: usize,
    /// Instances rejected by the frustum.
    pub culled_frustum: usize,
}

/// Everything the block-entity pass draws this frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockEntityFrame {
    /// Batches, keyed by `(model, texture)`.
    pub batches: Vec<BlockEntityBatch>,
    /// Culling counters.
    pub stats: BlockEntityCullStats,
}

/// Frustum-culls and batches resolved instances.
#[must_use]
pub fn plan_block_entities(
    instances: &[BlockEntityInstance],
    frustum: &Frustum,
) -> BlockEntityFrame {
    let mut batches: Vec<BlockEntityBatch> = Vec::new();
    let mut stats = BlockEntityCullStats {
        total: instances.len(),
        ..BlockEntityCullStats::default()
    };
    for inst in instances {
        if !frustum.intersects_aabb(inst.aabb_min, inst.aabb_max) {
            stats.culled_frustum += 1;
            continue;
        }
        stats.drawn += 1;
        match batches
            .iter_mut()
            .find(|b| b.model == inst.model && b.texture == inst.texture)
        {
            Some(batch) => {
                batch.lights.push(u32::from(inst.light));
                batch.tints.push(InstanceTint::rgb(inst.tint));
                for (slot, m) in batch.parts.iter_mut().zip(&inst.part_transforms) {
                    slot.push(*m);
                }
            }
            None => batches.push(BlockEntityBatch {
                model: inst.model,
                texture: inst.texture,
                parts: inst.part_transforms.iter().map(|m| vec![*m]).collect(),
                lights: vec![u32::from(inst.light)],
                tints: vec![InstanceTint::rgb(inst.tint)],
            }),
        }
    }
    BlockEntityFrame { batches, stats }
}

/// Conservative world AABB of a local box under `m`, by transforming all eight
/// corners.
fn transformed_aabb(m: &Mat4, local_min: Vec3, local_max: Vec3) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for i in 0..8 {
        let corner = Vec3::new(
            if i & 1 == 0 { local_min.x } else { local_max.x },
            if i & 2 == 0 { local_min.y } else { local_max.y },
            if i & 4 == 0 { local_min.z } else { local_max.z },
        );
        let world = m.transform_point3(corner);
        min = min.min(world);
        max = max.max(world);
    }
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;

    fn set() -> BlockEntityModelSet {
        BlockEntityModelSet::load()
    }

    /// The enchanting table's openness is the *live* form of the expression the
    /// lectern's is the frozen case of, and the two must not be confused.
    ///
    /// Predicted from outside constants: at `open == 1.2` (the lectern's fourth
    /// argument) and `time == 0` this must land on exactly `1.5`, the lectern's
    /// own value — that is the arithmetic identity the two share. Away from
    /// `time == 0` it must **leave** `1.5`, and by a predicted amount: the term is
    /// `sin(time * 0.02) * 0.1 * open`, so at `time = PI/2 / 0.02` the `sin` is
    /// exactly `1` and the result is `1.35 * 1.2 == 1.62`.
    ///
    /// A sign-only "it changes with time" assertion is satisfied by any live term
    /// at all, including one that varies `10×` too fast, which is what dropping
    /// the `0.02` produces.
    #[test]
    fn the_book_openness_breathes_around_the_lecterns_frozen_value() {
        let at_zero = enchanting_table_book_openness(0.0, 1.2);
        assert!(
            (at_zero - LECTERN_BOOK_OPENNESS).abs() < 1e-5,
            "time 0 gives {at_zero}, expected the lectern's {LECTERN_BOOK_OPENNESS}"
        );
        let peak_time = std::f32::consts::FRAC_PI_2 / 0.02;
        let at_peak = enchanting_table_book_openness(peak_time, 1.2);
        assert!(
            (at_peak - 1.62).abs() < 1e-4,
            "peak gives {at_peak}, expected 1.62 (1.35 * 1.2)"
        );
        // The trough is the mirror image, `1.15 * 1.2`.
        let trough = enchanting_table_book_openness(-peak_time, 1.2);
        assert!(
            (trough - 1.38).abs() < 1e-4,
            "trough gives {trough}, expected 1.38 (1.15 * 1.2)"
        );
    }

    /// The two page-flip phases are **half a period apart**, so the pages turn
    /// alternately, and both are clamped into `0..1`.
    ///
    /// The predicate that matters is "how far apart", not "are they different":
    /// two phases that differ by any amount at all satisfy the weak version, and
    /// reading both offsets as `0.25` (an easy transcription slip, since the
    /// expressions are otherwise identical) leaves them differing by zero — but
    /// so does *any* pair of offsets during the clamped stretches. So this asserts
    /// the unclamped interior: at `flip == 0` the raw values are
    /// `frac(0.25) * 1.6 - 0.3 == 0.1` and `frac(0.75) * 1.6 - 0.3 == 0.9`, i.e.
    /// exactly the lectern's own constant pair.
    #[test]
    fn the_two_page_flips_are_half_a_period_apart_and_clamped() {
        let (a, b) = enchanting_table_page_flips(0.0);
        assert!(
            (a - LECTERN_BOOK_PAGE_FLIP.0).abs() < 1e-5
                && (b - LECTERN_BOOK_PAGE_FLIP.1).abs() < 1e-5,
            "flip 0 gives ({a}, {b}), expected the lectern's {LECTERN_BOOK_PAGE_FLIP:?}"
        );
        // Half a period on: the two swap roles rather than both advancing.
        let (a, b) = enchanting_table_page_flips(0.5);
        assert!(
            (a - LECTERN_BOOK_PAGE_FLIP.1).abs() < 1e-5
                && (b - LECTERN_BOOK_PAGE_FLIP.0).abs() < 1e-5,
            "flip 0.5 gives ({a}, {b}), expected the pair swapped"
        );
        // The clamp fires, and it fires in both directions across one period —
        // the control for the clamp being load-bearing rather than decorative.
        let mut saw_low = false;
        let mut saw_high = false;
        for step in 0..200 {
            let flip = step as f32 / 100.0;
            let (a, b) = enchanting_table_page_flips(flip);
            for value in [a, b] {
                assert!(
                    (0.0..=1.0).contains(&value),
                    "flip {flip} produced {value}, outside 0..1"
                );
                saw_low |= value == 0.0;
                saw_high |= value == 1.0;
            }
        }
        assert!(
            saw_low && saw_high,
            "the clamp never fired across a full period, so it is untested here"
        );
    }

    /// An enchanting table's book is tilted `80°`, hovers, and turns about a
    /// *simulated* angle — three things that separate it from the lectern's book,
    /// which shares its every vertex.
    ///
    /// The tilt is asserted by where the book's local `+Y` ends up rather than by
    /// restating the constant: at `80°` about `Z` the local up axis leans
    /// `sin(80°) = 0.985` along world `-X` and keeps only `cos(80°) = 0.174` of
    /// its height. The lectern's `67.5°` would give `0.924`/`0.383`, so the two
    /// are separated by far more than any tolerance here.
    #[test]
    fn the_enchanting_table_book_is_tilted_eighty_degrees_and_hovers() {
        const POS: [i32; 3] = [3, 64, 7];
        let m = enchanting_table_book_placement_matrix(POS, 0.0, 0.0);
        let up = m.transform_vector3(Vec3::Y);
        assert!(
            (up.y - ENCHANTING_TABLE_BOOK_TILT_DEG.to_radians().cos()).abs() < 1e-4,
            "local up ends at {up:?}; a 67.5 degree tilt would keep 0.383 of its height"
        );
        // The hover: ±0.01 blocks about `0.75 + 0.1`, and its extremes are a
        // quarter period of `time * 0.1` apart.
        let base = Vec3::new(POS[0] as f32 + 0.5, POS[1] as f32 + 0.85, POS[2] as f32 + 0.5);
        let rest = m.transform_point3(Vec3::ZERO);
        assert!(
            rest.distance(base) < 1e-5,
            "rest origin {rest:?}, expected {base:?}"
        );
        let peak_time = std::f32::consts::FRAC_PI_2 / 0.1;
        let peak = enchanting_table_book_placement_matrix(POS, 0.0, peak_time)
            .transform_point3(Vec3::ZERO);
        assert!(
            (peak.y - (base.y + 0.01)).abs() < 1e-5,
            "hover peak at {}, expected {}",
            peak.y,
            base.y + 0.01
        );
    }

    /// The book turns about `y_rot` in **radians**, not degrees.
    ///
    /// Both hypotheses computed: a quarter turn is `PI/2` radians, and the same
    /// number read as degrees is `1.57°` — under two degrees, which on a book a
    /// few texels wide is visually indistinguishable from no rotation at all. So
    /// the assertion is that a `PI/2` input really does move the book's local `+Z`
    /// a full quarter turn, and that the degrees reading would leave it within
    /// `0.001` of where it started.
    ///
    /// Local `+Z` and **not** `+X`, and that is not arbitrary: the `80°` tilt
    /// about `Z` leaves local `Z` alone but stands local `X` almost vertical
    /// (`sin(80°) = 0.985` of it), and a Y rotation barely moves a near-vertical
    /// axis — measured, on the first run of this test, as `14°` for a genuine
    /// quarter turn. Picking the wrong probe axis here fails a correct
    /// implementation.
    #[test]
    fn the_book_yaw_is_radians_not_degrees() {
        const POS: [i32; 3] = [0, 0, 0];
        let rest = enchanting_table_book_placement_matrix(POS, 0.0, 0.0)
            .transform_vector3(Vec3::Z)
            .normalize();
        let quarter = enchanting_table_book_placement_matrix(POS, std::f32::consts::FRAC_PI_2, 0.0)
            .transform_vector3(Vec3::Z)
            .normalize();
        assert!(
            rest.dot(quarter).abs() < 1e-4,
            "PI/2 turned the book by {} degrees, not 90",
            rest.dot(quarter).acos().to_degrees()
        );
        let as_degrees =
            enchanting_table_book_placement_matrix(POS, std::f32::consts::FRAC_PI_2.to_radians(), 0.0)
                .transform_vector3(Vec3::Z)
                .normalize();
        assert!(
            rest.dot(as_degrees) > 0.999,
            "the degrees reading must be a near-no-op, and measured {}",
            rest.dot(as_degrees)
        );
    }

    /// The four cooking slots land in four **distinct** corners of the campfire's
    /// own block, clockwise seen from above, every one lifted onto its top face.
    ///
    /// Predicted from the pose stack, not read off the implementation: the
    /// pre-yaw offset is `Rx(90°) · (-0.3125, -0.3125, 0) = (-0.3125, 0, -0.3125)`,
    /// so a south-facing campfire's slot 0 sits at `(0.1875, 0.44921875, 0.1875)`
    /// and each further slot turns that a quarter turn about the block centre. A
    /// "four items somewhere on the campfire" assertion would accept all four
    /// stacked in one corner, which is what dropping the yaw term produces.
    #[test]
    fn the_four_campfire_slots_land_in_four_distinct_corners() {
        const POS: [i32; 3] = [10, 64, -3];
        let base = Vec3::new(POS[0] as f32, POS[1] as f32, POS[2] as f32);
        let expected = [
            Vec3::new(0.1875, CAMPFIRE_ITEM_LIFT, 0.1875),
            Vec3::new(0.8125, CAMPFIRE_ITEM_LIFT, 0.1875),
            Vec3::new(0.8125, CAMPFIRE_ITEM_LIFT, 0.8125),
            Vec3::new(0.1875, CAMPFIRE_ITEM_LIFT, 0.8125),
        ];
        for slot in 0..CAMPFIRE_SLOTS {
            let origin = campfire_item_matrix(POS, 0.0, slot).transform_point3(Vec3::ZERO);
            let want = base + expected[slot];
            assert!(
                origin.distance(want) < 1e-5,
                "slot {slot} pose origin {origin:?}, expected {want:?}"
            );
        }
    }

    /// `translations()`'s per-direction override, predicted from the real jar's
    /// arithmetic rather than restated: at `dust_progress = 3` (the maximum,
    /// `BrushableBlockEntity.getCompletionState()`'s own ceiling),
    /// `completionOffset = 3 / 10.0 * 0.75 = 0.225`, so an `EAST` hit pushes the
    /// item's `x` to `0.73 + 0.225 = 0.955` and leaves `y`/`z` at the base
    /// `0.0`/`0.5`.
    #[test]
    fn brushable_offset_moves_outward_along_the_hit_face() {
        use lodestone_assets::Direction;
        let offset = brushable_item_offset(Direction::East, 3);
        assert!(
            (offset.x - 0.955).abs() < 1e-5,
            "east offset.x = {}, expected 0.955",
            offset.x
        );
        assert!((offset.y - 0.0).abs() < 1e-5);
        assert!((offset.z - 0.5).abs() < 1e-5);

        let west = brushable_item_offset(Direction::West, 3);
        assert!(
            (west.x - 0.025).abs() < 1e-5,
            "west offset.x = {}, expected 0.025 (0.25 - 0.225)",
            west.x
        );
    }

    /// Every hit direction moves the item to a **distinct** point at the same
    /// `dust_progress`, and the outward distance grows monotonically with
    /// progress — the two properties `BrushableBlockRenderer.translations`
    /// exists to give: which face, and how far the dig has revealed the item.
    #[test]
    fn brushable_offset_differs_per_direction_and_grows_with_progress() {
        use lodestone_assets::Direction;
        const DIRS: [Direction; 6] = [
            Direction::Down,
            Direction::Up,
            Direction::West,
            Direction::North,
            Direction::East,
            Direction::South,
        ];
        let mut at_zero = Vec::new();
        for d in DIRS {
            let o = brushable_item_offset(d, 0);
            assert!(
                at_zero.iter().all(|p: &Vec3| p.distance(o) > 1e-4),
                "direction {d:?} collided with an earlier direction at {o:?}"
            );
            at_zero.push(o);
        }
        for d in DIRS {
            let near = BRUSHABLE_ITEM_BASE_OFFSET.distance(brushable_item_offset(d, 1));
            let far = BRUSHABLE_ITEM_BASE_OFFSET.distance(brushable_item_offset(d, 3));
            assert!(
                far > near,
                "{d:?}: distance from base did not grow with dust_progress ({near} -> {far})"
            );
        }
    }

    /// The extra `Ry` term is `11°` on north/south/up/down and `101°` on
    /// east/west — `(eastWest ? 90 : 0) + 11`, predicted rather than restated.
    /// Told apart via the local `+Z` axis after the fixed `75°` turn is undone
    /// algebraically: composing the matrix's own rotation out would just
    /// restate the code under test, so this instead checks the two east/west
    /// results agree with each other and disagree with the four others, which
    /// a dropped `eastWest` branch (always `+11°`) cannot produce.
    #[test]
    fn brushable_matrix_turns_an_extra_ninety_degrees_on_the_horizontal_axis() {
        use lodestone_assets::Direction;
        const POS: [i32; 3] = [5, 70, 5];
        let east = brushable_item_matrix(POS, Direction::East, 2);
        let west = brushable_item_matrix(POS, Direction::West, 2);
        let north = brushable_item_matrix(POS, Direction::North, 2);
        let south = brushable_item_matrix(POS, Direction::South, 2);
        let east_x = east.transform_vector3(Vec3::X).normalize();
        let west_x = west.transform_vector3(Vec3::X).normalize();
        let north_x = north.transform_vector3(Vec3::X).normalize();
        let south_x = south.transform_vector3(Vec3::X).normalize();
        assert!(
            east_x.dot(west_x) > 0.999,
            "east/west should share the same +90 extra turn"
        );
        assert!(
            north_x.dot(south_x) > 0.999,
            "north/south should share the same +0 extra turn"
        );
        assert!(
            east_x.dot(north_x) < 0.999,
            "east/west and north/south must differ by the extra 90 degrees"
        );
    }

    /// `(slot + facing.get2DDataValue()) % 4`: turning the campfire a quarter turn
    /// moves slot 0 to where slot 1 was, so a campfire facing west puts its first
    /// item where a south-facing one puts its second.
    ///
    /// The control is built in: dropping the facing term makes every arm of this
    /// loop compare a point against **itself at slot 0**, i.e. it would require
    /// all four facings to agree, which they must not.
    #[test]
    fn the_facing_offsets_which_corner_each_slot_uses() {
        const POS: [i32; 3] = [0, 70, 0];
        let mut seen = Vec::new();
        for facing_2d in 0..CAMPFIRE_SLOTS {
            let turned = campfire_item_matrix(POS, facing_2d as f32 * 90.0, 0)
                .transform_point3(Vec3::ZERO);
            let offset_slot =
                campfire_item_matrix(POS, 0.0, facing_2d).transform_point3(Vec3::ZERO);
            assert!(
                turned.distance(offset_slot) < 1e-5,
                "facing {}: slot 0 at {turned:?} but slot {facing_2d} of a south \
                 campfire is at {offset_slot:?}",
                facing_2d as f32 * 90.0
            );
            seen.push(turned);
        }
        for (i, a) in seen.iter().enumerate() {
            for b in &seen[i + 1..] {
                assert!(a.distance(*b) > 0.5, "two facings share a corner: {a:?}");
            }
        }
    }

    /// `Axis.XP.rotationDegrees(90)` is what makes a food sprite lie *on* the
    /// campfire instead of standing up out of it, and a missing `Rx` leaves the
    /// item vertical while every corner assertion above still passes.
    ///
    /// Asserted as two independent facts about the basis, plus the scale, so a
    /// rotation about the wrong axis fails one of them: the sprite's normal
    /// (`+Z`) becomes vertical, and its width axis (`+X`) stays horizontal.
    #[test]
    fn a_cooking_item_lies_flat_at_three_eighths_scale() {
        let m = campfire_item_matrix([0, 0, 0], 0.0, 0);
        let normal = m.transform_vector3(Vec3::Z);
        let across = m.transform_vector3(Vec3::X);
        assert!(
            normal.normalize().y.abs() > 0.999,
            "sprite normal {normal:?} is not vertical — the item is standing up"
        );
        assert!(
            across.normalize().y.abs() < 1e-5,
            "width axis {across:?} is not horizontal"
        );
        assert!(
            (across.length() - CAMPFIRE_ITEM_SCALE).abs() < 1e-6,
            "scale is {}, expected {CAMPFIRE_ITEM_SCALE}",
            across.length()
        );
    }

    #[test]
    fn every_ported_model_bakes_with_geometry_and_parts() {
        let set = set();
        assert_eq!(
            set.len(),
            22,
            "3 chest layers + 2 skull canvases + bell + 4 banner parts (standing \
             and wall, body and flag) + shield + shulker box + book + decorated \
             pot (base plus 4 side models) + 4 conduit layers (eye, wind, shell, \
             cage)"
        );
        for (name, mesh) in set.iter() {
            assert!(mesh.quad_count() > 0, "{name} baked no quads");
            assert_eq!(mesh.parts.len(), mesh.part_names.len());
            assert_eq!(mesh.parts.len(), mesh.part_rest.len());
        }
        for name in [CHEST_SINGLE, CHEST_LEFT, CHEST_RIGHT] {
            let mesh = set.get(name).unwrap();
            assert!(mesh.index_of("lid").is_some(), "{name} has no lid part");
            assert!(mesh.index_of("lock").is_some(), "{name} has no lock part");
            assert!(mesh.index_of("bottom").is_some(), "{name} has no bottom");
        }
        for name in [SKULL_MOB, SKULL_HUMANOID] {
            let mesh = set.get(name).unwrap();
            assert!(mesh.index_of("head").is_some(), "{name} has no head part");
        }
        let bell = set.get(BELL).unwrap();
        assert!(bell.index_of("bell_body").is_some(), "bell has no bell_body part");
        assert!(bell.index_of("bell_base").is_some(), "bell has no bell_base part");
        let banner_body = set.get(BANNER_BODY).unwrap();
        assert!(banner_body.index_of("pole").is_some(), "banner body has no pole part");
        assert!(banner_body.index_of("bar").is_some(), "banner body has no bar part");
        let banner_flag = set.get(BANNER_FLAG).unwrap();
        assert!(banner_flag.index_of("flag").is_some(), "banner flag has no flag part");
    }

    /// The rest AABB must land in `0..1` on Y for the chest layers — the
    /// assertion an entity-space (Y-flipped, `−1.501`) placement fails.
    /// Measured through the same `part_transforms` the draw uses, not from
    /// restated texel extents. Skull is deliberately excluded: it *is*
    /// authored entity-space (Y-down, see `skull_head_part`'s doc), so its
    /// rest bounds dip below zero on purpose — that is asserted by
    /// `skull_head_box_extends_below_its_pivot_like_a_mob_head` in the asset
    /// crate, not this one.
    #[test]
    fn rest_bounds_sit_inside_the_block_above_the_floor() {
        let set = set();
        for name in [CHEST_SINGLE, CHEST_LEFT, CHEST_RIGHT] {
            let mesh = set.get(name).unwrap();
            assert!(
                mesh.local_min.y >= -1e-5,
                "{name} dips below the floor: {}",
                mesh.local_min.y
            );
            assert!(
                (mesh.local_max.y - 14.0 / 16.0).abs() < 1e-4,
                "{name} closed lid tops at {} not 14/16",
                mesh.local_max.y
            );
        }
    }

    /// A chest's placement is a pure rigid motion: `det == +1` for every facing,
    /// so it cannot reverse a quad's winding. Measured, not asserted from
    /// "rotations have positive determinant".
    #[test]
    fn placement_preserves_orientation() {
        for name in ["south", "west", "north", "east"] {
            let yaw = horizontal_facing_yaw(name).expect(name);
            let m = block_entity_placement_matrix([3, 64, -7], yaw);
            assert!(
                (m.determinant() - 1.0).abs() < 1e-5,
                "{name}: det {}",
                m.determinant()
            );
        }
    }

    /// The concrete difference from the entity path, measured against the real
    /// `entity_model_matrix` rather than described. A block entity is neither
    /// flipped nor lifted.
    #[test]
    fn placement_does_not_flip_or_lift() {
        let pos = [0, 0, 0];
        let block = block_entity_placement_matrix(pos, 0.0);
        // The block-space origin stays at the block's own corner.
        let origin = block.transform_point3(Vec3::ZERO);
        assert!(origin.abs_diff_eq(Vec3::ZERO, 1e-5), "origin {origin}");
        // +Y stays +Y.
        let up = block.transform_vector3(Vec3::Y);
        assert!(up.abs_diff_eq(Vec3::Y, 1e-5), "up {up}");

        // The entity matrix, by contrast, flips Y and drops 1.501.
        let entity = crate::entity::entity_model_matrix(Vec3::ZERO, 0.0, 1.0);
        let entity_up = entity.transform_vector3(Vec3::Y);
        assert!(
            entity_up.y < 0.0,
            "the entity path is supposed to flip Y; if this fails the two \
             placements have converged and this test no longer measures anything"
        );
        // **`+1.501`, not `−1.501`** — measured, having first been written the
        // other way round. `entity_model_matrix` is
        // `translate(feet) · rotY · scale(−s,−s,s) · translate(0, −1.501, 0)`,
        // and the flip is applied *after* the lift, so the negative translate
        // comes out positive in world space. Reading the lift's sign off the
        // matrix expression left-to-right gets this backwards, which is the same
        // shape of error `CLAUDE.md` records for the depth-bias record.
        let entity_origin = entity.transform_point3(Vec3::ZERO);
        assert!(
            (entity_origin.y - crate::entity::MODEL_FEET_OFFSET).abs() < 1e-4,
            "entity origin y {}",
            entity_origin.y
        );
    }

    /// South is `0`, and reading the yaw off `Direction`'s *declaration* order
    /// instead (down/up/north/south/west/east) is a quarter-turn error on every
    /// chest in the world.
    #[test]
    fn facing_yaw_follows_direction_to_y_rot_not_declaration_order() {
        assert_eq!(horizontal_facing_yaw("south"), Some(0.0));
        assert_eq!(horizontal_facing_yaw("west"), Some(90.0));
        assert_eq!(horizontal_facing_yaw("north"), Some(180.0));
        assert_eq!(horizontal_facing_yaw("east"), Some(270.0));
        assert_eq!(horizontal_facing_yaw("up"), None);
        assert_eq!(horizontal_facing_yaw(""), None);
    }

    /// A north-facing chest's lock (which sticks out of the *front* at z ≈ 1 in
    /// model space) must end up on the low-Z side of the block. This is the
    /// check that a `+yaw` instead of `-yaw` would fail while every determinant
    /// and bounds test stayed green.
    #[test]
    fn facing_rotates_the_front_of_the_chest_to_the_named_side() {
        let set = set();
        let front_z = |facing: &str| -> Vec3 {
            let spawn = ChestSpawn {
                facing_yaw_deg: horizontal_facing_yaw(facing).unwrap(),
                ..ChestSpawn::at([0, 0, 0])
            };
            let inst = set.resolve_chest(&spawn).expect("resolve");
            let lock = set.get(inst.model).unwrap().index_of("lock").unwrap();
            // The lock's own pivot is at model (0, 9, 1); the latch box sits at
            // z 14..15 texels beyond it, i.e. the chest's front face.
            inst.part_transforms[lock].transform_point3(Vec3::new(0.5, 0.0, 15.0 / 16.0))
        };
        let south = front_z("south");
        let north = front_z("north");
        // Vanilla's south-facing chest has its latch on the +Z face.
        assert!(south.z > 0.9, "south latch z {}", south.z);
        assert!(north.z < 0.1, "north latch z {}", north.z);
        let west = front_z("west");
        let east = front_z("east");
        assert!(west.x < 0.1, "west latch x {}", west.x);
        assert!(east.x > 0.9, "east latch x {}", east.x);
    }

    /// The two easings, at both endpoints and the midpoint. `0.5 → 0.875` is the
    /// value that distinguishes vanilla's cubic ease-out from a linear ramp;
    /// the endpoints alone cannot.
    #[test]
    fn lid_easing_matches_vanillas_cubic_ease_out() {
        assert!((chest_lid_openness(0.0) - 0.0).abs() < 1e-6);
        assert!((chest_lid_openness(1.0) - 1.0).abs() < 1e-6);
        let mid = chest_lid_openness(0.5);
        assert!((mid - 0.875).abs() < 1e-6, "mid {mid}");
        assert!(mid > 0.5, "the ease must run ahead of linear");
        // Out-of-range input clamps rather than exploding into a lid that spins.
        assert!((chest_lid_openness(-1.0) - 0.0).abs() < 1e-6);
        assert!((chest_lid_openness(4.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn lid_angle_is_a_quarter_turn_backwards_when_fully_open() {
        assert!((chest_lid_x_rot(0.0) - 0.0).abs() < 1e-6);
        let full = chest_lid_x_rot(1.0);
        assert!(
            (full - -std::f32::consts::FRAC_PI_2).abs() < 1e-6,
            "full {full}"
        );
    }

    /// The animation has to *move geometry*, not merely produce a different
    /// number. A closed and an open chest must differ in the lid's part matrix
    /// and agree in the bottom's — the second half is what catches an override
    /// applied to the whole model instead of the named parts.
    #[test]
    fn opening_moves_the_lid_and_lock_and_leaves_the_bottom_alone() {
        let set = set();
        let closed = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let open = set
            .resolve_chest(&ChestSpawn {
                openness: 1.0,
                ..ChestSpawn::at([0, 0, 0])
            })
            .unwrap();
        let mesh = set.get(closed.model).unwrap();
        let lid = mesh.index_of("lid").unwrap();
        let lock = mesh.index_of("lock").unwrap();
        let bottom = mesh.index_of("bottom").unwrap();
        assert_ne!(closed.part_transforms[lid], open.part_transforms[lid]);
        assert_ne!(closed.part_transforms[lock], open.part_transforms[lock]);
        assert_eq!(closed.part_transforms[bottom], open.part_transforms[bottom]);

        // And it moves the right way: a fully open lid's far edge rises above
        // the closed chest's own top, rather than sinking into the box.
        let far_edge = Vec3::new(0.5, 0.0, 14.0 / 16.0);
        let closed_y = closed.part_transforms[lid].transform_point3(far_edge).y;
        let open_y = open.part_transforms[lid].transform_point3(far_edge).y;
        assert!(
            open_y > closed_y + 0.3,
            "an open lid must rise: closed {closed_y}, open {open_y}"
        );
    }

    #[test]
    fn material_resolution_covers_every_chest_block_and_nothing_else() {
        assert_eq!(
            ChestMaterial::from_block_path("chest"),
            Some(ChestMaterial::Regular)
        );
        assert_eq!(
            ChestMaterial::from_block_path("trapped_chest"),
            Some(ChestMaterial::Trapped)
        );
        assert_eq!(
            ChestMaterial::from_block_path("ender_chest"),
            Some(ChestMaterial::Ender)
        );
        assert_eq!(
            ChestMaterial::from_block_path("oxidized_copper_chest"),
            Some(ChestMaterial::CopperOxidized)
        );
        assert_eq!(ChestMaterial::from_block_path("barrel"), None);
        assert_eq!(ChestMaterial::from_block_path("chest_boat"), None);
    }

    /// `getChestMaterial` checks copper and ender *before* the seasonal flag, so
    /// December must not repaint them.
    #[test]
    fn christmas_overrides_only_plain_and_trapped_chests() {
        assert_eq!(
            chest_material_with_season(ChestMaterial::Regular, true),
            ChestMaterial::Christmas
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::Trapped, true),
            ChestMaterial::Christmas
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::Ender, true),
            ChestMaterial::Ender
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::CopperWeathered, true),
            ChestMaterial::CopperWeathered
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::Regular, false),
            ChestMaterial::Regular
        );
    }

    /// Ender has one sheet for all three halves; every other material has three
    /// distinct ones. A uniform `_left`/`_right` suffix rule would name
    /// `ender_left.png`, which does not exist in the jar.
    #[test]
    fn ender_uses_one_sheet_and_others_use_three() {
        for half in [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right] {
            assert_eq!(
                chest_texture_stem(ChestMaterial::Ender, half),
                "entity/chest/ender"
            );
        }
        for material in CHEST_MATERIALS {
            if *material == ChestMaterial::Ender {
                continue;
            }
            let stems: Vec<&str> = [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right]
                .into_iter()
                .map(|h| chest_texture_stem(*material, h))
                .collect();
            assert_eq!(
                stems.len(),
                stems.iter().collect::<std::collections::BTreeSet<_>>().len(),
                "{material:?} reuses a sheet across halves: {stems:?}"
            );
        }
    }

    /// The preload list is derived from the same match the renderer asks
    /// through, so it cannot go stale: 7 materials × 3 halves + 1 ender = 22.
    #[test]
    fn every_stem_the_renderer_can_ask_for_is_in_the_preload_list() {
        let stems = chest_texture_stems();
        assert_eq!(stems.len(), 22, "{stems:?}");
        for material in CHEST_MATERIALS {
            for half in [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right] {
                let stem = chest_texture_stem(*material, half);
                assert!(stems.contains(&stem), "{stem} missing from the preload list");
            }
        }
    }

    /// A camera 4 blocks back on `-Z` looking down `+Z` (yaw `0`) at the origin
    /// block — chests at `[0,0,0]`/`[1,0,0]` are in view, one 400 blocks behind
    /// is not.
    fn looking_at_origin() -> Camera {
        Camera {
            position: Vec3::new(0.5, 0.5, -4.0),
            yaw: 0.0,
            pitch: 0.0,
            ..Camera::default()
        }
    }

    #[test]
    fn planning_batches_by_model_and_texture_and_culls_what_is_behind() {
        let set = set();
        let front = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let trapped_same_mesh = set
            .resolve_chest(&ChestSpawn {
                material: ChestMaterial::Trapped,
                ..ChestSpawn::at([1, 0, 0])
            })
            .unwrap();
        assert_eq!(
            front.model, trapped_same_mesh.model,
            "a trapped chest shares the single-chest mesh; the batch key must \
             therefore include the texture"
        );
        let behind = set.resolve_chest(&ChestSpawn::at([0, 0, -400])).unwrap();

        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[front, trapped_same_mesh, behind],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.total, 3);
        assert_eq!(frame.stats.drawn, 2, "{:?}", frame.stats);
        assert_eq!(frame.stats.culled_frustum, 1);
        assert_eq!(
            frame.batches.len(),
            2,
            "two textures over one mesh must be two batches"
        );
        for batch in &frame.batches {
            assert_eq!(batch.count(), 1);
            assert_eq!(batch.parts.len(), set.get(batch.model).unwrap().parts.len());
        }
    }

    /// Two chests sharing model *and* texture must land in one batch with two
    /// instances per part — the whole point of instancing.
    #[test]
    fn identical_chests_share_one_batch() {
        let set = set();
        let a = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let b = set.resolve_chest(&ChestSpawn::at([1, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame =
            plan_block_entities(&[a, b], &Frustum::from_view_projection(cam.view_projection()));
        assert_eq!(frame.batches.len(), 1);
        assert_eq!(frame.batches[0].count(), 2);
        for part in &frame.batches[0].parts {
            assert_eq!(part.len(), 2);
        }
    }

    #[test]
    fn light_reaches_the_batch_unchanged() {
        let set = set();
        let dark = set
            .resolve_chest(&ChestSpawn {
                light: 0,
                ..ChestSpawn::at([0, 0, 0])
            })
            .unwrap();
        assert_eq!(dark.light, 0);
        let cam = looking_at_origin();
        let frame = plan_block_entities(&[dark], &Frustum::from_view_projection(cam.view_projection()));
        assert_eq!(frame.batches[0].lights, vec![0]);
    }

    #[test]
    fn an_unknown_half_degrades_to_a_whole_chest() {
        assert_eq!(ChestHalf::parse("single"), ChestHalf::Single);
        assert_eq!(ChestHalf::parse("left"), ChestHalf::Left);
        assert_eq!(ChestHalf::parse("right"), ChestHalf::Right);
        assert_eq!(ChestHalf::parse("sideways"), ChestHalf::Single);
    }

    // --- skull/head ---------------------------------------------------

    #[test]
    fn skull_type_from_path_covers_the_five_ported_types_and_declines_the_rest() {
        assert_eq!(
            SkullType::from_block_path("skeleton_skull"),
            Some(SkullType::Skeleton)
        );
        assert_eq!(
            SkullType::from_block_path("skeleton_wall_skull"),
            Some(SkullType::Skeleton)
        );
        assert_eq!(
            SkullType::from_block_path("wither_skeleton_skull"),
            Some(SkullType::WitherSkeleton)
        );
        assert_eq!(
            SkullType::from_block_path("wither_skeleton_wall_skull"),
            Some(SkullType::WitherSkeleton)
        );
        assert_eq!(
            SkullType::from_block_path("zombie_head"),
            Some(SkullType::Zombie)
        );
        assert_eq!(
            SkullType::from_block_path("creeper_wall_head"),
            Some(SkullType::Creeper)
        );
        assert_eq!(
            SkullType::from_block_path("player_head"),
            Some(SkullType::Player)
        );
        // Real skull types this renderer does not cover — must decline
        // rather than draw a wrong shape.
        assert_eq!(SkullType::from_block_path("dragon_head"), None);
        assert_eq!(SkullType::from_block_path("dragon_wall_head"), None);
        assert_eq!(SkullType::from_block_path("piglin_head"), None);
        assert_eq!(SkullType::from_block_path("piglin_wall_head"), None);
        // Not a skull at all.
        assert_eq!(SkullType::from_block_path("chest"), None);
    }

    #[test]
    fn every_skull_stem_is_in_the_preload_list() {
        let stems = skull_texture_stems();
        assert_eq!(stems.len(), 5, "{stems:?}");
        for t in SKULL_TYPES {
            assert!(
                stems.contains(&skull_texture_stem(*t)),
                "{t:?} missing from the preload list"
            );
        }
        // Distinct sheets — a copy-paste in `skull_texture_stem` collapsing
        // two types onto one file would still pass a naive coverage check.
        let unique: std::collections::BTreeSet<_> = stems.iter().collect();
        assert_eq!(unique.len(), stems.len(), "{stems:?}");
    }

    #[test]
    fn every_ported_skull_type_bakes_and_resolves() {
        let set = set();
        for t in SKULL_TYPES {
            let spawn = SkullSpawn {
                skull_type: *t,
                ..SkullSpawn::at([0, 0, 0])
            };
            let inst = set
                .resolve_skull(&spawn)
                .unwrap_or_else(|| panic!("{t:?} did not resolve"));
            assert!(!inst.part_transforms.is_empty(), "{t:?}");
            assert_eq!(inst.texture, skull_texture_stem(*t));
        }
    }

    /// Ground and wall placement both preserve orientation (`det == +1`),
    /// same as the chest placements — measured, not assumed, because this is
    /// the one block-entity placement that *does* apply the entity-style
    /// `scale(-1, -1, 1)` flip and a sign mistake there would show up as a
    /// negative determinant, not merely "upside down".
    #[test]
    fn skull_placement_preserves_orientation() {
        for seg in [0u8, 4, 8, 12, 15] {
            let m = skull_ground_placement_matrix([1, 2, 3], seg);
            assert!(
                (m.determinant() - 1.0).abs() < 1e-4,
                "segment {seg}: det {}",
                m.determinant()
            );
        }
        for yaw in [0.0_f32, 90.0, 180.0, 270.0] {
            let m = skull_wall_placement_matrix([1, 2, 3], yaw);
            assert!(
                (m.determinant() - 1.0).abs() < 1e-4,
                "yaw {yaw}: det {}",
                m.determinant()
            );
        }
    }

    /// Unlike a chest, a floor skull genuinely flips Y — the mirror image of
    /// `placement_does_not_flip_or_lift`'s chest assertion. Getting this
    /// backwards would bury the head texture upside down while every bounds
    /// and determinant check stayed green.
    #[test]
    fn ground_skull_flips_y_like_an_entity_head() {
        let m = skull_ground_placement_matrix([0, 0, 0], 0);
        let up = m.transform_vector3(Vec3::Y);
        assert!(up.y < 0.0, "expected an entity-style flip, got {up}");
    }

    /// The rotation segment spins the head about the block's own centre
    /// pivot `(0.5, 0, 0.5)`, so that pivot must land in the same world point
    /// regardless of segment — only the *head*, not the block position,
    /// rotates.
    #[test]
    fn ground_segment_rotates_about_the_block_centre() {
        let pos = [2, 5, -3];
        let unrotated = skull_ground_placement_matrix(pos, 0);
        let rotated = skull_ground_placement_matrix(pos, 8); // 180 degrees
        let a = unrotated.transform_point3(Vec3::ZERO);
        let b = rotated.transform_point3(Vec3::ZERO);
        assert!(a.abs_diff_eq(b, 1e-4), "pivot moved: {a} vs {b}");
        let expected = Vec3::new(2.5, 5.0, -2.5);
        assert!(a.abs_diff_eq(expected, 1e-4), "{a}");
    }

    /// `dir.getStepX()/getStepZ()` recovered by trig against a hand-verified
    /// table (not derived from the function under test): south `(0, 1)`,
    /// west `(-1, 0)`, north `(0, -1)`, east `(1, 0)`. A sign slip here
    /// offsets a wall skull toward the wrong wall while it still renders a
    /// plausible skull shape.
    #[test]
    fn wall_offset_moves_toward_the_named_direction() {
        let cases = [
            ("south", 0.0_f32, 0.0_f32, 1.0_f32),
            ("west", 90.0, -1.0, 0.0),
            ("north", 180.0, 0.0, -1.0),
            ("east", 270.0, 1.0, 0.0),
        ];
        for (name, yaw, step_x, step_z) in cases {
            let m = skull_wall_placement_matrix([0, 0, 0], yaw);
            let origin = m.transform_point3(Vec3::ZERO);
            let expected = Vec3::new(0.5 - step_x * 0.25, 0.25, 0.5 - step_z * 0.25);
            assert!(
                origin.abs_diff_eq(expected, 1e-4),
                "{name}: got {origin}, expected {expected}"
            );
        }
    }

    /// A chest and a skull share neither model nor texture, so a frame
    /// holding both must batch them separately — the same coverage the chest
    /// `planning_batches_by_model_and_texture_and_culls_what_is_behind` test
    /// gives two chest materials, now across two entirely different corpora,
    /// proving [`plan_block_entities`]/[`BlockEntityInstance`] are generic
    /// over block-entity *family*, not just over chest variants.
    #[test]
    fn chests_and_skulls_batch_independently_in_one_frame() {
        let set = set();
        let chest = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let skull = set.resolve_skull(&SkullSpawn::at([1, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, skull],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 2);
        assert_eq!(
            frame.batches.len(),
            2,
            "a chest and a skull must not share a batch"
        );
    }

    #[test]
    fn bell_stem_is_in_the_preload_list() {
        let stems = bell_texture_stems();
        assert_eq!(stems, vec![BELL_TEXTURE_STEM]);
        assert!(block_entity_texture_stems().contains(&BELL_TEXTURE_STEM));
    }

    /// Every stem [`shulker_texture_stem`] can return is preloaded, and the
    /// colour order is `DyeColor`'s **ordinal** order rather than the
    /// alphabetical one the texture directory suggests — reading it off the
    /// listing shifts every dyed box one sprite along, which draws a plausible
    /// wrong colour instead of nothing.
    #[test]
    fn every_shulker_stem_is_in_the_preload_list_in_dye_ordinal_order() {
        // `DyeColor`'s first four and last, from the enum's own declaration order
        // (`DyeColor.java`), not from this table.
        assert_eq!(
            &SHULKER_COLOURS[..4],
            &["white", "orange", "magenta", "light_blue"]
        );
        assert_eq!(SHULKER_COLOURS[15], "black");
        assert_eq!(SHULKER_COLOURS.len(), 16);

        let preload = block_entity_texture_stems();
        assert!(preload.contains(&shulker_texture_stem(None)));
        for colour in SHULKER_COLOURS {
            let stem = shulker_texture_stem(Some(colour));
            assert_ne!(
                stem, SHULKER_DEFAULT_TEXTURE_STEM,
                "{colour} fell through to the undyed sheet"
            );
            assert!(preload.contains(&stem), "{stem} missing from the preload list");
        }
        // An unrecognised name degrades to the undyed sheet rather than being
        // dropped — a plain `shulker_box` has no colour segment at all.
        assert_eq!(
            shulker_texture_stem(Some("chartreuse")),
            SHULKER_DEFAULT_TEXTURE_STEM
        );
    }

    /// An upward-facing shulker box occupies its own block cell and nothing else.
    ///
    /// The expectation comes from geometry rather than from the matrix: the box is
    /// authored as a 16×20 texel stack (`base` 8 tall from y=−8, `lid` 12 tall from
    /// y=−16, both at pivot y=24), so once vanilla's `scale(1, -1, -1)` and
    /// `translate(0, -1, 0)` are folded in it must sit in `0..1` on every axis, at
    /// `0.9995` scale about the block centre. Reusing
    /// [`block_entity_placement_matrix`] instead (a floor pivot, no flip) puts the
    /// box a half-block low and upside down — which still looks like a box.
    #[test]
    fn an_upward_shulker_sits_inside_its_own_block() {
        let set = set();
        let box_at = set.resolve_shulker(&ShulkerSpawn::at([3, 5, -2])).unwrap();
        let lo = Vec3::from(box_at.aabb_min);
        let hi = Vec3::from(box_at.aabb_max);
        let cell = Vec3::new(3.0, 5.0, -2.0);
        assert!(
            lo.cmpge(cell - Vec3::splat(0.001)).all() && hi.cmple(cell + Vec3::splat(1.001)).all(),
            "an up-facing box escaped its own cell: {lo} .. {hi}"
        );
        // And it fills nearly all of it — the `0.9995` shrink, not a half-height
        // box. A `0.5`-tall result is the floor-pivot mistake above.
        let size = hi - lo;
        assert!(
            size.min_element() > 0.99,
            "the box is not block-sized: {size}"
        );
        assert_eq!(box_at.texture, SHULKER_DEFAULT_TEXTURE_STEM);
    }

    /// A closed box needs no part override at all; an open one moves only `lid`.
    /// This is what lets a shulker box share the existing `(model, texture)` batch
    /// key with no per-instance animation state.
    #[test]
    fn a_closed_shulker_is_the_rest_pose_and_an_open_one_moves_only_the_lid() {
        let set = set();
        let mesh = set.get(SHULKER_BOX).unwrap();
        let lid = mesh.index_of("lid").expect("the lid part is named `lid`");
        let base = mesh.index_of("base").expect("the base part is named `base`");

        let closed = set.resolve_shulker(&ShulkerSpawn::at([0, 0, 0])).unwrap();
        let rest = mesh.part_transforms(shulker_placement_matrix([0, 0, 0], ShulkerFacing::Up), &[]);
        assert_eq!(closed.part_transforms[lid], rest[lid]);

        let open = set
            .resolve_shulker(&ShulkerSpawn {
                progress: 1.0,
                ..ShulkerSpawn::at([0, 0, 0])
            })
            .unwrap();
        assert_ne!(open.part_transforms[lid], closed.part_transforms[lid]);
        assert_eq!(
            open.part_transforms[base], closed.part_transforms[base],
            "opening a box moved its base"
        );
        // `lid.setPos(0, 24 - progress * 0.5 * 16, 0)` and `yRot = 270 * progress`
        // — predicted from the jar, not read back out of the port.
        assert_eq!(shulker_lid_pose(0.0), (24.0, 0.0));
        let (y, y_rot) = shulker_lid_pose(1.0);
        assert_eq!(y, 16.0);
        assert!((y_rot - 270.0_f32.to_radians()).abs() < 1e-5, "{y_rot}");
    }

    /// The six facings are six distinct placements, and a down-facing box is the
    /// up-facing one turned over — the `Direction.getRotation()` port.
    #[test]
    fn every_shulker_facing_is_a_distinct_placement() {
        let facings = [
            ShulkerFacing::Up,
            ShulkerFacing::Down,
            ShulkerFacing::North,
            ShulkerFacing::South,
            ShulkerFacing::West,
            ShulkerFacing::East,
        ];
        let mats: Vec<Mat4> = facings
            .iter()
            .map(|f| shulker_placement_matrix([0, 0, 0], *f))
            .collect();
        for i in 0..mats.len() {
            for j in (i + 1)..mats.len() {
                assert!(
                    !mats[i].abs_diff_eq(mats[j], 1e-5),
                    "{:?} and {:?} share a placement",
                    facings[i],
                    facings[j]
                );
            }
        }
        // Every facing still lands the box in its own cell, which is the property
        // an axis mix-up in `ShulkerFacing::rotation` breaks.
        let set = set();
        for facing in facings {
            let drawn = set
                .resolve_shulker(&ShulkerSpawn {
                    facing,
                    ..ShulkerSpawn::at([0, 0, 0])
                })
                .unwrap();
            let lo = Vec3::from(drawn.aabb_min);
            let hi = Vec3::from(drawn.aabb_max);
            assert!(
                lo.cmpge(Vec3::splat(-0.001)).all() && hi.cmple(Vec3::splat(1.001)).all(),
                "{facing:?} escaped its own cell: {lo} .. {hi}"
            );
        }
        assert_eq!(ShulkerFacing::from_name("up"), Some(ShulkerFacing::Up));
        assert_eq!(ShulkerFacing::from_name("sideways"), None);
        assert_eq!(ShulkerFacing::default(), ShulkerFacing::Up);
    }

    /// `BellModel.setupAnim`'s exact formula, predicted independently of the
    /// port rather than by re-deriving its own arithmetic: choosing
    /// `ticks = pi^2 / 2` makes `sin(ticks / pi) == sin(pi/2) == 1` exactly,
    /// so the only remaining unknown is `base_rot = 1 / (4 + ticks/3)` and
    /// each direction's sign/axis — a magnitude check, not merely a sign
    /// flip (`CLAUDE.md`'s "predict the value, do not merely assert the
    /// sign" rule).
    #[test]
    fn bell_shake_angle_matches_the_exact_vanilla_formula() {
        assert_eq!(bell_shake_angle(None, 999.0), (0.0, 0.0), "no direction, no motion");
        assert_eq!(
            bell_shake_angle(Some(BellShakeDirection::North), 0.0),
            (0.0, 0.0),
            "sin(0) is zero at tick 0"
        );

        let ticks = std::f32::consts::PI * std::f32::consts::PI / 2.0;
        let expected = 1.0 / (4.0 + ticks / 3.0);

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::North), ticks);
        assert!((x - -expected).abs() < 1e-4, "north x_rot {x}");
        assert_eq!(z, 0.0);

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::South), ticks);
        assert!((x - expected).abs() < 1e-4, "south x_rot {x}");
        assert_eq!(z, 0.0);

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::East), ticks);
        assert_eq!(x, 0.0);
        assert!((z - -expected).abs() < 1e-4, "east z_rot {z}");

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::West), ticks);
        assert_eq!(x, 0.0);
        assert!((z - expected).abs() < 1e-4, "west z_rot {z}");
    }

    /// The rim (`bell_base`) has no override of its own — if shaking the body
    /// did not also move it, that would mean the parent/child nesting broke
    /// (see `bell_model`'s doc), not merely that the shake is small.
    #[test]
    fn shaking_the_body_moves_the_rim_too_because_it_is_a_child() {
        let set = set();
        let resting = set.resolve_bell(&BellSpawn::at([0, 0, 0])).unwrap();
        assert_eq!(resting.texture, BELL_TEXTURE_STEM);
        assert!(!resting.part_transforms.is_empty());

        let mesh = set.get(BELL).unwrap();
        let body = mesh.index_of("bell_body").unwrap();
        let base = mesh.index_of("bell_base").unwrap();

        let ticks = std::f32::consts::PI * std::f32::consts::PI / 2.0;
        let shaking = set
            .resolve_bell(&BellSpawn {
                shake: Some((BellShakeDirection::East, ticks)),
                ..BellSpawn::at([0, 0, 0])
            })
            .unwrap();

        assert_ne!(
            shaking.part_transforms[body], resting.part_transforms[body],
            "the body itself must move"
        );
        assert_ne!(
            shaking.part_transforms[base], resting.part_transforms[base],
            "the rim must move with its parent"
        );
    }

    #[test]
    fn bells_batch_independently_from_chests_and_skulls() {
        let set = set();
        let chest = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let skull = set.resolve_skull(&SkullSpawn::at([1, 0, 0])).unwrap();
        let bell = set.resolve_bell(&BellSpawn::at([2, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, skull, bell],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(
            frame.batches.len(),
            3,
            "a chest, a skull and a bell must not share a batch"
        );
    }

    // --- banner -----------------------------------------------------------

    /// `banner_ground_placement_matrix`'s scale flips **two** axes (Y and Z),
    /// like `skull_ground_placement_matrix`'s single-axis flip is paired with
    /// the rotation's own handedness — the product of an even number of sign
    /// flips preserves orientation. Measured, not assumed: this is the same
    /// "measure the determinant, don't assert it" discipline
    /// `placement_preserves_orientation` already holds the chest placement
    /// to, generalised to a matrix whose magnitude is `(2/3)^3`, not `1`, so
    /// the assertion is on the *sign* of the determinant, not its value.
    #[test]
    fn banner_ground_placement_preserves_orientation() {
        for segment in [0u8, 1, 4, 8, 12, 15] {
            let m = banner_ground_placement_matrix([3, 64, -7], segment);
            assert!(
                m.determinant() > 0.0,
                "segment {segment}: det {} should be positive (two axis flips cancel)",
                m.determinant()
            );
        }
    }

    /// The two flips are real, individually — not merely a determinant that
    /// happens to be positive by some other route. `+Y` and `+Z` must each
    /// reverse under the placement's linear part, the mirror image of
    /// `placement_does_not_flip_or_lift`'s "chest does not flip" assertion.
    #[test]
    fn banner_ground_placement_flips_y_and_z_but_not_x() {
        let m = banner_ground_placement_matrix([0, 0, 0], 0);
        let up = m.transform_vector3(Vec3::Y);
        let fwd = m.transform_vector3(Vec3::Z);
        let right = m.transform_vector3(Vec3::X);
        assert!(up.y < 0.0, "expected a Y flip, got {up}");
        assert!(fwd.z < 0.0, "expected a Z flip, got {fwd}");
        assert!(right.x > 0.0, "X must not flip, got {right}");
        // Magnitude is the real `2/3` scale, not `1` — skipping it would
        // render a banner 1.5x too large.
        assert!((up.length() - 2.0 / 3.0).abs() < 1e-5, "up length {}", up.length());
    }

    /// `banner_phase`'s exact `floorMod` formula: zero at the origin with no
    /// game time, wraps every 100 ticks, and a negative-leaning block
    /// coordinate sum still lands in `0..1` rather than going negative
    /// (Rust's `%` truncates toward zero and would fail this).
    #[test]
    fn banner_phase_matches_the_floor_mod_formula_and_wraps() {
        assert_eq!(banner_phase([0, 0, 0], 0, 0.0), 0.0);
        // sum = 7 (x=1), game_time 93 -> 100 -> floorMod 0.
        assert_eq!(banner_phase([1, 0, 0], 93, 0.0), 0.0);
        // Partial tick folds in additively, still divided by 100.
        let with_partial = banner_phase([0, 0, 0], 0, 0.5);
        assert!((with_partial - 0.005).abs() < 1e-6, "{with_partial}");
        // A coordinate sum that goes negative must still wrap into 0..100,
        // not produce a negative phase.
        let negative = banner_phase([-5, 0, 0], 0, 0.0);
        assert!((0.0..1.0).contains(&negative), "{negative}");
        // 7 * -5 = -35; floorMod(-35, 100) = 65 -> phase 0.65.
        assert!((negative - 0.65).abs() < 1e-6, "{negative}");
    }

    /// `banner_flag_x_rot`'s exact formula at three phases — a magnitude
    /// prediction, not merely "the sign changes" (`CLAUDE.md`'s "predict the
    /// value" rule). `cos` is exactly `1`, `0` and `-1` at these three
    /// phases, so every intermediate multiply is exact rather than
    /// approximate.
    #[test]
    fn banner_flag_x_rot_matches_the_exact_vanilla_formula() {
        let pi = std::f32::consts::PI;
        // phase 0: cos(0) = 1 -> (-0.0125 + 0.01) * pi = -0.0025 * pi.
        assert!((banner_flag_x_rot(0.0) - (-0.0025 * pi)).abs() < 1e-5);
        // phase 0.25: cos(pi/2) = 0 -> -0.0125 * pi exactly.
        assert!((banner_flag_x_rot(0.25) - (-0.0125 * pi)).abs() < 1e-5);
        // phase 0.5: cos(pi) = -1 -> (-0.0125 - 0.01) * pi = -0.0225 * pi.
        assert!((banner_flag_x_rot(0.5) - (-0.0225 * pi)).abs() < 1e-5);
    }

    /// The base mask is always present and first, even with zero stored
    /// patterns, and every stored pattern follows in its own order —
    /// `resolve_banner` reaching all the way to `banner_pattern_layers`'
    /// own contract (`no_patterns_still_draws_the_base_layer`/
    /// `pattern_order_is_preserved_exactly` in `banner_pattern.rs`), not
    /// re-deriving it.
    #[test]
    fn resolve_banner_produces_the_base_layer_plus_every_pattern_in_order() {
        let set = set();
        let patterns = vec![
            StoredPatternLayer {
                pattern_asset_id: "creeper".to_string(),
                color: DyeColor::Lime,
            },
            StoredPatternLayer {
                pattern_asset_id: "stripe_top".to_string(),
                color: DyeColor::Black,
            },
        ];
        let banner = set
            .resolve_banner(&BannerSpawn {
                base_color: DyeColor::Red,
                patterns,
                ..BannerSpawn::at([0, 0, 0])
            })
            .expect("banner_body and banner_flag must both be in the corpus");
        assert_eq!(banner.layers.len(), 3, "base + 2 patterns");
        assert_eq!(banner.layers[0].color, DyeColor::Red.gamma_rgb());
        assert_eq!(banner.layers[1].color, DyeColor::Lime.gamma_rgb());
        assert_eq!(banner.layers[2].color, DyeColor::Black.gamma_rgb());
        assert!(
            banner.layers[0].sprite.path().ends_with("banner/base"),
            "{:?}",
            banner.layers[0].sprite
        );
        assert!(
            banner.layers[1].sprite.path().ends_with("banner/creeper"),
            "{:?}",
            banner.layers[1].sprite
        );
    }

    /// Every layer reuses the *flag's* posed transform, never the body's —
    /// pattern masks paint over the cloth, not the pole/bar, and a wrong
    /// wiring here would have every mask draw at the pole's own (much
    /// smaller, differently pivoted) rect instead of the flag's.
    #[test]
    fn resolve_banner_layers_share_the_flag_transform_not_the_body() {
        let set = set();
        let banner = set.resolve_banner(&BannerSpawn::at([0, 0, 0])).unwrap();
        let flag_mesh = set.get(BANNER_FLAG).unwrap();
        let flag_index = flag_mesh.index_of("flag").unwrap();
        let expected = banner.flag.part_transforms[flag_index];
        for (i, layer) in banner.layers.iter().enumerate() {
            assert_eq!(layer.transform, expected, "layer {i} transform must equal the flag's");
        }
        assert_ne!(
            banner.layers[0].transform, banner.body.transform,
            "the layer transform must not be the bare placement (the body's)"
        );
    }

    /// The sway moves the flag's own transform, and every layer moves with
    /// it — the same "does it move geometry, not just produce a different
    /// number" standard `opening_moves_the_lid_and_lock_and_leaves_the_bottom_alone`
    /// holds the chest lid to, and `shaking_the_body_moves_the_rim_too_because_it_is_a_child`
    /// holds the bell rim to.
    #[test]
    fn resolve_banner_sway_moves_the_flag_and_every_layer_with_it() {
        let set = set();
        let resting = set
            .resolve_banner(&BannerSpawn {
                patterns: vec![StoredPatternLayer {
                    pattern_asset_id: "creeper".to_string(),
                    color: DyeColor::Lime,
                }],
                ..BannerSpawn::at([0, 0, 0])
            })
            .unwrap();
        let swaying = set
            .resolve_banner(&BannerSpawn {
                phase: 0.5,
                patterns: vec![StoredPatternLayer {
                    pattern_asset_id: "creeper".to_string(),
                    color: DyeColor::Lime,
                }],
                ..BannerSpawn::at([0, 0, 0])
            })
            .unwrap();
        assert_ne!(
            resting.flag.part_transforms, swaying.flag.part_transforms,
            "the flag itself must move"
        );
        assert_eq!(
            resting.body.part_transforms, swaying.body.part_transforms,
            "the pole/bar must not move — only the flag sways"
        );
        assert_ne!(
            resting.layers[0].transform, swaying.layers[0].transform,
            "every pattern layer must move with the flag"
        );
        assert_ne!(
            resting.layers[1].transform, swaying.layers[1].transform,
            "including the base layer"
        );
    }

    #[test]
    fn banner_texture_stem_is_shared_by_body_and_flag_and_in_the_preload_list() {
        let set = set();
        let banner = set.resolve_banner(&BannerSpawn::at([0, 0, 0])).unwrap();
        assert_eq!(banner.body.texture, BANNER_BASE_TEXTURE_STEM);
        assert_eq!(banner.flag.texture, BANNER_BASE_TEXTURE_STEM);
        assert_eq!(banner_texture_stems(), vec![BANNER_BASE_TEXTURE_STEM]);
        assert!(block_entity_texture_stems().contains(&BANNER_BASE_TEXTURE_STEM));
    }

    /// A banner's opaque body+flag batch independently from a chest — the
    /// same coverage `bells_batch_independently_from_chests_and_skulls`
    /// gives bells, now for the fourth family. The banner's own translucent
    /// `layers` are not part of `plan_block_entities` at all (by design —
    /// see `BannerInstances`' doc), so only `body`/`flag` go into this call.
    #[test]
    fn banner_body_and_flag_batch_independently_from_a_chest() {
        let set = set();
        let chest = set.resolve_chest(&ChestSpawn::at([2, 0, 0])).unwrap();
        let banner = set.resolve_banner(&BannerSpawn::at([0, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, banner.body, banner.flag],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(
            frame.batches.len(),
            3,
            "chest, banner body and banner flag must all batch independently \
             (different model *and* different model between body/flag)"
        );
    }

    /// **A wall banner draws the pole-less rig, at the wall's own height.**
    ///
    /// Three assertions, each catching a different way this goes wrong while still
    /// drawing a recognisable banner:
    ///
    /// * the models are the *wall* pair, so the standing rig's 42-texel pole
    ///   cannot end up hanging in mid-air off a block face;
    /// * the wall body really has no `pole` part and the standing one does — an
    ///   `if (standing)` in `createBodyLayer` that was transcribed as
    ///   unconditional would give both a pole and pass any "two banner meshes
    ///   exist" check;
    /// * the two flags sit at **different heights**, which is the whole content of
    ///   the `standing ? -44 : -20.5` pose ternary. Their *cubes* are
    ///   byte-identical, so a copy that reused the standing pose produces a wall
    ///   banner buried two blocks into the floor and no assertion about geometry
    ///   would notice.
    #[test]
    fn a_wall_banner_uses_the_poleless_rig_and_hangs_at_its_own_height() {
        let set = set();
        let wall = set
            .resolve_banner(&BannerSpawn::on_wall([0, 0, 0], 180.0))
            .expect("both wall banner models must be in the corpus");
        assert_eq!(wall.body.model, BANNER_WALL_BODY);
        assert_eq!(wall.flag.model, BANNER_WALL_FLAG);
        assert_eq!(wall.body.texture, BANNER_BASE_TEXTURE_STEM, "one shared sheet");
        assert_eq!(wall.flag.texture, BANNER_BASE_TEXTURE_STEM);

        let standing_body = set.get(BANNER_BODY).unwrap();
        let wall_body = set.get(BANNER_WALL_BODY).unwrap();
        assert!(
            standing_body.index_of("pole").is_some(),
            "the standing body must have a pole"
        );
        assert!(
            wall_body.index_of("pole").is_none(),
            "createBodyLayer(false) adds no pole"
        );
        assert!(wall_body.index_of("bar").is_some());

        // The pose ternary, measured through the same `part_transforms` the draw
        // uses rather than by restating -44 and -20.5.
        let standing_flag = set.get(BANNER_FLAG).unwrap();
        let wall_flag = set.get(BANNER_WALL_FLAG).unwrap();
        let flag_y = |mesh: &BlockEntityMesh| {
            let i = mesh.index_of("flag").unwrap();
            mesh.part_transforms(Mat4::IDENTITY, &[])[i]
                .transform_point3(Vec3::ZERO)
                .y
        };
        let (standing_y, wall_y) = (flag_y(standing_flag), flag_y(wall_flag));
        assert!(
            (standing_y - -44.0 / 16.0).abs() < 1e-5,
            "standing flag pivot {standing_y}"
        );
        assert!(
            (wall_y - -20.5 / 16.0).abs() < 1e-5,
            "wall flag pivot {wall_y}"
        );
        assert!(
            wall_y > standing_y,
            "a wall banner hangs higher in model space than a standing one's \
             cloth: {wall_y} vs {standing_y}"
        );

        // The cubes are identical, which is exactly why the pose above is the
        // only thing separating them.
        assert_eq!(standing_flag.quad_count(), wall_flag.quad_count());
    }

    /// The two placements are one function with two angles — but the *angle
    /// conventions* are not interchangeable, and this is what stops a caller
    /// handing a wall banner a rotation segment.
    ///
    /// A segment is `22.5°` per step and a facing is `90°`, so segment `4` and
    /// facing `west` are the same `90°` rotation while segment `4` read as a
    /// *facing* would be nothing at all. The gate pins the shared shape and the
    /// distinct convention together: equal matrices at equal *angles*, and a
    /// deliberately unequal pair at the same numeric input.
    #[test]
    fn the_two_banner_placements_share_one_transform_and_two_angle_conventions() {
        let pos = [3, 70, -5];
        // Segment 4 is 4 * 22.5 = 90 degrees, which is also `west`'s toYRot.
        assert_eq!(
            banner_ground_placement_matrix(pos, 4),
            banner_wall_placement_matrix(pos, horizontal_facing_yaw("west").unwrap()),
            "one modelTransformation, two callers"
        );
        // The same *number* means different things to the two.
        assert_ne!(
            banner_ground_placement_matrix(pos, 4),
            banner_wall_placement_matrix(pos, 4.0),
            "a segment is 22.5 degrees per step; a facing yaw is degrees"
        );
        // And neither has skull's push away from the wall: the block's own
        // corner-plus-half is the whole translation.
        let at_origin = banner_wall_placement_matrix([0, 0, 0], 0.0).transform_point3(Vec3::ZERO);
        assert!(
            (at_origin - Vec3::new(0.5, 0.0, 0.5)).length() < 1e-6,
            "no extra offset away from the wall, got {at_origin}"
        );
    }

    /// `Direction.getClockWise().toYRot()`, hand-expanded from the jar's own two
    /// tables (`Direction.getClockWise`: north→east→south→west→north, and
    /// `toYRot`: south 0, west 90, north 180, east 270).
    ///
    /// The wrong hypothesis is not an error but a quarter turn, and it is
    /// spelled with the function *next to* the right one, so this asserts both
    /// arms in the same run: every facing's clockwise yaw must differ from its
    /// plain yaw by exactly 90°, and the four expected values are written out
    /// rather than derived from `horizontal_facing_yaw` (which would make the
    /// test agree with whatever the implementation does).
    #[test]
    fn a_lecterns_yaw_is_the_facing_turned_clockwise_not_the_facing() {
        for (facing, clockwise, plain) in [
            ("north", 270.0_f32, 180.0_f32),
            ("east", 0.0, 270.0),
            ("south", 90.0, 0.0),
            ("west", 180.0, 90.0),
        ] {
            assert_eq!(
                horizontal_facing_clockwise_yaw(facing),
                Some(clockwise),
                "{facing}"
            );
            assert_eq!(horizontal_facing_yaw(facing), Some(plain), "{facing}");
            assert_ne!(
                clockwise, plain,
                "{facing}: the two must differ, or this test proves nothing"
            );
        }
        assert_eq!(horizontal_facing_clockwise_yaw("up"), None);
    }

    /// `BookModel.State.forAnimation(0.0, 0.1, 0.9, 1.2)` collapses to a
    /// constant, computed here from the jar's four literals rather than by
    /// reading [`LECTERN_BOOK_OPENNESS`] back.
    ///
    /// The point is the `sin(progress * 0.02)` term: it is dead at
    /// `progress == 0`, which is why a lectern book must not be given a live
    /// clock. The second assertion is the control — with a *non*-zero progress
    /// the same formula does move, so the constant is a property of the
    /// lectern's arguments and not of the formula being inert.
    #[test]
    fn a_lectern_books_openness_is_constant_because_its_progress_term_is_dead() {
        fn for_animation(progress: f32, openness: f32) -> f32 {
            ((progress * 0.02).sin() * 0.1 + 1.25) * openness
        }
        assert!((for_animation(0.0, 1.2) - LECTERN_BOOK_OPENNESS).abs() < 1e-6);
        assert!((for_animation(0.0, 1.2) - 1.5).abs() < 1e-6);
        assert!(
            (for_animation(100.0, 1.2) - 1.5).abs() > 1e-3,
            "a live progress *would* move openness, so the constant above is \
             about the lectern's own arguments"
        );
    }

    /// The six posed parts, against `BookModel.setupAnim` transcribed by hand.
    ///
    /// `seam` must be absent from the list: the jar never poses it, and its rest
    /// `rotation(0, PI/2, 0)` is the spine's quarter turn — an override with a
    /// zero `y_rot` would flatten it into the covers, which still draws a
    /// plausible book.
    #[test]
    fn the_books_six_poses_match_setup_anim_and_leave_the_seam_alone() {
        let openness = 1.5_f32;
        let poses = book_part_poses(openness, (0.1, 0.9));
        let by_name = |name: &str| {
            poses
                .iter()
                .find(|(n, _, _)| *n == name)
                .copied()
                .unwrap_or_else(|| panic!("{name} is not posed"))
        };

        let slide = openness.sin();
        for (name, expected_y_rot, expected_x) in [
            ("left_lid", std::f32::consts::PI + 1.5, None),
            ("right_lid", -1.5, None),
            ("left_pages", 1.5, Some(slide)),
            ("right_pages", -1.5, Some(slide)),
            // openness - openness*2*flip: 1.5 - 0.3 and 1.5 - 2.7.
            ("flip_page1", 1.2, Some(slide)),
            ("flip_page2", -1.2, Some(slide)),
        ] {
            let (_, y_rot, x) = by_name(name);
            assert!(
                (y_rot - expected_y_rot).abs() < 1e-5,
                "{name}: y_rot {y_rot} != {expected_y_rot}"
            );
            match (x, expected_x) {
                (Some(a), Some(b)) => assert!((a - b).abs() < 1e-6, "{name}: x"),
                (None, None) => {}
                _ => panic!("{name}: x presence"),
            }
        }
        assert!(
            !poses.iter().any(|(n, _, _)| *n == "seam"),
            "the seam is never posed by the jar"
        );

        // The two flip pages must land on opposite sides of the spine — that is
        // what makes a book look mid-turn rather than shut. A transcription that
        // dropped the `* 2` gives 1.35 and 0.15: both positive, same side, and a
        // sign-only assertion would pass.
        let (_, flip1, _) = by_name("flip_page1");
        let (_, flip2, _) = by_name("flip_page2");
        assert!(flip1 > 0.0 && flip2 < 0.0, "{flip1} / {flip2}");
    }

    /// The `67.5°` tilt about **Z** is what makes a lectern book face a reader,
    /// and it is the whole difference from [`block_entity_placement_matrix`].
    ///
    /// Expectation from the transform algebra, not from the implementation: `Ry`
    /// preserves a vector's `y` component, so the angle between the book's own
    /// up axis and world up is exactly the tilt for **every** facing. Reusing
    /// the chest placement matrix gives `0°` — the wrong hypothesis is computed
    /// here and required to be far away, in the same run.
    #[test]
    fn the_books_placement_tilts_it_by_the_jars_angle_at_every_facing() {
        let up = Vec3::Y;
        for facing in ["north", "east", "south", "west"] {
            let yaw = horizontal_facing_clockwise_yaw(facing).unwrap();
            let m = lectern_book_placement_matrix([3, 4, 5], yaw);
            let book_up = m.transform_vector3(up).normalize();
            let angle = book_up.dot(up).clamp(-1.0, 1.0).acos().to_degrees();
            assert!(
                (angle - 67.5).abs() < 1e-3,
                "{facing}: tilt {angle} != 67.5"
            );

            // The wrong hypothesis, in the same run.
            let flat = block_entity_placement_matrix([3, 4, 5], yaw);
            let flat_angle = flat
                .transform_vector3(up)
                .normalize()
                .dot(up)
                .clamp(-1.0, 1.0)
                .acos()
                .to_degrees();
            assert!(flat_angle < 1e-3, "the chest matrix does not tilt at all");
        }

        // The facing really does turn the book: opposite facings must put the
        // book's horizontal lean in opposite directions. A placement that
        // dropped the `Ry` term entirely would satisfy the tilt assertion above
        // at all four facings and fail here.
        let north = lectern_book_placement_matrix(
            [0, 0, 0],
            horizontal_facing_clockwise_yaw("north").unwrap(),
        )
        .transform_vector3(Vec3::Y);
        let south = lectern_book_placement_matrix(
            [0, 0, 0],
            horizontal_facing_clockwise_yaw("south").unwrap(),
        )
        .transform_vector3(Vec3::Y);
        let horizontal = |v: Vec3| Vec3::new(v.x, 0.0, v.z);
        assert!(
            horizontal(north).dot(horizontal(south)) < 0.0,
            "north {north} vs south {south}"
        );
    }

    /// The lectern reaches the batcher, batches on its own key, and the six
    /// overrides really are in the instance's `part_transforms`.
    ///
    /// The last part is the one a "does it draw" check misses: a book whose
    /// overrides were dropped is a *shut* book, which still batches, still
    /// culls, still draws, and still looks like a book from any distance.
    #[test]
    fn a_lectern_batches_on_its_own_key_with_its_overrides_applied() {
        let set = set();
        let mesh = set.get(BOOK).unwrap();
        let lectern = set.resolve_lectern(&LecternSpawn::at([0, 0, 0])).unwrap();
        assert_eq!(lectern.model, BOOK);
        assert_eq!(lectern.texture, BOOK_TEXTURE_STEM);
        assert_eq!(lectern.part_transforms.len(), mesh.parts.len());

        // Rest transforms through the *same* placement, so the only difference
        // between the two is the override list.
        let placement = lectern_book_placement_matrix([0, 0, 0], LecternSpawn::at([0; 3]).facing_yaw_deg);
        let rest = mesh.part_transforms(placement, &[]);
        for name in [
            "left_lid",
            "right_lid",
            "left_pages",
            "right_pages",
            "flip_page1",
            "flip_page2",
        ] {
            let i = mesh.index_of(name).unwrap();
            assert_ne!(
                rest[i], lectern.part_transforms[i],
                "{name} was not posed"
            );
        }
        // …and the seam is, correctly, untouched.
        let seam = mesh.index_of("seam").unwrap();
        assert_eq!(rest[seam], lectern.part_transforms[seam]);

        let chest = set.resolve_chest(&ChestSpawn::at([2, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, lectern],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 2);
        assert_eq!(frame.batches.len(), 2, "a book is its own model and sheet");
    }
}

/// Held/dropped `minecraft:special` item forms — [`special_item_rig`] and the rig
/// geometry it names.
///
/// Its own module because these gates are about the **item** surfaces (hand, drop,
/// item frame, inventory slot), not about a placed block entity, and the two
/// families fail for different reasons.
#[cfg(test)]
mod special_item_tests {
    use super::*;

    /// Every `(model, sheet)` pair [`special_item_rig`] can return must really
    /// exist: the model in [`BlockEntityModelSet`] and the sheet in the preload
    /// list the shell builds bind groups from.
    ///
    /// **This is the island check, and it is the one that matters most here.** A
    /// typo in a model name or a stem is not a compile error — both are
    /// `&'static str` — and the only symptom is a held chest that silently draws
    /// nothing, which is byte-for-byte the bug this whole path exists to fix. So the
    /// assertion is not "the mapping returns something", it is "what it returns can
    /// be looked up".
    ///
    /// The subjects are the real 26.2 item paths, one per resolving `kind`, plus the
    /// two ends of the shulker colour range and both skull rigs (the mob 32×32 canvas
    /// and the humanoid 64×64 one) — a single subject per kind would leave whichever
    /// arm picked the wrong canvas passing.
    #[test]
    fn every_rig_and_sheet_the_resolver_names_can_actually_be_looked_up() {
        let models = BlockEntityModelSet::load();
        let stems = block_entity_texture_stems();
        let mut wrong: Vec<String> = Vec::new();
        for (kind, path) in [
            ("minecraft:chest", "chest"),
            ("minecraft:chest", "trapped_chest"),
            ("minecraft:chest", "ender_chest"),
            ("minecraft:chest", "oxidized_copper_chest"),
            ("minecraft:shulker_box", "shulker_box"),
            ("minecraft:shulker_box", "white_shulker_box"),
            ("minecraft:shulker_box", "black_shulker_box"),
            // `skeleton_skull` and `creeper_head` are the 32x32 mob rig;
            // `zombie_head` and `player_head` are the 64x64 humanoid one.
            ("minecraft:head", "skeleton_skull"),
            ("minecraft:head", "wither_skeleton_skull"),
            ("minecraft:head", "creeper_head"),
            ("minecraft:head", "zombie_head"),
            ("minecraft:player_head", "player_head"),
            ("minecraft:shield", "shield"),
        ] {
            let Some((model, stem)) = special_item_rig(kind, path) else {
                wrong.push(format!("{kind}/{path}: resolved to nothing"));
                continue;
            };
            if models.get(model).is_none() {
                wrong.push(format!(
                    "{kind}/{path}: model {model:?} is not in BLOCK_ENTITY_MODELS"
                ));
            }
            if !stems.contains(&stem) {
                wrong.push(format!(
                    "{kind}/{path}: sheet {stem:?} is not in the preload list, so the \
                     shell builds no bind group for it and this draws nothing"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// Both halves of the two-level key are load-bearing: the `kind` picks the rig
    /// and the item path picks the sheet *within* it.
    ///
    /// The wrong hypothesis this excludes is "the item path alone is enough" — and
    /// the discriminating pair is a plain chest against a trapped one, which share a
    /// `kind` and a **mesh** and differ only in sheet. A resolver keyed on `kind`
    /// alone passes any "does it resolve" check and draws every trapped chest with
    /// the plain sheet.
    #[test]
    fn the_kind_picks_the_rig_and_the_item_path_picks_the_sheet() {
        let (plain_model, plain_stem) =
            special_item_rig("minecraft:chest", "chest").expect("a plain chest");
        let (trapped_model, trapped_stem) =
            special_item_rig("minecraft:chest", "trapped_chest").expect("a trapped chest");
        assert_eq!(
            plain_model, trapped_model,
            "the two share one mesh; only the sheet differs"
        );
        assert_ne!(
            plain_stem, trapped_stem,
            "keying the sheet on `kind` alone draws every trapped chest plain"
        );
        assert_eq!(plain_model, CHEST_SINGLE);

        // And the reciprocal: one item path under two different `kind`s must not
        // collapse. `player_head` is its own `kind` in vanilla precisely because its
        // renderer resolves a profile texture.
        assert_eq!(
            special_item_rig("minecraft:player_head", "player_head"),
            special_item_rig("minecraft:head", "player_head"),
            "the two head kinds share one rig family here — we fetch no profile skin, \
             so a player head draws the default sheet exactly as a placed one does"
        );
    }

    /// A held chest is always the **single** chest layer, never a double half.
    ///
    /// `ChestSpecialRenderer.Unbaked`'s `chest_type` defaults to `ChestType.SINGLE`
    /// and no 26.2 item definition overrides it. The two double halves are 15 texels
    /// wide against the single's 14 and each omits the face meeting its partner, so
    /// picking one of those for an item leaves a chest with a hole in its side — and
    /// it still passes any "does a chest draw" gate.
    #[test]
    fn an_item_chest_is_the_single_layer_not_a_double_half() {
        let (model, _) = special_item_rig("minecraft:chest", "chest").expect("a chest");
        assert_eq!(model, CHEST_SINGLE);
        assert_ne!(model, CHEST_LEFT);
        assert_ne!(model, CHEST_RIGHT);
    }

    /// A dropped/framed/other-entity's-hand shield now resolves to the real rig and
    /// the **no-pattern** sheet specifically — never [`SHIELD_BASE_TEXTURE_STEM`],
    /// which would draw an opaque canvas meant to sit *under* a translucent
    /// dye/pattern layer this resolver never issues. Getting that backwards would
    /// still "resolve" (both stems are in the preload list) and still pass the
    /// corpus-wide lookup gate above, so this checks the sheet by name rather than
    /// merely that one was returned.
    #[test]
    fn shield_resolves_to_the_no_pattern_rig_and_sheet() {
        let (model, stem) =
            special_item_rig("minecraft:shield", "shield").expect("a shield now resolves here");
        assert_eq!(model, SHIELD);
        assert_eq!(
            stem, SHIELD_BASE_NO_PATTERN_TEXTURE_STEM,
            "a shield with no runtime dye/pattern state reaching this resolver must \
             draw the opaque no-pattern sheet, not the sheet meant to sit under a \
             translucent layer this resolver never issues"
        );
        assert_ne!(
            stem, SHIELD_BASE_TEXTURE_STEM,
            "the two sheets differ (the shield-bug fix's own 200-texel measurement), \
             so drawing the wrong one is a real, visible regression, not a rename"
        );
    }

    /// The five unported `kind`s, and the item paths a `kind` must decline, resolve to
    /// nothing rather than to a plausible wrong rig. `shield` is deliberately **not**
    /// in this list any more — see [`special_item_rig`]'s own doc for why it now
    /// resolves here too (always undyed/pattern-less), and
    /// [`shield_resolves_to_the_no_pattern_rig_and_sheet`] for its own positive gate.
    ///
    /// The `dragon_head`/`piglin_head` arm is the sharp one: both are real
    /// `minecraft:head` items in 26.2, so the `kind` matches and only
    /// `SkullType::from_block_path` declines them. A resolver that fell back to any
    /// skull rig would draw a dragon head as a skeleton skull — which looks like a
    /// texture bug rather than an unported rig.
    #[test]
    fn unported_kinds_and_undeclared_paths_resolve_to_nothing() {
        let mut wrong: Vec<String> = Vec::new();
        for (kind, path) in [
            ("minecraft:banner", "white_banner"),
            ("minecraft:conduit", "conduit"),
            ("minecraft:copper_golem_statue", "copper_golem_statue"),
            ("minecraft:decorated_pot", "decorated_pot"),
            ("minecraft:trident", "trident"),
            // Real `minecraft:head` items whose rigs are unrelated multi-part ones.
            ("minecraft:head", "dragon_head"),
            ("minecraft:head", "piglin_head"),
            // A datapack item declaring a `kind` over something that is not one.
            ("minecraft:chest", "diamond_pickaxe"),
            ("minecraft:shulker_box", "not_a_shulker_box"),
            ("minecraft:head", "stone"),
            // An unknown kind entirely.
            ("mypack:teapot", "teapot"),
        ] {
            if let Some(rig) = special_item_rig(kind, path) {
                wrong.push(format!("{kind}/{path}: resolved to {rig:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// **The geometry gate**: a chest rig is `18` quads — three boxes of six faces —
    /// which is what tells a real rig from the two things that look like a fix and
    /// are not.
    ///
    /// | hypothesis | quads |
    /// |---|---|
    /// | the flat `base` sprite fallback | `0` (the base model has no `elements` and no `layer0`) |
    /// | a plain block-item cube | `6` |
    /// | **the chest rig** | **`18`** |
    ///
    /// A presence-only assertion ("something drew") is satisfied by the first two,
    /// and the first is exactly what the GUI's own doc measured as *vacuous*. Both
    /// wrong values are named here rather than described, so the predicate is a
    /// prediction and not a sign test.
    ///
    /// The shulker box is checked too, and for a reason specific to it: a closed
    /// shulker box is **near-cubic**, so if it were the only subject a cube-shaped
    /// fallback would be hard to tell from the rig by silhouette. Its quad count
    /// still separates them.
    #[test]
    fn the_chest_rig_has_a_quad_count_no_sprite_or_cube_fallback_can_produce() {
        let models = BlockEntityModelSet::load();
        let chest = models.get(CHEST_SINGLE).expect("the single chest layer");
        assert_eq!(
            chest.quad_count(),
            18,
            "the single chest is bottom + lid + lock, three boxes of six faces"
        );
        assert_ne!(chest.quad_count(), 6, "a plain block-item cube");
        assert_ne!(chest.quad_count(), 0, "the vacuous base-sprite fallback");

        let shulker = models.get(SHULKER_BOX).expect("the shulker box layer");
        assert_ne!(shulker.quad_count(), 6, "a plain block-item cube");
        assert_ne!(shulker.quad_count(), 0, "the vacuous base-sprite fallback");
    }

    /// A chest's parts are **posed relative to each other**, which is the property a
    /// single-cube fallback structurally cannot have — and the reason a chest rather
    /// than a shulker box is the right subject for this gate.
    ///
    /// Predicted exactly, from `chest_single_model`'s own `PartPose::offset` values:
    /// `bottom` is `PartPose::ZERO`, so its matrix is the placement unchanged, while
    /// `lid` and `lock` are both `offset(0, 9, 1)` in texels — `9/16` up and `1/16`
    /// forward in block-local space. Not "the matrices differ", which a jitter would
    /// satisfy: the exact translation, derived from the model definition rather than
    /// restated as a number.
    #[test]
    fn the_chest_parts_are_posed_relative_to_the_placement_not_stacked_on_it() {
        let models = BlockEntityModelSet::load();
        let chest = models.get(CHEST_SINGLE).expect("the single chest layer");
        let placement = Mat4::from_translation(Vec3::new(3.0, 5.0, 7.0));
        let transforms = chest.part_transforms(placement, &[]);

        let bottom = chest.index_of("bottom").expect("a `bottom` part");
        let lid = chest.index_of("lid").expect("a `lid` part");
        let lock = chest.index_of("lock").expect("a `lock` part");

        let origin = |i: usize| transforms[i].transform_point3(Vec3::ZERO);
        let placed = placement.transform_point3(Vec3::ZERO);
        assert!(
            (origin(bottom) - placed).length() < 1e-5,
            "`bottom` is PartPose::ZERO, so it must be the placement unchanged; got {}",
            origin(bottom)
        );
        // `PartPose::offset(0.0, 9.0, 1.0)` in texels, and the mesh is block-local.
        let expected = placed + Vec3::new(0.0, 9.0 / 16.0, 1.0 / 16.0);
        let mut wrong: Vec<String> = Vec::new();
        for (name, index) in [("lid", lid), ("lock", lock)] {
            let got = origin(index);
            if (got - expected).length() > 1e-5 {
                wrong.push(format!("{name}: expected {expected}, got {got}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
        // And the control: the offset is big enough that "all parts share the
        // placement" — the single-cube hypothesis — fails the assertion above.
        assert!(
            (expected - placed).length() > 0.1,
            "control failed: the lid offset is too small to distinguish a real rig \
             from three copies of one cube"
        );
    }

    /// [`BlockEntityModelSet::resolve_special_item`] is what the three world
    /// surfaces (dropped stack, another entity's hand, item frame) each turn a
    /// placement into an instance with, so it has to carry the **whole rig** and be
    /// keyed on the sheet the batcher groups by.
    ///
    /// The geometry predicate is the one from
    /// `the_chest_rig_has_a_quad_count_no_sprite_or_cube_fallback_can_produce`,
    /// applied through this accessor: `18` quads and `3` parts, against `6` for a
    /// block-item cube and `0`/`1` for the sprite fallback. A presence assertion is
    /// satisfied by both wrong answers, which is exactly why the held chest looked
    /// fine in the GUI for so long.
    #[test]
    fn resolve_special_item_carries_the_whole_rig_and_the_items_own_sheet() {
        let models = BlockEntityModelSet::load();
        let placement = Mat4::from_translation(Vec3::new(12.0, 70.0, -4.0));
        let chest = models
            .resolve_special_item("minecraft:chest", "chest", placement, &[], 0x8F)
            .expect("a plain chest resolves");
        assert_eq!(chest.model, CHEST_SINGLE);
        // **Four**, not three. `chest_single_model`'s root is a real, geometry-free
        // `PartDef` and `bake_entity_parts` emits it under an empty name, so the
        // part list is `["", "bottom", "lid", "lock"]` — the count asserted by
        // `single_chest_has_the_three_vanilla_parts_in_order` in `lodestone-assets`.
        // "Three boxes, so three parts" is the plausible wrong number, and it fails
        // here rather than showing up as a missing lid.
        assert_eq!(
            chest.part_transforms.len(),
            4,
            "an empty-named root plus bottom + lid + lock; a cube fallback has one \
             part and a sprite none"
        );
        let mesh = models.get(chest.model).expect("the resolved mesh");
        assert_eq!(mesh.quad_count(), 18, "three boxes of six faces");
        assert_ne!(mesh.quad_count(), 6, "a plain block-item cube");
        assert_ne!(mesh.quad_count(), 0, "the vacuous base-sprite fallback");
        assert_eq!(chest.light, 0x8F, "the caller's light must ride through");

        // The AABB is the rig's own, moved by the placement — non-degenerate and
        // straddling the placement's translation. A zero-volume box would cull the
        // instance on the first frustum test and read as "nothing draws".
        let volume = (chest.aabb_max - chest.aabb_min).min_element();
        assert!(volume > 0.0, "degenerate AABB {:?}", chest.aabb_max - chest.aabb_min);
        let placed = placement.transform_point3(Vec3::ZERO);
        assert!(
            chest.aabb_min.x <= placed.x + 1.0 && chest.aabb_max.x >= placed.x,
            "the AABB {:?}..{:?} does not follow the placement at {placed}",
            chest.aabb_min,
            chest.aabb_max
        );

        // The **sheet** is what the batcher keys on alongside the model, so a
        // trapped chest must share the mesh and differ here. Collected, so one
        // wrong arm does not hide the other.
        let trapped = models
            .resolve_special_item("minecraft:chest", "trapped_chest", placement, &[], 0)
            .expect("a trapped chest resolves");
        let mut wrong: Vec<String> = Vec::new();
        if trapped.model != chest.model {
            wrong.push(format!(
                "a trapped chest took a different mesh ({}) from a plain one ({})",
                trapped.model, chest.model
            ));
        }
        if trapped.texture == chest.texture {
            wrong.push(format!(
                "a trapped chest shares the plain chest's sheet ({})",
                chest.texture
            ));
        }
        // And the negative control that must fire: an unported `kind` yields
        // nothing rather than a default oak chest.
        for (kind, path) in [
            ("minecraft:conduit", "conduit"),
            ("minecraft:trident", "trident"),
            ("minecraft:chest", "diamond_pickaxe"),
            ("mypack:teapot", "teapot"),
        ] {
            if models
                .resolve_special_item(kind, path, placement, &[], 0)
                .is_some()
            {
                wrong.push(format!("{kind}/{path} resolved to an instance"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// Every stored sherd this table names resolves to a **distinct** pattern
    /// stem, and an unrecognised path (a datapack item, or `None`'s own
    /// caller-side default) resolves to nothing rather than a plausible wrong
    /// sherd — the same "decline rather than guess" contract
    /// [`special_item_rig`]'s own tests hold `kind` to.
    #[test]
    fn every_named_sherd_resolves_to_a_distinct_pattern_stem() {
        let sherds = [
            "angler_pottery_sherd",
            "archer_pottery_sherd",
            "arms_up_pottery_sherd",
            "blade_pottery_sherd",
            "brewer_pottery_sherd",
            "burn_pottery_sherd",
            "danger_pottery_sherd",
            "explorer_pottery_sherd",
            "flow_pottery_sherd",
            "friend_pottery_sherd",
            "guster_pottery_sherd",
            "heart_pottery_sherd",
            "heartbreak_pottery_sherd",
            "howl_pottery_sherd",
            "miner_pottery_sherd",
            "mourner_pottery_sherd",
            "plenty_pottery_sherd",
            "prize_pottery_sherd",
            "scrape_pottery_sherd",
            "sheaf_pottery_sherd",
            "shelter_pottery_sherd",
            "skull_pottery_sherd",
            "snort_pottery_sherd",
        ];
        let mut stems: Vec<&'static str> = Vec::with_capacity(sherds.len());
        for sherd in sherds {
            let stem = decorated_pot_pattern_texture_stem(sherd)
                .unwrap_or_else(|| panic!("{sherd} must resolve to a pattern stem"));
            assert!(
                stem.starts_with("entity/decorated_pot/") && stem.ends_with("_pottery_pattern"),
                "{sherd} resolved to {stem}, which does not look like a decorated-pot pattern"
            );
            stems.push(stem);
        }
        let mut deduped = stems.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            stems.len(),
            "two different sherds resolved to the same pattern stem: {stems:?}"
        );

        // Absence and the "not a real sherd" case both decline, per the
        // function's own contract — a resolver that fell back to a plausible
        // pattern here would draw a datapack item as a real vanilla sherd.
        assert_eq!(decorated_pot_pattern_texture_stem("brick"), None);
        assert_eq!(decorated_pot_pattern_texture_stem("diamond_pickaxe"), None);
        assert_eq!(decorated_pot_pattern_texture_stem(""), None);
    }

    /// An undecorated pot resolves to five instances — the base plus all
    /// four sides, every side falling back to
    /// [`DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM`] — matching
    /// `DecoratedPotRenderer.submit`'s own unconditional four
    /// `submitModelPart` calls: a blank side is drawn with the default
    /// sprite, not skipped.
    #[test]
    fn an_undecorated_pot_resolves_to_a_base_and_four_default_sides() {
        let models = BlockEntityModelSet::load();
        let spawn = DecoratedPotSpawn::at([5, 70, -3]);
        let [base, front, back, left, right] = models
            .resolve_decorated_pot(&spawn)
            .expect("the decorated-pot corpus must resolve");

        assert_eq!(base.model, DECORATED_POT_BASE);
        assert_eq!(base.texture, DECORATED_POT_BASE_TEXTURE_STEM);

        for (name, inst, model) in [
            ("front", &front, DECORATED_POT_SIDE_FRONT),
            ("back", &back, DECORATED_POT_SIDE_BACK),
            ("left", &left, DECORATED_POT_SIDE_LEFT),
            ("right", &right, DECORATED_POT_SIDE_RIGHT),
        ] {
            assert_eq!(inst.model, model, "{name} took the wrong model");
            assert_eq!(
                inst.texture, DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM,
                "{name} of a blank pot must draw the default side sprite"
            );
        }
    }

    /// **The discriminating gate.** Four *pairwise-distinct* sherds, one per
    /// side, must resolve to four distinct textures **on the correct named
    /// model** — not merely "four different textures somewhere". Checking by
    /// `model` rather than by position is what makes this transposition-proof
    /// per `CLAUDE.md`'s evidence standard: a resolver that swapped, say,
    /// `left` and `right` would still produce "four distinct textures" but
    /// would fail this assertion, where a plain `HashSet::len() == 4` check
    /// would not catch it.
    ///
    /// The four sherds are chosen pairwise-distinct on purpose (never reusing
    /// one sherd across two sides) — the same discipline `CLAUDE.md` requires
    /// of adjacent same-typed fields, because a fixture with a repeated sherd
    /// cannot distinguish "resolved independently" from "one sherd painted
    /// everywhere".
    #[test]
    fn four_distinct_sherds_produce_four_distinct_textures_on_the_right_faces() {
        let models = BlockEntityModelSet::load();
        let spawn = DecoratedPotSpawn {
            front: Some("angler_pottery_sherd".to_string()),
            back: Some("skull_pottery_sherd".to_string()),
            left: Some("heart_pottery_sherd".to_string()),
            right: Some("danger_pottery_sherd".to_string()),
            ..DecoratedPotSpawn::at([1, 64, 1])
        };
        let [base, front, back, left, right] = models
            .resolve_decorated_pot(&spawn)
            .expect("the decorated-pot corpus must resolve");

        let expect = [
            ("front", &front, DECORATED_POT_SIDE_FRONT, "entity/decorated_pot/angler_pottery_pattern"),
            ("back", &back, DECORATED_POT_SIDE_BACK, "entity/decorated_pot/skull_pottery_pattern"),
            ("left", &left, DECORATED_POT_SIDE_LEFT, "entity/decorated_pot/heart_pottery_pattern"),
            ("right", &right, DECORATED_POT_SIDE_RIGHT, "entity/decorated_pot/danger_pottery_pattern"),
        ];
        let mut wrong: Vec<String> = Vec::new();
        for (name, inst, model, texture) in expect {
            if inst.model != model {
                wrong.push(format!("{name}: expected model {model}, got {}", inst.model));
            }
            if inst.texture != texture {
                wrong.push(format!("{name}: expected texture {texture}, got {}", inst.texture));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");

        // The base never varies with the sides.
        assert_eq!(base.model, DECORATED_POT_BASE);
        assert_eq!(base.texture, DECORATED_POT_BASE_TEXTURE_STEM);

        // Pairwise-distinct textures, not merely four non-default ones — a
        // resolver that mapped every side through the same lookup bug (e.g.
        // always the *first* sherd) would still clear the "not default"
        // check but fail this one.
        let textures = [front.texture, back.texture, left.texture, right.texture];
        for i in 0..textures.len() {
            for j in (i + 1)..textures.len() {
                assert_ne!(
                    textures[i], textures[j],
                    "sides at index {i} and {j} share a texture: {:?}",
                    textures
                );
            }
        }
    }

    /// A partially-decorated pot: only two of the four sides carry a sherd.
    /// The undecorated pair must still fall back to the default sprite while
    /// the decorated pair keeps its own — proving the fallback is per-side,
    /// not all-or-nothing.
    #[test]
    fn a_partially_decorated_pot_mixes_sherds_and_the_default_sprite() {
        let models = BlockEntityModelSet::load();
        let spawn = DecoratedPotSpawn {
            front: Some("brewer_pottery_sherd".to_string()),
            back: None,
            left: None,
            right: Some("miner_pottery_sherd".to_string()),
            ..DecoratedPotSpawn::at([0, 64, 0])
        };
        let [_base, front, back, left, right] = models
            .resolve_decorated_pot(&spawn)
            .expect("the decorated-pot corpus must resolve");

        assert_eq!(front.texture, "entity/decorated_pot/brewer_pottery_pattern");
        assert_eq!(right.texture, "entity/decorated_pot/miner_pottery_pattern");
        assert_eq!(back.texture, DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM);
        assert_eq!(left.texture, DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM);
    }

    /// [`decorated_pot_texture_stems`] must carry the base, the default side,
    /// and all twenty-three named patterns — the set
    /// [`block_entity_texture_stems`] (and so the shell's GPU texture loader)
    /// has to iterate for a pot with any combination of sherds to always find
    /// a bind group.
    #[test]
    fn decorated_pot_texture_stems_covers_the_base_default_and_every_pattern() {
        let stems = decorated_pot_texture_stems();
        assert!(stems.contains(&DECORATED_POT_BASE_TEXTURE_STEM));
        assert!(stems.contains(&DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM));
        assert_eq!(
            stems.len(),
            25,
            "expected base + default side + 23 sherd patterns, got {stems:?}"
        );
        assert!(block_entity_texture_stems().contains(&DECORATED_POT_BASE_TEXTURE_STEM));
    }

    /// The placement matrix's rotation term carries the `180°` offset chest's
    /// does not, and the pivot is the block's centre rather than its floor —
    /// both measured directly rather than merely "the pot draws somewhere",
    /// per [`decorated_pot_placement_matrix`]'s own doc on why this is not
    /// [`block_entity_placement_matrix`] with a yaw.
    #[test]
    fn decorated_pot_placement_uses_the_centre_pivot_and_the_180_degree_term() {
        let m = decorated_pot_placement_matrix([2, 5, 9], 0.0);
        // At `facing_yaw_deg == 0`, the rotation term is `180° - 0 = 180°`, a
        // half-turn about the block's own vertical centre line — so a point
        // on the +X face of the unit cube (local `(1, 0.5, 0.5)`) must land on
        // the -X side of the block after placement, not back on the +X side
        // the way an identity or a `0°` rotation would leave it.
        let p = m.transform_point3(Vec3::new(1.0, 0.5, 0.5));
        let origin_centre = Vec3::new(2.5, 5.5, 9.5);
        assert!(
            p.x < origin_centre.x,
            "expected the 180-degree term to flip +X across the block's centre; \
             got {p}, centre {origin_centre}"
        );
        // The centre pivot: a point already at local (0.5, 0.5, 0.5) is the
        // rotation axis itself and must map to the block's own centre exactly,
        // not the floor pivot chest's matrix uses.
        let centre = m.transform_point3(Vec3::splat(0.5));
        assert!(
            (centre - origin_centre).length() < 1e-5,
            "expected the centre point fixed at the block's own centre {origin_centre}, got {centre}"
        );
    }

    // --- conduit ---------------------------------------------------------

    /// Every offset the 42-cell "plus ring" candidate set contains — computed
    /// once here from the exact predicate `conduit_frame_scan` uses, so the
    /// fixture-building tests below can place blocks at a *chosen subset* of
    /// real candidates rather than guessing coordinates that might not
    /// qualify at all.
    fn conduit_frame_candidates() -> Vec<[i32; 3]> {
        let mut out = Vec::new();
        for ox in -2i32..=2 {
            for oy in -2i32..=2 {
                for oz in -2i32..=2 {
                    let (ax, ay, az) = (ox.abs(), oy.abs(), oz.abs());
                    let outside_inner = ax > 1 || ay > 1 || az > 1;
                    let on_plus_ring = (ox == 0 && (ay == 2 || az == 2))
                        || (oy == 0 && (ax == 2 || az == 2))
                        || (oz == 0 && (ax == 2 || ay == 2));
                    if outside_inner && on_plus_ring {
                        out.push([ox, oy, oz]);
                    }
                }
            }
        }
        out
    }

    /// Outside-arithmetic control on the scan's geometry alone, independent of
    /// which blocks are placed: exactly [`CONDUIT_FRAME_CANDIDATE_COUNT`] cells
    /// qualify, matching `ConduitBlockEntity.MIN_KILL_SIZE` — "hunting" is a
    /// full house, not a majority.
    #[test]
    fn conduit_frame_candidate_count_is_42_and_matches_min_kill_size() {
        let candidates = conduit_frame_candidates();
        assert_eq!(candidates.len(), CONDUIT_FRAME_CANDIDATE_COUNT as usize);
        let frame = conduit_frame_scan([0, 0, 0], |_| true, |_| true);
        assert_eq!(frame.effect_block_count, CONDUIT_FRAME_CANDIDATE_COUNT);
        assert!(frame.is_active());
        assert!(frame.is_hunting());
    }

    /// The conjunction trap: a room built entirely of frame blocks (all 42
    /// candidates present) is still an **empty** frame if even one cell of the
    /// conduit's own inner 3×3×3 is not water — vanilla's `updateShape`
    /// returns before the 5×5×5 pass ever runs. A resolver that scored the
    /// outer ring independently of the inner-cube gate would activate here;
    /// vanilla does not.
    #[test]
    fn conduit_frame_scan_requires_the_entire_inner_cube_to_be_water_even_with_every_frame_block_present()
     {
        let pos = [10, 20, 30];
        let missing = [pos[0], pos[1] + 1, pos[2]]; // one inner-cube cell, not water
        let frame = conduit_frame_scan(
            pos,
            |p| p != missing,
            |_| true, // every 5x5x5 candidate is a valid frame block
        );
        assert_eq!(
            frame.effect_block_count, 0,
            "one non-water inner cell must zero the whole frame even though \
             every outer candidate is a valid block"
        );
        assert!(!frame.is_active());
        assert!(!frame.is_hunting());
    }

    /// The discriminating pair for activation: 15 valid frame blocks (one
    /// short) against exactly 16 — `MIN_ACTIVE_SIZE`. Never "a bare conduit",
    /// which cannot tell "zero" from "any number below the threshold".
    #[test]
    fn conduit_frame_scan_discriminates_15_from_16_valid_blocks() {
        let pos = [0, 0, 0];
        let candidates = conduit_frame_candidates();
        for count in [15usize, 16usize] {
            let placed: std::collections::HashSet<[i32; 3]> = candidates[..count]
                .iter()
                .map(|o| [pos[0] + o[0], pos[1] + o[1], pos[2] + o[2]])
                .collect();
            let frame = conduit_frame_scan(pos, |_| true, |p| placed.contains(&p));
            assert_eq!(frame.effect_block_count, count as u32);
            assert_eq!(
                frame.is_active(),
                count >= 16,
                "count {count} active-ness mismatch"
            );
        }
    }

    /// The discriminating pair for hunting: 41 (active, not hunting) against
    /// exactly 42 — every candidate filled.
    #[test]
    fn conduit_frame_scan_requires_all_42_candidates_to_hunt() {
        let pos = [0, 0, 0];
        let candidates = conduit_frame_candidates();
        for count in [41usize, 42usize] {
            let placed: std::collections::HashSet<[i32; 3]> = candidates[..count]
                .iter()
                .map(|o| [pos[0] + o[0], pos[1] + o[1], pos[2] + o[2]])
                .collect();
            let frame = conduit_frame_scan(pos, |_| true, |p| placed.contains(&p));
            assert!(frame.is_active(), "{count} blocks must already be active");
            assert_eq!(
                frame.is_hunting(),
                count >= 42,
                "count {count} hunting mismatch"
            );
        }
    }

    /// `conduit_advance`'s two independent clauses: `tick_count` always steps,
    /// `active_rotation_ticks` only while active. Collected across a short
    /// active/inactive/active sequence and asserted as a table, not one
    /// `assert!` per iteration.
    #[test]
    fn conduit_advance_ticks_always_and_gates_rotation_on_active() {
        let steps = [true, true, false, false, true];
        let mut tick_count = 0u32;
        let mut active_rotation_ticks = 0u32;
        let mut rows = Vec::new();
        for active in steps {
            (tick_count, active_rotation_ticks) =
                conduit_advance(tick_count, active_rotation_ticks, active);
            rows.push((tick_count, active_rotation_ticks));
        }
        assert_eq!(
            rows,
            vec![(1, 1), (2, 2), (3, 2), (4, 2), (5, 3)],
            "tick_count must step every call; active_rotation_ticks only on an \
             active call"
        );
    }

    /// The unit trap, predicted exactly rather than sign-checked: the same
    /// `conduit_active_rotation_value` output is degrees in
    /// [`conduit_inactive_y_rot_radians`] and radians in
    /// [`conduit_active_axis_rotation_radians`] — a ~57× (`180/π`) gap between
    /// the two readings of the identical number. Both hypotheses computed from
    /// outside arithmetic (`f32::to_radians`, and the identity), not from a
    /// remembered literal.
    #[test]
    fn conduit_active_rotation_value_is_degrees_when_inactive_and_radians_when_active() {
        let active_ticks = 10u32;
        let partial = 0.5f32;

        let active_value = conduit_active_rotation_value(active_ticks, partial, true);
        let expected_active = (active_ticks as f32 + partial) * -0.0375;
        assert!((active_value - expected_active).abs() < 1e-6);
        // Active branch: used directly as radians.
        let active_axis_rad = conduit_active_axis_rotation_radians(active_value);
        assert!((active_axis_rad - expected_active).abs() < 1e-6);

        let inactive_value = conduit_active_rotation_value(active_ticks, partial, false);
        let expected_inactive = active_ticks as f32 * -0.0375;
        assert!(
            (inactive_value - expected_inactive).abs() < 1e-6,
            "inactive reading must drop the partial tick entirely"
        );
        // Inactive branch: the same *shape* of number, but read as degrees.
        let inactive_y_rot_rad = conduit_inactive_y_rot_radians(inactive_value);
        let expected_inactive_rad = expected_inactive.to_radians();
        assert!((inactive_y_rot_rad - expected_inactive_rad).abs() < 1e-6);

        // Isolate the two *readings* of one identical number (zero partial
        // tick, so the active branch's counter and the inactive branch's
        // counter coincide at `10`): applying `conduit_active_axis_rotation_radians`
        // (identity) against `conduit_inactive_y_rot_radians` (`to_radians`)
        // to the same value must differ by exactly `180/pi` — treating the
        // two readings as interchangeable is the exact bug this exists to
        // catch.
        let same_value = conduit_active_rotation_value(active_ticks, 0.0, true);
        assert!(
            (same_value - conduit_active_rotation_value(active_ticks, 0.0, false)).abs() < 1e-6,
            "zero partial tick must make the two branches' counters coincide"
        );
        let as_radians = conduit_active_axis_rotation_radians(same_value).abs();
        let as_degrees_then_radians = conduit_inactive_y_rot_radians(same_value).abs();
        let ratio = as_radians / as_degrees_then_radians;
        assert!(
            (ratio - 180.0 / std::f32::consts::PI).abs() < 1e-3,
            "expected the two readings of the same value to differ by exactly \
             180/pi (~57.2958x), got {ratio}x"
        );
    }

    /// `conduit_bob`'s exact formula at two discriminating `anim_time`s — `0`
    /// (`sin == 0`) and `5π` (`sin(0.5π) == 1`) — never a round guess.
    #[test]
    fn conduit_bob_matches_the_exact_formula_at_discriminating_anim_times() {
        let at_zero = conduit_bob(0.0);
        assert!(
            (at_zero - 0.75).abs() < 1e-5,
            "sin(0)=0 -> hh=0.5 -> hh*hh+hh=0.75, got {at_zero}"
        );
        let at_peak = conduit_bob(5.0 * std::f32::consts::PI);
        assert!(
            (at_peak - 2.0).abs() < 1e-4,
            "sin(pi/2)=1 -> hh=1.0 -> hh*hh+hh=2.0, got {at_peak}"
        );
    }

    /// `tickCount / 66 % 3` steps on **integer** ticks only — checked either
    /// side of both seams (`66`, `132`, `198`), the discriminating boundary
    /// inputs rather than round numbers.
    #[test]
    fn conduit_animation_phase_steps_every_66_ticks() {
        let cases = [
            (0u32, 0u8),
            (65, 0),
            (66, 1),
            (131, 1),
            (132, 2),
            (197, 2),
            (198, 0),
        ];
        let mismatches: Vec<_> = cases
            .into_iter()
            .filter_map(|(tick, expected)| {
                let got = conduit_animation_phase(tick);
                (got != expected).then_some((tick, expected, got))
            })
            .collect();
        assert!(mismatches.is_empty(), "{mismatches:?}");
    }

    /// The inactive branch resolves to exactly one instance — the shell —
    /// carrying the "degrees" reading of `active_rotation_value` threaded into
    /// its placement, matching [`conduit_inactive_y_rot_radians`] exactly
    /// (that function's own test pins down its arithmetic; this proves
    /// `resolve_conduit` actually plugs it in rather than the raw value).
    #[test]
    fn resolve_conduit_inactive_resolves_one_shell_instance_using_the_degrees_reading() {
        let models = BlockEntityModelSet::load();
        let spawn = ConduitSpawn {
            active_rotation_value: -30.0,
            ..ConduitSpawn::at([5, 10, -3])
        };
        let out = models.resolve_conduit(&spawn, Mat4::IDENTITY);
        assert_eq!(out.len(), 1, "inactive must resolve to exactly one instance");
        let inst = &out[0];
        assert_eq!(inst.model, CONDUIT_SHELL);
        assert_eq!(inst.texture, CONDUIT_SHELL_TEXTURE_STEM);

        let expected = Mat4::from_translation(Vec3::new(5.5, 10.5, -2.5))
            * Mat4::from_rotation_y(conduit_inactive_y_rot_radians(spawn.active_rotation_value));
        for (a, b) in inst.transform.to_cols_array().iter().zip(expected.to_cols_array()) {
            assert!((a - b).abs() < 1e-4, "{:?} != {:?}", inst.transform, expected);
        }
    }

    /// The active branch resolves to exactly four instances — cage, both wind
    /// planes, and the eye — with the right `(model, texture)` pairs, and the
    /// cage/eye share the bob height while **both** wind planes stay fixed at
    /// `y = 0.5`: the adjacent-same-shaped-expression trap this module's own
    /// doc comment calls out, checked with `hh != 1` so the two Y values
    /// cannot coincide by accident.
    #[test]
    fn resolve_conduit_active_resolves_cage_wind_wind_eye_with_the_bob_on_only_two_of_them() {
        let models = BlockEntityModelSet::load();
        // `anim_time` chosen so `conduit_bob` is not `1.0` (which would make
        // `bob == 0.5`, coincident with the wind planes' fixed value and
        // unable to distinguish the two).
        let anim_time = 1.0;
        let hh = conduit_bob(anim_time);
        let bob = 0.3 + hh * 0.2;
        assert!(
            (bob - 0.5).abs() > 1e-3,
            "bob {bob} must differ from the wind planes' fixed 0.5 for this test to discriminate"
        );
        let spawn = ConduitSpawn {
            active: true,
            hunting: false,
            active_rotation_value: 0.0,
            anim_time,
            animation_phase: 0,
            ..ConduitSpawn::at([0, 0, 0])
        };
        let out = models.resolve_conduit(&spawn, Mat4::IDENTITY);
        assert_eq!(out.len(), 4, "active must resolve to exactly four instances");

        let mut by_model: std::collections::HashMap<&str, Vec<&BlockEntityInstance>> =
            std::collections::HashMap::new();
        for inst in &out {
            by_model.entry(inst.model).or_default().push(inst);
        }
        assert_eq!(by_model.get(CONDUIT_CAGE).map(Vec::len), Some(1));
        assert_eq!(by_model.get(CONDUIT_WIND).map(Vec::len), Some(2));
        assert_eq!(by_model.get(CONDUIT_EYE).map(Vec::len), Some(1));

        let cage = by_model[CONDUIT_CAGE][0];
        assert_eq!(cage.texture, CONDUIT_CAGE_TEXTURE_STEM);
        let eye = by_model[CONDUIT_EYE][0];
        assert_eq!(eye.texture, CONDUIT_CLOSED_EYE_TEXTURE_STEM);

        let y_of = |inst: &BlockEntityInstance| inst.transform.col(3).y;
        assert!(
            (y_of(cage) - bob).abs() < 1e-4,
            "cage Y {} != bob {bob}",
            y_of(cage)
        );
        assert!(
            (y_of(eye) - bob).abs() < 1e-4,
            "eye Y {} != bob {bob}",
            y_of(eye)
        );
        for wind in &by_model[CONDUIT_WIND] {
            assert!(
                (y_of(wind) - 0.5).abs() < 1e-4,
                "wind plane Y {} must stay fixed at 0.5, not the bob {bob}",
                y_of(wind)
            );
        }
        // The two wind instances share a texture but must not share a
        // transform (the second carries the extra `scale(0.875)` +
        // `rotationXYZ` clause).
        assert_ne!(
            by_model[CONDUIT_WIND][0].transform, by_model[CONDUIT_WIND][1].transform,
            "the two wind planes must be posed differently"
        );
    }

    /// Both wind planes switch texture together with `animation_phase == 1`,
    /// and only then.
    #[test]
    fn resolve_conduit_wind_texture_follows_animation_phase() {
        let models = BlockEntityModelSet::load();
        for (phase, expected) in [
            (0u8, CONDUIT_WIND_TEXTURE_STEM),
            (1u8, CONDUIT_WIND_VERTICAL_TEXTURE_STEM),
            (2u8, CONDUIT_WIND_TEXTURE_STEM),
        ] {
            let spawn = ConduitSpawn {
                active: true,
                animation_phase: phase,
                ..ConduitSpawn::at([0, 0, 0])
            };
            let out = models.resolve_conduit(&spawn, Mat4::IDENTITY);
            let winds: Vec<_> = out.iter().filter(|i| i.model == CONDUIT_WIND).collect();
            assert_eq!(winds.len(), 2);
            for w in winds {
                assert_eq!(w.texture, expected, "phase {phase}");
            }
        }
    }

    /// The eye's sprite follows `hunting`, independent of everything else
    /// about the spawn.
    #[test]
    fn resolve_conduit_eye_texture_follows_hunting() {
        let models = BlockEntityModelSet::load();
        for (hunting, expected) in [
            (false, CONDUIT_CLOSED_EYE_TEXTURE_STEM),
            (true, CONDUIT_OPEN_EYE_TEXTURE_STEM),
        ] {
            let spawn = ConduitSpawn {
                active: true,
                hunting,
                ..ConduitSpawn::at([0, 0, 0])
            };
            let out = models.resolve_conduit(&spawn, Mat4::IDENTITY);
            let eye = out.iter().find(|i| i.model == CONDUIT_EYE).expect("eye");
            assert_eq!(eye.texture, expected, "hunting {hunting}");
        }
    }

    /// Only the eye reads `camera_orientation` — vanilla's
    /// `poseStack.mulPose(camera.orientation)` sits inside the eye's own
    /// `pushPose`/`popPose` pair alone. Changing it must move the eye's
    /// transform and leave the cage's and both wind planes' untouched.
    #[test]
    fn resolve_conduit_only_the_eye_billboards_with_camera_orientation() {
        let models = BlockEntityModelSet::load();
        let spawn = ConduitSpawn {
            active: true,
            ..ConduitSpawn::at([0, 0, 0])
        };
        let identity = models.resolve_conduit(&spawn, Mat4::IDENTITY);
        let rotated = models.resolve_conduit(&spawn, Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2));

        let by_model = |out: &[BlockEntityInstance], model: &str| -> Vec<Mat4> {
            out.iter().filter(|i| i.model == model).map(|i| i.transform).collect()
        };
        assert_eq!(by_model(&identity, CONDUIT_CAGE), by_model(&rotated, CONDUIT_CAGE));
        assert_eq!(by_model(&identity, CONDUIT_WIND), by_model(&rotated, CONDUIT_WIND));
        assert_ne!(
            by_model(&identity, CONDUIT_EYE),
            by_model(&rotated, CONDUIT_EYE),
            "the eye must be the one instance that moves with camera orientation"
        );
    }

    /// [`conduit_texture_stems`] and [`block_entity_texture_stems`] both carry
    /// all six conduit sheets — the loader's own enumeration, checked as a set
    /// rather than assuming the union function forwards correctly (the
    /// `Arc<S>`-forwarding class of bug this repo has shipped before: adding a
    /// stem list without adding it to the aggregator compiles clean and stays
    /// silently short).
    #[test]
    fn conduit_texture_stems_are_all_six_and_reach_the_aggregate_loader_list() {
        let stems = conduit_texture_stems();
        assert_eq!(stems.len(), 6, "{stems:?}");
        let all = block_entity_texture_stems();
        let missing: Vec<_> = stems.iter().filter(|s| !all.contains(s)).collect();
        assert!(
            missing.is_empty(),
            "conduit_texture_stems entries missing from block_entity_texture_stems: {missing:?}"
        );
    }
}
