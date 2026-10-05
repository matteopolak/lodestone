//! Keyframe-driven rigs: which animations a model plays, what starts them, and how
//! they pose a [`Skeleton`](crate::entity_anim::Skeleton).
//!
//! A [`RigSpec`] is the per-model table: the animations each [`Slot`] plays, the
//! walk cycles keyed on the limb swing, the head-tracking rule and the parts hidden
//! by state. The per-entity input is [`Keyframes`]: for each slot the milliseconds
//! since that animation started (or stopped), plus a few state flags. The shell fills
//! it from the entity's animation timers; this module only reads it.
//!
//! A keyframed rig **replaces** the family limb animation of
//! [`AnimFamily`](crate::entity_anim::AnimFamily), because a hopping rabbit or a
//! walking camel is not a four-legged swing. Head tracking is part of the spec
//! because these models each assign their head differently.
//!
//! See `docs/keyframe-animation.md`.

use lodestone_assets::entity::{BakedPart, PartPose};
use lodestone_assets::keyframe::{Anim, ChannelDef, Target};

/// How far, in blocks, a keyframed model's culling box grows past its rest pose on every side.
pub const KEYFRAME_BOUNDS_PAD: f32 = 0.5;

const DEG: f32 = std::f32::consts::PI / 180.0;

/// One independently timed animation state of an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The rabbit's hop.
    Hop,
    /// The rabbit's idle head tilt.
    IdleHeadTilt,
    /// The bat's flight loop.
    Fly,
    /// The bat's roost.
    Rest,
    /// The frog's leap.
    Jump,
    /// The frog's croak.
    Croak,
    /// The frog's tongue strike.
    Tongue,
    /// The frog's idle bob in water.
    SwimIdle,
    /// The camel's sitting down.
    Sit,
    /// The camel's seated hold.
    SitPose,
    /// The camel's standing up.
    SitUp,
    /// The camel's occasional idle.
    Idle,
    /// The camel's dash.
    Dash,
    /// The armadillo unrolling.
    RollOut,
    /// The armadillo rolling up.
    RollUp,
    /// The armadillo peeking out of its shell.
    Peek,
    /// The sniffer digging.
    Dig,
    /// The sniffer's long sniff.
    LongSniff,
    /// The sniffer standing up from a dig.
    Rise,
    /// The sniffer's happy wiggle.
    Happy,
    /// The sniffer's short scenting sniffs.
    Scent,
}

/// Every slot, in declaration order.
pub const ALL_SLOTS: [Slot; SLOT_COUNT] = [
    Slot::Hop,
    Slot::IdleHeadTilt,
    Slot::Fly,
    Slot::Rest,
    Slot::Jump,
    Slot::Croak,
    Slot::Tongue,
    Slot::SwimIdle,
    Slot::Sit,
    Slot::SitPose,
    Slot::SitUp,
    Slot::Idle,
    Slot::Dash,
    Slot::RollOut,
    Slot::RollUp,
    Slot::Peek,
    Slot::Dig,
    Slot::LongSniff,
    Slot::Rise,
    Slot::Happy,
    Slot::Scent,
];

/// How many slots exist.
pub const SLOT_COUNT: usize = 21;

impl Slot {
    const fn index(self) -> usize {
        self as usize
    }
}

/// The value of a stopped slot.
const STOPPED: i32 = i32::MIN;

/// A boolean the rig's animations or hidden parts depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    /// The frog is in water: its walk is the swim cycle.
    Swimming,
    /// The armadillo is curled in its shell.
    Hiding,
    /// The sniffer is searching: its walk is the search cycle.
    Searching,
    /// The bat is roosting.
    Resting,
}

impl Flag {
    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A condition on the per-entity input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    /// Always true.
    Always,
    /// The flag is set.
    Is(Flag),
    /// The flag is clear.
    IsNot(Flag),
    /// The slot's animation is running.
    Started(Slot),
    /// The slot's animation is stopped.
    Stopped(Slot),
}

/// The per-entity keyframe input, already evaluated for this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keyframes {
    millis: [i32; SLOT_COUNT],
    flags: u8,
}

impl Keyframes {
    /// Nothing running, no flags: the pose a keyframed rig shows at rest.
    pub const NONE: Keyframes = Keyframes { millis: [STOPPED; SLOT_COUNT], flags: 0 };

    /// `slot` running, `millis` since it started.
    #[must_use]
    pub fn with(mut self, slot: Slot, millis: i32) -> Self {
        self.millis[slot.index()] = millis;
        self
    }

    /// `flag` set to `on`.
    #[must_use]
    pub fn flag(mut self, flag: Flag, on: bool) -> Self {
        if on {
            self.flags |= flag.bit();
        } else {
            self.flags &= !flag.bit();
        }
        self
    }

    /// Whether `slot` is running.
    #[must_use]
    pub fn started(&self, slot: Slot) -> bool {
        self.millis[slot.index()] != STOPPED
    }

    /// Milliseconds since `slot` started, or `None` when stopped.
    #[must_use]
    pub fn elapsed(&self, slot: Slot) -> Option<i32> {
        self.started(slot).then(|| self.millis[slot.index()])
    }

    /// Whether `flag` is set.
    #[must_use]
    pub fn has(&self, flag: Flag) -> bool {
        self.flags & flag.bit() != 0
    }

    /// Whether `cond` holds for this input.
    #[must_use]
    pub fn holds(&self, cond: Cond) -> bool {
        match cond {
            Cond::Always => true,
            Cond::Is(flag) => self.has(flag),
            Cond::IsNot(flag) => !self.has(flag),
            Cond::Started(slot) => self.started(slot),
            Cond::Stopped(slot) => !self.started(slot),
        }
    }
}

impl Default for Keyframes {
    fn default() -> Self {
        Self::NONE
    }
}

/// How one head axis is driven.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Track {
    /// Not touched.
    Off,
    /// Set to the entity's relative look angle.
    Free,
    /// Set to the look angle clamped to `(min, max)` degrees.
    Clamp(f32, f32),
}

/// Head tracking for a keyframed rig: assigned (not added) while `when` holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadRule {
    /// The condition under which the head follows the look direction.
    pub when: Cond,
    /// Yaw handling.
    pub yaw: Track,
    /// Pitch handling.
    pub pitch: Track,
}

/// A walk cycle keyed on the limb swing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WalkSpec {
    /// The cycle.
    pub anim: Anim,
    /// Multiplier from walk position to animation clock.
    pub speed: f32,
    /// Multiplier from walk amplitude to blend weight, capped at one.
    pub scale: f32,
    /// When this cycle plays.
    pub when: Cond,
}

/// Everything a keyframed model needs besides its geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigSpec {
    /// Head tracking.
    pub head: HeadRule,
    /// Limb-swing cycles.
    pub walks: &'static [WalkSpec],
    /// Animations started by a [`Slot`].
    pub states: &'static [(Slot, Anim)],
    /// Parts whose own geometry is hidden while the condition holds.
    pub hides: &'static [(Cond, &'static [&'static str])],
}

const FREE_HEAD: HeadRule = HeadRule { when: Cond::Always, yaw: Track::Free, pitch: Track::Free };

const RABBIT_HEAD: HeadRule =
    HeadRule { when: Cond::Stopped(Slot::IdleHeadTilt), yaw: Track::Free, pitch: Track::Free };

const NO_HEAD: HeadRule = HeadRule { when: Cond::Always, yaw: Track::Off, pitch: Track::Off };

const CAMEL_HEAD: HeadRule = HeadRule {
    when: Cond::Always,
    yaw: Track::Clamp(-30.0, 30.0),
    pitch: Track::Clamp(-25.0, 45.0),
};

const ARMADILLO_HEAD: HeadRule = HeadRule {
    when: Cond::IsNot(Flag::Hiding),
    yaw: Track::Clamp(-32.5, 32.5),
    pitch: Track::Clamp(-22.5, 25.0),
};

const BAT_HEAD: HeadRule = HeadRule { when: Cond::Is(Flag::Resting), yaw: Track::Free, pitch: Track::Off };

const SHELL_HIDDEN: &[&str] = &["body", "left_hind_leg", "right_hind_leg", "tail"];

static RABBIT: RigSpec = RigSpec {
    head: RABBIT_HEAD,
    walks: &[],
    states: &[(Slot::Hop, Anim::RabbitHop), (Slot::IdleHeadTilt, Anim::RabbitIdleHeadTilt)],
    hides: &[],
};

static RABBIT_BABY: RigSpec = RigSpec {
    head: RABBIT_HEAD,
    walks: &[],
    states: &[(Slot::Hop, Anim::BabyRabbitHop), (Slot::IdleHeadTilt, Anim::BabyRabbitIdleHeadTilt)],
    hides: &[],
};

static BAT: RigSpec = RigSpec {
    head: BAT_HEAD,
    walks: &[],
    states: &[(Slot::Fly, Anim::BatFlying), (Slot::Rest, Anim::BatResting)],
    hides: &[],
};

static FROG: RigSpec = RigSpec {
    head: NO_HEAD,
    walks: &[
        WalkSpec { anim: Anim::FrogSwim, speed: 1.0, scale: 2.5, when: Cond::Is(Flag::Swimming) },
        WalkSpec { anim: Anim::FrogWalk, speed: 1.5, scale: 2.5, when: Cond::IsNot(Flag::Swimming) },
    ],
    states: &[
        (Slot::Jump, Anim::FrogJump),
        (Slot::Croak, Anim::FrogCroak),
        (Slot::Tongue, Anim::FrogTongue),
        (Slot::SwimIdle, Anim::FrogIdleWater),
    ],
    hides: &[(Cond::Stopped(Slot::Croak), &["croaking_body"])],
};

static CAMEL: RigSpec = RigSpec {
    head: CAMEL_HEAD,
    walks: &[WalkSpec { anim: Anim::CamelWalk, speed: 2.0, scale: 2.5, when: Cond::Always }],
    states: &[
        (Slot::Sit, Anim::CamelSit),
        (Slot::SitPose, Anim::CamelSitPose),
        (Slot::SitUp, Anim::CamelStandUp),
        (Slot::Idle, Anim::CamelIdle),
        (Slot::Dash, Anim::CamelDash),
    ],
    hides: &[],
};

/// The fox's head is assigned by its posture rig after the walk, as the client does
/// (`crate::entity_posture`), so the walk leaves it alone.
static FOX_BABY: RigSpec = RigSpec {
    head: NO_HEAD,
    walks: &[WalkSpec { anim: Anim::BabyFoxWalk, speed: 1.0, scale: 2.5, when: Cond::Always }],
    states: &[],
    hides: &[],
};

static CAMEL_BABY: RigSpec = RigSpec {
    head: CAMEL_HEAD,
    walks: &[WalkSpec { anim: Anim::BabyCamelWalk, speed: 2.0, scale: 2.5, when: Cond::Always }],
    states: &[
        (Slot::Sit, Anim::BabyCamelSit),
        (Slot::SitPose, Anim::BabyCamelSitPose),
        (Slot::SitUp, Anim::BabyCamelStandUp),
        (Slot::Idle, Anim::BabyCamelIdle),
        (Slot::Dash, Anim::BabyCamelDash),
    ],
    hides: &[],
};

static ARMADILLO: RigSpec = RigSpec {
    head: ARMADILLO_HEAD,
    walks: &[WalkSpec { anim: Anim::ArmadilloWalk, speed: 16.5, scale: 2.5, when: Cond::IsNot(Flag::Hiding) }],
    states: &[
        (Slot::RollOut, Anim::ArmadilloRollOut),
        (Slot::RollUp, Anim::ArmadilloRollUp),
        (Slot::Peek, Anim::ArmadilloPeek),
    ],
    hides: &[(Cond::Is(Flag::Hiding), SHELL_HIDDEN), (Cond::IsNot(Flag::Hiding), &["cube"])],
};

static ARMADILLO_BABY: RigSpec = RigSpec {
    head: ARMADILLO_HEAD,
    walks: &[WalkSpec { anim: Anim::BabyArmadilloWalk, speed: 16.5, scale: 2.5, when: Cond::IsNot(Flag::Hiding) }],
    states: &[
        (Slot::RollOut, Anim::BabyArmadilloRollOut),
        (Slot::RollUp, Anim::BabyArmadilloRollUp),
        (Slot::Peek, Anim::BabyArmadilloPeek),
    ],
    hides: &[(Cond::Is(Flag::Hiding), SHELL_HIDDEN), (Cond::IsNot(Flag::Hiding), &["cube"])],
};

static SNIFFER: RigSpec = RigSpec {
    head: FREE_HEAD,
    walks: &[
        WalkSpec { anim: Anim::SnifferSniffSearch, speed: 9.0, scale: 100.0, when: Cond::Is(Flag::Searching) },
        WalkSpec { anim: Anim::SnifferWalk, speed: 9.0, scale: 100.0, when: Cond::IsNot(Flag::Searching) },
    ],
    states: &[
        (Slot::Dig, Anim::SnifferDig),
        (Slot::LongSniff, Anim::SnifferLongSniff),
        (Slot::Rise, Anim::SnifferStandUp),
        (Slot::Happy, Anim::SnifferHappy),
        (Slot::Scent, Anim::SnifferSniffSniff),
    ],
    hides: &[],
};

/// The keyframe rig for a corpus model name, if it has one.
#[must_use]
pub fn rig_spec(model_name: &str) -> Option<&'static RigSpec> {
    Some(match model_name {
        "rabbit" => &RABBIT,
        "rabbit_baby" => &RABBIT_BABY,
        "bat" => &BAT,
        "frog" => &FROG,
        "camel" | "camel_husk" => &CAMEL,
        "camel_baby" => &CAMEL_BABY,
        "fox_baby" => &FOX_BABY,
        "armadillo" => &ARMADILLO,
        "armadillo_baby" => &ARMADILLO_BABY,
        "sniffer" => &SNIFFER,
        _ => return None,
    })
}

/// The part a bone name addresses. The name `root` is the model's own root part,
/// which the baked tree carries unnamed.
fn bone_index(parts: &[BakedPart], bone: &str) -> Option<usize> {
    if bone == "root" {
        return parts.iter().position(|p| p.parent.is_none());
    }
    parts.iter().position(|p| p.name == bone)
}

/// One animation's channels resolved to part indices.
#[derive(Debug, Clone, PartialEq)]
struct Resolved {
    anim: Anim,
    tracks: Vec<(usize, &'static ChannelDef)>,
}

impl Resolved {
    fn new(anim: Anim, parts: &[BakedPart]) -> Self {
        let tracks = anim
            .def()
            .channels
            .iter()
            .filter_map(|channel| {
                bone_index(parts, channel.bone).map(|index| (index, channel))
            })
            .collect();
        Resolved { anim, tracks }
    }

    fn apply(&self, poses: &mut [PartPose], millis: i64, weight: f32) {
        let seconds = self.anim.def().clock_seconds(millis);
        for (part, channel) in &self.tracks {
            let offset = channel.sample(seconds, weight);
            let pose = &mut poses[*part];
            match channel.target {
                Target::Position => {
                    pose.x += offset[0];
                    pose.y += offset[1];
                    pose.z += offset[2];
                }
                Target::Rotation => {
                    pose.x_rot += offset[0];
                    pose.y_rot += offset[1];
                    pose.z_rot += offset[2];
                }
                Target::Scale => {
                    pose.scale[0] += offset[0];
                    pose.scale[1] += offset[1];
                    pose.scale[2] += offset[2];
                }
            }
        }
    }
}

/// A [`RigSpec`] resolved against one skeleton's part order.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframeRig {
    spec: &'static RigSpec,
    head: Option<usize>,
    walks: Vec<Resolved>,
    states: Vec<Resolved>,
    hides: Vec<Vec<usize>>,
}

fn track_value(track: Track, degrees: f32) -> Option<f32> {
    match track {
        Track::Off => None,
        Track::Free => Some(degrees * DEG),
        Track::Clamp(lo, hi) => Some(degrees.clamp(lo, hi) * DEG),
    }
}

impl KeyframeRig {
    /// Resolve `spec` against `parts`. A bone the rig does not have is skipped; a test
    /// over the whole corpus asserts none is.
    #[must_use]
    pub fn resolve(spec: &'static RigSpec, parts: &[BakedPart]) -> Self {
        let find = |name: &str| parts.iter().position(|p| p.name == name);
        KeyframeRig {
            spec,
            head: find("head"),
            walks: spec.walks.iter().map(|w| Resolved::new(w.anim, parts)).collect(),
            states: spec.states.iter().map(|(_, anim)| Resolved::new(*anim, parts)).collect(),
            hides: spec.hides.iter().map(|(_, names)| names.iter().filter_map(|n| find(n)).collect()).collect(),
        }
    }

    /// The part names this rig's animations drive or hides that are missing from `parts`.
    #[must_use]
    pub fn missing_bones(spec: &RigSpec, parts: &[BakedPart]) -> Vec<&'static str> {
        let mut missing = Vec::new();
        let anims = spec.walks.iter().map(|w| w.anim).chain(spec.states.iter().map(|(_, a)| *a));
        let bones = anims
            .flat_map(|anim| anim.def().channels.iter().map(|c| c.bone))
            .chain(spec.hides.iter().flat_map(|(_, names)| names.iter().copied()));
        for bone in bones {
            if bone_index(parts, bone).is_none() && !missing.contains(&bone) {
                missing.push(bone);
            }
        }
        missing
    }

    /// Poses `poses` (already at rest) for this frame: head tracking, then every
    /// walk cycle and running state added on top.
    pub fn apply(&self, poses: &mut [PartPose], look: [f32; 2], limb: [f32; 2], input: &Keyframes) {
        let [head_yaw_deg, head_pitch_deg] = look;
        let [limb_swing, limb_swing_amount] = limb;
        let rule = &self.spec.head;
        if let Some(head) = self.head
            && input.holds(rule.when)
        {
            if let Some(yaw) = track_value(rule.yaw, head_yaw_deg) {
                poses[head].y_rot = yaw;
            }
            if let Some(pitch) = track_value(rule.pitch, head_pitch_deg) {
                poses[head].x_rot = pitch;
            }
        }
        for (walk, anim) in self.spec.walks.iter().zip(&self.walks) {
            if input.holds(walk.when) {
                let millis = (limb_swing * 50.0 * walk.speed) as i64;
                anim.apply(poses, millis, (limb_swing_amount * walk.scale).min(1.0));
            }
        }
        for ((slot, _), anim) in self.spec.states.iter().zip(&self.states) {
            if let Some(millis) = input.elapsed(*slot) {
                anim.apply(poses, i64::from(millis), 1.0);
            }
        }
    }

    /// Indices of parts whose own geometry is hidden for `input`.
    pub fn hidden<'a>(&'a self, input: &'a Keyframes) -> impl Iterator<Item = usize> + 'a {
        self.spec
            .hides
            .iter()
            .zip(&self.hides)
            .filter(|((cond, _), _)| input.holds(*cond))
            .flat_map(|(_, parts)| parts.iter().copied())
    }
}
