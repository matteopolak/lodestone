//! The Microsoft → Xbox Live → XSTS → Minecraft services authentication chain,
//! and the session-server `join` call.
//!
//! This module is native-only: it drives a series of HTTPS requests through
//! [`reqwest`] and a browser build authenticates over a different path. The flow
//! is:
//!
//! 1. request a device code from Microsoft and show the user a code + URL;
//! 2. poll until they finish signing in, yielding an MS OAuth token;
//! 3. exchange it for an Xbox Live token, then an XSTS token;
//! 4. exchange that for a Minecraft services token;
//! 5. fetch the player's profile (name + UUID);
//! 6. later, `POST` the server hash to the session server to prove ownership of
//!    the shared secret during a server join.
//!
//! None of these calls can be exercised without a real Microsoft account, so the
//! crate's automated tests cover only the pure pieces (the server hash and JSON
//! (de)serialisation shapes). This code is written to the documented protocol
//! but is, by construction, unverified end-to-end; see the crate report.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AuthError, Result, XstsErrorKind};

/// The public client ID Mojang's own launcher uses. Callers may substitute
/// their own registered Azure application ID.
pub const MOJANG_CLIENT_ID: &str = "00000000402b5328";

const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBL_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";
const JOIN_URL: &str = "https://sessionserver.mojang.com/session/minecraft/join";
const SCOPE: &str = "XboxLive.signin offline_access";

/// A prompt to show the user so they can authorize the sign-in on another
/// device or browser tab.
#[derive(Debug, Clone)]
pub struct DeviceCodePrompt {
    /// The short code the user types at [`Self::verification_uri`].
    pub user_code: String,
    /// The URL the user visits to enter [`Self::user_code`].
    pub verification_uri: String,
    /// A ready-made human-readable instruction from Microsoft.
    pub message: String,
    device_code: String,
    interval: u64,
    expires_in: u64,
}

impl DeviceCodePrompt {
    /// The server-recommended polling interval, in seconds.
    #[must_use]
    pub fn interval(&self) -> u64 {
        self.interval
    }

    /// Seconds until this device code expires and the user must restart.
    #[must_use]
    pub fn expires_in(&self) -> u64 {
        self.expires_in
    }
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
    message: String,
}

fn default_interval() -> u64 {
    5
}

fn default_expires_in() -> u64 {
    900
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    error: Option<String>,
}

/// The structured OAuth error body Microsoft returns on a 4xx (e.g.
/// `unauthorized_client`, `invalid_request`). Reading it — instead of collapsing
/// the response to a bare status code — is what lets a caller distinguish "your
/// request was malformed" from "your client id is not registered", which is the
/// difference between a bug in us and a configuration gap outside us.
#[derive(Deserialize)]
struct OAuthErrorBody {
    error: String,
    error_description: Option<String>,
}

impl OAuthErrorBody {
    fn into_service_error(self, step: &'static str) -> AuthError {
        let message = match self.error_description {
            Some(desc) => format!("{}: {desc}", self.error),
            None => self.error,
        };
        AuthError::Service { step, message }
    }
}

/// A Microsoft OAuth token pair. The refresh token is what we cache so a later
/// launch can skip the interactive step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsToken {
    /// Short-lived access token used to authenticate with Xbox Live.
    pub access_token: String,
    /// Long-lived token used to obtain a fresh access token without user
    /// interaction.
    pub refresh_token: String,
}

/// A player's public identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The player's current username.
    pub name: String,
    /// The player's account UUID.
    pub id: Uuid,
}

/// An authenticated Minecraft session: a services access token plus the profile
/// it belongs to.
#[derive(Debug, Clone)]
pub struct Session {
    /// The Minecraft services access token (a JWT), used as the `accessToken`
    /// in the session-server join call.
    pub access_token: String,
    /// The authenticated player's profile.
    pub profile: Profile,
}

/// Requests a device code, returning a prompt to show the user.
///
/// # Errors
///
/// Returns [`AuthError::Http`] on a transport failure or [`AuthError::Json`] if
/// the response is malformed.
pub async fn request_device_code(
    client: &reqwest::Client,
    client_id: &str,
) -> Result<DeviceCodePrompt> {
    let http = client
        .post(DEVICE_CODE_URL)
        .form(&[("client_id", client_id), ("scope", SCOPE)])
        .send()
        .await?;
    if !http.status().is_success() {
        let err: OAuthErrorBody = http.json().await?;
        return Err(err.into_service_error("device_code"));
    }
    let resp: DeviceCodeResponse = http.json().await?;
    Ok(DeviceCodePrompt {
        user_code: resp.user_code,
        verification_uri: resp.verification_uri,
        message: resp.message,
        device_code: resp.device_code,
        interval: resp.interval,
        expires_in: resp.expires_in,
    })
}

/// The outcome of a single poll of the token endpoint.
enum PollOutcome {
    /// The user has not finished yet; keep polling at the current interval.
    Pending,
    /// The server asked us to poll more slowly; the caller must add 5 seconds
    /// to its interval (RFC 8628 §3.5).
    SlowDown,
    /// Sign-in completed.
    Complete(MsToken),
}

/// Performs one raw poll of the token endpoint, classifying the response.
async fn poll_raw(
    client: &reqwest::Client,
    client_id: &str,
    device_code: &str,
) -> Result<PollOutcome> {
    let resp: TokenResponse = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", client_id),
            ("device_code", device_code),
        ])
        .send()
        .await?
        .json()
        .await?;
    classify_token_response(resp)
}

/// Classifies a parsed token-endpoint response into a [`PollOutcome`] or error.
///
/// Kept as a pure function (no I/O) so the poll-loop branches that real
/// device-code implementations get wrong — `authorization_pending` vs
/// `slow_down` vs `expired_token` vs a completed token — are exhaustively
/// testable without a network.
fn classify_token_response(resp: TokenResponse) -> Result<PollOutcome> {
    if let Some(err) = resp.error.as_deref() {
        return match err {
            "authorization_pending" => Ok(PollOutcome::Pending),
            "slow_down" => Ok(PollOutcome::SlowDown),
            "authorization_declined" | "access_denied" => Err(AuthError::AuthorizationDeclined),
            "expired_token" => Err(AuthError::DeviceCodeExpired),
            other => Err(AuthError::Service {
                step: "device_token",
                message: other.to_owned(),
            }),
        };
    }

    match (resp.access_token, resp.refresh_token) {
        (Some(access_token), Some(refresh_token)) => Ok(PollOutcome::Complete(MsToken {
            access_token,
            refresh_token,
        })),
        _ => Err(AuthError::Service {
            step: "device_token",
            message: "token response missing access/refresh token".to_owned(),
        }),
    }
}

/// Polls the token endpoint once.
///
/// This is the low-level primitive; most callers want [`PendingLogin::wait`] or
/// [`authenticate_with_device_code`], which drive the poll loop with the correct
/// backoff for you.
///
/// # Errors
///
/// Returns [`AuthError::AuthorizationPending`] (with the poll interval) while the
/// user has not finished — this also covers a `slow_down`, for which the
/// returned interval is bumped by 5 seconds — [`AuthError::AuthorizationDeclined`]
/// if they declined, or [`AuthError::DeviceCodeExpired`] if the code lapsed.
pub async fn poll_token(
    client: &reqwest::Client,
    client_id: &str,
    prompt: &DeviceCodePrompt,
) -> Result<MsToken> {
    match poll_raw(client, client_id, &prompt.device_code).await? {
        PollOutcome::Complete(token) => Ok(token),
        PollOutcome::Pending => Err(AuthError::AuthorizationPending {
            interval: prompt.interval,
        }),
        PollOutcome::SlowDown => Err(AuthError::AuthorizationPending {
            interval: prompt.interval + 5,
        }),
    }
}

/// An in-progress device-code login: the prompt to show the user plus the state
/// needed to poll for completion.
///
/// This is the framework-agnostic seam. A terminal caller can show the prompt
/// and call [`PendingLogin::wait`]; a GUI or browser caller (which must not
/// block) can show the prompt, hold the `PendingLogin`, and call
/// [`PendingLogin::poll_once`] from a timer or button handler. Nothing here
/// assumes a terminal or writes to stdout.
#[derive(Debug)]
pub struct PendingLogin {
    prompt: DeviceCodePrompt,
    interval: u64,
    remaining: u64,
}

impl PendingLogin {
    /// Begins a device-code login by requesting a code from Microsoft.
    ///
    /// # Errors
    ///
    /// Propagates any failure from [`request_device_code`].
    pub async fn begin(client: &reqwest::Client, client_id: &str) -> Result<Self> {
        let prompt = request_device_code(client, client_id).await?;
        let interval = prompt.interval;
        let remaining = prompt.expires_in;
        Ok(Self {
            prompt,
            interval,
            remaining,
        })
    }

    /// The prompt to display to the user.
    #[must_use]
    pub fn prompt(&self) -> &DeviceCodePrompt {
        &self.prompt
    }

    /// The current recommended delay before the next [`Self::poll_once`], in
    /// seconds. It grows if the server returns `slow_down`.
    #[must_use]
    pub fn interval(&self) -> u64 {
        self.interval
    }

    /// Polls once for completion.
    ///
    /// Returns `Ok(None)` while the user has not finished (the caller should wait
    /// [`Self::interval`] seconds and try again), or `Ok(Some(token))` once they
    /// have. A `slow_down` response is handled transparently by increasing
    /// [`Self::interval`].
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::AuthorizationDeclined`] if the user declined or
    /// [`AuthError::DeviceCodeExpired`] once the code has expired.
    pub async fn poll_once(
        &mut self,
        client: &reqwest::Client,
        client_id: &str,
    ) -> Result<Option<MsToken>> {
        match poll_raw(client, client_id, &self.prompt.device_code).await? {
            PollOutcome::Complete(token) => Ok(Some(token)),
            PollOutcome::Pending => Ok(None),
            PollOutcome::SlowDown => {
                self.interval += 5;
                Ok(None)
            }
        }
    }

    /// Drives the poll loop to completion, sleeping [`Self::interval`] seconds
    /// between attempts and giving up when the device code expires.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::AuthorizationDeclined`] or
    /// [`AuthError::DeviceCodeExpired`] as appropriate, or any transport error.
    pub async fn wait(mut self, client: &reqwest::Client, client_id: &str) -> Result<MsToken> {
        loop {
            let delay = self.interval;
            if self.remaining < delay {
                return Err(AuthError::DeviceCodeExpired);
            }
            self.remaining -= delay;
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            if let Some(token) = self.poll_once(client, client_id).await? {
                return Ok(token);
            }
        }
    }
}

/// Runs a full interactive device-code login and returns an authenticated
/// [`Session`].
///
/// `on_prompt` is called once with the user-facing prompt; supply a closure that
/// surfaces it however your front end wants (log line, dialog, web page). It is
/// deliberately *not* a `println!` inside this function, so the library never
/// assumes a terminal.
///
/// # Errors
///
/// Propagates any failure from the device-code, token, Xbox, XSTS,
/// Minecraft-login or profile steps.
pub async fn authenticate_with_device_code<F>(
    client: &reqwest::Client,
    client_id: &str,
    on_prompt: F,
) -> Result<Session>
where
    F: FnOnce(&DeviceCodePrompt),
{
    let pending = PendingLogin::begin(client, client_id).await?;
    on_prompt(pending.prompt());
    let ms_token = pending.wait(client, client_id).await?;
    session_from_ms_token(client, &ms_token.access_token).await
}

/// Refreshes a Microsoft token pair using a stored refresh token, skipping the
/// interactive device-code step.
///
/// # Errors
///
/// Returns [`AuthError::Http`]/[`AuthError::Json`] on transport/parse failure or
/// [`AuthError::Service`] if the refresh was rejected.
pub async fn refresh_token(
    client: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<MsToken> {
    let resp: TokenResponse = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("scope", SCOPE),
        ])
        .send()
        .await?
        .json()
        .await?;
    classify_refresh_response(resp)
}

/// Classifies a parsed refresh-token-endpoint response, kept pure (no I/O) for
/// the same testability reason as [`classify_token_response`].
fn classify_refresh_response(resp: TokenResponse) -> Result<MsToken> {
    match (resp.access_token, resp.refresh_token) {
        (Some(access_token), Some(refresh_token)) => Ok(MsToken {
            access_token,
            refresh_token,
        }),
        // `invalid_grant` is OAuth's code for "this refresh token is dead"
        // (revoked, expired past its own renewal window, password changed —
        // Microsoft does not distinguish which). Callers that want to fall
        // back to an interactive sign-in on a stale cache, and only then,
        // match this variant specifically rather than every `Service` error
        // (a transport hiccup should not silently discard a good cache).
        _ if resp.error.as_deref() == Some("invalid_grant") => Err(AuthError::RefreshTokenInvalid),
        _ => Err(AuthError::Service {
            step: "refresh",
            message: resp
                .error
                .unwrap_or_else(|| "refresh response missing tokens".to_owned()),
        }),
    }
}

#[derive(Deserialize)]
struct XboxResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: DisplayClaims,
}

#[derive(Deserialize)]
struct DisplayClaims {
    xui: Vec<Xui>,
}

#[derive(Deserialize)]
struct Xui {
    uhs: String,
}

/// The result of the Xbox authentication legs: an XSTS token and the user hash
/// that pairs with it.
#[derive(Debug, Clone)]
struct XstsToken {
    token: String,
    user_hash: String,
}

/// Authenticates the MS token with Xbox Live, returning the XBL token.
async fn authenticate_xbl(client: &reqwest::Client, ms_access_token: &str) -> Result<String> {
    let body = serde_json::json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            "RpsTicket": format!("d={ms_access_token}"),
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT",
    });
    let resp: XboxResponse = client
        .post(XBL_URL)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp.token)
}

/// The shape of an XSTS `401` body: `{"Identity":"0","XErr":2148916233,
/// "Message":"...","Redirect":"..."}`. Every field is optional here because
/// this is read best-effort from a body we don't control the shape of.
#[derive(Deserialize)]
struct XstsErrorBody {
    #[serde(rename = "XErr")]
    x_err: Option<i64>,
}

/// Exchanges an XBL token for an XSTS token + user hash.
async fn authorize_xsts(client: &reqwest::Client, xbl_token: &str) -> Result<XstsToken> {
    let body = serde_json::json!({
        "Properties": {
            "SandboxId": "RETAIL",
            "UserTokens": [xbl_token],
        },
        "RelyingParty": "rp://api.minecraftservices.com/",
        "TokenType": "JWT",
    });
    let http = client.post(XSTS_URL).json(&body).send().await?;
    if http.status() == reqwest::StatusCode::UNAUTHORIZED {
        let text = http.text().await.unwrap_or_default();
        // XErr classification is best-effort: an unparsable or code-less body
        // still surfaces as `AuthError::Xsts` (with `XstsErrorKind::Other(0)`)
        // rather than falling back to the less specific `AuthError::Service`,
        // so every 401 here is at least typed as "XSTS rejected this", which
        // is what lets a caller show an XSTS-specific UI state at all.
        let kind = serde_json::from_str::<XstsErrorBody>(&text)
            .ok()
            .and_then(|b| b.x_err)
            .map_or(XstsErrorKind::Other(0), XstsErrorKind::from_code);
        return Err(AuthError::Xsts {
            kind,
            message: text,
        });
    }
    let resp: XboxResponse = http.error_for_status()?.json().await?;
    let user_hash = resp
        .display_claims
        .xui
        .into_iter()
        .next()
        .map(|x| x.uhs)
        .ok_or(AuthError::Service {
            step: "xsts",
            message: "missing user hash in display claims".to_owned(),
        })?;
    Ok(XstsToken {
        token: resp.token,
        user_hash,
    })
}

#[derive(Deserialize)]
struct McLoginResponse {
    access_token: String,
}

/// Exchanges an XSTS token for a Minecraft services access token.
async fn login_with_xbox(client: &reqwest::Client, xsts: &XstsToken) -> Result<String> {
    let identity = format!("XBL3.0 x={};{}", xsts.user_hash, xsts.token);
    let resp: McLoginResponse = client
        .post(MC_LOGIN_URL)
        .json(&serde_json::json!({ "identityToken": identity }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp.access_token)
}

#[derive(Deserialize)]
struct ProfileResponse {
    id: String,
    name: String,
}

/// Fetches the Minecraft profile for a services access token.
async fn fetch_profile(client: &reqwest::Client, mc_access_token: &str) -> Result<Profile> {
    let http = client
        .get(MC_PROFILE_URL)
        .bearer_auth(mc_access_token)
        .send()
        .await?;
    if http.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(AuthError::NoMinecraftProfile);
    }
    let resp: ProfileResponse = http.error_for_status()?.json().await?;
    let id = Uuid::parse_str(&resp.id).map_err(|e| AuthError::Service {
        step: "profile",
        message: format!("invalid profile uuid {:?}: {e}", resp.id),
    })?;
    Ok(Profile {
        name: resp.name,
        id,
    })
}

/// Runs the full chain from a Microsoft access token to an authenticated
/// [`Session`].
///
/// # Errors
///
/// Propagates any failure from the Xbox, XSTS, Minecraft-login or profile steps.
pub async fn session_from_ms_token(
    client: &reqwest::Client,
    ms_access_token: &str,
) -> Result<Session> {
    let xbl = authenticate_xbl(client, ms_access_token).await?;
    let xsts = authorize_xsts(client, &xbl).await?;
    let access_token = login_with_xbox(client, &xsts).await?;
    let profile = fetch_profile(client, &access_token).await?;
    Ok(Session {
        access_token,
        profile,
    })
}

/// Notifies the session server that this player is joining a server, proving
/// possession of the shared secret via the server hash.
///
/// A successful join returns HTTP 204. The server later confirms with the
/// session server using the same hash; a mismatch is what produces the classic
/// "Failed to verify username" disconnect.
///
/// # Errors
///
/// Returns [`AuthError::Service`] if the session server rejects the join.
pub async fn join_server(
    client: &reqwest::Client,
    session: &Session,
    server_hash: &str,
) -> Result<()> {
    let body = serde_json::json!({
        "accessToken": session.access_token,
        "selectedProfile": session.profile.id.simple().to_string(),
        "serverId": server_hash,
    });
    let http = client.post(JOIN_URL).json(&body).send().await?;
    if http.status().is_success() {
        Ok(())
    } else {
        let status = http.status();
        let text = http.text().await.unwrap_or_default();
        Err(AuthError::Service {
            step: "join",
            message: format!("session server returned {status}: {text}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_profile_response_with_undashed_uuid() {
        // The profile endpoint returns the UUID without hyphens; ensure we accept
        // it and that `simple()` round-trips it for the join payload.
        let json = r#"{"id":"069a79f444e94726a5befca90e38aaf5","name":"Notch"}"#;
        let resp: ProfileResponse = serde_json::from_str(json).unwrap();
        let id = Uuid::parse_str(&resp.id).unwrap();
        assert_eq!(resp.name, "Notch");
        assert_eq!(id.simple().to_string(), "069a79f444e94726a5befca90e38aaf5");
    }

    #[test]
    fn detects_authorization_pending_error_shape() {
        let json = r#"{"error":"authorization_pending","error_description":"x"}"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error.as_deref(), Some("authorization_pending"));
        assert!(resp.access_token.is_none());
    }

    fn token_response(json: &str) -> TokenResponse {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn classifies_pending_as_keep_polling() {
        let out = classify_token_response(token_response(r#"{"error":"authorization_pending"}"#));
        assert!(matches!(out, Ok(PollOutcome::Pending)));
    }

    #[test]
    fn classifies_slow_down_distinctly_from_pending() {
        // slow_down must NOT be treated as a plain pending, or the client keeps
        // hammering at the old interval and Microsoft eventually rejects it.
        let out = classify_token_response(token_response(r#"{"error":"slow_down"}"#));
        assert!(matches!(out, Ok(PollOutcome::SlowDown)));
    }

    #[test]
    fn classifies_declined_and_denied_and_expired() {
        assert!(matches!(
            classify_token_response(token_response(r#"{"error":"authorization_declined"}"#)),
            Err(AuthError::AuthorizationDeclined)
        ));
        assert!(matches!(
            classify_token_response(token_response(r#"{"error":"access_denied"}"#)),
            Err(AuthError::AuthorizationDeclined)
        ));
        assert!(matches!(
            classify_token_response(token_response(r#"{"error":"expired_token"}"#)),
            Err(AuthError::DeviceCodeExpired)
        ));
    }

    #[test]
    fn classifies_a_completed_token() {
        let out = classify_token_response(token_response(
            r#"{"access_token":"a","refresh_token":"r"}"#,
        ));
        match out {
            Ok(PollOutcome::Complete(tok)) => {
                assert_eq!(tok.access_token, "a");
                assert_eq!(tok.refresh_token, "r");
            }
            _ => panic!("expected a completed token"),
        }
    }

    #[test]
    fn extracts_token_and_user_hash_from_xbox_response() {
        let json = r#"{"Token":"tok","DisplayClaims":{"xui":[{"uhs":"user-hash"}]}}"#;
        let resp: XboxResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.token, "tok");
        assert_eq!(resp.display_claims.xui[0].uhs, "user-hash");
    }

    #[test]
    fn classifies_a_completed_refresh() {
        let out = classify_refresh_response(token_response(
            r#"{"access_token":"a2","refresh_token":"r2"}"#,
        ));
        let tok = out.expect("expected a completed token");
        assert_eq!(tok.access_token, "a2");
        assert_eq!(tok.refresh_token, "r2");
    }

    #[test]
    fn classifies_invalid_grant_distinctly_from_other_refresh_failures() {
        // The whole point: `invalid_grant` must be recognisable so a caller
        // can fall back to interactive sign-in on exactly this case, and not
        // on e.g. a malformed request (`invalid_request`) that retrying
        // interactively would not fix either.
        let out = classify_refresh_response(token_response(
            r#"{"error":"invalid_grant","error_description":"token expired"}"#,
        ));
        assert!(matches!(out, Err(AuthError::RefreshTokenInvalid)));

        let out = classify_refresh_response(token_response(r#"{"error":"invalid_request"}"#));
        assert!(matches!(
            out,
            Err(AuthError::Service { step: "refresh", .. })
        ));
        assert!(
            !matches!(out, Err(AuthError::RefreshTokenInvalid)),
            "only invalid_grant should map to RefreshTokenInvalid"
        );
    }

    #[test]
    fn xsts_error_kind_maps_the_five_documented_codes() {
        // Values as published by unrelated third-party launchers (see the
        // type's doc comment) — this pins the mapping, not their accuracy.
        assert_eq!(
            XstsErrorKind::from_code(2_148_916_233),
            XstsErrorKind::NoXboxAccount
        );
        assert_eq!(
            XstsErrorKind::from_code(2_148_916_235),
            XstsErrorKind::RegionUnavailable
        );
        assert_eq!(
            XstsErrorKind::from_code(2_148_916_236),
            XstsErrorKind::AdultVerificationRequired
        );
        assert_eq!(
            XstsErrorKind::from_code(2_148_916_237),
            XstsErrorKind::AgeVerificationRequired
        );
        assert_eq!(
            XstsErrorKind::from_code(2_148_916_238),
            XstsErrorKind::ChildAccountNeedsFamily
        );
        assert_eq!(XstsErrorKind::from_code(999), XstsErrorKind::Other(999));
    }

    #[test]
    fn xsts_error_body_extracts_x_err_from_a_realistic_401_payload() {
        // Shape as documented externally: `Identity`/`Message`/`Redirect` are
        // always present alongside `XErr` on a real response; only `XErr` is
        // read here, but the others must not break parsing.
        let json = r#"{"Identity":"0","XErr":2148916233,"Message":"","Redirect":"https://start.ui.xboxlive.com/CreateAccount"}"#;
        let body: XstsErrorBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.x_err, Some(2_148_916_233));
        assert_eq!(XstsErrorKind::from_code(body.x_err.unwrap()), XstsErrorKind::NoXboxAccount);
    }

    #[test]
    fn xsts_error_body_missing_x_err_does_not_fail_to_parse() {
        // A 401 with an unrecognised shape must still parse (to `None`)
        // rather than lose the whole error to a JSON decode failure — the
        // call site falls back to `XstsErrorKind::Other(0)` in that case.
        let body: XstsErrorBody = serde_json::from_str(r#"{"unexpected":"shape"}"#).unwrap();
        assert_eq!(body.x_err, None);
    }

    #[test]
    fn device_code_response_defaults_missing_interval_and_expiry() {
        let resp: DeviceCodeResponse = serde_json::from_str(
            r#"{"device_code":"dc","user_code":"UC","verification_uri":"https://aka.ms/x","message":"m"}"#,
        )
        .unwrap();
        assert_eq!(resp.interval, 5);
        assert_eq!(resp.expires_in, 900);
    }
}
