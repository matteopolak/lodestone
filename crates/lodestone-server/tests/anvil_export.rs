#![cfg(not(target_arch = "wasm32"))]

//! Independent NBT-shape checks for native-to-Anvil chunk export.

use lodestone_core::Nbt;
use lodestone_server::{
    ChunkColumn, PersistedScheduledTick, TickPriority,
    anvil_export::{
        Error, ExportAuthorization, ExportLossDecision, TickQueue, UnsupportedNativeFeature,
        export_chunk, preflight_chunk,
    },
    world_storage::NativeChunkRecord,
};
use lodestone_world::{ColumnLight, Heightmap, LightData, NibbleArray};

fn field<'a>(compound: &'a Nbt, name: &str) -> &'a Nbt {
    let Nbt::Compound(fields) = compound else {
        panic!("expected compound while finding {name}");
    };
    fields
        .iter()
        .find(|(field, _)| field == name)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("missing field {name}"))
}

fn section(chunk: &Nbt, y: i8) -> &Nbt {
    let Nbt::List { elements, .. } = field(chunk, "sections") else {
        panic!("sections must be a list");
    };
    elements
        .iter()
        .find(|entry| matches!(field(entry, "Y"), Nbt::Byte(actual) if *actual == y))
        .unwrap_or_else(|| panic!("missing section Y={y}"))
}

fn record_with_typed_payload() -> NativeChunkRecord {
    let mut column = ChunkColumn::new(-64, 32);
    column.set_block(
        1,
        -63,
        2,
        "minecraft:oak_stairs[facing=west,half=top,shape=inner_left,waterlogged=false]",
    );
    column.set_biome_cell(2, 0, 1, "minecraft:desert");
    let mut motion = [0u16; 256];
    motion[Heightmap::index(1, 2)] = 17;
    column.set_motion_blocking(motion);

    let mut light = ColumnLight::new(2);
    *light.sky_mut(0) = LightData::Uniform(15);
    let mut values = NibbleArray::filled(0);
    values.set(NibbleArray::index(1, 2, 3), 7);
    *light.block_mut(1) = LightData::Values(values);
    *light.block_mut(3) = LightData::Uniform(2);

    NativeChunkRecord {
        column,
        light,
        block_scheduled_ticks: Vec::new(),
        fluid_scheduled_ticks: Vec::new(),
    }
}

#[test]
fn typed_native_fields_have_their_predicted_anvil_nbt_shape() {
    let record = record_with_typed_payload();
    let report = preflight_chunk(&record);
    assert!(report.unsupported().is_empty());
    let result = export_chunk(
        6,
        12,
        &record,
        100,
        Some(report.decide(ExportLossDecision::ProceedAndDiscardUnsupported)),
    )
    .expect("a record without native-only tick order exports losslessly");
    let chunk = result.chunk;

    assert_eq!(field(&chunk, "xPos"), &Nbt::Int(6));
    assert_eq!(field(&chunk, "zPos"), &Nbt::Int(12));
    assert_eq!(field(&chunk, "isLightOn"), &Nbt::Byte(1));

    let block_states = field(section(&chunk, -4), "block_states");
    let Nbt::List {
        elements: palette, ..
    } = field(block_states, "palette")
    else {
        panic!("block palette must be a list");
    };
    let stairs = palette
        .iter()
        .find(|entry| field(entry, "Name") == &Nbt::String("minecraft:oak_stairs".to_owned()))
        .expect("the typed block state must retain its identifier");
    let properties = field(stairs, "Properties");
    assert_eq!(field(properties, "facing"), &Nbt::String("west".to_owned()));
    assert_eq!(field(properties, "half"), &Nbt::String("top".to_owned()));
    assert_eq!(
        field(properties, "shape"),
        &Nbt::String("inner_left".to_owned())
    );
    assert_eq!(
        field(properties, "waterlogged"),
        &Nbt::String("false".to_owned())
    );

    let biomes = field(section(&chunk, -4), "biomes");
    let Nbt::List {
        elements: biome_palette,
        ..
    } = field(biomes, "palette")
    else {
        panic!("biome palette must be a list");
    };
    let desert = biome_palette
        .iter()
        .position(|entry| entry == &Nbt::String("minecraft:desert".to_owned()))
        .expect("the typed biome cell must retain its resource key");
    let Nbt::LongArray(biome_data) = field(biomes, "data") else {
        panic!("two biome entries require packed data");
    };
    // Cell (qx=2, qy=0, qz=1) has flat section index 6. With two palette
    // values Anvil uses one bit per cell, so bit 6 identifies that cell.
    assert_eq!(((biome_data[0] as u64 >> 6) & 1) as usize, desert);

    let Nbt::Compound(heightmaps) = field(&chunk, "Heightmaps") else {
        panic!("motion blocking must become a heightmap compound");
    };
    let Nbt::LongArray(motion) = heightmaps
        .iter()
        .find(|(name, _)| name == "MOTION_BLOCKING")
        .map(|(_, value)| value)
        .expect("motion blocking entry")
    else {
        panic!("motion blocking must be a packed long array");
    };
    // Height 32 requires six bits. XZ index 33 is entry 3 in long 3 because
    // ten six-bit entries fit in each non-spanning long.
    assert_eq!(motion[3], 17_i64 << 18);

    let Nbt::ByteArray(lower_sky) = field(section(&chunk, -5), "SkyLight") else {
        panic!("the lower boundary light section must be emitted");
    };
    assert_eq!(lower_sky.len(), 2048);
    assert!(lower_sky.iter().all(|value| *value == -1));
    let Nbt::ByteArray(block) = field(section(&chunk, -4), "BlockLight") else {
        panic!("varied in-range block light must be emitted");
    };
    assert_eq!(block.len(), 2048);
    // YZX index (1,2,3) is 561, which is the high nibble of byte 280.
    assert_eq!(block[280], 0x70);
    let Nbt::ByteArray(upper_block) = field(section(&chunk, -2), "BlockLight") else {
        panic!("the upper boundary light section must be emitted");
    };
    assert!(upper_block.iter().all(|value| *value == 0x22));
}

#[test]
fn native_tick_sequence_loss_is_reported_and_requires_matching_authorization() {
    let mut record = record_with_typed_payload();
    record.block_scheduled_ticks.push(PersistedScheduledTick {
        pos: (99, -60, 199),
        kind: "minecraft:oak_leaves".to_owned(),
        trigger_tick: 105,
        priority: TickPriority::High,
        insertion_order: 700,
    });
    let report = preflight_chunk(&record);
    assert_eq!(
        report.unsupported(),
        &[UnsupportedNativeFeature::TickInsertionOrder {
            queue: TickQueue::Block,
            ticks: 1,
        }]
    );
    assert!(matches!(
        export_chunk(6, 12, &record, 100, None),
        Err(Error::MissingAuthorization)
    ));
    assert!(matches!(
        export_chunk(6, 12, &record, 100, Some(ExportAuthorization::Lossless),),
        Err(Error::AuthorizationMismatch { .. })
    ));

    let result = export_chunk(
        6,
        12,
        &record,
        100,
        Some(report.decide(ExportLossDecision::ProceedAndDiscardUnsupported)),
    )
    .expect("the acknowledged sequence loss still exports the representable tick fields");
    let Nbt::List {
        elements: ticks, ..
    } = field(&result.chunk, "block_ticks")
    else {
        panic!("block ticks must be a list");
    };
    assert_eq!(ticks.len(), 1);
    assert_eq!(
        field(&ticks[0], "i"),
        &Nbt::String("minecraft:oak_leaves".to_owned())
    );
    assert_eq!(field(&ticks[0], "t"), &Nbt::Int(5));
    assert_eq!(field(&ticks[0], "p"), &Nbt::Int(-1));
    let Nbt::Compound(tick_fields) = &ticks[0] else {
        panic!("saved tick must be a compound");
    };
    assert!(
        tick_fields
            .iter()
            .all(|(name, _)| name != "insertion_order"),
        "the native-only sequence is reported before it is omitted"
    );
}

#[test]
fn impossible_native_tick_delay_refuses_the_whole_export() {
    let mut record = record_with_typed_payload();
    record.fluid_scheduled_ticks.push(PersistedScheduledTick {
        pos: (99, -60, 199),
        kind: "minecraft:water".to_owned(),
        trigger_tick: u64::MAX,
        priority: TickPriority::Normal,
        insertion_order: 701,
    });
    let report = preflight_chunk(&record);
    let authorization = report.decide(ExportLossDecision::ProceedAndDiscardUnsupported);
    assert!(matches!(
        export_chunk(6, 12, &record, 100, Some(authorization)),
        Err(Error::TickDelayOutOfRange {
            queue: TickQueue::Fluid,
            index: 0,
            ..
        })
    ));
}
