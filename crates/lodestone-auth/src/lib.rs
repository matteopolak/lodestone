//! Online-mode authentication for Lodestone.
//!
//! This crate turns a Microsoft account into everything the encryption
//! handshake needs to join an online-mode vanilla server:
//!
//! * [`server_hash`] — the non-standard SHA-1 the client sends to the session
//!   server (pure, fully tested against Mojang's published vectors);
//! * the Microsoft device-code OAuth flow and the Xbox Live → XSTS → Minecraft
//!   services token chain ([`flow`]);
//! * the session-server [`join_server`] call that proves ownership of the shared
//!   secret, and its server-side mirror [`has_joined`], which a *hosting* server
//!   uses to check that a connecting client really made that call;
//! * multi-account storage (native only): [`store`] holds the
//!   long-lived Microsoft **refresh** token in the real OS keychain, keyed by
//!   profile UUID; [`metadata`] holds everything else (username, profile UUID,
//!   skin URL, last-used time, which account is selected) in a plain JSON
//!   file so an account switcher can draw its list without unlocking the
//!   keychain; [`cache`]/[`migrate`] carry the legacy plaintext cache forward
//!   into the keychain exactly once, then delete it.
//!
//! The actual cipher (AES-128-CFB8), shared-secret generation and RSA wrapping
//! of the secret live in `lodestone-net`, because they sit in the sans-IO codec
//! so every transport (including the browser) inherits them. This crate is
//! purely the *identity* half: who you are and how the session server is told.
//!
//! ## Design constraint: credentials never touch this process
//!
//! Sign-in happens on Microsoft's own page in the user's browser (the
//! device-code flow in [`flow`]) — nothing here accepts a username or
//! password, and nothing should ever be added that does. Every store in this
//! crate holds **tokens only**.
//!
//! ## What is and isn't verified
//!
//! [`server_hash`] is checked against externally-reproduced vectors. The token
//! chain cannot be exercised without a real Microsoft account, so it is written
//! to the documented protocol but is unverified end-to-end; its tests cover only
//! the JSON shapes it parses. [`store`]'s keychain backend *is* verified against
//! the real OS keychain, but only by an `#[ignore]`d test — run explicitly, see
//! that module's docs — since it must not run unattended in every CI environment.

mod error;
mod hash;

pub use error::{AuthError, Result};
pub use hash::server_hash;

/// The loopback authorization-code sign-in — the real login page in the user's
/// browser, with no code to type. The other front end onto the same
/// [`flow`]-shaped token; see that module's docs for why loopback rather than an
/// embedded webview.
///
/// Native-only, unlike [`flow`] below: it binds a `127.0.0.1` listener and
/// launches the platform's browser, neither of which a wasm32 build can do
/// (it *is* the browser). [`flow`]'s device-code flow is the wasm32 front end
/// onto the identical downstream token chain.
#[cfg(not(target_arch = "wasm32"))]
pub mod browser_login;
#[cfg(not(target_arch = "wasm32"))]
pub mod cache;
/// Secure chat: fetching the Mojang-issued RSA chat-session key pair and
/// signing outgoing messages with it. Native-only — it makes an HTTPS call to
/// `api.minecraftservices.com` with the native-only `rsa`/`sha2` chain (see
/// this crate's `Cargo.toml`) — see that module's doc comment for the full
/// citation of what was and was not possible to verify against an outside
/// source.
#[cfg(not(target_arch = "wasm32"))]
pub mod chat_session;
/// The Microsoft device-code OAuth flow and the Xbox Live -> XSTS ->
/// Minecraft-services token chain.
///
/// **Not gated**, unlike this crate's other network modules. The device-code
/// flow needs no listener and no OS keychain — it shows a short code and
/// polls — so it is the one sign-in front end that ports to a browser: see
/// `crate::browser_login`'s doc for why the *loopback* flow specifically
/// cannot. Everything from [`flow::MsToken`] onward (Xbox Live, XSTS, the
/// Minecraft-services login and profile fetch) is the same code path on both
/// targets, over the same `reqwest::Client` — see this crate's `Cargo.toml`
/// for why `reqwest` itself is not native-only either. Only
/// [`flow::PendingLogin::wait`] (which blocks the calling task on
/// `tokio::time::sleep`, `Instant`-only) stays native; a wasm32 caller drives
/// [`flow::PendingLogin::poll_once`] from its own browser-clock timer instead
/// — `crates/lodestone-shell/src/menu/accounts.rs` is that caller.
pub mod flow;
/// Typed operations for the account-scoped Friends service.
///
/// The module accepts an already-resolved [`Session`] and never opens account
/// storage or refreshes an access token. Its fixed production origin and
/// redirect-free HTTP client keep that bearer token inside the trusted service
/// boundary; see [`friends`] for the request and response contract.
pub mod friends;
/// Composing [`flow`] + [`store`] + [`metadata`] into an actual login.
///
/// Not gated, for [`flow`]'s own reason: every piece it composes now has a
/// wasm32 arm (`store::LocalStorageStore` in place of the OS keychain), so
/// the composition itself needs no fork — a browser sign-in and a native one
/// call the exact same [`finish_interactive`]/[`try_cached_session`].
pub mod login;
/// The ownership proof every play path in the shell must be handed: a token
/// that can only be produced from a roster holding a real account.
///
/// Not `cfg`-gated, for [`metadata`]'s reason — it is a pure function of a
/// roster, with no HTTP client and no keychain, and the browser build needs the
/// same gate the native one has.
pub mod entitlement;
/// The account roster (`profiles.json`): which accounts exist, their display
/// names, and which one is selected.
///
/// **Not `cfg`-gated, unlike its neighbours here.** It is `serde` + `uuid` +
/// `std::path` with no HTTP client and no runtime — it was gated only because
/// the whole native block was written as one unit. `lodestone-shell`'s
/// account-switcher screen names [`AccountProfile`] and [`AccountsMetadata`]
/// throughout its UI *state*, so gating the types (rather than the sign-in flow
/// that needs a network) forced the browser build of that screen to fail with 27
/// errors for want of two plain structs.
///
/// **`std::fs::read_to_string`/`std::fs::write` always fail on wasm32**
/// (`Err(Unsupported)`, measured, not assumed) — so `load_from`/`save_to`'s
/// native bodies alone left the browser roster permanently empty on every
/// launch: an account could sign in but a reload could never see it again,
/// which is a real regression, not the "correct browser answer" an earlier
/// version of this doc claimed. Both functions now carry a `wasm32` arm that
/// reaches `localStorage` instead, keyed by the same [`std::path::Path`] the
/// native arm would open — see [`metadata::AccountsMetadata::load_from`].
pub mod metadata;
#[cfg(not(target_arch = "wasm32"))]
pub mod migrate;
/// Where account, options and server-list files live.
///
/// Not gated, for the same reason [`metadata`] is not: `PathBuf` construction over
/// `std::env::var_os`, which on wasm32 simply yields `None` for every variable and
/// falls through to the same default it would on a machine with no `HOME`.
pub mod paths;
/// Where the Microsoft refresh token, and the session derived from it,
/// actually live.
///
/// **Not gated.** [`store::SecretStore`], [`store::CachedSession`] and
/// [`store::MemoryStore`] have no keychain dependency and never did; only
/// [`store::KeychainStore`] (the real OS-keychain backend) is native-only, and
/// [`store::LocalStorageStore`] is its wasm32 sibling — see that module's docs
/// for why a browser's `localStorage` is a strictly weaker protection than an
/// OS keychain, not a drop-in equivalent of one.
pub mod store;
/// Fetching a skin/cape texture under the same allowed-host restriction the
/// vanilla client enforces for texture URLs. Native-only — unlike [`flow`],
/// nothing here has a wasm32 caller yet (`lodestone-shell`'s own skin fetch
/// is a separate, unrelated module).
#[cfg(not(target_arch = "wasm32"))]
pub mod texture;
/// The process-default rustls crypto provider, needed because `reqwest`'s
/// `rustls` feature requires the application to select one explicitly (see
/// `crate::tls`). Native-only, for the same reason as the modules above:
/// reqwest's rustls stack is itself `cfg(not(wasm32))`, so a browser build
/// has no provider to select.
#[cfg(not(target_arch = "wasm32"))]
pub mod tls;

#[cfg(not(target_arch = "wasm32"))]
pub use tls::install_crypto_provider;

#[cfg(not(target_arch = "wasm32"))]
pub use chat_session::{
    ChatKeyPair, ChatSession, LAST_SEEN_MAX_LEN, MOJANG_PUBLIC_KEYS_URL,
    MOJANG_PUBLIC_KEY_FAILURE_BACKOFF_BASE_MILLIS, MOJANG_PUBLIC_KEY_REFRESH_MILLIS,
    MojangPublicKeyCache, MojangPublicKeys, ProfilePublicKeyData, SIGNATURE_BYTES,
    SignedMessageLink, build_signature_payload, fetch_key_pair, fetch_mojang_public_keys,
    profile_public_key_has_expired, profile_public_key_signature_payload, verify_signature,
};

pub use entitlement::Entitlement;
pub use error::XstsErrorKind;
// `authenticate_with_device_code` is omitted here deliberately: it drives
// `PendingLogin::wait`, which is native-only (see `flow`'s module doc). Every
// other name compiles and is meaningful on both targets.
pub use flow::{
    DeviceCodePrompt, HasJoinedProfile, HasJoinedProperty, MOJANG_CLIENT_ID, MsToken,
    PendingLogin, Profile, ProfileSkin, Session, SkinVariant, has_joined, join_server, poll_token,
    refresh_token, request_device_code, session_from_ms_token,
};
#[cfg(not(target_arch = "wasm32"))]
pub use flow::authenticate_with_device_code;
pub use login::{
    CachedSessionOutcome, SelectedAccount, finish_interactive, resolve_client_id,
    resolve_selected_account, resolve_selected_account_with, try_cached_session,
};
pub use metadata::{AccountProfile, AccountsMetadata};
#[cfg(not(target_arch = "wasm32"))]
pub use store::KeychainStore;
#[cfg(target_arch = "wasm32")]
pub use store::LocalStorageStore;
pub use store::{AccountSecrets, MemoryStore, SecretStore, StorageMode};
#[cfg(not(target_arch = "wasm32"))]
pub use texture::{
    ALLOWED_TEXTURE_DOMAIN, ALLOWED_TEXTURE_SCHEMES, MAX_TEXTURE_BYTES, fetch_texture,
    is_allowed_texture_domain,
};
