//! File-backed durable-data lifecycle coverage.
//!
//! This uses the public snapshot-file adapter exactly as a world owner would:
//! load on open, mutate in memory, atomically save, then load again. The
//! backend remains deliberately outside this crate; the test only proves the
//! adapter does not lose opaque records or confuse deletion with unloading.

use lodestone_plugin_support::{
    plugin_data_snapshot_path, DataScope, PluginDataKey, PluginDataStore,
    PLUGIN_DATA_SNAPSHOT_FILE,
};

fn key(name: &str) -> PluginDataKey {
    PluginDataKey::new("claims", DataScope::World { id: [8; 16] }, name)
        .expect("valid plugin key")
}

#[test]
fn snapshot_file_replaces_atomically_and_reopen_observes_deletion() {
    let temp = tempfile::tempdir().expect("temporary backend");
    let path = temp.path().join("claims.json");

    assert!(
        PluginDataStore::load_snapshot_file(&path)
            .expect("missing backend file is an empty store")
            .is_empty()
    );

    let owner = key("owner");
    let opaque = key("opaque");
    let mut store = PluginDataStore::default();
    store
        .set(owner.clone(), 1, &"alice")
        .expect("store typed value");
    store
        .set_blob(opaque.clone(), 7, [0, 255, 1, 2])
        .expect("store opaque bytes");
    store
        .save_snapshot_file(&path)
        .expect("atomically commit first snapshot");

    let mut reopened =
        PluginDataStore::load_snapshot_file(&path).expect("reopen first snapshot");
    assert_eq!(
        reopened.get::<String>(&owner).expect("decode owner"),
        Some("alice".into())
    );
    assert_eq!(
        reopened.record(&opaque).expect("opaque record").schema_version(),
        7
    );
    assert_eq!(reopened.get_blob(&opaque), Some([0, 255, 1, 2].as_slice()));

    reopened.remove(&owner).expect("deletion is explicit");
    reopened
        .save_snapshot_file(&path)
        .expect("atomically commit deletion snapshot");
    let after_delete =
        PluginDataStore::load_snapshot_file(&path).expect("reopen deletion snapshot");
    assert_eq!(after_delete.get::<String>(&owner).expect("deleted lookup"), None);
    assert_eq!(after_delete.get_blob(&opaque), Some([0, 255, 1, 2].as_slice()));
}

#[test]
fn corrupt_existing_snapshot_is_an_error_not_an_empty_store() {
    let temp = tempfile::tempdir().expect("temporary backend");
    let path = temp.path().join("claims.json");
    std::fs::write(&path, b"not a snapshot").expect("write corrupt fixture");

    assert!(
        PluginDataStore::load_snapshot_file(&path).is_err(),
        "only a missing file means an empty store; corruption must be visible"
    );
}

#[test]
fn the_conventional_sidecar_path_is_shared_by_both_world_backends() {
    let world = tempfile::tempdir().expect("temporary world");
    let expected = world.path().join(PLUGIN_DATA_SNAPSHOT_FILE);
    assert_eq!(plugin_data_snapshot_path(world.path()), expected);

    // The sidecar is format-independent: backend directories can be named
    // `region` (Anvil) or `native` (Lodestone) without changing its payload.
    for backend_directory in [world.path().join("region"), world.path().join("native")] {
        let path = plugin_data_snapshot_path(&backend_directory);
        let key = PluginDataKey::new(
            "claims",
            DataScope::World { id: [9; 16] },
            "owner",
        )
        .expect("valid key");
        let mut store = PluginDataStore::default();
        store.set(key.clone(), 1, &"alice").expect("write record");
        store.save_snapshot_file(&path).expect("save sidecar");
        let reopened = PluginDataStore::load_snapshot_file(&path).expect("reopen sidecar");
        assert_eq!(reopened.get::<String>(&key).expect("decode"), Some("alice".into()));
    }
}
