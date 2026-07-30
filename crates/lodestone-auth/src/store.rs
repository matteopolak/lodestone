//! Where the Microsoft **refresh** token actually lives (issue #64).
//!
//! The Minecraft services access token is short-lived (~24 h) and is always
//! re-derived from the refresh token via [`crate::flow::refresh_token`] +
//! [`crate::flow::session_from_ms_token`] — it is never persisted. Only the
//! refresh token needs long-term storage, and it goes into the real OS
//! credential store: macOS Keychain, Windows Credential Manager, or a Linux
//! Secret Service keyring, all behind the `keyring` crate's `Entry` type.
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

use std::collections::HashMap;
use std::sync::Mutex;

use uuid::Uuid;

use crate::error::Result;

/// The keychain "service" every entry is grouped under; the per-entry
/// "account" (in Keychain Access's terminology) is the profile UUID. Using a
/// reverse-DNS-shaped constant, rather than a bare name like `"lodestone"`,
/// keeps this from colliding with an unrelated app's entries on a shared
/// Secret Service keyring.
const KEYCHAIN_SERVICE: &str = "dev.lodestone.ms-refresh-token";

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
}

/// How [`AccountSecrets`] is actually protecting tokens right now. Exists so
/// a caller (the account-switcher UI, issue #63) can say so — a fallback the
/// user cannot see is exactly the "silent leave-behind" this work exists to
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
}

impl std::fmt::Display for StorageMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keychain => write!(f, "OS keychain"),
            Self::SessionOnly { reason } => write!(
                f,
                "session-only (in-memory, lost on exit; OS keychain unavailable: {reason})"
            ),
        }
    }
}

/// In-memory [`SecretStore`]. Used both for hermetic tests and as the
/// automatic fallback backend when the real keychain cannot be reached.
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<HashMap<Uuid, String>>);

impl MemoryStore {
    /// Creates an empty in-memory store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, String>> {
        // A poisoned lock (a panic while held) still holds perfectly good
        // data; recovering it is strictly better than a second panic here.
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SecretStore for MemoryStore {
    fn save_refresh_token(&self, profile: Uuid, token: &str) -> Result<()> {
        self.lock().insert(profile, token.to_owned());
        Ok(())
    }

    fn load_refresh_token(&self, profile: Uuid) -> Result<Option<String>> {
        Ok(self.lock().get(&profile).cloned())
    }

    fn delete_refresh_token(&self, profile: Uuid) -> Result<()> {
        self.lock().remove(&profile);
        Ok(())
    }
}

/// The real OS-keychain-backed [`SecretStore`]. Holds no state of its own —
/// every call builds a fresh `keyring::Entry` for the profile it is given —
/// so it is trivially `Send + Sync` and cheap to construct.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainStore;

impl KeychainStore {
    fn entry(profile: Uuid) -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(KEYCHAIN_SERVICE, &profile.to_string())
    }
}

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
}

/// Probes whether the real keychain backend is actually reachable, without
/// requiring (or leaving behind) any real entry.
///
/// Constructing an `Entry` and asking for a password that was never set
/// distinguishes "the backend answered: no such entry" (`Ok`) from "the
/// backend could not be reached at all" (`Err`) — e.g. no D-Bus Secret
/// Service session on headless Linux, which is exactly the case the brief
/// asks to degrade gracefully rather than panic or fall back to plaintext.
fn probe_keychain() -> std::result::Result<(), String> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, "lodestone-availability-probe")
        .map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
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
}
