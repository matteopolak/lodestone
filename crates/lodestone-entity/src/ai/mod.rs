//! Goal-based mob AI.
//!
//! Vanilla drives mob behaviour with a [`GoalSelector`]: a prioritised set of
//! [`Goal`]s that claim mutually-exclusive [`Flag`]s (MOVE / LOOK / JUMP /
//! TARGET), where a higher-priority goal preempts a lower one holding the same
//! flag. This module reproduces that scheduler and a representative set of
//! goals ([`goals`]). Goals act on the mob through the [`MobController`] seam so
//! the AI layer stays free of world and physics dependencies.

pub mod bee;
pub mod block_edit;
pub mod enderman_block;
pub mod kinetic;
pub mod spear_use;
pub mod village_walk;
pub mod block_seek;
pub mod goal;
pub mod goals;
pub mod locomotion;
pub mod mob;
pub mod navigating_mob;
pub mod parrot;
pub mod pathfind_to_raid;
pub mod raider;
pub mod raid_garden;
pub mod remove_block;
pub mod roster;
pub mod stalk_attack;
pub mod target_class;
pub mod turtle_egg;

pub use block_edit::{BlockEdit, BlockExpect};
pub use goal::{Flag, FlagSet, Goal, GoalId, GoalSelector, MobAi, reduced_tick_delay};
pub use roster::{SpeciesContext, goals_for};
pub use target_class::{TargetClass, TargetClassSet};
pub use mob::{MobController, ProjectileKind, ProjectileLaunch, SwoopState};
pub use navigating_mob::{
    BABY_START_AGE, LOVE_TICKS, MAX_SWELL, MainHandItem, NavigatingMob,
    PARENT_AGE_AFTER_BREEDING,
};
