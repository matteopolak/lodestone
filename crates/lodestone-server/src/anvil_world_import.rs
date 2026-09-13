//! Deterministic, authorization-gated import of a world's terrain region set.
//!
//! This module composes the bounded region converter into one world-directory
//! operation. It discovers only canonical `region/r.<x>.<z>.mca` terrain
//! files, sorts them by region coordinates, and prepares every selected chunk
//! before opening the one native write transaction. Player files, entity and
//! POI sidecars, metadata, and auxiliary files intentionally remain separate
//! migration choices.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use lodestone_anvil::import_preflight::{ImportAuthorization, LossDecision, PreflightReport};

use crate::{
    anvil_import::{self, PreparedRegion},
    world_storage::WorldStorage,
};

/// The reviewed outcome of importing every terrain region selected from a
/// world directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldImportResult {
    /// One ordered, payload-free inventory over all selected terrain chunks.
    pub report: PreflightReport,
    /// Number of canonical terrain region files selected from `region/`.
    pub regions_seen: usize,
    /// Number of present chunk entries selected across all regions.
    pub chunks_seen: usize,
    /// Number of complete native chunk records committed in one transaction.
    pub records_written: usize,
}

/// A failure while discovering, preparing, authorizing, or committing a world
/// terrain import.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The selected world directory could not be walked.
    #[error("Anvil world terrain walk failed: {0}")]
    Io(#[source] std::io::Error),
    /// A terrain file would be silently skipped if its name did not establish
    /// the absolute chunk coordinate contract.
    #[error("terrain region file has no canonical r.<x>.<z>.mca name: {path}")]
    UnexpectedRegionFile {
        /// Path that was present under the selected `region/` directory.
        path: PathBuf,
    },
    /// The caller did not supply an explicit review decision.
    #[error("Anvil world import requires an explicit ImportAuthorization")]
    MissingAuthorization,
    /// The supplied decision cannot permit an import.
    #[error("Anvil world import authorization does not permit conversion: {authorization:?}")]
    AuthorizationDenied {
        /// Authorization supplied by the caller.
        authorization: ImportAuthorization,
    },
    /// The source changed after preflight or an authorization belongs to a
    /// different aggregate report.
    #[error(
        "Anvil world import authorization does not match this source: supplied {supplied:?}, required {required:?}"
    )]
    AuthorizationMismatch {
        /// Authorization supplied by the caller.
        supplied: ImportAuthorization,
        /// Authorization required by the fresh aggregate report.
        required: ImportAuthorization,
    },
    /// One selected terrain region could not become a complete native record.
    #[error("Anvil terrain region conversion failed: {0}")]
    Region(#[from] anvil_import::Error),
    /// The selected native backend rejected the aggregate dirty-record batch.
    #[error("native world storage failed: {0}")]
    Storage(#[source] crate::world_storage::Error),
}

/// Inventories all canonical terrain region files in deterministic coordinate
/// order without changing the destination backend.
///
/// The selected directory is an Anvil world root, whose terrain files live in
/// its `region/` child. Entity, POI, player, metadata, and auxiliary paths do
/// not participate in this bounded terrain operation.
pub fn preflight_world_directory(
    dimension: impl Into<String>,
    world_directory: impl AsRef<Path>,
) -> Result<PreflightReport, Error> {
    let dimension = dimension.into();
    let reports = discover_regions(world_directory.as_ref())?
        .into_iter()
        .map(|((region_x, region_z), path)| {
            anvil_import::preflight_region_file(&dimension, region_x, region_z, path)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PreflightReport::combine(reports))
}

/// Imports all canonical terrain region files from one world directory.
///
/// Discovery and preparation are deterministic and complete before the
/// aggregate report is authorized. A malformed later region therefore returns
/// before any native write; successful preparation commits every chunk through
/// one `WorldStorage::write_dirty_chunks` transaction.
pub fn import_world_directory(
    storage: &WorldStorage,
    dimension: impl Into<String>,
    world_directory: impl AsRef<Path>,
    min_y: i32,
    height: i32,
    authorization: Option<ImportAuthorization>,
) -> Result<WorldImportResult, Error> {
    let Some(authorization) = authorization else {
        return Err(Error::MissingAuthorization);
    };
    if !authorization.permits_conversion() {
        return Err(Error::AuthorizationDenied { authorization });
    }

    let dimension = dimension.into();
    let regions = discover_regions(world_directory.as_ref())?;
    let mut prepared = Vec::with_capacity(regions.len());
    for ((region_x, region_z), path) in regions {
        prepared.push(anvil_import::prepare_region_file(
            &dimension, region_x, region_z, &path, min_y, height,
        )?);
    }
    let report = PreflightReport::combine(prepared.iter().map(|region| region.report.clone()));
    let required = report.decide(LossDecision::ProceedAndDiscardUnsupported);
    if authorization != required {
        return Err(Error::AuthorizationMismatch {
            supplied: authorization,
            required,
        });
    }

    let regions_seen = prepared.len();
    let chunks_seen = prepared.iter().map(|region| region.chunks_seen).sum();
    let records_written = storage
        .write_dirty_chunks(prepared.iter().flat_map(PreparedRegion::dirty_records))
        .map_err(Error::Storage)?;
    Ok(WorldImportResult {
        report,
        regions_seen,
        chunks_seen,
        records_written,
    })
}

fn discover_regions(world_directory: &Path) -> Result<BTreeMap<(i32, i32), PathBuf>, Error> {
    let directory = world_directory.join("region");
    let mut regions = BTreeMap::new();
    for entry in fs::read_dir(&directory).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("mca") {
            continue;
        }
        if !entry.file_type().map_err(Error::Io)?.is_file() {
            return Err(Error::UnexpectedRegionFile { path });
        }
        let Some((region_x, region_z)) = parse_region_name(&path) else {
            return Err(Error::UnexpectedRegionFile { path });
        };
        regions.insert((region_x, region_z), path);
    }
    Ok(regions)
}

fn parse_region_name(path: &Path) -> Option<(i32, i32)> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".mca")?;
    let mut parts = stem.split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("r"), Some(x), Some(z), None) => Some((x.parse().ok()?, z.parse().ok()?)),
        _ => None,
    }
}
