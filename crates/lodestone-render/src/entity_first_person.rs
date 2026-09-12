use super::*;

/// Which humanoid arm receives a held item or first-person pose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arm {
    /// The right arm — a mob's main hand.
    Right,
    /// The left arm — a mob's off hand.
    Left,
}

impl Arm {
    /// The `entity_models` part name for this arm.
    #[must_use]
    pub const fn part_name(self) -> &'static str {
        match self {
            Arm::Right => "right_arm",
            Arm::Left => "left_arm",
        }
    }

    /// The overlay ("sleeve") part parented to this arm at `PartPose::ZERO`, for
    /// the models that have one (the two player rigs). It shares the arm's matrix
    /// exactly — see [`first_person_arm_pose`].
    #[must_use]
    pub const fn sleeve_part_name(self) -> &'static str {
        match self {
            Arm::Right => "right_sleeve",
            Arm::Left => "left_sleeve",
        }
    }

    /// Whether this is a left-hand context, i.e. whether
    /// [`display_matrix_for_hand`]'s mirror applies.
    #[must_use]
    pub const fn is_left(self) -> bool {
        matches!(self, Arm::Left)
    }

    /// Vanilla's own left-hand sign (`isLeftHand ? -1 : 1`), used for every mirrored
    /// term in both chains below.
    #[must_use]
    pub const fn invert(self) -> f32 {
        match self {
            Arm::Right => 1.0,
            Arm::Left => -1.0,
        }
    }

    /// The `display` slot an item held in this arm is posed by.
    #[must_use]
    pub const fn display_slot(self, first_person: bool) -> DisplaySlot {
        match (self, first_person) {
            (Arm::Right, false) => DisplaySlot::ThirdPersonRightHand,
            (Arm::Left, false) => DisplaySlot::ThirdPersonLeftHand,
            (Arm::Right, true) => DisplaySlot::FirstPersonRightHand,
            (Arm::Left, true) => DisplaySlot::FirstPersonLeftHand,
        }
    }
}

/// Vanilla's held-item hand-layer submit function's adult hand offset, in model texels
/// (offset_x, offset_y, offset_z). `x` is mirrored by [`Arm::invert`].
///
/// Read from 26.2's decompiled source, where the three
/// values are `1.0F`, `2.0F` and `-10.0F` and the translate is
/// `((isLeftHand ? -1 : 1) * offsetX / 16, offsetY / 16, offsetZ / 16)`.
pub const HELD_ITEM_OFFSET_TEXELS: [f32; 3] = [1.0, 2.0, -10.0];

/// The same offsets for a **baby** (vanilla's own baby-offset flag): `0.0`, `1.0`, `-4.5`.
///
/// Vanilla's predicate is `state.isBaby && state.entityType != ARMOR_STAND`; an
/// armour stand is never a baby in the shell's data, so the caller's
/// "is this mob drawn small?" test is sufficient.
pub const HELD_ITEM_BABY_OFFSET_TEXELS: [f32; 3] = [0.0, 1.0, -4.5];

/// The `display` transform to pose an item held in `arm` under.
///
/// Uses [`DisplayTransforms::get`] rather than `declared`, because unlike
/// `ground` there is **no** sensible fallback constant for a hand slot:
/// `block/block` and `item/generated` disagree on far more than scale, so an
/// undeclared hand slot should get vanilla's own answer — the identity
/// (vanilla's own no-transform constant, which is only the `-0.5` centring) — and not a
/// guess. `get` also applies
/// [`DisplaySlot::left_hand_fallback`](lodestone_assets::DisplaySlot::left_hand_fallback),
/// which matters in practice: neither `block/block` nor `item/generated` declares
/// `thirdperson_lefthand`.
#[must_use]
pub fn hand_transform(
    display: &DisplayTransforms,
    arm: Arm,
    first_person: bool,
) -> DisplayTransform {
    display.get(arm.display_slot(first_person))
}

/// The world placement matrix for an item held in a mob's hand, matching
/// vanilla's held-item hand-layer submit function's pose-stack order exactly:
///
/// ```text
/// part_transforms[arm] · Rx(-90°) · Ry(180°) · T(±ox/16, oy/16, oz/16)
///                      · display_matrix_for_hand(thirdperson_?hand, is_left)
/// ```
///
/// `arm_transform` is vanilla's own hand-translate result, an
/// **entity→world** matrix: [`EntityInstance::hand_transform`]`(arm)` — *not*
/// `part_transforms[skeleton.index_of(arm.part_name())]`, which is the same
/// value only for the models with no override (see the table below and
/// [`HandPoseOverride`](crate::entity_anim::HandPoseOverride)).
///
/// # Verified against source, and the three offsets are not the whole story
///
/// Read from the 26.2 decompile, not transcribed from a summary. Two things the
/// short form hides:
///
/// * The item's own `display` transform is **not** applied by the layer — it
///   happens one level down, inside vanilla's per-layer item-stack render-state
///   submit function
///   → its apply-transform step → the item transform's own apply call against
///   the left-hand display context.
///   That is why the left-hand mirror lives in [`display_matrix_for_hand`] and is
///   applied here even when the transform came from the right-hand fallback:
///   the left-hand display context is a property of the *context*, not of where
///   the numbers came from.
/// * Vanilla's held-item hand-layer submit function has two further pose steps this does not model, both
///   gated on state the shell does not track: its third-person attacking-item
///   spear animation
///   (a stab swing mid-attack) and its using-item arm-pose animation (`ticksUsingItem != 0`,
///   i.e. drawing a bow, eating, blocking with a shield). Both are the identity in
///   the resting case this renders.
///
/// # How to change it: the per-model hand-transform overrides
///
/// For most models `arm_transform` is vanilla's base humanoid hand-transform
/// function, which
/// its illager and armour-stand model variants use too, and — because the composed
/// part matrix already carries the *whole* parent chain — also covers models
/// whose arms hang off `body` rather than `root` (vanilla's copper-golem model spells out
/// `root · body · arm`). Five corpus models in 26.2 append or prepend more, and
/// [`Skeleton::translate_to_hand`](crate::entity_anim::Skeleton::translate_to_hand)
/// now models every one of them, selected per model name by
/// [`hand_pose_override_for`]:
///
/// | model | override |
/// |---|---|
/// | `skeleton`, `stray`, `wither_skeleton` | pivot `x += ±1` texel *before* the arm's own matrix |
/// | `player_slim` | pivot `x += ±0.5` texel, same position |
/// | `vex` | then `scale(0.55)`, then `translate(±0.046875, -0.15625, 0.078125)` |
/// | `allay` | a different chain entirely: `root · body`, then `T(0, 1/16, 3/16) · Rx(right_arm.xRot) · S(0.7) · T(1/16, 0, 0)` — the arm's matrix is never used |
/// | `copper_golem` | not in the corpus |
///
/// The two *pivot-shift* rows cannot be expressed as a pre- or post-multiplication
/// of the arm's already-composed matrix, because the shift goes between the
/// parent chain and the arm's own rotation, which that matrix has already
/// folded together. That is why the fix lives in `entity_anim`
/// ([`Skeleton::translate_to_hand`](crate::entity_anim::Skeleton::translate_to_hand)),
/// operating on the posed-but-not-yet-composed parts, rather than as a
/// correction applied to `arm_transform` here.
///
/// **Not yet wired to a live server.** `lodestone-shell`'s `merge_held_items`
/// (`crates/lodestone-shell/src/gpu.rs`) still builds `arm_transform` by
/// indexing `instance.part_transforms[skeleton.index_of(arm.part_name())]`
/// directly, which is exactly [`EntityInstance::hand_transform`]'s
/// [`HandPoseOverride::Structural`](crate::entity_anim::HandPoseOverride::Structural)
/// case and therefore still correct for every model but these five. Swapping
/// that one lookup for `instance.hand_transform(arm)` is the remaining step —
/// deliberately left undone here because this file's remit was
/// `lodestone-render` only.
#[must_use]
pub fn held_item_matrix(
    arm_transform: Mat4,
    arm: Arm,
    baby: bool,
    transform: &DisplayTransform,
) -> Mat4 {
    let [ox, oy, oz] = if baby {
        HELD_ITEM_BABY_OFFSET_TEXELS
    } else {
        HELD_ITEM_OFFSET_TEXELS
    };
    arm_transform
        * Mat4::from_rotation_x((-90.0f32).to_radians())
        * Mat4::from_rotation_y(180.0f32.to_radians())
        * Mat4::from_translation(Vec3::new(
            arm.invert() * ox / UNITS_PER_BLOCK,
            oy / UNITS_PER_BLOCK,
            oz / UNITS_PER_BLOCK,
        ))
        * display_matrix_for_hand(transform, arm.is_left())
}

/// Mesh one held item's baked geometry into a world-space [`ModelMesh`], ready
/// for the ordinary [`ModelPipeline`](crate::ModelPipeline) with a *world* camera
/// uniform — the same treatment [`dropped_item_mesh`] gives a drop, and for the
/// same reason (the pose is folded into vertex positions, so there is no
/// per-instance matrix to batch on).
///
/// `light` is the holder's own packed sky/block sample: the geometry comes from
/// [`mesh_item_quads`], which nails every vertex to
/// [`GUI_ITEM_LIGHT`](crate::GUI_ITEM_LIGHT) because an inventory slot is
/// full-bright by definition, and a sword in a zombie's hand in a cave is not.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn held_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    arm_transform: Mat4,
    arm: Arm,
    baby: bool,
    transform: &DisplayTransform,
    light: u8,
) -> ModelMesh {
    let pose = held_item_matrix(arm_transform, arm, baby, transform);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// The arm's forced roll in vanilla's first-person hand render function, in **radians**
/// (right arm `rot_z = 0.1F`, left arm `rot_z = -0.1F`). Mirrored by
/// [`Arm::invert`].
pub const FIRST_PERSON_ARM_Z_ROT: f32 = 0.1;

/// Vanilla's first-person player-arm render function's equip-height coefficient on `y`
/// (`ARM_HEIGHT_SCALE = -0.6F`).
///
/// Numerically equal to [`FIRST_PERSON_ITEM_EQUIP_DIP`], and deliberately a
/// separate constant: the two live in different vanilla methods over different base
/// offsets (`-0.6` for the arm, `-0.52` for the item), so the equality is a
/// coincidence of 26.2's numbers rather than a shared rule.
pub const FIRST_PERSON_ARM_EQUIP_DIP: f32 = -0.6;

/// Vertical FOV the first-person arm is projected with, in degrees.
///
/// **Not the player's FOV.** Vanilla's level-render function sets a *separate*
/// projection for the hand — its own hud projection's perspective setup with
/// `(0.05F, 100.0F, cameraState.hud_fov, w, h)` — and vanilla's hud-fov calculation is a hard-coded
/// `70.0F` passed through its death/fluid FOV modifier. So the arm keeps a
/// constant apparent size while the world FOV changes (sprinting, the FOV
/// slider), which is exactly the behaviour players expect and would be lost by
/// reusing `Camera::projection_matrix`.
pub const HAND_FOV_Y_DEGREES: f32 = 70.0;

/// Near plane for [`hand_projection`] (vanilla's `0.05F`).
pub const HAND_NEAR: f32 = 0.05;

/// Far plane for [`hand_projection`] (vanilla's `100.0F` — *not* the world's
/// render-distance-derived far plane).
pub const HAND_FAR: f32 = 100.0;

/// The projection the first-person arm is drawn with: vanilla's own hud projection.
///
/// This is the **whole** transform for the hand pass. Vanilla's held-item-in-hand
/// render function
/// does a pose-stack multiply by the inverse of its own model-view matrix while
/// pushing that model-view matrix onto a separate stack, and the shader
/// multiplies `Proj · ModelViewStack · PoseStack` — so the view rotation
/// cancels exactly and
/// the arm pose is already in **camera space**. That model-view matrix there is
/// the camera state's own view-rotation matrix, rotation-only, which is why nothing has to
/// undo a camera translation either.
///
/// A view matrix is orthonormal-plus-translation, so `det(view) = +1` and
/// `sign(det(hand_projection)) == sign(det(Camera::view_projection))` — which
/// is why this is built from [`Camera`](crate::Camera) rather than assembled
/// separately. The arm pose must therefore have a **positive** determinant,
/// exactly like a world model matrix and unlike the GUI item pose — see
/// `first_person_arm_pose_preserves_winding`.
#[must_use]
pub fn hand_projection(aspect: f32) -> Mat4 {
    // Built through `Camera::projection_matrix` itself rather than through an
    // equivalent constructor, so the two cannot disagree about depth range or
    // handedness. That is not hypothetical: this function used to call glam's
    // *forward* `directx::perspective` directly, and when the world projection
    // became reversed-Z the hand pass was left projecting the other way — near
    // at 0 against a depth buffer cleared to 0 and compared with "nearer is
    // greater", which discards the entire arm. Only the position and the two
    // angles are unused here; every field the projection reads is set below.
    crate::Camera {
        fov_y_degrees: HAND_FOV_Y_DEGREES,
        aspect: if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.0
        },
        near: HAND_NEAR,
        far: HAND_FAR,
        ..crate::Camera::default()
    }
    .projection_matrix()
}

/// The camera-space chain vanilla's first-person player-arm render function
/// builds, driven by
/// `attack_anim`.
///
/// `attack_anim` is vanilla's own attack-swing progress accessor,
/// i.e. swing progress in `0.0..=1.0`, interpolated from the **tick** clock
/// (`lodestone_entity::pose::EntityPose::attack_anim_lerp`). `0.0` is a fully
/// rested arm and reproduces this function's behaviour before the swing existed,
/// byte for byte, which is what `arm_chain_at_rest_matches_the_static_chain`
/// pins. Values outside the range are clamped rather than extrapolated: the
/// shaping functions below are periodic, so an out-of-range value does not fail,
/// it silently animates something else.
///
/// ```text
/// s  = sqrt(a)                     -- vanilla's own float sqrt
/// xs = -0.3 · sin(s·π)
/// ys =  0.4 · sin(s·2π)
/// zs = -0.4 · sin(a·π)
/// yr =  sin(s·π)                   -- the y-swing rotation term
/// zr =  sin(a²·π)                  -- the z-swing rotation term
///
/// T(i·(xs + 0.64000005), ys - 0.6, zs - 0.71999997)
///   · Ry(i·45°) · Ry(i·yr·70°) · Rz(i·zr·-20°)
///   · T(i·-1, 3.6, 3.5) · Rz(i·120°) · Rx(200°) · Ry(i·-135°) · T(i·5.6, 0, 0)
/// ```
///
/// with `i` = [`Arm::invert`].
///
/// # The `sqrt` is the shape of the animation, not a detail
///
/// Three of the five terms are driven by `sqrt(a)` and one by `a²`, and only
/// the z-swing position term is linear in `a`. `sin(sqrt(a)·π)` rises far faster than
/// `sin(a·π)` and decays slowly — the arm snaps out and eases back, which is what
/// a swing *reads* as. Substituting a linear ramp gives a symmetric, sluggish
/// pendulum that is visibly not Minecraft, so this is transcribed term by term
/// from vanilla's first-person player-arm render function's decompiled source
/// rather than eyeballed.
///
/// Note the y-swing position term uses `2π`, not `π`: over one swing the arm's vertical
/// offset goes up, back through zero, and down again, rather than making a single
/// hump like `x` and `z`.
///
/// The dropped terms and why:
///
/// * Vanilla's hands-with-items submit function prefixes `Rx((view_pitch - x_bob) · 0.1°)` and
///   `Ry((view_yaw - y_bob) · 0.1°)`, and its held-item-in-hand render function prefixes its own
///   hurt-bob and view-bob terms. All four need state the shell does not have
///   (`x_bob`/`y_bob`, hurt time, walk distance); all four are the identity
///   when standing still.
/// * Vanilla's item-arm attack-transform function — the *item*-in-hand swing (`45° + yr·-20°`,
///   `zr'·-20°`, `xzr·-80°`) — is a **different** chain for the case where the
///   main hand is not empty and vanilla draws the item instead of the arm. It is
///   not this one and must not be folded in; see
///   `RenderState::prepare_first_person_hand`'s `FirstPersonHand::Item` branch,
///   which is the *other* half of vanilla's `isEmpty()` fork — see
///   [`first_person_item_chain`].
///
/// There is no `scale` anywhere in the chain, and that is not an omission — the
/// large constants (`3.6`, `3.5`, `5.6`) are in blocks and largely cancel through
/// the three rotations. At rest the composed arm cube lands roughly `0.35..0.9`
/// blocks right, `0.29..0.99` down and `0.44..1.19` forward of the eye, i.e.
/// bottom-right of frame, which is what
/// `the_first_person_arm_lands_in_the_bottom_right_of_frame` pins.
#[must_use]
pub fn first_person_arm_chain(arm: Arm, attack_anim: f32) -> Mat4 {
    first_person_arm_chain_with_equip(arm, attack_anim, 0.0)
}

/// [`first_person_arm_chain`] with vanilla's own equip-height term — the equip/swap
/// dip.
///
/// Vanilla's first-person player-arm render function translates `y` by
/// `y_swing_position + -0.6F + inverse_arm_height * -0.6F`, so the dip coefficient is
/// [`FIRST_PERSON_ARM_EQUIP_DIP`] and it is **the same `-0.6`** the item chain uses
/// ([`FIRST_PERSON_ITEM_EQUIP_DIP`]) even though the two chains' *base* offsets
/// differ (`-0.6` here against the item's `-0.52`). Two constants rather than one
/// shared alias, because the equality is a coincidence of vanilla's numbers and
/// not a rule: they sit in different methods and either could move.
///
/// `inverse_arm_height` runs `0.0` (fully equipped, at rest) to `1.0` (fully
/// lowered, mid-swap). Passing a value outside that range is not clamped here —
/// the caller owns the ramp, and clamping in the matrix would hide a broken one.
///
/// [`first_person_arm_chain`] is this function at `0.0` and is kept as the name
/// every existing caller and gate uses, so adding the dip changed no call site's
/// behaviour and no test's expected matrix.
#[must_use]
pub fn first_person_arm_chain_with_equip(
    arm: Arm,
    attack_anim: f32,
    inverse_arm_height: f32,
) -> Mat4 {
    let i = arm.invert();
    let ArmSwingTerms {
        x_position,
        y_position,
        z_position,
        y_rotation,
        z_rotation,
    } = ArmSwingTerms::new(attack_anim);
    Mat4::from_translation(Vec3::new(
        i * (x_position + 0.640_000_05),
        y_position - 0.6 + inverse_arm_height * FIRST_PERSON_ARM_EQUIP_DIP,
        z_position - 0.719_999_97,
    )) * Mat4::from_rotation_y((i * 45.0).to_radians())
        * Mat4::from_rotation_y((i * y_rotation * 70.0).to_radians())
        * Mat4::from_rotation_z((i * z_rotation * -20.0).to_radians())
        * Mat4::from_translation(Vec3::new(i * -1.0, 3.6, 3.5))
        * Mat4::from_rotation_z((i * 120.0).to_radians())
        * Mat4::from_rotation_x(200.0f32.to_radians())
        * Mat4::from_rotation_y((i * -135.0).to_radians())
        * Mat4::from_translation(Vec3::new(i * 5.6, 0.0, 0.0))
}

/// The five scalars vanilla's first-person player-arm render function derives from `attack_value`, split out from
/// [`first_person_arm_chain`] so the *shaping* can be asserted against
/// hand-evaluated vanilla values on its own. Buried inside the matrix product,
/// swapping a `sqrt(a)` for an `a` is invisible: the matrix still moves, still has
/// determinant +1, and still keeps the arm on screen — it just animates wrong.
///
/// Every field is `0.0` at `attack_anim == 0.0`, which is what makes the swing
/// purely additive on top of the rest chain.
pub(crate) struct ArmSwingTerms {
    /// The x-swing position term, pre-`invert`: `-0.3 · sin(sqrt(a)·π)`.
    pub(crate) x_position: f32,
    /// The y-swing position term: `0.4 · sin(sqrt(a)·2π)` — note the `2π`.
    pub(crate) y_position: f32,
    /// The z-swing position term: `-0.4 · sin(a·π)`, the one linear-in-`a` term.
    pub(crate) z_position: f32,
    /// The y-swing rotation term: `sin(sqrt(a)·π)`, scaled by `70°` at the call site.
    pub(crate) y_rotation: f32,
    /// The z-swing rotation term: `sin(a²·π)`, scaled by `-20°` at the call site.
    pub(crate) z_rotation: f32,
}

impl ArmSwingTerms {
    /// `attack_anim` outside `0.0..=1.0` is clamped — see
    /// [`first_person_arm_chain`] on why extrapolating a periodic shaping
    /// function is worse than clamping it.
    pub(crate) fn new(attack_anim: f32) -> Self {
        use std::f32::consts::{PI, TAU};
        let a = attack_anim.clamp(0.0, 1.0);
        let s = a.sqrt();
        Self {
            x_position: -0.3 * (s * PI).sin(),
            y_position: 0.4 * (s * TAU).sin(),
            z_position: -0.4 * (a * PI).sin(),
            y_rotation: (s * PI).sin(),
            z_rotation: (a * a * PI).sin(),
        }
    }
}

/// The camera-space matrix to draw the first-person arm (and its sleeve) with, or
/// `None` if `mesh` has no such arm part.
///
/// ```text
/// first_person_arm_chain(arm, attack_anim) · rest_pose()[arm] · Rz(±0.1)
/// ```
///
/// Vanilla's first-person hand-render function calls its own arm reset-pose and then forces
/// `rot_z = ±0.1F`, so the arm part itself is drawn from its **authored rest pose**
/// with one rotation replaced — never from the third-person pose-setup result.
/// That is why this is a separate function from [`EntityInstance::part_transforms`]
/// and must stay one: the third-person player body needs the animated chain
/// (vanilla's attack-animation setup, which is
/// [`crate::entity_anim::Skeleton::pose`]'s `attack_anim`), and sharing a code
/// path would silently give one of the two the other's pose.
///
/// **The swing lives in the chain, not in the part pose**, and that is the whole
/// reason both can be animated by the same `attack_anim` number without sharing
/// any code: first person swings the *camera-space chain* the rested arm hangs
/// off, third person swings the *arm part* inside a rested body. Feeding this
/// function's `attack_anim` to `Skeleton::pose`, or vice versa, produces a
/// plausible-looking wrong answer, so the two paths take the same scalar and
/// nothing else.
///
/// `rest_pose()[arm] · Rz(0.1)` is *exact* rather than approximate because
/// `player_wide`'s `right_arm` is `PartPose::offset(-5, 2, 0)` with **zero** rest
/// rotation and hangs directly off an identity root — asserted by
/// `the_player_arm_rest_pose_is_a_pure_translation`, not commented.
///
/// `right_sleeve` is a child of `right_arm` at `PartPose::ZERO`, so it shares this
/// matrix exactly; [`first_person_arm_parts`] returns both indices for one matrix.
#[must_use]
pub fn first_person_arm_pose(mesh: &EntityMesh, arm: Arm, attack_anim: f32) -> Option<Mat4> {
    first_person_arm_pose_with_equip(mesh, arm, attack_anim, 0.0)
}

/// [`first_person_arm_pose`] with vanilla's own equip-height dip — see
/// [`first_person_arm_chain_with_equip`].
#[must_use]
pub fn first_person_arm_pose_with_equip(
    mesh: &EntityMesh,
    arm: Arm,
    attack_anim: f32,
    inverse_arm_height: f32,
) -> Option<Mat4> {
    let index = mesh.skeleton.index_of(arm.part_name())?;
    let rest = mesh.skeleton.rest_pose();
    let local = *rest.get(index)?;
    Some(
        first_person_arm_chain_with_equip(arm, attack_anim, inverse_arm_height)
            * local
            * Mat4::from_rotation_z(arm.invert() * FIRST_PERSON_ARM_Z_ROT),
    )
}

/// The mesh part indices [`first_person_arm_pose`]'s matrix draws: the arm, and
/// its sleeve overlay when the model has one.
///
/// Empty when the model has no such arm, so a caller can treat "no first-person
/// arm for this rig" as "draw nothing" without a second lookup.
#[must_use]
pub fn first_person_arm_parts(mesh: &EntityMesh, arm: Arm) -> Vec<usize> {
    let Some(index) = mesh.skeleton.index_of(arm.part_name()) else {
        return Vec::new();
    };
    let mut parts = vec![index];
    if let Some(sleeve) = mesh.skeleton.index_of(arm.sleeve_part_name()) {
        parts.push(sleeve);
    }
    parts
}

// ---------------------------------------------------------------------------
// The item in the first-person hand
// ---------------------------------------------------------------------------
//
// Vanilla draws the arm **or** the item, never both: its held-item hand-layer
// submit function branches
// on whether the item stack is empty and calls the arm-render function only in the empty case.
// So this is not a layer on top of `first_person_arm_chain` — it is the *other*
// branch, with its own translation and its own swing shaping, and folding one into
// the other produces a plausible-looking wrong pose. The two share only the
// attack-animation scalar.

/// Vanilla's item-arm transform function's translation, in blocks
/// (`invert * 0.56F`, `-0.52F`, `-0.72F`). `x` is mirrored by [`Arm::invert`] and
/// `y` additionally takes `inverse_arm_height * -0.6F`.
///
/// Note these are **not** [`first_person_arm_chain`]'s `0.64000005 / -0.6 /
/// -0.71999997`. The two chains are 0.08 blocks apart in `x`, which is small
/// enough to look like a rounding difference and is in fact the difference between
/// an item held in view and one clipping the frame edge.
pub const FIRST_PERSON_ITEM_OFFSET: [f32; 3] = [0.56, -0.52, -0.72];

/// Vanilla's item-arm transform function's equip-height coefficient on `y` (`-0.6F`).
pub const FIRST_PERSON_ITEM_EQUIP_DIP: f32 = -0.6;

/// The three scalars vanilla's item swing-arm function derives from `attack_value`.
///
/// **Different coefficients from [`ArmSwingTerms`]** (`-0.4 / 0.2 / -0.2` against
/// the arm's `-0.3 / 0.4 / -0.4`) and no rotation terms of its own — the rotation
/// comes from [`first_person_item_attack_chain`]. Kept as its own type so the two
/// cannot be swapped by autocomplete.
pub(crate) struct ItemSwingTerms {
    /// The x-swing position term, pre-`invert`: `-0.4 · sin(sqrt(a)·π)`.
    x_position: f32,
    /// The y-swing position term: `0.2 · sin(sqrt(a)·2π)` — the `2π`, as in the arm chain.
    y_position: f32,
    /// The z-swing position term: `-0.2 · sin(a·π)`.
    z_position: f32,
}

impl ItemSwingTerms {
    pub(crate) fn new(attack_anim: f32) -> Self {
        use std::f32::consts::{PI, TAU};
        let a = attack_anim.clamp(0.0, 1.0);
        let s = a.sqrt();
        Self {
            x_position: -0.4 * (s * PI).sin(),
            y_position: 0.2 * (s * TAU).sin(),
            z_position: -0.2 * (a * PI).sin(),
        }
    }
}

/// Vanilla's item-arm attack-transform function:
///
/// ```text
/// Ry(i·(45 + yr·-20)) · Rz(i·xzr·-20) · Rx(xzr·-80) · Ry(i·-45)
/// ```
///
/// with `yr = sin(a²·π)`, `xzr = sin(sqrt(a)·π)` and `i` = [`Arm::invert`].
///
/// **This is the identity at `attack_anim == 0.0`** — both shaping terms vanish and
/// the leading `Ry(i·45)` is cancelled exactly by the trailing `Ry(i·-45)`. That is
/// what makes the resting pose independent of the swing, and it is the property to
/// check first if a held item sits at a strange angle while standing still: a
/// dropped `Ry(i·-45)` looks like a permanent 45° twist, not like a broken swing.
#[must_use]
pub fn first_person_item_attack_chain(arm: Arm, attack_anim: f32) -> Mat4 {
    use std::f32::consts::PI;
    let i = arm.invert();
    let a = attack_anim.clamp(0.0, 1.0);
    let y_rotation = (a * a * PI).sin();
    let xz_rotation = (a.sqrt() * PI).sin();
    Mat4::from_rotation_y((i * (45.0 + y_rotation * -20.0)).to_radians())
        * Mat4::from_rotation_z((i * xz_rotation * -20.0).to_radians())
        * Mat4::from_rotation_x((xz_rotation * -80.0).to_radians())
        * Mat4::from_rotation_y((i * -45.0).to_radians())
}

/// The camera-space chain an item in the first-person hand is posed by, matching
/// vanilla's held-item hand-layer submit function's generic (melee/"whack") branch:
///
/// ```text
/// T(i·0.56, -0.52 + h·-0.6, -0.72)          -- applyItemArmTransform
///   · T(i·xs, ys, zs) · applyItemArmAttackTransform(arm, a)   -- swingArm
/// ```
///
/// `inverse_arm_height` is vanilla's own equip-height term — the equip/swap dip,
/// `swapAnimationScale(item) · (1 - lerp(oHeight, height))`. Pass `0.0` for a
/// fully-equipped hand; the shell tracks neither height, the same gap
/// [`first_person_arm_chain`] documents.
///
/// # The three swing animation types, and why the melee one is the one modelled
///
/// 26.2 branches on the item stack's own swing-animation type: the melee ("whack")
/// case runs
/// vanilla's item swing-arm function, the stab case runs vanilla's first-person
/// spear-attack animation, and the "none" case runs
/// nothing. At `attack_anim == 0.0` **all three are the identity**
/// ([`first_person_item_attack_chain`] cancels and the translations vanish), so a
/// resting hand is correct for every item whatever its type. Mid-swing, a spear
/// (stab) and the handful of "none" items get the melee motion here, which is
/// wrong but is a wrong *animation*, not a wrong resting pose — and it needs the
/// item's swing-animation-type component, which the item pipeline does not decode.
///
/// The determinant is **positive** (translations and rotations only), matching
/// [`hand_projection`]'s requirement — see `first_person_arm_pose_preserves_winding`
/// for why the hand pass takes the world rule and not the GUI one.
#[must_use]
pub fn first_person_item_chain(arm: Arm, attack_anim: f32, inverse_arm_height: f32) -> Mat4 {
    let i = arm.invert();
    let [ox, oy, oz] = FIRST_PERSON_ITEM_OFFSET;
    let ItemSwingTerms {
        x_position,
        y_position,
        z_position,
    } = ItemSwingTerms::new(attack_anim);
    Mat4::from_translation(Vec3::new(
        i * ox,
        oy + inverse_arm_height * FIRST_PERSON_ITEM_EQUIP_DIP,
        oz,
    )) * Mat4::from_translation(Vec3::new(i * x_position, y_position, z_position))
        * first_person_item_attack_chain(arm, attack_anim)
}

/// The full camera-space pose for an item in the first-person hand:
/// [`first_person_item_chain`] followed by the item's own
/// `firstperson_?hand` display transform.
///
/// `transform` is [`hand_transform`]`(&geometry.display, arm, true)` — note the
/// `true`. Passing `false` there is the silent failure mode: it reads
/// `thirdperson_righthand` instead, which for `item/generated` is a *different*
/// rotation and scale and puts the item at a visibly wrong angle without ever
/// putting it off screen.
#[must_use]
pub fn first_person_item_matrix(
    arm: Arm,
    attack_anim: f32,
    inverse_arm_height: f32,
    transform: &DisplayTransform,
) -> Mat4 {
    first_person_item_chain(arm, attack_anim, inverse_arm_height)
        * display_matrix_for_hand(transform, arm.is_left())
}

/// Vanilla's first-person eat-transform function's `Math.pow(scaled_usage_time, 27.0)`.
///
/// The exponent is the whole character of the animation and the one number a
/// "reasonable" simplification destroys. `1 - t` (a linear approach) and `1 - t^27`
/// agree only at the two endpoints: at `remaining = 30` of a 32-tick food the real
/// jiggle is `0.5755` and the linear reading is `0.03125`, an 18× difference, and by
/// `remaining = 24` the real one is already `0.9985` against `0.21875`. Linear reads
/// as the item *drifting* toward the mouth over the whole use; vanilla snaps it
/// there within about two ticks and then bobs.
pub const EAT_JIGGLE_EXPONENT: f64 = 27.0;

/// Vanilla's first-person eat-transform function's `scaled_usage_time < 0.8F` gate on the vertical bob.
///
/// **This is a bound on *remaining* time, so it opens *late*, not early.**
/// `scaled_usage_time` is `curr_usage_time / use_duration` where `curr_usage_time` counts
/// **down**, so the bob is suppressed for the first 20% of a use and runs for the
/// last 80% of it. Reading the comparison as "only near the start" — the natural
/// reading of `< 0.8` — inverts the animation and is invisible in a still frame.
pub const EAT_BOB_SCALED_LIMIT: f32 = 0.8;

/// Vanilla's first-person eat-transform function's `curr_usage_time`:
/// vanilla's own remaining-use-ticks accessor minus the frame interpolation
/// fraction, plus `1.0F`.
///
/// Named rather than inlined because the `+ 1.0` and the sign of the frame
/// interpolation fraction are both easy to lose and neither is checkable from
/// a screenshot: the result is a bob one tick out of phase. Note it can
/// exceed `use_duration` on the first tick of
/// a use (`remaining == duration` gives `duration + 1`), which makes
/// `scaledUsageTime > 1` and the jiggle **negative** for that instant. That is
/// vanilla, not a clamp we forgot — the item flicks away from the mouth before
/// coming to it.
#[must_use]
pub fn eat_usage_time(remaining_ticks: u32, partial_tick: f32) -> f32 {
    remaining_ticks as f32 - partial_tick + 1.0
}

/// Vanilla's first-person eat-transform function, transcribed: it computes a
/// remaining-use fraction (`curr_usage_time / use_duration`), applies a small
/// vertical bob while that fraction is below 0.8 (an absolute cosine of the
/// time, scaled), then derives a "jiggle" fraction as `1 - fraction^27`. The
/// jiggle drives a translate (sideways by arm, down, and — the z term is
/// always zero) and three successive axis rotations (Y, X, Z, all scaled by
/// the jiggle and, for Y and the sideways translate, by the arm's sign).
///
/// # Vanilla has no third-person counterpart
///
/// This is the *entire* eating animation. Vanilla's own humanoid arm-pose enum has no EAT or
/// DRINK variant and its arm-pose selection logic omits both from its chain, so
/// another player eating is drawn with the ordinary [`ArmPose::Item`](crate::ArmPose::Item)
/// raise plus crumbs. The dip, the twist and the bob exist only here.
///
/// # EAT and DRINK are the same transform
///
/// They are one `switch` case in vanilla's held-item hand-layer submit function, so a potion and a carrot move
/// identically in the hand. The two animations differ only in duration (via the
/// item's own consume-seconds property), sound, and whether particles are emitted at all.
///
/// # The `z` term is `eatJiggle * 0.0F`
///
/// Kept as a literal zero rather than dropped, because it is the one axis vanilla
/// deliberately does not move and a reader comparing against the Java otherwise has
/// to prove the omission was intentional.
#[must_use]
pub fn first_person_eat_transform(arm: Arm, curr_usage_time: f32, use_duration: u32) -> Mat4 {
    let i = arm.invert();
    let scaled = curr_usage_time / use_duration.max(1) as f32;
    let bob = if scaled < EAT_BOB_SCALED_LIMIT {
        // vanilla's own abs(cos(curr_usage_time / 4.0F * PI) * 0.1F) — an 8-tick period,
        // and the absolute value is what makes it a *bounce* rather than a
        // sinusoid: it never goes below the resting height.
        let height = ((curr_usage_time / 4.0 * std::f32::consts::PI).cos() * 0.1).abs();
        Mat4::from_translation(Vec3::new(0.0, height, 0.0))
    } else {
        Mat4::IDENTITY
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "vanilla's own `(float)Math.pow(..., 27.0)`: the pow is evaluated in double and narrowed"
    )]
    let jiggle = 1.0 - f64::from(scaled).powf(EAT_JIGGLE_EXPONENT) as f32;
    bob * Mat4::from_translation(Vec3::new(jiggle * 0.6 * i, jiggle * -0.5, jiggle * 0.0))
        * Mat4::from_rotation_y((i * jiggle * 90.0).to_radians())
        * Mat4::from_rotation_x((jiggle * 10.0).to_radians())
        * Mat4::from_rotation_z((i * jiggle * 30.0).to_radians())
}

/// The camera-space chain for an item being **eaten or drunk**, replacing
/// [`first_person_item_chain`] for as long as the use lasts.
///
/// ```text
/// the eat-transform (arm, curr_usage_time, use_duration)
///   · T(i·0.56, -0.52 + h·-0.6, -0.72)      -- the item-arm transform step
/// ```
///
/// # Two differences from [`first_person_item_chain`], both from the same `switch`
///
/// * **Vanilla's item-arm transform function comes *last*, not first.** The
///   eat/drink use-animation states have their own custom-arm-transform flag
///   set to true, so
///   the held-item hand-layer submit function skips the
///   pre-switch item-arm transform step and the case applies it *after*
///   the eat-transform step. Putting the offset first instead — the order every other
///   pose here uses — rotates the item about the camera rather than about the hand,
///   which swings it across the whole screen.
/// * **There is no swing.** The `player.isUsingItem()` branch never reaches
///   vanilla's own swing-arm step, so [`ItemSwingTerms`] and [`first_person_item_attack_chain`] do not
///   apply. Left-clicking while eating must not move the item.
#[must_use]
pub fn first_person_eat_chain(
    arm: Arm,
    curr_usage_time: f32,
    use_duration: u32,
    inverse_arm_height: f32,
) -> Mat4 {
    let i = arm.invert();
    let [ox, oy, oz] = FIRST_PERSON_ITEM_OFFSET;
    first_person_eat_transform(arm, curr_usage_time, use_duration)
        * Mat4::from_translation(Vec3::new(
            i * ox,
            oy + inverse_arm_height * FIRST_PERSON_ITEM_EQUIP_DIP,
            oz,
        ))
}

/// [`first_person_eat_chain`] followed by the item's own `firstperson_?hand`
/// display transform — the eating counterpart of [`first_person_item_matrix`].
#[must_use]
pub fn first_person_eat_matrix(
    arm: Arm,
    curr_usage_time: f32,
    use_duration: u32,
    inverse_arm_height: f32,
    transform: &DisplayTransform,
) -> Mat4 {
    first_person_eat_chain(arm, curr_usage_time, use_duration, inverse_arm_height)
        * display_matrix_for_hand(transform, arm.is_left())
}

/// The first-person item-use pose selected by
/// vanilla's held-item hand-layer submit function after it has resolved the item's
/// geometry.
///
/// `Bow` deliberately carries elapsed use ticks, not a precomputed power: the
/// same clock drives the pulling-model thresholds and vanilla's nonlinear draw
/// power, while keeping that conversion in the pose owner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FirstPersonItemUse {
    /// Vanilla's eat/drink use-animation state's custom arm transform.
    Eat {
        /// Vanilla's interpolated elapsed use time.
        curr_usage_time: f32,
        /// The item's configured use duration.
        use_duration: u32,
    },
    /// Vanilla's bow use-animation state's aimed, charging transform.
    Bow {
        /// Ticks elapsed since the bow use began.
        held_ticks: f32,
    },
}

/// Vanilla's bow draw-power function: the nonlinear charge fraction shared by its
/// launch velocity and its first-person pose.
#[must_use]
pub fn first_person_bow_power(held_ticks: f32) -> f32 {
    let charge = (held_ticks / 20.0).max(0.0);
    ((charge * charge + charge * 2.0) / 3.0).min(1.0)
}

/// Vanilla's bow use-animation-state transform, before the item's
/// own `firstperson_?hand` display transform.
///
/// ```text
/// T(i·0.56, -0.52 + h·-0.6, -0.72)          -- applyItemArmTransform
///   · T(i·-0.2785682, 0.18344387, 0.15731531)
///   · Rx(-13.935) · Ry(i·35.3) · Rz(i·-9.785)
///   · T(0, shake, 0) · T(0, 0, power·0.04)
///   · S(1, 1, 1 + power·0.2) · Ry(i·-45)
/// ```
///
/// # The leading arm transform is not optional, and omitting it hides the bow
///
/// Vanilla's held-item hand-layer submit function applies the item-arm
/// transform step **before** entering the
/// use-animation switch for every animation whose own custom-arm-transform
/// flag is false, and the bow state's is false — only eat, drink and spear opt out, and the
/// first two then re-apply it themselves *after* their own transform (which is
/// why [`first_person_eat_chain`] composes it last and this one composes it
/// first). Starting the chain at the BOW-specific translation therefore drops
/// `z = -0.72` and the item sits on, or behind, the near plane: the bow vanishes
/// the instant the use begins rather than being drawn in the wrong place, which
/// is what makes the omission read as a use-state bug.
///
/// `inverse_arm_height` is vanilla's own equip-height term, the same equip/swap dip
/// [`first_person_item_chain`] takes; a charging bow still dips while swapping.
///
/// # The shake follows the rotations
///
/// Vanilla's own pose-stack translate by `(0, shake · 0.004, 0)` sits *after*
/// the three rotation multiplies, so it displaces the bow along the
/// **rotated** local Y, not along camera-space Y. Folding it into the
/// leading translation's `y` — the arithmetically tempting simplification,
/// since it is the only non-zero component — tilts the wobble into the wrong
/// plane. Vanilla's own sine LUT rather than
/// `f32::sin`: vanilla's is a quantized lookup table and this repo's ported
/// trigonometry goes through `lodestone_physics::mth` for that reason.
#[must_use]
pub fn first_person_bow_chain(arm: Arm, held_ticks: f32, inverse_arm_height: f32) -> Mat4 {
    let i = arm.invert();
    let [ox, oy, oz] = FIRST_PERSON_ITEM_OFFSET;
    let held_ticks = held_ticks.max(0.0);
    let power = first_person_bow_power(held_ticks);
    let shake = if power > 0.1 {
        lodestone_physics::mth::sin(f64::from((held_ticks - 0.1) * 1.3)) * (power - 0.1) * 0.004
    } else {
        0.0
    };
    Mat4::from_translation(Vec3::new(
        i * ox,
        oy + inverse_arm_height * FIRST_PERSON_ITEM_EQUIP_DIP,
        oz,
    )) * Mat4::from_translation(Vec3::new(i * -0.278_568_2, 0.183_443_87, 0.157_315_31))
        * Mat4::from_rotation_x((-13.935f32).to_radians())
        * Mat4::from_rotation_y((i * 35.3).to_radians())
        * Mat4::from_rotation_z((i * -9.785).to_radians())
        * Mat4::from_translation(Vec3::new(0.0, shake, 0.0))
        * Mat4::from_translation(Vec3::new(0.0, 0.0, power * 0.04))
        * Mat4::from_scale(Vec3::new(1.0, 1.0, 1.0 + power * 0.2))
        * Mat4::from_rotation_y((i * -45.0).to_radians())
}

/// [`first_person_bow_chain`] followed by the item's own
/// `firstperson_?hand` display transform.
#[must_use]
pub fn first_person_bow_matrix(
    arm: Arm,
    held_ticks: f32,
    inverse_arm_height: f32,
    transform: &DisplayTransform,
) -> Mat4 {
    first_person_bow_chain(arm, held_ticks, inverse_arm_height)
        * display_matrix_for_hand(transform, arm.is_left())
}

/// Mesh the item in the first-person hand into a camera-space [`ModelMesh`], to be
/// drawn through the ordinary [`ModelPipeline`](crate::ModelPipeline) with
/// [`hand_projection`] alone as the camera uniform (the same uniform the bare arm
/// uses, and for the same reason: the pose is already camera-space).
///
/// `item_use` selects the pose. It is a parameter rather than separate mesh
/// functions so the use cases cannot diverge in lighting or quad-meshing.
#[must_use]
pub fn first_person_item_mesh_with_use(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    arm: Arm,
    attack_anim: f32,
    inverse_arm_height: f32,
    transform: &DisplayTransform,
    light: u8,
    item_use: Option<FirstPersonItemUse>,
) -> ModelMesh {
    let pose = match item_use {
        Some(FirstPersonItemUse::Eat {
            curr_usage_time,
            use_duration,
        }) => first_person_eat_matrix(
            arm,
            curr_usage_time,
            use_duration,
            inverse_arm_height,
            transform,
        ),
        Some(FirstPersonItemUse::Bow { held_ticks }) => {
            first_person_bow_matrix(arm, held_ticks, inverse_arm_height, transform)
        }
        None => first_person_item_matrix(arm, attack_anim, inverse_arm_height, transform),
    };
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// Mesh the item in the first-person hand into a camera-space [`ModelMesh`], to be
/// drawn through the ordinary [`ModelPipeline`](crate::ModelPipeline) with
/// [`hand_projection`] alone as the camera uniform (the same uniform the bare arm
/// uses, and for the same reason: the pose is already camera-space).
#[must_use]
pub fn first_person_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    arm: Arm,
    attack_anim: f32,
    inverse_arm_height: f32,
    transform: &DisplayTransform,
    light: u8,
) -> ModelMesh {
    let pose = first_person_item_matrix(arm, attack_anim, inverse_arm_height, transform);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}
