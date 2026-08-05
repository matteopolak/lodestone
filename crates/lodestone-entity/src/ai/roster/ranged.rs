//! Goal sets for the ranged attackers: skeleton bow, blaze, ghast, witch,
//! pillager, snow golem.
//!
//! # What it is
//!
//! Empty on purpose. This module is **pre-declared and pre-registered** in
//! [`FAMILIES`](super::FAMILIES) so that issue [#227] can fill it in without
//! editing a single shared file — no `mod` line, no registration list, no arm in
//! `mobs.rs`. Add tables and species here and they reach production through
//! [`super::goals_for`] immediately.
//!
//! # How to change it
//!
//! Add each species path to [`SPECIES`] and an arm to [`lookup`]; see
//! [`super::hostile_melee`] for the shape and the citation discipline. `roster`'s
//! own invariant gates iterate `SPECIES`, so anything you list is checked
//! automatically.
//!
//! # What blocks it, as of this module being created
//!
//! **No ranged goal type exists** — `RangedAttackGoal` and `BowAttack` are zero
//! hits tree-wide — and **nothing launches a projectile in production**:
//! `MobSim::spawn_projectile` has no caller outside
//! `crates/lodestone-server/tests/projectile_and_item_registries.rs`, so
//! `ProjectileRegistry` is permanently empty at runtime even though it is ticked.
//! Both are #227's to build; neither is a roster problem.
//!
//! What #227 *does* get for free from this unit is
//! [`GoalSelector::remove`](crate::ai::GoalSelector::remove), which vanilla's
//! `AbstractSkeleton.reassessWeaponGoal()` needs to swap the priority-4 melee
//! goal for the bow goal at runtime
//! (`monster/skeleton/AbstractSkeleton.java:132-146`). The skeleton's melee half
//! is already registered in [`super::hostile_melee::SKELETON`] at vanilla's
//! priority 4, so the swap has a real slot to replace.
//!
//! [#227]: https://github.com/matteopolak/lodestone/issues/227

use super::Registration;

/// Every species this family claims — none yet.
pub const SPECIES: &[&str] = &[];

/// Resolves a species path to its table. Always `None` until #227 lands.
#[must_use]
pub fn lookup(_species: &str) -> Option<&'static [Registration]> {
    // Replace with `match species { "blaze" => Some(BLAZE), _ => None }` when
    // this family gains its first table.
    None
}
