//! Authorization-gated import of one Anvil player file into typed native player data.
//!
//! The native player schema is deliberately a locator, not a replacement for
//! a complete player save. This module converts the identity, built-in
//! dimension, feet position, rotation, and game mode from one selected Anvil player root;
//! it inventories every other player value before writing the native record.
//! The Anvil file remains the authoritative complete player state.

use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
};

use lodestone_storage_schema::BuiltinDimension;

use crate::{
    player_data::PlayerData,
    world_storage::{NativePlayerData, NativePlayerRecord, WorldStorage},
};

/// The fixed-point producer contract used by this importer.
///
/// [`NativePlayerRecord`] deliberately leaves its coordinate unit to the
/// producer. A record written by this module has one thousand units per block,
/// so only a consumer that has selected the same contract may interpret it.
pub const POSITION_UNITS_PER_BLOCK: f64 = 1_000.0;

/// The explicitly selected player files a filesystem import may inspect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlayerFileSelection {
    /// Discover every canonical UUID `.dat` file in `players/data`.
    All,
    /// Discover exactly these canonical UUID `.dat` files in `players/data`.
    Uuids(Vec<uuid::Uuid>),
}

/// One deterministic player-file discovery result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedPlayerFile {
    uuid: uuid::Uuid,
    path: PathBuf,
}

impl SelectedPlayerFile {
    /// UUID supplied by the canonical filename.
    #[must_use]
    pub const fn uuid(&self) -> uuid::Uuid {
        self.uuid
    }

    /// Source file selected beneath the supplied Anvil world directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Discovers a deterministic, non-empty player-file selection beneath one
/// Anvil world directory.
///
/// `All` accepts only canonical UUID `.dat` names and sorts by UUID, while
/// `Uuids` sorts the explicit selection before deriving each canonical path.
/// A malformed `.dat` name is an error rather than an ignored save, and a
/// requested missing file is later refused during preflight.
pub fn discover_player_files(
    world_directory: &Path,
    selection: PlayerFileSelection,
) -> Result<Vec<SelectedPlayerFile>, Error> {
    let mut uuids = BTreeSet::new();
    match selection {
        PlayerFileSelection::All => {
            let directory = lodestone_anvil::player_dat::dir_in(world_directory);
            for entry in std::fs::read_dir(&directory).map_err(Error::PlayerDirectory)? {
                let entry = entry.map_err(Error::PlayerDirectory)?;
                if !entry.file_type().map_err(Error::PlayerDirectory)?.is_file() {
                    continue;
                }
                let path = entry.path();
                if path.extension().is_none_or(|extension| extension != "dat") {
                    continue;
                }
                let stem = path.file_stem().and_then(|stem| stem.to_str()).ok_or_else(|| {
                    Error::InvalidPlayerFilename {
                        path: path.clone(),
                    }
                })?;
                let uuid = uuid::Uuid::parse_str(stem).map_err(|_| Error::InvalidPlayerFilename {
                    path: path.clone(),
                })?;
                if uuid.to_string() != stem {
                    return Err(Error::InvalidPlayerFilename { path });
                }
                uuids.insert(uuid);
            }
        }
        PlayerFileSelection::Uuids(selected) => {
            for uuid in selected {
                if !uuids.insert(uuid) {
                    return Err(Error::DuplicatePlayerSelection(uuid));
                }
            }
        }
    }
    if uuids.is_empty() {
        return Err(Error::NoSelectedPlayers);
    }
    Ok(uuids
        .into_iter()
        .map(|uuid| SelectedPlayerFile {
            path: lodestone_anvil::player_dat::path_in(world_directory, &uuid.to_string()),
            uuid,
        })
        .collect())
}

/// A player value that the current typed native player record cannot retain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedPlayerData {
    /// The source game-data version validates the Anvil schema but has no
    /// native locator field.
    DataVersion,
    /// Velocity has no native locator field.
    Motion,
    /// Fire ticks have no native player field.
    FireTicks,
    /// Fall distance has no native locator field.
    FallDistance,
    /// Ground contact has no native locator field.
    GroundState,
    /// The Anvil decoder cannot prove that no unmodeled item component existed.
    UnverifiedInventoryComponents,
    /// Root fields the player schema preserves but does not model have no
    /// native locator field.
    PreservedRootFields,
    /// A position was rounded to the milliblock producer contract.
    PositionPrecision {
        /// Coordinate that required rounding.
        axis: PositionAxis,
    },
    /// A rotation was rounded to native millidegrees.
    RotationPrecision {
        /// Rotation component that required rounding.
        axis: RotationAxis,
    },
}

/// One coordinate component in a player position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionAxis {
    /// East/west position.
    X,
    /// Vertical position.
    Y,
    /// North/south position.
    Z,
}

/// One component in a player rotation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RotationAxis {
    /// Horizontal rotation.
    Yaw,
    /// Vertical rotation.
    Pitch,
}

/// A source value that cannot become a safe native locator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlayerImportBlocker {
    /// The player was saved in a dimension outside the native built-in census.
    UnsupportedDimension(String),
    /// A floating source value is NaN or infinite.
    NonFinitePosition {
        /// Coordinate containing the invalid value.
        axis: PositionAxis,
    },
    /// A scaled source coordinate does not fit the native signed i32 field.
    PositionOutOfRange {
        /// Coordinate that exceeds the native field range.
        axis: PositionAxis,
    },
    /// A source yaw or pitch is NaN or infinite.
    NonFiniteRotation {
        /// Rotation component containing the invalid value.
        axis: RotationAxis,
    },
    /// A millidegree source rotation does not fit the native signed i32 field.
    RotationOutOfRange {
        /// Rotation component that exceeds the native field range.
        axis: RotationAxis,
    },
}

/// Payload-free inventory for one player-to-locator conversion.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlayerImportReport {
    unsupported: Vec<UnsupportedPlayerData>,
    blockers: Vec<PlayerImportBlocker>,
}

/// Payload-free preflight result for one discovered player file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerFileImportReport {
    /// Identity taken from the selected canonical filename.
    pub uuid: uuid::Uuid,
    /// Loss and safety report for this one complete player root.
    pub report: PlayerImportReport,
}

/// Payload-free aggregate preflight for one selected filesystem batch.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlayerBatchImportReport {
    players: Vec<PlayerFileImportReport>,
}

impl PlayerBatchImportReport {
    /// Reports in deterministic UUID order.
    #[must_use]
    pub fn players(&self) -> &[PlayerFileImportReport] {
        &self.players
    }

    /// Number of loss categories across the entire selected batch.
    #[must_use]
    pub fn unsupported_count(&self) -> usize {
        self.players
            .iter()
            .map(|player| player.report.unsupported.len())
            .sum()
    }

    /// Number of unsafe source values across the entire selected batch.
    #[must_use]
    pub fn blocker_count(&self) -> usize {
        self.players
            .iter()
            .map(|player| player.report.blockers.len())
            .sum()
    }

    /// Applies a single decision after every selected player has been reviewed.
    #[must_use]
    pub fn decide(&self, decision: PlayerLossDecision) -> PlayerBatchImportAuthorization {
        if self.blocker_count() != 0 {
            return PlayerBatchImportAuthorization::Blocked {
                blockers: self.blocker_count(),
            };
        }
        match decision {
            PlayerLossDecision::Abort => PlayerBatchImportAuthorization::Aborted,
            PlayerLossDecision::ProceedAndDiscardUnsupported if self.unsupported_count() == 0 => {
                PlayerBatchImportAuthorization::Lossless
            }
            PlayerLossDecision::ProceedAndDiscardUnsupported => {
                PlayerBatchImportAuthorization::LossAccepted {
                    discarded_entries: self.unsupported_count(),
                }
            }
        }
    }
}

/// A prepared all-or-nothing filesystem player import.
///
/// The decoded player values stay private so callers cannot accidentally retain
/// the full unsupported NBT payload after the typed locator transaction.
pub struct PlayerBatchImportPlan {
    report: PlayerBatchImportReport,
    players: Vec<NativePlayerData>,
}

impl fmt::Debug for PlayerBatchImportPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlayerBatchImportPlan")
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

impl PlayerBatchImportPlan {
    /// The payload-free report to render for operator review.
    #[must_use]
    pub fn report(&self) -> &PlayerBatchImportReport {
        &self.report
    }
}

/// Batch authorization produced only after all selected player files were
/// preflighted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "pass this to import_player_batch; selected player loss must not be implicit"]
pub enum PlayerBatchImportAuthorization {
    /// The operator declined the reviewed batch.
    Aborted,
    /// The entire batch is lossless.
    Lossless,
    /// The operator accepted every reported dropped category in the batch.
    LossAccepted {
        /// Aggregate number of dropped report entries.
        discarded_entries: usize,
    },
    /// At least one selected player has unsafe data.
    Blocked {
        /// Aggregate number of blocking report entries.
        blockers: usize,
    },
}

impl PlayerBatchImportAuthorization {
    fn permits_conversion(self) -> bool {
        matches!(self, Self::Lossless | Self::LossAccepted { .. })
    }
}

/// The aggregate report and committed locator count from a player batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerBatchImportResult {
    /// The exact payload-free batch report that authorized the write.
    pub report: PlayerBatchImportReport,
    /// Number of locators committed in the one native transaction.
    pub records_written: usize,
}

impl PlayerImportReport {
    /// Values a caller must explicitly accept before conversion.
    #[must_use]
    pub fn unsupported(&self) -> &[UnsupportedPlayerData] {
        &self.unsupported
    }

    /// Values that must be repaired or supported before conversion.
    #[must_use]
    pub fn blockers(&self) -> &[PlayerImportBlocker] {
        &self.blockers
    }

    /// Applies the required explicit import decision.
    #[must_use]
    pub fn decide(&self, decision: PlayerLossDecision) -> PlayerImportAuthorization {
        if !self.blockers.is_empty() {
            return PlayerImportAuthorization::Blocked {
                blockers: self.blockers.len(),
            };
        }
        match decision {
            PlayerLossDecision::Abort => PlayerImportAuthorization::Aborted,
            PlayerLossDecision::ProceedAndDiscardUnsupported if self.unsupported.is_empty() => {
                PlayerImportAuthorization::Lossless
            }
            PlayerLossDecision::ProceedAndDiscardUnsupported => {
                PlayerImportAuthorization::LossAccepted {
                    discarded_entries: self.unsupported.len(),
                }
            }
        }
    }
}

/// A caller's decision after reviewing [`PlayerImportReport`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerLossDecision {
    /// Do not convert this player.
    Abort,
    /// Write the locator while discarding every reported source value.
    ProceedAndDiscardUnsupported,
}

/// An authorization tied to one current player conversion report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "pass this to import_player or import_player_file; player data must not be discarded implicitly"]
pub enum PlayerImportAuthorization {
    /// The caller declined the conversion.
    Aborted,
    /// The source has no values outside the locator schema.
    Lossless,
    /// The caller accepted this many discarded source values.
    LossAccepted {
        /// Number of report entries acknowledged by the caller.
        discarded_entries: usize,
    },
    /// One or more source values cannot be represented safely.
    Blocked {
        /// Number of blocking report entries.
        blockers: usize,
    },
}

impl PlayerImportAuthorization {
    fn permits_conversion(self) -> bool {
        matches!(self, Self::Lossless | Self::LossAccepted { .. })
    }
}

/// The report and one native record written by a completed conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerImportResult {
    /// The fresh report whose matching authorization permitted this write.
    pub report: PlayerImportReport,
    /// Number of native records committed; a player import writes exactly one.
    pub records_written: usize,
}

/// An error that prevents an Anvil player root becoming a native locator.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The selected world has no player files to preflight.
    #[error("player import selected no player data files")]
    NoSelectedPlayers,
    /// A selected source directory could not be enumerated.
    #[error("could not enumerate Anvil player-data directory: {0}")]
    PlayerDirectory(#[source] std::io::Error),
    /// An apparent player file does not have the canonical UUID filename.
    #[error("Anvil player-data filename is not a canonical UUID .dat file: {}", path.display())]
    InvalidPlayerFilename {
        /// Invalid filesystem path.
        path: PathBuf,
    },
    /// An explicit selection named one UUID more than once.
    #[error("Anvil player import selected UUID {0} more than once")]
    DuplicatePlayerSelection(uuid::Uuid),
    /// A selected player file disappeared before it could be decoded.
    #[error("selected Anvil player-data file is absent: {}", path.display())]
    MissingSelectedPlayerFile {
        /// Missing selected filesystem path.
        path: PathBuf,
    },
    /// Native-only conversion needs an explicit review of every discarded
    /// player value.
    #[error("Anvil player import requires an explicit PlayerImportAuthorization")]
    MissingAuthorization,
    /// The caller did not authorize conversion.
    #[error("Anvil player import authorization does not permit conversion: {authorization:?}")]
    AuthorizationDenied {
        /// Authorization supplied by the caller.
        authorization: PlayerImportAuthorization,
    },
    /// The source changed after preflight, or the authorization was derived
    /// from a different source/report.
    #[error(
        "Anvil player import authorization does not match this player: supplied {supplied:?}, required {required:?}"
    )]
    AuthorizationMismatch {
        /// Authorization supplied by the caller.
        supplied: PlayerImportAuthorization,
        /// Authorization the current source requires.
        required: PlayerImportAuthorization,
    },
    /// The batch authorization was not supplied.
    #[error("Anvil player batch import requires an explicit PlayerBatchImportAuthorization")]
    MissingBatchAuthorization,
    /// The caller did not authorize the selected batch.
    #[error("Anvil player batch import authorization does not permit conversion: {authorization:?}")]
    BatchAuthorizationDenied {
        /// Authorization supplied by the caller.
        authorization: PlayerBatchImportAuthorization,
    },
    /// The authorization does not match the complete preflighted batch.
    #[error(
        "Anvil player batch import authorization does not match the selected players: supplied {supplied:?}, required {required:?}"
    )]
    BatchAuthorizationMismatch {
        /// Authorization supplied by the caller.
        supplied: PlayerBatchImportAuthorization,
        /// Authorization required by the current batch report.
        required: PlayerBatchImportAuthorization,
    },
    /// The selected Anvil player file could not be decoded.
    #[error("Anvil player-data read error: {0}")]
    PlayerDat(#[source] lodestone_anvil::Error),
    /// The selected native backend refused or failed the locator write.
    #[error("native player locator storage failed: {0}")]
    Storage(#[source] crate::world_storage::Error),
}

/// Inventories every source field that one typed native player record cannot retain.
///
/// The report is payload-free: it records field categories and precision loss,
/// never the player inventory or preserved root values themselves.
#[must_use]
pub fn preflight_player(player: &PlayerData) -> PlayerImportReport {
    let mut report = PlayerImportReport {
        unsupported: vec![
            UnsupportedPlayerData::DataVersion,
            UnsupportedPlayerData::Motion,
            UnsupportedPlayerData::FireTicks,
            UnsupportedPlayerData::FallDistance,
            UnsupportedPlayerData::GroundState,
            UnsupportedPlayerData::UnverifiedInventoryComponents,
        ],
        blockers: Vec::new(),
    };
    if !player.preserved.is_empty() {
        report
            .unsupported
            .push(UnsupportedPlayerData::PreservedRootFields);
    }

    match builtin_dimension(&player.dimension) {
        Some(_) => {}
        None => report
            .blockers
            .push(PlayerImportBlocker::UnsupportedDimension(player.dimension.clone())),
    }
    inspect_position(&mut report, player.pos.x, PositionAxis::X);
    inspect_position(&mut report, player.pos.y, PositionAxis::Y);
    inspect_position(&mut report, player.pos.z, PositionAxis::Z);
    inspect_rotation(&mut report, player.rotation.yaw, RotationAxis::Yaw);
    inspect_rotation(&mut report, player.rotation.pitch, RotationAxis::Pitch);
    report
}

/// Reads, preflights, and imports one selected gzip-wrapped Anvil player file.
///
/// A missing file is returned as `Ok(None)`, matching the Anvil codec's
/// first-join meaning. The file is decoded once for preflight and conversion;
/// callers can call [`preflight_player_file`] first to obtain the authorization
/// and this function reruns the report before it writes.
pub fn import_player_file(
    storage: &WorldStorage,
    uuid: uuid::Uuid,
    path: &Path,
    authorization: Option<PlayerImportAuthorization>,
) -> Result<Option<PlayerImportResult>, Error> {
    let Some(root) = lodestone_anvil::player_dat::read_from_file(path).map_err(Error::PlayerDat)?
    else {
        return Ok(None);
    };
    let player = PlayerData::from_nbt(&root).map_err(Error::PlayerDat)?;
    import_player(storage, uuid, &player, authorization).map(Some)
}

/// Reads and preflights one selected gzip-wrapped Anvil player file.
///
/// The returned report stores no player payload. `Ok(None)` means the file is
/// absent and therefore has no player to convert.
pub fn preflight_player_file(path: &Path) -> Result<Option<PlayerImportReport>, Error> {
    let Some(root) = lodestone_anvil::player_dat::read_from_file(path).map_err(Error::PlayerDat)?
    else {
        return Ok(None);
    };
    let player = PlayerData::from_nbt(&root).map_err(Error::PlayerDat)?;
    Ok(Some(preflight_player(&player)))
}

/// Reads and preflights every discovered player before any native write opens.
///
/// The returned plan retains only the typed locator records required for its
/// later one-transaction commit. Its public report contains no inventory,
/// preserved NBT, or other unsupported source payload.
pub fn preflight_player_batch(
    selected: &[SelectedPlayerFile],
) -> Result<PlayerBatchImportPlan, Error> {
    if selected.is_empty() {
        return Err(Error::NoSelectedPlayers);
    }
    let mut reports = Vec::with_capacity(selected.len());
    let mut players = Vec::with_capacity(selected.len());
    for file in selected {
        let Some(root) = lodestone_anvil::player_dat::read_from_file(&file.path)
            .map_err(Error::PlayerDat)?
        else {
            return Err(Error::MissingSelectedPlayerFile {
                path: file.path.clone(),
            });
        };
        let player = PlayerData::from_nbt(&root).map_err(Error::PlayerDat)?;
        let report = preflight_player(&player);
        players.push(player_data_from_player_unchecked(file.uuid, &player));
        reports.push(PlayerFileImportReport {
            uuid: file.uuid,
            report,
        });
    }
    Ok(PlayerBatchImportPlan {
        report: PlayerBatchImportReport { players: reports },
        players,
    })
}

/// Commits every preflighted player locator in exactly one native transaction.
///
/// Any blocker, missing authorization, stale aggregate authorization, duplicate
/// UUID, or compact-key collision fails before the transaction is appended.
pub fn import_player_batch(
    storage: &WorldStorage,
    plan: PlayerBatchImportPlan,
    authorization: Option<PlayerBatchImportAuthorization>,
) -> Result<PlayerBatchImportResult, Error> {
    let Some(authorization) = authorization else {
        return Err(Error::MissingBatchAuthorization);
    };
    if !authorization.permits_conversion() {
        return Err(Error::BatchAuthorizationDenied { authorization });
    }
    let required = plan
        .report
        .decide(PlayerLossDecision::ProceedAndDiscardUnsupported);
    if authorization != required {
        return Err(Error::BatchAuthorizationMismatch {
            supplied: authorization,
            required,
        });
    }
    let records_written = storage
        .write_dirty_player_data_batch(plan.players)
        .map_err(Error::Storage)?;
    Ok(PlayerBatchImportResult {
        report: plan.report,
        records_written,
    })
}

/// Converts and commits one authorized player locator.
///
/// The UUID is supplied separately because Anvil player roots do not carry the
/// filename identity used by the native key. A successful result writes only
/// the seven fields in [`NativePlayerRecord`]; it never updates or replaces the
/// source player file.
pub fn import_player(
    storage: &WorldStorage,
    uuid: uuid::Uuid,
    player: &PlayerData,
    authorization: Option<PlayerImportAuthorization>,
) -> Result<PlayerImportResult, Error> {
    let Some(authorization) = authorization else {
        return Err(Error::MissingAuthorization);
    };
    if !authorization.permits_conversion() {
        return Err(Error::AuthorizationDenied { authorization });
    }

    let report = preflight_player(player);
    let required = report.decide(PlayerLossDecision::ProceedAndDiscardUnsupported);
    if authorization != required {
        return Err(Error::AuthorizationMismatch {
            supplied: authorization,
            required,
        });
    }

    storage
        .write_dirty_player_data(player_data_from_player(uuid, player))
        .map_err(Error::Storage)?;
    Ok(PlayerImportResult {
        report,
        records_written: 1,
    })
}

fn inspect_position(report: &mut PlayerImportReport, value: f64, axis: PositionAxis) {
    if !value.is_finite() {
        report
            .blockers
            .push(PlayerImportBlocker::NonFinitePosition { axis });
        return;
    }
    let scaled = value * POSITION_UNITS_PER_BLOCK;
    if !fits_i32(scaled) {
        report
            .blockers
            .push(PlayerImportBlocker::PositionOutOfRange { axis });
    } else if scaled.round() != scaled {
        report
            .unsupported
            .push(UnsupportedPlayerData::PositionPrecision { axis });
    }
}

fn inspect_rotation(report: &mut PlayerImportReport, value: f32, axis: RotationAxis) {
    if !value.is_finite() {
        report
            .blockers
            .push(PlayerImportBlocker::NonFiniteRotation { axis });
        return;
    }
    let scaled = f64::from(value) * 1_000.0;
    if !fits_i32(scaled) {
        report
            .blockers
            .push(PlayerImportBlocker::RotationOutOfRange { axis });
    } else if scaled.round() != scaled {
        report
            .unsupported
            .push(UnsupportedPlayerData::RotationPrecision { axis });
    }
}

fn fits_i32(value: f64) -> bool {
    value.round() >= f64::from(i32::MIN) && value.round() <= f64::from(i32::MAX)
}

fn player_data_from_player(uuid: uuid::Uuid, player: &PlayerData) -> NativePlayerData {
    NativePlayerData {
        locator: NativePlayerRecord {
            uuid: *uuid.as_bytes(),
            dimension: builtin_dimension(&player.dimension)
                .expect("preflight authorization rejects unsupported player dimensions"),
            x_fixed: round_to_i32(player.pos.x * POSITION_UNITS_PER_BLOCK),
            y_fixed: round_to_i32(player.pos.y * POSITION_UNITS_PER_BLOCK),
            z_fixed: round_to_i32(player.pos.z * POSITION_UNITS_PER_BLOCK),
            yaw_millidegrees: round_to_i32(f64::from(player.rotation.yaw) * 1_000.0),
            pitch_millidegrees: round_to_i32(f64::from(player.rotation.pitch) * 1_000.0),
        },
        game_mode: player.game_mode,
        runtime: Some(crate::world_storage::NativePlayerRuntimeState {
            health: player.health,
            air_supply: player.air_supply,
            experience: player.experience,
        }),
        inventory: Some(player.to_inventory()),
    }
}

fn player_data_from_player_unchecked(uuid: uuid::Uuid, player: &PlayerData) -> NativePlayerData {
    NativePlayerData {
        locator: NativePlayerRecord {
            uuid: *uuid.as_bytes(),
            dimension: builtin_dimension(&player.dimension)
                .unwrap_or(BuiltinDimension::Unspecified),
            x_fixed: round_to_i32(player.pos.x * POSITION_UNITS_PER_BLOCK),
            y_fixed: round_to_i32(player.pos.y * POSITION_UNITS_PER_BLOCK),
            z_fixed: round_to_i32(player.pos.z * POSITION_UNITS_PER_BLOCK),
            yaw_millidegrees: round_to_i32(f64::from(player.rotation.yaw) * 1_000.0),
            pitch_millidegrees: round_to_i32(f64::from(player.rotation.pitch) * 1_000.0),
        },
        game_mode: player.game_mode,
        runtime: Some(crate::world_storage::NativePlayerRuntimeState {
            health: player.health,
            air_supply: player.air_supply,
            experience: player.experience,
        }),
        inventory: Some(player.to_inventory()),
    }
}

fn round_to_i32(value: f64) -> i32 {
    value.round() as i32
}

fn builtin_dimension(dimension: &str) -> Option<BuiltinDimension> {
    match dimension {
        "minecraft:overworld" => Some(BuiltinDimension::Overworld),
        "minecraft:the_nether" => Some(BuiltinDimension::Nether),
        "minecraft:the_end" => Some(BuiltinDimension::End),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use lodestone_core::{Nbt, NbtTag};

    use super::*;
    use crate::world_storage::WorldStorageBackend;

    fn scratch(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lodestone-anvil-player-storage-{name}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create scratch directory");
        path
    }

    fn fixture_root() -> Nbt {
        Nbt::Compound(vec![
            (
                "DataVersion".to_owned(),
                Nbt::Int(lodestone_anvil::level_dat::DATA_VERSION_26_2),
            ),
            (
                "Pos".to_owned(),
                Nbt::List {
                    element_type: NbtTag::Double,
                    elements: vec![
                        Nbt::Double(-16.384),
                        Nbt::Double(64.125),
                        Nbt::Double(65.535),
                    ],
                },
            ),
            (
                "Motion".to_owned(),
                Nbt::List {
                    element_type: NbtTag::Double,
                    elements: vec![Nbt::Double(0.25), Nbt::Double(0.0), Nbt::Double(-0.125)],
                },
            ),
            (
                "Rotation".to_owned(),
                Nbt::List {
                    element_type: NbtTag::Float,
                    elements: vec![Nbt::Float(-90.001), Nbt::Float(45.002)],
                },
            ),
            ("Health".to_owned(), Nbt::Float(13.5)),
            ("Air".to_owned(), Nbt::Short(240)),
            ("Fire".to_owned(), Nbt::Short(12)),
            ("fall_distance".to_owned(), Nbt::Double(3.25)),
            ("OnGround".to_owned(), Nbt::Byte(0)),
            (
                "Dimension".to_owned(),
                Nbt::String("minecraft:the_nether".to_owned()),
            ),
            ("playerGameType".to_owned(), Nbt::Int(1)),
            ("SelectedItemSlot".to_owned(), Nbt::Int(4)),
            (
                "Inventory".to_owned(),
                Nbt::List {
                    element_type: NbtTag::Compound,
                    elements: Vec::new(),
                },
            ),
            ("XpLevel".to_owned(), Nbt::Int(7)),
            ("XpP".to_owned(), Nbt::Float(0.25)),
            ("XpTotal".to_owned(), Nbt::Int(341)),
            ("foodLevel".to_owned(), Nbt::Int(18)),
        ])
    }

    fn fixture_player() -> PlayerData {
        PlayerData::from_nbt(&fixture_root()).expect("independent fixture decodes")
    }

    #[test]
    fn independent_player_fixture_decodes_through_anvil_codec_and_maps_typed_fields() {
        let directory = scratch("fixture");
        let file = directory.join("fixture-player.dat");
        let root = fixture_root();
        lodestone_anvil::player_dat::write_to_file(&root, &file).expect("fixture gzip encodes");

        let report = preflight_player_file(&file)
            .expect("fixture gzip decodes through Anvil codec")
            .expect("fixture player exists");
        assert!(report.blockers().is_empty(), "fixture values are representable");
        assert!(
            report
                .unsupported()
                .contains(&UnsupportedPlayerData::PreservedRootFields),
            "unknown Anvil root fields must not disappear from preflight"
        );

        let native_dir = directory.join("native");
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: native_dir,
        })
        .expect("open native storage");
        let uuid = uuid::Uuid::from_bytes([0x42; 16]);
        let authorization = report.decide(PlayerLossDecision::ProceedAndDiscardUnsupported);
        let result = import_player_file(&storage, uuid, &file, Some(authorization))
            .expect("authorized file import")
            .expect("fixture player imports");
        assert_eq!(result.records_written, 1);
        assert_eq!(
            storage.load_player_data(*uuid.as_bytes()).expect("load typed player"),
            Some(NativePlayerData {
                locator: NativePlayerRecord {
                    uuid: *uuid.as_bytes(),
                    dimension: BuiltinDimension::Nether,
                    x_fixed: -16_384,
                    y_fixed: 64_125,
                    z_fixed: 65_535,
                    yaw_millidegrees: -90_001,
                    pitch_millidegrees: 45_002,
                },
                game_mode: Some(lodestone_model::GameMode::Creative),
                runtime: Some(crate::world_storage::NativePlayerRuntimeState {
                    health: 13.5,
                    air_supply: 240,
                    experience: crate::experience::PlayerExperience::restored(7, 0.25, 341),
                }),
                inventory: Some(fixture_player().to_inventory()),
            }),
            "fixture expectations name the typed native fields independently of conversion"
        );

        drop(storage);
        std::fs::remove_dir_all(directory).expect("remove scratch directory");
    }

    #[test]
    fn missing_or_stale_authorization_never_appends_a_locator() {
        let directory = scratch("authorization");
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: directory.clone(),
        })
        .expect("open native storage");
        let player = fixture_player();
        let uuid = uuid::Uuid::from_bytes([0x24; 16]);

        assert!(matches!(
            import_player(&storage, uuid, &player, None),
            Err(Error::MissingAuthorization)
        ));
        assert!(matches!(
            import_player(
                &storage,
                uuid,
                &player,
                Some(PlayerImportAuthorization::Lossless)
            ),
            Err(Error::AuthorizationMismatch { .. })
        ));
        assert!(storage.load_player(*uuid.as_bytes()).unwrap().is_none());

        drop(storage);
        std::fs::remove_dir_all(directory).expect("remove scratch directory");
    }

    #[test]
    fn precision_loss_requires_authorization_and_invalid_locator_values_block_it() {
        let mut player = fixture_player();
        player.pos.x = 1.0 / 3.0;
        player.rotation.pitch = f32::NAN;
        player.dimension = "example:custom".to_owned();
        let report = preflight_player(&player);

        assert!(report
            .unsupported()
            .contains(&UnsupportedPlayerData::PositionPrecision {
                axis: PositionAxis::X
            }));
        assert_eq!(
            report.decide(PlayerLossDecision::ProceedAndDiscardUnsupported),
            PlayerImportAuthorization::Blocked { blockers: 2 },
            "acknowledging ordinary loss cannot override a malformed locator"
        );
    }
}
