//! Overworld initial-light representation controls.
//!
//! These controls deliberately compare the generated initial fallback with a
//! retained settlement snapshot. The two paths have different authority: a
//! fallback reconstructs a compact representation from blocks and neighbours,
//! while a retained snapshot must preserve the light storage tags it was given.

use lodestone_core::Reader;
use lodestone_server::dimension::Dimension;
use lodestone_server::{ChunkColumn, ChunkSource, ServerDirective, ServerProtocol};
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_world::{ColumnLight, LightData, NibbleArray};

fn initial_packet(
    column: &ChunkColumn,
    neighbours: &[(i32, i32, ChunkColumn)],
) -> LevelChunkWithLight {
    let shape = ChunkShape::overworld_1_21();
    let ServerDirective::Send { payload, .. } = V770ServerProtocol
        .try_encode_chunk_with_neighbours_in_dimension(
            -25,
            -25,
            column,
            neighbours,
            Dimension::Overworld,
        )
        .expect("Overworld initial chunk encoding")
    else {
        panic!("Overworld initial chunk encoding must send a packet");
    };
    let mut reader = Reader::new(&payload);
    let packet = LevelChunkWithLight::decode(&mut reader, &shape)
        .expect("Overworld initial packet must decode");
    reader
        .ensure_empty()
        .expect("Overworld packet has no trailing bytes");
    packet
}

#[test]
fn retained_overworld_snapshot_preserves_light_storage_tags() {
    let shape = ChunkShape::overworld_1_21();
    let mut light = ColumnLight::new(shape.section_count);

    // Section 14 is the high section whose settled sky representation is
    // important in the large-world comparison. Keep a neighbouring missing
    // sky section as a control: both carry the same conceptual daylight only
    // after a client applies its own missing-data semantics, but they are not
    // the same persisted storage tag.
    *light.sky_mut(13) = LightData::Missing;
    *light.sky_mut(14) = LightData::Uniform(15);

    // These three block sections distinguish absent storage, an allocated
    // explicit zero, and an allocated per-cell array containing zeros.
    *light.block_mut(6) = LightData::Missing;
    *light.block_mut(14) = LightData::Uniform(0);
    let mut values = NibbleArray::filled(0);
    values.set(NibbleArray::index(4, 3, 9), 1);
    *light.block_mut(5) = LightData::Values(values);

    let mut column = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    column.set_retained_light(light.clone());
    // The neighbour-aware entrypoint is intentionally used even without
    // neighbours: the retained path must bypass fallback recomputation.
    let packet = initial_packet(&column, &[]);

    assert_eq!(packet.light, light);
    assert_eq!(packet.light.sky(13), &LightData::Missing);
    assert_eq!(packet.light.sky(14), &LightData::Uniform(15));
    assert_eq!(packet.light.block(6), &LightData::Missing);
    assert_eq!(packet.light.block(14), &LightData::Uniform(0));
    assert!(matches!(packet.light.block(5), LightData::Values(_)));
}

#[test]
fn generated_overworld_fallback_is_distinct_from_retained_snapshot() {
    let source = lodestone_server::overworld_chunk_source(42);
    let generated_column = ChunkSource::column(&source, -25, -25);
    assert!(
        generated_column.retained_light().is_none(),
        "the generated control must exercise initial fallback, not persisted light"
    );
    let mut neighbours = Vec::with_capacity(8);
    for dz in -1..=1 {
        for dx in -1..=1 {
            if (dx, dz) != (0, 0) {
                neighbours.push((
                    dx,
                    dz,
                    ChunkSource::column(&source, -25 + dx, -25 + dz),
                ));
            }
        }
    }
    let generated = initial_packet(&generated_column, &neighbours);

    // The generated path has no persisted allocation snapshot. Its compact
    // initial form omits high redundant sky and zero block sections. This is
    // intentionally contrasted with the retained control above; it must not
    // be used to rewrite a snapshot loaded from storage.
    assert_eq!(generated.light.sky(14), &LightData::Missing);
    assert_eq!(generated.light.block(14), &LightData::Missing);
}
