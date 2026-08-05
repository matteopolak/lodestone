//! Goal sets for the neutral mobs that hold a grudge: enderman, zombified
//! piglin, bee, wolf, llama, panda, polar bear.
//!
//! # What it is
//!
//! Empty on purpose. This module is **pre-declared and pre-registered** in
//! [`FAMILIES`](super::FAMILIES) so that issue [#233] can fill it in without
//! editing a single shared file.
//!
//! # How to change it
//!
//! Add each species path to [`SPECIES`] and an arm to [`lookup`]; see
//! [`super::hostile_melee`] for the shape and the citation discipline.
//!
//! # What blocks it, as of this module being created
//!
//! **The shared anger timer has no home yet.** Vanilla's
//! `NeutralMob.PERSISTENT_ANGER_TIME` is `TimeUtil.rangeOfSeconds(20, 39)` for
//! both zombified piglin (`monster/zombie/ZombifiedPiglin.java:58`) and bee
//! (`animal/bee/Bee.java:129`), and the state machine belongs in one place rather
//! than once per species.
//!
//! **There is no event bus, no cancellation and no hook registration
//! server-side**, so `alertOthers()`-style anger propagation
//! (`ZombifiedPiglin.java:139`, called from `:132`) has nowhere to publish. Plan a
//! direct sim-side census in `MobSim::tick` instead of waiting for a bus — the
//! perception feed issue #441 added is the precedent.
//!
//! **Bee anger and wolf tame state are entity metadata**, so anything reaching a
//! client must run `crates/protocol/v770/oracle-java/EntityDataIndexOracle.java`
//! rather than hand-counting an index. Hand counting has already shipped two
//! off-by-one bugs in this repo.
//!
//! [#233]: https://github.com/matteopolak/lodestone/issues/233

use super::Registration;

/// Every species this family claims — none yet.
pub const SPECIES: &[&str] = &[];

/// Resolves a species path to its table. Always `None` until #233 lands.
#[must_use]
pub fn lookup(_species: &str) -> Option<&'static [Registration]> {
    // Replace with `match species { "blaze" => Some(BLAZE), _ => None }` when
    // this family gains its first table.
    None
}
