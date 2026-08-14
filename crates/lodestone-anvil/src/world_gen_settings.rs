//! `world_gen_settings.dat` — where 26.2 keeps the **world seed**.
//!
//! # What it is
//!
//! The file that makes reopening a saved world reopen the *same* world.
//! Without it, chunks the player never visited are regenerated from a
//! different seed on the next open, and the world is self-inconsistent at the
//! edges of wherever the player had been — a defect no blocks-only save gate
//! can see, because every block such a gate checks was saved.
//!
//! # It is **not** in `level.dat`, and that is the trap
//!
//! Up to and including 1.16.5 the seed lived at `Data.WorldGenSettings.seed`
//! inside `level.dat`. **In 26.2 it does not.** Verified by decompressing four
//! real world folders with Python's stdlib `gzip` and hand-parsing the NBT with
//! `struct.unpack` — deliberately *not* with this crate's reader, per the
//! standing rule that an expected value must originate outside the code under
//! test:
//!
//! | world | `level.dat` bytes | `DataVersion` | seed field in `level.dat`? |
//! |---|---|---|---|
//! | `.cache/mc/26.2/world` | 513 | 4903 | **none** |
//! | `.cache/mc/creative/world` | 517 | 4903 | **none** |
//! | `.cache/mc/survival/world` | 515 | 4903 | **none** |
//! | `.cache/mc/1.16.5/world` | 2719 | 2586 | `Data.WorldGenSettings.seed` |
//!
//! A 26.2 `level.dat`'s `Data` compound holds only `difficulty_settings`,
//! `Time`, `GameType`, `ServerBrands`, `version`, `LastPlayed`, `spawn`,
//! `Version`, `LevelName`, `initialized`, `WasModded`, `DataVersion`,
//! `allowCommands` and `DataPacks`. So a port that reads 1.16-era docs, or
//! that greps `level.dat` for a seed and finds nothing, will conclude the seed
//! is unpersisted rather than that it moved.
//!
//! Where it moved, cited against `.cache/mc/26.2/src/`:
//!
//! - `LevelStorageSource.writeWorldGenSettings` calls the generic
//!   `writeSavedData(worldFolder, ops, WorldGenSettings.TYPE,
//!   WorldGenSettings.CODEC, …)`.
//! - `LevelStorageSource.writeSavedData` wraps the codec
//!   output as `fullTag.put("data", encoded)`, adds `DataVersion`, and writes
//!   it with `NbtIo.writeCompressed` (gzip, like `level.dat`) to
//!   `type.id().withSuffix(".dat").resolveAgainst(worldFolder.resolve("data"))`
//!   — i.e. **`<world>/data/minecraft/world_gen_settings.dat`**, confirmed
//!   present on disk in all three 26.2 oracle worlds above.
//! - The read side is `LevelStorageSource.readExistingSavedData(…,
//!   WorldGenSettings.TYPE)`, and **vanilla's own fallback when that file is
//!   unreadable regenerates the world with a fresh random seed**: it logs
//!   "Unable to read or access the world gen settings file! Falling back to
//!   the default settings with a random world seed" and builds
//!   `WorldOptions.defaultWithRandomSeed()`.
//!
//! # The layout, verified byte-by-byte
//!
//! `.cache/mc/26.2/world/data/minecraft/world_gen_settings.dat` decompresses
//! to 795 bytes: an unnamed root `Compound` holding
//!
//! ```text
//! root
//!   data: Compound
//!     bonus_chest:         Byte  = 0
//!     seed:                Long  = -195764831     <-- the whole point
//!     generate_structures: Byte  = 1
//!     dimensions:          Compound { minecraft:overworld, :the_nether, :the_end }
//!   DataVersion: Int = 4903
//! ```
//!
//! `seed` is a `Long` (tag 4), not an `Int` — checked against the raw tag byte,
//! not inferred from the value, which happens to fit in 32 bits in this world.
//!
//! # How to change it, and the gotchas
//!
//! - **Unknown fields are preserved.** [`read`] keeps the whole tree and
//!   [`set_seed`](WorldGenSettings::set_seed) rewrites one field in place, so
//!   round-tripping a real vanilla file does not silently drop `dimensions`.
//!   That matters: `dimensions` is the largest part of the file and this crate
//!   models none of its contents.
//! - **[`WorldGenSettings::from_seed`] writes no `dimensions` compound**, so a
//!   world *this* code creates from scratch is not one vanilla could open —
//!   `WorldGenSettings.CODEC` would reject it and fall back to a random seed.
//!   That is a named gap, not an oversight: a Lodestone world is already
//!   missing player data and block entities, so vanilla-openability is not a
//!   property it has to defend. The direction that **is** defended, and gated
//!   against a checked-in real vanilla file, is *reading* a seed vanilla wrote.
//! - **Always gzip**, never zlib — same as `level.dat`, unlike the default
//!   region-chunk scheme. See [`crate::level_dat`]'s doc for that trap.
//! - The write is **not** atomic here (vanilla's own `NbtIo.writeCompressed` is
//!   not either). Losing this file costs the seed, which is why
//!   [`crate::region_source`-style](crate) callers should write it once at
//!   world creation and then leave it alone rather than rewriting per save.
//!
//! # Dependencies
//!
//! [`lodestone_core`]'s NBT codec and `flate2`, exactly as [`crate::level_dat`].

use crate::{Error, Result};
use lodestone_core::Nbt;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The `"data"` wrapper `LevelStorageSource.writeSavedData` puts the codec
/// output under — lowercase, unlike `level.dat`'s `"Data"`
/// (`LevelStorageSource.TAG_DATA`).
const DATA_FIELD: &str = "data";
/// Set by `NbtUtils.addCurrentDataVersion`, called from
/// `LevelStorageSource.writeSavedData`.
const DATA_VERSION_FIELD: &str = "DataVersion";
/// `WorldOptions.CODEC`'s seed field.
const SEED_FIELD: &str = "seed";
/// `WorldOptions.CODEC`'s `generateStructures` field.
const GENERATE_STRUCTURES_FIELD: &str = "generate_structures";
/// `WorldOptions.CODEC`'s `generateBonusChest` field.
const BONUS_CHEST_FIELD: &str = "bonus_chest";

/// `SharedConstants`' data version for the 26.2 server this repo builds
/// against — read out of a real file written by `.cache/mc/creative/server.jar`
/// (see [`crate::level_dat`]'s doc, which measured the same 4903 independently),
/// not guessed.
pub const DATA_VERSION_26_2: i32 = 4903;

/// The path 26.2 stores world-gen settings at, relative to a world folder:
/// `<world_dir>/data/minecraft/world_gen_settings.dat`.
///
/// From `writeSavedData`'s
/// `type.id().withSuffix(".dat").resolveAgainst(worldFolder.resolve("data"))`
/// where `WorldGenSettings.TYPE`'s id is `minecraft:world_gen_settings` — a
/// `ResourceLocation`, so its namespace becomes a directory component.
#[must_use]
pub fn path_in(world_dir: &Path) -> PathBuf {
    world_dir
        .join("data")
        .join("minecraft")
        .join("world_gen_settings.dat")
}

fn compound_field<'a>(nbt: &'a Nbt, name: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields
            .iter()
            .find(|(field_name, _)| field_name == name)
            .map(|(_, value)| value),
        _ => None,
    }
}

/// A parsed `world_gen_settings.dat`: the full root NBT tree, preserved as-is
/// so a field this crate does not model (notably `dimensions`) survives a
/// read/modify/write cycle.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldGenSettings {
    /// The root `Compound` — the one containing `"data"` and `"DataVersion"`.
    pub root: Nbt,
}

impl WorldGenSettings {
    /// Wraps an already-built root compound.
    #[must_use]
    pub fn new(root: Nbt) -> Self {
        Self { root }
    }

    /// A fresh settings file for a brand-new world with `seed`.
    ///
    /// Field order matches what the real 26.2 files carry (`bonus_chest`,
    /// `seed`, `generate_structures`), which is cosmetic — NBT compounds are
    /// unordered — but makes a hexdump diff against a vanilla file readable.
    ///
    /// **No `dimensions` compound is emitted**; see the module doc for why
    /// that is a deliberate, named gap.
    #[must_use]
    pub fn from_seed(seed: i64) -> Self {
        Self {
            root: Nbt::Compound(vec![
                (
                    DATA_FIELD.to_string(),
                    Nbt::Compound(vec![
                        (BONUS_CHEST_FIELD.to_string(), Nbt::Byte(0)),
                        (SEED_FIELD.to_string(), Nbt::Long(seed)),
                        (GENERATE_STRUCTURES_FIELD.to_string(), Nbt::Byte(1)),
                    ]),
                ),
                (
                    DATA_VERSION_FIELD.to_string(),
                    Nbt::Int(DATA_VERSION_26_2),
                ),
            ]),
        }
    }

    /// The `"data"` compound, if the root has one.
    #[must_use]
    pub fn data(&self) -> Option<&Nbt> {
        compound_field(&self.root, DATA_FIELD)
    }

    /// The world seed.
    ///
    /// Accepts an `Int`-tagged seed as well as vanilla's `Long`: nothing this
    /// crate writes produces one, but a third-party tool narrowing a small
    /// seed to `Int` would otherwise read as a corrupt world rather than as
    /// the seed it plainly is.
    ///
    /// # Errors
    ///
    /// [`Error::MissingDataField`] if there is no `"data"` compound, or
    /// [`Error::MissingSeed`] if that compound carries no numeric `"seed"`.
    pub fn seed(&self) -> Result<i64> {
        let data = self.data().ok_or(Error::MissingDataField)?;
        match compound_field(data, SEED_FIELD) {
            Some(Nbt::Long(seed)) => Ok(*seed),
            Some(Nbt::Int(seed)) => Ok(i64::from(*seed)),
            _ => Err(Error::MissingSeed),
        }
    }

    /// Sets (or inserts) the seed, leaving every other field alone.
    ///
    /// # Errors
    ///
    /// [`Error::MissingDataField`] if the root has no `"data"` compound to
    /// write into.
    pub fn set_seed(&mut self, seed: i64) -> Result<()> {
        let Nbt::Compound(root_fields) = &mut self.root else {
            return Err(Error::MissingDataField);
        };
        let Some((_, data)) = root_fields.iter_mut().find(|(name, _)| name == DATA_FIELD) else {
            return Err(Error::MissingDataField);
        };
        let Nbt::Compound(data_fields) = data else {
            return Err(Error::MissingDataField);
        };
        if let Some(entry) = data_fields.iter_mut().find(|(name, _)| name == SEED_FIELD) {
            entry.1 = Nbt::Long(seed);
        } else {
            data_fields.push((SEED_FIELD.to_string(), Nbt::Long(seed)));
        }
        Ok(())
    }

    /// `generate_structures`, or `None` if absent/mistyped.
    #[must_use]
    pub fn generate_structures(&self) -> Option<bool> {
        match compound_field(self.data()?, GENERATE_STRUCTURES_FIELD) {
            Some(Nbt::Byte(b)) => Some(*b != 0),
            _ => None,
        }
    }

    /// `bonus_chest`, or `None` if absent/mistyped.
    #[must_use]
    pub fn bonus_chest(&self) -> Option<bool> {
        match compound_field(self.data()?, BONUS_CHEST_FIELD) {
            Some(Nbt::Byte(b)) => Some(*b != 0),
            _ => None,
        }
    }

    /// The root-level `"DataVersion"`.
    ///
    /// # Errors
    ///
    /// [`Error::MissingDataVersion`] if absent or not an `Int`.
    pub fn data_version(&self) -> Result<i32> {
        match compound_field(&self.root, DATA_VERSION_FIELD) {
            Some(Nbt::Int(version)) => Ok(*version),
            _ => Err(Error::MissingDataVersion),
        }
    }

    /// Whether the root carries a `dimensions` compound — i.e. whether this
    /// came from vanilla (or a round-trip of a vanilla file) rather than from
    /// [`Self::from_seed`]. See the module doc's gap note.
    #[must_use]
    pub fn has_dimensions(&self) -> bool {
        self.data()
            .and_then(|data| compound_field(data, "dimensions"))
            .is_some()
    }
}

/// Decodes a full `world_gen_settings.dat` file's contents (gzip-wrapped
/// named NBT).
///
/// # Errors
///
/// [`Error::NotGzip`] if `bytes` is not a gzip stream, or [`Error::Nbt`] if
/// the decompressed payload is not valid named NBT.
pub fn read(bytes: &[u8]) -> Result<WorldGenSettings> {
    let mut decompressed = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut decompressed)
        .map_err(|_| Error::NotGzip)?;

    let mut reader = lodestone_core::Reader::new(&decompressed);
    let (_, root) = lodestone_core::read_named_nbt(&mut reader).map_err(Error::Nbt)?;
    Ok(WorldGenSettings { root })
}

/// Reads and decodes `path`.
///
/// # Errors
///
/// [`Error::Io`] if the file cannot be read, plus [`read`]'s own errors.
pub fn read_from_file(path: &Path) -> Result<WorldGenSettings> {
    let bytes = std::fs::read(path).map_err(Error::Io)?;
    read(&bytes)
}

/// Encodes `settings` as a full file's contents (gzip-wrapped named NBT,
/// matching `NbtIo.writeCompressed`).
///
/// # Errors
///
/// [`Error::Nbt`] if the tree cannot be encoded, [`Error::Io`] if gzip fails.
pub fn write(settings: &WorldGenSettings) -> Result<Vec<u8>> {
    use std::io::Write;

    let mut writer = lodestone_core::Writer::default();
    lodestone_core::write_named_nbt(&mut writer, "", &settings.root).map_err(Error::Nbt)?;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&writer.into_vec()).map_err(Error::Io)?;
    encoder.finish().map_err(Error::Io)
}

/// Encodes and writes `settings` to `path`, creating parent directories —
/// vanilla's own `FileUtil.createDirectoriesSafe(path.getParent())`, called
/// from `LevelStorageSource.writeSavedData`, needed because
/// `<world>/data/minecraft/` does not exist in a world folder this code has
/// only ever written regions to.
///
/// # Errors
///
/// [`Error::Io`] if the directory or file cannot be written, plus [`write`]'s
/// own errors.
pub fn write_to_file(settings: &WorldGenSettings, path: &Path) -> Result<()> {
    let bytes = write(settings)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    std::fs::write(path, bytes).map_err(Error::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_seed_round_trips_through_our_own_codec() {
        // Weak on its own (`decode(encode(x)) == x`); the load-bearing
        // evidence is `reads_the_seed_a_real_vanilla_26_2_server_wrote` below.
        let settings = WorldGenSettings::from_seed(-195_764_831);
        let bytes = write(&settings).expect("encodes");
        let decoded = read(&bytes).expect("decodes");
        assert_eq!(decoded, settings);
        assert_eq!(decoded.seed().expect("has a seed"), -195_764_831);
        assert_eq!(decoded.data_version().expect("versioned"), DATA_VERSION_26_2);
    }

    #[test]
    fn from_seed_carries_a_full_64_bit_seed() {
        // The `Int`-vs-`Long` distinction the module doc calls out: a seed
        // that does not fit in 32 bits must survive, which an `Nbt::Int`
        // field would silently truncate.
        let seed = -6_148_914_691_236_517_206_i64;
        assert!(i32::try_from(seed).is_err(), "control: seed must not fit i32");
        let bytes = write(&WorldGenSettings::from_seed(seed)).expect("encodes");
        assert_eq!(read(&bytes).expect("decodes").seed().expect("seed"), seed);
    }

    #[test]
    fn set_seed_preserves_every_other_field() {
        let mut settings = WorldGenSettings::new(Nbt::Compound(vec![
            (
                DATA_FIELD.to_string(),
                Nbt::Compound(vec![
                    (SEED_FIELD.to_string(), Nbt::Long(1)),
                    (
                        "dimensions".to_string(),
                        Nbt::Compound(vec![("marker".to_string(), Nbt::Int(7))]),
                    ),
                ]),
            ),
            (DATA_VERSION_FIELD.to_string(), Nbt::Int(4903)),
        ]));
        settings.set_seed(99).expect("data compound exists");
        assert_eq!(settings.seed().expect("seed"), 99);
        assert!(
            settings.has_dimensions(),
            "rewriting the seed must not drop the unmodelled dimensions tree"
        );
    }

    #[test]
    fn set_seed_inserts_when_absent() {
        let mut settings = WorldGenSettings::new(Nbt::Compound(vec![(
            DATA_FIELD.to_string(),
            Nbt::Compound(vec![]),
        )]));
        assert!(matches!(settings.seed(), Err(Error::MissingSeed)));
        settings.set_seed(5).expect("inserts");
        assert_eq!(settings.seed().expect("seed"), 5);
    }

    #[test]
    fn missing_data_field_errors_cleanly() {
        let settings = WorldGenSettings::new(Nbt::Compound(vec![]));
        assert!(matches!(settings.seed(), Err(Error::MissingDataField)));
    }

    #[test]
    fn non_gzip_bytes_error_cleanly() {
        assert!(matches!(read(b"not gzip at all"), Err(Error::NotGzip)));
    }

    #[test]
    fn path_is_the_one_writesaveddata_resolves() {
        let path = path_in(Path::new("/saves/world"));
        assert!(path.ends_with("data/minecraft/world_gen_settings.dat"), "{path:?}");
    }

    /// **The oracle.** Reads a real `world_gen_settings.dat` written by a real
    /// Mojang 26.2 dedicated server, checked in at
    /// `tests/support/world_gen_settings_26_2_vanilla.dat`.
    ///
    /// The expected values did **not** come from this crate: the file was
    /// decompressed with Python's stdlib `gzip` and hand-parsed with
    /// `struct.unpack`, reading the raw tag byte for `seed` (4 = `Long`) rather
    /// than inferring the type from the value. So this asserts our reader
    /// recovers what an external decoder independently read from the same
    /// bytes.
    ///
    /// Checked in rather than `#[ignore]`d against `.cache/` (which
    /// [`crate::level_dat`]'s equivalent test has to do): the file is 795
    /// bytes, so the oracle can run on every `cargo test` instead of only when
    /// someone remembers `--ignored`.
    #[test]
    fn reads_the_seed_a_real_vanilla_26_2_server_wrote() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/support/world_gen_settings_26_2_vanilla.dat");
        let settings = read_from_file(&path).expect("checked-in vanilla fixture decodes");

        assert_eq!(settings.seed().expect("vanilla wrote a seed"), -195_764_831);
        assert_eq!(settings.data_version().expect("versioned"), 4903);
        assert_eq!(settings.generate_structures(), Some(true));
        assert_eq!(settings.bonus_chest(), Some(false));
        assert!(
            settings.has_dimensions(),
            "control: the fixture really is a full vanilla file, not a stub \
             one of our own writers could have produced"
        );
    }

    /// Round-trips the **real vanilla file** through our writer with the seed
    /// changed, and proves the unmodelled `dimensions` tree survives.
    ///
    /// This is the assertion that would fail if `set_seed` were implemented as
    /// "rebuild the compound from the fields we model" — the obvious
    /// implementation, and the one that silently discards most of the file.
    #[test]
    fn rewriting_a_real_vanilla_files_seed_keeps_its_dimensions() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/support/world_gen_settings_26_2_vanilla.dat");
        let mut settings = read_from_file(&path).expect("fixture decodes");
        let dimensions_before = settings
            .data()
            .and_then(|data| compound_field(data, "dimensions"))
            .cloned()
            .expect("fixture has dimensions");

        settings.set_seed(1_234_567_890_123).expect("rewrites");
        let reloaded = read(&write(&settings).expect("encodes")).expect("decodes");

        assert_eq!(reloaded.seed().expect("seed"), 1_234_567_890_123);
        let dimensions_after = reloaded
            .data()
            .and_then(|data| compound_field(data, "dimensions"))
            .cloned()
            .expect("dimensions survived the round trip");
        assert_eq!(dimensions_before, dimensions_after);
    }
}
