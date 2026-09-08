//! Production-path controls for native version-locked plugin registration.
//!
//! These tests use `lodestone_app::client_app`, the same composed `App` handed
//! to native consumers, and only then attempt to add a plugin. The mismatch
//! control proves that validation happens before Bevy's plugin build callback,
//! not after a plugin has already changed the application.

use bevy_app::{App, Plugin};
use lodestone_registry::plugin::{
    VersionDescriptor, VersionLockedPlugin, register_version_locked_plugin,
};

const V26_2: VersionDescriptor =
    VersionDescriptor::new("v26-2", 776, "lodestone:native-version@0.1");

#[derive(bevy_ecs::resource::Resource)]
struct Registered;

struct MarkerPlugin;

impl VersionLockedPlugin for MarkerPlugin {
    fn version_descriptor(&self) -> VersionDescriptor {
        V26_2
    }
}

impl Plugin for MarkerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Registered);
    }
}

#[test]
fn matching_version_lock_registers_on_the_real_native_app_path() {
    let mut app = lodestone_app::client_app();

    register_version_locked_plugin(MarkerPlugin, V26_2, |plugin| app.add_plugins(plugin))
        .expect("matching family, protocol, and ABI must permit registration");

    assert!(app.is_plugin_added::<MarkerPlugin>());
    assert!(app.world().get_resource::<Registered>().is_some());
}

#[test]
fn mismatched_version_lock_refuses_before_the_plugin_build_callback() {
    let mut app = lodestone_app::client_app();
    let selected = VersionDescriptor::new("v1-21-11", 774, V26_2.abi());

    let error =
        register_version_locked_plugin(MarkerPlugin, selected, |plugin| app.add_plugins(plugin))
            .expect_err("a different family and protocol must not register");

    assert!(!app.is_plugin_added::<MarkerPlugin>());
    assert!(app.world().get_resource::<Registered>().is_none());
    let message = error.to_string();
    assert!(
        message.contains("requires family=v26-2 protocol=776 ABI=lodestone:native-version@0.1")
    );
    assert!(
        message.contains(
            "host provides family=v1-21-11 protocol=774 ABI=lodestone:native-version@0.1"
        )
    );
}
