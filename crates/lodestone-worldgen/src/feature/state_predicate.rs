//! [`StatePredicate`]: a per-block-state boolean answered from a canonical
//! state-id table, for feature predicates built from a resolver document.

use std::collections::{HashMap, HashSet};
use lodestone_data::block_states::{BlockStateValue, StateId, STATE_COUNT};
use lodestone_worldgen_core::hash::{FastMap, FastSet};
use serde_json::Value;

/// A per-block-state boolean, stored as "the answer for each block's default
/// state" plus an override for every state that disagrees with its own default.
///
/// This is the compaction the resolver JSON uses: for most blocks every state
/// agrees, so a per-block answer plus a short override list is far smaller than
/// a per-state map while staying **exact**, because the overrides are complete
/// rather than a curated subset. Text exists only at the input boundary; the
/// complete answer table is stored in canonical state-id order.
#[derive(Debug)]
pub(crate) struct StatePredicate {
    /// Built-in defaults parsed once into the generated state's typed domain.
    /// Each entry is a block's canonical default state; all of that block's
    /// states inherit the answer unless an exact typed override exists.
    builtin_defaults: FastSet<StateId>,
    /// Exact built-in overrides parsed once from the resolver document.
    builtin_states: FastMap<StateId, bool>,
    answers: Box<[bool]>,
}

impl Clone for StatePredicate {
    fn clone(&self) -> Self {
        Self {
            builtin_defaults: self.builtin_defaults.clone(),
            builtin_states: self.builtin_states.clone(),
            answers: self.answers.clone(),
        }
    }
}

impl Default for StatePredicate {
    fn default() -> Self {
        Self {
            builtin_defaults: FastSet::default(),
            builtin_states: FastMap::default(),
            answers: vec![false; STATE_COUNT as usize].into_boxed_slice(),
        }
    }
}

impl StatePredicate {
    /// Builds from the two halves. `overrides` must list **every** disagreeing
    /// state; a partial list is silently wrong, which is why it is produced by a
    /// full walk of the state registry rather than by hand.
    ///
    /// Runs once per predicate, so the re-hash into the internal
    /// [`FastSet`]/[`FastMap`] is paid at construction and never again.
    #[must_use]
    pub(crate) fn new(by_block_default: HashSet<String>, overrides: HashMap<String, bool>) -> Self {
        let builtin_defaults = by_block_default
            .iter()
            .filter_map(|state| exact_builtin_state(state))
            .map(|state| state.block().default_state())
            .collect();
        let builtin_states = overrides
            .iter()
            .filter_map(|(state, &answer)| {
                exact_builtin_state(state).map(|state| (state, answer))
            })
            .collect();
        let mut predicate = Self {
            builtin_defaults,
            builtin_states,
            answers: vec![false; STATE_COUNT as usize].into_boxed_slice(),
        };
        for raw in 0..STATE_COUNT {
            let state = StateId::new(raw as u32).expect("generated state id");
            predicate.answers[state.index()] = predicate.builtin_answer(state);
        }
        predicate
    }

    /// Text lookup is retained only for unit tests at the config boundary.
    #[cfg(test)]
    #[must_use]
    fn test(&self, state: &str) -> bool {
        exact_builtin_state(state).is_some_and(|state| self.builtin_answer(state))
    }

    /// The built-in answer in the typed state domain.
    #[inline]
    fn builtin_answer(&self, state: StateId) -> bool {
        self.builtin_states
            .get(&state)
            .copied()
            .unwrap_or_else(|| self.builtin_defaults.contains(&state.block().default_state()))
    }

    /// Tests a canonical state without resolving its spelling.
    #[inline]
    #[must_use]
    pub(crate) fn test_id(&self, id: StateId) -> bool {
        self.answers[id.index()]
    }

    /// `true` when nothing was supplied — the "no data supplied" convention
    /// every other resolver-fed table in this crate follows.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.builtin_defaults.is_empty() && self.builtin_states.is_empty()
    }

    /// Parses `{"default": ["minecraft:stone", ...], "states": {"minecraft:snow[layers=8]": false, ...}}`.
    ///
    /// # Panics
    /// Panics on a malformed document — this is embedded, generated data, so a
    /// shape error is a build-time defect rather than untrusted input.
    #[must_use]
    pub(crate) fn parse(value: &Value) -> Self {
        if value.is_null() {
            return Self::default();
        }
        let by_block_default = value
            .get("default")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().expect("default entry is a string").to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let overrides = value
            .get("states")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            v.as_bool().expect("states entry is a boolean"),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self::new(by_block_default, overrides)
    }
}

/// Parses only an exact generated state. The forgiving `StateId::from_state_str`
/// API intentionally accepts shorthand/default forms, but a predicate override
/// with an unknown property must not be silently applied to a block default.
fn exact_builtin_state(encoded: &str) -> Option<StateId> {
    BlockStateValue::parse(encoded).state_id()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two-level lookup: an exact state wins, and a property-less name falls
    /// back to the block's default-state answer. This is what keeps the
    /// `level`-less `minecraft:water` matching.
    #[test]
    fn state_predicate_falls_back_from_state_to_block_default() {
        let mut by_state = HashMap::new();
        by_state.insert("minecraft:water[level=1]".to_owned(), false);
        let mut default = HashSet::new();
        default.insert("minecraft:water".to_owned());
        let p = StatePredicate::new(default, by_state);

        assert!(p.test("minecraft:water"), "property-less name uses the default state");
        assert!(
            p.test("minecraft:water[level=0]"),
            "the default state itself is not in `states`, so it falls through to the default"
        );
        assert!(!p.test("minecraft:water[level=1]"), "an override wins");
        assert!(!p.test("minecraft:stone"), "an unknown block is false");
    }

    #[test]
    fn state_predicate_late_builtin_state_is_correct_before_and_after_rebind() {
        let mut defaults = HashSet::new();
        defaults.insert("minecraft:stone".to_owned());
        let predicate = StatePredicate::new(defaults, HashMap::new());
        let stone = StateId::from_state_str("minecraft:stone").expect("stone state");
        let dirt = StateId::from_state_str("minecraft:dirt").expect("dirt state");
        assert!(predicate.test_id(stone));
        assert!(!predicate.test_id(dirt));
    }
}
