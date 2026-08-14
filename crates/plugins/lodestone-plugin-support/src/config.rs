//! The plugin config convention's other half: a minimal typed config-loading helper, mirroring
//! `JavaPlugin.getConfig()` — load a `T` from the plugin's own data
//! directory, falling back to `T::default()` when the file is absent or
//! unparseable, and save one back. serde-based, matching every other
//! data-model shape in this codebase (this crate's own dependency list, and
//! `lodestone-auth`'s `metadata.rs`, both already lean on serde rather than a
//! bespoke format).
//!
//! JSON, not YAML/TOML: `serde_json` is already a workspace dependency
//! (`lodestone-auth`'s cache/metadata files use it), so this needs no new
//! format dependency, and a plugin author gets whatever `#[derive(Serialize,
//! Deserialize)]` shape they already use for any other on-disk state.

use std::io;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Loads `<data_dir>/plugins/<plugin_name>/<file_name>` as a `T`, or returns
/// `T::default()` if the file does not exist, cannot be read, or does not
/// parse as JSON.
///
/// Deliberately forgiving rather than `Result`-returning: `JavaPlugin.getConfig()`
/// has the same shape — a plugin with no config file yet gets its defaults, not
/// a load error to handle on every startup. A plugin that needs to distinguish
/// "missing" from "corrupt" should read the file itself.
#[must_use]
pub fn load_or_default<T: DeserializeOwned + Default>(plugin_name: &str, file_name: &str) -> T {
    load_or_default_from(&crate::paths::plugin_data_dir(plugin_name).join(file_name))
}

fn load_or_default_from<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

/// Saves `value` as `<data_dir>/plugins/<plugin_name>/<file_name>`, creating
/// the plugin's data directory first if it does not exist yet — the write
/// half of [`load_or_default`], and what makes a plugin author's "load,
/// mutate, save" loop two calls instead of a `std::fs` dance repeated once
/// per plugin.
pub fn save<T: Serialize>(plugin_name: &str, file_name: &str, value: &T) -> io::Result<()> {
    let dir = crate::paths::ensure_plugin_data_dir(plugin_name)?;
    save_to(&dir.join(file_name), value)
}

fn save_to<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
    struct ToyConfig {
        greeting: String,
        max_players: u32,
    }

    #[test]
    fn missing_file_loads_as_default_not_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg: ToyConfig = load_or_default_from(&tmp.path().join("config.json"));
        assert_eq!(cfg, ToyConfig::default());
    }

    #[test]
    fn corrupt_file_loads_as_default_rather_than_panicking() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.json");
        std::fs::write(&path, b"not json at all {{{").expect("write garbage");
        let cfg: ToyConfig = load_or_default_from(&path);
        assert_eq!(cfg, ToyConfig::default());
    }

    #[test]
    fn a_saved_config_round_trips_through_disk_byte_for_byte_in_shape() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.json");
        let written = ToyConfig {
            greeting: "hello".to_string(),
            max_players: 20,
        };

        save_to(&path, &written).expect("save");
        assert!(path.is_file(), "save must actually create the file");

        let read_back: ToyConfig = load_or_default_from(&path);
        assert_eq!(
            read_back, written,
            "what comes back must be exactly what was saved, not a default \
             masking a silent parse failure"
        );
    }

    /// Removes a throwaway directory on drop, panic or not — the real
    /// `data_dir()` is the user's actual application-support directory, and a
    /// test that writes there must never leave a stray `plugins/<name>/`
    /// behind, including when an assertion in the test body panics.
    struct CleanupOnDrop(std::path::PathBuf);
    impl Drop for CleanupOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn save_creates_the_plugin_data_directory_it_needs() {
        // Route the *real* plugin_data_dir through a throwaway, uniquely-named
        // plugin and assert the directory now exists — this exercises `save`'s
        // own `ensure_plugin_data_dir` call, not just the pure `save_to`
        // helper, which is the whole point: `save_to` alone would pass even if
        // `save` forgot to create the directory first.
        let name = format!("lodestone-plugin-support-test-{}", std::process::id());
        let dir = lodestone_auth::paths::data_dir().join("plugins").join(&name);
        let _cleanup = CleanupOnDrop(dir.clone());
        // Best-effort cleanup from a previous crashed run; not load-bearing.
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!dir.exists(), "control: directory absent before save");

        let cfg = ToyConfig {
            greeting: "hi".to_string(),
            max_players: 1,
        };
        save(&name, "config.json", &cfg).expect("save");
        assert!(dir.is_dir(), "save must create the plugin's data directory");
        let read_back: ToyConfig = load_or_default(&name, "config.json");
        assert_eq!(read_back, cfg);
    }
}
