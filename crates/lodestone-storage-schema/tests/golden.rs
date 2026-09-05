use lodestone_storage_schema::generated::{general_record, storage_record};
use lodestone_storage_schema::{
    validate_extension_table, validate_record, validate_record_with_extensions, BiomeSection,
    BuiltinBiome, BuiltinDimension, EntityRecord, ExtensionTable, FORMAT_VERSION_V1, GameMode,
    GeneralRecord, LightData, LightSection, PlayerRecord, PlayerRuntimeState, ScheduledTick,
    ScheduledTickKind, ScheduledTickPriority, StorageRecord,
    ValidationError,
};
use prost::Message;

const CHUNK_V1: &str = include_str!("fixtures/chunk-v1.hex");
const WORLD_PROPERTIES_V1: &str = include_str!("fixtures/world-properties-v1.hex");
const EXTENSIONS_V1: &str = include_str!("fixtures/extensions-v1.hex");
const PLAYER_RUNTIME_V1: &str = include_str!("fixtures/player-runtime-v1.hex");
const PLAYER_INVENTORY_V1: &str = include_str!("fixtures/player-inventory-v1.hex");

#[test]
fn player_inventory_fixture_is_sparse_and_slot_typed() {
    let expected = fixture(PLAYER_INVENTORY_V1);
    let inventory = lodestone_storage_schema::PlayerInventory::decode(expected.as_slice()).unwrap();
    assert_eq!(inventory.selected_hotbar_slot, 4);
    assert_eq!(inventory.occupied_slots.len(), 1);
    let item = &inventory.occupied_slots[0];
    assert_eq!(item.slot, 4);
    assert_eq!(item.item_key, "minecraft:stone");
    assert_eq!(item.count, 32);
    assert!(item.custom_data.is_empty());
    assert_eq!(inventory.encode_to_vec(), expected);

    let mut record = StorageRecord {
        format_version: FORMAT_VERSION_V1,
        record: Some(storage_record::Record::General(GeneralRecord {
            extensions: Vec::new(),
            record: Some(general_record::Record::Player(PlayerRecord {
                player_uuid: vec![1; 16],
                dimension: BuiltinDimension::Overworld as i32,
                x_fixed: 0,
                y_fixed: 64_000,
                z_fixed: 0,
                yaw_millidegrees: 0,
                pitch_millidegrees: 0,
                game_mode: GameMode::Survival as i32,
                runtime_state: None,
                inventory: Some(inventory.clone()),
            })),
        })),
    };
    validate_record(&record).unwrap();
    player_in(&mut record)
        .inventory
        .as_mut()
        .unwrap()
        .occupied_slots
        .push(inventory.occupied_slots[0].clone());
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::DuplicatePlayerInventorySlot(4))
    );
}

#[test]
fn player_runtime_fixture_is_the_specified_scalar_wire_group() {
    let expected = fixture(PLAYER_RUNTIME_V1);
    let runtime = PlayerRuntimeState::decode(expected.as_slice()).unwrap();
    assert_eq!(runtime.health, 17.5);
    assert_eq!(runtime.air_supply, 123);
    assert_eq!(runtime.experience_level, 7);
    assert_eq!(runtime.experience_progress, 0.25);
    assert_eq!(runtime.experience_total, 91);
    assert_eq!(runtime.encode_to_vec(), expected);
}

#[test]
fn chunk_fixture_is_the_specified_v1_wire_record() {
    let expected = fixture(CHUNK_V1);
    let record = StorageRecord::decode(expected.as_slice()).unwrap();
    validate_record(&record).unwrap();

    assert_eq!(record.format_version, FORMAT_VERSION_V1);
    let Some(storage_record::Record::Chunk(chunk)) = record.record.as_ref() else {
        panic!("fixture must contain a chunk record");
    };
    assert_eq!((chunk.column_x, chunk.column_z), (-1, 2));
    assert_eq!(chunk.game_data_version, 4903);
    assert_eq!(chunk.sections.len(), 1);
    let section = &chunk.sections[0];
    assert_eq!(section.section_y, -4);
    assert_eq!(section.palette_bits, 4);
    assert_eq!(section.palette_state_ids, [0, 17, 300]);
    assert_eq!(section.block_state_indices, [1, 2, 3]);

    assert_eq!(record.encode_to_vec(), expected);
}

#[test]
fn typed_light_layers_preserve_missing_uniform_and_values_compactly() {
    let mut record = StorageRecord::decode(fixture(CHUNK_V1).as_slice()).unwrap();
    let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
        panic!("fixture must contain a chunk record");
    };
    let values = (0..2048).map(|index| (index as u8).wrapping_mul(3)).collect();
    chunk.light_sections = vec![
        LightSection {
            section_y: -5,
            sky_light: Some(LightData {
                data: Some(lodestone_storage_schema::generated::light_data::Data::Uniform(15)),
            }),
            block_light: Some(LightData {
                data: Some(lodestone_storage_schema::generated::light_data::Data::Values(values)),
            }),
        },
        // Both absent oneofs are the canonical Missing representation.
        LightSection {
            section_y: -4,
            sky_light: None,
            block_light: None,
        },
    ];
    validate_record(&record).unwrap();
    let encoded = record.encode_to_vec();
    assert!(encoded.len() < 2300, "uniform light must not expand to a second 2 KiB array");
    let decoded = StorageRecord::decode(encoded.as_slice()).unwrap();
    assert_eq!(decoded, record);

    {
        let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
            unreachable!();
        };
        let Some(light) = &mut chunk.light_sections[0].sky_light else {
            unreachable!();
        };
        light.data = Some(lodestone_storage_schema::generated::light_data::Data::Uniform(16));
    }
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::InvalidLightUniform(16))
    );

    {
        let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
            unreachable!();
        };
        chunk.light_sections[0].sky_light = Some(LightData {
            data: Some(
                lodestone_storage_schema::generated::light_data::Data::Values(vec![0; 2047]),
            ),
        });
    }
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::InvalidLightArrayLength(2047))
    );

    {
        let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
            unreachable!();
        };
        chunk.light_sections[0].sky_light = None;
        chunk.light_sections[1].section_y = -5;
    }
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::UnorderedLightSections {
            previous: -5,
            actual: -5,
        })
    );

    {
        let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
            unreachable!();
        };
        chunk.sections[0].sky_light = vec![0; 2048];
    }
    assert_eq!(validate_record(&record), Err(ValidationError::LegacyLightBytes));
}

#[test]
fn world_properties_fixture_is_the_specified_v1_wire_record() {
    let expected = fixture(WORLD_PROPERTIES_V1);
    let record = StorageRecord::decode(expected.as_slice()).unwrap();
    validate_record(&record).unwrap();

    let Some(storage_record::Record::General(general)) = record.record.as_ref() else {
        panic!("fixture must contain a general record");
    };
    let Some(general_record::Record::WorldProperties(world)) = general.record.as_ref() else {
        panic!("fixture must contain world properties");
    };
    assert_eq!(world.game_data_version, 4903);
    assert_eq!(world.seed, 123_456_789);
    assert_eq!(world.spawn_dimension, BuiltinDimension::Overworld as i32);
    assert_eq!((world.spawn_x, world.spawn_y, world.spawn_z), (-7, 80, 12));
    assert_eq!(world.day_time, 6000);
    assert_eq!(world.default_game_mode, GameMode::Unspecified as i32);

    assert_eq!(record.encode_to_vec(), expected);
}

#[test]
fn extension_fixture_resolves_a_compact_local_id() {
    let expected = fixture(EXTENSIONS_V1);
    let table = ExtensionTable::decode(expected.as_slice()).unwrap();
    validate_extension_table(&table).unwrap();
    assert_eq!(table.table_version, FORMAT_VERSION_V1);
    assert_eq!(table.extensions.len(), 1);
    assert_eq!(table.extensions[0].local_id, 7);
    assert_eq!(table.extensions[0].namespace, "example");
    assert_eq!(table.extensions[0].name, "weather");
    assert_eq!(table.extensions[0].schema_version, 2);
    assert_eq!(table.encode_to_vec(), expected);
}

#[test]
fn extension_values_must_name_a_registered_nonzero_id() {
    let table = ExtensionTable::decode(fixture(EXTENSIONS_V1).as_slice()).unwrap();
    let mut record = StorageRecord::decode(fixture(CHUNK_V1).as_slice()).unwrap();
    {
        let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
            unreachable!();
        };
        chunk.extensions.push(lodestone_storage_schema::ExtensionValue {
            local_id: 7,
            payload: vec![0xc0, 0xff, 0xee],
        });
    }
    validate_record_with_extensions(&record, &table).unwrap();

    let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
        unreachable!();
    };
    chunk.extensions[0].local_id = 8;
    assert_eq!(
        validate_record_with_extensions(&record, &table),
        Err(ValidationError::UnregisteredExtensionId(8))
    );
}

#[test]
fn player_locator_requires_a_complete_uuid_and_a_builtin_dimension() {
    let mut record = StorageRecord {
        format_version: FORMAT_VERSION_V1,
        record: Some(storage_record::Record::General(GeneralRecord {
            record: Some(general_record::Record::Player(PlayerRecord {
                player_uuid: vec![0x42; 16],
                dimension: BuiltinDimension::Nether as i32,
                x_fixed: -16_385,
                y_fixed: 256,
                z_fixed: 65_535,
                yaw_millidegrees: -90_001,
                pitch_millidegrees: 45_002,
                game_mode: GameMode::Creative as i32,
                runtime_state: Some(PlayerRuntimeState {
                    health: 17.5,
                    air_supply: 123,
                    experience_level: 7,
                    experience_progress: 0.25,
                    experience_total: 91,
                }),
                inventory: None,
            })),
            extensions: Vec::new(),
        })),
    };
    validate_record(&record).unwrap();

    set_player_uuid_and_dimension(
        &mut record,
        vec![0x42; 15],
        BuiltinDimension::Nether as i32,
    );
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::InvalidPlayerUuidLength(15))
    );

    set_player_uuid_and_dimension(&mut record, vec![0x42; 16], 77);
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::UnknownBuiltinDimension(77))
    );

    set_player_uuid_and_dimension(
        &mut record,
        vec![0x42; 16],
        BuiltinDimension::Nether as i32,
    );
    player_in(&mut record).game_mode = 99;
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::UnknownPlayerGameMode(99))
    );

    player_in(&mut record).game_mode = GameMode::Creative as i32;
    player_in(&mut record)
        .runtime_state
        .as_mut()
        .unwrap()
        .experience_progress = 1.0;
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::InvalidPlayerExperience)
    );
}

#[test]
fn resident_entity_requires_stable_identity_type_dimension_and_finite_pose() {
    let mut record = StorageRecord {
        format_version: FORMAT_VERSION_V1,
        record: Some(storage_record::Record::General(GeneralRecord {
            record: Some(general_record::Record::Entity(EntityRecord {
                entity_uuid: vec![0x71; 16],
                entity_type: "minecraft:cow".to_owned(),
                dimension: BuiltinDimension::Overworld as i32,
                x: -1.5,
                y: 64.0,
                z: 31.25,
                yaw: 135.5,
                pitch: -12.25,
                ..EntityRecord::default()
            })),
            extensions: Vec::new(),
        })),
    };
    validate_record(&record).unwrap();

    entity_in(&mut record).entity_uuid.pop();
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::InvalidEntityUuidLength(15))
    );
    entity_in(&mut record).entity_uuid.push(0x71);
    entity_in(&mut record).entity_type.clear();
    assert_eq!(validate_record(&record), Err(ValidationError::MissingEntityType));
    entity_in(&mut record).entity_type = "minecraft:cow".to_owned();
    entity_in(&mut record).x = f64::NAN;
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::NonFiniteEntityPosition)
    );
    entity_in(&mut record).x = -1.5;
    entity_in(&mut record).yaw = f32::INFINITY;
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::NonFiniteEntityRotation)
    );
}

fn entity_in(record: &mut StorageRecord) -> &mut EntityRecord {
    let Some(storage_record::Record::General(general)) = &mut record.record else {
        unreachable!();
    };
    let Some(general_record::Record::Entity(entity)) = &mut general.record else {
        unreachable!();
    };
    entity
}

fn player_in(record: &mut StorageRecord) -> &mut PlayerRecord {
    let Some(storage_record::Record::General(general)) = &mut record.record else {
        unreachable!();
    };
    let Some(general_record::Record::Player(player)) = &mut general.record else {
        unreachable!();
    };
    player
}

fn set_player_uuid_and_dimension(record: &mut StorageRecord, uuid: Vec<u8>, dimension: i32) {
    let player = player_in(record);
    player.player_uuid = uuid;
    player.dimension = dimension;
}

#[test]
fn invalid_palette_width_is_rejected_before_storage() {
    let mut record = StorageRecord::decode(fixture(CHUNK_V1).as_slice()).unwrap();
    let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
        unreachable!();
    };
    chunk.sections[0].palette_bits = 16;
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::InvalidPaletteBits(16))
    );
}

#[test]
fn scheduled_tick_enums_reject_unknown_or_unspecified_actions_before_storage() {
    let mut record = StorageRecord::decode(fixture(CHUNK_V1).as_slice()).unwrap();
    set_scheduled_tick(
        &mut record,
        ScheduledTickKind::Repeater as i32,
        ScheduledTickPriority::High as i32,
    );
    validate_record(&record).unwrap();

    set_scheduled_tick(&mut record, 99, ScheduledTickPriority::High as i32);
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::UnknownScheduledTickKind(99))
    );
    set_scheduled_tick(
        &mut record,
        ScheduledTickKind::Unspecified as i32,
        ScheduledTickPriority::High as i32,
    );
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::UnknownScheduledTickKind(0))
    );
    set_scheduled_tick(&mut record, ScheduledTickKind::Repeater as i32, 99);
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::UnknownScheduledTickPriority(99))
    );
}

fn set_scheduled_tick(record: &mut StorageRecord, kind: i32, priority: i32) {
    let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
        unreachable!();
    };
    chunk.block_scheduled_ticks = vec![ScheduledTick {
        x: -16,
        y: 70,
        z: 32,
        kind,
        trigger_tick: 123,
        priority,
        insertion_order: 9,
    }];
}

#[test]
fn complete_builtin_biome_grids_validate_and_an_unknown_value_does_not() {
    let mut record = StorageRecord::decode(fixture(CHUNK_V1).as_slice()).unwrap();
    {
        let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
            unreachable!();
        };
        chunk.biome_sections = vec![BiomeSection {
            section_y: -4,
            quart_rows: 4,
            biome_ids: vec![BuiltinBiome::Plains as i32; 64],
        }];
        chunk.surface_biome_ids = vec![BuiltinBiome::CherryGrove as i32; 16];
    }
    validate_record(&record).unwrap();

    let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
        unreachable!();
    };
    chunk.biome_sections[0].biome_ids[63] = BuiltinBiome::Unspecified as i32;
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::UnknownBuiltinBiome(
            BuiltinBiome::Unspecified as i32
        ))
    );
}

#[test]
fn motion_blocking_heightmap_is_optional_but_never_partial_or_wide() {
    let mut record = StorageRecord::decode(fixture(CHUNK_V1).as_slice()).unwrap();
    {
        let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
            unreachable!();
        };
        chunk.motion_blocking_heights = (0..256).map(|index| index as u32).collect();
    }
    validate_record(&record).unwrap();

    {
        let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
            unreachable!();
        };
        chunk.motion_blocking_heights.pop();
    }
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::InvalidMotionBlockingHeightCount(255))
    );

    {
        let Some(storage_record::Record::Chunk(chunk)) = &mut record.record else {
            unreachable!();
        };
        chunk.motion_blocking_heights = vec![0; 256];
        chunk.motion_blocking_heights[255] = u32::from(u16::MAX) + 1;
    }
    assert_eq!(
        validate_record(&record),
        Err(ValidationError::MotionBlockingHeightOutOfRange(
            u32::from(u16::MAX) + 1
        ))
    );
}

fn fixture(source: &str) -> Vec<u8> {
    source
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("fixture contains hexadecimal bytes"))
        .collect()
}
