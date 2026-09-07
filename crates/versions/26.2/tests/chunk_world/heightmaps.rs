//! Hermetic heightmap parity checks for the protocol-776 chunk encoder.
//!
//! The client-visible collection has exactly three numeric keys (world
//! surface, motion blocking, and motion blocking without leaves). Each value
//! is the first free block row relative to the dimension's minimum Y, packed
//! as 9-bit, non-straddling entries. This test exercises that contract through
//! the public encoder in all three supported dimensions.
//!
//! The raw-long assertions are deliberately independent of the heightmap
//! decoder: a decoder/encoder round trip could preserve the same mistaken bit
//! orientation. The decoded assertions then check that the same wire values
//! are exposed through the client packet type.

use lodestone_core::Reader;
use lodestone_server::dimension::Dimension;
use lodestone_server::{ChunkColumn, ServerDirective, ServerProtocol};
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_v26_2::V770ServerProtocol;

const WORLD_SURFACE: i32 = 1;
const MOTION_BLOCKING: i32 = 4;
const MOTION_BLOCKING_NO_LEAVES: i32 = 5;
const HEIGHTMAP_LONGS: usize = 37;

fn shape_for(dimension: Dimension) -> ChunkShape {
    match dimension {
        Dimension::Overworld => ChunkShape::overworld_1_21(),
        Dimension::Nether | Dimension::End => ChunkShape::nether_or_end_1_21(),
    }
}

/// Creates three deliberately distinct XZ entries:
///
/// * `(0, 0)` has stone, storing 8;
/// * `(6, 0)` has a leaf at relative height 63, storing 64 only for maps that
///   include leaves;
/// * `(7, 0)` has water at relative height 31, storing 32 for every map that
///   sees the fluid.
///
/// Entries six and seven straddle the first 9-bit packed-long boundary, which
/// catches both the entry width and the low-bit-first layout.
fn fixture_column(dimension: Dimension) -> ChunkColumn {
    let shape = shape_for(dimension);
    let mut column = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    column.set_block(0, shape.min_y + 7, 0, "minecraft:stone");
    column.set_block(
        6,
        shape.min_y + 63,
        0,
        "minecraft:oak_leaves[persistent=true,distance=7,waterlogged=false]",
    );
    column.set_block(7, shape.min_y + 31, 0, "minecraft:water[level=0]");
    column
}

fn encode(dimension: Dimension) -> Vec<u8> {
    let column = fixture_column(dimension);
    let ServerDirective::Send { payload, .. } = ServerProtocol::try_encode_chunk_in_dimension(
        &V770ServerProtocol,
        0,
        0,
        &column,
        dimension,
    )
    .expect("heightmap fixture encodes")
    else {
        panic!("heightmap fixture must produce a chunk packet");
    };
    payload
}

fn expected_longs(type_id: i32) -> [u64; HEIGHTMAP_LONGS] {
    let mut expected = [0; HEIGHTMAP_LONGS];
    // Entry 0 occupies bits 0..=8; entry 6 occupies bits 54..=62.
    expected[0] = 8;
    if type_id != MOTION_BLOCKING_NO_LEAVES {
        expected[0] |= 64 << 54;
    }
    // Entry 7 starts the next long because entries never straddle a long.
    expected[1] = 32;
    expected
}

#[test]
fn client_heightmaps_keep_keys_predicates_min_y_and_packing_in_each_dimension() {
    for dimension in Dimension::ALL {
        let shape = shape_for(dimension);
        let payload = encode(dimension);

        // Read only the map prefix by hand. The remainder is the section,
        // block-entity, and light payload and is intentionally left to the
        // packet decoder below.
        let mut raw = Reader::new(&payload);
        assert_eq!(raw.i32().expect("chunk x"), 0);
        assert_eq!(raw.i32().expect("chunk z"), 0);
        assert_eq!(raw.var_i32().expect("heightmap count"), 3);

        let mut entries = Vec::new();
        for _ in 0..3 {
            let type_id = raw.var_i32().expect("heightmap key");
            assert_eq!(raw.var_i32().expect("packed-long count"), HEIGHTMAP_LONGS as i32);
            let longs = (0..HEIGHTMAP_LONGS)
                .map(|_| raw.i64().expect("packed heightmap long") as u64)
                .collect::<Vec<_>>();
            entries.push((type_id, longs));
        }
        entries.sort_unstable_by_key(|(type_id, _)| *type_id);
        assert_eq!(
            entries.iter().map(|(type_id, _)| *type_id).collect::<Vec<_>>(),
            [WORLD_SURFACE, MOTION_BLOCKING, MOTION_BLOCKING_NO_LEAVES],
            "client heightmap keys for {dimension:?}"
        );
        for (type_id, longs) in entries {
            assert_eq!(
                longs,
                expected_longs(type_id).to_vec(),
                "packed {type_id} heightmap for {dimension:?}"
            );
        }

        let mut decoded_reader = Reader::new(&payload);
        let packet = LevelChunkWithLight::decode(&mut decoded_reader, &shape)
            .expect("chunk packet decodes");
        decoded_reader
            .ensure_empty()
            .expect("chunk packet has no trailing bytes");

        assert_eq!(packet.heightmaps.len(), 3);
        assert_eq!(
            packet
                .heightmaps
                .get(WORLD_SURFACE as u32)
                .expect("world surface map")
                .get(0, 0),
            8
        );
        assert_eq!(
            packet
                .heightmaps
                .get(WORLD_SURFACE as u32)
                .expect("world surface map")
                .get(6, 0),
            64
        );
        assert_eq!(
            packet
                .heightmaps
                .get(MOTION_BLOCKING as u32)
                .expect("motion blocking map")
                .get(6, 0),
            64
        );
        assert_eq!(
            packet
                .heightmaps
                .get(MOTION_BLOCKING_NO_LEAVES as u32)
                .expect("no-leaves map")
                .get(6, 0),
            0
        );
        for type_id in [
            WORLD_SURFACE as u32,
            MOTION_BLOCKING as u32,
            MOTION_BLOCKING_NO_LEAVES as u32,
        ] {
            assert_eq!(
                packet.heightmaps.get(type_id).expect("heightmap").get(7, 0),
                32,
                "fluid row for map {type_id} in {dimension:?}"
            );
        }
    }
}
