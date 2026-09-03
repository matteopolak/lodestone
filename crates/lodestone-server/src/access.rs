//! Server access control: ops, whitelist, player bans and IP bans,
//! in vanilla's own four JSON files, enforced at join.
//!
//! ## What it is
//!
//! Before this module `grep -rniE 'whitelist|banned.player|ops\.json|permission.level'`
//! over `crates/` returned two hits, both test *comments* about vanilla's RCON
//! console. There was no operator model at all: every connection was the
//! singleplayer owner, and a hosted world had no way to refuse anybody. That was
//! defensible while `bind` could not be configured; since `open_to_lan` landed with
//! RCON, the query protocol and LAN discovery, a hosted world has real remote
//! actors to authorise.
//!
//! Four files, vanilla's names and vanilla's field names, in the server directory:
//!
//! | file | entry |
//! |---|---|
//! | `ops.json` | `uuid`, `name`, `level` (0–4), `bypassesPlayerLimit` |
//! | `whitelist.json` | `uuid`, `name` |
//! | `banned-players.json` | `uuid`, `name`, `created`, `source`, `expires`, `reason` |
//! | `banned-ips.json` | `ip`, `created`, `source`, `expires`, `reason` |
//!
//! ## How it works
//!
//! [`AccessLists`] is the whole store; [`AccessHandle`] is the shared, cloneable
//! handle a connection and an admin console both hold, with the same
//! `with`-funnels-every-access shape [`crate::BlockEntityHandle`] established.
//!
//! [`AccessLists::may_join`] is vanilla `PlayerList.canPlayerLogin`, **in vanilla's
//! order** — player ban, then whitelist, then IP ban, then the player limit. The
//! order is observable: a banned *and* non-whitelisted player is told they are
//! banned, and a test that asserted the whitelist message would be asserting the
//! wrong precedence. It returns a [`JoinRefusal`] carrying the translation key the
//! client renders, so `server.rs` can hand it straight to
//! `ServerProtocol::encode_disconnect`.
//!
//! Timed bans expire. `expires` is vanilla's `"yyyy-MM-dd HH:mm:ss Z"` or the
//! literal `"forever"`, and [`parse_ban_expiry`] reads the former into a Unix
//! timestamp so an elapsed ban stops applying without anyone editing the file.
//! An **unparseable** expiry is treated as `forever`: refusing to enforce a ban we
//! cannot read is the wrong direction to fail.
//!
//! ## How to change it, and the gotchas
//!
//! * **Permission levels are stored and fully read.** `PermissionLevel` 0–4 is on
//!   every op entry, [`AccessLists::permission_level`]/[`AccessLists::command_permission_level`]
//!   answer it, and every built-in command root is gated at its vanilla level
//!   through `crate::commands::registrar::Registrar::require_level` — no longer a
//!   disclosed gap. Granting/revoking access itself has a real command surface
//!   too: `crate::commands::access_commands` (`/op`/`/deop`/`/whitelist`),
//!   scoped to RCON — see that module's own doc for why chat is deliberately
//!   excluded. See `docs/server-access-control.md`.
//! * **An empty `ops.json` does not mean "nobody is an operator".** A
//!   singleplayer world has no ops file and its one player must still be able to
//!   do everything, so [`AccessLists::default`] has `whitelist_enabled: false` and
//!   [`AccessLists::owner`] names a uuid that is always level 4. `server.rs`
//!   passes an owner for the in-memory constructors and `None` for a LAN host.
//!   Getting this backwards locks the player out of their own world, which is why
//!   the default is permissive and the *host* opts in.
//! * **Missing files are not errors.** A world that has never had an op has no
//!   `ops.json`, which is every world's first start. Only a malformed file is an
//!   error, and it is reported rather than silently treated as empty — an
//!   `ops.json` with a typo that read as "no operators" is how an admin loses
//!   access to their own server.
//! * **The name is a label, the uuid is the identity.** Offline mode derives the
//!   uuid from the name (see `CLAUDE.md`'s live-server hazards), so the two agree
//!   there, but a ban is matched on **uuid** and the name is only for the file to
//!   be human-readable. Matching on name would let a rename evade a ban.
//!
//! ## Configuration
//!
//! `whitelist.json`'s presence does not enable the whitelist; vanilla's
//! `white-list` property does, and here it is [`AccessLists::whitelist_enabled`],
//! set by the host through [`AccessHandle::set_whitelist_enabled`]. Default off.
//!
//! ## Dependencies
//!
//! `serde_json` (already a dependency) and `std::fs`. Native only, like
//! `region_source`: a browser world has no filesystem and no remote players.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use uuid::Uuid;

/// Vanilla's four file names, in the server directory.
pub const OPS_FILE: &str = "ops.json";
/// See [`OPS_FILE`].
pub const WHITELIST_FILE: &str = "whitelist.json";
/// See [`OPS_FILE`].
pub const BANNED_PLAYERS_FILE: &str = "banned-players.json";
/// See [`OPS_FILE`].
pub const BANNED_IPS_FILE: &str = "banned-ips.json";

/// The highest vanilla permission level (`PermissionLevel.ALL`): bypass spawn
/// protection, use every command including `/stop`.
pub const MAX_PERMISSION_LEVEL: u8 = 4;

/// Why a join was refused, as the translation key a vanilla client renders plus
/// the optional reason line vanilla appends.
///
/// The keys are vanilla's own (`PlayerList.canPlayerLogin`), so a real client shows
/// its localised text rather than raw English.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinRefusal {
    /// `multiplayer.disconnect.banned.reason`, with the ban's `reason`.
    Banned(String),
    /// `multiplayer.disconnect.banned_ip.reason`, with the ban's `reason`.
    IpBanned(String),
    /// `multiplayer.disconnect.not_whitelisted`.
    NotWhitelisted,
    /// `multiplayer.disconnect.server_full`.
    ServerFull,
}

impl JoinRefusal {
    /// The translation key a client resolves.
    #[must_use]
    pub fn translation_key(&self) -> &'static str {
        match self {
            Self::Banned(_) => "multiplayer.disconnect.banned.reason",
            Self::IpBanned(_) => "multiplayer.disconnect.banned_ip.reason",
            Self::NotWhitelisted => "multiplayer.disconnect.not_whitelisted",
            Self::ServerFull => "multiplayer.disconnect.server_full",
        }
    }

    /// The message to put on the wire. Our `encode_disconnect` takes plain text
    /// rather than a translation component, so a refusal with a reason renders as
    /// `"<key>: <reason>"` — the key is still there for a client that wants to map
    /// it, and the reason is the part a player actually needs.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Banned(reason) | Self::IpBanned(reason) if !reason.is_empty() => {
                format!("{}: {reason}", self.translation_key())
            }
            _ => self.translation_key().to_string(),
        }
    }
}

/// One `ops.json` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpEntry {
    /// The player's profile uuid — the identity a level is looked up by.
    pub uuid: Uuid,
    /// The player's name, for the file to be readable. Not matched on.
    pub name: String,
    /// `PermissionLevel`, `0..=4`. Values above 4 are clamped on load.
    pub level: u8,
    /// `bypassesPlayerLimit`.
    pub bypasses_player_limit: bool,
}

/// One `whitelist.json` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhitelistEntry {
    /// The whitelisted uuid.
    pub uuid: Uuid,
    /// The name, for readability.
    pub name: String,
}

/// One `banned-players.json` or `banned-ips.json` entry.
///
/// `created`/`expires` are kept as vanilla's own strings so a file this server
/// rewrites is byte-comparable with one vanilla wrote, rather than reformatted
/// through a different date library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BanEntry {
    /// The banned player's name (uuid bans) — informational.
    pub name: String,
    /// Who issued the ban. Vanilla defaults to `"(Unknown)"`.
    pub source: String,
    /// `"yyyy-MM-dd HH:mm:ss Z"`.
    pub created: String,
    /// `"forever"`, or `"yyyy-MM-dd HH:mm:ss Z"`.
    pub expires: String,
    /// The reason shown to the player. Vanilla's default is `"Banned by an operator."`.
    pub reason: String,
}

impl BanEntry {
    /// A permanent ban issued now by `source`.
    #[must_use]
    pub fn permanent(name: impl Into<String>, source: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
            created: format_now(),
            expires: "forever".to_string(),
            reason: reason.into(),
        }
    }

    /// Whether this ban still applies at `now_secs` (Unix seconds).
    ///
    /// An unparseable `expires` is treated as `forever` — see the module doc for
    /// why that is the right direction to fail.
    #[must_use]
    pub fn active_at(&self, now_secs: i64) -> bool {
        match parse_ban_expiry(&self.expires) {
            Some(expiry) => now_secs < expiry,
            None => true,
        }
    }
}

/// Parses vanilla's `"yyyy-MM-dd HH:mm:ss Z"` into Unix seconds, or `None` for
/// `"forever"` and for anything unparseable.
///
/// Hand-rolled rather than pulling in a date crate: the format is fixed and this is
/// the only place in the workspace that reads it. `Z` is `±HHMM`.
#[must_use]
pub fn parse_ban_expiry(expires: &str) -> Option<i64> {
    let expires = expires.trim();
    if expires.is_empty() || expires.eq_ignore_ascii_case("forever") {
        return None;
    }
    let mut parts = expires.split(' ');
    let date = parts.next()?;
    let time = parts.next()?;
    let zone = parts.next().unwrap_or("+0000");

    let mut ymd = date.split('-');
    let year: i64 = ymd.next()?.parse().ok()?;
    let month: i64 = ymd.next()?.parse().ok()?;
    let day: i64 = ymd.next()?.parse().ok()?;

    let mut hms = time.split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    let second: i64 = hms.next()?.parse().ok()?;

    // `days_from_civil` — Howard Hinnant's algorithm, the standard one.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    let offset = {
        let bytes = zone.as_bytes();
        if bytes.len() == 5 && (bytes[0] == b'+' || bytes[0] == b'-') {
            let hh: i64 = zone[1..3].parse().ok()?;
            let mm: i64 = zone[3..5].parse().ok()?;
            let magnitude = hh * 3600 + mm * 60;
            if bytes[0] == b'-' { -magnitude } else { magnitude }
        } else {
            0
        }
    };

    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// `"yyyy-MM-dd HH:mm:ss +0000"` for the current time, for a ban this server issues.
fn format_now() -> String {
    let secs = lodestone_time::epoch_duration().as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Inverse of `days_from_civil`.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} +0000",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Something went wrong reading or writing an access file.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The file could not be read or written.
    #[error("access list {path}: {source}")]
    Io {
        /// Which file.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The file exists but is not the JSON array of objects vanilla writes.
    ///
    /// Deliberately **not** treated as an empty list: an `ops.json` with a typo
    /// that read as "no operators" is how an admin loses access to their own
    /// server.
    #[error("access list {path} is malformed: {detail}")]
    Malformed {
        /// Which file.
        path: PathBuf,
        /// What was wrong.
        detail: String,
    },
}

/// Ops, whitelist and the two ban lists.
#[derive(Debug, Default, Clone)]
pub struct AccessLists {
    ops: HashMap<Uuid, OpEntry>,
    whitelist: HashMap<Uuid, WhitelistEntry>,
    bans: HashMap<Uuid, BanEntry>,
    ip_bans: HashMap<String, BanEntry>,
    whitelist_enabled: bool,
    max_players: Option<usize>,
    owner: Option<Uuid>,
}

impl AccessLists {
    /// Empty lists, whitelist off, no player limit — the singleplayer shape, and
    /// the one that cannot lock anybody out. See the module doc.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads all four files from `dir`. A missing file is an empty list, not an
    /// error; a malformed one **is** an error.
    ///
    /// # Errors
    /// [`Error::Io`] for a file that exists but cannot be read, [`Error::Malformed`]
    /// for one whose JSON is not vanilla's array-of-objects shape.
    pub fn load(dir: &Path) -> Result<Self, Error> {
        let mut lists = Self::new();
        for entry in read_array(&dir.join(OPS_FILE))? {
            let uuid = json_uuid(&entry, "uuid");
            if let Some(uuid) = uuid {
                lists.ops.insert(
                    uuid,
                    OpEntry {
                        uuid,
                        name: json_str(&entry, "name"),
                        level: u8::try_from(entry.get("level").and_then(serde_json::Value::as_u64).unwrap_or(0))
                            .unwrap_or(MAX_PERMISSION_LEVEL)
                            .min(MAX_PERMISSION_LEVEL),
                        bypasses_player_limit: entry
                            .get("bypassesPlayerLimit")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                    },
                );
            }
        }
        for entry in read_array(&dir.join(WHITELIST_FILE))? {
            if let Some(uuid) = json_uuid(&entry, "uuid") {
                lists.whitelist.insert(
                    uuid,
                    WhitelistEntry {
                        uuid,
                        name: json_str(&entry, "name"),
                    },
                );
            }
        }
        for entry in read_array(&dir.join(BANNED_PLAYERS_FILE))? {
            if let Some(uuid) = json_uuid(&entry, "uuid") {
                lists.bans.insert(uuid, ban_from_json(&entry));
            }
        }
        for entry in read_array(&dir.join(BANNED_IPS_FILE))? {
            let ip = json_str(&entry, "ip");
            if !ip.is_empty() {
                lists.ip_bans.insert(ip, ban_from_json(&entry));
            }
        }
        Ok(lists)
    }

    /// Writes all four files to `dir`, creating it if needed.
    ///
    /// Every file is written, including empty ones — vanilla does the same, and a
    /// world whose `ops.json` disappeared when the last op was removed would read
    /// as "the file was never created" on the next load.
    ///
    /// # Errors
    /// [`Error::Io`] if the directory cannot be created or a file cannot be written.
    pub fn save(&self, dir: &Path) -> Result<(), Error> {
        std::fs::create_dir_all(dir).map_err(|source| Error::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let ops: Vec<serde_json::Value> = self
            .sorted_ops()
            .into_iter()
            .map(|op| {
                serde_json::json!({
                    "uuid": op.uuid.to_string(),
                    "name": op.name,
                    "level": op.level,
                    "bypassesPlayerLimit": op.bypasses_player_limit,
                })
            })
            .collect();
        write_array(&dir.join(OPS_FILE), &ops)?;

        let mut whitelist: Vec<&WhitelistEntry> = self.whitelist.values().collect();
        whitelist.sort_by(|a, b| a.uuid.cmp(&b.uuid));
        let whitelist: Vec<serde_json::Value> = whitelist
            .into_iter()
            .map(|w| serde_json::json!({ "uuid": w.uuid.to_string(), "name": w.name }))
            .collect();
        write_array(&dir.join(WHITELIST_FILE), &whitelist)?;

        let mut bans: Vec<(&Uuid, &BanEntry)> = self.bans.iter().collect();
        bans.sort_by(|a, b| a.0.cmp(b.0));
        let bans: Vec<serde_json::Value> = bans
            .into_iter()
            .map(|(uuid, ban)| {
                let mut value = ban_to_json(ban);
                value["uuid"] = serde_json::Value::String(uuid.to_string());
                value
            })
            .collect();
        write_array(&dir.join(BANNED_PLAYERS_FILE), &bans)?;

        let mut ip_bans: Vec<(&String, &BanEntry)> = self.ip_bans.iter().collect();
        ip_bans.sort_by(|a, b| a.0.cmp(b.0));
        let ip_bans: Vec<serde_json::Value> = ip_bans
            .into_iter()
            .map(|(ip, ban)| {
                let mut value = ban_to_json(ban);
                value["ip"] = serde_json::Value::String(ip.clone());
                value
            })
            .collect();
        write_array(&dir.join(BANNED_IPS_FILE), &ip_bans)
    }

    /// Vanilla `PlayerList.canPlayerLogin`, in vanilla's own order: player ban,
    /// whitelist, IP ban, then the player limit.
    ///
    /// `now_secs` is Unix seconds, so a timed ban's expiry is testable without
    /// waiting; `online` is the count of players already connected.
    #[must_use]
    pub fn may_join(
        &self,
        uuid: Uuid,
        ip: Option<IpAddr>,
        online: usize,
        now_secs: i64,
    ) -> Result<(), JoinRefusal> {
        if let Some(ban) = self.bans.get(&uuid)
            && ban.active_at(now_secs)
        {
            return Err(JoinRefusal::Banned(ban.reason.clone()));
        }
        if self.whitelist_enabled && !self.whitelist.contains_key(&uuid) && self.owner != Some(uuid) {
            return Err(JoinRefusal::NotWhitelisted);
        }
        if let Some(ip) = ip
            && let Some(ban) = self.ip_bans.get(&ip.to_string())
            && ban.active_at(now_secs)
        {
            return Err(JoinRefusal::IpBanned(ban.reason.clone()));
        }
        if let Some(max) = self.max_players
            && online >= max
            && !self.bypasses_player_limit(uuid)
        {
            return Err(JoinRefusal::ServerFull);
        }
        Ok(())
    }

    /// The permission level `uuid` holds: their op entry's, `4` for the world
    /// owner, `0` otherwise.
    #[must_use]
    pub fn permission_level(&self, uuid: Uuid) -> u8 {
        if self.owner == Some(uuid) {
            return MAX_PERMISSION_LEVEL;
        }
        self.ops.get(&uuid).map_or(0, |op| op.level)
    }

    /// Whether `uuid` holds at least `level`.
    #[must_use]
    pub fn has_permission_level(&self, uuid: Uuid, level: u8) -> bool {
        self.permission_level(uuid) >= level
    }

    /// Whether **no** operator model has been configured: no owner, no ops.
    ///
    /// This is the state [`AccessLists::new`] and `AccessLists::default` produce,
    /// and the state every legacy `serve_connection*` entry point passes. It is
    /// the singleplayer shape.
    #[must_use]
    pub fn is_unconfigured(&self) -> bool {
        self.owner.is_none() && self.ops.is_empty()
    }

    /// The permission level to gate a **command** at — [`permission_level`], except
    /// that an [unconfigured](Self::is_unconfigured) world grants
    /// [`MAX_PERMISSION_LEVEL`].
    ///
    /// # Why this is not just `permission_level`
    ///
    /// Measured, not assumed: `grep set_owner crates/lodestone-server/src` finds
    /// **no production caller**. The module doc above says "`server.rs` passes an
    /// owner for the in-memory constructors"; that claim is stale. So *every*
    /// connection in the shipping product resolves to `permission_level == 0`, and
    /// gating `/gamemode` at its vanilla level 2 against that would make creative
    /// mode unreachable in singleplayer — a strictly worse outcome than the
    /// ungated version it replaced.
    ///
    /// Answering `MAX_PERMISSION_LEVEL` for a world with no operator model at all
    /// is the same posture the whole module already documents for the empty
    /// default ("the one that cannot lock a player out of their own world"), now
    /// stated where a command can read it. It is **not** a bypass: the moment a
    /// host names an owner or ops a single player, this collapses to
    /// [`permission_level`] and a non-op cannot use a level-2 command — which is
    /// vanilla's LAN behaviour.
    ///
    /// [`permission_level`]: Self::permission_level
    #[must_use]
    pub fn command_permission_level(&self, uuid: Uuid) -> u8 {
        if self.is_unconfigured() {
            return MAX_PERMISSION_LEVEL;
        }
        self.permission_level(uuid)
    }

    fn bypasses_player_limit(&self, uuid: Uuid) -> bool {
        self.owner == Some(uuid)
            || self
                .ops
                .get(&uuid)
                .is_some_and(|op| op.bypasses_player_limit)
    }

    /// Ops sorted by uuid, so a saved file is stable across runs.
    #[must_use]
    pub fn sorted_ops(&self) -> Vec<&OpEntry> {
        let mut ops: Vec<&OpEntry> = self.ops.values().collect();
        ops.sort_by(|a, b| a.uuid.cmp(&b.uuid));
        ops
    }

    /// Grants `uuid` operator status at `level` (clamped to `0..=4`).
    pub fn op(&mut self, uuid: Uuid, name: impl Into<String>, level: u8) {
        self.ops.insert(
            uuid,
            OpEntry {
                uuid,
                name: name.into(),
                level: level.min(MAX_PERMISSION_LEVEL),
                bypasses_player_limit: false,
            },
        );
    }

    /// Removes `uuid`'s operator status, returning whether it had any.
    pub fn deop(&mut self, uuid: Uuid) -> bool {
        self.ops.remove(&uuid).is_some()
    }

    /// Adds `uuid` to the whitelist.
    pub fn whitelist_add(&mut self, uuid: Uuid, name: impl Into<String>) {
        self.whitelist.insert(
            uuid,
            WhitelistEntry {
                uuid,
                name: name.into(),
            },
        );
    }

    /// Removes `uuid` from the whitelist, returning whether it was on it.
    pub fn whitelist_remove(&mut self, uuid: Uuid) -> bool {
        self.whitelist.remove(&uuid).is_some()
    }

    /// Whether the whitelist is enforced at all — vanilla's `white-list` property,
    /// not the file's presence.
    #[must_use]
    pub fn whitelist_enabled(&self) -> bool {
        self.whitelist_enabled
    }

    /// Turns whitelist enforcement on or off.
    pub fn set_whitelist_enabled(&mut self, enabled: bool) {
        self.whitelist_enabled = enabled;
    }

    /// Sets the maximum simultaneous players, or `None` for no limit.
    pub fn set_max_players(&mut self, max: Option<usize>) {
        self.max_players = max;
    }

    /// Names the world owner: always level 4, always whitelisted, always past the
    /// player limit. See the module doc for why this exists.
    pub fn set_owner(&mut self, owner: Option<Uuid>) {
        self.owner = owner;
    }

    /// Bans `uuid`.
    pub fn ban(&mut self, uuid: Uuid, entry: BanEntry) {
        self.bans.insert(uuid, entry);
    }

    /// Lifts `uuid`'s ban, returning whether there was one.
    pub fn pardon(&mut self, uuid: Uuid) -> bool {
        self.bans.remove(&uuid).is_some()
    }

    /// Bans an IP address, keyed by its string form (vanilla's own key).
    pub fn ban_ip(&mut self, ip: IpAddr, entry: BanEntry) {
        self.ip_bans.insert(ip.to_string(), entry);
    }

    /// Lifts an IP ban, returning whether there was one.
    pub fn pardon_ip(&mut self, ip: IpAddr) -> bool {
        self.ip_bans.remove(&ip.to_string()).is_some()
    }

    /// Every banned uuid paired with its entry, sorted by uuid.
    #[must_use]
    pub fn bans(&self) -> Vec<(Uuid, &BanEntry)> {
        let mut bans: Vec<(Uuid, &BanEntry)> = self.bans.iter().map(|(u, b)| (*u, b)).collect();
        bans.sort_by(|a, b| a.0.cmp(&b.0));
        bans
    }

    /// Every whitelisted uuid, sorted.
    #[must_use]
    pub fn whitelisted(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self.whitelist.keys().copied().collect();
        ids.sort();
        ids
    }
}

/// A cloneable handle over one [`AccessLists`], shared by every connection and by
/// an admin console.
///
/// Same `with`-funnels-every-access shape as [`crate::BlockEntityHandle`], and for
/// the same reason: one place handles a poisoned lock.
#[derive(Debug, Clone, Default)]
pub struct AccessHandle(Arc<Mutex<AccessLists>>);

impl AccessHandle {
    /// A handle over `lists`.
    #[must_use]
    pub fn new(lists: AccessLists) -> Self {
        Self(Arc::new(Mutex::new(lists)))
    }

    /// Loads the four files from `dir` into a fresh handle.
    ///
    /// # Errors
    /// Whatever [`AccessLists::load`] reports.
    pub fn load(dir: &Path) -> Result<Self, Error> {
        Ok(Self::new(AccessLists::load(dir)?))
    }

    /// Runs `f` against the locked lists.
    pub fn with<R>(&self, f: impl FnOnce(&mut AccessLists) -> R) -> R {
        let mut guard = self.0.lock().expect("access list lock poisoned");
        f(&mut guard)
    }

    /// [`AccessLists::may_join`] against the current wall clock.
    #[must_use]
    pub fn may_join(&self, uuid: Uuid, ip: Option<IpAddr>, online: usize) -> Result<(), JoinRefusal> {
        let now = lodestone_time::epoch_duration().as_secs() as i64;
        self.with(|lists| lists.may_join(uuid, ip, online, now))
    }

    /// [`AccessLists::permission_level`].
    #[must_use]
    pub fn permission_level(&self, uuid: Uuid) -> u8 {
        self.with(|lists| lists.permission_level(uuid))
    }

    /// [`AccessLists::command_permission_level`] — what `crate::server` resolves
    /// once, at the Play handoff, to gate the built-in command tree.
    #[must_use]
    pub fn command_permission_level(&self, uuid: Uuid) -> u8 {
        self.with(|lists| lists.command_permission_level(uuid))
    }

    /// Turns whitelist enforcement on or off.
    pub fn set_whitelist_enabled(&self, enabled: bool) {
        self.with(|lists| lists.set_whitelist_enabled(enabled));
    }

    /// Names the world owner — see [`AccessLists::set_owner`].
    pub fn set_owner(&self, owner: Option<Uuid>) {
        self.with(|lists| lists.set_owner(owner));
    }

    /// Writes the four files to `dir`.
    ///
    /// # Errors
    /// Whatever [`AccessLists::save`] reports.
    pub fn save(&self, dir: &Path) -> Result<(), Error> {
        self.with(|lists| lists.save(dir))
    }
}

fn ban_from_json(entry: &serde_json::Value) -> BanEntry {
    BanEntry {
        name: json_str(entry, "name"),
        source: match json_str(entry, "source") {
            s if s.is_empty() => "(Unknown)".to_string(),
            s => s,
        },
        created: json_str(entry, "created"),
        expires: match json_str(entry, "expires") {
            s if s.is_empty() => "forever".to_string(),
            s => s,
        },
        reason: match json_str(entry, "reason") {
            s if s.is_empty() => "Banned by an operator.".to_string(),
            s => s,
        },
    }
}

fn ban_to_json(ban: &BanEntry) -> serde_json::Value {
    serde_json::json!({
        "name": ban.name,
        "created": ban.created,
        "source": ban.source,
        "expires": ban.expires,
        "reason": ban.reason,
    })
}

fn json_str(entry: &serde_json::Value, key: &str) -> String {
    entry
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn json_uuid(entry: &serde_json::Value, key: &str) -> Option<Uuid> {
    Uuid::parse_str(entry.get(key)?.as_str()?).ok()
}

/// Reads one file as vanilla's array of objects. A missing file is an empty list.
fn read_array(path: &Path) -> Result<Vec<serde_json::Value>, Error> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| Error::Malformed {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    match value {
        serde_json::Value::Array(entries) => Ok(entries),
        other => Err(Error::Malformed {
            path: path.to_path_buf(),
            detail: format!("expected a JSON array, found {other}"),
        }),
    }
}

fn write_array(path: &Path, entries: &[serde_json::Value]) -> Result<(), Error> {
    let body = serde_json::to_string_pretty(entries).unwrap_or_else(|_| "[]".to_string());
    std::fs::write(path, body).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// **Vanilla's refusal order is observable and this is it**: a player who is
    /// both banned and not whitelisted is told they are banned, not that they are
    /// unlisted (`PlayerList.canPlayerLogin` checks the ban first). A test written
    /// against the wrong precedence would look right and assert nothing.
    #[test]
    fn refusal_order_is_vanillas() {
        let mut lists = AccessLists::new();
        lists.set_whitelist_enabled(true);
        lists.ban(
            uuid(1),
            BanEntry::permanent("bad", "op", "griefing"),
        );
        assert_eq!(
            lists.may_join(uuid(1), None, 0, 0),
            Err(JoinRefusal::Banned("griefing".to_string()))
        );
        // Not banned, not whitelisted.
        assert_eq!(
            lists.may_join(uuid(2), None, 0, 0),
            Err(JoinRefusal::NotWhitelisted)
        );
        // Whitelisted and clear.
        lists.whitelist_add(uuid(2), "good");
        assert_eq!(lists.may_join(uuid(2), None, 0, 0), Ok(()));

        // An IP ban is checked *after* the whitelist, so a whitelisted player on a
        // banned address gets the IP message.
        let ip: IpAddr = "10.0.0.9".parse().unwrap();
        lists.ban_ip(ip, BanEntry::permanent("", "op", "vpn"));
        assert_eq!(
            lists.may_join(uuid(2), Some(ip), 0, 0),
            Err(JoinRefusal::IpBanned("vpn".to_string()))
        );

        // The limit is last, and the owner is past it.
        lists.pardon_ip(ip);
        lists.set_max_players(Some(1));
        assert_eq!(
            lists.may_join(uuid(2), None, 1, 0),
            Err(JoinRefusal::ServerFull)
        );
        lists.set_owner(Some(uuid(2)));
        assert_eq!(lists.may_join(uuid(2), None, 1, 0), Ok(()));
    }

    /// A timed ban stops applying once its `expires` passes, and vanilla's exact
    /// date format is what carries it. The expected timestamps come from the
    /// format's definition, not from our own formatter.
    #[test]
    fn timed_bans_expire_and_forever_does_not() {
        // 2024-01-01 00:00:00 UTC = 1,704,067,200 (19,723 days since the epoch).
        assert_eq!(parse_ban_expiry("2024-01-01 00:00:00 +0000"), Some(1_704_067_200));
        // The same instant written in +0100 is one hour earlier in UTC terms.
        assert_eq!(parse_ban_expiry("2024-01-01 01:00:00 +0100"), Some(1_704_067_200));
        assert_eq!(parse_ban_expiry("forever"), None);
        assert_eq!(parse_ban_expiry(""), None);

        let mut lists = AccessLists::new();
        let timed = BanEntry {
            expires: "2024-01-01 00:00:00 +0000".to_string(),
            ..BanEntry::permanent("temp", "op", "cooling off")
        };
        lists.ban(uuid(3), timed);
        assert!(lists.may_join(uuid(3), None, 0, 1_704_067_199).is_err(), "one second before expiry");
        assert!(lists.may_join(uuid(3), None, 0, 1_704_067_200).is_ok(), "at expiry");

        // An unreadable expiry keeps the ban rather than dropping it.
        let mut lists = AccessLists::new();
        lists.ban(
            uuid(4),
            BanEntry {
                expires: "not a date".to_string(),
                ..BanEntry::permanent("weird", "op", "?")
            },
        );
        assert!(lists.may_join(uuid(4), None, 0, i64::MAX).is_err());
    }

    /// All four files round-trip through vanilla's field names, and a **malformed**
    /// file is an error rather than an empty list — the failure that would silently
    /// remove every operator.
    #[test]
    fn files_round_trip_and_malformed_is_an_error() {
        let dir = std::env::temp_dir().join("lodestone-access-r7q2");
        let _ = std::fs::remove_dir_all(&dir);

        let mut lists = AccessLists::new();
        lists.op(uuid(1), "admin", 4);
        lists.whitelist_add(uuid(2), "guest");
        lists.ban(uuid(3), BanEntry::permanent("cheater", "admin", "x-ray"));
        lists.ban_ip("192.0.2.5".parse().unwrap(), BanEntry::permanent("", "admin", "proxy"));
        lists.save(&dir).expect("save");

        let back = AccessLists::load(&dir).expect("load");
        assert_eq!(back.permission_level(uuid(1)), 4);
        assert_eq!(back.whitelisted(), vec![uuid(2)]);
        assert_eq!(back.bans().len(), 1);
        assert_eq!(back.bans()[0].1.reason, "x-ray");
        assert_eq!(
            back.may_join(uuid(3), None, 0, 0),
            Err(JoinRefusal::Banned("x-ray".to_string()))
        );
        assert_eq!(
            back.may_join(uuid(9), Some("192.0.2.5".parse().unwrap()), 0, 0),
            Err(JoinRefusal::IpBanned("proxy".to_string()))
        );
        // `whitelist_enabled` is a *property*, not the file's presence, so it does
        // not survive a save/load — vanilla keeps it in server.properties.
        assert!(!back.whitelist_enabled());

        std::fs::write(dir.join(OPS_FILE), "{ not an array }").expect("write");
        assert!(matches!(
            AccessLists::load(&dir),
            Err(Error::Malformed { .. })
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing file is an empty list, which is every world's first start.
    #[test]
    fn a_world_with_no_files_admits_everyone() {
        let dir = std::env::temp_dir().join("lodestone-access-r7q2-empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let lists = AccessLists::load(&dir).expect("load");
        assert_eq!(lists.may_join(uuid(1), None, 0, 0), Ok(()));
        assert_eq!(lists.permission_level(uuid(1)), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `format_now` and `parse_ban_expiry` are inverses, so a ban this server
    /// issues can be read back by this server — and by vanilla, since the format is
    /// vanilla's.
    #[test]
    fn issued_bans_are_readable() {
        let now = format_now();
        let parsed = parse_ban_expiry(&now).expect("our own format must parse");
        let real = lodestone_time::epoch_duration().as_secs() as i64;
        assert!(
            (parsed - real).abs() <= 2,
            "format_now/parse round-trip drifted: {parsed} vs {real} ({now})"
        );
    }
}
