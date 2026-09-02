//! The scoreboard store — objectives and per-holder
//! scores, behind [`ScoreboardHandle`].
//!
//! # What it is
//!
//! The real scoreboard record carries objectives, one score table keyed
//! `(holder, objective)`, display slots and teams. This is the subset a
//! command layer needs: objectives and scores. No criteria are *simulated* —
//! a `dummy` objective and a `minecraft.custom:minecraft.deaths` objective
//! behave identically here, because nothing in this server increments a
//! score on its own. Every score changes because a command (or `/execute
//! store`, not yet built — see `crate::commands::execute`'s module doc)
//! asked for it to. **Teams are a separate store** —
//! `crate::commands::team_store`, reached through the identical
//! `WorldStateHandle`-sibling shape this module uses, not folded in here,
//! matching the real scoreboard record keeping objectives/scores and teams as
//! two tables. Display slots (`/scoreboard objectives setdisplay`) are still
//! not modelled at all: nothing in this crate renders a sidebar, so a stored
//! display slot would be write-only.
//!
//! # How it works
//!
//! [`ScoreboardHandle`] is `Arc<Mutex<ScoreboardState>>`, shaped exactly like
//! [`crate::world_state::WorldStateHandle`]: cheap to clone, and every clone
//! shares the store. It rides *inside* `WorldStateHandle` as a sibling field
//! next to `anchors` rather than as its own parameter threaded through
//! `CommandWorld`, `RconConfig` and the command-block tick loop
//! independently — `WorldStateHandle` is already the one handle proven to
//! reach every command entry point (a live connection's `ChatCommand` arm,
//! RCON, and `crate::tick`'s `TICK_COMMAND_BLOCK` drain all already hold
//! one), so a second field on it is reached by all three for free. Adding a
//! fourth parameter next to `rules`/`state` on `CommandWorld` would not be:
//! see this module's own history for why (`crate::game_rules`'s
//! `RuleStore` split exists precisely because a *second*, disconnected store
//! was built once already and nothing read it).
//!
//! # How to change it
//!
//! Read/write access is via `WorldStateHandle::scoreboard`
//! (`crate::world_state`), never a second constructor — a
//! `ScoreboardHandle::new()` built anywhere outside `WorldStateHandle`'s own
//! `Default` is a fresh, disconnected store, the exact island shape
//! `crate::commands`' own module doc records for the pre-fix `/gamerule`.
//!
//! # Configuration
//!
//! None.
//!
//! # Dependencies
//!
//! None outside `std`. `lodestone_command_mc::ScoreOperation` is reused by
//! [`ScoreboardHandle::operation`] rather than re-declared here, so the
//! parser and the mutator agree on the same nine tokens by construction.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_command_mc::ScoreOperation;

/// One registered objective. Criteria and the display name are stored for
/// `/scoreboard objectives list` to echo back; neither is interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Objective {
    pub name: String,
    pub criteria: String,
    pub display_name: String,
}

#[derive(Debug, Default)]
struct ScoreboardState {
    /// Insertion order, matching the real objective-registry's own iteration
    /// order closely enough for `objectives list` to read the same way every
    /// time it is called.
    objectives: Vec<Objective>,
    /// `(holder, objective) -> score`. A `HashMap<String, HashMap<String,
    /// i32>>` rather than one map keyed by a tuple: `/scoreboard players
    /// reset <holder>` (no objective given) removes one holder's whole inner
    /// map in one operation, and `/scoreboard players list` (no holder
    /// given) needs exactly the outer key set.
    scores: HashMap<String, HashMap<String, i32>>,
}

impl ScoreboardState {
    fn objective(&self, name: &str) -> Option<&Objective> {
        self.objectives.iter().find(|o| o.name == name)
    }
}

/// A cheap, cloneable handle to one world's scoreboard. See the module doc
/// for why this is reached through [`crate::world_state::WorldStateHandle::scoreboard`]
/// rather than constructed directly.
#[derive(Debug, Clone, Default)]
pub struct ScoreboardHandle(Arc<Mutex<ScoreboardState>>);

/// Why a scoreboard operation could not run — every variant is a player-
/// facing message, matching the real command's own error text
/// closely enough to be recognisable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScoreboardError {
    ObjectiveAlreadyExists(String),
    UnknownObjective(String),
    NoScore { holder: String, objective: String },
}

impl std::fmt::Display for ScoreboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ObjectiveAlreadyExists(name) => {
                write!(f, "An objective already exists by the name '{name}'")
            }
            Self::UnknownObjective(name) => write!(f, "Unknown scoreboard objective '{name}'"),
            Self::NoScore { holder, objective } => {
                write!(f, "Can't get value of {objective} for {holder}; none is set")
            }
        }
    }
}

impl ScoreboardHandle {
    fn with<R>(&self, f: impl FnOnce(&mut ScoreboardState) -> R) -> R {
        f(&mut self.0.lock().expect("scoreboard lock poisoned"))
    }

    /// The real add-objective rule — refuses a duplicate name, the real
    /// "already exists" error.
    pub fn add_objective(
        &self,
        name: &str,
        criteria: &str,
        display_name: &str,
    ) -> Result<(), ScoreboardError> {
        self.with(|state| {
            if state.objective(name).is_some() {
                return Err(ScoreboardError::ObjectiveAlreadyExists(name.to_string()));
            }
            state.objectives.push(Objective {
                name: name.to_string(),
                criteria: criteria.to_string(),
                display_name: display_name.to_string(),
            });
            Ok(())
        })
    }

    /// The real remove-objective rule — also purges every score recorded under
    /// it, matching the real cleanup (a removal loop over every holder's
    /// scores in the same rule).
    pub fn remove_objective(&self, name: &str) -> Result<(), ScoreboardError> {
        self.with(|state| {
            let before = state.objectives.len();
            state.objectives.retain(|o| o.name != name);
            if state.objectives.len() == before {
                return Err(ScoreboardError::UnknownObjective(name.to_string()));
            }
            for holder_scores in state.scores.values_mut() {
                holder_scores.remove(name);
            }
            Ok(())
        })
    }

    /// Every registered objective, in registration order.
    #[must_use]
    pub fn objectives(&self) -> Vec<Objective> {
        self.with(|state| state.objectives.clone())
    }

    #[must_use]
    pub fn has_objective(&self, name: &str) -> bool {
        self.with(|state| state.objective(name).is_some())
    }

    /// `/scoreboard players set` — an absolute value, created if absent.
    pub fn set_score(&self, holder: &str, objective: &str, value: i32) -> Result<i32, ScoreboardError> {
        self.with(|state| {
            if state.objective(objective).is_none() {
                return Err(ScoreboardError::UnknownObjective(objective.to_string()));
            }
            state.scores.entry(holder.to_string()).or_default().insert(objective.to_string(), value);
            Ok(value)
        })
    }

    /// `/scoreboard players add` — a non-negative delta, per vanilla's own
    /// `integer(0)` bound on the argument (enforced by the command layer,
    /// not here); this saturates rather than panics on overflow either way.
    pub fn add_score(&self, holder: &str, objective: &str, delta: i32) -> Result<i32, ScoreboardError> {
        self.adjust(holder, objective, |current| current.saturating_add(delta))
    }

    /// `/scoreboard players remove`.
    pub fn remove_score(&self, holder: &str, objective: &str, delta: i32) -> Result<i32, ScoreboardError> {
        self.adjust(holder, objective, |current| current.saturating_sub(delta))
    }

    fn adjust(
        &self,
        holder: &str,
        objective: &str,
        f: impl FnOnce(i32) -> i32,
    ) -> Result<i32, ScoreboardError> {
        self.with(|state| {
            if state.objective(objective).is_none() {
                return Err(ScoreboardError::UnknownObjective(objective.to_string()));
            }
            let entry = state.scores.entry(holder.to_string()).or_default();
            let current = entry.get(objective).copied().unwrap_or(0);
            let next = f(current);
            entry.insert(objective.to_string(), next);
            Ok(next)
        })
    }

    /// `/scoreboard players get` / `/execute if score`'s single-value read.
    pub fn get_score(&self, holder: &str, objective: &str) -> Result<i32, ScoreboardError> {
        self.with(|state| {
            if state.objective(objective).is_none() {
                return Err(ScoreboardError::UnknownObjective(objective.to_string()));
            }
            state
                .scores
                .get(holder)
                .and_then(|scores| scores.get(objective))
                .copied()
                .ok_or_else(|| ScoreboardError::NoScore {
                    holder: holder.to_string(),
                    objective: objective.to_string(),
                })
        })
    }

    /// `/scoreboard players reset <holder> <objective>` — one score. `false`
    /// if there was nothing to remove (still a success in vanilla).
    pub fn reset_score(&self, holder: &str, objective: &str) -> bool {
        self.with(|state| {
            state.scores.get_mut(holder).is_some_and(|scores| scores.remove(objective).is_some())
        })
    }

    /// `/scoreboard players reset <holder>` — every objective for one holder.
    pub fn reset_all(&self, holder: &str) {
        self.with(|state| {
            state.scores.remove(holder);
        });
    }

    /// `(objective, score)` for one holder, sorted by objective name for a
    /// deterministic listing.
    #[must_use]
    pub fn scores_for(&self, holder: &str) -> Vec<(String, i32)> {
        self.with(|state| {
            let mut entries: Vec<(String, i32)> = state
                .scores
                .get(holder)
                .map(|scores| scores.iter().map(|(k, v)| (k.clone(), *v)).collect())
                .unwrap_or_default();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            entries
        })
    }

    /// Every holder with at least one recorded score — `*`'s resolution for
    /// `/scoreboard players list`/`reset`/`operation`, and the real
    /// tracked-players query.
    #[must_use]
    pub fn tracked_holders(&self) -> Vec<String> {
        self.with(|state| {
            let mut holders: Vec<String> = state.scores.keys().cloned().collect();
            holders.sort();
            holders
        })
    }

    /// `/scoreboard players operation` — the real score-operation rule's nine
    /// cases. Both scores are created (defaulting to `0`) if absent, matching
    /// the real get-or-create-score rule. Returns the target's new
    /// score.
    #[allow(clippy::too_many_lines)]
    pub fn operation(
        &self,
        target_holder: &str,
        target_objective: &str,
        op: ScoreOperation,
        source_holder: &str,
        source_objective: &str,
    ) -> Result<i32, ScoreboardError> {
        self.with(|state| {
            if state.objective(target_objective).is_none() {
                return Err(ScoreboardError::UnknownObjective(target_objective.to_string()));
            }
            if state.objective(source_objective).is_none() {
                return Err(ScoreboardError::UnknownObjective(source_objective.to_string()));
            }
            let target = state
                .scores
                .get(target_holder)
                .and_then(|s| s.get(target_objective))
                .copied()
                .unwrap_or(0);
            let source = state
                .scores
                .get(source_holder)
                .and_then(|s| s.get(source_objective))
                .copied()
                .unwrap_or(0);
            let (new_target, new_source) = match op {
                ScoreOperation::Assign => (source, source),
                ScoreOperation::Add => (target.saturating_add(source), source),
                ScoreOperation::Subtract => (target.saturating_sub(source), source),
                ScoreOperation::Multiply => (target.saturating_mul(source), source),
                ScoreOperation::Divide => {
                    (if source == 0 { target } else { target.saturating_div(source) }, source)
                }
                ScoreOperation::Modulo => {
                    (if source == 0 { target } else { target.rem_euclid(source) }, source)
                }
                ScoreOperation::Min => (target.min(source), source),
                ScoreOperation::Max => (target.max(source), source),
                ScoreOperation::Swap => (source, target),
            };
            state
                .scores
                .entry(target_holder.to_string())
                .or_default()
                .insert(target_objective.to_string(), new_target);
            state
                .scores
                .entry(source_holder.to_string())
                .or_default()
                .insert(source_objective.to_string(), new_source);
            Ok(new_target)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_score_cannot_be_set_against_an_unknown_objective() {
        let handle = ScoreboardHandle::default();
        assert_eq!(
            handle.set_score("Steve", "nope", 5),
            Err(ScoreboardError::UnknownObjective("nope".to_string()))
        );
    }

    #[test]
    fn set_then_get_round_trips_and_reset_clears_it() {
        let handle = ScoreboardHandle::default();
        handle.add_objective("kills", "dummy", "Kills").unwrap();
        assert_eq!(handle.set_score("Steve", "kills", 7), Ok(7));
        assert_eq!(handle.get_score("Steve", "kills"), Ok(7));
        assert!(handle.reset_score("Steve", "kills"));
        assert_eq!(
            handle.get_score("Steve", "kills"),
            Err(ScoreboardError::NoScore { holder: "Steve".to_string(), objective: "kills".to_string() })
        );
    }

    #[test]
    fn add_and_remove_are_deltas_against_a_default_of_zero() {
        let handle = ScoreboardHandle::default();
        handle.add_objective("kills", "dummy", "Kills").unwrap();
        assert_eq!(handle.add_score("Steve", "kills", 3), Ok(3));
        assert_eq!(handle.add_score("Steve", "kills", 4), Ok(7));
        assert_eq!(handle.remove_score("Steve", "kills", 2), Ok(5));
    }

    #[test]
    fn removing_an_objective_purges_its_scores_but_not_others() {
        let handle = ScoreboardHandle::default();
        handle.add_objective("a", "dummy", "A").unwrap();
        handle.add_objective("b", "dummy", "B").unwrap();
        handle.set_score("Steve", "a", 1).unwrap();
        handle.set_score("Steve", "b", 2).unwrap();
        handle.remove_objective("a").unwrap();
        assert!(handle.get_score("Steve", "a").is_err());
        assert_eq!(handle.get_score("Steve", "b"), Ok(2));
    }

    /// Every one of the nine operation tokens, against pairwise-distinct
    /// operands so a transposition (e.g. `min`/`max` swapped) would fail.
    #[test]
    fn every_operation_token_computes_its_own_distinct_result() {
        let handle = ScoreboardHandle::default();
        handle.add_objective("x", "dummy", "X").unwrap();
        let case = |op: ScoreOperation, target: i32, source: i32| {
            let handle = ScoreboardHandle::default();
            handle.add_objective("x", "dummy", "X").unwrap();
            handle.set_score("t", "x", target).unwrap();
            handle.set_score("s", "x", source).unwrap();
            handle.operation("t", "x", op, "s", "x").unwrap()
        };
        assert_eq!(case(ScoreOperation::Assign, 11, 4), 4);
        assert_eq!(case(ScoreOperation::Add, 11, 4), 15);
        assert_eq!(case(ScoreOperation::Subtract, 11, 4), 7);
        assert_eq!(case(ScoreOperation::Multiply, 11, 4), 44);
        assert_eq!(case(ScoreOperation::Divide, 11, 4), 2);
        assert_eq!(case(ScoreOperation::Modulo, 11, 4), 3);
        assert_eq!(case(ScoreOperation::Min, 11, 4), 4);
        assert_eq!(case(ScoreOperation::Max, 11, 4), 11);

        // Swap needs to be checked on both sides.
        let handle = ScoreboardHandle::default();
        handle.add_objective("x", "dummy", "X").unwrap();
        handle.set_score("t", "x", 11).unwrap();
        handle.set_score("s", "x", 4).unwrap();
        let new_target = handle.operation("t", "x", ScoreOperation::Swap, "s", "x").unwrap();
        assert_eq!(new_target, 4, "target takes the source's old value");
        assert_eq!(handle.get_score("s", "x"), Ok(11), "source takes the target's old value");
    }

    #[test]
    fn dividing_or_modulo_by_zero_leaves_the_target_unchanged_rather_than_panicking() {
        let handle = ScoreboardHandle::default();
        handle.add_objective("x", "dummy", "X").unwrap();
        handle.set_score("t", "x", 11).unwrap();
        handle.set_score("s", "x", 0).unwrap();
        assert_eq!(handle.operation("t", "x", ScoreOperation::Divide, "s", "x"), Ok(11));
        assert_eq!(handle.operation("t", "x", ScoreOperation::Modulo, "s", "x"), Ok(11));
    }

    #[test]
    fn tracked_holders_lists_every_holder_with_at_least_one_score() {
        let handle = ScoreboardHandle::default();
        handle.add_objective("x", "dummy", "X").unwrap();
        handle.set_score("Steve", "x", 1).unwrap();
        handle.set_score("Alex", "x", 2).unwrap();
        assert_eq!(handle.tracked_holders(), vec!["Alex".to_string(), "Steve".to_string()]);
    }

    #[test]
    fn two_handles_from_one_default_do_not_share_a_store() {
        // The negative control matching `WorldStateHandle`'s own sharing
        // gate: two independently-defaulted handles must NOT be the same
        // store, which is exactly the island shape this module's doc warns
        // against for a `ScoreboardHandle` built outside `WorldStateHandle`.
        let a = ScoreboardHandle::default();
        let b = ScoreboardHandle::default();
        a.add_objective("x", "dummy", "X").unwrap();
        assert!(!b.has_objective("x"));
    }
}
