//! The **loopback authorization-code** sign-in: the real Microsoft login page in
//! the user's own browser, with no code to type.
//!
//! # What it is
//!
//! [`crate::flow`]'s device-code flow shows a short code and asks the user to
//! type it at `microsoft.com/link`. It works everywhere, needs no listener and no
//! browser, and is the only option on a headless host — but it is two devices'
//! worth of ceremony for someone sitting at the machine. This module is the other
//! front end: it opens the browser at Microsoft's ordinary login page, catches the
//! redirect on a loopback socket, and the user never sees a code.
//!
//! **Everything downstream is unchanged.** Both flows end at the same
//! [`MsToken`], so Xbox Live → XSTS → `login_with_xbox` → profile is one code
//! path with one set of tests. The only thing that differs is how the
//! authorization code gets back to us.
//!
//! # How it works
//!
//! This is RFC 8252 (*OAuth 2.0 for Native Apps*) §7.3, the loopback
//! interception redirect, with RFC 7636 PKCE:
//!
//! 1. Bind `127.0.0.1:0` — the OS picks a free port, so nothing is hardcoded and
//!    two instances cannot collide. The bound port becomes part of the
//!    `redirect_uri`, which is why the listener must exist *before* the URL.
//! 2. Generate a PKCE verifier and its S256 challenge, and a random `state`.
//! 3. The **caller** opens the browser at [`LoopbackLogin::authorize_url`].
//!    Launching a browser is a shell concern and `menu/accounts.rs` already
//!    owns `open_in_browser`; a second copy here would be one implementation
//!    too many of a thing that differs per platform.
//! 4. The user signs in. Microsoft redirects to
//!    `http://127.0.0.1:<port>/?code=…&state=…`, which their browser dutifully
//!    requests from us. We read one request line, answer with a small page telling
//!    them to go back to the game, and keep the `code`.
//! 5. Exchange the code — plus the PKCE *verifier* — for a token at the same
//!    `TOKEN_URL` the device flow uses.
//!
//! ## Why loopback rather than an embedded webview
//!
//! An embedded webview would keep the login inside the game window, which is what
//! the official launcher does. It is deliberately not what this does:
//!
//! * RFC 8252 §8.12 recommends against it — an embedded user-agent can read what
//!   the user types, so the user has no way to tell a real login from a fake one.
//!   Loopback hands sign-in to a browser whose address bar the user can check.
//! * It keeps working with passkeys, hardware keys, password managers and
//!   whatever 2FA the account has, because it *is* the user's browser.
//! * Microsoft increasingly detects and refuses embedded browsers for personal
//!   accounts, so the webview route is the one that breaks without warning.
//! * The browser very often has a live Microsoft session already, in which case
//!   sign-in is one click rather than a password.
//!
//! The cost is that focus leaves the game for a moment. That is the trade, and it
//! is the one every other native app makes.
//!
//! # How to change it
//!
//! The two halves are separable on purpose, and the seam is where the tests live:
//!
//! * [`build_authorize_url`] and [`parse_redirect_request`] are **pure** — no
//!   socket, no browser, no clock. Every parsing and encoding rule is asserted
//!   against them directly.
//! * [`LoopbackLogin`] is the I/O shell. It exposes
//!   [`poll_once`](LoopbackLogin::poll_once), deliberately shaped like
//!   [`crate::flow::PendingLogin::poll_once`] so a UI driving one can drive the
//!   other from the same timer without a second code path.
//!
//! If you add a query parameter, add it to `build_authorize_url` and assert it
//! there; do not build URLs at the call site.
//!
//! # Configuration
//!
//! Nothing here is configurable at runtime, and the client id is a parameter
//! rather than a constant — see [`crate::login::CLIENT_ID_ENV`].
//!
//! **The Azure app registration needs exactly `http://localhost` as a redirect
//! URI** — no port — under platform *Mobile and desktop applications*. Azure
//! treats `http://localhost` as loopback and ignores the port, which is the only
//! reason an OS-assigned random port can work: a URI Azure matched *including* the
//! port could not be registered in advance.
//!
//! The registered string and [`LoopbackLogin::begin`]'s `redirect_uri` must agree
//! on the spelling, and they did not at first: this doc said `localhost` while the
//! code sent `127.0.0.1`. Azure string-matches the host, so that combination fails
//! with `invalid_request: The provided value for the input parameter
//! 'redirect_uri' is not valid` — which reads like a missing registration rather
//! than a mismatched one. See that function for why the code moved rather than the
//! instruction.
//!
//! The device-code flow needs no redirect URI at all, so an app registered only for
//! that will fail here until the entry is added.
//!
//! # Dependencies
//!
//! `tokio` for the listener, `reqwest` for the exchange, `sha2` + `base64` for the
//! PKCE challenge, `rand` for the verifier and `state`. Native-only: a wasm build
//! can neither bind a socket nor launch a browser.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore as _;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use crate::error::{AuthError, Result};
use crate::flow::MsToken;

/// Microsoft's authorization endpoint, the `/consumers/` tenant.
///
/// `/consumers/` — not `/common/` — because a Minecraft account is a personal
/// Microsoft account. It matches [`crate::flow`]'s device-code and token URLs, and
/// it is the reason an Azure app for this must be registered as *Personal
/// Microsoft accounts only*.
const AUTHORIZE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize";

/// The token endpoint. Identical to [`crate::flow`]'s — the same endpoint serves
/// both grant types, so only `grant_type` and its companion fields differ.
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";

/// The scope both flows request. Kept in step with [`crate::flow`]'s deliberately:
/// a token minted under a different scope would fail later, at Xbox Live, with an
/// error that says nothing about scope.
const SCOPE: &str = "XboxLive.signin offline_access";

/// How long to wait for the browser redirect before giving up.
///
/// Generous because the user may be typing a password, fetching a phone for 2FA,
/// or picking between several signed-in accounts. Microsoft's own authorization
/// codes are far shorter-lived than this, so the practical cap is theirs, not ours.
const REDIRECT_TIMEOUT: Duration = Duration::from_secs(300);

/// Cap on the request bytes read from the browser.
///
/// Only the first line is ever needed — `GET /?code=… HTTP/1.1` — and a browser
/// sends a few hundred bytes of headers after it. The cap exists so a client that
/// never closes the connection cannot make us read forever; it is a robustness
/// bound, not a protocol limit.
const MAX_REQUEST_BYTES: usize = 8192;

/// The page the browser lands on after a successful redirect.
///
/// Deliberately tiny and self-contained: no external CSS, no fonts, nothing to
/// fetch. It is served from a socket that closes immediately afterwards, so any
/// reference to a second resource would fail.
const SUCCESS_BODY: &str = "<!doctype html><meta charset=utf-8>\
<title>Signed in</title>\
<body style=\"font-family:system-ui,sans-serif;text-align:center;padding:4rem\">\
<h2>Signed in</h2><p>You can close this tab and go back to Lodestone.</p>";

/// The page served when Microsoft redirects with an error instead of a code.
const FAILURE_BODY: &str = "<!doctype html><meta charset=utf-8>\
<title>Sign-in failed</title>\
<body style=\"font-family:system-ui,sans-serif;text-align:center;padding:4rem\">\
<h2>Sign-in failed</h2><p>Go back to Lodestone for the details.</p>";

/// A PKCE verifier and the S256 challenge derived from it (RFC 7636 §4.1-4.2).
///
/// The verifier never leaves this process until the *token* request, and the
/// challenge is what travels in the browser-visible URL. That asymmetry is the
/// whole point: an attacker who intercepts the redirect gets a code they cannot
/// exchange, because they never saw the verifier.
#[derive(Debug, Clone)]
pub struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    /// Generates a fresh verifier from the OS CSPRNG.
    ///
    /// 32 random bytes, base64url-encoded to 43 characters — the shortest length
    /// RFC 7636 §4.1 permits, and the length every major provider uses.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let challenge = Self::challenge_for(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    /// The S256 challenge for a given verifier: `base64url(sha256(ascii(verifier)))`.
    ///
    /// Split out from [`generate`](Self::generate) so it can be asserted against
    /// RFC 7636's own published vector, which is the only way to know the encoding
    /// is right without a live provider. A `base64` variant with padding, or the
    /// standard rather than URL-safe alphabet, both produce a plausible-looking
    /// string that Microsoft rejects.
    #[must_use]
    pub fn challenge_for(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }

    /// The challenge, for the authorize URL.
    #[must_use]
    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    /// The verifier, for the token exchange.
    #[must_use]
    pub fn verifier(&self) -> &str {
        &self.verifier
    }
}

/// What a redirect request turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectOutcome {
    /// An authorization code, with the `state` the browser echoed back. The
    /// caller must still compare `state` against the one it generated —
    /// [`parse_redirect_request`] cannot, because it does not know it.
    Code { code: String, state: String },
    /// Microsoft redirected with an error rather than a code — the user pressed
    /// cancel, or consent was refused.
    Error {
        error: String,
        description: Option<String>,
    },
}

/// Builds the authorization URL.
///
/// Pure, so every parameter and every escape is asserted without a network call.
/// `response_type=code` selects the authorization-code grant; `code_challenge` +
/// `code_challenge_method=S256` are PKCE; `state` is CSRF protection.
#[must_use]
pub fn build_authorize_url(
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> String {
    // Hand-rolled rather than pulling a URL crate: five parameters, all of which
    // we generate ourselves except `client_id`. `percent_encode` below is applied
    // to every value, including the ones that cannot currently contain a reserved
    // character, because "cannot currently" is how encoding bugs are born.
    let mut url = String::with_capacity(AUTHORIZE_URL.len() + 256);
    url.push_str(AUTHORIZE_URL);
    url.push('?');
    for (i, (key, value)) in [
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPE),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        // Without this a browser with a live session silently reuses the account
        // it already has, which makes "add a *second* account" impossible — the
        // user is never offered a choice and the same profile arrives twice.
        ("prompt", "select_account"),
    ]
    .into_iter()
    .enumerate()
    {
        if i > 0 {
            url.push('&');
        }
        url.push_str(key);
        url.push('=');
        url.push_str(&percent_encode(value));
    }
    url
}

/// Percent-encodes everything outside RFC 3986's unreserved set.
///
/// Deliberately strict: encoding a character that did not need it is harmless,
/// while missing one truncates a parameter. The space in [`SCOPE`] is the case
/// that actually matters — it must become `%20`, and a `+` would be wrong in a
/// query *value* here even though servers often tolerate it.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Decodes one `application/x-www-form-urlencoded` query value.
///
/// Handles `%XX` and `+`. A malformed escape is left as literal text rather than
/// rejected: this parses a redirect we are about to validate by `state` anyway,
/// and failing the whole sign-in over a stray `%` would be worse than passing a
/// value that then fails to match.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses the browser's HTTP request line into a [`RedirectOutcome`].
///
/// Pure, and takes the whole request text so the "only the first line matters"
/// rule is asserted rather than assumed. Accepts the request line in its real
/// shape — `GET /?code=…&state=… HTTP/1.1` — and tolerates a path other than `/`,
/// since nothing here depends on it.
///
/// # Errors
///
/// [`AuthError::Service`] when the request is not a parseable HTTP request line,
/// or carries neither `code` nor `error`. Both mean something other than
/// Microsoft's redirect arrived on the port — a browser prefetch, or a stray
/// `/favicon.ico`.
pub fn parse_redirect_request(request: &str) -> Result<RedirectOutcome> {
    let line = request.lines().next().unwrap_or_default();
    // "GET <target> HTTP/1.1" — take the middle field.
    let mut parts = line.split_whitespace();
    let _method = parts.next();
    let target = parts.next().ok_or_else(|| AuthError::Service {
        step: "redirect",
        message: format!("not an HTTP request line: {line:?}"),
    })?;

    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, raw) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "code" => code = Some(percent_decode(raw)),
            "state" => state = Some(percent_decode(raw)),
            "error" => error = Some(percent_decode(raw)),
            "error_description" => description = Some(percent_decode(raw)),
            _ => {}
        }
    }

    if let Some(error) = error {
        return Ok(RedirectOutcome::Error { error, description });
    }
    match code {
        Some(code) => Ok(RedirectOutcome::Code {
            code,
            state: state.unwrap_or_default(),
        }),
        None => Err(AuthError::Service {
            step: "redirect",
            message: format!(
                "redirect carried neither `code` nor `error` (target {target:?}); \
                 something other than Microsoft's redirect reached the port"
            ),
        }),
    }
}

/// An in-progress browser sign-in: a bound loopback listener plus the PKCE and
/// `state` secrets the eventual redirect is validated against.
///
/// Shaped to mirror [`crate::flow::PendingLogin`] so a UI can drive either from
/// one timer. Dropping it closes the listener, which is the whole teardown —
/// there is no task to leak.
pub struct LoopbackLogin {
    listener: TcpListener,
    redirect_uri: String,
    authorize_url: String,
    pkce: Pkce,
    state: String,
    started: Instant,
}

impl LoopbackLogin {
    /// Binds a loopback listener and builds the authorization URL.
    ///
    /// Binds `127.0.0.1:0` — explicitly loopback, never `0.0.0.0`, so the socket
    /// is unreachable from the network — and lets the OS pick the port. The port
    /// is only known *after* binding, and it is part of the `redirect_uri`, which
    /// is why the two cannot be separated.
    ///
    /// # Errors
    ///
    /// [`AuthError::Service`] if the listener cannot bind or its address cannot
    /// be read.
    pub async fn begin(client_id: &str) -> Result<Self> {
        let listener =
            TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|e| AuthError::Service {
                    step: "loopback_bind",
                    message: format!("could not bind a loopback port for the sign-in redirect: {e}"),
                })?;
        let port = listener
            .local_addr()
            .map_err(|e| AuthError::Service {
                step: "loopback_bind",
                message: format!("bound a loopback port but could not read it back: {e}"),
            })?
            .port();

        // **`localhost`, not `127.0.0.1`** — and the reason is Azure, not us.
        //
        // The literal would be preferable on its own merits: it cannot be
        // redirected by a hosts file, and it cannot resolve to IPv6 `::1` while we
        // listen on IPv4. That was this line's first form, with a comment asserting
        // "Azure's loopback exemption covers both spellings and ignores the port".
        //
        // It does not, or at least not verifiably: Microsoft documents the
        // port-agnostic loopback exemption for `http://localhost`, and a registered
        // `http://127.0.0.1` is not documented to wildcard the port. Since the port
        // is chosen by the OS at bind time, a redirect URI Azure matches *with* its
        // port cannot be registered in advance at all — so `localhost` is the only
        // spelling that can work here. Reported from play as
        // `invalid_request: The provided value for the input parameter
        // 'redirect_uri' is not valid`.
        //
        // Residual risk, accepted knowingly: a browser that resolves `localhost` to
        // `::1` will not reach a listener bound to `127.0.0.1`. Browsers try the
        // other family on a refused connection, and the listener stays IPv4-only
        // because binding both families needs two sockets and Azure only needs the
        // *string* to say `localhost`. If a callback ever hangs with the browser on
        // the redirect page, this is the first thing to suspect.
        let redirect_uri = format!("http://localhost:{port}");

        let pkce = Pkce::generate();
        let mut state_bytes = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut state_bytes);
        let state = URL_SAFE_NO_PAD.encode(state_bytes);
        let authorize_url =
            build_authorize_url(client_id, &redirect_uri, pkce.challenge(), &state);

        Ok(Self {
            listener,
            redirect_uri,
            authorize_url,
            pkce,
            state,
            started: Instant::now(),
        })
    }

    /// The URL the user must visit. Shown in the UI as a fallback so a failed
    /// browser launch is recoverable by copy-paste rather than fatal.
    #[must_use]
    pub fn authorize_url(&self) -> &str {
        &self.authorize_url
    }

    /// Whether the user has taken longer than [`REDIRECT_TIMEOUT`].
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.started.elapsed() >= REDIRECT_TIMEOUT
    }

    /// Accepts a pending redirect if one has arrived, without blocking.
    ///
    /// Returns `Ok(None)` when no browser has connected yet, so a UI can call this
    /// from a frame timer. Mirrors [`crate::flow::PendingLogin::poll_once`]'s
    /// shape for exactly that reason.
    ///
    /// # Errors
    ///
    /// [`AuthError::Service`] on a `state` mismatch (a redirect we did not
    /// initiate), on a redirect carrying an OAuth error, or if the token exchange
    /// fails.
    pub async fn poll_once(&mut self, http: &reqwest::Client, client_id: &str) -> Result<Option<MsToken>> {
        // `try_accept`-shaped: one non-blocking probe per call.
        let stream = match self.listener.accept().now_or_never_compat().await {
            Some(accepted) => {
                accepted
                    .map_err(|e| AuthError::Service {
                        step: "redirect",
                        message: format!("loopback accept failed: {e}"),
                    })?
                    .0
            }
            None => return Ok(None),
        };

        let outcome = self.read_and_answer(stream).await?;
        match outcome {
            RedirectOutcome::Error { error, description } => Err(AuthError::Service {
                step: "redirect",
                message: match description {
                    Some(d) => format!("{error}: {d}"),
                    None => error,
                },
            }),
            RedirectOutcome::Code { code, state } => {
                // Constant-time is unnecessary — `state` is a public CSRF nonce,
                // not a secret — but the comparison itself is not optional: it is
                // what stops an unrelated page on the machine walking us through
                // an authorization it started.
                if state != self.state {
                    return Err(AuthError::Service {
                        step: "redirect",
                        message: "the redirect's `state` did not match the one we sent; \
                                  ignoring a sign-in this client did not start"
                            .to_owned(),
                    });
                }
                let token = exchange_code(
                    http,
                    client_id,
                    &code,
                    &self.redirect_uri,
                    self.pkce.verifier(),
                )
                .await?;
                Ok(Some(token))
            }
        }
    }

    /// Reads the request, answers with a small page, and parses the query.
    async fn read_and_answer(&self, mut stream: TcpStream) -> Result<RedirectOutcome> {
        let mut buf = Vec::with_capacity(1024);
        let mut chunk = [0u8; 1024];
        // Read until the end of the headers, the cap, or EOF. The request line is
        // all we need and it arrives first, so this cannot block waiting for a
        // body that a GET does not have.
        loop {
            let n = stream.read(&mut chunk).await.map_err(|e| AuthError::Service {
                step: "redirect",
                message: format!("could not read the redirect request: {e}"),
            })?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() >= MAX_REQUEST_BYTES || buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }

        let request = String::from_utf8_lossy(&buf);
        let outcome = parse_redirect_request(&request);
        let body = match &outcome {
            Ok(RedirectOutcome::Code { .. }) => SUCCESS_BODY,
            _ => FAILURE_BODY,
        };
        // Best-effort: the user's sign-in already succeeded or failed by now, and
        // failing to paint their tab must not change that verdict.
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.flush().await;
        outcome
    }
}

/// Exchanges an authorization code for a token pair.
///
/// The PKCE `code_verifier` goes here and nowhere else — this is the request that
/// proves we are the client that started the flow. `redirect_uri` must be
/// byte-identical to the one in the authorize URL; Microsoft compares them and
/// rejects a mismatch with `invalid_grant`, which reads like an expired code.
///
/// # Errors
///
/// [`AuthError::Service`] on an OAuth error body, or if the response is a 200
/// carrying no `access_token`.
pub async fn exchange_code(
    http: &reqwest::Client,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<MsToken> {
    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
            ("scope", SCOPE),
        ])
        .send()
        .await?;

    if !resp.status().is_success() {
        let err: OAuthErrorBody = resp.json().await?;
        return Err(err.into_service_error("token"));
    }

    let body: TokenResponse = resp.json().await?;
    match (body.access_token, body.refresh_token) {
        (Some(access_token), Some(refresh_token)) => Ok(MsToken {
            access_token,
            refresh_token,
        }),
        (Some(access_token), None) => Ok(MsToken {
            access_token,
            // No refresh token means `offline_access` was not granted. The session
            // still works; it simply cannot be resumed on a later launch, and an
            // empty string is what the cache already treats as "nothing to try".
            refresh_token: String::new(),
        }),
        (None, _) => Err(AuthError::Service {
            step: "token",
            message: "Microsoft returned success with no access token".to_owned(),
        }),
    }
}

/// The token endpoint's success body. A local mirror of [`crate::flow`]'s private
/// one — the two are separate so neither module can silently change the other's
/// wire contract.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
}

/// The OAuth error body, as in [`crate::flow`]: read rather than collapsed to a
/// status code, because `invalid_request` naming the redirect URI is a
/// configuration gap outside us while `invalid_grant` is a stale code.
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

/// `FutureExt::now_or_never` for the accept probe, without pulling `futures`.
///
/// Polls the future exactly once: `Some` if it was already ready, `None` if it
/// would have parked. That is precisely "did a browser connect yet", and it is
/// what lets [`LoopbackLogin::poll_once`] be called from a frame timer without a
/// blocking accept or a spawned task.
trait NowOrNever: Sized {
    #[allow(async_fn_in_trait)]
    async fn now_or_never_compat(self) -> Option<Self::Output>
    where
        Self: std::future::Future;
}

impl<F: std::future::Future> NowOrNever for F {
    async fn now_or_never_compat(self) -> Option<F::Output> {
        use std::task::{Context, Poll, Waker};
        let mut pinned = std::pin::pin!(self);
        let mut cx = Context::from_waker(Waker::noop());
        match pinned.as_mut().poll(&mut cx) {
            Poll::Ready(value) => Some(value),
            Poll::Pending => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B's published vector. This is the whole reason
    /// [`Pkce::challenge_for`] is a separate function: the expected value
    /// originates in the RFC, not in our own encoder, so a wrong base64 alphabet
    /// or stray padding fails here rather than at Microsoft.
    #[test]
    fn the_pkce_challenge_matches_rfc_7636s_published_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            Pkce::challenge_for(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    /// A generated verifier must be RFC-legal: 43-128 characters from the
    /// unreserved set, and its challenge must be the S256 of *that* verifier.
    #[test]
    fn a_generated_verifier_is_rfc_legal_and_self_consistent() {
        let pkce = Pkce::generate();
        assert_eq!(pkce.verifier().len(), 43, "32 bytes base64url is 43 chars");
        assert!(
            pkce.verifier()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')),
            "verifier {:?} escaped the unreserved set",
            pkce.verifier()
        );
        assert_eq!(pkce.challenge(), Pkce::challenge_for(pkce.verifier()));
        // Two calls must not agree, or the CSPRNG is not being consulted.
        assert_ne!(Pkce::generate().verifier(), pkce.verifier());
    }

    #[test]
    fn the_authorize_url_carries_every_required_parameter() {
        let url = build_authorize_url("cid", "http://127.0.0.1:5000", "chal", "st8");
        assert!(url.starts_with(AUTHORIZE_URL), "{url}");
        for expected in [
            "client_id=cid",
            "response_type=code",
            "code_challenge=chal",
            "code_challenge_method=S256",
            "state=st8",
            "prompt=select_account",
            // The redirect URI and scope must be *encoded*: a raw `:` or `/` is
            // tolerated by many servers but a raw space is not, and asserting the
            // encoded form pins both rules at once.
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A5000",
            "scope=XboxLive.signin%20offline_access",
        ] {
            assert!(url.contains(expected), "{expected} missing from {url}");
        }
        // The space must never become `+` in a query value here.
        assert!(!url.contains('+'), "a `+` reached the URL: {url}");
    }

    #[test]
    fn a_real_redirect_line_yields_its_code_and_state() {
        let request = "GET /?code=M.C123_abc&state=xyz HTTP/1.1\r\n\
                       Host: 127.0.0.1:5000\r\n\
                       User-Agent: Mozilla/5.0\r\n\r\n";
        assert_eq!(
            parse_redirect_request(request).unwrap(),
            RedirectOutcome::Code {
                code: "M.C123_abc".to_owned(),
                state: "xyz".to_owned(),
            }
        );
    }

    /// Percent-escapes must survive the round trip. A real Microsoft code is
    /// base64url-ish but the `state` and error text are not, so this is not
    /// hypothetical.
    #[test]
    fn escaped_values_are_decoded() {
        let request = "GET /?code=a%2Fb%2Bc&state=s%20t HTTP/1.1\r\n\r\n";
        assert_eq!(
            parse_redirect_request(request).unwrap(),
            RedirectOutcome::Code {
                code: "a/b+c".to_owned(),
                state: "s t".to_owned(),
            }
        );
    }

    #[test]
    fn a_cancelled_sign_in_is_an_error_outcome_not_a_parse_failure() {
        let request = "GET /?error=access_denied&error_description=The+user+cancelled \
                       HTTP/1.1\r\n\r\n";
        assert_eq!(
            parse_redirect_request(request).unwrap(),
            RedirectOutcome::Error {
                error: "access_denied".to_owned(),
                description: Some("The user cancelled".to_owned()),
            }
        );
    }

    /// A browser will happily ask for `/favicon.ico` on the same port. That is
    /// neither a code nor an error and must not be mistaken for either.
    #[test]
    fn an_unrelated_request_is_rejected_rather_than_read_as_a_code() {
        for line in [
            "GET /favicon.ico HTTP/1.1\r\n\r\n",
            "GET / HTTP/1.1\r\n\r\n",
            "GET /?state=only HTTP/1.1\r\n\r\n",
            "garbage",
        ] {
            assert!(
                parse_redirect_request(line).is_err(),
                "{line:?} should not parse as a redirect"
            );
        }
    }

    /// Only the first line is consulted — asserted by putting a decoy `code` in a
    /// later header, which a whole-body scan would pick up.
    #[test]
    fn only_the_request_line_is_read() {
        let request = "GET /?code=real&state=s HTTP/1.1\r\n\
                       X-Decoy: code=fake\r\n\r\n";
        match parse_redirect_request(request).unwrap() {
            RedirectOutcome::Code { code, .. } => assert_eq!(code, "real"),
            other => panic!("expected a code, got {other:?}"),
        }
    }

    /// The listener must be loopback-only. Binding `0.0.0.0` would expose the
    /// authorization code to anything on the network, and the assertion is cheap.
    #[tokio::test]
    async fn begin_binds_loopback_only_and_puts_that_port_in_the_redirect_uri() {
        let login = LoopbackLogin::begin("cid").await.unwrap();
        let addr = login.listener.local_addr().unwrap();
        assert!(addr.ip().is_loopback(), "bound a non-loopback address {addr}");
        assert_ne!(addr.port(), 0, "the OS must have assigned a real port");
        // The *string* must say `localhost` even though the *socket* is bound to
        // 127.0.0.1 — Azure matches the host textually and only port-wildcards
        // `localhost`. This assertion is the one that would have caught the
        // mismatch that reached a player as `invalid_request` on `redirect_uri`:
        // it pinned `127.0.0.1`, agreeing with the code and disagreeing with the
        // registration the module docs told the user to create. A test can only
        // catch that if it knows which of the two is authoritative, and Azure is.
        assert_eq!(login.redirect_uri, format!("http://localhost:{}", addr.port()));
        assert!(
            login.authorize_url().contains(&percent_encode(&login.redirect_uri)),
            "the authorize URL must carry the encoded redirect URI it bound"
        );
        assert!(!login.is_expired(), "a fresh login cannot be expired");
    }

    /// No browser has connected, so a poll must report "nothing yet" rather than
    /// blocking. This is what makes the flow drivable from a frame timer, and a
    /// regression here would hang the game rather than fail a test — so the test
    /// carries its own timeout.
    #[tokio::test]
    async fn poll_once_returns_none_without_blocking_when_no_browser_has_connected() {
        let mut login = LoopbackLogin::begin("cid").await.unwrap();
        // `Client::new()` panics without an installed rustls crypto
        // provider, so this line doubles as a provider canary.
        crate::install_crypto_provider();
        let http = reqwest::Client::new();
        let polled = tokio::time::timeout(
            Duration::from_secs(2),
            login.poll_once(&http, "cid"),
        )
        .await
        .expect("poll_once blocked; it must be non-blocking");
        assert_eq!(polled.unwrap(), None);
    }

    /// A redirect whose `state` does not match is refused, and the *matching*
    /// case is the control: without it, "mismatched state fails" would also pass
    /// on a build where every redirect fails.
    #[tokio::test]
    async fn a_mismatched_state_is_refused_and_a_matching_one_gets_past_the_check() {
        let login = LoopbackLogin::begin("cid").await.unwrap();

        // Drive the pure predicate the same way `poll_once` does, since exchanging
        // a code needs Microsoft.
        let forged = parse_redirect_request("GET /?code=c&state=not-ours HTTP/1.1\r\n\r\n").unwrap();
        match forged {
            RedirectOutcome::Code { state, .. } => {
                assert_ne!(state, login.state, "a forged state must not match");
            }
            other => panic!("expected a code, got {other:?}"),
        }

        let genuine = format!("GET /?code=c&state={} HTTP/1.1\r\n\r\n", login.state);
        match parse_redirect_request(&genuine).unwrap() {
            RedirectOutcome::Code { state, .. } => {
                assert_eq!(state, login.state, "the control failed: our own state must match");
            }
            other => panic!("expected a code, got {other:?}"),
        }
    }
}
