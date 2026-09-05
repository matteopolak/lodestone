//! Versioned, generated types for Lodestone-native world storage records.
//!
//! This crate owns only the format vocabulary and its drift gate. It does not
//! choose a storage engine, perform file I/O, or connect to the server.

pub mod generated {
    include!("generated/lodestone.storage.v1.rs");
}

pub use generated::{
    BiomeSection, BuiltinBiome, BuiltinDimension, ChunkRecord, ChunkSection, EntityRecord,
    EntityRoster, ExtensionTable, ExtensionValue, GameMode, GeneralRecord, ItemEntityState,
    LightData, LightSection, LivingEntityState, PlayerInventory, PlayerInventorySlot, PlayerRecord,
    PlayerRuntimeState, RegisteredExtension, ScheduledTick, ScheduledTickKind,
    ScheduledTickPriority, StorageRecord, WorldProperties,
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
        // These fields were present in the initial vocabulary but had no
        // representation for Missing versus Uniform. Native v1 light data
        // uses the typed `light_sections` stream below; accepting both would
        // create two disagreeing sources of truth.
        if !section.sky_light.is_empty() || !section.block_light.is_empty() {
            return Err(ValidationError::LegacyLightBytes);
        }
    }
    if !chunk.light_sections.is_empty() {
        let mut previous_y = None;
        for section in &chunk.light_sections {
            if let Some(previous_y) = previous_y {
                if section.section_y <= previous_y {
                    return Err(ValidationError::UnorderedLightSections {
                        previous: previous_y,
                        actual: section.section_y,
                    });
                }
            }
            previous_y = Some(section.section_y);
            validate_light_data(section.sky_light.as_ref())?;
            validate_light_data(section.block_light.as_ref())?;
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
    for tick in chunk
        .block_scheduled_ticks
        .iter()
        .chain(chunk.fluid_scheduled_ticks.iter())
    {
        let kind = ScheduledTickKind::try_from(tick.kind)
            .map_err(|_| ValidationError::UnknownScheduledTickKind(tick.kind))?;
        if kind == ScheduledTickKind::Unspecified {
            return Err(ValidationError::UnknownScheduledTickKind(tick.kind));
        }
        ScheduledTickPriority::try_from(tick.priority)
            .map_err(|_| ValidationError::UnknownScheduledTickPriority(tick.priority))?;
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

fn validate_light_data(data: Option<&LightData>) -> Result<(), ValidationError> {
    let Some(data) = data else {
        return Ok(());
    };
    match &data.data {
        None => Ok(()),
        Some(generated::light_data::Data::Uniform(value)) => {
            if *value <= 15 {
                Ok(())
            } else {
                Err(ValidationError::InvalidLightUniform(*value))
            }
        }
        Some(generated::light_data::Data::Values(values)) => {
            if values.len() == 2048 {
                Ok(())
            } else {
                Err(ValidationError::InvalidLightArrayLength(values.len()))
            }
        }
    }
}

fn validate_general(general: &GeneralRecord) -> Result<(), ValidationError> {
    match &general.record {
        Some(generated::general_record::Record::Player(player)) => validate_player(player)?,
        Some(generated::general_record::Record::Entity(entity)) => validate_entity(entity)?,
        Some(generated::general_record::Record::EntityRoster(roster)) => {
            validate_entity_roster(roster)?;
        }
        Some(_) => {}
        None => return Err(ValidationError::MissingGeneralRecord),
    }
    validate_extension_values(&general.extensions)
}

fn validate_entity(entity: &EntityRecord) -> Result<(), ValidationError> {
    if entity.entity_uuid.len() != 16 {
        return Err(ValidationError::InvalidEntityUuidLength(
            entity.entity_uuid.len(),
        ));
    }
    if entity.entity_type.is_empty() {
        return Err(ValidationError::MissingEntityType);
    }
    if BuiltinDimension::try_from(entity.dimension).is_err()
        || entity.dimension == BuiltinDimension::Unspecified as i32
    {
        return Err(ValidationError::UnknownBuiltinDimension(entity.dimension));
    }
    if !entity.x.is_finite() || !entity.y.is_finite() || !entity.z.is_finite() {
        return Err(ValidationError::NonFiniteEntityPosition);
    }
    if !entity.yaw.is_finite() || !entity.pitch.is_finite() {
        return Err(ValidationError::NonFiniteEntityRotation);
    }
    if !entity.motion_x.is_finite()
        || !entity.motion_y.is_finite()
        || !entity.motion_z.is_finite()
    {
        return Err(ValidationError::NonFiniteEntityMotion);
    }
    match &entity.durable_state {
        Some(generated::entity_record::DurableState::Living(living)) => {
            if !living.health.is_finite() || living.health <= 0.0 {
                return Err(ValidationError::InvalidLivingEntityHealth);
            }
        }
        Some(generated::entity_record::DurableState::Item(item)) => {
            if item.item_key.is_empty()
                || item.count == 0
                || item.count > u32::from(u8::MAX)
                || i16::try_from(item.age).is_err()
                || i16::try_from(item.pickup_delay).is_err()
            {
                return Err(ValidationError::InvalidItemEntityState);
            }
        }
        None => {}
    }
    Ok(())
}

fn validate_entity_roster(roster: &EntityRoster) -> Result<(), ValidationError> {
    if BuiltinDimension::try_from(roster.dimension).is_err()
        || roster.dimension == BuiltinDimension::Unspecified as i32
    {
        return Err(ValidationError::UnknownBuiltinDimension(roster.dimension));
    }
    let mut seen = std::collections::HashSet::new();
    for uuid in &roster.entity_uuids {
        if uuid.len() != 16 {
            return Err(ValidationError::InvalidEntityUuidLength(uuid.len()));
        }
        if !seen.insert(uuid) {
            return Err(ValidationError::DuplicateEntityRosterUuid);
        }
    }
    Ok(())
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
    if GameMode::try_from(player.game_mode).is_err() {
        return Err(ValidationError::UnknownPlayerGameMode(player.game_mode));
    }
    if let Some(runtime) = &player.runtime_state {
        if !runtime.health.is_finite() || !(0.0..=20.0).contains(&runtime.health) {
            return Err(ValidationError::InvalidPlayerHealth);
        }
        if !(-20..=300).contains(&runtime.air_supply) {
            return Err(ValidationError::InvalidPlayerAirSupply(runtime.air_supply));
        }
        if runtime.experience_level < 0
            || runtime.experience_total < 0
            || !runtime.experience_progress.is_finite()
            || !(0.0..1.0).contains(&runtime.experience_progress)
        {
            return Err(ValidationError::InvalidPlayerExperience);
        }
    }
    if let Some(inventory) = &player.inventory {
        if inventory.selected_hotbar_slot > 8 {
            return Err(ValidationError::InvalidSelectedHotbarSlot(
                inventory.selected_hotbar_slot,
            ));
        }
        let mut seen = [false; 41];
        for item in &inventory.occupied_slots {
            let Ok(slot) = usize::try_from(item.slot) else {
                return Err(ValidationError::InvalidPlayerInventorySlot(item.slot));
            };
            if slot >= seen.len() {
                return Err(ValidationError::InvalidPlayerInventorySlot(item.slot));
            }
            if seen[slot] {
                return Err(ValidationError::DuplicatePlayerInventorySlot(item.slot));
            }
            seen[slot] = true;
            if item.item_key.is_empty() || item.count == 0 {
                return Err(ValidationError::InvalidPlayerInventoryItem(item.slot));
            }
        }
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
    LegacyLightBytes,
    UnorderedLightSections { previous: i32, actual: i32 },
    InvalidLightUniform(u32),
    InvalidLightArrayLength(usize),
    BiomeSectionCount { expected: usize, actual: usize },
    BiomeSectionCoordinateMismatch { expected: i32, actual: i32 },
    InvalidBiomeQuartRows(u32),
    InvalidBiomeCellCount { expected: usize, actual: usize },
    InvalidSurfaceBiomeCount(usize),
    UnknownBuiltinBiome(i32),
    InvalidMotionBlockingHeightCount(usize),
    MotionBlockingHeightOutOfRange(u32),
    UnknownScheduledTickKind(i32),
    UnknownScheduledTickPriority(i32),
    InvalidPlayerUuidLength(usize),
    InvalidEntityUuidLength(usize),
    MissingEntityType,
    NonFiniteEntityPosition,
    NonFiniteEntityRotation,
    NonFiniteEntityMotion,
    InvalidLivingEntityHealth,
    InvalidItemEntityState,
    DuplicateEntityRosterUuid,
    UnknownBuiltinDimension(i32),
    UnknownPlayerGameMode(i32),
    InvalidPlayerHealth,
    InvalidPlayerAirSupply(i32),
    InvalidPlayerExperience,
    InvalidSelectedHotbarSlot(u32),
    InvalidPlayerInventorySlot(u32),
    DuplicatePlayerInventorySlot(u32),
    InvalidPlayerInventoryItem(u32),
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
            Self::LegacyLightBytes => formatter.write_str(
                "chunk section uses legacy raw light bytes; use the typed light-section stream",
            ),
            Self::UnorderedLightSections { previous, actual } => write!(
                formatter,
                "light sections must be strictly ordered: {actual} follows {previous}",
            ),
            Self::InvalidLightUniform(value) => {
                write!(formatter, "light uniform value {value} exceeds the four-bit range")
            }
            Self::InvalidLightArrayLength(actual) => {
                write!(formatter, "expected 2048 light bytes, found {actual}")
            }
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
            Self::UnknownScheduledTickKind(kind) => {
                write!(formatter, "unknown scheduled-tick kind {kind}")
            }
            Self::UnknownScheduledTickPriority(priority) => {
                write!(formatter, "unknown scheduled-tick priority {priority}")
            }
            Self::InvalidPlayerUuidLength(actual) => {
                write!(formatter, "expected a 16-byte player UUID, found {actual} bytes")
            }
            Self::InvalidEntityUuidLength(actual) => {
                write!(formatter, "expected a 16-byte entity UUID, found {actual} bytes")
            }
            Self::MissingEntityType => formatter.write_str("entity has no type key"),
            Self::NonFiniteEntityPosition => {
                formatter.write_str("entity position contains a non-finite coordinate")
            }
            Self::NonFiniteEntityRotation => {
                formatter.write_str("entity rotation contains a non-finite angle")
            }
            Self::NonFiniteEntityMotion => {
                formatter.write_str("entity motion contains a non-finite coordinate")
            }
            Self::InvalidLivingEntityHealth => formatter.write_str("invalid living-entity health"),
            Self::InvalidItemEntityState => formatter.write_str("invalid item-entity state"),
            Self::DuplicateEntityRosterUuid => {
                formatter.write_str("entity roster repeats a UUID")
            }
            Self::UnknownBuiltinDimension(id) => {
                write!(formatter, "unknown built-in dimension {id}")
            }
            Self::UnknownPlayerGameMode(mode) => {
                write!(formatter, "unknown player game mode {mode}")
            }
            Self::InvalidPlayerHealth => formatter.write_str("invalid player health"),
            Self::InvalidPlayerAirSupply(air) => {
                write!(formatter, "invalid player air supply {air}")
            }
            Self::InvalidPlayerExperience => formatter.write_str("invalid player experience"),
            Self::InvalidSelectedHotbarSlot(slot) => {
                write!(formatter, "invalid selected hotbar slot {slot}")
            }
            Self::InvalidPlayerInventorySlot(slot) => {
                write!(formatter, "invalid player inventory slot {slot}")
            }
            Self::DuplicatePlayerInventorySlot(slot) => {
                write!(formatter, "duplicate player inventory slot {slot}")
            }
            Self::InvalidPlayerInventoryItem(slot) => {
                write!(formatter, "invalid player inventory item in slot {slot}")
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
