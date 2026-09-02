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
//! Where it moved:
//!
//! - The generic per-file-type save routine wraps the codec's encoded output
//!   in an outer compound under a lowercase `"data"` key, adds `DataVersion`,
//!   and writes the whole thing gzip-compressed (like `level.dat`) to a path
//!   built from the file type's own resource-location id resolved against
//!   the world folder's `data` directory — i.e.
//!   **`<world>/data/minecraft/world_gen_settings.dat`**, confirmed present
//!   on disk in all three 26.2 oracle worlds above.
//! - **Vanilla's own fallback when that file is unreadable regenerates the
//!   world with a fresh random seed**: it logs "Unable to read or access the
//!   world gen settings file! Falling back to the default settings with a
//!   random world seed" and builds a default options value with a freshly
//!   rolled seed.
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
//!   the real decode path would reject it for a missing required field and
//!   fall back to a random seed. That is a named gap, not an oversight: a
//!   Lodestone world is already missing player data and block entities, so
//!   vanilla-openability is not a property it has to defend. The direction
//!   that **is** defended, and gated against a checked-in real vanilla file,
//!   is *reading* a seed vanilla wrote.
//! - **Always gzip**, never zlib — same as `level.dat`, unlike the default
//!   region-chunk scheme. See [`crate::level_dat`]'s doc for that trap.
//! - The write is **not** atomic here (a real save's own write of this file
//!   is not either). Losing this file costs the seed, which is why
//!   [`crate::region_source`-style](crate) callers should write it once at
//!   world creation and then leave it alone rather than rewriting per save.
//!
//! # Dependencies
//!
//! [`lodestone_core`]'s NBT codec and `flate2`, exactly as [`crate::level_dat`].

use crate::{Error, Result};
use lodestone_core::{Nbt, NbtTag};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The `"data"` wrapper vanilla's generic per-type-file save path puts the
/// codec output under — lowercase, unlike `level.dat`'s `"Data"`.
const DATA_FIELD: &str = "data";
/// Stamped onto the root tag by vanilla's own save path, the same
/// current-data-version write [`crate::level_dat`] documents.
const DATA_VERSION_FIELD: &str = "DataVersion";
/// The world seed, as vanilla's own world-options codec names it.
const SEED_FIELD: &str = "seed";
/// Whether structures generate, as vanilla's own world-options codec names it.
const GENERATE_STRUCTURES_FIELD: &str = "generate_structures";
/// Whether the bonus chest spawns, as vanilla's own world-options codec names it.
const BONUS_CHEST_FIELD: &str = "bonus_chest";

/// Vanilla's own data version for the 26.2 server this repo builds
/// against — read out of a real file written by `.cache/mc/creative/server.jar`
/// (see [`crate::level_dat`]'s doc, which measured the same 4903 independently),
/// not guessed.
pub const DATA_VERSION_26_2: i32 = 4903;

/// The path 26.2 stores world-gen settings at, relative to a world folder:
/// `<world_dir>/data/minecraft/world_gen_settings.dat`.
///
/// Derived by vanilla's generic per-type-file save path from this file's
/// registered id, `minecraft:world_gen_settings` — a namespaced identifier,
/// so its namespace becomes a directory component and the id (with a `.dat`
/// suffix) becomes the filename, resolved under the world folder's `data/`.
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

fn compound_fields_mut(nbt: &mut Nbt) -> Option<&mut Vec<(String, Nbt)>> {
    match nbt {
        Nbt::Compound(fields) => Some(fields),
        _ => None,
    }
}

/// One layer of vanilla's own flat generator, bottom to top — `block` a
/// namespaced block id, `height` the layer's block count. Field names match
/// vanilla's own layer-info codec (`block`, `height`) verbatim; see
/// [`WorldGenSettings::with_overworld_flat_generator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatLayer<'a> {
    pub block: &'a str,
    pub height: i32,
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

    /// The `"data"` compound's field list, mutably, if the root has one —
    /// mirrors [`crate::level_dat::LevelDat`]'s own `data_fields_mut`, one
    /// field short of worth sharing across the two modules.
    fn data_fields_mut(&mut self) -> Option<&mut Vec<(String, Nbt)>> {
        compound_fields_mut(&mut self.root)?
            .iter_mut()
            .find(|(name, _)| name == DATA_FIELD)
            .and_then(|(_, value)| compound_fields_mut(value))
    }

    /// The `"dimensions"` compound's `"minecraft:overworld"` entry, mutably —
    /// created (with the `dimensions` compound around it, if that was
    /// missing too) rather than requiring it to already exist, since
    /// [`Self::from_seed`] emits no `dimensions` compound at all (this
    /// module's own doc names that gap). Returns `None` only if there is no
    /// `"data"` compound to build under, i.e. the root itself is malformed.
    fn overworld_entry_mut(&mut self) -> Option<&mut Vec<(String, Nbt)>> {
        let data_fields = self.data_fields_mut()?;
        if !data_fields.iter().any(|(name, _)| name == "dimensions") {
            data_fields.push(("dimensions".to_string(), Nbt::Compound(Vec::new())));
        }
        let dimensions = data_fields
            .iter_mut()
            .find(|(name, _)| name == "dimensions")
            .and_then(|(_, value)| compound_fields_mut(value))?;
        if !dimensions.iter().any(|(name, _)| name == "minecraft:overworld") {
            dimensions.push((
                "minecraft:overworld".to_string(),
                Nbt::Compound(vec![(
                    "type".to_string(),
                    Nbt::String("minecraft:overworld".to_string()),
                )]),
            ));
        }
        dimensions
            .iter_mut()
            .find(|(name, _)| name == "minecraft:overworld")
            .and_then(|(_, value)| compound_fields_mut(value))
    }

    /// Sets (or replaces) the overworld entry's `"generator"` field, leaving
    /// its own `"type": "minecraft:overworld"` field (and any
    /// `minecraft:the_nether`/`minecraft:the_end` sibling this settings value
    /// already carries, e.g. from a real vanilla file round-tripped through
    /// [`read`]) alone.
    fn set_overworld_generator(&mut self, generator: Nbt) -> bool {
        let Some(overworld) = self.overworld_entry_mut() else {
            return false;
        };
        if let Some(entry) = overworld.iter_mut().find(|(name, _)| name == "generator") {
            entry.1 = generator;
        } else {
            overworld.push(("generator".to_string(), generator));
        }
        true
    }

    /// Overrides the overworld dimension's generator with vanilla's own flat
    /// ("Superflat") generator shape — the "Customize Type" screen's Flat
    /// half. Field names and nesting match a real 26.2
    /// `world_gen_settings.dat`'s own
    /// `data.dimensions.minecraft:overworld.generator` compound exactly
    /// (`type: "minecraft:flat"`, `settings: {layers, biome, features,
    /// lakes}`, each layer `{block, height}`), hand-decoded byte-for-byte
    /// from `.cache/mc/26.2/world`'s own file — the same discipline this
    /// module's own doc uses for the seed field, independent of this crate's
    /// own NBT reader. `structure_overrides` (a per-preset list of structure
    /// sets to keep enabled) is not modelled: this crate's flat generator has
    /// no structure placement to gate.
    ///
    /// A no-op (returns `self` unchanged) if `layers` is empty — mirrors
    /// [`Self::with_enabled_features`]'s "nothing chosen writes nothing"
    /// rule, though in practice every [`FlatLayer`] caller in this tree
    /// always has vanilla's own bundled default (Classic Flat) to fall back
    /// on, so this only fires for a hand-built, deliberately empty caller.
    #[must_use]
    pub fn with_overworld_flat_generator(
        mut self,
        layers: &[FlatLayer<'_>],
        biome: &str,
        features: bool,
        lakes: bool,
    ) -> Self {
        if layers.is_empty() {
            return self;
        }
        let layer_elements = layers
            .iter()
            .map(|layer| {
                Nbt::Compound(vec![
                    ("block".to_string(), Nbt::String(layer.block.to_string())),
                    ("height".to_string(), Nbt::Int(layer.height)),
                ])
            })
            .collect();
        let settings = Nbt::Compound(vec![
            ("features".to_string(), Nbt::Byte(i8::from(features))),
            ("biome".to_string(), Nbt::String(biome.to_string())),
            ("layers".to_string(), Nbt::List { element_type: NbtTag::Compound, elements: layer_elements }),
            ("lakes".to_string(), Nbt::Byte(i8::from(lakes))),
        ]);
        let generator = Nbt::Compound(vec![
            ("settings".to_string(), settings),
            ("type".to_string(), Nbt::String("minecraft:flat".to_string())),
        ]);
        self.set_overworld_generator(generator);
        self
    }

    /// Overrides the overworld dimension's generator with vanilla's own
    /// fixed-biome noise generator — the "Customize Type" screen's Single
    /// Biome half. `settings: "minecraft:overworld"` is a plain **string**
    /// registry reference, not an inline compound — verified against the
    /// same real file's `minecraft:the_nether`/`minecraft:the_end` entries,
    /// which reference `minecraft:nether`/`minecraft:end` the identical way;
    /// this crate has no noise-generator-settings model of its own to inline
    /// even if the real format called for one. `biome_source.type:
    /// "minecraft:fixed"` plus a single `biome` field is vanilla's own
    /// registered shape for "one biome, everywhere".
    #[must_use]
    pub fn with_overworld_fixed_biome_generator(mut self, biome: &str) -> Self {
        let generator = Nbt::Compound(vec![
            ("settings".to_string(), Nbt::String("minecraft:overworld".to_string())),
            (
                "biome_source".to_string(),
                Nbt::Compound(vec![
                    ("type".to_string(), Nbt::String("minecraft:fixed".to_string())),
                    ("biome".to_string(), Nbt::String(biome.to_string())),
                ]),
            ),
            ("type".to_string(), Nbt::String("minecraft:noise".to_string())),
        ]);
        self.set_overworld_generator(generator);
        self
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
/// matching the real write path).
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
/// matching vanilla's own generic per-type-file save path, which does the
/// same safe directory creation before writing, needed because
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

    /// Walks straight to `data.dimensions.minecraft:overworld.generator`
    /// without going through any accessor this crate defines — the same
    /// "independent of our own reader" discipline the checked-in-fixture test
    /// above uses, just applied to a value this process built instead of one
    /// Mojang's server wrote.
    fn overworld_generator(settings: &WorldGenSettings) -> &Nbt {
        settings
            .data()
            .and_then(|data| compound_field(data, "dimensions"))
            .and_then(|dims| compound_field(dims, "minecraft:overworld"))
            .and_then(|overworld| compound_field(overworld, "generator"))
            .expect("generator was written")
    }

    /// Vanilla's own real "Classic Flat" preset —
    /// `data/minecraft/worldgen/flat_level_generator_preset/classic_flat.json`
    /// in Mojang's own generator data, not derived from this crate: bedrock,
    /// two dirt, one grass block, on plains, with lakes and features off.
    #[test]
    fn with_overworld_flat_generator_round_trips_classic_flats_real_layers() {
        let layers =
            [FlatLayer { block: "minecraft:bedrock", height: 1 }, FlatLayer { block: "minecraft:dirt", height: 2 }, FlatLayer {
                block: "minecraft:grass_block",
                height: 1,
            }];
        let settings =
            WorldGenSettings::from_seed(1).with_overworld_flat_generator(&layers, "minecraft:plains", false, false);
        let reloaded = read(&write(&settings).expect("encodes")).expect("decodes");

        let generator = overworld_generator(&reloaded);
        assert_eq!(compound_field(generator, "type"), Some(&Nbt::String("minecraft:flat".to_string())));
        let settings_compound = compound_field(generator, "settings").expect("settings compound");
        assert_eq!(compound_field(settings_compound, "biome"), Some(&Nbt::String("minecraft:plains".to_string())));
        assert_eq!(compound_field(settings_compound, "features"), Some(&Nbt::Byte(0)));
        assert_eq!(compound_field(settings_compound, "lakes"), Some(&Nbt::Byte(0)));
        let Some(Nbt::List { elements, .. }) = compound_field(settings_compound, "layers") else {
            panic!("layers must be a list");
        };
        assert_eq!(
            elements,
            &[
                Nbt::Compound(vec![
                    ("block".to_string(), Nbt::String("minecraft:bedrock".to_string())),
                    ("height".to_string(), Nbt::Int(1)),
                ]),
                Nbt::Compound(vec![
                    ("block".to_string(), Nbt::String("minecraft:dirt".to_string())),
                    ("height".to_string(), Nbt::Int(2)),
                ]),
                Nbt::Compound(vec![
                    ("block".to_string(), Nbt::String("minecraft:grass_block".to_string())),
                    ("height".to_string(), Nbt::Int(1)),
                ]),
            ]
        );
    }

    #[test]
    fn with_overworld_flat_generator_is_a_no_op_for_an_empty_slice() {
        let settings = WorldGenSettings::from_seed(1).with_overworld_flat_generator(&[], "minecraft:plains", false, false);
        assert!(
            !settings.has_dimensions(),
            "an empty layer list must write nothing, matching with_enabled_features's own \
             empty-input rule"
        );
    }

    #[test]
    fn with_overworld_fixed_biome_generator_round_trips_the_chosen_biome() {
        let settings = WorldGenSettings::from_seed(1).with_overworld_fixed_biome_generator("minecraft:desert");
        let reloaded = read(&write(&settings).expect("encodes")).expect("decodes");

        let generator = overworld_generator(&reloaded);
        assert_eq!(compound_field(generator, "type"), Some(&Nbt::String("minecraft:noise".to_string())));
        // A plain string registry reference, not an inline compound — see
        // this method's own doc for why, checked against the real
        // `minecraft:the_nether`/`minecraft:the_end` entries in the checked-in
        // vanilla fixture.
        assert_eq!(compound_field(generator, "settings"), Some(&Nbt::String("minecraft:overworld".to_string())));
        let biome_source = compound_field(generator, "biome_source").expect("biome_source compound");
        assert_eq!(compound_field(biome_source, "type"), Some(&Nbt::String("minecraft:fixed".to_string())));
        assert_eq!(compound_field(biome_source, "biome"), Some(&Nbt::String("minecraft:desert".to_string())));
    }

    /// **The proof that a different customization produces a different
    /// file** — the standard of proof `docs/README.md`'s world-creation doc
    /// cites for this feature: not that the builder methods exist, but that
    /// two different player choices resolve to two different persisted
    /// generators. Classic Flat's three real layers
    /// (bedrock/dirt/grass) versus the Void preset's single real layer (one
    /// block of air) — both taken from Mojang's own generator data, not from
    /// each other.
    #[test]
    fn two_different_flat_presets_persist_different_generators() {
        let classic = WorldGenSettings::from_seed(1).with_overworld_flat_generator(
            &[FlatLayer { block: "minecraft:bedrock", height: 1 }, FlatLayer { block: "minecraft:dirt", height: 2 }, FlatLayer {
                block: "minecraft:grass_block",
                height: 1,
            }],
            "minecraft:plains",
            false,
            false,
        );
        let void = WorldGenSettings::from_seed(1).with_overworld_flat_generator(
            &[FlatLayer { block: "minecraft:air", height: 1 }],
            "minecraft:the_void",
            true,
            false,
        );

        let classic_bytes = write(&classic).expect("encodes");
        let void_bytes = write(&void).expect("encodes");
        assert_ne!(classic_bytes, void_bytes, "two different presets must persist different bytes");

        let Some(Nbt::List { elements: classic_layers, .. }) =
            compound_field(compound_field(overworld_generator(&classic), "settings").unwrap(), "layers")
        else {
            panic!("classic layers must be a list");
        };
        let Some(Nbt::List { elements: void_layers, .. }) =
            compound_field(compound_field(overworld_generator(&void), "settings").unwrap(), "layers")
        else {
            panic!("void layers must be a list");
        };
        assert_eq!(classic_layers.len(), 3, "Classic Flat has three real layers");
        assert_eq!(void_layers.len(), 1, "The Void has exactly one real layer");
        assert_ne!(classic_layers, void_layers);
    }

    /// Applying a generator override to a **real vanilla file** must not
    /// disturb the sibling dimensions it did not touch — mirrors
    /// `rewriting_a_real_vanilla_files_seed_keeps_its_dimensions`'s own
    /// reasoning, extended from the seed field to the overworld generator.
    #[test]
    fn overriding_the_overworld_generator_preserves_the_real_files_nether_and_end() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/support/world_gen_settings_26_2_vanilla.dat");
        let settings = read_from_file(&path).expect("fixture decodes");
        let dimensions_before = settings
            .data()
            .and_then(|data| compound_field(data, "dimensions"))
            .and_then(|dims| match dims {
                Nbt::Compound(fields) => Some(fields.clone()),
                _ => None,
            })
            .expect("fixture has dimensions");
        let nether_before = dimensions_before
            .iter()
            .find(|(name, _)| name == "minecraft:the_nether")
            .cloned()
            .expect("fixture has a nether entry");
        let end_before =
            dimensions_before.iter().find(|(name, _)| name == "minecraft:the_end").cloned().expect("fixture has an end entry");

        let overridden = settings.with_overworld_fixed_biome_generator("minecraft:jungle");
        let reloaded = read(&write(&overridden).expect("encodes")).expect("decodes");

        let dimensions_after = reloaded
            .data()
            .and_then(|data| compound_field(data, "dimensions"))
            .and_then(|dims| match dims {
                Nbt::Compound(fields) => Some(fields.clone()),
                _ => None,
            })
            .expect("dimensions survived");
        assert!(dimensions_after.contains(&nether_before), "the nether entry must be untouched");
        assert!(dimensions_after.contains(&end_before), "the end entry must be untouched");
        let biome_source = compound_field(overworld_generator(&reloaded), "biome_source").expect("biome_source");
        assert_eq!(compound_field(biome_source, "biome"), Some(&Nbt::String("minecraft:jungle".to_string())));
    }
}
