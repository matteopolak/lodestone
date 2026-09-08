//! Initial Nether light admits cardinal source columns but defers diagonals.

use lodestone_server::dimension::Dimension;
use lodestone_server::{ChunkColumn, ServerProtocol};
use lodestone_v26_2::packets::chunk::ChunkShape;
use lodestone_v26_2::V770ServerProtocol;

fn block_at(light: &lodestone_world::ColumnLight, y: i32, x: usize, z: usize) -> u8 {
    let section = ((y + 16) / 16) as usize;
    let local_y = (y + 16).rem_euclid(16) as usize;
    light.section_light(section).block_at(x, local_y, z)
}

#[test]
fn initial_nether_admits_cardinal_sources_but_defers_diagonal_sources() {
    let shape = ChunkShape::nether_or_end_1_21();
    let center = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    let proto = V770ServerProtocol;

    let mut west = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    west.set_block(15, 82, 13, "minecraft:glowstone");
    let with_west = proto
        .compute_initial_column_light_with_neighbours_in_dimension(
            &center,
            &[(-1, 0, west)],
            Dimension::Nether,
        )
        .expect("initial Nether light with cardinal source");

    let without_west = proto
        .compute_initial_column_light_with_neighbours_in_dimension(
            &center,
            &[],
            Dimension::Nether,
        )
        .expect("initial Nether light without cardinal source");
    assert_eq!(block_at(&without_west, 82, 0, 13), 0);
    assert_eq!(block_at(&with_west, 82, 0, 13), 14);

    let mut north_west = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    north_west.set_block(15, 82, 15, "minecraft:glowstone");
    let with_diagonal = proto
        .compute_initial_column_light_with_neighbours_in_dimension(
            &center,
            &[(-1, -1, north_west)],
            Dimension::Nether,
        )
        .expect("initial Nether light with diagonal source");
    assert_eq!(block_at(&with_diagonal, 82, 0, 0), 0);
}
