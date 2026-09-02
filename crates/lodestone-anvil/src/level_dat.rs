//! `level.dat` world metadata: a single gzip-wrapped, named-root NBT file.
//!
//! Verified against the real 26.2 read/write path, not transcribed from
//! memory:
//!
//! - The payload is wrapped in **gzip**, not zlib — unlike the *default*
//!   region-chunk scheme. This is the trap this crate's own `level_dat`
//!   module exists to avoid: don't reuse `region.rs`'s container for this
//!   file, only the shared NBT codec.
//! - The real level.dat write is its own direct compressed-NBT write,
//!   distinct from the generic per-type `.dat`-file save path used for the
//!   unrelated files under a world's `data/` folder (e.g. saved game
//!   rules) — easy to conflate at a skim, since both stamp the current
//!   data version onto the root tag before writing.
//! - The read side reads the compressed file directly, with the NBT
//!   byte-accounting limit relaxed to unbounded, since decompression
//!   already ran.
//!
//! # The `Data`/`DataVersion` structure, verified against a real file
//!
//! Every value below was read out of
//! `.cache/mc/creative/world/level.dat` — a real file this repo's own
//! creative oracle wrote, decompressed with Python's stdlib `gzip` module
//! and parsed by hand with `struct.unpack`, **not** with this crate's own
//! reader (`decode(encode(x)) == x` against our own writer proves nothing
//! per this repo's own standing rule; an external decoder had to be on at
//! least one side):
//!
//! - Decompresses to 517 bytes, starting `0a 0000` — an unnamed (name
//!   length 0) root `Compound` tag, exactly the "named NBT with an empty
//!   root name" form [`lodestone_core::read_named_nbt`] already implements.
//! - Its first field is `0a 0004 "Data"` — a `Compound` named `"Data"`,
//!   the name every real file uses for this compound.
//! - Nested inside `"Data"`, at byte offset 368, is a field named
//!   `"DataVersion"` (an 11-character UTF-8 name, tag byte `0x03` = `Int`)
//!   with value **4903** — this oracle's world was created with the 26.2
//!   server (`.cache/mc/creative/server.jar`), so 4903 is that build's real
//!   `SharedConstants` data version, not a guess.
//!
//! # What a real 26.2 `level.dat` actually contains
//!
//! Read with the same foreign reader — Python stdlib `gzip` plus a
//! hand-written `struct.unpack` NBT walker sharing no code with this crate —
//! across **all five** 26.2 oracle worlds in `.cache/mc` (`26.2`, `survival`,
//! `creative`, `terrain`, `online262`, plus `oracle`). Every one decodes to
//! the **same 14 fields** inside `"Data"`, and the list is worth stating
//! exactly, because two things people expect are *not* on it:
//!
//! | field | tag | note |
//! |---|---|---|
//! | `LevelName` | `String` | the world's display name |
//! | `GameType` | `Int` | 0 survival, 1 creative, 2 adventure, 3 spectator |
//! | `Time` | `Long` | total ticks the world has run |
//! | `spawn` | `Compound` | `pos` (`IntArray`, 3 entries), `yaw`/`pitch` (`Float`), `dimension` (`String`) |
//! | `difficulty_settings` | `Compound` | `difficulty` (`String`), `hardcore`, `locked` (`Byte`) |
//! | `LastPlayed` | `Long` | epoch millis |
//! | `DataVersion` | `Int` | 4903 for 26.2 |
//! | `Version` | `Compound` | `Id`, `Name`, `Series`, `Snapshot` |
//! | `version` | `Int` | 19133, the *Anvil format* version — not the same field as `Version` or `DataVersion`, and all three coexist |
//! | `DataPacks` | `Compound` | `Enabled`/`Disabled` lists of `String` |
//! | `ServerBrands` | `List<String>` | every brand that has written this world |
//! | `WasModded` | `Byte` | set when a non-`vanilla` brand wrote it |
//! | `allowCommands` | `Byte` | |
//! | `initialized` | `Byte` | |
//!
//! **There is no seed**, in any field, in any of the six files, despite
//! earlier reports to the contrary; it lives in
//! `<world>/data/minecraft/world_gen_settings.dat`, modelled by
//! [`crate::world_gen_settings`]. See that module.
//!
//! **There is no weather and no day-time either**, which is the same mistake
//! one field further on and is worth writing down before someone models them
//! here. 26.2 keeps each in its own file beside the seed, and the real files
//! on disk say so plainly:
//!
//! | what | file | fields (read from the real 26.2 world) |
//! |---|---|---|
//! | weather | `data/minecraft/weather.dat` | `raining`, `rain_time`, `thundering`, `thunder_time`, `clear_weather_time` — **snake_case**, unlike `level.dat`'s mixed casing |
//! | day time | `data/minecraft/world_clocks.dat` | one compound per dimension key (`minecraft:overworld`, `minecraft:the_end`), each with `total_ticks` (`Long`) |
//! | game rules | `data/minecraft/game_rules.dat` | |
//!
//! So `level.dat`'s `Time` is the world's *total* tick count; the per-
//! dimension clock that actually drives the sky is `world_clocks.dat`. A
//! `DayTime` field does not exist in 26.2 at all.
//!
//! Everything else (world border, ender dragon fight, custom boss events)
//! stays unmodelled, and [`LevelDat`] keeps the whole tree so a read/modify/
//! write cycle never drops a field this crate does not understand.

use crate::{Error, Result};
use lodestone_core::{Nbt, NbtTag};
use std::io::Read;
use std::path::{Path, PathBuf};

const DATA_FIELD: &str = "Data";
const DATA_VERSION_FIELD: &str = "DataVersion";
const LEVEL_NAME_FIELD: &str = "LevelName";
const GAME_TYPE_FIELD: &str = "GameType";
const TIME_FIELD: &str = "Time";
const LAST_PLAYED_FIELD: &str = "LastPlayed";
const SPAWN_FIELD: &str = "spawn";
const DIFFICULTY_SETTINGS_FIELD: &str = "difficulty_settings";

/// 26.2's own `SharedConstants` data version, as read out of every real
/// oracle world's `level.dat` with an independent parser.
pub const DATA_VERSION_26_2: i32 = 4903;
/// The Anvil *storage* format version carried in the lowercase `version`
/// field — 19133 in every real 26.2 file, and unrelated to
/// [`DATA_VERSION_26_2`].
pub const ANVIL_VERSION: i32 = 19133;

/// The path 26.2 stores world metadata at, relative to a world folder:
/// `<world_dir>/level.dat`.
///
/// Unlike [`crate::world_gen_settings::path_in`] this one is flat: `level.dat`
/// is *not* under `data/`, because it is written along its own direct
/// compressed-NBT write path rather than by the generic per-type-file save
/// path that resolves a namespaced identifier against `data/`. Conflating
/// the two is exactly the trap this module's header warns about.
#[must_use]
pub fn path_in(world_dir: &Path) -> PathBuf {
    world_dir.join("level.dat")
}

/// Where a player appears on entering the world, as `level.dat`'s `spawn`
/// compound models it in 26.2.
///
/// Pre-1.21 worlds carried flat `SpawnX`/`SpawnY`/`SpawnZ` ints; 26.2 nests
/// them in an `IntArray` and adds an explicit dimension, so code ported from
/// an older schema will look for fields that no longer exist.
#[derive(Debug, Clone, PartialEq)]
pub struct Spawn {
    /// Block position, in the order the `pos` `IntArray` stores it.
    pub pos: [i32; 3],
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// The dimension key, e.g. `minecraft:overworld`.
    pub dimension: String,
}

impl Default for Spawn {
    fn default() -> Self {
        Self {
            pos: [0, 64, 0],
            yaw: 0.0,
            pitch: 0.0,
            dimension: "minecraft:overworld".to_string(),
        }
    }
}

fn compound_fields(nbt: &Nbt) -> Option<&[(String, Nbt)]> {
    match nbt {
        Nbt::Compound(fields) => Some(fields),
        _ => None,
    }
}

fn compound_field<'a>(nbt: &'a Nbt, name: &str) -> Option<&'a Nbt> {
    compound_fields(nbt)?
        .iter()
        .find(|(field_name, _)| field_name == name)
        .map(|(_, value)| value)
}

fn compound_fields_mut(nbt: &mut Nbt) -> Option<&mut Vec<(String, Nbt)>> {
    match nbt {
        Nbt::Compound(fields) => Some(fields),
        _ => None,
    }
}

/// Builds the `spawn` compound in the field order the real 26.2 files carry.
fn spawn_to_nbt(spawn: &Spawn) -> Nbt {
    Nbt::Compound(vec![
        ("pos".to_string(), Nbt::IntArray(spawn.pos.to_vec())),
        ("pitch".to_string(), Nbt::Float(spawn.pitch)),
        (
            "dimension".to_string(),
            Nbt::String(spawn.dimension.clone()),
        ),
        ("yaw".to_string(), Nbt::Float(spawn.yaw)),
    ])
}

/// A parsed `level.dat`: the full root NBT tree (unnamed root `Compound`
/// containing a `"Data"` compound), preserved as-is so round-tripping a
/// real file never silently drops a field this crate doesn't otherwise
/// understand.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelDat {
    /// The root `Compound` tag (i.e. containing the `"Data"` field), exactly
    /// as decoded.
    pub root: Nbt,
}

impl LevelDat {
    /// Wraps an already-built `"Data"` compound (or any NBT tree) as the
    /// root of a new `level.dat`. Most callers building a fresh world want
    /// [`Self::from_data`], which puts `data` under the `"Data"` key for
    /// them, matching the real structure.
    #[must_use]
    pub fn new(root: Nbt) -> Self {
        Self { root }
    }

    /// Wraps `data` as the `"Data"` field of a fresh root compound —
    /// matching the structure `LevelStorageSource` writes, so a caller only
    /// has to build the inner compound.
    #[must_use]
    pub fn from_data(data: Nbt) -> Self {
        Self {
            root: Nbt::Compound(vec![(DATA_FIELD.to_string(), data)]),
        }
    }

    /// The `"Data"` compound, if the root has one.
    #[must_use]
    pub fn data(&self) -> Option<&Nbt> {
        compound_field(&self.root, DATA_FIELD)
    }

    /// The `"Data"` compound's field list, mutably, if the root has one in the
    /// shape [`Self::from_data`]/[`Self::for_new_world`] build.
    fn data_fields_mut(&mut self) -> Option<&mut Vec<(String, Nbt)>> {
        compound_fields_mut(&mut self.root)?
            .iter_mut()
            .find(|(name, _)| name == DATA_FIELD)
            .and_then(|(_, value)| compound_fields_mut(value))
    }

    /// The `"DataVersion"` field inside `"Data"`, per this module's doc.
    pub fn data_version(&self) -> Result<i32> {
        let data = self.data().ok_or(Error::MissingDataCompound)?;
        match compound_field(data, DATA_VERSION_FIELD) {
            Some(Nbt::Int(version)) => Ok(*version),
            _ => Err(Error::MissingDataVersion),
        }
    }

    /// A `level.dat` for a brand-new world, carrying **exactly** the 14 fields
    /// every real 26.2 world in `.cache/mc` was measured to have, in the order
    /// those files carry them.
    ///
    /// The order is cosmetic — NBT compounds are unordered — but it makes a
    /// hexdump diff against a vanilla file readable, which is the only way the
    /// byte-layout gate in this crate's tests is legible when it fails.
    ///
    /// `WasModded` is written as **1**, and that is deliberate rather than
    /// sloppy: the field's meaning is "a brand other than `vanilla` wrote this
    /// world", which is exactly true here. Writing 0 would be a claim about
    /// provenance that is false, and the honest value costs nothing but a
    /// vanilla client's own advisory.
    #[must_use]
    pub fn for_new_world(name: &str, spawn: &Spawn, game_type: i32) -> Self {
        Self::from_data(Nbt::Compound(vec![
            (
                DIFFICULTY_SETTINGS_FIELD.to_string(),
                Nbt::Compound(vec![
                    ("difficulty".to_string(), Nbt::String("easy".to_string())),
                    ("hardcore".to_string(), Nbt::Byte(0)),
                    ("locked".to_string(), Nbt::Byte(0)),
                ]),
            ),
            (TIME_FIELD.to_string(), Nbt::Long(0)),
            (GAME_TYPE_FIELD.to_string(), Nbt::Int(game_type)),
            (
                "ServerBrands".to_string(),
                Nbt::List {
                    element_type: NbtTag::String,
                    elements: vec![Nbt::String("lodestone".to_string())],
                },
            ),
            ("version".to_string(), Nbt::Int(ANVIL_VERSION)),
            (LAST_PLAYED_FIELD.to_string(), Nbt::Long(0)),
            (SPAWN_FIELD.to_string(), spawn_to_nbt(spawn)),
            (
                "Version".to_string(),
                Nbt::Compound(vec![
                    ("Snapshot".to_string(), Nbt::Byte(0)),
                    ("Series".to_string(), Nbt::String("main".to_string())),
                    ("Id".to_string(), Nbt::Int(DATA_VERSION_26_2)),
                    ("Name".to_string(), Nbt::String("26.2".to_string())),
                ]),
            ),
            (LEVEL_NAME_FIELD.to_string(), Nbt::String(name.to_string())),
            ("initialized".to_string(), Nbt::Byte(1)),
            ("WasModded".to_string(), Nbt::Byte(1)),
            (
                DATA_VERSION_FIELD.to_string(),
                Nbt::Int(DATA_VERSION_26_2),
            ),
            ("allowCommands".to_string(), Nbt::Byte(0)),
            (
                "DataPacks".to_string(),
                Nbt::Compound(vec![
                    (
                        "Enabled".to_string(),
                        Nbt::List {
                            element_type: NbtTag::String,
                            elements: vec![Nbt::String("vanilla".to_string())],
                        },
                    ),
                    (
                        "Disabled".to_string(),
                        Nbt::List {
                            element_type: NbtTag::End,
                            elements: Vec::new(),
                        },
                    ),
                ]),
            ),
        ]))
    }

    /// Adds the `enabled_features` field to the `"Data"` compound, for the
    /// experimental feature flags a player turned on in Create New World's
    /// Experiments screen.
    ///
    /// `ids` are bare flag ids with no namespace — [`ExperimentFlag`]'s own
    /// `id()` shape (the shell's world-creation menu passes plain strings;
    /// not depended on from here). The written list always carries
    /// `"minecraft:vanilla"` alongside them: every real feature-flag set a
    /// freshly created world can have already contains it by default, and
    /// vanilla's own construction path only ever *joins* onto that default
    /// rather than replacing it. Each id gets the `minecraft:` namespace
    /// prefix a namespaced identifier's wire form always carries.
    ///
    /// A no-op for an empty slice, deliberately not folded into
    /// [`Self::for_new_world`]: every real 26.2 `level.dat` this crate has
    /// measured (this module's own doc) omits `enabled_features` entirely,
    /// because every measured world had no experiment turned on — the
    /// vanilla codec's `lenientOptionalFieldOf` default. Writing the field
    /// only when the player actually chose something keeps that parity for
    /// the common case instead of adding a field vanilla itself would not.
    #[must_use]
    pub fn with_enabled_features(mut self, ids: &[String]) -> Self {
        if ids.is_empty() {
            return self;
        }
        let mut elements = vec![Nbt::String("minecraft:vanilla".to_string())];
        elements.extend(ids.iter().map(|id| Nbt::String(format!("minecraft:{id}"))));
        if let Some(fields) = self.data_fields_mut() {
            fields.push((
                "enabled_features".to_string(),
                Nbt::List {
                    element_type: NbtTag::String,
                    elements,
                },
            ));
        }
        self
    }

    /// The world's display name, or `None` if absent or mistyped.
    #[must_use]
    pub fn level_name(&self) -> Option<&str> {
        match compound_field(self.data()?, LEVEL_NAME_FIELD) {
            Some(Nbt::String(name)) => Some(name),
            _ => None,
        }
    }

    /// `GameType`: 0 survival, 1 creative, 2 adventure, 3 spectator.
    #[must_use]
    pub fn game_type(&self) -> Option<i32> {
        match compound_field(self.data()?, GAME_TYPE_FIELD) {
            Some(Nbt::Int(kind)) => Some(*kind),
            _ => None,
        }
    }

    /// `enabled_features`, verbatim (with whatever namespace the list carries,
    /// `minecraft:` for everything [`Self::with_enabled_features`] writes) —
    /// empty if the field is absent, matching vanilla's own default (the
    /// base feature-flag set, no experiment turned on).
    #[must_use]
    pub fn enabled_features(&self) -> Vec<String> {
        let Some(data) = self.data() else {
            return Vec::new();
        };
        match compound_field(data, "enabled_features") {
            Some(Nbt::List { elements, .. }) => elements
                .iter()
                .filter_map(|element| match element {
                    Nbt::String(id) => Some(id.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// `Time`: total ticks this world has run.
    ///
    /// Note this is **not** the day time. 26.2 keeps the per-dimension clock
    /// that drives the sky in `data/minecraft/world_clocks.dat`, and has no
    /// `DayTime` field at all — see the module doc.
    #[must_use]
    pub fn time(&self) -> Option<i64> {
        match compound_field(self.data()?, TIME_FIELD) {
            Some(Nbt::Long(ticks)) => Some(*ticks),
            _ => None,
        }
    }

    /// `LastPlayed`, in epoch milliseconds.
    #[must_use]
    pub fn last_played(&self) -> Option<i64> {
        match compound_field(self.data()?, LAST_PLAYED_FIELD) {
            Some(Nbt::Long(at)) => Some(*at),
            _ => None,
        }
    }

    /// The `difficulty` string inside `difficulty_settings` — 26.2 nests it,
    /// where pre-1.21 worlds had a flat `Difficulty` byte.
    #[must_use]
    pub fn difficulty(&self) -> Option<&str> {
        let settings = compound_field(self.data()?, DIFFICULTY_SETTINGS_FIELD)?;
        match compound_field(settings, "difficulty") {
            Some(Nbt::String(name)) => Some(name),
            _ => None,
        }
    }

    /// `Version.Name` — the human version string a real file carries
    /// (`"26.2"` in every 26.2 world measured above), or `None` when the
    /// nested `Version` compound is absent or carries no `Name`.
    ///
    /// **Not [`DATA_VERSION_26_2`] and not the lowercase `version` field.**
    /// All three coexist in one file and mean different things (see the module
    /// doc's table); this is the only one that is a *display* string, the one
    /// vanilla shows on a world-select row. A `None` here matches vanilla's
    /// own placeholder for an unrecognised version, not an error.
    #[must_use]
    pub fn version_name(&self) -> Option<&str> {
        let version = compound_field(self.data()?, "Version")?;
        match compound_field(version, "Name") {
            Some(Nbt::String(name)) => Some(name),
            _ => None,
        }
    }

    /// `allowCommands` — decides whether a world-select row says "Cheats".
    /// Absent or mistyped is `false`, matching a fresh world.
    #[must_use]
    pub fn allow_commands(&self) -> bool {
        matches!(
            self.data().and_then(|d| compound_field(d, "allowCommands")),
            Some(Nbt::Byte(b)) if *b != 0
        )
    }

    /// The `hardcore` byte inside `difficulty_settings`.
    ///
    /// Nested beside [`Self::difficulty`] rather than flat: 26.2 moved both
    /// into `difficulty_settings`, so code ported from a pre-1.21 schema looks
    /// for a top-level `hardcore` that is not there.
    #[must_use]
    pub fn hardcore(&self) -> bool {
        let Some(settings) = self
            .data()
            .and_then(|d| compound_field(d, DIFFICULTY_SETTINGS_FIELD))
        else {
            return false;
        };
        matches!(compound_field(settings, "hardcore"), Some(Nbt::Byte(b)) if *b != 0)
    }

    /// The world spawn, or `None` if the `spawn` compound is absent or does
    /// not carry a 3-entry `pos`.
    #[must_use]
    pub fn spawn(&self) -> Option<Spawn> {
        let spawn = compound_field(self.data()?, SPAWN_FIELD)?;
        let pos = match compound_field(spawn, "pos") {
            Some(Nbt::IntArray(xs)) if xs.len() == 3 => [xs[0], xs[1], xs[2]],
            _ => return None,
        };
        let angle = |key: &str| match compound_field(spawn, key) {
            Some(Nbt::Float(v)) => *v,
            _ => 0.0,
        };
        let dimension = match compound_field(spawn, "dimension") {
            Some(Nbt::String(key)) => key.clone(),
            _ => "minecraft:overworld".to_string(),
        };
        Some(Spawn {
            pos,
            yaw: angle("yaw"),
            pitch: angle("pitch"),
            dimension,
        })
    }

    /// Replaces the world spawn, leaving every other field alone.
    ///
    /// # Errors
    ///
    /// [`Error::MissingDataCompound`] if the root has no `"Data"` compound.
    pub fn set_spawn(&mut self, spawn: &Spawn) -> Result<()> {
        self.set_data_field(SPAWN_FIELD, spawn_to_nbt(spawn))
    }

    /// Replaces `Time`, the world's total elapsed ticks.
    ///
    /// # Errors
    ///
    /// [`Error::MissingDataCompound`] if the root has no `"Data"` compound.
    pub fn set_time(&mut self, ticks: i64) -> Result<()> {
        self.set_data_field(TIME_FIELD, Nbt::Long(ticks))
    }

    /// Replaces `LastPlayed`, in epoch milliseconds.
    ///
    /// # Errors
    ///
    /// [`Error::MissingDataCompound`] if the root has no `"Data"` compound.
    pub fn set_last_played(&mut self, at_millis: i64) -> Result<()> {
        self.set_data_field(LAST_PLAYED_FIELD, Nbt::Long(at_millis))
    }

    /// Sets (or inserts) one field of the `"Data"` compound **in place**,
    /// leaving field order and every other entry untouched.
    ///
    /// In place rather than remove-then-push because a rewritten `level.dat`
    /// that reorders its fields diffs against a vanilla file as though every
    /// value changed, which makes the byte-layout gate unreadable exactly when
    /// it is trying to tell you something.
    ///
    /// # Errors
    ///
    /// [`Error::MissingDataCompound`] if the root has no `"Data"` compound.
    pub fn set_data_field(&mut self, field: &str, value: Nbt) -> Result<()> {
        let Nbt::Compound(root_fields) = &mut self.root else {
            return Err(Error::MissingDataCompound);
        };
        let Some((_, data)) = root_fields.iter_mut().find(|(name, _)| name == DATA_FIELD) else {
            return Err(Error::MissingDataCompound);
        };
        let Nbt::Compound(data_fields) = data else {
            return Err(Error::MissingDataCompound);
        };
        if let Some(entry) = data_fields.iter_mut().find(|(name, _)| name == field) {
            entry.1 = value;
        } else {
            data_fields.push((field.to_string(), value));
        }
        Ok(())
    }

    /// Sets (or inserts) the `"DataVersion"` field inside `"Data"`.
    pub fn set_data_version(&mut self, version: i32) -> Result<()> {
        let Nbt::Compound(root_fields) = &mut self.root else {
            return Err(Error::MissingDataCompound);
        };
        let Some((_, data)) = root_fields
            .iter_mut()
            .find(|(name, _)| name == DATA_FIELD)
        else {
            return Err(Error::MissingDataCompound);
        };
        let Nbt::Compound(data_fields) = data else {
            return Err(Error::MissingDataCompound);
        };
        if let Some(entry) = data_fields
            .iter_mut()
            .find(|(name, _)| name == DATA_VERSION_FIELD)
        {
            entry.1 = Nbt::Int(version);
        } else {
            data_fields.push((DATA_VERSION_FIELD.to_string(), Nbt::Int(version)));
        }
        Ok(())
    }
}

/// Decodes `bytes` (a full `level.dat` file's contents: gzip-wrapped named
/// NBT) into a [`LevelDat`].
pub fn read(bytes: &[u8]) -> Result<LevelDat> {
    let mut decompressed = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut decompressed)
        .map_err(|_| Error::NotGzip)?;

    let mut reader = lodestone_core::Reader::new(&decompressed);
    let (_, root) = lodestone_core::read_named_nbt(&mut reader).map_err(Error::Nbt)?;
    Ok(LevelDat { root })
}

/// Reads and decodes `path` as a `level.dat` file.
pub fn read_from_file(path: &Path) -> Result<LevelDat> {
    let bytes = std::fs::read(path).map_err(Error::Io)?;
    read(&bytes)
}

/// Encodes `level` as a full `level.dat` file's contents (gzip-wrapped named
/// NBT, matching the real write path).
pub fn write(level: &LevelDat) -> Result<Vec<u8>> {
    use std::io::Write;

    let mut writer = lodestone_core::Writer::default();
    lodestone_core::write_named_nbt(&mut writer, "", &level.root).map_err(Error::Nbt)?;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&writer.into_vec()).map_err(Error::Io)?;
    encoder.finish().map_err(Error::Io)
}

/// Encodes and writes `level` to `path`.
pub fn write_to_file(level: &LevelDat, path: &Path) -> Result<()> {
    let bytes = write(level)?;
    std::fs::write(path, bytes).map_err(Error::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_data(version: i32) -> Nbt {
        Nbt::Compound(vec![
            ("LevelName".to_string(), Nbt::String("New World".to_string())),
            ("RandomSeed".to_string(), Nbt::Long(42)),
            (DATA_VERSION_FIELD.to_string(), Nbt::Int(version)),
        ])
    }

    #[test]
    fn round_trips_through_our_own_codec() {
        let level = LevelDat::from_data(sample_data(4903));
        let bytes = write(&level).expect("encodes");
        let decoded = read(&bytes).expect("decodes");
        assert_eq!(decoded, level);
        assert_eq!(decoded.data_version().expect("has DataVersion"), 4903);
    }

    #[test]
    fn set_data_version_updates_in_place() {
        let mut level = LevelDat::from_data(sample_data(100));
        level.set_data_version(200).expect("field exists");
        assert_eq!(level.data_version().expect("has DataVersion"), 200);
    }

    #[test]
    fn set_data_version_inserts_when_absent() {
        let data = Nbt::Compound(vec![(
            "LevelName".to_string(),
            Nbt::String("No version yet".to_string()),
        )]);
        let mut level = LevelDat::from_data(data);
        assert!(matches!(
            level.data_version(),
            Err(Error::MissingDataVersion)
        ));
        level.set_data_version(4903).expect("inserts");
        assert_eq!(level.data_version().expect("now present"), 4903);
    }

    #[test]
    fn missing_data_compound_errors_cleanly() {
        // Corrupt-input control: a root with no "Data" field at all must
        // not panic when asked for a version.
        let level = LevelDat::new(Nbt::Compound(vec![]));
        assert!(matches!(
            level.data_version(),
            Err(Error::MissingDataCompound)
        ));
    }

    #[test]
    fn non_gzip_bytes_error_cleanly() {
        assert!(matches!(read(b"not gzip at all"), Err(Error::NotGzip)));
    }

    /// The checked-in real vanilla file: `.cache/mc/26.2/world/level.dat` as
    /// written by the 26.2 server jar, copied verbatim into
    /// `tests/support/level_dat_26_2_vanilla.dat` (384 bytes) so these gates
    /// need no oracle world and are **not** `#[ignore]`d.
    fn vanilla_fixture() -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/support/level_dat_26_2_vanilla.dat");
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// Every expected value below came from the **foreign reader**: Python's
    /// stdlib `gzip` plus a `struct.unpack` NBT walker sharing no code with
    /// this crate, run over this exact file and five sibling 26.2 worlds. That
    /// is what makes this gate external evidence rather than
    /// `decode(encode(x)) == x`.
    #[test]
    fn reads_every_modelled_field_a_real_vanilla_26_2_server_wrote() {
        let level = read(&vanilla_fixture()).expect("decodes a real vanilla level.dat");
        assert_eq!(level.level_name(), Some("world"));
        assert_eq!(level.game_type(), Some(0));
        assert_eq!(level.time(), Some(274_249));
        assert_eq!(level.last_played(), Some(1_785_182_459_463));
        assert_eq!(level.difficulty(), Some("easy"));
        assert_eq!(level.data_version().expect("has DataVersion"), 4903);
        // The three world-select fields (the version-name string, the
        // cheats flag, the hardcore flag), read out of this same file by the
        // same foreign parser: `Version = {Snapshot: 0, Series: "main", Id: 4903,
        // Name: "26.2"}`, `allowCommands = 0`, `difficulty_settings.hardcore = 0`.
        // All three land on the *default*-looking answer here, which is why
        // `the_hardcore_and_cheats_detectors_fire_on_a_world_that_has_them` is
        // the control: without it "false" is equally consistent with an
        // accessor that always says false.
        assert_eq!(level.version_name(), Some("26.2"));
        assert!(!level.allow_commands());
        assert!(!level.hardcore());
        let spawn = level.spawn().expect("has a spawn compound");
        assert_eq!(spawn.pos, [0, -60, 0]);
        assert_eq!(spawn.dimension, "minecraft:overworld");
        assert_eq!((spawn.yaw, spawn.pitch), (0.0, 0.0));
    }

    /// A correction pinned as a gate so it cannot be re-asserted by a future
    /// reader: a 26.2 `level.dat` carries
    /// **no seed**, under any spelling. The seed is in
    /// `data/minecraft/world_gen_settings.dat` — see [`crate::world_gen_settings`].
    #[test]
    fn a_real_26_2_level_dat_carries_no_seed_under_any_spelling() {
        let level = read(&vanilla_fixture()).expect("decodes");
        let Some(Nbt::Compound(fields)) = level.data().cloned() else {
            panic!("real file has a Data compound");
        };
        let named: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            named.len(),
            14,
            "the real 26.2 schema is 14 fields; got {named:?}"
        );
        for name in &named {
            assert!(
                !name.to_ascii_lowercase().contains("seed"),
                "a 26.2 level.dat must carry no seed field, but found {name:?}"
            );
        }
        // And the control for that assertion: the detector does fire on a
        // field that *is* named like a seed. Without this, "no match" is
        // equally consistent with a broken search.
        let planted = ["WorldGenSettings", "RandomSeed"];
        assert!(
            planted
                .iter()
                .any(|name| name.to_ascii_lowercase().contains("seed")),
            "the seed-name detector itself is broken"
        );
    }

    /// **The byte-layout gate, and the strongest evidence here.**
    ///
    /// Decode a real vanilla file and re-encode it with *our* writer, then
    /// compare the uncompressed NBT payload byte for byte against vanilla's
    /// own. The expected value originates entirely outside this crate — it is
    /// literally Mojang's bytes — so unlike a round trip through our own codec
    /// this cannot be satisfied by two symmetric misunderstandings of tag
    /// order, string encoding, `IntArray` framing or compound termination.
    ///
    /// Compressed bytes are deliberately *not* compared: gzip output depends
    /// on the encoder, and a mismatch there would say nothing about the schema.
    #[test]
    fn re_encoding_a_real_vanilla_file_reproduces_mojangs_own_bytes() {
        let raw = vanilla_fixture();
        let mut vanilla_payload = Vec::new();
        flate2::read::GzDecoder::new(&raw[..])
            .read_to_end(&mut vanilla_payload)
            .expect("the fixture is gzip");

        let level = read(&raw).expect("decodes");
        let mut writer = lodestone_core::Writer::default();
        lodestone_core::write_named_nbt(&mut writer, "", &level.root).expect("encodes");
        let ours = writer.into_vec();

        assert_eq!(
            ours.len(),
            vanilla_payload.len(),
            "our re-encode is {} bytes against vanilla's {}",
            ours.len(),
            vanilla_payload.len()
        );
        if ours != vanilla_payload {
            let at = ours
                .iter()
                .zip(&vanilla_payload)
                .position(|(a, b)| a != b)
                .unwrap_or(0);
            panic!(
                "first differing byte at offset {at}: ours {:#04x}, vanilla {:#04x}",
                ours[at], vanilla_payload[at]
            );
        }
    }

    /// A fresh world must claim the same *schema* a real one has — same key
    /// set, no inventions and no omissions. Comparing against the real file's
    /// own key list rather than a hand-written one is the point: a hand-written
    /// expectation would just restate whatever `for_new_world` happens to emit.
    #[test]
    fn a_new_world_carries_exactly_the_real_schemas_field_set() {
        let real = read(&vanilla_fixture()).expect("decodes");
        let fresh = LevelDat::for_new_world("New World", &Spawn::default(), 0);

        let names = |level: &LevelDat| -> Vec<String> {
            let Some(Nbt::Compound(fields)) = level.data().cloned() else {
                panic!("has a Data compound");
            };
            let mut names: Vec<String> = fields.into_iter().map(|(name, _)| name).collect();
            names.sort();
            names
        };
        assert_eq!(
            names(&fresh),
            names(&real),
            "a fresh world's field set must match a real 26.2 world's"
        );
        assert_eq!(fresh.level_name(), Some("New World"));
        assert_eq!(fresh.time(), Some(0));
        assert_eq!(fresh.spawn().expect("has spawn").pos, [0, 64, 0]);
    }

    /// **An empty flag list must not add a field**, matching every real 26.2
    /// `level.dat` this crate has measured (no `enabled_features` at all — see
    /// this module's own 14-field doc) and keeping
    /// [`a_new_world_carries_exactly_the_real_schemas_field_set`]'s schema
    /// assertion true for the common case of a world where nothing was ever
    /// toggled.
    #[test]
    fn with_enabled_features_is_a_no_op_for_an_empty_slice() {
        let plain = LevelDat::for_new_world("New World", &Spawn::default(), 0);
        let untouched = plain.clone().with_enabled_features(&[]);
        assert_eq!(
            untouched, plain,
            "an empty flag list must leave the level.dat byte-for-byte identical"
        );
        assert!(untouched.enabled_features().is_empty());
    }

    /// **The real path a toggled Experiments flag takes to disk** (issue #693):
    /// `with_enabled_features`, a real gzip round trip, and read back with
    /// [`LevelDat::enabled_features`] — a decoder sharing its parsing with
    /// every other accessor in this file but not with the encoder under test,
    /// so this is not the closed `decode(encode(x)) == x` loop this repo's own
    /// evidence rules warn against on its own; the real safeguard is
    /// [`re_encoding_a_real_vanilla_file_reproduces_mojangs_own_bytes`]
    /// pinning this crate's writer against Mojang's own bytes elsewhere in
    /// this schema, and vanilla's `FeatureFlagRegistry::codec` (this method's
    /// doc) fixing the `minecraft:`-namespaced list shape being asserted here.
    #[test]
    fn with_enabled_features_round_trips_the_chosen_flags_plus_vanilla() {
        let level = LevelDat::for_new_world("New World", &Spawn::default(), 0)
            .with_enabled_features(&["redstone_experiments".to_string()]);
        let round_tripped = read(&write(&level).expect("encodes")).expect("decodes");
        assert_eq!(
            round_tripped.enabled_features(),
            vec![
                "minecraft:vanilla".to_string(),
                "minecraft:redstone_experiments".to_string(),
            ],
            "the base vanilla flag must survive alongside the chosen one, both \
             carrying the minecraft: namespace Identifier's wire form uses"
        );
    }

    /// A read/modify/write cycle must not drop or reorder the fields this
    /// crate does not model — the property that lets us open a world a real
    /// server made, play it, and hand it back.
    #[test]
    fn editing_a_real_file_preserves_every_other_field_and_its_order() {
        let mut level = read(&vanilla_fixture()).expect("decodes");
        let before = match level.data().cloned() {
            Some(Nbt::Compound(fields)) => fields,
            _ => panic!("has a Data compound"),
        };

        level.set_time(999_111).expect("sets Time");
        level
            .set_spawn(&Spawn {
                pos: [12, 70, -34],
                yaw: 90.0,
                pitch: -5.0,
                dimension: "minecraft:overworld".to_string(),
            })
            .expect("sets spawn");

        let round_tripped = read(&write(&level).expect("encodes")).expect("decodes");
        assert_eq!(round_tripped.time(), Some(999_111));
        let spawn = round_tripped.spawn().expect("has spawn");
        assert_eq!(spawn.pos, [12, 70, -34]);
        assert_eq!((spawn.yaw, spawn.pitch), (90.0, -5.0));

        let after = match round_tripped.data().cloned() {
            Some(Nbt::Compound(fields)) => fields,
            _ => panic!("has a Data compound"),
        };
        let key_order = |fields: &[(String, Nbt)]| -> Vec<String> {
            fields.iter().map(|(name, _)| name.clone()).collect()
        };
        assert_eq!(
            key_order(&after),
            key_order(&before),
            "editing must not reorder or drop fields"
        );
        for (name, value) in &before {
            if name == TIME_FIELD || name == SPAWN_FIELD {
                continue;
            }
            let still = compound_field(round_tripped.data().expect("data"), name);
            assert_eq!(
                still,
                Some(value),
                "unmodelled field {name} changed across a read/modify/write"
            );
        }
    }

    /// **The control** for the three assertions the real-file gate makes in
    /// the false direction.
    ///
    /// A real vanilla world is `allowCommands = 0`, `hardcore = 0` and does
    /// carry a `Version.Name`, so that gate cannot distinguish a working
    /// accessor from one that returns `false`/`None` unconditionally. This
    /// drives the *other* branch of each: a world that really is hardcore with
    /// cheats on and no `Version` compound at all.
    #[test]
    fn the_hardcore_and_cheats_detectors_fire_on_a_world_that_has_them() {
        let level = LevelDat::from_data(Nbt::Compound(vec![
            (
                DIFFICULTY_SETTINGS_FIELD.to_string(),
                Nbt::Compound(vec![
                    ("difficulty".to_string(), Nbt::String("hard".to_string())),
                    ("hardcore".to_string(), Nbt::Byte(1)),
                ]),
            ),
            ("allowCommands".to_string(), Nbt::Byte(1)),
        ]));
        assert!(level.hardcore(), "the hardcore detector never fires");
        assert!(level.allow_commands(), "the cheats detector never fires");
        assert_eq!(
            level.version_name(),
            None,
            "a file with no Version compound must report no version name, \
             not a fabricated one"
        );
        // And a `Version` compound whose `Name` is present is read from
        // `Name`, never from `Id`/`Series` — the three coexist.
        let mut with_version = level;
        with_version
            .set_data_field(
                "Version",
                Nbt::Compound(vec![
                    ("Id".to_string(), Nbt::Int(1)),
                    ("Series".to_string(), Nbt::String("main".to_string())),
                    ("Name".to_string(), Nbt::String("1.21.11".to_string())),
                ]),
            )
            .expect("inserts");
        assert_eq!(with_version.version_name(), Some("1.21.11"));
    }

    #[test]
    fn level_dat_sits_at_the_world_root_not_under_data() {
        let path = path_in(Path::new("/worlds/demo"));
        assert_eq!(path, Path::new("/worlds/demo/level.dat"));
        assert_ne!(
            path,
            crate::world_gen_settings::path_in(Path::new("/worlds/demo")),
            "level.dat and world_gen_settings.dat must not resolve to the same file"
        );
    }

    #[test]
    #[ignore = "requires .cache/mc/creative/world/level.dat (this repo's creative oracle world; not checked in, see scripts/live-oracles/creative.sh)"]
    fn real_creative_oracle_level_dat_reports_the_expected_data_version() {
        // Independent-oracle evidence, not our own writer: the expected
        // value (4903) came from decompressing this exact file with
        // Python's stdlib `gzip` and hand-parsing the NBT bytes with
        // `struct.unpack` — see this module's doc comment for the byte
        // offsets. This test only proves our *reader* recovers the same
        // value from the same real bytes; it does not touch `write`.
        //
        // `#[ignore]`d rather than skip-on-missing-file: this repo's own
        // standing rule is that a test whose precondition silently
        // downgrades a missing fixture to a pass is a vacuous test. Run
        // explicitly with `cargo test -p lodestone-anvil -- --ignored`.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache/mc/creative/world/level.dat");
        let level = read_from_file(&path).unwrap_or_else(|e| {
            panic!(
                "no real level.dat at {} to verify against ({e}); fetch/generate it first",
                path.display()
            )
        });
        assert_eq!(level.data_version().expect("has DataVersion"), 4903);
    }
}
