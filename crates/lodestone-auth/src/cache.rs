//! The **legacy** on-disk token cache (pre-issue-#64) and its one-time
//! migration into the OS keychain.
//!
//! Before issue #64, this module was the crate's only refresh-token storage:
//! `default_cache_path()` returned one fixed filename
//! (`<data_dir>/lodestone/ms_token.json`), so a second account was impossible
//! by construction, and `save()` wrote the token — a long-lived Microsoft
//! **refresh token** — to that file as plain JSON. Both problems are why this
//! work exists; see `docs/accounts.md`.
//!
//! Ongoing storage is [`crate::store::AccountSecrets`] now. This module keeps
//! only what a one-time migration needs: reading the old file
//! ([`load_legacy_cache`]) and removing it once its token has been moved
//! ([`delete_legacy_cache`]). [`crate::migrate::migrate_legacy_cache`] drives
//! the full sequence and is what a fresh launch should call.
//!
//! `save`/`load` (the raw file I/O) stay `pub(crate)`: nothing should ever
//! write a *new* plaintext token cache again, but tests still need to
//! fabricate a legacy file to exercise the migration path hermetically.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::flow::MsToken;

/// Returns the legacy cache file path
/// (`<data_dir>/lodestone/ms_token.json`) this module used before issue #64.
///
/// Kept only so existing callers/tests that named this function directly
/// still resolve; prefer [`crate::paths::legacy_token_cache_path`] (which this
/// now simply forwards to) in new code.
#[must_use]
pub fn default_cache_path() -> PathBuf {
    crate::paths::legacy_token_cache_path()
}

/// Writes `token` to `path` as plain JSON, creating parent directories as
/// needed. **Legacy format, test-only** — nothing in this crate calls this in
/// production any more (that is the bug this work fixes); it exists purely so
/// tests can fabricate a legacy file to exercise the migration path, and so
/// the shape of the pre-#64 format stays documented in one place rather than
/// re-derived ad hoc in each test.
///
/// # Errors
/// Returns [`crate::AuthError::Cache`] on any filesystem error and
/// [`crate::AuthError::Json`] if serialisation fails.
#[cfg(test)]
pub(crate) fn save(path: &Path, token: &MsToken) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(token)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Loads a cached token from `path`, returning `Ok(None)` if the file does not
/// exist.
///
/// # Errors
/// Returns [`crate::AuthError::Cache`] on a filesystem error other than
/// not-found, or [`crate::AuthError::Json`] if the file is corrupt.
pub(crate) fn load(path: &Path) -> Result<Option<MsToken>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Reads the legacy plaintext token cache at `path`, if present.
///
/// Read-only: this neither writes to the keychain nor deletes the file — it
/// is the low-level primitive [`crate::migrate::migrate_legacy_cache`] uses.
/// A caller wanting the real on-disk location should pass
/// [`crate::paths::legacy_token_cache_path`].
///
/// # Errors
/// Returns [`crate::AuthError::Cache`] on a filesystem error other than
/// not-found. A file that exists but fails to parse as JSON returns
/// [`crate::AuthError::Json`] — the caller should still proceed to
/// [`delete_legacy_cache`] in that case, since nothing in a corrupt token
/// file is recoverable and a leave-behind is the outcome this migration
/// exists to avoid.
pub fn load_legacy_cache(path: &Path) -> Result<Option<MsToken>> {
    load(path)
}

/// Deletes the legacy plaintext token cache at `path`, if it exists.
/// Idempotent: a missing file is not an error.
///
/// # Errors
/// Returns [`crate::AuthError::Cache`] on a filesystem error other than
/// not-found.
pub fn delete_legacy_cache(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".cache-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("ms_token.json")
    }

    #[test]
    fn save_then_load_round_trips_and_missing_is_none() {
        let path = temp_path("roundtrip");
        let dir = path.parent().unwrap().to_path_buf();

        assert!(load(&path).unwrap().is_none());

        let token = MsToken {
            access_token: "a".to_owned(),
            refresh_token: "r".to_owned(),
        };
        save(&path, &token).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.access_token, "a");
        assert_eq!(loaded.refresh_token, "r");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_legacy_cache_is_the_same_read_as_load() {
        let path = temp_path("legacy-read");
        let dir = path.parent().unwrap().to_path_buf();
        assert_eq!(load_legacy_cache(&path).unwrap(), None);
        let token = MsToken {
            access_token: "a".to_owned(),
            refresh_token: "r".to_owned(),
        };
        save(&path, &token).unwrap();
        let loaded = load_legacy_cache(&path).unwrap().unwrap();
        assert_eq!(loaded.refresh_token, "r");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_legacy_cache_removes_the_file_and_is_idempotent() {
        let path = temp_path("legacy-delete");
        let dir = path.parent().unwrap().to_path_buf();

        // Deleting a file that was never created must not error.
        delete_legacy_cache(&path).unwrap();

        save(
            &path,
            &MsToken {
                access_token: "a".to_owned(),
                refresh_token: "r".to_owned(),
            },
        )
        .unwrap();
        assert!(path.exists());
        delete_legacy_cache(&path).unwrap();
        assert!(!path.exists(), "the plaintext file must actually be gone");

        // Control: deleting again (already gone) must still not error — this
        // is the "no silent leave-behind" guarantee having teeth.
        delete_legacy_cache(&path).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_cache_path_agrees_with_the_shared_paths_helper() {
        assert_eq!(default_cache_path(), crate::paths::legacy_token_cache_path());
    }
}
