//! Online-mode configuration: session-server verification, key pair and profile-key checks for a server that authenticates its players.

use super::*;

/// The session-server check a login performs once encryption is up: given the
/// HTTP client, the username and the server-id hash, answer whether the
/// client really holds the shared secret it claims to.
///
/// Boxed rather than a plain `fn` pointer so [`OnlineModeConfig::for_test`]
/// can close over a fixture instead of a real client — see that
/// constructor's own doc comment for why a substitutable seam exists here at
/// all rather than only [`OnlineModeConfig::new`].
#[cfg(not(target_arch = "wasm32"))]
pub(super) type SessionVerify = Arc<
    dyn Fn(
            reqwest::Client,
            String,
            String,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = lodestone_auth::Result<Option<lodestone_auth::HasJoinedProfile>>> + Send>,
        > + Send
        + Sync,
>;

/// Configuration for the online-mode encryption + session-server handshake
/// (the online-mode handshake). Pass `Some` to opt a connection into online
/// mode; callers that do not enable authentication pass `None` for offline
/// mode.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct OnlineModeConfig {
    /// The HTTP client the session-server `hasJoined` call uses. Owned by the
    /// caller (rather than constructed fresh per login) so a real host can
    /// share one client's connection pool across every player who joins.
    pub http: reqwest::Client,
    pub(super) verify: SessionVerify,
    /// Host-shared Mojang issuer keys for profile-key provenance. The mutex
    /// covers only the tiny cache update/read; HTTP always happens after it is
    /// released so one slow services request never stalls another login.
    pub(super) profile_key_cache: Arc<Mutex<lodestone_auth::MojangPublicKeyCache>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for OnlineModeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `SessionVerify` (a boxed `Fn`) has no `Debug` impl to derive; a
        // one-line placeholder is more useful than a compile error over a
        // lint.
        f.debug_struct("OnlineModeConfig").finish_non_exhaustive()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl OnlineModeConfig {
    /// The real thing: `verify` calls [`lodestone_auth::has_joined`] against
    /// `sessionserver.mojang.com`.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            verify: Arc::new(|http, username, hash| {
                Box::pin(async move { lodestone_auth::has_joined(&http, &username, &hash).await })
            }),
            profile_key_cache: Arc::new(Mutex::new(
                lodestone_auth::MojangPublicKeyCache::empty(),
            )),
        }
    }

    /// Substitutes a fixture for the real session-server call in integration
    /// tests. This constructor is available in normal builds because those
    /// tests use the versioned protocol crate as a regular dependency.
    ///
    /// The fixture prevents login tests from contacting the external session
    /// service. The crate has no HTTP-mocking dependency, so the verifier is
    /// injected directly at this seam.
    ///
    /// **Not `#[cfg(test)]`**: the login-sequence test that needs it
    /// (`tests/online_mode.rs`) drives the real [`V770ServerProtocol`] from
    /// `lodestone-v26-2`, which has a *normal* dependency on this crate for the
    /// `ServerProtocol` trait. Adding `lodestone-v26-2` as a *dev*-dependency
    /// here (so a `#[cfg(test)] mod tests` unit test could reach it) makes
    /// this crate's own lib-test compilation and the copy `lodestone-v26-2`
    /// links against two different instantiations of the same trait —
    /// measured: `V770ServerProtocol: ServerProtocol is not implemented`
    /// against the crate's own trait. An external `tests/*.rs` binary has no
    /// such self-reference (it depends on this crate exactly once, normally),
    /// so the test using this constructor lives there, and it needs `pub`.
    pub fn for_test(
        verify: impl Fn(String, String) -> lodestone_auth::Result<Option<lodestone_auth::HasJoinedProfile>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        // `reqwest::Client::new()` panics without a crypto provider installed
        // (see `lodestone_auth::install_crypto_provider`'s own doc); this
        // `http` value is never actually used by the fixture `verify` below
        // (it ignores its `_http` parameter), but the field still needs a
        // real, valid `Client` to satisfy the type. Installing twice in one
        // process is not an error.
        lodestone_auth::install_crypto_provider();
        let verify = Arc::new(verify);
        Self {
            http: reqwest::Client::new(),
            verify: Arc::new(move |_http, username, hash| {
                let result = verify(username, hash);
                Box::pin(async move { result })
            }),
            profile_key_cache: Arc::new(Mutex::new(
                lodestone_auth::MojangPublicKeyCache::empty(),
            )),
        }
    }

    /// Returns the latest issuer-key snapshot, refreshing the shared cache
    /// when authlib policy says it is due. A successful response lives for 24
    /// hours; failures retain the last good set and schedule the capped
    /// 5–320-minute backoff. A first-fetch failure returns `None`, which makes
    /// secure-profile enforcement degrade exactly as vanilla does when it
    /// cannot validate profile keys.
    pub(super) async fn profile_key_issuers(
        &self,
        now_millis: i64,
    ) -> Option<lodestone_auth::MojangPublicKeys> {
        let due = self
            .profile_key_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .needs_refresh(now_millis);
        if due {
            let fetched = lodestone_auth::fetch_mojang_public_keys(&self.http).await;
            let mut cache = self
                .profile_key_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match fetched {
                Ok(keys) => cache.record_success(keys, now_millis),
                Err(error) => {
                    cache.record_failure(now_millis);
                    tracing::warn!(
                        error = %error,
                        "Mojang profile-key issuer refresh failed; announcement validation and secure-profile enforcement are unavailable until a key set is cached"
                    );
                }
            }
        }
        self.profile_key_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
    }
}
