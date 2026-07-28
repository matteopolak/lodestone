//! The multiplayer server list: the entries themselves, and where they live on
//! disk.
//!
//! ## What it is
//!
//! A small ordered list of [`ServerEntry`] (label + host + optional port),
//! persisted as JSON so it survives a restart. This is the model only — no
//! networking (see [`super::status`]) and no drawing (see [`super::render`]).
//!
//! ## Where the file goes
//!
//! There was **no user-state directory helper anywhere in this workspace** when
//! this was written — nothing reads or writes a config/data dir, so there was no
//! existing convention to follow and this file establishes one. It deliberately
//! uses the platform's own location rather than inventing a dotfile:
//!
//! | platform | directory |
//! |---|---|
//! | macOS   | `~/Library/Application Support/lodestone` |
//! | Windows | `%APPDATA%\lodestone` |
//! | other   | `$XDG_DATA_HOME/lodestone`, else `~/.local/share/lodestone` |
//!
//! `LODESTONE_DATA_DIR` overrides all of it. That override is what makes this
//! testable: a test that wrote to the real directory would clobber the
//! developer's own server list, so [`ServerList::load_from`]/[`ServerList::save_to`]
//! take an explicit path and the environment is only consulted by
//! [`servers_path`].
//!
//! ## How to change it
//!
//! The on-disk shape is a JSON **array of objects**, hand-built through
//! `serde_json::Value` rather than `derive(Serialize)` — `lodestone-shell`
//! depends on `serde_json` but not on `serde` itself, so a derive would need a
//! new dependency. If you add a field, give it a default in [`entry_from_json`]
//! so an older file still loads; [`ServerList::load_from`] treats a malformed
//! file as *empty*, never as an error, because a corrupt list must not stop the
//! game from launching.

use std::path::{Path, PathBuf};

/// The port assumed when an entry does not pin one.
///
/// Matches `lodestone_net::DEFAULT_PORT`. It is restated rather than imported
/// because `lodestone-shell` does not (yet) depend on `lodestone-net` — see the
/// note on [`super::status`].
pub const DEFAULT_PORT: u16 = 25565;

/// Longest accepted server label, in characters. Keeps one pathological entry
/// from making the list unreadable.
pub const MAX_NAME_CHARS: usize = 48;

/// One saved server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerEntry {
    /// Display label, e.g. `"Home survival"`.
    pub name: String,
    /// Hostname or IP literal, without a port.
    pub host: String,
    /// Explicit port, or `None` to use SRV resolution then [`DEFAULT_PORT`].
    ///
    /// `None` is **not** the same as `Some(25565)`: vanilla only performs the
    /// `_minecraft._tcp` SRV lookup when the user did not pin a port, so
    /// collapsing these two makes a large slice of real servers unreachable.
    pub port: Option<u16>,
}

impl ServerEntry {
    /// A new entry, with the label and host trimmed.
    #[must_use]
    pub fn new(name: impl Into<String>, host: impl Into<String>, port: Option<u16>) -> Self {
        let mut e = Self {
            name: name.into().trim().to_string(),
            host: host.into().trim().to_string(),
            port,
        };
        if e.name.chars().count() > MAX_NAME_CHARS {
            e.name = e.name.chars().take(MAX_NAME_CHARS).collect();
        }
        e
    }

    /// Parses a user-typed `host` or `host:port` into an entry field pair.
    ///
    /// A bare IPv6 literal is *not* supported here (it would be ambiguous with
    /// the port separator); bracketed `[::1]:25565` is. Anything after a colon
    /// that is not a valid port is treated as part of the host, so a typo shows
    /// up as a failed connection rather than a silently different port.
    #[must_use]
    pub fn split_host_port(input: &str) -> (String, Option<u16>) {
        let s = input.trim();
        if let Some(rest) = s.strip_prefix('[') {
            // Bracketed IPv6, optionally followed by :port.
            if let Some((addr, tail)) = rest.split_once(']') {
                let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
                return (addr.to_string(), port);
            }
        }
        match s.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && !h.contains(':') => match p.parse::<u16>() {
                Ok(port) => (h.to_string(), Some(port)),
                Err(_) => (s.to_string(), None),
            },
            _ => (s.to_string(), None),
        }
    }

    /// `host` or `host:port`, as the list row shows it.
    #[must_use]
    pub fn address_label(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{}", self.host, p),
            None => self.host.clone(),
        }
    }

    /// The port to actually dial when connecting.
    #[must_use]
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(DEFAULT_PORT)
    }

    /// Whether this entry is complete enough to save/connect.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.host.is_empty()
    }
}

/// An ordered, persistable list of saved servers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerList {
    entries: Vec<ServerEntry>,
}

impl ServerList {
    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The entries, in display order.
    #[must_use]
    pub fn entries(&self) -> &[ServerEntry] {
        &self.entries
    }

    /// Number of saved servers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the list has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry at `index`, if any.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&ServerEntry> {
        self.entries.get(index)
    }

    /// Appends an entry, returning its index. Invalid entries (no host) are
    /// rejected so the list can never hold a row that cannot be dialed.
    pub fn add(&mut self, entry: ServerEntry) -> Option<usize> {
        if !entry.is_valid() {
            return None;
        }
        self.entries.push(entry);
        Some(self.entries.len() - 1)
    }

    /// Replaces the entry at `index`. Returns whether it applied.
    pub fn update(&mut self, index: usize, entry: ServerEntry) -> bool {
        if !entry.is_valid() {
            return false;
        }
        match self.entries.get_mut(index) {
            Some(slot) => {
                *slot = entry;
                true
            }
            None => false,
        }
    }

    /// Removes the entry at `index`, returning it.
    pub fn remove(&mut self, index: usize) -> Option<ServerEntry> {
        (index < self.entries.len()).then(|| self.entries.remove(index))
    }

    // -- persistence ------------------------------------------------------

    /// Serialises the list to the on-disk JSON form.
    #[must_use]
    pub fn to_json(&self) -> String {
        let arr: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|e| {
                let mut obj = serde_json::Map::new();
                obj.insert("name".into(), e.name.clone().into());
                obj.insert("host".into(), e.host.clone().into());
                if let Some(p) = e.port {
                    obj.insert("port".into(), p.into());
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        // Pretty-printed: this is a file humans hand-edit.
        serde_json::to_string_pretty(&serde_json::Value::Array(arr))
            .unwrap_or_else(|_| "[]".to_string())
    }

    /// Parses the on-disk JSON form. Unknown fields are ignored and malformed
    /// *rows* are skipped, so one bad entry does not lose the rest.
    #[must_use]
    pub fn from_json(text: &str) -> Self {
        let Ok(serde_json::Value::Array(items)) = serde_json::from_str(text) else {
            return Self::new();
        };
        Self {
            entries: items.iter().filter_map(entry_from_json).collect(),
        }
    }

    /// Loads from `path`. A missing or unreadable file is an **empty list**, not
    /// an error: a first run and a corrupt file must both still let the player
    /// into the game.
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path).map_or_else(|_| Self::new(), |t| Self::from_json(&t))
    }

    /// Writes to `path`, creating parent directories as needed.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error if the directory cannot be created or
    /// the file cannot be written.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.to_json())
    }
}

/// Decodes one JSON row, or `None` if it is not a usable entry.
fn entry_from_json(v: &serde_json::Value) -> Option<ServerEntry> {
    let obj = v.as_object()?;
    let host = obj.get("host")?.as_str()?.trim();
    if host.is_empty() {
        return None;
    }
    let name = obj
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(host);
    let port = obj
        .get("port")
        .and_then(serde_json::Value::as_u64)
        .and_then(|p| u16::try_from(p).ok());
    Some(ServerEntry::new(name, host, port))
}

/// The directory Lodestone keeps user state in.
///
/// `LODESTONE_DATA_DIR` overrides the platform default; see the module docs.
#[must_use]
pub fn data_dir() -> PathBuf {
    data_dir_from(
        std::env::var_os("LODESTONE_DATA_DIR").as_deref(),
        std::env::var_os("HOME").as_deref(),
        std::env::var_os("APPDATA").as_deref(),
        std::env::var_os("XDG_DATA_HOME").as_deref(),
    )
}

/// The platform lookup [`data_dir`] performs, with the environment injected.
///
/// Split out purely so it is **testable without mutating the process
/// environment**. `std::env::set_var` is `unsafe` from the 2024 edition and this
/// workspace is `deny(unsafe_code)`, but the deeper reason is that it is shared
/// mutable state: the whole test binary is one process, so a test that sets
/// `HOME` or `LODESTONE_DATA_DIR` is racing every other test that reads it.
fn data_dir_from(
    override_dir: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    appdata: Option<&std::ffi::OsStr>,
    xdg: Option<&std::ffi::OsStr>,
) -> PathBuf {
    if let Some(dir) = override_dir {
        return PathBuf::from(dir);
    }
    let home = home.map(PathBuf::from);
    if cfg!(target_os = "macos") {
        if let Some(h) = home {
            return h.join("Library/Application Support/lodestone");
        }
    } else if cfg!(target_os = "windows") {
        if let Some(app) = appdata {
            return PathBuf::from(app).join("lodestone");
        }
    } else if let Some(x) = xdg {
        return PathBuf::from(x).join("lodestone");
    } else if let Some(h) = home {
        return h.join(".local/share/lodestone");
    }
    // Last resort: a namespaced directory under the working directory, so the
    // game still starts on a machine with no HOME at all.
    PathBuf::from(".lodestone")
}

/// Full path to the saved server list.
#[must_use]
pub fn servers_path() -> PathBuf {
    data_dir().join("servers.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn sample() -> ServerList {
        let mut list = ServerList::new();
        list.add(ServerEntry::new("Home", "mc.example.com", None));
        list.add(ServerEntry::new("Local", "127.0.0.1", Some(25565)));
        list
    }

    #[test]
    fn add_edit_delete_round_trip() {
        let mut list = sample();
        assert_eq!(list.len(), 2);

        assert!(list.update(0, ServerEntry::new("Renamed", "other.example", Some(25))));
        assert_eq!(list.get(0).unwrap().name, "Renamed");
        assert_eq!(list.get(0).unwrap().port, Some(25));

        let removed = list.remove(0).expect("entry 0 should exist");
        assert_eq!(removed.name, "Renamed");
        assert_eq!(list.len(), 1);
        assert_eq!(list.get(0).unwrap().name, "Local", "order must be preserved");

        // Out-of-range operations are refused rather than panicking.
        assert!(!list.update(9, ServerEntry::new("x", "h", None)));
        assert!(list.remove(9).is_none());
    }

    #[test]
    fn an_entry_without_a_host_is_refused() {
        // The list must never hold a row that cannot be dialed.
        let mut list = ServerList::new();
        assert_eq!(list.add(ServerEntry::new("nameless", "   ", None)), None);
        assert!(list.is_empty());
        list.add(ServerEntry::new("ok", "h", None));
        assert!(!list.update(0, ServerEntry::new("broken", "", None)));
        assert_eq!(list.get(0).unwrap().host, "h");
    }

    #[test]
    fn json_round_trips_including_the_absent_port() {
        // `None` vs `Some(25565)` is load-bearing: only `None` triggers SRV.
        let list = sample();
        let back = ServerList::from_json(&list.to_json());
        assert_eq!(back, list);
        assert_eq!(back.get(0).unwrap().port, None);
        assert_eq!(back.get(1).unwrap().port, Some(25565));
        assert!(
            !list.to_json().contains("\"port\": 0"),
            "an absent port must be omitted, not written as 0"
        );
    }

    #[test]
    fn a_corrupt_file_loads_as_empty_rather_than_failing() {
        // A corrupt server list must not stop the game from launching.
        assert!(ServerList::from_json("}{ not json").is_empty());
        assert!(ServerList::from_json("{\"not\":\"an array\"}").is_empty());
        assert!(ServerList::load_from(Path::new("/nonexistent/servers.json")).is_empty());
    }

    #[test]
    fn one_bad_row_does_not_lose_the_others() {
        let json = r#"[
            {"name":"good","host":"a.example"},
            {"name":"hostless"},
            "not an object",
            {"name":"also good","host":"b.example","port":1234}
        ]"#;
        let list = ServerList::from_json(json);
        assert_eq!(list.len(), 2, "{:?}", list.entries());
        assert_eq!(list.get(1).unwrap().port, Some(1234));
    }

    #[test]
    fn a_row_without_a_name_falls_back_to_its_host() {
        let list = ServerList::from_json(r#"[{"host":"bare.example"}]"#);
        assert_eq!(list.get(0).unwrap().name, "bare.example");
    }

    #[test]
    fn save_and_load_through_a_real_file() {
        let dir = std::env::temp_dir().join(format!("lodestone-servers-{}", std::process::id()));
        let path = dir.join("nested/servers.json");
        let list = sample();
        list.save_to(&path).expect("save should create parents");
        assert_eq!(ServerList::load_from(&path), list);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_port_splitting_handles_the_shapes_users_type() {
        assert_eq!(
            ServerEntry::split_host_port("mc.example.com"),
            ("mc.example.com".into(), None)
        );
        assert_eq!(
            ServerEntry::split_host_port(" mc.example.com:25566 "),
            ("mc.example.com".into(), Some(25566))
        );
        // A non-numeric or out-of-range "port" stays part of the host, so a
        // typo fails visibly instead of silently dialing a different port.
        assert_eq!(
            ServerEntry::split_host_port("mc.example.com:notaport"),
            ("mc.example.com:notaport".into(), None)
        );
        assert_eq!(
            ServerEntry::split_host_port("mc.example.com:99999"),
            ("mc.example.com:99999".into(), None)
        );
        // Bracketed IPv6, with and without a port.
        assert_eq!(
            ServerEntry::split_host_port("[::1]:25565"),
            ("::1".into(), Some(25565))
        );
        assert_eq!(ServerEntry::split_host_port("[::1]"), ("::1".into(), None));
        // A bare IPv6 literal must not have its last group eaten as a port.
        assert_eq!(
            ServerEntry::split_host_port("fe80::1:2:3"),
            ("fe80::1:2:3".into(), None)
        );
    }

    #[test]
    fn labels_and_ports_render_as_the_list_shows_them() {
        let e = ServerEntry::new("n", "h", None);
        assert_eq!(e.address_label(), "h");
        assert_eq!(e.effective_port(), DEFAULT_PORT);
        let e = ServerEntry::new("n", "h", Some(1));
        assert_eq!(e.address_label(), "h:1");
        assert_eq!(e.effective_port(), 1);
    }

    #[test]
    fn overlong_names_are_truncated() {
        let e = ServerEntry::new("x".repeat(500), "h", None);
        assert_eq!(e.name.chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn the_data_dir_honours_the_override_and_is_namespaced_otherwise() {
        // The override is what keeps tests off the developer's real list. It is
        // exercised through `data_dir_from` rather than by setting the variable:
        // `set_var` is `unsafe` under this workspace's `deny(unsafe_code)`, and
        // it is also *shared mutable state* — tests share one process, so
        // another test reading HOME concurrently would observe the mutation.
        let o = OsString::from("/tmp/lodestone-test-dir");
        assert_eq!(
            data_dir_from(Some(&o), None, None, None),
            PathBuf::from("/tmp/lodestone-test-dir")
        );

        // Without the override we must still land somewhere plausible and
        // per-user, on this platform and on the others.
        let home = OsString::from("/home/someone");
        let d = data_dir_from(None, Some(&home), None, None);
        assert!(
            d.starts_with(&home),
            "the default must live under HOME: {d:?}"
        );
        assert!(
            d.to_string_lossy().contains("lodestone"),
            "data dir should be namespaced: {d:?}"
        );

        // The real accessor agrees on the namespace, whatever this machine's
        // environment happens to be.
        assert!(data_dir().to_string_lossy().contains("lodestone"));
        assert!(servers_path().ends_with("servers.json"));
    }

    #[test]
    fn every_platform_branch_lands_somewhere_per_user() {
        // XDG wins over the HOME fallback on non-mac/non-Windows, and each
        // branch is namespaced. `cfg!` picks one arm at compile time, so only
        // the arm for *this* platform is asserted concretely; the rest is a
        // shape check that still fails if a branch returns the bare env value.
        let xdg = OsString::from("/xdg/data");
        let home = OsString::from("/home/someone");
        let appdata = OsString::from(r"C:\Users\someone\AppData\Roaming");
        let d = data_dir_from(None, Some(&home), Some(&appdata), Some(&xdg));
        assert_ne!(
            d,
            PathBuf::from(&xdg),
            "must append the app name, not use the base dir directly"
        );
        assert!(d.to_string_lossy().contains("lodestone"), "{d:?}");
        if cfg!(target_os = "macos") {
            assert_eq!(d, PathBuf::from("/home/someone/Library/Application Support/lodestone"));
        } else if cfg!(target_os = "windows") {
            assert_eq!(d, PathBuf::from(r"C:\Users\someone\AppData\Roaming").join("lodestone"));
        } else {
            assert_eq!(d, PathBuf::from("/xdg/data/lodestone"));
        }

        // Nothing at all in the environment must still yield a usable path
        // rather than an empty one — the game has to start.
        let last = data_dir_from(None, None, None, None);
        assert!(!last.as_os_str().is_empty());
        assert!(last.to_string_lossy().contains("lodestone"), "{last:?}");
    }
}
