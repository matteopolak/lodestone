//! The plugin config convention's real consumer: a toy plugin that loads its config in
//! `Plugin::build` (matching the familiar plugin-config convention, loaded
//! at plugin start-up), exposes what it loaded as a resource, and can save an update
//! back out — driven through a real `bevy_ecs` `App`, not called as a bare
//! function.
//!
//! Runs against a real, uniquely-named directory under the process's actual
//! `lodestone_auth::paths::data_dir()` (there is no override-free way to
//! redirect it without `std::env::set_var`, which is `unsafe` under this
//! workspace's `deny(unsafe_code)` — see `src/paths.rs`'s own doc comment).
//! Cleaned up unconditionally via a `Drop` guard, including on panic.

use lodestone_ecs::app::{App, Plugin};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct GreeterConfig {
    message: String,
    shout: bool,
}

#[derive(Debug, Clone, bevy_ecs::resource::Resource)]
struct LoadedGreeterConfig(GreeterConfig);

const PLUGIN_NAME_PREFIX: &str = "lodestone-plugin-support-config-it";
const FILE_NAME: &str = "config.json";

struct GreeterPlugin {
    plugin_name: String,
}

impl Plugin for GreeterPlugin {
    fn build(&self, app: &mut App) {
        let cfg: GreeterConfig =
            lodestone_plugin_support::config::load_or_default(&self.plugin_name, FILE_NAME);
        app.insert_resource(LoadedGreeterConfig(cfg));
    }
}

/// Removes the throwaway plugin directory on drop, panic or not.
struct CleanupOnDrop(std::path::PathBuf);
impl Drop for CleanupOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_plugin_with_no_prior_config_boots_with_defaults() {
    let name = format!("{PLUGIN_NAME_PREFIX}-defaults-{}", std::process::id());
    let _cleanup = CleanupOnDrop(lodestone_plugin_support::paths::plugin_data_dir(&name));

    let mut app = App::new();
    app.add_plugins(GreeterPlugin {
        plugin_name: name,
    });

    assert_eq!(
        app.world().resource::<LoadedGreeterConfig>().0,
        GreeterConfig::default(),
        "a plugin whose config file has never been written must boot with \
         defaults, not fail to build"
    );
}

#[test]
fn a_plugin_boots_with_whatever_was_saved_on_a_previous_run() {
    let name = format!("{PLUGIN_NAME_PREFIX}-persisted-{}", std::process::id());
    let _cleanup = CleanupOnDrop(lodestone_plugin_support::paths::plugin_data_dir(&name));

    // Simulate a previous run's shutdown: save a real config to disk before
    // any `App` exists.
    let saved = GreeterConfig {
        message: "ahoy".to_string(),
        shout: true,
    };
    lodestone_plugin_support::config::save(&name, FILE_NAME, &saved).expect("save");

    // A fresh App/plugin — as if the process restarted — must load exactly
    // what the previous run saved, through Plugin::build, not through a
    // direct function call the test makes on its own behalf.
    let mut app = App::new();
    app.add_plugins(GreeterPlugin {
        plugin_name: name,
    });

    assert_eq!(
        app.world().resource::<LoadedGreeterConfig>().0,
        saved,
        "Plugin::build must load exactly what a previous run persisted"
    );
}
