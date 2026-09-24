use glam::{Mat4, Vec3};

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
