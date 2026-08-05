//! Which species run on a [`Brain`], and the behaviour set they get.
//!
//! The [`ai::roster`](crate::ai::roster) analogue for the *other* AI
//! architecture. `ai::roster::goals_for` answers "what goals does a cow get";
//! this answers "does this species have a brain at all, and what is in it".
//!
//! # The species list is a jar claim, so it is gated against the jar
//!
//! [`BRAIN_SPECIES`] is a hand-transcribed name list, and `CLAUDE.md` is blunt
//! about those: five have been wrong recently in this repo, the worst a tag set
//! 1-of-4 correct *with a false positive*. So this list is not merely asserted —
//! `tests/brain_census.rs` re-derives it from the decompiled 26.2 sources
//! (every `world/entity/**.java` declaring `brainProvider`/`makeBrain`, joined to
//! its `EntityType.register` id) and fails on any drift in **either** direction,
//! with `LODESTONE_REGEN=1` to refresh. The gate is `#[ignore]`d because it needs
//! `.cache/mc/26.2/src`, which is not repo state.
//!
//! A name missing from this list is not a cosmetic defect: it is a species that
//! silently gets [`FALLBACK`](crate::ai::roster::FALLBACK) stroll-and-look goals
//! instead of a brain, which is exactly the failure mode that looks fine on
//! screen and is wrong.
//!
//! # Scope: this is the plumbing, not the behaviour packages
//!
//! Every brain species here gets the same generic CORE+IDLE [`scaffold`]. That is
//! deliberate and it is what issue #209 asks for — the *composition*, proven with
//! the scaffold `behaviors.rs` already ships. The per-species behaviour packages
//! (a villager's profession schedule and trading availability, a warden's
//! vibration sensor and anger, a piglin's barter and hoglin hunt) are separate
//! units layered on this one, and each needs machinery this crate does not have
//! yet — the warden's needs vanilla's world-event/vibration registry, which is
//! **absent** here (`lodestone_ecs::GameEvent` is the client plugin bus and
//! `GAME_EVENT` is vanilla's weather packet; neither is the vibration listener).
//!
//! So a camel and a warden currently behave identically: they wander and watch
//! players, through a real brain, on real paths. That is a floor, not a finish —
//! and it is a floor above the previous state, where they wandered through
//! `FALLBACK` goals and the brain ran for nobody.

use super::activity::Activity;
use super::behavior::{Behavior, BehaviorControl, Leaf};
use super::behaviors::{LookAtTargetSink, MoveToTargetSink, RandomStroll, SetPlayerLookTarget};
use super::driver::BrainGoal;
use super::gate::GateBehavior;
use super::sensor::NearestPlayerSensor;
use super::Brain;

/// The walk-target speed **modifier** the scaffold's stroll writes.
///
/// This is vanilla's `speedModifier` unit — the `0.6F`-ish float a brain
/// behaviour hands to `WalkTarget` — not blocks per tick. Worth stating plainly
/// because the two are easy to confuse and
/// [`SpeciesContext::speed`](crate::ai::SpeciesContext::speed) is the *other* one:
/// it is the `movement_speed` attribute in blocks/tick, and feeding it here would
/// be a unit error.
///
/// It also does not currently change how fast the mob moves.
/// [`NavigatingMob::advance`](crate::ai::NavigatingMob) is a kinematic follower
/// that steps at its own `step_per_tick` and ignores the speed the navigator was
/// started with, so this value reaches
/// [`MobController::move_to`](crate::ai::MobController::move_to) and stops there.
/// Stated so nobody later "fixes" a speed bug by editing this constant.
pub const SCAFFOLD_STROLL_SPEED: f32 = 1.0;

/// How far the scaffold's look behaviour will track a player, in blocks. Vanilla's
/// brain mobs use `8.0F` for the generic `SetEntityLookTarget` player row, the
/// same figure the goal system's `LookAtPlayerGoal(Player.class, 8.0F)` uses.
pub const SCAFFOLD_LOOK_DISTANCE: f32 = 8.0;

/// The resource-key **paths** of every concrete 26.2 mob that runs on a
/// [`Brain`] rather than a [`GoalSelector`](crate::ai::GoalSelector).
///
/// Transcribed from the crate census in [`super`]'s module doc; gated against the
/// decompiled sources by `tests/brain_census.rs` — see this module's doc for why
/// the gate exists rather than trust.
///
/// Sorted, so a diff against the jar-derived list is a plain set comparison.
pub const BRAIN_SPECIES: &[&str] = &[
    "allay",
    "armadillo",
    "axolotl",
    "breeze",
    "camel",
    "copper_golem",
    "creaking",
    "frog",
    "goat",
    "happy_ghast",
    "hoglin",
    "nautilus",
    "piglin",
    "piglin_brute",
    "sniffer",
    "tadpole",
    "villager",
    "warden",
    "zoglin",
    "zombie_nautilus",
];

/// Whether `species` (a resource-key path, `"villager"` not
/// `"minecraft:villager"`) runs on a brain.
#[must_use]
pub fn is_brain_species(species: &str) -> bool {
    BRAIN_SPECIES.contains(&species)
}

fn leaf<B: Behavior + 'static>(b: B) -> Box<dyn BehaviorControl> {
    Box::new(Leaf::new(b))
}

/// The universal CORE + IDLE scaffold every vanilla brain shares: the move and
/// look **sinks** in `CORE`, and a run-one gate of look-at-player / stroll in
/// `IDLE`.
///
/// The stroll ⇄ move-sink pair is the architecture in miniature — two behaviours
/// that never name each other, coordinating entirely through the `WALK_TARGET`
/// memory. `CORE` holds the sinks because they must keep running across an
/// activity switch; `IDLE` holds the deciders.
#[must_use]
pub fn scaffold(stroll_speed: f32, look_distance: f32) -> Brain {
    let mut brain = Brain::new();
    brain.add_sensor(Box::new(NearestPlayerSensor));
    brain.add_activity(
        Activity::CORE,
        vec![
            (0, leaf(LookAtTargetSink::default())),
            (1, leaf(MoveToTargetSink::new())),
        ],
        Vec::new(),
        Vec::new(),
    );
    brain.add_activity(
        Activity::IDLE,
        vec![(
            5,
            Box::new(GateBehavior::run_one(
                "idle_gate",
                vec![
                    leaf(SetPlayerLookTarget::new(look_distance)),
                    leaf(RandomStroll::new(stroll_speed)),
                ],
            )),
        )],
        Vec::new(),
        Vec::new(),
    );
    brain
}

/// The [`BrainGoal`] a brain-driven species gets, or `None` for a species that
/// runs on the goal system.
///
/// This is the function [`ai::roster::goals_for`](crate::ai::roster::goals_for)
/// consults, which is how a brain reaches a real mob in a real world: the server's
/// `spawn_species` already installs whatever `goals_for` returns, so nothing on
/// the host side has to know brains exist.
#[must_use]
pub fn brain_for(species: &str) -> Option<BrainGoal> {
    is_brain_species(species)
        .then(|| BrainGoal::idle(scaffold(SCAFFOLD_STROLL_SPEED, SCAFFOLD_LOOK_DISTANCE)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn species_list_is_sorted_and_has_no_duplicates() {
        // Not cosmetic: the jar-drift gate compares sorted sets, and a duplicate
        // would make `BRAIN_SPECIES.len()` disagree with the set size while every
        // `contains` check still passed.
        let mut sorted = BRAIN_SPECIES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.as_slice(), BRAIN_SPECIES, "list must be sorted+unique");
        assert_eq!(BRAIN_SPECIES.len(), 20, "the crate census counts 20");
    }

    #[test]
    fn brain_species_get_a_brain_and_goal_species_do_not() {
        assert!(brain_for("villager").is_some());
        assert!(brain_for("warden").is_some());
        assert!(brain_for("camel").is_some());
        // The negative half, and it is the one that matters: a goal-system mob
        // must not acquire a brain, or it would get the scaffold's stroll *and*
        // its roster's stroll, both writing movement.
        assert!(brain_for("zombie").is_none());
        assert!(brain_for("cow").is_none());
        assert!(brain_for("creeper").is_none());
        // Near-misses that are genuinely goal-driven despite the name: a
        // zombified piglin is a `Zombie` subclass and a zombie villager is a
        // `Zombie`, so neither has a brain even though "piglin" and "villager"
        // are substrings. A `contains`-style match would get both wrong.
        assert!(brain_for("zombified_piglin").is_none());
        assert!(brain_for("zombie_villager").is_none());
    }

    /// Named for exactly what it checks. An earlier draft called this
    /// `..._and_the_sensor` while asserting nothing about sensors — a small
    /// instance of the same disease as the rest of this issue, so it is worth not
    /// shipping.
    #[test]
    fn the_scaffold_starts_with_core_and_idle_active() {
        let brain = scaffold(SCAFFOLD_STROLL_SPEED, SCAFFOLD_LOOK_DISTANCE);
        assert!(brain.is_active(Activity::CORE), "CORE is always active");
        assert!(brain.is_active(Activity::IDLE), "IDLE is the default");
    }
}
