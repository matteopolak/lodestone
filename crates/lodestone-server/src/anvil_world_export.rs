//! Deterministic, authorization-gated export of complete native terrain chunks.
//!
//! An explicit caller selection and an all-native point-in-time snapshot both
//! prepare the complete source batch before this module creates a staging
//! directory. Publication is one directory rename.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use lodestone_anvil::{
    CompressionScheme,
    region::{build_region_from_nbt, region_and_local},
};
use lodestone_core::Nbt;

use crate::{
    anvil_export::{self, ChunkExportReport, ExportLossDecision},
    world_storage::{NativeChunkRecord, WorldStorage},
};

/// One absolute chunk coordinate selected for a terrain export.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ChunkCoordinate {
    /// Chunk X coordinate.
    pub x: i32,
    /// Chunk Z coordinate.
    pub z: i32,
}

/// The complete, explicit input contract for a native terrain export.
///
/// The selected chunk coordinates are sorted by this constructor. A caller
/// supplies the vertical extent needed to reopen typed native records, plus
/// every output-affecting value: tick conversion time, compression, and region
/// timestamp. No current clock or native-store enumeration participates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldExportInput {
    chunks: Vec<ChunkCoordinate>,
    min_y: i32,
    height: i32,
    game_time: u64,
    compression: CompressionScheme,
    timestamp: u32,
}

impl WorldExportInput {
    /// Validates and canonicalizes a selected terrain batch.
    pub fn new(
        mut chunks: Vec<ChunkCoordinate>,
        min_y: i32,
        height: i32,
        game_time: u64,
        compression: CompressionScheme,
        timestamp: u32,
    ) -> Result<Self, Error> {
        if chunks.is_empty() {
            return Err(Error::EmptySelection);
        }
        if height <= 0 || min_y.rem_euclid(16) != 0 {
            return Err(Error::InvalidChunkExtent { min_y, height });
        }
        chunks.sort_unstable();
        if let Some(coordinate) = chunks.windows(2).find_map(|pair| {
            (pair[0] == pair[1]).then_some(pair[0])
        }) {
            return Err(Error::DuplicateChunk { coordinate });
        }
        Ok(Self {
            chunks,
            min_y,
            height,
            game_time,
            compression,
            timestamp,
        })
    }

    /// The canonical coordinate order this export will read and emit.
    #[must_use]
    pub fn chunks(&self) -> &[ChunkCoordinate] {
        &self.chunks
    }
}

/// One complete native terrain selection captured for reviewed export.
///
/// The snapshot owns decoded records rather than just their keys. A later
/// native write therefore cannot replace a reviewed column between preflight
/// and [`export_native_world_snapshot`]. Callers can capture either an
/// explicit [`WorldExportInput`] selection or every committed terrain record.
#[derive(Debug)]
pub struct NativeWorldExportSnapshot {
    input: WorldExportInput,
    selected: Vec<(ChunkCoordinate, NativeChunkRecord)>,
}

impl NativeWorldExportSnapshot {
    /// The canonical coordinates captured in this point-in-time selection.
    #[must_use]
    pub fn chunks(&self) -> &[ChunkCoordinate] {
        self.input.chunks()
    }
}

/// One selected chunk's payload-free native-to-Anvil loss inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldChunkExportReport {
    /// Absolute source chunk coordinate.
    pub coordinate: ChunkCoordinate,
    /// The one-chunk converter's loss inventory.
    pub report: ChunkExportReport,
}

/// The aggregate loss inventory for one explicit terrain batch.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorldExportReport {
    chunks: Vec<WorldChunkExportReport>,
}

impl WorldExportReport {
    /// Every selected chunk's report, in canonical coordinate order.
    #[must_use]
    pub fn chunks(&self) -> &[WorldChunkExportReport] {
        &self.chunks
    }

    /// Number of native-only fields that this entire selected batch discards.
    #[must_use]
    pub fn unsupported_count(&self) -> usize {
        self.chunks
            .iter()
            .map(|chunk| chunk.report.unsupported().len())
            .sum()
    }

    /// Applies one aggregate caller decision to this exact report.
    #[must_use]
    pub fn decide(&self, decision: WorldExportLossDecision) -> WorldExportAuthorization {
        match decision {
            WorldExportLossDecision::Abort => WorldExportAuthorization::Aborted,
            WorldExportLossDecision::ProceedAndDiscardUnsupported
                if self.unsupported_count() == 0 =>
            {
                WorldExportAuthorization::Lossless
            }
            WorldExportLossDecision::ProceedAndDiscardUnsupported => {
                WorldExportAuthorization::LossAccepted {
                    discarded_features: self.unsupported_count(),
                }
            }
        }
    }
}

/// A caller's decision after reviewing an aggregate world export report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldExportLossDecision {
    /// Do not create an output world directory.
    Abort,
    /// Export all selected terrain while discarding all reported native-only values.
    ProceedAndDiscardUnsupported,
}

/// Authorization bound to one exact aggregate world export report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "pass this result to export_world_directory; native-only fields must not be discarded implicitly"]
pub enum WorldExportAuthorization {
    /// The caller declined export.
    Aborted,
    /// The selected batch is fully representable.
    Lossless,
    /// The caller accepted exactly this many discarded native-only fields.
    LossAccepted {
        /// Count of report entries accepted by the caller.
        discarded_features: usize,
    },
}

impl WorldExportAuthorization {
    fn permits_export(self) -> bool {
        matches!(self, Self::Lossless | Self::LossAccepted { .. })
    }
}

/// The completed result from one published terrain-world directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldExportResult {
    /// The reviewed aggregate report that authorized this export.
    pub report: WorldExportReport,
    /// Number of selected native records converted.
    pub chunks_exported: usize,
    /// Number of terrain region files published.
    pub regions_published: usize,
}

/// A failure while selecting, converting, staging, or publishing terrain.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Native terrain export refuses an empty selected or captured source.
    #[error("Anvil world export requires at least one native terrain record")]
    EmptySelection,
    /// The native record decoder requires a positive, section-aligned extent.
    #[error("native chunk extent min_y={min_y}, height={height} is invalid")]
    InvalidChunkExtent {
        /// Requested lower build bound.
        min_y: i32,
        /// Requested vertical extent.
        height: i32,
    },
    /// A coordinate appeared more than once in an otherwise explicit batch.
    #[error("Anvil world export selected chunk ({coordinate:?}) more than once")]
    DuplicateChunk {
        /// Repeated coordinate.
        coordinate: ChunkCoordinate,
    },
    /// The caller omitted aggregate loss authorization.
    #[error("Anvil world export requires an explicit WorldExportAuthorization")]
    MissingAuthorization,
    /// The caller declined the export.
    #[error("Anvil world export authorization does not permit conversion: {authorization:?}")]
    AuthorizationDenied {
        /// Supplied decision.
        authorization: WorldExportAuthorization,
    },
    /// The native selection changed after its report was reviewed.
    #[error(
        "Anvil world export authorization does not match this native selection: supplied {supplied:?}, required {required:?}"
    )]
    AuthorizationMismatch {
        /// Supplied decision.
        supplied: WorldExportAuthorization,
        /// Decision required by the newly prepared aggregate report.
        required: WorldExportAuthorization,
    },
    /// A named chunk had no complete typed native record.
    #[error("selected native chunk ({coordinate:?}) is absent")]
    MissingChunk {
        /// Missing coordinate.
        coordinate: ChunkCoordinate,
    },
    /// The selected typed backend could not load a record.
    #[error("native world storage failed: {0}")]
    Storage(#[source] crate::world_storage::Error),
    /// A prepared record cannot become a complete Anvil chunk tree.
    #[error("native chunk ({coordinate:?}) cannot export to Anvil: {source}")]
    Chunk {
        /// Coordinate of the rejected record.
        coordinate: ChunkCoordinate,
        /// One-chunk conversion failure.
        #[source]
        source: anvil_export::Error,
    },
    /// The caller's target is not a new world directory.
    #[error("Anvil world export destination already exists: {path}")]
    DestinationExists {
        /// Existing publication path.
        path: PathBuf,
    },
    /// A prior interrupted export must be inspected or removed explicitly.
    #[error("Anvil world export staging directory already exists: {path}")]
    StagingExists {
        /// Existing same-parent staging path.
        path: PathBuf,
    },
    /// The destination has no usable leaf name for a same-parent staging directory.
    #[error("Anvil world export destination has no usable directory name: {path}")]
    InvalidDestination {
        /// Invalid destination path.
        path: PathBuf,
    },
    /// Staging or publishing filesystem work failed.
    #[error("Anvil world export filesystem operation failed: {0}")]
    Io(#[source] std::io::Error),
    /// Existing Anvil container encoding rejected a prepared terrain region.
    #[error("Anvil region encoding failed: {0}")]
    Anvil(#[source] lodestone_anvil::Error),
}

/// Preflights one explicit selected native terrain batch without filesystem mutation.
pub fn preflight_world_export(
    storage: &WorldStorage,
    input: &WorldExportInput,
) -> Result<WorldExportReport, Error> {
    Ok(report_for(&load_selected(storage, input)?))
}

/// Captures one explicit native terrain selection for reviewed export.
///
/// Unlike [`preflight_world_export`] followed by [`export_world_directory`],
/// this holds the storage lock from the reviewed selection through every typed
/// decode, then owns the resulting records. A later incremental native write
/// cannot replace a selected column before [`export_native_world_snapshot`]
/// publishes it. The supplied input remains the snapshot's complete output
/// contract and was already validated by [`WorldExportInput::new`].
pub fn snapshot_world_export(
    storage: &WorldStorage,
    input: &WorldExportInput,
) -> Result<NativeWorldExportSnapshot, Error> {
    let coordinates = input
        .chunks
        .iter()
        .map(|coordinate| lodestone_storage::NativeChunkCoordinate {
            column_x: coordinate.x,
            column_z: coordinate.z,
        })
        .collect::<Vec<_>>();
    let selected = storage
        .native_chunk_records_for(&coordinates, input.min_y, input.height)
        .map_err(Error::Storage)?
        .into_iter()
        .map(|snapshot| {
            (
                ChunkCoordinate {
                    x: snapshot.coordinate.column_x,
                    z: snapshot.coordinate.column_z,
                },
                snapshot.record,
            )
        })
        .collect();
    Ok(NativeWorldExportSnapshot {
        input: input.clone(),
        selected,
    })
}

/// Captures every complete committed native terrain record for a reviewed export.
///
/// The storage boundary holds its native lock from recovered-index discovery
/// through every typed decode. This function then owns those records, so a
/// later preflight and export operate on precisely the same terrain values.
/// Empty native storage is rejected rather than producing an empty Anvil world.
pub fn snapshot_native_world_export(
    storage: &WorldStorage,
    min_y: i32,
    height: i32,
    game_time: u64,
    compression: CompressionScheme,
    timestamp: u32,
) -> Result<NativeWorldExportSnapshot, Error> {
    let selected = storage
        .native_chunk_records(min_y, height)
        .map_err(Error::Storage)?
        .into_iter()
        .map(|snapshot| {
            (
                ChunkCoordinate {
                    x: snapshot.coordinate.column_x,
                    z: snapshot.coordinate.column_z,
                },
                snapshot.record,
            )
        })
        .collect::<Vec<_>>();
    let input = WorldExportInput::new(
        selected.iter().map(|(coordinate, _)| *coordinate).collect(),
        min_y,
        height,
        game_time,
        compression,
        timestamp,
    )?;
    Ok(NativeWorldExportSnapshot { input, selected })
}

/// Preflights an all-native point-in-time terrain snapshot without filesystem mutation.
#[must_use]
pub fn preflight_native_world_export(snapshot: &NativeWorldExportSnapshot) -> WorldExportReport {
    report_for(&snapshot.selected)
}

/// Exports an explicit native terrain selection into a new Anvil world directory.
///
/// The output directory must not exist. The complete native batch is loaded,
/// authorized, and converted before the coordinator creates its same-parent
/// staging directory. All terrain regions and oversized sidecars are written
/// under staging; a final directory rename is the only publication step.
/// Metadata, player data, entities, POI, and every other auxiliary path are
/// intentionally outside this terrain-only operation.
pub fn export_world_directory(
    storage: &WorldStorage,
    input: &WorldExportInput,
    destination: impl AsRef<Path>,
    authorization: Option<WorldExportAuthorization>,
) -> Result<WorldExportResult, Error> {
    let selected = load_selected(storage, input)?;
    export_selected(&selected, input, destination.as_ref(), authorization)
}

/// Publishes the exact all-native terrain values previously captured in `snapshot`.
///
/// Unlike [`export_world_directory`], this function never reads the native
/// store. The report used to authorize it and the converted terrain both come
/// from `snapshot`, making a reviewed point-in-time export independent of
/// subsequent incremental native writes.
pub fn export_native_world_snapshot(
    snapshot: &NativeWorldExportSnapshot,
    destination: impl AsRef<Path>,
    authorization: Option<WorldExportAuthorization>,
) -> Result<WorldExportResult, Error> {
    export_selected(
        &snapshot.selected,
        &snapshot.input,
        destination.as_ref(),
        authorization,
    )
}

fn export_selected(
    selected: &[(ChunkCoordinate, NativeChunkRecord)],
    input: &WorldExportInput,
    destination: &Path,
    authorization: Option<WorldExportAuthorization>,
) -> Result<WorldExportResult, Error> {
    let Some(authorization) = authorization else {
        return Err(Error::MissingAuthorization);
    };
    if !authorization.permits_export() {
        return Err(Error::AuthorizationDenied { authorization });
    }

    let report = report_for(selected);
    let required = report.decide(WorldExportLossDecision::ProceedAndDiscardUnsupported);
    if authorization != required {
        return Err(Error::AuthorizationMismatch {
            supplied: authorization,
            required,
        });
    }

    let regions = convert_selected(selected, input)?;
    let regions_published = regions.len();
    publish_regions(destination, &regions, input.compression, input.timestamp)?;
    Ok(WorldExportResult {
        report,
        chunks_exported: selected.len(),
        regions_published,
    })
}

fn load_selected(
    storage: &WorldStorage,
    input: &WorldExportInput,
) -> Result<Vec<(ChunkCoordinate, NativeChunkRecord)>, Error> {
    input
        .chunks
        .iter()
        .copied()
        .map(|coordinate| {
            storage
                .load_chunk(coordinate.x, coordinate.z, input.min_y, input.height)
                .map_err(Error::Storage)?
                .map(|record| (coordinate, record))
                .ok_or(Error::MissingChunk { coordinate })
        })
        .collect()
}

fn report_for(selected: &[(ChunkCoordinate, NativeChunkRecord)]) -> WorldExportReport {
    WorldExportReport {
        chunks: selected
            .iter()
            .map(|(coordinate, record)| WorldChunkExportReport {
                coordinate: *coordinate,
                report: anvil_export::preflight_chunk(record),
            })
            .collect(),
    }
}

fn convert_selected(
    selected: &[(ChunkCoordinate, NativeChunkRecord)],
    input: &WorldExportInput,
) -> Result<BTreeMap<(i32, i32), BTreeMap<(i32, i32), Nbt>>, Error> {
    let mut regions = BTreeMap::new();
    for (coordinate, record) in selected {
        let report = anvil_export::preflight_chunk(record);
        let authorization = report.decide(ExportLossDecision::ProceedAndDiscardUnsupported);
        let chunk = anvil_export::export_chunk(
            coordinate.x,
            coordinate.z,
            record,
            input.game_time,
            Some(authorization),
        )
        .map_err(|source| Error::Chunk {
            coordinate: *coordinate,
            source,
        })?
        .chunk;
        let (region_x, region_z, _, _) = region_and_local(coordinate.x, coordinate.z);
        regions
            .entry((region_x, region_z))
            .or_insert_with(BTreeMap::new)
            .insert((coordinate.x, coordinate.z), chunk);
    }
    Ok(regions)
}

fn publish_regions(
    destination: &Path,
    regions: &BTreeMap<(i32, i32), BTreeMap<(i32, i32), Nbt>>,
    compression: CompressionScheme,
    timestamp: u32,
) -> Result<(), Error> {
    if destination.exists() {
        return Err(Error::DestinationExists {
            path: destination.to_path_buf(),
        });
    }
    let staging = staging_directory(destination)?;
    if staging.exists() {
        return Err(Error::StagingExists { path: staging });
    }
    let region_directory = staging.join("region");
    fs::create_dir_all(&region_directory).map_err(Error::Io)?;
    for (&(region_x, region_z), chunks) in regions {
        let built = build_region_from_nbt(chunks, compression, timestamp).map_err(Error::Anvil)?;
        let region_path = region_directory.join(format!("r.{region_x}.{region_z}.mca"));
        fs::write(region_path, built.bytes).map_err(Error::Io)?;
        for (chunk_x, chunk_z, bytes) in built.external {
            let external = region_directory.join(format!("c.{chunk_x}.{chunk_z}.mcc"));
            fs::write(external, bytes).map_err(Error::Io)?;
        }
    }
    fs::rename(staging, destination).map_err(Error::Io)
}

fn staging_directory(destination: &Path) -> Result<PathBuf, Error> {
    let Some(name) = destination.file_name().and_then(|name| name.to_str()) else {
        return Err(Error::InvalidDestination {
            path: destination.to_path_buf(),
        });
    };
    Ok(destination.with_file_name(format!(".{name}.lodestone-export-staging")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_sorts_coordinates_and_rejects_duplicates() {
        let input = WorldExportInput::new(
            vec![
                ChunkCoordinate { x: 32, z: 0 },
                ChunkCoordinate { x: 0, z: 0 },
            ],
            0,
            16,
            0,
            CompressionScheme::Zlib,
            1,
        )
        .expect("distinct section-aligned selection is valid");
        assert_eq!(
            input.chunks(),
            &[ChunkCoordinate { x: 0, z: 0 }, ChunkCoordinate { x: 32, z: 0 }]
        );
        assert!(matches!(
            WorldExportInput::new(
                vec![ChunkCoordinate { x: 0, z: 0 }, ChunkCoordinate { x: 0, z: 0 }],
                0,
                16,
                0,
                CompressionScheme::Zlib,
                1,
            ),
            Err(Error::DuplicateChunk { .. })
        ));
    }
}
