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

pub use goal::{Flag, FlagSet, Goal, GoalSelector, MobAi};
pub use mob::MobController;
pub use navigating_mob::{BABY_START_AGE, LOVE_TICKS, NavigatingMob, PARENT_AGE_AFTER_BREEDING};
