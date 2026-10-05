//! Keyframe animation: authored per-bone channels of timed vectors, sampled at a time.
//!
//! A definition ([`AnimDef`]) is a length, a looping flag and a list of channels. A
//! channel ([`ChannelDef`]) targets one bone's position, rotation or scale with a
//! list of keys ([`Key`]); each key carries the vector at its time and the
//! interpolation used to reach it from the previous key. Sampling a channel yields
//! an **offset** that the caller adds to the bone's authored pose, never a
//! replacement, so several animations stack on one rig.
//!
//! The data lives in [`data`], generated from the reference definitions by
//! `scripts/gen-keyframes.py`. Vectors are stored as the client holds them: rotations
//! in radians, positions with y already negated, scales as `value - 1`.
//!
//! See `docs/keyframe-animation.md` for how the shell drives these.

mod data;

pub use data::KEYFRAME_COUNT;

/// How a key is reached from the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interp {
    /// Straight-line blend between the two neighbouring keys.
    Linear,
    /// Cubic spline through the previous, this, next and following keys.
    CatmullRom,
}

/// Which pose channel a track drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Pivot offset in model texels.
    Position,
    /// Euler rotation offset in radians.
    Rotation,
    /// Per-axis scale offset (`scale - 1`).
    Scale,
}

/// One timed vector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key {
    /// Seconds from the start of the animation.
    pub time: f32,
    /// The value in the target's own units (see [`Target`]).
    pub value: [f32; 3],
    /// How this key is reached from the previous one.
    pub interp: Interp,
}

/// One bone's track for one target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelDef {
    /// The part this track drives, by the rig's own part name.
    pub bone: &'static str,
    /// Which pose channel it offsets.
    pub target: Target,
    /// Keys in ascending time order; never empty.
    pub keys: &'static [Key],
}

/// A whole animation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimDef {
    /// Length in seconds.
    pub length: f32,
    /// Whether the clock wraps at `length`.
    pub looping: bool,
    /// Every track.
    pub channels: &'static [ChannelDef],
}

/// The animations the renderer drives, one per generated definition.
#[allow(missing_docs, reason = "each variant is named for the animation it plays")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Anim {
    RabbitHop,
    RabbitIdleHeadTilt,
    BabyRabbitHop,
    BabyRabbitIdleHeadTilt,
    BatFlying,
    BatResting,
    FrogJump,
    FrogCroak,
    FrogTongue,
    FrogWalk,
    FrogSwim,
    FrogIdleWater,
    CamelWalk,
    CamelSit,
    CamelSitPose,
    CamelStandUp,
    CamelIdle,
    CamelDash,
    BabyCamelWalk,
    BabyCamelSit,
    BabyCamelSitPose,
    BabyCamelStandUp,
    BabyCamelIdle,
    BabyCamelDash,
    ArmadilloWalk,
    ArmadilloRollOut,
    ArmadilloRollUp,
    ArmadilloPeek,
    BabyArmadilloWalk,
    BabyArmadilloRollOut,
    BabyArmadilloRollUp,
    BabyArmadilloPeek,
    SnifferWalk,
    SnifferSniffSearch,
    SnifferDig,
    SnifferLongSniff,
    SnifferStandUp,
    SnifferHappy,
    SnifferSniffSniff,
    BabyFoxWalk,
    BabyAxolotlSwim,
    BabyAxolotlWalkFloor,
    BabyAxolotlWalkUnderwater,
    BabyAxolotlIdleUnderwater,
    BabyAxolotlIdleFloorUnderwater,
    BabyAxolotlIdleFloor,
    BabyAxolotlPlayDead,
}

/// The first index in `0..len` where `holds` is true, found by halving; `len` when none is.
fn first_true(len: usize, holds: impl Fn(usize) -> bool) -> usize {
    let (mut from, mut remaining) = (0, len);
    while remaining > 0 {
        let half = remaining / 2;
        let middle = from + half;
        if holds(middle) {
            remaining = half;
        } else {
            from = middle + 1;
            remaining -= half + 1;
        }
    }
    from
}

/// `a + (b - a) * t` with a single rounding, as the reference's vector blend does it.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    t.mul_add(b - a, a)
}

/// The spline through four points at `t`.
fn catmull_rom(t: f32, p0: f32, p1: f32, p2: f32, p3: f32) -> f32 {
    0.5 * (2.0 * p1
        + (p2 - p0) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
        + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t * t * t)
}

impl AnimDef {
    /// The animation clock for `millis` since it started: seconds, wrapped at the
    /// length when looping.
    #[must_use]
    pub fn clock_seconds(&self, millis: i64) -> f32 {
        let seconds = millis as f32 / 1000.0;
        if self.looping { seconds % self.length } else { seconds }
    }

    /// Calls `apply(bone, target, offset)` once per channel for the animation at
    /// `millis` since its start, every vector multiplied by `weight`.
    ///
    /// `weight` is `1.0` for a state-driven animation and the clamped walk speed for
    /// a walk cycle.
    pub fn sample(&self, millis: i64, weight: f32, mut apply: impl FnMut(&'static str, Target, [f32; 3])) {
        let seconds = self.clock_seconds(millis);
        for channel in self.channels {
            apply(channel.bone, channel.target, channel.sample(seconds, weight));
        }
    }
}

impl ChannelDef {
    /// This track's offset at `seconds` on the animation clock, times `weight`.
    #[must_use]
    pub fn sample(&self, seconds: f32, weight: f32) -> [f32; 3] {
        let keys = self.keys;
        // The first key at or after the clock, by the reference's own halving search
        // (so a NaN clock lands past the end rather than at the start); the segment
        // starts one key before it.
        let first_at_or_after = first_true(keys.len(), |i| seconds <= keys[i].time);
        let prev = first_at_or_after.saturating_sub(1);
        let next = (prev + 1).min(keys.len() - 1);
        let (from, to) = (&keys[prev], &keys[next]);
        let alpha = if next != prev {
            ((seconds - from.time) / (to.time - from.time)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        match to.interp {
            Interp::Linear => std::array::from_fn(|i| lerp(from.value[i], to.value[i], alpha) * weight),
            Interp::CatmullRom => {
                let p0 = &keys[prev.saturating_sub(1)];
                let p3 = &keys[(next + 1).min(keys.len() - 1)];
                std::array::from_fn(|i| {
                    catmull_rom(alpha, p0.value[i], from.value[i], to.value[i], p3.value[i]) * weight
                })
            }
        }
    }
}
