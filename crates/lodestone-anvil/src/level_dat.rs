//! `level.dat` world metadata: a single gzip-wrapped, named-root NBT file.
//!
//! Cited against `.cache/mc/26.2/src/`:
//!
//! - `net/minecraft/nbt/NbtIo.java`: `readCompressed`/`writeCompressed`
//!   (`NbtIo.java:32-34,68-76`) wrap the payload in gzip
//!   (`createDecompressorStream`/`createCompressorStream`,
//!   `NbtIo.java:38-44`: `GZIPInputStream`/`GZIPOutputStream`) — **not**
//!   zlib, unlike the *default* region-chunk scheme. This is the trap issue
//!   #300 itself calls out: don't reuse `region.rs`'s container for this
//!   file, only the shared NBT codec.
//! - `net/minecraft/world/level/storage/LevelStorageSource.java:598`:
//!   `NbtIo.writeCompressed(root, dataFile);` is the real level.dat write
//!   call (distinct from the generic `writeSavedData` at
//!   `LevelStorageSource.java:196-204`, which is for the unrelated per-type
//!   `.dat` files under a world's `data/` folder, e.g. saved game rules —
//!   easy to conflate at a skim, since both call `NbtUtils.addCurrentDataVersion`
//!   then `NbtIo.writeCompressed`).
//! - `net/minecraft/world/level/storage/LevelStorageSource.java:276-277`:
//!   `readLevelDataTagRaw` — `NbtIo.readCompressed(dataFile,
//!   NbtAccounter.uncompressedQuota())` is the read side.
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
//!   matching `LevelStorageSource.TAG_DATA = "Data"`
//!   (`LevelStorageSource.java:93`).
//! - Nested inside `"Data"`, at byte offset 368, is a field named
//!   `"DataVersion"` (an 11-character UTF-8 name, tag byte `0x03` = `Int`)
//!   with value **4903** — this oracle's world was created with the 26.2
//!   server (`.cache/mc/creative/server.jar`), so 4903 is that build's real
//!   `SharedConstants` data version, not a guess.
//!
//! This module models exactly that much: the gzip+named-NBT envelope, and a
//! `DataVersion` accessor into the `"Data"` compound. Every other
//! `LevelData` field (seed, spawn position, game time, weather, game
//! rules, world border, ...) is deliberately unmodelled — see the crate doc
//! for why, and issue [#437](https://github.com/matteopolak/lodestone/issues/437)
//! for where that should land instead.

use crate::{Error, Result};
use lodestone_core::Nbt;
use std::io::Read;
use std::path::Path;

const DATA_FIELD: &str = "Data";
const DATA_VERSION_FIELD: &str = "DataVersion";

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

    /// The `"DataVersion"` field inside `"Data"`, per this module's doc.
    pub fn data_version(&self) -> Result<i32> {
        let data = self.data().ok_or(Error::MissingDataCompound)?;
        match compound_field(data, DATA_VERSION_FIELD) {
            Some(Nbt::Int(version)) => Ok(*version),
            _ => Err(Error::MissingDataVersion),
        }
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
/// NBT, matching `NbtIo.writeCompressed`).
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
