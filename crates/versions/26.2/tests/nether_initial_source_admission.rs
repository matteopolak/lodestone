//! Initial Nether light keeps its admission boundary explicit.

use lodestone_server::dimension::Dimension;
use lodestone_server::{
    retained_chunk_source_for_view_radius, ChunkColumn, ChunkSource, RetainedLightStatus,
    ServerProtocol,
};
use lodestone_v26_2::packets::chunk::ChunkShape;
use lodestone_v26_2::V770ServerProtocol;
use lodestone_world::{ColumnLight, LightData, LightStorage, NibbleArray};

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

#[test]
fn retained_diagonal_light_crosses_only_admitted_bridges() {
    let shape = ChunkShape::nether_or_end_1_21();
    let center = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    let north_west = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    let north = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    let proto = V770ServerProtocol;
    let with_bridge = proto.compute_initial_column_light_with_neighbours_seeded(
        &center,
        &[(-1, -1, north_west), (0, -1, north)],
        Dimension::Nether,
        |dx, dz, x, y, z| {
            if (dx, dz, x, y, z) == (-1, -1, 15, 82, 15) {
                15
            } else {
                0
            }
        },
    );
    assert_eq!(
        block_at(&with_bridge, 82, 0, 0),
        13,
        "retained light must cross only the columns admitted to this snapshot"
    );

    let without_bridge = proto.compute_initial_column_light_with_neighbours_seeded(
        &center,
        &[(-1, -1, ChunkColumn::new(shape.min_y, shape.world_height as i32))],
        Dimension::Nether,
        |dx, dz, x, y, z| {
            if (dx, dz, x, y, z) == (-1, -1, 15, 82, 15) {
                15
            } else {
                0
            }
        },
    );
    assert_eq!(
        block_at(&without_bridge, 82, 0, 0),
        0,
        "a missing bridge must remain an opaque seam"
    );
}

#[test]
fn plural_admission_commits_center_and_dependency_readiness_atomically() {
    let source = retained_chunk_source_for_view_radius(lodestone_server::nether_chunk_source(42), 8);
    let center = source.column(0, 0);
    let protocol = V770ServerProtocol;
    let mut compute = |centre: &ChunkColumn,
                       neighbours: &[(i32, i32, ChunkColumn)]| {
        protocol.compute_initial_column_lights_with_neighbours_in_dimension(
            centre,
            neighbours,
            Dimension::Nether,
        )
    };
    source
        .settle_resident_column_lights_with_neighbours(
            0,
            0,
            &center,
            &[(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)],
            false,
            false,
            true,
            &mut compute,
        )
        .expect("production plural light admission");

    for &(dx, dz) in &[
        (0, 0),
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ] {
        let column = source
            .resident_column(dx, dz)
            .expect("atomic admission retains every footprint column");
        let expected = if (dx, dz) == (0, 0) {
            RetainedLightStatus::CentreSettled
        } else {
            RetainedLightStatus::DependencyInitialized
        };
        assert_eq!(
            column.retained_light_status(),
            Some(expected),
            "admitted column ({dx},{dz}) must carry its typed lifecycle stage even when zero-valued"
        );
    }

    let dependency = source
        .resident_column(1, 0)
        .expect("the east dependency remains resident for its own admission");
    let mut centre_admission_calls = 0;
    let mut centre_compute = |centre: &ChunkColumn,
                              neighbours: &[(i32, i32, ChunkColumn)]| {
        centre_admission_calls += 1;
        protocol.compute_initial_column_lights_with_neighbours_in_dimension(
            centre,
            neighbours,
            Dimension::Nether,
        )
    };
    let settled = source
        .settle_resident_column_lights_with_neighbours(
            1,
            0,
            &dependency,
            &[
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ],
            false,
            false,
            true,
            &mut centre_compute,
        )
        .expect("a populated dependency must run its own centre admission");
    assert_eq!(centre_admission_calls, 1);
    assert_eq!(
        settled.retained_light_status(),
        Some(RetainedLightStatus::CentreSettled)
    );

    let mut skipped_calls = 0;
    let mut skipped_compute = |centre: &ChunkColumn,
                               neighbours: &[(i32, i32, ChunkColumn)]| {
        skipped_calls += 1;
        protocol.compute_initial_column_lights_with_neighbours_in_dimension(
            centre,
            neighbours,
            Dimension::Nether,
        )
    };
    source
        .settle_resident_column_lights_with_neighbours(
            1,
            0,
            &settled,
            &[
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ],
            false,
            false,
            true,
            &mut skipped_compute,
        )
        .expect("a centre-settled snapshot must use the fast path");
    assert_eq!(skipped_calls, 0);
}

#[test]
fn dependency_centre_admission_promotes_exact_snapshot_and_initializes_new_dependency() {
    let shape = ChunkShape::nether_or_end_1_21();
    let section_count = shape.section_count;
    let light_section_count = section_count + 2;
    let source = retained_chunk_source_for_view_radius(lodestone_server::nether_chunk_source(42), 8);
    let mut retained = ColumnLight::new(section_count);
    let mut block_values = NibbleArray::filled(11);
    block_values.set(NibbleArray::index(3, 5, 7), 2);
    *retained.sky_mut(1) = LightData::Uniform(4);
    *retained.block_mut(6) = LightData::Values(block_values);
    let mut allocated = vec![false; light_section_count];
    allocated[1] = true;
    allocated[6] = true;
    let mut light_and_data = vec![false; light_section_count];
    light_and_data[6] = true;
    let storage = LightStorage::from_masks(allocated, light_and_data);

    let mut centre = source.column(0, 0);
    centre.set_block(8, 82, 8, "minecraft:netherrack");
    retained.set_storage(storage.clone());
    centre.set_retained_light_with_status(
        retained.clone(),
        RetainedLightStatus::DependencyInitialized,
    );
    assert!(source.store_resident_column(0, 0, &centre));

    let mut new_dependency = source.column(1, 0);
    new_dependency.set_block(0, 82, 0, "minecraft:glowstone");
    assert!(source.store_resident_column(1, 0, &new_dependency));
    assert!(new_dependency.retained_light().is_none());

    let proto = V770ServerProtocol;
    let fresh_centre = {
        let mut column = centre.clone();
        column.clear_retained_light();
        proto
            .compute_initial_column_light_with_neighbours_in_dimension(
                &column,
                &[(1, 0, new_dependency.clone())],
                Dimension::Nether,
            )
            .expect("fresh Nether centre computation")
    };
    assert_ne!(
        fresh_centre, retained,
        "the retained fixture must differ from a fresh computation"
    );

    let settled = source
        .settle_resident_column_lights_with_neighbours(
            0,
            0,
            &centre,
            &[(1, 0)],
            false,
            false,
            true,
            &mut |centre, neighbours| {
                proto.compute_initial_column_lights_with_neighbours_in_dimension(
                    centre,
                    neighbours,
                    Dimension::Nether,
                )
            },
        )
        .expect("Nether dependency centre admission");
    assert_eq!(settled.retained_light(), Some(&retained));
    assert_eq!(settled.retained_light_status(), Some(RetainedLightStatus::CentreSettled));
    assert_eq!(settled.retained_light().and_then(|light| light.storage()), Some(&storage));

    let dependency = source
        .resident_column(1, 0)
        .expect("new dependency remains resident");
    let dependency_storage = dependency
        .retained_light()
        .and_then(|light| light.storage())
        .expect("new dependency receives allocated light storage");
    assert_eq!(dependency_storage.section_count(), light_section_count);
    assert!(
        (0..dependency_storage.section_count())
            .any(|section| dependency_storage.is_allocated(section)),
        "the emitter dependency must retain at least one allocated section"
    );
    assert!(
        dependency
            .retained_light()
            .is_some_and(ColumnLight::has_nonzero_values),
        "the newly admitted emitter dependency must retain its computed light"
    );
    assert_eq!(
        dependency.retained_light_status(),
        Some(RetainedLightStatus::DependencyInitialized)
    );
}
