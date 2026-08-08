//! The per-player `.dat` container — 26.2's `PlayerDataStorage` (issue
//! [#302](https://github.com/matteopolak/lodestone/issues/302)).
//!
//! # What it is
//!
//! The gzip-wrapped named-NBT file one player's persistent state lives in, and
//! the directory it lives in. Deliberately **schema-free**, exactly like
//! [`crate::region`]: this module knows the container (where the file is, how it
//! is compressed, how the write is made crash-safe) and nothing at all about
//! what a player *is*. `lodestone_server::player_data` owns the schema.
//!
//! # The directory is NOT `playerdata/`
//!
//! Every pre-1.21 reference — and every wiki page, and this repo's own first
//! guess — says `<world>/playerdata/<uuid>.dat`. **26.2 stores it at
//! `<world>/players/data/<uuid>.dat`**, alongside sibling `players/stats/` and
//! `players/advancements/` directories. That is not read off a changelog: the
//! oracle world at `.cache/mc/survival/world` has 287 `.dat` files under
//! `players/data/` and **no `playerdata/` directory at all**. A reader pointed
//! at the old path finds nothing, reports "this player is new", and hands the
//! player an empty inventory — silent data loss that looks like correct
//! first-join behaviour, which is why [`dir_in`] exists rather than a literal at
//! each call site.
//!
//! # The `_old` sibling
//!
//! Vanilla writes `<uuid>.dat_old` next to the live file: `PlayerDataStorage`
//! writes a temp file, moves the current file to `.dat_old`, then moves the temp
//! into place, so a crash mid-write costs the *previous* save rather than both.
//! 46 of the oracle world's 287 players have one. [`write_to_file`] does the
//! same three-step dance for the same reason — a half-written player file is
//! indistinguishable from a corrupt one and would cost the player everything.
//!
//! # How to change it, and the gotchas
//!
//! - **The root NBT name is the empty string**, like `level.dat` and unlike the
//!   protocol crates' nameless network NBT. Verified with a foreign reader
//!   against the oracle world's files.
//! - **A missing file is `Ok(None)`, not an error.** Every player's first join
//!   has no file, and turning that into an error would make joining a fresh
//!   world fail. A file that *exists* but does not decode **is** an error: that
//!   is a real save this code cannot read, and overwriting it silently is the
//!   one outcome worth refusing.
//! - This module does not check `DataVersion` — the caller does, through
//!   [`crate::require_supported_data_version`], because only the caller knows
//!   whether a missing version means "corrupt" or "not applicable".
//!
//! # Dependencies
//!
//! `lodestone-core`'s NBT codec, `flate2` for the gzip wrapper, and `std::fs`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use lodestone_core::Nbt;

use crate::{Error, Result};

/// The directory 26.2 keeps per-player `.dat` files in, relative to a world
/// folder: `<world_dir>/players/data`.
///
/// See this module's doc for why it is not `playerdata/`.
#[must_use]
pub fn dir_in(world_dir: &Path) -> PathBuf {
    world_dir.join("players").join("data")
}

/// The file one player's data lives in: `<world_dir>/players/data/<uuid>.dat`.
///
/// `uuid` is formatted by the caller so this module stays free of a uuid
/// dependency; vanilla uses the canonical hyphenated lowercase form, which is
/// what every file in the oracle world is named.
#[must_use]
pub fn path_in(world_dir: &Path, uuid: &str) -> PathBuf {
    dir_in(world_dir).join(format!("{uuid}.dat"))
}

/// Decodes a player `.dat` file's contents (gzip-wrapped named NBT) into its
/// root compound.
pub fn read(bytes: &[u8]) -> Result<Nbt> {
    let mut decompressed = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut decompressed)
        .map_err(|_| Error::NotGzip)?;
    let mut reader = lodestone_core::Reader::new(&decompressed);
    let (_, root) = lodestone_core::read_named_nbt(&mut reader).map_err(Error::Nbt)?;
    Ok(root)
}

/// Reads `path`, or `Ok(None)` if it does not exist.
///
/// See the module doc: "no file" is a player's first join, "a file that will not
/// decode" is a real error.
pub fn read_from_file(path: &Path) -> Result<Option<Nbt>> {
    match std::fs::read(path) {
        Ok(bytes) => read(&bytes).map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::Io(err)),
    }
}

/// Encodes `root` as a player `.dat` file's contents.
pub fn write(root: &Nbt) -> Result<Vec<u8>> {
    let mut writer = lodestone_core::Writer::default();
    lodestone_core::write_named_nbt(&mut writer, "", root).map_err(Error::Nbt)?;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&writer.into_vec()).map_err(Error::Io)?;
    encoder.finish().map_err(Error::Io)
}

/// Writes `root` to `path` through vanilla's own temp/`.dat_old`/rename dance.
///
/// Creates the parent directory if it is missing, so a caller does not have to
/// order that against the first save.
pub fn write_to_file(root: &Nbt, path: &Path) -> Result<()> {
    let bytes = write(root)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    // Same directory as the target, because `rename` is only atomic within one
    // filesystem — a temp file in the OS temp dir can be on another device and
    // would degrade to a copy, which is exactly the non-atomic write this avoids.
    let temp = path.with_extension("dat_tmp");
    std::fs::write(&temp, &bytes).map_err(Error::Io)?;
    let old = path.with_extension("dat_old");
    // Vanilla's `PlayerDataStorage.save` ignores a failure to shuffle the
    // previous file aside (there may not be one), and so do we: the rename
    // below is what has to succeed.
    let _ = std::fs::rename(path, &old);
    std::fs::rename(&temp, path).map_err(Error::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_is_players_data_not_playerdata() {
        let dir = dir_in(Path::new("/w"));
        assert_eq!(dir, Path::new("/w/players/data"));
        assert_eq!(
            path_in(Path::new("/w"), "00dd60bd-39a4-381a-bc60-741f6ae2a0c2"),
            Path::new("/w/players/data/00dd60bd-39a4-381a-bc60-741f6ae2a0c2.dat")
        );
    }

    #[test]
    fn missing_file_is_none_but_undecodable_file_is_an_error() {
        let dir = std::env::temp_dir().join("lodestone-player-dat-gate-c1");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let missing = dir.join("nobody.dat");
        let _ = std::fs::remove_file(&missing);
        assert!(read_from_file(&missing).expect("missing is Ok").is_none());

        // The control for the clause above: a file that exists must NOT read as
        // "this player is new", or a corrupt save would be silently replaced.
        let garbage = dir.join("garbage.dat");
        std::fs::write(&garbage, b"not gzip at all").expect("write");
        assert!(matches!(read_from_file(&garbage), Err(Error::NotGzip)));
        let _ = std::fs::remove_file(&garbage);
    }

    #[test]
    fn write_moves_the_previous_file_aside() {
        let dir = std::env::temp_dir().join("lodestone-player-dat-gate-c2");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("player.dat");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("dat_old"));

        let first = Nbt::Compound(vec![("Score".to_owned(), Nbt::Int(1))]);
        write_to_file(&first, &path).expect("first write");
        assert!(!path.with_extension("dat_old").exists(), "nothing to shuffle yet");

        let second = Nbt::Compound(vec![("Score".to_owned(), Nbt::Int(2))]);
        write_to_file(&second, &path).expect("second write");
        assert_eq!(read_from_file(&path).expect("reads").as_ref(), Some(&second));
        let old = read_from_file(&path.with_extension("dat_old")).expect("reads old");
        assert_eq!(old.as_ref(), Some(&first), "the previous save is kept");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("dat_old"));
    }
}
