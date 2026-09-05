//! Versioned, generated types for Lodestone-native world storage records.
//!
//! This crate owns only the format vocabulary and its drift gate. It does not
//! choose a storage engine, perform file I/O, or connect to the server.

pub mod generated {
    include!("generated/lodestone.storage.v1.rs");
}

pub use generated::{
    BuiltinDimension, ChunkRecord, ChunkSection, EntityRecord, ExtensionTable, ExtensionValue,
    GameMode, GeneralRecord, PlayerRecord, RegisteredExtension, StorageRecord, WorldProperties,
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
    validate_extension_values(&chunk.extensions)
}

fn validate_general(general: &GeneralRecord) -> Result<(), ValidationError> {
    if general.record.is_none() {
        return Err(ValidationError::MissingGeneralRecord);
    }
    validate_extension_values(&general.extensions)
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
