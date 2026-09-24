//! Loss reporting for an Anvil-to-native-world import.
//!
//! This module deliberately stops before conversion. It inventories which
//! source values have a typed destination in the initial native vocabulary,
//! which values would be discarded, and which malformed values make an import
//! unsafe. The report keeps paths, coordinates, and reasons, but never an NBT
//! value that the native format cannot use. See `docs/anvil-import-preflight.md`.

use crate::{level_dat, world_gen_settings};
use lodestone_core::Nbt;

/// A source file or record that contributed to an import preflight.
///
/// This is intentionally identifying metadata only. In particular, no variant
/// contains the NBT payload read from that source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportSource {
    /// The world's top-level `level.dat` file.
    LevelDat,
    /// The world's `data/minecraft/world_gen_settings.dat` file.
    WorldGenSettings,
    /// One decoded chunk NBT root.
    Chunk {
        /// Dimension identifier supplied by the region walker.
        dimension: String,
        /// Absolute chunk X coordinate.
        x: i32,
        /// Absolute chunk Z coordinate.
        z: i32,
    },
    /// One decoded player NBT root.
    Player {
        /// The caller's stable player identifier, normally a UUID filename.
        identifier: String,
    },
    /// A file the native importer has no registered extension for.
    AuxiliaryFile {
        /// World-relative slash-separated path.
        path: String,
    },
}

/// A field or record location in an Anvil source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    /// The source containing this item.
    pub source: ImportSource,
    /// Dot-separated path inside the source, or `"$"` for a whole NBT root.
    pub path: String,
}

impl SourceLocation {
    fn level(path: &str) -> Self {
        Self {
            source: ImportSource::LevelDat,
            path: path.to_string(),
        }
    }

    fn world_gen(path: &str) -> Self {
        Self {
            source: ImportSource::WorldGenSettings,
            path: path.to_string(),
        }
    }
}

/// A typed native field that the future converter may populate.
///
/// `SupportedData` means the source value has a declared native destination;
/// it does not claim that a full Anvil converter has been wired yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeField {
    /// Native world `game_data_version`.
    GameDataVersion,
    /// Native world `default_game_mode`.
    DefaultGameMode,
    /// Native world `seed`.
    WorldSeed,
    /// Native world spawn block position.
    SpawnPosition,
    /// Native world spawn dimension.
    SpawnDimension,
    /// Native chunk column coordinates supplied to the record key and body.
    ChunkCoordinates,
    /// Native chunk data-version census.
    ChunkDataVersion,
    /// Native section Y coordinate.
    ChunkSectionY,
    /// Native per-section block-state palettes and indices.
    ChunkBlockStates,
    /// Native per-section three-dimensional biome cells and surface answer.
    ChunkBiomes,
    /// Native `MOTION_BLOCKING` heightmap.
    ChunkMotionBlocking,
    /// Native per-section sky-light arrays or uniform values.
    ChunkSkyLight,
    /// Native per-section block-light arrays or uniform values.
    ChunkBlockLight,
}

/// A source value with a typed destination in the initial native vocabulary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupportedData {
    /// Where the source value was found.
    pub location: SourceLocation,
    /// The field a later converter may populate.
    pub destination: NativeField,
}

/// Why an otherwise readable source value cannot be represented.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LossReason {
    /// The native vocabulary has no field for this source value.
    NoNativeDestination,
    /// Chunk block-state conversion has not been specified yet.
    ChunkMappingUnavailable,
    /// Player-state conversion has not been specified yet.
    PlayerMappingUnavailable,
    /// No extension/schema registration accepts this auxiliary data.
    UnregisteredExtension,
}

/// A source value that a confirmed lossy import would discard.
///
/// The value itself is deliberately absent: this is a loss report, not a
/// compatibility payload cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsupportedData {
    /// Where the discarded value came from.
    pub location: SourceLocation,
    /// Why the native importer cannot retain it.
    pub reason: LossReason,
}

/// Why conversion cannot proceed even after a caller accepts ordinary loss.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockerReason {
    /// A required source field was missing or had a different NBT type.
    MissingOrMalformedValue,
    /// The source's data version is not the version this importer can inspect.
    UnsupportedDataVersion,
    /// The spawn dimension is not one of the native built-in dimensions.
    UnsupportedSpawnDimension,
}

/// A malformed or incompatible source value that prevents conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportBlocker {
    /// Where the blocking value should be repaired or upgraded.
    pub location: SourceLocation,
    /// Why acknowledging loss cannot make this value safe to convert.
    pub reason: BlockerReason,
}

/// The decision every import caller must make after inspecting a report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LossDecision {
    /// Do not convert this world.
    Abort,
    /// Convert and intentionally discard every [`UnsupportedData`] entry.
    ProceedAndDiscardUnsupported,
}

/// The outcome of applying a caller's explicit [`LossDecision`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "pass this result to the conversion boundary; do not convert after an aborted or blocked preflight"]
pub enum ImportAuthorization {
    /// The caller explicitly declined conversion.
    Aborted,
    /// The inventory has no lossy entries and no blockers.
    Lossless,
    /// The caller accepted the report's known data loss.
    LossAccepted {
        /// Number of source entries that the conversion will discard.
        discarded_entries: usize,
    },
    /// A malformed or incompatible source cannot be converted safely.
    Blocked {
        /// Number of blocking entries in the report.
        blockers: usize,
    },
}

impl ImportAuthorization {
    /// Whether a future conversion boundary may start conversion.
    #[must_use]
    pub fn permits_conversion(self) -> bool {
        matches!(self, Self::Lossless | Self::LossAccepted { .. })
    }
}

/// A completed, payload-free inventory of an Anvil import.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PreflightReport {
    supported: Vec<SupportedData>,
    unsupported: Vec<UnsupportedData>,
    blockers: Vec<ImportBlocker>,
}

impl PreflightReport {
    /// Starts a new inventory.
    #[must_use]
    pub fn builder() -> PreflightBuilder {
        PreflightBuilder::default()
    }

    /// Values with a declared typed native destination.
    #[must_use]
    pub fn supported(&self) -> &[SupportedData] {
        &self.supported
    }

    /// Values a confirmed lossy import would discard.
    #[must_use]
    pub fn unsupported(&self) -> &[UnsupportedData] {
        &self.unsupported
    }

    /// Values that require source repair or an importer upgrade before import.
    #[must_use]
    pub fn blockers(&self) -> &[ImportBlocker] {
        &self.blockers
    }

    /// Applies the caller's required, explicit choice.
    ///
    /// A lossy report never silently becomes an authorization. Blockers win
    /// over a loss acknowledgement: a caller must repair or upgrade the source
    /// rather than treating an incompatible value as disposable data.
    #[must_use]
    pub fn decide(&self, decision: LossDecision) -> ImportAuthorization {
        if !self.blockers.is_empty() {
            return ImportAuthorization::Blocked {
                blockers: self.blockers.len(),
            };
        }
        match decision {
            LossDecision::Abort => ImportAuthorization::Aborted,
            LossDecision::ProceedAndDiscardUnsupported if self.unsupported.is_empty() => {
                ImportAuthorization::Lossless
            }
            LossDecision::ProceedAndDiscardUnsupported => ImportAuthorization::LossAccepted {
                discarded_entries: self.unsupported.len(),
            },
        }
    }

    /// Combines reports in the caller's supplied order.
    ///
    /// A world-scale caller uses this to make one authorization cover every
    /// selected source member. The entries stay payload-free and retain their
    /// original source locations, so the combined report is still suitable
    /// for presenting a precise repair or data-loss decision.
    #[must_use]
    pub fn combine(reports: impl IntoIterator<Item = Self>) -> Self {
        let mut combined = Self::default();
        for report in reports {
            combined.supported.extend(report.supported);
            combined.unsupported.extend(report.unsupported);
            combined.blockers.extend(report.blockers);
        }
        combined
    }
}

/// Incrementally builds a [`PreflightReport`] while an Anvil walker decodes a
/// world. It borrows decoded NBT only long enough to classify it.
#[derive(Clone, Debug, Default)]
pub struct PreflightBuilder {
    report: PreflightReport,
}

impl PreflightBuilder {
    /// Inspects the typed `level.dat` wrapper without retaining its NBT tree.
    pub fn inspect_level_dat(&mut self, level: &level_dat::LevelDat) {
        let Nbt::Compound(root) = &level.root else {
            self.block(SourceLocation::level("$"), BlockerReason::MissingOrMalformedValue);
            return;
        };
        let mut found_data = false;
        for (name, value) in root {
            if name == "Data" {
                found_data = true;
                self.inspect_level_data(value);
            } else {
                self.loss(SourceLocation::level(name), LossReason::NoNativeDestination);
            }
        }
        if !found_data {
            self.block(
                SourceLocation::level("Data"),
                BlockerReason::MissingOrMalformedValue,
            );
        }
    }

    /// Inspects the typed world-generation settings wrapper without retaining
    /// its NBT tree.
    pub fn inspect_world_gen_settings(&mut self, settings: &world_gen_settings::WorldGenSettings) {
        let Nbt::Compound(root) = &settings.root else {
            self.block(
                SourceLocation::world_gen("$"),
                BlockerReason::MissingOrMalformedValue,
            );
            return;
        };
        let mut found_data = false;
        let mut found_version = false;
        for (name, value) in root {
            match name.as_str() {
                "data" => {
                    found_data = true;
                    self.inspect_world_gen_data(value);
                }
                "DataVersion" => {
                    found_version = true;
                    self.inspect_data_version(SourceLocation::world_gen("DataVersion"), value);
                }
                _ => self.loss(SourceLocation::world_gen(name), LossReason::NoNativeDestination),
            }
        }
        if !found_data {
            self.block(
                SourceLocation::world_gen("data"),
                BlockerReason::MissingOrMalformedValue,
            );
        }
        if !found_version {
            self.block(
                SourceLocation::world_gen("DataVersion"),
                BlockerReason::MissingOrMalformedValue,
            );
        }
    }

    /// Reports a decoded chunk. Native chunk conversion remains deliberately
    /// unavailable until its palette and registry mapping is specified.
    pub fn inspect_chunk(&mut self, dimension: impl Into<String>, x: i32, z: i32, chunk: &Nbt) {
        let source = ImportSource::Chunk {
            dimension: dimension.into(),
            x,
            z,
        };
        if matches!(chunk, Nbt::Compound(_)) {
            self.loss(
                SourceLocation {
                    source,
                    path: "$".to_string(),
                },
                LossReason::ChunkMappingUnavailable,
            );
        } else {
            self.block(
                SourceLocation {
                    source,
                    path: "$".to_string(),
                },
                BlockerReason::MissingOrMalformedValue,
            );
        }
    }

    /// Inspects the subset of one chunk that the version-1 native record can
    /// consume.
    ///
    /// This is intentionally separate from [`Self::inspect_chunk`], which is
    /// the conservative report used by a future whole-world walker while no
    /// chunk consumer has been selected. The bounded consumer calls this
    /// method and receives field-level loss entries for block entities, ticks,
    /// structures, and other source payloads it will drop.
    pub fn inspect_native_chunk(
        &mut self,
        dimension: impl Into<String>,
        x: i32,
        z: i32,
        chunk: &Nbt,
    ) {
        let source = ImportSource::Chunk {
            dimension: dimension.into(),
            x,
            z,
        };
        let location = |path: String| SourceLocation {
            source: source.clone(),
            path,
        };
        let Nbt::Compound(fields) = chunk else {
            self.block(location("$".to_owned()), BlockerReason::MissingOrMalformedValue);
            return;
        };

        let mut data_version = false;
        let mut x_pos = false;
        let mut z_pos = false;
        let mut sections = false;
        for (name, value) in fields {
            match name.as_str() {
                "DataVersion" => {
                    data_version = true;
                    if matches!(value, Nbt::Int(level_dat::DATA_VERSION_26_2)) {
                        self.support(location(name.clone()), NativeField::ChunkDataVersion);
                    } else {
                        self.block(location(name.clone()), BlockerReason::UnsupportedDataVersion);
                    }
                }
                "xPos" => {
                    x_pos = true;
                    if matches!(value, Nbt::Int(value) if *value == x) {
                        self.support(location(name.clone()), NativeField::ChunkCoordinates);
                    } else {
                        self.block(location(name.clone()), BlockerReason::MissingOrMalformedValue);
                    }
                }
                "zPos" => {
                    z_pos = true;
                    if matches!(value, Nbt::Int(value) if *value == z) {
                        self.support(location(name.clone()), NativeField::ChunkCoordinates);
                    } else {
                        self.block(location(name.clone()), BlockerReason::MissingOrMalformedValue);
                    }
                }
                "sections" => {
                    sections = true;
                    self.inspect_native_chunk_sections(&source, value);
                }
                "Heightmaps" => self.inspect_native_heightmaps(&source, value),
                "Status"
                | "yPos"
                | "LastUpdate"
                | "InhabitedTime"
                | "isLightOn"
                | "block_entities"
                | "block_ticks"
                | "fluid_ticks"
                | "structures"
                | "entities"
                | "PostProcessing" => {
                    self.loss(location(name.clone()), LossReason::NoNativeDestination);
                }
                _ => self.loss(location(name.clone()), LossReason::NoNativeDestination),
            }
        }
        if !data_version {
            self.block(
                location("DataVersion".to_owned()),
                BlockerReason::MissingOrMalformedValue,
            );
        }
        if !x_pos {
            self.block(location("xPos".to_owned()), BlockerReason::MissingOrMalformedValue);
        }
        if !z_pos {
            self.block(location("zPos".to_owned()), BlockerReason::MissingOrMalformedValue);
        }
        if !sections {
            self.block(
                location("sections".to_owned()),
                BlockerReason::MissingOrMalformedValue,
            );
        }
    }

    fn inspect_native_chunk_sections(&mut self, source: &ImportSource, value: &Nbt) {
        let location = |path: String| SourceLocation {
            source: source.clone(),
            path,
        };
        let Nbt::List { elements, .. } = value else {
            self.block(
                location("sections".to_owned()),
                BlockerReason::MissingOrMalformedValue,
            );
            return;
        };
        for (index, section) in elements.iter().enumerate() {
            let path = format!("sections[{index}]");
            let Nbt::Compound(fields) = section else {
                self.block(location(path), BlockerReason::MissingOrMalformedValue);
                continue;
            };
            let mut found_y = false;
            for (name, value) in fields {
                let field_path = format!("sections[{index}].{name}");
                match name.as_str() {
                    "Y" => {
                        found_y = true;
                        if matches!(value, Nbt::Byte(_)) {
                            self.support(location(field_path), NativeField::ChunkSectionY);
                        } else {
                            self.block(
                                location(field_path),
                                BlockerReason::MissingOrMalformedValue,
                            );
                        }
                    }
                    "block_states" => {
                        if matches!(value, Nbt::Compound(_)) {
                            self.support(location(field_path), NativeField::ChunkBlockStates);
                        } else {
                            self.block(
                                location(field_path),
                                BlockerReason::MissingOrMalformedValue,
                            );
                        }
                    }
                    "biomes" => {
                        if matches!(value, Nbt::Compound(_)) {
                            self.support(location(field_path), NativeField::ChunkBiomes);
                        } else {
                            self.block(
                                location(field_path),
                                BlockerReason::MissingOrMalformedValue,
                            );
                        }
                    }
                    "SkyLight" | "BlockLight" => {
                        if matches!(value, Nbt::ByteArray(bytes) if bytes.len() == 2048) {
                            let destination = if name == "SkyLight" {
                                NativeField::ChunkSkyLight
                            } else {
                                NativeField::ChunkBlockLight
                            };
                            self.support(location(field_path), destination);
                        } else {
                            self.block(
                                location(field_path),
                                BlockerReason::MissingOrMalformedValue,
                            );
                        }
                    }
                    _ => self.loss(location(field_path), LossReason::NoNativeDestination),
                }
            }
            if !found_y {
                self.block(
                    location(format!("sections[{index}].Y")),
                    BlockerReason::MissingOrMalformedValue,
                );
            }
        }
    }

    fn inspect_native_heightmaps(&mut self, source: &ImportSource, value: &Nbt) {
        let location = |path: String| SourceLocation {
            source: source.clone(),
            path,
        };
        let Nbt::Compound(fields) = value else {
            self.block(
                location("Heightmaps".to_owned()),
                BlockerReason::MissingOrMalformedValue,
            );
            return;
        };
        let mut found_motion_blocking = false;
        for (name, value) in fields {
            let path = format!("Heightmaps.{name}");
            if name == "MOTION_BLOCKING" {
                found_motion_blocking = true;
                if matches!(value, Nbt::LongArray(values) if !values.is_empty()) {
                    self.support(location(path), NativeField::ChunkMotionBlocking);
                } else {
                    self.block(location(path), BlockerReason::MissingOrMalformedValue);
                }
            } else {
                self.loss(location(path), LossReason::NoNativeDestination);
            }
        }
        if !found_motion_blocking {
            self.loss(
                location("Heightmaps.MOTION_BLOCKING".to_owned()),
                LossReason::NoNativeDestination,
            );
        }
    }

    /// Reports a decoded player record. No player mapping is retained as an
    /// opaque extension while the typed conversion is unavailable.
    pub fn inspect_player(&mut self, identifier: impl Into<String>, player: &Nbt) {
        let source = ImportSource::Player {
            identifier: identifier.into(),
        };
        if matches!(player, Nbt::Compound(_)) {
            self.loss(
                SourceLocation {
                    source,
                    path: "$".to_string(),
                },
                LossReason::PlayerMappingUnavailable,
            );
        } else {
            self.block(
                SourceLocation {
                    source,
                    path: "$".to_string(),
                },
                BlockerReason::MissingOrMalformedValue,
            );
        }
    }

    /// Reports a world-relative auxiliary file for which no extension is
    /// registered. Its bytes are intentionally not accepted or retained.
    pub fn inspect_unregistered_auxiliary_file(&mut self, path: impl Into<String>) {
        self.loss(
            SourceLocation {
                source: ImportSource::AuxiliaryFile { path: path.into() },
                path: "$".to_string(),
            },
            LossReason::UnregisteredExtension,
        );
    }

    /// Finishes the inventory and releases the builder.
    #[must_use]
    pub fn finish(self) -> PreflightReport {
        self.report
    }

    fn inspect_level_data(&mut self, data: &Nbt) {
        let Nbt::Compound(fields) = data else {
            self.block(
                SourceLocation::level("Data"),
                BlockerReason::MissingOrMalformedValue,
            );
            return;
        };
        let mut version = false;
        let mut game_mode = false;
        let mut spawn = false;
        for (name, value) in fields {
            match name.as_str() {
                "DataVersion" => {
                    version = true;
                    self.inspect_data_version(SourceLocation::level("Data.DataVersion"), value);
                }
                "GameType" => {
                    game_mode = true;
                    if matches!(value, Nbt::Int(0..=3)) {
                        self.support(
                            SourceLocation::level("Data.GameType"),
                            NativeField::DefaultGameMode,
                        );
                    } else {
                        self.block(
                            SourceLocation::level("Data.GameType"),
                            BlockerReason::MissingOrMalformedValue,
                        );
                    }
                }
                "spawn" => {
                    spawn = true;
                    self.inspect_spawn(value);
                }
                _ => self.loss(
                    SourceLocation::level(&format!("Data.{name}")),
                    LossReason::NoNativeDestination,
                ),
            }
        }
        if !version {
            self.block(
                SourceLocation::level("Data.DataVersion"),
                BlockerReason::MissingOrMalformedValue,
            );
        }
        if !game_mode {
            self.block(
                SourceLocation::level("Data.GameType"),
                BlockerReason::MissingOrMalformedValue,
            );
        }
        if !spawn {
            self.block(
                SourceLocation::level("Data.spawn"),
                BlockerReason::MissingOrMalformedValue,
            );
        }
    }

    fn inspect_world_gen_data(&mut self, data: &Nbt) {
        let Nbt::Compound(fields) = data else {
            self.block(
                SourceLocation::world_gen("data"),
                BlockerReason::MissingOrMalformedValue,
            );
            return;
        };
        let mut seed = false;
        for (name, value) in fields {
            if name == "seed" {
                seed = true;
                if matches!(value, Nbt::Long(_)) {
                    self.support(SourceLocation::world_gen("data.seed"), NativeField::WorldSeed);
                } else {
                    self.block(
                        SourceLocation::world_gen("data.seed"),
                        BlockerReason::MissingOrMalformedValue,
                    );
                }
            } else {
                self.loss(
                    SourceLocation::world_gen(&format!("data.{name}")),
                    LossReason::NoNativeDestination,
                );
            }
        }
        if !seed {
            self.block(
                SourceLocation::world_gen("data.seed"),
                BlockerReason::MissingOrMalformedValue,
            );
        }
    }

    fn inspect_data_version(&mut self, location: SourceLocation, value: &Nbt) {
        if matches!(value, Nbt::Int(level_dat::DATA_VERSION_26_2)) {
            self.support(location, NativeField::GameDataVersion);
        } else {
            self.block(location, BlockerReason::UnsupportedDataVersion);
        }
    }

    fn inspect_spawn(&mut self, spawn: &Nbt) {
        let Nbt::Compound(fields) = spawn else {
            self.block(
                SourceLocation::level("Data.spawn"),
                BlockerReason::MissingOrMalformedValue,
            );
            return;
        };
        let mut position = false;
        let mut dimension = false;
        for (name, value) in fields {
            match name.as_str() {
                "pos" => {
                    position = true;
                    if matches!(value, Nbt::IntArray(values) if values.len() == 3) {
                        self.support(
                            SourceLocation::level("Data.spawn.pos"),
                            NativeField::SpawnPosition,
                        );
                    } else {
                        self.block(
                            SourceLocation::level("Data.spawn.pos"),
                            BlockerReason::MissingOrMalformedValue,
                        );
                    }
                }
                "dimension" => {
                    dimension = true;
                    if matches!(value, Nbt::String(value) if is_builtin_dimension(value)) {
                        self.support(
                            SourceLocation::level("Data.spawn.dimension"),
                            NativeField::SpawnDimension,
                        );
                    } else {
                        self.block(
                            SourceLocation::level("Data.spawn.dimension"),
                            BlockerReason::UnsupportedSpawnDimension,
                        );
                    }
                }
                _ => self.loss(
                    SourceLocation::level(&format!("Data.spawn.{name}")),
                    LossReason::NoNativeDestination,
                ),
            }
        }
        if !position {
            self.block(
                SourceLocation::level("Data.spawn.pos"),
                BlockerReason::MissingOrMalformedValue,
            );
        }
        if !dimension {
            self.block(
                SourceLocation::level("Data.spawn.dimension"),
                BlockerReason::MissingOrMalformedValue,
            );
        }
    }

    fn support(&mut self, location: SourceLocation, destination: NativeField) {
        self.report.supported.push(SupportedData {
            location,
            destination,
        });
    }

    fn loss(&mut self, location: SourceLocation, reason: LossReason) {
        self.report.unsupported.push(UnsupportedData { location, reason });
    }

    fn block(&mut self, location: SourceLocation, reason: BlockerReason) {
        self.report.blockers.push(ImportBlocker { location, reason });
    }
}

fn is_builtin_dimension(value: &str) -> bool {
    matches!(value, "minecraft:overworld" | "minecraft:the_nether" | "minecraft:the_end")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_26_2_fixtures_report_typed_values_and_known_loss() {
        let level = level_dat::read(include_bytes!("../tests/support/level_dat_26_2_vanilla.dat"))
            .expect("checked-in level.dat fixture decodes");
        let settings = world_gen_settings::read(include_bytes!(
            "../tests/support/world_gen_settings_26_2_vanilla.dat"
        ))
        .expect("checked-in world-gen fixture decodes");

        let mut builder = PreflightReport::builder();
        builder.inspect_level_dat(&level);
        builder.inspect_world_gen_settings(&settings);
        let report = builder.finish();

        assert!(report.blockers().is_empty(), "26.2 fixtures must be importable");
        assert!(report.supported().iter().any(|item| {
            item.location == SourceLocation::level("Data.DataVersion")
                && item.destination == NativeField::GameDataVersion
        }));
        assert!(report.supported().iter().any(|item| {
            item.location == SourceLocation::level("Data.spawn.pos")
                && item.destination == NativeField::SpawnPosition
        }));
        assert!(report.supported().iter().any(|item| {
            item.location == SourceLocation::world_gen("data.seed")
                && item.destination == NativeField::WorldSeed
        }));
        assert!(report.unsupported().iter().any(|item| {
            item.location == SourceLocation::level("Data.LevelName")
                && item.reason == LossReason::NoNativeDestination
        }));
        assert!(report.unsupported().iter().any(|item| {
            item.location == SourceLocation::world_gen("data.dimensions")
                && item.reason == LossReason::NoNativeDestination
        }));
    }

    #[test]
    fn lossy_report_needs_a_specific_confirmation() {
        let level = level_dat::LevelDat::from_data(Nbt::Compound(vec![
            ("DataVersion".to_string(), Nbt::Int(level_dat::DATA_VERSION_26_2)),
            ("GameType".to_string(), Nbt::Int(1)),
            (
                "spawn".to_string(),
                Nbt::Compound(vec![
                    ("pos".to_string(), Nbt::IntArray(vec![1, 80, -3])),
                    ("dimension".to_string(), Nbt::String("minecraft:overworld".to_string())),
                ]),
            ),
            ("LevelName".to_string(), Nbt::String("lossy".to_string())),
        ]));
        let mut builder = PreflightReport::builder();
        builder.inspect_level_dat(&level);
        let report = builder.finish();

        assert_eq!(report.unsupported().len(), 1);
        assert_eq!(report.decide(LossDecision::Abort), ImportAuthorization::Aborted);
        assert_eq!(
            report.decide(LossDecision::ProceedAndDiscardUnsupported),
            ImportAuthorization::LossAccepted {
                discarded_entries: 1
            }
        );
    }

    #[test]
    fn unknown_payload_is_never_retained_for_round_tripping() {
        let marker = "do-not-retain-this-unknown-nbt-payload";
        let level = level_dat::LevelDat::from_data(Nbt::Compound(vec![
            ("DataVersion".to_string(), Nbt::Int(level_dat::DATA_VERSION_26_2)),
            ("GameType".to_string(), Nbt::Int(0)),
            (
                "spawn".to_string(),
                Nbt::Compound(vec![
                    ("pos".to_string(), Nbt::IntArray(vec![0, 64, 0])),
                    ("dimension".to_string(), Nbt::String("minecraft:overworld".to_string())),
                ]),
            ),
            ("UnknownPayload".to_string(), Nbt::String(marker.to_string())),
        ]));
        let mut builder = PreflightReport::builder();
        builder.inspect_level_dat(&level);
        let report = builder.finish();

        assert_eq!(
            report.unsupported(),
            &[UnsupportedData {
                location: SourceLocation::level("Data.UnknownPayload"),
                reason: LossReason::NoNativeDestination,
            }]
        );
        assert!(
            !format!("{report:?}").contains(marker),
            "the report must retain the field location, never unknown NBT content"
        );
    }

    #[test]
    fn malformed_chunks_block_and_valid_chunks_are_explicit_loss() {
        let mut builder = PreflightReport::builder();
        builder.inspect_chunk(
            "minecraft:overworld",
            4,
            -2,
            &Nbt::Compound(Vec::new()),
        );
        builder.inspect_chunk("minecraft:overworld", 5, -2, &Nbt::Int(4));
        let report = builder.finish();

        assert_eq!(report.unsupported().len(), 1);
        assert_eq!(report.blockers().len(), 1);
        assert_eq!(
            report.decide(LossDecision::ProceedAndDiscardUnsupported),
            ImportAuthorization::Blocked { blockers: 1 }
        );
    }
}
