//! Raw-packet control for the saved-light admission boundary.
//!
//! The external End capture reports a sky-only edge mismatch when a retained
//! centre snapshot outlives a neighbouring block mutation. This control keeps
//! the geometry relative: it compares a mutation-then-settle admission with a
//! fresh source that already contains the same final 3x3 footprint.

use lodestone_server::{
    ChunkColumn, ChunkSource, ServerDirective, ServerProtocol,
    retained_chunk_source_for_view_radius,
};
use lodestone_server::dimension::Dimension;
use lodestone_v26_2::V770ServerProtocol;

const LIGHT_NEIGHBOUR_OFFSETS: [(i32, i32); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// A relative End footprint with one horizontal barrier layer. The optional
/// hole lets the mutation control change the neighbour without naming an
/// external world coordinate.
#[derive(Debug, Clone, Copy)]
struct BarrierSource {
    east_hole: bool,
}

impl BarrierSource {
    fn barrier_column() -> ChunkColumn {
        let mut column = ChunkColumn::new(0, 256);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 0, z, "minecraft:end_stone");
            }
        }
        column
    }
}

impl ChunkSource for BarrierSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = if (cx, cz) == (0, 0) || (cx, cz) == (1, 0) {
            Self::barrier_column()
        } else {
            ChunkColumn::new(0, 256)
        };
        if (cx, cz) == (1, 0) && self.east_hole {
            column.set_block(0, 0, 8, "minecraft:air");
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_owned()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}

    fn dimension(&self) -> Option<Dimension> {
        Some(Dimension::End)
    }
}

fn preload_footprint<S: ChunkSource>(source: &S) {
    let mut coordinates = Vec::with_capacity(LIGHT_NEIGHBOUR_OFFSETS.len() + 1);
    coordinates.push((0, 0));
    coordinates.extend(LIGHT_NEIGHBOUR_OFFSETS);
    for (cx, cz) in coordinates {
        let _ = source.column(cx, cz);
    }
}

fn settle_centre<S: ChunkSource>(source: &S, proto: &V770ServerProtocol) {
    let centre = source
        .resident_column(0, 0)
        .expect("the centre must be resident before the light fence");
    let mut compute = |centre: &ChunkColumn,
                       neighbours: &[(i32, i32, ChunkColumn)]| {
        proto.compute_initial_column_light_with_neighbours_in_dimension(
            centre,
            neighbours,
            Dimension::End,
        )
    };
    source
        .settle_resident_column_light_with_neighbours(
            0,
            0,
            &centre,
            &LIGHT_NEIGHBOUR_OFFSETS,
            true,
            false,
            false,
            &mut compute,
        )
        .expect("the complete relative footprint must settle");
}

fn raw_packet<S: ChunkSource>(source: &S, proto: &V770ServerProtocol) -> Vec<u8> {
    let centre = source
        .resident_column(0, 0)
        .expect("the centre must remain resident after settlement");
    let neighbours = LIGHT_NEIGHBOUR_OFFSETS
        .into_iter()
        .map(|(dx, dz)| {
            (
                dx,
                dz,
                source
                    .resident_column(dx, dz)
                    .expect("the complete relative footprint must remain resident"),
            )
        })
        .collect::<Vec<_>>();
    let ServerDirective::Send { payload, .. } = proto
        .try_encode_chunk_with_neighbours_in_dimension(
            0,
            0,
            &centre,
            &neighbours,
            Dimension::End,
        )
        .expect("the End packet must encode")
    else {
        panic!("the End encoder must send a chunk packet");
    };
    payload
}

#[test]
fn neighbouring_mutation_replaces_saved_end_snapshot_before_raw_encode() {
    let proto = V770ServerProtocol;

    let stale_order = retained_chunk_source_for_view_radius(BarrierSource { east_hole: false }, 2);
    preload_footprint(&stale_order);
    settle_centre(&stale_order, &proto);
    let before_mutation = raw_packet(&stale_order, &proto);

    // The east neighbour is the relative source of the lower-apron light. The
    // store must invalidate the centre snapshot even though the centre itself
    // is not the edited column.
    stale_order.set_block(16, 0, 8, "minecraft:air");
    assert_eq!(
        stale_order
            .resident_column(0, 0)
            .expect("the centre remains resident")
            .retained_light(),
        None,
        "a neighbouring block write must invalidate the centre snapshot"
    );
    settle_centre(&stale_order, &proto);
    let after_mutation = raw_packet(&stale_order, &proto);
    assert_ne!(
        before_mutation, after_mutation,
        "the relative neighbour hole must change the raw End packet"
    );

    // Independent admission control: the same final footprint is settled once
    // before encoding, so its packet is the expected result for the mutation
    // path without relying on the stale snapshot's previous bytes.
    let fresh_order = retained_chunk_source_for_view_radius(BarrierSource { east_hole: true }, 2);
    preload_footprint(&fresh_order);
    settle_centre(&fresh_order, &proto);
    let independently_settled = raw_packet(&fresh_order, &proto);
    assert_eq!(
        after_mutation, independently_settled,
        "raw packet bytes must depend on the settled final footprint, not admission order"
    );
}
