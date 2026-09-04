//! Where the Microsoft **refresh** token, and the session derived from it,
//! actually live (extended here to include the session cache below).
//!
//! The Minecraft services access token is short-lived (its real lifetime
//! comes back with it — see [`crate::flow::Session::expires_at`] — typically,
//! but not assumed to always be, ~24h) and was previously re-derived from the
//! refresh token via [`crate::flow::refresh_token`] +
//! [`crate::flow::session_from_ms_token`] on *every* join, never persisted.
//! It now is: [`CachedSession`] holds it (plus the profile it belongs to),
//! keyed the same way the refresh token is, so a join with a still-valid
//! cache skips that whole chain — and the refresh-token redemption that used
//! to gate it — entirely. Both go into the real OS credential store: macOS
//! Keychain, Windows Credential Manager, or a Linux Secret Service keyring,
//! all behind the `keyring` crate's `Entry` type, under two separate
//! services ([`KEYCHAIN_SERVICE`] and [`SESSION_KEYCHAIN_SERVICE`]) so either
//! can be cleared independently.
//!
//! Entries are keyed by **profile UUID** (`docs/accounts.md`), one credential
//! per Microsoft account, so multiple accounts coexist — the whole point of
//! this crate no longer having a single fixed `ms_token.json`.
//!
//! ## The trait seam
//!
//! [`SecretStore`] is the storage contract. [`KeychainStore`] is the real
//! backend; [`MemoryStore`] is an in-memory implementation used both by
//! hermetic tests (no test should require a real keychain to pass in CI or on
//! a colleague's machine) and as the automatic fallback when the real
//! keychain cannot be reached at all.
//!
//! [`AccountSecrets`] is the façade callers actually hold: it picks a backend
//! once, at construction, and remembers *why* if it had to fall back — see
//! [`StorageMode`]. That decision is deliberately made once and remembered
//! rather than re-probed on every call: `keyring`'s own `Entry::new` latches
//! its backend-initialization failure process-wide on the first attempt (see
//! `keyring::v1`'s `SET_CREDENTIAL_STORE` atomic), so retrying the real
//! backend after it has already failed once in this process cannot succeed
//! anyway.
//!
//! ## What is *not* built here
//!
//! The brief for this work offered two options for the "keychain is
//! unavailable" fallback: a session-only in-memory mode, or an
//! explicitly-encrypted-at-rest file. Only the former is implemented.
//! An at-rest-encrypted fallback needs a key from somewhere, and the only
//! sources available without inventing new scope are (a) a user-supplied
//! passphrase — which starts to blur the "never accept a password" design
//! constraint even though it would not be the Microsoft account password, and
//! adds real key-derivation/salt/nonce-management surface with no UI yet to
//! collect it (the shell is held) — or (b) a machine-local key stored
//! alongside the ciphertext, which is obfuscation, not protection, since
//! reading the disk gets an attacker both. Session-only was judged the
//! honest choice: it fails loudly (the account has to be re-added) rather
//! than pretending to protect a secret it cannot actually protect.
//!
//! ## The wasm32 backend: `localStorage`, and why it is not "the keychain, but browser"
//!
//! [`LocalStorageStore`] is a real [`SecretStore`] for the browser build, not
//! a stub — a browser account round-trips a refresh token exactly like a
//! native one. But it is **not equivalent protection**, and code that meets a
//! stored token in a browser context should not imply otherwise:
//!
//! * an OS keychain entry is access-controlled by the OS (macOS Keychain
//!   prompts, Secret Service is per-user-session); `localStorage` is readable
//!   by any JavaScript that runs on the same origin — any XSS on this page,
//!   or a browser extension with page access, reads it in plaintext;
//! * it has no encryption of its own — the browser stores it as a plain
//!   string on disk;
//! * it survives a tab close but not "clear site data"/private-mode
//!   teardown, and is scoped per-origin rather than per-OS-user.
//!
//! This is why [`StorageMode::BrowserLocalStorage`] is its own variant rather
//! than reusing [`StorageMode::Keychain`]'s label: a UI surfacing
//! [`AccountSecrets::mode`] must be able to say which protection an account
//! actually has, not report "keychain" for a token sitting in
//! `localStorage`.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Result;

/// The keychain "service" every refresh-token entry is grouped under; the
/// per-entry "account" (in Keychain Access's terminology) is the profile
/// UUID. Using a reverse-DNS-shaped constant, rather than a bare name like
/// `"lodestone"`, keeps this from colliding with an unrelated app's entries
/// on a shared Secret Service keyring.
const KEYCHAIN_SERVICE: &str = "dev.lodestone.ms-refresh-token";

/// The keychain "service" cached-session entries are grouped under —
/// deliberately a separate service from [`KEYCHAIN_SERVICE`] rather than a
/// second entry under the same one, so the refresh token and the derived
/// session can be cleared independently (e.g. a corrupt cached session must
/// not take the refresh token down with it when deleted).
const SESSION_KEYCHAIN_SERVICE: &str = "dev.lodestone.mc-session";

/// A cached, already-derived Minecraft session — the product of
/// `crate::login::try_cached_session`'s XBL -> XSTS -> Minecraft-services ->
/// profile chain, persisted so a join with a still-valid cache can skip that
/// whole chain, *and* the refresh-token redemption that gates it.
///
/// `access_token` is a bearer credential exactly like the Microsoft refresh
/// token it is derived from, so it lives in the same class of storage (the OS
/// keychain via [`KeychainStore`]/[`AccountSecrets`]) rather than a plaintext
/// file — see `crate::cache`'s module doc for why that plaintext path is
/// legacy and must not be resurrected for this either.
///
/// The profile fields duplicate what `crate::metadata::AccountsMetadata`
/// already knows, on purpose: a cache hit reconstructs a full
/// `crate::flow::Session` ([`Self::to_session`]) without depending on
/// `profiles.json` staying in sync, or existing at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedSession {
    /// The Minecraft services access token.
    pub access_token: String,
    /// Unix timestamp (seconds) this token stops being valid — copied
    /// straight from [`crate::flow::Session::expires_at`], which is itself
    /// read from Mojang's real response, never invented here.
    pub expires_at: u64,
    /// [`crate::flow::Profile::id`], stored as text: `keyring`'s backend and
    /// `serde_json` both want a plain string, and round-tripping through
    /// `Uuid`'s `Display`/`FromStr` needs no extra dependency on this crate's
    /// `uuid` feature set (which does not enable `serde`).
    pub profile_id: String,
    /// [`crate::flow::Profile::name`].
    pub profile_name: String,
    /// [`crate::flow::ProfileSkin::url`], if the profile has an active skin.
    pub skin_url: Option<String>,
    /// `"Classic"` or `"Slim"` — [`crate::flow::SkinVariant`] spelled as a
    /// plain string for the same reason `profile_id` is: kept independent of
    /// `serde` support on `flow`'s own types, which exist for the network
    /// wire, not for this cache.
    pub skin_variant: Option<String>,
}

impl CachedSession {
    /// Captures a derived [`crate::flow::Session`] as a storable value.
    #[must_use]
    pub fn from_session(session: &crate::flow::Session) -> Self {
        Self {
            access_token: session.access_token.clone(),
            expires_at: session.expires_at,
            profile_id: session.profile.id.to_string(),
            profile_name: session.profile.name.clone(),
            skin_url: session.profile.skin.as_ref().map(|s| s.url.clone()),
            skin_variant: session
                .profile
                .skin
                .as_ref()
                .map(|s| skin_variant_tag(s.variant).to_owned()),
        }
    }

    /// The inverse of [`Self::from_session`]. `None` only if `profile_id` is
    /// not a valid UUID — corruption to be treated as "no usable cache",
    /// never a panic; a caller degrades to redoing the full sign-in chain.
    #[must_use]
    pub fn to_session(&self) -> Option<crate::flow::Session> {
        let id = Uuid::parse_str(&self.profile_id).ok()?;
        let skin = self.skin_url.clone().map(|url| crate::flow::ProfileSkin {
            url,
            variant: self
                .skin_variant
                .as_deref()
                .map(skin_variant_from_tag)
                .unwrap_or(crate::flow::SkinVariant::Classic),
        });
        Some(crate::flow::Session {
            access_token: self.access_token.clone(),
            profile: crate::flow::Profile {
                name: self.profile_name.clone(),
                id,
                skin,
            },
            expires_at: self.expires_at,
        })
    }
}

fn skin_variant_tag(variant: crate::flow::SkinVariant) -> &'static str {
    match variant {
        crate::flow::SkinVariant::Classic => "Classic",
        crate::flow::SkinVariant::Slim => "Slim",
    }
}

fn skin_variant_from_tag(tag: &str) -> crate::flow::SkinVariant {
    if tag == "Slim" {
        crate::flow::SkinVariant::Slim
    } else {
        crate::flow::SkinVariant::Classic
    }
}

/// A place refresh tokens are kept, keyed by profile UUID. Behind a trait so
/// hermetic tests exercise [`MemoryStore`] and never require a real OS
/// keychain to run — only the `#[ignore]`d test in this module touches the
/// genuine backend.
pub trait SecretStore: std::fmt::Debug + Send + Sync {
    /// Stores (overwriting any existing) refresh token for `profile`.
    ///
    /// # Errors
    /// Returns [`crate::AuthError`] if the underlying store rejects the
    /// write (locked, denied, or a platform failure).
    fn save_refresh_token(&self, profile: Uuid, token: &str) -> Result<()>;

    /// Loads the refresh token for `profile`, or `Ok(None)` if there is none.
    ///
    /// # Errors
    /// Returns [`crate::AuthError`] for any failure other than "no entry".
    fn load_refresh_token(&self, profile: Uuid) -> Result<Option<String>>;

    /// Deletes the refresh token for `profile`. Idempotent: deleting an
    /// absent entry is not an error.
    ///
    /// # Errors
    /// Returns [`crate::AuthError`] for any failure other than "no entry".
    fn delete_refresh_token(&self, profile: Uuid) -> Result<()>;

    /// Stores (overwriting any existing) cached session for `profile`.
    ///
    /// # Errors
    /// Returns [`crate::AuthError`] if the underlying store rejects the
    /// write (locked, denied, or a platform failure).
    fn save_session(&self, profile: Uuid, session: &CachedSession) -> Result<()>;

    /// Loads the cached session for `profile`. `Ok(None)` covers both "no
    /// entry" and "the stored value could not be parsed" — a corrupt cache
    /// entry must degrade to "there is no usable cache" (and the caller
    /// re-running the full sign-in chain), never propagate as an error that
    /// could block a join outright. A backend that treats the two
    /// differently should log the parse failure itself before returning
    /// `Ok(None)`, so the degradation is still visible in the log even
    /// though it is not visible to the caller as an `Err`.
    ///
    /// # Errors
    /// Returns [`crate::AuthError`] for a transport/platform failure reaching
    /// the store at all (locked, denied) — never for a parse failure once the
    /// entry was read.
    fn load_session(&self, profile: Uuid) -> Result<Option<CachedSession>>;

    /// Deletes the cached session for `profile`. Idempotent.
    ///
    /// # Errors
    /// Returns [`crate::AuthError`] for any failure other than "no entry".
    fn delete_session(&self, profile: Uuid) -> Result<()>;
}

/// How [`AccountSecrets`] is actually protecting tokens right now. Exists so
/// a caller (the account-switcher UI) can say so — a fallback the user
/// cannot see is exactly the "silent leave-behind" this work exists to
/// avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageMode {
    /// Tokens are held in the real OS keychain (macOS Keychain, Windows
    /// Credential Manager, or a Secret-Service-backed Linux keyring).
    Keychain,
    /// The OS keychain could not be reached (no backend, locked, denied, or a
    /// headless/CI environment with no keyring daemon at all). Tokens live
    /// only in this process's memory for this run and are lost on exit —
    /// deliberately never a silent fallback to a plaintext file. `reason` is
    /// the underlying platform error, for a diagnostic message.
    SessionOnly {
        /// Why the real keychain was not used.
        reason: String,
    },
    /// Tokens are held in the browser's `localStorage` — [`LocalStorageStore`],
    /// the wasm32 backend. **Weaker than [`Self::Keychain`], not an
    /// equivalent**: see this module's doc for why, and never render this the
    /// same way `Keychain` is rendered.
    BrowserLocalStorage,
}

impl std::fmt::Display for StorageMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keychain => write!(f, "OS keychain"),
            Self::SessionOnly { reason } => write!(
                f,
                "session-only (in-memory, lost on exit; OS keychain unavailable: {reason})"
            ),
            Self::BrowserLocalStorage => write!(
                f,
                "browser local storage (weaker than an OS keychain: readable by any script \
                 on this page; cleared if you clear this site's data)"
            ),
        }
    }
}

/// In-memory [`SecretStore`]. Used both for hermetic tests and as the
/// automatic fallback backend when the real keychain cannot be reached.
///
/// Two independent maps, matching [`KeychainStore`]'s two separate keychain
/// services: a refresh token and a cached session for the same profile can be
/// present, absent, or cleared independently.
#[derive(Debug, Default)]
pub struct MemoryStore {
    refresh_tokens: Mutex<HashMap<Uuid, String>>,
    sessions: Mutex<HashMap<Uuid, CachedSession>>,
}

impl MemoryStore {
    /// Creates an empty in-memory store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn refresh_tokens(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, String>> {
        // A poisoned lock (a panic while held) still holds perfectly good
        // data; recovering it is strictly better than a second panic here.
        self.refresh_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, CachedSession>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SecretStore for MemoryStore {
    fn save_refresh_token(&self, profile: Uuid, token: &str) -> Result<()> {
        self.refresh_tokens().insert(profile, token.to_owned());
        Ok(())
    }

    fn load_refresh_token(&self, profile: Uuid) -> Result<Option<String>> {
        Ok(self.refresh_tokens().get(&profile).cloned())
    }

    fn delete_refresh_token(&self, profile: Uuid) -> Result<()> {
        self.refresh_tokens().remove(&profile);
        Ok(())
    }

    fn save_session(&self, profile: Uuid, session: &CachedSession) -> Result<()> {
        self.sessions().insert(profile, session.clone());
        Ok(())
    }

    fn load_session(&self, profile: Uuid) -> Result<Option<CachedSession>> {
        Ok(self.sessions().get(&profile).cloned())
    }

    fn delete_session(&self, profile: Uuid) -> Result<()> {
        self.sessions().remove(&profile);
        Ok(())
    }
}

/// The real OS-keychain-backed [`SecretStore`]. Holds no state of its own —
/// every call builds a fresh `keyring::Entry` for the profile it is given —
/// so it is trivially `Send + Sync` and cheap to construct.
///
/// Native-only: no wasm32 keychain backend exists. [`LocalStorageStore`]
/// below is the wasm32 sibling — a real `SecretStore`, but a strictly weaker
/// one; see this module's doc for why the two are not interchangeable labels.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainStore;

#[cfg(not(target_arch = "wasm32"))]
impl KeychainStore {
    fn entry(profile: Uuid) -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(KEYCHAIN_SERVICE, &profile.to_string())
    }

    fn session_entry(profile: Uuid) -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(SESSION_KEYCHAIN_SERVICE, &profile.to_string())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SecretStore for KeychainStore {
    fn save_refresh_token(&self, profile: Uuid, token: &str) -> Result<()> {
        Self::entry(profile)?.set_password(token)?;
        Ok(())
    }

    fn load_refresh_token(&self, profile: Uuid) -> Result<Option<String>> {
        match Self::entry(profile)?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn delete_refresh_token(&self, profile: Uuid) -> Result<()> {
        match Self::entry(profile)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn save_session(&self, profile: Uuid, session: &CachedSession) -> Result<()> {
        // A JSON-serialise failure here would mean `CachedSession` itself
        // cannot round-trip, which is a programmer error in this crate, not a
        // storage failure — `unwrap_or_default` would silently write `"{}"`
        // and the next read would fail to parse it back into a profile id,
        // degrading exactly as a genuinely corrupt entry would. There is no
        // externally-triggerable way to reach this arm.
        let text = serde_json::to_string(session)?;
        Self::session_entry(profile)?.set_password(&text)?;
        Ok(())
    }

    fn load_session(&self, profile: Uuid) -> Result<Option<CachedSession>> {
        match Self::session_entry(profile)?.get_password() {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(session) => Ok(Some(session)),
                Err(e) => {
                    // Degrade to "no usable cache" rather than propagating —
                    // see the trait doc — but log so the degradation is
                    // visible rather than a silent extra round trip nobody
                    // can explain.
                    tracing::warn!(
                        target: "auth",
                        profile = %profile,
                        error = %e,
                        "cached session for this profile could not be parsed; treating as absent"
                    );
                    Ok(None)
                }
            },
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn delete_session(&self, profile: Uuid) -> Result<()> {
        match Self::session_entry(profile)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Probes whether the real keychain backend is actually reachable, without
/// requiring (or leaving behind) any real entry.
///
/// Constructing an `Entry` and asking for a password that was never set
/// distinguishes "the backend answered: no such entry" (`Ok`) from "the
/// backend could not be reached at all" (`Err`) — e.g. no D-Bus Secret
/// Service session on headless Linux, which is exactly the case the brief
/// asks to degrade gracefully rather than panic or fall back to plaintext.
#[cfg(not(target_arch = "wasm32"))]
fn probe_keychain() -> std::result::Result<(), String> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, "lodestone-availability-probe")
        .map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// The wasm32 [`SecretStore`]: the browser's `localStorage`, in place of the
/// OS keychain — see this module's doc for why this is a real backend, not a
/// stub, and why it is not equivalent protection. Holds no state of its own,
/// like [`KeychainStore`]: every call reaches `window.localStorage` fresh.
///
/// Keyed under the *same* two service strings [`KeychainStore`] uses
/// ([`KEYCHAIN_SERVICE`]/[`SESSION_KEYCHAIN_SERVICE`]), joined with the
/// profile UUID — there is no cross-origin collision risk to defend against
/// here the way there might be on a shared multi-app keyring, since a
/// browser's `localStorage` is already same-origin-isolated, but reusing the
/// constants keeps one naming scheme rather than two.
#[cfg(target_arch = "wasm32")]
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalStorageStore;

#[cfg(target_arch = "wasm32")]
impl LocalStorageStore {
    /// The live `localStorage` handle, or a typed error if this page has none
    /// — e.g. `window` itself absent (a non-browser wasm host), or storage
    /// disabled/blocked by the user's browser settings.
    fn storage() -> Result<web_sys::Storage> {
        web_sys::window()
            .ok_or_else(|| crate::AuthError::Storage("no global `window` in this wasm host".to_owned()))?
            .local_storage()
            .map_err(|e| crate::AuthError::Storage(format!("{e:?}")))?
            .ok_or_else(|| {
                crate::AuthError::Storage(
                    "this browser has no `localStorage` (disabled, or blocked by settings)"
                        .to_owned(),
                )
            })
    }

    fn key(service: &str, profile: Uuid) -> String {
        format!("{service}:{profile}")
    }
}

#[cfg(target_arch = "wasm32")]
impl SecretStore for LocalStorageStore {
    fn save_refresh_token(&self, profile: Uuid, token: &str) -> Result<()> {
        Self::storage()?
            .set_item(&Self::key(KEYCHAIN_SERVICE, profile), token)
            .map_err(|e| crate::AuthError::Storage(format!("{e:?}")))
    }

    fn load_refresh_token(&self, profile: Uuid) -> Result<Option<String>> {
        Self::storage()?
            .get_item(&Self::key(KEYCHAIN_SERVICE, profile))
            .map_err(|e| crate::AuthError::Storage(format!("{e:?}")))
    }

    fn delete_refresh_token(&self, profile: Uuid) -> Result<()> {
        Self::storage()?
            .remove_item(&Self::key(KEYCHAIN_SERVICE, profile))
            .map_err(|e| crate::AuthError::Storage(format!("{e:?}")))
    }

    fn save_session(&self, profile: Uuid, session: &CachedSession) -> Result<()> {
        // Same non-triggerable-in-practice failure `KeychainStore::save_session`
        // notes: a serialise failure here means `CachedSession` cannot
        // round-trip at all, a programmer error in this crate rather than a
        // storage failure.
        let text = serde_json::to_string(session)?;
        Self::storage()?
            .set_item(&Self::key(SESSION_KEYCHAIN_SERVICE, profile), &text)
            .map_err(|e| crate::AuthError::Storage(format!("{e:?}")))
    }

    fn load_session(&self, profile: Uuid) -> Result<Option<CachedSession>> {
        let text = Self::storage()?
            .get_item(&Self::key(SESSION_KEYCHAIN_SERVICE, profile))
            .map_err(|e| crate::AuthError::Storage(format!("{e:?}")))?;
        let Some(text) = text else { return Ok(None) };
        match serde_json::from_str(&text) {
            Ok(session) => Ok(Some(session)),
            Err(e) => {
                // Same degrade-and-log rule as `KeychainStore::load_session` —
                // see the trait doc.
                tracing::warn!(
                    target: "auth",
                    profile = %profile,
                    error = %e,
                    "cached session for this profile could not be parsed; treating as absent"
                );
                Ok(None)
            }
        }
    }

    fn delete_session(&self, profile: Uuid) -> Result<()> {
        Self::storage()?
            .remove_item(&Self::key(SESSION_KEYCHAIN_SERVICE, profile))
            .map_err(|e| crate::AuthError::Storage(format!("{e:?}")))
    }
}

/// The façade callers hold: a [`SecretStore`] backend plus the
/// [`StorageMode`] it settled on.
///
/// [`Self::open`] decides the mode once, via [`probe_keychain`], and does not
/// re-probe later — see the module docs for why a later retry cannot help.
/// A caller that observes a mid-session [`crate::AuthError`] from
/// save/load/delete (e.g. the user locks their session, or revokes access)
/// gets that as an explicit error, not a silent mode change; a fresh
/// [`Self::open`] is the way to re-decide.
#[derive(Debug)]
pub struct AccountSecrets {
    backend: Box<dyn SecretStore>,
    mode: StorageMode,
}

impl AccountSecrets {
    /// Opens the real store: tries the OS keychain, and falls back to an
    /// in-memory, session-only store (logging why) if it cannot be reached.
    /// Never falls back to a plaintext file.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn open() -> Self {
        match probe_keychain() {
            Ok(()) => {
                tracing::debug!("OS keychain available; refresh tokens will be stored there");
                Self {
                    backend: Box::new(KeychainStore),
                    mode: StorageMode::Keychain,
                }
            }
            Err(reason) => {
                tracing::warn!(
                    reason = %reason,
                    "OS keychain unavailable; refresh tokens will be kept in memory for this \
                     session only and lost on exit (never falling back to a plaintext file)"
                );
                Self {
                    backend: Box::new(MemoryStore::new()),
                    mode: StorageMode::SessionOnly { reason },
                }
            }
        }
    }

    /// Opens the real store on the wasm32 target: there is no OS keychain to
    /// try at all, so this goes straight to [`LocalStorageStore`] — still
    /// falling back to an in-memory, session-only store if `localStorage`
    /// itself turns out to be unavailable (private-mode restrictions, or a
    /// non-browser wasm host with no `window`), by the same probe-once
    /// principle [`Self::open`]'s native arm uses.
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn open() -> Self {
        // A throwaway key, never left behind: `Storage::get_item` on a key
        // that was never set is `Ok(None)`, so this distinguishes "no
        // localStorage at all" (an `Err` from `LocalStorageStore::storage`)
        // from "localStorage works and this key simply is not in it" without
        // writing anything real.
        match LocalStorageStore::storage() {
            Ok(_) => {
                tracing::debug!("browser localStorage available; refresh tokens will be stored there");
                Self {
                    backend: Box::new(LocalStorageStore),
                    mode: StorageMode::BrowserLocalStorage,
                }
            }
            Err(e) => {
                let reason = e.to_string();
                tracing::warn!(
                    reason = %reason,
                    "browser localStorage unavailable; refresh tokens will be kept in memory \
                     for this tab only and lost on reload"
                );
                Self {
                    backend: Box::new(MemoryStore::new()),
                    mode: StorageMode::SessionOnly { reason },
                }
            }
        }
    }

    /// Builds a façade over an explicit backend and mode — the seam hermetic
    /// tests (and anything that wants to inject its own [`SecretStore`]) use
    /// instead of [`Self::open`].
    #[must_use]
    pub fn with_backend(backend: Box<dyn SecretStore>, mode: StorageMode) -> Self {
        Self { backend, mode }
    }

    /// How tokens are currently being protected. Surface this in the UI
    /// whenever it is not [`StorageMode::Keychain`].
    #[must_use]
    pub fn mode(&self) -> &StorageMode {
        &self.mode
    }

    /// See [`SecretStore::save_refresh_token`].
    ///
    /// # Errors
    /// See [`SecretStore::save_refresh_token`].
    pub fn save_refresh_token(&self, profile: Uuid, token: &str) -> Result<()> {
        self.backend.save_refresh_token(profile, token)
    }

    /// See [`SecretStore::load_refresh_token`].
    ///
    /// # Errors
    /// See [`SecretStore::load_refresh_token`].
    pub fn load_refresh_token(&self, profile: Uuid) -> Result<Option<String>> {
        self.backend.load_refresh_token(profile)
    }

    /// See [`SecretStore::delete_refresh_token`].
    ///
    /// # Errors
    /// See [`SecretStore::delete_refresh_token`].
    pub fn delete_refresh_token(&self, profile: Uuid) -> Result<()> {
        self.backend.delete_refresh_token(profile)
    }

    /// See [`SecretStore::save_session`].
    ///
    /// # Errors
    /// See [`SecretStore::save_session`].
    pub fn save_session(&self, profile: Uuid, session: &CachedSession) -> Result<()> {
        self.backend.save_session(profile, session)
    }

    /// See [`SecretStore::load_session`].
    ///
    /// # Errors
    /// See [`SecretStore::load_session`].
    pub fn load_session(&self, profile: Uuid) -> Result<Option<CachedSession>> {
        self.backend.load_session(profile)
    }

    /// See [`SecretStore::delete_session`].
    ///
    /// # Errors
    /// See [`SecretStore::delete_session`].
    pub fn delete_session(&self, profile: Uuid) -> Result<()> {
        self.backend.delete_session(profile)
    }
}

/// `AccountSecrets` — the façade every real caller actually holds — must
/// implement `SecretStore` so it can be handed to
/// [`crate::login::finish_interactive`] or
/// [`crate::login::try_cached_session`], both of which take
/// `secrets: &dyn SecretStore`. Without this impl, code that only holds an
/// `AccountSecrets` has no way to reach those functions and would have to
/// hand-roll `finish_interactive`'s derive-session-then-save-token
/// composition itself. The body here is identical to the inherent methods
/// above (both simply forward to `self.backend`), so this adds no new
/// behaviour — it only makes the existing behaviour reachable through the
/// trait object callers need.
impl SecretStore for AccountSecrets {
    fn save_refresh_token(&self, profile: Uuid, token: &str) -> Result<()> {
        self.backend.save_refresh_token(profile, token)
    }

    fn load_refresh_token(&self, profile: Uuid) -> Result<Option<String>> {
        self.backend.load_refresh_token(profile)
    }

    fn delete_refresh_token(&self, profile: Uuid) -> Result<()> {
        self.backend.delete_refresh_token(profile)
    }

    fn save_session(&self, profile: Uuid, session: &CachedSession) -> Result<()> {
        self.backend.save_session(profile, session)
    }

    fn load_session(&self, profile: Uuid) -> Result<Option<CachedSession>> {
        self.backend.load_session(profile)
    }

    fn delete_session(&self, profile: Uuid) -> Result<()> {
        self.backend.delete_session(profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips_and_missing_is_none() {
        let store = MemoryStore::new();
        let id = Uuid::new_v4();
        assert_eq!(store.load_refresh_token(id).unwrap(), None);
        store.save_refresh_token(id, "r-token").unwrap();
        assert_eq!(store.load_refresh_token(id).unwrap().as_deref(), Some("r-token"));
    }

    #[test]
    fn memory_store_keeps_separate_profiles_separate() {
        let store = MemoryStore::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        store.save_refresh_token(a, "token-a").unwrap();
        store.save_refresh_token(b, "token-b").unwrap();
        assert_eq!(store.load_refresh_token(a).unwrap().as_deref(), Some("token-a"));
        assert_eq!(store.load_refresh_token(b).unwrap().as_deref(), Some("token-b"));
    }

    #[test]
    fn memory_store_delete_is_idempotent_and_actually_removes() {
        let store = MemoryStore::new();
        let id = Uuid::new_v4();
        // Deleting an entry that was never set must not error — same
        // idempotence contract as the real keychain backend.
        store.delete_refresh_token(id).unwrap();
        store.save_refresh_token(id, "r-token").unwrap();
        store.delete_refresh_token(id).unwrap();
        assert_eq!(store.load_refresh_token(id).unwrap(), None);
    }

    /// Proves `AccountSecrets` implements `SecretStore` and so can be passed
    /// as `&dyn SecretStore` to `login::finish_interactive`/
    /// `try_cached_session`. Also checks the trait path and the
    /// inherent-method path agree, which they must since both simply forward
    /// to the same boxed backend.
    #[test]
    fn account_secrets_is_usable_as_a_dyn_secret_store() {
        let secrets = AccountSecrets::with_backend(Box::new(MemoryStore::new()), StorageMode::Keychain);
        let id = Uuid::new_v4();

        fn save_through_trait_object(store: &dyn SecretStore, profile: Uuid, token: &str) {
            store.save_refresh_token(profile, token).unwrap();
        }
        save_through_trait_object(&secrets, id, "r-token");

        // The inherent method (used by every direct caller in this crate and
        // in `menu/accounts.rs`) sees the same write the trait-object call made.
        assert_eq!(secrets.load_refresh_token(id).unwrap().as_deref(), Some("r-token"));

        let as_trait: &dyn SecretStore = &secrets;
        assert_eq!(as_trait.load_refresh_token(id).unwrap().as_deref(), Some("r-token"));
        as_trait.delete_refresh_token(id).unwrap();
        assert_eq!(secrets.load_refresh_token(id).unwrap(), None);
    }

    #[test]
    fn account_secrets_with_a_memory_backend_reports_session_only() {
        let secrets = AccountSecrets::with_backend(
            Box::new(MemoryStore::new()),
            StorageMode::SessionOnly {
                reason: "test harness".to_owned(),
            },
        );
        assert!(matches!(secrets.mode(), StorageMode::SessionOnly { .. }));
        // And the display text actually says so, for the UI surface this is
        // meant to feed.
        assert!(secrets.mode().to_string().contains("session-only"));

        let id = Uuid::new_v4();
        secrets.save_refresh_token(id, "r").unwrap();
        assert_eq!(secrets.load_refresh_token(id).unwrap().as_deref(), Some("r"));
        secrets.delete_refresh_token(id).unwrap();
        assert_eq!(secrets.load_refresh_token(id).unwrap(), None);
    }

    #[test]
    fn account_secrets_with_a_keychain_mode_label_reports_keychain() {
        // Exercises the *label*, not the real backend — no keychain is
        // touched. A memory backend stands in so the mode string is still
        // asserted without requiring a real OS keychain to run this test.
        let secrets = AccountSecrets::with_backend(Box::new(MemoryStore::new()), StorageMode::Keychain);
        assert_eq!(secrets.mode(), &StorageMode::Keychain);
        assert_eq!(secrets.mode().to_string(), "OS keychain");
    }

    /// The one test in this crate that touches the real, physical OS
    /// keychain. It creates and then deletes a throwaway entry keyed by a
    /// freshly-generated random UUID with an obviously-fake token string —
    /// never a real profile id or a real token — so it is safe to run
    /// against a developer's actual Keychain.
    ///
    /// Run explicitly: `cargo test -p lodestone-auth --no-fail-fast -- --ignored --nocapture`
    #[test]
    #[ignore = "touches the real OS keychain; run with -- --ignored --nocapture"]
    fn real_keychain_round_trips_a_throwaway_token() {
        let secrets = AccountSecrets::open();
        if !matches!(secrets.mode(), StorageMode::Keychain) {
            // A machine genuinely without a reachable keychain (headless
            // CI, no Secret Service session) cannot exercise this — that is
            // the fallback path working as designed, not a failure of this
            // test, so skip rather than fail.
            eprintln!(
                "skipping real_keychain_round_trips_a_throwaway_token: no real keychain \
                 available on this machine ({})",
                secrets.mode()
            );
            return;
        }

        let profile = Uuid::new_v4();
        let token = "not-a-real-token-just-a-round-trip-probe";

        assert_eq!(secrets.load_refresh_token(profile).unwrap(), None);
        secrets.save_refresh_token(profile, token).unwrap();
        assert_eq!(secrets.load_refresh_token(profile).unwrap().as_deref(), Some(token));
        secrets.delete_refresh_token(profile).unwrap();
        assert_eq!(secrets.load_refresh_token(profile).unwrap(), None);

        println!("real_keychain_round_trips_a_throwaway_token: PASSED against the real OS keychain");
    }

    // -- CachedSession ------------------------------------------------------

    fn sample_session(id: Uuid) -> crate::flow::Session {
        crate::flow::Session {
            access_token: "mc-access-token".to_owned(),
            profile: crate::flow::Profile {
                name: "Notch".to_owned(),
                id,
                skin: Some(crate::flow::ProfileSkin {
                    url: "https://textures.minecraft.net/texture/abc123".to_owned(),
                    variant: crate::flow::SkinVariant::Slim,
                }),
            },
            expires_at: 1_700_100_000,
        }
    }

    /// `to_session(from_session(x))` must reproduce every field, including
    /// the skin variant — an enum with no numeric "obviously wrong" value, so
    /// this picks `Slim` specifically (not the `Classic` default a bug could
    /// silently fall back to) to make a transposition or a dropped field
    /// visible rather than coincidentally passing.
    #[test]
    fn cached_session_round_trips_every_field_including_a_non_default_skin_variant() {
        let id = Uuid::new_v4();
        let session = sample_session(id);
        let cached = CachedSession::from_session(&session);
        assert_eq!(cached.profile_id, id.to_string());
        assert_eq!(cached.access_token, "mc-access-token");
        assert_eq!(cached.expires_at, 1_700_100_000);
        assert_eq!(cached.skin_variant.as_deref(), Some("Slim"));

        let restored = cached.to_session().expect("valid profile id must restore");
        assert_eq!(restored.access_token, session.access_token);
        assert_eq!(restored.expires_at, session.expires_at);
        assert_eq!(restored.profile.id, session.profile.id);
        assert_eq!(restored.profile.name, session.profile.name);
        let restored_skin = restored.profile.skin.expect("skin must survive the round trip");
        let original_skin = session.profile.skin.expect("fixture always sets a skin");
        assert_eq!(restored_skin.url, original_skin.url);
        assert_eq!(restored_skin.variant, original_skin.variant);
    }

    /// A profile with no active skin must round-trip to `None`, not to a
    /// skin with an empty URL — the two are different values and only one of
    /// them is "no skin set".
    #[test]
    fn cached_session_round_trips_a_missing_skin_as_none() {
        let id = Uuid::new_v4();
        let mut session = sample_session(id);
        session.profile.skin = None;
        let cached = CachedSession::from_session(&session);
        assert_eq!(cached.skin_url, None);
        assert_eq!(cached.skin_variant, None);
        let restored = cached.to_session().unwrap();
        assert_eq!(restored.profile.skin, None);
    }

    /// A corrupted `profile_id` must degrade to "no usable cache"
    /// ([`None`]), never panic — this is the exact shape a hand-edited or
    /// bit-rotted keychain entry could produce.
    #[test]
    fn cached_session_with_an_invalid_profile_id_fails_to_restore_rather_than_panicking() {
        let mut cached = CachedSession::from_session(&sample_session(Uuid::new_v4()));
        cached.profile_id = "not-a-uuid".to_owned();
        assert_eq!(cached.to_session(), None);
    }

    #[test]
    fn memory_store_round_trips_a_session_independently_of_the_refresh_token() {
        let store = MemoryStore::new();
        let id = Uuid::new_v4();
        let cached = CachedSession::from_session(&sample_session(id));

        assert_eq!(store.load_session(id).unwrap(), None);
        store.save_session(id, &cached).unwrap();
        assert_eq!(store.load_session(id).unwrap(), Some(cached));
        // The refresh-token map must be untouched by a session write — the
        // two are separate stores precisely so one can be cleared without
        // the other, and this is the control that they are not secretly
        // sharing state.
        assert_eq!(store.load_refresh_token(id).unwrap(), None);

        store.delete_session(id).unwrap();
        assert_eq!(store.load_session(id).unwrap(), None);
        // Deleting an absent session must not error — same idempotence
        // contract as the refresh-token store.
        store.delete_session(id).unwrap();
    }

    #[test]
    fn memory_store_keeps_separate_profiles_sessions_separate() {
        let store = MemoryStore::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let session_a = CachedSession::from_session(&sample_session(a));
        let mut session_b = CachedSession::from_session(&sample_session(b));
        session_b.access_token = "different-token".to_owned();

        store.save_session(a, &session_a).unwrap();
        store.save_session(b, &session_b).unwrap();
        assert_eq!(store.load_session(a).unwrap().unwrap().access_token, "mc-access-token");
        assert_eq!(store.load_session(b).unwrap().unwrap().access_token, "different-token");
    }

    #[test]
    fn account_secrets_forwards_session_storage_through_the_trait_object() {
        let secrets = AccountSecrets::with_backend(Box::new(MemoryStore::new()), StorageMode::Keychain);
        let id = Uuid::new_v4();
        let cached = CachedSession::from_session(&sample_session(id));

        fn save_through_trait_object(store: &dyn SecretStore, profile: Uuid, session: &CachedSession) {
            store.save_session(profile, session).unwrap();
        }
        save_through_trait_object(&secrets, id, &cached);
        assert_eq!(secrets.load_session(id).unwrap(), Some(cached));
    }

    /// The real-keychain counterpart to
    /// [`real_keychain_round_trips_a_throwaway_token`], for the session
    /// service. Same safety property: a random UUID, an obviously-fake
    /// token, created and deleted within the test.
    ///
    /// Run explicitly: `cargo test -p lodestone-auth --no-fail-fast -- --ignored --nocapture`
    #[test]
    #[ignore = "touches the real OS keychain; run with -- --ignored --nocapture"]
    fn real_keychain_round_trips_a_throwaway_session() {
        let secrets = AccountSecrets::open();
        if !matches!(secrets.mode(), StorageMode::Keychain) {
            eprintln!(
                "skipping real_keychain_round_trips_a_throwaway_session: no real keychain \
                 available on this machine ({})",
                secrets.mode()
            );
            return;
        }

        let profile = Uuid::new_v4();
        let cached = CachedSession::from_session(&sample_session(profile));

        assert_eq!(secrets.load_session(profile).unwrap(), None);
        secrets.save_session(profile, &cached).unwrap();
        assert_eq!(secrets.load_session(profile).unwrap(), Some(cached));
        secrets.delete_session(profile).unwrap();
        assert_eq!(secrets.load_session(profile).unwrap(), None);

        println!("real_keychain_round_trips_a_throwaway_session: PASSED against the real OS keychain");
    }
}
