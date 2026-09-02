//! Where singleplayer worlds live on disk, and the list of them.
//!
//! # What it is
//!
//! The shell's counterpart to vanilla's own level-storage source: it answers "which worlds exist?", "which
//! directory is this one?", "where does a new one go?" and "delete that one".
//! Persistence itself landed in issue
//!  and reached
//! **zero players**, because `net.rs` opened every session through
//! `IntegratedServer::open_in_memory_with_mobs` — the non-persistent
//! constructor — and the persistent one needs a `world_dir` the shell had no
//! concept of.
//!
//! # The product decision, restated now that it has changed
//!
//! That fix named two materially different readings:
//!
//! 1. **One implicit default world** — every singleplayer session opens
//!    [`default_world_dir`], and it persists.
//! 2. **A save list** — create/select/delete, each world its own directory with
//!    its own name and seed, which is what vanilla does.
//!
//! That fix shipped (1). **This module now implements (2), and (1) is gone as a
//! product behaviour.** The wart that forced the change was real and a player
//! reported it: with one implicit world, *"Create New World" could not create a
//! second one* — pressing it with a typed seed opened the existing world
//! instead, because the stored seed of an existing world always wins over a
//! requested one (see `lodestone_server::region_source::resolve_world_seed` for
//! why that rule is the only safe one). The typed seed therefore took effect
//! only on the very first launch, when no world existed yet.
//!
//! Creating a **new directory** is what fixes that properly, and it is why the
//! fix is here rather than in `resolve_world_seed`: forcing a requested seed
//! onto an existing world is the alternative, and it silently destroys the
//! continuity of the world the player was building.
//!
//! [`default_world_dir`] survives as exactly one thing: the folder name
//! `"world"` a *dedicated* server uses, which is what an already-existing
//! Lodestone save from before this change is called. Nothing calls it to *open*
//! a world any more — [`list_worlds_in`] finds that directory like any other.
//!
//! # How it works
//!
//! [`saves_dir`] is the root; every world is a subdirectory of it holding a
//! `level.dat` (vanilla's own layout, so nothing had to move). Enumeration is
//! vanilla's own level-storage-source find-level-candidates + load-level-summaries
//! collapsed into one synchronous pass:
//!
//! - list the root's entries, keep the **directories**, keep those with a
//!   regular `level.dat` (vanilla additionally accepts the pre-1.13
//!   `level.dat_old`, which this client has never written and does not read);
//! - read each `level.dat` through [`lodestone_anvil::level_dat`] into a
//!   [`WorldSummary`], vanilla's own level summary;
//! - a directory whose `level.dat` will not decode becomes an
//!   **unreadable** summary rather than being dropped — vanilla's own
//!   corrupted-level summary, which is still listed, still deletable and not
//!   playable;
//! - sort by [`WorldSummary::cmp_for_list`], which is vanilla's own level-summary compare-to
//!   verbatim: last-played **descending**, ties broken by directory name
//!   ascending.
//!
//! **Nothing in the pass can fail loudly.** A stray file in `saves/`, a
//! directory with no `level.dat`, an unreadable directory, a missing root — all
//! produce a shorter list, never an error and never a panic, for
//! `menu::servers::ServerList::load_from`'s reason: a corrupt file must not stop
//! the game from starting.
//!
//! # How to change it
//!
//! - **Creation** is [`create_world`]. It writes the directory *and* a
//!   `level.dat` carrying the player's typed name, then hands the directory back
//!   for `begin_singleplayer` to open. Writing `level.dat` here rather than
//!   letting the server do it is not redundant:
//!   `region_source::LevelDatHandle::open_or_create` derives `LevelName` from
//!   the **directory's own file name**, so a world called `My World!` would be
//!   listed as `my world_` (its sanitised folder) forever. The typed name and
//!   the folder name are two different strings and this is the only place that
//!   knows both.
//! - **The seed** is not written here at all, and must not be:
//!   `resolve_world_seed` owns `world_gen_settings.dat` and creates it on the
//!   world's first open with whatever seed the launcher requested. Because
//!   [`create_world`] made a *fresh* directory, that file does not exist yet, so
//!   the requested seed wins — which is the whole point.
//! - **Names**: [`sanitise_name`] is vanilla's own sanitize-name and
//!   [`available_dir_name`] is vanilla's own find-available-name, including its
//!   `" (N)"` counter. See each for the two places this deliberately differs.
//! - **Deletion is [`delete_world_in`], and it exists because its screen does**
//!   (issue ). This
//!   section used to say deletion was deliberately absent, and the reasoning it
//!   gave was right and is worth keeping: the destructive part of Delete is not
//!   the four-line `remove_dir_all`, it is a *confirmation the player cannot fire
//!   by accident*, and **arming the existing Delete button and confirming with a
//!   second press of the same button is deletable-by-double-click** — which for
//!   an irreversible operation is worse than no Delete at all. What changed is
//!   that [`crate::menu::confirm`] and [`crate::menu::Screen::Confirm`] now
//!   exist, so the affirmative control is a *different control on a different
//!   screen* whose rect does not overlap the Delete button's; a second click
//!   where the player just clicked lands on nothing.
//!   `the_confirmation_cannot_be_fired_by_a_second_click_where_delete_was` is the
//!   gate on that, and it derives both rects from the layouts the draw uses.
//! - [`world_dir_in`]'s containment check is what [`delete_world_in`] goes
//!   through, and it was already load-bearing before there was a delete: a
//!   [`WorldSummary::dir_name`] of `..` reaching [`crate::app`] would otherwise
//!   open the saves *root* as a world — and reaching `remove_dir_all` it would
//!   **empty** it. See [`delete_world_in`] for all three of its refusals.
//!
//! # Configuration
//!
//! Rooted at [`crate::menu::servers::data_dir`], so the `LODESTONE_DATA_DIR`
//! environment variable relocates saves along with `options.json`,
//! `servers.json` and `hidden_players.json`. On macOS that yields
//! `~/Library/Application Support/lodestone/saves/<world>`.
//!
//! **Tests must never call the no-argument [`saves_dir`]/[`create_world`].**
//! Every one of them has an explicit-root
//! twin (`*_in`) taking the root as a parameter, and that is what a test uses —
//! a temp directory, never the developer's own saves. The split is a
//! `#[cfg(test)]`-free fork for `CLAUDE.md`'s reason: an early return on
//! `cfg!(test)` is a silent skip, while a separate function is *assertable*.
//! `the_no_argument_helpers_are_the_only_users_of_the_real_root` is the gate.
//! `crate::menu::nav::MenuNav` derives its root from the same directory it takes
//! `servers.json` from, so a test that points `MenuNav` at a temp path gets a
//! temp saves root for free.
//!
//! # Dependencies
//!
//! [`crate::menu::servers::data_dir`] for the platform directory, and
//! [`lodestone_anvil::level_dat`] for the `level.dat` codec — deliberately no
//! version family either way, so
//! `cargo check -p lodestone-shell --no-default-features` still holds.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// The directory name every world folder sits under, matching vanilla's own
/// `saves/` (vanilla's own level-storage-source default roots at `<game dir>/saves`).
pub const SAVES_DIR: &str = "saves";

/// The folder name a *dedicated* server uses (`level-name=world` in
/// `server.properties`), and therefore the name of any world this client wrote
/// while that fix's "one implicit world" reading shipped.
///
/// Only [`default_world_dir`] reads it now. It is **not** the default name a new
/// world gets — that is [`DEFAULT_NEW_WORLD_NAME`], vanilla's own
/// `selectWorld.newWorld`.
pub const DEFAULT_WORLD_NAME: &str = "world";

/// `selectWorld.newWorld` — the name [`available_dir_name`] falls back to for an
/// empty typed name, matching vanilla's own world-creation-UI-state find-result-folder.
pub const DEFAULT_NEW_WORLD_NAME: &str = "New World";

/// Vanilla's own world-creation-UI-state find-result-folder's own second fallback, used
/// when the first one cannot produce a usable folder name at all.
const LAST_RESORT_NAME: &str = "World";

/// Vanilla's own illegal-file-characters constant,
/// verbatim and in vanilla's own order.
const ILLEGAL_FILE_CHARACTERS: [char; 15] = [
    '/', '\n', '\r', '\t', '\0', '\u{c}', '`', '?', '*', '\\', '<', '>', '|', '"', ':',
];

/// Vanilla's own max-file-name constant.
const MAX_FILE_NAME: usize = 255;

/// The root every world folder lives under.
///
/// **The real one.** See the module doc: a test wants the `_in` twin of whatever
/// it is calling, not this.
#[must_use]
pub fn saves_dir() -> PathBuf {
    crate::menu::servers::data_dir().join(SAVES_DIR)
}

/// The folder a pre-save-list Lodestone world was written to.
///
/// Nothing opens a world through this any more (see the module doc); it exists
/// so the name that reading (1) chose is still written down in one place, and so
/// the gate below can assert it is a child of [`saves_dir`] — which is what
/// makes [`list_worlds_in`] find such a world without a migration step.
#[must_use]
pub fn default_world_dir() -> PathBuf {
    saves_dir().join(DEFAULT_WORLD_NAME)
}

/// One world on disk, as much of vanilla's own level summary as a 26.2 `level.dat`
/// can actually supply.
///
/// # What is here and what vanilla has that this does not
///
/// Read the schema table in [`lodestone_anvil::level_dat`]'s module doc before
/// adding a field: it is the measured 14-key set every real 26.2 world carries,
/// verified with a foreign parser. In particular a 26.2 `level.dat` has **no
/// seed**, **no weather** and **no day-time**, so none of those can appear on a
/// row no matter how much the UI would like them.
///
/// Deliberately **not** ported from vanilla's own level summary:
///
/// - `icon` — vanilla shows `icon.png`, a screenshot the client saves on quit.
///   This client writes none, so an icon field would be `None` forever: the
///   island `CLAUDE.md` names.
/// - `locked` — `DirectoryLock`/`session.lock`. Nothing in this client writes
///   one, so a `locked` flag would be a constant `false` claiming a check
///   happened.
/// - `requiresManualConversion` / `requiresFileFixing` / `experimental` /
///   `BackupStatus` — all four are `DataFixer` questions, and there is no data
///   fixer here. [`Self::readable`] is the one real failure mode this client
///   has, and it maps to vanilla's own corrupted-level summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldSummary {
    /// The directory name under [`saves_dir`] — vanilla's `levelId`.
    ///
    /// A single path component, never a path: [`world_dir_in`] is the only way
    /// to turn one back into a directory, and it re-checks that.
    pub dir_name: String,
    /// `LevelName`, or [`Self::dir_name`] when it is absent or empty —
    /// vanilla's own level-name accessor.
    pub display_name: String,
    /// `LastPlayed`, epoch millis, or `-1` when unknown.
    ///
    /// `-1` is vanilla's own sentinel, not ours: its own symlink-level-summary
    /// last-played accessor
    /// returns `-1L` and vanilla's own world-list-entry widget tests
    /// `lastPlayed != -1L` before formatting a date.
    pub last_played: i64,
    /// `GameType`: 0 survival, 1 creative, 2 adventure, 3 spectator. `None`
    /// when the field is absent.
    pub game_type: Option<i32>,
    /// `Version.Name`, the display version string — `None` is vanilla's
    /// own unknown-version placeholder.
    pub version_name: Option<String>,
    /// `allowCommands` — vanilla's own has-commands accessor.
    pub allow_commands: bool,
    /// `difficulty_settings.hardcore` — vanilla's own is-hardcore accessor.
    pub hardcore: bool,
    /// `false` when the directory has a `level.dat` that would not decode.
    ///
    /// Vanilla's own corrupted-level summary: still listed, still deletable, **not**
    /// playable and not editable. Dropping such a world from the list instead
    /// would leave a folder the player can see in Finder and cannot get rid of
    /// from the game.
    pub readable: bool,
}

impl WorldSummary {
    /// Vanilla's own primary-action-active accessor — whether **Play
    /// Selected World** is live for this row.
    ///
    /// Narrowed to `readable`: vanilla's `isDisabled()` is
    /// `locked || requiresManualConversion || !isCompatible`, and this client
    /// models none of those three (see the type's doc). A world whose metadata
    /// will not decode is the one case where opening it would go wrong.
    #[must_use]
    pub fn can_play(&self) -> bool {
        self.readable
    }

    /// Vanilla's own can-edit accessor.
    #[must_use]
    pub fn can_edit(&self) -> bool {
        self.readable
    }

    /// Vanilla's own can-recreate accessor — the same predicate vanilla
    /// gives `canEdit`, kept separate because they answer different questions
    /// and only one of them is going to gain a screen first.
    #[must_use]
    pub fn can_recreate(&self) -> bool {
        self.readable
    }

    /// Vanilla's own can-delete accessor — **unconditionally `true`**, in
    /// vanilla too. A corrupt world is the one you most need to be able to
    /// remove.
    #[must_use]
    pub fn can_delete(&self) -> bool {
        true
    }

    /// Vanilla's own level-summary compare-to, verbatim: last played
    /// **descending** (most recent first), ties broken by `levelId` ascending.
    ///
    /// Written as an explicit comparator rather than an `Ord` impl because the
    /// ordering is a *presentation* rule for one screen, not this type's
    /// natural order — an `Ord` would make `sort()` elsewhere silently mean
    /// "most recently played", which is not what a set or a map wants.
    #[must_use]
    pub fn cmp_for_list(&self, rhs: &Self) -> Ordering {
        rhs.last_played
            .cmp(&self.last_played)
            .then_with(|| self.dir_name.cmp(&rhs.dir_name))
    }

    /// Vanilla's own level-summary get-info readable branch, minus the four
    /// `DataFixer` clauses this client has no source for: the game mode, then
    /// `", Cheats"` if commands are on, then `", Version: <name>"`.
    ///
    /// Hardcore replaces the game mode outright rather than appending, which is
    /// vanilla's own shape (`:166-168`) — a hardcore world is not "Survival,
    /// Hardcore".
    #[must_use]
    pub fn info_line(&self) -> String {
        let mut out = if self.hardcore {
            "Hardcore".to_string()
        } else {
            game_mode_caption(self.game_type).to_string()
        };
        if self.allow_commands {
            // `selectWorld.commands`.
            out.push_str(", Cheats");
        }
        // `selectWorld.version` + the name, or `selectWorld.versionUnknown`.
        out.push_str(", Version: ");
        out.push_str(self.version_name.as_deref().unwrap_or("Unknown"));
        out
    }

    /// Vanilla's own world-list-entry widget's second text line:
    /// the folder name, plus the last-played timestamp in parentheses when
    /// there is one.
    ///
    /// **The format is a deliberate deviation.** Vanilla uses
    /// its own localized-date-formatter at short style — the *user's* locale
    /// and the *system* time zone. This shell has neither a locale table nor a
    /// tz database, so it prints ISO `YYYY-MM-DD HH:MM` in **UTC**. Guessing a
    /// locale format would be wrong in a way nobody could test; an ISO
    /// timestamp is unambiguous and says what it is.
    #[must_use]
    pub fn detail_line(&self) -> String {
        if self.last_played == -1 {
            return self.dir_name.clone();
        }
        match format_epoch_millis_utc(self.last_played) {
            Some(stamp) => format!("{} ({stamp} UTC)", self.dir_name),
            None => self.dir_name.clone(),
        }
    }
}

/// Vanilla's own game-type name accessor, via the `gameMode.*` translation keys
/// its own level-summary create-info builds.
fn game_mode_caption(game_type: Option<i32>) -> &'static str {
    match game_type {
        Some(0) => "Survival",
        Some(1) => "Creative",
        Some(2) => "Adventure",
        Some(3) => "Spectator",
        // Vanilla has no fifth mode; an unknown int is a world some other tool
        // wrote, and saying so is better than claiming Survival.
        _ => "Unknown",
    }
}

/// `YYYY-MM-DD HH:MM` in UTC for an epoch-millisecond stamp, or `None` for a
/// negative one (a clock set before 1970 — `LastPlayed` is never legitimately
/// negative, `-1` already means "unknown").
///
/// The date arithmetic is Howard Hinnant's `civil_from_days`, which is exact for
/// every day in the proleptic Gregorian calendar and needs no table. Reproduced
/// rather than pulled in as a dependency because this is the only date this
/// client formats; `the_timestamp_format_matches_an_independent_calendar` checks
/// it against values produced by Python's own `datetime`, i.e. outside this
/// code.
#[must_use]
pub fn format_epoch_millis_utc(millis: i64) -> Option<String> {
    if millis < 0 {
        return None;
    }
    let secs = millis / 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hour, minute) = (rem / 3600, (rem % 3600) / 60);

    // civil_from_days, shifted to a 1970 epoch.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    Some(format!("{year:04}-{m:02}-{d:02} {hour:02}:{minute:02}"))
}

/// Every world under `root`, sorted by [`WorldSummary::cmp_for_list`].
///
/// Vanilla's own level-storage-source find-level-candidates + load-level-summaries. Never fails:
/// a missing or unlistable `root` is an **empty** list, which is the state a
/// fresh install is in and which the world-select screen must render rather than
/// crash on.
///
/// Unlike vanilla this does **not** create `root` when it is missing
/// (vanilla's own find-level-candidates does). Listing is a
/// read; the directory is created by [`create_world_in`], the one operation that
/// needs it to exist. A client that is opened and never played should write
/// nothing.
#[must_use]
pub fn list_worlds_in(root: &Path) -> Vec<WorldSummary> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut worlds: Vec<WorldSummary> = entries
        .flatten()
        .filter_map(|entry| summarise_dir(&entry.path()))
        .collect();
    worlds.sort_by(WorldSummary::cmp_for_list);
    worlds
}

/// One directory as a [`WorldSummary`], or `None` when it is not a world at all.
///
/// The three `None` cases are the whole robustness story, and each has been a
/// real thing in a real `saves/` folder: a plain **file** (`.DS_Store`), a
/// directory with **no `level.dat`** (an interrupted create, or someone's
/// unrelated folder), and a name that is not valid UTF-8.
fn summarise_dir(path: &Path) -> Option<WorldSummary> {
    // Browser: there is no `saves/` directory, so there is no world to summarise.
    // The `!path.is_dir()` guard below would already return `None` here on its own
    // (`is_dir()` is `false` for everything on wasm32, because the metadata call
    // returns `Err(Unsupported)` — measured, not assumed), so this early return
    // changes no behaviour; it exists so the `lodestone_anvil` reads further down
    // can be gated without leaving a body that only *happens* to be unreachable.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        return None;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
    if !path.is_dir() {
        return None;
    }
    let dir_name = path.file_name()?.to_str()?.to_string();
    let data_file = lodestone_anvil::level_dat::path_in(path);
    if !data_file.is_file() {
        return None;
    }
    // From here the directory *is* a world: a decode failure downgrades it to
    // unreadable rather than hiding it. See `WorldSummary::readable`.
    let Ok(level) = lodestone_anvil::level_dat::read_from_file(&data_file) else {
        return Some(WorldSummary {
            display_name: dir_name.clone(),
            dir_name,
            last_played: -1,
            game_type: None,
            version_name: None,
            allow_commands: false,
            hardcore: false,
            readable: false,
        });
    };
    let display_name = level
        .level_name()
        .filter(|name| !name.is_empty())
        .unwrap_or(&dir_name)
        .to_string();
    Some(WorldSummary {
        dir_name,
        display_name,
        last_played: level.last_played().unwrap_or(-1),
        game_type: level.game_type(),
        version_name: level.version_name().map(str::to_string),
        allow_commands: level.allow_commands(),
        hardcore: level.hardcore(),
        readable: true,
    })
    }
}

/// Vanilla's own sanitize-name: every
/// [`ILLEGAL_FILE_CHARACTERS`] becomes `_`, then so does every `.`, `/` and `"`.
///
/// The second pass overlaps the first (`/` and `"` are in both lists) and is
/// reproduced anyway, because the `.` is only in the second — that is what stops
/// a name like `..` becoming a directory traversal.
#[must_use]
pub fn sanitise_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ILLEGAL_FILE_CHARACTERS.contains(&ch) || matches!(ch, '.' | '/' | '"') {
                '_'
            } else {
                ch
            }
        })
        .collect()
}

/// Vanilla's own is-path-part-portable check — the reserved-Windows-filename
/// check, as a matcher rather than a regex.
///
/// Vanilla's pattern is `.*\.|(?:COM|CLOCK\$|CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\..*)?`
/// case-insensitively, and `isPathPartPortable` is its **negation**. The
/// leading-alternative `.*\.` (any name ending in a dot) is unreachable after
/// [`sanitise_name`] has already replaced every `.`, but is checked anyway so
/// this function answers the same question vanilla's does for any input.
#[must_use]
fn is_path_part_portable(name: &str) -> bool {
    if name.ends_with('.') {
        return false;
    }
    // The reserved name may be followed by `.<anything>`; the dot itself is the
    // separator, so compare against the part before the first dot.
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(stem.as_str(), "COM" | "CLOCK$" | "CON" | "PRN" | "AUX" | "NUL") {
        return false;
    }
    if let Some(digit) = stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT"))
        && digit.len() == 1
        && matches!(digit.as_bytes()[0], b'1'..=b'9')
    {
        return false;
    }
    true
}

/// Vanilla's own find-available-name against `root`: the directory name a
/// world called `requested` should get, guaranteed not to be one that already
/// exists.
///
/// Vanilla's sequence, reproduced: sanitise; wrap a non-portable name in
/// underscores; strip and adopt a trailing `" (N)"` counter if the name already
/// has one; clamp to [`MAX_FILE_NAME`]; then try `name`, `name (1)`,
/// `name (2)`, … until one is free.
///
/// **Two deliberate differences from vanilla, both toward doing less:**
///
/// - Vanilla probes by `Files.createDirectory` then `deleteIfExists`, so it
///   detects an unwritable root as well as a taken name. This checks
///   `Path::exists`, because creating and deleting directories to answer a
///   *question* is a side effect a caller cannot see coming — and the real
///   creation immediately afterwards reports an unwritable root anyway.
/// - An empty or whitespace-only `requested` becomes
///   [`DEFAULT_NEW_WORLD_NAME`], which is
///   vanilla's own world-creation-UI-state find-result-folder's job rather than
///   `findAvailableName`'s. It is here because this is the only entry point.
///
/// The counter is bounded: after [`MAX_DUPLICATE_ATTEMPTS`] it gives up and
/// returns the last candidate rather than looping forever on a root that
/// somehow reports every name as taken.
#[must_use]
pub fn available_dir_name(root: &Path, requested: &str) -> String {
    let trimmed = requested.trim();
    let base = if trimmed.is_empty() {
        DEFAULT_NEW_WORLD_NAME
    } else {
        trimmed
    };
    let mut base = sanitise_name(base);
    if !is_path_part_portable(&base) {
        base = format!("_{base}_");
    }
    // A sanitised name can still be empty (`"..."` is three underscores, but
    // `""` after trimming was handled above; a name of only characters the
    // sanitiser drops cannot happen since it *replaces* rather than removes).
    // Kept as a guard anyway: an empty component would resolve to `root`
    // itself, and `delete_world` must never be handed that.
    if base.is_empty() {
        base = LAST_RESORT_NAME.to_string();
    }
    let (mut base, mut count) = split_copy_counter(&base);
    truncate_chars(&mut base, MAX_FILE_NAME);

    let mut candidate = base.clone();
    for _ in 0..MAX_DUPLICATE_ATTEMPTS {
        candidate = if count == 0 {
            base.clone()
        } else {
            let suffix = format!(" ({count})");
            let mut stem = base.clone();
            truncate_chars(&mut stem, MAX_FILE_NAME - suffix.len());
            format!("{stem}{suffix}")
        };
        if !root.join(&candidate).exists() {
            return candidate;
        }
        count += 1;
    }
    candidate
}

/// How many `" (N)"` suffixes [`available_dir_name`] will try.
///
/// Vanilla's loop is unbounded. A bound is the difference between "the player
/// has a thousand worlds called New World" and "the menu thread never returns",
/// and the failure mode of exceeding it is one clashing name rather than a hang.
const MAX_DUPLICATE_ATTEMPTS: u32 = 1_000;

/// Vanilla's own copy-counter pattern applied: split `"name (7)"` into
/// `("name", 7)`, or leave a name with no counter alone at `("name", 0)`.
///
/// Vanilla's regex is `(<name>.*) \((<count>\d*)\)` with `DOTALL`, anchored by
/// `matches()`. Note `\d*` accepts an **empty** count, which
/// `Integer.parseInt("")` then throws on — vanilla would propagate that out of
/// `findAvailableName` as an unchecked exception, which
/// vanilla's own world-creation-UI-state find-result-folder catches and answers with `"World"`.
/// Here an unparseable count simply means "no counter", which reaches the same
/// place without the round trip through a panic.
fn split_copy_counter(name: &str) -> (String, u32) {
    let Some(stripped) = name.strip_suffix(')') else {
        return (name.to_string(), 0);
    };
    let Some(open) = stripped.rfind(" (") else {
        return (name.to_string(), 0);
    };
    let digits = &stripped[open + 2..];
    // `.*` is greedy and `\d*` cannot match a non-digit, so a counter is
    // digits-only and the stem is everything before the last " (".
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return (name.to_string(), 0);
    }
    match digits.parse::<u32>() {
        Ok(count) => (stripped[..open].to_string(), count),
        Err(_) => (name.to_string(), 0),
    }
}

/// Truncate `s` to at most `max` **characters**, in place.
///
/// Characters rather than bytes: vanilla's `String.substring` counts UTF-16
/// units, and slicing a Rust `String` by byte index would panic mid-codepoint on
/// a name with any non-ASCII in it — a crash on a world called `Мир`.
fn truncate_chars(s: &mut String, max: usize) {
    if let Some((byte_index, _)) = s.char_indices().nth(max) {
        s.truncate(byte_index);
    }
}

/// The directory of the world named `dir_name` under `root`, or `None` when
/// `dir_name` is not a plain directory name.
///
/// **This is the containment check every path-taking operation here goes
/// through**, and it exists because [`WorldSummary::dir_name`] travels through
/// the UI as a `String`: a value of `..` or `/etc` would otherwise resolve to
/// somewhere that is not a world — the saves root itself, for `..`, which
/// `IntegratedServer` would then happily fill with region files. It is also the
/// gate a future delete must go through (see the module doc). A name is accepted
/// only if it is exactly one `Component::Normal` — no separators, no `.`, no
/// `..`, not absolute, not a prefix.
#[must_use]
pub fn world_dir_in(root: &Path, dir_name: &str) -> Option<PathBuf> {
    let mut components = Path::new(dir_name).components();
    let first = components.next()?;
    if components.next().is_some() {
        return None;
    }
    match first {
        std::path::Component::Normal(name) if name == std::ffi::OsStr::new(dir_name) => {
            Some(root.join(dir_name))
        }
        _ => None,
    }
}

/// Why a world could not be created.
///
/// A typed error rather than `io::Error` so the menu can distinguish "the
/// filesystem said no" from "the metadata could not be written", which are
/// different things to tell a player.
#[derive(Debug)]
pub enum SaveError {
    /// The filesystem refused.
    Io(std::io::Error),
    /// `level.dat` could not be encoded or written.
    ///
    /// Native-only: it names a `lodestone_anvil` error, and that crate is a
    /// `cfg(not(wasm32))` dependency because its readers and writers are
    /// `std::fs`-based. A browser cannot reach this variant because it cannot
    /// reach [`create_world_in`], which refuses before writing anything.
    #[cfg(not(target_arch = "wasm32"))]
    Anvil(lodestone_anvil::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Io(e) => write!(f, "{e}"),
            #[cfg(not(target_arch = "wasm32"))]
            SaveError::Anvil(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SaveError {}

/// Create a new world under the real [`saves_dir`]. See the module doc: a test
/// calls [`create_world_in`].
///
/// # Errors
///
/// See [`create_world_in`].
pub fn create_world(
    name: &str,
    game_type: i32,
    enabled_features: &[String],
    generator_override: Option<(&GeneratorOverride, &str)>,
) -> Result<PathBuf, SaveError> {
    create_world_in(&saves_dir(), name, game_type, enabled_features, generator_override)
}

/// A chosen "Customize Type" generator, collected from
/// [`crate::menu::create_world::WorldCreationConfig::flat_layers`]/
/// [`crate::menu::create_world::WorldCreationConfig::single_biome`] — see
/// [`create_world_in`]'s own doc for where and why this reaches disk.
#[derive(Debug, Clone, PartialEq)]
#[cfg(not(target_arch = "wasm32"))]
pub enum GeneratorOverride {
    /// Vanilla's own flat ("Superflat") generator — bottom-to-top
    /// `(block id, height)` pairs, plus the fixed surface biome and the two
    /// decoration flags real flat presets carry.
    Flat { layers: Vec<(String, i32)>, biome: String, features: bool, lakes: bool },
    /// Vanilla's own fixed-biome noise generator — one biome id, everywhere.
    FixedBiome { biome: String },
}

/// Vanilla's own seed-parsing rule — trim, a valid `i64` literal used
/// verbatim, free text hashed with Java's own `String.hashCode()`, empty
/// means a fresh random `i64` — applied here **only** so a "Customize Type"
/// choice has a real seed to write alongside it into `world_gen_settings.dat`
/// at world-creation time, before `app/launch.rs`'s own
/// `resolve_launch_seed`/`parse_seed` would normally resolve one.
///
/// This deliberately duplicates that pair rather than sharing them: this
/// session's working scope keeps `crates/lodestone-shell/src/app/**` off
/// limits (another agent's concurrent work), and the rule itself is small,
/// stable and independently checkable — see this module's own tests, which
/// check it against the JVM-verified `String.hashCode()` constants
/// `lodestone_worldgen_core::hash`'s own tests use, not against
/// `app/launch.rs`'s copy. The two copies computing the *same* deterministic
/// value from the *same* typed string can never disagree; the only place
/// this matters at all is an **empty** seed, where either copy's random draw
/// is equally valid — see [`create_world_in`]'s own doc for why the draw made
/// here, not `app/launch.rs`'s later one, is the one that actually sticks
/// once a customized world is created.
#[cfg(not(target_arch = "wasm32"))]
fn resolve_seed_for_creation(raw: &str) -> i64 {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(crate::platform::epoch_duration().as_nanos());
        return hasher.finish() as i64;
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return n;
    }
    i64::from(lodestone_worldgen_core::hash::java_string_hash(trimmed))
}

/// Create a new world directory under `root` for a world the player named
/// `name`, and return the directory.
///
/// Three steps, in this order and for these reasons:
///
/// 1. `create_dir_all(root)` — vanilla's `findResultFolder` does the same
///   , and it must happen before
///    [`available_dir_name`] probes for a free name.
/// 2. `create_dir` (**not** `create_dir_all`) for the world itself: the name
///    came from `available_dir_name`, so an `AlreadyExists` here means another
///    process won the race, and failing is right — the alternative is opening
///    somebody else's world.
/// 3. write `level.dat` with the player's typed `name` as `LevelName`. This is
///    the step that makes the name mean anything: see the module doc's "How to
///    change it" on why leaving it to
///    `region_source::LevelDatHandle::open_or_create` loses it.
///
/// The seed is deliberately **not** written on the ordinary path —
/// `resolve_world_seed` creates `world_gen_settings.dat` on the world's first
/// open, and because this directory is new the requested seed is the one
/// that wins. `generator_override` is the one exception: see this doc's own
/// section below for why a "Customize Type" choice needs the seed written
/// *here* instead.
///
/// `enabled_features` is [`crate::menu::create_world::WorldCreationConfig::experiments`]
/// (Experiments half) — bare flag ids, [`ExperimentFlag::id`]'s
/// own shape — written into `level.dat`'s `enabled_features` field through
/// [`lodestone_anvil::level_dat::LevelDat::with_enabled_features`]. Empty
/// (nothing turned on) writes nothing extra, matching every other decorative
/// field this function already leaves at its vanilla default.
///
/// [`ExperimentFlag::id`]: crate::menu::create_world::ExperimentFlag::id
///
/// # `generator_override` — the "Customize Type" half, and why it is not
/// `level.dat`
///
/// `Some((override, raw_seed))` is
/// [`crate::menu::create_world::WorldCreationConfig::flat_layers`]/
/// [`crate::menu::create_world::WorldCreationConfig::single_biome`], paired
/// with the screen's own typed seed text
/// ([`crate::menu::create_world::WorldCreationConfig::seed`]). Vanilla does
/// **not** keep generator customization in `level.dat` — a 26.2 `level.dat`
/// has no such field at all (this crate's own `level_dat` module doc measured
/// the real 14-field schema); it lives in
/// `<world>/data/minecraft/world_gen_settings.dat`, in the *same*
/// `dimensions.minecraft:overworld.generator` compound the seed's own file
/// carries — verified against a real 26.2 world folder, hand-decoded
/// independently of this crate's own NBT reader (see
/// [`lodestone_anvil::world_gen_settings`]'s own doc and the fixture its
/// tests check into `crates/lodestone-anvil/tests/support/`).
///
/// That file is normally created lazily, by `resolve_world_seed` on first
/// open — but that function **errors** if the file already exists without a
/// numeric `seed` field (`Error::MissingSeed`, propagated as a session-open
/// failure, not a fallback), so a generator override cannot be pre-written
/// with no seed alongside it. [`resolve_seed_for_creation`] resolves the same
/// seed `app/launch.rs`'s own `resolve_launch_seed` would (vanilla's own
/// seed-parsing rule — see that function's own doc for why it is duplicated
/// rather than shared) and this function writes both together, before the
/// server ever opens the directory — the same "shell writes it first, the
/// server only ever reads it back" shape [`Self`] already uses for
/// `enabled_features`. Because `resolve_world_seed`'s existing-file branch
/// always wins over a `requested` seed, the value resolved *here* — not
/// whatever `app/launch.rs` independently resolves moments later at launch —
/// is the one that sticks; for a literal or hashed seed the two computations
/// agree by construction (same deterministic rule, same input string), and
/// for an empty (random) seed either draw is an equally valid "give me a
/// random seed".
///
/// `None` (every world type this screen offers besides Flat and Single
/// Biome) changes nothing: no extra file is written, matching the "seed is
/// not written here" rule above exactly as it always has.
///
/// # Errors
///
/// [`SaveError::Io`] if either directory cannot be created,
/// [`SaveError::Anvil`] if `level.dat` (or, for a customized world,
/// `world_gen_settings.dat`) cannot be written.
pub fn create_world_in(
    root: &Path,
    name: &str,
    game_type: i32,
    enabled_features: &[String],
    generator_override: Option<(&GeneratorOverride, &str)>,
) -> Result<PathBuf, SaveError> {
    // Browser: refuse explicitly rather than half-succeeding. `create_dir_all`
    // below returns `Err(Unsupported)` on wasm32 (measured), so this would already
    // fail — but it would fail as `SaveError::Io("operation not supported on this
    // platform")`, which tells the player nothing about *why* and reads like a
    // permissions problem. A browser singleplayer world is real, it just lives in
    // memory: `IntegratedServer::open_in_memory` is the path, and it needs no
    // `saves/` directory and no `level.dat`.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (root, name, game_type, enabled_features, generator_override);
        return Err(SaveError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a browser has no saves directory; browser worlds are in-memory only",
        )));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
    std::fs::create_dir_all(root).map_err(SaveError::Io)?;
    let dir_name = available_dir_name(root, name);
    let dir = root.join(&dir_name);
    std::fs::create_dir(&dir).map_err(SaveError::Io)?;

    // The typed name, trimmed, or the folder name when the player typed
    // nothing — vanilla's own level-name accessor falls back to the id for an empty
    // `LevelName`, so writing an empty one would list the folder anyway; being
    // explicit means the file itself is honest.
    let trimmed = name.trim();
    let level_name = if trimmed.is_empty() {
        dir_name.as_str()
    } else {
        trimmed
    };
    // `Spawn::default()` is `[0, 64, 0]` facing north, which is
    // `IntegratedServer::open_persistent_with_mobs`' own default too — it
    // rewrites the field with the real mob centre on first open, so this is a
    // placeholder that gets corrected rather than a competing claim.
    let level = lodestone_anvil::level_dat::LevelDat::for_new_world(
        level_name,
        &lodestone_anvil::level_dat::Spawn::default(),
        game_type,
    )
    .with_enabled_features(enabled_features);
    lodestone_anvil::level_dat::write_to_file(
        &level,
        &lodestone_anvil::level_dat::path_in(&dir),
    )
    .map_err(SaveError::Anvil)?;

    if let Some((generator, raw_seed)) = generator_override {
        let seed = resolve_seed_for_creation(raw_seed);
        let settings = lodestone_anvil::world_gen_settings::WorldGenSettings::from_seed(seed);
        let settings = match generator {
            GeneratorOverride::Flat { layers, biome, features, lakes } => {
                let layer_refs: Vec<_> = layers
                    .iter()
                    .map(|(block, height)| lodestone_anvil::world_gen_settings::FlatLayer {
                        block,
                        height: *height,
                    })
                    .collect();
                settings.with_overworld_flat_generator(&layer_refs, biome, *features, *lakes)
            }
            GeneratorOverride::FixedBiome { biome } => settings.with_overworld_fixed_biome_generator(biome),
        };
        lodestone_anvil::world_gen_settings::write_to_file(
            &settings,
            &lodestone_anvil::world_gen_settings::path_in(&dir),
        )
        .map_err(SaveError::Anvil)?;
    }

    Ok(dir)
    }
}

/// Why a world could not be deleted.
///
/// Four variants rather than one `io::Error`, and the split is the whole safety
/// argument: three of them are **refusals** this module makes before touching the
/// filesystem at all, and each names a different way a `dir_name` travelling
/// through the UI as a `String` could have pointed somewhere that is not a world.
/// Collapsing them into `Io` would make "we refused" and "the filesystem
/// refused" the same observation, and only one of those is a bug.
#[derive(Debug)]
pub enum DeleteError {
    /// `dir_name` is not a single plain path component, so [`world_dir_in`]
    /// declined to resolve it. `..` is the case that matters: it would otherwise
    /// name the saves **root**.
    NotAWorldName(String),
    /// The directory resolved, and holds no `level.dat`. This is what stops the
    /// saves root itself and somebody's unrelated folder from being removed —
    /// see [`delete_world_in`].
    NotAWorld(PathBuf),
    /// The entry is a **symlink**, so removing it would either delete the link
    /// (leaving the target) or, if anything followed it, delete a directory
    /// outside `saves/`. Refused rather than resolved.
    Symlink(PathBuf),
    /// The filesystem refused the removal itself.
    Io(std::io::Error),
}

impl std::fmt::Display for DeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeleteError::NotAWorldName(name) => {
                write!(f, "{name:?} is not a world folder name")
            }
            DeleteError::NotAWorld(dir) => {
                write!(f, "{} holds no level.dat", dir.display())
            }
            DeleteError::Symlink(dir) => {
                write!(f, "{} is a symbolic link", dir.display())
            }
            DeleteError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DeleteError {}

/// Remove the world named `dir_name` from under `root` —
/// vanilla's own level-storage-access delete-level, and the destructive half
/// of issue.
///
/// **There is deliberately no no-argument twin.** Every other operation here has
/// one because production calls it with the real root; this one is only ever
/// reached from [`crate::menu::nav::MenuNav`], which holds its own root, so a
/// `delete_world()` would exist solely as a way for a test to delete the
/// developer's worlds. See the module doc's note on
/// `no_test_touches_the_real_saves_dir`.
///
/// # The three refusals, in order, and why each one is here
///
/// 1. [`world_dir_in`] — the containment check. A [`WorldSummary::dir_name`] is a
///    `String` by the time it has been through the UI, and a value of `..`
///    resolves to the saves **root**; `/etc` resolves to `/etc`. Neither is
///    representable as one `Component::Normal`, so both are refused before any
///    path is built.
/// 2. **Not a symlink.** `std::fs::remove_dir_all` does not follow one, so this is
///    belt-and-braces — but the belt is what makes the guarantee *assertable*: a
///    symlink in `saves/` pointing at the player's home directory must produce a
///    named refusal, not an `io::Error` whose kind depends on the platform.
///    Checked with `symlink_metadata`, which does not follow.
/// 3. **A `level.dat` must be present.** This is the guard that makes the target
///    a *world* rather than a directory: the saves root has no `level.dat`, and
///    neither does the `notaworld` folder every fixture here carries. It is
///    deliberately an *existence* check and not a decode: vanilla's
///    `canDelete()` is unconditionally `true`, a corrupt world is the one you
///    most need to remove, and requiring a decode would make exactly that world
///    permanent.
///
/// # Errors
///
/// [`DeleteError`], one variant per refusal above plus [`DeleteError::Io`] for a
/// removal the filesystem declined. Returns the directory that was removed, so a
/// caller (and a gate) can say *which* one without re-deriving it.
pub fn delete_world_in(root: &Path, dir_name: &str) -> Result<PathBuf, DeleteError> {
    let Some(dir) = world_dir_in(root, dir_name) else {
        return Err(DeleteError::NotAWorldName(dir_name.to_string()));
    };
    // `symlink_metadata` rather than `metadata`: the latter follows the link and
    // would report the *target's* kind, which is the one answer that must not
    // decide this.
    let meta = std::fs::symlink_metadata(&dir).map_err(DeleteError::Io)?;
    if meta.is_symlink() {
        return Err(DeleteError::Symlink(dir));
    }
    if !meta.is_dir() {
        return Err(DeleteError::NotAWorld(dir));
    }
    // A directory with no `level.dat` is not a world, and must not be deleted.
    // Native-only because the check names `lodestone_anvil`; unreachable on wasm32
    // because `symlink_metadata` above already failed with `Unsupported`.
    #[cfg(not(target_arch = "wasm32"))]
    if !lodestone_anvil::level_dat::path_in(&dir).is_file() {
        return Err(DeleteError::NotAWorld(dir));
    }
    std::fs::remove_dir_all(&dir).map_err(DeleteError::Io)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp root for one test, named after the test so concurrent runs (and
    /// concurrent *agents*, per `CLAUDE.md`) cannot collide.
    ///
    /// Removed on entry rather than on exit: a panicking test leaves its
    /// directory behind on purpose, so the failure can be inspected.
    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lodestone-saves-{}-{tag}/saves",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    /// Write a world directory with a real `level.dat`, through the same codec
    /// production uses — the fixture has to be a file our reader can actually
    /// read, and hand-rolling gzip NBT here would test the fixture instead.
    fn plant_world(root: &Path, dir_name: &str, level_name: &str, last_played: i64, game_type: i32) {
        let dir = root.join(dir_name);
        std::fs::create_dir_all(&dir).expect("create world dir");
        let mut level = lodestone_anvil::level_dat::LevelDat::for_new_world(
            level_name,
            &lodestone_anvil::level_dat::Spawn::default(),
            game_type,
        );
        level.set_last_played(last_played).expect("sets LastPlayed");
        lodestone_anvil::level_dat::write_to_file(
            &level,
            &lodestone_anvil::level_dat::path_in(&dir),
        )
        .expect("write level.dat");
    }

    #[test]
    fn the_default_world_sits_under_the_data_dir() {
        let world = default_world_dir();
        assert!(
            world.starts_with(crate::menu::servers::data_dir()),
            "worlds must share the `LODESTONE_DATA_DIR` root that options.json \
             and servers.json use, or a relocated data dir would strand them: {world:?}"
        );
        assert!(world.ends_with("saves/world"), "{world:?}");
    }

    #[test]
    fn the_default_world_is_inside_the_saves_root() {
        // The save list enumerates children of `saves_dir()`; a pre-save-list
        // world that was not one would be invisible after this change, which
        // is the migration this assertion rules out.
        assert!(default_world_dir().starts_with(saves_dir()));
    }

    /// **The fixture's contents are stated and asserted before anything is
    /// measured**, because the `world` species of vacuous test lives in the
    /// input data: a root with one valid world cannot exercise sorting,
    /// duplicate names, a stray file or a corrupt entry, and no amount of
    /// reading the test source would show that.
    ///
    /// This root holds, deliberately:
    ///
    /// | entry | what it is |
    /// |---|---|
    /// | `alpha` | valid, `LevelName` "Alpha World", last played 3000 |
    /// | `bravo` | valid, `LevelName` "Bravo World", last played 1000 |
    /// | `charlie` | valid, `LevelName` "Charlie World", last played 3000 (**ties** with `alpha`) |
    /// | `delta` | valid, **empty** `LevelName` (falls back to the folder) |
    /// | `broken` | a `level.dat` that is not gzip at all |
    /// | `notaworld` | a directory with no `level.dat` |
    /// | `.DS_Store` | a plain **file** in the saves root |
    fn populated_root(tag: &str) -> PathBuf {
        let root = temp_root(tag);
        std::fs::create_dir_all(&root).expect("create root");
        plant_world(&root, "alpha", "Alpha World", 3_000, 0);
        plant_world(&root, "bravo", "Bravo World", 1_000, 1);
        plant_world(&root, "charlie", "Charlie World", 3_000, 0);
        plant_world(&root, "delta", "", 2_000, 0);
        let broken = root.join("broken");
        std::fs::create_dir_all(&broken).expect("create broken dir");
        std::fs::write(
            lodestone_anvil::level_dat::path_in(&broken),
            b"this is not gzip",
        )
        .expect("write broken level.dat");
        std::fs::create_dir_all(root.join("notaworld")).expect("create empty dir");
        std::fs::write(root.join(".DS_Store"), b"\x00\x01").expect("write stray file");
        root
    }

    #[test]
    fn the_fixture_root_really_contains_every_shape_the_enumeration_has_to_handle() {
        // The precondition assertion the fixture doc promises. Without it the
        // tests below are verified against whatever `populated_root` happens to
        // have written, which is the unreadable-from-source species.
        let root = populated_root("fixture-premise");
        let mut names: Vec<String> = std::fs::read_dir(&root)
            .expect("root exists")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                ".DS_Store".to_string(),
                "alpha".to_string(),
                "bravo".to_string(),
                "broken".to_string(),
                "charlie".to_string(),
                "delta".to_string(),
                "notaworld".to_string(),
            ]
        );
        assert!(root.join(".DS_Store").is_file(), "the stray entry is a file");
        assert!(
            !lodestone_anvil::level_dat::path_in(&root.join("notaworld")).exists(),
            "`notaworld` must have no level.dat"
        );
        assert!(
            lodestone_anvil::level_dat::read_from_file(&lodestone_anvil::level_dat::path_in(
                &root.join("broken")
            ))
            .is_err(),
            "`broken`'s level.dat must genuinely fail to decode, or the \
             corrupt-world path is never exercised"
        );
    }

    #[test]
    fn enumeration_lists_every_world_and_skips_everything_that_is_not_one() {
        let root = populated_root("enumerate");
        let worlds = list_worlds_in(&root);
        let dirs: Vec<&str> = worlds.iter().map(|w| w.dir_name.as_str()).collect();
        // Sorted: last played descending, ties by directory name ascending.
        // 3000 alpha, 3000 charlie, 2000 delta, 1000 bravo, then `broken` at
        // the `-1` sentinel.
        assert_eq!(dirs, vec!["alpha", "charlie", "delta", "bravo", "broken"]);
        assert!(
            !dirs.contains(&"notaworld"),
            "a directory with no level.dat is not a world"
        );
        assert!(
            !dirs.contains(&".DS_Store"),
            "a stray file in saves/ must not become a row"
        );

        let by_dir = |name: &str| {
            worlds
                .iter()
                .find(|w| w.dir_name == name)
                .unwrap_or_else(|| panic!("{name} missing"))
                .clone()
        };
        assert_eq!(by_dir("alpha").display_name, "Alpha World");
        assert_eq!(by_dir("alpha").game_type, Some(0));
        assert_eq!(by_dir("bravo").game_type, Some(1), "creative");
        assert_eq!(
            by_dir("delta").display_name,
            "delta",
            "an empty LevelName falls back to the folder name \
             (vanilla's own level-name accessor)"
        );
        assert_eq!(
            by_dir("alpha").version_name.as_deref(),
            Some("26.2"),
            "for_new_world writes Version.Name, and the summary must read it"
        );

        // The corrupt world: listed, not playable, still deletable — vanilla's
        // own corrupted-level summary.
        let broken = by_dir("broken");
        assert!(!broken.readable);
        assert!(!broken.can_play());
        assert!(!broken.can_edit());
        assert!(broken.can_delete(), "a corrupt world must be removable");
        assert_eq!(broken.last_played, -1);

        // -- control ---------------------------------------------------------
        // The filters above are only meaningful if this enumeration *can*
        // report a world at all, and if `readable` can be true. Both are shown
        // by the four valid rows, so the remaining control is the opposite
        // direction: an empty root must produce an empty list rather than the
        // same five rows from somewhere else.
        let empty = temp_root("enumerate-empty");
        std::fs::create_dir_all(&empty).expect("create empty root");
        assert!(
            list_worlds_in(&empty).is_empty(),
            "an empty root must list no worlds"
        );
        assert!(
            list_worlds_in(&empty.join("does-not-exist")).is_empty(),
            "a missing root must list no worlds rather than erroring"
        );
    }

    /// The tie-break is the part of `compareTo` a one-world fixture cannot see.
    #[test]
    fn ties_on_last_played_break_by_directory_name_ascending() {
        let root = temp_root("tie-break");
        std::fs::create_dir_all(&root).expect("create root");
        // Planted in the *wrong* order on purpose, so passing requires the sort
        // to actually run.
        plant_world(&root, "zebra", "Z", 5_000, 0);
        plant_world(&root, "aardvark", "A", 5_000, 0);
        let dirs: Vec<String> = list_worlds_in(&root)
            .into_iter()
            .map(|w| w.dir_name)
            .collect();
        assert_eq!(dirs, vec!["aardvark".to_string(), "zebra".to_string()]);

        // And the primary key really is descending, not ascending — which the
        // tie-break case above cannot distinguish.
        let root = temp_root("tie-break-order");
        std::fs::create_dir_all(&root).expect("create root");
        plant_world(&root, "aardvark", "A", 1_000, 0);
        plant_world(&root, "zebra", "Z", 5_000, 0);
        let dirs: Vec<String> = list_worlds_in(&root)
            .into_iter()
            .map(|w| w.dir_name)
            .collect();
        assert_eq!(
            dirs,
            vec!["zebra".to_string(), "aardvark".to_string()],
            "most recently played first — vanilla's own level-summary compare-to \
             returns 1 when this.lastPlayed < rhs.lastPlayed"
        );
    }

    #[test]
    fn illegal_characters_become_underscores_including_the_dot() {
        // Every character in vanilla's own illegal-file-characters constant, plus
        // the `.` that only the second pass catches.
        assert_eq!(sanitise_name("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitise_name(".."), "__", "the traversal name is defused");
        assert_eq!(sanitise_name("My World"), "My World", "spaces are legal");
        assert_eq!(sanitise_name("Мир 1.2"), "Мир 1_2", "non-ASCII survives");
        // The control: a name with none of them is returned unchanged, so the
        // assertions above are not satisfied by a function that mangles
        // everything.
        assert_eq!(sanitise_name("plain-name_9"), "plain-name_9");
    }

    #[test]
    fn a_duplicate_name_gets_vanillas_numeric_suffix() {
        let root = temp_root("dupes");
        std::fs::create_dir_all(&root).expect("create root");
        assert_eq!(available_dir_name(&root, "New World"), "New World");
        plant_world(&root, "New World", "New World", 1, 0);
        assert_eq!(available_dir_name(&root, "New World"), "New World (1)");
        plant_world(&root, "New World (1)", "New World", 1, 0);
        assert_eq!(available_dir_name(&root, "New World"), "New World (2)");
        // Asking for the *already-suffixed* name adopts its counter rather than
        // producing `New World (1) (1)` — vanilla's own copy-counter pattern.
        assert_eq!(available_dir_name(&root, "New World (1)"), "New World (2)");
        // An empty typed name is vanilla's `selectWorld.newWorld`.
        assert_eq!(available_dir_name(&root, "   "), "New World (2)");
        // A reserved Windows name is wrapped, not used.
        assert_eq!(available_dir_name(&root, "NUL"), "_NUL_");
        assert_eq!(available_dir_name(&root, "com1"), "_com1_");
        // And a name that merely *starts* with one is left alone — the control
        // for the wrapper, which would otherwise be satisfied by wrapping
        // everything.
        assert_eq!(available_dir_name(&root, "console"), "console");
    }

    #[test]
    fn the_copy_counter_split_matches_vanillas_regex_on_the_cases_that_differ() {
        assert_eq!(split_copy_counter("New World (3)"), ("New World".into(), 3));
        assert_eq!(split_copy_counter("New World"), ("New World".into(), 0));
        // `\d*` cannot match a non-digit, so this is not a counter.
        assert_eq!(split_copy_counter("New World (x)"), ("New World (x)".into(), 0));
        // An empty count is what makes vanilla's own `parseInt` throw; here it
        // means "no counter", which reaches the same fallback.
        assert_eq!(split_copy_counter("New World ()"), ("New World ()".into(), 0));
        // `.*` is greedy: the *last* parenthesised counter wins.
        assert_eq!(split_copy_counter("A (1) (2)"), ("A (1)".into(), 2));
    }

    #[test]
    fn a_long_name_is_clamped_to_the_filesystem_limit_without_splitting_a_codepoint() {
        let root = temp_root("long-name");
        std::fs::create_dir_all(&root).expect("create root");
        // 400 multi-byte characters: byte-slicing this would panic, which is
        // the crash this test exists for.
        let long: String = std::iter::repeat_n('Ж', 400).collect();
        let name = available_dir_name(&root, &long);
        assert_eq!(name.chars().count(), MAX_FILE_NAME);
        assert!(name.chars().all(|c| c == 'Ж'));
    }

    #[test]
    fn creating_a_world_writes_the_typed_name_and_a_fresh_directory() {
        let root = temp_root("create");
        let dir = create_world_in(&root, "My World!", 1, &[], None).expect("creates");
        assert_eq!(
            dir.file_name().and_then(|n| n.to_str()),
            Some("My World!"),
            "`!` is not an illegal character, so the folder keeps it"
        );
        assert!(
            lodestone_anvil::level_dat::path_in(&dir).is_file(),
            "a world with no level.dat is not one vanilla will open"
        );
        let worlds = list_worlds_in(&root);
        assert_eq!(worlds.len(), 1);
        assert_eq!(worlds[0].display_name, "My World!");
        assert_eq!(worlds[0].game_type, Some(1), "creative was requested");

        // The second create with the same name must be a **different**
        // directory — this is the whole bug the save list exists to fix, so it
        // is asserted directly rather than inferred from `available_dir_name`.
        let second = create_world_in(&root, "My World!", 1, &[], None).expect("creates a second");
        assert_ne!(second, dir, "Create New World must not reopen the first one");
        assert_eq!(
            second.file_name().and_then(|n| n.to_str()),
            Some("My World! (1)")
        );
        let worlds = list_worlds_in(&root);
        assert_eq!(worlds.len(), 2, "both worlds are listed");
        assert!(
            worlds.iter().all(|w| w.display_name == "My World!"),
            "two worlds may share a display name; only the folder is unique"
        );

        // The seed's file must NOT exist yet: `resolve_world_seed` creates it on
        // first open, and that is what makes the requested seed win for a new
        // world. Writing one here would re-introduce that fix's wart.
        assert!(
            !dir.join("data").join("minecraft").join("world_gen_settings.dat").exists(),
            "create must not write world_gen_settings.dat — `resolve_world_seed` \
             owns it, and a pre-written file would make the stored seed win over \
             the one the player typed"
        );
    }

    /// **The control for the customize-write path**: with no
    /// `generator_override`, nothing changes — same assertion as the test
    /// above, restated here so it stands next to the positive case below
    /// rather than only living on an unrelated-looking test.
    #[test]
    fn creating_an_uncustomized_world_still_writes_no_world_gen_settings_file() {
        let root = temp_root("create-uncustomized");
        let dir = create_world_in(&root, "Plain", 0, &[], None).expect("creates");
        assert!(!lodestone_anvil::world_gen_settings::path_in(&dir).exists());
    }

    /// A "Customize Type" choice reaches disk: `world_gen_settings.dat` is
    /// written **at creation time** (not left for `resolve_world_seed` to
    /// create later) with the chosen flat layers plus a real, numeric seed —
    /// the two together, which is the whole reason this write happens here
    /// rather than being left to the ordinary lazy path (see
    /// [`create_world_in`]'s own doc). A literal numeric seed makes the
    /// resolved value predictable, so this checks the exact number rather
    /// than merely "some seed got written".
    #[test]
    fn customizing_a_flat_world_writes_the_layers_and_a_real_seed_at_creation_time() {
        let root = temp_root("create-flat-customized");
        let generator = GeneratorOverride::Flat {
            layers: vec![("minecraft:bedrock".to_string(), 1), ("minecraft:air".to_string(), 59)],
            biome: "minecraft:plains".to_string(),
            features: false,
            lakes: false,
        };
        let dir = create_world_in(&root, "Flat World", 0, &[], Some((&generator, "4242")))
            .expect("creates");

        let path = lodestone_anvil::world_gen_settings::path_in(&dir);
        assert!(path.is_file(), "world_gen_settings.dat must exist right after creation, not \
             only after the world is first opened");
        let settings = lodestone_anvil::world_gen_settings::read_from_file(&path).expect("decodes");
        assert_eq!(settings.seed().expect("a literal seed parses verbatim"), 4242);
        assert!(settings.has_dimensions(), "the customized generator must be present");
    }

    /// The random-seed leg of [`resolve_seed_for_creation`]: an empty typed
    /// seed must still resolve to *some* real `i64` — vanilla's own "empty
    /// means random" rule — and two independent creations must not collide on
    /// the same draw (the discriminating control: a stub that always
    /// returned e.g. `0` would pass an "is present" check but fail this one).
    #[test]
    fn customizing_with_an_empty_seed_still_resolves_a_real_distinct_seed() {
        let root = temp_root("create-flat-random-seed");
        let generator = GeneratorOverride::FixedBiome { biome: "minecraft:desert".to_string() };
        let dir_a = create_world_in(&root, "A", 0, &[], Some((&generator, ""))).expect("creates");
        let dir_b = create_world_in(&root, "B", 0, &[], Some((&generator, ""))).expect("creates");

        let seed_a = lodestone_anvil::world_gen_settings::read_from_file(
            &lodestone_anvil::world_gen_settings::path_in(&dir_a),
        )
        .expect("decodes")
        .seed()
        .expect("a random draw is still a real seed");
        let seed_b = lodestone_anvil::world_gen_settings::read_from_file(
            &lodestone_anvil::world_gen_settings::path_in(&dir_b),
        )
        .expect("decodes")
        .seed()
        .expect("a random draw is still a real seed");
        assert_ne!(seed_a, seed_b, "two independent random draws must not collide");
    }

    #[test]
    fn a_name_of_only_illegal_characters_still_produces_a_usable_folder() {
        let root = temp_root("illegal-only");
        let dir = create_world_in(&root, "///", 0, &[], None).expect("creates");
        assert_eq!(dir.file_name().and_then(|n| n.to_str()), Some("___"));
        assert_eq!(list_worlds_in(&root).len(), 1);
    }

    /// The containment guard, with the traversal case named explicitly: `".."`
    /// would resolve to the saves **root**, which is the directory a world open
    /// must never be pointed at.
    #[test]
    fn world_dir_in_accepts_exactly_one_plain_component() {
        let root = Path::new("/saves");
        assert_eq!(
            world_dir_in(root, "alpha"),
            Some(PathBuf::from("/saves/alpha"))
        );
        assert_eq!(
            world_dir_in(root, "My World (1)"),
            Some(PathBuf::from("/saves/My World (1)"))
        );
        for bad in ["..", ".", "", "a/b", "/abs", "a/", "./a"] {
            assert_eq!(world_dir_in(root, bad), None, "{bad:?} must be refused");
        }
    }

    /// Every entry of [`populated_root`], sorted — the whole-directory
    /// observation the delete gates below measure *before and after*.
    ///
    /// A helper rather than an inline `read_dir` because "nothing outside the
    /// target was touched" is a statement about the **set**, and the only way to
    /// make it one is to compare sets rather than to spot-check names.
    fn entries_of(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .expect("root exists")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Delete removes **the selected world's own directory**, and nothing else in
    /// the saves root.
    ///
    /// The fixture is the reason this is not vacuous: a root with one world
    /// cannot distinguish "removed the right one" from "removed the only one",
    /// and a root with no non-world entries cannot show that the stray file and
    /// the `level.dat`-less folder survive. [`populated_root`]'s seven entries are
    /// asserted as the precondition.
    #[test]
    fn delete_removes_exactly_the_selected_world_and_nothing_else() {
        let root = populated_root("delete-one");
        let before = entries_of(&root);
        assert_eq!(before.len(), 7, "premise: seven entries to choose from");
        assert!(
            list_worlds_in(&root).len() >= 2,
            "premise: more than one world, or 'the right one' is not a question"
        );

        let removed = delete_world_in(&root, "bravo").expect("bravo is a world");
        assert_eq!(removed, root.join("bravo"));
        assert!(!removed.exists(), "the directory itself must be gone");

        let after = entries_of(&root);
        let expected: Vec<String> = before.iter().filter(|n| *n != "bravo").cloned().collect();
        assert_eq!(
            after, expected,
            "delete must remove exactly one entry: the world's own directory"
        );
        assert!(root.is_dir(), "the saves root itself must survive");
        // And the *list* changed by exactly that world.
        let dirs: Vec<String> = list_worlds_in(&root)
            .into_iter()
            .map(|w| w.dir_name)
            .collect();
        assert_eq!(dirs, vec!["alpha", "charlie", "delta", "broken"]);
    }

    /// A **corrupt** world is deletable — vanilla's `canDelete()` is
    /// unconditionally `true`, and the world whose `level.dat` will not decode is
    /// the one you most need to be able to remove.
    ///
    /// This is why [`delete_world_in`]'s third guard is an *existence* check on
    /// `level.dat` rather than a decode: a decode would make exactly this world
    /// permanent.
    #[test]
    fn a_corrupt_world_is_still_deletable() {
        let root = populated_root("delete-corrupt");
        let broken = list_worlds_in(&root)
            .into_iter()
            .find(|w| w.dir_name == "broken")
            .expect("premise: the fixture lists the corrupt world");
        assert!(!broken.readable, "premise: it really is undecodable");
        assert!(broken.can_delete());
        assert!(!broken.can_play(), "and it must stay non-playable");
        delete_world_in(&root, "broken").expect("a corrupt world must be removable");
        assert!(!root.join("broken").exists());
    }

    /// **Each of the three refusals is shown to fire, with a legitimate call as
    /// the executed control.**
    ///
    /// Every arm additionally asserts that the root's entry *set* is unchanged —
    /// an error return that had already deleted something would otherwise look
    /// identical to a refusal.
    #[test]
    fn every_delete_guard_fires_and_a_legitimate_name_is_the_control() {
        let root = populated_root("delete-guards");
        let before = entries_of(&root);

        // (1) The containment check. `..` is the one that matters: it resolves to
        // the saves root, which `remove_dir_all` would happily empty.
        for bad in ["..", ".", "", "a/b", "/etc", "alpha/..", "./alpha"] {
            let err = delete_world_in(&root, bad)
                .err()
                .unwrap_or_else(|| panic!("{bad:?} was accepted"));
            assert!(
                matches!(err, DeleteError::NotAWorldName(_)),
                "{bad:?} must be refused by name, got {err:?}"
            );
            assert_eq!(entries_of(&root), before, "{bad:?} changed the root");
        }
        // Stated separately, because it is the consequence that matters rather
        // than the return value: the root is still a directory with its contents.
        assert!(root.is_dir());

        // (2) A directory with no `level.dat` is not a world.
        let err = delete_world_in(&root, "notaworld").expect_err("not a world");
        assert!(
            matches!(err, DeleteError::NotAWorld(_)),
            "got {err:?}"
        );
        assert!(root.join("notaworld").is_dir(), "and it survives");
        // The same refusal for a plain file, which is the `.DS_Store` case.
        let err = delete_world_in(&root, ".DS_Store").expect_err("not a directory");
        assert!(matches!(err, DeleteError::NotAWorld(_)), "got {err:?}");
        assert!(root.join(".DS_Store").is_file());
        // A name that resolves to nothing at all is the filesystem's answer, not
        // ours — `symlink_metadata` is the first thing that can fail.
        let err = delete_world_in(&root, "no-such-world").expect_err("absent");
        assert!(matches!(err, DeleteError::Io(_)), "got {err:?}");
        assert_eq!(entries_of(&root), before);

        // (3) A symlink is refused, and its **target** survives untouched. This
        // is the assertion that says a delete cannot reach outside `saves/`.
        #[cfg(unix)]
        {
            let outside = std::env::temp_dir().join(format!(
                "lodestone-saves-{}-delete-guards-outside",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&outside);
            std::fs::create_dir_all(&outside).expect("create the outside dir");
            // Given a `level.dat`, so the *only* thing standing between the
            // delete and the target is the symlink guard.
            plant_world(&outside, "victim", "Victim", 1, 0);
            std::os::unix::fs::symlink(outside.join("victim"), root.join("linked"))
                .expect("create the symlink");
            assert!(
                lodestone_anvil::level_dat::path_in(&root.join("linked")).is_file(),
                "premise: the link resolves to a real world, so only the symlink \
                 guard can refuse it"
            );
            let err = delete_world_in(&root, "linked").expect_err("a symlink");
            assert!(matches!(err, DeleteError::Symlink(_)), "got {err:?}");
            assert!(
                outside.join("victim").is_dir(),
                "the symlink's target must be untouched"
            );
            assert!(
                root.join("linked").exists(),
                "and the link itself is left alone rather than half-removed"
            );
            let _ = std::fs::remove_dir_all(&outside);
            std::fs::remove_file(root.join("linked")).expect("tidy the link");
        }

        // -- control ---------------------------------------------------------
        // The refusals above are only about *these* names if a legitimate one
        // succeeds against the same root through the same function.
        let removed = delete_world_in(&root, "alpha").expect("alpha is a world");
        assert_eq!(removed, root.join("alpha"));
        assert!(!removed.exists());
        assert_ne!(
            entries_of(&root),
            before,
            "the control must actually change the root, or every assertion above \
             passes for a `delete_world_in` that never deletes anything"
        );
    }

    /// The date arithmetic, against values produced by Python's `datetime` —
    /// an independent calendar implementation, not our own.
    ///
    /// The stamps below were generated with
    /// `datetime.datetime.fromtimestamp(ms / 1000, datetime.timezone.utc)`, and
    /// `1785182459463` is the `LastPlayed` of the real vanilla `level.dat`
    /// checked into `lodestone-anvil`'s test fixtures — so at least one of these
    /// is a value a real Minecraft server wrote.
    #[test]
    fn the_timestamp_format_matches_an_independent_calendar() {
        for (millis, want) in [
            (0_i64, "1970-01-01 00:00"),
            (1_700_000_000_000, "2023-11-14 22:13"),
            (1_785_182_459_463, "2026-07-27 20:00"),
            // A leap day, which is where a hand-rolled calendar goes wrong.
            (1_583_020_800_000, "2020-03-01 00:00"),
            (1_582_934_400_000, "2020-02-29 00:00"),
        ] {
            assert_eq!(
                format_epoch_millis_utc(millis).as_deref(),
                Some(want),
                "{millis}"
            );
        }
        assert_eq!(
            format_epoch_millis_utc(-1),
            None,
            "the unknown sentinel has no date"
        );
    }

    #[test]
    fn the_info_line_is_vanillas_own_shape() {
        let mut world = WorldSummary {
            dir_name: "alpha".to_string(),
            display_name: "Alpha".to_string(),
            last_played: 1_700_000_000_000,
            game_type: Some(0),
            version_name: Some("26.2".to_string()),
            allow_commands: false,
            hardcore: false,
            readable: true,
        };
        assert_eq!(world.info_line(), "Survival, Version: 26.2");
        world.allow_commands = true;
        assert_eq!(world.info_line(), "Survival, Cheats, Version: 26.2");
        world.hardcore = true;
        assert_eq!(
            world.info_line(),
            "Hardcore, Cheats, Version: 26.2",
            "hardcore replaces the game mode rather than appending to it"
        );
        world.version_name = None;
        assert_eq!(world.info_line(), "Hardcore, Cheats, Version: Unknown");
        assert_eq!(
            world.detail_line(),
            "alpha (2023-11-14 22:13 UTC)",
            "the folder name and the last-played stamp"
        );
        world.last_played = -1;
        assert_eq!(
            world.detail_line(),
            "alpha",
            "no stamp at all when last-played is unknown, matching \
             `WorldSelectionList`'s own `lastPlayed != -1L` guard"
        );
    }

    /// **The OS-side-effect gate.** No test in this crate may reach the
    /// developer's real saves directory, and the mechanism that stops it is
    /// that every operation has an explicit-root twin — not a `cfg!(test)`
    /// early return, which would be a silent skip.
    ///
    /// This asserts the premise: the real root is where the other config files
    /// live, and a temp root is not it. It cannot assert "no test called
    /// `create_world`" — nothing in-process can — so the enforcement is the
    /// source scan in `tests/no_test_touches_the_real_saves_dir.rs`, which reads
    /// this crate's own `.rs` files. That is `CLAUDE.md`'s "grep for the effect,
    /// not the feature" applied to a filesystem write instead of to
    /// `Command::new("open")`.
    #[test]
    fn the_real_root_is_reachable_only_through_the_no_argument_helpers() {
        let root = saves_dir();
        assert!(root.ends_with(SAVES_DIR));
        assert_eq!(root, crate::menu::servers::data_dir().join(SAVES_DIR));
        // A temp root is not the real one, which is the premise every test
        // above rests on and is worth stating once.
        assert_ne!(temp_root("not-the-real-root"), root);
    }
}
