//! The single source of truth for which Minecraft release the repo's
//! gitignored reference cache (`.cache/mc/<version>`) is read from.
//!
//! The current version is the trimmed contents of the repo-root `mc-version`
//! file, overridable per process with [`VERSION_ENV`]. Every reader of the
//! cache goes through this crate: [`cache_root`] and [`client_jar`] for
//! version-agnostic consumers (assets, textures, sounds, decompiled
//! reference), and [`version_root`] with a named pin such as [`PINNED_26_2`]
//! for a test whose expected values only hold for one release.
//!
//! [`ASSETS_ENV`] points at a pack root directly and wins over the version
//! lookup, for an out-of-tree cache.

use std::path::{Path, PathBuf};

/// Overrides the repo's `mc-version` file for this process.
pub const VERSION_ENV: &str = "LODESTONE_MC_VERSION";

/// Names a directory to use as the cache root, bypassing the version lookup.
pub const ASSETS_ENV: &str = "LODESTONE_ASSETS";

/// The release whose generated tables the hosted 26.2 family and its canonical
/// data were taken from. A test naming this is deliberately independent of
/// [`current_version`].
pub const PINNED_26_2: &str = "26.2";

/// The `mc-version` file as of this build, used when no file is found on disk
/// (an installed binary run outside the repo).
const EMBEDDED_VERSION: &str = include_str!("../../../mc-version");

/// The repository root this crate was built from.
#[must_use]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/lodestone-mc-cache sits two levels under the repo root")
        .to_path_buf()
}

/// The current reference version: [`VERSION_ENV`], else the `mc-version` file
/// in the nearest ancestor of the working directory, else the file in the repo
/// this crate was built from, else the value embedded at build time.
#[must_use]
pub fn current_version() -> String {
    if let Some(v) = std::env::var_os(VERSION_ENV) {
        let v = v.to_string_lossy().trim().to_owned();
        if !v.is_empty() {
            return v;
        }
    }
    let from_cwd = std::env::current_dir().ok().and_then(|cwd| {
        cwd.ancestors()
            .find_map(|dir| std::fs::read_to_string(dir.join("mc-version")).ok())
    });
    let text = from_cwd
        .or_else(|| std::fs::read_to_string(workspace_root().join("mc-version")).ok())
        .unwrap_or_else(|| EMBEDDED_VERSION.to_owned());
    text.trim().to_owned()
}

/// `<repo>/.cache/mc/<version>`: the directory a specific release is cached in.
/// Prefer [`cache_root`] unless the caller is pinned to one release.
#[must_use]
pub fn version_root(version: &str) -> PathBuf {
    workspace_root().join(".cache/mc").join(version)
}

/// `.cache/mc/26.2` in this repo: the root a [`PINNED_26_2`] test reads.
#[must_use]
pub fn pinned_26_2_root() -> PathBuf {
    version_root(PINNED_26_2)
}

/// The cache directory for the current version, or `None` when it is not on
/// disk. [`ASSETS_ENV`] wins; then `.cache/mc/<current>` is looked up in each
/// ancestor of the working directory (the binary runs from the repo root, tests
/// from a crate directory), then in the repo this crate was built from.
#[must_use]
pub fn cache_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(ASSETS_ENV) {
        let dir = PathBuf::from(dir);
        return dir.is_dir().then_some(dir);
    }
    let rel = Path::new(".cache/mc").join(current_version());
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(found) = cwd.ancestors().map(|a| a.join(&rel)).find(|p| p.is_dir()) {
            return Some(found);
        }
    }
    let built = workspace_root().join(rel);
    built.is_dir().then_some(built)
}

/// `client.jar` inside [`cache_root`], when present.
#[must_use]
pub fn client_jar() -> Option<PathBuf> {
    cache_root()
        .map(|root| root.join("client.jar"))
        .filter(|jar| jar.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_file_is_read_and_trimmed() {
        let v = EMBEDDED_VERSION.trim();
        assert!(!v.is_empty() && !v.contains('\n'));
        assert_eq!(version_root(v), workspace_root().join(".cache/mc").join(v));
    }

    #[test]
    fn pin_is_independent_of_current() {
        assert_eq!(version_root(PINNED_26_2).file_name().unwrap(), "26.2");
    }
}
