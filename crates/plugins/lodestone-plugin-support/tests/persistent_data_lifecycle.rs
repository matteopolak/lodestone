//! Native plugin-data lifecycle coverage through the public ECS resource.
//!
//! The test deliberately treats the temporary file as a tiny stand-in for a
//! world backend. The plugin support crate owns the validated record and
//! snapshot contract; Anvil/Lodestone adapters own where those bytes are
//! committed.

use lodestone_ecs::app::App;
use lodestone_plugin_support::{DataScope, PersistentDataPlugin, PluginDataKey, PluginDataStore};

fn world_key() -> PluginDataKey {
    PluginDataKey::new("claims", DataScope::World { id: [4; 16] }, "owner")
        .expect("valid plugin key")
}

#[test]
fn native_plugin_data_survives_reload_then_deletion_is_persisted() {
    let temp = tempfile::tempdir().expect("temporary backend");
    let snapshot_path = temp.path().join("plugin-data.json");

    let mut first_app = App::new();
    first_app.add_plugins(PersistentDataPlugin);
    let key = world_key();
    first_app
        .world_mut()
        .resource_mut::<PluginDataStore>()
        .set(key.clone(), 1, &"alice")
        .expect("write native plugin data");
    let bytes = first_app
        .world()
        .resource::<PluginDataStore>()
        .to_snapshot_bytes()
        .expect("encode native snapshot");
    std::fs::write(&snapshot_path, bytes).expect("commit snapshot");

    // A new App models a process/world reopen. Restore goes through the same
    // public resource used by a plugin, rather than reaching into its map.
    let restored = PluginDataStore::from_snapshot_bytes(
        &std::fs::read(&snapshot_path).expect("read committed snapshot"),
    )
    .expect("restore native snapshot");
    let mut reopened_app = App::new();
    reopened_app.insert_resource(restored);
    assert_eq!(
        reopened_app
            .world()
            .resource::<PluginDataStore>()
            .get::<String>(&key)
            .expect("decode restored value"),
        Some("alice".to_owned())
    );

    reopened_app
        .world_mut()
        .resource_mut::<PluginDataStore>()
        .remove(&key)
        .expect("deletion must return the old record");
    let bytes = reopened_app
        .world()
        .resource::<PluginDataStore>()
        .to_snapshot_bytes()
        .expect("encode deletion snapshot");
    std::fs::write(&snapshot_path, bytes).expect("commit deletion");

    let after_delete = PluginDataStore::from_snapshot_bytes(
        &std::fs::read(&snapshot_path).expect("read deletion snapshot"),
    )
    .expect("restore deletion snapshot");
    assert_eq!(
        after_delete
            .get::<String>(&key)
            .expect("lookup deleted value"),
        None
    );
}
