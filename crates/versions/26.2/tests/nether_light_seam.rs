//! Focused initial Nether block-light lifecycle controls.

use lodestone_core::Reader;
use lodestone_server::{ChunkColumn, ServerDirective, ServerProtocol};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_server::dimension::Dimension;

const FIXTURE: &str = include_str!("fixtures/nether_initial_light_seam.txt");

fn fixture_value(key: &str) -> u8 {
    FIXTURE
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("missing {key} in Nether seam fixture"))
        .parse()
        .unwrap_or_else(|error| panic!("invalid {key} in Nether seam fixture: {error}"))
}

fn initial_packet(
    center: &ChunkColumn,
    neighbours: &[(i32, i32, ChunkColumn)],
) -> LevelChunkWithLight {
    let shape = ChunkShape::nether_or_end_1_21();
    let ServerDirective::Send { payload, .. } = V770ServerProtocol
        .try_encode_chunk_with_neighbours_in_dimension(
            0,
            0,
            center,
            neighbours,
            Dimension::Nether,
        )
        .expect("Nether initial chunk encoding")
    else {
        panic!("Nether initial chunk encoding must send a packet");
    };
    let mut reader = Reader::new(&payload);
    let packet = LevelChunkWithLight::decode(&mut reader, &shape)
        .expect("Nether initial packet must decode");
    reader.ensure_empty().expect("Nether packet has no trailing bytes");
    packet
}

#[test]
fn neighbour_emitter_is_deferred_but_center_emitter_is_retained() {
    let shape = ChunkShape::nether_or_end_1_21();
    let mut center = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    let mut north = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    north.set_block(8, 64, 0, "minecraft:glowstone");

    let deferred = initial_packet(&center, &[(0, 1, north)]);
    assert_eq!(
        deferred.light.section_light(5).block_at(8, 0, 15),
        fixture_value("neighbor_initial"),
        "a north-neighbour emitter must not cross the initial Nether seam"
    );

    center.set_block(8, 64, 15, "minecraft:glowstone");
    let retained = initial_packet(&center, &[]);
    assert_eq!(
        retained.light.section_light(5).block_at(8, 0, 15),
        fixture_value("center_initial"),
        "a center-column emitter must remain in the initial Nether packet"
    );
}

#[test]
fn wrong_direction_neighbor_does_not_change_the_north_seam_control() {
    let shape = ChunkShape::nether_or_end_1_21();
    let center = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    let mut east = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    east.set_block(0, 64, 8, "minecraft:glowstone");

    let packet = initial_packet(&center, &[(1, 0, east)]);
    assert_eq!(
        packet.light.section_light(5).block_at(8, 0, 15),
        fixture_value("neighbor_initial"),
        "an east-neighbour emitter is the wrong-direction control for the north seam"
    );
}
