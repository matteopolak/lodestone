//! Hermetic framing tests for the `level_chunk_with_light` decoder.
//!
//! These build a synthetic packet with the exact 26.2 wire framing (two shorts
//! per section, block container before biome container, `FixedSize` long
//! arrays, typed-list heightmaps) and round-trip it through
//! [`LevelChunkWithLight::decode`]. They guard the version-specific *glue*
//! without needing a live server; the live test proves it against real output.

use lodestone_core::{Reader, Writer};
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_world::{ColumnLight, Heightmaps, PalettedContainer};

/// Builds the length-prefixed section blob for a column whose bottom section
/// carries the given 4096 block ids and whose other sections are pure air.
fn encode_packet(x: i32, z: i32, shape: &ChunkShape, bottom_blocks: &[u32]) -> Vec<u8> {
    let mut w = Writer::default();
    w.i32(x);
    w.i32(z);

    // Heightmaps: empty typed-list is a valid (count = 0) payload.
    Heightmaps::new().encode(&mut w);

    // Section blob, length-prefixed.
    let mut blob = Writer::default();
    for index in 0..shape.section_count {
        let block_container = if index == 0 {
            PalettedContainer::from_values(shape.block_kind, bottom_blocks)
        } else {
            PalettedContainer::new(shape.block_kind, shape.air_id)
        };
        let non_air = (0..block_container.entry_count())
            .filter(|&i| block_container.get(i) != shape.air_id)
            .count() as i16;
        w_short_pair(&mut blob, non_air, 0);
        block_container.encode(&mut blob);
        PalettedContainer::new(shape.biome_kind, shape.biome_id).encode(&mut blob);
    }
    let blob = blob.into_vec();
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);

    // Block entities: empty list.
    w.var_i32(0);

    // Light: an all-absent column is a valid payload (four empty masks, two
    // empty update lists).
    ColumnLight::new(shape.section_count).encode(&mut w);

    w.into_vec()
}

fn w_short_pair(w: &mut Writer, a: i16, b: i16) {
    w.i16(a);
    w.i16(b);
}

#[test]
fn round_trips_and_leaves_zero_trailing_bytes() {
    let shape = ChunkShape::overworld_1_21();

    // Bottom section: solid layer of id 1 at local y=0, a marker of id 7 at
    // (x=1, y=1, z=2) to force an indirect palette, air elsewhere.
    let mut blocks = vec![0u32; 4096];
    for slot in blocks.iter_mut().take(256) {
        *slot = 1; // y=0 plane (YZX: y*256 + z*16 + x)
    }
    let marker = 256 + 2 * 16 + 1; // YZX: y=1, z=2, x=1
    blocks[marker] = 7;

    let bytes = encode_packet(3, -5, &shape, &blocks);
    let mut r = Reader::new(&bytes);
    let chunk = LevelChunkWithLight::decode(&mut r, &shape).expect("decodes");
    r.ensure_empty().expect("zero trailing bytes");

    assert_eq!(chunk.x, 3);
    assert_eq!(chunk.z, -5);

    // Solid layer landed at world y = -64 (min_y), for every (x, z).
    for x in 0..16 {
        for z in 0..16 {
            assert_eq!(chunk.column.get_block(x, -64, z), 1, "solid layer at y=-64");
        }
    }

    // YZX pinned at the version boundary: the marker at (x=1, y=1, z=2) maps to
    // world y = -63. A transposed decoder would place it elsewhere.
    assert_eq!(chunk.column.get_block(1, -63, 2), 7, "marker at (1,-63,2)");
    assert_eq!(
        chunk.column.get_block(0, -63, 0),
        0,
        "air beside the marker"
    );

    // High up is air, and that section is elided (single-value / absent).
    assert_eq!(
        chunk.column.get_block(0, 100, 0),
        0,
        "air far above terrain"
    );

    // Bottom section's block container is indirect (palette {0, 1, 7}).
    let bottom = chunk.column.section(0).expect("bottom section present");
    assert_eq!(bottom.block_states().palette_len(), 3, "indirect palette");
    assert!(!bottom.block_states().is_single());
}

#[test]
fn truncated_blob_errors_rather_than_panics() {
    let shape = ChunkShape::overworld_1_21();
    let blocks = vec![0u32; 4096];
    let mut bytes = encode_packet(0, 0, &shape, &blocks);
    // Chop off the light payload and part of the blob so decoding must fail
    // cleanly rather than panic or over-read.
    bytes.truncate(bytes.len() / 2);
    let mut r = Reader::new(&bytes);
    assert!(LevelChunkWithLight::decode(&mut r, &shape).is_err());
}
