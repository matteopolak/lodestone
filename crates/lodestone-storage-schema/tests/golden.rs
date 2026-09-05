use lodestone_storage_schema::generated::{general_record, storage_record};
use lodestone_storage_schema::{
    validate_extension_table, validate_record, validate_record_with_extensions, BuiltinDimension,
    ExtensionTable, FORMAT_VERSION_V1, GameMode, StorageRecord, ValidationError,
};
use prost::Message;

const CHUNK_V1: &str = include_str!("fixtures/chunk-v1.hex");
const WORLD_PROPERTIES_V1: &str = include_str!("fixtures/world-properties-v1.hex");
const EXTENSIONS_V1: &str = include_str!("fixtures/extensions-v1.hex");

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

fn fixture(source: &str) -> Vec<u8> {
    source
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("fixture contains hexadecimal bytes"))
        .collect()
}
