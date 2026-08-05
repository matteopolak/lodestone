//! Goal sets for the mobs whose attack is neither melee nor a projectile:
//! guardian, elder guardian, shulker, vex, warden, ravager.
//!
//! # What it is
//!
//! Empty on purpose. This module is **pre-declared and pre-registered** in
//! [`FAMILIES`](super::FAMILIES) so that issue [#232] can fill it in without
//! editing a single shared file.
//!
//! # How to change it
//!
//! Add each species path to [`SPECIES`] and an arm to [`lookup`]; see
//! [`super::hostile_melee`] for the shape and the citation discipline.
//!
//! # What blocks it, as of this module being created
//!
//! **A guardian's beam is a third attack shape** — charge, then damage on a
//! timer, with no projectile entity at all: `ATTACK_TIME = 80`
//! (`monster/Guardian.java:48`), `attackTime` starts at `-10` (`:365`) and the
//! damage lands at `getAttackDuration()` (`:396-402`). It needs neither
//! `MeleeAttackGoal` nor #227's launch path, so it does not depend on the ranged
//! family.
//!
//! **The warden is Brain-based, not `GoalSelector`-based**, so it does not belong
//! in this roster at all — it belongs to the Brain driver (issue #209). Do not
//! add a warden table here.
//!
//! **A ghast's fireball feeds `explosion.rs`**, which now has a real caller and a
//! real `encode_explode` (`crates/protocol/v770/src/server_protocol.rs:2085`), so
//! the explosion half is already wired; the launch half is #227's.
//!
//! [#232]: https://github.com/matteopolak/lodestone/issues/232

use super::Registration;

/// Every species this family claims — none yet.
pub const SPECIES: &[&str] = &[];

/// Resolves a species path to its table. Always `None` until #232 lands.
#[must_use]
pub fn lookup(_species: &str) -> Option<&'static [Registration]> {
    // Replace with `match species { "blaze" => Some(BLAZE), _ => None }` when
    // this family gains its first table.
    None
}
