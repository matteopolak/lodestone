//! `server.properties` — vanilla's `key=value` config file for a dedicated
//! server, and the seam that turns it into what this crate actually reads.
//!
//! ## What it is
//!
//! [`RawProperties`] is a faithful-enough `java.util.Properties` reader/writer
//! (see "How it works" for the subset it covers); [`ServerProperties`] is the
//! typed subset of vanilla's real key set this crate's own dedicated-hosting
//! path consumes, built on top of one. [`ServerProperties::load_or_create`] is
//! the one entry point `lodestone-dedicated-server`'s `main` calls.
//!
//! ## Ground truth, not a guess
//!
//! Every key name and every default value here is transcribed from
//! `DedicatedServerProperties.java` in this repo's own pinned 26.2 decompile
//! (`.cache/mc/26.2/src/net/minecraft/server/dedicated/`), not from an older
//! Minecraft version's documentation. Two things that transcription caught
//! that an assumption would not have:
//!
//! * **There is no `pvp` key in 26.2.** Older server.properties files carry
//!   one; `DedicatedServerProperties`'s field list does not, and neither does
//!   the real file this repo's own oracle already runs against
//!   (`.cache/mc/26.2/server.properties` has no `pvp`/`allow-nether`/
//!   `spawn-monsters`/`spawn-npcs`/`spawn-animals` line either). This module
//!   therefore does not model one — a `pvp=` line typed into the file by hand
//!   is preserved verbatim as an unknown key (see below) and does nothing,
//!   same as it would against a real 26.2 server.
//! * **`online-mode`'s real default is `true`.** A hand-authored default
//!   would be tempting to leave `false` (the safer footgun to ship); vanilla's
//!   own default is online, and [`default_raw`] matches it.
//!
//! ## How it works
//!
//! [`RawProperties`] preserves **every** key in insertion order, typed or not
//! — a datapack manager, hosting panel or future version of this crate that
//! writes a key this module has never heard of survives a round trip through
//! [`ServerProperties::load_or_create`]/[`ServerProperties::save`] unchanged.
//! Only the ~20 keys this crate's dedicated-hosting path actually reads grow a
//! named field; everything else (`management-server-*`, `resource-pack-*`,
//! `text-filtering-*`, …) stays in the raw store, written back with vanilla's
//! own default the first time and passed through byte-for-byte after that.
//!
//! Parsing covers the subset every real server.properties file in the wild
//! actually uses: `#`/`!` comment lines, blank lines, and `key=value` (or
//! `key:value`) entries with `\\`, `\:`, `\=`, `\#`, `\!`, `\n`, `\t`, `\r`,
//! `\f` escapes and a single escaped leading space. It does **not** implement
//! `java.util.Properties`' line-continuation (`\` at end of line) or
//! `\uXXXX` unicode escapes — no key this crate generates or vanilla ships
//! needs either, and Mojang's own writer only reaches for line-continuation
//! on values longer than a terminal line, which no property here is.
//!
//! ## How to change it
//!
//! Reading a new key: add a field to [`ServerProperties`], read it in
//! [`ServerProperties::from_raw`] with [`RawProperties::get`], and write its
//! default into [`default_raw`] (copy the value from the decompiled source,
//! not from memory — that is exactly the mistake this module's own doc found
//! once already, over `pvp`). Say in the crate that calls it whether the new
//! field actually changes behaviour or is accepted-and-ignored; this module
//! only parses, it does not grade its own keys.
//!
//! ## Configuration
//!
//! One file, `server.properties`, in the server's root directory (the same
//! directory `eula.txt` and the four `crate::access` JSON files live in).
//!
//! ## Dependencies
//!
//! `std::fs` only. Native-only ([`target_arch = "wasm32"`] is never a
//! dedicated server), matching `crate::region_source`/`crate::rcon`'s own gate.

use std::path::Path;

use lodestone_model::{Difficulty, GameMode};

/// One `java.util.Properties`-shaped file: every key in insertion order,
/// typed or not. See this module's own doc for the escape/parsing subset
/// covered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawProperties {
    entries: Vec<(String, String)>,
}

impl RawProperties {
    /// An empty store — every [`get`](Self::get) on it answers `None`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses `text` into an ordered key/value store. Never fails: a
    /// malformed line (no `=`/`:` and no whitespace to split on) is skipped
    /// rather than rejected, matching `java.util.Properties.load` treating an
    /// unparsable line as a key with an empty value only when *something*
    /// splits it — a line that is truly just noise is simply not a property.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
                continue;
            }
            if let Some((key, value)) = split_entry(trimmed) {
                set_entry(&mut entries, key, value);
            }
        }
        Self { entries }
    }

    /// The value for `key`, unescaped, or `None` if the file has no such
    /// entry.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Inserts or overwrites `key`'s value, preserving its original position
    /// if it already had one and appending it (in call order) otherwise —
    /// the same shape [`default_raw`] relies on to keep a fresh file in
    /// vanilla's own alphabetical order.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        set_entry(&mut self.entries, key.into(), value.into());
    }

    /// Renders back to `java.util.Properties.store` text: a header comment,
    /// then one `key=value` line per entry with vanilla's own escaping
    /// (`\\`, `\:`, `\=`, and a leading space escaped as `\ `).
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::from("#Minecraft server properties\n");
        for (key, value) in &self.entries {
            out.push_str(&escape(key, true));
            out.push('=');
            out.push_str(&escape(value, false));
            out.push('\n');
        }
        out
    }
}

/// First unescaped `=` or `:` splits `line` into `(key, value)`, both
/// trimmed of surrounding whitespace and unescaped. Whitespace with no `=`/
/// `:` at all also splits (`java.util.Properties` allows `key value`); a line
/// with none of the three is not a property.
fn split_entry(line: &str) -> Option<(String, String)> {
    let bytes = line.as_bytes();
    let mut escaped = false;
    let mut split_at = None;
    let mut first_ws = None;
    for (i, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match b {
            b'\\' => escaped = true,
            b'=' | b':' => {
                split_at = Some(i);
                break;
            }
            b' ' | b'\t' | b'\x0c' if first_ws.is_none() => first_ws = Some(i),
            _ => {}
        }
    }
    let idx = split_at.or(first_ws)?;
    let key = unescape(line[..idx].trim());
    let rest = &line[idx + 1..];
    // A `key value` split (no `=`/`:`) must not eat a leading `=`/`:` that
    // follows the whitespace (`key = value`); a `=`/`:` split already
    // consumed its own separator.
    let rest = if split_at.is_none() {
        rest.trim_start_matches([' ', '\t'])
    } else {
        rest
    };
    let value = unescape(rest.trim_start_matches([' ', '\t']));
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('f') => out.push('\x0c'),
            Some(other) => out.push(other), // `\:`, `\=`, `\\`, `\#`, `\!`, `\ `, ...
            None => out.push('\\'),
        }
    }
    out
}

/// Escapes `:`, `=`, `\`, and (for a key) leading whitespace, plus `\n`/`\t`
/// anywhere — the subset `java.util.Properties.store` actually emits for the
/// ASCII content every key/value in this file is.
fn escape(s: &str, is_key: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            ':' => out.push_str("\\:"),
            '=' => out.push_str("\\="),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            ' ' if is_key || i == 0 => out.push_str("\\ "),
            other => out.push(other),
        }
    }
    out
}

/// Overwrites `key`'s value in place if it is already present, else appends
/// a new `(key, value)` pair — the shared body behind
/// [`RawProperties::parse`] and [`RawProperties::set`].
fn set_entry(entries: &mut Vec<(String, String)>, key: String, value: String) {
    if let Some(slot) = entries.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = value;
    } else {
        entries.push((key, value));
    }
}

/// vanilla's real key set and real defaults, alphabetically ordered exactly
/// as `.cache/mc/26.2/server.properties` (and every vanilla-written file) is
/// — see this module's own doc comment on why that ordering and every value
/// below is transcribed from `DedicatedServerProperties.java`, not assumed.
#[must_use]
pub fn default_raw() -> RawProperties {
    let mut raw = RawProperties::new();
    for (key, value) in DEFAULTS {
        raw.set(*key, *value);
    }
    raw
}

/// `(key, default)` pairs, in `DedicatedServerProperties.java`'s own
/// declaration order (which is also the real file's alphabetical order).
/// `management-server-secret` here is `""` rather than vanilla's randomly
/// generated `SecurityConfig.generateSecretKey()` — this crate implements no
/// management server to protect with it, so a fixed empty default is honest
/// about that rather than manufacturing a secret nothing checks.
const DEFAULTS: &[(&str, &str)] = &[
    ("accepts-transfers", "false"),
    ("allow-flight", "false"),
    ("broadcast-console-to-ops", "true"),
    ("broadcast-rcon-to-ops", "true"),
    ("bug-report-link", ""),
    ("chat-spam-threshold-seconds", "10"),
    ("command-spam-threshold-seconds", "10"),
    ("difficulty", "easy"),
    ("enable-code-of-conduct", "false"),
    ("enable-jmx-monitoring", "false"),
    ("enable-query", "false"),
    ("enable-rcon", "false"),
    ("enable-status", "true"),
    ("enforce-secure-profile", "true"),
    ("enforce-whitelist", "false"),
    ("entity-broadcast-range-percentage", "100"),
    ("force-gamemode", "false"),
    ("function-permission-level", "2"),
    ("gamemode", "survival"),
    ("generate-structures", "true"),
    ("generator-settings", "{}"),
    ("hardcore", "false"),
    ("hide-online-players", "false"),
    ("initial-disabled-packs", ""),
    ("initial-enabled-packs", "vanilla"),
    ("level-name", "world"),
    ("level-seed", ""),
    ("level-type", "minecraft:normal"),
    ("log-ips", "true"),
    ("management-server-allowed-origins", ""),
    ("management-server-enabled", "false"),
    ("management-server-host", "localhost"),
    ("management-server-port", "0"),
    ("management-server-secret", ""),
    ("management-server-tls-enabled", "true"),
    ("management-server-tls-keystore", ""),
    ("management-server-tls-keystore-password", ""),
    ("max-chained-neighbor-updates", "1000000"),
    ("max-players", "20"),
    ("max-tick-time", "60000"),
    ("max-world-size", "29999984"),
    ("motd", "A Minecraft Server"),
    ("network-compression-threshold", "256"),
    ("online-mode", "true"),
    ("op-permission-level", "4"),
    ("pause-when-empty-seconds", "60"),
    ("player-idle-timeout", "0"),
    ("prevent-proxy-connections", "false"),
    ("query.port", "25565"),
    ("rate-limit", "0"),
    ("rcon.password", ""),
    ("rcon.port", "25575"),
    ("region-file-compression", "deflate"),
    ("require-resource-pack", "false"),
    ("resource-pack", ""),
    ("resource-pack-id", ""),
    ("resource-pack-prompt", ""),
    ("resource-pack-sha1", ""),
    ("server-ip", ""),
    ("server-port", "25565"),
    ("simulation-distance", "10"),
    ("spawn-protection", "16"),
    ("status-heartbeat-interval", "0"),
    ("sync-chunk-writes", "true"),
    ("text-filtering-config", ""),
    ("text-filtering-version", "0"),
    ("use-native-transport", "true"),
    ("view-distance", "10"),
    ("white-list", "false"),
];

/// The typed subset of `server.properties` this crate's dedicated-hosting
/// path actually consumes. See this module's own doc comment for which keys
/// those are and which real vanilla keys are deliberately left un-typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerProperties {
    /// `online-mode` — real value, real default (`true`).
    pub online_mode: bool,
    /// `motd`.
    pub motd: String,
    /// `level-name` — the world's subdirectory under the server root.
    pub level_name: String,
    /// `level-seed`, unparsed. Feed it to
    /// `lodestone_worldgen::hash::java_string_hash` the same way
    /// `WorldOptions.parseSeed`'s catch arm does; empty means "random".
    pub level_seed: String,
    /// `level-type`. `minecraft:normal`/`minecraft:large_biomes`/
    /// `minecraft:amplified` are real; anything else (including
    /// `minecraft:flat`, which needs a second key's JSON this module does not
    /// parse) falls back to normal — the caller is expected to log that.
    pub level_type: String,
    /// `gamemode` — the default mode a new player joins in.
    pub gamemode: GameMode,
    /// `difficulty`.
    pub difficulty: Difficulty,
    /// `max-players`.
    pub max_players: i32,
    /// `view-distance`.
    pub view_distance: i32,
    /// `simulation-distance`.
    pub simulation_distance: i32,
    /// `spawn-protection`. Parsed and preserved on save; **no enforcement
    /// exists in this crate** — see `docs/dedicated-server.md`'s
    /// accepted-and-ignored table.
    pub spawn_protection: i32,
    /// `server-port`.
    pub server_port: u16,
    /// `server-ip`. Empty means "every interface", matching vanilla's own
    /// `ServerConnectionListener` binding `InetAddress` only when this is
    /// non-empty.
    pub server_ip: String,
    /// `white-list`.
    pub white_list: bool,
    /// `enable-rcon`.
    pub enable_rcon: bool,
    /// `rcon.port`.
    pub rcon_port: u16,
    /// `rcon.password`.
    pub rcon_password: String,
    /// `enable-query`. Parsed and preserved; **not wired** — see
    /// `docs/dedicated-server.md`.
    pub enable_query: bool,
    /// `query.port`. Parsed and preserved; not wired (same reason as
    /// `enable_query` — see the doc above).
    pub query_port: u16,
    /// Everything else, including every key above (kept in sync by
    /// [`Self::to_raw`]) and every real vanilla key this struct does not
    /// model — round-tripped verbatim.
    raw: RawProperties,
}

impl ServerProperties {
    /// Reads `path`; if it does not exist, writes vanilla's own defaults
    /// there first (so a fresh server directory ends up with the same file a
    /// fresh vanilla one would) and returns those. Returns `(properties,
    /// created)` — `created` is `true` only on the fresh-write path, the same
    /// shape `crate::region_source::LevelDatHandle::open_or_create` uses.
    ///
    /// # Errors
    ///
    /// An IO error reading an existing file, or writing a missing one.
    pub fn load_or_create(path: &Path) -> std::io::Result<(Self, bool)> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok((Self::from_raw(RawProperties::parse(&text)), false)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let raw = default_raw();
                std::fs::write(path, raw.to_text())?;
                Ok((Self::from_raw(raw), true))
            }
            Err(err) => Err(err),
        }
    }

    /// Writes this configuration back to `path`, vanilla-escaped, with every
    /// unknown key preserved in its original position.
    ///
    /// # Errors
    ///
    /// An IO error writing the file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.to_raw().to_text())
    }

    fn from_raw(raw: RawProperties) -> Self {
        let get = |key: &str, default: &str| raw.get(key).unwrap_or(default).to_string();
        let get_bool = |key: &str, default: bool| {
            raw.get(key)
                .map_or(default, |v| v.eq_ignore_ascii_case("true"))
        };
        let get_i32 = |key: &str, default: i32| {
            raw.get(key)
                .and_then(|v| v.trim().parse::<i32>().ok())
                .unwrap_or(default)
        };
        let get_u16 = |key: &str, default: u16| {
            raw.get(key)
                .and_then(|v| v.trim().parse::<u16>().ok())
                .unwrap_or(default)
        };
        Self {
            online_mode: get_bool("online-mode", true),
            motd: get("motd", "A Minecraft Server"),
            level_name: get("level-name", "world"),
            level_seed: get("level-seed", ""),
            level_type: get("level-type", "minecraft:normal"),
            gamemode: parse_gamemode(raw.get("gamemode")),
            difficulty: parse_difficulty(raw.get("difficulty")),
            max_players: get_i32("max-players", 20),
            view_distance: get_i32("view-distance", 10),
            simulation_distance: get_i32("simulation-distance", 10),
            spawn_protection: get_i32("spawn-protection", 16),
            server_port: get_u16("server-port", 25565),
            server_ip: get("server-ip", ""),
            white_list: get_bool("white-list", false),
            enable_rcon: get_bool("enable-rcon", false),
            rcon_port: get_u16("rcon.port", 25575),
            rcon_password: get("rcon.password", ""),
            enable_query: get_bool("enable-query", false),
            query_port: get_u16("query.port", 25565),
            raw,
        }
    }

    /// Folds every typed field back into a clone of the original raw store
    /// (so an edited struct round-trips its own edits, not the stale text it
    /// was parsed from) and returns it.
    #[must_use]
    fn to_raw(&self) -> RawProperties {
        let mut raw = self.raw.clone();
        raw.set("online-mode", self.online_mode.to_string());
        raw.set("motd", self.motd.clone());
        raw.set("level-name", self.level_name.clone());
        raw.set("level-seed", self.level_seed.clone());
        raw.set("level-type", self.level_type.clone());
        raw.set("gamemode", gamemode_name(self.gamemode));
        raw.set("difficulty", difficulty_name(self.difficulty));
        raw.set("max-players", self.max_players.to_string());
        raw.set("view-distance", self.view_distance.to_string());
        raw.set("simulation-distance", self.simulation_distance.to_string());
        raw.set("spawn-protection", self.spawn_protection.to_string());
        raw.set("server-port", self.server_port.to_string());
        raw.set("server-ip", self.server_ip.clone());
        raw.set("white-list", self.white_list.to_string());
        raw.set("enable-rcon", self.enable_rcon.to_string());
        raw.set("rcon.port", self.rcon_port.to_string());
        raw.set("rcon.password", self.rcon_password.clone());
        raw.set("enable-query", self.enable_query.to_string());
        raw.set("query.port", self.query_port.to_string());
        raw
    }
}

/// `GameType::byName`/`GameType::byId`: name first (case-insensitive), then a
/// numeric id (0–3), defaulting to survival for anything else — matching
/// `DedicatedServerProperties`'s `dispatchNumberOrString`.
fn parse_gamemode(raw: Option<&str>) -> GameMode {
    match raw.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("survival") => GameMode::Survival,
        Some(v) if v.eq_ignore_ascii_case("creative") => GameMode::Creative,
        Some(v) if v.eq_ignore_ascii_case("adventure") => GameMode::Adventure,
        Some(v) if v.eq_ignore_ascii_case("spectator") => GameMode::Spectator,
        Some("0") => GameMode::Survival,
        Some("1") => GameMode::Creative,
        Some("2") => GameMode::Adventure,
        Some("3") => GameMode::Spectator,
        _ => GameMode::Survival,
    }
}

fn gamemode_name(mode: GameMode) -> &'static str {
    match mode {
        GameMode::Survival => "survival",
        GameMode::Creative => "creative",
        GameMode::Adventure => "adventure",
        GameMode::Spectator => "spectator",
    }
}

/// `Difficulty::byName`/`Difficulty::byId`, same shape as [`parse_gamemode`].
fn parse_difficulty(raw: Option<&str>) -> Difficulty {
    match raw.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("peaceful") => Difficulty::Peaceful,
        Some(v) if v.eq_ignore_ascii_case("easy") => Difficulty::Easy,
        Some(v) if v.eq_ignore_ascii_case("normal") => Difficulty::Normal,
        Some(v) if v.eq_ignore_ascii_case("hard") => Difficulty::Hard,
        Some("0") => Difficulty::Peaceful,
        Some("1") => Difficulty::Easy,
        Some("2") => Difficulty::Normal,
        Some("3") => Difficulty::Hard,
        _ => Difficulty::Easy,
    }
}

fn difficulty_name(difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::Peaceful => "peaceful",
        Difficulty::Easy => "easy",
        Difficulty::Normal => "normal",
        Difficulty::Hard => "hard",
    }
}

/// `WorldOptions.parseSeed`: trim, empty is "no seed" (caller picks random),
/// else parse as `i64`, else Java's `String::hashCode` widened to `i64` —
/// [`lodestone_worldgen::hash::java_string_hash`] is the same function
/// `lodestone-shell`'s own seed field uses for the identical formula
/// (verified against the JVM there), reused rather than reimplemented.
#[must_use]
pub fn parse_seed(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return Some(n);
    }
    Some(i64::from(lodestone_worldgen::hash::java_string_hash(
        trimmed,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_default_key() {
        let raw = default_raw();
        let text = raw.to_text();
        let parsed = RawProperties::parse(&text);
        for (key, value) in DEFAULTS {
            assert_eq!(
                parsed.get(key),
                Some(*value),
                "key {key:?} did not round-trip"
            );
        }
    }

    #[test]
    fn escapes_the_colon_in_level_type_exactly_like_the_real_file() {
        // `.cache/mc/26.2/server.properties` (the real, vanilla-written
        // oracle this repo already runs against) has `level-type=minecraft\
        // :flat` on disk — the outside source for this assertion, not a
        // fixture this test invented.
        let mut raw = RawProperties::new();
        raw.set("level-type", "minecraft:flat");
        let text = raw.to_text();
        assert!(
            text.contains("level-type=minecraft\\:flat"),
            "expected an escaped colon, got: {text}"
        );
        // And it must read back identically to the unescaped value.
        assert_eq!(RawProperties::parse(&text).get("level-type"), Some("minecraft:flat"));
    }

    #[test]
    fn unknown_keys_survive_a_load_edit_save_round_trip() {
        let mut raw = default_raw();
        raw.set("a-plugin-defined-key", "keep-me");
        let props = ServerProperties::from_raw(raw);
        let saved = props.to_raw();
        assert_eq!(saved.get("a-plugin-defined-key"), Some("keep-me"));
    }

    #[test]
    fn a_non_default_pairwise_distinct_fixture_reaches_every_typed_field() {
        // Evidence standard: every value below is non-default and distinct
        // from its neighbours, so a transposition or a field silently
        // falling back to its default cannot pass unnoticed.
        let text = "\
online-mode=false
motd=Distinct Test MOTD
level-name=distinct-world-dir
level-seed=distinct-seed-string
level-type=minecraft:large_biomes
gamemode=creative
difficulty=hard
max-players=7
view-distance=11
simulation-distance=9
spawn-protection=3
server-port=25566
server-ip=203.0.113.5
white-list=true
enable-rcon=true
rcon.port=25576
rcon.password=distinct-rcon-pw
enable-query=true
query.port=25567
";
        let props = ServerProperties::from_raw(RawProperties::parse(text));
        assert!(!props.online_mode);
        assert_eq!(props.motd, "Distinct Test MOTD");
        assert_eq!(props.level_name, "distinct-world-dir");
        assert_eq!(props.level_seed, "distinct-seed-string");
        assert_eq!(props.level_type, "minecraft:large_biomes");
        assert_eq!(props.gamemode, GameMode::Creative);
        assert_eq!(props.difficulty, Difficulty::Hard);
        assert_eq!(props.max_players, 7);
        assert_eq!(props.view_distance, 11);
        assert_eq!(props.simulation_distance, 9);
        assert_eq!(props.spawn_protection, 3);
        assert_eq!(props.server_port, 25566);
        assert_eq!(props.server_ip, "203.0.113.5");
        assert!(props.white_list);
        assert!(props.enable_rcon);
        assert_eq!(props.rcon_port, 25576);
        assert_eq!(props.rcon_password, "distinct-rcon-pw");
        assert!(props.enable_query);
        assert_eq!(props.query_port, 25567);
    }

    #[test]
    fn parse_seed_matches_the_jvm_hashcode_for_a_non_numeric_string() {
        // "test".hashCode() == 3556498, verified against the JVM in
        // `lodestone-worldgen-core`'s own oracle test — reused, not
        // reproduced, here.
        assert_eq!(parse_seed("test"), Some(3556498));
        assert_eq!(parse_seed("12345"), Some(12345));
        assert_eq!(parse_seed(""), None);
        assert_eq!(parse_seed("   "), None);
    }

    #[test]
    fn a_missing_file_is_created_with_vanillas_real_defaults_and_a_second_load_is_not_created() {
        let dir = std::env::temp_dir().join(format!(
            "lodestone-properties-test-{}-{}",
            std::process::id(),
            "a_missing_file_is_created"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("server.properties");
        let (first, created_first) = ServerProperties::load_or_create(&path).unwrap();
        assert!(created_first);
        assert!(first.online_mode, "vanilla's real default is online-mode=true");
        assert_eq!(first.motd, "A Minecraft Server");
        let (_second, created_second) = ServerProperties::load_or_create(&path).unwrap();
        assert!(!created_second);
        std::fs::remove_dir_all(&dir).ok();
    }
}
