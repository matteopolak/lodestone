//! Code-driven poses of the wolf, the fox and the felines (cat and ocelot): the
//! walk, sitting, lying, sleeping, crouching and pouncing, for adult and baby rigs;
//! and the adult axolotl's blend of swimming, hovering, crawling, lying still and
//! playing dead.
//!
//! These models are not keyframed. Their client poses each part from a handful of
//! per-entity facts: flags off the wire (sitting, sleeping, crouching, pouncing),
//! and amounts the client ramps every tick (a fox's crouch, a cat's lie-down and
//! relax). The shell keeps the ramps and fills a [`Posture`]; this module turns it
//! into part poses.
//!
//! A [`PostureRig`] **replaces** the family limb animation of
//! [`AnimFamily`](crate::entity_anim::AnimFamily): each of these models assigns its
//! own legs, tail and head, and the generic quadruped swing differs from the
//! feline's. A keyframed walk (the baby fox's) still runs first; the posture edits
//! land on top of it, and the head is assigned last, as the client does.
//!
//! The fixed offsets are tables of [`Edit`]s, one per state and rig, with every
//! number taken from the client's behaviour and checked part for part against a
//! JVM dump of the real pose setup (`tests/entities/posture_oracle.rs`).
//!
//! See `docs/entity-postures.md`.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};

use lodestone_assets::entity::{Affine, BakedPart, PartPose};

const DEG: f32 = PI / 180.0;

/// The walk cycle's angular frequency, shared by every four-legged pose setup.
const WALK_FREQ: f32 = 0.6662;

/// A wild wolf's tail angle, which is also its rest.
pub const WOLF_TAIL_WILD: f32 = PI / 5.0;
/// An angry wolf's tail angle: raised almost straight up.
pub const WOLF_TAIL_ANGRY: f32 = 1.539_380_4;

/// A tame wolf's tail angle from its health fraction: full health holds it at
/// `0.55 PI` and it droops by `0.4 PI` toward death.
#[must_use]
pub fn wolf_tame_tail_angle(health: f32, max_health: f32) -> f32 {
    let damage = (max_health - health) / max_health;
    (0.55 - damage * 0.4) * PI
}

/// The per-entity posture input, already interpolated for this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Posture {
    /// Sitting: a tamed wolf's or cat's sitting-pose bit, or a fox's sitting flag.
    pub sitting: bool,
    /// Crouching: a fox's crouch flag (stalking), or a feline's crouching pose.
    pub crouching: bool,
    /// A feline running: its tail streams out flat and the walk shortens.
    pub sprinting: bool,
    /// A fox asleep: lying on its side, legs tucked away, head turned back.
    pub sleeping: bool,
    /// A fox mid-pounce.
    pub pouncing: bool,
    /// A fox face first in the snow after a pounce.
    pub faceplanted: bool,
    /// A fox's crouch depth, `0..=5`, ramped while it crouches.
    pub crouch_amount: f32,
    /// A fox's head roll while it is interested in something, in radians.
    pub head_roll: f32,
    /// A cat's lie-down amount, `0..=1`.
    pub lie_down: f32,
    /// A cat's lie-down amount for the tail, which ramps more slowly.
    pub lie_down_tail: f32,
    /// A cat's relaxed head drop, `0..=1`.
    pub relax: f32,
    /// A wolf's tail pitch in radians: [`WOLF_TAIL_WILD`], [`WOLF_TAIL_ANGRY`] or
    /// [`wolf_tame_tail_angle`].
    pub tail_angle: f32,
    /// A wolf is angry: its tail stops wagging.
    pub angry: bool,
    /// An adult axolotl's eased state factors, each `0..=1`.
    pub axolotl: AxolotlFactors,
}

/// An adult axolotl's four state factors, each eased over ten ticks toward `1`
/// while its state holds and toward `0` otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AxolotlFactors {
    /// Playing dead.
    pub playing_dead: f32,
    /// In water (and not playing dead).
    pub in_water: f32,
    /// On the ground, out of water.
    pub on_ground: f32,
    /// Moving: walking, or its look changed this tick.
    pub moving: f32,
}

impl AxolotlFactors {
    /// No state reached yet: every factor zero, which is the client's start.
    pub const NONE: AxolotlFactors = AxolotlFactors { playing_dead: 0.0, in_water: 0.0, on_ground: 0.0, moving: 0.0 };
}

impl Posture {
    /// Standing, wild, nothing ramped.
    pub const NONE: Posture = Posture {
        sitting: false,
        crouching: false,
        sprinting: false,
        sleeping: false,
        pouncing: false,
        faceplanted: false,
        crouch_amount: 0.0,
        head_roll: 0.0,
        lie_down: 0.0,
        lie_down_tail: 0.0,
        relax: 0.0,
        tail_angle: WOLF_TAIL_WILD,
        angry: false,
        axolotl: AxolotlFactors::NONE,
    };
}

impl Default for Posture {
    fn default() -> Self {
        Self::NONE
    }
}

/// Which pose setup a rig runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Wolf.
    Wolf,
    /// Fox.
    Fox,
    /// Cat and ocelot.
    Feline,
    /// The adult axolotl (the baby is keyframed).
    Axolotl,
}

/// The posture kind and baby flag for a corpus model name, if it has a posture rig.
#[must_use]
pub fn posture_kind(model_name: &str) -> Option<(Kind, bool)> {
    Some(match model_name {
        "wolf" => (Kind::Wolf, false),
        "wolf_baby" => (Kind::Wolf, true),
        "fox" => (Kind::Fox, false),
        "fox_baby" => (Kind::Fox, true),
        "cat" | "ocelot" => (Kind::Feline, false),
        "cat_baby" | "ocelot_baby" => (Kind::Feline, true),
        "axolotl" => (Kind::Axolotl, false),
        _ => return None,
    })
}

/// A part a posture edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bone {
    Head,
    Body,
    UpperBody,
    Tail,
    Tail1,
    Tail2,
    RightHindLeg,
    LeftHindLeg,
    RightFrontLeg,
    LeftFrontLeg,
    TopGills,
    LeftGills,
    RightGills,
}

const BONES: [(Bone, &str); 13] = [
    (Bone::Head, "head"),
    (Bone::Body, "body"),
    (Bone::UpperBody, "upper_body"),
    (Bone::Tail, "tail"),
    (Bone::Tail1, "tail1"),
    (Bone::Tail2, "tail2"),
    (Bone::RightHindLeg, "right_hind_leg"),
    (Bone::LeftHindLeg, "left_hind_leg"),
    (Bone::RightFrontLeg, "right_front_leg"),
    (Bone::LeftFrontLeg, "left_front_leg"),
    (Bone::TopGills, "top_gills"),
    (Bone::LeftGills, "left_gills"),
    (Bone::RightGills, "right_gills"),
];

/// One change to one part.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    /// Move the pivot by this many model units, times the age scale (`0.5` on a baby rig).
    Shift([f32; 3]),
    /// Move the pivot by this many model units, the same on every rig.
    ShiftFixed([f32; 3]),
    /// Set the rotation about X.
    SetX(f32),
    /// Set the rotation about Y.
    SetY(f32),
    /// Set the rotation about Z.
    SetZ(f32),
    /// Turn about X by this much.
    TurnX(f32),
}

type Edit = (Bone, Op);

use Bone as B;
use Op::{SetX, SetY, SetZ, Shift, ShiftFixed, TurnX};

/// Sitting, shared by the adult and baby wolf: the haunches drop and fold under.
const WOLF_SIT: &[Edit] = &[
    (B::Body, Shift([0.0, 4.0, -2.0])),
    (B::Body, SetX(FRAC_PI_4)),
    (B::Tail, Shift([0.0, 9.0, -2.0])),
    (B::RightHindLeg, Shift([0.0, 6.7, -5.0])),
    (B::RightHindLeg, SetX(PI * 1.5)),
    (B::LeftHindLeg, Shift([0.0, 6.7, -5.0])),
    (B::LeftHindLeg, SetX(PI * 1.5)),
    (B::RightFrontLeg, SetX(5.811_947)),
    (B::RightFrontLeg, Shift([0.01, 1.0, 0.0])),
    (B::LeftFrontLeg, SetX(5.811_947)),
    (B::LeftFrontLeg, Shift([-0.01, 1.0, 0.0])),
];

/// The adult wolf's mane follows the body up.
const WOLF_ADULT_SIT: &[Edit] = &[
    (B::UpperBody, ShiftFixed([0.0, 2.0, 0.0])),
    (B::UpperBody, SetX(PI * 2.0 / 5.0)),
    (B::UpperBody, SetY(0.0)),
];

/// The baby wolf's body is authored level, so the shared sit leans it back.
const WOLF_BABY_SIT: &[Edit] = &[(B::Body, TurnX(-FRAC_PI_2))];

const FOX_ADULT_SIT: &[Edit] = &[
    (B::Body, SetX(PI / 6.0)),
    (B::Body, ShiftFixed([0.0, -7.0, 3.0])),
    (B::Tail, SetX(FRAC_PI_4)),
    (B::Head, ShiftFixed([0.0, -6.5, 2.75])),
    (B::RightFrontLeg, SetX(-PI / 12.0)),
    (B::LeftFrontLeg, SetX(-PI / 12.0)),
    (B::RightHindLeg, SetX(-PI * 5.0 / 12.0)),
    (B::RightHindLeg, ShiftFixed([0.0, 4.0, -0.25])),
    (B::LeftHindLeg, SetX(-PI * 5.0 / 12.0)),
    (B::LeftHindLeg, ShiftFixed([0.0, 4.0, -0.25])),
    (B::Tail, ShiftFixed([0.0, 0.0, -1.0])),
];

const FOX_BABY_SIT: &[Edit] = &[
    (B::Body, SetX(-0.959_931)),
    (B::Body, Shift([0.0, 3.0, -4.5])),
    (B::Tail, ShiftFixed([0.0, -0.6, 0.0])),
    (B::Tail, Shift([0.0, 0.0, -2.0])),
    (B::Tail, SetX(0.959_931_14)),
    (B::Head, ShiftFixed([0.0, -0.75, 0.0])),
    (B::RightFrontLeg, SetX(-PI / 12.0)),
    (B::LeftFrontLeg, SetX(-PI / 12.0)),
    (B::RightFrontLeg, ShiftFixed([0.01, 0.0, -1.5])),
    (B::LeftFrontLeg, ShiftFixed([-0.01, 0.0, -1.5])),
    (B::RightHindLeg, ShiftFixed([0.01, 0.0, -3.75])),
    (B::LeftHindLeg, ShiftFixed([-0.01, 0.0, -3.75])),
];

const FOX_ADULT_SLEEP: &[Edit] = &[
    (B::Body, SetZ(-FRAC_PI_2)),
    (B::Body, ShiftFixed([0.0, 5.0, 0.0])),
    (B::Tail, SetX(-PI * 5.0 / 6.0)),
    (B::Head, ShiftFixed([2.0, 2.99, 0.0])),
];

const FOX_BABY_SLEEP: &[Edit] = &[
    (B::Body, SetZ(-FRAC_PI_2)),
    (B::Body, SetX(-PI / 18.0)),
    (B::Body, ShiftFixed([-1.5, 1.5, -1.5])),
    (B::Tail, SetX(-2.181_661_6)),
    (B::Tail, ShiftFixed([-0.7, 0.9, 0.6])),
    (B::Head, ShiftFixed([-2.0, 2.8, -4.0])),
];

const FELINE_CROUCH: &[Edit] = &[
    (B::Body, Shift([0.0, 1.0, 0.0])),
    (B::Head, Shift([0.0, 2.0, 0.0])),
    (B::Tail1, Shift([0.0, 1.0, 0.0])),
    (B::Tail2, Shift([0.0, -4.0, 2.0])),
    (B::Tail1, SetX(FRAC_PI_2)),
    (B::Tail2, SetX(FRAC_PI_2)),
];

const FELINE_ADULT_SIT: &[Edit] = &[
    (B::Body, SetX(FRAC_PI_4)),
    (B::Body, Shift([0.0, -4.0, 5.0])),
    (B::Head, Shift([0.0, -3.3, 1.0])),
    (B::Tail1, Shift([0.0, 8.0, -2.0])),
    (B::Tail2, Shift([0.0, 2.0, -0.8])),
    (B::Tail1, SetX(1.727_876_1)),
    (B::Tail2, SetX(2.670_354)),
    (B::LeftFrontLeg, SetX(-PI / 20.0)),
    (B::LeftFrontLeg, Shift([0.0, 2.0, -2.0])),
    (B::RightFrontLeg, SetX(-PI / 20.0)),
    (B::RightFrontLeg, Shift([0.0, 2.0, -2.0])),
    (B::LeftHindLeg, SetX(-FRAC_PI_2)),
    (B::LeftHindLeg, Shift([0.0, 3.0, -4.0])),
    (B::RightHindLeg, SetX(-FRAC_PI_2)),
    (B::RightHindLeg, Shift([0.0, 3.0, -4.0])),
];

const FELINE_BABY_SIT: &[Edit] = &[
    (B::Body, TurnX(-0.436_332_32)),
    (B::Body, ShiftFixed([0.0, 1.25, 0.0])),
    (B::Head, ShiftFixed([0.0, 0.0, 0.75])),
    (B::Tail1, TurnX(0.545_415_4)),
    (B::Tail1, ShiftFixed([0.0, 4.0, -0.9])),
    (B::LeftHindLeg, ShiftFixed([0.0, 0.0, -0.9])),
    (B::RightHindLeg, ShiftFixed([0.0, 0.0, -0.9])),
];

/// The adult feline's legs on its side, applied whenever the lie-down amount is
/// positive (the body roll itself is the whole-model turn, [`PostureRig::root`]).
const FELINE_ADULT_LIE: &[Edit] = &[
    (B::LeftFrontLeg, SetX(-1.270_796_3)),
    (B::RightFrontLeg, SetX(-0.470_796_35)),
    (B::RightFrontLeg, SetZ(-0.2)),
    (B::RightFrontLeg, Shift([1.0, 0.0, 0.0])),
    (B::LeftHindLeg, SetX(-0.4)),
    (B::RightHindLeg, SetX(0.5)),
    (B::RightHindLeg, SetZ(-0.5)),
    (B::RightHindLeg, Shift([0.8, 2.0, 0.0])),
];

const FELINE_BABY_LIE: &[Edit] = &[
    (B::Body, ShiftFixed([1.0, 0.0, 0.0])),
    (B::Head, ShiftFixed([1.5, 0.75, -0.5])),
    (B::RightFrontLeg, SetX(-FRAC_PI_4)),
    (B::RightFrontLeg, ShiftFixed([3.5, -0.5, 0.0])),
    (B::LeftFrontLeg, SetX(-FRAC_PI_2)),
    (B::LeftFrontLeg, ShiftFixed([1.5, -1.0, -2.0])),
    (B::RightHindLeg, SetX(PI * 2.0 / 9.0)),
    (B::RightHindLeg, SetY(PI / 9.0)),
    (B::RightHindLeg, SetZ(-PI / 9.0)),
    (B::RightHindLeg, ShiftFixed([2.5, -0.25, 0.5])),
    (B::LeftHindLeg, ShiftFixed([1.5, 0.0, -1.0])),
    (B::Tail1, ShiftFixed([1.0, 0.5, -0.25])),
];

/// The client's table sine and cosine, which the axolotl's sways read.
fn mth_sin(x: f32) -> f32 {
    lodestone_physics::mth::sin(f64::from(x))
}

fn mth_cos(x: f32) -> f32 {
    lodestone_physics::mth::cos(f64::from(x))
}

/// The resting tail pitch both feline walks swing about.
const FELINE_TAIL_REST: f32 = 1.727_876_1;

/// A rotation lerp that wraps the difference into `[-180, 180)` **as if it were
/// degrees**, which is what the client does to these radian angles. Every
/// difference here is a few radians, far inside the wrap, so it reduces to a lerp.
fn rot_lerp(alpha: f32, from: f32, to: f32) -> f32 {
    let mut diff = (to - from) % 360.0;
    if diff >= 180.0 {
        diff -= 360.0;
    }
    if diff < -180.0 {
        diff += 360.0;
    }
    from + alpha * diff
}

/// A posture kind resolved against one skeleton's part order.
#[derive(Debug, Clone, PartialEq)]
pub struct PostureRig {
    kind: Kind,
    baby: bool,
    bones: [Option<usize>; BONES.len()],
}

impl PostureRig {
    /// Resolves the posture rig for `model_name` against `parts`, if it has one.
    #[must_use]
    pub fn for_model(model_name: &str, parts: &[BakedPart]) -> Option<Self> {
        let (kind, baby) = posture_kind(model_name)?;
        let mut bones = [None; BONES.len()];
        for (slot, (_, name)) in bones.iter_mut().zip(BONES) {
            *slot = parts.iter().position(|p| p.name == name);
        }
        Some(PostureRig { kind, baby, bones })
    }

    /// The kind of pose setup this rig runs.
    #[must_use]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    fn age_scale(&self) -> f32 {
        if self.baby { 0.5 } else { 1.0 }
    }

    fn bone(&self, bone: Bone) -> Option<usize> {
        self.bones[bone as usize]
    }

    fn edit(&self, poses: &mut [PartPose], edits: &[Edit]) {
        let scale = self.age_scale();
        for &(bone, op) in edits {
            let Some(i) = self.bone(bone) else { continue };
            let pose = &mut poses[i];
            match op {
                Shift([x, y, z]) => {
                    pose.x += x * scale;
                    pose.y += y * scale;
                    pose.z += z * scale;
                }
                ShiftFixed([x, y, z]) => {
                    pose.x += x;
                    pose.y += y;
                    pose.z += z;
                }
                SetX(v) => pose.x_rot = v,
                SetY(v) => pose.y_rot = v,
                SetZ(v) => pose.z_rot = v,
                TurnX(v) => pose.x_rot += v,
            }
        }
    }

    fn with(&self, poses: &mut [PartPose], bone: Bone, f: impl FnOnce(&mut PartPose)) {
        if let Some(i) = self.bone(bone) {
            f(&mut poses[i]);
        }
    }

    /// Poses `poses` (at rest, or after a keyframed walk) for this frame.
    pub fn apply(
        &self,
        poses: &mut [PartPose],
        look: [f32; 2],
        limb: [f32; 2],
        age_ticks: f32,
        posture: &Posture,
    ) {
        match self.kind {
            Kind::Wolf => self.apply_wolf(poses, look, limb, posture),
            Kind::Fox => self.apply_fox(poses, look, limb, age_ticks, posture),
            Kind::Feline => self.apply_feline(poses, look, limb, posture),
            Kind::Axolotl => self.apply_axolotl(poses, look, age_ticks, &posture.axolotl),
        }
    }

    /// The four-legged walk: diagonal pairs swing together.
    fn trot(&self, poses: &mut [PartPose], limb: [f32; 2]) {
        let [pos, amount] = limb;
        let swing = |phase: f32| (pos * WALK_FREQ + phase).cos() * 1.4 * amount;
        self.with(poses, B::RightHindLeg, |p| p.x_rot = swing(0.0));
        self.with(poses, B::LeftHindLeg, |p| p.x_rot = swing(PI));
        self.with(poses, B::RightFrontLeg, |p| p.x_rot = swing(PI));
        self.with(poses, B::LeftFrontLeg, |p| p.x_rot = swing(0.0));
    }

    fn look(&self, poses: &mut [PartPose], look: [f32; 2]) {
        let [yaw, pitch] = look;
        self.with(poses, B::Head, |p| {
            p.x_rot = pitch * DEG;
            p.y_rot = yaw * DEG;
        });
    }

    fn apply_wolf(&self, poses: &mut [PartPose], look: [f32; 2], limb: [f32; 2], posture: &Posture) {
        let [pos, amount] = limb;
        let wag = if posture.angry { 0.0 } else { (pos * WALK_FREQ).cos() * 1.4 * amount };
        self.with(poses, B::Tail, |p| p.y_rot = wag);
        if posture.sitting {
            self.edit(poses, WOLF_SIT);
            self.edit(poses, if self.baby { WOLF_BABY_SIT } else { WOLF_ADULT_SIT });
        } else {
            self.trot(poses, limb);
        }
        self.look(poses, look);
        self.with(poses, B::Tail, |p| p.x_rot = posture.tail_angle);
    }

    fn apply_fox(
        &self,
        poses: &mut [PartPose],
        look: [f32; 2],
        limb: [f32; 2],
        age_ticks: f32,
        posture: &Posture,
    ) {
        let scale = self.age_scale();
        // The roll is set before the baby's keyframed walk adds to it, and both fox
        // heads rest level, so adding it to whatever the walk left is the same thing.
        self.with(poses, B::Head, |p| p.z_rot += posture.head_roll);
        if !self.baby {
            self.trot(poses, limb);
        }
        if posture.crouching {
            let wiggle = age_ticks.cos() * 0.05;
            let body_drop = if self.baby { posture.crouch_amount / 6.0 } else { posture.crouch_amount };
            self.with(poses, B::Body, |p| {
                p.x_rot += 0.104_719_76;
                p.y_rot = wiggle;
                p.y += body_drop;
            });
            self.with(poses, B::Head, |p| p.y += posture.crouch_amount * scale);
            self.with(poses, B::RightHindLeg, |p| p.z_rot = wiggle);
            self.with(poses, B::LeftHindLeg, |p| p.z_rot = wiggle);
            self.with(poses, B::RightFrontLeg, |p| p.z_rot = wiggle / 2.0);
            self.with(poses, B::LeftFrontLeg, |p| p.z_rot = wiggle / 2.0);
        } else if posture.sleeping {
            self.edit(poses, if self.baby { FOX_BABY_SLEEP } else { FOX_ADULT_SLEEP });
        } else if posture.sitting {
            self.with(poses, B::Head, |p| {
                p.x_rot = 0.0;
                p.y_rot = 0.0;
            });
            self.edit(poses, if self.baby { FOX_BABY_SIT } else { FOX_ADULT_SIT });
        }
        if posture.pouncing && !self.baby {
            let half = posture.crouch_amount / 2.0;
            self.with(poses, B::Body, |p| p.y -= half);
            self.with(poses, B::Head, |p| p.y -= half);
        }
        if posture.sleeping {
            // Curled up, the head turns back along the body and breathes.
            self.with(poses, B::Head, |p| {
                p.x_rot = 0.0;
                p.y_rot = -PI * 2.0 / 3.0;
                p.z_rot = (age_ticks * 0.027).cos() / 22.0;
            });
        } else if !posture.faceplanted && !posture.crouching {
            self.look(poses, look);
        }
    }

    fn apply_feline(&self, poses: &mut [PartPose], look: [f32; 2], limb: [f32; 2], posture: &Posture) {
        let scale = self.age_scale();
        if posture.crouching {
            self.edit(poses, FELINE_CROUCH);
        } else if posture.sprinting {
            let tail1_y = self.bone(B::Tail1).map(|i| poses[i].y);
            self.with(poses, B::Tail2, |p| {
                if let Some(y) = tail1_y {
                    p.y = y;
                }
                p.z += 2.0 * scale;
                p.x_rot = FRAC_PI_2;
            });
            self.with(poses, B::Tail1, |p| p.x_rot = FRAC_PI_2);
        }
        self.look(poses, look);
        if posture.sitting {
            self.edit(poses, if self.baby { FELINE_BABY_SIT } else { FELINE_ADULT_SIT });
        } else {
            if !self.baby {
                self.with(poses, B::Body, |p| p.x_rot = FRAC_PI_2);
            }
            let [pos, amount] = limb;
            let swing = |phase: f32| (pos * WALK_FREQ + phase).cos() * amount;
            // A running cat's hind legs and front legs fall a little out of step.
            let lag = if posture.sprinting { 0.3 } else { PI };
            let (left_hind, right_hind, left_front, right_front) = if posture.sprinting {
                (swing(0.0), swing(lag), swing(PI + 0.3), swing(PI))
            } else {
                (swing(0.0), swing(PI), swing(PI), swing(0.0))
            };
            self.with(poses, B::LeftHindLeg, |p| p.x_rot = left_hind);
            self.with(poses, B::RightHindLeg, |p| p.x_rot = right_hind);
            self.with(poses, B::LeftFrontLeg, |p| p.x_rot = left_front);
            self.with(poses, B::RightFrontLeg, |p| p.x_rot = right_front);
            let tail_swing = if posture.sprinting {
                PI / 10.0
            } else if posture.crouching {
                0.471_238_94
            } else {
                FRAC_PI_4
            };
            self.with(poses, B::Tail2, |p| p.x_rot = FELINE_TAIL_REST + tail_swing * pos.cos() * amount);
        }
        let lie = posture.lie_down;
        let tail = posture.lie_down_tail;
        if lie > 0.0 {
            if self.baby {
                self.edit(poses, FELINE_BABY_LIE);
                self.with(poses, B::Head, |p| {
                    p.x_rot = rot_lerp(lie, p.x_rot, PI / 18.0);
                    p.z_rot = rot_lerp(lie, p.z_rot, -PI * 5.0 / 12.0);
                });
                // The baby's tail adds the lerped angle to itself, doubling it on the way.
                self.with(poses, B::Tail1, |p| {
                    p.x_rot += rot_lerp(tail, p.x_rot, -PI / 6.0);
                    p.y_rot += rot_lerp(tail, p.y_rot, 0.0);
                    p.z_rot += rot_lerp(tail, p.z_rot, -PI / 18.0);
                });
            } else {
                self.with(poses, B::Head, |p| {
                    p.z_rot = rot_lerp(lie, p.z_rot, -1.270_796_3);
                    p.y_rot = rot_lerp(lie, p.y_rot, 1.270_796_3);
                });
                self.edit(poses, FELINE_ADULT_LIE);
                self.with(poses, B::Tail1, |p| p.x_rot = rot_lerp(tail, p.x_rot, 0.8));
                self.with(poses, B::Tail2, |p| p.x_rot = rot_lerp(tail, p.x_rot, -0.4));
            }
        }
        if posture.relax > 0.0 {
            self.with(poses, B::Head, |p| p.x_rot = rot_lerp(posture.relax, p.x_rot, -0.581_776_44));
        }
    }

    /// The adult axolotl: each state's motion added in proportion to its factor, the
    /// body turned by the look's yaw, and the right legs mirroring the left ones
    /// except while crawling.
    fn apply_axolotl(&self, poses: &mut [PartPose], look: [f32; 2], age: f32, f: &AxolotlFactors) {
        let still = 1.0 - f.moving;
        let mirrored = 1.0 - f.on_ground.min(f.moving);
        self.with(poses, B::Body, |p| p.y_rot += look[0] * DEG);
        self.axolotl_swim(poses, age, look[1], f.moving.min(f.in_water));
        self.axolotl_hover(poses, age, still.min(f.in_water));
        self.axolotl_crawl(poses, age, f.moving.min(f.on_ground));
        self.axolotl_lie_still(poses, age, still.min(f.on_ground));
        if f.playing_dead > 1.0e-5 {
            let k = f.playing_dead;
            self.with(poses, B::LeftHindLeg, |p| {
                p.x_rot += 1.413_716_7 * k;
                p.y_rot += 1.099_557_4 * k;
                p.z_rot += FRAC_PI_4 * k;
            });
            self.with(poses, B::LeftFrontLeg, |p| {
                p.x_rot += FRAC_PI_4 * k;
                p.y_rot += 2.042_035 * k;
            });
            self.with(poses, B::Body, |p| {
                p.x_rot += -0.15 * k;
                p.z_rot += 0.35 * k;
            });
        }
        if mirrored > 1.0e-5 {
            for (right, left) in [(B::RightHindLeg, B::LeftHindLeg), (B::RightFrontLeg, B::LeftFrontLeg)] {
                let Some(l) = self.bone(left).map(|i| poses[i]) else { continue };
                self.with(poses, right, |p| {
                    p.x_rot += l.x_rot * mirrored;
                    p.y_rot += -l.y_rot * mirrored;
                    p.z_rot += -l.z_rot * mirrored;
                });
            }
        }
    }

    /// The gills fan out by `angle`: the top set pitches, the side sets turn apart.
    fn axolotl_gills(&self, poses: &mut [PartPose], top: f32, side: f32) {
        self.with(poses, B::TopGills, |p| p.x_rot += top);
        self.with(poses, B::LeftGills, |p| p.y_rot += side);
        self.with(poses, B::RightGills, |p| p.y_rot -= side);
    }

    fn axolotl_swim(&self, poses: &mut [PartPose], age: f32, pitch_deg: f32, k: f32) {
        if k <= 1.0e-5 {
            return;
        }
        let t = age * 0.33;
        let (sin, cos) = (mth_sin(t), mth_cos(t));
        let sway = 0.13 * sin;
        self.with(poses, B::Body, |p| {
            p.x_rot += (pitch_deg * DEG + sway) * k;
            p.y -= 0.45 * cos * k;
        });
        self.with(poses, B::Head, |p| p.x_rot -= sway * 1.8 * k);
        self.axolotl_gills(poses, (-0.5 * sin - 0.8) * k, (0.3 * sin + 0.9) * k);
        self.with(poses, B::Tail, |p| p.y_rot += 0.3 * mth_cos(t * 0.9) * k);
        self.with(poses, B::LeftHindLeg, |p| {
            p.x_rot += 1.884_955_8 * k;
            p.y_rot += -0.4 * sin * k;
            p.z_rot += FRAC_PI_2 * k;
        });
        self.with(poses, B::LeftFrontLeg, |p| {
            p.x_rot += 1.884_955_8 * k;
            p.y_rot += (-0.2 * cos - 0.1) * k;
            p.z_rot += FRAC_PI_2 * k;
        });
    }

    fn axolotl_hover(&self, poses: &mut [PartPose], age: f32, k: f32) {
        if k <= 1.0e-5 {
            return;
        }
        let t = age * 0.075;
        let cos = mth_cos(t);
        let bob = mth_sin(t) * 0.15;
        let body_pitch = (-0.15 + 0.075 * cos) * k;
        self.with(poses, B::Body, |p| {
            p.x_rot += body_pitch;
            p.y -= bob * k;
        });
        self.with(poses, B::Head, |p| p.x_rot -= body_pitch);
        self.axolotl_gills(poses, 0.2 * cos * k, (-0.3 * cos - 0.19) * k);
        self.with(poses, B::LeftHindLeg, |p| {
            p.x_rot += (PI * 3.0 / 4.0 - cos * 0.11) * k;
            p.y_rot += 0.471_238_94 * k;
            p.z_rot += 1.727_876_1 * k;
        });
        self.with(poses, B::LeftFrontLeg, |p| {
            p.x_rot += (FRAC_PI_4 - cos * 0.2) * k;
            p.y_rot += 2.042_035 * k;
        });
        self.with(poses, B::Tail, |p| p.y_rot += 0.5 * cos * k);
    }

    fn axolotl_crawl(&self, poses: &mut [PartPose], age: f32, k: f32) {
        if k <= 1.0e-5 {
            return;
        }
        let t = age * 0.11;
        let cos = mth_cos(t);
        let hind_sway = (cos * cos - 2.0 * cos) / 5.0;
        let front_sway = 0.7 * cos;
        let turn = 0.09 * cos * k;
        self.with(poses, B::Head, |p| p.y_rot += turn);
        self.with(poses, B::Tail, |p| p.y_rot += turn);
        let gills = (0.6 - 0.08 * (cos * cos + 2.0 * mth_sin(t))) * k;
        self.axolotl_gills(poses, gills, -gills);
        let (hind, front) = (0.942_477_9 * k, 1.099_557_4 * k);
        self.with(poses, B::LeftHindLeg, |p| {
            p.x_rot += hind;
            p.y_rot += (1.5 - hind_sway) * k;
            p.z_rot += -0.1 * k;
        });
        self.with(poses, B::LeftFrontLeg, |p| {
            p.x_rot += front;
            p.y_rot += (FRAC_PI_2 - front_sway) * k;
        });
        self.with(poses, B::RightHindLeg, |p| {
            p.x_rot += hind;
            p.y_rot += (-1.0 - hind_sway) * k;
        });
        self.with(poses, B::RightFrontLeg, |p| {
            p.x_rot += front;
            p.y_rot += (-FRAC_PI_2 - front_sway) * k;
        });
    }

    fn axolotl_lie_still(&self, poses: &mut [PartPose], age: f32, k: f32) {
        if k <= 1.0e-5 {
            return;
        }
        let t = age * 0.09;
        let (sin, cos) = (mth_sin(t), mth_cos(t));
        let movement = sin * sin - 2.0 * sin;
        let movement2 = cos * cos - 3.0 * sin;
        self.with(poses, B::Head, |p| {
            p.x_rot += -0.09 * movement * k;
            p.z_rot += -0.2 * k;
        });
        self.with(poses, B::Tail, |p| p.y_rot += (-0.1 + 0.1 * movement) * k);
        let gills = (0.6 + 0.05 * movement2) * k;
        self.axolotl_gills(poses, gills, -gills);
        self.with(poses, B::LeftHindLeg, |p| {
            p.x_rot += 1.1 * k;
            p.y_rot += 1.0 * k;
        });
        self.with(poses, B::LeftFrontLeg, |p| {
            p.x_rot += 0.8 * k;
            p.y_rot += 2.3 * k;
            p.z_rot -= 0.5 * k;
        });
    }

    /// Parts whose own geometry is hidden: a sleeping fox's legs.
    pub fn hidden(&self, posture: &Posture) -> impl Iterator<Item = usize> + '_ {
        let legs = [B::RightHindLeg, B::LeftHindLeg, B::RightFrontLeg, B::LeftFrontLeg];
        let hide = self.kind == Kind::Fox && posture.sleeping && !posture.crouching;
        legs.into_iter().filter(move |_| hide).filter_map(|b| self.bone(b))
    }

    /// The whole-model turn this posture applies before the model's own root, in
    /// model space: a lying cat rolls onto its side, and a pouncing or face-planted
    /// fox pitches with its look.
    ///
    /// The client applies these in the entity's own frame, after the body yaw and
    /// before the model's mirror and ground lift. Model space is that frame mirrored
    /// in X and Y and lifted by [`MODEL_FEET_OFFSET`](crate::entity::MODEL_FEET_OFFSET),
    /// so the turn is conjugated by that mirror and lift.
    #[must_use]
    pub fn root(&self, posture: &Posture, head_pitch_deg: f32) -> Option<Affine> {
        let entity_turn = match self.kind {
            Kind::Feline if posture.lie_down > 0.0 => {
                let lie = posture.lie_down;
                let angle = rot_lerp(lie, 0.0, 90.0) * DEG;
                glam::Mat4::from_translation(glam::Vec3::new(0.4 * lie, 0.15 * lie, 0.1 * lie))
                    * glam::Mat4::from_rotation_z(angle)
            }
            Kind::Fox if posture.pouncing || posture.faceplanted => {
                glam::Mat4::from_rotation_x(-head_pitch_deg * DEG)
            }
            _ => return None,
        };
        let model_to_entity = glam::Mat4::from_scale(glam::Vec3::new(-1.0, -1.0, 1.0))
            * glam::Mat4::from_translation(glam::Vec3::new(0.0, -crate::entity::MODEL_FEET_OFFSET, 0.0));
        let m = model_to_entity.inverse() * entity_turn * model_to_entity;
        let c = m.to_cols_array_2d();
        Some(Affine {
            m: [[c[0][0], c[1][0], c[2][0]], [c[0][1], c[1][1], c[2][1]], [c[0][2], c[1][2], c[2][2]]],
            t: [c[3][0], c[3][1], c[3][2]],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig(name: &str) -> PostureRig {
        let (kind, baby) = posture_kind(name).unwrap();
        PostureRig { kind, baby, bones: [None; BONES.len()] }
    }

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1.0e-4)
    }

    /// A fully lain-down cat. In the entity's frame the client turns the model 90
    /// degrees about Z and then moves it by (0.4, 0.15, 0.1). The model origin sits at
    /// (0, 1.501, 0) in that frame, turns to (-1.501, 0, 0) and moves to
    /// (-1.101, 0.15, 0.1); mirrored back into model space (x and y negated, y lifted
    /// by 1.501) that is (1.101, 1.351, 0.1). Model +X sits at (-1, 1.501, 0), turns to
    /// (-1.501, -1, 0), moves to (-1.101, -0.85, 0.1), and lands at (1.101, 2.351, 0.1).
    #[test]
    fn a_lying_cat_rolls_onto_its_side_about_the_entity_frame() {
        let posture = Posture { lie_down: 1.0, ..Posture::NONE };
        let turn = rig("cat").root(&posture, 0.0).expect("a lying cat turns");
        assert!(near(turn.apply([0.0, 0.0, 0.0]), [1.101, 1.351, 0.1]), "{:?}", turn.apply([0.0; 3]));
        assert!(near(turn.apply([1.0, 0.0, 0.0]), [1.101, 2.351, 0.1]), "{:?}", turn.apply([1.0, 0.0, 0.0]));
        // Controls: standing cats and wolves do not turn.
        assert!(rig("cat").root(&Posture::NONE, 0.0).is_none());
        assert!(rig("wolf").root(&posture, 0.0).is_none());
    }

    /// A pouncing fox pitched 30 degrees: the client turns the entity frame by -30
    /// degrees about X. The model origin at (0, 1.501, 0) goes to
    /// (0, 1.501 cos 30, -1.501 sin 30) = (0, 1.29990, -0.7505), which is
    /// (0, 0.20110, -0.7505) in model space.
    #[test]
    fn a_pouncing_fox_pitches_with_its_look() {
        let posture = Posture { pouncing: true, ..Posture::NONE };
        let turn = rig("fox").root(&posture, 30.0).expect("a pouncing fox turns");
        assert!(near(turn.apply([0.0; 3]), [0.0, 0.201_10, -0.7505]), "{:?}", turn.apply([0.0; 3]));
        assert!(rig("fox").root(&Posture::NONE, 30.0).is_none(), "control: a standing fox does not");
    }

    #[test]
    fn a_tame_wolf_tail_droops_with_its_health() {
        assert!((wolf_tame_tail_angle(40.0, 40.0) - 0.55 * PI).abs() < 1.0e-6);
        assert!((wolf_tame_tail_angle(20.0, 40.0) - 0.35 * PI).abs() < 1.0e-6);
    }
}
