//! The **"Play offline" identity**: the one persisted, user-editable name the
//! client joins under when no Microsoft account is signed in, and the stable
//! UUID derived from it.
//!
//! ## What it is
//!
//! This is the identity a join presents when no Microsoft account is signed in
//! and selected. It is no longer the *only* one: `net.rs`'s `RemoteAuth` now
//! resolves the account switcher's selection on the net thread, so a signed-in
//! player joins under their real profile and this module covers the
//! nobody-signed-in case, `connect_as` (live gates), and singleplayer. Before
//! this module existed the offline arm read
//!
//! ```text
//! username: unique_username(),      // from `lodestone-testsupport`
//! uuid: uuid::Uuid::new_v4(),
//! ```
//!
//! which is a **new account every launch**, twice over: `unique_username`
//! cannot return the same name twice by construction (its own test asserts
//! that), and `new_v4` is random. The owner's report — *"I keep spawning in the
//! air even if I rejoin"* — is the visible half; the invisible half is that no
//! per-player save could ever be found even once the server writes one, because
//! the key changes before the next launch reads it.
//!
//! This module is the fix: one name, persisted at [`offline_identity_path`],
//! editable, defaulting to [`DEFAULT_USERNAME`], plus [`offline_uuid`] to turn
//! it into the same UUID a vanilla offline-mode server would derive for it.
//!
//! ## How it works
//!
//! * **Storage** is a small JSON object (`offline.json`) beside `profiles.json`,
//!   `servers.json` and `options.json` in [`lodestone_auth::paths::data_dir`]:
//!   `{"username": "Player"}`. Parsing follows the same tolerance rule
//!   `AccountsMetadata`/`Options`/`Keybinds` establish — a missing, unreadable
//!   or malformed file is the *default*, never an error and never a panic,
//!   because refusing to start over a corrupt preferences file is worse than
//!   playing under the default name.
//! * **The UUID is derived, never stored.** [`offline_uuid`] reproduces Java's
//!   `UUID.nameUUIDFromBytes("OfflinePlayer:" + name)` — MD5 of those bytes with
//!   the version-3 and RFC 4122 variant bits stamped in — which is exactly how
//!   an offline-mode server computes the account id it persists player data
//!   under. Storing the UUID alongside the name would let the two drift; a
//!   derivation cannot.
//!
//! ## Why the UUID matters even though "the server derives it anyway"
//!
//! `CLAUDE.md` records that offline mode derives the account UUID from the
//! username *and ignores the UUID the client sends*. That is true of **vanilla**
//! and it is exactly why the name has to be stable. It is **not** true of our
//! own integrated server: `lodestone_server`'s login handler does
//! `login_uuid = Some(uuid)` — it echoes back whatever the client presented and
//! keys the player entity on it (`crates/lodestone-server/src/server.rs`, issue
//! That fix). So for singleplayer a stable *name* alone would not have fixed
//! anything; the random `new_v4` was the operative instability there. Both
//! halves had to go.
//!
//! And in the other direction, against real vanilla: our client **discards** the
//! profile in `LOGIN_FINISHED` (`v770`'s `handle_login` binds it to `_profile`),
//! so `NetClient::local_uuid` keeps whatever we sent. Sending a random v4 meant
//! the client's idea of its own identity disagreed with the server's for the
//! whole session — which is a latent defect in anything keyed on "am I this
//! player?", that fix's roster exclusion included. Deriving the UUID the way
//! vanilla does makes the two agree by construction. Fixing the *discard* is a
//! `crates/protocol/**` change and is not made here.
//!
//! ## How to change it
//!
//! * **The name is the only stored field.** If a second one is ever needed, add
//!   it to [`OfflineIdentity::from_json`] and [`OfflineIdentity::to_json`]
//!   together and keep the per-field tolerance: one bad field must not cost the
//!   others.
//! * **Do not "fix" [`offline_uuid`] to use `Uuid::new_v3`.** That is a
//!   *namespaced* v3 (`md5(namespace_bytes ‖ name)`); Java's
//!   `nameUUIDFromBytes` hashes the name bytes alone. The two never agree, and
//!   the wrong one still produces a stable, plausible, version-3 UUID — so no
//!   stability test can tell them apart. The gate that can is
//!   `tests/offline_identity_is_stable.rs`, which pins exact UUIDs computed
//!   outside this workspace.
//! * **The no-argument [`OfflineIdentity::load`]/[`OfflineIdentity::save`] touch
//!   the developer's real data directory.** Tests must use the `_from`/`_to`
//!   twins with a temp path, the same split `saves.rs` uses and
//!   `tests/no_test_touches_the_real_saves_dir.rs` polices there.
//! * **`unique_username` must never come back.** It lives in
//!   `lodestone-testsupport`, which is now a `[dev-dependencies]` entry of this
//!   crate — production code structurally cannot name it — with
//!   `tests/no_production_source_names_testsupport.rs` as the second layer.
//!   Live gates still need a fresh identity per run (a shared offline name is a
//!   shared player file, and a dead player is held on the death screen, which
//!   sends no chunks), so they pass one explicitly through
//!   [`crate::net::NetClient::connect_as`] / [`crate::sim::Sim::connect_as`].
//!
//! ## Configuration
//!
//! `LODESTONE_DATA_DIR` relocates the whole data directory, and therefore this
//! file with it (see [`lodestone_auth::paths::data_dir`]).
//!
//! ## Dependencies
//!
//! [`lodestone_auth::paths`] for the directory, `serde_json` for the file,
//! `lodestone_worldgen_core::hash::md5` for the RFC 1321 digest (the
//! workspace's one MD5, already verified against the RFC's published vectors),
//! and `lodestone_client::LoginProfile` for the value `net.rs` wants.

use std::path::{Path, PathBuf};

use lodestone_client::LoginProfile;
use uuid::Uuid;

/// The name a fresh install joins under.
///
/// Deliberately a constant rather than the OS account name: this string is sent
/// in the login-start packet to every server the player joins, so deriving it
/// from `$USER` would leak the machine's login name to strangers. "Player" is
/// also what a player expects to see and what vanilla's own demo/offline paths
/// use, so the default is recognisable as a placeholder to be changed.
pub const DEFAULT_USERNAME: &str = "Player";

/// The `nameUUIDFromBytes` prefix vanilla uses for offline accounts.
///
/// From `Player.createPlayerUUID` / the server's `GameProfile` construction in
/// offline mode. The exact bytes are load-bearing: change them and every
/// existing offline player file becomes unreachable.
const OFFLINE_PREFIX: &str = "OfflinePlayer:";

/// The persisted "Play offline" identity.
///
/// One field, because the offline placeholder is **not** an account: it has no
/// Mojang profile id, no skin URL, no keychain entry and no "last used"
/// meaning. See the module docs for why it is a single-valued setting rather
/// than a row in `profiles.json`'s `profiles` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineIdentity {
    username: String,
}

impl Default for OfflineIdentity {
    fn default() -> Self {
        Self {
            username: DEFAULT_USERNAME.to_owned(),
        }
    }
}

/// Why a name the player typed was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    /// Empty, or whitespace only. Vanilla's own validator rejects a space, so
    /// there is no "trim it for them" reading that keeps the name valid.
    Empty,
    /// More than 16 characters — the server's hard limit
    /// (`StringUtil.isValidPlayerName`).
    TooLong,
    /// Contains a character the login-start packet's validator rejects:
    /// anything at or below `' '`, or anything outside 7-bit ASCII.
    IllegalCharacter,
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NameError::Empty => write!(f, "a name is required"),
            NameError::TooLong => write!(f, "at most 16 characters"),
            NameError::IllegalCharacter => {
                write!(f, "letters, digits and punctuation only — no spaces")
            }
        }
    }
}

impl std::error::Error for NameError {}

/// Whether `name` is one a server will accept.
///
/// Mirrors vanilla's `StringUtil.isValidPlayerName` — the rule
/// `lodestone_server`'s own login handler applies before it derives the account
/// UUID (`is_valid_player_name`, `crates/lodestone-server/src/server.rs`) —
/// plus a non-empty check, because that helper accepts `""` and a server will
/// not. Duplicated rather than shared because that one is private to a crate
/// this must not depend on for a *client-side* input check; if either moves,
/// they must move together.
///
/// # Errors
/// Returns the first reason the name is unacceptable.
pub fn validate_username(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.chars().count() > 16 {
        return Err(NameError::TooLong);
    }
    if !name.chars().all(|c| c > ' ' && (c as u32) < 127) {
        return Err(NameError::IllegalCharacter);
    }
    Ok(())
}

/// The account UUID an offline-mode server derives for `username`.
///
/// Java's `UUID.nameUUIDFromBytes(("OfflinePlayer:" + username).getBytes(UTF_8))`:
/// the MD5 digest of those bytes with the version nibble forced to 3 and the
/// two variant bits to RFC 4122. **Not** `Uuid::new_v3`, which prepends a
/// namespace — see the module docs.
#[must_use]
pub fn offline_uuid(username: &str) -> Uuid {
    let mut digest = lodestone_worldgen_core::hash::md5(
        format!("{OFFLINE_PREFIX}{username}").as_bytes(),
    );
    digest[6] = (digest[6] & 0x0f) | 0x30;
    digest[8] = (digest[8] & 0x3f) | 0x80;
    Uuid::from_bytes(digest)
}

/// Where [`OfflineIdentity::load`] and [`OfflineIdentity::save`] read and write.
#[must_use]
pub fn offline_identity_path() -> PathBuf {
    lodestone_auth::paths::data_dir().join("offline.json")
}

impl OfflineIdentity {
    /// Loads from the real on-disk location. Missing or corrupt is
    /// [`Self::default`], never an error.
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(&offline_identity_path())
    }

    /// As [`Self::load`], from an explicit path — the twin tests must use, so
    /// nothing in the suite reads or writes the developer's real file.
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        // `crate::platform::store`, not `std::fs`: in a browser this file cannot be
        // read or written, so the player could never rename themselves and the name
        // would silently revert to `DEFAULT_USERNAME` on every reload. The browser arm
        // is `localStorage`. See that module.
        crate::platform::store::read_text(path)
            .map_or_else(|_| Self::default(), |t| Self::from_json(&t))
    }

    /// Parses `text`, degrading to the default rather than failing: a top level
    /// that is not an object, a missing `username`, a non-string `username`, or
    /// one that would not be accepted by [`validate_username`] all yield
    /// [`DEFAULT_USERNAME`].
    ///
    /// The validity check on *load* is the point worth keeping: an invalid name
    /// written by a future version, a hand edit, or a partial write would
    /// otherwise reach the login-start packet and be rejected by the server as
    /// a disconnect with no obvious cause.
    #[must_use]
    pub fn from_json(text: &str) -> Self {
        let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(text)
        else {
            return Self::default();
        };
        obj.get("username")
            .and_then(serde_json::Value::as_str)
            .filter(|n| validate_username(n).is_ok())
            .map_or_else(Self::default, |n| Self {
                username: n.to_owned(),
            })
    }

    /// An identity holding `username` **verbatim, without validating it**.
    ///
    /// For a caller that already has the name it must join under and is not
    /// storing it — [`crate::net::NetClient::connect_as`], i.e. the live gates.
    /// Nothing that *persists* a name should use this: [`Self::set_username`] is
    /// the validating door, and [`Self::from_json`] re-checks on load, so a
    /// value that reaches the file is checked twice. Unchecked here so a gate
    /// can deliberately drive a name the server will reject and observe the
    /// disconnect, rather than silently joining as [`DEFAULT_USERNAME`].
    #[must_use]
    pub fn from_username_unchecked(username: String) -> Self {
        Self { username }
    }

    /// The stored name.
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Replaces the stored name.
    ///
    /// # Errors
    /// Returns why the name was refused, leaving the identity unchanged — so a
    /// UI can report the reason and keep the old name live.
    pub fn set_username(&mut self, name: &str) -> Result<(), NameError> {
        validate_username(name)?;
        self.username = name.to_owned();
        Ok(())
    }

    /// The UUID this identity joins under: [`offline_uuid`] of [`Self::username`].
    #[must_use]
    pub fn uuid(&self) -> Uuid {
        offline_uuid(&self.username)
    }

    /// The login-start identity `net.rs` presents for an offline join.
    #[must_use]
    pub fn login_profile(&self) -> LoginProfile {
        LoginProfile {
            username: self.username.clone(),
            uuid: self.uuid(),
        }
    }

    /// The exact JSON [`Self::save_to`] writes, exposed so a test does not have
    /// to restate the shape.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "username".into(),
            serde_json::Value::String(self.username.clone()),
        );
        serde_json::Value::Object(obj)
    }

    /// Writes to the real on-disk location.
    ///
    /// # Errors
    /// The underlying I/O error if the directory cannot be created or the file
    /// cannot be written.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&offline_identity_path())
    }

    /// As [`Self::save`], to an explicit path (for tests).
    ///
    /// # Errors
    /// The underlying I/O error if the directory cannot be created or the file
    /// cannot be written.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        // `crate::platform::store` — see `crate::config::Options::save_to`.
        let text =
            serde_json::to_string_pretty(&self.to_json()).unwrap_or_else(|_| "{}".to_owned());
        crate::platform::store::write_text(path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vanilla-derived offline UUIDs, computed **outside this workspace** with
    /// CPython's `hashlib.md5` and the documented `nameUUIDFromBytes` bit
    /// stamping:
    ///
    /// ```text
    /// >>> d = bytearray(hashlib.md5(b"OfflinePlayer:Player").digest())
    /// >>> d[6] = (d[6] & 0x0f) | 0x30; d[8] = (d[8] & 0x3f) | 0x80
    /// >>> uuid.UUID(bytes=bytes(d))
    /// UUID('a01e3843-e521-3998-958a-f459800e4d11')
    /// ```
    ///
    /// A second implementation of the same published rule, which is what makes
    /// this an outside expectation rather than `derive(derive(x)) == x`. There
    /// is no JVM on this machine (`CLAUDE.md`), so a `nameUUIDFromBytes` oracle
    /// run is unavailable; the rule is short enough to re-derive exactly, and
    /// `"Player"`'s value is additionally the one every offline-mode server on
    /// the internet assigns to that name.
    const VECTORS: [(&str, &str); 5] = [
        ("Player", "a01e3843-e521-3998-958a-f459800e4d11"),
        ("Steve", "5627dd98-e6be-3c21-b8a8-e92344183641"),
        ("propagated", "1f83d2d8-7412-3e98-9ab7-b3b70e62e948"),
        ("Notch", "b50ad385-829d-3141-a216-7e7d7539ba7f"),
        ("Dev", "380df991-f603-344c-a090-369bad2a924a"),
    ];

    #[test]
    fn offline_uuid_matches_the_externally_computed_vectors() {
        for (name, expected) in VECTORS {
            assert_eq!(
                offline_uuid(name),
                Uuid::parse_str(expected).expect("hand-written vector parses"),
                "offline uuid for {name:?}"
            );
        }
    }

    /// The **negative control** for the vector above: the plausible wrong
    /// implementation is `Uuid::new_v3`, which hashes a *namespace* followed by
    /// the name. It produces a stable, version-3, entirely legitimate-looking
    /// UUID, so nothing except an exact external value can tell it apart.
    #[test]
    fn the_namespaced_v3_reading_disagrees_with_every_vector() {
        for (name, expected) in VECTORS {
            let namespaced = Uuid::new_v3(
                &Uuid::NAMESPACE_OID,
                format!("{OFFLINE_PREFIX}{name}").as_bytes(),
            );
            assert_ne!(
                namespaced.to_string(),
                expected,
                "if these agreed, the vectors could not distinguish the two derivations"
            );
        }
        // …and it really is version 3 too, so "check the version" would not
        // have caught it either.
        assert_eq!(
            Uuid::new_v3(&Uuid::NAMESPACE_OID, b"OfflinePlayer:Player").get_version_num(),
            3
        );
    }

    #[test]
    fn the_derived_uuid_is_version_three_and_rfc_4122_variant() {
        // Both stamped fields, asserted on the bytes rather than on the crate's
        // accessors, because the accessors are the thing under test.
        let bytes = *offline_uuid("Player").as_bytes();
        assert_eq!(bytes[6] & 0xf0, 0x30, "version nibble");
        assert_eq!(bytes[8] & 0xc0, 0x80, "variant bits");
        assert_eq!(offline_uuid("Player").get_version_num(), 3);
    }

    #[test]
    fn the_default_is_the_placeholder_name_and_its_derived_uuid() {
        let id = OfflineIdentity::default();
        assert_eq!(id.username(), DEFAULT_USERNAME);
        assert_eq!(id.uuid(), offline_uuid(DEFAULT_USERNAME));
        assert_eq!(
            id.uuid(),
            Uuid::parse_str("a01e3843-e521-3998-958a-f459800e4d11").unwrap()
        );
    }

    #[test]
    fn a_stored_name_round_trips_through_a_real_file() {
        let path = temp_path("roundtrip");
        let mut id = OfflineIdentity::default();
        id.set_username("Steve").expect("valid name");
        id.save_to(&path).expect("save should create parent dirs");
        let loaded = OfflineIdentity::load_from(&path);
        assert_eq!(loaded, id);
        assert_eq!(loaded.username(), "Steve");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_written_json_is_the_hand_written_shape() {
        let mut id = OfflineIdentity::default();
        id.set_username("Steve").unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&id.to_json()).unwrap(),
            "{\n  \"username\": \"Steve\"\n}"
        );
        // And the other direction, from a string this module did not produce.
        assert_eq!(
            OfflineIdentity::from_json("{\"username\": \"Steve\"}").username(),
            "Steve"
        );
    }

    #[test]
    fn a_missing_or_corrupt_file_is_the_default_not_an_error() {
        assert_eq!(
            OfflineIdentity::load_from(Path::new("/nonexistent/offline.json")),
            OfflineIdentity::default()
        );
        let path = temp_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "}{ not json").unwrap();
        assert_eq!(
            OfflineIdentity::load_from(&path),
            OfflineIdentity::default()
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_persisted_name_a_server_would_reject_falls_back_to_the_default() {
        // Each of these is a name that could reach the file by hand edit, a
        // partial write, or a future version with looser rules — and would be
        // an unexplained disconnect if it reached the login-start packet.
        for bad in [
            "{\"username\": \"\"}",
            "{\"username\": \"this name is far too long\"}",
            "{\"username\": \"has space\"}",
            "{\"username\": \"emoji\\u2764\"}",
            "{\"username\": 42}",
            "{\"username\": null}",
            "{}",
            "[]",
        ] {
            assert_eq!(
                OfflineIdentity::from_json(bad).username(),
                DEFAULT_USERNAME,
                "input: {bad}"
            );
        }
    }

    #[test]
    fn validate_username_accepts_what_a_server_accepts_and_nothing_else() {
        for ok in ["Player", "a", "0123456789abcdef", "Steve_1", "-x-"] {
            assert_eq!(validate_username(ok), Ok(()), "{ok:?}");
        }
        assert_eq!(validate_username(""), Err(NameError::Empty));
        // 17 characters: one past the server's hard limit.
        assert_eq!(
            validate_username("0123456789abcdefg"),
            Err(NameError::TooLong)
        );
        assert_eq!(validate_username("has space"), Err(NameError::IllegalCharacter));
        assert_eq!(validate_username("caf\u{e9}"), Err(NameError::IllegalCharacter));
        assert_eq!(validate_username("tab\there"), Err(NameError::IllegalCharacter));
        // Length is counted in `char`s, not bytes: a 16-char name of
        // multi-byte characters must fail on the *character* rule, not slip
        // through a byte-length check.
        assert_eq!(
            validate_username(&"\u{e9}".repeat(16)),
            Err(NameError::IllegalCharacter)
        );
    }

    #[test]
    fn set_username_leaves_the_old_name_live_when_it_refuses() {
        let mut id = OfflineIdentity::default();
        id.set_username("Steve").unwrap();
        assert_eq!(id.set_username("no good"), Err(NameError::IllegalCharacter));
        assert_eq!(id.username(), "Steve", "a refused edit must not clear the name");
    }

    #[test]
    fn the_login_profile_pairs_the_stored_name_with_its_derived_uuid() {
        let mut id = OfflineIdentity::default();
        id.set_username("propagated").unwrap();
        let profile = id.login_profile();
        assert_eq!(profile.username, "propagated");
        assert_eq!(
            profile.uuid,
            Uuid::parse_str("1f83d2d8-7412-3e98-9ab7-b3b70e62e948").unwrap()
        );
    }

    fn temp_path(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lodestone-offline-identity-test-{}-{tag}/offline.json",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        path
    }
}
