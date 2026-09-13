//! Staged export of typed native resident entities into one Anvil dimension.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    dimension::Dimension,
    entity_storage::{EntityStorage, SavedEntity},
    world_storage::{NativeEntityRecord, NativeGeneralRecord, WorldStorage},
};

/// Result of publishing one dimension's complete native entity snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityExportResult {
    /// Number of entity records published.
    pub entities_exported: usize,
}

/// Failure before or during native entity export.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Native storage could not produce a complete typed general snapshot.
    #[error("native entity snapshot failed: {0}")]
    Storage(#[source] crate::world_storage::Error),
    /// The destination must already be an Anvil world directory.
    #[error("Anvil entity export destination is not an existing directory: {path}")]
    MissingDestination { path: PathBuf },
    /// Existing entity sidecars are never replaced implicitly.
    #[error("Anvil entity export destination already has an entity directory: {path}")]
    DestinationExists { path: PathBuf },
    /// A prior interrupted export left the deterministic staging root behind.
    #[error("Anvil entity export staging path already exists: {path}")]
    StagingExists { path: PathBuf },
    /// The entity sidecar writer rejected the prepared population.
    #[error("Anvil entity export failed: {0}")]
    Entity(#[source] crate::region_source::Error),
    /// A filesystem publication operation failed.
    #[error("Anvil entity export filesystem operation failed: {0}")]
    Io(#[source] std::io::Error),
}

/// Exports one built-in dimension's typed native entity poses.
///
/// Every native general record is decoded before the destination is inspected,
/// so an unsupported or corrupt record cannot produce a partial sidecar. The
/// selected dimension's complete population is staged and its `entities`
/// directory is published with one same-filesystem rename.
pub fn export_entities(
    storage: &WorldStorage,
    destination: &Path,
    dimension: Dimension,
) -> Result<EntityExportResult, Error> {
    let entities: Vec<_> = storage
        .native_general_records()
        .map_err(Error::Storage)?
        .into_iter()
        .filter_map(|record| match record {
            NativeGeneralRecord::Entity(entity)
                if runtime_dimension(entity.dimension) == dimension =>
            {
                Some(entity)
            }
            NativeGeneralRecord::Entity(_)
            | NativeGeneralRecord::EntityRoster(_)
            | NativeGeneralRecord::WorldProperties(_)
            | NativeGeneralRecord::Player(_) => None,
        })
        .map(to_anvil_entity)
        .collect();

    if !destination.is_dir() {
        return Err(Error::MissingDestination {
            path: destination.to_owned(),
        });
    }
    let target = entity_directory(destination, dimension);
    if target.exists() {
        return Err(Error::DestinationExists { path: target });
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("world");
    let staging = parent.join(format!(".{name}.lodestone-entity-exporting"));
    if staging.exists() {
        return Err(Error::StagingExists { path: staging });
    }

    let result = (|| {
        let staged = EntityStorage::new_for_dimension(&staging, dimension).map_err(Error::Entity)?;
        staged.save(&entities).map_err(Error::Entity)?;
        let target_parent = target.parent().expect("entity directory has a dimension parent");
        fs::create_dir_all(target_parent).map_err(Error::Io)?;
        fs::rename(entity_directory(&staging, dimension), &target).map_err(Error::Io)?;
        Ok(EntityExportResult {
            entities_exported: entities.len(),
        })
    })();
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(Error::Io)?;
    }
    result
}

fn runtime_dimension(dimension: lodestone_storage_schema::BuiltinDimension) -> Dimension {
    match dimension {
        lodestone_storage_schema::BuiltinDimension::Overworld => Dimension::Overworld,
        lodestone_storage_schema::BuiltinDimension::Nether => Dimension::Nether,
        lodestone_storage_schema::BuiltinDimension::End => Dimension::End,
        lodestone_storage_schema::BuiltinDimension::Unspecified => {
            unreachable!("native decoder rejects unspecified dimensions")
        }
    }
}

fn entity_directory(world: &Path, dimension: Dimension) -> PathBuf {
    world
        .join("dimensions")
        .join("minecraft")
        .join(dimension.dir_name())
        .join("entities")
}

fn to_anvil_entity(entity: NativeEntityRecord) -> SavedEntity {
    let (health, item, age, pickup_delay) = match entity.state {
        Some(crate::world_storage::NativeEntityState::Living { health }) => {
            (Some(health), None, None, None)
        }
        Some(crate::world_storage::NativeEntityState::Item {
            item,
            count,
            age,
            pickup_delay,
        }) => (None, Some((item, count)), Some(age), Some(pickup_delay)),
        None => (None, None, None, None),
    };
    SavedEntity {
        id: entity.entity_type,
        uuid: uuid::Uuid::from_bytes(entity.uuid),
        pos: entity.position,
        motion: entity.motion,
        rotation: entity.rotation,
        health,
        item,
        age,
        pickup_delay,
        extra: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_storage::WorldStorageBackend;
    use lodestone_storage_schema::BuiltinDimension;

    fn scratch(name: &str) -> PathBuf {
        let unique = lodestone_time::epoch_duration().as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lodestone-native-entity-export-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated entity-export scratch directory");
        path
    }

    fn entity(uuid: [u8; 16], dimension: BuiltinDimension) -> NativeEntityRecord {
        NativeEntityRecord {
            uuid,
            entity_type: "minecraft:cow".parse().expect("canonical entity type"),
            dimension,
            position: lodestone_model::Vec3::new(1.25, 64.5, 2.75),
            rotation: lodestone_model::Rotation::new(-90.0, 30.0),
            motion: lodestone_model::Vec3::new(0.125, -0.25, 0.5),
            state: Some(crate::world_storage::NativeEntityState::Living { health: 7.5 }),
        }
    }

    #[test]
    fn selected_dimension_publishes_reopenable_entity_sidecar() {
        let scratch = scratch("round-trip");
        let destination = scratch.join("anvil");
        fs::create_dir(&destination).expect("create existing Anvil destination");
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: scratch.join("native"),
        })
        .expect("open native source");
        let overworld = entity([0x31; 16], BuiltinDimension::Overworld);
        let nether = entity([0x32; 16], BuiltinDimension::Nether);
        storage
            .write_dirty_entities(0, 0, 0, 128, [overworld.clone()])
            .expect("seed Overworld native entity");
        storage
            .write_dirty_entities(0, 0, 0, 128, [nether])
            .expect("seed Nether native entity");

        assert_eq!(
            export_entities(&storage, &destination, Dimension::Overworld)
                .expect("export Overworld entity snapshot"),
            EntityExportResult {
                entities_exported: 1,
            }
        );
        let exported = EntityStorage::open_readonly_for_dimension(
            &destination,
            Dimension::Overworld,
        )
        .load_chunk(0, 0)
        .expect("read exported sidecar");
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].uuid, uuid::Uuid::from_bytes(overworld.uuid));
        assert_eq!(exported[0].id, overworld.entity_type);
        assert_eq!(exported[0].pos, overworld.position);
        assert_eq!(exported[0].motion, overworld.motion);
        assert_eq!(exported[0].rotation, overworld.rotation);
        assert_eq!(exported[0].health, Some(7.5));
        assert!(
            EntityStorage::open_readonly_for_dimension(&destination, Dimension::Nether)
                .populated_chunks()
                .expect("inspect unselected Nether output")
                .is_empty(),
            "an Overworld export must not leak Nether records"
        );
        drop(storage);
        fs::remove_dir_all(scratch).expect("remove entity-export scratch directory");
    }

    #[test]
    fn existing_sidecar_is_not_merged_or_replaced() {
        let scratch = scratch("existing");
        let destination = scratch.join("anvil");
        let target = entity_directory(&destination, Dimension::End);
        fs::create_dir_all(&target).expect("create existing entity directory");
        let marker = target.join("owned.mca");
        fs::write(&marker, b"owned").expect("write existing marker");
        let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
            directory: scratch.join("native"),
        })
        .expect("open native source");

        assert!(matches!(
            export_entities(&storage, &destination, Dimension::End),
            Err(Error::DestinationExists { .. })
        ));
        assert_eq!(fs::read(marker).expect("read unchanged marker"), b"owned");
        drop(storage);
        fs::remove_dir_all(scratch).expect("remove entity-export scratch directory");
    }
}
