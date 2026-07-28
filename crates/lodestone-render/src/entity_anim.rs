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
//! Trigonometry here uses `f32::sin`/`cos` rather than vanilla's 65536-entry
//! `Mth` lookup table. That is a deliberate exception to the project's
//! bit-exactness rule: limb angles are never sent to a server and never feed
//! physics, so a sub-degree difference is invisible and unobservable. Anything
//! that *is* transmitted must still use the parity table.

use glam::Mat4;
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
    /// which raises a zombie's arms from `-PI/2.25` to `-PI/1.5`. Nothing
    /// decodes the shared-flags bit that carries it yet, so the shell always
    /// passes `false` — the non-aggressive branch is the pose a player sees on
    /// an idle or merely-walking zombie, and it is the one that was missing.
    pub aggressive: bool,
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
    };
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
        let mut poses: Vec<PartPose> = self.parts.iter().map(|p| p.rest).collect();
        self.setup_anim(&mut poses, input);
        self.compose(&poses)
    }

    /// The unanimated matrices — what a [`AnimFamily::Static`] model draws with,
    /// and the baseline an animated one is compared against in tests.
    #[must_use]
    pub fn rest_pose(&self) -> Vec<Mat4> {
        let poses: Vec<PartPose> = self.parts.iter().map(|p| p.rest).collect();
        self.compose(&poses)
    }

    /// Walks the hierarchy composing each part's transform onto its parent's.
    ///
    /// A single forward pass suffices because [`bake_entity_parts`] emits parts
    /// in pre-order, so a parent's chain is always already computed.
    ///
    /// [`bake_entity_parts`]: lodestone_assets::entity::bake_entity_parts
    fn compose(&self, poses: &[PartPose]) -> Vec<Mat4> {
        let mut chains: Vec<Affine> = Vec::with_capacity(self.parts.len());
        let mut out = Vec::with_capacity(self.parts.len());
        for (i, part) in self.parts.iter().enumerate() {
            let parent = part.parent.map_or(Affine::IDENTITY, |p| chains[p]);
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
}
