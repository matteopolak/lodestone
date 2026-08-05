//! Client-predicted local sounds: playing your *own* footsteps immediately instead
//! of waiting for the server to echo them back.
//!
//! # Which sounds vanilla predicts, and how it actually decides
//!
//! Not a list — a **three-level method override**, and reading it off the call sites
//! is the only reliable way. `Level.playSound` takes an `except` player, and the two
//! sides read that one argument in mirror-image ways:
//!
//! | | behaviour | cite |
//! |---|---|---|
//! | `ClientLevel.playSeededSound` | plays locally **iff `except == minecraft.player`** | `ClientLevel.java:679-693` |
//! | `ServerLevel.playSeededSound` | broadcasts to everyone **except** that player | `ServerLevel.java:1036-1058` |
//!
//! So `except` is simultaneously "who hears it locally" and "who is left out of the
//! broadcast". Which player gets passed is then decided by three overrides:
//!
//! | method | passes | consequence for the local player | cite |
//! |---|---|---|---|
//! | `Entity.playSound` | `null` | **silent** on the client; arrives only as a packet | `Entity.java:1500-1504` |
//! | `Player.playSound` | `this` | predicted, and the server leaves that player out | `Player.java:398-400` |
//! | `LocalPlayer.playSound` | — calls `playLocalSound` | **unconditionally local**, no `except` check at all | `LocalPlayer.java:540-542` |
//!
//! The practical rule that falls out: **every `playSound(event, volume, pitch)` call
//! reached with the local player as `this` is client-predicted**, because
//! `LocalPlayer` overrides it to a straight local play. That covers footsteps
//! (`Entity.playStepSound` → the override), muffled steps, and swim sounds.
//!
//! And it excludes attacks. `Player` routes those through a method vanilla literally
//! names **`playServerSideSound`**, which passes `null` (`Player.java:1009-1011`), as
//! do the level-up and deflect sounds (`:1571`, `:1025`). So *swing and attack sounds
//! are not predicted* — a guess to the contrary would have produced doubled hit
//! sounds on every swing.
//!
//! # The double-play question, answered structurally
//!
//! **Vanilla has no de-duplication logic whatsoever**, and does not need any: the
//! same `except` argument that makes the client play locally makes the server omit
//! that client from the broadcast. The two halves are one mechanism.
//!
//! Our situation is different in a way that makes the risk *lower*, not higher:
//! `crates/lodestone-server` sends **no sound packets at all** (grep for a
//! clientbound sound in it — there are none), so against the integrated server a
//! prediction is the only source, and against a real vanilla server the exclusion
//! above applies. There is no configuration reachable today that double-plays.
//!
//! [`PredictionLedger`] therefore exists as **defence in depth, not a fix**: it costs
//! a small ring buffer, and it means a future server-side `playSound` that forgets
//! the exclusion degrades to "correct" rather than "everything doubled". Its gate
//! includes the control that matters — that it does not suppress an *unrelated*
//! server sound, because a de-duplicator which swallows everything passes a
//! naive "no double-play" assertion perfectly.

use std::borrow::Cow;
use std::collections::VecDeque;

use glam::{DVec3, Vec3};
use lodestone_audio::JavaRandom;

/// `Entity.moveDistScale` — `Entity.java:875`. Distance travelled is scaled by this
/// before accumulating, so the step threshold of 1.0 corresponds to `1 / 0.6 ≈ 1.667`
/// blocks of travel.
pub const MOVE_DIST_SCALE: f32 = 0.6;

/// `Entity.nextStep`'s initial value — `Entity.java:246`.
pub const INITIAL_NEXT_STEP: f32 = 1.0;

/// `Entity.playStepSound`'s volume multiplier on the block's sound type —
/// `Entity.java:1473` (`soundType.getVolume() * 0.15F`).
pub const STEP_VOLUME_SCALE: f32 = 0.15;

/// `Entity.playMuffledStepSound`'s multipliers — `Entity.java:1468`
/// (`volume * 0.05F`, `pitch * 0.8F`). Used for the secondary sound when standing on
/// a `COMBINATION_STEP_SOUND_BLOCKS` block, and underwater
/// (`Player.playStepSound:1485-1487`).
pub const MUFFLED_STEP_VOLUME_SCALE: f32 = 0.05;
/// See [`MUFFLED_STEP_VOLUME_SCALE`].
pub const MUFFLED_STEP_PITCH_SCALE: f32 = 0.8;

/// `Entity.waterSwimSound`'s volume modifier when the entity is its own controller —
/// `Entity.java:1444`. It is `0.4` when a passenger is steering.
pub const SWIM_SELF_VOLUME_MODIFIER: f32 = 0.35;
/// See [`SWIM_SELF_VOLUME_MODIFIER`].
pub const SWIM_PASSENGER_VOLUME_MODIFIER: f32 = 0.4;

/// `Entity.playSwimSound`'s pitch jitter — `Entity.java:1490`:
/// `1.0 + (nextFloat() - nextFloat()) * 0.4`.
///
/// Draws **twice**, in that order, so it is order-sensitive against a shared stream.
/// The result is symmetric about 1.0 and bounded by `0.6..=1.4`, but *triangularly*
/// distributed rather than uniformly — a single `nextFloat()` mapped onto the same
/// range would be audibly flatter and is the obvious wrong simplification.
pub fn swim_pitch(rng: &mut JavaRandom) -> f32 {
    1.0 + (rng.next_f32() - rng.next_f32()) * 0.4
}

/// Vanilla's step-distance accumulator — `Entity.java:875-895` plus
/// `Entity.nextStep()` at `:1270-1271`.
///
/// # Why this is a distance accumulator and not a timer
///
/// Footsteps are spaced by **distance travelled**, so they speed up when you sprint
/// and stop when you walk into a wall — a tick-interval model gets both wrong, and
/// gets them wrong in a way that is obvious in play and invisible to a test that only
/// checks "a footstep happened".
///
/// The threshold re-arms to `(int)move_dist + 1`, i.e. the *next integer boundary*,
/// not `move_dist + 1`. That difference keeps steps locked to a global 1-block grid
/// of accumulated distance instead of drifting with each step's overshoot.
#[derive(Debug, Clone, PartialEq)]
pub struct StepAccumulator {
    move_dist: f32,
    next_step: f32,
}

impl Default for StepAccumulator {
    fn default() -> Self {
        Self {
            move_dist: 0.0,
            next_step: INITIAL_NEXT_STEP,
        }
    }
}

impl StepAccumulator {
    /// A fresh accumulator, matching a newly spawned entity.
    pub fn new() -> Self {
        Self::default()
    }

    /// Accumulated scaled distance (`Entity.moveDist`).
    pub fn move_dist(&self) -> f32 {
        self.move_dist
    }

    /// The threshold the next step fires at (`Entity.nextStep`).
    pub fn next_step(&self) -> f32 {
        self.next_step
    }

    /// Accumulate one movement and report whether the step threshold was crossed.
    ///
    /// `moved` is vanilla's `clippedMovement` — the movement *actually achieved*
    /// after collision, not the movement requested, which is why walking into a wall
    /// produces no footsteps. `climbing` selects the full 3D length instead of the
    /// horizontal component (`:881`), so climbing a ladder does step.
    /// `supporting_is_air` suppresses the step entirely (`:883`), which is what keeps
    /// a falling or flying player silent.
    ///
    /// This only reports the crossing; call [`StepAccumulator::consume`] once a sound
    /// has actually been produced. The split is faithful — vanilla re-arms
    /// `nextStep` only when `producedSideEffects` (or it is in water), so a crossing
    /// over a block that makes no sound leaves the threshold armed.
    pub fn advance(&mut self, moved: DVec3, climbing: bool, supporting_is_air: bool) -> bool {
        let full = (moved.length() * f64::from(MOVE_DIST_SCALE)) as f32;
        let horizontal = (DVec3::new(moved.x, 0.0, moved.z).length()
            * f64::from(MOVE_DIST_SCALE)) as f32;
        self.move_dist += if climbing { full } else { horizontal };
        self.move_dist > self.next_step && !supporting_is_air
    }

    /// Re-arm the threshold after a step sound was produced — `Entity.nextStep()`
    /// (`:1270-1271`): `(int)move_dist + 1`, truncating.
    pub fn consume(&mut self) {
        self.next_step = self.move_dist.trunc() + 1.0;
    }
}

/// How long a recorded prediction stays eligible to suppress a matching server echo.
///
/// One second. A vanilla server broadcasts the sound in the same tick it processes
/// the movement, so the real latency is one round trip; 20 ticks is generous enough
/// to cover a bad connection and short enough that a genuinely separate second
/// footstep at the same spot is never swallowed (at the `1.667`-block step spacing,
/// re-treading the same position inside 20 ticks means standing still, which produces
/// no steps).
pub const ECHO_WINDOW_TICKS: u64 = 20;

/// How far apart two same-named sounds may be and still be considered the same event,
/// in blocks.
///
/// The server rounds positions when it encodes them, so an echo does not arrive at
/// bit-identical coordinates; but two *different* footsteps are at least one step
/// apart. `2.0` sits between the two.
pub const ECHO_POSITION_EPSILON: f32 = 2.0;

/// One recorded prediction.
#[derive(Debug, Clone)]
struct Entry {
    event: Cow<'static, str>,
    position: Vec3,
    tick: u64,
}

/// Remembers recently predicted sounds so a server echo of the *same* sound can be
/// suppressed instead of played twice.
///
/// See the module docs for why this is defence in depth rather than a fix. The
/// behaviour that makes it safe is that a match **consumes** the entry: one
/// prediction can suppress exactly one echo, so a burst of three real footsteps is
/// never collapsed into one by a single stale prediction.
#[derive(Debug, Clone, Default)]
pub struct PredictionLedger {
    entries: VecDeque<Entry>,
}

impl PredictionLedger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many predictions are still eligible to suppress an echo.
    pub fn pending(&self) -> usize {
        self.entries.len()
    }

    /// Record a sound the client just played locally.
    pub fn record(&mut self, event: impl Into<Cow<'static, str>>, position: Vec3, tick: u64) {
        self.entries.push_back(Entry {
            event: event.into(),
            position,
            tick,
        });
        self.prune(tick);
    }

    /// Drop predictions older than [`ECHO_WINDOW_TICKS`].
    ///
    /// Call every tick as well as on record: a ledger pruned only on insert grows
    /// unboundedly whenever the player stops moving, and — worse — keeps a stale
    /// entry eligible forever, which would swallow one real sound much later.
    pub fn prune(&mut self, tick: u64) {
        while let Some(front) = self.entries.front() {
            if tick.saturating_sub(front.tick) > ECHO_WINDOW_TICKS {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    /// Whether an incoming server sound is our own echo and should be dropped.
    ///
    /// Matching is on event name, proximity within [`ECHO_POSITION_EPSILON`], and the
    /// [`ECHO_WINDOW_TICKS`] window. A match removes the entry.
    pub fn should_suppress(&mut self, event: &str, position: Vec3, tick: u64) -> bool {
        self.prune(tick);
        let found = self.entries.iter().position(|e| {
            e.event == event
                && tick.saturating_sub(e.tick) <= ECHO_WINDOW_TICKS
                && e.position.distance(position) <= ECHO_POSITION_EPSILON
        });
        match found {
            Some(index) => {
                self.entries.remove(index);
                true
            }
            None => false,
        }
    }
}
