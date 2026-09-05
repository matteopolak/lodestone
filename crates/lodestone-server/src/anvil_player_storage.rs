//! Authorization-gated import of one Anvil player file into a native locator.
//!
//! The native player schema is deliberately a locator, not a replacement for
//! a complete player save. This module converts the identity, built-in
//! dimension, feet position, and rotation from one selected Anvil player root;
//! it inventories every other player value before writing the native record.
//! The Anvil file remains the authoritative complete player state.

use std::path::Path;

use lodestone_storage_schema::BuiltinDimension;

use crate::{
    player_data::PlayerData,
    world_storage::{NativePlayerRecord, WorldStorage},
};

/// The fixed-point producer contract used by this importer.
///
/// [`NativePlayerRecord`] deliberately leaves its coordinate unit to the
/// producer. A record written by this module has one thousand units per block,
/// so only a consumer that has selected the same contract may interpret it.
pub const POSITION_UNITS_PER_BLOCK: f64 = 1_000.0;

/// A player value that the native locator cannot retain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedPlayerData {
    /// The source game-data version validates the Anvil schema but has no
    /// native locator field.
    DataVersion,
    /// Velocity has no native locator field.
    Motion,
    /// Health, air supply, and fire ticks have no native locator fields.
    VitalState,
    /// Fall distance has no native locator field.
    FallDistance,
    /// Ground contact has no native locator field.
    GroundState,
    /// Game mode has no native locator field.
    GameMode,
    /// The selected inventory slot has no native locator field.
    SelectedSlot,
    /// Inventory contents have no native locator field.
    Inventory,
    /// Experience values have no native locator field.
    Experience,
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
    /// The selected Anvil player file could not be decoded.
    #[error("Anvil player-data read error: {0}")]
    PlayerDat(#[source] lodestone_anvil::Error),
    /// The selected native backend refused or failed the locator write.
    #[error("native player locator storage failed: {0}")]
    Storage(#[source] crate::world_storage::Error),
}

/// Inventories every source field that one native locator cannot retain.
///
/// The report is payload-free: it records field categories and precision loss,
/// never the player inventory or preserved root values themselves.
#[must_use]
pub fn preflight_player(player: &PlayerData) -> PlayerImportReport {
    let mut report = PlayerImportReport {
        unsupported: vec![
            UnsupportedPlayerData::DataVersion,
            UnsupportedPlayerData::Motion,
            UnsupportedPlayerData::VitalState,
            UnsupportedPlayerData::FallDistance,
            UnsupportedPlayerData::GroundState,
            UnsupportedPlayerData::GameMode,
            UnsupportedPlayerData::SelectedSlot,
            UnsupportedPlayerData::Inventory,
            UnsupportedPlayerData::Experience,
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
        .write_dirty_player(locator_from_player(uuid, player))
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

fn locator_from_player(uuid: uuid::Uuid, player: &PlayerData) -> NativePlayerRecord {
    NativePlayerRecord {
        uuid: *uuid.as_bytes(),
        dimension: builtin_dimension(&player.dimension)
            .expect("preflight authorization rejects unsupported player dimensions"),
        x_fixed: round_to_i32(player.pos.x * POSITION_UNITS_PER_BLOCK),
        y_fixed: round_to_i32(player.pos.y * POSITION_UNITS_PER_BLOCK),
        z_fixed: round_to_i32(player.pos.z * POSITION_UNITS_PER_BLOCK),
        yaw_millidegrees: round_to_i32(f64::from(player.rotation.yaw) * 1_000.0),
        pitch_millidegrees: round_to_i32(f64::from(player.rotation.pitch) * 1_000.0),
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
    fn independent_player_fixture_decodes_through_anvil_codec_and_maps_locator_fields() {
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
                .contains(&UnsupportedPlayerData::Inventory),
            "the native locator must make inventory loss visible"
        );
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
            storage.load_player(*uuid.as_bytes()).expect("load locator"),
            Some(NativePlayerRecord {
                uuid: *uuid.as_bytes(),
                dimension: BuiltinDimension::Nether,
                x_fixed: -16_384,
                y_fixed: 64_125,
                z_fixed: 65_535,
                yaw_millidegrees: -90_001,
                pitch_millidegrees: 45_002,
            }),
            "fixture expectations name the native fields independently of conversion"
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
