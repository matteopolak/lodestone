//! One-time migration off the pre-issue-#64 plaintext `ms_token.json` and
//! into the keychain-backed [`crate::store`] plus [`crate::metadata`].
//!
//! A silent leave-behind is the worst outcome here: the file would stay
//! readable while the rest of the system (and the user) believe the token is
//! now protected. [`migrate_legacy_cache`] only deletes the plaintext file
//! *after* the token has been durably re-homed, and logs that it did so
//! without ever logging the token itself or the account's profile id.

use crate::error::Result;
use crate::flow::{self, Profile};
use crate::metadata::{AccountProfile, AccountsMetadata};
use crate::store::SecretStore;

/// Runs the migration once against an explicit legacy-file path (pass
/// [`crate::paths::legacy_token_cache_path`] in production; tests pass a
/// temporary path so nothing here touches a developer's real files or makes
/// a real network call unless a legacy file actually exists).
///
/// If no legacy file exists this is a cheap no-op — `Ok(None)`, no network
/// call. Otherwise:
///
/// 1. refreshes the cached Microsoft token (the cached access token is
///    short-lived and has likely gone stale sitting in that file);
/// 2. derives the account's Minecraft profile from the refreshed token, since
///    the legacy file itself has no profile UUID to key the keychain entry by;
/// 3. stores the (rotated) refresh token under that profile's UUID via
///    `secrets`;
/// 4. upserts a [`AccountProfile`] entry into `metadata` and marks it
///    [`AccountsMetadata::selected`] (there was, by construction, only ever
///    one account under the old single-file cache);
/// 5. deletes the legacy file;
/// 6. logs that a migration happened — never the token, never the profile id.
///
/// `metadata` is **not** saved to disk by this function — the caller decides
/// when to persist it (e.g. after also handling the returned `Profile`),
/// matching how [`AccountsMetadata::save`] is opt-in everywhere else.
///
/// # Errors
/// Propagates any failure from the Microsoft refresh/profile chain or from
/// `secrets`. On error, the legacy file is left in place, so a later launch
/// retries rather than silently losing the account.
pub async fn migrate_legacy_cache(
    client: &reqwest::Client,
    client_id: &str,
    legacy_path: &std::path::Path,
    secrets: &dyn SecretStore,
    metadata: &mut AccountsMetadata,
) -> Result<Option<Profile>> {
    let Some(cached) = crate::cache::load_legacy_cache(legacy_path)? else {
        return Ok(None);
    };

    let refreshed = flow::refresh_token(client, client_id, &cached.refresh_token).await?;
    let session = flow::session_from_ms_token(client, &refreshed.access_token).await?;

    secrets.save_refresh_token(session.profile.id, &refreshed.refresh_token)?;
    metadata.upsert(AccountProfile {
        profile_id: session.profile.id,
        username: session.profile.name.clone(),
        skin_url: None,
        last_used: unix_now(),
    });
    metadata.selected = Some(session.profile.id);

    crate::cache::delete_legacy_cache(legacy_path)?;
    tracing::info!(
        "migrated the legacy plaintext ms_token.json into the OS keychain and removed the file"
    );

    Ok(Some(session.profile))
}

/// Current Unix time in seconds, `0` if the clock is somehow before the
/// epoch. Shared with [`crate::login`], which stamps the same
/// [`AccountProfile::last_used`] field on a normal (non-migration) sign-in.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    fn temp_legacy_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lodestone-auth-migrate-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ms_token.json")
    }

    /// The one part of the migration that is hermetically testable without a
    /// real Microsoft account: the fast path where there is nothing to
    /// migrate. This must not make any network call — if it tried, this test
    /// would hang or fail in a sandboxed/offline CI environment, which is
    /// itself evidence the early return actually short-circuits before
    /// touching `client`.
    #[tokio::test]
    async fn no_legacy_file_is_a_no_op_and_touches_no_network() {
        let path = temp_legacy_path("absent");
        // Issue #446: without an installed rustls crypto provider `Client::new()`
        // panics, so this line doubles as a provider canary.
        crate::install_crypto_provider();
        let client = reqwest::Client::new();
        let secrets = MemoryStore::new();
        let mut metadata = AccountsMetadata::default();

        let result = migrate_legacy_cache(
            &client,
            "test-client-id",
            &path,
            &secrets,
            &mut metadata,
        )
        .await
        .unwrap();

        assert_eq!(result, None);
        assert_eq!(metadata, AccountsMetadata::default(), "nothing should have been touched");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The remainder of the chain (refresh → profile → keychain save →
    /// metadata upsert → delete) requires a live Microsoft refresh token and
    /// is, like the rest of `flow.rs`, exercised only by the live gate in
    /// `tests/device_code_live.rs` — there is no way to construct a hermetic
    /// fake for Microsoft's OAuth endpoint from here. This is a known,
    /// documented gap rather than a claimed coverage this module doesn't
    /// have.
    #[test]
    fn the_full_migration_chain_is_unverified_without_live_credentials() {
        // Intentionally empty: see the doc comment above. Present so the
        // absence of coverage here is a decision that shows up in `cargo
        // test` output, not a silent one.
    }
}
