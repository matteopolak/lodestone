//! Camera-facing **sprite billboards** for entities whose vanilla renderer
//! builds a quad vertex by vertex instead of posing a cuboid rig, plus the
//! fishing line that hangs off one of them.
//!
//! # What it is
//!
//! Three of the 26.2 entity renderers draw a single textured quad turned to
//! face the eye: `ExperienceOrbRenderer` (which lives in
//! [`crate::entity`](crate::entity) because it also owns an eleven-cell sprite
//! sheet and a pulsing tint), `DragonFireballRenderer` and
//! `FishingHookRenderer`. The last two are this module: one table row each, one
//! shared mesh builder, one shared placement matrix — and, for the hook, the
//! sixteen-segment catenary back to the caster's hand.
//!
//! # How it works
//!
//! [`ENTITY_SPRITES`] is the whole vocabulary. A caller asks
//! [`entity_sprite_index_for`] whether an entity type draws this way; if it does it
//! bakes [`entity_sprite_mesh`] once at bring-up (the geometry is constant —
//! only the placement changes per frame) and places each instance with
//! [`entity_sprite_matrix`].
//!
//! These are **not** [`crate::entity::model_for_type`] corpus entries and never
//! will be, for exactly the reason `experience_orb` is not: the corpus holds
//! cuboid part hierarchies and these are sprites. `entity_texture_candidates`
//! is likewise empty for both — each binds its own standalone sheet
//! ([`EntitySprite::texture`]), not a slice of the stitched atlas.
//!
//! # How to change it
//!
//! Adding a sprite is a row in [`ENTITY_SPRITES`] plus a texture bind group on
//! the shell side. The two numbers that matter per row are the **scale** and
//! the **quad rect**, and they are not interchangeable: vanilla's fireball quad
//! is `y ∈ [-0.25, 0.75]` (three-quarters above its own origin) while the
//! hook's is `y ∈ [-0.5, 0.5]` (centred). Centring the fireball sinks half the
//! sprite into whatever it is flying over; lifting the hook floats it above the
//! water it is meant to sit in. Read them off the renderer's own `vertex`
//! calls rather than from the entity's `EntityDimensions`, which is a hitbox
//! and is a different number.
//!
//! # Dependencies
//!
//! [`crate::entity::camera_orientation`] for the billboard rotation — the same
//! matrix every other billboard in this crate shares, derived from the view
//! matrix rather than written out as an Euler product (see its own doc for the
//! three stacked conventions that make every hand-written form wrong).
//! `lodestone_physics::mth` for the hand anchor's trigonometry.

use glam::{Mat4, Vec3};
use lodestone_physics::mth;

use crate::models::ModelVertex;

/// Where the dragon fireball's sheet lives in the vanilla jar —
/// `DragonFireballRenderer.TEXTURE_LOCATION`. A standalone sheet like
/// [`crate::entity::EXPERIENCE_ORB_TEXTURE`], not a slice of any atlas.
pub const DRAGON_FIREBALL_TEXTURE: &str =
    "assets/minecraft/textures/entity/enderdragon/dragon_fireball.png";

/// Where the fishing bobber's sheet lives in the vanilla jar —
/// `FishingHookRenderer.TEXTURE_LOCATION`. Standalone, like
/// [`DRAGON_FIREBALL_TEXTURE`].
pub const FISHING_HOOK_TEXTURE: &str =
    "assets/minecraft/textures/entity/fishing/fishing_hook.png";

/// The entity-type path a fishing bobber reports (`minecraft:fishing_bobber`,
/// `EntityTypes.FISHING_BOBBER`).
///
/// Named rather than spelled inline because three separate things key on it:
/// the sprite row below, the line pass, and the owner-id decode on the wire.
pub const FISHING_BOBBER_TYPE_PATH: &str = "fishing_bobber";

/// The entity-type path an ominous item spawner reports
/// (`minecraft:ominous_item_spawner`). Not a sprite — it draws its contained
/// **item**, through the item-cluster path — but it lives here beside its two
/// siblings because all three were stranded together and a reader looking for
/// one will look for the others.
pub const OMINOUS_ITEM_SPAWNER_TYPE_PATH: &str = "ominous_item_spawner";

/// One camera-facing sprite entity: which sheet it binds, how big it draws, and
/// where its quad sits relative to its own feet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntitySprite {
    /// The entity type's canonical path, without the `minecraft:` namespace —
    /// the same spelling `EntityDraw::type_path` carries.
    pub type_path: &'static str,
    /// The standalone sheet this sprite samples, as a jar path.
    pub texture: &'static str,
    /// `poseStack.scale(s, s, s)` from the renderer's own `submit`.
    pub scale: f32,
    /// The quad's lower-left corner in local space, before [`Self::scale`] —
    /// `(x, y)` from the renderer's `vertex` calls, with the constant offset
    /// each one subtracts already folded in.
    pub quad_min: [f32; 2],
    /// The quad's upper-right corner, the same way.
    pub quad_max: [f32; 2],
    /// Whether the renderer overrides `getBlockLightLevel` to a flat `15`.
    ///
    /// Vanilla's override returns the **block** level only; the sky nibble is
    /// still the probe's, which is why this is a flag rather than a whole
    /// packed byte — see [`crate::entity::experience_orb_light`] for the same
    /// asymmetry one boost over.
    pub full_bright: bool,
}

/// Every entity this module draws, in the order the shell bakes them (so a
/// batch's index into a baked part list is its index here).
///
/// Two rows, and the pair is not arbitrary: these are exactly the entity types
/// `crate::entity::thrown_item_for`'s doc calls out as having dedicated
/// renderers that are *not* item billboards. Everything else in that list
/// either has a corpus rig or is drawn as an item.
pub const ENTITY_SPRITES: &[EntitySprite] = &[
    // `DragonFireballRenderer`: `scale(2.0F)`, then `mulPose(camera.orientation)`,
    // and a quad whose four `vertex(buffer, pose, light, x, y, u, v, color)`
    // calls place `(x - 0.5F, y - 0.25F, 0.0F)` for `(x, y)` over
    // `(0,0) (1,0) (1,1) (0,1)`. `getBlockLightLevel` is a flat `15`.
    EntitySprite {
        type_path: "dragon_fireball",
        texture: DRAGON_FIREBALL_TEXTURE,
        scale: 2.0,
        quad_min: [-0.5, -0.25],
        quad_max: [0.5, 0.75],
        full_bright: true,
    },
    // `FishingHookRenderer`: `scale(0.5F)`, `mulPose(camera.orientation)`, and
    // `(x - 0.5F, y - 0.5F, 0.0F)` over the same four `(x, y)` — a **centred**
    // quad, unlike the fireball's. No `getBlockLightLevel` override, so the
    // bobber is lit by the probe like any other entity.
    EntitySprite {
        type_path: FISHING_BOBBER_TYPE_PATH,
        texture: FISHING_HOOK_TEXTURE,
        scale: 0.5,
        quad_min: [-0.5, -0.5],
        quad_max: [0.5, 0.5],
        full_bright: false,
    },
];

/// Which row of [`ENTITY_SPRITES`] `type_path` draws through, or `None` for
/// every entity that draws some other way — which is all but two of the 158
/// registry types.
///
/// # The index, not the reference, is the identity
///
/// A caller needs the row *and* its position, because the position is what
/// selects both the baked geometry and the texture bind group on the shell
/// side. Recovering the position from a returned `&'static EntitySprite` by
/// pointer comparison is the obvious move and it is **wrong here**:
/// [`ENTITY_SPRITES`] is a `const`, so it is inlined at every use site and may
/// occupy as many addresses as it has uses. Measured — a `std::ptr::eq` search
/// against the reference this function returned matched nothing under
/// Cranelift, and every sprite silently drew zero pixels while the table, the
/// mesh and the matrix were all correct. `CLAUDE.md` records the same shape for
/// `ai::roster::FALLBACK`. So this returns the index and
/// [`entity_sprite_at`] resolves it, with no address ever compared.
#[must_use]
pub fn entity_sprite_index_for(type_path: &str) -> Option<usize> {
    ENTITY_SPRITES.iter().position(|s| s.type_path == type_path)
}

/// The sprite row at `index`, for a caller that already has one from
/// [`entity_sprite_index_for`].
#[must_use]
pub fn entity_sprite_at(index: usize) -> Option<&'static EntitySprite> {
    ENTITY_SPRITES.get(index)
}

/// One sprite's quad in *local* space, ready to be posed by
/// [`entity_sprite_matrix`].
///
/// Winding and UV pairing follow [`crate::entity::experience_orb_mesh`]
/// exactly, and the UV pairing is the half worth checking: vanilla pairs the
/// quad's **bottom** vertices with `v = 1` and its top with `v = 0`, so the
/// sprite is not flipped. Both sheets here are close to radially symmetric,
/// which means getting that pair the wrong way round is nearly invisible — the
/// reason it is stated rather than left to the reader.
///
/// The `light`/`tint`/`anim` vertex lanes are inert defaults: this pass carries
/// its light **per instance**, because two bobbers in different cells are lit
/// differently and the mesh is shared.
#[must_use]
pub fn entity_sprite_mesh(sprite: &EntitySprite) -> (Vec<ModelVertex>, Vec<u32>) {
    let [x0, y0] = sprite.quad_min;
    let [x1, y1] = sprite.quad_max;
    // Bottom-left, bottom-right, top-right, top-left — with `v = 1` on the
    // bottom pair.
    let corners = [
        ([x0, y0], [0.0, 1.0]),
        ([x1, y0], [1.0, 1.0]),
        ([x1, y1], [1.0, 0.0]),
        ([x0, y1], [0.0, 0.0]),
    ];
    let vertices = corners
        .into_iter()
        .map(|([x, y], uv)| ModelVertex {
            position: [x, y, 0.0],
            uv,
            ao: 1.0,
            light: 0,
            tint: 255,
            anim: 0,
            cutout_bypass: 0,
            tint_rgb_override: [0, 0, 0, 0],
        })
        .collect();
    (vertices, vec![0, 1, 2, 0, 2, 3])
}

/// The world placement for one sprite instance:
///
/// ```text
/// T(feet) · S(scale) · camera_orientation
/// ```
///
/// Source order, matching both renderers' pose stacks (`scale` then
/// `mulPose`). A **uniform** scale commutes with a rotation, so this is the
/// same matrix as the orb's `T · orientation · S` — written in vanilla's order
/// anyway, because the next sprite added here might not be uniform and a
/// reader should not have to re-derive that the two agree.
///
/// `orientation` is [`crate::entity::camera_orientation`] of the view matrix:
/// one matrix per frame shared by every sprite, since a billboard's rotation
/// depends only on the camera.
///
/// Determinant is positive (a translation, a positive uniform scale and a
/// rotation), so this composes to terrain's winding.
#[must_use]
pub fn entity_sprite_matrix(feet: Vec3, orientation: Mat4, scale: f32) -> Mat4 {
    Mat4::from_translation(feet) * Mat4::from_scale(Vec3::splat(scale)) * orientation
}

// ---------------------------------------------------------------------------
// The fishing line
// ---------------------------------------------------------------------------
//
// `FishingHookRenderer`'s second submission: sixteen segments from the hook up
// to the caster's hand, sagging on the way. Vanilla draws them through
// `RenderTypes.lines()` at `appropriateLineWidth`, i.e. a **screen-space**
// width — see the shell's own line pass for why reproducing that with a
// `PrimitiveTopology::LineList` primitive would make the line invisible at any
// real resolution.

/// How many segments the line is divided into — `int steps = 16` in
/// `FishingHookRenderer.submit`, which emits `fraction(i, 16)` for `i` in
/// `0..16`.
///
/// The count is visible in the result: the sag is a quadratic evaluated at
/// these sample points and joined by straight chords, so halving it visibly
/// flattens the curve near the rod.
pub const FISHING_LINE_STEPS: usize = 16;

/// The constant `+0.25` that appears **twice** in vanilla's line maths, once in
/// each half — and it is the same 0.25 both times.
///
/// `extractRenderState` anchors the offset at
/// `entity.getPosition(t).add(0.0, 0.25, 0.0)`, and `stringVertex` then adds
/// `0.25` back to every sample's local `y`. The pose stack is at the entity's
/// **feet**, so the two together put the line's `a = 0` end exactly at the hook
/// point and its `a = 1` end exactly at the hand — which is the check worth
/// keeping, because dropping either occurrence still produces a plausible line
/// that simply starts a quarter block off.
pub const FISHING_LINE_HOOK_LIFT: f32 = 0.25;

/// `stringVertex`'s `setColor(-16777216)` — opaque black, as straight RGBA
/// floats.
pub const FISHING_LINE_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// `state.lineOriginOffset` — the vector from the **hook point** to the
/// caster's hand, which is what the whole curve is denominated in.
///
/// `playerPos.subtract(hookPos)` where `hookPos` is the bobber's feet lifted by
/// [`FISHING_LINE_HOOK_LIFT`]. Exposed separately from
/// [`fishing_line_points`] because it is the quantity a gate can predict from
/// the two endpoints alone, with no curve arithmetic in the way.
#[must_use]
pub fn fishing_line_origin_offset(hand: Vec3, bobber_feet: Vec3) -> Vec3 {
    hand - (bobber_feet + Vec3::new(0.0, FISHING_LINE_HOOK_LIFT, 0.0))
}

/// The [`FISHING_LINE_STEPS`]` + 1` world-space points the line is drawn
/// through, from the hook (index `0`) to the hand (index
/// [`FISHING_LINE_STEPS`]).
///
/// `stringVertex`, verbatim, with the pose stack's translation to the bobber's
/// feet folded in:
///
/// ```text
/// x = dx · a
/// y = dy · (a² + a) · 0.5 + 0.25
/// z = dz · a
/// ```
///
/// where `a = i / 16` and `(dx, dy, dz)` is [`fishing_line_origin_offset`].
///
/// # The sag is in `y` alone, and it is quadratic rather than catenary
///
/// `x` and `z` are plain linear interpolations; only the vertical term carries
/// the `(a² + a) · 0.5` shaping, which is `0` at `a = 0` and `1` at `a = 1` and
/// dips **below** the straight chord in between (at `a = 0.5` it is `0.375`,
/// not `0.5`). That is the whole of the visible droop. Substituting a true
/// catenary, or applying the same shaping to `x`/`z`, both look plausible and
/// are both wrong.
#[must_use]
pub fn fishing_line_points(bobber_feet: Vec3, hand: Vec3) -> Vec<Vec3> {
    let d = fishing_line_origin_offset(hand, bobber_feet);
    #[expect(
        clippy::cast_precision_loss,
        reason = "16 steps; every index is exactly representable"
    )]
    let steps = FISHING_LINE_STEPS as f32;
    (0..=FISHING_LINE_STEPS)
        .map(|i| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "0..=16; exactly representable"
            )]
            let a = i as f32 / steps;
            bobber_feet
                + Vec3::new(
                    d.x * a,
                    d.y * (a * a + a) * 0.5 + FISHING_LINE_HOOK_LIFT,
                    d.z * a,
                )
        })
        .collect()
}

/// Which side of the caster the line leaves from — vanilla's
/// `getHoldingArm(owner)`, reduced to the `invert` sign both hand-position
/// branches multiply by.
///
/// `owner.getMainHandItem().getItem() instanceof FishingRodItem ? owner.getMainArm()
/// : owner.getMainArm().getOpposite()`, then `RIGHT ? 1 : -1`. So a player who
/// cast the rod and then switched hotbar slots has the line jump to their other
/// hand, which is vanilla's own behaviour and not a bug to smooth over.
///
/// `main_arm_left` is `Player.getMainArm() == LEFT`; `rod_in_main_hand` is the
/// `instanceof` test. Both are the caller's to answer — this is the truth table,
/// not the lookup.
#[must_use]
pub fn fishing_holding_arm_sign(main_arm_left: bool, rod_in_main_hand: bool) -> f32 {
    let arm_is_left = if rod_in_main_hand {
        main_arm_left
    } else {
        !main_arm_left
    };
    if arm_is_left { -1.0 } else { 1.0 }
}

/// Vanilla's `swing2` — the shaped attack-swing amount the first-person hand
/// anchor is rotated by.
///
/// `Mth.sin(Mth.sqrt(swing) * (float) Math.PI)` for `swing =
/// owner.getAttackAnim(partialTicks)`, a `0..1` ramp. The `sqrt` inside the
/// `sin` is what makes the swing snap out and ease back rather than being
/// symmetric, and it is easy to drop when transcribing.
#[must_use]
pub fn fishing_swing_shaping(attack_anim: f32) -> f32 {
    mth::sin(f64::from(attack_anim.max(0.0).sqrt() * std::f32::consts::PI))
}

/// Where the line meets a caster this client draws as an ordinary entity —
/// vanilla's `getPlayerHandPos` **else** branch, taken for every remote player
/// and for our own body whenever the camera is detached.
///
/// ```text
/// rightOffset   = invert · 0.35 · scale
/// forwardOffset = 0.8 · scale
/// yOffset       = crouching ? -0.1875 : 0
/// hand = eye + (−cos·rightOffset − sin·forwardOffset,
///               yOffset − 0.45·scale,
///               −sin·rightOffset + cos·forwardOffset)
/// ```
///
/// `sin`/`cos` are of the owner's **body** yaw in radians, through
/// [`mth`] rather than `f32::sin`/`f32::cos`.
///
/// # `eye`, not feet
///
/// Vanilla adds this offset to `owner.getEyePosition(partialTicks)` and then
/// subtracts `0.45 · scale` from it, so the anchor ends up roughly at shoulder
/// height. Passing feet and expecting the `−0.45` to do the lifting puts the
/// line at the caster's ankles.
///
/// # The two horizontal terms are a rotation, and transposing them is silent
///
/// `(−cos, −sin)` scales the *right* offset and `(−sin, +cos)` the *forward*
/// one. Swapping which pair multiplies which offset still yields a hand
/// somewhere beside the player and only looks wrong once they turn.
#[must_use]
pub fn fishing_hand_anchor_third_person(
    eye: Vec3,
    body_yaw_deg: f32,
    entity_scale: f32,
    holding_arm_sign: f32,
    crouching: bool,
) -> Vec3 {
    let yaw = f64::from(body_yaw_deg.to_radians());
    let sin = mth::sin(yaw);
    let cos = mth::cos(yaw);
    let right_offset = holding_arm_sign * 0.35 * entity_scale;
    let forward_offset = 0.8 * entity_scale;
    let y_offset = if crouching { -0.1875 } else { 0.0 };
    eye + Vec3::new(
        -cos * right_offset - sin * forward_offset,
        y_offset - 0.45 * entity_scale,
        -sin * right_offset + cos * forward_offset,
    )
}

/// `getPointOnPlane`'s `x`, from `getPlayerHandPos`'s
/// `invert * 0.525F` — how far across the near plane the rod tip sits.
const FIRST_PERSON_PLANE_X: f32 = 0.525;
/// `getPointOnPlane`'s `y`, from the same call's literal `-0.1F`.
const FIRST_PERSON_PLANE_Y: f32 = -0.1;
/// `FishingHookRenderer.VIEW_BOBBING_SCALE` — the numerator of the
/// `960.0 / fov` factor the near-plane point is scaled by.
const VIEW_BOBBING_SCALE: f32 = 960.0;

/// Where the line meets the caster when the caster is **us** and the camera is
/// in first person — vanilla's `getPlayerHandPos` **if** branch.
///
/// There is no entity to read a position off here (vanilla's own first-person
/// player body is not drawn either), so the anchor is projected onto the
/// camera's near plane and pushed out along the view:
///
/// ```text
/// planeHeight = tan(fov/2) · zNear
/// planeWidth  = planeHeight · aspect
/// point       = forward·zNear + up·planeHeight·(−0.1) − left·planeWidth·(invert·0.525)
/// hand        = eye + point · (960/fov) · yRot(swing·0.5) · xRot(−swing·0.7)
/// ```
///
/// with `left = −right`, matching `Camera.setRotation`'s own `LEFT = (−1, 0, 0)`
/// basis vector.
///
/// # Two faithful details worth keeping
///
/// * The **whole point is proportional to `zNear`**, and it is then multiplied
///   by `960 / fov`. Neither factor cancels: dropping `zNear` moves the hand to
///   ~14 blocks in front of the eye, dropping the scale leaves it 5 cm away and
///   permanently clipped.
/// * The two rotations are **not** the same angle: `yRot(+swing · 0.5)` and
///   `xRot(−swing · 0.7)`. They are the arm swing carrying the rod tip, and
///   using one angle for both is a plausible-looking wrong version.
///
/// # A deliberate divergence, stated
///
/// Vanilla reads `options.fov()` — the *setting* — for both the plane height
/// and the `960 / fov` factor, while taking `zNear` and the aspect ratio from
/// the live projection. This takes all four off the same [`crate::camera::Camera`],
/// so a dynamic FOV modifier (sprinting, a spyglass) moves the anchor slightly
/// where vanilla's would not. The alternative is plumbing the raw option
/// through the renderer for one entity's line, which buys a few centimetres.
#[expect(
    clippy::too_many_arguments,
    reason = "a faithful transcription of a seven-input vanilla expression; \
              bundling them into a struct would hide which are camera facts"
)]
#[must_use]
pub fn fishing_hand_anchor_first_person(
    eye: Vec3,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
    near: f32,
    fov_y_degrees: f32,
    aspect: f32,
    swing: f32,
    holding_arm_sign: f32,
) -> Vec3 {
    let plane_height = (fov_y_degrees.to_radians() / 2.0).tan() * near;
    let plane_width = plane_height * aspect;
    // Vanilla's `left`, which is the negation of this crate's `right`.
    let left = -right;
    let point = forward * near + up * plane_height * FIRST_PERSON_PLANE_Y
        - left * plane_width * (holding_arm_sign * FIRST_PERSON_PLANE_X);
    let scaled = point * (VIEW_BOBBING_SCALE / fov_y_degrees.max(f32::EPSILON));
    eye + rotate_x(rotate_y(scaled, swing * 0.5), -swing * 0.7)
}

/// `Vec3.yRot(radians)`: `(x·cos + z·sin, y, z·cos − x·sin)`.
///
/// Written out rather than reached for through `glam::Mat3::from_rotation_y`
/// because vanilla's sign convention here is its own — `z·cos − x·sin`, not the
/// `z·cos + x·sin` a right-handed rotation about `+Y` would give — and because
/// `cos`/`sin` come from the quantized table.
fn rotate_y(v: Vec3, radians: f32) -> Vec3 {
    let cos = mth::cos(f64::from(radians));
    let sin = mth::sin(f64::from(radians));
    Vec3::new(v.x * cos + v.z * sin, v.y, v.z * cos - v.x * sin)
}

/// `Vec3.xRot(radians)`: `(x, y·cos + z·sin, z·cos − y·sin)`.
fn rotate_x(v: Vec3, radians: f32) -> Vec3 {
    let cos = mth::cos(f64::from(radians));
    let sin = mth::sin(f64::from(radians));
    Vec3::new(v.x, v.y * cos + v.z * sin, v.z * cos - v.y * sin)
}

// ---------------------------------------------------------------------------
// The ominous item spawner
// ---------------------------------------------------------------------------

/// `OminousItemSpawnerRenderer.ROTATION_SPEED` — degrees per tick the held item
/// spins about `+Y`.
pub const OMINOUS_SPAWNER_ROTATION_SPEED: f32 = 40.0;
/// `OminousItemSpawnerRenderer.TICKS_SCALING` — how many ticks the item takes to
/// grow from nothing to full size after the spawner appears.
pub const OMINOUS_SPAWNER_TICKS_SCALING: f32 = 50.0;

/// How large the spawner's item draws at `age_ticks` —
/// `min(ageInTicks, 50) / 50`, clamped to `1.0` afterwards.
///
/// Vanilla writes this as `if (ageInTicks <= 50) scale(min(age, 50) / 50)`,
/// which is the same function with the `> 50` case left as an implicit `1.0`
/// (no `scale` call at all). Folding the two arms into one expression is safe
/// here precisely because `min` already saturates.
///
/// A negative age cannot occur but clamps to `0` rather than mirroring.
#[must_use]
pub fn ominous_spawner_item_scale(age_ticks: f32) -> f32 {
    (age_ticks.max(0.0) / OMINOUS_SPAWNER_TICKS_SCALING).min(1.0)
}

/// The spawner item's spin in degrees — `Mth.wrapDegrees(ageInTicks * 40)`.
///
/// The wrap is preserved rather than dropped as "periodic anyway": it is, for a
/// rotation, but the wrapped value is also what a gate can predict without
/// worrying about `f32` losing precision on a large unwrapped total after a few
/// real-time minutes.
#[must_use]
pub fn ominous_spawner_spin_degrees(age_ticks: f32) -> f32 {
    lodestone_physics::mth::wrap_degrees_f32(age_ticks * OMINOUS_SPAWNER_ROTATION_SPEED)
}

/// The world placement for one copy of an ominous item spawner's item cluster,
/// matching `OminousItemSpawnerRenderer.submit`'s pose stack:
///
/// ```text
/// T(feet) · S(scale) · Ry(spin) · T(offset) · display_matrix(ground)
/// ```
///
/// `offset` is `ItemEntityRenderer.submitMultipleFromCount`'s per-copy scatter
/// (zero for the first copy), and the item's own `display.ground` transform sits
/// on the right for the reason [`crate::entity::dropped_item_matrix`] documents:
/// vanilla applies it *inside* `ItemStackRenderState.submit`, after every pose
/// the caller pushed.
///
/// # It is **not** a dropped item, and the difference is two missing terms
///
/// `ItemEntityRenderer.submit` translates by a bob (`sin(age/10 + bobOffset) ·
/// 0.1 + 0.1`) plus a hover lift, and spins at `ItemEntity.getSpin`'s own rate.
/// `OminousItemSpawnerRenderer` calls neither — it goes straight from the scale
/// to its own 40°/tick spin. Reusing the dropped-item matrix here draws an item
/// that bobs when it should hang still and spins at the wrong speed, which
/// reads as "close enough" in a screenshot and is wrong in motion.
#[must_use]
pub fn ominous_spawner_item_matrix(
    feet: Vec3,
    item_scale: f32,
    spin_deg: f32,
    offset: Vec3,
    ground: &lodestone_assets::DisplayTransform,
) -> Mat4 {
    Mat4::from_translation(feet)
        * Mat4::from_scale(Vec3::splat(item_scale))
        * Mat4::from_rotation_y(spin_deg.to_radians())
        * Mat4::from_translation(offset)
        * crate::item_render::display_matrix(ground)
}

/// Mesh one copy of an ominous item spawner's item into a world-space
/// [`crate::ModelMesh`], for the same pass and the same camera uniform
/// [`crate::entity::dropped_item_mesh`] feeds.
///
/// `light` is `OminousItemSpawnerRenderer.submit`'s literal `15728880`, i.e.
/// `LightTexture.FULL_BRIGHT` — the renderer never samples the world, so the
/// item glows at any time of day and in any cell. Passing a world sample here
/// instead draws a spawner item that goes dark at night, which is the
/// plausible-looking wrong version because every *other* item in this crate does
/// exactly that.
#[must_use]
pub fn ominous_spawner_item_mesh(
    quads: &[lodestone_assets::BakedQuad],
    gui_light: lodestone_assets::GuiLight,
    ground: &lodestone_assets::DisplayTransform,
    feet: Vec3,
    item_scale: f32,
    spin_deg: f32,
    offset: Vec3,
    light: u8,
) -> crate::ModelMesh {
    let pose = ominous_spawner_item_matrix(feet, item_scale, spin_deg, offset, ground);
    crate::entity::mesh_item_quads_with_light(quads, pose, gui_light, light)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is the vocabulary two other layers key on, so its rows are
    /// pinned against the vanilla renderers they were read off.
    ///
    /// Covers the geometry table only — it says nothing about whether either
    /// sprite reaches a pixel. `crates/lodestone-shell/tests/entity_sprite_pixels.rs`
    /// is the half that rasterises.
    #[test]
    fn the_sprite_table_matches_the_two_vanilla_renderers() {
        let fireball = entity_sprite_index_for("dragon_fireball")
            .and_then(entity_sprite_at)
            .expect("dragon_fireball is a sprite");
        assert!((fireball.scale - 2.0).abs() < f32::EPSILON);
        // `y ∈ [-0.25, 0.75]` — three quarters above the origin, not centred.
        assert_eq!(fireball.quad_min, [-0.5, -0.25]);
        assert_eq!(fireball.quad_max, [0.5, 0.75]);
        assert!(fireball.full_bright);

        let hook = entity_sprite_index_for(FISHING_BOBBER_TYPE_PATH)
            .and_then(entity_sprite_at)
            .expect("fishing_bobber is a sprite");
        assert!((hook.scale - 0.5).abs() < f32::EPSILON);
        // Centred, unlike the fireball's — the discriminating difference.
        assert_eq!(hook.quad_min, [-0.5, -0.5]);
        assert_eq!(hook.quad_max, [0.5, 0.5]);
        assert!(!hook.full_bright);

        // And the negative side: an entity with a real rig must not be claimed.
        assert!(entity_sprite_index_for("pig").is_none());
        assert!(entity_sprite_index_for("experience_orb").is_none());
    }

    /// The index a caller keys geometry and textures off must name the row it
    /// came from, at every row.
    ///
    /// This is the assertion the shipped bug failed: a lookup returned the
    /// right row and a `std::ptr::eq` search for its position found nothing,
    /// because [`ENTITY_SPRITES`] is a `const` and is inlined per use site.
    /// Nothing about the table, the mesh or the matrix was wrong, and every
    /// sprite drew zero pixels.
    #[test]
    fn every_sprite_index_resolves_back_to_its_own_row() {
        for (i, row) in ENTITY_SPRITES.iter().enumerate() {
            let index = entity_sprite_index_for(row.type_path)
                .unwrap_or_else(|| panic!("{} must resolve to an index", row.type_path));
            assert_eq!(index, i, "{} resolved to row {index}", row.type_path);
            let back = entity_sprite_at(index).expect("a resolved index must name a row");
            // By value, never by address: comparing `&'static` references here
            // would reproduce the very bug this gate exists for.
            assert_eq!(back.type_path, row.type_path);
            assert_eq!(back.texture, row.texture);
        }
        assert!(entity_sprite_index_for("pig").is_none());
    }

    /// The quad's UV pairing, which a radially symmetric sheet cannot show.
    #[test]
    fn the_sprite_quad_pairs_its_bottom_edge_with_v_one() {
        let hook = entity_sprite_index_for(FISHING_BOBBER_TYPE_PATH)
            .and_then(entity_sprite_at)
            .expect("fishing_bobber is a sprite");
        let (vertices, indices) = entity_sprite_mesh(hook);
        assert_eq!(indices, vec![0, 1, 2, 0, 2, 3]);
        // Vertex 0 is the bottom-left corner and must carry `v = 1`.
        assert_eq!(vertices[0].position, [-0.5, -0.5, 0.0]);
        assert_eq!(vertices[0].uv, [0.0, 1.0]);
        // Vertex 2 is the top-right and must carry `v = 0`.
        assert_eq!(vertices[2].position, [0.5, 0.5, 0.0]);
        assert_eq!(vertices[2].uv, [1.0, 0.0]);
    }

    /// The line's two endpoints are exact, and the middle sags **below** the
    /// straight chord by a predicted amount.
    ///
    /// Predicting the midpoint rather than asserting "it is below the chord" is
    /// the point: the wrong hypothesis here — dropping the `(a² + a) · 0.5`
    /// shaping for a plain lerp — also puts every sample between the endpoints,
    /// so a sign-only assertion passes under it. The two hypotheses are
    /// computed from the endpoints below and the measurement has to land on
    /// one.
    #[test]
    fn the_fishing_line_sags_by_the_quadratic_and_not_by_a_lerp() {
        // Pairwise-distinct coordinates, so a transposed axis cannot survive.
        let bobber = Vec3::new(11.0, 4.0, -7.0);
        let hand = Vec3::new(3.0, 9.0, 2.0);
        let points = fishing_line_points(bobber, hand);
        assert_eq!(points.len(), FISHING_LINE_STEPS + 1);

        // `a = 0` lands on the hook point: the bobber's feet lifted by 0.25.
        let hook = bobber + Vec3::new(0.0, FISHING_LINE_HOOK_LIFT, 0.0);
        assert!((points[0] - hook).length() < 1e-4, "got {:?}", points[0]);
        // `a = 1` lands on the hand, exactly.
        let last = points[FISHING_LINE_STEPS];
        assert!((last - hand).length() < 1e-4, "got {last:?}");

        // The midpoint, `a = 0.5`. Both hypotheses share x and z (the shaping
        // is vertical only), so the discriminator is y alone.
        let d = fishing_line_origin_offset(hand, bobber);
        let mid = points[FISHING_LINE_STEPS / 2];
        let correct = hook.y + d.y * (0.25 + 0.5) * 0.5;
        let wrong_lerp = hook.y + d.y * 0.5;
        assert!(
            (correct - wrong_lerp).abs() > 0.1,
            "the two hypotheses must differ at this fixture or the test is vacuous \
             (correct {correct}, lerp {wrong_lerp})"
        );
        assert!(
            (mid.y - correct).abs() < 1e-4,
            "midpoint y {} is neither the quadratic {correct} nor the lerp {wrong_lerp}",
            mid.y
        );
        // And the horizontal terms really are linear.
        assert!((mid.x - (hook.x + d.x * 0.5)).abs() < 1e-4);
        assert!((mid.z - (hook.z + d.z * 0.5)).abs() < 1e-4);
    }

    /// `getHoldingArm`'s truth table, all four rows — a two-boolean function
    /// where three of the four rows agree, so sampling fewer cannot see it.
    #[test]
    fn the_holding_arm_sign_covers_all_four_rows() {
        // Right-handed, rod in main hand -> the right arm.
        assert!((fishing_holding_arm_sign(false, true) - 1.0).abs() < f32::EPSILON);
        // Right-handed, rod in the off hand -> the opposite, i.e. left.
        assert!((fishing_holding_arm_sign(false, false) + 1.0).abs() < f32::EPSILON);
        // Left-handed, rod in main hand -> left.
        assert!((fishing_holding_arm_sign(true, true) + 1.0).abs() < f32::EPSILON);
        // Left-handed, rod in the off hand -> right.
        assert!((fishing_holding_arm_sign(true, false) - 1.0).abs() < f32::EPSILON);
    }

    /// The third-person anchor's horizontal pair, at a yaw where the two terms
    /// are separable.
    ///
    /// At yaw `0` a player faces `+Z`, so `sin = 0` and `cos = 1`: the forward
    /// offset lands entirely on `z` and the right offset entirely on `x`. That
    /// is the input where a transposition of the two terms is visible, and it
    /// is also exactly the "simple" fixture value — so the second arm below
    /// uses yaw `90`, where the two swap, to prove the pairing rather than the
    /// arithmetic at one convenient angle.
    #[test]
    fn the_third_person_anchor_places_forward_and_right_on_the_right_axes() {
        let eye = Vec3::new(5.0, 70.0, -3.0);
        let at_zero = fishing_hand_anchor_third_person(eye, 0.0, 1.0, 1.0, false);
        // `-cos·0.35 - sin·0.8` = `-0.35`; `-sin·0.35 + cos·0.8` = `+0.8`.
        assert!((at_zero.x - (eye.x - 0.35)).abs() < 1e-3, "{at_zero:?}");
        assert!((at_zero.z - (eye.z + 0.8)).abs() < 1e-3, "{at_zero:?}");
        assert!((at_zero.y - (eye.y - 0.45)).abs() < 1e-3, "{at_zero:?}");

        let at_ninety = fishing_hand_anchor_third_person(eye, 90.0, 1.0, 1.0, false);
        // sin = 1, cos = 0: x picks up the *forward* offset, z the *right* one.
        assert!((at_ninety.x - (eye.x - 0.8)).abs() < 1e-3, "{at_ninety:?}");
        assert!((at_ninety.z - (eye.z - 0.35)).abs() < 1e-3, "{at_ninety:?}");

        // Crouching drops the anchor by exactly 0.1875 and nothing else moves.
        let crouched = fishing_hand_anchor_third_person(eye, 0.0, 1.0, 1.0, true);
        assert!((crouched.y - (at_zero.y - 0.1875)).abs() < 1e-4);
        assert!((crouched.x - at_zero.x).abs() < 1e-6);
        assert!((crouched.z - at_zero.z).abs() < 1e-6);

        // The holding arm flips the *right* term's sign and leaves forward alone.
        let left = fishing_hand_anchor_third_person(eye, 0.0, 1.0, -1.0, false);
        assert!((left.x - (eye.x + 0.35)).abs() < 1e-3, "{left:?}");
        assert!((left.z - (eye.z + 0.8)).abs() < 1e-3, "{left:?}");
    }

    /// The first-person anchor lands in front of and beside the eye, at the
    /// magnitudes the two surviving factors predict.
    ///
    /// The prediction is computed from the vanilla expression rather than from
    /// a remembered round number, and the two dropped-factor hypotheses (no
    /// `zNear`, no `960 / fov`) are computed alongside so the measurement has
    /// to land on the right one of three.
    #[test]
    fn the_first_person_anchor_sits_at_the_rod_tip() {
        // A camera looking down `+Z` with no pitch: right = (-1, 0, 0),
        // up = (0, 1, 0), forward = (0, 0, 1) per `Camera::basis` at yaw 0.
        let eye = Vec3::new(0.0, 70.0, 0.0);
        let right = Vec3::new(-1.0, 0.0, 0.0);
        let up = Vec3::Y;
        let forward = Vec3::Z;
        let (near, fov, aspect) = (0.05_f32, 70.0_f32, 16.0 / 9.0);
        let hand =
            fishing_hand_anchor_first_person(eye, right, up, forward, near, fov, aspect, 0.0, 1.0);

        let plane_height = (fov.to_radians() / 2.0).tan() * near;
        let plane_width = plane_height * aspect;
        let boost = 960.0 / fov;
        let expect_forward = near * boost;
        // `left = -right = (1, 0, 0)`, so `-left * planeWidth * 0.525` moves in
        // `-x`.
        let expect_x = -plane_width * FIRST_PERSON_PLANE_X * boost;
        let expect_y = plane_height * FIRST_PERSON_PLANE_Y * boost;

        assert!(
            (hand.z - (eye.z + expect_forward)).abs() < 1e-4,
            "forward {} != {expect_forward}",
            hand.z - eye.z
        );
        assert!((hand.x - (eye.x + expect_x)).abs() < 1e-4, "{hand:?}");
        assert!((hand.y - (eye.y + expect_y)).abs() < 1e-4, "{hand:?}");

        // The two wrong hypotheses, each computed from the same constants: drop
        // `zNear`, or drop the `960 / fov` boost. Both must be far from the
        // measurement, or this gate is only checking that the function runs.
        let without_near = 1.0 * boost;
        let without_boost = near;
        assert!((expect_forward - without_near).abs() > 1.0);
        assert!((expect_forward - without_boost).abs() > 0.5);

        // The swing rotates the anchor; at swing 0 it must not.
        let swung = fishing_hand_anchor_first_person(
            eye, right, up, forward, near, fov, aspect, 0.7, 1.0,
        );
        assert!(
            (swung - hand).length() > 0.05,
            "a real swing must move the rod tip: {swung:?} vs {hand:?}"
        );
    }

    /// The spawner's grow-in ramp, at the two points that discriminate it from
    /// a plain unclamped ratio and from an instant pop.
    #[test]
    fn the_ominous_spawner_item_grows_over_fifty_ticks_and_then_stops() {
        assert!((ominous_spawner_item_scale(0.0) - 0.0).abs() < f32::EPSILON);
        // 25 ticks is exactly half, and it is not 0 or 1 under either wrong
        // hypothesis, which is why it is the sample.
        assert!((ominous_spawner_item_scale(25.0) - 0.5).abs() < 1e-6);
        assert!((ominous_spawner_item_scale(50.0) - 1.0).abs() < 1e-6);
        // Past the ramp it saturates rather than continuing to grow — the
        // unclamped hypothesis would read 4.0 here.
        assert!((ominous_spawner_item_scale(200.0) - 1.0).abs() < 1e-6);
    }

    /// The spin rate, at an age where 40°/tick and a plausible-looking
    /// 10°/tick (the vault's rate, the nearest sibling in this crate) differ.
    #[test]
    fn the_ominous_spawner_spins_at_forty_degrees_a_tick() {
        // 4 ticks: 160° under the real rate, 40° under the vault's. Wrapped
        // degrees are in `[-180, 180)`, so both are representable and distinct.
        let spin = ominous_spawner_spin_degrees(4.0);
        assert!((spin - 160.0).abs() < 1e-3, "got {spin}");
        assert!((spin - 40.0).abs() > 100.0);
        // And the wrap really wraps: 5 ticks is 200°, i.e. -160°.
        let wrapped = ominous_spawner_spin_degrees(5.0);
        assert!((wrapped + 160.0).abs() < 1e-3, "got {wrapped}");
    }
}
