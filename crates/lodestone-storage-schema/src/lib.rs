//! Versioned, generated types for Lodestone-native world storage records.
//!
//! This crate owns only the format vocabulary and its drift gate. It does not
//! choose a storage engine, perform file I/O, or connect to the server.

pub mod generated {
    include!("generated/lodestone.storage.v1.rs");
}

pub use generated::{
    BiomeSection, BuiltinBiome, BuiltinDimension, ChunkRecord, ChunkSection, EntityRecord,
    ExtensionTable, ExtensionValue, GameMode, GeneralRecord, PlayerRecord, RegisteredExtension,
    StorageRecord, WorldProperties,
};

/// The only storage-record format understood by the initial schema.
pub const FORMAT_VERSION_V1: u32 = 1;

/// Rejects a record whose required representation invariants are not expressible
/// in protobuf itself.
pub fn validate_record(record: &StorageRecord) -> Result<(), ValidationError> {
    if record.format_version != FORMAT_VERSION_V1 {
        return Err(ValidationError::UnsupportedFormatVersion(record.format_version));
    }

    match &record.record {
        Some(generated::storage_record::Record::Chunk(chunk)) => validate_chunk(chunk),
        Some(generated::storage_record::Record::General(general)) => validate_general(general),
        None => Err(ValidationError::MissingRecord),
    }
}

/// Validates that registered extension IDs are usable compact references.
pub fn validate_extension_table(table: &ExtensionTable) -> Result<(), ValidationError> {
    if table.table_version != FORMAT_VERSION_V1 {
        return Err(ValidationError::UnsupportedExtensionTableVersion(
            table.table_version,
        ));
    }

    let mut ids = std::collections::BTreeSet::new();
    for extension in &table.extensions {
        if extension.local_id == 0 {
            return Err(ValidationError::ZeroExtensionId);
        }
        if extension.namespace.is_empty() || extension.name.is_empty() {
            return Err(ValidationError::UnnamedExtension(extension.local_id));
        }
        if extension.schema_version == 0 {
            return Err(ValidationError::ZeroExtensionSchemaVersion(extension.local_id));
        }
        if !ids.insert(extension.local_id) {
            return Err(ValidationError::DuplicateExtensionId(extension.local_id));
        }
    }
    Ok(())
}

/// Validates a record plus the table that resolves its local extension IDs.
pub fn validate_record_with_extensions(
    record: &StorageRecord,
    table: &ExtensionTable,
) -> Result<(), ValidationError> {
    validate_record(record)?;
    validate_extension_table(table)?;
    let registered: std::collections::BTreeSet<_> =
        table.extensions.iter().map(|extension| extension.local_id).collect();
    let values = match &record.record {
        Some(generated::storage_record::Record::Chunk(chunk)) => &chunk.extensions,
        Some(generated::storage_record::Record::General(general)) => &general.extensions,
        None => return Err(ValidationError::MissingRecord),
    };
    for value in values {
        if value.local_id == 0 {
            return Err(ValidationError::ZeroExtensionId);
        }
        if !registered.contains(&value.local_id) {
            return Err(ValidationError::UnregisteredExtensionId(value.local_id));
        }
    }
    Ok(())
}

fn validate_chunk(chunk: &ChunkRecord) -> Result<(), ValidationError> {
    if chunk.game_data_version == 0 {
        return Err(ValidationError::MissingGameDataVersion);
    }
    for section in &chunk.sections {
        if !(1..=15).contains(&section.palette_bits) {
            return Err(ValidationError::InvalidPaletteBits(section.palette_bits));
        }
        if section.palette_state_ids.is_empty() {
            return Err(ValidationError::EmptyPalette);
        }
    }
    if !chunk.biome_sections.is_empty() || !chunk.surface_biome_ids.is_empty() {
        if chunk.biome_sections.len() != chunk.sections.len() {
            return Err(ValidationError::BiomeSectionCount {
                expected: chunk.sections.len(),
                actual: chunk.biome_sections.len(),
            });
        }
        if chunk.surface_biome_ids.len() != 16 {
            return Err(ValidationError::InvalidSurfaceBiomeCount(
                chunk.surface_biome_ids.len(),
            ));
        }
        for (block_section, biome_section) in chunk.sections.iter().zip(&chunk.biome_sections) {
            if biome_section.section_y != block_section.section_y {
                return Err(ValidationError::BiomeSectionCoordinateMismatch {
                    expected: block_section.section_y,
                    actual: biome_section.section_y,
                });
            }
            if !(1..=4).contains(&biome_section.quart_rows) {
                return Err(ValidationError::InvalidBiomeQuartRows(
                    biome_section.quart_rows,
                ));
            }
            let expected_cells = biome_section.quart_rows as usize * 16;
            if biome_section.biome_ids.len() != expected_cells {
                return Err(ValidationError::InvalidBiomeCellCount {
                    expected: expected_cells,
                    actual: biome_section.biome_ids.len(),
                });
            }
            validate_builtin_biomes(&biome_section.biome_ids)?;
        }
        validate_builtin_biomes(&chunk.surface_biome_ids)?;
    }
    if !chunk.motion_blocking_heights.is_empty() {
        if chunk.motion_blocking_heights.len() != 16 * 16 {
            return Err(ValidationError::InvalidMotionBlockingHeightCount(
                chunk.motion_blocking_heights.len(),
            ));
        }
        if let Some(&height) = chunk
            .motion_blocking_heights
            .iter()
            .find(|&&height| height > u32::from(u16::MAX))
        {
            return Err(ValidationError::MotionBlockingHeightOutOfRange(height));
        }
    }
    validate_extension_values(&chunk.extensions)
}

fn validate_builtin_biomes(ids: &[i32]) -> Result<(), ValidationError> {
    for &id in ids {
        if BuiltinBiome::try_from(id).is_err() || id == BuiltinBiome::Unspecified as i32 {
            return Err(ValidationError::UnknownBuiltinBiome(id));
        }
    }
    Ok(())
}

fn validate_general(general: &GeneralRecord) -> Result<(), ValidationError> {
    match &general.record {
        Some(generated::general_record::Record::Player(player)) => validate_player(player)?,
        Some(_) => {}
        None => return Err(ValidationError::MissingGeneralRecord),
    }
    validate_extension_values(&general.extensions)
}

fn validate_player(player: &PlayerRecord) -> Result<(), ValidationError> {
    if player.player_uuid.len() != 16 {
        return Err(ValidationError::InvalidPlayerUuidLength(
            player.player_uuid.len(),
        ));
    }
    if BuiltinDimension::try_from(player.dimension).is_err()
        || player.dimension == BuiltinDimension::Unspecified as i32
    {
        return Err(ValidationError::UnknownBuiltinDimension(player.dimension));
    }
    Ok(())
}

fn validate_extension_values(values: &[ExtensionValue]) -> Result<(), ValidationError> {
    if values.iter().any(|value| value.local_id == 0) {
        Err(ValidationError::ZeroExtensionId)
    } else {
        Ok(())
    }
}

/// A structural schema validation error, before any storage-engine policy runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    UnsupportedFormatVersion(u32),
    UnsupportedExtensionTableVersion(u32),
    MissingRecord,
    MissingGeneralRecord,
    MissingGameDataVersion,
    InvalidPaletteBits(u32),
    EmptyPalette,
    BiomeSectionCount { expected: usize, actual: usize },
    BiomeSectionCoordinateMismatch { expected: i32, actual: i32 },
    InvalidBiomeQuartRows(u32),
    InvalidBiomeCellCount { expected: usize, actual: usize },
    InvalidSurfaceBiomeCount(usize),
    UnknownBuiltinBiome(i32),
    InvalidMotionBlockingHeightCount(usize),
    MotionBlockingHeightOutOfRange(u32),
    InvalidPlayerUuidLength(usize),
    UnknownBuiltinDimension(i32),
    ZeroExtensionId,
    DuplicateExtensionId(u32),
    UnregisteredExtensionId(u32),
    UnnamedExtension(u32),
    ZeroExtensionSchemaVersion(u32),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedFormatVersion(version) => {
                write!(formatter, "unsupported storage record format version {version}")
            }
            Self::UnsupportedExtensionTableVersion(version) => {
                write!(formatter, "unsupported extension table version {version}")
            }
            Self::MissingRecord => formatter.write_str("storage record has no body"),
            Self::MissingGeneralRecord => formatter.write_str("general record has no body"),
            Self::MissingGameDataVersion => formatter.write_str("chunk has no game data version"),
            Self::InvalidPaletteBits(bits) => write!(formatter, "invalid palette width {bits}"),
            Self::EmptyPalette => formatter.write_str("chunk section has an empty palette"),
            Self::BiomeSectionCount { expected, actual } => {
                write!(formatter, "expected {expected} biome sections, found {actual}")
            }
            Self::BiomeSectionCoordinateMismatch { expected, actual } => {
                write!(formatter, "expected biome section Y {expected}, found {actual}")
            }
            Self::InvalidBiomeQuartRows(rows) => {
                write!(formatter, "biome section has invalid quart-row count {rows}")
            }
            Self::InvalidBiomeCellCount { expected, actual } => {
                write!(formatter, "expected {expected} biome cells, found {actual}")
            }
            Self::InvalidSurfaceBiomeCount(actual) => {
                write!(formatter, "expected 16 surface biomes, found {actual}")
            }
            Self::UnknownBuiltinBiome(id) => write!(formatter, "unknown built-in biome {id}"),
            Self::InvalidMotionBlockingHeightCount(actual) => {
                write!(formatter, "expected 256 motion-blocking heights, found {actual}")
            }
            Self::MotionBlockingHeightOutOfRange(height) => {
                write!(formatter, "motion-blocking height {height} exceeds u16")
            }
            Self::InvalidPlayerUuidLength(actual) => {
                write!(formatter, "expected a 16-byte player UUID, found {actual} bytes")
            }
            Self::UnknownBuiltinDimension(id) => {
                write!(formatter, "unknown built-in dimension {id}")
            }
            Self::ZeroExtensionId => formatter.write_str("extension local ID zero is reserved"),
            Self::DuplicateExtensionId(id) => write!(formatter, "duplicate extension local ID {id}"),
            Self::UnregisteredExtensionId(id) => {
                write!(formatter, "extension local ID {id} is not registered")
            }
            Self::UnnamedExtension(id) => write!(formatter, "extension {id} has no schema name"),
            Self::ZeroExtensionSchemaVersion(id) => {
                write!(formatter, "extension {id} has schema version zero")
            }
        }
    }
}

impl std::error::Error for ValidationError {}
