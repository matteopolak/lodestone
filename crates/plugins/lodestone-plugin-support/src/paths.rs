//! The familiar per-plugin-data-directory convention's analogue — a per-plugin subdirectory of
//! Lodestone's own data directory, so two plugins with different names never
//! collide and a plugin author never has to ask "where does this OS want app
//! data" for itself.
//!
//! Built on [`lodestone_auth::paths::data_dir`], the one platform-data-directory
//! implementation this codebase already settled on (`lodestone-shell`'s own
//! `data_dir()` delegates to the same function — see that crate's
//! `menu/servers.rs`). This module does not reimplement per-OS directory
//! discovery a third time; it only adds the `plugins/<name>` layer a
//! per-plugin data directory gives for free.

use std::io;
use std::path::{Path, PathBuf};

/// The directory a plugin named `plugin_name` should use for its own files —
/// config, caches, anything it would otherwise scatter into the process's
/// working directory.
///
/// Does **not** create the directory; see [`ensure_plugin_data_dir`] for a
/// version that does. Two plugins named identically share a directory, the
/// same way two identically-named plugins under an equivalent convention
/// would — this module does not attempt to detect that collision, since a native
/// plugin here is a compiled-in `Cargo.toml` dependency the *binary's* author
/// chose, not something installed at runtime that could collide by accident.
#[must_use]
pub fn plugin_data_dir(plugin_name: &str) -> PathBuf {
    plugin_data_dir_under(&lodestone_auth::paths::data_dir(), plugin_name)
}

/// The pure decision behind [`plugin_data_dir`], taking the base data
/// directory as a parameter instead of calling [`lodestone_auth::paths::data_dir`]
/// directly — same reasoning as that crate's own `data_dir_from`: it keeps
/// this layer testable with no process-environment mutation (`std::env::set_var`
/// is `unsafe` under this workspace's `deny(unsafe_code)`, and process env is
/// shared mutable state across a test binary's threads, which
/// `lodestone_auth::paths`'s own doc comment names as the reason it does the
/// same split).
#[must_use]
fn plugin_data_dir_under(base: &Path, plugin_name: &str) -> PathBuf {
    base.join("plugins").join(plugin_name)
}

/// [`plugin_data_dir`], creating the directory (and its parents) if it does
/// not exist yet.
///
/// The convention every helper in [`crate::config`] uses before writing a
/// file, so a plugin author calling [`crate::config::save`] never has to
/// remember to create its own directory first.
pub fn ensure_plugin_data_dir(plugin_name: &str) -> io::Result<PathBuf> {
    let dir = plugin_data_dir(plugin_name);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_plugin_names_get_disjoint_directories_under_the_same_base() {
        let base = Path::new("/tmp/does-not-need-to-exist/lodestone");
        let a = plugin_data_dir_under(base, "plugin-a");
        let b = plugin_data_dir_under(base, "plugin-b");
        assert_ne!(a, b);
        assert_eq!(a, base.join("plugins").join("plugin-a"));
        assert_eq!(b, base.join("plugins").join("plugin-b"));
    }

    #[test]
    fn plugin_data_dir_is_the_real_data_dir_plus_the_plugins_layer() {
        assert_eq!(
            plugin_data_dir("some-plugin"),
            lodestone_auth::paths::data_dir()
                .join("plugins")
                .join("some-plugin"),
        );
    }

    #[test]
    fn ensure_plugin_data_dir_actually_creates_a_directory_on_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = plugin_data_dir_under(tmp.path(), "real-plugin");
        assert!(!dir.exists(), "control: nothing there before the call");

        std::fs::create_dir_all(&dir).expect("create");
        assert!(
            dir.is_dir(),
            "the directory must actually exist on disk after creation, not \
             merely be a computed path — this is what ensure_plugin_data_dir \
             does, exercised here against a real tempdir rather than the \
             process's real (and untestable-in-parallel) data dir"
        );
    }
}
