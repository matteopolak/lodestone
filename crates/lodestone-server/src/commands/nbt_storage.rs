//! The NBT command-storage engine, behind [`NbtStorageHandle`]. `/data ...
//! storage` and `/execute if`/`unless data storage` are its consumers; `/data`
//! on an entity or block entity reads a live view instead
//! (`crate::commands::entity_data`).
//!
//! # How it works
//!
//! [`NbtStorageHandle`] is `Arc<Mutex<HashMap<String, Vec<(String,
//! SnbtValue)>>>>`, shaped like every other store in this module
//! ([`crate::commands::scoreboard_store::ScoreboardHandle`],
//! [`crate::commands::team_store::TeamHandle`]) and riding inside
//! [`crate::world_state::WorldStateHandle`] for the same reachability reason.
//! Reads and writes go through [`lodestone_command_mc::NbtPath`], the one path
//! engine, so storage and entity data share the grammar and the "nothing
//! changed" counts. [`merge`](NbtStorageHandle::merge) is the reference
//! compound merge.
//!
//! # How to change it
//!
//! Reach it via `WorldStateHandle::nbt_storage`, never a second constructor.
//!
//! # Configuration
//!
//! None.
//!
//! # Dependencies
//!
//! `lodestone_command_mc::{SnbtValue, NbtPath}`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_command_mc::{NbtPath, SnbtValue};

/// A cheap, cloneable handle to one world's NBT command storage. See the
/// module doc for why this is reached through
/// [`crate::world_state::WorldStateHandle::nbt_storage`] rather than
/// constructed directly.
#[derive(Debug, Clone, Default)]
pub struct NbtStorageHandle(Arc<Mutex<HashMap<String, Vec<(String, SnbtValue)>>>>);

impl NbtStorageHandle {
    fn with<R>(&self, f: impl FnOnce(&mut HashMap<String, Vec<(String, SnbtValue)>>) -> R) -> R {
        f(&mut self.0.lock().expect("nbt storage lock poisoned"))
    }

    /// The whole compound under `id`; an id never written reads as an empty
    /// one, as the reference creates storage on first read.
    #[must_use]
    pub fn root(&self, id: &str) -> SnbtValue {
        self.with(|store| SnbtValue::Compound(store.get(id).cloned().unwrap_or_default()))
    }

    /// Every value `path` selects under `id`.
    ///
    /// # Errors
    ///
    /// "Found no elements matching" the dead-ended prefix.
    pub fn get(&self, id: &str, path: &NbtPath) -> Result<Vec<SnbtValue>, String> {
        path.get(&self.root(id))
    }

    /// Replaces `id`'s whole compound.
    pub fn replace(&self, id: &str, data: Vec<(String, SnbtValue)>) {
        self.with(|store| {
            store.insert(id.to_string(), data);
        });
    }

    /// Merges `incoming` into `id`'s compound (created on first write).
    pub fn merge(&self, id: &str, incoming: Vec<(String, SnbtValue)>) {
        self.with(|store| {
            let root = store.entry(id.to_string()).or_default();
            lodestone_command_mc::merge_compound(root, &incoming);
        });
    }

    /// Writes `value` at `path` under `id`, creating the parents the path
    /// walks through. A path that cannot be satisfied leaves the storage as it was.
    pub fn set(&self, id: &str, path: &NbtPath, value: SnbtValue) {
        self.with(|store| {
            let root = store.entry(id.to_string()).or_default();
            let mut tree = SnbtValue::Compound(std::mem::take(root));
            let _ = path.set(&mut tree, &value);
            let SnbtValue::Compound(entries) = tree else { unreachable!("a path never replaces the root") };
            *root = entries;
        });
    }

    /// Removes what `path` selects under `id`; how many values went.
    pub fn remove(&self, id: &str, path: &NbtPath) -> usize {
        self.with(|store| {
            let Some(root) = store.get_mut(id) else { return 0 };
            let mut tree = SnbtValue::Compound(std::mem::take(root));
            let removed = path.remove(&mut tree);
            let SnbtValue::Compound(entries) = tree else { unreachable!("a path never replaces the root") };
            *root = entries;
            removed
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(s: &str) -> NbtPath {
        NbtPath::parse(s).expect("test path parses")
    }

    fn one(storage: &NbtStorageHandle, id: &str, p: &str) -> Option<SnbtValue> {
        storage.get(id, &path(p)).ok().and_then(|mut found| found.pop())
    }

    #[test]
    fn an_unwritten_id_reads_as_an_empty_compound_not_an_error() {
        let storage = NbtStorageHandle::default();
        assert_eq!(storage.root("minecraft:test"), SnbtValue::Compound(vec![]));
        assert_eq!(one(&storage, "minecraft:test", "missing"), None);
    }

    #[test]
    fn merge_writes_a_fresh_id_and_a_path_reads_the_written_value() {
        let storage = NbtStorageHandle::default();
        storage.merge("minecraft:test", vec![("a".to_string(), SnbtValue::Int(5))]);
        assert_eq!(one(&storage, "minecraft:test", "a"), Some(SnbtValue::Int(5)));
        assert_eq!(
            storage.root("minecraft:test"),
            SnbtValue::Compound(vec![("a".to_string(), SnbtValue::Int(5))])
        );
    }

    /// The discriminating case for `merge`: a nested compound merges
    /// key-by-key (b.x survives a second merge that only sets b.y), while a
    /// scalar at the same key is replaced outright rather than merged.
    #[test]
    fn merge_recurses_into_nested_compounds_but_replaces_a_scalar() {
        let storage = NbtStorageHandle::default();
        storage.merge(
            "minecraft:test",
            vec![
                ("scalar".to_string(), SnbtValue::Int(1)),
                ("b".to_string(), SnbtValue::Compound(vec![("x".to_string(), SnbtValue::Int(1))])),
            ],
        );
        storage.merge(
            "minecraft:test",
            vec![
                ("scalar".to_string(), SnbtValue::Int(2)),
                ("b".to_string(), SnbtValue::Compound(vec![("y".to_string(), SnbtValue::Int(2))])),
            ],
        );

        assert_eq!(one(&storage, "minecraft:test", "scalar"), Some(SnbtValue::Int(2)));
        assert_eq!(one(&storage, "minecraft:test", "b.x"), Some(SnbtValue::Int(1)), "x must survive the second merge");
        assert_eq!(one(&storage, "minecraft:test", "b.y"), Some(SnbtValue::Int(2)));
    }

    #[test]
    fn remove_reports_how_many_values_it_removed() {
        let storage = NbtStorageHandle::default();
        storage.merge("minecraft:test", vec![("a".to_string(), SnbtValue::Int(1))]);

        assert_eq!(storage.remove("minecraft:test", &path("a")), 1);
        assert_eq!(one(&storage, "minecraft:test", "a"), None);
        assert_eq!(storage.remove("minecraft:test", &path("a")), 0);
        assert_eq!(storage.remove("minecraft:test", &path("never.was.here")), 0);
        assert_eq!(storage.remove("minecraft:ghost", &path("a")), 0);
    }

    #[test]
    fn set_creates_intermediate_compounds_and_writes_the_leaf() {
        let storage = NbtStorageHandle::default();
        storage.set("minecraft:test", &path("a.b.c"), SnbtValue::Int(7));
        assert_eq!(one(&storage, "minecraft:test", "a.b.c"), Some(SnbtValue::Int(7)));
        assert_eq!(
            one(&storage, "minecraft:test", "a.b"),
            Some(SnbtValue::Compound(vec![("c".to_string(), SnbtValue::Int(7))]))
        );
    }

    #[test]
    fn set_reaches_into_list_elements() {
        let storage = NbtStorageHandle::default();
        storage.merge(
            "minecraft:test",
            vec![("l".to_string(), SnbtValue::List(vec![SnbtValue::Int(1), SnbtValue::Int(2)]))],
        );
        storage.set("minecraft:test", &path("l[1]"), SnbtValue::Int(9));
        assert_eq!(one(&storage, "minecraft:test", "l[1]"), Some(SnbtValue::Int(9)));
        assert_eq!(one(&storage, "minecraft:test", "l[0]"), Some(SnbtValue::Int(1)));
    }

    #[test]
    fn set_overwrites_an_existing_leaf_in_place() {
        let storage = NbtStorageHandle::default();
        storage.set("minecraft:test", &path("a"), SnbtValue::Int(1));
        storage.set("minecraft:test", &path("a"), SnbtValue::Int(2));
        assert_eq!(one(&storage, "minecraft:test", "a"), Some(SnbtValue::Int(2)));
    }

    #[test]
    fn remove_reaches_into_a_nested_compound() {
        let storage = NbtStorageHandle::default();
        storage.merge(
            "minecraft:test",
            vec![(
                "outer".to_string(),
                SnbtValue::Compound(vec![
                    ("keep".to_string(), SnbtValue::Int(1)),
                    ("drop".to_string(), SnbtValue::Int(2)),
                ]),
            )],
        );
        assert_eq!(storage.remove("minecraft:test", &path("outer.drop")), 1);
        assert_eq!(one(&storage, "minecraft:test", "outer.drop"), None);
        assert_eq!(one(&storage, "minecraft:test", "outer.keep"), Some(SnbtValue::Int(1)));
    }
}
