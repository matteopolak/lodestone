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
//! * multi-account storage (issue #64, native only): [`store`] holds the
//!   long-lived Microsoft **refresh** token in the real OS keychain, keyed by
//!   profile UUID; [`metadata`] holds everything else (username, profile UUID,
//!   skin URL, last-used time, which account is selected) in a plain JSON
//!   file so an account switcher can draw its list without unlocking the
//!   keychain; [`cache`]/[`migrate`] carry a pre-#64 plaintext cache forward
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
#[cfg(not(target_arch = "wasm32"))]
pub mod browser_login;
#[cfg(not(target_arch = "wasm32"))]
pub mod cache;
/// Secure chat: fetching the Mojang-issued RSA chat-session key pair and
/// signing outgoing messages with it. Native-only for the same reason
/// [`flow`] is — it makes an HTTPS call to `api.minecraftservices.com` — see
/// that module's doc comment for the full citation of what was and was not
/// possible to verify against an outside source.
#[cfg(not(target_arch = "wasm32"))]
pub mod chat_session;
#[cfg(not(target_arch = "wasm32"))]
pub mod flow;
#[cfg(not(target_arch = "wasm32"))]
pub mod login;
/// The account roster (`profiles.json`): which accounts exist, their display
/// names, and which one is selected.
///
/// **Not `cfg`-gated, unlike its neighbours here.** It is `serde` + `uuid` +
/// `std::path` with no HTTP client, no keychain and no runtime — it was gated only
/// because the whole native block was written as one unit. `lodestone-shell`'s
/// account-switcher screen names [`AccountProfile`] and [`AccountsMetadata`]
/// throughout its UI *state*, so gating the types (rather than the sign-in flow
/// that needs a network) forced the browser build of that screen to fail with 27
/// errors for want of two plain structs. Its `std::fs` load/save degrade to
/// `Err(Unsupported)` on wasm32 — measured, not assumed — which surfaces as an
/// empty roster: the correct browser answer, since there is no Microsoft sign-in
/// there to populate one.
pub mod metadata;
#[cfg(not(target_arch = "wasm32"))]
pub mod migrate;
/// Where account, options and server-list files live.
///
/// Not gated, for the same reason [`metadata`] is not: `PathBuf` construction over
/// `std::env::var_os`, which on wasm32 simply yields `None` for every variable and
/// falls through to the same default it would on a machine with no `HOME`.
pub mod paths;
#[cfg(not(target_arch = "wasm32"))]
pub mod store;
/// Fetching a skin/cape texture under authlib's own `TextureUrlChecker` host
/// restriction (issue #62). Native-only, as [`flow`] is and for the same reason.
#[cfg(not(target_arch = "wasm32"))]
pub mod texture;
/// The process-default rustls crypto provider (issue #446). Native-only, for the
/// same reason as the modules above: reqwest's rustls stack is itself
/// `cfg(not(wasm32))`, so a browser build has no provider to select.
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

pub use error::XstsErrorKind;
#[cfg(not(target_arch = "wasm32"))]
pub use flow::{
    DeviceCodePrompt, HasJoinedProfile, HasJoinedProperty, MOJANG_CLIENT_ID, MsToken,
    PendingLogin, Profile, ProfileSkin, Session, SkinVariant, authenticate_with_device_code,
    has_joined, join_server, poll_token, refresh_token, request_device_code,
    session_from_ms_token,
};
#[cfg(not(target_arch = "wasm32"))]
pub use login::{
    CachedSessionOutcome, SelectedAccount, finish_interactive, resolve_client_id,
    resolve_selected_account, resolve_selected_account_with, try_cached_session,
};
pub use metadata::{AccountProfile, AccountsMetadata};
#[cfg(not(target_arch = "wasm32"))]
pub use store::{AccountSecrets, KeychainStore, MemoryStore, SecretStore, StorageMode};
#[cfg(not(target_arch = "wasm32"))]
pub use texture::{
    ALLOWED_TEXTURE_DOMAIN, ALLOWED_TEXTURE_SCHEMES, MAX_TEXTURE_BYTES, fetch_texture,
    is_allowed_texture_domain,
};
