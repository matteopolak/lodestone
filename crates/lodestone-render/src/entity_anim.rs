//! Per-part entity animation: head tracking, walk cycles and attack swings.
//!
//! [`entity`](crate::entity) places a whole mob with one matrix, which is
//! correct for a statue and wrong for anything alive. Vanilla animates by
//! adjusting each `ModelPart`'s `PartPose` before rendering (`Model.setupAnim`),
//! then walking the part hierarchy composing transforms. This module does the
//! same thing over [`BakedPart`]s: it copies the rest poses, applies a family's
//! `setupAnim`, and composes the chain into one matrix per part.
//!
//! # Why per-part matrices rather than re-baking vertices
//!
//! A posed mob could be produced either by transforming its vertices on the CPU
//! each frame, or by keeping the vertices fixed in part-local space and handing
//! the GPU one matrix per part. The second wins for the same reason instancing
//! wins generally: a mob is ~10–35 parts but hundreds of quads, so the matrix
//! form moves ~1% of the data, and the existing entity pipeline already draws
//! instanced from a matrix buffer — animation becomes *more* instances, not a
//! new upload path.
//!
//! # Family classification is structural, not a name list
//!
//! Vanilla picks an animation by model class. We cannot see classes, so the
//! family is derived from which parts a model actually has — a model with
//! `right_hind_leg`/`left_front_leg`/… is a quadruped whatever it is called.
//! A hardcoded mob-name list would be a version-specific fact smuggled into a
//! version-free crate, and would silently freeze any mob added later; a
//! structural rule classifies new mobs correctly the day they are ported.
//!
//! # Scope, stated honestly
//!
//! The formulas are transcribed from the decompiled 26.2 client with the *state*
//! we actually track: body/head rotation, walk phase and amplitude, attack
//! progress and age. Vanilla's pose variations that depend on state we do not
//! yet decode — swimming, crouching, riding, fall-flying, per-arm item poses,
//! chicken wing flap speed — are deliberately absent rather than guessed. They
//! slot into the same functions when the state arrives.
//!
//! One `setupAnim` **override** is ported rather than just the base classes:
//! `AbstractZombieModel`'s raised arms ([`HumanoidArms::Zombie`]). It is not a
//! state gap — the pose is unconditional, so leaving it out drew every zombie,
//! husk and drowned with its arms hanging at its sides.
//!
//! # One thing here is not a `setupAnim` at all
//!
//! A creeper's pre-detonation swell ([`creeper_swell_scale`],
//! [`Skeleton::pose_swelling`]) is a **whole-model scale**, not a joint rotation.
//! In 26.2 it lives in `CreeperRenderer.scale` — a `PoseStack` op wrapped around
//! the model — and `CreeperModel.setupAnim` knows nothing about it. It is
//! implemented here anyway because this is the module that owns the part
//! matrices, and folding a scale into them is how a caller that only knows about
//! [`Skeleton`] can get it.
//!
//! Trigonometry here uses `f32::sin`/`cos` rather than vanilla's 65536-entry
//! `Mth` lookup table. That is a deliberate exception to the project's
//! bit-exactness rule: limb angles are never sent to a server and never feed
//! physics, so a sub-degree difference is invisible and unobservable. Anything
//! that *is* transmitted must still use the parity table.

use glam::{Mat4, Vec3};
use lodestone_assets::entity::{Affine, BakedPart, PartPose};

/// Radians per degree.
const DEG: f32 = std::f32::consts::PI / 180.0;
/// Vanilla's walk-cycle frequency multiplier (`walkAnimationPos * 0.6662`).
const WALK_FREQ: f32 = 0.6662;

/// Which arm rig a [`AnimFamily::Humanoid`] model animates with.
///
/// Vanilla expresses this by subclassing: `HumanoidModel.setupAnim` swings the
/// arms with the walk cycle, and `AbstractZombieModel.setupAnim` calls
/// `super.setupAnim` and then **overwrites** both arms via
/// `AnimationUtils.animateZombieArms`. A zombie's part hierarchy is identical to
/// a player's, so — unlike [`AnimFamily`] — this cannot be classified
/// structurally; the caller supplies it from the model name (see
/// [`humanoid_arms_for`](crate::entity::humanoid_arms_for)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HumanoidArms {
    /// `HumanoidModel`: arms swing opposite the legs, plus the idle bob.
    #[default]
    Swinging,
    /// `AbstractZombieModel`: both arms held out in front, walk swing discarded.
    Zombie,
}

/// Which of vanilla's per-model overrides of `HumanoidModel.translateToHand`
/// applies. Selected by the caller from the model name (see
/// [`hand_pose_override_for`](crate::entity::hand_pose_override_for)), the same
/// pattern [`HumanoidArms`] uses and for the same reason: a model class is not
/// visible to us, only the parts it declares and the name it was ported under.
///
/// # Why this cannot be a correction applied to `part_transforms[arm]`
///
/// Every one of these overrides is scoped to `translateToHand` alone — the
/// arm's *own* mesh keeps rendering through its unmodified pivot; only the
/// point a held item hangs from moves. `part_transforms[arm]` is the matrix the
/// whole-body instanced draw uses to place the arm's visible geometry
/// ([`crate::entity::plan_entities`]), so folding an override in there would
/// nudge the mob's visible forearm by the same amount it nudges the item — a
/// new, real defect traded for the one being fixed. [`Skeleton::translate_to_hand`]
/// therefore computes a *separate* matrix, never touching `part_transforms`.
///
/// The two pivot-shift cases below additionally cannot be expressed as a pre-
/// or post-multiplication of the arm's *already-composed* matrix: vanilla
/// shifts the arm's own pivot **before** its rotation is applied
/// (`part.x += offset; part.translateAndRotate(poseStack); part.x -= offset;`),
/// and `T(pivot) · R(rot)` does not commute, so the shift has to be folded in
/// while the pivot and the rotation are still two separate values — i.e. from
/// the posed [`PartPose`], not from the [`Mat4`] that already fused them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum HandPoseOverride {
    /// `HumanoidModel.translateToHand` / `IllagerModel` / `ArmorStandModel`:
    /// `root.translateAndRotate(); getArm(arm).translateAndRotate();` — exactly
    /// the ordinary composed chain, so this is the same value
    /// `part_transforms[arm]` already holds. Also correct, unmodified, for a
    /// model whose arms hang off `body` rather than `root` (`CopperGolemModel`)
    /// — the composed chain already includes the whole parent hierarchy.
    #[default]
    Structural,
    /// `SkeletonModel`/`StrayModel`/`WitherSkeletonModel.translateToHand`: the
    /// arm's pivot `x` is shifted by `±1.0` texel before its own rotation
    /// (`+1` for the right arm, `-1` for the left — vanilla's
    /// `arm == RIGHT ? 1.0F : -1.0F`). `PlayerModel`'s slim variant is the
    /// identical shift at `±0.5` texels. The `f32` is the magnitude in texels;
    /// the sign is derived from the arm at call time.
    PivotShiftTexels(f32),
    /// `VexModel.translateToHand`: `root · body · arm`, then `scale(0.55)`,
    /// then `translate(±0.046875, -0.15625, 0.078125)` (sign by arm). Vex's
    /// arms hang off `body`, not `root`, which the ordinary chain already
    /// handles — the override is the trailing scale-and-translate vanilla adds
    /// *after* the arm's own transform.
    Vex,
    /// `AllayModel.translateToHand`: a wholly different chain that never calls
    /// `getArm(arm).translateAndRotate()` at all — `root · body`, then
    /// `T(0, 1/16, 3/16) · Rx(right_arm.xRot) · S(0.7) · T(1/16, 0, 0)`.
    /// Vanilla does not branch on `arm` anywhere in this override, not even the
    /// translate's sign, so an off-hand item on an allay is posed identically
    /// to a main-hand one — read from source, not a guess: see
    /// `AllayModel.java`'s `translateToHand`.
    Allay,
}

/// `AnimationUtils.animateZombieArms`'s resting arm elevation,
/// `-PI / (isAggressive ? 1.5 : 2.25)` radians about X. Negative raises the arm
/// forward in the Y-down model frame, which is the "arms out in front" pose.
#[must_use]
fn zombie_arm_x_rest(aggressive: bool) -> f32 {
    -std::f32::consts::PI / if aggressive { 1.5 } else { 2.25 }
}

/// `animateZombieArms`'s resting arm splay: `rightArm.yRot = -0.1`,
/// `leftArm.yRot = 0.1` when not mid-swing.
const ZOMBIE_ARM_Y_REST: f32 = 0.1;

/// The largest value `Creeper.getSwelling` can return: `30 / (30 - 2)`.
///
/// Vanilla computes `Mth.lerp(partialTick, oldSwell, swell) / (maxSwell - 2)`
/// with `swell` capped at `maxSwell` and `maxSwell` defaulting to `30`. The
/// divisor is `28`, not `30`, so the parameter overshoots `1.0` by ~7% at the
/// instant of detonation — which is *deliberate*, not a rounding artefact: it is
/// what makes the creeper still be growing when it explodes rather than easing
/// to a stop. `maxSwell` is saved to NBT (`Fuse`) but never synchronised, so a
/// client always divides by 28.
pub const MAX_SWELL: f32 = 30.0 / (30.0 - 2.0);

/// A conservative upper bound on any component [`creeper_swell_scale`] returns
/// over `0..=MAX_SWELL`, for sizing bounds that must contain a swelling creeper.
///
/// The horizontal factor is `(1 + g⁴·0.4) · wobble` with `g⁴ ≤ 1` and
/// `|wobble - 1| ≤ MAX_SWELL · 0.01`, so it cannot exceed `1.4 · (1 + 0.0107)`.
/// The vertical factor peaks lower (`1.1 / 0.9893 ≈ 1.112`).
///
/// **This is not yet applied to anything.** [`EntityMesh::local_min`] /
/// `local_max` in [`crate::entity`] are derived from
/// [`Skeleton::rest_pose`], so a creeper's culling box describes it at its
/// resting size while a swelling one is drawn up to 41% wider. That is the
/// "correct until it clips at the screen edge" failure `from_named_model`'s own
/// doc comment warns about, in a file this change was not permitted to touch;
/// widening the creeper's local bounds by this factor is the fix.
///
/// [`EntityMesh::local_min`]: crate::entity::EntityMesh::local_min
pub const MAX_SWELL_SCALE: f32 = 1.4 * (1.0 + MAX_SWELL * 0.01);

/// `CreeperRenderer.scale`: the per-axis model scale for a creeper `swelling`
/// fraction, as `[x, y, z]`.
///
/// Transcribed from the decompiled 26.2 client:
///
/// ```text
///   float g = state.swelling;
///   float wobble = 1.0F + Mth.sin(g * 100.0F) * g * 0.01F;
///   g = Mth.clamp(g, 0.0F, 1.0F);
///   g *= g;
///   g *= g;
///   float s  = (1.0F + g * 0.4F) * wobble;
///   float hs = (1.0F + g * 0.1F) / wobble;
///   poseStack.scale(s, hs, s);
/// ```
///
/// Two details that a summary of this animation usually loses, and that are the
/// whole look:
///
/// * The dominant term is the **growth** `1 + g⁴·0.4`, which puffs the creeper
///   out by up to 40% horizontally and 10% vertically. The `sin(g · 100)`
///   `wobble` is a ±1% shudder *on top* of it — describing the swell as just
///   the sine term reproduces a barely-visible jitter and none of the swelling.
/// * `g` is raised to the **fourth** power, so nearly nothing happens for the
///   first two thirds of the fuse and then it balloons. A linear ramp would be
///   the same total growth spread evenly and would read as a different mob.
/// * Horizontal and vertical are *reciprocal* in `wobble` — the creeper
///   squashes as it widens, conserving its apparent bulk while it shudders.
///
/// The sine is `f32::sin` rather than vanilla's `Mth` table, per the module
/// note: this feeds a scale, never the wire.
#[must_use]
pub fn creeper_swell_scale(swell: f32) -> [f32; 3] {
    let wobble = 1.0 + (swell * 100.0).sin() * swell * 0.01;
    let g = swell.clamp(0.0, 1.0);
    let g = g * g;
    let g = g * g;
    let horizontal = (1.0 + g * 0.4) * wobble;
    let vertical = (1.0 + g * 0.1) / wobble;
    [horizontal, vertical, horizontal]
}

/// `CreeperRenderer.getWhiteOverlayProgress`: the white-flash overlay strength
/// for a given swell, `0.0..=1.0`.
///
/// Transcribed from the decompiled 26.2 client:
///
/// ```text
///   float step = state.swelling;
///   return (int)(step * 10.0F) % 2 == 0 ? 0.0F : Mth.clamp(step, 0.5F, 1.0F);
/// ```
///
/// # This is a **blink**, not a ramp
///
/// The player-visible defect this exists to fix was described as the creeper
/// failing to "expand/turn white or blink or whatever" — and "blink" is
/// exactly right, not a loose description of a fade. `swelling` is bucketed
/// into steps of `0.1`; even-numbered steps (`[0.0,0.1)`, `[0.2,0.3)`, …) are
/// fully off, odd-numbered steps are on at a strength clamped to
/// `0.5..=1.0`. So across one full fuse the overlay hard-cuts on/off five
/// times before detonation, each "on" pulse brighter than the last (`step`
/// itself is what gets clamped, and it is monotonically increasing), which is
/// the flicker vanilla is known for — not a smooth fade-to-white.
///
/// # Overlay progress vs. overlay alpha
///
/// This returns vanilla's `whiteOverlayProgress`, the same value
/// `OverlayTexture.pack(progress, redOverlay)` takes — it is **not** the
/// blend alpha yet. `OverlayTexture`'s white row quantises `progress` to
/// `u = floor(progress * 15)` and derives alpha as
/// `(1 - u/15 * 0.75) * 255`; see
/// [`crate::entity_pipeline::creeper_overlay_alpha_from_progress`] for that
/// second step, which a caller applies only when the hurt/death red overlay
/// is *not* also active — vanilla's texture has red and white on mutually
/// exclusive rows (`v == 3` for red ignores `u` entirely), so red always wins
/// when both are true.
#[must_use]
pub fn creeper_white_overlay_progress(swelling: f32) -> f32 {
    let step = swelling;
    if (step * 10.0) as i32 % 2 == 0 {
        0.0
    } else {
        step.clamp(0.5, 1.0)
    }
}

/// The model-space transform that reproduces vanilla's creeper swell, to be
/// composed *above* the root part.
///
/// # Why this is a scale about the feet and not about the model origin
///
/// `LivingEntityRenderer.render` orders the ops:
///
/// ```text
///   scale(-1, -1, 1)          // into the Y-down model frame
///   this.scale(state, stack)  // CreeperRenderer's swell
///   translate(0, -1.501, 0)   // lift the feet to the ground plane
/// ```
///
/// The swell is applied *before* the ground lift, so the lift is scaled too and
/// the creeper grows **upward out of the ground**. This crate applies the lift
/// later, in [`entity_model_matrix`](crate::entity::entity_model_matrix), so the
/// equivalent here is the conjugated form `T(+offset) ∘ S ∘ T(-offset)` about
/// the model-space plane the feet stand on.
///
/// Dropping the conjugation — scaling about the model origin, which is the
/// obvious thing to write — is not a subtle error: at full swell the vertical
/// factor `1.11` would drive the feet to `1.501 - 1.5·1.11 ≈ -0.16`, sinking the
/// creeper an eighth of a block into the floor as it inflates.
/// `swollen_creeper_keeps_its_feet_on_the_ground` pins this.
fn swell_root_affine(swell: f32) -> Affine {
    if swell == 0.0 {
        return Affine::IDENTITY;
    }
    let [sx, sy, sz] = creeper_swell_scale(swell);
    let offset = crate::entity::MODEL_FEET_OFFSET;
    Affine {
        m: [[sx, 0.0, 0.0], [0.0, sy, 0.0], [0.0, 0.0, sz]],
        t: [0.0, offset * (1.0 - sy), 0.0],
    }
}

/// Which of vanilla's `setupAnim` implementations a model animates with.
///
/// Derived from a model's part names by [`AnimFamily::classify`]; see the module
/// docs for why this is structural rather than a mob-name table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimFamily {
    /// No animatable joints found — drawn in its rest pose (boats, minecarts,
    /// slimes, end crystals).
    Static,
    /// Has a head but no recognised limb set: the head tracks, nothing else
    /// moves (shulker, allay, bat, squid).
    HeadOnly,
    /// Four legs in hind/front pairs — `QuadrupedModel`.
    Quadruped,
    /// Eight legs in hind/middle-hind/middle-front/front pairs — `SpiderModel`.
    Spider,
    /// Arms and legs — `HumanoidModel`.
    Humanoid,
    /// Fused `arms` part plus legs — `VillagerModel`.
    Villager,
    /// Wings plus legs — `ChickenModel`.
    Chicken,
}

impl AnimFamily {
    /// Classifies a model from the part names it declares.
    ///
    /// Checked most-specific first: an eight-legged model also matches the
    /// quadruped pattern, and a humanoid also has legs, so order is what makes
    /// the rules unambiguous.
    #[must_use]
    pub fn classify(names: &[&str]) -> Self {
        let has = |n: &str| names.iter().any(|p| *p == n);
        let legs = has("right_leg") && has("left_leg");
        if has("right_middle_front_leg") && has("right_middle_hind_leg") {
            AnimFamily::Spider
        } else if has("right_hind_leg")
            && has("left_hind_leg")
            && has("right_front_leg")
            && has("left_front_leg")
        {
            AnimFamily::Quadruped
        } else if legs && has("right_arm") && has("left_arm") {
            AnimFamily::Humanoid
        } else if legs && has("arms") {
            AnimFamily::Villager
        } else if legs && has("right_wing") && has("left_wing") {
            AnimFamily::Chicken
        } else if has("head") {
            AnimFamily::HeadOnly
        } else {
            AnimFamily::Static
        }
    }

    /// Whether this family's pose depends on anything but the rest pose. A
    /// `Static` model can reuse one cached matrix set for every instance.
    #[must_use]
    pub fn is_animated(self) -> bool {
        self != AnimFamily::Static
    }
}

/// The per-entity animation state a [`Skeleton`] poses from, already
/// interpolated for the frame.
///
/// This mirrors the subset of vanilla's `LivingEntityRenderState` that we track;
/// it is deliberately a plain value type so posing is a pure function and can be
/// unit-tested without a GPU, a world or a clock.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AnimInput {
    /// Head yaw **relative to the body**, in degrees (vanilla `netHeadYaw`).
    pub head_yaw_deg: f32,
    /// Head pitch in degrees, positive looking down (vanilla `headPitch`).
    pub head_pitch_deg: f32,
    /// Accumulated walk-cycle phase (`walkAnimationPos`).
    pub limb_swing: f32,
    /// Walk-cycle amplitude in `0..=1` (`walkAnimationSpeed`).
    pub limb_swing_amount: f32,
    /// Attack-swing progress in `0..=1` (`attackTime`); `0` means not swinging.
    pub attack_anim: f32,
    /// Continuous age in ticks, driving idle bob (`ageInTicks`).
    pub age_ticks: f32,
    /// Vanilla's `LivingEntityRenderState.isAggressive` (`Mob.isAggressive`),
    /// which raises a zombie's arms from `-PI/2.25` to `-PI/1.5`.
    ///
    /// It rides bit `0x04` of `Mob.DATA_MOB_FLAGS_ID` — a *separate* byte from
    /// the shared entity flags, at its own metadata index (15 in 26.2, confirmed
    /// against a jar dump rather than counted). An earlier note here called it a
    /// shared-flags bit; it is not, and looking for it at index 0 would find the
    /// unrelated unused bit there.
    ///
    /// **This used to be hardcoded `false` at every call site**, which made both
    /// consumers of it dead code: the zombie arm lift here, and (once #57 landed
    /// the pose machinery) the skeleton bow draw, which vanilla selects from this
    /// flag and *not* from the using-item bit. Issue #379 decoded the byte;
    /// `lodestone_shell::entities::render_anim` now feeds it from a `MobState`
    /// component. A `false` here still means exactly what it says — the pose an
    /// idle or merely-walking mob is in.
    pub aggressive: bool,
    /// How the arms are held for the item in use, if any (issue #57).
    ///
    /// Vanilla's `HumanoidModel.ArmPose`, reduced to the poses this build
    /// actually draws — see [`ArmPose`].
    pub arm_pose: ArmPose,
    /// Which hand holds the item [`arm_pose`](Self::arm_pose) describes.
    ///
    /// `false` is the main hand, which for every rig we draw is the right arm.
    /// Vanilla threads this as `AnimationUtils`' `holdingInRightArm` and as
    /// `HumanoidModel.setupAnim`'s `mainHandUsed == rightHanded` fork; it decides
    /// which arm *holds* and which arm *pulls*, so getting it wrong mirrors the
    /// pose rather than breaking it — a bow drawn with the wrong arm still looks
    /// like a bow draw, which is why it is a named field rather than an
    /// assumption.
    pub arm_pose_left_hand: bool,
}

impl AnimInput {
    /// The at-rest input: facing straight ahead, standing still, not attacking.
    pub const REST: AnimInput = AnimInput {
        head_yaw_deg: 0.0,
        head_pitch_deg: 0.0,
        limb_swing: 0.0,
        limb_swing_amount: 0.0,
        attack_anim: 0.0,
        age_ticks: 0.0,
        aggressive: false,
        arm_pose: ArmPose::Empty,
        arm_pose_left_hand: false,
    };
}

/// How a humanoid rig holds its arms for the item it is using — vanilla's
/// `HumanoidModel.ArmPose`, reduced to the cases this build draws.
///
/// # What is modelled, and what a variant means
///
/// Only the two-handed *ranged* poses, which are the ones issue #57 reported as
/// missing. `Empty` is "leave the arms wherever the walk cycle and the attack
/// swing put them" and is what every other item still gets — including a held
/// sword, which vanilla poses with `ITEM` (`xRot * 0.5 - PI/10`). That is a real
/// divergence and it is *deliberate*: `ITEM` needs to know only "is something in
/// the hand", which the equipment set already says, so it is a separate, cheap
/// follow-up rather than something to smuggle in behind a using-item bit that has
/// nothing to do with it.
///
/// Also absent, each for the same reason — they need per-item state this build
/// does not decode, and all of them are `Empty` today: `BLOCK` (shield, needs
/// `minecraft:blocks_attacks`), `SPYGLASS`, `TOOT_HORN`, `BRUSH`,
/// `THROW_TRIDENT` and `SPEAR`.
///
/// # Why the crossbow carries a fraction and the bow does not
///
/// The bow's arm pose is a *static* hold — vanilla's `BOW_AND_ARROW` arm is a
/// function of head rotation alone, and the draw progress goes into the **item's**
/// first-person transform, not the arms. The crossbow's `CROSSBOW_CHARGE` genuinely
/// interpolates the pulling arm over the charge, so it needs the fraction.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ArmPose {
    /// No item pose: the arms keep the walk/attack/idle result.
    #[default]
    Empty,
    /// Drawing a bow (`ArmPose::BOW_AND_ARROW`). Both arms come up in front,
    /// tracking the head.
    BowAndArrow,
    /// Winding a crossbow (`ArmPose::CROSSBOW_CHARGE`). The pulling arm rotates
    /// further as the charge advances.
    CrossbowCharge {
        /// Charge fraction in `0..=1`, vanilla's
        /// `clamp(ticksUsingItem, 0, maxCrossbowChargeDuration) /
        /// maxCrossbowChargeDuration`.
        ///
        /// The **fraction** rather than `(ticks, duration)` on purpose: the
        /// duration is `CrossbowItem.getChargeDuration`, which is
        /// `25 - 5 * QuickCharge level`, and resolving an enchantment level needs
        /// the enchantment registry. A caller with no enchantment data supplies
        /// `ticks / 25.0`, which is exact for an unenchanted crossbow and merely
        /// slow for an enchanted one; keeping that decision at the caller means
        /// this function cannot silently assume level 0.
        progress: f32,
    },
    /// Holding an already-charged crossbow (`ArmPose::CROSSBOW_HOLD`), which is
    /// **not** an in-use pose: vanilla shows it whenever a charged crossbow is
    /// held and the entity is not swinging, driven by the item's
    /// `minecraft:charged_projectiles` component rather than by the using-item
    /// bit.
    CrossbowHold,
}

impl ArmPose {
    /// Whether this pose occupies **both** arms, so the off hand's own pose is
    /// suppressed (vanilla `ArmPose.isTwoHanded`).
    ///
    /// Every pose modelled here is two-handed, which is not a coincidence — the
    /// ranged poses are exactly the ones that need a second arm — but it is
    /// asserted rather than assumed because the one-handed poses listed in the
    /// type docs will land here later.
    #[must_use]
    pub const fn is_two_handed(self) -> bool {
        match self {
            ArmPose::Empty => false,
            ArmPose::BowAndArrow | ArmPose::CrossbowCharge { .. } | ArmPose::CrossbowHold => true,
        }
    }
}

/// One node of an animatable model: its name, its parent, and its authored pose.
#[derive(Debug, Clone, PartialEq)]
struct SkelPart {
    name: String,
    parent: Option<usize>,
    rest: PartPose,
}

/// Named parts an animator needs to find, resolved once at load so posing never
/// does a string search.
///
/// Every slot is optional: a family is chosen from the parts a model *has*, but
/// a model may still be missing an incidental one (e.g. a quadruped with no
/// `body`), and a missing slot must skip that adjustment rather than panic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Slots {
    head: Option<usize>,
    body: Option<usize>,
    right_arm: Option<usize>,
    left_arm: Option<usize>,
    arms: Option<usize>,
    right_leg: Option<usize>,
    left_leg: Option<usize>,
    right_hind_leg: Option<usize>,
    left_hind_leg: Option<usize>,
    right_front_leg: Option<usize>,
    left_front_leg: Option<usize>,
    right_middle_hind_leg: Option<usize>,
    left_middle_hind_leg: Option<usize>,
    right_middle_front_leg: Option<usize>,
    left_middle_front_leg: Option<usize>,
    right_wing: Option<usize>,
    left_wing: Option<usize>,
}

/// A model's animatable skeleton: the part hierarchy stripped of geometry, plus
/// its [`AnimFamily`] and resolved part slots.
///
/// Built once per model type alongside its mesh; posing then costs one pass over
/// the parts with no allocation beyond the output matrices.
#[derive(Debug, Clone, PartialEq)]
pub struct Skeleton {
    parts: Vec<SkelPart>,
    family: AnimFamily,
    slots: Slots,
    arms: HumanoidArms,
}

impl Skeleton {
    /// Builds a skeleton from a model's [`bake_entity_parts`] output.
    ///
    /// [`bake_entity_parts`]: lodestone_assets::entity::bake_entity_parts
    #[must_use]
    pub fn from_parts(parts: &[BakedPart]) -> Self {
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        let family = AnimFamily::classify(&names);
        let find = |n: &str| names.iter().position(|p| *p == n);
        let slots = Slots {
            // Some models nest the visible head under a wrapper part that is the
            // one vanilla actually rotates (`head_parts` on horses). Prefer the
            // wrapper: rotating the inner part would pivot about the wrong point.
            head: find("head_parts").or_else(|| find("head")),
            body: find("body"),
            right_arm: find("right_arm"),
            left_arm: find("left_arm"),
            arms: find("arms"),
            right_leg: find("right_leg"),
            left_leg: find("left_leg"),
            right_hind_leg: find("right_hind_leg"),
            left_hind_leg: find("left_hind_leg"),
            right_front_leg: find("right_front_leg"),
            left_front_leg: find("left_front_leg"),
            right_middle_hind_leg: find("right_middle_hind_leg"),
            left_middle_hind_leg: find("left_middle_hind_leg"),
            right_middle_front_leg: find("right_middle_front_leg"),
            left_middle_front_leg: find("left_middle_front_leg"),
            right_wing: find("right_wing"),
            left_wing: find("left_wing"),
        };
        Skeleton {
            parts: parts
                .iter()
                .map(|p| SkelPart {
                    name: p.name.clone(),
                    parent: p.parent,
                    rest: p.rest,
                })
                .collect(),
            family,
            slots,
            arms: HumanoidArms::Swinging,
        }
    }

    /// Selects the humanoid arm rig, folding a [`HumanoidArms::Zombie`] rig's
    /// **resting** arm pose into the skeleton's authored rest.
    ///
    /// Why the rest pose and not just `setup_anim`: `animateZombieArms` assigns
    /// `xRot = -PI/2.25` and `yRot = ±0.1` unconditionally, so those *are* the
    /// zombie's resting arm angles — at `attackTime == 0` every other term in
    /// the formula is zero. Baking them in keeps the two invariants the rest
    /// pose carries elsewhere in the crate true for zombies as well: the mesh's
    /// local AABB (taken from [`Self::rest_pose`]) bounds the mob as drawn, and
    /// `pose(&AnimInput::REST)` differs from `rest_pose()` by no more than the
    /// idle arm sway. A non-humanoid, or a model with no arm slots, is
    /// unaffected.
    #[must_use]
    pub fn with_humanoid_arms(mut self, arms: HumanoidArms) -> Self {
        if self.family != AnimFamily::Humanoid {
            return self;
        }
        self.arms = arms;
        if arms == HumanoidArms::Zombie {
            let rest_x = zombie_arm_x_rest(false);
            for (slot, y) in [
                (self.slots.right_arm, -ZOMBIE_ARM_Y_REST),
                (self.slots.left_arm, ZOMBIE_ARM_Y_REST),
            ] {
                if let Some(i) = slot {
                    self.parts[i].rest.x_rot = rest_x;
                    self.parts[i].rest.y_rot = y;
                    self.parts[i].rest.z_rot = 0.0;
                }
            }
        }
        self
    }

    /// The animation family this model was classified into.
    #[must_use]
    pub fn family(&self) -> AnimFamily {
        self.family
    }

    /// The arm rig this model animates with.
    #[must_use]
    pub fn humanoid_arms(&self) -> HumanoidArms {
        self.arms
    }

    /// Number of parts (equal to the number of matrices [`Self::pose`] returns).
    #[must_use]
    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// Whether the model has no parts at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// The index of a part by name, for tests and debugging.
    #[must_use]
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.parts.iter().position(|p| p.name == name)
    }

    /// Poses the skeleton for `input` and returns one model-space matrix per
    /// part, in the same order as the [`BakedPart`]s it was built from.
    ///
    /// Multiply by the entity's placement matrix
    /// ([`entity_model_matrix`](crate::entity::entity_model_matrix)) to reach
    /// world space.
    #[must_use]
    pub fn pose(&self, input: &AnimInput) -> Vec<Mat4> {
        self.pose_swelling(input, 0.0)
    }

    /// [`Self::pose`], with a creeper's pre-detonation swell folded in as a
    /// whole-model scale (see [`creeper_swell_scale`]).
    ///
    /// `swell` is vanilla's `Creeper.getSwelling(partialTick)`: `0.0` while the
    /// fuse is unlit, rising to [`MAX_SWELL`] at detonation. `0.0` is exactly
    /// [`Self::pose`] — the scale reduces to the identity, not merely to
    /// something close to it, so passing it costs nothing and no non-creeper
    /// caller has to care.
    ///
    /// # This is a separate method, not a field on `AnimInput` — and that is
    /// still deliberate, just no longer for the reason once written here
    ///
    /// This section used to say **"no swell value exists to put in it."**
    /// That is false as of the chain landing, and it is worth recording
    /// exactly what closed it so the next reader does not have to re-derive
    /// it (a stale "still missing" claim here almost cost a wasted
    /// investigation once already): metadata index 16
    /// (`Creeper.DATA_SWELL_DIR`, decoded as `CreeperSwellDir`) reaches
    /// `ingest.rs:754`, folds into per-entity state at `state.rs:1128`,
    /// crosses the shell boundary at `net.rs:2108`, and lands on
    /// `entities.rs`' `CreeperFuse` component (`entities.rs:1832`/`1888`),
    /// which `tick_creeper_fuse` (`entities.rs:839`) integrates every tick —
    /// `swell += swell_dir` — exactly vanilla's `Creeper.getSwelling`
    /// client-side accumulation. `extract_entity_draws` (`entities.rs:1612`)
    /// reads that counter and is the caller that threads it into this
    /// method. `docs/entity-rendering.md` records the chain as closed.
    ///
    /// The "divide by 28" this method's own [`MAX_SWELL`] encodes is not a
    /// second, conflicting constant next to "`maxSwell` is 30" — they are the
    /// same fact: `Creeper.java`'s `maxSwell = 30`, and the client divides
    /// the accumulated counter by `maxSwell - 2 = 28`
    /// (`MAX_SWELL = 30.0 / (30.0 - 2.0)`, above).
    ///
    /// So a real, live-tested value exists and reaches this call. This stays
    /// a separate method rather than a field on [`AnimInput`] anyway: that
    /// struct is shared by every animated model, and threading a
    /// creeper-only scalar through it would widen every other caller's
    /// literal for a value only one rig uses — the current per-call
    /// parameter costs nothing and needs no such widening. Do not restructure
    /// `AnimInput` to add this field without a reason beyond tidiness; the
    /// current threading (`pose_swelling`/`creeper_swell_scale`, both already
    /// gated and tested) works.
    #[must_use]
    pub fn pose_swelling(&self, input: &AnimInput, swell: f32) -> Vec<Mat4> {
        self.compose_from(&self.posed(input), swell_root_affine(swell))
    }

    /// The unanimated matrices — what a [`AnimFamily::Static`] model draws with,
    /// and the baseline an animated one is compared against in tests.
    #[must_use]
    pub fn rest_pose(&self) -> Vec<Mat4> {
        let poses: Vec<PartPose> = self.parts.iter().map(|p| p.rest).collect();
        self.compose(&poses)
    }

    /// The animated per-part poses `input` produces, *before* composing them
    /// into a chain — i.e. what [`Self::setup_anim`] leaves in place. Shared by
    /// [`Self::pose_swelling`] (which composes the whole chain) and
    /// [`Self::translate_to_hand`] (which needs the arm's own pivot and
    /// rotation kept separate, not yet fused into a matrix).
    fn posed(&self, input: &AnimInput) -> Vec<PartPose> {
        let mut poses: Vec<PartPose> = self.parts.iter().map(|p| p.rest).collect();
        self.setup_anim(&mut poses, input);
        poses
    }

    /// The model-space matrix for `HumanoidModel.translateToHand(arm)` (or the
    /// model's own override — see [`HandPoseOverride`]), for the item this arm
    /// is holding.
    ///
    /// `input` re-derives the same animated pose [`Self::pose`] would, so a
    /// held item reflects the mob's current walk/attack state exactly as
    /// vanilla does (it poses the whole model, then runs `translateToHand`
    /// against the posed parts — never the rest pose). `left` selects the arm
    /// vanilla reads from `HumanoidArm`.
    ///
    /// Returns `None` only when this skeleton has no `right_arm`/`left_arm`
    /// slot at all — a family with no arms has nothing for a caller to fall
    /// back to.
    #[must_use]
    pub fn translate_to_hand(
        &self,
        input: &AnimInput,
        left: bool,
        override_: HandPoseOverride,
    ) -> Option<Mat4> {
        let arm_slot = if left {
            self.slots.left_arm
        } else {
            self.slots.right_arm
        }?;
        let poses = self.posed(input);
        // Same basis `pose()`/`part_transforms` use (root at identity, no
        // swell — only a creeper swells, and a creeper has no arms), so
        // `HandPoseOverride::Structural` below is bit-for-bit the value
        // `part_transforms[arm_slot]` already holds.
        let world = self.compose(&poses);
        let sign = if left { -1.0 } else { 1.0 };
        match override_ {
            HandPoseOverride::Structural => Some(world[arm_slot]),
            HandPoseOverride::PivotShiftTexels(texels) => {
                let parent_idx = self.parts[arm_slot].parent?;
                let mut shifted = poses[arm_slot];
                shifted.x += sign * texels;
                Some(world[parent_idx] * affine_to_mat4(&Affine::of_pose(&shifted)))
            }
            HandPoseOverride::Vex => {
                let body_idx = self.slots.body?;
                let arm_local = affine_to_mat4(&Affine::of_pose(&poses[arm_slot]));
                let scale = Mat4::from_scale(Vec3::splat(0.55));
                let post = Mat4::from_translation(Vec3::new(sign * 0.046875, -0.15625, 0.078125));
                Some(world[body_idx] * arm_local * scale * post)
            }
            HandPoseOverride::Allay => {
                let body_idx = self.slots.body?;
                let right_x_rot = poses[self.slots.right_arm?].x_rot;
                let pre = Mat4::from_translation(Vec3::new(0.0, 1.0 / 16.0, 3.0 / 16.0));
                let rot = Mat4::from_rotation_x(right_x_rot);
                let scale = Mat4::from_scale(Vec3::splat(0.7));
                let post = Mat4::from_translation(Vec3::new(1.0 / 16.0, 0.0, 0.0));
                Some(world[body_idx] * pre * rot * scale * post)
            }
        }
    }

    /// Walks the hierarchy composing each part's transform onto its parent's.
    ///
    /// A single forward pass suffices because [`bake_entity_parts`] emits parts
    /// in pre-order, so a parent's chain is always already computed.
    ///
    /// [`bake_entity_parts`]: lodestone_assets::entity::bake_entity_parts
    fn compose(&self, poses: &[PartPose]) -> Vec<Mat4> {
        self.compose_from(poses, Affine::IDENTITY)
    }

    /// [`Self::compose`], starting the chain from `root` rather than the
    /// identity, so a whole-model transform (the creeper swell) applies to every
    /// part without any part having to know about it.
    fn compose_from(&self, poses: &[PartPose], root: Affine) -> Vec<Mat4> {
        let mut chains: Vec<Affine> = Vec::with_capacity(self.parts.len());
        let mut out = Vec::with_capacity(self.parts.len());
        for (i, part) in self.parts.iter().enumerate() {
            let parent = part.parent.map_or(root, |p| chains[p]);
            let world = parent.compose(&Affine::of_pose(&poses[i]));
            chains.push(world);
            out.push(affine_to_mat4(&world));
        }
        out
    }

    /// Applies this model's family animation to a copy of the rest poses,
    /// mirroring the corresponding vanilla `setupAnim`.
    fn setup_anim(&self, poses: &mut [PartPose], input: &AnimInput) {
        let s = &self.slots;
        let pos = input.limb_swing;
        let amt = input.limb_swing_amount;

        // Every family but Static tracks with the head.
        if self.family != AnimFamily::Static
            && let Some(h) = s.head
        {
            // Added to the authored pose rather than assigned. Vanilla assigns
            // (`head.xRot = headPitch * DEG`), which is identical here for every
            // model that authors a zero head rotation -- but hoglin's head is
            // authored at 0.87 rad and vanilla *restores* exactly that in
            // `animateHeadbutt`, and the ender dragon likewise carries an
            // authored head rotation. Assigning would level both heads.
            poses[h].y_rot += input.head_yaw_deg * DEG;
            poses[h].x_rot += input.head_pitch_deg * DEG;
        }

        match self.family {
            AnimFamily::Static | AnimFamily::HeadOnly => {}

            // QuadrupedModel.setupAnim: diagonally opposite legs swing together.
            AnimFamily::Quadruped => {
                let swing = |phase: f32| (pos * WALK_FREQ + phase).cos() * 1.4 * amt;
                set_x_rot(poses, s.right_hind_leg, swing(0.0));
                set_x_rot(poses, s.left_hind_leg, swing(std::f32::consts::PI));
                set_x_rot(poses, s.right_front_leg, swing(std::f32::consts::PI));
                set_x_rot(poses, s.left_front_leg, swing(0.0));
            }

            // SpiderModel.setupAnim: legs splay outward (yRot) and lift (zRot),
            // each pair a quarter-cycle out of phase, and the adjustments are
            // *added* to the authored splay rather than replacing it.
            AnimFamily::Spider => {
                let p = pos * WALK_FREQ;
                let pairs = [
                    (s.right_hind_leg, s.left_hind_leg, 0.0),
                    (
                        s.right_middle_hind_leg,
                        s.left_middle_hind_leg,
                        std::f32::consts::PI,
                    ),
                    (
                        s.right_middle_front_leg,
                        s.left_middle_front_leg,
                        std::f32::consts::FRAC_PI_2,
                    ),
                    (
                        s.right_front_leg,
                        s.left_front_leg,
                        std::f32::consts::PI * 1.5,
                    ),
                ];
                for (right, left, phase) in pairs {
                    let swing = -((p * 2.0 + phase).cos() * 0.4) * amt;
                    let step = ((p + phase).sin() * 0.4).abs() * amt;
                    add_y_rot(poses, right, swing);
                    add_y_rot(poses, left, -swing);
                    add_z_rot(poses, right, step);
                    add_z_rot(poses, left, -step);
                }
            }

            // HumanoidModel.setupAnim, minus the swim/crouch/ride/item-pose
            // branches whose state we do not decode yet.
            AnimFamily::Humanoid => {
                let arm = |phase: f32| (pos * WALK_FREQ + phase).cos() * 2.0 * amt * 0.5;
                let leg = |phase: f32| (pos * WALK_FREQ + phase).cos() * 1.4 * amt;
                // A zombie rig runs `HumanoidModel.setupAnim` too, but
                // `animateZombieArms` then *assigns* over both arms, so the walk
                // swing and the first bob are dead stores. Skipping them here is
                // that same net effect without the wasted work — and, unlike
                // adding then overwriting, it cannot leave a stray term behind
                // if the zombie formula later grows an additive branch.
                if self.arms == HumanoidArms::Swinging {
                    set_x_rot(poses, s.right_arm, arm(std::f32::consts::PI));
                    set_x_rot(poses, s.left_arm, arm(0.0));
                }
                set_x_rot(poses, s.right_leg, leg(0.0));
                set_x_rot(poses, s.left_leg, leg(std::f32::consts::PI));
                // Vanilla nudges the legs off-axis so coincident faces never
                // z-fight when standing still.
                set_y_rot(poses, s.right_leg, 0.005);
                set_y_rot(poses, s.left_leg, -0.005);
                set_z_rot(poses, s.right_leg, 0.005);
                set_z_rot(poses, s.left_leg, -0.005);

                // Vanilla's ordering exactly: `setupAnim` poses the arms for the
                // item *after* the walk swing and *before* `setupAttackAnimation`
                // (`HumanoidModel.java:248-273`). It matters in both directions —
                // the item pose must overwrite the walk swing (it assigns), and
                // the attack swing must then be layered on top of it (it adds).
                self.pose_arms_for_item(poses, input);

                self.attack_anim(poses, input);

                match self.arms {
                    // AnimationUtils.bobModelPart on each arm, opposite signs.
                    HumanoidArms::Swinging => {
                        bob(poses, s.right_arm, input.age_ticks, 1.0);
                        bob(poses, s.left_arm, input.age_ticks, -1.0);
                    }
                    HumanoidArms::Zombie => self.animate_zombie_arms(poses, input),
                }
            }

            // VillagerModel.setupAnim: legs at half a humanoid's amplitude, arms
            // fused into one part that vanilla leaves at its authored pose.
            AnimFamily::Villager => {
                let leg = |phase: f32| (pos * WALK_FREQ + phase).cos() * 1.4 * amt * 0.5;
                set_x_rot(poses, s.right_leg, leg(0.0));
                set_x_rot(poses, s.left_leg, leg(std::f32::consts::PI));
                set_y_rot(poses, s.right_leg, 0.0);
                set_y_rot(poses, s.left_leg, 0.0);
            }

            // ChickenModel.setupAnim. The wing flap is driven by `flap`/
            // `flapSpeed`, which are wing-oscillator state we do not track, so
            // only the legs move; a guessed flap would be motion for its own
            // sake.
            AnimFamily::Chicken => {
                let leg = |phase: f32| (pos * WALK_FREQ + phase).cos() * 1.4 * amt;
                set_x_rot(poses, s.right_leg, leg(0.0));
                set_x_rot(poses, s.left_leg, leg(std::f32::consts::PI));
            }
        }
    }

    /// `AnimationUtils.animateZombieArms`: both arms held out in front, thrust
    /// forward and inward during an attack, then bobbed.
    ///
    /// Transcribed from the decompiled 26.2 client:
    ///
    /// ```text
    ///   f  = sin(attackTime * PI)
    ///   f1 = sin((1 - (1 - attackTime)^2) * PI)
    ///   rightArm.zRot = 0;            leftArm.zRot = 0
    ///   rightArm.yRot = -(0.1 - f*0.6); leftArm.yRot = 0.1 - f*0.6
    ///   f2 = -PI / (isAggressive ? 1.5 : 2.25)
    ///   rightArm.xRot = f2;           leftArm.xRot = f2
    ///   rightArm.xRot += f*1.2 - f1*0.4; leftArm.xRot += f*1.2 - f1*0.4
    ///   bobArms(leftArm, rightArm, ageInTicks)
    /// ```
    ///
    /// Three details that are easy to get wrong and are load bearing here:
    ///
    /// * It **assigns** all three rotations, so it overrides both the walk swing
    ///   and `setupAttackAnimation`'s arm rotations. A zombie's arms do not
    ///   swing as it walks — that is the whole visual signature.
    /// * It does **not** touch the arms' `x`/`z` *translation*, so
    ///   [`Self::attack_anim`]'s orbit of the twisting torso survives.
    /// * The bob is applied *again* after the assignment. Vanilla bobs zombie
    ///   arms twice (once in `HumanoidModel`, once here); only the second
    ///   survives the assignment, so exactly one bob is correct.
    ///
    /// The `x_rot`/`y_rot` assignments here reproduce the resting values
    /// [`Self::with_humanoid_arms`] folds into the rest pose whenever
    /// `attack_anim == 0` and `aggressive == false`, which is what keeps
    /// `pose(&AnimInput::REST)` and `rest_pose()` agreeing.
    fn animate_zombie_arms(&self, poses: &mut [PartPose], input: &AnimInput) {
        let t = input.attack_anim;
        let f = (t * std::f32::consts::PI).sin();
        let f1 = ((1.0 - (1.0 - t) * (1.0 - t)) * std::f32::consts::PI).sin();
        let x_rot = zombie_arm_x_rest(input.aggressive) + (f * 1.2 - f1 * 0.4);
        let y_rot = ZOMBIE_ARM_Y_REST - f * 0.6;
        let s = &self.slots;
        for (slot, sign) in [(s.right_arm, -1.0f32), (s.left_arm, 1.0)] {
            if let Some(i) = slot {
                poses[i].x_rot = x_rot;
                poses[i].y_rot = sign * y_rot;
                poses[i].z_rot = 0.0;
            }
        }
        bob(poses, s.right_arm, input.age_ticks, 1.0);
        bob(poses, s.left_arm, input.age_ticks, -1.0);
    }

    /// `HumanoidModel.poseRightArm`/`poseLeftArm` for the ranged [`ArmPose`]s:
    /// both arms come up to hold the weapon, tracking the head (issue #57).
    ///
    /// # These assign, they do not accumulate
    ///
    /// Vanilla writes `this.rightArm.xRot = …`, replacing whatever the walk cycle
    /// put there — an item pose is a *position*, not an offset. So this uses
    /// direct field assignment and **not** the `set_*_rot` helpers in this module,
    /// which despite their names do `+=` (see their doc comment). Using them here
    /// would leave the walk swing summed into the draw pose, making the bow hold
    /// wobble with the legs.
    ///
    /// # It reads the head, so the head must already be posed
    ///
    /// Every branch is a function of `head.y_rot`/`head.x_rot`, which
    /// [`Self::setup_anim`] writes before the family match. Reading it here rather
    /// than re-deriving from `head_yaw_deg`/`head_pitch_deg` is deliberate: the
    /// head pose is *added* to the model's authored rotation, so for a rig that
    /// authors a non-zero head (hoglin, ender dragon) the two differ, and vanilla
    /// reads the posed part.
    ///
    /// # A zombie rig deliberately loses this pose
    ///
    /// `HumanoidArms::Zombie` runs [`Self::animate_zombie_arms`] afterwards, which
    /// *assigns* over both arms and so erases whatever happened here. That is
    /// vanilla's behaviour, not a bug in the ordering: `AbstractZombieModel.setupAnim`
    /// calls `super.setupAnim` and then `AnimationUtils.animateZombieArms`
    /// unconditionally, so a bow-holding zombie keeps the arms-forward zombie pose
    /// in vanilla too. Skeletons — the rig the issue was reported against — are
    /// `HumanoidArms::Swinging` and do keep it.
    fn pose_arms_for_item(&self, poses: &mut [PartPose], input: &AnimInput) {
        let s = &self.slots;
        let (Some(right), Some(left)) = (s.right_arm, s.left_arm) else {
            return;
        };
        // `poseRightArm`/`poseLeftArm` differ only in which arm is treated as the
        // holder; vanilla picks by `mainHandUsed == rightHanded`. Both branches of
        // every modelled pose write both arms, so this resolves to one pair.
        let holding_in_right = !input.arm_pose_left_hand;
        let head_y_rot = s.head.map_or(0.0, |i| poses[i].y_rot);
        let head_x_rot = s.head.map_or(0.0, |i| poses[i].x_rot);

        match input.arm_pose {
            ArmPose::Empty => {}

            // `case BOW_AND_ARROW` (`HumanoidModel.java:353-357` for the right
            // arm, `:398-402` for the left). Both arms take the *same* xRot; the
            // two branches differ only in that the arm which is **not** holding
            // splays a further 0.4 rad away. Written out rather than folded into
            // one signed expression: the first attempt at that put the splay on
            // the wrong arm for the left-handed case and still produced a
            // plausible-looking bow draw, which is precisely the class of error a
            // screenshot cannot catch.
            ArmPose::BowAndArrow => {
                let x_rot = -std::f32::consts::FRAC_PI_2 + head_x_rot;
                if holding_in_right {
                    poses[right].y_rot = -0.1 + head_y_rot;
                    poses[left].y_rot = 0.1 + head_y_rot + 0.4;
                } else {
                    poses[right].y_rot = -0.1 + head_y_rot - 0.4;
                    poses[left].y_rot = 0.1 + head_y_rot;
                }
                poses[right].x_rot = x_rot;
                poses[left].x_rot = x_rot;
            }

            // `AnimationUtils.animateCrossbowCharge` (`AnimationUtils.java:20-32`).
            // The holding arm is fixed; the pulling arm's yaw lerps 0.4 -> 0.85 and
            // its pitch lerps toward -PI/2 as the charge advances. Note the holding
            // arm does **not** track the head here — unlike the hold pose below —
            // which is vanilla's asymmetry, not an omission.
            ArmPose::CrossbowCharge { progress } => {
                let (holding, pulling) = if holding_in_right {
                    (right, left)
                } else {
                    (left, right)
                };
                let sign = if holding_in_right { 1.0 } else { -1.0 };
                let alpha = progress.clamp(0.0, 1.0);
                poses[holding].y_rot = -0.8 * sign;
                poses[holding].x_rot = -0.970_796_35;
                poses[pulling].y_rot = lerp(alpha, 0.4, 0.85) * sign;
                poses[pulling].x_rot = lerp(alpha, -0.970_796_35, -std::f32::consts::FRAC_PI_2);
            }

            // `AnimationUtils.animateCrossbowHold` (`AnimationUtils.java:11-18`).
            ArmPose::CrossbowHold => {
                let (holding, shooting) = if holding_in_right {
                    (right, left)
                } else {
                    (left, right)
                };
                let sign = if holding_in_right { 1.0 } else { -1.0 };
                poses[holding].y_rot = -0.3 * sign + head_y_rot;
                poses[shooting].y_rot = 0.6 * sign + head_y_rot;
                poses[holding].x_rot = -std::f32::consts::FRAC_PI_2 + head_x_rot + 0.1;
                poses[shooting].x_rot = -1.5 + head_x_rot;
            }
        }
    }

    /// `HumanoidModel.setupAttackAnimation`'s `WHACK` branch: the body twists,
    /// both arms are carried around with it, and the swinging arm arcs down.
    ///
    /// Assumes the right arm is the attacking one — we do not decode a mob's
    /// main hand, and right is vanilla's default for every mob and the large
    /// majority of players.
    fn attack_anim(&self, poses: &mut [PartPose], input: &AnimInput) {
        let t = input.attack_anim;
        if t <= 0.0 {
            return;
        }
        let s = &self.slots;
        let body_yaw = (t.sqrt() * std::f32::consts::TAU).sin() * 0.2;
        set_y_rot(poses, s.body, body_yaw);
        // The arms orbit the twisting body so they stay attached to the torso.
        if let Some(i) = s.right_arm {
            poses[i].z = body_yaw.sin() * 5.0;
            poses[i].x = -body_yaw.cos() * 5.0;
            poses[i].y_rot += body_yaw;
        }
        if let Some(i) = s.left_arm {
            poses[i].z = -body_yaw.sin() * 5.0;
            poses[i].x = body_yaw.cos() * 5.0;
            poses[i].y_rot += body_yaw;
            poses[i].x_rot += body_yaw;
        }
        // The swing itself: an eased arc down, plus a lean tied to head pitch.
        let head_x_rot = s.head.map_or(0.0, |i| poses[i].x_rot);
        let eased = ease_out_quart(t);
        let arc = (eased * std::f32::consts::PI).sin();
        let lean = (t * std::f32::consts::PI).sin() * -(head_x_rot - 0.7) * 0.75;
        if let Some(i) = s.right_arm {
            poses[i].x_rot -= arc * 1.2 + lean;
            poses[i].y_rot += body_yaw * 2.0;
            poses[i].z_rot += (t * std::f32::consts::PI).sin() * -0.4;
        }
    }
}

/// `Mth.lerp(alpha, from, to)` — note vanilla's argument order puts the *alpha*
/// first, which is the opposite of most Rust `lerp` conventions and is the reason
/// this exists rather than a call to something in `glam`: transcribing
/// `Mth.lerp(lerpAlpha, 0.4F, 0.85F)` as `0.4.lerp(0.85, alpha)` is easy, and
/// getting it backwards silently swaps a crossbow's start and end pose.
fn lerp(alpha: f32, from: f32, to: f32) -> f32 {
    from + alpha * (to - from)
}

/// `Ease.outQuart`: `1 - (1 - t)^4`.
fn ease_out_quart(t: f32) -> f32 {
    let inv = 1.0 - t;
    1.0 - inv * inv * inv * inv
}

/// `AnimationUtils.bobModelPart`: a slow idle sway on an arm.
fn bob(poses: &mut [PartPose], slot: Option<usize>, age: f32, scale: f32) {
    if let Some(i) = slot {
        poses[i].z_rot += scale * ((age * 0.09).cos() * 0.05 + 0.05);
        poses[i].x_rot += scale * ((age * 0.067).sin() * 0.05);
    }
}

// Vanilla's `setupAnim` *assigns* limb rotations (`rightHindLeg.xRot = ...`)
// onto a part whose authored rotation it knows to be zero. Adding is identical
// wherever that holds -- `models_that_author_a_driven_limb_rotation` pins the
// corpus so the exception set cannot grow unnoticed -- and it stops a model
// with an authored limb pose (the ender dragon, whose legs sit at ~1.2 rad and
// which vanilla animates with a bespoke flight rig we do not model) from being
// flattened into a walking quadruped's rest pose.
fn set_x_rot(poses: &mut [PartPose], slot: Option<usize>, v: f32) {
    if let Some(i) = slot {
        poses[i].x_rot += v;
    }
}

fn set_y_rot(poses: &mut [PartPose], slot: Option<usize>, v: f32) {
    if let Some(i) = slot {
        poses[i].y_rot += v;
    }
}

fn set_z_rot(poses: &mut [PartPose], slot: Option<usize>, v: f32) {
    if let Some(i) = slot {
        poses[i].z_rot += v;
    }
}

fn add_y_rot(poses: &mut [PartPose], slot: Option<usize>, v: f32) {
    if let Some(i) = slot {
        poses[i].y_rot += v;
    }
}

fn add_z_rot(poses: &mut [PartPose], slot: Option<usize>, v: f32) {
    if let Some(i) = slot {
        poses[i].z_rot += v;
    }
}

/// Converts a row-major [`Affine`] into a column-major [`Mat4`].
fn affine_to_mat4(a: &Affine) -> Mat4 {
    Mat4::from_cols_array(&[
        a.m[0][0], a.m[1][0], a.m[2][0], 0.0, //
        a.m[0][1], a.m[1][1], a.m[2][1], 0.0, //
        a.m[0][2], a.m[1][2], a.m[2][2], 0.0, //
        a.t[0], a.t[1], a.t[2], 1.0,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use lodestone_assets::entity::bake_entity_parts;
    use lodestone_assets::entity_models::entity_models;

    /// Build a skeleton the way the renderer does — through
    /// [`crate::entity::humanoid_arms_for`], so a test never poses a rig the
    /// screen does not.
    fn skeleton_for(name: &str) -> Skeleton {
        let models = entity_models();
        let entry = models
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no model named {name}"));
        Skeleton::from_parts(&bake_entity_parts(&(entry.build)()))
            .with_humanoid_arms(crate::entity::humanoid_arms_for(name))
    }

    /// The forward reach of an arm in the model frame: how far a probe point
    /// `0.75` blocks down the limb axis sits toward the mob's facing (model
    /// `-Z`, which the placement flip turns into world-forward). Zero for an arm
    /// hanging straight down, `0.75` for one pointing dead ahead.
    fn arm_reach(skel: &Skeleton, arm: &str, input: &AnimInput) -> f32 {
        let i = skel.index_of(arm).unwrap_or_else(|| panic!("no {arm}"));
        let tip = skel.pose(input)[i].transform_point3(Vec3::new(0.0, 0.75, 0.0));
        -tip.z
    }

    /// The composed rest chain must reproduce the geometry `bake_entity`
    /// produces, or animation is posing a different model than the one drawn.
    #[test]
    fn posing_at_rest_keeps_every_joint_where_the_rest_pose_put_it() {
        // The mesh's local AABB is computed from `rest_pose`, so a joint that
        // moves at `AnimInput::REST` gives every instance a culling box that
        // describes a mob in a different place from the one drawn.
        //
        // Compared per joint, in the joint's own local frame -- otherwise a
        // legitimately rotated parent shows up as a translated child and the
        // assertion says nothing about where the rotation came from.
        for entry in entity_models() {
            let parts = bake_entity_parts(&(entry.build)());
            let skel = Skeleton::from_parts(&parts);
            let posed = skel.pose(&AnimInput::REST);
            let rest = skel.rest_pose();
            let local = |m: &[Mat4], i: usize| match parts[i].parent {
                Some(p) => m[p].inverse() * m[i],
                None => m[i],
            };
            for i in 0..parts.len() {
                let p = local(&posed, i);
                let r = local(&rest, i);
                let dt = (p.col(3) - r.col(3)).length();
                assert!(
                    dt < 1e-4,
                    "{}: joint {i} ({}) translated by {dt} at REST; its AABB would be wrong",
                    entry.name,
                    parts[i].name
                );
                for c in 0..3 {
                    let d = (p.col(c) - r.col(c)).length();
                    assert!(
                        d < 0.15,
                        "{}: joint {i} ({}) basis {c} rotated by {d} at REST -- larger than \
                         vanilla's 0.1 rad idle arm sway, so REST is not a resting pose",
                        entry.name,
                        parts[i].name
                    );
                }
            }
        }
    }

    #[test]
    fn models_that_author_a_driven_limb_rotation() {
        // `set_*_rot` adds rather than assigns (see its comment), which matches
        // vanilla exactly for every model whose driven limbs are authored at
        // zero rotation -- the overwhelming majority. This pins the set for
        // which that is *not* true, so the divergence stays explicit and a new
        // port cannot widen it unnoticed.
        //
        // For these, adding preserves the authored pose at rest (which is what
        // the culling AABB depends on) but diverges from vanilla's assignment
        // *while the limb is moving*. Spiders are not a divergence at all --
        // `SpiderModel.setupAnim` adds to the authored splay, which is why the
        // Spider arm uses `add_*` deliberately. The rest are approximations
        // pending a per-model `setupAnim` port, and are recorded in HANDOFF.md.
        let driven = [
            "right_leg",
            "left_leg",
            "right_arm",
            "left_arm",
            "arms",
            "right_hind_leg",
            "left_hind_leg",
            "right_front_leg",
            "left_front_leg",
            "right_wing",
            "left_wing",
        ];
        let mut found: Vec<&str> = Vec::new();
        for entry in entity_models() {
            let parts = bake_entity_parts(&(entry.build)());
            if parts.iter().any(|p| {
                driven.contains(&p.name.as_str())
                    && (p.rest.x_rot != 0.0 || p.rest.y_rot != 0.0 || p.rest.z_rot != 0.0)
            }) {
                found.push(entry.name);
            }
        }
        assert_eq!(
            found,
            [
                "spider",
                "cave_spider",
                "snow_golem",
                "ender_dragon",
                "witch",
                "villager",
                "rabbit",
                "bee",
                "parrot",
                "pillager",
                "vindicator",
                "evoker",
                "illusioner",
                "wandering_trader",
            ],
            "the set of models whose driven limbs carry an authored rotation changed; \
             each one animates differently from vanilla under additive `set_*_rot`"
        );
    }

    /// Where a model-space point ends up in world space, vertically.
    ///
    /// `entity_model_matrix` finishes with `scale(-1, -1, 1)` then
    /// `translate(0, -MODEL_FEET_OFFSET, 0)`, so a model-frame `y` (which points
    /// **down**) becomes `MODEL_FEET_OFFSET - y` blocks above the entity's feet
    /// position. Asserting in world terms is the point: "the creeper grows
    /// upward and does not sink" is a claim about the ground, and a model-frame
    /// assertion would state it upside down.
    fn world_height(model_y: f32) -> f32 {
        crate::entity::MODEL_FEET_OFFSET - model_y
    }

    /// The world height of the bottom of a creeper's hind foot: the leg part's
    /// pivot sits at `y = 18` texels and its cube runs 6 texels further down.
    fn creeper_foot_height(skel: &Skeleton, swell: f32) -> f32 {
        let i = skel.index_of("right_hind_leg").expect("hind leg");
        let leg = skel.pose_swelling(&AnimInput::REST, swell)[i];
        let sole = leg.transform_point3(Vec3::new(0.0, 6.0 / 16.0, 0.0));
        world_height(sole.y)
    }

    /// The world height of the top of a creeper's head: pivot at `y = 6` texels,
    /// cube top 8 texels above that.
    fn creeper_head_height(skel: &Skeleton, swell: f32) -> f32 {
        let i = skel.index_of("head").expect("head");
        let head = skel.pose_swelling(&AnimInput::REST, swell)[i];
        let crown = head.transform_point3(Vec3::new(0.0, -8.0 / 16.0, 0.0));
        world_height(crown.y)
    }

    #[test]
    fn swell_of_zero_is_bit_for_bit_the_unswollen_pose() {
        // Not "close to": `pose` delegates to `pose_swelling(_, 0.0)`, so any
        // drift here would apply to every mob in the game, every frame. The
        // guard is an exact `swell == 0.0` early return rather than a tolerance.
        assert_eq!(creeper_swell_scale(0.0), [1.0, 1.0, 1.0]);
        for entry in entity_models() {
            let skel = skeleton_for(entry.name);
            assert_eq!(
                skel.pose_swelling(&AnimInput::REST, 0.0),
                skel.pose(&AnimInput::REST),
                "{}: an unlit fuse perturbed the pose",
                entry.name
            );
        }
    }

    #[test]
    fn creeper_swell_scale_transcribes_the_vanilla_formula() {
        // Hand-evaluated from `CreeperRenderer.scale`, not read back off this
        // implementation.
        for swell in [0.25f32, 0.5, 0.75, 1.0, MAX_SWELL] {
            let wobble = 1.0 + (swell * 100.0).sin() * swell * 0.01;
            let g = swell.clamp(0.0, 1.0).powi(4);
            let want = [
                (1.0 + g * 0.4) * wobble,
                (1.0 + g * 0.1) / wobble,
                (1.0 + g * 0.4) * wobble,
            ];
            let got = creeper_swell_scale(swell);
            for axis in 0..3 {
                assert!(
                    (got[axis] - want[axis]).abs() < 1e-6,
                    "swell {swell} axis {axis}: {} vs {}",
                    got[axis],
                    want[axis]
                );
            }
        }
    }

    /// Hand-evaluated from `CreeperRenderer.getWhiteOverlayProgress`'s decompiled
    /// source at a grid of steps, not read back off this implementation — the
    /// same discipline `creeper_swell_scale_transcribes_the_vanilla_formula`
    /// uses one test up.
    #[test]
    fn white_overlay_progress_transcribes_the_vanilla_formula() {
        for step in [0.0f32, 0.05, 0.1, 0.15, 0.25, 0.35, 0.55, 0.65, 0.95, 1.0, MAX_SWELL] {
            let want = if (step * 10.0) as i32 % 2 == 0 {
                0.0
            } else {
                step.clamp(0.5, 1.0)
            };
            assert_eq!(
                creeper_white_overlay_progress(step),
                want,
                "step {step}"
            );
        }
    }

    /// The blink pattern by name: five on/off cycles across a fuse, each `on`
    /// pulse clamped to `0.5..=1.0` rather than fading smoothly. This is what
    /// distinguishes it from a ramp — see the function's own doc.
    #[test]
    fn white_overlay_progress_blinks_rather_than_ramps() {
        // [0.0, 0.1) is off.
        assert_eq!(creeper_white_overlay_progress(0.05), 0.0);
        // [0.1, 0.2) is on, clamped up to 0.5 even though step itself is small.
        assert_eq!(creeper_white_overlay_progress(0.15), 0.5);
        // [0.2, 0.3) is off again — a real hard cut, not a decay.
        assert_eq!(creeper_white_overlay_progress(0.25), 0.0);
        // [0.3, 0.4) is on, still clamped to 0.5 (step 0.35 < 0.5).
        assert_eq!(creeper_white_overlay_progress(0.35), 0.5);
        // Once step itself exceeds 0.5, an "on" pulse is no longer clamped flat:
        // [0.5, 0.6) reports the step value itself.
        assert!((creeper_white_overlay_progress(0.55) - 0.55).abs() < 1e-6);
        // [0.9, 1.0) is on at just under the maximum.
        assert!((creeper_white_overlay_progress(0.95) - 0.95).abs() < 1e-6);
    }

    /// The range is always `0.0..=1.0`, even at `MAX_SWELL` (~1.071, past 1.0) —
    /// `Mth.clamp(step, 0.5, 1.0)` caps the top explicitly, unlike
    /// `creeper_swell_scale`'s own `g` which clamps to `0.0..=1.0` *before*
    /// raising to the fourth power.
    #[test]
    fn white_overlay_progress_never_exceeds_one_even_past_max_swell() {
        let p = creeper_white_overlay_progress(MAX_SWELL);
        assert!((0.0..=1.0).contains(&p), "progress {p} out of range at MAX_SWELL");
    }

    #[test]
    fn the_swell_is_quartic_so_it_balloons_only_at_the_end() {
        // The signature of this animation: a creeper looks near-normal for most
        // of the fuse. A linear ramp would grow the same total amount and read
        // as a completely different mob, and it is the easy thing to write from
        // a prose description of the effect.
        let at = |s: f32| creeper_swell_scale(s)[0];
        // Half way through the fuse, `g⁴ = 0.0625`: 2.5% wider, not 20%.
        assert!(
            at(0.5) < 1.05,
            "half-fuse width was {}, which is a linear ramp, not a quartic one",
            at(0.5)
        );
        assert!(
            at(1.0) > 1.35,
            "full-fuse width was only {}; the creeper barely puffs at all",
            at(1.0)
        );
    }

    #[test]
    fn the_swell_wobbles_on_top_of_the_growth() {
        // `sin(swell * 100)` cycles every ~0.063 of swell, so two samples a half
        // cycle apart must straddle the growth curve rather than sit on it.
        let mid = 0.6f32;
        let half_cycle = std::f32::consts::PI / 100.0;
        let a = creeper_swell_scale(mid)[0];
        let b = creeper_swell_scale(mid + half_cycle)[0];
        assert!(
            (a - b).abs() > 1e-3,
            "the ±1% shudder is missing: {a} vs {b} a half wobble-cycle apart"
        );
        // And it is reciprocal between the axes: as it widens it squashes.
        let [x, y, _] = creeper_swell_scale(mid);
        let growth_free = (1.0 + (mid.powi(4)) * 0.4, 1.0 + (mid.powi(4)) * 0.1);
        assert!(
            (x > growth_free.0) == (y < growth_free.1),
            "the wobble moved both axes the same way; vanilla divides the vertical by it"
        );
    }

    #[test]
    fn swollen_creeper_keeps_its_feet_on_the_ground() {
        // The scale is conjugated about the ground plane because vanilla applies
        // it *before* the 1.501 ground lift. Scaling about the model origin
        // instead — the obvious implementation — buries the feet, and every
        // other assertion in this file would still pass.
        let skel = skeleton_for("creeper");
        let rest_foot = creeper_foot_height(&skel, 0.0);
        for step in 0..=64 {
            let swell = MAX_SWELL * step as f32 / 64.0;
            let foot = creeper_foot_height(&skel, swell);
            assert!(
                foot.abs() < 0.005,
                "at swell {swell} the sole sat {foot} blocks off the ground (rest: {rest_foot}); \
                 scaling about the model origin rather than the feet does exactly this"
            );
        }
    }

    #[test]
    fn a_swelling_creeper_grows_upward_and_outward() {
        let skel = skeleton_for("creeper");
        let rest = creeper_head_height(&skel, 0.0);
        let full = creeper_head_height(&skel, MAX_SWELL);
        assert!(
            (rest - 1.626).abs() < 0.01,
            "a resting creeper should stand 1.625 blocks tall, not {rest}"
        );
        assert!(
            full > rest * 1.08,
            "at full fuse the creeper reached {full} blocks against {rest} at rest — the vertical \
             factor is ~1.11, so this is not growing"
        );
        // Width: the head cube's half-extent is 4 texels either side.
        let i = skel.index_of("head").expect("head");
        let flank = |swell: f32| {
            skel.pose_swelling(&AnimInput::REST, swell)[i]
                .transform_point3(Vec3::new(4.0 / 16.0, 0.0, 0.0))
                .x
        };
        assert!(
            flank(MAX_SWELL) > flank(0.0) * 1.3,
            "the creeper widened from {} to {}; the horizontal factor is ~1.4",
            flank(0.0),
            flank(MAX_SWELL)
        );
    }

    #[test]
    fn max_swell_scale_bounds_every_point_of_the_fuse() {
        // `MAX_SWELL_SCALE` is what a culling box must be widened by. If it ever
        // stops being an upper bound, the box it sizes stops containing the mob.
        for step in 0..=4096 {
            let swell = MAX_SWELL * step as f32 / 4096.0;
            for axis in creeper_swell_scale(swell) {
                assert!(
                    axis <= MAX_SWELL_SCALE,
                    "swell {swell} scaled by {axis}, above the stated bound {MAX_SWELL_SCALE}"
                );
            }
        }
        // And it is not vacuously loose: the peak really does approach it.
        assert!(creeper_swell_scale(MAX_SWELL)[0] > MAX_SWELL_SCALE * 0.98);
    }

    #[test]
    fn rest_pose_matches_the_whole_model_bake() {
        let models = entity_models();
        let entry = models.iter().find(|e| e.name == "pig").unwrap();
        let def = (entry.build)();
        let parts = bake_entity_parts(&def);
        let skel = Skeleton::from_parts(&parts);
        let mats = skel.rest_pose();
        let whole = lodestone_assets::entity::bake_entity(&def);

        let mut recomposed = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            for quad in &part.quads {
                for p in quad.positions {
                    recomposed.push(mats[i].transform_point3(Vec3::from(p)));
                }
            }
        }
        assert_eq!(recomposed.len(), whole.len() * 4);
        for (got, want) in recomposed
            .iter()
            .zip(whole.iter().flat_map(|q| q.positions.iter()))
        {
            assert!(
                (*got - Vec3::from(*want)).length() < 1.0e-5,
                "rest pose diverges from bake_entity: {got:?} vs {want:?}"
            );
        }
    }

    #[test]
    fn families_are_classified_structurally() {
        assert_eq!(skeleton_for("pig").family(), AnimFamily::Quadruped);
        assert_eq!(skeleton_for("wolf").family(), AnimFamily::Quadruped);
        assert_eq!(skeleton_for("zombie").family(), AnimFamily::Humanoid);
        assert_eq!(skeleton_for("player_wide").family(), AnimFamily::Humanoid);
        assert_eq!(skeleton_for("spider").family(), AnimFamily::Spider);
        assert_eq!(skeleton_for("villager").family(), AnimFamily::Villager);
        assert_eq!(skeleton_for("chicken").family(), AnimFamily::Chicken);
        assert_eq!(skeleton_for("shulker").family(), AnimFamily::HeadOnly);
        assert_eq!(skeleton_for("boat").family(), AnimFamily::Static);
    }

    /// Walking must move the legs, and — the control that matters — standing
    /// still must not. Without the second half this passes for a model whose
    /// parts are simply always in motion.
    #[test]
    fn walking_swings_legs_and_standing_does_not() {
        for name in ["pig", "zombie", "villager", "chicken", "spider"] {
            let skel = skeleton_for(name);
            let leg = skel
                .index_of("right_hind_leg")
                .or_else(|| skel.index_of("right_leg"))
                .unwrap_or_else(|| panic!("{name} has no right leg"));

            let still = skel.pose(&AnimInput::REST);
            let still_again = skel.pose(&AnimInput {
                limb_swing: 7.0,
                ..AnimInput::REST
            });
            assert_eq!(
                still[leg], still_again[leg],
                "{name}: phase advanced at zero amplitude still moved the leg"
            );

            let walk_a = skel.pose(&AnimInput {
                limb_swing: 0.0,
                limb_swing_amount: 1.0,
                ..AnimInput::REST
            });
            let walk_b = skel.pose(&AnimInput {
                limb_swing: 3.0,
                limb_swing_amount: 1.0,
                ..AnimInput::REST
            });
            // Compare every column: an X-axis rotation leaves the X basis
            // vector and the pivot translation untouched, so a metric built
            // from those alone reads zero for a leg that is in fact swinging.
            let delta: f32 = (0..4)
                .map(|c| (walk_a[leg].col(c) - walk_b[leg].col(c)).length())
                .sum();
            assert!(
                delta > 1.0e-3,
                "{name}: leg did not move between walk phases (delta {delta})"
            );
        }
    }

    /// Legs on opposite sides must be out of phase, or the mob hops rather than
    /// walks. A formula that dropped the `+PI` would still pass a "the leg
    /// moved" assertion.
    #[test]
    fn opposing_legs_are_out_of_phase() {
        let skel = skeleton_for("pig");
        let right = skel.index_of("right_hind_leg").unwrap();
        let left = skel.index_of("left_hind_leg").unwrap();
        let mats = skel.pose(&AnimInput {
            limb_swing: 1.0,
            limb_swing_amount: 1.0,
            ..AnimInput::REST
        });
        // Compare each leg's forward axis against its own rest orientation; the
        // two must have rotated in opposite directions.
        let rest = skel.rest_pose();
        let swing_of = |i: usize| {
            let r = rest[i].col(1).truncate();
            let p = mats[i].col(1).truncate();
            r.cross(p).x
        };
        let (a, b) = (swing_of(right), swing_of(left));
        assert!(
            a * b < 0.0,
            "hind legs swung the same way (right {a}, left {b}) — phase offset lost"
        );
    }

    #[test]
    fn head_tracking_rotates_the_head_and_nothing_else() {
        let skel = skeleton_for("pig");
        let head = skel.index_of("head").unwrap();
        let body = skel.index_of("body").unwrap();
        let rest = skel.rest_pose();
        let looked = skel.pose(&AnimInput {
            head_yaw_deg: 45.0,
            head_pitch_deg: 20.0,
            ..AnimInput::REST
        });
        assert!(
            (looked[head].col(0) - rest[head].col(0)).length() > 0.1,
            "head did not rotate"
        );
        assert_eq!(looked[body], rest[body], "body moved with the head");
    }

    /// The reported defect: zombie arms hung at their sides instead of being
    /// held out in front.
    ///
    /// The reading is the arm's **forward reach**, not "did the matrix change" —
    /// a walk swing also changes the matrix, and the walk swing is exactly what
    /// this pose is not. A player is the control: same skeleton, same family,
    /// arms down, so a change that raised *every* humanoid's arms fails here.
    #[test]
    fn zombies_hold_their_arms_out_in_front_and_players_do_not() {
        let player = skeleton_for("player_wide");
        let player_reach = arm_reach(&player, "right_arm", &AnimInput::REST);
        assert!(
            player_reach.abs() < 0.1,
            "a resting player's arm should hang straight down, reached {player_reach} forward"
        );

        for name in ["zombie", "husk", "drowned", "zombie_villager"] {
            let skel = skeleton_for(name);
            assert_eq!(skel.humanoid_arms(), HumanoidArms::Zombie, "{name}");
            for arm in ["right_arm", "left_arm"] {
                let reach = arm_reach(&skel, arm, &AnimInput::REST);
                // -PI/2.25 (-80°) over a 0.75-block limb reaches 0.75*sin(80°)
                // ≈ 0.739 blocks forward. Anything under half a limb length is
                // an arm still pointing mostly downward.
                assert!(
                    reach > 0.5,
                    "{name}'s {arm} reached only {reach} blocks forward at rest — vanilla's \
                     animateZombieArms puts it at ~0.74"
                );
            }
        }
    }

    /// The pose must be *unconditional*, not a by-product of the walk cycle:
    /// vanilla assigns over `HumanoidModel`'s arm swing, so a walking zombie's
    /// arms stay out in front while its legs swing normally.
    #[test]
    fn a_walking_zombie_keeps_its_arms_out_and_still_swings_its_legs() {
        let skel = skeleton_for("zombie");
        let mid_walk = AnimInput {
            limb_swing: 0.0,
            limb_swing_amount: 1.0,
            ..AnimInput::REST
        };
        let half_cycle = AnimInput {
            limb_swing: std::f32::consts::PI / WALK_FREQ,
            ..mid_walk
        };

        for input in [&mid_walk, &half_cycle] {
            let reach = arm_reach(&skel, "right_arm", input);
            assert!(
                reach > 0.5,
                "a walking zombie's arm dropped to {reach} — the walk swing is still driving it"
            );
        }
        // Both walk phases must give the *same* arm pose (the swing is gone)...
        let a = arm_reach(&skel, "right_arm", &mid_walk);
        let b = arm_reach(&skel, "right_arm", &half_cycle);
        assert!(
            (a - b).abs() < 1.0e-5,
            "the arm moved between walk phases ({a} vs {b}) — animateZombieArms assigns, so a \
             zombie's arms do not swing as it walks"
        );
        // ...while the legs must still be swinging, or this passed by freezing
        // the whole model.
        let leg = skel.index_of("right_leg").unwrap();
        let delta: f32 = (0..4)
            .map(|c| {
                (skel.pose(&mid_walk)[leg].col(c) - skel.pose(&half_cycle)[leg].col(c)).length()
            })
            .sum();
        assert!(delta > 1.0e-3, "the legs stopped swinging too (delta {delta})");
    }

    /// `animateZombieArms`'s attack and aggression terms, which a pose that only
    /// hardcoded the resting elevation would silently drop.
    #[test]
    fn zombie_arms_thrust_on_attack_and_lift_when_aggressive() {
        let skel = skeleton_for("zombie");
        let rest = arm_reach(&skel, "right_arm", &AnimInput::REST);

        // f = sin(attackTime*PI) peaks at attackTime = 0.5, adding +1.2 rad, so
        // the arm swings up past vertical and its forward reach collapses.
        let mid_attack = arm_reach(
            &skel,
            "right_arm",
            &AnimInput {
                attack_anim: 0.5,
                ..AnimInput::REST
            },
        );
        assert!(
            mid_attack < rest - 0.3,
            "mid-attack reach {mid_attack} barely differs from rest {rest} — the f*1.2 - f1*0.4 \
             thrust is missing"
        );

        // isAggressive swaps -PI/2.25 for -PI/1.5, past horizontal, so the arm
        // reaches *less* far forward while sitting higher.
        let angry = arm_reach(
            &skel,
            "right_arm",
            &AnimInput {
                aggressive: true,
                ..AnimInput::REST
            },
        );
        assert!(
            (angry - rest).abs() > 0.05,
            "the aggressive branch changed nothing ({angry} vs {rest})"
        );
    }

    /// A swinging arm must leave the rest pose, and a non-swinging one must not.
    #[test]
    fn attack_swings_the_arm() {
        let skel = skeleton_for("zombie");
        let arm = skel.index_of("right_arm").unwrap();
        let rest = skel.pose(&AnimInput::REST);
        let mid = skel.pose(&AnimInput {
            attack_anim: 0.5,
            ..AnimInput::REST
        });
        assert!(
            (mid[arm].col(1) - rest[arm].col(1)).length() > 0.2,
            "attack animation did not move the arm"
        );
    }

    /// Every ported model must classify and pose without panicking, and every
    /// matrix must be finite — a NaN pivot silently deletes a mob from the
    /// screen rather than erroring.
    #[test]
    fn every_model_poses_finitely() {
        let mut animated = 0usize;
        for entry in entity_models() {
            let parts = bake_entity_parts(&(entry.build)());
            let skel = Skeleton::from_parts(&parts);
            assert_eq!(skel.len(), parts.len(), "{}", entry.name);
            if skel.family().is_animated() {
                animated += 1;
            }
            let mats = skel.pose(&AnimInput {
                head_yaw_deg: 60.0,
                head_pitch_deg: -30.0,
                limb_swing: 12.5,
                limb_swing_amount: 1.0,
                attack_anim: 0.75,
                age_ticks: 900.0,
                aggressive: true,
                ..AnimInput::REST
            });
            for (i, m) in mats.iter().enumerate() {
                assert!(
                    m.to_cols_array().iter().all(|v| v.is_finite()),
                    "{} part {i} produced a non-finite matrix",
                    entry.name
                );
            }
        }
        assert!(
            animated >= 60,
            "only {animated} of the corpus animates — classification is too narrow"
        );
    }

    // -----------------------------------------------------------------------
    // Arm poses for a used item (issue #57)
    // -----------------------------------------------------------------------

    /// The uncomposed arm rotations a skeleton rig ends up with, so the
    /// expectations below can be the vanilla constants themselves rather than a
    /// matrix nobody can check by eye.
    fn arm_rots(name: &str, input: &AnimInput) -> ((f32, f32, f32), (f32, f32, f32)) {
        let skel = skeleton_for(name);
        let poses = skel.posed(input);
        let r = skel.index_of("right_arm").expect("right arm");
        let l = skel.index_of("left_arm").expect("left arm");
        (
            (poses[r].x_rot, poses[r].y_rot, poses[r].z_rot),
            (poses[l].x_rot, poses[l].y_rot, poses[l].z_rot),
        )
    }

    /// **The expected values come from the 26.2 decompile, not from this code.**
    /// `HumanoidModel.poseRightArm`'s `case BOW_AND_ARROW` is four assignments
    /// with literal constants (`-0.1`, `0.1 + 0.4`, `-PI/2` twice); they are
    /// restated here so a change to the port shows up as a disagreement with
    /// vanilla rather than with our own previous output.
    #[test]
    fn the_bow_pose_matches_vanilla_pose_right_arm_constants() {
        // Head straight ahead isolates the constants from the head terms.
        let input = AnimInput {
            arm_pose: ArmPose::BowAndArrow,
            ..AnimInput::REST
        };
        let ((rx, ry, _), (lx, ly, _)) = arm_rots("skeleton", &input);
        let quarter = -std::f32::consts::FRAC_PI_2;
        assert!((ry - -0.1).abs() < 1e-6, "right yaw {ry} != -0.1");
        assert!((ly - 0.5).abs() < 1e-6, "left yaw {ly} != 0.1 + 0.4");
        assert!((rx - quarter).abs() < 1e-6, "right pitch {rx} != -PI/2");
        assert!((lx - quarter).abs() < 1e-6, "left pitch {lx} != -PI/2");
    }

    /// The pose **tracks the head**, and it reads the *posed* head rather than
    /// re-deriving from the degrees input. A 30-degree look adds 30 degrees in
    /// radians to both arms' yaw and pitch.
    #[test]
    fn the_bow_pose_tracks_the_head() {
        let input = AnimInput {
            arm_pose: ArmPose::BowAndArrow,
            head_yaw_deg: 30.0,
            head_pitch_deg: -20.0,
            ..AnimInput::REST
        };
        let ((rx, ry, _), (_, ly, _)) = arm_rots("skeleton", &input);
        let yaw = 30.0 * DEG;
        let pitch = -20.0 * DEG;
        assert!((ry - (-0.1 + yaw)).abs() < 1e-5, "right yaw {ry}");
        assert!((ly - (0.5 + yaw)).abs() < 1e-5, "left yaw {ly}");
        assert!(
            (rx - (-std::f32::consts::FRAC_PI_2 + pitch)).abs() < 1e-5,
            "right pitch {rx}"
        );
    }

    /// The left-handed branch moves the splay to the **other** arm. This is the
    /// case a folded signed expression got wrong while still producing a
    /// bow-shaped pose, so it is asserted independently of the right-handed one.
    #[test]
    fn the_bow_pose_mirrors_for_the_off_hand() {
        let right = AnimInput {
            arm_pose: ArmPose::BowAndArrow,
            ..AnimInput::REST
        };
        let left = AnimInput {
            arm_pose_left_hand: true,
            ..right
        };
        let ((_, r_ry, _), (_, r_ly, _)) = arm_rots("skeleton", &right);
        let ((_, l_ry, _), (_, l_ly, _)) = arm_rots("skeleton", &left);
        // Holding right: the *left* arm splays (+0.4). Holding left: the *right*
        // arm splays (-0.4). Vanilla `HumanoidModel.java:353-357` vs `:398-402`.
        assert!((r_ry - -0.1).abs() < 1e-6, "holding right, right yaw {r_ry}");
        assert!((r_ly - 0.5).abs() < 1e-6, "holding right, left yaw {r_ly}");
        assert!((l_ry - -0.5).abs() < 1e-6, "holding left, right yaw {l_ry}");
        assert!((l_ly - 0.1).abs() < 1e-6, "holding left, left yaw {l_ly}");
        assert_ne!(
            (r_ry, r_ly),
            (l_ry, l_ly),
            "the two hands must not produce the same pose"
        );
    }

    /// `AnimationUtils.animateCrossbowCharge`: the holding arm is fixed and the
    /// pulling arm interpolates. Checks both endpoints against the vanilla
    /// literals *and* that the middle is strictly between them — a lerp with its
    /// arguments swapped passes an endpoint check on one end and fails this.
    #[test]
    fn the_crossbow_charge_pulls_one_arm_across_the_charge() {
        let at = |p: f32| {
            arm_rots(
                "skeleton",
                &AnimInput {
                    arm_pose: ArmPose::CrossbowCharge { progress: p },
                    ..AnimInput::REST
                },
            )
        };
        let ((rx0, ry0, _), (lx0, ly0, _)) = at(0.0);
        let (_, (lx1, ly1, _)) = at(1.0);
        let (_, (_, ly_mid, _)) = at(0.5);

        // Holding arm (right) is constant: `holdingArm.yRot = -0.8`,
        // `holdingArm.xRot = -0.97079635`.
        assert!((ry0 - -0.8).abs() < 1e-6, "holding yaw {ry0}");
        assert!((rx0 - -0.970_796_35).abs() < 1e-6, "holding pitch {rx0}");
        // Pulling arm (left) at alpha 0: yaw 0.4, pitch equal to the holding arm's.
        assert!((ly0 - 0.4).abs() < 1e-6, "pulling yaw at 0 = {ly0}");
        assert!((lx0 - -0.970_796_35).abs() < 1e-6, "pulling pitch at 0 = {lx0}");
        // ...and at alpha 1: yaw 0.85, pitch -PI/2.
        assert!((ly1 - 0.85).abs() < 1e-6, "pulling yaw at 1 = {ly1}");
        assert!(
            (lx1 - -std::f32::consts::FRAC_PI_2).abs() < 1e-6,
            "pulling pitch at 1 = {lx1}"
        );
        assert!(
            ly0 < ly_mid && ly_mid < ly1,
            "the pulling arm must sweep monotonically: {ly0} -> {ly_mid} -> {ly1}"
        );
        // Out-of-range progress clamps rather than extrapolating past the pose.
        let (_, (_, ly_over, _)) = at(4.0);
        assert!((ly_over - ly1).abs() < 1e-6, "progress > 1 must clamp");
    }

    /// `animateCrossbowHold` is a *different* pose from the charge — the holding
    /// arm tracks the head here and does not in the charge. Asserting they differ
    /// is what stops one being wired where the other belongs, which a still
    /// screenshot of a crossbow cannot distinguish.
    #[test]
    fn the_crossbow_hold_and_charge_are_different_poses() {
        let hold = arm_rots(
            "skeleton",
            &AnimInput {
                arm_pose: ArmPose::CrossbowHold,
                head_yaw_deg: 40.0,
                ..AnimInput::REST
            },
        );
        let charge = arm_rots(
            "skeleton",
            &AnimInput {
                arm_pose: ArmPose::CrossbowCharge { progress: 1.0 },
                head_yaw_deg: 40.0,
                ..AnimInput::REST
            },
        );
        assert_ne!(hold, charge, "hold and charge must not coincide");
        // The hold's holding arm carries the head yaw; the charge's does not.
        let yaw = 40.0 * DEG;
        assert!(
            ((hold.0).1 - (-0.3 + yaw)).abs() < 1e-5,
            "hold holding-arm yaw {} != -0.3 + head",
            (hold.0).1
        );
        assert!(
            ((charge.0).1 - -0.8).abs() < 1e-6,
            "charge holding-arm yaw {} must ignore the head",
            (charge.0).1
        );
    }

    /// The **control for every assertion above**: with `ArmPose::Empty` the arms
    /// are wherever the walk cycle put them, and every pose above must differ from
    /// that. Without this, a `pose_arms_for_item` that never ran would still let
    /// the constants above pass if they happened to match a rest pose.
    #[test]
    fn every_arm_pose_moves_the_arms_off_the_unposed_result() {
        let base = AnimInput {
            limb_swing: 4.0,
            limb_swing_amount: 1.0,
            ..AnimInput::REST
        };
        let empty = arm_rots("skeleton", &base);
        for pose in [
            ArmPose::BowAndArrow,
            ArmPose::CrossbowCharge { progress: 0.0 },
            ArmPose::CrossbowCharge { progress: 1.0 },
            ArmPose::CrossbowHold,
        ] {
            let posed = arm_rots(
                "skeleton",
                &AnimInput {
                    arm_pose: pose,
                    ..base
                },
            );
            assert_ne!(posed, empty, "{pose:?} left the arms unchanged");
        }
        assert!(
            base.arm_pose == ArmPose::Empty && !ArmPose::Empty.is_two_handed(),
            "Empty is the no-op pose and is not two-handed"
        );
    }

    /// A zombie rig **loses** the pose, because `animate_zombie_arms` assigns over
    /// both arms afterwards — vanilla's own behaviour
    /// (`AbstractZombieModel.setupAnim` calls `super.setupAnim` then
    /// `animateZombieArms` unconditionally). Asserted rather than left implicit
    /// because it looks exactly like the pose failing to wire up, and the next
    /// person to see a bow-holding zombie with forward arms needs this test to
    /// tell them it is correct.
    #[test]
    fn a_zombie_rig_overwrites_the_item_pose_as_vanilla_does() {
        let base = AnimInput::REST;
        let bow = AnimInput {
            arm_pose: ArmPose::BowAndArrow,
            ..base
        };
        assert_eq!(
            arm_rots("zombie", &bow),
            arm_rots("zombie", &base),
            "a zombie rig must be unaffected by the item pose"
        );
        // The same pose on a `Swinging` rig *does* land — otherwise this test
        // would pass on a completely dead `pose_arms_for_item`.
        assert_ne!(
            arm_rots("skeleton", &bow),
            arm_rots("skeleton", &base),
            "control: the skeleton rig must still take the pose"
        );
    }

    /// [`HandPoseOverride::Structural`] must be the same value `pose()` already
    /// puts at the arm's index — it exists so a caller can ask for "no
    /// override" uniformly, not to compute anything different.
    #[test]
    fn structural_override_is_bit_for_bit_the_composed_arm_matrix() {
        let skel = skeleton_for("zombie");
        let input = AnimInput {
            limb_swing: 4.0,
            limb_swing_amount: 1.0,
            ..AnimInput::REST
        };
        let composed = skel.pose(&input);
        for (left, name) in [(false, "right_arm"), (true, "left_arm")] {
            let i = skel.index_of(name).unwrap();
            let got = skel
                .translate_to_hand(&input, left, HandPoseOverride::Structural)
                .unwrap();
            assert_eq!(
                got, composed[i],
                "{name}: structural override drifted from pose()"
            );
        }
    }

    /// `skeleton`/`stray`/`wither_skeleton` and `player_slim` shift the arm's
    /// pivot `x` *before* its own rotation, then restore it — vanilla's
    /// `part.x += offset; part.translateAndRotate(); part.x -= offset;`.
    ///
    /// # Why the expected value here is not just this crate's own formula read back
    ///
    /// Because translations commute with each other
    /// (`T(a)·T(b) = T(a+b) = T(b)·T(a)`), and both `skeleton` and `player_slim`
    /// have an **identity** root pose (`PartPose::ZERO`, so their arm's parent
    /// chain contributes no rotation), the whole "shift the pivot, rotate,
    /// restore" dance algebraically collapses to one pure world-space
    /// translation left-multiplied onto the ordinary structural pose:
    ///
    /// ```text
    ///   shifted = parent · T(pivot + shift) · R(rot)
    ///           = parent · T(shift) · T(pivot) · R(rot)      (T commutes with T)
    ///           = [parent · T(shift) · parent⁻¹] · [parent · T(pivot) · R(rot)]
    ///           = T(shift)                        · structural   (parent is a
    ///                                                 pure translation, so it
    ///                                                 conjugates a translation
    ///                                                 to itself)
    /// ```
    ///
    /// That derivation is independent of `translate_to_hand`'s own code path —
    /// it only assumes what a pivot shift *means* geometrically — so agreement
    /// is real evidence the implementation does the shift in the right place
    /// (between the parent chain and the arm's rotation), not just evidence it
    /// produces plausible-looking numbers.
    #[test]
    fn pivot_shift_is_a_pure_world_space_translation_of_the_structural_pose() {
        for (model, texels) in [("skeleton", 1.0f32), ("player_slim", 0.5)] {
            let skel = skeleton_for(model);
            // A walk swing gives the arm a real rotation, so the shift has to
            // interact with something other than the identity — the case that
            // would be silently wrong if the shift were applied after
            // composition instead of before the rotation.
            let input = AnimInput {
                limb_swing: 12.5,
                limb_swing_amount: 1.0,
                ..AnimInput::REST
            };
            for (left, sign) in [(false, 1.0f32), (true, -1.0f32)] {
                let structural = skel
                    .translate_to_hand(&input, left, HandPoseOverride::Structural)
                    .unwrap();
                let shifted = skel
                    .translate_to_hand(&input, left, HandPoseOverride::PivotShiftTexels(texels))
                    .unwrap();
                let expected =
                    Mat4::from_translation(Vec3::new(sign * texels / 16.0, 0.0, 0.0)) * structural;
                for (a, b) in shifted
                    .to_cols_array()
                    .iter()
                    .zip(expected.to_cols_array().iter())
                {
                    assert!(
                        (a - b).abs() < 1e-4,
                        "{model} {}: shifted {shifted:?} vs derived {expected:?}",
                        if left { "left" } else { "right" }
                    );
                }
            }
        }
    }

    /// `VexModel.translateToHand`: `root · body · arm`, then `scale(0.55)`,
    /// then a small arm-signed translate. Vex's arm rig never rotates in this
    /// port (`AnimFamily::HeadOnly` runs no arm `setupAnim`, and the rest pose
    /// authors no rotation either), so every step in the chain is a pure
    /// translation or a uniform scale — cheap to hand-compute independently
    /// and compare, with nothing shared with `translate_to_hand`'s own code.
    #[test]
    fn vex_hand_transform_transcribes_root_body_arm_scale_then_translate() {
        let skel = skeleton_for("vex");
        let body = skel.index_of("body").unwrap();
        let world_body = skel.pose(&AnimInput::REST)[body];
        // The arm's own baked pivot (`-1.75` for `right_arm`, `+1.75` for
        // `left_arm` — see `vex_model` in `lodestone-assets`) is independent of
        // vanilla's `mainArm` sign on the trailing translate, so the two are
        // named separately rather than reused from one `sign`.
        for (left, arm_x, post_sign, arm_name) in [
            (false, -1.75f32, 1.0f32, "right_arm"),
            (true, 1.75, -1.0, "left_arm"),
        ] {
            let arm_offset = Vec3::new(arm_x, 0.25, 0.0);
            let expected = world_body
                * Mat4::from_translation(arm_offset / 16.0)
                * Mat4::from_scale(Vec3::splat(0.55))
                * Mat4::from_translation(Vec3::new(post_sign * 0.046875, -0.15625, 0.078125));
            let got = skel
                .translate_to_hand(&AnimInput::REST, left, HandPoseOverride::Vex)
                .unwrap();
            for (a, b) in got
                .to_cols_array()
                .iter()
                .zip(expected.to_cols_array().iter())
            {
                assert!(
                    (a - b).abs() < 1e-4,
                    "vex {arm_name}: {got:?} vs {expected:?}"
                );
            }
        }
    }

    /// `AllayModel.translateToHand` never calls `getArm(arm)` at all: it reads
    /// `right_arm.xRot` and never mirrors by handedness (not even the
    /// translate's sign) — so a main-hand and an off-hand item pose the same.
    /// This is read from source (see [`HandPoseOverride::Allay`]'s doc
    /// comment), not assumed, and this test pins both halves of that claim:
    /// the transcribed chain, and the left/right symmetry.
    #[test]
    fn allay_hand_transform_transcribes_its_chain_and_ignores_which_arm() {
        let skel = skeleton_for("allay");
        let body = skel.index_of("body").unwrap();
        let world_body = skel.pose(&AnimInput::REST)[body];
        // Allay's arms never rotate in this port either (`HeadOnly` family, no
        // arm `setupAnim`, zero authored rotation), so `right_arm.xRot` is
        // exactly `0.0` here and `Rx(0)` is the identity — independently
        // computable without reading any of this crate's own posed state.
        let expected = world_body
            * Mat4::from_translation(Vec3::new(0.0, 1.0 / 16.0, 3.0 / 16.0))
            * Mat4::from_rotation_x(0.0)
            * Mat4::from_scale(Vec3::splat(0.7))
            * Mat4::from_translation(Vec3::new(1.0 / 16.0, 0.0, 0.0));
        let right = skel
            .translate_to_hand(&AnimInput::REST, false, HandPoseOverride::Allay)
            .unwrap();
        let left = skel
            .translate_to_hand(&AnimInput::REST, true, HandPoseOverride::Allay)
            .unwrap();
        assert_eq!(right, left, "allay must not mirror by arm at all");
        for (a, b) in right
            .to_cols_array()
            .iter()
            .zip(expected.to_cols_array().iter())
        {
            assert!((a - b).abs() < 1e-4, "allay: {right:?} vs {expected:?}");
        }
    }

    /// None of the four override kinds may flip handedness: every op involved
    /// (translation, a proper rotation, a uniform positive scale) has a
    /// positive determinant, so the sign a real camera would see must survive
    /// unchanged from the plain structural chain. This is the same invariant
    /// `entity::the_held_item_pose_preserves_winding_for_a_real_mob` pins one
    /// layer up, checked here at the source of the four new code paths rather
    /// than only downstream of them.
    #[test]
    fn no_override_flips_the_structural_determinant_sign() {
        let cases: [(&str, HandPoseOverride); 4] = [
            ("skeleton", HandPoseOverride::PivotShiftTexels(1.0)),
            ("player_slim", HandPoseOverride::PivotShiftTexels(0.5)),
            ("vex", HandPoseOverride::Vex),
            ("allay", HandPoseOverride::Allay),
        ];
        for (model, override_) in cases {
            let skel = skeleton_for(model);
            let structural_sign = skel
                .translate_to_hand(&AnimInput::REST, false, HandPoseOverride::Structural)
                .unwrap()
                .determinant()
                .signum();
            for left in [false, true] {
                let got = skel
                    .translate_to_hand(&AnimInput::REST, left, override_)
                    .unwrap();
                assert_eq!(
                    got.determinant().signum(),
                    structural_sign,
                    "{model} {}: override flipped handedness (det = {})",
                    if left { "left" } else { "right" },
                    got.determinant()
                );
            }
        }
    }
}
