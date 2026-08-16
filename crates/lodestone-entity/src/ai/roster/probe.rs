//! A [`MobController`] fake that records what a goal *asks the mob to do*, for
//! checking transcribed speed multipliers.
//!
//! # What it is
//!
//! Test-only. Every perception method answers permissively, so any goal in the
//! roster can reach `start()` without a world; every action method records
//! instead of acting. The one thing it is for is reading back the `speed`
//! argument a goal passes to [`MobController::move_to`].
//!
//! # Why it exists rather than a structural check
//!
//! A roster gate that compares *priorities* against the jar cannot see a wrong
//! speed: `TemptGoal` at priority 3 built with `1.1` instead of the cow's `1.25`
//! satisfies every priority assertion, and satisfies "the cow moved toward the
//! player" too, because that is a direction. Only a gate that predicts the
//! **value** `0.2 × 1.25 = 0.25` and requires the measurement to land on it can
//! tell the two hypotheses apart — the *magnitude* species of vacuous test in
//! CLAUDE.md's evidence standards.
//!
//! # How to change it
//!
//! It answers `Some`/`true` for everything by design, which is exactly what makes
//! it useless for asking whether a goal *should* run. Do not gate `can_use` on
//! it; that is what `ScriptMob` already does and what has previously hidden islands.
//! Use it only to read arguments back.

use lodestone_model::Vec3;

use crate::ai::mob::MobController;

/// Records the `(target, speed)` of every `move_to` a goal performs.
#[derive(Debug, Default)]
pub struct SpeedProbe {
    /// Every `move_to` call, in order.
    pub moves: Vec<(Vec3, f64)>,
    /// A fixed position 4 blocks away, returned by every perception method that
    /// answers with a position — close enough to be inside every range check in
    /// the roster (the tightest is `LookAtPlayerGoal(6.0)`).
    pub nearby: Vec3,
}

impl SpeedProbe {
    /// A probe at the origin with its perception target 4 blocks along +X.
    #[must_use]
    pub fn new() -> Self {
        Self {
            moves: Vec::new(),
            nearby: Vec3::new(4.0, 0.0, 0.0),
        }
    }

    /// The speed of the first `move_to`, if the goal performed one.
    #[must_use]
    pub fn first_speed(&self) -> Option<f64> {
        self.moves.first().map(|&(_, s)| s)
    }
}

impl MobController for SpeedProbe {
    // `next_f32` returns 0.0 so every probability gate (`LookAtPlayerGoal`'s
    // 0.02, `FloatGoal`'s 0.8) passes: they all test `next_f32() < p`.
    fn next_f32(&mut self) -> f32 {
        0.0
    }
    fn next_i32(&mut self, _bound: i32) -> i32 {
        0
    }
    fn next_f64(&mut self) -> f64 {
        0.0
    }
    fn position(&self) -> Vec3 {
        Vec3::default()
    }
    fn move_to(&mut self, target: Vec3, speed: f64) -> bool {
        self.moves.push((target, speed));
        true
    }
    fn navigation_done(&self) -> bool {
        false
    }
    fn stop_navigation(&mut self) {}
    fn set_jumping(&mut self, _jumping: bool) {}
    fn look_at(&mut self, _target: Vec3) {}
    fn look_toward(&mut self, _dx: f64, _dz: f64) {}
    fn random_stroll_target(&mut self) -> Option<Vec3> {
        Some(self.nearby)
    }

    // -- permissive perception, so every goal can start ---------------------
    fn in_water(&self) -> bool {
        true
    }
    fn nearest_player(&self) -> Option<Vec3> {
        Some(self.nearby)
    }
    fn last_hurt_by(&self) -> Option<Vec3> {
        Some(self.nearby)
    }
    fn temptation(&self) -> Option<Vec3> {
        Some(self.nearby)
    }
    fn avoid_threat(&self) -> Option<Vec3> {
        Some(self.nearby)
    }
    fn is_panicking(&self) -> bool {
        true
    }
    fn find_love_partner(&mut self) -> Option<Vec3> {
        Some(self.nearby)
    }
    fn love_partner_position(&self) -> Option<Vec3> {
        Some(self.nearby)
    }
    fn is_in_love(&self) -> bool {
        true
    }
    // `FollowParentGoal` returns early unless the mob is a baby with a parent in
    // range (`FollowParentGoal.canUse`, `getAge() >= 0` → no goal).
    fn is_baby(&self) -> bool {
        true
    }
    fn parent_position(&self) -> Option<Vec3> {
        Some(self.nearby)
    }
    fn attack_target(&self) -> Option<Vec3> {
        Some(self.nearby)
    }
    fn find_nearest_target(&mut self) -> Option<Vec3> {
        Some(self.nearby)
    }
    // Permissive like every other perception method here: the drowned's
    // trident goal (`RangedAttackGoal::with_required_main_hand`) is the one
    // production reader of `main_hand_item` today, so answering "holding a
    // trident" lets that goal reach `start()` through this probe too, instead
    // of this file's own permissive design silently excluding it.
    fn main_hand_item(&self) -> Option<&str> {
        Some("trident")
    }
}
