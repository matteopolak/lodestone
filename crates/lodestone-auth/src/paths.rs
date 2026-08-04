//! Shared on-disk locations for account state that is **not** secret: the
//! [`crate::metadata`] file, and the location of the legacy plaintext token
//! cache this crate is migrating away from (`docs/accounts.md`).
//!
//! This duplicates `lodestone-shell/src/menu/servers.rs`'s `data_dir()` /
//! `data_dir_from()` logic byte-for-byte rather than depending on that crate.
//! Two reasons, one structural and one circumstantial:
//!
//! * `lodestone-auth` must stay a leaf the shell depends on, never the other
//!   way around, so it cannot literally call into `lodestone-shell`.
//! * `crates/lodestone-shell/**` (including `config.rs`, the natural home for
//!   a shared path helper) was held by another agent while this was written,
//!   so the alternative — patching `config.rs` to expose the helper — was not
//!   available either. See the crate's own docs for the reasoning; if the
//!   shell's discovery ever changes, this copy must change with it.
//!
//! The duplication is a real cost — two copies that must agree — but it is
//! the one the task's own scope note asks for ("prefer keeping the path logic
//! inside `lodestone-auth`"), and the metadata file's whole point is to sit
//! **beside** `servers.json`/`options.json`, so approximating the directory
//! would silently break that.
//!
//! # Issue #67's "hoist to `lodestone-core`" is stale
//!
//! That issue proposed `lodestone-core` as the shared home because "both
//! crates already depend on it" — checked against the committed
//! `Cargo.toml`s and that is false today: neither `lodestone-auth` nor
//! `lodestone-shell` depends on `lodestone-core`, which is a narrowly-scoped
//! protocol-codec crate (VarInt/NBT, `Encode`/`Decode`) with no reason to grow
//! platform-directory logic. What *is* true, and wasn't when #67 was filed, is
//! simpler: `lodestone-shell` depends on `lodestone-auth` (see
//! `lodestone-shell/Cargo.toml`), so this module is already the correct
//! one-implementation home — the remaining work is deleting the shell's copy
//! in favour of calling [`data_dir`] here, not inventing a third crate. That
//! edit lives in `crates/lodestone-shell/src/menu/servers.rs`, which is
//! outside this crate's ownership.

use std::ffi::OsStr;
use std::path::PathBuf;

/// Returns the platform data directory Lodestone uses for all of its
/// non-secret on-disk state: `servers.json`, `options.json` (both owned by
/// `lodestone-shell`), and — from this crate — `profiles.json`.
///
/// Must match `lodestone_shell::menu::servers::data_dir()` exactly:
/// `LODESTONE_DATA_DIR` overrides everything; otherwise macOS uses
/// `~/Library/Application Support/lodestone`, Windows uses
/// `%APPDATA%/lodestone`, and everything else prefers `$XDG_DATA_HOME` and
/// falls back to `~/.local/share/lodestone`, with a last-resort relative
/// `.lodestone` if there is no home directory at all.
#[must_use]
pub fn data_dir() -> PathBuf {
    data_dir_from(
        std::env::var_os("LODESTONE_DATA_DIR").as_deref(),
        std::env::var_os("HOME").as_deref(),
        std::env::var_os("APPDATA").as_deref(),
        std::env::var_os("XDG_DATA_HOME").as_deref(),
    )
}

/// The pure decision behind [`data_dir`], taking its inputs as parameters
/// rather than reading the process environment directly — same reasoning as
/// the shell's copy: `std::env::set_var` is `unsafe` under this workspace's
/// `deny(unsafe_code)`, and process env is shared mutable state across a test
/// binary's threads.
fn data_dir_from(
    override_dir: Option<&OsStr>,
    home: Option<&OsStr>,
    appdata: Option<&OsStr>,
    xdg: Option<&OsStr>,
) -> PathBuf {
    if let Some(dir) = override_dir {
        return PathBuf::from(dir);
    }
    let home = home.map(PathBuf::from);
    if cfg!(target_os = "macos") {
        if let Some(h) = home {
            return h.join("Library/Application Support/lodestone");
        }
    } else if cfg!(target_os = "windows") {
        if let Some(app) = appdata {
            return PathBuf::from(app).join("lodestone");
        }
    } else if let Some(x) = xdg {
        return PathBuf::from(x).join("lodestone");
    } else if let Some(h) = home {
        return h.join(".local/share/lodestone");
    }
    // Last resort: a namespaced directory under the working directory, so the
    // client still starts on a machine with no HOME at all.
    PathBuf::from(".lodestone")
}

/// The non-secret account metadata file (`docs/accounts.md`): username,
/// profile UUID, skin URL, last-used timestamp, and which account is
/// selected — deliberately everything an account switcher needs to draw its
/// list *without* unlocking the keychain. Lives beside `servers.json` and
/// `options.json`.
#[must_use]
pub fn profiles_path() -> PathBuf {
    data_dir().join("profiles.json")
}

/// The legacy plaintext refresh-token cache this crate wrote before accounts
/// moved into the OS keychain (issue #64). Only consulted by
/// [`crate::cache::load_legacy_cache`] / [`crate::cache::delete_legacy_cache`]
/// / [`crate::migrate::migrate_legacy_cache`] for one-time migration; nothing
/// should write to this path again.
#[must_use]
pub fn legacy_token_cache_path() -> PathBuf {
    data_dir().join("ms_token.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn the_override_wins_and_is_used_verbatim() {
        let o = OsString::from("/tmp/lodestone-auth-test-dir");
        assert_eq!(
            data_dir_from(Some(&o), None, None, None),
            PathBuf::from("/tmp/lodestone-auth-test-dir")
        );
    }

    #[test]
    fn every_platform_branch_lands_somewhere_per_user_and_namespaced() {
        // XDG wins over the HOME fallback on non-mac/non-Windows; every
        // branch appends the app name rather than handing back the bare base
        // directory. `cfg!` only picks one arm at compile time, so only the
        // arm for *this* platform is asserted concretely.
        let xdg = OsString::from("/xdg/data");
        let home = OsString::from("/home/someone");
        let appdata = OsString::from(r"C:\Users\someone\AppData\Roaming");
        let d = data_dir_from(None, Some(&home), Some(&appdata), Some(&xdg));
        assert_ne!(
            d,
            PathBuf::from(&xdg),
            "must append the app name, not use the base dir directly"
        );
        assert!(d.to_string_lossy().contains("lodestone"), "{d:?}");
        if cfg!(target_os = "macos") {
            assert_eq!(
                d,
                PathBuf::from("/home/someone/Library/Application Support/lodestone")
            );
        } else if cfg!(target_os = "windows") {
            assert_eq!(
                d,
                PathBuf::from(r"C:\Users\someone\AppData\Roaming").join("lodestone")
            );
        } else {
            assert_eq!(d, PathBuf::from("/xdg/data/lodestone"));
        }

        // Nothing at all in the environment must still yield a usable,
        // non-empty, namespaced path — auth has to be able to start.
        let last = data_dir_from(None, None, None, None);
        assert!(!last.as_os_str().is_empty());
        assert!(last.to_string_lossy().contains("lodestone"), "{last:?}");
    }

    #[test]
    fn profiles_and_legacy_cache_live_beside_each_other() {
        assert_eq!(profiles_path().parent(), legacy_token_cache_path().parent());
        assert_eq!(profiles_path().file_name().unwrap(), "profiles.json");
        assert_eq!(legacy_token_cache_path().file_name().unwrap(), "ms_token.json");
    }

    #[test]
    fn the_real_accessor_agrees_on_the_namespace() {
        assert!(data_dir().to_string_lossy().contains("lodestone"));
    }
}
