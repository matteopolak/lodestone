//! Authorization-gated import of one Anvil entity-sidecar chunk.
//!
//! The existing [`crate::entity_storage::EntityStorage`] codec owns the
//! `entities/` region layout and decodes complete [`crate::entity_storage::SavedEntity`]
//! values. This module is its deliberately narrow native consumer: it imports
//! one selected overworld chunk's durable identity, type, feet position, and
//! rotation into [`crate::world_storage::NativeEntityRecord`]. Motion, health,
//! item state, age, pickup delay, and preserved fields have no native field;
//! every one is reported before a caller may authorize the write.

use crate::{
    entity_storage::{EntityStorage, SavedEntity},
    world_storage::{NativeEntityRecord, WorldStorage},
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

/// An error that prevents an Anvil entity-sidecar chunk becoming native records.
#[derive(Debug, thiserror::Error)]
pub enum Error {
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
