//! Per-part entity animation: head tracking, walk cycles and attack swings.
//!
//! [`entity`](crate::entity) places a whole mob with one matrix, which is
//! correct for a statue and wrong for anything alive. Vanilla animates by
//! adjusting each model part's [`PartPose`] before rendering, per model class,
//! then walking the part hierarchy composing transforms. This module does the
//! same thing over [`BakedPart`]s: it copies the rest poses, applies a family's
//! pose-setup step, and composes the chain into one matrix per part.
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
//! One pose-setup **override** is ported rather than just the base families:
//! the zombie family's raised arms ([`HumanoidArms::Zombie`]). It is not a
//! state gap — the pose is unconditional, so leaving it out drew every zombie,
//! husk and drowned with its arms hanging at its sides.
//!
//! # One thing here is not a pose-setup step at all
//!
//! A creeper's pre-detonation swell ([`creeper_swell_scale`],
//! [`Skeleton::pose_swelling`]) is a **whole-model scale**, not a joint rotation.
//! In 26.2 it is applied by the renderer as a transform-stack op wrapped around
//! the model, separately from the model's own pose-setup step, which knows
//! nothing about it. It is implemented here anyway because this is the module
//! that owns the part matrices, and folding a scale into them is how a caller
//! that only knows about [`Skeleton`] can get it.
//!
//! Trigonometry here uses `f32::sin`/`cos` rather than vanilla's 65536-entry
//! quantised lookup table. That is a deliberate exception to the project's
//! bit-exactness rule: limb angles are never sent to a server and never feed
//! physics, so a sub-degree difference is invisible and unobservable. Anything
//! that *is* transmitted must still use the parity table.

use glam::{Mat4, Vec3};
use lodestone_assets::entity::{Affine, BakedPart, PartPose};

/// Radians per degree.
const DEG: f32 = std::f32::consts::PI / 180.0;
/// Vanilla's walk-cycle frequency multiplier, applied to the accumulated
/// walk-distance counter that drives limb phase.
const WALK_FREQ: f32 = 0.6662;

/// Which arm rig a [`AnimFamily::Humanoid`] model animates with.
///
/// Vanilla expresses this by subclassing: the base humanoid pose-setup swings
/// the arms with the walk cycle, and the zombie family's pose-setup calls that
/// base behaviour and then **overwrites** both arms with a separate, shared
/// raised-arms routine. A zombie's part hierarchy is identical to a player's,
/// so — unlike [`AnimFamily`] — this cannot be classified structurally; the
/// caller supplies it from the model name (see
/// [`humanoid_arms_for`](crate::entity::humanoid_arms_for)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HumanoidArms {
    /// The base humanoid pose: arms swing opposite the legs, plus the idle bob.
    #[default]
    Swinging,
    /// The zombie family's pose: both arms held out in front, walk swing discarded.
    Zombie,
}

/// Which of vanilla's per-model overrides of the base humanoid's
/// hand-attachment-point transform applies. Selected by the caller from the model name (see
/// [`hand_pose_override_for`](crate::entity::hand_pose_override_for)), the same
/// pattern [`HumanoidArms`] uses and for the same reason: a model class is not
/// visible to us, only the parts it declares and the name it was ported under.
///
/// # Why this cannot be a correction applied to `part_transforms[arm]`
///
/// Every one of these overrides is scoped to the hand-attachment point alone —
/// the arm's *own* mesh keeps rendering through its unmodified pivot; only the
/// point a held item hangs from moves. `part_transforms[arm]` is the matrix the
/// whole-body instanced draw uses to place the arm's visible geometry
/// ([`crate::entity::plan_entities`]), so folding an override in there would
/// nudge the mob's visible forearm by the same amount it nudges the item — a
/// new, real defect traded for the one being fixed. [`Skeleton::translate_to_hand`]
/// therefore computes a *separate* matrix, never touching `part_transforms`.
///
/// The two pivot-shift cases below additionally cannot be expressed as a pre-
/// or post-multiplication of the arm's *already-composed* matrix: vanilla
/// shifts the arm's own pivot **before** its rotation is applied (offsetting
/// the pivot, composing the transform, then undoing the offset), and
/// `T(pivot) · R(rot)` does not commute, so the shift has to be folded in
/// while the pivot and the rotation are still two separate values — i.e. from
/// the posed [`PartPose`], not from the [`Mat4`] that already fused them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum HandPoseOverride {
    /// Most humanoid-family and armour-stand rigs, plus a golem whose arms hang
    /// off the body part rather than the root: `root · [body ·] arm` — exactly
    /// the ordinary composed chain, so this is the same value
    /// `part_transforms[arm]` already holds; the composed chain already
    /// includes the whole parent hierarchy either way.
    #[default]
    Structural,
    /// The skeleton family: the arm's pivot `x` is shifted by `±1.0` texel
    /// before its own rotation (`+1` for the right arm, `-1` for the left).
    /// The slim player variant uses the identical shift at `±0.5` texels. The
    /// `f32` is the magnitude in texels; the sign is derived from the arm at
    /// call time.
    PivotShiftTexels(f32),
    /// The vex rig: `root · body · arm`, then `scale(0.55)`, then
    /// `translate(±0.046875, -0.15625, 0.078125)` (sign by arm). Its arms hang
    /// off `body`, not `root`, which the ordinary chain already handles — the
    /// override is the trailing scale-and-translate vanilla adds *after* the
    /// arm's own transform.
    Vex,
    /// The allay rig: a wholly different chain that never composes the arm's
    /// own rotation into the chain at all — `root · body`, then
    /// `T(0, 1/16, 3/16) · Rx(right_arm.xRot) · S(0.7) · T(1/16, 0, 0)`.
    /// Vanilla does not branch on which arm anywhere in this override, not
    /// even the translate's sign, so an off-hand item on an allay is posed
    /// identically to a main-hand one — read from the decompiled source, not
    /// a guess.
    Allay,
}

/// Vanilla's shared raised-arms routine's resting arm elevation,
/// `-PI / (isAggressive ? 1.5 : 2.25)` radians about X. Negative raises the arm
/// forward in the Y-down model frame, which is the "arms out in front" pose.
#[must_use]
fn zombie_arm_x_rest(aggressive: bool) -> f32 {
    -std::f32::consts::PI / if aggressive { 1.5 } else { 2.25 }
}

/// The same routine's resting arm splay: the right arm's yaw at `-0.1`,
/// the left arm's at `0.1`, when not mid-swing.
const ZOMBIE_ARM_Y_REST: f32 = 0.1;

/// The largest value vanilla's creeper-swelling accessor can return: `30 / (30 - 2)`.
///
/// Vanilla computes a linear interpolation between the previous and current
/// swell over the frame, divided by `maxSwell - 2`, with `swell` capped at
/// `maxSwell` and `maxSwell` defaulting to `30`. The
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
/// doc comment warns about, in a file outside this module's ownership;
/// widening the creeper's local bounds by this factor is the fix.
///
/// [`EntityMesh::local_min`]: crate::entity::EntityMesh::local_min
pub const MAX_SWELL_SCALE: f32 = 1.4 * (1.0 + MAX_SWELL * 0.01);

/// Vanilla's per-axis model scale applied to a creeper for a given `swelling`
/// fraction, as `[x, y, z]`.
///
/// Transcribed from the decompiled 26.2 client:
///
/// ```text
///   g = swelling
///   wobble = 1.0 + sin(g * 100.0) * g * 0.01
///   g = clamp(g, 0.0, 1.0)
///   g *= g
///   g *= g
///   s  = (1.0 + g * 0.4) * wobble
///   hs = (1.0 + g * 0.1) / wobble
///   scale(s, hs, s)
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
/// The sine is `f32::sin` rather than vanilla's quantised lookup table, per
/// the module note: this feeds a scale, never the wire.
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

/// How far a dead entity ends up rotated, in vanilla's base rendering rule.
///
/// The base rule returns a flat `90.0F` — a mob lies flat on its
/// side — and this is a named constant rather than an inline 90 so the per-model
/// overrides (a spider's rig, notably) have somewhere to hang off later.
pub const FLIP_DEGREES: f32 = 90.0;

/// Vanilla's death fall-over, in **degrees** about the
/// entity's Z axis, for a `death_time` equal to vanilla's integer death timer
/// plus the frame's interpolation fraction, in ticks.
///
/// Transcribed from the decompiled 26.2 client:
///
/// ```text
///   if (death_time > 0.0) {
///      fall = (death_time - 1.0) / 20.0 * 1.6;
///      fall = sqrt(fall);
///      if (fall > 1.0) {
///         fall = 1.0;
///      }
///      rotate_z(fall * FLIP_DEGREES);
///   }
/// ```
///
/// # It is not linear in `death_time`, and "90° over 20 ticks" is the wrong answer
///
/// Two terms make the plausible reading wrong in opposite directions, and they
/// nearly cancel at `death_time == 20`, which is exactly the input someone would
/// reach for to check it:
///
/// * The `sqrt` front-loads the motion. The mob is already **halfway over by tick
///   4** (`sqrt(3/20 · 1.6) = 0.49`, 44°) where a linear ramp would have it at 18°.
///   That is the whole character of the animation — a body dropping and settling,
///   not a hand sweeping round a dial.
/// * The `1.6` factor drives the argument past 1 before the count does, so `fall`
///   saturates at `death_time == 13.5`, not 20. The mob is flat on its side for the
///   last ~6.5 ticks of vanilla's 20-tick death, which is what makes it *lie there*
///   before the server removes it.
///
/// At `death_time == 20` both readings give 90°, so a gate written there measures
/// only that the function runs. The discriminating ticks are the early ones and
/// anything in `1 < t < 13.5`.
///
/// # `death_time == 0` is alive, and the subtraction is why zero is returned twice
///
/// The vanilla check gates on `death_time > 0.0`, and `death_time == 1` independently
/// makes the expression exactly `0`. Both are returned as `0.0` here — but the
/// guard is load-bearing rather than redundant: without it `death_time == 0` would
/// evaluate `sqrt(-0.08)`, and **`f32::sqrt` of a negative is `NaN`**, which
/// propagates silently through a rotation matrix into vertices that vanish. A
/// living entity is the common case, so that NaN would be the *default* path.
///
/// `f32::sqrt` rather than vanilla's quantised lookup table, per the module note:
/// this feeds a rotation, never the wire.
#[must_use]
pub fn death_fall_over_degrees(death_time: f32) -> f32 {
    if death_time <= 0.0 {
        return 0.0;
    }
    let fall = ((death_time - 1.0) / 20.0 * 1.6).max(0.0).sqrt();
    fall.min(1.0) * FLIP_DEGREES
}

/// Vanilla's white-flash overlay strength for a given swell, `0.0..=1.0`.
///
/// Transcribed from the decompiled 26.2 client:
///
/// ```text
///   step = swelling;
///   return (int)(step * 10.0) % 2 == 0 ? 0.0 : clamp(step, 0.5, 1.0);
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
/// This returns vanilla's white-overlay progress, the same value its
/// overlay-texture lookup takes alongside the separate hurt/death red
/// overlay flag — it is **not** the blend alpha yet. The overlay texture's
/// white row quantises `progress` to `u = floor(progress * 15)` and derives
/// alpha as `(1 - u/15 * 0.75) * 255`; see
/// [`crate::entity_pipeline::creeper_overlay_alpha_from_progress`] for that
/// second step, which a caller applies only when the hurt/death red overlay
/// is *not* also active — vanilla's overlay texture has red and white on
/// mutually exclusive rows (the red row ignores the white quantisation
/// entirely), so red always wins when both are true.
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
/// Vanilla's base entity-rendering rule orders the ops:
///
/// ```text
///   scale(-1, -1, 1)          // into the Y-down model frame
///   apply_swell_scale(state)  // the creeper's swell
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

/// Which of vanilla's per-model pose-setup implementations a model animates with.
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
    /// Four legs in hind/front pairs.
    Quadruped,
    /// Eight legs in hind/middle-hind/middle-front/front pairs.
    Spider,
    /// Arms and legs.
    Humanoid,
    /// Fused `arms` part plus legs.
    Villager,
    /// Wings plus legs.
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

/// A boat or minecart's rocking state, interpolated for this frame — vanilla's
/// per-frame hurt-clock, hurt-direction and damage-amplitude trio.
///
/// Lives on [`AnimInput`] rather than on the draw record for the same reason
/// every other render-state scalar here does: it is per-entity, per-frame state
/// the renderer reads, and it is the one struct already threaded from the
/// extract step to every placement.
///
/// It affects the **placement**, not the skeleton pose — [`Skeleton::pose`]
/// ignores it entirely. The consumer is the boat placement in
/// `lodestone_shell::gpu::entity_passes`, which conjugates the roll into the
/// already-baked matrices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoatHurt {
    /// Vanilla's interpolated hurt clock — counts down from `10`. Zero or below is
    /// "not hurt", and the boat draws level.
    pub time: f32,
    /// Vanilla's hurt direction — `+1` or `-1`. It **multiplies** the whole roll, so a
    /// `0.0` here silences the animation completely; vanilla's registered
    /// default is `1`, which is why [`Self::REST`] uses that and not zero.
    pub dir: f32,
    /// Vanilla's interpolated accumulated damage, floored at `0` — accumulated
    /// damage x 10, the amplitude of the roll.
    pub damage: f32,
}

impl BoatHurt {
    /// A vehicle that has never been hit: no clock, no damage, and vanilla's own
    /// registered `+1` direction.
    pub const REST: BoatHurt = BoatHurt {
        time: 0.0,
        dir: 1.0,
        damage: 0.0,
    };
}

impl Default for BoatHurt {
    fn default() -> Self {
        Self::REST
    }
}

/// Vanilla's boat/minecart hull roll, in degrees:
/// `sin(time) * time * damage / 10 * dir`, about the model's
/// local X axis, and exactly `0.0` while the hurt clock is not running.
///
/// Three things about the formula are easy to get wrong and each is visible:
///
/// * **`sin` takes the hurt clock in radians, not degrees.** Vanilla passes
///   the hurt clock straight into its quantised sine lookup with no unit
///   conversion, so the boat swings through rather
///   more than one full period over the ten ticks — that oscillation *is* the
///   animation. Converting to degrees first gives a monotonic lean that never
///   comes back.
/// * **The hurt-clock factor appears twice**, once inside the sine and once as a
///   linear multiplier, so the swing decays as the clock runs out instead of
///   ending abruptly.
/// * **The direction multiplies the result**, so an unreported direction of `0`
///   silences the whole thing. See [`BoatHurt::dir`].
///
/// Vanilla's sine is a quantised lookup table rather than the library sine;
/// this uses [`lodestone_physics::mth::sin`] for that reason, since the
/// difference is real at the poles and this argument sweeps a whole period.
#[must_use]
pub fn boat_hurt_roll_degrees(hurt: BoatHurt) -> f32 {
    if hurt.time <= 0.0 {
        return 0.0;
    }
    lodestone_physics::mth::sin(f64::from(hurt.time)) * hurt.time * hurt.damage / 10.0 * hurt.dir
}

/// The per-entity animation state a [`Skeleton`] poses from, already
/// interpolated for the frame.
///
/// This mirrors the subset of vanilla's per-entity render state that we track;
/// it is deliberately a plain value type so posing is a pure function and can be
/// unit-tested without a GPU, a world or a clock.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AnimInput {
    /// Head yaw **relative to the body**, in degrees, matching vanilla's own
    /// per-frame value of the same meaning.
    pub head_yaw_deg: f32,
    /// Head pitch in degrees, positive looking down, matching vanilla's own
    /// per-frame value of the same meaning.
    pub head_pitch_deg: f32,
    /// Accumulated walk-cycle phase, matching vanilla's own counter of the
    /// same meaning.
    pub limb_swing: f32,
    /// Walk-cycle amplitude in `0..=1`, matching vanilla's own counter of the
    /// same meaning.
    pub limb_swing_amount: f32,
    /// Attack-swing progress in `0..=1`; `0` means not swinging, matching
    /// vanilla's own counter of the same meaning.
    pub attack_anim: f32,
    /// Continuous age in ticks, driving idle bob, matching vanilla's own
    /// counter of the same meaning.
    pub age_ticks: f32,
    /// Vanilla's aggressive-mob flag, which raises a zombie's arms from
    /// `-PI/2.25` to `-PI/1.5`.
    ///
    /// It rides bit `0x04` of a mob's own metadata flags byte — a *separate* byte from
    /// the shared entity flags, at its own metadata index (15 in 26.2, confirmed
    /// against a jar dump rather than counted). An earlier note here called it a
    /// shared-flags bit; it is not, and looking for it at index 0 would find the
    /// unrelated unused bit there.
    ///
    /// **This used to be hardcoded `false` at every call site**, which made both
    /// consumers of it dead code: the zombie arm lift here, and (once the pose
    /// machinery landed) the skeleton bow draw, which vanilla selects from this
    /// flag and *not* from the using-item bit. The byte is now decoded;
    /// `lodestone_shell::entities::render_anim` now feeds it from a `MobState`
    /// component. A `false` here still means exactly what it says — the pose an
    /// idle or merely-walking mob is in.
    pub aggressive: bool,
    /// How the arms are held for the item in use, if any.
    ///
    /// Vanilla's own arm-pose enumeration, reduced to the poses this build
    /// actually draws — see [`ArmPose`].
    pub arm_pose: ArmPose,
    /// Which hand holds the item [`arm_pose`](Self::arm_pose) describes.
    ///
    /// `false` is the main hand, which for every rig we draw is the right arm.
    /// Vanilla threads this as a shared "holding in right arm" helper and as
    /// the base humanoid pose-setup's main-hand-used-equals-right-handed fork;
    /// it decides which arm *holds* and which arm *pulls*, so getting it wrong
    /// mirrors the pose rather than breaking it — a bow drawn with the wrong
    /// arm still looks like a bow draw, which is why it is a named field
    /// rather than an assumption.
    pub arm_pose_left_hand: bool,
    /// Vanilla's crouching render flag, derived from whether the entity
    /// currently holds the crouching pose.
    ///
    /// **Not the shift-key flag.** The raw "shift key down" bit is a separate
    /// shared-flags bit `0x02`, and the crouching pose is a value at metadata
    /// index 6; holding shift while a standing box does not fit gives you the
    /// former without the latter. Both are decoded in this workspace, so a
    /// consumer that reaches for the flag because it is closer to hand is
    /// choosing the wrong one.
    ///
    /// Drives [`Skeleton::pose`]'s [`AnimFamily::Humanoid`] crouch branch — the
    /// forward body pitch, the lowered head, and the legs stepping back — which
    /// vanilla applies *after* the attack swing and *before* the idle arm bob.
    pub crouching: bool,
    /// Vanilla's own passenger flag — riding any vehicle (boat, minecart,
    /// horse, …), not a value of the entity's pose enumeration.
    /// There is no sitting pose for a mounted rider: vanilla's pose-update
    /// logic has no riding case, so this has to be threaded in as its own bit
    /// rather than read off [`Self::crouching`]'s pose accessor.
    ///
    /// Drives [`Skeleton::pose`]'s [`AnimFamily::Humanoid`] passenger branch —
    /// the folded knees and the arms dropped to rest on them — which vanilla
    /// applies *before* the item-pose block, so a using-item passenger's arms
    /// still take the item pose on top rather than being pinned to the sit
    /// rotation.
    pub is_passenger: bool,
    /// Vanilla's own per-frame swim-amount value, interpolated for this frame
    /// — a `0..1` ramp toward the swim pose, `0.0` for every entity that has
    /// never reported the swimming pose. Mirrors
    /// [`crate::entity_anim::AnimInput::crouching`]'s shape: a wire this build
    /// already carries end to end (`crate::entities::SwimRamp`/`tick_swim_ramp`
    /// on the shell side), just not into this struct until now.
    ///
    /// Drives [`Skeleton::pose`]'s [`AnimFamily::Humanoid`] swim branch —
    /// the base humanoid pose-setup's two swim-amount-positive clauses: the head
    /// pitching toward `-PI/4` and the arm-over-arm stroke plus leg flutter.
    /// This is the **limb-level** swim animation and is a different thing from
    /// the whole-body prone rotation `apply_swim_rotation` in
    /// `lodestone_shell::gpu::entity_passes` already applies — that ports
    /// vanilla's player-only body-pitch rule, gated on
    /// `type_path` at its call site; this field's branch is the base
    /// humanoid pose-setup clause and applies to every [`AnimFamily::Humanoid`]
    /// rig with nonzero `swim_amount`, remote mobs included, with no type gate
    /// of its own — vanilla runs it unconditionally for every renderer whose
    /// model is a humanoid rig.
    pub swim_amount: f32,
    /// An armour stand's six part rotations, in degrees — `Some` for **every**
    /// armour stand, `None` for everything else.
    ///
    /// # This is not "an optional extra pose", it is the whole animation
    ///
    /// Vanilla's armour-stand pose-setup calls the base humanoid pose-setup
    /// — head tracking, walk cycle, crouch, item pose, attack
    /// swing, idle bob — and then **assigns** `head`, `body`, both arms and both
    /// legs from these six values. Everything the base pass computed for those
    /// joints is discarded. [`Skeleton::setup_anim`] does the same, in the same
    /// order, for exactly the same reason: it is what stops an armour stand
    /// animating like a walking humanoid.
    ///
    /// So `None` here does not mean "leave the stand in a neutral pose" — it
    /// means "this is not an armour stand". A caller that passes `None` for a
    /// stand it has no pose data for gets the walk cycle, and a stand carried
    /// along by a moving contraption then swings its arms like a running
    /// player, with any held item swinging off the same arm. The honest default
    /// for a stand nobody has posed is
    /// [`ArmorStandPose::VANILLA_DEFAULT`](lodestone_model::ArmorStandPose::VANILLA_DEFAULT),
    /// which is the pose vanilla's own metadata defaults register — arms and
    /// legs slightly splayed, not zeroed.
    ///
    /// # Why the walk cycle is computed and then discarded rather than skipped
    ///
    /// Skipping it — an entity-type gate on the family, or a
    /// [`HumanoidArms`] variant — would be cheaper and would look tidier, and it
    /// is not what vanilla does. The two differ in a way that shows: the base
    /// pass writes part *translations* as well as rotations (the crouch's `y`
    /// offsets, the attack swing's arm orbit and body twist), and the assignment
    /// covers **rotations only**, so those translations survive in vanilla and
    /// would vanish under a gate. Matching the overwrite keeps this rig's
    /// behaviour derived from the source rather than from an argument about what
    /// ought to be equivalent — see [`HumanoidArms::Zombie`] for the one place
    /// this crate does take the skip, where the discarded terms provably have no
    /// such residue.
    pub armor_stand_pose: Option<lodestone_model::ArmorStandPose>,
    /// A vehicle's rocking state (vanilla's hurt triple), [`BoatHurt::REST`]
    /// for every entity that is not a boat, raft or minecart. Read by the boat
    /// placement, never by [`Skeleton::pose`] — see [`BoatHurt`].
    pub boat_hurt: BoatHurt,
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
        crouching: false,
        is_passenger: false,
        swim_amount: 0.0,
        armor_stand_pose: None,
        boat_hurt: BoatHurt::REST,
    };
}

/// How a humanoid rig holds its arms for the item it is using — vanilla's
/// own humanoid arm-pose enum, reduced to the cases this build draws.
///
/// # What is modelled, and what a variant means
///
/// Only the two-handed *ranged* poses are modelled.
/// `Empty` is "leave the arms wherever the walk cycle and the attack
/// swing put them" and is what every other item still gets — including a held
/// sword, which vanilla poses with its own one-handed item pose (a pitch term
/// scaled by `0.5` and offset by `-PI/10`). That is a real
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
    /// Holding an item (vanilla's item-hold pose) — the arm lifts by `PI/10` and its own
    /// swing is halved.
    ///
    /// **This is vanilla's pose for eating and drinking.** Vanilla's arm-pose
    /// enum has no separate eat/drink variant, and its arm-pose selection logic
    /// deliberately
    /// omits the eating/drinking item-use states from its `if` chain, so a consuming
    /// entity falls through to the item-hold pose. All of vanilla's distinctive eating motion —
    /// the dip and the jitter toward the mouth — is *first-person only*, in
    /// vanilla's first-person eat-transform function; in third person another player
    /// eating is an entity with a raised arm and crumbs coming off it. Looking for
    /// a third-person eating animation and not finding one is the expected outcome,
    /// not a gap.
    ///
    /// # The first one-handed pose here
    ///
    /// Vanilla's item-hold pose is neither two-handed nor
    /// flagged as affecting the off-hand pose — so vanilla poses each arm from *its own* pose and
    /// this one touches only the arm actually holding the item.
    /// [`Skeleton::pose_arms_for_item`] therefore writes one arm for this variant
    /// and two for every other, which is why `arm_pose_left_hand` is load-bearing
    /// here in a way it is not for the ranged poses (where a wrong value merely
    /// mirrors a symmetric result).
    Item,
    /// Drawing a bow (vanilla's bow-and-arrow pose). Both arms come up in front,
    /// tracking the head.
    BowAndArrow,
    /// Winding a crossbow (vanilla's crossbow-charge pose). The pulling arm rotates
    /// further as the charge advances.
    CrossbowCharge {
        /// Charge fraction in `0..=1`, vanilla's
        /// `clamp(ticksUsingItem, 0, maxCrossbowChargeDuration) /
        /// maxCrossbowChargeDuration`.
        ///
        /// The **fraction** rather than `(ticks, duration)` on purpose: the
        /// duration is vanilla's crossbow charge-duration function, which is
        /// `25 - 5 * QuickCharge level`, and resolving an enchantment level needs
        /// the enchantment registry. A caller with no enchantment data supplies
        /// `ticks / 25.0`, which is exact for an unenchanted crossbow and merely
        /// slow for an enchanted one; keeping that decision at the caller means
        /// this function cannot silently assume level 0.
        progress: f32,
    },
    /// Holding an already-charged crossbow (vanilla's crossbow-hold pose), which is
    /// **not** an in-use pose: vanilla shows it whenever a charged crossbow is
    /// held and the entity is not swinging, driven by the item's
    /// `minecraft:charged_projectiles` component rather than by the using-item
    /// bit.
    CrossbowHold,
}

impl ArmPose {
    /// Whether this pose occupies **both** arms, so the off hand's own pose is
    /// suppressed (vanilla's two-handed arm-pose flag).
    ///
    /// Every pose modelled here is two-handed, which is not a coincidence — the
    /// ranged poses are exactly the ones that need a second arm — but it is
    /// asserted rather than assumed because the one-handed poses listed in the
    /// type docs will land here later.
    #[must_use]
    pub const fn is_two_handed(self) -> bool {
        match self {
            // `ITEM(false, false)` — one-handed, and the first such pose here.
            ArmPose::Empty | ArmPose::Item => false,
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
    /// The three armour-stand-only parts vanilla's armour-stand pose setup drives
    /// from the *body* pose alongside `body` itself. No other model in the
    /// corpus declares them, so they resolve to `None` everywhere else and cost
    /// nothing.
    right_body_stick: Option<usize>,
    left_body_stick: Option<usize>,
    shoulder_stick: Option<usize>,
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
            right_body_stick: find("right_body_stick"),
            left_body_stick: find("left_body_stick"),
            shoulder_stick: find("shoulder_stick"),
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
    /// Why the rest pose and not just `setup_anim`: vanilla's zombie-arms animation assigns
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
    /// `swell` is vanilla's creeper swell-fraction accessor: `0.0` while the
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
    /// investigation once already): the creeper's swell-direction metadata
    /// index, decoded as `CreeperSwellDir`, reaches
    /// `ingest.rs`, folds into per-entity state,
    /// crosses the shell boundary, and lands on
    /// `entities.rs`' `CreeperFuse` component,
    /// which its fuse-tick integrates every tick —
    /// `swell += swell_dir` — exactly vanilla's client-side swell-fraction
    /// accumulation. `extract_entity_draws`
    /// reads that counter and is the caller that threads it into this
    /// method. `docs/entity-rendering.md` records the chain as closed.
    ///
    /// The "divide by 28" this method's own [`MAX_SWELL`] encodes is not a
    /// second, conflicting constant next to "vanilla's max-swell tick count is
    /// 30" — they are the
    /// same fact: vanilla's max-swell tick count is 30, and the client divides
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

    /// The model-space matrix for vanilla's held-item hand transform (or the
    /// model's own override — see [`HandPoseOverride`]), for the item this arm
    /// is holding.
    ///
    /// `input` re-derives the same animated pose [`Self::pose`] would, so a
    /// held item reflects the mob's current walk/attack state exactly as
    /// vanilla does (it poses the whole model, then runs the hand transform
    /// against the posed parts — never the rest pose). `left` selects the arm
    /// vanilla reads from its main-hand-side setting.
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
    /// mirroring the corresponding vanilla pose-setup function.
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
            // its headbutt-charge animation, and the ender dragon likewise carries an
            // authored head rotation. Assigning would level both heads.
            poses[h].y_rot += input.head_yaw_deg * DEG;
            poses[h].x_rot += input.head_pitch_deg * DEG;
        }

        match self.family {
            AnimFamily::Static | AnimFamily::HeadOnly => {}

            // Vanilla's quadruped pose setup: diagonally opposite legs swing together.
            AnimFamily::Quadruped => {
                let swing = |phase: f32| (pos * WALK_FREQ + phase).cos() * 1.4 * amt;
                set_x_rot(poses, s.right_hind_leg, swing(0.0));
                set_x_rot(poses, s.left_hind_leg, swing(std::f32::consts::PI));
                set_x_rot(poses, s.right_front_leg, swing(std::f32::consts::PI));
                set_x_rot(poses, s.left_front_leg, swing(0.0));
            }

            // Vanilla's spider pose setup: legs splay outward (yRot) and lift (zRot),
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

            // Vanilla's humanoid pose setup, minus the swim/ride branches whose state
            // we do not decode yet. The item-pose branch
            // (`pose_arms_for_item`) and the crouch branch is below.
            AnimFamily::Humanoid => {
                // Vanilla's humanoid pose setup swim head-pitch clause — only the
                // swim-amount-positive half; the fall-flying state is absent, as
                // this module's own doc already lists. Transcribed exactly:
                // `head_x_rot = rot_lerp_rad(swim_amount, head_x_rot, -PI/4)`.
                // `poses[h].x_rot` already holds `head_pitch * DEG` from the
                // family-independent head block above this match, matching
                // vanilla's own `head_x_rot = state_x_rot * DEG` immediately
                // before this branch.
                if input.swim_amount > 0.0
                    && let Some(h) = s.head
                {
                    poses[h].x_rot =
                        rot_lerp_rad(input.swim_amount, poses[h].x_rot, -std::f32::consts::FRAC_PI_4);
                }

                let arm = |phase: f32| (pos * WALK_FREQ + phase).cos() * 2.0 * amt * 0.5;
                let leg = |phase: f32| (pos * WALK_FREQ + phase).cos() * 1.4 * amt;
                // A zombie rig runs vanilla's humanoid pose setup too, but
                // its zombie-arms animation then *assigns* over both arms, so the walk
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

                // Vanilla's humanoid pose setup applies a passenger (riding/sitting)
                // branch, transcribed exactly: both arms rotate an additional
                // `-PI / 5` on their x-axis; both legs get a fixed sit rotation
                // (`x_rot = -1.4137167`), splayed outward on y (`+PI/10` /
                // `-PI/10`) and z (`+0.07853982` / `-0.07853982`) for the right and
                // left leg respectively.
                //
                // Placed exactly where vanilla places it: **after** the walk-swing
                // legs above (which it wholly overwrites — a riding leg has no walk
                // cycle) and **before** `pose_arms_for_item` below (the arms only
                // *add*, so a using-item passenger's item pose still lands on top of
                // the sit rotation rather than being erased by it — vanilla's own
                // per-arm item-pose step runs after this block and assigns over
                // whatever it left).
                if input.is_passenger {
                    if let Some(i) = s.right_arm {
                        poses[i].x_rot += -std::f32::consts::PI / 5.0;
                    }
                    if let Some(i) = s.left_arm {
                        poses[i].x_rot += -std::f32::consts::PI / 5.0;
                    }
                    if let Some(i) = s.right_leg {
                        poses[i].x_rot = -1.413_716_7;
                        poses[i].y_rot = std::f32::consts::PI / 10.0;
                        poses[i].z_rot = 0.078_539_82;
                    }
                    if let Some(i) = s.left_leg {
                        poses[i].x_rot = -1.413_716_7;
                        poses[i].y_rot = -std::f32::consts::PI / 10.0;
                        poses[i].z_rot = -0.078_539_82;
                    }
                }

                // Vanilla's ordering exactly: its humanoid pose setup poses the arms for the
                // item *after* the walk swing and *before* the attack-swing step.
                // It matters in both directions —
                // the item pose must overwrite the walk swing (it assigns), and
                // the attack swing must then be layered on top of it (it adds).
                self.pose_arms_for_item(poses, input);

                self.attack_anim(poses, input);

                // Vanilla's humanoid pose setup applies a crouch branch,
                // transcribed exactly: the body pitches to `0.5` (assign, not
                // add); both arms gain an additional `+0.4` x-rotation; both
                // legs shift `+4.0` on z; the head shifts `+4.2` and the body,
                // left arm and right arm all shift `+3.2` on y.
                //
                // Three things this position in the sequence is load bearing for,
                // all of them vanilla's ordering rather than the readable one:
                //
                // * It is **after** the attack-swing step, so a crouched swing
                //   keeps the body pitch — the attack twists `body.y_rot` and this
                //   assigns `body.x_rot`, two different axes, so neither erases the
                //   other.
                // * It is **before** the idle-bob step, so the idle arm bob rides on
                //   top of the lowered arms rather than being replaced by them.
                // * The arm `x_rot` and both leg/head/body translations **add**;
                //   only `body.x_rot` assigns. Making the whole block assign would
                //   flatten the walk swing out of a crouch-walk, which reads as
                //   "sneaking has no animation" rather than as a wrong pose.
                //
                // Translations are in model texels because `PartPose` is
                // (`Affine::of_pose` is `translate(pivot/16) ∘ rotZYX ∘ scale`,
                // matching vanilla's model-part translate-and-rotate transform), so no unit or sign
                // conversion applies — vanilla's model space is the same Y-down
                // texel space ours is, and the flip to world orientation happens
                // once, downstream of every pose.
                if input.crouching {
                    if let Some(i) = s.body {
                        poses[i].x_rot = 0.5;
                        poses[i].y += 3.2;
                    }
                    for arm in [s.right_arm, s.left_arm] {
                        if let Some(i) = arm {
                            poses[i].x_rot += 0.4;
                            poses[i].y += 3.2;
                        }
                    }
                    for leg in [s.right_leg, s.left_leg] {
                        if let Some(i) = leg {
                            poses[i].z += 4.0;
                        }
                    }
                    if let Some(i) = s.head {
                        poses[i].y += 4.2;
                    }
                }

                match self.arms {
                    // Vanilla's idle-bob animation utility on each arm, opposite signs,
                    // then vanilla's humanoid pose setup swim arm-stroke clause
                    // (see [`Self::pose_swim_arms`]) — vanilla runs both inside
                    // the same base-class call a zombie's raised-arm override
                    // (its own zombie-arms animation) runs strictly *after* and wholly
                    // overwrites, so neither belongs on the `Zombie` arm.
                    HumanoidArms::Swinging => {
                        bob(poses, s.right_arm, input.age_ticks, 1.0);
                        bob(poses, s.left_arm, input.age_ticks, -1.0);

                        // "not currently using an item" — approximated as `arm_pose
                        // == Empty` since every `ArmPose` variant this build
                        // tracks (`Item`, `BowAndArrow`, the crossbow poses)
                        // corresponds to a real vanilla using-item
                        // state (`Item` doubles as vanilla's eat/drink pose; see
                        // [`ArmPose::Item`]'s own doc). The `rightArmPose`/
                        // `leftArmPose != SPEAR` and `attackArm` conjuncts vanilla
                        // also gates on are not modelled — `SPEAR` is not a
                        // tracked [`ArmPose`] and this build does not track which
                        // arm is attacking — so both swim arms always take the
                        // full `swim_amount` rather than being selectively zeroed
                        // for a mid-swing attacking arm. Documented gap, not a
                        // guess: the untracked half of a real vanilla conjunction.
                        if input.swim_amount > 0.0 && input.arm_pose == ArmPose::Empty {
                            self.pose_swim_arms(poses, input);
                        }
                    }
                    HumanoidArms::Zombie => self.animate_zombie_arms(poses, input),
                }

                // Vanilla's humanoid pose setup swim leg-kick clause — outside the
                // using-item gate above (vanilla's legs kick regardless of the
                // arms) and not gated by [`Self::arms`] either (a swimming
                // zombie's raised-arm override never touches its legs, so the
                // kick survives for every [`AnimFamily::Humanoid`] rig).
                // Transcribed exactly: `lerp(swim_amount, leg_x_rot, 0.3 *
                // cos(animation_pos * 0.33333334F [+ PI for the left leg]))`.
                if input.swim_amount > 0.0 {
                    if let Some(i) = s.left_leg {
                        poses[i].x_rot = lerp(
                            input.swim_amount,
                            poses[i].x_rot,
                            0.3 * (pos * 0.333_333_34 + std::f32::consts::PI).cos(),
                        );
                    }
                    if let Some(i) = s.right_leg {
                        poses[i].x_rot = lerp(
                            input.swim_amount,
                            poses[i].x_rot,
                            0.3 * (pos * 0.333_333_34).cos(),
                        );
                    }
                }
            }

            // Vanilla's villager pose setup: legs at half a humanoid's amplitude, arms
            // fused into one part that vanilla leaves at its authored pose.
            AnimFamily::Villager => {
                let leg = |phase: f32| (pos * WALK_FREQ + phase).cos() * 1.4 * amt * 0.5;
                set_x_rot(poses, s.right_leg, leg(0.0));
                set_x_rot(poses, s.left_leg, leg(std::f32::consts::PI));
                set_y_rot(poses, s.right_leg, 0.0);
                set_y_rot(poses, s.left_leg, 0.0);
            }

            // Vanilla's chicken pose setup. The wing flap is driven by `flap`/
            // `flapSpeed`, which are wing-oscillator state we do not track, so
            // only the legs move; a guessed flap would be motion for its own
            // sake.
            AnimFamily::Chicken => {
                let leg = |phase: f32| (pos * WALK_FREQ + phase).cos() * 1.4 * amt;
                set_x_rot(poses, s.right_leg, leg(0.0));
                set_x_rot(poses, s.left_leg, leg(std::f32::consts::PI));
            }
        }

        // Vanilla's armour-stand-armour pose setup, which is the base humanoid
        // pose setup — everything above — followed by an unconditional assignment of all
        // six part rotations from the stand's pose. Placed here, last, because
        // that is where vanilla places it: the base pass has already run in
        // full and every joint it wrote is about to be overwritten.
        //
        // Rotations only. Vanilla assigns `xRot`/`yRot`/`zRot` and never touches
        // a part's translation, so the crouch's `y` offsets and the attack
        // swing's arm orbit survive underneath exactly as they do there.
        if let Some(pose) = input.armor_stand_pose {
            self.pose_armor_stand(poses, pose);
        }
    }

    /// Vanilla's armour-stand-armour pose setup's six assignments, plus
    /// vanilla's armour-stand pose setup's three body sticks.
    ///
    /// Two vanilla model classes, one function, because the split between them is a
    /// Java inheritance detail with no counterpart here: the armour-stand-armour
    /// model
    /// exists so the *armour* layer can be posed by the same code as the stand,
    /// and it drives head/body/arms/legs; the armour-stand model extends it to add
    /// the stand's own decorative sticks, which take the **body** pose. Our
    /// corpus has one `armor_stand` model carrying all of it, and a model
    /// lacking the sticks (any armour layer built on this rig) simply resolves
    /// those slots to `None`.
    ///
    /// # What is deliberately not ported
    ///
    /// Vanilla's armour-stand pose setup also sets `basePlate.yRot = -state.yRot`,
    /// cancelling the stand's body rotation so the plate stays world-aligned.
    /// That needs the entity's **absolute** yaw, which [`AnimInput`] does not
    /// carry — it holds head yaw *relative to the body*, by contract — and the
    /// whole-entity yaw is applied downstream by
    /// [`entity_model_matrix`](crate::entity::entity_model_matrix), outside this
    /// module. So the plate rotates with the stand here where vanilla holds it
    /// square. Left as a stated gap rather than approximated from the head yaw,
    /// which is a different angle and would be wrong by exactly the amount the
    /// head is turned.
    ///
    /// Angles arrive in degrees (the wire's units, and the builder's) and are
    /// converted once, here, next to the model space that consumes them.
    fn pose_armor_stand(&self, poses: &mut [PartPose], pose: lodestone_model::ArmorStandPose) {
        let s = &self.slots;
        // Named pairs, never a positional list: six same-typed triples in a row
        // is the shape a transposition survives every round trip, and the only
        // symptom would be a stand whose left arm sits where its right leg
        // should be.
        let assignments = [
            (s.head, pose.head),
            (s.body, pose.body),
            (s.left_arm, pose.left_arm),
            (s.right_arm, pose.right_arm),
            (s.left_leg, pose.left_leg),
            (s.right_leg, pose.right_leg),
            // Vanilla's armour-stand pose setup: all three sticks take the body pose.
            (s.right_body_stick, pose.body),
            (s.left_body_stick, pose.body),
            (s.shoulder_stick, pose.body),
        ];
        for (slot, rotation) in assignments {
            if let Some(i) = slot {
                poses[i].x_rot = rotation.x * DEG;
                poses[i].y_rot = rotation.y * DEG;
                poses[i].z_rot = rotation.z * DEG;
            }
        }
    }

    /// Vanilla's zombie-arms animation utility: both arms held out in front, thrust
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
    ///   arms twice (once in the base humanoid pose setup, once here); only the second
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

    /// Vanilla's humanoid pose setup swim arm-stroke block (`swimAmount > 0.0F`)
    /// (the `!state.isUsingItem` half — see the call site's own doc for the
    /// gate), transcribed exactly from the 26.2 client. Three windows of
    /// `animationPos % 26.0`: the recovery reach (`< 14`), the catch
    /// (`14..22`), and the pull back to the side (`22..26`);
    /// `quadraticArmUpdate(x) = -65x + x²` shapes the entry curve of the
    /// first window's `z_rot`. Every value is a *lerp from whatever the walk
    /// swing and idle bob already left the arm at* toward that window's
    /// stroke target, not an assignment — so a mid-ramp `swim_amount` blends
    /// continuously between walking and stroking rather than snapping.
    ///
    /// # Vanilla mixes lerp kinds per *arm*, not per axis
    ///
    /// Every left-arm component below uses the wrap-aware [`rot_lerp_rad`];
    /// every right-arm component uses the plain [`lerp`]. Kept exactly as the
    /// decompile has it rather than "fixed" to be consistent — the difference
    /// is bit-for-bit what a real client draws, and the two only diverge when
    /// a `to - from` gap exceeds `PI`, which the target angles below (all in
    /// `[0, PI]` or `[PI/2, PI]`-ish ranges reached by a bounded per-frame
    /// ramp) are not expected to.
    fn pose_swim_arms(&self, poses: &mut [PartPose], input: &AnimInput) {
        let s = &self.slots;
        let amount = input.swim_amount;
        let swim_pos = input.limb_swing.rem_euclid(26.0);
        let quad = |x: f32| -65.0 * x + x * x;
        let pi = std::f32::consts::PI;

        if swim_pos < 14.0 {
            let z_lean = 1.8707964 * quad(swim_pos) / quad(14.0);
            if let Some(i) = s.left_arm {
                poses[i].x_rot = rot_lerp_rad(amount, poses[i].x_rot, 0.0);
                poses[i].y_rot = rot_lerp_rad(amount, poses[i].y_rot, pi);
                poses[i].z_rot = rot_lerp_rad(amount, poses[i].z_rot, pi + z_lean);
            }
            if let Some(i) = s.right_arm {
                poses[i].x_rot = lerp(amount, poses[i].x_rot, 0.0);
                poses[i].y_rot = lerp(amount, poses[i].y_rot, pi);
                poses[i].z_rot = lerp(amount, poses[i].z_rot, pi - z_lean);
            }
        } else if swim_pos < 22.0 {
            let t = (swim_pos - 14.0) / 8.0;
            if let Some(i) = s.left_arm {
                poses[i].x_rot = rot_lerp_rad(amount, poses[i].x_rot, std::f32::consts::FRAC_PI_2 * t);
                poses[i].y_rot = rot_lerp_rad(amount, poses[i].y_rot, pi);
                poses[i].z_rot = rot_lerp_rad(amount, poses[i].z_rot, 5.012389 - 1.8707964 * t);
            }
            if let Some(i) = s.right_arm {
                poses[i].x_rot = lerp(amount, poses[i].x_rot, std::f32::consts::FRAC_PI_2 * t);
                poses[i].y_rot = lerp(amount, poses[i].y_rot, pi);
                poses[i].z_rot = lerp(amount, poses[i].z_rot, 1.2707963 + 1.8707964 * t);
            }
        } else {
            let t = (swim_pos - 22.0) / 4.0;
            let x_target = std::f32::consts::FRAC_PI_2 - std::f32::consts::FRAC_PI_2 * t;
            if let Some(i) = s.left_arm {
                poses[i].x_rot = rot_lerp_rad(amount, poses[i].x_rot, x_target);
                poses[i].y_rot = rot_lerp_rad(amount, poses[i].y_rot, pi);
                poses[i].z_rot = rot_lerp_rad(amount, poses[i].z_rot, pi);
            }
            if let Some(i) = s.right_arm {
                poses[i].x_rot = lerp(amount, poses[i].x_rot, x_target);
                poses[i].y_rot = lerp(amount, poses[i].y_rot, pi);
                poses[i].z_rot = lerp(amount, poses[i].z_rot, pi);
            }
        }
    }

    /// Vanilla's per-arm item-pose functions for the ranged [`ArmPose`]s:
    /// both arms come up to hold the weapon, tracking the head.
    ///
    /// # These assign, they do not accumulate
    ///
    /// Vanilla writes `right_arm_x_rot = …`, replacing whatever the walk cycle
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
    /// vanilla's behaviour, not a bug in the ordering: vanilla's zombie pose setup
    /// calls the base humanoid pose setup and then its zombie-arms animation utility
    /// unconditionally, so a bow-holding zombie keeps the arms-forward zombie pose
    /// in vanilla too. Skeletons — the rig the issue was reported against — are
    /// `HumanoidArms::Swinging` and do keep it.
    fn pose_arms_for_item(&self, poses: &mut [PartPose], input: &AnimInput) {
        let s = &self.slots;
        let (Some(right), Some(left)) = (s.right_arm, s.left_arm) else {
            return;
        };
        // Vanilla's per-arm item-pose functions differ only in which arm is treated as the
        // holder; vanilla picks by whether the using hand matches the mob's main
        // hand. Both branches of
        // every modelled pose write both arms, so this resolves to one pair.
        let holding_in_right = !input.arm_pose_left_hand;
        let head_y_rot = s.head.map_or(0.0, |i| poses[i].y_rot);
        let head_x_rot = s.head.map_or(0.0, |i| poses[i].x_rot);

        match input.arm_pose {
            ArmPose::Empty => {}

            // Vanilla's per-arm item-pose function, item case: the holding
            // arm's x-rotation becomes half its previous value minus `PI / 10`,
            // and its y-rotation is reset to `0.0`.
            //
            // **This is the third-person pose for eating and drinking** — see
            // [`ArmPose::Item`] for why vanilla has no separate one.
            //
            // Two things separate it from every pose below it:
            //
            // * It **reads its own previous value**, so unlike the ranged poses it
            //   is not a pure assignment: the arm keeps half of whatever the walk
            //   swing put there. Replacing `x_rot * 0.5` with a constant freezes a
            //   walking player's arm, which reads as the pose being applied at the
            //   wrong time rather than as a dropped term.
            // * It writes **one** arm. Vanilla's item arm-pose is neither two-handed nor
            //   flagged as affecting the off-hand pose, so vanilla poses each arm from its own
            //   pose and the off hand — empty-pose for an empty hand — keeps its
            //   swing. Touching both arms here would raise the empty hand too,
            //   which is what makes it look like a shrug rather than a bite.
            ArmPose::Item => {
                let holder = if holding_in_right { right } else { left };
                poses[holder].x_rot = poses[holder].x_rot * 0.5 - std::f32::consts::PI / 10.0;
                poses[holder].y_rot = 0.0;
            }

            // Vanilla's bow-and-arrow arm pose (right-arm and left-arm cases are
            // mirror images of each other). Both arms take the *same* xRot; the
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

            // Vanilla's crossbow-charge arm animation.
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

            // Vanilla's crossbow-hold arm animation.
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

    /// Vanilla's attack-animation setup, melee ("whack") branch: the body twists,
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

/// Vanilla's own lerp helper: `lerp(alpha, from, to)` — note vanilla's
/// argument order puts the *alpha*
/// first, which is the opposite of most Rust `lerp` conventions and is the reason
/// this exists rather than a call to something in `glam`: transcribing
/// `lerp(alpha, 0.4F, 0.85F)` as `0.4.lerp(0.85, alpha)` is easy, and
/// getting it backwards silently swaps a crossbow's start and end pose.
fn lerp(alpha: f32, from: f32, to: f32) -> f32 {
    from + alpha * (to - from)
}

/// Vanilla's own radian-rotation lerp helper: `rot_lerp_rad(alpha, from, to)`:
/// [`lerp`] with the `to - from` difference
/// first wrapped into `(-PI, PI]`, so a lerp across the wrap point (e.g.
/// `from` just under `PI`, `to` just over `-PI`) takes the short way round
/// instead of spinning the long way through zero. Used by the swim branch's
/// y-rotation/z-rotation arm blends, which cross that seam as the stroke cycles.
fn rot_lerp_rad(alpha: f32, from: f32, to: f32) -> f32 {
    let mut diff = to - from;
    while diff < -std::f32::consts::PI {
        diff += std::f32::consts::TAU;
    }
    while diff >= std::f32::consts::PI {
        diff -= std::f32::consts::TAU;
    }
    from + alpha * diff
}

/// Vanilla's ease-out-quart curve: `1 - (1 - t)^4`.
fn ease_out_quart(t: f32) -> f32 {
    let inv = 1.0 - t;
    1.0 - inv * inv * inv * inv
}

/// Vanilla's idle-bob animation utility: a slow idle sway on an arm.
fn bob(poses: &mut [PartPose], slot: Option<usize>, age: f32, scale: f32) {
    if let Some(i) = slot {
        poses[i].z_rot += scale * ((age * 0.09).cos() * 0.05 + 0.05);
        poses[i].x_rot += scale * ((age * 0.067).sin() * 0.05);
    }
}

// Vanilla's pose setup *assigns* limb rotations (`rightHindLeg.xRot = ...`)
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
    use super::{BoatHurt, boat_hurt_roll_degrees};

    /// The hull roll must land on `sin(t) * t * damage / 10 * dir` with `t` in
    /// **radians**, and must be exactly zero when the clock is not running.
    ///
    /// The discriminating input is `t = 7.0`: `sin(7 rad)` is `+0.657` while
    /// `sin(7°)` is `+0.122`, so the two readings are a factor of five apart and
    /// — more importantly — the radian reading has already crossed zero and come
    /// back, which is the oscillation the animation *is*. At a small `t` the two
    /// hypotheses agree to within a few percent, so a fixture at `t = 1` would
    /// measure nothing.
    ///
    /// Both expected values are computed here from the vanilla formula, not by
    /// calling the function twice.
    #[test]
    fn the_boat_roll_uses_radians_and_stops_when_the_clock_does() {
        let hurt = BoatHurt {
            time: 7.0,
            dir: -1.0,
            damage: 23.5,
        };
        let radians = 7.0_f32.sin() * 7.0 * 23.5 / 10.0 * -1.0;
        let degrees = 7.0_f32.to_radians().sin() * 7.0 * 23.5 / 10.0 * -1.0;
        let got = boat_hurt_roll_degrees(hurt);
        assert!(
            (got - radians).abs() < 1e-3,
            "expected {radians} (radians), got {got}; the degrees reading would \
             give {degrees}"
        );
        assert!(
            (radians - degrees).abs() > 1.0,
            "the two readings must be distinguishable at this input, or the gate \
             cannot fail: {radians} vs {degrees}"
        );

        // The sign is carried by `hurtDir` alone: the same hit the other way
        // round is the exact negation, which is what makes consecutive punches
        // rock the hull alternately.
        let flipped = boat_hurt_roll_degrees(BoatHurt { dir: 1.0, ..hurt });
        assert!((flipped + got).abs() < 1e-4, "hurtDir must negate the whole roll");

        // Not hurt: exactly zero, so the placement's early return is reachable.
        assert_eq!(boat_hurt_roll_degrees(BoatHurt::REST), 0.0);
        assert_eq!(
            boat_hurt_roll_degrees(BoatHurt {
                time: 0.0,
                ..hurt
            }),
            0.0
        );

        // And the trap the type's own doc names: a zero direction silences the
        // whole animation, which is why `REST` carries vanilla's registered `1`.
        assert_eq!(BoatHurt::REST.dir, 1.0);
        assert_eq!(
            boat_hurt_roll_degrees(BoatHurt { dir: 0.0, ..hurt }),
            0.0,
            "a zero hurtDir multiplies the roll away -- the reason the default is 1"
        );
    }

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
        // vanilla's spider pose setup adds to the authored splay, which is why the
        // Spider arm uses `add_*` deliberately. The rest are approximations
        // pending a per-model pose-setup port, and are recorded in HANDOFF.md.
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

    /// [`death_fall_over_degrees`] against vanilla's living-entity rotation setup, at
    /// ticks where the **linear** reading — "90 degrees over 20 ticks", the answer
    /// anybody writes from the description — gives a different angle.
    ///
    /// Live player report: *"stuff dying doesnt have the death animation (the one
    /// where they turn red and tilt on their side)"*. The tilt is this function; the
    /// red is the `hurtTime > 0 || deathTime > 0` disjunction in
    /// `lodestone-shell`'s `EntityDraw`.
    ///
    /// Every expectation is hand-evaluated from `sqrt((deathTime - 1)/20 · 1.6)`
    /// clamped to 1, times `getFlipDegrees()`'s 90 — and the linear reading is
    /// evaluated at the same tick so each row *records* that the two differ rather
    /// than asserting it. **`deathTime == 20` is the coincident input**: both
    /// readings give exactly 90 there, because the `sqrt` has already saturated, so
    /// a gate written at the obvious "end of the animation" tick measures only that
    /// the function runs. It is included as the control that must agree.
    ///
    /// Mismatches are collected rather than asserted inside the loop, so a neuter
    /// reports every arm instead of aborting on the first.
    #[test]
    fn death_fall_over_is_a_sqrt_ramp_that_saturates_before_the_count_does() {
        // (death_time, degrees). Hand-evaluated; `sqrt` saturates at 13.5, not 20.
        let cases: [(f32, f32); 8] = [
            // Alive. The guard, not the formula — without it this is `sqrt(-0.08)`.
            (0.0, 0.0),
            // The first tick of death: the expression is *independently* zero here.
            (1.0, 0.0),
            (2.0, 25.455_844),
            (4.0, 44.090_82),
            (6.0, 56.921_04),
            (10.0, 76.367_54),
            // The saturation point, and the whole reason "over 20 ticks" is wrong.
            (13.5, 90.0),
            // Coincident control: linear and real agree, so this row is not a test.
            (20.0, 90.0),
        ];
        let mut mismatches: Vec<String> = Vec::new();
        for (death_time, want) in cases {
            let got = death_fall_over_degrees(death_time);
            if (got - want).abs() > 1e-3 {
                mismatches.push(format!("deathTime {death_time}: want {want}, got {got}"));
            }
            // A living entity must not produce a NaN, which would propagate through
            // the rotation matrix and make every vertex vanish — a silent, total
            // failure rather than a wrong angle. Asserted for every row, since a
            // reordered guard could NaN anywhere.
            if !got.is_finite() {
                mismatches.push(format!("deathTime {death_time}: produced {got}"));
            }
            let linear = (death_time / 20.0).min(1.0) * FLIP_DEGREES;
            let coincident = (linear - want).abs() <= 1e-3;
            let expected_coincident = death_time == 0.0 || death_time >= 20.0;
            if coincident != expected_coincident {
                mismatches.push(format!(
                    "deathTime {death_time}: the linear reading gives {linear} against a \
                     real {want} — this row was classified as \
                     {}, so either the classification or one of the two readings is wrong",
                    if expected_coincident {
                        "coincident (a control)"
                    } else {
                        "discriminating"
                    }
                ));
            }
        }
        // Monotonic, and never past flat: a mob keeps toppling one way and stops.
        let mut previous = 0.0;
        for tick in 0u8..=40 {
            let got = death_fall_over_degrees(f32::from(tick));
            if got < previous - 1e-4 || got > FLIP_DEGREES + 1e-4 {
                mismatches.push(format!(
                    "deathTime {tick}: {got} is not a monotonic approach to \
                     {FLIP_DEGREES} (previous {previous})"
                ));
            }
            previous = got;
        }
        assert!(
            mismatches.is_empty(),
            "death fall-over diverges from vanilla's living-entity rotation setup:\n  {}",
            mismatches.join("\n  ")
        );
    }

    #[test]
    fn creeper_swell_scale_transcribes_the_vanilla_formula() {
        // Hand-evaluated from vanilla's creeper scale function, not read back off this
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

    /// Hand-evaluated from vanilla's creeper white-overlay-progress function's decompiled
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
    /// vanilla's own clamp helper caps the top explicitly at `clamp(step, 0.5, 1.0)`, unlike
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
                     zombie-arms animation puts it at ~0.74"
                );
            }
        }
    }

    /// The pose must be *unconditional*, not a by-product of the walk cycle:
    /// vanilla assigns over the base humanoid pose setup's arm swing, so a walking zombie's
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
            "the arm moved between walk phases ({a} vs {b}) — vanilla's zombie-arms animation assigns, so a \
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

    /// Vanilla's zombie-arms animation's attack and aggression terms, which a pose that only
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

    /// `parts[i].parent`-relative isolation of one joint, the same technique
    /// [`posing_at_rest_keeps_every_joint_where_the_rest_pose_put_it`] uses —
    /// factors out the whole parent chain (root/body yaw, mount offsets, …)
    /// so the comparison is purely about the one joint's own rotation.
    fn local_of(parts: &[BakedPart], posed: &[Mat4], i: usize) -> Mat4 {
        match parts[i].parent {
            Some(p) => posed[p].inverse() * posed[i],
            None => posed[i],
        }
    }

    /// Vanilla's humanoid pose setup swim head-pitch clause:
    /// `head_x_rot = rot_lerp_rad(swim_amount, head_x_rot, -PI/4)`. At
    /// `AnimInput::REST`'s `head_pitch_deg: 0.0`, `head_x_rot` going in is
    /// `0.0`, so a full `swim_amount` must land exactly on `-PI/4` — an exact
    /// prediction from the formula, not merely "the head moved".
    ///
    /// The `swim_amount: 0.0` arm is the control: it proves the assertion
    /// distinguishes "the branch ran" from "the head never moves at all",
    /// which a test that only checked the `1.0` case could not.
    #[test]
    fn swim_head_pitches_to_the_predicted_angle() {
        let models = entity_models();
        let entry = models.iter().find(|e| e.name == "player_wide").unwrap();
        let parts = bake_entity_parts(&(entry.build)());
        let skel = Skeleton::from_parts(&parts)
            .with_humanoid_arms(crate::entity::humanoid_arms_for("player_wide"));
        let head = skel.index_of("head").unwrap();

        let expected = affine_to_mat4(&Affine::of_pose(&PartPose {
            x_rot: -std::f32::consts::FRAC_PI_4,
            ..parts[head].rest
        }));

        let full_swim = skel.pose(&AnimInput {
            swim_amount: 1.0,
            ..AnimInput::REST
        });
        let actual = local_of(&parts, &full_swim, head);
        for c in 0..3 {
            let d = (actual.col(c) - expected.col(c)).length();
            assert!(
                d < 1e-4,
                "head basis {c} off by {d} from the predicted -PI/4 pitch at swim_amount=1.0"
            );
        }

        let no_swim = skel.pose(&AnimInput::REST);
        let control = local_of(&parts, &no_swim, head);
        let d_control = (control.col(2) - expected.col(2)).length();
        assert!(
            d_control > 0.3,
            "control: the rest head already sits at the swim-pitch target ({d_control}), so \
             the positive assertion above cannot distinguish the branch running from not"
        );
    }

    /// Vanilla's humanoid pose setup swim arm-stroke clause
    /// ([`Skeleton::pose_swim_arms`]'s own doc has the full transcription),
    /// predicted here independently from the same 26.2 decompile rather than
    /// by calling that method — a transcription mistake in the
    /// implementation must not also be baked into the prediction it is
    /// checked against.
    ///
    /// Two swim positions, chosen to discriminate the window logic rather
    /// than only exercise a boundary: `swim_pos == 0.0` (`quadraticArmUpdate`
    /// zero, so the first window's z-lean term is zero) and `swim_pos ==
    /// 18.0` (the *second* window, `t == 0.5`, where a build that only
    /// implemented the first window would predict the wrong angle). `amt:
    /// 0.0` keeps the ordinary walk cycle at zero so it cannot contribute.
    #[test]
    fn swim_arm_stroke_matches_the_vanilla_formula_across_two_windows() {
        let models = entity_models();
        let entry = models.iter().find(|e| e.name == "player_wide").unwrap();
        let parts = bake_entity_parts(&(entry.build)());
        let skel = Skeleton::from_parts(&parts)
            .with_humanoid_arms(crate::entity::humanoid_arms_for("player_wide"));
        let right = skel.index_of("right_arm").unwrap();
        let left = skel.index_of("left_arm").unwrap();
        let pi = std::f32::consts::PI;

        let check = |limb_swing: f32, right_target: (f32, f32, f32), left_target: (f32, f32, f32), label: &str| {
            let posed = skel.pose(&AnimInput {
                swim_amount: 1.0,
                limb_swing,
                ..AnimInput::REST
            });
            let expected_right = affine_to_mat4(&Affine::of_pose(&PartPose {
                x_rot: right_target.0,
                y_rot: right_target.1,
                z_rot: right_target.2,
                ..parts[right].rest
            }));
            let expected_left = affine_to_mat4(&Affine::of_pose(&PartPose {
                x_rot: left_target.0,
                y_rot: left_target.1,
                z_rot: left_target.2,
                ..parts[left].rest
            }));
            let actual_right = local_of(&parts, &posed, right);
            let actual_left = local_of(&parts, &posed, left);
            let mut mismatches = Vec::new();
            for c in 0..3 {
                let dr = (actual_right.col(c) - expected_right.col(c)).length();
                if dr >= 1e-4 {
                    mismatches.push(format!("{label} right_arm basis {c} off by {dr}"));
                }
                let dl = (actual_left.col(c) - expected_left.col(c)).length();
                if dl >= 1e-4 {
                    mismatches.push(format!("{label} left_arm basis {c} off by {dl}"));
                }
            }
            assert!(mismatches.is_empty(), "{}", mismatches.join("; "));
        };

        // swim_pos == 0.0: quadraticArmUpdate(0) == 0, so z_lean == 0.
        check(0.0, (0.0, pi, pi), (0.0, pi, pi), "window 1 (swim_pos=0)");

        // swim_pos == 18.0, second window, t == (18-14)/8 == 0.5:
        // xRot = (PI/2)*0.5; zRot(right) = 1.2707963 + 1.8707964*0.5;
        // zRot(left) = 5.012389 - 1.8707964*0.5.
        let t = 0.5;
        check(
            18.0,
            (std::f32::consts::FRAC_PI_2 * t, pi, 1.2707963 + 1.8707964 * t),
            (std::f32::consts::FRAC_PI_2 * t, pi, 5.012389 - 1.8707964 * t),
            "window 2 (swim_pos=18)",
        );

        // Control: at swim_amount == 0.0 the arm must sit nowhere near the
        // window-1 target, or the positive checks above would pass whether
        // or not `pose_swim_arms` ever runs. Basis 1 (the y-axis column), not
        // 0: `Rz(PI) . Ry(PI)` happens to fix the x-axis (a double flip
        // cancels there), so column 0 is the wrong column to discriminate on
        // — column 1 (and 2) move by a full 2.0, which is why the first
        // version of this control picked the one column that cannot see the
        // difference.
        let no_swim = skel.pose(&AnimInput::REST);
        let control = local_of(&parts, &no_swim, right);
        let window_1_target = affine_to_mat4(&Affine::of_pose(&PartPose {
            x_rot: 0.0,
            y_rot: pi,
            z_rot: pi,
            ..parts[right].rest
        }));
        let d_control = (control.col(1) - window_1_target.col(1)).length();
        assert!(
            d_control > 0.3,
            "control: the rest arm already matches the swim target ({d_control})"
        );
    }

    /// Vanilla's humanoid pose setup swim leg-kick clause is unconditional —
    /// not gated by `isUsingItem`, and (unlike the arm block) not gated by
    /// [`HumanoidArms`] either, since a swimming zombie's raised-arm override
    /// never touches its legs. `zombie` is used here specifically to prove
    /// that: the arm block above only ever runs for `Swinging`, so exercising
    /// the leg kick on the *other* arm rig is the discriminating case.
    #[test]
    fn swim_leg_kick_applies_even_on_the_zombie_arm_rig() {
        let models = entity_models();
        let entry = models.iter().find(|e| e.name == "zombie").unwrap();
        let parts = bake_entity_parts(&(entry.build)());
        let skel = Skeleton::from_parts(&parts)
            .with_humanoid_arms(crate::entity::humanoid_arms_for("zombie"));
        assert_eq!(skel.arms, HumanoidArms::Zombie, "zombie must be the non-swinging rig");
        let right_leg = skel.index_of("right_leg").unwrap();

        // swim_pos == 0.0: rightLeg.xRot = lerp(1.0, _, 0.3*cos(0)) == 0.3.
        // y_rot/z_rot: vanilla's unconditional 0.005 rad anti-z-fight nudge
        // (`this.rightLeg.yRot = 0.005F; this.rightLeg.zRot = 0.005F;`),
        // which the swim block's x_rot-only assignment does not touch.
        let posed = skel.pose(&AnimInput {
            swim_amount: 1.0,
            limb_swing: 0.0,
            ..AnimInput::REST
        });
        let expected = affine_to_mat4(&Affine::of_pose(&PartPose {
            x_rot: 0.3,
            y_rot: 0.005,
            z_rot: 0.005,
            ..parts[right_leg].rest
        }));
        let actual = local_of(&parts, &posed, right_leg);
        for c in 0..3 {
            let d = (actual.col(c) - expected.col(c)).length();
            assert!(d < 1e-4, "zombie right_leg basis {c} off by {d} at swim_amount=1.0");
        }
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
    // The crouch (vanilla's humanoid pose setup, crouch branch)
    // -----------------------------------------------------------------------

    /// **The expected values are vanilla's own crouch-branch literals**, not a
    /// restatement of the port: `body.xRot = 0.5F` (an assign), `arm.xRot += 0.4F`,
    /// `leg.z += 4.0F`, `head.y += 4.2F`, `body.y += 3.2F`, `arm.y += 3.2F`.
    ///
    /// Read off the *uncomposed* poses, in texels, so a unit error (texels vs
    /// blocks — a factor of 16) or a sign error is a numeric disagreement here
    /// rather than a model that still looks vaguely hunched in a screenshot.
    #[test]
    fn the_crouch_matches_vanillas_humanoid_setup_anim_constants() {
        let skel = skeleton_for("player_wide");
        let idx = |n: &str| skel.index_of(n).unwrap_or_else(|| panic!("no {n}"));
        let rest = skel.posed(&AnimInput::REST);
        let crouched = skel.posed(&AnimInput {
            crouching: true,
            ..AnimInput::REST
        });

        // `body.xRot = 0.5F` — an **assignment**, so it is the absolute value and
        // not a delta off whatever the rig authored.
        let body = idx("body");
        assert!(
            (crouched[body].x_rot - 0.5).abs() < 1e-6,
            "body pitch {} != 0.5",
            crouched[body].x_rot
        );
        assert!(
            (crouched[body].y - rest[body].y - 3.2).abs() < 1e-4,
            "body y moved by {}, want 3.2 texels",
            crouched[body].y - rest[body].y
        );
        for arm in ["right_arm", "left_arm"] {
            let i = idx(arm);
            assert!(
                (crouched[i].x_rot - rest[i].x_rot - 0.4).abs() < 1e-4,
                "{arm} pitch moved by {}, want += 0.4",
                crouched[i].x_rot - rest[i].x_rot
            );
            assert!(
                (crouched[i].y - rest[i].y - 3.2).abs() < 1e-4,
                "{arm} y moved by {}, want 3.2 texels",
                crouched[i].y - rest[i].y
            );
        }
        for leg in ["right_leg", "left_leg"] {
            let i = idx(leg);
            assert!(
                (crouched[i].z - rest[i].z - 4.0).abs() < 1e-4,
                "{leg} z moved by {}, want 4.0 texels",
                crouched[i].z - rest[i].z
            );
        }
        let head = idx("head");
        assert!(
            (crouched[head].y - rest[head].y - 4.2).abs() < 1e-4,
            "head y moved by {}, want 4.2 texels",
            crouched[head].y - rest[head].y
        );

        // The control: `crouching: false` must be bit-identical to `REST`, or the
        // above is measuring something other than the flag. This is also what
        // keeps `pose(&AnimInput::REST) == rest_pose()` — and hence every
        // model's cull AABB — untouched by this branch.
        let standing = skel.posed(&AnimInput {
            crouching: false,
            ..AnimInput::REST
        });
        assert_eq!(standing, rest, "crouching: false must change nothing");
    }

    /// The crouch sits **after** the attack swing and **before** the idle bob, and
    /// both orderings are observable rather than a matter of taste.
    #[test]
    fn the_crouch_layers_over_the_attack_swing_and_under_the_idle_bob() {
        let skel = skeleton_for("player_wide");
        let body = skel.index_of("body").expect("body");
        let arm = skel.index_of("right_arm").expect("right arm");

        // After `setupAttackAnimation`: that twists `body.y_rot`, this assigns
        // `body.x_rot`. Two axes, so a mid-swing crouch keeps both.
        let swinging = AnimInput {
            attack_anim: 0.5,
            ..AnimInput::REST
        };
        let swing_only = skel.posed(&swinging);
        assert!(
            swing_only[body].y_rot.abs() > 0.01,
            "precondition: this swing really does twist the body"
        );
        let both = skel.posed(&AnimInput {
            crouching: true,
            ..swinging
        });
        assert!(
            (both[body].y_rot - swing_only[body].y_rot).abs() < 1e-6,
            "the crouch must not erase the attack twist"
        );
        assert!((both[body].x_rot - 0.5).abs() < 1e-6, "and must still pitch the body");

        // Before `bobModelPart`: the age-driven bob is still layered on top, so a
        // crouching idle arm is not frozen. `age_ticks` alone must still move it.
        let crouched_early = skel.posed(&AnimInput {
            crouching: true,
            age_ticks: 0.0,
            ..AnimInput::REST
        });
        let crouched_later = skel.posed(&AnimInput {
            crouching: true,
            age_ticks: 9.0,
            ..AnimInput::REST
        });
        assert!(
            (crouched_early[arm].z_rot - crouched_later[arm].z_rot).abs() > 1e-4,
            "the idle bob must survive the crouch: {} vs {}",
            crouched_early[arm].z_rot,
            crouched_later[arm].z_rot
        );
    }

    // -----------------------------------------------------------------------
    // The riding sit pose (vanilla's humanoid pose setup, passenger
    // branch)
    // -----------------------------------------------------------------------

    /// **The expected values are vanilla's own literals**, not a
    /// restatement of the port: arms `+= -PI/5` (an add), legs `xRot =
    /// -1.4137167F` / `yRot = ±PI/10` / `zRot = ±0.07853982F` (all three
    /// assignments). Predicting the assign/add split matters here exactly as it
    /// does for the crouch test above: a plausible-but-wrong "add to the legs
    /// too" implementation would leave the walk swing summed in underneath, and
    /// this is the test that would catch it.
    #[test]
    fn the_sit_pose_matches_vanillas_humanoid_setup_anim_constants() {
        let skel = skeleton_for("player_wide");
        let idx = |n: &str| skel.index_of(n).unwrap_or_else(|| panic!("no {n}"));
        let rest = skel.posed(&AnimInput::REST);
        // Fed with a non-zero walk swing so the legs test is discriminating: if
        // the sit pose only *added* its rotation (the wrong hypothesis), the
        // walking legs' own swing would still be present underneath, which the
        // exact-value assertions below would catch as a non-zero residual.
        let walking = AnimInput {
            limb_swing: 10.0,
            limb_swing_amount: 1.0,
            ..AnimInput::REST
        };
        let seated = skel.posed(&AnimInput {
            is_passenger: true,
            ..walking
        });

        for arm in ["right_arm", "left_arm"] {
            let i = idx(arm);
            let walking_only = skel.posed(&walking)[i].x_rot;
            assert!(
                (seated[i].x_rot - walking_only - (-std::f32::consts::PI / 5.0)).abs() < 1e-5,
                "{arm} pitch moved by {}, want += -PI/5",
                seated[i].x_rot - walking_only
            );
        }

        let right_leg = idx("right_leg");
        assert!(
            (seated[right_leg].x_rot - (-1.413_716_7)).abs() < 1e-5,
            "right leg pitch {} != -1.4137167 (assignment, not the walk swing)",
            seated[right_leg].x_rot
        );
        assert!(
            (seated[right_leg].y_rot - std::f32::consts::PI / 10.0).abs() < 1e-5,
            "right leg yaw {}",
            seated[right_leg].y_rot
        );
        assert!(
            (seated[right_leg].z_rot - 0.078_539_82).abs() < 1e-5,
            "right leg roll {}",
            seated[right_leg].z_rot
        );

        let left_leg = idx("left_leg");
        assert!(
            (seated[left_leg].x_rot - (-1.413_716_7)).abs() < 1e-5,
            "left leg pitch {} != -1.4137167",
            seated[left_leg].x_rot
        );
        assert!(
            (seated[left_leg].y_rot - (-std::f32::consts::PI / 10.0)).abs() < 1e-5,
            "left leg yaw {}",
            seated[left_leg].y_rot
        );
        assert!(
            (seated[left_leg].z_rot - (-0.078_539_82)).abs() < 1e-5,
            "left leg roll {}",
            seated[left_leg].z_rot
        );

        // The control: `is_passenger: false` must be bit-identical to `REST`, or
        // the assertions above are measuring something other than the flag.
        let standing = skel.posed(&AnimInput {
            is_passenger: false,
            ..AnimInput::REST
        });
        assert_eq!(standing, rest, "is_passenger: false must change nothing");
    }

    // -----------------------------------------------------------------------
    // Arm poses for a used item
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
    /// Vanilla's right-arm item-pose function's bow-and-arrow case is four assignments
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
        // arm splays (-0.4). Vanilla's mirrored right-arm/left-arm bow poses.
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

    /// Vanilla's crossbow-charge arm animation: the holding arm is fixed and the
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
            ArmPose::Item,
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

    /// `ArmPose::Item` is the first **one-handed** pose here, so it must leave the
    /// other arm on its walk swing — and it must halve the holding arm's own swing
    /// rather than assign a constant.
    ///
    /// Both properties are invisible in a still frame and both fail in the
    /// plausible-looking direction: posing both arms reads as a shrug, and assigning
    /// a constant freezes the arm of a *walking* player, which looks like the pose
    /// being applied at the wrong moment rather than like a dropped term. Mismatches
    /// are collected so a neuter reports every arm.
    #[test]
    fn the_item_pose_moves_one_arm_and_keeps_half_of_its_swing() {
        let base = AnimInput {
            limb_swing: 4.0,
            limb_swing_amount: 1.0,
            ..AnimInput::REST
        };
        let unposed = arm_rots("skeleton", &base);
        let mut failures: Vec<String> = Vec::new();
        for left_hand in [false, true] {
            let posed = arm_rots(
                "skeleton",
                &AnimInput {
                    arm_pose: ArmPose::Item,
                    arm_pose_left_hand: left_hand,
                    ..base
                },
            );
            let (holder, other) = if left_hand {
                ((posed.1, unposed.1), (posed.0, unposed.0))
            } else {
                ((posed.0, unposed.0), (posed.1, unposed.1))
            };
            if other.0 != other.1 {
                failures.push(format!(
                    "left_hand={left_hand}: the non-holding arm moved ({:?} vs {:?}), so ITEM \
                     is being treated as two-handed",
                    other.0, other.1
                ));
            }
            if holder.0 == holder.1 {
                failures.push(format!(
                    "left_hand={left_hand}: the holding arm did not move at all"
                ));
            }
            // `xRot = xRot * 0.5 - PI/10`, so the swing survives at half amplitude.
            // Predicted from the unposed value and the two constants, and compared
            // against the wrong hypothesis (`xRot = -PI/10`, the swing discarded) —
            // the walk swing is non-zero at `limb_swing = 4.0`, so the two differ.
            let expected = holder.1.0 * 0.5 - std::f32::consts::PI / 10.0;
            let discarded = -std::f32::consts::PI / 10.0;
            if (holder.0.0 - expected).abs() > 1e-5 {
                failures.push(format!(
                    "left_hand={left_hand}: holding arm xRot {} is not `xRot * 0.5 - PI/10` \
                     ({expected}); the swing-discarding hypothesis would give {discarded}",
                    holder.0.0
                ));
            }
            if (expected - discarded).abs() < 1e-3 {
                failures.push(
                    "the walk swing is ~zero at this input, so the halving cannot be \
                     distinguished from discarding it"
                        .to_owned(),
                );
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
        assert!(!ArmPose::Item.is_two_handed(), "vanilla's item-hold pose is one-handed");
    }

    /// A zombie rig **loses** the pose, because `animate_zombie_arms` assigns over
    /// both arms afterwards — vanilla's own behaviour
    /// (its zombie pose setup calls the base humanoid pose setup then
    /// its zombie-arms animation unconditionally). Asserted rather than left implicit
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

    /// Vanilla's vex hand-transform function: `root · body · arm`, then `scale(0.55)`,
    /// then a small arm-signed translate. Vex's arm rig never rotates in this
    /// port (`AnimFamily::HeadOnly` runs no arm pose setup, and the rest pose
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

    /// Vanilla's allay hand-transform function never selects an arm by handedness at all: it reads
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
        // arm pose setup, zero authored rotation), so `right_arm.xRot` is
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

    /// A pose whose eighteen degree values are pairwise distinct, so no two
    /// parts can be exchanged without an assertion moving. Signs and magnitudes
    /// differ as well, so a mirrored assignment cannot pass either.
    fn a_distinct_pose() -> lodestone_model::ArmorStandPose {
        use lodestone_model::Vec3f;
        lodestone_model::ArmorStandPose {
            head: Vec3f::new(11.0, 12.0, 13.0),
            body: Vec3f::new(21.0, 22.0, 23.0),
            left_arm: Vec3f::new(31.0, 32.0, 33.0),
            right_arm: Vec3f::new(-41.0, -42.0, -43.0),
            left_leg: Vec3f::new(51.0, 52.0, 53.0),
            right_leg: Vec3f::new(-61.0, -62.0, -63.0),
        }
    }

    /// A walking, looking-around, mid-swing input — every term of the humanoid
    /// base pass switched on, so the assertions below are about the *overwrite*
    /// rather than about an input that happened to leave the joints alone.
    ///
    /// `limb_swing` is `7.3` rather than a round number on purpose: a phase that
    /// divides evenly into the walk frequency can make the leg terms coincide
    /// with each other, which would let a broken assignment pass for an innocent
    /// reason.
    fn a_walking_input() -> AnimInput {
        AnimInput {
            head_yaw_deg: 37.0,
            head_pitch_deg: -21.0,
            limb_swing: 7.3,
            limb_swing_amount: 1.0,
            age_ticks: 41.7,
            ..AnimInput::REST
        }
    }

    /// The armour stand is posed by the wire, not by the walk cycle:
    /// vanilla's armour-stand-armour pose setup runs the whole base humanoid pose setup
    /// and then **assigns** head, body, both arms and both legs from the stand's
    /// six pose accessors, and vanilla's armour-stand pose setup drives the three body
    /// sticks from the body pose on top.
    ///
    /// Nine exact predictions — `degrees * PI / 180`, derived from the wire's
    /// units and nothing in this crate — collected rather than asserted one at a
    /// time, so a transposition reports both halves of the swap instead of
    /// aborting on the first.
    #[test]
    fn an_armor_stands_pose_assigns_every_part_over_the_walk_cycle() {
        let skel = skeleton_for("armor_stand");
        assert_eq!(
            skel.family(),
            AnimFamily::Humanoid,
            "the premise: the stand classifies as a humanoid, so the base pass really does \
             swing its arms and legs — if this ever fails, the rest of this test is vacuous"
        );
        let pose = a_distinct_pose();
        let poses = skel.posed(&AnimInput {
            armor_stand_pose: Some(pose),
            ..a_walking_input()
        });
        let expected = [
            ("head", skel.slots.head, pose.head),
            ("body", skel.slots.body, pose.body),
            ("left_arm", skel.slots.left_arm, pose.left_arm),
            ("right_arm", skel.slots.right_arm, pose.right_arm),
            ("left_leg", skel.slots.left_leg, pose.left_leg),
            ("right_leg", skel.slots.right_leg, pose.right_leg),
            ("right_body_stick", skel.slots.right_body_stick, pose.body),
            ("left_body_stick", skel.slots.left_body_stick, pose.body),
            ("shoulder_stick", skel.slots.shoulder_stick, pose.body),
        ];
        let mut wrong = Vec::new();
        for (name, slot, want) in expected {
            let Some(i) = slot else {
                wrong.push(format!("{name}: the armour stand model has no such part"));
                continue;
            };
            let got = (poses[i].x_rot, poses[i].y_rot, poses[i].z_rot);
            let want = (want.x * DEG, want.y * DEG, want.z * DEG);
            if got != want {
                wrong.push(format!("{name}: got {got:?}, want {want:?}"));
            }
        }
        assert!(wrong.is_empty(), "armour-stand pose assignment mismatches:\n{}", wrong.join("\n"));
    }

    /// The control for the test above: with no pose the identical input really
    /// does swing the stand's limbs, so the assertions there are measuring an
    /// overwrite rather than an input that never moved anything.
    ///
    /// Both hypotheses are computed from outside constants and the measurement
    /// has to land on one of them: the walk value is
    /// `cos(limbSwing * 0.6662) * 1.4 * limbSwingAmount` from
    /// vanilla's humanoid pose setup, and the pose value is `1 degree` from
    /// vanilla's default right-leg armour-stand pose. At this phase they are nowhere near
    /// each other, which is what makes the pair discriminating.
    #[test]
    fn without_a_pose_the_same_input_swings_the_stands_limbs() {
        let skel = skeleton_for("armor_stand");
        let input = a_walking_input();
        let poses = skel.posed(&input);
        let leg = skel.slots.right_leg.expect("armour stand has a right leg");
        let walk_hypothesis = (input.limb_swing * WALK_FREQ).cos() * 1.4 * input.limb_swing_amount;
        let pose_hypothesis = 1.0 * DEG;
        assert!(
            (walk_hypothesis - pose_hypothesis).abs() > 0.1,
            "the two hypotheses must be far apart for this input to discriminate: \
             walk {walk_hypothesis}, pose {pose_hypothesis}"
        );
        assert_eq!(
            poses[leg].x_rot, walk_hypothesis,
            "with no pose the right leg must take the walk cycle exactly"
        );
        // And the head tracks, which the pose assignment also overwrites.
        let head = skel.slots.head.expect("armour stand has a head");
        assert_eq!(poses[head].y_rot, input.head_yaw_deg * DEG);
    }

    /// The case the reported defect was actually in: a stand nobody has posed.
    ///
    /// Vanilla's assignment is unconditional, and `ArmorStand`'s own `defineId`
    /// calls register a *non-zero* default pose, so an unposed stand overwrites
    /// the walk cycle with a small authored splay rather than keeping it. This
    /// is the arm that stops a stand carried by a moving contraption swinging
    /// like a running player; treating "no pose reported" as "leave the base
    /// pass alone" would pass every other test in this file and still ship the
    /// bug.
    #[test]
    fn the_vanilla_default_pose_also_replaces_the_walk_cycle() {
        let skel = skeleton_for("armor_stand");
        let input = AnimInput {
            armor_stand_pose: Some(lodestone_model::ArmorStandPose::VANILLA_DEFAULT),
            ..a_walking_input()
        };
        let poses = skel.posed(&input);
        let mut wrong = Vec::new();
        let expected = [
            // `ArmorStand.DEFAULT_*_POSE`, in degrees.
            ("left_arm", skel.slots.left_arm, (-10.0, 0.0, -10.0)),
            ("right_arm", skel.slots.right_arm, (-15.0, 0.0, 10.0)),
            ("left_leg", skel.slots.left_leg, (-1.0, 0.0, -1.0)),
            ("right_leg", skel.slots.right_leg, (1.0, 0.0, 1.0)),
            ("head", skel.slots.head, (0.0, 0.0, 0.0)),
            ("body", skel.slots.body, (0.0, 0.0, 0.0)),
        ];
        for (name, slot, (x, y, z)) in expected {
            let i = slot.unwrap_or_else(|| panic!("armour stand has a {name}"));
            let got = (poses[i].x_rot, poses[i].y_rot, poses[i].z_rot);
            let want = (x * DEG, y * DEG, z * DEG);
            if got != want {
                wrong.push(format!("{name}: got {got:?}, want {want:?}"));
            }
        }
        assert!(wrong.is_empty(), "default-pose mismatches:\n{}", wrong.join("\n"));
    }

    /// The assignment covers **rotations only**, exactly as vanilla's does, so
    /// the base pass's part *translations* survive underneath it.
    ///
    /// This is what makes "compute the walk cycle and overwrite it" different
    /// from "skip the walk cycle for this rig", which would be cheaper and would
    /// look equivalent: `setupAttackAnimation` moves the arms' `x`/`z` pivots as
    /// the torso twists, and nothing assigns those back. A type gate on the
    /// family would silently delete this motion.
    #[test]
    fn the_pose_assignment_leaves_the_base_passs_translations_alone() {
        let skel = skeleton_for("armor_stand");
        let arm = skel.slots.right_arm.expect("armour stand has a right arm");
        let swinging = AnimInput {
            attack_anim: 0.5,
            armor_stand_pose: Some(a_distinct_pose()),
            ..AnimInput::REST
        };
        let still = AnimInput {
            attack_anim: 0.0,
            ..swinging
        };
        let swung = skel.posed(&swinging);
        let rest = skel.posed(&still);
        assert_ne!(
            (swung[arm].x, swung[arm].z),
            (rest[arm].x, rest[arm].z),
            "the attack swing's arm orbit is a translation and must survive the pose assignment"
        );
        // While the rotations are identical in both, because the pose assigned
        // over whatever the swing left there.
        assert_eq!(
            (swung[arm].x_rot, swung[arm].y_rot, swung[arm].z_rot),
            (rest[arm].x_rot, rest[arm].y_rot, rest[arm].z_rot),
            "the pose must assign the same rotations regardless of the swing"
        );
    }

    /// A held item hangs off the *posed* arm, not off the walk cycle's arm.
    ///
    /// This is the half of the reported defect that made it obvious: the item
    /// swung. `Skeleton::translate_to_hand` re-derives the pose through the same
    /// `posed` call the body draw uses, so the pose reaches it for free — but
    /// "for free" is exactly the kind of claim that turns out to be false when a
    /// second code path re-implements the chain, so it is asserted rather than
    /// argued.
    #[test]
    fn a_posed_stands_held_item_follows_the_pose_not_the_walk_cycle() {
        let skel = skeleton_for("armor_stand");
        let walking = a_walking_input();
        let posed = AnimInput {
            armor_stand_pose: Some(a_distinct_pose()),
            ..walking
        };
        let hand_walking = skel
            .translate_to_hand(&walking, false, HandPoseOverride::Structural)
            .expect("armour stand has arms");
        let hand_posed = skel
            .translate_to_hand(&posed, false, HandPoseOverride::Structural)
            .expect("armour stand has arms");
        assert_ne!(
            hand_walking, hand_posed,
            "the hand matrix must follow the pose; if these agree, the item is still \
             hanging off the walk cycle's arm"
        );
        // And a posed stand's hand is invariant to the walk cycle entirely —
        // the property that actually stops the item swinging as the stand moves.
        let posed_still = AnimInput {
            limb_swing: 0.0,
            limb_swing_amount: 0.0,
            ..posed
        };
        assert_eq!(
            hand_posed,
            skel.translate_to_hand(&posed_still, false, HandPoseOverride::Structural)
                .expect("armour stand has arms"),
            "a posed stand's hand must not move with the walk cycle at all"
        );
    }
}
