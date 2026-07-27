//! On-disk cache for the Microsoft refresh token.
//!
//! Caching the long-lived refresh token lets a subsequent launch skip the
//! interactive device-code prompt. This is **native-only**: the whole module is
//! gated behind `cfg(not(target_arch = "wasm32"))` rather than a Cargo feature,
//! deliberately. A feature flag participates in Cargo's feature unification, so
//! another crate in the graph enabling it would switch the cache on for *every*
//! target including wasm — where there is no filesystem — silently reintroducing
//! a bug this project has already been bitten by once. A `cfg` on the target
//! architecture cannot be unified away.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::flow::MsToken;

/// Returns the default cache file path (`<data_dir>/lodestone/ms_token.json`),
/// falling back to the current directory if no home/data directory is known.
#[must_use]
pub fn default_cache_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("lodestone").join("ms_token.json")
}

/// Writes the token to `path`, creating parent directories as needed.
///
/// # Errors
///
/// Returns [`crate::AuthError::Cache`] on any filesystem error and
/// [`crate::AuthError::Json`] if serialisation fails.
pub fn save(path: &Path, token: &MsToken) -> Result<()> {
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
///
/// Returns [`crate::AuthError::Cache`] on a filesystem error other than
/// not-found, or [`crate::AuthError::Json`] if the file is corrupt.
pub fn load(path: &Path) -> Result<Option<MsToken>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips_and_missing_is_none() {
        // Write under the crate directory (cwd during unit tests) rather than a
        // temp dir, and clean up afterwards.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!(".cache-test-{}", std::process::id()));
        let path = dir.join("ms_token.json");
        let _ = std::fs::remove_dir_all(&dir);

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
}
