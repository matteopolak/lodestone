//! Composing `flow` + `store` + `metadata` into an actual login.
//!
//! `docs/accounts.md` sketches this exact composition — this module is that
//! consumer, so a connect path (or a UI driving one) has a single entry point
//! instead of re-deriving the refresh-then-fallback sequence at every call
//! site.
//!
//! Nothing here blocks: [`try_cached_session`] either returns a session
//! immediately or reports that no usable cached token exists, and completing
//! an interactive sign-in is left to the caller driving the existing
//! [`crate::flow::PendingLogin`] (poll-based already) however its front end
//! wants — a terminal prints the prompt and calls `.wait()`; a GUI shows the
//! code and calls `.poll_once()` from a timer. This module does not add a
//! second poll loop on top of that one.

use crate::error::{AuthError, Result};
use crate::flow::{self, MsToken, Session};
use crate::metadata::{AccountProfile, AccountsMetadata};
use crate::store::{CachedSession, SecretStore};

/// This module's wall clock. **Not** `crate::migrate::unix_now` — that
/// function is a plain `SystemTime::now()` confined to the native-only
/// `migrate` module (a legacy-cache one-time migration that has no wasm32
/// caller), and this module now runs on both targets (see this crate's
/// `lib.rs` doc on why `login` is unconditional). `lodestone_time::
/// epoch_duration()` is the portable equivalent every wasm-linked crate in
/// this workspace uses instead of `SystemTime::now()`, which traps on wasm32.
fn unix_now() -> u64 {
    lodestone_time::epoch_duration().as_secs()
}

/// Slack subtracted from a cached session's real `expires_at` when deciding
/// whether it is still usable, so a token that would expire *during* the
/// join that follows (the encryption handshake, then the session-server
/// `join` call) is not handed out as if it were good. 5 minutes is a chosen
/// safety margin, not a spec value or a measurement — Mojang publishes no
/// guidance on how long a join can take — sized to comfortably outlast that
/// handful of round trips on a slow connection while staying a small
/// fraction of the token's real ~24h lifetime, so it costs almost none of
/// the cache's usefulness.
const SESSION_EXPIRY_MARGIN_SECS: u64 = 5 * 60;

/// The pure decision behind the fast path in [`try_cached_session`]: is
/// `expires_at` still usable at `now`, once [`SESSION_EXPIRY_MARGIN_SECS`] is
/// subtracted? Split out (same reasoning as [`resolve_client_id_from`]) so
/// the exact boundary — is equality inside or outside the margin? — is
/// tested against synthetic timestamps rather than the real wall clock,
/// which this repo's own evidence standards flag as an unreliable thing to
/// assert an exact instant against.
fn session_is_still_usable(expires_at: u64, now: u64) -> bool {
    expires_at > now.saturating_add(SESSION_EXPIRY_MARGIN_SECS)
}

/// The environment variable overriding this build's Azure public-client id.
///
/// Set it to run against a different Azure AD application — a fork, a private
/// build, or a second registration for testing. Unset, [`DEFAULT_CLIENT_ID`]
/// applies.
pub const CLIENT_ID_ENV: &str = "LODESTONE_MS_CLIENT_ID";

/// Lodestone's own registered Azure public-client id.
///
/// This is **not** [`crate::flow::MOJANG_CLIENT_ID`], and the distinction is
/// the whole point: that constant is the *official launcher's* registration,
/// and Mojang gates production Minecraft API access per Azure application, so
/// borrowing it would misrepresent this client to Microsoft rather than merely
/// break a style rule. This id is Lodestone's own registration, which is why
/// it can ship as a default at all.
///
/// A public-client id is an identifier, not a credential — Azure public clients
/// hold no secret, the OAuth flow is device-code with PKCE, and every launcher
/// that ships one embeds it the same way. Nothing here is sensitive.
pub const DEFAULT_CLIENT_ID: &str = "0fe8ae70-f564-4969-9b9d-3438f1eb9a09";

/// Reads [`CLIENT_ID_ENV`], falling back to [`DEFAULT_CLIENT_ID`].
///
/// # Errors
/// Returns [`AuthError::MissingClientId`] only when the variable is *set* to a
/// blank or whitespace-only value — an explicit "use nothing", which is a
/// caller mistake worth naming rather than silently papering over with the
/// default. An **unset** variable is not an error.
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
    match value {
        // Unset: the shipped registration.
        None => Ok(DEFAULT_CLIENT_ID.to_owned()),
        Some(raw) => match raw.to_str() {
            Some(id) if !id.trim().is_empty() => Ok(id.to_owned()),
            // Set-but-empty is a caller mistake, not a request for the
            // default — someone wrote the variable and meant something by it.
            _ => Err(AuthError::MissingClientId { env: CLIENT_ID_ENV }),
        },
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
    /// A usable session, reached one of two ways: a still-valid cached
    /// [`crate::store::CachedSession`] (no network call at all — see the
    /// fast path at the top of [`try_cached_session`]), or, when that cache
    /// was cold/expired/unusable, a fresh refresh-token redemption through
    /// the full XBL/XSTS/Mojang-login/profile chain. In the latter case the
    /// rotated refresh token and the newly-derived session have both already
    /// been written back to `secrets` before this is returned.
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

    // Fast path: a still-valid cached session skips the refresh-token
    // redemption *and* the whole XBL/XSTS/Mojang-login/profile chain below —
    // no network call at all. This is the durability win, not just a
    // latency one: the refresh token rotates every time it is redeemed, so
    // avoiding a redemption whenever the cached access token is still good
    // means normal play redeems it far less often, which is strictly safer
    // against the orphaning failure mode `try_cached_session`'s doc above
    // already guards (a redemption whose downstream chain then fails).
    match secrets.load_session(profile_id) {
        Ok(Some(cached)) if session_is_still_usable(cached.expires_at, unix_now()) => {
            if let Some(session) = cached.to_session() {
                return Ok(CachedSessionOutcome::Ready(session));
            }
            // `to_session` only fails on a corrupt `profile_id`, which
            // `KeychainStore::load_session` cannot itself detect (it only
            // sees a JSON parse succeed or fail) — this is the one
            // corruption shape that gets past deserialisation and must be
            // caught here instead. Degrade visibly and fall through to a
            // full sign-in rather than propagating an error that would
            // block the join.
            tracing::warn!(
                target: "auth",
                profile = %profile_id,
                "cached session for this profile had an unparseable profile id; \
                 re-running the full sign-in chain"
            );
        }
        Ok(Some(_)) => {
            tracing::debug!(
                target: "auth",
                profile = %profile_id,
                margin_secs = SESSION_EXPIRY_MARGIN_SECS,
                "cached Minecraft session is within its expiry margin; \
                 redeeming the refresh token and re-running the chain"
            );
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(
                target: "auth",
                profile = %profile_id,
                error = %e,
                "could not read the cached session; re-running the full sign-in chain"
            );
        }
    }

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
            // Cache the derived session so the *next* join can take the fast
            // path above and skip this whole chain (and the refresh-token
            // redemption that gated it) entirely. Best-effort: a failure to
            // cache must not fail a join that otherwise succeeded — it only
            // means the next join redeems the refresh token again, which is
            // exactly today's behaviour.
            if let Err(e) = secrets.save_session(profile_id, &CachedSession::from_session(&session)) {
                tracing::warn!(
                    target: "auth",
                    profile = %profile_id,
                    error = %e,
                    "could not cache the derived session; the next join will redeem the \
                     refresh token again"
                );
            }
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
        /// The selected account's stable identity. Callers retain this for a
        /// server that encrypts but does not request Mojang authentication;
        /// an unavailable online account must not silently become a different
        /// offline identity in that case.
        profile_id: uuid::Uuid,
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
                profile_id,
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
            profile_id,
            detail: "the saved Microsoft session has expired; sign in to this account again"
                .to_owned(),
        },
        // A live transport/parse/keychain failure. Distinguished from the
        // expired-token case in the *text*, because the remedy differs: a
        // player whose network is down should not be told to sign in again.
        Err(e) => SelectedAccount::Unavailable {
            account,
            profile_id,
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
    // Same best-effort cache write as `try_cached_session`'s refresh arm, so
    // an account's very first join after signing in already leaves a warm
    // cache behind for the second one.
    if let Err(e) = secrets.save_session(session.profile.id, &CachedSession::from_session(&session)) {
        tracing::warn!(
            target: "auth",
            profile = %session.profile.id,
            error = %e,
            "could not cache the derived session; the next join will redeem the refresh \
             token again"
        );
    }
    metadata.upsert(AccountProfile {
        profile_id: session.profile.id,
        username: session.profile.name.clone(),
        // `skin_url` is recorded verbatim from the profile response's
        // `skins` array (`flow::fetch_profile`), unscreened at persist time.
        // The host allow list (`crate::texture`) is applied at *fetch* time
        // instead, so a URL that later becomes disallowed cannot be
        // laundered by already being in `profiles.json`.
        skin_url: session.profile.skin.as_ref().map(|s| s.url.clone()),
        last_used: unix_now(),
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
    fn an_unset_variable_yields_the_shipped_registration() {
        assert_eq!(resolve_client_id_from(None).unwrap(), DEFAULT_CLIENT_ID);
    }

    /// The shipped id must be **ours**, never the official launcher's:
    /// Mojang gates Minecraft API access per Azure application, so defaulting
    /// to `flow::MOJANG_CLIENT_ID` would misrepresent this client to Microsoft.
    /// Pinning the inequality rather than the value keeps this honest if the
    /// registration is ever rotated.
    #[test]
    fn the_shipped_id_is_not_the_official_launchers() {
        assert_ne!(DEFAULT_CLIENT_ID, crate::flow::MOJANG_CLIENT_ID);
        assert!(!DEFAULT_CLIENT_ID.trim().is_empty());
    }

    #[test]
    fn a_set_variable_overrides_the_default() {
        let id = resolve_client_id_from(Some(&OsString::from("an-override"))).unwrap();
        assert_eq!(id, "an-override", "an explicit id must win over the default");
        assert_ne!(id, DEFAULT_CLIENT_ID);
    }

    /// Set-but-blank is a caller mistake, not a request for the default —
    /// someone wrote the variable and meant something by it, and silently
    /// substituting the shipped id would hide the typo.
    #[test]
    fn set_but_blank_is_a_typed_error_rather_than_the_default() {
        let err = resolve_client_id_from(Some(&OsString::from(""))).unwrap_err();
        assert!(matches!(err, AuthError::MissingClientId { env } if env == CLIENT_ID_ENV));

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
        // `Client::new()` panics without an installed rustls crypto provider
        // (see `crate::tls`), which makes this test an incidental provider
        // canary too.
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
        let SelectedAccount::Unavailable {
            account,
            profile_id,
            detail,
        } = resolved
        else {
            panic!("a selected account that cannot be resolved must be Unavailable, got {resolved:?}");
        };
        assert_eq!(account, "Alice", "the failure must name the account");
        assert_eq!(profile_id, id, "the selected identity must survive the failure");
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

    /// What remains unverified without a live account: a stored refresh
    /// token actually being *redeemed* (the `flow::refresh_token` /
    /// `flow::session_from_ms_token` HTTP chain itself), and
    /// `finish_interactive`'s full chain. Both are, like the rest of
    /// `flow.rs`, exercised only by the live gate in
    /// `tests/device_code_live.rs`. Present so the gap shows up in `cargo
    /// test` output rather than silently.
    ///
    /// The *cache-hit* path — a still-valid `CachedSession` short-circuiting
    /// both of those — is **not** in this gap any more: it needs no network
    /// at all and is covered by the tests below.
    #[test]
    fn the_refresh_redemption_and_interactive_completion_paths_are_unverified_without_live_credentials(
    ) {
    }

    // -- session cache: the fast path in `try_cached_session` --------------

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Wraps a [`MemoryStore`] and counts calls to `load_refresh_token` —
    /// the operation that gates redeeming the refresh token (and therefore
    /// the whole downstream network chain). A test asserting "no network
    /// call happened" is only as good as evidence the counter would have
    /// caught one; this is that evidence, not an inference from "the test
    /// didn't hang".
    #[derive(Debug, Default)]
    struct CountingStore {
        inner: MemoryStore,
        load_refresh_calls: AtomicUsize,
    }

    impl CountingStore {
        fn new() -> Self {
            Self::default()
        }

        fn load_refresh_calls(&self) -> usize {
            self.load_refresh_calls.load(Ordering::SeqCst)
        }
    }

    impl SecretStore for CountingStore {
        fn save_refresh_token(&self, profile: uuid::Uuid, token: &str) -> Result<()> {
            self.inner.save_refresh_token(profile, token)
        }

        fn load_refresh_token(&self, profile: uuid::Uuid) -> Result<Option<String>> {
            self.load_refresh_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.load_refresh_token(profile)
        }

        fn delete_refresh_token(&self, profile: uuid::Uuid) -> Result<()> {
            self.inner.delete_refresh_token(profile)
        }

        fn save_session(&self, profile: uuid::Uuid, session: &CachedSession) -> Result<()> {
            self.inner.save_session(profile, session)
        }

        fn load_session(&self, profile: uuid::Uuid) -> Result<Option<CachedSession>> {
            self.inner.load_session(profile)
        }

        fn delete_session(&self, profile: uuid::Uuid) -> Result<()> {
            self.inner.delete_session(profile)
        }
    }

    fn cached_session_for(id: uuid::Uuid, expires_at: u64) -> CachedSession {
        CachedSession::from_session(&Session {
            access_token: "cached-mc-access-token".to_owned(),
            profile: flow::Profile {
                name: "Alice".to_owned(),
                id,
                skin: None,
            },
            expires_at,
        })
    }

    fn selected(id: uuid::Uuid) -> AccountsMetadata {
        let mut metadata = AccountsMetadata::default();
        metadata.upsert(AccountProfile {
            profile_id: id,
            username: "Alice".to_owned(),
            skin_url: None,
            last_used: 0,
        });
        metadata.selected = Some(id);
        metadata
    }

    /// The core durability + latency claim: a cached session comfortably
    /// past the expiry margin is returned as `Ready` *and*
    /// `load_refresh_token` is never called — proven by a counter, not by
    /// the absence of a hang. Deliberately seeds **no** refresh token at
    /// all: if the fast path failed to short-circuit, `load_refresh_token`
    /// would return `None` and this would observably become
    /// `SessionExpired` instead of `Ready`, so the wrong hypothesis is
    /// falsifiable here, not just untested.
    #[tokio::test]
    async fn a_warm_cache_returns_ready_and_never_touches_the_refresh_token_store() {
        crate::install_crypto_provider();
        let client = reqwest::Client::new();
        let id = uuid::Uuid::new_v4();
        let store = CountingStore::new();
        store
            .save_session(id, &cached_session_for(id, unix_now() + 10_000))
            .unwrap();

        let outcome = try_cached_session(&client, "test-client-id", &store, &selected(id))
            .await
            .unwrap();
        let CachedSessionOutcome::Ready(session) = outcome else {
            panic!("expected Ready from a warm cache, got {outcome:?}");
        };
        assert_eq!(session.access_token, "cached-mc-access-token");
        assert_eq!(
            store.load_refresh_calls(),
            0,
            "a warm cache must never read the refresh-token store"
        );
    }

    /// The first-join control for the test above: with **no** cache at all,
    /// the same call must fall through and the counter must move — proving
    /// the counter is actually capable of observing a call, not merely
    /// reading zero because it never runs.
    #[tokio::test]
    async fn an_absent_cache_falls_through_and_the_counter_proves_it_moved() {
        crate::install_crypto_provider();
        let client = reqwest::Client::new();
        let id = uuid::Uuid::new_v4();
        let store = CountingStore::new();

        let outcome = try_cached_session(&client, "test-client-id", &store, &selected(id))
            .await
            .unwrap();
        assert!(
            matches!(outcome, CachedSessionOutcome::SessionExpired { .. }),
            "no cache and no refresh token must be SessionExpired, got {outcome:?}"
        );
        assert_eq!(
            store.load_refresh_calls(),
            1,
            "an absent cache must fall through to the refresh-token store exactly once"
        );
    }

    /// A cached session inside the expiry margin is unusable and must fall
    /// through exactly like the absent-cache case — the margin exists
    /// precisely to make this arm unreachable near expiry.
    #[tokio::test]
    async fn a_cached_session_inside_the_margin_falls_through_like_no_cache_at_all() {
        crate::install_crypto_provider();
        let client = reqwest::Client::new();
        let id = uuid::Uuid::new_v4();
        let store = CountingStore::new();
        // 60s from now, well inside the 5-minute margin.
        store
            .save_session(id, &cached_session_for(id, unix_now() + 60))
            .unwrap();

        let outcome = try_cached_session(&client, "test-client-id", &store, &selected(id))
            .await
            .unwrap();
        assert!(
            matches!(outcome, CachedSessionOutcome::SessionExpired { .. }),
            "a within-margin cache must not be used, got {outcome:?}"
        );
        assert_eq!(store.load_refresh_calls(), 1);
    }

    /// A cached session with an unparseable `profile_id` degrades to "no
    /// usable cache" rather than propagating an error that would block the
    /// join — the corruption shape `CachedSession::to_session` returns
    /// `None` for.
    #[tokio::test]
    async fn a_cached_session_with_a_corrupt_profile_id_degrades_and_falls_through() {
        crate::install_crypto_provider();
        let client = reqwest::Client::new();
        let id = uuid::Uuid::new_v4();
        let store = CountingStore::new();
        let mut corrupt = cached_session_for(id, unix_now() + 10_000);
        corrupt.profile_id = "not-a-uuid".to_owned();
        store.save_session(id, &corrupt).unwrap();

        let outcome = try_cached_session(&client, "test-client-id", &store, &selected(id))
            .await
            .unwrap();
        assert!(
            matches!(outcome, CachedSessionOutcome::SessionExpired { .. }),
            "a corrupt cached profile id must degrade to falling through, got {outcome:?}"
        );
        assert_eq!(store.load_refresh_calls(), 1, "the degradation must still reach the refresh-token store");
    }

    /// The pure boundary decision, tested against synthetic timestamps
    /// rather than the real wall clock — this repo's evidence standards
    /// flag exact-instant wall-clock assertions as unreliable, and the
    /// inclusive/exclusive question at the boundary is exactly the kind of
    /// off-by-one a synthetic input can pin down where a live clock cannot.
    #[test]
    fn the_margin_boundary_is_exclusive() {
        let now = 1_700_000_000_u64;
        assert!(
            !session_is_still_usable(now + SESSION_EXPIRY_MARGIN_SECS, now),
            "expiring exactly at the margin must not count as usable"
        );
        assert!(
            session_is_still_usable(now + SESSION_EXPIRY_MARGIN_SECS + 1, now),
            "one second past the margin must count as usable"
        );
        assert!(
            !session_is_still_usable(now, now),
            "already expired must not count as usable"
        );
        assert!(
            !session_is_still_usable(0, now),
            "a zero/epoch expiry (never cached) must not count as usable"
        );
    }
}
