//! Authorization-gated import of one Anvil entity-sidecar chunk.
//!
//! The existing [`crate::entity_storage::EntityStorage`] codec owns the
//! `entities/` region layout and decodes complete [`crate::entity_storage::SavedEntity`]
//! values. This module is its deliberately narrow native consumer: it imports
//! one selected overworld chunk's durable identity, type, feet position, and
//! rotation into [`crate::world_storage::NativeEntityRecord`]. Motion, health,
//! item state, age, pickup delay, and preserved fields have no native field;
//! every one is reported before a caller may authorize the write.

use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};

use crate::{
    entity_storage::{EntityStorage, SavedEntity},
    world_storage::{NativeDirtyEntityChunk, NativeEntityRecord, WorldStorage},
};
use lodestone_storage_schema::BuiltinDimension;

/// One typed Anvil entity value absent from a native resident-entity record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnsupportedEntityData {
    /// Velocity has no native resident-entity field.
    Motion {
        /// Zero-based position in the selected source chunk's entity list.
        entity_index: usize,
    },
    /// Living health has no native resident-entity field.
    Health {
        /// Zero-based position in the selected source chunk's entity list.
        entity_index: usize,
    },
    /// Dropped-item stack state has no native resident-entity field.
    Item {
        /// Zero-based position in the selected source chunk's entity list.
        entity_index: usize,
    },
    /// Item lifetime has no native resident-entity field.
    Age {
        /// Zero-based position in the selected source chunk's entity list.
        entity_index: usize,
    },
    /// Item pickup delay has no native resident-entity field.
    PickupDelay {
        /// Zero-based position in the selected source chunk's entity list.
        entity_index: usize,
    },
    /// Fields preserved by the Anvil entity codec but not represented natively.
    PreservedFields {
        /// Zero-based position in the selected source chunk's entity list.
        entity_index: usize,
        /// Number of preserved fields that will not be written.
        fields: usize,
    },
}

/// One source condition that cannot become a safe resident-entity record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntityImportBlocker {
    /// The requested native extent is not a positive, section-aligned window.
    InvalidExtent {
        /// Requested minimum block Y coordinate.
        min_y: i32,
        /// Requested vertical size in blocks.
        height: i32,
    },
    /// An entity position contains NaN or infinity.
    NonFinitePosition {
        /// Zero-based position in the selected source chunk's entity list.
        entity_index: usize,
    },
    /// An entity rotation contains NaN or infinity.
    NonFiniteRotation {
        /// Zero-based position in the selected source chunk's entity list.
        entity_index: usize,
    },
    /// A finite position cannot be converted to a signed block coordinate.
    CoordinateOutOfRange {
        /// Zero-based position in the selected source chunk's entity list.
        entity_index: usize,
    },
    /// The entity pose belongs to another horizontal chunk column.
    OutsideColumn {
        /// Zero-based position in the selected source chunk's entity list.
        entity_index: usize,
        /// Block X coordinate derived from the entity's feet position.
        x: i32,
        /// Block Z coordinate derived from the entity's feet position.
        z: i32,
        /// Requested source-column X coordinate.
        expected_x: i32,
        /// Requested source-column Z coordinate.
        expected_z: i32,
    },
    /// The entity pose is outside the requested vertical window.
    OutsideExtent {
        /// Zero-based position in the selected source chunk's entity list.
        entity_index: usize,
        /// Block Y coordinate derived from the entity's feet position.
        y: i32,
        /// Requested minimum block Y coordinate.
        min_y: i32,
        /// Requested vertical size in blocks.
        height: i32,
    },
    /// Two entities in the selected source chunk have the same durable UUID.
    DuplicateUuid {
        /// Zero-based position of the later duplicate in the source list.
        entity_index: usize,
    },
}

/// Payload-free inventory for one entity-sidecar chunk conversion.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EntityImportReport {
    unsupported: Vec<UnsupportedEntityData>,
    blockers: Vec<EntityImportBlocker>,
}

impl EntityImportReport {
    /// Values a caller must explicitly accept before conversion.
    #[must_use]
    pub fn unsupported(&self) -> &[UnsupportedEntityData] {
        &self.unsupported
    }

    /// Values that must be repaired or supported before conversion.
    #[must_use]
    pub fn blockers(&self) -> &[EntityImportBlocker] {
        &self.blockers
    }

    /// Applies the required explicit import decision.
    #[must_use]
    pub fn decide(&self, decision: EntityLossDecision) -> EntityImportAuthorization {
        if !self.blockers.is_empty() {
            return EntityImportAuthorization::Blocked {
                blockers: self.blockers.len(),
            };
        }
        match decision {
            EntityLossDecision::Abort => EntityImportAuthorization::Aborted,
            EntityLossDecision::ProceedAndDiscardUnsupported if self.unsupported.is_empty() => {
                EntityImportAuthorization::Lossless
            }
            EntityLossDecision::ProceedAndDiscardUnsupported => {
                EntityImportAuthorization::LossAccepted {
                    discarded_entries: self.unsupported.len(),
                }
            }
        }
    }
}

/// A caller's decision after inspecting [`EntityImportReport`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityLossDecision {
    /// Do not import the selected entity chunk.
    Abort,
    /// Import its native fields while discarding every reported source value.
    ProceedAndDiscardUnsupported,
}

/// An authorization tied to one current entity conversion report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "pass this to import_entity_chunk; Anvil entity fields must not be discarded implicitly"]
pub enum EntityImportAuthorization {
    /// The caller declined the conversion.
    Aborted,
    /// The source has no values outside the resident-entity schema.
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

impl EntityImportAuthorization {
    fn permits_conversion(self) -> bool {
        matches!(self, Self::Lossless | Self::LossAccepted { .. })
    }
}

/// The report and native writes from one completed entity-sidecar conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityImportResult {
    /// The fresh report whose matching authorization permitted this write.
    pub report: EntityImportReport,
    /// Number of Anvil entity compounds seen in the selected source chunk.
    pub entities_seen: usize,
    /// Number of native records committed.
    pub records_written: usize,
}

/// One deterministic entity-sidecar source chunk selected for a batch import.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct SelectedEntityChunk {
    /// Chunk-column X coordinate.
    pub column_x: i32,
    /// Chunk-column Z coordinate.
    pub column_z: i32,
}

/// Filesystem selection for a native entity-sidecar conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntityChunkSelection {
    /// Every populated canonical overworld entity-sidecar chunk.
    All,
    /// Exactly these chunk columns, in deterministic coordinate order.
    Chunks(Vec<SelectedEntityChunk>),
}

/// One selected source chunk's payload-free loss report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityChunkImportReport {
    /// Source chunk-column X coordinate.
    pub column_x: i32,
    /// Source chunk-column Z coordinate.
    pub column_z: i32,
    /// Loss and safety report for that source sidecar chunk.
    pub report: EntityImportReport,
}

/// A source condition that prevents a whole entity-sidecar batch from being
/// committed safely.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntityBatchImportBlocker {
    /// A UUID appears in more than one selected source chunk.
    DuplicateUuid {
        /// The colliding durable identity.
        uuid: uuid::Uuid,
        /// First selected source chunk holding that identity.
        first: SelectedEntityChunk,
        /// Later selected source chunk holding that identity.
        second: SelectedEntityChunk,
    },
}

/// Payload-free aggregate review for a selected entity-sidecar batch.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EntityBatchImportReport {
    chunks: Vec<EntityChunkImportReport>,
    blockers: Vec<EntityBatchImportBlocker>,
}

impl EntityBatchImportReport {
    /// Source reports in deterministic `(x, z)` order.
    #[must_use]
    pub fn chunks(&self) -> &[EntityChunkImportReport] {
        &self.chunks
    }

    /// Cross-chunk conditions that no loss acknowledgement can authorize.
    #[must_use]
    pub fn blockers(&self) -> &[EntityBatchImportBlocker] {
        &self.blockers
    }

    /// Number of discarded source-field categories across the batch.
    #[must_use]
    pub fn unsupported_count(&self) -> usize {
        self.chunks
            .iter()
            .map(|chunk| chunk.report.unsupported().len())
            .sum()
    }

    /// Number of unsafe source values and cross-chunk collisions.
    #[must_use]
    pub fn blocker_count(&self) -> usize {
        self.blockers.len()
            + self
                .chunks
                .iter()
                .map(|chunk| chunk.report.blockers().len())
                .sum::<usize>()
    }

    /// Applies one decision after the entire sidecar selection is reviewed.
    #[must_use]
    pub fn decide(&self, decision: EntityLossDecision) -> EntityBatchImportAuthorization {
        if self.blocker_count() != 0 {
            return EntityBatchImportAuthorization::Blocked {
                blockers: self.blocker_count(),
            };
        }
        match decision {
            EntityLossDecision::Abort => EntityBatchImportAuthorization::Aborted,
            EntityLossDecision::ProceedAndDiscardUnsupported if self.unsupported_count() == 0 => {
                EntityBatchImportAuthorization::Lossless
            }
            EntityLossDecision::ProceedAndDiscardUnsupported => {
                EntityBatchImportAuthorization::LossAccepted {
                    discarded_entries: self.unsupported_count(),
                }
            }
        }
    }
}

/// An authorization tied to the exact aggregate entity-sidecar report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "pass this to import_entity_batch; entity fields must not be discarded implicitly"]
pub enum EntityBatchImportAuthorization {
    /// The caller declined conversion.
    Aborted,
    /// The complete source selection is lossless.
    Lossless,
    /// The caller accepted every reported discarded source value.
    LossAccepted {
        /// Number of report entries acknowledged by the caller.
        discarded_entries: usize,
    },
    /// The source contained non-representable or ambiguous records.
    Blocked {
        /// Number of blocking report entries.
        blockers: usize,
    },
}

impl EntityBatchImportAuthorization {
    fn permits_conversion(self) -> bool {
        matches!(self, Self::Lossless | Self::LossAccepted { .. })
    }
}

/// Prepared, typed inputs for a reviewed entity-sidecar batch.
#[derive(Clone, Debug)]
pub struct EntityBatchImportPlan {
    report: EntityBatchImportReport,
    chunks: Vec<NativeDirtyEntityChunk>,
}

impl EntityBatchImportPlan {
    /// The payload-free aggregate report requiring review.
    #[must_use]
    pub fn report(&self) -> &EntityBatchImportReport {
        &self.report
    }
}

/// Completed result for one committed entity-sidecar batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityBatchImportResult {
    /// Fresh aggregate report whose authorization permitted the native write.
    pub report: EntityBatchImportReport,
    /// Number of selected source chunks.
    pub chunks_seen: usize,
    /// Number of typed resident entity records committed in one transaction.
    pub records_written: usize,
}

/// An error that prevents an Anvil entity-sidecar chunk becoming native records.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The filesystem selection did not name any sidecar chunks.
    #[error("entity import selected no sidecar chunks")]
    NoSelectedChunks,
    /// An explicit entity chunk was named more than once.
    #[error("entity import selected chunk ({column_x}, {column_z}) more than once")]
    DuplicateChunkSelection {
        /// Duplicate chunk-column X coordinate.
        column_x: i32,
        /// Duplicate chunk-column Z coordinate.
        column_z: i32,
    },
    /// Native-only conversion needs an explicit review of every discarded field.
    #[error("Anvil entity import requires an explicit EntityImportAuthorization")]
    MissingAuthorization,
    /// The caller did not authorize conversion.
    #[error("Anvil entity import authorization does not permit conversion: {authorization:?}")]
    AuthorizationDenied {
        /// Authorization supplied by the caller.
        authorization: EntityImportAuthorization,
    },
    /// The source changed after preflight, or the authorization was derived
    /// from a different source/report.
    #[error(
        "Anvil entity import authorization does not match this entity chunk: supplied {supplied:?}, required {required:?}"
    )]
    AuthorizationMismatch {
        /// Authorization supplied by the caller.
        supplied: EntityImportAuthorization,
        /// Authorization the current source requires.
        required: EntityImportAuthorization,
    },
    /// The batch authorization was not supplied.
    #[error("Anvil entity batch import requires an explicit EntityBatchImportAuthorization")]
    MissingBatchAuthorization,
    /// The supplied aggregate decision cannot permit conversion.
    #[error("Anvil entity batch import authorization does not permit conversion: {authorization:?}")]
    BatchAuthorizationDenied {
        /// Authorization supplied by the caller.
        authorization: EntityBatchImportAuthorization,
    },
    /// The batch changed after review or the authorization targets another selection.
    #[error(
        "Anvil entity batch import authorization does not match the selected chunks: supplied {supplied:?}, required {required:?}"
    )]
    BatchAuthorizationMismatch {
        /// Authorization supplied by the caller.
        supplied: EntityBatchImportAuthorization,
        /// Authorization required by the prepared batch.
        required: EntityBatchImportAuthorization,
    },
    /// The selected Anvil entity sidecar could not be decoded.
    #[error("Anvil entity-sidecar read error: {0}")]
    EntitySidecar(#[source] crate::region_source::Error),
    /// The selected native backend refused or failed the entity write.
    #[error("native resident-entity storage failed: {0}")]
    Storage(#[source] crate::world_storage::Error),
}

/// Inventories every source field that native resident-entity records cannot retain.
///
/// The current Anvil codec normalizes a missing motion list to a zero vector,
/// so motion is reported for every decoded entity. This deliberately
/// conservative report avoids treating an omitted source field as native
/// support just because the codec supplied its semantic default.
#[must_use]
pub fn preflight_entities(
    column_x: i32,
    column_z: i32,
    min_y: i32,
    height: i32,
    entities: &[SavedEntity],
) -> EntityImportReport {
    let mut report = EntityImportReport::default();
    if min_y.rem_euclid(16) != 0 || height <= 0 {
        report
            .blockers
            .push(EntityImportBlocker::InvalidExtent { min_y, height });
    }

    let mut uuids = std::collections::HashSet::new();
    for (entity_index, entity) in entities.iter().enumerate() {
        report
            .unsupported
            .push(UnsupportedEntityData::Motion { entity_index });
        if entity.health.is_some() {
            report
                .unsupported
                .push(UnsupportedEntityData::Health { entity_index });
        }
        if entity.item.is_some() {
            report
                .unsupported
                .push(UnsupportedEntityData::Item { entity_index });
        }
        if entity.age.is_some() {
            report
                .unsupported
                .push(UnsupportedEntityData::Age { entity_index });
        }
        if entity.pickup_delay.is_some() {
            report
                .unsupported
                .push(UnsupportedEntityData::PickupDelay { entity_index });
        }
        if !entity.extra.is_empty() {
            report
                .unsupported
                .push(UnsupportedEntityData::PreservedFields {
                    entity_index,
                    fields: entity.extra.len(),
                });
        }
        if !uuids.insert(entity.uuid) {
            report
                .blockers
                .push(EntityImportBlocker::DuplicateUuid { entity_index });
        }
        inspect_pose(
            &mut report,
            entity_index,
            entity,
            column_x,
            column_z,
            min_y,
            height,
        );
    }
    report
}

/// Reads, preflights, and imports one selected overworld entity-sidecar chunk.
///
/// The source sidecar remains unchanged. The native storage record is a pose
/// locator, not an Anvil entity replacement, so this operation neither deletes
/// native records absent from the selected source chunk nor writes unsupported
/// fields as extensions.
pub fn import_entity_chunk(
    storage: &WorldStorage,
    sidecar: &EntityStorage,
    column_x: i32,
    column_z: i32,
    min_y: i32,
    height: i32,
    authorization: Option<EntityImportAuthorization>,
) -> Result<EntityImportResult, Error> {
    let Some(authorization) = authorization else {
        return Err(Error::MissingAuthorization);
    };
    if !authorization.permits_conversion() {
        return Err(Error::AuthorizationDenied { authorization });
    }

    let entities = sidecar
        .load_chunk(column_x, column_z)
        .map_err(Error::EntitySidecar)?;
    let report = preflight_entities(column_x, column_z, min_y, height, &entities);
    let required = report.decide(EntityLossDecision::ProceedAndDiscardUnsupported);
    if authorization != required {
        return Err(Error::AuthorizationMismatch {
            supplied: authorization,
            required,
        });
    }

    let records_written = storage
        .write_dirty_entities(
            column_x,
            column_z,
            min_y,
            height,
            entities.iter().map(native_record),
        )
        .map_err(Error::Storage)?;
    Ok(EntityImportResult {
        report,
        entities_seen: entities.len(),
        records_written,
    })
}

/// Discovers an explicit or complete deterministic entity-sidecar selection
/// without creating the source world's `entities/` directory.
pub fn discover_entity_chunks(
    world_directory: &Path,
    selection: EntityChunkSelection,
) -> Result<Vec<SelectedEntityChunk>, Error> {
    let selected = match selection {
        EntityChunkSelection::All => crate::entity_storage::EntityStorage::open_readonly(world_directory)
            .populated_chunks()
            .map_err(Error::EntitySidecar)?
            .into_iter()
            .map(|(column_x, column_z)| SelectedEntityChunk { column_x, column_z })
            .collect(),
        EntityChunkSelection::Chunks(chunks) => chunks,
    };
    let mut ordered = BTreeSet::new();
    for chunk in selected {
        if !ordered.insert(chunk) {
            return Err(Error::DuplicateChunkSelection {
                column_x: chunk.column_x,
                column_z: chunk.column_z,
            });
        }
    }
    if ordered.is_empty() {
        return Err(Error::NoSelectedChunks);
    }
    Ok(ordered.into_iter().collect())
}

/// Decodes and inventories every selected sidecar chunk before native storage
/// is opened. The returned plan retains only typed resident-entity poses.
pub fn preflight_entity_batch(
    world_directory: &Path,
    selected: &[SelectedEntityChunk],
    min_y: i32,
    height: i32,
) -> Result<EntityBatchImportPlan, Error> {
    if selected.is_empty() {
        return Err(Error::NoSelectedChunks);
    }
    let sidecar = crate::entity_storage::EntityStorage::open_readonly(world_directory);
    let mut reports = Vec::with_capacity(selected.len());
    let mut chunks = Vec::with_capacity(selected.len());
    let mut uuids = HashMap::new();
    let mut batch_blockers = Vec::new();
    for &selected_chunk in selected {
        let entities = sidecar
            .load_chunk(selected_chunk.column_x, selected_chunk.column_z)
            .map_err(Error::EntitySidecar)?;
        let report = preflight_entities(
            selected_chunk.column_x,
            selected_chunk.column_z,
            min_y,
            height,
            &entities,
        );
        for entity in &entities {
            if let Some(first) = uuids.insert(entity.uuid, selected_chunk) {
                batch_blockers.push(EntityBatchImportBlocker::DuplicateUuid {
                    uuid: entity.uuid,
                    first,
                    second: selected_chunk,
                });
            }
        }
        reports.push(EntityChunkImportReport {
            column_x: selected_chunk.column_x,
            column_z: selected_chunk.column_z,
            report,
        });
        chunks.push(NativeDirtyEntityChunk {
            column_x: selected_chunk.column_x,
            column_z: selected_chunk.column_z,
            min_y,
            height,
            entities: entities.iter().map(native_record).collect(),
        });
    }
    Ok(EntityBatchImportPlan {
        report: EntityBatchImportReport {
            chunks: reports,
            blockers: batch_blockers,
        },
        chunks,
    })
}

/// Commits every prepared entity-sidecar chunk in exactly one transaction.
///
/// Every source chunk has already decoded and every UUID/pose has already been
/// reviewed before this reaches [`WorldStorage::write_dirty_entity_chunks`].
pub fn import_entity_batch(
    storage: &WorldStorage,
    plan: EntityBatchImportPlan,
    authorization: Option<EntityBatchImportAuthorization>,
) -> Result<EntityBatchImportResult, Error> {
    let Some(authorization) = authorization else {
        return Err(Error::MissingBatchAuthorization);
    };
    if !authorization.permits_conversion() {
        return Err(Error::BatchAuthorizationDenied { authorization });
    }
    let required = plan
        .report
        .decide(EntityLossDecision::ProceedAndDiscardUnsupported);
    if authorization != required {
        return Err(Error::BatchAuthorizationMismatch {
            supplied: authorization,
            required,
        });
    }
    let chunks_seen = plan.chunks.len();
    let records_written = storage
        .write_dirty_entity_chunks(plan.chunks)
        .map_err(Error::Storage)?;
    Ok(EntityBatchImportResult {
        report: plan.report,
        chunks_seen,
        records_written,
    })
}

fn native_record(entity: &SavedEntity) -> NativeEntityRecord {
    NativeEntityRecord {
        uuid: *entity.uuid.as_bytes(),
        entity_type: entity.id.clone(),
        dimension: BuiltinDimension::Overworld,
        position: entity.pos,
        rotation: entity.rotation,
    }
}

fn inspect_pose(
    report: &mut EntityImportReport,
    entity_index: usize,
    entity: &SavedEntity,
    column_x: i32,
    column_z: i32,
    min_y: i32,
    height: i32,
) {
    if !entity.pos.x.is_finite() || !entity.pos.y.is_finite() || !entity.pos.z.is_finite() {
        report
            .blockers
            .push(EntityImportBlocker::NonFinitePosition { entity_index });
        return;
    }
    if !entity.rotation.yaw.is_finite() || !entity.rotation.pitch.is_finite() {
        report
            .blockers
            .push(EntityImportBlocker::NonFiniteRotation { entity_index });
        return;
    }
    let Some(x) = block_coordinate(entity.pos.x) else {
        report
            .blockers
            .push(EntityImportBlocker::CoordinateOutOfRange { entity_index });
        return;
    };
    let Some(y) = block_coordinate(entity.pos.y) else {
        report
            .blockers
            .push(EntityImportBlocker::CoordinateOutOfRange { entity_index });
        return;
    };
    let Some(z) = block_coordinate(entity.pos.z) else {
        report
            .blockers
            .push(EntityImportBlocker::CoordinateOutOfRange { entity_index });
        return;
    };
    if (x.div_euclid(16), z.div_euclid(16)) != (column_x, column_z) {
        report.blockers.push(EntityImportBlocker::OutsideColumn {
            entity_index,
            x,
            z,
            expected_x: column_x,
            expected_z: column_z,
        });
    }
    if !(min_y..min_y.saturating_add(height)).contains(&y) {
        report.blockers.push(EntityImportBlocker::OutsideExtent {
            entity_index,
            y,
            min_y,
            height,
        });
    }
}

fn block_coordinate(value: f64) -> Option<i32> {
    let floored = value.floor();
    (floored >= f64::from(i32::MIN) && floored <= f64::from(i32::MAX)).then_some(floored as i32)
}
