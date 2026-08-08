//! Composing `flow` + `store` + `metadata` into an actual login (issue #65).
//!
//! `docs/accounts.md` sketched this exact composition when issue #64 landed
//! ("None of that is built yet; this crate only provides the pieces") — this
//! module is that consumer, so a connect path (or a UI driving one) has a
//! single entry point instead of re-deriving the refresh-then-fallback
//! sequence at every call site.
//!
//! Nothing here blocks: [`try_cached_session`] either returns a session
//! immediately or reports that no usable cached token exists, and completing
//! an interactive sign-in is left to the caller driving the existing
//! [`crate::flow::PendingLogin`] (poll-based already) however its front end
//! wants — a terminal prints the prompt and calls `.wait()`; a GUI (issue
//! #66) shows the code and calls `.poll_once()` from a timer. This module
//! does not add a second poll loop on top of that one.

use crate::error::{AuthError, Result};
use crate::flow::{self, MsToken, Session};
use crate::metadata::{AccountProfile, AccountsMetadata};
use crate::store::SecretStore;

/// The environment variable naming this build's registered Azure public-client
/// id.
///
/// There is no id this crate can safely default to: Mojang gates production
/// access to the Minecraft API per Azure application, and
/// [`crate::flow::MOJANG_CLIENT_ID`] is the *official launcher's* registered
/// id, not ours — using it would misrepresent this client to Microsoft, not
/// just violate a style rule. A caller must register their own Azure AD
/// application, request Minecraft API access for it, and set this variable.
pub const CLIENT_ID_ENV: &str = "LODESTONE_MS_CLIENT_ID";

/// Reads [`CLIENT_ID_ENV`] from the process environment.
///
/// # Errors
/// Returns [`AuthError::MissingClientId`] if the variable is unset or empty —
/// a clear, typed error a UI can render, never a panic and never a silent
/// fallback to Mojang's own client id.
#[must_use = "the client id must be used, not discarded"]
pub fn resolve_client_id() -> Result<String> {
    resolve_client_id_from(std::env::var_os(CLIENT_ID_ENV).as_deref())
}

/// The pure decision behind [`resolve_client_id`], taking the environment
/// value as a parameter — same reasoning as `paths::data_dir_from`:
/// `std::env::set_var` is `unsafe` under this workspace's `deny(unsafe_code)`
/// and process env is shared mutable state across a test binary's threads, so
/// tests exercise this function directly rather than mutating the real
/// environment.
fn resolve_client_id_from(value: Option<&std::ffi::OsStr>) -> Result<String> {
    match value.and_then(|v| v.to_str()) {
        Some(id) if !id.trim().is_empty() => Ok(id.to_owned()),
        _ => Err(AuthError::MissingClientId { env: CLIENT_ID_ENV }),
    }
}

/// The result of trying the locally-cached credential.
#[derive(Debug)]
pub enum CachedSessionOutcome {
    /// A cached refresh token was silently refreshed into a usable session.
    /// The rotated refresh token has already been written back to `secrets`.
    Ready(Session),
    /// No usable cached token exists: either no account is selected, no
    /// refresh token is stored for it, or the stored token was rejected
    /// outright ([`AuthError::RefreshTokenInvalid`]). The caller should start
    /// an interactive device-code login (e.g.
    /// [`crate::flow::PendingLogin::begin`]) and, once it completes, call
    /// [`finish_interactive`].
    NoCachedToken,
}

/// Tries the selected account's cached refresh token.
///
/// Makes **no network call at all** when there is nothing to try (no selected
/// account, or no stored token for it) — mirroring
/// [`crate::migrate::migrate_legacy_cache`]'s "nothing to do" fast path, which
/// is what makes that branch hermetically testable and safe to call
/// unconditionally on every launch.
///
/// A transport/parse failure while refreshing ([`AuthError::Http`]/
/// [`AuthError::Json`]) is propagated rather than folded into
/// [`CachedSessionOutcome::NoCachedToken`]: if Microsoft is unreachable, a
/// fresh device-code flow will not fare any better, and silently steering the
/// user toward a prompt that also cannot complete would hide the real cause.
/// Only [`AuthError::RefreshTokenInvalid`] — the token itself is dead, not the
/// network — becomes `NoCachedToken`.
///
/// # Errors
/// Propagates any refresh/profile-fetch failure other than
/// [`AuthError::RefreshTokenInvalid`].
pub async fn try_cached_session(
    client: &reqwest::Client,
    client_id: &str,
    secrets: &dyn SecretStore,
    metadata: &AccountsMetadata,
) -> Result<CachedSessionOutcome> {
    let Some(profile_id) = metadata.selected else {
        return Ok(CachedSessionOutcome::NoCachedToken);
    };
    let Some(refresh) = secrets.load_refresh_token(profile_id)? else {
        return Ok(CachedSessionOutcome::NoCachedToken);
    };

    match flow::refresh_token(client, client_id, &refresh).await {
        Ok(ms_token) => {
            let session = flow::session_from_ms_token(client, &ms_token.access_token).await?;
            // The refresh token rotates on every use; persist the new one or
            // the *next* launch's refresh fails even though this one worked.
            secrets.save_refresh_token(session.profile.id, &ms_token.refresh_token)?;
            Ok(CachedSessionOutcome::Ready(session))
        }
        Err(AuthError::RefreshTokenInvalid) => Ok(CachedSessionOutcome::NoCachedToken),
        Err(other) => Err(other),
    }
}

/// Completes an interactive login after the caller has driven a
/// [`crate::flow::PendingLogin`] to a finished [`MsToken`]: derives the
/// session, saves the refresh token under the profile's UUID, and
/// upserts + selects the account in `metadata`.
///
/// `metadata` is **not** saved to disk here — same opt-in-save convention
/// [`crate::migrate::migrate_legacy_cache`] uses; the caller decides when to
/// persist it.
///
/// # Errors
/// Propagates any failure deriving the session or saving the refresh token.
pub async fn finish_interactive(
    client: &reqwest::Client,
    ms_token: &MsToken,
    secrets: &dyn SecretStore,
    metadata: &mut AccountsMetadata,
) -> Result<Session> {
    let session = flow::session_from_ms_token(client, &ms_token.access_token).await?;
    secrets.save_refresh_token(session.profile.id, &ms_token.refresh_token)?;
    metadata.upsert(AccountProfile {
        profile_id: session.profile.id,
        username: session.profile.name.clone(),
        // Issue #62: this field existed with nothing ever writing it. The profile
        // response's `skins` array is now kept (`flow::fetch_profile`), so the
        // pointer is recorded here — verbatim, unscreened. The host allow list
        // (`crate::texture`) is applied at *fetch* time, not at persist time, so
        // a URL that later becomes disallowed cannot be laundered by already
        // being in `profiles.json`.
        skin_url: session.profile.skin.as_ref().map(|s| s.url.clone()),
        last_used: crate::migrate::unix_now(),
    });
    metadata.selected = Some(session.profile.id);
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use std::ffi::OsString;

    #[test]
    fn missing_client_id_is_a_typed_error_not_a_default() {
        let err = resolve_client_id_from(None).unwrap_err();
        assert!(matches!(err, AuthError::MissingClientId { env } if env == CLIENT_ID_ENV));

        let err = resolve_client_id_from(Some(&OsString::from(""))).unwrap_err();
        assert!(matches!(err, AuthError::MissingClientId { .. }), "blank must not count as set");

        let err = resolve_client_id_from(Some(&OsString::from("   "))).unwrap_err();
        assert!(
            matches!(err, AuthError::MissingClientId { .. }),
            "whitespace-only must not count as set"
        );
    }

    #[test]
    fn a_configured_client_id_is_returned_verbatim() {
        let id = resolve_client_id_from(Some(&OsString::from("abcdef0123456789"))).unwrap();
        assert_eq!(id, "abcdef0123456789");
    }

    /// The two fast paths that must make **no** network call: nothing
    /// selected, and a selection with no stored token. If either tried to
    /// reach Microsoft this test would need real network access to even
    /// observe the (wrong) behaviour, which is itself the evidence these
    /// early returns work — the same reasoning
    /// `no_legacy_file_is_a_no_op_and_touches_no_network` in `migrate.rs`
    /// relies on.
    #[tokio::test]
    async fn no_selected_account_is_no_cached_token_with_no_network_call() {
        // Issue #446: `Client::new()` panics without an installed rustls crypto
        // provider, which makes this test an incidental provider canary too.
        crate::install_crypto_provider();
        let client = reqwest::Client::new();
        let secrets = MemoryStore::new();
        let metadata = AccountsMetadata::default();

        let outcome = try_cached_session(&client, "test-client-id", &secrets, &metadata)
            .await
            .unwrap();
        assert!(matches!(outcome, CachedSessionOutcome::NoCachedToken));
    }

    #[tokio::test]
    async fn selected_account_with_no_stored_token_is_no_cached_token() {
        crate::install_crypto_provider();
        let client = reqwest::Client::new();
        let secrets = MemoryStore::new();
        let mut metadata = AccountsMetadata::default();
        let id = uuid::Uuid::new_v4();
        metadata.upsert(AccountProfile {
            profile_id: id,
            username: "Alice".to_owned(),
            skin_url: None,
            last_used: 0,
        });
        metadata.selected = Some(id);

        let outcome = try_cached_session(&client, "test-client-id", &secrets, &metadata)
            .await
            .unwrap();
        assert!(matches!(outcome, CachedSessionOutcome::NoCachedToken));
    }

    /// The remainder — a stored token actually being refreshed, and
    /// `finish_interactive`'s full chain — needs a live Microsoft token and
    /// is, like the rest of `flow.rs`, exercised only by the live gate in
    /// `tests/device_code_live.rs`. Present so the gap shows up in `cargo
    /// test` output rather than silently.
    #[test]
    fn the_refresh_and_interactive_completion_paths_are_unverified_without_live_credentials() {}
}
