use glam::{Mat4, Vec3};
use lodestone_assets::entity::{EntityModelDef, PartPose, bake_entity_parts};
use lodestone_model::{CampfireSlot, ShelfSlot};

use crate::entity::{ENTITY_FULLBRIGHT, PartRange, push_part_quads};
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
/// Mirrors vanilla's own three-way chest-type property (single/left/right).
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
/// Mirrors vanilla's own chest material-type resolution, which picks the
/// sheet a chest is drawn with.
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
    /// The seasonal override, applied around the winter holiday.
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
    /// This is the *block*-driven half of vanilla's own chest-material resolution; the
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
/// Vanilla's own chest-material resolution checks copper **first**, then ender, and
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
/// material/half pair — vanilla's own sprite-choosing and default-texture
/// naming, which is `<prefix>`/`<prefix>_left`/`<prefix>_right`.
///
/// **Ender is deliberately half-independent.** Vanilla's own sprite-choosing
/// resolves every chest-type value to the single ender-chest texture
/// location, and the jar ships only
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

/// Vanilla's cubic ease-out on a chest's raw openness:
/// `open = 1 - open; open = 1 - open³`.
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
/// openness — vanilla's own model pose: `lid.xRot = -(open * PI/2)`.
///
/// Negative: the lid tips backwards, away from the chest's facing.
#[must_use]
pub fn chest_lid_x_rot(eased_openness: f32) -> f32 {
    -(eased_openness * std::f32::consts::FRAC_PI_2)
}

/// The world placement transform for a block entity at `pos` facing
/// `facing_yaw_deg`.
///
/// A rotation about the block's vertical centre, by negative the facing's
/// yaw, composed with the block's own translation. `facing_yaw_deg` is
/// vanilla's own horizontal-facing-to-yaw convention (south `0`, west `90`,
/// north `180`, east `270`) — the **same convention** the entity path's
/// `body_yaw_deg` uses, so a caller does not have to remember two.
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

/// Vanilla's own facing-to-yaw mapping for its four horizontal facing names,
/// or `None` for a value that is not a horizontal direction.
///
/// South is `0` because that is what vanilla's own facing-to-yaw mapping
/// returns, not because it is the natural choice — reading this off vanilla's
/// facing enum's declaration order instead gives down/up/north/south/west/east
/// and rotates every chest by a quarter turn.
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

/// Which of vanilla's skull/head types this renderer draws — all seven
/// vanilla defines.
///
/// Five of them share one CPU model (a single 8×8×8 head box —
/// see `lodestone_assets::block_entity_models::skull_mob_model`'s doc) and
/// differ only by canvas size and sheet. `Dragon` and `Piglin` do not: each
/// has its own multi-part rig on its own sheet, and each also *poses* a
/// child part every frame, which is why
/// [`BlockEntityModelSet::resolve_skull`] has overrides at all.
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
    /// `minecraft:player_head`/`player_wall_head`. Its spawn carries either
    /// the default Steve sheet or the placed head's decoded remote-skin URL.
    Player,
    /// `minecraft:dragon_head`/`dragon_wall_head` — the ender dragon's own
    /// head rig on the dragon sheet, with a posed jaw.
    Dragon,
    /// `minecraft:piglin_head`/`piglin_wall_head` — the piglin skull with its
    /// two posed ears.
    Piglin,
}

/// Model name of the 64×32-canvas skull head (skeleton/wither skeleton/creeper).
pub const SKULL_MOB: &str = "skull_mob";
/// Model name of the 64×64-canvas skull head (zombie/player).
pub const SKULL_HUMANOID: &str = "skull_humanoid";
/// Model name of the ender dragon's head rig —
/// [`lodestone_assets::block_entity_models::dragon_head_model`].
pub const SKULL_DRAGON: &str = "skull_dragon";
/// Model name of the piglin head rig —
/// [`lodestone_assets::block_entity_models::piglin_head_model`].
pub const SKULL_PIGLIN: &str = "skull_piglin";

/// The `"jaw"` child of [`SKULL_DRAGON`], posed by
/// [`dragon_head_jaw_x_rot`].
pub const DRAGON_HEAD_JAW_PART: &str = "jaw";
/// The two ear children of [`SKULL_PIGLIN`], posed by
/// [`piglin_head_ear_z_rots`], in that function's return order.
pub const PIGLIN_HEAD_EAR_PARTS: [&str; 2] = ["left_ear", "right_ear"];

impl SkullType {
    /// Resolves a block's registry path (namespace stripped, wall/floor
    /// suffix included) to its skull type, or `None` for a path that is not a
    /// skull at all.
    #[must_use]
    pub fn from_block_path(path: &str) -> Option<Self> {
        Some(match path {
            "skeleton_skull" | "skeleton_wall_skull" => SkullType::Skeleton,
            "wither_skeleton_skull" | "wither_skeleton_wall_skull" => SkullType::WitherSkeleton,
            "zombie_head" | "zombie_wall_head" => SkullType::Zombie,
            "creeper_head" | "creeper_wall_head" => SkullType::Creeper,
            "player_head" | "player_wall_head" => SkullType::Player,
            "dragon_head" | "dragon_wall_head" => SkullType::Dragon,
            "piglin_head" | "piglin_wall_head" => SkullType::Piglin,
            _ => return None,
        })
    }

    /// The baked model this type draws with.
    #[must_use]
    pub const fn model(self) -> &'static str {
        match self {
            SkullType::Skeleton | SkullType::WitherSkeleton | SkullType::Creeper => SKULL_MOB,
            SkullType::Zombie | SkullType::Player => SKULL_HUMANOID,
            SkullType::Dragon => SKULL_DRAGON,
            SkullType::Piglin => SKULL_PIGLIN,
        }
    }
}

/// The animation position every placed skull in this client draws at.
///
/// Vanilla's own animation position is produced by its own per-block-entity animation
/// accessor, a per-block tick counter that **only advances while the block state's `powered`
/// property is set** and that freezes rather than resets when power is
/// removed. It is therefore per-block-entity state this client does not carry:
/// there is no skull tick tracker the way there is a conduit one, and
/// `SkullSpawn` deliberately has no field for it rather than a field no
/// producer ever writes.
///
/// The consequence is exact and small: an *unpowered* skull — which is every
/// skull in a world nobody has wired to redstone — draws at `0.0` and is
/// pixel-correct, and a powered dragon head does not chomp and a powered
/// piglin head's ears do not wobble. Wiring it is a producer change (a
/// per-position counter in the shell, gated on `powered`), not a change to
/// [`dragon_head_jaw_x_rot`] or [`piglin_head_ear_z_rots`], which already take
/// the position as an argument.
pub const SKULL_RESTING_ANIMATION_POS: f32 = 0.0;

/// The dragon head's jaw angle — vanilla's own model pose:
/// `jaw.rot_x = (sin(animation_pos * PI * 0.2) + 1) * 0.2`.
///
/// Note this is **never zero**: at rest (`animation_pos == 0`) it is `0.2`
/// radians, so the authored `PartPose` the jaw carries in the mesh is not the
/// angle a dragon head is ever drawn at. Reading the mesh's rest pose instead
/// of calling this gives a jaw clamped shut.
#[must_use]
pub fn dragon_head_jaw_x_rot(animation_pos: f32) -> f32 {
    ((animation_pos * std::f32::consts::PI * 0.2).sin() + 1.0) * 0.2
}

/// The piglin head's two ear angles as `(left, right)` —
/// vanilla's own model pose:
///
/// ```text
/// left_ear.rot_z  = -(cos(animation_pos * PI * 0.2 * 1.2) + 2.5) * 0.2
/// right_ear.rot_z =  (cos(animation_pos * PI * 0.2      ) + 2.5) * 0.2
/// ```
///
/// The `1.2` on the left ear only — vanilla names it `asymmetry` — is the
/// whole point: with it the two ears drift out of phase, without it they are
/// mirror images forever and the animation reads as a single rocking motion.
/// It is also invisible at rest, where both sides evaluate to `±0.7`, so a
/// fixture at `animation_pos == 0` cannot tell the two hypotheses apart.
///
/// Like the jaw, these override rather than add to the authored `±PI/6` rest
/// pose, and `±0.7` is not `±PI/6` (`≈ ±0.5236`).
#[must_use]
pub fn piglin_head_ear_z_rots(animation_pos: f32) -> (f32, f32) {
    let left = -((animation_pos * std::f32::consts::PI * 0.2 * 1.2).cos() + 2.5) * 0.2;
    let right = ((animation_pos * std::f32::consts::PI * 0.2).cos() + 2.5) * 0.2;
    (left, right)
}

/// The jar sheet a [`SkullType`] draws with — vanilla's own type-to-skin
/// table, minus the `.png`/`assets/<ns>/textures/` wrapping.
///
/// **These are the mob skins already on disk for entity rendering, not a new
/// asset family.** `resources::load_block_entity_textures` (the shell's
/// loader) has to load them a second time regardless — this pass keeps its
/// own texture bind groups, entirely separate from vanilla's own entity
/// renderer's — but there is nothing to author or ship beyond this stem list.
#[must_use]
pub const fn skull_texture_stem(skull_type: SkullType) -> &'static str {
    match skull_type {
        SkullType::Skeleton => "entity/skeleton/skeleton",
        SkullType::WitherSkeleton => "entity/skeleton/wither_skeleton",
        SkullType::Zombie => "entity/zombie/zombie",
        SkullType::Creeper => "entity/creeper/creeper",
        SkullType::Player => "entity/player/wide/steve",
        SkullType::Dragon => "entity/enderdragon/dragon",
        SkullType::Piglin => "entity/piglin/piglin",
    }
}

/// Every skull type, for enumerating stems and exhaustiveness in tests.
pub const SKULL_TYPES: &[SkullType] = &[
    SkullType::Skeleton,
    SkullType::WitherSkeleton,
    SkullType::Zombie,
    SkullType::Creeper,
    SkullType::Player,
    SkullType::Dragon,
    SkullType::Piglin,
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
    /// A floor-placed skull's `rotation` property, `0..16` — vanilla's own
    /// 16-step rotation segment (16 steps of 22.5°, **not**
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
        /// Vanilla's own facing-to-yaw mapping of the `facing` property.
        facing_yaw_deg: f32,
    },
}

/// The world placement transform for a floor-standing skull — vanilla's own
/// ground-placement transform: translate to the block's centre, rotate about
/// Y by negative the segment's degrees, then flip X and Y
/// (`scale(-1, -1, 1)`), composed with the block's own translation.
///
/// **This is the one block-entity placement in this module that *does*
/// flip** (`scale(-1, -1, 1)`, matching
/// [`crate::entity::entity_model_matrix`]'s sign exactly) — unlike
/// [`block_entity_placement_matrix`]. The skull's head box is authored in
/// the same Y-down convention as a mob's head part, and vanilla never
/// re-authors it block-space-up the way the chest model was; see
/// `lodestone_assets::block_entity_models::skull_head_part`'s doc.
/// `rotation_segment` is vanilla's own 16-step rotation segment (16 steps of
/// 22.5°), not [`horizontal_facing_yaw`]'s four-value convention — segment
/// `0` is **north** (vanilla's own facing-to-yaw mapping puts north at
/// `180`), so do not reuse that helper here.
#[must_use]
pub fn skull_ground_placement_matrix(pos: [i32; 3], rotation_segment: u8) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    let segment_deg = f32::from(rotation_segment) * (360.0 / 16.0);
    Mat4::from_translation(origin)
        * Mat4::from_translation(Vec3::new(0.5, 0.0, 0.5))
        * Mat4::from_rotation_y(-segment_deg.to_radians())
        * Mat4::from_scale(Vec3::new(-1.0, -1.0, 1.0))
}

/// The world placement transform for a wall-mounted skull — vanilla's own
/// wall-placement transform:
/// `translate(0.5 − dir.stepX·0.25, 0.25, 0.5 − dir.stepZ·0.25) · rotY(−opposite(dir).toYRot()) · scale(−1,−1,1)`.
///
/// Vanilla's own per-direction step vector components are recovered from
/// `facing_yaw_deg` by trig rather than a second lookup table that could
/// drift from [`horizontal_facing_yaw`]'s: south `0° → (0, 1)`, west
/// `90° → (−1, 0)`, north `180° → (0, −1)`, east `270° → (1, 0)` —
/// hand-verified against vanilla's own facing enum, not derived from this
/// function.
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

/// The world placement transform for a ground/standing banner — vanilla's
/// own model-transformation / ground-placement construction:
///
/// ```text
/// MODEL_TRANSLATION = (0.5, 0.0, 0.5)
/// MODEL_SCALE       = (0.6666667, -0.6666667, -0.6666667)
/// transform(MODEL_TRANSLATION, rotate_y_degrees(-angle), MODEL_SCALE, none)
/// angle = segment * 22.5   // the rotation segment converted to degrees
/// ```
///
/// # A third placement shape, not a variant of the other two
///
/// [`block_entity_placement_matrix`]/[`skull_ground_placement_matrix`] both
/// exist because chest/skull geometry is baked *corner*-anchored, so rotating
/// it in place needs a pivot: `translate(pivot) · rotate · translate(-pivot)`.
/// A banner's model space is **not** corner-anchored — the banner flag
/// model's own part offset already positions the flag relative to an origin
/// the same way an entity's skeleton does — so vanilla itself uses a straight
/// `T · R · S` here instead, confirmed against vanilla's own generic
/// transform-composition helper (translate, then rotate by the left
/// rotation, then scale, with the right rotation unused since this call
/// passes none for it): `M = T * R * S`, scale applied to the model first, then rotated,
/// then translated to the block. [`banner_flag_placement_verifies_against_the_transformation_compose_formula`]
/// pins this against that literal formula rather than against this function's
/// own arithmetic restated.
///
/// # The `2/3` scale and the Y/Z flip are both real
///
/// The banner and banner-flag models are shared with the banner **item**'s
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

/// The world placement transform for a **wall** banner — vanilla's own
/// wall-placement construction, which composes its ground-placement
/// transform with the wall direction's yaw.
///
/// Byte-for-byte [`banner_ground_placement_matrix`] with a different angle:
/// both go through the same model-transformation construction, so the
/// translation `(0.5, 0, 0.5)` and the `(2/3, -2/3, -2/3)` scale are shared
/// and there is **no** extra push away from the wall — unlike
/// [`skull_wall_placement_matrix`], which has a `0.25` offset. Adding one here
/// on the assumption that "wall placements offset" would float the banner a
/// quarter block off the block face; the offset a wall banner needs is
/// already baked into its own mesh's `z` origins
/// (`lodestone_assets::block_entity_models::banner_wall_body_model`), which is
/// why the wall geometry is a second mesh rather than the standing one moved.
#[must_use]
pub fn banner_wall_placement_matrix(pos: [i32; 3], facing_yaw_deg: f32) -> Mat4 {
    banner_placement_matrix(pos, facing_yaw_deg)
}

/// Vanilla's own model-transformation construction, shared by both
/// attachments: translate `(0.5, 0, 0.5)`, rotate about Y by `-angle`, scale
/// `(2/3, -2/3, -2/3)`,
/// composed as `T · R · S` — see [`banner_ground_placement_matrix`]'s doc for why
/// this is `T · R · S` and not the pivot sandwich chest and skull use.
fn banner_placement_matrix(pos: [i32; 3], angle_deg: f32) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    Mat4::from_translation(origin)
        * Mat4::from_translation(Vec3::new(0.5, 0.0, 0.5))
        * Mat4::from_rotation_y(-angle_deg.to_radians())
        * Mat4::from_scale(Vec3::new(2.0 / 3.0, -2.0 / 3.0, -2.0 / 3.0))
}

/// Per-block-position phase offset for a banner's cloth sway — vanilla's own
/// per-frame render-state extraction:
///
/// ```text
/// phase = (floorMod(x*7 + y*9 + z*13 + gameTime, 100) + partialTicks) / 100
/// ```
///
/// So neighbouring banners do not sway in lockstep, and the phase advances
/// one step per game tick, wrapping every 100 ticks. `game_time` is the
/// world's raw tick counter (vanilla's own world-time accessor); `partial_tick` is the
/// usual sub-tick interpolation fraction, `0.0..1.0`.
#[must_use]
pub fn banner_phase(pos: [i32; 3], game_time: i64, partial_tick: f32) -> f32 {
    let sum = i64::from(pos[0]) * 7 + i64::from(pos[1]) * 9 + i64::from(pos[2]) * 13 + game_time;
    // `floorMod`, not Rust's `%` (which truncates toward zero): a negative
    // block coordinate must still wrap into `0..100`, not go negative.
    let wrapped = sum.rem_euclid(100);
    (wrapped as f32 + partial_tick) / 100.0
}

/// The flag part's `x_rot` override for a given phase — vanilla's own model
/// pose:
///
/// ```text
/// flag.rot_x = (-0.0125 + 0.01 * cos(2*PI*phase)) * PI
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

/// The jar sheet a bell draws with — vanilla's own bell-texture constant,
/// resolved from `"bell/bell_body"`.
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

/// A bell's shake direction — vanilla's own per-instance shake-direction
/// field, the four horizontal directions a player (or projectile) can hit a
/// bell from. `Option<BellShakeDirection>` (not a fifth "none" variant)
/// mirrors the jar's own nullable direction field, and matches how
/// [`BellSpawn::shake`] spells "at rest".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BellShakeDirection {
    /// North.
    North,
    /// South.
    South,
    /// East.
    East,
    /// West.
    West,
}

/// The bell body's `(x_rot, z_rot)` in radians for a shake in progress —
/// vanilla's own model pose:
///
/// ```text
/// baseRot = sin(ticks / PI) / (4 + ticks / 3)
/// NORTH: xRot = -baseRot   SOUTH: xRot = +baseRot
/// EAST:  zRot = -baseRot   WEST:  zRot = +baseRot
/// ```
///
/// `direction = None` returns `(0.0, 0.0)` without evaluating `base_rot` at
/// all, mirroring vanilla's own model pose function's null-direction guard
/// rather than computing the ratio and multiplying by a zero
/// that never appears in the real formula — there is no direction to carry a
/// sign for a bell at rest, so a literal port has nothing to multiply.
///
/// `ticks` is vanilla's raw per-block tick counter (`0..50`, **not** eased or
/// clamped here) plus partial tick, exactly as vanilla's own per-frame
/// render-state extraction passes it — unlike [`chest_lid_openness`], there is
/// only one transform here because vanilla itself has only one; its model
/// pose computes the angle directly from ticks with no separate easing pass.
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
/// Model name of a **wall** banner's bar — vanilla's own wall-body-layer
/// construction, which has no pole at all.
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
/// select a different **mesh**, since vanilla's own wall-body-layer
/// construction drops the pole.
///
/// No `Eq`/`Hash`, unlike [`SkullOrientation`]: the wall arm carries a yaw as an
/// `f32`, matching every other placement input in this module rather than
/// re-encoding four directions as an enum only this type would use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BannerAttachment {
    /// A standing banner, with the `ROTATION` property.
    Ground {
        /// The `ROTATION` block-state property, `0..16` — vanilla's own
        /// 16-step rotation segment, segment `0` being north.
        rotation_segment: u8,
    },
    /// A wall banner, with `FACING`.
    Wall {
        /// Vanilla's own facing-to-yaw mapping of the wall banner's `FACING`
        /// property (south `0`, west `90`, north `180`, east `270`) —
        /// [`horizontal_facing_yaw`]'s convention.
        facing_yaw_deg: f32,
    },
}

/// The jar sheet a banner's *body* (pole/bar) and *flag* both draw with for
/// their opaque pass — vanilla's own banner-base sprite constant, resolved
/// to `entity/banner/banner_base`. This is the plain wood/cloth texture,
/// never a pattern mask: vanilla's own banner-submission function passes
/// this one sprite id to *both* the body and flag model draws before its
/// pattern pass draws anything coloured over the flag a second time.
pub const BANNER_BASE_TEXTURE_STEM: &str = "entity/banner/banner_base";

/// The one banner sheet stem, for [`block_entity_texture_stems`] — mirrors
/// [`bell_texture_stems`]'s shape: one stem shared by all four banner models
/// (standing and wall, body and flag), because vanilla's own
/// banner-submission function passes the same base sprite to every one of
/// them.
#[must_use]
pub fn banner_texture_stems() -> Vec<&'static str> {
    vec![BANNER_BASE_TEXTURE_STEM]
}

/// Model name of a shield's `plate`+`handle` mesh (`lodestone_assets::
/// block_entity_models::shield_model`) — see that function's doc for why a
/// shield, unlike a banner, has only one mesh, reused for every draw.
pub const SHIELD: &str = "shield";

/// Vanilla's own no-pattern shield-base sprite constant —
/// `entity/shield/shield_base_nopattern`,
/// the plain wood-and-iron sheet every shield with no `minecraft:base_color`
/// and no stored `minecraft:banner_patterns` layer draws (the common case,
/// straight off a crafting table).
pub const SHIELD_BASE_NO_PATTERN_TEXTURE_STEM: &str = "entity/shield/shield_base_nopattern";

/// Vanilla's own shield-base sprite constant — `entity/shield/shield_base`,
/// the sheet a shield with a base colour or at least one loom pattern draws
/// its **opaque** pass
/// with (a plain grey canvas the translucent pattern layers tint and mask
/// over, mirroring [`BANNER_BASE_TEXTURE_STEM`]'s role for a banner).
pub const SHIELD_BASE_TEXTURE_STEM: &str = "entity/shield/shield_base";

/// Whether a shield stack's translucent pattern pass draws at all —
/// vanilla's own has-patterns check: `true` when
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

/// Vanilla's sixteen dye colours, in vanilla's own dye-colour **ordinal**
/// order — which is what its own shulker-box sprite lookup indexes.
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

/// Vanilla's own default shulker-texture location — the undyed box's sheet,
/// resolved from `"shulker"`.
pub const SHULKER_DEFAULT_TEXTURE_STEM: &str = "entity/shulker/shulker";

/// The sheet stem for one shulker box, by dye colour name, or the undyed sheet
/// for `None` — vanilla's own renderer's null-colour fork.
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
/// vanilla's own model-transform construction:
///
/// ```text
/// translation(0.5, 0.5, 0.5) · scale(0.9995) · rotate(facing.getRotation())
///   · scale(1, -1, -1) · translate(0, -1, 0)
/// ```
///
/// **This is not [`block_entity_placement_matrix`] with a yaw.** Three things
/// differ and each is visible: the pivot is the block's *centre* `(0.5, 0.5,
/// 0.5)` rather than its floor `(0.5, 0, 0.5)`; a shulker box can face **up or
/// down**, so the rotation is a full direction-to-rotation quaternion (vanilla's
/// own six-way mapping) and not a Y yaw; and it carries the `scale(1, -1, -1)`
/// entity flip and the `-1` lift that vanilla's own skull renderer also has and
/// its chest renderer does not. Reusing the chest matrix draws an upside-down
/// box on the floor for `facing=up`, which is the common case.
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

/// Which face a shulker box's lid opens toward — the block's own FACING
/// property, one of all six directions rather than the four horizontals a
/// chest has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ShulkerFacing {
    /// UP, and vanilla's own default when the FACING property is absent.
    #[default]
    Up,
    /// DOWN.
    Down,
    /// NORTH.
    North,
    /// SOUTH.
    South,
    /// WEST.
    West,
    /// EAST.
    East,
}

impl ShulkerFacing {
    /// Vanilla's direction-to-rotation function, as a rotation matrix.
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
/// vanilla's own lid animation update:
/// `lid.set_pos(0, 24 - progress * 0.5 * 16, 0)` and
/// `lid.rot_y = 270° * progress`.
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

/// The jar sheet the pot's base always draws with — vanilla's own
/// decorated-pot-base sheet mapping (`entity/decorated_pot/decorated_pot_base`).
pub const DECORATED_POT_BASE_TEXTURE_STEM: &str = "entity/decorated_pot/decorated_pot_base";

/// The jar sheet an undecorated side draws with — vanilla's own
/// decorated-pot-side sheet mapping (`entity/decorated_pot/decorated_pot_side`),
/// the decorated-pot renderer's own side-sprite fallback for an absent or
/// unrecognised sherd.
pub const DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM: &str =
    "entity/decorated_pot/decorated_pot_side";

/// A stored sherd's item path (namespace stripped, e.g.
/// `"angler_pottery_sherd"`) to the sherd's own pattern texture stem —
/// vanilla's own item-to-pattern mapping table plus each pattern's registered
/// asset id, transcribed from vanilla's decorated-pot-patterns class's own
/// bootstrap listing (26.2's decompiled source) — the pattern's registered
/// asset id, not a guess from the sherd's own name (they happen to share a
/// `<name>_pottery_pattern`/`<name>_pottery_sherd` stem, but that is a jar
/// convention this reads off the real registration, not an assumption).
///
/// `None` (an absent side, or an item path this table does not recognise —
/// vanilla's own side-sprite lookup falls back identically for a missing map
/// entry) maps to [`DECORATED_POT_SIDE_DEFAULT_TEXTURE_STEM`] by the caller,
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
/// `facing_yaw_deg` — vanilla's own model-transformation construction:
/// rotate about Y by `(180.0F - facing_yaw)` around the pivot
/// `(0.5F, 0.5F, 0.5F)`.
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
/// ordered `[back, left, right, front]` per vanilla's own pot-decorations
/// codec → the four `Option<String>` fields (`None` for an absent side —
/// vanilla's own pot-decorations type maps a stored `minecraft:brick` to
/// absent at parse time, so a caller reading raw NBT should do the same
/// rather than passing `Some("brick")` through); world light → `light`.
///
/// No wobble phase: the decorated pot's hit-wobble is a block-event-driven
/// animation with no producer in this workspace yet, the same
/// not-yet-triggered gap [`ShulkerSpawn::progress`] documents for the lid.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DecoratedPotSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// The facing direction's yaw, for the block's `HORIZONTAL_FACING`.
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

/// Model name of the conduit's inactive shell — vanilla's own conduit
/// shell model layer, the 6×6×6 layer.
pub const CONDUIT_SHELL: &str = "conduit_shell";

/// Model name of the conduit's active shell ("cage") — vanilla's own
/// conduit cage model layer, the 8×8×8 layer. A distinct mesh from
/// [`CONDUIT_SHELL`], not a scaled reuse — see
/// [`lodestone_assets::block_entity_models::conduit_cage_model`]'s doc.
pub const CONDUIT_CAGE: &str = "conduit_cage";

/// Model name of the conduit's wind plane — vanilla's own conduit wind
/// model layer, one 16×16×16 cube drawn twice per active frame at two
/// different poses sharing this one mesh.
pub const CONDUIT_WIND: &str = "conduit_wind";

/// Model name of the conduit's billboarded eye — vanilla's own conduit eye
/// model layer, the near-planar 8×8 box.
pub const CONDUIT_EYE: &str = "conduit_eye";

/// Vanilla's own inactive-shell texture — `entity/conduit/base`, the inactive shell's
/// sheet.
pub const CONDUIT_SHELL_TEXTURE_STEM: &str = "entity/conduit/base";

/// Vanilla's own active-cage texture — `entity/conduit/cage`, the active
/// cage's sheet.
pub const CONDUIT_CAGE_TEXTURE_STEM: &str = "entity/conduit/cage";

/// Vanilla's own wind texture — `entity/conduit/wind`, used for both wind
/// planes whenever [`ConduitSpawn::animation_phase`] is not `1`.
pub const CONDUIT_WIND_TEXTURE_STEM: &str = "entity/conduit/wind";

/// Vanilla's own vertical-wind texture — `entity/conduit/wind_vertical`,
/// used for both wind planes when [`ConduitSpawn::animation_phase`] **is**
/// `1`. Same UV layout as [`CONDUIT_WIND_TEXTURE_STEM`], different sheet —
/// picking the texture without also picking the plane's own rotation
/// (`wind1_rot`'s `1 =>` arm) draws a plane that spins but never actually
/// turns to face the direction its "vertical" sheet implies.
pub const CONDUIT_WIND_VERTICAL_TEXTURE_STEM: &str = "entity/conduit/wind_vertical";

/// Vanilla's own open-eye texture — `entity/conduit/open_eye`, drawn while
/// [`ConduitSpawn::hunting`].
pub const CONDUIT_OPEN_EYE_TEXTURE_STEM: &str = "entity/conduit/open_eye";

/// Vanilla's own closed-eye texture — `entity/conduit/closed_eye`, drawn
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
    /// Vanilla's own active check — [`ConduitFrame::is_active`].
    pub active: bool,
    /// Vanilla's own hunting check — [`ConduitFrame::is_hunting`]. Only
    /// meaningful (and only ever read) while `active`; vanilla's own hunting
    /// flag can theoretically be true while its active flag is false for one
    /// frame of hysteresis, but the inactive branch never reads it, so
    /// [`Self::resolve_conduit`]/[`BlockEntityModelSet::resolve_conduit`] does
    /// not either.
    pub hunting: bool,
    /// [`conduit_active_rotation_value`]'s output — see that function's doc
    /// for why the same number is read as degrees in one branch and radians
    /// in the other.
    pub active_rotation_value: f32,
    /// [`conduit_anim_time`] — the block entity's tick counter plus the
    /// partial tick, feeds [`conduit_bob`].
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
/// which blocks are placed — vanilla's own "hunting" threshold value,
/// which is exactly this count: "hunting" is not a majority threshold, it is
/// *every* candidate cell filled. Checked by
/// `conduit_frame_candidate_count_is_42_and_matches_min_kill_size` as an
/// outside-arithmetic control on the geometry alone, independent of the block
/// predicate.
pub const CONDUIT_FRAME_CANDIDATE_COUNT: u32 = 42;

/// One conduit's activation frame — vanilla's own shape-update effect-block
/// count, plus the two booleans it and the hunting update derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConduitFrame {
    /// The effect-block count. `0` whenever the inner 3×3×3 was not entirely
    /// water — vanilla's own shape update clears the list and returns before
    /// the 5×5×5 pass ever runs in that case, so a zero here does not
    /// distinguish "no water" from "water but zero frame blocks"; nothing
    /// downstream needs to.
    pub effect_block_count: u32,
}

impl ConduitFrame {
    /// Vanilla's own "active" threshold — the shape update's own return
    /// value.
    #[must_use]
    pub fn is_active(self) -> bool {
        self.effect_block_count >= 16
    }

    /// Vanilla's own "hunting" threshold — the hunting update's own
    /// condition (effect-block count >= 42). Exactly
    /// [`CONDUIT_FRAME_CANDIDATE_COUNT`], so this requires *every* candidate
    /// cell filled, not a majority.
    #[must_use]
    pub fn is_hunting(self) -> bool {
        self.effect_block_count >= 42
    }
}

/// Scans a conduit's activation frame around `pos` —
/// vanilla's own shape-update scan, clause by clause:
///
/// 1. The inner 3×3×3 (`ox,oy,oz` each `-1..=1`, all 27 cells, including the
///    conduit's own position) must be **entirely water** —
///    vanilla's own water-at check, which tests the fluid state's water tag:
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
///    (vanilla's own valid-frame-block set).
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
/// vanilla's own client-tick update's non-scan half: the tick counter always
/// advances by one, the active-rotation counter only advances while active.
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

/// Vanilla's own active-rotation accessor with its rotation-speed constant
/// (`-0.0375`), folded with the partial tick exactly as vanilla's own
/// render-state extraction does: the partial tick is added only while
/// active, so the inactive spin advances one **whole** step per tick with no
/// sub-tick smoothing.
///
/// # The returned number is not one unit — read both call sites before "fixing" this
///
/// This returns the raw `(counter + partial) * -0.0375` vanilla calls its
/// own active-rotation state, and vanilla's own submit step reads it two
/// **different** ways depending on which branch runs:
///
/// * **Inactive**: rotate about Y by `state * (PI / 180.0)` (`java.lang.Math`'s
///   own PI) — treated as **degrees**, converted once. See
///   [`conduit_inactive_y_rot_radians`].
/// * **Active**: `rotation = state * (180.0F / PI); … rotate about the axis
///   by rotation * (PI / 180.0)` — multiplied by `180/π` and then immediately
///   back by `π/180`, which is the identity; the two conversions cancel and
///   the axis rotation ends up using the raw value **as radians**,
///   unconverted. See [`conduit_active_axis_rotation_radians`].
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

/// The block entity's tick counter plus the partial tick — vanilla's own
/// render-state extraction's anim-time field. Feeds [`conduit_bob`].
#[must_use]
pub fn conduit_anim_time(tick_count: u32, partial_tick: f32) -> f32 {
    tick_count as f32 + partial_tick
}

/// The block entity's tick counter / 66 % 3 — **integer** division, so this steps once
/// every 66 ticks regardless of the fractional partial tick baked into
/// [`conduit_anim_time`] (which this does *not* take; it reads the raw tick
/// counter directly). Selects which of the two wind planes' extra rotation
/// applies in [`BlockEntityModelSet::resolve_conduit`], and which sprite
/// (`wind` vs `wind_vertical`) both planes draw with.
#[must_use]
pub fn conduit_animation_phase(tick_count: u32) -> u8 {
    ((tick_count / 66) % 3) as u8
}

/// The bob-height fold shared by the cage and the eye — vanilla's own
/// submit-step local `hh`, used as `0.3 + hh * 0.2`.
///
/// **Not** vanilla's animation-tick same-named local — that one
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
/// Named for the *mesh* rather than for the lectern, because the lectern and
/// the enchanting table renderers bake the same book model layer. Both
/// consume it — [`BlockEntityModels::
/// resolve_lectern`] and [`BlockEntityModels::resolve_enchanting_table`] — and
/// the whole difference is the animation state on top: the lectern's is frozen
/// (see [`LECTERN_BOOK_OPENNESS`]) and the table's is live.
pub const BOOK: &str = "book";

/// The jar sheet a book draws with — the enchanting table renderer's own
/// book texture, resolved under the block-entity sheet mapper to
/// `"enchantment/enchanting_table_book"`.
///
/// **The lectern renderer has no texture of its own** — it passes the
/// enchanting table's book texture straight through, which is why this stem
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
/// The lectern's book animation state is built with fixed arguments
/// `(0.0, 0.1, 0.9, 1.2)`, and the animation-state constructor computes
/// `openness = (sin(progress * 0.02) * 0.1 + 1.25) * openness`. With
/// `progress == 0` the `sin` term is exactly zero, so the whole expression
/// collapses to `1.25 * 1.2 == 1.5` for every lectern in the world, every frame.
///
/// That dead arithmetic is the trap: it *looks* like an animation, and porting a
/// live `progress` here would make every lectern book breathe, which vanilla's
/// does not. The page-flip animation belongs to the enchanting table renderer,
/// which feeds that same animation-state constructor a real,
/// client-simulated `progress`.
pub const LECTERN_BOOK_OPENNESS: f32 = 1.5;

/// A lectern book's page-flip pair — the animation state's second and third
/// arguments, also constant. Kept as named constants rather than inlined
/// because [`book_part_poses`] is the shared entry point for the enchanting
/// table too, where both of these *do* vary.
pub const LECTERN_BOOK_PAGE_FLIP: (f32, f32) = (0.1, 0.9);

/// The book model's per-part animation update: six per-part poses, as
/// `(part name, y_rot, x)`.
///
/// ```text
/// left_lid.rot_y    = PI + openness
/// right_lid.rot_y   = -openness
/// left_pages.rot_y  = openness
/// right_pages.rot_y = -openness
/// flip_page1.rot_y  = openness - openness * 2 * page_flip1
/// flip_page2.rot_y  = openness - openness * 2 * page_flip2
/// left_pages.x = right_pages.x = flip_page1.x = flip_page2.x = sin(openness)
/// ```
///
/// `x` is an **absolute** pivot in texels, not a delta: the animation update
/// assigns the left-pages pivot to `sin(openness)`, overwriting the rest
/// pose's `0`. The two lids keep their rest `z` of ∓1 and are not moved in
/// `x` at all, so they carry `None`.
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

/// Vanilla's clockwise-rotated yaw for its four horizontal facing
/// names, or `None` for anything else.
///
/// The lectern renderer's extracted render state stores the FACING
/// property's clockwise-rotated yaw, **not** its own bare yaw, and then the
/// submit step rotates by the *negation* of it. Both steps are easy to unwind
/// wrongly and each is a quarter turn: a book fed [`horizontal_facing_yaw`]
/// directly lies across the lectern's shelf at 90° to the reader.
///
/// A clockwise turn is `+90°` in yaw terms (north `180` → east `270`,
/// east `270` → south `0`), which is why this is one addition and not a second
/// four-arm match to keep in sync.
#[must_use]
pub fn horizontal_facing_clockwise_yaw(name: &str) -> Option<f32> {
    horizontal_facing_yaw(name).map(|yaw| (yaw + 90.0) % 360.0)
}

/// The world placement transform for a lectern's book:
///
/// ```text
/// translate(0.5, 1.0625, 0.5) · rotateY(-yaw) · rotateZ(67.5°) · translate(0, -0.125, 0)
/// ```
///
/// `yaw` is [`horizontal_facing_clockwise_yaw`]'s value, in degrees.
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

/// The enchanting table renderer's Z-axis tilt of 80 degrees — the tilt
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

/// The world placement transform for the floating book over an enchanting table:
///
/// ```text
/// T(pos) · T(0.5, 0.75, 0.5) · T(0, hover(time), 0) · Ry(-y_rot) · Rz(80°)
/// ```
///
/// # `y_rot` is **radians**, and it is not a block facing
///
/// Vanilla's Y-axis rotation for this book takes radians where the lectern's
/// takes degrees, and the value is the block entity's own client-simulated
/// rotation angle that chases the nearest player, not a facing direction. An
/// enchanting table has no `facing` property at all, so there is nothing on
/// its block state this could have come from; feeding it a facing yaw would
/// pin every book to a compass direction and look plausible until a player
/// walks around one.
#[must_use]
pub fn enchanting_table_book_placement_matrix(pos: [i32; 3], y_rot: f32, time: f32) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    Mat4::from_translation(origin + Vec3::new(0.5, 0.75, 0.5))
        * Mat4::from_translation(Vec3::new(0.0, enchanting_table_book_hover(time), 0.0))
        * Mat4::from_rotation_y(-y_rot)
        * Mat4::from_rotation_z(ENCHANTING_TABLE_BOOK_TILT_DEG.to_radians())
}

/// The book animation state's first output — the live `openness` the
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

/// The enchanting table renderer's two page-flip phases, from the block entity's
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

/// How many cooking slots a campfire has — vanilla's own fixed-size list of
/// 4 empty stacks.
pub const CAMPFIRE_SLOTS: usize = 4;

/// The uniform scale each cooking item is drawn at.
pub const CAMPFIRE_ITEM_SCALE: f32 = 0.375;

/// The campfire item's vertical lift: `0.44921875` blocks, i.e. `115/256`.
///
/// Not `0.4375` (`7/16`, the campfire block model's own top face) — the extra
/// `1/256` is what keeps a flat food sprite from z-fighting the log it lies on.
pub const CAMPFIRE_ITEM_LIFT: f32 = 0.449_218_75;

/// The world placement transform for the item cooking in a campfire's `slot`,
/// ported from vanilla's own pose-stack construction term for term:
///
/// ```text
/// T(pos) · T(0.5, 0.44921875, 0.5) · Ry(-slotYRot) · Rx(90°)
///        · T(-0.3125, -0.3125, 0) · S(0.375)
/// ```
///
/// Compose it with the item's own `display.fixed`
/// ([`display_matrix`](crate::display_matrix)) on the **right** — vanilla applies
/// that item transform inside its own layer-render-state submit step,
/// after everything above is on the pose stack. [`crate::entity::campfire_item_mesh`]
/// is that composition; prefer it to hand-multiplying here.
///
/// # A campfire is the only block entity here whose renderer draws no mesh of
/// its own
///
/// The campfire's own renderer has no model, no layer and no sheet: the fire, the logs and
/// the smoke are all part of the **block** model, and the whole renderer is this
/// pose repeated over four item stacks. So there is no `campfire_model()` builder
/// and no texture stem to preload — reading "campfire needs a fire texture" off
/// the block's appearance is the wrong inference, and it is the one this port
/// nearly made.
///
/// # `slot` is an offset from the block's facing, not an absolute corner
///
/// Vanilla derives the slot's world direction from `(slot + facing's 2D
/// index) % 4`, which means slot 0 always sits in the corner the campfire
/// *faces away* toward, and
/// the four march clockwise from there. Ignoring the facing term puts every
/// campfire's first item in the same world corner, which looks right until two
/// campfires face different ways.
///
/// `facing_yaw_deg` is [`horizontal_facing_yaw`]'s convention (south `0`), and
/// the 2D facing index is exactly that divided by `90` — the yaw is
/// `(data2d & 3) * 90` in vanilla's own direction type, so the two are one
/// expression and there is no second table to keep in sync.
#[must_use]
pub fn campfire_item_matrix(pos: [i32; 3], facing_yaw_deg: f32, slot: CampfireSlot) -> Mat4 {
    // `(slot + facing's 2D index) % 4`, then back through the yaw formula.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the four horizontal facing yaws are exact non-negative multiples of 90"
    )]
    let facing_2d = (facing_yaw_deg / 90.0) as usize;
    let slot_yaw = ((slot.index() + facing_2d) % 4) as f32 * 90.0;
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    Mat4::from_translation(origin + Vec3::new(0.5, CAMPFIRE_ITEM_LIFT, 0.5))
        * Mat4::from_rotation_y(-slot_yaw.to_radians())
        * Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2)
        * Mat4::from_translation(Vec3::new(-0.3125, -0.3125, 0.0))
        * Mat4::from_scale(Vec3::splat(CAMPFIRE_ITEM_SCALE))
}

/// The brushable block renderer's base offset — `[0.5, 0.0, 0.5]`
/// before the hit-direction override below replaces one axis.
pub(crate) const BRUSHABLE_ITEM_BASE_OFFSET: Vec3 = Vec3::new(0.5, 0.0, 0.5);

/// Vanilla's own outward lift along the hit direction:
/// `completion_state / 10.0F * 0.75F`, where `completion_state` is the block
/// state's own `dusted` property (`0..=3`, vanilla's own completion-state
/// range) fed straight in — **not** rescaled to `0..=10` first, matching
/// the real jar's own (slightly odd) division by a constant larger than the
/// value's own range.
#[must_use]
pub(crate) fn brushable_item_offset(
    hit_direction: lodestone_assets::Direction,
    dust_progress: u8,
) -> Vec3 {
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
/// sand/gravel block, ported from vanilla's own pose stack term for term:
///
/// ```text
/// T(pos) · T(0, 0.5, 0) · T(translations(hitDirection, dustProgress))
///        · Ry(75°) · Ry((east_west ? 90 : 0) + 11°) · S(0.5)
/// ```
///
/// Compose with the item's own `display.fixed` on the right —
/// [`crate::entity::brushable_item_mesh`] does this, the same composition
/// [`campfire_item_matrix`] uses for the same FIXED display-context
/// reason (vanilla's own render-state extraction resolves the block entity's
/// item in FIXED, not GROUND).
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

/// How many item slots a shelf has — vanilla's own max-items constant.
pub const SHELF_SLOTS: usize = 3;

/// The uniform scale each shelved item is drawn at.
pub const SHELF_ITEM_SCALE: f32 = 0.25;

/// Vanilla's own downward offset applied when `align_items_to_bottom` is
/// set, in the pre-scale (0.25×) local frame.
pub const SHELF_ALIGN_BOTTOM_OFFSET: f32 = -0.25;

/// Vanilla's own per-slot offset, before the item's own
/// bounding-box correction: `((slot - 1) * 0.3125, align_to_bottom ? -0.25 :
/// 0.0, -0.25)`. Slot `0` sits left of centre, slot `2` right of it, each
/// `0.3125` blocks apart in the *pre-scale* local frame (so `0.3125 * 0.25 =
/// 0.078125` world blocks).
#[must_use]
pub fn shelf_item_offset(slot: ShelfSlot, align_to_bottom: bool) -> Vec3 {
    let item_slot_position = (slot.index() as f32 - 1.0) * 0.3125;
    Vec3::new(
        item_slot_position,
        if align_to_bottom {
            SHELF_ALIGN_BOTTOM_OFFSET
        } else {
            0.0
        },
        -0.25,
    )
}

/// The world placement matrix for the item in a shelf's `slot`, up to (but
/// not including) the item's own bounding-box correction — see
/// [`crate::entity::shelf_item_mesh`] for why that last piece has to be
/// supplied by the caller: it needs the item's baked geometry, which this
/// function never sees.
///
/// Ports vanilla's own per-item pose stack construction up to (not
/// including) the final translate-by-offset-Y step:
///
/// ```text
/// T(pos) · T(0.5, 0.5, 0.5) · Ry(yaw) · T(offset) · S(0.25)
/// ```
///
/// `facing_yaw_deg` is [`horizontal_facing_yaw`]'s convention (south `0`) —
/// the shelf's own FACING property is horizontal-only in the real jar
/// (`north`/`south`/`west`/`east`), so vanilla's own submit step's
/// horizontal-vs-vertical branch never actually reaches its `180.0F` arm for
/// a placed shelf; this function ports only the reachable horizontal case
/// for that reason, taking the yaw directly rather than a facing direction.
#[must_use]
pub fn shelf_slot_matrix(
    pos: [i32; 3],
    facing_yaw_deg: f32,
    slot: ShelfSlot,
    align_to_bottom: bool,
) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    let offset = shelf_item_offset(slot, align_to_bottom);
    Mat4::from_translation(origin + Vec3::new(0.5, 0.5, 0.5))
        * Mat4::from_rotation_y(-facing_yaw_deg.to_radians())
        * Mat4::from_translation(offset)
        * Mat4::from_scale(Vec3::splat(SHELF_ITEM_SCALE))
}

/// Which of vanilla's four statue-pose values a placed statue is
/// showing — `copper_golem_pose` on the block state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CopperGolemPose {
    /// The statue is standing upright.
    Standing,
    /// The statue is sitting.
    Sitting,
    /// The statue is running.
    Running,
    /// The statue is posed as a star.
    Star,
}

impl CopperGolemPose {
    /// The [`BlockEntityModelSet`] model name for this pose — one of the four
    /// `copper_golem_statue_*` entries in [`BLOCK_ENTITY_MODELS`].
    #[must_use]
    pub const fn model_name(self) -> &'static str {
        match self {
            CopperGolemPose::Standing => "copper_golem_statue_standing",
            CopperGolemPose::Sitting => "copper_golem_statue_sitting",
            CopperGolemPose::Running => "copper_golem_statue_running",
            CopperGolemPose::Star => "copper_golem_statue_star",
        }
    }
}

/// Every copper golem statue pose, for enumerating stems and exhaustiveness
/// in tests — the [`SKULL_TYPES`] shape for this family.
pub const COPPER_GOLEM_POSES: &[CopperGolemPose] = &[
    CopperGolemPose::Standing,
    CopperGolemPose::Sitting,
    CopperGolemPose::Running,
    CopperGolemPose::Star,
];

/// Vanilla's own weathering-state enum, restricted to the four values a
/// statue's own block name can encode — vanilla's own oxidation-level lookup
/// key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CopperGolemOxidation {
    /// The statue has no oxidation.
    Unaffected,
    /// The statue is exposed to oxidation.
    Exposed,
    /// The statue is weathered.
    Weathered,
    /// The statue is fully oxidized.
    Oxidized,
}

/// Vanilla's own oxidation-level-to-texture lookup — the
/// sheet stem for each oxidation level. Waxing does not change the texture
/// (it only halts further weathering), so there is no fifth stem for a waxed
/// variant — both callers fold `waxed_` away before this ever runs
/// ([`copper_golem_statue_oxidation_from_item_path`] for an item stack, and the
/// shell's own block-state-keyed resolver for a placed statue).
#[must_use]
pub const fn copper_golem_statue_texture_stem(oxidation: CopperGolemOxidation) -> &'static str {
    match oxidation {
        CopperGolemOxidation::Unaffected => "entity/copper_golem/copper_golem",
        CopperGolemOxidation::Exposed => "entity/copper_golem/copper_golem_exposed",
        CopperGolemOxidation::Weathered => "entity/copper_golem/copper_golem_weathered",
        CopperGolemOxidation::Oxidized => "entity/copper_golem/copper_golem_oxidized",
    }
}

/// Every copper golem statue sheet stem the renderer can ask for — what the
/// shell preloads, mirroring [`skull_texture_stems`].
#[must_use]
pub fn copper_golem_statue_texture_stems() -> Vec<&'static str> {
    [
        CopperGolemOxidation::Unaffected,
        CopperGolemOxidation::Exposed,
        CopperGolemOxidation::Weathered,
        CopperGolemOxidation::Oxidized,
    ]
    .iter()
    .map(|o| copper_golem_statue_texture_stem(*o))
    .collect()
}

/// The world placement matrix for a copper golem statue —
/// vanilla's own model-transformation construction, composed with
/// the model's own `root.rot_z = PI` (vanilla's own statue animation update):
///
/// ```text
/// T(pos) · T(0.5, 0, 0.5) · Ry(-opposite_yaw) · Rz(180°)
/// ```
///
/// **`opposite_yaw`, not `facing_yaw_deg` itself**: vanilla's own
/// per-direction transformation map is built from the block's facing
/// direction's *opposite*, not the facing itself, unlike every other
/// block-entity placement in this crate (chest/skull/sign all rotate by the
/// facing directly). `facing_yaw_deg` here is still [`horizontal_facing_yaw`]'s
/// raw convention; the opposite is `+ 180°`, folded in below rather than
/// pushed onto every caller.
///
/// `Rz(180°) == scale(-1, -1, 1)` exactly (`cos(180°) = -1`, `sin(180°) = 0`,
/// so `RotZ(180°)`'s matrix *is* `diag(-1, -1, 1)`) — the same Y-down-model
/// flip [`skull_ground_placement_matrix`] applies via an explicit scale;
/// this function uses the rotation form because that is what the real jar's
/// own animation update does, and the two are algebraically identical.
#[must_use]
pub fn copper_golem_statue_placement_matrix(pos: [i32; 3], facing_yaw_deg: f32) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    let opposite_yaw_deg = facing_yaw_deg + 180.0;
    Mat4::from_translation(origin + Vec3::new(0.5, 0.0, 0.5))
        * Mat4::from_rotation_y(-opposite_yaw_deg.to_radians())
        * Mat4::from_rotation_z(std::f32::consts::PI)
}

/// One copper golem statue to draw this frame — vanilla's own statue
/// renderer. Resolved through [`BlockEntityModelSet`]
/// like chest/skull/bell (a real cuboid rig, unlike the campfire/vault/
/// brushable/shelf item-model family), since `copper_golem_statue.json` is a
/// total-absence hole exactly like chest's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CopperGolemStatueSpawn {
    /// Block position of the statue.
    pub pos: [i32; 3],
    /// The block's own `facing`, in [`horizontal_facing_yaw`]'s convention.
    pub facing_yaw_deg: f32,
    /// The block's `copper_golem_pose` property.
    pub pose: CopperGolemPose,
    /// The block's own oxidation level, from its registry name.
    pub oxidation: CopperGolemOxidation,
    /// Packed sky/block light at the statue.
    pub light: u8,
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
    stems.extend(copper_golem_statue_texture_stems());
    stems
}
