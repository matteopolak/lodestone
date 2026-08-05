//! The production driver: a [`Brain`] that runs inside a real
//! [`GoalSelector`](crate::ai::GoalSelector).
//!
//! # What this file exists to fix
//!
//! Before it, `lodestone-entity::brain` was an **island** in the exact sense
//! `CLAUDE.md` names as this repo's dominant defect class: ~1900 lines of
//! sensor/behaviour/activity machinery, a green hermetic suite, a live gate
//! proving `Brain::tick`'s ordering — and
//! `grep -rn 'lodestone_entity::brain\|BrainMob'` outside the crate returned
//! **nothing**. No mob in any world had a brain. The goal system had already been
//! in that state once and
//! [`NavigatingMob`](crate::ai::NavigatingMob)'s own module doc calls it out:
//! "two islands joined by a seam a fake always stubs".
//!
//! # Why a `Goal` and not a second driver entry point
//!
//! Vanilla's `Mob` holds `goalSelector` **and** `brain`, and
//! `Mob.customServerAiStep` ticks both. The tempting mirror of that here is a
//! second host-side call — `MobSim` learns to call `mob.tick_brain(&mut brain)`
//! next to `mob.tick(&mut goals)`. That is how the first island was built: a
//! subsystem whose only route to production is a call site somebody has to
//! remember to add.
//!
//! [`BrainGoal`] takes the other route. It is an ordinary [`Goal`], so it travels
//! the path that is **already wired**: `roster::goals_for` builds it,
//! `MobSim::spawn_species` installs it, `MobSim::tick` ticks it. Zero host
//! changes, and the brain cannot be silently dropped, because dropping it would
//! mean dropping the goal list every other species depends on.
//!
//! Its priority is `0` and it holds [`Flag::Move`] + [`Flag::Look`] — the two
//! things a brain's `MoveToTargetSink`/`LookAtTargetSink` command. That is not a
//! trick to win arbitration: a brain-driven vanilla mob genuinely has no
//! competing movement goal, because its movement lives in behaviours instead.
//! Holding the flags means a stray goal installed alongside a brain loses to it
//! rather than fighting it every tick.

use super::{Activity, Brain};
use crate::ai::{Flag, FlagSet, Goal, MobController};

/// A [`Brain`] packaged as a [`Goal`], so a host that ticks a
/// [`GoalSelector`](crate::ai::GoalSelector) ticks the brain.
///
/// The goal is always eligible and never stops: a brain is not a behaviour that
/// starts and finishes, it *is* the mob's AI, and vanilla ticks it every tick
/// unconditionally. Arbitration among the mob's actual behaviours happens inside
/// the brain, through memory, which is the whole point of the architecture.
pub struct BrainGoal {
    brain: Brain,
    /// Activities offered to
    /// [`Brain::set_active_activity_to_first_valid`] each tick, in precedence
    /// order. This is vanilla's per-species `updateActivity` — e.g.
    /// `WardenAi.updateActivity` offers `[EMERGE, DIG, FIGHT, INVESTIGATE,
    /// SNIFF, IDLE]`. For the generic scaffold it is `[IDLE]`.
    candidates: Vec<Activity>,
}

impl BrainGoal {
    /// Wraps `brain`, re-evaluating `candidates` each tick.
    ///
    /// `candidates` must end in a fallback whose requirements are unconditional
    /// (normally [`Activity::IDLE`]); [`Brain::set_active_activity_to_first_valid`]
    /// leaves the active set **unchanged** when nothing matches, so a list of
    /// exclusively conditional activities silently freezes the mob in whatever it
    /// was last doing.
    #[must_use]
    pub fn new(brain: Brain, candidates: Vec<Activity>) -> Self {
        Self { brain, candidates }
    }

    /// A brain whose only offered activity is [`Activity::IDLE`] — the CORE+IDLE
    /// scaffold every vanilla brain mob shares.
    #[must_use]
    pub fn idle(brain: Brain) -> Self {
        Self::new(brain, vec![Activity::IDLE])
    }

    /// The wrapped brain, for inspection.
    #[must_use]
    pub fn brain(&self) -> &Brain {
        &self.brain
    }

    /// The wrapped brain, mutably — how a host injects a memory (an attack
    /// target, a dig cooldown) from outside.
    pub fn brain_mut(&mut self) -> &mut Brain {
        &mut self.brain
    }
}

impl std::fmt::Debug for BrainGoal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrainGoal")
            .field("candidates", &self.candidates)
            .field("brain", &self.brain)
            .finish()
    }
}

impl Goal for BrainGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Look])
    }

    fn can_use(&mut self, _mob: &mut dyn MobController) -> bool {
        true
    }

    fn can_continue_to_use(&mut self, _mob: &mut dyn MobController) -> bool {
        true
    }

    /// Nothing preempts a brain. Vanilla has no goal that can, because a
    /// brain-driven mob's movement is not a goal in the first place.
    fn is_interruptable(&self) -> bool {
        false
    }

    /// Runs one brain tick, after re-evaluating which activity should be active.
    ///
    /// The order matters and matches vanilla `Mob.customServerAiStep` →
    /// species `Ai.updateActivity` → `brain.tick`: the activity switch happens
    /// **before** the tick, so a behaviour that became eligible this tick is
    /// scheduled this tick rather than next.
    ///
    /// A `mob` that answers `None` to
    /// [`brain_mob`](MobController::brain_mob) — every test double — makes this a
    /// no-op. That is deliberate; see that method's doc.
    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(brain_mob) = mob.brain_mob() else {
            return;
        };
        self.brain
            .set_active_activity_to_first_valid(&self.candidates);
        self.brain.tick(brain_mob);
    }
}
