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
use glam::{Mat4, Vec3};
use lodestone_model::{CampfireSlot, ShelfSlot};

use super::chest_skulls::horizontal_facing_yaw;
