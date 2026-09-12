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
use glam::{Mat4, Vec3};

use crate::entity::ENTITY_FULLBRIGHT;
