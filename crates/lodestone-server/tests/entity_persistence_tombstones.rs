//! Diagnostic controls for entity-region ownership and persistence.
//!
//! These tests deliberately exercise the public `EntityStorage` seam rather
//! than reaching into the private integrated-server implementation.  The
//! tombstone-capable API receives both the live population and the caller's
//! complete ownership set, so a missing owned UUID can be removed while an
//! opaque entity remains untouched.

use std::collections::HashSet;

use lodestone_core::Nbt;
use lodestone_model::{Rotation, Vec3};
use lodestone_server::entity_storage::{EntityStorage, SavedEntity};
use tempfile::tempdir;
use uuid::Uuid;

fn mob(uuid: Uuid) -> SavedEntity {
    SavedEntity {
        id: "minecraft:cow".parse().expect("valid entity key"),
        uuid,
        pos: Vec3::new(4.5, 64.0, 4.5),
        motion: Vec3::default(),
        rotation: Rotation::new(0.0, 0.0),
        health: Some(10.0),
        item: None,
        age: None,
        pickup_delay: None,
        extra: Vec::new(),
    }
}

/// A modeled entity that disappears must not return after a save/reopen.
///
#[test]
fn despawned_modeled_entity_is_removed_from_its_old_chunk() {
    let dir = tempdir().expect("temporary entity world");
    let storage = EntityStorage::new(dir.path()).expect("entity storage");
    let entity = mob(Uuid::new_v4());

    let owned = HashSet::from([entity.uuid]);
    storage
        .save_owned(std::slice::from_ref(&entity), &owned)
        .expect("initial entity save");
    storage
        .save_owned(&[], &owned)
        .expect("save after despawn");

    let loaded = storage.load_chunk(0, 0).expect("reload old chunk");
    assert!(
        loaded.is_empty(),
        "despawned UUID {} returned from its old chunk",
        entity.uuid
    );
}

/// A restart must preserve the ownership identity long enough for the first
/// post-reopen save to tombstone an entity that despawned before that save.
#[test]
fn restored_entity_can_be_tombstoned_after_restart() {
    let dir = tempdir().expect("temporary entity world");
    let entity = mob(Uuid::new_v4());

    {
        let storage = EntityStorage::new(dir.path()).expect("initial entity storage");
        storage
            .save(std::slice::from_ref(&entity))
            .expect("initial entity save");
    }

    let storage = EntityStorage::new(dir.path()).expect("reopened entity storage");
    let restored = storage
        .load_area(0..=0, 0..=0)
        .expect("restore entity area");
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].uuid, entity.uuid);

    let owned = HashSet::from([restored[0].uuid]);
    storage
        .save_owned(&[], &owned)
        .expect("tombstone after restart");
    assert!(
        storage
            .load_chunk(0, 0)
            .expect("reload tombstoned chunk")
            .is_empty()
    );
}

/// Reassigning the same UUID to a new owner remains exactly-once across a
/// chunk boundary: the receiving owner writes the entity at its destination,
/// and the previous owner's record is removed in the same region rewrite.
#[test]
fn ownership_reassignment_moves_one_entity_without_a_duplicate() {
    let dir = tempdir().expect("temporary entity world");
    let storage = EntityStorage::new(dir.path()).expect("entity storage");
    let entity = mob(Uuid::new_v4());
    let owned_by_source = HashSet::from([entity.uuid]);
    storage
        .save_owned(std::slice::from_ref(&entity), &owned_by_source)
        .expect("source owner save");

    let moved = SavedEntity {
        pos: Vec3::new(36.5, 64.0, 4.5),
        ..entity.clone()
    };
    let owned_by_destination = HashSet::from([moved.uuid]);
    storage
        .save_owned(std::slice::from_ref(&moved), &owned_by_destination)
        .expect("destination owner save");

    assert!(
        storage
            .load_chunk(0, 0)
            .expect("reload source chunk")
            .is_empty()
    );
    let destination = storage.load_chunk(2, 0).expect("reload destination chunk");
    assert_eq!(destination.len(), 1);
    assert_eq!(destination[0].uuid, entity.uuid);
}

/// The companion control: an entity outside this session's ownership must be
/// retained while a different live population is saved elsewhere.  The
/// tombstone-capable save must preserve this distinction.
#[test]
fn unowned_entity_is_preserved_when_another_population_is_saved() {
    let dir = tempdir().expect("temporary entity world");
    let storage = EntityStorage::new(dir.path()).expect("entity storage");
    let stranger = SavedEntity {
        extra: vec![("PersistenceRequired".to_owned(), Nbt::Byte(1))],
        ..mob(Uuid::new_v4())
    };
    let ours = SavedEntity {
        pos: Vec3::new(36.5, 64.0, 4.5),
        ..mob(Uuid::new_v4())
    };

    storage
        .save(std::slice::from_ref(&stranger))
        .expect("seed unowned entity");
    let owned = HashSet::from([ours.uuid]);
    storage
        .save_owned(std::slice::from_ref(&ours), &owned)
        .expect("save another population");

    let kept = storage.load_chunk(0, 0).expect("reload unowned chunk");
    assert_eq!(kept.len(), 1, "the unowned entity must remain stored");
    assert_eq!(kept[0].uuid, stranger.uuid);
    assert_eq!(kept[0].extra, stranger.extra);
}
