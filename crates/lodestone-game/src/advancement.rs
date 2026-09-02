//! The advancement tree and the local player's progress on it.
//!
//! ## What it is
//!
//! The client-side mirror of vanilla's `ClientAdvancements`: the nodes the server
//! sent (id, parent, display, requirements) plus per-criterion obtained times.
//! [`AdvancementStore::apply`] folds [`ClientEvent::AdvancementsUpdated`] and
//! nothing else.
//!
//! ## How it works
//!
//! The packet is a delta. `reset` (vanilla's first packet on join) clears
//! everything and treats `added` as the whole tree; otherwise `added` upserts,
//! `removed` deletes, and `progress` replaces the criterion map of the ids it
//! names — an id absent from `progress` keeps whatever it had.
//!
//! Completion is vanilla's AND-of-ORs over the node's own `requirements`: done
//! when **every** group has at least one obtained criterion. An empty group list
//! is never done, matching `AdvancementRequirements.test`.
//!
//! ## How to change it
//!
//! Progress is stored keyed by id even for an id with no node, because the two
//! arrive in the same packet but nothing guarantees the ordering across packets —
//! dropping unknown-id progress would lose it permanently. [`completion`] is
//! therefore `None` for a progress entry with no node, not `false`.

use std::collections::BTreeMap;

use lodestone_model::{
    event::{AdvancementEntry, ClientEvent},
    ids::Identifier,
};

/// One advancement's progress: criterion name → obtained epoch-millis.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    criteria: BTreeMap<String, Option<i64>>,
}

impl Progress {
    /// Whether one criterion is obtained.
    #[must_use]
    pub fn is_criterion_done(&self, criterion: &str) -> bool {
        self.criteria.get(criterion).is_some_and(Option::is_some)
    }

    /// How many criteria are obtained — the numerator of vanilla's `x/y`
    /// progress readout.
    #[must_use]
    pub fn obtained_count(&self) -> usize {
        self.criteria.values().filter(|slot| slot.is_some()).count()
    }

    /// When a criterion was obtained, if it was.
    #[must_use]
    pub fn obtained_at(&self, criterion: &str) -> Option<i64> {
        self.criteria.get(criterion).copied().flatten()
    }

    /// AND-of-ORs completion against a requirement shape. An empty shape is
    /// never done.
    #[must_use]
    pub fn is_done(&self, requirements: &[Vec<String>]) -> bool {
        !requirements.is_empty()
            && requirements
                .iter()
                .all(|group| group.iter().any(|name| self.is_criterion_done(name)))
    }
}

/// The tree plus progress, as the client knows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AdvancementStore {
    nodes: BTreeMap<Identifier, AdvancementEntry>,
    progress: BTreeMap<Identifier, Progress>,
    show_advancements: bool,
}

impl AdvancementStore {
    /// Fold one event. Returns `true` when this store changed.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        let ClientEvent::AdvancementsUpdated {
            reset,
            added,
            removed,
            progress,
            show_advancements,
        } = event
        else {
            return false;
        };
        if *reset {
            self.nodes.clear();
            self.progress.clear();
        }
        for entry in added {
            self.nodes.insert(entry.id.clone(), entry.clone());
        }
        for id in removed {
            self.nodes.remove(id);
            self.progress.remove(id);
        }
        for (id, criteria) in progress {
            self.progress.insert(
                id.clone(),
                Progress {
                    criteria: criteria.iter().cloned().collect(),
                },
            );
        }
        self.show_advancements = *show_advancements;
        true
    }

    /// One node, if the server sent it.
    #[must_use]
    pub fn get(&self, id: &Identifier) -> Option<&AdvancementEntry> {
        self.nodes.get(id)
    }

    /// One node's progress, if any has arrived.
    #[must_use]
    pub fn progress(&self, id: &Identifier) -> Option<&Progress> {
        self.progress.get(id)
    }

    /// Whether an advancement is complete: `None` when it has no node, so a
    /// caller cannot mistake "not sent" for "not obtained".
    #[must_use]
    pub fn completion(&self, id: &Identifier) -> Option<bool> {
        let node = self.nodes.get(id)?;
        Some(
            self.progress
                .get(id)
                .is_some_and(|progress| progress.is_done(&node.requirements)),
        )
    }

    /// Every node, ordered by id.
    pub fn nodes(&self) -> impl Iterator<Item = &AdvancementEntry> + '_ {
        self.nodes.values()
    }

    /// The roots (a tab each), ordered by id.
    pub fn roots(&self) -> impl Iterator<Item = &AdvancementEntry> + '_ {
        self.nodes.values().filter(|node| node.parent.is_none())
    }

    /// How many nodes are known.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether no advancement has arrived.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The server's `showAdvancements` flag.
    #[must_use]
    pub fn show_advancements(&self) -> bool {
        self.show_advancements
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(path: &str) -> Identifier {
        path.parse().expect("valid identifier")
    }

    fn node(path: &str, requirements: Vec<Vec<String>>) -> AdvancementEntry {
        AdvancementEntry {
            id: id(path),
            parent: None,
            display: None,
            requirements,
            sends_telemetry_event: false,
        }
    }

    fn groups(names: &[&[&str]]) -> Vec<Vec<String>> {
        names
            .iter()
            .map(|group| group.iter().map(|s| (*s).to_string()).collect())
            .collect()
    }

    /// AND-of-ORs: an `allOf` advancement (one criterion per group) needs both,
    /// an `anyOf` (both in one group) needs either.
    #[test]
    fn completion_is_and_of_ors() {
        let all_of = node("minecraft:story/a", groups(&[&["one"], &["two"]]));
        let any_of = node("minecraft:story/b", groups(&[&["one", "two"]]));
        let mut store = AdvancementStore::default();
        store.apply(&ClientEvent::AdvancementsUpdated {
            reset: true,
            added: vec![all_of, any_of],
            removed: Vec::new(),
            progress: vec![
                (
                    id("minecraft:story/a"),
                    vec![("one".into(), Some(1_700_000_000_000)), ("two".into(), None)],
                ),
                (
                    id("minecraft:story/b"),
                    vec![("one".into(), Some(1_700_000_000_000)), ("two".into(), None)],
                ),
            ],
            show_advancements: true,
        });
        assert_eq!(store.completion(&id("minecraft:story/a")), Some(false));
        assert_eq!(store.completion(&id("minecraft:story/b")), Some(true));
        assert_eq!(store.completion(&id("minecraft:story/never_sent")), None);
        assert_eq!(
            store
                .progress(&id("minecraft:story/a"))
                .unwrap()
                .obtained_at("one"),
            Some(1_700_000_000_000)
        );
        assert!(store.show_advancements());
    }

    /// A `reset` packet replaces the tree; a non-reset one upserts and removes.
    #[test]
    fn reset_replaces_and_a_delta_does_not() {
        let mut store = AdvancementStore::default();
        store.apply(&ClientEvent::AdvancementsUpdated {
            reset: true,
            added: vec![node("minecraft:a", groups(&[&["c"]]))],
            removed: Vec::new(),
            progress: Vec::new(),
            show_advancements: true,
        });
        store.apply(&ClientEvent::AdvancementsUpdated {
            reset: false,
            added: vec![node("minecraft:b", groups(&[&["c"]]))],
            removed: Vec::new(),
            progress: Vec::new(),
            show_advancements: true,
        });
        assert_eq!(store.len(), 2);
        store.apply(&ClientEvent::AdvancementsUpdated {
            reset: false,
            added: Vec::new(),
            removed: vec![id("minecraft:a")],
            progress: Vec::new(),
            show_advancements: true,
        });
        assert_eq!(store.len(), 1);
        assert!(store.get(&id("minecraft:b")).is_some());
        store.apply(&ClientEvent::AdvancementsUpdated {
            reset: true,
            added: vec![node("minecraft:c", groups(&[&["c"]]))],
            removed: Vec::new(),
            progress: Vec::new(),
            show_advancements: true,
        });
        assert_eq!(store.len(), 1);
        assert!(store.get(&id("minecraft:b")).is_none());
        assert_eq!(store.roots().count(), 1);
    }
}
