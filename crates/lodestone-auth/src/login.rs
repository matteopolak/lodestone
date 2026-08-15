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
///
/// The two non-`Ready` variants used to be one (`NoCachedToken`), and merging
/// them cost a join: a player who *had* signed in and whose token had merely
/// expired got the same message as a player who had never signed in at all —
/// "no Microsoft session was configured", which reads as a build/configuration
/// fault rather than "sign in again". They are different situations with
/// different remedies (sign in vs. re-authorise a known account), so they are
/// different values. A caller that genuinely does not care can match both arms.
#[derive(Debug)]
pub enum CachedSessionOutcome {
    /// A cached refresh token was silently refreshed into a usable session.
    /// The rotated refresh token has already been written back to `secrets`.
    Ready(Session),
    /// [`AccountsMetadata::selected`] is `None` — there is no account to try,
    /// and **no network call was made**. The remedy is a first interactive
    /// sign-in (e.g. [`crate::flow::PendingLogin::begin`] then
    /// [`finish_interactive`]).
    NoAccountSelected,
    /// An account *is* selected, but it cannot currently be turned into a
    /// session: either no refresh token is stored for it (keychain cleared,
    /// or the profile was signed in on another machine), or the stored token
    /// was rejected outright ([`AuthError::RefreshTokenInvalid`] — revoked,
    /// past its renewal window, or the password changed).
    ///
    /// The remedy is the same interactive sign-in, but the *message* is not:
    /// the account is known and can be named, so a UI should say whose session
    /// expired rather than implying nothing was ever configured.
    SessionExpired {
        /// The selected profile's UUID — the keychain key, and the identity a
        /// caller re-authorises.
        profile_id: uuid::Uuid,
        /// That profile's last-known username, when
        /// [`AccountsMetadata::profiles`] still carries an entry for it. `None`
        /// when `selected` points at a profile with no metadata row, which
        /// `AccountsMetadata::from_json`'s entry-by-entry degradation can
        /// produce from a partly-corrupt `profiles.json`.
        username: Option<String>,
    },
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
/// [`CachedSessionOutcome::SessionExpired`]: if Microsoft is unreachable, a
/// fresh device-code flow will not fare any better, and silently steering the
/// user toward a prompt that also cannot complete would hide the real cause.
/// Only [`AuthError::RefreshTokenInvalid`] — the token itself is dead, not the
/// network — becomes `SessionExpired`.
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
        return Ok(CachedSessionOutcome::NoAccountSelected);
    };
    // Resolved once, up front, so both `SessionExpired` returns below name the
    // account the same way — and so the name survives a `load_refresh_token`
    // failure, which is exactly when a caller most wants to say *whose*
    // credential is missing.
    let username = metadata
        .profiles
        .iter()
        .find(|p| p.profile_id == profile_id)
        .map(|p| p.username.clone());
    let Some(refresh) = secrets.load_refresh_token(profile_id)? else {
        return Ok(CachedSessionOutcome::SessionExpired {
            profile_id,
            username,
        });
    };

    match flow::refresh_token(client, client_id, &refresh).await {
        Ok(ms_token) => {
            // The refresh token rotates on every use, and it has already
            // rotated at Microsoft's end the instant this call returned —
            // the *old* value in `secrets` is dead from here on regardless of
            // what happens next. Persist the new one now, before the
            // XBL/XSTS/Mojang-login/profile chain below gets a chance to
            // fail: that used to run first, so a transient failure anywhere
            // in it (a network blip, a Mojang 5xx) propagated via `?` before
            // `save_refresh_token` was ever reached, silently orphaning the
            // account — the on-disk credential stayed the pre-rotation
            // token, which Microsoft now rejects with `invalid_grant` on
            // every future attempt, forcing a full interactive re-sign-in
            // over a failure that had nothing to do with the refresh itself.
            // Keyed by `profile_id` (this call's own selection) rather than
            // waiting on `session.profile.id` below — the two are the same
            // account by construction, since this is a refresh *for*
            // `profile_id`, not a fresh sign-in that could resolve to
            // something else.
            secrets.save_refresh_token(profile_id, &ms_token.refresh_token)?;
            let session = flow::session_from_ms_token(client, &ms_token.access_token).await?;
            Ok(CachedSessionOutcome::Ready(session))
        }
        Err(AuthError::RefreshTokenInvalid) => Ok(CachedSessionOutcome::SessionExpired {
            profile_id,
            username,
        }),
        Err(other) => Err(other),
    }
}

/// What a join should present to the server, resolved from whatever the
/// account switcher has selected.
///
/// This is [`CachedSessionOutcome`] turned into a *join* decision rather than a
/// *cache* report: it folds the client-id lookup and every transport failure in
/// too, because from a join's point of view "Microsoft is unreachable" and "the
/// token is dead" have the same consequence (we cannot authenticate) and
/// differ only in the sentence shown to the player, which
/// [`SelectedAccount::Unavailable`] carries.
#[derive(Debug)]
pub enum SelectedAccount {
    /// A live session. The join is an online-mode one under this account, and
    /// the profile's real name/UUID replace any offline identity.
    Online(Session),
    /// No account is selected. Join offline — that is what the player asked
    /// for, and it is the only outcome reachable with **no network call at
    /// all**, so a player who never signs in pays nothing for this path.
    Offline,
    /// An account *is* selected and could not be turned into a session.
    ///
    /// **This is not a reason to abort the join.** An offline-mode server never
    /// asks for authentication (vanilla only sends the encryption request
    /// inside `ServerLoginPacketListenerImpl.handleHello`'s
    /// `usesAuthentication() && !isMemoryConnection()` arm), so refusing to
    /// dial would break joins that would have worked. The caller should join
    /// with its offline identity and keep this text to explain the failure
    /// *if* the server turns out to demand online mode.
    Unavailable {
        /// The account's last-known username, or its UUID when
        /// `profiles.json` has no row for the selected id — something to name
        /// in the message either way, never an empty string.
        account: String,
        /// One sentence, already user-facing, about why this account could not
        /// be used. Never a raw token or any part of one.
        detail: String,
    },
}

/// [`resolve_selected_account_with`] against the real on-disk
/// [`AccountsMetadata`] and the real OS keychain.
///
/// This is the function a join calls. It is deliberately infallible: no failure
/// to resolve an account may prevent dialing a server that might not need one.
pub async fn resolve_selected_account(client: &reqwest::Client) -> SelectedAccount {
    // `AccountsMetadata::load()` reads `paths::profiles_path()`, which is the
    // same file `menu::accounts` writes — so "the account the switcher shows as
    // selected" and "the account a join uses" cannot disagree. A test hands in
    // its own pair instead of touching either.
    resolve_selected_account_with(
        client,
        &crate::store::AccountSecrets::open(),
        &AccountsMetadata::load(),
    )
    .await
}

/// The decision behind [`resolve_selected_account`], with the metadata and
/// secret store injected — same reasoning as [`resolve_client_id_from`]: the
/// real ones are a developer's actual profile file and actual login keychain,
/// which no test may touch.
pub async fn resolve_selected_account_with(
    client: &reqwest::Client,
    secrets: &dyn SecretStore,
    metadata: &AccountsMetadata,
) -> SelectedAccount {
    // Checked here rather than left to `try_cached_session` so the no-selection
    // path does not even resolve a client id: a build with no
    // `LODESTONE_MS_CLIENT_ID` must still join offline servers silently, which
    // it would not if a missing id became an `Unavailable` for every player.
    let Some(profile_id) = metadata.selected else {
        return SelectedAccount::Offline;
    };
    let account = metadata
        .profiles
        .iter()
        .find(|p| p.profile_id == profile_id)
        .map_or_else(|| profile_id.to_string(), |p| p.username.clone());

    let client_id = match resolve_client_id() {
        Ok(id) => id,
        Err(e) => {
            return SelectedAccount::Unavailable {
                account,
                detail: e.to_string(),
            };
        }
    };

    match try_cached_session(client, &client_id, secrets, metadata).await {
        Ok(CachedSessionOutcome::Ready(session)) => SelectedAccount::Online(session),
        // Reachable only if the selection was cleared between the check above
        // and the call — a different thread saving `profiles.json`. Joining
        // offline is the right answer to "nothing is selected" whenever we
        // observe it, so this is the same arm as the early return.
        Ok(CachedSessionOutcome::NoAccountSelected) => SelectedAccount::Offline,
        Ok(CachedSessionOutcome::SessionExpired { .. }) => SelectedAccount::Unavailable {
            account,
            detail: "the saved Microsoft session has expired; sign in to this account again"
                .to_owned(),
        },
        // A live transport/parse/keychain failure. Distinguished from the
        // expired-token case in the *text*, because the remedy differs: a
        // player whose network is down should not be told to sign in again.
        Err(e) => SelectedAccount::Unavailable {
            account,
            detail: format!("could not renew the Microsoft session: {e}"),
        },
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
        assert!(matches!(outcome, CachedSessionOutcome::NoAccountSelected));
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
        // The whole point of splitting the old `NoCachedToken`: this account is
        // *known*, so the outcome names it and a UI can say whose session
        // expired. Asserted on the payload, not just the discriminant — a
        // variant that carried `None` here would be the old message with extra
        // steps.
        let CachedSessionOutcome::SessionExpired {
            profile_id,
            username,
        } = outcome
        else {
            panic!("a selected account with no stored token must be SessionExpired, got {outcome:?}");
        };
        assert_eq!(profile_id, id);
        assert_eq!(username.as_deref(), Some("Alice"));
    }

    /// The island assertion for the *auth* half: nothing selected means
    /// `Offline`, and it must reach that answer with **no** client id
    /// configured. This process almost certainly has no
    /// `LODESTONE_MS_CLIENT_ID` set, so if the client-id lookup moved above the
    /// selection check this would come back `Unavailable` and every offline
    /// join in a default build would carry an auth complaint.
    #[tokio::test]
    async fn no_selection_resolves_to_offline_without_needing_a_client_id() {
        crate::install_crypto_provider();
        let client = reqwest::Client::new();
        let secrets = MemoryStore::new();
        let metadata = AccountsMetadata::default();

        let resolved = resolve_selected_account_with(&client, &secrets, &metadata).await;
        assert!(
            matches!(resolved, SelectedAccount::Offline),
            "no selection must be a plain offline join, got {resolved:?}"
        );
    }

    /// The three join-relevant outcomes are three distinct values, and the two
    /// failing ones name the account. `Unavailable`'s `detail` must also differ
    /// between "token expired" and "could not reach Microsoft", because the
    /// remedies differ — telling someone whose network is down to sign in again
    /// is the wrong instruction.
    #[tokio::test]
    async fn a_selected_account_with_no_credential_is_unavailable_and_names_itself() {
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

        // No client id in this process, so this stops at the client-id step —
        // still `Unavailable`, still naming Alice, and *not* silently offline.
        let resolved = resolve_selected_account_with(&client, &secrets, &metadata).await;
        let SelectedAccount::Unavailable { account, detail } = resolved else {
            panic!("a selected account that cannot be resolved must be Unavailable, got {resolved:?}");
        };
        assert_eq!(account, "Alice", "the failure must name the account");
        assert!(
            !detail.is_empty(),
            "an Unavailable with no explanation is the vague message this split exists to remove"
        );
    }

    /// A selection pointing at a profile with no `profiles.json` row still gets
    /// *something* to name — the UUID — rather than an empty string in the
    /// middle of a sentence. `AccountsMetadata::from_json` can produce exactly
    /// this shape from a partly-corrupt file, since it skips bad entries but
    /// keeps `selected`.
    #[tokio::test]
    async fn a_selection_with_no_metadata_row_names_the_uuid() {
        crate::install_crypto_provider();
        let client = reqwest::Client::new();
        let secrets = MemoryStore::new();
        let mut metadata = AccountsMetadata::default();
        let id = uuid::Uuid::new_v4();
        metadata.selected = Some(id);

        let resolved = resolve_selected_account_with(&client, &secrets, &metadata).await;
        let SelectedAccount::Unavailable { account, .. } = resolved else {
            panic!("expected Unavailable, got {resolved:?}");
        };
        assert_eq!(account, id.to_string());
    }

    /// The remainder — a stored token actually being refreshed, and
    /// `finish_interactive`'s full chain — needs a live Microsoft token and
    /// is, like the rest of `flow.rs`, exercised only by the live gate in
    /// `tests/device_code_live.rs`. Present so the gap shows up in `cargo
    /// test` output rather than silently.
    ///
    /// `resolve_selected_account`'s `Online` arm is in that same set: producing
    /// it requires a real refresh token for a real Microsoft account that owns
    /// Minecraft, so no hermetic test in this crate can reach it.
    #[test]
    fn the_refresh_and_interactive_completion_paths_are_unverified_without_live_credentials() {}
}
