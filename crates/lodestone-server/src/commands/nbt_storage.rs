//! The NBT command-storage engine — the real command-storage record, behind
//! [`NbtStorageHandle`]. `/data storage` and `/execute if`/`unless data
//! storage` are its two consumers.
//!
//! # What it is, and what it deliberately is not
//!
//! The real `/data` has three targets — `storage`, `entity`, `block` — and
//! this crate builds only the first. `storage` is a free-standing per-id NBT
//! compound with no owner in the world at all (the real command storage's own
//! backing is a resource-location-keyed compound map, not attached to
//! anything), so it needs nothing this crate lacks. `entity`/`block` would
//! need a command-reachable, mutable NBT view of a *live* entity or block
//! entity — a real subsystem this crate has nowhere (see
//! `crate::commands::execute`'s module doc, which already names this same
//! gap for `if data`/`if items`). Building `storage` alone is exactly the
//! shape `lodestone_command_mc::snbt`'s own module doc already called out as
//! the missing half: "`if data` additionally needs an NBT-storage engine
//! this server does not have" — that half now exists.
//!
//! # How it works
//!
//! [`NbtStorageHandle`] is `Arc<Mutex<HashMap<String, Vec<(String,
//! SnbtValue)>>>>`, shaped like every other store in this module
//! ([`crate::commands::scoreboard_store::ScoreboardHandle`],
//! [`crate::commands::team_store::TeamHandle`]): cheap to clone, rides
//! inside [`crate::world_state::WorldStateHandle`] as a sibling field for
//! the identical reachability reason those two document. A storage id's
//! compound is `Vec<(String, SnbtValue)>` rather than wrapping it in a
//! `SnbtValue::Compound` — that is exactly what
//! [`lodestone_command_mc::NbtCompoundArg`] already parses into, so
//! `/data merge storage <id> <nbt>`'s parsed argument and this store's
//! representation are the same type with no conversion at the seam.
//!
//! `NbtPathArg`'s v1 grammar (dotted compound keys only, no array index, no
//! filter compound — see that type's own doc) is what [`get`](NbtStorageHandle::get)/
//! [`remove`](NbtStorageHandle::remove) walk; [`merge`](NbtStorageHandle::merge)
//! is the real compound-merge rule: recurse when both sides hold a
//! compound at the same key, overwrite otherwise. [`set`](NbtStorageHandle::set)
//! is the other real primitive, the NBT-path set operation —
//! `/execute store data storage`'s own write, which creates every
//! intermediate compound `path` needs rather than requiring the caller to
//! shape one.
//!
//! # How to change it
//!
//! Read/write access is via `WorldStateHandle::nbt_storage`
//! (`crate::world_state`), never a second constructor — the identical rule
//! every sibling store in this module states, for the identical reason.
//!
//! # Configuration
//!
//! None.
//!
//! # Dependencies
//!
//! `lodestone_command_mc::snbt::SnbtValue`. Nothing else — this store never
//! reaches a chunk, an entity, or the wire.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_command_mc::SnbtValue;

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

    /// The real command-storage read side — the whole
    /// compound for an id that has never been written comes back as an
    /// empty one, matching the real "created on first read or write"
    /// posture, rather than an error.
    #[must_use]
    pub fn get(&self, id: &str, path: &[String]) -> Option<SnbtValue> {
        self.with(|store| {
            let root = store.get(id).cloned().unwrap_or_default();
            get_path(&root, path)
        })
    }

    /// The real command-storage set rule — merged into whatever this id
    /// already held (created empty if this is the first write).
    pub fn merge(&self, id: &str, incoming: Vec<(String, SnbtValue)>) {
        self.with(|store| {
            let root = store.entry(id.to_string()).or_default();
            merge_compound(root, incoming);
        });
    }

    /// The real NBT-path set rule — overwrites the single value at `path`
    /// (creating it, and every intermediate compound `path` walks through
    /// that does not exist yet, exactly like [`merge`](Self::merge) creates
    /// the id itself). Unlike `merge`, a non-compound value sitting where an
    /// intermediate segment needs a compound is replaced outright rather than
    /// merged into — `/execute store data storage`'s only caller, and the
    /// real rule itself does the same (a get-or-create-then-overwrite
    /// walk). A path of `[]` (structurally unreachable — [`lodestone_command_mc::NbtPathArg`]
    /// requires at least one segment) is a no-op rather than a panic.
    pub fn set(&self, id: &str, path: &[String], value: SnbtValue) {
        self.with(|store| {
            let root = store.entry(id.to_string()).or_default();
            set_path(root, path, value);
        });
    }

    /// Removes the value at `path`. Returns whether anything was actually
    /// removed — a `remove` on an id or path that was never set is a no-op,
    /// not an error (matching the real NBT-path remove rule's own count,
    /// which the real data command's `remove` handler reports back as "how
    /// many").
    pub fn remove(&self, id: &str, path: &[String]) -> bool {
        self.with(|store| match store.get_mut(id) {
            Some(root) => remove_path(root, path),
            None => false,
        })
    }
}

/// Walk `path` through nested compounds. An empty path is the whole root, an
/// out-of-range segment (missing key, or a non-compound value with more path
/// left) is `None`.
fn get_path(root: &[(String, SnbtValue)], path: &[String]) -> Option<SnbtValue> {
    let Some((first, rest)) = path.split_first() else {
        return Some(SnbtValue::Compound(root.to_vec()));
    };
    let value = root.iter().find(|(k, _)| k == first).map(|(_, v)| v)?;
    if rest.is_empty() {
        return Some(value.clone());
    }
    match value {
        SnbtValue::Compound(inner) => get_path(inner, rest),
        _ => None,
    }
}

/// Walks `path` through `root`, creating intermediate compounds as needed,
/// and overwrites the leaf. See [`NbtStorageHandle::set`]'s own doc for the
/// one divergence from [`merge_compound`]: a non-compound intermediate is
/// replaced, not merged into.
fn set_path(root: &mut Vec<(String, SnbtValue)>, path: &[String], value: SnbtValue) {
    match path {
        [] => {}
        [only] => match root.iter_mut().find(|(k, _)| k == only) {
            Some(entry) => entry.1 = value,
            None => root.push((only.clone(), value)),
        },
        [first, rest @ ..] => {
            let index = match root.iter().position(|(k, _)| k == first) {
                Some(index) => index,
                None => {
                    root.push((first.clone(), SnbtValue::Compound(Vec::new())));
                    root.len() - 1
                }
            };
            if !matches!(root[index].1, SnbtValue::Compound(_)) {
                root[index].1 = SnbtValue::Compound(Vec::new());
            }
            let SnbtValue::Compound(inner) = &mut root[index].1 else {
                unreachable!("just normalised to a Compound above")
            };
            set_path(inner, rest, value);
        }
    }
}

/// The real compound-merge rule: for each incoming key, recurse if **both** sides are
/// compounds at that key, otherwise the incoming value replaces whatever was
/// there (a list, a scalar, or nothing).
fn merge_compound(target: &mut Vec<(String, SnbtValue)>, incoming: Vec<(String, SnbtValue)>) {
    for (key, value) in incoming {
        match target.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => match (&mut entry.1, value) {
                (SnbtValue::Compound(existing_inner), SnbtValue::Compound(incoming_inner)) => {
                    merge_compound(existing_inner, incoming_inner);
                }
                (slot, other) => *slot = other,
            },
            None => target.push((key, value)),
        }
    }
}

/// Removes the value at `path` from `root`, returning whether anything was
/// actually there. `path` must be non-empty — [`NbtStorageHandle::remove`]'s
/// only caller, `/data remove storage`, requires `<path>` at the tree level.
fn remove_path(root: &mut Vec<(String, SnbtValue)>, path: &[String]) -> bool {
    match path {
        [] => false,
        [only] => {
            let before = root.len();
            root.retain(|(k, _)| k != only);
            root.len() != before
        }
        [first, rest @ ..] => match root.iter_mut().find(|(k, _)| k == first) {
            Some((_, SnbtValue::Compound(inner))) => remove_path(inner, rest),
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(s: &str) -> Vec<String> {
        s.split('.').map(str::to_string).collect()
    }

    #[test]
    fn an_unwritten_id_reads_as_an_empty_compound_not_an_error() {
        let storage = NbtStorageHandle::default();
        assert_eq!(storage.get("minecraft:test", &[]), Some(SnbtValue::Compound(vec![])));
        assert_eq!(storage.get("minecraft:test", &seg("missing")), None);
    }

    #[test]
    fn merge_writes_a_fresh_id_and_a_path_reads_the_written_value() {
        let storage = NbtStorageHandle::default();
        storage.merge("minecraft:test", vec![("a".to_string(), SnbtValue::Int(5))]);
        assert_eq!(storage.get("minecraft:test", &seg("a")), Some(SnbtValue::Int(5)));
        assert_eq!(
            storage.get("minecraft:test", &[]),
            Some(SnbtValue::Compound(vec![("a".to_string(), SnbtValue::Int(5))]))
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
                (
                    "b".to_string(),
                    SnbtValue::Compound(vec![("x".to_string(), SnbtValue::Int(1))]),
                ),
            ],
        );
        storage.merge(
            "minecraft:test",
            vec![
                ("scalar".to_string(), SnbtValue::Int(2)),
                (
                    "b".to_string(),
                    SnbtValue::Compound(vec![("y".to_string(), SnbtValue::Int(2))]),
                ),
            ],
        );

        assert_eq!(storage.get("minecraft:test", &seg("scalar")), Some(SnbtValue::Int(2)));
        assert_eq!(storage.get("minecraft:test", &seg("b.x")), Some(SnbtValue::Int(1)), "x must survive the second merge");
        assert_eq!(storage.get("minecraft:test", &seg("b.y")), Some(SnbtValue::Int(2)));
    }

    #[test]
    fn remove_reports_whether_it_actually_removed_something() {
        let storage = NbtStorageHandle::default();
        storage.merge("minecraft:test", vec![("a".to_string(), SnbtValue::Int(1))]);

        assert!(storage.remove("minecraft:test", &seg("a")));
        assert_eq!(storage.get("minecraft:test", &seg("a")), None);
        // The second removal of the same path is a no-op, reported as such.
        assert!(!storage.remove("minecraft:test", &seg("a")));
        // A path that was never there at all, same answer.
        assert!(!storage.remove("minecraft:test", &seg("never.was.here")));
        // An id that was never written at all.
        assert!(!storage.remove("minecraft:ghost", &seg("a")));
    }

    #[test]
    fn set_creates_intermediate_compounds_and_writes_the_leaf() {
        let storage = NbtStorageHandle::default();
        storage.set("minecraft:test", &seg("a.b.c"), SnbtValue::Int(7));
        assert_eq!(storage.get("minecraft:test", &seg("a.b.c")), Some(SnbtValue::Int(7)));
        // The intermediate compounds really were created, not just the leaf.
        assert_eq!(
            storage.get("minecraft:test", &seg("a.b")),
            Some(SnbtValue::Compound(vec![("c".to_string(), SnbtValue::Int(7))]))
        );
    }

    #[test]
    fn set_overwrites_a_scalar_that_is_in_the_way_of_a_deeper_path() {
        let storage = NbtStorageHandle::default();
        storage.merge("minecraft:test", vec![("a".to_string(), SnbtValue::Int(1))]);
        // `a` is currently a scalar; setting `a.b` must replace it with a
        // compound rather than merging into a non-existent one.
        storage.set("minecraft:test", &seg("a.b"), SnbtValue::Int(2));
        assert_eq!(storage.get("minecraft:test", &seg("a.b")), Some(SnbtValue::Int(2)));
    }

    #[test]
    fn set_overwrites_an_existing_leaf_in_place() {
        let storage = NbtStorageHandle::default();
        storage.set("minecraft:test", &seg("a"), SnbtValue::Int(1));
        storage.set("minecraft:test", &seg("a"), SnbtValue::Int(2));
        assert_eq!(storage.get("minecraft:test", &seg("a")), Some(SnbtValue::Int(2)));
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
        assert!(storage.remove("minecraft:test", &seg("outer.drop")));
        assert_eq!(storage.get("minecraft:test", &seg("outer.drop")), None);
        assert_eq!(storage.get("minecraft:test", &seg("outer.keep")), Some(SnbtValue::Int(1)));
    }
}
