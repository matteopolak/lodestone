//! Coordinate-free controls for the initial Overworld light lifecycle.
//!
//! An initial chunk can either carry a retained settlement snapshot or use the
//! protocol's generated fallback. Those are deliberately different wire
//! representations: the retained branch is authoritative, while the fallback
//! keeps one redundant full-sky section and omits unallocated zero block-light
//! sections. The controls below inspect the raw light suffix as well as the
//! decoded values so a local decode/encode cycle cannot hide a mask mistake.

use lodestone_core::Reader;
use lodestone_server::dimension::Dimension;
use lodestone_server::{ChunkColumn, ServerDirective, ServerProtocol};
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_world::{ColumnLight, Heightmaps, LightData, NibbleArray};

const TERRAIN_CELLS: usize = 29;
const PACKET_X: i32 = 3;
const PACKET_Z: i32 = -5;

fn terrain_column() -> ChunkColumn {
    let mut column = ChunkColumn::new(-64, 384);
    // Keep the terrain small but non-empty. The exact count is a guard against
    // comparing two all-air prefixes while the light suffix is under test.
    for index in 0..TERRAIN_CELLS {
        let x = (index % 8) as i32;
        let z = (index / 8) as i32;
        column.set_block(x, 0, z, "minecraft:stone");
    }
    column
}

fn retained_snapshot(section_count: usize) -> ColumnLight {
    let mut light = ColumnLight::new(section_count);

    // A varied section proves that the retained path keeps real array bytes,
    // while the full section proves that a high redundant sky section is not
    // reconstructed or trimmed after settlement.
    let mut varied = NibbleArray::filled(15);
    varied.set(0, 3);
    *light.sky_mut(5) = LightData::Values(varied);
    *light.sky_mut(14) = LightData::Uniform(15);

    // Uniform zero is an explicit empty-mask value. Missing remains present in
    // the rest of the column, so the two storage tags are distinguishable.
    *light.block_mut(14) = LightData::Uniform(0);
    light
}

fn encode_initial(column: &ChunkColumn) -> Vec<u8> {
    let directive = V770ServerProtocol
        .try_encode_chunk_with_neighbours_in_dimension(
            PACKET_X,
            PACKET_Z,
            column,
            &[],
            Dimension::Overworld,
        )
        .expect("Overworld initial chunk encoding");
    let ServerDirective::Send { payload, .. } = directive else {
        panic!("Overworld initial chunk encoding must send a packet");
    };
    payload
}

fn decode_packet(payload: &[u8]) -> LevelChunkWithLight {
    let shape = ChunkShape::overworld_1_21();
    let mut reader = Reader::new(payload);
    let packet = LevelChunkWithLight::decode(&mut reader, &shape)
        .expect("Overworld initial packet must decode");
    reader
        .ensure_empty()
        .expect("Overworld packet has no trailing bytes");
    packet
}

/// Returns the byte offset and exact light suffix after independently parsing
/// every non-light field. Comparing prefixes proves the controls use the same
/// 29 terrain cells; any remaining difference is the light lifecycle itself.
fn light_suffix(payload: &[u8]) -> (usize, &[u8]) {
    let shape = ChunkShape::overworld_1_21();
    let mut reader = Reader::new(payload);
    assert_eq!(reader.i32().expect("chunk x"), PACKET_X);
    assert_eq!(reader.i32().expect("chunk z"), PACKET_Z);
    Heightmaps::decode(shape.world_height, &mut reader).expect("heightmaps");
    let section_bytes = reader
        .var_i32()
        .expect("section blob length")
        .try_into()
        .expect("section blob length fits usize");
    reader.bytes(section_bytes).expect("section blob");
    lodestone_world::BlockEntity::decode_list(&mut reader).expect("block entities");
    let offset = reader.position();
    (offset, reader.remaining_bytes())
}

fn read_mask(reader: &mut Reader<'_>) -> Vec<usize> {
    let words = reader.var_i32().expect("light mask word count");
    assert!(words >= 0 && words <= 1, "26 light sections fit one mask word");
    let word = if words == 0 {
        0
    } else {
        reader.u64().expect("light mask word")
    };
    (0..26)
        .filter(|bit| word & (1_u64 << bit) != 0)
        .collect()
}

fn read_array(reader: &mut Reader<'_>) -> Vec<u8> {
    let length = reader
        .var_i32()
        .expect("light array length")
        .try_into()
        .expect("light array length fits usize");
    assert_eq!(length, 2048, "light arrays are exactly 2048 bytes");
    reader.bytes(length).expect("light array bytes").to_vec()
}

fn read_arrays(reader: &mut Reader<'_>) -> Vec<Vec<u8>> {
    let count = reader.var_i32().expect("light array count");
    assert!(count >= 0 && count <= 26, "one array per light section");
    (0..count).map(|_| read_array(reader)).collect()
}

fn non_air_cells(packet: &LevelChunkWithLight) -> usize {
    (0..packet.column.section_count())
        .map(|index| {
            packet
                .column
                .section(index)
                .map_or(0, |section| usize::from(section.non_air_count()))
        })
        .sum()
}

#[test]
fn retained_initial_light_round_trips_with_exact_packet_masks() {
    let shape = ChunkShape::overworld_1_21();
    let mut column = terrain_column();
    let snapshot = retained_snapshot(shape.section_count);
    column.set_retained_light(snapshot.clone());

    let payload = encode_initial(&column);
    let packet = decode_packet(&payload);
    assert_eq!(packet.light, snapshot, "retained light must be consumed verbatim");
    assert_eq!(non_air_cells(&packet), TERRAIN_CELLS);

    // The raw suffix is an independent packet-level assertion. The two sky
    // arrays are section 5's varied bytes and section 14's full-bright bytes;
    // block section 14 is explicit zero through the empty mask.
    let (_, suffix) = light_suffix(&payload);
    let mut reader = Reader::new(suffix);
    assert_eq!(read_mask(&mut reader), vec![5, 14], "sky present mask");
    assert!(read_mask(&mut reader).is_empty(), "block present mask");
    assert!(read_mask(&mut reader).is_empty(), "sky empty mask");
    assert_eq!(read_mask(&mut reader), vec![14], "block empty mask");
    let arrays = read_arrays(&mut reader);
    assert_eq!(arrays.len(), 2);
    assert_eq!(arrays[0][0], 0xf3, "varied sky nibble is on the wire");
    assert!(arrays[0][1..].iter().all(|&byte| byte == 0xff));
    assert!(arrays[1].iter().all(|&byte| byte == 0xff));
    assert!(read_arrays(&mut reader).is_empty(), "no block array for empty zero");
    reader
        .ensure_empty()
        .expect("exact light suffix has no trailing bytes");
}

#[test]
fn generated_initial_light_uses_compact_masks_without_changing_terrain() {
    let column = terrain_column();
    assert!(column.retained_light().is_none(), "generated control must be unsettled");

    let payload = encode_initial(&column);
    let packet = decode_packet(&payload);
    assert_eq!(non_air_cells(&packet), TERRAIN_CELLS);

    let (_, suffix) = light_suffix(&payload);
    let mut reader = Reader::new(suffix);
    let sky_present = read_mask(&mut reader);
    assert!(sky_present.contains(&6), "fallback keeps one full-sky section above terrain");
    assert!(!sky_present.contains(&14), "fallback omits the high redundant sky section");
    assert!(read_mask(&mut reader).is_empty(), "generated block present mask");
    assert!(read_mask(&mut reader).is_empty(), "generated sky empty mask");
    assert!(read_mask(&mut reader).is_empty(), "generated zero block sections are elided");

    let sky_arrays = read_arrays(&mut reader);
    assert!(!sky_arrays.is_empty(), "terrain must produce non-vacuous sky bytes");
    assert!(read_arrays(&mut reader).is_empty(), "no block arrays without emitters");
    reader
        .ensure_empty()
        .expect("exact generated light suffix has no trailing bytes");
}

#[test]
fn retained_and_generated_controls_share_the_same_terrain_prefix() {
    let shape = ChunkShape::overworld_1_21();
    let generated_payload = encode_initial(&terrain_column());
    let mut retained_column = terrain_column();
    retained_column.set_retained_light(retained_snapshot(shape.section_count));
    let retained_payload = encode_initial(&retained_column);

    let (generated_offset, generated_suffix) = light_suffix(&generated_payload);
    let (retained_offset, retained_suffix) = light_suffix(&retained_payload);
    assert_eq!(generated_offset, retained_offset);
    assert_eq!(
        &generated_payload[..generated_offset],
        &retained_payload[..retained_offset],
        "29 terrain cells and all non-light packet fields must be identical"
    );
    assert_ne!(generated_suffix, retained_suffix, "the lifecycle branches must be observable on the wire");
}
