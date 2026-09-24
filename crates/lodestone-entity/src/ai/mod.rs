//! Goal-based mob AI.
//!
//! Vanilla drives mob behaviour with a [`GoalSelector`]: a prioritised set of
//! [`Goal`]s that claim mutually-exclusive [`Flag`]s (MOVE / LOOK / JUMP /
//! TARGET), where a higher-priority goal preempts a lower one holding the same
//! flag. This module reproduces that scheduler and a representative set of
//! goals ([`goals`]). Goals act on the mob through the [`MobController`] seam so
//! the AI layer stays free of world and physics dependencies.

pub mod goal;
pub mod goals;
pub mod mob;
pub mod navigating_mob;
pub mod roster;

pub use goal::{Flag, FlagSet, Goal, GoalId, GoalSelector, MobAi};
pub use roster::{SpeciesContext, goals_for};
pub use mob::{MobController, ProjectileKind, ProjectileLaunch};
pub use navigating_mob::{
    BABY_START_AGE, LOVE_TICKS, MAX_SWELL, MainHandItem, NavigatingMob,
    PARENT_AGE_AFTER_BREEDING,
};
